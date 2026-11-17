//! Cuts a saved conversation back to a person's message, optionally moving the
//! project's history cursor with it.

use crate::ai::agent_loop::host::{PROJECT_EVENT, scopeguard};
use crate::ai::sessions::Store;
use crate::ai_service::Requests;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

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
    crate::ai::agent_loop::results::with_results(|results| results.forget(session_id));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sessions;
    use crate::ai::sessions::HistoryAt;
    use crate::ai::sessions::turns::SnapshotAt;
    use serde_json::json;

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
            assert_eq!(
                inner.undo.redo_entries().len(),
                1,
                "the edit is one redo away"
            );
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
                inner.undo.redo_entries().len(),
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
        assert_eq!(
            inner.undo.redo_entries().len(),
            redo,
            "and no redo entry appears"
        );
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
                inner.undo.redo_entries().len(),
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
        assert_eq!(inner.undo.redo_entries().len(), redo, "redo is kept");
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
            .claim_for("turn-1", None)
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
}
