//! Saved AI conversations: the command surface, the project facts a summary
//! names, and the autosave hook the chat path calls on every turn.
//!
//! The commands reach the store at `~/.opencompose/ai/sessions` through
//! [`Store::config`]. Nothing here writes to the project.
use crate::ai::sessions::{
    self, Compaction, Fork, Loaded, ProjectFacts, Scope, SessionMeta, Store,
};
use crate::ai_service::{ActiveTurn, Requests};
use crate::commands::AiProviderEntry;
use crate::state::{AppState, AppStateInner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tauri::{Manager, State};

/// Tauri event emitted when the autosave session changes.
pub const AUTO_SESSION_EVENT: &str = "ai_session";

fn store() -> Store {
    Store::config()
}

/// The project file a request is about: the given path, or the open one.
fn project_of(project: Option<&str>, inner: &AppStateInner) -> Option<PathBuf> {
    requested_project(project).or_else(|| inner.file_path.clone())
}

/// The project a request named itself; `None` when it named nothing.
fn requested_project(project: Option<&str>) -> Option<PathBuf> {
    project
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The workspace of a project file: its directory. A request may name the
/// directory itself, so both forms work on the agent surface.
pub(crate) fn workspace_of(path: Option<&Path>) -> Option<PathBuf> {
    let path = path?;
    if path.is_dir() || path.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR) {
        return Some(path.to_path_buf());
    }
    path.parent().map(Path::to_path_buf)
}

/// What a summary names instead of carrying the project: the path, the digest of
/// the DSL it was written against, and the track count.
pub fn facts(inner: &AppStateInner) -> ProjectFacts {
    ProjectFacts {
        path: inner
            .file_path
            .as_ref()
            .map(|path| path.display().to_string()),
        dsl_digest: sessions::dsl_digest(&inner.dsl_source),
        track_count: inner.project.tracks.len(),
    }
}

/// Sessions newest first: the current project's unless `all` is set.
#[tauri::command]
pub fn sessions_list(
    project: Option<String>,
    all: Option<bool>,
    state: State<AppState>,
) -> Result<Vec<SessionMeta>, String> {
    list_for(&state, project.as_deref(), all.unwrap_or(false))
}

/// List saved sessions for a project, or all projects.
pub fn list_for(
    state: &AppState,
    project: Option<&str>,
    all: bool,
) -> Result<Vec<SessionMeta>, String> {
    let requested = requested_project(project);
    let place = {
        let inner = state.lock_inner();
        crate::commands::workspace::scoped_workspace(&inner, requested.as_deref())
    };
    let store = store();
    if all {
        store.list(Scope::All)
    } else {
        store.list(Scope::Workspace(place.as_deref()))
    }
}

/// Starts a session and makes it the one the next turn saves into. `parent`
/// forks: the new session begins with a copy of that conversation, and
/// `through` copies only the first that many saved messages so the fork can
/// stop at a completed turn.
#[tauri::command]
pub fn sessions_new(
    project: Option<String>,
    title: Option<String>,
    parent: Option<String>,
    through: Option<usize>,
    state: State<AppState>,
) -> Result<SessionMeta, String> {
    new_fork(
        &state,
        project.as_deref(),
        title.as_deref(),
        parent.as_deref(),
        through,
    )
}

/// Create a session for a project, optionally forking a whole parent.
pub fn new_for(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    parent: Option<&str>,
) -> Result<SessionMeta, String> {
    new_fork(state, project, title, parent, None)
}

/// Create a session, copying `through` messages of `parent` when a parent is
/// named. A message count without a parent names no conversation to cut.
pub fn new_fork(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    parent: Option<&str>,
    through: Option<usize>,
) -> Result<SessionMeta, String> {
    let Some(parent) = parent else {
        if through.is_some() {
            return Err("a message count needs a session to fork from".to_owned());
        }
        return create_session(state, project, title, None);
    };
    create_session(state, project, title, Some(Fork { parent, through }))
}

fn create_session(
    state: &AppState,
    project: Option<&str>,
    title: Option<&str>,
    fork: Option<Fork<'_>>,
) -> Result<SessionMeta, String> {
    let (project, place) = {
        let inner = state.lock_inner();
        let requested = requested_project(project);
        let project = requested.clone().or_else(|| inner.file_path.clone());
        let place = crate::commands::workspace::scoped_workspace(&inner, requested.as_deref());
        (project, place)
    };
    let store = store();
    let meta = store.create(place.as_deref(), project.as_deref(), title, fork)?;
    sessions::set_active(&meta.workspace_key, &meta.id);
    Ok(meta)
}

/// The saved wire messages of one session, ready to replay.
#[tauri::command]
pub fn sessions_load(id: String) -> Result<Loaded, String> {
    let loaded = store().load(&id)?;
    sessions::set_active(&loaded.meta.workspace_key, &loaded.meta.id);
    Ok(loaded)
}

/// Drop all history before the `keep`-th compaction, then collect assets no
/// remaining message refers to.
#[tauri::command]
pub fn sessions_drop_history(id: String, keep: u64) -> Result<Value, String> {
    store().drop_history(&id, keep)
}

/// Cut the conversation back to its first `keep` wire messages, at the start of
/// a tool block the cut landed inside, closing any call the cut left without a
/// result. Refused while a turn is still writing this conversation's history.
#[tauri::command]
pub fn sessions_rewind(
    id: String,
    keep: usize,
    requests: State<'_, Requests>,
) -> Result<Value, String> {
    let active = requests.active()?;
    refuse_while_running(active.as_ref(), &id)?;
    let kept = store().rewind(&id, keep)?;
    Ok(json!({ "kept": kept }))
}

/// Why a rewind cannot run: a turn that is still answering writes messages at
/// the end of the very history the rewind rewrites. A request that has not named
/// its conversation yet may be saving into this one, so it counts as running.
fn refuse_while_running(active: Option<&ActiveTurn>, id: &str) -> Result<(), String> {
    let Some(turn) = active else {
        return Ok(());
    };
    match turn.session_id.as_deref() {
        Some(session) if session != id => Ok(()),
        _ => Err("A turn is still running in this conversation; stop it before reverting.".into()),
    }
}

/// Save the project DSL as it stands before a message is sent, so that message
/// can be rolled back to later.
#[tauri::command]
pub fn sessions_snapshot(id: String, dsl: String) -> Result<Value, String> {
    let sha = store().snapshot_dsl(&id, &dsl)?;
    Ok(json!({ "sha": sha, "bytes": dsl.len() }))
}

/// Read one saved DSL version back.
#[tauri::command]
pub fn sessions_version(id: String, sha: String) -> Result<Value, String> {
    let dsl = store().dsl_version(&id, &sha)?;
    Ok(json!({ "dsl": dsl }))
}

/// The history a compaction replaced, for the panel's original view.
#[tauri::command]
pub fn sessions_original(id: String) -> Result<Value, String> {
    let (messages, compactions) = store().original(&id)?;
    Ok(json!({ "messages": messages, "compactions": compactions }))
}

#[tauri::command]
/// Rename one saved session.
pub fn sessions_rename(id: String, title: String) -> Result<SessionMeta, String> {
    store().rename(&id, &title)
}

/// Sets what this session may do to the DSL without asking. The boundary takes
/// the enum, so a value outside the three modes is refused by deserialization.
#[tauri::command]
pub fn sessions_set_permission(
    id: String,
    permission: sessions::SessionPermission,
) -> Result<SessionMeta, String> {
    store().set_permission(&id, permission)
}

#[tauri::command]
/// Delete one saved session.
pub fn sessions_delete(id: String) -> Result<(), String> {
    store().delete(&id)
}

/// Folds the older turns of a session into one plain-text summary.
#[tauri::command]
pub fn sessions_compact(
    id: String,
    target_tokens: Option<u64>,
    state: State<AppState>,
) -> Result<Compaction, String> {
    compact_for(&state, &id, target_tokens)
}

/// Compact a session to a target token budget.
pub fn compact_for(
    state: &AppState,
    id: &str,
    target_tokens: Option<u64>,
) -> Result<Compaction, String> {
    let facts = {
        let inner = state.lock_inner();
        facts(&inner)
    };
    store().compact(id, target_tokens, &facts)
}

/// One model turn's saved-view decision.
pub struct PreparedTurn {
    pub session_id: String,
    pub messages: Value,
    pub compacted: Option<Compaction>,
    /// Archive files this turn's auditions evicted; the panel reports them.
    pub archive_evicted: Vec<crate::ai::sessions::EvictedAudition>,
    pub saved_auditions: Vec<Value>,
}

/// Identity captured by the request owner, not the active panel selection.
pub struct SavedTurnBinding<'a> {
    pub id: &'a str,
    pub incarnation: &'a str,
    pub project: Option<&'a Path>,
}

pub(crate) fn validate_saved_project(expected: &Path, actual: Option<&Path>) -> Result<(), String> {
    let expected = expected.canonicalize().map_err(|error| error.to_string())?;
    let actual = actual
        .ok_or("No project is open")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if expected != actual {
        return Err("The saved turn belongs to another project".into());
    }
    Ok(())
}

/// Resolves the active session for the open workspace (creating one on the first
/// turn), appends what the request adds, and returns what to send.
///
/// Returns `Ok(None)` when the request must go out unchanged: there is no session
/// to save into or the saved conversation cannot be replayed as it stands.
pub fn prepare_turn_for(
    app: &tauri::AppHandle,
    provider: &AiProviderEntry,
    messages: &Value,
    binding: SavedTurnBinding<'_>,
) -> Result<Option<PreparedTurn>, String> {
    let state = app.state::<AppState>();
    let (project, place, facts, source) = {
        let inner = state.lock_inner();
        if let Some(expected) = binding.project {
            validate_saved_project(expected, inner.file_path.as_deref())?;
        }
        let project = project_of(None, &inner);
        let place = crate::commands::workspace::scoped_workspace(&inner, None);
        (project, place, facts(&inner), inner.dsl_source.clone())
    };
    let session_id = binding.id;
    let store = store();
    let key = store.workspace_key(place.as_deref());
    let (_, meta) = store.find(session_id)?;
    if meta.workspace_key != key {
        return Err("The requested conversation belongs to another workspace".into());
    }
    let session_id = session_id.to_owned();
    let incoming = messages
        .as_array()
        .ok_or_else(|| "Invalid AI request messages".to_string())?;
    let overhead = sessions::estimate_tokens(
        &incoming
            .iter()
            .filter(|message| message["role"] == "system")
            .cloned()
            .collect::<Vec<_>>(),
    );
    let context = crate::ai::limits::context_length(provider);
    let prepared = store.prepare_owned(
        &session_id,
        incoming,
        project.as_deref(),
        crate::ai::sessions::PrepareContext {
            snapshot: Some(&source),
            overhead,
            context_length: context,
            facts: &facts,
            provider: &provider.id,
            model: &provider.model,
        },
        Some(binding.incarnation),
    )?;
    let saved = match prepared.live {
        Some(live) => live,
        None => prepared.messages,
    };
    let mut outbound: Vec<Value> = incoming
        .iter()
        .filter(|message| message["role"] == "system")
        .cloned()
        .collect();
    outbound.extend(saved);
    // The wire admits 100 messages. Should a conversation grow past what
    // compaction folded, send its newest messages that start on a user turn, so
    // no tool result is orphaned from its call.
    if outbound.len() > 96 {
        let keep = outbound.len() - 96;
        if let Some(start) = (keep..outbound.len()).find(|index| outbound[*index]["role"] == "user")
        {
            outbound.drain(..start);
            log::warn!(
                "[ai] session {session_id} sent the last {} messages",
                outbound.len()
            );
        }
    }
    if outbound.len() > 96 {
        return Ok(None);
    }
    outbound.insert(0, json!({"role": "system", "content": format!(
        "Authoritative current project state at request time: DSL sha256 {}; track count {}. Historical tool successes may have been rolled back or superseded. Inspect current project when continuing edits; do not assume historical mutations remain applied.", facts.dsl_digest, facts.track_count
    )}));
    if crate::ai::media::audio_supported(provider) {
        sessions::describe_audition_renders(store.root(), &mut outbound);
    } else {
        for entry in outbound
            .iter_mut()
            .filter_map(|message| message["media"].as_array_mut())
            .flatten()
        {
            if entry["kind"] == "audio" {
                entry["local_reference"] = json!(true);
            }
        }
    }
    sessions::bound_media_replay(&mut outbound, incoming);
    Ok(Some(PreparedTurn {
        session_id,
        messages: Value::Array(outbound),
        compacted: prepared.compacted,
        archive_evicted: prepared.evicted_auditions,
        saved_auditions: prepared.saved_auditions,
    }))
}

/// Persists panel-local proposal results without making another provider request.
#[tauri::command]
pub fn sessions_checkpoint(
    id: String,
    messages: Vec<Value>,
    status: Option<String>,
    path: Option<String>,
) -> Result<(), String> {
    if messages.len() > 100
        || serde_json::to_vec(&messages)
            .map_err(|e| e.to_string())?
            .len()
            > 2_000_000
    {
        return Err("Session checkpoint exceeds its bounded tool-result budget".into());
    }
    let outcome = match (status.as_deref(), path.as_deref()) {
        (None, None) => None,
        (Some(status), Some(path)) => Some((status, path)),
        _ => return Err("Proposal outcome requires both status and path".into()),
    };
    store().checkpoint_tools(&id, &messages, outcome)
}

/// Persists the host outcome, even when the provider failed before replying.
pub fn record_panel_rollback(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (place, facts) = {
        let inner = state.lock_inner();
        (
            crate::commands::workspace::scoped_workspace(&inner, None),
            facts(&inner),
        )
    };
    let store = store();
    if let Some(id) = sessions::active(&store.workspace_key(place.as_deref())) {
        store.record_rollback(&id, &facts)?;
    }
    Ok(())
}

/// Saves the assistant message a finished turn produced.
pub fn record_reply_for(
    app: &tauri::AppHandle,
    session_id: &str,
    reply: &Value,
    provider: &AiProviderEntry,
    incarnation: Option<&str>,
) -> Result<SessionMeta, String> {
    let state = app.state::<AppState>();
    let facts = {
        let inner = state.lock_inner();
        facts(&inner)
    };
    store().record_reply_owned(
        session_id,
        reply,
        &facts,
        (&provider.id, &provider.model),
        incarnation,
    )
}

/// The payload of [`AUTO_SESSION_EVENT`]: the panel adopts `messages` for the
/// next turn and can tell the user what was dropped.
pub fn compaction_event(
    turn: &PreparedTurn,
    compaction: &Compaction,
    provider: &AiProviderEntry,
) -> Value {
    json!({
        "session_id": turn.session_id,
        "compacted": true,
        "provider": provider.id,
        "model": provider.model,
        "dropped_from": compaction.dropped_from,
        "dropped_to": compaction.dropped_to,
        "tokens_before": compaction.tokens_before,
        "tokens_after": compaction.tokens_after,
        "retained_pairs": compaction.retained_pairs,
        "messages": turn.messages,
    })
}

/// The payload of [`AUTO_SESSION_EVENT`] when a turn's auditions had to evict
/// older archive files: the panel says what went so the loss is not silent.
pub fn archive_event(turn: &PreparedTurn) -> Value {
    json!({
        "session_id": turn.session_id,
        "evicted": turn.archive_evicted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(session: Option<&str>) -> ActiveTurn {
        ActiveTurn {
            request_id: "r1".into(),
            session_id: session.map(str::to_owned),
        }
    }

    #[test]
    fn a_rewind_is_refused_while_a_turn_writes_the_conversation() {
        assert!(refuse_while_running(None, "s1").is_ok());
        // A turn saving into another conversation does not block this one.
        assert!(refuse_while_running(Some(&running(Some("other"))), "s1").is_ok());
        assert!(refuse_while_running(Some(&running(Some("s1"))), "s1").is_err());
        // A request that has not named its conversation yet may be saving here.
        assert!(refuse_while_running(Some(&running(None)), "s1").is_err());
    }
}
