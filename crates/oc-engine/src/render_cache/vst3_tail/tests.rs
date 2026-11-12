use super::*;

fn project() -> Project {
    oc_dsl::parse(
        r#"@project
@preset parent
  source: vst3
  plugin: "test.fake.two_bus"
  params: {Gain: 0.1, gain_alias: 0.2}
@preset child
  extends: parent
  params: {Gain: 0.3}
@track lead | preset: child, instrument: synth/lead
  engine_params: {gain_alias: 0.7}
"#,
    )
    .unwrap()
}

#[test]
fn tail_request_retains_original_layers_and_effective_lane_overrides() {
    let original = project();
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let lanes = crate::source_lanes::plan(&resolved).unwrap();
    let track = &lanes.tracks().tracks[0];
    let preset = &lanes.tracks().presets[1];
    let tail = instrument_tail(&original.presets, preset, track, 48_000, None, |plan| {
        assert_eq!(plan.sample_rate, 48_000);
        assert_eq!(plan.max_frames, 512);
        assert!(plan.component_state.is_none());
        assert_eq!(
            plan.parameters
                .iter()
                .map(|row| (row.name.as_str(), row.native_value))
                .collect::<Vec<_>>(),
            [
                ("Gain", Some(0.1)),
                ("gain_alias", Some(0.2)),
                ("Gain", Some(0.3)),
                ("gain_alias", Some(0.7)),
            ]
        );
        assert!(plan.parameters.iter().all(|row| !row.external_claim));
        Ok(12_345)
    })
    .unwrap();
    assert_eq!(tail, 12_345);
}

#[test]
fn same_preset_lanes_each_contribute_their_own_declared_tail() {
    let mut original = project();
    original.tracks[0].engine_params = Some(vec![("gain_alias".into(), ParamValue::Number(0.1))]);
    let mut later = original.tracks[0].clone();
    later.name = "later".into();
    later.engine_params = Some(vec![("gain_alias".into(), ParamValue::Number(0.9))]);
    original.tracks.push(later);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let tail = project_instrument_tail_samples(
        &resolved,
        &original.presets,
        &mut HashMap::new(),
        48_000,
        None,
        InstrumentTailProbes {
            vst3: |plan| {
                Ok((plan.parameters.last().unwrap().native_value.unwrap() * 10_000.0) as u32)
            },
            vst2: super::super::vst2_prepare::execute_probe,
        },
    )
    .unwrap();
    assert_eq!(tail, 9_000);
}

#[test]
fn a_fixture_preset_does_not_end_the_project_tail_reduction() {
    let mut original = project();
    let mut other = original.presets[1].clone();
    other.name = "other".into();
    original.presets.push(other);
    let mut later = original.tracks[0].clone();
    later.name = "other".into();
    later.preset = Some("other".into());
    later.engine_params = Some(vec![("gain_alias".into(), ParamValue::Number(0.9))]);
    original.tracks.push(later);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let tail = project_instrument_tail_samples(
        &resolved,
        &original.presets,
        &mut HashMap::new(),
        48_000,
        None,
        InstrumentTailProbes {
            vst3: |plan| {
                Ok((plan.parameters.last().unwrap().native_value.unwrap() * 10_000.0) as u32)
            },
            vst2: super::super::vst2_prepare::execute_probe,
        },
    )
    .unwrap();
    assert_eq!(tail, 9_000);
}

#[test]
fn tail_preparation_failure_keeps_plugin_preset_and_track_context() {
    let original = project();
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let error = instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
        |_| {
            Err(Vst3OwnedError::Control(
                "native parameter write was refused".into(),
            ))
        },
    )
    .unwrap_err();
    assert!(error.contains("child"));
    assert!(error.contains("lead"));
    assert!(error.contains("native parameter write was refused"));
}

#[test]
fn inherited_state_aliases_materialize_only_the_winning_resource() {
    let mut original = project();
    original.presets[0].params.push((
        "state_file".into(),
        ParamValue::String("not-read-overridden-state.bin".into()),
    ));
    original.presets[1]
        .params
        .push(("vst3_state_hex".into(), ParamValue::String("0102".into())));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
        |plan| {
            assert_eq!(plan.component_state.as_deref(), Some([1, 2].as_slice()));
            assert!(
                plan.parameters
                    .iter()
                    .all(|row| !row.name.contains("state"))
            );
            Ok(0)
        },
    )
    .unwrap();
}

#[test]
fn failed_external_resource_claim_retains_a_typed_native_refusal_diagnostic() {
    let mut original = project();
    original.presets[1].params.push((
        "profile_manifest".into(),
        ParamValue::String("missing-profile.json".into()),
    ));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let error = instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
        |plan| {
            let row = plan
                .parameters
                .iter()
                .find(|row| row.name == "profile_manifest")
                .unwrap();
            assert!(!row.external_claim);
            assert_eq!(row.native_value, None);
            Err(Vst3OwnedError::Authored(Box::new(
                oc_host::preset_parameters::ParameterApplyError {
                    format: PluginFormat::Vst3,
                    parameter: row.name.clone(),
                    kind: oc_host::preset_parameters::ParameterErrorKind::Unknown,
                },
            )))
        },
    )
    .unwrap_err();
    assert!(error.contains("external profile input"));
    assert!(error.contains("bounded regular UTF-8 data"));
    assert!(!error.contains("missing-profile.json"));
}

#[test]
fn output_routing_and_tail_claims_resolve_profile_resources_from_the_same_root() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("profile.json"),
        r#"{"plugin_output_pair":2}"#,
    )
    .unwrap();
    let mut original = project();
    original.presets[1].params.push((
        "profile_manifest".into(),
        ParamValue::String("profile.json".into()),
    ));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let ports = [AudioPort {
        channels: 4,
        ..AudioPort::STEREO
    }];
    assert_eq!(
        resolve_group_pairs(
            &resolved,
            &[0],
            "VST3",
            "child",
            48_000,
            &ports,
            Some(root.path()),
        )
        .unwrap(),
        vec![1]
    );
    instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        Some(root.path()),
        |plan| {
            let row = plan
                .parameters
                .iter()
                .find(|row| row.name == "profile_manifest")
                .unwrap();
            assert!(row.external_claim);
            assert_eq!(row.native_value, None);
            Ok(0)
        },
    )
    .unwrap();
}

#[test]
fn shared_group_compares_native_initialization_but_retains_all_external_claims() {
    fn request() -> Vst3InstrumentRequest {
        let p = project();
        let resolved = oc_dsl::preset::resolve(&p).unwrap();
        instrument_request(
            &p.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
        )
        .unwrap()
    }
    let mut first = request();
    let mut second = request();
    second.plan.parameters.push(Vst3AuthoredInput {
        name: "cc.1.param".into(),
        native_value: None,
        domain: ParameterWriteDomain::LegacyVst3Auto,
        external_claim: true,
    });
    first.merge_shared(second).unwrap();
    assert!(
        first
            .plan
            .parameters
            .iter()
            .any(|r| r.name == "cc.1.param" && r.external_claim)
    );
    let mut changed = request();
    changed.plan.parameters[0].native_value = Some(0.9);
    assert!(first.merge_shared(changed).is_err());
    let mut changed = request();
    changed.plan.component_state = Some(vec![1, 2, 3]);
    assert!(first.merge_shared(changed).is_err());
}
