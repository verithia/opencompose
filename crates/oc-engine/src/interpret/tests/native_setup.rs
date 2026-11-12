//! MIDI export setup and native score setup have explicit owning policies.
use super::super::*;
fn project(source: &str, plugin: &str) -> Project {
    oc_dsl::parse(&format!("@project\n@preset voice\n  source: {source}\n  plugin: \"{plugin}\"\n@track lead | preset: voice, instrument: synth/lead\n  automation: sustain {{ 1:1:0=true, 1:2:0=false }}\n  @phrase held [1:1 - 2:1]\n    C4 q mf\n")).unwrap()
}
#[test]
fn native_vst3_score_suppresses_only_implicit_setup_while_export_keeps_it() {
    for (source, plugin) in [("vst3", "controlled.component"), ("sample", "sfizz")] {
        let project = project(source, plugin);
        let settings = InterpretSettings::default();
        let exported = try_interpret_at(&project, &settings, None).unwrap();
        for message in [
            MidiMessage::ProgramChange {
                channel: 0,
                program: 0,
            },
            MidiMessage::ControlChange {
                channel: 0,
                controller: 7,
                value: db_to_midi_volume(0.0),
            },
            MidiMessage::ControlChange {
                channel: 0,
                controller: 10,
                value: pan_to_midi(0.0),
            },
        ] {
            assert!(
                exported
                    .iter()
                    .any(|e| e.time_samples == 0 && e.event == message)
            );
        }
        assert!(
            exported
                .iter()
                .any(|e| matches!(e.event, MidiMessage::ControlChange { controller: 7, .. }))
        );
        assert!(
            exported
                .iter()
                .any(|e| matches!(e.event, MidiMessage::ControlChange { controller: 10, .. }))
        );
        let native = try_interpret_render_at(&project, &settings, None).unwrap();
        assert!(!native.iter().any(|e| matches!(
            e.event,
            MidiMessage::ProgramChange { .. }
                | MidiMessage::ControlChange {
                    controller: 7 | 10,
                    ..
                }
        )));
        assert!(
            native.iter().any(|e| matches!(
                e.event,
                MidiMessage::ControlChange {
                    controller: 64,
                    value: 127,
                    ..
                }
            )),
            "authored sustain survives native setup policy"
        );
        assert!(
            native
                .iter()
                .any(|e| matches!(e.event, MidiMessage::NoteOn { .. }))
        );
    }
}
#[test]
fn explicit_program_survives_native_policy_and_other_backend_setup_is_unchanged() {
    let mut project = project("vst3", "controlled.component");
    project.presets[0].program = Some(12);
    let events = try_interpret_render_at(&project, &InterpretSettings::default(), None).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.event, MidiMessage::ProgramChange { program: 12, .. }))
    );
    project.presets[0].source = Some("clap".into());
    assert_eq!(
        try_interpret_render_at(&project, &InterpretSettings::default(), None).unwrap(),
        try_interpret_at(&project, &InterpretSettings::default(), None).unwrap()
    );
}
