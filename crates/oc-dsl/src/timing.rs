//! Shared musical-time contract for project import, interpretation and transport.
//!
//! Tempo is quarter notes/minute. A BarBeat beat is the meter's denominator
//! note; its subdivision divides that beat into four. Note durations/start_beat
//! remain quarter-note units. Meter changes must start a bar. Tempo changes are
//! steps, not ramps. Construction rejects ambiguous maps rather than repairing
//! or silently sorting authored state.

use crate::ast::{BarBeat, ProjectProperties, TimeSignature};

#[derive(Debug, Clone)]
struct MeterSegment {
    bar: u32,
    quarter: f64,
    signature: TimeSignature,
}

#[derive(Debug, Clone)]
struct TempoSegment {
    quarter: f64,
    seconds: f64,
    bpm: f64,
}

/// Checked, immutable piecewise clock. Building this object does not imply that
/// a particular renderer or plugin host supports all mapped-time features.
#[derive(Debug, Clone)]
pub struct ProjectClock {
    meters: Vec<MeterSegment>,
    tempos: Vec<TempoSegment>,
}

/// A block-start snapshot on the same sample-quantized timeline as MIDI events.
#[derive(Debug, Clone, Copy)]
pub struct ClockPosition {
    pub quarter: f64,
    pub bpm: f64,
    pub signature: TimeSignature,
    pub bar_start_quarter: f64,
    pub bar_number: u32,
}

fn valid_meter(signature: TimeSignature) -> bool {
    (1..=32).contains(&signature.numerator)
        && matches!(signature.denominator, 1 | 2 | 4 | 8 | 16 | 32 | 64)
}

fn valid_tempo(bpm: f64) -> bool {
    bpm.is_finite() && (1.0..=400.0).contains(&bpm)
}

fn nonnegative(value: f64, name: &str) -> Result<(), String> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(format!("{name} must be finite and nonnegative"))
    }
}

impl ProjectClock {
    /// Convert an absolute sample index to its clock position.
    pub fn position_at_sample(&self, sample: u64, rate: u32) -> Result<ClockPosition, String> {
        if rate == 0 {
            return Err("sample rate must be positive".into());
        }
        let index = self
            .tempos
            .partition_point(|segment| (segment.seconds * f64::from(rate)) as u64 <= sample)
            - 1;
        let tempo = &self.tempos[index];
        // Anchor each change to its floored frame, as sample_at_quarter does.
        let origin = (tempo.seconds * f64::from(rate)) as u64;
        let quarter = tempo.quarter + (sample - origin) as f64 / f64::from(rate) * tempo.bpm / 60.0;
        let meter_index = self.meters.partition_point(|segment| {
            self.sample_at_quarter(segment.quarter, rate)
                .is_ok_and(|frame| frame <= sample)
        }) - 1;
        let meter = &self.meters[meter_index];
        let quarter = quarter.max(meter.quarter);
        let length =
            f64::from(meter.signature.numerator) * 4.0 / f64::from(meter.signature.denominator);
        let mut bars = ((quarter - meter.quarter) / length).floor();
        let next_bar_quarter = meter.quarter + (bars + 1.0) * length;
        if self
            .sample_at_quarter(next_bar_quarter, rate)
            .is_ok_and(|frame| frame <= sample)
        {
            bars += 1.0;
        }
        if bars > f64::from(u32::MAX - meter.bar) {
            return Err("bar number overflow".into());
        }
        let bar_number = u64::from(meter.bar) + bars as u64;
        Ok(ClockPosition {
            quarter: quarter.max(meter.quarter + bars * length),
            bpm: tempo.bpm,
            signature: meter.signature,
            bar_start_quarter: meter.quarter + bars * length,
            bar_number: u32::try_from(bar_number).map_err(|_| "bar number overflow")?,
        })
    }

    /// The next tempo or meter boundary sample, if any.
    pub fn next_change_sample(&self, sample: u64, rate: u32) -> Result<Option<u64>, String> {
        if rate == 0 {
            return Err("sample rate must be positive".into());
        }
        let tempo_index = self
            .tempos
            .partition_point(|segment| (segment.seconds * f64::from(rate)) as u64 <= sample);
        let tempo = self
            .tempos
            .get(tempo_index)
            .map(|segment| self.sample_at_quarter(segment.quarter, rate))
            .transpose()?;
        let meter_index = self.meters.partition_point(|segment| {
            self.sample_at_quarter(segment.quarter, rate)
                .is_ok_and(|frame| frame <= sample)
        });
        let meter = self
            .meters
            .get(meter_index)
            .map(|segment| self.sample_at_quarter(segment.quarter, rate))
            .transpose()?;
        Ok(tempo.into_iter().chain(meter).min())
    }
    /// Build the clock from project tempo and meter maps.
    pub fn new(properties: &ProjectProperties) -> Result<Self, String> {
        if !valid_meter(properties.time_signature) || !valid_tempo(properties.tempo) {
            return Err("invalid base meter or tempo".into());
        }
        let mut clock = Self {
            meters: vec![MeterSegment {
                bar: 1,
                quarter: 0.0,
                signature: properties.time_signature,
            }],
            tempos: vec![TempoSegment {
                quarter: 0.0,
                seconds: 0.0,
                bpm: properties.tempo,
            }],
        };
        let mut previous_bar = None;
        for point in &properties.time_sig_map {
            let signature = TimeSignature {
                numerator: point.numerator,
                denominator: point.denominator,
            };
            if point.position.bar == 0
                || point.position.beat != 1
                || point.position.subdivision != 0
                || !valid_meter(signature)
                || previous_bar.is_some_and(|bar| point.position.bar <= bar)
            {
                return Err(
                    "meter map must have unique ascending bar-start positions and valid meters"
                        .into(),
                );
            }
            previous_bar = Some(point.position.bar);
            let quarter = clock.quarter_notes_at(&point.position)?;
            let segment = MeterSegment {
                bar: point.position.bar,
                quarter,
                signature,
            };
            if point.position.bar == 1 {
                clock.meters[0] = segment;
            } else {
                clock.meters.push(segment);
            }
        }
        let mut previous_quarter = None;
        for point in &properties.tempo_map {
            let anchor = clock.quarter_notes_at(&point.position)?;
            let denominator = clock
                .meters
                .iter()
                .rev()
                .find(|m| m.bar <= point.position.bar)
                .unwrap()
                .signature
                .denominator;
            if !point.offset_quarters.is_finite()
                || point.offset_quarters < 0.0
                || point.offset_quarters >= 1.0 / f64::from(denominator)
            {
                return Err("tempo residual must fit within its anchor subdivision".into());
            }
            let quarter = anchor + point.offset_quarters;
            if !valid_tempo(point.bpm)
                || previous_quarter.is_some_and(|previous| quarter <= previous)
            {
                return Err(
                    "tempo map must have unique ascending positions and valid tempos".into(),
                );
            }
            previous_quarter = Some(quarter);
            let seconds = clock.seconds_at_quarter(quarter)?;
            let segment = TempoSegment {
                quarter,
                seconds,
                bpm: point.bpm,
            };
            if quarter == 0.0 {
                clock.tempos[0] = segment;
            } else {
                clock.tempos.push(segment);
            }
        }
        Ok(clock)
    }

    /// Time signature in effect at a one-indexed bar.
    pub fn meter_at_bar(&self, bar: u32) -> Result<TimeSignature, String> {
        if bar == 0 {
            return Err("bar is one-indexed".into());
        }
        let index = self.meters.partition_point(|segment| segment.bar <= bar) - 1;
        Ok(self.meters[index].signature)
    }

    /// Quarter-note position of a notated bar/beat/subdivision.
    pub fn quarter_notes_at(&self, position: &BarBeat) -> Result<f64, String> {
        let signature = self.meter_at_bar(position.bar)?;
        if position.beat == 0
            || position.beat > u32::from(signature.numerator)
            || position.subdivision >= 4
        {
            return Err("position must be inside its notated bar (subdivision 0..3)".into());
        }
        let index = self
            .meters
            .partition_point(|segment| segment.bar <= position.bar)
            - 1;
        let segment = &self.meters[index];
        let beat_quarters = 4.0 / f64::from(signature.denominator);
        Ok(segment.quarter
            + f64::from(position.bar - segment.bar)
                * f64::from(signature.numerator)
                * beat_quarters
            + (f64::from(position.beat - 1) + f64::from(position.subdivision) * 0.25)
                * beat_quarters)
    }

    /// Gain automation's explicit PPQ counts quarter-note ticks, unlike the
    /// ordinary BarBeat subdivision which divides a notated beat into four.
    pub fn quarter_notes_at_ppq(&self, position: &BarBeat, ppq: u32) -> Result<f64, String> {
        let meter = self.meter_at_bar(position.bar)?;
        if ppq == 0
            || ppq > 96000
            || u64::from(position.subdivision) * u64::from(meter.denominator) >= u64::from(ppq) * 4
        {
            return Err("PPQ subdivision must lie inside its notated beat".into());
        }
        let base = self.quarter_notes_at(&BarBeat::new(position.bar, position.beat, 0))?;
        Ok(base + f64::from(position.subdivision) / f64::from(ppq))
    }

    /// Exact inverse for editable OCP ranges. Never silently round a moved
    /// clip endpoint to a coarser grid after crossing a meter change.
    pub fn bar_beat_at_quarter(&self, quarter: f64) -> Result<BarBeat, String> {
        nonnegative(quarter, "quarter-note position")?;
        let index = self.meters.partition_point(|m| m.quarter <= quarter) - 1;
        let m = &self.meters[index];
        let ticks = (quarter - m.quarter) * f64::from(m.signature.denominator);
        if (ticks - ticks.round()).abs() > 1e-7 || ticks >= u64::MAX as f64 {
            return Err("position is not representable on the notated subdivision grid".into());
        }
        let ticks = ticks.round() as u64;
        let per_bar = u64::from(m.signature.numerator) * 4;
        let bar = u32::try_from(u64::from(m.bar) + ticks / per_bar).map_err(|_| "bar overflow")?;
        Ok(BarBeat::new(
            bar,
            (ticks % per_bar / 4) as u32 + 1,
            (ticks % 4) as u32,
        ))
    }

    /// Lower notated-grid anchor, for lossless anchor-plus-residual positions.
    pub fn bar_beat_floor_at_quarter(&self, quarter: f64) -> Result<BarBeat, String> {
        nonnegative(quarter, "quarter-note position")?;
        let m = &self.meters[self.meters.partition_point(|m| m.quarter <= quarter) - 1];
        let ticks = ((quarter - m.quarter) * f64::from(m.signature.denominator)).floor();
        self.bar_beat_at_quarter(m.quarter + ticks / f64::from(m.signature.denominator))
    }

    /// Seconds at a quarter-note position.
    pub fn seconds_at_quarter(&self, quarter: f64) -> Result<f64, String> {
        nonnegative(quarter, "quarter-note position")?;
        let index = self
            .tempos
            .partition_point(|segment| segment.quarter <= quarter)
            - 1;
        let segment = &self.tempos[index];
        let seconds = segment.seconds + (quarter - segment.quarter) * 60.0 / segment.bpm;
        nonnegative(seconds, "seconds")?;
        Ok(seconds)
    }

    /// Inverse integration, for transport and seeking (not seconds * current BPM).
    pub fn quarter_at_seconds(&self, seconds: f64) -> Result<f64, String> {
        nonnegative(seconds, "seconds")?;
        let index = self
            .tempos
            .partition_point(|segment| segment.seconds <= seconds)
            - 1;
        let segment = &self.tempos[index];
        let quarter = segment.quarter + (seconds - segment.seconds) * segment.bpm / 60.0;
        nonnegative(quarter, "quarter-note position")?;
        Ok(quarter)
    }

    /// Tempo in effect at a quarter-note position.
    pub fn tempo_at_quarter(&self, quarter: f64) -> Result<f64, String> {
        nonnegative(quarter, "quarter-note position")?;
        let index = self
            .tempos
            .partition_point(|segment| segment.quarter <= quarter)
            - 1;
        Ok(self.tempos[index].bpm)
    }

    /// True when a tempo change falls strictly inside the span, so a caller
    /// cannot treat quarter position as linear in elapsed samples.
    pub fn has_tempo_change_between(&self, start: f64, end: f64) -> bool {
        self.tempos
            .iter()
            .any(|segment| segment.quarter > start && segment.quarter < end)
    }

    /// Actual clip boundaries in project seconds, including fine placement.
    /// The media source offset is deliberately unrelated to these boundaries.
    pub fn audio_clip_range_seconds(
        &self,
        clip: &crate::ast::AudioClip,
    ) -> Result<(f64, f64), String> {
        let start = self.seconds_at_quarter(self.quarter_notes_at(&clip.time_range.start)?)?
            + clip.placement.start_offset_seconds;
        let end = self.seconds_at_quarter(self.quarter_notes_at(&clip.time_range.end)?)?
            + clip.placement.end_offset_seconds;
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return Err("audio clip placement requires finite boundaries and a non-negative start; end must be after start".into());
        }
        Ok((start, end))
    }

    /// Quantize actual endpoints once. Unchanged endpoints retain the legacy
    /// musical clock's floor; explicitly offset endpoints round the combined
    /// anchor plus offset to the nearest sample, never two separate terms.
    pub fn audio_clip_range_frames(
        &self,
        clip: &crate::ast::AudioClip,
        sample_rate: u32,
    ) -> Result<(u64, u64), String> {
        let (start, end) = self.audio_clip_range_seconds(clip)?;
        if sample_rate == 0 {
            return Err("sample rate must be positive".into());
        }
        let boundary = |seconds: f64, offset: f64| -> Result<u64, String> {
            let sample = seconds * f64::from(sample_rate);
            let quantized = if offset == 0.0 {
                sample.floor()
            } else {
                sample.round()
            };
            if !quantized.is_finite() || quantized >= u64::MAX as f64 {
                return Err("sample position overflow".into());
            }
            Ok(quantized as u64)
        };
        let start = boundary(start, clip.placement.start_offset_seconds)?;
        let end = boundary(end, clip.placement.end_offset_seconds)?;
        if end <= start {
            return Err("audio clip placement is shorter than one sample".into());
        }
        Ok((start, end))
    }

    /// Floor once at the absolute boundary. Millisecond/profile offsets should
    /// be applied afterward in samples; never warp an already interpreted event.
    pub fn sample_at_quarter(&self, quarter: f64, sample_rate: u32) -> Result<u64, String> {
        if sample_rate == 0 {
            return Err("sample rate must be positive".into());
        }
        let sample = self.seconds_at_quarter(quarter)? * f64::from(sample_rate);
        if !sample.is_finite() || sample >= u64::MAX as f64 {
            return Err("sample position overflow".into());
        }
        Ok(sample as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{TempoPoint, TimeSigPoint};

    #[test]
    fn fine_tempo_residual_roundtrips_and_drives_sample_boundaries() {
        let p = crate::parse("@project\n  tempo: 120\n  tempo_map:\n    1:1 + 0.125q = 60 bpm\n")
            .unwrap();
        let reopened = crate::parse(&crate::serialize(&p)).unwrap();
        assert_eq!(p, reopened);
        crate::validate(&reopened).unwrap();
        let clock = ProjectClock::new(&reopened.properties).unwrap();
        assert_eq!(clock.sample_at_quarter(0.125, 48000).unwrap(), 3000);
        assert_eq!(clock.sample_at_quarter(1.0, 48000).unwrap(), 45000);
        assert_eq!(clock.position_at_sample(2999, 48000).unwrap().bpm, 120.0);
        assert_eq!(clock.position_at_sample(3000, 48000).unwrap().bpm, 60.0);
        let mut invalid = p.clone();
        invalid.properties.tempo_map[0].offset_quarters = 0.25;
        assert!(crate::validate(&invalid).is_err());
        invalid.properties.tempo_map[0].offset_quarters = f64::NAN;
        assert!(ProjectClock::new(&invalid.properties).is_err());
        // Legacy serialized JSON has no residual and remains compatible.
        let old: TempoPoint =
            serde_json::from_str(r#"{"position":{"bar":1,"beat":1,"subdivision":0},"bpm":120}"#)
                .unwrap();
        assert_eq!(old.offset_quarters, 0.0);
    }

    #[test]
    fn sample_snapshots_floor_bar_boundaries_consistently_with_notes() {
        let clock = ProjectClock::new(&ProjectProperties {
            tempo: 137.0,
            ..Default::default()
        })
        .unwrap();
        for bar in 2..100 {
            let quarter = f64::from(bar - 1) * 4.0;
            let sample = clock.sample_at_quarter(quarter, 44100).unwrap();
            let before = clock.position_at_sample(sample - 1, 44100).unwrap();
            let at = clock.position_at_sample(sample, 44100).unwrap();
            assert_eq!(before.bar_number, bar - 1);
            assert_eq!(at.bar_number, bar);
            assert_eq!(at.bar_start_quarter, quarter);
            assert!(at.quarter >= at.bar_start_quarter);
        }
        assert!(clock.position_at_sample(u64::MAX, 1).is_err());
    }

    #[test]
    fn constant_four_four_preserves_existing_sample_arithmetic() {
        for bpm in [60.0, 90.1, 120.0, 137.77, 400.0] {
            let clock = ProjectClock::new(&ProjectProperties {
                tempo: bpm,
                ..Default::default()
            })
            .unwrap();
            for quarter in [0.0, 0.0001, 1.0, 3.75, 192.123456] {
                for rate in [44100, 48000, 96000] {
                    assert_eq!(
                        clock.sample_at_quarter(quarter, rate).unwrap(),
                        (quarter * 60.0 / bpm * f64::from(rate)) as u64
                    );
                }
            }
            assert_eq!(
                clock.quarter_notes_at(&BarBeat::new(3, 2, 3)).unwrap(),
                9.75
            );
        }
    }

    #[test]
    fn integrates_note_crossing_tempo_change_and_seeks_back() {
        let p = ProjectProperties {
            tempo: 120.0,
            tempo_map: vec![TempoPoint {
                position: BarBeat::new(1, 3, 0),
                offset_quarters: 0.0,
                bpm: 60.0,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&p).unwrap();
        assert_eq!(clock.sample_at_quarter(1.0, 48000).unwrap(), 24000);
        assert_eq!(clock.sample_at_quarter(3.0, 48000).unwrap(), 96000);
        assert_eq!(clock.tempo_at_quarter(2.0).unwrap(), 60.0);
        for quarter in [0.0, 1.0, 1.999, 2.0, 2.001, 3.0, 100.0] {
            let seconds = clock.seconds_at_quarter(quarter).unwrap();
            assert!((clock.quarter_at_seconds(seconds).unwrap() - quarter).abs() < 1e-10);
        }
    }

    #[test]
    fn meter_denominator_and_changes_share_quarter_note_axis() {
        let p = ProjectProperties {
            time_signature: TimeSignature {
                numerator: 7,
                denominator: 8,
            },
            time_sig_map: vec![TimeSigPoint {
                position: BarBeat::new(3, 1, 0),
                numerator: 3,
                denominator: 4,
            }],
            tempo_map: vec![TempoPoint {
                position: BarBeat::new(3, 2, 0),
                offset_quarters: 0.0,
                bpm: 60.0,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&p).unwrap();
        assert_eq!(clock.quarter_notes_at(&BarBeat::new(2, 1, 0)).unwrap(), 3.5);
        assert_eq!(
            clock.quarter_notes_at(&BarBeat::new(2, 7, 2)).unwrap(),
            6.75
        );
        assert_eq!(clock.quarter_notes_at(&BarBeat::new(3, 2, 0)).unwrap(), 8.0);
        assert_eq!(
            clock.quarter_notes_at(&BarBeat::new(4, 1, 0)).unwrap(),
            10.0
        );
        assert_eq!(clock.seconds_at_quarter(10.0).unwrap(), 6.0);
    }

    #[test]
    fn initial_points_override_defaults_without_elapsed_time() {
        let p = ProjectProperties {
            time_sig_map: vec![TimeSigPoint {
                position: BarBeat::new(1, 1, 0),
                numerator: 3,
                denominator: 8,
            }],
            tempo_map: vec![TempoPoint {
                position: BarBeat::new(1, 1, 0),
                offset_quarters: 0.0,
                bpm: 60.0,
            }],
            ..Default::default()
        };
        let clock = ProjectClock::new(&p).unwrap();
        assert_eq!(clock.quarter_notes_at(&BarBeat::new(2, 1, 0)).unwrap(), 1.5);
        assert_eq!(clock.seconds_at_quarter(1.5).unwrap(), 1.5);
    }

    #[test]
    fn rejects_ambiguous_or_invalid_maps_and_queries() {
        let mut p = ProjectProperties {
            time_sig_map: vec![TimeSigPoint {
                position: BarBeat::new(2, 2, 0),
                numerator: 3,
                denominator: 4,
            }],
            ..Default::default()
        };
        assert!(ProjectClock::new(&p).is_err());
        p.time_sig_map.clear();
        let point = TempoPoint {
            position: BarBeat::new(2, 1, 0),
            offset_quarters: 0.0,
            bpm: 90.0,
        };
        p.tempo_map = vec![point, point];
        assert!(ProjectClock::new(&p).is_err());
        p.tempo_map = vec![
            point,
            TempoPoint {
                position: BarBeat::new(1, 2, 0),
                offset_quarters: 0.0,
                bpm: 90.0,
            },
        ];
        assert!(ProjectClock::new(&p).is_err());
        p.tempo_map.clear();
        let clock = ProjectClock::new(&p).unwrap();
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(clock.seconds_at_quarter(bad).is_err());
            assert!(clock.quarter_at_seconds(bad).is_err());
            assert!(clock.tempo_at_quarter(bad).is_err());
        }
        assert!(clock.sample_at_quarter(1.0, 0).is_err());
        assert!(clock.sample_at_quarter(f64::MAX, 48000).is_err());
        for position in [
            BarBeat::new(0, 1, 0),
            BarBeat::new(1, 0, 0),
            BarBeat::new(1, 5, 0),
            BarBeat::new(1, 1, 4),
        ] {
            assert!(clock.quarter_notes_at(&position).is_err());
        }
    }
}
#[test]
fn inverse_edit_grid_preserves_positions_or_rejects_loss() {
    let project =
        crate::parse("@project\n  time_signature: 7/8\n  time_sig_map:\n    3:1 = 3/4\n").unwrap();
    let clock = ProjectClock::new(&project.properties).unwrap();
    for quarter in [0.0, 3.5, 3.625, 7.0, 7.5, 10.0] {
        assert_eq!(
            clock
                .quarter_notes_at(&clock.bar_beat_at_quarter(quarter).unwrap())
                .unwrap(),
            quarter
        );
    }
    assert!(clock.bar_beat_at_quarter(7.125).is_err());
    assert!(clock.bar_beat_at_quarter(-1.0).is_err());
}

#[cfg(test)]
mod audio_clip_placement_tests {
    use super::*;

    fn project() -> crate::ast::Project {
        crate::parse("@project\n  tempo: 137\n@audio_clip voice [1:2 - 2:1] | track: vocal\n  file: \"voice.wav\"\n@track vocal | instrument: audio/clip\n").unwrap()
    }

    #[test]
    fn audio_clip_placement_roundtrips_signed_seconds_and_json_defaults() {
        let mut project = project();
        project.audio_clip_mut(0).unwrap().placement = crate::ast::AudioClipPlacement {
            start_offset_seconds: -1.0 / 44100.0,
            end_offset_seconds: 1.0 / 48000.0,
        };
        let source = crate::serialize(&project);
        let decoded = crate::parse(&source).unwrap();
        assert_eq!(project.audio_clip(0), decoded.audio_clip(0));
        let plain = self::project();
        let json = serde_json::to_value(plain.audio_clip(0).unwrap()).unwrap();
        assert!(json.get("placement").is_none());
        let restored: crate::ast::AudioClip = serde_json::from_value(json).unwrap();
        assert!(restored.placement.is_zero());
        assert!(!crate::serialize(&plain).contains("placement_"));
    }

    #[test]
    fn audio_clip_placement_quantizes_combined_boundaries_and_retains_legacy_floor() {
        let mut project = project();
        let clock = ProjectClock::new(&project.properties).unwrap();
        for rate in [44100, 48000] {
            let clip = project.audio_clip_mut(0).unwrap();
            clip.placement = Default::default();
            let legacy = clock.sample_at_quarter(1.0, rate).unwrap();
            assert_eq!(clock.audio_clip_range_frames(clip, rate).unwrap().0, legacy);
            let anchor = clock.seconds_at_quarter(1.0).unwrap();
            for samples in [-1.0, 1.0] {
                clip.placement.start_offset_seconds = samples / f64::from(rate);
                let frames = clock.audio_clip_range_frames(clip, rate).unwrap();
                assert_eq!(
                    frames.0,
                    (anchor * f64::from(rate) + samples).round() as u64
                );
            }
            clip.placement.start_offset_seconds = 1e-12;
            assert_eq!(
                clock.audio_clip_range_frames(clip, rate).unwrap().0,
                ((anchor + 1e-12) * f64::from(rate)).round() as u64
            );
            clip.placement.start_offset_seconds = 0.0;
            assert_eq!(clock.audio_clip_range_frames(clip, rate).unwrap().0, legacy);
        }
    }

    #[test]
    fn audio_clip_placement_supports_subgrid_ranges_and_content_extent() {
        let mut project = project();
        let clip = project.audio_clip_mut(0).unwrap();
        clip.time_range.end = clip.time_range.start;
        clip.placement.end_offset_seconds = 0.010;
        crate::validate(&project).unwrap();
        let clock = ProjectClock::new(&project.properties).unwrap();
        let (start, end) = clock
            .audio_clip_range_seconds(project.audio_clip(0).unwrap())
            .unwrap();
        assert!((end - start - 0.010).abs() < 1e-12);
        let content_end = crate::arrangement::content_end_quarter(&project).unwrap();
        assert!((clock.seconds_at_quarter(content_end).unwrap() - end).abs() < 1e-12);
    }

    #[test]
    fn audio_clip_placement_rejects_invalid_effective_bounds() {
        for (start, end) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (-10.0, 0.0),
            (3.0, 0.0),
        ] {
            let mut project = project();
            project.audio_clip_mut(0).unwrap().placement = crate::ast::AudioClipPlacement {
                start_offset_seconds: start,
                end_offset_seconds: end,
            };
            let clock = ProjectClock::new(&project.properties).unwrap();
            assert!(
                clock
                    .audio_clip_range_seconds(project.audio_clip(0).unwrap())
                    .is_err()
            );
            assert!(crate::validate(&project).is_err());
        }
    }
}
