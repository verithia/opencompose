//! The `gliss` note mark: its spellings and what a slide needs from the score.
//!
//! A mark is a note modifier, so a score keeps it through parse, `fmt` and
//! `export`. Nothing here renders: the engine expands a mark into the existing
//! primitives — `pitch_curve` for a slide, short notes for a run.

use crate::ast::{NoteEvent, ParamValue, Pitch};

/// The modifier key every mark is stored under.
pub const MODIFIER: &str = "gliss";
/// `gliss`: slide to the pitch of the next note.
pub const NEXT: &str = "next";
/// `gliss=run`: write the slide as chromatic short notes.
pub const RUN: &str = "run";
/// `gliss=run_diatonic`: step through the project key instead.
pub const RUN_DIATONIC: &str = "run_diatonic";
/// A written length is a curve of that many seconds. The synth's own glide
/// parameter stops at four seconds, so a lengthened mark stops there too.
pub const MAX_PORTAMENTO_SECONDS: f64 = 4.0;

/// Every spelling a refusal offers a model that guessed.
pub const FORMS: &str = "write gliss (to the next note), gliss=G4 (to a written \
    pitch), gliss=120ms (reach the next note that early) or gliss=run (a run of \
    short notes)";

/// How a run steps between the two pitches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStyle {
    /// Every semitone between the two pitches.
    Chromatic,
    /// The pitches of the project key between the two.
    Diatonic,
}

/// One note's glissando mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gliss {
    /// `gliss`: slide to the next note's pitch over this note.
    ToNext,
    /// `gliss=G4`: slide to a written pitch.
    To(Pitch),
    /// `gliss=120ms`: reach the destination after that long, then hold it.
    Portamento(f64),
    /// `gliss=run` / `gliss=run_diatonic`: a run of short notes.
    Run(RunStyle),
}

impl Gliss {
    /// Whether the destination is the next note, so one must follow.
    pub fn needs_successor(self) -> bool {
        !matches!(self, Gliss::To(_))
    }

    /// The mark as it is written, for a refusal that quotes it.
    pub fn spelling(self) -> String {
        match self {
            Gliss::ToNext => MODIFIER.to_string(),
            Gliss::To(pitch) => format!("{MODIFIER}={}", pitch.to_text()),
            Gliss::Portamento(seconds) => {
                format!("{MODIFIER}={}ms", seconds * 1000.0)
            }
            Gliss::Run(RunStyle::Chromatic) => format!("{MODIFIER}={RUN}"),
            Gliss::Run(RunStyle::Diatonic) => format!("{MODIFIER}={RUN_DIATONIC}"),
        }
    }
}

/// One note's mark, or `None` when the note carries none.
pub fn form(modifiers: &[(String, ParamValue)]) -> Result<Option<Gliss>, String> {
    let mut found = modifiers.iter().filter(|(key, _)| key == MODIFIER);
    let Some((_, value)) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(format!("duplicate {MODIFIER}: one mark per note"));
    }
    match value {
        ParamValue::String(text) if text == NEXT => Ok(Some(Gliss::ToNext)),
        ParamValue::String(text) if text == RUN => Ok(Some(Gliss::Run(RunStyle::Chromatic))),
        ParamValue::String(text) if text == RUN_DIATONIC => {
            Ok(Some(Gliss::Run(RunStyle::Diatonic)))
        }
        ParamValue::String(text) => match Pitch::from_text(text) {
            Some(pitch) => Ok(Some(Gliss::To(pitch))),
            None => Err(format!(
                "{MODIFIER}={text} is not a glissando form: {FORMS}"
            )),
        },
        ParamValue::Time(seconds)
            if seconds.is_finite() && *seconds > 0.0 && *seconds <= MAX_PORTAMENTO_SECONDS =>
        {
            Ok(Some(Gliss::Portamento(*seconds)))
        }
        ParamValue::Time(seconds) => Err(format!(
            "{MODIFIER}={}ms is out of range: a length is more than 0 and at most {}ms, or {FORMS}",
            seconds * 1000.0,
            MAX_PORTAMENTO_SECONDS * 1000.0
        )),
        _ => Err(format!(
            "{MODIFIER} needs a pitch, a length or run: {FORMS}"
        )),
    }
}

/// What a mark that names no destination has to slide to.
#[derive(Debug, Clone, PartialEq)]
pub enum Successor {
    /// No later event of the phrase sounds.
    None,
    /// The next note's pitch.
    One(Pitch),
    /// The next onset holds a chord, so the destination is ambiguous.
    Chord(Vec<Pitch>),
}

/// The next sounding note *written after* this one in the same phrase. A rest
/// and a muted note do not sound, so they are skipped.
pub fn successor(events: &[NoteEvent], index: usize) -> Successor {
    let Some(note) = events.get(index) else {
        return Successor::None;
    };
    let later = || {
        events[index + 1..]
            .iter()
            .filter(|event| sounds(event) && event.start_beat >= note.start_beat)
    };
    let Some(onset) = later()
        .map(|event| event.start_beat)
        .min_by(|left, right| left.total_cmp(right))
    else {
        return Successor::None;
    };
    let pitches: Vec<Pitch> = later()
        .filter(|event| event.start_beat == onset)
        .map(|event| event.pitch)
        .collect();
    match pitches.len() {
        0 => Successor::None,
        1 => Successor::One(pitches[0]),
        _ => Successor::Chord(pitches),
    }
}

fn sounds(event: &NoteEvent) -> bool {
    !event.muted && !event.modifiers.iter().any(|(key, _)| key == "rest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Accidental, Duration, NoteName};

    fn text(value: &str) -> Vec<(String, ParamValue)> {
        vec![(MODIFIER.into(), ParamValue::String(value.into()))]
    }

    #[test]
    fn forms_parse_from_the_stored_modifier() {
        assert_eq!(form(&[]).unwrap(), None);
        assert_eq!(form(&text(NEXT)).unwrap(), Some(Gliss::ToNext));
        assert_eq!(
            form(&text("Bb3")).unwrap(),
            Some(Gliss::To(Pitch {
                note: NoteName::B,
                accidental: Accidental::Flat,
                octave: 3
            }))
        );
        assert_eq!(
            form(&text(RUN)).unwrap(),
            Some(Gliss::Run(RunStyle::Chromatic))
        );
        assert_eq!(
            form(&[(MODIFIER.into(), ParamValue::Time(0.12))]).unwrap(),
            Some(Gliss::Portamento(0.12))
        );
        assert_eq!(
            form(&text("nope")).unwrap_err(),
            format!("gliss=nope is not a glissando form: {FORMS}")
        );
        assert!(form(&[(MODIFIER.into(), ParamValue::Time(5.0))]).is_err());
        assert_eq!(form(&text(RUN)).unwrap().unwrap().spelling(), "gliss=run");
    }

    fn note(start_beat: f64, pitch: &str, rest: bool) -> NoteEvent {
        NoteEvent {
            start_beat,
            explicit_position: true,
            pitch: Pitch::from_text(pitch).unwrap(),
            duration: Duration {
                base: crate::ast::BaseDuration::Quarter,
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
            modifiers: if rest {
                vec![("rest".into(), ParamValue::Bool(true))]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn successor_skips_rests_and_names_a_chord() {
        let events = vec![
            note(0.0, "C4", false),
            note(1.0, "D4", true),
            note(2.0, "E4", false),
        ];
        assert_eq!(
            successor(&events, 0),
            Successor::One(Pitch::from_text("E4").unwrap())
        );
        assert_eq!(successor(&events, 2), Successor::None);
        let chord = vec![
            note(0.0, "C4", false),
            note(1.0, "E4", false),
            note(1.0, "G4", false),
        ];
        assert_eq!(
            successor(&chord, 0),
            Successor::Chord(vec![
                Pitch::from_text("E4").unwrap(),
                Pitch::from_text("G4").unwrap()
            ])
        );
    }
}
