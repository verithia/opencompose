//! Oscillator unit-fraction authority through patch writes and actual DSP.

use super::*;

const KEYS: [&str; 20] = [
    "osc.a.wavetable_position",
    "osc.b.wavetable_position",
    "osc.c.wavetable_position",
    "osc.a.warp_amount",
    "osc.b.warp_amount",
    "osc.c.warp_amount",
    "osc.a.warp_var",
    "osc.b.warp_var",
    "osc.c.warp_var",
    "osc.a.warp2_amount",
    "osc.b.warp2_amount",
    "osc.c.warp2_amount",
    "osc.a.warp2_var",
    "osc.b.warp2_var",
    "osc.c.warp2_var",
    "osc.a.width",
    "osc.b.width",
    "osc.c.width",
    "unison.width",
    "stereo_width",
];

fn field<'a>(patch: &'a mut SynthPatch, key: &str) -> &'a mut f64 {
    match key {
        "osc.a.wavetable_position" => &mut patch.oscillator_a_wavetable_position,
        "osc.b.wavetable_position" => &mut patch.oscillator_b_wavetable_position,
        "osc.c.wavetable_position" => &mut patch.oscillator_c_wavetable_position,
        "osc.a.warp_amount" => &mut patch.oscillator_a_warp_amount,
        "osc.b.warp_amount" => &mut patch.oscillator_b_warp_amount,
        "osc.c.warp_amount" => &mut patch.oscillator_c_warp_amount,
        "osc.a.warp_var" => &mut patch.oscillator_a_warp_var,
        "osc.b.warp_var" => &mut patch.oscillator_b_warp_var,
        "osc.c.warp_var" => &mut patch.oscillator_c_warp_var,
        "osc.a.warp2_amount" => &mut patch.oscillator_a_warp2_amount,
        "osc.b.warp2_amount" => &mut patch.oscillator_b_warp2_amount,
        "osc.c.warp2_amount" => &mut patch.oscillator_c_warp2_amount,
        "osc.a.warp2_var" => &mut patch.oscillator_a_warp2_var,
        "osc.b.warp2_var" => &mut patch.oscillator_b_warp2_var,
        "osc.c.warp2_var" => &mut patch.oscillator_c_warp2_var,
        "osc.a.width" | "unison.width" | "stereo_width" => &mut patch.stereo_width,
        "osc.b.width" => &mut patch.oscillator_b_stereo_width,
        "osc.c.width" => &mut patch.oscillator_c_stereo_width,
        _ => panic!("not an oscillator fraction: {key}"),
    }
}

#[test]
fn oscillator_fraction_schema_preserves_20_key_identity_bits_defaults_and_extremes() {
    for key in KEYS {
        let owner = oc_device_schema::synth::dsp_normalization(key).unwrap();
        let mut patch = SynthPatch::default();
        assert_eq!(
            field(&mut patch, key).to_bits(),
            owner.domain.default.to_bits(),
            "default {key}"
        );
        for value in [
            f64::MIN,
            -1.0,
            -0.0,
            0.0,
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            0.125,
            0.37,
            0.5,
            0.875,
            1.0,
            2.0,
            f64::MAX,
        ] {
            let expected = value.clamp(0.0, 1.0);
            assert_eq!(
                normalized_param_to_physical(key, value).to_bits(),
                expected.to_bits(),
                "forward {key}"
            );
            assert!(patch.set_param(key, value));
            assert_eq!(
                field(&mut patch, key).to_bits(),
                expected.to_bits(),
                "physical {key}"
            );
            patch.set_normalized_param(key, value);
            assert_eq!(
                field(&mut patch, key).to_bits(),
                expected.to_bits(),
                "normalized {key}"
            );
            if (0.0..=1.0).contains(&value) {
                assert_eq!(
                    physical_param_to_normalized(key, value).unwrap().to_bits(),
                    value.to_bits(),
                    "inverse {key}"
                );
            } else {
                assert_eq!(
                    physical_param_to_normalized(key, value),
                    None,
                    "inverse refusal {key}"
                );
            }
            assert_eq!(
                recalibrate_legacy_normalized_param(key, value),
                None,
                "legacy normalized domain {key}"
            );
        }
        patch.set_param(key, 0.37);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            patch.set_param(key, value);
            patch.set_normalized_param(key, value);
            assert_eq!(
                field(&mut patch, key).to_bits(),
                0.37f64.to_bits(),
                "invalid write {key}"
            );
            assert_eq!(
                normalized_param_to_physical(key, value).to_bits(),
                0.0f64.to_bits()
            );
            assert_eq!(physical_param_to_normalized(key, value), None);
        }
    }
    let mut patch = SynthPatch::default();
    let original = serde_json::to_value(&patch).unwrap();
    for version in [1.0, 2.0] {
        assert!(patch.set_param("synth_version", version));
        assert_eq!(serde_json::to_value(&patch).unwrap(), original);
    }
    assert!(
        !patch.set_param("osc.a.warp_shape", 0.7),
        "no invented alias"
    );
    assert_eq!(
        oc_device_schema::synth::dsp_normalization("osc.a.semitones"),
        None
    );
    assert_eq!(
        physical_param_to_normalized("osc.a.semitones", -12.0),
        None,
        "wide mapping policy is unchanged"
    );
}

#[test]
fn oscillator_fraction_schema_preserves_optional_serde_and_document_versions() {
    let mut encoded = serde_json::to_value(SynthPatch::default()).unwrap();
    for oscillator in ['a', 'b', 'c'] {
        for field in ["warp_amount", "warp_var", "warp2_amount", "warp2_var"] {
            encoded
                .as_object_mut()
                .unwrap()
                .remove(&format!("oscillator_{oscillator}_{field}"));
        }
    }
    for field in [
        "oscillator_c_wavetable_position",
        "oscillator_b_stereo_width",
        "oscillator_c_stereo_width",
    ] {
        encoded.as_object_mut().unwrap().remove(field);
    }
    for version in [1, document::PatchDocument::PORTABLE_VERSION] {
        let mut document = document::PatchDocument::from_json(&serde_json::to_vec(&serde_json::json!({
            "version": version, "name": "Unit fraction", "author": "OpenCompose", "category": "Synth", "use_policy": "Test", "patch": encoded,
        })).unwrap()).unwrap();
        for key in KEYS {
            let expected = oc_device_schema::synth::dsp_normalization(key)
                .unwrap()
                .domain
                .default;
            assert_eq!(
                field(&mut document.patch, key).to_bits(),
                expected.to_bits(),
                "version {version} {key}"
            );
        }
        let roundtrip = document::PatchDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(roundtrip.version, version);
    }
    for required in [
        "oscillator_a_wavetable_position",
        "oscillator_b_wavetable_position",
        "stereo_width",
    ] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(required);
        assert!(
            serde_json::from_value::<SynthPatch>(missing).is_err(),
            "required field {required} remains required"
        );
    }
}

fn active_patch() -> SynthPatch {
    SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_b_waveform: Waveform::Wavetable,
        oscillator_c_waveform: Waveform::Wavetable,
        oscillator_b_level: 0.3,
        oscillator_c_level: 0.2,
        oscillator_a_warp_mode: WarpMode::Sync,
        oscillator_a_warp2_mode: WarpMode::Flip,
        oscillator_b_warp_mode: WarpMode::BendPos,
        oscillator_b_warp2_mode: WarpMode::Dlm,
        oscillator_c_warp_mode: WarpMode::AsymNeg,
        oscillator_c_warp2_mode: WarpMode::Pwm,
        unison_voices: 3,
        oscillator_b_unison: 3,
        oscillator_c_unison: 3,
        detune_cents: 7.0,
        oscillator_b_detune_cents: 9.0,
        oscillator_c_detune_cents: 11.0,
        filter_wet: 0.0,
        ..SynthPatch::default()
    }
}

#[test]
fn oscillator_fraction_schema_exact_audio_matches_direct_patch_and_live_updates() {
    for value in [-0.0_f64, 0.125, 0.5, 0.875, 1.0] {
        let mut expected = active_patch();
        let mut actual = expected.clone();
        for key in KEYS {
            *field(&mut expected, key) = value;
            actual.set_normalized_param(key, value);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        let mut sounded = false;
        for frame in 0..512 {
            if frame == 256 {
                for key in KEYS {
                    *field(&mut expected.patch, key) = 0.37;
                    actual.set_param(key, 0.37);
                }
                expected.refresh_warp_cache();
            }
            let pair = actual.next_stereo(48_000);
            sounded |= pair.iter().any(|sample| *sample != 0.0);
            assert_eq!(
                pair.map(f32::to_bits),
                expected.next_stereo(48_000).map(f32::to_bits),
                "value {value} frame {frame}"
            );
        }
        assert!(sounded, "fixture produces audio");
    }
    assert_eq!(
        WarpSlot::new(WarpMode::Flip, 1.0, 0.5).amount,
        2.0,
        "transformed warp domain is not unit-clamped"
    );
}

#[test]
fn oscillator_fraction_schema_live_updates_and_rendering_do_not_allocate() {
    let mut patch = active_patch();
    let mut synth = OpenSynth::new(patch.clone());
    synth.note_on(0, 60, 100);
    // Factory table lazy construction is a resource-preparation responsibility.
    synth.next_stereo(48_000);
    assert_eq!(
        crate::test_alloc::operations(|| {
            for key in KEYS {
                for value in [-0.0_f64, 0.125, 0.5, 1.0] {
                    patch.set_param(key, value);
                    patch.set_normalized_param(key, value);
                    assert_eq!(
                        normalized_param_to_physical(key, value).to_bits(),
                        value.to_bits()
                    );
                    assert_eq!(
                        physical_param_to_normalized(key, value).unwrap().to_bits(),
                        value.to_bits()
                    );
                    synth.set_param(key, value);
                    synth.set_normalized_param(key, value);
                    for _ in 0..8 {
                        assert!(
                            synth
                                .next_stereo(48_000)
                                .iter()
                                .all(|sample| sample.is_finite())
                        );
                    }
                }
            }
        }),
        0
    );
}

#[test]
fn oscillator_fraction_schema_modulation_bounds_match_static_endpoint_audio() {
    let targets = [
        ("osc.a.width", ModDestination::StereoWidth),
        (
            "osc.a.wavetable_position",
            ModDestination::WavetablePositionA,
        ),
        (
            "osc.b.wavetable_position",
            ModDestination::WavetablePositionB,
        ),
        (
            "osc.c.wavetable_position",
            ModDestination::WavetablePositionC,
        ),
        ("osc.a.warp_amount", ModDestination::WarpAmountA),
        ("osc.b.warp_amount", ModDestination::WarpAmountB),
        ("osc.c.warp_amount", ModDestination::WarpAmountC),
        ("osc.a.warp2_amount", ModDestination::Warp2AmountA),
        ("osc.b.warp2_amount", ModDestination::Warp2AmountB),
        ("osc.c.warp2_amount", ModDestination::Warp2AmountC),
    ];
    for (amount, endpoint) in [(-1.0, 0.0), (1.0, 1.0)] {
        let mut reference = active_patch();
        let mut modulated = reference.clone();
        modulated.macros[0] = 1.0;
        for (index, (key, destination)) in targets.iter().enumerate() {
            *field(&mut reference, key) = endpoint;
            *field(&mut modulated, key) = 0.5;
            modulated.mod_routes[index] = ModRoute {
                source: ModSource::Macro1,
                destination: *destination,
                amount,
                ..Default::default()
            };
        }
        let mut reference = OpenSynth::new(reference);
        let mut modulated = OpenSynth::new(modulated);
        reference.note_on(0, 60, 100);
        modulated.note_on(0, 60, 100);
        for frame in 0..512 {
            assert_eq!(
                modulated.next_stereo(48_000).map(f32::to_bits),
                reference.next_stereo(48_000).map(f32::to_bits),
                "endpoint {endpoint} frame {frame}"
            );
        }
    }
}
