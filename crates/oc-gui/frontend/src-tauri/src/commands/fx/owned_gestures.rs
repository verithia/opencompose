//! Document-owned effect mutations and automation sessions.

use super::*;
use crate::state::{AppStateInner, EffectWriteGesture, UndoGestureToken};

fn require_owner(inner: &AppStateInner, token: &UndoGestureToken) -> Result<(), String> {
    if !inner.undo.is_owned_gesture_active(token) {
        return Err("Undo gesture is no longer active for this document".into());
    }
    inner.refuse_in_preview()
}

#[derive(serde::Serialize)]
/// Result of ending an owned effect gesture.
pub struct OwnedEffectGestureEnd {
    pub closed: bool,
    pub snapshot: ProjectSnapshot,
}

#[tauri::command]
/// Set effect bypass under an owned gesture.
pub fn set_effect_bypass_owned(
    target: String,
    effect_index: usize,
    bypass: bool,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    require_owner(&inner, &token)?;
    set_effect_bypass_inner(&mut inner, &target, effect_index, bypass)?;
    Ok(snapshot(&inner))
}

#[tauri::command]
/// Set one effect parameter under an owned gesture.
pub fn set_effect_param_owned(
    target: String,
    effect_index: usize,
    param_name: String,
    param_value: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_params_owned_inner(
        &mut inner,
        &target,
        effect_index,
        &[(param_name, param_value)],
        &token,
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
/// Set several effect parameters under an owned gesture.
pub fn set_effect_params_owned(
    target: String,
    effect_index: usize,
    params: Vec<(String, String)>,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_params_owned_inner(&mut inner, &target, effect_index, &params, &token)?;
    Ok(snapshot(&inner))
}

fn set_effect_params_owned_inner(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    params: &[(String, String)],
    token: &UndoGestureToken,
) -> Result<(), String> {
    require_owner(inner, token)?;
    set_effect_params_inner(inner, target, effect_index, params)
}

#[tauri::command]
/// Begin an owned effect parameter write.
pub fn begin_owned_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    begin_owned_effect_param_write_inner(&mut state.lock_inner(), &target, effect_index, &param)
}

#[tauri::command]
/// Begin an effect write guarded by a parameter stamp.
pub fn begin_guarded_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    expected_stamp: crate::state::ParameterTargetStamp,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    let mut inner = state.lock_inner();
    super::super::shared::require_parameter_admission(&inner, &expected_stamp)?;
    begin_owned_effect_param_write_inner(&mut inner, &target, effect_index, &param)
}

fn begin_owned_effect_param_write_inner(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
) -> Result<UndoGestureToken, String> {
    inner.refuse_in_preview()?;
    let parameter = parameter.trim();
    if parameter.is_empty() {
        return Err("Effect parameter name cannot be empty".into());
    }
    validate_effect_write(&inner.project, target, effect_index, parameter)?;
    let key = effect_write_key(target, effect_index, parameter);
    if let Some(session) = inner.effect_write_gestures.get(&key)
        && inner.undo.is_owned_gesture_active(&session.history_owner)
    {
        return Err(format!(
            "Automation gesture already active for '{parameter}'"
        ));
    }
    let token = inner
        .undo
        .begin_owned_gesture(UndoScope::Automation)
        .map_err(|error| error.to_string())?;
    inner.effect_write_gestures.insert(
        key,
        EffectWriteGesture {
            history_owner: token.clone(),
            owner: Some(token.clone()),
            old_project: inner.project.clone(),
        },
    );
    Ok(token)
}

#[tauri::command]
/// Update the live effect parameter value.
pub fn update_owned_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    position: ast::BarBeat,
    value: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_owned_effect_param_write_inner(
        &mut inner,
        &target,
        effect_index,
        &param,
        position,
        &value,
        &token,
    )?;
    Ok(snapshot(&inner))
}

fn update_owned_effect_param_write_inner(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
    position: ast::BarBeat,
    value: &str,
    token: &UndoGestureToken,
) -> Result<(), String> {
    publish_effect_write_point(
        inner,
        target,
        effect_index,
        parameter,
        position,
        value,
        Some(token),
    )
}
pub(super) fn publish_effect_write_point(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
    position: ast::BarBeat,
    value: &str,
    owner: Option<&UndoGestureToken>,
) -> Result<(), String> {
    if let Some(token) = owner {
        require_owner(inner, token)?;
    }
    inner.refuse_in_preview()?;
    let key = effect_write_key(target, effect_index, parameter);
    let session = inner
        .effect_write_gestures
        .get(&key)
        .ok_or_else(|| format!("No active automation gesture for '{parameter}'"))?;
    if session.owner.as_ref() != owner {
        return Err("Owned automation gesture requires its token".into());
    }
    let history_owner = session.history_owner.clone();
    if !inner.undo.is_owned_gesture_active(&history_owner) {
        return Err("Automation gesture is no longer active for this document".into());
    }
    let mut candidate = inner.project.clone();
    prepare_effect_write_point(
        &mut candidate,
        target,
        effect_index,
        parameter,
        position,
        value,
    )?;
    inner.publish_owned_automation_candidate(
        &history_owner,
        candidate,
        UndoEdit::new("write automation", UndoScope::Automation),
    )?;
    Ok(())
}

#[tauri::command]
/// End the effect write and close its undo entry.
pub fn end_owned_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<OwnedEffectGestureEnd, String> {
    end_owned_effect_param_write_inner(
        &mut state.lock_inner(),
        &target,
        effect_index,
        &param,
        &token,
    )
}

fn end_owned_effect_param_write_inner(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
    token: &UndoGestureToken,
) -> Result<OwnedEffectGestureEnd, String> {
    let closed = finish_effect_write(inner, target, effect_index, parameter, Some(token))?;
    Ok(OwnedEffectGestureEnd {
        closed,
        snapshot: snapshot(inner),
    })
}
pub(super) fn finish_effect_write(
    inner: &mut AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
    owner: Option<&UndoGestureToken>,
) -> Result<bool, String> {
    let key = effect_write_key(target, effect_index, parameter);
    let Some(session) = inner.effect_write_gestures.get(&key) else {
        return if owner.is_some() {
            Ok(false)
        } else {
            Err(format!("No active automation gesture for '{parameter}'"))
        };
    };
    if session.owner.as_ref() != owner {
        return if owner.is_some() {
            Ok(false)
        } else {
            Err("Owned automation gesture requires its token".into())
        };
    }
    if !inner.undo.is_owned_gesture_active(&session.history_owner) {
        return Ok(false);
    }
    let session = inner
        .effect_write_gestures
        .remove(&key)
        .expect("the exact session was checked under this lock");
    if session.old_project != inner.project {
        sync_completed_effect_write_audio(inner, &session.old_project, target);
    }
    inner.undo.end_owned_gesture(&session.history_owner);
    Ok(true)
}

// Closing a valid owner preserves the existing single end-of-write audio sync,
// without replaying the begin snapshot through undo history.
fn sync_completed_effect_write_audio(inner: &mut AppStateInner, old: &ast::Project, target: &str) {
    inner
        .audio
        .invalidate_inherited_live_synth(old, &inner.project);
    if target.starts_with("preset:") {
        inner.audio.mark_dirty();
        inner.audio.preload_project(&inner.project);
    } else {
        inner.audio.apply_chain_change(&inner.project);
    }
}

#[cfg(test)]
mod tests;
