//! Render: complete pipeline from AST → WAV file
//!
//! Pipeline:
//! 1. Interpret AST → MIDI events per track
//! 2. Render project-owned instruments natively, other tracks through the
//!    SoundFont engine
//! 3. Mix every track buffer through the complete AudioGraph
//! 4. Write to WAV
//!
//! The plugin-aware per-track path uses RenderCache and the complete AudioGraph.
//! Offline callers can opt into stage-labelled stem capture on RenderCache;
//! capture never replaces or independently reprocesses the authoritative mix.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use oc_dsl::ast::Project;
use serde::Serialize;

use crate::analysis;
use crate::dsp::AudioEffect;
use crate::dsp::limiter::Limiter;
use crate::encode::{self, EncodeReport, EncodeSpec};
use crate::graph::AudioGraph;
use crate::interpret::{self, InterpretSettings};
use crate::soundfont::SoundFontRenderer;
use oc_host::clap_instance::ClapInstance;

/// Request-owned configuration shared by mix, stem, and track-tap renders.
/// The worker count is resolved before rendering and stays fixed across stages.
pub struct RenderOptions<'a> {
    pub sample_rate: u32,
    /// Additional attenuation for full-mix export; stems and track taps keep their authored gain.
    pub output_gain_db: f32,
    pub resource_root: Option<&'a Path>,
    pub minimum_duration_seconds: f64,
    pub pre_roll_seconds: f64,
    pub cancel: Option<crate::render_cancel::CancelToken>,
    pub progress: Option<crate::render_cancel::ProgressCallback>,
    pub workers: crate::render_parallel::RenderWorkerOptions,
}

impl RenderOptions<'_> {
    /// Default request at a sample rate and worker policy.
    pub fn new(sample_rate: u32, workers: crate::render_parallel::RenderWorkerOptions) -> Self {
        Self {
            sample_rate,
            workers,
            output_gain_db: 0.0,
            resource_root: None,
            minimum_duration_seconds: 0.0,
            pre_roll_seconds: 0.0,
            cancel: None,
            progress: None,
        }
    }
}

/// Sample-exact track selection, separate from execution configuration.
pub struct TrackRenderRange {
    pub track: usize,
    pub start_sample: u64,
    pub end_sample: u64,
}

/// True-peak ceiling for loudness correction, enforced by the master limiter.
pub const LOUDNESS_CEILING_DBFS: f64 = -1.0;
/// Release a bounce assumes for a track that plays notes but declares none.
pub const ASSUMED_RELEASE_SECONDS: f64 = 0.5;
/// Longest tail a bounce renders past its range end.
pub const MAX_BOUNCE_TAIL_SECONDS: f64 = 4.0;
/// Master-bus limiter: 100 ms auto release, 5 ms lookahead, true peak.
const LOUDNESS_LIMITER_RELEASE_MS: f64 = 100.0;
const LOUDNESS_LIMITER_LOOKAHEAD_MS: f64 = 5.0;
/// A pass within this many LU of the target stops the correction loop.
const LOUDNESS_PASS_TOLERANCE_LU: f64 = 0.05;
const LOUDNESS_MAX_PASSES: usize = 2;

/// One measure → gain → limit pass over the unmodified mix.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoudnessPass {
    /// 1-based pass number.
    pub pass: usize,
    /// Cumulative correction gain applied before the limiter.
    pub gain_db: f64,
    pub achieved_lufs: f64,
    /// Achieved minus target after this pass.
    pub residual_lu: f64,
    pub sample_peak_dbfs: f64,
    pub true_peak_dbfs: f64,
    /// Detector true peak before the limiter, in dBFS.
    pub input_true_peak_dbfs: f64,
    pub short_term_max_lufs: Option<f64>,
    pub gain_reduction_max_db: f64,
    pub gain_reduction_mean_db: f64,
    pub gain_reduction_p50_db: f64,
    pub gain_reduction_p95_db: f64,
    pub gain_reduction_p99_db: f64,
    pub limited_fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
/// Measured loudness and limiter outcome for one render.
pub struct LoudnessReport {
    pub target_lufs: f64,
    pub measured_lufs: f64,
    /// Final cumulative correction gain applied before the limiter.
    pub applied_gain_db: f64,
    pub achieved_lufs: f64,
    pub sample_peak_dbfs: f64,
    pub ceiling_dbfs: f64,
    /// True when the limiter had to reduce gain to hold the ceiling.
    pub limited_by_ceiling: bool,
    /// How far the requested peak exceeded the ceiling, in dB (0 when untouched).
    pub peak_over_ceiling_db: f64,
    /// Achieved minus target after the final pass.
    pub shortfall_lu: f64,
    pub limiter_gain_reduction_max_db: f64,
    pub limiter_gain_reduction_mean_db: f64,
    pub limiter_gain_reduction_p50_db: f64,
    pub limiter_gain_reduction_p95_db: f64,
    pub limiter_gain_reduction_p99_db: f64,
    pub limiter_limited_fraction: f64,
    /// Detector true peak before the limiter, in dBFS.
    pub input_true_peak_dbfs: f64,
    pub short_term_max_lufs: Option<f64>,
    pub true_peak_dbfs: f64,
    pub passes: usize,
    pub ceiling_reached: bool,
    pub pass_reports: Vec<LoudnessPass>,
}

fn peak_linear(samples: &[f32]) -> f64 {
    samples
        .iter()
        .fold(0.0_f64, |peak, value| peak.max(f64::from(value.abs())))
}

struct LimitedPass {
    samples: Vec<f32>,
    gain_reduction: crate::dsp::limiter::GainReductionSummary,
    input_true_peak_dbfs: f64,
    short_term_max_lufs: Option<f64>,
}

/// Gain-scale a copy of `input`, run it through the oc-dsp `Limiter`, and
/// compensate its lookahead delay so the output stays frame-aligned.
fn limit_pass(input: &[f32], gain_db: f64, sample_rate: u32) -> LimitedPass {
    let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
    let mut samples: Vec<f32> = input.iter().map(|sample| sample * gain).collect();
    let mut limiter = Limiter::new(sample_rate);
    limiter.set_param("ceiling", LOUDNESS_CEILING_DBFS);
    limiter.set_param("release", LOUDNESS_LIMITER_RELEASE_MS);
    limiter.set_param("release_mode", 1.0);
    limiter.set_param("lookahead", LOUDNESS_LIMITER_LOOKAHEAD_MS);
    limiter.set_param("true_peak", 1.0);
    let latency = limiter.latency_samples() as usize;
    samples.resize(samples.len() + latency * 2, 0.0);
    limiter.process(&mut samples, sample_rate);
    samples.copy_within(latency * 2..latency * 2 + input.len(), 0);
    samples.truncate(input.len());
    let short_term_max_lufs =
        analysis::measure_stereo_interleaved(&samples, sample_rate).short_term_max_lufs;
    LimitedPass {
        samples,
        gain_reduction: limiter.gain_reduction_summary(),
        input_true_peak_dbfs: limiter.max_input_peak_dbfs(),
        short_term_max_lufs,
    }
}

/// Apply the project's static loudness target to a finished stereo mix.
///
/// Each pass applies the cumulative gain after the master fader, limits with
/// `oc_dsp::limiter::Limiter` at `LOUDNESS_CEILING_DBFS`, then re-measures.
/// A residual above `LOUDNESS_PASS_TOLERANCE_LU` trims the gain once; every
/// pass starts from the unmodified mix so the result is deterministic.
pub fn normalise_loudness(
    project: &Project,
    samples: &mut [f32],
    sample_rate: u32,
) -> Result<Option<LoudnessReport>, String> {
    let Some(target) = project
        .master
        .as_ref()
        .and_then(|master| master.loudness_target)
    else {
        return Ok(None);
    };
    let measured = analysis::measure_stereo_interleaved(samples, sample_rate)
        .integrated_lufs
        .ok_or_else(|| {
            format!(
                "loudness target {target} LUFS cannot be enforced: the mix is shorter than 400 ms or every block is below the absolute gate"
            )
        })?;
    let original = samples.to_vec();
    let source_peak = peak_linear(&original);
    let ceiling = 10.0_f64.powf(LOUDNESS_CEILING_DBFS / 20.0);
    let mut gain_db = target - measured;
    let mut requested_peak = source_peak * 10.0_f64.powf(gain_db / 20.0);
    let mut pass_reports = Vec::with_capacity(LOUDNESS_MAX_PASSES);
    let mut output = Vec::new();
    let mut final_gr = None;

    for pass in 1..=LOUDNESS_MAX_PASSES {
        let limited = limit_pass(&original, gain_db, sample_rate);
        let achieved = analysis::measure_stereo_interleaved(&limited.samples, sample_rate)
            .integrated_lufs
            .ok_or_else(|| "loudness meter failed after limiter pass".to_string())?;
        let residual = achieved - target;
        requested_peak = source_peak * 10.0_f64.powf(gain_db / 20.0);
        let summary = limited.gain_reduction;
        pass_reports.push(LoudnessPass {
            pass,
            gain_db,
            achieved_lufs: achieved,
            residual_lu: residual,
            sample_peak_dbfs: 20.0 * peak_linear(&limited.samples).max(1e-12).log10(),
            true_peak_dbfs: analysis::true_peak_dbfs(&limited.samples),
            input_true_peak_dbfs: limited.input_true_peak_dbfs,
            short_term_max_lufs: limited.short_term_max_lufs,
            gain_reduction_max_db: summary.max_db,
            gain_reduction_mean_db: summary.mean_db,
            gain_reduction_p50_db: summary.p50_db,
            gain_reduction_p95_db: summary.p95_db,
            gain_reduction_p99_db: summary.p99_db,
            limited_fraction: summary.limited_fraction,
        });
        final_gr = Some(summary);
        output = limited.samples;
        if residual.abs() <= LOUDNESS_PASS_TOLERANCE_LU || pass == LOUDNESS_MAX_PASSES {
            break;
        }
        gain_db -= residual;
    }

    samples.copy_from_slice(&output);
    let final_pass = pass_reports
        .last()
        .expect("the correction loop always runs at least one pass");
    let final_gr = final_gr.expect("the correction loop always runs at least one pass");
    let ceiling_reached = requested_peak > ceiling;
    let report = LoudnessReport {
        target_lufs: target,
        measured_lufs: measured,
        applied_gain_db: gain_db,
        achieved_lufs: final_pass.achieved_lufs,
        sample_peak_dbfs: final_pass.sample_peak_dbfs,
        ceiling_dbfs: LOUDNESS_CEILING_DBFS,
        limited_by_ceiling: ceiling_reached,
        peak_over_ceiling_db: if ceiling_reached {
            20.0 * (requested_peak / ceiling).log10()
        } else {
            0.0
        },
        shortfall_lu: final_pass.residual_lu,
        limiter_gain_reduction_max_db: final_gr.max_db,
        limiter_gain_reduction_mean_db: final_gr.mean_db,
        limiter_gain_reduction_p50_db: final_gr.p50_db,
        limiter_gain_reduction_p95_db: final_gr.p95_db,
        limiter_gain_reduction_p99_db: final_gr.p99_db,
        limiter_limited_fraction: final_gr.limited_fraction,
        input_true_peak_dbfs: final_pass.input_true_peak_dbfs,
        short_term_max_lufs: final_pass.short_term_max_lufs,
        true_peak_dbfs: final_pass.true_peak_dbfs,
        passes: pass_reports.len(),
        ceiling_reached,
        pass_reports,
    };
    Ok(Some(report))
}

fn normalise_for_export(
    project: &Project,
    samples: &mut [f32],
    sample_rate: u32,
) -> Result<(), String> {
    normalise_loudness(project, samples, sample_rate).map(|_| ())
}

/// True when any level moves during the render: a bus or master gain clip, a
/// track gain clip, or a hairpin that expands into one.
pub fn has_gain_automation(project: &Project) -> bool {
    project
        .buses
        .iter()
        .any(|bus| !bus.gain_automation.is_empty())
        || project
            .master
            .as_ref()
            .is_some_and(|master| !master.gain_automation.is_empty())
        || project
            .tracks
            .iter()
            .any(|track| !track.gain_automation.is_empty())
        || oc_dsl::dynamics::has_hairpins(project)
}

/// Projects with audio-derived control need the full semantic pass: a control
/// source names a track and a ride names a control source.
pub fn has_control_sources(project: &Project) -> bool {
    !project.properties.control_sources.is_empty()
        || project
            .tracks
            .iter()
            .any(|track| track.mix.control_gain.is_some())
}

pub(crate) fn validate_gain_project(project: &Project) -> Result<(), String> {
    validate_render_timing(project)?;
    if has_gain_automation(project) || has_control_sources(project) {
        oc_dsl::validate(project)
            .map_err(|errors| format!("Invalid automated project: {errors:?}"))?;
    }
    Ok(())
}

/// MIDI timing supports mapped time. Every hosted format now receives a
/// per-block clock: source lanes split their blocks, and the effect graph splits
/// at the same boundaries. Only a malformed map still rejects the render.
pub(crate) fn validate_render_timing(project: &Project) -> Result<(), String> {
    project
        .properties
        .checked_automation_normalization()
        .map_err(|error| error.to_string())?;
    oc_dsl::timing::ProjectClock::new(&project.properties)?;
    Ok(())
}

/// Render a project to a WAV file through the project AudioGraph.
pub fn render_to_wav(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    let audio = render_to_buffer(project, soundfont_path, spec.sample_rate)?;
    encode::encode_to_file(output_path, &spec, &audio)
}

/// Render each supported track independently, then apply the full AudioGraph.
/// This slower path is authoritative for offline causal mix supervision.
/// Builtin instrument presets are rendered through their declared parameter
/// state; other tracks use the SoundFont player. External CLAP/VST3 support
/// remains on the plugin-instance render path until this API owns instances.
pub fn render_to_wav_per_track(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    output_gain_db: f32,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    if !output_gain_db.is_finite() || output_gain_db > 0.0 {
        return Err("Per-track offline output gain must be finite and non-positive".into());
    }
    let mut audio =
        render_to_buffer_per_track_raw(project, soundfont_path, spec.sample_rate, None)?;
    let output_gain = 10.0_f32.powf(output_gain_db / 20.0);
    for sample in &mut audio {
        *sample *= output_gain;
    }
    normalise_for_export(project, &mut audio, spec.sample_rate)?;
    encode::encode_to_file(output_path, &spec, &audio)
}

/// Render through plugin-aware track backends and AudioGraph. Tracks that
/// reference one external preset share its stateful instance and may fan real
/// output pairs into separate track buffers through RenderCache.
pub fn render_to_wav_per_track_with_plugins(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    render_to_wav_per_track_with_plugins_at(
        project,
        soundfont_path,
        output_path,
        output_gain_db,
        plugin_instances,
        None,
        spec,
    )
}

/// Render a plugin-aware per-track WAV with an explicit project resource root.
pub fn render_to_wav_per_track_with_plugins_at(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    render_to_wav_per_track_with_plugins_at_minimum_duration(
        project,
        soundfont_path,
        output_path,
        output_gain_db,
        plugin_instances,
        resource_root,
        0.0,
        spec,
    )
}

// Public render entry point; changing the signature would break callers.
#[allow(clippy::too_many_arguments)]
/// Export at least this duration without padding or cutting musical events.
pub fn render_to_wav_per_track_with_plugins_at_minimum_duration(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    render_to_wav_per_track_with_plugins_at_minimum_duration_with_pre_roll(
        project,
        soundfont_path,
        output_path,
        output_gain_db,
        plugin_instances,
        resource_root,
        minimum_duration_seconds,
        0.0,
        spec,
    )
}

// Public render entry point; changing the signature would break callers.
#[allow(clippy::too_many_arguments)]
/// Export with a wall-clock warm-up before each plugin-backed render.
pub fn render_to_wav_per_track_with_plugins_at_minimum_duration_with_pre_roll(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    pre_roll_seconds: f64,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    let mut options = RenderOptions::new(spec.sample_rate, Default::default());
    options.output_gain_db = output_gain_db;
    options.resource_root = resource_root;
    options.minimum_duration_seconds = minimum_duration_seconds;
    options.pre_roll_seconds = pre_roll_seconds;
    render_to_wav_with_options_and_cache(
        project,
        soundfont_path,
        output_path,
        plugin_instances,
        options,
        spec,
        crate::render_cache::RenderCache::new(),
    )
}

/// Keep external metadata preparation injectable without replacing rendering or
/// file publication. The cache is fresh; the render owner configures its options.
pub(crate) fn render_to_wav_with_options_and_cache(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    options: RenderOptions<'_>,
    spec: EncodeSpec,
    cache: crate::render_cache::RenderCache,
) -> Result<EncodeReport, String> {
    if options.sample_rate != spec.sample_rate {
        return Err("Render and encoder sample rates must match".into());
    }
    let (audio, _cache) = render_full_for_export_with_options_and_cache(
        project,
        soundfont_path,
        plugin_instances,
        options,
        cache,
    )
    .map_err(|error| error.to_string())?;
    let write_start = std::time::Instant::now();
    let report = encode::encode_to_file(output_path, &spec, &audio)?;
    crate::stage_log::push("wav write", write_start);
    Ok(report)
}

/// Full mix for export: normalised samples plus the clean per-track cache.
/// Callers own the cache, so they can publish it after an off-lock render.
#[allow(clippy::too_many_arguments)]
pub fn render_full_for_export(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    pre_roll_seconds: f64,
) -> Result<(Vec<f32>, crate::render_cache::RenderCache), String> {
    render_full_for_export_with_cancel(
        project,
        soundfont_path,
        sample_rate,
        output_gain_db,
        plugin_instances,
        resource_root,
        minimum_duration_seconds,
        pre_roll_seconds,
        None,
        None,
    )
    .map_err(|error| error.to_string())
}

/// The same render with a cancellation token and a progress sink.
///
/// A token cancelled before or during the render returns
/// `RenderError::Cancelled` and no audio, so the caller can report a
/// cancelled export instead of a failure.
#[allow(clippy::too_many_arguments)]
pub fn render_full_for_export_with_cancel(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    output_gain_db: f32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    pre_roll_seconds: f64,
    cancel: Option<crate::render_cancel::CancelToken>,
    progress: Option<crate::render_cancel::ProgressCallback>,
) -> Result<(Vec<f32>, crate::render_cache::RenderCache), crate::render_cancel::RenderError> {
    render_full_for_export_with_options(
        project,
        soundfont_path,
        plugin_instances,
        RenderOptions {
            sample_rate,
            output_gain_db,
            resource_root,
            minimum_duration_seconds,
            pre_roll_seconds,
            cancel,
            progress,
            workers: crate::render_parallel::RenderWorkerOptions::default(),
        },
    )
}

/// Render using the caller's immutable configuration snapshot.
pub fn render_full_for_export_with_options(
    project: &Project,
    soundfont_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    options: RenderOptions<'_>,
) -> Result<(Vec<f32>, crate::render_cache::RenderCache), crate::render_cancel::RenderError> {
    render_full_for_export_with_options_and_cache(
        project,
        soundfont_path,
        plugin_instances,
        options,
        crate::render_cache::RenderCache::new(),
    )
}

/// Render with a fresh cache carrying explicitly prepared external metadata.
/// The caller supplies no prior render state. All standard render options are
/// configured here, and `render_full` rebuilds the audio through the same path
/// as the default entry point; metadata injection does not replace rendering.
pub fn render_full_for_export_with_options_and_cache(
    project: &Project,
    soundfont_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    options: RenderOptions<'_>,
    mut cache: crate::render_cache::RenderCache,
) -> Result<(Vec<f32>, crate::render_cache::RenderCache), crate::render_cancel::RenderError> {
    let RenderOptions {
        sample_rate,
        output_gain_db,
        resource_root,
        minimum_duration_seconds,
        pre_roll_seconds,
        cancel,
        progress,
        workers,
    } = options;
    use crate::render_cancel::{CancelToken, RenderError};

    if !output_gain_db.is_finite() || output_gain_db > 0.0 {
        return Err(RenderError::Message(
            "Per-track offline output gain must be finite and non-positive".into(),
        ));
    }
    cache.set_worker_options(workers);
    cache.set_pattern_previews(false);
    cache
        .set_minimum_duration(minimum_duration_seconds)
        .map_err(RenderError::Message)?;
    cache.set_resource_root(resource_root.map(Path::to_path_buf));
    cache
        .set_pre_roll_seconds(pre_roll_seconds)
        .map_err(RenderError::Message)?;
    if let Some(token) = &cancel {
        cache.set_cancel_token(token.clone());
    }
    if let Some(progress) = progress {
        cache.set_progress(progress);
    }
    let mut audio = match cache.render_full(project, soundfont_path, sample_rate, plugin_instances)
    {
        Ok(audio) => audio,
        Err(error) => {
            cache.clear_render_controls();
            return Err(if cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                RenderError::Cancelled
            } else {
                RenderError::Message(error)
            });
        }
    };
    cache.clear_render_controls();
    let output_gain = 10.0_f32.powf(output_gain_db / 20.0);
    let export_start = std::time::Instant::now();
    for sample in &mut audio {
        *sample *= output_gain;
    }
    normalise_for_export(project, &mut audio, sample_rate).map_err(RenderError::Message)?;
    if cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
        return Err(RenderError::Cancelled);
    }
    crate::stage_log::push("output gain + loudness", export_start);
    Ok((audio, cache))
}

/// Render the project through the legacy per-track instrument path.
///
/// This path supports source-owned effect sidechains and is used by CLI
/// playback. Plugin-backed projects use `render_to_buffer_with_plugins`.
pub fn render_to_buffer_per_track(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
) -> Result<Vec<f32>, String> {
    render_to_buffer_per_track_at(project, soundfont_path, sample_rate, None)
}

pub(crate) fn render_to_buffer_per_track_at(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vec<f32>, String> {
    let mut audio =
        render_to_buffer_per_track_raw(project, soundfont_path, sample_rate, resource_root)?;
    normalise_for_export(project, &mut audio, sample_rate)?;
    Ok(audio)
}

fn render_to_buffer_per_track_raw(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vec<f32>, String> {
    let resolved = oc_dsl::preset::resolve(project)?;
    let expanded = oc_dsl::arrangement::expand(&resolved)?;
    let project = &expanded;
    validate_gain_project(project)?;
    if project.audio_clip_count() > 0 {
        return Err(
            "Audio clips require the per-track render path with a project resource root".into(),
        );
    }
    let mut neutral = project.clone();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    for track in &mut neutral.tracks {
        track.mix.volume_db = 0.0;
        track.mix.pan = 0.0;
        track.mix.mute = false;
    }
    let settings = InterpretSettings {
        sample_rate,
        ..Default::default()
    };
    let events = interpret::try_interpret_at(&neutral, &settings, resource_root)?;
    if events.is_empty() {
        return Err("No events to render".into());
    }
    let last_event_time = events
        .iter()
        .map(|event| event.time_samples)
        .max()
        .unwrap_or(0);
    let total_duration_seconds =
        (last_event_time + sample_rate as u64 * 2) as f64 / sample_rate as f64;
    let total_frames = (total_duration_seconds * sample_rate as f64).ceil() as usize;
    let total_frames = total_frames.max(
        oc_dsl::arrangement::rendered_end_sample(project, sample_rate)?
            .unwrap_or(0)
            .try_into()
            .map_err(|_| "arrangement length exceeds platform size".to_string())?,
    );
    let mut graph = AudioGraph::try_from_project_with_source_latencies(project, sample_rate, &[])?;
    // Render the master chain's latency as extra frames, then shift it back.
    let master_shift = graph.master_chain.latency_samples() as usize * 2;
    let render_frames = total_frames + master_shift / 2;
    let render_samples = render_frames * 2;
    let mut track_buffers = Vec::with_capacity(project.tracks.len());
    for track_index in 0..project.tracks.len() {
        let track_events: Vec<_> = events
            .iter()
            .filter(|event| event.track_index == track_index)
            .cloned()
            .collect();
        if track_events.is_empty() {
            track_buffers.push(vec![0.0; render_samples]);
            continue;
        }
        let track = &project.tracks[track_index];
        let preset = track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|preset| preset.name == name));
        let plugin_id = preset.and_then(|preset| preset.plugin.as_deref());
        let is_builtin = preset.and_then(|preset| preset.source.as_deref()) == Some("builtin")
            || plugin_id
                .map(|plugin_id| plugin_id.starts_with("builtin."))
                .unwrap_or(false);
        if is_builtin {
            let buffer = crate::render_cache::render_builtin_project(
                plugin_id.unwrap_or("builtin.test_tone"),
                Some(&crate::preset::builtin_track_params_at(
                    project,
                    preset,
                    track,
                    resource_root,
                )),
                &track_events.iter().collect::<Vec<_>>(),
                render_frames,
                0,
                sample_rate,
                project.properties.tempo,
                resource_root,
                Some(&clock),
                None,
            )?;
            track_buffers.push(buffer);
            continue;
        }
        // The legacy path plays every non-builtin track through the fallback
        // bank and never resolves a preset file, so an empty path is a missing
        // bank, not an I/O failure on a path the project named.
        if soundfont_path.as_os_str().is_empty() {
            return Err(format!(
                "Track '{}' needs a SoundFont: no fallback bank was supplied. Pass --soundfont <path>.sf2, or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
                track.name
            ));
        }
        let mut renderer = SoundFontRenderer::new(sample_rate, soundfont_path)?;
        renderer.apply_preset_output(preset);
        if track.instrument.category == "drums" {
            renderer.enable_drum_channel();
        }
        let dynamic_layers = crate::preset::dynamic_layers_enabled(preset, track);
        renderer.set_dynamic_layers(dynamic_layers);
        if dynamic_layers {
            renderer.set_dynamic_ramps(crate::soundfont::dynamic_layer_ramps(
                project,
                track,
                &project.properties,
                sample_rate,
                0,
            )?);
        }
        let mut buffer = renderer.try_render_events(&track_events, total_duration_seconds)?;
        buffer.resize(render_samples, 0.0);
        track_buffers.push(buffer);
    }
    // The legacy path renders instruments itself, but plugin effects in the
    // graph still need the mapped clock when the project has one.
    let transport = (!project.properties.tempo_map.is_empty()
        || !project.properties.time_sig_map.is_empty())
    .then(|| crate::render_cache::project_transport(project, 0, sample_rate));
    crate::processing_failure::graph(&graph, None).map_err(|error| error.to_string())?;
    let mut mixed = graph.mix_with_transport(&mut track_buffers, transport);
    // This legacy source path still owns its native graph inserts. Stop their
    // processing on the same worker and verify the shared failure authority
    // before any buffer can reach normalisation or file publication.
    for chain in &mut graph.track_chains {
        chain.end_processing();
    }
    for bus in graph.bus_chains.values_mut() {
        bus.chain.end_processing();
    }
    graph.master_chain.end_processing();
    crate::processing_failure::graph(&graph, None).map_err(|error| error.to_string())?;
    mixed.copy_within(master_shift.., 0);
    mixed.truncate(total_frames * 2);
    Ok(mixed)
}

/// Render a plugin-aware WAV with an explicit project resource root.
pub fn render_to_wav_with_plugins_at(
    project: &Project,
    soundfont_path: &Path,
    output_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    spec: EncodeSpec,
) -> Result<EncodeReport, String> {
    let audio = render_to_buffer_with_plugins_at(
        project,
        soundfont_path,
        spec.sample_rate,
        plugin_instances,
        resource_root,
    )?;
    encode::encode_to_file(output_path, &spec, &audio)
}

/// Render a project to an in-memory stereo f32 buffer.
/// Used by both offline WAV export and playback pre-render.
///
/// The legacy SoundFont fallback is a track source only: fader, pan, mute,
/// solo, sends, buses and master FX all run through the project AudioGraph.
pub fn render_to_buffer(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
) -> Result<Vec<f32>, String> {
    let _timer = oc_diag::perf_counter("render.buffer.ms").map(oc_diag::Timer::start);
    oc_diag::diag_info!(
        "render: project={:?} tracks={} sample_rate={}",
        project.name.as_deref().unwrap_or("untitled"),
        project.tracks.len(),
        sample_rate
    );
    let audio = render_to_buffer_per_track(project, soundfont_path, sample_rate)?;
    oc_diag::diag_info!(
        "render: frames={} seconds={:.3}",
        audio.len() / 2,
        audio.len() as f64 / 2.0 / f64::from(sample_rate)
    );
    Ok(audio)
}

/// Render a project with plugin support through the authoritative RenderCache
/// path, including shared instruments, output fan-out, effects, buses, and PDC.
pub fn render_to_buffer_with_plugins(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
) -> Result<Vec<f32>, String> {
    render_to_buffer_with_plugins_at(project, soundfont_path, sample_rate, plugin_instances, None)
}

/// Render a plugin-aware buffer with an explicit project resource root.
pub fn render_to_buffer_with_plugins_at(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
) -> Result<Vec<f32>, String> {
    let options = RenderOptions {
        sample_rate,
        output_gain_db: 0.0,
        resource_root,
        minimum_duration_seconds: 0.0,
        pre_roll_seconds: 0.0,
        cancel: None,
        progress: None,
        workers: crate::render_parallel::RenderWorkerOptions::default(),
    };
    render_full_for_export_with_options(project, soundfont_path, plugin_instances, options)
        .map(|(audio, _)| audio)
        .map_err(|error| error.to_string())
}

/// Render one track's post-insert, pre-fader signal over a beat range.
///
/// The pass is a timeline slice, not a solo window: the graph runs from the
/// project start, so effect state and latency handling match the full mix at
/// the same beat. Sends, buses and master are excluded. Mute and solo do not
/// gate the tap, because the caller asked for this track's own signal.
///
/// Returns interleaved stereo samples for exactly `[from_beat, to_beat)`.
#[allow(clippy::too_many_arguments)]
pub fn render_track_post_fx_range(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    track: usize,
    from_beat: f64,
    to_beat: f64,
) -> Result<Vec<f32>, String> {
    if track >= project.tracks.len() {
        return Err(format!("track {track} out of range"));
    }
    if !from_beat.is_finite() || !to_beat.is_finite() {
        return Err("bounce range must be finite".into());
    }
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let start_sample = clock.sample_at_quarter(from_beat, sample_rate)?;
    let end_sample = clock.sample_at_quarter(to_beat, sample_rate)?;
    render_track_post_fx_frame_range(
        project,
        soundfont_path,
        sample_rate,
        plugin_instances,
        resource_root,
        track,
        start_sample,
        end_sample,
    )
}

/// Sample-exact counterpart of [`render_track_post_fx_range`].
/// Returns the track tap over `[start_sample, end_sample)` without converting
/// authored audio boundaries back through the musical grid.
#[allow(clippy::too_many_arguments)]
pub fn render_track_post_fx_frame_range(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    track: usize,
    start_sample: u64,
    end_sample: u64,
) -> Result<Vec<f32>, String> {
    render_track_post_fx_with_options(
        project,
        soundfont_path,
        plugin_instances,
        TrackRenderRange {
            track,
            start_sample,
            end_sample,
        },
        RenderOptions {
            resource_root,
            ..RenderOptions::new(
                sample_rate,
                crate::render_parallel::RenderWorkerOptions::default(),
            )
        },
    )
}

/// Render a track tap with an explicit worker snapshot.
pub fn render_track_post_fx_with_options(
    project: &Project,
    soundfont_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    range: TrackRenderRange,
    options: RenderOptions<'_>,
) -> Result<Vec<f32>, String> {
    let TrackRenderRange {
        track,
        start_sample,
        end_sample,
    } = range;
    let RenderOptions {
        sample_rate,
        resource_root,
        workers,
        ..
    } = options;
    if track >= project.tracks.len() {
        return Err(format!("track {track} out of range"));
    }
    if sample_rate == 0 || end_sample <= start_sample {
        return Err("bounce needs a positive sample rate and an end after its start".into());
    }
    let mut neutral;
    let project = if project.tracks[track].mix.mute || project.tracks.iter().any(|t| t.mix.solo) {
        neutral = project.clone();
        for track in &mut neutral.tracks {
            track.mix.solo = false;
        }
        neutral.tracks[track].mix.mute = false;
        &neutral
    } else {
        project
    };
    let mut cache = crate::render_cache::RenderCache::new();
    cache.set_worker_options(workers);
    cache.set_pattern_previews(false);
    cache.set_resource_root(resource_root.map(Path::to_path_buf));
    cache.render_track_tap_range(
        project,
        soundfont_path,
        sample_rate,
        plugin_instances,
        track,
        start_sample,
        end_sample,
    )
}

/// Seconds a bounce renders past its range end so the tail reaches the media.
///
/// The tail covers the insert chain's serial delay, the ring-out the chain
/// declares, the decay a reverb declares for a bounce only, and the release the
/// track's instrument declares. A track that plays notes but declares no release
/// gets [`ASSUMED_RELEASE_SECONDS`]. The sum is capped at
/// [`MAX_BOUNCE_TAIL_SECONDS`], so one long chain cannot make a bounce
/// unbounded.
pub fn bounce_tail_seconds(
    project: &Project,
    track: usize,
    sample_rate: u32,
    plugin_instances: &HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
) -> Result<f64, String> {
    let chain = crate::graph::track_chain_tail_seconds(project, track, sample_rate)?;
    let release = match declared_release_seconds(
        project,
        track,
        sample_rate,
        plugin_instances,
        resource_root,
    ) {
        Some(seconds) => seconds,
        None if plays_notes(project, track) => ASSUMED_RELEASE_SECONDS,
        None => 0.0,
    };
    Ok(
        (chain.latency + chain.ring + chain.bounce_ring + release.max(0.0))
            .min(MAX_BOUNCE_TAIL_SECONDS),
    )
}

/// Release one track's instrument declares, in seconds; `None` when it declares
/// none.
///
/// Reads the document where the release is a parameter (built-in synth, sampler
/// and drum kit) and the loaded instance where a hosted instrument reports its
/// own tail. An instrument that keeps its envelope inside the patch reports
/// nothing here.
fn declared_release_seconds(
    project: &Project,
    track: usize,
    sample_rate: u32,
    plugin_instances: &HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
) -> Option<f64> {
    let track = project.tracks.get(track)?;
    let preset = track
        .preset
        .as_deref()
        .and_then(|name| project.presets.iter().find(|preset| preset.name == name))?;
    match preset.source.as_deref() {
        Some("builtin") if preset.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID) => {
            let params =
                crate::preset::builtin_track_params_untrimmed(project, Some(preset), track);
            params
                .iter()
                .find(|(name, _)| name == "amp.release" || name == "release")
                .and_then(|(_, value)| value.as_f64())
        }
        Some("builtin") if preset.plugin.as_deref() == Some(oc_synth::sampler::PLUGIN_ID) => {
            oc_synth::sampler::params_tail_seconds(&preset.params, resource_root)
        }
        Some("builtin") if preset.plugin.as_deref() == Some(oc_synth::drumkit::PLUGIN_ID) => {
            oc_synth::drumkit::params_tail_seconds(&preset.params, resource_root)
        }
        Some("clap") => plugin_instances
            .get(&preset.name)
            .map(|instance| f64::from(instance.tail_samples()) / f64::from(sample_rate.max(1))),
        _ => None,
    }
}

/// True when the track plays note events rather than audio clips only.
fn plays_notes(project: &Project, track: usize) -> bool {
    let Some(track) = project.tracks.get(track) else {
        return false;
    };
    !track.phrases.is_empty()
        || track.pattern_clip_count() > 0
        || !track.patterns.is_empty()
        || track.performance.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Whether a stem is a track tap or a bus return.
pub enum StemRole {
    Track,
    Bus,
}

#[derive(Debug, Clone)]
/// One rendered stem and its routing role.
pub struct StemExportBuffer {
    pub name: String,
    pub role: StemRole,
    pub samples: Vec<f32>,
    /// Track output target, or the bus route that feeds the master.
    pub output: Option<String>,
    /// True when this stem is one summand of the pre-master mix.
    pub mix_sum_member: bool,
}

#[derive(Debug)]
/// A full mix and its pre-master stems from one graph pass.
pub struct StemsExport {
    pub mix: Vec<f32>,
    pub stems: Vec<StemExportBuffer>,
    pub sample_rate: u32,
    pub loudness: Option<LoudnessReport>,
}

/// Render a full mix and its pre-master stems from one graph pass.
///
/// Track stems are direct post-fader, post-track-FX outputs before sends. Bus
/// returns are separate stems. Only `mix_sum_member` stems sum to the
/// pre-master mix, so a track feeding a bus is not counted twice.
pub fn render_with_stems(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    pre_roll_seconds: f64,
) -> Result<StemsExport, String> {
    render_with_stems_with_cancel(
        project,
        soundfont_path,
        sample_rate,
        plugin_instances,
        resource_root,
        minimum_duration_seconds,
        pre_roll_seconds,
        None,
        None,
    )
    .map_err(|error| error.to_string())
}

/// The same stem render with a cancellation token and a progress sink.
#[allow(clippy::too_many_arguments)]
pub fn render_with_stems_with_cancel(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    minimum_duration_seconds: f64,
    pre_roll_seconds: f64,
    cancel: Option<crate::render_cancel::CancelToken>,
    progress: Option<crate::render_cancel::ProgressCallback>,
) -> Result<StemsExport, crate::render_cancel::RenderError> {
    render_with_stems_with_options(
        project,
        soundfont_path,
        plugin_instances,
        RenderOptions {
            sample_rate,
            output_gain_db: 0.0,
            resource_root,
            minimum_duration_seconds,
            pre_roll_seconds,
            cancel,
            progress,
            workers: crate::render_parallel::RenderWorkerOptions::default(),
        },
    )
}

/// Render using the caller's immutable configuration snapshot.
pub fn render_with_stems_with_options(
    project: &Project,
    soundfont_path: &Path,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    options: RenderOptions<'_>,
) -> Result<StemsExport, crate::render_cancel::RenderError> {
    let RenderOptions {
        sample_rate,
        output_gain_db: _,
        resource_root,
        minimum_duration_seconds,
        pre_roll_seconds,
        cancel,
        progress,
        workers,
    } = options;
    use crate::render_cancel::{CancelToken, RenderError};

    let mut cache = crate::render_cache::RenderCache::new();
    cache.set_worker_options(workers);
    cache
        .set_minimum_duration(minimum_duration_seconds)
        .map_err(RenderError::Message)?;
    cache.set_resource_root(resource_root.map(Path::to_path_buf));
    cache
        .set_pre_roll_seconds(pre_roll_seconds)
        .map_err(RenderError::Message)?;
    cache.capture_stems(true);
    if let Some(token) = &cancel {
        cache.set_cancel_token(token.clone());
    }
    if let Some(progress) = progress {
        cache.set_progress(progress);
    }
    let rendered = cache.render_full(project, soundfont_path, sample_rate, plugin_instances);
    cache.clear_render_controls();
    let mut mix = match rendered {
        Ok(mix) => mix,
        Err(error) => {
            return Err(if cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                RenderError::Cancelled
            } else {
                RenderError::Message(error)
            });
        }
    };
    let capture = cache
        .stems()
        .cloned()
        .ok_or_else(|| RenderError::Message("render completed without stem capture".into()))?;
    let loudness =
        normalise_loudness(project, &mut mix, sample_rate).map_err(RenderError::Message)?;
    let frames = mix.len();
    let labels: HashSet<String> = capture
        .master_inputs
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let mut stems = Vec::with_capacity(capture.tracks.len() + capture.buses.len());
    for (name, mut samples) in capture.tracks {
        samples.resize(frames, 0.0);
        samples.truncate(frames);
        stems.push(StemExportBuffer {
            output: Some(
                project
                    .tracks
                    .iter()
                    .find(|track| track.name == name)
                    .and_then(|track| track.mix.output.clone())
                    .unwrap_or_else(|| "master".to_string()),
            ),
            mix_sum_member: labels.contains(&format!("track:{name}")),
            name,
            role: StemRole::Track,
            samples,
        });
    }
    for (name, mut samples) in capture.buses {
        samples.resize(frames, 0.0);
        samples.truncate(frames);
        stems.push(StemExportBuffer {
            output: project
                .buses
                .iter()
                .find(|bus| bus.name == name)
                .map(|bus| {
                    if bus.routes.is_empty() {
                        "master".to_string()
                    } else {
                        bus.routes.join(",")
                    }
                }),
            mix_sum_member: labels.contains(&format!("bus:{name}")),
            name,
            role: StemRole::Bus,
            samples,
        });
    }
    Ok(StemsExport {
        mix,
        stems,
        sample_rate,
        loudness,
    })
}

const STEM_ROUTING_POLICY: &str = "Track stems are post-fader, post-track-FX direct outputs before sends. Bus returns are separate stems. Only stems with mix_sum_member=true sum to the pre-master mix; a track feeding a bus is represented by the bus return, not by both. Stems are pre-master-fader and pre-loudness, so the exported mix equals their sum after the master fader and master effects.";

#[derive(Serialize)]
struct StemManifest {
    file: String,
    name: String,
    role: &'static str,
    output: Option<String>,
    mix_sum_member: bool,
    frames: usize,
    peak_dbfs: f64,
    rms_dbfs: f64,
    integrated_lufs: Option<f64>,
}

#[derive(Serialize)]
struct MixManifest {
    file: String,
    frames: usize,
    peak_dbfs: f64,
    rms_dbfs: f64,
    integrated_lufs: Option<f64>,
    loudness_target_lufs: Option<f64>,
    applied_gain_db: Option<f64>,
    achieved_lufs: Option<f64>,
    peak_limited: bool,
}

#[derive(Serialize)]
struct RoutingManifest {
    master_fader_db: f64,
    loudness_normalisation: bool,
    policy: &'static str,
}

#[derive(Serialize)]
struct StemsManifest {
    format: &'static str,
    container: &'static str,
    song: String,
    sample_rate: u32,
    channels: u16,
    sample_format: &'static str,
    dithered: bool,
    frames: usize,
    mix: MixManifest,
    routing: RoutingManifest,
    stems: Vec<StemManifest>,
}

/// Reduce a name to file-safe characters, keeping letters and digits from every
/// script. Runs of hostile characters collapse to one dash, and `fallback` names
/// the file when nothing survives.
fn sanitize_stem_component(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            out.extend(character.to_lowercase());
        } else if character == '-' || character == '_' {
            out.push(character);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn unique_stem_name(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn level_fields(samples: &[f32], sample_rate: u32) -> (f64, f64, Option<f64>) {
    let levels = analysis::measure_stereo_interleaved(samples, sample_rate);
    (levels.peak_dbfs, levels.rms_dbfs, levels.integrated_lufs)
}

/// Write the mix, stems, and `stems.json` into `dir`. Returns the manifest path.
///
/// Every stem gets a dither seed derived from its file name, so the set does
/// not sum coherent noise. `spec.sample_rate` must match the rendered set.
pub fn write_stems_export(
    project: &Project,
    dir: &Path,
    song: &str,
    export: &StemsExport,
    spec: EncodeSpec,
) -> Result<std::path::PathBuf, String> {
    spec.validate()?;
    if spec.sample_rate != export.sample_rate {
        return Err(format!(
            "the stem set is {} Hz but the export spec is {} Hz",
            export.sample_rate, spec.sample_rate
        ));
    }
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let song = sanitize_stem_component(song, "song");
    let extension = spec.extension();
    let mut used = HashSet::new();
    used.insert(format!("{song}-mix"));
    let mix_file = format!("{song}-mix.{extension}");
    encode::encode_to_file(&dir.join(&mix_file), &spec, &export.mix)?;
    let frames = export.mix.len() / 2;
    let mut stems = Vec::with_capacity(export.stems.len());
    for stem in &export.stems {
        let role_prefix = match stem.role {
            StemRole::Track => "",
            StemRole::Bus => "bus-",
        };
        let base = unique_stem_name(
            format!(
                "{song}-{role_prefix}{}",
                sanitize_stem_component(&stem.name, "track")
            ),
            &mut used,
        );
        let file = format!("{base}.{extension}");
        // FLAC refuses dither, so the seed is inert there; WAV stems keep
        // decorrelated noise across the set.
        let stem_spec = spec.with_dither_seed(encode::seed_for_name(spec.wav.dither_seed, &base));
        encode::encode_to_file(&dir.join(&file), &stem_spec, &stem.samples)?;
        let (peak_dbfs, rms_dbfs, integrated_lufs) =
            level_fields(&stem.samples, export.sample_rate);
        stems.push(StemManifest {
            file,
            name: stem.name.clone(),
            role: match stem.role {
                StemRole::Track => "track",
                StemRole::Bus => "bus",
            },
            output: stem.output.clone(),
            mix_sum_member: stem.mix_sum_member,
            frames: stem.samples.len() / 2,
            peak_dbfs,
            rms_dbfs,
            integrated_lufs,
        });
    }
    let (peak_dbfs, rms_dbfs, integrated_lufs) = level_fields(&export.mix, export.sample_rate);
    let loudness = export.loudness.as_ref();
    let manifest = StemsManifest {
        format: "opencompose-stems-v1",
        container: spec.container.as_str(),
        song: song.clone(),
        sample_rate: export.sample_rate,
        channels: spec.channels,
        sample_format: spec.depth().as_str(),
        dithered: spec.dither(),
        frames,
        mix: MixManifest {
            file: mix_file,
            frames,
            peak_dbfs,
            rms_dbfs,
            integrated_lufs,
            loudness_target_lufs: loudness.map(|report| report.target_lufs),
            applied_gain_db: loudness.map(|report| report.applied_gain_db),
            achieved_lufs: loudness.map(|report| report.achieved_lufs),
            peak_limited: loudness.is_some_and(|report| report.limited_by_ceiling),
        },
        routing: RoutingManifest {
            master_fader_db: project
                .master
                .as_ref()
                .map_or(0.0, |master| master.volume_db),
            loudness_normalisation: loudness.is_some(),
            policy: STEM_ROUTING_POLICY,
        },
        stems,
    };
    let manifest_path = dir.join("stems.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    Ok(manifest_path)
}

#[cfg(test)]
mod tests {
    use super::{
        bounce_tail_seconds, render_to_buffer_per_track, render_to_buffer_with_plugins,
        render_to_wav, render_track_post_fx_range, write_stems_export,
    };
    use crate::encode::{EncodeSpec, WavOptions};
    use crate::render_cache::RenderCache;
    use md5::{Digest, Md5};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn prepared_metadata_cache_uses_identical_export_options_and_audio_pipeline() {
        let project = builtin_project(330);
        let options = || {
            let mut options = super::RenderOptions::new(
                12_000,
                crate::render_parallel::RenderWorkerOptions::default(),
            );
            options.output_gain_db = -6.0;
            options.minimum_duration_seconds = 3.0;
            options.pre_roll_seconds = 0.1;
            options
        };
        let (default_audio, default_cache) = super::render_full_for_export_with_options(
            &project,
            Path::new(""),
            &mut HashMap::new(),
            options(),
        )
        .unwrap();
        let (prepared_audio, prepared_cache) =
            super::render_full_for_export_with_options_and_cache(
                &project,
                Path::new(""),
                &mut HashMap::new(),
                options(),
                RenderCache::new(),
            )
            .unwrap();
        assert!(!default_audio.is_empty());
        assert_eq!(prepared_audio, default_audio);
        assert_eq!(prepared_cache.is_dirty(), default_cache.is_dirty());
        assert!(!prepared_cache.is_dirty());
    }

    fn default_spec(sample_rate: u32) -> EncodeSpec {
        EncodeSpec {
            container: oc_dsl::ast::ExportContainer::Wav,
            wav: WavOptions::default(),
            flac: crate::encode::FlacOptions::default(),
            mp3: crate::encode::Mp3Options::default(),
            sample_rate,
            channels: 2,
        }
    }

    fn builtin_project(frequency: u32) -> oc_dsl::ast::Project {
        let source = format!(
            r#"@ocp "0.2.0"

@project "builtin parameter test"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: {frequency}, amplitude: 0.2, waveform: 0}}

@track tone_track | preset: tone, instrument: synth/lead_square
  @phrase held [1:1:0 - 2:1:0]
    C4 w mf
"#
        );
        oc_dsl::parse(&source).expect("builtin project should parse")
    }

    fn long_native_project(bars: usize) -> oc_dsl::ast::Project {
        let mut notes = String::new();
        for _ in 0..bars {
            notes.push_str("    C4 w mf\n");
        }
        let source = format!(
            r#"@ocp "0.2.0"

@project "cancellation test"
  tempo: 120
  time_signature: 4/4

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track keys | preset: lead, instrument: synth/virtual_analog
  @phrase line [1:1 - {}:1]
{notes}"#,
            bars + 1
        );
        oc_dsl::parse(&source).expect("native project should parse")
    }

    #[test]
    fn explicit_worker_options_preserve_mix_stem_and_tap_samples() {
        use super::{RenderOptions, TrackRenderRange};
        use crate::render_parallel::RenderWorkerOptions;
        let mut project = builtin_project(440);
        let mut second = project.tracks[0].clone();
        second.name = "second_tone".into();
        project.tracks.push(second);
        let serial = RenderWorkerOptions::from_pin(Some("1"), || 8);
        let parallel = RenderWorkerOptions::from_pin(Some("2"), || 8);
        let render = |workers| {
            super::render_full_for_export_with_options(
                &project,
                Path::new(""),
                &mut HashMap::new(),
                RenderOptions::new(12_000, workers),
            )
            .unwrap()
            .0
        };
        let expected = render(serial);
        assert!(expected.iter().any(|sample| *sample != 0.0));
        assert_eq!(expected, render(parallel));
        let stems = super::render_with_stems_with_options(
            &project,
            Path::new(""),
            &mut HashMap::new(),
            RenderOptions::new(12_000, parallel),
        )
        .unwrap();
        assert_eq!(stems.mix, expected);
        assert_eq!(stems.stems.len(), 2);
        let tap = |workers| {
            super::render_track_post_fx_with_options(
                &project,
                Path::new(""),
                &mut HashMap::new(),
                TrackRenderRange {
                    track: 0,
                    start_sample: 0,
                    end_sample: 1200,
                },
                RenderOptions::new(12_000, workers),
            )
            .unwrap()
        };
        assert_eq!(tap(serial), tap(parallel));
    }

    #[test]
    fn explicit_worker_options_preserve_cancellation_and_validation_errors() {
        use super::RenderOptions;
        use crate::render_cancel::{CancelToken, RenderError};
        let project = builtin_project(440);
        for count in ["1", "2"] {
            let workers = crate::render_parallel::RenderWorkerOptions::from_pin(Some(count), || 8);
            let token = CancelToken::new();
            token.cancel();
            let mut options = RenderOptions::new(12_000, workers);
            options.cancel = Some(token.clone());
            assert!(matches!(
                super::render_full_for_export_with_options(
                    &project,
                    Path::new(""),
                    &mut HashMap::new(),
                    options
                ),
                Err(RenderError::Cancelled)
            ));
            let mut options = RenderOptions::new(12_000, workers);
            options.cancel = Some(token);
            assert!(matches!(
                super::render_with_stems_with_options(
                    &project,
                    Path::new(""),
                    &mut HashMap::new(),
                    options
                ),
                Err(RenderError::Cancelled)
            ));
            let mut options = RenderOptions::new(12_000, workers);
            options.output_gain_db = f32::NAN;
            assert!(matches!(super::render_full_for_export_with_options(
                &project, Path::new(""), &mut HashMap::new(), options
            ), Err(RenderError::Message(message)) if message.contains("finite")));
        }
    }

    /// Cancelling after the first source chunk returns the typed outcome.
    #[test]
    fn a_cancelled_render_returns_a_typed_cancelled() {
        use crate::render_cancel::{CancelToken, RenderError};

        let project = long_native_project(8);
        let token = CancelToken::new();
        let from_progress = token.clone();
        let mut instances = HashMap::new();
        let result = super::render_full_for_export_with_cancel(
            &project,
            Path::new("unused.sf2"),
            48_000,
            0.0,
            &mut instances,
            None,
            0.0,
            0.0,
            Some(token),
            Some(Box::new(move |_| from_progress.cancel())),
        );
        assert!(matches!(result, Err(RenderError::Cancelled)));
    }

    /// A render cancelled from another thread stops inside one 256-frame block.
    #[test]
    fn a_cancelled_native_render_stops_within_a_block() {
        use crate::render_cancel::{CancelToken, RenderError};
        use std::time::{Duration, Instant};

        let project = long_native_project(60);
        let token = CancelToken::new();
        let render_token = token.clone();
        let handle = std::thread::spawn(move || {
            let mut instances = HashMap::new();
            super::render_full_for_export_with_cancel(
                &project,
                Path::new("unused.sf2"),
                48_000,
                0.0,
                &mut instances,
                None,
                0.0,
                0.0,
                Some(render_token),
                None,
            )
        });
        // Wait until the render is past its one-time setup and inside the
        // source loop, which is the state the dialog's Cancel sees mid-export.
        std::thread::sleep(Duration::from_millis(500));
        let cancelled_at = Instant::now();
        token.cancel();
        let result = handle.join().expect("render thread must not panic");
        let latency = cancelled_at.elapsed();
        eprintln!("measured cancel latency: {latency:?}");
        assert!(matches!(result, Err(RenderError::Cancelled)));
        assert!(
            latency < Duration::from_millis(100),
            "cancel took {latency:?}, more than one block"
        );
    }

    #[test]
    fn explicit_export_bounds_cover_rests_and_invalidate_cached_length() {
        let project = builtin_project(220);
        let mut cache = RenderCache::new();
        let mut instances = HashMap::new();
        let sf = Path::new("unused.sf2");
        let original = cache
            .render_full(&project, sf, 8000, &mut instances)
            .unwrap();
        assert!(original.len() < 8 * 8000 * 2);
        cache.set_minimum_duration(8.0).unwrap();
        assert!(cache.is_dirty());
        let full = cache
            .render_full(&project, sf, 8000, &mut instances)
            .unwrap();
        assert_eq!(full.len(), 8 * 8000 * 2);
        assert_eq!(&full[..original.len()], original.as_slice());
        cache.set_minimum_duration(0.0).unwrap();
        let restored = cache
            .render_full(&project, sf, 8000, &mut instances)
            .unwrap();
        assert_eq!(restored, original);
        for invalid in [-1.0, 601.0, f64::NAN, f64::INFINITY] {
            assert!(cache.set_minimum_duration(invalid).is_err());
        }
    }

    #[test]
    fn mapped_master_automation_changes_real_audio_only_at_declared_boundaries() {
        let mut project = builtin_project(220);
        project.properties.tempo_map.push(oc_dsl::ast::TempoPoint {
            position: oc_dsl::ast::BarBeat::new(1, 3, 0),
            offset_quarters: 0.0,
            bpm: 60.0,
        });
        let sf = Path::new("unused.sf2");
        let dry = RenderCache::new()
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        let curves = oc_dsl::parse(
            "@project\n@master\n  gain_automation: step [1:2 - 2:1] {1:2=0, 1:3=0.5}\n",
        )
        .unwrap();
        project.master = curves.master;
        assert!(oc_dsl::validate(&project).is_ok());
        let mut cache = RenderCache::new();
        cache.capture_stems(true);
        let wet = cache
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        assert_eq!(wet.len(), dry.len());
        assert_eq!(cache.stems().unwrap().pre_master, dry);
        for (frame, (a, b)) in wet.chunks_exact(2).zip(dry.chunks_exact(2)).enumerate() {
            let factor = if !(4000..24000).contains(&frame) {
                1.0
            } else if frame < 8000 {
                0.0
            } else {
                0.5
            };
            assert_eq!(a, &[b[0] * factor, b[1] * factor], "frame {frame}");
        }
        assert_eq!(wet, render_to_buffer_per_track(&project, sf, 8000).unwrap());
    }

    #[test]
    fn mapped_tone_render_and_stems_obey_exact_note_boundaries() {
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
  tempo_map:
    1:3 = 60 bpm
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 250, amplitude: 0.2}
@track tone_track | preset: tone, instrument: synth/lead_square
  @phrase held [1:2 - 2:1]
    C4 h mf
"#,
        )
        .unwrap();
        let sf = Path::new("unused.sf2");
        let mut cache = RenderCache::new();
        cache.capture_stems(true);
        let audio = cache
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        // Quarter 1 = 0.5s; quarter 3 = 2s after the tempo change at quarter 2.
        assert_eq!(audio.len(), 4 * 8000 * 2);
        assert!(audio[..4000 * 2].iter().all(|sample| *sample == 0.0));
        assert!(
            audio[4000 * 2..4016 * 2]
                .iter()
                .any(|sample| sample.abs() > 0.001)
        );
        assert!(audio[16000 * 2..].iter().all(|sample| *sample == 0.0));
        assert_eq!(cache.stems().unwrap().pre_master, audio);
        assert_eq!(
            render_to_buffer_per_track(&project, sf, 8000).unwrap(),
            audio
        );
    }

    #[test]
    fn mapped_plugin_clock_is_accepted_and_invalid_maps_still_error() {
        use oc_dsl::ast::{Effect, EffectType};
        let mut project = builtin_project(220);
        project.properties.tempo_map.push(oc_dsl::ast::TempoPoint {
            position: oc_dsl::ast::BarBeat::new(1, 3, 0),
            offset_quarters: 0.0,
            bpm: 60.0,
        });
        // The timing preflight accepts AU instruments and effects; the graph and
        // source lanes carry the clock to them.
        project.presets[0].source = Some("au".into());
        project.presets[0].plugin = Some("Apple: DLSMusicDevice".into());
        project.tracks[0].effects.push(Effect {
            effect_type: EffectType::Plugin("au.Apple: AUReverb2".into()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        });
        super::validate_render_timing(&project).expect("mapped AU project");
        project.properties.tempo_map[0].bpm = f64::NAN;
        assert!(
            super::validate_render_timing(&project)
                .unwrap_err()
                .contains("valid tempos")
        );
    }

    /// System AudioUnits are optional on a bare machine; skip loudly when the
    /// registry does not carry the one a test names.
    #[cfg(target_os = "macos")]
    fn system_au_available(query: &str) -> bool {
        match oc_host::au_instance::AuInstance::load(query, 48_000, 512) {
            Ok(_) => true,
            Err(error) => {
                eprintln!("skipping: AudioUnit {query:?} is not available: {error}");
                false
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mapped_au_instrument_renders_with_host_tempo() {
        if !system_au_available("Apple: DLSMusicDevice") {
            return;
        }
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
  tempo_map:
    1:3 = 60 bpm
  time_sig_map:
    2:1 = 3/4
@preset dls
  source: au
  plugin: "Apple: DLSMusicDevice"
@track keys | preset: dls, instrument: keys/piano
  @phrase line [1:1 - 2:2]
    C4 h
    E4 h
"#,
        )
        .unwrap();
        let audio = RenderCache::new()
            .render_full(
                &project,
                Path::new("unused.sf2"),
                48_000,
                &mut HashMap::new(),
            )
            .expect("mapped AU project renders");
        assert!(!audio.is_empty());
        assert!(
            audio.iter().any(|sample| sample.abs() > 1e-3),
            "DLSMusicDevice rendered silence under the mapped clock"
        );
        assert!(audio.iter().all(|sample| sample.is_finite()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mapped_au_effect_renders_with_host_tempo() {
        if !system_au_available("Apple: AUReverb2") {
            return;
        }
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
  tempo_map:
    1:3 = 60 bpm
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2}
@track tone_track | preset: tone, instrument: synth/lead_square
  effects:
    - plugin:"au.Apple: AUReverb2"
  @phrase held [1:1 - 2:1]
    C4 w
"#,
        )
        .unwrap();
        let audio = RenderCache::new()
            .render_full(
                &project,
                Path::new("unused.sf2"),
                48_000,
                &mut HashMap::new(),
            )
            .expect("mapped AU effect project renders");
        assert!(!audio.is_empty());
        assert!(
            audio.iter().any(|sample| sample.abs() > 1e-3),
            "the AU effect chain rendered silence under the mapped clock"
        );
        assert!(audio.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn unavailable_effect_rejects_authoritative_render_at_every_graph_stage() {
        use oc_dsl::ast::{Effect, EffectType};
        let effect = Effect {
            effect_type: EffectType::Plugin("clap.nonexistent-pgen-effect".into()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        };
        for stage in ["track", "preset", "bus", "master"] {
            let mut project = builtin_project(220);
            match stage {
                "track" => project.tracks[0].effects.push(effect.clone()),
                "preset" => project.presets[0].effects.push(effect.clone()),
                "bus" => {
                    project.tracks[0].mix.output = Some("group".into());
                    project.buses = serde_json::from_value(serde_json::json!([{
                        "name":"group", "routes":["master"], "effects":[effect], "gain_automation":[]
                    }])).unwrap();
                }
                _ => {
                    project.master = serde_json::from_value(serde_json::json!({
                        "effects":[effect],"gain_automation":[],"loudness_target":null
                    }))
                    .unwrap()
                }
            }
            let error = RenderCache::new()
                .render_full(&project, Path::new("unused.sf2"), 8000, &mut HashMap::new())
                .unwrap_err();
            assert!(
                error.contains("refusing bypassed render"),
                "{stage}: {error}"
            );
        }
    }

    #[test]
    fn missing_clap_instance_never_substitutes_a_soundfont_instrument() {
        let mut project = builtin_project(220);
        project.presets[0].source = Some("clap".into());
        project.presets[0].plugin = Some("org.example.missing".into());
        project.properties.tempo_map.push(oc_dsl::ast::TempoPoint {
            position: oc_dsl::ast::BarBeat::new(1, 3, 0),
            offset_quarters: 0.0,
            bpm: 60.0,
        });
        let error = RenderCache::new()
            .render_full(
                &project,
                Path::new("missing.sf2"),
                8000,
                &mut HashMap::new(),
            )
            .unwrap_err();
        assert!(error.contains("no loaded instance"), "{error}");
        assert!(error.contains("refusing SoundFont fallback"), "{error}");
    }

    #[test]
    fn mapped_native_graph_stems_match_per_track_and_preserve_master_inputs() {
        let mut project = builtin_project(220);
        project.presets[0].plugin = Some("builtin.open_synth".into());
        project.presets[0].params.clear();
        project.properties.tempo_map.push(oc_dsl::ast::TempoPoint {
            position: oc_dsl::ast::BarBeat::new(1, 3, 0),
            offset_quarters: 0.0,
            bpm: 60.0,
        });
        let mut second = project.tracks[0].clone();
        second.name = "second".into();
        project.tracks.push(second);
        project.tracks[0].mix.output = Some("group".into());
        project.buses = serde_json::from_value(serde_json::json!([{
            "name":"group", "routes":["master"], "effects":[], "gain_automation":[]
        }]))
        .unwrap();
        let sf = Path::new("native-does-not-require-a-soundfont.sf2");
        let mut cache = RenderCache::new();
        cache.capture_stems(true);
        let audio = cache
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        assert!(audio.iter().all(|v| v.is_finite()));
        assert!(audio.iter().any(|v| v.abs() > 0.001));
        assert_eq!(
            audio,
            render_to_buffer_per_track(&project, sf, 8000).unwrap()
        );
        let stems = cache.stems().unwrap();
        assert_eq!(stems.tracks.len(), 2);
        assert_eq!(stems.buses.len(), 1);
        assert_eq!(stems.pre_master, audio);
        let mut sum = vec![0.0; audio.len()];
        for (_, input) in &stems.master_inputs {
            for (sum, value) in sum.iter_mut().zip(input) {
                *sum += value;
            }
        }
        assert_eq!(sum, stems.pre_master);
    }

    #[test]
    fn stem_capture_preserves_master_and_exposes_single_pass_routing() {
        let mut project = builtin_project(220);
        let mut second = project.tracks[0].clone();
        second.name = "second".into();
        project.tracks.push(second);
        project.tracks[0].mix.output = Some("group".into());
        project.buses = serde_json::from_value(serde_json::json!([{
            "name":"group", "routes":["master"], "effects":[], "gain_automation":[]
        }]))
        .unwrap();
        project.master = serde_json::from_value(serde_json::json!({
            // Use an in-range ceiling (-12 dB); checked rendering rejects out-of-domain parameter values.
            "effects":[{"effect_type":"Limiter","params":[["ceiling",{"Db":-12.0}]],"sidechain_source":null}],
            "gain_automation":[],"loudness_target":null
        })).unwrap();
        let sf = Path::new("unused.sf2");
        let mut ordinary = RenderCache::new();
        let expected = ordinary
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        assert!(ordinary.stems().is_none());
        let mut captured = RenderCache::new();
        captured.capture_stems(true);
        let actual = captured
            .render_full(&project, sf, 8000, &mut HashMap::new())
            .unwrap();
        assert_eq!(actual, expected);
        let stems = captured.stems().unwrap();
        assert_eq!(stems.tracks.len(), 2);
        assert_eq!(stems.buses.len(), 1);
        assert_eq!(stems.master_inputs.len(), 2);
        assert_ne!(stems.pre_master, actual); // Master limiting was not applied separately to stems.
        let mut sum = vec![0.0; stems.pre_master.len()];
        for (_, buffer) in &stems.master_inputs {
            for (sum, value) in sum.iter_mut().zip(buffer) {
                *sum += value;
            }
        }
        assert_eq!(sum, stems.pre_master);
        captured.mark_mix_dirty();
        assert!(captured.stems().is_none());
    }

    #[test]
    fn muted_stem_is_zero_and_capture_can_be_disabled() {
        let mut project = builtin_project(220);
        project.tracks[0].mix.mute = true;
        let mut cache = RenderCache::new();
        cache.capture_stems(true);
        cache
            .render_full(&project, Path::new("unused.sf2"), 8000, &mut HashMap::new())
            .unwrap();
        let stems = cache.stems().unwrap();
        assert_eq!(stems.tracks.len(), 1);
        assert!(stems.tracks[0].1.iter().all(|sample| *sample == 0.0));
        assert!(stems.master_inputs.is_empty());
        cache.capture_stems(false);
        assert!(cache.stems().is_none());
    }

    #[test]
    fn per_track_audio_graph_honors_builtin_preset_parameters() {
        let unused_soundfont = Path::new("unused-for-builtin-only-project.sf2");
        let low = render_to_buffer_per_track(&builtin_project(220), unused_soundfont, 8_000)
            .expect("220 Hz builtin render");
        let high = render_to_buffer_per_track(&builtin_project(880), unused_soundfont, 8_000)
            .expect("880 Hz builtin render");

        assert_eq!(low.len(), high.len());
        assert!(low.iter().any(|sample| sample.abs() > 0.01));
        assert_ne!(low, high, "frequency parameter must affect rendered audio");
    }

    /// A short one-track project whose only effect is a Character reverb.
    fn reverb_project(character: f64, track_name: &str) -> oc_dsl::ast::Project {
        let source = format!(
            r#"
@ocp "0.2.0"
@project "fdn repeat"
  tempo: 96
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 261.63, amplitude: 0.4, waveform: 0}}

@track {track_name} | preset: tone, instrument: piano/acoustic_grand
  effects:
    - reverb_fdn: decay=1.2s, mix=60%, character={character}%

  @phrase line [1:1 - 3:1]
    @0.0 C4 q mf
    @1.0 E4 q mf
    @2.0 G4 q mf
"#
        );
        oc_dsl::parse(&source).expect("reverb project should parse")
    }

    /// The defect this guards: a render of the same project must repeat.
    #[test]
    fn reverb_fdn_character_renders_repeat() {
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        for character in [0.0, 30.0, 60.0, 100.0] {
            let project = reverb_project(character, "rev");
            let first = render_to_buffer_per_track(&project, unused, 8_000).expect("first render");
            let second =
                render_to_buffer_per_track(&project, unused, 8_000).expect("second render");
            assert!(
                first == second,
                "character {character} rendered two different mixes"
            );
        }
    }

    /// Two projects that differ only in where the device sits keep distinct
    /// modulation sequences; determinism must not collapse the effect.
    #[test]
    fn reverb_fdn_location_selects_the_sequence() {
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let left = reverb_project(60.0, "left_hall");
        let right = reverb_project(60.0, "right_hall");
        let left = render_to_buffer_per_track(&left, unused, 8_000).expect("left render");
        let right = render_to_buffer_per_track(&right, unused, 8_000).expect("right render");
        assert_ne!(left, right, "one seed must not serve every device");
    }

    /// Determinism must keep the modulation: character 60 must move the mix.
    #[test]
    fn reverb_fdn_character_still_modulates() {
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let plain = reverb_project(0.0, "rev");
        let modulated = reverb_project(60.0, "rev");
        let plain = render_to_buffer_per_track(&plain, unused, 8_000).expect("plain render");
        let modulated =
            render_to_buffer_per_track(&modulated, unused, 8_000).expect("modulated render");
        let total: f32 = plain.iter().map(|sample| sample * sample).sum();
        let difference: f32 = plain
            .iter()
            .zip(&modulated)
            .map(|(plain, modulated)| (plain - modulated) * (plain - modulated))
            .sum();
        let relative_db = 10.0 * (difference / total).log10();
        assert!(
            relative_db > -40.0,
            "character 60 moved the mix by only {relative_db} dB"
        );
    }

    /// Minimal one-preset sine SoundFont; the legacy fallback needs a real font.
    fn soundfont_fixture() -> std::path::PathBuf {
        let dir = oc_test_support::temp_dir("ocp-render-sf2", "minimal");
        let path = dir.join("minimal-test.sf2");
        std::fs::write(&path, include_bytes!("../tests/fixtures/minimal-test.sf2"))
            .expect("fixture write");
        path
    }

    /// One track on a bus, with every mixer stage parameterised for comparison.
    fn mix_project(
        instrument: &str,
        volume_db: f64,
        pan: f64,
        bus_gain: f64,
        master_db: f64,
        master_gain: f64,
    ) -> oc_dsl::ast::Project {
        let source = format!(
            r#"@ocp "0.2.0"

@project "legacy fallback mix"
  tempo: 120
  time_signature: 4/4

@preset tone
{instrument}

@track keys | preset: tone, instrument: piano/acoustic_grand
  mix: vol={volume_db}dB, pan={pan}, output = group
  @phrase held [1:1 - 2:1]
    C4 w mf

@bus group
  routes: [master]
  effects:
    - gain: factor={bus_gain}

@master
  volume: {master_db}dB
  effects:
    - gain: factor={master_gain}
"#
        );
        oc_dsl::parse(&source).expect("mix project should parse")
    }

    fn builtin_instrument() -> &'static str {
        r#"  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 261.63, amplitude: 0.4, waveform: 0}"#
    }

    fn soundfont_instrument() -> &'static str {
        r#"  source: soundfont
  program: 0"#
    }

    /// Fader/pan gains the graph applies to one source width.
    fn pan_gains(volume_db: f64, pan: f64, width: crate::graph::SourceWidth) -> (f32, f32) {
        let mut probe = [1.0f32, 1.0];
        crate::graph::apply_volume_pan(&mut probe, 10.0_f64.powf(volume_db / 20.0), pan, width);
        (probe[0], probe[1])
    }

    /// Rebuild the graph's treatment of a neutral render: fader/pan, bus FX,
    /// master FX, then the master fader, in the graph's order.
    fn graph_treatment(
        neutral: &[f32],
        volume_db: f64,
        pan: f64,
        bus_gain: f64,
        master_db: f64,
        master_gain: f64,
    ) -> Vec<f32> {
        let width = crate::graph::SourceWidth::of_buffer(neutral);
        let (neutral_l, neutral_r) = pan_gains(0.0, 0.0, width);
        let (mixed_l, mixed_r) = pan_gains(volume_db, pan, width);
        let mut out = neutral.to_vec();
        for frame in out.chunks_exact_mut(2) {
            frame[0] *= mixed_l / neutral_l;
            frame[1] *= mixed_r / neutral_r;
        }
        for sample in &mut out {
            *sample *= bus_gain as f32;
        }
        for sample in &mut out {
            *sample *= master_gain as f32;
        }
        let master = 10.0_f64.powf(master_db / 20.0) as f32;
        for sample in &mut out {
            *sample *= master;
        }
        out
    }

    fn assert_same_mix(neutral: &[f32], mixed: &[f32], expected: &[f32]) {
        assert!(neutral.iter().any(|sample| sample.abs() > 0.01));
        assert_ne!(mixed, neutral, "mixer stages must change the audio");
        assert_eq!(mixed.len(), expected.len());
        let max_diff = mixed
            .iter()
            .zip(expected)
            .map(|(mixed, expected)| (mixed - expected).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff < 1e-6, "graph mix mismatch {max_diff}");
    }

    #[test]
    fn plugin_and_plain_entry_points_share_the_no_plugin_mix() {
        let project = mix_project(builtin_instrument(), -6.0, 0.5, 0.5, -3.0, 0.5);
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let mut instances = std::collections::HashMap::new();
        let plain = super::render_to_buffer(&project, unused, 8_000).unwrap();
        let via_plugins =
            super::render_to_buffer_with_plugins(&project, unused, 8_000, &mut instances).unwrap();
        assert_eq!(plain.len(), via_plugins.len());
        assert!(
            plain
                .iter()
                .zip(&via_plugins)
                .all(|(left, right)| left.to_bits() == right.to_bits()),
            "the two render entry points must produce the same samples"
        );
    }

    #[test]
    fn serial_and_parallel_workers_render_the_same_bytes() {
        let mut project = builtin_project(330);
        for suffix in ["b", "c", "d"] {
            let mut track = project.tracks[0].clone();
            track.name = format!("tone_{suffix}");
            project.tracks.push(track);
        }
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let render = |workers: usize| {
            let mut instances = HashMap::new();
            let options = super::RenderOptions {
                sample_rate: 8_000,
                output_gain_db: 0.0,
                resource_root: None,
                minimum_duration_seconds: 0.0,
                pre_roll_seconds: 0.0,
                cancel: None,
                progress: None,
                workers: crate::render_parallel::RenderWorkerOptions::new(
                    std::num::NonZeroUsize::new(workers).unwrap(),
                ),
            };
            super::render_full_for_export_with_options(&project, unused, &mut instances, options)
                .unwrap()
                .0
        };
        let serial = render(1);
        let parallel = render(4);
        assert_eq!(serial.len(), parallel.len());
        assert!(
            serial
                .iter()
                .zip(&parallel)
                .all(|(left, right)| left.to_bits() == right.to_bits()),
            "serial and parallel routing must produce the same mix"
        );
    }

    #[test]
    fn per_track_wav_matches_the_plain_wav() {
        let project = builtin_project(330);
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let dir = std::env::temp_dir().join(format!("oc-per-track-wav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain.wav");
        let per_track = dir.join("per-track.wav");
        super::render_to_wav(&project, unused, &plain, default_spec(8_000)).unwrap();
        super::render_to_wav_per_track(&project, unused, &per_track, 0.0, default_spec(8_000))
            .unwrap();
        assert_eq!(
            std::fs::read(&plain).unwrap(),
            std::fs::read(&per_track).unwrap(),
            "the named per-track file entry must share the plain export bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn per_track_buffer_matches_the_plain_render() {
        let project = mix_project(builtin_instrument(), -6.0, 0.5, 0.5, -3.0, 0.5);
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let plain = super::render_to_buffer(&project, unused, 8_000).unwrap();
        let per_track = super::render_to_buffer_per_track(&project, unused, 8_000).unwrap();
        assert_eq!(plain.len(), per_track.len());
        assert!(
            plain
                .iter()
                .zip(&per_track)
                .all(|(left, right)| left.to_bits() == right.to_bits()),
            "the per-track entry point must share the authoritative mix"
        );
    }

    #[test]
    fn legacy_render_without_a_soundfont_still_applies_the_graph_mix() {
        let unused = Path::new("unused-for-builtin-only-project.sf2");
        let neutral = mix_project(builtin_instrument(), 0.0, 0.0, 1.0, 0.0, 1.0);
        let mixed = mix_project(builtin_instrument(), -6.0, 0.5, 0.5, -3.0, 0.5);
        let neutral = super::render_to_buffer(&neutral, unused, 8_000).unwrap();
        let mixed = super::render_to_buffer(&mixed, unused, 8_000).unwrap();
        let expected = graph_treatment(&neutral, -6.0, 0.5, 0.5, -3.0, 0.5);
        assert_same_mix(&neutral, &mixed, &expected);
    }

    #[test]
    fn legacy_soundfont_render_gets_the_same_graph_mix_as_builtin() {
        let font = soundfont_fixture();
        let mut treatments = Vec::new();
        for instrument in [builtin_instrument(), soundfont_instrument()] {
            let neutral = mix_project(instrument, 0.0, 0.0, 1.0, 0.0, 1.0);
            let mixed = mix_project(instrument, -6.0, 0.5, 0.5, -3.0, 0.5);
            let neutral = super::render_to_buffer(&neutral, &font, 8_000).unwrap();
            let mixed = super::render_to_buffer(&mixed, &font, 8_000).unwrap();
            let expected = graph_treatment(&neutral, -6.0, 0.5, 0.5, -3.0, 0.5);
            assert_same_mix(&neutral, &mixed, &expected);
            let gains = neutral
                .chunks_exact(2)
                .zip(mixed.chunks_exact(2))
                .find(|(neutral, _)| neutral[0].abs() > 0.01 && neutral[1].abs() > 0.01)
                .map(|(neutral, mixed)| (mixed[0] / neutral[0], mixed[1] / neutral[1]))
                .expect("audible frame");
            assert!(
                gains.1 > gains.0 * 1.5,
                "{instrument}: right pan must lift the right channel"
            );
            treatments.push(gains);
        }
        // Equivalent sources present the same written mix to the same graph.
        assert!((treatments[0].0 - treatments[1].0).abs() < 1e-6);
        assert!((treatments[0].1 - treatments[1].1).abs() < 1e-6);
    }

    #[test]
    fn four_bars_of_heavy_compression_are_finite_and_do_not_clip() {
        let source = r#"@ocp "0.2.0"

@project "heavy compression"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 110, amplitude: 0.9, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  effects:
    - compression: threshold=-40dB, ratio=20:1, attack=0.1ms, release=10ms, makeup=12dB, lookahead=5ms, detector_hpf=120Hz, auto_makeup=true, mode=1
  @phrase held [1:1:0 - 5:1:0]
    C2 w ff
    C2 w ff
    C2 w ff
    C2 w ff

@master
  effects:
    - compression: threshold=-30dB, ratio=10:1, attack=1ms, release=50ms, makeup=6dB, mode=1
    - limiter: ceiling=-1dB, true_peak=true, lookahead=5ms, release=100ms
"#;
        let project = oc_dsl::parse(source).expect("heavy project should parse");
        let rendered = render_to_buffer_per_track(
            &project,
            Path::new("unused-for-builtin-only-project.sf2"),
            48_000,
        )
        .expect("heavy render");
        assert!(
            rendered.len() >= 8 * 48_000 * 2,
            "four bars of stereo audio"
        );
        assert!(rendered.iter().all(|sample| sample.is_finite()));
        let peak = rendered.iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
        assert!(
            peak <= 0.892,
            "true-peak limiter must hold the -1 dBTP ceiling, peak {peak}"
        );
    }

    #[test]
    fn master_fader_defaults_to_unity_and_applies_after_track_mix() {
        let sf = Path::new("unused.sf2");
        let unity = render_to_buffer_per_track(&builtin_project(220), sf, 8_000).unwrap();
        let mut attenuated = builtin_project(220);
        attenuated.master = oc_dsl::parse("@project\n@master\n  volume: -6dB\n")
            .unwrap()
            .master;
        let quiet = render_to_buffer_per_track(&attenuated, sf, 8_000).unwrap();
        let factor = 10.0_f32.powf(-6.0 / 20.0);
        let max_diff = quiet
            .iter()
            .zip(&unity)
            .map(|(quiet, unity)| (quiet - unity * factor).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff < 1e-6, "master fader mismatch {max_diff}");
    }

    fn sine_mix(sample_rate: u32, seconds: usize, amplitude: f64) -> Vec<f32> {
        let mut samples = Vec::with_capacity(sample_rate as usize * 2 * seconds);
        for index in 0..sample_rate as usize * seconds {
            let value = (amplitude
                * (2.0 * std::f64::consts::PI * 1_000.0 * index as f64 / f64::from(sample_rate))
                    .sin()) as f32;
            samples.extend_from_slice(&[value, value]);
        }
        samples
    }

    fn transient_mix(sample_rate: u32, seconds: usize) -> Vec<f32> {
        let frames = sample_rate as usize * seconds;
        let click_len = 200usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let time = index as f64 / f64::from(sample_rate);
            let mut value = 0.2 * (2.0 * std::f64::consts::PI * 1_000.0 * time).sin();
            let click = index % sample_rate as usize;
            if click < click_len {
                let envelope = 1.0 - click as f64 / click_len as f64;
                value += 1.5 * envelope * (2.0 * std::f64::consts::PI * 3_000.0 * time).sin();
            }
            samples.extend_from_slice(&[value as f32, value as f32]);
        }
        samples
    }

    #[test]
    fn loudness_target_reaches_target_and_reports_gain() {
        let sample_rate = 48_000;
        let mut samples = sine_mix(sample_rate, 5, 0.1);
        let project = oc_dsl::parse("@project\n@master\n  loudness: -14dB\n").unwrap();
        let report = super::normalise_loudness(&project, &mut samples, sample_rate)
            .unwrap()
            .unwrap();
        assert!((report.achieved_lufs - -14.0).abs() < 0.05, "{report:?}");
        assert!(
            (report.applied_gain_db - (-14.0 - report.measured_lufs)).abs() < 1e-9,
            "{report:?}"
        );
        assert!(report.sample_peak_dbfs <= super::LOUDNESS_CEILING_DBFS + 1e-6);
        assert!(!report.limited_by_ceiling);
        assert_eq!(report.passes, 1, "{report:?}");
        assert_eq!(report.limiter_gain_reduction_max_db, 0.0, "{report:?}");
        assert_eq!(report.pass_reports.len(), 1, "{report:?}");
    }

    #[test]
    fn loudness_target_limiter_holds_ceiling_and_trims_the_residual() {
        let sample_rate = 48_000;
        let mut samples = transient_mix(sample_rate, 5);
        let project = oc_dsl::parse("@project\n@master\n  loudness: -12dB\n").unwrap();
        let report = super::normalise_loudness(&project, &mut samples, sample_rate)
            .unwrap()
            .unwrap();
        assert!(report.ceiling_reached, "{report:?}");
        assert!(report.limited_by_ceiling, "{report:?}");
        assert!(report.peak_over_ceiling_db > 0.0, "{report:?}");
        assert!(report.limiter_gain_reduction_max_db > 0.0, "{report:?}");
        assert!(report.limiter_gain_reduction_mean_db >= 0.0, "{report:?}");
        assert!(report.true_peak_dbfs <= super::LOUDNESS_CEILING_DBFS + 1e-3);
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, value| peak.max(value.abs()));
        assert!(
            f64::from(peak) <= 10.0_f64.powf(super::LOUDNESS_CEILING_DBFS / 20.0) + 1e-6,
            "peak {peak}"
        );
        assert_eq!(report.passes, 2, "{report:?}");
        assert!(
            report.shortfall_lu.abs() <= report.pass_reports[0].residual_lu.abs(),
            "second pass must not increase the residual: {report:?}"
        );
        // The program-dependent release makes the second trim nonlinear, so
        // the two-pass correction can land a little under the target.
        assert!(report.shortfall_lu.abs() <= 0.25, "{report:?}");
    }

    #[test]
    fn loudness_correction_is_deterministic() {
        let sample_rate = 48_000;
        let mut first = transient_mix(sample_rate, 3);
        let mut second = first.clone();
        let project = oc_dsl::parse("@project\n@master\n  loudness: -12dB\n").unwrap();
        let first_report = super::normalise_loudness(&project, &mut first, sample_rate)
            .unwrap()
            .unwrap();
        let second_report = super::normalise_loudness(&project, &mut second, sample_rate)
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first_report, second_report);
    }

    #[test]
    fn limiter_pass_compensates_lookahead_latency() {
        let sample_rate = 48_000;
        let mut input = vec![0.0_f32; sample_rate as usize * 2];
        let onset = 1_000;
        input[onset * 2] = 0.25;
        input[onset * 2 + 1] = 0.25;
        let limited = super::limit_pass(&input, 0.0, sample_rate);
        let first = limited
            .samples
            .chunks_exact(2)
            .position(|frame| frame[0].abs() > 1e-12);
        assert_eq!(first, Some(onset));
        assert!((limited.samples[onset * 2] - 0.25).abs() <= 1e-6);
    }

    #[test]
    fn missing_loudness_target_leaves_the_mix_untouched() {
        let sample_rate = 48_000;
        let mut samples = sine_mix(sample_rate, 1, 0.5);
        let original = samples.clone();
        let project = oc_dsl::parse("@project\n@master\n  volume: -3dB\n").unwrap();
        assert!(
            super::normalise_loudness(&project, &mut samples, sample_rate)
                .unwrap()
                .is_none()
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn stems_sum_to_the_dry_mix_and_frames_match() {
        let project = oc_dsl::parse(
            r#"@project
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2}
@preset other
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2}
@track direct | preset: tone, instrument: synth/lead_square
  @phrase held [1:1 - 2:1]
    C4 w mf
@track routed | preset: other, instrument: synth/lead_square
  mix: output=group
  @phrase held [1:1 - 2:1]
    E4 w mf
@bus group
  routes: [master]
"#,
        )
        .unwrap();
        let export = super::render_with_stems(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            None,
            0.0,
            0.0,
        )
        .unwrap();
        assert!(export.loudness.is_none());
        assert_eq!(export.stems.len(), 3);
        let frames = export.mix.len();
        for stem in &export.stems {
            assert_eq!(stem.samples.len(), frames, "{}", stem.name);
        }
        let direct = export
            .stems
            .iter()
            .find(|stem| stem.name == "direct")
            .unwrap();
        let routed = export
            .stems
            .iter()
            .find(|stem| stem.name == "routed")
            .unwrap();
        let bus = export
            .stems
            .iter()
            .find(|stem| stem.name == "group")
            .unwrap();
        assert!(direct.mix_sum_member);
        assert!(!routed.mix_sum_member);
        assert!(bus.mix_sum_member);
        let mut sum = vec![0.0_f32; frames];
        for stem in export.stems.iter().filter(|stem| stem.mix_sum_member) {
            for (out, value) in sum.iter_mut().zip(&stem.samples) {
                *out += value;
            }
        }
        let max_diff = sum
            .iter()
            .zip(&export.mix)
            .map(|(sum, mix)| (sum - mix).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff <= 1e-6, "stem sum differs from mix by {max_diff}");
    }

    #[test]
    fn stems_export_writes_sanitized_unique_names_and_manifest() {
        let mut project = oc_dsl::parse(
            r#"@project
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2}
@track first | preset: tone, instrument: synth/lead_square
  @phrase held [1:1 - 2:1]
    C4 w mf
@track second | preset: tone, instrument: synth/lead_square
  mix: output=group
  @phrase held [1:1 - 2:1]
    E4 w mf
@bus group
  routes: [master]
"#,
        )
        .unwrap();
        project.tracks[0].name = "Lead Synth / Main".into();
        project.tracks[1].name = "lead synth main".into();
        let export = super::render_with_stems(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            None,
            0.0,
            0.0,
        )
        .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let manifest =
            write_stems_export(&project, dir, "My Song!", &export, default_spec(8_000)).unwrap();
        assert!(dir.join("my-song-lead-synth-main.wav").is_file());
        assert!(dir.join("my-song-lead-synth-main-2.wav").is_file());
        assert!(dir.join("my-song-bus-group.wav").is_file());
        assert!(dir.join("my-song-mix.wav").is_file());
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(json["format"], "opencompose-stems-v1");
        assert_eq!(json["container"], "wav");
        assert_eq!(json["sample_format"], "i16");
        assert_eq!(json["dithered"], false);
        assert_eq!(json["frames"], export.mix.len() / 2);
        assert_eq!(json["stems"].as_array().unwrap().len(), 3);
        assert!(
            json["routing"]["policy"]
                .as_str()
                .unwrap()
                .contains("pre-master")
        );
        let mix = std::iter::once(&json["mix"]);
        for entry in mix.chain(json["stems"].as_array().unwrap()) {
            let path = dir.join(entry["file"].as_str().unwrap());
            let wav = hound::WavReader::open(path).unwrap();
            assert_eq!(entry["frames"], json["frames"]);
            assert_eq!(entry["frames"], wav.duration());
            assert_eq!(json["sample_rate"], wav.spec().sample_rate);
            assert_eq!(json["channels"], wav.spec().channels);
        }
    }

    /// A CJK song name keeps its letters instead of collapsing to `untitled`;
    /// every script does, and a name with nothing usable falls back to a
    /// readable word.
    #[test]
    fn stem_names_keep_their_script_and_never_collapse_to_untitled() {
        assert_eq!(super::sanitize_stem_component("我的歌", "song"), "我的歌");
        assert_eq!(
            super::sanitize_stem_component("Café Déjà", "song"),
            "café-déjà"
        );
        assert_eq!(
            super::sanitize_stem_component("../../etc/passwd", "song"),
            "etc-passwd"
        );
        assert_eq!(super::sanitize_stem_component("!!! ???", "song"), "song");
        assert_eq!(super::sanitize_stem_component("???", "track"), "track");
        assert_eq!(
            super::sanitize_stem_component("My Song!", "song"),
            "my-song"
        );
    }

    /// Re-reading a clipped file shows 0.00 dBFS, so the render returns the
    /// level the mix reached before quantisation.
    #[test]
    fn a_hot_render_reports_the_peak_it_clipped() {
        let project = project_with_effect("test.fake.effect");
        let path = std::env::temp_dir().join(format!(
            "ocp-hot-render-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let report = render_to_wav(
            &project,
            Path::new("unused.sf2"),
            &path,
            default_spec(8_000),
        )
        .expect("a hot mix still writes");
        assert!(report.clipped, "{report:?}");
        assert!(
            (report.pre_clamp_peak_dbfs - 3.521_825).abs() < 0.01,
            "{report:?}"
        );
        assert!(
            report.pre_clamp_true_peak_dbfs >= report.pre_clamp_peak_dbfs - 0.01,
            "{report:?}"
        );

        // The file itself is clamped at full scale, which is why its own level
        // cannot report the overload.
        let written = crate::analysis::read_wav(&path).unwrap();
        let peak = written
            .channels
            .iter()
            .flat_map(|channel| channel.iter())
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        assert!(peak <= 1.0, "the written file must clamp, peak {peak}");
        assert!(
            (peak - 1.0).abs() < 1e-3,
            "the written file must clip, peak {peak}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// One builtin-source track with a hosted plugin in its insert chain.
    fn project_with_effect(plugin_id: &str) -> oc_dsl::ast::Project {
        let source = format!(
            r#"@ocp "0.2.0"

@project "fake insert"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}

@track Lead | preset: tone, instrument: synth/lead_square
  effects:
    - plugin:"{plugin_id}"
  @phrase held [1:1 - 2:1]
    C4 w mf
"#
        );
        oc_dsl::parse(&source).expect("fake insert project should parse")
    }

    fn sample_hash(audio: &[f32]) -> String {
        let bytes: Vec<u8> = audio
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        format!("{:032x}", Md5::digest(bytes))
    }

    /// One builtin-source track whose insert chain is the given effect line;
    /// an empty line leaves the track without an `effects:` block.
    fn project_with_effect_line(effect_line: &str) -> oc_dsl::ast::Project {
        let effects = if effect_line.is_empty() {
            String::new()
        } else {
            format!("  effects:\n    {effect_line}\n")
        };
        let source = format!(
            r#"@ocp "0.2.0"

@project "fake insert"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}

@track Lead | preset: tone, instrument: synth/lead_square
{effects}  @phrase held [1:1 - 2:1]
    C4 w mf
"#
        );
        oc_dsl::parse(&source).expect("effect line project should parse")
    }

    #[test]
    fn a_bypassed_plugin_effect_renders_like_an_absent_one() {
        let active = project_with_effect_line(r#"- plugin:"test.fake.effect""#);
        let bypassed = project_with_effect_line(r#"- plugin:"test.fake.effect": bypass=true"#);
        let absent = project_with_effect_line("");

        let active_audio =
            render_to_buffer_per_track(&active, Path::new("unused.sf2"), 48_000).unwrap();
        let bypassed_audio =
            render_to_buffer_per_track(&bypassed, Path::new("unused.sf2"), 48_000).unwrap();
        let absent_audio =
            render_to_buffer_per_track(&absent, Path::new("unused.sf2"), 48_000).unwrap();

        assert_ne!(
            sample_hash(&active_audio),
            sample_hash(&bypassed_audio),
            "the loaded fake insert must reach the mix"
        );
        assert_eq!(
            sample_hash(&bypassed_audio),
            sample_hash(&absent_audio),
            "a bypassed plugin is absent, bit for bit"
        );
    }

    #[test]
    fn a_bypassed_plugin_that_cannot_load_still_renders() {
        let project = project_with_effect_line(r#"- plugin:"test.missing.effect": bypass=true"#);
        render_to_buffer_per_track(&project, Path::new("unused.sf2"), 48_000)
            .expect("a bypassed plugin is never instantiated");
    }

    #[test]
    fn an_instrument_plugin_in_an_effects_chain_is_refused_by_name() {
        let project = project_with_effect("test.fake.two_bus");
        let error = render_to_buffer_per_track(&project, Path::new("unused.sf2"), 48_000)
            .expect_err("an instrument must not load as an insert");
        assert_eq!(
            error,
            "VST3 Plugin: an instrument plugin (0 audio inputs, 2 stereo outputs) cannot be used \
             as an effect on track 'Lead'; put it on an instrument track, or wrap it in a bus"
        );
    }

    #[test]
    fn a_refused_effects_chain_writes_no_output_file() {
        let project = project_with_effect("test.fake.two_bus");
        let path = std::env::temp_dir().join(format!(
            "ocp-refused-insert-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let error = render_to_wav(
            &project,
            Path::new("unused.sf2"),
            &path,
            default_spec(48_000),
        )
        .expect_err("the render must stop before writing");
        assert!(error.contains("cannot be used as an effect"), "{error}");
        assert!(
            !path.exists(),
            "a refused render left {} behind",
            path.display()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stereo_effect_plugin_in_an_effects_chain_still_processes() {
        let project = project_with_effect("test.fake.effect");
        let audio = render_to_buffer_per_track(&project, Path::new("unused.sf2"), 48_000)
            .expect("a stereo insert still renders");
        let peak = audio
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            (peak - 1.5).abs() < 1e-6,
            "the insert output must reach the mix, peak {peak}"
        );
        assert_eq!(sample_hash(&audio), "0eba27cef4e6d945d4f772b5cb1f4b25");
    }

    #[test]
    fn plugin_aware_export_uses_authoritative_render_cache_path() {
        let project = builtin_project(440);
        let unused_soundfont = Path::new("unused-for-builtin-only-project.sf2");
        let mut public_instances = HashMap::new();
        let public =
            render_to_buffer_with_plugins(&project, unused_soundfont, 8_000, &mut public_instances)
                .expect("public plugin-aware render");

        let mut direct_instances = HashMap::new();
        let direct = RenderCache::new()
            .render_full(&project, unused_soundfont, 8_000, &mut direct_instances)
            .expect("direct render cache render");

        assert_eq!(public, direct);
    }

    /// Two tone tracks; the first carries a lookahead limiter and a send, the
    /// second is dry. Both play four bars from bar 1.
    fn bounce_project() -> oc_dsl::ast::Project {
        let source = r#"@ocp "0.2.0"

@project "bounce"
  tempo: 120
  time_signature: 4/4

@preset low
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.3, waveform: 0}

@preset high
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 660, amplitude: 0.3, waveform: 0}

@bus hall
  routes: [master]
  effects:
    - reverb: decay=1.2s, mix=40%

@track low | preset: low, instrument: synth/lead_square
  mix: send(hall=50%)
  effects:
    - limiter: ceiling=-1dB, true_peak=true, lookahead=5ms, release=100ms
  @phrase held [1:1:0 - 5:1:0]
    C4 w mf
    C4 w mf
    C4 w mf
    C4 w mf

@track high | preset: high, instrument: synth/lead_square
  @phrase held [1:1:0 - 5:1:0]
    C5 w mf
    C5 w mf
    C5 w mf
    C5 w mf

@master
  effects:
    - eq: low_gain=-3dB
"#;
        oc_dsl::parse(source).expect("bounce project should parse")
    }

    fn find_stem<'a>(capture: &'a crate::graph::StemCapture, name: &str) -> &'a [f32] {
        capture
            .tracks
            .iter()
            .find(|(track, _)| track == name)
            .map(|(_, samples)| samples.as_slice())
            .unwrap_or_else(|| panic!("no stem captured for {name}"))
    }

    /// One held quarter note ending on the range end: a 200 ms release and
    /// 40 ms of insert delay in two lookahead limiters, at 60 bpm so one
    /// quarter is one second.
    fn release_project() -> oc_dsl::ast::Project {
        let source = r#"@ocp "0.2.0"

@project "release"
  tempo: 60
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 0, amp.attack: 0.001s, amp.decay: 0.1s, amp.sustain: 0.8, amp.release: 0.2s, output.gain: 0.4}

@track lead | preset: tone, instrument: synth/lead_sine
  effects:
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
  @phrase held [1:1:0 - 1:2:0]
    C4 q mf
"#;
        oc_dsl::parse(source).expect("release project should parse")
    }

    #[test]
    fn a_bounce_tail_covers_the_insert_delay_and_the_release() {
        let project = release_project();
        let tail =
            bounce_tail_seconds(&project, 0, 48_000, &HashMap::new(), None).expect("tail seconds");
        assert!(
            (tail - 0.24).abs() < 1e-6,
            "40 ms of delay plus a 200 ms release, got {tail}"
        );
    }

    /// The release project plus a 2 s reverb insert, optionally bypassed.
    fn reverb_release_project(bypass: bool) -> oc_dsl::ast::Project {
        let source = format!(
            r#"@ocp "0.2.0"

@project "reverb release"
  tempo: 60
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {{osc.a.waveform: 0, amp.attack: 0.001s, amp.decay: 0.1s, amp.sustain: 0.8, amp.release: 0.2s, output.gain: 0.4}}

@track lead | preset: tone, instrument: synth/lead_sine
  effects:
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - reverb: decay=2s, mix=30%{}
  @phrase held [1:1:0 - 1:2:0]
    C4 q mf
"#,
            if bypass { ", bypass=true" } else { "" }
        );
        oc_dsl::parse(&source).expect("reverb release project should parse")
    }

    #[test]
    fn a_bounce_tail_covers_a_reverb_decay() {
        let project = reverb_release_project(false);
        let tail =
            bounce_tail_seconds(&project, 0, 48_000, &HashMap::new(), None).expect("tail seconds");
        assert!(
            (tail - 2.24).abs() < 1e-6,
            "2 s of decay, 40 ms of delay and a 200 ms release, got {tail}"
        );

        // The same chain must not move the full-mix length: the reverb reports
        // its decay on the bounce-only path.
        let chain = crate::graph::track_chain_tail_seconds(&project, 0, 48_000).expect("chain");
        assert_eq!(chain.ring, 0.0, "the reverb declares no full-mix tail");
        assert!((chain.bounce_ring - 2.0).abs() < 1e-6, "{chain:?}");
        let graph = crate::graph::AudioGraph::from_project(&project, 48_000);
        assert_eq!(
            graph.max_effect_tail_samples(),
            0,
            "the full-mix length policy is unchanged"
        );
    }

    #[test]
    fn a_bypassed_reverb_adds_no_bounce_tail() {
        let project = reverb_release_project(true);
        let tail =
            bounce_tail_seconds(&project, 0, 48_000, &HashMap::new(), None).expect("tail seconds");
        assert!(
            (tail - 0.24).abs() < 1e-6,
            "a bypassed reverb keeps only the delay and the release, got {tail}"
        );
    }

    #[test]
    fn a_render_past_the_range_end_carries_the_release() {
        let project = release_project();
        let rate = 48_000;
        let soundfont = Path::new("unused-for-builtin-only-project.sf2");
        let tail =
            bounce_tail_seconds(&project, 0, rate, &HashMap::new(), None).expect("tail seconds");
        let mut instances = HashMap::new();
        let body = render_track_post_fx_range(
            &project,
            soundfont,
            rate,
            &mut instances,
            None,
            0,
            0.0,
            1.0,
        )
        .expect("range render");
        let with_tail = render_track_post_fx_range(
            &project,
            soundfont,
            rate,
            &mut instances,
            None,
            0,
            0.0,
            1.0 + tail,
        )
        .expect("range render with the tail");

        let body_frames = body.len() / 2;
        let tail_frames = with_tail.len() / 2;
        let extra = tail_frames - body_frames;
        assert!(
            (extra as f64 - tail * f64::from(rate)).abs() <= 2.0,
            "the tail adds {extra} frames, expected {:.0}",
            tail * f64::from(rate)
        );
        let after = &with_tail[body_frames * 2..];
        let peak = after
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            peak > 0.01,
            "the release has to sound past the range end, peak {peak}"
        );
    }

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("oc-bounce-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    #[test]
    fn a_bounce_is_a_timeline_slice_of_the_tracks_mix_tap() {
        let project = bounce_project();
        let soundfont = Path::new("unused-for-builtin-only-project.sf2");
        let rate = 8_000;
        let mut cache = RenderCache::new();
        cache.capture_track_pre_fader();
        cache
            .render_full(&project, soundfont, rate, &mut HashMap::new())
            .expect("full render with the track tap");
        let tap = find_stem(cache.stems().expect("captured taps"), "low").to_vec();

        let from = 4.0;
        let to = 8.0;
        let bounce = render_track_post_fx_range(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            None,
            0,
            from,
            to,
        )
        .expect("bounce");
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let start = clock.sample_at_quarter(from, rate).unwrap() as usize;
        let end = clock.sample_at_quarter(to, rate).unwrap() as usize;
        let frames = end - start;
        assert_eq!(bounce.len(), frames * 2, "the bounce covers the beat range");
        assert!(
            bounce.iter().any(|sample| sample.abs() > 1e-4),
            "the bounce is not silence"
        );
        for (index, sample) in bounce.iter().enumerate() {
            let reference = tap[start * 2 + index];
            assert!(
                (sample - reference).abs() <= 1e-6,
                "sample {index} is {sample}, the mix tap has {reference}"
            );
        }
        // Sample 0 is the mix tap at the range start, not the first sample of a
        // window rendered from silence.
        assert_eq!(bounce[0], tap[start * 2]);

        let again = render_track_post_fx_range(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            None,
            0,
            from,
            to,
        )
        .expect("second bounce");
        assert_eq!(bounce, again, "the bounce is deterministic");
    }

    #[test]
    fn a_project_with_the_bounce_substituted_keeps_the_track_stem() {
        let project = bounce_project();
        let soundfont = Path::new("unused-for-builtin-only-project.sf2");
        let rate = 8_000;
        let from = 4.0;
        let to = 8.0;
        let dir = scratch_dir("substitute");
        std::fs::create_dir_all(dir.join("media")).unwrap();
        let bounce = render_track_post_fx_range(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            None,
            0,
            from,
            to,
        )
        .expect("bounce");
        crate::wav::write_stereo_f32_wav(dir.join("media/bounce-low-1.wav"), &bounce, rate)
            .expect("bounce media");

        let mut original = RenderCache::new();
        original.capture_stems(true);
        original
            .render_full(&project, soundfont, rate, &mut HashMap::new())
            .expect("original render");
        let original_stem = find_stem(original.stems().expect("stems"), "low").to_vec();

        let mut substituted = project.clone();
        let source = substituted
            .tracks
            .iter_mut()
            .find(|track| track.name == "low")
            .expect("source track");
        let mix = source.mix.clone();
        source.mix.mute = true;
        let mut bounced = source.clone();
        bounced.name = "low (bounced)".to_string();
        bounced.instrument = oc_dsl::ast::InstrumentPath::new("audio", "clip");
        bounced.preset = None;
        bounced.effects.clear();
        bounced.gain_automation.clear();
        bounced.set_automation_lanes(std::iter::empty());
        bounced.clips.clear();
        bounced.phrases.clear();
        bounced.patterns.clear();
        bounced.inline_patterns.clear();
        bounced.performance = None;
        bounced.mix = mix;
        substituted.tracks.push(bounced);
        substituted
            .add_audio_clip(oc_dsl::ast::AudioClip {
                name: "low bounce".to_string(),
                track: "low (bounced)".to_string(),
                file: "media/bounce-low-1.wav".to_string(),
                time_range: oc_dsl::ast::TimeRange {
                    start: oc_dsl::ast::BarBeat::new(2, 1, 0),
                    end: oc_dsl::ast::BarBeat::new(3, 1, 0),
                },
                placement: Default::default(),
                offset_seconds: 0.0,
                gain_db: 0.0,
                fade_in_seconds: 0.0,
                fade_out_seconds: 0.0,
                invert: false,
                crossfade_seconds: 0.0,
                stretch: 1.0,
                stretch_mode: oc_dsl::ast::StretchMode::default(),
                tempo_follow: false,
                pitch: 0.0,
                formant: 0.0,
                bypass_processing: false,
                preserve_formants: false,
                warp_markers: Vec::new(),
                loop_seconds: 0.0,
                take_group: None,
                take: None,
                muted: false,
                gain_envelope: Vec::new(),
            })
            .expect("the bounced track exists");
        oc_dsl::validate(&substituted).expect("the substituted project validates");

        let mut cache = RenderCache::new();
        cache.capture_stems(true);
        cache.set_resource_root(Some(dir.clone()));
        cache
            .render_full(&substituted, soundfont, rate, &mut HashMap::new())
            .expect("substituted render");
        let substituted_stem = find_stem(cache.stems().expect("stems"), "low (bounced)");

        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let start = clock.sample_at_quarter(from, rate).unwrap() as usize;
        let end = clock.sample_at_quarter(to, rate).unwrap() as usize;
        for within in 0..(end - start) {
            for channel in 0..2 {
                let index = (start + within) * 2 + channel;
                assert!(
                    (substituted_stem[index] - original_stem[index]).abs() <= 1e-6,
                    "frame {within} channel {channel}: {} vs {}",
                    substituted_stem[index],
                    original_stem[index]
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A random LFO must render the same bytes twice: the sequence is seeded
    /// per note index, so reopening a project cannot change a printed take.
    #[test]
    fn a_random_lfo_renders_the_same_samples_twice() {
        let project = oc_dsl::parse(
            r#"@ocp "0.4.0"

@project "Random print"
  tempo: 120
  time_signature: 4/4

@preset rnd
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 0, osc.a.level: 0.7, lfo.shape: 4, lfo.rate: 8, mod.1.source: 0, mod.1.destination: 1, mod.1.amount: 0.5}

@track lead | preset: rnd, instrument: synth/lead
  @phrase line [1:1 - 3:1]
    C4 w mf
"#,
        )
        .unwrap();
        let render = || {
            crate::render::render_full_for_export(
                &project,
                std::path::Path::new(""),
                48_000,
                0.0,
                &mut std::collections::HashMap::new(),
                None,
                0.0,
                0.0,
            )
            .expect("the random patch renders")
            .0
        };
        let first = render();
        let second = render();
        assert!(
            first.iter().any(|sample| sample.abs() > 1e-4),
            "the patch has to sound for the comparison to mean anything"
        );
        assert_eq!(first, second, "a random LFO changed between renders");
    }
}
