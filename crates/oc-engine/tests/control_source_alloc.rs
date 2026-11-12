//! The control follower and its gain ride must not allocate per block.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_dsl::ast::{ControlCurve, Project};
use oc_dsl::formula::Formula;
use oc_engine::dsp::control_source::{ControlFollower, ControlSettings};
use oc_engine::graph::AudioGraph;
use oc_test_support::operations;

fn settings() -> ControlSettings {
    ControlSettings {
        attack_seconds: 0.005,
        release_seconds: 0.12,
        lfo: Some((oc_dsl::ast::ControlLfoShape::Sine, 3.0)),
        lfo_depth: 0.25,
        formula: Some(Formula::parse("1-x*x").unwrap()),
        scale: 1.0,
        offset: 0.0,
        curve: ControlCurve::Sqrt,
    }
}

fn program(frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        let t = frame as f64 / 48_000.0;
        let burst = if (frame / 4_800) % 2 == 0 { 0.8 } else { 0.0 };
        let sample = ((std::f64::consts::TAU * 90.0 * t).sin() * burst) as f32;
        buffer[frame * 2] = sample;
        buffer[frame * 2 + 1] = sample;
    }
    buffer
}

#[test]
fn the_control_path_does_not_allocate_per_block() {
    let mut follower = ControlFollower::new(48_000, settings());
    let source = program(24_000);
    let mut control = vec![0.0; 24_000];

    for _ in 0..3 {
        follower.process(&source, &mut control);
    }

    let allocations = operations(|| {
        for _ in 0..16 {
            follower.process(&source, &mut control);
        }
    });

    assert_eq!(
        allocations, 0,
        "control follower allocated in the audio path"
    );

    graph_adds_no_allocation_for_a_gain_ride();
}

fn graph_project(ride: bool) -> Project {
    let gain = if ride {
        "  mix: control(kick_env=-12dB)\n"
    } else {
        ""
    };
    oc_dsl::parse(&format!(
        "@project\n\n@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms\n\n@track kick | instrument: drums/kick\n\n@track bass | instrument: synth/bass\n{gain}"
    ))
    .unwrap()
}

fn graph_adds_no_allocation_for_a_gain_ride() {
    let with_ride = graph_project(true);
    let without_ride = graph_project(false);
    let frames = 512;
    let mut buffers = vec![vec![0.0; frames * 2]; 2];
    for frame in 0..frames {
        let value = ((frame as f32 * 0.01).sin() * 0.5).abs();
        buffers[0][frame * 2] = value;
        buffers[0][frame * 2 + 1] = value;
        buffers[1][frame * 2] = 0.25;
        buffers[1][frame * 2 + 1] = 0.25;
    }

    let count = |project: &Project| {
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(project, 48_000, &[]).unwrap();
        let mut work = buffers.clone();
        // Warm up the per-block control buffer and the graph's own scratch.
        for _ in 0..2 {
            graph.mix_processed(&mut work, None);
        }
        operations(|| {
            for _ in 0..8 {
                graph.mix_processed(&mut work, None);
            }
        })
    };

    let plain = count(&without_ride);
    let ridden = count(&with_ride);
    assert!(plain > 0, "the graph itself allocates a master buffer");
    assert_eq!(
        ridden,
        plain,
        "a gain ride added {} allocations to the audio path",
        ridden - plain
    );
}
