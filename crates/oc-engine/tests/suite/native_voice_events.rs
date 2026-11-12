#[cfg(feature = "plugin-host")]
use oc_engine::block_source::BlockSource;
use oc_engine::interpret::{MidiEvent, MidiMessage};
#[cfg(feature = "plugin-host")]
use oc_host::PluginTransport;
#[cfg(feature = "plugin-host")]
use oc_synth::synth::OpenSynth;
use oc_synth::synth::{NativeVoiceEvent, SynthPatch};

#[test]
fn checked_native_render_rejects_invalid_clock_allocation_and_order() {
    use oc_synth::synth::try_render_track_with_patch_and_clock as render;
    assert!(render(&[], 0, 0, SynthPatch::default(), 120.0, None).is_err());
    assert!(render(&[], usize::MAX, 48000, SynthPatch::default(), 120.0, None).is_err());
    let late = MidiEvent {
        time_samples: 10,
        track_index: 0,
        event: MidiMessage::NoteOff {
            channel: 0,
            note: 60,
        },
    };
    let early = MidiEvent {
        time_samples: 0,
        track_index: 0,
        event: MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    };
    assert!(
        render(
            &[&late, &early],
            100,
            48000,
            SynthPatch::default(),
            120.0,
            None
        )
        .is_err()
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn native_block_source_and_direct_synth_replay_identical_tail_events() {
    let mut direct = OpenSynth::new(SynthPatch::default());
    let mut block = BlockSource::Synth(Box::new(OpenSynth::new(SynthPatch::default())));
    let schedule = [
        NativeVoiceEvent::On {
            token: 1,
            channel: 0,
            note: 60,
            velocity: 100,
        },
        NativeVoiceEvent::Off { token: 1 },
        NativeVoiceEvent::On {
            token: 2,
            channel: 0,
            note: 60,
            velocity: 90,
        },
        NativeVoiceEvent::Pitch {
            token: 1,
            semitones: 0.5,
        },
        NativeVoiceEvent::Pitch {
            token: 1,
            semitones: -0.5,
        },
        NativeVoiceEvent::Off { token: 2 },
    ];
    for (i, event) in schedule.iter().enumerate() {
        assert_eq!(
            direct.send_native_voice(event).unwrap(),
            block.send_native_voice(event).unwrap()
        );
        let mut expected = vec![0.0; 960];
        direct.render_interleaved(&mut expected, 48000);
        let actual = block.render(
            480,
            48000,
            PluginTransport::new(i as u64 * 480, 48000, 120.0),
        );
        assert_eq!(expected, actual);
        assert!(actual.iter().all(|s| s.is_finite()));
    }
    assert!(direct.send_native_voice(&schedule[0]).is_err());
}

#[test]
fn timed_native_events_reach_checked_export_and_fail_on_token_reuse() {
    use oc_synth::synth::try_render_track_with_patch_and_clock as render;
    let events = [
        MidiEvent {
            time_samples: 0,
            track_index: 0,
            event: MidiMessage::NativeVoice(NativeVoiceEvent::On {
                token: 1,
                channel: 0,
                note: 60,
                velocity: 100,
            }),
        },
        MidiEvent {
            time_samples: 480,
            track_index: 0,
            event: MidiMessage::NativeVoice(NativeVoiceEvent::Off { token: 1 }),
        },
        MidiEvent {
            time_samples: 600,
            track_index: 0,
            event: MidiMessage::NativeVoice(NativeVoiceEvent::Pitch {
                token: 1,
                semitones: 1.0,
            }),
        },
    ];
    let refs = events.iter().collect::<Vec<_>>();
    let output = render(&refs, 2400, 48000, SynthPatch::default(), 120.0, None).unwrap();
    assert!(output.iter().all(|v| v.is_finite()));
    assert!(output.iter().any(|v| v.abs() > 0.0001));
    assert!(
        render(
            &[&events[0], &events[0]],
            2400,
            48000,
            SynthPatch::default(),
            120.0,
            None
        )
        .is_err()
    );
}
