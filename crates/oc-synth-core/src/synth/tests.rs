#[test]
fn normalized_discrete_controls_reach_full_domain() {
    let mut patch = super::SynthPatch::default();
    for key in [
        "osc.a.waveform",
        "osc.b.waveform",
        "osc.a.warp_mode",
        "osc.b.warp_mode",
        "osc.c.warp_mode",
        "filter.mode",
        "filter.model",
        "filter2.model",
        "quality.oversampling",
        "unison.voices",
        "step.division",
        "lfo.division",
        "mod.1.source",
        "mod.1.destination",
        "env3.attack",
        "env3.curve",
        "env4.release",
        "env4.curve",
        "lfo2.division",
        "lfo10.rate",
    ] {
        patch.set_normalized_param(key, 1.0);
    }
    // The retired LFO waveform selectors reach Smooth (6) at 1.0, which
    // folds into the same Curve shape; RandomHold is slot 4.
    patch.set_normalized_param("lfo.waveform", 4.0 / 6.0);
    patch.set_normalized_param("lfo2.waveform", 4.0 / 6.0);
    assert_eq!(patch.oscillator_a_waveform, super::Waveform::Spectral);
    assert_eq!(patch.oscillator_b_waveform, super::Waveform::Spectral);
    assert_eq!(patch.oscillator_a_warp_mode, super::WarpMode::RmFilt2);
    assert_eq!(patch.oscillator_b_warp_mode, super::WarpMode::RmFilt2);
    assert_eq!(patch.oscillator_c_warp_mode, super::WarpMode::RmFilt2);
    assert_eq!(patch.lfo_shape_type, super::LfoShapeType::RandomHold);
    assert_eq!(patch.filter_mode, super::FilterMode::Notch);
    assert_eq!(patch.filter_model, super::FilterModel::Moog24);
    assert_eq!(patch.filter2_model, super::FilterModel::Moog24);
    assert_eq!(patch.oscillator_quality, 2);
    assert_eq!(patch.unison_voices, MAX_UNISON_VOICES);
    assert_eq!((patch.step_division, patch.lfo_division), (5, 5));
    assert_eq!(patch.env3_attack, 10.0);
    assert_eq!(patch.env4_release, 20.0);
    assert_eq!((patch.env3_curve, patch.env4_curve), (1.0, 1.0));
    assert_eq!(
        patch.extra_lfos[0].shape_type,
        super::LfoShapeType::RandomHold
    );
    assert_eq!(patch.extra_lfos[0].division, 5);
    assert_eq!(patch.extra_lfos[0].division, 5);
    assert_eq!(
        patch.mod_routes[0].source,
        super::ModSource::Timbre,
        "normalized source domain reaches the last appended source"
    );
    assert_eq!(
        patch.mod_routes[0].destination,
        super::ModDestination::SourceBendC
    );
    // A legacy lane keeps the physical value it had under the old table:
    // 10/25 was Step, 9/14 was Filter Drive.
    let source = super::recalibrate_legacy_normalized_param("mod.1.source", 10.0 / 25.0)
        .expect("source table moved");
    patch.set_normalized_param("mod.1.source", source);
    assert_eq!(patch.mod_routes[0].source, super::ModSource::Step);
    let destination = super::recalibrate_legacy_normalized_param("mod.1.destination", 9.0 / 14.0)
        .expect("destination table moved");
    patch.set_normalized_param("mod.1.destination", destination);
    assert_eq!(
        patch.mod_routes[0].destination,
        super::ModDestination::FilterDrive
    );
    // B/C detune and unison gained normalized arms.
    patch.set_normalized_param("osc.b.detune", 0.5);
    patch.set_normalized_param("osc.c.detune", 0.25);
    assert_eq!(patch.oscillator_b_detune_cents, 50.0);
    assert_eq!(patch.oscillator_c_detune_cents, 25.0);
    patch.set_normalized_param("osc.b.unison", 0.0);
    patch.set_normalized_param("osc.c.unison", 1.0);
    assert_eq!(patch.oscillator_b_unison, 1);
    assert_eq!(patch.oscillator_c_unison, MAX_UNISON_VOICES);
    patch.set_normalized_param("step.sync", 0.5);
    assert!(patch.step_sync);
    patch.set_normalized_param("step.1", 0.5);
    assert_eq!(patch.step_values[0], 0.0);
    patch.set_normalized_param("unison.voices", 0.0);
    assert_eq!(patch.unison_voices, 1);
}
#[test]
fn legacy_normalized_lanes_keep_the_physical_value_they_had() {
    let compare = |key: &str, legacy: f64, physical: f64| {
        let mut expected = SynthPatch::default();
        assert!(expected.set_param(key, physical), "set_param {key}");
        let mut actual = SynthPatch::default();
        let current = super::recalibrate_legacy_normalized_param(key, legacy)
            .unwrap_or_else(|| panic!("{key} table did not move"));
        actual.set_normalized_param(key, current);
        assert_eq!(
            serde_json::to_value(&expected).unwrap(),
            serde_json::to_value(&actual).unwrap(),
            "{key}: legacy lane {legacy} no longer selects physical {physical}"
        );
    };
    for key in [
        "osc.a.waveform",
        "osc.b.waveform",
        "osc.c.waveform",
        "lfo.waveform",
    ] {
        for id in 0..=4 {
            compare(key, id as f64 / 4.0, id as f64);
        }
    }
    for key in ["filter.model", "filter2.model"] {
        for id in 0..=2 {
            compare(key, id as f64 / 2.0, id as f64);
        }
    }
    for id in 0..=25 {
        compare("mod.1.source", id as f64 / 25.0, id as f64);
    }
    for id in 0..=14 {
        compare("mod.1.destination", id as f64 / 14.0, id as f64);
    }
    for id in 0..=25 {
        compare("fxmod.1.source", id as f64 / 25.0, id as f64);
    }
    // These had no normalized arm, so a lane value was the physical value.
    let mut patch = SynthPatch::default();
    let current = super::recalibrate_legacy_normalized_param("osc.b.detune", 0.5).expect("detune");
    patch.set_normalized_param("osc.b.detune", current);
    assert_eq!(patch.oscillator_b_detune_cents, 0.5);
    let current = super::recalibrate_legacy_normalized_param("osc.c.unison", 1.0).expect("unison");
    patch.set_normalized_param("osc.c.unison", current);
    assert_eq!(patch.oscillator_c_unison, 1);
    let current = super::recalibrate_legacy_normalized_param("fx.1.type", 1.0).expect("fx type");
    patch
        .fx
        .push(super::SynthFx::new(super::SynthFxType::Drive));
    patch.set_normalized_param("fx.1.type", current);
    assert_eq!(patch.fx[0].fx_type, super::SynthFxType::Compressor);
    assert_eq!(
        super::recalibrate_legacy_normalized_param("filter.cutoff", 0.5),
        None
    );
}

#[test]
fn nonfinite_automation_preserves_patch_and_audio() {
    let mut actual = super::OpenSynth::new(super::SynthPatch::default());
    let mut expected = super::OpenSynth::new(super::SynthPatch::default());
    actual.note_on(0, 60, 100);
    expected.note_on(0, 60, 100);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for key in [
            "filter.cutoff",
            "output.gain",
            "amp.attack",
            "mod.1.amount",
            "osc.b.semitones",
            "tuning.fine",
            "voice.polyphony",
        ] {
            actual.set_param(key, value);
            actual.set_normalized_param(key, value);
        }
    }
    for _ in 0..4800 {
        assert_eq!(actual.next_stereo(48_000), expected.next_stereo(48_000));
    }
}
use super::*;
#[test]
fn arpeggiator_audio_is_block_invariant_and_stops() {
    let patch = SynthPatch {
        arpeggiator: Some(arpeggiator::ArpSettings {
            beats_per_step: 0.25,
            gate: 0.5,
            descending: false,
        }),
        ..Default::default()
    };
    let restored: SynthPatch =
        serde_json::from_str(&serde_json::to_string(&patch).unwrap()).unwrap();
    assert_eq!(patch.arpeggiator, restored.arpeggiator);
    let render = |block: usize| {
        let mut synth = OpenSynth::new_with_tempo(restored.clone(), 120.0);
        synth.note_on(0, 60, 100);
        synth.note_on(0, 64, 90);
        synth.note_on(1, 67, 80);
        let mut audio = vec![0.0; 8000];
        for chunk in audio.chunks_mut(block * 2) {
            synth.render_interleaved(chunk, 8000);
        }
        synth.all_sound_off(0);
        synth.all_sound_off(1);
        assert!(synth.is_idle());
        audio
    };
    let whole = render(4000);
    assert!(whole.iter().any(|x| x.abs() > 0.001));
    assert!(whole.iter().all(|x| x.is_finite()));
    assert_eq!(whole, render(37));
}

#[test]
fn oscillator_is_finite_and_bounded() {
    for waveform in [
        Waveform::Sine,
        Waveform::Saw,
        Waveform::Square,
        Waveform::Triangle,
    ] {
        for index in 0..10_000 {
            assert!(oscillator_sample(waveform, index as f64 / 10_000.0, 0.01).abs() <= 1.01);
        }
    }
}

#[test]
fn note_off_has_release_tail() {
    let patch = SynthPatch {
        attack: 0.0,
        release: 0.01,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 69, 127);
    for _ in 0..100 {
        synth.next_stereo(48_000);
    }
    synth.note_off(0, 69);
    assert_ne!(synth.next_stereo(48_000), [0.0, 0.0]);
    for _ in 0..1_000 {
        synth.next_stereo(48_000);
    }
    assert_eq!(synth.voices.len(), 0);
}

#[test]
fn voice_count_is_bounded_and_deterministic() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    assert_eq!(synth.voices.capacity(), MAX_VOICES);
    for note in 20..100 {
        synth.note_on(0, note, 100);
    }
    assert_eq!(synth.voices.len(), 16);
    assert_eq!(synth.voices.first().unwrap().note, 84);

    synth.set_param("voice.polyphony", 4.0);
    assert_eq!(synth.voices.len(), 4);
    assert_eq!(synth.voices.first().unwrap().note, 96);
    synth.set_normalized_param("voice.polyphony", 1.0);
    assert_eq!(synth.patch.polyphony, MAX_VOICES);
    for note in 20..100 {
        synth.note_on(0, note, 100);
    }
    assert_eq!(synth.voices.len(), MAX_VOICES);
    assert_eq!(synth.voices.capacity(), MAX_VOICES);
}

#[test]
fn resonant_filter_stays_finite_at_extreme_settings() {
    let mut filter = StateVariableFilter::default();
    for index in 0..100_000 {
        let input = if index == 0 { 1.0 } else { 0.0 };
        assert!(filter.lowpass(input, 20_000.0, 1.0, 44_100.0).is_finite());
    }
}

#[test]
fn ladder_filter_is_bounded_at_maximum_resonance() {
    for dirty in [false, true] {
        for poles in [1, 3, 4] {
            for cutoff in [20.0, 200.0, 2_000.0, 20_000.0] {
                let mut filter = LadderFilter::default();
                let g = (std::f64::consts::PI * cutoff / 44_100.0).tan();
                let g = g / (1.0 + g);
                for index in 0..200_000 {
                    let input = if index < 8 { 1.0 } else { 0.0 };
                    let output = filter.process(input, g, poles as f64, dirty, poles);
                    assert!(output.is_finite(), "non-finite ladder output");
                    assert!(
                        output.abs() <= 1.01,
                        "ladder escaped at cutoff {cutoff}: {output}"
                    );
                }
            }
        }
    }
}

#[test]
fn ladder_models_are_finite_and_distinct_from_svf() {
    let render = |model: FilterModel| {
        let patch = SynthPatch {
            oscillator_a_level: 0.6,
            sub_level: 0.2,
            filter_cutoff: 800.0,
            filter_resonance: 1.0,
            filter_drive: 1.0,
            filter_envelope_amount: 3.0,
            filter_model: model,
            filter2_enabled: true,
            filter2_cutoff: 1_200.0,
            filter2_resonance: 1.0,
            filter2_model: model,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        for note in [36, 48, 60, 72] {
            synth.note_on(0, note, 120);
        }
        (0..32_000)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<f32>>()
    };
    let svf = render(FilterModel::Svf);
    let ladder = render(FilterModel::Ladder);
    let dirty = render(FilterModel::DirtyLadder);
    for audio in [&svf, &ladder, &dirty] {
        assert!(audio.iter().all(|sample| sample.is_finite()));
        let peak = audio.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        // Res 1.0 plus drive 1.0 is a resonant amplifier (Q is unbounded);
        // the voice bus is scaled later, so only finiteness is required.
        assert!(peak < 8.0, "peak {peak} escaped");
    }
    assert_ne!(svf, ladder);
    assert_ne!(ladder, dirty);
}

#[test]
fn ladder_tails_decay_at_maximum_resonance() {
    // The feedback stays inside the oscillation threshold, and the dirty
    // path's saturation must not add small-signal loop gain, so a tiny
    // impulse drains instead of ringing into self-oscillation.
    let fs = 48_000.0;
    for cutoff in [80.0, 425.0, 4_000.0] {
        let tan = (std::f64::consts::PI * cutoff / fs).tan();
        let g = tan / (1.0 + tan);
        for poles in [1, 3, 4] {
            let k = 2f64.powf(poles as f64 / 2.0) * LADDER_FEEDBACK_MARGIN;
            for dirty in [false, true] {
                let mut filter = LadderFilter::default();
                let mut tail = 0.0_f64;
                let mut peak = 0.0_f64;
                for index in 0..fs as usize {
                    let input = if index == 0 { 1e-3 } else { 0.0 };
                    let output = filter.process(input, g, k, dirty, poles);
                    peak = peak.max(output.abs());
                    if index > (0.75 * fs) as usize {
                        tail = tail.max(output.abs());
                    }
                }
                assert!(peak < 2.0, "poles={poles} dirty={dirty} peak {peak}");
                assert!(
                    tail < 1e-6,
                    "poles={poles} dirty={dirty} at {cutoff} Hz: tail {tail:e}"
                );
            }
        }
    }
}

#[test]
fn ladder_slope_tracks_the_pole_count() {
    // Cascaded one-pole stages: |H(2fc)| = 20*log10(5^(-poles/2)).
    let fs = 48_000.0;
    let cutoff = 1_000.0;
    let g = (std::f64::consts::PI * cutoff / fs).tan();
    let g = g / (1.0 + g);
    for (poles, expected_db) in [(1, -6.99), (3, -20.97), (4, -27.96)] {
        let mut filter = LadderFilter::default();
        let mut input_power = 0.0;
        let mut output_power = 0.0;
        for index in 0..fs as usize {
            let input = 0.1 * (2.0 * std::f64::consts::PI * 2.0 * cutoff * index as f64 / fs).sin();
            let output = filter.process(input, g, 0.0, false, poles);
            if index > 4_800 {
                input_power += input * input;
                output_power += output * output;
            }
        }
        let measured_db = 10.0 * (output_power / input_power).log10();
        assert!(
            (measured_db - expected_db).abs() < 1.0,
            "poles={poles}: {measured_db:.2} dB vs {expected_db:.2} dB"
        );
    }
}

#[test]
fn svf_damping_matches_the_cutoff_dependent_law() {
    let rate = 48_000.0;
    for (resonance, cutoff, expected) in [
        (0.0, 1_000.0, 2.0),
        (0.5, 1_000.0, 0.318_207_169_492_571_0),
        (0.7, 1_000.0, 0.170_617_561_542_611_1),
        (0.9, 1_000.0, 0.051_992_507_149_406_51),
        (1.0, 1_000.0, 0.0),
        // The 1/g - g ceiling takes over as the corner approaches Nyquist.
        (0.0, 10_000.0, 1.033_918_202_695_860_9),
        (0.9, 22_000.0, 0.017_184_099_206_342_45),
    ] {
        let g = (std::f64::consts::PI * cutoff / rate).sin();
        let damping = svf_damping(resonance, g);
        assert!(
            (damping - expected).abs() < 1e-12,
            "res {resonance} fc {cutoff}: {damping} vs {expected}"
        );
    }
}

#[test]
fn svf_prewarp_is_sine_and_the_applied_damping_is_capped() {
    let rate = 48_000.0;
    for cutoff in [100.0, 1_000.0, 10_000.0] {
        let coefficients = FilterCoefficients::new(FilterModel::Svf, cutoff, 0.0, rate);
        let g = (std::f64::consts::PI * cutoff / rate).sin();
        assert!((coefficients.g - g).abs() < 1e-15, "cutoff {cutoff}");
        assert!((coefficients.damping - svf_damping(0.0, g)).abs() < 1e-15);
    }
    for cutoff in [20.0, 425.0, 4_000.0] {
        let coefficients = FilterCoefficients::new(FilterModel::Svf, cutoff, 1.0, rate);
        assert_eq!(
            coefficients.damping, SVF_MIN_DAMPING,
            "cutoff {cutoff}: full resonance must hit the linear ceiling"
        );
    }
}

#[test]
fn no_filter_model_self_oscillates_at_full_resonance() {
    let rate = 48_000.0;
    for model in [
        FilterModel::Svf,
        FilterModel::Svf1,
        FilterModel::Svf3,
        FilterModel::Svf4,
        FilterModel::Ladder,
        FilterModel::DirtyLadder,
        FilterModel::Ladder3,
        FilterModel::Ladder1,
        FilterModel::Moog6,
        FilterModel::Moog12,
        FilterModel::Moog18,
        FilterModel::Moog24,
    ] {
        for cutoff in [40.0, 425.0, 4_000.0] {
            let coefficients = FilterCoefficients::new(model, cutoff, 1.0, rate);
            let mut svf = StateVariableFilter::default();
            let mut ladder = LadderFilter::default();
            let mut moog = MoogLadder::default();
            for _ in 0..(rate as usize) {
                let output = process_voice_filter(
                    model,
                    &mut svf,
                    &mut ladder,
                    &mut moog,
                    0.0,
                    0.0,
                    coefficients,
                    FilterMode::LowPass,
                );
                assert_eq!(output, 0.0, "{model:?} rang from a zero state");
            }
            let mut tail = 0.0_f64;
            for index in 0..(2.0 * rate) as usize {
                let input = if index == 0 { 1e-3 } else { 0.0 };
                let output = process_voice_filter(
                    model,
                    &mut svf,
                    &mut ladder,
                    &mut moog,
                    input,
                    0.0,
                    coefficients,
                    FilterMode::LowPass,
                );
                if index > (1.5 * rate) as usize {
                    tail = tail.max(output.abs());
                }
            }
            assert!(
                tail < 1e-6,
                "{model:?} at {cutoff} Hz still rings at {tail:e}"
            );
        }
    }
}

#[test]
fn moog_stage_has_a_fixed_zero_at_minus_029() {
    let rate = 48_000.0;
    for cutoff in [129.9, 425.0, 4_000.0] {
        let coefficients = FilterCoefficients::new(FilterModel::Moog24, cutoff, 0.0, rate);
        let g = coefficients.moog_g;
        let k = coefficients.moog_k;
        let decay = 1.0 - g - k;
        let amplitude = 1e-4;
        let mut moog = MoogLadder::default();
        let h0 = moog.substep(amplitude, g, k, 0.0, 1);
        let h1 = moog.substep(0.0, g, k, 0.0, 1);
        // The stage numerator is G + k*z^-1, so the zero is -k/G.
        let numerator_1 = (h1 - decay * h0) / amplitude;
        let zero = -numerator_1 / (h0 / amplitude);
        assert!(
            (zero + MOOG_STAGE_ZERO).abs() < 1e-12,
            "cutoff {cutoff}: zero at {zero}, not -{MOOG_STAGE_ZERO}"
        );
        // The zero does not change the stage's unity DC gain.
        let dc = (h0 / amplitude + numerator_1) / (1.0 - decay);
        assert!((dc - 1.0).abs() < 1e-12, "cutoff {cutoff}: DC gain {dc}");
    }
}

#[test]
fn moog_decimator_has_unity_dc_gain() {
    // [0.25, 0.50, 0.25] sums to one: a constant settles to the same value.
    let coefficients = FilterCoefficients::new(FilterModel::Moog24, 425.0, 0.0, 48_000.0);
    let mut moog = MoogLadder::default();
    let mut last = 0.0;
    for _ in 0..48_000 {
        last = moog.process(
            0.25,
            coefficients.moog_g,
            coefficients.moog_k,
            coefficients.moog_feedback,
            4,
        );
    }
    assert!((last - 0.25).abs() < 1e-9, "decimator DC gain {last}");
}

#[test]
fn drive_stage_matches_the_measured_law() {
    // Drive 0 is the transparent stage; the saturator lives behind it.
    assert_eq!(drive_signal(0.37, 0.0), 0.37);
    assert_eq!(saturate(0.37), 0.37 - 4.0 / 27.0 * 0.37_f64.powi(3));
    // Small-signal gain is `(1 + drive)^4`.
    for (drive, expected) in [(0.5, 5.0625), (1.0, 16.0)] {
        let gain = drive_signal(1e-6, drive) / 1e-6;
        assert!(
            (gain - expected).abs() < 1e-6,
            "drive={drive}: {gain:.6} vs {expected:.6}"
        );
    }
    // Four transfer points across the drive range.
    for (value, drive) in [(0.05, 0.5), (0.2, 0.5), (0.5, 0.5), (0.2, 1.0)] as [(f64, f64); 4] {
        let x = (value * (1.0 + drive).powi(4)).clamp(-1.5, 1.5);
        let expected = x - 4.0 / 27.0 * x * x * x;
        assert!(
            (drive_signal(value, drive) - expected).abs() < 1e-15,
            "value={value} drive={drive}"
        );
    }
    // Hard clamp at +/-1.5 before the cubic, so loud inputs saturate at 1.
    assert!((drive_signal(0.5, 0.5) - 1.0).abs() < 1e-12);
    assert!((saturate(10.0) - 1.0).abs() < 1e-12);
    assert!((saturate(-10.0) + 1.0).abs() < 1e-12);
    assert!((drive_signal(-0.5, 0.5) + 1.0).abs() < 1e-12);
}

#[test]
fn filter_model_defaults_to_svf_and_legacy_json_is_bit_identical() {
    let patch = SynthPatch {
        filter2_enabled: true,
        filter2_cutoff: 2_400.0,
        filter2_resonance: 0.4,
        ..Default::default()
    };
    let mut legacy = serde_json::to_value(&patch).unwrap();
    let fields = legacy.as_object_mut().unwrap();
    fields.remove("filter_model");
    fields.remove("filter2_model");
    let legacy: SynthPatch = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.filter_model, FilterModel::Svf);
    assert_eq!(legacy.filter2_model, FilterModel::Svf);
    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        for note in [48, 55, 60] {
            synth.note_on(0, note, 100);
        }
        (0..8_192)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<f32>>()
    };
    assert_eq!(render(patch), render(legacy));
}

#[test]
fn filter_model_params_round_trip_through_physical_and_normalized_values() {
    let mut patch = SynthPatch::default();
    patch.set_param("filter.model", 1.0);
    patch.set_param("filter2.model", 2.0);
    assert_eq!(patch.filter_model, FilterModel::Ladder);
    assert_eq!(patch.filter2_model, FilterModel::DirtyLadder);
    // Every physical model id is reachable at its normalized position.
    for model in 0..=11 {
        patch.set_normalized_param("filter.model", model as f64 / 11.0);
        assert_eq!(
            patch.filter_model,
            super::FilterModel::from_value(model as f64),
            "model {model}"
        );
        patch.set_normalized_param("filter2.model", model as f64 / 11.0);
        assert_eq!(
            patch.filter2_model,
            super::FilterModel::from_value(model as f64),
            "filter 2 model {model}"
        );
    }
    // A legacy 0..2 lane keeps the physical model it used to select.
    let model =
        super::recalibrate_legacy_normalized_param("filter.model", 1.0).expect("model table moved");
    patch.set_normalized_param("filter.model", model);
    assert_eq!(patch.filter_model, FilterModel::DirtyLadder);
    patch.set_param("filter.model", 99.0);
    assert_eq!(patch.filter_model, FilterModel::Svf);
    let restored: SynthPatch =
        serde_json::from_str(&serde_json::to_string(&patch).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
}

#[test]
fn track_render_obeys_note_start_and_keeps_release_tail() {
    let events = [
        MidiEvent {
            time_samples: 100,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: 69,
                velocity: 100,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: 300,
            event: MidiMessage::NoteOff {
                channel: 0,
                note: 69,
            },
            track_index: 0,
        },
    ];
    let refs: Vec<_> = events.iter().collect();
    let rendered = render_track(&refs, 1_000, 48_000, None);
    assert!(rendered[..200].iter().all(|sample| *sample == 0.0));
    assert!(
        rendered[202..600]
            .iter()
            .any(|sample| sample.abs() > 0.0001)
    );
    assert!(rendered[602..].iter().any(|sample| sample.abs() > 0.0001));
    assert!(rendered.iter().all(|sample| sample.is_finite()));
}

#[test]
fn sustain_defers_release_and_is_channel_scoped() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 60, 100);
    synth.note_on(1, 60, 100);
    synth.set_sustain(0, true);
    synth.note_off(0, 60);
    synth.note_off(1, 60);
    assert_ne!(synth.voices[0].amplitude_stage, EnvStage::Release);
    assert_eq!(synth.voices[1].amplitude_stage, EnvStage::Release);
    synth.set_sustain(0, false);
    assert_eq!(synth.voices[0].amplitude_stage, EnvStage::Release);
}

#[test]
fn pitch_curve_does_not_retune_neighbor_or_released_voice() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 60, 100);
    synth.note_on(0, 67, 100);
    synth.set_note_expression(0, 60, "pitch_curve", 14.0);
    assert_eq!(synth.voices[0].expression_tuning, 14.0);
    assert_eq!(synth.voices[1].expression_tuning, 0.0);
    synth.note_off(0, 60);
    synth.set_note_expression(0, 60, "pitch_curve", 2.0);
    assert_eq!(synth.voices[0].expression_tuning, 14.0);
}

#[test]
fn pitch_curve_rejects_invalid_direct_values_without_clamping() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 60, 100);
    synth.set_note_expression(0, 60, "pitch_curve", -12.0);
    for value in [25.0, -25.0, f64::NAN, f64::INFINITY] {
        synth.set_note_expression(0, 60, "pitch_curve", value);
        assert_eq!(synth.voices[0].expression_tuning, -12.0);
    }
    synth.set_note_expression(0, 60, "tuning", 14.0);
    assert_eq!(synth.voices[0].expression_tuning, 4.0);
}

#[test]
fn identified_curve_follows_release_not_same_pitch_retrigger() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    let first = synth.note_on_identified(0, 60, 100).unwrap();
    let mut audio = vec![0.0; 4800];
    synth.render_interleaved(&mut audio, 48000);
    assert!(audio.iter().any(|v| v.abs() > 0.0001));
    assert!(synth.note_off_identified(first));
    let second = synth.note_on_identified(0, 60, 100).unwrap();
    assert_ne!(first, second);
    assert_eq!(synth.voices.len(), 2);
    assert!(synth.set_identified_pitch(first, -0.5).unwrap());
    assert_eq!(synth.voices[0].expression_tuning, -0.5);
    assert_eq!(synth.voices[0].amplitude_stage, EnvStage::Release);
    assert_eq!(synth.voices[1].expression_tuning, 0.0);
    assert!(synth.voices[1].key_down);
    assert!(!synth.note_off_identified(u64::MAX));
}

#[test]
fn stolen_identified_voice_cannot_retune_replacement() {
    let patch = SynthPatch {
        polyphony: 1,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    let first = synth.note_on_identified(0, 60, 100).unwrap();
    let second = synth.note_on_identified(0, 64, 100).unwrap();
    assert!(!synth.set_identified_pitch(first, 12.0).unwrap());
    assert!(synth.set_identified_pitch(second, 1.0).unwrap());
    assert_eq!(synth.voices[0].expression_tuning, 1.0);
}

#[test]
fn native_event_token_storage_is_bounded_and_stolen_tokens_do_not_rebind() {
    let patch = SynthPatch {
        polyphony: 2,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    for token in 0..1000 {
        synth
            .send_native_voice(&NativeVoiceEvent::On {
                token,
                channel: 0,
                note: 60,
                velocity: 100,
            })
            .unwrap();
        assert!(synth.event_voices.len() <= 2);
    }
    assert!(
        !synth
            .send_native_voice(&NativeVoiceEvent::Pitch {
                token: 0,
                semitones: 12.0
            })
            .unwrap()
    );
    assert!(synth.voices.iter().all(|v| v.expression_tuning == 0.0));
}

#[test]
fn midi_panic_and_controller_reset_are_channel_scoped() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 60, 100);
    synth.note_on(1, 64, 100);
    synth.set_sustain(0, true);
    synth.all_notes_off(0);
    assert!(!synth.voices[0].key_down);
    assert_ne!(synth.voices[0].amplitude_stage, EnvStage::Release);
    assert!(synth.voices[1].key_down);

    synth.set_pitch_bend(0, 4096);
    synth.set_channel_pressure(0, 100);
    synth.set_mod_wheel(0, 96);
    synth.reset_controllers(0);
    assert_eq!(synth.pitch_bend[0], 0.0);
    assert_eq!(synth.channel_pressure[0], 0.0);
    assert_eq!(synth.mod_wheel[0], 0.0);
    assert!(!synth.sustain[0]);
    assert_eq!(synth.voices[0].amplitude_stage, EnvStage::Release);

    synth.all_sound_off(0);
    assert_eq!(synth.voices.len(), 1);
    assert_eq!(synth.voices[0].channel, 1);
}

#[test]
fn track_render_applies_midi_all_sound_off_at_event_offset() {
    let events = [
        MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: 100,
            event: MidiMessage::ControlChange {
                channel: 0,
                controller: 120,
                value: 0,
            },
            track_index: 0,
        },
    ];
    let refs: Vec<_> = events.iter().collect();
    let rendered = render_track(&refs, 200, 48_000, None);
    assert!(rendered[..200].iter().any(|sample| sample.abs() > 0.0001));
    assert!(rendered[200..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn v1_aliases_and_v2_routes_are_backward_compatible() {
    let mut patch = SynthPatch::default();
    patch.set_param("osc.waveform", 2.0);
    patch.set_param("waveform", 3.0);
    patch.set_param("mod.1.source", 3.0);
    patch.set_param("mod.1.destination", 0.0);
    patch.set_param("mod.1.amount", 0.75);
    patch.set_param("macro.1", 0.4);
    assert_eq!(patch.oscillator_a_waveform, Waveform::Triangle);
    assert_eq!(patch.mod_routes[0].source, ModSource::Macro1);
    assert_eq!(
        patch.mod_routes[0].destination,
        ModDestination::FilterCutoff
    );
    assert_eq!(patch.mod_routes[0].amount, 0.75);
    assert_eq!(patch.macros[0], 0.4);
}

#[test]
fn noise_and_modulation_render_is_reproducible() {
    let mut patch = SynthPatch {
        noise_level: 0.4,
        ..Default::default()
    };
    patch.mod_routes[0] = ModRoute {
        source: ModSource::Lfo,
        destination: ModDestination::FilterCutoff,
        amount: 0.8,
        ..ModRoute::default()
    };
    let mut first = OpenSynth::new(patch.clone());
    let mut second = OpenSynth::new(patch);
    first.note_on(0, 60, 110);
    second.note_on(0, 60, 110);
    let a: Vec<_> = (0..2_000).map(|_| first.next_stereo(48_000)).collect();
    let b: Vec<_> = (0..2_000).map(|_| second.next_stereo(48_000)).collect();
    assert_eq!(a, b);
    assert!(a.iter().flatten().all(|sample| sample.is_finite()));
}

#[test]
fn waveform_and_mod_destination_ids_are_append_only() {
    // Original numeric waveform ids keep their meaning.
    for (value, waveform) in [
        (0.0, Waveform::Sine),
        (1.0, Waveform::Saw),
        (2.0, Waveform::Square),
        (3.0, Waveform::Triangle),
        (4.0, Waveform::Wavetable),
    ] {
        assert_eq!(waveform_from_value(value), waveform);
    }
    assert_eq!(waveform_from_value(5.0), Waveform::Granular);
    assert_eq!(waveform_from_value(6.0), Waveform::Spectral);
    // Original modulation destination ids keep their meaning.
    for (value, destination) in [
        (1.0, ModDestination::Pitch),
        (2.0, ModDestination::OscillatorMix),
        (15.0, ModDestination::VoiceAmp),
        (16.0, ModDestination::WarpAmountA),
        (19.0, ModDestination::Warp2AmountA),
        (21.0, ModDestination::Warp2AmountC),
        (22.0, ModDestination::WavetablePositionB),
    ] {
        assert_eq!(mod_destination_from_value(value), destination);
    }
    for (value, destination) in [
        (23.0, ModDestination::SourceDensityA),
        (25.0, ModDestination::SourceDensityC),
        (26.0, ModDestination::SourcePositionA),
        (29.0, ModDestination::SourceMorphA),
        (32.0, ModDestination::SourceBendA),
        (34.0, ModDestination::SourceBendC),
    ] {
        assert_eq!(mod_destination_from_value(value), destination);
    }
    // Serialized patch names for the original variants are unchanged.
    assert_eq!(
        serde_json::to_string(&Waveform::Wavetable).unwrap(),
        "\"Wavetable\""
    );
    assert_eq!(
        serde_json::to_string(&ModDestination::FilterCutoff).unwrap(),
        "\"FilterCutoff\""
    );
}

fn source_patch(waveform: Waveform, sample: &std::path::Path) -> SynthPatch {
    let mut patch = SynthPatch {
        filter_wet: 0.0,
        oscillator_a_level: 1.0,
        oscillator_a_waveform: waveform,
        ..Default::default()
    };
    match waveform {
        Waveform::Granular => {
            let settings = granular::GranularPatch {
                sample: sample.to_path_buf(),
                root_note: 60.0,
                density: 60.0,
                grain_length: 0.03,
                position: 0.1,
                loop_start: 0.0,
                loop_end: 0.0,
                ping_pong: false,
                scan_rate: 0.0,
                window: granular::GrainWindow::Hann,
                window_param: 0.5,
                window_skew: 0.0,
                random_offset: 0.2,
                random_length: 0.1,
                random_pitch: 0.3,
                random_direction: 0.2,
                random_pan: 0.4,
                random_gain: 0.1,
                reverse: false,
                pitch_track: true,
                seed: 12_345,
            };
            patch.granular[0] = Some(settings);
        }
        Waveform::Spectral => {
            let settings = spectral::SpectralPatch {
                sample: sample.to_path_buf(),
                root_note: 60.0,
                position: 0.1,
                loop_start: 0.0,
                loop_end: 0.0,
                ping_pong: false,
                scan_rate: 0.0,
                pitch_track: true,
                warp: spectral::SpectralWarp::Comb,
                morph: 0.5,
                warp2: spectral::SpectralWarp::Off,
                morph2: 0.0,
                bend: 0.0,
                phase_lock: 1.0,
                transients: false,
                low_hz: 0.0,
                high_hz: 0.0,
                band_smooth: false,
                seed: 1,
            };
            patch.spectral[0] = Some(settings);
        }
        _ => unreachable!("source patch needs a source waveform"),
    }
    patch
}

fn render_source(waveform: Waveform) -> Vec<f32> {
    let path = std::env::temp_dir().join(format!(
        "opencompose-source-{}-{waveform:?}.wav",
        std::process::id()
    ));
    write_sampler_tone(&path, 441.0);
    let patch = source_patch(waveform, &path);
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut output = Vec::with_capacity(4_096);
    for _ in 0..4_096 {
        output.push(synth.next_stereo(48_000)[0]);
    }
    let _ = std::fs::remove_file(&path);
    output
}

#[test]
fn granular_source_renders_deterministically() {
    let first = render_source(Waveform::Granular);
    let second = render_source(Waveform::Granular);
    assert_eq!(first, second);
    assert!(first.iter().any(|sample| sample.abs() > 1.0e-4));
}

#[test]
fn spectral_source_renders_deterministically() {
    let first = render_source(Waveform::Spectral);
    let second = render_source(Waveform::Spectral);
    assert_eq!(first, second);
    assert!(first.iter().any(|sample| sample.abs() > 1.0e-4));
}

/// Note-on reuses an idle engine; emptying the idle set forces the fallback
/// that builds a fresh one, which is the path this optimisation replaced.
#[test]
fn prebuilt_sources_render_like_fresh_ones() {
    for waveform in [Waveform::Granular, Waveform::Spectral] {
        let path = std::env::temp_dir().join(format!(
            "opencompose-source-idle-{}-{waveform:?}.wav",
            std::process::id()
        ));
        write_sampler_tone(&path, 441.0);
        let patch = source_patch(waveform, &path);
        let render = |reuse_idle: bool| {
            let mut synth = OpenSynth::new(patch.clone());
            if !reuse_idle {
                synth.spare_voices.clear();
            }
            synth.note_on(0, 60, 100);
            (0..4_096)
                .map(|_| synth.next_stereo(48_000))
                .collect::<Vec<_>>()
        };
        assert_eq!(render(true), render(false), "{waveform:?}");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn source_density_modulation_changes_grain_density() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-source-density-{}.wav",
        std::process::id()
    ));
    write_sampler_tone(&path, 441.0);
    let render = |amount: f64| {
        let mut patch = source_patch(Waveform::Granular, &path);
        patch.mod_routes[0] = ModRoute {
            source: ModSource::Macro1,
            destination: ModDestination::SourceDensityA,
            amount,
            ..ModRoute::default()
        };
        patch.macros[0] = 1.0;
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        for _ in 0..2_048 {
            synth.next_stereo(48_000);
        }
        let mut energy = 0.0;
        for _ in 0..8_192 {
            let [left, right] = synth.next_stereo(48_000);
            energy += f64::from(left * left + right * right);
        }
        energy
    };
    let sparse = render(0.0);
    let dense = render(0.8);
    assert!(sparse > 0.0 && dense > 0.0);
    assert_ne!(sparse, dense);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn normalized_automation_maps_to_physical_parameter_ranges() {
    let mut patch = SynthPatch::default();
    patch.set_normalized_param("filter.cutoff", 0.0);
    assert_eq!(patch.filter_cutoff, 20.0);
    patch.set_normalized_param("filter.cutoff", 1.0);
    assert_eq!(patch.filter_cutoff, 20_000.0);
    patch.set_normalized_param("osc.b.semitones", 0.0);
    assert_eq!(patch.oscillator_b_semitones, -24.0);
    patch.set_normalized_param("mod.1.amount", 0.25);
    assert_eq!(patch.mod_routes[0].amount, -0.5);
}

#[test]
fn physical_lane_values_invert_the_normalized_table() {
    for (name, physical) in [
        ("filter.cutoff", 200.0),
        ("filter.cutoff", 8_000.0),
        ("amp.attack", 0.27),
        ("amp.release", 8.0),
        ("osc.b.semitones", -12.0),
        ("unison.detune", 25.0),
        ("filter.model", 3.0),
    ] {
        let normalized = physical_param_to_normalized(name, physical)
            .unwrap_or_else(|| panic!("{name} has no normalized position"));
        let round = normalized_param_to_physical(name, normalized);
        assert!(
            (round - physical).abs() < 1e-9,
            "{name}: {physical} -> {normalized} -> {round}"
        );
    }
    assert_eq!(
        physical_param_to_normalized("filter.resonance", 0.35),
        Some(0.35)
    );
    assert_eq!(physical_param_to_normalized("filter.resonance", 1.5), None);
    assert!(patch_param_is_continuous("filter.cutoff"));
    assert!(!patch_param_is_continuous("filter.model"));
}

#[test]
fn dense_polyphony_stays_finite_with_modulated_filter() {
    let mut patch = SynthPatch {
        filter_envelope_amount: 5.0,
        ..Default::default()
    };
    patch.mod_routes[0] = ModRoute {
        source: ModSource::Lfo,
        destination: ModDestination::FilterCutoff,
        amount: 1.0,
        ..ModRoute::default()
    };
    let mut synth = OpenSynth::new(patch);
    for note in 48..64 {
        synth.note_on(0, note, 120);
    }
    for _ in 0..48_000 {
        assert!(
            synth
                .next_stereo(48_000)
                .iter()
                .all(|sample| sample.is_finite())
        );
    }
}

#[test]
fn mono_legato_reuses_voice_and_returns_to_last_held_note() {
    let patch = SynthPatch {
        monophonic: true,
        legato: true,
        glide_time: 0.1,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    for _ in 0..100 {
        synth.next_stereo(48_000);
    }
    let envelope = synth.voices[0].amplitude_level;
    synth.note_on(0, 72, 110);
    assert_eq!(synth.voices.len(), 1);
    assert_eq!(synth.voices[0].note, 72);
    assert!(synth.voices[0].pitch_note < 72.0);
    assert_eq!(synth.voices[0].amplitude_level, envelope);
    synth.note_off(0, 72);
    assert_eq!(synth.voices[0].note, 60);
    assert_ne!(synth.voices[0].amplitude_stage, EnvStage::Release);
    synth.note_off(0, 60);
    assert_eq!(synth.voices[0].amplitude_stage, EnvStage::Release);
}

#[test]
fn a_switch_parameter_accepts_a_boolean() {
    // Switch parameters accept boolean values in addition to numeric 0/1.
    let mut patch = SynthPatch::default();
    assert!(!patch.lfo_sync);
    patch.apply_params(Some(&vec![(
        "lfo.sync".to_string(),
        ParamValue::Bool(true),
    )]));
    assert!(patch.lfo_sync);
    patch.apply_params(Some(&vec![(
        "lfo.sync".to_string(),
        ParamValue::Bool(false),
    )]));
    assert!(!patch.lfo_sync);
}

#[test]
fn tempo_synced_lfo_uses_project_tempo_and_selected_division() {
    let patch = SynthPatch {
        lfo_sync: true,
        lfo_division: 4, // one bar / four beats
        ..Default::default()
    };
    let mut synth = OpenSynth::new_with_tempo(patch, 120.0);
    for _ in 0..24_000 {
        synth.next_stereo(48_000);
    }
    assert!((synth.lfo_phase - 0.25).abs() < 1e-9);
}

#[test]
fn tempo_change_preserves_synced_phase_and_free_running_hertz() {
    let mut patch = SynthPatch {
        lfo_sync: true,
        lfo_division: 4,
        ..Default::default()
    };
    let mut synth = OpenSynth::new_with_tempo(patch.clone(), 120.0);
    synth.advance_idle(24000, 48000);
    assert!((synth.lfo_phase - 0.25).abs() < 1e-9);
    synth.set_tempo(60.0, 48000).unwrap();
    assert!((synth.lfo_phase - 0.25).abs() < 1e-9);
    synth.advance_idle(24000, 48000);
    assert!((synth.lfo_phase - 0.375).abs() < 1e-9);
    patch.lfo_sync = false;
    patch.lfo_rate = 0.5;
    let mut synth = OpenSynth::new_with_tempo(patch, 120.0);
    synth.advance_idle(24000, 48000);
    synth.set_tempo(60.0, 48000).unwrap();
    synth.advance_idle(24000, 48000);
    assert!((synth.lfo_phase - 0.5).abs() < 1e-9);
    assert!(synth.set_tempo(f64::NAN, 48000).is_err());
    assert!(synth.set_tempo(120.0, 0).is_err());
}

#[test]
fn mapped_idle_render_matches_explicit_clock_changes_without_phase_reset() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n").unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let patch = SynthPatch {
        lfo_sync: true,
        lfo_pitch_cents: 75.0,
        lfo_retrigger: false,
        ..Default::default()
    };
    let event = MidiEvent {
        time_samples: 36000,
        track_index: 0,
        event: MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 90,
        },
    };
    let mapped = render_track_with_patch_and_clock(
        &[&event],
        40000,
        48000,
        patch.clone(),
        120.0,
        Some(&clock),
    );
    let legacy = render_track_with_patch(&[&event], 40000, 48000, patch.clone(), 120.0);
    let mut expected = OpenSynth::new_with_tempo(patch, 120.0);
    expected.advance_idle(24000, 48000);
    expected.set_tempo(60.0, 48000).unwrap();
    expected.advance_idle(12000, 48000);
    expected.note_on(0, 60, 90);
    let tail: Vec<_> = (0..4000)
        .flat_map(|_| expected.next_stereo(48000))
        .collect();
    assert_eq!(&mapped[72000..], tail.as_slice());
    assert_ne!(mapped, legacy);
    assert!(mapped[..72000].iter().all(|sample| *sample == 0.0));
}

#[test]
fn oscillator_quality_modes_are_bounded_deterministic_and_distinct() {
    let mut outputs = Vec::new();
    for quality in 0..=2 {
        let patch = SynthPatch {
            oscillator_quality: quality,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 108, 127);
        let rendered: Vec<_> = (0..512).map(|_| synth.next_stereo(44_100)).collect();
        assert!(rendered.iter().flatten().all(|sample| sample.is_finite()));
        outputs.push(rendered);
    }
    assert_ne!(outputs[0], outputs[1]);
    assert_ne!(outputs[1], outputs[2]);
}

#[test]
fn representative_patch_meets_audio_health_thresholds() {
    let patch = SynthPatch {
        oscillator_a_level: 0.72,
        oscillator_b_level: 0.3,
        oscillator_b_semitones: 12.0,
        sub_level: 0.12,
        noise_level: 0.02,
        filter_cutoff: 2_400.0,
        filter_resonance: 0.38,
        filter_envelope_amount: 2.4,
        oscillator_quality: 2,
        ..Default::default()
    };
    let mut first = OpenSynth::new_with_tempo(patch.clone(), 118.0);
    let mut second = OpenSynth::new_with_tempo(patch, 118.0);
    for note in [48, 55, 60, 64] {
        first.note_on(0, note, 104);
        second.note_on(0, note, 104);
    }
    let a: Vec<f32> = (0..48_000)
        .flat_map(|_| first.next_stereo(48_000))
        .collect();
    let b: Vec<f32> = (0..48_000)
        .flat_map(|_| second.next_stereo(48_000))
        .collect();
    assert_eq!(a, b, "benchmark renders must be sample deterministic");
    let peak = a
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let mean = a.iter().sum::<f32>() / a.len() as f32;
    let rms = (a.iter().map(|sample| sample * sample).sum::<f32>() / a.len() as f32).sqrt();
    assert!(peak > 0.05 && peak < 0.999, "unexpected peak {peak}");
    assert!(rms > 0.01 && rms < 0.5, "unexpected RMS {rms}");
    assert!(mean.abs() < 0.01, "excessive DC offset {mean}");
    assert!(a.iter().all(|sample| sample.is_finite()));
}

#[test]
fn pulse_width_ring_mod_and_filter_modes_are_audibly_distinct_and_finite() {
    let mut outputs = Vec::new();
    for mode in [
        FilterMode::LowPass,
        FilterMode::BandPass,
        FilterMode::HighPass,
        FilterMode::Notch,
    ] {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Square,
            oscillator_a_pulse_width: 0.23,
            oscillator_b_level: 0.35,
            ring_mod_level: 0.4,
            filter_mode: mode,
            filter_cutoff: 1_600.0,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 52, 112);
        let rendered: Vec<_> = (0..2_048).map(|_| synth.next_stereo(48_000)).collect();
        assert!(rendered.iter().flatten().all(|sample| sample.is_finite()));
        outputs.push(rendered);
    }
    assert_ne!(outputs[0], outputs[1]);
    assert_ne!(outputs[1], outputs[2]);
    assert_ne!(outputs[2], outputs[3]);

    let driven_patch = SynthPatch {
        filter_cutoff: 1_600.0,
        filter_drive: 1.0,
        ..Default::default()
    };
    let mut driven = OpenSynth::new(driven_patch);
    driven.note_on(0, 52, 112);
    let driven_render: Vec<_> = (0..2_048).map(|_| driven.next_stereo(48_000)).collect();
    assert!(
        driven_render
            .iter()
            .flatten()
            .all(|sample| sample.is_finite())
    );
    assert_ne!(outputs[0], driven_render);
}

#[test]
fn pressure_brightness_and_per_note_tuning_reach_the_native_voice() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(3, 60, 100);
    synth.set_channel_pressure(3, 64);
    synth.set_poly_pressure(3, 60, 96);
    synth.set_note_expression(3, 60, "tuning", 0.25);
    synth.set_note_expression(3, 60, "brightness", 0.8);
    assert!((synth.channel_pressure[3] - 64.0 / 127.0).abs() < 1e-9);
    assert!((synth.voices[0].pressure - 96.0 / 127.0).abs() < 1e-9);
    assert_eq!(synth.voices[0].expression_tuning, 0.25);
    assert_eq!(synth.voices[0].expression_brightness, 0.8);
    for _ in 0..4_096 {
        assert!(
            synth
                .next_stereo(48_000)
                .iter()
                .all(|sample| sample.is_finite())
        );
    }
}

#[test]
fn triangle_corners_are_continuous_bounded_and_dc_balanced() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for hz in [40.0, 440.0, 1661.2, 3750.0, 8000.0] {
            let dt = hz / rate;
            let at = |phase| {
                oscillator_sample_with_pulse_width(
                    Waveform::Triangle,
                    phase,
                    dt,
                    dt,
                    0.5,
                    0.0,
                    None,
                    &mut MipState::default(),
                    IMPORTED_READOUT_DELAY * dt,
                )
            };
            for corner in [0.0, 0.5, dt, 1.0 - dt, 0.5 - dt, 0.5 + dt] {
                let before = at((corner - 1e-9_f64).rem_euclid(1.0));
                let after = at((corner + 1e-9_f64).rem_euclid(1.0));
                assert!((before - after).abs() < 1e-6);
            }
            let samples: Vec<_> = (0..4096).map(|i| at(i as f64 / 4096.0)).collect();
            assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
            assert!(samples.iter().sum::<f64>().abs() / 4096.0 < 1e-12);
            // Away from corners, preserve the original ramp exactly.
            assert_eq!(at(0.25), 0.0);
        }
    }
}

#[test]
fn triangle_folded_ninth_is_suppressed() {
    let rate = 48_000.0;
    let frequency = 3750.0;
    let amplitude = |samples: &[f64], hz: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (index, value) in samples.iter().enumerate() {
            let phase = TAU * hz * index as f64 / rate;
            re += value * phase.cos();
            im += value * phase.sin();
        }
        re.hypot(im)
    };
    for quality in [1, 2, 4] {
        let samples: Vec<_> = (0..4096)
            .map(|index| {
                (0..quality)
                    .map(|sub| {
                        oscillator_sample_with_pulse_width(
                            Waveform::Triangle,
                            ((index as f64 + sub as f64 / quality as f64) * frequency / rate)
                                .fract(),
                            frequency / rate / quality as f64,
                            frequency / rate,
                            0.5,
                            0.0,
                            None,
                            &mut MipState::default(),
                            IMPORTED_READOUT_DELAY * frequency / rate,
                        )
                    })
                    .sum::<f64>()
                    / quality as f64
            })
            .collect();
        let ratio = amplitude(&samples, 14_250.0) / amplitude(&samples, frequency);
        eprintln!(
            "triangle {quality}x folded ninth: {} dBc",
            20.0 * ratio.log10()
        );
        assert!(ratio < 0.002, "triangle folded ninth exceeds -54 dBc");
    }
}

#[test]
fn builtin_wavetable_rejects_folded_high_harmonics() {
    // Coherent 3750 Hz at 48 kHz: harmonic nine folds to 14250 Hz,
    // which is not an in-band harmonic of the intended oscillator.
    let length = 4096;
    let amplitude = |samples: &[f64], frequency: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (index, value) in samples.iter().enumerate() {
            let phase = TAU * frequency * index as f64 / 48_000.0;
            re += value * phase.cos();
            im += value * phase.sin();
        }
        (re * re + im * im).sqrt() * 2.0 / samples.len() as f64
    };
    for oversampling in [1, 2, 4] {
        let mut mip = MipState::default();
        let samples: Vec<_> = (0..length + 128)
            .map(|index| {
                (0..oversampling)
                    .map(|sub| {
                        oscillator_sample_with_pulse_width(
                            Waveform::Wavetable,
                            ((index as f64 + sub as f64 / oversampling as f64) * 3750.0 / 48_000.0)
                                .fract(),
                            3750.0 / 48_000.0 / oversampling as f64,
                            3750.0 / 48_000.0,
                            0.5,
                            0.5,
                            None,
                            &mut mip,
                            IMPORTED_READOUT_DELAY * 3750.0 / 48_000.0,
                        )
                    })
                    .sum::<f64>()
                    / oversampling as f64
            })
            .collect();
        // The table starts at level 0 and crossfades to the pitch level;
        // skip the 64-sample crossfade before measuring the steady state.
        let samples = &samples[512..];
        let alias_ratio = amplitude(samples, 14_250.0) / amplitude(samples, 3750.0);
        eprintln!(
            "{oversampling}x folded ninth: {} dBc",
            20.0 * alias_ratio.log10()
        );
        assert!(
            alias_ratio < 0.001,
            "folded ninth harmonic: {} dBc",
            20.0 * alias_ratio.log10()
        );
    }
}

fn band_energy(samples: &[f64], low: f64, high: f64, sample_rate: f64) -> f64 {
    let n = samples.len();
    let window: Vec<f64> = (0..n)
        .map(|index| 0.5 - 0.5 * (TAU * index as f64 / (n - 1) as f64).cos())
        .collect();
    let first = (low * n as f64 / sample_rate).ceil() as usize;
    let last = ((high * n as f64 / sample_rate).floor() as usize).min(n / 2);
    let mut energy = 0.0;
    for bin in first..=last {
        let coefficient = 2.0 * (TAU * bin as f64 / n as f64).cos();
        let (mut previous, mut current) = (0.0, 0.0);
        for (sample, weight) in samples.iter().zip(&window) {
            let next = sample * weight + coefficient * current - previous;
            previous = current;
            current = next;
        }
        energy += current * current + previous * previous - coefficient * current * previous;
    }
    energy
}

fn band_db(samples: &[f64], low: f64, high: f64) -> f64 {
    10.0 * (band_energy(samples, low, high, 48_000.0)
        / band_energy(samples, 1000.0, 4000.0, 48_000.0))
    .log10()
}

fn render_default_wavetable(note: u8, position: f64) -> Vec<f64> {
    render_oscillator(Waveform::Wavetable, note, position)
}

fn render_oscillator(waveform: Waveform, note: u8, position: f64) -> Vec<f64> {
    let patch = SynthPatch {
        oscillator_a_waveform: waveform,
        oscillator_a_wavetable_position: position,
        oscillator_a_level: 0.8,
        filter_wet: 0.0,
        filter_cutoff: 20_000.0,
        attack: 0.001,
        decay: 0.0,
        sustain: 1.0,
        unison_voices: 1,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..24_576)
        .map(|_| f64::from(synth.next_stereo(48_000)[0]))
        .collect()
}

/// Ideal band-limited square: only odd harmonics carry `1/h`.
fn ideal_square_band_db(frequency: f64) -> f64 {
    let band = |low: f64, high: f64| {
        let first = (low / frequency).ceil().max(1.0) as usize;
        let last = ((high / frequency).ceil() - 1.0).max(0.0) as usize;
        (first..=last)
            .filter(|harmonic| harmonic % 2 == 1)
            .map(|harmonic| 1.0 / (harmonic * harmonic) as f64)
            .sum::<f64>()
    };
    10.0 * (band(16_000.0, 20_000.0) / band(1_000.0, 4_000.0)).log10()
}

#[test]
fn default_wavetable_is_a_nine_frame_basic_shapes_bank() {
    let table = default_shapes_table();
    assert_eq!(table.frame_count, DEFAULT_SHAPE_COUNT);
    assert_eq!(table.frame_size, WAVETABLE_SIZE);
    assert!(!table.imported_readout);
    let frame_at =
        |index: usize| &table.samples[index * WAVETABLE_SIZE..(index + 1) * WAVETABLE_SIZE];
    for index in 0..DEFAULT_SHAPE_COUNT {
        let peak = frame_at(index)
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        assert!((peak - 1.0).abs() < 1e-9, "frame {index} peak {peak}");
    }
    // One full period per frame: a rectangular DFT at integer harmonics.
    let frame_harmonic = |index: usize, harmonic: usize| {
        let frame = frame_at(index);
        let step = TAU * harmonic as f64 / WAVETABLE_SIZE as f64;
        let (mut real, mut imag) = (0.0, 0.0);
        for (sample, value) in frame.iter().enumerate() {
            real += value * (step * sample as f64).cos();
            imag -= value * (step * sample as f64).sin();
        }
        2.0 * (real * real + imag * imag).sqrt() / WAVETABLE_SIZE as f64
    };
    let db = |index: usize, harmonic: usize| {
        20.0 * (frame_harmonic(index, harmonic) / frame_harmonic(index, 1)).log10()
    };
    // Saw carries 1/h, sine has one partial, square only odd partials.
    assert!((db(0, 2) - -6.02).abs() < 0.1);
    assert!((db(0, 3) - -9.54).abs() < 0.1);
    assert!(db(1, 2) < -60.0);
    assert!(db(4, 2) < -60.0);
    assert!((db(4, 3) - -9.54).abs() < 0.1);
    assert!(db(8, 2) > -0.5, "narrowest pulse lost its evens");
}

#[test]
fn default_wavetable_position_selects_and_blends_frames() {
    let note = 48;
    let f0 = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
    let level = |position: f64, harmonic: usize| {
        let samples = render_default_wavetable(note, position);
        let amps = harmonic_amplitudes(&samples, 48_000.0, f0, 4);
        20.0 * (amps[harmonic - 1] / amps[0]).log10()
    };
    // Frame 0 is the saw, frame 4 the square, frame 8 the narrowest pulse.
    assert!((level(0.0, 2) - -6.02).abs() < 0.3);
    assert!(level(0.5, 2) < -60.0);
    assert!((level(0.5, 3) - -9.54).abs() < 0.4);
    assert!(level(1.0, 2) > -1.0);
    // 0.0625 is halfway between the saw and the sine: the even partial is
    // halved in amplitude, i.e. 12 dB below the saw's.
    let blend = level(0.0625, 2);
    assert!(
        (-16.0..-8.0).contains(&blend),
        "halfway blend even partial {blend} dB"
    );
}

/// G3 sits at the bottom of a mip level, where an octave-following corner
/// would land 5.7 kHz under the audio Nyquist and read dark.
#[test]
fn default_wavetable_keeps_g3_at_the_bottom_of_a_level() {
    let note = 55;
    let frequency = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
    let value = band_db(
        &render_default_wavetable(note, 0.5)[8_192..],
        16_000.0,
        20_000.0,
    );
    let ideal = ideal_square_band_db(frequency);
    eprintln!("G3: default square {value:.2} dB, ideal {ideal:.2} dB");
    assert!(
        (value - ideal).abs() < 2.0,
        "G3 {value:.2} dB vs ideal {ideal:.2} dB"
    );
}

/// Inter-harmonic energy below 20 kHz relative to the harmonic energy, in dB.
/// The mask matches the probe tool: +-40 Hz around every harmonic.
fn inter_harmonic_floor(samples: &[f64], f0: f64) -> f64 {
    let n = samples.len();
    let mut buffer: Vec<rustfft::num_complex::Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            let phase = TAU * index as f64 / (n - 1) as f64;
            let window = 0.5 - 0.5 * phase.cos();
            rustfft::num_complex::Complex::new(value * window, 0.0)
        })
        .collect();
    let mut planner = rustfft::FftPlanner::<f64>::new();
    planner.plan_fft_forward(n).process(&mut buffer);
    let bin_hz = 48_000.0 / n as f64;
    let (mut harmonic, mut inter) = (0.0, 0.0);
    for (index, value) in buffer.iter().enumerate().take(n / 2) {
        let frequency = index as f64 * bin_hz;
        if frequency >= 20_000.0 {
            break;
        }
        let nearest = (frequency / f0).round();
        let masked = nearest >= 1.0 && (frequency - nearest * f0).abs() <= 40.0;
        if masked {
            harmonic += value.norm_sqr();
        } else {
            inter += value.norm_sqr();
        }
    }
    10.0 * (inter.max(1e-300) / harmonic.max(1e-300)).log10()
}

/// The run-split, tier-unrolled window has to read exactly the taps the
/// modulo form reads, at every tier and at levels narrower than the kernel.
#[test]
fn readout_window_reads_the_taps_the_modulo_form_reads() {
    use super::{READOUT_ROWS, READOUT_TAPS, readout_tables};

    /// The pre-change formulation: blend each tap, wrap with `rem_euclid`.
    fn modulo_read(
        table: &super::ReadoutTable,
        block: &[f64],
        level_len: usize,
        phase: f64,
    ) -> f64 {
        let phase_position = phase.rem_euclid(1.0) * level_len as f64;
        let first_sample = phase_position.floor() as usize;
        let fraction = phase_position - phase_position.floor();
        let row = fraction * READOUT_ROWS as f64;
        let row_index = row.floor() as usize;
        let row_mix = row - row.floor();
        let taps = table.taps;
        let mut sum = 0.0;
        for index in 0..taps {
            let first = table.rows[row_index * taps + index];
            let second = table.rows[(row_index + 1) * taps + index];
            let coefficient = first + (second - first) * row_mix;
            let sample = ((first_sample + index) as isize - table.center as isize)
                .rem_euclid(level_len as isize) as usize;
            sum += coefficient * block[sample];
        }
        sum
    }

    for (tier, &taps) in READOUT_TAPS.iter().enumerate() {
        for &level_len in &[4usize, 8, 16, 32, 64, 256] {
            let block: Vec<f64> = (0..level_len)
                .map(|index| ((index as f64) * 0.618_033_988_75).sin())
                .collect();
            let table = &readout_tables()[tier][3];
            assert_eq!(table.taps, taps);
            for step in 0..64 {
                // Wrapped and unwrapped phases, so both branches of the wrap
                // helper are read against the same reference.
                for phase in [step as f64 / 64.0, step as f64 / 64.0 - 0.25] {
                    let actual = table.read(&block, level_len, phase);
                    let expected = modulo_read(table, &block, level_len, phase);
                    let scale = expected.abs().max(1e-12);
                    assert!(
                        (actual - expected).abs() <= 1e-12 * scale,
                        "tier {tier} level {level_len} phase {phase}: {actual} vs {expected}"
                    );
                }
            }
        }
    }
}

/// Harmonics past the audio Nyquist must be removed, not folded back in. C4 is
/// printed for the record: its level reads 16 taps, the shortest tier, so it
/// only recovers what the level wrap cost (docs/synth-character.md §1).
#[test]
fn default_wavetable_keeps_the_inter_harmonic_floor_below_minus_60_db() {
    for note in [60, 84, 96] {
        let frequency = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
        let floor = inter_harmonic_floor(&render_default_wavetable(note, 0.5)[8_192..], frequency);
        eprintln!("note {note} ({frequency:.1} Hz) inter-harmonic floor {floor:.1} dB");
        if note >= 84 {
            assert!(
                floor < -60.0,
                "note {note} inter-harmonic floor {floor:.1} dB"
            );
        }
    }
}

#[test]
fn default_wavetable_top_octave_tracks_a_full_band_square() {
    for note in [24, 36, 48, 55, 60, 72, 84, 96] {
        let frequency = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
        let value = band_db(
            &render_default_wavetable(note, 0.5)[8_192..],
            16_000.0,
            20_000.0,
        );
        let ideal = ideal_square_band_db(frequency);
        eprintln!(
            "note {note} ({frequency:.1} Hz) 16-20k re 1-4k: default square {value:.1} dB, ideal {ideal:.1} dB"
        );
        assert!(
            (value - ideal).abs() < 2.5,
            "{} Hz default square {value} dB vs ideal {ideal} dB",
            frequency
        );
    }
    let c3 = band_db(
        &render_default_wavetable(48, 0.5)[8_192..],
        16_000.0,
        20_000.0,
    );
    assert!(
        (c3 - -18.4).abs() < 1.5,
        "C3 default square reads {c3} dB, expected about -18.4 dB"
    );
}

fn band_db_at(samples: &[f64], low: f64, high: f64, sample_rate: f64) -> f64 {
    10.0 * (band_energy(samples, low, high, sample_rate)
        / band_energy(samples, 1000.0, 4000.0, sample_rate))
    .log10()
}

fn render_init_table(note: u8, sample_rate: u32) -> Vec<f64> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_a_level: 0.8,
        filter_wet: 0.0,
        filter_cutoff: 20_000.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..sample_rate as usize)
        .map(|_| f64::from(synth.next_stereo(sample_rate)[0]))
        .collect()
}

fn render_pwm_warp(note: u8, sample_rate: u32, knob: f64) -> Vec<f64> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_a_wavetable_position: 0.45,
        oscillator_a_level: 0.65,
        oscillator_a_warp_mode: WarpMode::Pwm,
        oscillator_a_warp_amount: knob,
        filter_wet: 0.0,
        filter_cutoff: 20_000.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..sample_rate as usize)
        .map(|_| f64::from(synth.next_stereo(sample_rate)[0]))
        .collect()
}

/// The PWM map's ramp/hold corners must be band-limited. The band-limited
/// readout is a continuous periodic function of table phase, so the map
/// makes slope jumps, not steps, at `amount` and at 0; the residual is
/// scaled from the readout's own slope across the frame's cycle boundary.
/// The 20-24 kHz gap is now the readout's own transition band: the corner
/// follows the audio Nyquist, so what is left is the filter's passband droop
/// in the last four kHz, 1.5 dB at C3 and 0.03 dB at C4.
#[test]
fn pwm_warp_keeps_its_corners_below_nyquist_at_both_rates() {
    for note in [48, 60] {
        let low = band_db_at(
            &render_pwm_warp(note, 48_000, 0.6),
            20_000.0,
            24_000.0,
            48_000.0,
        );
        let high = band_db_at(
            &render_pwm_warp(note, 96_000, 0.6),
            20_000.0,
            24_000.0,
            96_000.0,
        );
        eprintln!("note {note}: 20-24 kHz {low:.2} dB at 48 kHz vs {high:.2} dB at 96 kHz");
        assert!(
            (low - high).abs() < 1.8,
            "note {note}: 20-24 kHz {low:.1} dB at 48 kHz vs {high:.1} dB at 96 kHz"
        );
    }
    // The correction must not move the unwarped init table.
    let unwarped_48 = render_init_table(48, 48_000);
    let c3_48 = band_db_at(&unwarped_48, 16_000.0, 20_000.0, 48_000.0);
    assert!(
        (c3_48 - -17.9).abs() < 0.5,
        "unwarped C3 16-20 kHz reads {c3_48:.1} dB, documented -17.9 dB"
    );
    let wide_48 = band_db_at(&unwarped_48, 16_000.0, 24_000.0, 48_000.0);
    let wide_96 = band_db_at(&render_init_table(48, 96_000), 16_000.0, 24_000.0, 96_000.0);
    assert!(
        (wide_48 - wide_96).abs() < 0.6,
        "unwarped 16-24 kHz {wide_48:.1} dB at 48 kHz vs {wide_96:.1} dB at 96 kHz"
    );
}

#[test]
fn default_wavetable_is_deterministic() {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_a_wavetable_position: 0.63,
        ..Default::default()
    };
    let mut a = OpenSynth::new(patch.clone());
    let mut b = OpenSynth::new(patch);
    a.note_on(0, 72, 110);
    b.note_on(0, 72, 110);
    for _ in 0..4_096 {
        assert_eq!(a.next_stereo(48_000), b.next_stereo(48_000));
    }
}

#[test]
fn maximum_imported_bank_has_bounded_shared_storage() {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..MAX_CUSTOM_WAVETABLE_SAMPLES)
                .map(|i| (TAU * (i % 2048) as f64 / 2048.0).sin())
                .collect(),
        ),
        frame_size: 2048,
        frame_count: MAX_CUSTOM_WAVETABLE_SAMPLES / 2048,
        imported_readout: false,
        mips: Default::default(),
    };
    let started = std::time::Instant::now();
    let mips = table.prepare_mips();
    let bytes: usize = mips
        .levels
        .iter()
        .map(|v| v.capacity() * size_of::<f64>())
        .sum();
    assert!(bytes <= 48 * 1024 * 1024, "bank allocated {bytes} bytes");
    assert!(mips.levels.iter().flatten().all(|v| v.is_finite()));
    assert_eq!(mips.levels.len(), MIP_LEVEL_LIMIT);
    let duplicate = table.clone();
    assert!(std::ptr::eq(mips, duplicate.prepare_mips()));
    eprintln!(
        "maximum imported bank: {bytes} bytes, preparation {:?}",
        started.elapsed()
    );
}

#[test]
fn imported_mip_level_law_holds() {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..WAVETABLE_SIZE)
                .map(|i| (TAU * i as f64 / WAVETABLE_SIZE as f64).sin())
                .collect(),
        ),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: false,
        mips: Default::default(),
    };
    table.prepare_mips();
    for (frequency, expected) in [
        (32.703, 0),
        (130.813, 2),
        (261.626, 3),
        (523.251, 4),
        (1046.502, 5),
        (2093.005, 6),
        (4186.009, 7),
    ] {
        assert_eq!(
            table.level_for(frequency / 48_000.0),
            expected,
            "level for {frequency} Hz"
        );
    }
    assert_eq!(table.level_for(0.0), 0);
    assert_eq!(table.level_for(200_000.0 / 48_000.0), MIP_LEVEL_LIMIT - 1);
}

/// A square frame, so the readout carries the high-frequency content a
/// corner change acts on.
fn square_table() -> CustomWavetable {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..WAVETABLE_SIZE)
                .map(|i| if i < WAVETABLE_SIZE / 2 { 1.0 } else { -1.0 })
                .collect(),
        ),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: false,
        mips: Default::default(),
    };
    table.prepare_mips();
    table
}

/// Drive one readout per sample while the pitch climbs across one bin
/// boundary of `level`; report the sample where the corner changes.
fn sweep_bin_change(
    level: usize,
    samples: usize,
) -> (CustomWavetable, Vec<f64>, Vec<f64>, Vec<usize>, usize) {
    let table = square_table();
    let start = level as f64 + 0.02;
    let span = 0.96;
    let mut mip = MipState::default();
    let mut phase = 0.0;
    let mut out = Vec::with_capacity(samples);
    let mut phases = Vec::with_capacity(samples);
    let mut bins = Vec::with_capacity(samples);
    for index in 0..samples {
        let position = start + span * index as f64 / samples as f64;
        let dt = 2f64.powf(position) / WAVETABLE_SIZE as f64;
        phase = (phase + dt).fract();
        out.push(table.sample_mip(&mut mip, phase, 0.0, dt, 0.0));
        phases.push(phase);
        bins.push(mip.bin);
    }
    let first = (1..samples)
        .find(|&index| bins[index] != bins[index - 1])
        .expect("the sweep crosses a corner bin");
    (table, out, phases, bins, first)
}

#[test]
fn a_corner_bin_change_fades_instead_of_stepping() {
    // The readout must keep the old corner at the sample the corner changes
    // and fade into the new one, exactly as it does for a level change.
    let level = 6;
    let samples = 24_000;
    let (table, out, phases, bins, change) = sweep_bin_change(level, samples);
    let mips = table.mips.get().unwrap();
    let old_bin = table.sample_level(mips, level, phases[change], 0.0, bins[change - 1]);
    let new_bin = table.sample_level(mips, level, phases[change], 0.0, bins[change]);
    let corner_step = (old_bin - new_bin).abs();
    assert!(corner_step > 0.02, "corner difference {corner_step}");
    let weight = 1.0 / f64::from(MIP_CROSSFADE_SAMPLES / 2);
    assert!(
        (out[change] - old_bin).abs() <= corner_step * weight,
        "readout stepped {} of a {corner_step} corner difference at sample {change}",
        (out[change] - old_bin).abs()
    );
}

#[test]
fn a_corner_bin_change_adds_no_step_to_a_long_note() {
    // A long note whose pitch crosses a corner boundary must fade from the
    // old corner into the new one: the readout keeps the old corner at the
    // change and gains no more than 1/32 of the corner difference per
    // sample, against a full switch in one sample when the corner is
    // swapped outright.
    let level = 7;
    let samples = 24_000;
    let (table, out, phases, bins, change) = sweep_bin_change(level, samples);
    let mips = table.mips.get().unwrap();
    let (old_bin, new_bin) = (bins[change - 1], bins[change]);
    let mut old = Vec::new();
    let mut new = Vec::new();
    for &phase in &phases[change..(change + usize::from(MIP_CROSSFADE_SAMPLES) + 1).min(samples)] {
        old.push(table.sample_level(mips, level, phase, 0.0, old_bin));
        new.push(table.sample_level(mips, level, phase, 0.0, new_bin));
    }
    let corner_step = (old[0] - new[0]).abs();
    assert!(corner_step > 0.02, "corner difference {corner_step}");
    let corner_max = old
        .iter()
        .zip(&new)
        .fold(0.0_f64, |worst, (old, new)| worst.max((old - new).abs()));
    // The readout may move toward the new corner by at most one 64th of the
    // corner difference per sample; a switch moves the whole way at once.
    for index in 0..old.len() {
        let residual = (out[change + index] - old[index]).abs();
        let allowed = corner_max * (index + 1) as f64 / f64::from(MIP_CROSSFADE_SAMPLES);
        assert!(
            residual <= allowed * 1.05 + 1e-9,
            "readout moved {residual:.4} of a {corner_max:.4} corner difference after {} samples",
            index + 1
        );
    }
    let last = old.len() - 1;
    assert!(
        (out[change + last] - new[last]).abs() <= corner_max / 16.0,
        "fade did not reach the new corner"
    );
}

#[test]
fn a_long_static_note_keeps_one_readout_state() {
    // A sustained note at a fixed pitch moves neither the level nor the
    // corner, so nothing about the readout can step mid-note; the phase
    // advances by the same step every sample and never resets.
    let table = square_table();
    let dt: f64 = 130.813 / 48_000.0;
    let mut mip = MipState::default();
    let mut phase: f64 = 0.0;
    for index in 0..576_000 {
        let next = (phase + dt).fract();
        table.sample_mip(&mut mip, phase, 0.0, dt, 0.0);
        if index >= usize::from(MIP_CROSSFADE_SAMPLES) {
            assert_eq!(mip.blend, 0, "readout still fading at sample {index}");
        }
        assert_eq!(mip.bin, readout_bin(table.level_position(dt)));
        assert_eq!(mip.level, table.level_for(dt));
        phase = next;
    }
}

#[test]
fn imported_mip_crossfade_lasts_64_samples() {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..WAVETABLE_SIZE)
                .map(|i| (TAU * i as f64 / WAVETABLE_SIZE as f64).sin())
                .collect(),
        ),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: false,
        mips: Default::default(),
    };
    table.prepare_mips();
    let mut state = MipState::default();
    let slow = 130.813 / 48_000.0;
    state.level = 2;
    table.sample_mip(&mut state, 0.1, 0.0, slow, 0.0);
    assert_eq!(state.level, 2);
    assert_eq!(state.blend, 0);
    let fast = 523.251 / 48_000.0;
    table.sample_mip(&mut state, 0.2, 0.0, fast, 0.0);
    assert_eq!(state.level, 4);
    assert_eq!(state.blend, MIP_CROSSFADE_SAMPLES - 1);
    let mut samples = 1;
    while state.blend > 0 {
        table.sample_mip(&mut state, 0.3, 0.0, fast, 0.0);
        samples += 1;
    }
    assert_eq!(samples, MIP_CROSSFADE_SAMPLES as usize);
}

#[test]
fn imported_mip_cache_is_shared_and_constructor_prepares_it() {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..WAVETABLE_SIZE * 2)
                .map(|i| (TAU * (i % WAVETABLE_SIZE) as f64 / WAVETABLE_SIZE as f64).sin())
                .collect(),
        ),
        frame_size: WAVETABLE_SIZE,
        frame_count: 2,
        imported_readout: false,
        mips: Default::default(),
    };
    let serialized = serde_json::to_string(&table).unwrap();
    table.prepare_mips();
    assert_eq!(serialized, serde_json::to_string(&table).unwrap());
    assert!(Arc::ptr_eq(&table.mips, &table.clone().mips));
    let restored: CustomWavetable = serde_json::from_str(&serialized).unwrap();
    assert!(restored.mips.get().is_none());
    let patch = SynthPatch {
        custom_wavetable: Some(restored.clone()),
        ..Default::default()
    };
    let _synth = OpenSynth::new(patch);
    assert!(
        restored.mips.get().is_some(),
        "constructor must prepare the shared mip cache"
    );
}

#[test]
fn wavetables_keep_their_frame_mean_and_are_peak_normalised() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-wavetable-dc-{}.wav",
        std::process::id()
    ));
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for i in 0..WAVETABLE_SIZE {
        writer
            .write_sample((3.0 + 2.0 * (TAU * i as f64 / WAVETABLE_SIZE as f64).sin()) as f32)
            .unwrap();
    }
    writer.finalize().unwrap();
    let plain = CustomWavetable::load(path.to_str().unwrap()).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.splice(12..12, *b"JUNK\x01\0\0\0A\0");
    let size = (bytes.len() as u32 - 8).to_le_bytes();
    bytes[4..8].copy_from_slice(&size);
    std::fs::write(&path, bytes).unwrap();
    let imported = CustomWavetable::load_imported(path.to_str().unwrap()).unwrap();
    assert_eq!(plain.samples, imported.samples);
    std::fs::remove_file(path).unwrap();
    let mean =
        |table: &CustomWavetable| table.samples.iter().sum::<f64>() / table.samples.len() as f64;
    let peak = |table: &CustomWavetable| {
        table
            .samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()))
    };
    for table in [&plain, &imported] {
        assert!(
            (mean(table) - 0.6).abs() < 1e-6,
            "frame mean must survive, got {}",
            mean(table)
        );
        assert!((peak(table) - 1.0).abs() < 1e-6, "peak {}", peak(table));
    }
}

#[test]
fn imported_readout_aligns_the_global_start_phase() {
    let samples: Arc<Vec<f64>> = Arc::new(
        (0..WAVETABLE_SIZE)
            .map(|i| (TAU * i as f64 / WAVETABLE_SIZE as f64).sin())
            .collect(),
    );
    // Both sides take the imported kernel: the built-in bank reads through
    // its own band-limited kernel, so only this path carries the delay.
    let undelayed = CustomWavetable {
        samples: samples.clone(),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: true,
        mips: Default::default(),
    };
    let imported = CustomWavetable {
        samples,
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: true,
        mips: Default::default(),
    };
    undelayed.prepare_mips();
    imported.prepare_mips();
    let dt = 2093.005 / 48_000.0;
    let level = undelayed.level_for(dt);
    let mut plain_state = MipState {
        level,
        previous: level,
        blend: 0,
        ..Default::default()
    };
    let mut imported_state = plain_state;
    for phase in [0.0, 0.1, 0.37, 0.9] {
        let aligned = phase - IMPORTED_READOUT_DELAY * dt;
        let expected = undelayed.sample_mip(&mut plain_state, aligned, 0.0, dt, 0.0);
        let actual = imported.sample_mip(
            &mut imported_state,
            phase,
            0.0,
            dt,
            IMPORTED_READOUT_DELAY * dt,
        );
        assert!((actual - expected).abs() < 1e-12);
    }
}

/// The 4x sub alignment is a C-layer start-phase step and nothing else.
#[test]
fn sub_4x_c_phase_step_only_applies_to_the_four_times_layout() {
    let patch = |sub_semitones: f64, c_semitones: f64, sub_level: f64| SynthPatch {
        sub_level,
        sub_semitones,
        oscillator_c_semitones: c_semitones,
        oscillator_c_level: 0.5,
        oscillator_start_phase: 0.5,
        ..Default::default()
    };
    let layout = |patch: &SynthPatch| {
        let mut voice = Voice::new(0, 36, 100, 0);
        lay_out_voice_phases(
            &mut voice,
            0.5,
            0.0,
            0.0,
            sub_c_phase_offset(patch),
            [false, false, false],
            1,
            1,
            1,
        );
        voice
    };
    // C two octaves above the sub: the measured step, applied to C only.
    let voice = layout(&patch(-36.0, -12.0, 0.5));
    let step = voice.phases[PHASE_C] - 0.5;
    assert!(
        (step - SUB_4X_C_PHASE_STEP).abs() < 1e-12,
        "C phase step {step}"
    );
    assert!(voice.phases[PHASE_SUB].abs() < 1e-12);
    // Any other ratio, or an inaudible sub, keeps the shared start phase.
    for (sub_semitones, c_semitones, sub_level) in [
        (-24.0, -12.0, 0.5),
        (-36.0, -12.0, 0.0),
        (-36.0, 0.0, 0.5),
        (-36.0, -6.0, 0.5),
    ] {
        let voice = layout(&patch(sub_semitones, c_semitones, sub_level));
        assert!(
            (voice.phases[PHASE_C] - 0.5).abs() < 1e-12,
            "{sub_semitones}/{c_semitones}/{sub_level} moved the C phase"
        );
    }
}

/// Single-frame 2048-point saw with 64 harmonics, for readout measurements.
fn saw_wavetable() -> CustomWavetable {
    let mut samples = vec![0.0; WAVETABLE_SIZE];
    for harmonic in 1..=64 {
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample += (TAU * harmonic as f64 * index as f64 / WAVETABLE_SIZE as f64).sin()
                / harmonic as f64;
        }
    }
    CustomWavetable {
        samples: Arc::new(samples),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: true,
        mips: Default::default(),
    }
}

fn render_wavetable(table: CustomWavetable, note: u8, quality: usize) -> Vec<[f32; 2]> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        oscillator_a_level: 0.8,
        unison_voices: 1,
        detune_cents: 0.0,
        stereo_width: 0.0,
        custom_wavetable: Some(table),
        oscillator_quality: quality,
        filter_wet: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..49_152).map(|_| synth.next_stereo(48_000)).collect()
}

#[test]
fn wavetable_readout_keeps_c7_harmonics_to_h11() {
    let table = saw_wavetable();
    table.prepare_mips();
    let f0 = 2093.005;
    let dt = f0 / 48_000.0;
    let mut state = MipState {
        level: table.level_for(dt),
        ..Default::default()
    };
    let mut rendered = Vec::with_capacity(8_192);
    let mut phase = 0.0;
    for _ in 0..8_192 {
        rendered.push(table.sample_mip(&mut state, phase, 0.0, dt, IMPORTED_READOUT_DELAY * dt));
        phase = (phase + dt).fract();
    }
    let amplitude = |harmonic: usize| {
        let (mut re, mut im) = (0.0, 0.0);
        for (index, &value) in rendered.iter().enumerate() {
            let angle = -TAU * harmonic as f64 * f0 * index as f64 / 48_000.0;
            re += value * angle.cos();
            im += value * angle.sin();
        }
        re.hypot(im)
    };
    let fundamental = amplitude(1);
    for harmonic in 1..=8 {
        let relative = 20.0 * (amplitude(harmonic) / fundamental).log10();
        let expected = -20.0 * (harmonic as f64).log10();
        assert!(
            (relative - expected).abs() < 0.5,
            "H{harmonic} moved to {relative:.2} dB"
        );
    }
    for harmonic in 9..=11 {
        let relative = 20.0 * (amplitude(harmonic) / fundamental).log10();
        assert!(
            relative > -25.0,
            "C7 H{harmonic} was cut to {relative:.2} dB"
        );
    }
}

#[test]
fn c7_readout_rejects_content_above_nyquist() {
    let f0 = 2093.005;
    let rendered = render_wavetable(saw_wavetable(), 96, 0);
    let harmonics = steady_harmonics(&rendered, f0, 11);
    for harmonic in 1..=11 {
        let expected = -20.0 * (harmonic as f64).log10();
        let measured = 20.0 * (harmonics[harmonic - 1] / harmonics[0]).log10();
        assert!(
            (measured - expected).abs() < 3.0,
            "H{harmonic} at {measured:.2} dB, expected {expected:.2} dB"
        );
    }
    for harmonic in 12..=16 {
        let alias = 48_000.0 - harmonic as f64 * f0;
        let relative = 20.0 * (steady_amplitude(&rendered, alias) / harmonics[0]).log10();
        assert!(
            relative < -28.0,
            "H{harmonic} folds to {relative:.1} dB at {alias:.0} Hz"
        );
    }
}

#[test]
fn wavetable_readout_keeps_the_harmonic_trend_across_the_keyboard() {
    for note in [24u8, 48, 72, 84] {
        let f0 = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
        let rendered = render_wavetable(saw_wavetable(), note, 0);
        let measured = steady_harmonics(&rendered, f0, 16);
        for harmonic in 1..=16 {
            let expected = -20.0 * (harmonic as f64).log10();
            let relative = 20.0 * (measured[harmonic - 1] / measured[0]).log10();
            assert!(
                (relative - expected).abs() < 0.5,
                "note {note} H{harmonic}: {relative:.2} dB, expected {expected:.2} dB"
            );
        }
    }
}

#[test]
fn table_readout_ignores_the_quality_oversampling() {
    let low = render_wavetable(saw_wavetable(), 96, 0);
    let high = render_wavetable(saw_wavetable(), 96, 2);
    assert_eq!(
        low, high,
        "a band-limited table read must not be box-averaged"
    );
}

#[test]
fn readout_preserves_a_frame_mean() {
    let table = CustomWavetable {
        samples: Arc::new(vec![0.5; WAVETABLE_SIZE]),
        frame_size: WAVETABLE_SIZE,
        frame_count: 1,
        imported_readout: true,
        mips: Default::default(),
    };
    table.prepare_mips();
    let mut state = MipState::default();
    for phase in [0.0, 0.3, 0.9] {
        let value = table.sample_mip(&mut state, phase, 0.0, 10.0 / 48_000.0, 0.0);
        assert!((value - 0.5).abs() < 1e-9, "phase {phase}: {value}");
    }
}

#[test]
fn imported_wavetable_folded_ninth_is_suppressed() {
    let table = CustomWavetable {
        samples: Arc::new(
            (0..2048)
                .map(|i| {
                    let p = i as f64 / 2048.0;
                    0.8 * (TAU * p).sin() + 0.2 * (TAU * 9.0 * p).sin()
                })
                .collect(),
        ),
        frame_size: 2048,
        frame_count: 1,
        imported_readout: false,
        mips: Default::default(),
    };
    table.prepare_mips();
    let mut state = MipState {
        level: table.level_for(3000.0 / 48_000.0),
        ..Default::default()
    };
    let samples: Vec<_> = (0..4096)
        .map(|i| {
            table.sample_mip(
                &mut state,
                (i as f64 * 3000.0 / 48000.0).fract(),
                0.0,
                3000.0 / 48000.0,
                IMPORTED_READOUT_DELAY * 3000.0 / 48000.0,
            )
        })
        .collect();
    let amplitude = |hz: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &value) in samples.iter().enumerate() {
            let angle = TAU * hz * i as f64 / 48000.0;
            re += value * angle.cos();
            im += value * angle.sin();
        }
        re.hypot(im)
    };
    let ratio = amplitude(21000.0) / amplitude(3000.0);
    eprintln!("imported folded ninth dBc={}", 20.0 * ratio.log10());
    assert!(ratio < 0.001, "folded ninth ratio={ratio}");
}

#[test]
fn external_float_wavetable_rejects_nonfinite_in_every_channel() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-wavetable-nonfinite-{}.wav",
        std::process::id()
    ));
    for channel in 0..2 {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..128 {
                let value = if i == 32 + channel {
                    invalid
                } else {
                    (i as f32).sin()
                };
                writer.write_sample(value).unwrap();
            }
            writer.finalize().unwrap();
            let result = CustomWavetable::load(path.to_str().unwrap());
            assert!(result.is_err(), "accepted {invalid} on channel {channel}");
        }
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn imported_frames_keep_their_dc_and_share_one_peak_scale() {
    let path =
        std::env::temp_dir().join(format!("opencompose-frame-dc-{}.wav", std::process::id()));
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for (offset, amplitude) in [(0.3, 0.2), (-0.3, 0.4)] {
        for i in 0..WAVETABLE_SIZE {
            writer
                .write_sample(
                    (offset + amplitude * (TAU * i as f64 / WAVETABLE_SIZE as f64).sin()) as f32,
                )
                .unwrap();
        }
    }
    writer.finalize().unwrap();
    let table = CustomWavetable::load(path.to_str().unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    let peak = 0.7;
    for (position, mean) in [(0.0, 0.3 / peak), (1.0, -0.3 / peak)] {
        let measured = (0..WAVETABLE_SIZE)
            .map(|i| table.sample(i as f64 / WAVETABLE_SIZE as f64, position))
            .sum::<f64>()
            / WAVETABLE_SIZE as f64;
        assert!(
            (measured - mean).abs() < 1e-6,
            "position={position} mean={measured}, expected {mean}"
        );
    }
    // The common peak scale keeps the frames' relative dynamics.
    let ac = |position: f64| {
        let mean = (0..WAVETABLE_SIZE)
            .map(|i| table.sample(i as f64 / WAVETABLE_SIZE as f64, position))
            .sum::<f64>()
            / WAVETABLE_SIZE as f64;
        (0..WAVETABLE_SIZE)
            .map(|i| table.sample(i as f64 / WAVETABLE_SIZE as f64, position) - mean)
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt()
    };
    assert!((ac(0.0) / ac(1.0) - 0.5).abs() < 1e-6);
}

#[test]
fn external_stereo_multiframe_wavetable_loads_normalizes_and_morphs() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-wavetable-{}-{}.wav",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).expect("create fixture wavetable");
    for sample in 0..(WAVETABLE_SIZE * 2) {
        let phase = (sample % WAVETABLE_SIZE) as f64 / WAVETABLE_SIZE as f64;
        let left = if sample < WAVETABLE_SIZE {
            (TAU * phase).sin()
        } else if phase < 0.5 {
            1.0
        } else {
            -1.0
        };
        writer
            .write_sample((left * i16::MAX as f64) as i16)
            .unwrap();
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();

    let table = CustomWavetable::load(path.to_str().unwrap()).expect("load fixture wavetable");
    assert_eq!(table.frame_size, WAVETABLE_SIZE);
    assert_eq!(table.frame_count, 2);
    assert!((table.sample(0.25, 0.0) - 1.0).abs() < 0.01);
    assert!(table.sample(0.25, 1.0) > 0.9);
    assert!(table.sample(0.75, 1.0) < -0.9);
    assert!(table.sample(0.25, 0.5).is_finite());
    std::fs::remove_file(&path).expect("remove fixture wavetable");
}

#[test]
fn lfo_shapes_retrigger_and_sample_hold_are_deterministic() {
    let patch = SynthPatch {
        lfo_shape_type: LfoShapeType::RandomHold,
        lfo_rate: 24.0,
        lfo_retrigger: true,
        lfo_pitch_cents: 25.0,
        ..Default::default()
    };
    let mut a = OpenSynth::new(patch.clone());
    let mut b = OpenSynth::new(patch);
    a.note_on(0, 60, 100);
    b.note_on(0, 60, 100);
    assert_eq!(a.lfo_phase, 0.0);
    for _ in 0..4_096 {
        assert_eq!(a.next_stereo(48_000), b.next_stereo(48_000));
    }
    assert_eq!(a.lfo_hold_value, b.lfo_hold_value);
    assert_ne!(a.lfo_hold_value, 0.0);
    a.note_on(0, 67, 100);
    assert_eq!(a.lfo_phase, 0.0);
}

#[test]
fn mod_wheel_is_channel_scoped_and_available_to_routes() {
    let mut patch = SynthPatch::default();
    patch.mod_routes[0] = ModRoute {
        source: ModSource::ModWheel,
        destination: ModDestination::FilterCutoff,
        amount: 1.0,
        ..ModRoute::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(2, 60, 100);
    synth.note_on(3, 64, 100);
    synth.set_mod_wheel(2, 127);
    assert_eq!(synth.mod_wheel[2], 1.0);
    assert_eq!(synth.mod_wheel[3], 0.0);
    for _ in 0..2_048 {
        assert!(
            synth
                .next_stereo(48_000)
                .iter()
                .all(|sample| sample.is_finite())
        );
    }
}

#[test]
fn step_modulator_holds_or_smoothly_interpolates_bipolar_values() {
    let mut values = [0.0; 16];
    values[0] = -1.0;
    values[1] = 1.0;
    assert_eq!(step_value_at(&values, 0.03125, 0.0), -1.0);
    let smoothed = step_value_at(&values, 0.03125, 1.0);
    assert!(
        smoothed.abs() < 1e-12,
        "halfway smooth value was {smoothed}"
    );
    assert_eq!(step_value_at(&values, 1.0 / 16.0, 1.0), 1.0);
}

#[test]
fn tempo_synced_step_modulator_advances_one_step_per_sixteenth() {
    let patch = SynthPatch {
        step_sync: true,
        step_division: 0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new_with_tempo(patch, 120.0);
    for _ in 0..6_000 {
        synth.next_stereo(48_000);
    }
    assert!((synth.step_phase - 1.0 / 16.0).abs() < 1e-9);
}

#[test]
fn step_parameters_are_bounded_and_append_to_the_modulation_source_abi() {
    let mut patch = SynthPatch::default();
    patch.set_param("step.rate", 99.0);
    patch.set_param("step.smoothing", -2.0);
    patch.set_param("step.1", -5.0);
    patch.set_param("step.16", 5.0);
    patch.set_param("mod.1.source", 10.0);
    assert_eq!(patch.step_rate, 30.0);
    assert_eq!(patch.step_smoothing, 0.0);
    assert_eq!(patch.step_values[0], -1.0);
    assert_eq!(patch.step_values[15], 1.0);
    assert_eq!(patch.mod_routes[0].source, ModSource::Step);
    patch.set_param("mod.16.source", 10.0);
    patch.set_param("mod.16.destination", 9.0);
    patch.set_param("mod.16.amount", -0.75);
    assert_eq!(patch.mod_routes[15].source, ModSource::Step);
    assert_eq!(
        patch.mod_routes[15].destination,
        ModDestination::FilterDrive
    );
    assert_eq!(patch.mod_routes[15].amount, -0.75);
}

#[test]
fn envelope_curves_change_shape_without_changing_stage_duration() {
    let mut slow_stage = EnvStage::Attack;
    let mut slow_level = 0.0;
    let mut slow_phase = 0.0;
    let mut fast_stage = EnvStage::Attack;
    let mut fast_level = 0.0;
    let mut fast_phase = 0.0;
    let mut slow_curve = EnvelopeCurve::default();
    slow_curve.rebuild(-1.0);
    let mut fast_curve = EnvelopeCurve::default();
    fast_curve.rebuild(1.0);
    let steps = EnvelopeStageSteps::new(1.0, 0.1, 0.5, 0.2, 100.0);
    for _ in 0..50 {
        assert!(tick_envelope(
            &mut slow_stage,
            &mut slow_level,
            &mut slow_phase,
            0.0,
            steps,
            &slow_curve,
        ));
        assert!(tick_envelope(
            &mut fast_stage,
            &mut fast_level,
            &mut fast_phase,
            0.0,
            steps,
            &fast_curve,
        ));
    }
    assert!(slow_level < 0.1);
    assert!(fast_level > 0.8);
    for _ in 0..50 {
        tick_envelope(
            &mut slow_stage,
            &mut slow_level,
            &mut slow_phase,
            0.0,
            steps,
            &slow_curve,
        );
        tick_envelope(
            &mut fast_stage,
            &mut fast_level,
            &mut fast_phase,
            0.0,
            steps,
            &fast_curve,
        );
    }
    assert_eq!(slow_stage, EnvStage::Decay);
    assert_eq!(fast_stage, EnvStage::Decay);
    assert_eq!(slow_level, 1.0);
    assert_eq!(fast_level, 1.0);
}

#[test]
fn attack_10_90_times_follow_the_curve_sweep() {
    // 0.5 s attack, sine, 48 kHz; measured on the reference render.
    let expected = [0.023, 0.199, 0.398, 0.200, 0.020];
    for (knob, expected) in [0.0, 25.0, 50.0, 75.0, 100.0].iter().zip(expected) {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Sine,
            oscillator_a_level: 1.0,
            unison_voices: 1,
            stereo_width: 0.0,
            filter_wet: 0.0,
            attack: 0.5,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            amp_curve: (knob - 50.0) / 50.0,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 84, 127);
        let mut level = Vec::with_capacity(48_000);
        for _ in 0..48_000 {
            let frame = synth.next_stereo(48_000);
            level.push(f64::from(frame[0]).abs());
        }
        // 2 ms sliding peak: a 2 kHz sine fills every window.
        let window = 96;
        let envelope: Vec<f64> = (0..level.len())
            .map(|index| {
                let start = index.saturating_sub(window - 1);
                level[start..=index].iter().copied().fold(0.0, f64::max)
            })
            .collect();
        let peak = envelope.iter().copied().fold(0.0, f64::max);
        let crossing = |threshold: f64| {
            envelope
                .iter()
                .position(|value| *value >= threshold * peak)
                .expect("attack reaches the threshold")
        };
        let measured = (crossing(0.9) - crossing(0.1)) as f64 / 48_000.0;
        // The 20 ms extremes carry a ~3 ms analysis rise; judge them absolutely.
        let tolerance = if *knob == 0.0 || *knob == 100.0 {
            0.004
        } else {
            0.05 * expected
        };
        assert!(
            (measured - expected).abs() < tolerance,
            "curve {knob}: measured {measured:.4} s, reference {expected:.3} s"
        );
    }
}

#[test]
fn selectable_unison_stack_is_bounded_deterministic_and_audibly_distinct() {
    let single_patch = SynthPatch {
        unison_voices: 1,
        ..Default::default()
    };
    let mut wide_patch = single_patch.clone();
    wide_patch.unison_voices = 7;
    wide_patch.detune_cents = 32.0;
    wide_patch.stereo_width = 1.0;
    let mut single = OpenSynth::new(single_patch);
    let mut wide_a = OpenSynth::new(wide_patch.clone());
    let mut wide_b = OpenSynth::new(wide_patch);
    single.note_on(0, 60, 110);
    wide_a.note_on(0, 60, 110);
    wide_b.note_on(0, 60, 110);
    let single_render: Vec<_> = (0..1_024).map(|_| single.next_stereo(48_000)).collect();
    let wide_render: Vec<_> = (0..1_024).map(|_| wide_a.next_stereo(48_000)).collect();
    let duplicate: Vec<_> = (0..1_024).map(|_| wide_b.next_stereo(48_000)).collect();
    assert_ne!(single_render, wide_render);
    assert_eq!(wide_render, duplicate);
    assert!(
        wide_render
            .iter()
            .flatten()
            .all(|sample| sample.is_finite())
    );

    let mut bounded = SynthPatch::default();
    bounded.set_param("unison.voices", 0.0);
    assert_eq!(bounded.unison_voices, 1);
    bounded.set_param("unison.voices", 99.0);
    assert_eq!(bounded.unison_voices, MAX_UNISON_VOICES);
}

/// Steady-window amplitude of one frequency, Hann-windowed and scaled to a
/// full-scale sinusoid.
fn steady_amplitude(samples: &[[f32; 2]], frequency: f64) -> f64 {
    let start = 8_192;
    let length = 32_768;
    let (mut re, mut im, mut norm) = (0.0, 0.0, 0.0);
    for index in 0..length {
        let window = 0.5 - 0.5 * (TAU * index as f64 / length as f64).cos();
        let value = f64::from(samples[start + index][0]) * window;
        let angle = TAU * frequency * index as f64 / 48_000.0;
        re += value * angle.cos();
        im += value * angle.sin();
        norm += window;
    }
    2.0 * re.hypot(im) / norm
}

fn steady_harmonics(samples: &[[f32; 2]], f0: f64, harmonics: usize) -> Vec<f64> {
    (1..=harmonics)
        .map(|harmonic| steady_amplitude(samples, harmonic as f64 * f0))
        .collect()
}

fn render_unison_detuned(
    voices: usize,
    random_phase: f64,
    note: u8,
    detune_cents: f64,
) -> Vec<[f32; 2]> {
    let patch = SynthPatch {
        oscillator_a_level: 0.8,
        unison_voices: voices,
        detune_cents,
        stereo_width: 0.0,
        oscillator_random_phase: random_phase,
        oscillator_quality: 0,
        filter_wet: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..49_152).map(|_| synth.next_stereo(48_000)).collect()
}

fn render_unison_note(voices: usize, random_phase: f64, note: u8) -> Vec<[f32; 2]> {
    render_unison_detuned(voices, random_phase, note, 0.0)
}

fn render_unison(voices: usize, random_phase: f64) -> Vec<[f32; 2]> {
    render_unison_note(voices, random_phase, 48)
}

/// Stack power of the law: what a stack carries against one voice when its
/// voices beat apart, so the voices add as power.
fn unison_stack_power(count: usize, detune_cents: f64) -> f64 {
    let base = unison_voice_gain(count, detune_cents);
    (0..count)
        .map(|index| (base * unison_voice_weight(count, index)).powi(2))
        .sum()
}

/// Stack amplitude of the law: what a stack carries when its voices share a
/// phase, so the voices add as amplitude.
fn unison_stack_amplitude(count: usize, detune_cents: f64) -> f64 {
    let base = unison_voice_gain(count, detune_cents);
    (0..count)
        .map(|index| base * unison_voice_weight(count, index))
        .sum()
}

#[test]
fn unison_law_normalises_power_when_detuned_and_amplitude_when_coherent() {
    for count in UNISON_TABLE_COUNTS {
        let amplitude = unison_stack_amplitude(count, 0.0);
        assert!(
            (amplitude - 1.0).abs() < 0.01,
            "N={count} at zero detune: amplitude {amplitude:.4}, expected one voice"
        );
        for detune in [6.0, 12.0, 22.0] {
            let power_db = 10.0 * unison_stack_power(count, detune).log10();
            assert!(
                power_db.abs() < 0.5,
                "N={count} detune={detune}: power {power_db:+.2} dB against one voice"
            );
        }
    }
    // More voices are a sound, never a volume control.
    for pair in UNISON_TABLE_COUNTS.windows(2) {
        for detune in UNISON_TABLE_DETUNES {
            assert!(
                unison_voice_gain(pair[1], detune) < unison_voice_gain(pair[0], detune),
                "{} voices at {detune} cents are not quieter than {}",
                pair[1],
                pair[0]
            );
        }
    }
}

#[test]
fn unison_centre_voice_sits_below_the_outer_voices() {
    for count in [3usize, 4, 5, 6, 7, 8, 16] {
        let ratio = 20.0 * unison_voice_weight(count, count / 2).log10();
        assert!(
            (ratio + 2.45).abs() < 0.05,
            "N={count}: centre {ratio:.2} dB below outer, target -2.45 dB"
        );
    }
}

#[test]
fn one_unison_voice_stays_at_unity() {
    assert_eq!(unison_voice_gain(1, 0.0), 1.0);
    assert!(unison_voice_gain(1, 22.0) == 1.0);
    assert!(unison_voice_weights(1).iter().all(|weight| *weight == 1.0));
    assert!(unison_voice_weights(2).iter().all(|weight| *weight == 1.0));
}

fn steady_rms(samples: &[[f32; 2]]) -> f64 {
    let steady = &samples[48_000..];
    (steady
        .iter()
        .flat_map(|frame| frame.iter())
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / (steady.len() * 2) as f64)
        .sqrt()
}

/// One cell of the level harness: one oscillator, level 1.0, no filter, no
/// width, the patch trim pinned to 0 dB. Four seconds, the first second
/// discarded so the envelope has settled.
fn render_unison_cell(count: usize, detune_cents: f64, note: u8) -> Vec<[f32; 2]> {
    let patch = SynthPatch {
        oscillator_a_level: 1.0,
        unison_voices: count,
        detune_cents,
        stereo_width: 0.0,
        oscillator_quality: 0,
        filter_wet: 0.0,
        loudness_gain: 1.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, note, 110);
    (0..192_000).map(|_| synth.next_stereo(48_000)).collect()
}

/// The measured grid the level law is built from.
const UNISON_TABLE_COUNTS: [usize; 8] = [1, 2, 3, 4, 6, 8, 12, 16];
const UNISON_TABLE_DETUNES: [f64; 4] = [0.0, 6.0, 12.0, 22.0];

/// Reports the grid the level law is measured from: one oscillator at C5
/// with the level settings the table cites, each stack against one voice at
/// the same detune, and the per-voice gain that would land it on one voice.
///
/// C4 is measured too, because a law measured at one pitch has to travel.
/// The C5 cells are the table, so they also guard it against drift.
#[test]
#[ignore = "measurement: prints the unison level grid"]
fn measure_unison_level_table() {
    for note in [72u8, 60] {
        for detune in UNISON_TABLE_DETUNES {
            let single = steady_rms(&render_unison_cell(1, detune, note));
            for count in UNISON_TABLE_COUNTS {
                let stack = steady_rms(&render_unison_cell(count, detune, note));
                let measured_db = 20.0 * (stack / single).log10();
                let gain = unison_voice_gain(count, detune);
                let exact = gain / 10f64.powf(measured_db / 20.0);
                eprintln!(
                    "UNISON_TABLE note={note} detune={detune:5.1} count={count:2} \
                         gain={gain:.6} measured_db={measured_db:+.3} exact_gain={exact:.6}"
                );
                if note == 72 {
                    assert!(
                        (exact - gain).abs() < 0.002,
                        "N={count} detune={detune}: the table carries {gain:.6}, \
                             the render wants {exact:.6}"
                    );
                }
            }
        }
    }
    // Cells between the rows and columns: what the linear interpolation
    // costs a count and a spread the table does not hold.
    for (count, detune) in [(5usize, 9.0), (7, 15.0), (11, 9.0)] {
        let single = steady_rms(&render_unison_cell(1, detune, 72));
        let stack = steady_rms(&render_unison_cell(count, detune, 72));
        eprintln!(
            "UNISON_TABLE between count={count} detune={detune} db={:+.2}",
            20.0 * (stack / single).log10()
        );
    }
}

/// The measured grid, rendered: every stack in it carries one voice's level
/// at the pitch the table was measured at. An octave down the beats are half
/// as fast, so the same table lands within the wider bound instead.
#[test]
fn rendered_unison_grid_carries_one_voice() {
    for (note, tolerance_db) in [(72u8, 1.0), (60u8, 1.5)] {
        for detune in UNISON_TABLE_DETUNES {
            let single = steady_rms(&render_unison_cell(1, detune, note));
            for count in UNISON_TABLE_COUNTS {
                let stack = steady_rms(&render_unison_cell(count, detune, note));
                let level_db = 20.0 * (stack / single).log10();
                assert!(
                    level_db.abs() < tolerance_db,
                    "note {note}, N={count}, detune {detune}: stack {level_db:+.2} dB \
                         against one voice"
                );
            }
        }
    }
}

/// A cell the table does not hold: between two count rows and two detune
/// columns.
#[test]
fn rendered_unison_between_table_cells_stays_within_one_and_a_half_db() {
    for (count, detune) in [(5usize, 9.0), (7, 15.0), (11, 9.0)] {
        let single = steady_rms(&render_unison_cell(1, detune, 72));
        let stack = steady_rms(&render_unison_cell(count, detune, 72));
        let level_db = 20.0 * (stack / single).log10();
        assert!(
            level_db.abs() < 1.5,
            "N={count} at {detune} cents: stack {level_db:+.2} dB against one voice"
        );
    }
}

#[test]
fn unison_stack_is_coherent_at_zero_detune_and_power_sums_when_detuned() {
    // No detune means no drift to separate the voices, so an independent
    // start would only cancel them. The table puts the coherent sum on one
    // voice, which is what makes the stack usable at zero detune.
    let mut coherent = 0.0;
    for note in 48..64 {
        let f0 = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
        let single = steady_amplitude(&render_unison_note(1, 0.0, note), f0);
        let stack = steady_amplitude(&render_unison_note(7, 0.0, note), f0);
        coherent += stack / single;
    }
    coherent /= 16.0;
    assert!(
        (0.90..1.10).contains(&coherent),
        "seven voices at zero detune average {coherent:.3}x one voice, expected about one"
    );

    // A detuned layer starts its voices independently, so the steady
    // fundamental adds with power instead of amplitude.
    let mut spread = 0.0;
    for note in 48..64 {
        let f0 = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
        let single = steady_amplitude(&render_unison_note(1, 0.0, note), f0);
        let stack = steady_amplitude(&render_unison_detuned(7, 0.0, note, 17.0), f0);
        spread += stack / single;
    }
    spread /= 16.0;
    // A detuned stack carries one voice's power, but its voices are not in
    // phase, so the fundamental at f0 stays far below the coherent sum.
    assert!(
        (0.20..0.90).contains(&spread),
        "seven detuned voices average {spread:.3}x one voice at f0"
    );
}

#[test]
fn random_start_phase_amount_still_changes_the_stack() {
    let f0 = 440.0 * 2f64.powf((48.0 - 69.0) / 12.0);
    // The stack is dispersed at any jitter now, so compare the samples
    // rather than a coherent-vs-spread level: the control must still move
    // the phases it is meant to move.
    assert_ne!(
        render_unison(8, 0.0),
        render_unison(8, 1.0),
        "the random-phase amount must move the stack's start phase"
    );
    // The cancellation check needs a *detuned* stack: at zero detune the
    // voices are deliberately coherent (see the unison section of
    // docs/synth-character.md), so a shared start phase makes the harmonics
    // add coherently and not cancel, which is not what this asserts.
    let spread = steady_harmonics(&render_unison_detuned(8, 1.0, 48, 14.0), f0, 8);
    for harmonic in 2..=8 {
        let relative = 20.0 * (spread[harmonic - 1] / spread[0]).log10();
        assert!(
            relative > -30.0,
            "H{harmonic} cancelled at {relative:.1} dB with random phases"
        );
    }
}

#[test]
fn filter_wet_zero_bypasses_the_filter_and_one_applies_it() {
    let render = |cutoff: f64, wet: f64| {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Saw,
            oscillator_a_level: 0.8,
            filter_mode: FilterMode::LowPass,
            filter_cutoff: cutoff,
            filter_resonance: 0.0,
            filter_wet: wet,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 72, 100);
        (0..2_048)
            .map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let rms = |audio: &[[f32; 2]]| {
        (audio
            .iter()
            .flat_map(|frame| frame.iter())
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / (audio.len() * 2) as f64)
            .sqrt()
    };
    let dark_dry = render(200.0, 0.0);
    let open_dry = render(10_000.0, 0.0);
    let dark_wet = render(200.0, 1.0);
    let open_wet = render(10_000.0, 1.0);
    assert_eq!(dark_dry, open_dry, "wet=0 must ignore the cutoff");
    assert!(
        rms(&dark_wet) < rms(&open_wet) * 0.9,
        "wet=1 must attenuate a closed low-pass"
    );
    assert!(dark_wet.iter().flatten().all(|sample| sample.is_finite()));
}

#[test]
fn unison_coefficient_cache_tracks_physical_and_normalized_updates() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    assert_eq!(synth.unison_voice_count, 2);
    assert_eq!(synth.unison_positions[0], -1.0);
    assert_eq!(synth.unison_positions[1], 1.0);
    assert!((synth.unison_ratios[0] - 2f64.powf(-7.0 / 1200.0)).abs() < f64::EPSILON);
    assert!((synth.unison_ratios[1] - 2f64.powf(7.0 / 1200.0)).abs() < f64::EPSILON);

    synth.set_param("unison.voices", 7.0);
    synth.set_param("detune", 24.0);
    assert_eq!(synth.unison_voice_count, 7);
    assert_eq!(synth.unison_positions[3], 0.0);
    assert_eq!(synth.unison_ratios[3], 1.0);
    assert!((synth.unison_ratios[6] - 2f64.powf(24.0 / 1200.0)).abs() < f64::EPSILON);

    synth.set_normalized_param("unison.detune", 0.5);
    assert!((synth.patch.detune_cents - 50.0).abs() < f64::EPSILON);
    assert!((synth.unison_ratios[0] - 2f64.powf(-50.0 / 1200.0)).abs() < f64::EPSILON);
}

#[test]
fn oscillator_phase_controls_are_retriggered_and_seed_deterministic() {
    let patch = SynthPatch {
        oscillator_start_phase: 0.25,
        oscillator_random_phase: 1.0,
        ..Default::default()
    };
    let mut first = OpenSynth::new(patch.clone());
    let mut duplicate = OpenSynth::new(patch);
    first.note_on(2, 64, 100);
    duplicate.note_on(2, 64, 100);
    assert_eq!(first.voices[0].phases, duplicate.voices[0].phases);
    assert_ne!(first.voices[0].phases[0], 0.25);
    for _ in 0..512 {
        assert_eq!(first.next_stereo(48_000), duplicate.next_stereo(48_000));
    }

    let fixed = SynthPatch {
        oscillator_start_phase: 0.25,
        oscillator_random_phase: 0.0,
        sub_initial_phase: 0.5,
        unison_voices: 1,
        ..Default::default()
    };
    let mut fixed_synth = OpenSynth::new(fixed);
    fixed_synth.note_on(0, 60, 100);
    assert_eq!(fixed_synth.voices[0].phases[0], 0.25);
    assert!((fixed_synth.voices[0].phases[PHASE_B] - 0.25).abs() < 1e-12);
    // The sub follows its own SubOsc initial phase, not oscillator A's.
    assert!((fixed_synth.voices[0].phases[PHASE_SUB] - 0.5).abs() < 1e-12);
}

#[test]
fn unison_voices_start_at_independent_phases() {
    for count in 2..=MAX_UNISON_VOICES {
        let patch = SynthPatch {
            unison_voices: count,
            oscillator_start_phase: 0.0,
            oscillator_random_phase: 0.0,
            detune_cents: 12.0,
            ..Default::default()
        };
        let mut first = OpenSynth::new(patch.clone());
        let mut duplicate = OpenSynth::new(patch);
        first.note_on(0, 60, 100);
        duplicate.note_on(0, 60, 100);
        let phases = first.voices[0].phases;
        assert_eq!(phases, duplicate.voices[0].phases);
        for index in 0..count {
            assert!(
                (0.0..1.0).contains(&phases[index]),
                "N={count} voice {index} phase {}",
                phases[index]
            );
            for other in index + 1..count {
                assert!(
                    (phases[index] - phases[other]).abs() > 1e-6,
                    "N={count} voices {index} and {other} share a start phase"
                );
            }
        }
    }
}

#[test]
fn unison_phase_arrangement_is_independent_of_the_note() {
    let patch = SynthPatch {
        unison_voices: 7,
        oscillator_start_phase: 0.0,
        oscillator_random_phase: 0.3,
        detune_cents: 12.0,
        ..Default::default()
    };
    let offsets = |note: u8, velocity: u8| {
        let mut synth = OpenSynth::new(patch.clone());
        synth.note_on(0, note, velocity);
        let phases = synth.voices[0].phases;
        let first = phases[0];
        (0..7)
            .map(|index| (phases[index] - first).rem_euclid(1.0))
            .collect::<Vec<_>>()
    };
    let reference = offsets(48, 100);
    for (note, velocity) in [(36, 20), (55, 64), (60, 100), (72, 127)] {
        assert_eq!(
            offsets(note, velocity),
            reference,
            "note {note} velocity {velocity} must share the voice layout"
        );
    }
}

#[test]
fn near_coherent_stack_keeps_one_stereo_image_for_every_note() {
    let patch = SynthPatch {
        oscillator_a_level: 0.8,
        unison_voices: 7,
        detune_cents: 0.78,
        stereo_width: 0.7,
        oscillator_quality: 0,
        filter_wet: 0.0,
        ..Default::default()
    };
    let correlation = |note: u8| {
        let mut synth = OpenSynth::new(patch.clone());
        synth.note_on(0, note, 100);
        let rendered: Vec<_> = (0..108_000).map(|_| synth.next_stereo(48_000)).collect();
        channel_correlation(&rendered[12_000..])
    };
    let first = correlation(48);
    for note in [55, 60, 67] {
        let other = correlation(note);
        assert!(
            (other - first).abs() < 0.25,
            "note {note} correlations {other:.3} vs {first:.3}"
        );
    }
    assert!(
        first < 0.6,
        "a near-coherent stack must stay wide, correlation {first:.3}"
    );
}

#[test]
fn output_soft_clip_is_transparent_below_the_knee_and_bounded() {
    let knee = OUTPUT_CEILING_KNEE;
    for value in [-knee, -0.1, 0.0, 0.1, 0.3, knee] {
        assert_eq!(soft_clip(value), value);
    }
    let mut previous = knee;
    for value in [0.46, 0.5, 0.6, 1.0, 2.0, 10.0, 1e6] {
        let clipped = soft_clip(value);
        assert!(clipped >= previous && clipped <= OUTPUT_CEILING);
        assert_eq!(soft_clip(-value), -clipped);
        previous = clipped;
    }
    assert_eq!(soft_clip(1e9), OUTPUT_CEILING);
}

#[test]
fn appended_modulation_destinations_change_oscillator_b_and_filter_drive() {
    let render = |destination| {
        let mut patch = SynthPatch {
            oscillator_b_level: 0.7,
            ..Default::default()
        };
        patch.macros[0] = 1.0;
        patch.mod_routes[0] = ModRoute {
            source: ModSource::Macro1,
            destination,
            amount: 1.0,
            ..ModRoute::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..1_024)
            .map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let tuned = render(ModDestination::OscillatorBTuning);
    let driven = render(ModDestination::FilterDrive);
    assert_ne!(tuned, driven);
    assert!(
        tuned
            .iter()
            .chain(&driven)
            .flatten()
            .all(|sample| sample.is_finite())
    );
    assert_eq!(
        mod_destination_from_value(8.0),
        ModDestination::OscillatorBTuning
    );
    assert_eq!(mod_destination_from_value(9.0), ModDestination::FilterDrive);
}

#[test]
fn legacy_patch_json_with_sixteen_routes_and_no_env3_env4_extra_lfos_loads() {
    let mut expected = SynthPatch::default();
    expected.mod_routes[0] = ModRoute {
        source: ModSource::Lfo,
        destination: ModDestination::FilterCutoff,
        amount: 0.8,
        ..ModRoute::default()
    };
    let mut value = serde_json::to_value(&expected).unwrap();
    let patch = value.as_object_mut().unwrap();
    for key in [
        "env3_attack",
        "env3_decay",
        "env3_sustain",
        "env3_release",
        "env3_curve",
        "env4_attack",
        "env4_decay",
        "env4_sustain",
        "env4_release",
        "env4_curve",
        "extra_lfos",
    ] {
        patch.remove(key);
    }
    let legacy: SynthPatch = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.mod_routes.len(), 16);
    assert_eq!(legacy.extra_lfos, expected.extra_lfos);
    assert_eq!(legacy.env3_attack, expected.env3_attack);
    assert_eq!(legacy.env4_release, expected.env4_release);

    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..2_048)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    assert_eq!(render(expected), render(legacy));
}

#[test]
fn slot_64_params_extend_the_matrix_and_survive_json_round_trip() {
    let mut patch = SynthPatch::default();
    assert_eq!(patch.mod_routes.len(), 16);
    patch.set_param("mod.16.amount", 0.5);
    assert_eq!(patch.mod_routes.len(), 16);
    patch.set_param("mod.17.source", 20.0);
    assert_eq!(patch.mod_routes.len(), 64);
    assert_eq!(patch.mod_routes[16].source, ModSource::Env3);
    patch.set_param("mod.64.destination", 14.0);
    patch.set_param("mod.64.amount", 0.75);
    assert_eq!(
        patch.mod_routes[63].destination,
        ModDestination::Filter2Drive
    );
    assert_eq!(patch.mod_routes[63].amount, 0.75);
    patch.set_normalized_param("mod.64.amount", 0.25);
    assert_eq!(patch.mod_routes[63].amount, -0.5);

    let restored: SynthPatch =
        serde_json::from_str(&serde_json::to_string(&patch).unwrap()).unwrap();
    assert_eq!(restored.mod_routes.len(), 64);
    assert_eq!(restored.mod_routes[63].amount, -0.5);
    assert_eq!(restored.mod_routes[16].source, ModSource::Env3);
}

#[test]
fn lfo2_through_lfo10_are_independent_matrix_sources() {
    let render = |source: ModSource| {
        let mut patch = SynthPatch::default();
        patch.extra_lfos[0].rate = 2.3;
        patch.mod_routes[0] = ModRoute {
            source,
            destination: ModDestination::FilterCutoff,
            // Unipolar LFOs: a positive depth would clamp a high cutoff.
            amount: -0.2,
            ..ModRoute::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..2_048)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    assert_ne!(render(ModSource::Lfo), render(ModSource::Lfo2));
    assert_eq!(mod_source_from_value(11.0), ModSource::Lfo2);
    assert_eq!(mod_source_from_value(19.0), ModSource::Lfo10);
    assert_eq!(mod_source_from_value(20.0), ModSource::Env3);
    assert_eq!(mod_source_from_value(21.0), ModSource::Env4);

    let mut patch = SynthPatch::default();
    patch.extra_lfos[4].retrigger = true;
    let mut synth = OpenSynth::new(patch);
    for _ in 0..100 {
        synth.next_stereo(48_000);
    }
    assert_ne!(synth.extra_lfo_phases[4], 0.0);
    synth.note_on(0, 60, 100);
    assert_eq!(synth.extra_lfo_phases[4], 0.0);

    let mut synced = SynthPatch::default();
    synced.extra_lfos[0].sync = true;
    synced.extra_lfos[0].division = 4;
    let mut synth = OpenSynth::new_with_tempo(synced, 120.0);
    for _ in 0..24_000 {
        synth.next_stereo(48_000);
    }
    assert!((synth.extra_lfo_phases[0] - 0.25).abs() < 1e-9);

    let mut sampled = SynthPatch::default();
    sampled.extra_lfos[1].shape_type = LfoShapeType::RandomHold;
    sampled.extra_lfos[1].rate = 30.0;
    let mut iterated = OpenSynth::new(sampled.clone());
    let mut skipped = OpenSynth::new(sampled);
    for _ in 0..96_000 {
        iterated.next_stereo(48_000);
    }
    skipped.advance_idle(96_000, 48_000);
    assert!((iterated.extra_lfo_phases[1] - skipped.extra_lfo_phases[1]).abs() < 1e-9);
    assert_eq!(
        iterated.extra_lfo_random_states[1],
        skipped.extra_lfo_random_states[1]
    );
    assert_eq!(
        iterated.extra_lfo_hold_values[1],
        skipped.extra_lfo_hold_values[1]
    );
}

#[test]
fn env3_and_env4_feed_the_modulation_matrix() {
    let render = |source: ModSource, sustain: f64| {
        let mut patch = SynthPatch {
            env3_attack: 0.0,
            env3_decay: 0.0,
            env3_release: 0.0,
            env4_attack: 0.0,
            env4_decay: 0.0,
            env4_release: 0.0,
            env3_sustain: sustain,
            env4_sustain: sustain,
            ..Default::default()
        };
        patch.mod_routes[0] = ModRoute {
            source,
            destination: ModDestination::FilterCutoff,
            amount: 1.0,
            ..ModRoute::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..2_048)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    assert_ne!(
        render(ModSource::Env3, 1.0),
        render(ModSource::Env3, 0.0),
        "Env 3 must be able to drive a destination"
    );
    assert_ne!(
        render(ModSource::Env4, 1.0),
        render(ModSource::Env4, 0.0),
        "Env 4 must be able to drive a destination"
    );

    let patch = SynthPatch {
        env3_attack: 0.0,
        env3_decay: 0.0,
        env3_sustain: 1.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    for _ in 0..16 {
        synth.next_stereo(48_000);
    }
    assert_eq!(synth.voices[0].env3_level, 1.0);
}

#[test]
fn amp_envelope_is_available_as_a_matrix_source() {
    let voice = Voice::new(0, 60, 100, 0);
    let value = modulation_source(
        ModSource::AmpEnvelope,
        &[0.0; 10],
        &[0.42, 0.1, 0.2, 0.3],
        &voice,
        &[0.0; 4],
        &[0.0; 4],
        0.0,
        0.0,
        0.0,
    );
    assert_eq!(value, 0.42);
    assert_eq!(
        mod_source_from_value(ModSource::AmpEnvelope as usize as f64),
        ModSource::AmpEnvelope
    );
}

#[test]
fn oscillator_level_routes_add_in_the_amplitude_domain() {
    // A level route at `amount` adds to sqrt(base), so the gain is the
    // square of the sum: base 0.25 + 50 % -> 1.0, base 0 + 50 % -> 0.25.
    let rms = |base: f64, macro_value: f64, routed: bool| {
        let mut patch = SynthPatch {
            filter_wet: 0.0,
            oscillator_a_waveform: Waveform::Sine,
            ..SynthPatch::default()
        };
        patch.oscillator_a_level = base;
        patch.oscillator_b_level = 0.0;
        patch.oscillator_c_level = 0.0;
        patch.noise_level = 0.0;
        patch.attack = 0.0;
        patch.decay = 0.0;
        patch.sustain = 1.0;
        patch.macros[0] = macro_value;
        if routed {
            patch.mod_routes[0] = ModRoute {
                source: ModSource::Macro1,
                destination: ModDestination::OscillatorALevel,
                amount: 1.0,
                ..ModRoute::default()
            };
        }
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 110);
        let samples: Vec<f32> = (0..8_192).flat_map(|_| synth.next_stereo(48_000)).collect();
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    };
    let routed_half = rms(0.25, 0.5, true);
    let stored_full = rms(1.0, 0.0, false);
    assert!(
        (routed_half - stored_full).abs() < 1e-4 * stored_full,
        "route at 50 % from 0.25 must reach unity: {routed_half} vs {stored_full}"
    );
    let routed_from_zero = rms(0.0, 0.5, true);
    let stored_quarter = rms(0.25, 0.0, false);
    assert!(
        (routed_from_zero - stored_quarter).abs() < 1e-4 * stored_quarter,
        "route at 50 % from zero must reach 0.25: {routed_from_zero} vs {stored_quarter}"
    );
}

#[test]
fn appended_modulation_destinations_change_oscillator_c_and_filter2() {
    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 55, 110);
        (0..2_048)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let muted = render(SynthPatch::default());
    let mut c_level = SynthPatch::default();
    c_level.macros[0] = 1.0;
    c_level.mod_routes[0] = ModRoute {
        source: ModSource::Macro1,
        destination: ModDestination::OscillatorCLevel,
        amount: 1.0,
        ..ModRoute::default()
    };
    let c_level_audio = render(c_level);
    assert_ne!(muted, c_level_audio);

    let mut c_position = SynthPatch {
        oscillator_c_level: 0.6,
        oscillator_c_waveform: Waveform::Wavetable,
        oscillator_c_wavetable: Some(CustomWavetable {
            samples: Arc::new(
                (0..2 * WAVETABLE_SIZE)
                    .map(|index| {
                        let phase = (index % WAVETABLE_SIZE) as f64 / WAVETABLE_SIZE as f64;
                        if index < WAVETABLE_SIZE {
                            (TAU * phase).sin()
                        } else {
                            2.0 * phase - 1.0
                        }
                    })
                    .collect(),
            ),
            frame_size: WAVETABLE_SIZE,
            frame_count: 2,
            imported_readout: false,
            mips: Default::default(),
        }),
        ..Default::default()
    };
    let c_position_base = render(c_position.clone());
    c_position.macros[0] = 1.0;
    c_position.mod_routes[0] = ModRoute {
        source: ModSource::Macro1,
        destination: ModDestination::WavetablePositionC,
        amount: 1.0,
        ..ModRoute::default()
    };
    let c_position_audio = render(c_position);
    assert_ne!(c_position_base, c_position_audio);

    let filter2_base = SynthPatch {
        filter2_enabled: true,
        filter2_cutoff: 700.0,
        filter2_resonance: 0.1,
        ..Default::default()
    };
    let filter2_base_audio = render(filter2_base.clone());
    for (destination, mut patch) in [
        (ModDestination::Filter2Cutoff, filter2_base.clone()),
        (ModDestination::Filter2Resonance, filter2_base.clone()),
        (ModDestination::Filter2Drive, filter2_base),
    ] {
        patch.macros[0] = 1.0;
        patch.mod_routes[0] = ModRoute {
            source: ModSource::Macro1,
            destination,
            amount: 1.0,
            ..ModRoute::default()
        };
        let audio = render(patch);
        assert_ne!(
            filter2_base_audio, audio,
            "{destination:?} must change filter 2 output"
        );
        assert!(audio.iter().all(|sample| sample.is_finite()));
    }
    assert_eq!(
        mod_destination_from_value(10.0),
        ModDestination::OscillatorCLevel
    );
    assert_eq!(
        mod_destination_from_value(11.0),
        ModDestination::WavetablePositionC
    );
    assert_eq!(
        mod_destination_from_value(12.0),
        ModDestination::Filter2Cutoff
    );
    assert_eq!(
        mod_destination_from_value(13.0),
        ModDestination::Filter2Resonance
    );
    assert_eq!(
        mod_destination_from_value(14.0),
        ModDestination::Filter2Drive
    );
}

#[test]
fn global_tuning_is_bounded_deterministic_and_changes_pitch() {
    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 69, 100);
        (0..1_024)
            .map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let concert = render(SynthPatch::default());
    let orchestral_patch = SynthPatch {
        tuning_reference_hz: 442.0,
        tuning_transpose: 12.0,
        tuning_fine_cents: -14.0,
        ..Default::default()
    };
    let orchestral = render(orchestral_patch.clone());
    assert_ne!(concert, orchestral);
    assert_eq!(orchestral, render(orchestral_patch));

    let mut bounded = SynthPatch::default();
    bounded.set_param("tuning.reference_hz", 999.0);
    bounded.set_param("tuning.transpose", -99.0);
    bounded.set_param("tuning.fine", 500.0);
    assert_eq!(bounded.tuning_reference_hz, 480.0);
    assert_eq!(bounded.tuning_transpose, -24.0);
    assert_eq!(bounded.tuning_fine_cents, 100.0);
}

#[test]
fn oscillator_c_params_round_trip_and_render_at_level() {
    let mut patch = SynthPatch::default();
    patch.set_param("osc.c.waveform", 1.0);
    patch.set_param("osc.c.level", 2.0);
    patch.set_param("osc.c.semitones", -7.0);
    patch.set_param("osc.c.pulse_width", 0.3);
    patch.set_param("osc.c.wavetable_position", 0.4);
    patch.set_param("filter2.enabled", 1.0);
    patch.set_param("filter2.cutoff", 900.0);
    patch.set_param("filter2.resonance", 0.6);
    patch.set_param("filter2.drive", 0.2);
    patch.set_param("filter2.mode", 1.0);
    patch.set_param("filter.routing", 1.0);
    assert_eq!(patch.oscillator_c_waveform, Waveform::Saw);
    assert_eq!(patch.oscillator_c_level, 1.0);
    assert_eq!(patch.oscillator_c_semitones, -7.0);
    assert_eq!(patch.oscillator_c_pulse_width, 0.3);
    assert_eq!(patch.oscillator_c_wavetable_position, 0.4);
    assert!(patch.filter2_enabled);
    assert_eq!(patch.filter2_cutoff, 900.0);
    assert_eq!(patch.filter2_resonance, 0.6);
    assert_eq!(patch.filter2_drive, 0.2);
    assert_eq!(patch.filter2_mode, FilterMode::BandPass);
    assert_eq!(patch.filter_routing, FilterRouting::Parallel);
    patch.set_normalized_param("filter2.cutoff", 0.5);
    assert!((patch.filter2_cutoff - 20.0 * 1000f64.sqrt()).abs() < 1e-9);
    patch.set_normalized_param("filter.routing", 0.0);
    assert_eq!(patch.filter_routing, FilterRouting::Serial);

    let render = |level: f64| {
        let patch = SynthPatch {
            oscillator_c_waveform: Waveform::Sine,
            oscillator_c_semitones: 12.0,
            oscillator_c_level: level,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 110);
        (0..4_096)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let quiet = render(0.0);
    let loud = render(1.0);
    let rms = |samples: &[f32]| {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    };
    assert!(quiet.iter().all(|s| s.is_finite()));
    assert!(loud.iter().all(|s| s.is_finite()));
    assert!(rms(&quiet) > 0.0);
    assert!(rms(&loud) > rms(&quiet) * 1.1);
    assert_ne!(quiet, loud);
}

#[test]
fn warp_modes_are_deterministic_finite_and_change_the_signal() {
    let render = |waveform: Waveform, mode: WarpMode, amount: f64| {
        let patch = SynthPatch {
            oscillator_a_waveform: waveform,
            oscillator_a_warp_mode: mode,
            oscillator_a_warp_amount: amount,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 57, 112);
        (0..4_096)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let off = render(Waveform::Saw, WarpMode::Off, 0.0);
    assert!(off.iter().all(|sample| sample.is_finite()));
    assert!(off.iter().any(|sample| *sample != 0.0));
    for mode in [
        WarpMode::Sync,
        WarpMode::BendPos,
        WarpMode::Pwm,
        WarpMode::AsymPos,
        WarpMode::Flip,
        WarpMode::Dlm,
        WarpMode::Quantize,
        WarpMode::EvenOdd,
        WarpMode::DistTube,
        WarpMode::SelfPd,
        WarpMode::RmNoise,
    ] {
        let warped = render(Waveform::Saw, mode, 0.85);
        assert!(
            warped.iter().all(|sample| sample.is_finite()),
            "{mode:?} produced non-finite audio"
        );
        assert_ne!(off, warped, "{mode:?} must change the waveform");
        assert_eq!(
            warped,
            render(Waveform::Saw, mode, 0.85),
            "{mode:?} must render deterministically"
        );
    }
}

#[test]
fn warp_applies_to_oscillators_b_and_c() {
    let render = |oscillator: usize, mode: WarpMode| {
        let mut patch = SynthPatch {
            gain: 1.0,
            oscillator_a_level: 0.0,
            oscillator_b_level: 0.0,
            oscillator_c_level: 0.0,
            oscillator_a_waveform: Waveform::Saw,
            oscillator_b_waveform: Waveform::Saw,
            oscillator_c_waveform: Waveform::Saw,
            ..Default::default()
        };
        match oscillator {
            0 => {
                patch.oscillator_a_level = 0.8;
                patch.oscillator_a_warp_mode = mode;
                patch.oscillator_a_warp_amount = 0.8;
            }
            1 => {
                patch.oscillator_b_level = 0.8;
                patch.oscillator_b_warp_mode = mode;
                patch.oscillator_b_warp_amount = 0.8;
            }
            _ => {
                patch.oscillator_c_level = 0.8;
                patch.oscillator_c_warp_mode = mode;
                patch.oscillator_c_warp_amount = 0.8;
            }
        }
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 57, 112);
        (0..4_096)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    for oscillator in 0..3 {
        let off = render(oscillator, WarpMode::Off);
        let sync = render(oscillator, WarpMode::Sync);
        assert!(off.iter().all(|sample| sample.is_finite()));
        assert!(sync.iter().all(|sample| sample.is_finite()));
        assert!(off.iter().any(|sample| *sample != 0.0));
        assert_ne!(
            off, sync,
            "oscillator {oscillator} must respond to its own warp settings"
        );
    }
}

#[test]
fn warp_params_round_trip_through_physical_and_normalized_paths() {
    let mut patch = SynthPatch::default();
    patch.set_param("osc.a.warp_mode", 1.0);
    patch.set_param("osc.a.warp_amount", 0.42);
    patch.set_param("osc.b.warp_mode", 2.0);
    patch.set_param("osc.b.warp_amount", 1.7);
    patch.set_normalized_param("osc.c.warp_mode", 0.4);
    patch.set_normalized_param("osc.c.warp_amount", 0.65);
    patch.set_param("osc.a.warp_var", 0.3);
    patch.set_param("osc.a.warp2_mode", 57.0);
    patch.set_param("osc.a.warp2_amount", 0.8);
    patch.set_param("osc.a.warp2_var", 0.6);
    assert_eq!(patch.oscillator_a_warp_mode, WarpMode::Sync);
    assert_eq!(patch.oscillator_a_warp_amount, 0.42);
    assert_eq!(patch.oscillator_b_warp_mode, WarpMode::BendPos);
    assert_eq!(patch.oscillator_b_warp_amount, 1.0);
    assert_eq!(patch.oscillator_c_warp_mode, WarpMode::DistRectify);
    assert!((patch.oscillator_c_warp_amount - 0.65).abs() < 1e-12);
    assert_eq!(patch.oscillator_a_warp_var, 0.3);
    assert_eq!(patch.oscillator_a_warp2_mode, WarpMode::SelfPd);
    assert_eq!(patch.oscillator_a_warp2_amount, 0.8);
    assert_eq!(patch.oscillator_a_warp2_var, 0.6);
    let params = vec![
        (
            "osc.a.warp_mode".to_string(),
            oc_dsl::ast::ParamValue::Number(5.0),
        ),
        (
            "osc.a.warp_amount".to_string(),
            oc_dsl::ast::ParamValue::Number(0.25),
        ),
    ];
    patch.apply_params(Some(&params));
    assert_eq!(patch.oscillator_a_warp_mode, WarpMode::Pwm);
    assert_eq!(patch.oscillator_a_warp_amount, 0.25);
}

#[test]
fn serial_and_parallel_dual_filter_paths_are_finite_and_distinct() {
    let render = |routing: FilterRouting| {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Saw,
            oscillator_b_level: 0.25,
            oscillator_c_level: 0.25,
            filter_cutoff: 3_200.0,
            filter_resonance: 0.4,
            filter2_enabled: true,
            filter2_cutoff: 700.0,
            filter2_resonance: 0.65,
            filter2_mode: FilterMode::LowPass,
            filter_routing: routing,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 57, 112);
        (0..4_096)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let serial = render(FilterRouting::Serial);
    let parallel = render(FilterRouting::Parallel);
    assert!(serial.iter().all(|s| s.is_finite()));
    assert!(parallel.iter().all(|s| s.is_finite()));
    assert!(serial.iter().any(|s| *s != 0.0));
    assert_ne!(serial, parallel);
}

#[test]
fn idle_span_advance_matches_sample_iteration_including_sample_and_hold() {
    let patch = SynthPatch {
        lfo_shape_type: LfoShapeType::RandomHold,
        lfo_rate: 30.0,
        step_sync: false,
        step_rate: 13.0,
        ..Default::default()
    };
    let mut iterated = OpenSynth::new(patch.clone());
    let mut skipped = OpenSynth::new(patch);
    for _ in 0..96_000 {
        iterated.next_stereo(48_000);
    }
    skipped.advance_idle(96_000, 48_000);
    assert!((iterated.lfo_phase - skipped.lfo_phase).abs() < 1e-9);
    assert!((iterated.step_phase - skipped.step_phase).abs() < 1e-9);
    assert_eq!(iterated.lfo_random_state, skipped.lfo_random_state);
    assert_eq!(iterated.lfo_hold_value, skipped.lfo_hold_value);
}

#[test]
fn idle_span_keeps_chaos_advancing() {
    let patch = SynthPatch {
        lfo_shape_type: LfoShapeType::ChaosA,
        lfo_rate: 7.0,
        ..Default::default()
    };
    let mut iterated = OpenSynth::new(patch.clone());
    let mut skipped = OpenSynth::new(patch);
    for _ in 0..96_000 {
        iterated.next_stereo(48_000);
    }
    skipped.advance_idle(96_000, 48_000);
    // Both paths advance 14 cycles = 448 map steps; only rounding differs.
    let fraction_delta =
        (iterated.lfo_runtime[0].chaos_frac - skipped.lfo_runtime[0].chaos_frac).rem_euclid(1.0);
    assert!(fraction_delta.min(1.0 - fraction_delta) < 1e-6);
    for synth in [&iterated, &skipped] {
        let output = synth.lfo_runtime[0].output;
        assert!(output.is_finite() && (0.0..=1.0).contains(&output));
    }
    let fresh = OpenSynth::new(SynthPatch {
        lfo_shape_type: LfoShapeType::ChaosA,
        ..Default::default()
    });
    assert_ne!(
        iterated.lfo_runtime[0].chaos_curr,
        fresh.lfo_runtime[0].chaos_curr
    );
}

#[test]
fn modulation_route_cache_tracks_patch_and_automation_updates() {
    let mut patch = SynthPatch::default();
    patch.mod_routes[3].amount = 0.5;
    patch.mod_routes[12].amount = -0.25;
    let mut synth = OpenSynth::new(patch);
    assert_eq!(synth.active_mod_route_count, 2);
    assert_eq!(&synth.active_mod_routes[..2], &[3, 12]);
    synth.set_param("mod.4.amount", 0.0);
    assert_eq!(&synth.active_mod_routes[..1], &[12]);
    synth.set_normalized_param("mod.1.amount", 1.0);
    assert_eq!(&synth.active_mod_routes[..2], &[0, 12]);
    assert!(!synth.oscillator_b_tuning_modulated);
    synth.set_param("mod.1.destination", 8.0);
    assert!(synth.oscillator_b_tuning_modulated);
    synth.set_param("mod.1.amount", 0.0);
    assert!(!synth.oscillator_b_tuning_modulated);
}

#[test]
fn inactive_mod_route_fields_keep_the_legacy_json_shape() {
    let value = serde_json::to_value(ModRoute::default()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    let route = ModRoute {
        bipolar: true,
        auxiliary: Some(ModSource::Macro6),
        auxiliary_inverted: true,
        ..ModRoute::default()
    };
    let value = serde_json::to_value(route).unwrap();
    assert_eq!(value["bipolar"], true);
    assert_eq!(value["auxiliary"], "Macro6");
    assert_eq!(value["auxiliary_inverted"], true);
}

#[test]
fn auxiliary_source_scales_and_inverts_the_route() {
    let mut sources = [0.0; MOD_SOURCE_TABLE.len()];
    sources[ModSource::Lfo5 as usize] = 1.0;
    sources[ModSource::Macro6 as usize] = 0.0;
    let route = CompiledModRoute {
        source: ModSource::Lfo5 as u8,
        auxiliary: Some(ModSource::Macro6 as u8),
        auxiliary_inverted: false,
        bipolar: false,
        destination: ModDestination::FilterCutoff,
        amount: 0.6,
        curve: 0.0,
        slew_up_seconds: 0.0,
        slew_down_seconds: 0.0,
        slew_up: 1.0,
        slew_down: 1.0,
    };
    assert_eq!(route.value(&sources), 0.0);
    sources[ModSource::Macro6 as usize] = 0.5;
    assert!((route.value(&sources) - 0.3).abs() < 1e-12);
    sources[ModSource::Macro6 as usize] = 1.0;
    assert!((route.value(&sources) - 0.6).abs() < 1e-12);

    let inverted = CompiledModRoute {
        auxiliary_inverted: true,
        ..route
    };
    assert!((inverted.value(&sources) + 0.6).abs() < 1e-12);
}

#[test]
fn bipolar_route_centres_a_unipolar_source() {
    let mut sources = [0.0; MOD_SOURCE_TABLE.len()];
    let unipolar = CompiledModRoute {
        source: ModSource::Macro1 as u8,
        auxiliary: None,
        auxiliary_inverted: false,
        bipolar: false,
        destination: ModDestination::FilterCutoff,
        amount: 0.5,
        curve: 0.0,
        slew_up_seconds: 0.0,
        slew_down_seconds: 0.0,
        slew_up: 1.0,
        slew_down: 1.0,
    };
    let bipolar = CompiledModRoute {
        bipolar: true,
        ..unipolar
    };
    sources[ModSource::Macro1 as usize] = 0.5;
    assert!((unipolar.value(&sources) - 0.25).abs() < 1e-12);
    assert_eq!(bipolar.value(&sources), 0.0);
    sources[ModSource::Macro1 as usize] = 0.0;
    assert!((bipolar.value(&sources) + 0.5).abs() < 1e-12);
    sources[ModSource::Macro1 as usize] = 1.0;
    assert!((bipolar.value(&sources) - 0.5).abs() < 1e-12);

    // LFOs are unipolar sources too, so the flag re-centres them.
    sources[ModSource::Lfo5 as usize] = 0.5;
    let lfo = CompiledModRoute {
        source: ModSource::Lfo5 as u8,
        auxiliary: None,
        bipolar: true,
        amount: 0.5,
        ..unipolar
    };
    assert_eq!(lfo.value(&sources), 0.0);
    let centred = CompiledModRoute {
        source: ModSource::Lfo5 as u8,
        auxiliary: None,
        bipolar: false,
        amount: 0.5,
        ..unipolar
    };
    assert!((centred.value(&sources) - 0.25).abs() < 1e-12);
    sources[ModSource::Lfo5 as usize] = 0.0;
    assert!((lfo.value(&sources) + 0.5).abs() < 1e-12);
}

#[test]
fn pad_lfo5_x_macro6_route_is_silent_at_zero_macro() {
    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..4_096)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let mut patch = SynthPatch {
        filter_cutoff: 800.0,
        ..Default::default()
    };
    patch.extra_lfos[4].rate = 3.0;
    patch.mod_routes[1] = ModRoute {
        source: ModSource::Lfo5,
        destination: ModDestination::FilterCutoff,
        amount: 0.602_443_665_266_037,
        auxiliary: Some(ModSource::Macro6),
        ..ModRoute::default()
    };
    let silent = render(patch.clone());
    let mut without_route = patch.clone();
    without_route.mod_routes[1] = ModRoute::default();
    assert_eq!(silent, render(without_route));

    patch.extra_macros[1] = 1.0;
    let open = render(patch);
    assert_ne!(silent, open);
}

#[test]
fn oscillator_b_ratio_cache_tracks_physical_and_normalized_updates() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    assert_eq!(synth.oscillator_b_ratio, 1.0);
    synth.set_param("osc.b.semitones", 0.07); // Seven cents, not seven semitones.
    assert!((synth.oscillator_b_ratio - 2.0_f64.powf(7.0 / 1200.0)).abs() < 1e-12);
    synth.set_param("osc.b.semitones", 12.0);
    assert!((synth.oscillator_b_ratio - 2.0).abs() < f64::EPSILON);
    synth.set_normalized_param("osc.b.semitones", 0.0);
    assert!((synth.oscillator_b_ratio - 0.25).abs() < f64::EPSILON);
}

#[test]
fn static_voice_frequency_cache_invalidates_for_pitch_controls() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 69, 100);
    assert!(!synth.pitch_modulated);
    assert!(!synth.voices[0].frequency_valid);
    synth.next_stereo(48_000);
    assert!(synth.voices[0].frequency_valid);
    assert!((synth.voices[0].frequency_hz - 440.0).abs() < 1e-12);

    synth.set_pitch_bend(0, 4096);
    assert!(!synth.voices[0].frequency_valid);
    synth.next_stereo(48_000);
    assert!(synth.voices[0].frequency_hz > 440.0);

    synth.set_note_expression(0, 69, "tuning", -0.5);
    assert!(!synth.voices[0].frequency_valid);
    synth.set_param("tuning.reference_hz", 442.0);
    synth.next_stereo(48_000);
    assert!(synth.voices[0].frequency_valid);

    synth.set_param("lfo.pitch", 12.0);
    assert!(synth.pitch_modulated);
    synth.set_param("lfo.pitch", 0.0);
    assert!(!synth.pitch_modulated);
    synth.set_param("mod.1.destination", 1.0);
    synth.set_param("mod.1.amount", 0.5);
    assert!(synth.pitch_modulated);
    synth.set_normalized_param("mod.1.amount", 0.5);
    assert!(!synth.pitch_modulated);
}

#[test]
fn static_filter_coefficients_cache_and_dynamic_controls_refresh() {
    let mut synth = OpenSynth::new(SynthPatch::default());
    synth.note_on(0, 60, 100);
    assert!(!synth.filter_modulated);
    synth.next_stereo(48_000);
    assert!(synth.voices[0].filter_coefficients_valid);
    assert_eq!(synth.voices[0].filter_sample_rate, 48_000);
    let initial_g = synth.voices[0].filter_coefficients.g;
    for _ in 0..16 {
        synth.next_stereo(48_000);
    }
    assert_eq!(synth.voices[0].filter_coefficients.g, initial_g);

    synth.set_param("filter.cutoff", 2_000.0);
    assert!(!synth.voices[0].filter_coefficients_valid);
    synth.next_stereo(48_000);
    assert_ne!(synth.voices[0].filter_coefficients.g, initial_g);
    let lower_g = synth.voices[0].filter_coefficients.g;
    synth.next_stereo(44_100);
    assert_eq!(synth.voices[0].filter_sample_rate, 44_100);
    assert_ne!(synth.voices[0].filter_coefficients.g, lower_g);

    synth.set_channel_pressure(0, 64);
    assert!(!synth.voices[0].filter_coefficients_valid);
    synth.set_poly_pressure(0, 60, 32);
    synth.set_note_expression(0, 60, "brightness", 0.8);
    synth.next_stereo(48_000);
    assert!(synth.voices[0].filter_coefficients_valid);

    synth.set_param("filter.env.amount", 2.0);
    assert!(synth.filter_modulated);
    synth.next_stereo(48_000);
    assert_eq!(synth.voices[0].filter_coefficient_age, 31);
    synth.next_stereo(48_000);
    assert_eq!(synth.voices[0].filter_coefficient_age, 30);
    synth.set_param("filter.env.amount", 0.0);
    assert!(!synth.filter_modulated);
    assert!(!synth.voices[0].filter_coefficients_valid);

    synth.set_param("mod.1.destination", 0.0);
    synth.set_param("mod.1.amount", 0.25);
    assert!(synth.filter_modulated);
}

#[test]
fn sparse_track_skip_is_sample_identical_to_naive_iteration() {
    let on = MidiEvent {
        time_samples: 24_000,
        event: MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
        track_index: 0,
    };
    let off = MidiEvent {
        time_samples: 30_000,
        event: MidiMessage::NoteOff {
            channel: 0,
            note: 60,
        },
        track_index: 0,
    };
    let events = vec![&on, &off];
    let optimized = render_track(&events, 48_000, 48_000, None);
    let mut naive = vec![0.0; 96_000];
    let mut synth = OpenSynth::new(SynthPatch::default());
    for frame in 0..48_000 {
        if frame == 24_000 {
            synth.note_on(0, 60, 100);
        }
        if frame == 30_000 {
            synth.note_off(0, 60);
        }
        let [left, right] = synth.next_stereo(48_000);
        naive[frame * 2] = left;
        naive[frame * 2 + 1] = right;
    }
    assert_eq!(optimized, naive);
}

fn render_frames(synth: &mut OpenSynth, frames: usize) -> Vec<f32> {
    let mut output = vec![0.0; frames * 2];
    synth.render_interleaved(&mut output, 48_000);
    output
}

#[test]
fn empty_and_disabled_fx_chain_are_bit_identical_to_no_fx() {
    let mut without = OpenSynth::new(SynthPatch::default());
    without.note_on(0, 60, 100);

    let empty_patch = SynthPatch {
        fx: Vec::new(),
        ..Default::default()
    };
    let mut empty = OpenSynth::new(empty_patch);
    empty.note_on(0, 60, 100);

    let mut disabled_patch = SynthPatch::default();
    let mut delay = SynthFx::new(SynthFxType::Delay);
    delay.enabled = false;
    let mut reverb = SynthFx::new(SynthFxType::Reverb);
    reverb.enabled = false;
    disabled_patch.fx = vec![delay, reverb];
    let mut disabled = OpenSynth::new(disabled_patch);
    disabled.note_on(0, 60, 100);

    let baseline = render_frames(&mut without, 4_800);
    assert_eq!(baseline, render_frames(&mut empty, 4_800));
    assert_eq!(baseline, render_frames(&mut disabled, 4_800));
}

#[test]
fn master_gain_after_the_rack_does_not_change_the_compressor_input() {
    let render = |gain: f64| {
        let mut patch = SynthPatch {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            filter_wet: 0.0,
            filter_cutoff: 20_000.0,
            velocity_to_amp: 0.0,
            gain,
            ..SynthPatch::default()
        };
        patch.oscillator_a_level = 0.2;
        let mut compressor = SynthFx::new(SynthFxType::Compressor);
        compressor.set_param("threshold", -45.0);
        compressor.set_param("ratio", 4.0);
        compressor.set_param("attack", 0.1);
        compressor.set_param("release", 100.0);
        compressor.set_param("makeup", 0.0);
        patch.fx = vec![compressor];
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 127);
        let samples = render_frames(&mut synth, 24_000);
        (samples
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    };
    let unity = render(1.0);
    let quiet = render(0.125);
    // A pre-rack master would move the compressor to a different part of its
    // curve; a post-rack master scales the same signal by 1/8.
    assert!(
        (quiet / unity - 0.125).abs() < 1.0e-4,
        "master changed the compressor input: {unity} {quiet}"
    );
}

#[test]
fn drive_changes_output_without_breaking_finiteness() {
    let mut clean = OpenSynth::new(SynthPatch::default());
    let patch = SynthPatch {
        fx: vec![SynthFx::new(SynthFxType::Drive)],
        ..Default::default()
    };
    let mut driven = OpenSynth::new(patch);
    clean.note_on(0, 60, 100);
    driven.note_on(0, 60, 100);
    let baseline = render_frames(&mut clean, 4_800);
    let processed = render_frames(&mut driven, 4_800);
    assert!(processed.iter().all(|sample| sample.is_finite()));
    assert!(
        processed
            .iter()
            .zip(&baseline)
            .any(|(left, right)| left != right)
    );
}

#[test]
fn delay_tail_is_finite_and_decays_to_silence() {
    let mut patch = SynthPatch {
        release: 0.05,
        ..Default::default()
    };
    let mut delay = SynthFx::new(SynthFxType::Delay);
    delay.set_param("time", 0.03);
    delay.set_param("feedback", 0.2);
    delay.set_param("mix", 0.5);
    patch.fx = vec![delay];
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut output = render_frames(&mut synth, 4_800);
    synth.note_off(0, 60);
    output.extend(render_frames(&mut synth, 48_000 * 4));
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(output.iter().any(|sample| sample.abs() > 1e-4));
    let tail = &output[output.len() - 48_000..];
    assert!(tail.iter().all(|sample| *sample == 0.0));
    assert!(synth.is_idle());
}

#[test]
fn fx_params_roundtrip_through_apply_params_and_json() {
    use oc_dsl::ast::ParamValue;
    let params = vec![
        ("fx.1.type".to_string(), ParamValue::String("delay".into())),
        ("fx.1.enabled".to_string(), ParamValue::Number(0.0)),
        ("fx.1.time".to_string(), ParamValue::Number(0.4)),
        ("fx.1.feedback".to_string(), ParamValue::Number(0.6)),
        ("fx.2.type".to_string(), ParamValue::Number(4.0)),
        ("fx.2.size".to_string(), ParamValue::Number(0.75)),
    ];
    let mut patch = SynthPatch::default();
    patch.apply_params(Some(&params));
    assert_eq!(patch.fx.len(), 2);
    assert_eq!(patch.fx[0].fx_type, SynthFxType::Delay);
    assert!(!patch.fx[0].enabled);
    assert!((patch.fx[0].param("time").unwrap() - 0.4).abs() < 1e-9);
    assert!((patch.fx[0].param("feedback").unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(patch.fx[1].fx_type, SynthFxType::Reverb);
    assert!((patch.fx[1].param("size").unwrap() - 0.75).abs() < 1e-9);
    let json = serde_json::to_vec(&patch).unwrap();
    let restored: SynthPatch = serde_json::from_slice(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
    // Removing all fx.* keys removes the modules on the next param apply.
    let cleared = vec![("fx.1.type".to_string(), ParamValue::Number(0.0))];
    let mut patch = restored;
    patch.fx.clear();
    patch.apply_params(Some(&cleared));
    assert_eq!(patch.fx.len(), 1);
    assert_eq!(patch.fx[0].fx_type, SynthFxType::Drive);

    // An explicit zero count clears a rack imported from a state file
    // even when the editor removed every other fx.* key.
    let mut patch = SynthPatch {
        fx: vec![SynthFx::new(SynthFxType::Delay)],
        ..Default::default()
    };
    patch.apply_params(Some(&vec![(
        "fx.count".to_string(),
        ParamValue::Number(0.0),
    )]));
    assert!(patch.fx.is_empty());
}

#[test]
fn per_oscillator_pan_attenuates_only_the_opposite_channel() {
    let render = |pan: f64| {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Sine,
            oscillator_a_level: 0.8,
            unison_voices: 1,
            oscillator_a_pan: pan,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 69, 100);
        (0..4_096)
            .map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let energy = |samples: &[[f32; 2]], channel: usize| {
        samples
            .iter()
            .map(|sample| (sample[channel] as f64).powi(2))
            .sum::<f64>()
    };
    let centre = render(0.0);
    let right = render(1.0);
    let left = render(-1.0);
    assert!(energy(&centre, 0) > 0.0 && energy(&centre, 1) > 0.0);
    assert_eq!(energy(&right, 0), 0.0);
    assert!(energy(&right, 1) > 0.0);
    assert_eq!(energy(&left, 1), 0.0);
    assert!(energy(&left, 0) > 0.0);
    // Centre panning must keep the legacy unpanned signal untouched.
    let unpanned = SynthPatch {
        oscillator_a_waveform: Waveform::Sine,
        oscillator_a_level: 0.8,
        unison_voices: 1,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(unpanned);
    synth.note_on(0, 69, 100);
    let legacy = (0..4_096)
        .map(|_| synth.next_stereo(48_000))
        .collect::<Vec<_>>();
    assert_eq!(centre, legacy);
}

fn unison_render(voices: usize, width: f64) -> Vec<[f32; 2]> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Saw,
        oscillator_a_level: 0.8,
        unison_voices: voices,
        detune_cents: 18.0,
        stereo_width: width,
        ..SynthPatch::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    (0..8_192).map(|_| synth.next_stereo(48_000)).collect()
}

fn channel_correlation(rendered: &[[f32; 2]]) -> f64 {
    let mut left_energy = 0.0;
    let mut right_energy = 0.0;
    let mut cross = 0.0;
    for [left, right] in rendered {
        let left = f64::from(*left);
        let right = f64::from(*right);
        left_energy += left * left;
        right_energy += right * right;
        cross += left * right;
    }
    cross / (left_energy * right_energy).sqrt()
}

#[test]
fn unison_width_zero_keeps_both_channels_identical() {
    let rendered = unison_render(4, 0.0);
    assert!(rendered.iter().all(|[left, right]| left == right));
}

#[test]
fn unison_full_width_hard_pans_every_voice() {
    let rendered = unison_render(4, 1.0);
    assert!(
        channel_correlation(&rendered).abs() < 0.5,
        "full width must decorrelate the stack"
    );
    let mid = rendered
        .iter()
        .map(|[left, right]| f64::from(left + right) * 0.5)
        .map(|sample| sample * sample)
        .sum::<f64>();
    let side = rendered
        .iter()
        .map(|[left, right]| f64::from(left - right) * 0.5)
        .map(|sample| sample * sample)
        .sum::<f64>();
    // The width gain on panned voices widens the mid/side balance further;
    // the measured target lifts them by 2.05 dB at full width.
    assert!((10.0 * (mid / side).log10()).abs() < 4.5);
}

#[test]
fn unison_full_width_keeps_the_odd_voice_centred() {
    let even = channel_correlation(&unison_render(4, 1.0));
    let odd = channel_correlation(&unison_render(3, 1.0));
    assert!(odd > even, "the centre voice must stay in the mid");
}

#[test]
fn unison_spacing_is_uniform_in_cents_at_seven_voices() {
    let count = 7;
    let detune = 17.0;
    let (positions, ratios, _) = unison_layout(count, detune);
    let cents: Vec<f64> = ratios[..count]
        .iter()
        .map(|ratio| 1200.0 * ratio.log2())
        .collect();
    let step = 2.0 * detune / (count - 1) as f64;
    for (index, value) in cents.iter().enumerate() {
        let expected = -detune + index as f64 * step;
        assert!(
            (value - expected).abs() < 1e-9,
            "voice {index} sits at {value} cents, expected {expected}"
        );
    }
    assert!((cents[0] + detune).abs() < 1e-9, "half span below");
    assert!((cents[count - 1] - detune).abs() < 1e-9, "half span above");
    assert_eq!(positions[count / 2], 0.0);
}

#[test]
fn unison_pan_sides_alternate_along_the_detune_ladder() {
    // Positive = right, measured per voice index from the flattest voice up.
    let expected: [&[f64]; 7] = [
        &[0.0],
        &[1.0, -1.0],
        &[1.0, 0.0, -1.0],
        &[1.0, -1.0, 1.0, -1.0],
        &[1.0, -1.0, 0.0, 1.0, -1.0],
        &[1.0, -1.0, 1.0, -1.0, 1.0, -1.0],
        &[1.0, -1.0, 1.0, 0.0, -1.0, 1.0, -1.0],
    ];
    for count in 1..=7 {
        let (positions, _, _) = unison_layout(count, 17.0);
        let sides: Vec<f64> = (0..count)
            .map(|index| unison_pan_side(positions[index], count))
            .collect();
        assert_eq!(sides, expected[count - 1], "N={count}");
        let spread = unison_pan_spread(1.0);
        for index in 0..count {
            let pan = unison_pan(positions[index], count, 1.0);
            assert!(
                (pan.abs() - spread * sides[index].abs()).abs() < 1e-12,
                "N={count} voice {index} pan {pan}"
            );
        }
    }
}

#[test]
fn unison_width_zero_pans_every_voice_centre() {
    for count in 1..=MAX_UNISON_VOICES {
        let (positions, _, _) = unison_layout(count, 17.0);
        for &position in &positions[..count] {
            assert_eq!(unison_pan(position, count, 0.0), 0.0);
        }
    }
    let rendered = unison_render(7, 0.0);
    assert!(rendered.iter().all(|[left, right]| left == right));
}

#[test]
fn unison_pan_keeps_the_mono_sum_and_lifts_panned_voices_with_width() {
    // The balance keeps L + R = 2 inside the panned field. The measured
    // target then lifts the panned voices relative to the centre as the
    // width rises; the centre voice does not move.
    let measured = [
        (0.00, 0.00),
        (0.25, 0.15),
        (0.50, 0.55),
        (0.70, 1.04),
        (0.85, 1.49),
        (1.00, 2.05),
    ];
    for count in 1..=MAX_UNISON_VOICES {
        let (positions, _, _) = unison_layout(count, 17.0);
        for (index, position) in positions.iter().enumerate() {
            let pan = unison_pan(*position, count, 0.0);
            let (left, right) = unison_pan_gains(pan, 0.0);
            assert!(
                ((left + right) - 2.0).abs() < 1e-12,
                "N={count} voice {index}: L+R = {}",
                left + right
            );
            if pan == 0.0 {
                continue;
            }
            for (width, expected_db) in measured {
                let pan = unison_pan(*position, count, width);
                let (left, right) = unison_pan_gains(pan, width);
                let lift_db = 20.0 * ((left + right) * 0.5).log10();
                assert!(
                    (lift_db - expected_db).abs() < 0.05,
                    "N={count} voice {index} width {width}: lift {lift_db:.2} dB, measured {expected_db:.2} dB"
                );
            }
        }
    }
    // The centre voice of an odd stack is not panned and takes no lift.
    assert_eq!(unison_pan_gains(0.0, 0.7), (1.0, 1.0));
    assert_eq!(unison_pan_gains(0.0, 1.0), (1.0, 1.0));
}

#[test]
fn unison_pan_spread_matches_the_measured_width_curve() {
    // Measured normalised pan distance per width control.
    let measured = [
        (0.02, 0.025),
        (0.10, 0.126),
        (0.20, 0.250),
        (0.30, 0.370),
        (0.50, 0.592),
        (0.70, 0.783),
        (0.90, 0.937),
        (1.00, 1.000),
    ];
    for (width, expected) in measured {
        let spread = unison_pan_spread(width);
        assert!(
            (spread - expected).abs() < 0.01,
            "width {width}: spread {spread}, measured {expected}"
        );
    }
    assert_eq!(unison_pan_spread(0.0), 0.0);
    assert!((unison_pan_spread(1.0) - 1.0).abs() < 1e-12);
    let mut previous = 0.0;
    for step in 0..=20 {
        let spread = unison_pan_spread(step as f64 / 20.0);
        assert!(spread >= previous - 1e-12, "spread must be monotone");
        previous = spread;
    }
}

#[test]
fn unison_width_lifts_the_panned_mono_sum_by_the_measured_amount() {
    // Every voice of a four-voice stack is panned, so the measured width
    // gain applies to all of them: the mono sum is 1 + 0.258 = 1.258 times
    // the width-0 sum (2.05 dB). The centre voice never takes the gain.
    let mono = unison_render(4, 0.0);
    let wide = unison_render(4, 1.0);
    let sum = |rendered: &[[f32; 2]]| {
        rendered
            .iter()
            .map(|[left, right]| f64::from((*left + *right) * 0.5).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let ratio = sum(&wide) / sum(&mono);
    assert!(
        (ratio - 1.258).abs() < 1e-3,
        "width 1 mono sum ratio {ratio:.4}, expected 1.258"
    );
}

#[test]
fn per_oscillator_unison_is_independent_and_deterministic() {
    let patch = SynthPatch {
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.5,
        oscillator_b_unison: 4,
        oscillator_b_detune_cents: 25.0,
        oscillator_b_stereo_width: 1.0,
        ..Default::default()
    };
    let render = || {
        let mut synth = OpenSynth::new(patch.clone());
        synth.note_on(0, 60, 100);
        (0..1024)
            .map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let first = render();
    let second = render();
    assert_eq!(first, second);
    assert!(first.iter().flatten().any(|sample| sample.abs() > 0.001));
    assert!(
        first
            .iter()
            .any(|[left, right]| (left - right).abs() > 0.001),
        "unison width should produce stereo"
    );
    let mut mono = patch.clone();
    mono.oscillator_b_unison = 1;
    let mut mono_synth = OpenSynth::new(mono);
    mono_synth.note_on(0, 60, 100);
    let mono_render: Vec<_> = (0..1024).map(|_| mono_synth.next_stereo(48_000)).collect();
    assert_ne!(first, mono_render);
}

#[test]
fn fx_parameter_modulation_changes_rendered_output() {
    let render = |macro2: f64, with_route: bool| {
        let mut patch = SynthPatch {
            unison_voices: 1,
            ..Default::default()
        };
        let mut distortion = SynthFx::new(SynthFxType::Distortion);
        distortion.set_param("mix", 0.0);
        distortion.set_param("drive", 0.6);
        patch.fx = vec![distortion];
        if with_route {
            patch.fx_mod_routes = vec![FxModRoute {
                source: ModSource::Macro2,
                fx_index: 0,
                parameter: FxParam::Wet,
                amount: 1.0,
                bipolar: false,
            }];
        }
        patch.macros[1] = macro2;
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 40, 100);
        render_frames(&mut synth, 4_800)
    };
    let no_route = render(0.0, false);
    let dry = render(0.0, true);
    let wet = render(1.0, true);
    assert_eq!(dry, no_route, "a zero-source route must be inaudible");
    assert_ne!(wet, no_route, "Macro2 must change the distortion output");
    assert!(wet.iter().all(|sample| sample.is_finite()));
    let rms = |samples: &[f32]| {
        (samples
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    };
    // The distortion now sees the pre-master level, so heavy drive
    // compresses the wet path; this test only guards the modulation itself.
    let ratio = rms(&wet) / rms(&dry);
    assert!(
        (0.25..2.0).contains(&ratio),
        "distortion level ratio {ratio}"
    );
}

#[test]
fn sweet_bass_style_fx_routes_stay_finite_and_in_range() {
    let mut patch = SynthPatch::default();
    let mut distortion = SynthFx::new(SynthFxType::Distortion);
    distortion.set_param("mix", 0.0);
    distortion.set_param("drive", 0.5);
    let mut eq = SynthFx::new(SynthFxType::Eq3);
    eq.set_param("high", 5.05);
    eq.set_param("high_freq", 5_486.7);
    let mut flanger = SynthFx::new(SynthFxType::Flanger);
    flanger.set_param("mix", 0.0);
    patch.fx = vec![distortion, eq, flanger];
    patch.fx_mod_routes = vec![
        FxModRoute {
            source: ModSource::Macro2,
            fx_index: 0,
            parameter: FxParam::Wet,
            amount: 1.0,
            bipolar: false,
        },
        FxModRoute {
            source: ModSource::Macro4,
            fx_index: 1,
            parameter: FxParam::Gain2,
            amount: 0.394_736_766_815_185_55,
            bipolar: false,
        },
        FxModRoute {
            source: ModSource::Macro7,
            fx_index: 1,
            parameter: FxParam::Freq1,
            amount: 1.0,
            bipolar: false,
        },
        FxModRoute {
            source: ModSource::Lfo3,
            fx_index: 2,
            parameter: FxParam::Feedback,
            amount: -0.113_213_896_751_403_8,
            bipolar: false,
        },
        FxModRoute {
            source: ModSource::Lfo4,
            fx_index: 2,
            parameter: FxParam::Depth,
            amount: 0.475_102_663_040_161_1,
            bipolar: false,
        },
        FxModRoute {
            source: ModSource::Macro8,
            fx_index: 2,
            parameter: FxParam::Wet,
            amount: 1.0,
            bipolar: false,
        },
    ];
    patch.macros = [0.0, 1.0, 0.0, 1.0];
    patch.extra_macros = [0.0, 0.0, 1.0, 1.0];
    patch.lfo_rate = 7.0;
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 40, 100);
    let output = render_frames(&mut synth, 9_600);
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(output.iter().any(|sample| *sample != 0.0));
    assert!((synth.fx_chain.module_value(0, "mix").unwrap() - 1.0).abs() < 1e-9);
    // Eq high gain is 5.05 dB + 0.3947 * Macro4(1.0) * 24 dB, clamped.
    let high = synth.fx_chain.module_value(1, "high").unwrap();
    assert!(high.is_finite() && (-24.0..=24.0).contains(&high));
    let low_freq = synth.fx_chain.module_value(1, "low_freq").unwrap();
    assert!((20.0..=20_000.0).contains(&low_freq));
    let flanger_mix = synth.fx_chain.module_value(2, "mix").unwrap();
    assert!((0.0..=1.0).contains(&flanger_mix));
    assert!((flanger_mix - 1.0).abs() < 1e-9);
    let feedback = synth.fx_chain.module_value(2, "feedback").unwrap();
    let depth = synth.fx_chain.module_value(2, "depth").unwrap();
    assert!((0.0..=0.95).contains(&feedback));
    assert!((0.0..=1.0).contains(&depth));

    // Removing every route must restore the imported base values on the
    // next edit, without waiting for a control tick or leaving residue.
    synth.set_param("fxmod.1.amount", 0.0);
    assert_eq!(synth.fx_chain.module_value(0, "mix"), Some(0.0));
    synth.set_param("fxmod.6.amount", 0.0);
    synth.set_param("fxmod.5.amount", 0.0);
    synth.set_param("fxmod.3.amount", 0.0);
    synth.set_param("fxmod.2.amount", 0.0);
    synth.set_param("fxmod.4.amount", 0.0);
    assert_eq!(synth.fx_chain.module_value(1, "high"), Some(5.05));
    assert_eq!(synth.fx_chain.module_value(2, "mix"), Some(0.0));
}

#[test]
fn bipolar_fx_routes_centre_a_unipolar_macro() {
    let render_mix = |macro2: f64| {
        let mut patch = SynthPatch::default();
        let mut distortion = SynthFx::new(SynthFxType::Distortion);
        distortion.set_param("mix", 0.5);
        patch.fx = vec![distortion];
        patch.fx_mod_routes = vec![FxModRoute {
            source: ModSource::Macro2,
            fx_index: 0,
            parameter: FxParam::Wet,
            amount: 0.5,
            bipolar: true,
        }];
        patch.macros[1] = macro2;
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        render_frames(&mut synth, 1);
        synth.fx_chain.module_value(0, "mix").unwrap()
    };
    assert!((render_mix(0.0) - 0.0).abs() < 1e-9);
    assert!((render_mix(0.5) - 0.5).abs() < 1e-9);
    assert!((render_mix(1.0) - 1.0).abs() < 1e-9);
}

#[test]
fn fx_modulation_routes_roundtrip_through_params_and_json() {
    use oc_dsl::ast::ParamValue;
    let patch = SynthPatch {
        fx: vec![SynthFx::new(SynthFxType::Distortion)],
        fx_mod_routes: vec![FxModRoute {
            source: ModSource::Macro2,
            fx_index: 0,
            parameter: FxParam::Wet,
            amount: 0.75,
            bipolar: true,
        }],
        ..Default::default()
    };
    let json = serde_json::to_vec(&patch).unwrap();
    let restored: SynthPatch = serde_json::from_slice(&json).unwrap();
    assert_eq!(restored.fx_mod_routes.len(), 1);
    let route = restored.fx_mod_routes[0];
    assert_eq!(route.source, ModSource::Macro2);
    assert_eq!(route.parameter, FxParam::Wet);
    assert!(route.bipolar);

    // Old documents omit the field entirely and must keep loading.
    let mut legacy = serde_json::to_value(SynthPatch::default()).unwrap();
    legacy.as_object_mut().unwrap().remove("fx_mod_routes");
    let legacy: SynthPatch = serde_json::from_value(legacy).unwrap();
    assert!(legacy.fx_mod_routes.is_empty());

    let params = vec![
        ("fx.1.type".to_string(), ParamValue::Number(6.0)),
        ("fx.1.mix".to_string(), ParamValue::Number(0.0)),
        ("fxmod.1.source".to_string(), ParamValue::Number(4.0)),
        ("fxmod.1.fx_index".to_string(), ParamValue::Number(0.0)),
        ("fxmod.1.parameter".to_string(), ParamValue::Number(0.0)),
        ("fxmod.1.amount".to_string(), ParamValue::Number(0.5)),
        ("fxmod.1.bipolar".to_string(), ParamValue::Number(1.0)),
    ];
    let mut applied = SynthPatch::default();
    applied.apply_params(Some(&params));
    assert_eq!(applied.fx_mod_routes.len(), 1);
    let route = applied.fx_mod_routes[0];
    assert_eq!(route.source, ModSource::Macro2);
    assert_eq!(route.parameter, FxParam::Wet);
    assert!((route.amount - 0.5).abs() < 1e-9);
    assert!(route.bipolar);
}

#[test]
fn fx_param_numeric_abi_is_append_only() {
    assert_eq!(FxParam::ALL.len(), 27);
    for (index, parameter) in FxParam::ALL.iter().enumerate() {
        assert_eq!(fx_param_from_value(index as f64), *parameter);
        assert!(!parameter.native_name().is_empty());
    }
    assert_eq!(fx_param_from_value(-1.0), FxParam::Wet);
    assert_eq!(fx_param_from_value(99.0), FxParam::Wet);
}

fn high_band_energy(samples: &[f32], cutoff_hz: f64, sample_rate: f64) -> f64 {
    let alpha = 1.0 / (1.0 + std::f64::consts::TAU * cutoff_hz / sample_rate);
    let mut low = 0.0;
    let mut energy = 0.0;
    for sample in samples {
        let value = f64::from(*sample);
        low += alpha * (value - low);
        let high = value - low;
        energy += high * high;
    }
    energy
}

fn render_source_route(route: SourceFilterRoute) -> Vec<f32> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Saw,
        oscillator_a_level: 0.8,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        unison_voices: 1,
        stereo_width: 0.0,
        filter_cutoff: 900.0,
        filter_resonance: 0.1,
        filter2_enabled: true,
        filter2_cutoff: 12_000.0,
        filter2_resonance: 0.1,
        filter_routing: FilterRouting::Parallel,
        source_filter_routes: Some([route; 5]),
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 45, 100);
    (0..8_192).map(|_| synth.next_stereo(48_000)[0]).collect()
}

#[test]
fn source_routed_to_filter_two_has_a_different_spectrum_than_filter_one() {
    let filter1 = render_source_route(SourceFilterRoute {
        target: SourceFilterTarget::Filter1,
        balance: 0.0,
    });
    let filter2 = render_source_route(SourceFilterRoute {
        target: SourceFilterTarget::Filter2,
        balance: 1.0,
    });
    let high1 = high_band_energy(&filter1, 2_000.0, 48_000.0);
    let high2 = high_band_energy(&filter2, 2_000.0, 48_000.0);
    assert!(high1 > 0.0);
    assert!(
        high2 > high1 * 5.0,
        "filter 2 route should pass the open filter: {high1} vs {high2}"
    );
}

#[test]
fn both_balance_splits_energy_between_the_two_filters() {
    let energy = |balance: f64| {
        high_band_energy(
            &render_source_route(SourceFilterRoute {
                target: SourceFilterTarget::Both,
                balance,
            }),
            2_000.0,
            48_000.0,
        )
    };
    let closed = energy(0.0);
    let split = energy(0.5);
    let open = energy(1.0);
    assert!(
        closed < split && split < open,
        "balance should move energy from the closed filter to the open one: {closed} {split} {open}"
    );
}

#[test]
fn explicit_filter_one_routing_matches_the_legacy_mix() {
    let render = |routes: Option<[SourceFilterRoute; 5]>| {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Saw,
            oscillator_a_level: 0.6,
            oscillator_b_level: 0.3,
            oscillator_c_level: 0.2,
            filter_cutoff: 1_200.0,
            filter_resonance: 0.3,
            source_filter_routes: routes,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 55, 110);
        render_frames(&mut synth, 4_096)
    };
    let legacy = render(None);
    let explicit = render(Some([SourceFilterRoute::default(); 5]));
    let difference = legacy
        .iter()
        .zip(&explicit)
        .map(|(left, right)| f64::from((left - right).abs()))
        .fold(0.0, f64::max);
    assert!(
        difference < 1e-6,
        "legacy and filter-1 routes differ: {difference}"
    );
}

#[test]
fn source_routing_is_absent_from_the_legacy_patch_json() {
    let value = serde_json::to_value(SynthPatch::default()).unwrap();
    assert!(value.get("source_filter_routes").is_none());
    let patch: SynthPatch = serde_json::from_value(value).unwrap();
    assert!(patch.source_filter_routes.is_none());
}

#[test]
fn macro_eight_can_modulate_a_destination() {
    let mut patch = SynthPatch {
        oscillator_a_level: 0.6,
        ..Default::default()
    };
    patch.extra_macros[3] = 0.0;
    patch.mod_routes[0] = ModRoute {
        source: ModSource::Macro8,
        destination: ModDestination::FilterCutoff,
        amount: 0.8,
        ..ModRoute::default()
    };
    let mut closed = OpenSynth::new(patch.clone());
    closed.note_on(0, 60, 100);
    let closed_render: Vec<_> = (0..2048).map(|_| closed.next_stereo(48_000)).collect();
    patch.extra_macros[3] = 1.0;
    let mut open = OpenSynth::new(patch);
    open.note_on(0, 60, 100);
    let open_render: Vec<_> = (0..2048).map(|_| open.next_stereo(48_000)).collect();
    assert_ne!(closed_render, open_render);
}

fn write_sampler_tone(path: &std::path::Path, frequency: f64) -> usize {
    use super::*;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let frames = 44_100;
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..frames {
        let value = (TAU * frequency * frame as f64 / 44_100.0).sin();
        writer
            .write_sample((value * 0.8 * f64::from(i16::MAX)) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
    frames
}

fn sampler_tone_patch(path: &std::path::Path, frames: usize) -> super::SynthPatch {
    use super::*;
    SynthPatch {
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        sub_level: 0.0,
        noise_level: 0.0,
        gain: 1.0,
        filter_cutoff: 18_000.0,
        filter_model: FilterModel::Svf,
        filter2_enabled: false,
        filter_keytrack: 0.0,
        filter_envelope_amount: 0.0,
        sampler: Some(crate::sampler::SamplerPatch {
            mono: false,
            velocity_curve: crate::sampler::VelocityCurve::Linear,
            regions: vec![crate::sampler::SampleRegion {
                sample: path.to_path_buf(),
                lokey: 0,
                hikey: 127,
                lovel: 0,
                hivel: 127,
                pitch_keycenter: 60.0,
                loop_start: Some(0),
                loop_end: Some(frames as u64),
                loop_mode: crate::sampler::LoopMode::LoopContinuous,
                offset: None,
                end: Some(frames as u64),
                tune_cents: 0.0,
                gain: 1.0,
                amp_veltrack: 0.0,
                pan: 0.0,
                seq_position: None,
                seq_length: None,
                group: None,
                off_by: None,
                unison: 1,
                unison_detune_cents: 0.0,
                unison_width: 0.0,
                // Slow SFZ release: any decay below comes from the voice envelope.
                ampeg: crate::sampler::AmpEnvelope {
                    attack: 0.0,
                    decay: 0.0,
                    sustain: 1.0,
                    release: 2.0,
                },
            }],
            gain: 1.0,
            pan: 0.0,
            sfz: None,
            diagnostics: Vec::new(),
        }),
        ..SynthPatch::default()
    }
}

fn render_stereo_rms(synth: &mut super::OpenSynth, frames: usize) -> f64 {
    let mut sum = 0.0;
    for _ in 0..frames {
        let [left, right] = synth.next_stereo(48_000);
        sum += f64::from(left) * f64::from(left) + f64::from(right) * f64::from(right);
    }
    (sum / (2.0 * frames as f64)).sqrt()
}

/// Amplitude of `hz` in the next `frames` samples (note 60 plays the
/// fixture at its recorded pitch).
fn render_tone_amplitude(synth: &mut super::OpenSynth, frames: usize, hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for frame in 0..frames {
        let [left, _] = synth.next_stereo(48_000);
        let angle = TAU * hz * frame as f64 / 48_000.0;
        re += f64::from(left) * angle.cos();
        im += f64::from(left) * angle.sin();
    }
    2.0 * re.hypot(im) / frames as f64
}

#[test]
fn sampler_output_is_filtered_by_the_voice_filter() {
    use super::*;
    let path = std::env::temp_dir().join(format!(
        "opencompose-sampler-filter-{}.wav",
        std::process::id()
    ));
    let frames = write_sampler_tone(&path, 8_000.0);
    let patch = sampler_tone_patch(&path, frames);
    let render = |cutoff: f64| {
        let mut patch = patch.clone();
        patch.filter_cutoff = cutoff;
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 127);
        let _ = render_stereo_rms(&mut synth, 2_400);
        render_tone_amplitude(&mut synth, 4_800, 8_000.0)
    };
    let open = render(18_000.0);
    let closed = render(200.0);
    std::fs::remove_file(&path).unwrap();
    assert!(
        open > 0.1,
        "sample layer silent with an open filter: {open}"
    );
    assert!(
        closed < open * 0.01,
        "8 kHz tone passed the voice filter: open={open} closed={closed}"
    );
}

#[test]
fn sampler_output_follows_its_source_filter_route() {
    use super::*;
    let path = std::env::temp_dir().join(format!(
        "opencompose-sampler-route-{}.wav",
        std::process::id()
    ));
    let frames = write_sampler_tone(&path, 8_000.0);
    let patch = sampler_tone_patch(&path, frames);
    let render = |target: SourceFilterTarget| {
        let mut patch = patch.clone();
        patch.filter_cutoff = 200.0;
        patch.filter2_enabled = true;
        patch.filter2_cutoff = 18_000.0;
        let mut routes = [SourceFilterRoute::default(); 5];
        routes[0].target = target;
        patch.source_filter_routes = Some(routes);
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 127);
        let _ = render_stereo_rms(&mut synth, 2_400);
        render_tone_amplitude(&mut synth, 4_800, 8_000.0)
    };
    let filter2 = render(SourceFilterTarget::Filter2);
    let filter1 = render(SourceFilterTarget::Filter1);
    std::fs::remove_file(&path).unwrap();
    assert!(filter2 > 0.1, "sample silent through filter 2: {filter2}");
    assert!(
        filter1 < filter2 * 0.01,
        "sample ignored its Osc 1 filter route: filter1={filter1} filter2={filter2}"
    );
}

#[test]
fn sampler_output_follows_the_voice_amp_envelope() {
    use super::*;
    let path = std::env::temp_dir().join(format!(
        "opencompose-sampler-envelope-{}.wav",
        std::process::id()
    ));
    let frames = write_sampler_tone(&path, 440.0);
    let mut patch = sampler_tone_patch(&path, frames);
    patch.attack = 0.2;
    patch.release = 0.05;
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 127);
    let attack = render_stereo_rms(&mut synth, 400);
    let _ = render_stereo_rms(&mut synth, 9_600);
    let sustain = render_stereo_rms(&mut synth, 2_400);
    synth.note_off(0, 60);
    let _ = render_stereo_rms(&mut synth, 4_800);
    let tail = render_stereo_rms(&mut synth, 480);
    std::fs::remove_file(&path).unwrap();
    assert!(sustain > 0.1, "sample layer silent at sustain: {sustain}");
    assert!(
        attack < sustain * 0.2,
        "amp attack did not attenuate the sample layer: attack={attack} sustain={sustain}"
    );
    assert!(
        tail < sustain * 0.01,
        "amp release did not stop the sample layer: tail={tail} sustain={sustain}"
    );
}

fn render_velocity_rms(patch: super::SynthPatch, velocity: u8) -> f64 {
    let mut synth = super::OpenSynth::new(patch);
    synth.note_on(0, 69, velocity);
    let _ = render_stereo_rms(&mut synth, 2_400);
    render_stereo_rms(&mut synth, 4_800)
}

#[test]
fn velocity_to_amp_is_linear_and_zero_keeps_the_level_constant() {
    let patch = |amount: f64| super::SynthPatch {
        velocity_to_amp: amount,
        attack: 0.0,
        decay: 0.0,
        sustain: 1.0,
        release: 0.0,
        unison_voices: 1,
        stereo_width: 0.0,
        filter_cutoff: 18_000.0,
        ..super::SynthPatch::default()
    };
    let full = render_velocity_rms(patch(1.0), 127);
    let mp = render_velocity_rms(patch(1.0), 64);
    assert!((mp / full - 64.0 / 127.0).abs() < 0.01);
    let flat = render_velocity_rms(patch(0.0), 64) / render_velocity_rms(patch(0.0), 127);
    assert!((flat - 1.0).abs() < 0.01);
}

#[test]
fn velocity_routed_to_voice_amp_follows_the_depth_law() {
    let render_at = |bipolar: bool, velocity: u8| {
        let patch = super::SynthPatch {
            velocity_to_amp: 0.0,
            mod_routes: vec![super::ModRoute {
                source: super::ModSource::Velocity,
                destination: super::ModDestination::VoiceAmp,
                amount: 1.0,
                bipolar,
                ..super::ModRoute::default()
            }],
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            unison_voices: 1,
            stereo_width: 0.0,
            filter_cutoff: 18_000.0,
            ..super::SynthPatch::default()
        };
        render_velocity_rms(patch, velocity)
    };
    // A 100% bipolar route turns the 0.5-centred VoiceAmp into velocity/127.
    let bipolar_ratio = render_at(true, 96) / render_at(true, 64);
    assert!((bipolar_ratio - 96.0 / 64.0).abs() < 0.02);
    // Unipolar is full-range: the normalised gain is 1 + 2 * velocity. The
    // pair stays below `MAX_VOICE_AMP`.
    let unipolar = |velocity: f64| 1.0 + 2.0 * velocity / 127.0;
    let unipolar_ratio = render_at(false, 48) / render_at(false, 24);
    assert!(
        (unipolar_ratio - unipolar(48.0) / unipolar(24.0)).abs() < 0.02,
        "unipolar ratio {unipolar_ratio:.3}"
    );
    // The stage ceiling holds the multiplier at +6 dB: the reachable gain
    // is 3.0 at velocity 127, so the rendered ratio to velocity 64 is 2.
    let capped_ratio = render_at(false, 127) / render_at(false, 64);
    assert!(
        (capped_ratio - super::MAX_VOICE_AMP / unipolar(64.0)).abs() < 0.02,
        "the stage ceiling did not hold the route at {}: ratio {capped_ratio:.3}",
        super::MAX_VOICE_AMP
    );
}

#[test]
fn voice_amp_route_depth_follows_the_law_at_every_amount() {
    let render_ratio = |amount: f64| {
        let patch = super::SynthPatch {
            velocity_to_amp: 0.0,
            mod_routes: vec![super::ModRoute {
                source: super::ModSource::Velocity,
                destination: super::ModDestination::VoiceAmp,
                amount,
                bipolar: true,
                ..super::ModRoute::default()
            }],
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            unison_voices: 1,
            stereo_width: 0.0,
            filter_cutoff: 18_000.0,
            ..super::SynthPatch::default()
        };
        20.0 * (render_velocity_rms(patch.clone(), 96) / render_velocity_rms(patch, 64)).log10()
    };
    // VoiceAmp is 0.5-centred, so depth `a` moves the mp->f level
    // by 20*log10((1 + a*65/127) / (1 + a*1/127)); the measured
    // +1.04/+1.97/+3.56 dB on the same isolated route.
    for (amount, measured_db) in [(0.25, 1.04), (0.5, 1.97), (1.0, 3.56)] {
        let gained = render_ratio(amount);
        assert!(
            (gained - measured_db).abs() < 0.15,
            "depth {amount}: engine {gained:+.2} dB, reference {measured_db:+.2} dB"
        );
    }
}

#[test]
fn sub_oscillator_gain_law_is_linear_and_normalised() {
    let render = |level: f64| {
        let patch = SynthPatch {
            gain: 1.0,
            oscillator_a_level: 0.0,
            oscillator_b_level: 0.0,
            oscillator_c_level: 0.0,
            noise_level: 0.0,
            sub_level: level,
            filter_cutoff: 20_000.0,
            filter_resonance: 0.0,
            filter_drive: 0.0,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 127);
        (0..24_000)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let peak = |samples: &[f32]| {
        samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
    };
    let quarter = peak(&render(0.25));
    let half = peak(&render(0.5));
    assert!(
        (half / quarter - 2.0).abs() < 0.02,
        "sub level must stay linear: {quarter} {half}"
    );
    // The sine table amplitude is 0.6575 at level 1.
    assert!(
        (half - 0.5 * 0.6575_f32).abs() < 0.01,
        "unexpected sub peak {half}"
    );
}

#[test]
fn sub_shape_level_table_matches_the_expected_amplitudes() {
    let rms = |waveform: SubShape| {
        let patch = SynthPatch {
            gain: 1.0,
            oscillator_a_level: 0.0,
            oscillator_b_level: 0.0,
            oscillator_c_level: 0.0,
            noise_level: 0.0,
            sub_level: 0.5,
            sub_waveform: waveform,
            sub_semitones: 0.0,
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            filter_wet: 0.0,
            ..Default::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 36, 127);
        let samples: Vec<f32> = (0..48_000).map(|_| synth.next_stereo(48_000)[0]).collect();
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    };
    // Table RMS of the six shapes; the band-limited readout keeps
    // it within the tolerance while the peak of the discontinuous shapes
    // carries the FIR overshoot.
    for (waveform, amplitude) in [
        (SubShape::Sine, 0.464_905),
        (SubShape::RoundRect, 0.466_246),
        (SubShape::Triangle, 0.462_976),
        (SubShape::Saw, 0.438_879),
        (SubShape::Square, 0.447_083),
        (SubShape::Pulse, 0.490_216),
    ] {
        let measured = rms(waveform);
        let expected = 0.5 * amplitude;
        assert!(
            (measured - expected).abs() < 0.05 * expected,
            "{waveform:?}: rms {measured}, expected {expected}"
        );
    }
}

fn render_sub_notes(patch: SynthPatch, onsets: &[(u64, u64)]) -> Vec<f32> {
    let mut events = Vec::new();
    for &(on, off) in onsets {
        events.push(MidiEvent {
            time_samples: on,
            track_index: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: 48,
                velocity: 127,
            },
        });
        events.push(MidiEvent {
            time_samples: off,
            track_index: 0,
            event: MidiMessage::NoteOff {
                channel: 0,
                note: 48,
            },
        });
    }
    let total = onsets.last().map(|&(_, off)| off as usize).unwrap_or(0) + 1;
    let refs: Vec<&MidiEvent> = events.iter().collect();
    render_track_with_patch_and_clock(&refs, total, 48_000, patch, 60.0, None)
}

fn sub_only_patch() -> SynthPatch {
    SynthPatch {
        gain: 1.0,
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        noise_level: 0.0,
        sub_level: 0.5,
        sub_semitones: 0.0,
        attack: 0.0,
        decay: 0.0,
        sustain: 1.0,
        release: 0.0,
        filter_wet: 0.0,
        ..Default::default()
    }
}

#[test]
fn sub_initial_phase_180_degrees_inverts_the_waveform() {
    let render = |phase: f64| {
        let mut patch = sub_only_patch();
        patch.sub_initial_phase = phase;
        let out = render_sub_notes(patch, &[(0, 4_800)]);
        out.chunks_exact(2)
            .map(|frame| frame[0])
            .take(512)
            .collect::<Vec<f32>>()
    };
    let zero = render(0.0);
    let inverted = render(0.5);
    for (index, (a, b)) in zero.iter().zip(&inverted).enumerate() {
        assert!(
            (a + b).abs() < 1e-6,
            "sample {index}: {a} vs {b} is not an inversion"
        );
    }
    assert!(zero.iter().any(|sample| sample.abs() > 0.1));
}

#[test]
fn sub_phase_resets_per_note_unless_contiguous() {
    let first = |out: &[f32], start: usize| -> Vec<f32> {
        out[start * 2..start * 2 + 256]
            .chunks_exact(2)
            .map(|f| f[0])
            .collect()
    };
    let second_onset = 72_000usize;
    let mut reset = sub_only_patch();
    reset.sub_initial_phase = 0.3;
    let a = render_sub_notes(reset.clone(), &[(0, 24_000), (second_onset as u64, 96_000)]);
    assert_eq!(first(&a, 0), first(&a, second_onset));

    let mut contiguous = reset;
    contiguous.sub_contiguous_phase = true;
    let b = render_sub_notes(contiguous, &[(0, 24_000), (second_onset as u64, 96_000)]);
    assert_ne!(first(&b, 0), first(&b, second_onset));
}

#[test]
fn sub_pan_uses_the_centre_compensated_law() {
    let (left, right) = sub_pan_gains(0.0);
    assert!((left - 1.0).abs() < 1e-12 && (right - 1.0).abs() < 1e-12);
    let centre = 0.5 + UNISON_PAN_BASIS * 0.25;
    let (left, right) = sub_pan_gains(-1.0);
    assert!(right.abs() < 1e-12);
    assert!((left - 1.0 / centre).abs() < 1e-12);
    let (left, right) = sub_pan_gains(1.0);
    assert!(left.abs() < 1e-12);
    assert!((right - 1.0 / centre).abs() < 1e-12);

    // A hard-left sub must leave the right channel silent.
    let mut patch = sub_only_patch();
    patch.sub_pan = -1.0;
    let out = render_sub_notes(patch, &[(0, 24_000)]);
    let peak = |channel: usize| {
        out.chunks_exact(2)
            .map(|frame| frame[channel].abs())
            .fold(0.0_f32, f32::max)
    };
    assert!(peak(1).abs() < 1e-6, "right channel {}", peak(1));
    assert!(peak(0) > 0.1, "left channel {}", peak(0));
}

#[test]
fn sub_fine_is_in_cents_and_clamps_to_100() {
    let mut synth = OpenSynth::new(sub_only_patch());
    synth.set_param("sub.fine", 50.0);
    assert!((synth.sub_ratio - 2f64.powf(50.0 / 1200.0)).abs() < 1e-12);
    synth.set_param("sub.fine", 1200.0);
    assert_eq!(synth.patch.sub_fine_cents, 100.0);
    assert!((synth.sub_ratio - 2f64.powf(100.0 / 1200.0)).abs() < 1e-12);
    synth.set_param("sub.fine", -1200.0);
    assert_eq!(synth.patch.sub_fine_cents, -100.0);

    // Rendered pitch moves by the requested cents (integer crossing count).
    let frequency = |fine: f64| {
        let mut patch = sub_only_patch();
        patch.sub_fine_cents = fine;
        let out = render_sub_notes(patch, &[(0, 192_000)]);
        let mono: Vec<f32> = out.chunks_exact(2).map(|frame| frame[0]).collect();
        let steady = &mono[9_600..];
        let crossings = steady
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count() as f64;
        crossings * 48_000.0 / steady.len() as f64
    };
    let base = frequency(0.0);
    assert!((base - 130.813).abs() < 1.0, "base {base}");
    let cents = |value: f64| 1200.0 * (value / base).log2();
    assert!((cents(frequency(50.0)) - 50.0).abs() < 8.0);
    assert!((cents(frequency(-50.0)) + 50.0).abs() < 8.0);
    assert!((cents(frequency(1200.0)) - 100.0).abs() < 8.0);
}

#[test]
fn sub_saw_crest_pins_the_band_limited_edge() {
    let patch = SynthPatch {
        gain: 1.0,
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        noise_level: 0.0,
        sub_level: 0.5,
        sub_waveform: SubShape::Saw,
        sub_semitones: 0.0,
        filter_wet: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 48, 127);
    // Skip the attack so the measured crest is the steady saw edge.
    let rendered: Vec<f32> = (0..72_000)
        .flat_map(|_| synth.next_stereo(48_000))
        .collect();
    let steady = &rendered[2 * 48_000..];
    let peak = steady
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let rms =
        (steady.iter().map(|sample| sample * sample).sum::<f32>() / steady.len() as f32).sqrt();
    let crest = peak / rms;
    // The imported C3 saw crest is 2.47: the 113-tap mip overshoots the wrap.
    assert!(
        (2.0..=2.8).contains(&crest),
        "sub saw crest {crest:.3} (peak {peak:.4}, rms {rms:.4})"
    );
}

#[test]
fn sub_mips_reject_images_above_nyquist() {
    let patch = SynthPatch {
        gain: 1.0,
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        noise_level: 0.0,
        sub_level: 0.5,
        sub_waveform: SubShape::Saw,
        sub_semitones: 0.0,
        filter_wet: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 96, 127);
    let rendered: Vec<[f32; 2]> = (0..49_152).map(|_| synth.next_stereo(48_000)).collect();
    let f0 = 2093.005;
    let harmonics = steady_harmonics(&rendered, f0, 8);
    for harmonic in 1..=5 {
        let expected = -20.0 * (harmonic as f64).log10();
        let measured = 20.0 * (harmonics[harmonic - 1] / harmonics[0]).log10();
        assert!(
            (measured - expected).abs() < 1.0,
            "H{harmonic} at {measured:.2} dB, expected {expected:.2} dB"
        );
    }
    // C7 reads a 16-point table, so images of H1..H4 fold into the audio band.
    for harmonic in 1..=4 {
        let image = 16.0 * f0 - harmonic as f64 * f0;
        let alias = 48_000.0 - image;
        let relative = 20.0 * (steady_amplitude(&rendered, alias) / harmonics[0]).log10();
        assert!(
            relative < -50.0,
            "H{harmonic} image at {alias:.0} Hz: {relative:.1} dB"
        );
    }
    // H8 sits on the table Nyquist and must not survive the readout.
    let h8 = 20.0 * (harmonics[7] / harmonics[0]).log10();
    assert!(h8 < -50.0, "H8 {h8:.1} dB");
}

/// The bass demo's onset peak is the sub oscillator riding the amp
/// envelope: its first wrap lands while `Env0` is still near the attack
/// peak, so it is louder than the steady wrap by the decay-to-sustain
/// ratio. Pin that start-up shape so a future oscillator change is measured.
#[test]
fn sub_saw_start_up_is_louder_than_its_steady_wrap() {
    let mut patch = SynthPatch {
        gain: 1.0,
        oscillator_a_level: 0.0,
        oscillator_b_level: 0.0,
        oscillator_c_level: 0.0,
        noise_level: 0.0,
        sub_level: 0.5,
        sub_waveform: SubShape::Saw,
        sub_semitones: -36.0,
        filter_wet: 0.0,
        ..Default::default()
    };
    // BA - Sweet Bass Env0: 0.515 ms attack, 835 ms decay, 0.706 sustain
    // control value, 0.332 decay curve.
    patch.attack = 0.000_515;
    patch.decay = 0.835;
    patch.sustain = 0.705_882_3 * 0.705_882_3;
    patch.release = 0.150;
    patch.amp_curve = 0.332;
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 36, 127);
    // Left channel only: the voice is mono, and an interleaved vector would
    // halve the effective sample rate.
    let rendered: Vec<f32> = (0..96_000).map(|_| synth.next_stereo(48_000)[0]).collect();
    // C2 sub at octave -3 is 8.1758 Hz, a 5871-sample period.
    let period = (48_000.0_f64 / 8.175_798_915_643_707).round() as usize;
    let cycle_rms = |index: usize| {
        let start = index * period;
        let window = &rendered[start..start + period];
        (window.iter().map(|sample| sample * sample).sum::<f32>() / window.len() as f32).sqrt()
    };
    let cycles: Vec<f32> = (0..9).map(cycle_rms).collect();
    let first = cycles[0];
    let steady = cycles[4..9].iter().sum::<f32>() / 5.0;
    let ratio = first / steady;
    // The decay-to-sustain gain is 1 / 0.498 = 2.0; the first wrap lands
    // after part of the decay, so the measured ratios cluster near 1.8.
    assert!(
        (1.6..=2.0).contains(&ratio),
        "sub start-up ratio {ratio:.3} (cycles {cycles:?})"
    );
}

/// B and C transpose while sounding, not only while their phase advances
/// silently: a preset that raises their level must not play at note pitch.
#[test]
fn active_oscillators_apply_their_transpose() {
    // One oscillator silenced at a time so each crossing count is pure.
    fn pitch_of(oscillator: char, semitones: f64) -> f64 {
        let mut patch = SynthPatch {
            gain: 1.0,
            oscillator_a_level: 0.0,
            oscillator_b_level: 0.0,
            oscillator_c_level: 0.0,
            filter_wet: 0.0,
            ..Default::default()
        };
        match oscillator {
            'b' => {
                patch.oscillator_b_level = 0.5;
                patch.oscillator_b_waveform = Waveform::Saw;
                patch.oscillator_b_unison = 1;
                patch.oscillator_b_semitones = semitones;
            }
            _ => {
                patch.oscillator_c_level = 0.5;
                patch.oscillator_c_waveform = Waveform::Saw;
                patch.oscillator_c_semitones = semitones;
            }
        }
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 127);
        let samples: Vec<f32> = (0..24_000)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect();
        let mono: Vec<f32> = samples.iter().step_by(2).copied().collect();
        mono.windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count() as f64
    }
    for oscillator in ['b', 'c'] {
        let unit = pitch_of(oscillator, 0.0);
        let octave = pitch_of(oscillator, -12.0);
        assert!(
            (unit / octave - 2.0).abs() < 0.05,
            "{oscillator} transpose ignored: {unit} Hz vs {octave} Hz"
        );
    }
}

fn harmonic_amplitudes(samples: &[f64], sample_rate: f64, f0: f64, count: usize) -> Vec<f64> {
    let start = samples.len() / 8;
    let window = &samples[start..];
    let n = window.len();
    let mean = window.iter().sum::<f64>() / n as f64;
    (1..=count)
        .map(|h| {
            let step = std::f64::consts::TAU * h as f64 * f0 / sample_rate;
            let (mut real, mut imag) = (0.0, 0.0);
            for (index, sample) in window.iter().enumerate() {
                let hann = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / n as f64).cos();
                let value = (sample - mean) * hann;
                real += value * (step * index as f64).cos();
                imag -= value * (step * index as f64).sin();
            }
            2.0 * (real * real + imag * imag).sqrt() / n as f64
        })
        .collect()
}

fn warp_render(mode: WarpMode, amount: f64) -> Vec<f64> {
    warp_render_slots(mode, amount, WarpMode::Off, 0.0)
}

fn warp_render_slots(mode1: WarpMode, amount1: f64, mode2: WarpMode, amount2: f64) -> Vec<f64> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Saw,
        unison_voices: 1,
        detune_cents: 0.0,
        stereo_width: 0.0,
        sustain: 1.0,
        decay: 0.1,
        attack: 0.001,
        oscillator_a_warp_mode: mode1,
        oscillator_a_warp_amount: amount1,
        oscillator_a_warp2_mode: mode2,
        oscillator_a_warp2_amount: amount2,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 57, 112);
    let mut output = Vec::new();
    for _ in 0..16_384 {
        let frame = synth.next_stereo(48_000);
        output.push(frame[0] as f64);
    }
    output
}

fn harmonic_db(samples: &[f64], f0: f64, count: usize) -> Vec<f64> {
    let amps = harmonic_amplitudes(samples, 48_000.0, f0, count);
    amps.iter().map(|a| 20.0 * a.log10()).collect()
}

fn correlation(first: &[f64], second: &[f64]) -> f64 {
    let n = first.len().min(second.len());
    let a = &first[..n];
    let b = &second[..n];
    let mean_a = a.iter().sum::<f64>() / n as f64;
    let mean_b = b.iter().sum::<f64>() / n as f64;
    let mut cross = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b) {
        let x = x - mean_a;
        let y = y - mean_b;
        cross += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    cross / (norm_a * norm_b).sqrt()
}

fn rms(samples: &[f64]) -> f64 {
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

const WARP_TEST_F0: f64 = 220.0;

#[test]
fn warp_identity_points_preserve_the_readout() {
    let off = warp_render(WarpMode::Off, 0.0);
    for (mode, knob) in [
        (WarpMode::BendPos, 0.0),
        (WarpMode::BendNeg, 0.0),
        (WarpMode::BendPosNeg, 0.5),
        (WarpMode::AsymPos, 0.0),
        (WarpMode::AsymNeg, 0.0),
        (WarpMode::AsymPosNeg, 0.5),
        (WarpMode::Pwm, 0.0),
        (WarpMode::Flip, 0.0),
        (WarpMode::Remap1, 0.5),
        (WarpMode::Quantize, 0.0),
        (WarpMode::EvenOdd, 0.5),
        (WarpMode::SelfPd, 0.0),
        (WarpMode::RmNoise, 0.0),
    ] {
        let warped = warp_render(mode, knob);
        let corr = correlation(&warped, &off);
        assert!(
            corr > 0.9999,
            "{mode:?} at knob {knob} is not identity: {corr}"
        );
    }
    // sync at knob 0 has unit increment, but its window runs one sample
    // ahead of the phase, so the window wrap restarts the phase one sample
    // early (phase is read before the increment advances).
    let corr = correlation(&warp_render(WarpMode::Sync, 0.0), &off);
    assert!(corr > 0.98, "sync at knob 0: {corr}");
}

#[test]
fn even_odd_adds_odd_harmonics_and_cancels_evens() {
    let off = harmonic_db(&warp_render(WarpMode::Off, 0.0), WARP_TEST_F0, 8);
    let even_odd = harmonic_db(&warp_render(WarpMode::EvenOdd, 0.0), WARP_TEST_F0, 8);
    for harmonic in 0..8 {
        let delta = even_odd[harmonic] - off[harmonic];
        if harmonic % 2 == 0 {
            assert!(
                (delta - 6.02).abs() < 0.05,
                "H{} must rise by 6.02 dB, got {delta}",
                harmonic + 1
            );
        } else {
            assert!(
                even_odd[harmonic] - even_odd[0] < -100.0,
                "H{} must cancel, got {} dB",
                harmonic + 1,
                even_odd[harmonic] - even_odd[0]
            );
        }
    }
    let half = warp_render(WarpMode::EvenOdd, 0.5);
    assert_eq!(half, warp_render(WarpMode::Off, 0.0));
}

#[test]
fn flip_negates_the_readout_at_half_amount() {
    let off = warp_render(WarpMode::Off, 0.0);
    let flipped = warp_render(WarpMode::Flip, 0.5);
    let corr = correlation(&flipped, &off);
    assert!(corr < -0.9999, "flip correlation {corr}");
    assert!((rms(&flipped) / rms(&off) - 1.0).abs() < 1.0e-6);
}

#[test]
fn remap_folds_are_amount_independent_integer_reads() {
    let off = warp_render(WarpMode::Off, 0.0);
    assert_eq!(warp_render(WarpMode::Remap1, 0.25), off);
    for mode in [WarpMode::Remap2, WarpMode::Remap3, WarpMode::Remap4] {
        assert_eq!(
            warp_render(mode, 0.25),
            warp_render(mode, 0.75),
            "{mode:?} must be amount independent"
        );
    }
    let folds = [
        (WarpMode::Remap2, 2usize),
        (WarpMode::Remap3, 3usize),
        (WarpMode::Remap4, 4usize),
    ];
    for (mode, fold) in folds {
        let spectrum = harmonic_db(&warp_render(mode, 0.5), WARP_TEST_F0, 10);
        let fundamental = spectrum[fold - 1];
        assert!(
            fundamental - spectrum[0] > 60.0,
            "{mode:?} H1 must vanish: {} dB",
            spectrum[0] - fundamental
        );
        assert!(
            (spectrum[2 * fold - 1] - (fundamental - 6.02)).abs() < 1.0,
            "{mode:?} H{} must sit near H{fold} - 6 dB",
            2 * fold
        );
    }
}

#[test]
fn sync_scales_the_increment_and_restarts_each_note_cycle() {
    assert!((warp_mod::sync_factor(0.5) - 2.875).abs() < 1.0e-12);
    let off = harmonic_db(&warp_render(WarpMode::Off, 0.0), WARP_TEST_F0, 8);
    let sync = harmonic_db(&warp_render(WarpMode::Sync, 0.5), WARP_TEST_F0, 8);
    let deltas: Vec<f64> = sync.iter().zip(&off).map(|(a, b)| a - b).collect();
    assert!((deltas[0] + 19.0).abs() < 3.0, "H1 delta {}", deltas[0]);
    assert!((deltas[2] - 9.3).abs() < 2.0, "H3 delta {}", deltas[2]);
    assert!(
        sync[2] - sync[0] > 15.0,
        "H3 must dominate, H3-H1 = {}",
        sync[2] - sync[0]
    );
    // The window wrap restarts the phase every note cycle, so the output is
    // periodic at f0: no f0/4 partials.
    let samples = warp_render(WarpMode::Sync, 0.5);
    let quarter = harmonic_amplitudes(&samples, 48_000.0, WARP_TEST_F0 / 4.0, 1)[0];
    let third = harmonic_amplitudes(&samples, 48_000.0, WARP_TEST_F0, 3)[2];
    assert!(
        third / quarter > 100.0,
        "f0/4 energy must stay far below H3: {} dB",
        20.0 * (quarter / third).log10()
    );
}

#[test]
fn dlm_reads_a_triangle() {
    let spectrum = harmonic_db(&warp_render(WarpMode::Dlm, 0.5), WARP_TEST_F0, 8);
    assert!(
        spectrum[1] - spectrum[0] < -60.0,
        "triangle H2 {}",
        spectrum[1]
    );
    assert!(
        (spectrum[2] - spectrum[0] + 20.0).abs() < 2.0,
        "H3 {}",
        spectrum[2]
    );
    assert!(
        (spectrum[4] - spectrum[0] + 30.0).abs() < 5.0,
        "H5 {}",
        spectrum[4]
    );
}

/// The acceptance levels are measured on the `Saw Alice` frame; its
/// rounded discontinuity shifts the large-signal H1 against a pure saw.
fn imported_saw_table() -> Option<CustomWavetable> {
    let root = std::env::var_os("OCP_PRESET_TABLES")?;
    let path = std::path::PathBuf::from(root).join("S2 Tables/Analog/Saw Alice.wav");
    CustomWavetable::load_imported(path.to_str()?).ok()
}

fn render_with_table(table: CustomWavetable, mode: WarpMode, amount: f64) -> Vec<f64> {
    let patch = SynthPatch {
        oscillator_a_waveform: Waveform::Wavetable,
        custom_wavetable: Some(table),
        unison_voices: 1,
        detune_cents: 0.0,
        stereo_width: 0.0,
        sustain: 1.0,
        decay: 0.1,
        attack: 0.001,
        oscillator_a_warp_mode: mode,
        oscillator_a_warp_amount: amount,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 57, 112);
    let mut output = Vec::new();
    for _ in 0..16_384 {
        let frame = synth.next_stereo(48_000);
        output.push(frame[0] as f64);
    }
    output
}

#[test]
fn distortion_modes_match_the_measured_levels() {
    let Some(table) = imported_saw_table() else {
        return;
    };
    let off = harmonic_db(
        &render_with_table(table.clone(), WarpMode::Off, 0.0),
        WARP_TEST_F0,
        8,
    );
    for (mode, expected) in [(WarpMode::DistTube, 2.66), (WarpMode::DistSoftSat, 7.31)] {
        let spectrum = harmonic_db(
            &render_with_table(table.clone(), mode, 0.5),
            WARP_TEST_F0,
            8,
        );
        let delta = spectrum[0] - off[0];
        assert!(
            (delta - expected).abs() < 0.5,
            "{mode:?} H1 {delta} dB, expected {expected}"
        );
    }
    let fold = harmonic_db(
        &render_with_table(table, WarpMode::DistLinFold, 0.5),
        WARP_TEST_F0,
        8,
    );
    let delta = fold[0] - off[0];
    assert!(delta < -10.0, "fold H1 must drop, got {delta}");
    assert!(
        fold[7] - fold[0] > 2.0,
        "fold must lift H8 above the depressed H1"
    );
}

#[test]
fn distortion_shapers_are_odd_and_keep_the_fundamental_scale() {
    // Small-signal gains from the drive curves, independent of the table.
    for (mode, gain) in [
        (WarpMode::DistTube, 1.0 / (1.0 - 0.989 * 0.5)),
        (WarpMode::DistHardClip, 1.0 + 4.0 * 0.125),
        (WarpMode::DistSoftSat, 1.0 + 16.0 * 0.25),
    ] {
        let shaper =
            |x: f64| warp_mod::shaper(x, warp_mod::amount_curve(mode, 0.5), mode.dist_id());
        let small = 1.0e-4;
        assert!(
            (shaper(small) / small - gain).abs() < 0.01,
            "{mode:?} small-signal gain {}",
            shaper(small) / small
        );
        assert!((shaper(-small) / small + gain).abs() < 0.01);
    }
}

#[test]
fn two_slots_apply_slot_two_before_slot_one() {
    let knob1 = 0.7;
    let knob2 = 0.3;
    let phase = 0.37;
    let warp = WarpParams::new(WarpMode::BendPos, knob1, 0.0, WarpMode::Pwm, knob2, 0.0);
    let mut mip = MipState::default();
    let actual = oscillator_sample_with_warp(
        Waveform::Sine,
        phase,
        0.001,
        0.001,
        0.5,
        0.0,
        None,
        warp,
        0.0,
        &mut mip,
    );
    let (after_two, _) = warp_mod::phase_warp(
        WarpMode::Pwm,
        warp_mod::amount_curve(WarpMode::Pwm, knob2),
        phase,
    );
    let (after_one, _) = warp_mod::phase_warp(
        WarpMode::BendPos,
        warp_mod::amount_curve(WarpMode::BendPos, knob1),
        after_two,
    );
    let expected = read_warped(
        Waveform::Sine,
        after_one,
        0.001,
        0.001,
        0.5,
        0.0,
        None,
        &mut MipState::default(),
        0.0,
    );
    assert!((actual - expected).abs() < 1.0e-12);
    let swapped = WarpParams::new(WarpMode::Pwm, knob2, 0.0, WarpMode::BendPos, knob1, 0.0);
    let other = oscillator_sample_with_warp(
        Waveform::Sine,
        phase,
        0.001,
        0.001,
        0.5,
        0.0,
        None,
        swapped,
        0.0,
        &mut MipState::default(),
    );
    assert!((other - actual).abs() > 1.0e-6, "slot order must matter");
}

#[test]
fn inert_warp_families_are_off_or_a_flat_gain() {
    let off = warp_render(WarpMode::Off, 0.0);
    for mode in WarpMode::ALL {
        if matches!(mode as u8, 33..=56 | 58..=65) {
            assert_eq!(warp_render(mode, 0.5), off, "{mode:?} must be inert");
        }
    }
    for mode in [
        WarpMode::RmNoise,
        WarpMode::RmSub,
        WarpMode::RmFilt1,
        WarpMode::RmFilt2,
    ] {
        for amount in [0.25, 0.5, 0.75] {
            let rendered = warp_render(mode, amount);
            assert!((correlation(&rendered, &off) - 1.0).abs() < 1.0e-9);
            let expected = 1.0 - amount;
            assert!(
                (rms(&rendered) / rms(&off) - expected).abs() < 1.0e-6,
                "{mode:?} at {amount} must be a flat 1-a gain"
            );
        }
    }
}

#[test]
fn default_triangle_matches_the_retired_triangle_source() {
    let shape = LfoShape::triangle();
    for step in 0..48_001 {
        let phase = step as f64 / 48_000.0;
        let old = 1.0 - 2.0 * (phase - 0.5).abs();
        let new = curve_shape_value(&shape, phase);
        assert_eq!(new as f32, old as f32, "phase {phase}");
    }
}

#[test]
fn curve_bend_half_is_straight_and_bends_are_monotonic() {
    let mut shape = LfoShape::from_points(&[0.0, 1.0], &[0.0, 1.0], &[0.5, 0.5]);
    assert!((curve_shape_value(&shape, 0.25) - 0.25).abs() < 1e-12);
    shape.curve[0] = 0.25;
    let early = curve_shape_value(&shape, 0.25);
    shape.curve[0] = 0.75;
    let late = curve_shape_value(&shape, 0.25);
    assert!(early > 0.25 && late < 0.25, "early={early} late={late}");
    shape.curve[0] = 0.5;
    assert!((curve_shape_value(&shape, 0.25) - 0.25).abs() < 1e-12);
}

#[test]
fn curve_wraps_and_reads_arbitrary_breakpoints() {
    let shape = LfoShape::from_points(&[0.0, 0.25, 0.75, 1.0], &[0.0, 1.0, 0.0, 1.0], &[0.5; 4]);
    assert!((curve_shape_value(&shape, 0.0) - 0.0).abs() < 1e-12);
    assert!((curve_shape_value(&shape, 0.25) - 1.0).abs() < 1e-12);
    assert!((curve_shape_value(&shape, 0.5) - 0.5).abs() < 1e-12);
    assert!((curve_shape_value(&shape, 0.75) - 0.0).abs() < 1e-12);
    assert!((curve_shape_value(&shape, 1.0) - curve_shape_value(&shape, 0.0)).abs() < 1e-12);
    // The path generator is cyclic and stays in the source range.
    for step in 0..2_000 {
        let value = path_shape_value(&shape, step as f64 / 2_000.0);
        assert!((0.0..=1.0).contains(&value), "path value {value}");
    }
}

#[test]
fn shape_types_render_deterministically() {
    for shape_type in [
        LfoShapeType::Curve,
        LfoShapeType::Path,
        LfoShapeType::ChaosA,
        LfoShapeType::ChaosB,
        LfoShapeType::RandomHold,
    ] {
        let mut patch = SynthPatch {
            lfo_shape_type: shape_type,
            lfo_shape: std::sync::Arc::new(LfoShape::from_points(
                &[0.0, 0.4, 0.7, 1.0],
                &[0.2, 1.0, 0.1, 0.6],
                &[0.5, 0.25, 0.8, 0.5],
            )),
            lfo_rate: 7.0,
            lfo_retrigger: true,
            ..Default::default()
        };
        patch.mod_routes[0] = ModRoute {
            source: ModSource::Lfo,
            destination: ModDestination::Pitch,
            amount: 0.5,
            ..ModRoute::default()
        };
        let mut a = OpenSynth::new(patch.clone());
        let mut b = OpenSynth::new(patch);
        a.note_on(0, 60, 100);
        b.note_on(0, 60, 100);
        for _ in 0..4_096 {
            assert_eq!(
                a.next_stereo(48_000),
                b.next_stereo(48_000),
                "{shape_type:?}"
            );
            assert!((0.0..=1.0).contains(&a.lfo_runtime[0].output));
        }
        assert_eq!(a.lfo_runtime[0].output, b.lfo_runtime[0].output);
    }
}

#[test]
fn chaotic_generators_are_bounded_and_not_periodic() {
    for shape_type in [LfoShapeType::ChaosA, LfoShapeType::ChaosB] {
        let mut state = lfo_chaos_seed(0);
        let mut values = Vec::with_capacity(20_000);
        for _ in 0..20_000 {
            let value = chaos_step(shape_type, &mut state);
            assert!(value.is_finite() && (0.0..=1.0).contains(&value));
            values.push(value);
        }
        // A short period would show up as an exact repeated window.
        for gap in 1..=512 {
            let repeated = values[..2_048]
                .iter()
                .zip(&values[gap..gap + 2_048])
                .all(|(a, b)| a == b);
            assert!(!repeated, "{shape_type:?} repeated with period {gap}");
        }
        // Neighbouring states diverge, the defining property of chaos.
        let mut first = lfo_chaos_seed(0);
        let mut second = lfo_chaos_seed(0);
        second[0] += 1e-12;
        let mut divergence: f64 = 0.0;
        for _ in 0..256 {
            divergence = divergence.max(
                (chaos_step(shape_type, &mut first) - chaos_step(shape_type, &mut second)).abs(),
            );
        }
        assert!(divergence > 0.1, "{shape_type:?} divergence {divergence}");
    }
    let mut first = lfo_chaos_seed(3);
    let mut second = lfo_chaos_seed(3);
    let mut different = false;
    for _ in 0..1_000 {
        if chaos_step(LfoShapeType::ChaosA, &mut first)
            != chaos_step(LfoShapeType::ChaosB, &mut second)
        {
            different = true;
        }
    }
    assert!(different, "the two generators must be distinct");
}

#[test]
fn lfo_rate_accuracy_and_x10() {
    let measured = |rate: f64, x10: bool| {
        let increment = super::phase_increment(free_lfo_rate(rate, x10), 48_000);
        increment as f64 / super::PHASE_SCALE * 48_000.0
    };
    for rate in [1.0, 8.0, 16.0, 100.0] {
        let error = (measured(rate, false) - rate).abs();
        assert!(
            error < 1e-6,
            "rate {rate} measured {}",
            measured(rate, false)
        );
    }
    assert!((measured(8.0, true) - 80.0).abs() < 1e-6);
    assert_eq!(
        free_lfo_rate(120.0, false),
        100.0,
        "the knob stops at 100 Hz"
    );
}

#[test]
fn sync_ladder_has_43_entries_and_a_quarter_note_centre() {
    assert_eq!(super::LFO_SYNC_DIVISIONS.len(), 43);
    assert!((super::LFO_SYNC_DIVISIONS[22] - 1.0).abs() < 1e-12);
    for pair in super::LFO_SYNC_DIVISIONS.windows(2) {
        assert!(pair[0] > pair[1], "ladder must descend");
    }
    assert!((synced_lfo_rate(120.0, 2, Some(22)) - 2.0).abs() < 1e-12);
    assert!((synced_lfo_rate(120.0, 2, None) - 2.0).abs() < 1e-12);
    // The retired six-step selector keeps its old meaning.
    assert!((synced_lfo_rate(120.0, 4, None) - 0.5).abs() < 1e-12);
}

#[test]
fn free_running_lfos_ignore_phase_and_retriggered_ones_apply_it() {
    let render = |phase: f64, retrigger: bool| {
        let mut patch = SynthPatch {
            lfo_phase: phase,
            lfo_retrigger: retrigger,
            lfo_rate: 3.0,
            lfo_shape: std::sync::Arc::new(LfoShape::saw()),
            ..Default::default()
        };
        patch.mod_routes[0] = ModRoute {
            source: ModSource::Lfo,
            destination: ModDestination::Pitch,
            amount: 0.5,
            ..ModRoute::default()
        };
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        let mut outputs = Vec::new();
        for _ in 0..2_048 {
            synth.next_stereo(48_000);
            outputs.push(synth.lfo_runtime[0].output);
        }
        outputs
    };
    let free: Vec<Vec<f64>> = [0.0, 90.0, 180.0, 270.0]
        .into_iter()
        .map(|phase| render(phase, false))
        .collect();
    assert_eq!(free[0], free[1]);
    assert_eq!(free[0], free[2]);
    assert_eq!(free[0], free[3]);
    let triggered = render(90.0, true);
    assert!((triggered[0] - 0.25).abs() < 1e-9, "{}", triggered[0]);
    assert!((render(270.0, true)[0] - 0.75).abs() < 1e-9);
    assert_ne!(triggered[0], render(0.0, true)[0]);
}

#[test]
fn lfo_source_updates_every_sample() {
    let patch = SynthPatch {
        lfo_rate: 50.0,
        lfo_shape: std::sync::Arc::new(LfoShape::triangle()),
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut previous = synth.lfo_runtime[0].output;
    let mut changes = 0;
    for _ in 0..1_000 {
        synth.next_stereo(48_000);
        let value = synth.lfo_runtime[0].output;
        if value != previous {
            changes += 1;
        }
        previous = value;
    }
    assert!(changes > 990, "only {changes} per-sample updates");
}

#[test]
fn smooth_parameter_lags_the_source_from_the_midpoint() {
    let patch = SynthPatch {
        lfo_shape: std::sync::Arc::new(LfoShape::from_points(
            &[0.0, 1.0],
            &[1.0, 1.0],
            &[0.5, 0.5],
        )),
        lfo_smooth: 0.1,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut previous = synth.lfo_runtime[0].output;
    for _ in 0..512 {
        synth.next_stereo(48_000);
        let value = synth.lfo_runtime[0].output;
        assert!(value >= previous && value <= 1.0);
        previous = value;
    }
    assert!(previous > 0.9, "{previous}");
}

#[test]
fn lfo_bend_skews_phase_without_moving_the_endpoints() {
    assert_eq!(warp_lfo_phase(0.0, 0.75), 0.0);
    assert_eq!(warp_lfo_phase(1.0, 0.75), 1.0);
    assert!((warp_lfo_phase(0.5, 0.5) - 0.25).abs() < 1e-12);
    assert!((warp_lfo_phase(0.5, -0.5) - 0.5f64.sqrt()).abs() < 1e-12);
    assert_eq!(warp_lfo_phase(0.5, 0.0), 0.5);
}

#[test]
fn route_curve_is_odd_and_monotonic() {
    assert_eq!(shape_mod_value(0.25, 0.0), 0.25);
    let soft = shape_mod_value(0.5, -1.0);
    let hard = shape_mod_value(0.5, 1.0);
    assert!((soft - 0.5f64.powf(0.25)).abs() < 1e-12);
    assert!((hard - 0.5f64.powf(4.0)).abs() < 1e-12);
    assert_eq!(shape_mod_value(-0.5, 1.0), -hard);
    assert!(shape_mod_value(0.3, -1.0) < shape_mod_value(0.6, -1.0));
}

#[test]
fn route_slew_reaches_target_without_overshoot() {
    let up = 1.0 - (-1.0 / (0.01f64 * 48_000.0)).exp();
    let down = 1.0 - (-1.0 / (0.05f64 * 48_000.0)).exp();
    let mut state = 0.0;
    let mut previous = 0.0;
    for _ in 0..48_000 {
        state = slew_step(state, 1.0, up, down);
        assert!(state >= previous && state <= 1.0);
        previous = state;
    }
    assert!((state - 1.0).abs() < 1e-9);
    let mut falling = 1.0;
    let mut previous = 1.0;
    for _ in 0..48_000 {
        falling = slew_step(falling, -1.0, up, down);
        assert!(falling <= previous && falling >= -1.0);
        previous = falling;
    }
    assert!((falling + 1.0).abs() < 1e-6);

    // Different rise/fall times: 10 ms up covers less ground than 10 ms down.
    let mut rising = 0.0;
    let mut falling = 1.0;
    for _ in 0..480 {
        rising = slew_step(rising, 1.0, up, down);
        falling = slew_step(falling, 0.0, up, down);
    }
    assert!(rising < falling);
}

#[test]
fn voice_route_slew_state_moves_monotonically_to_the_route_target() {
    let mut patch = SynthPatch::default();
    patch.macros[0] = 1.0;
    patch.mod_routes[0] = ModRoute {
        source: ModSource::Macro1,
        destination: ModDestination::FilterCutoff,
        amount: 1.0,
        slew_up: 0.01,
        slew_down: 0.05,
        ..ModRoute::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut previous = 0.0;
    for _ in 0..4_800 {
        synth.next_stereo(48_000);
        let state = synth.voices[0].route_slew[0];
        assert!(state >= previous);
        previous = state;
    }
    assert!((previous - 11.4).abs() < 1e-3);
}

/// Every learn-table destination must be reachable through the numeric
/// `mod.N.destination` ABI; a learn must never write an unloadable route.
#[test]
fn catalog_learn_targets_resolve_to_reachable_destinations() {
    let catalog = oc_dsp::schema::synth::synth_catalog();
    for target in catalog.mod_learn_targets {
        let destination = mod_destination_from_value(target.destination as f64);
        assert_eq!(
            mod_destination_value(destination),
            Some(target.destination as u8),
            "{} targets an unreachable destination {}",
            target.control,
            target.destination
        );
    }
}

/// The macro-sweep acceptance for macro learn: the learned route must move
/// its destination parameter as the learned macro is driven 0 → 1, and it
/// must do so through the same `set_normalized_param` path automation uses.
#[test]
fn learned_macro_route_moves_the_destination_monotonically() {
    let mut patch = SynthPatch {
        filter_cutoff: 200.0,
        filter_model: FilterModel::Svf,
        filter_resonance: 0.0,
        ..Default::default()
    };
    let macro_source = macro_mod_source(0).unwrap();
    assert_eq!(macro_source, ModSource::Macro1);
    patch.mod_routes[0] = ModRoute {
        source: macro_source,
        destination: ModDestination::FilterCutoff,
        amount: 0.5,
        ..ModRoute::default()
    };
    assert_eq!(mod_source_value(macro_source), 3);
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut previous = 0.0;
    for step in 0..=10 {
        synth.set_normalized_param("macro.1", f64::from(step) / 10.0);
        // The filter coefficient is refreshed at control rate (32 samples).
        for _ in 0..64 {
            synth.next_stereo(48_000);
        }
        let coefficient = synth.voices[0].filter_coefficients.g;
        assert!(
            coefficient > previous,
            "macro step {step}: cutoff coefficient did not rise ({coefficient} <= {previous})"
        );
        previous = coefficient;
    }
}

/// `bipolar` and `aux` are settable parameters, not only JSON fields.
#[test]
fn route_bipolar_and_aux_params_change_the_rendered_cutoff() {
    let render = |patch: &SynthPatch| {
        let mut synth = OpenSynth::new(patch.clone());
        synth.note_on(0, 60, 100);
        for _ in 0..64 {
            synth.next_stereo(48_000);
        }
        synth.voices[0].filter_coefficients.g
    };
    let base = SynthPatch {
        filter_cutoff: 400.0,
        filter_model: FilterModel::Svf,
        filter_resonance: 0.0,
        ..Default::default()
    };
    let mut gated = base.clone();
    gated.set_param("macro.1", 1.0);
    gated.set_param("mod.1.source", 3.0);
    gated.set_param("mod.1.destination", 0.0);
    gated.set_param("mod.1.amount", 0.5);
    gated.set_param("mod.1.aux", 5.0); // auxiliary = Macro 2
    let closed = render(&gated);
    gated.set_param("macro.2", 1.0);
    let open = render(&gated);
    assert!(open > closed, "aux gate did not open the route");
    gated.set_param("mod.1.aux_invert", 1.0);
    let inverted = render(&gated);
    assert!(
        inverted < closed,
        "inverted aux did not flip the route direction"
    );

    // A bipolar route re-centres a unipolar macro, so macro 0 pushes down.
    let mut bipolar = base.clone();
    bipolar.set_param("macro.1", 0.0);
    bipolar.set_param("mod.1.source", 3.0);
    bipolar.set_param("mod.1.destination", 0.0);
    bipolar.set_param("mod.1.amount", 0.25);
    let unipolar = render(&bipolar);
    bipolar.set_param("mod.1.bipolar", 1.0);
    let centred = render(&bipolar);
    assert!(
        centred < unipolar,
        "bipolar did not centre the unipolar macro on zero"
    );
}

#[test]
fn warp_flags_match_the_original_mode_predicates() {
    for mode in WarpMode::ALL {
        let slot = WarpSlot::new(mode, 0.5, 0.5);
        assert_eq!(slot.has(WARP_PHASE), mode.is_phase_warp(), "{mode:?}");
        assert_eq!(slot.has(WARP_FLIP), mode == WarpMode::Flip, "{mode:?}");
        assert_eq!(slot.has(WARP_SELFPD), mode == WarpMode::SelfPd, "{mode:?}");
        assert_eq!(
            slot.has(WARP_EVENODD),
            mode == WarpMode::EvenOdd,
            "{mode:?}"
        );
        assert_eq!(slot.has(WARP_DIST), mode.is_dist(), "{mode:?}");
        assert_eq!(slot.has(WARP_SYNC), mode == WarpMode::Sync, "{mode:?}");
        assert_eq!(slot.has(WARP_RM_GAIN), mode.is_rm_gain(), "{mode:?}");
        assert_eq!(slot.flags == 0, mode.is_bypass(), "{mode:?}");
    }
}

#[test]
fn duplicate_route_grouping_preserves_the_sample_stream() {
    let mut patch = SynthPatch::default();
    patch.macros[0] = 0.7;
    patch.mod_routes = (0..8)
        .map(|_| ModRoute {
            source: ModSource::Macro1,
            destination: ModDestination::FilterCutoff,
            amount: 0.5,
            ..ModRoute::default()
        })
        .collect();
    let mut grouped = OpenSynth::new(patch.clone());
    let mut plain = OpenSynth::new(patch);
    assert_eq!(grouped.route_group_count, 1);
    assert_eq!(grouped.route_groups[0].len, 8);
    // Expand the grouped run back to one route per group as the reference.
    plain.route_group_count = plain.compiled_mod_route_count;
    for index in 0..plain.compiled_mod_route_count {
        plain.route_groups[index] = RouteGroup {
            first: index as u8,
            len: 1,
        };
    }
    grouped.note_on(0, 60, 100);
    plain.note_on(0, 60, 100);
    for _ in 0..2_000 {
        let [grouped_left, grouped_right] = grouped.next_stereo(48_000);
        let [plain_left, plain_right] = plain.next_stereo(48_000);
        assert_eq!(grouped_left.to_bits(), plain_left.to_bits());
        assert_eq!(grouped_right.to_bits(), plain_right.to_bits());
    }
}

#[test]
fn zero_attack_starts_from_silence_and_ramps_monotonically() {
    let patch = SynthPatch {
        attack: 0.0,
        amp_curve: -1.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let mut levels = Vec::new();
    for _ in 0..72 {
        synth.next_stereo(48_000);
        levels.push(synth.voices[0].amplitude_level);
    }
    let floor = 1.0 / (MIN_ENVELOPE_ATTACK_SECONDS * 48_000.0);
    assert!((levels[0] - floor).abs() < 1e-9);
    assert!(levels[..48].windows(2).all(|pair| pair[1] >= pair[0]));
    assert!((levels[47] - 1.0).abs() < 1e-9);
    assert!(levels[0] < 0.05);
}

#[test]
fn zero_attack_output_does_not_start_hot() {
    let patch = SynthPatch {
        attack: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let first = synth.next_stereo(48_000);
    assert!(first.iter().all(|sample| sample.abs() < 0.02));
}

#[test]
fn new_modulation_parameters_round_trip_through_set_param_and_json() {
    let mut patch = SynthPatch::default();
    patch.set_param("lfo.curve.0", -0.75);
    patch.set_param("lfo.curve.255", 0.5);
    patch.set_param("lfo.bend", -0.25);
    patch.set_param("lfo.smooth", 0.4);
    patch.set_param("lfo.rise", 0.75);
    patch.set_param("lfo.delay", 0.125);
    patch.set_param("lfo.phase", 90.0);
    patch.set_param("lfo.rate_x10", 1.0);
    patch.set_param("lfo.sync_div", 22.0);
    patch.set_param("lfo.shape", 1.0);
    patch.set_param("lfo2.bend", 0.5);
    patch.set_param("lfo2.smooth", 0.5);
    patch.set_param("lfo2.rise", 1.0);
    patch.set_param("lfo2.delay", 0.25);
    patch.set_param("lfo2.rate_x10", 1.0);
    patch.set_param("mod.4.curve", 0.5);
    patch.set_param("mod.4.slew_up", 0.02);
    patch.set_param("mod.4.slew_down", 0.2);
    patch.set_param("mod.4.bipolar", 1.0);
    patch.set_param("mod.4.aux", 5.0);
    patch.set_param("mod.4.aux_invert", 1.0);
    patch.lfo_shape = std::sync::Arc::new(LfoShape::from_points(
        &[0.0, 0.25, 0.5, 1.0],
        &[0.1, 1.0, 0.2, 0.9],
        &[0.5, 0.3, 0.7, 0.5],
    ));

    assert_eq!(patch.lfo_curve[0], -0.75);
    assert_eq!(patch.lfo_curve[255], 0.5);
    assert_eq!((patch.lfo_bend, patch.lfo_smooth), (-0.25, 0.4));
    assert_eq!((patch.lfo_rise, patch.lfo_delay), (0.75, 0.125));
    assert_eq!((patch.lfo_phase, patch.lfo_rate_x10), (90.0, true));
    assert_eq!(patch.lfo_sync_div, Some(22));
    assert_eq!(patch.lfo_shape_type, LfoShapeType::Path);
    assert_eq!(patch.extra_lfos[0].bend, 0.5);
    assert_eq!(patch.extra_lfos[0].smooth, 0.5);
    assert_eq!(patch.extra_lfos[0].rise, 1.0);
    assert_eq!(patch.extra_lfos[0].delay, 0.25);
    assert!(patch.extra_lfos[0].rate_x10);
    assert_eq!(
        (patch.mod_routes[3].curve, patch.mod_routes[3].slew_up),
        (0.5, 0.02)
    );
    assert_eq!(patch.mod_routes[3].slew_down, 0.2);
    assert!(patch.mod_routes[3].bipolar);
    assert_eq!(patch.mod_routes[3].auxiliary, Some(ModSource::Macro2));
    assert!(patch.mod_routes[3].auxiliary_inverted);

    let json = serde_json::to_value(&patch).unwrap();
    assert_eq!(
        json["lfo_curve"].as_array().unwrap().len(),
        LFO_CURVE_POINTS
    );
    assert_eq!(json["lfo_shape"]["points"], 4);
    assert_eq!(json["lfo_shape"]["x"].as_array().unwrap().len(), 480);
    let restored: SynthPatch = serde_json::from_slice(&serde_json::to_vec(&patch).unwrap())
        .expect("new modulation fields must round-trip");
    assert_eq!(restored.lfo_curve, patch.lfo_curve);
    assert_eq!(restored.lfo_shape, patch.lfo_shape);
    assert_eq!(restored.lfo_shape_type, patch.lfo_shape_type);
    assert_eq!(restored.lfo_phase, 90.0);
    assert!(restored.lfo_rate_x10);
    assert_eq!(restored.lfo_sync_div, Some(22));
    assert_eq!(restored.lfo_bend, patch.lfo_bend);
    assert_eq!(restored.lfo_smooth, patch.lfo_smooth);
    assert_eq!(restored.lfo_rise, patch.lfo_rise);
    assert_eq!(restored.lfo_delay, patch.lfo_delay);
    assert_eq!(restored.extra_lfos, patch.extra_lfos);
    assert_eq!(restored.mod_routes[3].curve, 0.5);
    assert_eq!(restored.mod_routes[3].slew_up, 0.02);
    assert_eq!(restored.mod_routes[3].slew_down, 0.2);
    assert!(restored.mod_routes[3].bipolar);
    assert_eq!(restored.mod_routes[3].auxiliary, Some(ModSource::Macro2));
    assert!(restored.mod_routes[3].auxiliary_inverted);

    patch.set_normalized_param("lfo.curve.3", 1.0);
    patch.set_normalized_param("lfo.bend", 0.0);
    patch.set_normalized_param("lfo.rise", 1.0);
    patch.set_normalized_param("mod.1.curve", 1.0);
    patch.set_normalized_param("mod.1.slew_up", 1.0);
    patch.set_normalized_param("mod.1.bipolar", 1.0);
    patch.set_normalized_param("mod.1.aux", 1.0);
    patch.set_normalized_param("mod.1.aux_invert", 1.0);
    assert_eq!(patch.lfo_curve[3], 1.0);
    assert_eq!(patch.lfo_bend, -1.0);
    assert_eq!(patch.lfo_rise, 5.0);
    assert_eq!(patch.mod_routes[0].curve, 1.0);
    assert_eq!(patch.mod_routes[0].slew_up, MAX_ROUTE_SLEW_SECONDS);
    assert!(patch.mod_routes[0].bipolar);
    assert_eq!(patch.mod_routes[0].auxiliary, Some(ModSource::Timbre));
    assert!(patch.mod_routes[0].auxiliary_inverted);
    // 0 disables the auxiliary source again.
    patch.set_param("mod.1.aux", 0.0);
    assert_eq!(patch.mod_routes[0].auxiliary, None);
}

#[test]
fn legacy_waveform_and_curve_params_fold_into_the_shape() {
    let mut patch = SynthPatch::default();
    patch.set_normalized_param("lfo.waveform", 4.0 / 6.0);
    assert_eq!(patch.lfo_shape_type, LfoShapeType::RandomHold);
    let mut saw = SynthPatch::default();
    saw.set_param("lfo.waveform", 2.0);
    assert_eq!(*saw.lfo_shape, LfoShape::saw());
    let mut curved = SynthPatch::default();
    curved.set_param("lfo.waveform", 5.0);
    assert_eq!(curved.lfo_shape.points, LFO_CURVE_POINTS);
    curved.set_param("lfo.curve.32", 1.0);
    assert!((curved.lfo_shape.y[32] - 1.0).abs() < 1e-6);
    // Old JSON names still parse, then fold in on load.
    let settings: LfoSettings = serde_json::from_str(
        r#"{"rate":1.0,"waveform":"Sine","sync":false,"retrigger":false,"division":2}"#,
    )
    .unwrap();
    assert_eq!(settings.legacy_waveform, Some(LegacyLfoWaveform::Sine));
    let mut legacy = SynthPatch {
        lfo_shape: std::sync::Arc::new(LfoShape::saw()),
        legacy_lfo_waveform: Some(LegacyLfoWaveform::Triangle),
        ..Default::default()
    };
    legacy.normalize_legacy_lfo();
    assert_eq!(*legacy.lfo_shape, LfoShape::triangle());
    // A retired triangle patch renders exactly like the new default shape.
    let render = |patch: SynthPatch| {
        let mut synth = OpenSynth::new(patch);
        synth.note_on(0, 60, 100);
        (0..2_048)
            .flat_map(|_| synth.next_stereo(48_000))
            .collect::<Vec<_>>()
    };
    let mut triangle = SynthPatch::default();
    triangle.set_param("lfo.waveform", 1.0);
    triangle.lfo_rate = 4.0;
    triangle.mod_routes[0] = ModRoute {
        source: ModSource::Lfo,
        destination: ModDestination::Pitch,
        amount: 0.5,
        ..ModRoute::default()
    };
    let mut shaped = triangle.clone();
    shaped.lfo_shape = std::sync::Arc::new(LfoShape::triangle());
    shaped.lfo_shape_type = LfoShapeType::Curve;
    triangle.legacy_lfo_waveform = Some(LegacyLfoWaveform::Triangle);
    triangle.normalize_legacy_lfo();
    assert_eq!(render(triangle), render(shaped));
}

#[test]
fn modulation_law_measurements() {
    let shape = LfoShape::triangle();
    let samples: Vec<(f64, f64)> = [0.0, 0.125, 0.25, 0.5, 0.75]
        .into_iter()
        .map(|phase| (phase, curve_shape_value(&shape, phase)))
        .collect();
    println!("default triangle samples: {samples:?}");
    let up = 1.0 - (-1.0 / (0.01f64 * 48_000.0)).exp();
    let mut state = 0.0;
    let mut at_10 = 0.0;
    let mut at_50 = 0.0;
    for index in 0..48_000 {
        state = slew_step(state, 1.0, up, up);
        if index + 1 == 480 {
            at_10 = state;
        }
        if index + 1 == 2_400 {
            at_50 = state;
        }
    }
    println!("slew tau=10ms: 10ms={at_10:.6} 50ms={at_50:.6} 1s={state:.9} coeff={up:.9}");
    println!(
        "attack floor: {:.6} s = {:.0} samples at 48 kHz, first level {:.6}",
        MIN_ENVELOPE_ATTACK_SECONDS,
        MIN_ENVELOPE_ATTACK_SECONDS * 48_000.0,
        1.0 / (MIN_ENVELOPE_ATTACK_SECONDS * 48_000.0)
    );
    println!(
        "route curve at 0.5: soft={:.6} hard={:.6}, at -0.5 hard={:.6}",
        shape_mod_value(0.5, -1.0),
        shape_mod_value(0.5, 1.0),
        shape_mod_value(-0.5, 1.0)
    );
    println!(
        "fade delay=240 rise=480 samples: 0s={:.6} 240={:.6} 241={:.6} 719={:.6} 720={:.6}",
        lfo_fade(0.0, 0.005, 0.01),
        lfo_fade(0.005, 0.005, 0.01),
        lfo_fade(241.0 / 48_000.0, 0.005, 0.01),
        lfo_fade(719.0 / 48_000.0, 0.005, 0.01),
        lfo_fade(720.0 / 48_000.0, 0.005, 0.01)
    );
    let coefficient = smoothing_coefficient(0.1, MAX_LFO_SMOOTH_SECONDS, 48_000.0);
    let mut smooth = 0.5;
    for _ in 0..64 {
        smooth += (1.0 - smooth) * coefficient;
    }
    println!("smooth parameter 0.1 after 64 samples: {smooth:.9}");
    assert!(at_10 < at_50 && at_50 < state);
}

fn routed_pitch_patch(source: ModSource) -> SynthPatch {
    SynthPatch {
        filter_wet: 0.0,
        oscillator_a_level: 0.5,
        mod_routes: vec![ModRoute {
            source,
            destination: ModDestination::Pitch,
            amount: 1.0,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn voice_frequency(synth: &OpenSynth, channel: u8) -> f64 {
    synth
        .voices
        .iter()
        .find(|voice| voice.channel == channel)
        .map(|voice| voice.frequency_hz)
        .expect("voice on channel")
}

#[test]
fn mpe_bend_range_scales_channel_bend() {
    let patch = SynthPatch {
        filter_wet: 0.0,
        mpe: Some(MpeConfig {
            lower: Some(MpeMemberRange::new(2, 16)),
            upper: None,
            bend_range: Some(48.0),
        }),
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 69, 100);
    synth.next_stereo(48_000);
    let base = voice_frequency(&synth, 1);
    assert!((base - 440.0).abs() < 1e-9);
    let bend = 8191.0 / 8192.0 * 48.0;
    synth.set_pitch_bend(1, 8191);
    synth.next_stereo(48_000);
    let bent = voice_frequency(&synth, 1);
    assert!((bent - 440.0 * 2f64.powf(bend / 12.0)).abs() < 1e-6);
}

#[test]
fn legacy_bend_keeps_the_two_semitone_law() {
    let patch = SynthPatch {
        filter_wet: 0.0,
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 69, 100);
    synth.next_stereo(48_000);
    synth.set_pitch_bend(0, 4096);
    synth.next_stereo(48_000);
    let expected = 440.0 * 2f64.powf(4096.0 / 8192.0 * 2.0 / 12.0);
    assert!((voice_frequency(&synth, 0) - expected).abs() < 1e-6);
}

#[test]
fn mpe_member_bends_do_not_cross_voices() {
    let patch = SynthPatch {
        filter_wet: 0.0,
        mpe: Some(MpeConfig {
            lower: Some(MpeMemberRange::new(2, 16)),
            upper: None,
            bend_range: Some(48.0),
        }),
        ..Default::default()
    };
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    synth.note_on(0, 64, 100);
    synth.next_stereo(48_000);
    let second_before = voice_frequency(&synth, 2);
    synth.set_pitch_bend(1, 8191);
    synth.next_stereo(48_000);
    let first = voice_frequency(&synth, 1);
    let second = voice_frequency(&synth, 2);
    assert!(
        (first - 440.0 * 2f64.powf((60.0 - 69.0 + 8191.0 / 8192.0 * 48.0) / 12.0)).abs() < 1e-6
    );
    assert!((second - second_before).abs() < 1e-9);
}

#[test]
fn pressure_and_timbre_reach_pitch_routes() {
    for (source, set) in [(ModSource::Aftertouch, 0u8), (ModSource::Timbre, 1u8)] {
        let mut synth = OpenSynth::new(routed_pitch_patch(source));
        synth.note_on(0, 69, 100);
        synth.next_stereo(48_000);
        let base = voice_frequency(&synth, 0);
        assert!((base - 440.0).abs() < 1e-9, "source={source:?} base={base}");
        if set == 0 {
            synth.set_channel_pressure(0, 127);
        } else {
            synth.set_timbre(0, 127);
        }
        synth.next_stereo(48_000);
        assert!((voice_frequency(&synth, 0) - 880.0).abs() < 1e-6);
    }
}

#[path = "reset_tests.rs"]
mod reset_tests;

#[test]
fn tuning_schema_is_the_default_clamp_and_automation_authority() {
    let original = SynthPatch::default();
    for (key, domain, default) in [
        (
            "tuning.reference_hz",
            TUNING_REFERENCE_HZ,
            original.tuning_reference_hz,
        ),
        (
            "tuning.transpose",
            TUNING_TRANSPOSE,
            original.tuning_transpose,
        ),
        ("tuning.fine", TUNING_FINE, original.tuning_fine_cents),
    ] {
        assert_eq!(oc_device_schema::synth::numeric_domain(key), Some(domain));
        assert_eq!(default, domain.default);
        for position in [-1.0_f64, 0.0, 0.125, 0.5, 0.875, 1.0, 2.0] {
            let selected = position.clamp(0.0, 1.0);
            let legacy = match key {
                "tuning.reference_hz" => 400.0 + selected * 80.0,
                "tuning.transpose" => selected * 48.0 - 24.0,
                _ => selected * 200.0 - 100.0,
            };
            assert_eq!(normalized_param_to_physical(key, position), legacy);
            let mut physical = SynthPatch::default();
            physical.set_param(key, legacy);
            let mut normalized = SynthPatch::default();
            normalized.set_normalized_param(key, position);
            assert_eq!(physical.tuning_reference_hz, normalized.tuning_reference_hz);
            assert_eq!(physical.tuning_transpose, normalized.tuning_transpose);
            assert_eq!(physical.tuning_fine_cents, normalized.tuning_fine_cents);
            assert_eq!(physical_param_to_normalized(key, legacy), Some(selected));
        }
        for input in [f64::MIN, domain.min, domain.default, domain.max, f64::MAX] {
            let mut patch = original.clone();
            patch.set_param(key, input);
            let applied = match key {
                "tuning.reference_hz" => patch.tuning_reference_hz,
                "tuning.transpose" => patch.tuning_transpose,
                _ => patch.tuning_fine_cents,
            };
            assert_eq!(applied, domain.clamp(input).unwrap());
        }
        for input in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut patch = original.clone();
            patch.set_param(key, input);
            patch.set_normalized_param(key, input);
            assert_eq!(patch.tuning_reference_hz, original.tuning_reference_hz);
            assert_eq!(patch.tuning_transpose, original.tuning_transpose);
            assert_eq!(patch.tuning_fine_cents, original.tuning_fine_cents);
            assert_eq!(physical_param_to_normalized(key, input), None);
        }
    }
}

#[test]
fn tuning_schema_automation_preserves_exact_audio_against_physical_law() {
    for position in [0.0, 0.125, 0.5, 0.875, 1.0] {
        let expected = SynthPatch {
            tuning_reference_hz: 400.0 + position * 80.0,
            tuning_transpose: position * 48.0 - 24.0,
            tuning_fine_cents: position * 200.0 - 100.0,
            ..SynthPatch::default()
        };
        let mut actual = SynthPatch::default();
        for key in ["tuning.reference_hz", "tuning.transpose", "tuning.fine"] {
            actual.set_normalized_param(key, position);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 69, 100);
        actual.note_on(0, 69, 100);
        for frame in 0..512 {
            assert_eq!(
                actual.next_stereo(48_000),
                expected.next_stereo(48_000),
                "position {position}, frame {frame}"
            );
        }
    }
}

#[test]
fn tuning_schema_control_updates_perform_no_heap_operations() {
    let mut patch = SynthPatch::default();
    let operations = crate::test_alloc::operations(|| {
        for key in ["tuning.reference_hz", "tuning.transpose", "tuning.fine"] {
            patch.set_normalized_param(key, 0.375);
            let physical = normalized_param_to_physical(key, 0.75);
            patch.set_param(key, physical);
            assert_eq!(physical_param_to_normalized(key, physical), Some(0.75));
        }
    });
    assert_eq!(operations, 0);
}

#[test]
fn discrete_schema_controls_match_physical_writes_for_every_recognized_key() {
    let mut count = 0;
    for key in oc_device_schema::synth::parameter_keys() {
        let Some(domain) = discrete_automation_domain(&key) else {
            continue;
        };
        count += 1;
        for position in [0.0, 0.5, 1.0] {
            let physical = domain.denormalize_linear(position).unwrap();
            assert_eq!(
                normalized_param_to_physical(&key, position),
                physical,
                "{key}"
            );
            assert_eq!(
                physical_param_to_normalized(&key, physical),
                Some(position),
                "{key}"
            );
            let mut expected = SynthPatch::default();
            let mut actual = expected.clone();
            assert!(expected.set_param(&key, physical), "{key}");
            actual.set_normalized_param(&key, position);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{key} @ {position}"
            );
        }
        assert_eq!(physical_param_to_normalized(&key, f64::NAN), None, "{key}");
    }
    assert!(
        count > 300,
        "indexed discrete families must be covered: {count}"
    );
    // Do not assign new automation laws to controls that historically used the
    // unit-fraction fallback (even when a wider physical catalog range exists).
    assert!(discrete_automation_domain("lfo.shape.points").is_none());
    assert_eq!(normalized_param_to_physical("lfo.shape.points", 0.5), 0.5);
    assert_eq!(physical_param_to_normalized("lfo.shape.points", 3.0), None);
    assert_eq!(physical_param_to_normalized("filter.routing", 1.5), None);
}

#[test]
fn discrete_schema_defaults_keep_factory_patch_and_optional_legacy_state() {
    let patch = SynthPatch::default();
    assert_eq!(patch.oscillator_a_waveform, Waveform::Saw);
    assert_eq!(patch.oscillator_b_waveform, Waveform::Square);
    assert_eq!(patch.oscillator_c_waveform, Waveform::Sine);
    assert_eq!(patch.filter_model, FilterModel::Svf);
    assert_eq!(patch.filter2_model, FilterModel::Svf);
    assert_eq!(patch.unison_voices, 2);
    assert_eq!(patch.polyphony, 16);
    assert!(patch.legato && patch.step_sync);
    assert!(patch.legacy_lfo_waveform.is_none() && patch.lfo_sync_div.is_none());
    assert!(
        patch
            .extra_lfos
            .iter()
            .all(|lfo| lfo.legacy_waveform.is_none() && lfo.sync_div.is_none())
    );
    for (key, physical) in [
        ("unison.voices", -3.0),
        ("voice.polyphony", 99.0),
        ("lfo.division", 99.0),
        ("quality.oversampling", -3.0),
    ] {
        let mut actual = patch.clone();
        actual.set_param(key, physical);
        let domain = oc_device_schema::synth::numeric_domain(key).unwrap();
        let mut expected = patch.clone();
        expected.set_param(key, domain.clamp(physical.round()).unwrap());
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{key}"
        );
    }
    // Invalid enum ids keep their existing fallback instead of becoming a
    // clamp-to-last-enum behavior.
    let mut invalid = patch.clone();
    invalid.set_param("filter.mode", 99.0);
    assert_eq!(invalid.filter_mode, FilterMode::LowPass);
    invalid.set_param("osc.a.waveform", 99.0);
    assert_eq!(invalid.oscillator_a_waveform, Waveform::Saw);
}

#[test]
fn discrete_schema_mapping_and_fixed_patch_updates_need_no_heap() {
    let keys = oc_device_schema::synth::parameter_keys();
    let mut patch = SynthPatch::default();
    let operations = crate::test_alloc::operations(|| {
        for key in &keys {
            if discrete_automation_domain(key).is_some() {
                let physical = normalized_param_to_physical(key, 0.75);
                assert_eq!(
                    physical_param_to_normalized(key, physical),
                    Some(0.75),
                    "{key}"
                );
            }
        }
        for key in [
            "unison.voices",
            "osc.b.unison",
            "voice.polyphony",
            "quality.oversampling",
            "lfo.division",
        ] {
            patch.set_normalized_param(key, 0.5);
        }
    });
    assert_eq!(operations, 0);
}

#[test]
fn discrete_schema_keeps_exact_rendered_audio_for_existing_numeric_laws() {
    for position in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let mut expected = SynthPatch::default();
        expected.set_param("osc.a.waveform", position * 6.0);
        expected.set_param("filter.model", position * 11.0);
        expected.set_param("unison.voices", 1.0 + position * 15.0);
        expected.set_param("quality.oversampling", position * 2.0);
        let mut actual = SynthPatch::default();
        for key in [
            "osc.a.waveform",
            "filter.model",
            "unison.voices",
            "quality.oversampling",
        ] {
            actual.set_normalized_param(key, position);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        for frame in 0..256 {
            assert_eq!(
                actual.next_stereo(48_000),
                expected.next_stereo(48_000),
                "{position}, frame {frame}"
            );
        }
    }
}

#[test]
fn power_time_schema_owns_actual_defaults_clamps_and_both_mapping_directions() {
    let mut count = 0;
    for key in oc_device_schema::synth::parameter_keys() {
        let Some(mapping) = oc_device_schema::synth::dsp_normalization(&key) else {
            continue;
        };
        if !matches!(
            mapping.law,
            oc_device_schema::synth::SynthDspLaw::Square
                | oc_device_schema::synth::SynthDspLaw::Cubic
        ) {
            continue;
        }
        count += 1;
        for position in [0.0_f64, 0.125, 0.5, 1.0] {
            let physical = match mapping.law {
                oc_device_schema::synth::SynthDspLaw::Square => {
                    position.powi(2) * mapping.domain.max
                }
                oc_device_schema::synth::SynthDspLaw::Cubic => {
                    position.powi(3) * mapping.domain.max
                }
                _ => unreachable!(),
            };
            assert_eq!(
                normalized_param_to_physical(&key, position),
                physical,
                "{key}"
            );
            let round = physical_param_to_normalized(&key, physical).unwrap();
            assert!((round - position).abs() < 1e-12, "{key}");
            let mut actual = SynthPatch::default();
            let mut expected = actual.clone();
            actual.set_normalized_param(&key, position);
            expected.set_param(&key, physical);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{key} @ {position}"
            );
        }
        assert_eq!(normalized_param_to_physical(&key, 0.0), 0.0, "{key}");
        let mut negative = SynthPatch::default();
        negative.set_param(&key, -1.0);
        let mut zero = SynthPatch::default();
        zero.set_param(&key, 0.0);
        assert_eq!(
            serde_json::to_value(negative).unwrap(),
            serde_json::to_value(zero).unwrap(),
            "physical clamp {key}"
        );
        assert_eq!(
            physical_param_to_normalized(&key, -1.0),
            match mapping.law {
                oc_device_schema::synth::SynthDspLaw::Square => None,
                _ => Some(0.0),
            },
            "inverse {key}"
        );
        assert_eq!(physical_param_to_normalized(&key, f64::NAN), None, "{key}");
    }
    assert!(count >= 161);
    let patch = SynthPatch::default();
    assert_eq!(patch.attack, 0.008);
    assert_eq!(patch.decay, 0.16);
    assert_eq!(patch.release, 0.28);
    assert_eq!(patch.filter_attack, 0.01);
    assert_eq!(patch.filter_decay, 0.2);
    assert_eq!(patch.filter_release, 0.25);
    assert_eq!(patch.glide_time, 0.08);
    assert_eq!(patch.lfo_rise, 0.0);
    assert_eq!(ModRoute::default().slew_up, 0.0);
}

#[test]
fn power_time_schema_preserves_exact_audio_for_historical_cubic_and_square_laws() {
    for position in [0.0_f64, 0.125, 0.5, 1.0] {
        let expected = SynthPatch {
            attack: position.powi(3) * 10.0,
            decay: position.powi(3) * 10.0,
            release: position.powi(3) * 20.0,
            lfo_rise: position.powi(2) * 5.0,
            lfo_delay: position.powi(2) * 5.0,
            ..SynthPatch::default()
        };
        let mut actual = SynthPatch::default();
        for key in [
            "amp.attack",
            "amp.decay",
            "amp.release",
            "lfo.rise",
            "lfo.delay",
        ] {
            actual.set_normalized_param(key, position);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        for frame in 0..512 {
            if frame == 128 {
                actual.note_off(0, 60);
                expected.note_off(0, 60);
            }
            assert_eq!(
                actual.next_stereo(48_000),
                expected.next_stereo(48_000),
                "{position}, frame {frame}"
            );
        }
    }
}

#[test]
fn power_time_schema_mappings_and_prepared_updates_allocate_nothing() {
    let keys = oc_device_schema::synth::parameter_keys();
    let mut patch = SynthPatch::default();
    patch.set_param("mod.64.slew_up", 0.0);
    let operations = crate::test_alloc::operations(|| {
        for key in &keys {
            if let Some(mapping) = oc_device_schema::synth::dsp_normalization(key) {
                if !matches!(
                    mapping.law,
                    oc_device_schema::synth::SynthDspLaw::Square
                        | oc_device_schema::synth::SynthDspLaw::Cubic
                ) {
                    continue;
                }
                patch.set_normalized_param(key, 0.5);
                let physical = normalized_param_to_physical(key, 0.5);
                assert_eq!(physical_param_to_normalized(key, physical), Some(0.5));
                patch.set_param(key, mapping.domain.default);
            }
        }
    });
    assert_eq!(operations, 0);
}

#[test]
fn log_rate_schema_preserves_exact_existing_laws_and_actual_physical_writes() {
    let mut count = 0;
    for key in oc_device_schema::synth::parameter_keys() {
        let Some(mapping) = oc_device_schema::synth::dsp_normalization(&key) else {
            continue;
        };
        if !matches!(
            mapping.law,
            oc_device_schema::synth::SynthDspLaw::Log10 | oc_device_schema::synth::SynthDspLaw::Log
        ) {
            continue;
        }
        count += 1;
        for value in [-1.0_f64, 0.0, 0.125, 0.5, 0.875, 1.0, 2.0] {
            let normalized = value.clamp(0.0, 1.0);
            let expected = if key == "step.rate" {
                0.01 * 3000f64.powf(normalized)
            } else if key.contains("cutoff") {
                20.0 * 1000f64.powf(normalized)
            } else {
                0.01 * 10_000f64.powf(normalized)
            };
            assert_eq!(
                normalized_param_to_physical(&key, value),
                expected,
                "forward {key} @ {value}"
            );
            let inverse = if key == "step.rate" {
                (expected / 0.01).ln() / 3000f64.ln()
            } else if key.contains("cutoff") {
                (expected / 20.0).log10() / 3.0
            } else {
                (expected / 0.01).log10() / 4.0
            };
            assert_eq!(
                physical_param_to_normalized(&key, expected),
                Some(inverse.clamp(0.0, 1.0)),
                "inverse {key}"
            );
            let mut physical = SynthPatch::default();
            let mut actual = physical.clone();
            assert!(physical.set_param(&key, expected));
            actual.set_normalized_param(&key, value);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(physical).unwrap(),
                "actual write {key}"
            );
        }
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                physical_param_to_normalized(&key, invalid),
                None,
                "invalid inverse {key}"
            );
        }
    }
    assert!(count >= 13, "both filters and ten LFOs plus step: {count}");
    let mut patch = SynthPatch::default();
    assert_eq!(patch.filter_cutoff, 12_000.0);
    assert_eq!(patch.filter2_cutoff, 12_000.0);
    assert_eq!(patch.lfo_rate, 5.2);
    assert_eq!(patch.step_rate, 4.0);
    patch.set_param("lfo.rate", 0.0);
    assert_eq!(
        patch.lfo_rate, 0.0,
        "zero remains a valid free-running physical rate"
    );
    assert_eq!(free_lfo_rate(patch.lfo_rate, true), 0.0);
    patch.set_normalized_param("lfo.rate", 0.0);
    assert_eq!(
        patch.lfo_rate, 0.01,
        "normalized zero truthfully selects the positive floor"
    );
    assert_eq!(free_lfo_rate(100.0, true), 1000.0);
    patch.set_param("step.rate", 0.0);
    assert_eq!(patch.step_rate, 0.01);
}

#[test]
fn log_rate_schema_preserves_exact_audio_and_low_sample_rate_runtime_bounds() {
    for rate in [8_000, 48_000] {
        for value in [0.0_f64, 0.125, 0.5, 1.0] {
            let filter_cutoff = 20.0 * 1000f64.powf(value);
            let expected = SynthPatch {
                filter_cutoff,
                filter2_cutoff: filter_cutoff,
                filter2_enabled: true,
                lfo_rate: 0.01 * 10_000f64.powf(value),
                step_rate: 0.01 * 3000f64.powf(value),
                ..SynthPatch::default()
            };
            let mut actual = SynthPatch {
                filter2_enabled: true,
                ..SynthPatch::default()
            };
            for key in ["filter.cutoff", "filter2.cutoff", "lfo.rate", "step.rate"] {
                actual.set_normalized_param(key, value);
            }
            let mut expected = OpenSynth::new(expected);
            let mut actual = OpenSynth::new(actual);
            expected.note_on(0, 60, 100);
            actual.note_on(0, 60, 100);
            for frame in 0..256 {
                assert_eq!(
                    actual.next_stereo(rate),
                    expected.next_stereo(rate),
                    "rate {rate} @ {value}, frame {frame}"
                );
            }
        }
    }
}

#[test]
fn log_rate_schema_updates_and_mappings_allocate_nothing() {
    let mut patch = SynthPatch::default();
    let operations = crate::test_alloc::operations(|| {
        for key in [
            "filter.cutoff",
            "cutoff",
            "filter2.cutoff",
            "lfo.rate",
            "lfo2.rate",
            "lfo10.rate",
            "step.rate",
        ] {
            patch.set_normalized_param(key, 0.5);
            let physical = normalized_param_to_physical(key, 0.5);
            assert!(physical_param_to_normalized(key, physical).is_some());
            patch.set_param(key, 0.0);
        }
    });
    assert_eq!(operations, 0);
}

const OSCILLATOR_AFFINE_KEYS: [&str; 13] = [
    "osc.a.pan",
    "osc.b.pan",
    "osc.c.pan",
    "osc.a.pulse_width",
    "osc.b.pulse_width",
    "osc.c.pulse_width",
    "osc.b.semitones",
    "osc.c.semitones",
    "unison.detune",
    "detune",
    "osc.a.detune",
    "osc.b.detune",
    "osc.c.detune",
];

fn previous_oscillator_affine_value(key: &str, value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if key.ends_with(".pan") {
        value * 2.0 - 1.0
    } else if key.ends_with(".pulse_width") {
        0.05 + value * 0.9
    } else if key.ends_with(".semitones") {
        value * 48.0 - 24.0
    } else {
        value * 100.0
    }
}

#[test]
fn oscillator_affine_schema_preserves_all_keys_setters_and_exact_mapping_arithmetic() {
    for key in OSCILLATOR_AFFINE_KEYS {
        let owner = oc_device_schema::synth::dsp_normalization(key).unwrap();
        assert_eq!(owner.law, oc_device_schema::synth::SynthDspLaw::Linear);
        for normalized in [-1.0_f64, 0.0, 0.1, 0.125, 0.5, 0.875, 1.0, 2.0] {
            let expected = previous_oscillator_affine_value(key, normalized);
            assert_eq!(
                normalized_param_to_physical(key, normalized).to_bits(),
                expected.to_bits(),
                "{key} @ {normalized}"
            );
            let inverse = if key.ends_with(".pan") {
                (expected + 1.0) / 2.0
            } else if key.ends_with(".pulse_width") {
                (expected - 0.05) / 0.9
            } else if key.ends_with(".semitones") {
                (expected + 24.0) / 48.0
            } else {
                expected / 100.0
            };
            assert_eq!(
                physical_param_to_normalized(key, expected)
                    .unwrap()
                    .to_bits(),
                inverse.clamp(0.0, 1.0).to_bits(),
                "inverse {key}"
            );
            let mut actual = SynthPatch::default();
            let mut physical = actual.clone();
            actual.set_normalized_param(key, normalized);
            assert!(physical.set_param(key, expected));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(physical).unwrap(),
                "{key}"
            );
        }
        for physical in [
            f64::MIN,
            owner.domain.min,
            owner.domain.default,
            owner.domain.max,
            f64::MAX,
        ] {
            let mut actual = SynthPatch::default();
            let mut bounded = actual.clone();
            actual.set_param(key, physical);
            bounded.set_param(key, physical.clamp(owner.domain.min, owner.domain.max));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(bounded).unwrap(),
                "bound {key}"
            );
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut actual = SynthPatch::default();
            let original = actual.clone();
            actual.set_param(key, invalid);
            actual.set_normalized_param(key, invalid);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(original).unwrap(),
                "invalid {key}"
            );
            assert_eq!(physical_param_to_normalized(key, invalid), None);
        }
    }
    let patch = SynthPatch::default();
    assert_eq!(patch.oscillator_a_pulse_width, 0.5);
    assert_eq!(patch.oscillator_b_pulse_width, 0.5);
    assert_eq!(patch.oscillator_c_pulse_width, 0.5);
    assert_eq!(patch.oscillator_a_pan, 0.0);
    assert_eq!(patch.detune_cents, 7.0);
    assert_eq!(patch.oscillator_b_detune_cents, 0.0);
    assert_eq!(patch.oscillator_c_detune_cents, 0.0);
    assert_eq!(normalized_param_to_physical("osc.a.semitones", 0.5), 0.5);
    assert_eq!(normalized_param_to_physical("sub.pan", 0.5), 0.5);
}

#[test]
fn oscillator_affine_schema_preserves_exact_pulse_pan_detune_audio() {
    for value in [0.0_f64, 0.125, 0.5, 0.875, 1.0] {
        let mut expected = SynthPatch {
            oscillator_a_waveform: Waveform::Square,
            oscillator_b_level: 0.2,
            oscillator_c_level: 0.2,
            ..Default::default()
        };
        let mut actual = expected.clone();
        for key in OSCILLATOR_AFFINE_KEYS {
            expected.set_param(key, previous_oscillator_affine_value(key, value));
            actual.set_normalized_param(key, value);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        for frame in 0..512 {
            assert_eq!(
                actual.next_stereo(48_000),
                expected.next_stereo(48_000),
                "{value}, frame {frame}"
            );
        }
    }
}

#[test]
fn oscillator_affine_schema_updates_and_mappings_need_no_heap() {
    let mut patch = SynthPatch::default();
    let operations = crate::test_alloc::operations(|| {
        for key in OSCILLATOR_AFFINE_KEYS {
            patch.set_normalized_param(key, 0.5);
            let physical = normalized_param_to_physical(key, 0.75);
            assert!(physical_param_to_normalized(key, physical).is_some());
            patch.set_param(key, physical);
        }
    });
    assert_eq!(operations, 0);
}

fn lfo_step_affine_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for number in 1..=10 {
        let prefix = if number == 1 {
            "lfo".to_string()
        } else {
            format!("lfo{number}")
        };
        keys.extend(["phase", "bend", "smooth"].map(|field| format!("{prefix}.{field}")));
    }
    keys.extend((1..=16).map(|slot| format!("step.{slot}")));
    keys.push("step.smoothing".into());
    keys
}

fn previous_lfo_step_affine_value(key: &str, normalized: f64) -> f64 {
    let value = normalized.clamp(0.0, 1.0);
    if key.ends_with(".phase") {
        value * 360.0
    } else if key.ends_with("smooth") || key == "step.smoothing" {
        value
    } else {
        value * 2.0 - 1.0
    }
}

#[test]
fn lfo_step_affine_schema_preserves_all_47_keys_exact_maps_and_actual_writes() {
    let keys = lfo_step_affine_keys();
    assert_eq!(keys.len(), 47);
    for key in &keys {
        let owner = oc_device_schema::synth::dsp_normalization(key).unwrap();
        for value in [-1.0_f64, -0.0, 0.0, 0.1, 0.5, 0.875, 1.0, 2.0] {
            let expected = previous_lfo_step_affine_value(key, value);
            assert_eq!(
                normalized_param_to_physical(key, value).to_bits(),
                expected.to_bits(),
                "{key} @ {value}"
            );
            let inverse = if key.ends_with(".phase") {
                expected / 360.0
            } else if key.ends_with("smooth") || key == "step.smoothing" {
                expected
            } else {
                (expected + 1.0) / 2.0
            };
            assert_eq!(
                physical_param_to_normalized(key, expected)
                    .unwrap()
                    .to_bits(),
                inverse.clamp(0.0, 1.0).to_bits(),
                "inverse {key}"
            );
            let mut actual = SynthPatch::default();
            let mut physical = actual.clone();
            actual.set_normalized_param(key, value);
            assert!(physical.set_param(key, expected));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(physical).unwrap(),
                "write {key}"
            );
        }
        for value in [
            f64::MIN,
            owner.domain.min,
            owner.domain.default,
            owner.domain.max,
            f64::MAX,
        ] {
            let mut actual = SynthPatch::default();
            let mut bounded = actual.clone();
            actual.set_param(key, value);
            bounded.set_param(key, value.clamp(owner.domain.min, owner.domain.max));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(bounded).unwrap(),
                "bound {key}"
            );
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut actual = SynthPatch::default();
            let expected = actual.clone();
            actual.set_param(key, value);
            actual.set_normalized_param(key, value);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "invalid {key}"
            );
            assert_eq!(physical_param_to_normalized(key, value), None);
        }
    }
    let mut serialized = serde_json::to_value(SynthPatch::default()).unwrap();
    for field in ["lfo_phase", "lfo_bend", "lfo_smooth"] {
        serialized.as_object_mut().unwrap().remove(field);
    }
    for lfo in serialized["extra_lfos"].as_array_mut().unwrap() {
        for field in ["phase", "bend", "smooth"] {
            lfo.as_object_mut().unwrap().remove(field);
        }
    }
    let restored: SynthPatch = serde_json::from_value(serialized).unwrap();
    assert_eq!(restored.lfo_phase, 0.0);
    assert_eq!(restored.lfo_bend, 0.0);
    assert_eq!(restored.lfo_smooth, 0.0);
    assert!(
        restored
            .extra_lfos
            .iter()
            .all(|lfo| lfo.phase == 0.0 && lfo.bend == 0.0 && lfo.smooth == 0.0)
    );
    assert_eq!(restored.step_smoothing, 0.0);
    assert_eq!(restored.step_values, [0.0; 16]);
    assert_eq!(normalized_param_to_physical("lfo.pitch", 0.5), 100.0);
    assert_eq!(normalized_param_to_physical("sub.pan", 0.5), 0.5);
}

#[test]
fn lfo_step_affine_schema_preserves_exact_modulated_audio() {
    let keys = lfo_step_affine_keys();
    for value in [0.0_f64, 0.25, 0.5, 0.75, 1.0] {
        let mut expected = SynthPatch {
            lfo_retrigger: true,
            ..SynthPatch::default()
        };
        expected.extra_lfos[0].retrigger = true;
        expected.step_sync = false;
        expected.step_rate = 30.0;
        for (index, source) in [ModSource::Lfo, ModSource::Lfo2, ModSource::Step]
            .into_iter()
            .enumerate()
        {
            expected.mod_routes[index] = ModRoute {
                source,
                destination: ModDestination::FilterCutoff,
                amount: 0.2,
                ..Default::default()
            };
        }
        let mut actual = expected.clone();
        for (index, key) in keys.iter().enumerate() {
            let position = if key.starts_with("step.") && index % 2 == 0 {
                1.0 - value
            } else {
                value
            };
            expected.set_param(key, previous_lfo_step_affine_value(key, position));
            actual.set_normalized_param(key, position);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        let mut sounded = false;
        for frame in 0..2048 {
            let pair = actual.next_stereo(48_000);
            sounded |= pair.iter().any(|sample| *sample != 0.0);
            assert_eq!(pair, expected.next_stereo(48_000), "{value}, frame {frame}");
        }
        assert!(sounded);
    }
}

#[test]
fn lfo_step_affine_schema_updates_and_mappings_allocate_nothing() {
    let keys = lfo_step_affine_keys();
    let mut patch = SynthPatch::default();
    let operations = crate::test_alloc::operations(|| {
        for key in &keys {
            patch.set_normalized_param(key, 0.5);
            let physical = normalized_param_to_physical(key, 0.75);
            assert_eq!(physical_param_to_normalized(key, physical), Some(0.75));
            patch.set_param(key, physical);
        }
    });
    assert_eq!(operations, 0);
}

fn signed_modulation_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "amp.curve",
        "filter_env.curve",
        "env3.curve",
        "env4.curve",
        "filter.env.amount",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for slot in 1..=64 {
        keys.push(format!("mod.{slot}.amount"));
        keys.push(format!("mod.{slot}.curve"));
        keys.push(format!("fxmod.{slot}.amount"));
    }
    keys
}

fn previous_signed_modulation_value(key: &str, normalized: f64) -> f64 {
    let value = normalized.clamp(0.0, 1.0);
    if key == "filter.env.amount" {
        value * 16.0 - 8.0
    } else {
        value * 2.0 - 1.0
    }
}

#[test]
fn signed_modulation_schema_preserves_all_197_exact_maps_setters_and_defaults() {
    let keys = signed_modulation_keys();
    assert_eq!(keys.len(), 197);
    for key in &keys {
        let owner = oc_device_schema::synth::dsp_normalization(key).unwrap();
        assert_eq!(owner.domain.default, 0.0, "{key}");
        for normalized in [-1.0_f64, -0.0, 0.0, 0.125, 0.5, 0.875, 1.0, 2.0] {
            let physical = previous_signed_modulation_value(key, normalized);
            assert_eq!(
                normalized_param_to_physical(key, normalized).to_bits(),
                physical.to_bits(),
                "forward {key}"
            );
            let inverse = if key == "filter.env.amount" {
                (physical + 8.0) / 16.0
            } else {
                (physical + 1.0) / 2.0
            };
            assert_eq!(
                physical_param_to_normalized(key, physical)
                    .unwrap()
                    .to_bits(),
                inverse.clamp(0.0, 1.0).to_bits(),
                "inverse {key}"
            );
            let mut expected = SynthPatch::default();
            let mut actual = expected.clone();
            expected.set_param(key, physical);
            actual.set_normalized_param(key, normalized);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "actual {key}"
            );
        }
        for physical in [
            f64::MIN,
            owner.domain.min,
            owner.domain.default,
            owner.domain.max,
            f64::MAX,
        ] {
            let mut actual = SynthPatch::default();
            let mut expected = actual.clone();
            actual.set_param(key, physical);
            expected.set_param(key, physical.clamp(owner.domain.min, owner.domain.max));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "bound {key}"
            );
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut actual = SynthPatch::default();
            let original = actual.clone();
            actual.set_param(key, invalid);
            actual.set_normalized_param(key, invalid);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(original).unwrap(),
                "invalid {key}"
            );
            assert_eq!(physical_param_to_normalized(key, invalid), None);
        }
    }
    let mut serialized = serde_json::to_value(SynthPatch::default()).unwrap();
    for field in ["env3_curve", "env4_curve"] {
        serialized.as_object_mut().unwrap().remove(field);
    }
    let restored: SynthPatch = serde_json::from_value(serialized).unwrap();
    assert_eq!(
        (
            restored.amp_curve,
            restored.filter_curve,
            restored.env3_curve,
            restored.env4_curve,
            restored.filter_envelope_amount
        ),
        (0.0, 0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(
        (
            ModRoute::default().amount,
            ModRoute::default().curve,
            FxModRoute::default().amount
        ),
        (0.0, 0.0, 0.0)
    );
    assert_eq!(
        serde_json::from_value::<ModRoute>(serde_json::to_value(ModRoute::default()).unwrap())
            .unwrap()
            .curve,
        0.0
    );
    assert_eq!(normalized_param_to_physical("lfo.pitch", 0.5), 100.0);
}

#[test]
fn signed_modulation_schema_preserves_exact_envelope_route_and_fx_audio() {
    for value in [0.0_f64, 0.125, 0.5, 0.875, 1.0] {
        let mut expected = SynthPatch::default();
        expected.mod_routes[0] = ModRoute {
            source: ModSource::Lfo,
            destination: ModDestination::FilterCutoff,
            amount: 0.25,
            ..Default::default()
        };
        expected.fx = vec![SynthFx::new(SynthFxType::Distortion)];
        expected.fx_mod_routes = vec![FxModRoute {
            source: ModSource::Macro2,
            fx_index: 0,
            parameter: FxParam::Wet,
            amount: 0.5,
            bipolar: false,
        }];
        expected.macros[1] = 0.75;
        let mut actual = expected.clone();
        for key in [
            "amp.curve",
            "filter_env.curve",
            "env3.curve",
            "env4.curve",
            "filter.env.amount",
            "mod.1.amount",
            "mod.1.curve",
            "fxmod.1.amount",
        ] {
            expected.set_param(key, previous_signed_modulation_value(key, value));
            actual.set_normalized_param(key, value);
        }
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 60, 100);
        actual.note_on(0, 60, 100);
        let mut sounded = false;
        for frame in 0..2048 {
            if frame == 1024 {
                expected.note_off(0, 60);
                actual.note_off(0, 60);
            }
            let pair = actual.next_stereo(48_000);
            sounded |= pair.iter().any(|sample| *sample != 0.0);
            assert_eq!(pair, expected.next_stereo(48_000), "{value} frame {frame}");
        }
        assert!(sounded);
    }
}

#[test]
fn signed_modulation_schema_prepared_updates_and_mappings_allocate_nothing() {
    let keys = signed_modulation_keys();
    let mut patch = SynthPatch::default();
    patch.set_param("mod.64.amount", 0.0);
    patch.set_param("fxmod.64.amount", 0.0);
    let operations = crate::test_alloc::operations(|| {
        for key in &keys {
            patch.set_normalized_param(key, 0.5);
            let physical = normalized_param_to_physical(key, 0.75);
            assert_eq!(physical_param_to_normalized(key, physical), Some(0.75));
            patch.set_param(key, physical);
        }
    });
    assert_eq!(operations, 0);
}

const AMPLITUDE_FRACTION_KEYS: [&str; 15] = [
    "osc.a.level",
    "osc.b.level",
    "osc.c.level",
    "ring.level",
    "sub.level",
    "noise.level",
    "output.gain",
    "gain",
    "amp.sustain",
    "sustain",
    "filter_env.sustain",
    "env3.sustain",
    "env4.sustain",
    "amp.damping",
    "velocity.to_amp",
];

fn amplitude_fraction_field<'a>(patch: &'a mut SynthPatch, key: &str) -> &'a mut f64 {
    match key {
        "osc.a.level" => &mut patch.oscillator_a_level,
        "osc.b.level" => &mut patch.oscillator_b_level,
        "osc.c.level" => &mut patch.oscillator_c_level,
        "ring.level" => &mut patch.ring_mod_level,
        "sub.level" => &mut patch.sub_level,
        "noise.level" => &mut patch.noise_level,
        "output.gain" | "gain" => &mut patch.gain,
        "amp.sustain" | "sustain" => &mut patch.sustain,
        "filter_env.sustain" => &mut patch.filter_sustain,
        "env3.sustain" => &mut patch.env3_sustain,
        "env4.sustain" => &mut patch.env4_sustain,
        "amp.damping" => &mut patch.amp_damping,
        "velocity.to_amp" => &mut patch.velocity_to_amp,
        _ => panic!("not an amplitude-fraction control: {key}"),
    }
}

#[test]
fn amplitude_fraction_schema_preserves_all_15_identity_bits_defaults_and_physical_writes() {
    for key in AMPLITUDE_FRACTION_KEYS {
        let owner = oc_device_schema::synth::dsp_normalization(key).unwrap();
        assert_eq!(owner.law, oc_device_schema::synth::SynthDspLaw::Identity);
        let expected_default = match key {
            "osc.a.level" => 0.78_f64,
            "output.gain" | "gain" => 0.22,
            "amp.sustain" | "sustain" | "env3.sustain" | "env4.sustain" => 0.72,
            "velocity.to_amp" => 1.0,
            _ => 0.0,
        };
        assert_eq!(
            owner.domain.default.to_bits(),
            expected_default.to_bits(),
            "factory law {key}"
        );
        let mut patch = SynthPatch::default();
        assert_eq!(
            amplitude_fraction_field(&mut patch, key).to_bits(),
            owner.domain.default.to_bits(),
            "default {key}"
        );
        for value in [
            -1.0_f64,
            -0.0,
            0.0,
            f64::MIN_POSITIVE,
            0.125,
            0.72,
            0.875,
            1.0,
            2.0,
        ] {
            let expected = value.clamp(0.0, 1.0);
            assert_eq!(
                normalized_param_to_physical(key, value).to_bits(),
                expected.to_bits(),
                "forward {key}"
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
                    "inverse {key}"
                );
            }
            patch.set_param(key, value);
            assert_eq!(
                amplitude_fraction_field(&mut patch, key).to_bits(),
                expected.to_bits(),
                "physical write {key}"
            );
            patch.set_normalized_param(key, value);
            assert_eq!(
                amplitude_fraction_field(&mut patch, key).to_bits(),
                expected.to_bits(),
                "normalized write {key}"
            );
        }
        for value in [f64::MIN, f64::MAX] {
            patch.set_param(key, value);
            assert_eq!(
                *amplitude_fraction_field(&mut patch, key),
                value.clamp(0.0, 1.0)
            );
            assert_eq!(physical_param_to_normalized(key, value), None);
        }
        patch.set_param(key, 0.375);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            patch.set_param(key, invalid);
            patch.set_normalized_param(key, invalid);
            assert_eq!(
                *amplitude_fraction_field(&mut patch, key),
                0.375,
                "invalid write {key}"
            );
            assert_eq!(
                normalized_param_to_physical(key, invalid).to_bits(),
                0.0_f64.to_bits()
            );
            assert_eq!(physical_param_to_normalized(key, invalid), None);
        }
    }
    let mut encoded = serde_json::to_value(SynthPatch::default()).unwrap();
    for field in [
        "oscillator_c_level",
        "env3_sustain",
        "env4_sustain",
        "amp_damping",
        "velocity_to_amp",
    ] {
        encoded.as_object_mut().unwrap().remove(field);
    }
    let mut decoded: SynthPatch = serde_json::from_value(encoded).unwrap();
    for key in AMPLITUDE_FRACTION_KEYS {
        assert_eq!(
            amplitude_fraction_field(&mut decoded, key).to_bits(),
            oc_device_schema::synth::dsp_normalization(key)
                .unwrap()
                .domain
                .default
                .to_bits(),
            "absent serde default {key}"
        );
    }
}

#[test]
fn amplitude_fraction_schema_preserves_exact_active_oscillator_envelope_and_velocity_audio() {
    for value in [0.0_f64, 0.125, 0.5, 0.875, 1.0] {
        let mut expected = SynthPatch {
            oscillator_random_phase: 0.0,
            oscillator_b_waveform: Waveform::Saw,
            oscillator_c_waveform: Waveform::Triangle,
            filter_envelope_amount: 1.0,
            ..SynthPatch::default()
        };
        expected.mod_routes[0] = ModRoute {
            source: ModSource::Env3,
            destination: ModDestination::FilterCutoff,
            amount: 0.25,
            ..Default::default()
        };
        let mut actual = expected.clone();
        for key in AMPLITUDE_FRACTION_KEYS {
            *amplitude_fraction_field(&mut expected, key) = value;
            actual.set_normalized_param(key, value);
        }
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let mut expected = OpenSynth::new(expected);
        let mut actual = OpenSynth::new(actual);
        expected.note_on(0, 64, 79);
        actual.note_on(0, 64, 79);
        let mut sounded = false;
        for frame in 0..4096 {
            if frame == 2048 {
                expected.note_off(0, 64);
                actual.note_off(0, 64);
            }
            let pair = actual.next_stereo(48_000);
            sounded |= pair.iter().any(|sample| *sample != 0.0);
            assert_eq!(
                pair.map(f32::to_bits),
                expected.next_stereo(48_000).map(f32::to_bits),
                "{value} frame {frame}"
            );
        }
        assert_eq!(sounded, value != 0.0);
    }
}

#[test]
fn amplitude_fraction_schema_prepared_updates_and_mappings_allocate_nothing() {
    let mut patch = SynthPatch::default();
    let mut synth = OpenSynth::new(patch.clone());
    synth.note_on(0, 60, 100);
    synth.next_stereo(48_000);
    let operations = crate::test_alloc::operations(|| {
        for key in AMPLITUDE_FRACTION_KEYS {
            for value in [-0.0_f64, 0.125, 0.5, 1.0] {
                patch.set_normalized_param(key, value);
                let physical = normalized_param_to_physical(key, value);
                assert_eq!(physical.to_bits(), value.to_bits());
                assert_eq!(
                    physical_param_to_normalized(key, physical)
                        .unwrap()
                        .to_bits(),
                    value.to_bits()
                );
                patch.set_param(key, physical);
                synth.set_normalized_param(key, value);
                synth.set_param(key, physical);
                for _ in 0..32 {
                    assert!(
                        synth
                            .next_stereo(48_000)
                            .iter()
                            .all(|sample| sample.is_finite())
                    );
                }
            }
        }
    });
    assert_eq!(operations, 0);
}
