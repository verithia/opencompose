//! AudioUnit render-path tests (macOS).

use super::*;

fn au_project(plugin: &str) -> Project {
    oc_dsl::parse(&format!(
        r#"@ocp "0.2.0"
@project "au render"
  tempo: 120
@preset au_keys
  source: au
  plugin: "{plugin}"
@track keys | preset: au_keys, instrument: keys/piano
  @phrase body [1:1 - 3:1]
    C4 q mf
    E4 q mf
    G4 q mf
    C5 h f
"#
    ))
    .expect("AU test project parses")
}

#[test]
fn output_port_beyond_the_single_au_pair_is_a_readable_refusal() {
    let project = oc_dsl::parse(
        "@ocp \"0.2.0\"\n@project \"au port\"\n  tempo: 120\n@preset au_keys\n  source: au\n  plugin: \"Apple: DLSMusicDevice\"\n@track keys | preset: au_keys, instrument: keys/piano, port: 1\n  @phrase body [1:1 - 2:1]\n    C4 q mf\n",
    )
    .expect("AU port project parses");
    let error = RenderCache::new()
        .render_full(
            &project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .expect_err("AU serves only the main pair");
    assert!(
        error.contains("AudioUnit preset 'au_keys' exposes only the main stereo pair"),
        "{error}"
    );
    assert!(
        error.contains("track 'keys' requests output port 2"),
        "{error}"
    );
}

#[test]
fn missing_au_preset_renders_silence_and_records_a_warning() {
    // The warning buffer is process-global; keep another test's drain out of
    // this test's window.
    let _serial = crate::render_warnings::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::render_warnings::clear();
    let audio = render_without_cache_bounds(&au_project("au:aumu:zzzz:zzzz"));
    assert!(
        audio.iter().all(|sample| *sample == 0.0),
        "a missing AudioUnit must render silence, not substitute a backend"
    );
    let warnings = crate::render_warnings::take();
    assert!(
        warnings.iter().any(|warning| {
            warning.contains("AU preset 'au_keys'") && warning.contains("zzzz:zzzz")
        }),
        "expected a typed AU warning, got {warnings:?}"
    );
}

#[test]
fn system_au_instrument_renders_into_the_mix() {
    if oc_host::au_instance::resolve_component("Apple: DLSMusicDevice").is_err() {
        eprintln!("skipping: 'Apple: DLSMusicDevice' is not registered");
        return;
    }
    // The warning sink is process-global, so this test never touches it: the
    // fallback test asserts on it and cargo runs test threads in parallel.
    let audio = render_without_cache_bounds(&au_project("Apple: DLSMusicDevice"));
    let peak = audio
        .iter()
        .map(|sample| f64::from(*sample).abs())
        .fold(0.0, f64::max);
    assert!(
        peak > 1e-2,
        "DLSMusicDevice rendered {peak:.6} peak; expected an audible note"
    );
}

#[test]
fn au_performance_keeps_controller_timing_in_export_and_streaming() {
    use crate::block_source::{BlockSource, SmartSource, SourceOutput};
    use oc_dsl::timing::ProjectClock;

    if oc_host::au_instance::resolve_component("Apple: DLSMusicDevice").is_err() {
        return;
    }
    const RATE: u32 = 48_000;
    const BLOCK: usize = 128;
    let project = |volume| {
        oc_dsl::parse(&format!(
            r#"@project
  tempo: 120
@preset piano
  source: au
  plugin: "Apple: DLSMusicDevice"
@track play | preset: piano, instrument: keys/piano
  performance: {{ @0 ch0 program 0; @0 ch0 cc 7 100; @0.01 ch0 note_on 60 100; @0.51 ch0 cc 7 {volume}; @2 ch0 note_off 60 37; }}
"#
        ))
        .unwrap()
    };
    let loud = project(100);
    let muted = project(0);
    let offline = |project| {
        RenderCache::new()
            .render_full(project, Path::new("unused.sf2"), RATE, &mut HashMap::new())
            .unwrap()
    };
    let streaming = |project: &Project| {
        let clock = ProjectClock::new(&project.properties).unwrap();
        let commands = oc_synth::performance::schedule(
            project.tracks[0].performance.as_ref().unwrap(),
            &clock,
            RATE,
        )
        .unwrap();
        let preset = &project.presets[0];
        let source = BlockSource::new_performance(
            Some(preset),
            &preset.params,
            Path::new("unused.sf2"),
            RATE,
            120.0,
            &commands,
            None,
        )
        .unwrap();
        let mut source = SmartSource::new(source, RATE, false);
        assert!(source.performance_uses_offsets());
        assert!(
            source
                .send_performance_at(&commands[0], BLOCK as u32, BLOCK as u32)
                .is_err()
        );
        let mut output = SourceOutput::new(0, &[]);
        let frame_count = RATE as usize + 1_024;
        let mut audio = Vec::with_capacity(frame_count * 2);
        let mut next = 0;
        for start in (0..frame_count).step_by(BLOCK) {
            let end = (start + BLOCK).min(frame_count);
            let transport = PluginTransport::new(start as u64, RATE, 120.0);
            assert_eq!(
                crate::test_alloc::operations(|| {
                    while let Some(command) = commands.get(next) {
                        if command.sample >= end as u64 {
                            break;
                        }
                        source
                            .send_performance_at(
                                command,
                                (command.sample - start as u64) as u32,
                                (end - start) as u32,
                            )
                            .unwrap();
                        next += 1;
                    }
                    source.render_into(end - start, RATE, transport, &mut output);
                    audio.extend_from_slice(output.main());
                }),
                0
            );
        }
        assert_eq!(next, commands.len());
        assert!(
            !source.rewind(),
            "an AU performance lane must rebuild on rewind"
        );
        audio
    };
    let energy = |audio: &[f32], from, to| {
        audio[from..to]
            .iter()
            .map(|&x| f64::from(x).powi(2))
            .sum::<f64>()
    };
    for (loud, muted) in [
        (offline(&loud), offline(&muted)),
        (streaming(&loud), streaming(&muted)),
    ] {
        let early = 9_600..19_200;
        assert_eq!(loud[early.clone()], muted[early.clone()]);
        assert!(energy(&loud, early.start, early.end) > 1e-4);
        let late = 43_200..76_800;
        let unmuted = energy(&loud, late.start, late.end);
        assert!(unmuted > 1e-4);
        assert!(energy(&muted, late.start, late.end) < 0.1 * unmuted);
    }
}

#[test]
fn missing_au_performance_source_refuses_the_render() {
    let project = oc_dsl::parse(
        r#"@project
@preset absent
  source: au
  plugin: "aumu:zzzz:zzzz"
@track play | preset: absent, instrument: keys/piano
  performance: { @0 ch0 note_on 60 100; @1 ch0 note_off 60 64; }
"#,
    )
    .unwrap();
    let error = RenderCache::new()
        .render_full(
            &project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap_err();
    assert!(error.contains("AudioUnit performance preset 'absent'"));
}

#[test]
fn au_score_streaming_keeps_note_offsets() {
    use crate::block_source::{BlockSource, SmartSource, SourceOutput};

    if oc_host::au_instance::resolve_component("Apple: DLSMusicDevice").is_err() {
        return;
    }
    let project = au_project("Apple: DLSMusicDevice");
    let preset = &project.presets[0];
    let mut source = SmartSource::new(
        BlockSource::new(
            Some(preset),
            &preset.params,
            Path::new("unused.sf2"),
            48_000,
            120.0,
            None,
        )
        .unwrap(),
        48_000,
        false,
    );
    assert!(source.supports_score_offsets());
    source
        .send_score_at(
            &MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
            79,
        )
        .unwrap();
    let mut output = SourceOutput::new(0, &[]);
    let transport = PluginTransport::new(0, 48_000, 120.0);
    source.render_into(128, 48_000, transport, &mut output);
    assert!(output.main()[..158].iter().all(|sample| *sample == 0.0));
    assert!(
        output.main()[158..]
            .iter()
            .any(|sample| sample.abs() > 1e-5)
    );
    assert!(source.rewind());
}

/// FNV-1a over the raw sample bits; any change in the AU render path shows up.
fn sample_digest(audio: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for sample in audio {
        for byte in sample.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

#[test]
fn au_project_renders_the_same_samples_after_a_fresh_lifecycle() {
    if oc_host::au_instance::resolve_component("Apple: DLSMusicDevice").is_err() {
        eprintln!("skipping: 'Apple: DLSMusicDevice' is not registered");
        return;
    }
    let project = au_project("Apple: DLSMusicDevice");
    // Each render builds, initializes, renders and drops its own AU instance,
    // so the second pass starts from a completely new lifecycle.
    let first = render_without_cache_bounds(&project);
    let second = render_without_cache_bounds(&project);
    eprintln!(
        "AU render digest: {:016x} samples={}",
        sample_digest(&first),
        first.len()
    );
    assert_eq!(
        first, second,
        "a fresh AudioUnit lifecycle must not change the rendered samples"
    );
}

#[test]
fn au_offline_warmup_keeps_the_first_authored_note_at_time_zero() {
    let mut prepared = AuInstance::load("Apple: DLSMusicDevice", 48_000, 128)
        .expect("system instrument for offline preparation");
    let mut reference =
        AuInstance::load("Apple: DLSMusicDevice", 48_000, 128).expect("fresh system instrument");
    super::super::offline_warmup::prepare_au(
        &mut prepared,
        super::super::offline_warmup::WarmupOptions {
            seconds: 0.001,
            cancel: None,
        },
    )
    .unwrap();
    prepared.note_on_channel(0, 60, 0.9, 0);
    reference.note_on_channel(0, 60, 0.9, 0);
    let mut heard = false;
    for offset in (0..4096).step_by(128) {
        let mut actual = [0.0; 256];
        let mut expected = [0.0; 256];
        let transport = PluginTransport::new(offset, 48_000, 120.0);
        prepared
            .process_block_into(&mut actual, Some(&transport))
            .unwrap();
        reference
            .process_block_into(&mut expected, Some(&transport))
            .unwrap();
        heard |= actual.iter().any(|value| value.abs() > 1e-6);
        assert_eq!(
            actual, expected,
            "warm-up must not shift the note or its phase"
        );
    }
    assert!(heard);
}

/// These guards count Rust host allocation and deallocation. AudioToolbox and
/// the system unit may allocate through C/Objective-C allocators, which this
/// counter deliberately does not claim to observe. No device stream is opened.
#[test]
fn prepared_au_instrument_first_midi_partial_blocks_reset_and_adapter_retain_host_storage() {
    use oc_audio::AudioEffect;
    let mut instance =
        AuInstance::load("Apple: DLSMusicDevice", 48_000, 128).expect("system DLS unit");
    let mut output = [0.0; 256];
    assert_eq!(
        crate::test_alloc::operations(|| {
            instance.program_change(0, 0, 0);
            instance.note_on_channel(0, 60, 0.9, 0);
            instance.process_block_into(&mut output, None).unwrap();
        }),
        0,
        "first queued MIDI and native block must retain Rust host storage"
    );
    let mut sounding = output.iter().any(|sample| sample.abs() > 1e-6);
    for frames in [1, 31, 0, 127, 128]
        .into_iter()
        .chain(std::iter::repeat_n(128, 20))
    {
        assert_eq!(
            crate::test_alloc::operations(|| instance
                .process_block_into(&mut output[..frames * 2], None)
                .unwrap()),
            0
        );
        sounding |= output[..frames * 2]
            .iter()
            .any(|sample| sample.abs() > 1e-6);
    }
    assert!(
        sounding,
        "system instrument should generate samples without a device"
    );
    assert_eq!(
        crate::test_alloc::operations(|| {
            instance.note_off_channel(0, 60, 0);
            instance.process_block_into(&mut output, None).unwrap();
            instance.reset_prepared().unwrap();
            instance.note_on_channel(0, 67, 0.9, 0);
            instance.process_block_into(&mut output, None).unwrap();
        }),
        0
    );
    assert!(!instance.processing_failed());
    let mut source = crate::block_source::SmartSource::new(
        crate::block_source::BlockSource::Au(Box::new(instance)),
        48_000,
        false,
    );
    let mut pairs = crate::block_source::SourceOutput::new(0, &[]);
    assert_eq!(
        crate::test_alloc::operations(|| source.render_into(
            128,
            48_000,
            PluginTransport::new(0, 48_000, 120.0),
            &mut pairs
        )),
        0
    );
    assert_eq!(pairs.main().len(), 256);
    assert!(!source.processing_failed());
}

#[test]
fn prepared_au_effect_first_parameter_partial_and_reset_match_fresh_native_audio() {
    use oc_audio::AudioEffect;
    let mut actual =
        AuInstance::load("Apple: AUDynamicsProcessor", 48_000, 128).expect("system dynamics unit");
    let mut reference = AuInstance::load("Apple: AUDynamicsProcessor", 48_000, 128)
        .expect("reference dynamics unit");
    let parameter = actual
        .get_params()
        .into_iter()
        .find(|parameter| parameter.writable)
        .expect("writable native parameter");
    let value = parameter.min + 0.4 * (parameter.max - parameter.min);
    assert!(reference.set_param_by_name(&parameter.name, value));
    let input: Vec<f32> = (0..256)
        .map(|index| ((index / 2) as f32 * 0.17).sin() * 0.5)
        .collect();
    let mut buffer = input.clone();
    let mut expected = input.clone();
    reference.process_interleaved_with_transport(&mut expected, None);
    assert_eq!(
        crate::test_alloc::operations(|| {
            assert!(actual.set_param_by_name(&parameter.name, value));
            actual
                .process_effect_into(&mut buffer, 48_000, None)
                .unwrap();
        }),
        0
    );
    assert_eq!(buffer, expected);
    for frames in [0, 1, 31, 128] {
        buffer.copy_from_slice(&input);
        expected.copy_from_slice(&input);
        reference.process_interleaved(&mut expected[..frames * 2]);
        assert_eq!(
            crate::test_alloc::operations(|| actual
                .process_effect_into(&mut buffer[..frames * 2], 48_000, None)
                .unwrap()),
            0
        );
        assert_eq!(buffer, expected);
    }
    // Queue a parameter on the first frame; its host scratch is already
    // prepared even if no prior call scheduled automation.
    assert!(reference.queue_param_by_name(&parameter.name, 0.6, 0));
    buffer.copy_from_slice(&input);
    expected.copy_from_slice(&input);
    reference.process_interleaved(&mut expected);
    assert_eq!(
        crate::test_alloc::operations(|| {
            assert!(actual.queue_param_by_name(&parameter.name, 0.6, 0));
            actual
                .process_effect_into(&mut buffer, 48_000, None)
                .unwrap();
        }),
        0
    );
    assert_eq!(buffer, expected);
    reference.reset_prepared().unwrap();
    buffer.copy_from_slice(&input);
    expected.copy_from_slice(&input);
    reference.process_interleaved(&mut expected);
    assert_eq!(
        crate::test_alloc::operations(|| {
            actual.reset_prepared().unwrap();
            actual.prepare_block_buffers().unwrap();
            actual
                .process_effect_into(&mut buffer, 48_000, None)
                .unwrap();
        }),
        0
    );
    assert_eq!(buffer, expected);
    assert!(!actual.processing_failed());
}

#[test]
fn prepared_au_invalid_destinations_offsets_rate_and_queue_overflow_refuse_atomically() {
    use oc_audio::{AudioEffect, ProcessingFailureReport};
    use oc_host::au_instance::{AuProcessError, MAX_PENDING_EVENTS};
    let mut instance =
        AuInstance::load("Apple: DLSMusicDevice", 48_000, 128).expect("system DLS unit");
    instance.note_on_channel(0, 60, 0.9, 0);
    let mut odd = [9.0; 3];
    let mut large = [9.0; 258];
    let mut output = [9.0; 256];
    assert_eq!(
        crate::test_alloc::operations(|| {
            assert_eq!(
                instance.process_block_into(&mut odd, None),
                Err(AuProcessError::IncompleteFrame)
            );
            instance.prepare_block_buffers().unwrap();
            assert_eq!(
                instance.process_block_into(&mut large, None),
                Err(AuProcessError::CapacityExceeded)
            );
            instance.prepare_block_buffers().unwrap();
            assert_eq!(
                instance.process_effect_into(&mut output, 44_100, None),
                Err(AuProcessError::SampleRateChanged)
            );
        }),
        0
    );
    assert_eq!(odd, [9.0; 3]);
    assert_eq!(large, [9.0; 258]);
    assert_eq!(output, [9.0; 256]);
    assert!(instance.processing_failed());
    assert!(matches!(
        instance.take_processing_failure(),
        ProcessingFailureReport::Ready(_)
    ));
    assert_eq!(
        instance.take_processing_failure(),
        ProcessingFailureReport::Reported
    );
    instance.prepare_block_buffers().unwrap();
    assert_eq!(
        crate::test_alloc::operations(|| instance.process_block_into(&mut output, None).unwrap()),
        0
    );
    // Invalid short blocks must not clamp a future event early or consume it.
    instance.note_off_channel(0, 60, 31);
    assert_eq!(
        crate::test_alloc::operations(|| {
            assert_eq!(
                instance.process_block_into(&mut output[..2], None),
                Err(AuProcessError::EventOffsetOutOfRange)
            );
            instance.prepare_block_buffers().unwrap();
            instance.process_block_into(&mut output, None).unwrap();
            for _ in 0..MAX_PENDING_EVENTS {
                instance.note_on_channel(0, 60, 0.9, 0);
            }
            instance.note_on_channel(0, 67, 0.9, 0);
            assert_eq!(
                instance.process_block_into(&mut output, None),
                Err(AuProcessError::EventCapacityExceeded)
            );
            assert_eq!(
                instance.prepare_block_buffers(),
                Err(AuProcessError::EventCapacityExceeded)
            );
            instance.reset_prepared().unwrap();
            instance.process_block_into(&mut output, None).unwrap();
        }),
        0
    );
}
