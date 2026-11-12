use super::*;
use oc_dsl::performance::ChannelCommand;
fn event(sample: u64, event: MidiMessage) -> MidiEvent {
    MidiEvent {
        time_samples: sample,
        track_index: 0,
        event,
    }
}
fn note(sample: u64) -> MidiEvent {
    event(
        sample,
        MidiMessage::NoteOn {
            channel: 3,
            note: 60,
            velocity: 100,
        },
    )
}
fn point(sample: u64, value: f64) -> MidiEvent {
    event(
        sample,
        MidiMessage::PluginParam {
            name: "Gain".into(),
            value,
        },
    )
}
fn prepared(events: &[MidiEvent], frames: usize) -> Result<Schedule, String> {
    let refs: Vec<_> = events.iter().collect();
    compile(
        &[&refs],
        None,
        frames,
        &PluginTransport::new(0, 48_000, 120.0),
    )
}
#[test]
fn vst2_runtime_actual_blocks_accept_dense_adjacent_edges_and_refuse_one_overfull_block() {
    let events: Vec<_> = (0..600)
        .map(|_| note(127))
        .chain((0..600).map(|_| note(128)))
        .collect();
    assert!(prepared(&events, 256).is_ok());
    let events: Vec<_> = (0..MAX_PENDING_MIDI + 1).map(|_| note(127)).collect();
    assert!(prepared(&events, 256).unwrap_err().contains("MIDI density"));
    let events: Vec<_> = (0..MAX_VST2_RUNTIME_PARAMETER_POINTS)
        .map(|_| point(127, 0.25))
        .chain((0..MAX_VST2_RUNTIME_PARAMETER_POINTS).map(|_| point(128, 0.75)))
        .collect();
    let schedule = prepared(&events, 256).unwrap();
    assert_eq!(
        schedule.max_parameter_points,
        MAX_VST2_RUNTIME_PARAMETER_POINTS
    );
    assert_eq!(schedule.names, ["Gain"]);
}
#[test]
fn vst2_runtime_fixed_packets_preserve_all_channels_release_and_cc14_law() {
    for channel in 0..16 {
        let packets = packets::encode(&MidiMessage::ControlChange14 {
            channel,
            controller: 7,
            value: 0x1234,
        })
        .unwrap();
        assert_eq!(packets.count, 2);
        assert_eq!(
            packets.bytes,
            [[0xb0 | channel, 7, 0x24], [0xb0 | channel, 39, 0x34]]
        );
        let packets = packets::encode(&MidiMessage::ControlChange14 {
            channel,
            controller: 64,
            value: 0x1234,
        })
        .unwrap();
        assert_eq!(packets.count, 1);
        assert_eq!(packets.bytes[0], [0xb0 | channel, 64, 0x24]);
        assert_eq!(
            packets::encode(&MidiMessage::NoteOff { channel, note: 60 })
                .unwrap()
                .bytes[0],
            [0x80 | channel, 60, 0]
        );
        assert_eq!(
            packets::encode(&MidiMessage::ProgramChange {
                channel,
                program: 127
            })
            .unwrap()
            .bytes[0],
            [0xc0 | channel, 127, 0]
        );
        assert_eq!(
            packets::encode(&MidiMessage::ChannelPressure {
                channel,
                pressure: 10
            })
            .unwrap()
            .bytes[0],
            [0xd0 | channel, 10, 0]
        );
        for (value, bytes) in [(-8192, [0, 0]), (8191, [127, 127])] {
            assert_eq!(
                packets::encode(&MidiMessage::PitchBend { channel, value })
                    .unwrap()
                    .bytes[0],
                [0xe0 | channel, bytes[0], bytes[1]]
            );
        }
    }
}
#[test]
fn vst2_runtime_compound_cc14_counts_both_packets_before_admission() {
    let events: Vec<_> = (0..MAX_PENDING_MIDI - 1)
        .map(|_| note(0))
        .chain([event(
            0,
            MidiMessage::ControlChange14 {
                channel: 0,
                controller: 1,
                value: 500,
            },
        )])
        .collect();
    assert!(prepared(&events, 128).is_err());
}
#[test]
fn vst2_runtime_ordered_points_and_performance_packets_keep_original_source_order() {
    let events = [point(1, 0.2), note(2), point(1, 0.3), note(2)];
    let refs: Vec<_> = events.iter().collect();
    let performance = [oc_synth::performance::ScheduledCommand {
        sample: 3,
        channel: 2,
        message: ChannelCommand::NoteOff {
            note: 60,
            velocity: 37,
        },
    }];
    let schedule = compile(
        &[&refs],
        Some(&performance),
        128,
        &PluginTransport::new(0, 48_000, 120.0),
    )
    .unwrap();
    let mut cursor = Cursor::default();
    let block = cursor.block(&schedule, 0, 128).unwrap();
    assert_eq!(block.performance[0].bytes, [0x82, 60, 37]);
    let values: Vec<_> = block
        .score
        .iter()
        .filter_map(|event| match event.value {
            ScoreValue::Parameter { value, .. } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(values, [0.2, 0.3]);
    assert_eq!(
        block
            .score
            .iter()
            .filter(|event| matches!(event.value, ScoreValue::Midi(_)))
            .count(),
        2
    );
}
#[test]
fn vst2_runtime_exact_tempo_plan_is_consumed_through_short_last_block_without_heap() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n@track t | instrument: piano/acoustic_grand\n").unwrap();
    let transport = project_transport(&project, 23_990, 48_000);
    let events = [note(9), note(10), point(129, 0.5)];
    let refs: Vec<_> = events.iter().collect();
    let schedule = compile(&[&refs], None, 140, &transport).unwrap();
    assert_eq!(
        schedule
            .blocks
            .iter()
            .map(|span| (span.start, span.frames))
            .collect::<Vec<_>>(),
        [(0, 10), (10, 128), (138, 2)]
    );
    let mut cursor = Cursor::default();
    let mut seen = 0;
    assert_eq!(
        crate::test_alloc::operations(|| {
            for span in &schedule.blocks {
                let block = cursor.block(&schedule, span.start, span.frames).unwrap();
                seen += block.score.len() + block.performance.len();
            }
        }),
        0
    );
    assert_eq!(seen, 3);
}
#[test]
fn vst2_runtime_explicit_note_expression_noop_and_invalid_performance_horizon() {
    let packets = packets::encode(&MidiMessage::NoteExpression {
        channel: 0,
        note: 60,
        expression: "pressure".into(),
        value: 0.5,
    })
    .unwrap();
    assert_eq!(packets.count, 0);
    let performance = [oc_synth::performance::ScheduledCommand {
        sample: 128,
        channel: 0,
        message: ChannelCommand::ProgramChange { program: 0 },
    }];
    assert!(
        compile(
            &[],
            Some(&performance),
            128,
            &PluginTransport::new(0, 48_000, 120.0)
        )
        .is_err()
    );
}

#[test]
fn vst2_runtime_meter_boundary_uses_the_same_prepared_block_plan() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n  time_sig_map:\n    2:1 = 3/4\n@track t | instrument: piano/acoustic_grand\n").unwrap();
    let transport = project_transport(&project, 95_990, 48_000);
    let events = [note(9), note(10), point(129, 0.5)];
    let refs: Vec<_> = events.iter().collect();
    let schedule = compile(&[&refs], None, 140, &transport).unwrap();
    assert_eq!(
        schedule
            .blocks
            .iter()
            .map(|span| (span.start, span.frames))
            .collect::<Vec<_>>(),
        [(0, 10), (10, 128), (138, 2)]
    );
    assert_eq!(transport.at_sample(96_000).musical_position().0, 3);
    let mut cursor = Cursor::default();
    let first = cursor.block(&schedule, 0, 10).unwrap();
    assert_eq!(first.score.len(), 1);
    let next = cursor.block(&schedule, 10, 128).unwrap();
    assert_eq!(next.score.len(), 2);
}
