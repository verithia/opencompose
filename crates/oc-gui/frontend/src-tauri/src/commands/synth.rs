//! OpenSynth presets and patches, drum pads, and preset import.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State};

use oc_dsl::ast;
use oc_synth::synth::{
    DEFAULT_PREVIEW_SAMPLES, MAX_MOD_ROUTES, PREVIEW_SAMPLE_RATE, WaveformPreview,
    WaveformPreviewer,
};

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

mod owned_gestures;
pub use owned_gestures::*;

use super::plugin_host::{plugin_automation_lane_name, upsert_plugin_automation_point};
use super::shared::{ProjectSnapshot, parse_param_value, resync_with_undo, run_blocking, snapshot};

/// Preview renders share one FX rack so a slider drag does not rebuild it.
static WAVEFORM_PREVIEWER: Mutex<Option<WaveformPreviewer>> = Mutex::new(None);

/// Parameters that only name a source file for a whole-preset load. The editor
/// keeps them for provenance; their mapped parameters are already in the patch.
fn is_provenance_file(key: &str) -> bool {
    matches!(key, "serum_file" | "serum_root" | "open_synth_state_file")
}

/// Render one oscillator's real waveform for the patch editor. This is an
/// offline call; the trace comes from the same synth the instrument plays.
#[tauri::command]
pub fn synth_waveform_preview(
    params: Vec<(String, Option<String>)>,
    oscillator: u8,
    samples: Option<usize>,
    state: State<AppState>,
) -> Result<WaveformPreview, String> {
    if oscillator > 2 {
        return Err(format!("Oscillator {oscillator} out of range (0..=2)"));
    }
    let mut parsed = Vec::with_capacity(params.len());
    for (name, value) in params {
        let key = name.trim();
        if key.is_empty() || is_provenance_file(key) {
            continue;
        }
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            parsed.push((key.to_owned(), parse_param_value(&value)?));
        }
    }
    let root = {
        let inner = state.lock_inner();
        inner
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf)
    };
    let patch = crate::live_synth::patch(&parsed, root.as_deref())?;
    let mut renderer = WAVEFORM_PREVIEWER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let renderer = renderer.get_or_insert_with(WaveformPreviewer::new);
    Ok(renderer.render(
        &patch,
        usize::from(oscillator),
        samples.unwrap_or(DEFAULT_PREVIEW_SAMPLES),
        PREVIEW_SAMPLE_RATE,
    ))
}

/// Set a track's preset reference. The resync runs on the blocking pool so the
/// webview thread keeps painting while the audio graph is rebuilt.
#[tauri::command]
pub async fn set_track_preset(
    track_index: usize,
    preset_name: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        set_track_preset_blocking(&state, track_index, preset_name)
    })
    .await
}

/// The track-preset mutation without the Tauri wrapper. Shared with tests.
pub(crate) fn set_track_preset_blocking(
    state: &AppState,
    track_index: usize,
    preset_name: Option<String>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track {} out of range", track_index))?;
    track.preset = preset_name;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("track preset", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

/// Set a pattern's preset reference. Every clip of that pattern plays it
/// instead of its hosting track's preset. The resync runs on the blocking pool
/// so the webview thread keeps painting while the audio graph is rebuilt.
#[tauri::command]
pub async fn set_pattern_preset(
    pattern_name: String,
    preset: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        set_pattern_preset_blocking(&state, pattern_name, preset)
    })
    .await
}

/// The pattern-preset mutation without the Tauri wrapper. Shared with tests.
///
/// Clearing leaves `None`, so the pattern follows its hosting track again;
/// the resync may then inject the family default the editor shows for a bare
/// pattern, which is not an override.
pub(crate) fn set_pattern_preset_blocking(
    state: &AppState,
    pattern_name: String,
    preset: Option<String>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let pattern = inner
        .project
        .patterns
        .iter_mut()
        .find(|p| p.name == pattern_name)
        .ok_or_else(|| format!("Pattern '{}' not found", pattern_name))?;
    pattern.preset = preset.filter(|name| !name.trim().is_empty());
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("pattern preset", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

/// Add a new preset definition to the project
#[tauri::command]
pub fn add_preset(
    name: String,
    source: String,
    program: Option<u8>,
    plugin: Option<String>,
    patch: Option<String>,
    file: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    add_preset_inner(
        &mut state.lock_inner(),
        name,
        source,
        program,
        plugin,
        patch,
        file,
    )
}

/// The preset-add path without the Tauri wrapper. Shared with tests.
pub fn add_preset_inner(
    inner: &mut crate::state::AppStateInner,
    name: String,
    source: String,
    program: Option<u8>,
    plugin: Option<String>,
    patch: Option<String>,
    file: Option<String>,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    if inner.project.presets.iter().any(|p| p.name == name) {
        return Err(format!("Preset '{}' already exists", name));
    }
    inner.project.presets.push(ast::PresetDef {
        name,
        source: Some(source),
        plugin,
        program,
        patch,
        file,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("preset add", UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

/// Remove a preset definition
#[tauri::command]
pub fn remove_preset(name: String, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.presets.retain(|p| p.name != name);
    // Clear preset ref from any tracks using it
    for track in &mut inner.project.tracks {
        if track.preset.as_deref() == Some(&name) {
            track.preset = None;
        }
    }
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("preset remove", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

/// Update the GM program for an existing SoundFont preset without changing track assignments.
///
/// The resync may open the audio device and prepare a SoundFont bank, so it runs
/// on the blocking pool: the webview thread must keep painting the panel that
/// asked for the change.
#[tauri::command]
pub async fn set_preset_program(
    name: String,
    program: u8,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        set_preset_program_blocking(&state, name, program)
    })
    .await
}

/// The preset-program mutation without the Tauri wrapper. Shared with tests.
pub(crate) fn set_preset_program_blocking(
    state: &AppState,
    name: String,
    program: u8,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let preset = inner
        .project
        .presets
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Preset '{}' not found", name))?;
    if preset.source.as_deref() != Some("soundfont") {
        return Err(format!("Preset '{}' is not a SoundFont preset", name));
    }
    preset.program = Some(program);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("preset program", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

/// Add, update, or remove a preset parameter. Preset params are used for
/// plugin state, profile manifests, CC mappings, drum maps, and render hints.
#[tauri::command]
pub fn set_preset_param(
    name: String,
    param_name: String,
    param_value: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    owned_gestures::apply_single_parameter(&mut inner, name, param_name, param_value)
}

/// Add, update, or remove multiple preset parameters as one undoable mutation.
/// A whole-patch load runs on the blocking pool: it clones the project, resolves
/// every preset inherit and re-serialises the DSL, which must not hold the UI.
#[tauri::command]
pub async fn set_preset_params(
    name: String,
    params: Vec<(String, Option<String>)>,
    replace_synth: Option<bool>,
    effects: Option<Vec<ast::Effect>>,
    patch_label: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, String> {
    let replace_synth = replace_synth.unwrap_or(false);
    // Claimed at arrival, not inside the task: the blocking pool may start the
    // newer click first, and the ticket is what remembers the click order.
    let ticket = replace_synth.then(|| app.state::<AppState>().patch_load.begin());
    run_blocking(move || {
        let state = app.state::<AppState>();
        set_preset_params_blocking(
            &state,
            name,
            params,
            replace_synth,
            effects.unwrap_or_default(),
            patch_label,
            ticket,
        )
    })
    .await
}

/// The preset-batch mutation without the Tauri wrapper. Shared with tests.
pub(crate) fn set_preset_params_blocking(
    state: &AppState,
    name: String,
    params: Vec<(String, Option<String>)>,
    replace_synth: bool,
    effects: Vec<ast::Effect>,
    patch_label: Option<String>,
    ticket: Option<u64>,
) -> Result<ProjectSnapshot, String> {
    owned_gestures::apply_parameter_batch(
        state,
        owned_gestures::PresetParameterBatch {
            name,
            params,
            replace_synth,
            effects,
            patch_label,
            ticket,
        },
        None,
    )
}

/// Replace the preset's parameters with a seeded, safety-bounded variation.
/// The patch label records the seed so a variation can be reproduced from the CLI.
#[tauri::command]
pub fn randomize_preset(
    name: String,
    seed: u64,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    owned_gestures::apply_randomize_preset(&mut inner, &name, seed)
}

/// Detach the resolved patch and swap in a seeded variation. Shared with tests.
pub(crate) fn randomize_preset_in_project(
    project: &mut ast::Project,
    name: &str,
    seed: u64,
) -> Result<(), String> {
    let resolved = oc_dsl::preset::resolve(project)?;
    let source = resolved
        .presets
        .iter()
        .find(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    if source.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
        return Err("Randomisation requires an OpenSynth preset".into());
    }
    let mut replacement = source.clone();
    replacement.params = oc_synth::synth::random::randomize_params(&source.params, seed);
    replacement.extends = None;
    replacement.patch = Some(format!("Random {seed}"));
    let index = project
        .presets
        .iter()
        .position(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    project.presets[index] = replacement;
    Ok(())
}

/// What a macro learn did to the preset's matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroLearnStatus {
    /// A free slot now holds `macro → control destination`.
    Bound,
    /// The existing `macro → control destination` route was cleared.
    Unbound,
    /// The control has no matrix destination; nothing was written.
    Unsupported,
    /// All 64 slots are in use; nothing was written.
    Full,
}

/// One learn outcome plus the ids the editor needs to name the route.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroLearnResult {
    pub status: MacroLearnStatus,
    /// 1-based matrix slot that changed; absent when nothing was written.
    pub slot: Option<usize>,
    /// Numeric `mod.N.destination` the control drives, when it has one.
    pub destination: Option<usize>,
    /// Numeric `mod.N.source` written on a bind.
    pub source: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Result of a macro learn edit.
pub struct MacroLearnResponse {
    pub snapshot: ProjectSnapshot,
    pub result: MacroLearnResult,
}

/// Learn (or unlearn) a macro route from a control click, as one undo step.
#[tauri::command]
pub fn synth_macro_learn(
    name: String,
    macro_index: usize,
    control: String,
    state: State<AppState>,
) -> Result<MacroLearnResponse, String> {
    let mut inner = state.lock_inner();
    owned_gestures::apply_macro_learn(&mut inner, &name, macro_index, &control)
}

/// Toggle one macro route for `control`: an existing route from the same macro
/// to the control's destination is cleared, otherwise the first free slot gets
/// `source = macro`, `destination = control default`, `amount = 0.5`.
///
/// A control without a default destination is reported as unsupported; no
/// route is written and no destination is guessed.
pub(crate) fn macro_learn_inner(
    project: &mut ast::Project,
    name: &str,
    macro_index: usize,
    control: &str,
    root: Option<&std::path::Path>,
) -> Result<MacroLearnResult, String> {
    if !(1..=8).contains(&macro_index) {
        return Err(format!("Macro {macro_index} out of range (1..=8)"));
    }
    let Some(destination) = oc_engine::dsp::schema::synth::learn_destination(control) else {
        return Ok(MacroLearnResult {
            status: MacroLearnStatus::Unsupported,
            slot: None,
            destination: None,
            source: None,
        });
    };
    let source = oc_synth::synth::macro_mod_source(macro_index - 1)
        .ok_or_else(|| format!("Macro {macro_index} out of range (1..=8)"))?;
    let source_id = usize::from(oc_synth::synth::mod_source_value(source));
    let destination_route = oc_synth::synth::mod_destination_from_value(destination as f64);

    // Resolve inherited routes first: an inherited route is learned/unlearned
    // the same way an explicit one is.
    let resolved = oc_dsl::preset::resolve(project)?;
    let preset = resolved
        .presets
        .iter()
        .find(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    let patch = crate::live_synth::patch(&preset.params, root)?;

    let mut cleared = Vec::new();
    for (index, route) in patch.mod_routes.iter().enumerate() {
        if route.source == source
            && route.destination == destination_route
            && route.amount.abs() >= 0.0001
        {
            cleared.push(index + 1);
        }
    }
    if !cleared.is_empty() {
        let params: Vec<(String, Option<String>)> = cleared
            .iter()
            .map(|slot| (format!("mod.{slot}.amount"), Some("0".to_string())))
            .collect();
        apply_preset_params(project, name, params)?;
        return Ok(MacroLearnResult {
            status: MacroLearnStatus::Unbound,
            slot: cleared.first().copied(),
            destination: Some(destination),
            source: Some(source_id),
        });
    }

    let mut slot = None;
    for index in 0..MAX_MOD_ROUTES {
        let free = patch
            .mod_routes
            .get(index)
            .is_none_or(|route| route.amount.abs() < 0.0001);
        if free {
            slot = Some(index + 1);
            break;
        }
    }
    let Some(slot) = slot else {
        return Ok(MacroLearnResult {
            status: MacroLearnStatus::Full,
            slot: None,
            destination: Some(destination),
            source: Some(source_id),
        });
    };
    apply_preset_params(
        project,
        name,
        vec![
            (format!("mod.{slot}.source"), Some(source_id.to_string())),
            (
                format!("mod.{slot}.destination"),
                Some(destination.to_string()),
            ),
            (format!("mod.{slot}.amount"), Some("0.5".to_string())),
        ],
    )?;
    Ok(MacroLearnResult {
        status: MacroLearnStatus::Bound,
        slot: Some(slot),
        destination: Some(destination),
        source: Some(source_id),
    })
}

/// Replace a preset's whole patch in one undoable step: parameters, effect
/// chain, and the provenance label. Replacing (not merging) is what makes
/// load A → B → A produce the same parameter set every time.
pub(crate) fn load_synth_patch_into_project(
    project: &mut ast::Project,
    name: &str,
    patch_label: Option<&str>,
    params: Vec<(String, Option<String>)>,
    effects: Vec<ast::Effect>,
) -> Result<(), String> {
    replace_synth_patch(project, name, params)?;
    let label = patch_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let preset = project
        .presets
        .iter_mut()
        .find(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    preset.effects = effects;
    preset.patch = label;
    Ok(())
}

pub(crate) fn replace_synth_patch(
    project: &mut ast::Project,
    name: &str,
    params: Vec<(String, Option<String>)>,
) -> Result<(), String> {
    let unknown = unknown_synth_params(&params);
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown OpenSynth parameter(s): {}",
            unknown.join(", ")
        ));
    }
    let index = project
        .presets
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    let resolved = oc_dsl::preset::resolve(project)?;
    let mut replacement = resolved
        .presets
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    if replacement.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
        return Err("Factory patch replacement requires an OpenSynth preset".into());
    }
    // Materialize inherited effects before detaching; synth parameters are replaced,
    // not merged. Parent presets and track routing remain untouched.
    replacement.extends = None;
    replacement.params.clear();
    replacement.file = None;
    replacement.patch = None;
    replacement.program = None;
    let mut candidate = project.clone();
    candidate.presets[index] = replacement;
    apply_preset_params(&mut candidate, name, params)?;
    *project = candidate;
    Ok(())
}

/// Reject a factory batch naming keys the synth would silently ignore.
pub(crate) fn unknown_synth_params(params: &[(String, Option<String>)]) -> Vec<String> {
    let mut probe = oc_synth::synth::SynthPatch::default();
    let mut unknown = std::collections::BTreeSet::new();
    for (key, _) in params {
        let file_key = matches!(
            key.as_str(),
            "wavetable.file" | "open_synth_state_file" | "serum_file" | "serum_root"
        );
        if !file_key && !oc_synth::synth::is_array_param(key) && !probe.set_param(key, 0.0) {
            unknown.insert(key.clone());
        }
    }
    unknown.into_iter().collect()
}

pub(crate) fn apply_preset_params(
    project: &mut ast::Project,
    name: &str,
    params: Vec<(String, Option<String>)>,
) -> Result<(), String> {
    let mut parsed_params = Vec::with_capacity(params.len());
    for (param_name, param_value) in params {
        let key = param_name.trim();
        if key.is_empty() {
            return Err("Preset parameter name cannot be empty".to_string());
        }

        let parsed = match param_value {
            Some(value) if !value.trim().is_empty() => Some(parse_param_value(&value)?),
            _ => None,
        };
        parsed_params.push((key.to_string(), parsed));
    }

    // Follow this preset's inheritance without making an unrelated broken
    // preset prevent repairing the requested one. Validate before any mutation.
    if crate::device_catalog::is_open_synth_preset(project, name) {
        for (key, value) in &parsed_params {
            if let Some(value) = value {
                crate::device_catalog::validate_synth_value(key, value)?;
            }
        }
    }

    let preset = project
        .presets
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Preset '{}' not found", name))?;

    for (key, parsed) in parsed_params {
        match parsed {
            Some(parsed) => {
                if let Some(idx) = preset.params.iter().position(|(k, _)| k == &key) {
                    preset.params[idx] = (key, parsed);
                } else {
                    preset.params.push((key, parsed));
                }
            }
            None => preset.params.retain(|(k, _)| k != &key),
        }
    }
    Ok(())
}

/// Prepare the selected patch without claiming the audio device.
#[tauri::command]
pub async fn warm_open_synth_keyboard(
    preset_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let preset = {
        let inner = state.lock_inner();
        oc_dsl::preset::resolve_preset(&inner.project.presets, &preset_name)?
    };
    crate::live_synth::warm_keyboard_trim(preset).await;
    Ok(())
}

/// Play a note on an OpenSynth preset without starting the transport.
#[tauri::command]
pub fn open_synth_note_on(
    preset_name: String,
    note: u8,
    velocity: u8,
    state: State<AppState>,
) -> Result<Option<crate::audio::TransportSnapshot>, String> {
    let mut inner = state.lock_inner();
    inner
        .audio
        .refuse_during_take("starting a synth audition")?;
    let preset = oc_dsl::preset::resolve_preset(&inner.project.presets, &preset_name)?;
    if preset.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
        // A hosted VST3 preset is audible through the instance its editor owns.
        if preset.source.as_deref() == Some("vst3") {
            let keepalive = inner
                .vst3_editor_keepalives
                .get(&preset_name)
                .ok_or_else(|| "Open the plugin editor to audition this preset".to_string())?;
            if !keepalive.note_on(note, velocity) {
                return Err("Plugin preview queue is full".into());
            }
            return Ok(None);
        }
        return Err("Selected preset is not OpenSynth".into());
    }
    let root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let mpe = oc_engine::mpe::patch_params(&inner.project);
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let tempo = inner.project.properties.tempo;
    let opened = inner.audio.live_synth_note_on(
        &preset_name,
        || {
            let mut patch = crate::live_synth::prepare_keyboard_patch(&preset, root.as_deref())?;
            patch.patch.apply_params(Some(&mpe));
            Ok(patch)
        },
        tempo,
        note,
        velocity,
    )?;
    if opened {
        state.audition.cancel_all();
        Ok(Some(inner.audio.transport_snapshot(&clock)?))
    } else {
        Ok(None)
    }
}

/// Release a note on an open synth.
#[tauri::command]
pub fn open_synth_note_off(
    preset_name: String,
    note: u8,
    state: State<AppState>,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    if let Some(keepalive) = inner.vst3_editor_keepalives.get(&preset_name) {
        keepalive.note_off(note);
        return Ok(());
    }
    inner.audio.live_synth_note_off(&preset_name, note)
}

/// Prepare the pad off the app lock, then publish only the latest strike.
#[tauri::command]
pub async fn preview_drum_pad(
    preset_name: String,
    note: u8,
    velocity: Option<u8>,
    start_paused: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    super::transport::with_native_start(app, move |state| {
        preview_drum_pad_inner(
            preset_name,
            note,
            velocity,
            start_paused.unwrap_or(false),
            state,
        )
    })
    .await
}

fn preview_drum_pad_inner(
    preset_name: String,
    note: u8,
    velocity: Option<u8>,
    start_paused: bool,
    state: &AppState,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let started_ns = crate::first_block::now_ns();
    let (generation, params, root, io, spec) = {
        let mut inner = state.lock_inner();
        let preset = inner
            .project
            .presets
            .iter()
            .find(|preset| preset.name == preset_name)
            .ok_or_else(|| format!("Preset '{preset_name}' not found"))?;
        if preset.plugin.as_deref() != Some(oc_synth::drumkit::PLUGIN_ID) {
            return Err(format!("Preset '{preset_name}' is not a drum kit"));
        }
        let params = preset.params.clone();
        let root = inner
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        inner.audio.replace_private_audition(start_paused)?;
        let generation = state.audition.begin(&preset_name);
        let io = inner.audio.audio_io.clone();
        let spec = io.spec();
        (generation, params, root, io, spec)
    };
    let result: Result<(), String> = (|| {
        if !state.audition.is_current(generation) {
            return Ok(());
        }
        #[cfg(test)]
        let simulated = state.audio_simulated();
        #[cfg(not(test))]
        let simulated = false;
        let stream = crate::audio::AudioState::open_drum_pad_preview(
            &io,
            &spec,
            simulated,
            started_ns,
            crate::audio::DrumPadPreview {
                params: &params,
                root: root.as_deref(),
                note,
                velocity: velocity.unwrap_or(110),
            },
        )?;
        let mut inner = state.lock_inner();
        if state.audition.is_current(generation) {
            inner.audio.install_audition_render(stream)?;
            state.audition.complete(generation, started);
        }
        Ok(())
    })();
    if let Err(error) = &result {
        state.audition.fail(generation, error.clone());
    }
    result
}

/// One imported external preset: the whole patch as project params plus the
/// provenance the header shows.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExternalPresetImport {
    /// File name, shown as the preset's provenance label.
    pub label: String,
    /// Name and author declared inside the file, for the status line.
    pub name: String,
    pub author: String,
    /// Whole-patch batch: `serum_file` plus every numeric value the file
    /// declares. Keys absent here are reset by the load.
    pub params: Vec<(String, Option<String>)>,
    /// Mapping notes for values the engine could not express.
    pub warnings: Vec<String>,
}

/// External preset formats the importer crate reads.
pub(crate) const EXTERNAL_PRESET_FORMATS: &str = ".SerumPreset";

/// Parse an external preset file into a whole-patch batch, off the webview
/// thread: a preset file carries its own wavetables and mapping tables.
#[tauri::command]
pub async fn import_external_preset(path: String) -> Result<ExternalPresetImport, String> {
    run_blocking(move || import_external_preset_at(std::path::Path::new(&path))).await
}

pub(crate) fn import_external_preset_at(
    path: &std::path::Path,
) -> Result<ExternalPresetImport, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("Preset path is not UTF-8: {}", path.display()))?;
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid preset path: {text}"))?
        .to_string();
    let supported = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("SerumPreset"));
    if !supported {
        return Err(format!(
            "Unsupported preset file '{label}'. LOAD imports {EXTERNAL_PRESET_FORMATS} files; \
             factory and user-library presets are in the preset list."
        ));
    }
    let preset = oc_synth::synth::foreign_preset::ForeignPreset::load(path)
        .map_err(|error| format!("{label}: {error}"))?;
    let metadata = preset.metadata_summary();
    let mut diagnostics = oc_synth::synth::foreign_preset::ImportDiagnostics::default();
    let patch = preset.to_synth_patch_diagnosed(
        oc_synth::synth::foreign_preset::infer_content_root(path).as_deref(),
        &mut diagnostics,
    );
    let document = oc_synth::synth::document::PatchDocument {
        version: 1,
        name: if metadata.name.trim().is_empty() {
            label.clone()
        } else {
            metadata.name.clone()
        },
        author: if metadata.author.trim().is_empty() {
            "OpenCompose".into()
        } else {
            metadata.author.clone()
        },
        category: "import".into(),
        use_policy: "project preset".into(),
        patch,
    };
    let mut params: Vec<(String, Option<String>)> = document
        .to_project_params()
        .map_err(|error| format!("{label}: {error}"))?
        .iter()
        .map(|(key, value)| (key.clone(), Some(param_text(value))))
        .collect();
    // Provenance plus the single-file slots the load must not inherit.
    params.push(("serum_file".into(), Some(text.to_string())));
    params.push(("wavetable.file".into(), None));
    params.push(("open_synth_state_file".into(), None));
    Ok(ExternalPresetImport {
        label,
        name: metadata.name,
        author: metadata.author,
        params,
        warnings: diagnostics
            .warnings()
            .iter()
            .map(|warning| {
                format!(
                    "{} (×{}): {}",
                    warning.class, warning.count, warning.message
                )
            })
            .collect(),
    })
}

/// Batch text that `parse_param_value` reads back to the same value.
pub(super) fn param_text(value: &ast::ParamValue) -> String {
    match value {
        ast::ParamValue::Number(number) => number.to_string(),
        ast::ParamValue::Db(number) => format!("{number}dB"),
        ast::ParamValue::Percentage(number) => format!("{number}%"),
        ast::ParamValue::Frequency(number) => format!("{number}Hz"),
        ast::ParamValue::Cents(number) => format!("{number}ct"),
        ast::ParamValue::Time(number) if *number < 1.0 => format!("{}ms", number * 1000.0),
        ast::ParamValue::Time(number) => format!("{number}s"),
        ast::ParamValue::String(text) => text.clone(),
        ast::ParamValue::Bool(flag) => u8::from(*flag).to_string(),
        ast::ParamValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ============================================================================
// Built-in synth parameter automation
// ============================================================================

/// One transaction key per preset parameter; separate from the hosted-plugin
/// namespace so an OpenSynth preset and a plugin preset cannot collide.
fn synth_gesture_key(preset_name: &str, param_key: &str) -> String {
    format!("synth\0{preset_name}\0{param_key}")
}

fn validate_synth_automation_track(
    inner: &crate::state::AppStateInner,
    preset_name: &str,
    track_index: usize,
) -> Result<(), String> {
    let track = inner
        .project
        .tracks
        .get(track_index)
        .ok_or_else(|| format!("Track index {track_index} out of range"))?;
    if track.preset.as_deref() != Some(preset_name) {
        return Err(format!(
            "Track {track_index} does not use preset '{preset_name}'"
        ));
    }
    let preset = inner
        .project
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)
        .ok_or_else(|| format!("Preset '{preset_name}' not found"))?;
    if preset.plugin.as_deref() != Some("builtin.open_synth") {
        return Err(format!("Preset '{preset_name}' is not an OpenSynth preset"));
    }
    Ok(())
}

/// Begin one built-in synth automation transaction. Intermediate points update
/// the preset and the live parameter, but only `end` commits one undo entry.
#[tauri::command]
pub fn begin_synth_param_gesture(
    preset_name: String,
    track_index: usize,
    param_key: String,
    state: State<AppState>,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    begin_synth_param_gesture_inner(&mut inner, &preset_name, track_index, &param_key)
}

/// The editor's lane values use the widened normalized table. Before the
/// first synth lane write on an older document, re-express every existing
/// built-in-synth lane point in the new table and stamp the version. Hosted
/// plugin lanes and non-synth tracks are untouched.
fn upgrade_synth_automation_normalization(project: &mut ast::Project) {
    if project.properties.uses_wide_automation_normalization() {
        return;
    }
    let synth_presets: std::collections::HashSet<String> = project
        .presets
        .iter()
        .filter(|preset| preset.plugin.as_deref() == Some("builtin.open_synth"))
        .map(|preset| preset.name.clone())
        .collect();
    for track in &mut project.tracks {
        let Some(preset) = track.preset.as_deref() else {
            continue;
        };
        if !synth_presets.contains(preset) {
            continue;
        }
        for lane in track.automation_mut() {
            let Some(key) = oc_engine::interpret::decode_plugin_automation_param(&lane.param)
            else {
                continue;
            };
            for point in &mut lane.points {
                // A physical point is read in the patch's own unit and is
                // already table independent.
                if oc_engine::interpret::lane_param_value_is_physical(&point.value) {
                    continue;
                }
                let Some(normalized) =
                    oc_engine::interpret::param_value_to_normalized(&point.value)
                else {
                    continue;
                };
                let Some(recalibrated) =
                    oc_synth::synth::recalibrate_legacy_normalized_param(&key, normalized)
                else {
                    continue;
                };
                point.value = ast::ParamValue::Number(recalibrated);
            }
        }
    }
    project.properties.automation_normalization = Some(ast::WIDE_AUTOMATION_NORMALIZATION);
}

pub(crate) fn begin_synth_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_key: &str,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    validate_synth_automation_track(inner, preset_name, track_index)?;
    let param_key = param_key.trim();
    if param_key.is_empty() {
        return Err("Synth parameter name cannot be empty".into());
    }
    let key = synth_gesture_key(preset_name, param_key);
    if inner
        .plugin_automation_gestures
        .get(&key)
        .is_some_and(|session| inner.undo.is_owned_gesture_active(&session.history_owner))
    {
        return Err(format!(
            "Automation gesture already active for '{param_key}'"
        ));
    }
    let old_project = inner.project.clone();
    let history_owner = inner
        .undo
        .begin_owned_gesture(UndoScope::Automation)
        .map_err(|error| error.to_string())?;
    let gesture = crate::state::PluginAutomationGesture {
        history_owner,
        old_project,
        track_index,
        lane_param: plugin_automation_lane_name(param_key),
        owner: None,
    };
    inner.plugin_automation_gestures.insert(key, gesture);
    Ok(())
}

/// Append/replace a point in the active gesture at `position`. `normalized` is
/// the lane value; `physical` also updates the preset and the live parameter so
/// a drag is audible while it writes.
#[tauri::command]
pub fn update_synth_param_gesture(
    preset_name: String,
    param_key: String,
    normalized: f64,
    physical: Option<f64>,
    position: ast::BarBeat,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_synth_param_gesture_inner(
        &mut inner,
        &preset_name,
        &param_key,
        normalized,
        physical,
        position,
    )?;
    Ok(snapshot(&inner))
}

pub(crate) fn update_synth_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    param_key: &str,
    normalized: f64,
    physical: Option<f64>,
    position: ast::BarBeat,
) -> Result<(), String> {
    owned_gestures::publish_synth_recording_point(
        inner,
        preset_name,
        param_key,
        owned_gestures::SynthRecordingPoint {
            normalized,
            physical,
            position,
            owner: None,
        },
    )
}

#[tauri::command]
pub fn end_synth_param_gesture(
    preset_name: String,
    param_key: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    end_synth_param_gesture_inner(&mut inner, &preset_name, &param_key)?;
    Ok(snapshot(&inner))
}

pub(crate) fn end_synth_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    param_key: &str,
) -> Result<(), String> {
    owned_gestures::finish_synth_recording(inner, preset_name, param_key, None).map(|_| ())
}

/// Drop the built-in automation lane that targets `param_key`, as one undo entry.
#[tauri::command]
pub fn remove_synth_param_automation(
    track_index: usize,
    param_key: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    remove_synth_param_automation_inner(&mut inner, track_index, &param_key)?;
    Ok(snapshot(&inner))
}

pub(crate) fn remove_synth_param_automation_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    param_key: &str,
) -> Result<(), String> {
    let param_key = param_key.trim();
    if param_key.is_empty() {
        return Err("Synth parameter name cannot be empty".into());
    }
    let lane_param = plugin_automation_lane_name(param_key);
    let track = inner
        .project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track index {track_index} out of range"))?;
    if !track.automation().any(|lane| lane.param == lane_param) {
        return Err(format!("No automation lane for '{param_key}'"));
    }
    let old_project = inner.project.clone();
    let kept: Vec<ast::AutomationLane> = inner.project.tracks[track_index]
        .automation()
        .filter(|lane| lane.param != lane_param)
        .cloned()
        .collect();
    inner.project.tracks[track_index].set_automation_lanes(kept);
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("automation remove", UndoScope::Automation),
    )?;
    Ok(())
}
