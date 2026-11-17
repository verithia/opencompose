//! The application host of the agent loop, and the commands a view drives a
//! turn with. The desktop panel and `POST /ai/turn` run the same host; they
//! differ only in whether a person answers approvals.
//!
//! A turn owns the backend's one provider slot from start to end, so nothing
//! else (a second turn, a rewind, a revert) can write the conversation while
//! it runs. Its project edits go into one batch owned by the conversation,
//! which the loop commits on every ending.

use super::{
    ApprovalRequest, AudioShape, Cancel, Decision, Document, Edited, Host, PendingQuestion, Reply,
    TurnEvent, TurnOutcome, TurnSpec, UserQuestion,
};
use crate::ai::sessions::{SessionPermission, Store};
use crate::ai::timing::TurnStatsHandle;
use crate::ai_service::{AiError, Requests, TurnChannels, TurnRequest};
use crate::commands::AiProviderEntry;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// The event a view hears for its turn.
pub const TURN_EVENT: &str = "ai_turn";
/// The document a turn changed, for the panel running it.
pub const PROJECT_EVENT: &str = "ai_turn_project";
/// Pending questions one turn may hold; a running turn asks one at a time.
const MAX_PENDING: usize = 4;
/// How long a question waits for a person. A closed panel leaves the card
/// for a reopened one; past this the call is refused, so a turn nobody
/// returns to does not hold its slot forever.
const QUESTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// Longest turn id a client may choose; see [`round_request_id`].
const MAX_REQUEST_ID: usize = 96;

/// A question's key: the turn's request id and the call.
type QuestionKey = (String, String);
/// A question waiting for a person, kept so a window that reloaded can ask
/// it again.
enum Waiting {
    Approval {
        answer: oneshot::Sender<Decision>,
        request: ApprovalRequest,
    },
    UserQuestion {
        answer: oneshot::Sender<Reply>,
        request: UserQuestion,
    },
}
/// Questions waiting for a person.
static PENDING: LazyLock<Mutex<HashMap<QuestionKey, Waiting>>> = LazyLock::new(Mutex::default);
/// Running turn controls, keyed by request id.
static RUNNING: LazyLock<Mutex<HashMap<String, RunningTurn>>> = LazyLock::new(Mutex::default);

#[derive(Clone)]
struct RunningTurn {
    session_id: String,
    cancel: Cancel,
    steering: super::steering::Steering,
}

/// Registration owns the inbox lifetime even when preparation fails or its
/// future is dropped before the loop starts.
struct RegisteredTurn {
    request_id: String,
    control: RunningTurn,
}

impl Drop for RegisteredTurn {
    fn drop(&mut self) {
        lock(&RUNNING).remove(&self.request_id);
        self.control.steering.close();
        lock(&PENDING).retain(|(turn, _), _| turn != &self.request_id);
    }
}

/// The adapter accepts user content only; host metadata and roles cannot be forged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerMessage {
    pub content: String,
    #[serde(default)]
    pub media: Vec<Value>,
    /// The caller's own name for the message, kept with it in the saved
    /// conversation, so a caller that lost the reply can find it by id.
    #[serde(default)]
    pub id: Option<String>,
}

/// Longest steer id a caller may choose.
const MAX_STEER_ID: usize = 64;

/// A person's attachments: bounded in number, and each one a valid
/// descriptor, so a malformed one never reaches the saved conversation.
fn check_person_media(media: &Value) -> Result<(), AiError> {
    let Some(items) = media.as_array() else {
        return match media {
            Value::Null => Ok(()),
            _ => Err(AiError::new(
                "request",
                "'media' must be a list of attachments",
            )),
        };
    };
    if items.len() > crate::ai::media::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(AiError::new(
            "request",
            "Too many attachments on one message",
        ));
    }
    for item in items {
        crate::ai::media::from_wire(item)
            .map_err(|error| AiError::new("request", AiError::from(error).message))?;
    }
    Ok(())
}

/// Resolves after durable delivery at a complete round boundary. It never
/// cancels a provider request, tool call or transaction.
pub async fn steer(
    request_id: &str,
    session_id: &str,
    message: SteerMessage,
) -> Result<(), AiError> {
    let turn = lock(&RUNNING)
        .get(request_id)
        .filter(|turn| turn.session_id == session_id && !turn.cancel.is_cancelled())
        .cloned()
        .ok_or_else(|| AiError::new("steer_closed", "No matching turn is accepting messages"))?;
    let refuse = |text: &str| AiError::new("request", text);
    if message.content.trim().is_empty() && message.media.is_empty() {
        return Err(refuse("A steering message cannot be empty"));
    }
    let mut meta = crate::ai::sessions::message_meta(crate::ai::sessions::MessageKind::User);
    if let Some(id) = &message.id {
        if !valid_request_id(id) || id.len() > MAX_STEER_ID {
            return Err(refuse(
                "A steer id is up to 64 letters, digits, '-', '_' or '.'",
            ));
        }
        meta["steer_id"] = json!(id);
    }
    let mut user = json!({"role":"user", "content":message.content, "message_meta": meta});
    if !message.media.is_empty() {
        user["media"] = json!(message.media);
    }
    check_person_media(&user["media"])?;
    crate::ai_service::validate_chat_messages(&json!([user.clone()]))
        .map_err(|error| refuse(&error.message))?;
    if user.to_string().len() > crate::ai::LIMIT {
        return Err(refuse("The steering message is too large"));
    }
    let receipt = turn
        .steering
        .enqueue(user)
        .map_err(|refusal| match refusal {
            super::steering::Refusal::Closed => {
                AiError::new("steer_closed", "The turn has finished accepting messages")
            }
            super::steering::Refusal::Full => {
                AiError::new("steer_full", "The turn's steering queue is full")
            }
        })?;
    match receipt.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(super::steering::Undelivered::Save(error))) => {
            Err(AiError::new("session_save", error))
        }
        Ok(Err(super::steering::Undelivered::Closed)) | Err(_) => Err(AiError::new(
            "steer_closed",
            "The turn ended before delivery",
        )),
    }
}

/// Stops a running turn for a caller that names its conversation too, so a
/// client cannot stop another conversation's turn by guessing its id.
pub fn cancel_in(request_id: &str, session_id: &str) -> bool {
    let turn = lock(&RUNNING)
        .get(request_id)
        .filter(|turn| turn.session_id == session_id)
        .cloned();
    turn.is_some_and(|turn| {
        turn.cancel.cancel();
        true
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a view or a headless client asks for.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnStart {
    pub provider_id: String,
    pub request_id: String,
    pub session_id: String,
    /// The system prompt and the new user message; history comes from the store.
    pub messages: Vec<Value>,
    #[serde(default)]
    pub toolsets: Option<Vec<String>>,
    #[serde(default)]
    pub label: Option<String>,
    /// Tools this conversation already answered "always allow" for.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// The project file a headless caller bound the turn to; preparation
    /// refuses when the open project is another one.
    #[serde(skip)]
    pub project: Option<std::path::PathBuf>,
    /// Whether the system prompt asks for tool calls written as text. The
    /// window that built the prompt says so, so the prompt and the parser
    /// never disagree; absent, the provider's saved capability decides.
    #[serde(default)]
    pub text_protocol: Option<bool>,
}

/// The envelope every turn event travels in.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub request_id: String,
    pub session_id: String,
    #[serde(flatten)]
    pub event: TurnEvent,
}

/// The application host for one turn.
pub struct AppHost {
    steering: super::steering::Steering,
    app: AppHandle,
    provider: AiProviderEntry,
    request_id: String,
    session_id: String,
    owner: String,
    incarnation: Option<String>,
    toolsets: Option<Vec<String>>,
    /// The accounting of the round in flight, or of the admitted first round
    /// before it is sent. Each provider round is its own accounted request,
    /// so a long turn never exceeds the per-request attempt cap.
    round_stats: Mutex<Option<TurnStatsHandle>>,
    channels: TurnChannels,
    /// Whether a person is there to ask; headless has nobody.
    attended: bool,
    allowed: Mutex<Vec<String>>,
}

impl AppHost {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    fn document(&self) -> Document {
        let state = self.state();
        let inner = state.lock_inner();
        Document {
            dsl: inner.dsl_source.clone(),
            revision: inner.revision,
            track_count: inner.project.tracks.len(),
        }
    }

    /// Publishes the document. The panel running the turn hears every change;
    /// for a headless turn the window hears the document once its batch
    /// is closed, as it does for any external edit.
    fn publish(&self) {
        let state = self.state();
        let inner = state.lock_inner();
        if self.attended {
            let _ = self
                .app
                .emit(PROJECT_EVENT, crate::commands::shared::snapshot(&inner));
        } else if inner.transaction.is_none() {
            let _ = self.app.emit(
                "agent_project_changed",
                json!({
                    "snapshot": crate::commands::shared::snapshot(&inner),
                    "replaces_document": false,
                }),
            );
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(AppHandle) -> T + Send + 'static,
    ) -> Result<T, String> {
        let app = self.app.clone();
        tauri::async_runtime::spawn_blocking(move || work(app))
            .await
            .map_err(|error| format!("background task failed: {error}"))
    }
}

impl Host for AppHost {
    fn steering(&self) -> Option<&super::steering::Steering> {
        Some(&self.steering)
    }
    async fn send(&self, wire: &[Value], round: usize) -> Result<Value, AiError> {
        let admitted = lock(&self.round_stats).clone();
        let stats = match admitted {
            Some(stats) => stats,
            None => {
                // Admission writes the usage ledger; it runs off the executor.
                let (provider, session, request) = (
                    self.provider.clone(),
                    self.session_id.clone(),
                    round_request_id(&self.request_id, round),
                );
                let stats = self
                    .blocking(move |_| admit_round(&provider, &session, &request))
                    .await
                    .map_err(AiError::config)??;
                *lock(&self.round_stats) = Some(stats.clone());
                stats
            }
        };
        let reply = crate::ai_service::send(
            self.provider.clone(),
            TurnRequest {
                messages: Value::Array(wire.to_vec()),
                use_tools: true,
                toolsets: self.toolsets.as_deref(),
                pager: true,
                output_limit: None,
            },
            self.channels.clone(),
            &stats,
        )
        .await;
        lock(&self.round_stats).take();
        if let Some(notices) = reply
            .as_ref()
            .ok()
            .and_then(|reply| reply.get("media_notices"))
            .filter(|notices| notices.as_array().is_some_and(|items| !items.is_empty()))
        {
            self.emit(TurnEvent::Media {
                notices: notices.clone(),
            });
        }
        finish_accounting(&self.app, &stats, &self.request_id).await;
        reply
    }

    async fn begin_batch(&self, label: &str) -> Result<(), String> {
        let owner = self.owner.clone();
        let label = label.to_owned();
        let session = self.session_id.clone();
        let stranded = self
            .blocking(move |app| begin_loop_batch(&app.state::<AppState>(), label, owner, &session))
            .await??;
        if let Some(stranded) = stranded {
            if let Err(reason) = stranded {
                log::warn!("[ai] a stranded batch committed but was not saved: {reason}");
            }
            self.publish();
        }
        Ok(())
    }

    async fn commit_batch(&self) -> Result<(), super::CommitError> {
        let owner = self.owner.clone();
        let result = self
            .blocking(move |app| {
                let state = app.state::<AppState>();
                commit_batch_and_save(&state, &owner)
            })
            .await
            .map_err(super::CommitError::commit)?;
        self.publish();
        result
    }

    async fn batch_changes(&self) -> Option<oc_dsl::change::ChangeSet> {
        self.blocking(move |app| {
            let state = app.state::<AppState>();
            let inner = state.lock_inner();
            let base = inner.transaction_base_project()?;
            crate::agent_api::describe_edit(base, &inner.project)
        })
        .await
        .ok()
        .flatten()
    }

    async fn call(&self, tool: &str, args: &Value, cancel: &Cancel) -> Result<Value, Value> {
        let refusal = crate::ai::skill_hints::refusal;
        let checked = crate::tool_registry::panel_tool_in(tool, self.toolsets.as_deref())
            .and_then(|def| crate::tool_registry::validate_args(&def, args));
        if let Err(message) = checked {
            return Err(refusal(&message));
        }
        let (tool, args, owner) = (tool.to_owned(), args.clone(), self.owner.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (cancel, stop) = (cancel.clone(), Arc::clone(&stop));
            tauri::async_runtime::spawn(async move {
                cancel.cancelled().await;
                stop.store(true, Ordering::Release);
            })
        };
        let result = self
            .blocking(move |app| {
                if tool == "separate_stems" {
                    return crate::agent_api::separate_stems_cancellable(&args, Some(&stop));
                }
                crate::agent_api::dispatch_panel(&app, &tool, &args, &owner)
            })
            .await;
        watcher.abort();
        let result = result
            .map_err(|error| refusal(&error))?
            .map_err(|error| refusal(&error));
        if result
            .as_ref()
            .is_ok_and(|value| value["revision"].is_u64())
        {
            self.publish();
        }
        result
    }

    async fn ask(&self, request: ApprovalRequest) -> Option<Decision> {
        if !self.attended {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        let key = (self.request_id.clone(), request.call_id.clone());
        {
            let mut pending = lock(&PENDING);
            if pending
                .keys()
                .filter(|(turn, _)| turn == &self.request_id)
                .count()
                >= MAX_PENDING
            {
                // A person is there but cannot be asked: that is a refusal,
                // never the "nobody attends" answer that lets a call apply.
                return Some(Decision::default());
            }
            pending.insert(
                key.clone(),
                Waiting::Approval {
                    answer: sender,
                    request: request.clone(),
                },
            );
        }
        // The question leaves the list however the wait ends, including when
        // a cancel drops this future, so a reloaded window never repaints a
        // card nobody waits on.
        let _asked = AskedQuestion(key.clone());
        let tool = request.tool.clone();
        self.emit(TurnEvent::Ask { request });
        let decision = Some(await_answer(receiver, QUESTION_TIMEOUT).await);
        if decision.as_ref().is_some_and(|decision| decision.expired) {
            self.emit(TurnEvent::Expired {
                call_id: key.1.clone(),
            });
        }
        if decision
            .as_ref()
            .is_some_and(|decision| decision.approved && decision.always)
        {
            let mut allowed = lock(&self.allowed);
            if !allowed.contains(&tool) {
                allowed.push(tool);
            }
        }
        decision
    }

    async fn ask_user(&self, request: UserQuestion) -> Option<Reply> {
        if !self.attended {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        let key = (self.request_id.clone(), request.call_id.clone());
        {
            let mut pending = lock(&PENDING);
            if pending
                .keys()
                .filter(|(turn, _)| turn == &self.request_id)
                .count()
                >= MAX_PENDING
            {
                return Some(Reply::expired());
            }
            pending.insert(
                key.clone(),
                Waiting::UserQuestion {
                    answer: sender,
                    request: request.clone(),
                },
            );
        }
        let _asked = AskedQuestion(key.clone());
        self.emit(TurnEvent::AskUser { request });
        let reply = await_reply(receiver, QUESTION_TIMEOUT).await;
        if reply.expired {
            self.emit(TurnEvent::Expired {
                call_id: key.1.clone(),
            });
        }
        Some(reply)
    }

    fn allowed(&self) -> Vec<String> {
        lock(&self.allowed).clone()
    }

    async fn compact(
        &self,
        cancel: &Cancel,
    ) -> Result<Vec<Value>, crate::ai::sessions::CompactionFailure> {
        use crate::ai::sessions::CompactionFailure;
        let facts = {
            let state = self.state();
            let inner = state.lock_inner();
            crate::commands::facts(&inner)
        };
        let context = crate::ai::limits::context_length(&self.provider);
        let target = (context as f64 * crate::ai::sessions::AUTO_COMPACT_TARGET_FRACTION) as u64;
        let id = self.session_id.clone();
        let planned = {
            let (id, facts) = (id.clone(), facts.clone());
            self.blocking(move |_| Store::config().plan_compaction_now(&id, target, &facts))
                .await
                .map_err(|_| CompactionFailure::ProviderError)?
        };
        let plan = match planned {
            Ok(plan) => plan,
            Err(_) => return Err(CompactionFailure::Conflict),
        };
        let summary = crate::ai_service::compaction_summary(
            &self.app,
            self.provider.clone(),
            &id,
            plan.input.clone(),
            Some(cancel),
        )
        .await;
        let (summary, mut failure) = match summary {
            Ok(summary) => (Some(summary), None),
            Err(CompactionFailure::Cancelled) => return Err(CompactionFailure::Cancelled),
            Err(failure) => (None, Some(failure)),
        };
        if cancel.is_cancelled() {
            return Err(CompactionFailure::Cancelled);
        }
        let outcome = match Store::config().commit_compaction(plan, summary.as_deref()) {
            Ok(commit) => commit.into_compaction(&mut failure),
            Err(error) => {
                failure.get_or_insert(CompactionFailure::Conflict);
                let retried = Store::config().mechanical_recompaction(&id, target, &facts);
                match retried {
                    Ok(Some(outcome)) => outcome,
                    Ok(None) => return Err(CompactionFailure::Conflict),
                    Err(_) => {
                        log::warn!("[ai] session {id} mid-turn compaction failed: {error}");
                        return Err(CompactionFailure::Conflict);
                    }
                }
            }
        };
        match failure.and_then(CompactionFailure::notice_reason) {
            Some(reason) => {
                let _ = self.app.emit(
                    crate::commands::AUTO_SESSION_EVENT,
                    crate::commands::compaction_failed_event(&id, reason),
                );
            }
            None => {
                let _ = self.app.emit(
                    crate::commands::AUTO_SESSION_EVENT,
                    crate::commands::compaction_recorded_event(&id, &outcome),
                );
            }
        }
        Ok(outcome.messages)
    }

    async fn record(&self, messages: &[Value]) -> Result<Vec<Value>, String> {
        let (id, incarnation, messages) = (
            self.session_id.clone(),
            self.incarnation.clone(),
            messages.to_vec(),
        );
        self.blocking(move |_| Store::config().append_turn(&id, &messages, incarnation.as_deref()))
            .await?
    }

    async fn stored_result(&self, call: &str) -> Option<String> {
        let (id, call) = (self.session_id.clone(), call.to_owned());
        self.blocking(move |_| Store::config().tool_result(&id, &call))
            .await
            .ok()?
            .ok()
            .flatten()
    }

    async fn record_steering(&self, messages: &[Value]) -> Result<Vec<Value>, String> {
        let (id, incarnation, messages) = (
            self.session_id.clone(),
            self.incarnation.clone(),
            messages.to_vec(),
        );
        let (dsl, project, history) = {
            let state = self.state();
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                inner.file_path.clone(),
                crate::ai::sessions::HistoryAt::of(&inner),
            )
        };
        self.blocking(move |_| {
            super::steering::record(
                &Store::config(),
                &id,
                &messages,
                incarnation.as_deref(),
                &dsl,
                project.as_deref(),
                history.as_ref(),
            )
        })
        .await?
    }

    async fn started(&self, call_id: &str) -> Result<(), String> {
        let (id, call) = (self.session_id.clone(), call_id.to_owned());
        self.blocking(move |_| Store::config().mark_started(&id, &call))
            .await?
    }

    async fn close_round(&self) -> Result<Vec<crate::ai::sessions::turns::Closed>, String> {
        let id = self.session_id.clone();
        self.blocking(move |_| Store::config().close_round_typed(&id))
            .await?
    }

    fn emit(&self, event: TurnEvent) {
        let _ = self.app.emit(
            TURN_EVENT,
            Envelope {
                request_id: self.request_id.clone(),
                session_id: self.session_id.clone(),
                event,
            },
        );
    }

    async fn refresh(&self) -> Document {
        self.document()
    }

    async fn validate_edit(&self, path: &str, dsl: &str) -> Result<(), Vec<String>> {
        let (path, dsl) = (path.to_owned(), dsl.to_owned());
        let result = self
            .blocking(move |app| {
                let state = app.state::<AppState>();
                let inner = state.lock_inner();
                crate::commands::validate_edit_in_project(&inner, dsl, path)
            })
            .await
            .map_err(|error| vec![error])?;
        if result.valid {
            Ok(())
        } else {
            Err(result.errors)
        }
    }

    async fn apply_edit(&self, path: &str, dsl: &str, expected: &str) -> Result<Edited, String> {
        let (path, dsl, expected) = (path.to_owned(), dsl.to_owned(), expected.to_owned());
        let owner = self.owner.clone();
        let (snapshot, changes) = self
            .blocking(move |app| {
                let _acting =
                    crate::edit_history::act_as(crate::edit_history::Author::from_caller(&owner));
                let state = app.state::<AppState>();
                let mut inner = state.lock_inner();
                let before = inner.project.clone();
                let snapshot =
                    crate::commands::apply_edit_to_project(&mut inner, dsl, path, expected)?;
                let changes = crate::agent_api::describe_edit(&before, &inner.project);
                Ok::<_, String>((snapshot, changes))
            })
            .await??;
        self.publish();
        Ok(Edited {
            document: Document {
                dsl: snapshot.dsl_source,
                revision: snapshot.revision,
                track_count: snapshot.track_count,
            },
            changes,
        })
    }

    async fn subtree(&self, path: &str) -> String {
        let state = self.state();
        let inner = state.lock_inner();
        oc_dsl::ops::extract(&inner.project, path)
            .map(|extract| extract.dsl_text)
            .unwrap_or_default()
    }

    async fn introduced_errors(&self, before: &str, after: &str) -> Result<Vec<String>, String> {
        let (before, after) = (before.to_owned(), after.to_owned());
        self.blocking(move |app| {
            let state = app.state::<AppState>();
            let inner = state.lock_inner();
            crate::commands::introduced_by_change(&inner, &before, &after)
        })
        .await?
    }

    async fn undo(&self, written: u64) -> Result<(), String> {
        let result = self
            .blocking(move |app| {
                let state = app.state::<AppState>();
                let mut inner = state.lock_inner();
                if inner.revision != written {
                    return Err("the project changed after the edit".to_owned());
                }
                inner.undo_step().map(|_| ())
            })
            .await?;
        self.publish();
        result
    }
}

/// Removes its question from [`PENDING`] when the wait ends or is dropped.
struct AskedQuestion(QuestionKey);

impl Drop for AskedQuestion {
    fn drop(&mut self) {
        lock(&PENDING).remove(&self.0);
    }
}

/// A person's answer, or a refusal: when nobody answers in time, and when
/// the question was dropped unanswered. A person was asked, so this is never
/// the "nobody attends" answer that lets a replacement apply.
async fn await_answer(
    receiver: oneshot::Receiver<Decision>,
    wait: std::time::Duration,
) -> Decision {
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) => Decision::default(),
        Err(_) => Decision::expired(),
    }
}

/// A person's reply, or the refusal the same wait produces.
async fn await_reply(receiver: oneshot::Receiver<Reply>, wait: std::time::Duration) -> Reply {
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) | Err(_) => Reply::expired(),
    }
}

/// A turn id a caller may choose: bounded for the accounting ledger, and
/// without `:`, which separates it from the conversation in a batch owner.
pub fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// What a turn may bring: system messages, then at most one message of the
/// person. History comes from the store, and results and host messages are the
/// loop's to write, so a caller cannot add a result or claim a host kind.
fn check_turn_messages(messages: &[Value]) -> Result<(), AiError> {
    let refuse = |message: &str| Err(AiError::new("request", message));
    let (last, system) = match messages.split_last() {
        Some((last, rest)) if last["role"] != "system" => (Some(last), rest),
        _ => (None, messages),
    };
    if system.len() > MAX_SYSTEM_MESSAGES
        || system.iter().any(|message| message["role"] != "system")
    {
        return refuse("A turn carries system messages and at most one message of the person");
    }
    if let Some(last) = last {
        let kind = last["message_meta"]["kind"].as_str();
        if last["role"] != "user" || !matches!(kind, None | Some("user")) {
            return refuse("The turn's last message must be the person's own message");
        }
        check_person_media(&last["media"])?;
    }
    Ok(())
}

/// The system messages one turn may send.
const MAX_SYSTEM_MESSAGES: usize = 8;

/// A turn reports success only after its committed edits reach the working file.
/// Commits a batch a gone turn left open, so the edits that landed stay, and
/// then writes the project file. `None` means no loop-owned batch is open; a
/// `/call` client's open batch is its live transaction and is left alone. The
/// inner `Err` is why the file was not written; the commit stands either way.
pub(crate) fn commit_stranded(state: &AppState) -> Result<Option<Result<(), String>>, String> {
    let Some(owner) = state
        .batch_owner()
        .filter(|open| stranded_loop_batch(Some(open)))
    else {
        return Ok(None);
    };
    crate::agent_api::transaction_commit(state, &owner)?;
    Ok(Some(crate::project_persist::flush_state(state).map(|_| ())))
}

/// Opens a turn's batch, committing a stranded one first. A stranded batch
/// whose file write fails still closed, so the new batch opens: the turn's own
/// commit writes the whole project again and reports a file it cannot write.
/// Returns what happened to a stranded batch, if there was one.
pub(crate) fn begin_loop_batch(
    state: &AppState,
    label: String,
    owner: String,
    session: &str,
) -> Result<Option<Result<(), String>>, String> {
    let stranded = commit_stranded(state)?;
    // The binding and the batch open under the same state lock, so a file
    // opened meanwhile cannot receive the batch the conversation did not bind.
    let mut inner = state.lock_inner();
    let open = inner.file_path.clone();
    let (_, meta) = Store::config()
        .find(session)
        .map_err(|error| format!("The conversation's binding could not be read: {error}"))?;
    let status = crate::ai::sessions::binding_status(&meta, open.as_deref());
    if !status.matches {
        let mismatch = crate::ai::sessions::BindingMismatch {
            bound: status.bound.unwrap_or_default(),
            open: status.open.unwrap_or_default(),
        };
        return Err(format!(
            "{} This turn cannot edit the open file; stop it and bind the conversation to that \
             file before trying again.",
            mismatch.message()
        ));
    }
    inner.begin_transaction(label, owner)?;
    Ok(stranded)
}

fn commit_batch_and_save(state: &AppState, owner: &str) -> Result<(), super::CommitError> {
    crate::agent_api::transaction_commit(state, owner).map_err(super::CommitError::commit)?;
    crate::project_persist::flush_state(state)
        .map(|_| ())
        .map_err(|reason| {
            super::CommitError::save(format!(
                "The edits are committed in memory but the project file was not written: {reason}"
            ))
        })
}

/// The audio a provider's selected model takes in, or `None`.
fn audio_shape(entry: &AiProviderEntry) -> Option<AudioShape> {
    if !crate::ai::media::audio_supported(entry) {
        return None;
    }
    let input = crate::ai::catalog::model(entry.wire(), &entry.model)
        .map(|model| model.audio_input())
        .unwrap_or_default();
    Some(AudioShape {
        channels: input.channels,
        sample_rate: input.sample_rate,
    })
}

/// The accounting identity of one provider round: the turn's id, then the
/// round. The first round keeps the turn's own id.
fn round_request_id(request_id: &str, round: usize) -> String {
    if round == 0 {
        request_id.to_owned()
    } else {
        format!("{request_id}.{round}")
    }
}

/// Admits one provider round to the conversation's usage ledger.
fn admit_round(
    provider: &AiProviderEntry,
    session_id: &str,
    request_id: &str,
) -> Result<TurnStatsHandle, AiError> {
    let stats = TurnStatsHandle::new(&provider.id, &provider.model, provider.wire());
    let lease = crate::ai::session_usage::SessionLease::admit(
        Store::config(),
        session_id,
        request_id,
        crate::ai::session_usage::Purpose::Chat,
    )
    .map_err(|error| AiError::new(error.code.as_str(), error.message))?;
    stats.set_accounting(lease);
    Ok(stats)
}

/// Runs one turn to its end on the backend's provider slot. `channels`
/// streams text to a view; `attended` says whether a person answers.
pub async fn run(
    app: &AppHandle,
    start: TurnStart,
    channels: TurnChannels,
    attended: bool,
    owner: String,
) -> Result<TurnOutcome, AiError> {
    // Round suffixes keep later rounds' accounting ids within the ledger's 100.
    if !valid_request_id(&start.request_id) {
        return Err(AiError::new("request", "Invalid AI request identifier"));
    }
    check_turn_messages(&start.messages)?;
    let messages = Value::Array(start.messages.clone());
    crate::ai_service::validate_chat_messages(&messages)?;
    let provider = crate::ai_service::provider(&start.provider_id).map_err(AiError::config)?;
    let requests = app.state::<Requests>();
    // The cancel handle is published before the slot is claimed, so a cancel
    // that arrives at any point reaches the turn instead of freeing its slot.
    let cancel = Cancel::default();
    let control = RunningTurn {
        session_id: start.session_id.clone(),
        cancel: cancel.clone(),
        steering: super::steering::Steering::default(),
    };
    {
        let mut running = lock(&RUNNING);
        if running.contains_key(&start.request_id) {
            return Err(AiError::new("busy", "Another AI request is active"));
        }
        running.insert(start.request_id.clone(), control.clone());
    }
    let _registration = RegisteredTurn {
        request_id: start.request_id.clone(),
        control: control.clone(),
    };
    let claim = match requests.claim_for(&start.request_id, Some(&start.session_id)) {
        Ok(Some(claim)) => claim,
        refused => {
            return Err(match refused {
                Err(error) => AiError::config(error),
                _ => AiError::new("busy", "Another AI request is active"),
            });
        }
    };
    requests.set_session_owned(&start.request_id, &claim.identity, &start.session_id);
    let release = scopeguard(&requests, &start.request_id, &claim.identity);
    // `ai_cancel` sends on the slot's cancel channel; it cancels the turn.
    {
        let cancel = cancel.clone();
        let receiver = claim.cancel_rx;
        tauri::async_runtime::spawn(async move {
            if receiver.await.is_ok() {
                cancel.cancel();
            }
        });
    }
    let outcome = run_claimed(
        app,
        &start,
        &provider,
        channels,
        attended,
        owner,
        control.clone(),
    )
    .await;
    drop(release);
    outcome
}

struct Release<'a> {
    requests: &'a Requests,
    id: String,
    owner: Arc<()>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let _ = self.requests.release_owned(&self.id, &self.owner);
    }
}

fn scopeguard<'a>(requests: &'a Requests, id: &str, owner: &Arc<()>) -> Release<'a> {
    Release {
        requests,
        id: id.to_owned(),
        owner: Arc::clone(owner),
    }
}

async fn run_claimed(
    app: &AppHandle,
    start: &TurnStart,
    provider: &AiProviderEntry,
    channels: TurnChannels,
    attended: bool,
    owner: String,
    control: RunningTurn,
) -> Result<TurnOutcome, AiError> {
    let stats = {
        let (provider, session, request) = (
            provider.clone(),
            start.session_id.clone(),
            start.request_id.clone(),
        );
        tauri::async_runtime::spawn_blocking(move || admit_round(&provider, &session, &request))
            .await
            .map_err(|error| AiError::config(format!("background task failed: {error}")))??
    };
    let incarnation = stats
        .accounting()
        .map(|lease| lease.update().incarnation)
        .ok_or_else(|| AiError::new("accounting_request", "The turn has no accounting owner"))?;
    let messages = Value::Array(start.messages.clone());
    let tool_tokens = super::wire::offered_tool_tokens(start.toolsets.as_deref(), true);
    let permission = Store::config()
        .find(&start.session_id)
        .map(|(_, meta)| meta.permission)
        .unwrap_or_default();
    let permission = turn_permission(attended, permission);
    let surface = crate::ai::host_notice::ToolSurface {
        names: super::wire::offered_tool_names(start.toolsets.as_deref()),
        permission,
    };
    // Preparation closes any round a dead turn left open, saves the new user
    // message with its snapshot, and compacts when the window needs it.
    let prepared = crate::commands::prepare_turn_for(
        app,
        provider,
        &messages,
        crate::commands::SavedTurnBinding {
            id: &start.session_id,
            incarnation: &incarnation,
            project: start.project.as_deref(),
            tool_tokens,
            surface: Some(surface),
            cancel: Some(&control.cancel),
        },
    )
    .await
    .map_err(AiError::from);
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            finish_accounting(app, &stats, &start.request_id).await;
            return Err(error);
        }
    };
    if let Some(compaction) = prepared.compacted.as_ref() {
        let _ = app.emit(
            crate::commands::AUTO_SESSION_EVENT,
            crate::commands::compaction_event(&prepared, compaction, provider),
        );
    }
    let host = AppHost {
        steering: control.steering,
        app: app.clone(),
        provider: provider.clone(),
        request_id: start.request_id.clone(),
        session_id: start.session_id.clone(),
        owner,
        incarnation: Some(incarnation),
        toolsets: start.toolsets.clone(),
        round_stats: Mutex::new(Some(stats)),
        channels,
        attended,
        allowed: Mutex::new(start.allowed.clone()),
    };
    let spec = TurnSpec {
        wire: prepared.messages.as_array().cloned().unwrap_or_default(),
        permission,
        label: start
            .label
            .clone()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "AI edit".into()),
        audio: audio_shape(provider),
        scope: start.session_id.clone(),
        toolsets: start.toolsets.clone(),
        text_protocol: start
            .text_protocol
            .unwrap_or(!provider.capabilities.native_tool_calling),
        context_length: crate::ai::limits::context_length(provider),
        max_output: crate::ai::limits::max_output(provider),
    };
    let outcome = super::run_turn(&host, spec, control.cancel).await;
    // A round cancelled mid-request, or a first round never sent, still
    // closes its accounting.
    let unfinished = lock(&host.round_stats).take();
    if let Some(stats) = unfinished {
        finish_accounting(app, &stats, &start.request_id).await;
    }
    Ok(outcome)
}

/// The permission a turn runs under. With nobody attending there is nobody
/// to show a review to, so the stored mode cannot hold a write back; confirm
/// calls are still refused, because the gate asks and nobody answers.
fn turn_permission(attended: bool, stored: SessionPermission) -> SessionPermission {
    if attended {
        stored
    } else {
        SessionPermission::Write
    }
}

async fn finish_accounting(app: &AppHandle, stats: &TurnStatsHandle, request_id: &str) {
    let purpose = crate::ai::session_usage::Purpose::Chat;
    let accounting =
        crate::ai::session_usage::checkpoint_final(stats, app, request_id, purpose).await;
    if stats.attempted() || accounting.is_some() {
        let _ = app.emit(
            "ai_stats",
            crate::ai::session_usage::StatsEvent {
                turn: stats.snapshot(),
                client_request_id: request_id.to_owned(),
                purpose,
                accounting,
            },
        );
    }
}

/// Whether the open batch was left by a backend turn that is gone. It is
/// asked by a turn that holds the request slot, and a turn holds the slot for
/// as long as its batch is open, so every loop-owned batch it sees is
/// stranded, whatever conversation it belongs to. A `/call` client's batch is
/// still in use.
fn stranded_loop_batch(open: Option<&str>) -> bool {
    open.is_some_and(crate::state::loop_batch_owner)
}

/// Answers an approval a running turn asked. A stale answer changes nothing.
pub fn answer(request_id: &str, call_id: &str, decision: Decision) -> Result<(), String> {
    let key = (request_id.to_owned(), call_id.to_owned());
    let question = {
        let mut pending = lock(&PENDING);
        if !matches!(pending.get(&key), Some(Waiting::Approval { .. })) {
            return Err("Nothing is waiting for an answer to this question".into());
        }
        pending.remove(&key)
    };
    let Some(Waiting::Approval { answer, .. }) = question else {
        return Err("Nothing is waiting for an answer to this question".into());
    };
    answer
        .send(decision)
        .map_err(|_| "The turn stopped before the answer arrived".to_string())
}

/// Why a model question's answer was refused. The answer is not delivered and
/// the question stays open, so a view can correct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyError {
    /// No question waits under this turn and call id.
    NotWaiting,
    /// The option index names no offered choice.
    OptionOutOfRange,
    /// The question offers no free text.
    TextNotAccepted,
    /// The text is over the answer bound.
    TextTooLong,
    /// The waiting turn ended before the answer arrived.
    TurnStopped,
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotWaiting => "Nothing is waiting for an answer to this question",
            Self::OptionOutOfRange => "The chosen option is not offered by this question",
            Self::TextNotAccepted => "This question accepts no free-text answer",
            Self::TextTooLong => "The answer is too long",
            Self::TurnStopped => "The turn stopped before the answer arrived",
        })
    }
}

/// Answers a question a running turn asked. An answer outside the question's
/// bounds is refused before anything is removed, so the card can try again.
pub fn reply(request_id: &str, call_id: &str, reply: Reply) -> Result<(), ReplyError> {
    let key = (request_id.to_owned(), call_id.to_owned());
    let mut pending = lock(&PENDING);
    let Some(Waiting::UserQuestion { request, .. }) = pending.get(&key) else {
        return Err(ReplyError::NotWaiting);
    };
    validate_reply(&reply, request)?;
    let Some(Waiting::UserQuestion { answer, .. }) = pending.remove(&key) else {
        return Err(ReplyError::NotWaiting);
    };
    answer.send(reply).map_err(|_| ReplyError::TurnStopped)
}

/// Checks an answer against the question it answers: a choice must be one of
/// the offered options, and text must be allowed and within the bound.
fn validate_reply(reply: &Reply, request: &UserQuestion) -> Result<(), ReplyError> {
    if reply
        .option
        .is_some_and(|index| index >= request.options.len())
    {
        return Err(ReplyError::OptionOutOfRange);
    }
    if let Some(text) = reply
        .text
        .as_deref()
        .filter(|text| !crate::ai::session_state::is_blank_text(text))
    {
        if !request.free_text {
            return Err(ReplyError::TextNotAccepted);
        }
        if text.chars().count() > request.max_chars {
            return Err(ReplyError::TextTooLong);
        }
    }
    Ok(())
}

/// Whether a backend turn is running in this conversation. A compaction,
/// history drop or delete under it would move the round it is writing.
pub fn session_running(session_id: &str) -> bool {
    lock(&RUNNING)
        .values()
        .any(|turn| turn.session_id == session_id)
}

/// Stops a running turn. Completed calls keep their results.
pub fn cancel(request_id: &str) -> bool {
    match lock(&RUNNING).get(request_id) {
        Some(turn) => {
            turn.cancel.cancel();
            true
        }
        None => false,
    }
}

/// What a window asks a recall to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallRequest {
    pub index: usize,
    /// Whether to move the project's history cursor back to the position the
    /// message recorded, undoing the edits made since.
    pub undo_edits: bool,
    /// Whether the caller accepted undoing edits by other authors.
    pub confirmed: bool,
    /// The history position the confirmation named. A confirmed call whose
    /// history moved since refuses instead of undoing edits it never listed.
    pub token: Option<RecallToken>,
}

/// The history position a recall confirmation was shown for: the applied level
/// and the newest applied entry. Either moving means the dialog described a
/// history that no longer exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallToken {
    pub applied: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<u64>,
}

impl RecallToken {
    fn of(inner: &crate::state::AppStateInner) -> Self {
        Self {
            applied: inner.undo.applied_level(),
            newest: inner.undo.mark().at,
        }
    }
}

/// One author whose edits a recall would undo, and how many.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecallOther {
    pub author: crate::edit_history::Author,
    pub count: usize,
}

/// Why a recall did not run. The frontend localizes these; no prose crosses
/// the command boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallRefusal {
    TurnRunning,
    BatchOpen,
    OtherProject,
    NoRecordedPosition,
    PositionCleared,
    NotAPersonMessage,
    ConversationChanged,
    EditsChanged,
    PreviewOpen,
}

/// What the cut message gives back to the composer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallMessage {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Value>,
}

/// The answer to a recall.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecallOutcome {
    Recalled {
        kept: usize,
        message: RecallMessage,
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<crate::commands::shared::ProjectSnapshot>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        unsaved: Option<String>,
    },
    NeedsConfirmation {
        others: Vec<RecallOther>,
        token: RecallToken,
    },
    Refused {
        reason: RecallRefusal,
    },
}

impl RecallOutcome {
    fn refused(reason: RecallRefusal) -> Self {
        Self::Refused { reason }
    }
}

/// Cuts the conversation just before a person's message and returns that
/// message. With `undo_edits` it also moves the project's history cursor back
/// to the position the message recorded, as one history jump: a redo brings
/// the edits back, and no new entry is pushed. Refused while any turn runs.
pub fn recall(
    app: &AppHandle,
    session_id: &str,
    request: RecallRequest,
) -> Result<RecallOutcome, String> {
    let state = app.state::<AppState>();
    let revision = state.lock_inner().revision;
    let outcome = recall_in(
        &app.state::<Requests>(),
        &state,
        &Store::config(),
        session_id,
        request,
    );
    // A cut that failed after moving the cursor puts the document back under a
    // new revision. The window hears it, so its next edit is not refused as
    // stale. A successful recall returns its snapshot to the caller instead.
    if !matches!(outcome, Ok(RecallOutcome::Recalled { .. }))
        && let Some(snapshot) = moved_since(&state, revision)
    {
        let _ = app.emit(PROJECT_EVENT, snapshot);
    }
    outcome
}

/// The document, when its revision moved past `revision`.
fn moved_since(
    state: &AppState,
    revision: u64,
) -> Option<crate::commands::shared::ProjectSnapshot> {
    let inner = state.lock_inner();
    (inner.revision != revision).then(|| crate::commands::shared::snapshot(&inner))
}

/// [`recall`] over its parts.
fn recall_in(
    requests: &Requests,
    state: &AppState,
    store: &Store,
    session_id: &str,
    request: RecallRequest,
) -> Result<RecallOutcome, String> {
    recall_with(
        requests,
        state,
        store,
        session_id,
        request,
        |store, point| store.cut_before_user(session_id, point),
    )
}

/// The recall with its cut supplied, so a test can make the cut fail.
///
/// The document lock is held from the cursor move through the cut, so no
/// other edit lands between them and a failed cut puts the cursor, the
/// document and both history stacks back exactly. Nothing is written to disk
/// until the cut has succeeded.
fn recall_with(
    requests: &Requests,
    state: &AppState,
    store: &Store,
    session_id: &str,
    request: RecallRequest,
    cut: impl FnOnce(
        &Store,
        &crate::ai::sessions::turns::RecallPoint,
    ) -> Result<usize, crate::ai::sessions::turns::CutFailure>,
) -> Result<RecallOutcome, String> {
    use crate::ai::sessions::turns::{CutFailure, RecallPointError};

    // Holding the slot for the whole recall keeps a turn from starting in it.
    let slot = format!("recall-{session_id}");
    let Some(claim) = requests.claim_internal(&slot)? else {
        return Ok(RecallOutcome::refused(RecallRefusal::TurnRunning));
    };
    let _release = scopeguard(requests, &slot, &claim.identity);
    let point = match store.recall_point(session_id, request.index) {
        Ok(point) => point,
        Err(RecallPointError::NotAPersonMessage) => {
            return Ok(RecallOutcome::refused(RecallRefusal::NotAPersonMessage));
        }
        Err(RecallPointError::Store(message)) => return Err(message),
    };
    let mut inner = state.lock_inner();
    if inner.transaction.is_some() {
        return Ok(RecallOutcome::refused(RecallRefusal::BatchOpen));
    }
    // A preview holds two documents; moving the history under it would land
    // the cursor on whichever side is on screen. The typed refusal says so
    // before a confirmation dialog can offer the action.
    if request.undo_edits && inner.previewing() {
        return Ok(RecallOutcome::refused(RecallRefusal::PreviewOpen));
    }
    let mut moved = false;
    let mut before = None;
    if request.undo_edits {
        let Some(recorded) = point.history_at.as_ref() else {
            return Ok(RecallOutcome::refused(RecallRefusal::NoRecordedPosition));
        };
        // The position names one project file; moving another file's history
        // would undo edits the message never saw.
        let open = inner
            .file_path
            .as_deref()
            .map(crate::ai::sessions::canonical_path)
            .map(|path| path.to_string_lossy().into_owned());
        if open.as_deref() != Some(recorded.project.as_str()) {
            return Ok(RecallOutcome::refused(RecallRefusal::OtherProject));
        }
        let applied = inner.undo.applied_level();
        let level = match recorded.at {
            None => 0,
            Some(id) => match inner.undo.level_of(id) {
                Some(level) => level,
                None => return Ok(RecallOutcome::refused(RecallRefusal::PositionCleared)),
            },
        };
        if applied > level {
            let token = RecallToken::of(&inner);
            if !request.confirmed {
                let entries =
                    inner
                        .undo
                        .entries_between(level, applied, &inner.project.properties)?;
                let mut others: Vec<RecallOther> = Vec::new();
                for (author, _cause, _changes) in entries {
                    let own = matches!(
                        &author,
                        crate::edit_history::Author::Agent { session: Some(session) }
                            if session == session_id
                    );
                    if own {
                        continue;
                    }
                    match others.iter_mut().find(|other| other.author == author) {
                        Some(other) => other.count += 1,
                        None => others.push(RecallOther { author, count: 1 }),
                    }
                }
                if !others.is_empty() {
                    return Ok(RecallOutcome::NeedsConfirmation { others, token });
                }
            } else if request.token != Some(token) {
                // The person confirmed a history that has since moved. Undoing
                // it would apply a different set of edits than the dialog named.
                return Ok(RecallOutcome::refused(RecallRefusal::EditsChanged));
            }
            // Everything the move touches, so a failed cut puts it back
            // exactly: both history stacks, the project, its text and the
            // dirty flag.
            before = Some((
                inner.project.clone(),
                inner.dsl_source.clone(),
                inner.dirty,
                inner.undo.take_copy(),
            ));
            inner.restore_level(level)?;
            moved = true;
        }
    }
    // The session store never takes the document lock, so holding it here
    // cannot deadlock. The cut checks that the message is still the one that
    // was chosen, so a compaction in between cannot move it.
    let cut_result = (|| {
        if request.undo_edits
            && point
                .history_at
                .as_ref()
                .is_some_and(|position| position.at.is_none())
            && !recall_root_matches(store, session_id, &point.message, &inner.project)?
        {
            return Err(CutFailure::PositionCleared);
        }
        cut(store, &point)
    })();
    if let Err(error) = cut_result {
        if let Some((project, source, dirty, history)) = before {
            inner.project = project;
            inner.undo.put_back(history);
            inner.resync_after_history();
            inner.dsl_source = source;
            inner.dirty = dirty;
        }
        return match error {
            CutFailure::ConversationChanged => {
                Ok(RecallOutcome::refused(RecallRefusal::ConversationChanged))
            }
            CutFailure::PositionCleared => {
                Ok(RecallOutcome::refused(RecallRefusal::PositionCleared))
            }
            CutFailure::Store(message) => Err(message),
        };
    }
    let unsaved = if moved {
        crate::commands::publish_restored(&mut inner).err()
    } else {
        None
    };
    let snapshot = moved.then(|| Box::new(crate::commands::shared::snapshot(&inner)));
    let message = RecallMessage {
        content: point.message["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        media: point
            .message
            .get("media")
            .filter(|media| media.as_array().is_some_and(|entries| !entries.is_empty()))
            .cloned(),
    };
    drop(inner);
    super::results::with_results(|results| results.forget(session_id));
    Ok(RecallOutcome::Recalled {
        kept: point.keep,
        message,
        snapshot,
        unsaved,
    })
}

/// An absent entry id identifies the original root only while its saved
/// document still matches. Pruning may rebase level zero without changing
/// the project path, so the existing snapshot supplies the root identity.
fn recall_root_matches(
    store: &Store,
    session_id: &str,
    message: &Value,
    project: &oc_dsl::ast::Project,
) -> Result<bool, String> {
    let Some(sha) = message["dsl_sha"].as_str() else {
        return Ok(false);
    };
    let expected = crate::commands::installed_project(&store.dsl_version(session_id, sha)?)?;
    let actual = crate::commands::installed_project(&oc_dsl::serialize(project))?;
    Ok(expected == actual)
}

/// The approvals a running turn is waiting on, for a window that reloaded
/// while the card was open.
#[cfg(test)]
pub fn questions(request_id: &str) -> Vec<ApprovalRequest> {
    lock(&PENDING)
        .iter()
        .filter(|((turn, _), _)| turn == request_id)
        .filter_map(|(_, question)| match question {
            Waiting::Approval { request, .. } => Some(request.clone()),
            Waiting::UserQuestion { .. } => None,
        })
        .collect()
}

/// Every question a running turn is waiting on, typed for the wire.
pub fn pending_questions(request_id: &str) -> Vec<PendingQuestion> {
    lock(&PENDING)
        .iter()
        .filter(|((turn, _), _)| turn == request_id)
        .map(|(_, question)| match question {
            Waiting::Approval { request, .. } => PendingQuestion::Approval {
                request: request.clone(),
            },
            Waiting::UserQuestion { request, .. } => PendingQuestion::UserQuestion {
                request: request.clone(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sessions;
    use crate::ai::sessions::HistoryAt;
    use crate::ai::sessions::turns::SnapshotAt;
    use serde_json::Map;

    #[test]
    fn a_committed_turn_reports_a_working_file_failure_and_keeps_its_edits() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("blocked");
        std::fs::write(&parent, "not a directory").unwrap();
        let state = AppState::new();
        state
            .begin_transaction("Fixture".into(), "owner".into())
            .unwrap();
        {
            let mut inner = state.lock_inner();
            inner.file_path = Some(parent.join("song.ocp"));
            inner.project.properties.tempo = 138.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.dirty = true;
        }
        let error = commit_batch_and_save(&state, "owner").unwrap_err();
        assert_eq!(error.failure, super::super::CommitFailure::Save);
        assert!(
            error.message.contains("project file was not written"),
            "{error:?}"
        );
        assert!(state.lock_inner().transaction.is_none());
        assert_eq!(state.lock_inner().project.properties.tempo, 138.0);
        assert!(!parent.join("song.ocp").exists());
        let working = root.path().join("song.ocp");
        state
            .begin_transaction("Retry publication".into(), "owner".into())
            .unwrap();
        state.lock_inner().file_path = Some(working.clone());
        commit_batch_and_save(&state, "owner").unwrap();
        assert_eq!(
            std::fs::read_to_string(working).unwrap(),
            state.lock_inner().dsl_source
        );
    }

    const BEFORE: &str = "@project \"Recall\"\n  tempo: 120\n";
    const AFTER: &str = "@project \"Recall\"\n  tempo: 150\n";

    fn user(text: &str, sha: &str) -> Value {
        json!({
            "role": "user", "content": text, "dsl_sha": sha,
            "message_meta": sessions::message_meta(sessions::MessageKind::User),
        })
    }

    /// Records a person's message from the state's current document, with the
    /// history position beside it.
    fn record_user(state: &AppState, store: &Store, id: &str, text: &str) -> HistoryAt {
        let (dsl, project, history) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                inner.file_path.clone(),
                HistoryAt::of(&inner).expect("the fixture has a project file"),
            )
        };
        let sha = store.snapshot_dsl(id, &dsl).unwrap();
        store
            .append_turn_with_snapshot(
                id,
                &[user(text, &sha)],
                None,
                SnapshotAt {
                    dsl: &dsl,
                    project: project.as_deref(),
                    history: Some(&history),
                },
            )
            .unwrap();
        history
    }

    fn apply_edit(state: &AppState, source: &str) {
        let mut inner = state.lock_inner();
        crate::commands::update_dsl_inner(source.to_string(), &mut inner).unwrap();
    }

    /// A conversation of two person messages, the first sent against `BEFORE`.
    /// The project has moved on to `AFTER`, by the conversation's own agent
    /// when `own` is set.
    fn fixture_edited_as(
        own: bool,
    ) -> (
        tempfile::TempDir,
        Store,
        String,
        AppState,
        std::path::PathBuf,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("song.ocp");
        std::fs::write(&path, BEFORE).unwrap();
        let store = Store::new(root.path().join("sessions"));
        let id = store.create(None, None, Some("Recall"), None).unwrap().id;
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(BEFORE).unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.file_path = Some(path.clone());
        }
        record_user(&state, &store, &id, "first");
        store
            .append_turn(
                &id,
                &[json!({"role": "assistant", "content": "done"})],
                None,
            )
            .unwrap();
        let author = if own {
            crate::edit_history::Author::Agent {
                session: Some(id.clone()),
            }
        } else {
            crate::edit_history::Author::Person
        };
        let _acting = crate::edit_history::act_as(author);
        apply_edit(&state, AFTER);
        record_user(&state, &store, &id, "second");
        (root, store, id, state, path)
    }

    fn fixture() -> (
        tempfile::TempDir,
        Store,
        String,
        AppState,
        std::path::PathBuf,
    ) {
        fixture_edited_as(false)
    }

    fn recalled(outcome: RecallOutcome) -> (usize, RecallMessage, Option<Value>, Option<String>) {
        match outcome {
            RecallOutcome::Recalled {
                kept,
                message,
                snapshot,
                unsaved,
            } => (
                kept,
                message,
                snapshot.and_then(|snapshot| serde_json::to_value(snapshot).ok()),
                unsaved,
            ),
            other => panic!("expected a recall, got {other:?}"),
        }
    }

    fn refused(outcome: RecallOutcome) -> RecallRefusal {
        match outcome {
            RecallOutcome::Refused { reason } => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn plain(index: usize) -> RecallRequest {
        RecallRequest {
            index,
            undo_edits: false,
            confirmed: false,
            token: None,
        }
    }

    fn with_undo(state: &AppState, index: usize) -> RecallRequest {
        RecallRequest {
            index,
            undo_edits: true,
            confirmed: true,
            token: Some(RecallToken::of(&state.lock_inner())),
        }
    }

    #[test]
    fn a_plain_recall_cuts_without_touching_the_document_or_history() {
        let (_root, store, id, state, _path) = fixture();
        let (source, history, revision) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
                inner.revision,
            )
        };
        let requests = Requests::default();
        let (kept, message, snapshot, unsaved) =
            recalled(recall_in(&requests, &state, &store, &id, plain(0)).unwrap());
        assert_eq!(kept, 0);
        assert_eq!(message.content, "first");
        assert!(message.media.is_none());
        assert!(snapshot.is_none(), "the project is untouched");
        assert!(unsaved.is_none());
        let inner = state.lock_inner();
        assert_eq!(inner.dsl_source, source);
        assert_eq!(inner.project.properties.tempo, 150.0);
        assert_eq!(inner.revision, revision, "no document revision moved");
        assert_eq!(
            serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
            history,
            "no history entry was pushed"
        );
        drop(inner);
        assert!(
            store.view(&id).unwrap().is_empty(),
            "the conversation is cut before the message"
        );
        assert!(requests.active().unwrap().is_none(), "the slot is released");
    }

    #[test]
    fn a_recall_and_undo_moves_the_cursor_back_and_a_redo_restores_the_edits() {
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        let requests = Requests::default();
        let (kept, message, snapshot, unsaved) =
            recalled(recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap());
        assert_eq!(kept, 0);
        assert_eq!(message.content, "first");
        assert!(snapshot.is_some(), "the undone document is published");
        assert!(unsaved.is_none());
        {
            let inner = state.lock_inner();
            assert_eq!(inner.project.properties.tempo, 120.0);
            assert_eq!(inner.undo.applied_level(), 0);
            assert_eq!(inner.undo.redo.len(), 1, "the edit is one redo away");
        }
        assert!(store.view(&id).unwrap().is_empty());
        state.lock_inner().redo_step().unwrap();
        assert_eq!(
            state.lock_inner().project.properties.tempo,
            150.0,
            "a redo brings the edit back"
        );
    }

    #[test]
    fn edits_by_another_author_need_confirmation_and_confirmed_recall_proceeds() {
        let (_root, store, id, state, _path) = fixture();
        let requests = Requests::default();
        let (source, history) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
            )
        };
        let outcome = recall_in(
            &requests,
            &state,
            &store,
            &id,
            RecallRequest {
                index: 0,
                undo_edits: true,
                confirmed: false,
                token: None,
            },
        )
        .unwrap();
        match outcome {
            RecallOutcome::NeedsConfirmation { others, .. } => assert_eq!(
                others,
                vec![RecallOther {
                    author: crate::edit_history::Author::Person,
                    count: 1,
                }]
            ),
            other => panic!("expected a confirmation, got {other:?}"),
        }
        {
            let inner = state.lock_inner();
            assert_eq!(inner.dsl_source, source, "nothing moved");
            assert_eq!(inner.project.properties.tempo, 150.0);
            assert_eq!(
                serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
                history
            );
        }
        assert_eq!(store.view(&id).unwrap().len(), 3);

        let (kept, _, snapshot, _) =
            recalled(recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap());
        assert_eq!(kept, 0);
        assert!(snapshot.is_some());
        assert_eq!(state.lock_inner().project.properties.tempo, 120.0);
    }

    #[cfg(unix)]
    #[test]
    fn a_recall_stands_when_the_session_metadata_cannot_be_written() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        let (dir, _) = store.find(&id).unwrap();
        let outcome = {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            let requests = Requests::default();
            let outcome = recall_in(&requests, &state, &store, &id, with_undo(&state, 0));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            outcome
        };
        let (kept, _, snapshot, _) = recalled(outcome.unwrap());
        assert_eq!(kept, 0);
        assert!(snapshot.is_some(), "the moved document is published");
        let inner = state.lock_inner();
        assert_eq!(inner.project.properties.tempo, 120.0, "the history moved");
        assert_eq!(inner.undo.applied_level(), 0);
        drop(inner);
        assert!(store.view(&id).unwrap().is_empty(), "the cut is committed");
    }

    #[test]
    fn a_confirmation_refuses_when_edits_land_after_it_was_shown() {
        let (_root, store, id, state, _path) = fixture();
        let requests = Requests::default();
        let token = match recall_in(
            &requests,
            &state,
            &store,
            &id,
            RecallRequest {
                index: 0,
                undo_edits: true,
                confirmed: false,
                token: None,
            },
        )
        .unwrap()
        {
            RecallOutcome::NeedsConfirmation { token, .. } => token,
            other => panic!("expected a confirmation, got {other:?}"),
        };
        // Another edit lands while the dialog is open.
        apply_edit(&state, "@project \"Recall\"\n  tempo: 160\n");
        let outcome = recall_in(
            &requests,
            &state,
            &store,
            &id,
            RecallRequest {
                index: 0,
                undo_edits: true,
                confirmed: true,
                token: Some(token),
            },
        )
        .unwrap();
        assert_eq!(refused(outcome), RecallRefusal::EditsChanged);
        assert_eq!(
            state.lock_inner().project.properties.tempo,
            160.0,
            "the unlisted edit stays"
        );
        assert_eq!(
            store.view(&id).unwrap().len(),
            3,
            "the conversation is whole"
        );
    }

    #[test]
    fn a_failed_cut_after_the_cursor_moved_puts_everything_back() {
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        let (source, history, redo, revision) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
                inner.undo.redo.len(),
                inner.revision,
            )
        };
        let requests = Requests::default();
        let error = recall_with(
            &requests,
            &state,
            &store,
            &id,
            with_undo(&state, 0),
            |_, _| {
                Err(crate::ai::sessions::turns::CutFailure::Store(
                    "the session store is read-only".into(),
                ))
            },
        )
        .unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        // The text is back under a new revision, which the window is told.
        let moved = moved_since(&state, revision).expect("the revision moved");
        assert_eq!(moved.dsl_source, source);
        let inner = state.lock_inner();
        assert_eq!(inner.dsl_source, source, "the exact text is back");
        assert_eq!(inner.project.properties.tempo, 150.0);
        assert_eq!(
            serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
            history,
            "the cursor is back where it was"
        );
        assert_eq!(inner.undo.redo.len(), redo, "and no redo entry appears");
        drop(inner);
        assert_eq!(
            store.view(&id).unwrap().len(),
            3,
            "the conversation is whole"
        );
        assert!(requests.active().unwrap().is_none(), "the slot is released");
    }

    #[test]
    fn a_changed_conversation_is_a_typed_refusal_and_puts_the_cursor_back() {
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        let source = state.lock_inner().dsl_source.clone();
        let requests = Requests::default();
        let outcome = recall_with(
            &requests,
            &state,
            &store,
            &id,
            with_undo(&state, 0),
            |_, _| Err(crate::ai::sessions::turns::CutFailure::ConversationChanged),
        )
        .unwrap();
        assert_eq!(refused(outcome), RecallRefusal::ConversationChanged);
        assert_eq!(state.lock_inner().dsl_source, source);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
    }

    #[test]
    fn a_failed_cut_keeps_a_full_history_and_its_redo_entries_exactly() {
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        for tempo in 0..crate::state::MAX_UNDO + 2 {
            let source = AFTER.replace("150", &(151 + tempo).to_string());
            apply_edit(&state, &source);
            if tempo == crate::state::MAX_UNDO - 2 {
                // The original root is evicted. Target a retained entry so
                // the failed cut still exercises restoration after a move.
                record_user(&state, &store, &id, "retained position");
            }
        }
        {
            let mut inner = state.lock_inner();
            // A full undo stack and two redo entries: a restore that pushed an
            // entry would evict the oldest one and clear the redo stack.
            inner.undo_step().unwrap();
            inner.undo_step().unwrap();
        }
        let (source, undo, redo, tempo) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
                inner.undo.redo.len(),
                inner.project.properties.tempo,
            )
        };
        assert_eq!(redo, 2);
        let requests = Requests::default();
        recall_with(
            &requests,
            &state,
            &store,
            &id,
            with_undo(&state, 3),
            |_, _| {
                Err(crate::ai::sessions::turns::CutFailure::Store(
                    "the session store is read-only".into(),
                ))
            },
        )
        .unwrap_err();
        let mut inner = state.lock_inner();
        assert_eq!(inner.dsl_source, source);
        assert_eq!(inner.project.properties.tempo, tempo);
        assert_eq!(
            serde_json::to_value(inner.undo.history(&inner.project.properties)).unwrap(),
            undo,
            "no entry was evicted"
        );
        assert_eq!(inner.undo.redo.len(), redo, "redo is kept");
        // The kept redo still works.
        let mut project = inner.project.clone();
        inner.undo.redo(&mut project).unwrap();
        assert_ne!(project.properties.tempo, tempo);
    }

    #[test]
    fn recall_is_refused_while_a_turn_runs_and_changes_nothing() {
        let (_root, store, id, state, _path) = fixture();
        let requests = Requests::default();
        let _turn = requests
            .claim("turn-1")
            .unwrap()
            .expect("the turn holds the slot");
        let outcome = recall_in(&requests, &state, &store, &id, plain(0)).unwrap();
        assert_eq!(refused(outcome), RecallRefusal::TurnRunning);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
    }

    #[test]
    fn recall_is_refused_on_anything_but_a_person_message() {
        let (_root, store, id, state, _path) = fixture();
        let requests = Requests::default();
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, plain(1)).unwrap()),
            RecallRefusal::NotAPersonMessage,
            "an assistant row"
        );
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, plain(9)).unwrap()),
            RecallRefusal::NotAPersonMessage,
            "past the end"
        );
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
        assert!(requests.active().unwrap().is_none());
    }

    #[test]
    fn recall_with_an_open_batch_changes_nothing() {
        let (_root, store, id, state, _path) = fixture();
        state
            .begin_transaction("turn".into(), "panel:x".into())
            .unwrap();
        let requests = Requests::default();
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, plain(0)).unwrap()),
            RecallRefusal::BatchOpen
        );
        assert_eq!(store.view(&id).unwrap().len(), 3);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
    }

    #[test]
    fn a_recall_and_undo_refuses_a_position_from_another_project_file() {
        let (root, store, id, state, _path) = fixture();
        let other = root.path().join("other.ocp");
        std::fs::write(&other, AFTER).unwrap();
        state.lock_inner().file_path = Some(other);
        let requests = Requests::default();
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap()),
            RecallRefusal::OtherProject
        );
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
        // A plain recall does not touch the document, so it still runs.
        assert!(matches!(
            recall_in(&requests, &state, &store, &id, plain(0)).unwrap(),
            RecallOutcome::Recalled { .. }
        ));
        assert!(store.view(&id).unwrap().is_empty());
    }

    #[test]
    fn a_recall_and_undo_refuses_a_message_without_a_recorded_position() {
        let (_root, store, id, state, _path) = fixture();
        let row = user("no position", &"c".repeat(64));
        store.append_turn(&id, &[row], None).unwrap();
        let requests = Requests::default();
        // The fixture's rows are indices 0..2; the appended row is 3.
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, with_undo(&state, 3)).unwrap()),
            RecallRefusal::NoRecordedPosition
        );
        assert_eq!(store.view(&id).unwrap().len(), 4);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        // A plain recall still cuts at that message.
        assert!(matches!(
            recall_in(&requests, &state, &store, &id, plain(3)).unwrap(),
            RecallOutcome::Recalled { .. }
        ));
        assert_eq!(store.view(&id).unwrap().len(), 3);
    }

    #[test]
    fn a_recall_and_undo_refuses_a_cleared_position() {
        let (_root, store, id, state, path) = fixture();
        let project = crate::ai::sessions::canonical_path(&path)
            .to_string_lossy()
            .into_owned();
        let row = json!({
            "role": "user", "content": "lost",
            "message_meta": sessions::message_meta(sessions::MessageKind::User),
            "history_at": { "project": project, "level": 3, "at": 999_999_u64 },
        });
        store.append_turn(&id, &[row], None).unwrap();
        let requests = Requests::default();
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, with_undo(&state, 3)).unwrap()),
            RecallRefusal::PositionCleared
        );
        assert_eq!(store.view(&id).unwrap().len(), 4);
    }

    #[test]
    fn a_recall_refuses_a_root_rebased_by_history_cleanup() {
        let (_root, store, id, state, _path) = fixture_edited_as(true);
        let _acting = crate::edit_history::act_as(crate::edit_history::Author::Agent {
            session: Some(id.clone()),
        });
        apply_edit(&state, "@project \"Recall\"\n  tempo: 160\n");
        let before = {
            let mut inner = state.lock_inner();
            assert_eq!(inner.undo.clear_older(1).unwrap(), 1);
            (
                inner.project.clone(),
                inner.dsl_source.clone(),
                inner.undo.applied_level(),
                inner.undo.total_levels(),
            )
        };
        let rows = store.view(&id).unwrap();
        let requests = Requests::default();
        assert_eq!(
            refused(recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap()),
            RecallRefusal::PositionCleared,
        );
        let inner = state.lock_inner();
        assert_eq!(inner.project, before.0);
        assert_eq!(inner.dsl_source, before.1);
        assert_eq!(inner.undo.applied_level(), before.2);
        assert_eq!(inner.undo.total_levels(), before.3);
        assert_eq!(store.view(&id).unwrap(), rows);
    }

    #[test]
    fn a_recall_with_undo_is_refused_inside_an_edit_preview() {
        let (_root, store, id, state, _path) = fixture();
        {
            let mut inner = state.lock_inner();
            inner.preview = Some(crate::preview::PreviewSession {
                base_project: inner.project.clone(),
                base_dsl: inner.dsl_source.clone(),
                proposal_project: inner.project.clone(),
                proposal_dsl: inner.dsl_source.clone(),
                subtree_dsl: String::new(),
                side: crate::preview::PreviewSide::Base,
                target_path: "project".into(),
                highlight: crate::preview::PreviewHighlight::default(),
                base_dirty: inner.dirty,
            });
        }
        let requests = Requests::default();
        let outcome = recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap();
        assert_eq!(refused(outcome), RecallRefusal::PreviewOpen);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
    }

    #[test]
    fn a_recall_reports_a_file_it_could_not_write() {
        let root = tempfile::tempdir().unwrap();
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, "not a directory").unwrap();
        let path = blocked.join("song.ocp");
        let store = Store::new(root.path().join("sessions"));
        let id = store.create(None, None, Some("Blocked"), None).unwrap().id;
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(BEFORE).unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.file_path = Some(path);
        }
        record_user(&state, &store, &id, "first");
        let _acting = crate::edit_history::act_as(crate::edit_history::Author::Agent {
            session: Some(id.clone()),
        });
        apply_edit(&state, AFTER);
        let requests = Requests::default();
        let (_, _, snapshot, unsaved) =
            recalled(recall_in(&requests, &state, &store, &id, with_undo(&state, 0)).unwrap());
        assert!(snapshot.is_some());
        assert!(
            unsaved
                .as_deref()
                .is_some_and(|reason| reason.contains("not written")),
            "{unsaved:?}"
        );
        assert_eq!(state.lock_inner().project.properties.tempo, 120.0);
    }

    #[test]
    fn a_stale_answer_changes_nothing_and_cancel_reaches_a_running_turn() {
        let decision = Decision {
            approved: true,
            ..Decision::default()
        };
        assert!(answer("nobody", "c1", decision.clone()).is_err());
        let (sender, receiver) = oneshot::channel();
        let request = ApprovalRequest {
            call_id: "c1".into(),
            tool: "tracks_remove".into(),
            args: Map::new(),
            path: None,
            before: None,
            after: None,
        };
        lock(&PENDING).insert(
            ("t-ans".into(), "c1".into()),
            Waiting::Approval {
                answer: sender,
                request: request.clone(),
            },
        );
        assert_eq!(
            questions("t-ans"),
            vec![request],
            "a reloaded window sees the open card"
        );
        assert!(questions("t-other").is_empty());
        answer("t-ans", "c1", decision.clone()).unwrap();
        assert_eq!(receiver.blocking_recv().unwrap(), decision);
        assert!(
            answer("t-ans", "c1", decision).is_err(),
            "an answer is taken once"
        );

        assert!(!cancel("t-none"));
        let running = Cancel::default();
        lock(&RUNNING).insert(
            "t-run".into(),
            RunningTurn {
                session_id: "s".into(),
                cancel: running.clone(),
                steering: super::super::steering::Steering::default(),
            },
        );
        assert!(cancel("t-run"));
        assert!(running.is_cancelled());
        lock(&RUNNING).remove("t-run");
    }

    #[test]
    fn rounds_have_distinct_bounded_accounting_ids() {
        let id = "r".repeat(MAX_REQUEST_ID);
        assert_eq!(round_request_id(&id, 0), id);
        let last = round_request_id(&id, super::super::wire::MAX_TOOL_ROUNDS - 1);
        assert!(last.len() <= 100, "ledger ids stay within 100 bytes");
        assert_ne!(round_request_id("t", 1), round_request_id("t", 2));
    }
    #[tokio::test]
    async fn steering_is_scoped_bounded_and_rejects_cancelled_turns() {
        let control = RunningTurn {
            session_id: "steer-session".into(),
            cancel: Cancel::default(),
            steering: super::super::steering::Steering::default(),
        };
        lock(&RUNNING).insert("scoped-steering-test".into(), control.clone());
        let message = || SteerMessage {
            content: "continue".into(),
            media: Vec::new(),
            id: None,
        };
        assert_eq!(
            steer("scoped-steering-test", "foreign", message())
                .await
                .unwrap_err()
                .kind,
            "steer_closed"
        );
        assert!(steer("missing", "steer-session", message()).await.is_err());
        assert!(
            steer(
                "scoped-steering-test",
                "steer-session",
                SteerMessage {
                    content: "x".repeat(crate::ai::LIMIT + 1),
                    media: Vec::new(),
                    id: None,
                }
            )
            .await
            .is_err()
        );
        assert!(
            serde_json::from_value::<SteerMessage>(json!({"content":"test", "role":"system"}))
                .is_err()
        );
        assert!(
            control
                .steering
                .take(super::super::steering::Boundary::Continue)
                .is_empty()
        );
        control.cancel.cancel();
        assert!(
            steer("scoped-steering-test", "steer-session", message())
                .await
                .is_err()
        );
        lock(&RUNNING).remove("scoped-steering-test");
    }
    #[tokio::test]
    async fn a_dropped_registration_refuses_pending_steering_and_releases_its_identity() {
        let control = RunningTurn {
            session_id: "unprepared".into(),
            cancel: Cancel::default(),
            steering: super::super::steering::Steering::default(),
        };
        let id = "unprepared-steering-test";
        lock(&RUNNING).insert(id.into(), control.clone());
        let registration = RegisteredTurn {
            request_id: id.into(),
            control: control.clone(),
        };
        let receipt = control
            .steering
            .enqueue(json!({"role":"user", "content":"continue"}))
            .unwrap();
        drop(registration);
        assert!(receipt.await.unwrap().is_err());
        assert!(!lock(&RUNNING).contains_key(id));
        assert!(control.steering.enqueue(json!({})).is_err());
    }

    #[tokio::test]
    async fn an_unanswered_question_expires_as_a_refusal_and_an_answer_wins_before_it() {
        let wait = std::time::Duration::from_millis(20);
        let (_sender, receiver) = oneshot::channel::<Decision>();
        let expired = await_answer(receiver, wait).await;
        assert!(expired.expired && !expired.approved);
        // A question dropped unanswered is refused, never taken as approval.
        let (sender, receiver) = oneshot::channel::<Decision>();
        drop(sender);
        let dropped = await_answer(receiver, wait).await;
        assert!(!dropped.approved && !dropped.expired);

        let (sender, receiver) = oneshot::channel();
        let approved = Decision {
            approved: true,
            ..Decision::default()
        };
        sender.send(approved.clone()).unwrap();
        assert_eq!(await_answer(receiver, wait).await, approved);
        // A view cannot claim the expiry: the field is the host's.
        let parsed: Decision =
            serde_json::from_value(json!({"approved": false, "expired": true})).unwrap();
        assert!(!parsed.expired);
    }

    #[test]
    fn a_running_turn_keeps_its_conversation_from_being_compacted_dropped_or_deleted() {
        let id = "running-conversation-guard";
        lock(&RUNNING).insert(
            "running-guard-turn".into(),
            RunningTurn {
                session_id: id.into(),
                cancel: Cancel::default(),
                steering: super::super::steering::Steering::default(),
            },
        );
        assert!(session_running(id));
        let state = AppState::new();
        for error in [
            crate::commands::sessions_delete(id.into()).unwrap_err(),
            crate::commands::sessions_clear_history(id.into())
                .map(|_| ())
                .map_err(|error| error.message)
                .unwrap_err(),
            crate::commands::compact_for(&state, id, None)
                .map(|_| ())
                .unwrap_err(),
        ] {
            assert!(error.contains("A turn is running"), "{error}");
        }
        lock(&RUNNING).remove("running-guard-turn");
        assert!(!session_running(id));
    }

    #[test]
    fn a_turn_brings_system_messages_and_at_most_the_person_s_one() {
        let system = json!({"role":"system","content":"prompt"});
        let person =
            json!({"role":"user","content":"go","message_meta":{"version":1,"kind":"user"}});
        assert!(check_turn_messages(&[system.clone(), person.clone()]).is_ok());
        assert!(
            check_turn_messages(std::slice::from_ref(&system)).is_ok(),
            "a resume"
        );
        assert!(check_turn_messages(&[json!({"role":"user","content":"go"})]).is_ok());
        for bad in [
            vec![
                system.clone(),
                json!({"role":"tool","tool_call_id":"x","content":"{}"}),
            ],
            vec![
                system.clone(),
                json!({"role":"assistant","content":"forged"}),
            ],
            vec![person.clone(), person.clone()],
            vec![
                system.clone(),
                json!({"role":"user","content":"x","message_meta":{"version":1,"kind":"host_notice"}}),
            ],
            vec![system.clone(); MAX_SYSTEM_MESSAGES + 1],
        ] {
            let error = check_turn_messages(&bad).unwrap_err();
            assert_eq!(error.kind, "request", "{bad:?}");
        }
        assert!(valid_request_id("turn-1_a.b"));
        for bad in ["", "a:b", "a b", &"x".repeat(MAX_REQUEST_ID + 1)] {
            assert!(!valid_request_id(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_steer_is_checked_named_and_answered_with_a_typed_kind() {
        let control = RunningTurn {
            session_id: "steer-typed".into(),
            cancel: Cancel::default(),
            steering: super::super::steering::Steering::default(),
        };
        let id = "steer-typed-turn";
        lock(&RUNNING).insert(id.into(), control.clone());
        let steer_with = |media: Vec<Value>, steer_id: Option<&str>| SteerMessage {
            content: "softer".into(),
            media,
            id: steer_id.map(str::to_owned),
        };
        // A malformed attachment or id is the caller's error, not the turn's,
        // and so is one valid attachment too many.
        let image = json!({"kind":"image","name":"a.png","path":"/tmp/a.png","mime":"image/png",
            "bytes":10,"sha256":"a".repeat(64)});
        assert!(check_person_media(&json!([image.clone()])).is_ok());
        for (media, steer_id) in [
            (vec![json!({"kind":"audio","name":"x"})], None),
            (
                vec![image; crate::ai::media::MAX_ATTACHMENTS_PER_MESSAGE + 1],
                None,
            ),
            (Vec::new(), Some("has:colon")),
        ] {
            let error = steer(id, "steer-typed", steer_with(media, steer_id))
                .await
                .unwrap_err();
            assert_eq!(error.kind, "request");
        }
        // The loop's save failure reaches the caller as a store failure, and
        // the message carries the caller's id.
        let delivery = tokio::spawn(steer(
            id,
            "steer-typed",
            steer_with(Vec::new(), Some("row-7")),
        ));
        let pending = loop {
            let pending = control
                .steering
                .take(super::super::steering::Boundary::Continue);
            if !pending.is_empty() {
                break pending;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(pending[0].message["message_meta"]["steer_id"], "row-7");
        for row in pending {
            let _ = row
                .receipt
                .send(Err(super::super::steering::Undelivered::Save(
                    "disk full".into(),
                )));
        }
        assert_eq!(delivery.await.unwrap().unwrap_err().kind, "session_save");

        // Cancel names the conversation too.
        assert!(!cancel_in(id, "another-conversation"));
        assert!(!control.cancel.is_cancelled());
        assert!(cancel_in(id, "steer-typed"));
        assert!(control.cancel.is_cancelled());
        lock(&RUNNING).remove(id);
    }

    #[test]
    fn a_user_question_and_an_approval_answer_their_own_kind() {
        let key: QuestionKey = ("ask-turn".into(), "q1".into());
        let (sender, receiver) = oneshot::channel();
        lock(&PENDING).insert(
            key,
            Waiting::UserQuestion {
                answer: sender,
                request: UserQuestion {
                    call_id: "q1".into(),
                    question: "Which mood?".into(),
                    options: vec!["Warm".into(), "Bright".into()],
                    free_text: true,
                    max_chars: crate::ai::agent_loop::MAX_ANSWER_CHARS,
                },
            },
        );
        let pending = pending_questions("ask-turn");
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            &pending[0],
            PendingQuestion::UserQuestion { request } if request.question == "Which mood?"
        ));
        assert!(
            questions("ask-turn").is_empty(),
            "an approval view stays empty"
        );
        assert!(
            answer("ask-turn", "q1", Decision::default()).is_err(),
            "an approval answer does not reach a question"
        );
        let reply_value = Reply {
            option: Some(1),
            ..Reply::default()
        };
        reply("ask-turn", "q1", reply_value.clone()).unwrap();
        assert_eq!(receiver.blocking_recv().unwrap(), reply_value);
        assert_eq!(
            reply("ask-turn", "q1", Reply::default()),
            Err(ReplyError::NotWaiting)
        );
    }

    #[test]
    fn a_question_answer_outside_its_bounds_is_refused_and_stays_pending() {
        let key: QuestionKey = ("ask-bounds".into(), "q1".into());
        let (sender, _receiver) = oneshot::channel();
        lock(&PENDING).insert(
            key.clone(),
            Waiting::UserQuestion {
                answer: sender,
                request: UserQuestion {
                    call_id: "q1".into(),
                    question: "Which mood?".into(),
                    options: vec!["Warm".into(), "Bright".into()],
                    free_text: false,
                    max_chars: crate::ai::agent_loop::MAX_ANSWER_CHARS,
                },
            },
        );
        assert_eq!(
            reply(
                "ask-bounds",
                "q1",
                Reply {
                    option: Some(2),
                    text: None,
                    ..Reply::default()
                }
            ),
            Err(ReplyError::OptionOutOfRange)
        );
        assert_eq!(
            reply(
                "ask-bounds",
                "q1",
                Reply {
                    option: None,
                    text: Some("warm".into()),
                    ..Reply::default()
                }
            ),
            Err(ReplyError::TextNotAccepted)
        );
        assert_eq!(
            pending_questions("ask-bounds").len(),
            1,
            "a refused answer leaves the question open"
        );

        // A question that accepts text still bounds it.
        lock(&PENDING).remove(&key);
        let (sender, receiver) = oneshot::channel();
        lock(&PENDING).insert(
            key.clone(),
            Waiting::UserQuestion {
                answer: sender,
                request: UserQuestion {
                    call_id: "q1".into(),
                    question: "Which mood?".into(),
                    options: Vec::new(),
                    free_text: true,
                    max_chars: crate::ai::agent_loop::MAX_ANSWER_CHARS,
                },
            },
        );
        assert_eq!(
            reply(
                "ask-bounds",
                "q1",
                Reply {
                    option: None,
                    text: Some("x".repeat(super::super::MAX_ANSWER_CHARS + 1)),
                    ..Reply::default()
                },
            ),
            Err(ReplyError::TextTooLong)
        );
        let accepted = Reply {
            option: None,
            text: Some("warm".into()),
            ..Reply::default()
        };
        reply("ask-bounds", "q1", accepted.clone()).unwrap();
        assert_eq!(receiver.blocking_recv().unwrap(), accepted);
    }

    #[test]
    fn a_question_leaves_the_list_when_its_wait_is_dropped() {
        let key: QuestionKey = ("asked-guard-turn".into(), "c1".into());
        let (sender, _receiver) = oneshot::channel();
        lock(&PENDING).insert(
            key.clone(),
            Waiting::Approval {
                answer: sender,
                request: ApprovalRequest {
                    call_id: "c1".into(),
                    tool: "tracks_remove".into(),
                    args: Map::new(),
                    path: None,
                    before: None,
                    after: None,
                },
            },
        );
        let guard = AskedQuestion(key.clone());
        assert_eq!(questions("asked-guard-turn").len(), 1);
        drop(guard);
        assert!(questions("asked-guard-turn").is_empty());
    }
}
