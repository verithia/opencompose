use super::*;

fn claim(name: &str, value: ParamValue) -> ProfileAssignmentClaim {
    checked_profile_assignment(name, &value, 48_000, None)
}

#[test]
fn checked_claims_keep_native_candidates_and_legacy_profile_clamps() {
    assert_eq!(
        claim("Gain", ParamValue::Number(0.7)),
        ProfileAssignmentClaim::Unrecognized
    );
    assert_eq!(
        claim("cc", ParamValue::Number(200.0)),
        ProfileAssignmentClaim::Accepted
    );
    let params = [("cc".into(), ParamValue::Number(200.0))];
    assert_eq!(
        InstrumentProfile::from_preset_params(&params, 48_000).control_cc(""),
        Some(127)
    );
    assert_eq!(
        claim("mpe_first_channel", ParamValue::Number(-2.0)),
        ProfileAssignmentClaim::Accepted
    );
    assert_eq!(
        claim(
            "control_breath_cc",
            ParamValue::String("not numeric".into())
        ),
        ProfileAssignmentClaim::Invalid(ProfileAssignmentError::InvalidValue)
    );
    assert_eq!(
        claim("plugin_output_pair", ParamValue::Number(f64::NAN)),
        ProfileAssignmentClaim::Invalid(ProfileAssignmentError::InvalidValue)
    );
}

#[test]
fn checked_resources_and_runtime_share_project_root_and_nested_manifest_parser() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/base.json"),
        r#"{"control_breath_cc":12}"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("profiles/main.toml"),
        "profile_manifest = \"base.json\"\ncontrol_expression_cc = 11\n",
    )
    .unwrap();
    let value = ParamValue::String("profiles/main.toml".into());
    assert_eq!(
        checked_profile_assignment("profile_manifest", &value, 48_000, Some(root.path())),
        ProfileAssignmentClaim::Accepted
    );
    let params = [("profile_manifest".into(), value)];
    let profile = InstrumentProfile::from_preset_params_at(&params, 48_000, Some(root.path()));
    assert_eq!(profile.control_cc("breath"), Some(12));
    assert_eq!(profile.control_cc("expression"), Some(11));
    assert_eq!(
        InstrumentProfile::from_manifest_file(root.path().join("profiles/main.toml"), 48_000)
            .unwrap(),
        profile
    );
}

#[test]
fn track_profile_and_interpreter_share_root_and_override_consumption() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("profile.json"),
        r#"{"control_breath_cc":12}"#,
    )
    .unwrap();
    let project = oc_dsl::parse(
        r#"@project
@preset voice
  source: vst3
  plugin: "fixture"
  params: {profile_manifest: "profile.json", control_expression_cc: 10}
@track lead | preset: voice, instrument: synth/lead
  engine_params: {control_expression_cc: 11}
  @phrase held [1:1 - 2:1]
    C4 q mf
"#,
    )
    .unwrap();
    let profile = InstrumentProfile::from_track(
        &project.presets,
        &project.tracks[0],
        48_000,
        Some(root.path()),
    );
    assert_eq!(profile.control_cc("breath"), Some(12));
    assert_eq!(profile.control_cc("expression"), Some(11));
    let mut absolute = project.clone();
    absolute.presets[0].params[0].1 = ParamValue::String(
        root.path()
            .join("profile.json")
            .to_string_lossy()
            .into_owned(),
    );
    let settings = crate::interpret::InterpretSettings {
        sample_rate: 48_000,
        ..Default::default()
    };
    let relative_events =
        crate::interpret::try_interpret_at(&project, &settings, Some(root.path())).unwrap();
    let absolute_events = crate::interpret::try_interpret(&absolute, &settings).unwrap();
    assert_eq!(relative_events, absolute_events);
}

#[test]
fn missing_malformed_directory_and_oversized_resources_never_claim_a_native_name() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("bad.json"), "{").unwrap();
    std::fs::write(
        root.path().join("huge.json"),
        vec![b' '; MAX_RESOURCE_BYTES + 1],
    )
    .unwrap();
    for name in ["missing.json", "bad.json", "huge.json", "."] {
        let result = checked_profile_assignment(
            "profile_manifest",
            &ParamValue::String(name.into()),
            48_000,
            Some(root.path()),
        );
        assert!(
            matches!(result, ProfileAssignmentClaim::Invalid(_)),
            "{name}"
        );
        if let ProfileAssignmentClaim::Invalid(error) = result {
            assert!(
                !error
                    .to_string()
                    .contains(&root.path().to_string_lossy().to_string())
            );
        }
    }
}

#[test]
fn manifest_cycles_depth_and_assignment_budgets_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("cycle.json"),
        r#"{"profile_manifest":"cycle.json"}"#,
    )
    .unwrap();
    assert_eq!(
        checked_profile_assignment(
            "profile_manifest",
            &ParamValue::String("cycle.json".into()),
            48_000,
            Some(root.path())
        ),
        ProfileAssignmentClaim::Invalid(ProfileAssignmentError::ResourceCycle)
    );
    for i in 0..=MAX_RESOURCE_DEPTH {
        let body = if i == MAX_RESOURCE_DEPTH {
            "{}".into()
        } else {
            format!(r#"{{"profile_manifest":"{}.json"}}"#, i + 1)
        };
        std::fs::write(root.path().join(format!("{i}.json")), body).unwrap();
    }
    assert_eq!(
        checked_profile_assignment(
            "profile_manifest",
            &ParamValue::String("0.json".into()),
            48_000,
            Some(root.path())
        ),
        ProfileAssignmentClaim::Invalid(ProfileAssignmentError::ResourceDepth)
    );
    let value = ParamValue::Number(1.0);
    assert_eq!(
        checked_profile_assignments(
            std::iter::repeat_n(("mpe", &value), MAX_ASSIGNMENTS + 1),
            48_000,
            None
        ),
        Err(ProfileAssignmentError::ResourceLimit)
    );
}

#[test]
fn tunings_use_identical_checked_parsers_and_refuse_nonfinite_or_overflow_counts() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("valid.scl"), "Fixture\n1\n125.0\n").unwrap();
    std::fs::write(root.path().join("valid.tun"), "note61=25.0\n").unwrap();
    std::fs::write(root.path().join("invalid.tun"), "note61=NaN\n").unwrap();
    std::fs::write(
        root.path().join("invalid.scl"),
        format!("Fixture\n{}\n100.0\n", usize::MAX),
    )
    .unwrap();
    for (name, file) in [("scala_file", "valid.scl"), ("tun_file", "valid.tun")] {
        let value = ParamValue::String(file.into());
        assert_eq!(
            checked_profile_assignment(name, &value, 48_000, Some(root.path())),
            ProfileAssignmentClaim::Accepted
        );
        let profile = InstrumentProfile::from_preset_params_at(
            &[(name.into(), value)],
            48_000,
            Some(root.path()),
        );
        assert_eq!(profile.note_tuning_cents(61), Some(25.0));
    }
    for (name, file) in [("scala_file", "invalid.scl"), ("tun_file", "invalid.tun")] {
        assert_eq!(
            checked_profile_assignment(
                name,
                &ParamValue::String(file.into()),
                48_000,
                Some(root.path())
            ),
            ProfileAssignmentClaim::Invalid(ProfileAssignmentError::TuningSyntax)
        );
    }
}

#[test]
fn resource_budget_is_shared_across_one_checked_authored_batch() {
    let root = tempfile::tempdir().unwrap();
    let body = format!("{{}}{}", " ".repeat(MAX_RESOURCE_BYTES - 2));
    std::fs::write(root.path().join("large.json"), body).unwrap();
    let value = ParamValue::String("large.json".into());
    let claims = checked_profile_assignments(
        std::iter::repeat_n(("profile_manifest", &value), 5),
        48_000,
        Some(root.path()),
    )
    .unwrap();
    assert_eq!(&claims[..4], &[ProfileAssignmentClaim::Accepted; 4]);
    assert_eq!(
        claims[4],
        ProfileAssignmentClaim::Invalid(ProfileAssignmentError::ResourceLimit)
    );
}
