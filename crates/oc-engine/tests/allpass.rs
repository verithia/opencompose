//! Shared schema, DSL and production chain contracts for phase sections.
use oc_dsl::ast::{EffectType, ParamValue};
use oc_engine::dsp::try_create_effect;

#[test]
fn allpass_schema_roundtrip_factory_and_chain_tail_use_the_same_control() {
    let source = "@project\n@track phase | instrument: audio/clip\n  effects:\n    - allpass: coefficient=0.999\n";
    let project = oc_dsl::parse(source).unwrap();
    assert_eq!(
        project.tracks[0].effects[0].effect_type,
        EffectType::AllPass
    );
    assert_eq!(
        oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap(),
        project
    );
    let schema = oc_engine::dsp::schema::find_device("allpass").unwrap();
    let control = schema.param("coefficient").unwrap();
    assert_eq!((control.min, control.max), (-0.999, 0.999));
    for rate in [8_000, 48_000] {
        let effect = &project.tracks[0].effects[0];
        let mut device = try_create_effect(&effect.effect_type, &effect.params, rate).unwrap();
        let tail = device.tail_samples();
        let chain_tail = oc_engine::graph::track_chain_tail_seconds(&project, 0, rate).unwrap();
        assert_eq!(chain_tail.latency, 0.0);
        assert!((chain_tail.ring - f64::from(tail) / f64::from(rate)).abs() < 1e-12);
        let mut audio = vec![0.0; (tail as usize + 32) * 2];
        audio[0] = 1.0;
        device.process(&mut audio, rate);
        assert!(audio[2].abs() > 1e-3);
        assert!(
            audio[(tail as usize + 1) * 2..]
                .iter()
                .all(|x| x.abs() < 1e-6)
        );
        assert!(
            try_create_effect(
                &EffectType::AllPass,
                &[("coefficient".into(), ParamValue::Number(1.0))],
                rate
            )
            .is_err()
        );
    }
}

#[test]
fn allpass_real_audio_region_end_impulse_renders_its_declared_tail_at_8khz() {
    let root = oc_test_support::temp_dir("allpass-render", "tail");
    std::fs::create_dir_all(&root).unwrap();
    let mut writer = hound::WavWriter::create(
        root.join("impulse.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for i in 0..8_000 {
        writer
            .write_sample(if i == 7_999 { 1.0_f32 } else { 0.0 })
            .unwrap();
    }
    writer.finalize().unwrap();
    std::fs::write(root.join("regions.json"), r#"{"sample_rate":8000,"regions":[{"file":"impulse.wav","region":{"timeline_start":0,"source_start":0,"length":8000,"gain":1.0,"envelope":[]}}]}"#).unwrap();
    let project = oc_dsl::parse("@project\n@preset input\n  source: builtin\n  plugin: \"builtin.audio_regions\"\n  params: {audio_region_state_file: \"regions.json\"}\n@track input | preset: input\n  mix: pan_law=balance\n  effects:\n    - allpass: coefficient=0.999\n").unwrap();
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache.set_resource_root(Some(root));
    let audio = cache
        .render_full(
            &project,
            std::path::Path::new("unused.sf2"),
            8_000,
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
    let tail = try_create_effect(
        &project.tracks[0].effects[0].effect_type,
        &project.tracks[0].effects[0].params,
        8_000,
    )
    .unwrap()
    .tail_samples() as usize;
    assert!(
        audio.len() / 2 >= 8_000 + tail,
        "the declared IIR ring-out is cut"
    );
    assert!(audio[8_000 * 2].abs() > 1e-3, "the first tail frame is cut");
    assert!(audio[audio.len() - 32..].iter().all(|x| x.abs() < 1e-6));
}
