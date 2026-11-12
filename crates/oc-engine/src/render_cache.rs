//! Per-track render cache + incremental mixing.
//!
//! Most tracks are rendered independently to their own stereo buffer. Tracks
//! that use the same external plugin preset and declare the same multi-output
//! routing share one stateful instance: their events are processed together and
//! declared output pairs are fanned into separate track buffers. Editing one member invalidates the whole group.
//! The final mix combines all track buffers through the audio graph
//! (effects, buses, master).
//!
//! ```text
//! Track 0: [cached f32 buffer] ─┐
//! Track 1: [cached f32 buffer] ─┤──► AudioGraph.mix() ──► master buffer
//! Track 2: [DIRTY → re-render] ─┘
//! ```
//!
//! A `tracks` window reuses the selected tracks' post-FX buffers as snapshots
//! and stops the graph pass at the window end. Snapshots cost 8 bytes per
//! frame, i.e. 384 KB per track-second at 48 kHz, and are capped by
//! [`RenderCache::set_post_fx_snapshot_budget`] (64 MiB, least-recently-used
//! eviction). A cold window sources only the selected tracks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::dsp::AudioEffect;
use crate::graph::{AudioGraph, SourceWidth};
use crate::interpret::{self, InterpretSettings, MidiEvent, MidiMessage};
use crate::render_control::InstrumentProfile;
use crate::render_parallel::RenderPool;
use crate::soundfont::SoundFontRenderer;
use oc_dsl::ast::{ParamValue, PresetDef, Project};
#[cfg(target_os = "macos")]
use oc_host::au_instance::AuInstance;
use oc_host::clap_instance::ClapInstance;
use oc_host::ports::AudioPort;
use oc_host::vst2::Vst2Instance;
use oc_host::vst3_instance::Vst3Instance;
use oc_host::{PluginFormat, PluginStateContext, PluginTransport, find_plugin_info};

const DEFAULT_RENDER_TAIL_SECONDS: u32 = 2;
/// Maximum extension for a reported plug-in or effect tail.
pub const MAX_DECLARED_PLUGIN_TAIL_SECONDS: u32 = 30;

type AuxOutputBuffers = HashMap<(usize, usize), Vec<f32>>;

mod builtin;
mod cache;
mod clap_prepare;
mod event_sink;
mod helpers;
mod patterns;
mod plugin_tracks;
mod preset_state;
mod vst2_group;
mod vst2_prepare;
mod vst2_runtime;
mod vst3_group;
mod vst3_tail;
mod window;

#[cfg(test)]
mod tests;

use self::builtin::*;
use self::event_sink::*;
use self::helpers::*;
pub use self::helpers::{
    aux_pairs_by_track, track_output_pair, track_requests_non_main_output, validate_aux_sends,
};
use self::plugin_tracks::*;
pub(crate) use self::plugin_tracks::{
    queue_vst2_performance_command, queue_vst3_performance_command,
};
use self::preset_state::*;
use self::vst3_group::{Vst3GroupRenderOptions, render_tracks_via_vst3};
use self::window::*;

pub(crate) use self::builtin::render_builtin_project;
pub use self::builtin::{render_track_via_builtin_pub, render_track_via_builtin_with_tempo};
pub(crate) use self::event_sink::queue_plugin_midi_event;
pub(crate) use self::helpers::project_transport;
pub(crate) use self::preset_state::sfizz_vst3_state_bytes;
#[cfg(target_os = "macos")]
pub use self::preset_state::{apply_au_preset_params, apply_au_preset_state_at};
pub use self::preset_state::{
    apply_clap_preset_params, apply_clap_preset_state_at, apply_lv2_preset_params,
    apply_lv2_preset_state_at, apply_vst2_preset_params, apply_vst2_preset_state_at,
    apply_vst3_preset_params, apply_vst3_preset_state_at, materialize_vst3_preset_state_at,
};

/// Offline range render request: sample bounds plus an optional track subset.
/// `render_range` slices the timeline, so notes sounding at `start_sample`
/// continue; `render_solo_window` starts from silence for previews.
#[derive(Debug, Clone)]
pub struct RangeRenderRequest {
    pub start_sample: u64,
    pub end_sample: u64,
    /// `None` renders every track. Indices refer to the project's track order;
    /// a shared multi-output plugin group is expanded to the whole group.
    pub tracks: Option<Vec<usize>>,
    /// Capture post-fader track taps alongside the master mix.
    pub stems: bool,
}

/// One range render result. `stems` is `(track_index, track_name, samples)`.
/// `cache_hit` is true when the request was answered from an already clean
/// cached render without re-synthesising any source: a full-mix slice, a
/// snapshot or recomputed subset window, or silence past the cached horizon.
/// Only a cold cache (no clean full render to cut from) reports false.
#[derive(Debug)]
pub struct RangeRenderOutput {
    pub audio: Vec<f32>,
    pub stems: Vec<(usize, String, Vec<f32>)>,
    pub sample_rate: u32,
    pub frames: usize,
    /// Effective track selection after shared plugin-output groups are expanded.
    pub tracks: Vec<usize>,
    pub cache_hit: bool,
    /// How a `tracks` window was assembled; `None` for a full-mix window.
    pub window: Option<RangeWindowPath>,
}

/// Where a track-subset window came from. Every variant draws the same timeline
/// slice; they differ only in how much was recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeWindowPath {
    /// Served from post-track-FX snapshots taken off the cached full render.
    Snapshot,
    /// Track effects were re-processed for the selected tracks, bounded at the
    /// window end, because no snapshot covered it.
    Recomputed,
    /// No clean cached mix: only the selected tracks were rendered.
    Cold,
    /// The window starts past the rendered timeline, so the answer is silence.
    Silent,
}

/// Frames and margins for one graph pass, derived from the instantiated graph.
pub(super) struct MixLayout {
    /// Frames the graph runs on (track buffer length, master accumulator).
    pub(super) render_total: usize,
    /// Frames kept after the master latency shift (program + PDC + tail).
    pub(super) output_total: usize,
    /// Latency of the master chain, in frames.
    pub(super) master_latency: usize,
}

impl MixLayout {
    pub(super) fn new(
        graph: &AudioGraph,
        sample_rate: u32,
        program: usize,
        source_tails: &[u32],
    ) -> Result<Self, String> {
        let tail = usize::try_from(bounded_tail_samples(
            graph.max_effect_tail_with_sources(source_tails),
            sample_rate,
        ))
        .map_err(|_| "effect tail render length exceeds platform size".to_string())?;
        let output_total = program
            .checked_add(graph.pre_master_latency_samples() as usize)
            .and_then(|samples| samples.checked_add(tail))
            .ok_or_else(|| "PDC render length overflow".to_string())?;
        let master_latency = graph.master_chain.latency_samples() as usize;
        let render_total = output_total
            .checked_add(master_latency)
            .ok_or_else(|| "master PDC render length overflow".to_string())?;
        Ok(Self {
            render_total,
            output_total,
            master_latency,
        })
    }

    /// Stereo sample count of `render_total`.
    pub(super) fn render_stereo(&self) -> usize {
        self.render_total * 2
    }
}

/// What one cache render covers.
enum RenderScope<'a> {
    /// The whole project.
    Full,
    /// Only these tracks; the program length is unchanged.
    Tracks(&'a [usize]),
    /// A time window of the timeline from silence; previews only.
    Window(&'a RangeRenderRequest),
}

impl<'a> RenderScope<'a> {
    /// Tracks this scope renders; `None` means every track.
    fn tracks(&self) -> Option<&'a [usize]> {
        match self {
            Self::Full => None,
            Self::Tracks(tracks) => Some(tracks),
            Self::Window(request) => request.tracks.as_deref(),
        }
    }

    fn window(&self) -> Option<&'a RangeRenderRequest> {
        match self {
            Self::Window(request) => Some(request),
            _ => None,
        }
    }

    fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// One source-render pass, carrying the request's worker snapshot through any
/// scratch caches without consulting process configuration again.
struct RenderPass<'a> {
    scope: RenderScope<'a>,
    workers: crate::render_parallel::RenderWorkerOptions,
}

/// Timing and routing for a graph pass over cached source buffers.
struct GraphMixOptions<'a> {
    sample_rate: u32,
    total_samples: usize,
    timeline_start: u64,
    selection: Option<&'a [usize]>,
    window: Option<&'a WindowMix<'a>>,
    workers: crate::render_parallel::RenderWorkerOptions,
}

#[derive(Clone, Copy)]
struct RangeRenderPass<'a> {
    request: &'a RangeRenderRequest,
    frames: usize,
    workers: crate::render_parallel::RenderWorkerOptions,
}

struct ScratchRenderPass<'a> {
    selected: &'a [usize],
    stems: bool,
    window_end: Option<u64>,
    control_only: &'a [usize],
    workers: crate::render_parallel::RenderWorkerOptions,
}

pub struct RenderCache {
    worker_options: Option<crate::render_parallel::RenderWorkerOptions>,
    vst3_tail_probe: vst3_tail::Vst3TailProbe,
    vst2_tail_probe: vst2_prepare::Vst2TailProbe,
    vst2_group_renderer: vst2_group::Vst2GroupRenderer,
    track_buffers: Vec<Option<Vec<f32>>>,
    track_dirty: Vec<bool>,
    track_source_latencies: Vec<u32>,
    track_source_tails: Vec<u32>,
    /// Source lane → hosting track. A pattern's preset makes one track render
    /// through several instruments; this maps each of them back.
    lane_targets: Vec<usize>,
    /// Content hash of each track's audio clips; a mismatch forces a re-render.
    track_clip_fingerprints: Vec<u64>,
    /// Content hash of each track's hairpin ramps, which feed the sampler.
    track_ramp_fingerprints: Vec<u64>,
    /// Plugin auxiliary output pairs requested by each track's mix sends.
    aux_pairs: Vec<Vec<usize>>,
    /// Pre-FX plugin outputs keyed by (hosting track, stereo output pair).
    aux_buffers: AuxOutputBuffers,
    /// Per-named-pattern render cache. Key = pattern name.
    pattern_buffers: HashMap<String, Vec<f32>>,
    /// Which patterns need re-rendering.
    pattern_dirty: HashMap<String, bool>,
    /// Reverse map: pattern name → track indices that reference it via clips.
    /// Built during render_incremental from project data.
    pattern_to_tracks: HashMap<String, Vec<usize>>,
    duration_seconds: f64,
    /// Program length of the cached track buffers; a window is cut from this.
    render_horizon_samples: usize,
    minimum_duration_seconds: f64,
    sample_rate: u32,
    mix_dirty: bool,
    master_buffer: Option<Vec<f32>>,
    stem_capture: Option<crate::graph::StemCapture>,
    consume_source_buffers: bool,
    /// Directory used to resolve relative external plugin-state resources.
    /// CLI/GUI callers should bind this to the containing project directory.
    resource_root: Option<PathBuf>,
    /// Wall-clock warm-up per plugin instance before the project starts.
    pre_roll_seconds: f64,
    native_state_stamps: HashMap<PathBuf, (u64, std::time::SystemTime)>,
    /// Pattern preview buffers are only for the GUI browser, not offline export.
    pattern_previews: bool,
    /// Post-track-FX buffers reused by later subset windows, one per track.
    post_fx_snapshots: Vec<Option<PostFxSnapshot>>,
    /// Byte budget for `post_fx_snapshots`; 0 disables them.
    post_fx_budget_bytes: usize,
    post_fx_bytes: usize,
    post_fx_clock: u64,
    /// Widths classified by the last full mix; a window reuses them so its pan
    /// law cannot depend on what the window happens to contain.
    full_mix_widths: Vec<SourceWidth>,
    /// Master chain latency of the last mix, in frames.
    master_latency_samples: u32,
    /// Tracks rendered for a control feed only; their output stays out of the mix.
    control_only: Vec<usize>,
    /// Cancellation shared with the caller of an offline export.
    cancel: Option<crate::render_cancel::CancelToken>,
    /// Progress sink called between source groups.
    progress: Option<crate::render_cancel::ProgressCallback>,
    /// Test-only instrumentation for the "a subset must not re-mix" contracts.
    #[cfg(any(test, feature = "test-support"))]
    counters: RenderCounters,
}

/// Test-only render counters, read as deltas around one request.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderCounters {
    /// Whole-project renders (a missing or stale cache being refreshed).
    pub full_render_calls: usize,
    /// Tracks whose source was rendered.
    pub source_renders: usize,
    /// Graph passes over the whole cached timeline.
    pub full_timeline_mix_passes: usize,
}

/// A track's post-track-FX stereo buffer, prefixed at frame 0.
///
/// A window needs the frames up to its end plus the master chain's latency.
/// One frame is 8 bytes, so a stored track costs `8 * sample_rate` bytes per
/// second of covered timeline (384 KB per track-second at 48 kHz).
struct PostFxSnapshot {
    frames: usize,
    samples: Vec<f32>,
    used: u64,
}

/// Default byte cap for post-FX snapshots. One cached track-second of a stereo
/// 48 kHz render costs 384 KB, so this holds about 170 track-seconds.
const DEFAULT_POST_FX_BUDGET_BYTES: usize = 64 * 1024 * 1024;

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            worker_options: None,
            vst3_tail_probe: vst3_tail::execute_probe,
            vst2_tail_probe: vst2_prepare::execute_probe,
            vst2_group_renderer: vst2_group::render,
            track_buffers: Vec::new(),
            track_dirty: Vec::new(),
            track_source_latencies: Vec::new(),
            track_source_tails: Vec::new(),
            lane_targets: Vec::new(),
            track_clip_fingerprints: Vec::new(),
            track_ramp_fingerprints: Vec::new(),
            aux_pairs: Vec::new(),
            aux_buffers: HashMap::new(),
            pattern_buffers: HashMap::new(),
            pattern_dirty: HashMap::new(),
            pattern_to_tracks: HashMap::new(),
            duration_seconds: 0.0,
            render_horizon_samples: 0,
            minimum_duration_seconds: 0.0,
            sample_rate: 44100,
            mix_dirty: true,
            master_buffer: None,
            stem_capture: None,
            consume_source_buffers: false,
            resource_root: None,
            pre_roll_seconds: 0.0,
            native_state_stamps: HashMap::new(),
            // A SoundFont-only leaf cache with no consumer today; opt in
            // explicitly so an all-builtin project never opens a font.
            pattern_previews: false,
            post_fx_snapshots: Vec::new(),
            post_fx_budget_bytes: DEFAULT_POST_FX_BUDGET_BYTES,
            post_fx_bytes: 0,
            post_fx_clock: 0,
            full_mix_widths: Vec::new(),
            master_latency_samples: 0,
            control_only: Vec::new(),
            cancel: None,
            progress: None,
            #[cfg(any(test, feature = "test-support"))]
            counters: RenderCounters::default(),
        }
    }

    /// Set the worker snapshot for subsequent requests through this cache.
    /// An unset override retains the inherited CLI pin at each render boundary.
    pub fn set_worker_options(&mut self, options: crate::render_parallel::RenderWorkerOptions) {
        self.worker_options = Some(options);
    }

    /// Inject only the external metadata query; request assembly and tail
    /// reduction remain shared with normal offline rendering.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_vst3_tail_probe(
        &mut self,
        probe: fn(
            oc_host::vst3_instance::Vst3OwnedAuthoredPlan,
        ) -> Result<u32, oc_host::vst3_instance::Vst3OwnedError>,
    ) {
        self.vst3_tail_probe = probe;
    }

    /// Inject only the UI-owned VST2 tail query, retaining production request assembly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_vst2_tail_probe(
        &mut self,
        probe: fn(
            oc_host::vst2::Vst2OwnedAuthoredPlan,
        ) -> Result<Option<u32>, oc_host::vst2::Vst2OwnedError>,
    ) {
        self.vst2_tail_probe = probe;
    }

    /// The injected boundary replaces external group execution only; cache and
    /// publication behavior still use the production render owner.
    #[cfg(test)]
    pub(crate) fn set_vst2_group_renderer(&mut self, renderer: vst2_group::Vst2GroupRenderer) {
        self.vst2_group_renderer = renderer;
    }

    /// Poll `token` between source chunks; a cancelled render publishes nothing.
    pub fn set_cancel_token(&mut self, token: crate::render_cancel::CancelToken) {
        self.cancel = Some(token);
    }

    /// Report how much of a source pass has completed.
    pub fn set_progress(&mut self, progress: crate::render_cancel::ProgressCallback) {
        self.progress = Some(progress);
    }

    /// Drop the render controls so a cache kept for later windows carries none.
    pub fn clear_render_controls(&mut self) {
        self.cancel = None;
        self.progress = None;
    }

    /// Report completed source tracks as a fraction of the project.
    fn report_progress(&self, stage: crate::render_cancel::RenderStage, done: usize, total: usize) {
        if let Some(progress) = &self.progress {
            progress(crate::render_cancel::RenderProgress {
                stage,
                fraction: (done as f32 / total.max(1) as f32).clamp(0.0, 1.0),
            });
        }
    }

    /// Byte cap for post-track-FX snapshots; `0` disables and drops them.
    /// Each stored track costs `8 * sample_rate` bytes per covered second.
    pub fn set_post_fx_snapshot_budget(&mut self, bytes: usize) {
        self.post_fx_budget_bytes = bytes;
        self.trim_post_fx_snapshots();
    }

    /// Bytes currently held by post-track-FX snapshots.
    pub fn post_fx_snapshot_bytes(&self) -> usize {
        self.post_fx_bytes
    }

    /// Test-only counters, for asserting what a request did and did not run.
    #[cfg(test)]
    pub(crate) fn render_counters(&self) -> RenderCounters {
        self.counters
    }

    /// Frames covered by one track's snapshot, if it has one.
    pub fn post_fx_snapshot_frames(&self, track: usize) -> Option<usize> {
        self.post_fx_snapshots
            .get(track)
            .and_then(|snapshot| snapshot.as_ref())
            .map(|snapshot| snapshot.frames)
    }

    /// Skip the pattern preview SoundFont pass when nothing will read it.
    pub fn set_pattern_previews(&mut self, enabled: bool) {
        self.pattern_previews = enabled;
    }

    /// Opt-in extra offline buffers. Disabled by default to avoid GUI memory overhead.
    pub fn capture_stems(&mut self, enabled: bool) {
        if enabled && self.stem_capture.is_some() {
            return;
        }
        self.stem_capture = enabled.then(crate::graph::StemCapture::default);
        self.mix_dirty = true;
    }

    /// Track/bus export without additional full-length master diagnostic taps.
    pub fn capture_export_stems(&mut self) {
        self.stem_capture = Some(crate::graph::StemCapture {
            capture_master_inputs: false,
            ..Default::default()
        });
        self.mix_dirty = true;
    }

    /// Capture each track's post-insert, pre-fader chain output. Offline device
    /// analysis reads the signal an insert sees, before the pan law.
    pub fn capture_track_pre_fader(&mut self) {
        self.stem_capture = Some(crate::graph::StemCapture {
            capture_master_inputs: false,
            pre_fader_tracks: true,
            ..Default::default()
        });
        self.mix_dirty = true;
    }

    /// True when the cache was asked for post-insert, pre-fader timbre taps.
    fn pre_fader_capture(&self) -> bool {
        self.stem_capture
            .as_ref()
            .is_some_and(|capture| capture.pre_fader_tracks)
    }

    /// One track's post-insert, pre-fader signal over `[start_sample, end_sample)`.
    ///
    /// The window is a slice of the same timeline a full mix renders, so the
    /// source, effect state and latency plan are the ones the mix uses. Send,
    /// bus and master paths are excluded: the answer is the track tap, not a
    /// solo mix.
    #[allow(clippy::too_many_arguments)]
    pub fn render_track_tap_range(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        track: usize,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<Vec<f32>, String> {
        let name = project
            .tracks
            .get(track)
            .map(|track| track.name.clone())
            .ok_or_else(|| format!("track {track} out of range"))?;
        self.capture_track_pre_fader();
        let output = self.render_range(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RangeRenderRequest {
                start_sample,
                end_sample,
                tracks: Some(vec![track]),
                stems: true,
            },
        )?;
        output
            .stems
            .into_iter()
            .find(|(_, stem, _)| stem == &name)
            .map(|(_, _, samples)| samples)
            .ok_or_else(|| format!("the render captured no signal for track '{name}'"))
    }

    /// Consuming one-shot export: no reusable dry/master cache is retained.
    /// Ownership prevents callers from subsequently remixing an emptied cache.
    pub fn render_export(
        mut self,
        project: &Project,
        soundfont: &Path,
        sample_rate: u32,
    ) -> Result<(Vec<f32>, crate::graph::StemCapture), String> {
        self.capture_export_stems();
        self.consume_source_buffers = true;
        let audio = self.render_full(project, soundfont, sample_rate, &mut HashMap::new())?;
        let stems = self.stem_capture.take().ok_or("missing export stems")?;
        Ok((audio, stems))
    }

    /// Explicit offline export bounds, processed through the complete graph.
    /// Zero retains the historical event-plus-tail policy. Never trims events.
    pub fn set_minimum_duration(&mut self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
            return Err("minimum render duration must be finite and within 0..600 seconds".into());
        }
        if self.minimum_duration_seconds != seconds {
            self.minimum_duration_seconds = seconds;
            self.mark_all_dirty();
        }
        Ok(())
    }

    pub fn stems(&self) -> Option<&crate::graph::StemCapture> {
        if self.is_dirty() {
            None
        } else {
            self.stem_capture.as_ref()
        }
    }

    pub fn set_resource_root(&mut self, root: Option<PathBuf>) {
        if self.resource_root != root {
            self.resource_root = root;
            self.mark_all_dirty();
        }
    }

    /// Silent warm-up before a plugin-backed render. The loader runs on a
    /// background thread, so this is wall-clock time, not render length.
    pub fn set_pre_roll_seconds(&mut self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
            return Err("pre-roll must be finite and within 0..600 seconds".into());
        }
        if self.pre_roll_seconds != seconds {
            self.pre_roll_seconds = seconds;
            self.mark_all_dirty();
        }
        Ok(())
    }

    pub fn pre_roll_seconds(&self) -> f64 {
        self.pre_roll_seconds
    }

    /// Mark a pattern as dirty (needs re-render).
    /// Only marks tracks that actually reference this pattern via clips.
    pub fn mark_pattern_dirty(&mut self, name: &str) {
        self.pattern_dirty.insert(name.to_string(), true);
        // Dirty only tracks that reference this pattern via clips
        if let Some(track_indices) = self.pattern_to_tracks.get(name).cloned() {
            for i in track_indices {
                if i < self.track_dirty.len() {
                    self.track_dirty[i] = true;
                }
                self.forget_post_fx_snapshot(i);
            }
            self.mix_dirty = true;
        } else {
            // No mapping yet (first render hasn't happened), mark all dirty
            self.mark_all_dirty();
        }
    }

    /// Mark a specific track as dirty (needs re-render).
    pub fn mark_track_dirty(&mut self, track_index: usize) {
        if track_index < self.track_dirty.len() {
            self.track_dirty[track_index] = true;
        }
        self.forget_post_fx_snapshot(track_index);
        self.mix_dirty = true;
    }

    /// Mark all tracks as dirty (full re-render needed).
    pub fn mark_all_dirty(&mut self) {
        for d in &mut self.track_dirty {
            *d = true;
        }
        // A full re-render rebuilds track buffers; snapshots no longer match.
        self.clear_post_fx_snapshots();
        self.mix_dirty = true;
    }

    /// Mark only the mix as dirty (track buffers valid, but routing/effects changed).
    pub fn mark_mix_dirty(&mut self) {
        self.mix_dirty = true;
    }

    /// Check if anything needs re-rendering.
    pub fn is_dirty(&self) -> bool {
        self.mix_dirty
            || self.track_dirty.iter().any(|&d| d)
            || self
                .native_state_stamps
                .iter()
                .any(|(path, stamp)| native_state_stamp(path).as_ref() != Ok(stamp))
    }

    /// Get the cached master buffer (may be stale if dirty).
    pub fn master_buffer(&self) -> Option<&[f32]> {
        self.master_buffer.as_deref()
    }

    /// Get a specific track's cached buffer if it's clean.
    pub fn get_track_buffer(&self, index: usize) -> Option<&[f32]> {
        if index < self.track_dirty.len() && !self.track_dirty[index] {
            self.track_buffers.get(index).and_then(|b| b.as_deref())
        } else {
            None
        }
    }

    pub fn duration(&self) -> f64 {
        self.duration_seconds
    }

    pub fn sr(&self) -> u32 {
        self.sample_rate
    }

    /// Get a cached pattern buffer if available and not dirty.
    pub fn get_pattern_buffer(&self, name: &str) -> Option<&[f32]> {
        if self.pattern_dirty.get(name).copied().unwrap_or(true) {
            None
        } else {
            self.pattern_buffers.get(name).map(|b| b.as_slice())
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    pub fn num_tracks(&self) -> usize {
        self.track_buffers.len()
    }

    /// How many tracks have cached buffers.
    pub fn cached_count(&self) -> usize {
        self.track_buffers.iter().filter(|b| b.is_some()).count()
    }

    /// How many tracks need re-rendering.
    pub fn dirty_count(&self) -> usize {
        self.track_dirty.iter().filter(|&&d| d).count()
    }
}
