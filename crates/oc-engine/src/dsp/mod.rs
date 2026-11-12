//! Built-in DSP effects — pure Rust, zero dependencies.
//!
//! All effects implement the `AudioEffect` trait and process audio
//! in-place as stereo interleaved f32 buffers.
//!
//! These are the "factory" effects that ship with OpenCompose.
//! Users can also load VST3/CLAP plugins via the plugin bridge.

pub use oc_dsp::{
    biquad, breath_split, chorus, compressor, crossover, deesser, delay, detector, distortion,
    doubler, ducked_send, dynamic_eq, eq, exciter, fet, flanger, gate, iron, leveler, limiter,
    multiband, opto, passive_eq, phaser, plosive, resonance, reverb, reverb_fdn, saturator, schema,
    stereo_width, tuner,
};

pub mod control_source;
pub mod schema_bridge;

pub use schema_bridge::{
    ResolvedEffect, device_for_effect, legacy_warnings, resolve_effect, validate_param,
    validate_static_param,
};

use std::collections::HashMap;

/// All built-in effects process stereo interleaved f32 in-place.
pub use oc_audio::AudioEffect;

/// Exact linear utility gain; zero is a silent routing sink.
struct LinearGain(f64);

#[test]
fn utility_gain_is_exact_and_zero_silences() {
    let mut gain = LinearGain(0.5);
    let mut samples = [1.0, -1.0, 0.25, -0.25];
    gain.process(&mut samples, 48000);
    assert_eq!(samples, [0.5, -0.5, 0.125, -0.125]);
    gain.set_param("factor", 0.0);
    gain.process(&mut samples, 48000);
    assert_eq!(samples, [0.0; 4]);
}

impl AudioEffect for LinearGain {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        oc_dsp::simd::apply_uniform_gain(buffer, self.0 as f32);
    }
    fn set_param(&mut self, name: &str, value: f64) {
        if name == "factor" && value.is_finite() && value >= 0.0 {
            self.0 = value;
        }
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        (name == "factor").then_some(self.0)
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "gain"
    }
}

#[test]
fn fractional_mix_keeps_static_percentage_and_automation_contexts_distinct() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    for effect in [E::Compression, E::Multiband] {
        let device = device_for_effect(&effect).unwrap();
        assert_eq!(
            validate_static_param(device, "mix", &P::Percentage(40.0)),
            Ok(0.4)
        );
        assert_eq!(
            validate_static_param(device, "mix", &P::Percentage(0.4)),
            Ok(0.004)
        );
        assert_eq!(
            crate::automation::point_value("mix", &P::Percentage(0.4), Some(device)),
            Ok(0.4)
        );
        assert!(validate_static_param(device, "mix", &P::Percentage(101.0)).is_err());
        for (physical, percentage) in [(0.4, 40.0), (0.004, 0.4), (1.0, 100.0)] {
            let mut bare =
                try_create_effect(&effect, &[("mix".into(), P::Number(physical))], 48_000).unwrap();
            let mut typed = try_create_effect(
                &effect,
                &[("mix".into(), P::Percentage(percentage))],
                48_000,
            )
            .unwrap();
            assert_eq!(bare.get_param("mix"), Some(physical));
            assert_eq!(typed.get_param("mix"), Some(physical));
            let input: Vec<f32> = (0..8192)
                .map(|sample| (sample as f32 * 0.027).sin() * 0.8)
                .collect();
            let mut a = input.clone();
            let mut b = input;
            bare.process(&mut a, 48_000);
            typed.process(&mut b, 48_000);
            assert_eq!(a, b, "equivalent static values must render the same audio");
        }
        assert_eq!(
            try_create_effect(&effect, &[], 48_000)
                .unwrap()
                .get_param("mix"),
            Some(1.0)
        );
    }
}

#[test]
fn typed_production_controls_reach_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let compressor = try_create_effect(
        &E::Compression,
        &[
            ("ratio".into(), P::String("2:1".into())),
            ("attack".into(), P::Time(0.02)),
            ("release".into(), P::Time(0.15)),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(compressor.get_param("ratio"), Some(2.0));
    assert_eq!(compressor.get_param("attack"), Some(20.0));
    assert_eq!(compressor.get_param("release"), Some(150.0));
    let reverb = try_create_effect(
        &E::Reverb,
        &[
            ("decay".into(), P::Time(2.4)),
            ("mix".into(), P::Percentage(25.0)),
            ("predelay".into(), P::Time(0.02)),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(reverb.get_param("decay_seconds"), Some(2.4));
    assert_eq!(reverb.get_param("mix"), Some(0.25));
    assert_eq!(reverb.get_param("predelay"), Some(20.0));
    assert!(try_create_effect(&E::Limiter, &[("ceiling".into(), P::Db(500.0))], 48000).is_err());
}

#[test]
fn compression_controls_reach_the_dsp() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let c = try_create_effect(
        &E::Compression,
        &[
            ("range".into(), P::Db(6.0)),
            ("stereo_link".into(), P::Percentage(50.0)),
            ("hold".into(), P::Time(0.1)),
            ("auto_release".into(), P::Bool(true)),
            ("ratio_plain".into(), P::Number(0.6)),
            ("max_lookahead".into(), P::Time(0.02)),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(c.get_param("range"), Some(6.0));
    assert_eq!(c.get_param("stereo_link"), Some(0.5));
    assert_eq!(c.get_param("hold"), Some(100.0));
    assert_eq!(c.get_param("auto_release"), Some(1.0));
    assert_eq!(c.get_param("ratio_plain"), Some(0.6));
    assert_eq!(c.get_param("max_lookahead"), Some(20.0));
    assert!((c.get_param("ratio").unwrap() - 4.0).abs() < 1e-9);
}

#[test]
fn compression_stereo_link_converts_both_declared_forms() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    // The schema declares 0-100 %, the device stores 0-1: the percentage-typed
    // value, the GUI/preset number and a partial number all divide.
    for value in [P::Percentage(100.0), P::Number(100.0)] {
        let c = try_create_effect(&E::Compression, &[("stereo_link".into(), value)], 48_000)
            .expect("stereo link accepts the declared unit");
        assert_eq!(c.get_param("stereo_link"), Some(1.0));
    }
    let c = try_create_effect(
        &E::Compression,
        &[("stereo_link".into(), P::Number(37.0))],
        48_000,
    )
    .unwrap();
    assert!((c.get_param("stereo_link").unwrap() - 0.37).abs() < 1e-12);
}

#[cfg(test)]
fn reverb_fdn_post_eq_shape(label: &str) -> f64 {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::ReverbFdn,
        &[("post_eq1_shape".into(), P::String(label.into()))],
        48_000,
    )
    .unwrap_or_else(|error| panic!("post EQ shape {label}: {error}"));
    effect.get_param("post_eq1_shape").unwrap()
}

#[test]
fn reverb_fdn_post_eq_bell_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("Bell"), 0.0);
}

#[test]
fn reverb_fdn_post_eq_low_shelf_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("Low Shelf"), 1.0);
}

#[test]
fn reverb_fdn_post_eq_high_shelf_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("High Shelf"), 2.0);
}

#[test]
fn reverb_fdn_post_eq_low_cut_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("Low Cut"), 3.0);
}

#[test]
fn reverb_fdn_post_eq_high_cut_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("High Cut"), 4.0);
}

#[test]
fn reverb_fdn_post_eq_notch_label_decodes() {
    assert_eq!(reverb_fdn_post_eq_shape("Notch"), 5.0);
}

#[test]
fn reverb_fdn_post_eq_accepts_snake_case_labels() {
    for (label, code) in [
        ("bell", 0.0),
        ("low_shelf", 1.0),
        ("high_shelf", 2.0),
        ("low_cut", 3.0),
        ("high_cut", 4.0),
        ("notch", 5.0),
    ] {
        assert_eq!(reverb_fdn_post_eq_shape(label), code, "{label}");
    }
}

#[test]
fn reverb_fdn_aliases_share_one_axis() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::ReverbFdn,
        &[
            ("space".into(), P::Percentage(30.0)),
            ("brightness".into(), P::Percentage(55.0)),
            ("character".into(), P::Percentage(20.0)),
            ("distance".into(), P::Percentage(40.0)),
        ],
        48_000,
    )
    .unwrap();
    let decay = effect.get_param("decay").unwrap();
    assert!((decay - 910.28).abs() < 0.05, "space 30 % is {decay} ms");
    assert!((effect.get_param("damping").unwrap() - 45.0).abs() < 1e-9);
    assert!((effect.get_param("modulation").unwrap() - 20.0).abs() < 1e-9);
    assert!((effect.get_param("early_late").unwrap() - 60.0).abs() < 1e-9);
}

#[test]
fn deesser_analog_modes_reach_the_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::DeEsser,
        &[
            ("threshold_mode".into(), P::String("relative".into())),
            ("relative_threshold".into(), P::Db(-10.0)),
            ("detector".into(), P::String("in-band".into())),
            ("sibilance_band".into(), P::String("7 kHz".into())),
            ("band_processing".into(), P::String("split_band".into())),
            ("band_estimation".into(), P::String("learn".into())),
            ("band_override".into(), P::Frequency(5_500.0)),
            ("segment_detection".into(), P::String("on".into())),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(effect.get_param("threshold_mode"), Some(1.0));
    assert_eq!(effect.get_param("relative_threshold"), Some(-10.0));
    assert_eq!(effect.get_param("detector"), Some(1.0));
    assert_eq!(effect.get_param("sibilance_band"), Some(3.0));
    assert_eq!(effect.get_param("band_processing"), Some(1.0));
    assert_eq!(effect.get_param("band_estimation"), Some(1.0));
    assert_eq!(effect.get_param("band_override"), Some(5_500.0));
    assert_eq!(effect.get_param("segment_detection"), Some(1.0));
    assert!(
        try_create_effect(
            &E::DeEsser,
            &[("sibilance_band".into(), P::String("4 kHz".into()))],
            48000
        )
        .is_err()
    );
    assert!(
        try_create_effect(
            &E::DeEsser,
            &[("relative_threshold".into(), P::Db(-60.0))],
            48000
        )
        .is_err()
    );
}

#[test]
fn breath_split_controls_reach_the_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::BreathSplit,
        &[
            ("breath_level".into(), P::Db(-18.0)),
            ("voice_level".into(), P::Db(-3.0)),
            ("threshold".into(), P::Number(0.7)),
            ("smoothing".into(), P::Time(0.05)),
            ("monitor".into(), P::Bool(true)),
            ("split_frequency".into(), P::Frequency(2_500.0)),
        ],
        48_000,
    )
    .unwrap();
    assert_eq!(effect.name(), "breath-split");
    assert_eq!(effect.get_param("breath_level"), Some(-18.0));
    assert_eq!(effect.get_param("voice_level"), Some(-3.0));
    assert_eq!(effect.get_param("threshold"), Some(0.7));
    assert_eq!(effect.get_param("smoothing"), Some(50.0));
    assert_eq!(effect.get_param("monitor"), Some(1.0));
    assert_eq!(effect.get_param("split_frequency"), Some(2_500.0));
    assert_eq!(effect.latency_samples(), 128);
    assert!(
        try_create_effect(
            &E::BreathSplit,
            &[("threshold".into(), P::Percentage(20.0))],
            48_000
        )
        .is_err()
    );
    assert!(
        try_create_effect(
            &E::BreathSplit,
            &[("smoothing".into(), P::Percentage(20.0))],
            48_000
        )
        .is_err()
    );
    assert!(
        try_create_effect(
            &E::BreathSplit,
            &[("split_frequency".into(), P::Frequency(6_000.0))],
            8_000
        )
        .is_err()
    );
}

#[test]
fn plosive_controls_reach_the_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::Plosive,
        &[
            ("amount".into(), P::Percentage(80.0)),
            ("threshold".into(), P::Db(7.0)),
            ("frequency".into(), P::Frequency(110.0)),
            ("release".into(), P::Time(0.055)),
            ("monitor".into(), P::Bool(true)),
        ],
        48_000,
    )
    .unwrap();
    assert_eq!(effect.name(), "plosive");
    assert_eq!(effect.get_param("amount"), Some(80.0));
    assert_eq!(effect.get_param("threshold"), Some(7.0));
    assert_eq!(effect.get_param("frequency"), Some(110.0));
    assert_eq!(effect.get_param("release"), Some(55.0));
    assert_eq!(effect.get_param("monitor"), Some(1.0));
    assert_eq!(effect.latency_samples(), 0);
    assert!(try_create_effect(&E::Plosive, &[("amount".into(), P::Db(80.0))], 48_000).is_err());
    assert!(
        try_create_effect(
            &E::Plosive,
            &[("frequency".into(), P::Frequency(200.0))],
            48_000
        )
        .is_err()
    );
    assert!(try_create_effect(&E::Plosive, &[("bogus".into(), P::Number(1.0))], 48_000).is_err());
}

#[test]
fn reverb_fdn_controls_reach_the_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::ReverbFdn,
        &[
            ("decay".into(), P::Time(2.5)),
            ("space".into(), P::Percentage(50.0)),
            ("decay_rate".into(), P::Percentage(100.0)),
            ("brightness".into(), P::Percentage(50.0)),
            ("character".into(), P::Percentage(0.0)),
            ("distance".into(), P::Percentage(50.0)),
            ("predelay".into(), P::Time(0.02)),
            ("stereo_width".into(), P::Percentage(70.0)),
            ("mix".into(), P::Percentage(100.0)),
            ("decay_band1".into(), P::Db(6.0)),
            ("decay_band1_freq".into(), P::Frequency(700.0)),
            ("decay_band1_q".into(), P::Number(1.4)),
            ("decay_band1_shape".into(), P::String("low shelf".into())),
            ("post_eq3_freq".into(), P::Frequency(1_200.0)),
            ("post_eq3_gain".into(), P::Db(6.0)),
            ("post_eq3_q".into(), P::Number(2.0)),
            ("post_eq3_shape".into(), P::String("low cut".into())),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(effect.name(), "reverb-fdn");
    assert_eq!(effect.get_param("decay"), Some(2500.0));
    assert_eq!(effect.get_param("space"), Some(50.0));
    assert_eq!(effect.get_param("predelay"), Some(20.0));
    assert_eq!(effect.get_param("mix"), Some(100.0));
    assert_eq!(effect.get_param("decay_band1"), Some(6.0));
    assert_eq!(effect.get_param("decay_band1_freq"), Some(700.0));
    assert_eq!(effect.get_param("decay_band1_q"), Some(1.4));
    assert_eq!(effect.get_param("decay_band1_shape"), Some(1.0));
    assert_eq!(effect.get_param("post_eq3_freq"), Some(1_200.0));
    assert_eq!(effect.get_param("post_eq3_gain"), Some(6.0));
    assert_eq!(effect.get_param("post_eq3_q"), Some(2.0));
    assert_eq!(effect.get_param("post_eq3_shape"), Some(3.0));
    assert!(
        try_create_effect(
            &E::ReverbFdn,
            &[("space".into(), P::Percentage(150.0))],
            48000
        )
        .is_err()
    );
    assert!(try_create_effect(&E::ReverbFdn, &[("decay".into(), P::Time(60.0))], 48000).is_err());
}

#[test]
fn limiter_oversampling_and_release_styles_reach_the_dsp() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let effect = try_create_effect(
        &E::Limiter,
        &[
            ("release_mode".into(), P::String("punch".into())),
            ("oversampling".into(), P::String("8x".into())),
            ("link".into(), P::Percentage(40.0)),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(effect.get_param("release_mode"), Some(2.0));
    assert_eq!(effect.get_param("oversampling"), Some(1.0));
    assert_eq!(effect.get_param("link"), Some(40.0));
    let effect = try_create_effect(
        &E::Limiter,
        &[
            ("release_mode".into(), P::String("sustain".into())),
            ("oversampling".into(), P::Number(16.0)),
        ],
        48000,
    )
    .unwrap();
    assert_eq!(effect.get_param("release_mode"), Some(3.0));
    assert_eq!(effect.get_param("oversampling"), Some(2.0));
    let effect = try_create_effect(
        &E::Limiter,
        &[("oversampling".into(), P::Number(4.0))],
        48000,
    )
    .unwrap();
    assert_eq!(effect.get_param("oversampling"), Some(0.0));
    assert!(
        try_create_effect(
            &E::Limiter,
            &[("oversampling".into(), P::String("32x".into()))],
            48000
        )
        .is_err()
    );
    assert!(
        try_create_effect(
            &E::Limiter,
            &[("release_mode".into(), P::String("glue".into()))],
            48000
        )
        .is_err()
    );
}

#[test]
fn checked_effects_reject_wrong_units_unknown_parameters_and_clamping() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    for (effect, name, value) in [
        (E::Compression, "ratio", P::String("2:0".into())),
        (E::Compression, "attack", P::Percentage(20.0)),
        (E::Compression, "attack", P::Time(20.0)),
        (E::Reverb, "decay", P::Time(100000.0)),
        (E::Reverb, "mix", P::Percentage(150.0)),
        (E::Limiter, "typo", P::Number(1.0)),
        (E::Leveler, "target", P::Percentage(20.0)),
        (E::Leveler, "window", P::Percentage(20.0)),
        (E::Opto, "ratio", P::Number(4.0)),
        (E::Fet, "ratio", P::Number(4.0)),
        (E::Fet, "bias", P::Db(3.0)),
        (E::Iron, "drive", P::Percentage(20.0)),
        (E::Iron, "ceiling", P::Db(3.0)),
        (E::Iron, "bogus", P::Number(1.0)),
        (E::PassiveEq, "low_boost", P::Percentage(20.0)),
        (E::PassiveEq, "high_q", P::Db(1.0)),
        (E::PassiveEq, "low_freq", P::Percentage(20.0)),
        (E::Doubler, "delay", P::Time(1.0)),
        (E::Doubler, "voices", P::Percentage(20.0)),
        (E::Exciter, "drive", P::Percentage(20.0)),
        (E::DuckedSend, "predelay", P::Time(0.5)),
        (E::DuckedSend, "width", P::Db(2.0)),
        (E::Resonance, "depth", P::Percentage(20.0)),
        (E::Resonance, "attack", P::Percentage(20.0)),
        (E::Resonance, "bogus", P::Number(1.0)),
        (E::Delay, "feedback", P::Number(1.0)),
        (E::DynamicEq, "band1_wobble", P::Number(1.0)),
        (E::DynamicEq, "band1_type", P::String("wobble".into())),
        (E::DynamicEq, "band1_freq", P::Percentage(20.0)),
        (E::DynamicEq, "band1_gain", P::Time(2.0)),
        (E::DynamicEq, "band1_threshold", P::Percentage(20.0)),
        (E::DynamicEq, "band1_attack", P::Percentage(20.0)),
        (E::DynamicEq, "band1_enabled", P::Number(2.0)),
    ] {
        assert!(
            try_create_effect(&effect, &[(name.into(), value)], 48000).is_err(),
            "{effect:?}.{name}"
        );
    }
}

#[test]
fn resonance_factory_reports_the_stft_latency() {
    use oc_dsl::ast::EffectType as E;
    let fx = try_create_effect(&E::Resonance, &[], 48000).unwrap();
    assert_eq!(fx.name(), "resonance");
    assert_eq!(fx.latency_samples(), 2_304);
    let bypass = try_create_effect(
        &E::Resonance,
        &[("mix".into(), oc_dsl::ast::ParamValue::Percentage(0.0))],
        48000,
    )
    .unwrap();
    assert_eq!(bypass.latency_samples(), 0);
}

#[test]
fn tuner_factory_reports_the_psola_delay() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let fx = try_create_effect(&E::Tuner, &[], 48_000).unwrap();
    assert_eq!(fx.name(), "tuner");
    assert_eq!(fx.latency_samples(), 1_920);
    assert_eq!(fx.get_param("amount"), Some(1.0));
    let low = try_create_effect(
        &E::Tuner,
        &[("mode".into(), P::String("Low Latency".into()))],
        48_000,
    )
    .unwrap();
    assert_eq!(low.latency_samples(), 960);
    let bypass =
        try_create_effect(&E::Tuner, &[("amount".into(), P::Percentage(0.0))], 48_000).unwrap();
    assert_eq!(bypass.latency_samples(), 0);
}

#[test]
fn tuner_parameters_reach_the_dsp_in_declared_units() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let fx = try_create_effect(
        &E::Tuner,
        &[
            ("amount".into(), P::Percentage(50.0)),
            ("retune".into(), P::Time(0.08)),
            ("key".into(), P::String("A".into())),
            ("scale".into(), P::String("major".into())),
            ("mode".into(), P::String("Low Latency".into())),
            ("enabled".into(), P::Bool(true)),
        ],
        48_000,
    )
    .unwrap();
    assert_eq!(fx.get_param("amount"), Some(0.5));
    assert_eq!(fx.get_param("retune"), Some(80.0));
    assert_eq!(fx.get_param("key"), Some(9.0));
    assert_eq!(fx.get_param("scale"), Some(1.0));
    assert_eq!(fx.get_param("mode"), Some(1.0));
    assert_eq!(fx.get_param("enabled"), Some(1.0));
    assert!(
        try_create_effect(&E::Tuner, &[("retune".into(), P::Percentage(20.0))], 48_000).is_err()
    );
    assert!(
        try_create_effect(&E::Tuner, &[("key".into(), P::String("H".into()))], 48_000).is_err()
    );
    assert!(try_create_effect(&E::Tuner, &[("bogus".into(), P::Number(1.0))], 48_000).is_err());
}

#[test]
fn checked_eq_forwards_shelves_and_retains_all_controls() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    let parameters = vec![
        ("freq".into(), P::Frequency(2500.0)),
        ("gain".into(), P::Db(6.0)),
        ("q".into(), P::Number(2.0)),
        ("low_gain".into(), P::Db(-3.0)),
        ("high_gain".into(), P::Db(-4.0)),
    ];
    let fx = try_create_effect(&E::Eq, &parameters, 48000).unwrap();
    for (name, value) in parameters {
        assert_eq!(fx.get_param(&name), value.as_f64());
    }
    assert!(try_create_effect(&E::Eq, &[("freq".into(), P::Frequency(40000.0))], 48000).is_err());
    assert!(try_create_effect(&E::Eq, &[("gain".into(), P::Time(2.0))], 48000).is_err());
    for effect in [
        E::Distortion,
        E::Flanger,
        E::Phaser,
        E::Gate,
        E::StereoWidth,
        E::DeEsser,
        E::BreathSplit,
        E::Plosive,
        E::Multiband,
        E::Leveler,
        E::Opto,
        E::Resonance,
    ] {
        assert!(try_create_effect(&effect, &[], 48000).is_ok());
    }
}

#[test]
fn typed_stereo_delay_times_reach_both_channels() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    for rate in [8000, 44100, 48000] {
        let fx = try_create_effect(
            &E::Delay,
            &[
                ("time_l".into(), P::Time(0.125)),
                ("time_r".into(), P::Time(0.075)),
            ],
            rate,
        )
        .unwrap();
        for (name, expected) in [("time_l", 125.0), ("time_r", 75.0)] {
            assert!((fx.get_param(name).unwrap() - expected).abs() <= 1000.0 / rate as f64);
        }
        assert!(try_create_effect(&E::Delay, &[("time_r".into(), P::Time(3.0))], rate).is_err());
        assert!(
            try_create_effect(&E::Delay, &[("time_l".into(), P::Percentage(20.0))], rate).is_err()
        );
    }
}

#[test]
fn checked_builtin_factory_control_matrix() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    // Non-default values: a dropped factory assignment must fail this test.
    let cases = [
        (E::Gain, "factor", P::Number(0.37), "factor", 0.37),
        (E::Saturator, "drive", P::Percentage(43.0), "drive", 0.43),
        (E::Saturator, "mix", P::Percentage(61.0), "mix", 0.61),
        (E::Eq, "low_freq", P::Frequency(180.0), "low_freq", 180.0),
        (E::Eq, "low_gain", P::Db(-4.0), "low_gain", -4.0),
        (E::Eq, "freq", P::Frequency(1700.0), "freq", 1700.0),
        (E::Eq, "mid_freq", P::Frequency(1900.0), "mid_freq", 1900.0),
        (E::Eq, "gain", P::Db(3.0), "gain", 3.0),
        (E::Eq, "mid_gain", P::Db(-3.0), "mid_gain", -3.0),
        (E::Eq, "q", P::Number(1.7), "q", 1.7),
        (
            E::Eq,
            "high_freq",
            P::Frequency(3100.0),
            "high_freq",
            3100.0,
        ),
        (E::Eq, "high_gain", P::Db(-5.0), "high_gain", -5.0),
        (
            E::Compression,
            "threshold",
            P::Db(-17.0),
            "threshold",
            -17.0,
        ),
        (
            E::Compression,
            "ratio",
            P::String("7:2".into()),
            "ratio",
            3.5,
        ),
        (E::Compression, "attack", P::Time(0.017), "attack", 17.0),
        (E::Compression, "release", P::Time(0.173), "release", 173.0),
        (E::Compression, "knee", P::Db(7.0), "knee", 7.0),
        (E::Compression, "makeup", P::Db(4.0), "makeup", 4.0),
        (
            E::Compression,
            "lookahead",
            P::Time(0.007),
            "lookahead",
            7.0,
        ),
        (
            E::Compression,
            "detector_hpf",
            P::Frequency(120.0),
            "detector_hpf",
            120.0,
        ),
        (
            E::Compression,
            "detector",
            P::String("rms".into()),
            "detector",
            1.0,
        ),
        (E::Compression, "mix", P::Percentage(37.0), "mix", 0.37),
        (
            E::Compression,
            "auto_makeup",
            P::Bool(true),
            "auto_makeup",
            1.0,
        ),
        (E::Compression, "mode", P::Number(1.0), "mode", 1.0),
        (
            E::Compression,
            "sidechain_eq1_on",
            P::Bool(true),
            "sidechain_eq1_on",
            1.0,
        ),
        (
            E::Compression,
            "sidechain_eq1_type",
            P::String("high_shelf".into()),
            "sidechain_eq1_type",
            2.0,
        ),
        (
            E::Compression,
            "sidechain_eq1_frequency",
            P::Frequency(6_000.0),
            "sidechain_eq1_frequency",
            6_000.0,
        ),
        (
            E::Compression,
            "sidechain_eq1_gain",
            P::Db(9.0),
            "sidechain_eq1_gain",
            9.0,
        ),
        (
            E::Compression,
            "sidechain_eq1_q",
            P::Number(1.4),
            "sidechain_eq1_q",
            1.4,
        ),
        (
            E::Compression,
            "sidechain_eq6_on",
            P::Bool(true),
            "sidechain_eq6_on",
            1.0,
        ),
        (
            E::Compression,
            "sidechain_eq6_type",
            P::String("low_cut".into()),
            "sidechain_eq6_type",
            3.0,
        ),
        (E::Multiband, "lookahead", P::Time(0.005), "lookahead", 5.0),
        (
            E::Multiband,
            "band1_lookahead",
            P::Time(0.004),
            "band1_lookahead",
            4.0,
        ),
        (E::Multiband, "band2_solo", P::Bool(true), "band2_solo", 1.0),
        (E::Multiband, "band3_mute", P::Bool(true), "band3_mute", 1.0),
        (E::Reverb, "decay", P::Time(1.7), "decay_seconds", 1.7),
        (E::Reverb, "decay", P::Number(0.63), "decay", 0.63),
        (E::Reverb, "predelay", P::Time(0.037), "predelay", 37.0),
        (E::Reverb, "damping", P::Percentage(41.0), "damping", 0.41),
        (E::Reverb, "mix", P::Percentage(23.0), "mix", 0.23),
        (E::Delay, "time", P::Time(0.137), "time_l", 137.0),
        (E::Delay, "time_l", P::Time(0.173), "time_l", 173.0),
        (E::Delay, "time_r", P::Time(0.197), "time_r", 197.0),
        (E::Delay, "feedback", P::Percentage(41.0), "feedback", 0.41),
        (E::Delay, "mix", P::Percentage(23.0), "mix", 0.23),
        (E::Limiter, "ceiling", P::Db(-3.0), "ceiling", -3.0),
        (E::Limiter, "release", P::Time(0.173), "release", 173.0),
        (E::Limiter, "lookahead", P::Time(0.005), "lookahead", 5.0),
        (E::Limiter, "true_peak", P::Bool(true), "true_peak", 1.0),
        (
            E::Limiter,
            "release_mode",
            P::String("auto".into()),
            "release_mode",
            1.0,
        ),
        (
            E::Reverb,
            "modulation",
            P::Percentage(40.0),
            "modulation",
            40.0,
        ),
        (
            E::Reverb,
            "early_late",
            P::Percentage(25.0),
            "early_late",
            25.0,
        ),
        (
            E::Reverb,
            "stereo_width",
            P::Percentage(140.0),
            "stereo_width",
            140.0,
        ),
        (E::Reverb, "low_cut", P::Frequency(80.0), "low_cut", 80.0),
        (
            E::Reverb,
            "high_cut",
            P::Frequency(6_000.0),
            "high_cut",
            6_000.0,
        ),
        (E::DeEsser, "threshold", P::Db(-42.0), "threshold", -42.0),
        (E::DeEsser, "range", P::Db(9.0), "range", 9.0),
        (E::DeEsser, "attack", P::Time(0.002), "attack", 2.0),
        (E::DeEsser, "release", P::Time(0.08), "release", 80.0),
        (
            E::DeEsser,
            "high_frequency",
            P::Frequency(6_500.0),
            "high_frequency",
            6_500.0,
        ),
        (
            E::DeEsser,
            "low_frequency",
            P::Frequency(12_500.0),
            "low_frequency",
            12_500.0,
        ),
        (
            E::DeEsser,
            "band_processing",
            P::String("split_band".into()),
            "band_processing",
            1.0,
        ),
        (
            E::DeEsser,
            "stereo_link",
            P::Percentage(60.0),
            "stereo_link",
            60.0,
        ),
        (E::DeEsser, "lookahead", P::Time(0.004), "lookahead", 4.0),
        (
            E::BreathSplit,
            "breath_level",
            P::Db(-18.0),
            "breath_level",
            -18.0,
        ),
        (
            E::BreathSplit,
            "voice_level",
            P::Db(-4.0),
            "voice_level",
            -4.0,
        ),
        (
            E::BreathSplit,
            "threshold",
            P::Number(0.7),
            "threshold",
            0.7,
        ),
        (
            E::BreathSplit,
            "smoothing",
            P::Time(0.05),
            "smoothing",
            50.0,
        ),
        (E::BreathSplit, "monitor", P::Bool(true), "monitor", 1.0),
        (
            E::BreathSplit,
            "split_frequency",
            P::Frequency(2_500.0),
            "split_frequency",
            2_500.0,
        ),
        (E::Plosive, "amount", P::Percentage(70.0), "amount", 70.0),
        (E::Plosive, "threshold", P::Db(9.0), "threshold", 9.0),
        (
            E::Plosive,
            "frequency",
            P::Frequency(100.0),
            "frequency",
            100.0,
        ),
        (E::Plosive, "release", P::Time(0.075), "release", 75.0),
        (E::Plosive, "monitor", P::Bool(true), "monitor", 1.0),
        (
            E::Multiband,
            "band_count",
            P::String("4 Bands".into()),
            "band_count",
            2.0,
        ),
        (
            E::Multiband,
            "crossover_1",
            P::Frequency(250.0),
            "crossover_1",
            250.0,
        ),
        (
            E::Multiband,
            "crossover_2",
            P::Frequency(2_500.0),
            "crossover_2",
            2_500.0,
        ),
        (
            E::Multiband,
            "band1_threshold",
            P::Db(-24.0),
            "band1_threshold",
            -24.0,
        ),
        (
            E::Multiband,
            "band1_ratio",
            P::String("4:1".into()),
            "band1_ratio",
            4.0,
        ),
        (
            E::Multiband,
            "band2_attack",
            P::Time(0.003),
            "band2_attack",
            3.0,
        ),
        (
            E::Multiband,
            "band2_release",
            P::Time(0.15),
            "band2_release",
            150.0,
        ),
        (E::Multiband, "band3_knee", P::Db(3.0), "band3_knee", 3.0),
        (
            E::Multiband,
            "band4_makeup",
            P::Db(2.0),
            "band4_makeup",
            2.0,
        ),
        (E::Multiband, "mix", P::Percentage(50.0), "mix", 0.5),
        (E::Chorus, "rate", P::Frequency(2.3), "rate", 2.3),
        (E::Chorus, "depth", P::Time(0.007), "depth", 7.0),
        (E::Chorus, "mix", P::Percentage(23.0), "mix", 0.23),
        (
            E::DynamicEq,
            "band1_enabled",
            P::Bool(true),
            "band1_enabled",
            1.0,
        ),
        (
            E::DynamicEq,
            "band1_type",
            P::String("bell".into()),
            "band1_type",
            0.0,
        ),
        (
            E::DynamicEq,
            "band1_type",
            P::String("band_pass".into()),
            "band1_type",
            6.0,
        ),
        (E::DynamicEq, "band1_solo", P::Bool(true), "band1_solo", 1.0),
        (
            E::DynamicEq,
            "band1_channel",
            P::String("mid".into()),
            "band1_channel",
            1.0,
        ),
        (
            E::DynamicEq,
            "band1_freq",
            P::Frequency(1_200.0),
            "band1_freq",
            1_200.0,
        ),
        (E::DynamicEq, "band1_q", P::Number(1.4), "band1_q", 1.4),
        (E::DynamicEq, "band1_gain", P::Db(4.5), "band1_gain", 4.5),
        (
            E::DynamicEq,
            "band1_dynamic",
            P::Bool(true),
            "band1_dynamic",
            1.0,
        ),
        (
            E::DynamicEq,
            "band1_threshold",
            P::Db(-27.0),
            "band1_threshold",
            -27.0,
        ),
        (
            E::DynamicEq,
            "band1_range",
            P::Db(-8.0),
            "band1_range",
            -8.0,
        ),
        (
            E::DynamicEq,
            "band1_attack",
            P::Time(0.013),
            "band1_attack",
            13.0,
        ),
        (
            E::DynamicEq,
            "band1_release",
            P::Time(0.245),
            "band1_release",
            245.0,
        ),
        (
            E::DynamicEq,
            "low_cut_enabled",
            P::Bool(true),
            "low_cut_enabled",
            1.0,
        ),
        (
            E::DynamicEq,
            "low_cut_freq",
            P::Frequency(35.0),
            "low_cut_freq",
            35.0,
        ),
        (E::DynamicEq, "low_cut_q", P::Number(0.9), "low_cut_q", 0.9),
        (
            E::DynamicEq,
            "high_cut_freq",
            P::Frequency(3_000.0),
            "high_cut_freq",
            3_000.0,
        ),
        (E::Distortion, "drive", P::Percentage(37.0), "drive", 0.37),
        (E::Distortion, "tone", P::Percentage(61.0), "tone", 0.61),
        (E::Distortion, "output", P::Db(-3.0), "output", -3.0),
        (E::Distortion, "mix", P::Percentage(43.0), "mix", 0.43),
        (E::Flanger, "rate", P::Frequency(2.3), "rate", 2.3),
        (E::Flanger, "depth", P::Time(0.007), "depth", 7.0),
        (
            E::Flanger,
            "feedback",
            P::Percentage(41.0),
            "feedback",
            0.41,
        ),
        (E::Flanger, "mix", P::Percentage(23.0), "mix", 0.23),
        (E::Phaser, "rate", P::Frequency(1.7), "rate", 1.7),
        (E::Phaser, "depth", P::Percentage(71.0), "depth", 0.71),
        (E::Phaser, "feedback", P::Percentage(31.0), "feedback", 0.31),
        (E::Phaser, "mix", P::Percentage(19.0), "mix", 0.19),
        (E::Gate, "threshold", P::Db(-23.0), "threshold", -23.0),
        (E::Gate, "attack", P::Time(0.003), "attack", 3.0),
        (E::Gate, "hold", P::Time(0.011), "hold", 11.0),
        (E::Gate, "release", P::Time(0.173), "release", 173.0),
        (E::Gate, "range", P::Db(-41.0), "range", -41.0),
        (E::Leveler, "target", P::Db(-24.0), "target", -24.0),
        (E::Leveler, "speed", P::Number(4.0), "speed", 4.0),
        (E::Leveler, "range", P::Db(20.0), "range", 20.0),
        (E::Leveler, "window", P::Time(0.75), "window", 750.0),
        (E::Leveler, "gate", P::Db(-50.0), "gate", -50.0),
        (
            E::Leveler,
            "gate_release",
            P::Time(1.2),
            "gate_release",
            1_200.0,
        ),
        (E::Opto, "peak_reduction", P::Db(9.0), "peak_reduction", 9.0),
        (E::Opto, "release", P::Time(2.5), "release", 2_500.0),
        (E::Opto, "makeup", P::Db(3.0), "makeup", 3.0),
        (E::Fet, "drive", P::Db(6.0), "drive", 6.0),
        (E::Fet, "attack", P::Time(0.0003), "attack", 0.3),
        (E::Fet, "release", P::Time(0.12), "release", 120.0),
        (E::Fet, "output", P::Db(-1.0), "output", -1.0),
        (E::Fet, "bias", P::Percentage(75.0), "bias", 0.75),
        (E::Iron, "drive", P::Db(6.0), "drive", 6.0),
        (E::Iron, "bias", P::Number(0.2), "bias", 0.2),
        (E::Iron, "ceiling", P::Percentage(30.0), "ceiling", 0.3),
        (E::Iron, "low_freq", P::Frequency(25.0), "low_freq", 25.0),
        (
            E::Iron,
            "high_freq",
            P::Frequency(18_000.0),
            "high_freq",
            18_000.0,
        ),
        (E::Iron, "output", P::Db(-2.0), "output", -2.0),
        (
            E::PassiveEq,
            "low_freq",
            P::Frequency(100.0),
            "low_freq",
            100.0,
        ),
        (E::PassiveEq, "low_boost", P::Db(5.0), "low_boost", 5.0),
        (E::PassiveEq, "low_cut", P::Db(5.0), "low_cut", 5.0),
        (
            E::PassiveEq,
            "high_cut_freq",
            P::Frequency(10_000.0),
            "high_cut_freq",
            10_000.0,
        ),
        (E::PassiveEq, "high_cut", P::Db(4.0), "high_cut", 4.0),
        (E::PassiveEq, "high_q", P::Number(1.2), "high_q", 1.2),
        (E::PassiveEq, "tilt", P::Db(2.0), "tilt", 2.0),
        (
            E::PassiveEq,
            "air_freq",
            P::Frequency(10_000.0),
            "air_freq",
            10_000.0,
        ),
        (E::PassiveEq, "air", P::Db(3.0), "air", 3.0),
        (E::Doubler, "voices", P::Number(3.0), "voices", 3.0),
        (E::Doubler, "delay", P::Time(0.021), "delay", 21.0),
        (E::Doubler, "rate", P::Frequency(0.4), "rate", 0.4),
        (E::Doubler, "depth", P::Time(0.003), "depth", 3.0),
        (E::Doubler, "detune", P::Number(7.0), "detune", 7.0),
        (E::Doubler, "focus", P::Frequency(250.0), "focus", 250.0),
        (E::Doubler, "mix", P::Percentage(37.0), "mix", 0.37),
        (E::Exciter, "amount", P::Percentage(63.0), "amount", 0.63),
        (E::Exciter, "drive", P::Db(9.0), "drive", 9.0),
        (
            E::Exciter,
            "character",
            P::Percentage(41.0),
            "character",
            0.41,
        ),
        (
            E::Exciter,
            "low_freq",
            P::Frequency(10_000.0),
            "low_freq",
            10_000.0,
        ),
        (
            E::Exciter,
            "high_freq",
            P::Frequency(17_000.0),
            "high_freq",
            17_000.0,
        ),
        (E::Exciter, "output", P::Db(-3.0), "output", -3.0),
        (E::DuckedSend, "predelay", P::Time(0.035), "predelay", 35.0),
        (E::DuckedSend, "depth", P::Db(12.0), "depth", 12.0),
        (E::DuckedSend, "attack", P::Time(0.03), "attack", 30.0),
        (E::DuckedSend, "release", P::Time(0.5), "release", 500.0),
        (E::DuckedSend, "threshold", P::Db(-35.0), "threshold", -35.0),
        (E::DuckedSend, "width", P::Percentage(150.0), "width", 150.0),
        (E::Resonance, "threshold", P::Db(9.0), "threshold", 9.0),
        (E::Resonance, "depth", P::Db(12.0), "depth", 12.0),
        (
            E::Resonance,
            "selectivity",
            P::Percentage(60.0),
            "selectivity",
            60.0,
        ),
        (E::Resonance, "attack", P::Time(0.02), "attack", 20.0),
        (E::Resonance, "release", P::Time(0.4), "release", 400.0),
        (E::Resonance, "mix", P::Percentage(70.0), "mix", 0.7),
        (E::Resonance, "enabled", P::Bool(false), "enabled", 0.0),
        (
            E::StereoWidth,
            "amount",
            P::Percentage(150.0),
            "amount",
            1.5,
        ),
        (E::Limiter, "link", P::Percentage(37.0), "link", 37.0),
        (
            E::ReverbFdn,
            "decay_band1_freq",
            P::Frequency(700.0),
            "decay_band1_freq",
            700.0,
        ),
        (
            E::ReverbFdn,
            "decay_band1_q",
            P::Number(1.6),
            "decay_band1_q",
            1.6,
        ),
        (
            E::ReverbFdn,
            "decay_band1_shape",
            P::String("bell".into()),
            "decay_band1_shape",
            2.0,
        ),
    ];
    for rate in [8000, 44100, 48000] {
        for (effect, name, value, effective_name, expected) in &cases {
            let fx = try_create_effect(effect, &[(name.to_string(), value.clone())], rate)
                .unwrap_or_else(|error| panic!("{effect:?}.{name} at {rate}: {error}"));
            let tolerance = if matches!(effective_name, &"time_l" | &"time_r" | &"predelay") {
                1000.0 / rate as f64
            } else {
                1e-9
            };
            assert!(
                (fx.get_param(effective_name).unwrap() - expected).abs() <= tolerance,
                "{effect:?}.{name} at {rate}: got {:?}, expected {expected}",
                fx.get_param(effective_name)
            );
            assert!(
                try_create_effect(effect, &[(name.to_string(), P::Number(f64::NAN))], rate)
                    .is_err(),
                "{effect:?}.{name} at {rate} accepted NaN"
            );
        }
    }
}

pub(crate) fn typed_builtin_value(
    effect_type: &oc_dsl::ast::EffectType,
    name: &str,
    value: &oc_dsl::ast::ParamValue,
) -> Option<f64> {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    match (effect_type, name, value) {
        (E::Compression, "ratio", P::String(ratio)) => ratio.split_once(':').and_then(|(a, b)| {
            let a = a.trim().parse::<f64>().ok()?;
            let b = b.trim().parse::<f64>().ok()?;
            (a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0).then_some(a / b)
        }),
        (E::Compression, "detector", P::String(mode)) => match mode.as_str() {
            "peak" => Some(0.0),
            "rms" => Some(1.0),
            _ => None,
        },
        (E::Compression, "auto_makeup" | "auto_release", P::Bool(on))
        | (E::Limiter, "true_peak", P::Bool(on)) => Some(if *on { 1.0 } else { 0.0 }),
        (E::Limiter, "release_mode", P::String(mode)) => match mode.to_ascii_lowercase().as_str() {
            "fast" => Some(0.0),
            "auto" => Some(1.0),
            "punch" => Some(2.0),
            "sustain" => Some(3.0),
            _ => None,
        },
        (E::Limiter, "oversampling", P::String(factor)) => {
            match factor.to_ascii_lowercase().as_str() {
                "4x" | "4" => Some(0.0),
                "8x" | "8" => Some(1.0),
                "16x" | "16" => Some(2.0),
                _ => None,
            }
        }
        (E::Limiter, "oversampling", P::Number(number)) => match number.round() as i32 {
            0 | 4 => Some(0.0),
            1 | 8 => Some(1.0),
            2 | 16 => Some(2.0),
            _ => None,
        },
        (E::Limiter, "link", P::Percentage(percent)) => Some(*percent),
        (
            E::Compression,
            "attack" | "release" | "lookahead" | "hold" | "max_lookahead",
            P::Time(seconds),
        )
        | (E::Limiter, "release" | "lookahead", P::Time(seconds)) => Some(seconds * 1000.0),
        (E::Leveler, "window" | "gate_release", P::Time(seconds))
        | (E::Opto, "release", P::Time(seconds)) => Some(seconds * 1000.0),
        (E::Compression, "detector_hpf", P::Frequency(hz)) => Some(*hz),
        (E::Compression, name, P::Frequency(hz))
            if compressor::sidechain_eq_param(name).map(|(_, field)| field)
                == Some("frequency") =>
        {
            Some(*hz)
        }
        (E::Compression, name, P::Db(db))
            if compressor::sidechain_eq_param(name).map(|(_, field)| field) == Some("gain") =>
        {
            Some(*db)
        }
        (E::Compression, name, P::Number(number))
            if compressor::sidechain_eq_param(name).map(|(_, field)| field) == Some("q") =>
        {
            Some(*number)
        }
        (E::Compression, name, P::Bool(on))
            if compressor::sidechain_eq_param(name).map(|(_, field)| field) == Some("on") =>
        {
            Some(if *on { 1.0 } else { 0.0 })
        }
        (E::Compression, name, P::String(shape))
            if compressor::sidechain_eq_param(name).map(|(_, field)| field) == Some("type") =>
        {
            compressor::sidechain_eq_type_code(shape)
        }
        (E::Compression, "stereo_link", P::Percentage(percent)) => Some(*percent / 100.0),
        // The schema declares 0-100 % while the device stores 0-1, so a bare
        // number is a percentage too: the GUI save and a preset write it that way.
        (E::Compression, "stereo_link", P::Number(number)) => Some(*number / 100.0),
        (E::Compression, "ratio_plain" | "attack_plain" | "release_plain", P::Number(number)) => {
            Some(*number)
        }
        (E::Reverb, "predelay", P::Time(seconds))
        | (E::ReverbFdn, "decay" | "predelay", P::Time(seconds))
        | (E::Delay, "time" | "time_l" | "time_r", P::Time(seconds))
        | (E::Chorus | E::Flanger, "depth", P::Time(seconds)) => Some(seconds * 1000.0),
        (
            E::ReverbFdn,
            "mix" | "damping" | "stereo_width" | "character" | "modulation" | "brightness"
            | "distance" | "early_late" | "space" | "decay_rate",
            P::Percentage(percent),
        ) => Some(*percent),
        (E::ReverbFdn, name, P::Db(db))
            if name.starts_with("decay_band") || name.starts_with("post_eq") =>
        {
            Some(*db)
        }
        (E::ReverbFdn, name, P::Frequency(hz))
            if name.starts_with("decay_band") || name.starts_with("post_eq") =>
        {
            Some(*hz)
        }
        (E::ReverbFdn, name, P::String(shape))
            if name.starts_with("decay_band") && name.ends_with("_shape") =>
        {
            reverb_fdn::decay_eq_shape_code(shape)
        }
        (E::ReverbFdn, name, P::String(shape)) if name.ends_with("_shape") => {
            reverb_fdn::post_eq_shape_code(shape)
        }
        (E::ReverbFdn, name, P::Number(number))
            if name.starts_with("decay_band") || name.starts_with("post_eq") =>
        {
            Some(*number)
        }
        (_, "mix" | "damping" | "feedback", P::Percentage(percent))
        | (E::Saturator, "drive", P::Percentage(percent))
        | (E::Distortion, "drive" | "tone", P::Percentage(percent))
        | (E::Phaser, "depth", P::Percentage(percent))
        | (E::StereoWidth, "amount", P::Percentage(percent)) => Some(percent / 100.0),
        (E::Resonance, "selectivity", P::Percentage(percent)) => Some(*percent),
        (E::Resonance, "threshold" | "depth", P::Db(db)) => Some(*db),
        (E::Resonance, "attack" | "release", P::Time(seconds)) => Some(*seconds * 1000.0),
        (E::Resonance, name, P::Bool(on)) if resonance::switch_param(name) => {
            Some(if *on { 1.0 } else { 0.0 })
        }
        (E::Doubler, "voices", P::Number(number)) => Some(number.round().clamp(2.0, 4.0)),
        (E::Doubler, "detune", P::Cents(cents)) => Some(*cents),
        (E::Doubler, "delay" | "depth", P::Time(seconds)) => Some(seconds * 1000.0),
        (E::Doubler, "rate" | "focus", P::Frequency(hz)) => Some(*hz),
        (E::Exciter, "amount" | "character", P::Percentage(percent)) => Some(*percent / 100.0),
        (E::Exciter, "drive" | "output", P::Db(db)) => Some(*db),
        (E::Exciter, "low_freq" | "high_freq", P::Frequency(hz)) => Some(*hz),
        (E::DuckedSend, "predelay" | "attack" | "release", P::Time(seconds)) => {
            Some(seconds * 1000.0)
        }
        (E::DuckedSend, "depth" | "threshold", P::Db(db)) => Some(*db),
        (E::DuckedSend, "width", P::Percentage(percent)) => Some(*percent),
        (E::DuckedSend, "width", P::Number(number)) => Some(*number),
        (E::Tuner, "amount", P::Percentage(percent)) => Some(*percent / 100.0),
        (E::Tuner, "retune", P::Time(seconds)) => Some(*seconds * 1000.0),
        (E::Tuner, "key", P::String(name)) => tuner::key_name(name),
        (E::Tuner, "scale", P::String(name)) => tuner::scale_name(name),
        (E::Tuner, "mode", P::String(name)) => tuner::mode_code(name),
        (E::Tuner, name, P::Bool(on)) if tuner::switch_param(name) => {
            Some(if *on { 1.0 } else { 0.0 })
        }
        (E::Fet, "bias", P::Percentage(percent)) => Some(*percent / 100.0),
        (E::Fet, "bias", P::Number(number)) => Some(*number / 100.0),
        (E::Fet, "drive" | "output", P::Db(db)) => Some(*db),
        (E::Fet, "attack" | "release", P::Time(seconds)) => Some(*seconds * 1000.0),
        (_, _, P::Number(number)) => Some(*number),
        (E::Leveler, "target" | "range" | "gate", P::Db(db))
        | (E::Opto, "peak_reduction" | "makeup", P::Db(db)) => Some(*db),
        (E::Compression, "threshold" | "knee" | "makeup" | "range", P::Db(db))
        | (E::Limiter, "ceiling", P::Db(db))
        | (E::Distortion, "output", P::Db(db))
        | (E::Gate, "threshold" | "range", P::Db(db)) => Some(*db),
        (E::Gate, "attack" | "hold" | "release", P::Time(seconds)) => Some(*seconds * 1000.0),
        (E::Reverb, "decay", P::Time(seconds)) => Some(*seconds),
        (E::Chorus | E::Flanger | E::Phaser, "rate", P::Frequency(hz)) => Some(*hz),
        (E::Eq, "freq" | "mid_freq" | "low_freq" | "high_freq", P::Frequency(hz)) => Some(*hz),
        (E::Eq, "gain" | "mid_gain" | "low_gain" | "high_gain", P::Db(db)) => Some(*db),
        (E::DynamicEq, name, P::Frequency(hz)) if dynamic_eq::frequency_param(name) => Some(*hz),
        (E::DynamicEq, name, P::Db(db)) if dynamic_eq::db_param(name) => Some(*db),
        (E::DynamicEq, name, P::Time(seconds)) if dynamic_eq::time_ms_param(name) => {
            Some(*seconds * 1000.0)
        }
        (E::DynamicEq, name, P::Bool(on)) if dynamic_eq::switch_param(name) => {
            Some(if *on { 1.0 } else { 0.0 })
        }
        (E::DynamicEq, name, P::String(kind)) if dynamic_eq::type_param(name) => {
            dynamic_eq::BandKind::from_name(kind).map(|kind| kind.code())
        }
        (E::DynamicEq, name, P::String(target)) if dynamic_eq::channel_param(name) => {
            dynamic_eq::ChannelTarget::from_name(target).map(|target| target.code())
        }
        (E::DynamicEq, _, _) => None,
        (E::Reverb, "modulation" | "early_late" | "stereo_width", P::Percentage(percent)) => {
            Some(*percent)
        }
        (E::Reverb, "low_cut" | "high_cut", P::Frequency(hz)) => Some(*hz),
        (E::DeEsser, name, P::Frequency(hz)) if deesser::frequency_param(name) => Some(*hz),
        (E::DeEsser, name, P::Db(db)) if deesser::db_param(name) => Some(*db),
        (E::DeEsser, name, P::Time(seconds)) if deesser::time_ms_param(name) => {
            Some(*seconds * 1000.0)
        }
        (E::DeEsser, name, P::Percentage(percent)) if deesser::percent_param(name) => {
            Some(*percent)
        }
        (E::DeEsser, "band_processing", P::String(mode)) => deesser::band_processing_code(mode),
        (E::DeEsser, "threshold_mode", P::String(mode)) => deesser::threshold_mode_code(mode),
        (E::DeEsser, "detector", P::String(place)) => deesser::detector_code(place),
        (E::DeEsser, "sibilance_band", P::String(band)) => deesser::sibilance_band_code(band),
        (E::DeEsser, "band_estimation", P::String(mode)) => deesser::band_estimation_code(mode),
        (E::DeEsser, "segment_detection", P::String(mode)) => deesser::segment_detection_code(mode),
        (E::DeEsser, _, _) => None,
        (E::Multiband, "band_count", P::String(mode)) => multiband::band_count_code(mode),
        (E::Multiband, "slope", P::String(text)) => multiband::slope_code(text),
        (E::Multiband, "phase_mode", P::String(text)) => multiband::phase_mode_code(text),
        (E::Multiband, "detector", P::String(text)) => multiband::detector_code(text),
        (E::Multiband, name, P::Frequency(hz)) if multiband::frequency_param(name) => Some(*hz),
        (E::Multiband, name, P::Db(db)) if multiband::db_param(name) => Some(*db),
        (E::Multiband, name, P::Time(seconds)) if multiband::time_ms_param(name) => {
            Some(*seconds * 1000.0)
        }
        (E::Multiband, "lookahead", P::Time(seconds)) => Some(*seconds * 1000.0),
        (E::Multiband, name, P::Bool(on)) if multiband::switch_param(name) => {
            Some(if *on { 1.0 } else { 0.0 })
        }
        (E::Multiband, name, P::String(text)) if multiband::ratio_param(name) => {
            multiband::ratio_code(text)
        }
        (E::Multiband, _, _) => None,
        (E::Iron, "ceiling", P::Percentage(percent)) => Some(percent / 100.0),
        (E::Iron, "drive" | "output", P::Db(db))
        | (
            E::PassiveEq,
            "low_boost" | "low_cut" | "high_boost" | "high_cut" | "tilt" | "air",
            P::Db(db),
        ) => Some(*db),
        (E::Iron, "low_freq" | "high_freq", P::Frequency(hz))
        | (
            E::PassiveEq,
            "low_freq" | "high_freq" | "high_cut_freq" | "air_freq",
            P::Frequency(hz),
        ) => Some(*hz),
        (E::BreathSplit, "breath_level" | "voice_level", P::Db(db)) => Some(*db),
        (E::BreathSplit, "smoothing", P::Time(seconds)) => Some(seconds * 1000.0),
        (E::BreathSplit, "split_frequency", P::Frequency(hz)) => Some(*hz),
        (E::BreathSplit, "monitor", P::Bool(on)) => Some(if *on { 1.0 } else { 0.0 }),
        (E::Plosive, "amount", P::Percentage(percent)) => Some(*percent),
        (E::Plosive, "threshold", P::Db(db)) => Some(*db),
        (E::Plosive, "frequency", P::Frequency(hz)) => Some(*hz),
        (E::Plosive, "release", P::Time(seconds)) => Some(seconds * 1000.0),
        (E::Plosive, "monitor", P::Bool(on)) => Some(if *on { 1.0 } else { 0.0 }),
        (
            E::Gain
            | E::Eq
            | E::Compression
            | E::Limiter
            | E::Reverb
            | E::Delay
            | E::Chorus
            | E::Saturator
            | E::Distortion
            | E::Flanger
            | E::Phaser
            | E::Gate
            | E::Leveler
            | E::Opto
            | E::Iron
            | E::PassiveEq
            | E::Doubler
            | E::Exciter
            | E::DuckedSend
            | E::Fet
            | E::Resonance
            | E::Tuner
            | E::BreathSplit
            | E::Plosive
            | E::StereoWidth,
            _,
            _,
        ) => None,
        _ => value.as_f64(),
    }
}

/// The refusal a value the factory cannot type earns: it names the value as the
/// project wrote it and one spelling the device accepts.
fn invalid_typed_parameter(
    effect_type: &oc_dsl::ast::EffectType,
    name: &str,
    value: &oc_dsl::ast::ParamValue,
) -> String {
    let accepted = schema_bridge::device_for_effect(effect_type)
        .and_then(|device| device.param(name))
        .map(|param| accepted_spelling(name, param))
        .unwrap_or_else(|| "a number or the unit its documentation states".to_string());
    format!(
        "invalid typed effect parameter: {name}={} ({name} expects {accepted})",
        value_text(value)
    )
}

/// One documented spelling of `name`, built from the device's declared default
/// so the value is in range by construction.
fn accepted_spelling(name: &str, param: &oc_dsp::schema::ParamSchema) -> String {
    use oc_dsp::schema::{ParamKind, ParamUnit};
    match param.kind {
        ParamKind::Bool => format!("true or false, such as {name}=true"),
        ParamKind::Enum => format!(
            "one of {}, written by index, such as {name}={}",
            param
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| format!("{option} ({index})"))
                .collect::<Vec<_>>()
                .join(", "),
            param.default
        ),
        ParamKind::Continuous => match param.unit {
            ParamUnit::Hz => format!("a frequency in Hz, such as {name}={}Hz", param.default),
            ParamUnit::Db => format!("a gain in dB, such as {name}={}dB", param.default),
            ParamUnit::Ms => format!("a time in ms, such as {name}={}ms", param.default),
            ParamUnit::Percent => format!("a percentage, such as {name}={}%", param.default),
            ParamUnit::Fraction => format!(
                "a bare fraction, such as {name}={} (displayed as {}{})",
                param.default,
                param.default * param.unit.definition().display_multiplier,
                param.unit.definition().display_unit
            ),
            ParamUnit::Seconds => format!("a time in seconds, such as {name}={}s", param.default),
            ParamUnit::Cents => format!("a tuning in cents, such as {name}={}ct", param.default),
            ParamUnit::Semitones | ParamUnit::Octaves | ParamUnit::Degrees => format!(
                "a number in {}, such as {name}={}",
                param.unit.as_str(),
                param.default
            ),
            ParamUnit::Ratio => format!("a ratio, such as {name}=2:1"),
            ParamUnit::None => format!("a number, such as {name}={}", param.default),
        },
    }
}

/// A parameter value as a project writes it, so a refusal names what it read.
fn value_text(value: &oc_dsl::ast::ParamValue) -> String {
    use oc_dsl::ast::ParamValue as P;
    match value {
        P::Number(number) => number.to_string(),
        P::Db(db) => format!("{db}dB"),
        P::Percentage(percent) => format!("{percent}%"),
        P::Frequency(hz) => format!("{hz}Hz"),
        P::Cents(cents) => format!("{cents}ct"),
        P::Time(seconds) => format!("{seconds}s"),
        P::String(text) => text.clone(),
        P::Bool(on) => on.to_string(),
        P::Array(values) => format!("{values:?}"),
    }
}

/// Why a plugin could not be built as an insert effect.
enum PluginEffectError {
    /// Not installed, not resolvable, or the loader refused to instantiate it.
    Unavailable,
    /// Declared buses an insert cannot feed, already formatted for the user.
    #[cfg_attr(not(feature = "plugin-host"), allow(dead_code))]
    Refused(String),
}

pub fn try_create_effect(
    effect_type: &oc_dsl::ast::EffectType,
    params: &[(String, oc_dsl::ast::ParamValue)],
    sample_rate: u32,
) -> Result<Box<dyn AudioEffect>, String> {
    try_create_effect_at(effect_type, params, sample_rate, "")
}

/// Build an effect that sits in the chain named by `place`.
///
/// `place` is the user-facing location (`track 'Lead'`, `bus 'Drum Bus'`,
/// `master`) a refusal names; an empty place omits the clause.
pub fn try_create_effect_at(
    effect_type: &oc_dsl::ast::EffectType,
    params: &[(String, oc_dsl::ast::ParamValue)],
    sample_rate: u32,
    place: &str,
) -> Result<Box<dyn AudioEffect>, String> {
    // `bypass` is the effect's own field; a parameter of that name would be a
    // second meaning, so it is refused instead of reaching any device.
    if params.iter().any(|(name, _)| name == "bypass") {
        return Err(
            "'bypass' is an effect field, not a device parameter; write bypass=true on the effect"
                .into(),
        );
    }
    if let oc_dsl::ast::EffectType::Plugin(id) = effect_type {
        let values = params
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.as_str(), f)))
            .collect();
        create_plugin_effect(id, &values, sample_rate, place).map_err(|error| match error {
            PluginEffectError::Unavailable => {
                format!("external effect '{id}' could not be loaded; refusing bypassed render")
            }
            PluginEffectError::Refused(reason) => reason,
        })
    } else {
        use oc_dsl::ast::EffectType;
        // The idle-gate switch is a playback property, not a device parameter.
        let params: Vec<_> = params
            .iter()
            .filter(|(name, _)| name != crate::idle_gate::SMART_DISABLE_PARAM)
            .cloned()
            .collect();
        let resolved = schema_bridge::resolve_effect(effect_type, &params);
        let effect_type = &resolved.effect_type;
        let params = resolved.params.as_slice();
        if !matches!(
            effect_type,
            EffectType::Gain
                | EffectType::Eq
                | EffectType::DynamicEq
                | EffectType::Compression
                | EffectType::DeEsser
                | EffectType::BreathSplit
                | EffectType::Plosive
                | EffectType::Multiband
                | EffectType::Leveler
                | EffectType::Opto
                | EffectType::Fet
                | EffectType::Limiter
                | EffectType::Reverb
                | EffectType::ReverbFdn
                | EffectType::Delay
                | EffectType::Chorus
                | EffectType::Saturator
                | EffectType::Distortion
                | EffectType::Flanger
                | EffectType::Phaser
                | EffectType::Gate
                | EffectType::Iron
                | EffectType::PassiveEq
                | EffectType::Doubler
                | EffectType::Exciter
                | EffectType::DuckedSend
                | EffectType::Resonance
                | EffectType::Tuner
                | EffectType::StereoWidth
        ) {
            return Err(format!(
                "built-in effect {effect_type:?} has no implemented DSP; refusing passthrough"
            ));
        }
        if matches!(effect_type, oc_dsl::ast::EffectType::Reverb) {
            for (name, value) in params {
                if let ("decay", oc_dsl::ast::ParamValue::Time(seconds)) = (name.as_str(), value)
                    && (!seconds.is_finite() || !(0.01..=60.0).contains(seconds))
                {
                    return Err("typed reverb decay must be within 0.01..60 seconds".into());
                }
            }
        }
        if matches!(effect_type, oc_dsl::ast::EffectType::Limiter) {
            for (name, value) in params {
                if name == "ceiling"
                    && !value
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && (-12.0..=0.0).contains(&v))
                {
                    return Err(
                        "limiter.ceiling must be within -12..0 dB; refusing silent clamp".into(),
                    );
                }
            }
        }
        let fx = create_effect(effect_type, params, sample_rate);
        use oc_dsl::ast::{EffectType as E, ParamValue as P};
        if matches!(
            effect_type,
            E::Gain
                | E::Eq
                | E::DynamicEq
                | E::Compression
                | E::DeEsser
                | E::BreathSplit
                | E::Plosive
                | E::Multiband
                | E::Leveler
                | E::Opto
                | E::Fet
                | E::Limiter
                | E::Reverb
                | E::ReverbFdn
                | E::Delay
                | E::Chorus
                | E::Saturator
                | E::Distortion
                | E::Flanger
                | E::Phaser
                | E::Gate
                | E::Iron
                | E::PassiveEq
                | E::Doubler
                | E::Exciter
                | E::DuckedSend
                | E::Resonance
                | E::Tuner
                | E::StereoWidth
        ) {
            let mut seen = std::collections::HashSet::new();
            for (name, value) in params {
                if !seen.insert(name) {
                    return Err(format!("duplicate effect parameter: {name}"));
                }
                let expected = typed_builtin_value(effect_type, name, value)
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| invalid_typed_parameter(effect_type, name, value))?;
                let effective_name = match (effect_type, name.as_str(), value) {
                    (E::Reverb, "decay", P::Time(_)) => "decay_seconds",
                    (E::Delay, "time", _) => "time_l",
                    _ => name.as_str(),
                };
                let actual = fx
                    .get_param(effective_name)
                    .ok_or_else(|| format!("unsupported effect parameter: {name}"))?;
                let tolerance = if matches!(
                    (effect_type, name.as_str()),
                    (E::Delay, "time" | "time_l" | "time_r") | (E::Reverb, "predelay")
                ) {
                    1000.0 / sample_rate.max(1) as f64
                } else {
                    1e-9
                };
                if !actual.is_finite() || (actual - expected).abs() > tolerance {
                    return Err(format!(
                        "effect parameter {name} requested {expected} but DSP uses {actual}; refusing silent clamp"
                    ));
                }
            }
        }
        Ok(fx)
    }
}

/// Create an effect instance from the DSL EffectType and parameters.
/// Authoritative offline renders must not silently replace missing external
/// effects with passthrough; legacy realtime callers retain create_effect.
pub fn create_effect(
    effect_type: &oc_dsl::ast::EffectType,
    params: &[(String, oc_dsl::ast::ParamValue)],
    sample_rate: u32,
) -> Box<dyn AudioEffect> {
    use oc_dsl::ast::EffectType;

    let resolved = schema_bridge::resolve_effect(effect_type, params);
    let effect_type = &resolved.effect_type;
    let params = resolved.params.as_slice();
    let param_map: HashMap<&str, f64> = params
        .iter()
        .filter_map(|(k, v)| {
            typed_builtin_value(effect_type, k, v).map(|value| (k.as_str(), value))
        })
        .collect();

    match effect_type {
        EffectType::Gain => Box::new(LinearGain(param_map.get("factor").copied().unwrap_or(1.0))),
        EffectType::Saturator => {
            let mut fx = saturator::Saturator::new();
            for name in ["drive", "mix"] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Eq => {
            let mut fx = eq::ParametricEq::new(sample_rate);
            for name in [
                "low_freq",
                "low_gain",
                "freq",
                "mid_freq",
                "gain",
                "mid_gain",
                "q",
                "high_freq",
                "high_gain",
            ] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::DynamicEq => {
            let mut fx = dynamic_eq::DynamicEq::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Compression => {
            let mut fx = compressor::Compressor::new(sample_rate);
            // The engineering/plain pairs resolve in their historical order, so
            // a document that sets both keeps the value it had before.
            const COUPLED: [&str; 6] = [
                "ratio",
                "ratio_plain",
                "attack",
                "attack_plain",
                "release",
                "release_plain",
            ];
            for (name, value) in params {
                if COUPLED.contains(&name.as_str()) {
                    continue;
                }
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            for name in COUPLED {
                if let Some((_, value)) = params.iter().find(|(key, _)| key == name)
                    && let Some(value) = typed_builtin_value(effect_type, name, value)
                {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Leveler => {
            let mut fx = leveler::Leveler::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Opto => {
            let mut fx = opto::Opto::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Fet => {
            let mut fx = fet::Fet::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Iron => {
            let mut fx = iron::Iron::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::PassiveEq => {
            let mut fx = passive_eq::PassiveEq::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Doubler => {
            let mut fx = doubler::Doubler::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Exciter => {
            let mut fx = exciter::Exciter::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::DuckedSend => {
            let mut fx = ducked_send::DuckedSend::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Resonance => {
            let mut fx = resonance::Resonance::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Tuner => {
            let mut fx = tuner::Tuner::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::DeEsser => {
            let mut fx = deesser::DeEsser::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::BreathSplit => {
            let mut fx = breath_split::BreathSplit::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Plosive => {
            let mut fx = plosive::Plosive::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Multiband => {
            let mut fx = multiband::MultibandCompressor::new(sample_rate);
            for (name, value) in params {
                if let Some(value) = typed_builtin_value(effect_type, name, value) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Reverb => {
            let mut fx = reverb::Reverb::new(sample_rate);
            if let Some((_, oc_dsl::ast::ParamValue::Time(seconds))) =
                params.iter().find(|(name, _)| name == "decay")
            {
                fx.set_decay_seconds(*seconds);
            } else if let Some(&d) = param_map.get("decay") {
                fx.set_param("decay", d);
            }
            for name in [
                "mix",
                "predelay",
                "damping",
                "modulation",
                "early_late",
                "stereo_width",
                "low_cut",
                "high_cut",
            ] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::ReverbFdn => {
            let mut fx = reverb_fdn::ReverbFdn::new(sample_rate);
            for name in [
                "decay",
                "space",
                "decay_rate",
                "brightness",
                "character",
                "distance",
                "predelay",
                "stereo_width",
                "mix",
                "damping",
                "modulation",
                "early_late",
                "decay_band1",
                "decay_band2",
                "decay_band3",
                "decay_band4",
                "decay_band5",
                "decay_band6",
                "post_eq1_freq",
                "post_eq1_gain",
                "post_eq1_q",
                "post_eq1_shape",
                "post_eq2_freq",
                "post_eq2_gain",
                "post_eq2_q",
                "post_eq2_shape",
                "post_eq3_freq",
                "post_eq3_gain",
                "post_eq3_q",
                "post_eq3_shape",
                "post_eq4_freq",
                "post_eq4_gain",
                "post_eq4_q",
                "post_eq4_shape",
                "post_eq5_freq",
                "post_eq5_gain",
                "post_eq5_q",
                "post_eq5_shape",
                "post_eq6_freq",
                "post_eq6_gain",
                "post_eq6_q",
                "post_eq6_shape",
                "decay_band1_freq",
                "decay_band1_q",
                "decay_band1_shape",
                "decay_band2_freq",
                "decay_band2_q",
                "decay_band2_shape",
                "decay_band3_freq",
                "decay_band3_q",
                "decay_band3_shape",
                "decay_band4_freq",
                "decay_band4_q",
                "decay_band4_shape",
                "decay_band5_freq",
                "decay_band5_q",
                "decay_band5_shape",
                "decay_band6_freq",
                "decay_band6_q",
                "decay_band6_shape",
            ] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Delay => {
            let mut fx = delay::StereoDelay::new(sample_rate);
            if let Some(&t) = param_map.get("time") {
                fx.set_param("time", t);
            }
            for name in ["time_l", "time_r"] {
                if let Some(&time) = param_map.get(name) {
                    fx.set_param(name, time);
                }
            }
            if let Some(&fb) = param_map.get("feedback") {
                fx.set_param("feedback", fb);
            }
            if let Some(&m) = param_map.get("mix") {
                fx.set_param("mix", m);
            }
            Box::new(fx)
        }
        EffectType::Limiter => {
            let mut fx = limiter::Limiter::new(sample_rate);
            for name in [
                "ceiling",
                "release",
                "release_mode",
                "lookahead",
                "true_peak",
                "oversampling",
                "link",
            ] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Chorus => {
            let mut fx = chorus::Chorus::new(sample_rate);
            if let Some(&r) = param_map.get("rate") {
                fx.set_param("rate", r);
            }
            if let Some(&d) = param_map.get("depth") {
                fx.set_param("depth", d);
            }
            if let Some(&m) = param_map.get("mix") {
                fx.set_param("mix", m);
            }
            Box::new(fx)
        }
        EffectType::Distortion => {
            let mut fx = distortion::Distortion::new();
            for name in ["drive", "tone", "output", "mix"] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Flanger => {
            let mut fx = flanger::Flanger::new(sample_rate);
            for name in ["rate", "depth", "feedback", "mix"] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Phaser => {
            let mut fx = phaser::Phaser::new(sample_rate);
            for name in ["rate", "depth", "feedback", "mix"] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::Gate => {
            let mut fx = gate::Gate::new(sample_rate);
            for name in ["threshold", "attack", "hold", "release", "range"] {
                if let Some(&value) = param_map.get(name) {
                    fx.set_param(name, value);
                }
            }
            Box::new(fx)
        }
        EffectType::StereoWidth => {
            let mut fx = stereo_width::StereoWidth::new();
            if let Some(&value) = param_map.get("amount") {
                fx.set_param("amount", value);
            }
            Box::new(fx)
        }
        EffectType::Plugin(plugin_id) => {
            create_plugin_effect(plugin_id, &param_map, sample_rate, "")
                .unwrap_or_else(|_| Box::new(Passthrough))
        }
    }
}

/// Effect parameter that selects a declared sidechain input by index.
#[cfg(feature = "plugin-host")]
const SIDECHAIN_INPUT: &str = "sidechain_input";

#[cfg(feature = "plugin-host")]
fn sidechain_input_index(params: &HashMap<&str, f64>) -> Option<usize> {
    params
        .get(SIDECHAIN_INPUT)
        .map(|value| value.max(0.0) as usize)
}

#[cfg(feature = "plugin-host")]
fn apply_sidechain_input(
    params: &HashMap<&str, f64>,
    instance: &mut oc_host::clap_instance::ClapInstance,
) {
    let Some(index) = sidechain_input_index(params) else {
        return;
    };
    if index < instance.sidechain_input_count() {
        instance.set_sidechain_input_index(index);
    }
}

#[cfg(feature = "plugin-host")]
fn apply_sidechain_input_vst3(
    params: &HashMap<&str, f64>,
    instance: &mut oc_host::vst3_instance::Vst3Instance,
) {
    let Some(index) = sidechain_input_index(params) else {
        return;
    };
    if index < instance.sidechain_input_count() {
        instance.set_sidechain_input_index(index);
    }
}

/// Refuse a plugin that declares outputs but no audio input channel.
///
/// An insert has one stereo input: such a plugin could never be fed, and its
/// own input-buffer contract has nothing to describe. The message names the
/// plugin, its declared topology and the chain it sits in.
#[cfg(feature = "plugin-host")]
fn refuse_instrument_insert(
    name: &str,
    topology: oc_host::ports::BusTopology,
    place: &str,
) -> Result<(), PluginEffectError> {
    if !topology.is_instrument() {
        return Ok(());
    }
    let at = if place.is_empty() {
        String::new()
    } else {
        format!(" on {place}")
    };
    Err(PluginEffectError::Refused(format!(
        "{name}: an instrument plugin ({inputs}, {outputs}) cannot be used as an effect{at}; put it \
         on an instrument track, or wrap it in a bus",
        inputs = count_noun(
            topology.input_channels() as usize,
            "audio input",
            "audio inputs"
        ),
        outputs = count_noun(topology.output_pairs(), "stereo output", "stereo outputs"),
    )))
}

#[cfg(feature = "plugin-host")]
fn count_noun(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

#[cfg(feature = "plugin-host")]
fn create_plugin_effect(
    plugin_id: &str,
    params: &HashMap<&str, f64>,
    sample_rate: u32,
    place: &str,
) -> Result<Box<dyn AudioEffect>, PluginEffectError> {
    let (required_format, plugin_id) = if let Some(id) = plugin_id.strip_prefix("clap.") {
        (Some(oc_audio::PluginFormat::Clap), id)
    } else if let Some(id) = plugin_id.strip_prefix("vst3.") {
        (Some(oc_audio::PluginFormat::Vst3), id)
    } else if let Some(id) = plugin_id.strip_prefix("au.") {
        (Some(oc_audio::PluginFormat::Au), id)
    } else if let Some(id) = plugin_id.strip_prefix("lv2.") {
        (Some(oc_audio::PluginFormat::Lv2), id)
    } else if let Some(id) = plugin_id.strip_prefix("vst2.") {
        (Some(oc_audio::PluginFormat::Vst2), id)
    } else {
        (None, plugin_id)
    };
    // In-process fake components are never installed as bundles.
    #[cfg(any(test, feature = "test-support"))]
    if required_format.is_none_or(|format| format == oc_audio::PluginFormat::Vst3)
        && let Some(mut instance) =
            oc_host::vst3_instance::test_support::fake_instance_for_id(plugin_id, 512)
    {
        refuse_instrument_insert(
            instance.name(),
            oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
            place,
        )?;
        for (name, value) in params.iter().filter(|(name, _)| **name != SIDECHAIN_INPUT) {
            instance.set_param(name, *value);
        }
        return Ok(Box::new(instance));
    }
    if required_format == Some(oc_audio::PluginFormat::Au) {
        #[cfg(target_os = "macos")]
        {
            let mut instance = oc_host::au_instance::AuInstance::load(plugin_id, sample_rate, 512)
                .ok()
                .ok_or(PluginEffectError::Unavailable)?;
            refuse_instrument_insert(
                instance.name(),
                oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
                place,
            )?;
            if !instance.host_callbacks_installed() {
                crate::render_warnings::push(format!(
                    "AU component '{}' does not accept host tempo callbacks; its tempo-synced internals keep a constant context",
                    instance.component_name()
                ));
            }
            for (name, value) in params.iter().filter(|(name, _)| **name != SIDECHAIN_INPUT) {
                instance.set_param_by_name(name, *value);
            }
            return Ok(Box::new(instance));
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(PluginEffectError::Unavailable);
        }
    }
    let info = if required_format == Some(oc_audio::PluginFormat::Lv2) {
        oc_host::find_plugin_info(plugin_id, oc_audio::PluginFormat::Lv2)
    } else {
        oc_host::cached_plugins()
            .iter()
            .find(|plugin| {
                matches!(
                    plugin.format,
                    oc_audio::PluginFormat::Clap
                        | oc_audio::PluginFormat::Vst3
                        | oc_audio::PluginFormat::Lv2
                        | oc_audio::PluginFormat::Vst2
                ) && required_format.is_none_or(|format| plugin.format == format)
                    && (plugin.id == plugin_id
                        || plugin.name == plugin_id
                        || plugin.path == plugin_id)
            })
            .cloned()
    }
    .ok_or(PluginEffectError::Unavailable)?;

    match info.format {
        oc_audio::PluginFormat::Clap => {
            let owned_params: Vec<(String, f64)> = params
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect();
            let instance = oc_host::clap_instance::ClapInstance::load_prepared_on_main(
                std::path::Path::new(&info.path),
                &info.id,
                sample_rate,
                512,
                move |instance| {
                    let borrowed: HashMap<&str, f64> = owned_params
                        .iter()
                        .map(|(name, value)| (name.as_str(), *value))
                        .collect();
                    for (name, value) in borrowed
                        .iter()
                        .filter(|(name, _)| **name != SIDECHAIN_INPUT)
                    {
                        instance.set_param(name, *value);
                    }
                    apply_sidechain_input(&borrowed, instance);
                    Ok(())
                },
            )
            .map_err(|error| {
                PluginEffectError::Refused(format!("CLAP effect '{}': {error}", info.name))
            })?;
            refuse_instrument_insert(
                &info.name,
                oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
                place,
            )?;

            Ok(Box::new(instance))
        }
        oc_audio::PluginFormat::Vst3 => {
            let mut instance = oc_host::vst3_instance::Vst3Instance::load(
                std::path::Path::new(&info.path),
                sample_rate,
                512,
            )
            .ok()
            .ok_or(PluginEffectError::Unavailable)?;
            refuse_instrument_insert(
                &info.name,
                oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
                place,
            )?;

            for (name, value) in params.iter().filter(|(name, _)| **name != SIDECHAIN_INPUT) {
                instance.set_param(name, *value);
            }
            apply_sidechain_input_vst3(params, &mut instance);

            Ok(Box::new(instance))
        }
        oc_audio::PluginFormat::Lv2 => {
            let mut instance = oc_host::lv2::Lv2Instance::load(
                std::path::Path::new(&info.path),
                &info.id,
                sample_rate,
                512,
            )
            .ok()
            .ok_or(PluginEffectError::Unavailable)?;
            refuse_instrument_insert(
                &info.name,
                oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
                place,
            )?;

            for (name, value) in params.iter().filter(|(name, _)| **name != SIDECHAIN_INPUT) {
                instance.set_param(name, *value);
            }

            Ok(Box::new(instance))
        }
        oc_audio::PluginFormat::Vst2 => {
            let mut instance = oc_host::vst2::Vst2Instance::load(
                std::path::Path::new(&info.path),
                sample_rate,
                512,
            )
            .ok()
            .ok_or(PluginEffectError::Unavailable)?;
            refuse_instrument_insert(
                &info.name,
                oc_host::ports::BusTopology::new(instance.input_ports(), instance.output_ports()),
                place,
            )?;

            for (name, value) in params.iter().filter(|(name, _)| **name != SIDECHAIN_INPUT) {
                instance.set_param(name, *value);
            }

            Ok(Box::new(instance))
        }
        _ => Err(PluginEffectError::Unavailable),
    }
}
#[cfg(not(feature = "plugin-host"))]
fn create_plugin_effect(
    _plugin_id: &str,
    _params: &HashMap<&str, f64>,
    _sample_rate: u32,
    _place: &str,
) -> Result<Box<dyn AudioEffect>, PluginEffectError> {
    Err(PluginEffectError::Unavailable)
}

/// No-op effect (passthrough).
struct Passthrough;

impl AudioEffect for Passthrough {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Passthrough"
    }
}

#[test]
fn dynamic_eq_band_three_static_and_dynamic_gain_reach_the_dsp() {
    use oc_dsl::ast::{EffectType as E, ParamValue as P};
    const SR: u32 = 48_000;

    fn sine(freq: f64, amplitude: f64, frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let value = (std::f64::consts::TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
            buffer[frame * 2] = value as f32;
            buffer[frame * 2 + 1] = value as f32;
        }
        buffer
    }

    fn rms(buffer: &[f32], start: usize) -> f64 {
        let samples = &buffer[start * 2..];
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    let mut static_band = try_create_effect(
        &E::DynamicEq,
        &[
            ("band3_enabled".into(), P::Bool(true)),
            ("band3_type".into(), P::String("bell".into())),
            ("band3_freq".into(), P::Frequency(1_000.0)),
            ("band3_gain".into(), P::Db(6.0)),
            ("band3_q".into(), P::Number(1.0)),
        ],
        SR,
    )
    .unwrap();
    let mut buffer = sine(1_000.0, 0.1, SR as usize);
    let input_rms = rms(&buffer, 0);
    static_band.process(&mut buffer, SR);
    let boosted = rms(&buffer, SR as usize / 2);
    let gain_db = 20.0 * (boosted / input_rms).log10();
    assert!(
        (gain_db - 6.0).abs() < 0.5,
        "band 3 static gain was {gain_db} dB"
    );

    let mut dynamic_band = try_create_effect(
        &E::DynamicEq,
        &[
            ("band3_enabled".into(), P::Bool(true)),
            ("band3_dynamic".into(), P::Bool(true)),
            ("band3_freq".into(), P::Frequency(1_000.0)),
            ("band3_q".into(), P::Number(1.0)),
            ("band3_threshold".into(), P::Db(-30.0)),
            ("band3_range".into(), P::Db(-12.0)),
            ("band3_attack".into(), P::Time(0.005)),
            ("band3_release".into(), P::Time(0.050)),
        ],
        SR,
    )
    .unwrap();
    let mut buffer = sine(1_000.0, 0.5, SR as usize);
    let input_rms = rms(&buffer, 0);
    dynamic_band.process(&mut buffer, SR);
    let reduced = rms(&buffer, SR as usize / 2);
    assert!(
        reduced < input_rms * 0.5,
        "band 3 dynamic range did not attenuate: {reduced} vs {input_rms}"
    );
    assert!(
        dynamic_band.gain_reduction_db() > 3.0,
        "meter reported {} dB",
        dynamic_band.gain_reduction_db()
    );
}

#[cfg(test)]
mod accepted_spelling_tests {
    use super::*;

    /// An enum refusal must suggest a spelling the DSL can parse. Option names
    /// with spaces cannot be written there, so the suggestion is an index.
    #[test]
    fn an_enum_refusal_suggests_an_index() {
        let device = schema_bridge::device_for_effect(&oc_dsl::ast::EffectType::Multiband)
            .expect("the multiband device resolves");
        let param = device.param("band1_detector").expect("the detector param");
        let text = accepted_spelling("band1_detector", param);
        assert!(text.contains("written by index"), "{text}");
        assert!(text.contains("band1_detector=0"), "{text}");
    }
}
