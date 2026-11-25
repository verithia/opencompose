use oc_dsl::ast::EffectType;
use oc_engine::dsp::try_create_effect;
use oc_test_support::operations;

#[test]
fn stereo_matrix_roundtrips_and_applies_negative_gain_with_ordered_delay() {
    for rate in [8_000, 44_100, 48_000] {
        for before in [true, false] {
            let source = format!(
                "@project\n@track test | instrument: audio/clip\n  effects:\n    - stereo_matrix: left_from_left=0%, left_from_right=-100%, right_from_left=100%, right_from_right=0%, offset=10ms, delay_before_matrix={before}\n"
            );
            let project = oc_dsl::parse(&source).unwrap();
            oc_dsl::validate(&project).unwrap();
            assert_eq!(
                oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap(),
                project
            );
            let effect = &project.tracks[0].effects[0];
            assert_eq!(effect.effect_type, EffectType::StereoMatrix);
            let mut device = try_create_effect(&effect.effect_type, &effect.params, rate).unwrap();
            let frames = (rate / 100) as usize;
            assert_eq!(device.tail_samples(), frames as u32);
            assert_eq!(device.latency_samples(), 0);

            let mut full = vec![0.0; (frames + 3) * 2];
            full[0..2].copy_from_slice(&[0.5, 0.25]);
            let mut chunked = full.clone();
            device.process(&mut full, rate);
            device.reset();
            for block in chunked.chunks_mut(34) {
                device.process(block, rate);
            }
            assert_eq!(chunked, full);

            let expected = if before {
                [(1, 0.5), (frames * 2, -0.25)]
            } else {
                [(0, -0.25), (frames * 2 + 1, 0.5)]
            };
            for (index, sample) in full.into_iter().enumerate() {
                let value = expected
                    .iter()
                    .find(|(ordinal, _)| *ordinal == index)
                    .map_or(0.0, |(_, value)| *value);
                assert_eq!(sample, value, "sample {index}, before={before}");
            }
        }
    }
}

#[test]
fn stereo_matrix_uses_prepared_history_for_automation_and_clears_it_on_reset() {
    let mut fx = try_create_effect(&EffectType::StereoMatrix, &[], 8_000).unwrap();
    let mut initial = [0.25, 0.5];
    fx.process(&mut initial, 8_000);
    assert_eq!(initial, [0.25, 0.5]);
    let mut next = [0.0, 0.0];
    let mut reset = [0.0, 0.0];
    let count = operations(|| {
        fx.set_param("offset", -0.125);
        fx.process(&mut next, 8_000);
        fx.reset();
        fx.process(&mut reset, 8_000);
    });
    assert_eq!(count, 0);
    assert_eq!(next, [0.25, 0.0]);
    assert_eq!(reset, [0.0, 0.0]);
}

#[test]
fn stereo_matrix_rejects_out_of_range_and_incompatible_parameter_types() {
    for effect in [
        "offset=-501ms",
        "offset=6%",
        "left_from_right=-201%",
        "delay_before_matrix=2",
        "right_from_right=100%, right_from_right=50%",
    ] {
        let input = format!(
            "@project\n@track test | instrument: audio/clip\n  effects:\n    - stereo_matrix: {effect}\n"
        );
        let project = oc_dsl::parse(&input).unwrap();
        let effect = &project.tracks[0].effects[0];
        assert!(try_create_effect(&effect.effect_type, &effect.params, 48_000).is_err());
    }
}

#[test]
fn stereo_matrix_tail_covers_a_signed_offset_that_starts_after_the_first_block() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n@track test | instrument: audio/clip\n  effects:\n    - stereo_matrix\n  automation: fx1.offset {1:1=0ms curve=stairs, 1:2=-100ms}\n").unwrap();
    let tail = oc_engine::graph::track_chain_tail_seconds(&project, 0, 48_000).unwrap();
    assert_eq!(tail.latency, 0.0);
    assert_eq!(tail.ring, 0.5);
    assert_eq!(
        oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap(),
        project
    );
}
