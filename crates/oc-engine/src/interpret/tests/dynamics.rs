//! Dynamics agree across notes, automation, MIDI export and normalized controls.
use super::super::*;

#[test]
fn dynamics_tokens_have_one_velocity_across_playback_and_midi_export() {
    for token in ["ppp", "pp", "p", "mp", "mf", "f", "ff", "fff"] {
        let dynamics = Dynamics::from_token(token).unwrap();
        let expected = dynamics.to_velocity();
        let source = format!(
            "@project\n  tempo: 120\n@track piano | instrument: piano/acoustic_grand\n  automation: dynamics {{ 1:1:0={token} }}\n  @phrase part [1:1:0 - 2:1:0]\n    C4 q {token}\n"
        );
        let project = oc_dsl::parse(&source).unwrap();
        let events = interpret(&project, &InterpretSettings::default());
        assert!(
            events.iter().any(|event| matches!(event.event,
            MidiMessage::NoteOn { velocity, .. } if velocity == expected)),
            "{token}"
        );
        assert!(
            events.iter().any(|event| matches!(event.event,
            MidiMessage::ControlChange { controller: 11, value, .. } if value == expected)),
            "{token}"
        );
        let export =
            crate::midi::export_smf(&project, &crate::midi::ExportOptions::default()).unwrap();
        let smf = midly::Smf::parse(&export.bytes).unwrap();
        let mut note = false;
        let mut automation = false;
        for event in smf.tracks.iter().flatten() {
            match event.kind {
                midly::TrackEventKind::Midi {
                    message: midly::MidiMessage::NoteOn { vel, .. },
                    ..
                } => {
                    assert_eq!(vel.as_int(), expected, "{token}");
                    note = true;
                }
                midly::TrackEventKind::Midi {
                    message: midly::MidiMessage::Controller { controller, value },
                    ..
                } if controller.as_int() == 11 => {
                    assert_eq!(value.as_int(), expected, "{token}");
                    automation = true;
                }
                _ => {}
            }
        }
        assert!(note && automation, "{token}");
        let value = ParamValue::String(token.into());
        assert_eq!(
            param_value_to_midi14(&value),
            Some(u16::from(expected) * 129)
        );
        assert_eq!(
            param_value_to_normalized(&value),
            Some(dynamics.to_amplitude())
        );
    }
    assert_eq!(Dynamics::from_token("forte"), None);
    assert_eq!(dynamics_name_to_velocity("unknown"), None);
}

#[test]
fn a_hosted_hairpin_sends_cc11_and_a_builtin_hairpin_keeps_its_gain_clip() {
    let hosted = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset bank\n  source: soundfont\n  program: 0\n@track strings | preset: bank, instrument: strings/violin\n  @phrase swell [1:1 - 4:1]\n    @0.0 cresc\n    @0 C4 w ppp\n    @4 C4 w\n    @8 C4 w fff\n",
    )
    .unwrap();
    let expanded = oc_dsl::dynamics::expand(&hosted).unwrap();
    assert!(
        expanded.tracks[0].gain_automation.is_empty(),
        "a hosted track must not receive the builtin gain clip"
    );
    let events = interpret(&hosted, &InterpretSettings::default());
    let values: Vec<u8> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::ControlChange {
                controller: 11,
                value,
                ..
            } => Some(value),
            _ => None,
        })
        .collect();
    assert!(values.len() > 8, "the swell needs a continuous CC11 ramp");
    assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(values.first().copied(), Some(16));
    assert_eq!(values.last().copied(), Some(127));
    let rise = 20.0 * (1.0_f64 / (16.0_f64 / 127.0).powi(2)).log10();
    assert!(rise >= 12.0, "the swell must span at least 12 dB: {rise}");

    let exported =
        crate::midi::export_smf(&hosted, &crate::midi::ExportOptions::default()).unwrap();
    let smf = midly::Smf::parse(&exported.bytes).unwrap();
    let mut exported_values = Vec::new();
    for event in smf.tracks.iter().flatten() {
        if let midly::TrackEventKind::Midi {
            message: midly::MidiMessage::Controller { controller, value },
            ..
        } = event.kind
            && controller.as_int() == 11
        {
            exported_values.push(value.as_int());
        }
    }
    assert_eq!(
        exported_values, values,
        "MIDI export must carry the same CC11 ramp"
    );

    let builtin = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead_saw\n  @phrase swell [1:1 - 4:1]\n    @0.0 cresc\n    @0 C4 w ppp\n    @4 C4 w\n    @8 C4 w fff\n",
    )
    .unwrap();
    let expanded = oc_dsl::dynamics::expand(&builtin).unwrap();
    assert_eq!(
        expanded.tracks[0].gain_automation.len(),
        1,
        "a builtin synth keeps the post-insert gain ramp"
    );
    let builtin_events = interpret(&builtin, &InterpretSettings::default());
    assert!(
        !builtin_events.iter().any(|event| matches!(
            event.event,
            MidiMessage::ControlChange { controller: 11, .. }
        )),
        "the builtin synth has no CC11 receiver, so the interpreter must not send one"
    );
}
