use super::*;

fn original() -> Project {
    oc_dsl::parse(
        r#"@project
@preset parent
  source: vst2
  plugin: "test.fixture.vst2.tone"
  params: {Gain: 0.1, gain_alias: 0.2}
@preset child
  extends: parent
  params: {Gain: 0.3}
@track lead | preset: child, instrument: synth/lead
  engine_params: {gain_alias: 0.7}
  @phrase line [1:1 - 2:1]
    C4 q
"#,
    )
    .unwrap()
}

#[test]
fn vst2_tail_original_layers_track_alias_order_and_domains_are_preserved() {
    let original = original();
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert_eq!(plan.sample_rate, 48_000);
    assert_eq!(plan.max_frames, 512);
    assert!(plan.state.is_none());
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
    assert!(
        plan.parameters
            .iter()
            .all(|row| row.domain == ParameterWriteDomain::Normalized)
    );
}

#[test]
fn vst2_tail_winning_state_alias_is_materialized_without_reading_overridden_file() {
    let mut original = original();
    original.presets[0].params.push((
        "state_file".into(),
        ParamValue::String("overridden-missing.bin".into()),
    ));
    original.presets[1]
        .params
        .push(("vst2_state_hex".into(), ParamValue::String("0a ff".into())));
    original.tracks[0]
        .engine_params
        .as_mut()
        .unwrap()
        .push(("midi_program".into(), ParamValue::Number(3.0)));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert_eq!(plan.state.as_deref(), Some([10, 255].as_slice()));
    assert!(
        plan.parameters
            .iter()
            .all(|row| !row.name.contains("state") && row.name != "midi_program")
    );
}

#[test]
fn vst2_tail_non_numeric_and_profile_looking_authored_values_are_not_silently_dropped() {
    let mut original = original();
    original.presets[1].params.extend([
        (
            "profile_manifest".into(),
            ParamValue::String("missing-profile.json".into()),
        ),
        (
            "sample_resource".into(),
            ParamValue::String("not-a-native-number".into()),
        ),
        ("Percentage".into(), ParamValue::Percentage(25.0)),
        ("Switch".into(), ParamValue::Bool(true)),
    ]);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    for name in ["profile_manifest", "sample_resource"] {
        assert!(
            plan.parameters
                .iter()
                .any(|row| row.name == name && row.native_value.is_none())
        );
    }
    assert!(
        plan.parameters
            .iter()
            .any(|row| row.name == "Percentage" && row.native_value == Some(0.25))
    );
    assert!(
        plan.parameters
            .iter()
            .any(|row| row.name == "Switch" && row.native_value == Some(1.0))
    );
}

#[test]
fn vst2_materializer_retains_resource_root_envelope_identity_and_input_bounds() {
    let root = tempfile::tempdir().unwrap();
    let context =
        PluginStateContext::new(PluginFormat::Vst2, Some("test.fixture.vst2.tone"), "child");
    let encoded = oc_host::encode_plugin_state(
        PluginFormat::Vst2,
        "test.fixture.vst2.tone",
        b"opaque chunk",
    );
    std::fs::write(root.path().join("checked.bin"), &encoded).unwrap();
    let params = vec![(
        "vst2_state_file".into(),
        ParamValue::String("checked.bin".into()),
    )];
    assert_eq!(
        materialize_vst2_preset_state_at(&params, &context, Some(root.path()))
            .unwrap()
            .as_deref(),
        Some(b"opaque chunk".as_slice())
    );
    let wrong = PluginStateContext::new(PluginFormat::Vst2, Some("different-plugin"), "child");
    assert!(materialize_vst2_preset_state_at(&params, &wrong, Some(root.path())).is_err());
    std::fs::write(
        root.path().join("checked.bin"),
        oc_host::encode_plugin_state(
            PluginFormat::Vst3,
            "test.fixture.vst2.tone",
            b"wrong format",
        ),
    )
    .unwrap();
    assert!(materialize_vst2_preset_state_at(&params, &context, Some(root.path())).is_err());
    assert!(
        materialize_vst2_preset_state_at(
            &[("vst2_state_hex".into(), ParamValue::String("0g".into()))],
            &context,
            None
        )
        .is_err()
    );
    std::fs::remove_file(root.path().join("checked.bin")).unwrap();
    std::fs::create_dir(root.path().join("checked.bin")).unwrap();
    assert!(materialize_vst2_preset_state_at(&params, &context, Some(root.path())).is_err());
}

#[test]
fn vst2_tail_authored_budgets_refuse_before_probe_dispatch() {
    let mut original = original();
    original.presets[1].params.push((
        "x".repeat(MAX_VST2_AUTHORED_NAME_BYTES + 1),
        ParamValue::Number(0.5),
    ));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let error = instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
        |_| panic!("invalid request must not dispatch"),
    )
    .unwrap_err();
    assert!(error.contains("name exceeds"), "{error}");
    original.presets[1].params = (0..MAX_VST2_AUTHORED_PARAMETERS)
        .map(|n| (format!("param{n}"), ParamValue::Number(0.5)))
        .collect();
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let error = instrument_tail(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
        |_| panic!("oversized request must not dispatch"),
    )
    .unwrap_err();
    assert!(error.contains("count exceeds"), "{error}");
}

fn project_tail(
    project: &Project,
    definitions: &[PresetDef],
    probe: Vst2TailProbe,
) -> Result<u64, String> {
    project_instrument_tail_samples(
        project,
        definitions,
        &mut HashMap::new(),
        48_000,
        None,
        InstrumentTailProbes {
            vst3: super::super::vst3_tail::execute_probe,
            vst2: probe,
        },
    )
}
#[test]
fn vst2_tail_same_preset_lanes_probe_their_own_overrides_and_cap_declared_tail() {
    let mut original = original();
    let mut later = original.tracks[0].clone();
    later.name = "later".into();
    later.engine_params = Some(vec![("gain_alias".into(), ParamValue::Number(0.9))]);
    original.tracks.push(later);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    assert_eq!(
        project_tail(&resolved, &original.presets, |plan| Ok(Some(
            (plan.parameters.last().unwrap().native_value.unwrap() * 10_000.0) as u32
        )))
        .unwrap(),
        9_000
    );
    assert_eq!(
        project_tail(&resolved, &original.presets, |_| Ok(Some(u32::MAX))).unwrap(),
        48_000 * 30
    );
    assert_eq!(
        project_tail(&resolved, &original.presets, |_| Ok(None)).unwrap(),
        0
    );
}

#[test]
fn vst2_tail_preparation_error_keeps_context_and_invalidates_existing_cache() {
    let mut cache = RenderCache::new();
    let builtin = oc_dsl::parse(
        r#"@project
@preset warm
  source: builtin
  plugin: "builtin.test_tone"
@track warm | preset: warm, instrument: synth/lead
  @phrase line [1:1 - 2:1]
    C4 q
"#,
    )
    .unwrap();
    cache
        .render_full(
            &builtin,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(cache.master_buffer.is_some());
    assert!(cache.render_horizon_samples > 0);
    assert!(cache.duration() > 0.0);
    cache.set_vst2_tail_probe(|_| {
        Err(Vst2OwnedError::Control(
            "native tail probe or CLOSE refused".into(),
        ))
    });
    let error = cache
        .render_incremental(
            &original(),
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("child")
            && error.contains("lead")
            && error.contains("native tail probe or CLOSE refused"),
        "{error}"
    );
    assert!(cache.master_buffer.is_none());
    assert!(cache.track_buffers.iter().all(Option::is_none));
    assert_eq!(cache.render_horizon_samples, 0);
    assert_eq!(cache.duration(), 0.0);
    assert_eq!(cache.duration_seconds(), 0.0);
}

#[test]
fn vst2_tail_native_state_looking_suffixes_keep_their_authored_values() {
    let mut original = original();
    original.presets[1].params.extend([
        (
            "state_file.dll".into(),
            ParamValue::String("must-not-be-opened.state".into()),
        ),
        ("StateFile.dll".into(), ParamValue::Number(0.75)),
    ]);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert!(plan.state.is_none());
    assert!(
        plan.parameters
            .iter()
            .any(|row| row.name == "state_file.dll" && row.native_value.is_none())
    );
    assert!(
        plan.parameters
            .iter()
            .any(|row| row.name == "StateFile.dll" && row.native_value == Some(0.75))
    );

    original.presets[1]
        .params
        .push(("vst2_state_hex".into(), ParamValue::String("0102".into())));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert_eq!(plan.state.as_deref(), Some([1, 2].as_slice()));
    original.presets[1].params.last_mut().unwrap().1 = ParamValue::Number(0.5);
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    assert!(
        instrument_tail(
            &original.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
            |_| panic!("malformed recognized state must not dispatch")
        )
        .is_err()
    );
}

#[test]
fn vst2_tail_state_input_borrows_the_typed_winner_before_decoding() {
    let mut original = original();
    original.presets[0].params.push((
        "vst2_state_hex".into(),
        ParamValue::String("invalid overridden state".into()),
    ));
    original.presets[1].params.push((
        "state_file".into(),
        ParamValue::String("relative.state".into()),
    ));
    let assembled = assemble_hosted_parameters(HostedPresetInput {
        definitions: &original.presets,
        preset_name: "child",
        track: Some(&original.tracks[0]),
        derived: &[],
        format: PluginFormat::Vst2,
    })
    .unwrap();
    let source = match &original.presets[1].params.last().unwrap().1 {
        ParamValue::String(text) => text,
        _ => unreachable!(),
    };
    match borrowed_state_input(&assembled.host_parameters)
        .unwrap()
        .unwrap()
    {
        PluginStateInput::File(path) => {
            assert_eq!(path, "relative.state");
            assert_eq!(path.as_ptr(), source.as_ptr());
        }
        _ => panic!("typed last state consumer must select the file"),
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("relative.state"), [1, 2, 3]).unwrap();
    let context =
        PluginStateContext::new(PluginFormat::Vst2, Some("test.fixture.vst2.tone"), "child");
    assert_eq!(
        materialize_plugin_state_input(
            borrowed_state_input(&assembled.host_parameters).unwrap(),
            &context,
            Some(root.path())
        )
        .unwrap()
        .as_deref(),
        Some([1, 2, 3].as_slice())
    );
}

#[test]
fn vst2_tail_only_validated_main_pair_rows_receive_typed_routing_claims() {
    for raw in [0.0, 1.0] {
        let mut original = original();
        original.presets[1].params.extend([
            ("plugin_output_pair".into(), ParamValue::Number(raw)),
            (
                "profile_manifest".into(),
                ParamValue::String("unconsumed-missing-profile.json".into()),
            ),
        ]);
        let resolved = oc_dsl::preset::resolve(&original).unwrap();
        let plan = instrument_request(
            &original.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
        )
        .unwrap();
        assert!(
            plan.parameters
                .iter()
                .any(|row| row.name == "plugin_output_pair"
                    && row.claim == Vst2AuthoredClaim::InstrumentOutputRouting)
        );
        assert!(
            plan.parameters
                .iter()
                .filter(|row| row.name != "plugin_output_pair")
                .all(|row| row.claim == Vst2AuthoredClaim::NativeOnly)
        );
        assert!(
            plan.parameters
                .iter()
                .any(|row| row.name == "profile_manifest" && row.native_value.is_none())
        );
    }
}
#[test]
fn vst2_tail_unaccepted_output_candidates_remain_native_for_inventory_arbitration() {
    for raw in [2.0, 0.75, -1.0, f64::NAN, f64::INFINITY] {
        let mut original = original();
        original.presets[1]
            .params
            .push(("output_pair".into(), ParamValue::Number(raw)));
        let resolved = oc_dsl::preset::resolve(&original).unwrap();
        let plan = instrument_request(
            &original.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
        )
        .unwrap();
        let row = plan
            .parameters
            .iter()
            .find(|row| row.name == "output_pair")
            .unwrap();
        assert_eq!(row.claim, Vst2AuthoredClaim::NativeOnly);
        assert!(
            row.native_value
                .is_some_and(|value| value == raw || (value.is_nan() && raw.is_nan()))
        );
    }
}
#[test]
fn vst2_tail_routing_override_chain_is_claimed_only_for_final_main_pair() {
    for final_value in [0.0, 1.0] {
        let mut original = original();
        original.presets[0]
            .params
            .push(("output_pair".into(), ParamValue::Number(2.0)));
        original.presets[1]
            .params
            .push(("mic_output_pair".into(), ParamValue::Number(final_value)));
        let resolved = oc_dsl::preset::resolve(&original).unwrap();
        let plan = instrument_request(
            &original.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
        )
        .unwrap();
        let routing: Vec<_> = plan
            .parameters
            .iter()
            .filter(|row| matches!(row.name.as_str(), "output_pair" | "mic_output_pair"))
            .collect();
        assert_eq!(routing.len(), 2);
        assert!(
            routing
                .iter()
                .all(|row| row.claim == Vst2AuthoredClaim::InstrumentOutputRouting)
        );
        assert_eq!(routing[0].native_value, Some(2.0));
        assert_eq!(routing[1].native_value, Some(final_value));
    }
    let mut original = original();
    original.presets[0]
        .params
        .push(("output_pair".into(), ParamValue::Number(0.0)));
    original.presets[1]
        .params
        .push(("plugin_output_pair".into(), ParamValue::Number(2.0)));
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert!(
        plan.parameters
            .iter()
            .all(|row| row.claim == Vst2AuthoredClaim::NativeOnly)
    );
}
#[test]
fn vst2_tail_typed_track_nonmain_output_is_refused_without_synthetic_native_rows() {
    let original = original();
    let mut resolved = oc_dsl::preset::resolve(&original).unwrap();
    resolved.tracks[0].output_pair = Some(1);
    assert!(
        instrument_tail(
            &original.presets,
            &resolved.presets[1],
            &resolved.tracks[0],
            48_000,
            None,
            |_| panic!("nonmain track routing must not dispatch")
        )
        .is_err()
    );
    resolved.tracks[0].output_pair = Some(0);
    let plan = instrument_request(
        &original.presets,
        &resolved.presets[1],
        &resolved.tracks[0],
        48_000,
        None,
    )
    .unwrap();
    assert!(
        plan.parameters
            .iter()
            .all(|row| row.claim == Vst2AuthoredClaim::NativeOnly)
    );
}
