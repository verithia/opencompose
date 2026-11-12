//! MIDI take recording: capture from the live MIDI thread, turn the events into
//! pattern notes, and commit them as one undo entry.
//!
//! The MIDI thread timestamps every event in project frames counted from the
//! record press, so the count-in shows up as leading frames and is trimmed the
//! same way the audio take is. `transform` is a pure function of those frames:
//! it maps them through `ProjectClock`, quantises, applies the punch range, and
//! returns the notes the commit writes into the target pattern.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use oc_dsl::ast::{self, BarBeat};
use oc_dsl::timing::ProjectClock;
use serde::{Deserialize, Serialize};

use crate::commands::shared::{ProjectSnapshot, resync_with_undo, snapshot};
use crate::midi_bridge::LiveEvent;
use crate::state::{AppStateInner, UndoEdit, UndoScope};

/// Hard cap on one take's events. A take that reaches it stops capturing and
/// reports the overflow instead of growing without bound.
pub const MAX_CAPTURED_EVENTS: usize = 200_000;

/// The shortest note a take keeps: a 64th note.
pub const SHORTEST_NOTE_QUARTERS: f64 = 0.0625;

/// A recorded start keeps the captured time to this many quarters.
const START_GRANULARITY: f64 = 0.0001;

/// The note model expresses durations on the editor's 1/48-quarter grid.
const DURATION_TICKS_PER_QUARTER: f64 = 48.0;

/// Starts closer than this are the same musical position.
const SAME_START: f64 = 1e-6;

// ============================================================================
// Settings
// ============================================================================

/// Input quantise applied to a MIDI take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantise {
    #[default]
    Off,
    ThirtySecond,
    Sixteenth,
    Eighth,
    Quarter,
}

impl Quantise {
    /// Snap step in quarter notes, or `None` when quantise is off.
    pub fn grid(self) -> Option<f64> {
        match self {
            Self::Off => None,
            Self::ThirtySecond => Some(0.125),
            Self::Sixteenth => Some(0.25),
            Self::Eighth => Some(0.5),
            Self::Quarter => Some(1.0),
        }
    }

    /// Only a 1/16 grid and coarser snap ends; on a 1/32 grid a snapped end
    /// would shorten a played note that ran a little past the line.
    pub fn snaps_ends(self) -> bool {
        matches!(self, Self::Sixteenth | Self::Eighth | Self::Quarter)
    }

    #[cfg(test)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ThirtySecond => "1/32",
            Self::Sixteenth => "1/16",
            Self::Eighth => "1/8",
            Self::Quarter => "1/4",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "off" => Some(Self::Off),
            "1/32" | "thirty_second" => Some(Self::ThirtySecond),
            "1/16" | "sixteenth" => Some(Self::Sixteenth),
            "1/8" | "eighth" => Some(Self::Eighth),
            "1/4" | "quarter" => Some(Self::Quarter),
            _ => None,
        }
    }
}

/// The take settings the transport bar edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiTakeSettings {
    pub quantise: Quantise,
    /// Keep the notes already in the pattern instead of replacing the span.
    pub overdub: bool,
}

impl Default for MidiTakeSettings {
    fn default() -> Self {
        Self {
            quantise: Quantise::Off,
            overdub: false,
        }
    }
}

/// The MIDI half of `RecordState`: what the next take will do.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MidiRecordView {
    /// Pattern the next take lands in; `None` means it creates one.
    pub target: Option<String>,
    pub quantise: Quantise,
    pub overdub: bool,
    /// The selected MIDI input, as the arm chip names it.
    pub input: Option<String>,
}

// ============================================================================
// Capture
// ============================================================================

/// One captured MIDI event, timestamped in project frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturedEvent {
    /// Project frame the event happened on.
    pub frame: u64,
    pub kind: CapturedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedKind {
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
    },
    /// Sustain pedal, CC 64. Notes released while it is down keep sounding.
    Sustain {
        down: bool,
    },
}

struct TakeBuffer {
    started: Instant,
    start_frame: u64,
    rate: u32,
    events: Vec<CapturedEvent>,
    overflowed: bool,
}

impl TakeBuffer {
    fn frame_at(&self, now: Instant) -> u64 {
        let nanos = u64::try_from(now.saturating_duration_since(self.started).as_nanos())
            .unwrap_or(u64::MAX);
        let frames = u128::from(nanos) * u128::from(self.rate) / 1_000_000_000;
        self.start_frame
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX))
    }
}

/// What a finished capture holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturedTake {
    pub events: Vec<CapturedEvent>,
    /// The take reached the event cap and stopped storing.
    pub overflowed: bool,
    /// Events the MIDI thread could not hand over because the buffer was busy.
    pub dropped: u64,
}

/// The capture buffer the MIDI thread writes and the transport poll reads.
pub struct MidiCapture {
    take: Mutex<Option<TakeBuffer>>,
    dropped: AtomicU64,
}

impl Default for MidiCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiCapture {
    pub fn new() -> Self {
        Self {
            take: Mutex::new(None),
            dropped: AtomicU64::new(0),
        }
    }

    /// Open a take at `start_frame`. Events before this call are gone.
    pub fn begin(&self, start_frame: u64, rate: u32) {
        self.dropped.store(0, Ordering::Release);
        *self.lock() = Some(TakeBuffer {
            started: Instant::now(),
            start_frame,
            rate: rate.max(1),
            events: Vec::new(),
            overflowed: false,
        });
    }

    /// Close the take and return what it captured.
    pub fn take(&self) -> CapturedTake {
        let dropped = self.dropped.swap(0, Ordering::AcqRel);
        let Some(buffer) = self.lock().take() else {
            return CapturedTake::default();
        };
        CapturedTake {
            events: buffer.events,
            overflowed: buffer.overflowed,
            dropped,
        }
    }

    /// Timestamp and store one live event. Never blocks the MIDI thread: a busy
    /// buffer counts the event as dropped instead of waiting for the poll.
    pub fn record(&self, event: LiveEvent) {
        let Some(kind) = captured_kind(event) else {
            return;
        };
        let Ok(mut take) = self.take.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(buffer) = take.as_mut() else {
            return;
        };
        let frame = buffer.frame_at(Instant::now());
        store(buffer, frame, kind);
    }

    /// Offer one event while the poll holds the buffer, which is the path that
    /// drops it. Tests use this instead of racing a real MIDI thread.
    #[cfg(test)]
    pub(crate) fn record_while_busy(&self, event: LiveEvent) {
        let _held = self.lock();
        self.record(event);
    }

    /// Store one event at an explicit frame; the tests use this path.
    #[cfg(test)]
    pub fn record_at(&self, frame: u64, kind: CapturedKind) {
        let mut take = self.lock();
        let Some(buffer) = take.as_mut() else {
            return;
        };
        store(buffer, frame, kind);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<TakeBuffer>> {
        self.take.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn store(buffer: &mut TakeBuffer, frame: u64, kind: CapturedKind) {
    if buffer.events.len() >= MAX_CAPTURED_EVENTS {
        buffer.overflowed = true;
        return;
    }
    buffer.events.push(CapturedEvent { frame, kind });
}

/// The events a take stores; everything else is audition-only.
fn captured_kind(event: LiveEvent) -> Option<CapturedKind> {
    match event {
        LiveEvent::NoteOn { note, velocity, .. } if velocity > 0 => Some(CapturedKind::NoteOn {
            key: note,
            velocity,
        }),
        // A zero-velocity note-on is the other spelling of note-off.
        LiveEvent::NoteOn { note, .. } | LiveEvent::NoteOff { note, .. } => {
            Some(CapturedKind::NoteOff { key: note })
        }
        LiveEvent::ControlChange {
            controller: 64,
            value,
            ..
        } => Some(CapturedKind::Sustain { down: value >= 64 }),
        _ => None,
    }
}

// ============================================================================
// Transform: capture frames to pattern notes
// ============================================================================

/// One note the take produces, in beats inside the target pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TakeNote {
    pub start_beat: f64,
    pub duration_beats: f64,
    pub key: u8,
    pub velocity: u8,
}

/// The notes, the span the take covered, and what was thrown away.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TakeNotes {
    pub notes: Vec<TakeNote>,
    /// Record position inside the pattern.
    pub span_start_beat: f64,
    /// Stop position inside the pattern.
    pub span_end_beat: f64,
    /// Notes that started outside the punch range.
    pub discarded_punch: usize,
    /// Notes that started before the take did (during the count-in).
    pub discarded_before_take: usize,
    /// Notes shorter than a 64th after quantise.
    pub dropped_short: usize,
    /// Notes folded onto another note with the same key and start.
    pub merged: usize,
    /// Note-offs with no note-on inside the take.
    pub ignored_note_offs: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct TakeTransform<'a> {
    pub events: &'a [CapturedEvent],
    pub clock: &'a ProjectClock,
    pub rate: u32,
    /// Project frame the record press happened on.
    pub record_start_frame: u64,
    /// Project frame the transport stopped on.
    pub stop_frame: u64,
    /// Quarter the target pattern's beat 0 plays at.
    pub origin_quarter: f64,
    /// Leading frames that belong to the count-in.
    pub head_trim_frames: u64,
    pub quantise: Quantise,
    /// Enabled punch range, in absolute quarters.
    pub punch: Option<(f64, f64)>,
}

/// A note whose key is down, or released under the sustain pedal.
#[derive(Debug, Clone, Copy)]
struct Sounding {
    key: u8,
    start: u64,
    velocity: u8,
}

/// Turn captured events into notes for the pattern at `origin_quarter`.
///
/// Events are stamped from the press, before the count-in and the wait for the
/// transport to roll. Every surviving frame is shifted onto the roll, so a note
/// is written at the beat that was played.
pub fn transform(input: &TakeTransform<'_>) -> Result<TakeNotes, String> {
    let trim = input
        .record_start_frame
        .saturating_add(input.head_trim_frames);
    let stop = input.stop_frame.max(trim);
    let mut held: Vec<Sounding> = Vec::new();
    let mut sustained: Vec<Sounding> = Vec::new();
    let mut pedal_down = false;
    let mut finished: Vec<(Sounding, u64)> = Vec::new();
    let mut result = TakeNotes::default();

    for event in input.events {
        match event.kind {
            CapturedKind::NoteOn { key, velocity } => {
                // A re-trigger ends the previous note at the new one's frame.
                if let Some(index) = held.iter().position(|note| note.key == key) {
                    let previous = held.remove(index);
                    finished.push((previous, event.frame.max(previous.start)));
                }
                held.push(Sounding {
                    key,
                    start: event.frame,
                    velocity: velocity.clamp(1, 127),
                });
            }
            CapturedKind::NoteOff { key } => {
                let Some(index) = held.iter().position(|note| note.key == key) else {
                    result.ignored_note_offs += 1;
                    continue;
                };
                let note = held.remove(index);
                if pedal_down {
                    sustained.push(note);
                } else {
                    finished.push((note, event.frame.max(note.start)));
                }
            }
            CapturedKind::Sustain { down } => {
                if pedal_down == down {
                    continue;
                }
                pedal_down = down;
                if !down {
                    for note in sustained.drain(..) {
                        finished.push((note, event.frame.max(note.start)));
                    }
                }
            }
        }
    }
    // Everything still sounding at the stop ends there.
    for note in held.drain(..).chain(sustained.drain(..)) {
        finished.push((note, stop));
    }

    let grid = input.quantise.grid();
    for (note, end_frame) in finished {
        // A note that started before the roll belongs to the count-in.
        if note.start < trim {
            result.discarded_before_take += 1;
            continue;
        }
        // Move the note from the press clock onto the roll; the frames that
        // survive the trim are the ones the transport was playing.
        let start_frame = note.start.saturating_sub(input.head_trim_frames);
        let end_frame = end_frame
            .max(note.start)
            .saturating_sub(input.head_trim_frames);
        let start_quarter = input
            .clock
            .position_at_sample(start_frame, input.rate)?
            .quarter;
        let end_quarter = input
            .clock
            .position_at_sample(end_frame, input.rate)?
            .quarter;
        if let Some((in_beats, out_beats)) = input.punch
            && (start_quarter < in_beats - 1e-9 || start_quarter >= out_beats - 1e-9)
        {
            result.discarded_punch += 1;
            continue;
        }
        let played = end_quarter - start_quarter;
        // A sub-64th note is a stick bounce, not a note; emitting it would be a
        // click in the piano roll.
        if played < SHORTEST_NOTE_QUARTERS - 1e-9 {
            result.dropped_short += 1;
            continue;
        }
        let mut start = start_quarter - input.origin_quarter;
        let mut duration = played;
        if let Some(grid) = grid {
            let snapped = snap(start, grid);
            duration += start - snapped;
            start = snapped;
            if input.quantise.snaps_ends() {
                duration = snap(end_quarter - input.origin_quarter, grid) - start;
            }
        }
        let floor = grid.unwrap_or(SHORTEST_NOTE_QUARTERS);
        result.notes.push(TakeNote {
            start_beat: round_start(start.max(0.0)),
            duration_beats: duration.max(floor).min(1024.0),
            key: note.key,
            velocity: note.velocity,
        });
    }
    result.span_start_beat = input
        .clock
        .position_at_sample(input.record_start_frame, input.rate)?
        .quarter
        - input.origin_quarter;
    // The span is a transport range too, so the take's head comes off the stop.
    let stop_frame = stop
        .saturating_sub(input.head_trim_frames)
        .max(input.record_start_frame);
    result.span_end_beat = input
        .clock
        .position_at_sample(stop_frame, input.rate)?
        .quarter
        - input.origin_quarter;
    merge_same_start(&mut result);
    Ok(result)
}

/// Fold notes that landed on the same key and start into one; the longer
/// sustain and the louder velocity win, so nothing audible is thrown away.
fn merge_same_start(take: &mut TakeNotes) {
    take.notes.sort_by(|a, b| {
        a.start_beat
            .total_cmp(&b.start_beat)
            .then(a.key.cmp(&b.key))
    });
    let mut merged: Vec<TakeNote> = Vec::with_capacity(take.notes.len());
    for note in take.notes.drain(..) {
        if let Some(last) = merged.last_mut()
            && last.key == note.key
            && (last.start_beat - note.start_beat).abs() <= SAME_START
        {
            last.velocity = last.velocity.max(note.velocity);
            last.duration_beats = last.duration_beats.max(note.duration_beats);
            take.merged += 1;
            continue;
        }
        merged.push(note);
    }
    take.notes = merged;
}

fn snap(value: f64, grid: f64) -> f64 {
    (value / grid).round() * grid
}

fn round_start(value: f64) -> f64 {
    (value / START_GRANULARITY).round() * START_GRANULARITY
}

// ============================================================================
// Recorder state
// ============================================================================

/// Where a take lands.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TakeTarget {
    /// An existing pattern under the playhead, with the quarter its beat 0
    /// plays at, and the clip that places it.
    Pattern {
        name: String,
        origin_quarter: f64,
        clip_index: usize,
    },
    /// A pattern and clip created at the record position when the take stops.
    New { origin_quarter: f64, start: BarBeat },
}

impl TakeTarget {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Pattern { name, .. } => Some(name),
            Self::New { .. } => None,
        }
    }

    fn origin_quarter(&self) -> f64 {
        match self {
            Self::Pattern { origin_quarter, .. } | Self::New { origin_quarter, .. } => {
                *origin_quarter
            }
        }
    }
}

struct TakeSession {
    track: String,
    target: TakeTarget,
    record_start_frame: u64,
    head_trim_frames: u64,
    punch: Option<(f64, f64)>,
    settings: MidiTakeSettings,
    started: Instant,
    /// The trim has been resolved from the frame the transport rolled at.
    anchored: bool,
}

impl TakeSession {
    /// Resolve the head trim from the frame the transport rolled at.
    ///
    /// A captured event is stamped from the press, so everything before the
    /// roll is the count-in plus the wait for the transport. The playhead is
    /// timeline frames, so it stays at the record start until the count-in
    /// ends and then reports exactly the frames the events are ahead by.
    /// Counting them at any poll gives the same roll frame, which is why the
    /// dispatch return is not an input.
    fn resolve_roll(&mut self, playhead: u64, rate: f64) {
        if self.anchored {
            return;
        }
        let played = playhead.saturating_sub(self.record_start_frame);
        if played == 0 {
            // The transport still holds at the record start: the count-in has
            // not ended, so the roll frame is not known.
            return;
        }
        let elapsed = (self.started.elapsed().as_secs_f64() * rate).round() as u64;
        self.head_trim_frames = elapsed.saturating_sub(played);
        self.anchored = true;
    }
}

/// The MIDI take in progress, its settings and its last report line.
pub struct MidiRecorder {
    pub(crate) capture: Arc<MidiCapture>,
    pub settings: MidiTakeSettings,
    session: Option<TakeSession>,
    /// The last refusal or take line, shown until the next press.
    pub report: Option<String>,
}

impl Default for MidiRecorder {
    fn default() -> Self {
        Self {
            capture: Arc::new(MidiCapture::new()),
            settings: MidiTakeSettings::default(),
            session: None,
            report: None,
        }
    }
}

impl MidiRecorder {
    pub(crate) fn recording(&self) -> bool {
        self.session.is_some()
    }

    pub(crate) fn seconds(&self) -> f64 {
        self.session
            .as_ref()
            .map_or(0.0, |session| session.started.elapsed().as_secs_f64())
    }

    /// The pattern a live take is writing into.
    pub(crate) fn live_target(&self) -> Option<&str> {
        self.session
            .as_ref()
            .and_then(|session| session.target.name())
    }
}

/// The MIDI half of the transport's record state.
pub(crate) fn view(inner: &AppStateInner) -> MidiRecordView {
    let recorder = &inner.audio.midi_recorder;
    // Only a MIDI arm has a target pattern; an audio take reports none, so the
    // UI never claims a pattern it will not touch.
    let midi_armed = crate::record::mode_of(inner) == crate::record::RecordMode::Midi;
    MidiRecordView {
        target: recorder
            .live_target()
            .map(std::string::ToString::to_string)
            .or_else(|| midi_armed.then(|| armed_target(inner)).flatten()),
        quantise: recorder.settings.quantise,
        overdub: recorder.settings.overdub,
        input: inner.midi_live.input(),
    }
}

/// The pattern the armed track's next take would land in, for the arm chip.
fn armed_target(inner: &AppStateInner) -> Option<String> {
    let track = inner
        .project
        .tracks
        .iter()
        .find(|track| Some(track.name.as_str()) == inner.audio.recorder.armed.as_deref())?;
    let clock = ProjectClock::new(&inner.project.properties).ok()?;
    let quarter = clock
        .position_at_sample(
            inner.audio.timeline_cursor_frame(),
            inner.audio.sample_rate.max(1),
        )
        .ok()?
        .quarter;
    match target_for(&inner.project, track, &clock, quarter).ok()? {
        TakeTarget::Pattern { name, .. } => Some(name),
        TakeTarget::New { .. } => None,
    }
}

// ============================================================================
// Starting, polling and stopping
// ============================================================================

/// Why a MIDI take cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiRefusal {
    /// No track is armed.
    NotArmed,
    /// The MIDI preferences name no input.
    NoInput,
    /// The MIDI thread could not open the selected input.
    Input(String),
    /// The armed track is not an instrument track.
    NotInstrument(String),
}

impl MidiRefusal {
    pub fn reason(&self) -> String {
        match self {
            Self::NotArmed => "No track is armed for recording.".into(),
            Self::NoInput => "No MIDI input is selected.".into(),
            Self::Input(error) => error.clone(),
            Self::NotInstrument(track) => {
                format!("Track '{track}' has no instrument to record MIDI into.")
            }
        }
    }

    /// The one line the transport shows when a press fails.
    pub fn message(&self) -> String {
        let reason = self.reason();
        let mut chars = reason.chars();
        let reason = match chars.next() {
            Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        format!(
            "MIDI recording could not start: {}. The project is unchanged.",
            reason.trim_end_matches('.')
        )
    }
}

/// What ended a take.
enum Finish {
    Stop,
    /// The MIDI input died under the take.
    InputLost(Option<String>),
}

/// Start a MIDI take on the armed track.
///
/// `start_input` opens the MIDI thread; the command layer passes the real
/// backend and the tests pass a fake one, exactly like `MidiLive::configure`.
pub(crate) fn start_inner<S>(
    inner: &mut AppStateInner,
    rate: u32,
    start_input: S,
) -> Result<(), String>
where
    S: FnOnce(
        &str,
        crate::midi_live::LiveInputSink,
    ) -> Result<crate::midi_live::MidiRuntime, String>,
{
    if inner.audio.midi_recorder.session.is_some() {
        return Ok(());
    }
    let Some(track_name) = inner.audio.recorder.armed.clone() else {
        return refuse(inner, MidiRefusal::NotArmed);
    };
    let Some(track) = inner
        .project
        .tracks
        .iter()
        .find(|track| track.name == track_name)
        .cloned()
    else {
        return refuse(inner, MidiRefusal::NotArmed);
    };
    if is_audio_track(&track) {
        return refuse(inner, MidiRefusal::NotInstrument(track.name.clone()));
    }
    if inner.midi_live.input().is_none() {
        return refuse(inner, MidiRefusal::NoInput);
    }
    let clock = ProjectClock::new(&inner.project.properties)?;
    let playhead = inner.audio.timeline_cursor_frame();
    let quarter = clock.position_at_sample(playhead, rate)?.quarter;
    let target = target_for(&inner.project, &track, &clock, quarter)?;

    // The thread must be polling before the transport rolls, or the first note
    // is lost; the count-in gives it time to settle.
    if !inner.midi_live.active() {
        let sink = crate::midi_live::LiveInputSink {
            route: inner.audio.live_input_route(),
            capture: Arc::clone(&inner.audio.midi_recorder.capture),
        };
        if let Err(error) = inner
            .midi_live
            .ensure_input(move |selector| start_input(selector, sink))
        {
            return refuse(inner, MidiRefusal::Input(error));
        }
    }

    let plan =
        oc_audio::click::count_in_plan(&clock, inner.audio.click.count_in.bars(), rate, quarter)?;
    // Only the count-in and the wait for the transport are trimmed: the MIDI
    // thread timestamps on arrival, so a device's own latency is already inside
    // the frame and is not compensated. The wait is resolved from the playhead
    // once the transport rolls.
    inner.audio.midi_recorder.capture.begin(playhead, rate);
    inner.audio.midi_recorder.session = Some(TakeSession {
        track: track.name.clone(),
        target,
        record_start_frame: playhead,
        head_trim_frames: plan.frames,
        punch: inner
            .audio
            .punch_region
            .filter(|_| inner.audio.punch_enabled),
        settings: inner.audio.midi_recorder.settings,
        started: Instant::now(),
        anchored: false,
    });
    inner.audio.midi_recorder.report = None;
    inner.audio.recorder.report = None;
    inner.audio.pending_count_in = (plan.frames > 0).then_some(plan);
    inner.audio.seek_frame(playhead, &clock)?;
    Ok(())
}

fn refuse<T>(inner: &mut AppStateInner, refusal: MidiRefusal) -> Result<T, String> {
    say(inner, refusal.message());
    Err(refusal.reason())
}

/// Set the MIDI take line and drop the audio one, so only the take that just
/// spoke is reported.
fn say(inner: &mut AppStateInner, message: String) {
    inner.audio.midi_recorder.report = Some(message);
    inner.audio.recorder.report = None;
}

/// A take interrupted by the MIDI input dying still commits what arrived.
pub(crate) fn poll_inner(inner: &mut AppStateInner) -> Result<(), String> {
    resolve_roll(inner);
    if !inner.audio.midi_recorder.recording() || inner.midi_live.active() {
        return Ok(());
    }
    let error = inner.midi_live.error();
    log::warn!(
        "MIDI take ended: {}",
        error.as_deref().unwrap_or("the input closed")
    );
    finish_inner(inner, Finish::InputLost(error))?;
    Ok(())
}

/// The clause a take line adds when the capture dropped events.
fn dropped_clause(dropped: u64) -> String {
    let verb = if dropped == 1 {
        "event was"
    } else {
        "events were"
    };
    format!("{dropped} {verb} dropped because the capture buffer was busy")
}

/// The line a take shows when its input died mid-recording.
fn stopped_line(kept: usize, dropped: u64, reason: Option<&str>) -> String {
    let notes = if kept == 1 { "note" } else { "notes" };
    let mut line = match reason {
        Some(reason) => format!(
            "{}. Kept {kept} {notes} up to the last event.",
            reason.trim_end_matches('.')
        ),
        None => format!("The MIDI input stopped: {kept} {notes} were kept up to the last event."),
    };
    if dropped > 0 {
        line.push_str(&format!(" {}", dropped_clause(dropped)));
    }
    line
}

/// Stop the take and write its notes into the pattern.
pub(crate) fn stop_inner(inner: &mut AppStateInner) -> Result<Option<ProjectSnapshot>, String> {
    if !inner.audio.midi_recorder.recording() {
        return Ok(None);
    }
    finish_inner(inner, Finish::Stop)
}

/// Drop a take that never reached the transport, e.g. a render that failed.
pub(crate) fn cancel_inner(inner: &mut AppStateInner) {
    if inner.audio.midi_recorder.session.take().is_some() {
        inner.audio.midi_recorder.capture.take();
        inner.midi_live.stop_if_record_only();
        inner.audio.pending_count_in = None;
    }
}

/// Derive the take's head trim from the frame the transport rolled at.
///
/// The trim cannot come from the play dispatch: that returns before the
/// render thread applies the play, and a project that renders first waits
/// longer than the return says. The playhead holds at the record start through
/// the count-in and then reports the timeline frames it has played, which are
/// exactly the frames the events are ahead by.
fn resolve_roll(inner: &mut AppStateInner) {
    let playhead = inner.audio.take_progress_frame();
    let rate = f64::from(inner.audio.sample_rate.max(1));
    if let Some(session) = inner.audio.midi_recorder.session.as_mut() {
        session.resolve_roll(playhead, rate);
    }
}

fn finish_inner(
    inner: &mut AppStateInner,
    finish: Finish,
) -> Result<Option<ProjectSnapshot>, String> {
    let Some(mut session) = inner.audio.midi_recorder.session.take() else {
        return Ok(None);
    };
    // The transport may have rolled since the last poll; resolve the trim at
    // the stop position as well, so a short take is not written late.
    let playhead = inner.audio.take_progress_frame();
    session.resolve_roll(playhead, f64::from(inner.audio.sample_rate.max(1)));
    // Stopping the thread routes the still-held note-offs the way the audition
    // needs them; only then is the capture closed.
    inner.midi_live.stop_if_record_only();
    let finish = match finish {
        Finish::Stop if !inner.midi_live.active() => inner
            .midi_live
            .error()
            .map_or(Finish::Stop, |reason| Finish::InputLost(Some(reason))),
        other => other,
    };
    let captured = inner.audio.midi_recorder.capture.take();
    if !session.anchored && session.head_trim_frames > 0 {
        match &finish {
            Finish::InputLost(reason) => {
                say(inner, stopped_line(0, captured.dropped, reason.as_deref()))
            }
            Finish::Stop => inner.audio.midi_recorder.report = None,
        }
        return Ok(None);
    }
    let clock = ProjectClock::new(&inner.project.properties)?;
    // A transport that never advanced still reports the span the events cover.
    let stop_frame = captured
        .events
        .iter()
        .map(|event| event.frame)
        .max()
        .unwrap_or_default()
        .max(playhead)
        .max(session.record_start_frame);
    let rate = inner.audio.sample_rate.max(1);
    let take = transform(&TakeTransform {
        events: &captured.events,
        clock: &clock,
        rate,
        record_start_frame: session.record_start_frame,
        stop_frame,
        origin_quarter: session.target.origin_quarter(),
        head_trim_frames: session.head_trim_frames,
        quantise: session.settings.quantise,
        punch: session.punch,
    })?;
    if take.notes.is_empty() {
        // A take that lost every event says why instead of claiming that
        // nothing arrived.
        let line = if let Finish::InputLost(Some(reason)) = &finish {
            stopped_line(0, captured.dropped, Some(reason))
        } else if captured.dropped > 0 {
            format!(
                "Nothing was recorded: {}.",
                dropped_clause(captured.dropped)
            )
        } else {
            "Nothing was recorded: no notes arrived.".to_string()
        };
        say(inner, line);
        return Ok(None);
    }
    let pattern = commit(inner, &session, &take)?;
    let line = match finish {
        Finish::InputLost(reason) => {
            stopped_line(take.notes.len(), captured.dropped, reason.as_deref())
        }
        Finish::Stop => take_line(&pattern, &take, captured.overflowed, captured.dropped),
    };
    say(inner, line);
    Ok(Some(snapshot(inner)))
}

/// The report line a finished MIDI take shows.
fn take_line(pattern: &str, take: &TakeNotes, overflowed: bool, dropped: u64) -> String {
    let count = take.notes.len();
    let notes = if count == 1 {
        "1 note".to_string()
    } else {
        format!("{count} notes")
    };
    let mut extra = Vec::new();
    // Losses come first: what never reached the take outranks what the
    // transform threw away afterwards.
    if dropped > 0 {
        extra.push(dropped_clause(dropped));
    }
    if take.discarded_punch > 0 {
        extra.push(format!(
            "{} discarded outside the punch range",
            take.discarded_punch
        ));
    }
    if take.dropped_short > 0 {
        extra.push(format!(
            "{} dropped as shorter than a 64th",
            take.dropped_short
        ));
    }
    if take.merged > 0 {
        extra.push(format!("{} merged onto the same start", take.merged));
    }
    if overflowed {
        extra.push(format!(
            "the capture stopped after {MAX_CAPTURED_EVENTS} events"
        ));
    }
    if extra.is_empty() {
        format!("Recorded {notes} into {pattern}")
    } else {
        format!("Recorded {notes} into {pattern} ({})", extra.join(", "))
    }
}

// ============================================================================
// Target selection and commit
// ============================================================================

/// An instrument track records MIDI; an `audio/…` track records the input.
pub(crate) fn is_audio_track(track: &ast::Track) -> bool {
    track
        .instrument
        .category
        .trim()
        .eq_ignore_ascii_case("audio")
}

/// Where the take at `quarter` lands.
///
/// The pattern under the playhead is the target, so notes land at their absolute
/// beat inside it. When the playhead is over none of the track's clips the take
/// creates a pattern and places its clip at the record position, which keeps
/// every written start non-negative.
fn target_for(
    project: &ast::Project,
    track: &ast::Track,
    clock: &ProjectClock,
    quarter: f64,
) -> Result<TakeTarget, String> {
    let known = |clip: &ast::Clip| {
        project
            .patterns
            .iter()
            .any(|pattern| pattern.name == clip.pattern)
    };
    let mut fallback = None;
    for (clip_index, clip) in track.pattern_clips().enumerate() {
        if !known(clip) {
            continue;
        }
        let candidate = TakeTarget::Pattern {
            name: clip.pattern.clone(),
            origin_quarter: clock.quarter_notes_at(&clip.time_range.start)? + clip.offset_quarters,
            clip_index,
        };
        fallback.get_or_insert(candidate.clone());
        let end = clock.quarter_notes_at(&clip.time_range.end)?;
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        if (start..end).contains(&quarter) {
            return Ok(candidate);
        }
    }
    if let Some(target) = fallback {
        return Ok(target);
    }
    let start = clock.bar_beat_floor_at_quarter(quarter)?;
    Ok(TakeTarget::New {
        origin_quarter: clock.quarter_notes_at(&start)?,
        start,
    })
}

/// Build the note events a take writes.
fn note_events(notes: &[TakeNote]) -> Result<Vec<ast::NoteEvent>, String> {
    let mut events = Vec::with_capacity(notes.len());
    for note in notes {
        let beats = round_duration(note.duration_beats);
        events.push(ast::NoteEvent {
            start_beat: note.start_beat,
            explicit_position: true,
            pitch: pitch_for_key(note.key)?,
            duration: crate::commands::notes::editor_duration(beats)?,
            dynamics: None,
            velocity: Some(note.velocity),
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: vec![],
        });
    }
    Ok(events)
}

/// A duration on the editor's 1/48-quarter grid; the note model has no finer
/// resolution.
fn round_duration(beats: f64) -> f64 {
    (beats * DURATION_TICKS_PER_QUARTER).round() / DURATION_TICKS_PER_QUARTER
}

/// MIDI key number to the canonical written pitch, including octave -1.
pub fn pitch_for_key(key: u8) -> Result<ast::Pitch, String> {
    ast::Pitch::from_midi(key).ok_or_else(|| format!("MIDI note {key} must be within 0..127"))
}

/// Write the take into its pattern and return the pattern's name.
fn commit(
    inner: &mut AppStateInner,
    session: &TakeSession,
    take: &TakeNotes,
) -> Result<String, String> {
    let old_project = inner.project.clone();
    let events = note_events(&take.notes)?;
    let name = match &session.target {
        TakeTarget::Pattern {
            name, clip_index, ..
        } => {
            let pattern = inner
                .project
                .patterns
                .iter_mut()
                .find(|pattern| &pattern.name == name)
                .ok_or_else(|| format!("Pattern '{name}' not found"))?;
            let existing = std::mem::take(&mut pattern.events);
            pattern.events = merge_take(existing, events, take, session.settings.overdub);
            extend_clip(inner, &session.track, *clip_index, take)?;
            name.clone()
        }
        TakeTarget::New { start, .. } => {
            let pattern_name = unique_pattern_name(&inner.project, &session.track);
            let track = inner
                .project
                .tracks
                .iter()
                .find(|track| track.name == session.track)
                .cloned()
                .ok_or_else(|| format!("Track '{}' not found", session.track))?;
            inner.project.patterns.push(ast::PatternDef {
                name: pattern_name.clone(),
                instrument: track.instrument.clone(),
                preset: track.preset.clone(),
                engine: track.engine.clone(),
                transpose: None,
                swing: None,
                bars: None,
                effects: vec![],
                events,
                pedals: vec![],
                hairpins: Vec::new(),
                hits: vec![],
                color: None,
            });
            place_clip(inner, &session.track, &pattern_name, *start, take)?;
            pattern_name
        }
    };
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("record midi take", UndoScope::Project),
    )?;
    Ok(name)
}

/// Replace the recorded span, or merge over it when overdubbing.
///
/// Replace removes every note that starts inside the span the take covered, so
/// a second pass does not double the first. Overdub keeps them and folds a note
/// that lands exactly on an existing one into it.
fn merge_take(
    existing: Vec<ast::NoteEvent>,
    incoming: Vec<ast::NoteEvent>,
    take: &TakeNotes,
    overdub: bool,
) -> Vec<ast::NoteEvent> {
    let mut events: Vec<ast::NoteEvent> = if overdub {
        existing
    } else {
        existing
            .into_iter()
            .filter(|event| {
                let start = event.start_beat;
                start < take.span_start_beat - SAME_START
                    || start >= take.span_end_beat - SAME_START
            })
            .collect()
    };
    for event in incoming {
        let key = event.pitch.to_midi();
        if let Some(same) = events.iter_mut().find(|other| {
            other.pitch.to_midi() == key
                && (other.start_beat - event.start_beat).abs() <= SAME_START
        }) {
            let end = same.duration.to_beats().max(event.duration.to_beats());
            if let Ok(duration) = crate::commands::notes::editor_duration(round_duration(end)) {
                same.duration = duration;
            }
            same.velocity = Some(same.velocity.unwrap_or(0).max(event.velocity.unwrap_or(0)));
            continue;
        }
        events.push(event);
    }
    events.sort_by(|a, b| a.start_beat.total_cmp(&b.start_beat));
    events
}

/// A pattern name no other pattern occupies.
fn unique_pattern_name(project: &ast::Project, track: &str) -> String {
    let base = format!("{track} take");
    for index in 1.. {
        let candidate = if index == 1 {
            base.clone()
        } else {
            format!("{base} {index}")
        };
        if !project
            .patterns
            .iter()
            .any(|pattern| pattern.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("the take counter always finds a free name")
}

/// Keep the target's clip covering everything the take recorded.
fn extend_clip(
    inner: &mut AppStateInner,
    track_name: &str,
    clip_index: usize,
    take: &TakeNotes,
) -> Result<(), String> {
    let clock = ProjectClock::new(&inner.project.properties)?;
    let Some(track) = inner
        .project
        .tracks
        .iter_mut()
        .find(|track| track.name == track_name)
    else {
        return Ok(());
    };
    let Some(clip) = track.pattern_clip_mut(clip_index) else {
        return Ok(());
    };
    let start = clock.quarter_notes_at(&clip.time_range.start)? + clip.offset_quarters;
    let end = clock.quarter_notes_at(&clip.time_range.end)?;
    let content = take
        .notes
        .iter()
        .map(|note| note.start_beat + note.duration_beats)
        .fold(0.0_f64, f64::max);
    if start + content <= end + 1e-9 {
        return Ok(());
    }
    clip.time_range.end = crate::record::ceil_bar_beat(&clock, start + content)?;
    Ok(())
}

/// Place a new pattern on the track, covering everything the take recorded.
fn place_clip(
    inner: &mut AppStateInner,
    track_name: &str,
    pattern_name: &str,
    start: BarBeat,
    take: &TakeNotes,
) -> Result<(), String> {
    let clock = ProjectClock::new(&inner.project.properties)?;
    let origin = clock.quarter_notes_at(&start)?;
    let content = take
        .notes
        .iter()
        .map(|note| note.start_beat + note.duration_beats)
        .fold(0.0_f64, f64::max)
        .max(SHORTEST_NOTE_QUARTERS);
    let end = crate::record::ceil_bar_beat(&clock, origin + content)?;
    let track = inner
        .project
        .tracks
        .iter_mut()
        .find(|track| track.name == track_name)
        .ok_or_else(|| format!("Track '{track_name}' not found"))?;
    track.push_pattern_clip(ast::Clip {
        pattern: pattern_name.to_string(),
        time_range: ast::TimeRange { start, end },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    Ok(())
}

// ============================================================================
// Commands
// ============================================================================

/// Set input quantise and the overdub choice.
#[tauri::command]
pub fn set_midi_record_settings(
    quantise: Option<String>,
    overdub: Option<bool>,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<MidiRecordView, String> {
    let mut inner = state.lock_inner();
    if let Some(text) = quantise {
        inner.audio.midi_recorder.settings.quantise = Quantise::parse(&text).ok_or_else(|| {
            format!("Unknown quantise '{text}'; use off, 1/32, 1/16, 1/8 or 1/4.")
        })?;
    }
    if let Some(overdub) = overdub {
        inner.audio.midi_recorder.settings.overdub = overdub;
    }
    Ok(view(&inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn clock() -> ProjectClock {
        ProjectClock::new(&ast::ProjectProperties::default()).expect("clock")
    }

    /// One quarter at 120 bpm is 24 000 frames at 48 kHz.
    fn quarters(value: f64) -> u64 {
        (value * 24_000.0).round() as u64
    }

    fn on(frame: u64, key: u8, velocity: u8) -> CapturedEvent {
        CapturedEvent {
            frame,
            kind: CapturedKind::NoteOn { key, velocity },
        }
    }

    fn off(frame: u64, key: u8) -> CapturedEvent {
        CapturedEvent {
            frame,
            kind: CapturedKind::NoteOff { key },
        }
    }

    fn take(events: &[CapturedEvent], quantise: Quantise) -> TakeNotes {
        let clock = clock();
        transform(&TakeTransform {
            events,
            clock: &clock,
            rate: RATE,
            record_start_frame: 0,
            stop_frame: quarters(8.0),
            origin_quarter: 0.0,
            head_trim_frames: 0,
            quantise,
            punch: None,
        })
        .expect("transform")
    }

    #[test]
    fn note_positions_and_durations_come_from_the_project_clock() {
        let notes = take(
            &[
                on(quarters(1.0), 60, 100),
                off(quarters(1.5), 60),
                on(quarters(2.0), 64, 90),
                off(quarters(3.0), 64),
            ],
            Quantise::Off,
        );
        assert_eq!(notes.notes.len(), 2);
        assert!((notes.notes[0].start_beat - 1.0).abs() < 1e-6);
        assert!((notes.notes[0].duration_beats - 0.5).abs() < 1e-6);
        assert_eq!(notes.notes[0].key, 60);
        assert_eq!(notes.notes[0].velocity, 100);
        assert!((notes.notes[1].start_beat - 2.0).abs() < 1e-6);
        assert!((notes.notes[1].duration_beats - 1.0).abs() < 1e-6);
        assert_eq!(notes.span_start_beat, 0.0);
        assert!((notes.span_end_beat - 8.0).abs() < 1e-6);
    }

    #[test]
    fn notes_land_at_their_beat_inside_the_pattern() {
        let clock = clock();
        let notes = transform(&TakeTransform {
            events: &[on(quarters(4.0), 60, 100), off(quarters(5.0), 60)],
            clock: &clock,
            rate: RATE,
            record_start_frame: quarters(4.0),
            stop_frame: quarters(8.0),
            // The playhead sits at beat 4 of a pattern that starts at beat 0.
            origin_quarter: 0.0,
            head_trim_frames: 0,
            quantise: Quantise::Off,
            punch: None,
        })
        .expect("transform");
        assert!(
            (notes.notes[0].start_beat - 4.0).abs() < 1e-6,
            "{:?}",
            notes.notes
        );
        assert!((notes.span_start_beat - 4.0).abs() < 1e-6);
    }

    #[test]
    fn a_held_note_ends_at_the_stop_position() {
        let notes = take(&[on(quarters(1.0), 60, 100)], Quantise::Off);
        assert_eq!(notes.notes.len(), 1);
        assert!(
            (notes.notes[0].duration_beats - 7.0).abs() < 1e-6,
            "{:?}",
            notes.notes
        );
    }

    #[test]
    fn a_note_off_without_a_note_on_is_ignored() {
        let notes = take(
            &[off(quarters(1.0), 60), on(quarters(2.0), 64, 80)],
            Quantise::Off,
        );
        assert_eq!(notes.notes.len(), 1);
        assert_eq!(notes.notes[0].key, 64);
        assert_eq!(notes.ignored_note_offs, 1);
    }

    #[test]
    fn the_count_in_head_is_trimmed_and_a_note_from_it_is_discarded() {
        let clock = clock();
        let notes = transform(&TakeTransform {
            events: &[
                // Played during the count-in, released after it: discarded.
                on(quarters(0.5), 60, 100),
                off(quarters(2.5), 60),
                // Played on the first beat after the count-in: beat 1.
                on(quarters(3.0), 64, 90),
                off(quarters(3.5), 64),
                // And one beat later: beat 2.
                on(quarters(4.0), 67, 80),
                off(quarters(4.5), 67),
            ],
            clock: &clock,
            rate: RATE,
            record_start_frame: 0,
            stop_frame: quarters(8.0),
            origin_quarter: 0.0,
            head_trim_frames: quarters(2.0),
            quantise: Quantise::Off,
            punch: None,
        })
        .expect("transform");
        assert_eq!(notes.notes.len(), 2);
        assert_eq!(notes.discarded_before_take, 1);
        assert_eq!(notes.notes[0].key, 64);
        assert!(
            (notes.notes[0].start_beat - 1.0).abs() < 1e-6,
            "the count-in head is trimmed, not just discarded: {:?}",
            notes.notes
        );
        assert_eq!(notes.notes[1].key, 67);
        assert!(
            (notes.notes[1].start_beat - 2.0).abs() < 1e-6,
            "{:?}",
            notes.notes
        );
    }

    #[test]
    fn the_head_trim_comes_from_the_roll_the_transport_reached() {
        // The press is one quarter before the roll: the first block waits,
        // then a one-bar count-in of four quarters.
        let wait = quarters(1.0);
        let count_in = quarters(4.0);
        let roll = wait + count_in;
        let mut session = TakeSession {
            track: "Lead".into(),
            target: TakeTarget::New {
                origin_quarter: 0.0,
                start: BarBeat::new(1, 1, 0),
            },
            record_start_frame: 0,
            head_trim_frames: count_in,
            punch: None,
            settings: MidiTakeSettings::default(),
            started: Instant::now(),
            anchored: false,
        };
        let rate = f64::from(RATE);
        // The playhead is timeline frames: it holds at the record start through
        // the count-in, then reports the frames played. Whichever poll sees the
        // roll first, the trim is the count-in plus the wait.
        for rolled in [1u64, 512, quarters(2.0)] {
            session.anchored = false;
            let elapsed = std::time::Duration::from_secs_f64((roll + rolled) as f64 / rate);
            session.started = Instant::now() - elapsed;
            session.resolve_roll(rolled, rate);
            assert_eq!(
                session.head_trim_frames, roll,
                "a poll {rolled} frames into the roll resolves the same head"
            );
        }
        // A playhead that has not left the record start has not rolled yet.
        session.anchored = false;
        session.head_trim_frames = count_in;
        session.resolve_roll(0, rate);
        assert!(!session.anchored);
        assert_eq!(session.head_trim_frames, count_in);
    }

    #[test]
    fn quantise_snaps_starts_and_keeps_durations_musical() {
        // A note played a little late and short: the 1/8 grid snaps its start
        // and its end, so the written duration stays musical.
        let notes = take(
            &[on(quarters(1.06), 60, 100), off(quarters(1.44), 60)],
            Quantise::Eighth,
        );
        assert_eq!(notes.notes.len(), 1);
        assert!((notes.notes[0].start_beat - 1.0).abs() < 1e-6);
        assert!((notes.notes[0].duration_beats - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_1_32_grid_snaps_starts_but_not_ends() {
        let notes = take(
            &[on(quarters(1.01), 60, 100), off(quarters(1.22), 60)],
            Quantise::ThirtySecond,
        );
        assert!((notes.notes[0].start_beat - 1.0).abs() < 1e-6);
        // Only the start snapped, so the played length survives.
        assert!((notes.notes[0].duration_beats - 0.22).abs() < 1e-6);
    }

    #[test]
    fn two_notes_on_the_same_quantised_start_merge() {
        let notes = take(
            &[
                on(quarters(1.01), 60, 60),
                off(quarters(1.20), 60),
                on(quarters(1.04), 60, 110),
                off(quarters(1.60), 60),
            ],
            Quantise::Quarter,
        );
        assert_eq!(notes.notes.len(), 1, "{:?}", notes.notes);
        assert_eq!(notes.notes[0].velocity, 110, "the louder take wins");
        assert!((notes.notes[0].start_beat - 1.0).abs() < 1e-6);
        assert!((notes.notes[0].duration_beats - 1.0).abs() < 1e-6);
        assert_eq!(notes.merged, 1);
    }

    #[test]
    fn a_note_shorter_than_a_64th_is_dropped() {
        // About two milliseconds of key contact is well under a 64th here.
        let notes = take(&[on(1_000, 60, 100), off(1_001, 60)], Quantise::Off);
        assert!(notes.notes.is_empty());
        assert_eq!(notes.dropped_short, 1);
    }

    #[test]
    fn punch_discards_notes_outside_the_range_without_clipping() {
        let clock = clock();
        let notes = transform(&TakeTransform {
            events: &[
                // Starts before the punch in but sounds across it: discarded.
                on(quarters(0.5), 60, 100),
                off(quarters(1.5), 60),
                on(quarters(2.0), 64, 90),
                off(quarters(2.5), 64),
                on(quarters(4.5), 67, 90),
                off(quarters(5.0), 67),
            ],
            clock: &clock,
            rate: RATE,
            record_start_frame: 0,
            stop_frame: quarters(8.0),
            origin_quarter: 0.0,
            head_trim_frames: 0,
            quantise: Quantise::Off,
            punch: Some((1.0, 4.0)),
        })
        .expect("transform");
        assert_eq!(notes.notes.len(), 1);
        assert_eq!(notes.notes[0].key, 64);
        assert_eq!(notes.discarded_punch, 2);
    }

    #[test]
    fn the_sustain_pedal_extends_the_notes_it_covers() {
        let notes = take(
            &[
                on(quarters(1.0), 60, 100),
                CapturedEvent {
                    frame: quarters(1.1),
                    kind: CapturedKind::Sustain { down: true },
                },
                off(quarters(1.2), 60),
                CapturedEvent {
                    frame: quarters(3.0),
                    kind: CapturedKind::Sustain { down: false },
                },
            ],
            Quantise::Off,
        );
        assert_eq!(notes.notes.len(), 1);
        assert!(
            (notes.notes[0].duration_beats - 2.0).abs() < 1e-6,
            "{:?}",
            notes.notes
        );
    }

    #[test]
    fn a_retrigger_ends_the_previous_note() {
        let notes = take(
            &[
                on(quarters(1.0), 60, 100),
                on(quarters(1.5), 60, 80),
                off(quarters(2.0), 60),
            ],
            Quantise::Off,
        );
        assert_eq!(notes.notes.len(), 2);
        assert!((notes.notes[0].duration_beats - 0.5).abs() < 1e-6);
        assert!((notes.notes[1].duration_beats - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_capture_refuses_events_past_its_hard_cap() {
        let capture = MidiCapture::new();
        capture.begin(0, RATE);
        for frame in 0..u64::try_from(MAX_CAPTURED_EVENTS).unwrap() {
            capture.record_at(
                frame,
                CapturedKind::NoteOn {
                    key: 60,
                    velocity: 100,
                },
            );
        }
        capture.record_at(
            1,
            CapturedKind::NoteOn {
                key: 60,
                velocity: 100,
            },
        );
        let take = capture.take();
        assert_eq!(take.events.len(), MAX_CAPTURED_EVENTS);
        assert!(take.overflowed, "the overflow is reported, not grown");
        assert!(
            capture.take().events.is_empty(),
            "the take is closed after the read"
        );
    }

    #[test]
    fn an_event_outside_a_take_is_ignored() {
        let capture = MidiCapture::new();
        capture.record_at(
            0,
            CapturedKind::NoteOn {
                key: 60,
                velocity: 100,
            },
        );
        assert!(capture.take().events.is_empty());
    }

    /// A contended capture lock loses the event. The count reaches the caller
    /// and the take line says why, and a second take starts from zero.
    #[test]
    fn a_contended_capture_counts_the_events_it_dropped() {
        let capture = MidiCapture::new();
        capture.begin(0, RATE);
        capture.record_while_busy(LiveEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        let dropped = capture.take();
        assert_eq!(dropped.dropped, 1, "the loss is reported to the caller");

        capture.begin(0, RATE);
        let clean = capture.take();
        assert_eq!(clean.dropped, 0, "the counter resets with the take");

        let notes = take(&[on(0, 60, 100), off(quarters(1.0), 60)], Quantise::Off);
        let line = take_line("hook", &notes, false, 1);
        assert!(
            line.ends_with("(1 event was dropped because the capture buffer was busy)"),
            "{line}"
        );
        let clean_line = take_line("hook", &notes, false, 0);
        assert!(!clean_line.contains("dropped"), "{clean_line}");
    }

    #[test]
    fn the_capture_timestamps_live_events_from_the_record_position() {
        let capture = MidiCapture::new();
        capture.begin(1_000, RATE);
        capture.record(LiveEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        capture.record(LiveEvent::ControlChange {
            channel: 0,
            controller: 1,
            value: 64,
        });
        let take = capture.take();
        assert_eq!(take.events.len(), 1, "only notes and CC 64 are captured");
        assert_eq!(
            take.events[0].kind,
            CapturedKind::NoteOn {
                key: 60,
                velocity: 100
            }
        );
        assert!(take.events[0].frame >= 1_000);
    }

    #[test]
    fn a_zero_velocity_note_on_is_a_note_off() {
        assert_eq!(
            captured_kind(LiveEvent::NoteOn {
                channel: 0,
                note: 62,
                velocity: 0
            }),
            Some(CapturedKind::NoteOff { key: 62 })
        );
    }

    #[test]
    fn keys_spell_the_written_pitch() {
        assert_eq!(pitch_for_key(60).unwrap().to_text(), "C4");
        assert_eq!(pitch_for_key(61).unwrap().to_text(), "C#4");
        assert_eq!(pitch_for_key(127).unwrap().to_text(), "G9");
        assert_eq!(pitch_for_key(0).unwrap().to_text(), "C-1");
        assert!(pitch_for_key(128).is_err());
        assert!(pitch_for_key(255).is_err());
    }

    #[test]
    fn quantise_names_round_trip() {
        for quantise in [
            Quantise::Off,
            Quantise::ThirtySecond,
            Quantise::Sixteenth,
            Quantise::Eighth,
            Quantise::Quarter,
        ] {
            assert_eq!(Quantise::parse(quantise.as_str()), Some(quantise));
        }
        assert_eq!(Quantise::parse("1/3"), None);
    }

    #[test]
    fn the_settings_report_the_armed_tracks_target_pattern() {
        let clock = clock();
        let mut project = ast::Project {
            name: Some("Test".into()),
            ocp_version: Some("0.2.0".into()),
            properties: ast::ProjectProperties::default(),
            presets: vec![],
            tracks: vec![],
            patterns: vec![],
            buses: vec![],
            master: None,
            sections: vec![],
            markers: vec![],
            groups: vec![],
        };
        let mut lead = instrument_track("Lead");
        lead.push_pattern_clip(ast::Clip {
            pattern: "hook".into(),
            time_range: ast::TimeRange {
                start: BarBeat::new(1, 1, 0),
                end: BarBeat::new(3, 1, 0),
            },
            offset_quarters: 0.0,
            loop_quarters: 0.0,
            id: None,
        });
        project.tracks.push(lead);
        project.patterns.push(ast::PatternDef {
            name: "hook".into(),
            instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
            preset: None,
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: vec![],
            events: vec![],
            pedals: vec![],
            hairpins: Vec::new(),
            hits: vec![],
            color: None,
        });
        let target = target_for(&project, &project.tracks[0], &clock, 0.0).expect("target");
        assert_eq!(target.name(), Some("hook"));
        assert_eq!(target.origin_quarter(), 0.0);
        // A playhead past the clip falls back to the same clip, so notes keep
        // landing at their absolute beat inside the pattern.
        let target = target_for(&project, &project.tracks[0], &clock, 9.0).expect("target");
        assert_eq!(target.name(), Some("hook"));
        // With no clip there is nothing to land in, so a pattern is created.
        let empty = instrument_track("Empty");
        let target = target_for(&project, &empty, &clock, 0.0).expect("target");
        assert!(matches!(target, TakeTarget::New { .. }));
    }

    /// An instrument track with nothing on it yet.
    fn instrument_track(name: &str) -> ast::Track {
        ast::Track {
            name: name.into(),
            instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
            preset: None,
            engine: None,
            mix: ast::MixState::default(),
            effects: vec![],
            phrases: vec![],
            patterns: vec![],
            inline_patterns: vec![],
            clips: oc_dsl::ast::ClipPool::new(),
            gain_automation: Vec::new(),
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        }
    }
}
