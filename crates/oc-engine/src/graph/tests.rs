use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn checked_builtin_sidechain_drives_compressor_and_rejects_ignored_keys() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let mut effect = Effect {
        effect_type: EffectType::Compression,
        params: vec![
            ("threshold".into(), ParamValue::Db(-24.0)),
            ("ratio".into(), ParamValue::String("4:1".into())),
            ("attack".into(), ParamValue::Time(0.0001)),
        ],
        bypass: false,
        sidechain_source: Some("kick".into()),
    };
    let mut chain = build_effect_chain(
        &[effect.clone()],
        &[],
        &[],
        EffectChainBuild {
            sample_rate: 48000,
            checked: true,
            properties: &Default::default(),
            owner: "",
            place: "",
            touch: None,
        },
    )
    .unwrap();
    let mut audio = vec![0.05; 9600];
    let taps = HashMap::from([("kick".into(), vec![1.0; 9600])]);
    chain.process_with_sidechains(&mut audio, &taps, 48000, None);
    assert!(audio[9599] < 0.01);
    effect.effect_type = EffectType::Gain;
    effect.params = vec![("factor".into(), ParamValue::Number(1.0))];
    let failure = build_effect_chain(
        &[effect],
        &[],
        &[],
        EffectChainBuild {
            sample_rate: 48000,
            checked: true,
            properties: &Default::default(),
            owner: "",
            place: "",
            touch: None,
        },
    )
    .err()
    .unwrap();
    assert!(failure.contains("refusing ignored key"));
}

#[test]
fn checked_dynamic_eq_sidechain_routes_the_key_into_the_detector() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let effect = Effect {
        effect_type: EffectType::DynamicEq,
        params: vec![
            ("band1_enabled".into(), ParamValue::Bool(true)),
            ("band1_type".into(), ParamValue::String("bell".into())),
            ("band1_freq".into(), ParamValue::Frequency(1_000.0)),
            ("band1_q".into(), ParamValue::Number(1.0)),
            ("band1_gain".into(), ParamValue::Db(0.0)),
            ("band1_dynamic".into(), ParamValue::Bool(true)),
            ("band1_sidechain".into(), ParamValue::Bool(true)),
            ("band1_threshold".into(), ParamValue::Db(-40.0)),
            ("band1_range".into(), ParamValue::Db(-12.0)),
            ("band1_attack".into(), ParamValue::Time(0.005)),
            ("band1_release".into(), ParamValue::Time(0.1)),
        ],
        bypass: false,
        sidechain_source: Some("kick".into()),
    };
    let mut chain = build_effect_chain(
        &[effect],
        &[],
        &[],
        EffectChainBuild {
            sample_rate: 48000,
            checked: true,
            properties: &Default::default(),
            owner: "",
            place: "",
            touch: None,
        },
    )
    .unwrap();
    let frames = 24_000;
    let mut main: Vec<f32> = (0..frames)
        .flat_map(|frame| {
            let value =
                (std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin() as f32 * 0.001;
            [value, value]
        })
        .collect();
    let original = main.clone();
    let silent = vec![0.0f32; frames * 2];
    chain.process_with_sidechains(
        &mut main,
        &HashMap::from([("kick".into(), silent)]),
        48000,
        None,
    );
    assert_eq!(main, original, "silent key moved the audio path");
    assert_eq!(chain.effects[0].band_gain_db(0), Some(0.0));

    chain.reset();
    let loud: Vec<f32> = (0..frames)
        .flat_map(|frame| {
            let value = (std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin() as f32;
            [value, value]
        })
        .collect();
    let mut ducked = original.clone();
    chain.process_with_sidechains(
        &mut ducked,
        &HashMap::from([("kick".into(), loud)]),
        48000,
        None,
    );
    assert!(
        chain.effects[0]
            .band_gain_db(0)
            .is_some_and(|gain| gain < -11.5),
        "key did not drive the detector: {:?}",
        chain.effects[0].band_gain_db(0)
    );
    let input_rms = (original
        .iter()
        .map(|sample| (*sample as f64).powi(2))
        .sum::<f64>()
        / original.len() as f64)
        .sqrt();
    let output_rms = (ducked
        .iter()
        .map(|sample| (*sample as f64).powi(2))
        .sum::<f64>()
        / ducked.len() as f64)
        .sqrt();
    assert!(
        output_rms < input_rms * 0.35,
        "main path was not ducked: {input_rms:.5} -> {output_rms:.5}"
    );
}

#[test]
fn previously_dsp_less_mixer_effects_now_change_graph_output() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let cases = [
        (
            EffectType::Distortion,
            vec![
                ("drive".into(), ParamValue::Percentage(80.0)),
                ("tone".into(), ParamValue::Percentage(30.0)),
            ],
        ),
        (
            EffectType::Flanger,
            vec![
                ("depth".into(), ParamValue::Time(0.004)),
                ("feedback".into(), ParamValue::Percentage(60.0)),
            ],
        ),
        (
            EffectType::Phaser,
            vec![
                ("depth".into(), ParamValue::Percentage(80.0)),
                ("feedback".into(), ParamValue::Percentage(50.0)),
            ],
        ),
        (
            EffectType::Gate,
            vec![
                ("threshold".into(), ParamValue::Db(0.0)),
                ("range".into(), ParamValue::Db(-60.0)),
            ],
        ),
        (
            EffectType::StereoWidth,
            vec![("amount".into(), ParamValue::Percentage(50.0))],
        ),
    ];
    for (effect_type, params) in cases {
        let effect = Effect {
            effect_type: effect_type.clone(),
            params,
            bypass: false,
            sidechain_source: None,
        };
        let mut chain = EffectChain::try_from_effects(&[effect], 48000)
            .unwrap_or_else(|error| panic!("{effect_type:?}: {error}"));
        assert_eq!(chain.effects.len(), 1);
        let original: Vec<f32> = (0..4800)
            .flat_map(|i| {
                let left =
                    (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 48000.0).sin() as f32 * 0.25;
                let right =
                    (2.0 * std::f64::consts::PI * 330.0 * i as f64 / 48000.0).sin() as f32 * 0.25;
                [left, right]
            })
            .collect();
        let mut audio = original.clone();
        chain.process(&mut audio, 48000);
        assert!(
            audio.iter().all(|value| value.is_finite()),
            "{effect_type:?}"
        );
        let changed = audio
            .iter()
            .zip(&original)
            .filter(|(after, before)| (*after - *before).abs() > 1e-4)
            .count();
        assert!(changed > 100, "{effect_type:?} changed {changed} samples");
    }
}

#[test]
fn resonance_chain_reports_the_stft_latency() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let effect = Effect {
        effect_type: EffectType::Resonance,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[effect], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 2_304);

    let bypass = Effect {
        effect_type: EffectType::Resonance,
        params: vec![("mix".into(), ParamValue::Percentage(0.0))],
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[bypass], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 0);
}

#[test]
fn a_bypassed_effect_is_absent_from_the_render_and_the_latency() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};

    let effect = Effect {
        effect_type: EffectType::Compression,
        params: vec![
            ("threshold".into(), ParamValue::Db(-45.0)),
            ("ratio".into(), ParamValue::String("8:1".into())),
            ("attack".into(), ParamValue::Time(0.001)),
            ("release".into(), ParamValue::Time(0.05)),
            ("makeup".into(), ParamValue::Db(12.0)),
        ],
        bypass: false,
        sidechain_source: None,
    };
    let signal: Vec<f32> = (0..2_048)
        .flat_map(|frame| {
            let value = ((frame as f64) * 0.017).sin() as f32 * 0.4;
            [value, value]
        })
        .collect();

    let mut active_chain =
        EffectChain::try_from_effects(std::slice::from_ref(&effect), 48_000).unwrap();
    let mut active = signal.clone();
    active_chain.process(&mut active, 48_000);
    assert_ne!(
        active, signal,
        "the active compressor must change the signal"
    );

    let mut bypassed_chain = EffectChain::try_from_effects(
        &[Effect {
            bypass: true,
            ..effect
        }],
        48_000,
    )
    .unwrap();
    let mut bypassed = signal.clone();
    bypassed_chain.process(&mut bypassed, 48_000);

    let mut absent_chain = EffectChain::new();
    let mut absent = signal.clone();
    absent_chain.process(&mut absent, 48_000);

    assert_eq!(
        bypassed, signal,
        "a bypassed effect must not touch a sample"
    );
    assert_eq!(
        bypassed, absent,
        "a bypassed effect and an absent one must match bit for bit"
    );
    assert_eq!(bypassed_chain.latency_samples(), 0);
    assert_eq!(bypassed_chain.tail_samples(), 0);
}

#[test]
fn bypassing_an_effect_recomputes_the_chain_latency() {
    use oc_dsl::ast::{Effect, EffectType};

    let resonance = Effect {
        effect_type: EffectType::Resonance,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let gain = Effect {
        effect_type: EffectType::Gain,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[gain.clone(), resonance.clone()], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 2_304);

    let bypassed = EffectChain::try_from_effects(
        &[
            gain,
            Effect {
                bypass: true,
                ..resonance
            },
        ],
        48_000,
    )
    .unwrap();
    assert_eq!(
        bypassed.effects.len(),
        2,
        "the slot stays so meters keep their index"
    );
    assert_eq!(bypassed.latency_samples(), 0);
}

#[test]
fn a_chain_where_every_effect_is_bypassed_equals_a_dry_render() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};

    let effect = |effect_type: EffectType, params: Vec<(String, ParamValue)>| Effect {
        effect_type,
        params,
        bypass: true,
        sidechain_source: None,
    };
    let effects = vec![
        effect(
            EffectType::Delay,
            vec![
                ("time_l".into(), ParamValue::Time(0.01)),
                ("mix".into(), ParamValue::Percentage(100.0)),
            ],
        ),
        effect(
            EffectType::Reverb,
            vec![("mix".into(), ParamValue::Percentage(100.0))],
        ),
        effect(EffectType::Compression, Vec::new()),
    ];
    let mut chain = EffectChain::try_from_effects(&effects, 48_000).unwrap();
    assert_eq!(
        chain.effects.len(),
        3,
        "every bypassed entry keeps its slot"
    );
    assert_eq!(chain.latency_samples(), 0);
    assert_eq!(chain.tail_samples(), 0);

    let signal: Vec<f32> = (0..2_048)
        .flat_map(|frame| {
            let value = ((frame as f64) * 0.011).sin() as f32 * 0.3;
            [value, value]
        })
        .collect();
    let mut audio = signal.clone();
    chain.process(&mut audio, 48_000);
    assert_eq!(audio, signal, "an all-bypassed chain is a dry render");
}

#[test]
fn tuner_chain_reports_the_psola_delay() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let effect = Effect {
        effect_type: EffectType::Tuner,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[effect], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 1_920);

    let low = Effect {
        effect_type: EffectType::Tuner,
        params: vec![("mode".into(), ParamValue::String("Low Latency".into()))],
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[low], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 960);

    let bypass = Effect {
        effect_type: EffectType::Tuner,
        params: vec![("amount".into(), ParamValue::Percentage(0.0))],
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[bypass], 48_000).unwrap();
    assert_eq!(chain.latency_samples(), 0);
}

#[test]
fn deesser_changes_graph_output() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let cases = [(
        EffectType::DeEsser,
        vec![
            ("threshold".into(), ParamValue::Db(-50.0)),
            ("range".into(), ParamValue::Db(24.0)),
            ("attack".into(), ParamValue::Time(0.001)),
            ("release".into(), ParamValue::Time(0.05)),
            ("high_frequency".into(), ParamValue::Frequency(6_000.0)),
            ("low_frequency".into(), ParamValue::Frequency(12_000.0)),
            (
                "band_processing".into(),
                ParamValue::String("split_band".into()),
            ),
        ],
        8_000.0,
    )];
    for (effect_type, params, frequency) in cases {
        let effect = Effect {
            effect_type: effect_type.clone(),
            params,
            bypass: false,
            sidechain_source: None,
        };
        let mut chain = EffectChain::try_from_effects(&[effect], 48_000)
            .unwrap_or_else(|error| panic!("{effect_type:?}: {error}"));
        let original: Vec<f32> = (0..4_800)
            .flat_map(|i| {
                let left = (2.0 * std::f64::consts::PI * frequency * i as f64 / 48_000.0).sin()
                    as f32
                    * 0.25;
                let right = (2.0 * std::f64::consts::PI * frequency * i as f64 / 48_000.0).sin()
                    as f32
                    * 0.25;
                [left, right]
            })
            .collect();
        let mut audio = original.clone();
        chain.process(&mut audio, 48_000);
        assert!(audio.iter().all(|value| value.is_finite()));
        let changed = audio
            .iter()
            .zip(&original)
            .filter(|(after, before)| (*after - *before).abs() > 1e-4)
            .count();
        assert!(changed > 100, "{effect_type:?} changed {changed} samples");
    }
}

#[test]
fn multiband_changes_graph_output() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let effect = Effect {
        effect_type: EffectType::Multiband,
        params: vec![
            ("crossover_1".into(), ParamValue::Frequency(500.0)),
            ("band1_threshold".into(), ParamValue::Db(-30.0)),
            ("band1_range".into(), ParamValue::Db(-12.0)),
            ("band1_ratio".into(), ParamValue::String("8:1".into())),
            ("band1_attack".into(), ParamValue::Time(0.001)),
            ("band1_release".into(), ParamValue::Time(0.05)),
        ],
        bypass: false,
        sidechain_source: None,
    };
    let mut chain =
        EffectChain::try_from_effects(&[effect], 48_000).expect("multiband should build");
    let original: Vec<f32> = (0..4_800)
        .flat_map(|i| {
            let value =
                (2.0 * std::f64::consts::PI * 100.0 * i as f64 / 48_000.0).sin() as f32 * 0.25;
            [value, value]
        })
        .collect();
    let mut audio = original.clone();
    chain.process(&mut audio, 48_000);
    assert!(audio.iter().all(|value| value.is_finite()));
    let changed = audio
        .iter()
        .zip(&original)
        .filter(|(after, before)| (*after - *before).abs() > 1e-4)
        .count();
    assert!(changed > 100, "multiband changed {changed} samples");
}

#[test]
fn reverb_pro_controls_change_graph_output() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    let effect = Effect {
        effect_type: EffectType::Reverb,
        params: vec![
            ("decay".into(), ParamValue::Time(2.4)),
            ("mix".into(), ParamValue::Percentage(50.0)),
            ("modulation".into(), ParamValue::Percentage(60.0)),
            ("early_late".into(), ParamValue::Percentage(40.0)),
            ("stereo_width".into(), ParamValue::Percentage(140.0)),
            ("low_cut".into(), ParamValue::Frequency(120.0)),
            ("high_cut".into(), ParamValue::Frequency(8_000.0)),
        ],
        bypass: false,
        sidechain_source: None,
    };
    let mut chain =
        EffectChain::try_from_effects(&[effect], 48_000).expect("reverb pro controls should build");
    let original: Vec<f32> = (0..9_600)
        .flat_map(|i| {
            let value =
                (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 48_000.0).sin() as f32 * 0.25;
            [value, value * 0.5]
        })
        .collect();
    let mut audio = original.clone();
    chain.process(&mut audio, 48_000);
    assert!(audio.iter().all(|value| value.is_finite()));
    let changed = audio
        .iter()
        .zip(&original)
        .filter(|(after, before)| (*after - *before).abs() > 1e-4)
        .count();
    assert!(changed > 100, "reverb changed {changed} samples");
}

struct TransportAwareEffect {
    seen: Arc<Mutex<Option<(u64, f64)>>>,
    latency: u32,
}

struct LatencyEffect {
    delay: StereoDelayCompensator,
    latency: u32,
}

struct MutableLatencyEffect {
    latency: Arc<AtomicU32>,
}

struct TailEffect(u32);

struct SidechainCaptureEffect {
    seen: Arc<Mutex<Option<(usize, usize)>>>,
}

/// Records the first non-zero sample of the key it is handed.
struct KeyProbe {
    seen: Arc<Mutex<Option<f32>>>,
    source: &'static str,
}

impl AudioEffect for KeyProbe {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
    fn process_with_sidechain(
        &mut self,
        _buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        _transport: Option<PluginTransport>,
    ) {
        *self.seen.lock().unwrap() = Some(
            sidechain
                .iter()
                .find(|sample| **sample != 0.0)
                .copied()
                .unwrap_or(0.0),
        );
    }
    fn supports_sidechain(&self) -> bool {
        true
    }
    fn sidechain_source(&self) -> Option<&str> {
        Some(self.source)
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "key-probe"
    }
}

impl AudioEffect for SidechainCaptureEffect {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        _transport: Option<PluginTransport>,
    ) {
        let main = buffer
            .chunks_exact(2)
            .position(|frame| frame[0] != 0.0)
            .unwrap();
        let key = sidechain
            .chunks_exact(2)
            .position(|frame| frame[0] != 0.0)
            .unwrap();
        *self.seen.lock().unwrap() = Some((main, key));
    }
    fn supports_sidechain(&self) -> bool {
        true
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "sidechain-capture"
    }
}

impl AudioEffect for TailEffect {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn tail_samples(&self) -> u32 {
        self.0
    }
    fn name(&self) -> &str {
        "tail-test"
    }
}

impl AudioEffect for MutableLatencyEffect {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
    fn name(&self) -> &str {
        "mutable-latency-test"
    }
}

impl LatencyEffect {
    fn new(latency: u32) -> Self {
        Self {
            delay: StereoDelayCompensator::new(latency),
            latency,
        }
    }
}

impl AudioEffect for LatencyEffect {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.delay.process(buffer);
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        self.latency
    }
    fn name(&self) -> &str {
        "latency-test"
    }
}

impl AudioEffect for TransportAwareEffect {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}

    fn process_with_transport(
        &mut self,
        _buffer: &mut [f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        *self.seen.lock().unwrap() =
            transport.map(|transport| (transport.sample_pos, transport.tempo));
    }

    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        self.latency
    }
    fn name(&self) -> &str {
        "transport-aware"
    }
}

/// One process call as a hosted plugin would see it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SeenBlock {
    sample_pos: u64,
    frames: usize,
    tempo: f64,
    ppq: f64,
    numerator: u16,
    denominator: u16,
}

/// Stand-in for a hosted plugin: records the clock context of every call.
struct RecordingEffect {
    seen: Arc<Mutex<Vec<SeenBlock>>>,
    latency: u32,
}

impl RecordingEffect {
    fn record(&self, buffer: &[f32], transport: Option<PluginTransport>) {
        let transport = transport.expect("mapped chain must pass a transport");
        let (numerator, denominator, _, _) = transport.musical_position();
        self.seen.lock().unwrap().push(SeenBlock {
            sample_pos: transport.sample_pos,
            frames: buffer.len() / 2,
            tempo: transport.current_tempo(),
            ppq: transport.quarter_notes(),
            numerator,
            denominator,
        });
    }
}

impl AudioEffect for RecordingEffect {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {
        panic!("mapped chain must call process_with_transport");
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.record(buffer, transport);
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        self.latency
    }
    fn name(&self) -> &str {
        "transport-recorder"
    }
}

fn recording_chain() -> (EffectChain, Arc<Mutex<Vec<SeenBlock>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![Box::new(RecordingEffect {
            seen: seen.clone(),
            latency: 0,
        })],
    };
    (chain, seen)
}

fn mapped_transport(project: &oc_dsl::ast::Project, sample_pos: u64, rate: u32) -> PluginTransport {
    PluginTransport::new(sample_pos, rate, project.properties.tempo).with_clock(Arc::new(
        oc_dsl::timing::ProjectClock::new(&project.properties).expect("valid map"),
    ))
}

fn blocks(seen: &Arc<Mutex<Vec<SeenBlock>>>) -> Vec<SeenBlock> {
    seen.lock().unwrap().clone()
}

#[test]
fn mapped_effect_splits_mid_block_at_a_tempo_change() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n").unwrap();
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 47_936, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].sample_pos, 47_936);
    assert_eq!(seen[0].frames, 64);
    assert_eq!(seen[0].tempo, 120.0);
    assert_eq!(seen[1].sample_pos, 48_000);
    assert_eq!(seen[1].frames, 64);
    assert_eq!(seen[1].tempo, 60.0);
    // PPQ is continuous across the seam: the first block ends exactly where
    // the second begins, and the slower tempo advances it half as fast.
    assert!((seen[1].ppq - 2.0).abs() < 1e-9);
    assert_eq!(seen[0].numerator, 4);
    assert_eq!(seen[1].numerator, 4);
}

#[test]
fn mapped_effect_splits_mid_block_at_a_meter_change() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  time_sig_map:\n    2:1 = 3/4\n").unwrap();
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 95_936, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].frames, 64);
    assert_eq!((seen[0].numerator, seen[0].denominator), (4, 4));
    assert!((seen[0].ppq - 3.997_333_333_333_333_5).abs() < 1e-9);
    assert_eq!(seen[1].frames, 64);
    assert_eq!((seen[1].numerator, seen[1].denominator), (3, 4));
    assert!((seen[1].ppq - 4.0).abs() < 1e-9);
}

#[test]
fn mapped_effect_applies_a_boundary_change_to_the_next_block_only() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n  time_sig_map:\n    2:1 = 3/4\n",
    )
    .unwrap();
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    // The first call ends exactly at the tempo change, so it keeps the old
    // tempo for its whole length.
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 47_936, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!((seen[0].tempo, seen[0].numerator), (120.0, 4));
    assert_eq!((seen[1].tempo, seen[1].numerator), (60.0, 4));

    // A block starting on the boundary has no zero-length sub-block: the
    // first call already sees the new tempo and meter.
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 48_000, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].sample_pos, 48_000);
    assert_eq!(seen[0].frames, 128);
    assert_eq!(seen[0].tempo, 60.0);
}

#[test]
fn mapped_effect_splits_when_tempo_and_meter_change_together() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n  time_sig_map:\n    2:1 = 3/4\n",
    )
    .unwrap();
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 95_936, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].frames, 64);
    assert_eq!((seen[0].tempo, seen[0].numerator), (120.0, 4));
    assert_eq!(seen[1].frames, 64);
    assert_eq!((seen[1].tempo, seen[1].numerator), (60.0, 3));
    assert!((seen[1].ppq - 4.0).abs() < 1e-9);
}

#[test]
fn mapped_effect_applies_a_first_sample_change_to_that_block() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n").unwrap();
    let (mut chain, seen) = recording_chain();
    let mut buffer = vec![0.0; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 24_000, 48_000)),
    );
    let seen = blocks(&seen);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].tempo, 60.0);
    assert_eq!(seen[0].ppq, 1.0);
}

#[test]
fn mapped_effect_ramp_keeps_contiguous_monotonic_blocks() {
    let mut map = String::new();
    for beat in 1..=4u32 {
        for step in 1..=3u32 {
            let bpm = 110 + (beat - 1) * 3 + step;
            map.push_str(&format!(
                "    1:{beat} + {}q = {bpm} bpm\n",
                f64::from(step) * 0.0625
            ));
        }
    }
    let project = oc_dsl::parse(&format!("@project\n  tempo: 120\n  tempo_map:\n{map}")).unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).expect("valid ramp");
    let (mut chain, seen) = recording_chain();
    let frames = 48_000;
    let mut buffer = vec![0.0; frames * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 0, 48_000)),
    );
    let seen = blocks(&seen);
    let mut position = 0usize;
    let mut previous_ppq = f64::NEG_INFINITY;
    let mut shortened = false;
    for block in &seen {
        assert_eq!(block.sample_pos, position as u64);
        assert!(block.frames > 0 && block.frames <= 128, "{block:?}");
        shortened |= block.frames < 128;
        assert!(block.ppq > previous_ppq, "{block:?}");
        previous_ppq = block.ppq;
        let expected = clock.position_at_sample(block.sample_pos, 48_000).unwrap();
        assert_eq!(block.tempo, expected.bpm, "{block:?}");
        position += block.frames;
    }
    assert_eq!(position, frames);
    assert!(shortened, "the ramp must split at least one block");
}

#[test]
fn mapped_effect_reports_latency_without_changing_clock_blocks() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![Box::new(RecordingEffect {
            seen: seen.clone(),
            latency: 64,
        })],
    };
    let mut buffer = vec![0.0; 256 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 47_872, 48_000)),
    );
    assert_eq!(chain.latency_samples(), 64);
    let seen = blocks(&seen);
    assert_eq!(seen.iter().map(|block| block.frames).sum::<usize>(), 256);
    assert_eq!(seen.first().unwrap().sample_pos, 47_872);
    assert_eq!(seen.last().unwrap().tempo, 60.0);
}

#[test]
fn hosted_vst3_effect_receives_split_clock_blocks() {
    use oc_host::vst3_instance::test_support::{FakeControl, fake_effect_instance};

    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n").unwrap();
    let control = Arc::new(FakeControl::default());
    let instance = fake_effect_instance(&control, 2, 512);
    let mut chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![Box::new(instance)],
    };
    let mut buffer = vec![0.1; 128 * 2];
    chain.process_with_transport(
        &mut buffer,
        48_000,
        Some(mapped_transport(&project, 47_936, 48_000)),
    );
    let contexts = control.contexts.lock().unwrap();
    assert_eq!(contexts.len(), 2, "{contexts:?}");
    assert_eq!(contexts[0].num_samples, 64);
    assert_eq!(contexts[0].project_time_samples, 47_936);
    assert!((contexts[0].tempo - 120.0).abs() < 1e-9);
    assert_eq!(contexts[1].num_samples, 64);
    assert_eq!(contexts[1].project_time_samples, 48_000);
    assert!((contexts[1].tempo - 60.0).abs() < 1e-9);
    // The plugin sees continuous musical time and the meter in force.
    assert!((contexts[1].project_time_music - 2.0).abs() < 1e-9);
    assert_eq!(contexts[1].time_sig_numerator, 4);
    assert_eq!(contexts[1].time_sig_denominator, 4);
    assert!(contexts[1].playing);
}

/// The streaming path builds its chains through this helper, so it must
/// refuse the instrument shape with the message minus the track clause.
#[test]
fn the_streaming_chain_refuses_an_instrument_insert() {
    use oc_dsl::ast::{Effect, EffectType};

    let effect = Effect {
        effect_type: EffectType::Plugin("test.fake.two_bus".into()),
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let error = EffectChain::try_from_effects(&[effect], 48_000)
        .err()
        .expect("an instrument must not load as a streaming insert");
    assert_eq!(
        error,
        "VST3 Plugin: an instrument plugin (0 audio inputs, 2 stereo outputs) cannot be \
             used as an effect; put it on an instrument track, or wrap it in a bus"
    );
}

#[test]
fn the_streaming_chain_still_loads_a_stereo_insert() {
    use oc_dsl::ast::{Effect, EffectType};

    let effect = Effect {
        effect_type: EffectType::Plugin("test.fake.effect".into()),
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let mut chain = EffectChain::try_from_effects(&[effect], 48_000)
        .expect("a stereo insert must load on the streaming path");
    let original = vec![0.25f32; 512 * 2];
    let mut audio = original.clone();
    chain.process(&mut audio, 48_000);
    assert!(
        audio
            .iter()
            .zip(&original)
            .any(|(after, before)| (*after - *before).abs() > 1e-4),
        "the stereo insert left the stream unchanged"
    );
}

#[test]
fn effect_chain_forwards_transport_context() {
    let seen = Arc::new(Mutex::new(None));
    let mut chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![Box::new(TransportAwareEffect {
            seen: seen.clone(),
            latency: 0,
        })],
    };
    let mut buffer = vec![0.0; 16];

    chain.process_with_transport(
        &mut buffer,
        44_100,
        Some(PluginTransport::new(44_100, 44_100, 120.0)),
    );

    assert_eq!(*seen.lock().unwrap(), Some((44_100, 120.0)));
}

#[test]
fn sidechain_wrapper_aligns_detector_impulse_to_main_path_latency() {
    let seen = Arc::new(Mutex::new(None));
    let mut effect = SidechainEffect::new(
        Box::new(SidechainCaptureEffect { seen: seen.clone() }),
        "kick".into(),
    );
    effect.configure_sidechain_alignment(4, 1);
    let mut main = vec![0.0; 16];
    main[8] = 1.0;
    let mut key = vec![0.0; 16];
    key[2] = 1.0;

    effect.process_with_sidechain(&mut main, &key, 48_000, None);

    assert_eq!(*seen.lock().unwrap(), Some((4, 4)));
    assert_eq!(effect.latency_samples(), 0);
}

/// A subset window leaves unselected track buffers empty; routing treats
/// an empty buffer as silence instead of touching it.
#[test]
fn an_empty_track_buffer_is_inert_in_the_mix() {
    let project = oc_dsl::parse(
        "@project\n\
             @track a | instrument: keys/piano\n  mix: vol=-3dB, pan=-20\n\
             @track b | instrument: keys/piano\n  mix: vol=-6dB, pan=30, send(space=-9dB)\n\
             @bus space\n  routes: [master]\n",
    )
    .unwrap();
    let frames = 64;
    let signal: Vec<f32> = (0..frames)
        .flat_map(|frame| {
            let value = frame as f32 * 0.001;
            [value, -value]
        })
        .collect();

    let mut empty_graph = AudioGraph::from_project(&project, 48_000);
    let mut empty = vec![signal.clone(), Vec::new()];
    let empty_master = empty_graph.mix_processed(&mut empty, None);
    assert!(empty[1].is_empty(), "routing must not fill an empty track");
    assert!(
        empty_graph.bus_chains["space"]
            .buffer
            .iter()
            .all(|sample| *sample == 0.0),
        "an empty track must not feed its send"
    );

    let mut zero_graph = AudioGraph::from_project(&project, 48_000);
    let mut zeros = vec![signal, vec![0.0; frames * 2]];
    let zero_master = zero_graph.mix_processed(&mut zeros, None);

    assert_eq!(empty_master, zero_master);
}

/// One tap rule for both paths: the source's post-insert buffer, before the
/// fader and before the mute gate.
#[test]
fn a_key_reads_the_source_post_insert_pre_fader_and_pre_mute_tap() {
    fn project(mute: bool) -> oc_dsl::ast::Project {
        oc_dsl::parse(&format!(
                "@project\n\
                 @bus glue\n  routes: [master]\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, sidechain=source\n\
                 @track source | instrument: keys/piano\n  mix: vol=-12dB, mute={mute}\n  effects:\n    - gain: factor=0.5\n\
                 @track consumer | instrument: keys/piano\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, sidechain=source\n"
            ))
            .unwrap()
    }

    fn levels(project: &oc_dsl::ast::Project) -> (Option<f32>, Option<f32>) {
        let mut graph = AudioGraph::from_project(project, 48_000);
        let chain_key = Arc::new(Mutex::new(None));
        let bus_key = Arc::new(Mutex::new(None));
        graph.track_chains[1].effects = vec![Box::new(KeyProbe {
            seen: chain_key.clone(),
            source: "source",
        })];
        graph.bus_chains.get_mut("glue").unwrap().chain.effects = vec![Box::new(KeyProbe {
            seen: bus_key.clone(),
            source: "source",
        })];
        let mut tracks = vec![vec![1.0_f32; 16], vec![1.0_f32; 16]];
        graph.process_track_effects(&mut tracks, None);
        graph.mix_processed(&mut tracks, None);
        (*chain_key.lock().unwrap(), *bus_key.lock().unwrap())
    }

    let (chain, bus) = levels(&project(false));
    // The chain gain halves the signal (post-insert); the -12 dB fader
    // would be 0.126 if the tap were post-fader.
    assert_eq!(chain, Some(0.5), "track chain key tap");
    assert_eq!(bus, Some(0.5), "bus key tap");
    assert_eq!(chain, bus, "the two paths must agree");

    let (chain, bus) = levels(&project(true));
    assert_eq!(chain, Some(0.5), "a muted source still feeds the key");
    assert_eq!(bus, Some(0.5), "a muted source still feeds the bus key");
}

/// A key declared in a preset only learns its host at render time, so the
/// static pass cannot refuse it; the render must say the effect runs dry.
#[test]
fn a_key_that_no_tap_can_satisfy_warns_instead_of_processing_silently() {
    let project = oc_dsl::parse(
            "@project\n\
             @preset duck\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, sidechain=glue\n\
             @bus glue\n  routes: [master]\n\
             @track bass | preset: duck, instrument: bass/electric_bass\n",
        )
        .unwrap();
    oc_dsl::validate(&project).expect("a preset key is undecidable statically");
    // The warning buffer is process-global; keep another test's drain out
    // of this test's window.
    let _serial = crate::render_warnings::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::render_warnings::clear();
    AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
        .expect("a keyless tap is a warning, not a build failure");
    let warnings = crate::render_warnings::take();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("glue") && warning.contains("bass")),
        "expected a key warning naming the bus and the track, got {warnings:?}"
    );
}

/// The source's insert chain is part of the key's arrival time, so the
/// detector must see the source's tap on the frame the destination plays.
#[test]
fn key_alignment_includes_the_source_insert_chain_latency() {
    let project = oc_dsl::parse(
            "@project\n\
             @track source | instrument: keys/piano\n\
             @track consumer | instrument: keys/piano\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, sidechain=source\n",
        )
        .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    graph.track_chains[0].effects = vec![Box::new(LatencyEffect::new(8))];
    let seen = Arc::new(Mutex::new(None));
    graph.track_chains[1].effects = vec![Box::new(SidechainEffect::new(
        Box::new(SidechainCaptureEffect { seen: seen.clone() }),
        "source".into(),
    ))];
    graph.configure_sidechain_alignment();
    let mut tracks = vec![impulse(64, 1.0), impulse(64, 1.0)];
    graph.process_track_effects(&mut tracks, None);
    assert_eq!(
        *seen.lock().unwrap(),
        Some((8, 8)),
        "the key must arrive on the frame the delayed main path plays"
    );
}

#[test]
fn a_key_cycle_is_rejected_over_the_union_graph() {
    let cases = [
        // Two buses keyed by each other.
        "@project\n@bus a\n  routes: [master]\n  effects:\n    - compression: sidechain=b\n@bus b\n  routes: [master]\n  effects:\n    - compression: sidechain=a\n",
        // An audio route and a key edge close the same loop.
        "@project\n@bus a\n  routes: [b]\n  effects:\n    - compression: sidechain=b\n@bus b\n  routes: [master]\n",
        // A tracked chain keyed by itself.
        "@project\n@track a | instrument: keys/piano\n  effects:\n    - compression: sidechain=a\n",
    ];
    for source in cases {
        let project = oc_dsl::parse(source).unwrap();
        let error = AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
            .err()
            .unwrap_or_else(|| panic!("accepted a key cycle:\n{source}"));
        assert!(
            error.contains("key cycle") || error.contains("keys itself"),
            "{error}"
        );
        assert!(error.contains("'a'"), "{error}");
        assert!(
            error.contains("'b'") || error.contains("keys itself"),
            "{error}"
        );
    }

    let self_key = oc_dsl::parse(
        "@project\n@bus a\n  routes: [master]\n  effects:\n    - compression: sidechain=a\n",
    )
    .unwrap();
    let error = AudioGraph::try_from_project_with_source_latencies(&self_key, 48_000, &[])
        .err()
        .unwrap();
    assert!(error.contains("bus 'a' keys itself"), "{error}");
}

/// No key anywhere: the graph reports no key sources and the mix keeps the
/// documented signal (gain, then the mono pan law).
#[test]
fn a_project_without_keys_renders_the_same_buffer() {
    let project = oc_dsl::parse(
        "@project\n@track a | instrument: keys/piano\n  effects:\n    - gain: factor=0.5\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    assert!(graph.sidechain_source_tracks().is_empty());
    assert!(!graph.has_track_sidechains());
    let mut tracks = vec![vec![1.0_f32; 8]];
    graph.process_track_effects(&mut tracks, None);
    let mixed = graph.mix_processed(&mut tracks, None);
    let mono = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    for frame in mixed.chunks_exact(2) {
        assert_eq!(frame, &[mono, mono][..]);
    }
}

#[test]
fn send_pan_is_unity_at_centre_and_width_aware_elsewhere() {
    assert_eq!(send_pan_gains(0.0, SourceWidth::Mono), (1.0, 1.0));
    assert_eq!(send_pan_gains(0.0, SourceWidth::Stereo), (1.0, 1.0));

    let (left, right) = send_pan_gains(-1.0, SourceWidth::Mono);
    assert!(left > 1.414 && right.abs() < 1e-6);
    let (left, right) = send_pan_gains(1.0, SourceWidth::Mono);
    assert!(left.abs() < 1e-6 && right > 1.414);

    // Stereo sends use the channel balance law: the near side stays at
    // unity and only the far side attenuates.
    assert_eq!(send_pan_gains(-1.0, SourceWidth::Stereo), (1.0, 0.0));
    assert_eq!(send_pan_gains(1.0, SourceWidth::Stereo), (0.0, 1.0));
    assert_eq!(send_pan_gains(-0.5, SourceWidth::Stereo), (1.0, 0.5));
}

#[test]
fn mixer_state_patch_updates_bus_return_fader_mute_and_solo() {
    let project = oc_dsl::parse(
            "@project\n@bus glue\n  routes: [master]\n@track a | instrument: keys/piano\n  mix: send(glue=50%)\n",
        )
        .unwrap();
    let mut patched = AudioGraph::from_project(&project, 48000);
    assert_eq!(patched.bus_chains["glue"].gain, 1.0);
    assert!(patched.bus_chains["glue"].audible);

    let mut authored = project.clone();
    authored.buses[0].volume_db = -6.0;
    authored.buses[0].mute = true;
    authored.buses[0].solo = true;
    let expected = AudioGraph::from_project(&authored, 48000);
    patched.apply_mixer_state(&authored);

    assert_eq!(
        patched.bus_chains["glue"].gain,
        expected.bus_chains["glue"].gain
    );
    assert!((patched.bus_chains["glue"].gain - 0.501_187_2).abs() < 1e-6);
    assert!(!patched.bus_chains["glue"].audible, "mute wins over solo");
    // The soloed bus keeps its input channel alive even though the return is muted.
    assert!(!patched.routing[0].mute);
}

#[test]
fn mixer_state_patch_updates_gain_pan_and_solo_without_rebuilding_chains() {
    let project = oc_dsl::parse(
        "@project\n@track a | instrument: keys/piano\n@track b | instrument: keys/piano\n",
    )
    .unwrap();
    let mut patched = AudioGraph::from_project(&project, 48000);
    let mut authored = project.clone();
    authored.tracks[0].mix.volume_db = -6.0;
    authored.tracks[0].mix.pan = -0.5;
    authored.tracks[1].mix.solo = true;
    let expected = AudioGraph::from_project(&authored, 48000);

    patched.apply_mixer_state(&authored);

    assert_eq!(patched.routing[0].volume, expected.routing[0].volume);
    assert_eq!(patched.routing[0].pan, expected.routing[0].pan);
    assert!(patched.routing[0].mute, "unsoloed track must be silenced");
    assert!(!patched.routing[1].mute);

    let mut muted = authored.clone();
    muted.tracks[1].mix.solo = false;
    muted.tracks[1].mix.mute = true;
    patched.apply_mixer_state(&muted);
    assert!(!patched.routing[0].mute);
    assert!(patched.routing[1].mute);
    assert_eq!(patched.track_chains.len(), 2);
}

#[test]
fn effect_chain_sums_reported_tails_safely() {
    let chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![Box::new(TailEffect(128)), Box::new(TailEffect(256))],
    };

    assert_eq!(chain.tail_samples(), 384);
}

#[test]
fn export_capture_omits_diagnostics_without_changing_audio() {
    let project = oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap();
    let mut full = StemCapture::default();
    let mut lean = StemCapture {
        capture_master_inputs: false,
        ..Default::default()
    };
    let input = vec![vec![0.25_f32; 128]];
    let first = AudioGraph::from_project(&project, 48000).mix_processed_with_stems(
        &mut input.clone(),
        None,
        Some(&mut full),
    );
    let second = AudioGraph::from_project(&project, 48000).mix_processed_with_stems(
        &mut input.clone(),
        None,
        Some(&mut lean),
    );
    assert_eq!(first, second);
    assert_eq!(full.tracks, lean.tracks);
    assert_eq!(full.buses, lean.buses);
    assert!(!full.master_inputs.is_empty());
    assert!(!full.pre_master.is_empty());
    assert!(lean.master_inputs.is_empty() && lean.pre_master.is_empty());
    assert!(!lean.capture_master_inputs);
}

#[test]
fn performance_send_graph_routes_blocks_seek_and_static_amount() {
    use crate::performance_send::SendEnvelope;
    use oc_dsl::performance::ChannelCommand;
    use oc_synth::performance::ScheduledCommand;
    let project = oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap();
    let mut graph = AudioGraph::from_project(&project, 48000);
    graph.bus_chains.insert(
        "return".into(),
        BusNode {
            chain: EffectChain::new(),
            buffer: vec![],
            routes: vec![],
            gain: 1.0,
            audible: true,
        },
    );
    graph.routing[0].sends.push(SendRoute {
        bus: "return".into(),
        amount: 0.5,
        pre_fader: false,
        pan: 0.0,
    });
    let commands = [ScheduledCommand {
        sample: 2,
        channel: 0,
        message: ChannelCommand::ControlChange {
            controller: 91,
            value: 127,
        },
    }];
    let envelope = SendEnvelope::compile(&commands, 0, 91, 0).unwrap();
    assert!(
        graph
            .bind_performance_send(1, "return", envelope.clone())
            .is_err()
    );
    assert!(
        graph
            .bind_performance_send(0, "missing", envelope.clone())
            .is_err()
    );
    graph
        .bind_performance_send(0, "return", envelope.clone())
        .unwrap();
    assert!(graph.bind_performance_send(0, "return", envelope).is_err());
    let whole = graph.mix(&mut [vec![0.25; 12]]);
    let mut dry = [0.25; 12];
    apply_volume_pan(
        &mut dry,
        graph.routing[0].volume,
        graph.routing[0].pan,
        SourceWidth::Mono,
    );
    assert_eq!(&whole[..4], &dry[..4]); // dry path not suppressed
    assert_eq!(&whole[4..], &[dry[0] + dry[0] * 0.5; 8]);
    graph.reset();
    let mut chunks = graph.mix(&mut [vec![0.25; 2]]);
    chunks.extend(graph.mix(&mut [vec![0.25; 4]]));
    chunks.extend(graph.mix(&mut [vec![0.25; 6]]));
    assert_eq!(chunks, whole);
    graph.reset();
    let seek = graph.mix_with_transport(
        &mut [vec![0.25; 4]],
        Some(PluginTransport::new(3, 48000, 120.)),
    );
    assert_eq!(seek, whole[6..10]);
    graph.reset();
    assert_eq!(graph.mix(&mut [vec![0.25; 12]]), whole);
}

#[test]
fn performance_send_graph_latency_and_reset_clear_delayed_samples() {
    use crate::performance_send::SendEnvelope;
    use oc_dsl::performance::ChannelCommand;
    use oc_synth::performance::ScheduledCommand;
    let project = oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap();
    let mut graph = AudioGraph::from_project_with_source_latencies(&project, 48000, &[2]);
    graph.bus_chains.insert(
        "return".into(),
        BusNode {
            chain: EffectChain::new(),
            buffer: vec![],
            routes: vec![],
            gain: 1.0,
            audible: true,
        },
    );
    graph.routing[0].sends.push(SendRoute {
        bus: "return".into(),
        amount: 1.0,
        pre_fader: false,
        pan: 0.0,
    });
    graph.routing[0].direct_to_master = false;
    let envelope = SendEnvelope::compile(
        &[ScheduledCommand {
            sample: 1,
            channel: 0,
            message: ChannelCommand::ControlChange {
                controller: 93,
                value: 127,
            },
        }],
        0,
        93,
        0,
    )
    .unwrap();
    graph.bind_performance_send(0, "return", envelope).unwrap();
    let audio = graph.mix(&mut [vec![0.25; 12]]);
    assert_eq!(&audio[..6], &[0.; 6]);
    let mut wet = [0.25; 6];
    apply_volume_pan(
        &mut wet,
        graph.routing[0].volume,
        graph.routing[0].pan,
        SourceWidth::Mono,
    );
    assert_eq!(&audio[6..], &wet);
    graph.direct_delays[0] = StereoDelayCompensator::new(2);
    graph.direct_delays[0].process(&mut [1.; 2]);
    graph.reset();
    let mut silence = [0.; 8];
    graph.direct_delays[0].process(&mut silence);
    assert_eq!(silence, [0.; 8]);
}

#[test]
fn effect_chain_sums_reported_latency_safely() {
    let seen = Arc::new(Mutex::new(None));
    let chain = EffectChain {
        ramps: Vec::new(),
        boundary: ProcessBoundary::default(),
        effects: vec![
            Box::new(TransportAwareEffect {
                seen: seen.clone(),
                latency: 128,
            }),
            Box::new(TransportAwareEffect { seen, latency: 256 }),
        ],
    };
    assert_eq!(chain.latency_samples(), 384);
}

#[test]
fn pdc_aligns_unequal_direct_track_latencies() {
    let mut graph = AudioGraph {
        track_chains: vec![
            EffectChain::new(),
            EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(LatencyEffect::new(2))],
            },
        ],
        bus_chains: HashMap::new(),
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
        ],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 2],
        source_latencies: vec![0, 0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into(), "b".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    graph.refresh_latency_compensation();
    let mut tracks = vec![vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0], vec![0.0; 6]];
    tracks[1][0] = 1.0;
    tracks[1][1] = 1.0;
    for (chain, buffer) in graph.track_chains.iter_mut().zip(&mut tracks) {
        chain.process(buffer, graph.sample_rate);
    }

    let mixed = graph.mix_processed(&mut tracks, None);

    assert_eq!(&mixed[..4], &[0.0, 0.0, 0.0, 0.0]);
    assert!((mixed[4] - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert!((mixed[5] - std::f32::consts::SQRT_2).abs() < 1e-6);
}

#[test]
fn exclusive_output_has_no_parallel_dry_path_and_keeps_bus_latency() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  mix: output=fx\n@bus fx\n  routes: [master]\n").unwrap();
    let mut graph = AudioGraph::from_project(&project, 48000);
    graph
        .bus_chains
        .get_mut("fx")
        .unwrap()
        .chain
        .effects
        .push(Box::new(LatencyEffect::new(2)));
    graph.refresh_latency_compensation();
    let mut tracks = vec![vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0]];
    let mixed = graph.mix(&mut tracks);
    assert_eq!(&mixed[..4], &[0.0; 4]);
    assert!((mixed[4] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((mixed[5] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn pdc_aligns_dry_and_send_return_paths() {
    let mut buses = HashMap::new();
    buses.insert(
        "fx".to_string(),
        BusNode {
            chain: EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(LatencyEffect::new(2))],
            },
            buffer: vec![],
            routes: vec!["master".to_string()],
            gain: 1.0,
            audible: true,
        },
    );
    let mut graph = AudioGraph {
        track_chains: vec![EffectChain::new()],
        bus_chains: buses,
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![TrackRouting {
            sends: vec![SendRoute {
                bus: "fx".to_string(),
                amount: 1.0,
                pre_fader: false,
                pan: 0.0,
            }],
            aux_sends: vec![],
            volume: 1.0,
            pan: 0.0,
            mute: false,
            polarity_inverted: false,
            direct_to_master: true,
        }],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 1],
        source_latencies: vec![0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    graph.refresh_latency_compensation();
    let mut tracks = vec![vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0]];

    let mixed = graph.mix_processed(&mut tracks, None);

    assert_eq!(&mixed[..4], &[0.0, 0.0, 0.0, 0.0]);
    assert!((mixed[4] - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert!((mixed[5] - std::f32::consts::SQRT_2).abs() < 1e-6);
}

#[test]
fn nested_bus_routes_process_source_before_target_with_pdc_and_tails() {
    let mut buses = HashMap::new();
    buses.insert(
        "room".to_string(),
        BusNode {
            chain: EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(LatencyEffect::new(1)), Box::new(TailEffect(10))],
            },
            buffer: vec![],
            routes: vec!["glue".to_string()],
            gain: 1.0,
            audible: true,
        },
    );
    buses.insert(
        "glue".to_string(),
        BusNode {
            chain: EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(LatencyEffect::new(1)), Box::new(TailEffect(20))],
            },
            buffer: vec![],
            routes: vec!["master".to_string()],
            gain: 1.0,
            audible: true,
        },
    );
    let mut graph = AudioGraph {
        track_chains: vec![EffectChain::new()],
        bus_chains: buses,
        master_chain: EffectChain {
            ramps: Vec::new(),
            boundary: ProcessBoundary::default(),
            effects: vec![Box::new(TailEffect(30))],
        },
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![TrackRouting {
            sends: vec![SendRoute {
                bus: "room".to_string(),
                amount: 1.0,
                pre_fader: false,
                pan: 0.0,
            }],
            aux_sends: vec![],
            volume: 1.0,
            pan: 0.0,
            mute: false,
            polarity_inverted: false,
            direct_to_master: true,
        }],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 1],
        source_latencies: vec![0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into(), "b".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    graph.refresh_latency_compensation();
    let mut tracks = vec![vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0]];

    let mixed = graph.mix_processed(&mut tracks, None);

    assert_eq!(graph.bus_order, vec!["room", "glue"]);
    assert_eq!(graph.pre_master_latency_samples(), 2);
    assert_eq!(graph.max_effect_tail_samples(), 60);
    assert_eq!(&mixed[..4], &[0.0, 0.0, 0.0, 0.0]);
    assert!((mixed[4] - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert!((mixed[5] - std::f32::consts::SQRT_2).abs() < 1e-6);
}

#[test]
fn ducked_send_rides_the_existing_reverb_return_and_releases() {
    fn project(depth_db: f64) -> oc_dsl::ast::Project {
        oc_dsl::parse(&format!(
                "@project\n  tempo: 120\n  time_signature: 4/4\n\n@bus verb\n  routes: [master]\n  effects:\n    - reverb: decay=2s, mix=100%, predelay=0ms\n    - ducked_send: predelay=30ms, depth={depth_db}dB, attack=20ms, release=300ms, threshold=-45dB, width=100%, sidechain=Voice\n\n@track Voice | instrument: keys/piano\n  mix: output=verb\n"
            ))
            .unwrap()
    }

    fn render(project: &oc_dsl::ast::Project, input: &[f32]) -> Vec<f32> {
        let mut graph = AudioGraph::from_project(project, 48_000);
        let mut output = Vec::new();
        for chunk in input.chunks(480 * 2) {
            let mut tracks = vec![chunk.to_vec()];
            output.extend(graph.mix_processed(&mut tracks, None));
        }
        output
    }

    fn rms(buffer: &[f32], start_seconds: f64, end_seconds: f64) -> f64 {
        let start = (start_seconds * 48_000.0) as usize;
        let end = (end_seconds * 48_000.0) as usize;
        let mut sum = 0.0;
        let mut count = 0;
        for frame in start..end.min(buffer.len() / 2) {
            for channel in 0..2 {
                let value = buffer[frame * 2 + channel] as f64;
                sum += value * value;
                count += 1;
            }
        }
        (sum / count.max(1) as f64).sqrt()
    }

    let mut input = Vec::with_capacity((2.5 * 48_000.0) as usize * 2);
    for frame in 0..(2.5 * 48_000.0) as usize {
        let value = if frame < 48_000 {
            0.25 * (std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin()
        } else {
            0.0
        };
        input.push(value as f32);
        input.push(value as f32);
    }

    let reference = render(&project(0.0), &input);
    let ducked = render(&project(12.0), &input);
    let during = 20.0 * (rms(&ducked, 0.7, 0.95) / rms(&reference, 0.7, 0.95).max(1e-12)).log10();
    assert!(
        (during + 12.0).abs() < 1.5,
        "the return dropped {during:.2} dB through the reverb send"
    );
    let after = 20.0 * (rms(&ducked, 1.50, 1.75) / rms(&reference, 1.50, 1.75).max(1e-12)).log10();
    assert!(
        after > -1.0,
        "the return was still {after:.2} dB down 1.5x release after the dry stopped"
    );
}

#[test]
fn bus_sidechain_dependency_runs_key_before_target_and_delivers_audio() {
    let seen = Arc::new(Mutex::new(None));
    let mut buses = HashMap::new();
    buses.insert(
        "key".to_string(),
        BusNode {
            chain: EffectChain::new(),
            buffer: vec![],
            routes: vec!["master".to_string()],
            gain: 1.0,
            audible: true,
        },
    );
    buses.insert(
        "ducked".to_string(),
        BusNode {
            chain: EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(SidechainEffect::new(
                    Box::new(SidechainCaptureEffect { seen: seen.clone() }),
                    "key".into(),
                ))],
            },
            buffer: vec![],
            routes: vec!["master".to_string()],
            gain: 1.0,
            audible: true,
        },
    );
    let mut graph = AudioGraph {
        track_chains: vec![EffectChain::new(), EffectChain::new()],
        bus_chains: buses,
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![
            TrackRouting {
                sends: vec![SendRoute {
                    bus: "key".to_string(),
                    amount: 1.0,
                    pre_fader: false,
                    pan: 0.0,
                }],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
            TrackRouting {
                sends: vec![SendRoute {
                    bus: "ducked".to_string(),
                    amount: 1.0,
                    pre_fader: false,
                    pan: 0.0,
                }],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
        ],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 2],
        source_latencies: vec![0, 0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["kick".into(), "bass".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    graph.refresh_latency_compensation();
    let mut tracks = vec![vec![1.0, 1.0, 0.0, 0.0], vec![1.0, 1.0, 0.0, 0.0]];

    graph.mix_processed(&mut tracks, None);

    assert_eq!(graph.bus_order, vec!["key", "ducked"]);
    assert_eq!(*seen.lock().unwrap(), Some((0, 0)));
}

#[test]
fn stable_latency_plan_preserves_streaming_delay_history() {
    let latency = Arc::new(AtomicU32::new(2));
    let mut graph = AudioGraph {
        track_chains: vec![
            EffectChain::new(),
            EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(MutableLatencyEffect { latency })],
            },
        ],
        bus_chains: HashMap::new(),
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
        ],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 2],
        source_latencies: vec![0, 0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into(), "b".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    assert!(graph.refresh_latency_compensation());
    graph.direct_delays[0].process(&mut [1.0, 1.0]);
    let position = graph.direct_delays[0].position;
    let buffered = graph.direct_delays[0].buffer.clone();

    assert!(!graph.refresh_latency_compensation());
    assert_eq!(graph.direct_delays[0].position, position);
    assert_eq!(graph.direct_delays[0].buffer, buffered);
}

#[test]
fn changed_effect_latency_invalidates_compensation_plan() {
    let latency = Arc::new(AtomicU32::new(2));
    let mut graph = AudioGraph {
        track_chains: vec![
            EffectChain::new(),
            EffectChain {
                ramps: Vec::new(),
                boundary: ProcessBoundary::default(),
                effects: vec![Box::new(MutableLatencyEffect {
                    latency: latency.clone(),
                })],
            },
        ],
        bus_chains: HashMap::new(),
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
        ],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 2],
        source_latencies: vec![0, 0],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    assert!(graph.refresh_latency_compensation());
    assert_eq!(graph.direct_delays[0].buffer.len(), 4);

    latency.store(4, Ordering::Relaxed);

    assert!(graph.refresh_latency_compensation());
    assert_eq!(graph.direct_delays[0].buffer.len(), 8);
    assert_eq!(graph.direct_delays[0].position, 0);
}

#[test]
fn pdc_aligns_instrument_source_latency_before_insert_effects() {
    let mut graph = AudioGraph {
        track_chains: vec![EffectChain::new(), EffectChain::new()],
        bus_chains: HashMap::new(),
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
            TrackRouting {
                sends: vec![],
                aux_sends: vec![],
                volume: 1.0,
                pan: 0.0,
                mute: false,
                polarity_inverted: false,
                direct_to_master: true,
            },
        ],
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; 2],
        source_latencies: vec![0, 2],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: vec!["a".into(), "b".into()],
        spectrum: None,
        spectrum_target: None,
        control_runtimes: Vec::new(),
        control_gains: vec![None],
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        control_fresh: Vec::new(),
    };
    graph.refresh_latency_compensation();
    let mut tracks = vec![
        vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0],
    ];

    let mixed = graph.mix_processed(&mut tracks, None);

    assert_eq!(&mixed[..4], &[0.0, 0.0, 0.0, 0.0]);
    assert!((mixed[4] - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert!((mixed[5] - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert_eq!(graph.pre_master_latency_samples(), 2);
}

#[test]
fn meters_read_track_bus_and_master_gain_reduction() {
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
@track keys | instrument: piano/acoustic_grand
  mix: send(fx=100%)
  effects:
    - compression: threshold=-30dB, ratio=8:1, attack=1ms
@bus fx
  routes: [master]
  effects:
    - compression: threshold=-30dB, ratio=4:1, attack=1ms
@master
  effects:
    - compression: threshold=-30dB, ratio=4:1, attack=1ms
"#,
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    let mut tracks = vec![vec![0.5_f32; 4800]];
    graph.mix(&mut tracks);
    assert!(graph.track_gain_reduction_db(0) > 5.0);
    assert!(graph.bus_gain_reduction_db("fx") > 1.0);
    assert!(graph.master_gain_reduction_db() > 0.5);
}

/// A silenced channel's buffer is what the streaming meters read, so it
/// must be zero, and its PDC line must be flushed rather than replayed.
#[test]
fn a_muted_track_meters_zero_and_its_delay_line_is_flushed() {
    let project = oc_dsl::parse(
        "@project\n\
             @track a | instrument: keys/piano\n\
             @track b | instrument: keys/piano\n",
    )
    .unwrap();
    // Track b's 2500-sample source latency puts track a's direct route
    // behind the master reference, so track a's output is buffered.
    let mut graph = AudioGraph::from_project_with_source_latencies(&project, 48_000, &[0, 2_500]);
    const BLOCK: usize = 4_800;
    const DELAY: usize = 2_500;
    let loud = vec![0.5_f32; BLOCK * 2];
    let silent = vec![0.0_f32; BLOCK * 2];
    let peak_of = |buffer: &[f32]| buffer.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));

    // Two blocks of loud audio fill track a's delay line.
    let mut pre_mute_peak = 0.0_f32;
    for _ in 0..2 {
        let mut tracks = vec![loud.clone(), silent.clone()];
        graph.mix(&mut tracks);
        pre_mute_peak = pre_mute_peak.max(peak_of(&tracks[0]));
    }
    assert!(pre_mute_peak > 0.1, "the loud track reaches the mix");

    // A full second of mute: every block's meter tap reads zero. Mute rides
    // the same 10 ms gain ramp as the fader, so the first block carries the
    // fade and the measurement starts after it.
    graph.routing[0].mute = true;
    let mut settle = vec![loud.clone(), silent.clone()];
    graph.mix(&mut settle);
    let mut muted_peak = 0.0_f32;
    for _ in 0..10 {
        let mut tracks = vec![loud.clone(), silent.clone()];
        graph.mix(&mut tracks);
        muted_peak = muted_peak.max(peak_of(&tracks[0]));
    }

    // Unmute into silence. The first `DELAY` frames are the zeros the line
    // was fed while muted; pre-mute audio here is the stale burst.
    graph.routing[0].mute = false;
    let mut tracks = vec![silent.clone(), silent.clone()];
    graph.mix(&mut tracks);
    let burst = peak_of(&tracks[0][..DELAY * 2]);
    eprintln!("muted block peak {muted_peak}, unmute burst peak {burst}");
    assert_eq!(muted_peak, 0.0, "a muted channel must meter zero");
    assert_eq!(burst, 0.0, "unmuting must not replay pre-mute audio");
}

/// Unmuting resumes in the same place in the compensation timeline: after
/// the line refills, the channel matches a never-muted reference.
#[test]
fn unmuting_resumes_in_phase_with_a_never_muted_reference() {
    let project = oc_dsl::parse(
        "@project\n\
             @track a | instrument: keys/piano\n\
             @track b | instrument: keys/piano\n",
    )
    .unwrap();
    let build = || AudioGraph::from_project_with_source_latencies(&project, 48_000, &[0, 2_500]);
    let mut muted = build();
    let mut reference = build();
    const BLOCK: usize = 4_800;
    const PRE_BLOCKS: usize = 3;
    const MUTE_BLOCKS: usize = 10;
    const POST_BLOCKS: usize = 3;

    // A non-repeating ramp keeps every shifted copy distinguishable.
    let signal = |start: usize| -> Vec<f32> {
        let mut buffer = Vec::with_capacity(BLOCK * 2);
        for frame in start..start + BLOCK {
            let value = (frame % 977) as f32 / 977.0 - 0.5;
            buffer.push(value);
            buffer.push(value);
        }
        buffer
    };

    let mut start = 0;
    for block in 0..PRE_BLOCKS + MUTE_BLOCKS + POST_BLOCKS {
        let input = signal(start);
        start += BLOCK;
        if block == PRE_BLOCKS {
            muted.routing[0].mute = true;
        }
        if block == PRE_BLOCKS + MUTE_BLOCKS {
            muted.routing[0].mute = false;
        }
        let mut muted_tracks = vec![input.clone(), vec![0.0; BLOCK * 2]];
        let mut reference_tracks = vec![input, vec![0.0; BLOCK * 2]];
        let muted_mix = muted.mix(&mut muted_tracks);
        let reference_mix = reference.mix(&mut reference_tracks);
        // The first unmuted block still drains the zeros the line was fed
        // while muted; from the next block the two are sample-identical.
        if block > PRE_BLOCKS + MUTE_BLOCKS {
            assert_eq!(muted_mix, reference_mix, "block {block} is out of phase");
        }
    }
}

/// Solo on another track silences this one through the same resolved mute
/// flag, and the soloed track still reaches the master.
#[test]
fn a_soloed_other_track_meters_this_one_at_zero() {
    let project = oc_dsl::parse(
        "@project\n\
             @track a | instrument: keys/piano\n\
             @track b | instrument: keys/piano\n  mix: solo=true\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    assert!(graph.track_is_silenced(0));
    let mut tracks = vec![vec![0.5_f32; 9_600], vec![0.5_f32; 9_600]];
    let mixed = graph.mix(&mut tracks);
    assert_eq!(
        tracks[0].iter().fold(0.0_f32, |peak, s| peak.max(s.abs())),
        0.0,
        "another track's solo meters this one at zero"
    );
    assert!(
        mixed.iter().any(|sample| *sample != 0.0),
        "the soloed track still plays"
    );
}

/// A quiet channel that is not silenced still reaches the meter tap.
#[test]
fn a_quiet_unmuted_track_still_meters_its_level() {
    let project = oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    assert!(!graph.track_is_silenced(0));
    let mut tracks = vec![vec![0.001_f32; 9_600]];
    graph.mix(&mut tracks);
    let peak = tracks[0].iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert!(
        (peak - 0.001 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-7,
        "a quiet unmuted channel still meters its level: {peak}"
    );
}

#[test]
fn stereo_buffer_is_unity_per_channel_at_centre() {
    let mut buffer = vec![0.4, 0.25, 0.4, 0.25];
    apply_volume_pan(&mut buffer, 1.0, 0.0, SourceWidth::Stereo);
    for frame in buffer.chunks_exact(2) {
        assert!((frame[0] - 0.4).abs() < 1e-6);
        assert!((frame[1] - 0.25).abs() < 1e-6);
    }
}

#[test]
fn mono_buffer_stays_minus_three_db_per_channel_at_centre() {
    let mut buffer = vec![0.4, 0.4, 0.4, 0.4];
    apply_volume_pan(&mut buffer, 1.0, 0.0, SourceWidth::Mono);
    for sample in buffer {
        assert!((sample - 0.4 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }
}

#[test]
fn stereo_hard_pan_keeps_the_near_channel_at_unity() {
    let mut left = vec![0.4, 0.25];
    apply_volume_pan(&mut left, 1.0, -1.0, SourceWidth::Stereo);
    assert!((left[0] - 0.4).abs() < 1e-6);
    assert!(left[1].abs() < 1e-6);

    let mut right = vec![0.4, 0.25];
    apply_volume_pan(&mut right, 1.0, 1.0, SourceWidth::Stereo);
    assert!(right[0].abs() < 1e-6);
    assert!((right[1] - 0.25).abs() < 1e-6);
}

#[test]
fn stereo_balance_attenuates_only_the_side_opposite_the_pan() {
    let mut right = vec![1.0, 1.0];
    apply_volume_pan(&mut right, 1.0, 0.25, SourceWidth::Stereo);
    assert!((right[0] - 0.75).abs() < 1e-6);
    assert!((right[1] - 1.0).abs() < 1e-6);

    let mut left = vec![1.0, 1.0];
    apply_volume_pan(&mut left, 1.0, -0.25, SourceWidth::Stereo);
    assert!((left[0] - 1.0).abs() < 1e-6);
    assert!((left[1] - 0.75).abs() < 1e-6);
}

#[test]
fn mono_hard_pan_keeps_constant_power() {
    let mut buffer = vec![1.0, 1.0];
    apply_volume_pan(&mut buffer, 1.0, -1.0, SourceWidth::Mono);
    assert!((buffer[0] - 1.0).abs() < 1e-6);
    assert!(buffer[1].abs() < 1e-6);
}

#[test]
fn detected_width_promotes_mono_to_stereo_and_sticks() {
    let mut graph = AudioGraph::from_project(
        &oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap(),
        48_000,
    );
    assert_eq!(graph.track_widths(), &[SourceWidth::Mono]);
    graph.detect_track_widths(&[vec![0.5, 0.25]]);
    assert_eq!(graph.track_widths(), &[SourceWidth::Stereo]);
    graph.detect_track_widths(&[vec![0.5, 0.5]]);
    assert_eq!(graph.track_widths(), &[SourceWidth::Stereo]);
}

#[test]
fn explicit_balance_keeps_dual_mono_at_unity_from_the_first_block() {
    let project =
        oc_dsl::parse("@project\n@track stem | instrument: audio/clip\n  mix: pan_law=balance\n")
            .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    graph.set_track_widths(&[SourceWidth::Mono]);
    let mut tracks = vec![vec![0.5, 0.5, 0.25, 0.25]];
    assert_eq!(graph.mix_processed(&mut tracks, None), tracks[0]);
    assert_eq!(graph.track_widths(), &[SourceWidth::Stereo]);
}

#[test]
fn graph_mixes_stereo_and_mono_tracks_with_their_own_law() {
    let project = oc_dsl::parse(
        "@project\n@track stereo | instrument: keys/piano\n@track mono | instrument: keys/piano\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, 48_000);
    let mut tracks = vec![vec![0.5, 0.25, 0.5, 0.25], vec![0.5, 0.5, 0.5, 0.5]];
    let mixed = graph.mix_processed(&mut tracks, None);
    let pan = std::f32::consts::FRAC_1_SQRT_2;
    assert!((mixed[0] - (0.5 + 0.5 * pan)).abs() < 1e-6);
    assert!((mixed[1] - (0.25 + 0.5 * pan)).abs() < 1e-6);
}
// ---- Destination-referenced latency compensation --------------------

/// Delay line latency of a track or bus chain, in frames.
fn chain_with_latency(latency: u32) -> EffectChain {
    let mut chain = EffectChain::new();
    if latency > 0 {
        chain.effects.push(Box::new(LatencyEffect::new(latency)));
    }
    chain
}

/// A graph with one mono track per entry of `latencies`; effects carry the
/// latency so the delay is real audio, not a reported number.
fn staged_graph(
    names: &[&str],
    latencies: &[u32],
    buses: HashMap<String, BusNode>,
    routing: Vec<TrackRouting>,
) -> AudioGraph {
    AudioGraph {
        track_chains: latencies
            .iter()
            .map(|latency| chain_with_latency(*latency))
            .collect(),
        bus_chains: buses,
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing,
        sample_rate: 48_000,
        track_widths: vec![SourceWidth::Mono; latencies.len()],
        source_latencies: vec![0; latencies.len()],
        latency_compensation: vec![],
        latency_offsets: vec![],
        recording: false,
        latency_diagnostics: vec![],
        latency_readout: vec![],
        control_runtimes: Vec::new(),
        control_gains: Vec::new(),
        control_only: Vec::new(),
        key_plan: KeyPlan::default(),
        prepared: None,
        pre_master_latency_samples: 0,
        direct_delays: vec![],
        send_delays: HashMap::new(),
        aux_send_delays: HashMap::new(),
        performance_sends: HashMap::new(),
        send_cursor: 0,
        bus_route_delays: HashMap::new(),
        bus_order: vec![],
        bus_downstream_latencies: HashMap::new(),
        latency_signature: vec![],
        track_names: names.iter().map(|name| name.to_string()).collect(),
        spectrum: None,
        spectrum_target: None,
        control_fresh: Vec::new(),
    }
}

fn master_route() -> TrackRouting {
    TrackRouting {
        sends: vec![],
        aux_sends: vec![],
        volume: 1.0,
        pan: 0.0,
        mute: false,
        polarity_inverted: false,
        direct_to_master: true,
    }
}

fn route_into(bus: &str, direct_to_master: bool) -> TrackRouting {
    TrackRouting {
        sends: vec![SendRoute {
            bus: bus.to_string(),
            amount: 1.0,
            pre_fader: false,
            pan: 0.0,
        }],
        direct_to_master,
        ..master_route()
    }
}

fn bus_node(latency: u32) -> BusNode {
    BusNode {
        chain: chain_with_latency(latency),
        buffer: vec![],
        routes: vec!["master".to_string()],
        gain: 1.0,
        audible: true,
    }
}

fn impulse(frames: usize, amplitude: f32) -> Vec<f32> {
    let mut buffer = vec![0.0; frames * 2];
    buffer[0] = amplitude;
    buffer[1] = amplitude;
    buffer
}

/// Frames of a stereo tap that are not silent.
fn sounding_frames(tap: &[f32]) -> Vec<usize> {
    tap.chunks_exact(2)
        .enumerate()
        .filter(|(_, frame)| frame[0] != 0.0 || frame[1] != 0.0)
        .map(|(index, _)| index)
        .collect()
}

/// Lag, in frames, of the cross-correlation peak of two stereo taps.
fn correlation_lag(reference: &[f32], candidate: &[f32], max_lag: usize) -> i64 {
    let frames = reference.len().min(candidate.len()) / 2;
    let mut best = (f64::NEG_INFINITY, 0_i64);
    for lag in -(max_lag as i64)..=(max_lag as i64) {
        let mut sum = 0.0_f64;
        for frame in 0..frames {
            let other = frame as i64 + lag;
            if other < 0 || other as usize >= frames {
                continue;
            }
            sum += f64::from(reference[frame * 2]) * f64::from(candidate[other as usize * 2]);
        }
        if sum > best.0 {
            best = (sum, lag);
        }
    }
    best.1
}

fn tap<'a>(stems: &'a StemCapture, name: &str) -> &'a [f32] {
    &stems
        .master_inputs
        .iter()
        .find(|(label, _)| label == name)
        .unwrap_or_else(|| {
            panic!(
                "no master input tap {name}: {:?}",
                stems
                    .master_inputs
                    .iter()
                    .map(|(label, _)| label)
                    .collect::<Vec<_>>()
            )
        })
        .1
}

fn applied_delay(delay: &StereoDelayCompensator) -> u32 {
    (delay.buffer.len() / 2) as u32
}

/// Three-track series/parallel graph: A(0) -> B(128) -> master, A -> master
/// directly, C(512) -> master. Every path has to land on the same sample.
#[test]
fn pdc_references_the_destination_and_aligns_every_master_path() {
    let frames = 1_200;
    let mut buses = HashMap::new();
    buses.insert("B".to_string(), bus_node(128));
    let mut graph = staged_graph(
        &["a", "c"],
        &[0, 512],
        buses,
        vec![route_into("B", true), master_route()],
    );
    graph.refresh_latency_compensation();

    // Reference: the global maximum is the master's business only.
    assert_eq!(graph.pre_master_latency_samples(), 512);
    // The route into the bus is referenced to the bus, not to the graph.
    assert_eq!(applied_delay(&graph.send_delays[&(0, 0)]), 0);
    assert_eq!(
        applied_delay(&graph.bus_route_delays[&("B".to_string(), "master".to_string())]),
        384
    );
    // Direct paths keep the master reference.
    assert_eq!(applied_delay(&graph.direct_delays[0]), 512);
    assert_eq!(applied_delay(&graph.direct_delays[1]), 0);

    // A(0) feeding B(128) is the refusal case: the destination is already
    // more latent than the source.
    assert_eq!(
        graph.latency_diagnostics(),
        &[LatencyDiagnostic {
            source: "a".into(),
            destination: "B".into(),
            delta: -128,
        }]
    );

    let mut tracks = vec![impulse(frames, 0.5), impulse(frames, 0.25)];
    for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
        chain.process(buffer, graph.sample_rate);
    }
    let mut stems = StemCapture::default();
    let mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));

    // Every path is one impulse on the same frame: this is alignment, not a
    // small residual.
    for name in ["track:a", "track:c", "bus:B"] {
        assert_eq!(
            sounding_frames(tap(&stems, name)),
            vec![512],
            "path {name} must land on the master reference with no smear"
        );
    }
    let reference = tap(&stems, "track:a");
    for name in ["track:c", "bus:B"] {
        assert_eq!(
            correlation_lag(reference, tap(&stems, name), 64),
            0,
            "path {name} must correlate at lag zero"
        );
    }
    assert_eq!(sounding_frames(&mixed), vec![512]);
}

/// Two sources with different latencies merge into one bus. The merge is
/// referenced to the destination (128), not to the slowest path (512).
#[test]
fn pdc_aligns_a_merge_to_its_destination_not_the_global_maximum() {
    let frames = 1_200;
    let mut buses = HashMap::new();
    buses.insert("B".to_string(), bus_node(0));
    let mut graph = staged_graph(
        &["a", "b", "c"],
        &[0, 128, 512],
        buses,
        vec![
            route_into("B", false),
            route_into("B", false),
            master_route(),
        ],
    );
    graph.refresh_latency_compensation();

    assert_eq!(graph.pre_master_latency_samples(), 512);
    // The early source waits for the destination, not for the graph.
    assert_eq!(applied_delay(&graph.send_delays[&(0, 0)]), 128);
    assert_eq!(applied_delay(&graph.send_delays[&(1, 0)]), 0);
    // The destination's own return carries the rest of the master reference.
    assert_eq!(
        applied_delay(&graph.bus_route_delays[&("B".to_string(), "master".to_string())]),
        384
    );
    assert!(graph.latency_diagnostics().is_empty());

    let mut tracks = vec![
        impulse(frames, 0.25),
        impulse(frames, 0.5),
        impulse(frames, 0.25),
    ];
    for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
        chain.process(buffer, graph.sample_rate);
    }
    let mut stems = StemCapture::default();
    let mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));

    assert_eq!(sounding_frames(tap(&stems, "bus:B")), vec![512]);
    assert_eq!(sounding_frames(tap(&stems, "track:c")), vec![512]);
    assert_eq!(
        correlation_lag(tap(&stems, "track:c"), tap(&stems, "bus:B"), 64),
        0
    );
    assert_eq!(sounding_frames(&mixed), vec![512]);
}

/// A destination that is more latent than its source cannot be made up at
/// the source: it is a typed diagnostic, and the master stays aligned.
#[test]
fn pdc_refuses_a_later_destination_with_a_diagnostic_and_stays_aligned() {
    // The warning buffer is process-global; keep another test's drain out
    // of this test's window.
    let _serial = crate::render_warnings::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let frames = 2_600;
    let mut buses = HashMap::new();
    buses.insert("B".to_string(), bus_node(1_024));
    let mut graph = staged_graph(
        &["a", "c"],
        &[0, 1_024],
        buses,
        vec![route_into("B", true), master_route()],
    );
    crate::render_warnings::clear();
    graph.refresh_latency_compensation();

    let diagnostics = graph.latency_diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].source, "a");
    assert_eq!(diagnostics[0].destination, "B");
    assert_eq!(diagnostics[0].delta, -1_024);
    assert!(
        diagnostics[0]
            .to_string()
            .contains("1024 samples more latency")
    );
    assert_eq!(
        graph.validate_latency_compensation(),
        Err(diagnostics.to_vec())
    );
    assert!(
        crate::render_warnings::take()
            .iter()
            .any(|warning| warning.contains("1024 samples more latency")),
        "the render must carry the diagnostic as a warning"
    );

    let mut tracks = vec![impulse(frames, 0.5), impulse(frames, 0.25)];
    for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
        chain.process(buffer, graph.sample_rate);
    }
    let mut stems = StemCapture::default();
    let mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));

    // Not silently misaligned: both paths still land on the reference.
    assert_eq!(sounding_frames(tap(&stems, "track:a")), vec![1_024]);
    assert_eq!(sounding_frames(tap(&stems, "bus:B")), vec![1_024]);
    assert_eq!(
        correlation_lag(tap(&stems, "track:a"), tap(&stems, "bus:B"), 64),
        0
    );
    assert_eq!(sounding_frames(&mixed), vec![1_024]);
}

/// A recording pass drops the compensation for a `WhenRecording` track, so
/// its tap lines up with the timeline instead of with the master reference.
#[test]
fn when_recording_bypass_drops_only_the_recording_paths_delay() {
    let project = oc_dsl::parse(
            "@project\n  tempo: 120\n@track a | instrument: keys/piano\n  mix: pdc=when_recording\n@track b | instrument: keys/piano\n",
        )
        .unwrap();
    let frames = 1_200;
    let build = |recording: bool, policy: Option<LatencyCompensation>| {
        let mut graph = AudioGraph::from_project(&project, 48_000);
        graph.track_chains[1]
            .effects
            .push(Box::new(LatencyEffect::new(512)));
        if let Some(policy) = policy {
            graph.set_latency_compensation(0, policy);
        }
        graph.set_recording(recording);
        graph.refresh_latency_compensation();
        graph
    };
    let render = |graph: &mut AudioGraph| {
        let mut tracks = vec![impulse(frames, 0.5), impulse(frames, 0.5)];
        for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
            chain.process(buffer, graph.sample_rate);
        }
        let mut stems = StemCapture::default();
        let mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));
        (stems, mixed)
    };

    let mut playing = build(false, None);
    assert_eq!(playing.pre_master_latency_samples(), 512);
    assert_eq!(applied_delay(&playing.direct_delays[0]), 512);
    let (aligned, _) = render(&mut playing);
    assert_eq!(sounding_frames(tap(&aligned, "track:a")), vec![512]);
    assert_eq!(sounding_frames(tap(&aligned, "track:b")), vec![512]);
    assert_eq!(
        correlation_lag(tap(&aligned, "track:b"), tap(&aligned, "track:a"), 600),
        0
    );

    // A recording pass leaves the recording path on the timeline, so it no
    // longer lines up with the master reference.
    let mut recording = build(true, None);
    assert_eq!(applied_delay(&recording.direct_delays[0]), 0);
    assert_eq!(applied_delay(&recording.direct_delays[1]), 0);
    let (recorded, _) = render(&mut recording);
    assert_eq!(sounding_frames(tap(&recorded, "track:a")), vec![0]);
    assert_eq!(sounding_frames(tap(&recorded, "track:b")), vec![512]);
    assert_eq!(
        correlation_lag(tap(&recorded, "track:b"), tap(&recorded, "track:a"), 600),
        -512
    );

    // `off` is the permanent form of the same bypass.
    let mut disabled = build(false, Some(LatencyCompensation::Off));
    // The bypassed path drops its 512-frame delay; the other track already
    // carries the reference latency and needs none.
    assert_eq!(applied_delay(&disabled.direct_delays[0]), 0);
    assert_eq!(applied_delay(&disabled.direct_delays[1]), 0);
    let (dropped, _) = render(&mut disabled);
    assert_eq!(sounding_frames(tap(&dropped, "track:a")), vec![0]);
    assert_eq!(sounding_frames(tap(&dropped, "track:b")), vec![512]);
}

// ---- Manual latency offsets -----------------------------------------

/// A delay that is real audio but is not reported to the plan, standing in
/// for an external instrument's round trip the user calibrates by hand.
struct UnreportedDelayEffect {
    delay: StereoDelayCompensator,
}

impl AudioEffect for UnreportedDelayEffect {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.delay.process(buffer);
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        0
    }
    fn name(&self) -> &str {
        "unreported-delay-test"
    }
}

fn add_unreported_delay(chain: &mut EffectChain, samples: u32) {
    chain.effects.push(Box::new(UnreportedDelayEffect {
        delay: StereoDelayCompensator::new(samples),
    }));
}

/// a and b merge into bus B; c defines the master reference. Every track
/// carries a click, so the bus tap is a buffer equality, not a proxy.
fn merge_graph(offsets: [i32; 3], source_latencies: [u32; 3], unreported_delay: u32) -> AudioGraph {
    let mut buses = HashMap::new();
    buses.insert("B".to_string(), bus_node(0));
    let mut graph = staged_graph(
        &["a", "b", "c"],
        &[0, 128, 512],
        buses,
        vec![
            route_into("B", false),
            route_into("B", false),
            master_route(),
        ],
    );
    graph.source_latencies = source_latencies.to_vec();
    graph.latency_offsets = offsets.to_vec();
    add_unreported_delay(&mut graph.track_chains[0], unreported_delay);
    graph
}

fn merge_taps(graph: &mut AudioGraph, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut tracks = vec![
        impulse(frames, 0.25),
        impulse(frames, 0.5),
        impulse(frames, 0.25),
    ];
    for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
        chain.process(buffer, graph.sample_rate);
    }
    let mut stems = StemCapture::default();
    let _mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));
    (
        tap(&stems, "bus:B").to_vec(),
        tap(&stems, "track:c").to_vec(),
    )
}

/// A +128 manual offset corrects a source that is really 128 samples late
/// but reports none; a -128 offset corrects a source that over-reports by
/// 128. Both land on the destination reference exactly, and the applied
/// delay moves by exactly the offset.
#[test]
fn manual_offset_moves_the_alignment_by_exactly_the_offset() {
    let frames = 1_200;

    // Reference: the source is not late and reports no latency.
    let mut clean = merge_graph([0, 0, 0], [0, 0, 0], 0);
    clean.refresh_latency_compensation();
    let (reference, _) = merge_taps(&mut clean, frames);
    assert_eq!(sounding_frames(&reference), vec![512]);

    // +128: the source is late by 128 but reports 0.
    let mut late = merge_graph([0, 0, 0], [0, 0, 0], 128);
    late.refresh_latency_compensation();
    assert_eq!(applied_delay(&late.send_delays[&(0, 0)]), 128);
    let (uncorrected, _) = merge_taps(&mut late, frames);
    assert_eq!(sounding_frames(&uncorrected), vec![512, 640]);

    let mut corrected = merge_graph([128, 0, 0], [0, 0, 0], 128);
    corrected.refresh_latency_compensation();
    assert_eq!(applied_delay(&corrected.send_delays[&(0, 0)]), 0);
    let (aligned, _) = merge_taps(&mut corrected, frames);
    assert_eq!(sounding_frames(&aligned), vec![512]);
    assert_eq!(
        aligned, reference,
        "the +128 offset must restore the exact reference buffer"
    );

    // -128: the source over-reports 128 and is not late at all.
    let mut over_reported = merge_graph([0, 0, 0], [128, 0, 0], 0);
    over_reported.refresh_latency_compensation();
    assert_eq!(applied_delay(&over_reported.send_delays[&(0, 0)]), 0);
    let (early, _) = merge_taps(&mut over_reported, frames);
    assert_eq!(sounding_frames(&early), vec![384, 512]);

    let mut corrected = merge_graph([-128, 0, 0], [128, 0, 0], 0);
    corrected.refresh_latency_compensation();
    assert_eq!(applied_delay(&corrected.send_delays[&(0, 0)]), 128);
    let (aligned, _) = merge_taps(&mut corrected, frames);
    assert_eq!(sounding_frames(&aligned), vec![512]);
    assert_eq!(
        aligned, reference,
        "the -128 offset must restore the exact reference buffer"
    );
}

/// The readout splits the track's own latency from its manual offset, names
/// the reference it is compared against and the delay applied at each
/// destination, and matches both the planned delays and the diagnostic.
#[test]
fn latency_readout_matches_the_plan_and_the_diagnostic() {
    let mut buses = HashMap::new();
    buses.insert("X".to_string(), bus_node(128));
    let graph_routing = vec![
        route_into("X", true),
        route_into("X", false),
        master_route(),
    ];
    let mut graph = staged_graph(&["a", "b", "c"], &[0, 128, 512], buses, graph_routing);
    graph.refresh_latency_compensation();

    assert_eq!(graph.pre_master_latency_samples(), 512);
    let readout = graph.latency_readout();
    assert_eq!(readout.len(), 3);

    let a = &readout[0];
    assert_eq!(a.track, "a");
    assert_eq!(
        (
            a.reported_samples,
            a.requested_offset_samples,
            a.applied_offset_samples
        ),
        (0, 0, 0)
    );
    assert_eq!(a.effective_samples, 0);
    assert!(!a.offset_clamped);
    assert!(!a.bypassed);
    assert_eq!(
        a.destinations,
        vec![
            DestinationLatency {
                destination: "master".into(),
                reference_samples: 512,
                compensation_samples: 512,
                diagnostic_delta: None,
            },
            DestinationLatency {
                destination: "X".into(),
                reference_samples: 128,
                compensation_samples: 128,
                diagnostic_delta: Some(-128),
            },
        ]
    );
    assert_eq!(
        applied_delay(&graph.direct_delays[0]),
        a.destinations[0].compensation_samples
    );
    assert_eq!(
        applied_delay(&graph.send_delays[&(0, 0)]),
        a.destinations[1].compensation_samples
    );

    let b = &readout[1];
    assert_eq!(b.reported_samples, 128);
    assert_eq!(
        b.destinations,
        vec![DestinationLatency {
            destination: "X".into(),
            reference_samples: 128,
            compensation_samples: 0,
            diagnostic_delta: None,
        }]
    );
    assert_eq!(
        applied_delay(&graph.send_delays[&(1, 0)]),
        b.destinations[0].compensation_samples
    );

    let c = &readout[2];
    assert_eq!(c.reported_samples, 512);
    assert_eq!(
        c.destinations,
        vec![DestinationLatency {
            destination: "master".into(),
            reference_samples: 512,
            compensation_samples: 0,
            diagnostic_delta: None,
        }]
    );

    // The same numbers the readable diagnostic reports.
    let diagnostics = graph.latency_diagnostics();
    assert_eq!(
        diagnostics,
        &[LatencyDiagnostic {
            source: "a".into(),
            destination: "X".into(),
            delta: -128,
        }]
    );
    assert_eq!(a.destinations[1].diagnostic_delta, Some(-128));
    assert_eq!(
        graph.latency_readout_for("b").map(|r| r.reported_samples),
        Some(128)
    );
}

/// An offset may only redistribute the delay the plan already applies: it
/// is clamped at both ends, with one warning, never silently misapplied.
#[test]
fn manual_offset_is_clamped_to_the_plan_delay_budget() {
    let mut graph = merge_graph([200, -1_000, 0], [0, 0, 0], 0);
    graph.refresh_latency_compensation();

    let readout = graph.latency_readout();
    // a can only move into the 128 samples the bus already waits for.
    assert_eq!(readout[0].requested_offset_samples, 200);
    assert_eq!(readout[0].applied_offset_samples, 128);
    assert!(readout[0].offset_clamped);
    assert_eq!(readout[0].effective_samples, 128);
    // b cannot claim a negative latency.
    assert_eq!(readout[1].requested_offset_samples, -1_000);
    assert_eq!(readout[1].applied_offset_samples, -128);
    assert!(readout[1].offset_clamped);
    assert_eq!(readout[1].effective_samples, 0);
    // c asked for nothing.
    assert!(!readout[2].offset_clamped);

    // The plan still applies exactly the delays the readout reports.
    assert_eq!(applied_delay(&graph.send_delays[&(0, 0)]), 0);
    assert_eq!(applied_delay(&graph.send_delays[&(1, 0)]), 128);
    assert_eq!(graph.pre_master_latency_samples(), 512);

    // The warning text is a pure function of the clamp, so it is asserted
    // without the shared render-warning sink that parallel tests drain.
    let (_, warning) = super::latency::clamp_offset("a", 200, 0, 128);
    assert_eq!(
        warning.as_deref(),
        Some(
            "latency offset on 'a' clamped from 200 to 128 samples: the plan's delay \
                 budget is 0..=128 samples"
        )
    );
    let (_, warning) = super::latency::clamp_offset("b", -1_000, -128, 128);
    assert_eq!(
        warning.as_deref(),
        Some(
            "latency offset on 'b' clamped from -1000 to -128 samples: the plan's delay \
                 budget is -128..=128 samples"
        )
    );
    assert_eq!(super::latency::clamp_offset("c", 0, 0, 0), (0, None));
}

/// The offset is part of the declared latency, so it survives the
/// `WhenRecording` bypass: the policy drops the delay, not the calibration.
#[test]
fn manual_offset_survives_the_when_recording_bypass() {
    let project = oc_dsl::parse(
            "@project\n  tempo: 120\n@track a | instrument: keys/piano\n  mix: pdc=when_recording, latency=128\n@track b | instrument: keys/piano\n",
        )
        .unwrap();
    let frames = 1_200;
    let build = |recording: bool| {
        let mut graph = AudioGraph::from_project(&project, 48_000);
        graph.track_chains[0]
            .effects
            .push(Box::new(LatencyEffect::new(128)));
        graph.track_chains[1]
            .effects
            .push(Box::new(LatencyEffect::new(512)));
        graph.set_recording(recording);
        graph.refresh_latency_compensation();
        graph
    };
    let render = |graph: &mut AudioGraph| {
        let mut tracks = vec![impulse(frames, 0.5), impulse(frames, 0.5)];
        for (chain, buffer) in graph.track_chains.iter_mut().zip(tracks.iter_mut()) {
            chain.process(buffer, graph.sample_rate);
        }
        let mut stems = StemCapture::default();
        let _mixed = graph.mix_processed_with_stems(&mut tracks, None, Some(&mut stems));
        (
            tap(&stems, "track:a").to_vec(),
            tap(&stems, "track:b").to_vec(),
        )
    };

    let mut playing = build(false);
    assert_eq!(playing.latency_readout()[0].applied_offset_samples, 128);
    assert_eq!(playing.latency_readout()[0].effective_samples, 256);
    assert!(playing.latency_readout()[0].destinations[0].compensation_samples > 0);
    let (a_playing, b_playing) = render(&mut playing);
    assert_eq!(sounding_frames(&a_playing), vec![384]);

    let mut recording = build(true);
    assert!(recording.latency_readout()[0].bypassed);
    assert_eq!(
        recording.latency_readout()[0].applied_offset_samples,
        128,
        "the manual offset is a declared latency, not a compensation"
    );
    assert_eq!(
        recording.latency_readout()[0].destinations[0].compensation_samples,
        0,
        "the recording pass drops the compensation"
    );
    let (a_recording, b_recording) = render(&mut recording);
    assert_eq!(sounding_frames(&a_recording), vec![128]);
    assert_eq!(sounding_frames(&b_recording), vec![512]);
    assert_eq!(
        sounding_frames(&b_playing),
        vec![512],
        "the policy only drops the recording path's delay"
    );
}

const SOLO_MODES_FIXTURE: &str = r#"@ocp "0.2.0"

@project "solo modes"
  tempo: 120
  time_signature: 4/4

@preset tone_a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@preset tone_b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2, waveform: 0}

@preset tone_c
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@preset tone_d
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 550, amplitude: 0.2, waveform: 0}

@bus bus_a
  routes: [main, reverb]

@bus main
  routes: [side]

@bus side
  routes: [master]

@bus reverb
  routes: [master]

@master

@track src_a | preset: tone_a, instrument: synth/lead_square
  mix: output = bus_a, send(reverb = -6dB, pre)
  @phrase src_a [1:1 - 2:1]
    C4 w mf

@track src_b | preset: tone_b, instrument: synth/lead_square
  mix: output = main, send(reverb = -6dB, post)
  @phrase src_b [1:1 - 2:1]
    E4 w mf

@track src_c | preset: tone_c, instrument: synth/lead_square
  mix: output = reverb
  @phrase src_c [1:1 - 2:1]
    G4 w mf

@track src_d | preset: tone_d, instrument: synth/lead_square
  @phrase src_d [1:1 - 2:1]
    C5 w mf
"#;

fn audible_names<'a>(names: impl Iterator<Item = (&'a str, bool)>) -> Vec<&'a str> {
    names
        .filter(|(_, audible)| *audible)
        .map(|(name, _)| name)
        .collect()
}

/// Hand-computed audible sets: sources feed `bus_a`, `bus_a` fans out to
/// `main` and the `reverb` return, `main` continues through `side`; `src_c`
/// feeds only the reverb return and `src_d` feeds no bus at all.
#[test]
fn solo_modes_resolve_the_hand_computed_audible_set() {
    struct Case {
        mode: SoloMode,
        solo_tracks: &'static [&'static str],
        solo_buses: &'static [&'static str],
        tracks: &'static [&'static str],
        buses: &'static [&'static str],
    }
    let all_buses: &'static [&'static str] = &["bus_a", "main", "side", "reverb"];
    let cases = [
        // Solo the bus that carries a side path and the reverb return.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &[],
            solo_buses: &["main"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &[],
            solo_buses: &["main"],
            tracks: &["src_a", "src_b"],
            buses: &["bus_a", "main", "side"],
        },
        // A bus that feeds two destinations: upstream keeps only its feeds.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &[],
            solo_buses: &["bus_a"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &[],
            solo_buses: &["bus_a"],
            tracks: &["src_a"],
            buses: all_buses,
        },
        // The reverb return: both walks back through bus_a, upstream stops
        // at bus_a.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &[],
            solo_buses: &["reverb"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &[],
            solo_buses: &["reverb"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: &["bus_a", "reverb"],
        },
        // A soloed channel keeps its direct output and both send taps:
        // destinations are the path that carries it to master.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &["src_a"],
            solo_buses: &[],
            tracks: &["src_a"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &["src_a"],
            solo_buses: &[],
            tracks: &["src_a"],
            buses: all_buses,
        },
        // A channel whose only path is an exclusive bus output.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &["src_c"],
            solo_buses: &[],
            tracks: &["src_c"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &["src_c"],
            solo_buses: &[],
            tracks: &["src_c"],
            buses: &["reverb"],
        },
        // A soloed channel that feeds no bus at all still reaches master.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &["src_d"],
            solo_buses: &[],
            tracks: &["src_d"],
            buses: &[],
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &["src_d"],
            solo_buses: &[],
            tracks: &["src_d"],
            buses: &[],
        },
        // Simultaneous solos union their sets.
        Case {
            mode: SoloMode::Both,
            solo_tracks: &["src_c"],
            solo_buses: &["main"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: all_buses,
        },
        Case {
            mode: SoloMode::Upstream,
            solo_tracks: &["src_c"],
            solo_buses: &["main"],
            tracks: &["src_a", "src_b", "src_c"],
            buses: all_buses,
        },
    ];

    for case in cases {
        let mut project = oc_dsl::parse(SOLO_MODES_FIXTURE).unwrap();
        project.properties.solo_mode = case.mode;
        for track in &mut project.tracks {
            track.mix.solo = case.solo_tracks.contains(&track.name.as_str());
        }
        for bus in &mut project.buses {
            bus.solo = case.solo_buses.contains(&bus.name.as_str());
        }
        let tracks = audible_names(
            project
                .tracks
                .iter()
                .zip(audible_tracks(&project))
                .map(|(track, audible)| (track.name.as_str(), audible)),
        );
        let buses = audible_names(
            project
                .buses
                .iter()
                .zip(audible_buses(&project))
                .map(|(bus, audible)| (bus.name.as_str(), audible)),
        );
        assert_eq!(
            tracks,
            case.tracks.to_vec(),
            "{:?} solo tracks {:?} buses {:?}",
            case.mode,
            case.solo_tracks,
            case.solo_buses
        );
        assert_eq!(
            buses,
            case.buses.to_vec(),
            "{:?} solo tracks {:?} buses {:?}",
            case.mode,
            case.solo_tracks,
            case.solo_buses
        );
    }
}

#[test]
fn mute_wins_over_solo_in_every_mode() {
    for mode in SoloMode::ALL {
        let mut project = oc_dsl::parse(SOLO_MODES_FIXTURE).unwrap();
        project.properties.solo_mode = mode;
        let main = project
            .buses
            .iter()
            .position(|bus| bus.name == "main")
            .unwrap();
        project.buses[main].solo = true;
        project.buses[main].mute = true;
        assert!(
            !audible_buses(&project)[main],
            "{mode}: muted bus stays silent"
        );

        let src_b = project
            .tracks
            .iter()
            .position(|track| track.name == "src_b")
            .unwrap();
        project.tracks[src_b].mix.solo = true;
        project.tracks[src_b].mix.mute = true;
        assert!(
            !audible_tracks(&project)[src_b],
            "{mode}: muted channel stays silent"
        );
    }
}
// ---- Live chain patching ------------------------------------------------

/// A constant-input track whose only insert is a gain, so a parameter fade
/// is visible sample by sample.
fn fade_project() -> Project {
    oc_dsl::parse(
            "@project\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  effects:\n    - gain: factor=1\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        )
        .unwrap()
}

fn set_factor(project: &mut Project, factor: f64) {
    project.tracks[0].effects[0].params =
        vec![("factor".into(), oc_dsl::ast::ParamValue::Number(factor))];
}

/// One track per gain level, so a chain can be rebuilt while another one
/// keeps ringing.
fn ringing_project() -> Project {
    oc_dsl::parse(
            "@project\n  tempo: 120\n@track one | instrument: synth/lead_square\n  effects:\n    - delay: time=10ms, mix=50%, feedback=50%\n    - gain: factor=1\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n@track two | instrument: synth/lead_square\n  effects:\n    - delay: time=10ms, mix=50%, feedback=50%\n    - gain: factor=1\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        )
        .unwrap()
}

/// Peak of the last `frames` frames of a stereo buffer.
fn tail_peak(buffer: &[f32]) -> f32 {
    buffer
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn a_parameter_patch_fades_instead_of_rebuilding_the_chain() {
    let project = fade_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    // A constant input makes the fade shape directly readable.
    let mut buffers = vec![vec![1.0_f32; 4096 * 2]];
    graph.process_track_effects(&mut buffers, None);
    assert!((buffers[0][0] - 1.0).abs() < 1e-6);

    let mut next = project.clone();
    set_factor(&mut next, 0.25);
    let patched = graph.patch_chains(&project, &next, None, true).unwrap();
    assert_eq!(patched, ChainPatch::Patched { chains: 1 });

    let mut buffers = vec![vec![1.0_f32; 4096 * 2]];
    graph.process_track_effects(&mut buffers, None);
    // The fade starts at the old value: the first sample moves by one step
    // of the ramp, never by the whole change.
    assert!(
        (buffers[0][0] - 1.0).abs() < 0.005,
        "the first sample steps: {}",
        buffers[0][0]
    );
    // A 20 ms fade at 48 kHz is 960 frames; the value lands exactly there.
    let fade = crate::touch::release_fade_samples(48_000) as usize;
    assert!((buffers[0][fade * 2] - 0.25).abs() < 1e-6);
    assert!((buffers[0][(4095) * 2] - 0.25).abs() < 1e-6);
    // No click: the whole change is spread over the fade, and no adjacent
    // sample moves by more than this project's click threshold.
    let biggest = buffers[0]
        .chunks_exact(2)
        .map(|frame| frame[0])
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(biggest < 0.005, "the fade steps by {biggest}");
    assert_eq!(
        graph.track_chains[0].effects[0].get_param("factor"),
        Some(0.25)
    );
}

#[test]
fn a_parameter_patch_keeps_the_chain_dsp_state() {
    // The delay line must keep ringing: a rebuilt chain would start empty.
    let project = ringing_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    buffers[0][0] = 1.0;
    buffers[0][1] = 1.0;
    graph.process_track_effects(&mut buffers, None);

    let mut next = project.clone();
    next.tracks[0].effects[1].params =
        vec![("factor".into(), oc_dsl::ast::ParamValue::Number(0.5))];
    graph.patch_chains(&project, &next, None, true).unwrap();

    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    graph.process_track_effects(&mut buffers, None);
    assert!(
        tail_peak(&buffers[0]) > 1e-3,
        "the patched chain lost its delay tail"
    );
}

/// The shape of a chain decides whether it is rebuilt, so a change to one
/// track's chain must leave every other track's DSP state alone.
#[test]
fn a_shape_change_rebuilds_only_its_own_chain() {
    let project = ringing_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    buffers[1][0] = 1.0;
    buffers[1][1] = 1.0;
    graph.process_track_effects(&mut buffers, None);

    let mut next = project.clone();
    next.tracks[0].effects.push(oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Limiter,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    });
    assert_eq!(
        graph.patch_chains(&project, &next, None, true).unwrap(),
        ChainPatch::Patched { chains: 1 }
    );

    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    graph.process_track_effects(&mut buffers, None);
    assert!(
        tail_peak(&buffers[1]) > 1e-3,
        "the untouched chain lost its delay tail"
    );
}

#[test]
fn a_bypass_toggle_rebuilds_that_chain_and_keeps_the_others() {
    let project = ringing_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    buffers[1][0] = 1.0;
    buffers[1][1] = 1.0;
    graph.process_track_effects(&mut buffers, None);

    let mut next = project.clone();
    next.tracks[0].effects[0].bypass = true;
    assert_eq!(
        graph.patch_chains(&project, &next, None, true).unwrap(),
        ChainPatch::Patched { chains: 1 }
    );

    let mut buffers = vec![vec![0.0_f32; 512], vec![0.0_f32; 512]];
    graph.process_track_effects(&mut buffers, None);
    assert!(
        tail_peak(&buffers[1]) > 1e-3,
        "the untouched chain lost its delay tail"
    );
    // The bypassed track's own delay line is gone: a bypassed entry is
    // absent, so it neither sounds nor reports a tail.
    assert_eq!(graph.track_chains[0].effects[0].latency_samples(), 0);
}

/// A drag sends one document per input event; only the last value may be
/// left in the device, and the fade must never queue.
#[test]
fn the_last_parameter_write_of_a_drag_wins() {
    let project = fade_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let mut previous = project.clone();
    for step in 1..=50 {
        let mut next = previous.clone();
        set_factor(&mut next, 1.0 - f64::from(step) * 0.015);
        graph.patch_chains(&previous, &next, None, true).unwrap();
        previous = next;
    }
    let final_value = 1.0 - 50.0 * 0.015;
    let mut buffers = vec![vec![1.0_f32; 4096 * 2]];
    graph.process_track_effects(&mut buffers, None);
    let fade = crate::touch::release_fade_samples(48_000) as usize;
    assert!((buffers[0][fade * 2] - final_value as f32).abs() < 1e-6);
    assert_eq!(
        graph.track_chains[0].effects[0].get_param("factor"),
        Some(final_value)
    );
}

/// A document that adds a node cannot be absorbed by a chain patch.
#[test]
fn a_new_track_is_structural() {
    let project = fade_project();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let mut next = project.clone();
    next.tracks.push(project.tracks[0].clone());
    assert_eq!(
        graph.patch_chains(&project, &next, None, true).unwrap(),
        ChainPatch::Structural
    );
}
