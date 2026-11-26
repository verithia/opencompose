use oc_dsl::ast::{EffectType, ParamValue};
use oc_engine::dsp::{create_effect, try_create_effect};

#[test]
fn transfer_shaper_curve_validation_and_production_chain_are_shared() {
    assert_eq!(
        oc_engine::dsp::device_for_effect(&EffectType::TransferShaper)
            .unwrap()
            .id,
        "transfer-shaper"
    );
    let text = r#"{ "version":1,"domain":"odd","samples":[0,0.75,1] }"#;
    let source = format!(
        "@project\n@track shape | instrument: audio/clip\n  effects:\n    - transfer_shaper: curve={}, dry_gain=0.25, wet_gain=0.5\n",
        serde_json::to_string(text).unwrap()
    );
    let project = oc_dsl::parse(&source).unwrap();
    oc_dsl::validate(&project).unwrap();
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    assert_eq!(reopened, project);
    assert_eq!(
        project.tracks[0].effects[0].params[0].1,
        ParamValue::String(text.into())
    );
    let mut chain =
        oc_engine::graph::EffectChain::try_from_effects(&project.tracks[0].effects, 44100).unwrap();
    let mut pcm = [0.5, -0.5, 0.0, 0.25];
    chain.process(&mut pcm, 44100);
    assert_eq!(pcm, [0.5, -0.5, 0.0, 0.25]);
    for bad in ["{}", r#"{"version":1,"domain":"odd","samples":[1,1]}"#] {
        let params = [("curve".into(), ParamValue::String(bad.into()))];
        assert!(try_create_effect(&EffectType::TransferShaper, &params, 44100).is_err());
        let legacy = create_effect(&EffectType::TransferShaper, &params, 44100);
        assert!(legacy.processing_failed());
        assert!(matches!(
            legacy.take_processing_failure(),
            oc_audio::ProcessingFailureReport::Ready(_)
        ));
        assert_eq!(
            legacy.take_processing_failure(),
            oc_audio::ProcessingFailureReport::Reported
        );
    }
    assert!(
        try_create_effect(
            &EffectType::TransferShaper,
            &[("wet_gain".into(), ParamValue::Number(17.0))],
            44100
        )
        .is_err()
    );
}

#[test]
fn transfer_shaper_reads_project_assets_and_rejects_unusable_files() {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
@project
@bus shape
  effects:
    - transfer_shaper: curve_file="curve.json"
"#;
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    assert_eq!(oc_dsl::media::declarations(&project)[0].path, "curve.json");
    let create = |project: &oc_dsl::ast::Project| {
        oc_engine::graph::AudioGraph::try_from_project_with_resources(
            project,
            44_100,
            &[],
            Some(root.path()),
            None,
        )
    };
    let validate =
        || oc_engine::graph::validate_effect_parameters(&project, 44_100, Some(root.path()));
    assert!(create(&project).is_err());
    assert_eq!(validate().len(), 1);
    let path = root.path().join("curve.json");
    std::fs::write(&path, r#"{"version":1,"domain":"odd","samples":[0,1]}"#).unwrap();
    assert!(create(&project).is_ok());
    assert!(validate().is_empty());
    let ambiguous = source.replace(
        "curve_file=\"curve.json\"",
        r#"curve_file="curve.json", curve="{\"version\":1,\"domain\":\"odd\",\"samples\":[0,1]}""#,
    );
    let ambiguous = oc_dsl::parse(&ambiguous).unwrap();
    assert!(oc_dsl::validate(&ambiguous).is_err());
    assert!(create(&ambiguous).is_err());
    std::fs::write(&path, "{}").unwrap();
    assert!(create(&project).is_err());
}
