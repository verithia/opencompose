//! Audio engine state and real-time audio I/O.
//!
//! Two playback modes:
//! 1. **Streaming** (default): Background render thread produces audio in chunks,
//!    audio callback reads from a lock-free ring buffer. Instant playback start.
//! 2. **Legacy** (fallback/export): Pre-render entire project into a monolithic buffer.
//!    Used for WAV export and preview functions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use oc_audio::audio_io::{
    AudioIo, AudioSelection, AudioStatus, InputSelection, ManagedStream, OutputDevice, RenderSink,
    RenderSource, StreamSink, StreamSpec,
};

use crate::input::{InputPort, InputSlot, MonitorOnlyOutput, MonitorSource};
use serde::Serialize;

use oc_dsl::ast::{BarBeat, Project};
use oc_engine::render_cache::RenderCache;

use crate::audio_slot::{CallbackRoute, OutputSlot};
#[cfg(test)]
use crate::audition_figure::FigureVoice;
use crate::first_block::{StartPath, now_ns};
use crate::render_thread::{RenderCommand, RenderThreadHandle, StreamingPlayback};
#[cfg(test)]
use oc_engine::render_cache::RangeRenderRequest;

// ============================================================================
// Shared playback state (lock-free, used by audio callback)
// ============================================================================

pub struct AudioPlayback {
    pub buffer: Vec<f32>,
    /// A/B pair: the second take. Both buffers share one cursor, so switching
    /// sides keeps the playback position and is just a read-pointer change.
    pub alt: Option<Vec<f32>>,
    pub cursor: AtomicU64,
    pub playing: AtomicBool,
    /// False reads `buffer`, true reads `alt`; ignored when `alt` is `None`.
    pub side: AtomicBool,
    /// Press-to-first-block trace; this path plays an already rendered buffer.
    pub first_block: crate::first_block::FirstBlock,
    stream_error: Mutex<Option<String>>,
}

type AuditionPlaybackStream = (Arc<AudioPlayback>, StreamSink, Arc<dyn RenderSource>);

impl AudioPlayback {
    /// The take the callback should read right now.
    pub(crate) fn active_buffer(&self) -> &[f32] {
        if self.side.load(Ordering::Acquire)
            && let Some(alt) = &self.alt
        {
            return alt;
        }
        &self.buffer
    }
}

/// One pad strike and its kit resources, borrowed while preparing a preview.
pub(crate) struct DrumPadPreview<'a> {
    pub params: &'a [(String, oc_dsl::ast::ParamValue)],
    pub root: Option<&'a Path>,
    pub note: u8,
    pub velocity: u8,
}

/// A builtin synth audition already streaming on a private render thread.
pub(crate) struct PreparedAuditionStream {
    thread: RenderThreadHandle,
    pair: Option<(
        RenderThreadHandle,
        Arc<AtomicBool>,
        Arc<[std::sync::atomic::AtomicU32; 2]>,
    )>,
    end_frame: Option<u64>,
    start_frame: u64,
    sample_rate: u32,
    stream: Option<StreamSink>,
    source: Option<Arc<dyn RenderSource>>,
    #[cfg(test)]
    simulated: Option<SimulatedOutput>,
}

impl PreparedAuditionStream {
    pub(crate) fn pair_gains(&self) -> Option<Arc<[std::sync::atomic::AtomicU32; 2]>> {
        self.pair.as_ref().map(|(_, _, gains)| gains.clone())
    }
}

// ============================================================================
// Audio subsystem
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Audio actively streaming (or about to start)
    Playing,
    /// Paused at current position, can resume instantly
    Paused,
    /// Stopped / not enabled — cursor at 0, needs render to start
    Stopped,
}

pub struct AudioState {
    pub soundfont_path: Option<PathBuf>,
    /// Resolved rate of the current output device; every render thread uses it.
    pub sample_rate: u32,
    /// Owner of the device list, the chosen device and the negotiated format.
    pub audio_io: Arc<AudioIo>,
    /// The main output stream (transport, legacy playback or the live synth).
    main_stream: Arc<OutputSlot>,
    main_callback_route: Arc<CallbackRoute>,
    idle_monitor: Option<MonitorOnlyOutput>,
    monitor_output_error: Option<String>,
    /// The audition stream, separate so a preview never moves the transport.
    audition_slot: Arc<OutputSlot>,
    /// The capture stream; opened on demand (preferences, monitoring, record).
    input_slot: Arc<InputSlot>,
    /// Lock-free capture path: monitor ring, level meter and take ring.
    pub(crate) input_port: Arc<InputPort>,
    /// Why the capture stream is not open, when it should be.
    input_error: Option<String>,
    /// The audition click: level, toggle and count-in choice.
    pub(crate) click: oc_audio::click::ClickSettings,
    /// Armed track and any take in progress.
    pub(crate) recorder: crate::record::Recorder,
    /// The MIDI take, its capture buffer and its settings.
    pub(crate) midi_recorder: crate::midi_record::MidiRecorder,
    /// Take auditioned through the live render thread. Session state: it never
    /// reaches the DSL, so it cannot survive into an export.
    pub(crate) take_solo: Option<(String, u32)>,
    /// Count-in queued for the next play.
    pub(crate) pending_count_in: Option<oc_audio::click::CountInPlan>,
    /// Legacy: Cached render — stays alive across pause/resume (used for previews + export)
    pub playback: Option<Arc<AudioPlayback>>,
    /// Preset audition playback; kept apart so the transport cursor never moves.
    pub(crate) audition_playback: Option<Arc<AudioPlayback>>,
    /// Builtin synth auditions stream through a private render thread.
    pub(crate) audition_render: Option<RenderThreadHandle>,
    audition_pair: Option<(
        RenderThreadHandle,
        Arc<AtomicBool>,
        Arc<[std::sync::atomic::AtomicU32; 2]>,
    )>,
    audition_output_error: Option<String>,
    pub(crate) audition_paused: bool,
    pub(crate) audition_pause_intent: Arc<AtomicBool>,
    pub(crate) transport_pause_intent: Arc<AtomicBool>,
    audition_end_frame: Option<u64>,
    audition_start_frame: u64,
    audition_sample_rate: u32,
    #[cfg(test)]
    audition_render_simulated: Option<SimulatedOutput>,
    /// Whether the project has changed since last render (need re-render on next play)
    pub render_dirty: bool,
    /// Per-track incremental render cache — used for legacy offline renders (export, preview)
    pub render_cache: Option<RenderCache>,
    pub resource_root: Option<PathBuf>,
    /// Background streaming render thread (used for main playback)
    pub render_thread: Option<RenderThreadHandle>,
    /// Whether we're using streaming mode (true) or legacy mode (false) for current playback
    pub streaming_active: bool,
    /// User-facing transport state: Playing, Paused, or Stopped.
    /// - Playing: user pressed play, audio is (or will be) streaming
    /// - Paused: user pressed pause, position retained, can resume
    /// - Stopped: user pressed stop or never started, position at 0
    pub playback_state: PlaybackState,
    /// Shared with an open editor's preview monitor: false while the transport
    /// plays, so the same instrument is not heard twice.
    monitor_gate: Arc<AtomicBool>,
    pending_seek_frame: Option<u64>,
    /// Latest live seek and its worker acknowledgement generation.
    pending_live_seek: Option<(u64, u64)>,
    /// End frame for the next play only; a track preview stops here.
    pending_play_end_frame: Option<u64>,
    /// Identity and end bound of the current solo graph; edits invalidate resume.
    track_preview: Option<(usize, Option<u64>)>,
    /// A play request whose first chunk has not been produced yet.
    pending_start: Option<PendingStart>,
    /// The press that starts audio, kept until the playback object exists.
    first_block_start: Option<(u64, StartPath)>,
    /// Latest non-fatal transport status; the UI renders it until audio flows.
    transport_status: Option<String>,
    /// A ProjectChanged has been sent and has not been observed as loaded yet.
    load_queued: bool,
    /// Newest FX-chain document waiting for the render thread. A drag replaces
    /// it, so one patch is in flight instead of one per input event.
    pending_chain: Option<Box<Project>>,
    #[cfg(test)]
    simulate_output: bool,
    #[cfg(test)]
    simulated_output: Option<SimulatedOutput>,
    #[cfg(test)]
    audition_simulated_output: Option<SimulatedAuditionOutput>,
    /// Active OpenSynth audition (keyboard or live MIDI preset name).
    live_synth: Option<String>,
    /// Live MPE zones for the audition instrument.
    mpe_config: Option<oc_synth::synth::MpeConfig>,
    /// Analyser target requested by the open panel; replayed on a new worker.
    spectrum_request: Option<String>,
    /// Shared producer slot: GUI and MIDI threads push, the audio callback pops.
    live_route: crate::live_synth::Route,
    /// Session loop region in beats. Playback only: it never joins the project
    /// and no render or export path reads it. The beats are the session state;
    /// the frames below are a cache of the current clock and device rate.
    loop_region: Option<(f64, f64)>,
    /// The same region in frames on the current clock, so a new render thread
    /// can be handed it. The project end is applied where it is read.
    loop_frames: Option<(u64, u64)>,
    /// Whether the transport loops the region; a cleared region disarms it.
    loop_enabled: bool,
    /// Punch range in beats. Session state like the loop region: a MIDI take
    /// reads it, and it is the natural home for an audio punch later.
    pub(crate) punch_region: Option<(f64, f64)>,
    /// Whether a take keeps only the notes inside the punch range.
    pub(crate) punch_enabled: bool,
    /// Fold the monitored master to mono. Playback only: no offline render or
    /// export path reads it, so a mono check never changes a written file.
    monitor_mono: bool,
}

/// The click's settings as the transport controls read them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClickSettingsView {
    pub enabled: bool,
    pub level_db: f64,
    pub count_in: String,
    /// Count-in is offered only while a track is armed for recording.
    pub count_in_available: bool,
}

impl ClickSettingsView {
    fn of(settings: oc_audio::click::ClickSettings, armed: usize) -> Self {
        Self {
            enabled: settings.enabled,
            level_db: settings.level_db,
            count_in: settings.count_in.key().to_string(),
            count_in_available: armed > 0,
        }
    }
}

/// A play request that has been dispatched but has no audible chunk yet.
#[derive(Clone, Copy)]
struct PendingStart {
    started: std::time::Instant,
    frame: u64,
    /// True when the request continues from the live cursor, not a seek.
    resume: bool,
    /// `play_applied` value before dispatch; the worker increments it when applied.
    before_applied: u64,
}

/// Bounded, asynchronous wait for the first chunk; never spins the UI thread.
const FIRST_CHUNK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// What a play press dispatched: streaming is live, or a legacy render is needed.
pub(crate) enum PlayDispatch {
    /// Playback is dispatched on the streaming path.
    Started { did_render: bool },
}

/// One enumerated output device; the audio-IO owner defines the shape.
pub type AudioDeviceInfo = OutputDevice;

/// The capture side of the audio section: what is open, how loud it is, and
/// whether monitoring is on.
#[derive(Debug, Clone, Serialize)]
pub struct InputView {
    /// The capture stream is open.
    pub open: bool,
    /// Name of the capture device that is open.
    pub device: Option<String>,
    pub channels: u16,
    pub sample_rate: u32,
    /// Peak absolute sample since the last read, 0..1.
    pub level_peak: f64,
    pub clipped: bool,
    pub monitoring: bool,
    pub monitor_gain_db: f64,
    /// Frames the monitor path dropped because the output did not keep up.
    pub monitor_dropped_frames: u64,
    pub monitor_error: Option<String>,
    /// Why capture is closed or failed.
    pub error: Option<String>,
}

/// The audio section's snapshot: the owner's status plus measured latency.
#[derive(Debug, Clone, Serialize)]
pub struct AudioIoView {
    pub status: AudioStatus,
    /// Measured device callback frames; 0 while no stream reports one.
    pub callback_frames: u32,
    /// One callback period plus the live master chain.
    pub latency_frames: u32,
    pub latency_seconds: f64,
    pub underruns: u64,
    pub input: InputView,
    /// Frames of capture the app reports as its own latency: one capture
    /// callback period plus the output path. Recording compensates by this.
    pub input_latency_frames: u32,
}

/// Audio configuration for the agent API, as one snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct AudioConfig {
    pub sample_rate: u32,
    /// Frames of the opened period; 0 when the device controls it.
    pub buffer_size: u32,
    pub output_device: Option<String>,
    pub soundfont_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransportSnapshot {
    pub playing: bool,
    pub position_beats: f64,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    /// True if play() triggered a full re-render (vs instant resume)
    #[serde(default)]
    pub did_render: bool,
    /// Warnings from plugin loading (e.g. "Plugin X not found")
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Play was dispatched but no audible chunk exists yet.
    #[serde(default)]
    pub starting: bool,
    /// A paused solo can resume by rebuilding at the new output rate.
    #[serde(default)]
    pub track_preview_rebuild: bool,
    /// Non-fatal transport status the UI should show (e.g. first-chunk failure).
    #[serde(default)]
    pub status: Option<String>,
    /// The project is still being built or pre-rendered in the background.
    #[serde(default)]
    pub warming: bool,
    /// The project is fully warm: sources built and the opening chunks queued.
    #[serde(default)]
    pub prepared: bool,
    /// Stage and progress of a project rebuild the render thread is running.
    #[serde(default)]
    pub build: Option<crate::build_progress::BuildSnapshot>,
    /// Current preset audition progress, set by `get_transport_state`.
    #[serde(default)]
    pub audition: Option<crate::audition::AuditionSnapshot>,
    /// An audition is still sounding; the transport itself may be paused.
    #[serde(default)]
    pub audition_active: bool,
    /// A live input or an editor can sound while the arrangement is paused.
    #[serde(default)]
    pub native_monitoring: bool,
    /// Queued notes, active voices or release audio in the live keyboard synth.
    #[serde(default)]
    pub live_synth_sounding: bool,
    /// The audition click's settings, as the transport controls show them.
    #[serde(default)]
    pub click: ClickSettingsView,
    /// Count-in frames still to sound before the timeline starts.
    #[serde(default)]
    pub count_in_remaining_frames: u64,
    /// Armed track, take progress and the last record report.
    #[serde(default)]
    pub record: crate::record::RecordState,
    /// The punch range a take keeps notes inside.
    #[serde(default)]
    pub punch: PunchState,
    /// The project as it stands after a stop that committed a take. Present
    /// only when the stop changed the project, so the UI adopts the new clip or
    /// notes without a full reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::commands::shared::ProjectSnapshot>,
    /// Seconds the live monitoring path holds the reported position back by:
    /// one device callback period plus the master path's reported latency. The
    /// position fields already include it; a buffered preview reports zero
    /// because it is not compensated here.
    #[serde(default)]
    pub monitor_latency_seconds: f64,
    /// Debug: press-to-first-block numbers for the current start. Absent when
    /// no path has started audio in this process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_block: Option<crate::first_block::FirstBlockSnapshot>,
}

/// The session loop region as the transport reports it.
///
/// The beats are what the user drew; `clamped` says the project end cuts the
/// region short, so playback wraps at the end of the project instead of at
/// `end_beats`.
#[derive(Debug, Clone, Serialize)]
pub struct LoopState {
    pub enabled: bool,
    pub start_beats: Option<f64>,
    pub end_beats: Option<f64>,
    pub clamped: bool,
}

/// The punch range a take keeps notes inside.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PunchState {
    pub enabled: bool,
    pub start_beats: Option<f64>,
    pub end_beats: Option<f64>,
}

/// A copy of `project` with every pass of the take's group muted except `take`.
///
/// This is the live audition overlay. It never mutates the document, so an
/// export and the DSL source keep the mute flags the user wrote.
pub(crate) fn take_solo_project(project: &Project, track: &str, take: u32) -> Project {
    let mut copy = project.clone();
    let Some(group) = copy
        .audio_clips()
        .iter()
        .find(|clip| clip.track == track && clip.take == Some(take))
        .and_then(|clip| clip.take_group.clone())
    else {
        return copy;
    };
    for clip in copy.audio_clips_mut() {
        if clip.track == track && clip.take_group.as_deref() == Some(group.as_str()) {
            clip.muted = clip.take != Some(take);
        }
    }
    copy
}

impl AudioState {
    fn build(audio_io: Arc<AudioIo>) -> Self {
        let main_stream = Arc::new(OutputSlot::new("main", Arc::clone(&audio_io)));
        let audition_slot = Arc::new(OutputSlot::new("audition", Arc::clone(&audio_io)));
        let input_port = Arc::new(InputPort::new());
        let input_slot = Arc::new(InputSlot::new(
            Arc::clone(&audio_io),
            Arc::clone(&input_port),
        ));
        audio_io.register_stream(main_stream.clone());
        audio_io.register_stream(audition_slot.clone());
        audio_io.register_stream(input_slot.clone());
        let sample_rate = audio_io.sample_rate();
        let click = audio_io.click_settings();
        Self {
            soundfont_path: find_default_soundfont(),
            sample_rate,
            audio_io,
            main_stream,
            main_callback_route: Arc::new(CallbackRoute::default()),
            idle_monitor: None,
            monitor_output_error: None,
            audition_slot,
            input_slot,
            input_port,
            input_error: None,
            click,
            recorder: crate::record::Recorder::default(),
            midi_recorder: crate::midi_record::MidiRecorder::default(),
            take_solo: None,
            pending_count_in: None,
            playback: None,
            audition_playback: None,
            audition_render: None,
            audition_pair: None,
            audition_output_error: None,
            audition_paused: false,
            audition_pause_intent: Arc::new(AtomicBool::new(false)),
            transport_pause_intent: Arc::new(AtomicBool::new(false)),
            audition_end_frame: None,
            audition_start_frame: 0,
            audition_sample_rate: 0,
            #[cfg(test)]
            audition_render_simulated: None,
            render_dirty: true,
            render_cache: None,
            resource_root: None,
            render_thread: None,
            streaming_active: false,
            playback_state: PlaybackState::Stopped,
            monitor_gate: Arc::new(AtomicBool::new(true)),
            pending_seek_frame: None,
            pending_live_seek: None,
            pending_play_end_frame: None,
            track_preview: None,
            pending_start: None,
            first_block_start: None,
            transport_status: None,
            load_queued: false,
            pending_chain: None,
            #[cfg(test)]
            simulate_output: false,
            #[cfg(test)]
            simulated_output: None,
            #[cfg(test)]
            audition_simulated_output: None,
            live_synth: None,
            mpe_config: None,
            spectrum_request: None,
            live_route: Arc::new(Mutex::new(None)),
            loop_region: None,
            loop_frames: None,
            loop_enabled: false,
            punch_region: None,
            punch_enabled: false,
            monitor_mono: false,
        }
    }

    /// The capture section's snapshot, without opening anything.
    pub fn input_view(&self) -> InputView {
        let status = self.audio_io.status();
        let (level, clipped) = self.input_port.take_level();
        let selection = status.input.clone();
        InputView {
            open: self.input_slot.is_open(),
            device: status
                .opened_input
                .as_ref()
                .map(|opened| opened.name.clone()),
            channels: status
                .opened_input
                .as_ref()
                .map_or(selection.input_channels.count(), |opened| opened.channels),
            sample_rate: status
                .opened_input
                .as_ref()
                .map_or(self.sample_rate, |opened| opened.sample_rate),
            level_peak: level,
            clipped,
            monitoring: selection.input_monitor,
            monitor_gain_db: selection.input_monitor_gain_db,
            monitor_dropped_frames: self.input_port.monitor_dropped_frames(),
            monitor_error: self.monitor_output_error.clone(),
            error: self.input_error.clone(),
        }
    }

    pub(crate) fn input_monitor_active(&self) -> bool {
        self.input_slot.is_open()
            && self.main_stream.is_open()
            && self.audio_io.input_selection().input_monitor
    }

    pub(crate) fn capture_format(&self) -> Option<(u32, u16)> {
        self.input_slot.format()
    }

    pub(crate) fn take_capture_failure(&mut self) -> Option<String> {
        let error = self.input_port.take_error()?;
        self.input_slot.close();
        self.audio_io.note_input_failure(&error);
        self.input_error = Some(error.clone());
        self.sync_idle_monitor();
        Some(error)
    }

    /// Open the capture stream if a device exists and nothing is open. The
    /// preferences section, monitoring and recording all call this.
    pub fn ensure_input(&mut self) {
        if self.input_slot.is_open() {
            return;
        }
        // Only explicit capture/device UI paths call this. Output startup and
        // transport snapshots must never enumerate microphone configurations.
        if let Err(error) = self.audio_io.refresh_inputs() {
            self.input_error = Some(error);
            return;
        }
        if !self.audio_io.has_inputs() {
            return;
        }
        match self.input_slot.open() {
            Ok(()) => {
                self.input_error = None;
                self.apply_input_monitor(&self.audio_io.input_selection());
            }
            Err(error) => {
                log::warn!("could not open the audio input: {error}");
                self.input_error = Some(error);
            }
        }
    }

    /// Apply a capture selection. A device, rate or channel change reopens the
    /// stream; monitoring and gain take effect immediately.
    pub fn set_input_io(&mut self, selection: InputSelection) -> AudioIoView {
        self.reap_idle_monitor_failure();
        let current = self.audio_io.input_selection();
        let format_changed = current.input_device != selection.input_device
            || current.input_channels != selection.input_channels;
        if self.recorder.recording() && format_changed {
            let mut view = self.audio_io_view();
            view.status.input_failure = Some(oc_audio::audio_io::AudioFailure::RecordingActive);
            return view;
        }
        if !format_changed
            && (self.recorder.recording()
                || current.input_monitor != selection.input_monitor
                || current.input_monitor_gain_db != selection.input_monitor_gain_db)
        {
            let applied = self
                .audio_io
                .apply_input_monitor(selection.input_monitor, selection.input_monitor_gain_db);
            if applied.input_monitor {
                self.ensure_input();
            }
            self.apply_input_monitor(&applied);
            self.sync_idle_monitor();
            return self.audio_io_view();
        }
        let before = self.audio_io.status().opened_input;
        let was_open = self.input_slot.is_open();
        let status = self.audio_io.apply_input(&selection);
        if let Some(failure) = &status.input_failure {
            self.input_error = Some(format_input_failure(failure));
            return self.audio_io_view();
        }
        let after = status.opened_input.clone();
        let format_changed = match (&before, &after) {
            (Some(before), Some(after)) => {
                before.key != after.key
                    || before.sample_rate != after.sample_rate
                    || before.channels != after.channels
            }
            _ => true,
        };
        if format_changed || !was_open {
            self.input_slot.close();
            self.ensure_input();
        }
        let applied = self.audio_io.input_selection();
        self.apply_input_monitor(&applied);
        self.sync_idle_monitor();
        self.audio_io_view()
    }

    fn apply_input_monitor(&self, applied: &InputSelection) {
        self.input_port
            .set_monitor_gain_db(applied.input_monitor_gain_db);
        self.input_port.set_monitoring(applied.input_monitor);
    }

    /// Use the existing main slot when there is no transport or synth output.
    fn sync_idle_monitor(&mut self) {
        if !self.input_slot.is_open() || !self.audio_io.input_selection().input_monitor {
            if self.idle_monitor.is_some() {
                self.drop_stream();
            }
            self.monitor_output_error = None;
            return;
        }
        if self.main_stream.is_open() {
            return;
        }
        let monitor = MonitorOnlyOutput::default();
        let source = self.main_source(Arc::new(monitor.clone()));
        match self.main_stream.open(source) {
            Ok(()) => {
                self.idle_monitor = Some(monitor);
                self.monitor_output_error = None;
            }
            Err(error) => self.monitor_output_error = Some(error),
        }
    }

    pub(crate) fn reap_idle_monitor_failure(&mut self) {
        if let Some(error) = self
            .idle_monitor
            .as_ref()
            .and_then(MonitorOnlyOutput::take_failure)
        {
            self.drop_stream();
            self.monitor_output_error = Some(error);
        }
    }

    /// Turn monitoring on or off without touching the device choice.
    pub fn set_input_monitor(&mut self, on: bool, gain_db: f64) -> AudioIoView {
        let mut selection = self.audio_io.input_selection();
        selection.input_monitor = on;
        selection.input_monitor_gain_db = gain_db;
        self.set_input_io(selection)
    }

    /// Wrap a main-output source so monitored input is summed into every block.
    fn main_source(&self, source: Arc<dyn RenderSource>) -> Arc<dyn RenderSource> {
        self.main_callback_route.wrap(Arc::new(MonitorSource::new(
            source,
            Arc::clone(&self.input_port),
        )))
    }

    fn transport_source(&self, source: Arc<dyn RenderSource>) -> Arc<dyn RenderSource> {
        let source = Arc::new(IntentGatedSource {
            source,
            gate: Arc::clone(&self.transport_pause_intent),
        });
        self.main_source(source)
    }

    pub fn new() -> Self {
        #[cfg(test)]
        {
            // Tests never touch the operator's audio device.
            Self::build(Arc::new(AudioIo::new(
                Arc::new(oc_audio::audio_io::NullBackend),
                None,
            )))
        }
        #[cfg(not(test))]
        {
            let io = Arc::new(AudioIo::new(
                Arc::new(oc_audio::audio_io::CpalBackend::new()),
                AudioIo::preferences_path(),
            ));
            io.ensure_loaded();
            Self::build(io)
        }
    }

    /// Build with an explicit device backend; tests use this to drive opens,
    /// failures and disconnects without hardware.
    #[cfg(test)]
    pub(crate) fn with_backend(backend: Arc<dyn oc_audio::audio_io::DeviceBackend>) -> Self {
        Self::build(Arc::new(AudioIo::new(backend, None)))
    }

    /// The preferences section's snapshot: owner status plus measured latency.
    pub fn audio_io_view(&self) -> AudioIoView {
        let status = self.audio_io.status();
        let callbacks = if status.stream_open {
            self.render_thread.as_ref()
        } else {
            None
        };
        let callback_frames = callbacks.map_or(0, |rt| {
            u32::try_from(rt.playback.device_buffer_frames.load(Ordering::Acquire)).unwrap_or(0)
        });
        let latency_frames = callbacks.map_or(0, |rt| {
            u32::try_from(rt.playback.monitor_latency_frames()).unwrap_or(u32::MAX)
        });
        let rate = status
            .opened
            .as_ref()
            .map_or(self.sample_rate, |opened| opened.sample_rate);
        let input_latency_frames = self.input_latency_frames();
        AudioIoView {
            status,
            callback_frames,
            latency_frames,
            latency_seconds: f64::from(latency_frames) / f64::from(rate.max(1)),
            underruns: callbacks.map_or(0, |rt| rt.playback.underruns.load(Ordering::Acquire)),
            input: self.input_view(),
            input_latency_frames,
        }
    }

    /// Capture frames the app attributes to its own latency: one capture
    /// callback period plus the output path. Recording shifts the take by this.
    pub fn input_latency_frames(&self) -> u32 {
        let capture_rate = self
            .capture_format()
            .map_or(self.sample_rate, |(rate, _)| rate);
        let input = crate::record::convert_frames(
            self.input_port.block_frames(),
            capture_rate,
            self.sample_rate,
        );
        let output = self
            .render_thread
            .as_ref()
            .map_or(0, |rt| rt.playback.monitor_latency_frames());
        u32::try_from(output.saturating_add(input)).unwrap_or(u32::MAX)
    }

    /// Change the output selection.
    ///
    /// A period change only rebuilds the streams; a rate change also drops the
    /// render thread and its ring, so the transport stays paused afterwards. A
    /// device change while playing resumes only when the open succeeded. The
    /// rate that counts is the one the stream reports it opened, not the one
    /// negotiation asked for: a device may answer a different rate, and only an
    /// answer that differs from the running render thread needs a rebuild.
    pub fn set_audio_io(&mut self, selection: AudioSelection) -> AudioIoView {
        self.reap_idle_monitor_failure();
        if self.recorder.recording() || self.midi_recorder.recording() {
            let current = self.audio_io.selection();
            let mut view = self.audio_io_view();
            if current.output_device != selection.output_device
                || current.sample_rate != selection.sample_rate
                || current.buffer_frames != selection.buffer_frames
            {
                view.status.failure = Some(oc_audio::audio_io::AudioFailure::RecordingActive);
            }
            return view;
        }
        let _ = self.reap_live_synth_failure();
        let was_playing = self.playback_state == PlaybackState::Playing;
        if was_playing {
            self.pause();
        }
        let status = self.audio_io.apply(&selection);
        let opened = status
            .opened
            .as_ref()
            .map(|opened| opened.sample_rate)
            .filter(|_| self.audio_io.has_devices());
        if let Some(rate) = opened.filter(|&rate| rate != self.sample_rate) {
            self.adopt_sample_rate(rate);
        } else if was_playing && status.failure.is_none() {
            let _ = self.resume_streaming();
        }
        self.sync_idle_monitor();
        self.audio_io_view()
    }

    /// Re-enumerate devices, restore a reconnected stored device, and keep the
    /// render rate in step with the format that would open now.
    pub fn refresh_audio_io(&mut self) -> AudioIoView {
        self.reap_idle_monitor_failure();
        let _ = self.reap_live_synth_failure();
        if let Err(error) = self.audio_io.refresh() {
            log::warn!("could not list audio devices: {error}");
        }
        if !self.recorder.recording() && !self.midi_recorder.recording() {
            self.audio_io.reconnect();
        }
        self.sync_sample_rate();
        self.sync_idle_monitor();
        self.audio_io_view()
    }

    /// The last measured first block, without publishing the debug line the
    /// transport poll owns. Read-only: the Performance page samples it.
    pub fn first_block_readout(&self) -> Option<crate::first_block::FirstBlockSnapshot> {
        let trace = self
            .streaming_active
            .then(|| {
                self.render_thread
                    .as_ref()
                    .map(|rt| Arc::clone(&rt.playback.first_block))
            })
            .flatten();
        match trace {
            Some(trace) => Some(trace.snapshot()),
            None => self.playback.as_ref().map(|pb| pb.first_block.snapshot()),
        }
    }

    /// Adopt the device's rate as the render rate. A change rebuilds the render
    /// thread and leaves the transport paused: every rendered buffer would
    /// otherwise play at the wrong speed.
    ///
    /// The rate is the one the open stream reports, so a device that answers a
    /// different rate than negotiation asked for is followed rather than
    /// assumed away.
    fn sync_sample_rate(&mut self) {
        // Headless runs and machines with no output keep the fallback rate.
        if !self.audio_io.has_devices() {
            return;
        }
        let rate = self
            .audio_io
            .status()
            .opened
            .as_ref()
            .map_or(self.sample_rate, |opened| opened.sample_rate);
        if rate == self.sample_rate {
            return;
        }
        if self.recorder.recording() {
            // Finalize against the original transport clock before rebuilding.
            self.input_port
                .interrupt_recording("The output device changed its sample rate");
            self.pause();
            return;
        }
        if self.playback_state == PlaybackState::Playing {
            self.pause();
            self.transport_status = Some("The output device changed its sample rate".into());
        }
        self.adopt_sample_rate(rate);
    }

    fn adopt_sample_rate(&mut self, rate: u32) {
        let old_rate = u128::from(self.sample_rate.max(1));
        let rescale = |frame: u64| {
            let frame = (u128::from(frame) * u128::from(rate) + old_rate / 2) / old_rate;
            frame.min(u128::from(u64::MAX)) as u64
        };
        self.pending_seek_frame = self
            .pending_seek_frame
            .or_else(|| {
                self.render_thread
                    .as_ref()
                    .map(|rt| rt.playback.play_cursor_frames.load(Ordering::Acquire))
            })
            .or_else(|| {
                self.playback
                    .as_ref()
                    .map(|pb| pb.cursor.load(Ordering::Acquire) / 2)
            })
            .map(rescale);
        self.pending_play_end_frame = self.pending_play_end_frame.map(rescale);
        self.track_preview = self
            .track_preview
            .map(|(index, end)| (index, end.map(rescale)));
        self.stop_audition();
        self.drop_stream();
        self.render_thread = None;
        self.playback = None;
        self.render_dirty = true;
        self.load_queued = false;
        self.pending_start = None;
        self.sample_rate = rate;
        let had_input = self.input_slot.is_open();
        self.input_slot.close();
        if had_input {
            self.ensure_input();
        }
        self.sync_idle_monitor();
    }

    /// Resume the streaming transport on the stream that a sweep rebuilt.
    fn resume_streaming(&mut self) -> Result<(), String> {
        if !self.streaming_active {
            return Ok(());
        }
        let Some(rt) = self.render_thread.as_ref() else {
            return Ok(());
        };
        let requested = self.pending_seek_frame.take();
        let resume = requested.is_none();
        let frame =
            requested.unwrap_or_else(|| rt.playback.play_cursor_frames.load(Ordering::Acquire));
        let before_applied = rt.playback.play_applied.load(Ordering::Acquire);
        rt.send(RenderCommand::Play {
            start_frame: frame,
            resume,
            end_frame: self.track_preview.and_then(|(_, end)| end),
        });
        self.set_playback_state(PlaybackState::Playing);
        self.pending_start = Some(PendingStart {
            started: std::time::Instant::now(),
            frame,
            resume,
            before_applied,
        });
        self.transport_status = None;
        Ok(())
    }

    /// Gate for an open VST3 editor's preview monitor stream.
    pub fn monitor_gate(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.monitor_gate)
    }

    fn set_playback_state(&mut self, state: PlaybackState) {
        self.playback_state = state;
        self.monitor_gate
            .store(state != PlaybackState::Playing, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn enable_simulated_output(&mut self) {
        self.simulate_output = true;
    }

    #[cfg(test)]
    pub(crate) fn simulated_output(&self) -> bool {
        self.simulate_output
    }

    pub fn config(&self) -> AudioConfig {
        let status = self.audio_io.status();
        let opened = status.opened;
        AudioConfig {
            sample_rate: opened
                .as_ref()
                .map_or(self.sample_rate, |opened| opened.sample_rate),
            // 0 means "the device controls the period".
            buffer_size: opened
                .as_ref()
                .and_then(|opened| opened.buffer_frames)
                .unwrap_or(0),
            output_device: opened.map(|opened| opened.name),
            soundfont_path: self
                .soundfont_path
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }

    /// Current gain-reduction/peak meters; `available` is false on legacy playback.
    /// Point the analyser at one node; None stops all analyser work.
    pub fn set_spectrum(&mut self, target: Option<String>) {
        self.spectrum_request = target.clone();
        if let Some(handle) = &self.render_thread {
            let _ = handle.cmd_tx.send(RenderCommand::Spectrum { target });
        }
    }

    /// The audible render position, independent of any parked seek.
    #[cfg(test)]
    pub fn playhead_frame(&self) -> u64 {
        self.render_thread
            .as_ref()
            .map_or(0, |rt| rt.playback.audible_frames())
    }

    /// Audible progress of the current recording run. A parked seek or an
    /// unacknowledged Play may still expose the previous run's device cursor.
    pub(crate) fn take_progress_frame(&self) -> u64 {
        self.recording_playback()
            .map_or(0, StreamingPlayback::audible_frames)
    }

    /// Audio loop alignment needs elapsed device frames even after a wrap.
    pub(crate) fn audio_take_progress_frame(&self) -> u64 {
        self.recording_playback()
            .map_or(0, StreamingPlayback::unwrapped_take_frame)
    }

    fn recording_playback(&self) -> Option<&StreamingPlayback> {
        let Some(rt) = &self.render_thread else {
            return None;
        };
        let pb = &rt.playback;
        if self.pending_seek_frame.is_some()
            || self.pending_count_in.is_some()
            || self.pending_start.is_some_and(|start| {
                pb.play_applied.load(Ordering::Acquire) == start.before_applied
            })
            || pb.count_in_remaining.load(Ordering::Acquire) != 0
        {
            return None;
        }
        Some(pb)
    }

    /// The timeline position used for starting takes and placing material.
    /// The parked seek is authoritative even before a render thread exists.
    pub(crate) fn timeline_cursor_frame(&self) -> u64 {
        if let Some(frame) = self.pending_seek_frame {
            return frame;
        }
        if (self.streaming_active || self.playback.is_none())
            && let Some(rt) = &self.render_thread
        {
            let pb = &rt.playback;
            let remaining = pb.count_in_remaining.load(Ordering::Acquire);
            let total = pb.total_frames.load(Ordering::Acquire);
            let played_out = self.pending_start.is_none() && pb.has_played_out();
            return self.streaming_cursor_frame(pb, remaining, total, played_out);
        }
        self.playback
            .as_ref()
            .map_or(0, |pb| pb.cursor.load(Ordering::Acquire) / 2)
    }

    fn streaming_cursor_frame(
        &self,
        pb: &StreamingPlayback,
        count_in_remaining: u64,
        total_frames: u64,
        played_out: bool,
    ) -> u64 {
        // Hold the requested position until Play applies, and then hold the
        // record position until the count-in reaches the first timeline block.
        let frames = match self.pending_start {
            Some(pending) => pending.frame,
            None if count_in_remaining > 0 => pb.count_in_start.load(Ordering::Acquire),
            None => pb.audible_frames(),
        };
        if played_out {
            total_frames
        } else if total_frames > 0 {
            frames.min(total_frames)
        } else {
            frames
        }
    }

    /// Move the transport to `frame` without starting it.
    pub fn seek_frame(
        &mut self,
        frame: u64,
        clock: &oc_dsl::timing::ProjectClock,
    ) -> Result<(), String> {
        let quarter = clock.position_at_sample(frame, self.sample_rate)?.quarter;
        self.seek(quarter, clock)
    }

    /// Send the queued count-in to the render thread, ahead of the play.
    fn replay_count_in(&mut self) {
        let Some(plan) = self.pending_count_in.take() else {
            return;
        };
        if let Some(handle) = &self.render_thread {
            let _ = handle.cmd_tx.send(RenderCommand::CountIn { plan });
        }
    }

    /// Send the click settings to a freshly spawned render thread.
    fn replay_click(&self) {
        if let Some(handle) = &self.render_thread {
            let _ = handle.cmd_tx.send(RenderCommand::Click {
                settings: self.click,
            });
        }
    }

    /// The click's settings as the transport shows them.
    pub fn click_view(&self) -> ClickSettingsView {
        ClickSettingsView::of(self.click, 0)
    }

    /// Change the click: level, toggle or count-in choice.
    pub fn set_click(&mut self, settings: oc_audio::click::ClickSettings) -> ClickSettingsView {
        self.click = self.audio_io.apply_click(settings);
        self.replay_click();
        self.click_view()
    }

    fn replay_spectrum_request(&self) {
        if let Some(handle) = &self.render_thread {
            let _ = handle.cmd_tx.send(RenderCommand::Spectrum {
                target: self.spectrum_request.clone(),
            });
        }
    }

    pub fn meter_snapshot(&self) -> crate::meters::MeterSnapshot {
        match &self.render_thread {
            Some(handle) => handle
                .playback
                .meters
                .snapshot(handle.playback.active.load(Ordering::Acquire)),
            None => crate::meters::MeterSnapshot::unavailable(),
        }
    }

    /// The meter hub of the live stream, if one is open.
    fn meter_hub(&self) -> Option<Arc<crate::meters::MeterHub>> {
        self.render_thread
            .as_ref()
            .map(|handle| Arc::clone(&handle.playback.meters))
    }

    /// Fold the monitored master to mono.
    ///
    /// This is transport monitoring, not project data: it never reaches the
    /// DSL, the offline renderer or an export, so a written file is the same
    /// with the mono check on or off.
    pub fn set_monitor_mono(
        &mut self,
        enabled: bool,
    ) -> Result<crate::meters::MonoCheckSnapshot, String> {
        let Some(hub) = self.meter_hub() else {
            return Err("Mono check needs streaming playback.".into());
        };
        if enabled && hub.master_channels() != oc_engine::graph::MASTER_CHANNELS {
            return Err("Mono check needs a stereo master.".into());
        }
        hub.set_mono_requested(enabled);
        self.monitor_mono = enabled;
        Ok(hub.snapshot(false).mono)
    }

    /// Re-push the mono request after the render thread is (re)created.
    fn apply_mono_check(&self) {
        if let Some(hub) = self.meter_hub() {
            hub.set_mono_requested(self.monitor_mono);
        }
    }

    /// Clear every clip latch; the only way a latched indicator goes out.
    pub fn reset_meter_clip(&self) {
        if let Some(hub) = self.meter_hub() {
            hub.reset_clips();
        }
    }

    pub fn transport_snapshot(
        &mut self,
        clock: &oc_dsl::timing::ProjectClock,
    ) -> Result<TransportSnapshot, String> {
        self.reap_idle_monitor_failure();
        let _ = self.reap_live_synth_failure();
        self.reap_audition_failure();
        // The poll is the one place a chain document left waiting by a drag is
        // handed over once the previous patch landed.
        self.pump_chain_change();
        // A history step or a load that finished since the last poll can change
        // the clock or the project end without passing through an edit. The
        // poll reconciles the seam, and an unchanged pair costs nothing.
        self.retime_loop(clock);
        let (warming, prepared, total_frames) = self.warm_flags();
        let track_preview_rebuild = self.track_preview_needs_rebuild();
        let audition_active = self.audition_active();
        let first_block = self.first_block_snapshot();
        if let Some(frame) = self.pending_seek_frame {
            return Ok(TransportSnapshot {
                playing: false,
                position_beats: clock.position_at_sample(frame, self.sample_rate)?.quarter,
                position_seconds: frame as f64 / self.sample_rate as f64,
                duration_seconds: if track_preview_rebuild {
                    self.track_preview.and_then(|(_, end)| end).unwrap_or(0)
                } else {
                    total_frames
                } as f64
                    / self.sample_rate as f64,
                did_render: false,
                warnings: vec![],
                starting: false,
                track_preview_rebuild,
                status: self.transport_status.clone(),
                warming,
                prepared,
                build: crate::build_progress::snapshot(),
                audition: None,
                audition_active,
                native_monitoring: false,
                live_synth_sounding: false,
                click: ClickSettingsView::of(self.click, 0),
                count_in_remaining_frames: 0,
                record: self.record_state(0),
                punch: self.punch_state(),
                project: None,
                monitor_latency_seconds: 0.0,
                first_block,
            });
        }
        // Streaming mode: read from render thread's shared state. A warm thread
        // reports progress before the first play; legacy previews keep priority.
        if self.streaming_active || (self.render_thread.is_some() && self.playback.is_none()) {
            // A stream the backend reported dead must never keep saying Playing.
            if let Some(rt) = self.render_thread.as_ref()
                && let Some(error) = rt.playback.take_stream_failure()
            {
                if self.recorder.recording() {
                    self.input_port.interrupt_recording(&error);
                }
                self.audio_io.note_stream_failure("main", &error);
                // The stream is dead: drop it so nothing claims to be open.
                self.drop_stream();
                self.pause();
                self.transport_status = Some(format!("Audio output stopped: {error}"));
            }
            let (ready, loaded, applied, waiting_for_clip) = self
                .render_thread
                .as_ref()
                .map(|rt| {
                    (
                        rt.playback.ready.load(Ordering::Acquire)
                            && rt.playback.position_applied.load(Ordering::Acquire)
                                == rt.playback.position_requested.load(Ordering::Acquire),
                        rt.playback.project_loaded.load(Ordering::Acquire),
                        rt.playback.play_applied.load(Ordering::Acquire),
                        rt.playback.waiting_for_clip.load(Ordering::Acquire),
                    )
                })
                .unwrap_or((false, false, 0, false));
            let load_error = self.render_thread.as_ref().and_then(|rt| rt.load_error());
            if (loaded && self.load_queued) || waiting_for_clip {
                self.load_queued &= !loaded;
                if let Some(pending) = self.pending_start.as_mut() {
                    pending.started = std::time::Instant::now();
                }
            }
            self.poll_stream_state(ready, applied, load_error);
            if let Some(ref rt) = self.render_thread {
                let pb = &rt.playback;
                let starting = self.pending_start;
                let count_in_remaining = pb.count_in_remaining.load(Ordering::Acquire);
                let total_frames = pb.total_frames.load(Ordering::Acquire);
                let played_out = starting.is_none() && pb.has_played_out();
                let frames =
                    self.streaming_cursor_frame(pb, count_in_remaining, total_frames, played_out);
                let seconds = frames as f64 / self.sample_rate as f64;
                let beats = clock.position_at_sample(frames, self.sample_rate)?.quarter;
                let duration = total_frames as f64 / self.sample_rate as f64;
                let monitor_latency_seconds =
                    pb.monitor_latency_frames() as f64 / self.sample_rate as f64;

                // Transition Playing → Paused when render reaches end of project.
                // The cursor is timeline frames, so a count-in never brings the
                // end forward.
                if self.playback_state == PlaybackState::Playing && played_out {
                    self.set_playback_state(PlaybackState::Paused);
                }

                // playing = true only in Playing state
                let playing = self.playback_state == PlaybackState::Playing;

                return Ok(TransportSnapshot {
                    playing,
                    position_beats: beats,
                    position_seconds: seconds,
                    duration_seconds: duration,
                    did_render: false,
                    warnings: vec![],
                    starting: starting.is_some(),
                    track_preview_rebuild,
                    status: self.transport_status.clone(),
                    warming,
                    prepared,
                    build: crate::build_progress::snapshot(),
                    audition: None,
                    audition_active,
                    native_monitoring: false,
                    live_synth_sounding: false,
                    click: ClickSettingsView::of(self.click, 0),
                    count_in_remaining_frames: count_in_remaining,
                    record: self.record_state(count_in_remaining),
                    punch: self.punch_state(),
                    project: None,
                    monitor_latency_seconds,
                    first_block,
                });
            }
        }

        // Legacy mode: read from monolithic buffer playback
        if let Some(ref pb) = self.playback {
            let cursor = pb.cursor.load(Ordering::Acquire);
            let total_samples = pb.buffer.len() as u64;
            let frames = cursor / 2;
            let seconds = frames as f64 / self.sample_rate as f64;
            let beats = clock.position_at_sample(frames, self.sample_rate)?.quarter;
            let duration = (total_samples / 2) as f64 / self.sample_rate as f64;
            let playing = pb.playing.load(Ordering::Acquire);

            Ok(TransportSnapshot {
                playing,
                position_beats: beats,
                position_seconds: seconds,
                duration_seconds: duration,
                did_render: false,
                warnings: vec![],
                starting: false,
                track_preview_rebuild,
                status: self.transport_status.clone(),
                warming,
                prepared,
                build: crate::build_progress::snapshot(),
                audition: None,
                audition_active,
                native_monitoring: false,
                live_synth_sounding: false,
                click: ClickSettingsView::of(self.click, 0),
                count_in_remaining_frames: 0,
                record: self.record_state(0),
                punch: self.punch_state(),
                project: None,
                monitor_latency_seconds: 0.0,
                first_block,
            })
        } else {
            Ok(TransportSnapshot {
                playing: false,
                position_beats: 0.0,
                position_seconds: 0.0,
                duration_seconds: 0.0,
                did_render: false,
                warnings: vec![],
                starting: false,
                track_preview_rebuild,
                status: self.transport_status.clone(),
                warming,
                prepared,
                build: crate::build_progress::snapshot(),
                audition: None,
                audition_active,
                native_monitoring: false,
                live_synth_sounding: false,
                click: ClickSettingsView::of(self.click, 0),
                count_in_remaining_frames: 0,
                record: self.record_state(0),
                punch: self.punch_state(),
                project: None,
                monitor_latency_seconds: 0.0,
                first_block,
            })
        }
    }

    /// Debug numbers for the current start, plus one log line per start.
    ///
    /// The audio callback cannot log, so the transport poll publishes the line
    /// the first time it sees the first block.
    fn first_block_snapshot(&mut self) -> Option<crate::first_block::FirstBlockSnapshot> {
        let trace = self
            .streaming_active
            .then(|| {
                self.render_thread
                    .as_ref()
                    .map(|rt| Arc::clone(&rt.playback.first_block))
            })
            .flatten();
        let Some(trace) = trace else {
            return self.playback.as_ref().map(|pb| pb.first_block.snapshot());
        };
        if let Some(line) = trace.take_report() {
            log::debug!("[first_block] {line}");
        }
        Some(trace.snapshot())
    }

    /// Warm-up progress from the render thread: (warming, prepared, total frames).
    fn warm_flags(&self) -> (bool, bool, u64) {
        use crate::render_thread::{WARM_BUILDING, WARM_READY};
        match self.render_thread.as_ref() {
            Some(rt) => {
                let state = rt.playback.warm_state.load(Ordering::Acquire);
                let loaded = rt.playback.project_loaded.load(Ordering::Acquire);
                let failed = !loaded && rt.load_error().is_some();
                (
                    state == WARM_BUILDING || (self.load_queued && !loaded && !failed),
                    state == WARM_READY,
                    rt.playback.total_frames.load(Ordering::Acquire),
                )
            }
            None => (false, false, 0),
        }
    }

    /// Track the pending first block and report worker failures during playback.
    fn poll_stream_state(&mut self, ready: bool, applied: u64, error: Option<String>) {
        let Some(pending) = self.pending_start else {
            if self.playback_state == PlaybackState::Playing
                && let Some(error) = error
            {
                self.set_playback_state(PlaybackState::Paused);
                self.transport_status = Some(format!("Playback stopped: {error}"));
            }
            return;
        };
        // A resume has no fresh chunk to wait for: the lookahead is still intact.
        if pending.resume && ready && error.is_none() {
            self.pending_start = None;
            self.transport_status = None;
            return;
        }
        if applied != pending.before_applied && ready && error.is_none() {
            self.pending_start = None;
            self.transport_status = None;
            return;
        }
        let failure = match error {
            Some(error) => Some(format!("Playback failed: {error}")),
            None if self.load_queued || pending.started.elapsed() < FIRST_CHUNK_DEADLINE => return,
            None => Some(format!(
                "Playback produced no audio within {}s",
                FIRST_CHUNK_DEADLINE.as_secs()
            )),
        };
        self.pending_start = None;
        if !pending.resume {
            self.pending_seek_frame = Some(pending.frame);
        }
        self.set_playback_state(PlaybackState::Paused);
        if let Some(rt) = self.render_thread.as_ref() {
            rt.send(RenderCommand::Pause);
        }
        self.transport_status = failure;
    }

    /// Play from current position using streaming render (instant start).
    ///
    /// If the project was pre-loaded via `preload_project()` (called on edit),
    /// playback starts instantly (ring buffer already has chunks).
    /// Otherwise falls back to loading on-demand (still fast, just not zero-latency).
    /// A project that cannot stream returns the legacy render for the caller's worker.
    pub fn play(
        &mut self,
        project: &Project,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
    ) -> Result<PlayDispatch, String> {
        #[cfg(test)]
        let phase = std::time::Instant::now();
        // The press is what the first block is measured from: preparation work
        // after this point belongs to the latency it reports. A staged path
        // (a track preview, a take) keeps its tag but still gets this instant.
        let path = self
            .first_block_start
            .take()
            .map_or(StartPath::Play, |(_, path)| path);
        self.first_block_start = Some((now_ns(), path));
        let prepared =
            oc_engine::block_source::prepare_project(project, self.resource_root.as_deref())?;
        #[cfg(test)]
        crate::profile_phase("press_prepare", phase);
        self.play_prepared(prepared, plugin_instances)
    }

    /// Start playback for a project already prepared outside the app lock.
    fn play_prepared(
        &mut self,
        mut prepared: Project,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
    ) -> Result<PlayDispatch, String> {
        self.sync_sample_rate();
        self.track_preview = None;
        self.pending_live_seek = None;
        self.stop_audition();
        // A preview end bound covers this dispatch only.
        let play_end_frame = self.pending_play_end_frame.take();
        if self.render_dirty || self.render_thread.is_none() {
            for preset in &mut prepared.presets {
                if preset.source.as_deref() == Some("clap")
                    && let Some(instance) = plugin_instances.get(&preset.name)
                {
                    let bytes = instance.state_bytes()?;
                    let hex = bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    preset.params.retain(|(key, _)| key != "clap_state_hex");
                    preset.params.insert(
                        0,
                        (
                            "clap_state_hex".into(),
                            oc_dsl::ast::ParamValue::String(hex),
                        ),
                    );
                }
            }
        }
        let project = &prepared;
        if !supports_resolved_block_sources(project) {
            let sources = project
                .tracks
                .iter()
                .take(8)
                .map(|track| {
                    let preset = project
                        .presets
                        .iter()
                        .find(|p| Some(&p.name) == track.preset.as_ref());
                    format!(
                        "{}: {}{}",
                        track.name,
                        preset
                            .and_then(|p| p.plugin.as_deref().or(p.source.as_deref()))
                            .unwrap_or("default SoundFont"),
                        if track.performance.is_some() {
                            " performance"
                        } else {
                            ""
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Streaming playback does not support this source or performance configuration ({sources}); use offline export for rendering"
            ));
        }

        let soundfont_path = self.soundfont_path.clone().unwrap_or_default();

        // Ensure render thread is spawned
        if self.render_thread.is_none() {
            self.render_thread = Some(RenderThreadHandle::spawn(self.sample_rate));
            // The seam is in beats: derive it on this clock at the current rate,
            // which a device change may have replaced since it was last pushed.
            self.retime_loop_on(&prepared.properties);
            self.apply_mono_check();
            self.replay_spectrum_request();
            self.replay_click();
        }

        // A load already in flight is left alone; Play is queued behind it.
        if let Some(error) = self.render_thread.as_ref().unwrap().load_error() {
            log::warn!("[audio] Previous project load failed: {error}");
            self.render_dirty = true;
            self.load_queued = false;
        }
        let loaded = self
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .project_loaded
            .load(Ordering::Acquire);
        if loaded {
            self.load_queued = false;
        }
        let played_out = self
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .has_played_out();
        let load_in_flight = self.load_queued && !loaded;
        let did_render = !load_in_flight && self.render_dirty;
        if did_render {
            self.render_thread
                .as_ref()
                .unwrap()
                .send(RenderCommand::ProjectChanged {
                    root: self.resource_root.clone(),
                    project: Box::new(self.project_for_playback(project)),
                    soundfont_path,
                    warm: false,
                });
            self.render_dirty = false;
            self.load_queued = true;
        }

        let before_applied = self
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .play_applied
            .load(Ordering::Acquire);
        // An explicit seek wins. A naturally drained arrangement starts over.
        let (start_frame, resume) = match self.pending_seek_frame.take() {
            Some(frame) => (frame, false),
            None if played_out => (0, false),
            None => (
                self.render_thread
                    .as_ref()
                    .unwrap()
                    .playback
                    .play_cursor_frames
                    .load(Ordering::Acquire),
                true,
            ),
        };
        // What this press costs: a warm resume, a plain play from a prefilled
        // ring, or a play that has to rebuild the project graph first.
        let (started_ns, mut path) = self
            .first_block_start
            .take()
            .unwrap_or((now_ns(), StartPath::Play));
        if path == StartPath::Play {
            path = if did_render || load_in_flight {
                StartPath::Rebuild
            } else if resume && self.playback_state == PlaybackState::Paused {
                StartPath::Resume
            } else {
                StartPath::Play
            };
        }
        self.first_block_start = Some((started_ns, path));
        // The worker/callback own the consumed cursor. PendingStart below
        // reports the requested position without relabelling queued old audio.
        // A queued count-in must reach the render thread before this Play.
        self.replay_count_in();
        // A chain edit made while stopped must be in the graph before this
        // press renders its first block.
        self.pump_chain_change();
        self.render_thread
            .as_ref()
            .unwrap()
            .playback
            .first_block
            .begin_at(started_ns, path);
        self.render_thread
            .as_ref()
            .unwrap()
            .send(RenderCommand::Play {
                start_frame,
                resume,
                end_frame: play_end_frame,
            });
        #[cfg(test)]
        let phase = std::time::Instant::now();

        // Start the consumer when the warm-up could not open the device. A
        // running consumer keeps its warm ring: restarting it would drop audio.
        if !self.streaming_active {
            if self.idle_monitor.is_none() {
                self.drop_stream();
            }
            let playback = Arc::clone(&self.render_thread.as_ref().unwrap().playback);
            if let Err(error) = self.start_streaming_output(Arc::clone(&playback)) {
                // Play was queued before the device opened. Park the worker
                // and keep the requested position available for a retry.
                self.render_thread
                    .as_ref()
                    .unwrap()
                    .send(RenderCommand::Pause);
                playback.first_block.cancel();
                self.first_block_start = None;
                self.pending_seek_frame = Some(start_frame);
                return Err(error);
            }
            self.streaming_active = true;
        }
        #[cfg(test)]
        crate::profile_phase("press_stream_start", phase);

        // Do not wait for the first chunk: the callback outputs silence until it
        // lands, and transport_snapshot reports progress or a bounded failure.
        self.set_playback_state(PlaybackState::Playing);
        self.pending_start = Some(PendingStart {
            started: std::time::Instant::now(),
            frame: start_frame,
            resume,
            before_applied,
        });
        self.transport_status = None;
        log::info!(
            "[audio] Play dispatched (load={})",
            if did_render { "queued" } else { "warm" }
        );
        Ok(PlayDispatch::Started { did_render })
    }

    /// Tag the next press as a recording start, before the take opens its
    /// input stream, so the measured latency covers the whole press path.
    pub(crate) fn record_start_at(&mut self, path: StartPath) {
        self.first_block_start = Some((now_ns(), path));
    }

    /// Push a mixer-only edit (gain/pan/mute/solo) into the live stream.
    ///
    /// Instruments and DSP state stay untouched, so an A/B mute toggle never
    /// pays for a project reload.
    pub fn apply_mixer_change(&mut self, project: &Project) {
        self.track_preview = None;
        if self.render_dirty && self.render_thread.is_some() {
            // An outstanding full load also carries this mixer edit.
            self.preload_project(project);
            return;
        }
        let Ok(prepared) =
            oc_engine::block_source::prepare_project(project, self.resource_root.as_deref())
        else {
            self.mark_dirty();
            return;
        };
        self.invalidate_render_cache();
        let Some(rt) = self.render_thread.as_ref() else {
            return; // the first load will carry this mixer state
        };
        // A chain document still waiting predates this mixer edit; the mixer
        // document carries it, so the patch that follows installs the newest.
        if self.pending_chain.is_some() {
            self.pending_chain = Some(Box::new(prepared.clone()));
        }
        rt.send(RenderCommand::MixerChanged {
            project: Box::new(prepared),
            soundfont_path: self.soundfont_path.clone().unwrap_or_default(),
            requested_ns: now_ns(),
        });
    }

    /// Push an FX-chain edit (effect parameters, bypass, an effect added, moved
    /// or removed, or lane points) into the live stream.
    ///
    /// Only the affected chains are patched: instruments, sounding voices, the
    /// ring and the playhead stay as they were. A drag replaces the pending
    /// document instead of queueing one patch per input event.
    pub fn apply_chain_change(&mut self, project: &Project) {
        self.track_preview = None;
        self.invalidate_render_cache();
        if self.render_thread.is_none() {
            // Nothing is loaded to patch, so the next load must carry it.
            self.mark_dirty();
            return;
        }
        if self.render_dirty {
            self.preload_project(project);
            return;
        }
        let Ok(prepared) =
            oc_engine::block_source::prepare_project(project, self.resource_root.as_deref())
        else {
            self.mark_dirty();
            return;
        };
        self.pending_chain = Some(Box::new(prepared));
        self.pump_chain_change();
    }

    /// Hand the newest chain document to the render thread unless the previous
    /// one is still unapplied.
    ///
    /// The render thread applies chain documents in order, so the newest one
    /// supersedes any older document still waiting here. This keeps a drag to a
    /// bounded number of patches: at most one in flight plus this one.
    fn pump_chain_change(&mut self) {
        let Some(document) = self.pending_chain.take() else {
            return;
        };
        let Some(rt) = self.render_thread.as_ref() else {
            self.pending_chain = Some(document);
            return;
        };
        let playback = &rt.playback;
        if playback.chain_applied.load(Ordering::Acquire)
            < playback.chain_requests.load(Ordering::Acquire)
        {
            self.pending_chain = Some(document);
            return;
        }
        playback.chain_requests.fetch_add(1, Ordering::AcqRel);
        rt.send(RenderCommand::ChainChanged {
            project: document,
            soundfont_path: self.soundfont_path.clone().unwrap_or_default(),
        });
        // A load already in flight carries the same document plus this edit, so
        // the patch is covered. A merely loaded graph does not: an unrelated
        // structural edit may still be waiting for its reload.
        if self.load_queued {
            self.render_dirty = false;
        }
    }

    /// Render the authoritative playback buffer without opening an audio device.
    /// Only SoundFont tracks consume the optional font path; their loader still
    /// fails closed if it is empty, while native/plugin instruments do not use it.
    #[cfg(test)]
    #[cfg(test)]
    fn render_for_playback(
        &mut self,
        project: &Project,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
    ) -> Result<Vec<f32>, String> {
        let soundfont_path = self.soundfont_path.clone().unwrap_or_default();
        let mut cache = self.render_cache.take().unwrap_or_default();
        let rendered = Self::render_into_cache(
            project,
            &soundfont_path,
            self.sample_rate,
            &mut cache,
            plugin_instances,
        );
        self.render_cache = Some(cache);
        rendered
    }

    #[cfg(test)]
    /// Fill the per-track cache off any caller thread; the cache stays owned by
    /// the caller so a render can run outside the app lock.
    pub(crate) fn render_into_cache(
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        cache: &mut RenderCache,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
    ) -> Result<Vec<f32>, String> {
        let minimum_seconds = crate::project_extent::section_seconds(project)?;
        cache.set_minimum_duration(minimum_seconds)?;
        cache.set_worker_options(crate::gui_settings::load().worker_options());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.render_incremental(project, soundfont_path, sample_rate, plugin_instances)
        }));
        result
            .map_err(|error| {
                let detail = error
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| error.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown cause");
                format!("Render panicked: {detail}")
            })?
            .map_err(|error| format!("Render failed: {error}"))
    }

    #[allow(dead_code)]
    pub fn render_project_to_wav(
        &self,
        project: &Project,
        output: &std::path::Path,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
        resource_root: Option<&std::path::Path>,
    ) -> Result<(), String> {
        self.render_project_to_wav_with_pre_roll(
            project,
            output,
            plugin_instances,
            resource_root,
            0.0,
        )
    }

    pub fn render_project_to_wav_with_pre_roll(
        &self,
        project: &Project,
        output: &std::path::Path,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
        resource_root: Option<&std::path::Path>,
        pre_roll_seconds: f64,
    ) -> Result<(), String> {
        let soundfont_path = self.soundfont_path.clone().unwrap_or_default();
        let spec = oc_engine::encode::EncodeSpec::for_project(project, self.sample_rate);
        let (audio, _) = oc_engine::render::render_full_for_export_with_options(
            project,
            &soundfont_path,
            plugin_instances,
            oc_engine::render::RenderOptions {
                resource_root,
                minimum_duration_seconds: crate::project_extent::section_seconds(project)?,
                pre_roll_seconds,
                ..oc_engine::render::RenderOptions::new(
                    spec.sample_rate,
                    crate::gui_settings::load().worker_options(),
                )
            },
        )
        .map_err(|error| error.to_string())?;
        oc_engine::encode::encode_to_file(output, &spec, &audio).map(|_| ())
    }

    pub(crate) fn retire_owned_effect_touches(&self) {
        if let Some(handle) = &self.render_thread {
            handle.touch().release_all_owned();
        }
    }

    /// Pause — keep position, stop audio output
    pub fn pause(&mut self) {
        self.retire_owned_effect_touches();
        self.set_playback_state(PlaybackState::Paused);
        let seek = self.pending_live_seek.take();
        let start = self.pending_start.take();
        if let Some(rt) = &self.render_thread {
            let pb = &rt.playback;
            if let Some((frame, requested)) = seek
                && pb.position_applied.load(Ordering::Acquire) != requested
            {
                self.pending_seek_frame = Some(frame);
            } else if let Some(pending) = start
                && !pending.resume
                && pb.play_applied.load(Ordering::Acquire) == pending.before_applied
            {
                self.pending_seek_frame = Some(pending.frame);
            }
        }
        self.transport_status = None;
        if self.streaming_active
            && let Some(ref rt) = self.render_thread
        {
            rt.send(RenderCommand::Pause);
        }
        if let Some(ref pb) = self.playback {
            pb.playing.store(false, Ordering::Release);
        }
    }

    pub(crate) fn refuse_during_take(&self, action: &str) -> Result<(), String> {
        if self.recorder.recording() || self.midi_recorder.recording() {
            return Err(format!("Stop recording before {action}."));
        }
        Ok(())
    }

    pub(crate) fn audition_blocks_recording(&self) -> bool {
        self.track_preview.is_some() || self.audition_paused || self.audition_active()
    }

    /// Replace the current preview and publish its intent before it is prepared.
    pub(crate) fn replace_private_audition(&mut self, start_paused: bool) -> Result<(), String> {
        self.refuse_during_take("starting an audition")?;
        self.stop_audition();
        self.audition_pause_intent
            .store(start_paused, Ordering::Release);
        self.audition_paused = start_paused;
        Ok(())
    }

    /// Silence a preset audition; the transport cursor and state stay untouched.
    pub fn stop_audition(&mut self) {
        self.audition_output_error = None;
        self.audition_paused = false;
        self.audition_end_frame = None;
        self.audition_start_frame = 0;
        self.audition_sample_rate = 0;
        self.audition_slot.close();
        self.audition_playback = None;
        self.audition_render = None;
        self.audition_pair = None;
        #[cfg(test)]
        {
            self.audition_simulated_output = None;
            self.audition_render_simulated = None;
        }
    }

    /// Whether either audition path still has audio to play.
    pub fn audition_active(&self) -> bool {
        if self.audition_paused {
            return false;
        }
        if let Some(playback) = &self.audition_playback {
            let frames = playback.cursor.load(Ordering::Acquire) / 2;
            return playback.playing.load(Ordering::Acquire)
                && frames < playback.active_buffer().len() as u64 / 2;
        }
        if let Some(render) = &self.audition_render {
            return render.playback.active.load(Ordering::Acquire);
        }
        false
    }

    fn private_audition_finished(&self) -> bool {
        if let Some(first) = &self.audition_render {
            let finished = |render: &RenderThreadHandle| {
                let playback = &render.playback;
                playback.project_loaded.load(Ordering::Acquire)
                    && playback.position_requested.load(Ordering::Acquire)
                        == playback.position_applied.load(Ordering::Acquire)
                    && if self.audition_end_frame.is_some() {
                        let end = playback.run_output_end_frame();
                        end > 0 && playback.timeline_frames() >= end
                    } else {
                        playback.has_played_out()
                    }
            };
            return finished(first)
                && self
                    .audition_pair
                    .as_ref()
                    .is_none_or(|(second, _, _)| finished(second));
        }
        self.audition_playback.as_ref().is_some_and(|buffer| {
            buffer.cursor.load(Ordering::Acquire) >= buffer.active_buffer().len() as u64
        })
    }

    pub(crate) fn pause_private_audition(&mut self) {
        self.audition_paused = !self.private_audition_finished();
        if let Some((render, _, _)) = &self.audition_pair {
            render.send(RenderCommand::Pause);
        }
        if let Some(render) = &self.audition_render {
            render.send(RenderCommand::Pause);
        }
        if let Some(buffer) = &self.audition_playback {
            buffer.playing.store(false, Ordering::Release);
        }
    }

    pub(crate) fn resume_private_audition(&mut self) -> Result<(), String> {
        self.refuse_during_take("resuming an audition")?;
        self.reap_audition_failure();
        if let Some(error) = &self.audition_output_error {
            return Err(error.clone());
        }
        if self.private_audition_finished() {
            self.audition_paused = false;
            return Ok(());
        }
        if !self.audition_paused {
            return Ok(());
        }
        if let Some(render) = &self.audition_render {
            if let Some(error) = render.load_error() {
                return Err(error);
            }
            let frame = render
                .playback
                .play_cursor_frames
                .load(Ordering::Acquire)
                .max(self.audition_start_frame);
            if self.audition_end_frame.is_some_and(|end| frame >= end) {
                self.audition_paused = false;
                return Ok(());
            }
            render.send(RenderCommand::Play {
                start_frame: frame,
                resume: true,
                end_frame: self.audition_end_frame,
            });
        } else if let Some(buffer) = &self.audition_playback {
            if buffer.cursor.load(Ordering::Acquire) >= buffer.active_buffer().len() as u64 {
                self.audition_paused = false;
                return Ok(());
            }
            buffer.playing.store(true, Ordering::Release);
        }
        if let Some((render, _, _)) = &self.audition_pair {
            if let Some(error) = render.load_error() {
                return Err(error);
            }
            render.send(RenderCommand::Play {
                start_frame: render.playback.play_cursor_frames.load(Ordering::Acquire),
                resume: true,
                end_frame: self.audition_end_frame,
            });
        }
        // With a request still preparing, this is the intent applied on install.
        self.audition_paused = false;
        Ok(())
    }

    pub(crate) fn fill_audition_snapshot(&self, snapshot: &mut crate::audition::AuditionSnapshot) {
        if let Some(error) = &self.audition_output_error {
            snapshot.status = crate::audition::ERROR.into();
            snapshot.error = Some(error.clone());
            return;
        }
        snapshot.finished = self.private_audition_finished();
        snapshot.paused = self.audition_paused && !snapshot.finished;
        if let Some(render) = self.audition_render.as_ref() {
            let trace = Arc::clone(&render.playback.first_block);
            if let Some(line) = trace.take_report() {
                log::debug!("[first_block] {line}");
            }
            snapshot.first_block = Some(trace.snapshot());
        } else if let Some(playback) = self.audition_playback.as_ref() {
            snapshot.first_block = Some(playback.first_block.snapshot());
        }
        if let Some((render, _, _)) = &self.audition_pair
            && let Some(error) = render.load_error()
        {
            snapshot.status = crate::audition::ERROR.into();
            snapshot.error = Some(error);
        }
        if let Some(render) = &self.audition_render {
            snapshot.instances = render
                .playback
                .instrument_instantiations
                .load(Ordering::Relaxed) as usize;
            snapshot.loads = snapshot.instances;
            if let Some(error) = render.load_error() {
                snapshot.status = crate::audition::ERROR.into();
                snapshot.error = Some(error);
            } else if !self.audition_paused
                && render.playback.position_requested.load(Ordering::Acquire)
                    != render.playback.position_applied.load(Ordering::Acquire)
            {
                snapshot.status = crate::audition::LOADING.into();
            }
        }
        let frames = self
            .audition_render
            .as_ref()
            .map(|r| r.playback.play_cursor_frames.load(Ordering::Acquire))
            .or_else(|| {
                self.audition_playback
                    .as_ref()
                    .map(|p| p.cursor.load(Ordering::Acquire) / 2)
            })
            .unwrap_or(0);
        snapshot.position_seconds = frames.saturating_sub(self.audition_start_frame) as f64
            / if self.audition_sample_rate > 0 {
                self.audition_sample_rate
            } else {
                self.sample_rate.max(1)
            } as f64;
    }

    /// A failed audition stops owning output without moving the arrangement.
    pub(crate) fn reap_audition_failure(&mut self) {
        let failure = self
            .audition_render
            .as_ref()
            .and_then(|render| render.playback.take_stream_failure())
            .or_else(|| {
                self.audition_pair
                    .as_ref()
                    .and_then(|(render, _, _)| render.playback.take_stream_failure())
            })
            .or_else(|| {
                self.audition_playback.as_ref().and_then(|playback| {
                    playback
                        .stream_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                })
            });
        if let Some(error) = failure {
            self.stop_audition();
            self.audio_io.note_stream_failure("audition", &error);
            self.audition_output_error = Some(error);
        }
    }

    /// Move all source workers out so exit can retire them without holding the
    /// document mutex. Device routes are silenced before any worker is joined.
    pub(crate) fn take_render_owners_for_exit(&mut self) -> Vec<RenderThreadHandle> {
        self.pause();
        let mut workers = Vec::with_capacity(3);
        if let Some(worker) = self.render_thread.take() {
            workers.push(worker);
        }
        if let Some(worker) = self.audition_render.take() {
            workers.push(worker);
        }
        if let Some((worker, audible, _)) = self.audition_pair.take() {
            audible.store(false, Ordering::Release);
            workers.push(worker);
        }
        self.stop_audition();
        self.end_live_synth();
        workers
    }

    /// Stop — pause and reset cursor to beginning
    pub fn stop(&mut self) {
        self.retire_owned_effect_touches();
        self.track_preview = None;
        self.stop_audition();
        // Commands are asynchronous: retain the requested zero until next Play
        // so an immediate snapshot/restart cannot reuse the worker's old cursor.
        self.pending_seek_frame = Some(0);
        self.pending_live_seek = None;
        self.pending_play_end_frame = None;
        self.pending_start = None;
        self.transport_status = None;
        self.first_block_start = None;
        if let Some(render) = self.render_thread.as_ref() {
            render.playback.first_block.cancel();
        }
        if let Some(playback) = self.playback.as_ref() {
            playback.first_block.cancel();
        }
        self.end_live_synth();
        self.set_playback_state(PlaybackState::Stopped);
        if self.streaming_active
            && let Some(ref rt) = self.render_thread
        {
            rt.send(RenderCommand::Stop);
        }
        if let Some(ref pb) = self.playback {
            pb.playing.store(false, Ordering::Release);
            pb.cursor.store(0, Ordering::Release);
        }
    }

    /// Opening/creating a project must not reuse the previous project's audio.
    /// Ordinary transport stop deliberately retains caches for fast replay.
    pub fn reset_project(&mut self) {
        self.stop();
        if self.idle_monitor.is_none() {
            self.drop_stream();
            self.sync_idle_monitor();
        }
        self.render_thread = None;
        self.playback = None;
        self.render_cache = None;
        self.render_dirty = true;
        self.load_queued = false;
    }

    /// Mute audio output (for cursor scrub). Render thread keeps running.
    pub fn mute(&mut self) {
        if let Some(ref rt) = self.render_thread {
            rt.playback.muted.store(true, Ordering::Release);
        }
    }

    /// Unmute audio output (resume after cursor scrub).
    pub fn unmute(&mut self) {
        if let Some(ref rt) = self.render_thread {
            rt.playback.muted.store(false, Ordering::Release);
        }
    }

    /// Seek to a beat position
    pub fn seek(&mut self, beat: f64, clock: &oc_dsl::timing::ProjectClock) -> Result<(), String> {
        if !beat.is_finite() || beat < 0.0 {
            return Err("Seek position must be a non-negative finite beat".into());
        }
        let frame = clock.sample_at_quarter(beat, self.sample_rate)?;
        let sample = frame
            .checked_mul(2)
            .ok_or("Stereo seek position overflow")?;
        self.pending_seek_frame = (self.playback_state != PlaybackState::Playing).then_some(frame);

        if self.streaming_active && self.playback_state == PlaybackState::Playing {
            if let Some(pending) = self.pending_start.as_mut() {
                pending.frame = frame;
            }
            if let Some(ref rt) = self.render_thread {
                // Update the shared cursor immediately so transport_snapshot
                // reads the correct position without waiting for the render thread.
                rt.playback
                    .play_cursor_frames
                    .store(frame, Ordering::Release);
                rt.playback.first_block.begin_at(now_ns(), StartPath::Seek);
                rt.send(RenderCommand::Seek { frame });
                self.pending_live_seek = Some((
                    frame,
                    rt.playback.position_requested.load(Ordering::Acquire),
                ));
            }
        } else if self.streaming_active
            && !self.render_dirty
            && let Some(rt) = self.render_thread.as_ref()
            && rt.playback.project_loaded.load(Ordering::Acquire)
        {
            // Use the paused time to rebuild DSP history at the new cursor.
            // A drained run can still own the worker until explicitly parked.
            rt.send(RenderCommand::Pause);
            rt.send(RenderCommand::Seek { frame });
        }
        // Legacy seek (stereo sample offset)
        if let Some(ref pb) = self.playback {
            let max = pb.buffer.len() as u64;
            pb.cursor.store(sample.min(max), Ordering::Release);
        }
        Ok(())
    }

    pub fn loop_state(&self) -> LoopState {
        LoopState {
            enabled: self.loop_enabled,
            start_beats: self.loop_region.map(|region| region.0),
            end_beats: self.loop_region.map(|region| region.1),
            clamped: self.loop_clamped(),
        }
    }

    /// The active loop region in frames, when Loop is armed.
    ///
    /// The end stops at the project end: the renderer has no audio past it, so a
    /// take's pass ends where the transport does.
    pub(crate) fn active_loop_frames(&self) -> Option<(u64, u64)> {
        if !self.loop_enabled {
            return None;
        }
        let (start, end) = self.loop_frames?;
        let project_end = self.project_end_frame();
        if project_end == 0 {
            return Some((start, end));
        }
        Some((start, end.min(project_end.max(start))))
    }

    /// Whether the project end cuts the region short.
    ///
    /// Read from the current playback bound, so the report follows a project
    /// that grew or shrank without the region changing.
    fn loop_clamped(&self) -> bool {
        let Some((_, end)) = self.loop_frames else {
            return false;
        };
        let project_end = self.project_end_frame();
        project_end > 0 && end > project_end
    }

    /// The playback bound of the loaded project in frames; `0` until a project
    /// reaches the render thread.
    fn project_end_frame(&self) -> u64 {
        self.render_thread
            .as_ref()
            .map_or(0, |rt| rt.playback.total_frames.load(Ordering::Acquire))
    }

    /// The authored region in frames on `clock`, or `None` when none is set.
    ///
    /// The beats are the state; this is the only place they become frames, so a
    /// tempo, meter or device-rate change re-derives the same beats on the new
    /// clock instead of keeping a stale audio position. The project end is not
    /// applied here: playback bounds its own wrap with the frame count it holds.
    fn region_frames(
        &self,
        clock: &oc_dsl::timing::ProjectClock,
    ) -> Result<Option<(u64, u64)>, String> {
        let Some((start, end)) = self.loop_region else {
            return Ok(None);
        };
        Ok(Some((
            clock.sample_at_quarter(start, self.sample_rate)?,
            clock.sample_at_quarter(end, self.sample_rate)?,
        )))
    }

    /// Re-derive the frame seam from the authored beats on `clock`.
    ///
    /// Every path that changes the clock or the device rate calls this, so the
    /// seam the transport wraps at describes the current tempo map. A pair that
    /// changed is pushed to the render thread, which drops the pass already
    /// queued for the old seam and rebuilds from the audible position: a live
    /// change is heard from where the audio is, never replayed.
    pub(crate) fn retime_loop(&mut self, clock: &oc_dsl::timing::ProjectClock) -> LoopState {
        match self.region_frames(clock) {
            Ok(frames) => self.loop_frames = frames,
            Err(error) => {
                // A clock that cannot place this region leaves the seam where it
                // is; the caller's edit still publishes.
                log::warn!("loop region not re-timed: {error}");
                return self.loop_state();
            }
        }
        self.publish_loop_region();
        self.loop_state()
    }

    /// Re-derive the seam on the clock `properties` describes.
    fn retime_loop_on(&mut self, properties: &oc_dsl::ast::ProjectProperties) {
        match oc_dsl::timing::ProjectClock::new(properties) {
            Ok(clock) => {
                self.retime_loop(&clock);
            }
            Err(error) => log::warn!("loop region not re-timed: {error}"),
        }
    }

    /// Hand the cached seam to the render thread, when it differs from the one
    /// the playback object holds.
    ///
    /// An unchanged pair is not re-pushed: that would clear a published seam and
    /// drop the rolling pass on every unrelated edit.
    fn publish_loop_region(&self) {
        let Some(rt) = self.render_thread.as_ref() else {
            return;
        };
        let region = self
            .loop_frames
            .map(|(start, end)| crate::render_thread::LoopRegion { start, end });
        let armed = self.loop_enabled && region.is_some();
        if rt.playback.loop_region() == region && rt.playback.loop_enabled() == armed {
            return;
        }
        rt.playback.set_loop_region(region);
        rt.playback.set_loop_enabled(armed);
    }

    /// Set or clear the session loop region, in beats.
    ///
    /// This is transport state, not project data: it never reaches the DSL, the
    /// offline renderer or an export. Clearing the region also disarms Loop, so
    /// the toggle cannot claim a wrap that has nowhere to go.
    pub fn set_loop_region(
        &mut self,
        start: Option<f64>,
        end: Option<f64>,
        enabled: bool,
        clock: &oc_dsl::timing::ProjectClock,
    ) -> Result<LoopState, String> {
        match (start, end) {
            (None, None) => {
                self.loop_region = None;
                self.loop_frames = None;
                self.loop_enabled = false;
                self.publish_loop_region();
                Ok(self.loop_state())
            }
            (Some(_), None) | (None, Some(_)) => Err(
                "A loop region needs both a start and an end; drag in the ruler to set one.".into(),
            ),
            (Some(start), Some(end)) => {
                if !start.is_finite() || !end.is_finite() || start < 0.0 || end < 0.0 {
                    return Err(
                        "Loop region must be two non-negative finite beat positions.".into(),
                    );
                }
                let start_bar = clock.bar_beat_at_quarter(start)?.bar;
                let meter = clock.meter_at_bar(start_bar)?;
                // A beat is the meter's denominator note, not always a quarter.
                let beat_quarters = 4.0 / f64::from(meter.denominator);
                if end - start < beat_quarters - 1e-9 {
                    return Err(
                        "Loop region is shorter than one beat; drag a longer region or clear it."
                            .into(),
                    );
                }
                let start_frame = clock.sample_at_quarter(start, self.sample_rate)?;
                let end_frame = clock.sample_at_quarter(end, self.sample_rate)?;
                if end_frame <= start_frame {
                    return Err(
                        "Loop region is shorter than one beat; drag a longer region or clear it."
                            .into(),
                    );
                }
                let project_end = self.project_end_frame();
                if project_end > 0 && start_frame >= project_end {
                    return Err(
                        "Loop region starts past the project end; drag it over a bar that has audio."
                            .into(),
                    );
                }
                self.loop_region = Some((start, end));
                self.loop_enabled = enabled;
                Ok(self.retime_loop(clock))
            }
        }
    }

    /// The record fields a transport snapshot carries. The command layer
    /// refines them with `record::state_of`, which also knows the armed track.
    fn record_state(&self, count_in_remaining_frames: u64) -> crate::record::RecordState {
        crate::record::RecordState {
            armed: self.recorder.armed.clone(),
            recording: self.recorder.recording() || self.midi_recorder.recording(),
            seconds: if self.midi_recorder.recording() {
                self.midi_recorder.seconds()
            } else {
                self.recorder.seconds()
            },
            report: self
                .midi_recorder
                .report
                .clone()
                .or_else(|| self.recorder.report.clone()),
            disabled_reason: None,
            count_in_remaining_frames,
            mode: crate::record::RecordMode::default(),
            midi: crate::midi_record::MidiRecordView::default(),
            audio_device: None,
        }
    }

    pub fn punch_state(&self) -> PunchState {
        PunchState {
            enabled: self.punch_enabled,
            start_beats: self.punch_region.map(|region| region.0),
            end_beats: self.punch_region.map(|region| region.1),
        }
    }

    /// Set or clear the punch range, in beats.
    ///
    /// Session state, like the loop region: it never reaches the DSL. A take
    /// keeps only the notes whose start falls inside an enabled range.
    pub fn set_punch_region(
        &mut self,
        start: Option<f64>,
        end: Option<f64>,
        enabled: bool,
    ) -> Result<PunchState, String> {
        match (start, end) {
            (None, None) => {
                self.punch_region = None;
                self.punch_enabled = false;
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "A punch range needs both an in and an out position; set both or clear it."
                        .into(),
                );
            }
            (Some(start), Some(end)) => {
                if !start.is_finite() || !end.is_finite() || start < 0.0 || end < 0.0 {
                    return Err("Punch positions must be two non-negative finite beats.".into());
                }
                if end - start < crate::midi_record::SHORTEST_NOTE_QUARTERS - 1e-9 {
                    return Err(
                        "The punch out must be at least a 64th note after the punch in.".into(),
                    );
                }
                self.punch_region = Some((start, end));
                self.punch_enabled = enabled;
            }
        }
        Ok(self.punch_state())
    }

    /// Mark the streaming state and cached renders stale after a structural edit.
    pub fn mark_dirty(&mut self) {
        self.track_preview = None;
        self.render_dirty = true;
        // Any load already queued predates this edit, so it cannot satisfy it.
        self.load_queued = false;
        // A full load carries every chain, so a waiting patch is redundant.
        self.pending_chain = None;
        self.invalidate_render_cache();
    }

    fn invalidate_render_cache(&mut self) {
        if let Some(ref mut cache) = self.render_cache {
            cache.mark_all_dirty();
        }
    }

    /// The project as the live render thread should hear it.
    ///
    /// Take solo is an audition overlay: the soloed pass is unmuted and the
    /// other passes of its group are muted, but the document keeps every flag
    /// the user wrote, so an export never hears the audition.
    fn project_for_playback(&self, project: &Project) -> Project {
        let Some((track, take)) = self.take_solo.as_ref() else {
            return project.clone();
        };
        take_solo_project(project, track, *take)
    }

    /// Eagerly build the project in the background and pre-render the opening
    /// lookahead so the next play only has to start the consumer.
    /// Call this after loading a project or a structural edit.
    pub fn preload_project(&mut self, project: &Project) {
        self.track_preview = None;
        self.sync_sample_rate();
        // Every load and every structural edit passes through here, so this is
        // where a tempo, meter or rate change re-derives the loop seam. It runs
        // before the early returns below: those skip rebuilding audio, not the
        // clock the transport wraps on.
        self.retime_loop_on(&project.properties);
        let playback = self.project_for_playback(project);
        // The predicates below resolve the presets themselves, so building the
        // block sources here would only be thrown away. Building them used to
        // cost most of an open; the render thread builds what it needs when it
        // receives the project.
        let project = &playback;
        // CLAP editor state is captured from the live instance by play(); a worker
        // snapshot would race it. VST3 state lives in the preset params.
        if project
            .presets
            .iter()
            .any(|p| p.source.as_deref() == Some("clap"))
        {
            return;
        }
        // Plugin/native projects use the authoritative cached renderer in play().
        // Do not create an unrelated SoundFont streaming job on every edit.
        if project_needs_plugin_aware_render(project) && !supports_block_sources(project) {
            return;
        }
        let soundfont_path = self.soundfont_path.clone().unwrap_or_default();

        // Ensure render thread is spawned
        if self.render_thread.is_none() {
            self.render_thread = Some(RenderThreadHandle::spawn(self.sample_rate));
            self.retime_loop_on(&project.properties);
            self.apply_mono_check();
            self.replay_spectrum_request();
            self.replay_click();
        }

        // Open the device before the build so the first press only enables the
        // consumer. A silent stream costs one callback; a press-time device open
        // would land in the latency the user hears.
        #[cfg(test)]
        let phase = std::time::Instant::now();
        let mut output_started = self.streaming_active;
        if !self.streaming_active && self.live_synth.is_none() {
            let playback = Arc::clone(&self.render_thread.as_ref().unwrap().playback);
            match self.start_streaming_output(playback) {
                Ok(()) => {
                    self.streaming_active = true;
                    output_started = true;
                }
                Err(error) => {
                    log::warn!("[audio] Warm-up could not start the audio device: {error}");
                }
            }
        }
        #[cfg(test)]
        crate::profile_phase("preload_stream_start", phase);

        // Pre-render only from the start; a nonzero cursor needs an exact-history
        // seek, which the worker performs on play. No consumer means no ring.
        let warm = output_started
            && self
                .render_thread
                .as_ref()
                .is_none_or(|rt| rt.playback.play_cursor_frames.load(Ordering::Acquire) == 0);

        // Send project to render thread (it will load in background)
        #[cfg(test)]
        let phase = std::time::Instant::now();
        self.render_thread
            .as_ref()
            .unwrap()
            .send(RenderCommand::ProjectChanged {
                // The load may be the first one after opening a file, so the
                // directory travels with it instead of coming from the panel.
                root: self.resource_root.clone(),
                project: Box::new(playback),
                soundfont_path,
                warm,
            });
        #[cfg(test)]
        crate::profile_phase("preload_dispatch", phase);
        self.render_dirty = false;
        self.load_queued = true;
    }

    /// Stream one kit pad privately for at most four seconds.
    pub(crate) fn open_drum_pad_preview(
        io: &Arc<AudioIo>,
        spec: &StreamSpec,
        simulated: bool,
        started_ns: u64,
        request: DrumPadPreview<'_>,
    ) -> Result<PreparedAuditionStream, String> {
        let DrumPadPreview {
            params,
            root,
            note,
            velocity,
        } = request;
        let kit = oc_synth::drumkit::kit_from_params(params, root)?;
        if !kit.pads.iter().any(|pad| pad.note == note) {
            return Err(format!("kit has no pad for MIDI note {note}"));
        }
        let mut project = oc_dsl::parse("@project\n@preset pad\n  source: builtin\n  plugin: \"builtin.drumkit\"\n@track audition | preset: pad, instrument: drums/drumkit\n  @phrase hit [1:1 - 2:1]\n    C2 16th\n").map_err(|e| e.to_string())?;
        project.presets[0].params = params.to_vec();
        let event = &mut project.tracks[0].phrases[0].events[0];
        event.pitch = oc_dsl::ast::Pitch::from_midi(note)
            .ok_or_else(|| format!("MIDI note {note} is outside 0..127"))?;
        event.velocity = Some(velocity);
        let project = oc_engine::block_source::prepare_project(&project, root)?;
        Self::open_audition_render_window(
            io,
            spec,
            PathBuf::new(),
            project,
            simulated,
            started_ns,
            StartPath::DrumPad,
            0,
            Some(u64::from(spec.sample_rate) * 4),
        )
    }

    /// Render the GM program audition without touching playback state.
    #[cfg(test)]
    pub fn render_program_audition(
        soundfont_path: &Path,
        sample_rate: u32,
        program: u8,
        channel: u8,
    ) -> Result<Vec<f32>, String> {
        let renderer =
            oc_engine::soundfont::SharedSoundFontRenderer::new(sample_rate, soundfont_path)?;
        let drums = channel == 9;
        if drums {
            renderer.enable_drum_channel();
        }
        renderer.send(&oc_engine::interpret::MidiMessage::ProgramChange { channel, program })?;
        renderer.send(&oc_engine::interpret::MidiMessage::ControlChange {
            channel,
            controller: 7,
            value: 100,
        })?;
        let mut voice = ProgramVoice {
            renderer: &renderer,
            channel,
        };
        crate::audition_figure::render(&mut voice, sample_rate, drums)
    }

    /// Build an audition playback stream without touching the app state.
    pub fn open_audition_playback(
        io: &AudioIo,
        spec: &StreamSpec,
        buffer: Vec<f32>,
        started_ns: u64,
        path: StartPath,
    ) -> Result<AuditionPlaybackStream, String> {
        let rendered_frames = buffer.len() as u64 / 2;
        let playback = Arc::new(AudioPlayback {
            buffer,
            alt: None,
            cursor: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            side: AtomicBool::new(false),
            first_block: crate::first_block::FirstBlock::new(),
            stream_error: Mutex::new(None),
        });
        // Stamped before the stream opens, so the sink can never deliver first.
        playback.first_block.begin_at(started_ns, path);
        playback.first_block.add_rendered(rendered_frames);
        let source =
            Arc::new(CallbackRoute::default()).wrap(Arc::new(BufferSource(Arc::clone(&playback))));
        let sink = io.open_stream_with(spec, Arc::clone(&source))?;
        if sink.sample_rate != spec.sample_rate {
            return Err("The output device opened at a different preview sample rate".into());
        }
        Ok((playback, sink, source))
    }

    /// Install a prepared audition stream; the caller holds the app lock.
    pub fn install_audition_playback(
        &mut self,
        playback: Arc<AudioPlayback>,
        stream: StreamSink,
        source: Arc<dyn RenderSource>,
        sample_rate: u32,
    ) -> Result<(), String> {
        self.refuse_during_take("starting an audition")?;
        let paused = self.audition_paused || self.audition_pause_intent.load(Ordering::Acquire);
        self.stop_audition();
        self.audition_slot.install(stream, source);
        self.audition_paused = paused;
        self.audition_sample_rate = sample_rate;
        playback.playing.store(!paused, Ordering::Release);
        self.audition_playback = Some(playback);
        Ok(())
    }

    pub(crate) fn has_audition_pair(&self) -> bool {
        self.audition_pair.is_some()
    }
    #[cfg(test)]
    pub(crate) fn paired_audition_cursors(&self) -> (u64, u64) {
        (
            self.audition_render
                .as_ref()
                .unwrap()
                .playback
                .play_cursor_frames
                .load(Ordering::Acquire),
            self.audition_pair
                .as_ref()
                .unwrap()
                .0
                .playback
                .play_cursor_frames
                .load(Ordering::Acquire),
        )
    }

    /// Select the aligned stream, replaying only a naturally completed pair.
    pub fn select_audition_side(&mut self, side: bool) -> bool {
        let Some((second, selected, _)) = &self.audition_pair else {
            return false;
        };
        selected.store(side, Ordering::Release);
        if let Some(first) = &self.audition_render {
            let ended = self.audition_end_frame.is_some_and(|end| {
                first.playback.play_cursor_frames.load(Ordering::Acquire) >= end
                    && second.playback.play_cursor_frames.load(Ordering::Acquire) >= end
            });
            if ended {
                self.audition_paused = false;
                self.audition_pause_intent.store(false, Ordering::Release);
                for render in [first, second] {
                    render.send(RenderCommand::Play {
                        start_frame: 0,
                        resume: false,
                        end_frame: self.audition_end_frame,
                    });
                }
            }
        }
        true
    }

    pub(crate) fn open_audition_pair_stream(
        io: &Arc<AudioIo>,
        spec: &StreamSpec,
        soundfont_path: PathBuf,
        a: Project,
        b: Project,
        simulated_output: bool,
    ) -> Result<PreparedAuditionStream, String> {
        let first = RenderThreadHandle::spawn(spec.sample_rate);
        let second = RenderThreadHandle::spawn(spec.sample_rate);
        let end_frame =
            Some((crate::ab_audition::MATCH_WINDOW_SECONDS * spec.sample_rate as f64) as u64);
        for (thread, project) in [(&first, a), (&second, b)] {
            thread.send(RenderCommand::ProjectChanged {
                // Both candidates arrive with their media already resolved.
                root: None,
                project: Box::new(project),
                soundfont_path: soundfont_path.clone(),
                warm: false,
            });
            thread.send(RenderCommand::Play {
                start_frame: 0,
                resume: false,
                end_frame,
            });
            thread.playback.active.store(true, Ordering::Release);
            thread
                .playback
                .output_suspended
                .store(true, Ordering::Release);
        }
        let selected = Arc::new(AtomicBool::new(true));
        let gains = Arc::new(std::array::from_fn(|_| {
            std::sync::atomic::AtomicU32::new(1.0_f32.to_bits())
        }));
        let source = Arc::new(CallbackRoute::default()).wrap(Arc::new(PairedStreamingSource {
            a: first.playback.clone(),
            b: second.playback.clone(),
            selected: selected.clone(),
            gains: gains.clone(),
            previous: std::array::from_fn(|_| std::sync::atomic::AtomicU32::new(1.0_f32.to_bits())),
            ramp_frames: (spec.sample_rate / 50).max(1) as f32,
        }));
        #[cfg(test)]
        if simulated_output {
            let simulated = Some(SimulatedOutput::start_sink(source.create(spec)));
            return Ok(PreparedAuditionStream {
                thread: first,
                pair: Some((second, selected, gains)),
                end_frame,
                start_frame: 0,
                sample_rate: spec.sample_rate,
                stream: None,
                source: None,
                simulated,
            });
        }
        let _ = simulated_output;
        let stream = io.open_stream_with(spec, source.clone())?;
        Ok(PreparedAuditionStream {
            thread: first,
            pair: Some((second, selected, gains)),
            end_frame,
            start_frame: 0,
            sample_rate: spec.sample_rate,
            stream: Some(stream),
            source: Some(source),
            #[cfg(test)]
            simulated: None,
        })
    }

    /// Publish a rendered audition buffer on the separate audition stream.
    ///
    /// This path renders the whole preview before it can sound; the trace
    /// reports that render as the audio computed ahead of the first block.
    pub fn play_audition_buffer(&mut self, buffer: Vec<f32>) -> Result<(), String> {
        self.refuse_during_take("starting an audition")?;
        let (started_ns, path) = self
            .first_block_start
            .take()
            .unwrap_or((now_ns(), StartPath::PresetAudition));
        #[cfg(test)]
        if self.simulate_output {
            let rendered_frames = buffer.len() as u64 / 2;
            let paused = self.audition_paused || self.audition_pause_intent.load(Ordering::Acquire);
            self.stop_audition();
            self.audition_paused = paused;
            self.audition_sample_rate = self.audio_io.spec().sample_rate;
            let playback = Arc::new(AudioPlayback {
                buffer,
                alt: None,
                cursor: AtomicU64::new(0),
                playing: AtomicBool::new(!paused),
                side: AtomicBool::new(false),
                first_block: crate::first_block::FirstBlock::new(),
                stream_error: Mutex::new(None),
            });
            playback.first_block.begin_at(started_ns, path);
            playback.first_block.add_rendered(rendered_frames);
            self.audition_playback = Some(Arc::clone(&playback));
            self.audition_simulated_output = Some(SimulatedAuditionOutput::start(playback));
            return Ok(());
        }
        let spec = self.audio_io.spec();
        let (playback, stream, source) =
            Self::open_audition_playback(&self.audio_io, &spec, buffer, started_ns, path)?;
        self.install_audition_playback(playback, stream, source, spec.sample_rate)
    }

    /// Audition blocks the simulated device has played for the current preview.
    #[cfg(test)]
    pub(crate) fn heard_audition_blocks(&self) -> u64 {
        self.audition_simulated_output
            .as_ref()
            .map_or(0, SimulatedAuditionOutput::heard_blocks)
            .max(
                self.audition_render_simulated
                    .as_ref()
                    .map_or(0, SimulatedOutput::heard_blocks),
            )
    }

    /// Loudest sample the simulated device has played from the current preview.
    #[cfg(test)]
    pub(crate) fn heard_audition_peak(&self) -> f32 {
        self.audition_render_simulated
            .as_ref()
            .map_or(0.0, SimulatedOutput::heard_peak)
    }

    /// Streaming blocks the simulated device has played since startup.
    #[cfg(test)]
    pub(crate) fn simulated_output_heard_blocks(&self) -> u64 {
        self.simulated_output
            .as_ref()
            .map_or(0, SimulatedOutput::heard_blocks)
    }

    /// Loudest PCM sample delivered by this simulated output instance.
    #[cfg(test)]
    pub(crate) fn simulated_output_heard_peak(&self) -> f32 {
        self.simulated_output
            .as_ref()
            .map_or(0.0, SimulatedOutput::heard_peak)
    }

    /// Start a builtin synth audition on its own render thread. The device is
    /// opened before the project is dispatched, so the first chunk is audible
    /// as soon as the worker has built it instead of after the whole phrase.
    pub(crate) fn open_audition_render(
        io: &Arc<AudioIo>,
        spec: &StreamSpec,
        soundfont_path: PathBuf,
        project: Project,
        simulated_output: bool,
        started_ns: u64,
        path: StartPath,
    ) -> Result<PreparedAuditionStream, String> {
        Self::open_audition_render_window(
            io,
            spec,
            soundfont_path,
            project,
            simulated_output,
            started_ns,
            path,
            0,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_audition_render_window(
        io: &Arc<AudioIo>,
        spec: &StreamSpec,
        soundfont_path: PathBuf,
        project: Project,
        simulated_output: bool,
        started_ns: u64,
        path: StartPath,
        start_frame: u64,
        end_frame: Option<u64>,
    ) -> Result<PreparedAuditionStream, String> {
        #[cfg(not(test))]
        let _ = simulated_output;
        let thread = RenderThreadHandle::spawn(spec.sample_rate);
        let playback = Arc::clone(&thread.playback);
        // The preview starts its own worker: the press is when this command ran,
        // and everything the worker builds is inside the measured latency.
        playback.first_block.begin_at(started_ns, path);
        thread.send(RenderCommand::ProjectChanged {
            // Every audition caller hands over a prepared project, so its media
            // paths are already absolute.
            root: None,
            project: Box::new(project),
            soundfont_path,
            warm: false,
        });
        thread.send(RenderCommand::Play {
            start_frame,
            resume: false,
            end_frame,
        });
        playback.active.store(true, Ordering::Release);
        // A prepared stream cannot sound before its generation is committed.
        playback.output_suspended.store(true, Ordering::Release);
        #[cfg(test)]
        if simulated_output {
            let simulated = Some(SimulatedOutput::start(Arc::clone(&playback)));
            return Ok(PreparedAuditionStream {
                thread,
                pair: None,
                end_frame,
                start_frame,
                sample_rate: spec.sample_rate,
                stream: None,
                source: None,
                simulated,
            });
        }
        let source = Arc::new(CallbackRoute::default())
            .wrap(Arc::new(StreamingSource(Arc::clone(&playback))));
        let stream = io.open_stream_with(spec, Arc::clone(&source))?;
        Ok(PreparedAuditionStream {
            thread,
            pair: None,
            end_frame,
            start_frame,
            sample_rate: spec.sample_rate,
            stream: Some(stream),
            source: Some(source),
            #[cfg(test)]
            simulated: None,
        })
    }

    /// Install a prepared audition render; the caller holds the app lock.
    pub(crate) fn install_audition_render(
        &mut self,
        prepared: PreparedAuditionStream,
    ) -> Result<(), String> {
        self.refuse_during_take("starting an audition")?;
        let PreparedAuditionStream {
            thread,
            pair,
            end_frame,
            start_frame,
            sample_rate,
            stream,
            source,
            #[cfg(test)]
            simulated,
        } = prepared;
        let paused = self.audition_paused || self.audition_pause_intent.load(Ordering::Acquire);
        self.stop_audition();
        if let (Some(stream), Some(source)) = (stream, source) {
            self.audition_slot.install(stream, source);
        }
        self.audition_render = Some(thread);
        self.audition_pair = pair;
        self.audition_end_frame = end_frame;
        self.audition_start_frame = start_frame;
        self.audition_sample_rate = sample_rate;
        if paused {
            self.pause_private_audition();
        } else if let Some(render) = &self.audition_render {
            render
                .playback
                .output_suspended
                .store(false, Ordering::Release);
        }
        if !paused && let Some((render, _, _)) = &self.audition_pair {
            render
                .playback
                .output_suspended
                .store(false, Ordering::Release);
        }
        #[cfg(test)]
        {
            self.audition_render_simulated = simulated;
        }
        Ok(())
    }

    /// Preview a solo mix through the authoritative graph. Raw cached track
    /// buffers are pre-insert and cannot stand in for the audible project mix.
    #[cfg(test)]
    pub fn preview_track(
        &mut self,
        project: &oc_dsl::ast::Project,
        track_index: usize,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
        resource_root: Option<&std::path::Path>,
    ) -> Result<(), String> {
        self.preview_track_with_intent(project, track_index, plugin_instances, resource_root, false)
    }

    pub(crate) fn preview_track_with_intent(
        &mut self,
        project: &oc_dsl::ast::Project,
        track_index: usize,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
        resource_root: Option<&std::path::Path>,
        start_paused: bool,
    ) -> Result<(), String> {
        self.refuse_during_take("previewing a track")?;
        #[cfg(test)]
        let phase = std::time::Instant::now();
        let started_ns = now_ns();
        let solo_project = solo_preview_project(project, track_index)?;
        if project_needs_plugin_aware_render(&solo_project)
            && !supports_block_sources(&solo_project)
        {
            return Err("Track preview requires a supported streaming source; this project uses an unsupported source or performance timeline".into());
        }
        // An audition is a fresh pass, not a resume at the previous track's
        // cursor (which may already be beyond this track's material).
        self.transport_pause_intent
            .store(start_paused, Ordering::Release);
        self.stop();
        self.sync_sample_rate();
        // Start at the first content and stop after the last one plus the
        // effect tail; a track with none falls back to the whole-project pass.
        let clock = oc_dsl::timing::ProjectClock::new(&solo_project.properties)?;
        if let Some((start, end)) =
            track_preview_range_quarters(&solo_project, track_index, &clock)?
        {
            self.seek(start, &clock)?;
            self.pending_play_end_frame = Some(preview_end_frame(&clock, end, self.sample_rate)?);
        } else {
            self.pending_play_end_frame = None;
        }
        #[cfg(test)]
        crate::profile_phase("preview_stop", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();
        self.resource_root = resource_root.map(std::path::Path::to_path_buf);
        self.mark_dirty();
        let end_frame = self.pending_play_end_frame;
        let start_frame = self.pending_seek_frame.unwrap_or(0);
        self.first_block_start = Some((started_ns, StartPath::TrackPreview));
        let result = self.play(&solo_project, plugin_instances).map(|_| ());
        #[cfg(test)]
        crate::profile_phase("preview_play_dispatch", phase);
        // The next main transport play must restore the original mix.
        self.mark_dirty();
        if result.is_ok() && self.streaming_active {
            self.track_preview = Some((track_index, end_frame));
            if start_paused {
                self.pause();
                // The worker may not have applied the first Play yet.
                self.pending_seek_frame = Some(start_frame);
            }
        }
        result.map_err(|error| format!("Track '{}': {error}", project.tracks[track_index].name))
    }

    fn track_preview_needs_rebuild(&self) -> bool {
        self.track_preview.is_some()
            && self.render_thread.is_none()
            && self.playback_state == PlaybackState::Paused
    }

    /// Reuse the paused solo graph, or rebuild it when the output rate changed.
    pub(crate) fn resume_track_preview_for_project(
        &mut self,
        track_index: usize,
        project: &Project,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
    ) -> Result<(), String> {
        self.refuse_during_take("resuming a track preview")?;
        self.sync_sample_rate();
        if !self.track_preview_needs_rebuild() {
            return self.resume_track_preview(track_index);
        }
        let (active_index, end_frame) = self.track_preview.unwrap();
        if active_index != track_index {
            return Err("Only the matching paused track preview can resume".into());
        }
        let start_frame = self.pending_seek_frame.unwrap_or(0);
        if end_frame.is_some_and(|end| start_frame >= end) {
            return Err("Track preview has finished; start it again".into());
        }
        let solo_project = solo_preview_project(project, track_index)?;
        self.pending_play_end_frame = end_frame;
        let result = self.play(&solo_project, plugin_instances);
        self.mark_dirty();
        if result.is_ok() {
            self.track_preview = Some((track_index, end_frame));
            self.transport_pause_intent.store(false, Ordering::Release);
        } else {
            self.pending_play_end_frame = None;
            self.pending_seek_frame = Some(start_frame);
        }
        result.map(|_| ())
    }

    /// Resume the paused solo graph without preparing/reloading the arrangement.
    pub fn resume_track_preview(&mut self, track_index: usize) -> Result<(), String> {
        let (active_index, end_frame) = self
            .track_preview
            .ok_or("Track preview is no longer available; start it again")?;
        if active_index != track_index || self.playback_state != PlaybackState::Paused {
            return Err("Only the matching paused track preview can resume".into());
        }
        if !self.streaming_active {
            return Err("Track preview stream is no longer available".into());
        }
        let rt = self
            .render_thread
            .as_ref()
            .ok_or("Track preview graph is no longer available")?;
        if let Some(error) = rt.load_error() {
            return Err(format!("Track preview failed: {error}"));
        }
        let requested = self.pending_seek_frame;
        let start_frame =
            requested.unwrap_or_else(|| rt.playback.play_cursor_frames.load(Ordering::Acquire));
        let resume = requested.is_none();
        if end_frame.is_some_and(|end| start_frame >= end) {
            return Err("Track preview has finished; start it again".into());
        }
        let before_applied = rt.playback.play_applied.load(Ordering::Acquire);
        rt.send(RenderCommand::Play {
            start_frame,
            resume,
            end_frame,
        });
        self.pending_start = Some(PendingStart {
            started: std::time::Instant::now(),
            frame: start_frame,
            resume,
            before_applied,
        });
        self.pending_seek_frame = None;
        self.transport_pause_intent.store(false, Ordering::Release);
        self.transport_status = None;
        self.set_playback_state(PlaybackState::Playing);
        Ok(())
    }

    #[cfg(test)]
    /// Audition one pattern on the private audition stream; the transport and
    /// its cursor are not touched.
    pub fn preview_pattern(
        &mut self,
        project: &oc_dsl::ast::Project,
        pattern_name: &str,
        resource_root: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let started_ns = now_ns();
        let audition = pattern_audition_project(project, pattern_name)?;
        let prepared = oc_engine::block_source::prepare_project(&audition, resource_root)?;
        #[cfg(test)]
        let simulated_output = self.simulate_output;
        #[cfg(not(test))]
        let simulated_output = false;
        let spec = self.audio_io.spec();
        let prepared = Self::open_audition_render(
            &self.audio_io,
            &spec,
            self.soundfont_path.clone().unwrap_or_default(),
            prepared,
            simulated_output,
            started_ns,
            StartPath::PatternPreview,
        )?;
        self.install_audition_render(prepared)
    }

    #[cfg(test)]
    /// Render only the block's own window with the track soloed; the graph keeps
    /// the track's instruments and effects, so the audition matches the mix.
    pub(crate) fn render_block_window(
        project: &Project,
        track_index: usize,
        window: std::ops::Range<BarBeat>,
        sample_rate: u32,
        soundfont_path: &Path,
        plugin_instances: &mut std::collections::HashMap<
            String,
            oc_host::clap_instance::ClapInstance,
        >,
        resource_root: Option<&Path>,
    ) -> Result<Vec<f32>, String> {
        let (start_sample, end_sample) = block_preview_sample_range(
            project,
            track_index,
            window.start,
            window.end,
            sample_rate,
        )?;
        let mut cache = RenderCache::new();
        cache.set_worker_options(crate::gui_settings::load().worker_options());
        cache.set_resource_root(resource_root.map(Path::to_path_buf));
        let rendered = cache.render_solo_window(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RangeRenderRequest {
                start_sample,
                end_sample,
                tracks: Some(vec![track_index]),
                stems: false,
            },
        )?;
        Ok(rendered.audio)
    }

    /// Render a prepared audition project to a buffer; no lock, no device. The
    /// preview measures this render to hold its level under a ceiling.
    #[cfg(test)]
    pub(crate) fn render_prepared_audition(
        project: &Project,
        sample_rate: u32,
        soundfont_path: Option<&Path>,
        resource_root: Option<&Path>,
    ) -> Result<Vec<f32>, String> {
        let mut cache = RenderCache::new();
        cache.set_worker_options(crate::gui_settings::load().worker_options());
        cache.set_resource_root(resource_root.map(Path::to_path_buf));
        cache.set_minimum_duration(crate::project_extent::section_seconds(project)?)?;
        let soundfont = soundfont_path.unwrap_or_else(|| Path::new(""));
        cache.render_incremental(
            project,
            soundfont,
            sample_rate,
            &mut std::collections::HashMap::new(),
        )
    }

    /// Whether an OpenSynth audition stream is open for keyboard or live MIDI.
    pub fn live_synth_active(&self) -> bool {
        self.live_synth.is_some()
            && self
                .live_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|producer| !producer.failed())
    }

    pub(crate) fn live_synth_sounding(&self) -> bool {
        self.live_synth.is_some()
            && self
                .live_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(crate::live_synth::CommandProducer::sounding_or_pending)
    }

    /// Retire a failed callback before it can own another keyboard command.
    pub(crate) fn reap_live_synth_failure(&mut self) -> Option<String> {
        let failure = self
            .live_route
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(crate::live_synth::CommandProducer::take_failure);
        let error = failure?;
        self.end_live_synth();
        self.audio_io.note_stream_failure("main", &error);
        let status = format!("Synth audition stopped: {error}");
        self.transport_status = Some(status.clone());
        Some(status)
    }

    /// Apply live MPE zones to the current and future audition instruments.
    pub fn set_mpe_config(&mut self, config: Option<oc_synth::synth::MpeConfig>) {
        self.mpe_config = config;
        let mut route = self.live_route.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(producer) = route.as_mut() {
            let _ = producer.push(crate::live_synth::Command::Mpe(config));
        }
    }

    /// Clone the shared producer slot so a MIDI thread can route without this lock.
    pub(crate) fn live_input_route(&self) -> crate::live_synth::Route {
        Arc::clone(&self.live_route)
    }

    fn push_live_command(&self, command: crate::live_synth::Command) -> Result<(), ()> {
        let mut route = self.live_route.lock().unwrap_or_else(|e| e.into_inner());
        match route.as_mut() {
            Some(producer) => producer.push(command),
            None => Err(()),
        }
    }

    pub(crate) fn live_synth_note_on(
        &mut self,
        preset: &str,
        load_patch: impl FnOnce() -> Result<crate::live_synth::KeyboardPatch, String>,
        tempo: f64,
        note: u8,
        velocity: u8,
    ) -> Result<bool, String> {
        self.refuse_during_take("starting a synth audition")?;
        let _ = self.reap_live_synth_failure();
        let opened = self.live_synth.as_deref() != Some(preset);
        if opened {
            // Prepare successfully before disturbing current playback.
            // Repeated keys use the prepared instrument, not repeated file I/O.
            let patch = load_patch()?;
            let source = Arc::new(crate::live_synth::LiveSource::new(
                patch,
                tempo,
                self.mpe_config,
                Arc::clone(&self.live_route),
            ));
            self.stop_audition();
            self.track_preview = None;
            self.pending_play_end_frame = None;
            if self.playback_state == PlaybackState::Playing {
                self.pause();
            }
            if self.idle_monitor.is_none() {
                self.drop_stream();
            }
            self.transport_status = None;
            if let Err(error) = self.main_stream.open(self.main_source(source)) {
                self.sync_idle_monitor();
                return Err(error);
            }
            self.idle_monitor = None;
            self.monitor_output_error = None;
            self.live_synth = Some(preset.to_string());
        }
        let result = self.push_live_command(crate::live_synth::Command::On(
            note.min(127),
            velocity.clamp(1, 127),
        ));
        if result.is_err() {
            if let Some(error) = self.reap_live_synth_failure() {
                return Err(error);
            }
            self.end_live_synth();
            return Err("Synth audition queue full; audition stopped".into());
        }
        Ok(opened)
    }

    pub fn live_synth_note_off(&mut self, preset: &str, note: u8) -> Result<(), String> {
        let _ = self.reap_live_synth_failure();
        if self.live_synth.as_deref() == Some(preset)
            && self
                .push_live_command(crate::live_synth::Command::Off(note.min(127)))
                .is_err()
        {
            if self.reap_live_synth_failure().is_some() {
                return Ok(());
            }
            self.end_live_synth();
            return Err("Synth audition queue full; audition stopped".into());
        }
        Ok(())
    }

    pub fn invalidate_live_synth(&mut self, preset: &str) {
        if self.live_synth.as_deref() == Some(preset) {
            self.end_live_synth();
        }
    }

    /// Retire a loaded child when only its inherited patch changed.
    pub(crate) fn invalidate_inherited_live_synth(&mut self, before: &Project, after: &Project) {
        let Some(name) = self.live_synth.as_deref() else {
            return;
        };
        let previous = before.presets.iter().find(|preset| preset.name == name);
        let current = after.presets.iter().find(|preset| preset.name == name);
        if current.is_some_and(|preset| preset.extends.is_some())
            && previous == current
            && oc_dsl::preset::resolve_preset(&before.presets, name)
                != oc_dsl::preset::resolve_preset(&after.presets, name)
        {
            self.end_live_synth();
        }
    }

    /// Whole-project restoration cannot retain a patch from the previous snapshot.
    /// Leave ordinary transport alone when no keyboard audition is active.
    pub fn end_live_synth(&mut self) {
        if self.live_synth.is_some() {
            self.drop_stream();
            self.sync_idle_monitor();
        }
    }

    pub fn update_live_synth(&mut self, preset: &str, key: &str, value: f64) {
        let _ = self.reap_live_synth_failure();
        let Some(command) = crate::live_synth::Command::parameter(key, value) else {
            self.invalidate_live_synth(preset);
            return;
        };
        if self.live_synth.as_deref() == Some(preset) && self.push_live_command(command).is_err() {
            if self.reap_live_synth_failure().is_some() {
                return;
            }
            log::warn!("Synth audition queue full during edit; ending audition");
            self.end_live_synth();
        }
    }

    fn drop_stream(&mut self) {
        #[cfg(test)]
        {
            self.simulated_output = None;
        }
        self.main_callback_route.retire_current();
        self.main_stream.close();
        self.idle_monitor = None;
        self.live_synth = None;
        *self.live_route.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.streaming_active = false;
    }

    fn start_streaming_output(&mut self, playback: Arc<StreamingPlayback>) -> Result<(), String> {
        #[cfg(test)]
        if self.simulate_output {
            if self.idle_monitor.is_some() {
                self.drop_stream();
            }
            self.simulated_output = Some(SimulatedOutput::start(playback));
            return Ok(());
        }
        self.main_stream
            .open(self.transport_source(Arc::new(StreamingSource(Arc::clone(&playback)))))?;
        self.idle_monitor = None;
        self.monitor_output_error = None;
        Ok(())
    }
}

/// One line for a refused capture action, as the preferences section shows it.
fn format_input_failure(failure: &oc_audio::audio_io::AudioFailure) -> String {
    use oc_audio::audio_io::AudioFailure;
    match failure {
        AudioFailure::RecordingActive => {
            "Stop recording before changing audio devices or formats.".into()
        }
        AudioFailure::NoDevices => "No audio output devices found.".to_string(),
        AudioFailure::NoInputDevices => {
            "No audio input devices found. Connect one and choose ↻.".to_string()
        }
        AudioFailure::ListFailed { error } => format!("Could not list audio inputs: {error}"),
        AudioFailure::InputOpenFailed {
            device,
            error,
            fallback,
        } => match fallback {
            Some(fallback) => {
                format!("Could not open {device}: {error}. Using {fallback}.")
            }
            None => format!("Could not open {device}: {error}."),
        },
        AudioFailure::OpenFailed { device, error, .. } => {
            format!("Could not open {device}: {error}.")
        }
        AudioFailure::StreamLost { device, error } => {
            format!("Audio output stopped on {device}: {error}")
        }
    }
}

/// SoundFont voice for the audition figure; owns no state beyond the channel.
#[cfg(test)]
struct ProgramVoice<'a> {
    renderer: &'a oc_engine::soundfont::SharedSoundFontRenderer,
    channel: u8,
}

#[cfg(test)]
impl FigureVoice for ProgramVoice<'_> {
    fn note_on(&mut self, note: u8, velocity: u8) -> Result<(), String> {
        self.renderer
            .send(&oc_engine::interpret::MidiMessage::NoteOn {
                channel: self.channel,
                note,
                velocity,
            })
    }

    fn note_off(&mut self, note: u8) -> Result<(), String> {
        self.renderer
            .send(&oc_engine::interpret::MidiMessage::NoteOff {
                channel: self.channel,
                note,
            })
    }

    fn render(&mut self, frames: u32) -> Vec<f32> {
        self.renderer.render_samples(frames as usize)
    }
}

#[cfg(test)]
impl FigureVoice for oc_host::clap_instance::ClapInstance {
    fn note_on(&mut self, note: u8, velocity: u8) -> Result<(), String> {
        self.note_on(note, f64::from(velocity) / 127.0, 0);
        Ok(())
    }

    fn note_off(&mut self, note: u8) -> Result<(), String> {
        self.note_off(note, 0);
        Ok(())
    }

    fn render(&mut self, frames: u32) -> Vec<f32> {
        self.process_block(frames)
    }
}

#[cfg(test)]
impl FigureVoice for oc_host::vst3_instance::Vst3Instance {
    fn note_on(&mut self, note: u8, velocity: u8) -> Result<(), String> {
        self.note_on(note, f32::from(velocity) / 127.0, 0);
        Ok(())
    }

    fn note_off(&mut self, note: u8) -> Result<(), String> {
        self.note_off(note, 0);
        Ok(())
    }

    fn render(&mut self, frames: u32) -> Vec<f32> {
        self.process_block(frames)
    }
}

/// Test sink replaces only the physical device: real worker, commands, ring and
/// production callback still run. Drop joins the sole consumer before replacement.
#[cfg(test)]
struct SimulatedOutput {
    stop: Arc<AtomicBool>,
    nonzero: Arc<AtomicBool>,
    heard_blocks: Arc<AtomicU64>,
    /// Loudest sample handed to the device, as `f32` bits.
    peak: Arc<std::sync::atomic::AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(test)]
impl SimulatedOutput {
    fn start(playback: Arc<StreamingPlayback>) -> Self {
        Self::start_sink(Arc::new(StreamingSink(playback)))
    }
    fn start_sink(sink: Arc<dyn RenderSink>) -> Self {
        sink.opened();
        let stop = Arc::new(AtomicBool::new(false));
        let halted = stop.clone();
        let nonzero = Arc::new(AtomicBool::new(false));
        let heard = nonzero.clone();
        let heard_blocks = Arc::new(AtomicU64::new(0));
        let blocks = heard_blocks.clone();
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0.0_f32.to_bits()));
        let loudest = peak.clone();
        let thread = std::thread::spawn(move || {
            let mut block = [0.0; 256];
            let mut played = 0.0_f32;
            while !halted.load(Ordering::Acquire) {
                sink.fill(&mut block);
                assert!(block.iter().all(|v| v.is_finite()));
                if block.iter().any(|v| v.abs() > 1e-6) {
                    heard.store(true, Ordering::Release);
                    blocks.fetch_add(1, Ordering::Release);
                }
                for sample in &block {
                    played = played.max(sample.abs());
                }
                loudest.store(played.to_bits(), Ordering::Release);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        Self {
            stop,
            nonzero,
            heard_blocks,
            peak,
            thread: Some(thread),
        }
    }

    fn heard_blocks(&self) -> u64 {
        self.heard_blocks.load(Ordering::Acquire)
    }

    /// Loudest sample the device has played since this sink started.
    fn heard_peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Acquire))
    }
}
#[cfg(test)]
impl Drop for SimulatedOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

/// Test sink for the audition playback: runs the production buffer callback.
#[cfg(test)]
struct SimulatedAuditionOutput {
    stop: Arc<AtomicBool>,
    heard_blocks: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(test)]
impl SimulatedAuditionOutput {
    fn start(playback: Arc<AudioPlayback>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let halted = stop.clone();
        let heard_blocks = Arc::new(AtomicU64::new(0));
        let blocks = heard_blocks.clone();
        let thread = std::thread::spawn(move || {
            let mut block = [0.0; 256];
            while !halted.load(Ordering::Acquire) {
                audio_callback(&mut block, &playback);
                if block.iter().any(|v| v.abs() > 1e-6) {
                    blocks.fetch_add(1, Ordering::Release);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        Self {
            stop,
            heard_blocks,
            thread: Some(thread),
        }
    }

    fn heard_blocks(&self) -> u64 {
        self.heard_blocks.load(Ordering::Acquire)
    }
}
#[cfg(test)]
impl Drop for SimulatedAuditionOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn supports_block_sources(project: &Project) -> bool {
    let Ok(project) = oc_dsl::preset::resolve(project) else {
        return false;
    };
    supports_resolved_block_sources(&project)
}

fn supports_resolved_block_sources(project: &Project) -> bool {
    let Ok((project, _)) = crate::render_thread::split_source_lanes(project) else {
        return false;
    };
    project.tracks.iter().all(|t| {
        (t.performance.is_none()
            || t.preset.as_ref().is_some_and(|name| {
                project.presets.iter().any(|p| {
                    &p.name == name
                        && (p.plugin.as_deref() == Some("builtin.open_synth")
                            || matches!(p.source.as_deref(), Some("clap" | "vst2" | "vst3")))
                })
            }))
            && t.preset.as_ref().is_none_or(|name| {
                project.presets.iter().any(|p| {
                    &p.name == name
                        && (matches!(
                            p.plugin.as_deref(),
                            Some(
                                "builtin.open_synth"
                                    | "builtin.test_tone"
                                    | oc_synth::sampler::PLUGIN_ID
                                    | oc_synth::drumkit::PLUGIN_ID
                            )
                        ) || matches!(
                            p.source.as_deref(),
                            Some("clap" | "vst3" | "vst2" | "au" | "lv2")
                        ) || (p.source.as_deref() == Some("sample")
                            && p.plugin.as_deref() == Some("sfizz"))
                            || (p
                                .source
                                .as_deref()
                                .is_none_or(|source| source == "soundfont")
                                && p.plugin.is_none()))
                })
            })
    })
}

fn project_needs_plugin_aware_render(project: &Project) -> bool {
    let preset_needs_plugin_render = |preset_name: &str| {
        project
            .presets
            .iter()
            .find(|p| p.name == preset_name)
            .map(|p| {
                matches!(
                    p.source.as_deref(),
                    Some("clap") | Some("builtin") | Some("vst3") | Some("au")
                ) || p
                    .plugin
                    .as_deref()
                    .map(|id| id.starts_with("builtin."))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    };

    if project.tracks.iter().any(|track| {
        track.performance.is_some()
            || track
                .preset
                .as_deref()
                .map(&preset_needs_plugin_render)
                .unwrap_or(false)
            || track.inline_patterns.iter().any(|pattern| {
                pattern
                    .preset
                    .as_deref()
                    .map(&preset_needs_plugin_render)
                    .unwrap_or(false)
            })
    }) {
        return true;
    }

    if project.patterns.iter().any(|pattern| {
        pattern
            .preset
            .as_deref()
            .map(&preset_needs_plugin_render)
            .unwrap_or(false)
    }) {
        return true;
    }

    let has_plugin_effect = |effects: &[oc_dsl::ast::Effect]| {
        effects
            .iter()
            .any(|effect| matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_)))
    };

    project
        .tracks
        .iter()
        .any(|track| has_plugin_effect(&track.effects))
        || project
            .buses
            .iter()
            .any(|bus| has_plugin_effect(&bus.effects))
        || project
            .master
            .as_ref()
            .map(|master| has_plugin_effect(&master.effects))
            .unwrap_or(false)
}

pub(crate) fn prepare_preset_audition(
    project: &Project,
    name: &str,
    resource_root: Option<&Path>,
) -> Result<Project, String> {
    let audition = preset_audition_project(project, name)?;
    let solo = solo_preview_project(&audition, 0)?;
    oc_engine::block_source::prepare_project(&solo, resource_root)
        .map_err(|error| error.to_string())
}

/// The octave the shared figure plays in for this patch.
///
/// A sub-bass preset auditioned at C4-C5 sits two octaves above its own filter
/// cutoff, so it is nearly silent rather than quiet. The figure moves down for a
/// low patch instead of the preview gaining it into hiss.
fn audition_register_offset(project: &Project, name: &str) -> i16 {
    let cutoff = project
        .presets
        .iter()
        .find(|preset| preset.name == name)
        .and_then(|preset| {
            preset
                .params
                .iter()
                .find_map(|(key, value)| match (key.as_str(), value) {
                    ("filter.cutoff", oc_dsl::ast::ParamValue::Number(hz)) => Some(*hz),
                    _ => None,
                })
        });
    match cutoff {
        Some(hz) if hz < 500.0 => -24,
        Some(hz) if hz < 1_200.0 => -12,
        _ => 0,
    }
}

/// Builtin/sampler/SoundFont preset audition: the shared figure on whichever
/// patch the preset resolves to.
pub(crate) fn preset_audition_project(project: &Project, name: &str) -> Result<Project, String> {
    if !project.presets.iter().any(|preset| preset.name == name) {
        return Err(format!("Preset '{name}' not found"));
    }
    let mut audition = oc_dsl::parse("@project\n@track audition | instrument: synth/lead_square\n  @phrase notes [1:1:0 - 2:1:0]\n").map_err(|e| e.to_string())?;
    audition.properties.tempo = crate::audition_figure::TEMPO_BPM;
    audition.presets = project.presets.clone();
    audition.tracks[0].preset = Some(name.into());
    let drums = project
        .presets
        .iter()
        .any(|p| p.name == name && p.plugin.as_deref() == Some(oc_synth::drumkit::PLUGIN_ID))
        || project
            .tracks
            .iter()
            .any(|t| t.preset.as_deref() == Some(name) && t.instrument.category == "drums")
        || project
            .patterns
            .iter()
            .any(|p| p.preset.as_deref() == Some(name) && p.instrument.category == "drums");
    if drums {
        audition.tracks[0].instrument = oc_dsl::ast::InstrumentPath::new("drums", "drumkit");
        audition.tracks[0].channel = Some(9);
    }
    let register = if drums {
        0
    } else {
        audition_register_offset(project, name)
    };
    audition.tracks[0].phrases[0].events =
        crate::audition_figure::phrase_events_in(drums, register);
    Ok(audition)
}

/// One-track project that plays a single pattern from its start. The pattern
/// keeps its instrument, preset, effects, swing and transpose.
pub(crate) fn pattern_audition_project(
    project: &Project,
    pattern_name: &str,
) -> Result<Project, String> {
    let pattern = project
        .patterns
        .iter()
        .chain(
            project
                .tracks
                .iter()
                .flat_map(|track| track.inline_patterns.iter()),
        )
        .find(|pattern| pattern.name == pattern_name)
        .cloned()
        .ok_or_else(|| format!("Pattern '{pattern_name}' not found"))?;
    let mut audition = project.clone();
    audition.name = Some("Clip audition".into());
    audition.tracks = vec![oc_dsl::ast::Track {
        name: "Audition".into(),
        instrument: pattern.instrument.clone(),
        preset: pattern.preset.clone(),
        engine: pattern.engine.clone(),
        mix: oc_dsl::ast::MixState::default(),
        effects: pattern.effects.clone(),
        phrases: Vec::new(),
        patterns: Vec::new(),
        inline_patterns: vec![pattern],
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: vec![],
    }];
    audition.patterns.clear();
    audition.buses.clear();
    audition.master = None;
    audition.sections.clear();
    audition.clear_audio_clips();
    audition.properties.mpe = None;
    Ok(audition)
}

pub(crate) fn solo_preview_project(
    project: &Project,
    track_index: usize,
) -> Result<Project, String> {
    if track_index >= project.tracks.len() {
        return Err(format!("Track {track_index} out of range"));
    }
    let mut solo = project.clone();
    // Keep source indices, sidechain dependencies and routing identities.
    for (index, track) in solo.tracks.iter_mut().enumerate() {
        track.mix.mute = index != track_index;
        track.mix.solo = false;
    }
    Ok(solo)
}

/// Ring-out kept after previewed content so bus and master effects decay.
/// Four seconds covers typical reverb and delay tails without empty bars.
const PREVIEW_TAIL_SECONDS: f64 = 4.0;

/// First and last audible content on a track in rendered quarters; `None`
/// without content. Ranges follow the expanded arrangement, so repeats count.
fn track_preview_range_quarters(
    project: &Project,
    track_index: usize,
    clock: &oc_dsl::timing::ProjectClock,
) -> Result<Option<(f64, f64)>, String> {
    let expanded = oc_dsl::arrangement::expand(project)?;
    let Some(track) = expanded.tracks.get(track_index) else {
        return Ok(None);
    };
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let ranges = track
        .pattern_clips()
        .map(|clip| &clip.time_range)
        .chain(
            track
                .phrases
                .iter()
                .filter_map(|phrase| phrase.time_range.as_ref()),
        )
        .chain(
            expanded
                .audio_clips()
                .into_iter()
                .filter(|clip| clip.track == track.name)
                .map(|clip| &clip.time_range),
        );
    for range in ranges {
        starts.push(clock.quarter_notes_at(&range.start)?);
        ends.push(clock.quarter_notes_at(&range.end)?);
    }
    // A pattern lane lasts its repeat count of bars, not just its start bar.
    for pattern in &track.patterns {
        let Some(range) = &pattern.time_range else {
            continue;
        };
        starts.push(clock.quarter_notes_at(&range.start)?);
        let end_bar = range
            .start
            .bar
            .checked_add(pattern.repeat)
            .ok_or("pattern repeat bar overflow")?;
        ends.push(clock.quarter_notes_at(&oc_dsl::ast::BarBeat::new(end_bar, 1, 0))?);
    }
    if let Some(timeline) = &track.performance {
        for event in &timeline.events {
            starts.push(event.quarter);
            ends.push(event.quarter);
        }
    }
    let Some(start) = starts.into_iter().reduce(f64::min) else {
        return Ok(None);
    };
    let end = ends.into_iter().reduce(f64::max).unwrap_or(start);
    Ok(Some((start, end.max(start))))
}

/// Preview stop frame: content end plus the effect tail allowance.
fn preview_end_frame(
    clock: &oc_dsl::timing::ProjectClock,
    end_quarter: f64,
    sample_rate: u32,
) -> Result<u64, String> {
    let content_end = clock.sample_at_quarter(end_quarter, sample_rate)?;
    let tail = (PREVIEW_TAIL_SECONDS * f64::from(sample_rate)).ceil() as u64;
    content_end
        .checked_add(tail)
        .ok_or_else(|| "preview end frame overflow".to_string())
}

/// Sample window of one block's own range; empty or reversed ranges are rejected.
pub(crate) fn block_preview_sample_range(
    project: &Project,
    track_index: usize,
    start: BarBeat,
    end: BarBeat,
    sample_rate: u32,
) -> Result<(u64, u64), String> {
    let track = project
        .tracks
        .get(track_index)
        .ok_or_else(|| format!("Track {track_index} out of range"))?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let start_quarter = clock.quarter_notes_at(&start)?;
    let end_quarter = clock.quarter_notes_at(&end)?;
    let start_sample = clock.sample_at_quarter(start_quarter, sample_rate)?;
    // Test the block itself for length; the tail below must not make an empty
    // block look previewable.
    let content_end = clock.sample_at_quarter(end_quarter, sample_rate)?;
    // The window keeps the block's own ring-out instead of cutting it dead.
    let end_sample = preview_end_frame(&clock, end_quarter, sample_rate)?;
    if content_end <= start_sample {
        return Err(format!(
            "Block on track '{}' has no audible length",
            track.name
        ));
    }
    Ok((start_sample, end_sample))
}

// ============================================================================
// cpal stream
// ============================================================================

// Counters are interned once when a device opens, so the callback only touches
// atomics: no allocation and no lock on the realtime thread.
static CALLBACK_COUNT: std::sync::OnceLock<Option<&'static oc_diag::Counter>> =
    std::sync::OnceLock::new();
static CALLBACK_MS: std::sync::OnceLock<Option<&'static oc_diag::Counter>> =
    std::sync::OnceLock::new();
static UNDERRUNS: std::sync::OnceLock<Option<&'static oc_diag::Counter>> =
    std::sync::OnceLock::new();

fn register_audio_counters() {
    let _ = CALLBACK_COUNT.set(oc_diag::perf_counter("audio.callback.count"));
    let _ = CALLBACK_MS.set(oc_diag::perf_counter("audio.callback.ms"));
    let _ = UNDERRUNS.set(oc_diag::perf_counter("audio.underrun"));
}

fn bump(counter: &std::sync::OnceLock<Option<&'static oc_diag::Counter>>) {
    if let Some(counter) = counter.get().and_then(|counter| *counter) {
        counter.add(1);
    }
}

/// Pause intent can arrive while the app lock is held by source preparation.
struct IntentGatedSource {
    source: Arc<dyn RenderSource>,
    gate: Arc<AtomicBool>,
}
impl RenderSource for IntentGatedSource {
    fn create(&self, spec: &StreamSpec) -> Arc<dyn RenderSink> {
        Arc::new(IntentGatedSink {
            sink: self.source.create(spec),
            gate: Arc::clone(&self.gate),
        })
    }
}
struct IntentGatedSink {
    sink: Arc<dyn RenderSink>,
    gate: Arc<AtomicBool>,
}
impl RenderSink for IntentGatedSink {
    fn opened(&self) {
        self.sink.opened();
    }

    fn fill(&self, output: &mut [f32]) {
        if self.gate.load(Ordering::Acquire) {
            output.fill(0.0);
        } else {
            self.sink.fill(output);
        }
    }
    fn failed(&self, error: String) {
        self.sink.failed(error);
    }
}

/// Both workers are consumed by the same device callback. Neither cursor moves
/// unless both have matching frames ready; no full-figure buffers are retained.
struct PairedStreamingSource {
    a: Arc<StreamingPlayback>,
    b: Arc<StreamingPlayback>,
    selected: Arc<AtomicBool>,
    gains: Arc<[std::sync::atomic::AtomicU32; 2]>,
    previous: [std::sync::atomic::AtomicU32; 2],
    ramp_frames: f32,
}
impl RenderSource for PairedStreamingSource {
    fn create(&self, _spec: &StreamSpec) -> Arc<dyn RenderSink> {
        Arc::new(Self {
            a: self.a.clone(),
            b: self.b.clone(),
            selected: self.selected.clone(),
            gains: self.gains.clone(),
            previous: std::array::from_fn(|_| std::sync::atomic::AtomicU32::new(1.0_f32.to_bits())),
            ramp_frames: self.ramp_frames,
        })
    }
}
impl RenderSink for PairedStreamingSource {
    fn opened(&self) {
        StreamingSink(self.a.clone()).opened();
        StreamingSink(self.b.clone()).opened();
    }

    fn fill(&self, output: &mut [f32]) {
        output.fill(0.0);
        // Empty callbacks acknowledge seeks even while paused, without consuming.
        for playback in [&self.a, &self.b] {
            if playback.seek_generation.load(Ordering::Acquire)
                != playback.callback_generation.load(Ordering::Acquire)
            {
                streaming_audio_callback(&mut [], playback);
            }
        }
        if [&self.a, &self.b].iter().any(|p| {
            p.output_suspended.load(Ordering::Acquire)
                || !p.active.load(Ordering::Acquire)
                || p.position_applied.load(Ordering::Acquire)
                    != p.position_requested.load(Ordering::Acquire)
                || p.seek_generation.load(Ordering::Acquire)
                    != p.callback_generation.load(Ordering::Acquire)
        }) {
            return;
        }
        // A completed pair can restart its two workers independently; wait
        // until both seek handshakes have reached the same cursor.
        if self.a.play_cursor_frames.load(Ordering::Acquire)
            != self.b.play_cursor_frames.load(Ordering::Acquire)
        {
            return;
        }
        // SAFETY: this is the sole consumer of both rings; device teardown joins
        // the callback before either graph can be replaced.
        let a = unsafe { &mut *self.a.consumer_ptr() };
        // SAFETY: this callback exclusively consumes B's ring, and device
        // teardown joins it before B's graph or consumer can be replaced.
        let b = unsafe { &mut *self.b.consumer_ptr() };
        let samples = output.len().min(a.slots()).min(b.slots()) / 2 * 2;
        let side = self.selected.load(Ordering::Acquire);
        let mut gains = self
            .previous
            .each_ref()
            .map(|g| f32::from_bits(g.load(Ordering::Relaxed)));
        let targets = self
            .gains
            .each_ref()
            .map(|g| f32::from_bits(g.load(Ordering::Acquire)));
        let steps = std::array::from_fn::<_, 2, _>(|i| (targets[i] - gains[i]) / self.ramp_frames);
        for (index, sample) in output.iter_mut().take(samples).enumerate() {
            if index % 2 == 0 {
                for i in 0..2 {
                    if (targets[i] - gains[i]).abs() <= steps[i].abs() {
                        gains[i] = targets[i];
                    } else {
                        gains[i] += steps[i];
                    }
                }
            }
            let av = a.pop().expect("paired ring availability");
            let bv = b.pop().expect("paired ring availability");
            *sample = if side { bv * gains[1] } else { av * gains[0] };
        }
        for (previous, gain) in self.previous.iter().zip(gains) {
            previous.store(gain.to_bits(), Ordering::Relaxed);
        }
        for playback in [&self.a, &self.b] {
            playback
                .play_cursor_frames
                .fetch_add((samples / 2) as u64, Ordering::Release);
            playback
                .device_buffer_frames
                .store((output.len() / 2) as u64, Ordering::Relaxed);
        }
    }
    fn failed(&self, error: String) {
        StreamingSink(self.a.clone()).failed(error.clone());
        StreamingSink(self.b.clone()).failed(error);
    }
}

/// Main streaming output: one device stream reads the render thread's ring.
struct StreamingSource(Arc<StreamingPlayback>);

impl RenderSource for StreamingSource {
    fn create(&self, _spec: &StreamSpec) -> Arc<dyn RenderSink> {
        register_audio_counters();
        Arc::new(StreamingSink(Arc::clone(&self.0)))
    }
}

struct StreamingSink(Arc<StreamingPlayback>);

impl RenderSink for StreamingSink {
    fn opened(&self) {
        self.0.take_stream_failure();
    }

    fn fill(&self, output: &mut [f32]) {
        streaming_audio_callback(output, &self.0);
    }

    fn failed(&self, error: String) {
        log::error!("streaming audio output stopped: {error}");
        *self
            .0
            .stream_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error);
        self.0.stream_failed.store(true, Ordering::Release);
    }
}

/// Preview output: one device stream reads a rendered buffer.
struct BufferSource(Arc<AudioPlayback>);

impl RenderSource for BufferSource {
    fn create(&self, _spec: &StreamSpec) -> Arc<dyn RenderSink> {
        register_audio_counters();
        Arc::new(BufferSink(Arc::clone(&self.0)))
    }
}

struct BufferSink(Arc<AudioPlayback>);

impl RenderSink for BufferSink {
    fn opened(&self) {
        self.0
            .stream_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    fn fill(&self, output: &mut [f32]) {
        audio_callback(output, &self.0);
    }

    fn failed(&self, error: String) {
        log::error!("audio output stopped: {error}");
        *self
            .0
            .stream_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error);
    }
}

/// Lock-free audio callback. No allocations, no locks.
/// Uses compare_exchange loop to avoid losing seek() updates that happen
/// between our load and store of the cursor.
fn audio_callback(output: &mut [f32], playback: &AudioPlayback) {
    bump(&CALLBACK_COUNT);
    let _timer = CALLBACK_MS
        .get()
        .and_then(|counter| *counter)
        .map(oc_diag::Timer::start);
    if !playback.playing.load(Ordering::Acquire) {
        output.fill(0.0);
        return;
    }

    let buf = playback.active_buffer();
    let buf_len = buf.len() as u64;
    let requested = output.len() as u64;

    // Atomically advance the cursor using compare_exchange to avoid
    // overwriting a concurrent seek() operation.
    let mut cursor = playback.cursor.load(Ordering::Acquire);
    loop {
        if cursor >= buf_len {
            bump(&UNDERRUNS);
            playback.playing.store(false, Ordering::Release);
            output.fill(0.0);
            return;
        }

        let available = buf_len - cursor;
        let to_copy = requested.min(available);
        let new_cursor = cursor + to_copy;

        match playback.cursor.compare_exchange_weak(
            cursor,
            new_cursor,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // Successfully advanced cursor — copy audio data
                let src_start = cursor as usize;
                let to_copy = to_copy as usize;
                output[..to_copy].copy_from_slice(&buf[src_start..src_start + to_copy]);
                output[to_copy..].fill(0.0);
                // This path plays an already rendered buffer, so whatever it
                // computes before its first block is the whole render.
                playback
                    .first_block
                    .observe(to_copy as u64 / 2, (output.len() / 2) as u64, 0);
                return;
            }
            Err(actual) => {
                // Cursor was changed (likely by seek()) — retry with new value
                cursor = actual;
            }
        }
    }
}

// ============================================================================
// Streaming audio output (ring buffer based)
// ============================================================================

/// Lock-free streaming audio callback. Reads from ring buffer.
/// Outputs silence on underrun (ring buffer empty while the pass still has
/// audio to play) and on the end of the pass, which is not counted as one.
/// On seek: discards stale data from the old position before reading new audio.
pub(crate) fn streaming_audio_callback(output: &mut [f32], playback: &StreamingPlayback) {
    let take_run = playback.play_applied.load(Ordering::Acquire);
    // SAFETY: The audio callback is the only thread that reads from the consumer.
    // This is guaranteed by cpal's single-callback-thread model.
    let consumer = unsafe { &mut *playback.consumer_ptr() };

    // This block is presented one callback period later; the UI position has to
    // trail the ring by that period to show the audio being heard.
    if output.len() >= 2 {
        playback
            .device_buffer_frames
            .store((output.len() / 2) as u64, Ordering::Relaxed);
    }

    // Check for seek — if generation changed, flush all stale data from the buffer.
    // This runs even while inactive: a paused stream must still acknowledge the
    // generation or the producer would wait for an ack that never comes.
    let current_gen = playback.seek_generation.load(Ordering::Acquire);
    let callback_gen = playback.callback_generation.load(Ordering::Acquire);
    if current_gen != callback_gen {
        // Discard everything currently in the ring buffer (stale audio from old position)
        let stale = consumer.slots();
        if stale > 0
            && let Ok(chunk) = consumer.read_chunk(stale)
        {
            chunk.commit_all(); // discard
        }
        playback
            .callback_generation
            .store(current_gen, Ordering::Release);
        // A seek resumes the exact audio the transport asked for, so it starts
        // on its own first sample. A loop wrap drains the ring instead, so its
        // silence comes from the underrun below and its audio fades in there.
        // Output silence this callback — fresh data will arrive next callback
        output.fill(0.0);
        return;
    }

    if playback.output_suspended.load(Ordering::Acquire)
        || !playback.active.load(Ordering::Acquire)
        || playback.position_applied.load(Ordering::Acquire) != playback.position_requested.load(Ordering::Acquire)
        // Play can finish between the initial seek handshake and this gate.
        // Never consume an old ring under its newly acknowledged position.
        || playback.seek_generation.load(Ordering::Acquire) != current_gen
        || playback.play_applied.load(Ordering::Acquire) != take_run
    {
        output.fill(0.0);
        return;
    }

    let available = consumer.slots();
    let requested = output.len();

    if available == 0 {
        // The pass played out: the producer has nothing left to send, so an
        // empty ring is the end of the program rather than a starve. Clearing
        // the fade keeps the next play starting on its own first sample.
        if playback.has_played_out() {
            playback.resume_fade.store(0, Ordering::Relaxed);
            output.fill(0.0);
            return;
        }
        // Underrun — output silence. Only count once audio has flowed for this
        // generation; the silent gap before the first chunk is not an underrun.
        // The audio that follows a starve fades in, so the gap is heard as a
        // dip instead of a step into the waveform.
        if playback.ready.load(Ordering::Acquire) {
            playback.underruns.fetch_add(1, Ordering::Relaxed);
            playback
                .resume_fade
                .store(RESUME_FADE_FRAMES, Ordering::Relaxed);
        }
        output.fill(0.0);
        return;
    }

    let to_read = requested.min(available);

    if let Ok(read_chunk) = consumer.read_chunk(to_read) {
        let (first, second) = read_chunk.as_slices();
        let first_len = first.len().min(requested);
        output[..first_len].copy_from_slice(&first[..first_len]);
        let mut written = first_len;
        if written < to_read && !second.is_empty() {
            let second_len = second.len().min(requested - written);
            output[written..written + second_len].copy_from_slice(&second[..second_len]);
            written += second_len;
        }
        // Audio that follows a starve fades in.
        let mut resume_fade = playback.resume_fade.load(Ordering::Relaxed);
        if resume_fade > 0 {
            fade_in_head(output, written / 2, &mut resume_fade);
            playback.resume_fade.store(resume_fade, Ordering::Relaxed);
        }
        // Audio that stops early fades out instead of stepping to silence, and
        // the audio after it fades in for the same reason.
        if written < requested {
            if playback.ready.load(Ordering::Acquire) {
                fade_out_tail(output, written);
                playback
                    .resume_fade
                    .store(RESUME_FADE_FRAMES, Ordering::Relaxed);
            }
            output[written..].fill(0.0);
        }
        read_chunk.commit_all();

        // Advance the timeline cursor only for samples actually committed, and
        // not for the count-in: it sounds before the timeline and is not part
        // of its position.
        let frames_read = (written / 2) as u64;
        let count_in_left = playback.count_in_remaining.load(Ordering::Acquire);
        let count_in_read = frames_read.min(count_in_left);
        if count_in_read > 0 {
            playback
                .count_in_remaining
                .store(count_in_left - count_in_read, Ordering::Release);
        }
        if frames_read > count_in_read {
            playback.count_take_device_frames(take_run, frames_read - count_in_read);
            playback
                .play_cursor_frames
                .fetch_add(frames_read - count_in_read, Ordering::Release);
        }
        // A loop wrap keeps the ring running across the seam, so the playhead
        // jumps where the audio does instead of counting past the region.
        if frames_read > 0
            && let Some(seam) = playback.loop_seam()
        {
            let cursor = playback.timeline_frames();
            if cursor >= seam.end {
                playback
                    .play_cursor_frames
                    .store(seam.start + (cursor - seam.end), Ordering::Release);
            }
        }
        // The first non-empty block for this start is now at the device sink.
        playback.first_block.observe(
            frames_read,
            (output.len() / 2) as u64,
            (available / 2).saturating_sub(frames_read as usize) as u64,
        );
    } else {
        output.fill(0.0);
    }
    // Scrub mute silences output, not the transport clock or seek handshake.
    // Consume only this callback's frames, never discard the whole lookahead.
    if playback.muted.load(Ordering::Acquire) {
        output.fill(0.0);
    }
}

/// Frames over which audio that follows a starve fades in, and audio that stops
/// early fades out. Two milliseconds sits below the ear's click threshold, so a
/// gap is heard as a dip in level rather than as a step in the waveform.
pub(crate) const RESUME_FADE_FRAMES: u32 = 96;

/// Raised-cosine shape of the starve fade: 0 at the silence, 1 at the end.
fn starve_gain(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Fade the tail of `written` interleaved samples out to silence.
pub(crate) fn fade_out_tail(output: &mut [f32], written: usize) {
    let frames = (written / 2).min(RESUME_FADE_FRAMES as usize);
    for index in 0..frames {
        let t = 1.0 - (index as f32 + 0.5) / frames as f32;
        let gain = starve_gain(t);
        let start = written - (frames - index) * 2;
        output[start] *= gain;
        output[start + 1] *= gain;
    }
}

/// Fade the first `frames` frames in, carrying the remaining fade across
/// callbacks so a starve shorter than one callback period stays continuous.
pub(crate) fn fade_in_head(output: &mut [f32], frames: usize, remaining: &mut u32) {
    let done = RESUME_FADE_FRAMES.saturating_sub(*remaining);
    let take = (*remaining as usize).min(frames);
    for index in 0..take {
        let t = (done as usize + index + 1) as f32 / RESUME_FADE_FRAMES as f32;
        let gain = starve_gain(t);
        output[index * 2] *= gain;
        output[index * 2 + 1] *= gain;
    }
    *remaining -= take as u32;
}

// ============================================================================
// SoundFont discovery
// ============================================================================

fn find_default_soundfont() -> Option<PathBuf> {
    // Check environment variable first (highest priority)
    if let Ok(sf) = std::env::var("OCP_SOUNDFONT") {
        let p = PathBuf::from(sf);
        if p.exists() {
            return Some(p);
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().unwrap_or_default();
    default_soundfont_in(exe_dir.as_deref(), &cwd)
}

/// The search order used at startup and by tests: the executable's directory
/// first (a packaged app), then the working directory (a development checkout).
fn default_soundfont_in(exe_dir: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    // Try relative to the executable (works in packaged app)
    let relative_candidates = [
        "soundfonts/FluidR3Mono_GM.sf3",
        "../soundfonts/FluidR3Mono_GM.sf3",
        "../../soundfonts/FluidR3Mono_GM.sf3",
        "../Resources/soundfonts/FluidR3Mono_GM.sf3", // macOS .app bundle
    ];

    if let Some(exe_dir) = exe_dir {
        for c in &relative_candidates {
            let p = exe_dir.join(c);
            if p.is_file() {
                return Some(p.canonicalize().unwrap_or(p));
            }
        }
    }

    // Fallback: try relative to current working directory (for development
    // builds before Tauri bundle resources are copied).
    let cwd_candidates = [
        "soundfonts/FluidR3Mono_GM.sf3",
        "../soundfonts/FluidR3Mono_GM.sf3",
        "../../soundfonts/FluidR3Mono_GM.sf3",
        "crates/oc-gui/frontend/src-tauri/resources/soundfonts/FluidR3Mono_GM.sf3",
    ];
    for c in &cwd_candidates {
        let p = cwd.join(c);
        if p.is_file() {
            return Some(p.canonicalize().unwrap_or(p));
        }
    }

    // The resource directory is not always next to the executable (deb and
    // AppImage), so Tauri's resolver gets the final say in setup().
    None
}

#[cfg(test)]
mod tests;
