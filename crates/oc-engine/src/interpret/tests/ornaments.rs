//! Ornament marks expand into ordinary notes and gates.

use super::super::*;

fn settings() -> InterpretSettings {
    InterpretSettings {
        sample_rate: 44100,
        ..Default::default()
    }
}

fn note_ons(events: &[MidiEvent]) -> Vec<(u64, u8)> {
    events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, .. } => Some((event.time_samples, note)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_trill_alternates_with_the_next_scale_step_and_closes_on_the_written_pitch() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  key: C major\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf ornament=trill\n",
    )
    .unwrap();
    let events = try_interpret(&project, &settings()).unwrap();
    assert_eq!(note_ons(&events), vec![(0, 60), (7350, 62), (14700, 60)]);
}

#[test]
fn a_mordent_holds_the_written_pitch_after_the_upper_step() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  key: C major\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    E4 q mf ornament=mordent\n",
    )
    .unwrap();
    let events = try_interpret(&project, &settings()).unwrap();
    let ons = note_ons(&events);
    assert_eq!(
        ons.iter().map(|(_, note)| *note).collect::<Vec<_>>(),
        vec![64, 65, 64]
    );
    assert_eq!(ons[1].0, 5512);
    assert_eq!(ons[2].0, 11025);
    let last_off = events
        .iter()
        .filter(|event| matches!(event.event, MidiMessage::NoteOff { note: 64, .. }))
        .map(|event| event.time_samples)
        .max()
        .unwrap();
    assert_eq!(last_off, 22050);
}

#[test]
fn a_fermata_lengthens_the_gate_by_half() {
    let plain = oc_dsl::parse(
        "@project\n  tempo: 120\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf\n",
    )
    .unwrap();
    let held = oc_dsl::parse(
        "@project\n  tempo: 120\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf ornament=fermata\n",
    )
    .unwrap();
    let plain_off = try_interpret(&plain, &settings())
        .unwrap()
        .into_iter()
        .find(|event| matches!(event.event, MidiMessage::NoteOff { note: 60, .. }))
        .unwrap()
        .time_samples;
    let held_off = try_interpret(&held, &settings())
        .unwrap()
        .into_iter()
        .find(|event| matches!(event.event, MidiMessage::NoteOff { note: 60, .. }))
        .unwrap()
        .time_samples;
    assert_eq!(plain_off, 22050);
    assert_eq!(held_off, 33075);
}

#[test]
fn an_arpeggio_rolls_a_phrase_chord_in_pitch_order() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  @phrase line [1:1 - 2:1]\n    C4 q mf ornament=arpeggio, E4 q mf ornament=arpeggio, G4 q mf ornament=arpeggio\n",
    )
    .unwrap();
    let events = try_interpret(&project, &settings()).unwrap();
    assert_eq!(note_ons(&events), vec![(0, 60), (2756, 64), (5512, 67)]);
}

#[test]
fn an_arpeggio_rolls_a_placed_pattern_chord_the_same_way() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@pattern roll | instrument: piano/acoustic_grand\n  events:\n    @0 C4 q mf ornament=arpeggio, E4 q mf ornament=arpeggio, G4 q mf ornament=arpeggio\n@track keys | instrument: piano/acoustic_grand\n  clip: roll [1:1 - 2:1]\n",
    )
    .unwrap();
    let events = try_interpret(&project, &settings()).unwrap();
    assert_eq!(note_ons(&events), vec![(0, 60), (2756, 64), (5512, 67)]);
}

#[test]
fn an_unknown_ornament_is_refused_at_the_source() {
    let error = oc_dsl::parse(
        "@project\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf ornament=roll\n",
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("ornament=roll is not an ornament"),
        "{error}"
    );
}

#[test]
fn a_trill_cannot_share_a_note_with_an_articulation() {
    let error = oc_dsl::parse(
        "@project\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf articulation=staccato ornament=trill\n",
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("cannot share the note with articulation="),
        "{error}"
    );
}

#[test]
fn a_chord_cannot_carry_a_trill_on_one_member() {
    let error = oc_dsl::parse(
        "@project\n  tempo: 120\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    @0 C4 q mf ornament=trill, E4 q mf ornament=trill\n",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("needs a line of its own"), "{error}");
}

#[test]
fn fmt_keeps_the_mark_and_midi_export_writes_the_expansion() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  key: C major\n@track lead | instrument: synth/lead_saw\n  @phrase line [1:1 - 2:1]\n    C4 q mf ornament=trill\n",
    )
    .unwrap();
    let text = oc_dsl::serialize(&project);
    assert!(text.contains("ornament=trill"), "{text}");

    let export = crate::midi::export_smf(&project, &crate::midi::ExportOptions::default()).unwrap();
    let smf = midly::Smf::parse(&export.bytes).unwrap();
    let mut notes = Vec::new();
    for event in smf.tracks.iter().flatten() {
        if let midly::TrackEventKind::Midi {
            message: midly::MidiMessage::NoteOn { key, vel },
            ..
        } = event.kind
            && vel.as_int() > 0
        {
            notes.push(key.as_int());
        }
    }
    assert_eq!(notes, vec![60, 62, 60]);
}
