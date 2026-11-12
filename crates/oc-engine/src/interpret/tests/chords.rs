//! Chord tokens: simultaneous voices, one attack, one amp envelope per voice.

#[cfg(feature = "plugin-host")]
use std::path::Path;

use super::super::*;
#[cfg(feature = "plugin-host")]
use crate::render::render_to_buffer_per_track;

const RATE: u32 = 48_000;

fn preset(filter_env: bool) -> String {
    let amount = if filter_env { 0.8 } else { 0.0 };
    format!(
        r#"@preset chord_tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {{osc.a.waveform: 0, osc.a.level: 1, osc.a.unison: 1, amp.attack: 0.002, amp.decay: 0.001, amp.sustain: 1.0, amp.release: 0.005, filter.cutoff: 18000, filter.resonance: 0.0, filter.env.amount: {amount}, voice.polyphony: 16, output.gain: 0.2}}
"#
    )
}

fn project(events: &str, filter_env: bool, moving_filter: bool) -> Project {
    let lane = if moving_filter {
        "  automation: pluginparam_66696c7465722e6375746f6666 {1:1:0=0.15, 2:1:0=0.85}\n"
    } else {
        ""
    };
    let text = format!(
        r#"@ocp "0.2.0"
@project "Chords"
  tempo: 120
  time_signature: 4/4

{}@track keys | preset: chord_tone, instrument: synth/pad
{lane}  @phrase chord [2:1:0 - 3:1:0]
{events}"#,
        preset(filter_env)
    );
    oc_dsl::parse(&text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

#[cfg(feature = "plugin-host")]
fn render(events: &str, filter_env: bool, moving_filter: bool) -> Vec<f32> {
    render_to_buffer_per_track(
        &project(events, filter_env, moving_filter),
        Path::new("unused.sf2"),
        RATE,
    )
    .expect("render")
}

#[cfg(feature = "plugin-host")]
fn left(buffer: &[f32]) -> Vec<f32> {
    buffer.iter().step_by(2).copied().collect()
}

/// Hann-windowed single-frequency DFT magnitude; the window keeps the leakage
/// 30 Hz away far below the fundamentals.
#[cfg(feature = "plugin-host")]
fn magnitude(samples: &[f32], frequency: f64) -> f64 {
    let step = std::f64::consts::TAU * frequency / RATE as f64;
    let last = (samples.len() - 1) as f64;
    let (mut real, mut imaginary, mut norm) = (0.0f64, 0.0f64, 0.0f64);
    for (index, sample) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / last).cos();
        let phase = step * index as f64;
        real += f64::from(*sample) * window * phase.cos();
        imaginary -= f64::from(*sample) * window * phase.sin();
        norm += window;
    }
    (real * real + imaginary * imaginary).sqrt() / norm
}

#[cfg(feature = "plugin-host")]
fn rms(samples: &[f32], start: usize, end: usize) -> f64 {
    let window = &samples[start..end];
    let sum: f64 = window.iter().map(|sample| f64::from(*sample).powi(2)).sum();
    (sum / window.len() as f64).sqrt()
}

/// 120 bpm, 48 kHz: one beat is 24 000 samples; the phrase starts at bar 2,
/// one bar (four beats) into the project.
const BEAT: usize = 24_000;
const OFFSET: usize = 4 * BEAT;

#[test]
#[cfg(feature = "plugin-host")]
fn chord_sounds_both_fundamentals_without_a_second_attack() {
    let chord = left(&render("[A4 A5] h mf\n", false, false));
    let sequential = left(&render("A4 q mf\n    A5 q mf\n", false, false));

    // A long window resolves the two fundamentals; 20 ms windows track them.
    let spectrum = &chord[OFFSET + BEAT / 2..OFFSET + BEAT / 2 + 16_384];
    let a4 = magnitude(spectrum, 440.0);
    let a5 = magnitude(spectrum, 880.0);
    let neighbour = magnitude(spectrum, 410.0)
        .max(magnitude(spectrum, 470.0))
        .max(magnitude(spectrum, 850.0))
        .max(magnitude(spectrum, 910.0));
    assert!(
        a4 > 30.0 * neighbour,
        "440 Hz {a4} vs neighbour {neighbour}"
    );
    assert!(
        a5 > 30.0 * neighbour,
        "880 Hz {a5} vs neighbour {neighbour}"
    );
    assert!(
        (a4 / a5 - 1.0).abs() < 0.25,
        "both fundamentals should carry comparable energy: {a4} vs {a5}"
    );

    let early = OFFSET + BEAT / 2;
    let late = OFFSET + BEAT + BEAT / 2;
    fn window(samples: &[f32], start: usize) -> &[f32] {
        &samples[start..start + 960]
    }

    let a4_early = magnitude(window(&chord, early), 440.0);
    let a5_early = magnitude(window(&chord, early), 880.0);
    let a4_late = magnitude(window(&chord, late), 440.0);
    let a5_late = magnitude(window(&chord, late), 880.0);

    // One attack: the upper partial is already at its steady level in beat 1 and
    // does not re-onset at beat 2. A sequential pair is silent there.
    assert!(
        (a4_late / a4_early - 1.0).abs() < 0.1,
        "the 440 Hz partial re-attacked: {a4_early} then {a4_late}"
    );
    assert!(
        (a5_late / a5_early - 1.0).abs() < 0.1,
        "the 880 Hz partial re-attacked: {a5_early} then {a5_late}"
    );
    let sequential_early = magnitude(window(&sequential, early), 880.0);
    let sequential_late = magnitude(window(&sequential, late), 880.0);
    assert!(
        sequential_early < 0.1 * sequential_late,
        "the reference must not sound 880 Hz before its second note: {sequential_early} vs {sequential_late}"
    );

    // Both partials are there from the chord's own onset, not a beat apart.
    let onset = &chord[OFFSET..OFFSET + 960];
    assert!(
        magnitude(onset, 440.0) > 0.5 * a4_early,
        "440 Hz is missing from the chord onset"
    );
    assert!(
        magnitude(onset, 880.0) > 0.5 * a5_early,
        "880 Hz is missing from the chord onset"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn chord_under_a_moving_filter_is_the_sum_of_its_voices() {
    let chord = left(&render("[A4 A5] h mf\n", true, true));
    let a4 = left(&render("A4 h mf\n", true, true));
    let a5 = left(&render("A5 h mf\n", true, true));

    assert_eq!(chord.len(), a4.len());
    assert_eq!(chord.len(), a5.len());
    let mut worst: f32 = 0.0;
    for ((chord, a4), a5) in chord.iter().zip(&a4).zip(&a5) {
        worst = worst.max((*chord - (a4 + a5)).abs());
    }
    assert!(
        worst < 1e-5,
        "a chord must be one voice per pitch, not one event rendered twice: worst {worst}"
    );
    assert!(
        rms(&a4, OFFSET, OFFSET + BEAT) > 0.01,
        "the reference voice is silent"
    );
}

#[test]
fn chord_notes_stop_at_the_event_boundary() {
    let project = project("[C4 E4] q mf\n    G4 q mf\n", false, false);
    let settings = InterpretSettings {
        sample_rate: RATE,
        ..Default::default()
    };
    let events = try_interpret(&project, &settings).unwrap();
    let at = |time: u64, matcher: &dyn Fn(&MidiMessage) -> bool| {
        events
            .iter()
            .any(|event| event.time_samples == time && matcher(&event.event))
    };
    let on = |note: u8| {
        move |message: &MidiMessage| {
            matches!(
                message,
                MidiMessage::NoteOn {
                    note: played,
                    ..
                } if *played == note
            )
        }
    };
    let off = |note: u8| {
        move |message: &MidiMessage| {
            matches!(
                message,
                MidiMessage::NoteOff {
                    note: stopped,
                    ..
                } if *stopped == note
            )
        }
    };
    let start = OFFSET as u64;
    let boundary = (OFFSET + BEAT) as u64;
    assert!(
        at(start, &on(60)) && at(start, &on(64)),
        "chord onsets at bar 2"
    );
    assert!(at(boundary, &off(60)), "C4 stops at the event boundary");
    assert!(at(boundary, &off(64)), "E4 stops at the event boundary");
    assert!(at(boundary, &on(67)), "G4 starts at the event boundary");
}

#[test]
fn chord_in_a_placed_phrase_keeps_its_position() {
    let project = project("[C4 E4] q mf\n", false, false);
    let settings = InterpretSettings {
        sample_rate: RATE,
        ..Default::default()
    };
    let events = try_interpret(&project, &settings).unwrap();
    // The phrase starts at bar 2 beat 1: one bar is four beats.
    let offset = 4 * BEAT as u64;
    let starts: Vec<u64> = events
        .iter()
        .filter(|event| matches!(event.event, MidiMessage::NoteOn { note: 60 | 64, .. }))
        .map(|event| event.time_samples)
        .collect();
    assert_eq!(starts, vec![offset, offset]);
}

#[test]
#[cfg(feature = "plugin-host")]
fn comma_form_and_chord_sugar_render_byte_identically() {
    // A pure chord line, a comma list, and a mixed line: the sugar expands to
    // exactly the comma form before the interpreter ever sees it.
    let sugar = render(
        "@0 [A4 A5] h mf\n    @1 C4 q mf, E4 q mf\n    @2 [G4 B4] q mp, D5 q pp\n",
        true,
        true,
    );
    let comma = render(
        "@0 A4 h mf, A5 h mf\n    @1 C4 q mf, E4 q mf\n    @2 G4 q mp, B4 q mp, D5 q pp\n",
        true,
        true,
    );
    assert!(
        left(&comma).iter().any(|sample| sample.abs() > 1e-4),
        "the compared project must actually sound"
    );
    assert_eq!(sugar.len(), comma.len());
    let differences = sugar
        .iter()
        .zip(&comma)
        .filter(|(left, right)| left.to_bits() != right.to_bits())
        .count();
    assert_eq!(differences, 0, "every sample must be bit-identical");
}
