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
