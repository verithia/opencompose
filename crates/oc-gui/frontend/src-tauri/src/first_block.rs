//! Press-to-first-block measurement for every path that starts audio.
//!
//! One `FirstBlock` lives on every playback object. The command thread stamps
//! it when it starts audio, the producer counts the frames it renders, and the
//! device sink stamps the first block it hands out. Every field is an atomic,
//! so the audio callback only reads and writes counters: no allocation, no
//! lock, no thread spawn.
//!
//! The numbers answer two questions per path: how long the first block took,
//! and how much audio the producer had to compute before it could leave. A
//! streaming path computes at most the lookahead window; a path that renders a
//! range first shows the whole range in `frames_computed`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

/// The contract every start path is measured against: the first block reaches
/// the device within this many milliseconds of the press.
pub const FIRST_BLOCK_BUDGET_MS: f64 = 50.0;

/// Monotonic origin shared by every measurement in this process.
fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Nanoseconds since the process origin. Monotonic on every platform we ship.
pub fn now_ns() -> u64 {
    // Zero is reserved for an unarmed trace, including on coarse clocks where
    // the first elapsed reading is exactly zero.
    u64::try_from(origin().elapsed().as_nanos())
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1)
}

fn millis(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

/// What started the audio. The tag is what the harness reports per path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StartPath {
    Play = 0,
    Resume = 1,
    Seek = 2,
    TrackPreview = 3,
    PresetAudition = 4,
    PatternPreview = 5,
    ClipPreview = 6,
    DrumPad = 7,
    RecordAudio = 8,
    RecordMidi = 9,
    /// A pruned lane is being rebuilt after an unmute.
    LaneUnmute = 10,
    /// A press that had to rebuild the project graph first.
    Rebuild = 11,
    /// A press with no audio requested yet: the stream opening path.
    StreamOpen = 12,
}

impl StartPath {
    /// Wire label for the start path.
    pub fn as_str(self) -> &'static str {
        match self {
            StartPath::Play => "play",
            StartPath::Resume => "resume",
            StartPath::Seek => "seek",
            StartPath::TrackPreview => "track_preview",
            StartPath::PresetAudition => "preset_audition",
            StartPath::PatternPreview => "pattern_preview",
            StartPath::ClipPreview => "clip_preview",
            StartPath::DrumPad => "drum_pad",
            StartPath::RecordAudio => "record_audio",
            StartPath::RecordMidi => "record_midi",
            StartPath::LaneUnmute => "lane_unmute",
            StartPath::Rebuild => "rebuild",
            StartPath::StreamOpen => "stream_open",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => StartPath::Resume,
            2 => StartPath::Seek,
            3 => StartPath::TrackPreview,
            4 => StartPath::PresetAudition,
            5 => StartPath::PatternPreview,
            6 => StartPath::ClipPreview,
            7 => StartPath::DrumPad,
            8 => StartPath::RecordAudio,
            9 => StartPath::RecordMidi,
            10 => StartPath::LaneUnmute,
            11 => StartPath::Rebuild,
            12 => StartPath::StreamOpen,
            _ => StartPath::Play,
        }
    }
}

/// One start's numbers, as a debug snapshot reads them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FirstBlockSnapshot {
    pub path: String,
    /// Monotonic id of the start this snapshot describes.
    pub seq: u64,
    /// The first block has reached the device sink.
    pub delivered: bool,
    /// Milliseconds from the press to that block.
    pub latency_ms: Option<f64>,
    /// Milliseconds since the press; grows while a start is still waiting.
    pub since_start_ms: f64,
    /// Device block size of the first delivered block, in frames.
    pub block_frames: u64,
    /// Frames the producer rendered before that block left.
    pub frames_computed: u64,
    /// `frames_computed / block_frames`, the streaming bound in blocks.
    pub blocks_computed: f64,
    /// Frames already queued in the ring when the block left.
    pub queued_frames: u64,
    /// Milliseconds from the press to the lane splice, for `lane_unmute`.
    pub splice_ms: Option<f64>,
}

/// The press-to-first-block trace for one playback generation.
///
/// The audio callback is the only writer of the delivery fields and the
/// producer is the only writer of the frame counters; both use atomics.
pub struct FirstBlock {
    seq: AtomicU64,
    path: AtomicU8,
    start_ns: AtomicU64,
    delivered_ns: AtomicU64,
    block_frames: AtomicU64,
    queued_frames: AtomicU64,
    frames_computed: AtomicU64,
    rendered_frames: AtomicU64,
    /// Delivery only counts once the rebuilt lane has been spliced in.
    splice_gated: AtomicBool,
    spliced_ns: AtomicU64,
    /// One debug line per start, written by the transport poll.
    reported: AtomicBool,
}

impl Default for FirstBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl FirstBlock {
    /// Create an empty first-block recorder.
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            path: AtomicU8::new(StartPath::Play as u8),
            start_ns: AtomicU64::new(0),
            delivered_ns: AtomicU64::new(0),
            block_frames: AtomicU64::new(0),
            queued_frames: AtomicU64::new(0),
            frames_computed: AtomicU64::new(0),
            rendered_frames: AtomicU64::new(0),
            splice_gated: AtomicBool::new(false),
            spliced_ns: AtomicU64::new(0),
            reported: AtomicBool::new(true),
        }
    }

    /// Stamp a start at an already-measured instant and return its sequence.
    ///
    /// `started_ns` comes from the command that starts audio, so the measured
    /// latency includes everything between the press and the device.
    pub fn begin_at(&self, started_ns: u64, path: StartPath) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.path.store(path as u8, Ordering::Relaxed);
        self.rendered_frames.store(0, Ordering::Relaxed);
        self.frames_computed.store(0, Ordering::Relaxed);
        self.block_frames.store(0, Ordering::Relaxed);
        self.queued_frames.store(0, Ordering::Relaxed);
        self.spliced_ns.store(0, Ordering::Relaxed);
        self.splice_gated
            .store(path == StartPath::LaneUnmute, Ordering::Relaxed);
        self.reported.store(false, Ordering::Relaxed);
        self.start_ns.store(started_ns.max(1), Ordering::Relaxed);
        self.delivered_ns.store(0, Ordering::Release);
        seq
    }

    /// Count frames the producer rendered for this start.
    pub fn add_rendered(&self, frames: u64) {
        self.rendered_frames.fetch_add(frames, Ordering::Relaxed);
    }

    /// Mark that the rebuilt lane is now part of the mix.
    pub fn mark_spliced(&self) {
        self.spliced_ns.store(now_ns(), Ordering::Release);
    }

    /// Record the first block the device sink received. Called from the audio
    /// callback, so this only touches atomics.
    pub fn observe(&self, frames_read: u64, block_frames: u64, queued_frames: u64) {
        if frames_read == 0
            || self.start_ns.load(Ordering::Acquire) == 0
            || self.delivered_ns.load(Ordering::Relaxed) != 0
        {
            return;
        }
        if self.splice_gated.load(Ordering::Relaxed) && self.spliced_ns.load(Ordering::Relaxed) == 0
        {
            return;
        }
        self.frames_computed.store(
            self.rendered_frames.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.block_frames.store(block_frames, Ordering::Relaxed);
        self.queued_frames.store(queued_frames, Ordering::Relaxed);
        self.delivered_ns.store(now_ns(), Ordering::Release);
    }

    /// Forget any pending start; a stop must not keep claiming the device never
    /// answered.
    pub fn cancel(&self) {
        self.delivered_ns.store(0, Ordering::Release);
        self.reported.store(true, Ordering::Relaxed);
        self.start_ns.store(0, Ordering::Relaxed);
        self.splice_gated.store(false, Ordering::Relaxed);
        self.spliced_ns.store(0, Ordering::Relaxed);
    }

    /// Sequence number of the latest first block.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Start path that produced the latest block.
    pub fn path(&self) -> StartPath {
        StartPath::from_u8(self.path.load(Ordering::Relaxed))
    }

    /// Snapshot of the latest first block.
    pub fn snapshot(&self) -> FirstBlockSnapshot {
        let start_ns = self.start_ns.load(Ordering::Acquire);
        let delivered_ns = self.delivered_ns.load(Ordering::Acquire);
        let block_frames = self.block_frames.load(Ordering::Acquire);
        let frames_computed = self.frames_computed.load(Ordering::Acquire);
        let spliced_ns = self.spliced_ns.load(Ordering::Acquire);
        let now = now_ns();
        FirstBlockSnapshot {
            path: self.path().as_str().to_string(),
            seq: self.seq(),
            delivered: delivered_ns != 0,
            latency_ms: (delivered_ns != 0 && start_ns != 0)
                .then(|| millis(delivered_ns.saturating_sub(start_ns))),
            since_start_ms: if start_ns == 0 {
                0.0
            } else {
                millis(now.saturating_sub(start_ns))
            },
            block_frames,
            frames_computed,
            blocks_computed: if block_frames == 0 {
                0.0
            } else {
                frames_computed as f64 / block_frames as f64
            },
            queued_frames: self.queued_frames.load(Ordering::Acquire),
            splice_ms: (spliced_ns != 0 && start_ns != 0)
                .then(|| millis(spliced_ns.saturating_sub(start_ns))),
        }
    }

    /// One debug line per start, or `None` once it has been reported.
    pub fn take_report(&self) -> Option<String> {
        if self.delivered_ns.load(Ordering::Acquire) == 0 {
            return None;
        }
        let snapshot = self.snapshot();
        let latency_ms = snapshot.latency_ms?;
        if self.reported.swap(true, Ordering::AcqRel) {
            return None;
        }
        let splice = snapshot
            .splice_ms
            .map(|ms| format!(" splice_ms={ms:.2}"))
            .unwrap_or_default();
        Some(format!(
            "first_block path={} seq={} latency_ms={:.2} block_frames={} frames_computed={} blocks_computed={:.2} queued_frames={}{}",
            snapshot.path,
            snapshot.seq,
            latency_ms,
            snapshot.block_frames,
            snapshot.frames_computed,
            snapshot.blocks_computed,
            snapshot.queued_frames,
            splice,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_origin_is_valid_but_unarmed_and_cancelled_deliveries_are_ignored() {
        let trace = FirstBlock::new();
        trace.observe(128, 128, 0);
        assert!(!trace.snapshot().delivered);
        trace.begin_at(0, StartPath::Play);
        trace.observe(128, 128, 0);
        let snapshot = trace.snapshot();
        assert!(snapshot.delivered);
        assert!(
            snapshot
                .latency_ms
                .is_some_and(|value| value.is_finite() && value >= 0.0)
        );
        assert!(trace.take_report().is_some());
        assert!(trace.take_report().is_none());
        trace.cancel();
        trace.observe(128, 128, 0);
        assert!(!trace.snapshot().delivered);
        assert!(trace.snapshot().latency_ms.is_none());
        assert!(trace.take_report().is_none());
    }

    #[test]
    fn a_start_records_the_press_the_block_and_the_frames_before_it() {
        let trace = FirstBlock::new();
        assert!(!trace.snapshot().delivered);
        let press = now_ns();
        trace.begin_at(press, StartPath::Play);
        trace.add_rendered(1024);
        assert_eq!(trace.snapshot().path, "play");
        assert!(!trace.snapshot().delivered);
        trace.observe(256, 256, 768);
        let snapshot = trace.snapshot();
        assert!(snapshot.delivered);
        assert!(snapshot.latency_ms.unwrap() >= 0.0);
        assert_eq!(snapshot.block_frames, 256);
        assert_eq!(snapshot.frames_computed, 1024);
        assert_eq!(snapshot.blocks_computed, 4.0);
        assert_eq!(snapshot.queued_frames, 768);
    }

    #[test]
    fn a_splice_gated_start_waits_for_the_lane() {
        let trace = FirstBlock::new();
        trace.begin_at(now_ns(), StartPath::LaneUnmute);
        trace.add_rendered(4096);
        trace.observe(256, 256, 0);
        assert!(!trace.snapshot().delivered, "the lane was not spliced yet");
        trace.mark_spliced();
        trace.observe(256, 256, 0);
        let snapshot = trace.snapshot();
        assert!(snapshot.delivered);
        assert!(snapshot.splice_ms.is_some());
    }

    #[test]
    fn a_silent_callback_block_does_not_deliver() {
        let trace = FirstBlock::new();
        trace.begin_at(now_ns(), StartPath::Play);
        trace.observe(0, 256, 0);
        assert!(!trace.snapshot().delivered);
    }

    #[test]
    fn a_new_start_supersedes_the_previous_one() {
        let trace = FirstBlock::new();
        trace.begin_at(now_ns(), StartPath::Play);
        trace.observe(256, 256, 0);
        assert!(trace.snapshot().delivered);
        trace.begin_at(now_ns(), StartPath::Seek);
        let snapshot = trace.snapshot();
        assert!(!snapshot.delivered);
        assert_eq!(snapshot.path, "seek");
        assert_eq!(snapshot.frames_computed, 0);
    }
}
