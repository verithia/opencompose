//! Background streaming render thread.
//!
//! Architecture:
//!   Main thread → [Command channel] → Render thread → [Ring buffer] → Audio callback
//!
//! The render thread synthesizes audio in chunks (CHUNK_FRAMES) ahead of the playhead.
//! It keeps independent instrument sources and effect chains alive during playback,
//! only recreating them when the project structure changes.
//!
//! The audio callback is completely lock-free: it reads from the ring buffer
//! and outputs silence on underrun.

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use oc_audio::click::{ClickEvent, ClickSettings, ClickTrack, CountInPlan};
use oc_audio::{AudioProcessContext, AudioProcessInput};
use oc_dsl::ast::{Effect, InstrumentPath, ParamValue, PresetDef, Project};
use oc_dsl::timing::ProjectClock;
use oc_engine::block_source::{BlockSource, MAX_SOURCE_BLOCK_FRAMES, SmartSource, SourceOutput};
use oc_engine::clip_lane::ClipLanes;
use oc_engine::graph::{AudioGraph, PreparedGraphBlock};
use oc_engine::interpret::{self, InterpretSettings, MidiEvent, MidiMessage};
use oc_engine::media_cache::MediaKey;
use oc_host::PluginTransport;

use crate::first_block::{FirstBlock, StartPath};
use crate::meters::{Correlation, GONIOMETER_VALUES, MeterHub, MonitorValues, goniometer};

/// How many frames to render per steady-state chunk. ~93ms at 44100Hz.
/// Large enough to amortize per-chunk overhead, small enough for responsive seek.
pub const CHUNK_FRAMES: usize = 4096;

/// The first chunk of a playback generation is short so sound starts sooner.
pub const FIRST_CHUNK_FRAMES: usize = 1024;

/// Ring buffer capacity in frames. ~2 seconds of capacity, not a fill target.
const RING_BUFFER_FRAMES: usize = 88200;

/// Queued audio never exceeds this; a mixer edit is heard within one window.
pub const LOOKAHEAD_FRAMES: usize = 12000;

/// Bound on waiting for the callback to acknowledge a seek generation.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// A seek that happens while the transport is playing may spend this long
/// reconstructing the skipped audio. Past it the destination starts with its
/// voices released: a jump must not hold the device silent for the whole gap.
const SEEK_PREROLL_BUDGET: std::time::Duration = std::time::Duration::from_millis(20);

/// MIDI channels and notes a lane can hold; the sounding set is one bit per
/// (channel, note) pair, so it never reallocates while rendering.
const SOUNDING_CHANNELS: usize = 16;
const SOUNDING_NOTES: usize = 128;
const SOUNDING_WORDS: usize = SOUNDING_CHANNELS * SOUNDING_NOTES / 64;

/// One lane's slice of the sounding-note bitset.
fn lane_sounding(words: &mut [u64], lane: usize) -> &mut [u64] {
    &mut words[lane * SOUNDING_WORDS..][..SOUNDING_WORDS]
}

/// Mark one (channel, note) pair as sounding or released.
fn set_sounding(words: &mut [u64], channel: u8, note: u8, on: bool) {
    if usize::from(channel) >= SOUNDING_CHANNELS || usize::from(note) >= SOUNDING_NOTES {
        return;
    }
    let index = usize::from(channel) * SOUNDING_NOTES + usize::from(note);
    let mask = 1u64 << (index % 64);
    let word = &mut words[index / 64];
    if on {
        *word |= mask;
    } else {
        *word &= !mask;
    }
}

/// Record a dispatched score event in the sounding set.
fn track_sounding(words: &mut [u64], message: &MidiMessage) {
    match message {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } => set_sounding(words, *channel, *note, *velocity > 0),
        MidiMessage::NoteOff { channel, note } => set_sounding(words, *channel, *note, false),
        _ => {}
    }
}

/// Record a dispatched performance command in the sounding set.
fn track_sounding_command(
    words: &mut [u64],
    channel: u8,
    command: &oc_dsl::performance::ChannelCommand,
) {
    match command {
        oc_dsl::performance::ChannelCommand::NoteOn { note, velocity } => {
            set_sounding(words, channel, *note, *velocity > 0)
        }
        oc_dsl::performance::ChannelCommand::NoteOff { note, .. } => {
            set_sounding(words, channel, *note, false)
        }
        _ => {}
    }
}

/// Meter frames the producer may hold for the device; the lookahead bounds
/// the real number at a handful.
const MAX_PENDING_METERS: usize = 16;

/// Callback buffers of presentation delay between handing audio to the device
/// and hearing it. A device callback fills the buffer that plays one period
/// later, so the cursor has to trail the ring by that period (see
/// `docs/playback-latency.md`).
pub const DEVICE_LATENCY_BLOCKS: u64 = 1;

/// Warm-up lifecycle reported to the transport UI, and never a device start.
pub const WARM_IDLE: u8 = 0;
/// Warm-up is building sources.
pub const WARM_BUILDING: u8 = 1;
/// Warm-up is ready.
pub const WARM_READY: u8 = 2;

/// The one live-monitoring latency quantity: the device holds one callback
/// period of audio, and the live master path reports the rest.
pub fn monitor_latency_frames(device_buffer_frames: u64, chain_latency_frames: u64) -> u64 {
    device_buffer_frames * DEVICE_LATENCY_BLOCKS + chain_latency_frames
}

// ============================================================================
// Commands from main thread → render thread
// ============================================================================

/// Commands sent from the main thread to the render thread.
pub enum RenderCommand {
    #[cfg(test)]
    TestBarrier {
        entered: Sender<()>,
        release: Receiver<()>,
    },
    /// Start/resume rendering from a given sample position.
    Play {
        start_frame: u64,
        resume: bool,
        /// Play only up to this frame (a track preview's content bound).
        end_frame: Option<u64>,
    },
    /// Pause rendering (keep state for resume).
    Pause,
    /// Seek to a new position (flush buffer, re-render from there).
    Seek { frame: u64 },
    /// Stop and reset everything.
    Stop,
    /// Project has changed — re-interpret and invalidate.
    ProjectChanged {
        project: Box<Project>,
        soundfont_path: PathBuf,
        /// Directory the project's relative media paths resolve against. The
        /// worker holds no panel state, so the load carries its own.
        root: Option<PathBuf>,
        /// Pre-render the opening lookahead while stopped (warm-up on load).
        warm: bool,
    },
    /// Mixer-only change: patch the live graph, never rebuild existing instruments.
    MixerChanged {
        project: Box<Project>,
        soundfont_path: PathBuf,
        /// Monotonic instant of the edit, for the lane-build measurement.
        requested_ns: u64,
    },
    /// FX-chain-only change: patch the affected effect chains in the live graph.
    /// Instruments, sounding voices, the ring and the playhead stay untouched.
    ChainChanged {
        project: Box<Project>,
        soundfont_path: PathBuf,
    },
    /// Point the analyser at one node, or stop it with None.
    Spectrum { target: Option<String> },
    /// The click's level and count-in choice.
    Click { settings: ClickSettings },
    /// Emit this count-in before the timeline starts.
    CountIn { plan: CountInPlan },
    /// Pruned lanes finished building on the source worker; splice them in.
    SourcesBuilt { epoch: u64, lanes: Vec<BuiltLane> },
    /// A source worker build failed; the load error is published and playback stops.
    SourceBuildFailed { epoch: u64, error: String },
    /// Shutdown the render thread.
    Shutdown,
}

/// A count-in being emitted: the plan and how much of it has sounded.
struct CountInState {
    plan: CountInPlan,
    emitted: u64,
}

/// Result of an FX-chain edit on a loaded project.
enum ChainOutcome {
    /// The affected chains were patched; sources and the playhead untouched.
    Patched { chains: u64 },
    /// The document could not be absorbed by a chain patch; it was reloaded.
    Reloaded,
}

/// Result of a mixer edit on a loaded project.
enum MixerOutcome {
    /// Graph patched in place; instruments and timeline untouched.
    Patched,
    /// The project had to be reloaded (no live graph).
    Reloaded,
    /// Pruned lanes are being built on the source worker.
    NeedsSources { tracks: Vec<usize> },
}

/// How one lane's instrument was built, kept so a lane that cannot rewind can
/// be rebuilt without resolving the project graph again.
pub(crate) struct LaneSpec {
    pub preset: Option<oc_dsl::ast::PresetDef>,
    pub params: Vec<(String, oc_dsl::ast::ParamValue)>,
    pub performance: bool,
    pub smart_disable: bool,
}

/// One pruned lane instantiated off the render thread, ready to splice in.
pub(crate) struct BuiltLane {
    pub target: usize,
    pub source: SmartSource,
    pub chain: oc_engine::graph::EffectChain,
    pub events: Vec<MidiEvent>,
    pub spec: LaneSpec,
}

/// A mixed chunk's meters, held until the device reaches that audio.
struct PendingMeters {
    /// Stream frame where the chunk starts; it is released one device block
    /// into the cursor, when the device plays it.
    due_frame: u64,
    frame: crate::meters::MeterFrame,
}

/// A request for the source worker to build lanes for hot-added tracks.
struct SourceBuildRequest {
    epoch: u64,
    worker_options: oc_engine::render_parallel::RenderWorkerOptions,
    project: Project,
    tracks: Vec<usize>,
    soundfont_path: PathBuf,
    sample_rate: u32,
    instrument_instantiations: Arc<AtomicU64>,
    /// Directory a hot-added lane's relative preset assets resolve against.
    root: Option<PathBuf>,
}

// ============================================================================
// Shared state (lock-free, between render thread and audio callback)
// ============================================================================

/// A session loop region in frames. Playback reads it; offline render never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    pub start: u64,
    pub end: u64,
}

impl LoopRegion {
    /// The frame playback jumps back to once `cursor` reaches the region end.
    ///
    /// `playback_end` bounds the region: audio past the project end does not
    /// exist, so a region that reaches past it wraps at the last frame instead.
    /// A region with nothing left to play (its start is at or past the project
    /// end) returns `None`, so playback stops normally.
    pub fn wrap_target(&self, cursor: u64, playback_end: u64) -> Option<u64> {
        let end = self.end.min(playback_end);
        (end > self.start && cursor >= end).then_some(self.start)
    }
}

/// A start/end frame pair that a reader never sees half-written.
///
/// The writer takes the version counter, stores both frames, and releases it.
/// A reader compares the counter across its two loads, so it can only return a
/// pair one writer stored. The callback reads this without a lock; a read that
/// keeps losing the race reports no pair, which costs one callback's picture
/// update and no audio. Only command and render threads write, never the
/// callback, so a writer's short spin is acceptable.
struct AtomicPair {
    /// Odd while a write is in flight.
    version: AtomicU64,
    start: AtomicU64,
    end: AtomicU64,
}

/// How many times a reader retries a pair that changed underneath it.
const PAIR_READ_ATTEMPTS: u32 = 4;

impl AtomicPair {
    fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }

    /// Publish `region` and return the pair it replaced.
    ///
    /// The replaced value is read before the write: with one writer it is exact,
    /// and with two it is the pair that was visible when this write began.
    fn store(&self, region: Option<LoopRegion>) -> Option<LoopRegion> {
        let previous = self.load();
        let mut version = self.version.load(Ordering::Acquire);
        loop {
            if version.is_multiple_of(2)
                && let Ok(claimed) = self.version.compare_exchange(
                    version,
                    version + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
            {
                version = claimed;
                break;
            }
            std::hint::spin_loop();
            version = self.version.load(Ordering::Acquire);
        }
        let (start, end) = region.map_or((0, 0), |region| (region.start, region.end));
        self.start.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Relaxed);
        self.version
            .store(version.wrapping_add(2), Ordering::Release);
        previous
    }

    /// The pair, or `None` when no region is set or the reader kept losing.
    fn load(&self) -> Option<LoopRegion> {
        for _ in 0..PAIR_READ_ATTEMPTS {
            let version = self.version.load(Ordering::Acquire);
            if version % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let start = self.start.load(Ordering::Acquire);
            let end = self.end.load(Ordering::Acquire);
            if self.version.load(Ordering::Acquire) == version {
                return (end != 0).then_some(LoopRegion { start, end });
            }
            std::hint::spin_loop();
        }
        None
    }
}

/// A loop pass that stops mid-waveform steps, so the last frames of a pass that
/// ends at the region boundary fade out. `bounded` marks a preview render whose
/// end is not the loop region.
fn fade_closed_loop_pass(
    chunk: &mut [f32],
    cursor: u64,
    playback_end: u64,
    region: Option<LoopRegion>,
    bounded: bool,
) {
    let Some(region) = region else {
        return;
    };
    if bounded || cursor < region.end.min(playback_end) {
        return;
    }
    crate::audio::fade_out_tail(chunk, chunk.len());
}

pub struct StreamingPlayback {
    /// Ring buffer consumer (audio callback reads from here).
    /// Wrapped in UnsafeCell because the audio callback needs &mut access
    /// while the struct is behind Arc (shared reference).
    /// SAFETY: Only the audio callback thread ever accesses this field.
    consumer: UnsafeCell<rtrb::Consumer<f32>>,
    /// True if the render thread is actively producing audio
    pub active: AtomicBool,
    /// Immediate consumer gate owned by command intent, never by queued worker Play.
    pub output_suspended: AtomicBool,
    /// Timeline position in frames (for UI transport display). The callback
    /// advances it only for timeline audio, never for the count-in.
    pub play_cursor_frames: AtomicU64,
    /// Latency the live master path reports (pre-master PDC + master chain).
    /// Written by the render thread after every mixed chunk.
    pub chain_latency_frames: Arc<AtomicU64>,
    /// Frames per device callback, as measured by the audio callback.
    pub device_buffer_frames: AtomicU64,
    /// Total duration in frames
    pub total_frames: AtomicU64,
    output_end_frames: AtomicU64,
    /// Set to true once the first chunk is available (play can start)
    pub ready: AtomicBool,
    pub waiting_for_clip: AtomicBool,
    /// Frames of audio still to fade in after a starve, so a producer hiccup is
    /// heard as a dip instead of a step into the waveform. The audio callback
    /// owns it; the atomic keeps the shared handle free of a second unsafe cell.
    pub resume_fade: AtomicU32,
    /// Set to true when ProjectChanged has been fully processed (renderers ready).
    /// Cleared when a new ProjectChanged is sent.
    pub project_loaded: AtomicBool,
    /// Warm-up lifecycle: `WARM_IDLE`, `WARM_BUILDING` or `WARM_READY`.
    pub warm_state: AtomicU8,
    /// Project loads performed by the render thread (diagnostics and tests).
    pub project_loads: AtomicU64,
    /// Incremented whenever a Play command has actually been applied.
    pub play_applied: AtomicU64,
    /// Play/Seek intent is published before enqueue; callbacks wait until all
    /// position requests have been applied, even if an older ring is active.
    pub position_requested: AtomicU64,
    pub position_applied: AtomicU64,
    /// Instrument instances constructed by the render thread (diagnostics and tests).
    pub instrument_instantiations: Arc<AtomicU64>,
    /// Chain documents the audio side has handed over. The audio side waits for
    /// the previous one to land, so a drag cannot queue a patch per input event.
    pub chain_requests: AtomicU64,
    /// Chain documents the render thread has applied.
    pub chain_applied: AtomicU64,
    /// Effect chains rebuilt or faded in place (diagnostics and tests).
    pub chain_patches: AtomicU64,
    /// Held notes across every lane after the last chunk (tests).
    #[cfg(test)]
    pub sounding_voices: Arc<AtomicU64>,
    /// Main/render-thread diagnostic only; the audio callback never locks this.
    load_error: Mutex<Option<String>>,
    /// Incremented on seek. Audio callback discards stale buffer data when this
    /// changes. Render thread writes this value after flushing and re-rendering.
    pub seek_generation: AtomicU64,
    /// The generation that the audio callback last saw. When seek_generation > this,
    /// the callback flushes the ring buffer before reading new data.
    pub callback_generation: AtomicU64,
    /// When true, audio callback outputs silence (used during cursor scrub).
    /// The render thread keeps running so resume is instant after unmute.
    pub muted: AtomicBool,
    /// Callbacks that wanted audio but found the ring short; diagnostics and tests.
    pub underruns: AtomicU64,
    /// Playable lanes in the render graph (one per source, not per project track).
    pub active_sources: AtomicU64,
    /// Count-in frames this run sounds before the timeline starts. It stays
    /// set for the run so the reported playhead can hold at the record start.
    pub count_in_frames: AtomicU64,
    /// Count-in frames still to be consumed from the ring; the callback drops
    /// them from the play cursor instead of counting them as timeline audio.
    pub count_in_remaining: AtomicU64,
    /// Timeline frame the count-in ends at; the playhead sits here until it does.
    pub count_in_start: AtomicU64,
    /// Device-consumed timeline frames for the current Play, across loop wraps.
    take_start_frame: AtomicU64,
    take_start_run: AtomicU64,
    take_device_run: AtomicU64,
    take_device_frames: AtomicU64,
    /// The output stream reported a backend error; the transport must stop
    /// claiming to play until a new stream is opened.
    pub stream_failed: AtomicBool,
    /// Reason of the last stream failure, taken by the transport snapshot.
    pub stream_error: Mutex<Option<String>>,
    /// Gain-reduction and peak meters published by the render thread.
    pub meters: Arc<MeterHub>,
    /// Live per-parameter touch overrides; offline renders never see this.
    pub touch: Arc<oc_engine::touch::TouchRegistry>,
    /// Session loop region in frames. The pair is one value: a reader never
    /// pairs one edit's start with another edit's end.
    loop_region: AtomicPair,
    loop_enabled: AtomicBool,
    /// Where an armed pass ends and the pass after it starts. The callback
    /// wraps the playhead here, because a wrap keeps the ring running instead
    /// of flushing it, so the picture has to follow the audio.
    loop_seam: AtomicPair,
    /// Bumped when the loop changes. The ring can hold the head of a wrapped
    /// pass, which belongs to the old transport state once the loop changes,
    /// so the render thread discards it and rebuilds from where the audio is.
    pub(crate) loop_generation: AtomicU64,
    /// Press-to-first-block trace for the current start. Atomics only, so the
    /// audio callback can stamp it without allocating or locking.
    pub first_block: Arc<FirstBlock>,
}

impl StreamingPlayback {
    /// Get the consumer pointer.
    /// SAFETY: Caller must guarantee single-threaded access (only from audio callback).
    pub unsafe fn consumer_ptr(&self) -> *mut rtrb::Consumer<f32> {
        self.consumer.get()
    }

    /// Frames the picture trails the ring by so it matches the sound: the
    /// device period plus the latency of the live master path.
    pub fn monitor_latency_frames(&self) -> u64 {
        monitor_latency_frames(
            self.device_buffer_frames.load(Ordering::Acquire),
            self.chain_latency_frames.load(Ordering::Acquire),
        )
    }

    /// Take the reason of a dead output stream, if one was reported.
    pub fn take_stream_failure(&self) -> Option<String> {
        if !self.stream_failed.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.stream_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// Timeline position: the count-in is not part of it, so the cursor never
    /// moves while the count-in sounds.
    pub fn timeline_frames(&self) -> u64 {
        self.play_cursor_frames.load(Ordering::Acquire)
    }

    /// The pass has played out: its cursor reached the frame the run ends at.
    ///
    /// The producer may park on an empty pass while the ring still drains, so
    /// this is the end-of-stream signal the audio callback reads: an empty ring
    /// after this frame is the end of the program, not a starve. Atomics only;
    /// safe from the audio callback.
    pub fn has_played_out(&self) -> bool {
        let total = self.total_frames.load(Ordering::Acquire);
        let end = self.output_end_frames.load(Ordering::Acquire).max(total);
        end > 0 && self.timeline_frames() >= end
    }

    /// Exact output boundary published by the current run, including its drain.
    pub(crate) fn run_output_end_frame(&self) -> u64 {
        self.output_end_frames.load(Ordering::Acquire)
    }

    /// Playback position of the audio currently leaving the device.
    ///
    /// A run with a count-in holds at its record start until the first timeline
    /// audio reaches the device, then trails the cursor by the monitor latency.
    /// The count-in sounds ahead of the timeline and must not move the picture.
    pub fn audible_frames(&self) -> u64 {
        let timeline = self.timeline_frames();
        let latency = self.monitor_latency_frames();
        if self.count_in_frames.load(Ordering::Acquire) == 0 {
            return timeline.saturating_sub(latency);
        }
        let start = self.count_in_start.load(Ordering::Acquire);
        if timeline < start {
            // A loop wrapped back before the record start: the count-in hold
            // belongs to the head of the run, not to this pass.
            return timeline.saturating_sub(latency);
        }
        start + (timeline - start).saturating_sub(latency)
    }

    fn publish_play_run(&self, start_frame: u64) {
        self.take_start_run.store(0, Ordering::SeqCst);
        self.take_start_frame.store(start_frame, Ordering::SeqCst);
        let run = self
            .play_applied
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.take_start_run.store(run, Ordering::SeqCst);
    }

    pub(crate) fn count_take_device_frames(&self, run: u64, frames: u64) {
        if run == 0 || frames == 0 {
            return;
        }
        if self.take_device_run.load(Ordering::Relaxed) != run {
            // The callback is the sole writer. Unpublish the old counter
            // before resetting it, then publish the new run and its first block.
            self.take_device_run.store(0, Ordering::SeqCst);
            self.take_device_frames.store(frames, Ordering::SeqCst);
            self.take_device_run.store(run, Ordering::SeqCst);
        } else {
            self.take_device_frames.fetch_add(frames, Ordering::SeqCst);
        }
    }

    pub(crate) fn unwrapped_take_frame(&self) -> u64 {
        let run = self.play_applied.load(Ordering::SeqCst);
        if run == 0
            || self.take_device_run.load(Ordering::SeqCst) != run
            || self.take_start_run.load(Ordering::SeqCst) != run
        {
            return 0;
        }
        let start = self.take_start_frame.load(Ordering::SeqCst);
        let frames = self.take_device_frames.load(Ordering::SeqCst);
        if self.take_device_run.load(Ordering::SeqCst) != run
            || self.take_start_run.load(Ordering::SeqCst) != run
            || self.play_applied.load(Ordering::SeqCst) != run
        {
            return 0;
        }
        start.saturating_add(frames.saturating_sub(self.monitor_latency_frames()))
    }

    /// Arm or clear the session loop region. `None` also disarms Loop, so a
    /// cleared region never leaves a toggle claiming it will wrap.
    pub fn set_loop_region(&self, region: Option<LoopRegion>) {
        let changed = self.loop_region.store(region) != region;
        if region.is_none() {
            self.loop_enabled.store(false, Ordering::Release);
        }
        // A new region has not wrapped yet, and the pass already queued for the
        // old one is no longer the audio the transport describes.
        let queued = self.publish_loop_seam(None);
        if changed || queued {
            self.invalidate_loop_pass();
        }
    }

    pub fn set_loop_enabled(&self, enabled: bool) {
        // A region is required; the control layer refuses the alternative.
        let armed = enabled && self.loop_region.load().is_some();
        let changed = self.loop_enabled.swap(armed, Ordering::Release) != armed;
        let mut queued = false;
        if !armed {
            queued = self.publish_loop_seam(None);
        }
        if changed || queued {
            self.invalidate_loop_pass();
        }
    }

    /// Ask the render thread to drop the queued pass and rebuild from the
    /// audible position. The callback flushes the ring on the new seek
    /// generation, so no audio from the previous transport state is heard.
    fn invalidate_loop_pass(&self) {
        self.loop_generation.fetch_add(1, Ordering::Release);
    }

    /// Publish the seam the callback wraps the playhead at. `None` clears it, so
    /// a disarmed loop never moves the picture. Returns whether a seam was
    /// already published, because dropping one invalidates queued audio.
    fn publish_loop_seam(&self, seam: Option<LoopRegion>) -> bool {
        self.loop_seam.store(seam).is_some()
    }

    /// The seam playback last wrapped at, or `None` while it has none to wrap.
    pub fn loop_seam(&self) -> Option<LoopRegion> {
        self.loop_seam.load()
    }

    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Acquire)
    }

    pub fn loop_region(&self) -> Option<LoopRegion> {
        self.loop_region.load()
    }

    /// The region playback actually wraps on: a drawn but disarmed region never
    /// ends a pass, so it never fades one.
    pub fn armed_loop_region(&self) -> Option<LoopRegion> {
        self.loop_enabled().then(|| self.loop_region()).flatten()
    }

    /// Where playback must jump back to, or `None` when it keeps playing.
    pub fn loop_wrap(&self, cursor: u64, playback_end: u64) -> Option<u64> {
        if !self.loop_enabled() {
            return None;
        }
        self.loop_region()?.wrap_target(cursor, playback_end)
    }
}

// SAFETY: StreamingPlayback is shared via Arc between the main thread and the
// audio callback thread. The `consumer` (UnsafeCell) field is only ever accessed
// from the audio callback thread (single reader guarantee required by rtrb).
// The atomic fields are inherently Sync.
unsafe impl Sync for StreamingPlayback {}
// SAFETY: ownership transfer preserves the sole callback consumer; all shared
// control fields are atomic, and the ring producer has a separate endpoint.
unsafe impl Send for StreamingPlayback {}

// ============================================================================
// Render thread handle (held by AudioState)
// ============================================================================

pub struct RenderThreadHandle {
    pub cmd_tx: Sender<RenderCommand>,
    pub playback: Arc<StreamingPlayback>,
    thread: Option<JoinHandle<()>>,
    /// Test-only delay priced into every source-worker build of this handle.
    #[cfg(test)]
    pub test_build_delay_ms: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    /// Producer side (only used to create the ring buffer; not accessed after spawn)
    _producer_moved: bool,
}

/// The shared player state, built once per render thread and by callback tests.
fn streaming_playback(consumer: rtrb::Consumer<f32>) -> StreamingPlayback {
    StreamingPlayback {
        consumer: UnsafeCell::new(consumer),
        active: AtomicBool::new(false),
        output_suspended: AtomicBool::new(false),
        play_cursor_frames: AtomicU64::new(0),
        chain_latency_frames: Arc::new(AtomicU64::new(0)),
        device_buffer_frames: AtomicU64::new(0),
        total_frames: AtomicU64::new(0),
        output_end_frames: AtomicU64::new(0),
        ready: AtomicBool::new(false),
        waiting_for_clip: AtomicBool::new(false),
        resume_fade: AtomicU32::new(0),
        project_loaded: AtomicBool::new(false),
        warm_state: AtomicU8::new(WARM_IDLE),
        project_loads: AtomicU64::new(0),
        play_applied: AtomicU64::new(0),
        position_requested: AtomicU64::new(0),
        position_applied: AtomicU64::new(0),
        instrument_instantiations: Arc::new(AtomicU64::new(0)),
        chain_requests: AtomicU64::new(0),
        chain_applied: AtomicU64::new(0),
        chain_patches: AtomicU64::new(0),
        #[cfg(test)]
        sounding_voices: Arc::new(AtomicU64::new(0)),
        load_error: Mutex::new(None),
        seek_generation: AtomicU64::new(0),
        callback_generation: AtomicU64::new(0),
        muted: AtomicBool::new(false),
        underruns: AtomicU64::new(0),
        active_sources: AtomicU64::new(0),
        count_in_frames: AtomicU64::new(0),
        count_in_remaining: AtomicU64::new(0),
        count_in_start: AtomicU64::new(0),
        take_start_frame: AtomicU64::new(0),
        take_start_run: AtomicU64::new(0),
        take_device_run: AtomicU64::new(0),
        take_device_frames: AtomicU64::new(0),
        stream_failed: AtomicBool::new(false),
        stream_error: Mutex::new(None),
        meters: Arc::new(MeterHub::new()),
        touch: Arc::new(oc_engine::touch::TouchRegistry::new()),
        loop_region: AtomicPair::new(),
        loop_enabled: AtomicBool::new(false),
        loop_seam: AtomicPair::new(),
        loop_generation: AtomicU64::new(0),
        first_block: Arc::new(FirstBlock::new()),
    }
}

impl RenderThreadHandle {
    /// Spawn the background render thread.
    pub fn spawn(sample_rate: u32) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (build_tx, build_rx) = std::sync::mpsc::channel();

        // Create ring buffer (stereo interleaved, so capacity * 2 floats)
        let (producer, consumer) = rtrb::RingBuffer::new(RING_BUFFER_FRAMES * 2);

        let playback = Arc::new(streaming_playback(consumer));

        let playback_clone = Arc::clone(&playback);
        let render_build_tx = build_tx.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        #[cfg(test)]
        let test_build_delay_ms = Arc::new(AtomicU64::new(0));
        let builder = SourceBuilder {
            requests: build_rx,
            cmd_tx: cmd_tx.clone(),
            shutdown: Arc::clone(&shutdown),
            #[cfg(test)]
            delay_ms: Arc::clone(&test_build_delay_ms),
        };
        std::thread::Builder::new()
            .name("ocp-source-build".into())
            .spawn(move || builder.run())
            .expect("Failed to spawn source build thread");
        let thread = std::thread::Builder::new()
            .name("ocp-render".into())
            .spawn(move || {
                render_loop(
                    cmd_rx,
                    producer,
                    playback_clone,
                    sample_rate,
                    worker_shutdown,
                    render_build_tx,
                );
            })
            .expect("Failed to spawn render thread");

        Self {
            cmd_tx,
            playback,
            thread: Some(thread),
            #[cfg(test)]
            test_build_delay_ms,
            shutdown,
            _producer_moved: true,
        }
    }

    /// Send a command to the render thread.
    pub fn send(&self, cmd: RenderCommand) {
        // Retire at request time: a queued Stop must not clear a newer touch
        // admitted while the worker is still handling an older command.
        match &cmd {
            RenderCommand::Stop | RenderCommand::Shutdown => self.playback.touch.clear(),
            RenderCommand::Pause => self.playback.touch.release_all_owned(),
            _ => {}
        }
        if matches!(
            &cmd,
            RenderCommand::Play { .. } | RenderCommand::Seek { .. }
        ) {
            self.playback
                .position_requested
                .fetch_add(1, Ordering::AcqRel);
        }
        match &cmd {
            RenderCommand::Pause | RenderCommand::Stop | RenderCommand::Shutdown => {
                self.playback
                    .output_suspended
                    .store(true, Ordering::Release);
            }
            RenderCommand::Play { .. } => {
                self.playback
                    .output_suspended
                    .store(false, Ordering::Release);
            }
            _ => {}
        }
        if matches!(&cmd, RenderCommand::ProjectChanged { .. }) {
            self.playback.project_loaded.store(false, Ordering::Release);
            *self.playback.load_error.lock().unwrap() = None;
        }
        let _ = self.cmd_tx.send(cmd);
    }

    /// Wait until the render thread has produced at least one chunk (test helper).
    /// Returns false if timeout or thread died.
    #[cfg(test)]
    pub fn wait_ready(&self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        while !self.playback.ready.load(Ordering::Acquire) {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    /// Wait until the render thread has finished processing ProjectChanged.
    /// Returns the load failure rather than misreporting it as a timeout.
    #[cfg(test)]
    pub fn wait_project_loaded(&self, timeout: std::time::Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        while !self.playback.project_loaded.load(Ordering::Acquire) {
            if let Some(error) = self.load_error() {
                return Err(error);
            }
            if start.elapsed() > timeout {
                return Err("Render thread timed out loading project".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    }

    /// Reason the last project load failed, if it did.
    pub fn load_error(&self) -> Option<String> {
        self.playback.load_error.lock().unwrap().clone()
    }

    /// Live touch state shared with the GUI commands.
    pub fn touch(&self) -> Arc<oc_engine::touch::TouchRegistry> {
        Arc::clone(&self.playback.touch)
    }
}

impl RenderThreadHandle {
    /// Exit owns the main loop and can drain pending native control requests
    /// while waiting. Project replacement keeps the nonblocking Drop behavior.
    pub(crate) fn shutdown_for_exit(
        mut self,
        executor: &oc_host::clap_main::ClapMainExecutor,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        self.shutdown.store(true, Ordering::Release);
        self.playback.active.store(false, Ordering::Release);
        let _ = self.cmd_tx.send(RenderCommand::Shutdown);
        let Some(worker) = self.thread.take() else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        while !worker.is_finished() {
            executor.poll()?;
            if started.elapsed() >= timeout {
                // A stuck foreign call cannot be interrupted safely in-process.
                // Keep its ownership on the worker until process termination.
                drop(worker);
                return Err("A plugin did not finish during shutdown; its native ownership remains retained".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        worker
            .join()
            .map_err(|_| "The render worker failed during shutdown".to_owned())?;
        executor.poll()
    }
}

impl Drop for RenderThreadHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.playback.active.store(false, Ordering::Release);
        let _ = self.cmd_tx.send(RenderCommand::Shutdown);
        // Project replacement runs on the UI thread. The worker owns its source
        // instances and ring producer, so let it finish any in-flight plugin call
        // and destroy them itself. Joining here freezes the UI during slow loads.
        // A stuck third-party call cannot be forcibly cancelled safely in-process.
        drop(self.thread.take());
    }
}

// ============================================================================
// The render loop (runs on background thread)
// ============================================================================

/// Each pattern with an explicit preset owns a source lane. Audio is summed into
/// its original track before that track's routing/effects, never another synth.
pub(crate) fn split_source_lanes(project: &Project) -> Result<(Project, Vec<usize>), String> {
    let plan = oc_engine::source_lanes::plan(project)?;
    Ok((plan.project.into_owned(), plan.targets))
}

/// Note-bearing sources keep their instrument; a clip-only track does not need one.
fn is_musical_event(event: &MidiMessage) -> bool {
    matches!(
        event,
        MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. } | MidiMessage::NativeVoice(_)
    )
}

/// True when a lane's audio comes only from clip lanes on its track.
fn clip_only_lane(project: &Project, lane: &oc_dsl::ast::Track, has_musical_events: bool) -> bool {
    !has_musical_events
        && lane.preset.is_none()
        && project
            .audio_clips()
            .iter()
            .any(|clip| clip.track == lane.name)
}

/// Everything that decides how a lane's instrument and FX chain are built.
///
/// Notes, clip placement and automation are deliberately absent: those are
/// re-interpreted on every load, and rebuilding a plugin instance for them is
/// what made a note edit wait on every instrument in the project.
#[derive(serde::Serialize)]
struct SourceSignature<'a> {
    presets: &'a [PresetDef],
    soundfont: &'a Path,
    /// Tempo and meter decide the sample position of every scheduled
    /// performance command, which a lane captures when it is built.
    properties: &'a oc_dsl::ast::ProjectProperties,
    tracks: Vec<TrackSignature<'a>>,
}

#[derive(serde::Serialize)]
struct TrackSignature<'a> {
    instrument: &'a InstrumentPath,
    preset: &'a Option<String>,
    engine: &'a Option<String>,
    engine_params: &'a Option<Vec<(String, ParamValue)>>,
    effects: &'a [Effect],
    performance: Option<&'a oc_dsl::performance::PerformanceTimeline>,
    smart_disable: bool,
    /// Whether the track carries any authored note material. A lane with none
    /// and an audio clip is built silent, so gaining notes must rebuild it.
    has_notes: bool,
    /// Hairpin ramps are installed on the source when it is built.
    gain_automation: &'a [oc_dsl::ast::GainAutomation],
}

fn source_signature(project: &Project, soundfont_path: &Path) -> u64 {
    let signature = SourceSignature {
        presets: &project.presets,
        soundfont: soundfont_path,
        properties: &project.properties,
        tracks: project
            .tracks
            .iter()
            .map(|track| TrackSignature {
                instrument: &track.instrument,
                preset: &track.preset,
                engine: &track.engine,
                engine_params: &track.engine_params,
                effects: &track.effects,
                performance: track.performance.as_ref(),
                smart_disable: track.mix.smart_disable,
                has_notes: !track.phrases.is_empty()
                    || !track.patterns.is_empty()
                    || !track.inline_patterns.is_empty()
                    || track.pattern_clip_count() > 0,
                gain_automation: &track.gain_automation,
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&signature).unwrap_or_default();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct RenderState {
    project: Option<Project>,
    worker_options: oc_engine::render_parallel::RenderWorkerOptions,
    /// Directory the loaded project's relative media paths resolve against.
    root: Option<PathBuf>,
    /// Preroll destination; discarded chunks reconstruct voices and DSP history.
    seek_target: Option<u64>,
    /// Wall-clock deadline for an exact seek preroll. Only set for a seek that
    /// arrives while the transport is playing.
    preroll_deadline: Option<std::time::Instant>,
    /// Warm-up staging: pre-render into `rendered_ahead` while playback is idle.
    warm_fill: bool,
    /// Independent pattern/track instrument instances; the SoundFont player is one adapter.
    renderers: Vec<SmartSource>,
    /// The instrument spec each lane was built from, one per renderer.
    source_specs: Vec<LaneSpec>,
    /// Audio clips preloaded at project load; chunk mixing copies from here.
    clip_lanes: ClipLanes,
    /// Media version each loaded clip is pinned to, for reload logging.
    clip_media: Vec<(String, MediaKey)>,
    source_tracks: Vec<usize>,
    /// Preallocated main/aux buffers per source lane, planned at load.
    source_outputs: Vec<SourceOutput>,
    track_chunks: Vec<Vec<f32>>,
    lane_audio: Vec<f32>,
    dsp_block: Vec<f32>,
    preroll_buffer: Vec<f32>,
    free_meter_frames: Vec<crate::meters::MeterFrame>,
    /// Per-chunk auxiliary returns keyed by (hosting track, stereo pair).
    aux_buffers: HashMap<(usize, usize), Vec<f32>>,
    /// Per-lane, per-pair PDC delay for auxiliary returns; sized at load.
    aux_delays: Vec<Vec<VecDeque<f32>>>,
    /// Auxiliary sends already reported as unsupported, so each warns once.
    warned_aux: HashSet<(usize, usize)>,
    /// Tracks already reported to route an output port the source lacks.
    warned_ports: HashSet<usize>,
    /// Signature of everything an instrument or lane is built from. A project
    /// change that leaves it alone reuses the built sources instead of
    /// instantiating every plugin again.
    source_signature: u64,
    source_chains: Vec<oc_engine::graph::EffectChain>,
    source_delays: Vec<std::collections::VecDeque<f32>>,
    /// All MIDI events sorted by time
    events: Vec<MidiEvent>,
    /// Events split by track index
    events_by_track: Vec<Vec<usize>>, // indices into `events`
    /// Per-track event cursor (how far we've sent events)
    track_event_cursors: Vec<usize>,
    /// Notes each lane is holding, one bit per (channel, note) pair. A seek
    /// that skips audio releases these: the skipped range never names a note
    /// that started before the cursor, and channel state cannot stop it.
    sounding: Vec<u64>,
    performance_events: Vec<Vec<oc_synth::performance::ScheduledCommand>>,
    performance_cursors: Vec<usize>,
    /// Audio graph for mixing (owns effect chains)
    graph: Option<AudioGraph>,
    transport: Option<PluginTransport>,
    /// Current render position in frames
    render_cursor: u64,
    /// DSP always advances in unchanged blocks, even when the consumer seeks
    /// inside one. Retain its unconsumed suffix instead of repartitioning DSP.
    dsp_cursor: u64,
    rendered_ahead: std::collections::VecDeque<f32>,
    /// Total project duration in frames
    total_frames: u64,
    drain_frames: u64,
    /// Playback end for the current run; a preview stops at its content bound.
    play_end_frame: Option<u64>,
    /// Is rendering active
    playing: bool,
    paused: bool,
    /// True while the warm-up has published audio into the ring for the current
    /// seek generation, so a press can start the consumer instead of rendering.
    ring_prefilled: bool,
    /// Frames of the loop seam's fade-in still to apply, or 0. Set by the wrap,
    /// spent on the first chunk the pass after it renders.
    loop_seam_fade: u32,
    /// Actual origin of stopped warm audio, independent of the UI cursor.
    warm_ring_start_frame: u64,
    /// Sample rate
    sample_rate: u32,
    /// Number of tracks
    num_tracks: usize,
    /// Soundfont path (for creating renderers)
    soundfont_path: PathBuf,
    render_error: Option<String>,
    processing_fault: bool,
    waiting_for_clip: bool,
    /// Meters shared with the UI; None in offline-only test states.
    meter_hub: Option<Arc<MeterHub>>,
    /// Reused per-chunk input peaks so publishing does not allocate.
    meter_input_peaks: Vec<f32>,
    /// Instrument-construction counter; shared with the playback handle.
    instrument_instantiations: Arc<AtomicU64>,
    /// Live master-path latency; shared with the playback handle.
    chain_latency_frames: Arc<AtomicU64>,
    /// Held notes across every lane, published after each chunk (tests).
    #[cfg(test)]
    sounding_voices: Arc<AtomicU64>,
    /// Beat clock for the click; rebuilt whenever the project is loaded.
    clock: Option<ProjectClock>,
    click: ClickTrack,
    click_events: Vec<ClickEvent>,
    completed_count_in: Option<CountInState>,
    click_settings: ClickSettings,
    /// Count-in audio still to be emitted before the timeline starts.
    count_in: Option<CountInState>,
    /// Count-in armed for the next fresh Play. A running transport can render
    /// the count-in before the Play is applied, so the Play restarts it from
    /// this plan instead of trusting what `count_in` already emitted.
    armed_count_in: Option<CountInPlan>,
    /// Meter frames rendered ahead of the device, with the cursor frame that
    /// makes them audible.
    pending_meters: std::collections::VecDeque<PendingMeters>,
    /// Analyser node requested by the UI; survives project reloads.
    spectrum_target: Option<oc_engine::spectrum::SpectrumTarget>,
    /// Tracks handed to the source worker and not spliced in yet.
    pending_sources: Vec<usize>,
    /// Bumped on every project load; stale worker results are discarded.
    source_epoch: u64,
    /// Live touch overrides; `None` in offline-only states and offline renders.
    touch: Option<Arc<oc_engine::touch::TouchRegistry>>,
    /// Fold the monitored master to mono; read from the meter hub each loop.
    mono_check: bool,
    /// Short-term loudness of the monitored master.
    loudness: oc_engine::analysis::ShortTermLoudness,
    /// Left/right correlation of the monitored master, before any mono fold.
    correlation: Correlation,
}

impl RenderState {
    fn new(sample_rate: u32) -> Self {
        Self {
            project: None,
            worker_options: oc_engine::render_parallel::RenderWorkerOptions::machine(),
            root: None,
            seek_target: None,
            preroll_deadline: None,
            warm_fill: false,
            renderers: Vec::new(),
            source_specs: Vec::new(),
            clip_lanes: ClipLanes::default(),
            clip_media: Vec::new(),
            source_tracks: Vec::new(),
            source_outputs: Vec::new(),
            track_chunks: Vec::new(),
            lane_audio: Vec::with_capacity(CHUNK_FRAMES * 2),
            dsp_block: Vec::with_capacity(CHUNK_FRAMES * 2),
            preroll_buffer: Vec::with_capacity(CHUNK_FRAMES * 2),
            free_meter_frames: Vec::with_capacity(MAX_PENDING_METERS),
            aux_buffers: HashMap::new(),
            aux_delays: Vec::new(),
            warned_aux: HashSet::new(),
            warned_ports: HashSet::new(),
            source_signature: 0,
            source_chains: Vec::new(),
            source_delays: Vec::new(),
            events: Vec::new(),
            events_by_track: Vec::new(),
            track_event_cursors: Vec::new(),
            sounding: Vec::new(),
            performance_events: Vec::new(),
            performance_cursors: Vec::new(),
            graph: None,
            transport: None,
            render_cursor: 0,
            dsp_cursor: 0,
            rendered_ahead: std::collections::VecDeque::with_capacity(
                (LOOKAHEAD_FRAMES + CHUNK_FRAMES) * 2,
            ),
            total_frames: 0,
            drain_frames: 0,
            play_end_frame: None,
            playing: false,
            paused: false,
            ring_prefilled: false,
            loop_seam_fade: 0,
            warm_ring_start_frame: 0,
            sample_rate,
            num_tracks: 0,
            soundfont_path: PathBuf::new(),
            render_error: None,
            processing_fault: false,
            waiting_for_clip: false,
            meter_hub: None,
            meter_input_peaks: Vec::new(),
            instrument_instantiations: Arc::new(AtomicU64::new(0)),
            chain_latency_frames: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            sounding_voices: Arc::new(AtomicU64::new(0)),
            clock: None,
            click: {
                let mut click = ClickTrack::new();
                click
                    .prepare(CHUNK_FRAMES * 2 + (sample_rate as usize * 3 / 100) * 2)
                    .expect("bounded click capacity");
                click
            },
            click_events: Vec::with_capacity(CHUNK_FRAMES * 2),
            completed_count_in: None,
            click_settings: ClickSettings::default(),
            count_in: None,
            armed_count_in: None,
            pending_meters: std::collections::VecDeque::with_capacity(MAX_PENDING_METERS),
            spectrum_target: None,
            pending_sources: Vec::new(),
            source_epoch: 0,
            touch: None,
            mono_check: false,
            loudness: oc_engine::analysis::ShortTermLoudness::new(sample_rate),
            correlation: Correlation::new(sample_rate),
        }
    }

    /// End of the current playback run: a preview bound, else the whole project.
    fn timeline_end(&self) -> u64 {
        self.play_end_frame.unwrap_or(self.total_frames)
    }

    fn playback_end(&self) -> u64 {
        self.play_end_frame
            .unwrap_or_else(|| self.total_frames.saturating_add(self.drain_frames))
    }

    fn update_drain_frames(&mut self) {
        let Some(graph) = self.graph.as_ref() else {
            self.drain_frames = 0;
            return;
        };
        let mut source_tails = vec![0; self.num_tracks];
        for (&track, chain) in self.source_tracks.iter().zip(&self.source_chains) {
            source_tails[track] = source_tails[track].max(chain.tail_samples());
        }
        let effect_tail = u64::from(graph.max_effect_tail_with_sources(&source_tails)).min(
            u64::from(self.sample_rate)
                * u64::from(oc_engine::render_cache::MAX_DECLARED_PLUGIN_TAIL_SECONDS),
        );
        self.drain_frames = u64::from(graph.pre_master_latency_samples())
            + u64::from(graph.master_chain.latency_samples())
            + effect_tail;
    }

    fn publish_ends(&self, playback: &StreamingPlayback) {
        playback
            .output_end_frames
            .store(self.playback_end(), Ordering::Release);
        playback
            .total_frames
            .store(self.timeline_end(), Ordering::Release);
    }

    /// Load a project, resolving its relative media paths against `root`.
    ///
    /// The directory travels with the load so the first load after opening a
    /// file never depends on the panel having stored the open path yet.
    fn load_project_from(
        &mut self,
        project: &Project,
        soundfont_path: PathBuf,
        root: Option<PathBuf>,
        cursor: u64,
    ) -> Result<(), String> {
        self.root = root;
        self.replace_project(project, soundfont_path, Some(cursor))
    }

    fn load_project(&mut self, project: &Project, soundfont_path: PathBuf) -> Result<(), String> {
        self.replace_project(project, soundfont_path, None)
    }

    /// Rebuild internal state from a new project.
    fn replace_project(
        &mut self,
        project: &Project,
        soundfont_path: PathBuf,
        progressive_cursor: Option<u64>,
    ) -> Result<(), String> {
        // Build a replacement completely before publishing it. No partial track
        // set or bypassed effect chain may be reported as a loaded project.
        let mut replacement = Self::new(self.sample_rate);
        // A change that leaves every instrument, preset and lane chain alone
        // (a note edit, a clip move, automation) reuses the built sources: the
        // project is re-interpreted, but no plugin is instantiated again.
        let signature = source_signature(project, &soundfont_path);
        let reuse_sources = signature == self.source_signature
            && self.renderers.len() == project.tracks.len()
            && !self.renderers.is_empty();
        if reuse_sources {
            replacement.renderers = std::mem::take(&mut self.renderers);
            replacement.source_chains = std::mem::take(&mut self.source_chains);
            replacement.source_specs = std::mem::take(&mut self.source_specs);
            replacement.source_signature = self.source_signature;
        }
        replacement.source_epoch = self.source_epoch.wrapping_add(1);
        replacement.root.clone_from(&self.root);
        replacement.meter_hub = self.meter_hub.take();
        replacement.meter_input_peaks = std::mem::take(&mut self.meter_input_peaks);
        replacement.instrument_instantiations = Arc::clone(&self.instrument_instantiations);
        replacement.chain_latency_frames = Arc::clone(&self.chain_latency_frames);
        #[cfg(test)]
        {
            replacement.sounding_voices = Arc::clone(&self.sounding_voices);
        }
        replacement.touch.clone_from(&self.touch);
        replacement.click_settings = self.click_settings;
        replacement.clock = ProjectClock::new(&project.properties).ok();
        replacement.mono_check = self.mono_check;
        replacement
            .spectrum_target
            .clone_from(&self.spectrum_target);
        if let Err(error) = replacement.build_project(project, soundfont_path, progressive_cursor) {
            self.meter_hub = replacement.meter_hub.take();
            self.meter_input_peaks = std::mem::take(&mut replacement.meter_input_peaks);
            if reuse_sources {
                self.renderers = std::mem::take(&mut replacement.renderers);
                self.source_chains = std::mem::take(&mut replacement.source_chains);
                self.source_specs = std::mem::take(&mut replacement.source_specs);
            }
            return Err(error);
        }
        replacement.project = Some(project.clone());
        // Streamed clips stay pinned to the decode they were loaded with; this
        // line is the only place a changed source file becomes visible.
        for (name, media) in &replacement.clip_media {
            if self
                .clip_media
                .iter()
                .any(|(previous_name, previous)| previous_name == name && previous != media)
            {
                log::info!(
                    "[render_thread] audio clip '{name}' media changed; reloading its decode"
                );
            }
        }
        *self = replacement;
        Ok(())
    }

    fn build_project(
        &mut self,
        project: &Project,
        soundfont_path: PathBuf,
        progressive_cursor: Option<u64>,
    ) -> Result<(), String> {
        // A full rebuild instantiates every instrument, so name the work as it
        // runs: the panel has nothing else to show for a multi-second load.
        // The scope publishes progress only while this build owns the report.
        let progress = crate::build_progress::scope(project.tracks.len());
        // A reused lane set is still the signature this build describes.
        self.source_signature = source_signature(project, &soundfont_path);
        #[cfg(test)]
        let phase = std::time::Instant::now();
        let worker_options = crate::gui_settings::load().worker_options();
        self.worker_options = worker_options;
        let resolved = oc_dsl::preset::resolve(project)?;
        let project = &resolved;
        // A pattern's preset overrides its host track's preset for its clips.
        // Refuse a reference the project does not declare instead of streaming
        // the track's instrument as a silent fallback.
        oc_engine::source_lanes::validate(project)?;
        #[cfg(test)]
        crate::profile_phase("build_resolve", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();
        if let Some(error) = oc_host::plugin_preset_render_error(project) {
            return Err(error);
        }
        #[cfg(test)]
        crate::profile_phase("build_plugin_check", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();
        if self.sample_rate == 0 {
            return Err("Streaming sample rate must be positive".into());
        }
        let clock = Arc::new(ProjectClock::new(&project.properties).map_err(|e| e.to_string())?);
        self.transport = Some(
            PluginTransport::new(0, self.sample_rate, project.properties.tempo).with_clock(clock),
        );
        self.soundfont_path = soundfont_path;
        self.num_tracks = project.tracks.len();

        // Interpret project → MIDI events
        let settings = InterpretSettings {
            sample_rate: self.sample_rate,
            ..Default::default()
        };
        let (mut source_project, mut source_tracks) = split_source_lanes(project)?;
        progress.stage(crate::build_progress::Stage::Arrangement);
        // Preserve the original duration even if muted, unreferenced sources are
        // omitted. Sidechain taps still need their source audio despite mute.
        if project
            .tracks
            .iter()
            .any(|track| track.performance.is_some())
        {
            oc_dsl::validate(project)
                .map_err(|errors| format!("Invalid performance project: {errors:?}"))?;
        }
        let full_performance_end = source_project
            .tracks
            .iter()
            .filter_map(|track| track.performance.as_ref())
            .map(|timeline| {
                oc_synth::performance::schedule(
                    timeline,
                    &ProjectClock::new(&project.properties)?,
                    self.sample_rate,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .flatten()
            .map(|event| event.sample)
            .max()
            .unwrap_or(0);
        let keys: std::collections::HashSet<&str> = project
            .tracks
            .iter()
            .flat_map(|track| track.effects.iter())
            .chain(project.buses.iter().flat_map(|bus| bus.effects.iter()))
            .chain(
                project
                    .master
                    .iter()
                    .flat_map(|master| master.effects.iter()),
            )
            .filter_map(|effect| effect.sidechain_source.as_deref())
            .collect();
        // Bus sidechains can depend on arbitrary upstream tracks: retain the
        // complete graph in that case rather than guess its dependency closure.
        let has_bus_key = keys
            .iter()
            .any(|key| !project.tracks.iter().any(|track| track.name == *key));
        let keep_source = |target: usize| {
            !project.tracks[target].mix.mute || keys.contains(project.tracks[target].name.as_str())
        };
        let mut pruned_score_end = 0;
        if !has_bus_key && source_tracks.iter().any(|&target| !keep_source(target)) {
            // Only a pruned project needs a second score to preserve the end
            // of muted lanes. Interpret the complete score before reindexing.
            let mut score_project = source_project.clone();
            for track in &mut score_project.tracks {
                track.performance = None;
            }
            pruned_score_end = interpret::try_interpret(&score_project, &settings)?
                .last()
                .map_or(0, |event| event.time_samples);
            let retained: Vec<_> = source_project
                .tracks
                .into_iter()
                .zip(source_tracks)
                .filter(|(_, target)| keep_source(*target))
                .collect();
            (source_project.tracks, source_tracks) = retained.into_iter().unzip();
        }
        // Match offline source rendering: graph faders/pan/mutes must not also
        // become sampler CC7/CC10 or suppress source notes before graph mixing.
        for track in &mut source_project.tracks {
            track.mix.volume_db = 0.0;
            track.mix.pan = 0.0;
            track.mix.mute = false;
        }
        self.source_tracks = source_tracks;
        let mut score_project = source_project.clone();
        self.performance_events = Vec::with_capacity(source_project.tracks.len());
        for (lane, track) in source_project.tracks.iter().enumerate() {
            let commands = if let Some(timeline) = &track.performance {
                let delegated = oc_engine::performance_send::project_plan(
                    project,
                    self.source_tracks[lane],
                    self.sample_rate,
                )?;
                oc_synth::performance::schedule(
                    timeline,
                    &ProjectClock::new(&project.properties)?,
                    self.sample_rate,
                )?
                .into_iter()
                .filter(|command| {
                    !matches!(command.message,
                        oc_dsl::performance::ChannelCommand::ControlChange { controller, .. }
                        if delegated.iter().any(|(cc, _, _)| *cc == controller))
                })
                .collect()
            } else {
                Vec::new()
            };
            self.performance_events.push(commands);
            score_project.tracks[lane].performance = None;
        }
        self.performance_cursors = vec![0; source_project.tracks.len()];
        self.events = interpret::try_interpret(&score_project, &settings)?;
        self.events.retain(|event| {
            source_project.tracks[event.track_index]
                .performance
                .is_none()
        });
        // Calculate total duration
        let last_event = self
            .events
            .last()
            .map_or(0, |event| event.time_samples)
            .max(pruned_score_end)
            .max(full_performance_end);
        let clip_end = oc_engine::audio_clip::project_end_frame(project, self.sample_rate)?;
        let tail = self.sample_rate as u64 * 2;
        let section_frames = (crate::project_extent::section_seconds(project)?
            * f64::from(self.sample_rate))
        .ceil() as u64;
        self.total_frames = (last_event.max(clip_end) + tail)
            .max(self.sample_rate as u64)
            .max(section_frames);

        if let Some(cursor) = progressive_cursor {
            self.clip_lanes = ClipLanes::load_progressively(
                project,
                self.sample_rate,
                self.root.as_deref(),
                cursor,
            )?;
            self.clip_media = self.clip_lanes.identities();
        }

        // Split events by track (store indices for efficiency)
        self.events_by_track = vec![Vec::new(); source_project.tracks.len()];
        for (idx, event) in self.events.iter().enumerate() {
            if event.track_index < source_project.tracks.len() {
                self.events_by_track[event.track_index].push(idx);
            }
        }

        // Reset cursors
        self.track_event_cursors = vec![0; source_project.tracks.len()];
        #[cfg(test)]
        crate::profile_phase("build_split_interpret", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();

        // Recreate for each project version: changed SoundFonts and residual
        // voices/controllers must not leak across edits with equal track counts.
        if self.renderers.len() != source_project.tracks.len() {
            // A reused lane set that no longer matches the pruned track list is
            // rebuilt from scratch: its chains and specs must go with it.
            self.renderers.clear();
            self.source_chains.clear();
            self.source_specs.clear();
            let lane_presets: Vec<_> = source_project
                .tracks
                .iter()
                .map(|track| {
                    project
                        .presets
                        .iter()
                        .find(|p| Some(&p.name) == track.preset.as_ref())
                })
                .collect();
            let calibration_presets: Vec<_> = source_project
                .tracks
                .iter()
                .zip(&lane_presets)
                .filter_map(|(track, preset)| {
                    let pinned = track
                        .engine_params
                        .iter()
                        .flatten()
                        .any(|(key, _)| key == oc_engine::loudness::TRIM_PARAM);
                    if pinned { None } else { *preset }
                })
                .collect();
            std::thread::scope(|scope| -> Result<(), String> {
                scope.spawn(move || {
                    oc_engine::loudness::prepare_parallel_with_options(
                        calibration_presets,
                        worker_options,
                    )
                });
                for (lane_index, (track, preset)) in
                    source_project.tracks.iter().zip(lane_presets).enumerate()
                {
                    progress.lane(lane_index, &track.name);
                    #[cfg(test)]
                    let source_start = std::time::Instant::now();
                    let params = oc_engine::preset::builtin_track_params_at(
                        project,
                        preset,
                        track,
                        self.root.as_deref(),
                    );
                    let mut chain =
                        oc_engine::source_lanes::effect_chain(project, track, self.sample_rate)?;
                    // Lane effects run on the streaming path, so their hosted
                    // plugins participate in the idle gate.
                    chain.set_streaming_idle_gate(true);
                    self.source_chains.push(chain);
                    let has_musical = track.performance.is_some()
                        || self.events_by_track[lane_index]
                            .iter()
                            .any(|&index| is_musical_event(&self.events[index].event));
                    let mut source = if clip_only_lane(project, track, has_musical) {
                        BlockSource::Silent
                    } else {
                        self.instrument_instantiations
                            .fetch_add(1, Ordering::Relaxed);
                        if track.performance.is_some() {
                            BlockSource::new_performance(
                                preset,
                                &params,
                                &self.soundfont_path,
                                self.sample_rate,
                                project.properties.tempo,
                                &self.performance_events[lane_index],
                                self.root.as_deref(),
                            )
                        } else {
                            let events = self.events_by_track[lane_index]
                                .iter()
                                .map(|&index| &self.events[index].event)
                                .collect::<Vec<_>>();
                            BlockSource::new_streaming(
                                preset,
                                &params,
                                &self.soundfont_path,
                                self.sample_rate,
                                project.properties.tempo,
                                self.root.as_deref(),
                                oc_engine::block_source::StreamingSourcePreparation {
                                    events: &events,
                                    workers: worker_options,
                                },
                            )
                        }
                        .map_err(|e| format!("Failed to create source for {}: {e}", track.name))?
                    };
                    // A full load starts at frame zero; a hot-added lane moves its
                    // clock when it is spliced in.
                    source.install_hairpin_layers(
                        preset,
                        project,
                        track,
                        &project.properties,
                        0,
                    )?;
                    self.source_specs.push(LaneSpec {
                        preset: preset.cloned(),
                        params: params.clone(),
                        performance: track.performance.is_some(),
                        smart_disable: track.mix.smart_disable,
                    });
                    self.renderers.push(SmartSource::new(
                        source,
                        self.sample_rate,
                        track.mix.smart_disable,
                    ));
                    #[cfg(test)]
                    if std::env::var_os("OCP_STREAM_PROFILE").is_some() {
                        eprintln!(
                            "source_load track={} milliseconds={:.3}",
                            track.name,
                            source_start.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                }
                Ok(())
            })?;
        }

        progress.stage(crate::build_progress::Stage::Effects);
        // Auxiliary pairs are planned once per load; blocks only reuse buffers.
        self.source_outputs
            .resize_with(self.renderers.len(), || SourceOutput::new(0, &[]));
        // A fresh load holds no voices, and the set is sized once so marking a
        // note while rendering never allocates.
        self.sounding = vec![0; self.renderers.len() * SOUNDING_WORDS];
        self.sync_aux_plan(project);

        #[cfg(test)]
        crate::profile_phase("build_sources", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();
        self.rebuild_compensation_and_graph(project)?;
        #[cfg(test)]
        crate::profile_phase("build_graph", phase);
        #[cfg(test)]
        let phase = std::time::Instant::now();

        if progressive_cursor.is_none() {
            self.clip_lanes = ClipLanes::load(project, self.sample_rate, self.root.as_deref())?;
            self.clip_media = self.clip_lanes.identities();
        }
        #[cfg(test)]
        crate::profile_phase("build_graph_clips", phase);

        // NOTE: Do NOT pre-send events here. The render_chunk() method handles
        // all event dispatching naturally as it advances through the timeline.
        // Pre-sending causes double-dispatch and overflows the player's internal
        // event ringbuffer.

        // Build audio graph (effect chains, routing, buses, master)
        self.render_cursor = 0;
        self.dsp_cursor = 0;
        self.rendered_ahead.clear();
        Ok(())
    }

    /// Recompute latency compensation and rebuild the mixer graph for the lanes.
    fn rebuild_compensation_and_graph(&mut self, project: &Project) -> Result<(), String> {
        let mut track_latencies = vec![0; self.num_tracks];
        let source_latencies = self
            .renderers
            .iter()
            .zip(&self.source_chains)
            .map(|(source, chain)| source.latency_samples() + chain.latency_samples())
            .collect::<Vec<_>>();
        for (&target, &latency) in self.source_tracks.iter().zip(&source_latencies) {
            track_latencies[target] = track_latencies[target].max(latency);
        }
        self.source_delays = self
            .source_tracks
            .iter()
            .zip(source_latencies)
            .map(|(&target, latency)| {
                std::collections::VecDeque::from(vec![
                    0.0;
                    (track_latencies[target] - latency) as usize
                        * 2
                ])
            })
            .collect();
        // Preset effects run on the pattern lanes; the graph's chains carry only
        // each track's own effects, exactly as an in-place chain patch does.
        let mixer_project = oc_engine::graph::graph_document(project);
        self.graph = Some(match self.touch.as_ref() {
            Some(touch) => AudioGraph::try_from_project_with_source_latencies_and_touch(
                &mixer_project,
                self.sample_rate,
                &track_latencies,
                Arc::clone(touch),
            )?,
            None => AudioGraph::try_from_project_with_source_latencies(
                &mixer_project,
                self.sample_rate,
                &track_latencies,
            )?,
        });
        if let Some(graph) = &mut self.graph {
            graph.set_spectrum_target(self.spectrum_target.clone());
            publish_chain_latency(&self.chain_latency_frames, graph);
        }
        self.prepare_streaming()?;
        self.update_drain_frames();
        self.rebuild_aux_delays();
        self.meter_input_peaks.clear();
        self.meter_input_peaks.resize(self.num_tracks, 0.0);
        if let Some(hub) = &self.meter_hub {
            if let Some(graph) = self.graph.as_ref() {
                hub.set_layout(
                    graph.track_chains.len(),
                    graph.bus_names(),
                    oc_engine::graph::MASTER_CHANNELS,
                );
            }
            hub.set_available(true);
        }
        Ok(())
    }

    fn processing_failed(&self) -> bool {
        self.renderers.iter().any(SmartSource::processing_failed)
            || self
                .source_chains
                .iter()
                .any(oc_engine::graph::EffectChain::processing_failed)
            || self
                .graph
                .as_ref()
                .is_some_and(AudioGraph::processing_failed)
    }

    fn take_processing_error(&mut self) -> Option<String> {
        use oc_audio::ProcessingFailureReport as Report;
        if !self.processing_fault {
            return None;
        }
        let mut report = Report::Unavailable;
        for source in &self.renderers {
            if source.processing_failed() {
                report = report.merge(source.take_processing_failure());
            }
            if matches!(report, Report::Ready(_)) {
                break;
            }
        }
        if !matches!(report, Report::Ready(_)) {
            for chain in &self.source_chains {
                if chain.processing_failed() {
                    report = report.merge(chain.take_processing_failure());
                }
                if matches!(report, Report::Ready(_)) {
                    break;
                }
            }
        }
        if !matches!(report, Report::Ready(_))
            && let Some(graph) = &self.graph
        {
            report = report.merge(graph.take_processing_failure());
        }
        let message = match report {
            Report::Pending => return None,
            Report::Ready(message) => message,
            Report::Reported => {
                "A plugin failed; its diagnostic was already reported by another owner".into()
            }
            Report::Unavailable => "An audio processor failed without a detailed diagnostic".into(),
        };
        self.processing_fault = false;
        Some(message)
    }

    fn prepare_streaming(&mut self) -> Result<(), String> {
        if let Some(graph) = &mut self.graph {
            graph
                .prepare(CHUNK_FRAMES)
                .map_err(|error| error.to_string())?;
        }
        for chain in &mut self.source_chains {
            chain
                .prepare_block(CHUNK_FRAMES)
                .map_err(|error| format!("Source chain preparation failed: {error:?}"))?;
        }
        self.track_chunks
            .resize_with(self.num_tracks, || Vec::with_capacity(CHUNK_FRAMES * 2));
        for buffer in &mut self.track_chunks {
            if buffer.capacity() < CHUNK_FRAMES * 2 {
                buffer.reserve(CHUNK_FRAMES * 2 - buffer.len());
            }
        }
        self.prepare_meter_frames();
        Ok(())
    }

    fn clear_pending_meters(&mut self) {
        while let Some(pending) = self.pending_meters.pop_front() {
            self.free_meter_frames.push(pending.frame);
        }
    }

    fn prepare_meter_frames(&mut self) {
        self.clear_pending_meters();
        self.free_meter_frames.clear();
        if self.meter_hub.is_some()
            && let Some(graph) = &self.graph
        {
            for _ in 0..MAX_PENDING_METERS {
                self.free_meter_frames.push(MeterHub::capture(
                    graph,
                    &[],
                    &[],
                    &[],
                    MonitorValues::default(),
                ));
            }
        }
    }

    /// Point the analyser at a panel target (`track:N`, `bus:name`, `master`).
    fn set_spectrum_target(&mut self, target: Option<String>) {
        let resolved = target.and_then(|target| resolve_spectrum_target(&target));
        self.spectrum_target = resolved.clone();
        if let Some(graph) = &mut self.graph {
            graph.set_spectrum_target(resolved);
        }
    }

    /// Patch a mixer-only edit. Existing instruments are never rebuilt; a track
    /// that was muted (and pruned) at load is handed to the source worker.
    fn apply_mixer(
        &mut self,
        project: &Project,
        soundfont_path: PathBuf,
    ) -> Result<MixerOutcome, String> {
        if self.project.is_none() || self.graph.is_none() {
            self.load_project(project, soundfont_path)?;
            return Ok(MixerOutcome::Reloaded);
        }
        self.soundfont_path = soundfont_path;
        let resolved = oc_dsl::preset::resolve(project)?;
        let project = &resolved;
        let audible = oc_engine::graph::audible_tracks(project);
        let missing: Vec<usize> = audible
            .iter()
            .enumerate()
            .filter(|(_, audible)| **audible)
            .map(|(index, _)| index)
            .filter(|index| !self.source_tracks.contains(index))
            .filter(|index| !self.pending_sources.contains(index))
            .collect();
        if let Some(graph) = self.graph.as_mut() {
            graph.apply_mixer_state(project);
            graph.apply_send_state(project);
            graph.set_master_gain_db(project.master.as_ref().map_or(0.0, |m| m.volume_db));
        }
        self.project = Some(project.clone());
        // A send added or retargeted by the mixer changes what lanes capture.
        self.sync_aux_plan(project);
        self.prepare_streaming()?;
        if missing.is_empty() {
            return Ok(MixerOutcome::Patched);
        }
        // Performance lanes need complete controller/held-note history. The
        // score-only hot-add worker cannot reconstruct that state by skipping
        // commands before the current frame. Rebuild and use bounded exact
        // history preroll instead of silently dropping those commands.
        if missing
            .iter()
            .any(|&index| project.tracks[index].performance.is_some())
        {
            let end = self.play_end_frame;
            self.load_project(project, self.soundfont_path.clone())?;
            self.play_end_frame = end;
            return Ok(MixerOutcome::Reloaded);
        }
        self.pending_sources.extend(missing.iter().copied());
        Ok(MixerOutcome::NeedsSources { tracks: missing })
    }

    /// Patch the effect chains an FX edit changed, without touching sources.
    ///
    /// A parameter change is faded into the live device, so the chain keeps its
    /// DSP state and the edit cannot click; a shape change rebuilds that one
    /// chain. An edit the chains cannot absorb (a track or bus came or went)
    /// reloads the project and keeps the transport where it was.
    fn apply_chain_change(
        &mut self,
        project: &Project,
        soundfont_path: PathBuf,
    ) -> Result<ChainOutcome, String> {
        if self.project.is_none() || self.graph.is_none() {
            self.load_project(project, soundfont_path)?;
            return Ok(ChainOutcome::Reloaded);
        }
        self.soundfont_path = soundfont_path.clone();
        let resolved = oc_dsl::preset::resolve(project)?;
        let previous = self
            .project
            .clone()
            .ok_or_else(|| "No project loaded".to_string())?;
        // A running transport fades the change; a stopped one applies it at
        // once, so the next press renders the new value from its first sample.
        let fade = self.playing || self.paused;
        let patched = self.graph.as_mut().unwrap().patch_chains(
            &previous,
            &resolved,
            self.touch.as_ref(),
            fade,
        )?;
        let chains = match patched {
            oc_engine::graph::ChainPatch::Patched { chains } => chains as u64,
            oc_engine::graph::ChainPatch::Structural => {
                let end = self.play_end_frame;
                self.load_project(&resolved, soundfont_path)?;
                self.play_end_frame = end;
                return Ok(ChainOutcome::Reloaded);
            }
        };
        self.project = Some(resolved);
        self.prepare_streaming()?;
        Ok(ChainOutcome::Patched { chains })
    }

    /// Drop audio staged while the transport was stopped.
    ///
    /// A warm-up renders ahead of the playhead with the chains it was built
    /// from; once an edit changed a chain, that audio cannot be published by
    /// the next press.
    fn discard_staged_audio(&mut self, playback: &StreamingPlayback) {
        self.rendered_ahead.clear();
        self.ring_prefilled = false;
        self.warm_fill = false;
        invalidate_playback_buffer(playback);
    }

    /// Plan every lane's auxiliary pairs from the project's sends.
    ///
    /// A send the source cannot fan out keeps the main pair only and warns
    /// once per load; offline rendering rejects that project instead.
    fn sync_aux_plan(&mut self, project: &Project) {
        if self.source_outputs.len() != self.renderers.len() {
            self.source_outputs
                .resize_with(self.renderers.len(), || SourceOutput::new(0, &[]));
        }
        let requested = oc_engine::render_cache::aux_pairs_by_track(project);
        let mut warned = std::mem::take(&mut self.warned_aux);
        let mut warned_ports = std::mem::take(&mut self.warned_ports);
        let mut changed = false;
        for lane in 0..self.renderers.len() {
            let track = self.source_tracks[lane];
            let pairs = requested.get(track).map(Vec::as_slice).unwrap_or(&[]);
            let name = project
                .tracks
                .get(track)
                .map_or("", |track| track.name.as_str());
            let main_pair = self.lane_main_pair(project, lane, track, name, &mut warned_ports);
            let wanted =
                supported_aux_pairs(&self.renderers[lane], pairs, track, name, &mut warned);
            if wanted != self.source_outputs[lane].aux_pairs()
                || main_pair != self.source_outputs[lane].main_pair()
            {
                self.source_outputs[lane] = SourceOutput::new(main_pair, &wanted);
                changed = true;
            }
        }
        self.warned_aux = warned;
        self.warned_ports = warned_ports;
        if changed || self.aux_delays.len() != self.renderers.len() {
            self.rebuild_aux_buffers();
            self.rebuild_aux_delays();
        }
    }

    /// Pair a lane reads as its main output. A routing error keeps pair 0;
    /// offline rendering refuses the same project instead.
    ///
    /// A track that asked for the main pair falls back to exactly that pair, so
    /// the message is a debug note; only a lost later pair warns, once per
    /// track.
    fn lane_main_pair(
        &self,
        project: &Project,
        lane: usize,
        track: usize,
        track_name: &str,
        warned: &mut HashSet<usize>,
    ) -> usize {
        let ports = self.renderers[lane].output_ports();
        match oc_engine::render_cache::track_output_pair(project, track, &ports, self.sample_rate) {
            Ok(pair) => pair,
            Err(error) => {
                let lost_output = oc_engine::render_cache::track_requests_non_main_output(
                    project,
                    track,
                    self.sample_rate,
                );
                if lost_output {
                    if warned.insert(track) {
                        log::warn!(
                            "[render_thread] {error}; track '{track_name}' plays its main pair"
                        );
                    }
                } else {
                    log::debug!(
                        "[render_thread] {error}; track '{track_name}' plays its main pair"
                    );
                }
                0
            }
        }
    }

    /// Delay each lane's auxiliary pairs to the lane-aligned point the graph
    /// assumes. Aux bypasses the lane chain, but the graph's PDC uses the
    /// lane's full latency, so the difference is applied here.
    fn rebuild_aux_delays(&mut self) {
        let mut track_latencies = vec![0_u32; self.num_tracks];
        let lane_latencies: Vec<u32> = self
            .renderers
            .iter()
            .zip(&self.source_chains)
            .map(|(source, chain)| source.latency_samples() + chain.latency_samples())
            .collect();
        for (&target, &latency) in self.source_tracks.iter().zip(&lane_latencies) {
            track_latencies[target] = track_latencies[target].max(latency);
        }
        let mut delays = Vec::with_capacity(self.renderers.len());
        for (lane, source) in self.renderers.iter().enumerate() {
            let target = self.source_tracks[lane];
            let delay = track_latencies
                .get(target)
                .copied()
                .unwrap_or(0)
                .saturating_sub(source.latency_samples()) as usize
                * 2;
            let pairs = self
                .source_outputs
                .get(lane)
                .map_or(0, |output| output.aux_pairs().len());
            let mut lane_delays = Vec::with_capacity(pairs);
            for _ in 0..pairs {
                let mut line = VecDeque::with_capacity(delay + MAX_SOURCE_BLOCK_FRAMES * 2);
                line.extend(std::iter::repeat_n(0.0, delay));
                lane_delays.push(line);
            }
            delays.push(lane_delays);
        }
        self.aux_delays = delays;
    }

    /// Key the per-chunk auxiliary buffers by the planned (track, pair) pairs.
    fn rebuild_aux_buffers(&mut self) {
        let keys: Vec<(usize, usize)> = self
            .source_outputs
            .iter()
            .enumerate()
            .flat_map(|(lane, output)| {
                let track = self.source_tracks[lane];
                output
                    .aux_pairs()
                    .iter()
                    .map(move |&pair| (track, pair))
                    .collect::<Vec<_>>()
            })
            .collect();
        self.aux_buffers.clear();
        for key in keys {
            self.aux_buffers
                .entry(key)
                .or_insert_with(|| Vec::with_capacity(CHUNK_FRAMES * 2));
        }
    }

    /// Splice lanes built by the source worker into the live graph.
    fn splice_sources(&mut self, lanes: Vec<BuiltLane>) -> Result<(), String> {
        let Some(project) = self.project.clone() else {
            return Ok(());
        };
        if lanes.is_empty() {
            return Ok(());
        }
        for lane in lanes {
            self.pending_sources.retain(|target| *target != lane.target);
            let lane_index = self.renderers.len();
            let mut indices = Vec::with_capacity(lane.events.len());
            for mut event in lane.events {
                event.track_index = lane_index;
                indices.push(self.events.len());
                self.events.push(event);
            }
            // A hot-added lane starts at the current DSP position; notes already
            // sounding are not retriggered.
            let cursor = indices
                .iter()
                .position(|&index| self.events[index].time_samples >= self.dsp_cursor)
                .unwrap_or(indices.len());
            let mut source = lane.source;
            // The lane was built at frame zero, so hand it the project position
            // it joins at; the hairpin ramps are in project frames.
            source.set_project_frame(self.dsp_cursor);
            self.renderers.push(source);
            self.source_specs.push(lane.spec);
            self.source_outputs.push(SourceOutput::new(0, &[]));
            self.source_chains.push(lane.chain);
            self.source_tracks.push(lane.target);
            self.events_by_track.push(indices);
            self.track_event_cursors.push(cursor);
            self.sounding
                .resize(self.renderers.len() * SOUNDING_WORDS, 0);
            self.performance_events.push(Vec::new());
            self.performance_cursors.push(0);
        }
        self.num_tracks = project.tracks.len();
        self.sync_aux_plan(&project);
        self.rebuild_compensation_and_graph(&project)?;
        if let Some(graph) = self.graph.as_mut() {
            graph.apply_mixer_state(&project);
            graph.apply_send_state(&project);
            graph.set_master_gain_db(project.master.as_ref().map_or(0.0, |m| m.volume_db));
        }
        self.prepare_streaming()?;
        Ok(())
    }

    /// Render `frames` frames (at most CHUNK_FRAMES) and return the mixed output.
    ///
    /// Events are interleaved with rendering (not sent all at once) to avoid
    /// overflowing the player's internal event ringbuffer.
    #[cfg(test)]
    fn render_chunk(&mut self, frames: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(CHUNK_FRAMES * 2);
        self.render_chunk_into(frames, &mut output);
        output
    }

    fn render_chunk_into(&mut self, frames: usize, output: &mut Vec<f32>) {
        self.render_chunk_before_into(
            self.render_cursor + frames.min(CHUNK_FRAMES) as u64,
            false,
            output,
        );
    }

    fn render_chunk_before_into(&mut self, limit: u64, seek_history: bool, output: &mut Vec<f32>) {
        output.clear();
        self.waiting_for_clip = false;
        let frames = (limit
            .min(self.playback_end())
            .saturating_sub(self.render_cursor))
        .min(CHUNK_FRAMES as u64) as usize;
        while self.rendered_ahead.len() < frames * 2 {
            let dsp_frames = if seek_history {
                frames.min(limit.saturating_sub(self.dsp_cursor) as usize)
            } else {
                frames
            };
            let mut block = std::mem::take(&mut self.dsp_block);
            self.render_dsp_chunk_into(dsp_frames, seek_history, &mut block);
            let empty = block.is_empty();
            self.rendered_ahead.extend(block.iter().copied());
            self.dsp_block = block;
            if empty {
                return;
            }
        }
        output.extend(self.rendered_ahead.drain(..frames * 2));
        self.render_cursor += frames as u64;
    }

    fn render_dsp_chunk_into(&mut self, frames: usize, seek_history: bool, mixed: &mut Vec<f32>) {
        mixed.clear();
        if self.processing_failed() {
            self.processing_fault = true;
            return;
        }
        let chunk_start = self.dsp_cursor;
        let chunk_end = (chunk_start + frames.min(CHUNK_FRAMES) as u64).min(self.playback_end());
        let actual_frames = chunk_end.saturating_sub(chunk_start) as usize;
        self.waiting_for_clip = false;

        if actual_frames == 0 {
            return;
        }

        let clips_ready = if seek_history {
            self.clip_lanes
                .ready_for_seek_history(chunk_start, chunk_end)
        } else {
            self.clip_lanes.ready_for(chunk_start, chunk_end)
        };
        match clips_ready {
            Ok(true) => {}
            Ok(false) => {
                self.waiting_for_clip = true;
                return;
            }
            Err(error) => {
                self.render_error = Some(error);
                return;
            }
        }

        // After the program, let delay lines and effects process silence.
        // A bounded audition supplies its own end and does not add this drain.
        let source_end = if self.play_end_frame.is_some() {
            chunk_end
        } else {
            chunk_end.min(self.total_frames)
        };
        // Per-track render: interleave events with synthesis
        let mut track_chunks = std::mem::take(&mut self.track_chunks);
        for buffer in &mut track_chunks {
            buffer.clear();
            buffer.resize(actual_frames * 2, 0.0);
        }
        let mut track_audio = std::mem::take(&mut self.lane_audio);
        let sub_chunk_size: usize = 64; // Render in small sub-chunks for timing accuracy

        // Auxiliary returns start at silence; capacity is kept from load.
        for buffer in self.aux_buffers.values_mut() {
            buffer.clear();
            buffer.resize(actual_frames * 2, 0.0);
        }

        for track_idx in 0..self.renderers.len() {
            track_audio.clear();
            let cursor = &mut self.track_event_cursors[track_idx];
            let track_events = &self.events_by_track[track_idx];
            let mut pos = chunk_start;

            while pos < source_end {
                if self.renderers[track_idx].is_vst3()
                    || self.renderers[track_idx].performance_uses_offsets()
                {
                    // Same 128-frame VST3 processing quantum as offline export.
                    // Keep sample offsets; chopping at each note changes plugin
                    // block-dependent envelopes and round-robin behavior.
                    let transport = self.transport.as_ref().unwrap().at_sample(pos);
                    let frames = transport.block_frames((source_end - pos).min(128) as usize);
                    let end = pos + frames as u64;
                    let performance_cursor = &mut self.performance_cursors[track_idx];
                    while *performance_cursor < self.performance_events[track_idx].len() {
                        let command = &self.performance_events[track_idx][*performance_cursor];
                        if command.sample >= end {
                            break;
                        }
                        if let Err(error) = self.renderers[track_idx].send_performance_at(
                            command,
                            command.sample.saturating_sub(pos) as u32,
                            frames as u32,
                        ) {
                            self.render_error = Some(format!("Track {track_idx}: {error}"));
                            return;
                        }
                        track_sounding_command(
                            lane_sounding(&mut self.sounding, track_idx),
                            command.channel,
                            &command.message,
                        );
                        *performance_cursor += 1;
                    }
                    while *cursor < track_events.len() {
                        let ev = &self.events[track_events[*cursor]];
                        if ev.time_samples >= end {
                            break;
                        }
                        if let Err(error) = self.renderers[track_idx]
                            .send_vst3_at(&ev.event, ev.time_samples.saturating_sub(pos) as i32)
                        {
                            self.render_error = Some(format!("Track {track_idx}: {error}"));
                            return;
                        }
                        track_sounding(lane_sounding(&mut self.sounding, track_idx), &ev.event);
                        *cursor += 1;
                    }
                    self.renderers[track_idx].render_into(
                        frames,
                        self.sample_rate,
                        transport,
                        &mut self.source_outputs[track_idx],
                    );
                    track_audio.extend_from_slice(self.source_outputs[track_idx].main());
                    accumulate_aux(
                        &self.source_outputs[track_idx],
                        &mut self.aux_delays[track_idx],
                        &mut self.aux_buffers,
                        self.source_tracks[track_idx],
                        (pos - chunk_start) as usize * 2,
                    );
                    pos = end;
                    continue;
                }
                let performance_cursor = &mut self.performance_cursors[track_idx];
                while *performance_cursor < self.performance_events[track_idx].len() {
                    let command = &self.performance_events[track_idx][*performance_cursor];
                    if command.sample > pos {
                        break;
                    }
                    if let Err(error) = self.renderers[track_idx].send_performance_at(command, 0, 1)
                    {
                        self.render_error = Some(format!("Track {track_idx}: {error}"));
                        return;
                    }
                    track_sounding_command(
                        lane_sounding(&mut self.sounding, track_idx),
                        command.channel,
                        &command.message,
                    );
                    *performance_cursor += 1;
                }
                // Send all events at or before current position
                while *cursor < track_events.len() {
                    let ev = &self.events[track_events[*cursor]];
                    if ev.time_samples > pos {
                        break;
                    }
                    if let Err(error) = self.renderers[track_idx].send(&ev.event) {
                        self.render_error = Some(format!("Track {track_idx}: {error}"));
                        return;
                    }
                    track_sounding(lane_sounding(&mut self.sounding, track_idx), &ev.event);
                    *cursor += 1;
                }

                // Determine sub-chunk size: render up to next event or sub_chunk_size
                let next_event_time = if *cursor < track_events.len() {
                    self.events[track_events[*cursor]].time_samples
                } else {
                    source_end
                };
                let next_event_time = next_event_time.min(
                    self.performance_events[track_idx]
                        .get(*performance_cursor)
                        .map_or(source_end, |event| event.sample),
                );
                let remaining = (source_end - pos) as usize;
                let until_next = (next_event_time - pos) as usize;
                let clock_limit = self
                    .transport
                    .as_ref()
                    .unwrap()
                    .at_sample(pos)
                    .next_clock_change_sample()
                    .map(|end| (end - pos) as usize)
                    .unwrap_or(remaining);
                let frames_to_render = sub_chunk_size
                    .min(remaining)
                    .min(until_next.max(1))
                    .min(clock_limit.max(1));

                let transport = self.transport.as_ref().unwrap().at_sample(pos);
                self.renderers[track_idx].render_into(
                    frames_to_render,
                    self.sample_rate,
                    transport,
                    &mut self.source_outputs[track_idx],
                );
                track_audio.extend_from_slice(self.source_outputs[track_idx].main());
                // Auxiliary outputs bypass the lane chain and the fader.
                accumulate_aux(
                    &self.source_outputs[track_idx],
                    &mut self.aux_delays[track_idx],
                    &mut self.aux_buffers,
                    self.source_tracks[track_idx],
                    (pos - chunk_start) as usize * 2,
                );
                pos += frames_to_render as u64;
            }

            track_audio.resize(actual_frames * 2, 0.0);
            let drain_start = source_end.saturating_sub(chunk_start) as usize * 2;
            for (&pair, delay) in self.source_outputs[track_idx]
                .aux_pairs()
                .iter()
                .zip(&mut self.aux_delays[track_idx])
            {
                if !delay.is_empty()
                    && let Some(bus) = self
                        .aux_buffers
                        .get_mut(&(self.source_tracks[track_idx], pair))
                {
                    for out in &mut bus[drain_start..] {
                        delay.push_back(0.0);
                        *out += delay.pop_front().unwrap();
                    }
                }
            }
            let transport = self.transport.as_ref().map(|t| t.at_sample(chunk_start));
            if let Err(error) = self.source_chains[track_idx].try_process_with_context(
                &mut track_audio,
                AudioProcessContext {
                    sample_rate: self.sample_rate,
                    transport: transport.as_ref(),
                    input: AudioProcessInput::Main,
                },
            ) {
                self.render_error = Some(format!("Track {track_idx}: {error}"));
                self.track_chunks = track_chunks;
                self.lane_audio = track_audio;
                return;
            }
            let delay = &mut self.source_delays[track_idx];
            for (out, sample) in track_chunks[self.source_tracks[track_idx]]
                .iter_mut()
                .zip(track_audio.iter().copied())
            {
                if delay.is_empty() {
                    *out += sample;
                } else {
                    delay.push_back(sample);
                    *out += delay.pop_front().unwrap();
                }
            }
        }

        if self.processing_failed() {
            self.processing_fault = true;
            self.track_chunks = track_chunks;
            self.lane_audio = track_audio;
            return;
        }

        // Clip lanes sit alongside the instrument sources: same track buffer,
        // before track effects and routing. The worker can read spooled clips.
        if let Err(error) = self.clip_lanes.mix_into(chunk_start, &mut track_chunks) {
            self.render_error = Some(error);
            return;
        }

        // Source inserts and summing precede the parent graph exactly as offline.
        self.mix_chunk_into(&mut track_chunks, actual_frames, mixed);
        self.track_chunks = track_chunks;
        self.lane_audio = track_audio;
        if self.processing_failed() {
            self.processing_fault = true;
        }
        if self.render_error.is_some() || self.processing_fault {
            mixed.clear();
            return;
        }

        self.dsp_cursor = chunk_end;
        #[cfg(test)]
        self.publish_sounding_voices();
    }

    /// Publish how many notes the lanes are holding, so a test can tell a
    /// patched chain from a rebuilt project.
    #[cfg(test)]
    fn publish_sounding_voices(&self) {
        let held = self
            .sounding
            .iter()
            .map(|word| word.count_ones() as u64)
            .sum();
        self.sounding_voices.store(held, Ordering::Release);
    }

    /// Mix track chunks through the audio graph, then sum the click.
    #[cfg(test)]
    fn mix_chunk(&mut self, track_chunks: &mut [Vec<f32>], frames: usize) -> Vec<f32> {
        let mut mixed = Vec::with_capacity(frames * 2);
        if let Some(graph) = &mut self.graph {
            graph.prepare(frames).unwrap();
        }
        if self.free_meter_frames.is_empty() && self.pending_meters.is_empty() {
            self.prepare_meter_frames();
        }
        self.mix_chunk_into(track_chunks, frames, &mut mixed);
        mixed
    }

    fn mix_chunk_into(
        &mut self,
        track_chunks: &mut [Vec<f32>],
        frames: usize,
        mixed: &mut Vec<f32>,
    ) {
        if self.processing_failed() {
            self.processing_fault = true;
            mixed.clear();
            return;
        }
        self.mix_tracks_into(track_chunks, frames, mixed);
        if self.processing_failed() {
            self.processing_fault = true;
            mixed.clear();
            return;
        }
        if self.render_error.is_none() && !self.processing_fault {
            self.mix_click(mixed, self.dsp_cursor);
        }
    }

    /// Sum the metronome into one mixed block, after the master chain: the
    /// click is audition material, never part of the project's audio. It
    /// sounds on the same reference as the mix, so the chain that delays the
    /// backing audio delays the click by the same frames.
    fn mix_click(&mut self, mixed: &mut [f32], start_frame: u64) {
        if !self.click_settings.enabled || mixed.is_empty() {
            return;
        }
        let Some(clock) = self.clock.as_ref() else {
            return;
        };
        let frames = mixed.len() / 2;
        let (window_start, shift) = self.click_window(start_frame);
        let mut event_frames = frames.saturating_sub(shift);
        if self.drain_frames > 0 && self.play_end_frame.is_none() {
            event_frames =
                event_frames.min(self.total_frames.saturating_sub(window_start) as usize);
        }
        if let Err(error) = oc_audio::click::click_events_into(
            clock,
            window_start,
            event_frames,
            self.sample_rate,
            &mut self.click_events,
        ) {
            self.render_error = Some(error.to_string());
            mixed.fill(0.0);
            return;
        }
        for event in &mut self.click_events {
            event.offset += shift;
        }
        if let Err(error) = self.click.mix_prepared(
            mixed,
            &self.click_events,
            self.sample_rate,
            self.click_settings.gain(),
        ) {
            self.render_error = Some(error.to_string());
            mixed.fill(0.0);
        }
    }

    /// The timeline this block's click sounds on, and how far its first event
    /// moves into the block because of the reference delay.
    ///
    /// A block's audio sits `delay` frames behind its start, so the click reads
    /// its events from `start_frame - delay`. A run that begins inside the
    /// delay has no timeline before frame zero: its window starts at the first
    /// frame and the events move to the head by the missing amount.
    fn click_window(&self, start_frame: u64) -> (u64, usize) {
        let delay = self.chain_latency_frames.load(Ordering::Acquire);
        match start_frame.checked_sub(delay) {
            Some(window_start) => (window_start, 0),
            None => (0, (delay - start_frame) as usize),
        }
    }

    /// Emit the next count-in block. The timeline does not advance: no
    /// project audio is rendered until the count-in is over.
    #[cfg(test)]
    fn render_count_in_chunk(&mut self, frames: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(frames * 2);
        self.render_count_in_chunk_into(frames, &mut output);
        output
    }

    fn render_count_in_chunk_into(&mut self, frames: usize, out: &mut Vec<f32>) {
        out.clear();
        let Some(state) = self.count_in.as_mut() else {
            return;
        };
        let left = state.plan.frames.saturating_sub(state.emitted);
        let take = frames.min(CHUNK_FRAMES).min(left as usize);
        if take == 0 {
            return;
        }
        let start = state.emitted as usize;
        let delay = self.chain_latency_frames.load(Ordering::Acquire) as usize;
        self.click_events.clear();
        for event in &state.plan.events {
            let at = event.offset + delay;
            if at >= start && at < start + take {
                if self.click_events.len() == self.click_events.capacity() {
                    self.render_error = Some("Count-in click event capacity exceeded".into());
                    return;
                }
                self.click_events.push(ClickEvent {
                    offset: at - start,
                    accent: event.accent,
                });
            }
        }
        out.resize(take * 2, 0.0);
        if let Err(error) = self.click.mix_prepared(
            out,
            &self.click_events,
            self.sample_rate,
            self.click_settings.gain(),
        ) {
            self.render_error = Some(error.to_string());
            out.clear();
            return;
        }
        state.emitted += take as u64;
        if state.emitted >= state.plan.frames {
            // Keep the plan's allocation until the next control-plane start.
            self.completed_count_in = self.count_in.take();
        }
    }

    /// Mix track chunks through the audio graph (effects + routing + master).
    fn mix_tracks_into(
        &mut self,
        track_chunks: &mut [Vec<f32>],
        frames: usize,
        mixed: &mut Vec<f32>,
    ) {
        mixed.clear();
        mixed.resize(frames * 2, 0.0);
        let Some(graph) = self.graph.as_mut() else {
            // No graph — just sum all tracks

            for buf in track_chunks.iter() {
                for (i, &s) in buf.iter().enumerate() {
                    if i < mixed.len() {
                        mixed[i] += s;
                    }
                }
            }
            return;
        };

        if self.meter_hub.is_some() {
            for (slot, buffer) in self.meter_input_peaks.iter_mut().zip(track_chunks.iter()) {
                *slot = peak_of(buffer);
            }
        }
        let transport = self
            .transport
            .as_ref()
            .map(|t| t.at_sample(self.dsp_cursor));
        if let Err(error) = graph.process_prepared(PreparedGraphBlock {
            tracks: track_chunks,
            transport: transport.as_ref(),
            aux: &self.aux_buffers,
            output: mixed,
        }) {
            self.render_error = Some(error.to_string());
            mixed.clear();
            return;
        }

        if graph.processing_failed() {
            self.processing_fault = true;
            mixed.clear();
            return;
        }

        let monitor = if self.meter_hub.is_some() {
            // Both readings describe the stereo mix, so they stay meaningful
            // while MONO is engaged and the listener hears the fold.
            self.correlation.push(mixed);
            self.loudness.push_interleaved(mixed);
            let mut points = [0.0f32; GONIOMETER_VALUES];
            let goniometer_points = goniometer(mixed, &mut points);
            MonitorValues {
                short_term_lufs: self.loudness.lufs().map(|value| value as f32),
                correlation: self.correlation.value(),
                goniometer: points,
                goniometer_points,
            }
        } else {
            MonitorValues::default()
        };
        // The monitor fold is the last thing before the device: it changes what
        // is heard, never what is rendered or exported.
        if self.mono_check {
            fold_to_mono(mixed);
        }
        // The picture must not run ahead of the sound: hold this chunk's meters
        // until the cursor reaches the audio the device is playing.
        if self.seek_target.is_none() && self.meter_hub.is_some() {
            let Some(mut frame) = self
                .free_meter_frames
                .pop()
                .or_else(|| self.pending_meters.pop_front().map(|pending| pending.frame))
            else {
                // Layout preparation always owns a bounded frame pool.
                self.render_error = Some("Streaming meter frames require preparation".into());
                mixed.clear();
                return;
            };
            MeterHub::capture_into(
                graph,
                &self.meter_input_peaks,
                track_chunks,
                mixed,
                monitor,
                &mut frame,
            );
            self.pending_meters.push_back(PendingMeters {
                due_frame: self.dsp_cursor,
                frame,
            });
            if let Some(hub) = &self.meter_hub {
                hub.set_mono_applied(self.mono_check);
            }
        }
    }

    /// Publish meter frames whose audio has left the ring for the device.
    fn release_meters(&mut self, cursor_frames: u64, device_frames: u64) {
        let Some(hub) = self.meter_hub.clone() else {
            return;
        };
        // A warm ring has meters queued but nothing has left the device yet.
        if cursor_frames == 0 {
            return;
        }
        let reached = cursor_frames.saturating_sub(device_frames * DEVICE_LATENCY_BLOCKS);
        while let Some(due_frame) = self.pending_meters.front().map(|p| p.due_frame) {
            if due_frame > reached {
                break;
            }
            if let Some(pending) = self.pending_meters.pop_front() {
                hub.apply(&pending.frame);
                self.free_meter_frames.push(pending.frame);
            }
        }
    }

    /// Start an interruptible, exact-history seek. Do not approximate held notes
    /// by retriggering them at the destination with fresh attack envelopes.
    fn seek_to(&mut self, frame: u64) -> Result<(), String> {
        if self.project.is_none() {
            return Err("No streaming project loaded".into());
        }
        self.clear_pending_meters();
        // A reading must never average audio from two places in the timeline.
        self.loudness.reset();
        self.correlation.reset();
        let target = frame.min(self.timeline_end());
        let playing = self.playing;
        let play_end_frame = self.play_end_frame;
        // DSP only moves forward: a forward seek (including the warm staging)
        // can keep its history, while a backward seek rewinds the lanes to the
        // origin and replays the gap. The instruments stay instantiated.
        if target < self.render_cursor && self.dsp_cursor != 0 {
            self.rewind_for_seek()?;
        }
        self.playing = playing;
        self.play_end_frame = play_end_frame;
        self.loop_seam_fade = 0;
        self.seek_target = (target > self.render_cursor).then_some(target);
        self.update_preroll_deadline();
        Ok(())
    }

    fn start_playing(&mut self) {
        self.playing = true;
        self.update_preroll_deadline();
    }

    fn update_preroll_deadline(&mut self) {
        // A pause can afford exact history; playback cannot hold the device
        // silent for the length of the gap.
        self.preroll_deadline = self
            .seek_target
            .filter(|_| self.playing)
            .map(|_| std::time::Instant::now() + SEEK_PREROLL_BUDGET);
    }

    /// Return every lane to the project origin while keeping its instrument.
    ///
    /// A source that cannot restore its own DSP state is rebuilt alone; the
    /// other lanes keep their instances and plugin state.
    fn rewind_for_seek(&mut self) -> Result<(), String> {
        for lane in 0..self.renderers.len() {
            if self.renderers[lane].rewind() {
                continue;
            }
            self.rebuild_source(lane)?;
        }
        for chain in &mut self.source_chains {
            chain.reset();
        }
        if let Some(graph) = self.graph.as_mut() {
            graph.reset();
        }
        for delay in &mut self.source_delays {
            for sample in delay.iter_mut() {
                *sample = 0.0;
            }
        }
        for lane in &mut self.aux_delays {
            for delay in lane {
                for sample in delay.iter_mut() {
                    *sample = 0.0;
                }
            }
        }
        self.render_cursor = 0;
        self.dsp_cursor = 0;
        self.track_event_cursors.fill(0);
        self.performance_cursors.fill(0);
        self.sounding.fill(0);
        self.rendered_ahead.clear();
        self.ring_prefilled = false;
        self.click.reset();
        self.meter_input_peaks.fill(0.0);
        Ok(())
    }

    /// Build a fresh instance for one lane whose source cannot rewind.
    ///
    /// Only the lane's own instrument is constructed; the graph, the other
    /// lanes and every effect chain stay where they are.
    fn rebuild_source(&mut self, lane: usize) -> Result<(), String> {
        let Some(spec) = self.source_specs.get(lane) else {
            return Err("Cannot rebuild a lane without its instrument spec".into());
        };
        let preset = spec.preset.clone();
        let params = spec.params.clone();
        let performance = spec.performance;
        let smart_disable = spec.smart_disable;
        let commands: &[oc_synth::performance::ScheduledCommand] = self
            .performance_events
            .get(lane)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let tempo = self.project.as_ref().map_or(120.0, |p| p.properties.tempo);
        self.instrument_instantiations
            .fetch_add(1, Ordering::Relaxed);
        let source = if performance {
            BlockSource::new_performance(
                preset.as_ref(),
                &params,
                &self.soundfont_path,
                self.sample_rate,
                tempo,
                commands,
                self.root.as_deref(),
            )
        } else {
            let events = self.events_by_track[lane]
                .iter()
                .map(|&index| &self.events[index].event)
                .collect::<Vec<_>>();
            BlockSource::new_streaming(
                preset.as_ref(),
                &params,
                &self.soundfont_path,
                self.sample_rate,
                tempo,
                self.root.as_deref(),
                oc_engine::block_source::StreamingSourcePreparation {
                    events: &events,
                    workers: self.worker_options,
                },
            )
        }
        .map_err(|error| format!("Failed to rebuild a lane: {error}"))?;
        self.renderers[lane] = SmartSource::new(source, self.sample_rate, smart_disable);
        Ok(())
    }

    /// True when an exact preroll has spent its wall-clock budget.
    fn preroll_expired(&self) -> bool {
        self.preroll_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }

    /// Start at the destination instead of reconstructing the skipped gap.
    ///
    /// The audio between the previous position and the destination is not
    /// replayed, so every voice alive there is released and the channel state
    /// the destination needs is re-applied from the last event before it.
    fn abandon_preroll(&mut self) {
        let Some(target) = self.seek_target.take() else {
            return;
        };
        log::debug!(
            "[render_thread] seek preroll spent its {:?} budget; starting at frame {target} with released voices",
            SEEK_PREROLL_BUDGET
        );
        self.preroll_deadline = None;
        self.rendered_ahead.clear();
        self.clear_pending_meters();
        self.loudness.reset();
        self.correlation.reset();
        self.render_cursor = target;
        self.dsp_cursor = target;

        // Reduce the skipped range to the state the destination needs: the
        // notes it would still be holding, and the last value of every channel
        // control. Everything else in the gap is in the past.
        let mut programs: HashMap<(usize, u8), usize> = HashMap::new();
        let mut bends: HashMap<(usize, u8), usize> = HashMap::new();
        let mut pressures: HashMap<(usize, u8), usize> = HashMap::new();
        let mut controllers: HashMap<(usize, u8, u8), usize> = HashMap::new();
        let mut held: Vec<(usize, u8, u8)> = Vec::new();
        for lane in 0..self.events_by_track.len() {
            for &index in &self.events_by_track[lane][self.track_event_cursors[lane]..] {
                let event = &self.events[index];
                if event.time_samples >= target {
                    break;
                }
                match &event.event {
                    MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    } if *velocity > 0
                        && !held
                            .iter()
                            .any(|&(l, c, n)| l == lane && c == *channel && n == *note) =>
                    {
                        held.push((lane, *channel, *note));
                    }
                    MidiMessage::NoteOff { channel, note }
                    | MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity: 0,
                    } => held.retain(|&(l, c, n)| !(l == lane && c == *channel && n == *note)),
                    MidiMessage::ProgramChange { channel, .. } => {
                        programs.insert((lane, *channel), index);
                    }
                    MidiMessage::PitchBend { channel, .. } => {
                        bends.insert((lane, *channel), index);
                    }
                    MidiMessage::ChannelPressure { channel, .. } => {
                        pressures.insert((lane, *channel), index);
                    }
                    MidiMessage::ControlChange {
                        channel,
                        controller,
                        ..
                    }
                    | MidiMessage::ControlChange14 {
                        channel,
                        controller,
                        ..
                    } => {
                        controllers.insert((lane, *channel, *controller), index);
                    }
                    _ => {}
                }
            }
        }
        for (lane, channel, note) in held {
            let _ = self.renderers[lane].send(&MidiMessage::NoteOff { channel, note });
        }
        // A note whose note-on is before the cursor is not in the skipped range,
        // so release everything the lanes are still holding. A bare channel
        // reset is not enough: the renderers are MIDI sinks, and a hosted
        // instrument keeps its own voices.
        for lane in 0..self.renderers.len() {
            let words = lane_sounding(&mut self.sounding, lane);
            for index in 0..SOUNDING_CHANNELS * SOUNDING_NOTES {
                if words[index / 64] & (1u64 << (index % 64)) == 0 {
                    continue;
                }
                let _ = self.renderers[lane].send(&MidiMessage::NoteOff {
                    channel: (index / SOUNDING_NOTES) as u8,
                    note: (index % SOUNDING_NOTES) as u8,
                });
            }
        }
        self.sounding.fill(0);
        for index in programs
            .values()
            .chain(bends.values())
            .chain(pressures.values())
            .chain(controllers.values())
        {
            let event = &self.events[*index];
            let _ = self.renderers[event.track_index].send(&event.event);
        }
        for lane in 0..self.events_by_track.len() {
            let events = &self.events;
            let cursor = &mut self.track_event_cursors[lane];
            while *cursor < self.events_by_track[lane].len()
                && events[self.events_by_track[lane][*cursor]].time_samples < target
            {
                *cursor += 1;
            }
        }
        // Performance lanes are scheduled commands, not interpreted events.
        for lane in 0..self.performance_events.len() {
            let commands = &self.performance_events[lane];
            let cursor = &mut self.performance_cursors[lane];
            while *cursor < commands.len() && commands[*cursor].sample < target {
                if let oc_dsl::performance::ChannelCommand::NoteOff { note, .. } =
                    commands[*cursor].message
                {
                    let channel = commands[*cursor].channel;
                    let _ = self.renderers[lane].send(&MidiMessage::NoteOff { channel, note });
                }
                *cursor += 1;
            }
        }
    }

    /// At most one bounded audio chunk per command-loop iteration. Returns the
    /// frames it computed: a seek pays them before its first block.
    fn preroll_chunk(&mut self) -> u64 {
        let Some(target) = self.seek_target else {
            return 0;
        };
        let mut rendered = std::mem::take(&mut self.preroll_buffer);
        self.render_chunk_before_into(target, true, &mut rendered);
        let frames = (rendered.len() / 2) as u64;
        self.preroll_buffer = rendered;
        if self.render_cursor == target {
            self.seek_target = None;
            self.preroll_deadline = None;
        }
        frames
    }
}

/// Auxiliary pairs a lane captures, limited to what its source declares.
///
/// Returns the pairs in plan order; an unsupported pair is dropped and warned
/// once per (track, pair) so a playback session does not repeat the message.
fn supported_aux_pairs(
    source: &SmartSource,
    requested: &[usize],
    track: usize,
    track_name: &str,
    warned: &mut HashSet<(usize, usize)>,
) -> Vec<usize> {
    if requested.is_empty() {
        return Vec::new();
    }
    let capacity = source.aux_capacity();
    let mut pairs = Vec::with_capacity(requested.len());
    for &pair in requested {
        if capacity.is_some_and(|capacity| pair < capacity) {
            pairs.push(pair);
        } else if warned.insert((track, pair)) {
            log::warn!(
                "[render_thread] track '{track_name}' sends auxiliary output {} but its instrument {}; that send is silent while streaming",
                pair + 1,
                capacity.map_or_else(
                    || "hosts no plugin outputs".to_string(),
                    |capacity| format!("declares {capacity} stereo pair(s)"),
                ),
            );
        }
    }
    pairs
}

/// Add one rendered block's auxiliary pairs to the chunk buffers at `offset`.
fn accumulate_aux(
    output: &SourceOutput,
    delays: &mut [VecDeque<f32>],
    aux_buffers: &mut HashMap<(usize, usize), Vec<f32>>,
    track: usize,
    offset: usize,
) {
    for ((pair, aux), delay) in output.aux().zip(delays.iter_mut()) {
        let Some(bus) = aux_buffers.get_mut(&(track, pair)) else {
            continue;
        };
        let start = offset.min(bus.len());
        let frames = aux.len().min(bus.len() - start);
        let bus = &mut bus[start..start + frames];
        if delay.is_empty() {
            for (out, sample) in bus.iter_mut().zip(&aux[..frames]) {
                *out += sample;
            }
        } else {
            for (out, sample) in bus.iter_mut().zip(&aux[..frames]) {
                delay.push_back(*sample);
                *out += delay.pop_front().unwrap();
            }
        }
    }
}

/// Instantiate the sources for pruned lanes. Runs on the source worker so a
/// slow preset import never stalls the producer thread.
fn build_lanes(
    project: &Project,
    tracks: &[usize],
    soundfont_path: &std::path::Path,
    sample_rate: u32,
    instrument_instantiations: &AtomicU64,
    root: Option<&std::path::Path>,
    worker_options: oc_engine::render_parallel::RenderWorkerOptions,
) -> Result<Vec<BuiltLane>, String> {
    let settings = InterpretSettings {
        sample_rate,
        ..Default::default()
    };
    let (lane_project, lane_targets) = split_source_lanes(project)?;
    let mut lanes = Vec::new();
    for (lane, &target) in lane_project.tracks.iter().zip(&lane_targets) {
        if !tracks.contains(&target) {
            continue;
        }
        let mut lane = lane.clone();
        lane.mix.volume_db = 0.0;
        lane.mix.pan = 0.0;
        lane.mix.mute = false;
        let mut single = lane_project.clone();
        single.tracks = vec![lane.clone()];
        let events = interpret::try_interpret(&single, &settings)?;
        let has_musical = events.iter().any(|event| is_musical_event(&event.event));
        let preset = project
            .presets
            .iter()
            .find(|p| Some(&p.name) == lane.preset.as_ref());
        let params = oc_engine::preset::builtin_track_params_at(project, preset, &lane, root);
        let mut chain = oc_engine::source_lanes::effect_chain(project, &lane, sample_rate)?;
        chain.set_streaming_idle_gate(true);
        let mut source = if clip_only_lane(project, &lane, has_musical) {
            BlockSource::Silent
        } else {
            instrument_instantiations.fetch_add(1, Ordering::Relaxed);
            let messages = events.iter().map(|event| &event.event).collect::<Vec<_>>();
            BlockSource::new_streaming(
                preset,
                &params,
                soundfont_path,
                sample_rate,
                project.properties.tempo,
                root,
                oc_engine::block_source::StreamingSourcePreparation {
                    events: &messages,
                    workers: worker_options,
                },
            )
            .map_err(|e| format!("Failed to create source for {}: {e}", lane.name))?
        };
        // The splice moves the lane's clock to the position it joins at.
        source.install_hairpin_layers(preset, project, &lane, &project.properties, 0)?;
        lanes.push(BuiltLane {
            target,
            source: SmartSource::new(source, sample_rate, lane.mix.smart_disable),
            chain,
            events,
            spec: LaneSpec {
                preset: preset.cloned(),
                params: params.clone(),
                performance: lane.performance.is_some(),
                smart_disable: lane.mix.smart_disable,
            },
        });
    }
    Ok(lanes)
}

/// Long-lived worker that builds hot-added lanes without touching the producer.
struct SourceBuilder {
    requests: Receiver<SourceBuildRequest>,
    cmd_tx: Sender<RenderCommand>,
    shutdown: Arc<AtomicBool>,
    /// Test-only delay that prices a debug instrument build.
    #[cfg(test)]
    delay_ms: Arc<AtomicU64>,
}

impl SourceBuilder {
    fn run(self) {
        while let Ok(request) = self.requests.recv() {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            #[cfg(test)]
            let delay = self.delay_ms.load(Ordering::Acquire);
            #[cfg(test)]
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            let result = build_lanes(
                &request.project,
                &request.tracks,
                &request.soundfont_path,
                request.sample_rate,
                &request.instrument_instantiations,
                request.root.as_deref(),
                request.worker_options,
            );
            let command = match result {
                Ok(lanes) => RenderCommand::SourcesBuilt {
                    epoch: request.epoch,
                    lanes,
                },
                Err(error) => RenderCommand::SourceBuildFailed {
                    epoch: request.epoch,
                    error,
                },
            };
            if self.cmd_tx.send(command).is_err() {
                break;
            }
        }
    }
}

fn resolve_spectrum_target(target: &str) -> Option<oc_engine::spectrum::SpectrumTarget> {
    use oc_engine::spectrum::SpectrumTarget;
    if target == "master" {
        return Some(SpectrumTarget::Master);
    }
    if let Some(name) = target.strip_prefix("bus:") {
        return Some(SpectrumTarget::Bus(name.to_string()));
    }
    if let Some(index) = target.strip_prefix("track:") {
        return index.parse().ok().map(SpectrumTarget::Track);
    }
    None
}

fn seek_or_fail(state: &mut RenderState, playback: &StreamingPlayback, frame: u64) -> bool {
    match state.seek_to(frame) {
        Ok(()) => true,
        Err(error) => {
            fail_playback(state, playback, error);
            false
        }
    }
}

/// Drop all playable state after a load failure and publish the reason.
fn fail_playback(state: &mut RenderState, playback: &StreamingPlayback, error: String) {
    let meter_hub = state.meter_hub.take();
    let instantiations = Arc::clone(&state.instrument_instantiations);
    let chain_latency = Arc::clone(&state.chain_latency_frames);
    chain_latency.store(0, Ordering::Release);
    let next_epoch = state.source_epoch.wrapping_add(1);
    *state = RenderState::new(state.sample_rate);
    state.source_epoch = next_epoch;
    state.meter_hub = meter_hub;
    state.instrument_instantiations = instantiations;
    state.chain_latency_frames = chain_latency;
    playback.active.store(false, Ordering::Release);
    playback.project_loaded.store(false, Ordering::Release);
    state.publish_ends(playback);
    playback.active_sources.store(0, Ordering::Release);
    playback.meters.clear();
    playback.meters.set_unavailable(error.clone());
    *playback.load_error.lock().unwrap() = Some(error);
    invalidate_playback_buffer(playback);
}

fn invalidate_playback_buffer(playback: &StreamingPlayback) {
    playback.warm_state.store(WARM_IDLE, Ordering::Release);
    playback.ready.store(false, Ordering::Release);
    playback.waiting_for_clip.store(false, Ordering::Release);
    playback.seek_generation.fetch_add(1, Ordering::Release);
}

fn wait_for_clip(state: &RenderState, playback: &StreamingPlayback) -> bool {
    playback
        .waiting_for_clip
        .store(state.waiting_for_clip, Ordering::Release);
    if state.waiting_for_clip {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    state.waiting_for_clip || state.render_error.is_some()
}

/// Copy one interleaved chunk into the ring, tolerating a wrap.
fn write_ring_chunk(producer: &mut rtrb::Producer<f32>, chunk: &[f32]) {
    if let Ok(mut write_chunk) = producer.write_chunk(chunk.len()) {
        let (first, second) = write_chunk.as_mut_slices();
        let first_len = first.len().min(chunk.len());
        first[..first_len].copy_from_slice(&chunk[..first_len]);
        if first_len < chunk.len() {
            let remaining = chunk.len() - first_len;
            second[..remaining].copy_from_slice(&chunk[first_len..]);
        }
        write_chunk.commit_all();
    }
}

/// Fold interleaved stereo to mono in place. Each frame becomes the average of
/// its two channels, so a correlated pair keeps its level and a hard-panned
/// source drops 6 dB, which is what a mono check is for.
fn fold_to_mono(buffer: &mut [f32]) {
    for frame in buffer.chunks_exact_mut(2) {
        let mono = 0.5 * (frame[0] + frame[1]);
        frame[0] = mono;
        frame[1] = mono;
    }
}

/// Sample peak of an interleaved stereo buffer; non-finite samples are ignored.
fn peak_of(buffer: &[f32]) -> f32 {
    let mut peak = 0.0f32;
    for sample in buffer {
        if sample.is_finite() {
            peak = peak.max(sample.abs());
        }
    }
    peak.min(16.0)
}

/// Publish the latency of the live pre-master and master chains.
fn publish_chain_latency(cell: &AtomicU64, graph: &AudioGraph) {
    let latency = u64::from(graph.pre_master_latency_samples())
        + u64::from(graph.master_chain.latency_samples());
    cell.store(latency, Ordering::Release);
}

/// Main render loop — runs on the background thread.
fn render_loop(
    cmd_rx: Receiver<RenderCommand>,
    mut producer: rtrb::Producer<f32>,
    playback: Arc<StreamingPlayback>,
    sample_rate: u32,
    shutdown: Arc<AtomicBool>,
    build_tx: Sender<SourceBuildRequest>,
) {
    let mut state = RenderState::new(sample_rate);
    let mut chunk = Vec::with_capacity(CHUNK_FRAMES * 2);
    state.meter_hub = Some(Arc::clone(&playback.meters));
    state.instrument_instantiations = Arc::clone(&playback.instrument_instantiations);
    state.chain_latency_frames = Arc::clone(&playback.chain_latency_frames);
    #[cfg(test)]
    {
        state.sounding_voices = Arc::clone(&playback.sounding_voices);
    }
    state.touch = Some(Arc::clone(&playback.touch));
    // Reset whenever a seek generation has not been acknowledged for this long.
    let mut ack_wait_started: Option<std::time::Instant> = None;
    // Loop generation already applied; a newer one invalidates the queued pass.
    let mut loop_generation = playback.loop_generation.load(Ordering::Acquire);
    #[cfg(test)]
    let mut warm_profile: Option<std::time::Instant> = None;
    #[cfg(test)]
    let mut warm_first: Option<std::time::Instant> = None;
    // No consumer to acknowledge: stage in the DSP queue and publish on Play.
    let mut warm_stage = false;

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if state.processing_failed() {
            state.processing_fault = true;
            state.playing = false;
            state.paused = false;
            state.seek_target = None;
            state.warm_fill = false;
            state.rendered_ahead.clear();
            state.clear_pending_meters();
            playback.active.store(false, Ordering::Release);
            invalidate_playback_buffer(&playback);
        }
        // The monitor fold follows the hub, so a toggle takes effect on the
        // next chunk and reports back through `mono.applied`.
        state.mono_check = playback.meters.mono_requested();
        // Meters reach the UI only once the device has reached their audio.
        state.release_meters(
            playback.play_cursor_frames.load(Ordering::Acquire),
            playback.device_buffer_frames.load(Ordering::Acquire),
        );
        if let Some(error) = state.take_processing_error() {
            log::error!("[render_thread] Processor failed: {error}");
            fail_playback(&mut state, &playback, error);
            continue;
        }
        if let Some(error) = state.render_error.take() {
            log::error!("[render_thread] Render failed: {error}");
            state.playing = false;
            state.seek_target = None;
            state.warm_fill = false;
            playback.warm_state.store(WARM_IDLE, Ordering::Release);
            playback.active.store(false, Ordering::Release);
            *playback.load_error.lock().unwrap() = Some(error);
        }
        // Check for commands (non-blocking when playing, blocking when idle)
        let cmd = if state.playing
            || state.seek_target.is_some()
            || state.warm_fill
            || state.processing_fault
        {
            cmd_rx.try_recv().ok()
        } else {
            // Block until a command arrives (saves CPU when idle)
            match cmd_rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => break, // Channel closed
            }
        };
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // A loop toggle arrives as a generation, not as a command: it has to
        // discard the pass already queued (the ring can hold the head of a
        // wrapped pass) and rebuild from where the audio is. Applied before
        // the command below, so a press that starts playback is never rewound.
        let requested_loop = playback.loop_generation.load(Ordering::Acquire);
        if requested_loop != loop_generation {
            loop_generation = requested_loop;
            if state.playing && !state.warm_fill && state.seek_target.is_none() {
                let audible = playback.audible_frames();
                if !seek_or_fail(&mut state, &playback, audible) {
                    continue;
                }
                state.ring_prefilled = false;
                // Flush the previous generation before this one is written.
                invalidate_playback_buffer(&playback);
            }
        }

        if let Some(cmd) = cmd {
            // Ack on every exit (including rejected requests), after command
            // state/ring invalidation is visible. A newer Pause still owns its
            // independent output gate and cannot be undone by this ack.
            struct PositionAck<'a>(Option<&'a AtomicU64>);
            impl Drop for PositionAck<'_> {
                fn drop(&mut self) {
                    if let Some(applied) = self.0 {
                        applied.fetch_add(1, Ordering::Release);
                    }
                }
            }
            let _position_ack = PositionAck(
                matches!(
                    &cmd,
                    RenderCommand::Play { .. } | RenderCommand::Seek { .. }
                )
                .then_some(&playback.position_applied),
            );
            match cmd {
                #[cfg(test)]
                RenderCommand::TestBarrier { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                RenderCommand::Play {
                    start_frame,
                    resume,
                    end_frame,
                } => {
                    if state.project.is_none() {
                        log::warn!("[render_thread] No project loaded, ignoring Play");
                        continue;
                    }
                    // A preview bound covers this run and is cleared by the next Play.
                    state.play_end_frame = end_frame;
                    state.publish_ends(&playback);
                    // Any press ends the staging phase; playback owns the queue.
                    state.warm_fill = false;
                    warm_stage = false;
                    if playback.warm_state.load(Ordering::Acquire) == WARM_BUILDING {
                        playback.warm_state.store(WARM_IDLE, Ordering::Release);
                    }
                    // The warm ring already holds audio for this exact position:
                    // the press only enables the consumer, it does not render.
                    // An explicit seek to that same position needs no flush.
                    let count_in_pending = state.armed_count_in.is_some();
                    let warm_ring = !count_in_pending
                        && state.play_end_frame.is_none()
                        && state.ring_prefilled
                        && state.warm_ring_start_frame == start_frame
                        && playback.callback_generation.load(Ordering::Acquire)
                            == playback.seek_generation.load(Ordering::Acquire);
                    if warm_ring {
                        state.ring_prefilled = false;
                        state.paused = false;
                        state.seek_target = None;
                        state.start_playing();
                        playback.warm_state.store(WARM_READY, Ordering::Release);
                        playback.ready.store(true, Ordering::Release);
                        playback.publish_play_run(start_frame);
                        playback.active.store(true, Ordering::Release);
                        continue;
                    }
                    if !count_in_pending && resume && state.paused && state.seek_target.is_none() {
                        state.paused = false;
                        state.start_playing();
                        playback.publish_play_run(playback.timeline_frames());
                        playback.active.store(true, Ordering::Release);
                        continue;
                    }
                    let start_frame = start_frame.min(state.timeline_end());
                    state.paused = false;
                    // A fresh play starts with the count-in armed since the
                    // press; a resume keeps the run's mapping. Either way the
                    // count-in frames are not part of the timeline cursor.
                    let count_in = match state.armed_count_in.take() {
                        Some(plan) => {
                            let frames = plan.frames;
                            // This Play invalidates the ring, so any count-in
                            // rendered before it is gone. Sound the whole plan
                            // again from a clean click.
                            state.click.reset();
                            state.completed_count_in = None;
                            state.count_in = Some(CountInState { plan, emitted: 0 });
                            frames
                        }
                        None => {
                            state.count_in = None;
                            0
                        }
                    };
                    if state.seek_target.unwrap_or(state.render_cursor) != start_frame
                        && !seek_or_fail(&mut state, &playback, start_frame)
                    {
                        continue;
                    }
                    state.start_playing();
                    state.ring_prefilled = false;
                    // Invalidate before enabling output so a running callback
                    // flushes stale audio instead of playing it.
                    playback.active.store(false, Ordering::Release);
                    invalidate_playback_buffer(&playback);
                    playback
                        .play_cursor_frames
                        .store(start_frame, Ordering::Release);
                    // Publish the count-in only after the flush, so a callback
                    // cannot spend the new count-in on the previous ring.
                    playback
                        .count_in_start
                        .store(start_frame, Ordering::Release);
                    playback.count_in_frames.store(count_in, Ordering::Release);
                    playback
                        .count_in_remaining
                        .store(count_in, Ordering::Release);
                    playback.publish_play_run(start_frame);
                    playback.active.store(true, Ordering::Release);
                }
                RenderCommand::Click { settings } => {
                    state.click_settings = settings;
                    if !settings.enabled {
                        state.click.reset();
                    }
                }
                RenderCommand::CountIn { plan } => {
                    state.click.reset();
                    state.armed_count_in = (plan.frames > 0).then(|| plan.clone());
                    state.count_in = (plan.frames > 0).then_some(CountInState {
                        plan: CountInPlan {
                            frames: plan.frames,
                            events: plan.events,
                        },
                        emitted: 0,
                    });
                    playback
                        .count_in_frames
                        .store(plan.frames, Ordering::Release);
                    playback
                        .count_in_remaining
                        .store(plan.frames, Ordering::Release);
                }
                RenderCommand::Pause => {
                    state.playing = false;
                    state.update_preroll_deadline();
                    state.paused = true;
                    state.count_in = None;
                    // Keep the count-in cursor mapping: the ring still holds
                    // the count-in, and a resume must not count it as timeline.
                    state.armed_count_in = None;
                    playback.active.store(false, Ordering::Release);
                    state.loudness.reset();
                    state.correlation.reset();
                }
                RenderCommand::Seek { frame } => {
                    // A seek to the opening frame already staged by warm-up
                    // can keep that lookahead for the first Play.
                    if !state.playing
                        && (state.warm_fill || state.ring_prefilled)
                        && frame == state.warm_ring_start_frame
                    {
                        continue;
                    }
                    if !seek_or_fail(&mut state, &playback, frame) {
                        continue;
                    }
                    // The opening lookahead belongs to the previous position.
                    state.warm_fill = false;
                    warm_stage = false;
                    state.count_in = None;
                    state.armed_count_in = None;
                    playback.count_in_frames.store(0, Ordering::Release);
                    playback.count_in_remaining.store(0, Ordering::Release);
                    state.ring_prefilled = false;
                    playback
                        .play_cursor_frames
                        .store(frame.min(state.timeline_end()), Ordering::Release);
                    invalidate_playback_buffer(&playback);
                }
                RenderCommand::Stop => {
                    state.playing = false;
                    state.paused = false;
                    // Reset lazily on next Play; a subsequent project replacement
                    // must not pay for reloading the discarded source graph first.
                    state.seek_target = None;
                    state.preroll_deadline = None;
                    state.ring_prefilled = false;
                    state.count_in = None;
                    state.armed_count_in = None;
                    playback.count_in_frames.store(0, Ordering::Release);
                    playback.count_in_remaining.store(0, Ordering::Release);
                    playback.active.store(false, Ordering::Release);
                    playback.play_cursor_frames.store(0, Ordering::Release);
                    invalidate_playback_buffer(&playback);
                    state.clear_pending_meters();
                    state.loudness.reset();
                    state.correlation.reset();
                    playback.meters.clear();
                }
                RenderCommand::ProjectChanged {
                    project,
                    soundfont_path,
                    root,
                    warm,
                } => {
                    let was_playing = state.playing;
                    let current_frame = playback.play_cursor_frames.load(Ordering::Acquire);
                    playback.project_loaded.store(false, Ordering::Release);
                    playback.project_loads.fetch_add(1, Ordering::Relaxed);
                    let prepare_from = if was_playing { current_frame } else { 0 };
                    let loaded =
                        state.load_project_from(&project, soundfont_path, root, prepare_from);
                    if let Err(error) = loaded {
                        log::error!("[render_thread] Project load failed: {error}");
                        fail_playback(&mut state, &playback, error);
                        continue;
                    }
                    state.playing = was_playing;
                    state.clear_pending_meters();
                    // Already a fresh state; no second SoundFont reload needed.
                    state.seek_target = (was_playing && current_frame > 0)
                        .then_some(current_frame.min(state.total_frames));
                    invalidate_playback_buffer(&playback);
                    state.publish_ends(&playback);
                    playback.project_loaded.store(true, Ordering::Release);
                    playback
                        .active_sources
                        .store(state.renderers.len() as u64, Ordering::Release);
                    // Warm only a stopped project: a playing project is filled by
                    // the consumer-driven loop and must not preroll twice.
                    if warm && !was_playing {
                        state.warm_fill = true;
                        warm_stage = false;
                        playback.warm_state.store(WARM_BUILDING, Ordering::Release);
                        #[cfg(test)]
                        {
                            warm_profile = Some(std::time::Instant::now());
                            warm_first = None;
                        }
                    }
                    log::info!(
                        "[render_thread] Project loaded: {} tracks, {:.1}s duration",
                        state.num_tracks,
                        state.total_frames as f64 / sample_rate as f64
                    );
                }
                RenderCommand::MixerChanged {
                    project,
                    soundfont_path,
                    requested_ns,
                } => {
                    let was_playing = state.playing;
                    let current_frame = playback.play_cursor_frames.load(Ordering::Acquire);
                    match state.apply_mixer(&project, soundfont_path) {
                        Ok(MixerOutcome::Patched) => {
                            state.update_drain_frames();
                            state.publish_ends(&playback);
                        }
                        Ok(MixerOutcome::Reloaded) => {
                            state.playing = was_playing;
                            state.clear_pending_meters();
                            state.seek_target = (current_frame > 0)
                                .then_some(current_frame.min(state.timeline_end()));
                            invalidate_playback_buffer(&playback);
                            state.publish_ends(&playback);
                            playback.project_loaded.store(true, Ordering::Release);
                            playback
                                .active_sources
                                .store(state.renderers.len() as u64, Ordering::Release);
                        }
                        Ok(MixerOutcome::NeedsSources { tracks }) => {
                            // The instruments are built on the source worker; this
                            // thread keeps producing the lanes it already owns.
                            playback
                                .first_block
                                .begin_at(requested_ns, StartPath::LaneUnmute);
                            state.publish_ends(&playback);
                            if let Some(project) = state.project.clone() {
                                let _ = build_tx.send(SourceBuildRequest {
                                    epoch: state.source_epoch,
                                    worker_options: state.worker_options,
                                    project,
                                    tracks,
                                    soundfont_path: state.soundfont_path.clone(),
                                    sample_rate,
                                    instrument_instantiations: Arc::clone(
                                        &state.instrument_instantiations,
                                    ),
                                    root: state.root.clone(),
                                });
                            }
                        }
                        Err(error) => {
                            log::error!("[render_thread] Mixer change failed: {error}");
                            fail_playback(&mut state, &playback, error);
                        }
                    }
                }
                RenderCommand::ChainChanged {
                    project,
                    soundfont_path,
                } => {
                    let was_playing = state.playing;
                    let current_frame = playback.play_cursor_frames.load(Ordering::Acquire);
                    match state.apply_chain_change(&project, soundfont_path) {
                        Ok(ChainOutcome::Patched { chains }) => {
                            if let Some(graph) = state.graph.as_ref() {
                                publish_chain_latency(&state.chain_latency_frames, graph);
                            }
                            // Audio staged while stopped was rendered with the
                            // previous chain; the next press must render with
                            // this one.
                            if !was_playing && !state.paused {
                                state.discard_staged_audio(&playback);
                            }
                            state.update_drain_frames();
                            state.publish_ends(&playback);
                            playback.chain_patches.fetch_add(chains, Ordering::Relaxed);
                        }
                        Ok(ChainOutcome::Reloaded) => {
                            state.playing = was_playing;
                            state.clear_pending_meters();
                            state.seek_target = (current_frame > 0)
                                .then_some(current_frame.min(state.timeline_end()));
                            invalidate_playback_buffer(&playback);
                            state.publish_ends(&playback);
                            playback.project_loaded.store(true, Ordering::Release);
                            playback
                                .active_sources
                                .store(state.renderers.len() as u64, Ordering::Release);
                        }
                        Err(error) => {
                            log::error!("[render_thread] Chain change failed: {error}");
                            fail_playback(&mut state, &playback, error);
                        }
                    }
                    playback.chain_applied.fetch_add(1, Ordering::Release);
                }
                RenderCommand::SourcesBuilt { epoch, lanes } => {
                    if epoch != state.source_epoch {
                        log::debug!("[render_thread] Dropping a stale source build");
                        continue;
                    }
                    if let Err(error) = state.splice_sources(lanes) {
                        log::error!("[render_thread] Hot source splice failed: {error}");
                        fail_playback(&mut state, &playback, error);
                        continue;
                    }
                    playback
                        .active_sources
                        .store(state.renderers.len() as u64, Ordering::Release);
                    state.publish_ends(&playback);
                    // The rebuilt lane is part of the mix from here on.
                    playback.first_block.mark_spliced();
                }
                RenderCommand::SourceBuildFailed { epoch, error } => {
                    if epoch != state.source_epoch {
                        continue;
                    }
                    log::error!("[render_thread] Hot source build failed: {error}");
                    fail_playback(&mut state, &playback, error);
                }
                RenderCommand::Spectrum { target } => {
                    state.set_spectrum_target(target);
                    if state.spectrum_target.is_none() {
                        playback.meters.clear_spectrum();
                    }
                }
                RenderCommand::Shutdown => {
                    break;
                }
            }
        }

        if state.processing_fault {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }

        if state.warm_fill {
            // Publish the opening lookahead straight into the ring while the
            // consumer is idle, so a press only has to enable output.
            let acked = playback.callback_generation.load(Ordering::Acquire)
                == playback.seek_generation.load(Ordering::Acquire);
            if !acked && !warm_stage {
                // A running consumer flushes the previous generation first.
                let waited = ack_wait_started.get_or_insert_with(std::time::Instant::now);
                if waited.elapsed() < ACK_TIMEOUT {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                // No consumer at all: keep the old staging path for the press.
                warm_stage = true;
            }
            ack_wait_started = None;
            let queued = if warm_stage {
                state.rendered_ahead.len() / 2
            } else {
                RING_BUFFER_FRAMES
                    .saturating_sub(producer.slots() / 2)
                    .min(LOOKAHEAD_FRAMES)
            };
            let remaining = if warm_stage {
                state.playback_end().saturating_sub(state.dsp_cursor)
            } else {
                state.playback_end().saturating_sub(state.render_cursor)
            } as usize;
            let mut frames = CHUNK_FRAMES
                .min(LOOKAHEAD_FRAMES.saturating_sub(queued))
                .min(remaining);
            // Warm only up to the loop end, so the first press does not hear
            // audio the loop would never play.
            if let Some(region) = playback.loop_region() {
                let cursor = if warm_stage {
                    state.dsp_cursor
                } else {
                    state.render_cursor
                };
                let end = region.end.min(state.timeline_end());
                if end > cursor {
                    frames = frames.min((end - cursor) as usize);
                } else {
                    frames = 0;
                }
            }
            if frames == 0 || (!warm_stage && producer.slots() < frames * 2) {
                state.warm_fill = false;
                playback.warm_state.store(WARM_READY, Ordering::Release);
                #[cfg(test)]
                if let Some(start) = warm_profile {
                    crate::profile_phase("warm_total", start);
                }
                continue;
            }
            if !warm_stage && !state.ring_prefilled {
                state.warm_ring_start_frame = state.render_cursor;
            }
            if warm_stage {
                state.render_dsp_chunk_into(frames, false, &mut chunk);
            } else {
                state.render_chunk_into(frames, &mut chunk);
            }
            if wait_for_clip(&state, &playback) {
                continue;
            }
            if chunk.is_empty() {
                state.warm_fill = false;
                playback.warm_state.store(WARM_READY, Ordering::Release);
                #[cfg(test)]
                if let Some(start) = warm_profile {
                    crate::profile_phase("warm_total", start);
                }
                continue;
            }
            if warm_stage {
                state.rendered_ahead.extend(chunk.iter().copied());
            } else {
                playback.first_block.add_rendered((chunk.len() / 2) as u64);
                write_ring_chunk(&mut producer, &chunk);
                state.ring_prefilled = true;
                playback.ready.store(true, Ordering::Release);
            }
            #[cfg(test)]
            if warm_first.is_none() {
                warm_first = Some(std::time::Instant::now());
                if let Some(start) = warm_profile {
                    crate::profile_phase("warm_first_chunk", start);
                }
            }
            continue;
        }

        if state.seek_target.is_some() {
            if state.preroll_expired() {
                state.abandon_preroll();
                continue;
            }
            let computed = state.preroll_chunk();
            if wait_for_clip(&state, &playback) {
                continue;
            }
            playback.first_block.add_rendered(computed);
            continue; // Recheck transport commands before another chunk.
        }

        // The count-in sounds before the timeline: click only, playhead still.
        if state.playing && state.count_in.is_some() {
            let frames = CHUNK_FRAMES;
            if producer.slots() < frames * 2 {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            state.render_count_in_chunk_into(frames, &mut chunk);
            if !chunk.is_empty() {
                playback.first_block.add_rendered((chunk.len() / 2) as u64);
                write_ring_chunk(&mut producer, &chunk);
                playback.ready.store(true, Ordering::Release);
            }
            continue;
        }

        // Session loop: never render past the region end, then rebuild the pass
        // from its start. The rebuild runs while the ring still holds the tail
        // of the pass just rendered, so the seam is a fade instead of a callback
        // of silence; the playhead wraps where that audio does, not here.
        if state.playing
            && state.play_end_frame.is_none()
            && let Some(target) = playback.loop_wrap(state.render_cursor, state.timeline_end())
        {
            if let Some(region) = playback.armed_loop_region() {
                playback.publish_loop_seam(Some(LoopRegion {
                    start: region.start,
                    end: region.end.min(state.timeline_end()),
                }));
            }
            if !seek_or_fail(&mut state, &playback, target) {
                continue;
            }
            state.loop_seam_fade = crate::audio::RESUME_FADE_FRAMES;
            continue;
        }

        // Render ahead if playing and the ring buffer still has room
        if state.playing && state.render_cursor < state.playback_end() {
            // Only the callback owns the consumer, so wait for it to discard the
            // previous generation before publishing. The wait is bounded: a
            // stream that never acks must not wedge the producer.
            if playback.callback_generation.load(Ordering::Acquire)
                != playback.seek_generation.load(Ordering::Acquire)
            {
                let waited = ack_wait_started.get_or_insert_with(std::time::Instant::now);
                if waited.elapsed() < ACK_TIMEOUT {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
            } else {
                ack_wait_started = None;
            }

            // Keep a bounded lookahead: the ring is capacity, not a fill target,
            // so a mixer edit is heard within one window instead of two seconds.
            let queued_samples = RING_BUFFER_FRAMES * 2 - producer.slots();
            let first = !playback.ready.load(Ordering::Acquire);
            let mut frames = if first {
                FIRST_CHUNK_FRAMES
            } else {
                CHUNK_FRAMES
            };
            // A chunk that crosses the region end is shortened, so no audio
            // past the loop point is ever rendered or queued.
            if state.play_end_frame.is_none()
                && let Some(region) = playback.loop_region()
            {
                let end = region.end.min(state.timeline_end());
                if end > state.render_cursor {
                    frames = frames.min((end - state.render_cursor) as usize);
                }
            }
            let chunk_stereo_samples = frames * 2;

            // The chunk that opens a wrapped pass goes into the ring even when
            // the lookahead is full: the pass behind the seam is what keeps the
            // callback fed until this chunk is ready.
            let seam_head = state.loop_seam_fade > 0;
            if (seam_head || queued_samples + chunk_stereo_samples <= LOOKAHEAD_FRAMES * 2)
                && producer.slots() >= chunk_stereo_samples
            {
                state.render_chunk_into(frames, &mut chunk);
                if wait_for_clip(&state, &playback) {
                    continue;
                }
                fade_closed_loop_pass(
                    &mut chunk,
                    state.render_cursor,
                    state.timeline_end(),
                    playback.armed_loop_region(),
                    state.play_end_frame.is_some(),
                );
                if state.loop_seam_fade > 0 {
                    let frames = chunk.len() / 2;
                    crate::audio::fade_in_head(&mut chunk, frames, &mut state.loop_seam_fade);
                }

                if chunk.is_empty() {
                    // Reached end of project
                    state.playing = false;
                    playback.active.store(false, Ordering::Release);
                    state.clear_pending_meters();
                    playback.meters.clear();
                    continue;
                }

                playback.first_block.add_rendered((chunk.len() / 2) as u64);

                // Write chunk to ring buffer
                write_ring_chunk(&mut producer, &chunk);

                // Signal ready after first chunk
                if !playback.ready.load(Ordering::Acquire) {
                    playback.ready.store(true, Ordering::Release);
                }
            } else {
                // Lookahead is full — sleep briefly to avoid busy-waiting
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else if state.playing && state.render_cursor >= state.playback_end() {
            // Finished rendering — wait for audio callback to drain the buffer
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    log::info!("[render_thread] Shutdown complete");
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "render_thread/source_lane_tests.rs"]
mod source_lane_tests;

#[cfg(test)]
#[path = "render_thread/owned_touch_tests.rs"]
mod owned_touch_tests;
