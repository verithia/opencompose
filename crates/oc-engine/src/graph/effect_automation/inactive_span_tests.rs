/// Inactive boundaries do not become new native processing boundaries.
#[test]
fn inactive_envelope_skip_preserves_native_fallback_segments() {
    let project = project("@project\n  tempo: 120\n");
    let mut lane = lane(
        "filter.cutoff",
        960,
        &[(BarBeat::new(1, 1, 5), ParamValue::Number(0.8))],
    );
    lane.time_range = Some(TimeRange {
        start: BarBeat::new(1, 1, 4),
        end: BarBeat::new(1, 1, 8),
    });
    let compiled = compile(
        &plugin_effect(),
        &[&lane],
        &project.properties,
        48_000,
        "track:0",
        0,
    )
    .unwrap();
    let segments = Arc::new(Mutex::new(Vec::new()));
    let mut actual = AutomatedEffect::new(
        Box::new(Recorder {
            current: 0.0,
            segments: segments.clone(),
        }),
        compiled.lanes,
        48_000,
        None,
        compiled.clock,
    )
    .unwrap();
    actual.process_with_transport(
        &mut [0.0; 512],
        48_000,
        Some(PluginTransport::new(0, 48_000, 120.0)),
    );
    // The first point holds from the clip head. Neither its later authored
    // point nor the clip end creates an extra processing call.
    assert_eq!(*segments.lock().unwrap(), [(0, 100, 0.0), (100, 156, 0.8)]);
}

/// The per-sample evaluator is the oracle for events and smoothing state.
#[test]
fn inactive_envelope_skip_preserves_events_and_partition_boundaries() {
    let project = project("@project\n  tempo: 120\n");
    let mut envelope = lane_with(
        "filter.cutoff",
        960,
        3.0,
        &[
            (BarBeat::new(1, 1, 5), ParamValue::Number(0.2)),
            (BarBeat::new(1, 1, 7), ParamValue::Number(0.9)),
        ],
    );
    envelope.time_range = Some(TimeRange {
        start: BarBeat::new(1, 1, 4),
        end: BarBeat::new(1, 1, 8),
    });
    let mut unbounded = envelope.clone();
    unbounded.time_range = None;
    let lfo = wobble_lane(LfoSpeed::Hz { hz: 2.0 });
    for lane in [&envelope, &unbounded, &lfo] {
        for block in [1, 17, 128, 1024] {
            let compiled = compile(
                &plugin_effect(),
                &[lane],
                &project.properties,
                48_000,
                "track:0",
                0,
            )
            .unwrap();
            let mut oracle =
                compile_lane("cutoff", lane, None, &compiled.clock, 48_000, "track:0", 0).unwrap();
            let segments = Arc::new(Mutex::new(Vec::new()));
            let mut actual = AutomatedEffect::new(
                Box::new(Recorder {
                    current: 0.0,
                    segments,
                }),
                compiled.lanes,
                48_000,
                None,
                compiled.clock,
            )
            .unwrap();
            for start in (0..1300).step_by(block) {
                let frames = block.min(1300 - start) as u64;
                let mut expected = Vec::new();
                for offset in 0..frames {
                    let tempo = oracle
                        .input
                        .timeline()
                        .unwrap()
                        .lfo_follows_tempo()
                        .then(|| {
                            actual
                                .clock
                                .sample_tempo_at(start as u64 + offset, 48_000)
                                .unwrap()
                        });
                    if let Some(value) = oracle.next_value(tempo, None)
                        && oracle.applied != Some(value)
                    {
                        expected.push((offset, 0, value));
                        oracle.applied = Some(value);
                    }
                }
                let mut events = Vec::new();
                actual.collect_events(&mut events, start as u64, frames, 48_000);
                assert_eq!(events, expected, "block {block}, start {start}");
                assert_eq!(actual.lanes[0].cursor, oracle.cursor);
                assert_eq!(
                    actual.lanes[0].filtered.to_bits(),
                    oracle.filtered.to_bits()
                );
                assert_eq!(actual.lanes[0].started, oracle.started);
                assert_eq!(actual.lanes[0].applied, oracle.applied);
            }
        }
    }
}

#[test]
fn inactive_envelope_skip_keeps_touch_hold_and_release_outside_clip() {
    let project = project("@project\n  tempo: 120\n");
    let mut lane = lane_with(
        "filter.cutoff",
        960,
        4.0,
        &[(BarBeat::new(1, 1, 20), ParamValue::Number(0.8))],
    );
    lane.time_range = Some(TimeRange {
        start: BarBeat::new(1, 1, 20),
        end: BarBeat::new(1, 1, 40),
    });
    let compiled = compile(
        &plugin_effect(),
        &[&lane],
        &project.properties,
        48_000,
        "track:0",
        0,
    )
    .unwrap();
    let registry = Arc::new(TouchRegistry::new());
    let key = touch::effect_key("track:0", 0, "cutoff");
    let mut actual = AutomatedEffect::new(
        Box::new(Recorder {
            current: 0.0,
            segments: Arc::new(Mutex::new(Vec::new())),
        }),
        compiled.lanes,
        48_000,
        Some(registry.clone()),
        compiled.clock,
    )
    .unwrap();
    registry.begin(&key, 0.1);
    let mut events = Vec::new();
    actual.collect_events(&mut events, 0, 64, 48_000);
    assert_eq!(events, [(0, 0, 0.1)]);
    registry.release(&key);
    events.clear();
    actual.collect_events(&mut events, 64, 400, 48_000);
    assert!(events.is_empty());
    assert!(!actual.lanes[0].started);
    assert_eq!(actual.lanes[0].applied, Some(0.1));
    actual.collect_events(&mut events, 464, 600, 48_000);
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|(offset, _, value)| *offset >= 36 && (0.1..=0.8).contains(value))
    );
    assert!(actual.lanes[0].started);
    let filtered = actual.lanes[0].filtered;
    let applied = actual.lanes[0].applied;
    events.clear();
    actual.collect_events(&mut events, 1064, 2000, 48_000);
    assert!(events.is_empty());
    assert_eq!(actual.lanes[0].filtered.to_bits(), filtered.to_bits());
    assert_eq!(actual.lanes[0].applied, applied);
    assert_eq!(actual.lanes[0].cursor, 3064);
}
