//! Control edges: a kick's control output drives a compressor's control input.
//!
//! The control input is a two-input node's silent port: the signal shapes the
//! device but never reaches the audio sum. These tests drive the graph with
//! synthetic buffers so the depth and the onset sample are exact, then repeat
//! the render in blocks to prove the control is evaluated before its consumer
//! rather than one block late.

use oc_audio::PluginTransport;
use oc_dsl::ast::{Project, RouteKind};
use oc_engine::graph::AudioGraph;

const RATE: u32 = 48_000;
const FRAMES: usize = 48_000;
const ONSET: usize = 1_000;
const KICK_END: usize = 4_000;

/// Kick control edge into a bass-linked compressor.
const PROJECT: &str = "@project \"control route\"\n  tempo: 120\n  time_signature: 4/4\n\n@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms\n\n@track kick | instrument: drums/kick\n\n@track bass | instrument: synth/bass\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, attack=0.1ms, release=100ms, knee=0dB, makeup=0dB, mix=100%\n  route: control kick_env -> compression\n";

fn project() -> Project {
    oc_dsl::parse(PROJECT).expect("control route project parses")
}

fn transport(sample_pos: u64) -> PluginTransport {
    PluginTransport::new(sample_pos, RATE, 120.0)
}

/// A unit step on the kick and a constant bass, both mono.
fn kick_and_bass() -> (Vec<f32>, Vec<f32>) {
    let mut kick = vec![0.0_f32; FRAMES * 2];
    for frame in ONSET..KICK_END {
        kick[frame * 2] = 1.0;
        kick[frame * 2 + 1] = 1.0;
    }
    let mut bass = vec![0.0_f32; FRAMES * 2];
    for frame in 0..FRAMES {
        bass[frame * 2] = 0.5;
        bass[frame * 2 + 1] = 0.5;
    }
    (kick, bass)
}

fn graph_for(project: &Project) -> AudioGraph {
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(project, RATE, &[]).expect("graph");
    // Track 0 feeds the control follower only; it must not reach the mix, so
    // the measured gain is the bass path only.
    graph.set_control_only_tracks(&[0]);
    graph
}

/// Render block by block with one buffer per track, returning the stereo mix.
fn render(project: &Project, buffers: &[Vec<f32>], block: usize) -> Vec<f32> {
    let mut graph = graph_for(project);
    let mut output = Vec::with_capacity(FRAMES * 2);
    let mut position = 0;
    while position < FRAMES {
        let end = (position + block).min(FRAMES);
        let mut chunk: Vec<Vec<f32>> = buffers
            .iter()
            .map(|buffer| buffer[position * 2..end * 2].to_vec())
            .collect();
        output.extend(graph.mix_with_transport(&mut chunk, Some(transport(position as u64))));
        position = end;
    }
    output
}

fn plain_project() -> Project {
    let mut plain = project();
    plain.tracks[1].effects.clear();
    plain.tracks[1].input_routes.clear();
    plain
}

/// Gain in dB per sample from the ducked and the plain bass render.
fn gain_db(ducked: &[f32], plain: &[f32]) -> Vec<f64> {
    ducked
        .chunks_exact(2)
        .zip(plain.chunks_exact(2))
        .map(|(ducked, plain)| {
            let reference = f64::from(plain[0]).abs().max(1e-9);
            20.0 * (f64::from(ducked[0]).abs().max(1e-9) / reference).log10()
        })
        .collect()
}

fn first_below(gain: &[f64], threshold_db: f64) -> Option<usize> {
    gain.iter().position(|value| *value < threshold_db)
}

#[test]
fn a_kick_control_edge_ducks_the_bass_by_the_configured_depth() {
    let (kick, bass) = kick_and_bass();
    let buffers = [kick, bass];
    let mixed = render(&project(), &buffers, 512);
    let plain = render(&plain_project(), &buffers, 512);
    let gain = gain_db(&mixed, &plain);
    assert_eq!(gain.len(), FRAMES);

    let plateau = ONSET + (0.010 * RATE as f64) as usize..ONSET + (0.050 * RATE as f64) as usize;
    let depth = gain[plateau]
        .iter()
        .fold(0.0_f64, |deep, value| deep.min(*value));
    println!("control-edge ducking depth {depth:.3} dB (expected -18)");
    assert!(
        (depth + 18.0).abs() < 0.1,
        "ducking depth {depth:.3} dB, expected -18 dB within 0.1 dB"
    );

    // The control is evaluated before its consumer: the first ducked sample is
    // the onset sample, never the next block. With a 0.1 ms compressor attack
    // the first sample already carries several dB of reduction.
    assert!(
        gain[ONSET - 1] > -0.001,
        "pre-onset gain {:.6}",
        gain[ONSET - 1]
    );
    assert!(
        gain[ONSET] < -0.5,
        "onset sample gain {:.3} dB; a one-block-late duck would sit at 0",
        gain[ONSET]
    );
    assert_eq!(first_below(&gain, -0.5), Some(ONSET));

    // Release: after the kick stops, follower and compressor return to unity.
    let end = KICK_END + 1;
    let at = |millis: f64| gain[(end as f64 + millis * RATE as f64 / 1000.0) as usize];
    println!(
        "release after kick: +50ms {:.2} dB, +150ms {:.2} dB, +400ms {:.2} dB",
        at(50.0),
        at(150.0),
        at(400.0)
    );
    assert!(at(150.0) < -1.0, "still ducked 150 ms after the kick");
    assert!(at(700.0) > -1.0, "released 700 ms after the kick");
    assert!(gain[FRAMES - 1] > -0.1, "tail returns to unity");
}

#[test]
fn the_control_input_contributes_nothing_to_the_audio_sum() {
    // The bass is silent and the control is full scale. If the control port
    // summed anywhere, this render would not be digital silence.
    let frames = ONSET + 512;
    let (kick, _) = kick_and_bass();
    let silent_bass = vec![0.0_f32; FRAMES * 2];
    let mut graph = graph_for(&project());
    let mut chunk: Vec<Vec<f32>> = vec![
        kick[..frames * 2].to_vec(),
        silent_bass[..frames * 2].to_vec(),
    ];
    let output = graph.mix_with_transport(&mut chunk, Some(transport(0)));
    let peak = output
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "control input leaked into the audio sum: peak {peak}"
    );
    // The control did arrive at full scale, and the device is ducking.
    let values = graph.control_values("kick_env").expect("control values");
    assert_eq!(values.len(), frames);
    assert!(
        values
            .iter()
            .skip(ONSET)
            .all(|value| (*value - 1.0).abs() < 1e-6),
        "control did not reach full scale: {:?}",
        &values[ONSET..ONSET + 8]
    );
    let reduction = graph.track_chains[1].effects[0].gain_reduction_db();
    assert!(
        reduction > 16.0,
        "the compressor did not duck on the control input: {reduction:.3} dB"
    );
}

#[test]
fn the_render_is_block_size_invariant() {
    let (kick, bass) = kick_and_bass();
    let buffers = [kick, bass];
    let whole = render(&project(), &buffers, FRAMES);
    let blocked = render(&project(), &buffers, 64);
    assert_eq!(whole.len(), blocked.len());
    let mut worst = 0.0_f32;
    for (a, b) in whole.iter().zip(&blocked) {
        worst = worst.max((a - b).abs());
    }
    assert!(
        worst < 1e-6,
        "a control edge must not depend on the block split; worst {worst}"
    );
}

#[test]
fn one_control_source_fans_out_to_two_consumers() {
    let text = format!(
        "{PROJECT}\n@track bass_b | instrument: synth/bass\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, attack=0.1ms, release=100ms, knee=0dB, makeup=0dB, mix=100%\n  route: control kick_env -> compression\n"
    );
    let fanout = oc_dsl::parse(&text).expect("fan-out parses");
    assert_eq!(fanout.tracks.len(), 3);
    // Both consumers get the same control, so the two-track sum equals the
    // single-track renders added together. Render each consumer alone.
    let mut single = fanout.clone();
    single.tracks[2].mix.mute = true;
    let mut other = fanout.clone();
    other.tracks[1].mix.mute = true;

    let (kick, bass) = kick_and_bass();
    let buffers = [kick, bass.clone(), bass];
    let both = render(&fanout, &buffers, 512);
    let first = render(&single, &buffers, 512);
    let second = render(&other, &buffers, 512);
    assert_eq!(both.len(), first.len());
    let mut worst = 0.0_f32;
    for (sum, (a, b)) in both.iter().zip(first.iter().zip(second.iter())) {
        worst = worst.max((sum - (a + b)).abs());
    }
    assert!(worst < 1e-5, "fan-out sum differs by {worst}");

    let mut one = fanout.clone();
    one.tracks.drain(2..);
    let one_render = render(&one, &buffers[..2], 512);
    let plain = render(&plain_project(), &buffers[..2], 512);
    let gain = gain_db(&one_render, &plain);
    let depth = gain[ONSET + 500..ONSET + 2_000]
        .iter()
        .fold(0.0_f64, |deep, value| deep.min(*value));
    assert!((depth + 18.0).abs() < 0.1, "fan-out depth {depth:.3} dB");
}

#[test]
fn an_audio_route_feeds_the_key_without_a_control_source() {
    // The same two-input node with the default `audio` kind: the kick's
    // post-insert audio is the key. This is the legacy key path as an edge.
    let text = PROJECT
        .replace(
            "route: control kick_env -> compression",
            "route: kick -> compression",
        )
        .replace(
            "@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms\n\n",
            "",
        );
    let project = oc_dsl::parse(&text).expect("audio route parses");
    assert_eq!(project.tracks[1].input_routes[0].kind, RouteKind::Audio);
    let (kick, bass) = kick_and_bass();
    let buffers = [kick, bass];
    let mixed = render(&project, &buffers, 512);
    let plain = render(&plain_project(), &buffers, 512);
    let gain = gain_db(&mixed, &plain);
    let depth = gain[ONSET + 500..ONSET + 2_000]
        .iter()
        .fold(0.0_f64, |deep, value| deep.min(*value));
    println!("audio key route ducking depth {depth:.3} dB");
    assert!(
        (depth + 18.0).abs() < 0.2,
        "audio route depth {depth:.3} dB, expected -18 dB"
    );
}

#[test]
fn a_control_route_to_an_audio_only_input_is_refused() {
    let text = PROJECT
        .replace(
            "- compression: threshold=-24dB, ratio=4:1, attack=0.1ms, release=100ms, knee=0dB, makeup=0dB, mix=100%",
            "- eq: mid_freq=1kHz",
        )
        .replace(
            "route: control kick_env -> compression",
            "route: control kick_env -> eq",
        );
    let project = oc_dsl::parse(&text).expect("text parses");
    let errors = oc_dsl::validate(&project).expect_err("validator refuses");
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("does not accept a control signal"),
        "readable refusal missing: {message}"
    );
    assert!(message.contains("eq"), "{message}");

    let graph_error = AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[])
        .err()
        .expect("graph build refuses");
    assert!(
        graph_error.contains("does not accept a control input"),
        "{graph_error}"
    );
}

#[test]
fn a_track_cannot_feed_its_own_control_input() {
    let text = PROJECT.replace(
        "from: track(kick), attack: 0ms, release: 100ms",
        "from: track(bass), attack: 0ms, release: 100ms",
    );
    let project = oc_dsl::parse(&text).expect("self-feed parses");
    let errors = oc_dsl::validate(&project).expect_err("validator refuses a self-feed");
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("would close a loop"), "{message}");
    let graph_error = AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[])
        .err()
        .expect("graph build refuses");
    assert!(
        graph_error.contains("cannot feed control source"),
        "{graph_error}"
    );
}

#[test]
fn an_undeclared_control_source_is_refused() {
    let text = PROJECT.replace("route: control kick_env ->", "route: control ghost ->");
    let project = oc_dsl::parse(&text).expect("text parses");
    let errors = oc_dsl::validate(&project).expect_err("validator refuses");
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("Unknown control route source 'ghost'"),
        "{message}"
    );
}

#[test]
fn an_unresolved_control_source_keeps_the_legacy_one_input_behavior() {
    // The legacy infallible builder must not panic when the AST has a route
    // whose control source was removed; the device simply gets no control.
    let mut project = project();
    project.properties.control_sources.clear();
    let mut graph = AudioGraph::from_project_with_source_latencies(&project, RATE, &[]);
    graph.set_control_only_tracks(&[0]);
    let (kick, bass) = kick_and_bass();
    let output = graph.mix_with_transport(
        &mut [kick[..1_024].to_vec(), bass[..1_024].to_vec()],
        Some(transport(0)),
    );
    assert!(output.iter().all(|sample| sample.is_finite()));
}

#[test]
fn two_tracks_cannot_feed_each_other_through_control() {
    let text = "@project \"control loop\"\n@control_source a_env | from: track(a), attack: 0ms, release: 100ms\n@control_source b_env | from: track(b), attack: 0ms, release: 100ms\n@track a | instrument: synth/lead\n  effects:\n    - compression: ratio=4:1\n  route: control b_env -> compression\n@track b | instrument: synth/bass\n  effects:\n    - compression: ratio=4:1\n  route: control a_env -> compression\n";
    let project = oc_dsl::parse(text).expect("loop project parses");
    let errors = oc_dsl::validate(&project).expect_err("validator refuses the loop");
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("close a loop between"), "{message}");
    assert!(
        message.contains("'a'") && message.contains("'b'"),
        "{message}"
    );
    let graph_error = AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[])
        .err()
        .expect("graph build refuses the loop");
    assert!(
        graph_error.contains("close a loop between tracks"),
        "{graph_error}"
    );
}
