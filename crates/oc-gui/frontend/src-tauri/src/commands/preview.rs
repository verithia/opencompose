//! Preview an edit: begin, swap sides, accept, decline.
//!
//! Every entry point is idempotent: a repeated begin showing the same proposal,
//! a swap to the side already on screen, and a second accept or decline of a
//! preview that is already closed all answer with the current state instead of
//! moving the document twice.

use serde::Serialize;
use tauri::State;

use crate::audio::PlaybackState;
use crate::preview::{PreviewSession, PreviewSide, PreviewState};
use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, snapshot};

/// Refusal when the document moved under an open preview.
const MOVED: &str = "The document changed while the edit preview was open. Decline the preview and ask for a new proposal.";

/// Refusal when the proposal was made against another document.
const STALE: &str = "The document changed since this proposal was made, so the preview was refused. Ask the assistant for a new proposal.";

/// A project snapshot with the preview the panel paints beside it.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewSnapshot {
    #[serde(flatten)]
    pub snapshot: ProjectSnapshot,
    pub preview: PreviewState,
}

#[tauri::command]
/// Begin an A/B preview session.
pub fn preview_begin(
    new_dsl: String,
    target_path: String,
    expected_dsl: String,
    state: State<AppState>,
) -> Result<PreviewSnapshot, String> {
    let mut inner = state.lock_inner();
    begin_inner(&mut inner, new_dsl, target_path, expected_dsl)
}

#[tauri::command]
/// Switch the preview to one side.
pub fn preview_show(side: String, state: State<AppState>) -> Result<PreviewSnapshot, String> {
    let side = PreviewSide::parse(&side)?;
    let mut inner = state.lock_inner();
    show_inner(&mut inner, side)
}

#[tauri::command]
/// Accept the previewed side as the project.
pub fn preview_accept(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    accept_inner(&mut inner)
}

#[tauri::command]
/// Cancel the preview.
pub fn preview_cancel(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    cancel_inner(&mut inner)
}

#[tauri::command]
/// Read the current preview state.
pub fn preview_state(state: State<AppState>) -> Option<PreviewState> {
    state_inner(&state.lock_inner())
}

/// Open a preview of one subtree replacement and show the proposal.
pub fn begin_inner(
    inner: &mut AppStateInner,
    new_dsl: String,
    target_path: String,
    expected_dsl: String,
) -> Result<PreviewSnapshot, String> {
    if let Some(open) = inner.preview.as_ref() {
        // The same press twice shows the preview that is already open.
        if open.target_path == target_path
            && open.subtree_dsl == new_dsl
            && inner.dsl_source == open.dsl()
        {
            let preview = open.view();
            return Ok(PreviewSnapshot {
                snapshot: snapshot(inner),
                preview,
            });
        }
        return Err(format!(
            "A preview of {} is already open; accept or decline it first.",
            open.target_path
        ));
    }
    if inner.dsl_source != expected_dsl {
        return Err(STALE.to_string());
    }
    if inner.transaction.is_some() {
        return Err("commit or roll back the active transaction before previewing an edit".into());
    }
    if inner.audio.recorder.recording() || inner.audio.midi_recorder.recording() {
        return Err("Stop recording before previewing an edit.".into());
    }

    let edit = oc_dsl::ai::AiEdit {
        new_dsl: new_dsl.clone(),
        target_path: target_path.clone(),
        explanation: None,
    };
    // The same checks Accept runs, so a document the panel shows is one Accept
    // can commit: it parses, its media exists, and the DSP factory takes it.
    let validation =
        crate::commands::validate_edit_in_project(inner, new_dsl.clone(), target_path.clone());
    if !validation.valid {
        return Err(validation.errors.join("\n"));
    }
    let root = crate::commands::shared::project_root(inner);
    let mut proposal_project = inner.project.clone();
    let applied = oc_dsl::ai::apply_edit_in(&mut proposal_project, &edit, root.as_deref())?;

    let base_project = inner.project.clone();
    let highlight = crate::preview::highlight(&base_project, &proposal_project);
    let session = PreviewSession {
        base_dsl: inner.dsl_source.clone(),
        base_project,
        proposal_project: proposal_project.clone(),
        proposal_dsl: applied.dsl_text.clone(),
        subtree_dsl: new_dsl,
        side: PreviewSide::Proposal,
        target_path,
        highlight,
        base_dirty: inner.dirty,
    };
    install_preview_side(inner, proposal_project, applied.dsl_text);
    inner.preview = Some(session);
    let preview = inner
        .preview
        .as_ref()
        .expect("the session was just installed")
        .view();
    Ok(PreviewSnapshot {
        snapshot: snapshot(inner),
        preview,
    })
}

/// Put one side of the open preview on screen.
pub fn show_inner(inner: &mut AppStateInner, side: PreviewSide) -> Result<PreviewSnapshot, String> {
    let Some(session) = inner.preview.as_ref() else {
        return Err("no edit preview is open".into());
    };
    if session.side == side {
        // The requested side is already on screen: nothing to install.
        let preview = session.view();
        return Ok(PreviewSnapshot {
            snapshot: snapshot(inner),
            preview,
        });
    }
    if inner.dsl_source != session.dsl() {
        return Err(MOVED.to_string());
    }
    let project = session.project_for(side).clone();
    let dsl = session.dsl_for(side).to_string();
    install_preview_side(inner, project, dsl);
    if let Some(session) = inner.preview.as_mut() {
        session.side = side;
    }
    let preview = inner
        .preview
        .as_ref()
        .expect("the session was just swapped")
        .view();
    Ok(PreviewSnapshot {
        snapshot: snapshot(inner),
        preview,
    })
}

/// Commit the proposal as exactly one undo entry.
pub fn accept_inner(inner: &mut AppStateInner) -> Result<ProjectSnapshot, String> {
    let Some(session) = inner.preview.as_ref() else {
        // A second Accept has nothing left to commit.
        return Ok(snapshot(inner));
    };
    if inner.dsl_source != session.dsl() {
        return Err(MOVED.to_string());
    }
    // Accept writes one entry; an open drag would merge it into the drag's entry
    // instead, so it has to finish first.
    if inner.undo.is_in_gesture() {
        return Err("finish the drag before accepting the preview".into());
    }
    let base_project = session.base_project.clone();
    let proposal_project = session.proposal_project.clone();
    let proposal_dsl = session.proposal_dsl.clone();
    let showing_proposal = session.side == PreviewSide::Proposal;
    inner.preview = None;
    if !showing_proposal {
        install_preview_side(inner, proposal_project.clone(), proposal_dsl.clone());
    }
    inner.dsl_source = proposal_dsl;
    if let Err(reason) = inner.undo.push(
        base_project,
        &proposal_project,
        UndoEdit::new("Preview edit", UndoScope::Project),
    ) {
        // The accepted document stands; only its history level is missing.
        log::error!("[preview] accepted edit not recorded in the undo history: {reason}");
    }
    inner.dirty = true;
    // A process killed after Accept must not lose the committed edit.
    crate::autosave::flush_dirty(inner);
    // The accepted document is the project; publish it to the file. An open
    // batch is published by its commit instead.
    // An accepted preview the file did not take must not look published.
    if let Err(reason) = crate::project_persist::flush(inner) {
        log::error!("[preview] accepted edit not written to the project file: {reason}");
    }
    Ok(snapshot(inner))
}

/// Restore the base document exactly as it was and discard the proposal.
pub fn cancel_inner(inner: &mut AppStateInner) -> Result<ProjectSnapshot, String> {
    let Some(session) = inner.preview.take() else {
        // A second Decline has nothing left to restore.
        return Ok(snapshot(inner));
    };
    if inner.dsl_source != session.base_dsl {
        install_preview_side(inner, session.base_project, session.base_dsl);
    }
    inner.dirty = session.base_dirty;
    Ok(snapshot(inner))
}

fn install_preview_side(inner: &mut AppStateInner, project: oc_dsl::ast::Project, dsl: String) {
    inner.audio.mark_dirty();
    super::project::install_document(inner, project, dsl);
    // Some hosted sources defer loading until Play can capture plug-in state.
    // Park the cursor so the previous side cannot keep sounding under this one.
    if inner.audio.render_dirty && inner.audio.playback_state == PlaybackState::Playing {
        inner.audio.pause();
    }
}

/// The open preview, or `None` when the document is not being previewed.
pub fn state_inner(inner: &AppStateInner) -> Option<PreviewState> {
    inner.preview.as_ref().map(PreviewSession::view)
}
