//! Owner-scoped synth mutations and automation, admitted under the document lock.

use super::*;
use crate::state::{AppStateInner, UndoGestureToken};

pub(super) struct PresetParameterBatch {
    pub(super) name: String,
    pub(super) params: Vec<(String, Option<String>)>,
    pub(super) replace_synth: bool,
    pub(super) effects: Vec<ast::Effect>,
    pub(super) patch_label: Option<String>,
    pub(super) ticket: Option<u64>,
}

fn require_owner(inner: &AppStateInner, token: &UndoGestureToken) -> Result<(), String> {
    if inner.undo.is_owned_gesture_active(token) {
        Ok(())
    } else {
        Err("Undo gesture is no longer active for this document".into())
    }
}

pub(super) fn apply_randomize_preset(
    inner: &mut AppStateInner,
    name: &str,
    seed: u64,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    randomize_preset_in_project(&mut inner.project, name, seed)?;
    inner.audio.invalidate_live_synth(name);

    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("randomize patch", UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

pub(super) fn apply_macro_learn(
    inner: &mut AppStateInner,
    name: &str,
    macro_index: usize,
    control: &str,
) -> Result<MacroLearnResponse, String> {
    let root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let old_project = inner.project.clone();
    let result = macro_learn_inner(
        &mut inner.project,
        name,
        macro_index,
        control,
        root.as_deref(),
    )?;
    if matches!(
        result.status,
        MacroLearnStatus::Bound | MacroLearnStatus::Unbound
    ) {
        inner.audio.invalidate_live_synth(name);
        resync_with_undo(
            inner,
            old_project,
            UndoEdit::new("macro learn", UndoScope::Preset),
        )?;
    }
    Ok(MacroLearnResponse {
        snapshot: snapshot(inner),
        result,
    })
}

#[tauri::command]
/// Randomize a preset under an owned undo gesture.
pub fn randomize_preset_owned(
    name: String,
    seed: u64,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    randomize_preset_owned_inner(&mut inner, &name, seed, &token)
}

fn randomize_preset_owned_inner(
    inner: &mut AppStateInner,
    name: &str,
    seed: u64,
    token: &UndoGestureToken,
) -> Result<ProjectSnapshot, String> {
    require_mutation_owner(inner, token)?;
    apply_randomize_preset(inner, name, seed)
}

#[tauri::command]
/// Assign a control to a synth macro under an owned gesture.
pub fn synth_macro_learn_owned(
    name: String,
    macro_index: usize,
    control: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<MacroLearnResponse, String> {
    let mut inner = state.lock_inner();
    macro_learn_owned_inner(&mut inner, &name, macro_index, &control, &token)
}

fn macro_learn_owned_inner(
    inner: &mut AppStateInner,
    name: &str,
    macro_index: usize,
    control: &str,
    token: &UndoGestureToken,
) -> Result<MacroLearnResponse, String> {
    require_mutation_owner(inner, token)?;
    apply_macro_learn(inner, name, macro_index, control)
}

pub(super) fn apply_single_parameter(
    inner: &mut AppStateInner,
    name: String,
    param_name: String,
    param_value: Option<String>,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    let key = param_name.trim();
    apply_preset_params(
        &mut inner.project,
        &name,
        vec![(key.to_owned(), param_value)],
    )?;
    let preset = inner
        .project
        .presets
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;

    let numeric = preset
        .params
        .iter()
        .find(|(param, _)| param == key)
        .and_then(|(_, value)| {
            value.as_f64().or_else(|| match value {
                ast::ParamValue::String(text) => text.parse().ok(),
                _ => None,
            })
        });
    if let Some(value) = numeric {
        inner.audio.update_live_synth(&name, key, value);
    } else {
        inner.audio.invalidate_live_synth(&name);
    }
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("preset parameter", UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

pub(super) fn apply_parameter_batch(
    state: &AppState,
    request: PresetParameterBatch,
    owner: Option<&UndoGestureToken>,
) -> Result<ProjectSnapshot, String> {
    let PresetParameterBatch {
        name,
        params,
        replace_synth,
        effects,
        patch_label,
        ticket,
    } = request;
    let _timer = oc_diag::perf_counter("cmd.set_preset_params.ms").map(oc_diag::Timer::start);
    oc_diag::diag_info!(
        "cmd: set_preset_params preset={name:?} params={} replace_synth={replace_synth} effects={}",
        params.len(),
        effects.len()
    );
    // A newer click owns the project; leave this request's copy of it alone.
    let mut inner = state.lock_inner();
    if let Some(token) = owner {
        require_mutation_owner(&inner, token)?;
    }
    if ticket.is_some_and(|ticket| state.patch_load.superseded(ticket)) {
        return Ok(snapshot(&inner));
    }
    let old_project = inner.project.clone();
    if replace_synth {
        load_synth_patch_into_project(
            &mut inner.project,
            &name,
            patch_label.as_deref(),
            params,
            effects,
        )?;
        // A whole-patch load is the moment "recently used" changes; auditions
        // never take this path.
        if let Some(label) = patch_label.as_deref() {
            super::super::patch::record_patch_use(label);
        }
    } else {
        apply_preset_params(&mut inner.project, &name, params)?;
    }
    inner.audio.invalidate_live_synth(&name);

    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("preset parameters", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
/// Set one preset parameter under an owned gesture.
pub fn set_preset_param_owned(
    name: String,
    param_name: String,
    param_value: Option<String>,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_preset_param_owned_inner(&mut inner, name, param_name, param_value, &token)
}

fn set_preset_param_owned_inner(
    inner: &mut AppStateInner,
    name: String,
    param_name: String,
    param_value: Option<String>,
    token: &UndoGestureToken,
) -> Result<ProjectSnapshot, String> {
    require_mutation_owner(inner, token)?;
    apply_single_parameter(inner, name, param_name, param_value)
}

#[tauri::command]
/// Set several preset parameters under one owned gesture.
pub async fn set_preset_params_owned(
    name: String,
    params: Vec<(String, Option<String>)>,
    replace_synth: Option<bool>,
    effects: Option<Vec<ast::Effect>>,
    patch_label: Option<String>,
    token: UndoGestureToken,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, String> {
    let replace_synth = replace_synth.unwrap_or(false);
    // A stale request cannot supersede a current patch click while it waits
    // for the blocking pool. Both ticket admission and the eventual mutation
    // verify the same owner while holding the project mutex.
    let ticket = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        require_mutation_owner(&inner, &token)?;
        replace_synth.then(|| state.patch_load.begin())
    };
    let request = PresetParameterBatch {
        name,
        params,
        replace_synth,
        effects: effects.unwrap_or_default(),
        patch_label,
        ticket,
    };
    run_blocking(move || {
        let state = app.state::<AppState>();
        apply_parameter_batch(&state, request, Some(&token))
    })
    .await
}

#[derive(Serialize)]
/// Result of ending an owned synth gesture.
pub struct OwnedSynthGestureEnd {
    pub closed: bool,
    pub snapshot: ProjectSnapshot,
}

fn require_mutation_owner(inner: &AppStateInner, token: &UndoGestureToken) -> Result<(), String> {
    require_owner(inner, token)?;
    inner.refuse_in_preview()?;
    Ok(())
}

#[tauri::command]
/// Begin an owned synth parameter gesture.
pub fn begin_owned_synth_param_gesture(
    preset_name: String,
    track_index: usize,
    param_key: String,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    begin_owned_synth_param_gesture_inner(
        &mut state.lock_inner(),
        &preset_name,
        track_index,
        &param_key,
    )
}

#[tauri::command]
/// Begin a synth gesture guarded by a parameter target stamp.
pub fn begin_guarded_synth_param_gesture(
    preset_name: String,
    track_index: usize,
    param_key: String,
    expected_stamp: crate::state::ParameterTargetStamp,
    state: State<AppState>,
) -> Result<UndoGestureToken, String> {
    let mut inner = state.lock_inner();
    super::super::shared::require_parameter_admission(&inner, &expected_stamp)?;
    begin_owned_synth_param_gesture_inner(&mut inner, &preset_name, track_index, &param_key)
}

fn begin_owned_synth_param_gesture_inner(
    inner: &mut AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_key: &str,
) -> Result<UndoGestureToken, String> {
    inner.refuse_in_preview()?;
    validate_synth_automation_track(inner, preset_name, track_index)?;
    let param_key = param_key.trim();
    if param_key.is_empty() {
        return Err("Synth parameter name cannot be empty".into());
    }
    let key = synth_gesture_key(preset_name, param_key);
    let stale = inner
        .plugin_automation_gestures
        .get(&key)
        .is_some_and(|gesture| !inner.undo.is_owned_gesture_active(&gesture.history_owner));
    if stale {
        inner.plugin_automation_gestures.remove(&key);
    }
    if inner.plugin_automation_gestures.contains_key(&key) {
        return Err(format!(
            "Automation gesture already active for '{param_key}'"
        ));
    }
    let token = inner
        .undo
        .begin_owned_gesture(UndoScope::Automation)
        .map_err(|error| error.to_string())?;
    let gesture = crate::state::PluginAutomationGesture {
        history_owner: token.clone(),
        owner: Some(token.clone()),
        old_project: inner.project.clone(),
        track_index,
        lane_param: plugin_automation_lane_name(param_key),
    };
    inner.plugin_automation_gestures.insert(key, gesture);
    Ok(token)
}

fn synth_recording_session<'a>(
    inner: &'a AppStateInner,
    preset_name: &str,
    param_key: &str,
    owner: Option<&UndoGestureToken>,
) -> Result<&'a crate::state::PluginAutomationGesture, String> {
    inner.refuse_in_preview()?;
    if let Some(token) = owner {
        require_owner(inner, token)?;
    }
    let gesture = inner
        .plugin_automation_gestures
        .get(&synth_gesture_key(preset_name, param_key.trim()))
        .ok_or_else(|| format!("No active automation gesture for '{param_key}'"))?;
    if gesture.owner.as_ref() != owner {
        return Err("Owned automation gesture requires its token".into());
    }
    if !inner.undo.is_owned_gesture_active(&gesture.history_owner) {
        return Err("Automation gesture is no longer active for this document".into());
    }
    Ok(gesture)
}

#[tauri::command]
/// Update the live synth parameter value.
pub fn update_owned_synth_param_gesture(
    preset_name: String,
    param_key: String,
    normalized: f64,
    physical: Option<f64>,
    position: ast::BarBeat,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_owned_synth_param_gesture_inner(
        &mut inner,
        &preset_name,
        &param_key,
        OwnedSynthPoint {
            normalized,
            physical,
            position,
            token: &token,
        },
    )?;
    Ok(snapshot(&inner))
}

struct OwnedSynthPoint<'a> {
    normalized: f64,
    physical: Option<f64>,
    position: ast::BarBeat,
    token: &'a UndoGestureToken,
}

fn update_owned_synth_param_gesture_inner(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_key: &str,
    point: OwnedSynthPoint<'_>,
) -> Result<(), String> {
    require_mutation_owner(inner, point.token)?;
    publish_synth_recording_point(
        inner,
        preset_name,
        param_key,
        SynthRecordingPoint {
            normalized: point.normalized,
            physical: point.physical,
            position: point.position,
            owner: Some(point.token),
        },
    )
}
pub(super) struct SynthRecordingPoint<'a> {
    pub normalized: f64,
    pub physical: Option<f64>,
    pub position: ast::BarBeat,
    pub owner: Option<&'a UndoGestureToken>,
}
pub(super) fn publish_synth_recording_point(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_key: &str,
    point: SynthRecordingPoint<'_>,
) -> Result<(), String> {
    let gesture = synth_recording_session(inner, preset_name, param_key, point.owner)?;
    let history_owner = gesture.history_owner.clone();
    let track_index = gesture.track_index;
    let lane_param = gesture.lane_param.clone();
    let original_track = gesture
        .old_project
        .tracks
        .get(track_index)
        .map(|track| &track.name);
    if inner
        .project
        .tracks
        .get(track_index)
        .map(|track| &track.name)
        != original_track
    {
        return Err("Automation gesture target changed".into());
    }
    validate_synth_automation_track(inner, preset_name, track_index)?;
    if !point.normalized.is_finite() {
        return Err("Automation value is not finite".into());
    }
    if point.physical.is_some_and(|physical| !physical.is_finite()) {
        return Err("Parameter value is not finite".into());
    }
    // The normalization upgrade, physical patch and lane point publish as one
    // candidate; a refused first point cannot leave a migration-only mutation.
    let mut candidate = inner.project.clone();
    oc_dsl::timing::ProjectClock::new(&candidate.properties)?.quarter_notes_at(&point.position)?;
    upgrade_synth_automation_normalization(&mut candidate);
    if let Some(physical) = point.physical {
        apply_preset_params(
            &mut candidate,
            preset_name,
            vec![(param_key.trim().to_owned(), Some(physical.to_string()))],
        )?;
    }
    upsert_plugin_automation_point(
        &mut candidate,
        track_index,
        &lane_param,
        point.position,
        point.normalized,
    )?;
    inner.publish_owned_automation_candidate(
        &history_owner,
        candidate,
        UndoEdit::new("synth parameter automation", UndoScope::Automation)
            .with_scope(UndoScope::Preset),
    )?;
    if let Some(physical) = point.physical {
        inner
            .audio
            .update_live_synth(preset_name, param_key.trim(), physical);
    }

    Ok(())
}

#[tauri::command]
/// End the gesture and close its undo entry.
pub fn end_owned_synth_param_gesture(
    preset_name: String,
    param_key: String,
    token: UndoGestureToken,
    state: State<AppState>,
) -> Result<OwnedSynthGestureEnd, String> {
    end_owned_synth_param_gesture_inner(&mut state.lock_inner(), &preset_name, &param_key, &token)
}

fn end_owned_synth_param_gesture_inner(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_key: &str,
    token: &UndoGestureToken,
) -> Result<OwnedSynthGestureEnd, String> {
    let closed = finish_synth_recording(inner, preset_name, param_key, Some(token))?;
    Ok(OwnedSynthGestureEnd {
        closed,
        snapshot: snapshot(inner),
    })
}
pub(super) fn finish_synth_recording(
    inner: &mut AppStateInner,
    preset_name: &str,
    param_key: &str,
    owner: Option<&UndoGestureToken>,
) -> Result<bool, String> {
    let key = synth_gesture_key(preset_name, param_key.trim());
    let Some(session) = inner.plugin_automation_gestures.get(&key) else {
        return if owner.is_some() {
            Ok(false)
        } else {
            Err(format!("No active automation gesture for '{param_key}'"))
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
    if owner.is_none() && session.old_project != inner.project {
        inner
            .audio
            .invalidate_inherited_live_synth(&session.old_project, &inner.project);
        inner.audio.mark_dirty();
        inner.audio.preload_project(&inner.project);
    }
    inner.undo.end_owned_gesture(&session.history_owner);
    Ok(true)
}

#[cfg(test)]
mod tests;
