//! Atomic edits to one post-insert gain envelope, without changing its stage.
use oc_dsl::ast;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::shared::{
    ProjectSnapshot, Target, parse_target, require_parameter_admission, resync_chain_with_undo,
    snapshot, validation_text,
};
use crate::state::{AppState, AppStateInner, ParameterTargetStamp, UndoEdit, UndoScope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GainAutomationEdit {
    pub target: String,
    pub expected_stamp: ParameterTargetStamp,
    pub expected_revision: u64,
    pub index: usize,
    pub expected: ast::GainAutomation,
    pub replacement: ast::GainAutomation,
}

#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum GainAutomationEditError {
    Stale,
    Invalid { message: String },
    Refused { message: String },
}

fn envelopes_mut<'a>(
    project: &'a mut ast::Project,
    target: &str,
) -> Result<&'a mut Vec<ast::GainAutomation>, String> {
    match parse_target(target)? {
        Target::Track(index) => project
            .tracks
            .get_mut(index)
            .map(|track| &mut track.gain_automation)
            .ok_or_else(|| format!("Track {index} out of range")),
        Target::Bus(name) => project
            .buses
            .iter_mut()
            .find(|bus| bus.name == name)
            .map(|bus| &mut bus.gain_automation)
            .ok_or_else(|| format!("Bus '{name}' not found")),
        Target::Master => project
            .master
            .as_mut()
            .map(|master| &mut master.gain_automation)
            .ok_or_else(|| "The project has no master section".to_string()),
    }
}

pub(crate) fn replace_gain_automation_inner(
    inner: &mut AppStateInner,
    edit: GainAutomationEdit,
) -> Result<(), GainAutomationEditError> {
    require_parameter_admission(inner, &edit.expected_stamp)
        .map_err(|message| GainAutomationEditError::Refused { message })?;
    if inner.revision != edit.expected_revision {
        return Err(GainAutomationEditError::Stale);
    }
    let mut candidate = inner.project.clone();
    let envelope = envelopes_mut(&mut candidate, &edit.target)
        .map_err(|message| GainAutomationEditError::Invalid { message })?
        .get_mut(edit.index)
        .ok_or(GainAutomationEditError::Stale)?;
    if *envelope != edit.expected {
        return Err(GainAutomationEditError::Stale);
    }
    *envelope = edit.replacement;
    // The parser-backed agent path and this typed GUI adapter share validation.
    oc_dsl::validate(&candidate).map_err(|errors| GainAutomationEditError::Invalid {
        message: validation_text(&errors),
    })?;
    let previous = std::mem::replace(&mut inner.project, candidate);
    resync_chain_with_undo(
        inner,
        previous,
        UndoEdit::new("gain envelope", UndoScope::Automation),
    )
    .map_err(|message| GainAutomationEditError::Refused { message })
}

#[tauri::command]
pub fn replace_gain_automation(
    request: GainAutomationEdit,
    state: State<AppState>,
) -> Result<ProjectSnapshot, GainAutomationEditError> {
    let mut inner = state.lock_inner();
    replace_gain_automation_inner(&mut inner, request)?;
    Ok(snapshot(&inner))
}
