use super::*;

struct FakeOwner {
    format: PluginFormat,
    writable: bool,
    range: ParameterRange,
    writes: Vec<(u32, f64, ParameterWriteDomain)>,
    invalidate_after_write: bool,
    conversion_failure: bool,
    conversion_result: Option<f64>,
}
impl FakeOwner {
    fn new(format: PluginFormat) -> Self {
        Self {
            format,
            writable: true,
            range: ParameterRange {
                min: -100.0,
                max: 100.0,
            },
            writes: Vec::new(),
            invalidate_after_write: false,
            conversion_failure: false,
            conversion_result: None,
        }
    }
}
impl AuthoredParameterOwner for FakeOwner {
    type Key = u32;
    fn parameter_format(&self) -> PluginFormat {
        self.format
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        if self.invalidate_after_write && !self.writes.is_empty() {
            Err(ParameterErrorKind::InventoryChanged)
        } else {
            Ok(())
        }
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<u32>, ParameterErrorKind> {
        // Hidden writable and non-automatable static controls are present
        // in this complete inventory; a filtered UI list is not consulted.
        let key = match name {
            "gain" | "Gain Alias" => 1,
            "hidden" | "static only" => 2,
            "readonly" => 3,
            "StateFile.dll" | "state_file.dll" => 4,
            "ambiguous" => return Err(ParameterErrorKind::Ambiguous),
            _ => return Err(ParameterErrorKind::Unknown),
        };
        Ok(ResolvedParameter {
            key,
            writable: self.writable && key != 3,
            plain_range: Some(self.range),
        })
    }
    fn prepare_bound_value(
        &mut self,
        _: &u32,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        if self.conversion_failure && value == 25.0 {
            return Err(ParameterErrorKind::Native {
                operation: "plainParamToNormalized",
                source: Box::new(std::io::Error::other("original conversion fault")),
            });
        }
        Ok(PreparedParameterValue {
            value: self.conversion_result.unwrap_or(value),
            domain,
        })
    }
    fn write_bound_parameter(
        &mut self,
        key: &u32,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        self.writes.push((*key, value, domain));
        Ok(())
    }
}
fn assignment(name: &str, value: f64, domain: ParameterWriteDomain) -> AuthoredParameter<'_> {
    AuthoredParameter {
        name,
        value,
        domain,
    }
}

#[test]
fn authored_batch_validates_all_winners_before_writing_and_keeps_last_alias() {
    let mut owner = FakeOwner::new(PluginFormat::Clap);
    let plain = ParameterWriteDomain::NativePlain;
    owner
        .apply_authored_parameters(&[
            assignment("gain", f64::NAN, plain),
            assignment("hidden", 12.0, plain),
            assignment("Gain Alias", 24.0, plain),
        ])
        .unwrap();
    assert_eq!(owner.writes, [(2, 12.0, plain), (1, 24.0, plain)]);
    owner.writes.clear();
    let error = owner
        .apply_authored_parameters(&[
            assignment("gain", 24.0, plain),
            assignment("Gain Alias", f64::NAN, plain),
        ])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::NonFinite));
    assert!(owner.writes.is_empty());
    for name in ["readonly", "unknown", "ambiguous"] {
        assert!(
            owner
                .apply_authored_parameters(&[
                    assignment("gain", 10.0, plain),
                    assignment(name, 1.0, plain),
                ])
                .is_err()
        );
        assert!(owner.writes.is_empty());
    }
    owner
        .apply_authored_parameters(&[assignment("static only", 7.0, plain)])
        .unwrap();
    assert_eq!(owner.writes, [(2, 7.0, plain)]);
}

#[test]
fn authored_batch_enforces_each_format_domain_without_clamping() {
    use ParameterWriteDomain::{LegacyVst3Auto, NativePlain, Normalized};
    for format in [
        PluginFormat::Clap,
        PluginFormat::Au,
        PluginFormat::Lv2,
        PluginFormat::Vst2,
        PluginFormat::Vst3,
    ] {
        let mut owner = FakeOwner::new(format);
        let domain = if format == PluginFormat::Vst2 {
            Normalized
        } else if format == PluginFormat::Vst3 {
            LegacyVst3Auto
        } else {
            NativePlain
        };
        for value in [0.0, 1.0] {
            owner
                .apply_authored_parameters(&[assignment("gain", value, domain)])
                .unwrap();
        }
        let expected = if matches!(format, PluginFormat::Vst2 | PluginFormat::Vst3) {
            Normalized
        } else {
            NativePlain
        };
        assert_eq!(owner.writes[1], (1, 1.0, expected));
        owner.writes.clear();
        assert!(
            owner
                .apply_authored_parameters(&[assignment("gain", 101.0, domain)])
                .is_err()
        );
        assert!(owner.writes.is_empty());
        if format != PluginFormat::Vst2 {
            owner
                .apply_authored_parameters(&[assignment("gain", -24.0, domain)])
                .unwrap();
            assert_eq!(owner.writes[0], (1, -24.0, NativePlain));
        }
    }
    let mut owner = FakeOwner::new(PluginFormat::Clap);
    assert!(matches!(
        owner
            .apply_authored_parameters(&[assignment("gain", 0.5, Normalized)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::UnsupportedDomain
    ));
}

#[test]
fn authored_batch_refuses_malformed_ranges_and_unrepresentable_native_values() {
    for range in [
        ParameterRange { min: 2.0, max: 1.0 },
        ParameterRange {
            min: f64::NAN,
            max: 1.0,
        },
        ParameterRange {
            min: 0.0,
            max: f64::INFINITY,
        },
    ] {
        let mut owner = FakeOwner::new(PluginFormat::Clap);
        owner.range = range;
        assert!(matches!(
            owner
                .apply_authored_parameters(&[assignment(
                    "gain",
                    0.5,
                    ParameterWriteDomain::NativePlain
                )])
                .unwrap_err()
                .kind,
            ParameterErrorKind::InvalidMetadata
        ));
        assert!(owner.writes.is_empty());
    }
    let mut owner = FakeOwner::new(PluginFormat::Clap);
    owner.range = ParameterRange { min: 4.0, max: 4.0 };
    owner
        .apply_authored_parameters(&[assignment("gain", 4.0, ParameterWriteDomain::NativePlain)])
        .unwrap();
    assert!(
        owner
            .apply_authored_parameters(&[assignment(
                "gain",
                4.1,
                ParameterWriteDomain::NativePlain
            )])
            .is_err()
    );
    for format in [PluginFormat::Au, PluginFormat::Lv2] {
        let mut owner = FakeOwner::new(format);
        owner.range = ParameterRange {
            min: -f64::MAX,
            max: f64::MAX,
        };
        assert!(matches!(
            owner
                .apply_authored_parameters(&[assignment(
                    "gain",
                    f64::MAX,
                    ParameterWriteDomain::NativePlain
                )])
                .unwrap_err()
                .kind,
            ParameterErrorKind::Unrepresentable
        ));
        assert!(owner.writes.is_empty());
    }
}

#[test]
fn authored_batch_prepares_conversions_before_writes_and_preserves_native_cause() {
    let mut owner = FakeOwner::new(PluginFormat::Vst3);
    owner.conversion_failure = true;
    let error = owner
        .apply_authored_parameters(&[
            assignment("gain", 0.25, ParameterWriteDomain::LegacyVst3Auto),
            assignment("hidden", 25.0, ParameterWriteDomain::LegacyVst3Auto),
        ])
        .unwrap_err();
    assert_eq!(error.parameter, "hidden");
    assert!(error.to_string().contains("original conversion fault"));
    let native = error.source().unwrap().source().unwrap();
    assert_eq!(
        native.downcast_ref::<std::io::Error>().unwrap().to_string(),
        "original conversion fault"
    );
    assert!(owner.writes.is_empty());
    owner.conversion_failure = false;
    owner.conversion_result = Some(f64::NAN);
    assert!(matches!(
        owner
            .apply_authored_parameters(&[assignment("gain", 0.5, ParameterWriteDomain::Normalized)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::NonFinite
    ));
    assert!(owner.writes.is_empty());
}

#[test]
fn authored_batch_stops_after_reentrant_inventory_invalidation() {
    let mut owner = FakeOwner::new(PluginFormat::Clap);
    owner.invalidate_after_write = true;
    let error = owner
        .apply_authored_parameters(&[
            assignment("gain", 1.0, ParameterWriteDomain::NativePlain),
            assignment("hidden", 2.0, ParameterWriteDomain::NativePlain),
        ])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::InventoryChanged));
    assert_eq!(owner.writes.len(), 1);
}

#[test]
fn host_parameter_registry_covers_only_exact_documented_backend_keys() {
    let formats = [
        PluginFormat::Clap,
        PluginFormat::Vst3,
        PluginFormat::Vst2,
        PluginFormat::Au,
        PluginFormat::Lv2,
    ];
    for format in formats {
        for (name, expected) in [
            (
                "state_file",
                HostOwnedParameter::State(HostStateEncoding::File),
            ),
            (
                "plugin_state_file",
                HostOwnedParameter::State(HostStateEncoding::File),
            ),
            (
                "state_hex",
                HostOwnedParameter::State(HostStateEncoding::Hex),
            ),
            (
                "plugin_state_hex",
                HostOwnedParameter::State(HostStateEncoding::Hex),
            ),
            ("plugin_version", HostOwnedParameter::PluginVersion),
            (
                "plugin_architecture",
                HostOwnedParameter::PluginArchitecture,
            ),
            ("plugin_state_abi", HostOwnedParameter::PluginStateAbi),
        ] {
            assert_eq!(
                classify_host_parameter(format, name).unwrap(),
                Some(expected)
            );
            assert!(
                expected
                    .validate_value(&ParamValue::String("fixture".into()))
                    .is_ok()
            );
            if matches!(expected, HostOwnedParameter::State(_)) {
                assert!(
                    expected
                        .validate_value(&ParamValue::String(" ".into()))
                        .is_err()
                );
            } else {
                assert!(
                    expected
                        .validate_value(&ParamValue::String(String::new()))
                        .is_ok()
                );
            }
            for invalid in [ParamValue::Number(1.0), ParamValue::Array(vec![])] {
                assert!(matches!(
                    expected.validate_value(&invalid),
                    Err(ParameterErrorKind::InvalidHostValue)
                ));
            }
        }
        for (backend, prefix) in [
            (PluginFormat::Clap, "clap"),
            (PluginFormat::Vst3, "vst3"),
            (PluginFormat::Vst2, "vst2"),
            (PluginFormat::Au, "au"),
            (PluginFormat::Lv2, "lv2"),
        ] {
            for suffix in ["state_file", "state_hex"] {
                let result = classify_host_parameter(format, &format!("{prefix}_{suffix}"));
                assert_eq!(result.is_ok(), backend == format);
            }
        }
        for ordinary in [
            "sample_file",
            "open_synth_state_file",
            "midi_program",
            "mpe.bend_range",
            "plugin_output_pair",
            "output.gain",
            "other_state_file",
            "plugin_other",
            "tuning.reference_hz",
        ] {
            assert!(
                classify_host_parameter(format, ordinary).unwrap().is_none(),
                "{ordinary}"
            );
        }
    }
    assert!(matches!(
        classify_host_parameter(PluginFormat::Vst3, "serum_file").unwrap(),
        Some(HostOwnedParameter::State(
            HostStateEncoding::ImportedVst3Resource
        ))
    ));
    assert!(classify_host_parameter(PluginFormat::Clap, "serum_file").is_err());
    assert_eq!(
        classify_host_parameter(PluginFormat::Vst3, "Plugin.State-Hex").unwrap(),
        Some(HostOwnedParameter::State(HostStateEncoding::Hex))
    );
    assert_eq!(
        HostOwnedParameter::State(HostStateEncoding::File).consumer(),
        HostOwnedParameter::State(HostStateEncoding::Hex).consumer()
    );
    let mut owner = FakeOwner::new(PluginFormat::Clap);
    assert!(matches!(
        owner
            .apply_authored_parameters(&[assignment(
                "state_file",
                1.0,
                ParameterWriteDomain::NativePlain
            )])
            .unwrap_err()
            .kind,
        ParameterErrorKind::ReservedNameCollision
    ));
    assert!(owner.writes.is_empty());
}

#[test]
fn owned_authored_raw_values_are_validated_after_canonical_alias_winners() {
    let row = |name: &str, value| OwnedAuthoredParameter {
        name: name.into(),
        value,
        domain: ParameterWriteDomain::Normalized,
    };
    for invalid in [None, Some(f64::NAN), Some(f64::INFINITY)] {
        let mut owner = FakeOwner::new(PluginFormat::Vst3);
        owner
            .apply_owned_authored_parameters(&[row("gain", invalid), row("Gain Alias", Some(0.75))])
            .unwrap();
        assert_eq!(owner.writes, [(1, 0.75, ParameterWriteDomain::Normalized)]);
        owner.writes.clear();
        let error = owner
            .apply_owned_authored_parameters(&[row("gain", Some(0.75)), row("Gain Alias", invalid)])
            .unwrap_err();
        assert!(match invalid {
            None => matches!(error.kind, ParameterErrorKind::NonNumeric),
            Some(_) => matches!(error.kind, ParameterErrorKind::NonFinite),
        });
        assert!(owner.writes.is_empty());
    }
}

#[test]
fn host_parameter_keys_preserve_filename_suffixes_and_identity_prefixes() {
    assert_eq!(host_parameter_key("Plugin.State-Hex"), "pluginstatehex");
    for (name, expected) in [
        ("StateFile.dll", "statefiledll"),
        ("state_file.vst3", "statefilevst3"),
        ("vst3:state_file.dll", "vst3statefiledll"),
        ("plugin:state_file", "pluginstatefile"),
        ("vendor:state_file", "vendorstatefile"),
    ] {
        assert_eq!(host_parameter_key(name), expected);
    }
    for format in [
        PluginFormat::Clap,
        PluginFormat::Vst2,
        PluginFormat::Vst3,
        PluginFormat::Au,
        PluginFormat::Lv2,
    ] {
        for name in [
            "StateFile.dll",
            "state_file.dll",
            "state_file.vst3",
            "plugin_version.vst",
            "vendor:state_file",
            "vst3:state_file.dll",
            "ＳtateFile",
        ] {
            assert_eq!(
                classify_host_parameter(format, name).unwrap(),
                None,
                "{name}"
            );
        }
        for alias in [
            "state_file",
            "State.File",
            "STATE-FILE",
            " plugin : state_file ",
        ] {
            assert_eq!(
                classify_host_parameter(format, alias).unwrap(),
                Some(HostOwnedParameter::State(HostStateEncoding::File)),
                "{alias}"
            );
        }
    }
    let mut native = FakeOwner::new(PluginFormat::Vst2);
    native
        .apply_owned_authored_parameters(&[OwnedAuthoredParameter {
            name: "StateFile.dll".into(),
            value: Some(0.5),
            domain: ParameterWriteDomain::Normalized,
        }])
        .unwrap();
    assert_eq!(native.writes, [(4, 0.5, ParameterWriteDomain::Normalized)]);
    native.writes.clear();
    let invalid_native = native
        .apply_owned_authored_parameters(&[OwnedAuthoredParameter {
            name: "state_file.dll".into(),
            value: None,
            domain: ParameterWriteDomain::Normalized,
        }])
        .unwrap_err();
    assert!(matches!(
        invalid_native.kind,
        ParameterErrorKind::NonNumeric
    ));
    assert!(native.writes.is_empty());
}
