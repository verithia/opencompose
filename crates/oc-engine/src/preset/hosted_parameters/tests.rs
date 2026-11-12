use super::*;
use oc_host::preset_parameters::HostStateEncoding;

fn input<'a>(project: &'a oc_dsl::ast::Project, name: &'a str) -> HostedPresetInput<'a> {
    HostedPresetInput {
        definitions: &project.presets,
        preset_name: name,
        track: project.tracks.first(),
        derived: &[],
        format: PluginFormat::Clap,
    }
}

#[test]
fn hosted_input_preserves_raw_layer_order_instead_of_flattened_alias_order() {
    let project = oc_dsl::parse(
        r#"@project
@preset parent
  source: clap
  plugin: "fixture"
  program: 25
  params: {Gain: 0.1, gain_alias: 0.2, state_file: "parent.bin"}
@preset child
  extends: parent
  params: {Gain: "not numeric yet", plugin_state_hex: "child hex"}
@track lead | instrument: synth/lead_saw, preset: child
  engine_params: {gain_alias: 0.7, clap_state_file: "track.bin"}
"#,
    )
    .unwrap();
    let assembled = assemble_hosted_parameters(input(&project, "child")).unwrap();
    assert_eq!(assembled.resolution.preset.program, Some(25));
    assert_eq!(assembled.resolution.preset.params[0].0, "Gain");
    assert_eq!(
        assembled
            .authored
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        [
            "Gain",
            "gain_alias",
            "state_file",
            "Gain",
            "plugin_state_hex",
            "gain_alias",
            "clap_state_file"
        ]
    );
    assert_eq!(
        assembled.authored[3].origin,
        AssignmentOrigin::Preset("child")
    );
    assert_eq!(
        assembled.authored[5].origin,
        AssignmentOrigin::TrackOverride("lead")
    );
    assert!(std::ptr::eq(
        assembled.authored[3].value,
        &project.presets[1].params[0].1
    ));
    let state = assembled
        .host_parameter(HostParameterConsumer::State)
        .unwrap();
    assert_eq!(state.assignment.name, "clap_state_file");
    assert_eq!(
        state.assignment.origin,
        AssignmentOrigin::TrackOverride("lead")
    );
    assert_eq!(
        state.parameter,
        HostOwnedParameter::State(HostStateEncoding::File)
    );
    assert_eq!(
        state.assignment.value,
        &ParamValue::String("track.bin".into())
    );
}

#[test]
fn hosted_input_checks_only_effective_host_values_and_never_reads_their_paths() {
    let mut project = oc_dsl::parse(r#"@project
@preset tone
  source: clap
  plugin: "fixture"
  params: {state_file: 1, plugin_state_hex: "not decoded in assembly", plugin_version: 3, Plugin.Version: ""}
"#).unwrap();
    let assembled = assemble_hosted_parameters(input(&project, "tone")).unwrap();
    assert_eq!(assembled.host_parameters.len(), 2);
    assert_eq!(
        assembled
            .host_parameter(HostParameterConsumer::PluginVersion)
            .unwrap()
            .assignment
            .value,
        &ParamValue::String(String::new())
    );
    assert_eq!(
        assembled
            .host_parameter(HostParameterConsumer::State)
            .unwrap()
            .parameter,
        HostOwnedParameter::State(HostStateEncoding::Hex)
    );
    project.presets[0].params.push((
        "clap_state_file".into(),
        ParamValue::String("unread-nonexistent-state.bin".into()),
    ));
    assert!(assemble_hosted_parameters(input(&project, "tone")).is_ok());
    project.presets[0]
        .params
        .push(("state_hex".into(), ParamValue::Array(vec![])));
    let error = assemble_hosted_parameters(input(&project, "tone")).unwrap_err();
    assert!(matches!(
        error,
        HostedInputError::HostParameter {
            source: ParameterErrorKind::InvalidHostValue,
            ..
        }
    ));
    project.presets[0].params.push((
        "plugin_state_file".into(),
        ParamValue::String("last.bin".into()),
    ));
    assert_eq!(
        assemble_hosted_parameters(input(&project, "tone"))
            .unwrap()
            .host_parameter(HostParameterConsumer::State)
            .unwrap()
            .assignment
            .name,
        "plugin_state_file"
    );
}

#[test]
fn hosted_input_preserves_native_raw_values_until_canonical_binding_winners_exist() {
    let mut project =
        oc_dsl::parse("@project\n@preset tone\n  source: clap\n  plugin: \"fixture\"\n").unwrap();
    project.presets[0].params = vec![
        (
            "Gain".into(),
            ParamValue::String("overwritten after binding".into()),
        ),
        ("gain_alias".into(), ParamValue::Number(0.7)),
        ("other".into(), ParamValue::Number(f64::NAN)),
        ("other_alias".into(), ParamValue::Bool(true)),
        ("array_control".into(), ParamValue::Array(vec![1.0])),
        (
            "sample_file".into(),
            ParamValue::String("authored native candidate".into()),
        ),
    ];
    let assembled = assemble_hosted_parameters(input(&project, "tone")).unwrap();
    assert_eq!(assembled.authored.len(), 6);
    assert!(
        assembled
            .authored
            .iter()
            .all(|entry| entry.annotation == AssignmentAnnotation::UnclassifiedAuthored)
    );
    for (entry, (_, original)) in assembled.authored.iter().zip(&project.presets[0].params) {
        assert!(std::ptr::eq(entry.value, original));
    }
    assert!(matches!(assembled.authored[2].value, ParamValue::Number(value) if value.is_nan()));
    assert!(matches!(assembled.authored[4].value, ParamValue::Array(_)));
}

#[test]
fn hosted_input_keeps_track_program_routing_and_derived_origins_distinct() {
    let project = oc_dsl::parse(r#"@project
@preset tone
  source: clap
  plugin: "fixture"
  program: 25
  params: {midi_program: 7, plugin_output_pair: 1, mpe.bend_range: 0.25, profile_manifest: "profile.json"}
@track lead | instrument: synth/lead_saw, preset: tone
  engine_params: {midi_program: 33, midi_bend_range: 12, plugin_output_pair: 3}
"#).unwrap();
    let derived = vec![
        ("mpe.bend_range".into(), ParamValue::Number(48.0)),
        ("output.loudness_trim_db".into(), ParamValue::Number(-2.0)),
        (
            "state_file".into(),
            ParamValue::String("derived config is not authored state".into()),
        ),
    ];
    let mut request = input(&project, "tone");
    request.derived = &derived;
    let assembled = assemble_hosted_parameters(request).unwrap();
    assert_eq!(assembled.resolution.preset.program, Some(25));
    assert_eq!(
        assembled.authored[0].annotation,
        AssignmentAnnotation::UnclassifiedAuthored
    );
    assert_eq!(
        assembled.authored[1].annotation,
        AssignmentAnnotation::ProfileCandidate(crate::render_control::ProfileAssignmentCandidate {
            kind: crate::render_control::ProfileKeyKind::OutputPair,
            value_status: crate::render_control::ProfileValueStatus::ConvertedCandidate,
        })
    );
    assert_eq!(
        assembled.authored[2].annotation,
        AssignmentAnnotation::UnclassifiedAuthored
    );
    assert_eq!(
        assembled.authored[3].annotation,
        AssignmentAnnotation::ProfileCandidate(crate::render_control::ProfileAssignmentCandidate {
            kind: crate::render_control::ProfileKeyKind::Manifest,
            value_status: crate::render_control::ProfileValueStatus::DeferredResource,
        })
    );
    assert_eq!(
        assembled.authored[4].annotation,
        AssignmentAnnotation::Engine(KnownEngineConsumer::MidiProgram)
    );
    assert_eq!(assembled.authored[4].value, &ParamValue::Number(33.0));
    assert_eq!(
        assembled.authored[5].annotation,
        AssignmentAnnotation::Engine(KnownEngineConsumer::MidiBendRange)
    );
    assert_eq!(assembled.authored[6].value, &ParamValue::Number(3.0));
    assert_eq!(assembled.derived.len(), 3);
    assert!(
        assembled
            .derived
            .iter()
            .all(|entry| entry.origin == AssignmentOrigin::DerivedEngine
                && entry.annotation == AssignmentAnnotation::DerivedEngine)
    );
    assert!(assembled.host_parameters.is_empty());
    assert!(std::ptr::eq(assembled.track.unwrap(), &project.tracks[0]));
}

#[test]
fn hosted_input_uses_registry_backend_policy_without_wildcard_exemptions() {
    for (source, format, state) in [
        ("clap", PluginFormat::Clap, "clap_state_file"),
        ("vst3", PluginFormat::Vst3, "vst3_state_file"),
        ("vst2", PluginFormat::Vst2, "vst2_state_file"),
        ("au", PluginFormat::Au, "au_state_file"),
        ("lv2", PluginFormat::Lv2, "lv2_state_file"),
    ] {
        let project = oc_dsl::parse(&format!("@project\n@preset tone\n  source: {source}\n  plugin: \"fixture\"\n  params: {{{state}: \"never-read.bin\", custom_file: \"native candidate\"}}\n")).unwrap();
        let mut request = input(&project, "tone");
        request.format = format;
        let assembled = assemble_hosted_parameters(request).unwrap();
        assert_eq!(assembled.host_parameters.len(), 1);
        assert_eq!(
            assembled.authored[1].annotation,
            AssignmentAnnotation::UnclassifiedAuthored
        );
    }
    let project = oc_dsl::parse("@project\n@preset tone\n  source: clap\n  params: {vst3_state_file: \"foreign.bin\", state_file: \"last.bin\"}\n").unwrap();
    assert!(matches!(
        assemble_hosted_parameters(input(&project, "tone")),
        Err(HostedInputError::HostParameter {
            source: ParameterErrorKind::WrongBackend { .. },
            ..
        })
    ));
}

#[test]
fn hosted_input_profile_candidates_remain_unready_and_never_hide_arbitrary_native_prefixes() {
    use crate::render_control::{ProfileAssignmentCandidate, ProfileKeyKind, ProfileValueStatus};
    for format in [
        PluginFormat::Clap,
        PluginFormat::Vst3,
        PluginFormat::Vst2,
        PluginFormat::Au,
        PluginFormat::Lv2,
    ] {
        let mut project=oc_dsl::parse("@project\n@preset base\n  source: clap\n  plugin: \"fixture\"\n@preset tone\n  extends: base\n@track lead | preset: tone, instrument: synth/lead_saw\n").unwrap();
        project.presets[0].params = vec![
            ("output_pair".into(), ParamValue::Number(1.0)),
            ("control.brightness.cc".into(), ParamValue::Array(vec![0.5])),
            (
                "profile_manifest".into(),
                ParamValue::String("not-read.json".into()),
            ),
            ("articulation.native_gain".into(), ParamValue::Number(0.6)),
            ("mpe.bend_range".into(), ParamValue::Number(12.0)),
        ];
        project.presets[1].params = vec![
            (
                "OUTPUT.PAIR".into(),
                ParamValue::String("not a pair".into()),
            ),
            ("cc14param".into(), ParamValue::String("Cutoff".into())),
        ];
        project.tracks[0].engine_params =
            Some(vec![("plugin.output.pair".into(), ParamValue::Number(3.0))]);
        let derived = vec![("profile_manifest".into(), ParamValue::Array(vec![]))];
        let mut request = input(&project, "tone");
        request.format = format;
        request.derived = &derived;
        let assembled = assemble_hosted_parameters(request).unwrap();
        let candidate = |kind, value_status| {
            AssignmentAnnotation::ProfileCandidate(ProfileAssignmentCandidate {
                kind,
                value_status,
            })
        };
        assert_eq!(assembled.authored.len(), 8);
        assert_eq!(
            assembled.authored[0].annotation,
            candidate(
                ProfileKeyKind::OutputPair,
                ProfileValueStatus::ConvertedCandidate
            )
        );
        assert_eq!(
            assembled.authored[1].annotation,
            candidate(ProfileKeyKind::ControlCc, ProfileValueStatus::InvalidValue)
        );
        assert_eq!(
            assembled.authored[2].annotation,
            candidate(
                ProfileKeyKind::Manifest,
                ProfileValueStatus::DeferredResource
            )
        );
        for index in [3, 4] {
            assert_eq!(
                assembled.authored[index].annotation,
                AssignmentAnnotation::UnclassifiedAuthored
            );
        }
        assert_eq!(
            assembled.authored[5].annotation,
            candidate(ProfileKeyKind::OutputPair, ProfileValueStatus::InvalidValue)
        );
        assert_eq!(
            assembled.authored[6].annotation,
            candidate(
                ProfileKeyKind::CcParam,
                ProfileValueStatus::ConvertedCandidate
            )
        );
        assert_eq!(
            assembled.authored[7].annotation,
            candidate(
                ProfileKeyKind::OutputPair,
                ProfileValueStatus::ConvertedCandidate
            )
        );
        assert_eq!(
            assembled.derived[0].annotation,
            AssignmentAnnotation::DerivedEngine
        );
        assert!(assembled.host_parameters.is_empty());
        // Recognition neither drops a native-name collision candidate nor
        // resolves which consumer owns it. Values/origins stay intact.
        assert!(std::ptr::eq(
            assembled.authored[1].value,
            &project.presets[0].params[1].1
        ));
        assert_eq!(
            assembled.authored[5].origin,
            AssignmentOrigin::Preset("tone")
        );
        assert_eq!(
            assembled.authored[7].origin,
            AssignmentOrigin::TrackOverride("lead")
        );
    }
}
