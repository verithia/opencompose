//! Mapped interpreter tests.

use super::super::*;

#[test]
fn mapped_notes_controls_and_physical_profile_offsets_share_one_clock() {
    let project = oc_dsl::parse(r#"@project
  tempo: 120
  tempo_map:
1:3 = 60 bpm
@preset expressive
  source: clap
  plugin: "example.piano"
  params: {keyswitch_duration_ms: 10, articulation_legato_keyswitch: "C0", articulation_legato_overlap_ms: 20, articulation_legato_release_note: "D0", articulation_legato_release_velocity: 12, articulation_legato_release_duration_ms: 30, articulation_legato_release_sustain_aware: true}
@track piano | instrument: piano/acoustic_grand, preset: expressive
  automation: sustain {1:1=true, 1:4=false}
  @phrase main [1:2 - 2:1]
C4 q mf articulation=legato expression={dynamics:mp->ff}
"#).unwrap();
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    let events = try_interpret(&project, &settings).unwrap();
    assert!(events.iter().any(
        |e| e.time_samples == 24000 && matches!(e.event, MidiMessage::NoteOn { note: 60, .. })
    ));
    // Legato adds 5% musical duration, then 20ms physical overlap.
    assert!(events.iter().any(|e| e.time_samples.abs_diff(51360) <= 1
        && matches!(e.event, MidiMessage::NoteOff { note: 60, .. })));
    assert!(events.iter().any(
        |e| e.time_samples == 23520 && matches!(e.event, MidiMessage::NoteOn { note: 12, .. })
    ));
    assert!(events.iter().any(|e| e.time_samples == 96000
        && matches!(
            e.event,
            MidiMessage::ControlChange {
                controller: 64,
                value: 0,
                ..
            }
        )));
    assert!(events.iter().any(|e| e.time_samples == 96000
        && matches!(
            e.event,
            MidiMessage::NoteOn {
                note: 14,
                velocity: 12,
                ..
            }
        )));
    assert!(events.iter().any(
        |e| e.time_samples == 97440 && matches!(e.event, MidiMessage::NoteOff { note: 14, .. })
    ));
    assert!(events.iter().any(|e| e.time_samples.abs_diff(50400) <= 1
        && matches!(e.event, MidiMessage::ControlChange { controller: 11, .. })));
}

#[test]
fn mapped_clip_and_repeated_drums_follow_meter_changes() {
    let mut project = oc_dsl::parse(
        r#"@project
  tempo: 120
  time_signature: 7/8
  time_sig_map:
3:1 = 3/4
@track piano | instrument: piano/acoustic_grand
  @phrase main [2:1 - 3:1]
C4 q mf
@track drums | instrument: drums/acoustic_kit
  @pattern main [2:1 - 5:1]
kick 1:0 mf
"#,
    )
    .unwrap();
    project.tracks[1].patterns[0].repeat = 3;
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    let original = try_interpret(&project, &settings).unwrap();
    let drum_times: Vec<_> = original
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NoteOn { channel: 9, .. } => Some(e.time_samples),
            _ => None,
        })
        .collect();
    assert_eq!(drum_times, vec![84000, 168000, 240000]);
    // Build a clip from the same phrase; placement must use the same meter.
    let phrase = project.tracks[0].phrases.remove(0);
    let pattern = PatternDef {
        name: "notes".into(),
        instrument: project.tracks[0].instrument.clone(),
        engine: None,
        preset: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        color: None,
        events: phrase.events,
        pedals: vec![],
        hairpins: vec![],
        hits: vec![],
    };
    project.patterns.push(pattern);
    project.tracks[0].push_pattern_clip(Clip {
        pattern: "notes".into(),
        time_range: phrase.time_range.unwrap(),
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    assert_eq!(try_interpret(&project, &settings).unwrap(), original);
}

#[test]
fn mapped_vibrato_retains_hertz_and_delay_seconds() {
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
  tempo_map:
1:3 = 60 bpm
@track piano | instrument: piano/acoustic_grand
  @phrase main [1:2 - 2:1]
C4 h mf vibrato={rate:5Hz,depth:20ct,delay:0.2s}
"#,
    )
    .unwrap();
    let events = try_interpret(
        &project,
        &InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    let bends: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::PitchBend { value, .. } if value != 0 => Some(e.time_samples),
            _ => None,
        })
        .collect();
    assert_eq!(bends[0], 33600);
    assert!(bends.last().unwrap() > &90000);
    assert!(
        bends
            .windows(2)
            .all(|pair| (pair[1] - pair[0]).abs_diff(2400) <= 1)
    );
}

#[test]
fn checked_interpretation_rejects_bad_clock_and_positions() {
    let mut project = oc_dsl::parse("@project\n@track piano | instrument: piano/acoustic_grand\n  @phrase main [1:1 - 2:1]\n    C4 q mf\n").unwrap();
    let settings = InterpretSettings::default();
    project.tracks[0].phrases[0]
        .time_range
        .as_mut()
        .unwrap()
        .start
        .bar = 0;
    assert!(try_interpret(&project, &settings).is_err());
    project.tracks[0].phrases[0]
        .time_range
        .as_mut()
        .unwrap()
        .start
        .bar = 1;
    project.properties.tempo = f64::NAN;
    assert!(try_interpret(&project, &settings).is_err());
    project.properties.tempo = 120.0;
    assert!(
        try_interpret(
            &project,
            &InterpretSettings {
                sample_rate: 0,
                ..settings
            }
        )
        .is_err()
    );
}

#[test]
fn test_interpret_simple() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track piano | instrument: piano/acoustic_grand
  @phrase intro [1:1:0 - 2:4:0]
C4 q mf
E4 q mf
G4 h f
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);

    // Should have: 1 program change + 1 volume CC + 1 pan CC + 3 note-on + 3 note-off = 9
    assert_eq!(
        events.len(),
        9,
        "Expected 9 events, got {}: {:?}",
        events.len(),
        events
    );

    // First event should be program change at time 0
    assert!(matches!(
        events[0].event,
        MidiMessage::ProgramChange { program: 0, .. }
    ));

    // Find note-on events
    let note_ons: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.event, MidiMessage::NoteOn { .. }))
        .collect();
    assert_eq!(note_ons.len(), 3);

    // First note should be C4 (MIDI 60)
    if let MidiMessage::NoteOn { note, velocity, .. } = &note_ons[0].event {
        assert_eq!(*note, 60); // C4
        assert_eq!(*velocity, 80); // mf
    }
}
