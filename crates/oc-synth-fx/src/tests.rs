use super::*;

#[test]
fn compressor_reports_its_lookahead_latency() {
    let latency = lookahead_samples(48_000.0);
    assert_eq!(latency, 96);
    let fx = SynthFx::new(SynthFxType::Compressor);
    assert_eq!(
        fx_rack_latency(std::slice::from_ref(&fx), 48_000.0),
        latency
    );
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(std::slice::from_ref(&fx));
    assert_eq!(chain.latency_samples(), latency);

    let mut bypassed = fx;
    bypassed.set_param("limiter", 0.0);
    assert_eq!(fx_rack_latency(&[bypassed.clone()], 48_000.0), 0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[bypassed]);
    assert_eq!(chain.latency_samples(), 0);
}

#[test]
fn effect_labels_and_numeric_types_round_trip() {
    for fx_type in SynthFxType::ALL {
        assert_eq!(SynthFxType::from_label(fx_type.label()), Some(fx_type));
        assert_eq!(
            SynthFxType::from_index(fx_type.index() as f64),
            Some(fx_type)
        );
    }
    assert_eq!(SynthFxType::from_label("EQ"), Some(SynthFxType::Eq3));
    assert_eq!(
        SynthFxType::from_label("saturation"),
        Some(SynthFxType::Distortion)
    );
    assert_eq!(
        SynthFxType::from_label("flanger"),
        Some(SynthFxType::Flanger)
    );
    assert_eq!(SynthFxType::from_index(9.0), Some(SynthFxType::Filter));
    assert_eq!(SynthFxType::from_index(99.0), None);
}

#[test]
fn numeric_fx_abi_is_append_only() {
    let expected = [
        (SynthFxType::Drive, 0),
        (SynthFxType::Compressor, 1),
        (SynthFxType::Eq3, 2),
        (SynthFxType::Delay, 3),
        (SynthFxType::Reverb, 4),
        (SynthFxType::Chorus, 5),
        (SynthFxType::Distortion, 6),
        (SynthFxType::Phaser, 7),
        (SynthFxType::Flanger, 8),
        (SynthFxType::Filter, 9),
        (SynthFxType::Utils, 10),
        (SynthFxType::Split, 11),
        (SynthFxType::Split3, 12),
        (SynthFxType::SplitMs, 13),
        (SynthFxType::Bode, 14),
        (SynthFxType::ParallelRack, 15),
        (SynthFxType::HyperD, 16),
        (SynthFxType::Conv, 17),
        (SynthFxType::Multiband, 18),
    ];
    for (fx_type, index) in expected {
        assert_eq!(fx_type.index(), index);
        assert_eq!(SynthFxType::from_index(index as f64), Some(fx_type));
    }
    assert_eq!(SynthFxType::ALL.len(), expected.len());
}

#[test]
fn default_params_are_finite_and_bounded() {
    for fx_type in SynthFxType::ALL {
        let fx = SynthFx::new(fx_type);
        assert!(!fx.params.is_empty());
        assert!(fx.params.len() <= MAX_FX_VALUE_SLOTS);
        assert!(fx.params.iter().all(|(_, value)| value.is_finite()));
    }
}

#[test]
fn empty_chain_bypasses_without_arithmetic() {
    let mut chain = SynthFxChain::new();
    for sample in [0.0, 0.25, -1.5, 3.0] {
        assert_eq!(chain.process(sample, -sample, 48_000.0), [sample, -sample]);
    }
    assert!(!chain.is_active());
}

#[test]
fn fx_rack_latency_matches_the_chain() {
    let mut delay = SynthFx::new(SynthFxType::Delay);
    delay.set_param("mix", 0.25);
    let mut distortion = SynthFx::new(SynthFxType::Distortion);
    distortion.set_param("mix", 0.5);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[delay.clone(), distortion.clone()]);
    assert_eq!(
        chain.latency_samples(),
        delay_latency_samples(48_000.0) + DISTORTION_LATENCY
    );
    assert_eq!(
        fx_rack_latency(&[delay.clone(), distortion.clone()], 48_000.0),
        chain.latency_samples()
    );
    // A disabled module is not in the audio path, so it has no latency.
    delay.enabled = false;
    distortion.enabled = false;
    assert_eq!(fx_rack_latency(&[delay, distortion], 48_000.0), 0);
}

#[test]
fn delay_time_control_follows_the_curve_map() {
    // The stored control is not seconds: t = 2^(20x - 2).
    for (control, seconds) in [
        (0.0, 0.25),
        (0.05, 0.5),
        (0.1, 1.0),
        (0.15, 2.0),
        (0.2, 4.0),
        (0.25, 8.0),
        (0.3, 16.0),
        (0.35, 32.0),
    ] {
        let measured = delay_time_seconds(control);
        assert!(
            (measured - seconds).abs() < 1e-12,
            "control {control}: {measured} vs {seconds}"
        );
    }
    let mut previous = 0.0;
    for step in 0..=100 {
        let seconds = delay_time_seconds(step as f64 / 100.0);
        assert!(seconds > previous);
        previous = seconds;
    }
    // The line and the latency clamp the top of the curve.
    let maximum = (DELAY_LINE_SAMPLES - DELAY_INTERP_TAPS) as f64 - 3_000.0;
    assert!((delay_time_samples(1.0, 48_000.0) - maximum).abs() < 1e-9);
    assert!((delay_time_samples(0.0, 48_000.0) - 12_000.0).abs() < 1e-9);
}

#[test]
fn delay_fractional_read_tracks_pitch_within_one_cent() {
    // Read a 1 kHz tone while advancing 1.5 samples per output sample:
    // the interpolated output must be 1.5 kHz within one cent.
    let sr = 48_000.0;
    let mut line = vec![0.0; 1 << 17];
    for (index, sample) in line.iter_mut().enumerate() {
        *sample = (TAU * 1_000.0 * index as f64 / sr).sin();
    }
    let taps = delay_interpolation_taps();
    let mut output = Vec::with_capacity(40_000);
    for step in 0..40_000 {
        output.push(sinc_read(&line, 1_000.0 + 1.5 * step as f64, &taps));
    }
    let mut crossings = Vec::new();
    for pair in output.windows(2).enumerate() {
        let (index, pair) = pair;
        if pair[0] <= 0.0 && pair[1] > 0.0 {
            let fraction = -pair[0] / (pair[1] - pair[0]);
            crossings.push(index as f64 + fraction);
        }
    }
    assert!(crossings.len() > 1_000, "crossings {}", crossings.len());
    let periods = (crossings.len() - 1) as f64;
    let span = crossings[crossings.len() - 1] - crossings[0];
    let frequency = periods * sr / span;
    let cents = 1_200.0 * (frequency / 1_500.0).log2();
    assert!(
        cents.abs() < 1.0,
        "frequency {frequency} Hz ({cents:+.3} cents)"
    );
}

#[test]
fn delay_damping_filter_is_a_zdf_one_pole() {
    let sr = 48_000.0;
    for cutoff in [100.0, 250.0, 1_000.0, 5_000.0, 10_000.0] {
        let coefficient = damping_coefficient(cutoff, sr);
        let w = TAU * cutoff / sr;
        let pole = 1.0 - 2.0 * coefficient;
        let numerator = coefficient * (1.0 + w.cos()).hypot(w.sin());
        let denominator = (1.0 - pole * w.cos()).hypot(pole * w.sin());
        let gain = numerator / denominator;
        assert!(
            (20.0 * gain.log10() + 3.0103).abs() < 0.05,
            "cutoff {cutoff}: {:.3} dB",
            20.0 * gain.log10()
        );
    }
    // DC gain is exactly one: a step settles at 1.
    let coefficient = damping_coefficient(1_000.0, sr);
    let mut damping = ZdfDamping::default();
    let mut last = 0.0;
    for _ in 0..sr as usize {
        last = damping.process(1.0, coefficient);
    }
    assert!((last - 1.0).abs() < 1e-6, "step settles at {last}");
}

#[test]
fn delay_feedback_is_linear_in_the_control() {
    let steady = |feedback: f64| {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Delay);
        fx.set_param("time", 0.0); // 0.25 s
        fx.set_param("feedback", feedback);
        fx.set_param("tone", 1.0);
        fx.set_param("mix", 1.0);
        chain.sync_from_patch(&[fx]);
        let settle = 50 * (12_000 + 3_000);
        let mut last = 0.0;
        for _ in 0..settle {
            last = chain.process(0.1, 0.1, 48_000.0)[0];
        }
        last
    };
    for feedback in [0.2, 0.5, 0.8] {
        let measured = steady(feedback);
        let expected = 0.1 / (1.0 - feedback);
        assert!(
            (measured - expected).abs() < expected * 5e-3,
            "feedback {feedback}: {measured:.6} vs {expected:.6}"
        );
    }
}

#[test]
fn delay_reports_its_latency_and_delays_the_dry_path() {
    let latency = delay_latency_samples(48_000.0);
    assert_eq!(latency, 3_000);
    let run = |mix: f64| {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Delay);
        fx.set_param("time", 0.05); // 0.5 s
        fx.set_param("feedback", 0.0);
        fx.set_param("mix", mix);
        chain.sync_from_patch(&[fx]);
        assert_eq!(chain.latency_samples(), latency);
        let mut output = Vec::new();
        for index in 0..(latency + 24_000 + 8) {
            let input = if index == 0 { 1.0 } else { 0.0 };
            output.push(chain.process(input, input, 48_000.0)[0]);
        }
        output
    };
    let bypass = run(0.0);
    assert!(bypass[..latency].iter().all(|sample| *sample == 0.0));
    assert_eq!(bypass[latency], 1.0);
    let half = run(0.5);
    let dry_gain = (1.0 - 0.5f64.powi(3)).sqrt();
    let wet_gain = 0.5f64.sqrt();
    assert!((half[latency] - dry_gain).abs() < 1e-9);
    assert!(half[..latency].iter().all(|sample| *sample == 0.0));
    assert!((half[latency + 24_000] - wet_gain).abs() < 1e-9);
}

#[test]
fn delay_staging_keeps_the_dry_until_the_wet_arrives() {
    // The dry trim compensates a wet power that is not there yet, so an
    // impulse keeps all but the wet control's committed share.
    let latency = delay_latency_samples(48_000.0);
    let echo = (delay_time_seconds(0.05) * 48_000.0) as usize;
    for mix in [0.184, 0.403, 0.75, 1.0] {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Delay);
        fx.set_param("time", 0.05);
        fx.set_param("feedback", 0.0);
        fx.set_param("mix", mix);
        chain.sync_from_patch(&[fx]);
        let mut output = Vec::new();
        for index in 0..(latency + echo + 8) {
            let input = if index == 0 { 1.0 } else { 0.0 };
            output.push(chain.process(input, input, 48_000.0)[0]);
        }
        let dry = (1.0 - mix * mix * mix).sqrt();
        assert!((output[latency] - dry).abs() < 1e-9, "mix {mix} dry");
        assert!(
            (output[latency + echo] - mix.sqrt()).abs() < 1e-9,
            "mix {mix} wet"
        );
    }
}

#[test]
fn power_split_settles_to_the_constant_power_pair() {
    let mut split = PowerSplit::new(48_000.0);
    let mut share = 0.0;
    for _ in 0..48_000 {
        share = split.share([1.0, -1.0], [1.0, -1.0], 0.25);
    }
    let (dry, _) = wet_dry_power(0.25);
    assert!((1.0 - share - dry * dry).abs() < 1e-3, "settled dry");
}

#[test]
fn power_split_keeps_the_dry_before_the_wet_arrives() {
    let mut split = PowerSplit::new(48_000.0);
    let mut share = 0.0;
    for _ in 0..24 {
        share = split.share([1.0, -1.0], [0.0, 0.0], 0.25);
    }
    assert!(
        (1.0 - share - (1.0 - 0.25f64.powi(3))).abs() < 1e-3,
        "share {share}"
    );
}

#[test]
fn power_split_stays_primed_on_a_note_onset() {
    let mut split = PowerSplit::new(48_000.0);
    for _ in 0..48_000 {
        split.share([1.0, -1.0], [1.0, -1.0], 0.25);
    }
    let share = split.share([1.0, -1.0], [0.0, 0.0], 0.25);
    assert!((share - 0.25).abs() < 1e-3, "share {share}");
}

#[test]
fn power_split_rearms_after_digital_silence() {
    let mut split = PowerSplit::new(48_000.0);
    for _ in 0..48_000 {
        split.share([1.0, -1.0], [1.0, -1.0], 0.25);
    }
    for _ in 0..48_000 {
        split.share([0.0, 0.0], [0.0, 0.0], 0.25);
    }
    let share = split.share([1.0, -1.0], [0.0, 0.0], 0.25);
    assert!((share - 0.25f64.powi(3)).abs() < 1e-3, "share {share}");
}

#[test]
fn power_split_kills_the_dry_at_full_wet() {
    let mut split = PowerSplit::new(48_000.0);
    assert_eq!(split.share([1.0, 1.0], [0.0, 0.0], 1.0), 1.0);
}

#[test]
fn delay_tail_decays_to_silence() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Delay);
    fx.set_param("time", 0.05); // 0.5 s
    fx.set_param("feedback", 0.2);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    for index in 0..4_800 {
        chain.process(if index == 0 { 0.5 } else { 0.0 }, 0.0, 48_000.0);
    }
    assert!(chain.tail_active());
    for _ in 0..(48_000 * 5) {
        chain.process(0.0, 0.0, 48_000.0);
    }
    assert!(!chain.tail_active());
}

fn new_fx_types() -> [SynthFxType; 4] {
    [
        SynthFxType::Distortion,
        SynthFxType::Phaser,
        SynthFxType::Flanger,
        SynthFxType::Filter,
    ]
}

fn noise(seed: &mut u64) -> f64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*seed >> 32) as u32 as f64 / u32::MAX as f64) * 1.2 - 0.6
}

#[test]
fn new_effects_are_finite_and_stable_at_extremes() {
    for fx_type in new_fx_types() {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(fx_type);
        match fx_type {
            SynthFxType::Distortion => {
                fx.set_param("drive", 1.0);
                fx.set_param("level", 2.0);
                fx.set_param("mix", 1.0);
            }
            SynthFxType::Phaser => {
                fx.set_param("rate", 8.0);
                fx.set_param("depth", 1.0);
                fx.set_param("feedback", 0.85);
                fx.set_param("mix", 1.0);
            }
            SynthFxType::Flanger => {
                fx.set_param("rate", 8.0);
                fx.set_param("depth", 1.0);
                fx.set_param("feedback", 0.85);
                fx.set_param("mix", 1.0);
            }
            SynthFxType::Filter => {
                fx.set_param("cutoff", 20.0);
                fx.set_param("resonance", 0.95);
                fx.set_param("mode", 2.0);
                fx.set_param("mix", 1.0);
            }
            _ => unreachable!(),
        }
        chain.sync_from_patch(&[fx]);
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..96_000 {
            let input = noise(&mut seed);
            let [left, right] = chain.process(input, -input, 48_000.0);
            assert!(left.is_finite() && right.is_finite(), "{fx_type:?}");
            assert!(
                left.abs() < 20.0 && right.abs() < 20.0,
                "{fx_type:?} {left} {right}"
            );
        }
    }
}

#[test]
fn bypassed_new_effects_are_bit_identical() {
    let mut chain = SynthFxChain::new();
    let mut modules = Vec::new();
    for fx_type in new_fx_types() {
        let mut fx = SynthFx::new(fx_type);
        fx.enabled = false;
        modules.push(fx);
    }
    chain.sync_from_patch(&modules);
    for sample in [0.0, 0.1, -0.7, 1.3, -2.0] {
        assert_eq!(chain.process(sample, -sample, 48_000.0), [sample, -sample]);
    }
}

#[test]
fn disabled_new_effects_do_not_change_the_old_chain() {
    let old = [
        SynthFxType::Drive,
        SynthFxType::Compressor,
        SynthFxType::Eq3,
        SynthFxType::Delay,
        SynthFxType::Reverb,
        SynthFxType::Chorus,
    ];
    let mut old_chain = SynthFxChain::new();
    old_chain.sync_from_patch(&old.iter().copied().map(SynthFx::new).collect::<Vec<_>>());
    let with_disabled = old
        .iter()
        .copied()
        .map(SynthFx::new)
        .chain(new_fx_types().into_iter().map(|fx_type| {
            let mut fx = SynthFx::new(fx_type);
            fx.enabled = false;
            fx
        }))
        .collect::<Vec<_>>();
    let mut combined_chain = SynthFxChain::new();
    combined_chain.sync_from_patch(&with_disabled);
    let mut seed = 0x0f0f_0f0f_1234_5678u64;
    for _ in 0..4_800 {
        let input = noise(&mut seed) * 0.5;
        assert_eq!(
            old_chain.process(input, -input, 48_000.0),
            combined_chain.process(input, -input, 48_000.0)
        );
    }
}

#[test]
fn new_effect_params_round_trip() {
    for fx_type in new_fx_types() {
        let mut fx = SynthFx::new(fx_type);
        for (index, (name, _)) in fx_type.default_params().iter().enumerate() {
            fx.set_param(name, index as f64 + 0.25);
        }
        let json = serde_json::to_vec(&fx).expect("serialize fx");
        let restored: SynthFx = serde_json::from_slice(&json).expect("deserialize fx");
        assert_eq!(restored.fx_type, fx_type);
        assert_eq!(restored.params, fx.params);
    }
}

#[test]
fn flanger_tail_decays_to_silence() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Flanger);
    fx.set_param("rate", 0.5);
    fx.set_param("depth", 0.8);
    fx.set_param("feedback", 0.5);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    for index in 0..1_000 {
        chain.process(if index == 0 { 0.5 } else { 0.0 }, 0.0, 48_000.0);
    }
    assert!(chain.tail_active());
    for _ in 0..48_000 {
        chain.process(0.0, 0.0, 48_000.0);
    }
    assert!(!chain.tail_active());
}

#[test]
fn eq_band_types_match_the_measured_response() {
    let sr = 48_000.0;
    let bell = EqState::band_coefficients(0, 1_000.0, 12.0, EQ_TYPE_BELL as f64, 50.0, sr);
    assert!((bell.response_db(1_000.0, sr) - 12.0).abs() < 0.05);
    assert!((bell.response_db(500.0, sr) - 2.85).abs() < 0.15);
    assert!(bell.response_db(60.0, sr).abs() < 0.1);

    let low_cut = EqState::band_coefficients(0, 1_000.0, 0.0, EQ_TYPE_CUT as f64, 50.0, sr);
    assert!((low_cut.response_db(60.0, sr) + 49.2).abs() < 0.5);
    assert!((low_cut.response_db(1_000.0, sr) - 2.17).abs() < 0.15);
    let high_cut = EqState::band_coefficients(1, 1_000.0, 0.0, EQ_TYPE_CUT as f64, 50.0, sr);
    assert!((high_cut.response_db(4_000.0, sr) + 24.1).abs() < 0.5);

    let low_shelf = EqState::band_coefficients(0, 1_000.0, 12.0, EQ_TYPE_SHELF as f64, 50.0, sr);
    assert!((low_shelf.response_db(60.0, sr) - 12.0).abs() < 0.2);
    assert!((low_shelf.response_db(1_000.0, sr) - 6.0).abs() < 0.15);
    let high_shelf = EqState::band_coefficients(1, 1_000.0, 12.0, EQ_TYPE_SHELF as f64, 50.0, sr);
    assert!((high_shelf.response_db(8_000.0, sr) - 12.0).abs() < 0.2);
    assert!((high_shelf.response_db(1_000.0, sr) - 6.0).abs() < 0.15);
}

#[test]
fn cut_band_matches_the_measured_bass_high_pass() {
    // The BA preset's second EQ: CUT at 21.5 Hz, reso 45. Toggling only this
    // band on the same derived preset moved the reference by -15.7 dB at 8.18 Hz
    // and -3.4 dB at 16.36 Hz; the engine matched within 0.5 dB.
    let sr = 48_000.0;
    let cut = EqState::band_coefficients(0, 21.533, 0.0, EQ_TYPE_CUT as f64, 45.084, sr);
    for (freq, expected) in [
        (8.18_f64, -16.5_f64),
        (16.36, -4.5),
        (24.54, -0.3),
        (40.9, 0.5),
    ] {
        let measured = cut.response_db(freq, sr);
        assert!(
            (measured - expected).abs() < 1.0,
            "{freq} Hz: {measured:+.2} dB vs reference {expected:+.2} dB"
        );
    }
    // A steeper (higher-order) cut would gut the sub at twice the depth.
    assert!(cut.response_db(8.18, sr) > -17.0);
}

#[test]
fn eq_resonance_mapping_matches_the_measured_q_curve() {
    for (reso, expected) in [
        (0.0, 0.100),
        (20.0, 0.131),
        (50.0, 1.286),
        (80.0, 7.783),
        (100.0, 18.837),
    ] {
        assert!((eq_q(reso) - expected).abs() < 0.02, "reso {reso}");
    }
    assert!((eq_q(-50.0) - 0.1).abs() < 1e-12);
    assert!(eq_q(150.0) <= 18.85);
}

#[test]
fn eq_without_type_data_keeps_the_legacy_shelves() {
    let mut eq = EqState::default();
    let mut values = [0.0; MAX_FX_VALUE_SLOTS];
    values[EQ_LOW_FREQ] = 250.0;
    values[EQ_HIGH_FREQ] = 4_000.0;
    values[EQ_TYPE1] = -1.0;
    values[EQ_TYPE2] = -1.0;
    for sample in [0.25, -0.75, 0.5] {
        let (left, right) = eq.process(sample, -sample, &values, 48_000.0);
        assert!((left - sample).abs() < 1e-12);
        assert!((right + sample).abs() < 1e-12);
    }
    values[EQ_LOW_GAIN] = 12.0;
    let (lifted, _) = eq.process(0.25, 0.0, &values, 48_000.0);
    assert!(lifted > 0.25);
}

/// Steady sine through the rack, measured on an integer cycle window.
fn distortion_probe(amount: f64, amplitude: f64) -> (f64, f64, f64) {
    const FREQ: f64 = 62.5;
    const PERIOD: usize = 768;
    const CYCLES: usize = 64;
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Distortion);
    fx.set_param("drive", amount);
    fx.set_param("tone", 1.0);
    fx.set_param("level", 1.0);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    let warmup = 48_000;
    let window = PERIOD * CYCLES;
    let mut samples = Vec::with_capacity(window);
    for index in 0..warmup + window {
        let phase = std::f64::consts::TAU * FREQ * index as f64 / 48_000.0;
        let [left, _] = chain.process(amplitude * phase.sin(), 0.0, 48_000.0);
        if index >= warmup {
            samples.push(left);
        }
    }
    let amplitude_at = |harmonic: usize| {
        let mut real = 0.0;
        let mut imag = 0.0;
        for (index, sample) in samples.iter().enumerate() {
            let angle =
                std::f64::consts::TAU * (harmonic * CYCLES) as f64 * index as f64 / window as f64;
            real += sample * angle.cos();
            imag -= sample * angle.sin();
        }
        2.0 * (real * real + imag * imag).sqrt() / window as f64
    };
    let fundamental = amplitude_at(1);
    let harmonics: f64 = (2..=20)
        .map(|harmonic| {
            let value = amplitude_at(harmonic);
            value * value
        })
        .sum::<f64>()
        .sqrt();
    let even: f64 = (1..=10)
        .map(|index| {
            let value = amplitude_at(index * 2);
            value * value
        })
        .sum::<f64>()
        .sqrt();
    let rms = (samples.iter().map(|sample| sample * sample).sum::<f64>() / window as f64).sqrt();
    (rms, harmonics / fundamental, even / fundamental)
}

#[test]
fn distortion_small_signal_gain_follows_the_one_pole_law() {
    assert!((distortion_gain(0.0) - 0.5).abs() < 1e-12);
    let mut previous = 0.0;
    for step in 0..=10 {
        let amount = step as f64 / 10.0;
        let expected = 0.5 / (1.0 - 0.9825 * amount);
        assert!((distortion_gain(amount) - expected).abs() < 1e-12);
        assert!(distortion_gain(amount) > previous, "amount {amount}");
        previous = distortion_gain(amount);
    }
    // Full drive is the measured +29.1 dB, not the old +16 dB sqrt trim.
    assert!((20.0 * distortion_gain(1.0).log10() - 29.1).abs() < 0.3);
    let (unity, _, _) = distortion_probe(0.0, 1e-4);
    let (hot, _, _) = distortion_probe(1.0, 1e-4);
    let measured = 20.0 * (hot / unity).log10();
    assert!(
        (measured - 35.15).abs() < 1.0,
        "measured rise {measured:.2} dB"
    );
}

#[test]
fn distortion_oversampler_is_latency_aligned_and_flat() {
    let mut oversampler = Oversampler8x::new();
    let mut impulse = Vec::new();
    for index in 0..512 {
        let input = if index == 0 { 1.0 } else { 0.0 };
        impulse.push(oversampler.process(input, |sample| sample));
    }
    let peak = impulse
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))
        .map(|(index, _)| index)
        .unwrap();
    assert_eq!(peak, DISTORTION_LATENCY);
    let mut sine = Oversampler8x::new();
    let mut input = Vec::new();
    let mut output = Vec::new();
    for index in 0..24_000 {
        let sample = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.5;
        input.push(sample);
        output.push(sine.process(sample, |value| value));
    }
    let error = |lag: usize| {
        let count = output.len() - 1_000 - lag;
        let sum: f64 = (1_000..(1_000 + count))
            .map(|index| (output[index] - input[index - lag]).powi(2))
            .sum();
        (sum / count as f64).sqrt()
    };
    assert!(
        error(DISTORTION_LATENCY) < 0.02,
        "alignment error {:.4}",
        error(DISTORTION_LATENCY)
    );
    assert!(error(DISTORTION_LATENCY - 1) > error(DISTORTION_LATENCY));
    assert!(error(DISTORTION_LATENCY + 1) > error(DISTORTION_LATENCY));
    let power: f64 = output[1_000..].iter().map(|sample| sample * sample).sum();
    let count = output.len() - 1_000;
    let gain = 20.0 * ((power / count as f64).sqrt() / (0.5 / 2.0_f64.sqrt())).log10();
    assert!(gain.abs() < 0.05, "identity gain {gain:+.3} dB");
}

#[test]
fn distortion_shaper_modes_have_distinct_transfers() {
    let inputs = [-1.0, -0.5, -0.25, 0.25, 0.5, 1.0];
    let mut signatures = Vec::new();
    for mode in [
        ShaperMode::Soft,
        ShaperMode::Hard,
        ShaperMode::Fold,
        ShaperMode::Asymmetric,
        ShaperMode::Diode,
        ShaperMode::Crush,
    ] {
        for drive in [0.2, 0.5, 0.9] {
            let transfer: Vec<f64> = inputs
                .iter()
                .map(|input| shaper_cascade(mode, 4, input * drive))
                .collect();
            assert!(transfer.iter().all(|value| value.is_finite()));
            assert!(transfer.iter().all(|value| value.abs() < 1.5));
            signatures.push(transfer);
        }
        // Every law passes through zero.
        let at_zero = shaper_cascade(mode, 4, 0.0);
        assert!(at_zero.abs() < 1e-12, "{mode:?} at zero: {at_zero}");
    }
    // The four symmetric laws are odd; the biased laws are not.
    for law in [0, 1, 2, 5] {
        for input in [0.1, 0.5, 1.0] {
            let positive = shaper_stage(law, input);
            let negative = shaper_stage(law, -input);
            assert!((positive + negative).abs() < 1e-12, "law {law} is not odd");
        }
    }
    for law in [3, 4] {
        let positive = shaper_stage(law, 0.5);
        let negative = shaper_stage(law, -0.5);
        assert!(
            (positive + negative).abs() > 1e-3,
            "law {law} is not biased"
        );
    }
    // No two modes pin the same transfer at the same drive.
    for first in 0..signatures.len() {
        for second in (first + 1)..signatures.len() {
            assert_ne!(signatures[first], signatures[second]);
        }
    }
    // The cascade stays bounded at maximum drive and stage count.
    for mode in [
        ShaperMode::Soft,
        ShaperMode::Hard,
        ShaperMode::Fold,
        ShaperMode::Asymmetric,
        ShaperMode::Diode,
        ShaperMode::Crush,
    ] {
        for input in [-10.0, -1.0, 1.0, 10.0] {
            let value = shaper_cascade(mode, 16, input * 33.168);
            assert!(value.is_finite() && value.abs() < 1.5, "{mode:?} {input}");
        }
    }
}

#[test]
fn distortion_oversampling_rejects_alias_above_minus_60_db() {
    // 5 kHz at maximum drive through the default law. Non-harmonic DFT
    // bins can only be aliases of harmonics above Nyquist.
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Distortion);
    fx.set_param("drive", 1.0);
    fx.set_param("mode", ShaperMode::Soft.index() as f64);
    fx.set_param("stages", 2.0);
    fx.set_param("tone", 1.0);
    fx.set_param("level", 1.0);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    let window = 2_400;
    let mut samples = Vec::with_capacity(window);
    for index in 0..(4_800 + window) {
        let input = (TAU * 5_000.0 * index as f64 / 48_000.0).sin() * 0.5;
        let [left, _] = chain.process(input, input, 48_000.0);
        if index >= 4_800 {
            samples.push(left);
        }
    }
    let magnitude = |bin: usize| {
        let mut real = 0.0;
        let mut imag = 0.0;
        for (index, sample) in samples.iter().enumerate() {
            let angle = TAU * bin as f64 * index as f64 / window as f64;
            real += sample * angle.cos();
            imag -= sample * angle.sin();
        }
        2.0 * (real * real + imag * imag).sqrt() / window as f64
    };
    let fundamental = magnitude(250);
    assert!(fundamental > 1e-3);
    let mut worst = 0.0_f64;
    let mut worst_bin = 0;
    for bin in 1..window / 2 {
        let harmonic = (bin + 125) / 250 * 250;
        if bin.abs_diff(harmonic) <= 2 {
            continue;
        }
        let level = magnitude(bin);
        if level > worst {
            worst = level;
            worst_bin = bin;
        }
    }
    let rejection = 20.0 * (worst / fundamental).log10();
    assert!(
        rejection < -60.0,
        "alias at bin {worst_bin}: {rejection:+.1} dBc"
    );
}

#[test]
fn distortion_drive_map_is_monotone_and_trim_is_unit_at_default_mode() {
    let mut previous = 0.0;
    for value in DISTORTION_DRIVE {
        assert!(value > previous);
        previous = value;
    }
    let mut previous = 0.0;
    for step in 0..=100 {
        let amount = step as f64 / 100.0;
        let gain = distortion_gain(amount);
        assert!(gain > previous, "gain is not monotone at {amount}");
        previous = gain;
    }
    assert_eq!(distortion_trim(0.0), 1.0);
    for step in 0..=10 {
        let amount = step as f64 / 10.0;
        assert!(
            (distortion_trim(amount)
                - distortion_gain(amount) / table_lerp(&DISTORTION_DRIVE, amount))
            .abs()
                < 1e-12
        );
    }
    // The measured small-signal law still holds through the module.
    fn small_signal_gain(amount: f64) -> f64 {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Distortion);
        fx.set_param("drive", amount);
        fx.set_param("mode", 0.0);
        fx.set_param("stages", 2.0);
        fx.set_param("tone", 1.0);
        fx.set_param("level", 1.0);
        fx.set_param("mix", 1.0);
        chain.sync_from_patch(&[fx]);
        let amplitude = 1e-4;
        let mut sum = 0.0;
        let mut count = 0;
        for index in 0..24_000 {
            let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * amplitude;
            let [left, _] = chain.process(input, input, 48_000.0);
            if index >= 4_800 {
                sum += left * left;
                count += 1;
            }
        }
        20.0 * ((sum / count as f64).sqrt() / (amplitude / 2.0_f64.sqrt())).log10()
    }
    for amount in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let measured = small_signal_gain(amount);
        let expected = 20.0 * distortion_gain(amount).log10();
        assert!(
            (measured - expected).abs() < 0.05,
            "amount {amount}: {measured:+.2} dB vs {expected:+.2} dB"
        );
    }
}

#[test]
fn distortion_zero_drive_is_a_harmonic_null() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Distortion);
    fx.set_param("drive", 0.0);
    fx.set_param("mode", 0.0);
    fx.set_param("stages", 2.0);
    fx.set_param("tone", 1.0);
    fx.set_param("level", 1.0);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    let window = 4_800;
    let mut samples = Vec::with_capacity(window);
    for index in 0..(4_800 + window) {
        let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.5;
        let [left, _] = chain.process(input, input, 48_000.0);
        if index >= 4_800 {
            samples.push(left);
        }
    }
    let magnitude = |bin: usize| {
        let mut real = 0.0;
        let mut imag = 0.0;
        for (index, sample) in samples.iter().enumerate() {
            let angle = TAU * bin as f64 * index as f64 / window as f64;
            real += sample * angle.cos();
            imag -= sample * angle.sin();
        }
        2.0 * (real * real + imag * imag).sqrt() / window as f64
    };
    let fundamental = magnitude(100);
    for harmonic in 2..=20 {
        let level = 20.0 * (magnitude(harmonic * 100) / fundamental).log10();
        assert!(level < -120.0, "harmonic {harmonic}: {level:.1} dBc");
    }
    let gain = 20.0 * (fundamental / 0.5).log10();
    assert!((gain + 6.0206).abs() < 0.05, "0-drive gain {gain:+.3} dB");
}

#[test]
fn distortion_thd_follows_the_multi_stage_law() {
    // Default mode (soft), two stages, 0.165-peak 62.5 Hz probe. This is the
    // new multi-stage law; the monotone drive taper and the unit trim are
    // still the measured ones. See docs/fx-delay-distortion.md.
    for (amount, expected) in [
        (0.0, 0.0),
        (0.1, 0.262),
        (0.2, 0.610),
        (0.3, 1.096),
        (0.4, 1.836),
        (0.5, 3.099),
        (0.6, 5.096),
        (0.7, 6.054),
        (0.8, 7.272),
        (0.9, 9.898),
        (1.0, 31.959),
    ] {
        let (_, thd, even) = distortion_probe(amount, 0.165);
        let percent = thd * 100.0;
        let tolerance = (expected * 0.05_f64).max(0.05_f64);
        assert!(
            (percent - expected).abs() < tolerance,
            "amount {amount}: THD {percent:.3}% vs {expected}%"
        );
        assert!(even < 1e-6, "amount {amount}: even content {even}");
    }
    let (_, thd, _) = distortion_probe(0.486_842, 0.165);
    assert!(
        (thd * 100.0 - 2.919).abs() < 0.05,
        "bass-knob THD {:.3}%",
        thd * 100.0
    );
    // Level-compensated drive: at the bass knob a 0.44-peak sine keeps its
    // RMS within 1.5 dB instead of the old 3.5 dB loss.
    let (rms44, _, _) = distortion_probe(0.486_842, 0.44);
    let gain = 20.0 * (rms44 / (0.44 / std::f64::consts::SQRT_2)).log10();
    assert!(
        (-1.5..=0.0).contains(&gain),
        "bass-knob 0.44-peak module gain {gain:+.2} dB (RMS {rms44:.4})"
    );
}

/// Steady saw through the chorus: stereo gain and the worst 50 ms window.
fn chorus_probe(mix: f64) -> (f64, f64) {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Chorus);
    fx.set_param("rate", 0.6);
    fx.set_param("depth", 0.35);
    fx.set_param("feedback", 0.0);
    fx.set_param("mix", mix);
    chain.sync_from_patch(&[fx]);
    let mut samples = Vec::new();
    for index in 0..144_000 {
        let mut input = 0.0;
        for harmonic in 1..=80 {
            let phase = std::f64::consts::TAU * 130.0 * harmonic as f64 * index as f64 / 48_000.0;
            input += phase.sin() / harmonic as f64;
        }
        let input = input * 0.18;
        let [left, right] = chain.process(input, input, 48_000.0);
        if index >= 48_000 {
            samples.push([left, right]);
        }
    }
    let rms = (samples
        .iter()
        .map(|[left, right]| (left * left + right * right) * 0.5)
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt();
    let mut windows = Vec::new();
    for chunk in samples.chunks(2_400) {
        if chunk.len() == 2_400 {
            windows.push(
                (chunk
                    .iter()
                    .map(|[left, right]| (left * left + right * right) * 0.5)
                    .sum::<f64>()
                    / 2_400.0)
                    .sqrt(),
            );
        }
    }
    let mean = windows.iter().sum::<f64>() / windows.len() as f64;
    let min = windows.iter().copied().fold(f64::INFINITY, f64::min);
    (rms, min / mean)
}

#[test]
fn delay_full_wet_is_level_neutral() {
    fn probe(mix: f64) -> f64 {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Delay);
        fx.set_param("time", 0.0); // 0.25 s
        fx.set_param("feedback", 0.0);
        fx.set_param("tone", 1.0);
        fx.set_param("mix", mix);
        chain.sync_from_patch(&[fx]);
        let mut sum = 0.0;
        let mut count = 0;
        for index in 0..96_000 {
            let mut input = 0.0;
            for harmonic in 1..=80 {
                let phase =
                    std::f64::consts::TAU * 130.0 * harmonic as f64 * index as f64 / 48_000.0;
                input += phase.sin() / harmonic as f64;
            }
            let input = input * 0.18;
            let [left, right] = chain.process(input, input, 48_000.0);
            if index >= 48_000 {
                sum += (left * left + right * right) * 0.5;
                count += 1;
            }
        }
        (sum / count as f64).sqrt()
    }
    let dry = probe(0.0);
    let wet = probe(1.0);
    let gain = 20.0 * (wet / dry).log10();
    assert!(gain.abs() < 0.2, "full-wet gain {gain:+.2} dB");
}

#[test]
fn chorus_wet_response_matches_the_measured_staging() {
    let (dry, _) = chorus_probe(0.0);
    // Expected gain staging: +2.9 / +1.5 / -0.5 dB at 25 / 50 / 75 %.
    for (mix, expected) in [(0.25, 2.9), (0.5, 1.5), (0.75, -0.5), (1.0, 0.0)] {
        let (rms, _) = chorus_probe(mix);
        let gain = 20.0 * (rms / dry).log10();
        assert!(
            (gain - expected).abs() < 0.9,
            "mix {mix}: {gain:.2} dB vs {expected} dB"
        );
    }
    // The dual voice keeps the 50 % sum from notching; a single tap drops
    // the envelope towards zero as its comb crosses the dry.
    for mix in [0.25, 0.5] {
        let (_, floor) = chorus_probe(mix);
        assert!(floor > 0.6, "mix {mix}: envelope floor {floor:.2}");
    }
}

#[test]
fn chorus_wet_zero_is_the_unprocessed_signal() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Chorus);
    fx.set_param("mix", 0.0);
    chain.sync_from_patch(&[fx]);
    for sample in [0.0, 0.1, -0.7, 1.3] {
        assert_eq!(chain.process(sample, -sample, 48_000.0), [sample, -sample]);
    }
}

#[test]
fn compressor_ratio_reaches_limiter_values() {
    // 7 dB over a -20 dB RMS threshold at the mid band.
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Compressor);
    fx.set_param("threshold", -20.0);
    fx.set_param("ratio", 1_000.0);
    fx.set_param("attack", 0.1);
    fx.set_param("release", 50.0);
    fx.set_param("makeup", 0.0);
    chain.sync_from_patch(&[fx]);
    let mut rms = 0.0;
    let mut count = 0;
    for index in 0..96_000 {
        let phase = std::f64::consts::TAU * 1_000.0 * index as f64 / 48_000.0;
        let input = 0.316 * phase.sin();
        let [left, _] = chain.process(input, input, 48_000.0);
        if index >= 48_000 {
            rms += left * left;
            count += 1;
        }
    }
    let gain = 20.0 * ((rms / count as f64).sqrt() / (0.316 / std::f64::consts::SQRT_2)).log10();
    assert!((gain + 7.0).abs() < 1.5, "gain {gain:.2} dB");
}

#[test]
fn wet_dry_crossfade_preserves_a_correlated_level() {
    let (dry, wet) = wet_dry_gains(0.0);
    assert!((dry - 1.0).abs() < 1e-12 && wet.abs() < 1e-12);
    let (dry, wet) = wet_dry_gains(1.0);
    assert!(dry.abs() < 1e-12 && (wet - 1.0).abs() < 1e-12);
    // An equal-power crossfade made every mix +3 dB hot for correlated
    // content, which compounded to +10.7 dB on the bass chain.
    for mix in [0.064, 0.146, 0.25, 0.5, 0.671] {
        let (dry, wet) = wet_dry_gains(mix);
        assert!((dry + wet - 1.0).abs() < 1e-12, "mix {mix} is not unity");
    }
}

#[test]
fn power_staging_keeps_a_decorrelated_sum_level_neutral() {
    // A delayed or reverberated copy has no fixed phase relation to the
    // dry, so its power adds: `d^2 + w^2 = 1` is the neutral split.
    for mix in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let (dry, wet) = wet_dry_power(mix);
        assert!(
            (dry * dry + wet * wet - 1.0).abs() < 1e-12,
            "mix {mix} is not power-neutral"
        );
    }
    assert!((wet_dry_power(1.0).1 - 1.0).abs() < 1e-12);
}

#[test]
fn serial_wet_modules_do_not_inflate_the_chain() {
    // The bass rack runs four wet modules in series; the amplitude
    // law keeps the summed staging at unity where equal-power was +6.2 dB.
    let mut chain = SynthFxChain::new();
    let mut fx = Vec::new();
    for mix in [0.671, 0.146, 0.146, 0.064] {
        let mut module = SynthFx::new(SynthFxType::Drive);
        module.set_param("drive", 0.0);
        module.set_param("tone", 1.0);
        module.set_param("level", 1.0);
        module.set_param("mix", mix);
        fx.push(module);
    }
    chain.sync_from_patch(&fx);
    let (mut input, mut output) = (0.0, 0.0);
    for index in 0..24_000 {
        let phase = std::f64::consts::TAU * 65.0 * index as f64 / 48_000.0;
        let x = phase.sin() * 0.25;
        let [left, right] = chain.process(x, x, 48_000.0);
        if index > 12_000 {
            input += x * x;
            output += (left * left + right * right) * 0.5;
        }
    }
    let gain_db = 10.0 * (output / input).log10();
    assert!(gain_db.abs() < 0.5, "serial staging gain {gain_db:+.2} dB");
}

#[test]
fn level_out_scales_the_whole_module_output() {
    // Wet 0: the post-mix trim must scale the dry path too.
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Distortion);
    fx.set_param("drive", 0.0);
    fx.set_param("mix", 0.0);
    fx.set_param("level_out", 4.0);
    chain.sync_from_patch(&[fx]);
    // The 8x shaper has a fixed latency, so the dry path is aligned.
    chain.process(0.1, -0.2, 48_000.0);
    for _ in 0..(DISTORTION_LATENCY - 1) {
        chain.process(0.0, 0.0, 48_000.0);
    }
    let (left, right) = chain.process(0.0, 0.0, 48_000.0).into();
    assert!((left - 0.4).abs() < 1e-9);
    assert!((right + 0.8).abs() < 1e-9);

    // Wet 1: the same trim scales the wet path.
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Delay);
    fx.set_param("mix", 1.0);
    fx.set_param("feedback", 0.0);
    fx.set_param("level_out", 0.5);
    chain.sync_from_patch(&[fx]);
    for _ in 0..8_000 {
        chain.process(0.0, 0.0, 48_000.0);
    }
    let (left, _) = chain.process(1.0, 1.0, 48_000.0).into();
    // The first sample is delayed, so only the trim half of the path shows.
    assert!(left.abs() < 1e-9);
}

#[test]
fn omitted_params_use_the_module_defaults() {
    let fx = SynthFx {
        fx_type: SynthFxType::Eq3,
        enabled: true,
        params: Vec::new(),
    };
    let module = FxModule::from_patch(&fx, 0);
    assert_eq!(module.values[EQ_TYPE1], -1.0);
    assert_eq!(module.values[module.level_out_slot], 1.0);
}

#[test]
fn compressor_makeup_stays_in_decibels() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Compressor);
    fx.set_param("threshold", 0.0);
    fx.set_param("ratio", 1.0);
    fx.set_param("makeup", 6.0);
    chain.sync_from_patch(&[fx]);
    let mut last = (0.0, 0.0);
    for _ in 0..2_000 {
        let [left, right] = chain.process(0.1, -0.1, 48_000.0);
        last = (left, right);
    }
    assert!((last.0 - 0.1995).abs() < 1e-3);
    assert!((last.1 + 0.1995).abs() < 1e-3);
}

fn rms_of(samples: &[f64]) -> f64 {
    (samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64).sqrt()
}

fn sine_rms(fx: SynthFx, freq: f64, seconds: f64) -> (f64, f64) {
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let total = (seconds * 48_000.0) as usize;
    let skip = total / 4;
    let mut left = Vec::with_capacity(total);
    let mut right = Vec::with_capacity(total);
    for index in 0..total {
        let phase = TAU * freq * index as f64 / 48_000.0;
        let input = phase.sin() * 0.5;
        let [l, r] = chain.process(input, input, 48_000.0);
        if index >= skip {
            left.push(l);
            right.push(r);
        }
    }
    (rms_of(&left), rms_of(&right))
}

fn goertzel(samples: &[f64], freq: f64) -> f64 {
    let omega = TAU * freq / 48_000.0;
    let coefficient = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for sample in samples {
        let s0 = sample + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coefficient * s1 * s2).max(0.0).sqrt() / samples.len() as f64
}

#[test]
fn every_adjacent_module_runs_in_series() {
    // A module-skipping bug in the splitter walk would leave one of these
    // trims unapplied; two serial half-gain modules must be 0.25x.
    let mut fx = SynthFx::new(SynthFxType::Filter);
    fx.set_param("mix", 0.0);
    fx.set_param("level_out", 0.5);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx.clone(), fx]);
    let [left, right] = chain.process(0.8, -0.4, 48_000.0);
    assert!((left - 0.2).abs() < 1e-12, "{left}");
    assert!((right + 0.1).abs() < 1e-12, "{right}");
}

#[test]
fn utils_filters_match_the_measured_response() {
    let mut fx = SynthFx::new(SynthFxType::Utils);
    fx.set_param("hpf", 400.0);
    fx.set_param("lpf", 20_000.0);
    // One 2-pole stage is -3 dB at the corner, the LR4 pair -6 dB; the
    // measured curve is -5.7 dB at 400 Hz and -24.6 dB at 200 Hz.
    let (corner_db, _) = sine_rms(fx.clone(), 400.0, 0.4);
    let (octave_db, _) = sine_rms(fx.clone(), 200.0, 0.4);
    let (above_db, _) = sine_rms(fx.clone(), 800.0, 0.4);
    let corner = 20.0 * (corner_db / (0.5 / std::f64::consts::SQRT_2)).log10();
    let octave = 20.0 * (octave_db / (0.5 / std::f64::consts::SQRT_2)).log10();
    let above = 20.0 * (above_db / (0.5 / std::f64::consts::SQRT_2)).log10();
    assert!((corner + 6.0).abs() < 0.7, "400 Hz {corner:+.2} dB");
    assert!((octave + 24.5).abs() < 1.5, "200 Hz {octave:+.2} dB");
    assert!(above > -1.0, "800 Hz {above:+.2} dB");

    // The LPF is a single 2-pole Butterworth: -3 dB at the corner and
    // -12 dB one octave up, unlike the 24 dB/oct HPF.
    let mut low = SynthFx::new(SynthFxType::Utils);
    low.set_param("hpf", 20.0);
    low.set_param("lpf", 1_000.0);
    let (corner, _) = sine_rms(low.clone(), 1_000.0, 0.4);
    let (octave, _) = sine_rms(low, 2_000.0, 0.4);
    let corner = 20.0 * (corner / (0.5 / std::f64::consts::SQRT_2)).log10();
    let octave = 20.0 * (octave / (0.5 / std::f64::consts::SQRT_2)).log10();
    assert!((corner + 3.0).abs() < 0.7, "1 kHz {corner:+.2} dB");
    assert!((octave + 12.0).abs() < 1.0, "2 kHz {octave:+.2} dB");
}

#[test]
fn utils_neutral_setting_is_level_neutral() {
    let fx = SynthFx::new(SynthFxType::Utils);
    let (left, right) = sine_rms(fx, 1_000.0, 0.4);
    let reference = 0.5 / std::f64::consts::SQRT_2;
    assert!((left / reference - 1.0).abs() < 0.002, "{left}");
    assert!((right / reference - 1.0).abs() < 0.002, "{right}");
}

#[test]
fn utils_width_balance_and_polarity_transform_as_expected() {
    let run = |params: &[(&str, f64)], left: f64, right: f64| {
        let mut fx = SynthFx::new(SynthFxType::Utils);
        for (name, value) in params {
            fx.set_param(name, *value);
        }
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[fx]);
        let total = 24_000;
        let mut out = Vec::with_capacity(total);
        for index in 0..total {
            let phase = TAU * 1_000.0 * index as f64 / 48_000.0;
            let input = phase.sin() * 0.25;
            let [l, r] = chain.process(input * left, input * right, 48_000.0);
            if index > 4_000 {
                out.push((l, r));
            }
        }
        out
    };
    // Pure side input doubles at width 200 and vanishes at width 0.
    let doubled = run(&[("width", 200.0)], 1.0, -1.0);
    let (l, r) = doubled[0];
    assert!((l - (-r)).abs() < 1e-9);
    let side = rms_of(
        &doubled
            .iter()
            .map(|(l, r)| (l - r) * 0.5)
            .collect::<Vec<_>>(),
    );
    let mid = rms_of(
        &doubled
            .iter()
            .map(|(l, r)| (l + r) * 0.5)
            .collect::<Vec<_>>(),
    );
    assert!(side > 1e-3 && mid < 1e-6, "side {side} mid {mid}");
    let collapsed = run(&[("width", 0.0)], 1.0, -1.0);
    assert!(
        collapsed
            .iter()
            .all(|(l, r)| l.abs() < 1e-9 && r.abs() < 1e-9)
    );

    // Linear balance mutes the opposite channel, and polarity inverts it.
    let left_only = run(&[("balance", -100.0)], 1.0, 1.0);
    assert!(rms_of(&left_only.iter().map(|(_, r)| *r).collect::<Vec<_>>()) < 1e-6);
    let inverted = run(&[("polarity_l", 1.0)], 1.0, 1.0);
    assert!(inverted.iter().all(|(l, r)| (l + r).abs() < 1e-9));
}

#[test]
fn empty_splitters_are_magnitude_flat() {
    for fx_type in [
        SynthFxType::Split,
        SynthFxType::Split3,
        SynthFxType::SplitMs,
    ] {
        let fx = SynthFx::new(fx_type);
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[fx]);
        let mut seed = 0x1020_3040_5060_7080u64;
        let mut input_energy = 0.0;
        let mut output_energy = 0.0;
        for index in 0..48_000 {
            let sample = noise(&mut seed) * 0.2;
            let [l, r] = chain.process(sample, -sample, 48_000.0);
            if index > 4_800 {
                input_energy += sample * sample;
                output_energy += (l * l + r * r) * 0.5;
            }
        }
        let gain = 10.0 * (output_energy / input_energy).log10();
        assert!(gain.abs() < 0.1, "{fx_type:?} gain {gain:+.2} dB");
    }
}

#[test]
fn a_splitter_only_processes_its_branch_children() {
    // High band gets a 200 Hz low-pass; the 100 Hz low band stays clean.
    let mut split = SynthFx::new(SynthFxType::Split);
    split.set_param("freq", 500.0);
    split.set_param("count1", 0.0);
    split.set_param("count2", 1.0);
    let mut filter = SynthFx::new(SynthFxType::Filter);
    filter.set_param("cutoff", 200.0);
    filter.set_param("resonance", 0.0);
    filter.set_param("mix", 1.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[split, filter]);
    let mut low = Vec::new();
    let mut high = Vec::new();
    for index in 0..24_000 {
        let low_input = (TAU * 100.0 * index as f64 / 48_000.0).sin() * 0.25;
        if index > 4_800 {
            low.push(chain.process(low_input, low_input, 48_000.0)[0]);
        }
    }
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[
        {
            let mut split = SynthFx::new(SynthFxType::Split);
            split.set_param("freq", 500.0);
            split.set_param("count1", 0.0);
            split.set_param("count2", 1.0);
            split
        },
        {
            let mut filter = SynthFx::new(SynthFxType::Filter);
            filter.set_param("cutoff", 200.0);
            filter.set_param("resonance", 0.0);
            filter.set_param("mix", 1.0);
            filter
        },
    ]);
    for index in 0..24_000 {
        let high_input = (TAU * 4_000.0 * index as f64 / 48_000.0).sin() * 0.25;
        if index > 4_800 {
            high.push(chain.process(high_input, high_input, 48_000.0)[0]);
        }
    }
    assert!(rms_of(&low) > 0.15, "low {}", rms_of(&low));
    assert!(rms_of(&high) < 0.002, "high {}", rms_of(&high));
}

#[test]
fn parallel_rack_sums_branches_with_their_gains() {
    // Branch 1 is dry; branch 2 is a pass-through filter scaled by one half.
    let mut parallel = SynthFx::new(SynthFxType::ParallelRack);
    parallel.set_param("count2", 1.0);
    parallel.set_param("gain2", 0.5);
    let mut filter = SynthFx::new(SynthFxType::Filter);
    filter.set_param("mix", 0.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[parallel.clone(), filter.clone()]);
    let [left, right] = chain.process(0.2, -0.4, 48_000.0);
    assert!((left - 0.3).abs() < 1e-12, "{left}");
    assert!((right + 0.6).abs() < 1e-12, "{right}");

    // A muted branch contributes exactly zero and leaves the dry path.
    parallel.set_param("gain2", 0.0);
    chain.sync_from_patch(&[parallel, filter]);
    let [left, right] = chain.process(0.2, -0.4, 48_000.0);
    assert!((left - 0.2).abs() < 1e-12, "{left}");
    assert!((right + 0.4).abs() < 1e-12, "{right}");
}

#[test]
fn split_band_gains_scale_and_mute_only_their_band() {
    let freq = 500.0;
    let mut split = SynthFx::new(SynthFxType::Split);
    split.set_param("freq", freq);
    split.set_param("gain2", 0.5);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[split.clone()]);
    let mut reference = CrossoverState::default();
    let mut seed = 0x0bad_f00d_dead_beefu64;
    for _ in 0..4_096 {
        let input = noise(&mut seed) * 0.3;
        let (lows, highs) = reference.split_2(input, -input, freq, 48_000.0);
        let [left, right] = chain.process(input, -input, 48_000.0);
        let expected_left = lows[0] + highs[0] * 0.5;
        let expected_right = lows[1] + highs[1] * 0.5;
        assert!((left - expected_left).abs() < 1e-12);
        assert!((right - expected_right).abs() < 1e-12);
    }

    // Muting the high band leaves the low band exactly as the crossover
    // produced it.
    split.set_param("gain2", 0.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[split]);
    let mut reference = CrossoverState::default();
    let mut seed = 0x0bad_f00d_dead_beefu64;
    for _ in 0..4_096 {
        let input = noise(&mut seed) * 0.3;
        let (lows, _) = reference.split_2(input, -input, freq, 48_000.0);
        let [left, right] = chain.process(input, -input, 48_000.0);
        assert!((left - lows[0]).abs() < 1e-12);
        assert!((right - lows[1]).abs() < 1e-12);
    }
}

#[test]
fn mid_side_band_gain_mutes_the_side_band() {
    let mut split = SynthFx::new(SynthFxType::SplitMs);
    split.set_param("gain1", 1.0);
    split.set_param("gain2", 0.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[split]);
    let [left, right] = chain.process(0.2, -0.2, 48_000.0);
    assert!(left.abs() < 1e-12 && right.abs() < 1e-12);
}

#[test]
fn nested_split_processes_its_inner_chain() {
    // The outer low band feeds a nested split whose high band is low-passed
    // at 100 Hz. The outer high band is muted, so a 1 kHz tone can only
    // reach the output through the nested chain and is removed there.
    let mut outer = SynthFx::new(SynthFxType::Split);
    outer.set_param("freq", 500.0);
    outer.set_param("count1", 1.0);
    outer.set_param("count2", 0.0);
    outer.set_param("gain2", 0.0);
    let mut inner = SynthFx::new(SynthFxType::Split);
    inner.set_param("freq", 200.0);
    inner.set_param("count1", 0.0);
    inner.set_param("count2", 1.0);
    let mut filter = SynthFx::new(SynthFxType::Filter);
    filter.set_param("cutoff", 100.0);
    filter.set_param("resonance", 0.0);
    filter.set_param("mix", 1.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[outer, inner, filter]);
    let mut high = Vec::new();
    for index in 0..24_000 {
        let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.25;
        if index > 4_800 {
            high.push(chain.process(input, input, 48_000.0)[0]);
        }
    }
    assert!(rms_of(&high) < 0.005, "high {}", rms_of(&high));

    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[
        {
            let mut outer = SynthFx::new(SynthFxType::Split);
            outer.set_param("freq", 500.0);
            outer.set_param("count1", 1.0);
            outer.set_param("count2", 0.0);
            outer.set_param("gain2", 0.0);
            outer
        },
        {
            let mut inner = SynthFx::new(SynthFxType::Split);
            inner.set_param("freq", 200.0);
            inner.set_param("count1", 0.0);
            inner.set_param("count2", 1.0);
            inner
        },
        {
            let mut filter = SynthFx::new(SynthFxType::Filter);
            filter.set_param("cutoff", 100.0);
            filter.set_param("resonance", 0.0);
            filter.set_param("mix", 1.0);
            filter
        },
    ]);
    let mut low = Vec::new();
    for index in 0..24_000 {
        let input = (TAU * 100.0 * index as f64 / 48_000.0).sin() * 0.25;
        if index > 4_800 {
            low.push(chain.process(input, input, 48_000.0)[0]);
        }
    }
    assert!(rms_of(&low) > 0.05, "low {}", rms_of(&low));
}

#[test]
fn bode_frequency_shift_moves_a_sine() {
    let mut fx = SynthFx::new(SynthFxType::Bode);
    fx.set_param("shift", 10.0);
    fx.set_param("range", 1_000.0);
    fx.set_param("output_width", 0.0);
    fx.set_param("mono_input", 1.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let mut output = Vec::new();
    for index in 0..48_000 {
        let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.5;
        let [l, _] = chain.process(input, input, 48_000.0);
        if index > 4_800 {
            output.push(l);
        }
    }
    let lower = goertzel(&output, 900.0);
    let centre = goertzel(&output, 1_000.0);
    let upper = goertzel(&output, 1_100.0);
    assert!(lower > 0.1 && upper > 0.1, "{lower} {upper}");
    assert!(centre < lower * 0.05, "carrier {centre}");

    // Output mix selects one sideband; width spreads them to L/R.
    let mut mono = SynthFx::new(SynthFxType::Bode);
    mono.set_param("shift", 10.0);
    mono.set_param("range", 1_000.0);
    mono.set_param("output_mix", -100.0);
    mono.set_param("output_width", 100.0);
    mono.set_param("mono_input", 1.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[mono]);
    let mut output = Vec::new();
    for index in 0..48_000 {
        let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.5;
        let [l, r] = chain.process(input, input, 48_000.0);
        if index > 4_800 {
            output.push((l, r));
        }
    }
    let left = rms_of(&output.iter().map(|(l, _)| *l).collect::<Vec<_>>());
    let right = rms_of(&output.iter().map(|(_, r)| *r).collect::<Vec<_>>());
    assert!((left - right).abs() < 1e-9, "{left} {right}");
    let lower = goertzel(&output.iter().map(|(l, _)| *l).collect::<Vec<_>>(), 900.0);
    let upper = goertzel(&output.iter().map(|(l, _)| *l).collect::<Vec<_>>(), 1_100.0);
    assert!(lower > 0.1 && upper < lower * 0.05, "{lower} {upper}");
}

#[test]
fn bode_zero_shift_is_a_neutral_delay() {
    let mut fx = SynthFx::new(SynthFxType::Bode);
    fx.set_param("shift", 0.0);
    fx.set_param("range", 100.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let mut seed = 0xdead_beef_1234_5678u64;
    let mut input = Vec::new();
    let mut output = Vec::new();
    for _ in 0..1_000 {
        let sample = noise(&mut seed) * 0.2;
        input.push(sample);
        output.push(chain.process(sample, sample, 48_000.0));
    }
    let latency = BODE_HILBERT_CENTER;
    for index in latency..input.len() {
        assert!((output[index][0] - input[index - latency]).abs() < 1e-9);
        assert!((output[index][1] - input[index - latency]).abs() < 1e-9);
    }
}

#[test]
fn new_fx_families_stay_finite_at_extremes() {
    let cases: [(SynthFxType, &[(&str, f64)]); 5] = [
        (
            SynthFxType::Utils,
            &[
                ("hpf", 400.0),
                ("lpf", 50.0),
                ("lf_mono", 1.0),
                ("lf_xover", 400.0),
                ("width", 800.0),
                ("balance", -100.0),
                ("polarity_l", 1.0),
                ("polarity_r", 1.0),
                ("mix", 1.0),
                ("level_out", 4.0),
            ],
        ),
        (SynthFxType::Split, &[("freq", 20.0)]),
        (SynthFxType::Split3, &[("freq", 20.0), ("freq2", 20.0)]),
        (SynthFxType::SplitMs, &[]),
        (
            SynthFxType::Bode,
            &[
                ("shift", 100.0),
                ("range", 5_000.0),
                ("feedback", 0.95),
                ("delay", 0.01),
                ("output_mix", 100.0),
                ("output_width", 100.0),
                ("mix", 1.0),
            ],
        ),
    ];
    for (fx_type, params) in cases {
        let mut fx = SynthFx::new(fx_type);
        for (name, value) in params {
            fx.set_param(name, *value);
        }
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[fx]);
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..48_000 {
            let input = noise(&mut seed);
            let [l, r] = chain.process(input, -input, 48_000.0);
            assert!(l.is_finite() && r.is_finite(), "{fx_type:?}");
            assert!(l.abs() < 20.0 && r.abs() < 20.0, "{fx_type:?} {l} {r}");
        }
    }
}

fn render_left(fx: SynthFx, samples: usize, mono: bool) -> Vec<f64> {
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut output = Vec::with_capacity(samples);
    for _ in 0..samples {
        let input = noise(&mut seed) * 0.25;
        let [left, _] = chain.process(input, if mono { input } else { -input }, 48_000.0);
        output.push(left);
    }
    output
}

fn rms_difference(a: &[f64], b: &[f64]) -> f64 {
    let energy = a
        .iter()
        .zip(b)
        .map(|(left, right)| (left - right) * (left - right))
        .sum::<f64>();
    (energy / a.len() as f64).sqrt()
}

#[test]
fn hyperd_impulse_response_is_a_diffuse_network() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::HyperD);
    fx.set_param("mix", 1.0);
    fx.set_param("depth", 0.5);
    fx.set_param("feedback", 0.6);
    chain.sync_from_patch(&[fx]);
    let mut output = Vec::new();
    for index in 0..9_600 {
        let input = if index == 0 { 1.0 } else { 0.0 };
        let [left, _] = chain.process(input, 0.0, 48_000.0);
        output.push(left);
    }
    let peak = output
        .iter()
        .fold(0.0_f64, |peak, value| peak.max(value.abs()));
    let floor = peak * 1e-3;
    let significant = output.iter().filter(|value| value.abs() > floor).count();
    let last = output
        .iter()
        .rposition(|value| value.abs() > floor)
        .unwrap_or(0);
    // A single modulated delay would expose one tap; the diffuser spreads
    // the impulse over many taps and several milliseconds.
    assert!(significant >= 16, "only {significant} taps");
    assert!(last > 240, "tail ends after {last} samples");
}

#[test]
fn hyperd_modulation_phase_changes_the_output() {
    let patch = || {
        let mut fx = SynthFx::new(SynthFxType::HyperD);
        fx.set_param("rate", 0.4);
        fx.set_param("depth", 0.5);
        fx.set_param("mix", 1.0);
        fx
    };
    let render = |phase: f64, depth: f64| {
        let mut fx = patch();
        fx.set_param("depth", depth);
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[fx]);
        chain.hyperds[0].phase = phase;
        let mut seed = 0x0f0f_1234_5678_9abcu64;
        let mut output = Vec::new();
        for _ in 0..24_000 {
            let input = noise(&mut seed) * 0.2;
            output.push(chain.process(input, input, 48_000.0)[0]);
        }
        output
    };
    let first = render(0.0, 0.5);
    let second = render(0.37, 0.5);
    let modulated = rms_difference(&first, &second) / rms_of(&first);
    assert!(modulated > 0.05, "phase difference {modulated:.4}");
    // With the LFO depth at zero the network is static, so the start phase
    // must not matter: the difference is modulation, not a fixed offset.
    let static_first = render(0.0, 0.0);
    let static_second = render(0.37, 0.0);
    assert!(rms_difference(&static_first, &static_second) < 1e-12);
}

#[test]
fn hyperd_side_is_wide_and_the_mono_sum_is_exact() {
    let mut fx = SynthFx::new(SynthFxType::HyperD);
    fx.set_param("mix", 1.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let mut seed = 0xdead_beef_cafe_babeu64;
    let (mut side_energy, mut mid_energy) = (0.0, 0.0);
    let mut worst_error = 0.0_f64;
    for index in 0..48_000 {
        let input = noise(&mut seed) * 0.25;
        let [left, right] = chain.process(input, input, 48_000.0);
        if index > 4_800 {
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;
            mid_energy += mid * mid;
            side_energy += side * side;
            let error = ((left + right) - 2.0 * input).abs();
            worst_error = worst_error.max(error);
        }
    }
    let ratio = 10.0 * (side_energy / mid_energy).log10();
    assert!(ratio > -6.0, "side/mid only {ratio:+.2} dB");
    assert!(worst_error < 1e-12, "mono sum error {worst_error:e}");
}

#[test]
fn hyperd_is_loudness_sane() {
    let output = render_left(
        {
            let mut fx = SynthFx::new(SynthFxType::HyperD);
            fx.set_param("mix", 1.0);
            fx
        },
        48_000,
        true,
    );
    let input_rms = 0.15 / 3.0_f64.sqrt();
    let gain = 20.0 * (rms_of(&output) / input_rms).log10();
    assert!(gain.abs() < 6.0, "wet gain {gain:+.2} dB");
}

#[test]
fn convolution_ir_generation_is_deterministic() {
    let mut first = ConvState::new();
    let mut second = ConvState::new();
    first.prepare(48_000.0, 2);
    second.prepare(48_000.0, 2);
    for channel in 0..2 {
        assert_eq!(first.taps(channel), second.taps(channel));
    }
    // A rate round-trip must not leave state in the generator.
    first.prepare(96_000.0, 2);
    first.prepare(48_000.0, 2);
    assert_eq!(first.taps(0), second.taps(0));
    assert_eq!(first.taps(1), second.taps(1));
    let mut other = ConvState::new();
    other.prepare(48_000.0, 0);
    assert_ne!(first.taps(0), other.taps(0));
}

#[test]
fn convolution_matches_direct_convolution() {
    let mut chain = SynthFxChain::new();
    let mut fx = SynthFx::new(SynthFxType::Conv);
    fx.set_param("size", 0.1);
    fx.set_param("damp", 0.0);
    fx.set_param("predelay", 0.0);
    fx.set_param("width", 1.0);
    fx.set_param("mix", 1.0);
    chain.sync_from_patch(&[fx]);
    let taps = chain.convolutions[0].taps(0).to_vec();
    let total = CONV_LATENCY + taps.len() + 2 * CONV_BLOCK;
    let mut output = Vec::with_capacity(total);
    for index in 0..total {
        let input = if index == 0 { 1.0 } else { 0.0 };
        output.push(chain.process(input, input, 48_000.0)[0]);
    }
    let peak = taps
        .iter()
        .fold(0.0_f64, |peak, value| peak.max(value.abs()));
    for (index, tap) in taps.iter().enumerate() {
        let measured = output[CONV_LATENCY + index];
        assert!(
            (measured - tap).abs() < peak * 1e-9,
            "tap {index}: {measured} vs {tap}"
        );
    }
}

#[test]
fn convolution_latency_and_crossfade_are_exact() {
    let patch = |mix: f64| {
        let mut fx = SynthFx::new(SynthFxType::Conv);
        fx.set_param("size", 0.1);
        fx.set_param("damp", 0.0);
        fx.set_param("predelay", 0.0);
        fx.set_param("width", 1.0);
        fx.set_param("mix", mix);
        fx
    };
    let run = |mix: f64| {
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[patch(mix)]);
        assert_eq!(chain.latency_samples(), CONV_LATENCY);
        let mut seed = 0x1122_3344_5566_7788u64;
        let mut output = Vec::new();
        for _ in 0..4_096 {
            let input = noise(&mut seed) * 0.2;
            output.push(chain.process(input, input, 48_000.0));
        }
        output
    };
    let dry = run(0.0);
    let wet = run(1.0);
    let mixed = run(0.5);
    // Drive the same input again and check the dry path sample by sample.
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[patch(0.0)]);
    let mut seed = 0x1122_3344_5566_7788u64;
    let mut inputs = Vec::new();
    for index in 0..4_096 {
        let input = noise(&mut seed) * 0.2;
        inputs.push(input);
        let [left, _] = chain.process(input, input, 48_000.0);
        assert_eq!(left, dry[index][0]);
        if index < CONV_LATENCY {
            assert_eq!(left, 0.0);
        } else {
            assert_eq!(left, inputs[index - CONV_LATENCY]);
        }
    }
    for index in 0..4_096 {
        for channel in 0..2 {
            let expected = dry[index][channel] + (wet[index][channel] - dry[index][channel]) * 0.5;
            assert!((mixed[index][channel] - expected).abs() < 1e-12);
        }
    }
}

#[test]
fn convolution_tail_decays_below_minus_120_db() {
    for (preset, conv_preset) in CONV_PRESETS.iter().enumerate() {
        let mut chain = SynthFxChain::new();
        let mut fx = SynthFx::new(SynthFxType::Conv);
        fx.set_param("size", (preset as f64 + 0.5) / CONV_IR_PRESETS as f64);
        fx.set_param("damp", 0.0);
        fx.set_param("predelay", 0.0);
        fx.set_param("mix", 1.0);
        chain.sync_from_patch(&[fx]);
        let samples = (conv_preset.seconds * 48_000.0) as usize;
        let total = CONV_LATENCY + samples + 2 * CONV_BLOCK;
        let mut peak = 0.0_f64;
        let mut output = Vec::with_capacity(total);
        for index in 0..total {
            let input = if index == 0 { 1.0 } else { 0.0 };
            let [left, right] = chain.process(input, input, 48_000.0);
            peak = peak.max(left.abs()).max(right.abs());
            output.push(left);
        }
        let documented = CONV_LATENCY + samples;
        let tail = output[documented..]
            .iter()
            .fold(0.0_f64, |peak, value| peak.max(value.abs()));
        let level = 20.0 * (tail / peak).max(1e-30).log10();
        assert!(level < -120.0, "preset {preset} tail {level:.1} dB");
        assert!(peak < 2.0 && peak > 1e-3, "preset {preset} peak {peak}");
    }
}

/// Building a rack must not build an IR bank for a module the rack does not
/// hold: an unprepared slot keeps the one-block placeholder IR.
#[test]
fn a_rack_without_a_convolution_module_generates_no_ir() {
    let generated = |slot: &ConvState| slot.taps(0).len() > CONV_BLOCK;
    let mut chain = SynthFxChain::new();
    for slot in 0..INITIAL_TIME_INSTANCES {
        assert!(
            !generated(&chain.convolutions[slot]),
            "slot {slot} built an IR bank at construction"
        );
    }
    chain.sync_from_patch(&[SynthFx::new(SynthFxType::Reverb)]);
    for slot in 0..INITIAL_TIME_INSTANCES {
        assert!(
            !generated(&chain.convolutions[slot]),
            "slot {slot} built an IR bank for a rack with no convolution"
        );
    }
    // The module that does exist prepares its own slot and nothing else.
    chain.sync_from_patch(&[SynthFx::new(SynthFxType::Conv)]);
    assert!(generated(&chain.convolutions[0]));
    assert!(!generated(&chain.convolutions[1]));
}

#[test]
fn convolution_reset_is_idempotent() {
    let mut state = ConvState::new();
    state.prepare(48_000.0, 1);
    let mut seed = 0x0102_0304_0506_0708u64;
    for _ in 0..4_096 {
        let input = noise(&mut seed) * 0.2;
        state.process(input, input, &conv_values(0.5), 48_000.0);
    }
    state.reset();
    state.reset();
    let mut first = Vec::new();
    for index in 0..2_048 {
        let input = if index == 0 { 1.0 } else { 0.0 };
        first.push(state.process(input, input, &conv_values(0.5), 48_000.0));
    }
    let mut fresh = ConvState::new();
    fresh.prepare(48_000.0, 1);
    let mut second = Vec::new();
    for index in 0..2_048 {
        let input = if index == 0 { 1.0 } else { 0.0 };
        second.push(fresh.process(input, input, &conv_values(0.5), 48_000.0));
    }
    assert_eq!(first, second);

    let mut hyperd = HyperDState::new();
    hyperd.reset();
    hyperd.reset();
    let mut silence = [0.0; MAX_FX_VALUE_SLOTS];
    silence[4] = 1.0;
    for _ in 0..1_024 {
        let (left, right) = hyperd.process(0.0, 0.0, &silence, 48_000.0);
        assert_eq!((left, right), (0.0, 0.0));
    }
}

fn conv_values(size: f64) -> [f64; MAX_FX_VALUE_SLOTS] {
    let mut values = [0.0; MAX_FX_VALUE_SLOTS];
    values[0] = size;
    values[1] = 0.0;
    values[2] = 0.0;
    values[3] = 1.0;
    values[4] = 1.0;
    values
}

#[test]
fn convolution_and_hyperd_wet_levels_are_sane() {
    for fx_type in [SynthFxType::Conv, SynthFxType::HyperD] {
        let output = render_left(
            {
                let mut fx = SynthFx::new(fx_type);
                fx.set_param("mix", 1.0);
                fx
            },
            48_000,
            true,
        );
        let gain = 20.0 * (rms_of(&output) / (0.15 / 3.0_f64.sqrt())).log10();
        assert!(gain.abs() < 6.0, "{fx_type:?} wet gain {gain:+.2} dB");
    }
}

#[test]
fn bode_swap_ab_exchanges_the_two_sidebands() {
    let render = |swap: f64| {
        let mut fx = SynthFx::new(SynthFxType::Bode);
        fx.set_param("shift", 50.0);
        fx.set_param("range", 100.0);
        fx.set_param("output_mix", 0.0);
        fx.set_param("output_width", 100.0);
        fx.set_param("mix", 1.0);
        fx.set_param("level_out", 1.0);
        fx.set_param("swap_ab", swap);
        let mut chain = SynthFxChain::new();
        chain.sync_from_patch(&[fx]);
        let mut out = Vec::with_capacity(4_096);
        for index in 0..4_096 {
            let input = (TAU * 1_000.0 * index as f64 / 48_000.0).sin() * 0.3;
            out.push(chain.process(input, input, 48_000.0));
        }
        out
    };
    let plain = render(0.0);
    let swapped = render(1.0);
    // Past the Hilbert latency, swapping A/B makes L the old R exactly.
    for index in 200..plain.len() {
        assert!((swapped[index][0] - plain[index][1]).abs() < 1e-6);
        assert!((swapped[index][1] - plain[index][0]).abs() < 1e-6);
    }
    assert!(rms_of(&plain.iter().map(|frame| frame[0]).collect::<Vec<_>>()) > 1e-3);
}

#[test]
fn multiband_global_makeup_is_a_static_gain() {
    let mut fx = SynthFx::new(SynthFxType::Multiband);
    fx.set_param("band_count", 1.0);
    fx.set_param("mix", 1.0);
    fx.set_param("level_out", 1.0);
    fx.set_param("makeup", 0.0);
    let (dry_left, dry_right) = sine_rms(fx.clone(), 440.0, 0.5);
    fx.set_param("makeup", 6.0);
    let (wet_left, wet_right) = sine_rms(fx, 440.0, 0.5);
    let gain = 20.0 * (wet_left / dry_left).log10();
    assert!((gain - 6.0).abs() < 0.2, "makeup gain {gain:+.2} dB");
    assert!((wet_left - wet_right).abs() < 1e-9);
    assert!(dry_right.is_finite());
}

/// Pearson correlation of two equal-length windows.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    let mean_a = a.iter().sum::<f64>() / n;
    let mean_b = b.iter().sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut variance_a = 0.0;
    let mut variance_b = 0.0;
    for (x, y) in a.iter().zip(b) {
        covariance += (x - mean_a) * (y - mean_b);
        variance_a += (x - mean_a) * (x - mean_a);
        variance_b += (y - mean_b) * (y - mean_b);
    }
    covariance / (variance_a * variance_b).sqrt()
}

fn reverb_fx(size: f64, damp: f64) -> SynthFx {
    let mut fx = SynthFx::new(SynthFxType::Reverb);
    fx.set_param("size", size);
    fx.set_param("damp", damp);
    fx.set_param("predelay", 0.0);
    fx.set_param("width", 1.0);
    fx.set_param("mix", 1.0);
    fx
}

fn reverb_chain(size: f64, damp: f64) -> SynthFxChain {
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[reverb_fx(size, damp)]);
    chain
}

/// Band-limited noise burst -> reverb; returns the fitted -60 dB decay time.
fn noise_band_rt60(size: f64, band: usize) -> f64 {
    let mut chain = reverb_chain(size, 0.0);
    let burst = 24_000usize;
    let total = burst + 48_000 * 8;
    let low = ReverbLowpass::lowpass(REVERB_BAND_HZ[0], 48_000.0);
    let high = ReverbLowpass::highpass(REVERB_BAND_HZ[1], 48_000.0);
    let mut states = [[0.0f64; 2]; 4];
    let mut seed = 0x51ed_270b_9a3c_1f77u64;
    let mut tail = Vec::with_capacity(total - burst);
    for frame in 0..total {
        let mut input = noise(&mut seed);
        for stage in 0..2 {
            input = match band {
                0 => low.process(&mut states[stage], input),
                1 => {
                    let value = low.process(&mut states[stage], input);
                    high.process(&mut states[stage + 2], value)
                }
                _ => high.process(&mut states[stage + 2], input),
            };
        }
        let input = if frame < burst { input * 0.35 } else { 0.0 };
        let [left, _] = chain.process(input, input, 48_000.0);
        if frame >= burst {
            tail.push(left);
        }
    }
    let window = 2_400;
    let hop = 1_200;
    let mut points = Vec::new();
    let mut start = 0;
    while start + window <= tail.len() {
        let rms = rms_of(&tail[start..start + window]);
        points.push((start as f64 / 48_000.0, 20.0 * rms.max(1e-15).log10()));
        start += hop;
    }
    let peak = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let fitted: Vec<_> = points
        .iter()
        .copied()
        .filter(|point| point.1 <= peak - 5.0 && point.1 >= peak - 25.0)
        .collect();
    assert!(fitted.len() >= 3, "band {band} decay is too short to fit");
    let count = fitted.len() as f64;
    let mean_time = fitted.iter().map(|point| point.0).sum::<f64>() / count;
    let mean_db = fitted.iter().map(|point| point.1).sum::<f64>() / count;
    let slope = fitted
        .iter()
        .map(|point| (point.0 - mean_time) * (point.1 - mean_db))
        .sum::<f64>()
        / fitted
            .iter()
            .map(|point| (point.0 - mean_time).powi(2))
            .sum::<f64>();
    -60.0 / slope
}

#[test]
fn reverb_size_maps_to_the_pinned_rt60_table() {
    let table = [
        (0.0, 0.585),
        (0.1, 0.772),
        (0.2, 1.019),
        (0.3, 1.344),
        (0.4, 1.773),
        (0.5, 2.340),
        (0.6, 3.088),
        (0.7, 4.074),
        (0.8, 5.376),
        (0.9, 7.094),
        (1.0, 9.360),
    ];
    for (size, seconds) in table {
        assert!(
            (reverb_rt60_mid(size) - seconds).abs() < 0.01,
            "size {size}: {} vs {seconds}",
            reverb_rt60_mid(size)
        );
    }
    for pair in table.windows(2) {
        assert!(reverb_rt60_mid(pair[1].0) > reverb_rt60_mid(pair[0].0));
    }
    // No band may reach unity: the loop always loses energy.
    let delay = 0.037;
    for size in [0.0, 0.5, 1.0] {
        let mid = reverb_rt60_mid(size);
        assert!(reverb_band_gain(mid * REVERB_LOW_RATIO, delay) < 1.0);
        assert!(reverb_band_gain(mid, delay) < 1.0);
        assert!(reverb_band_gain(mid * REVERB_HIGH_RATIO, delay) < 1.0);
    }
}

#[test]
fn reverb_rt60_is_pinned_per_band_at_three_sizes() {
    let sizes = [0.25f64, 0.5, 0.6];
    for size in sizes {
        let mid_target = reverb_rt60_mid(size);
        let low = noise_band_rt60(size, 0);
        let mid = noise_band_rt60(size, 1);
        let high = noise_band_rt60(size, 2);
        assert!(
            (mid - mid_target).abs() < mid_target * 0.12,
            "size {size} mid {mid:.3} vs {mid_target:.3}"
        );
        assert!(
            (low - mid_target * REVERB_LOW_RATIO).abs() < mid_target * 0.18,
            "size {size} low {low:.3}"
        );
        assert!(
            (high - mid_target * REVERB_HIGH_RATIO).abs() < mid_target * 0.25,
            "size {size} high {high:.3}"
        );
        assert!(low > mid * 0.95, "size {size}: low {low:.3} mid {mid:.3}");
        assert!(
            high < mid * 0.85,
            "size {size}: high {high:.3} mid {mid:.3}"
        );
    }
}

#[test]
fn reverb_tail_is_stereo_decorrelated() {
    let mut chain = reverb_chain(0.6, 0.4);
    let mut seed = 0xabcd_ef01_2345_6789u64;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for frame in 0..(48_000 * 4) {
        let input = if frame < 24_000 {
            noise(&mut seed) * 0.25
        } else {
            0.0
        };
        let [l, r] = chain.process(input, input, 48_000.0);
        if frame >= 48_000 {
            left.push(l);
            right.push(r);
        }
    }
    let correlation = correlation(&left, &right);
    assert!(
        correlation.abs() < 0.10,
        "tail correlation {correlation:.3}"
    );
    let left_rms = rms_of(&left);
    let right_rms = rms_of(&right);
    let balance = 20.0 * (left_rms / right_rms).log10();
    assert!(balance.abs() < 1.5, "channel balance {balance:+.2} dB");
    assert!(left_rms > 1e-4);
}

#[test]
fn reverb_damping_filter_does_not_detune_the_tail() {
    // The damping stage is a static one-pole; a real filter cannot shift a
    // tone, but the test pins it against a future modulated design.
    let damping = |damp: f64, input: f64, state: &mut f64| {
        let pole = damp * (-TAU * REVERB_DAMP_HZ / 48_000.0).exp();
        let output = input * (1.0 - pole) + *state * pole;
        *state = output;
        output
    };
    for damp in [0.2, 0.5, 0.9] {
        let mut state = 0.0;
        let mut crossings = 0;
        let mut previous = 0.0;
        let mut count = 0;
        for frame in 0..48_000 {
            let input = (TAU * 1_000.0 * frame as f64 / 48_000.0).sin();
            let output = damping(damp, input, &mut state);
            if frame > 4_800 {
                if previous <= 0.0 && output > 0.0 {
                    crossings += 1;
                }
                previous = output;
                count += 1;
            }
        }
        let frequency = crossings as f64 * 48_000.0 / count as f64;
        assert!(
            (frequency - 1_000.0).abs() < 0.5,
            "damp {damp}: {frequency:.2} Hz"
        );
    }
    // The tank delays are fixed integers, so the tail cannot be detuned by
    // a time-varying read.
    let mut state = ReverbState::new();
    let lengths = state.lengths;
    let mut values = [0.0; MAX_FX_VALUE_SLOTS];
    values[0] = 0.6;
    values[1] = 0.9;
    values[4] = 1.0;
    for _ in 0..4_800 {
        state.process(0.25, -0.25, &values, 48_000.0);
    }
    assert_eq!(state.lengths, lengths);
}

#[test]
fn reverb_predelay_is_sample_exact() {
    let onset = |predelay: f64| {
        let mut chain = SynthFxChain::new();
        let mut fx = reverb_fx(0.4, 0.0);
        fx.set_param("predelay", predelay);
        chain.sync_from_patch(&[fx]);
        for frame in 0..48_000 {
            let input = if frame == 0 { 1.0 } else { 0.0 };
            let [left, _] = chain.process(input, input, 48_000.0);
            if left.abs() > 1e-12 {
                return frame;
            }
        }
        usize::MAX
    };
    let short = onset(0.05);
    let long = onset(0.15);
    assert_eq!(long - short, 4_800);
    assert!(short < 48_000 && long < 48_000);
}

#[test]
fn reverb_wet_zero_is_a_bit_exact_bypass() {
    let mut fx = reverb_fx(0.35, 0.3377);
    fx.set_param("mix", 0.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    let mut seed = 0xfeed_face_1234_5678u64;
    for _ in 0..48_000 {
        let input = noise(&mut seed) * 0.5;
        assert_eq!(chain.process(input, -input, 48_000.0), [input, -input]);
    }
}

#[test]
fn reverb_reports_zero_latency_and_passes_the_dry_path() {
    let fx = reverb_fx(0.5, 0.4);
    assert_eq!(fx_rack_latency(std::slice::from_ref(&fx), 48_000.0), 0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[fx]);
    assert_eq!(chain.latency_samples(), 0);
    // A wet-0 module is a bit-exact bypass: the tank is not on the dry path.
    let mut bypass = reverb_fx(0.5, 0.4);
    bypass.set_param("mix", 0.0);
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&[bypass]);
    assert_eq!(chain.process(1.0, -0.5, 48_000.0), [1.0, -0.5]);
}

#[test]
fn reverb_long_burst_decays_at_the_extremes() {
    let mut chain = SynthFxChain::new();
    let mut fx = reverb_fx(1.0, 1.0);
    fx.set_param("predelay", 0.2);
    chain.sync_from_patch(&[fx]);
    let mut seed = 0x0bad_c0de_dead_beefu64;
    let mut peak = 0.0f64;
    let mut windows = Vec::new();
    let mut second_rms = 0.0f64;
    let mut second_count = 0;
    let mut last = 0.0f64;
    for frame in 0..(31 * 48_000) {
        let input = if frame < 48_000 {
            noise(&mut seed) * 0.5
        } else {
            0.0
        };
        let [left, right] = chain.process(input, input, 48_000.0);
        assert!(left.is_finite() && right.is_finite());
        peak = peak.max(left.abs()).max(right.abs());
        if frame >= 48_000 {
            second_rms += left * left;
            second_count += 1;
            if frame >= 30 * 48_000 {
                last = last.max(left.abs());
            }
            if second_count == 48_000 {
                windows.push((second_rms / 48_000.0).sqrt());
                second_rms = 0.0;
                second_count = 0;
            }
        }
    }
    assert!(peak < 2.0, "peak {peak}");
    assert!(last < 1e-6, "tail still at {last:.3e} after 30 s");
    for pair in windows.windows(2) {
        assert!(
            pair[1] <= pair[0] * 1.25 + 1e-12,
            "decay stalled: {:.3e} -> {:.3e}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn reverb_has_no_dc_buildup() {
    let mut chain = reverb_chain(1.0, 1.0);
    let mut last = 0.0f64;
    for frame in 0..(31 * 48_000) {
        let input = if frame < 48_000 { 0.5 } else { 0.0 };
        let [left, _] = chain.process(input, input, 48_000.0);
        if frame >= 30 * 48_000 {
            last = last.max(left.abs());
        }
    }
    assert!(last < 1e-6, "DC residue {last:.3e} after 30 s");
}

#[test]
fn reverb_buffer_budget_is_pinned() {
    let state = ReverbState::new();
    assert_eq!(state.allocated_bytes(), 1_223_936);
    let chain = SynthFxChain::new();
    let total: usize = chain.reverbs.iter().map(ReverbState::allocated_bytes).sum();
    assert_eq!(total, 2 * 1_223_936);
    // Processing never resizes the buffers.
    let mut chain = reverb_chain(0.6, 0.4);
    for _ in 0..4_800 {
        chain.process(0.1, -0.1, 48_000.0);
    }
    assert_eq!(chain.reverbs[0].allocated_bytes(), 1_223_936);
    // Every rate snaps to a prime that still fits the 96 kHz capacity.
    let mut state = ReverbState::new();
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        state.prepare(rate);
        for line in 0..REVERB_TOTAL_LINES {
            assert!(is_prime(state.lengths[line]), "rate {rate} line {line}");
            assert!(state.lengths[line] <= state.lines[line].len());
        }
    }
}

fn reset_probe_fx(fx_type: SynthFxType) -> SynthFx {
    let mut fx = SynthFx::new(fx_type);
    // Low normalized time controls and feedback expose stale delay histories.
    match fx_type {
        SynthFxType::Delay => {
            fx.set_param("time", 0.005);
            fx.set_param("feedback", 0.6);
            fx.set_param("mix", 0.7);
        }
        SynthFxType::Reverb => {
            fx.set_param("predelay", 0.0);
            fx.set_param("mix", 0.7);
        }
        SynthFxType::Bode => {
            fx.set_param("shift", 40.0);
            fx.set_param("feedback", 0.5);
            fx.set_param("delay", 0.005);
        }
        SynthFxType::Eq3 => {
            fx.set_param("low", 6.0);
            fx.set_param("high", -3.0);
        }
        SynthFxType::Multiband => {
            fx.set_param("lookahead", 2.0);
            fx.set_param("band1_threshold", -18.0);
            fx.set_param("band1_ratio", 4.0);
        }
        _ => {}
    }
    fx
}

#[test]
fn reset_restores_fresh_output_for_every_effect_without_heap_operations() {
    for fx_type in SynthFxType::ALL {
        let fx = reset_probe_fx(fx_type);
        let mut used = SynthFxChain::new();
        let mut fresh = SynthFxChain::new();
        let rate = 44_100.0;
        used.sample_rate = rate;
        fresh.sample_rate = rate;
        used.sync_from_patch(std::slice::from_ref(&fx));
        fresh.sync_from_patch(std::slice::from_ref(&fx));
        for frame in 0..4096 {
            let input = 0.5 * (frame as f64 * 0.13).sin() + if frame == 0 { 0.4 } else { 0.0 };
            used.process(input, -input * 0.7, rate);
        }
        // Exercise base restoration as well as the history reset. The first
        // process call applies these live controls to any prepared processors.
        used.set_module_param(0, "mix", 0.1);
        used.set_module_param(0, "limiter", 0.0);
        used.set_module_param(0, "phase_mode", 2.0);
        used.set_module_param(0, "lookahead", 8.0);
        used.process(0.3, -0.2, rate);
        let operations = crate::test_alloc::operations(|| used.reset());
        assert_eq!(
            operations, 0,
            "reset allocated or freed memory for {fx_type:?}"
        );
        assert!(!used.tail_active(), "pending tail for {fx_type:?}");
        assert_eq!(
            used.latency_samples(),
            fresh.latency_samples(),
            "{fx_type:?}"
        );
        assert_eq!(used.tail_samples, fresh.tail_samples, "{fx_type:?}");
        assert_eq!(
            used.modules[0].values, fresh.modules[0].values,
            "{fx_type:?}"
        );
        for frame in 0..8192 {
            let input = if frame < 256 {
                0.2 * (frame as f64 * 0.19).cos()
            } else {
                0.0
            };
            let actual = used.process(input, input * 0.3, rate);
            let expected = fresh.process(input, input * 0.3, rate);
            assert_eq!(actual, expected, "{fx_type:?} frame {frame}");
        }
    }
}

#[test]
fn reset_discards_an_observed_tail_and_is_idempotent() {
    let mut chain = SynthFxChain::new();
    let fx = [
        reset_probe_fx(SynthFxType::Delay),
        reset_probe_fx(SynthFxType::Reverb),
        reset_probe_fx(SynthFxType::Conv),
    ];
    chain.sync_from_patch(&fx);
    let mut tail_peak = 0.0f64;
    for frame in 0..12_000 {
        let input = if frame == 0 { 1.0 } else { 0.0 };
        let output = chain.process(input, input, 48_000.0);
        if frame >= 4096 {
            tail_peak = tail_peak.max(output[0].abs()).max(output[1].abs());
        }
    }
    assert!(tail_peak > 1e-8, "fixture must produce a real pending tail");
    assert!(chain.tail_active());
    assert_eq!(
        crate::test_alloc::operations(|| {
            chain.reset();
            chain.reset();
        }),
        0
    );
    assert!(!chain.tail_active());
    for frame in 0..12_000 {
        assert_eq!(
            chain.process(0.0, 0.0, 48_000.0),
            [0.0, 0.0],
            "frame {frame}"
        );
    }
}

#[test]
fn reset_keeps_prepared_storage_and_convolution_ir() {
    let mut chain = SynthFxChain::new();
    let fx = [
        reset_probe_fx(SynthFxType::Conv),
        reset_probe_fx(SynthFxType::Delay),
        reset_probe_fx(SynthFxType::Multiband),
        reset_probe_fx(SynthFxType::Compressor),
    ];
    chain.sample_rate = 96_000.0;
    chain.sync_from_patch(&fx);
    for frame in 0..512 {
        chain.process(if frame == 0 { 1.0 } else { 0.0 }, 0.0, 96_000.0);
    }
    let convolution = &chain.convolutions[0];
    let ir_taps = convolution.left.taps.to_vec();
    let ir_spectra: Vec<_> = convolution
        .left
        .spectra
        .iter()
        .map(|value| (value.re, value.im))
        .collect();
    let ir_identity = (
        convolution.ir_rate,
        convolution.ir_preset,
        convolution.left.ir_samples,
    );
    let storage = (
        convolution.left.taps.as_ptr(),
        convolution.left.spectra.as_ptr(),
        convolution.left.history.as_ptr(),
        chain.delays[0].left.as_ptr(),
        chain.delays[0].taps.as_deref().unwrap().as_ptr(),
        chain.multibands[2].compressor.as_deref().unwrap() as *const MultibandCompressor,
        chain.compressors[3].compander.as_deref().unwrap() as *const Compander,
    );
    assert_eq!(crate::test_alloc::operations(|| chain.reset()), 0);
    let convolution = &chain.convolutions[0];
    assert_eq!(
        storage,
        (
            convolution.left.taps.as_ptr(),
            convolution.left.spectra.as_ptr(),
            convolution.left.history.as_ptr(),
            chain.delays[0].left.as_ptr(),
            chain.delays[0].taps.as_deref().unwrap().as_ptr(),
            chain.multibands[2].compressor.as_deref().unwrap() as *const MultibandCompressor,
            chain.compressors[3].compander.as_deref().unwrap() as *const Compander,
        )
    );
    assert_eq!(
        ir_identity,
        (
            convolution.ir_rate,
            convolution.ir_preset,
            convolution.left.ir_samples
        )
    );
    assert_eq!(convolution.left.taps.as_ref(), ir_taps.as_slice());
    for (actual, expected) in convolution.left.spectra.iter().zip(ir_spectra) {
        assert_eq!((actual.re, actual.im), expected);
    }
    assert_eq!(chain.sample_rate, 96_000.0);
}

#[test]
fn three_time_effects_match_three_independent_racks_without_runtime_allocation() {
    for fx_type in [
        SynthFxType::Delay,
        SynthFxType::Reverb,
        SynthFxType::Chorus,
        SynthFxType::Flanger,
        SynthFxType::Bode,
        SynthFxType::Conv,
    ] {
        let modules: Vec<_> = (0..3)
            .map(|index| {
                let mut fx = reset_probe_fx(fx_type);
                fx.set_param("mix", 0.4 + index as f64 * 0.2);
                fx.set_param("time", 0.005 + index as f64 * 0.002);
                fx.set_param("rate", 0.2 + index as f64 * 0.3);
                fx.set_param("size", 0.2 + index as f64 * 0.15);
                fx
            })
            .collect();
        let mut combined = SynthFxChain::new();
        combined.sync_from_patch(&modules);
        assert_eq!(
            combined.modules[..3]
                .iter()
                .map(|module| module.time_slot)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let mut independent: Vec<_> = modules
            .iter()
            .map(|fx| {
                let mut chain = SynthFxChain::new();
                chain.sync_from_patch(std::slice::from_ref(fx));
                chain
            })
            .collect();
        // The delay time is a normalized logarithmic control. Cover all
        // three wet arrivals as well as their fixed dry-path latencies.
        let frames = if fx_type == SynthFxType::Delay {
            combined.latency_samples()
                + modules
                    .iter()
                    .map(|fx| {
                        delay_time_samples(fx.param("time").unwrap(), 48_000.0).ceil() as usize
                    })
                    .sum::<usize>()
                + 1024
        } else {
            6144
        };
        let mut output_peak = 0.0f64;
        let mut late_peak = 0.0f64;
        let operations = crate::test_alloc::operations(|| {
            for frame in 0..frames {
                let left = if frame < 512 {
                    0.2 * (frame as f64 * 0.17).sin()
                } else {
                    0.0
                };
                let right = if frame == 1777 { 0.4 } else { left * -0.7 };
                let actual = combined.process(left, right, 48_000.0);
                let peak = actual[0].abs().max(actual[1].abs());
                output_peak = output_peak.max(peak);
                if frame >= frames - 1024 {
                    late_peak = late_peak.max(peak);
                }
                let mut expected = [left, right];
                for chain in &mut independent {
                    expected = chain.process(expected[0], expected[1], 48_000.0);
                }
                assert_eq!(actual, expected, "{fx_type:?} frame {frame}");
            }
        });
        assert_eq!(operations, 0, "runtime heap operations for {fx_type:?}");
        assert!(output_peak > 1e-8, "{fx_type:?} probe must produce audio");
        if fx_type == SynthFxType::Delay {
            assert!(
                late_peak > 1e-8,
                "delay probe must reach the final wet arrival"
            );
        }
        assert_eq!(crate::test_alloc::operations(|| combined.reset()), 0);
        for _ in 0..512 {
            assert_eq!(combined.process(0.0, 0.0, 48_000.0), [0.0, 0.0]);
        }
    }
}

#[test]
fn every_rack_slot_can_own_an_independent_time_effect() {
    let modules = vec![SynthFx::new(SynthFxType::Flanger); MAX_SYNTH_FX];
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&modules);
    assert_eq!(chain.flangers.len(), MAX_SYNTH_FX);
    for (slot, module) in chain.modules.iter().enumerate() {
        assert_eq!(module.time_slot, slot);
    }
    for (index, first) in chain.flangers.iter().enumerate() {
        for second in &chain.flangers[index + 1..] {
            assert_ne!(first.left.as_ptr(), second.left.as_ptr());
            assert_ne!(first.right.as_ptr(), second.right.as_ptr());
        }
    }
    assert_eq!(
        crate::test_alloc::operations(|| {
            for _ in 0..64 {
                chain.process(0.1, -0.1, 48_000.0);
            }
            chain.reset();
        }),
        0
    );
}

#[test]
fn time_state_capacity_is_retained_across_topology_changes() {
    let modules = vec![reset_probe_fx(SynthFxType::Reverb); 3];
    let mut chain = SynthFxChain::new();
    chain.sync_from_patch(&modules);
    for frame in 0..4096 {
        chain.process(if frame == 0 { 1.0 } else { 0.0 }, 0.3, 48_000.0);
    }
    let pointers: Vec<_> = chain
        .reverbs
        .iter()
        .map(|state| state.lines[0].as_ptr())
        .collect();
    let capacity = chain.reverbs.capacity();
    let mut mixed = modules.clone();
    mixed[1] = reset_probe_fx(SynthFxType::Chorus);
    let allocations = crate::test_alloc::operations(|| {
        chain.sync_from_patch(&modules[..1]);
        chain.sync_from_patch(&mixed);
        chain.sync_from_patch(&modules);
    });
    assert_eq!(allocations, 0);
    assert_eq!(chain.reverbs.capacity(), capacity);
    assert_eq!(chain.reverbs.len(), 3);
    assert_eq!(
        chain
            .reverbs
            .iter()
            .map(|state| state.lines[0].as_ptr())
            .collect::<Vec<_>>(),
        pointers
    );
    assert!(!chain.tail_active());
    for _ in 0..4096 {
        assert_eq!(chain.process(0.0, 0.0, 48_000.0), [0.0, 0.0]);
    }
    let mut fresh = SynthFxChain::new();
    fresh.sync_from_patch(&modules);
    for frame in 0..1024 {
        let input = if frame < 64 { 0.2 } else { 0.0 };
        assert_eq!(
            chain.process(input, -input, 48_000.0),
            fresh.process(input, -input, 48_000.0)
        );
    }
}

#[test]
fn parameter_only_sync_keeps_time_histories_without_heap_operations() {
    let mut modules = vec![reset_probe_fx(SynthFxType::Delay); 3];
    let mut edited = SynthFxChain::new();
    let mut continuous = SynthFxChain::new();
    edited.sync_from_patch(&modules);
    continuous.sync_from_patch(&modules);
    // Change the control just before the first echo can traverse the full
    // rack. A fixed short window could finish before even the dry latency.
    let warmup = edited.latency_samples()
        + delay_time_samples(modules[0].param("time").unwrap(), 48_000.0).ceil() as usize
        - 64;
    for frame in 0..warmup {
        let input = if frame < 128 { 0.4 } else { 0.0 };
        edited.process(input, input, 48_000.0);
        continuous.process(input, input, 48_000.0);
    }
    modules[2].set_param("mix", 0.9);
    continuous.set_module_param(2, "mix", 0.9);
    assert_eq!(
        crate::test_alloc::operations(|| edited.sync_from_patch(&modules)),
        0
    );
    assert!(edited.tail_active());
    let mut peak = 0.0f64;
    for frame in 0..4096 {
        let actual = edited.process(0.0, 0.0, 48_000.0);
        let expected = continuous.process(0.0, 0.0, 48_000.0);
        peak = peak.max(actual[0].abs());
        assert_eq!(actual, expected, "frame {frame}");
    }
    assert!(
        peak > 1e-8,
        "parameter editing should preserve an audible tail"
    );
}
