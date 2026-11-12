//! Drums interpreter tests.

use super::super::*;

#[test]
fn drum_hihat_articulations_map_to_gm_notes() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1:0 - 1:2:0]
hihat 1:0 mf articulation=closed
hihat 1:1 mf articulation=half_open
hihat 1:2 mf articulation=open
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let notes: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect();

    assert!(notes.contains(&42));
    assert!(notes.contains(&44));
    assert!(notes.contains(&46));
}

#[test]
fn preset_profile_overrides_drum_notes() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset custom_kit
  source: clap
  plugin: "example.drums"
  params: {drum_kick_note: 24, drum_hihat_open_note: 26}

@track drums | instrument: drums/acoustic_kit, preset: custom_kit
  @pattern beat [1:1:0 - 1:2:0]
kick 1:0 mf
hihat 1:1 mf articulation=open
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let notes: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect();

    assert!(notes.contains(&24));
    assert!(notes.contains(&26));
    assert!(!notes.contains(&36));
    assert!(!notes.contains(&46));
}

#[test]
fn preset_profile_chokes_drum_group_before_next_hit() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset custom_kit
  source: clap
  plugin: "example.drums"
  params: {drum_hihat_open_choke_group: "hihat", drum_hihat_closed_choke_group: "hihat"}

@track drums | instrument: drums/acoustic_kit, preset: custom_kit
  @pattern beat [1:1:0 - 1:2:0]
hihat 1:0 mf articulation=open
hihat 1:1 mf articulation=closed
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let closed_time = beats_to_samples(0.25, 120.0, settings.sample_rate);

    let same_time_events: Vec<_> = events
        .iter()
        .filter(|event| event.time_samples == closed_time)
        .filter(|event| {
            matches!(
                event.event,
                MidiMessage::NoteOff { .. } | MidiMessage::NoteOn { .. }
            )
        })
        .collect();

    assert!(matches!(
        same_time_events[0].event,
        MidiMessage::NoteOff {
            channel: 9,
            note: 46
        }
    ));
    assert!(matches!(
        same_time_events[1].event,
        MidiMessage::NoteOn {
            channel: 9,
            note: 42,
            ..
        }
    ));
}

#[test]
fn preset_profile_chokes_drum_group_in_time_order() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset custom_kit
  source: clap
  plugin: "example.drums"
  params: {drum_hihat_open_choke_group: "hihat", drum_hihat_closed_choke_group: "hihat"}

@track drums | instrument: drums/acoustic_kit, preset: custom_kit
  @pattern beat [1:1:0 - 1:2:0]
hihat 1:1 mf articulation=closed
hihat 1:0 mf articulation=open
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let closed_time = beats_to_samples(0.25, 120.0, settings.sample_rate);

    assert!(events.iter().any(|event| matches!(
        event.event,
        MidiMessage::NoteOff { channel: 9, note: 46 }
            if event.time_samples == closed_time
    )));
    assert!(!events.iter().any(|event| matches!(
        event.event,
        MidiMessage::NoteOff { channel: 9, note: 42 }
            if event.time_samples == 0
    )));
}

#[test]
fn preset_profile_delays_drum_choke_release() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset custom_kit
  source: clap
  plugin: "example.drums"
  params: {drum_hihat_open_choke_group: "hihat", drum_hihat_closed_choke_group: "hihat", choke_hihat_release_ms: 25}

@track drums | instrument: drums/acoustic_kit, preset: custom_kit
  @pattern beat [1:1:0 - 1:2:0]
hihat 1:0 mf articulation=open
hihat 1:1 mf articulation=closed
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let closed_time = beats_to_samples(0.25, 120.0, settings.sample_rate);
    let release_samples = (settings.sample_rate as f64 * 0.025) as u64;

    assert!(events.iter().any(|event| matches!(
        event.event,
        MidiMessage::NoteOff { channel: 9, note: 46 }
            if event.time_samples == closed_time + release_samples
    )));
    assert!(!events.iter().any(|event| matches!(
        event.event,
        MidiMessage::NoteOff { channel: 9, note: 46 }
            if event.time_samples == closed_time
    )));
}

fn dump_project(drums: bool) -> Project {
    let track = if drums {
        let hits: String = (1..=4)
            .flat_map(|beat| (0..4).map(move |sub| format!("    kick {beat}:{sub} f\n")))
            .collect();
        format!(
            "@track drums | instrument: drums/acoustic_kit\n  @pattern beat [1:1 - 2:1]\n{hits}"
        )
    } else {
        "@track keys | instrument: keys/piano\n  @phrase lead [1:1 - 2:1]\n    C4 q mf\n"
            .to_string()
    };
    oc_dsl::parse(&format!(
        "@project \"Test\"\n  tempo: 120\n  time_signature: 4/4\n\n{track}"
    ))
    .unwrap()
}

#[test]
fn the_trace_dump_names_the_event_count_and_stops_at_twenty_events() {
    let project = dump_project(true);
    let events = interpret(&project, &InterpretSettings::default());
    let track_events = events.iter().filter(|event| event.track_index == 0).count();
    assert!(track_events > 20, "the fixture needs more than 20 events");
    let dump = drum_events_dump(&events, &project).expect("a drum track dumps its events");
    let mut lines = dump.lines();
    let header = format!("[interpret] drum track events: {track_events}");
    assert_eq!(lines.next(), Some(header.as_str()), "{dump}");
    assert_eq!(lines.count(), 20, "the body is capped at 20 events: {dump}");
}

#[test]
fn a_project_without_a_drum_track_has_no_trace_dump() {
    let project = dump_project(false);
    let events = interpret(&project, &InterpretSettings::default());
    assert!(
        drum_events_dump(&events, &project).is_none(),
        "only a drum track has a dump"
    );
}
