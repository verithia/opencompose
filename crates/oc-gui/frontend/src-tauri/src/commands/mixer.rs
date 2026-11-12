//! Track mix, bus routing, sends, and channel presentation state.

use tauri::State;

use oc_dsl::ast;

use crate::preview::READ_ONLY;
use crate::state::AppState;
use crate::state::{MixerTouch, UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, resync_mixer_with_undo, resync_with_undo, snapshot};

/// The fader target a pointer gesture owns: one track, or the master.
fn track_target(track_index: usize) -> String {
    format!("track:{track_index}")
}

/// Reject a level that the mixer cannot represent before anything is written.
fn validate_track_mix(volume_db: Option<f64>, pan: Option<f64>) -> Result<(), String> {
    if volume_db.is_some_and(|value| !value.is_finite() || !(-120.0..=24.0).contains(&value)) {
        return Err("track volume must be finite and within -120..24 dB".into());
    }
    if pan.is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value)) {
        return Err("track pan must be finite and within -1..1".into());
    }
    Ok(())
}

/// Apply a subset of a track's fader and pan.
fn apply_track_mix(
    project: &mut ast::Project,
    track_index: usize,
    volume_db: Option<f64>,
    pan: Option<f64>,
) -> Result<(), String> {
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {track_index} out of range"))?;
    if let Some(volume_db) = volume_db {
        track.mix.volume_db = volume_db;
    }
    if let Some(pan) = pan {
        track.mix.pan = pan;
    }
    Ok(())
}

/// Take the pre-gesture project when the open touch belongs to this target.
/// A touch for another target stays open for its own commit.
fn take_touch_base(inner: &mut crate::state::AppStateInner, target: &str) -> Option<ast::Project> {
    let matches = inner
        .mixer_touch
        .as_ref()
        .is_some_and(|touch| touch.target == target);
    if matches {
        inner.mixer_touch.take().map(|touch| touch.old_project)
    } else {
        None
    }
}

pub(crate) fn update_track_mix_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    volume_db: Option<f64>,
    pan: Option<f64>,
) -> Result<(), String> {
    validate_track_mix(volume_db, pan)?;
    if inner.project.tracks.get(track_index).is_none() {
        return Err(format!("Track {track_index} out of range"));
    }
    // A live drag already moved the AST; its gesture started from the project
    // the pointer grabbed, so the undo entry must diff against that document.
    let old_project =
        take_touch_base(inner, &track_target(track_index)).unwrap_or_else(|| inner.project.clone());
    apply_track_mix(&mut inner.project, track_index, volume_db, pan)?;
    resync_mixer_with_undo(
        inner,
        old_project,
        UndoEdit::new("mixer levels", UndoScope::MixerLevels),
    )
}

/// Update one track mix settings.
#[tauri::command]
pub fn update_track_mix(
    track_index: usize,
    volume_db: Option<f64>,
    pan: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_track_mix_inner(&mut inner, track_index, volume_db, pan)?;
    Ok(snapshot(&inner))
}

/// Open a mixer fader drag. The pre-gesture project is kept until the pointer
/// releases, so the live updates in between do not serialize or record undo.
#[tauri::command]
pub fn begin_mixer_touch(target: String, state: State<AppState>) -> Result<(), String> {
    let mut inner = state.lock_inner();
    begin_mixer_touch_inner(&mut inner, &target)
}

pub(crate) fn begin_mixer_touch_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
) -> Result<(), String> {
    if inner.preview.is_some() {
        return Err(READ_ONLY.to_string());
    }
    if let Some(index) = track_index_of(target) {
        if inner.project.tracks.get(index).is_none() {
            return Err(format!("Track {index} out of range"));
        }
    } else if target != "master" {
        return Err(format!("'{target}' is not a mixer target"));
    } else if inner.project.master.is_none() {
        return Err("No master section".into());
    }
    if inner.mixer_touch.is_some() {
        return Err("a mixer gesture is already active".into());
    }
    inner.mixer_touch = Some(MixerTouch {
        target: target.to_string(),
        old_project: inner.project.clone(),
    });
    Ok(())
}

/// Move a fader while the pointer holds it. The live graph hears the new value
/// without serializing the DSL, touching undo, or returning a snapshot.
#[tauri::command]
pub fn update_mixer_touch(
    target: String,
    volume_db: Option<f64>,
    pan: Option<f64>,
    state: State<AppState>,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    update_mixer_touch_inner(&mut inner, &target, volume_db, pan)
}

pub(crate) fn update_mixer_touch_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    volume_db: Option<f64>,
    pan: Option<f64>,
) -> Result<(), String> {
    validate_track_mix(volume_db, pan)?;
    if inner.preview.is_some() {
        return Err(READ_ONLY.to_string());
    }
    match inner.mixer_touch.as_ref() {
        Some(touch) if touch.target == target => {}
        Some(touch) => {
            return Err(format!(
                "the active mixer gesture belongs to '{}'",
                touch.target
            ));
        }
        None => return Err("no active mixer gesture".into()),
    }
    if let Some(index) = track_index_of(target) {
        apply_track_mix(&mut inner.project, index, volume_db, pan)?;
    } else if pan.is_some() {
        return Err("the master has no pan".into());
    } else if let Some(master) = inner.project.master.as_mut()
        && let Some(volume_db) = volume_db
    {
        master.volume_db = volume_db;
    }
    inner.audio.apply_mixer_change(&inner.project);
    Ok(())
}

fn track_index_of(target: &str) -> Option<usize> {
    target.strip_prefix("track:")?.parse().ok()
}

/// Set the master volume in dB.
#[tauri::command]
pub fn set_master_volume(
    volume_db: f64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    if !volume_db.is_finite() || !(-120.0..=24.0).contains(&volume_db) {
        return Err("master volume must be finite and within -120..24 dB".into());
    }
    let mut inner = state.lock_inner();
    let old_project =
        take_touch_base(&mut inner, "master").unwrap_or_else(|| inner.project.clone());
    match &mut inner.project.master {
        Some(master) => master.volume_db = volume_db,
        None => {
            inner.project.master = Some(ast::Master {
                gain_automation: Vec::new(),
                automation: Vec::new(),
                effects: vec![],
                loudness_target: None,
                volume_db,
                input_routes: vec![],
            })
        }
    }
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("master level", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

/// Mute or unmute one track.
#[tauri::command]
pub fn set_track_mute(
    track_index: usize,
    mute: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?
        .mix
        .mute = mute;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track mute", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

/// Solo or unsolo one track.
#[tauri::command]
pub fn set_track_solo(
    track_index: usize,
    solo: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?
        .mix
        .solo = solo;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track solo", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

fn bus_mut<'a>(project: &'a mut ast::Project, name: &str) -> Result<&'a mut ast::Bus, String> {
    project
        .buses
        .iter_mut()
        .find(|bus| bus.name == name)
        .ok_or_else(|| format!("Bus '{}' not found", name))
}

pub(crate) fn apply_bus_mix(
    project: &mut ast::Project,
    name: &str,
    volume_db: Option<f64>,
    mute: Option<bool>,
    solo: Option<bool>,
) -> Result<(), String> {
    let bus = bus_mut(project, name)?;
    if let Some(volume_db) = volume_db {
        bus.volume_db = volume_db;
    }
    if let Some(mute) = mute {
        bus.mute = mute;
    }
    if let Some(solo) = solo {
        bus.solo = solo;
    }
    Ok(())
}

/// Update any subset of a bus return's fader, mute and solo as one undo entry.
#[tauri::command]
pub fn update_bus_mix(
    name: String,
    volume_db: Option<f64>,
    mute: Option<bool>,
    solo: Option<bool>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    if volume_db.is_some_and(|value| !value.is_finite() || !(-120.0..=24.0).contains(&value)) {
        return Err("bus volume must be finite and within -120..24 dB".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_bus_mix(&mut inner.project, &name, volume_db, mute, solo)?;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("bus levels", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

/// Mute or unmute one bus.
#[tauri::command]
pub fn set_bus_mute(
    name: String,
    mute: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    update_bus_mix(name, None, Some(mute), None, state)
}

/// Solo or unsolo one bus.
#[tauri::command]
pub fn set_bus_solo(
    name: String,
    solo: bool,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    update_bus_mix(name, None, None, Some(solo), state)
}

/// Rename one track.
#[tauri::command]
pub fn rename_track(
    track_index: usize,
    new_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    track.name = new_name;
    // Track names affect DSL output but not audio — just resync for consistency
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track rename", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

// ============================================================================
// Latency readout
// ============================================================================

/// The three-part latency readout the inspector shows for one track.
#[derive(serde::Serialize)]
pub struct TrackLatencyReadoutResponse {
    pub readout: oc_engine::graph::TrackLatencyReadout,
    /// Where the readout differs from the live plan, when it can.
    pub note: Option<String>,
}

/// Read the latency a track reports.
#[tauri::command]
pub fn track_latency_readout(
    track_index: usize,
    state: State<AppState>,
) -> Result<TrackLatencyReadoutResponse, String> {
    let project = state.lock_inner().project.clone();
    plan_track_latency(&project, track_index)
}

/// Plan one track's latency from the project as declared.
///
/// The plan is built without hosting plugins: a readout must not instantiate
/// them, so external effect latency is left out until the project is streaming
/// (where the same plan picks it up).
pub(crate) fn plan_track_latency(
    project: &ast::Project,
    track_index: usize,
) -> Result<TrackLatencyReadoutResponse, String> {
    let name = project
        .tracks
        .get(track_index)
        .map(|track| track.name.clone())
        .ok_or_else(|| format!("Track {track_index} out of range"))?;
    let mut plan = project.clone();
    let mut skipped_plugins = false;
    for track in &mut plan.tracks {
        skipped_plugins |= strip_plugin_effects(&mut track.effects);
        // Effect-parameter lanes reference effect indices, which shift.
        let kept: Vec<ast::AutomationLane> = track
            .automation()
            .filter(|lane| oc_dsl::automation::effect_lane(&lane.param).is_none())
            .cloned()
            .collect();
        track.set_automation_lanes(kept);
    }
    for preset in &mut plan.presets {
        skipped_plugins |= strip_plugin_effects(&mut preset.effects);
    }
    let graph =
        oc_engine::graph::AudioGraph::try_from_project_with_source_latencies(&plan, 48_000, &[])?;
    let readout = graph
        .latency_readout_for(&name)
        .cloned()
        .ok_or_else(|| format!("No latency plan for track '{name}'"))?;
    Ok(TrackLatencyReadoutResponse {
        readout,
        note: skipped_plugins
            .then(|| "hosted plugin latency is not part of an inspector readout".to_string()),
    })
}

/// Drop external effect entries so planning never loads a plugin.
fn strip_plugin_effects(effects: &mut Vec<ast::Effect>) -> bool {
    let before = effects.len();
    effects.retain(|effect| !matches!(effect.effect_type, ast::EffectType::Plugin(_)));
    effects.len() != before
}

// ============================================================================
// Send routing
// ============================================================================

/// Select the exclusive main output, independently of auxiliary sends.
pub(crate) fn apply_set_track_output(
    project: &mut ast::Project,
    track_index: usize,
    output: &str,
) -> Result<(), String> {
    if output != "master" && !project.buses.iter().any(|bus| bus.name == output) {
        return Err(format!("Bus '{}' not found", output));
    }
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    track.mix.output = (output != "master").then(|| output.to_string());
    Ok(())
}

/// Select the instrument output port a track takes; port 0 stays unwritten.
pub(crate) fn apply_set_track_output_port(
    project: &mut ast::Project,
    track_index: usize,
    port: u16,
) -> Result<(), String> {
    if port > 63 {
        return Err("output port must be 0-63".into());
    }
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    track.output_port = (port > 0).then_some(port);
    Ok(())
}

/// Select a track output port.
#[tauri::command]
pub fn set_track_output_port(
    track_index: usize,
    port: u16,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_set_track_output_port(&mut inner.project, track_index, port)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track output port", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Route a track to a bus or the master.
#[tauri::command]
pub fn set_track_output(
    track_index: usize,
    output: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_set_track_output(&mut inner.project, track_index, &output)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track output", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Add or replace the send from one track tap into a bus.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_add_send(
    project: &mut ast::Project,
    track_index: usize,
    bus_name: &str,
    amount: f64,
    pre_fader: Option<bool>,
    source: Option<ast::SendSource>,
    pan: Option<f64>,
) -> Result<(), String> {
    if !amount.is_finite() || !(0.0..=4.0).contains(&amount) {
        return Err("send amount must be finite and within 0..=4 (+12 dB)".into());
    }
    if pan.is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value)) {
        return Err("send pan must be finite and within -1..=1".into());
    }
    if !project.buses.iter().any(|bus| bus.name == bus_name) {
        return Err(format!("Bus '{}' not found", bus_name));
    }

    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;

    if let Some(source) = &source {
        let hosts_plugin =
            source.owner == track.name || track.preset.as_deref() == Some(source.owner.as_str());
        if !hosts_plugin {
            return Err(format!(
                "track '{}' does not host plugin '{}'",
                track.name, source.owner
            ));
        }
        if source.output < 2 {
            return Err("auxiliary output out1 is the track's main output".into());
        }
    }

    // Replace the send from the same tap; the main and aux taps can coexist.
    track
        .mix
        .sends
        .retain(|send| !(send.target == bus_name && send.source == source));
    track.mix.sends.push(ast::Send {
        target: bus_name.to_string(),
        amount: amount.clamp(0.0, 4.0),
        source,
        pre_fader: pre_fader.unwrap_or(false),
        pan: pan.unwrap_or(0.0).clamp(-1.0, 1.0),
    });
    Ok(())
}

/// Resolve a graph node id (`track:0`, `bus:name`, `master`) to its effects
/// and device-input routes. The two borrows are disjoint fields of one owner.
fn input_route_owner<'a>(
    project: &'a mut ast::Project,
    owner: &str,
) -> Result<(&'a [ast::Effect], &'a mut Vec<ast::InputRoute>, String), String> {
    if owner == "master" {
        let master = project
            .master
            .as_mut()
            .ok_or_else(|| "this project has no master chain".to_string())?;
        return Ok((
            &master.effects,
            &mut master.input_routes,
            "master".to_string(),
        ));
    }
    if let Some(rest) = owner.strip_prefix("track:") {
        let index: usize = rest
            .parse()
            .map_err(|_| format!("Invalid mixer node '{owner}'"))?;
        let track = project
            .tracks
            .get_mut(index)
            .ok_or_else(|| format!("Track {index} out of range"))?;
        return Ok((
            &track.effects,
            &mut track.input_routes,
            format!("track '{}'", track.name),
        ));
    }
    if let Some(name) = owner.strip_prefix("bus:") {
        let bus = project
            .buses
            .iter_mut()
            .find(|bus| bus.name == name)
            .ok_or_else(|| format!("Bus '{name}' not found"))?;
        return Ok((&bus.effects, &mut bus.input_routes, format!("bus '{name}'")));
    }
    Err(format!("Invalid mixer node '{owner}'"))
}

/// The index of a control route's effect target, resolving the author's
/// address (`compression`, `fx2`) against the owner's current chain.
fn control_route_index(effects: &[ast::Effect], target: &str) -> Option<usize> {
    oc_dsl::automation::resolve_effect(effects, target).ok()
}

/// Tracks left in a control-edge cycle after adding one more edge. The graph
/// evaluates control before its consumer, so a cycle would never settle.
fn control_route_loop(
    project: &ast::Project,
    owner_track: usize,
    feed_track: usize,
) -> Option<Vec<String>> {
    let names: Vec<&str> = project
        .tracks
        .iter()
        .map(|track| track.name.as_str())
        .collect();
    let position = |name: &str| names.iter().position(|candidate| *candidate == name);
    let mut edges = vec![Vec::new(); names.len()];
    let mut indegree = vec![0_usize; names.len()];
    let add = |from: usize, to: usize, edges: &mut Vec<Vec<usize>>, indegree: &mut [usize]| {
        edges[from].push(to);
        indegree[to] += 1;
    };
    for (consumer, track) in project.tracks.iter().enumerate() {
        for route in track
            .input_routes
            .iter()
            .filter(|route| route.kind == ast::RouteKind::Control)
        {
            let Some(source) = project
                .properties
                .control_sources
                .iter()
                .find(|candidate| candidate.name == route.source)
            else {
                continue;
            };
            let Some(feed) = position(&source.source_track) else {
                continue;
            };
            if feed != consumer {
                add(feed, consumer, &mut edges, &mut indegree);
            }
        }
    }
    add(feed_track, owner_track, &mut edges, &mut indegree);
    let mut ready: Vec<usize> = (0..names.len())
        .filter(|index| indegree[*index] == 0)
        .collect();
    let mut ordered = 0;
    while let Some(node) = ready.pop() {
        ordered += 1;
        for &next in &edges[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }
    if ordered == names.len() {
        return None;
    }
    Some(
        names
            .iter()
            .enumerate()
            .filter(|(index, _)| indegree[*index] > 0)
            .map(|(_, name)| (*name).to_string())
            .collect(),
    )
}

/// Connect a `@control_source` reading to one device's control input.
///
/// This is the same edge the graph draws and the DSL writes as
/// `route: control <source> -> <effect>`; the command refuses an audio-only
/// device with a readable reason instead of ignoring the connection.
pub(crate) fn apply_add_control_route(
    project: &mut ast::Project,
    owner: &str,
    target: &str,
    source: &str,
) -> Result<(), String> {
    let declaration = project
        .properties
        .control_sources
        .iter()
        .find(|candidate| candidate.name == source)
        .ok_or_else(|| format!("Unknown control source '{source}'"))?;
    let feed_track = project
        .tracks
        .iter()
        .position(|track| track.name == declaration.source_track)
        .ok_or_else(|| {
            format!(
                "Control source '{source}' reads unknown track '{}'",
                declaration.source_track
            )
        })?;
    if owner == format!("track:{feed_track}") {
        return Err(format!(
            "Control input refused: '{source}' reads the same track that would consume it, and control is evaluated before its consumer"
        ));
    }
    if let Some(consumer) = owner.strip_prefix("track:")
        && let Ok(consumer) = consumer.parse::<usize>()
        && let Some(cycle) = control_route_loop(project, consumer, feed_track)
    {
        return Err(format!(
            "Control input refused: this edge would close a loop between tracks {}; control is evaluated before its consumer",
            cycle.join(" -> ")
        ));
    }
    {
        let (effects, routes, label) = input_route_owner(project, owner)?;
        let index = oc_dsl::automation::resolve_effect(effects, target)
            .map_err(|reason| format!("Route target '{target}' in {label}: {reason}"))?;
        let effect = &effects[index];
        let name = effect.effect_type.lane_name().to_string();
        let device = effect
            .effect_type
            .device_id()
            .and_then(oc_engine::dsp::schema::find_device)
            .ok_or_else(|| {
                format!(
                    "Control input refused: {name} in {label} does not declare a control input; only built-in devices expose one"
                )
            })?;
        if !device
            .ports
            .iter()
            .any(|port| port.kind == oc_engine::dsp::schema::PortKind::Control)
        {
            let ports = device
                .ports
                .iter()
                .map(|port| port.id)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Control input refused: {name} in {label} does not accept a control signal (its inputs are {ports})"
            ));
        }
        if effect.sidechain_source.is_some()
            || routes.iter().any(|route| {
                route.kind == ast::RouteKind::Audio
                    && control_route_index(effects, &route.target) == Some(index)
            })
        {
            return Err(format!(
                "Control input refused: {name} in {label} already has an audio key input; a device takes one detector input"
            ));
        }
        // Replace an existing control route to the same device: one edge per input.
        routes.retain(|route| {
            route.kind != ast::RouteKind::Control
                || control_route_index(effects, &route.target) != Some(index)
        });
        routes.push(ast::InputRoute {
            kind: ast::RouteKind::Control,
            source: source.to_string(),
            target: target.to_string(),
        });
        // An audio route for the same device is now impossible, but the
        // reverse order leaves a stale key; drop it explicitly.
        routes.retain(|route| {
            !(route.kind == ast::RouteKind::Audio
                && control_route_index(effects, &route.target) == Some(index))
        });
    }
    Ok(())
}

/// Disconnect one device's control input.
pub(crate) fn apply_remove_control_route(
    project: &mut ast::Project,
    owner: &str,
    target: &str,
) -> Result<(), String> {
    let (effects, routes, label) = input_route_owner(project, owner)?;
    let index = oc_dsl::automation::resolve_effect(effects, target)
        .map_err(|reason| format!("Route target '{target}' in {label}: {reason}"))?;
    let before = routes.len();
    routes.retain(|route| {
        !(route.kind == ast::RouteKind::Control
            && control_route_index(effects, &route.target) == Some(index))
    });
    if routes.len() == before {
        return Err(format!("No control route into '{target}' in {label}"));
    }
    Ok(())
}

/// Connect a control source to a device's control input in the selected node.
#[tauri::command]
pub fn add_control_route(
    owner: String,
    target: String,
    source: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_add_control_route(&mut inner.project, &owner, &target, &source)?;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add control route", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Disconnect a device's control input in the selected node.
#[tauri::command]
pub fn remove_control_route(
    owner: String,
    target: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_remove_control_route(&mut inner.project, &owner, &target)?;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove control route", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Add a send from a track to a bus. `pre_fader` defaults to post-fader.
pub fn add_send(
    track_index: usize,
    bus_name: String,
    amount: f64,
    pre_fader: Option<bool>,
    source_owner: Option<String>,
    source_output: Option<u8>,
    pan: Option<f64>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let source = match (source_owner, source_output) {
        (Some(owner), Some(output)) => Some(ast::SendSource { owner, output }),
        (None, None) => None,
        _ => return Err("auxiliary send needs both owner and output".into()),
    };
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_add_send(
        &mut inner.project,
        track_index,
        &bus_name,
        amount,
        pre_fader,
        source,
        pan,
    )?;
    if let Some(cycle) = routing_cycle_error(&inner.project) {
        inner.project = old_project;
        return Err(cycle);
    }
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add send", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

#[tauri::command]
// Tauri command arity is the frontend IPC contract.
#[allow(clippy::too_many_arguments)]
/// Update one existing send. Missing fields keep their current value.
pub fn set_send(
    track_index: usize,
    bus_name: String,
    amount: Option<f64>,
    pre_fader: Option<bool>,
    pan: Option<f64>,
    source_owner: Option<String>,
    source_output: Option<u8>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    if amount.is_some_and(|value| !value.is_finite() || !(0.0..=4.0).contains(&value)) {
        return Err("send amount must be finite and within 0..=4 (+12 dB)".into());
    }
    if pan.is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value)) {
        return Err("send pan must be finite and within -1..=1".into());
    }
    let source = match (source_owner, source_output) {
        (Some(owner), Some(output)) => Some(ast::SendSource { owner, output }),
        (None, None) => None,
        _ => return Err("auxiliary send needs both owner and output".into()),
    };
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let send = track
        .mix
        .sends
        .iter_mut()
        .find(|send| send.target == bus_name && send.source == source)
        .ok_or_else(|| format!("Send to '{}' not found", bus_name))?;
    if let Some(amount) = amount {
        send.amount = amount.clamp(0.0, 4.0);
    }
    if let Some(pre_fader) = pre_fader {
        send.pre_fader = pre_fader;
    }
    if let Some(pan) = pan {
        send.pan = pan.clamp(-1.0, 1.0);
    }
    if let Some(cycle) = routing_cycle_error(&inner.project) {
        inner.project = old_project;
        return Err(cycle);
    }
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("send level", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

/// Move one existing send to another bus without touching its level, tap or pan.
pub(crate) fn apply_set_send_target(
    project: &mut ast::Project,
    track_index: usize,
    from_bus: &str,
    to_bus: &str,
    source: Option<ast::SendSource>,
) -> Result<(), String> {
    if !project.buses.iter().any(|bus| bus.name == to_bus) {
        return Err(format!("Bus '{}' not found", to_bus));
    }
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    let index = track
        .mix
        .sends
        .iter()
        .position(|send| send.target == from_bus && send.source == source)
        .ok_or_else(|| format!("Send to '{}' not found", from_bus))?;
    if track
        .mix
        .sends
        .iter()
        .enumerate()
        .any(|(i, send)| i != index && send.target == to_bus && send.source == source)
    {
        return Err(format!(
            "Track '{}' already sends to '{}' from this tap",
            track.name, to_bus
        ));
    }
    track.mix.sends[index].target = to_bus.to_string();
    Ok(())
}

/// Re-point a send at another bus as one undo entry.
#[tauri::command]
pub fn set_send_target(
    track_index: usize,
    from_bus: String,
    to_bus: String,
    source_owner: Option<String>,
    source_output: Option<u8>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let source = match (source_owner, source_output) {
        (Some(owner), Some(output)) => Some(ast::SendSource { owner, output }),
        (None, None) => None,
        _ => return Err("auxiliary send needs both owner and output".into()),
    };
    let mut inner = state.lock_inner();
    if from_bus == to_bus {
        return Ok(snapshot(&inner));
    }
    let old_project = inner.project.clone();
    apply_set_send_target(&mut inner.project, track_index, &from_bus, &to_bus, source)?;
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("send routing", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Remove a send from a track
#[tauri::command]
pub fn remove_send(
    track_index: usize,
    bus_name: String,
    source_owner: Option<String>,
    source_output: Option<u8>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let source = match (source_owner, source_output) {
        (Some(owner), Some(output)) => Some(ast::SendSource { owner, output }),
        (None, None) => None,
        _ => return Err("auxiliary send needs both owner and output".into()),
    };
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    track
        .mix
        .sends
        .retain(|send| !(send.target == bus_name && send.source == source));
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove send", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Set presentation/solo overrides on one channel. `None` leaves a field
/// unchanged; an empty string clears the display name or colour.
#[tauri::command]
pub fn set_track_channel_overrides(
    track_index: usize,
    display_name: Option<String>,
    color: Option<String>,
    solo_safe: Option<bool>,
    polarity_inverted: Option<bool>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    for (field, value) in [("display name", &display_name), ("colour", &color)] {
        if let Some(value) = value
            && (value.len() > 64 || value.chars().any(char::is_control))
        {
            return Err(format!(
                "channel {field} must be at most 64 characters without control characters"
            ));
        }
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    if let Some(display_name) = display_name {
        track.mix.overrides.display_name =
            Some(display_name).filter(|name| !name.trim().is_empty());
    }
    if let Some(color) = color {
        track.mix.overrides.color = Some(color).filter(|value| !value.trim().is_empty());
    }
    if let Some(solo_safe) = solo_safe {
        track.mix.overrides.solo_safe = solo_safe;
    }
    if let Some(polarity_inverted) = polarity_inverted {
        track.mix.polarity_inverted = polarity_inverted;
    }
    resync_mixer_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("channel overrides", UndoScope::MixerLevels),
    )?;
    Ok(snapshot(&inner))
}

/// First bus-routing cycle in the project, if any.
pub(crate) fn routing_cycle_error(project: &ast::Project) -> Option<String> {
    oc_dsl::validate(project)
        .err()?
        .into_iter()
        .find_map(|error| {
            matches!(error, oc_dsl::error::ValidationError::RoutingCycle { .. })
                .then(|| error.to_string())
        })
}
/// Add a bus with the given routes. Returns the trimmed name that was pushed.
pub(crate) fn apply_add_bus(
    project: &mut ast::Project,
    name: &str,
    routes: Vec<String>,
) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Bus name cannot be empty".to_string());
    }
    if name == "master" || project.buses.iter().any(|bus| bus.name == name) {
        return Err(format!("Bus '{}' already exists", name));
    }
    for route in &routes {
        if route == &name {
            return Err(format!("Bus '{}' cannot route to itself", name));
        }
        if route != "master" && !project.buses.iter().any(|bus| &bus.name == route) {
            return Err(format!("Route target '{}' not found", route));
        }
    }
    project.buses.push(ast::Bus {
        gain_automation: Vec::new(),
        automation: Vec::new(),
        name: name.clone(),
        routes,
        effects: vec![],
        volume_db: 0.0,
        mute: false,
        solo: false,
        input_routes: vec![],
    });
    Ok(name)
}

/// Add a bus.
#[tauri::command]
pub fn add_bus(
    name: String,
    routes: Vec<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_add_bus(&mut inner.project, &name, routes)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add bus", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Set a bus routing graph.
#[tauri::command]
pub fn set_bus_routes(
    name: String,
    routes: Vec<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    set_bus_routes_in_project(&mut inner.project, &name, routes)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("bus routing", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

pub(crate) fn set_bus_routes_in_project(
    project: &mut ast::Project,
    name: &str,
    mut routes: Vec<String>,
) -> Result<(), String> {
    if !project.buses.iter().any(|bus| bus.name == name) {
        return Err(format!("Bus '{}' not found", name));
    }
    if routes.is_empty() {
        routes.push("master".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for route in &routes {
        if route == name {
            return Err(format!("Bus '{}' cannot route to itself", name));
        }
        if !seen.insert(route.clone()) {
            return Err(format!("Route target '{}' is duplicated", route));
        }
        if route != "master" && !project.buses.iter().any(|bus| &bus.name == route) {
            return Err(format!("Route target '{}' not found", route));
        }
    }

    let mut candidate = project.clone();
    candidate
        .buses
        .iter_mut()
        .find(|bus| bus.name == name)
        .expect("bus existence checked")
        .routes = routes;
    if let Err(errors) = oc_dsl::validate(&candidate)
        && let Some(error) = errors
            .into_iter()
            .find(|error| matches!(error, oc_dsl::error::ValidationError::RoutingCycle { .. }))
    {
        return Err(error.to_string());
    }
    *project = candidate;
    Ok(())
}

/// Every edge that keeps a bus in the routing graph. Read-only.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BusUsage {
    /// Tracks whose exclusive main output targets the bus.
    pub track_outputs: Vec<usize>,
    /// Tracks with at least one send into the bus.
    pub sends: Vec<usize>,
    /// Buses that route into the bus.
    pub bus_routes: Vec<String>,
}

impl BusUsage {
    pub(crate) fn is_empty(&self) -> bool {
        self.track_outputs.is_empty() && self.sends.is_empty() && self.bus_routes.is_empty()
    }

    pub(crate) fn describe(&self) -> String {
        fn count(n: usize, singular: &str, plural: &str) -> String {
            format!("{n} {}", if n == 1 { singular } else { plural })
        }
        let mut parts = Vec::new();
        if !self.track_outputs.is_empty() {
            parts.push(count(
                self.track_outputs.len(),
                "track output",
                "track outputs",
            ));
        }
        if !self.sends.is_empty() {
            parts.push(count(self.sends.len(), "send", "sends"));
        }
        if !self.bus_routes.is_empty() {
            parts.push(count(self.bus_routes.len(), "bus route", "bus routes"));
        }
        parts.join(", ")
    }
}

pub(crate) fn bus_usage(project: &ast::Project, name: &str) -> Result<BusUsage, String> {
    if !project.buses.iter().any(|bus| bus.name == name) {
        return Err(format!("Bus '{}' not found", name));
    }
    Ok(BusUsage {
        track_outputs: project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.mix.output.as_deref() == Some(name))
            .map(|(index, _)| index)
            .collect(),
        sends: project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.mix.sends.iter().any(|send| send.target == name))
            .map(|(index, _)| index)
            .collect(),
        bus_routes: project
            .buses
            .iter()
            .filter(|bus| bus.name != name && bus.routes.iter().any(|route| route == name))
            .map(|bus| bus.name.clone())
            .collect(),
    })
}

/// Delete a bus. A referenced bus is rejected unless `reroute` explicitly
/// accepts the documented re-route: main outputs to it return to master, sends
/// into it are dropped and buses routed only into it return to master.
pub(crate) fn apply_remove_bus(
    project: &mut ast::Project,
    name: &str,
    reroute: bool,
) -> Result<(), String> {
    let usage = bus_usage(project, name)?;
    if !usage.is_empty() && !reroute {
        return Err(format!(
            "Bus '{}' is in use ({}); re-route it explicitly to delete",
            name,
            usage.describe()
        ));
    }
    remove_bus_references(project, name)
}

/// Remove a bus.
#[tauri::command]
pub fn remove_bus(
    name: String,
    reroute: Option<bool>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_remove_bus(&mut inner.project, &name, reroute.unwrap_or(false))?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove bus", UndoScope::MixerRouting),
    )?;
    Ok(snapshot(&inner))
}

/// Remove a bus and every edge that targets it. Keeping dangling sends or
/// bus-to-bus routes makes the serialized project disagree with the rendered
/// routing graph and can silently drop audio after reopening a session.
pub(crate) fn remove_bus_references(project: &mut ast::Project, name: &str) -> Result<(), String> {
    if !project.buses.iter().any(|bus| bus.name == name) {
        return Err(format!("Bus '{}' not found", name));
    }

    project.buses.retain(|bus| bus.name != name);
    for track in &mut project.tracks {
        if track.mix.output.as_deref() == Some(name) {
            track.mix.output = None;
        }
        track.mix.sends.retain(|send| send.target != name);
    }
    for bus in &mut project.buses {
        let before = bus.routes.len();
        bus.routes.retain(|route| route != name);
        // Removing the last route would silently fall back to master; write it.
        if bus.routes.is_empty() && before > 0 {
            bus.routes.push("master".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONG: &str = r#"@ocp "0.2.0"

@project "mixer"
  tempo: 120

@track a | instrument: piano/acoustic_grand
"#;

    fn state() -> AppState {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            // Load the defaults now, as an opened project already carries them;
            // otherwise the first commit would diff against a raw parse.
            inner.project = oc_dsl::parse(SONG).unwrap();
            oc_dsl::instruments::apply_defaults(&mut inner.project);
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        state
    }

    #[test]
    fn a_track_mix_refuses_non_finite_and_out_of_range_values() {
        let state = state();
        let mut inner = state.lock_inner();
        for (volume_db, pan) in [
            (Some(f64::NAN), None),
            (Some(f64::INFINITY), None),
            (Some(24.1), None),
            (Some(-120.1), None),
            (None, Some(f64::NAN)),
            (None, Some(1.01)),
            (None, Some(-1.01)),
        ] {
            update_track_mix_inner(&mut inner, 0, volume_db, pan)
                .expect_err("a value outside the mixer range must be refused");
        }
        assert_eq!(inner.project.tracks[0].mix.volume_db, 0.0);
        assert_eq!(inner.project.tracks[0].mix.pan, 0.0);
        // A live update validates the same range before it patches the graph.
        begin_mixer_touch_inner(&mut inner, "track:0").unwrap();
        update_mixer_touch_inner(&mut inner, "track:0", Some(25.0), None)
            .expect_err("a live update must refuse an out-of-range level");
        update_mixer_touch_inner(&mut inner, "track:0", None, Some(-1.5))
            .expect_err("a live update must refuse an out-of-range pan");
        assert_eq!(inner.project.tracks[0].mix.volume_db, 0.0);
        assert_eq!(inner.project.tracks[0].mix.pan, 0.0);
        // The documented edges are accepted.
        update_track_mix_inner(&mut inner, 0, Some(-120.0), Some(-1.0)).unwrap();
        update_track_mix_inner(&mut inner, 0, Some(24.0), Some(1.0)).unwrap();
    }

    #[test]
    fn a_live_mixer_drag_commits_one_undo_entry() {
        let state = state();
        let mut inner = state.lock_inner();
        begin_mixer_touch_inner(&mut inner, "track:0").unwrap();
        for db in [-1.0, -3.0, -6.0] {
            update_mixer_touch_inner(&mut inner, "track:0", Some(db), None).unwrap();
        }
        assert_eq!(inner.project.tracks[0].mix.volume_db, -6.0);
        assert!(inner.undo.undo.is_empty(), "live updates record no history");
        assert!(!inner.dirty, "nothing is committed before the release");
        // The release is the only whole-document mutation.
        update_track_mix_inner(&mut inner, 0, Some(-6.0), None).unwrap();
        assert_eq!(inner.undo.undo.len(), 1);
        assert!(inner.dirty);
        assert!(inner.dsl_source.contains("-6"), "{}", inner.dsl_source);
    }

    #[test]
    fn a_live_drag_that_returns_to_its_start_records_no_entry() {
        let state = state();
        let mut inner = state.lock_inner();
        begin_mixer_touch_inner(&mut inner, "track:0").unwrap();
        update_mixer_touch_inner(&mut inner, "track:0", Some(-9.0), None).unwrap();
        update_track_mix_inner(&mut inner, 0, Some(0.0), None).unwrap();
        assert!(inner.undo.undo.is_empty());
        assert_eq!(inner.project.tracks[0].mix.volume_db, 0.0);
    }
}
