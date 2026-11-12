//! Exact close ownership and locked admission for hosted automation.

use super::*;
use crate::state::{AppStateInner, PluginAutomationGesture, UndoGestureToken};

#[derive(Serialize)]
/// Result of ending an owned plugin parameter gesture.
pub struct OwnedPluginGestureEnd {
    pub closed: bool,
    pub snapshot: ProjectSnapshot,
}

fn require_owner(inner: &AppStateInner, token: &UndoGestureToken) -> Result<(), String> {
    if !inner.undo.is_owned_gesture_active(token) {
        return Err("Undo gesture is no longer active for this document".into());
    }
    inner.refuse_in_preview()
}

/// Begin an owned plugin parameter gesture.
#[tauri::command]
pub fn begin_owned_plugin_param_gesture(
    preset_name: String,
    track_index: usize,
    param_name: String,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    let mut inner = state.lock_inner();
    begin_owned_plugin_param_gesture_inner(&mut inner, &preset_name, track_index, &param_name)
}

/// Begin a gesture guarded by an expected parameter stamp.
#[tauri::command]
pub fn begin_guarded_plugin_param_gesture(
    preset_name: String,
    track_index: usize,
    param_name: String,
    expected_stamp: crate::state::ParameterTargetStamp,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    let mut inner = state.lock_inner();
    super::super::shared::require_parameter_admission(&inner, &expected_stamp)?;
    begin_owned_plugin_param_gesture_inner(&mut inner, &preset_name, track_index, &param_name)
}

fn begin_owned_plugin_param_gesture_inner(
    inner: &mut AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_name: &str,
) -> Result<UndoGestureToken, String> {
    inner.refuse_in_preview()?;
    let track = inner
        .project
        .tracks
        .get(track_index)
        .ok_or_else(|| format!("Track index {track_index} out of range"))?;
    if track.preset.as_deref() != Some(preset_name) {
        return Err("Track does not reference this plugin preset".into());
    }
    if !has_plugin_instance(inner, preset_name) {
        return Err(format!("Plugin for '{preset_name}' not loaded"));
    }
    begin_owned_plugin_session(inner, preset_name, track_index, param_name)
}

fn begin_owned_plugin_session(
    inner: &mut AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_name: &str,
) -> Result<UndoGestureToken, String> {
    begin_recording_session(inner, preset_name, track_index, param_name, true)
}
pub(super) fn begin_legacy_plugin_session(
    inner: &mut AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_name: &str,
) -> Result<(), String> {
    begin_recording_session(inner, preset_name, track_index, param_name, false).map(|_| ())
}
fn begin_recording_session(
    inner: &mut AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_name: &str,
    expose_owner: bool,
) -> Result<UndoGestureToken, String> {
    inner.refuse_in_preview()?;
    let param_name = param_name.trim();
    if param_name.is_empty() {
        return Err("Plugin parameter name cannot be empty".into());
    }
    let key = plugin_gesture_key(preset_name, param_name);
    if let Some(session) = inner.plugin_automation_gestures.get(&key)
        && inner.undo.is_owned_gesture_active(&session.history_owner)
    {
        return Err(format!(
            "Automation gesture already active for '{param_name}'"
        ));
    }
    let token = inner
        .undo
        .begin_owned_gesture(UndoScope::Automation)
        .map_err(|error| error.to_string())?;
    inner.plugin_automation_gestures.insert(
        key,
        PluginAutomationGesture {
            history_owner: token.clone(),
            owner: expose_owner.then(|| token.clone()),
            old_project: inner.project.clone(),
            track_index,
            lane_param: plugin_automation_lane_name(param_name),
        },
    );
    Ok(token)
}

/// Update an owned plugin parameter gesture.
#[tauri::command]
pub fn update_owned_plugin_param_gesture(
    preset_name: String,
    param_name: String,
    value: f64,
    position: ast::BarBeat,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    apply_plugin_point(
        &mut state.lock_inner(),
        &preset_name,
        &param_name,
        value,
        position,
        Some(&token),
        set_plugin_parameter,
    )
}

pub(super) fn apply_plugin_point<F>(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_name: &str,
    value: f64,
    position: ast::BarBeat,
    owner: Option<&UndoGestureToken>,
    set_parameter: F,
) -> Result<ProjectSnapshot, String>
where
    F: FnOnce(&mut AppStateInner, &str, &str, f64) -> Result<f64, String>,
{
    if let Some(token) = owner {
        require_owner(inner, token)?;
    }
    inner.refuse_in_preview()?;
    if !value.is_finite() {
        return Err("Automation value/position is invalid".into());
    }
    oc_dsl::timing::ProjectClock::new(&inner.project.properties)?.quarter_notes_at(&position)?;
    let key = plugin_gesture_key(preset_name, param_name);
    let session = inner
        .plugin_automation_gestures
        .get(&key)
        .ok_or_else(|| format!("No active automation gesture for '{param_name}'"))?;
    if session.owner.as_ref() != owner {
        return Err("Owned automation gesture requires its token".into());
    }
    let history_owner = session.history_owner.clone();
    if !inner.undo.is_owned_gesture_active(&history_owner) {
        return Err("Automation gesture is no longer active for this document".into());
    }
    let track_index = session.track_index;
    let lane_param = session.lane_param.clone();
    if owner.is_some()
        && inner
            .project
            .tracks
            .get(track_index)
            .is_none_or(|track| track.preset.as_deref() != Some(preset_name))
    {
        return Err("Track does not reference this plugin preset".into());
    }
    // Build the lane before changing the native instance; malformed targets or
    // clocks cannot leave a live-only parameter write behind.
    let mut candidate = inner.project.clone();
    upsert_plugin_automation_point(&mut candidate, track_index, &lane_param, position, 0.0)?;
    let prepared = prepare_owned_plugin_control(inner, preset_name, param_name, value)?;
    let normalized = prepared.normalized();
    set_prepared_point(
        &mut candidate,
        track_index,
        &lane_param,
        position,
        normalized,
    );
    inner.publish_owned_automation_with(
        &history_owner,
        candidate,
        UndoEdit::new("plugin parameter automation", UndoScope::Automation)
            .with_scope(UndoScope::PluginState),
        move |inner| prepared.apply(inner, preset_name, param_name, value, set_parameter),
    )?;
    Ok(snapshot(inner))
}

/// End an owned plugin parameter gesture.
#[tauri::command]
pub fn end_owned_plugin_param_gesture(
    preset_name: String,
    param_name: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<OwnedPluginGestureEnd, String> {
    end_owned_plugin_session(&mut state.lock_inner(), &preset_name, &param_name, &token)
}

fn end_owned_plugin_session(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_name: &str,
    token: &UndoGestureToken,
) -> Result<OwnedPluginGestureEnd, String> {
    let closed = finish_plugin_recording(inner, preset_name, param_name, Some(token))?;
    Ok(OwnedPluginGestureEnd {
        closed,
        snapshot: snapshot(inner),
    })
}
pub(super) fn finish_plugin_recording(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_name: &str,
    owner: Option<&UndoGestureToken>,
) -> Result<bool, String> {
    let key = plugin_gesture_key(preset_name, param_name);
    let Some(session) = inner.plugin_automation_gestures.get(&key) else {
        return if owner.is_some() {
            Ok(false)
        } else {
            Err(format!("No active automation gesture for '{param_name}'"))
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
        .plugin_automation_gestures
        .remove(&key)
        .expect("the exact session was checked under this lock");
    if inner.project != session.old_project {
        inner
            .audio
            .invalidate_inherited_live_synth(&session.old_project, &inner.project);
        inner.audio.mark_dirty();
        inner.audio.preload_project(&inner.project);
    }
    inner.undo.end_owned_gesture(&session.history_owner);
    Ok(true)
}

enum PreparedOwnedPluginControl {
    Clap(oc_host::clap_instance::PreparedClapControlParameter),
    Normalized(f64),
}
impl PreparedOwnedPluginControl {
    fn normalized(&self) -> f64 {
        match self {
            Self::Clap(plan) => plan.normalized_value(),
            Self::Normalized(value) => *value,
        }
    }
    fn apply<F>(
        self,
        inner: &mut AppStateInner,
        preset: &str,
        parameter: &str,
        value: f64,
        set: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut AppStateInner, &str, &str, f64) -> Result<f64, String>,
    {
        match self {
            Self::Clap(plan) => inner
                .plugin_instances
                .get(preset)
                .ok_or("Plugin parameter owner was replaced")?
                .queue_prepared_control_parameter(plan),
            Self::Normalized(expected) => {
                let actual = set(inner, preset, parameter, value)?;
                if !actual.is_finite() || actual.to_bits() != expected.to_bits() {
                    return Err("Plugin parameter normalization changed before publication".into());
                }
                Ok(())
            }
        }
    }
}
fn prepare_owned_plugin_control(
    inner: &AppStateInner,
    preset: &str,
    parameter: &str,
    value: f64,
) -> Result<PreparedOwnedPluginControl, String> {
    if let Some(instance) = inner.plugin_instances.get(preset) {
        return instance
            .prepare_control_parameter_value(parameter, value)
            .map(PreparedOwnedPluginControl::Clap);
    }
    // The other existing setter adapters receive normalized values and return
    // exactly this clamp; no native discovery/conversion precedes checkpointing.
    Ok(PreparedOwnedPluginControl::Normalized(
        value.clamp(0.0, 1.0),
    ))
}
fn set_prepared_point(
    candidate: &mut ast::Project,
    track_index: usize,
    lane_param: &str,
    position: ast::BarBeat,
    normalized: f64,
) {
    let lane_index = candidate.tracks[track_index]
        .automation()
        .position(|lane| lane.param == lane_param)
        .expect("the prepared lane exists");
    let lane = candidate.tracks[track_index]
        .automation_lane_mut(lane_index)
        .expect("the prepared lane exists");
    let point = lane
        .points
        .iter_mut()
        .find(|point| point.position == position)
        .expect("the prepared point exists");
    point.value = ast::ParamValue::Number(normalized);
}

#[cfg(test)]
mod tests;
