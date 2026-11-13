//! The application host of the agent loop, and the commands a view drives a
//! turn with. The desktop panel and `POST /ai/turn` run the same host; they
//! differ only in whether a person answers approvals.
//!
//! A turn owns the backend's one provider slot from start to end, so nothing
//! else (a second turn, a rewind, a revert) can write the conversation while
//! it runs. Its project edits go into one batch owned by the conversation,
//! which the loop commits on every ending.

use super::{
    ApprovalRequest, AudioShape, Cancel, Decision, Document, Host, TurnEvent, TurnOutcome, TurnSpec,
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
/// Longest turn id a client may choose; see [`round_request_id`].
const MAX_REQUEST_ID: usize = 96;

/// A question's key: the turn's request id and the call.
type QuestionKey = (String, String);
/// A question waiting for a person, kept so a window that reloaded can ask
/// it again.
struct Question {
    answer: oneshot::Sender<Decision>,
    request: ApprovalRequest,
}
/// Questions waiting for a person.
static PENDING: LazyLock<Mutex<HashMap<QuestionKey, Question>>> = LazyLock::new(Mutex::default);
/// Cancel handles of running turns, keyed by request id.
static RUNNING: LazyLock<Mutex<HashMap<String, Cancel>>> = LazyLock::new(Mutex::default);

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
    async fn send(&self, wire: &[Value], round: usize) -> Result<Value, AiError> {
        let admitted = lock(&self.round_stats).clone();
        let stats = match admitted {
            Some(stats) => stats,
            None => {
                let stats = admit_round(
                    &self.provider,
                    &self.session_id,
                    &round_request_id(&self.request_id, round),
                )?;
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
        self.blocking(move |app| {
            let state = app.state::<AppState>();
            // A panel batch a dead turn stranded is this conversation's own;
            // it commits, so its applied edits stay. A headless owner's open
            // batch belongs to a client's live transaction, so it is refused
            // by the begin below instead of committed under the client.
            if stranded_panel_batch(state.batch_owner().as_deref(), &owner) {
                state.commit_transaction()?;
            }
            state.begin_transaction(label, owner).map(|_| ())
        })
        .await?
    }

    async fn commit_batch(&self) -> Result<(), String> {
        let owner = self.owner.clone();
        let result = self
            .blocking(move |app| {
                let state = app.state::<AppState>();
                crate::agent_api::transaction_commit(&state, &owner)?;
                // The commit publishes the document; the file is its durable record.
                if let Err(reason) = crate::project_persist::flush_state(&state) {
                    log::error!("[ai] committed batch not written to the project file: {reason}");
                }
                Ok(())
            })
            .await?;
        self.publish();
        result
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
                return Some(Decision {
                    approved: false,
                    target: None,
                    always: false,
                });
            }
            pending.insert(
                key.clone(),
                Question {
                    answer: sender,
                    request: request.clone(),
                },
            );
        }
        let tool = request.tool.clone();
        self.emit(TurnEvent::Ask { request });
        let decision = receiver.await.ok();
        lock(&PENDING).remove(&key);
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

    fn allowed(&self) -> Vec<String> {
        lock(&self.allowed).clone()
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

    async fn started(&self, call_id: &str) -> Result<(), String> {
        let (id, call) = (self.session_id.clone(), call_id.to_owned());
        self.blocking(move |_| Store::config().mark_started(&id, &call))
            .await?
    }

    async fn close_round(&self) -> Result<Vec<Value>, String> {
        let id = self.session_id.clone();
        self.blocking(move |_| Store::config().close_round(&id))
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

    async fn apply_edit(&self, path: &str, dsl: &str, expected: &str) -> Result<Document, String> {
        let (path, dsl, expected) = (path.to_owned(), dsl.to_owned(), expected.to_owned());
        let snapshot = self
            .blocking(move |app| {
                let state = app.state::<AppState>();
                let mut inner = state.lock_inner();
                crate::commands::apply_edit_to_project(&mut inner, dsl, path, expected)
            })
            .await??;
        self.publish();
        Ok(Document {
            dsl: snapshot.dsl_source,
            revision: snapshot.revision,
            track_count: snapshot.track_count,
        })
    }

    async fn subtree(&self, path: &str) -> String {
        let state = self.state();
        let inner = state.lock_inner();
        oc_dsl::ops::extract(&inner.project, path)
            .map(|extract| extract.dsl_text)
            .unwrap_or_default()
    }

    async fn undo(&self) -> Result<(), String> {
        let result = self
            .blocking(|app| app.state::<AppState>().lock_inner().undo_step().map(|_| ()))
            .await?;
        self.publish();
        result
    }
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
    if start.request_id.is_empty() || start.request_id.len() > MAX_REQUEST_ID {
        return Err(AiError::config("Invalid AI request identifier"));
    }
    let messages = Value::Array(start.messages.clone());
    crate::ai_service::validate_chat_messages(&messages)?;
    let provider = crate::ai_service::provider(&start.provider_id).map_err(AiError::config)?;
    let requests = app.state::<Requests>();
    // The cancel handle is published before the slot is claimed, so a cancel
    // that arrives at any point reaches the turn instead of freeing its slot.
    let cancel = Cancel::default();
    {
        let mut running = lock(&RUNNING);
        if running.contains_key(&start.request_id) {
            return Err(AiError::new("busy", "Another AI request is active"));
        }
        running.insert(start.request_id.clone(), cancel.clone());
    }
    let claim = match requests.claim(&start.request_id) {
        Ok(Some(claim)) => claim,
        refused => {
            lock(&RUNNING).remove(&start.request_id);
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
    let outcome = run_claimed(app, &start, &provider, channels, attended, owner, cancel).await;
    lock(&RUNNING).remove(&start.request_id);
    lock(&PENDING).retain(|(turn, _), _| turn != &start.request_id);
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
    cancel: Cancel,
) -> Result<TurnOutcome, AiError> {
    let stats = admit_round(provider, &start.session_id, &start.request_id)?;
    let incarnation = stats
        .accounting()
        .map(|lease| lease.update().incarnation)
        .ok_or_else(|| AiError::new("accounting_request", "The turn has no accounting owner"))?;
    let messages = Value::Array(start.messages.clone());
    // Preparation closes any round a dead turn left open, saves the new user
    // message with its snapshot, and compacts when the window needs it.
    let prepared = {
        let (app, provider, id, incarnation, project) = (
            app.clone(),
            provider.clone(),
            start.session_id.clone(),
            incarnation.clone(),
            start.project.clone(),
        );
        tauri::async_runtime::spawn_blocking(move || {
            crate::commands::prepare_turn_for(
                &app,
                &provider,
                &messages,
                crate::commands::SavedTurnBinding {
                    id: &id,
                    incarnation: &incarnation,
                    project: project.as_deref(),
                },
            )
        })
        .await
        .map_err(|error| format!("Session persistence task failed: {error}"))
        .and_then(|result| result)
    };
    let prepared = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            finish_accounting(app, &stats, &start.request_id).await;
            return Err(AiError::new(
                "session_save",
                "The saved conversation cannot be replayed",
            ));
        }
        Err(error) => {
            finish_accounting(app, &stats, &start.request_id).await;
            return Err(AiError::new("session_save", error));
        }
    };
    if let Some(compaction) = prepared.compacted.as_ref() {
        let _ = app.emit(
            crate::commands::AUTO_SESSION_EVENT,
            crate::commands::compaction_event(&prepared, compaction, provider),
        );
    }
    let permission = Store::config()
        .find(&start.session_id)
        .map(|(_, meta)| meta.permission)
        .unwrap_or_default();
    let host = AppHost {
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
        permission: turn_permission(attended, permission),
        label: start
            .label
            .clone()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "AI edit".into()),
        audio: audio_shape(provider),
        scope: start.session_id.clone(),
        toolsets: start.toolsets.clone(),
    };
    let outcome = super::run_turn(&host, spec, cancel).await;
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

/// Whether the open batch is a panel turn's batch that a dead turn of the
/// same conversation left behind. Only panel turns strand batches: every
/// other owner's batch is a client's own, still in use.
fn stranded_panel_batch(open: Option<&str>, owner: &str) -> bool {
    open == Some(owner) && owner.starts_with(crate::state::PANEL_BATCH_OWNER_PREFIX)
}

/// Answers a question a running turn asked. A stale answer changes nothing.
pub fn answer(request_id: &str, call_id: &str, decision: Decision) -> Result<(), String> {
    let question = lock(&PENDING)
        .remove(&(request_id.to_owned(), call_id.to_owned()))
        .ok_or("Nothing is waiting for an answer to this question")?;
    question
        .answer
        .send(decision)
        .map_err(|_| "The turn stopped before the answer arrived".to_string())
}

/// Stops a running turn. Completed calls keep their results.
pub fn cancel(request_id: &str) -> bool {
    match lock(&RUNNING).get(request_id) {
        Some(cancel) => {
            cancel.cancel();
            true
        }
        None => false,
    }
}

/// Restores the project to the snapshot a person's message was sent against
/// and cuts the conversation just before that message, as one step: the
/// project change is one undo entry, and a cut that fails puts the document
/// back. Refused while any turn runs.
pub fn revert_before(app: &AppHandle, session_id: &str, index: usize) -> Result<Value, String> {
    revert_in(
        &app.state::<Requests>(),
        &app.state::<AppState>(),
        &Store::config(),
        session_id,
        index,
    )
}

/// [`revert_before`] over its parts.
fn revert_in(
    requests: &Requests,
    state: &AppState,
    store: &Store,
    session_id: &str,
    index: usize,
) -> Result<Value, String> {
    revert_with(requests, state, store, session_id, index, |store| {
        store.cut_before_user(session_id, index)
    })
}

/// The revert with its cut supplied, so a test can make the cut fail.
///
/// The document lock is held from the restore through the cut, so no other
/// edit lands between them and an undo after a failed cut removes exactly
/// the restore. Nothing is written to disk until the cut has succeeded.
fn revert_with(
    requests: &Requests,
    state: &AppState,
    store: &Store,
    session_id: &str,
    index: usize,
    cut: impl FnOnce(&Store) -> Result<usize, String>,
) -> Result<Value, String> {
    // Holding the slot for the whole revert keeps a turn from starting in it.
    let slot = format!("revert-{session_id}");
    let Some(claim) = requests.claim(&slot)? else {
        return Err("A turn is still running; stop it before reverting.".into());
    };
    let _release = scopeguard(requests, &slot, &claim.identity);
    let point = store.revert_point(session_id, index)?;
    let dsl = store.dsl_version(session_id, &point.dsl_sha)?;
    let mut inner = state.lock_inner();
    if inner.transaction.is_some() {
        return Err("A batch of edits is still open; finish it before reverting.".into());
    }
    let before = (inner.undo.mark(), inner.dsl_source.clone(), inner.dirty);
    let restored = inner.dsl_source != dsl;
    if restored {
        crate::commands::restore_dsl_with_undo(&mut inner, dsl, "Revert conversation")?;
    }
    // The session store never takes the document lock, so holding it here
    // cannot deadlock.
    if let Err(error) = cut(store) {
        if restored {
            let (mark, source, dirty) = before;
            match inner.undo_step() {
                Ok(_) => {
                    // The undo leaves the restore as a redo entry; the step
                    // never happened, so it goes, and the exact text returns.
                    inner.undo.restore(mark);
                    inner.dsl_source = source;
                    inner.dirty = dirty;
                }
                Err(undo) => log::error!("[ai] revert could not put the document back: {undo}"),
            }
        }
        return Err(error);
    }
    if restored {
        crate::commands::publish_restored(&mut inner);
    }
    let snapshot = crate::commands::shared::snapshot(&inner);
    drop(inner);
    super::results::with_results(|results| results.forget(session_id));
    Ok(json!({ "kept": index, "snapshot": snapshot }))
}

/// The questions a running turn is waiting on, for a window that reloaded
/// while the card was open.
pub fn questions(request_id: &str) -> Vec<ApprovalRequest> {
    lock(&PENDING)
        .iter()
        .filter(|((turn, _), _)| turn == request_id)
        .map(|(_, question)| question.request.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sessions;
    use serde_json::Map;

    const BEFORE: &str = "@project \"Revert\"\n  tempo: 120\n";
    const AFTER: &str = "@project \"Revert\"\n  tempo: 150\n";

    fn user(text: &str, sha: &str) -> Value {
        json!({
            "role": "user", "content": text, "dsl_sha": sha,
            "message_meta": sessions::message_meta(sessions::MessageKind::User),
        })
    }

    /// A conversation of two user messages, the first sent against `BEFORE`,
    /// and a project that has moved on to `AFTER`.
    fn fixture() -> (tempfile::TempDir, Store, String, AppState) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let id = store.create(None, None, Some("Revert"), None).unwrap().id;
        let before = store
            .snapshot_dsl(&id, &oc_dsl::serialize(&oc_dsl::parse(BEFORE).unwrap()))
            .unwrap();
        let after = store.snapshot_dsl(&id, AFTER).unwrap();
        store
            .append_turn(
                &id,
                &[
                    user("first", &before),
                    json!({"role": "assistant", "content": "done"}),
                ],
                None,
            )
            .unwrap();
        store
            .append_turn(&id, &[user("second", &after)], None)
            .unwrap();
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(AFTER).unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        (root, store, id, state)
    }

    #[test]
    fn revert_restores_cuts_and_undoes_as_one_step() {
        let (_root, store, id, state) = fixture();
        let requests = Requests::default();
        let answer = revert_in(&requests, &state, &store, &id, 0).unwrap();
        assert_eq!(answer["kept"], 0);
        assert_eq!(state.lock_inner().project.properties.tempo, 120.0);
        assert!(
            store.view(&id).unwrap().is_empty(),
            "the conversation is cut before the message"
        );
        assert!(requests.active().unwrap().is_none(), "the slot is released");
        state.lock_inner().undo_step().unwrap();
        assert_eq!(
            state.lock_inner().project.properties.tempo,
            150.0,
            "one undo takes it back"
        );
    }

    #[test]
    fn revert_is_refused_while_a_turn_runs_and_changes_nothing() {
        let (_root, store, id, state) = fixture();
        let requests = Requests::default();
        let _turn = requests
            .claim("turn-1")
            .unwrap()
            .expect("the turn holds the slot");
        let error = revert_in(&requests, &state, &store, &id, 0).unwrap_err();
        assert!(error.contains("still running"), "{error}");
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
    }

    #[test]
    fn revert_is_refused_on_anything_but_a_user_message() {
        let (_root, store, id, state) = fixture();
        let requests = Requests::default();
        assert!(
            revert_in(&requests, &state, &store, &id, 1).is_err(),
            "an assistant row"
        );
        assert!(
            revert_in(&requests, &state, &store, &id, 9).is_err(),
            "past the end"
        );
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
        assert_eq!(store.view(&id).unwrap().len(), 3);
        assert!(requests.active().unwrap().is_none());
    }

    #[test]
    fn an_attended_turn_keeps_its_mode_and_an_unattended_one_writes() {
        for mode in [
            SessionPermission::RequireApproval,
            SessionPermission::Autoreview,
            SessionPermission::Write,
        ] {
            assert_eq!(turn_permission(true, mode), mode);
            assert_eq!(turn_permission(false, mode), SessionPermission::Write);
            // A confirm call still needs a person in the mode a headless turn
            // runs under, and the headless host has nobody to answer.
            assert_eq!(
                super::super::gate::gate("tracks_remove", turn_permission(false, mode), &[]).need,
                super::super::gate::Need::Approval
            );
        }
    }

    #[test]
    fn only_a_panel_batch_of_the_same_conversation_counts_as_stranded() {
        let panel = crate::state::panel_batch_owner(Some("s1"));
        let http = crate::state::http_session_owner("s1");
        assert!(stranded_panel_batch(Some(&panel), &panel));
        assert!(
            !stranded_panel_batch(Some(&http), &http),
            "a headless client's open transaction is its own"
        );
        assert!(!stranded_panel_batch(
            Some(&crate::state::panel_batch_owner(Some("s2"))),
            &panel
        ));
        assert!(!stranded_panel_batch(None, &panel));
    }

    #[test]
    fn a_failed_cut_leaves_the_document_and_its_history_as_they_were() {
        let (_root, store, id, state) = fixture();
        let (source, revision_labels, redo) = {
            let inner = state.lock_inner();
            (
                inner.dsl_source.clone(),
                inner.undo.undo_labels(),
                inner.undo.redo_labels(),
            )
        };
        let requests = Requests::default();
        let error = revert_with(&requests, &state, &store, &id, 0, |_| {
            Err("the session store is read-only".into())
        })
        .unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        let inner = state.lock_inner();
        assert_eq!(inner.dsl_source, source, "the exact text is back");
        assert_eq!(inner.project.properties.tempo, 150.0);
        assert_eq!(
            inner.undo.undo_labels(),
            revision_labels,
            "no undo entry stays"
        );
        assert_eq!(inner.undo.redo_labels(), redo, "and no redo entry appears");
        drop(inner);
        assert_eq!(
            store.view(&id).unwrap().len(),
            3,
            "the conversation is whole"
        );
        assert!(requests.active().unwrap().is_none(), "the slot is released");
    }

    #[test]
    fn revert_with_an_open_batch_changes_nothing() {
        let (_root, store, id, state) = fixture();
        state
            .begin_transaction("turn".into(), "panel:x".into())
            .unwrap();
        let requests = Requests::default();
        assert!(revert_in(&requests, &state, &store, &id, 0).is_err());
        assert_eq!(store.view(&id).unwrap().len(), 3);
        assert_eq!(state.lock_inner().project.properties.tempo, 150.0);
    }

    #[test]
    fn a_stale_answer_changes_nothing_and_cancel_reaches_a_running_turn() {
        let decision = Decision {
            approved: true,
            target: None,
            always: false,
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
            Question {
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
        lock(&RUNNING).insert("t-run".into(), running.clone());
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
}
