//! Prepared graph processing through the production DSP and routing core.

use super::*;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

const RATE: u32 = 48_000;
const MAX_FRAMES: usize = 128;

fn project() -> Project {
    oc_dsl::parse(
        r#"@project
  tempo: 137
@control_source env | from: track(key), attack: 5ms, release: 120ms
@track key | instrument: drums/kick
  effects:
    - gain: factor=0.75
  mix: send(a=25%)
@track audio | instrument: synth/lead
  effects:
    - compression: threshold=-18dB, ratio=4, attack=5ms, release=80ms, sidechain=key
  mix: send(a=40%)
@track controlled | instrument: synth/lead
  effects:
    - compression: threshold=-18dB, ratio=2, attack=5ms, release=80ms
  mix: control(env=-12dB)
@bus a
  routes: [b, master]
  effects:
    - gain: factor=0.7
@bus b
  routes: [master]
  effects:
    - gain: factor=0.6
@master
  effects:
    - gain: factor=0.9
"#,
    )
    .unwrap()
}

fn graph() -> AudioGraph {
    let mut project = project();
    project.tracks[2]
        .input_routes
        .push(oc_dsl::ast::InputRoute {
            kind: oc_dsl::ast::RouteKind::Control,
            source: "env".into(),
            target: "fx1".into(),
        });
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[0, 37, 13]).unwrap();
    graph.routing[0].sends[0].pre_fader = true;
    graph.routing[1].aux_sends.push(AuxSendRoute {
        pair: 1,
        bus: "b".into(),
        amount: 0.3,
        pan: -0.2,
    });
    let envelope = crate::performance_send::SendEnvelope::compile(&[], 0, 91, 96).unwrap();
    graph.bind_performance_send(0, "a", envelope).unwrap();
    graph.invalidate_latency_plan();
    graph.set_spectrum_target(Some(SpectrumTarget::Master));
    graph.track_chains[0]
        .fade_params(&[(0, "factor".into(), 0.75, 0.2)], RATE)
        .unwrap();
    graph.prepare(MAX_FRAMES).unwrap();
    graph
}

#[test]
fn prepared_graph_matches_allocating_wrapper_with_controls_keys_routes_pdc_and_completed_ramps() {
    let mut prepared = graph();
    let mut reference = graph();
    let mut cursor = 0u64;
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project().properties).unwrap());
    let weak = Arc::downgrade(&clock);
    let transport_owner = PluginTransport::new(0, RATE, 137.0).with_clock(clock);
    for block in 0..60 {
        let frames = [128, 17, 64, 128, 1, 127][block % 6];
        let input: Vec<Vec<f32>> = (0..3)
            .map(|track| {
                (0..frames * 2)
                    .map(|sample| {
                        if block % 9 == 0 {
                            0.0
                        } else {
                            ((cursor as f32 + sample as f32) * 0.031 + track as f32).sin() * 0.2
                        }
                    })
                    .collect()
            })
            .collect();
        let mut actual_input = input.clone();
        let mut expected_input = input;
        let aux = HashMap::from([((1, 1), vec![0.03; frames * 2])]);
        if block == 10 {
            prepared.routing[0].mute = true;
            reference.routing[0].mute = true;
            prepared.routing[1].volume = 0.4;
            reference.routing[1].volume = 0.4;
        }
        if block == 25 {
            prepared.routing[0].mute = false;
            reference.routing[0].mute = false;
        }
        let transport = transport_owner.at_sample(cursor);
        let expected = reference.mix_with_transport_and_aux(
            &mut expected_input,
            Some(transport.clone()),
            &aux,
        );
        let mut actual = vec![f32::NAN; frames * 2];
        let mut result = Ok(());
        let operations = crate::test_alloc::operations(|| {
            result = prepared.process_prepared(PreparedGraphBlock {
                tracks: &mut actual_input,
                transport: Some(&transport),
                aux: &aux,
                output: &mut actual,
            });
            std::hint::black_box(prepared.track_chains[1].gain_reduction_db());
            std::hint::black_box(prepared.spectrum_bins());
        });
        assert_eq!(result, Ok(()));
        assert_eq!(operations, 0, "heap operation in block {block}");
        assert_eq!(actual, expected, "block {block}");
        drop(transport);
        assert_eq!(weak.strong_count(), 1, "only caller keeps the mapped clock");
        cursor += frames as u64;
    }
    assert!(prepared.track_chains[0].ramps.iter().all(ParamRamp::done));
    assert_eq!(
        prepared.track_chains[0].ramps.len(),
        1,
        "completed storage is retained until a control-plane edit"
    );
}

#[test]
fn prepared_graph_handles_empty_sparse_and_short_blocks_without_heap_operations() {
    let empty = oc_dsl::parse("@project").unwrap();
    let mut graph = AudioGraph::try_from_project_with_source_latencies(&empty, RATE, &[]).unwrap();
    graph.prepare(MAX_FRAMES).unwrap();
    let mut input = Vec::new();
    let mut output = [1.0; 34];
    assert_eq!(
        crate::test_alloc::operations(|| graph
            .process_into(&mut input, None, &HashMap::new(), &mut output)
            .unwrap()),
        0
    );
    assert_eq!(output, [0.0; 34]);

    let mut prepared = graph_with_one_gain();
    prepared.prepare(MAX_FRAMES).unwrap();
    for frames in [1, 17, 128, 0] {
        let mut input = vec![Vec::new()];
        let mut output = vec![1.0; frames * 2];
        assert_eq!(
            crate::test_alloc::operations(|| prepared
                .process_into(&mut input, None, &HashMap::new(), &mut output)
                .unwrap()),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.0));
    }
}

fn graph_with_one_gain() -> AudioGraph {
    AudioGraph::try_from_project_with_source_latencies(
        &oc_dsl::parse(
            "@project\n@track one | instrument: synth/lead\n  effects:\n    - gain: factor=0.5\n",
        )
        .unwrap(),
        RATE,
        &[],
    )
    .unwrap()
}

struct LatencyProbe {
    latency: Arc<AtomicU32>,
    calls: Arc<AtomicUsize>,
}

impl AudioEffect for LatencyProbe {
    fn process(&mut self, _buffer: &mut [f32], _rate: u32) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "latency probe"
    }
    fn latency_samples(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

#[test]
fn prepared_graph_refuses_capacity_shape_and_latency_changes_before_consuming_audio() {
    let mut graph = graph_with_one_gain();
    let latency = Arc::new(AtomicU32::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    graph.track_chains[0].effects.push(Box::new(LatencyProbe {
        latency: latency.clone(),
        calls: calls.clone(),
    }));
    let mut input = vec![vec![0.25; 32]];
    let original = input.clone();
    let mut output = [3.0; 32];
    assert_eq!(
        graph.process_into(&mut input, None, &HashMap::new(), &mut output),
        Err(PreparationError::NotPrepared)
    );
    graph.prepare(8).unwrap();
    assert_eq!(
        graph.process_into(&mut input, None, &HashMap::new(), &mut output),
        Err(PreparationError::CapacityExceeded)
    );
    graph.prepare(16).unwrap();
    latency.store(7, Ordering::Relaxed);
    let mut result = Ok(());
    assert_eq!(
        crate::test_alloc::operations(|| {
            result = graph.process_into(&mut input, None, &HashMap::new(), &mut output);
        }),
        0
    );
    assert_eq!(result, Err(PreparationError::LatencyChanged));
    assert_eq!(input, original);
    assert_eq!(output, [3.0; 32]);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(graph.send_cursor, 0);
    graph.prepare(16).unwrap();
    graph.routing[0].direct_to_master = false;
    assert_eq!(
        graph.process_into(&mut input, None, &HashMap::new(), &mut output),
        Err(PreparationError::TopologyChanged)
    );
    assert_eq!(input, original);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn prepared_graph_recycles_completed_ramps_on_control_plane_and_refuses_unbounded_edits() {
    let mut graph = graph_with_one_gain();
    let change = [(0, "factor".into(), 0.5, 0.8)];
    graph.track_chains[0].fade_params(&change, RATE).unwrap();
    graph.prepare(128).unwrap();
    for _ in 0..20 {
        let mut input = vec![vec![0.1; 256]];
        let mut output = [0.0; 256];
        assert_eq!(
            crate::test_alloc::operations(|| graph
                .process_into(&mut input, None, &HashMap::new(), &mut output)
                .unwrap()),
            0
        );
    }
    assert_eq!(graph.track_chains[0].ramps.len(), 1);
    graph.track_chains[0].fade_params(&change, RATE).unwrap();
    assert_eq!(graph.track_chains[0].ramps.len(), 1);
    let oversized: Vec<_> = (0..1025)
        .map(|index| (0, format!("parameter_{index}"), 0.0, 1.0))
        .collect();
    assert!(graph.track_chains[0].fade_params(&oversized, RATE).is_err());
    assert_eq!(graph.track_chains[0].ramps.len(), 1);
}

#[test]
fn prepared_graph_device_scratch_preserves_fresh_and_running_compressor_state() {
    use oc_audio::AudioEffect;
    let mut prepared = oc_dsp::compressor::Compressor::new(RATE);
    let mut reference = oc_dsp::compressor::Compressor::new(RATE);
    prepared.prepare_block(MAX_FRAMES).unwrap();
    let control = [0.9; MAX_FRAMES];
    for block in 0..6 {
        let frames = if block % 2 == 0 { 31 } else { MAX_FRAMES };
        let mut actual = vec![0.8; frames * 2];
        let mut expected = actual.clone();
        reference.process_with_control(&mut expected, &control[..frames], RATE, None);
        assert_eq!(
            crate::test_alloc::operations(|| prepared.process_with_control(
                &mut actual,
                &control[..frames],
                RATE,
                None
            )),
            0,
            "device block {block}"
        );
        assert_eq!(actual, expected);
        assert_eq!(prepared.latency_samples(), reference.latency_samples());
        assert_eq!(prepared.gain_reduction_db(), reference.gain_reduction_db());
        // Re-preparing an already-running processor must preserve its detector,
        // lookahead and filters; no synthetic audio is processed to warm it up.
        assert_eq!(
            crate::test_alloc::operations(|| prepared.prepare_block(MAX_FRAMES).unwrap()),
            0
        );
    }
    assert_eq!(
        prepared.prepare_block(usize::MAX),
        Err(oc_audio::BlockPreparationError::CapacityExceeded)
    );
}

#[test]
fn prepared_graph_refuses_unrepresentable_capacity_without_mutating_prepared_state() {
    let mut graph = graph_with_one_gain();
    graph.prepare(16).unwrap();
    for capacity in [usize::MAX, isize::MAX as usize / 4] {
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert_eq!(
                    graph.prepare(capacity),
                    Err(PreparationError::CapacityExceeded)
                );
            }),
            0
        );
    }
    let mut input = vec![vec![0.2; 32]];
    let mut output = [0.0; 32];
    assert_eq!(
        crate::test_alloc::operations(|| graph
            .process_into(&mut input, None, &HashMap::new(), &mut output)
            .unwrap()),
        0
    );
    assert!(output.iter().any(|sample| *sample != 0.0));
}

#[test]
fn borrowed_prepared_graph_keeps_caller_clock_on_empty_and_refused_blocks() {
    let mut graph = graph_with_one_gain();
    graph.prepare(MAX_FRAMES).unwrap();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project().properties).unwrap());
    let weak = Arc::downgrade(&clock);
    let transport = PluginTransport::new(0, RATE, 137.0).with_clock(clock);
    let aux = HashMap::new();
    for samples in [0, 1, MAX_FRAMES * 2 + 2] {
        let mut tracks = vec![vec![0.2; samples]];
        let mut output = vec![0.3; samples];
        let before = output.clone();
        let mut result = Ok(());
        assert_eq!(
            crate::test_alloc::operations(|| {
                result = graph.process_prepared(PreparedGraphBlock {
                    tracks: &mut tracks,
                    transport: Some(&transport),
                    aux: &aux,
                    output: &mut output,
                });
            }),
            0
        );
        if samples == 0 {
            assert_eq!(result, Ok(()));
        } else {
            assert_eq!(
                result,
                Err(if samples == 1 {
                    PreparationError::InvalidBuffer
                } else {
                    PreparationError::CapacityExceeded
                })
            );
            assert_eq!(output, before);
            assert!(tracks[0].iter().all(|sample| *sample == 0.2));
        }
        assert_eq!(weak.strong_count(), 1);
    }
    drop(transport);
    assert_eq!(weak.strong_count(), 0);
}

struct BorrowedDispatchProbe;

impl AudioEffect for BorrowedDispatchProbe {
    fn process(&mut self, buffer: &mut [f32], _rate: u32) {
        buffer.fill(-1.0);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let start = transport.unwrap().sample_pos;
        for (frame, samples) in buffer.chunks_exact_mut(2).enumerate() {
            samples.fill((start + frame as u64) as f32);
        }
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let start = transport.unwrap().sample_pos;
        for (frame, samples) in buffer.chunks_exact_mut(2).enumerate() {
            for channel in 0..2 {
                samples[channel] = (start + frame as u64) as f32 + key[frame * 2 + channel];
            }
        }
    }
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let start = transport.unwrap().sample_pos;
        for (frame, samples) in buffer.chunks_exact_mut(2).enumerate() {
            samples.fill((start + frame as u64) as f32 + control[frame]);
        }
    }
    fn reset(&mut self) {}
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn name(&self) -> &str {
        "borrowed-dispatch-probe"
    }
}

#[test]
fn borrowed_effect_chain_dispatch_preserves_mapped_main_key_control_offsets_without_heap_operations()
 {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n").unwrap();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
    let weak = Arc::downgrade(&clock);
    let transport = PluginTransport::new(47_936, RATE, 120.0).with_clock(clock);
    let key: Vec<f32> = (0..256).map(|sample| sample as f32 * 0.25).collect();
    let control: Vec<f32> = (0..128).map(|frame| frame as f32 * 0.5).collect();
    let mut chain = EffectChain::new();
    chain.effects.push(Box::new(BorrowedDispatchProbe));
    chain.prepare_block(128).unwrap();
    for input in [
        AudioProcessInput::Main,
        AudioProcessInput::Sidechain(&key),
        AudioProcessInput::Control(&control),
    ] {
        for _ in 0..2 {
            let mut output = [0.0; 256];
            assert_eq!(
                crate::test_alloc::operations(|| chain.process_with_context(
                    &mut output,
                    AudioProcessContext {
                        sample_rate: RATE,
                        transport: Some(&transport),
                        input
                    }
                )),
                0
            );
            for (sample, actual) in output.iter().enumerate() {
                let extra = match input {
                    AudioProcessInput::Main => 0.0,
                    AudioProcessInput::Sidechain(key) => key[sample],
                    AudioProcessInput::Control(control) => control[sample / 2],
                };
                assert_eq!(
                    *actual,
                    (47_936 + sample / 2) as f32 + extra,
                    "sample {sample}"
                );
            }
            assert_eq!(weak.strong_count(), 1);
        }
    }
    drop(transport);
    assert_eq!(weak.strong_count(), 0);
}

#[derive(Clone, Copy)]
enum RampInput {
    Main,
    Key,
    Control,
}

struct RampDispatchProbe {
    input: RampInput,
    amount: f64,
}

impl RampDispatchProbe {
    fn render(
        &self,
        buffer: &mut [f32],
        transport: Option<PluginTransport>,
        key: Option<&[f32]>,
        control: Option<&[f32]>,
    ) {
        let transport = transport.expect("ramped block supplies transport");
        let tempo = transport.current_tempo() as f32;
        for (frame, samples) in buffer.chunks_exact_mut(2).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                let signal = key.map_or_else(
                    || control.map_or(0.0, |values| values[frame]),
                    |values| values[frame * 2 + channel],
                );
                *sample = (transport.sample_pos + frame as u64) as f32
                    + tempo
                    + signal
                    + (self.amount * 1024.0) as f32;
            }
        }
    }
}

impl AudioEffect for RampDispatchProbe {
    fn process(&mut self, _buffer: &mut [f32], _rate: u32) {
        panic!("missing dispatch context");
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.render(buffer, transport, None, None);
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.render(buffer, transport, Some(key), None);
    }
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.render(buffer, transport, None, Some(control));
    }
    fn sidechain_source(&self) -> Option<&str> {
        matches!(self.input, RampInput::Key).then_some("key")
    }
    fn control_source(&self) -> Option<&str> {
        matches!(self.input, RampInput::Control).then_some("control")
    }
    fn reset(&mut self) {}
    fn set_param(&mut self, name: &str, value: f64) {
        assert_eq!(name, "amount");
        self.amount = value;
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        (name == "amount").then_some(self.amount)
    }
    fn name(&self) -> &str {
        "ramp-dispatch-probe"
    }
}

#[test]
fn pending_parameter_ramps_preserve_main_key_control_spans_and_tempo_seams_without_heap_work() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n").unwrap();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
    let owner = PluginTransport::new(47_934, RATE, 120.0).with_clock(clock);
    let total = crate::touch::release_fade_samples(RATE);
    assert!(total > 128 && total.is_multiple_of(PARAM_RAMP_STEP));
    let keys = HashMap::from([(
        "key".into(),
        (0..256).map(|sample| sample as f32 * 0.25).collect(),
    )]);
    let controls = HashMap::from([(
        "control".into(),
        (0..128).map(|frame| frame as f32 * 0.5).collect(),
    )]);
    for input in [RampInput::Main, RampInput::Key, RampInput::Control] {
        let mut chain = EffectChain::new();
        chain
            .effects
            .push(Box::new(RampDispatchProbe { input, amount: 0.0 }));
        chain.prepare_block(128).unwrap();
        chain
            .fade_params(&[(0, "amount".into(), 0.0, 1.0)], RATE)
            .unwrap();
        assert_eq!(chain.ramps.len(), 1);
        for block in 0..(total as usize / 128 + 2) {
            let position = block * 128;
            let transport = owner.at_sample(owner.sample_pos + position as u64);
            let mut output = [0.0; 256];
            assert_eq!(
                crate::test_alloc::operations(|| chain.process_with_inputs(
                    &mut output,
                    &keys,
                    &controls,
                    RATE,
                    Some(transport.clone())
                )),
                0,
                "block {block}"
            );
            for (sample, actual) in output.iter().enumerate() {
                let frame = sample / 2;
                let absolute = owner.sample_pos + (position + frame) as u64;
                let tempo = if absolute < 48_000 { 120.0 } else { 60.0 };
                let signal = match input {
                    RampInput::Main => 0.0,
                    RampInput::Key => keys["key"][sample],
                    RampInput::Control => controls["control"][frame],
                };
                let step_end = (((position + frame) / PARAM_RAMP_STEP as usize + 1)
                    * PARAM_RAMP_STEP as usize)
                    .min(total as usize);
                let amount = step_end as f64 / f64::from(total);
                let expected = absolute as f32 + tempo + signal + (amount * 1024.0) as f32;
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "block {block} sample {sample}"
                );
            }
        }
        assert!(chain.ramps[0].done());
        assert_eq!(chain.effects[0].get_param("amount"), Some(1.0));
    }
}

#[test]
fn temporal_chain_admission_refuses_before_audio_or_ramps_for_all_inputs() {
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project().properties).unwrap());
    for mapped in [false, true] {
        let mut transport = PluginTransport::new(u64::MAX - 1, RATE, 137.0);
        if mapped {
            transport = transport.with_clock(clock.clone());
        }
        let key = [0.2; 4];
        let control = [0.3; 2];
        for input in [
            AudioProcessInput::Main,
            AudioProcessInput::Sidechain(&key),
            AudioProcessInput::Control(&control),
        ] {
            let mut chain = EffectChain::new();
            chain.effects.push(Box::new(RampDispatchProbe {
                input: RampInput::Main,
                amount: 0.0,
            }));
            chain
                .fade_params(&[(0, "amount".into(), 0.0, 1.0)], RATE)
                .unwrap();
            let expected = EffectChainProcessError::TransportRange(
                oc_audio::TransportRangeError::SamplePositionOverflow {
                    start: u64::MAX - 1,
                    frames: 2,
                },
            );
            let mut buffer = [0.7; 4];
            let mut result = Ok(());
            assert_eq!(
                crate::test_alloc::operations(|| {
                    result = chain.try_process_with_context(
                        &mut buffer,
                        AudioProcessContext {
                            sample_rate: RATE,
                            transport: Some(&transport),
                            input,
                        },
                    );
                }),
                0
            );
            assert_eq!(result, Err(expected));
            assert_eq!(buffer, [0.7; 4]);
            assert_eq!(chain.ramps[0].elapsed, 0);
            assert_eq!(chain.effects[0].get_param("amount"), Some(0.0));
            assert!(chain.processing_failed());
            // A different invalid call and a plain process cannot replace or
            // bypass the first refusal.
            assert_eq!(
                chain.try_process_with_context(
                    &mut buffer[..3],
                    AudioProcessContext {
                        sample_rate: RATE,
                        transport: None,
                        input: AudioProcessInput::Main
                    }
                ),
                Err(expected)
            );
            chain.process(&mut buffer, RATE);
            assert_eq!(buffer, [0.7; 4]);
            assert_eq!(
                chain.take_processing_failure(),
                oc_audio::ProcessingFailureReport::Ready(expected.to_string())
            );
            assert_eq!(
                chain.take_processing_failure(),
                oc_audio::ProcessingFailureReport::Reported
            );
            chain.reset();
            assert!(
                chain.processing_failed(),
                "reset does not authorize a failed chain"
            );
            chain.prepare_block(2).unwrap();
            assert!(!chain.processing_failed());
            assert_eq!(
                chain.take_processing_failure(),
                oc_audio::ProcessingFailureReport::Unavailable
            );
        }
    }
}

#[test]
fn temporal_legacy_input_admission_retains_first_error_and_explicit_prepare_recovers() {
    let keys = HashMap::from([("key".into(), vec![0.2; 4])]);
    let controls = HashMap::from([("control".into(), vec![0.3; 2])]);
    for input in [RampInput::Main, RampInput::Key, RampInput::Control] {
        let mut chain = EffectChain::new();
        chain
            .effects
            .push(Box::new(RampDispatchProbe { input, amount: 0.0 }));
        chain
            .fade_params(&[(0, "amount".into(), 0.0, 1.0)], RATE)
            .unwrap();
        let invalid = PluginTransport::new(u64::MAX, RATE, 137.0);
        let valid = PluginTransport::new(0, RATE, 137.0);
        let mut buffer = [0.7; 4];
        let expected = EffectChainProcessError::TransportRange(
            oc_audio::TransportRangeError::SamplePositionOverflow {
                start: u64::MAX,
                frames: 2,
            },
        );
        assert_eq!(
            crate::test_alloc::operations(|| chain.process_with_inputs(
                &mut buffer,
                &keys,
                &controls,
                RATE,
                Some(invalid)
            )),
            0
        );
        assert_eq!(chain.boundary.failure(), Some(expected));
        assert_eq!(chain.ramps[0].elapsed, 0);
        assert_eq!(buffer, [0.7; 4]);
        chain.prepare_block(2).unwrap();
        assert_eq!(
            crate::test_alloc::operations(|| chain
                .try_process_with_inputs(&mut buffer, &keys, &controls, RATE, Some(&valid))
                .unwrap()),
            0
        );
        assert_eq!(chain.ramps[0].elapsed, 2);
        assert_ne!(buffer, [0.7; 4]);
        assert!(!chain.processing_failed());
    }
}

#[test]
fn temporal_prepared_graph_refuses_flat_and_mapped_overflow_before_any_dsp() {
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project().properties).unwrap());
    for mapped in [false, true] {
        let mut graph = graph_with_one_gain();
        graph.prepare(2).unwrap();
        graph.track_chains[0]
            .fade_params(&[(0, "factor".into(), 0.5, 0.2)], RATE)
            .unwrap();
        let mut transport = PluginTransport::new(u64::MAX - 1, RATE, 137.0);
        if mapped {
            transport = transport.with_clock(clock.clone());
        }
        let mut tracks = vec![vec![0.7; 4]];
        let mut output = [0.8; 4];
        let aux = HashMap::new();
        let mut result = Ok(());
        assert_eq!(
            crate::test_alloc::operations(|| {
                result = graph.process_prepared(PreparedGraphBlock {
                    tracks: &mut tracks,
                    transport: Some(&transport),
                    aux: &aux,
                    output: &mut output,
                });
            }),
            0
        );
        assert_eq!(
            result,
            Err(PreparationError::TransportRange(
                oc_audio::TransportRangeError::SamplePositionOverflow {
                    start: u64::MAX - 1,
                    frames: 2
                }
            ))
        );
        assert_eq!(tracks[0], [0.7; 4]);
        assert_eq!(output, [0.8; 4]);
        assert_eq!(graph.track_chains[0].ramps[0].elapsed, 0);
        assert_eq!(
            graph.track_chains[0].effects[0].get_param("factor"),
            Some(0.5)
        );
        graph.prepare(2).unwrap();
        let valid = PluginTransport::new(0, RATE, 137.0);
        assert_eq!(
            crate::test_alloc::operations(|| graph
                .process_prepared(PreparedGraphBlock {
                    tracks: &mut tracks,
                    transport: Some(&valid),
                    aux: &aux,
                    output: &mut output
                })
                .unwrap()),
            0
        );
        assert_eq!(graph.track_chains[0].ramps[0].elapsed, 2);
    }
}

#[test]
fn temporal_empty_and_exclusive_end_boundaries_remain_admitted() {
    let mut chain = EffectChain::new();
    chain.effects.push(Box::new(BorrowedDispatchProbe));
    let end = PluginTransport::new(u64::MAX - 2, RATE, 120.0);
    let mut output = [0.0; 4];
    assert_eq!(
        crate::test_alloc::operations(|| chain
            .try_process_with_context(
                &mut output,
                AudioProcessContext {
                    sample_rate: RATE,
                    transport: Some(&end),
                    input: AudioProcessInput::Main
                }
            )
            .unwrap()),
        0
    );
    assert!(!chain.processing_failed());
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project().properties).unwrap());
    for transport in [
        end.at_sample(u64::MAX),
        end.at_sample(u64::MAX).with_clock(clock),
    ] {
        assert_eq!(
            crate::test_alloc::operations(|| chain
                .try_process_with_context(
                    &mut [],
                    AudioProcessContext {
                        sample_rate: RATE,
                        transport: Some(&transport),
                        input: AudioProcessInput::Main
                    }
                )
                .unwrap()),
            0
        );
    }
    let mut graph = graph_with_one_gain();
    graph.prepare(2).unwrap();
    let mut tracks = vec![Vec::new()];
    let aux = HashMap::new();
    let transport = end.at_sample(u64::MAX);
    assert_eq!(
        crate::test_alloc::operations(|| graph
            .process_prepared(PreparedGraphBlock {
                tracks: &mut tracks,
                transport: Some(&transport),
                aux: &aux,
                output: &mut []
            })
            .unwrap()),
        0
    );
}
