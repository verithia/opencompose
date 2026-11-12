//! Streaming smart-disable tests for hosted plugin effects.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::*;
use crate::idle_gate::{IdleGate, IdleGatedPlugin};
use oc_host::vst3_instance::test_support::FakeControl;

const BLOCK: usize = 128;
const SAMPLE_RATE: u32 = 48_000;

/// One track whose only device is the gated fake insert effect.
fn gated_effect_graph(opt_out: bool) -> (AudioGraph, Arc<FakeControl>) {
    use oc_host::vst3_instance::test_support::{FakeControl, fake_effect_instance};

    let control = Arc::new(FakeControl::default());
    // The fake insert passes its input through, so a silent lane produces
    // silent output the way a real effect on silence does.
    control.copy_input.store(true, Ordering::Relaxed);
    let instance = fake_effect_instance(&control, 2, 512);
    let gate = IdleGate::new(SAMPLE_RATE, instance.plugin_tail_samples(), opt_out);
    let mut chain = EffectChain::new();
    chain
        .effects
        .push(Box::new(IdleGatedPlugin::new(Box::new(instance), gate)));
    (plain_graph(chain), control)
}

fn plain_graph(chain: EffectChain) -> AudioGraph {
    let mut graph = AudioGraph {
        track_chains: vec![chain],
        bus_chains: HashMap::new(),
        master_chain: EffectChain::new(),
        master_gain: 1.0,
        ramps: MixerRamps::default(),
        routing: vec![TrackRouting {
            sends: vec![],
            aux_sends: vec![],
            volume: 1.0,
            pan: 0.0,
            mute: false,
            polarity_inverted: false,
            direct_to_master: true,
        }],
        sample_rate: SAMPLE_RATE,
        track_widths: vec![SourceWidth::Mono],
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
        track_names: vec!["lane".into()],
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
    graph
}

fn transport(position: u64) -> PluginTransport {
    PluginTransport::new(position, SAMPLE_RATE, 120.0)
}

fn calls(control: &FakeControl) -> usize {
    control.process_calls.load(Ordering::Relaxed)
}

/// Feed `count` blocks of `level` and return the output of the last block.
fn feed(
    graph: &mut AudioGraph,
    tracks: &mut [Vec<f32>],
    count: usize,
    level: f32,
    streaming: bool,
) -> Vec<f32> {
    let mut last = Vec::new();
    for block in 0..count {
        tracks[0].fill(level);
        let position = (block * BLOCK) as u64;
        last = if streaming {
            graph.mix_with_transport_and_aux(tracks, Some(transport(position)), &HashMap::new())
        } else {
            graph.mix_with_transport(tracks, Some(transport(position)))
        };
    }
    last
}

#[test]
fn a_silent_hosted_effect_pauses_after_the_idle_window() {
    let (mut graph, control) = gated_effect_graph(false);
    let mut tracks = vec![vec![0.0f32; BLOCK * 2]];
    let idle_blocks = (SAMPLE_RATE as usize * 4) / BLOCK;

    feed(&mut graph, &mut tracks, idle_blocks, 0.0, true);
    let paused = calls(&control);
    assert_eq!(
        paused, idle_blocks,
        "the fade-out block still runs the plugin"
    );

    feed(&mut graph, &mut tracks, 8, 0.0, true);
    assert_eq!(
        calls(&control),
        paused,
        "a paused effect must not keep processing"
    );
}

#[test]
fn a_transient_resumes_the_effect_within_one_block() {
    let (mut graph, control) = gated_effect_graph(false);
    let mut tracks = vec![vec![0.0f32; BLOCK * 2]];
    let idle_blocks = (SAMPLE_RATE as usize * 4) / BLOCK;
    // Six seconds of silence: four to pause, two more skipped.
    feed(&mut graph, &mut tracks, idle_blocks * 3 / 2, 0.0, true);
    let paused = calls(&control);
    assert_eq!(paused, idle_blocks);

    let muted = feed(&mut graph, &mut tracks, 1, 0.5, true);
    assert_eq!(
        calls(&control),
        paused + 1,
        "the first block with input resumes processing"
    );
    assert!(
        muted.iter().all(|sample| *sample == 0.0),
        "the resume block is muted so the fade can start from silence"
    );

    let ramped = feed(&mut graph, &mut tracks, 1, 0.5, true);
    assert_eq!(calls(&control), paused + 2, "the fade-in block runs too");
    assert_eq!(ramped[0], 0.0, "the fade-in starts at silence");
    assert!(
        ramped.iter().any(|sample| sample.abs() > 0.1),
        "the fade-in reaches the effect output"
    );

    // The effect continues to run while it has input again.
    feed(&mut graph, &mut tracks, 4, 0.5, true);
    assert_eq!(calls(&control), paused + 6);
}

#[test]
fn the_offline_entry_calls_the_effect_on_every_block() {
    let (mut graph, control) = gated_effect_graph(false);
    let mut tracks = vec![vec![0.0f32; BLOCK * 2]];
    let idle_blocks = (SAMPLE_RATE as usize * 4) / BLOCK;

    feed(&mut graph, &mut tracks, idle_blocks + 8, 0.0, false);
    assert_eq!(
        calls(&control),
        idle_blocks + 8,
        "an offline pass never skips a plugin block"
    );
}

#[test]
fn an_opted_out_effect_keeps_processing() {
    let (mut graph, control) = gated_effect_graph(true);
    let mut tracks = vec![vec![0.0f32; BLOCK * 2]];
    let idle_blocks = (SAMPLE_RATE as usize * 4) / BLOCK;

    feed(&mut graph, &mut tracks, idle_blocks + 8, 0.0, true);
    assert_eq!(
        calls(&control),
        idle_blocks + 8,
        "the opt-out keeps a silent plugin running"
    );
}

#[test]
fn a_long_tail_effect_keeps_processing() {
    use oc_host::vst3_instance::test_support::{FakeControl, fake_effect_instance};

    let control = Arc::new(FakeControl::default());
    control.copy_input.store(true, Ordering::Relaxed);
    // Ten seconds is longer than the idle window, so the gate stays off.
    control
        .tail_samples
        .store(10 * SAMPLE_RATE, Ordering::Relaxed);
    let instance = fake_effect_instance(&control, 2, 512);
    let gate = IdleGate::new(SAMPLE_RATE, instance.plugin_tail_samples(), false);
    assert!(!gate.is_active(), "a long tail disables the gate");
    let mut chain = EffectChain::new();
    chain
        .effects
        .push(Box::new(IdleGatedPlugin::new(Box::new(instance), gate)));
    let mut graph = plain_graph(chain);
    let mut tracks = vec![vec![0.0f32; BLOCK * 2]];
    let idle_blocks = (SAMPLE_RATE as usize * 4) / BLOCK;

    feed(&mut graph, &mut tracks, idle_blocks + 8, 0.0, true);
    assert_eq!(calls(&control), idle_blocks + 8);
}

#[test]
fn the_smart_disable_parameter_opts_out_by_default() {
    use oc_dsl::ast::{Effect, EffectType, ParamValue};

    let mut effect = Effect {
        effect_type: EffectType::Plugin("test.fake.effect".into()),
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    assert!(
        smart_disable(&effect),
        "the gate is on unless the effect opts out"
    );

    effect
        .params
        .push(("smart_disable".into(), ParamValue::Bool(false)));
    assert!(!smart_disable(&effect));
    effect.params[0].1 = ParamValue::Bool(true);
    assert!(smart_disable(&effect));
}

#[test]
fn a_built_chain_gates_a_hosted_plugin_only() {
    use oc_dsl::ast::{Effect, EffectType};

    let plugin = Effect {
        effect_type: EffectType::Plugin("test.fake.effect".into()),
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[plugin], SAMPLE_RATE).expect("fake insert loads");
    assert!(
        chain.effects[0].plugin_tail_samples().is_some(),
        "the hosted effect carries its tail"
    );

    let gain = Effect {
        effect_type: EffectType::Gain,
        params: Vec::new(),
        bypass: false,
        sidechain_source: None,
    };
    let chain = EffectChain::try_from_effects(&[gain], SAMPLE_RATE).expect("gain loads");
    assert!(chain.effects[0].plugin_tail_samples().is_none());
}
