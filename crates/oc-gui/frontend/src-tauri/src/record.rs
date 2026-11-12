//! Take recording: arm a track, capture the input into a WAV in the project's
//! media folder, and turn it into an `@audio_clip` at the record position.
//!
//! The capture callback only fills a ring; this module drains it from the
//! transport poll, drops everything the capture holds before the transport
//! rolled (the count-in, the reported latency and the wait for the first
//! block), and writes the take. The file therefore starts at the clip position
//! and the clip carries no media offset, so the performance lands on the beat
//! the click marked.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use oc_audio::click::CountInPlan;
use oc_dsl::ast::{self, BarBeat};
use oc_dsl::timing::ProjectClock;

use crate::commands::shared::{ProjectSnapshot, commit_validated_edit, project_root, snapshot};
use crate::state::{AppStateInner, UndoEdit, UndoScope};

/// Media folder a take is written into, relative to the project file.
pub const MEDIA_DIR: &str = "media";

/// Seconds the head may be held while the transport has not rolled yet. A
/// press that takes longer than this uses the press estimate instead.
const MAX_HEAD_HOLD_SECONDS: u64 = 10;

/// Convert a frame distance between the device clocks, rounding to the nearest frame.
pub(crate) fn convert_frames(frames: u64, from: u32, to: u32) -> u64 {
    let from = u128::from(from.max(1));
    let scaled = (u128::from(frames) * u128::from(to) + from / 2) / from;
    scaled.min(u128::from(u64::MAX)) as u64
}

/// Why a record press was refused, or what a finished take needs to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordRefusal {
    /// No track is armed.
    NotArmed,
    /// No capture device is enumerated.
    NoInput,
    /// The project has no file, so a relative media path has no root.
    UnsavedProject,
    /// The capture stream could not be opened.
    Input(String),
    /// The take could not be written.
    Take(String),
    /// The armed track is an instrument track, which records MIDI instead.
    NotAudioTrack(String),
}

impl RecordRefusal {
    /// Why recording was refused.
    pub fn reason(&self) -> String {
        match self {
            RecordRefusal::NotArmed => "No track is armed for recording.".into(),
            RecordRefusal::NoInput => "No audio input is connected.".into(),
            RecordRefusal::UnsavedProject => {
                "Save the project first: a take needs a media folder.".into()
            }
            RecordRefusal::Input(error) => error.clone(),
            RecordRefusal::Take(error) => error.clone(),
            RecordRefusal::NotAudioTrack(track) => {
                format!("Track '{track}' records MIDI, not audio.")
            }
        }
    }

    /// The one line the transport shows when a press fails.
    pub fn message(&self) -> String {
        let reason = self.reason();
        format!(
            "Recording could not start: {}. The project is unchanged.",
            reason.trim_end_matches('.')
        )
    }
}

/// One pass of a loop-recording session, in project frames and beats.
#[derive(Debug, Clone)]
pub(crate) struct PassPlan {
    pub start_frame: u64,
    pub start_beat: BarBeat,
    /// Frames the pass lasts; `None` means it ends when the transport stops.
    pub frames: Option<u64>,
    /// Exact clip end when the pass has one (the loop end).
    pub end_beat: Option<BarBeat>,
}

/// Punch window in project frames and beats; a take keeps only this span.
#[derive(Debug, Clone)]
pub(crate) struct PunchWindow {
    pub start_frame: u64,
    pub end_frame: u64,
    pub start_beat: BarBeat,
    pub end_beat: BarBeat,
}

/// One take in progress.
pub(crate) struct RecordSession {
    pub track: String,
    /// Absolute path of the WAV being written.
    path: PathBuf,
    /// Path stored on the clip, relative to the project file.
    media: String,
    writer: hound::WavWriter<BufWriter<File>>,
    channels: u16,
    sample_rate: u32,
    project_rate: u32,
    started: Instant,
    /// Project frame the pass starts at.
    pass_start_frame: u64,
    /// Capture frames the pass lasts, when a loop region bounds it.
    pass_frames: Option<u64>,
    /// Frames of this pass offered to the take so far, written or dropped.
    consumed: u64,
    /// Beat the clip starts at.
    clip_start_beat: BarBeat,
    /// Exact beat the clip ends at, when the pass or punch fixes it.
    clip_end_beat: Option<BarBeat>,
    /// Frames already written to the WAV.
    written: u64,
    /// Frames still to drop from the head.
    head_trim: u64,
    /// Frames the head drops when the roll frame is never seen.
    head_estimate: u64,
    /// This pass begins with a count-in. A stop before it ends performs
    /// nothing, so it writes no take.
    count_in: bool,
    /// Frames of this pass before the clip starts (the punch in).
    pass_skip: u64,
    /// Captured frames held until the roll frame is known. Everything before
    /// the roll belongs to no take, so it is never written.
    head: Vec<f32>,
    /// Frames the head may hold before the estimate is used instead.
    head_limit: u64,
    /// The roll frame has been resolved (or the estimate was accepted).
    anchored: bool,
    /// Largest number of frames this take writes; `None` writes to the pass end.
    write_cap: Option<u64>,
    /// Frames the capture ring had to drop because the pump fell behind.
    dropped: u64,
    /// One-based pass number inside `take_group`.
    take: u32,
    /// Session group when the take belongs to a loop session.
    take_group: Option<String>,
    /// Test hook: fail every write, as a full disk would.
    #[cfg(test)]
    fail_writes: bool,
}

/// What a finished take captured, before it becomes a clip.
pub(crate) struct FinishedTake {
    pub track: String,
    pub media: String,
    pub path: PathBuf,
    pub sample_rate: u32,
    pub written_frames: u64,
    pub dropped_frames: u64,
    pub clip_start_beat: BarBeat,
    pub clip_end_beat: Option<BarBeat>,
    pub take: u32,
    pub take_group: Option<String>,
}

impl RecordSession {
    /// Start a take on `track`, writing `media/take-<track>-<take>.wav` under
    /// `root`. `head_trim_frames` is the count-in plus the reported latency
    /// in project frames,
    /// applied only to the first pass of a session; the press-to-roll wait is
    /// resolved from the transport position once the take is rolling.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        root: &Path,
        track: &str,
        channels: u16,
        sample_rate: u32,
        project_rate: u32,
        plan: &PassPlan,
        punch: Option<&PunchWindow>,
        head_trim_frames: u64,
        count_in: bool,
        take: u32,
        take_group: Option<String>,
    ) -> Result<Self, RecordRefusal> {
        let media_dir = root.join(MEDIA_DIR);
        std::fs::create_dir_all(&media_dir).map_err(|error| {
            RecordRefusal::Take(format!("create {}: {error}", media_dir.display()))
        })?;
        let pass_end = plan
            .frames
            .map(|frames| plan.start_frame.saturating_add(frames));
        let punch_start = punch.map_or(plan.start_frame, |punch| {
            plan.start_frame.max(punch.start_frame)
        });
        let open_end = match (pass_end, punch) {
            (Some(pass), Some(punch)) => Some(pass.min(punch.end_frame)),
            (Some(pass), None) => Some(pass),
            (None, Some(punch)) => Some(punch.end_frame),
            (None, None) => None,
        };
        let clip_end_frame = open_end;
        let capture_frames = |position: u64| {
            convert_frames(
                position.saturating_sub(plan.start_frame),
                project_rate,
                sample_rate,
            )
        };
        let punch_skip = capture_frames(punch_start);
        let write_cap = clip_end_frame.map(|end| capture_frames(end).saturating_sub(punch_skip));
        let clip_start_beat = match punch {
            Some(punch) if punch_start > plan.start_frame => punch.start_beat,
            _ => plan.start_beat,
        };
        let clip_end_beat = match (clip_end_frame, punch) {
            (Some(end), Some(punch))
                if end <= punch.end_frame && pass_end.is_none_or(|pass| end < pass) =>
            {
                Some(punch.end_beat)
            }
            (Some(_), _) => plan.end_beat,
            (None, _) => None,
        };
        // The file always starts at the clip position, so the head drops the
        // count-in, the latency and the part of the pass before the punch in.
        // The press-to-roll wait is resolved once the transport rolls.
        let head_trim_frames = convert_frames(head_trim_frames, project_rate, sample_rate);
        let head_estimate = head_trim_frames;
        let head_limit = head_trim_frames
            .saturating_add(MAX_HEAD_HOLD_SECONDS.saturating_mul(u64::from(sample_rate.max(1))));
        let spec = hound::WavSpec {
            channels: channels.max(1),
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let (name, path, file) = create_take_file(&media_dir, track, take)?;
        let writer = hound::WavWriter::new(BufWriter::new(file), spec).map_err(|error| {
            let _ = std::fs::remove_file(&path);
            RecordRefusal::Take(format!("create {}: {error}", path.display()))
        })?;
        Ok(Self {
            track: track.to_string(),
            path,
            media: format!("{MEDIA_DIR}/{name}"),
            writer,
            channels: channels.max(1),
            sample_rate: sample_rate.max(1),
            project_rate,
            started: Instant::now(),
            pass_start_frame: plan.start_frame,
            pass_frames: pass_end.map(capture_frames),
            consumed: 0,
            clip_start_beat,
            clip_end_beat,
            written: 0,
            head_trim: head_estimate,
            head_estimate,
            count_in,
            pass_skip: punch_skip,
            head: Vec::new(),
            head_limit,
            // A later loop pass starts on a running transport, so it has no
            // roll frame to wait for. The first pass always does.
            anchored: take > 1,
            write_cap,
            dropped: 0,
            take,
            take_group,
            #[cfg(test)]
            fail_writes: false,
        })
    }

    /// Test hook: make every write fail, as a full disk would.
    #[cfg(test)]
    pub(crate) fn fail_writes(&mut self) {
        self.fail_writes = true;
    }

    pub(crate) fn seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Hold raw input until the first pass can be aligned. Only frames after
    /// the roll enter the pass budget, including the part before a punch in.
    fn prepare_samples(
        &mut self,
        samples: &mut Vec<f32>,
        playhead: u64,
        input_block: u64,
        finishing: bool,
    ) -> bool {
        if self.anchored {
            return true;
        }
        self.head.append(samples);
        let channels = usize::from(self.channels);
        let held = (self.head.len() / channels) as u64;
        let played = playhead.saturating_sub(self.pass_start_frame);
        let trim = if played > 0 {
            held.saturating_sub(convert_frames(played, self.project_rate, self.sample_rate))
                .saturating_add(input_block)
        } else if !self.count_in && (finishing || held >= self.head_limit) {
            self.head_estimate
        } else {
            // An unusually long count-in wait must not grow the buffer forever.
            // Retain the newest input, since that is the part a roll can use.
            let excess = held.saturating_sub(self.head_limit) as usize * channels;
            if excess > 0 {
                self.head.drain(..excess);
            }
            return false;
        };
        self.head_trim = trim;
        self.anchored = true;
        std::mem::swap(samples, &mut self.head);
        true
    }

    /// Write the part of `samples` belonging to this pass. The caller sends
    /// the remaining frames to the next take.
    fn feed(&mut self, samples: &[f32]) -> Result<usize, String> {
        let channels = usize::from(self.channels);
        let frames = samples.len() / channels;
        let preroll = (self.head_trim as usize).min(frames);
        self.head_trim -= preroll as u64;
        let pass_frames = self.pass_frames.map_or(frames - preroll, |pass| {
            (pass.saturating_sub(self.consumed) as usize).min(frames - preroll)
        });
        let skip = (self.pass_skip.saturating_sub(self.consumed) as usize).min(pass_frames);
        let room = self
            .write_cap
            .map_or(u64::MAX, |cap| cap.saturating_sub(self.written));
        let write = (pass_frames - skip).min(room as usize);
        if write > 0 {
            #[cfg(test)]
            if self.fail_writes {
                return Err(format!(
                    "write {}: no space left on device",
                    self.path.display()
                ));
            }
            let start = (preroll + skip) * channels;
            for sample in &samples[start..start + write * channels] {
                self.writer
                    .write_sample(*sample)
                    .map_err(|error| format!("write {}: {error}", self.path.display()))?;
            }
            self.written += write as u64;
        }
        self.consumed += pass_frames as u64;
        Ok((preroll + pass_frames) * channels)
    }

    /// Close the file without committing a take.
    fn discard(self) {
        let RecordSession { path, writer, .. } = self;
        drop(writer);
        let _ = std::fs::remove_file(path);
    }

    fn at_pass_end(&self) -> bool {
        self.pass_frames
            .is_some_and(|frames| self.consumed >= frames)
    }

    /// Stop cutting the take at the loop end; the transport no longer loops.
    fn open_ended(&mut self) {
        self.pass_frames = None;
        self.write_cap = None;
        self.clip_end_beat = None;
    }

    /// Cut the take at `frames` from the pass start; used when Loop is armed
    /// after recording began.
    fn bound_to_pass(&mut self, frames: u64, end_beat: Option<BarBeat>) {
        let frames = convert_frames(frames, self.project_rate, self.sample_rate);
        self.pass_frames = Some(frames);
        self.write_cap = Some(
            self.write_cap
                .unwrap_or(u64::MAX)
                .min(frames.saturating_sub(self.pass_skip)),
        );
        self.clip_end_beat = end_beat;
    }

    /// Close the WAV and report what it holds.
    fn finish(self) -> Result<FinishedTake, String> {
        self.writer
            .finalize()
            .map_err(|error| format!("close {}: {error}", self.path.display()))?;
        Ok(FinishedTake {
            track: self.track,
            media: self.media,
            path: self.path,
            sample_rate: self.sample_rate,
            written_frames: self.written,
            dropped_frames: self.dropped,
            clip_start_beat: self.clip_start_beat,
            clip_end_beat: self.clip_end_beat,
            take: self.take,
            take_group: self.take_group,
        })
    }
}

/// Claim the take name atomically so two sessions can share a media folder.
fn create_take_file(
    dir: &Path,
    track: &str,
    take: u32,
) -> Result<(String, PathBuf, File), RecordRefusal> {
    let slug: String = track
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_ascii_lowercase();
    let slug = if slug.is_empty() {
        "track".to_string()
    } else {
        slug
    };
    let preferred = format!("take-{slug}-{take}.wav");
    let alternates = (2..).map(|index| format!("take-{slug}-{take}-{index}.wav"));
    for name in std::iter::once(preferred).chain(alternates) {
        let path = dir.join(&name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((name, path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(RecordRefusal::Take(format!(
                    "create {}: {error}",
                    path.display()
                )));
            }
        }
    }
    unreachable!("the take suffix always finds a free name")
}

/// The head trim of a take: the count-in plus the reported latency.
pub(crate) fn head_trim_seconds(plan: &CountInPlan, rate: u32, latency_seconds: f64) -> f64 {
    let count_in = plan.frames as f64 / f64::from(rate.max(1));
    count_in + latency_seconds
}

/// The smallest beat at or after `quarter`, so a take's tail is never cut.
pub(crate) fn ceil_bar_beat(clock: &ProjectClock, quarter: f64) -> Result<BarBeat, String> {
    let position = clock.bar_beat_floor_at_quarter(quarter)?;
    let floor = clock.quarter_notes_at(&position)?;
    if quarter - floor <= 1e-9 {
        return Ok(position);
    }
    let signature = clock.meter_at_bar(position.bar)?;
    let mut bar = position.bar;
    let mut beat = position.beat;
    let mut subdivision = position.subdivision + 1;
    if subdivision >= 4 {
        subdivision = 0;
        beat += 1;
    }
    if beat > u32::from(signature.numerator) {
        beat = 1;
        bar += 1;
    }
    Ok(BarBeat::new(bar, beat, subdivision))
}

/// Turn a finished take into an `@audio_clip` on the armed track.
///
/// The clip starts at the pass position or the punch in, and the file starts
/// there too: the take dropped the count-in, the latency and the press-to-roll
/// wait, so the clip carries no media offset. Without an exact end the range
/// rounds up to the next beat subdivision, so the recorded tail is always
/// inside the clip.
fn take_clip(project: &ast::Project, take: &FinishedTake) -> Result<ast::AudioClip, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let start_quarter = clock.quarter_notes_at(&take.clip_start_beat)?;
    let start_seconds = clock.seconds_at_quarter(start_quarter)?;
    let take_seconds = take.written_frames as f64 / f64::from(take.sample_rate.max(1));
    if take_seconds <= 0.0 {
        return Err("The take is empty.".into());
    }
    let clip_seconds = take_seconds;
    let end = match take.clip_end_beat {
        Some(end) if clock.quarter_notes_at(&end)? > start_quarter + 1e-9 => end,
        _ => {
            let end_quarter = clock.quarter_at_seconds(start_seconds + clip_seconds)?;
            let mut end = ceil_bar_beat(&clock, end_quarter)?;
            if end == take.clip_start_beat {
                let signature = clock.meter_at_bar(end.bar)?;
                end = if end.subdivision + 1 < 4 {
                    BarBeat::new(end.bar, end.beat, end.subdivision + 1)
                } else if end.beat < u32::from(signature.numerator) {
                    BarBeat::new(end.bar, end.beat + 1, 0)
                } else {
                    BarBeat::new(end.bar + 1, 1, 0)
                };
            }
            end
        }
    };
    Ok(ast::AudioClip {
        name: unique_clip_name(project, &take.track),
        track: take.track.clone(),
        file: take.media.clone(),
        time_range: ast::TimeRange {
            start: take.clip_start_beat,
            end,
        },
        // The file starts at the clip position, so there is no media offset.
        placement: Default::default(),
        offset_seconds: 0.0,
        loop_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
        invert: false,
        crossfade_seconds: 0.0,
        stretch: 1.0,
        stretch_mode: ast::StretchMode::default(),
        tempo_follow: false,
        pitch: 0.0,
        formant: 0.0,
        bypass_processing: false,
        preserve_formants: false,
        warp_markers: Vec::new(),
        take_group: take.take_group.clone(),
        take: take.take_group.as_ref().map(|_| take.take),
        muted: false,
        gain_envelope: Vec::new(),
    })
}

fn unique_clip_name(project: &ast::Project, track: &str) -> String {
    let base = format!("{track} take");
    for index in 1.. {
        let candidate = format!("{base} {index}");
        if !project
            .audio_clips()
            .iter()
            .any(|clip| clip.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("the take counter always finds a free name")
}

/// Record every armed track, or the one named, for the length of one press.
/// The commands drive this; it lives here so the state machine is testable.
#[derive(Default)]
pub(crate) struct Recorder {
    /// Track armed for recording; at most one, since multi-track arm is out.
    pub armed: Option<String>,
    pub session: Option<RecordSession>,
    /// Passes of this session already closed and waiting for the stop.
    pub(crate) finished: Vec<FinishedTake>,
    /// The last refusal or take report, shown until the next press.
    pub report: Option<String>,
    #[cfg(test)]
    pub(crate) fail_next_pass_open: bool,
}

impl Recorder {
    pub(crate) fn recording(&self) -> bool {
        self.session.is_some() || !self.finished.is_empty()
    }

    pub(crate) fn seconds(&self) -> f64 {
        self.session
            .as_ref()
            .map_or(0.0, |session| session.seconds())
    }

    fn reset_takes(&mut self) {
        self.session = None;
        self.finished.clear();
    }
}

/// What an armed track records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    /// An `audio/…` track: capture the input into a WAV take.
    #[default]
    Audio,
    /// An instrument track: capture MIDI into a pattern.
    Midi,
}

/// The record state the transport reads.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RecordState {
    pub armed: Option<String>,
    pub recording: bool,
    pub seconds: f64,
    pub report: Option<String>,
    /// Why recording cannot start, when it cannot.
    pub disabled_reason: Option<String>,
    /// Frames of count-in still to sound.
    pub count_in_remaining_frames: u64,
    /// What the armed track will record.
    pub mode: RecordMode,
    /// The MIDI half: target pattern, quantise, overdub and the selected input.
    pub midi: crate::midi_record::MidiRecordView,
    /// The capture device an audio take would use, for the arm chip.
    pub audio_device: Option<String>,
}

/// Drain the take for one poll, and finalise it when the capture stream died.
///
/// A take interrupted by a capture error is kept: the frames already written
/// are the performance the user played, and discarding them would lose work.
pub(crate) fn poll_inner(inner: &mut AppStateInner) -> Result<(), String> {
    let playhead = inner.audio.audio_take_progress_frame();
    poll_at(inner, playhead)
}

/// Drain the capture ring at `playhead`. Tests drive the transport position
/// directly; production reads it from the audio state.
pub(crate) fn poll_at(inner: &mut AppStateInner, playhead: u64) -> Result<(), String> {
    let capture_failure = inner.audio.take_capture_failure();
    if !inner.audio.recorder.recording() {
        return Ok(());
    }
    if let Some(error) = capture_failure {
        stop_inner(inner)?;
        say(
            inner,
            Some(format!("Recording stopped: {error}. The take was kept.")),
        );
        return Ok(());
    }
    if let Err(error) = pump_capture(inner, playhead, false) {
        recover_completed_passes(inner, &error)?;
    }
    Ok(())
}

/// Align and distribute input with the same rules during polling and Stop.
fn pump_capture(inner: &mut AppStateInner, playhead: u64, finishing: bool) -> Result<(), String> {
    let port = std::sync::Arc::clone(&inner.audio.input_port);
    let mut samples = Vec::new();
    port.take_recorded(&mut samples, 1 << 20);
    if samples.is_empty() && !finishing {
        return Ok(());
    }
    ensure_loop_state(inner)?;
    let Some(session) = inner.audio.recorder.session.as_mut() else {
        return Ok(());
    };
    session.dropped = session.dropped.max(port.dropped_frames());
    if !session.prepare_samples(&mut samples, playhead, port.block_frames(), finishing)
        || samples.is_empty()
    {
        return Ok(());
    }
    let rate = inner.audio.sample_rate;
    let mut offset = 0usize;
    while let Some(session) = inner.audio.recorder.session.as_mut() {
        offset += session.feed(&samples[offset..])?;
        if !session.at_pass_end() {
            break;
        }
        let done = inner
            .audio
            .recorder
            .session
            .take()
            .expect("a finished pass is still open");
        let track = done.track.clone();
        let take = done.take;
        let group = done.take_group.clone();
        finish_pass(inner, done)?;
        if finishing && offset >= samples.len() {
            break;
        }
        let Some((start, end)) = inner.audio.active_loop_frames() else {
            break;
        };
        let clock = ProjectClock::new(&inner.project.properties)?;
        let Some(plan) = loop_pass_plan(&clock, start, end, rate)? else {
            break;
        };
        let session = open_session(
            inner,
            &track,
            &plan,
            0,
            false,
            take.saturating_add(1),
            group,
        )
        .map_err(|refusal| refusal.reason())?;
        inner.audio.recorder.session = Some(session);
        if offset >= samples.len() {
            break;
        }
    }
    Ok(())
}

/// `true` when Loop is armed; the pass the current take belongs to is bounded
/// by the loop region from that moment.
fn ensure_loop_state(inner: &mut AppStateInner) -> Result<(), String> {
    let loop_frames = inner.audio.active_loop_frames();
    let rate = inner.audio.sample_rate;
    let clock = ProjectClock::new(&inner.project.properties)?;
    let Some(session) = inner.audio.recorder.session.as_mut() else {
        return Ok(());
    };
    let Some((_start, end)) = loop_frames else {
        if session.pass_frames.is_some() {
            session.open_ended();
        }
        return Ok(());
    };
    if session.take_group.is_none() {
        session.take_group = Some(take_group_name(&session.track));
        session.take = 1;
    }
    if session.pass_frames.is_none() {
        let frames = end.saturating_sub(session.pass_start_frame);
        if frames > 0 {
            let end_beat =
                clock.bar_beat_floor_at_quarter(clock.position_at_sample(end, rate)?.quarter)?;
            session.bound_to_pass(frames, Some(end_beat));
        }
    }
    Ok(())
}

/// Pass boundaries for a loop that starts at `start` and ends at `end`.
fn loop_pass_plan(
    clock: &ProjectClock,
    start: u64,
    end: u64,
    rate: u32,
) -> Result<Option<PassPlan>, String> {
    if end <= start {
        return Ok(None);
    }
    let start_beat =
        clock.bar_beat_floor_at_quarter(clock.position_at_sample(start, rate)?.quarter)?;
    let end_beat = clock.bar_beat_floor_at_quarter(clock.position_at_sample(end, rate)?.quarter)?;
    Ok(Some(PassPlan {
        start_frame: start,
        start_beat,
        frames: Some(end - start),
        end_beat: Some(end_beat),
    }))
}

/// The session's punch window in frames and beats, when one is armed.
fn punch_window(
    inner: &AppStateInner,
    clock: &ProjectClock,
    rate: u32,
) -> Result<Option<PunchWindow>, String> {
    if !inner.audio.punch_enabled {
        return Ok(None);
    }
    let Some((start, end)) = inner.audio.punch_region else {
        return Ok(None);
    };
    if end <= start {
        return Ok(None);
    }
    Ok(Some(PunchWindow {
        start_frame: clock.sample_at_quarter(start, rate)?,
        end_frame: clock.sample_at_quarter(end, rate)?,
        start_beat: clock.bar_beat_floor_at_quarter(start)?,
        end_beat: clock.bar_beat_floor_at_quarter(end)?,
    }))
}

/// Open one pass of a take, resolving the punch window against the pass.
fn open_session(
    inner: &AppStateInner,
    track: &str,
    plan: &PassPlan,
    head_trim_frames: u64,
    count_in: bool,
    take: u32,
    take_group: Option<String>,
) -> Result<RecordSession, RecordRefusal> {
    #[cfg(test)]
    if take > 1 && inner.audio.recorder.fail_next_pass_open {
        return Err(RecordRefusal::Take(
            "no space left for the next take".into(),
        ));
    }
    let root = crate::commands::shared::project_root(inner).ok_or(RecordRefusal::UnsavedProject)?;
    let (capture_rate, channels) = inner.audio.capture_format().ok_or(RecordRefusal::NoInput)?;
    let project_rate = inner.audio.sample_rate;
    let clock = ProjectClock::new(&inner.project.properties).map_err(RecordRefusal::Take)?;
    let punch = punch_window(inner, &clock, project_rate).map_err(RecordRefusal::Take)?;
    RecordSession::start(
        &root,
        track,
        channels,
        capture_rate,
        project_rate,
        plan,
        punch.as_ref(),
        head_trim_frames,
        count_in,
        take,
        take_group,
    )
}

/// Group stamped on every pass of one loop session.
fn take_group_name(track: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("{track} take {millis}")
}

/// The transport line a refused press shows, or the last take report.
pub(crate) fn report_line(inner: &AppStateInner) -> Option<String> {
    inner
        .audio
        .midi_recorder
        .report
        .clone()
        .or_else(|| inner.audio.recorder.report.clone())
}

/// Set the take line. Only the recorder that just spoke keeps its line, so a
/// stale MIDI report cannot hide a new audio take, or the other way round.
pub(crate) fn say(inner: &mut AppStateInner, message: Option<String>) {
    inner.audio.recorder.report = message;
    inner.audio.midi_recorder.report = None;
}

/// What the armed track records: an `audio/…` track records the input, every
/// other track records MIDI from its instrument.
pub(crate) fn mode_of(inner: &AppStateInner) -> RecordMode {
    let Some(name) = inner.audio.recorder.armed.as_deref() else {
        return RecordMode::Audio;
    };
    match inner.project.tracks.iter().find(|track| track.name == name) {
        Some(track) if crate::midi_record::is_audio_track(track) => RecordMode::Audio,
        Some(_) => RecordMode::Midi,
        None => RecordMode::Audio,
    }
}

/// Everything the transport needs to decide whether recording can start.
pub(crate) fn disabled_reason(inner: &AppStateInner) -> Option<String> {
    // A take commits a clip; a preview holds the document read-only.
    if let Some(reason) = crate::commands::shared::preview_refusal(inner) {
        return Some(reason);
    }
    if inner.project.tracks.is_empty() {
        return Some("Add a track first.".to_string());
    }
    // Arming is always possible: the refusal belongs to the armed track, and a
    // track that is not armed yet has no device to be missing.
    inner.audio.recorder.armed.as_ref()?;
    match mode_of(inner) {
        RecordMode::Midi => {
            if inner.midi_live.input().is_none() {
                return Some(crate::midi_record::MidiRefusal::NoInput.reason());
            }
        }
        RecordMode::Audio => {
            if !inner.audio.audio_io.has_inputs() {
                return Some("No audio input is connected.".to_string());
            }
            if inner.file_path.is_none() {
                return Some("Save the project first: a take needs a media folder.".to_string());
            }
        }
    }
    None
}

/// The capture device an audio take would use, for the arm chip.
fn audio_device(inner: &AppStateInner) -> Option<String> {
    let status = inner.audio.audio_io.status();
    status
        .opened_input
        .as_ref()
        .map(|opened| opened.name.clone())
        .or(status.input.input_device_name.clone())
}

/// The take pump plus the transport fields the UI polls.
pub(crate) fn state_of(inner: &AppStateInner) -> RecordState {
    let midi_recording = inner.audio.midi_recorder.recording();
    RecordState {
        armed: inner.audio.recorder.armed.clone(),
        recording: inner.audio.recorder.recording() || midi_recording,
        seconds: if midi_recording {
            inner.audio.midi_recorder.seconds()
        } else {
            inner.audio.recorder.seconds()
        },
        report: report_line(inner),
        disabled_reason: disabled_reason(inner),
        count_in_remaining_frames: 0,
        mode: mode_of(inner),
        midi: crate::midi_record::view(inner),
        audio_device: audio_device(inner),
    }
}

/// Start a take. The caller holds the app lock.
pub(crate) fn start_inner(inner: &mut AppStateInner, rate: u32) -> Result<RecordState, String> {
    if inner.audio.recorder.recording() {
        return Ok(state_of(inner));
    }
    // The take commits a clip, so a preview must be finished first. Nothing is
    // opened, armed or written for a refused press.
    if let Some(reason) = crate::commands::shared::preview_refusal(inner) {
        say(inner, Some(reason.clone()));
        return Err(reason);
    }
    let Some(track) = inner.audio.recorder.armed.clone() else {
        let refusal = RecordRefusal::NotArmed;
        say(inner, Some(refusal.message()));
        return Err(refusal.reason());
    };
    // The arm rule sends an instrument track to the MIDI recorder; arriving
    // here with one means a caller skipped the dispatch.
    if mode_of(inner) == RecordMode::Midi {
        let refusal = RecordRefusal::NotAudioTrack(track);
        say(inner, Some(refusal.message()));
        return Err(refusal.reason());
    }
    // A device that was unplugged since the list was read must be seen here.
    let _ = inner.audio.take_capture_failure();
    let _ = inner.audio.audio_io.refresh_inputs();
    if let Some(reason) = disabled_reason(inner) {
        let refusal = if reason == RecordRefusal::UnsavedProject.reason() {
            RecordRefusal::UnsavedProject
        } else {
            RecordRefusal::NoInput
        };
        say(inner, Some(refusal.message()));
        return Err(reason);
    }
    let clock = ProjectClock::new(&inner.project.properties)?;
    let playhead = inner.audio.timeline_cursor_frame();
    let quarter = clock.position_at_sample(playhead, rate)?.quarter;
    let start_beat = clock.bar_beat_floor_at_quarter(quarter)?;
    inner.audio.ensure_input();
    if inner.audio.input_view().error.is_some() {
        let refusal =
            RecordRefusal::Input(inner.audio.input_view().error.clone().unwrap_or_default());
        say(inner, Some(refusal.message()));
        return Err(refusal.reason());
    }
    let Some((_, channels)) = inner.audio.capture_format() else {
        let refusal = RecordRefusal::NoInput;
        say(inner, Some(refusal.message()));
        return Err(refusal.reason());
    };
    let latency_seconds = f64::from(inner.audio.input_latency_frames()) / f64::from(rate.max(1));
    let plan =
        oc_audio::click::count_in_plan(&clock, inner.audio.click.count_in.bars(), rate, quarter)?;
    let trim = head_trim_seconds(&plan, rate, latency_seconds);
    let head_trim_frames = (trim * f64::from(rate)).round() as u64;
    let loop_frames = inner.audio.active_loop_frames();
    let pass = match loop_frames {
        Some((_start, end)) if end > playhead => {
            let end_beat =
                clock.bar_beat_floor_at_quarter(clock.position_at_sample(end, rate)?.quarter)?;
            PassPlan {
                start_frame: playhead,
                start_beat,
                frames: Some(end - playhead),
                end_beat: Some(end_beat),
            }
        }
        _ => PassPlan {
            start_frame: playhead,
            start_beat,
            frames: None,
            end_beat: None,
        },
    };
    let take_group = loop_frames.map(|_| take_group_name(&track));
    inner.audio.recorder.reset_takes();
    let session = open_session(
        inner,
        &track,
        &pass,
        head_trim_frames,
        plan.frames > 0,
        1,
        take_group,
    )
    .map_err(|refusal| {
        let message = refusal.message();
        say(inner, Some(message));
        refusal.reason()
    })?;
    inner.audio.recorder.session = Some(session);
    say(inner, None);
    inner.audio.input_port.set_recording_channels(channels);
    // The count-in reaches the render thread on the play that follows.
    inner.audio.pending_count_in = (plan.frames > 0).then_some(plan);
    inner.audio.seek_frame(playhead, &clock)?;
    Ok(state_of(inner))
}

/// Drop a take that never reached the transport: close and remove the file,
/// clear the session and stop the capture, so a second press starts fresh.
pub(crate) fn cancel_inner(inner: &mut AppStateInner) {
    inner.audio.input_port.set_recording_channels(0);
    inner
        .audio
        .input_port
        .take_recorded(&mut Vec::new(), 1 << 20);
    if let Some(session) = inner.audio.recorder.session.take() {
        session.discard();
    }
    for take in std::mem::take(&mut inner.audio.recorder.finished) {
        let _ = std::fs::remove_file(&take.path);
    }
    inner.audio.pending_count_in = None;
}

/// Stop the take, write the clips and clear the recording state.
///
/// A loop session commits one clip per pass. The newest pass is unmuted and
/// every earlier pass in the group is muted, so the track sounds like the last
/// pass until the user comps. Empty passes write no clip and are removed.
pub(crate) fn stop_inner(inner: &mut AppStateInner) -> Result<ProjectSnapshot, String> {
    let playhead = inner.audio.audio_take_progress_frame();
    if let Err(error) = finish_inner(inner, playhead) {
        return recover_completed_passes(inner, &error)?.ok_or(error);
    }
    commit_finished_takes(inner)
}

fn recover_completed_passes(
    inner: &mut AppStateInner,
    error: &str,
) -> Result<Option<ProjectSnapshot>, String> {
    abort_session(inner, error);
    if inner.audio.recorder.finished.is_empty() {
        return Ok(None);
    }
    let revision = inner.revision;
    let snapshot = commit_finished_takes(inner)?;
    if revision == inner.revision {
        say(inner, Some(format!("Recording failed: {error}.")));
    } else if let Some(report) = inner.audio.recorder.report.clone() {
        say(inner, Some(format!("Recording stopped: {error}. {report}")));
    }
    Ok(Some(snapshot))
}

fn commit_finished_takes(inner: &mut AppStateInner) -> Result<ProjectSnapshot, String> {
    let finished = std::mem::take(&mut inner.audio.recorder.finished);
    if finished.is_empty() {
        say(inner, None);
        return inner
            .audio
            .transport_snapshot(&ProjectClock::new(&inner.project.properties)?)
            .map(|_| snapshot(inner));
    }
    let mut takes = Vec::with_capacity(finished.len());
    for take in finished {
        if take.written_frames == 0 {
            let _ = std::fs::remove_file(&take.path);
            continue;
        }
        takes.push(take);
    }
    if takes.is_empty() {
        say(
            inner,
            Some("Nothing was recorded: the input was silent.".into()),
        );
        return Ok(snapshot(inner));
    }
    let newest = takes.iter().map(|take| take.take).max().unwrap_or(1);
    let session_takes = takes.iter().any(|take| take.take_group.is_some());
    let label = if takes.len() == 1 {
        "record take".to_string()
    } else {
        format!("record {} takes", takes.len())
    };
    let root = project_root(inner);
    let result = commit_validated_edit(inner, UndoEdit::new(&label, UndoScope::Project), |next| {
        for take in &takes {
            if root
                .as_ref()
                .is_none_or(|root| root.join(&take.media) != take.path)
            {
                return Err("The project folder changed during recording.".into());
            }
            let mut clip = take_clip(next, take)?;
            clip.muted = take.take_group.is_some() && take.take != newest;
            next.add_audio_clip(clip)
                .map_err(|error| format!("The take clip could not be placed: {error}"))?;
        }
        Ok(())
    });
    if let Err(error) = result {
        let files = takes
            .iter()
            .map(|take| take.path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let line =
            format!("Recorded audio is on disk ({files}). The project could not add it. {error}");
        say(inner, Some(line.clone()));
        return Err(line);
    }
    let line = if session_takes {
        take_report(inner, &takes, newest)
    } else {
        let name = inner
            .project
            .audio_clips()
            .into_iter()
            .last()
            .map_or(takes[0].track.as_str(), |clip| clip.name.as_str());
        single_take_report(&takes[0], name)
    };
    say(inner, Some(line));
    Ok(snapshot(inner))
}

/// Finalise the open take at `playhead`: drain the capture, resolve the head,
/// close the file and queue the take.
///
/// A write that fails closes the take and drops it here, so neither Stop nor
/// the transport poll ever leaves a session open.
fn finish_inner(inner: &mut AppStateInner, playhead: u64) -> Result<(), String> {
    inner.audio.input_port.set_recording_channels(0);
    pump_capture(inner, playhead, true)?;
    if let Some(session) = inner.audio.recorder.session.take() {
        if session.anchored {
            finish_pass(inner, session)?;
        } else {
            // Stopping before the count-in rolled has no take to keep.
            session.discard();
        }
    }
    Ok(())
}

fn finish_pass(inner: &mut AppStateInner, session: RecordSession) -> Result<(), String> {
    let path = session.path.clone();
    match session.finish() {
        Ok(take) => {
            inner.audio.recorder.finished.push(take);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

/// Drop the open take after a failed write: the file cannot be trusted, so it
/// is closed and removed instead of being committed.
fn abort_session(inner: &mut AppStateInner, error: &str) {
    if let Some(session) = inner.audio.recorder.session.take() {
        session.discard();
    }
    inner.audio.input_port.set_recording_channels(0);
    inner
        .audio
        .input_port
        .take_recorded(&mut Vec::new(), 1 << 20);
    inner.audio.pending_count_in = None;
    say(
        inner,
        Some(format!(
            "Recording failed: {error}. The project is unchanged."
        )),
    );
}

/// Report for one ordinary take, unchanged from the single-take behaviour.
fn single_take_report(take: &FinishedTake, name: &str) -> String {
    if take.dropped_frames > 0 {
        format!(
            "Recorded {} to {}. {} frames were dropped because the take pump fell behind.",
            name, take.media, take.dropped_frames
        )
    } else {
        format!(
            "Recorded {} to {} ({} frames at {} Hz).",
            name, take.media, take.written_frames, take.sample_rate
        )
    }
}

/// Report for a loop session: how many takes landed, the range they cover and
/// which pass is playing.
fn take_report(inner: &AppStateInner, takes: &[FinishedTake], newest: u32) -> String {
    let noun = if takes.len() == 1 { "take" } else { "takes" };
    let clock = match ProjectClock::new(&inner.project.properties) {
        Ok(clock) => clock,
        Err(_) => {
            return format!("Recorded {} {noun}. Take {newest} is playing.", takes.len());
        }
    };
    let seconds = |beat: &BarBeat| {
        clock
            .quarter_notes_at(beat)
            .ok()
            .and_then(|quarter| clock.seconds_at_quarter(quarter).ok())
            .unwrap_or(0.0)
    };
    let mut start = f64::INFINITY;
    let mut end = 0.0_f64;
    for take in takes {
        let from = seconds(&take.clip_start_beat);
        let to = seconds(take.clip_end_beat.as_ref().unwrap_or(&take.clip_start_beat));
        start = start.min(from);
        end = end.max(to);
    }
    let dropped: u64 = takes.iter().map(|take| take.dropped_frames).sum();
    let mut line = format!(
        "Recorded {} {noun} ({}–{}). Take {newest} is playing; open Takes to comp.",
        takes.len(),
        format_clock(start),
        format_clock(end)
    );
    if dropped > 0 {
        line.push_str(&format!(
            " {dropped} frames were dropped because the take pump fell behind."
        ));
    }
    line
}

fn format_clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}
