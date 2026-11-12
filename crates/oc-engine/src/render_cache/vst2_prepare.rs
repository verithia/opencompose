//! One original-authored request owner for UI-owned VST2 instrument preparation.
use super::*;
use crate::preset::hosted_parameters::{
    AssignmentAnnotation, EffectiveHostParameter, HostedPresetInput, assemble_hosted_parameters,
};
use oc_host::preset_parameters::{HostOwnedParameter, HostStateEncoding, ParameterWriteDomain};
use oc_host::vst2::{
    MAX_VST2_AUTHORED_NAME_BYTES, MAX_VST2_AUTHORED_PARAMETERS, MAX_VST2_AUTHORED_TOTAL_NAME_BYTES,
    Vst2AuthoredClaim, Vst2AuthoredInput, Vst2OwnedAuthoredPlan, Vst2OwnedError,
};

#[cfg(test)]
mod tests;

pub(super) type Vst2TailProbe = fn(Vst2OwnedAuthoredPlan) -> Result<Option<u32>, Vst2OwnedError>;

pub(super) fn execute_probe(plan: Vst2OwnedAuthoredPlan) -> Result<Option<u32>, Vst2OwnedError> {
    plan.execute_probe_on_ui()
        .map(|snapshot| snapshot.tail_samples)
}

pub(super) fn instrument_tail(
    definitions: &[PresetDef],
    preset: &PresetDef,
    track: &oc_dsl::ast::Track,
    sample_rate: u32,
    resource_root: Option<&Path>,
    probe: Vst2TailProbe,
) -> Result<u32, String> {
    let plan = instrument_request(definitions, preset, track, sample_rate, resource_root)?;
    // Unknown/unbounded native tails keep the shared render horizon fallback.
    // A failed preparation or CLOSE is an error, never a successful fallback.
    probe(plan).map(|tail| tail.unwrap_or(0)).map_err(|error| {
        format!(
            "VST2 tail preparation for preset '{}' on track '{}': {error}",
            preset.name, track.name
        )
    })
}

pub(super) fn instrument_request(
    definitions: &[PresetDef],
    preset: &PresetDef,
    track: &oc_dsl::ast::Track,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vst2OwnedAuthoredPlan, String> {
    let plugin = preset
        .plugin
        .as_deref()
        .ok_or_else(|| format!("VST2 preset '{}' has no plugin id", preset.name))?;
    let info = find_plugin_info(plugin, PluginFormat::Vst2).ok_or_else(|| {
        format!(
            "VST2 plugin '{plugin}' for preset '{}' was not found",
            preset.name
        )
    })?;
    let assembled = assemble_hosted_parameters(HostedPresetInput {
        definitions,
        preset_name: &preset.name,
        track: Some(track),
        derived: &[],
        format: PluginFormat::Vst2,
    })
    .map_err(|error| error.to_string())?;
    if assembled.authored.len() > MAX_VST2_AUTHORED_PARAMETERS {
        return Err("VST2 authored parameter count exceeds its preparation budget".into());
    }
    let mut name_bytes = 0usize;
    for assignment in &assembled.authored {
        name_bytes = name_bytes
            .checked_add(assignment.name.len())
            .filter(|total| *total <= MAX_VST2_AUTHORED_TOTAL_NAME_BYTES)
            .ok_or("VST2 authored parameter names exceed their preparation budget")?;
        if assignment.name.len() > MAX_VST2_AUTHORED_NAME_BYTES {
            return Err("VST2 authored parameter name exceeds its preparation budget".into());
        }
    }
    // Typed track routing is consumed independently of authored native values.
    let mut routing = InstrumentProfile::default();
    routing.set_output_port(
        track.output_port.map(usize::from),
        track.output_pair.map(usize::from),
    );
    resolve_output_pair(
        &[AudioPort::STEREO],
        &routing,
        "VST2",
        &preset.name,
        &track.name,
    )?;
    let routing_rows: Vec<_> = assembled
        .authored
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            crate::render_control::classify_profile_assignment(row.name, row.value)
                .filter(|candidate| {
                    candidate.kind == crate::render_control::ProfileKeyKind::OutputPair
                })
                .map(|_| index)
        })
        .collect();
    let checked = crate::render_control::checked_profile_assignments(
        routing_rows.iter().map(|&index| {
            let row = &assembled.authored[index];
            (row.name, row.value)
        }),
        sample_rate,
        resource_root,
    )
    .map_err(|error| error.to_string())?;
    let mut claims = vec![Vst2AuthoredClaim::NativeOnly; assembled.authored.len()];
    let eligible: Vec<_> = routing_rows
        .iter()
        .zip(checked)
        .filter_map(|(&index, accepted)| {
            if !matches!(
                accepted,
                crate::render_control::ProfileAssignmentClaim::Accepted
            ) {
                return None;
            }
            crate::render_control::checked_output_pair(assembled.authored[index].value)
                .ok()
                .map(|pair| (index, pair))
        })
        .collect();
    // An external output consumer applies ordered overrides as one chain. Native
    // inventory still arbitrates every grant, including shadowed routing rows.
    if eligible.last().is_some_and(|(_, pair)| *pair == 0) {
        for (index, _) in eligible {
            claims[index] = Vst2AuthoredClaim::InstrumentOutputRouting;
        }
    }
    let parameters = assembled
        .authored
        .iter()
        .enumerate()
        .filter_map(|(index, assignment)| match assignment.annotation {
            AssignmentAnnotation::Host(_) | AssignmentAnnotation::Engine(_) => None,
            _ => Some(Vst2AuthoredInput {
                name: assignment.name.to_owned(),
                native_value: param_value_as_f64(assignment.value),
                domain: ParameterWriteDomain::Normalized,
                claim: claims[index],
            }),
        })
        .collect();
    let context = PluginStateContext::new(PluginFormat::Vst2, Some(info.id.as_str()), &preset.name);
    let state = materialize_plugin_state_input(
        borrowed_state_input(&assembled.host_parameters)?,
        &context,
        resource_root,
    )?;
    Ok(Vst2OwnedAuthoredPlan {
        path: PathBuf::from(info.path),
        sample_rate,
        max_frames: 512,
        state,
        parameters,
    })
}

fn borrowed_state_input<'a>(
    parameters: &[EffectiveHostParameter<'a>],
) -> Result<Option<PluginStateInput<'a>>, String> {
    for entry in parameters {
        let HostOwnedParameter::State(encoding) = entry.parameter else {
            continue;
        };
        let ParamValue::String(text) = entry.assignment.value else {
            return Err("VST2 state requires a string declaration".into());
        };
        return match encoding {
            HostStateEncoding::Hex => Ok(Some(PluginStateInput::Hex(text))),
            HostStateEncoding::File => Ok(Some(PluginStateInput::File(text))),
            HostStateEncoding::ImportedVst3Resource => {
                Err("VST2 does not accept imported VST3 state resources".into())
            }
        };
    }
    Ok(None)
}
