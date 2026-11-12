use oc_engine::interpret::{InterpretSettings, MidiMessage, interpret};

#[test]
fn explicit_return_does_not_play_at_zero_and_muted_notes_stay_silent() {
    let mut p = oc_dsl::parse(
        r#"@project "Return placement"
  tempo: 120
  time_signature: 4/4
@track keys | instrument: piano/acoustic_grand
  @phrase original [1:1 - 2:1]
    C4 q mf v=80
    G4 q mf v=80
"#,
    )
    .unwrap();
    let mut variant = p.tracks[0].phrases[0].clone();
    variant.name = "response".into();
    variant.time_range.as_mut().unwrap().start.bar = 5;
    variant.time_range.as_mut().unwrap().end.bar = 6;
    variant.events[0].velocity = Some(65);
    variant.events[1].muted = true;
    p.tracks[0].phrases.push(variant);
    let settings = InterpretSettings {
        sample_rate: 48000,
        ..Default::default()
    };
    let notes: Vec<_> = interpret(&p, &settings)
        .into_iter()
        .filter_map(|e| match e.event {
            MidiMessage::NoteOn { velocity, .. } => Some((e.time_samples, velocity)),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec![(0, 80), (24000, 80), (384000, 65)]);
}
