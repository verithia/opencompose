//! Phrases interpreter tests.

use super::super::*;

#[test]
fn phrase_and_clip_have_identical_automation_tie_order() {
    let source = "@project\n  tempo: 120\n@track lead | instrument: piano/acoustic_grand\n  @phrase material [1:1 - 3:1]\n    @0 C4 h mf\n    @2 E4 h mf\n    @4 G4 h mf\n  automation: dynamics {1:1=0.2, 1:3=0.8, 2:1=0.4}\n";
    let phrase_project = oc_dsl::parse(source).unwrap();
    let mut clip_project = phrase_project.clone();
    let phrase = clip_project.tracks[0].phrases.remove(0);
    clip_project.patterns.push(PatternDef {
        name: "material".into(),
        instrument: clip_project.tracks[0].instrument.clone(),
        preset: None,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: phrase.events,
        pedals: vec![],
        hairpins: Vec::new(),
        hits: vec![],
        color: None,
    });
    clip_project.tracks[0].push_pattern_clip(Clip {
        pattern: "material".into(),
        time_range: phrase.time_range.unwrap(),
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    assert_eq!(
        try_interpret(&phrase_project, &settings).unwrap(),
        try_interpret(&clip_project, &settings).unwrap()
    );
}

#[test]
fn unqualified_performance_playback_rejects_instead_of_silence() {
    let project = oc_dsl::parse("@project\n@track t | instrument: piano/acoustic_grand\n  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 37; }\n").unwrap();
    oc_dsl::validate(&project).unwrap();
    assert!(
        try_interpret(&project, &InterpretSettings::default())
            .unwrap_err()
            .contains("performance timeline playback")
    );
}

#[test]
fn explicit_gate_roundtrips_and_integrates_tempo() {
    let text = "@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n@track drums | instrument: drums/acoustic_kit\n  @pattern main [1:1 - 2:1]\n    kick 1:0 mf gate_quarters=1.125\n";
    let p = oc_dsl::parse(text).unwrap();
    let mut p = oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap();
    oc_dsl::validate(&p).unwrap();
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    let events = try_interpret(&p, &settings).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.time_samples == 30000 && matches!(e.event, MidiMessage::NoteOff { .. }))
    );
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        p.tracks[0].patterns[0].hits[0].modifiers[0].1 = ParamValue::Number(value);
        assert!(oc_dsl::validate(&p).is_err());
        assert!(try_interpret(&p, &settings).is_err());
    }
    let duplicate = vec![("gate_quarters".into(), ParamValue::Number(1.0)); 2];
    assert!(performance_gate_quarters(&duplicate).is_err());
    assert_eq!(
        performance_gate_quarters(&[("duration_beats".into(), ParamValue::Number(9.0))]).unwrap(),
        None
    );
}

#[test]
fn explicit_drum_microtiming_integrates_before_tempo_conversion() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n@track drums | instrument: drums/acoustic_kit\n  @pattern main [1:1 - 2:1]\n    kick 2:0 mf timing_offset_beats=-0.0625\n    snare 3:0 mf timing_offset_beats=0.0625\n").unwrap();
    let text = oc_dsl::serialize(&project);
    let mut project = oc_dsl::parse(&text).unwrap();
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    let times = |p: &Project| {
        try_interpret(p, &settings)
            .unwrap()
            .into_iter()
            .filter_map(|e| matches!(e.event, MidiMessage::NoteOn { .. }).then_some(e.time_samples))
            .collect::<Vec<_>>()
    };
    oc_dsl::validate(&project).unwrap();
    assert_eq!(times(&project), vec![22500, 51000]);
    let hits = project.tracks[0].patterns.remove(0).hits;
    project.patterns.push(PatternDef {
        name: "named".into(),
        instrument: InstrumentPath::new("drums", "acoustic_kit"),
        preset: None,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        hits,
        color: None,
    });
    project.tracks[0].push_pattern_clip(Clip {
        pattern: "named".into(),
        time_range: TimeRange {
            start: BarBeat::new(1, 1, 0),
            end: BarBeat::new(2, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    oc_dsl::validate(&project).unwrap();
    assert_eq!(times(&project), vec![22500, 51000]);
    project.patterns[0].hits[0]
        .modifiers
        .push(("timing_offset_beats".into(), ParamValue::Number(0.0)));
    assert!(oc_dsl::validate(&project).is_err());
    assert!(try_interpret(&project, &settings).is_err());
}

#[test]
fn drum_explicit_pitch_and_velocity_survive_interpretation() {
    let mut project = oc_dsl::parse(
        r#"@project "Drum fidelity"
  tempo: 120
  time_signature: 4/4
@track drums | instrument: drums/acoustic_kit
  @pattern main [1:1 - 2:1]
hihat 1:0 mf midi_note=46 velocity=37
kick 2:0 mf midi_note=35 velocity=113
kick 3:0 mf
"#,
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    let notes: Vec<_> = interpret(&project, &InterpretSettings::default())
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, velocity, .. } => Some((note, velocity)),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec![(46, 37), (35, 113), (36, 80)]);
    // Named patterns placed as clips must obey the same explicit fields.
    let hits = project.tracks[0].patterns.remove(0).hits;
    project.patterns.push(PatternDef {
        name: "named".into(),
        instrument: InstrumentPath::new("drums", "acoustic_kit"),
        preset: None,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        hits,
        color: None,
    });
    project.tracks[0].push_pattern_clip(Clip {
        pattern: "named".into(),
        time_range: TimeRange {
            start: BarBeat::new(1, 1, 0),
            end: BarBeat::new(2, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    oc_dsl::validate(&project).unwrap();
    let clip_notes: Vec<_> = interpret(&project, &InterpretSettings::default())
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, velocity, .. } => Some((note, velocity)),
            _ => None,
        })
        .collect();
    assert_eq!(clip_notes, notes);
}

#[test]
fn phrase_explicit_velocity_overrides_coarse_dynamics() {
    let project = oc_dsl::parse(
        r#"@project "Velocity fidelity"
  tempo: 120
  time_signature: 4/4
@track piano | instrument: piano/acoustic_grand
  @phrase main [1:1:0 - 2:1:0]
C4 q mf v=37
D4 q mf v=113
E4 q mf
"#,
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    let velocities: Vec<_> = interpret(&project, &InterpretSettings::default())
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { velocity, .. } => Some(velocity),
            _ => None,
        })
        .collect();
    assert_eq!(velocities, vec![37, 113, 80]);
}

#[test]
fn a_late_pattern_pedal_lifts_after_the_last_emitted_press() {
    let project = oc_dsl::parse(
        r#"@project "Pedal boundary"
  tempo: 120
  time_signature: 4/4

@pattern clipped | instrument: piano/acoustic_grand
  events:
    C4 h mf articulation=staccato
    @1.5 pedal down
    @2 pedal up

@track piano | instrument: piano/acoustic_grand
  clip: clipped [1:1 - 2:1] loop: 2q
  @phrase next [2:1 - 3:1]
    G4 q mf
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

    let pedal: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::ControlChange {
                controller: 64,
                value,
                ..
            } => Some((event.time_samples, value)),
            _ => None,
        })
        .collect();
    // Both written lifts are at the excluded source boundary. The fallback
    // must run after the second press, even though the last C4 already ended.
    assert_eq!(pedal, [(36_000, 127), (84_000, 127), (84_000, 0)]);
    assert!(events.iter().any(|event| {
        matches!(event.event, MidiMessage::NoteOff { note: 60, .. }) && event.time_samples == 72_000
    }));
}

#[test]
fn a_swung_pattern_pedal_press_stays_inside_its_placement() {
    let project = oc_dsl::parse(
        r#"@project "Swung pedal"
  tempo: 120
  time_signature: 4/4
  swing: 90%

@pattern clipped | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @0.5 pedal down

@track piano | instrument: piano/acoustic_grand
  clip: clipped [1:1 - 1:1:3]
"#,
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    let events = try_interpret(&project, &InterpretSettings::default()).unwrap();
    assert!(
        events
            .iter()
            .any(|event| { matches!(event.event, MidiMessage::NoteOn { note: 60, .. }) })
    );
    assert!(!events.iter().any(|event| matches!(
        event.event,
        MidiMessage::ControlChange { controller: 64, .. }
    )));
}

#[test]
fn loop_trim_ends_channel_pitch_bend_at_the_actual_note_off() {
    let project = oc_dsl::parse(
        r#"@project "Trimmed pitch bend"
  tempo: 120
  time_signature: 4/4

@pattern bent | instrument: synth/lead_square
  events:
    @0.5 C4 q mf bend=100ct

@track lead | instrument: synth/lead_square
  clip: bent [1:1 - 1:2:3] loop: 1q
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
    let bend_state = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::PitchBend { value, .. } => Some((event.time_samples, value != 0)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        bend_state,
        [
            (12_000, true),
            (36_000, false),
            (36_000, true),
            (42_000, false)
        ]
    );
    assert!(events.iter().any(|event| {
        event.time_samples == 42_000 && matches!(event.event, MidiMessage::NoteOff { note: 60, .. })
    }));
}
