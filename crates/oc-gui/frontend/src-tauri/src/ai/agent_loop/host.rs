//! The application host of the agent loop, and the commands a view drives a
//! turn with. The desktop panel and `POST /ai/turn` run the same host; they
//! differ only in whether a person answers approvals.
//!
//! A turn owns the backend's one provider slot from start to end, so nothing
//! else (a second turn, a rewind, a revert) can write the conversation while
//! it runs. Its project edits go into one batch owned by the conversation,
//! which the loop commits on every ending.

pub mod batch;
pub mod questions;

pub(crate) use batch::{begin_loop_batch, commit_batch_and_save, commit_stranded};

pub use questions::{AnswerError, ReplyError, answer, pending_questions, reply};

use super::{
    ApprovalRequest, AudioShape, Cancel, Decision, Dispatched, Document, Edited, Host, Reply,
    TurnEvent, TurnOutcome, TurnSpec, UserQuestion,
};
use crate::ai::sessions::{SessionPermission, Store};
use crate::ai::timing::TurnStatsHandle;
use crate::ai_service::{AiError, Requests, TurnChannels, TurnRequest};
use crate::commands::AiProviderEntry;
use crate::state::AppState;
use questions::{PENDING, Waiting, wait_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// The event a view hears for its turn.
pub const TURN_EVENT: &str = "ai_turn";
/// The document a turn changed, for the panel running it.
pub const PROJECT_EVENT: &str = "ai_turn_project";
/// Longest turn id a client may choose; see [`round_request_id`].
const MAX_REQUEST_ID: usize = 96;

/// Running turn controls, keyed by request id.
static RUNNING: LazyLock<Mutex<HashMap<String, RunningTurn>>> = LazyLock::new(Mutex::default);

#[derive(Clone)]
struct RunningTurn {
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
        questions::lock(&PENDING).retain(|(turn, _), _| turn != &self.request_id);
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
    requests: &Requests,
    request_id: &str,
    session_id: &str,
    message: SteerMessage,
) -> Result<(), AiError> {
    if !requests
        .session_is(request_id, session_id)
        .map_err(|error| AiError::new("request", error))?
    {
        return Err(AiError::new(
            "steer_closed",
            "No matching turn is accepting messages",
        ));
    }
    let turn = lock(&RUNNING)
        .get(request_id)
        .filter(|turn| !turn.cancel.is_cancelled())
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
pub fn cancel_in(requests: &Requests, request_id: &str, session_id: &str) -> bool {
    requests.session_is(request_id, session_id).unwrap_or(false)
        && requests.cancel(request_id).unwrap_or(false)
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

    async fn call(&self, tool: &str, args: &Value, cancel: &Cancel) -> Result<Dispatched, Value> {
        let refusal = crate::ai::skill_hints::refusal;
        let checked = crate::tool_registry::panel_tool_in(tool, self.toolsets.as_deref())
            .and_then(|def| crate::tool_registry::validate_args(&def, args));
        if let Err(message) = checked {
            return Err(refusal(&message));
        }
        let (tool, args, owner) = (tool.to_owned(), args.clone(), self.owner.clone());
        let stop = cancel.flag();
        let result = self
            .blocking(move |app| {
                if tool == "separate_stems" {
                    return crate::agent_api::separate_stems_cancellable(&args, Some(&stop)).map(
                        |value| Dispatched {
                            value,
                            changes: None,
                        },
                    );
                }
                crate::agent_api::dispatch_panel(&app, &tool, &args, &owner)
            })
            .await;
        let result = result
            .map_err(|error| refusal(&error))?
            .map_err(|error| refusal(&error));
        if result
            .as_ref()
            .is_ok_and(|dispatched| dispatched.value["revision"].is_u64())
        {
            self.publish();
        }
        result
    }

    async fn ask(&self, request: ApprovalRequest) -> Option<Decision> {
        if !self.attended {
            return None;
        }
        let key = (self.request_id.clone(), request.call_id.clone());
        let tool = request.tool.clone();
        let (sender, receiver) = oneshot::channel();
        // A person is there but cannot be asked: that is a refusal, never the
        // "nobody attends" answer that lets a call apply.
        let Some(decision) = wait_for(
            key.clone(),
            Waiting::Approval {
                answer: sender,
                request: request.clone(),
            },
            receiver,
            Decision::expired(),
            Decision::default(),
            || self.emit(TurnEvent::Ask { request }),
        )
        .await
        else {
            return Some(Decision::default());
        };
        if decision.expired {
            self.emit(TurnEvent::Expired { call_id: key.1 });
        }
        if decision.approved && decision.always {
            let mut allowed = lock(&self.allowed);
            if !allowed.contains(&tool) {
                allowed.push(tool);
            }
        }
        Some(decision)
    }

    async fn ask_user(&self, request: UserQuestion) -> Option<Reply> {
        if !self.attended {
            return None;
        }
        let key = (self.request_id.clone(), request.call_id.clone());
        let (sender, receiver) = oneshot::channel();
        let Some(reply) = wait_for(
            key.clone(),
            Waiting::UserQuestion {
                answer: sender,
                request: request.clone(),
            },
            receiver,
            Reply::expired(),
            Reply::expired(),
            || self.emit(TurnEvent::AskUser { request }),
        )
        .await
        else {
            return Some(Reply::busy());
        };
        if reply.expired {
            self.emit(TurnEvent::Expired { call_id: key.1 });
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
        let (summary, provider_failure) = match summary {
            Ok(summary) => (Some(summary), None),
            Err(CompactionFailure::Cancelled) => return Err(CompactionFailure::Cancelled),
            Err(failure) => (None, Some(failure)),
        };
        if cancel.is_cancelled() {
            return Err(CompactionFailure::Cancelled);
        }
        let resolution = self
            .blocking(move |_| Store::config().finish_compaction(plan, summary.as_deref()))
            .await
            .map_err(|_| CompactionFailure::Store)?;
        let failure = provider_failure.or(resolution.failure);
        let outcome = resolution
            .compaction
            .ok_or(failure.unwrap_or(CompactionFailure::Store))?;
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

    async fn stored_result(&self, _call: &str, sha: &str) -> Option<String> {
        let (id, sha) = (self.session_id.clone(), sha.to_owned());
        self.blocking(move |_| Store::config().stored_result(&id, &sha))
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
    let claim = match requests.claim_for(&start.request_id, Some(&start.session_id)) {
        Ok(Some(claim)) => claim,
        refused => {
            return Err(match refused {
                Err(error) => AiError::config(error),
                _ => AiError::new("busy", "Another AI request is active"),
            });
        }
    };
    let release = scopeguard(&requests, &start.request_id, &claim.identity);
    let control = RunningTurn {
        cancel: claim.cancel.clone(),
        steering: super::steering::Steering::default(),
    };
    lock(&RUNNING).insert(start.request_id.clone(), control.clone());
    let _registration = RegisteredTurn {
        request_id: start.request_id.clone(),
        control: control.clone(),
    };
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

pub(crate) struct Release<'a> {
    requests: &'a Requests,
    id: String,
    owner: Arc<()>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let _ = self.requests.release_owned(&self.id, &self.owner);
    }
}

pub(crate) fn scopeguard<'a>(requests: &'a Requests, id: &str, owner: &Arc<()>) -> Release<'a> {
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
            request_id: Some(&start.request_id),
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

#[cfg(test)]
mod tests {
    use super::questions::{AskedQuestion, MAX_PENDING, QuestionKey};
    use super::*;
    use crate::ai::agent_loop::PendingQuestion;
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
            pending_questions("t-ans"),
            vec![PendingQuestion::Approval {
                request: request.clone(),
            }],
            "a reloaded window sees the open card"
        );
        assert!(pending_questions("t-other").is_empty());
        answer("t-ans", "c1", decision.clone()).unwrap();
        assert_eq!(receiver.blocking_recv().unwrap(), decision);
        assert!(
            answer("t-ans", "c1", decision).is_err(),
            "an answer is taken once"
        );

        let requests = Requests::default();
        assert!(!requests.cancel("t-none").unwrap());
        let claim = requests.claim_for("t-run", Some("s")).unwrap().unwrap();
        assert!(requests.cancel("t-run").unwrap());
        assert!(claim.cancel.is_cancelled());
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
            cancel: Cancel::default(),
            steering: super::super::steering::Steering::default(),
        };
        let requests = Requests::default();
        let _claim = requests
            .claim_for("scoped-steering-test", Some("steer-session"))
            .unwrap()
            .unwrap();
        lock(&RUNNING).insert("scoped-steering-test".into(), control.clone());
        let message = || SteerMessage {
            content: "continue".into(),
            media: Vec::new(),
            id: None,
        };
        assert_eq!(
            steer(&requests, "scoped-steering-test", "foreign", message())
                .await
                .unwrap_err()
                .kind,
            "steer_closed"
        );
        assert!(
            steer(&requests, "missing", "steer-session", message())
                .await
                .is_err()
        );
        assert!(
            steer(
                &requests,
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
            steer(
                &requests,
                "scoped-steering-test",
                "steer-session",
                message()
            )
            .await
            .is_err()
        );
        lock(&RUNNING).remove("scoped-steering-test");
    }
    #[tokio::test]
    async fn a_dropped_registration_refuses_pending_steering_and_releases_its_identity() {
        let control = RunningTurn {
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
        let request = ApprovalRequest {
            call_id: "q".into(),
            tool: "tracks_remove".into(),
            args: Map::new(),
            path: None,
            before: None,
            after: None,
        };
        let (_sender, receiver) = oneshot::channel::<Decision>();
        let expired = match tokio::time::timeout(wait, receiver).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => Decision::default(),
            Err(_) => Decision::expired(),
        };
        assert!(expired.expired && !expired.approved);
        // A question dropped unanswered is refused, never taken as approval.
        let (sender, receiver) = oneshot::channel::<Decision>();
        drop(sender);
        let dropped = match tokio::time::timeout(wait, receiver).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => Decision::default(),
            Err(_) => Decision::expired(),
        };
        assert!(!dropped.approved && !dropped.expired);

        let (sender, receiver) = oneshot::channel();
        let approved = Decision {
            approved: true,
            ..Decision::default()
        };
        let key = ("answered".to_owned(), "q".into());
        let awaiting = wait_for(
            key.clone(),
            Waiting::Approval {
                answer: sender,
                request,
            },
            receiver,
            Decision::expired(),
            Decision::default(),
            || {},
        );
        let answerer = async {
            while !lock(&PENDING).contains_key(&key) {
                tokio::task::yield_now().await;
            }
            answer(&key.0, &key.1, approved.clone()).unwrap();
        };
        let (waited, ()) = tokio::join!(awaiting, answerer);
        assert_eq!(waited.unwrap(), approved);
        // A view cannot claim the expiry: the field is the host's.
        let parsed: Decision =
            serde_json::from_value(json!({"approved": false, "expired": true})).unwrap();
        assert!(!parsed.expired);
    }

    #[tokio::test]
    async fn a_full_question_queue_refuses_instead_of_waiting() {
        let turn = "full-question-turn";
        for index in 0..MAX_PENDING {
            let (sender, _receiver) = oneshot::channel();
            lock(&PENDING).insert(
                (turn.into(), format!("c{index}")),
                Waiting::Approval {
                    answer: sender,
                    request: ApprovalRequest {
                        call_id: format!("c{index}"),
                        tool: "tracks_remove".into(),
                        args: Map::new(),
                        path: None,
                        before: None,
                        after: None,
                    },
                },
            );
        }
        let (sender, receiver) = oneshot::channel();
        let refused = wait_for(
            (turn.into(), "extra".into()),
            Waiting::Approval {
                answer: sender,
                request: ApprovalRequest {
                    call_id: "extra".into(),
                    tool: "tracks_remove".into(),
                    args: Map::new(),
                    path: None,
                    before: None,
                    after: None,
                },
            },
            receiver,
            Decision::expired(),
            Decision::default(),
            || {},
        )
        .await;
        assert!(refused.is_none(), "a full queue refuses the question");
        lock(&PENDING).retain(|(key, _), _| key != turn);
    }

    #[test]
    fn a_running_turn_keeps_its_conversation_from_being_compacted_dropped_or_deleted() {
        let id = "running-conversation-guard";
        let requests = Requests::default();
        let claim = requests
            .claim_for("running-guard-turn", Some(id))
            .unwrap()
            .unwrap();
        assert!(requests.session_busy(id).unwrap());
        assert!(
            crate::commands::refuse_while_running(&requests, id)
                .unwrap_err()
                .message
                .contains("A turn is running")
        );
        requests
            .release_owned("running-guard-turn", &claim.identity)
            .unwrap();
        assert!(!requests.session_busy(id).unwrap());
        assert!(crate::commands::refuse_while_running(&requests, id).is_ok());
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
        let requests = Requests::default();
        let _claim = requests
            .claim_for("steer-typed-turn", Some("steer-typed"))
            .unwrap()
            .unwrap();
        let control = RunningTurn {
            cancel: _claim.cancel.clone(),
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
            let error = steer(&requests, id, "steer-typed", steer_with(media, steer_id))
                .await
                .unwrap_err();
            assert_eq!(error.kind, "request");
        }
        // The loop's save failure reaches the caller as a store failure, and
        // the message carries the caller's id.
        let delivery = {
            let requests = requests.clone();
            tokio::spawn(async move {
                steer(
                    &requests,
                    id,
                    "steer-typed",
                    steer_with(Vec::new(), Some("row-7")),
                )
                .await
            })
        };
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
        assert!(!cancel_in(&requests, id, "another-conversation"));
        assert!(!control.cancel.is_cancelled());
        assert!(cancel_in(&requests, id, "steer-typed"));
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
        assert_eq!(
            pending_questions("ask-turn").len(),
            1,
            "the question still waits for its own answer"
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
        assert_eq!(pending_questions("asked-guard-turn").len(), 1);
        drop(guard);
        assert!(pending_questions("asked-guard-turn").is_empty());
    }
}
