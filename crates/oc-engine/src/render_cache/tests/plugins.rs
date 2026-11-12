//! Shared plugin group and tail-budget tests.

use super::super::*;
use super::*;

#[test]
fn reused_plugin_preset_does_not_merge_independent_tracks() {
    let mut project = shared_plugin_project();
    project.presets[0].params.clear();
    for track in &mut project.tracks {
        track.engine_params = None;
    }
    assert_eq!(
        group_tracks_by_preset(&project, &[0, 1]),
        vec![vec![0], vec![1]]
    );
    let mut dirty = vec![false, true];
    propagate_shared_plugin_dirty(&project, &mut dirty);
    assert_eq!(dirty, vec![false, true]);
}

#[test]
fn shared_plugin_groups_invalidate_together_and_keep_track_pair_overrides() {
    let project = shared_plugin_project();
    let mut dirty = vec![false, true];

    propagate_shared_plugin_dirty(&project, &mut dirty);

    assert_eq!(dirty, vec![true, true]);
    assert_eq!(group_tracks_by_preset(&project, &[0, 1]), vec![vec![0, 1]]);
    assert_eq!(
        instrument_profile_for_track(&project, 0, 48_000).output_pair(),
        0
    );
    assert_eq!(
        instrument_profile_for_track(&project, 1, 48_000).output_pair(),
        2
    );
}

#[test]
fn selecting_one_track_of_a_shared_output_group_selects_the_group() {
    let project = shared_plugin_project();
    for selected in [vec![0], vec![1]] {
        let mut expanded = selected.clone();
        expand_shared_output_groups(&project, &mut expanded);
        assert_eq!(
            expanded,
            vec![0, 1],
            "{selected:?} must expand to the group"
        );
    }

    // Without declared output routing the tracks stay independent.
    let mut independent = shared_plugin_project();
    independent.presets[0].params.clear();
    for track in &mut independent.tracks {
        track.engine_params = None;
    }
    let mut selected = vec![0];
    expand_shared_output_groups(&independent, &mut selected);
    assert_eq!(selected, vec![0]);
}

#[test]
fn declared_tail_budget_caps_infinite_plugins_and_preserves_finite_tails() {
    assert_eq!(bounded_tail_samples(12_345, 48_000), 12_345);
    assert_eq!(bounded_tail_samples(u32::MAX, 48_000), 30_u64 * 48_000);
}

impl PluginMidiEventSink for TestSink {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.0
            .push(QueuedEvent::NoteOn(channel, note, velocity, sample_offset));
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.0
            .push(QueuedEvent::NoteOff(channel, note, sample_offset));
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.0.push(QueuedEvent::ControlChange(
            channel,
            controller,
            value,
            sample_offset,
        ));
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.0
            .push(QueuedEvent::ProgramChange(channel, program, sample_offset));
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.0
            .push(QueuedEvent::PitchBend(channel, value, sample_offset));
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.0.push(QueuedEvent::ChannelPressure(
            channel,
            pressure,
            sample_offset,
        ));
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.0.push(QueuedEvent::PolyPressure(
            channel,
            note,
            pressure,
            sample_offset,
        ));
    }

    fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    ) {
        self.0.push(QueuedEvent::NoteExpression(
            channel,
            note,
            expression.to_string(),
            value,
            sample_offset,
        ));
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        self.0.push(QueuedEvent::PluginParam(
            name.to_string(),
            value,
            sample_offset,
        ));
    }
}

#[test]
fn queue_plugin_midi_event_forwards_non_note_controls() {
    let mut rejected = TestSink::default();
    let event = MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::Off { token: 1 });
    assert!(queue_plugin_midi_event(&mut rejected, &event, 0).is_err());
    assert!(rejected.0.is_empty());
    let mut sink = TestSink::default();

    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::ControlChange {
            channel: 2,
            controller: 74,
            value: 96,
        },
        17,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::ControlChange14 {
            channel: 2,
            controller: 1,
            value: 0x2050,
        },
        18,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::ProgramChange {
            channel: 3,
            program: 41,
        },
        23,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::PitchBend {
            channel: 4,
            value: -1024,
        },
        29,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::PluginParam {
            name: "Expression".to_string(),
            value: 0.5,
        },
        31,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::ChannelPressure {
            channel: 5,
            pressure: 64,
        },
        37,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::PolyPressure {
            channel: 6,
            note: 72,
            pressure: 96,
        },
        41,
    )
    .unwrap();
    queue_plugin_midi_event(
        &mut sink,
        &MidiMessage::NoteExpression {
            channel: 7,
            note: 73,
            expression: "tuning".to_string(),
            value: 0.25,
        },
        43,
    )
    .unwrap();

    assert_eq!(
        sink.0,
        vec![
            QueuedEvent::ControlChange(2, 74, 96, 17),
            QueuedEvent::ControlChange(2, 1, 64, 18),
            QueuedEvent::ControlChange(2, 33, 80, 18),
            QueuedEvent::ProgramChange(3, 41, 23),
            QueuedEvent::PitchBend(4, -1024, 29),
            QueuedEvent::PluginParam("Expression".to_string(), 0.5, 31),
            QueuedEvent::ChannelPressure(5, 64, 37),
            QueuedEvent::PolyPressure(6, 72, 96, 41),
            QueuedEvent::NoteExpression(7, 73, "tuning".to_string(), 0.25, 43),
        ]
    );
}

#[test]
fn queue_vst3_profile_event_maps_configured_cc_to_param() {
    let profile = InstrumentProfile::from_preset_params(
        &[(
            "cc_64_param".to_string(),
            ParamValue::String("Sustain Pedal".to_string()),
        )],
        44_100,
    );
    let mut sink = TestSink::default();

    queue_vst3_profile_event(
        &mut sink,
        &MidiMessage::ControlChange14 {
            channel: 0,
            controller: 64,
            value: 12_288,
        },
        21,
        &profile,
    )
    .unwrap();
    queue_vst3_profile_event(
        &mut sink,
        &MidiMessage::ControlChange {
            channel: 0,
            controller: 64,
            value: 96,
        },
        19,
        &profile,
    )
    .unwrap();
    queue_vst3_profile_event(
        &mut sink,
        &MidiMessage::ControlChange {
            channel: 0,
            controller: 1,
            value: 32,
        },
        23,
        &profile,
    )
    .unwrap();

    assert_eq!(
        sink.0,
        vec![
            QueuedEvent::PluginParam("Sustain Pedal".to_string(), 12288.0 / 16383.0, 21),
            QueuedEvent::PluginParam("Sustain Pedal".to_string(), 96.0 / 127.0, 19),
            QueuedEvent::ControlChange(0, 1, 32, 23),
        ]
    );
}
