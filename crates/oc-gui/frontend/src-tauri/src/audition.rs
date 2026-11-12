//! Preset audition off the webview thread: the worker snapshots under the app
//! lock, renders outside it, and re-takes the lock only to publish playback.

use oc_audio::audio_io::StreamSpec;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tauri::Manager;

use oc_dsl::ast::{ParamValue, PresetDef, Project};

use crate::state::AppState;

/// Audition status before a render starts.
pub const IDLE: &str = "idle";
/// Audition status while a render is loading.
pub const LOADING: &str = "loading";
/// Audition status when the render is playable.
pub const READY: &str = "ready";
/// Audition status after a failed render.
pub const ERROR: &str = "error";

#[derive(Clone, Debug, Serialize)]
/// Public audition state for the panel.
pub struct AuditionSnapshot {
    pub preset: Option<String>,
    pub status: String,
    pub ready_ms: Option<f64>,
    pub paused: bool,
    /// The callback consumed the complete audition, including its output tail.
    pub finished: bool,
    pub position_seconds: f64,
    /// Instances currently held by the audition cache.
    pub instances: usize,
    /// Instances constructed since startup, including discarded overlapped loads.
    pub loads: usize,
    pub error: Option<String>,
    /// Debug: press-to-first-block numbers for the current preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_block: Option<crate::first_block::FirstBlockSnapshot>,
}

#[derive(Default)]
struct Status {
    preset: Option<String>,
    status: String,
    ready_ms: Option<f64>,
    error: Option<String>,
}

/// Coordinates one audition render and its generation.
pub struct AuditionController {
    generation: AtomicU64,
    /// Monotonic instant of the request, for the first-block trace.
    requested_ns: AtomicU64,
    epoch: Arc<AtomicU64>,
    status: Mutex<Status>,
    resolved: Mutex<Option<(u64, Arc<Project>)>>,
}

impl AuditionController {
    /// Create a controller and its shared epoch.
    pub fn new() -> (Self, Arc<AtomicU64>) {
        let epoch = Arc::new(AtomicU64::new(0));
        (
            Self {
                generation: AtomicU64::new(0),
                requested_ns: AtomicU64::new(0),
                epoch: Arc::clone(&epoch),
                status: Mutex::new(Status {
                    status: IDLE.into(),
                    ..Status::default()
                }),
                resolved: Mutex::new(None),
            },
            epoch,
        )
    }

    /// Mark a new request current and return its generation.
    pub fn begin(&self, preset: &str) -> u64 {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.requested_ns
            .store(crate::first_block::now_ns(), Ordering::SeqCst);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *status = Status {
            preset: Some(preset.to_owned()),
            status: LOADING.into(),
            ready_ms: None,
            error: None,
        };
        generation
    }

    /// Monotonic instant of the current request.
    pub(crate) fn requested_ns(&self) -> u64 {
        self.requested_ns.load(Ordering::Acquire)
    }

    /// Whether a worker generation is still current.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// Drop the current request and ignore any worker still preparing it.
    pub fn cancel_all(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::SeqCst);
        *status = Status {
            status: IDLE.into(),
            ..Status::default()
        };
    }

    /// Current audition epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Current audition snapshot.
    pub fn snapshot(&self) -> AuditionSnapshot {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        AuditionSnapshot {
            preset: status.preset.clone(),
            status: status.status.clone(),
            ready_ms: status.ready_ms,
            instances: 0,
            loads: 0,
            error: status.error.clone(),
            paused: false,
            finished: false,
            position_seconds: 0.0,
            first_block: None,
        }
    }

    /// Resolve the project once per revision; the caller runs this off the UI thread.
    pub fn resolve(&self, revision: u64, project: &Project) -> Result<Arc<Project>, String> {
        let mut cached = self.resolved.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_revision, resolved)) = cached.as_ref()
            && *cached_revision == revision
        {
            return Ok(Arc::clone(resolved));
        }
        let resolved = Arc::new(oc_dsl::preset::resolve(project)?);
        *cached = Some((revision, Arc::clone(&resolved)));
        Ok(resolved)
    }

    /// Invalidate prepared projects and prevent a prior project audition committing.
    pub fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.resolved.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn complete(&self, generation: u64, started: Instant) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_current(generation) {
            return;
        }
        if status.status == LOADING {
            status.status = READY.into();
            status.ready_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
    }

    pub(crate) fn fail(&self, generation: u64, error: String) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_current(generation) {
            return;
        }
        status.status = ERROR.into();
        status.error = Some(error);
        status.ready_ms = Some(0.0);
    }

    fn cancel(&self, generation: u64) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_current(generation) {
            return;
        }
        if status.status == LOADING {
            status.status = IDLE.into();
        }
    }
}

enum PreparedAudition {
    /// Builtin synth phrase left to render on a private streaming worker.
    Stream {
        epoch: u64,
        project: Box<Project>,
        spec: StreamSpec,
        soundfont_path: Option<PathBuf>,
    },
}

struct RequestSnapshot {
    project: Project,
    root: Option<PathBuf>,
    revision: u64,
    epoch: u64,
    soundfont_path: Option<PathBuf>,
    spec: StreamSpec,
}

/// Queue one audition on the blocking pool; the invoke returns before it runs.
pub fn start(app: tauri::AppHandle, preset_name: String, start_paused: bool) -> Result<(), String> {
    let generation = {
        let state = app.state::<AppState>();
        let mut inner = state.lock_inner();
        inner.audio.replace_private_audition(start_paused)?;
        state.audition.begin(&preset_name)
    };
    let started = Instant::now();
    drop(tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        run(&state, &preset_name, generation, started);
    }));
    Ok(())
}

/// Queue a factory-patch audition. The candidate rides in a throwaway preset on
/// a project copy, so the open project and the live synth are never touched.
pub fn start_factory(
    app: tauri::AppHandle,
    params: Vec<(String, ParamValue)>,
    effects: Vec<oc_dsl::ast::Effect>,
    label: Option<String>,
    start_paused: bool,
) -> Result<(), String> {
    let label = label.unwrap_or_else(|| "Factory patch".into());
    let generation = {
        let state = app.state::<AppState>();
        let mut inner = state.lock_inner();
        inner.audio.replace_private_audition(start_paused)?;
        state.audition.begin(&label)
    };
    let started = Instant::now();
    drop(tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let result = prepare_factory(&state, &params, &effects, generation);
        finish(&state, result, generation, started);
    }));
    Ok(())
}

pub(crate) fn run(state: &AppState, preset_name: &str, generation: u64, started: Instant) {
    let result = prepare(state, preset_name, generation);
    finish(state, result, generation, started);
}

#[cfg(test)]
pub(crate) fn run_factory(
    state: &AppState,
    params: &[(String, ParamValue)],
    effects: &[oc_dsl::ast::Effect],
    generation: u64,
    started: Instant,
) {
    let result = prepare_factory(state, params, effects, generation);
    finish(state, result, generation, started);
}

fn finish(
    state: &AppState,
    result: Result<Option<PreparedAudition>, String>,
    generation: u64,
    started: Instant,
) {
    match result {
        Ok(Some(prepared)) => match commit(state, prepared, generation) {
            Ok(()) => state.audition.complete(generation, started),
            Err(error) => state.audition.fail(generation, error),
        },
        Ok(None) => state.audition.cancel(generation),
        Err(error) => state.audition.fail(generation, error),
    }
}

/// The candidate is the whole patch, so it needs no project preset to inherit.
pub(crate) fn factory_preset(
    name: &str,
    params: &[(String, ParamValue)],
    effects: &[oc_dsl::ast::Effect],
) -> PresetDef {
    PresetDef {
        name: name.to_owned(),
        source: Some("builtin".into()),
        plugin: Some(oc_synth::synth::PLUGIN_ID.into()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: params.to_vec(),
        effects: effects.to_vec(),
        extends: None,
    }
}

pub(crate) fn unique_factory_preset_name(project: &Project, base: &str) -> String {
    if project.presets.iter().all(|preset| preset.name != base) {
        return base.to_owned();
    }
    let mut index = 2;
    loop {
        let name = format!("{base} {index}");
        if project.presets.iter().all(|preset| preset.name != name) {
            return name;
        }
        index += 1;
    }
}

/// The project snapshot one rendered candidate needs: the same copy the
/// ordinary audition uses, without touching the open project.
pub(crate) struct CandidateRenderContext {
    pub project: Project,
    pub root: Option<PathBuf>,
    /// Read by the test-only whole-candidate renderer.
    #[cfg(test)]
    pub sample_rate: u32,
    pub soundfont_path: Option<PathBuf>,
    pub spec: StreamSpec,
    pub epoch: u64,
}

pub(crate) fn candidate_render_context(state: &AppState) -> CandidateRenderContext {
    let inner = state.lock_inner();
    CandidateRenderContext {
        project: inner.project.clone(),
        root: inner
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        #[cfg(test)]
        sample_rate: inner.audio.sample_rate,
        soundfont_path: inner.audio.soundfont_path.clone(),
        spec: inner.audio.audio_io.spec(),
        epoch: state.audition.epoch(),
    }
}

/// Render one whole candidate through the same figure and builtin path as a
/// preset preview. `name` must not already be a preset in `context.project`.
#[cfg(test)]
pub(crate) fn render_candidate(
    context: &CandidateRenderContext,
    name: &str,
    params: &[(String, ParamValue)],
    effects: &[oc_dsl::ast::Effect],
) -> Result<Vec<f32>, String> {
    let mut project = context.project.clone();
    project.presets.push(factory_preset(name, params, effects));
    let project = crate::audio::prepare_preset_audition(&project, name, context.root.as_deref())?;
    crate::audio::AudioState::render_prepared_audition(
        &project,
        context.sample_rate,
        context.soundfont_path.as_deref(),
        context.root.as_deref(),
    )
}

/// Stream the shared figure with the candidate effects on a private project.
/// The synth's master ceiling bounds the figure, so the preview starts without
/// rendering it first; offline comparison keeps its own loudness matching.
fn prepare_factory(
    state: &AppState,
    params: &[(String, ParamValue)],
    effects: &[oc_dsl::ast::Effect],
    generation: u64,
) -> Result<Option<PreparedAudition>, String> {
    #[cfg(test)]
    let phase = Instant::now();
    let context = candidate_render_context(state);
    let name = unique_factory_preset_name(&context.project, "Factory audition");
    #[cfg(test)]
    crate::profile_phase("audition_snapshot", phase);
    if !state.audition.is_current(generation) || state.audition.epoch() != context.epoch {
        return Ok(None);
    }
    let mut project = context.project.clone();
    project.presets.push(factory_preset(&name, params, effects));
    let project = crate::audio::prepare_preset_audition(&project, &name, context.root.as_deref())?;
    if !state.audition.is_current(generation) || state.audition.epoch() != context.epoch {
        return Ok(None);
    }
    Ok(Some(PreparedAudition::Stream {
        epoch: context.epoch,
        project: Box::new(project),
        spec: context.spec,
        soundfont_path: context.soundfont_path,
    }))
}

/// Integrated loudness every browser audition is matched to: the median of the
/// 62 factory patches rendered through this figure (-17.2 LUFS over a 14.9 dB
/// spread). See docs/factory-presets.md.
#[cfg(test)]
pub const AUDITION_REFERENCE_LUFS: f64 = -17.2;
/// Gain limits keep a very quiet patch audible and a hot one from clipping.
#[cfg(test)]
const AUDITION_MIN_GAIN_DB: f64 = -18.0;
#[cfg(test)]
const AUDITION_MAX_GAIN_DB: f64 = 12.0;
/// Peak ceiling left after a positive match (-0.5 dBFS).
#[cfg(test)]
const AUDITION_PEAK_CEILING: f64 = 0.944;

/// Gain that lands a measured audition on the bank reference.
#[cfg(test)]
pub(crate) fn audition_gain_db(measured_lufs: Option<f64>) -> f64 {
    let Some(lufs) = measured_lufs else {
        return 0.0;
    };
    if !lufs.is_finite() {
        return 0.0;
    }
    (AUDITION_REFERENCE_LUFS - lufs).clamp(AUDITION_MIN_GAIN_DB, AUDITION_MAX_GAIN_DB)
}

/// Peak ceiling the instrument output is normalised to (-6 dBFS). Every stage
/// that can raise the voice path carries its own normalisation and the master
/// ceiling bounds their sum, so a preview plays below this level without being
/// rendered and measured first. See `docs/synth-levels.md`.
#[cfg(test)]
pub(crate) const PREVIEW_PEAK_CEILING_DB: f64 = -6.0;

/// Highest absolute sample in an interleaved buffer.
#[cfg(test)]
fn peak(buffer: &[f32]) -> f64 {
    buffer
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())))
}

/// Test-only guard for the preview contract. The application never renders a
/// preview to check it; the tests render the figure once and assert the synth
/// holds it under [`PREVIEW_PEAK_CEILING_DB`]. A preset effect after the
/// instrument is a user gain and sits outside the instrument's contract.
#[cfg(test)]
pub(crate) fn preview_peak_db(buffer: &[f32]) -> f64 {
    let peak = peak(buffer);
    if peak <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * peak.log10()
    }
}

/// Scale a rendered candidate to the reference loudness, then cap the gain so
/// the matched buffer keeps 0.5 dB of peak headroom. Loudness gating is
/// level-dependent, so a second pass trims the residual error.
#[cfg(test)]
fn apply_audition_level(buffer: &mut [f32], sample_rate: u32) -> f64 {
    if buffer.is_empty() {
        return 0.0;
    }
    let mut applied_db = 0.0;
    for _ in 0..2 {
        let measured = oc_engine::analysis::measure_stereo_interleaved(buffer, sample_rate);
        let mut gain_db = audition_gain_db(measured.integrated_lufs);
        let peak = buffer
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())));
        if peak > 0.0 {
            let headroom_db = 20.0 * (AUDITION_PEAK_CEILING / peak).log10();
            gain_db = gain_db.min(headroom_db);
        }
        if gain_db.abs() < 0.05 {
            break;
        }
        let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
        for sample in buffer.iter_mut() {
            *sample *= gain;
        }
        applied_db += gain_db;
    }
    applied_db
}

fn snapshot(state: &AppState, _preset_name: &str) -> RequestSnapshot {
    let inner = state.lock_inner();
    let project = inner.project.clone();
    RequestSnapshot {
        project,
        root: inner
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        revision: inner.revision,
        epoch: state.audition.epoch(),
        soundfont_path: inner.audio.soundfont_path.clone(),
        spec: inner.audio.audio_io.spec(),
    }
}

fn prepare(
    state: &AppState,
    preset_name: &str,
    generation: u64,
) -> Result<Option<PreparedAudition>, String> {
    #[cfg(test)]
    let phase = Instant::now();
    let request = snapshot(state, preset_name);
    #[cfg(test)]
    crate::profile_phase("audition_snapshot", phase);
    if !state.audition.is_current(generation) || state.audition.epoch() != request.epoch {
        return Ok(None);
    }
    #[cfg(test)]
    let phase = Instant::now();
    let resolved = state.audition.resolve(request.revision, &request.project)?;
    #[cfg(test)]
    crate::profile_phase("audition_resolve", phase);
    let preset = resolved
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)
        .cloned();
    let preset = preset.ok_or_else(|| format!("Preset '{preset_name}' not found"))?;
    let mut project = request.project.clone();
    // Live CLAP edits belong to the audition snapshot, never to the saved project.
    if preset.source.as_deref() == Some("clap") {
        let inner = state.lock_inner();
        if let Some(instance) = inner.plugin_instances.get(preset_name) {
            let bytes = instance.state_bytes()?;
            if let Some(preset) = project.presets.iter_mut().find(|p| p.name == preset_name) {
                preset.params.retain(|(key, _)| key != "clap_state_hex");
                preset.params.push((
                    "clap_state_hex".into(),
                    ParamValue::String(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
                ));
            }
        }
    }
    let project =
        crate::audio::prepare_preset_audition(&project, preset_name, request.root.as_deref())?;
    if !state.audition.is_current(generation) || state.audition.epoch() != request.epoch {
        return Ok(None);
    }
    Ok(Some(PreparedAudition::Stream {
        epoch: request.epoch,
        project: Box::new(project),
        spec: request.spec,
        soundfont_path: request.soundfont_path,
    }))
}

fn commit(state: &AppState, prepared: PreparedAudition, generation: u64) -> Result<(), String> {
    if !state.audition.is_current(generation) {
        return Ok(());
    }
    match prepared {
        PreparedAudition::Stream {
            epoch,
            project,
            spec,
            soundfont_path,
        } => {
            if state.audition.epoch() != epoch {
                return Ok(());
            }
            #[cfg(test)]
            let simulated = state.audio_simulated();
            #[cfg(not(test))]
            let simulated = false;
            #[cfg(test)]
            let phase = Instant::now();
            let io = state.lock_inner().audio.audio_io.clone();
            let prepared = crate::audio::AudioState::open_audition_render(
                &io,
                &spec,
                soundfont_path.unwrap_or_default(),
                *project,
                simulated,
                state.audition.requested_ns(),
                crate::first_block::StartPath::PresetAudition,
            );
            #[cfg(test)]
            crate::profile_phase("audition_device", phase);
            let mut inner = state.lock_inner();
            if !state.audition.is_current(generation) || state.audition.epoch() != epoch {
                return Ok(());
            }
            let prepared = prepared?;
            #[cfg(test)]
            let phase = Instant::now();
            let result = inner.audio.install_audition_render(prepared);
            #[cfg(test)]
            crate::profile_phase("audition_install", phase);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSPORT_PROJECT: &str = r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
@preset synth
  source: builtin
  plugin: "builtin.open_synth"
@track lead | preset: synth, instrument: synth/lead_square
  @phrase note [1:1 - 3:1]
    C4 w mf
"#;

    fn simulated_state(project: &Project) -> (AppState, oc_dsl::timing::ProjectClock) {
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = project.clone();
            inner.audio.enable_simulated_output();
            inner.audio.sample_rate = 8000;
        }
        (state, clock)
    }

    #[test]
    fn every_preset_adapter_prepares_a_stream_without_prerendering() {
        for (source, plugin) in [
            ("soundfont", ""),
            ("sample", "sfizz"),
            ("vst3", "missing.test"),
            ("clap", "missing.test"),
            ("vst2", "missing.test"),
            ("au", "missing.test"),
            ("lv2", "missing.test"),
            ("builtin", "builtin.sampler"),
            ("builtin", "builtin.drumkit"),
        ] {
            let mut project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
            let preset = project
                .presets
                .iter_mut()
                .find(|p| p.name == "tone")
                .unwrap();
            preset.source = Some(source.into());
            preset.plugin = (!plugin.is_empty()).then(|| plugin.into());
            let (state, _) = simulated_state(&project);
            let generation = state.audition.begin("tone");
            assert!(
                matches!(
                    prepare(&state, "tone", generation).unwrap(),
                    Some(PreparedAudition::Stream { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn private_stream_pause_resume_retains_cursor_and_pending_pause() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, clock) = simulated_state(&project);
        let generation = state.audition.begin("tone");
        state.lock_inner().audio.pause_private_audition();
        run(&state, "tone", generation, Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(30));
        {
            let inner = state.lock_inner();
            assert!(inner.audio.audition_paused);
            assert!(!inner.audio.audition_active());
            assert_eq!(inner.audio.heard_audition_blocks(), 0);
        }
        state.lock_inner().audio.resume_private_audition().unwrap();
        wait_for_audition_audio(&state);
        state.lock_inner().audio.pause_private_audition();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let playback = state
            .lock_inner()
            .audio
            .audition_render
            .as_ref()
            .unwrap()
            .playback
            .clone();
        let cursor = playback.play_cursor_frames.load(Ordering::Acquire);
        assert!(cursor > 0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(playback.play_cursor_frames.load(Ordering::Acquire), cursor);
        state.lock_inner().audio.resume_private_audition().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while playback.play_cursor_frames.load(Ordering::Acquire) <= cursor {
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut inner = state.lock_inner();
        assert_eq!(
            inner
                .audio
                .transport_snapshot(&clock)
                .unwrap()
                .position_beats,
            0.0
        );
        inner.audio.stop_audition();
        let mut snapshot = state.audition.snapshot();
        inner.audio.fill_audition_snapshot(&mut snapshot);
        assert!(!snapshot.paused);
        assert_eq!(snapshot.position_seconds, 0.0);
    }

    #[test]
    fn factory_preview_prepares_stream_without_full_figure_buffer() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _) = simulated_state(&project);
        let generation = state.audition.begin("factory");
        assert!(matches!(
            prepare_factory(&state, &[], &[], generation).unwrap(),
            Some(PreparedAudition::Stream { .. })
        ));
    }

    #[test]
    fn preview_preserves_the_stopped_cursor() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, clock) = simulated_state(&project);
        {
            let mut inner = state.lock_inner();
            inner.audio.seek(3.0, &clock).unwrap();
        }
        let before = state.lock_inner().audio.transport_snapshot(&clock).unwrap();
        assert_eq!(before.position_beats, 3.0);

        let generation = state.audition.begin("tone");
        run(&state, "tone", generation, Instant::now());

        assert_eq!(state.audition.snapshot().status, READY);
        let mut inner = state.lock_inner();
        let after = inner.audio.transport_snapshot(&clock).unwrap();
        assert_eq!(after.position_beats, before.position_beats);
        assert_eq!(after.position_seconds, before.position_seconds);
        assert_eq!(after.playing, before.playing);
        assert!(inner.audio.audition_render.is_some());
        assert!(inner.audio.audition_playback.is_none());
        drop(inner);
        wait_for_audition_audio(&state);
    }

    #[test]
    fn builtin_synth_preview_streams_instead_of_rendering_its_phrase_first() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, clock) = simulated_state(&project);
        let position = state
            .lock_inner()
            .audio
            .transport_snapshot(&clock)
            .unwrap()
            .position_beats;

        let generation = state.audition.begin("synth");
        run(&state, "synth", generation, Instant::now());

        // The phrase is left to the private streaming worker: no rendered
        // buffer is published and the transport cursor has not moved.
        let mut inner = state.lock_inner();
        assert!(inner.audio.audition_render.is_some());
        assert!(inner.audio.audition_playback.is_none());
        let after = inner.audio.transport_snapshot(&clock).unwrap();
        assert_eq!(after.position_beats, position);
        assert!(!after.playing);
        drop(inner);

        // Audio must reach the device without waiting for the full phrase.
        let started = Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(3) {
            if state.lock_inner().audio.heard_audition_blocks() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            state.lock_inner().audio.heard_audition_blocks() > 0,
            "streamed audition never reached the device"
        );
        assert_eq!(state.audition.snapshot().status, READY);
    }

    #[test]
    fn preview_does_not_stop_a_playing_transport() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, clock) = simulated_state(&project);
        {
            let mut inner = state.lock_inner();
            let mut instances = std::mem::take(&mut inner.plugin_instances);
            let result = inner.audio.play(&project, &mut instances);
            inner.plugin_instances = instances;
            result.unwrap();
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let before = loop {
            let snapshot = state.lock_inner().audio.transport_snapshot(&clock).unwrap();
            if snapshot.playing && snapshot.position_beats > 0.25 {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "transport did not start");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };

        let generation = state.audition.begin("tone");
        run(&state, "tone", generation, Instant::now());

        {
            let mut inner = state.lock_inner();
            let after = inner.audio.transport_snapshot(&clock).unwrap();
            assert!(after.playing, "audition paused the transport");
            assert!(after.position_beats >= before.position_beats);
            assert!(inner.audio.audition_render.is_some());
            inner.audio.reset_project();
        }
    }

    #[test]
    fn stopping_cancels_the_running_audition_figure() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let generation = state.audition.begin("synth");
        run(&state, "synth", generation, Instant::now());
        assert!(state.lock_inner().audio.audition_render.is_some());
        wait_for_audition_audio(&state);

        let mut inner = state.lock_inner();
        inner.audio.stop_audition();
        drop(inner);
        state.audition.cancel_all();

        assert!(!state.lock_inner().audio.audition_active());
        assert!(state.lock_inner().audio.audition_render.is_none());
        let snapshot = state.audition.snapshot();
        assert_eq!(snapshot.status, IDLE);
        assert_eq!(snapshot.preset, None);
    }

    #[test]
    fn a_second_preview_stops_the_previous_figure() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let generation = state.audition.begin("synth");
        run(&state, "synth", generation, Instant::now());
        assert!(state.lock_inner().audio.audition_render.is_some());
        wait_for_audition_audio(&state);

        let previous = state
            .lock_inner()
            .audio
            .audition_render
            .as_ref()
            .unwrap()
            .playback
            .clone();
        let generation = state.audition.begin("tone");
        run(&state, "tone", generation, Instant::now());

        let inner = state.lock_inner();
        assert!(
            !previous.active.load(Ordering::Acquire),
            "the replaced figure must stop streaming"
        );
        assert!(inner.audio.audition_render.is_some());
        assert_eq!(state.audition.snapshot().status, READY);
    }

    fn wait_for_audition_audio(state: &AppState) {
        let started = Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(3) {
            if state.lock_inner().audio.heard_audition_blocks() > 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("streamed audition never reached the device");
    }

    fn factory_batch(entries: &[(&str, &str)]) -> Vec<(String, ParamValue)> {
        entries
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_owned(),
                    ParamValue::Number(value.parse().expect("numeric candidate value")),
                )
            })
            .collect()
    }

    fn bright_candidate() -> Vec<(String, ParamValue)> {
        factory_batch(&[
            ("osc.a.waveform", "1"),
            ("osc.a.level", "0.7"),
            ("filter.cutoff", "14000"),
            ("filter.resonance", "0.1"),
            ("filter.env.amount", "0"),
            ("amp.attack", "0.005"),
            ("amp.decay", "0.2"),
            ("amp.sustain", "0.8"),
            ("amp.release", "0.2"),
            ("output.gain", "0.22"),
        ])
    }

    fn dark_candidate() -> Vec<(String, ParamValue)> {
        factory_batch(&[
            ("osc.a.waveform", "2"),
            ("osc.a.level", "0.7"),
            ("filter.cutoff", "240"),
            ("filter.resonance", "0.5"),
            ("filter.env.amount", "0"),
            ("amp.attack", "0.005"),
            ("amp.decay", "0.2"),
            ("amp.sustain", "0.8"),
            ("amp.release", "0.2"),
            ("output.gain", "0.22"),
        ])
    }

    fn mono(buffer: &[f32]) -> Vec<f32> {
        buffer
            .chunks(2)
            .map(|pair| (pair[0] + pair[1]) * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let energy = samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>();
        (energy / samples.len() as f64).sqrt() as f32
    }

    /// Brute-force DFT centroid over the steady part of the first note.
    fn spectral_centroid(samples: &[f32], sample_rate: f32) -> f32 {
        let onset = samples
            .iter()
            .position(|sample| sample.abs() > 1.0e-3)
            .unwrap_or(0);
        let start = (onset + 2_048).min(samples.len());
        let slice = &samples[start..samples.len().min(start + 512)];
        if slice.len() < 32 {
            return 0.0;
        }
        let n = slice.len();
        let mut weighted = 0.0f64;
        let mut total = 0.0f64;
        for k in 1..=n / 2 {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (index, sample) in slice.iter().enumerate() {
                let angle = -2.0 * std::f64::consts::PI * k as f64 * index as f64 / n as f64;
                re += f64::from(*sample) * angle.cos();
                im += f64::from(*sample) * angle.sin();
            }
            let magnitude = (re * re + im * im).sqrt();
            weighted += magnitude * k as f64 * f64::from(sample_rate) / n as f64;
            total += magnitude;
        }
        if total > 0.0 {
            (weighted / total) as f32
        } else {
            0.0
        }
    }

    fn render_factory_candidate(
        state: &AppState,
        params: &[(String, ParamValue)],
        effects: &[oc_dsl::ast::Effect],
        label: &str,
    ) -> Vec<f32> {
        let context = candidate_render_context(state);
        let name = unique_factory_preset_name(&context.project, label);
        let mut buffer = render_candidate(&context, &name, params, effects).unwrap();
        apply_audition_level(&mut buffer, context.sample_rate);
        buffer
    }

    fn audition_lufs(buffer: &[f32]) -> f64 {
        oc_engine::analysis::measure_stereo_interleaved(buffer, 48_000)
            .integrated_lufs
            .expect("candidate has a measurable loudness")
    }

    #[test]
    fn factory_candidates_sound_different_without_touching_the_project() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let before = {
            let inner = state.lock_inner();
            (
                inner.project.clone(),
                inner.revision,
                inner.dirty,
                inner.dsl_source.clone(),
                inner.undo.undo.len(),
                inner.undo.redo.len(),
            )
        };

        let bright = bright_candidate();
        let generation = state.audition.begin("Bright candidate");
        run_factory(&state, &bright, &[], generation, Instant::now());
        assert_eq!(state.audition.snapshot().status, READY);
        wait_for_audition_audio(&state);
        {
            state.audition_pause_intent.store(false, Ordering::Release);
            state.lock_inner().audio.stop_audition();
            state.audition.cancel_all();
        }

        let after = {
            let inner = state.lock_inner();
            (
                inner.project.clone(),
                inner.revision,
                inner.dirty,
                inner.dsl_source.clone(),
                inner.undo.undo.len(),
                inner.undo.redo.len(),
            )
        };
        assert_eq!(after.0, before.0, "audition mutated the project");
        assert_eq!(after.1, before.1, "audition bumped the revision");
        assert_eq!(after.2, before.2, "audition changed the dirty flag");
        assert_eq!(after.3, before.3, "audition rewrote the DSL source");
        assert_eq!(after.4, before.4, "audition pushed an undo entry");
        assert_eq!(after.5, before.5, "audition pushed a redo entry");
        assert!(
            !state.lock_inner().audio.live_synth_active(),
            "audition started the live synth"
        );

        let dark = dark_candidate();
        let bright_buffer = render_factory_candidate(&state, &bright, &[], "Bright candidate");
        let dark_buffer = render_factory_candidate(&state, &dark, &[], "Dark candidate");
        let bright_mono = mono(&bright_buffer);
        let dark_mono = mono(&dark_buffer);
        let bright_rms = rms(&bright_mono);
        let dark_rms = rms(&dark_mono);
        assert!(
            bright_rms > 0.002,
            "bright candidate is silent: {bright_rms}"
        );
        assert!(dark_rms > 0.002, "dark candidate is silent: {dark_rms}");
        let bright_centroid = spectral_centroid(&bright_mono, 48_000.0);
        let dark_centroid = spectral_centroid(&dark_mono, 48_000.0);
        assert!(
            bright_centroid - dark_centroid > 500.0,
            "candidates sound alike: bright {bright_centroid} Hz (rms {bright_rms}) vs dark {dark_centroid} Hz (rms {dark_rms})"
        );
        // Level is deliberately not a discriminator: the resonance
        // normalisation brings a high-Q patch toward the level of a flat one.
        // The timbre and the waveform still differ.
        assert_ne!(bright_buffer, dark_buffer);
    }

    fn gain_candidate(gain: &str) -> Vec<(String, ParamValue)> {
        factory_batch(&[
            ("osc.a.waveform", "1"),
            ("osc.a.level", "0.7"),
            ("filter.cutoff", "14000"),
            ("filter.resonance", "0.1"),
            ("filter.env.amount", "0"),
            ("amp.attack", "0.005"),
            ("amp.decay", "0.2"),
            ("amp.sustain", "0.8"),
            ("amp.release", "0.2"),
            ("output.gain", gain),
        ])
    }

    #[test]
    fn offline_candidates_are_gain_matched_to_the_reference() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let loud = render_factory_candidate(&state, &gain_candidate("0.32"), &[], "Loud");
        let quiet = render_factory_candidate(&state, &gain_candidate("0.16"), &[], "Quiet");
        let loud_lufs = audition_lufs(&loud);
        let quiet_lufs = audition_lufs(&quiet);
        assert!(
            (loud_lufs - quiet_lufs).abs() < 0.35,
            "matching left the pair {:.2} LU apart ({loud_lufs:.2} vs {quiet_lufs:.2})",
            (loud_lufs - quiet_lufs).abs()
        );
        for (label, lufs) in [("loud", loud_lufs), ("quiet", quiet_lufs)] {
            assert!(
                (lufs - AUDITION_REFERENCE_LUFS).abs() < 1.5,
                "{label} audition sits at {lufs:.2} LUFS, away from the reference"
            );
        }
    }

    #[test]
    fn a_preset_effect_chain_reaches_the_browser_audition() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let darken = oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Eq,
            params: vec![
                ("high_freq".into(), ParamValue::Frequency(1_200.0)),
                ("high_gain".into(), ParamValue::Db(-30.0)),
            ],
            bypass: false,
            sidechain_source: None,
        };
        let plain = render_factory_candidate(&state, &bright_candidate(), &[], "Plain");
        let filtered = render_factory_candidate(&state, &bright_candidate(), &[darken], "Filtered");
        let plain_centroid = spectral_centroid(&mono(&plain), 48_000.0);
        let filtered_centroid = spectral_centroid(&mono(&filtered), 48_000.0);
        assert!(
            plain_centroid > filtered_centroid + 200.0,
            "the preset effect chain did not reach the audition: {plain_centroid} Hz vs {filtered_centroid} Hz"
        );
    }

    #[test]
    fn level_matching_is_bounded_and_skips_silence() {
        assert_eq!(audition_gain_db(None), 0.0);
        assert_eq!(audition_gain_db(Some(f64::NAN)), 0.0);
        assert_eq!(audition_gain_db(Some(AUDITION_REFERENCE_LUFS - 2.0)), 2.0);
        assert_eq!(
            audition_gain_db(Some(AUDITION_REFERENCE_LUFS + 40.0)),
            AUDITION_MIN_GAIN_DB
        );
        assert_eq!(
            audition_gain_db(Some(AUDITION_REFERENCE_LUFS - 40.0)),
            AUDITION_MAX_GAIN_DB
        );
        let mut silence = vec![0.0f32; 4_800 * 2];
        assert_eq!(apply_audition_level(&mut silence, 48_000), 0.0);
    }

    fn first_audible_block_ms(state: &AppState, started: Instant) -> Option<f64> {
        while started.elapsed() < std::time::Duration::from_secs(3) {
            if state.lock_inner().audio.heard_audition_blocks() > 0 {
                return Some(started.elapsed().as_secs_f64() * 1000.0);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        None
    }

    #[test]
    #[ignore = "measures the factory audition handoff on the simulated device; run alone with --ignored"]
    fn review_factory_audition_latency() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        {
            state.lock_inner().audio.sample_rate = 48_000;
        }

        // Warm the process-wide synth tables so both paths measure the path itself.
        let started = Instant::now();
        let generation = state.audition.begin("synth");
        run(&state, "synth", generation, started);
        first_audible_block_ms(&state, started);
        state.audition.cancel_all();
        state.audition_pause_intent.store(false, Ordering::Release);
        state.lock_inner().audio.stop_audition();

        let started = Instant::now();
        let generation = state.audition.begin("Glass Pad");
        run_factory(&state, &bright_candidate(), &[], generation, started);
        let factory_ready_ms = state.audition.snapshot().ready_ms;
        let factory_first_ms = first_audible_block_ms(&state, started);
        assert_eq!(state.audition.snapshot().status, READY);
        assert!(
            factory_first_ms.is_some(),
            "factory audition never reached the device"
        );
        state.audition.cancel_all();
        state.audition_pause_intent.store(false, Ordering::Release);
        state.lock_inner().audio.stop_audition();

        let started = Instant::now();
        let generation = state.audition.begin("synth");
        run(&state, "synth", generation, started);
        let preset_ready_ms = state.audition.snapshot().ready_ms;
        let preset_first_ms = first_audible_block_ms(&state, started);
        state.audition.cancel_all();
        state.audition_pause_intent.store(false, Ordering::Release);
        state.lock_inner().audio.stop_audition();

        eprintln!(
            "FACTORY_AUDITION sample_rate=48000 factory_ready_ms={factory_ready_ms:?} factory_first_block_ms={factory_first_ms:?} preset_ready_ms={preset_ready_ms:?} preset_first_block_ms={preset_first_ms:?}"
        );
        assert!(
            preset_first_ms.is_some(),
            "preset preview never reached the device"
        );
    }

    #[test]
    fn preset_and_factory_auditions_can_start_paused_and_resume_after_install() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _) = simulated_state(&project);
        for factory in [false, true] {
            state
                .lock_inner()
                .audio
                .replace_private_audition(true)
                .unwrap();
            let generation = state.audition.begin("synth");
            let mut snapshot = state.audition.snapshot();
            state
                .lock_inner()
                .audio
                .fill_audition_snapshot(&mut snapshot);
            assert!(snapshot.paused);
            assert_eq!(snapshot.status, LOADING);

            if factory {
                run_factory(&state, &bright_candidate(), &[], generation, Instant::now());
            } else {
                run(&state, "synth", generation, Instant::now());
            }
            assert_eq!(state.audition.snapshot().status, READY);
            {
                let mut inner = state.lock_inner();
                assert!(!inner.audio.audition_active());
                assert_eq!(inner.audio.heard_audition_blocks(), 0);
                state.audition_pause_intent.store(false, Ordering::Release);
                inner.audio.resume_private_audition().unwrap();
            }
            wait_for_audition_audio(&state);
        }
    }

    #[test]
    fn newer_request_supersedes_an_in_flight_audition() {
        let (controller, _epoch) = AuditionController::new();
        let first = controller.begin("a");
        let second = controller.begin("b");
        assert!(!controller.is_current(first));
        assert!(controller.is_current(second));
        controller.complete(first, Instant::now());
        controller.fail(first, "stale failure".into());
        controller.cancel(first);
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.preset.as_deref(), Some("b"));
        assert_eq!(snapshot.status, LOADING);
        controller.complete(second, Instant::now());
        assert_eq!(controller.snapshot().status, READY);
        assert!(controller.snapshot().ready_ms.is_some());
    }

    #[test]
    fn resolved_presets_are_cached_by_revision() {
        let (controller, _epoch) = AuditionController::new();
        let project = oc_dsl::parse(
            "@project\n@preset a\n  source: builtin\n  plugin: \"builtin.test_tone\"\n",
        )
        .unwrap();
        let first = controller.resolve(0, &project).unwrap();
        let second = controller.resolve(0, &project).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let third = controller.resolve(1, &project).unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
    }

    #[test]
    #[ignore = "needs OCP_AUDITION_PROJECT with VST3 presets and the plugin installed; no audio device"]
    fn review_preset_audition_latency() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let path = std::env::var("OCP_AUDITION_PROJECT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp/audition-bench.ocp"));
        let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let plugin_presets: Vec<String> = project
            .presets
            .iter()
            .filter(|preset| preset.source.as_deref() == Some("vst3"))
            .map(|preset| preset.name.clone())
            .collect();
        let state = Arc::new(AppState::new());
        {
            let mut inner = state.lock_inner();
            inner.project = project;
            inner.file_path = Some(path);
            inner.audio.enable_simulated_output();
        }

        let running = Arc::new(AtomicBool::new(true));
        let monitor_state = Arc::clone(&state);
        let monitor_running = Arc::clone(&running);
        let monitor = std::thread::spawn(move || {
            let mut blocked_since: Option<Instant> = None;
            let mut worst = Duration::ZERO;
            while monitor_running.load(Ordering::Relaxed) {
                match monitor_state.inner.try_lock() {
                    Ok(_) => {
                        if let Some(start) = blocked_since.take() {
                            worst = worst.max(start.elapsed());
                        }
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        blocked_since.get_or_insert_with(Instant::now);
                    }
                    Err(std::sync::TryLockError::Poisoned(_)) => {}
                }
                std::thread::sleep(Duration::from_micros(100));
            }
            worst
        });

        let mut worst_click = 0.0f64;
        for name in &plugin_presets {
            let generation = state.audition.begin(name);
            let started = Instant::now();
            run(&state, name, generation, started);
            let snapshot = state.audition.snapshot();
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            worst_click = worst_click.max(elapsed);
            eprintln!(
                "AFTER preset={name} total_ms={elapsed:.1} ready_ms={:?} status={} instances={} loads={}",
                snapshot.ready_ms, snapshot.status, snapshot.instances, snapshot.loads
            );
            assert_eq!(snapshot.status, READY, "preset {name}");
        }

        let repeat = plugin_presets.first().cloned().unwrap_or_default();
        let repeats = [repeat.clone(), repeat];
        for (index, name) in repeats.iter().enumerate() {
            let generation = state.audition.begin(name);
            let started = Instant::now();
            run(&state, name, generation, started);
            let snapshot = state.audition.snapshot();
            eprintln!(
                "REPEAT index={index} preset={name} total_ms={:.1} ready_ms={:?} instances={} loads={}",
                started.elapsed().as_secs_f64() * 1000.0,
                snapshot.ready_ms,
                snapshot.instances,
                snapshot.loads
            );
        }

        let aba: Vec<&str> = [
            plugin_presets.get(2),
            plugin_presets.get(3),
            plugin_presets.get(2),
        ]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
        for name in aba {
            let generation = state.audition.begin(name);
            let started = Instant::now();
            run(&state, name, generation, started);
            let snapshot = state.audition.snapshot();
            eprintln!(
                "ABA preset={name} total_ms={:.1} instances={} loads={}",
                started.elapsed().as_secs_f64() * 1000.0,
                snapshot.instances,
                snapshot.loads
            );
        }

        for name in ["builtin_tone", "builtin_plain"] {
            let generation = state.audition.begin(name);
            let started = Instant::now();
            run(&state, name, generation, started);
            let snapshot = state.audition.snapshot();
            let nonzero = {
                let inner = state.lock_inner();
                inner
                    .audio
                    .playback
                    .as_ref()
                    .map(|playback| playback.buffer.iter().any(|sample| sample.abs() > 1.0e-4))
                    .unwrap_or(false)
            };
            eprintln!(
                "NON_PLUGIN preset={name} total_ms={:.1} status={} nonzero={}",
                started.elapsed().as_secs_f64() * 1000.0,
                snapshot.status,
                nonzero
            );
            assert_eq!(snapshot.status, READY, "preset {name}");
        }

        running.store(false, Ordering::Relaxed);
        let max_blocked = monitor.join().unwrap();
        eprintln!(
            "AFTER_SUMMARY clicks={} worst_click_ms={:.1} max_lock_blocked_ms={:.1} instances={} loads={}",
            plugin_presets.len(),
            worst_click,
            max_blocked.as_secs_f64() * 1000.0,
            state.audition.snapshot().instances,
            state.audition.snapshot().loads
        );
    }

    const SOUNDFONT_PROJECT: &str = r#"@project
  tempo: 120
@preset font
  source: soundfont
  program: 0
@track keys | preset: font, instrument: piano/acoustic_grand
  @phrase p [1:1:0 - 2:1:0]
    C4 w mf
"#;

    /// The SoundFont fixture the engine's own tests render with.
    fn soundfont_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../crates/oc-engine/tests/fixtures/minimal-test.sf2")
    }

    fn streamed_project(prepared: Option<PreparedAudition>) -> Project {
        match prepared.expect("the preview prepared a stream") {
            PreparedAudition::Stream { project, .. } => *project,
        }
    }

    /// One rendered preview figure: its loudest sample in dBFS.
    fn preview_level(project: &Project, sample_rate: u32, soundfont: Option<&Path>) -> f64 {
        let buffer = crate::audio::AudioState::render_prepared_audition(
            project,
            sample_rate,
            soundfont,
            None,
        )
        .expect("the preview figure renders");
        preview_peak_db(&buffer)
    }

    fn sine_buffer(amplitude: f32, seconds: f64, sample_rate: u32) -> Vec<f32> {
        let frames = (seconds * f64::from(sample_rate)) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let phase =
                2.0 * std::f64::consts::PI * 1_000.0 * frame as f64 / f64::from(sample_rate);
            let sample = amplitude * phase.sin() as f32;
            buffer.push(sample);
            buffer.push(sample);
        }
        buffer
    }

    fn gain_effect(factor: f64) -> oc_dsl::ast::Effect {
        oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Gain,
            params: vec![("factor".into(), ParamValue::Number(factor))],
            bypass: false,
            sidechain_source: None,
        }
    }

    #[test]
    fn a_preview_is_not_trimmed_by_a_measurement() {
        // The preview does not render and measure the figure: the prepared
        // audition keeps the level its patch sets.
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
        state.lock_inner().audio.sample_rate = rate;
        let generation = state.audition.begin("Factory audition");
        let factory = streamed_project(
            prepare_factory(&state, &gain_candidate("1.0"), &[], generation).unwrap(),
        );
        assert_eq!(
            factory.tracks[0].mix.volume_db, 0.0,
            "the preview baked a trim into the audition track"
        );
    }

    #[test]
    fn the_measured_path_keeps_its_reference_gain() {
        // A source that reports loudness still matches the bank reference.
        assert_eq!(audition_gain_db(Some(-24.2)), 7.0);
        assert_eq!(audition_gain_db(None), 0.0);
        let mut measured = sine_buffer(0.1, 2.0, 48_000);
        apply_audition_level(&mut measured, 48_000);
        let lufs = oc_engine::analysis::measure_stereo_interleaved(&measured, 48_000)
            .integrated_lufs
            .expect("the measured buffer keeps a loudness");
        assert!(
            (lufs - AUDITION_REFERENCE_LUFS).abs() < 1.5,
            "the measured path left the buffer at {lufs:.2} LUFS"
        );
    }

    #[test]
    fn a_synth_preview_stays_under_the_ceiling_without_a_measurement() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
        state.lock_inner().audio.sample_rate = rate;

        // The patch browser's own audition at unity output. The instrument's
        // master ceiling holds it; nothing rendered it to find that out.
        let generation = state.audition.begin("Hot candidate");
        let factory = streamed_project(
            prepare_factory(&state, &gain_candidate("1.0"), &[], generation).unwrap(),
        );
        assert_eq!(factory.tracks[0].mix.volume_db, 0.0);
        let factory_peak = preview_level(&factory, rate, None);
        assert!(
            factory_peak <= PREVIEW_PEAK_CEILING_DB,
            "the factory preview peaks at {factory_peak:.2} dBFS"
        );

        // A SoundFont instrument is outside the synth's contract: its own
        // sample level applies and the preview is not trimmed either.
        let font = soundfont_fixture();
        state.lock_inner().audio.soundfont_path = Some(font.clone());
        let mut loud = oc_dsl::parse(SOUNDFONT_PROJECT).unwrap();
        loud.presets[0].effects.push(gain_effect(16.0));
        state.lock_inner().project = loud;
        state.audition.invalidate();
        let generation = state.audition.begin("font");
        let loud_font = streamed_project(prepare(&state, "font", generation).unwrap());
        assert_eq!(loud_font.tracks[0].mix.volume_db, 0.0);
        assert!(
            preview_level(&loud_font, rate, Some(&font)) > PREVIEW_PEAK_CEILING_DB,
            "a loud SoundFont keeps its own level"
        );

        state.lock_inner().project = oc_dsl::parse(SOUNDFONT_PROJECT).unwrap();
        state.audition.invalidate();
        let generation = state.audition.begin("font");
        let quiet_font = streamed_project(prepare(&state, "font", generation).unwrap());
        assert_eq!(quiet_font.tracks[0].mix.volume_db, 0.0);
        assert!(
            preview_level(&quiet_font, rate, Some(&font)) < PREVIEW_PEAK_CEILING_DB,
            "the quiet SoundFont preview sits below the ceiling"
        );
    }

    /// The streamed preview — not only the project it was built from — plays at
    /// or below the ceiling. This runs the whole press path on the simulated
    /// device and reads the loudest sample the device actually heard.
    #[test]
    fn the_streamed_preview_plays_at_or_below_the_ceiling() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
        state.lock_inner().audio.sample_rate = rate;

        let generation = state.audition.begin("Hot candidate");
        let started = Instant::now();
        run_factory(&state, &gain_candidate("1.0"), &[], generation, started);
        assert_eq!(state.audition.snapshot().status, READY);

        // Two seconds of the figure covers every note; the ceiling is reached there.
        let blocks = 2 * u64::from(rate) / 256;
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while state.lock_inner().audio.heard_audition_blocks() < blocks {
            assert!(
                Instant::now() < deadline,
                "the preview never played a second"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let played = state.lock_inner().audio.heard_audition_peak();
        let played_db = 20.0 * f64::from(played).log10();
        state.lock_inner().audio.stop_audition();
        state.audition.cancel_all();
        assert!(
            played_db <= PREVIEW_PEAK_CEILING_DB + 0.1,
            "the streamed preview peaked at {played_db:.2} dBFS"
        );
    }

    /// The loudest patch in the browser's bank. Before the stage law it
    /// rendered at -3.14 dBFS and the preview trimmed it by 2.86 dB; the
    /// resonance normalisation and the master ceiling now hold it below the
    /// contract with no measurement. The bank-wide harness
    /// (`review_factory_preview_levels`) reports the ranking.
    #[test]
    fn the_loudest_factory_patch_stays_under_the_ceiling() {
        let loudest = factory_batch(&[
            ("osc.a.waveform", "1"),
            ("osc.a.level", "1.0"),
            ("osc.b.waveform", "1"),
            ("osc.b.level", "0.5"),
            ("osc.b.semitones", "7"),
            ("filter.model", "0"),
            ("filter.mode", "1"),
            ("filter.cutoff", "800"),
            ("filter.resonance", "0.5"),
            ("filter2.enabled", "1"),
            ("filter2.model", "0"),
            ("filter2.mode", "1"),
            ("filter2.cutoff", "2400"),
            ("filter2.resonance", "0.35"),
            ("filter.routing", "1"),
            ("lfo2.rate", "5.5"),
            ("mod.1.source", "11"),
            ("mod.1.destination", "0"),
            ("mod.1.amount", "0.15"),
            ("mod.2.source", "11"),
            ("mod.2.destination", "12"),
            ("mod.2.amount", "-0.12"),
            ("amp.attack", "0.02"),
            ("amp.decay", "0.3"),
            ("amp.sustain", "0.85"),
            ("amp.release", "0.4"),
            ("unison.voices", "1"),
            ("unison.width", "0"),
            ("voice.mono", "1"),
            ("voice.legato", "1"),
            ("voice.glide", "0.08"),
            ("output.gain", "1.00"),
        ]);

        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let mut candidate = project.clone();
        candidate
            .presets
            .push(factory_preset("Factory audition", &loudest, &[]));
        let preview =
            crate::audio::prepare_preset_audition(&candidate, "Factory audition", None).unwrap();
        assert_eq!(
            preview.tracks[0].mix.volume_db, 0.0,
            "the preview baked a trim into the audition track"
        );
        let peak_db = preview_level(&preview, 48_000, None);
        assert!(
            peak_db <= PREVIEW_PEAK_CEILING_DB,
            "the loudest factory patch peaks at {peak_db:.2} dBFS"
        );
    }

    /// Every patch in the browser's own bank file, name and parameters.
    fn factory_bank() -> Vec<(String, Vec<(String, ParamValue)>)> {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data/factoryPresets.ts"),
        )
        .expect("the browser's factory bank");
        let mut bank = Vec::new();
        let mut name: Option<String> = None;
        let mut params: Vec<(String, ParamValue)> = Vec::new();
        let mut in_params = false;
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("name: \"") {
                name = Some(rest.trim_end_matches("\",").to_owned());
            } else if line.starts_with("params: [") {
                in_params = true;
            } else if in_params && line == "]," {
                if let Some(name) = name.take() {
                    bank.push((name, std::mem::take(&mut params)));
                }
                in_params = false;
            } else if in_params && line.starts_with('[') {
                // One line carries several `["key", "value"]` pairs.
                let mut cursor = 0;
                while let Some(offset) = line[cursor..].find("[\"") {
                    let key_start = cursor + offset + 2;
                    let Some(key_length) = line[key_start..].find('"') else {
                        break;
                    };
                    let key = &line[key_start..key_start + key_length];
                    let mut value_start = key_start + key_length + 1;
                    while matches!(line.as_bytes().get(value_start), Some(b',' | b' ')) {
                        value_start += 1;
                    }
                    let quoted = line.as_bytes().get(value_start) == Some(&b'"');
                    let (value, next) = if quoted {
                        let inner = value_start + 1;
                        let Some(length) = line[inner..].find('"') else {
                            break;
                        };
                        (&line[inner..inner + length], inner + length + 1)
                    } else {
                        let end = line[value_start..]
                            .find(']')
                            .map_or(line.len(), |offset| value_start + offset);
                        (&line[value_start..end], end)
                    };
                    if !value.is_empty()
                        && value != "null"
                        && let Ok(parsed) = crate::commands::shared::parse_param_value(value)
                    {
                        params.push((key.to_owned(), parsed));
                    }
                    cursor = next;
                }
            }
        }
        bank
    }

    /// One oscillator at several unison counts and detune spreads: the summed
    /// level is what shows whether the unison law normalises power or peak.
    #[test]
    #[ignore = "reports the unison sum of a single oscillator"]
    fn review_unison_sum_levels() {
        let root = None;
        for detune in [0.0_f64, 6.0, 22.0] {
            for voices in [1_usize, 2, 4, 6, 9, 16] {
                let dsl = format!(
                    "@project\n  tempo: 120\n@preset probe\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{osc.a.level: 1.0, osc.a.unison: {voices}, osc.a.detune: {detune}, osc.a.waveform: 1, osc.b.level: 0, osc.c.level: 0, sub.level: 0, filter.cutoff: 20000, amp.sustain: 1.0, output.loudness_trim_db: 0}}\n@track lead | preset: probe, instrument: synth/lead_square\n  @phrase note [1:1 - 3:1]\n    C4 w\n"
                );
                let project = oc_dsl::parse(&dsl).unwrap();
                let Ok(prepared) = crate::audio::prepare_preset_audition(&project, "probe", root)
                else {
                    continue;
                };
                let Ok(buffer) = crate::audio::AudioState::render_prepared_audition(
                    &prepared, 48_000, None, root,
                ) else {
                    continue;
                };
                let rms = (buffer.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
                    / buffer.len().max(1) as f64)
                    .sqrt();
                eprintln!(
                    "UNISON detune={detune:5.1} voices={voices:2} rms_db={:.2}",
                    20.0 * rms.max(1e-9).log10()
                );
            }
        }
    }

    /// Stage-by-stage loudness of one preset: the same patch with one stage
    /// isolated at a time, so a level difference can be attributed.
    #[test]
    #[ignore = "reads OCP_PRESET_STAGE_PROJECT and OCP_PRESET_STAGE_NAME"]
    fn review_preset_stage_levels() {
        let Some(path) = std::env::var_os("OCP_PRESET_STAGE_PROJECT") else {
            eprintln!("STAGE_LEVEL set OCP_PRESET_STAGE_PROJECT to an .ocp path");
            return;
        };
        let name = std::env::var("OCP_PRESET_STAGE_NAME").expect("OCP_PRESET_STAGE_NAME");
        let path = std::path::PathBuf::from(path);
        let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let root = path.parent().map(std::path::Path::to_path_buf);
        let variants: Vec<(&str, Vec<(&str, ParamValue)>)> = vec![
            ("as authored", vec![]),
            (
                "unison 1 everywhere",
                vec![
                    ("osc.a.unison", ParamValue::Number(1.0)),
                    ("osc.b.unison", ParamValue::Number(1.0)),
                    ("osc.c.unison", ParamValue::Number(1.0)),
                ],
            ),
            ("sub off", vec![("sub.level", ParamValue::Number(0.0))]),
            ("osc B off", vec![("osc.b.level", ParamValue::Number(0.0))]),
            ("osc A off", vec![("osc.a.level", ParamValue::Number(0.0))]),
            (
                "sub phase 0.5",
                vec![("sub.initial_phase", ParamValue::Number(0.5))],
            ),
            (
                "sub phase 0.25",
                vec![("sub.initial_phase", ParamValue::Number(0.25))],
            ),
            (
                "only the sub",
                vec![
                    ("osc.a.level", ParamValue::Number(0.0)),
                    ("osc.b.level", ParamValue::Number(0.0)),
                    ("osc.c.level", ParamValue::Number(0.0)),
                ],
            ),
            (
                "A and sub, no B",
                vec![("osc.b.level", ParamValue::Number(0.0))],
            ),
            (
                "osc B and C off",
                vec![
                    ("osc.b.level", ParamValue::Number(0.0)),
                    ("osc.c.level", ParamValue::Number(0.0)),
                    ("sub.level", ParamValue::Number(0.0)),
                ],
            ),
            (
                "filter wide open",
                vec![
                    ("filter.cutoff", ParamValue::Number(20_000.0)),
                    ("filter.env.amount", ParamValue::Number(0.0)),
                    ("filter.resonance", ParamValue::Number(0.0)),
                    ("filter.drive", ParamValue::Number(0.0)),
                ],
            ),
            (
                "sustain 1, long release",
                vec![
                    ("amp.sustain", ParamValue::Number(1.0)),
                    ("amp.decay", ParamValue::Number(0.01)),
                    ("amp.release", ParamValue::Number(0.8)),
                ],
            ),
        ];
        for (label, overrides) in variants {
            let mut candidate = project.clone();
            if let Some(preset) = candidate.presets.iter_mut().find(|p| p.name == name) {
                // Pin the trim to zero so the numbers are the raw stage output.
                preset
                    .params
                    .push(("output.loudness_trim_db".into(), ParamValue::Number(0.0)));
                for (key, value) in &overrides {
                    match preset.params.iter_mut().find(|(k, _)| k == key) {
                        Some(slot) => slot.1 = value.clone(),
                        None => preset.params.push(((*key).to_string(), value.clone())),
                    }
                }
            }
            let Ok(prepared) =
                crate::audio::prepare_preset_audition(&candidate, &name, root.as_deref())
            else {
                eprintln!("STAGE_LEVEL {label:24} prepare failed");
                continue;
            };
            let Ok(buffer) = crate::audio::AudioState::render_prepared_audition(
                &prepared,
                48_000,
                None,
                root.as_deref(),
            ) else {
                eprintln!("STAGE_LEVEL {label:24} render failed");
                continue;
            };
            let rms = (buffer.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
                / buffer.len().max(1) as f64)
                .sqrt();
            eprintln!(
                "STAGE_LEVEL {:24} rms_db={:.2}",
                label,
                20.0 * rms.max(1e-9).log10()
            );
        }
        let _ = project;
    }

    /// Peak and sustained loudness of a project's own synth presets, so two
    /// patches can be compared at the level the preview plays them.
    #[test]
    #[ignore = "reads OCP_PRESET_PROBE_PROJECT and reports each preset's level"]
    fn review_project_preset_levels() {
        let Some(path) = std::env::var_os("OCP_PRESET_PROBE_PROJECT") else {
            eprintln!("PRESET_LEVEL set OCP_PRESET_PROBE_PROJECT to an .ocp path");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let root = path.parent().map(std::path::Path::to_path_buf);
        let rate = 48_000;
        for preset in &project.presets {
            if preset.plugin.as_deref() != Some("builtin.open_synth")
                && preset.source.as_deref() != Some("soundfont")
            {
                continue;
            }
            let font = preset.file.as_deref().map(std::path::PathBuf::from);
            let Ok(prepared) =
                crate::audio::prepare_preset_audition(&project, &preset.name, root.as_deref())
            else {
                eprintln!("PRESET_LEVEL {:?} prepare failed", preset.name);
                continue;
            };
            let Ok(buffer) = crate::audio::AudioState::render_prepared_audition(
                &prepared,
                rate,
                font.as_deref(),
                root.as_deref(),
            ) else {
                eprintln!("PRESET_LEVEL {:?} render failed", preset.name);
                continue;
            };
            let peak = buffer.iter().fold(0.0f64, |m, s| m.max(f64::from(s.abs())));
            let peak_db = 20.0 * peak.max(1e-9).log10();
            let frames = buffer.len() / 2;
            let from = frames / 3;
            let to = (frames * 2 / 3).max(from + 1);
            let window = &buffer[from * 2..to * 2];
            let rms = (window.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
                / window.len() as f64)
                .sqrt();
            let rms_db = 20.0 * rms.max(1e-9).log10();
            let full = (buffer.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
                / buffer.len().max(1) as f64)
                .sqrt();
            let full_db = 20.0 * full.max(1e-9).log10();
            eprintln!(
                "PRESET_LEVEL name={:?} peak_db={peak_db:.2} full_rms_db={full_db:.2} mid_rms_db={rms_db:.2}",
                preset.name
            );
        }
    }

    /// Why a GM SoundFont program sits so far below a synth patch: the zone's
    /// own attenuation, the scale of the sample the zone picks, and what the
    /// shared figure renders through the engine's own gain law.
    ///
    /// Reports programs 0, 1, 40 and 48 against one synth oscillator playing
    /// the figure's first note, so a quiet program can be told from a quiet
    /// whole path.
    #[test]
    #[ignore = "measurement: needs OCP_REVIEW_SOUNDFONT naming a bank"]
    fn review_gm_program_levels() {
        let Some(path) = soundfont_under_review() else {
            return;
        };
        let font = oc_engine::sf2::SoundFont::load_shared(&path).expect("the bank loads");
        eprintln!(
            "GM_LEVEL bank={} presets={} samples={} compressed={}",
            path.display(),
            font.presets.len(),
            font.sample_headers.len(),
            font.compressed
        );

        // The instrument's own scale: what each zone multiplies in before the
        // engine's gain law, and how hot the sample it plays is.
        for (bank, program) in [(0u16, 0u16), (0, 1), (0, 40), (0, 48)] {
            let Some(index) = font.preset_index(bank, program) else {
                continue;
            };
            let preset = &font.presets[index];
            let zone_peak_db = preset
                .zones
                .iter()
                .map(|zone| {
                    let peak = (zone.start..zone.end).fold(0.0f32, |loudest, frame| {
                        loudest.max(font.sample_bank.point(frame as usize).abs())
                    });
                    20.0 * f64::from(peak).log10()
                })
                .fold(f64::NEG_INFINITY, f64::max);
            let attenuation_db = preset
                .zones
                .iter()
                .map(|zone| f64::from(zone.attenuation_cb) * 0.04)
                .fold(f64::NEG_INFINITY, f64::max);
            eprintln!(
                "GM_LEVEL preset={} program={program} zones={} loudest_sample_db={zone_peak_db:.2} \
                 loudest_zone_attenuation_db={attenuation_db:.2}",
                preset.name,
                preset.zones.len()
            );
            for zone in preset
                .zones
                .iter()
                .filter(|zone| zone.matches(60, 100))
                .take(3)
            {
                let peak = (zone.start..zone.end).fold(0.0f32, |loudest, frame| {
                    loudest.max(font.sample_bank.point(frame as usize).abs())
                });
                let mut controllers = [0u8; 128];
                controllers[7] = 100;
                controllers[10] = 64;
                controllers[11] = 127;
                let input =
                    oc_engine::sf2::modulation::ModInput::new(&controllers, 0, 60, 100, 0.0);
                let record_db = f64::from(zone.modulation(&input).attenuation_cb) * 0.1;
                eprintln!(
                    "GM_LEVEL zone program={program} key={}..{} vel={}..{} attenuation_units={} \
                     generator_db={:.2} record_db={:.2} sample={} peak_db={:.2}",
                    zone.key_low,
                    zone.key_high,
                    zone.vel_low,
                    zone.vel_high,
                    zone.attenuation_cb,
                    f64::from(zone.attenuation_cb) * 0.04,
                    record_db,
                    font.sample_headers[zone.sample].name,
                    20.0 * f64::from(peak).log10()
                );
                for modulator in &zone.modulators {
                    if modulator.destination
                        == oc_engine::sf2::modulation::Destination::InitialAttenuation
                    {
                        eprintln!(
                            "GM_LEVEL record program={program} source={:?} direction={:?} \
                             polarity={:?} transform={:?} amount={:.2}",
                            modulator.source,
                            modulator.direction,
                            modulator.polarity,
                            modulator.transform,
                            modulator.amount
                        );
                    }
                }
            }
        }

        for program in [0u8, 1, 40, 48] {
            let buffer =
                crate::audio::AudioState::render_program_audition(&path, 48_000, program, 0)
                    .expect("the program renders");
            let (peak, rms) = peak_and_rms(&buffer);
            eprintln!("GM_LEVEL figure program={program} peak_db={peak:.2} rms_db={rms:.2}");
        }

        // One piano note at several velocity and channel-volume settings, so
        // the velocity law can be separated from the bank's own records.
        for (velocity, volume) in [(100u8, 100u8), (127, 100), (127, 127), (100, 127)] {
            let mut renderer = oc_engine::sf2::Sf2Renderer::from_font(Arc::clone(&font), 48_000);
            renderer
                .send(&oc_engine::interpret::MidiMessage::ProgramChange {
                    channel: 0,
                    program: 0,
                })
                .expect("program 0");
            renderer
                .send(&oc_engine::interpret::MidiMessage::ControlChange {
                    channel: 0,
                    controller: 7,
                    value: volume,
                })
                .expect("volume");
            renderer
                .send(&oc_engine::interpret::MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity,
                })
                .expect("note on");
            let buffer = renderer.render_samples(48_000);
            let (peak, rms) = peak_and_rms(&buffer);
            eprintln!(
                "GM_LEVEL note velocity={velocity} cc7={volume} peak_db={peak:.2} rms_db={rms:.2}"
            );
        }

        // One synth oscillator at the figure's first note, the reference the
        // SoundFont path has to sit next to.
        let (reference_peak, reference_rms) = synth_reference_level();
        eprintln!("GM_LEVEL synth_reference peak_db={reference_peak:.2} rms_db={reference_rms:.2}");
    }

    /// The same program and note through every bank named in
    /// `OCP_REVIEW_SOUNDFONTS` (comma separated), so a quiet bank can be told
    /// from a quiet path.
    #[test]
    #[ignore = "measurement: needs OCP_REVIEW_SOUNDFONTS naming banks"]
    fn review_gm_bank_comparison() {
        let Some(banks) = std::env::var_os("OCP_REVIEW_SOUNDFONTS") else {
            eprintln!("GM_BANK set OCP_REVIEW_SOUNDFONTS to comma-separated banks");
            return;
        };
        for bank in banks.to_string_lossy().split(',') {
            let bank = std::path::PathBuf::from(bank.trim());
            let Ok(font) = oc_engine::sf2::SoundFont::load_shared(&bank) else {
                eprintln!("GM_BANK {}: does not load", bank.display());
                continue;
            };
            let Some(index) = font.preset_index(0, 0) else {
                eprintln!("GM_BANK {}: no program 0", bank.display());
                continue;
            };
            let preset = &font.presets[index];
            let mut controllers = [0u8; 128];
            controllers[7] = 100;
            controllers[10] = 64;
            controllers[11] = 127;
            let attenuation_db = preset
                .zones
                .iter()
                .filter(|zone| zone.matches(60, 100))
                .map(|zone| {
                    let input =
                        oc_engine::sf2::modulation::ModInput::new(&controllers, 0, 60, 100, 0.0);
                    f64::from(zone.attenuation_cb) * 0.04
                        + f64::from(zone.modulation(&input).attenuation_cb) * 0.1
                })
                .fold(f64::NEG_INFINITY, f64::max);
            let mut renderer = oc_engine::sf2::Sf2Renderer::from_font(Arc::clone(&font), 48_000);
            renderer
                .send(&oc_engine::interpret::MidiMessage::ProgramChange {
                    channel: 0,
                    program: 0,
                })
                .expect("program 0");
            renderer
                .send(&oc_engine::interpret::MidiMessage::ControlChange {
                    channel: 0,
                    controller: 7,
                    value: 100,
                })
                .expect("volume");
            renderer
                .send(&oc_engine::interpret::MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                })
                .expect("note on");
            let buffer = renderer.render_samples(48_000);
            let (peak, rms) = peak_and_rms(&buffer);
            eprintln!(
                "GM_BANK bank={} preset={} zones={} attenuation_db={attenuation_db:.2} \
                 peak_db={peak:.2} rms_db={rms:.2}",
                bank.display(),
                preset.name,
                preset.zones.len()
            );
        }
    }

    /// The bank the measurement should read: `OCP_REVIEW_SOUNDFONT`.
    fn soundfont_under_review() -> Option<std::path::PathBuf> {
        match std::env::var_os("OCP_REVIEW_SOUNDFONT") {
            Some(path) => Some(std::path::PathBuf::from(path)),
            None => {
                eprintln!("GM_LEVEL set OCP_REVIEW_SOUNDFONT to a SoundFont bank");
                None
            }
        }
    }

    fn peak_and_rms(buffer: &[f32]) -> (f64, f64) {
        let peak = buffer.iter().fold(0.0f64, |loudest, sample| {
            loudest.max(f64::from(sample.abs()))
        });
        let rms = (buffer
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / buffer.len().max(1) as f64)
            .sqrt();
        (20.0 * peak.max(1e-9).log10(), 20.0 * rms.max(1e-9).log10())
    }

    /// Peak and RMS of one saw oscillator at C4, velocity 100, wide open: the
    /// level a synth patch carries at the same note.
    fn synth_reference_level() -> (f64, f64) {
        use oc_synth::synth::{OpenSynth, SynthPatch, Waveform};
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Saw,
            oscillator_a_level: 1.0,
            unison_voices: 1,
            stereo_width: 0.0,
            filter_wet: 0.0,
            loudness_gain: 1.0,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        let buffer: Vec<f32> = (0..96_000)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect();
        peak_and_rms(&buffer)
    }

    #[test]
    #[ignore = "renders every factory patch to report its preview peak; run alone with --ignored --nocapture"]
    fn review_factory_preview_levels() {
        let project = oc_dsl::parse(TRANSPORT_PROJECT).unwrap();
        let (state, _clock) = simulated_state(&project);
        state.lock_inner().audio.sample_rate = 48_000;
        let context = candidate_render_context(&state);
        let bank = factory_bank();
        let mut loudest = (f64::NEG_INFINITY, String::new());
        for (name, params) in &bank {
            let preset = unique_factory_preset_name(&context.project, "Factory audition");
            let buffer = render_candidate(&context, &preset, params, &[]).unwrap();
            let peak_db = preview_peak_db(&buffer);
            eprintln!("FACTORY_LEVEL patch={name:?} peak_db={peak_db:.2}");
            if peak_db > loudest.0 {
                loudest = (peak_db, name.clone());
            }
        }
        eprintln!(
            "FACTORY_LEVEL loudest={:?} peak_db={:.2} patches={}",
            loudest.1,
            loudest.0,
            bank.len()
        );
    }
}
