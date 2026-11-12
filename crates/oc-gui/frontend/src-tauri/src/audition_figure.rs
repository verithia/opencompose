//! The one note figure every preset audition plays: a short ascending scale,
//! or a kick/snare/hat groove on the GM drum channel.

use oc_dsl::ast::{BaseDuration, Duration, NoteEvent, Pitch};

/// Eighths at this tempo keep the figure near 1.3 s of notes.
pub(crate) const TEMPO_BPM: f64 = 140.0;
/// One step is an eighth note, so the figure spans three quarters.
pub(crate) const STEP_QUARTERS: f64 = 0.5;
/// Release tail rendered after the last note.
#[cfg(test)]
pub(crate) const TAIL_SECONDS: f64 = 1.0;

/// C-major pentatonic ascent over an octave: attack, body and release all sound.
const PITCHED: &[(u8, u8)] = &[
    (60, 100),
    (62, 92),
    (64, 96),
    (67, 100),
    (69, 92),
    (72, 104),
];
/// Drum kits have no pitch to climb, so the drum channel gets a groove instead.
const DRUMS: &[(u8, u8)] = &[
    (36, 110),
    (42, 84),
    (38, 100),
    (42, 84),
    (36, 110),
    (42, 84),
];

#[cfg(test)]
const BLOCK_FRAMES: u32 = 128;

/// Notes the audition plays, as (MIDI note, velocity).
pub(crate) fn notes(drums: bool) -> &'static [(u8, u8)] {
    if drums { DRUMS } else { PITCHED }
}

/// Frames of one eighth note at the audition tempo.
#[cfg(test)]
fn step_frames(sample_rate: u32) -> u32 {
    (f64::from(sample_rate) * 60.0 / TEMPO_BPM * STEP_QUARTERS).round() as u32
}

/// Note-on length inside one step; the gap articulates consecutive notes.
#[cfg(test)]
fn gate_frames(sample_rate: u32) -> u32 {
    step_frames(sample_rate).saturating_sub(sample_rate / 50)
}

/// Frames of the whole figure plus one release tail.
#[cfg(test)]
pub(crate) fn total_frames(sample_rate: u32, drums: bool) -> u32 {
    notes(drums).len() as u32 * step_frames(sample_rate)
        + (f64::from(sample_rate) * TAIL_SECONDS).round() as u32
}

/// A note-driven instrument the figure can play through.
#[cfg(test)]
pub(crate) trait FigureVoice {
    fn note_on(&mut self, note: u8, velocity: u8) -> Result<(), String>;
    fn note_off(&mut self, note: u8) -> Result<(), String>;
    fn render(&mut self, frames: u32) -> Vec<f32>;
}

/// Render the figure as interleaved stereo through any note-taking instrument.
#[cfg(test)]
pub(crate) fn render(
    voice: &mut dyn FigureVoice,
    sample_rate: u32,
    drums: bool,
) -> Result<Vec<f32>, String> {
    let total = total_frames(sample_rate, drums);
    let mut buffer = Vec::with_capacity(total as usize * 2);
    let mut position = 0u32;
    let step = step_frames(sample_rate);
    let gate = gate_frames(sample_rate);
    for (index, &(note, velocity)) in notes(drums).iter().enumerate() {
        let on = index as u32 * step;
        advance(voice, &mut buffer, &mut position, on);
        voice.note_on(note, velocity)?;
        advance(voice, &mut buffer, &mut position, on + gate);
        voice.note_off(note)?;
    }
    advance(voice, &mut buffer, &mut position, total);
    Ok(buffer)
}

/// Render frames up to `until`, so note events land on their exact frame.
#[cfg(test)]
fn advance(voice: &mut dyn FigureVoice, buffer: &mut Vec<f32>, position: &mut u32, until: u32) {
    while *position < until {
        let frames = (until - *position).min(BLOCK_FRAMES);
        buffer.extend_from_slice(&voice.render(frames));
        *position += frames;
    }
}

/// The same figure as phrase events for the streamed builtin/sampler audition.
#[cfg(test)]
pub(crate) fn phrase_events() -> Vec<NoteEvent> {
    phrase_events_for(false)
}

#[cfg(test)]
pub(crate) fn phrase_events_for(drums: bool) -> Vec<NoteEvent> {
    phrase_events_in(drums, 0)
}

/// The figure transposed by `semitones`, so a low patch is auditioned where it
/// is meant to sound instead of two octaves above its own cutoff.
pub(crate) fn phrase_events_in(drums: bool, semitones: i16) -> Vec<NoteEvent> {
    notes(drums)
        .iter()
        .enumerate()
        .map(|(index, &(note, velocity))| NoteEvent {
            start_beat: index as f64 * STEP_QUARTERS,
            explicit_position: false,
            pitch: Pitch::from_midi((i32::from(note) + i32::from(semitones)).clamp(0, 127) as u8)
                .expect("audition transposition is clamped to seven-bit MIDI"),
            duration: Duration {
                base: BaseDuration::Eighth,
                dots: 0,
                triplet: false,
                tuplet: None,
                ties: Vec::new(),
            },
            dynamics: None,
            velocity: Some(velocity),
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the note stream and yields silence for any block.
    #[derive(Default)]
    struct Recorder {
        on: Vec<(u8, u8)>,
        off: Vec<u8>,
        frames: u32,
    }

    impl FigureVoice for Recorder {
        fn note_on(&mut self, note: u8, velocity: u8) -> Result<(), String> {
            self.on.push((note, velocity));
            Ok(())
        }

        fn note_off(&mut self, note: u8) -> Result<(), String> {
            self.off.push(note);
            Ok(())
        }

        fn render(&mut self, frames: u32) -> Vec<f32> {
            self.frames += frames;
            vec![0.0; frames as usize * 2]
        }
    }

    #[test]
    fn the_figure_climbs_more_than_one_note() {
        let notes = notes(false);
        assert!(notes.len() > 1);
        assert!(
            notes.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "the audition figure must ascend: {notes:?}"
        );
        assert_eq!(notes.first().unwrap().0, 60);
        assert_eq!(notes.last().unwrap().0, 72);
        let seconds = notes.len() as f64 * 60.0 / TEMPO_BPM * STEP_QUARTERS;
        assert!(
            (1.0..=2.0).contains(&seconds),
            "figure lasts {seconds}s, outside the 1-2s window"
        );
    }

    #[test]
    fn the_drum_channel_gets_a_groove_instead_of_a_scale() {
        let drums = notes(true);
        assert_eq!(drums.len(), notes(false).len());
        assert_eq!(drums[0].0, 36, "the groove starts on the kick");
        assert!(drums.iter().any(|&(note, _)| note == 38), "snare");
        assert!(drums.iter().any(|&(note, _)| note == 42), "hihat");
    }

    #[test]
    fn every_note_is_played_and_released_in_order() {
        let sample_rate = 48_000;
        let mut recorder = Recorder::default();
        let buffer =
            render(&mut recorder, sample_rate, false).expect("figure renders through any voice");
        let expected = notes(false);
        assert_eq!(recorder.on.len(), expected.len());
        assert!(recorder.on.len() > 1);
        assert_eq!(
            recorder
                .on
                .iter()
                .map(|&(note, _)| note)
                .collect::<Vec<_>>(),
            expected.iter().map(|&(note, _)| note).collect::<Vec<_>>()
        );
        assert_eq!(recorder.off.len(), expected.len());
        assert_eq!(buffer.len(), total_frames(sample_rate, false) as usize * 2);
        assert_eq!(recorder.frames, total_frames(sample_rate, false));
    }

    #[test]
    fn the_rendered_length_keeps_a_release_tail() {
        let sample_rate = 48_000;
        let step = step_frames(sample_rate);
        assert_eq!(step, 10_286);
        let total = total_frames(sample_rate, false);
        assert_eq!(
            total,
            notes(false).len() as u32 * step + sample_rate,
            "one second of tail follows the last note"
        );
    }

    #[test]
    fn phrase_events_carry_the_scale_to_the_streamed_audition() {
        let events = phrase_events();
        let expected = notes(false);
        assert_eq!(events.len(), expected.len());
        assert!(events.len() > 1);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(
                event.start_beat,
                index as f64 * STEP_QUARTERS,
                "notes step by an eighth"
            );
            assert_eq!(event.pitch.to_midi(), expected[index].0);
            assert_eq!(event.velocity, Some(expected[index].1));
            assert_eq!(event.duration.to_beats(), STEP_QUARTERS);
        }
    }
}
