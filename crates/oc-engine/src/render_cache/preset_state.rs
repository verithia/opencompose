//! Plugin preset state, params and sfizz glue.

use std::sync::Arc;

use super::*;

mod hex;
mod imported;
use imported::imported_vst3_state;
const MAX_STATE_BYTES: usize = oc_host::plugin_state_io::MAX_PLUGIN_STATE_BYTES;

/// Resolve numeric plugin parameters, excluding host-owned metadata keys.
fn numeric_parameter_assignments(
    format: oc_host::PluginFormat,
    params: &[(String, ParamValue)],
) -> Result<Vec<(&str, f64)>, String> {
    let mut assignments = Vec::new();
    for (name, value) in params {
        match oc_host::preset_parameters::classify_host_parameter(format, name) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) => return Err(format!("{format:?} parameter '{name}': {error}")),
        }
        let Some(value) = param_value_as_f64(value) else {
            return Err(format!(
                "{format:?} parameter '{name}' requires a numeric value"
            ));
        };
        assignments.push((name.as_str(), value));
    }
    Ok(assignments)
}

/// Apply plain preset parameters to a CLAP instance.
pub fn apply_clap_preset_params(
    instance: &mut ClapInstance,
    params: &[(String, ParamValue)],
) -> Result<(), String> {
    for (name, value) in numeric_parameter_assignments(oc_host::PluginFormat::Clap, params)? {
        if !instance.set_param_by_name(name, value) {
            return Err(format!("CLAP parameter '{name}' was rejected"));
        }
    }
    Ok(())
}

/// Apply state and parameters to a CLAP instance.
pub fn apply_clap_preset_state_at(
    instance: &mut ClapInstance,
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    let Some(bytes) = resolve_plugin_state_bytes(
        params,
        context,
        "clapstatehex",
        "clapstatefile",
        resource_root,
    )?
    else {
        return Ok(());
    };
    instance
        .set_state_bytes(&bytes)
        .map_err(|err| format!("failed to apply CLAP state: {}", err))
}

/// Apply AudioUnit parameters in the unit's own engineering units.
#[cfg(target_os = "macos")]
pub fn apply_au_preset_params(
    instance: &mut oc_host::au_instance::AuInstance,
    params: &[(String, ParamValue)],
) -> Result<(), String> {
    for (name, value) in numeric_parameter_assignments(oc_host::PluginFormat::Au, params)? {
        if !instance.set_param_by_name(name, value) {
            return Err(format!("AU parameter '{name}' was rejected"));
        }
    }
    Ok(())
}

/// Restore an AudioUnit's `kAudioUnitProperty_ClassInfo` from `au_state_*`.
#[cfg(target_os = "macos")]
pub fn apply_au_preset_state_at(
    instance: &mut oc_host::au_instance::AuInstance,
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    let Some(bytes) =
        resolve_plugin_state_bytes(params, context, "austatehex", "austatefile", resource_root)?
    else {
        return Ok(());
    };
    instance
        .set_state_bytes(&bytes)
        .map_err(|error| format!("failed to apply AU state: {error}"))
}

/// Apply LV2 control-port values in the port's own engineering units.
pub fn apply_lv2_preset_params(
    instance: &mut oc_host::lv2::Lv2Instance,
    params: &[(String, ParamValue)],
) -> Result<(), String> {
    for (name, value) in numeric_parameter_assignments(oc_host::PluginFormat::Lv2, params)? {
        if !instance.set_param_by_name(name, value) {
            return Err(format!("LV2 parameter '{name}' was rejected"));
        }
    }
    Ok(())
}

/// Restore an LV2 `state:interface` chunk from `lv2_state_hex` or `lv2_state_file`.
pub fn apply_lv2_preset_state_at(
    instance: &mut oc_host::lv2::Lv2Instance,
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    let Some(bytes) = resolve_plugin_state_bytes(
        params,
        context,
        "lv2statehex",
        "lv2statefile",
        resource_root,
    )?
    else {
        return Ok(());
    };
    instance
        .set_state_bytes(&bytes)
        .map_err(|error| format!("failed to apply LV2 state: {error}"))
}

/// Apply plain preset parameters to a VST2 instance.
pub fn apply_vst2_preset_params(
    instance: &mut oc_host::vst2::Vst2Instance,
    params: &[(String, ParamValue)],
) -> Result<(), String> {
    for (name, value) in numeric_parameter_assignments(oc_host::PluginFormat::Vst2, params)? {
        if !instance.set_param_by_name(name, value) {
            return Err(format!("VST2 parameter '{name}' was rejected"));
        }
    }
    Ok(())
}

/// Apply state and parameters to a VST2 instance.
pub fn apply_vst2_preset_state_at(
    instance: &mut oc_host::vst2::Vst2Instance,
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    let Some(bytes) = materialize_vst2_preset_state_at(params, context, resource_root)? else {
        return Ok(());
    };
    instance
        .set_state_bytes(&bytes)
        .map_err(|error| format!("failed to apply VST2 state: {error}"))
}

/// Resolve bounded external state before any native instance is prepared.
pub fn materialize_vst2_preset_state_at(
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<Option<Vec<u8>>, String> {
    resolve_plugin_state_bytes(
        params,
        context,
        "vst2statehex",
        "vst2statefile",
        resource_root,
    )
}

/// Apply plain preset parameters to a VST3 instance.
pub fn apply_vst3_preset_params(
    instance: &mut Vst3Instance,
    params: &[(String, ParamValue)],
) -> Result<(), String> {
    for (name, value) in numeric_parameter_assignments(oc_host::PluginFormat::Vst3, params)? {
        if !instance.set_param_by_name(name, value) {
            return Err(format!("VST3 parameter '{name}' was rejected"));
        }
    }
    Ok(())
}

/// Apply state and parameters to a VST3 instance.
pub fn apply_vst3_preset_state_at(
    instance: &mut Vst3Instance,
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    let imported = params.iter().any(|(name, value)| {
        oc_host::preset_parameters::host_parameter_key(name) == "serumfile"
            && matches!(value, ParamValue::String(_))
    });
    let Some(bytes) = materialize_vst3_preset_state_at(params, context, resource_root)? else {
        return Ok(());
    };
    instance.set_state_bytes(&bytes).map_err(|error| {
        if imported {
            format!("failed to apply imported preset: {error}")
        } else {
            format!("failed to apply VST3 state: {error}")
        }
    })
}

/// Resolve bounded processor bytes before any native instance is constructed.
pub fn materialize_vst3_preset_state_at(
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<Option<Arc<Vec<u8>>>, String> {
    if let Some(path) = params.iter().find_map(|(name, value)| {
        (oc_host::preset_parameters::host_parameter_key(name) == "serumfile").then_some(
            match value {
                ParamValue::String(path) => Some(path.as_str()),
                _ => None,
            },
        )?
    }) {
        let state = imported_vst3_state(path, resource_root)?;
        return Ok(Some(state));
    }
    resolve_plugin_state_bytes(
        params,
        context,
        "vst3statehex",
        "vst3statefile",
        resource_root,
    )
    .map(|state| state.map(Arc::new))
}

pub(super) fn param_value_as_f64(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Number(n) => Some(*n),
        ParamValue::Db(n) => Some(*n),
        ParamValue::Percentage(n) => Some(*n / 100.0),
        ParamValue::Frequency(n) => Some(*n),
        ParamValue::Time(n) => Some(*n),
        ParamValue::Cents(n) => Some(*n),
        ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        ParamValue::String(_) | ParamValue::Array(_) => None,
    }
}

pub(super) fn resolve_state_file_path<'a>(
    params: &'a [(String, ParamValue)],
    format_key: &str,
) -> Option<&'a str> {
    for wanted in [format_key, "pluginstatefile", "statefile"] {
        if let Some(path) = params.iter().find_map(|(name, value)| {
            if oc_host::preset_parameters::host_parameter_key(name) == wanted
                && let ParamValue::String(path) = value
            {
                return Some(path.as_str());
            }
            None
        }) {
            return Some(path);
        }
    }
    None
}

/// A borrowed state declaration; decoding admits its size before allocating bytes.
#[derive(Debug, Clone, Copy)]
pub(super) enum PluginStateInput<'a> {
    Hex(&'a str),
    File(&'a str),
}

pub(super) fn materialize_plugin_state_input(
    input: Option<PluginStateInput<'_>>,
    context: &oc_host::PluginStateContext<'_>,
    resource_root: Option<&Path>,
) -> Result<Option<Vec<u8>>, String> {
    let encoded = match input {
        Some(PluginStateInput::Hex(hex)) => decode_plugin_state_hex(context.format_label(), hex)?,
        Some(PluginStateInput::File(path)) => {
            read_plugin_state_file(context.format_label(), path, resource_root)?
        }
        None => return Ok(None),
    };
    oc_host::decode_plugin_state(&encoded, context).map(Some)
}

pub(super) fn resolve_plugin_state_bytes(
    params: &[(String, ParamValue)],
    context: &oc_host::PluginStateContext<'_>,
    format_hex_key: &str,
    format_file_key: &str,
    resource_root: Option<&Path>,
) -> Result<Option<Vec<u8>>, String> {
    let input = resolve_state_hex(params, format_hex_key)
        .map(PluginStateInput::Hex)
        .or_else(|| resolve_state_file_path(params, format_file_key).map(PluginStateInput::File));
    materialize_plugin_state_input(input, context, resource_root)
}

pub(super) fn resolve_state_hex<'a>(
    params: &'a [(String, ParamValue)],
    format_key: &str,
) -> Option<&'a str> {
    for wanted in [format_key, "pluginstatehex", "statehex"] {
        if let Some(hex) = params.iter().find_map(|(name, value)| {
            if oc_host::preset_parameters::host_parameter_key(name) == wanted
                && let ParamValue::String(hex) = value
            {
                return Some(hex.as_str());
            }
            None
        }) {
            return Some(hex);
        }
    }
    None
}

pub(super) fn decode_plugin_state_hex(format: &str, hex: &str) -> Result<Vec<u8>, String> {
    hex::decode(format, hex)
}

pub(super) fn read_plugin_state_file(
    format: &str,
    path: &str,
    resource_root: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let resolved = resolve_state_resource_path(path, resource_root);
    oc_host::plugin_state_io::read_regular_state_file(&resolved, MAX_STATE_BYTES).map_err(|error| {
        format!(
            "failed to read {format} state file '{path}' (resolved to '{}'): {error}",
            resolved.display()
        )
    })
}

fn resolve_state_resource_path(path: &str, resource_root: Option<&Path>) -> PathBuf {
    let declared = Path::new(path);
    if declared.is_absolute() {
        declared.to_path_buf()
    } else if let Some(root) = resource_root {
        root.join(declared)
    } else {
        declared.to_path_buf()
    }
}

pub(super) fn note_source_project(project: &Project) -> Project {
    let mut source = project.clone();
    for track in &mut source.tracks {
        track.performance = None;
        // The graph owns fader and pan. Baking them into MIDI CC7/CC10 here
        // applies the mixer twice (with the sampler's nonlinear CC law too),
        // and makes cached sources depend on otherwise mixer-only edits.
        track.mix.volume_db = 0.0;
        track.mix.pan = 0.0;
        track.mix.mute = false;
    }
    source
}

pub(super) fn is_sfizz_sample_preset(preset: &PresetDef) -> bool {
    preset.source.as_deref() == Some("sample")
        && preset
            .plugin
            .as_deref()
            .is_some_and(|plugin| plugin.eq_ignore_ascii_case("sfizz"))
}

/// Encode sfizz-ui's public VST3 component-state ABI (version 5). This lets an
/// OCP sample preset load an SFZ without opening a plugin editor. The format is
/// defined by sfizz-ui's BSD-licensed `SfizzVstState::store` implementation.
pub(crate) fn sfizz_vst3_state_bytes(
    preset: &PresetDef,
    resource_root: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let declared = preset
        .file
        .as_deref()
        .ok_or_else(|| format!("sfizz sample preset '{}' has no SFZ file", preset.name))?;
    let path = Path::new(declared);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(root) = resource_root {
        root.join(path)
    } else {
        path.to_path_buf()
    };
    let resolved = resolved.canonicalize().map_err(|err| {
        format!(
            "sfizz sample preset '{}' could not resolve SFZ '{}' (resolved to '{}'): {}",
            preset.name,
            declared,
            resolved.display(),
            err
        )
    })?;
    let path = resolved.to_string_lossy();
    let mut state = Vec::with_capacity(path.len() + 64);
    state.extend_from_slice(&5_u64.to_le_bytes());
    write_vst3_str8(&mut state, &path)?;
    state.extend_from_slice(&0_f32.to_le_bytes()); // volume dB
    state.extend_from_slice(&64_i32.to_le_bytes()); // voices
    state.extend_from_slice(&0_i32.to_le_bytes()); // oversampling log2
    state.extend_from_slice(&8192_i32.to_le_bytes()); // preload
    write_vst3_str8(&mut state, "")?; // Scala path
    state.extend_from_slice(&60_i32.to_le_bytes());
    state.extend_from_slice(&440_f32.to_le_bytes());
    state.extend_from_slice(&0_f32.to_le_bytes());
    state.extend_from_slice(&2_i32.to_le_bytes()); // live sample quality
    state.extend_from_slice(&1_i32.to_le_bytes()); // live oscillator quality
    state.extend_from_slice(&10_i32.to_le_bytes()); // offline sample quality
    state.extend_from_slice(&3_i32.to_le_bytes()); // offline oscillator quality
    state.extend_from_slice(&0_i16.to_le_bytes()); // sustain cancels release
    state.extend_from_slice(&(-1_i32).to_le_bytes()); // last keyswitch
    state.extend_from_slice(&0_u32.to_le_bytes()); // saved CC count
    Ok(state)
}

pub(super) fn write_vst3_str8(output: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let length = value
        .len()
        .checked_add(1)
        .and_then(|length| i32::try_from(length).ok())
        .ok_or_else(|| "VST3 state string exceeds i32 length".to_string())?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    output.push(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_metadata_is_not_written_as_a_plugin_parameter() {
        let params = vec![
            (
                "clapstatehex".to_string(),
                ParamValue::String("00".to_string()),
            ),
            ("Gain".to_string(), ParamValue::Number(0.5)),
        ];
        let assignments =
            numeric_parameter_assignments(oc_host::PluginFormat::Clap, &params).unwrap();
        assert_eq!(assignments, vec![("Gain", 0.5)]);
    }

    #[test]
    fn a_non_numeric_plugin_parameter_is_refused() {
        let params = vec![("Gain".to_string(), ParamValue::String("loud".to_string()))];
        let error =
            numeric_parameter_assignments(oc_host::PluginFormat::Clap, &params).unwrap_err();
        assert!(error.contains("requires a numeric value"), "{error}");
    }

    #[test]
    fn host_metadata_is_filtered_for_every_native_format() {
        let params = vec![
            ("statehex".to_string(), ParamValue::String("00".to_string())),
            ("Gain".to_string(), ParamValue::Number(0.5)),
        ];
        for format in [
            oc_host::PluginFormat::Clap,
            oc_host::PluginFormat::Au,
            oc_host::PluginFormat::Lv2,
            oc_host::PluginFormat::Vst2,
            oc_host::PluginFormat::Vst3,
        ] {
            let assignments = numeric_parameter_assignments(format, &params).unwrap();
            assert_eq!(assignments, vec![("Gain", 0.5)], "{format:?}");
        }
    }

    #[test]
    fn a_rejected_vst3_parameter_is_reported_by_name() {
        let mut instance = oc_host::vst3_instance::test_support::fake_instance_for_id(
            "test.fake.one_bus_unity",
            512,
        )
        .expect("in-process fake VST3");
        let params = vec![(
            "no-such-authored-parameter".to_string(),
            ParamValue::Number(0.5),
        )];
        let error = apply_vst3_preset_params(&mut instance, &params).unwrap_err();
        assert!(error.contains("no-such-authored-parameter"), "{error}");
    }

    #[test]
    fn metadata_for_another_backend_is_refused() {
        let params = vec![(
            "vst3statehex".to_string(),
            ParamValue::String("00".to_string()),
        )];
        let error =
            numeric_parameter_assignments(oc_host::PluginFormat::Clap, &params).unwrap_err();
        assert!(error.contains("belongs to"), "{error}");
    }
}
