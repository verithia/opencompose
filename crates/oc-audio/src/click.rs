//! The audition click and its count-in, in sample positions on the project
//! clock.
//!
//! This module is only used by the streaming transport: an offline render never
//! builds a [`ClickTrack`], so a render of a project is byte-identical whether
//! the click is on or off. Beat positions are computed from [`ProjectClock`],
//! which honours tempo and meter maps.

use serde::{Deserialize, Serialize};

use oc_dsl::timing::ProjectClock;

/// Click length: 30 ms is audible at 300 BPM and never masks the next beat.
pub const CLICK_SECONDS: f64 = 0.030;

/// Decay constant of the click envelope.
const CLICK_DECAY_SECONDS: f64 = 0.006;

/// Frequency of the accented click and of the plain one.
const ACCENT_HZ: f64 = 1_760.0;
const PLAIN_HZ: f64 = 1_200.0;

/// Guard against a malformed meter map that never reaches the window.
const MAX_BARS: usize = 512;

/// How many bars of count-in precede a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CountIn {
    #[default]
    Off,
    OneBar,
    TwoBars,
}

impl CountIn {
    /// Number of count-in bars, zero when off.
    pub fn bars(self) -> u8 {
        match self {
            CountIn::Off => 0,
            CountIn::OneBar => 1,
            CountIn::TwoBars => 2,
        }
    }

    /// The stored spelling, also used as the wire value.
    pub fn key(self) -> &'static str {
        match self {
            CountIn::Off => "off",
            CountIn::OneBar => "one_bar",
            CountIn::TwoBars => "two_bars",
        }
    }
}

/// The click the transport plays: whether it sounds, and how loud.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickSettings {
    pub enabled: bool,
    /// Click level in dB; the click is summed after the master chain.
    pub level_db: f64,
    pub count_in: CountIn,
}

impl Default for ClickSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            level_db: -6.0,
            count_in: CountIn::Off,
        }
    }
}

impl ClickSettings {
    /// Linear gain of the click.
    pub fn gain(&self) -> f32 {
        10f64.powf(self.level_db.clamp(-60.0, 6.0) / 20.0) as f32
    }
}

/// One click in a chunk: the sample it starts on and whether it is the bar's
/// first beat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickEvent {
    /// Sample offset from the start of the block.
    pub offset: usize,
    pub accent: bool,
}

/// A count-in: how many frames it occupies and where its clicks fall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountInPlan {
    pub frames: u64,
    pub events: Vec<ClickEvent>,
}

impl CountInPlan {
    /// A count-in spanning `frames` with no click events.
    pub fn silent(frames: u64) -> Self {
        Self {
            frames,
            events: Vec::new(),
        }
    }
}

/// Explicit scheduling, preparation, or bounded mixing failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickError {
    EventCapacityExceeded,
    VoiceCapacityExceeded,
    NotPrepared,
    InvalidCapacity,
    AllocationFailed,
    InvalidEvents,
    InvalidRate,
    WindowOverflow,
    BarLimitExceeded,
    Clock(String),
}

impl std::fmt::Display for ClickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ClickError {}

/// Allocating convenience API. Streaming callers should retain storage and use
/// [`click_events_into`] instead.
pub fn click_events(
    clock: &ProjectClock,
    start_frame: u64,
    frames: usize,
    rate: u32,
) -> Result<Vec<ClickEvent>, String> {
    let mut events = Vec::new();
    visit_click_events(clock, start_frame, frames, rate, |event| {
        events.push(event);
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok(events)
}

/// Beat clicks inside `[start_frame, start_frame + frames)`, honouring the
/// project tempo and meter maps and accenting each bar's first beat.
///
/// Replace `events` without growing its caller-retained allocation. On failure
/// the output is empty (never a truncated schedule); its capacity is unchanged.
/// Valid clock/window processing performs no heap operations, including on a
/// capacity error. Clock validation failures retain the clock's error string.
pub fn click_events_into(
    clock: &ProjectClock,
    start_frame: u64,
    frames: usize,
    rate: u32,
    events: &mut Vec<ClickEvent>,
) -> Result<(), ClickError> {
    events.clear();
    let result = visit_click_events(clock, start_frame, frames, rate, |event| {
        if events.len() == events.capacity() {
            return Err(ClickError::EventCapacityExceeded);
        }
        events.push(event);
        Ok(())
    });
    if result.is_err() {
        events.clear();
    }
    result
}

fn visit_click_events(
    clock: &ProjectClock,
    start_frame: u64,
    frames: usize,
    rate: u32,
    mut emit: impl FnMut(ClickEvent) -> Result<(), ClickError>,
) -> Result<(), ClickError> {
    if rate == 0 || frames == 0 {
        return Ok(());
    }
    let end = start_frame
        .checked_add(frames as u64)
        .ok_or(ClickError::WindowOverflow)?;
    let position = clock
        .position_at_sample(start_frame, rate)
        .map_err(ClickError::Clock)?;
    let mut bar_start = position.bar_start_quarter;
    let mut bar = position.bar_number;
    let mut signature = position.signature;
    for _ in 0..MAX_BARS {
        let beat_quarters = 4.0 / f64::from(signature.denominator);
        let bar_quarters = f64::from(signature.numerator) * beat_quarters;
        let bar_frame = clock
            .sample_at_quarter(bar_start, rate)
            .map_err(ClickError::Clock)?;
        if bar_frame >= end {
            return Ok(());
        }
        for beat in 0..signature.numerator {
            let quarter = bar_start + f64::from(beat) * beat_quarters;
            let frame = clock
                .sample_at_quarter(quarter, rate)
                .map_err(ClickError::Clock)?;
            if frame >= start_frame && frame < end {
                emit(ClickEvent {
                    offset: (frame - start_frame) as usize,
                    accent: beat == 0,
                })?;
            }
        }
        bar_start += bar_quarters;
        bar = bar.checked_add(1).ok_or(ClickError::WindowOverflow)?;
        signature = clock.meter_at_bar(bar).map_err(ClickError::Clock)?;
    }
    if clock
        .sample_at_quarter(bar_start, rate)
        .map_err(ClickError::Clock)?
        >= end
    {
        Ok(())
    } else {
        Err(ClickError::BarLimitExceeded)
    }
}

/// The count-in that ends at `at_quarter`: whole bars of the meter in force
/// there, at the tempo in force there.
pub fn count_in_plan(
    clock: &ProjectClock,
    bars: u8,
    rate: u32,
    at_quarter: f64,
) -> Result<CountInPlan, String> {
    if bars == 0 || rate == 0 {
        return Ok(CountInPlan::silent(0));
    }
    let position = clock.bar_beat_at_quarter(at_quarter)?;
    let signature = clock.meter_at_bar(position.bar)?;
    let bpm = clock.tempo_at_quarter(at_quarter)?;
    let beat_seconds = 60.0 / bpm.max(1.0) * (4.0 / f64::from(signature.denominator));
    let numerator = u32::from(signature.numerator.max(1));
    let beats = u32::from(bars) * numerator;
    let mut events = Vec::with_capacity(beats as usize);
    for beat in 0..beats {
        let offset = (f64::from(beat) * beat_seconds * f64::from(rate)).round() as usize;
        events.push(ClickEvent {
            offset,
            accent: beat % numerator == 0,
        });
    }
    let frames = (f64::from(beats) * beat_seconds * f64::from(rate)).round() as u64;
    Ok(CountInPlan { frames, events })
}

/// The click's sample stream, kept across blocks so a click that starts near a
/// block boundary finishes in the next one.
#[derive(Default)]
pub struct ClickTrack {
    voices: Vec<Voice>,
    prepared_limit: Option<usize>,
}

struct Voice {
    accent: bool,
    elapsed: u32,
    length: u32,
}

impl ClickTrack {
    /// An empty click voice pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve voice storage on the control thread, before the first click.
    /// Existing voices are retained; shrinking below their count is rejected.
    pub fn prepare(&mut self, max_voices: usize) -> Result<(), ClickError> {
        if max_voices == 0 || max_voices < self.voices.len() {
            return Err(ClickError::InvalidCapacity);
        }
        self.voices
            .try_reserve(max_voices.saturating_sub(self.voices.len()))
            .map_err(|_| ClickError::AllocationFailed)?;
        self.prepared_limit = Some(max_voices);
        Ok(())
    }

    /// Start a click immediately without allocating. A full voice pool is
    /// reported explicitly; no active click is stolen.
    pub fn trigger_prepared(&mut self, accent: bool, rate: u32) -> Result<(), ClickError> {
        let limit = self.prepared_limit.ok_or(ClickError::NotPrepared)?;
        if rate == 0 {
            return Err(ClickError::InvalidRate);
        }
        if self.voices.len() >= limit {
            return Err(ClickError::VoiceCapacityExceeded);
        }
        self.voices.push(Voice {
            accent,
            elapsed: 0,
            length: (CLICK_SECONDS * f64::from(rate)).round() as u32,
        });
        Ok(())
    }

    /// Bounded, allocation-free mixing, including its first call. Events must
    /// be ordered and inside the block. Validation and capacity failures leave
    /// both output and active voices untouched. Odd trailing samples are untouched.
    pub fn mix_prepared(
        &mut self,
        out: &mut [f32],
        events: &[ClickEvent],
        rate: u32,
        gain: f32,
    ) -> Result<(), ClickError> {
        let limit = self.prepared_limit.ok_or(ClickError::NotPrepared)?;
        if rate == 0 {
            return Err(ClickError::InvalidRate);
        }
        let frames = out.len() / 2;
        if events.iter().any(|event| event.offset >= frames)
            || events
                .windows(2)
                .any(|pair| pair[0].offset > pair[1].offset)
        {
            return Err(ClickError::InvalidEvents);
        }
        if self.voices.len() > limit {
            return Err(ClickError::VoiceCapacityExceeded);
        }
        if gain <= 0.0 {
            return Ok(());
        }
        let length = ((CLICK_SECONDS * f64::from(rate)).round() as usize).max(1);
        // Count only voices still sounding at each trigger. Earlier events are
        // sorted, so partition_point bounds this check without scratch storage.
        for (index, event) in events.iter().enumerate() {
            let carried = self
                .voices
                .iter()
                .filter(|voice| {
                    event.offset < voice.length.saturating_sub(voice.elapsed).max(1) as usize
                })
                .count();
            let expired =
                events[..index].partition_point(|earlier| event.offset - earlier.offset >= length);
            if carried + index - expired + 1 > limit {
                return Err(ClickError::VoiceCapacityExceeded);
            }
        }
        self.mix(out, events, rate, gain);
        Ok(())
    }

    /// Release every active click voice.
    pub fn reset(&mut self) {
        self.voices.clear();
    }

    /// Sum the clicks of this block into an interleaved stereo buffer.
    pub fn mix(&mut self, out: &mut [f32], events: &[ClickEvent], rate: u32, gain: f32) {
        if gain <= 0.0 || rate == 0 {
            return;
        }
        let frames = out.len() / 2;
        let length = (CLICK_SECONDS * f64::from(rate)).round() as u32;
        let mut next = 0;
        for frame in 0..frames {
            while next < events.len() && events[next].offset == frame {
                self.voices.push(Voice {
                    accent: events[next].accent,
                    elapsed: 0,
                    length,
                });
                next += 1;
            }
            if self.voices.is_empty() {
                continue;
            }
            let mut sample = 0.0f32;
            for voice in &mut self.voices {
                sample += voice.next(rate);
            }
            self.voices.retain(|voice| voice.elapsed < voice.length);
            out[frame * 2] += sample * gain;
            out[frame * 2 + 1] += sample * gain;
        }
    }
}

impl Voice {
    fn next(&mut self, rate: u32) -> f32 {
        let t = f64::from(self.elapsed) / f64::from(rate);
        let hz = if self.accent { ACCENT_HZ } else { PLAIN_HZ };
        let value = (std::f64::consts::TAU * hz * t).sin() * (-t / CLICK_DECAY_SECONDS).exp() * 0.5;
        self.elapsed += 1;
        value as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::{ProjectProperties, TimeSignature};

    #[global_allocator]
    static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

    #[test]
    fn retained_schedule_never_grows_and_reports_incomplete_windows() {
        let clock = clock(120.0, 4, 4);
        let expected = click_events(&clock, 0, 96_000, 48_000).unwrap();
        let mut events = Vec::with_capacity(4);
        let capacity = events.capacity();
        let pointer = events.as_ptr();
        let mut result = Ok(());
        let heap = oc_test_support::operations(|| {
            result = click_events_into(&clock, 0, 96_000, 48_000, &mut events);
        });
        assert_eq!(heap, 0);
        assert_eq!(result, Ok(()));
        assert_eq!(events, expected);
        assert_eq!(events.capacity(), capacity);
        assert_eq!(events.as_ptr(), pointer);
        let heap = oc_test_support::operations(|| {
            result = click_events_into(&clock, 0, 192_000, 48_000, &mut events);
        });
        assert_eq!(heap, 0);
        assert_eq!(result, Err(ClickError::EventCapacityExceeded));
        assert!(events.is_empty());
        assert_eq!(events.capacity(), capacity);
        assert_eq!(events.as_ptr(), pointer);
        assert_eq!(
            click_events_into(&clock, u64::MAX, 2, 48_000, &mut events),
            Err(ClickError::WindowOverflow)
        );
        assert_eq!(click_events_into(&clock, 0, 0, 48_000, &mut events), Ok(()));
    }

    #[test]
    fn prepared_first_trigger_and_mix_need_no_heap_or_warmup() {
        let mut track = ClickTrack::new();
        track.prepare(2).unwrap();
        let mut output = [0.0; 128];
        let mut result = Ok(());
        let heap = oc_test_support::operations(|| {
            result = track
                .trigger_prepared(true, 48_000)
                .and_then(|()| track.mix_prepared(&mut output, &[], 48_000, 1.0));
        });
        assert_eq!(heap, 0);
        assert_eq!(result, Ok(()));
        assert!(output.iter().any(|sample| *sample != 0.0));
        track.reset();
        let heap = oc_test_support::operations(|| {
            result = track.mix_prepared(
                &mut output,
                &[ClickEvent {
                    offset: 0,
                    accent: false,
                }],
                48_000,
                1.0,
            );
        });
        assert_eq!(heap, 0);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn prepared_mix_matches_convenience_across_partial_blocks_and_tail() {
        let mut prepared = ClickTrack::new();
        prepared.prepare(2).unwrap();
        let mut convenience = ClickTrack::new();
        for rate in [1_000, 44_100, 48_000] {
            prepared.reset();
            convenience.reset();
            for (size, events) in [
                (
                    3,
                    vec![ClickEvent {
                        offset: 0,
                        accent: true,
                    }],
                ),
                (
                    65,
                    vec![ClickEvent {
                        offset: 1,
                        accent: false,
                    }],
                ),
                (13, vec![]),
                (4097, vec![]),
            ] {
                let mut a = vec![0.125; size];
                let mut b = a.clone();
                prepared.mix_prepared(&mut a, &events, rate, 0.8).unwrap();
                convenience.mix(&mut b, &events, rate, 0.8);
                assert_eq!(a, b, "rate {rate}, samples {size}");
                assert_eq!(a.last(), Some(&0.125), "odd trailing sample stays intact");
            }
            assert!(prepared.voices.is_empty());
        }
    }

    #[test]
    fn prepared_capacity_errors_do_not_modify_output_or_voices() {
        let mut track = ClickTrack::new();
        assert_eq!(track.prepare(0), Err(ClickError::InvalidCapacity));
        assert_eq!(
            track.trigger_prepared(true, 48_000),
            Err(ClickError::NotPrepared)
        );
        track.prepare(1).unwrap();
        track.trigger_prepared(true, 48_000).unwrap();
        let mut out = [0.25; 128];
        let event = [ClickEvent {
            offset: 0,
            accent: false,
        }];
        let mut result = Ok(());
        let heap = oc_test_support::operations(|| {
            result = track.mix_prepared(&mut out, &event, 48_000, 1.0);
        });
        assert_eq!(heap, 0);
        assert_eq!(result, Err(ClickError::VoiceCapacityExceeded));
        assert_eq!(out, [0.25; 128]);
        assert_eq!(track.voices.len(), 1);
        assert_eq!(track.voices[0].elapsed, 0);
        assert_eq!(
            track.trigger_prepared(false, 48_000),
            Err(ClickError::VoiceCapacityExceeded)
        );
        // One voice is enough when the next click starts after the old tail.
        let mut long = [0.0; 3000];
        track
            .mix_prepared(
                &mut long,
                &[ClickEvent {
                    offset: 1440,
                    accent: false,
                }],
                48_000,
                1.0,
            )
            .unwrap();
        assert_eq!(track.voices.len(), 1);
        assert!(!track.voices[0].accent);
    }

    #[test]
    fn prepared_rejects_invalid_events_without_consuming_carried_tail() {
        let mut track = ClickTrack::new();
        track.prepare(2).unwrap();
        track.trigger_prepared(true, 48_000).unwrap();
        let mut out = [0.25; 8];
        for events in [
            vec![ClickEvent {
                offset: 4,
                accent: false,
            }],
            vec![
                ClickEvent {
                    offset: 2,
                    accent: false,
                },
                ClickEvent {
                    offset: 1,
                    accent: false,
                },
            ],
        ] {
            assert_eq!(
                track.mix_prepared(&mut out, &events, 48_000, 1.0),
                Err(ClickError::InvalidEvents)
            );
            assert_eq!(out, [0.25; 8]);
            assert_eq!(track.voices[0].elapsed, 0);
        }
        assert_eq!(
            track.mix_prepared(&mut out, &[], 0, 1.0),
            Err(ClickError::InvalidRate)
        );
        track.mix_prepared(&mut [], &[], 48_000, 1.0).unwrap();
        assert_eq!(track.voices[0].elapsed, 0);
    }

    fn clock(bpm: f64, numerator: u32, denominator: u32) -> ProjectClock {
        let properties = ProjectProperties {
            tempo: bpm,
            time_signature: TimeSignature {
                numerator: numerator as u8,
                denominator: denominator as u8,
            },
            ..Default::default()
        };
        ProjectClock::new(&properties).expect("clock")
    }

    /// Frames holding a non-zero click sample.
    fn hit_frames(out: &[f32]) -> Vec<usize> {
        out.chunks(2)
            .enumerate()
            .filter(|(_, frame)| frame.iter().any(|sample| *sample != 0.0))
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn clicks_land_on_the_beat_at_120_bpm() {
        let clock = clock(120.0, 4, 4);
        // 120 BPM: one beat is 0.5 s, 24 000 frames at 48 kHz.
        let events = click_events(&clock, 0, 96_000, 48_000).expect("events");
        assert_eq!(
            events.iter().map(|event| event.offset).collect::<Vec<_>>(),
            vec![0, 24_000, 48_000, 72_000]
        );
        assert_eq!(
            events.iter().filter(|event| event.accent).count(),
            1,
            "one bar line in one bar"
        );
        assert!(events[0].accent);
        assert!(!events[1].accent && !events[2].accent && !events[3].accent);
    }

    #[test]
    fn the_first_beat_of_every_bar_is_accented() {
        let clock = clock(120.0, 4, 4);
        let events = click_events(&clock, 0, 192_000, 48_000).expect("events");
        let accents: Vec<usize> = events
            .iter()
            .filter(|event| event.accent)
            .map(|event| event.offset)
            .collect();
        assert_eq!(accents, vec![0, 96_000]);
    }

    #[test]
    fn a_meter_change_moves_the_bar_lines() {
        // 4/4 from bar 1, 3/4 from bar 2: the second bar is three beats long.
        let properties = ProjectProperties {
            tempo: 120.0,
            time_signature: TimeSignature {
                numerator: 4,
                denominator: 4,
            },
            time_sig_map: vec![oc_dsl::ast::TimeSigPoint {
                position: oc_dsl::ast::BarBeat::new(2, 1, 0),
                numerator: 3,
                denominator: 4,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&properties).expect("clock");
        let events = click_events(&clock, 0, 192_000, 48_000).expect("events");
        let offsets: Vec<usize> = events.iter().map(|event| event.offset).collect();
        // Bar 1: 0, 24k, 48k, 72k. Bar 2 starts at 96k and holds three beats,
        // so bar 3 starts at 168k.
        assert_eq!(
            offsets,
            vec![0, 24_000, 48_000, 72_000, 96_000, 120_000, 144_000, 168_000]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.accent)
                .map(|event| event.offset)
                .collect::<Vec<_>>(),
            vec![0, 96_000, 168_000]
        );
    }

    #[test]
    fn a_tempo_change_moves_later_beats() {
        let properties = ProjectProperties {
            tempo: 120.0,
            tempo_map: vec![oc_dsl::ast::TempoPoint {
                position: oc_dsl::ast::BarBeat::new(2, 1, 0),
                offset_quarters: 0.0,
                bpm: 60.0,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&properties).expect("clock");
        let events = click_events(&clock, 0, 200_000, 48_000).expect("events");
        let offsets: Vec<usize> = events.iter().map(|event| event.offset).collect();
        // Bar 1 at 120 BPM (0.5 s a beat, 24k frames), bar 2 at 60 BPM
        // (1 s a beat, 48k frames) from frame 96k.
        assert_eq!(
            offsets,
            vec![0, 24_000, 48_000, 72_000, 96_000, 144_000, 192_000]
        );
    }

    #[test]
    fn a_block_that_starts_mid_bar_keeps_the_bar_accent() {
        let clock = clock(120.0, 4, 4);
        // Start halfway through beat 2: the next events are beats 3 and 4, then
        // the bar line at frame 96k.
        let events = click_events(&clock, 36_000, 64_000, 48_000).expect("events");
        assert_eq!(
            events.iter().map(|event| event.offset).collect::<Vec<_>>(),
            vec![12_000, 36_000, 60_000],
            "beats 3 and 4, then the bar line"
        );
        assert!(!events[0].accent, "beat 3 is not a bar line");
        assert!(!events[1].accent, "beat 4 is not a bar line");
        assert!(events[2].accent, "the next bar line is");
    }

    #[test]
    fn the_count_in_is_one_or_two_bars_long() {
        let clock = clock(120.0, 4, 4);
        let one = count_in_plan(&clock, 1, 48_000, 0.0).expect("one bar");
        assert_eq!(one.frames, 96_000, "4 beats of 0.5 s at 48 kHz");
        assert_eq!(one.events.len(), 4);
        assert!(one.events[0].accent);
        let two = count_in_plan(&clock, 2, 48_000, 0.0).expect("two bars");
        assert_eq!(two.frames, 192_000);
        assert_eq!(two.events.len(), 8);
        assert_eq!(
            two.events
                .iter()
                .map(|event| event.offset)
                .collect::<Vec<_>>(),
            vec![0, 24_000, 48_000, 72_000, 96_000, 120_000, 144_000, 168_000]
        );
        assert_eq!(
            two.events
                .iter()
                .filter(|event| event.accent)
                .map(|event| event.offset)
                .collect::<Vec<_>>(),
            vec![0, 96_000]
        );
        assert_eq!(count_in_plan(&clock, 0, 48_000, 0.0).unwrap().frames, 0);
    }

    #[test]
    fn the_count_in_uses_the_meter_and_tempo_at_the_record_position() {
        let properties = ProjectProperties {
            tempo: 120.0,
            time_signature: TimeSignature {
                numerator: 4,
                denominator: 4,
            },
            time_sig_map: vec![oc_dsl::ast::TimeSigPoint {
                position: oc_dsl::ast::BarBeat::new(3, 1, 0),
                numerator: 3,
                denominator: 4,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&properties).expect("clock");
        // Record at the top of bar 3: three beats, not four.
        let quarter = clock
            .quarter_notes_at(&oc_dsl::ast::BarBeat::new(3, 1, 0))
            .unwrap();
        let plan = count_in_plan(&clock, 1, 48_000, quarter).unwrap();
        assert_eq!(plan.frames, 72_000, "3 beats of 0.5 s at 48 kHz");
        assert_eq!(plan.events.len(), 3);
        assert!(plan.events[0].accent);
    }

    #[test]
    fn a_click_starts_exactly_on_its_event_frame() {
        let mut track = ClickTrack::new();
        let mut out = vec![0.0f32; 64 * 2];
        track.mix(
            &mut out,
            &[
                ClickEvent {
                    offset: 10,
                    accent: true,
                },
                ClickEvent {
                    offset: 40,
                    accent: false,
                },
            ],
            48_000,
            1.0,
        );
        let hits = hit_frames(&out);
        let first = *hits.first().expect("a click");
        assert!(
            (10..14).contains(&first),
            "the accented click starts on frame 10, got {first}"
        );
        assert!(
            hits.iter().all(|frame| *frame >= 10),
            "nothing sounds before the first event: {hits:?}"
        );
        assert!(
            hits.iter().all(|frame| *frame < 10 + 1_440),
            "a 30 ms click fits in 1440 frames at 48 kHz: {hits:?}"
        );
    }

    #[test]
    fn the_accent_is_a_different_pitch_and_decays() {
        let mut accent = ClickTrack::new();
        let mut plain = ClickTrack::new();
        let mut a = vec![0.0f32; 32 * 2];
        let mut p = vec![0.0f32; 32 * 2];
        accent.mix(
            &mut a,
            &[ClickEvent {
                offset: 0,
                accent: true,
            }],
            48_000,
            1.0,
        );
        plain.mix(
            &mut p,
            &[ClickEvent {
                offset: 0,
                accent: false,
            }],
            48_000,
            1.0,
        );
        assert_ne!(a, p, "the accent sounds different from a plain beat");
        let head: f32 = a[2..20].iter().map(|sample| sample.abs()).sum();
        let tail: f32 = a[40..58].iter().map(|sample| sample.abs()).sum();
        assert!(tail < head, "the click decays: {head} vs {tail}");
    }

    #[test]
    fn the_click_level_scales_the_output() {
        let mut loud = ClickTrack::new();
        let mut quiet = ClickTrack::new();
        let mut a = vec![0.0f32; 16 * 2];
        let mut b = vec![0.0f32; 16 * 2];
        let event = ClickEvent {
            offset: 0,
            accent: true,
        };
        loud.mix(&mut a, &[event], 48_000, 1.0);
        quiet.mix(&mut b, &[event], 48_000, 0.5);
        let peak_a = a.iter().fold(0.0f32, |acc, sample| acc.max(sample.abs()));
        let peak_b = b.iter().fold(0.0f32, |acc, sample| acc.max(sample.abs()));
        assert!((peak_a - peak_b * 2.0).abs() < 1e-6, "{peak_a} vs {peak_b}");
    }

    #[test]
    fn settings_default_to_a_silent_click() {
        let settings = ClickSettings::default();
        assert!(!settings.enabled, "the click never starts on by itself");
        assert_eq!(settings.count_in, CountIn::Off);
        assert!(settings.gain() > 0.0);
        assert_eq!(CountIn::TwoBars.bars(), 2);
        assert_eq!(CountIn::TwoBars.key(), "two_bars");
    }
}
