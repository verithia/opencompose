use oc_synth_core::synth::{
    OpenSynth, SynthPatch, Waveform,
    granular::{GrainWindow, GranularPatch},
};

#[test]
fn prepared_grain_cloud_handles_fast_signed_scans_and_reset_without_heap_operations() {
    let root = oc_test_support::temp_dir("oc-granular", "bounded-render");
    let sample = root.join("carrier.wav");
    let mut writer = hound::WavWriter::create(
        &sample,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..8_000 {
        writer
            .write_sample(((frame as f32 * 0.11).sin()) * 0.25)
            .unwrap();
    }
    writer.finalize().unwrap();
    for scan_rate in [-4096.0, 4096.0] {
        let mut patch = SynthPatch {
            oscillator_a_waveform: Waveform::Granular,
            oscillator_a_level: 1.0,
            oscillator_b_level: 0.0,
            oscillator_c_level: 0.0,
            sub_level: 0.0,
            noise_level: 0.0,
            filter_wet: 0.0,
            ..Default::default()
        };
        patch.granular[0] = Some(GranularPatch {
            channel_mode: Default::default(),
            pitch_mode: Default::default(),
            read_mode: Default::default(),
            loop_grains: false,
            envelope: None,
            sample: sample.clone(),
            root_note: 60.0,
            density: 400.0,
            grain_length: 0.02,
            position: 0.01,
            loop_start: 0.01,
            loop_end: 0.011,
            ping_pong: true,
            scan_rate,
            window: GrainWindow::Hann,
            window_param: 0.5,
            window_skew: 0.0,
            random_offset: 0.0,
            random_length: 0.0,
            random_pitch: 0.0,
            random_direction: 0.0,
            random_pan: 0.0,
            random_gain: 0.0,
            reverse: false,
            pitch_track: true,
            seed: 1,
        });
        let mut synth = OpenSynth::new(patch);
        let mut finite = true;
        let mut peak = 0.0_f32;
        let operations = oc_test_support::operations(|| {
            synth.note_on(0, 60, 100);
            for _ in 0..4_096 {
                let frame = synth.next_stereo(8_000);
                finite &= frame.iter().all(|v| v.is_finite());
                peak = peak.max(frame[0].abs());
            }
            synth.note_off(0, 60);
            synth.reset();
            synth.note_on(0, 60, 100);
            for _ in 0..64 {
                synth.next_stereo(8_000);
            }
            synth.reset();
        });
        assert_eq!(
            operations, 0,
            "prepared granular note/render/reset must be heap-free"
        );
        assert!(finite);
        assert!(peak > 0.001, "a bounded scan must still deliver real audio");
    }
}

#[test]
fn stereo_side_content_survives_the_actual_synth_without_heap_operations() {
    use oc_synth_core::synth::{
        NativeVoiceEvent,
        granular::{PitchMode, SourceChannelMode},
    };
    let root = oc_test_support::temp_dir("oc-granular", "stereo-synth");
    let sample = root.join("side.wav");
    let mut writer = hound::WavWriter::create(
        &sample,
        hound::WavSpec {
            channels: 2,
            sample_rate: 8_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..8_000 {
        let value = (frame as f32 * 0.11).sin() * 0.25;
        writer.write_sample(value).unwrap();
        writer.write_sample(-value).unwrap();
    }
    writer.finalize().unwrap();
    let mut patch = SynthPatch {
        oscillator_a_waveform: Waveform::Granular,
        oscillator_a_level: 1.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        sub_level: 0.0,
        noise_level: 0.0,
        filter_wet: 0.0,
        ..Default::default()
    };
    patch.granular[0] = Some(GranularPatch {
        channel_mode: SourceChannelMode::Stereo,
        pitch_mode: PitchMode::Follow,
        read_mode: Default::default(),
        loop_grains: false,
        envelope: None,
        sample,
        root_note: 60.0,
        density: 40.0,
        grain_length: 0.02,
        position: 0.0,
        loop_start: 0.0,
        loop_end: 0.0,
        ping_pong: false,
        scan_rate: 0.0,
        window: GrainWindow::Hann,
        window_param: 0.5,
        window_skew: 0.0,
        random_offset: 0.0,
        random_length: 0.0,
        random_pitch: 0.0,
        random_direction: 0.0,
        random_pan: 0.0,
        random_gain: 0.0,
        reverse: false,
        pitch_track: true,
        seed: 1,
    });
    let on = NativeVoiceEvent::On {
        token: 1,
        channel: 0,
        note: 60,
        velocity: 100,
    };
    let pitch = |frame: usize| NativeVoiceEvent::Pitch {
        token: 1,
        semitones: if (frame / 64).is_multiple_of(2) {
            -12.0
        } else {
            12.0
        },
    };
    let mut fresh = OpenSynth::new(patch.clone());
    fresh.send_native_voice(&on).unwrap();
    let mut expected_after_seek = [0.0; 2];
    for frame in 0..512 {
        if frame % 64 == 0 {
            fresh.send_native_voice(&pitch(frame)).unwrap();
        }
        expected_after_seek = fresh.next_stereo(8_000);
    }
    let mut synth = OpenSynth::new(patch);
    let mut actual_after_seek = [0.0; 2];
    let mut peak = 0.0_f32;
    let mut side_error = 0.0_f32;
    let operations = oc_test_support::operations(|| {
        synth.send_native_voice(&on).unwrap();
        for index in 0..4_096 {
            if index % 64 == 0 {
                synth.send_native_voice(&pitch(index)).unwrap();
            }
            let frame = synth.next_stereo(8_000);
            peak = peak.max(frame[0].abs());
            side_error = side_error.max((frame[0] + frame[1]).abs());
        }
        synth
            .send_native_voice(&NativeVoiceEvent::Off { token: 1 })
            .unwrap();
        synth.reset();
        synth.send_native_voice(&on).unwrap();
        // Seeking a source reconstructs it by reset and replay to the cursor.
        for frame in 0..512 {
            if frame % 64 == 0 {
                synth.send_native_voice(&pitch(frame)).unwrap();
            }
            actual_after_seek = synth.next_stereo(8_000);
        }
        synth.reset();
    });
    assert_eq!(operations, 0);
    assert_eq!(actual_after_seek, expected_after_seek);
    assert!(
        peak > 0.001,
        "preserving stereo must not cancel a side-only source"
    );
    assert!(
        side_error < 1e-6,
        "centre playback must preserve opposite channel polarity"
    );
}
