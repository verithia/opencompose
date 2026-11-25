//! Prepared compact curves keep the audio-time evaluator allocation-free.

use oc_dsl::ast::Curve;
use oc_dsl::automation::LaneCurve;
use oc_dsl::timing::ProjectClock;
use oc_test_support::operations;

#[test]
fn prepared_insert_mix_processes_and_seeks_without_allocating() {
    use oc_audio::PluginTransport;
    use oc_engine::graph::AudioGraph;

    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=2, slot/wet=25%\n  automation: fx1.slot/wet [1:2 - 1:4] {1:2=0.25, 1:3=0.75}\n",
    ).unwrap();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    let chain = &mut graph.track_chains[0];
    chain.prepare_block(513).unwrap();
    let mut stereo = [1.0; 1026];
    let allocations = operations(|| {
        for position in [0, 24_000, 24_513, 72_000, 0] {
            stereo.fill(1.0);
            chain.process_with_transport(
                &mut stereo,
                48_000,
                Some(PluginTransport::new(position, 48_000, 120.0)),
            );
            std::hint::black_box(&stereo);
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(stereo, [1.25; 1026]);
}

#[test]
fn prepared_exponential_mapping_and_jumps_do_not_allocate_when_sampled_or_sought() {
    let mut project = oc_dsl::parse(
        "@project\n  tempo: 120\n@bus out\n  effects:\n    - gain: factor=1\n  automation: fx1.factor {1:1=0, 1:2=1, 1:2=0, 1:3=1}\n",
    )
    .unwrap();
    let clock = ProjectClock::new(&project.properties).unwrap();
    let lane = &mut project.buses[0].automation[0];
    lane.preserve_discontinuities = true;
    lane.points[0].curve = Curve::Exponential;
    lane.points[0].curvature = Some(-3.0);
    lane.points[2].curve = Curve::Exponential;
    lane.points[2].curvature = Some(3.0);
    lane.value_mapping = Some(
        serde_json::from_value(serde_json::json!({
            "curve": "exponential", "curvature": 2.0, "scale": 1.5, "offset": 0.1
        }))
        .unwrap(),
    );
    let curve = LaneCurve::new(lane, &clock, 48_000, |value| {
        value
            .as_f64()
            .ok_or_else(|| "expected a numeric anchor".into())
    })
    .unwrap();
    assert!(curve.envelope_value(23_999.0).unwrap() > 1.59);
    assert_eq!(curve.envelope_value(24_000.0), Some(0.1));
    lane.value_mapping
        .as_mut()
        .unwrap()
        .input_quantization_steps = Some(128);
    let quantized = LaneCurve::new(lane, &clock, 48_000, |value| {
        value
            .as_f64()
            .ok_or_else(|| "expected a numeric anchor".into())
    })
    .unwrap();
    let gain_project = oc_dsl::parse(
        "@project\n  tempo: 120\n@bus out\n  gain_automation: segments [1:1 - 1:3] discontinuities mapping(exponential, curvature=2, input_quantization_steps=128) {1:1=0 curve=exponential curvature=-3, 1:2=1, 1:2=0, 1:3=1}\n",
    )
    .unwrap();
    let gain = oc_dsl::gain_automation::compile_segments(
        &gain_project.buses[0].gain_automation[0],
        &clock,
        48_000,
    )
    .unwrap();
    assert_eq!(gain.envelope_value(24_000.0), Some(0.0));
    assert_eq!(gain.envelope_value(48_000.0), None);
    let allocations = operations(|| {
        for sample in 0..48_000 {
            std::hint::black_box(curve.envelope_value(sample as f64));
            std::hint::black_box(quantized.envelope_value(sample as f64));
            std::hint::black_box(gain.envelope_value(sample as f64));
        }
        for sample in [47_999.0, 0.0, 23_999.0, 24_000.0, 12_000.0] {
            std::hint::black_box(curve.envelope_value(sample));
            std::hint::black_box(quantized.envelope_value(sample));
            std::hint::black_box(gain.envelope_value(sample));
        }
    });
    assert_eq!(allocations, 0, "prepared curve sampling allocated");
}
