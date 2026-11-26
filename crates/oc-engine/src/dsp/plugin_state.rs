//! Insert state uses the same bounded decoder and identity envelope as instruments.

use oc_dsl::ast::ParamValue;
use oc_host::preset_parameters::{HostOwnedParameter, HostStateEncoding};
use oc_host::{PluginFormat, PluginStateContext};
use std::collections::HashMap;
use std::path::Path;

use crate::render_cache::{PluginStateInput, materialize_plugin_state_input};

pub(super) struct Prepared<'a> {
    pub state: Option<Vec<u8>>,
    pub controls: HashMap<&'a str, f64>,
    pub sidechain: Option<super::plugin_sidechain::Selection>,
}

pub(super) fn prepare<'a>(
    format: PluginFormat,
    plugin: &str,
    place: &str,
    params: &'a [(String, ParamValue)],
    resource_root: Option<&Path>,
) -> Result<Prepared<'a>, String> {
    let mut winners = HashMap::new();
    let mut controls = HashMap::new();
    for (name, value) in params {
        if let Some(parameter) = oc_host::preset_parameters::classify_host_parameter(format, name)
            .map_err(|error| format!("effect '{plugin}' {place}: {error}"))?
        {
            winners.insert(parameter.consumer(), (name, parameter, value));
        } else {
            let value = value.as_f64().filter(|value| value.is_finite()).ok_or_else(|| {
                format!("effect '{plugin}' {place}, {name}: native control requires a finite numeric value")
            })?;
            controls.insert(name.as_str(), value);
        }
    }
    let mut state = None;
    for (_, (name, parameter, value)) in winners {
        parameter
            .validate_value(value)
            .map_err(|error| format!("effect '{plugin}' {place}, {name}: {error}"))?;
        if let HostOwnedParameter::State(encoding) = parameter {
            let ParamValue::String(text) = value else {
                unreachable!("host value is validated")
            };
            let input = match encoding {
                HostStateEncoding::Hex => PluginStateInput::Hex(text),
                HostStateEncoding::File => PluginStateInput::File(text),
                HostStateEncoding::ImportedVst3Resource => {
                    return Err(format!(
                        "effect '{plugin}' {place}: use a plugin state file or inline state for insert effects"
                    ));
                }
            };
            let context = PluginStateContext::new(format, Some(plugin), place);
            state = materialize_plugin_state_input(Some(input), &context, resource_root)
                .map_err(|error| format!("effect '{plugin}' {place}: {error}"))?;
        }
    }
    let sidechain = controls
        .remove(super::plugin_sidechain::PARAMETER)
        .map(|value| super::plugin_sidechain::Selection::parse(format, value))
        .transpose()
        .map_err(|error| format!("effect '{plugin}' {place}: {error}"))?;
    Ok(Prepared {
        state,
        controls,
        sidechain,
    })
}

pub(super) fn apply<E: std::fmt::Display>(
    state: &Option<Vec<u8>>,
    restore: impl FnOnce(&[u8]) -> Result<(), E>,
) -> Result<(), String> {
    if let Some(bytes) = state {
        restore(bytes)
            .map_err(|error| format!("failed to restore external effect state: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(text: &str) -> Vec<(String, ParamValue)> {
        vec![("state_hex".into(), ParamValue::String(text.into()))]
    }

    #[test]
    fn insert_state_decodes_shared_envelopes_and_checks_format_identity_and_integrity() {
        let encoded = oc_host::encode_plugin_state(PluginFormat::Vst3, "test.effect", b"opaque");
        let hex: String = encoded.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            prepare(
                PluginFormat::Vst3,
                "test.effect",
                "bus",
                &params(&hex),
                None
            )
            .unwrap()
            .state,
            Some(b"opaque".to_vec())
        );
        assert!(
            prepare(
                PluginFormat::Vst3,
                "different.effect",
                "bus",
                &params(&hex),
                None
            )
            .is_err()
        );
        assert!(prepare(PluginFormat::Au, "test.effect", "bus", &params(&hex), None).is_err());
        let mut broken = encoded;
        *broken.last_mut().unwrap() ^= 1;
        let hex: String = broken.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(
            prepare(
                PluginFormat::Vst3,
                "test.effect",
                "bus",
                &params(&hex),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn last_host_state_alias_wins_before_value_validation() {
        let values = vec![
            ("state_hex".into(), ParamValue::Bool(false)),
            ("vst3_state_hex".into(), ParamValue::String("00 ff".into())),
        ];
        assert_eq!(
            prepare(PluginFormat::Vst3, "test.effect", "master", &values, None)
                .unwrap()
                .state,
            Some(vec![0, 255])
        );
    }

    #[test]
    fn malformed_wrong_backend_and_unavailable_states_are_refused() {
        let root = oc_test_support::temp_dir("insert-state", "missing");
        for text in ["", "0", "💥", "xx"] {
            assert!(
                prepare(
                    PluginFormat::Vst3,
                    "test.effect",
                    "master",
                    &params(text),
                    Some(&root)
                )
                .is_err()
            );
        }
        for (name, value) in [
            ("au_state_hex", ParamValue::String("00".into())),
            ("state_file", ParamValue::String("unopened.bin".into())),
            ("state_hex", ParamValue::Number(0.0)),
        ] {
            assert!(
                prepare(
                    PluginFormat::Vst3,
                    "test.effect",
                    "master",
                    &[(name.into(), value)],
                    Some(&root),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn file_state_uses_the_project_root_and_checks_the_envelope() {
        let root = oc_test_support::temp_dir("insert-state", "sidecar");
        let path = root.join("state.bin");
        std::fs::write(
            &path,
            oc_host::encode_plugin_state(PluginFormat::Vst3, "test.effect", b"saved bank"),
        )
        .unwrap();
        let values = [
            ("state_hex".into(), ParamValue::String("00".into())),
            (
                "vst3_state_file".into(),
                ParamValue::String("state.bin".into()),
            ),
        ];
        let result = prepare(
            PluginFormat::Vst3,
            "test.effect",
            "master",
            &values,
            Some(&root),
        );
        assert_eq!(
            result.unwrap().state.as_deref(),
            Some(b"saved bank".as_slice())
        );
        assert!(
            prepare(
                PluginFormat::Vst3,
                "another.effect",
                "master",
                &values,
                Some(&root)
            )
            .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            prepare(
                PluginFormat::Vst3,
                "test.effect",
                "master",
                &values,
                Some(&root)
            )
            .is_err()
        );
    }

    #[test]
    fn missing_state_does_not_restore_and_native_refusal_is_not_success() {
        apply::<String>(&None, |_| panic!("no state to apply")).unwrap();
        let error = apply(&Some(vec![1]), |_| Err("native refusal")).unwrap_err();
        assert!(error.contains("native refusal"));
    }

    #[test]
    fn sidechain_selection_is_host_owned_and_uses_the_last_authored_index() {
        let values = vec![
            ("sidechain_input".into(), ParamValue::Number(-1.0)),
            ("Gain".into(), ParamValue::Number(0.5)),
            ("sidechain_input".into(), ParamValue::Number(1.0)),
        ];
        let input = prepare(PluginFormat::Vst3, "test.effect", "master", &values, None).unwrap();
        assert_eq!(input.controls, HashMap::from([("Gain", 0.5)]));
        let mut selected = None;
        input
            .sidechain
            .unwrap()
            .apply(2, |index| selected = Some(index))
            .unwrap();
        assert_eq!(selected, Some(1));
        for format in [PluginFormat::Au, PluginFormat::Vst2, PluginFormat::Lv2] {
            assert!(prepare(format, "test.effect", "master", &values, None).is_err());
        }
    }

    #[test]
    fn host_metadata_never_becomes_a_numeric_native_parameter() {
        for format in [
            PluginFormat::Vst3,
            PluginFormat::Vst2,
            PluginFormat::Clap,
            PluginFormat::Au,
            PluginFormat::Lv2,
        ] {
            let values = vec![
                ("Gain".into(), ParamValue::Number(0.5)),
                ("state_hex".into(), ParamValue::Bool(false)),
                ("plugin_state_hex".into(), ParamValue::String("00".into())),
                ("plugin_version".into(), ParamValue::String("1".into())),
            ];
            let input = prepare(format, "test.effect", "master", &values, None).unwrap();
            assert_eq!(input.controls, HashMap::from([("Gain", 0.5)]));
            assert_eq!(input.state, Some(vec![0]));
        }
        for value in [
            ParamValue::Number(f64::NAN),
            ParamValue::String("unbound".into()),
        ] {
            assert!(
                prepare(
                    PluginFormat::Vst3,
                    "test.effect",
                    "master",
                    &[("Gain".into(), value)],
                    None,
                )
                .is_err()
            );
        }
    }
}
