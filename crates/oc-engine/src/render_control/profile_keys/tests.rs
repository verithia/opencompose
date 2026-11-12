use super::*;
fn samples() -> Vec<(&'static str, ProfileKeyKind, ParamValue)> {
    vec![
        (
            "profile_manifest",
            Manifest,
            ParamValue::String("not-opened.json".into()),
        ),
        (
            "scala_root_note",
            ScalaRoot,
            ParamValue::String("C4".into()),
        ),
        (
            "scala_file",
            ScalaFile,
            ParamValue::String("not-opened.scl".into()),
        ),
        (
            "tun_file",
            TunFile,
            ParamValue::String("not-opened.tun".into()),
        ),
        (
            "keyswitch_duration_ms",
            KeyswitchDuration,
            ParamValue::Number(20.0),
        ),
        ("plugin_output_pair", OutputPair, ParamValue::Number(3.0)),
        ("mpe.enabled", MpeEnabled, ParamValue::Bool(true)),
        (
            "mpe.first_channel",
            MpeFirstChannel,
            ParamValue::Number(2.0),
        ),
        ("mpe.last_channel", MpeLastChannel, ParamValue::Number(10.0)),
        (
            "articulation.legato.round_robin_keyswitches",
            ArtRoundRobin,
            ParamValue::String("C0 D0".into()),
        ),
        (
            "articulation.legato.keyswitch",
            ArtKeyswitch,
            ParamValue::String("E0".into()),
        ),
        (
            "articulation.legato.cc_value",
            ArtCcValue,
            ParamValue::Number(64.0),
        ),
        (
            "articulation.legato.cc_reset",
            ArtCcReset,
            ParamValue::Number(0.0),
        ),
        ("articulation.legato.cc", ArtCc, ParamValue::Number(21.0)),
        (
            "articulation.legato.param_value",
            ArtParamValue,
            ParamValue::Number(0.6),
        ),
        (
            "articulation.legato.param_reset",
            ArtParamReset,
            ParamValue::Number(0.4),
        ),
        (
            "articulation.legato.param",
            ArtParam,
            ParamValue::String("cutoff".into()),
        ),
        (
            "articulation.legato.duration_scale",
            ArtDurationScale,
            ParamValue::Number(1.1),
        ),
        (
            "articulation.legato.humanize_timing_ms",
            ArtHumanizeTiming,
            ParamValue::Number(10.0),
        ),
        (
            "articulation.legato.humanize_velocity",
            ArtHumanizeVelocity,
            ParamValue::Number(5.0),
        ),
        (
            "articulation.legato.release_note",
            ArtReleaseNote,
            ParamValue::String("C6".into()),
        ),
        (
            "articulation.legato.release_velocity",
            ArtReleaseVelocity,
            ParamValue::Number(61.0),
        ),
        (
            "articulation.legato.release_duration_ms",
            ArtReleaseDuration,
            ParamValue::Number(10.0),
        ),
        (
            "articulation.legato.release_sustain_aware",
            ArtReleaseSustainAware,
            ParamValue::Bool(true),
        ),
        (
            "articulation.legato.overlap_ms",
            ArtOverlap,
            ParamValue::Number(10.0),
        ),
        ("microtuning.a4.cents", NoteTuning, ParamValue::Cents(12.5)),
        (
            "control.brightness.cc14",
            ControlCc14,
            ParamValue::Number(7.0),
        ),
        ("control.expression.cc", ControlCc, ParamValue::Number(11.0)),
        (
            "control.brightness.param",
            ControlParam,
            ParamValue::String("cutoff".into()),
        ),
        ("cc7param", CcParam, ParamValue::String("cutoff".into())),
        (
            "control.brightness.note_expression",
            ControlNoteExpression,
            ParamValue::String("timbre".into()),
        ),
        (
            "control.brightness.on_note",
            ControlOnNote,
            ParamValue::String("C6".into()),
        ),
        (
            "control.brightness.off_note",
            ControlOffNote,
            ParamValue::String("D6".into()),
        ),
        (
            "control.brightness.noise_velocity",
            ControlNoiseVelocity,
            ParamValue::Number(55.0),
        ),
        (
            "control.brightness.noise_duration_ms",
            ControlNoiseDuration,
            ParamValue::Number(10.0),
        ),
        (
            "control.brightness.noise_threshold",
            ControlNoiseThreshold,
            ParamValue::Number(0.4),
        ),
        (
            "control.brightness.range_min",
            ControlRangeMin,
            ParamValue::Number(0.2),
        ),
        (
            "control.brightness.range_max",
            ControlRangeMax,
            ParamValue::Number(0.8),
        ),
        ("drum.snare.note", DrumNote, ParamValue::String("D1".into())),
        ("pad.snare.note", PadNote, ParamValue::String("C1".into())),
        (
            "drum.snare.choke_group",
            DrumChokeGroup,
            ParamValue::String("Open Hat".into()),
        ),
        (
            "drum.snare.choke_release_ms",
            DrumChokeRelease,
            ParamValue::Number(10.0),
        ),
        (
            "choke.OpenHat.release_ms",
            ChokeRelease,
            ParamValue::Number(20.0),
        ),
    ]
}
#[test]
fn profile_classifier_covers_every_parser_kind_and_actual_profile_consumers() {
    let samples = samples();
    assert_eq!(samples.len(), 43);
    let mut kinds = Vec::new();
    for (name, kind, value) in &samples {
        let candidate = classify_profile_assignment(name, value).unwrap();
        assert_eq!(candidate.kind, *kind, "{name}");
        assert!(!kinds.contains(kind));
        kinds.push(*kind);
    }
    let params: Vec<_> = samples
        .into_iter()
        .filter(|(_, kind, _)| !matches!(kind, Manifest | ScalaFile | TunFile))
        .map(|(name, _, value)| (name.to_owned(), value))
        .collect();
    let profile = InstrumentProfile::from_preset_params(&params, 48_000);
    assert_eq!(profile.output_pair, Some(2));
    assert!(profile.mpe_enabled);
    assert_eq!(
        (profile.mpe_first_channel, profile.mpe_last_channel),
        (2, 10)
    );
    assert_eq!(profile.keyswitch_duration_samples, 960);
    let art = &profile.articulation_profiles["legato"];
    assert_eq!(art.round_robin_keyswitches, vec![12, 14]);
    assert_eq!(art.keyswitch, Some(16));
    assert_eq!(
        art.control,
        Some(ArticulationControl {
            controller: 21,
            value: 64,
            reset_value: Some(0)
        })
    );
    assert_eq!(
        art.param,
        Some(ArticulationParam {
            name: "cutoff".into(),
            value: 0.6,
            reset_value: Some(0.4)
        })
    );
    assert_eq!(art.duration_scale, Some(1.1));
    assert_eq!(
        art.humanize,
        Some(ArticulationHumanize {
            timing_samples: 480,
            velocity: 5
        })
    );
    assert_eq!(
        art.release_trigger,
        Some(ArticulationReleaseTrigger {
            note: 84,
            velocity: 61,
            duration_samples: 480,
            sustain_aware: true
        })
    );
    assert_eq!(art.overlap_samples, Some(480));
    assert_eq!(profile.note_tunings_cents.get(&69), Some(&12.5));
    assert_eq!(profile.control_cc14.get("brightness"), Some(&7));
    assert_eq!(profile.control_cc.get("expression"), Some(&11));
    assert_eq!(
        profile
            .control_params
            .get("brightness")
            .map(|value| value.as_str()),
        Some("cutoff")
    );
    assert_eq!(
        profile.cc_params.get(&7).map(|value| value.as_str()),
        Some("cutoff")
    );
    assert_eq!(
        profile
            .control_note_expressions
            .get("brightness")
            .map(|value| value.as_str()),
        Some("timbre")
    );
    let noise = &profile.control_noise_triggers["brightness"];
    assert_eq!(noise.on_note, Some(84));
    assert_eq!(noise.off_note, Some(86));
    assert_eq!(noise.velocity, 55);
    assert_eq!(noise.duration_samples, 480);
    assert_eq!(
        (noise.threshold, noise.range_min, noise.range_max),
        (0.4, Some(0.2), Some(0.8))
    );
    assert_eq!(profile.drum_notes.get("snare"), Some(&26));
    assert_eq!(
        profile
            .drum_choke_groups
            .get("snare")
            .map(|value| value.as_str()),
        Some("openhat")
    );
    assert_eq!(profile.drum_choke_release_samples.get("snare"), Some(&480));
    assert_eq!(
        profile.drum_choke_release_samples.get("openhat"),
        Some(&960)
    );
}

#[test]
fn profile_classifier_preserves_ordered_alias_value_fallthrough_and_empty_legacy_selectors() {
    for (value, kind) in [
        (ParamValue::Number(17.0), ControlCc14),
        (ParamValue::String("17".into()), ControlCc14),
        (ParamValue::String("Cutoff".into()), CcParam),
    ] {
        assert_eq!(
            classify_profile_assignment("CC 14 param", &value)
                .unwrap()
                .kind,
            kind
        );
        let profile =
            InstrumentProfile::from_preset_params(&[("CC 14 param".into(), value)], 48_000);
        if kind == ControlCc14 {
            assert_eq!(profile.control_cc14.get("param"), Some(&17));
            assert!(profile.cc_params.is_empty());
        } else {
            assert_eq!(
                profile.cc_params.get(&14).map(|value| value.as_str()),
                Some("Cutoff")
            );
            assert!(profile.control_cc14.is_empty());
        }
    }
    let params = vec![
        ("keyswitch".into(), ParamValue::String("C0".into())),
        ("cc".into(), ParamValue::Number(11.0)),
        ("param".into(), ParamValue::String("cutoff".into())),
    ];
    let profile = InstrumentProfile::from_preset_params(&params, 48_000);
    assert_eq!(profile.articulation_profiles[""].keyswitch, Some(12));
    assert_eq!(profile.control_cc.get(""), Some(&11));
    assert_eq!(
        profile.control_params.get("").map(|value| value.as_str()),
        Some("cutoff")
    );
}

#[test]
fn profile_classifier_keeps_invalid_values_explicit_and_unknown_prefixes_native_candidates() {
    for name in [
        "control.cutoff",
        "articulation.legato",
        "mpe.bend_range",
        "drum.eq_gain",
        "profile.native_gain",
        "tuning.bogus.cents",
    ] {
        assert!(
            classify_profile_assignment(name, &ParamValue::Number(0.5)).is_none(),
            "{name}"
        );
    }
    // The cc<name> legacy alias accepts arbitrary control names. It is a
    // profile candidate, never a blanket native-parameter exemption.
    assert_eq!(
        classify_profile_assignment("cc999param", &ParamValue::Number(0.5))
            .unwrap()
            .kind,
        ControlCc
    );
    for value in [
        ParamValue::Array(vec![]),
        ParamValue::String("not a number".into()),
        ParamValue::Number(f64::NAN),
        ParamValue::String("NaN".into()),
    ] {
        assert_eq!(
            classify_profile_assignment("plugin_output_pair", &value)
                .unwrap()
                .value_status,
            ProfileValueStatus::InvalidValue
        );
    }
    for name in ["profile_manifest", "scala_file", "tun_file"] {
        assert_eq!(
            classify_profile_assignment(name, &ParamValue::String("never-opened".into()))
                .unwrap()
                .value_status,
            ProfileValueStatus::DeferredResource
        );
    }
    // A terminal scalar branch never falls through to the broad legacy
    // keyswitch alias when its value cannot be converted as milliseconds.
    let value = ParamValue::String("C4".into());
    assert_eq!(
        classify_profile_assignment("keyswitch_duration_ms", &value).unwrap(),
        ProfileAssignmentCandidate {
            kind: KeyswitchDuration,
            value_status: ProfileValueStatus::InvalidValue
        }
    );
    assert!(
        InstrumentProfile::from_preset_params(&[("keyswitch_duration_ms".into(), value)], 48_000)
            .articulation_profiles
            .is_empty()
    );
}

#[test]
fn profile_classifier_all_declared_aliases_use_the_same_normalization_and_value_owner() {
    let samples = samples();
    for rule in RULES {
        let value = &samples
            .iter()
            .find(|(_, kind, _)| *kind == rule.kind)
            .unwrap()
            .2;
        let names: Vec<_> = match rule.pattern {
            Pattern::Exact(names) => names.iter().map(|name| (*name).to_owned()).collect(),
            Pattern::Affix { prefix, suffix, .. } => {
                let selector = if rule.kind == CcParam {
                    "14"
                } else if rule.kind == NoteTuning {
                    "c4"
                } else {
                    "legato"
                };
                vec![format!("{prefix}.{selector}.{suffix}")]
            }
        };
        for name in names {
            let mixed = format!("_{}-", name.to_uppercase());
            let candidate = classify_profile_assignment(&mixed, value).unwrap();
            assert_eq!(candidate.kind, rule.kind, "{mixed}");
            assert_ne!(
                candidate.value_status,
                ProfileValueStatus::InvalidValue,
                "{mixed}"
            );
        }
    }
    for name in [
        "articulation.keyswitch",
        "control.cc",
        "control.cc14",
        "control.param",
        "drum.note",
        "pad.note",
        "choke.release_ms",
    ] {
        assert!(
            classify_profile_assignment(name, &ParamValue::Number(0.5)).is_none(),
            "{name}"
        );
    }
}
