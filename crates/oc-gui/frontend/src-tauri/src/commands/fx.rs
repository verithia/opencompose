//! Built-in effect parameters, sidechain routing, meters, and spectrum state.

use tauri::State;

mod owned_gestures;
pub use owned_gestures::*;

use oc_dsl::ast;

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::plugin_catalog::resolve_plugin_requirement;
use super::shared::{
    ProjectSnapshot, Target, parse_effect_type, parse_param_value, parse_target,
    resync_chain_with_undo, resync_with_undo, snapshot, validation_text,
};

// ============================================================================
// Effects management
// ============================================================================

/// An empty master section, created when a chain edit resolves the master
/// scope on a project that has none.
fn empty_master() -> ast::Master {
    ast::Master {
        gain_automation: Vec::new(),
        automation: Vec::new(),
        effects: Vec::new(),
        loudness_target: None,
        volume_db: 0.0,
        input_routes: Vec::new(),
    }
}

/// The effect chain a panel target names. The rack and the device panel open on
/// a track, bus, master, or preset chain; any other scope is refused in words.
fn effect_chain_mut<'a>(
    project: &'a mut ast::Project,
    target: &str,
) -> Result<&'a mut Vec<ast::Effect>, String> {
    if let Some(name) = target.strip_prefix("preset:") {
        return Ok(&mut project
            .presets
            .iter_mut()
            .find(|preset| preset.name == name)
            .ok_or_else(|| format!("Preset '{name}' not found"))?
            .effects);
    }
    match parse_target(target) {
        Ok(Target::Track(index)) => Ok(&mut project
            .tracks
            .get_mut(index)
            .ok_or_else(|| format!("Track {index} out of range"))?
            .effects),
        Ok(Target::Bus(name)) => Ok(&mut project
            .buses
            .iter_mut()
            .find(|bus| bus.name == name)
            .ok_or_else(|| format!("Bus '{name}' not found"))?
            .effects),
        Ok(Target::Master) => Ok(&mut project.master.get_or_insert_with(empty_master).effects),
        Err(_) => Err(format!(
            "Effects can be reordered only in a track, bus, master, or preset chain; \
             '{target}' is not one of them"
        )),
    }
}

/// Publish an effect-chain edit.
///
/// A track, bus or master chain lives in the live graph, so the edit patches it
/// and never rebuilds an instrument. A preset chain does not: a preset's effects
/// run on every pattern lane that uses it, so those edits keep the full rebuild.
fn resync_effect_edit(
    inner: &mut crate::state::AppStateInner,
    old_project: ast::Project,
    edit: UndoEdit,
    target: &str,
) -> Result<(), String> {
    if target.starts_with("preset:") {
        resync_with_undo(inner, old_project, edit)
    } else {
        resync_chain_with_undo(inner, old_project, edit)
    }
}

/// Refuse a plugin effect the host cannot resolve to an installed effect.
///
/// The render path loads the class through the same resolver, so an insert that
/// no longer resolves would leave a silent slot; the refusal names it here.
fn ensure_effect_plugin(plugin_id: &str) -> Result<(), String> {
    let Some(plugin) = resolve_plugin_requirement(plugin_id, oc_host::cached_plugins()) else {
        return Err(format!("Plugin '{plugin_id}' is not installed"));
    };
    if plugin.category == oc_host::PluginCategory::Instrument {
        return Err(format!(
            "Plugin '{}' is an instrument; add it as an instrument instead of an effect",
            plugin.name
        ));
    }
    Ok(())
}

pub(crate) fn add_effect_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_type: &str,
) -> Result<(), String> {
    let effect = ast::Effect {
        effect_type: parse_effect_type(effect_type)?,
        params: vec![],
        bypass: false,
        sidechain_source: None,
    };
    if let ast::EffectType::Plugin(plugin_id) = &effect.effect_type {
        ensure_effect_plugin(plugin_id)?;
    }
    inner.refuse_in_preview()?;
    let old_project = inner.project.clone();
    effect_chain_mut(&mut inner.project, target)?.push(effect);
    inner.invalidate_parameter_owners();
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("add effect", UndoScope::MixerRouting),
        target,
    )?;
    Ok(())
}

/// Add an effect to a track, bus or master chain.
#[tauri::command]
pub fn add_effect(
    target: String,
    effect_type: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    add_effect_inner(&mut inner, &target, &effect_type)?;
    Ok(snapshot(&inner))
}

pub(crate) fn remove_effect_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    let mut candidate = inner.project.clone();
    let effects = effect_chain_mut(&mut candidate, target)?;
    if effect_index >= effects.len() {
        return Err(format!("Effect index {effect_index} out of range"));
    }
    effects.remove(effect_index);
    let old_project = std::mem::replace(&mut inner.project, candidate);
    inner.invalidate_parameter_owners();
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("remove effect", UndoScope::MixerRouting),
        target,
    )?;
    Ok(())
}

/// Remove an effect from a chain.
#[tauri::command]
pub fn remove_effect(
    target: String,
    effect_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_effect_inner(&mut inner, &target, effect_index)?;
    Ok(snapshot(&inner))
}

/// Move one effect to another position in its chain.
pub(crate) fn move_effect_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    new_index: usize,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    let mut candidate = inner.project.clone();
    let effect_count = effect_chain_mut(&mut candidate, target)?.len();
    if effect_index >= effect_count {
        return Err(format!(
            "Effect {effect_index} is out of range for {target}: the chain has {effect_count} effect(s)"
        ));
    }
    if new_index >= effect_count {
        return Err(format!(
            "Cannot move effect {effect_index} to position {new_index}: \
             {target} has {effect_count} effect(s)"
        ));
    }
    if effect_index == new_index {
        return Ok(());
    }

    let effects = effect_chain_mut(&mut candidate, target)?;
    let effect = effects.remove(effect_index);
    effects.insert(new_index, effect);
    let old_project = std::mem::replace(&mut inner.project, candidate);
    inner.invalidate_parameter_owners();
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("move effect", UndoScope::MixerRouting),
        target,
    )?;
    Ok(())
}

/// Reorder an effect within its chain.
#[tauri::command]
pub fn move_effect(
    target: String,
    effect_index: usize,
    new_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_effect_inner(&mut inner, &target, effect_index, new_index)?;
    Ok(snapshot(&inner))
}

/// Set the effect's own bypass field, the one control that works on every
/// device. A bypassed effect leaves the chain; it is not a device parameter.
pub(crate) fn set_effect_bypass_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    bypass: bool,
) -> Result<ast::EffectType, String> {
    inner.refuse_in_preview()?;
    let mut next = inner.project.clone();
    let effect = effect_chain_mut(&mut next, target)?
        .get_mut(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))?;
    let effect_type = effect.effect_type.clone();
    if effect.bypass == bypass {
        return Ok(effect_type);
    }
    effect.bypass = bypass;
    let old_project = std::mem::replace(&mut inner.project, next);
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("bypass effect", UndoScope::Preset),
        target,
    )?;
    Ok(effect_type)
}

/// Bypass or enable one effect.
#[tauri::command]
pub fn set_effect_bypass(
    target: String,
    effect_index: usize,
    bypass: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_bypass_inner(&mut inner, &target, effect_index, bypass)?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_effect_param_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    param_value: &str,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    let mut candidate = inner.project.clone();

    let effect = effect_chain_mut(&mut candidate, target)?
        .get_mut(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))?;

    // `bypass` has one meaning: the effect field, never a device parameter.
    if param_name == "bypass" {
        return Err(
            "'bypass' is an effect field, not a device parameter; toggle it with set_effect_bypass"
                .into(),
        );
    }

    let val = parse_param_value(param_value)?;
    validate_builtin_effect_param(effect, param_name, &val)?;

    // Find existing param or add new
    if let Some(idx) = effect.params.iter().position(|(k, _)| k == param_name) {
        effect.params[idx] = (param_name.to_string(), val);
    } else {
        effect.params.push((param_name.to_string(), val));
    }

    let old_project = std::mem::replace(&mut inner.project, candidate);
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("effect parameter", UndoScope::Preset),
        target,
    )?;
    Ok(())
}

/// Set one effect parameter.
#[tauri::command]
pub fn set_effect_param(
    target: String,
    effect_index: usize,
    param_name: String,
    param_value: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_param_inner(&mut inner, &target, effect_index, &param_name, &param_value)?;
    Ok(snapshot(&inner))
}

/// Apply several parameters from one gesture so the edit lands as one undo entry.
pub(crate) fn set_effect_params_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    params: &[(String, String)],
) -> Result<(), String> {
    if params.is_empty() {
        return Ok(());
    }
    inner.refuse_in_preview()?;
    let mut candidate = inner.project.clone();

    let effect = effect_chain_mut(&mut candidate, target)?
        .get_mut(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))?;

    // `bypass` has one meaning: the effect field, never a device parameter.
    if params.iter().any(|(name, _)| name == "bypass") {
        return Err(
            "'bypass' is an effect field, not a device parameter; toggle it with set_effect_bypass"
                .into(),
        );
    }

    // Validate the whole batch before touching the project so a failure is atomic.
    let mut updates = Vec::with_capacity(params.len());
    for (name, raw) in params {
        let value = parse_param_value(raw)?;
        validate_builtin_effect_param(effect, name, &value)?;
        updates.push((name.clone(), value));
    }
    for (name, value) in updates {
        if let Some(index) = effect.params.iter().position(|(key, _)| key == &name) {
            effect.params[index] = (name, value);
        } else {
            effect.params.push((name, value));
        }
    }

    let old_project = std::mem::replace(&mut inner.project, candidate);
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("effect parameters", UndoScope::Preset),
        target,
    )?;
    Ok(())
}

/// Set several effect parameters in one edit.
#[tauri::command]
pub fn set_effect_params(
    target: String,
    effect_index: usize,
    params: Vec<(String, String)>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_params_inner(&mut inner, &target, effect_index, &params)?;
    Ok(snapshot(&inner))
}

pub(crate) fn validate_builtin_effect_param(
    effect: &ast::Effect,
    name: &str,
    value: &ast::ParamValue,
) -> Result<(), String> {
    match oc_engine::dsp::device_for_effect(&effect.effect_type) {
        Some(device) => oc_engine::dsp::validate_static_param(device, name, value).map(|_| ()),
        None => Ok(()),
    }
}

/// Return the device catalog as JSON.
#[tauri::command]
pub fn devices_catalog() -> serde_json::Value {
    oc_engine::dsp::schema::catalog_json()
}

/// Current gain-reduction and level meters for every graph node.
#[tauri::command]
pub fn meter_snapshot(state: State<AppState>) -> crate::meters::MeterSnapshot {
    meter_snapshot_for_state(&state)
}

pub(crate) fn meter_snapshot_for_state(state: &AppState) -> crate::meters::MeterSnapshot {
    state.lock_inner().audio.meter_snapshot()
}

/// Fold the monitored master to mono, or return it to stereo. Playback only:
/// a render or an export is byte-identical either way.
#[tauri::command]
pub fn set_monitor_mono(
    enabled: bool,
    state: State<AppState>,
) -> Result<crate::meters::MonoCheckSnapshot, String> {
    state.lock_inner().audio.set_monitor_mono(enabled)
}

/// Clear the latched clip indicator on every node.
#[tauri::command]
pub fn reset_meter_clip(state: State<AppState>) {
    state.lock_inner().audio.reset_meter_clip();
}

/// The effect a sidechain edit resolves. A preset chain takes its host at
/// render time, so it names no owner here.
fn sidechain_effect_mut<'a>(
    project: &'a mut ast::Project,
    target: &str,
    effect_index: usize,
) -> Result<&'a mut ast::Effect, String> {
    let effects = match parse_target(target)? {
        Target::Track(index) => {
            &mut project
                .tracks
                .get_mut(index)
                .ok_or_else(|| format!("Track {index} out of range"))?
                .effects
        }
        Target::Bus(name) => {
            &mut project
                .buses
                .iter_mut()
                .find(|bus| bus.name == name)
                .ok_or_else(|| format!("Bus '{name}' not found"))?
                .effects
        }
        Target::Master => &mut project.master.get_or_insert_with(empty_master).effects,
    };
    effects
        .get_mut(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))
}

/// Route an effect's detector key from another track or bus, or clear it.
pub(crate) fn set_effect_sidechain_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    source: Option<String>,
) -> Result<(), String> {
    let source = source
        .map(|source| source.trim().to_string())
        .filter(|source| !source.is_empty());
    let mut next = inner.project.clone();
    let effect = sidechain_effect_mut(&mut next, target, effect_index)?;
    if effect.sidechain_source == source {
        return Ok(());
    }
    effect.sidechain_source = source;
    // A key re-wires the graph's detector edges and their alignment, which is a
    // structural change to the graph, so it keeps the full rebuild.
    // The validator owns which key an owner may read: a track chain reads a
    // track only, so a bus key is refused in the validator's own sentence.
    oc_dsl::validate(&next).map_err(|errors| validation_text(&errors))?;
    let old_project = std::mem::replace(&mut inner.project, next);
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("effect sidechain", UndoScope::MixerRouting),
    )
}

/// Set the sidechain source for a track effect.
#[tauri::command]
pub fn set_effect_sidechain(
    target: String,
    effect_index: usize,
    source: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_effect_sidechain_inner(&mut inner, &target, effect_index, source)?;
    Ok(snapshot(&inner))
}

/// Start or stop the spectrum analyser for one panel target.
#[tauri::command]
pub fn set_spectrum(target: Option<String>, state: State<AppState>) {
    state.lock_inner().audio.set_spectrum(target);
}

// ============================================================================
// Effect-lane editing
// ============================================================================

/// A copy of one panel target's automation lanes, in pool order.
///
/// A track keeps its lanes in the clip pool, so an owner is edited by copying
/// its lanes, changing them and writing them back.
pub(crate) fn owner_lanes(
    project: &ast::Project,
    target: &str,
) -> Result<Vec<ast::AutomationLane>, String> {
    let owner = parse_target(target)?.automation_owner();
    if let ast::AutomationOwner::Bus(name) = &owner
        && !project.buses.iter().any(|bus| &bus.name == name)
    {
        return Err(format!("Bus '{name}' not found"));
    }
    if let ast::AutomationOwner::Track(index) = owner
        && index >= project.tracks.len()
    {
        return Err(format!("Track {index} out of range"));
    }
    Ok(project.automation(&owner).into_iter().cloned().collect())
}

/// Write one panel target's automation lanes back.
pub(crate) fn replace_owner_lanes(
    project: &mut ast::Project,
    target: &str,
    lanes: Vec<ast::AutomationLane>,
) -> Result<(), String> {
    match parse_target(target)? {
        Target::Track(index) => project
            .tracks
            .get_mut(index)
            .ok_or_else(|| format!("Track {index} out of range"))?
            .set_automation_lanes(lanes),
        Target::Bus(name) => {
            project
                .buses
                .iter_mut()
                .find(|bus| bus.name == name)
                .ok_or_else(|| format!("Bus '{name}' not found"))?
                .automation = lanes
        }
        Target::Master => project.master.get_or_insert_with(empty_master).automation = lanes,
    }
    Ok(())
}

/// Lane index for a `(param, index)` key.
///
/// An index is authoritative: it addresses one lane even when several lanes
/// share the parameter. Without one the parameter must name exactly one lane,
/// so two clips on one target are refused with a message naming the ambiguity
/// instead of silently editing the first.
pub(crate) fn resolve_lane_index(
    lanes: &[ast::AutomationLane],
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<usize, String> {
    let refs: Vec<&ast::AutomationLane> = lanes.iter().collect();
    resolve_lane_index_in(&refs, lane_param, lane_index)
}

/// [`resolve_lane_index`] over borrowed lanes.
pub(crate) fn resolve_lane_index_in(
    lanes: &[&ast::AutomationLane],
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<usize, String> {
    if let Some(index) = lane_index {
        let lane = lanes.get(index).ok_or_else(|| {
            format!(
                "Automation lane {index} is out of range: the owner has {} lane(s)",
                lanes.len()
            )
        })?;
        if lane.param != lane_param {
            return Err(format!(
                "Automation lane {index} targets '{}', not '{lane_param}'",
                lane.param
            ));
        }
        return Ok(index);
    }
    let mut matches = lanes
        .iter()
        .enumerate()
        .filter(|(_, lane)| lane.param == lane_param)
        .map(|(index, _)| index);
    let Some(first) = matches.next() else {
        return Err(format!("Automation lane '{lane_param}' not found"));
    };
    let rest: Vec<usize> = matches.collect();
    if rest.is_empty() {
        return Ok(first);
    }
    let mut indices = vec![first];
    indices.extend(rest);
    let list = indices
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Automation target '{lane_param}' has {} lanes ({list}); pass a lane index to choose one",
        indices.len()
    ))
}

fn lane_mut_at<'a>(
    project: &'a mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
) -> Result<&'a mut ast::AutomationLane, String> {
    let owner = parse_target(target)?.automation_owner();
    let index = {
        let lanes = owner_lanes(project, target)?;
        resolve_lane_index(&lanes, lane_param, lane_index)?
    };
    project
        .automation_lane_mut(&owner, index)
        .ok_or_else(|| format!("Automation lane {index} is out of range"))
}

/// Refusal for every point edit on a muted lane.
pub(crate) const MUTED_LANE: &str = "This lane is muted.";

/// A bare number written in the unit the lane already uses, so a point added
/// to a `dB` lane stays in dB instead of switching that point to a plain
/// number.
pub(crate) fn lane_unit_value(lane: &ast::AutomationLane, value: f64) -> ast::ParamValue {
    match lane.points.first().map(|point| &point.value) {
        Some(ast::ParamValue::Db(_)) => ast::ParamValue::Db(value),
        Some(ast::ParamValue::Percentage(_)) => ast::ParamValue::Percentage(value),
        Some(ast::ParamValue::Frequency(_)) => ast::ParamValue::Frequency(value),
        Some(ast::ParamValue::Cents(_)) => ast::ParamValue::Cents(value),
        Some(ast::ParamValue::Time(_)) => ast::ParamValue::Time(value),
        _ => ast::ParamValue::Number(value),
    }
}

/// Check a position/value pair before any lane is touched.
fn check_point(position_beats: f64, value: f64) -> Result<(), String> {
    if !position_beats.is_finite() || position_beats < 0.0 {
        return Err("Automation position must be a non-negative finite beat".into());
    }
    if !value.is_finite() {
        return Err("Automation value must be a finite number".into());
    }
    Ok(())
}

/// Refuse a point value outside the range the lane's device declares.
///
/// An effect lane is authored in the device's own unit, so the value goes
/// through the same schema the renderer reads and a refusal names the range. A
/// patch or control lane has no device schema here and keeps the finite-number
/// check; a hosted-plugin lane is checked by the plugin, not by us.
fn check_effect_value(
    project: &ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    value: f64,
) -> Result<(), String> {
    let lanes = owner_lanes(project, target)?;
    let index = resolve_lane_index(&lanes, lane_param, lane_index)?;
    let lane = &lanes[index];
    let Some((effect_target, parameter)) = lane.effect_lane() else {
        return Ok(());
    };
    let effects: &[ast::Effect] = match parse_target(target)? {
        Target::Track(index) => {
            &project
                .tracks
                .get(index)
                .ok_or_else(|| format!("Track {index} out of range"))?
                .effects
        }
        Target::Bus(name) => {
            &project
                .buses
                .iter()
                .find(|bus| bus.name == name)
                .ok_or_else(|| format!("Bus '{name}' not found"))?
                .effects
        }
        Target::Master => {
            &project
                .master
                .as_ref()
                .ok_or("The project has no master section")?
                .effects
        }
    };
    let effect_index = oc_dsl::automation::resolve_effect(effects, effect_target)
        .map_err(|error| format!("automation lane '{}': {error}", lane.param))?;
    oc_engine::automation::effect_point_value(
        &effects[effect_index],
        parameter,
        &lane_unit_value(lane, value),
    )
    .map(|_| ())
}

pub(crate) fn apply_lane_smoothing(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    smoothing_ms: f64,
) -> Result<(), String> {
    if !smoothing_ms.is_finite() || !(0.0..=ast::MAX_SMOOTHING_MS).contains(&smoothing_ms) {
        return Err(format!(
            "Smoothing must be within 0..={} ms",
            ast::MAX_SMOOTHING_MS
        ));
    }
    lane_mut_at(project, target, lane_param, lane_index)?.smoothing_ms = smoothing_ms;
    Ok(())
}

/// Change only the lane's mode: every other field, including the cycle
/// controls, is left as the project had it so a round trip through envelope
/// mode does not lose an LFO setup.
pub(crate) fn apply_lane_mode(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    mode: &str,
) -> Result<(), String> {
    let mode = ast::LaneMode::from_keyword(mode)
        .ok_or_else(|| format!("Unknown automation mode '{mode}'"))?;
    lane_mut_at(project, target, lane_param, lane_index)?.mode = mode;
    Ok(())
}

pub(crate) fn apply_lane_lfo(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    lfo: ast::LaneLfo,
) -> Result<(), String> {
    oc_dsl::automation::validate_lane_lfo(&lfo)?;
    lane_mut_at(project, target, lane_param, lane_index)?.lfo = lfo;
    Ok(())
}

pub(crate) fn apply_lane_point_curve(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
    curve: &str,
    tension: Option<f64>,
) -> Result<(), String> {
    let curve =
        ast::Curve::from_keyword(curve).ok_or_else(|| format!("Unknown curve '{curve}'"))?;
    let lane = lane_mut_at(project, target, lane_param, lane_index)?;
    let point = lane
        .points
        .get_mut(point_index)
        .ok_or_else(|| format!("Point {point_index} is out of range"))?;
    // Validate before mutating so a rejected edit leaves the point untouched.
    let tension = if curve == ast::Curve::Smooth {
        let tension = tension.or(point.tension).unwrap_or(1.0);
        if !tension.is_finite() || !(0.0..=1.0).contains(&tension) {
            return Err("Tension must be within 0..=1".into());
        }
        Some(tension)
    } else {
        None
    };
    point.curve = curve;
    point.tension = tension;
    Ok(())
}

/// Add one point at a quarter-note position, keeping the list in musical order.
///
/// The point is the only thing written: a position outside the lane's span is
/// accepted, because a span gates writes, not authoring.
pub(crate) fn apply_add_point(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    position_beats: f64,
    value: f64,
) -> Result<(), String> {
    check_point(position_beats, value)?;
    check_effect_value(project, target, lane_param, lane_index, value)?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let position = clock.bar_beat_at_quarter(position_beats)?;
    let quarter = clock.quarter_notes_at(&position)?;
    let lane = lane_mut_at(project, target, lane_param, lane_index)?;
    if lane.muted {
        return Err(MUTED_LANE.into());
    }
    let value = lane_unit_value(lane, value);
    let at = lane.points.partition_point(|point| {
        clock
            .quarter_notes_at(&point.position)
            .is_ok_and(|existing| existing < quarter)
    });
    lane.points.insert(
        at,
        ast::AutomationPoint {
            position,
            value,
            curve: ast::Curve::default(),
            tension: None,
        },
    );
    Ok(())
}

/// Remove one point from a lane.
pub(crate) fn apply_remove_point(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
) -> Result<(), String> {
    let lane = lane_mut_at(project, target, lane_param, lane_index)?;
    if lane.muted {
        return Err(MUTED_LANE.into());
    }
    if point_index >= lane.points.len() {
        return Err(format!(
            "Point {point_index} is out of range: the lane has {} point(s)",
            lane.points.len()
        ));
    }
    lane.points.remove(point_index);
    Ok(())
}

/// Move one point in both axes. The index is kept, so a point dragged past its
/// neighbour stays the point the gesture grabbed; the evaluator sorts.
pub(crate) fn apply_move_point(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
    position_beats: f64,
    value: f64,
) -> Result<(), String> {
    check_point(position_beats, value)?;
    check_effect_value(project, target, lane_param, lane_index, value)?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let position = clock.bar_beat_at_quarter(position_beats)?;
    let lane = lane_mut_at(project, target, lane_param, lane_index)?;
    if lane.muted {
        return Err(MUTED_LANE.into());
    }
    if point_index >= lane.points.len() {
        return Err(format!(
            "Point {point_index} is out of range: the lane has {} point(s)",
            lane.points.len()
        ));
    }
    let value = lane_unit_value(lane, value);
    let point = &mut lane.points[point_index];
    point.position = position;
    point.value = value;
    Ok(())
}

/// Silence or restore one lane. A muted lane writes nothing, so the target
/// keeps the value its `effects:` entry declares.
pub(crate) fn apply_lane_mute(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    muted: bool,
) -> Result<(), String> {
    lane_mut_at(project, target, lane_param, lane_index)?.muted = muted;
    Ok(())
}

pub(crate) fn set_automation_smoothing_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    smoothing_ms: f64,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_lane_smoothing(
        &mut inner.project,
        target,
        lane_param,
        lane_index,
        smoothing_ms,
    )?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("automation smoothing", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn set_automation_point_curve_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
    curve: &str,
    tension: Option<f64>,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_lane_point_curve(
        &mut inner.project,
        target,
        lane_param,
        lane_index,
        point_index,
        curve,
        tension,
    )?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("automation curve", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn set_automation_mode_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    mode: &str,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_lane_mode(&mut inner.project, target, lane_param, lane_index, mode)?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("automation mode", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn set_automation_lfo_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    lfo: ast::LaneLfo,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_lane_lfo(&mut inner.project, target, lane_param, lane_index, lfo)?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("automation lfo", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn add_automation_point_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    position_beats: f64,
    value: f64,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_add_point(
        &mut inner.project,
        target,
        lane_param,
        lane_index,
        position_beats,
        value,
    )?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("add automation point", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn remove_automation_point_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_remove_point(
        &mut inner.project,
        target,
        lane_param,
        lane_index,
        point_index,
    )?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("remove automation point", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn move_automation_point_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    point_index: usize,
    position_beats: f64,
    value: f64,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_move_point(
        &mut inner.project,
        target,
        lane_param,
        lane_index,
        point_index,
        position_beats,
        value,
    )?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("move automation point", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

pub(crate) fn set_automation_lane_mute_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    muted: bool,
) -> Result<(), String> {
    let old_project = inner.project.clone();
    apply_lane_mute(&mut inner.project, target, lane_param, lane_index, muted)?;
    resync_effect_edit(
        inner,
        old_project,
        UndoEdit::new("automation mute", UndoScope::Automation),
        target,
    )?;
    Ok(())
}

/// Set smoothing for an automation lane.
#[tauri::command]
pub fn set_automation_smoothing(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    smoothing_ms: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_automation_smoothing_inner(&mut inner, &target, &lane_param, lane_index, smoothing_ms)?;
    Ok(snapshot(&inner))
}

/// Set the curve of one automation point.
#[tauri::command]
pub fn set_automation_point_curve(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    point_index: usize,
    curve: String,
    tension: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_automation_point_curve_inner(
        &mut inner,
        &target,
        &lane_param,
        lane_index,
        point_index,
        &curve,
        tension,
    )?;
    Ok(snapshot(&inner))
}

/// Set an automation lane mode.
#[tauri::command]
pub fn set_automation_mode(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    mode: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_automation_mode_inner(&mut inner, &target, &lane_param, lane_index, &mode)?;
    Ok(snapshot(&inner))
}

/// Set the LFO settings of an automation lane.
#[tauri::command]
pub fn set_automation_lfo(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    lfo: ast::LaneLfo,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_automation_lfo_inner(&mut inner, &target, &lane_param, lane_index, lfo)?;
    Ok(snapshot(&inner))
}

/// Add one point to a lane at a quarter-note position.
///
/// `lane_index` keys the lane; without one `lane_param` must name one lane.
#[tauri::command]
pub fn add_automation_point(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    position_beats: f64,
    value: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    add_automation_point_inner(
        &mut inner,
        &target,
        &lane_param,
        lane_index,
        position_beats,
        value,
    )?;
    Ok(snapshot(&inner))
}

/// Remove one point from a lane.
#[tauri::command]
pub fn remove_automation_point(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    point_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_automation_point_inner(&mut inner, &target, &lane_param, lane_index, point_index)?;
    Ok(snapshot(&inner))
}

/// Move one point in both axes; the index stays the grabbed point.
#[tauri::command]
pub fn move_automation_point(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    point_index: usize,
    position_beats: f64,
    value: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    move_automation_point_inner(
        &mut inner,
        &target,
        &lane_param,
        lane_index,
        point_index,
        position_beats,
        value,
    )?;
    Ok(snapshot(&inner))
}

/// Mute or unmute one lane.
#[tauri::command]
pub fn set_automation_lane_mute(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    muted: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    set_automation_lane_mute_inner(&mut inner, &target, &lane_param, lane_index, muted)?;
    Ok(snapshot(&inner))
}

// ============================================================================
// Effect-parameter writes
// ============================================================================

/// Lane an effect-parameter write addresses: `fx<N>.<param>`.
///
/// The chain-position form is the only one that stays unambiguous when two
/// entries of one type sit in the same chain, so a write always uses it.
pub(crate) fn effect_write_lane(effect_index: usize, parameter: &str) -> String {
    format!("fx{}.{}", effect_index + 1, parameter)
}

/// The chain entry a write addresses.
fn effect_for_write<'a>(
    project: &'a ast::Project,
    target: &str,
    effect_index: usize,
) -> Result<&'a ast::Effect, String> {
    if target.starts_with("preset:") {
        return Err("A preset chain owns no automation lanes.".into());
    }
    let effects = match parse_target(target)? {
        Target::Track(index) => {
            &project
                .tracks
                .get(index)
                .ok_or_else(|| format!("Track {index} out of range"))?
                .effects
        }
        Target::Bus(name) => {
            &project
                .buses
                .iter()
                .find(|bus| bus.name == name)
                .ok_or_else(|| format!("Bus '{name}' not found"))?
                .effects
        }
        Target::Master => {
            &project
                .master
                .as_ref()
                .ok_or_else(|| "Master not found".to_string())?
                .effects
        }
    };
    effects
        .get(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))
}

/// Refuse a write before any point lands: a hosted plugin has no device schema,
/// and a built-in device names its own parameters.
fn validate_effect_write(
    project: &ast::Project,
    target: &str,
    effect_index: usize,
    parameter: &str,
) -> Result<(), String> {
    let effect = effect_for_write(project, target, effect_index)?;
    if oc_engine::dsp::device_for_effect(&effect.effect_type).is_none() {
        return Err("The chain entry is a plugin; use its own panel.".into());
    }
    oc_dsl::automation::validate_device_param(effect, parameter)
}

/// Check one point's value in the device's own unit and range.
fn validate_effect_write_value(
    project: &ast::Project,
    target: &str,
    effect_index: usize,
    parameter: &str,
    value: &ast::ParamValue,
) -> Result<(), String> {
    let effect = effect_for_write(project, target, effect_index)?;
    let device = oc_engine::dsp::device_for_effect(&effect.effect_type)
        .ok_or_else(|| "The chain entry is a plugin; use its own panel.".to_string())?;
    oc_engine::dsp::validate_param(device, parameter, value).map(|_| ())
}

/// Replace the point at `position`, creating the lane when the parameter has
/// none yet. A muted lane refuses the write like every other point edit.
pub(crate) fn upsert_effect_lane_point(
    project: &mut ast::Project,
    target: &str,
    lane_param: &str,
    position: ast::BarBeat,
    value: ast::ParamValue,
) -> Result<(), String> {
    let mut lanes = owner_lanes(project, target)?;
    let index = match lanes.iter().position(|lane| lane.param == lane_param) {
        Some(index) => index,
        None => {
            lanes.push(ast::AutomationLane {
                param: lane_param.to_string(),
                ppq: 0,
                smoothing_ms: 0.0,
                points: Vec::new(),
                ..Default::default()
            });
            lanes.len() - 1
        }
    };
    let lane = &mut lanes[index];
    if lane.muted {
        return Err(MUTED_LANE.into());
    }
    let point = ast::AutomationPoint {
        position,
        value,
        curve: ast::Curve::Linear,
        tension: None,
    };
    match lane
        .points
        .iter_mut()
        .find(|existing| existing.position == position)
    {
        Some(existing) => *existing = point,
        None => {
            lane.points.push(point);
            lane.points.sort_by_key(|entry| {
                (
                    entry.position.bar,
                    entry.position.beat,
                    entry.position.subdivision,
                )
            });
        }
    }
    replace_owner_lanes(project, target, lanes)?;
    Ok(())
}

fn effect_write_key(target: &str, effect_index: usize, parameter: &str) -> String {
    format!("{target}\u{0}{effect_index}\u{0}{parameter}")
}

pub(crate) fn begin_effect_param_write_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    let parameter = parameter.trim();
    if parameter.is_empty() {
        return Err("Effect parameter name cannot be empty".into());
    }
    validate_effect_write(&inner.project, target, effect_index, parameter)?;
    let key = effect_write_key(target, effect_index, parameter);
    if inner
        .effect_write_gestures
        .get(&key)
        .is_some_and(|session| inner.undo.is_owned_gesture_active(&session.history_owner))
    {
        return Err(format!(
            "Automation gesture already active for '{parameter}'"
        ));
    }
    let history_owner = inner
        .undo
        .begin_owned_gesture(UndoScope::Automation)
        .map_err(|error| error.to_string())?;
    inner.effect_write_gestures.insert(
        key,
        crate::state::EffectWriteGesture {
            history_owner,
            owner: None,
            old_project: inner.project.clone(),
        },
    );
    Ok(())
}

/// One sampled point. The chain entry keeps the written value, so the panel and
/// the next render agree with the lane.
pub(crate) fn update_effect_param_write_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
    position: ast::BarBeat,
    value_text: &str,
) -> Result<(), String> {
    owned_gestures::publish_effect_write_point(
        inner,
        target,
        effect_index,
        parameter,
        position,
        value_text,
        None,
    )
}

fn prepare_effect_write_point(
    project: &mut ast::Project,
    target: &str,
    effect_index: usize,
    parameter: &str,
    position: ast::BarBeat,
    value_text: &str,
) -> Result<(), String> {
    oc_dsl::timing::ProjectClock::new(&project.properties)?.quarter_notes_at(&position)?;
    let value = parse_param_value(value_text)?;
    validate_effect_write_value(project, target, effect_index, parameter, &value)?;
    {
        let effect = effect_chain_mut(project, target)?
            .get_mut(effect_index)
            .ok_or_else(|| format!("Effect index {effect_index} out of range"))?;
        match effect.params.iter_mut().find(|(name, _)| name == parameter) {
            Some(slot) => slot.1 = value.clone(),
            None => effect.params.push((parameter.to_string(), value.clone())),
        }
    }
    upsert_effect_lane_point(
        project,
        target,
        &effect_write_lane(effect_index, parameter),
        position,
        value,
    )?;
    Ok(())
}

/// Close the write as one undo entry labelled `write automation`. A gesture
/// that changed nothing records nothing.
pub(crate) fn end_effect_param_write_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    parameter: &str,
) -> Result<(), String> {
    owned_gestures::finish_effect_write(inner, target, effect_index, parameter, None).map(|_| ())
}

/// Open one effect-parameter write: a drag writes points until `end`.
#[tauri::command]
pub fn begin_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    state: State<AppState>,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    begin_effect_param_write_inner(&mut inner, &target, effect_index, &param)
}

/// Write one point into the lane, creating the lane on the first write.
#[tauri::command]
pub fn update_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    position: ast::BarBeat,
    value: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_effect_param_write_inner(&mut inner, &target, effect_index, &param, position, &value)?;
    Ok(snapshot(&inner))
}

/// Close the write: one drag is one undo entry.
#[tauri::command]
pub fn end_effect_param_write(
    target: String,
    effect_index: usize,
    param: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    end_effect_param_write_inner(&mut inner, &target, effect_index, &param)?;
    Ok(snapshot(&inner))
}
