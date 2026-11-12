//! Slip and loop: a pattern clip shows the part of its pattern its offset
//! chooses, and a loop repeats content for the placement's full length.
//!
//! These read the interpreted event stream directly, so they name the sample
//! the engine actually schedules rather than a waveform shape.

use oc_dsl::ast::Project;
use oc_engine::interpret::{InterpretSettings, MidiEvent, MidiMessage, try_interpret};

const RATE: u32 = 48_000;

fn project(text: &str) -> Project {
    let project = oc_dsl::parse(text).expect("fixture parses");
    oc_dsl::validate(&project).expect("fixture validates");
    project
}

fn settings() -> InterpretSettings {
    InterpretSettings {
        sample_rate: RATE,
        ..Default::default()
    }
}

/// `(sample, midi note)` for every note onset, in time order.
fn onsets(events: &[MidiEvent]) -> Vec<(u64, u8)> {
    events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, .. } => Some((event.time_samples, note)),
            _ => None,
        })
        .collect()
}

fn riff(text: &str) -> Project {
    project(&format!(
        r#"@project "Clip content"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
{text}
"#
    ))
}

#[test]
fn a_pattern_clip_without_slip_plays_its_pattern_from_the_clip_start() {
    let project = riff("  clip: riff [2:1 - 6:1]\n");
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // Bar 2 starts at four quarter notes: two seconds.
    assert_eq!(
        onsets,
        vec![(96_000, 60), (120_000, 62), (144_000, 64), (168_000, 65),]
    );
}

#[test]
fn a_slipped_pattern_clip_starts_at_the_offset_and_keeps_its_range() {
    let project = riff("  clip: riff [2:1 - 6:1] offset: 1q\n");
    let clip = &project.tracks[0].pattern_clip(0).unwrap();
    assert!((clip.offset_quarters - 1.0).abs() < 1e-9);
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // The C4 in the hidden head is gone; the rest moves back one quarter.
    assert_eq!(onsets, vec![(96_000, 62), (120_000, 64), (144_000, 65)]);
    // Slip moves content only: the clip still starts and ends where it did.
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(2, 1, 0));
    assert_eq!(clip.time_range.end, oc_dsl::ast::BarBeat::new(6, 1, 0));
}

#[test]
fn a_slip_past_the_content_plays_nothing() {
    let project = riff("  clip: riff [2:1 - 6:1] offset: 4.5q\n");
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    assert!(onsets.is_empty(), "{onsets:?}");
}

#[test]
fn a_slip_that_starts_inside_a_note_drops_that_note() {
    let project = riff("  clip: riff [2:1 - 6:1] offset: 0.5q\n");
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // The C4 begins at content beat 0, inside the hidden head, so it is silent.
    assert_eq!(onsets, vec![(108_000, 62), (132_000, 64), (156_000, 65)]);
}

#[test]
fn a_drum_hit_in_the_hidden_head_is_silent() {
    let project = project(
        r#"@project "Hit slip"
  tempo: 120
  time_signature: 4/4

@kit basic
  pad kick | note: C1
  pad snare | note: D1

@pattern beat | instrument: drumkit
  hits:
    kick 1:0 ff
    snare 2:0 ff
    kick 3:0 ff
    snare 4:0 ff

@track drums | instrument: drumkit, preset: basic
  clip: beat [1:1 - 2:1] offset: 1q
"#,
    );
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // The first kick sits in the hidden head; the other three start on beats
    // 1, 2 and 3 of the clip.
    assert_eq!(onsets, vec![(0, 26), (24_000, 24), (48_000, 26)]);
}

#[test]
fn a_looped_pattern_clip_repeats_its_content_for_the_clip_length() {
    let text = r#"@project "Loop"
  tempo: 120
  time_signature: 4/4

@pattern two | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: two [1:1 - 2:1] loop: 2q
"#;
    let project = project(text);
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // Two beats of content repeated across a four-beat clip.
    assert_eq!(
        onsets,
        vec![(0, 60), (24_000, 62), (48_000, 60), (72_000, 62)]
    );
}

#[test]
fn a_loop_excludes_the_note_at_the_source_window_end() {
    let text = r#"@project "Loop end"
  tempo: 120
  time_signature: 4/4

@pattern two | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: two [1:1 - 1:3] loop: 1q
"#;
    let project = project(text);
    let onsets = onsets(&try_interpret(&project, &settings()).unwrap());
    // D4 starts on the source window's exclusive end and never plays.
    assert_eq!(onsets, vec![(0, 60), (24_000, 60)]);
}

#[test]
fn a_slipped_loop_repeats_only_its_source_window_through_the_partial_final_cycle() {
    let project = riff("  clip: riff [1:1 - 2:1] offset: 1q loop: 1.5q\n");
    let events = try_interpret(&project, &settings()).unwrap();
    // Only D4 and E4 start in [1, 2.5). The last E4 would start at the
    // placement's exclusive end, four quarters into the timeline.
    assert_eq!(
        onsets(&events),
        vec![
            (0, 62),
            (24_000, 64),
            (36_000, 62),
            (60_000, 64),
            (72_000, 62)
        ]
    );
}

#[test]
fn a_loop_clamps_a_note_at_the_placement_end() {
    let text = r#"@project "Loop tail"
  tempo: 120
  time_signature: 4/4

@pattern long | instrument: piano/acoustic_grand
  events:
    C4 h. mf

@track keys | instrument: piano/acoustic_grand
  clip: long [1:1 - 1:3] loop: 1q
"#;
    let project = project(text);
    let events = try_interpret(&project, &settings()).unwrap();
    let offs: Vec<u64> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOff { note: 60, .. } => Some(event.time_samples),
            _ => None,
        })
        .collect();
    // A three-beat note inside a two-beat clip stops at the clip end, twice.
    assert_eq!(offs, vec![48_000, 48_000]);
}

#[test]
fn shifted_loop_hits_keep_positive_gates_inside_the_final_clip_edge() {
    let project = project(
        r#"@project "Shifted hits"
  tempo: 120
  time_signature: 4/4

@pattern beat | instrument: drums/acoustic_kit
  hits:
    kick 1:2 mf midi_note=36 timing_offset_beats=-0.125 gate_quarters=1.75
    snare 1:3 mf midi_note=38 timing_offset_beats=0.125
    kick 3:1 mf

@track drums | instrument: drums/acoustic_kit
  clip: beat [1:1 - 1:3] offset: 0.5q loop: 1.625q
"#,
    );
    let events = try_interpret(&project, &settings()).unwrap();
    let gates: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, .. } => Some((event.time_samples, note, true)),
            MidiMessage::NoteOff { note, .. } => Some((event.time_samples, note, false)),
            _ => None,
        })
        .collect();
    assert_eq!(
        gates,
        [
            (0, 36, true), // Early pickup stays at the clip head.
            (9_000, 38, true),
            (12_000, 38, false),
            (36_000, 36, true),
            (42_000, 36, false), // First gate crosses the internal seam.
            (48_000, 36, false), // Final gate stops at the placement edge.
        ]
    );
}
