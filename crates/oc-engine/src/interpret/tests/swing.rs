//! Swing timing tests: written positions stay intact, onsets move in samples.

use super::super::*;

const STRAIGHT_EIGHTHS: &str = r#"@project "swing"
  tempo: 120
  time_signature: 4/4
  swing: 66%

@pattern straight | instrument: synth/lead_square
  events:
    C4 8th mf
    D4 8th mf
    E4 8th mf
    F4 8th mf
    G4 8th mf
    A4 8th mf
    B4 8th mf
    C5 8th mf

@track lead | instrument: synth/lead_square
  clip: straight [1:1 - 2:1]
"#;

fn onsets(project: &Project) -> Vec<u64> {
    try_interpret(
        project,
        &InterpretSettings {
            sample_rate: 48_000,
            ..Default::default()
        },
    )
    .unwrap()
    .into_iter()
    .filter_map(|event| match event.event {
        MidiMessage::NoteOn { .. } => Some(event.time_samples),
        _ => None,
    })
    .collect()
}

#[test]
fn swing_66_delays_only_off_beat_eighths_by_16_percent_of_a_quarter() {
    let project = oc_dsl::parse(STRAIGHT_EIGHTHS).unwrap();
    // 120 BPM at 48 kHz: a quarter is 24000 samples, an 8th 12000, the
    // 66% off-beat delay (0.16 quarter) 3840.
    let expected = [0, 15840, 24000, 39840, 48000, 63840, 72000, 87840];
    assert_eq!(onsets(&project), expected);
    let written: Vec<f64> = project.patterns[0]
        .events
        .iter()
        .map(|event| event.start_beat)
        .collect();
    assert_eq!(written, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
}

#[test]
fn pattern_swing_overrides_project_swing() {
    let mut project = oc_dsl::parse(STRAIGHT_EIGHTHS).unwrap();
    project.patterns[0].swing = Some(50.0);
    assert_eq!(
        onsets(&project),
        [0, 12000, 24000, 36000, 48000, 60000, 72000, 84000]
    );

    project.properties.swing = Some(50.0);
    project.patterns[0].swing = Some(66.0);
    assert_eq!(
        onsets(&project),
        [0, 15840, 24000, 39840, 48000, 63840, 72000, 87840]
    );
}

#[test]
fn swing_percentage_endpoints_map_onto_adjacent_beats_inside_the_clip() {
    let mut project = oc_dsl::parse(STRAIGHT_EIGHTHS).unwrap();
    project.properties.swing = Some(100.0);
    assert_eq!(
        onsets(&project),
        [0, 24000, 24000, 48000, 48000, 72000, 72000]
    );
    project.properties.swing = Some(0.0);
    assert_eq!(
        onsets(&project),
        [0, 0, 24000, 24000, 48000, 48000, 72000, 72000]
    );
}

#[test]
fn swing_can_cross_an_internal_loop_seam_but_not_the_clip_end() {
    let mut project = oc_dsl::parse(STRAIGHT_EIGHTHS).unwrap();
    project.properties.swing = Some(100.0);
    project.tracks[0].pattern_clip_mut(0).unwrap().loop_quarters = 1.0;
    assert_eq!(
        onsets(&project),
        [0, 24000, 24000, 48000, 48000, 72000, 72000]
    );
}

#[test]
fn swing_also_offsets_drum_hits_on_the_off_beat() {
    let project = oc_dsl::parse(
        r#"@project "swing drums"
  tempo: 120
  time_signature: 4/4
  swing: 66%

@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1 - 2:1]
    kick 1:0 mf
    hihat 1:2 mf
"#,
    )
    .unwrap();
    assert_eq!(onsets(&project), [0, 15840]);
}

#[test]
fn advanced_drum_hit_at_song_head_has_a_positive_gate() {
    let project = oc_dsl::parse(
        r#"@project "swing drums at song head"
  tempo: 120
  swing: 0%

@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1 - 2:1]
    kick 1:2 mf timing_offset_beats=-0.125 gate_quarters=0.000001
"#,
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    let events = try_interpret(
        &project,
        &InterpretSettings {
            sample_rate: 48_000,
            ..Default::default()
        },
    )
    .unwrap();
    let gates: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { .. } => Some((event.time_samples, true)),
            MidiMessage::NoteOff { .. } => Some((event.time_samples, false)),
            _ => None,
        })
        .collect();
    assert_eq!(gates, [(0, true), (1, false)]);
}

/// The documented limit: swing moves off-beat eighths, so a sixteenth grid
/// keeps every written position until a subdivision setting exists.
#[test]
fn swing_leaves_sixteenth_notes_on_their_written_grid() {
    let project = oc_dsl::parse(
        r#"@project "swing"
  tempo: 120
  time_signature: 4/4
  swing: 66%

@pattern sixteenths | instrument: synth/lead_square
  events:
    C4 16th mf
    D4 16th mf
    E4 16th mf
    F4 16th mf

@track lead | instrument: synth/lead_square
  clip: sixteenths [1:1 - 2:1]
"#,
    )
    .unwrap();
    // 120 BPM at 48 kHz: a sixteenth is 6000 samples, an eighth 12000. The
    // off-beat eighth at 12000 moves by 16 % of a quarter; the sixteenths at
    // 6000 and 18000 stay where they were written.
    assert_eq!(onsets(&project), [0, 6000, 15840, 18000]);
}

/// A 16th subdivision delays every second sixteenth by half an eighth's
/// amount, and leaves the off-beat eighth an even 16th alone.
#[test]
fn a_sixteenth_subdivision_swings_every_second_sixteenth() {
    let project = oc_dsl::parse(
        r#"@project "swing"
  tempo: 120
  time_signature: 4/4
  swing: 55%
  swing_subdivision: 16th

@pattern sixteenths | instrument: synth/lead_square
  events:
    C4 16th mf
    D4 16th mf
    E4 16th mf
    F4 16th mf

@track lead | instrument: synth/lead_square
  clip: sixteenths [1:1 - 2:1]
"#,
    )
    .unwrap();
    // 120 BPM at 48 kHz: a quarter is 24000 samples, a 16th 6000. At 55 % the
    // off-beat 16th moves by 0.025 quarter = 600 samples; the off-beat eighth
    // at 12000 is an even 16th and stays.
    assert_eq!(onsets(&project), [0, 6600, 12000, 18600]);
}
