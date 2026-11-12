//! Ornament marks and the notes they expand to.
//!
//! `ornament=trill`, `ornament=mordent`, `ornament=fermata` and
//! `ornament=arpeggio` are stored in the existing note modifier list. The
//! marks add no AST field: playback expands them into ordinary notes or a
//! changed gate, and `fmt` keeps the mark as it was written.
//!
//! The expansion is deliberately small and measurable. A trill alternates with
//! the next scale step at a sixteenth-note period and ends on the written
//! pitch. A mordent plays the written pitch, the step above, then holds the
//! written pitch. A fermata lengthens the gate by half. An arpeggio delays each
//! member of one onset by a thirty-second note in ascending pitch order.

use crate::ast::{KeyMode, KeySignature, NoteEvent, ParamValue, Pitch};

/// The modifier key every ornament is stored under.
pub const MODIFIER: &str = "ornament";
/// One alternation of a trill or mordent, in quarter notes.
pub const STEP_QUARTERS: f64 = 0.25;
/// One arpeggio delay, in quarter notes.
pub const ARPEGGIO_STEP_QUARTERS: f64 = 0.125;
/// A fermata holds the written gate this many times.
pub const FERMATA_SCALE: f64 = 1.5;

/// Every spelling a refusal offers.
pub const FORMS: &str =
    "write ornament=trill, ornament=mordent, ornament=fermata or ornament=arpeggio";

/// One written ornament.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ornament {
    /// Alternate with the next scale step.
    Trill,
    /// Written pitch, step above, held written pitch.
    Mordent,
    /// Hold the written gate longer.
    Fermata,
    /// Roll the members of one onset.
    Arpeggio,
}

impl Ornament {
    /// Canonical DSL spelling.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Trill => "trill",
            Self::Mordent => "mordent",
            Self::Fermata => "fermata",
            Self::Arpeggio => "arpeggio",
        }
    }

    /// Whether the mark changes pitch or splits one note into several.
    pub fn changes_pitch(self) -> bool {
        matches!(self, Self::Trill | Self::Mordent)
    }

    /// Whether the mark moves the onset instead of the gate.
    pub fn moves_onset(self) -> bool {
        matches!(self, Self::Arpeggio)
    }
}

/// One note or part of a note a mark produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Pitch this part sounds.
    pub pitch: Pitch,
    /// Offset from the written onset, in quarter notes.
    pub offset_quarters: f64,
    /// Gate length, in quarter notes.
    pub duration_quarters: f64,
}

/// Read the mark on a note, or `None` when it carries none.
///
/// A malformed value and a duplicate mark are refused here so a parser,
/// validator and the interpreter all report the same spelling.
pub fn form(modifiers: &[(String, ParamValue)]) -> Result<Option<Ornament>, String> {
    let mut found = modifiers.iter().filter(|(key, _)| key == MODIFIER);
    let Some((_, value)) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(format!("duplicate {MODIFIER}: one ornament per note"));
    }
    match value {
        ParamValue::String(text) if text == "trill" => Ok(Some(Ornament::Trill)),
        ParamValue::String(text) if text == "mordent" => Ok(Some(Ornament::Mordent)),
        ParamValue::String(text) if text == "fermata" => Ok(Some(Ornament::Fermata)),
        ParamValue::String(text) if text == "arpeggio" => Ok(Some(Ornament::Arpeggio)),
        ParamValue::String(text) => Err(format!("{MODIFIER}={text} is not an ornament: {FORMS}")),
        _ => Err(format!("{MODIFIER} needs a name: {FORMS}")),
    }
}

/// Refuse marks that would fight a per-note pitch control or articulation.
///
/// A trill and a mordent write their own pitch sequence, so a bend, vibrato,
/// glissando or expression curve would describe a second pitch or level path
/// for the same note. An articulation keyswitch would also fire once per
/// generated part, which is not a trill on the instrument. Fermata and
/// arpeggio change only time and keep every other note field.
pub fn check_combinations(ornament: Ornament, note: &NoteEvent) -> Result<(), String> {
    if !ornament.changes_pitch() {
        return Ok(());
    }
    if note.articulation.is_some() {
        return Err(format!(
            "{MODIFIER}={} writes its own note sequence, so it cannot share the note with articulation=: remove one of them",
            ornament.as_token()
        ));
    }
    let conflicting = [
        ("bend", note.pitch_bend.is_some()),
        ("vibrato", note.vibrato.is_some()),
        ("expression", note.expression.is_some()),
        (
            "gliss",
            note.modifiers
                .iter()
                .any(|(key, _)| key == crate::gliss::MODIFIER),
        ),
        (
            "pitch_curve",
            note.modifiers.iter().any(|(key, _)| key == "pitch_curve"),
        ),
    ]
    .into_iter()
    .find_map(|(name, present)| present.then_some(name));
    if let Some(conflicting) = conflicting {
        return Err(format!(
            "{MODIFIER}={} writes its own note sequence, so it cannot share the note with {conflicting}=: remove one of them",
            ornament.as_token()
        ));
    }
    if note.modifiers.iter().any(|(key, _)| key == "rest") {
        return Err(format!(
            "{MODIFIER}={} sounds the note, so it cannot share the note with rest",
            ornament.as_token()
        ));
    }
    Ok(())
}

/// Expand one note into the parts its mark plays.
///
/// `total_quarters` is the gate the written note would have after its
/// articulation and profile corrections. A mark that changes no pitch returns
/// one segment, so callers always have one path.
pub fn segments(
    note: &NoteEvent,
    key: Option<KeySignature>,
    total_quarters: f64,
) -> Result<Vec<Segment>, String> {
    let Some(ornament) = form(&note.modifiers)? else {
        return Ok(vec![Segment {
            pitch: note.pitch,
            offset_quarters: 0.0,
            duration_quarters: total_quarters,
        }]);
    };
    check_combinations(ornament, note)?;
    if !total_quarters.is_finite() || total_quarters <= 0.0 {
        return Err(format!(
            "{MODIFIER}={} needs a positive gate, got {total_quarters} quarters",
            ornament.as_token()
        ));
    }
    match ornament {
        Ornament::Fermata => Ok(vec![Segment {
            pitch: note.pitch,
            offset_quarters: 0.0,
            duration_quarters: total_quarters * FERMATA_SCALE,
        }]),
        Ornament::Arpeggio => Ok(vec![Segment {
            pitch: note.pitch,
            offset_quarters: 0.0,
            duration_quarters: total_quarters,
        }]),
        Ornament::Trill => Ok(trill(note.pitch, key, total_quarters)),
        Ornament::Mordent => Ok(mordent(note.pitch, key, total_quarters)),
    }
}

/// Delays for every note of a phrase, indexed like the input.
///
/// A mark on any member of one onset rolls that whole onset. Members are
/// ordered by sounding pitch, and an unmarked onset keeps zero delay. The
/// function never allocates when no note carries an arpeggio.
pub fn arpeggio_offsets(notes: &[NoteEvent]) -> Vec<f64> {
    let mut offsets = vec![0.0; notes.len()];
    if !notes
        .iter()
        .any(|note| matches!(form(&note.modifiers), Ok(Some(Ornament::Arpeggio))))
    {
        return offsets;
    }
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|left, right| {
        notes[*left]
            .start_beat
            .total_cmp(&notes[*right].start_beat)
            .then_with(|| {
                notes[*left]
                    .pitch
                    .to_midi()
                    .cmp(&notes[*right].pitch.to_midi())
            })
            .then_with(|| left.cmp(right))
    });
    let mut group_start = 0;
    while group_start < order.len() {
        let onset = notes[order[group_start]].start_beat;
        let mut group_end = group_start + 1;
        while group_end < order.len() && (notes[order[group_end]].start_beat - onset).abs() <= 1e-6
        {
            group_end += 1;
        }
        let group = &order[group_start..group_end];
        let rolled = group
            .iter()
            .any(|index| matches!(form(&notes[*index].modifiers), Ok(Some(Ornament::Arpeggio))));
        if rolled {
            let mut by_pitch: Vec<usize> = group.to_vec();
            by_pitch.sort_by_key(|index| notes[*index].pitch.to_midi());
            for (rank, index) in by_pitch.into_iter().enumerate() {
                offsets[index] = rank as f64 * ARPEGGIO_STEP_QUARTERS;
            }
        }
        group_start = group_end;
    }
    offsets
}

/// The next scale step above a pitch.
///
/// A project key selects the step: in C major B4 steps to C5 and E4 to F4. A
/// note outside the scale takes the nearest step above it. Without a key the
/// step is one semitone, so a trill still has a defined upper pitch.
pub fn step_above(pitch: Pitch, key: Option<KeySignature>) -> Pitch {
    let midi = pitch.to_midi();
    let Some(key) = key else {
        return Pitch::from_midi(midi.saturating_add(1)).unwrap_or(pitch);
    };
    let intervals: &[i16] = match key.mode {
        KeyMode::Major => &[0, 2, 4, 5, 7, 9, 11],
        KeyMode::Minor => &[0, 2, 3, 5, 7, 8, 10],
    };
    let root = Pitch {
        note: key.root,
        accidental: key.accidental,
        octave: 0,
    }
    .semitone_number();
    let tonic = root.rem_euclid(12);
    for step in 1..=2 {
        let candidate = midi.saturating_add(step);
        let degree = (i16::from(candidate) - tonic).rem_euclid(12);
        if intervals.contains(&degree)
            && let Some(pitch) = Pitch::from_midi(candidate)
        {
            return pitch;
        }
    }
    Pitch::from_midi(midi.saturating_add(1)).unwrap_or(pitch)
}

fn trill(pitch: Pitch, key: Option<KeySignature>, total: f64) -> Vec<Segment> {
    let parts = (total / STEP_QUARTERS).floor() as usize;
    // An even part count would end on the upper pitch; drop one so the trill
    // returns to the written pitch, as a player would close it.
    let parts = if parts.is_multiple_of(2) {
        parts - 1
    } else {
        parts
    };
    if parts < 3 {
        return vec![Segment {
            pitch,
            offset_quarters: 0.0,
            duration_quarters: total,
        }];
    }
    let step = total / parts as f64;
    let upper = step_above(pitch, key);
    (0..parts)
        .map(|index| Segment {
            pitch: if index % 2 == 0 { pitch } else { upper },
            offset_quarters: step * index as f64,
            duration_quarters: step,
        })
        .collect()
}

fn mordent(pitch: Pitch, key: Option<KeySignature>, total: f64) -> Vec<Segment> {
    if total < STEP_QUARTERS * 2.0 {
        return vec![Segment {
            pitch,
            offset_quarters: 0.0,
            duration_quarters: total,
        }];
    }
    let upper = step_above(pitch, key);
    if total < STEP_QUARTERS * 3.0 {
        // A short note has no room for two sixteenths and a hold, so its three
        // parts share the gate equally instead of losing the closing pitch.
        let step = total / 3.0;
        return vec![
            Segment {
                pitch,
                offset_quarters: 0.0,
                duration_quarters: step,
            },
            Segment {
                pitch: upper,
                offset_quarters: step,
                duration_quarters: step,
            },
            Segment {
                pitch,
                offset_quarters: step * 2.0,
                duration_quarters: step,
            },
        ];
    }
    vec![
        Segment {
            pitch,
            offset_quarters: 0.0,
            duration_quarters: STEP_QUARTERS,
        },
        Segment {
            pitch: upper,
            offset_quarters: STEP_QUARTERS,
            duration_quarters: STEP_QUARTERS,
        },
        Segment {
            pitch,
            offset_quarters: STEP_QUARTERS * 2.0,
            duration_quarters: total - STEP_QUARTERS * 2.0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Accidental, BaseDuration, Duration, NoteName};

    fn note(start_beat: f64, pitch: &str) -> NoteEvent {
        NoteEvent {
            start_beat,
            explicit_position: true,
            pitch: Pitch::from_text(pitch).unwrap(),
            duration: Duration {
                base: BaseDuration::Quarter,
                dots: 0,
                triplet: false,
                tuplet: None,
                ties: Vec::new(),
            },
            dynamics: None,
            velocity: None,
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: Vec::new(),
        }
    }

    fn marked(value: &str) -> Vec<(String, ParamValue)> {
        vec![(MODIFIER.into(), ParamValue::String(value.into()))]
    }

    #[test]
    fn forms_parse_and_unknown_values_name_the_spellings() {
        assert_eq!(form(&[]).unwrap(), None);
        for (text, expected) in [
            ("trill", Ornament::Trill),
            ("mordent", Ornament::Mordent),
            ("fermata", Ornament::Fermata),
            ("arpeggio", Ornament::Arpeggio),
        ] {
            assert_eq!(form(&marked(text)).unwrap(), Some(expected));
        }
        assert_eq!(
            form(&marked("roll")).unwrap_err(),
            format!("{MODIFIER}=roll is not an ornament: {FORMS}")
        );
        assert!(form(&[(MODIFIER.into(), ParamValue::Bool(true))]).is_err());
        assert!(form(&[marked("trill"), marked("mordent")].concat()).is_err());
    }

    #[test]
    fn a_trill_alternates_by_scale_step_and_ends_on_the_written_pitch() {
        let mut note = note(0.0, "C4");
        note.modifiers = marked("trill");
        let segments = segments(&note, Some(c_major()), 1.0).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].pitch, note.pitch);
        assert_eq!(segments[1].pitch, Pitch::from_text("D4").unwrap());
        assert_eq!(segments[2].pitch, note.pitch);
        assert!(
            (segments
                .iter()
                .map(|part| part.duration_quarters)
                .sum::<f64>()
                - 1.0)
                .abs()
                < 1e-9
        );
        assert!((segments[2].offset_quarters - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_mordent_holds_the_written_pitch_after_the_upper_step() {
        let mut note = note(0.0, "E4");
        note.modifiers = marked("mordent");
        let segments = segments(&note, Some(c_major()), 2.0).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[1].pitch, Pitch::from_text("F4").unwrap());
        assert_eq!(segments[2].pitch, note.pitch);
        assert!((segments[2].duration_quarters - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_short_mordent_keeps_all_three_parts_inside_its_gate() {
        let mut note = note(0.0, "E4");
        note.modifiers = marked("mordent");
        let segments = segments(&note, Some(c_major()), 0.5).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments.iter().map(|part| part.pitch).collect::<Vec<_>>(),
            vec![note.pitch, Pitch::from_text("F4").unwrap(), note.pitch]
        );
        assert!(
            (segments
                .iter()
                .map(|part| part.duration_quarters)
                .sum::<f64>()
                - 0.5)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn a_short_note_is_left_whole_and_a_fermata_lengthens_it() {
        let short = note(0.0, "C4");
        assert_eq!(segments(&short, None, 0.25).unwrap().len(), 1);

        let mut held = short.clone();
        held.modifiers = marked("fermata");
        let parts = segments(&held, None, 1.0).unwrap();
        assert_eq!(parts.len(), 1);
        assert!((parts[0].duration_quarters - 1.5).abs() < 1e-9);
    }

    #[test]
    fn an_arpeggio_rolls_one_onset_in_pitch_order() {
        let mut low = note(0.0, "C4");
        let mut mid = note(0.0, "E4");
        let mut high = note(0.0, "G4");
        low.modifiers = marked("arpeggio");
        mid.modifiers = marked("arpeggio");
        high.modifiers = marked("arpeggio");
        let offsets = arpeggio_offsets(&[high, low, mid]);
        assert_eq!(offsets, vec![0.25, 0.0, 0.125]);
    }

    #[test]
    fn a_note_without_the_mark_has_no_delay() {
        let low = note(0.0, "C4");
        let high = note(0.0, "G4");
        assert_eq!(arpeggio_offsets(&[low, high]), vec![0.0, 0.0]);
    }

    #[test]
    fn a_pitch_control_cannot_share_a_trill() {
        let mut note = note(0.0, "C4");
        note.modifiers = marked("trill");
        note.pitch_bend = Some(20.0);
        assert!(segments(&note, None, 1.0).unwrap_err().contains("bend="));
    }

    fn c_major() -> KeySignature {
        KeySignature {
            root: NoteName::C,
            accidental: Accidental::Natural,
            mode: KeyMode::Major,
        }
    }
}
