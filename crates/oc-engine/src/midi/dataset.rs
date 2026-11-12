//! Dataset presentation of the same parsed MIDI performance.
//! Source decoding, channel handling, clocks and raw event interpretation stay
//! in the parent importer; this module only selects notation and annotations.
use super::*;

pub(super) fn validate_clocks(clocks: &ClockCollector) -> Result<(), MidiError> {
    fn insert<T: PartialEq>(
        map: &mut BTreeMap<u64, T>,
        tick: u64,
        value: T,
        kind: &str,
        offset: usize,
    ) -> Result<(), MidiError> {
        if map.get(&tick).is_some_and(|old| *old != value) {
            return Err(MidiError::InvalidEvent {
                offset,
                message: format!("conflicting MIDI {kind} events at tick {tick}"),
            });
        }
        map.insert(tick, value);
        Ok(())
    }
    let mut tempos = BTreeMap::new();
    for event in &clocks.tempos {
        insert(&mut tempos, event.tick, event.micros, "tempo", event.offset)?;
    }
    let mut meters = BTreeMap::new();
    for event in &clocks.meters {
        insert(
            &mut meters,
            event.tick,
            (event.numerator, event.denominator),
            "meter",
            event.offset,
        )?;
    }
    Ok(())
}

pub(super) fn note_modifiers(note: &NoteTick, ppq: u16) -> Vec<(String, ParamValue)> {
    let start = note.start_tick as f64 / f64::from(ppq);
    let duration = (note.end_tick - note.start_tick) as f64 / f64::from(ppq);
    let mut modifiers = vec![
        ("gate_quarters".into(), ParamValue::Number(duration)),
        (
            "midi_start_tick".into(),
            ParamValue::Number(note.start_tick as f64),
        ),
        (
            "midi_duration_ticks".into(),
            ParamValue::Number((note.end_tick - note.start_tick) as f64),
        ),
        ("duration_beats".into(), ParamValue::Number(duration)),
    ];
    let offset = start - (start * 4.0).round() / 4.0;
    if offset.abs() > 1.0 / f64::from(ppq) {
        modifiers.push(("humanize_offset_beats".into(), ParamValue::Number(offset)));
    }
    modifiers
}

pub(super) fn notation_duration(beats: f64) -> Duration {
    // Quantize to nearest standard duration
    let (base, dots) = if beats >= 3.5 {
        (BaseDuration::Whole, 0)
    } else if beats >= 2.5 {
        (BaseDuration::Half, 1) // dotted half
    } else if beats >= 1.8 {
        (BaseDuration::Half, 0)
    } else if beats >= 1.3 {
        (BaseDuration::Quarter, 1) // dotted quarter
    } else if beats >= 0.8 {
        (BaseDuration::Quarter, 0)
    } else if beats >= 0.6 {
        (BaseDuration::Eighth, 1) // dotted eighth
    } else if beats >= 0.4 {
        (BaseDuration::Eighth, 0)
    } else if beats >= 0.2 {
        (BaseDuration::Sixteenth, 0)
    } else {
        (BaseDuration::ThirtySecond, 0)
    };

    Duration {
        base,
        dots,
        triplet: false,
        tuplet: None,
        ties: vec![],
    }
}

pub(super) fn drum_pattern(unit: &Unit, clock: &ProjectClock) -> Result<Pattern, MidiError> {
    let mut hits = Vec::with_capacity(unit.notes.len());
    let mut last_quarter = 0.0_f64;
    for note in &unit.notes {
        let start = note.start_tick as f64 / f64::from(unit.ppq);
        let grid = (start * 4.0).round() / 4.0;
        let whole = grid.floor();
        if whole > f64::from(u32::MAX - 1) {
            return Err(MidiError::InvalidEvent {
                offset: 0,
                message: "drum position exceeds the project beat range".into(),
            });
        }
        let offset = start - grid;
        let mut modifiers = note_modifiers(note, unit.ppq);
        modifiers.push(("midi_note".into(), ParamValue::Number(f64::from(note.key))));
        modifiers.push((
            "velocity".into(),
            ParamValue::Number(f64::from(note.velocity)),
        ));
        if offset != 0.0 {
            modifiers.push(("grid_offset_beats".into(), ParamValue::Number(offset)));
        }
        hits.push(HitEvent {
            drum: gm_percussion_to_name(note.key).into(),
            position: BeatPosition {
                beat: whole as u32 + 1,
                subdivision: ((grid - whole) * 4.0).round() as u32,
            },
            dynamics: Some(velocity_to_dynamics(note.velocity)),
            velocity: Some(note.velocity),
            articulation: None,
            modifiers,
        });
        last_quarter = last_quarter.max(start + 0.25);
    }
    let end = enclosing_bar(clock, last_quarter)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?;
    Ok(Pattern {
        name: "main".into(),
        time_range: Some(TimeRange {
            start: BarBeat::new(1, 1, 0),
            end: BarBeat::new(end, 1, 0),
        }),
        repeat: 1,
        hits,
    })
}

fn gm_percussion_to_name(midi: u8) -> &'static str {
    match midi {
        35 | 36 => "kick",
        37 => "rim",
        38 | 40 => "snare",
        39 => "clap",
        41 | 43 => "tom_low",
        42 | 44 => "hihat", // closed
        45 | 47 => "tom_mid",
        46 => "hihat", // open
        48 | 50 => "tom_high",
        49 | 57 => "crash",
        51 | 59 => "ride",
        52 => "china",
        53 => "ride", // ride bell
        54 => "tambourine",
        55 => "splash",
        56 => "cowbell",
        69 => "cabasa",
        70 => "shaker",
        73 | 74 => "guiro",
        75..=77 => "woodblock",
        _ => "snare", // fallback
    }
}
