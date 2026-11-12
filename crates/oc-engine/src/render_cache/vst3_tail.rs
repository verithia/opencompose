//! UI-owned preparation for an instrument's declared offline tail.

use super::*;
use crate::preset::hosted_parameters::{
    AssignmentAnnotation, HostedPresetInput, assemble_hosted_parameters,
};
use crate::render_control::{ProfileAssignmentClaim, checked_profile_assignments};
use oc_host::preset_parameters::ParameterWriteDomain;
use oc_host::vst3_instance::{
    MAX_VST3_AUTHORED_NAME_BYTES, MAX_VST3_AUTHORED_PARAMETERS, MAX_VST3_AUTHORED_TOTAL_NAME_BYTES,
    Vst3AuthoredInput, Vst3OwnedAuthoredPlan, Vst3OwnedError, Vst3ProcessingBudget,
};

#[cfg(test)]
mod tests;

pub(super) type Vst3TailProbe = fn(Vst3OwnedAuthoredPlan) -> Result<u32, Vst3OwnedError>;

pub(super) fn execute_probe(plan: Vst3OwnedAuthoredPlan) -> Result<u32, Vst3OwnedError> {
    plan.execute_probe_on_ui()
        .map(|snapshot| snapshot.tail_samples)
}

pub(super) struct Vst3InstrumentRequest {
    pub plan: Vst3OwnedAuthoredPlan,
    invalid_claims: Vec<(String, String)>,
}
impl Vst3InstrumentRequest {
    pub fn execute(self) -> Result<oc_host::vst3_instance::Vst3OwnedInstance, String> {
        let Self {
            plan,
            invalid_claims,
        } = self;
        plan.execute_on_ui().map_err(|error| {
            let detail = if let Vst3OwnedError::Authored(apply) = &error {
                invalid_claims
                    .iter()
                    .rev()
                    .find(|(name, _)| name == &apply.parameter)
                    .map(|(_, cause)| format!("; external profile input: {cause}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            format!("{error}{detail}")
        })
    }
    pub fn merge_shared(&mut self, other: Self) -> Result<(), String> {
        let native = |rows: &[Vst3AuthoredInput]| {
            rows.iter()
                .filter(|row| !row.external_claim)
                .map(|row| (row.name.clone(), row.native_value, row.domain))
                .collect::<Vec<_>>()
        };
        // Profile consumers are per-track. Only actual native initial values
        // and component state must agree across the shared component.
        if self.plan.path != other.plan.path
            || self.plan.component_state != other.plan.component_state
            || native(&self.plan.parameters) != native(&other.plan.parameters)
        {
            return Err("shared VST3 native initialization differs".into());
        }
        let mut claimed: std::collections::HashSet<_> = self
            .plan
            .parameters
            .iter()
            .filter(|row| row.external_claim)
            .map(|row| row.name.clone())
            .collect();
        for row in other
            .plan
            .parameters
            .into_iter()
            .filter(|row| row.external_claim)
        {
            if claimed.insert(row.name.clone()) {
                self.plan.parameters.push(row);
            }
        }
        if self.plan.parameters.len() > MAX_VST3_AUTHORED_PARAMETERS {
            return Err("shared VST3 authored claim budget exceeded".into());
        }
        self.invalid_claims.extend(other.invalid_claims);
        Ok(())
    }
}
pub(super) fn instrument_tail(
    definitions: &[PresetDef],
    preset: &PresetDef,
    track: &oc_dsl::ast::Track,
    sample_rate: u32,
    resource_root: Option<&Path>,
    probe: Vst3TailProbe,
) -> Result<u32, String> {
    let mut request = instrument_request(definitions, preset, track, sample_rate, resource_root)?;
    request.plan.budget = Vst3ProcessingBudget {
        events: request.plan.parameters.len().max(1),
        points_per_parameter: 1,
    };
    let diagnostic = request.invalid_claims;
    probe(request.plan).map_err(|error| {
        let detail = if let Vst3OwnedError::Authored(apply) = &error {
            diagnostic
                .iter()
                .rev()
                .find(|(name, _)| name == &apply.parameter)
                .map(|(_, cause)| format!("; external profile input: {cause}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        format!(
            "VST3 tail preparation for preset '{}' on track '{}': {error}{detail}",
            preset.name, track.name
        )
    })
}

/// One bounded state/authored-value assembly owner for tail and actual audio.
pub(super) fn instrument_request(
    definitions: &[PresetDef],
    preset: &PresetDef,
    track: &oc_dsl::ast::Track,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vst3InstrumentRequest, String> {
    let plugin_id = preset
        .plugin
        .as_deref()
        .ok_or_else(|| format!("VST3 preset '{}' has no plugin id", preset.name))?;
    let (path, identity) = resolve_plugin(plugin_id, &preset.name)?;
    let assembled = assemble_hosted_parameters(HostedPresetInput {
        definitions,
        preset_name: &preset.name,
        track: Some(track),
        derived: &[],
        format: PluginFormat::Vst3,
    })
    .map_err(|error| error.to_string())?;
    if assembled.authored.len() > MAX_VST3_AUTHORED_PARAMETERS {
        return Err("VST3 authored parameter count exceeds its preparation budget".into());
    }
    let mut name_bytes = 0usize;
    for assignment in &assembled.authored {
        name_bytes = name_bytes
            .checked_add(assignment.name.len())
            .filter(|total| *total <= MAX_VST3_AUTHORED_TOTAL_NAME_BYTES)
            .ok_or("VST3 authored parameter names exceed their preparation budget")?;
        if assignment.name.len() > MAX_VST3_AUTHORED_NAME_BYTES {
            return Err("VST3 authored parameter name exceeds its preparation budget".into());
        }
    }
    let claims = checked_profile_assignments(
        assembled
            .authored
            .iter()
            .map(|assignment| (assignment.name, assignment.value)),
        sample_rate,
        resource_root,
    )
    .map_err(|error| error.to_string())?;
    let parameters = assembled
        .authored
        .iter()
        .zip(&claims)
        .filter_map(|(assignment, claim)| match assignment.annotation {
            AssignmentAnnotation::Host(_) | AssignmentAnnotation::Engine(_) => None,
            _ => Some(Vst3AuthoredInput {
                name: assignment.name.to_owned(),
                native_value: param_value_as_f64(assignment.value),
                domain: ParameterWriteDomain::LegacyVst3Auto,
                external_claim: matches!(claim, ProfileAssignmentClaim::Accepted),
            }),
        })
        .collect::<Vec<_>>();
    let component_state = if is_sfizz_sample_preset(preset) {
        Some(sfizz_vst3_state_bytes(preset, resource_root)?)
    } else {
        let state_parameters = assembled
            .host_parameters
            .iter()
            .map(|entry| {
                (
                    entry.assignment.name.to_owned(),
                    entry.assignment.value.clone(),
                )
            })
            .collect::<Vec<_>>();
        let context =
            PluginStateContext::new(PluginFormat::Vst3, Some(identity.as_str()), &preset.name);
        materialize_vst3_preset_state_at(&state_parameters, &context, resource_root)?
            .map(|bytes| bytes.as_ref().clone())
    };
    let invalid_claims = assembled
        .authored
        .iter()
        .zip(&claims)
        .filter_map(|(row, claim)| {
            if let ProfileAssignmentClaim::Invalid(cause) = claim {
                Some((row.name.to_owned(), cause.to_string()))
            } else {
                None
            }
        })
        .collect();
    Ok(Vst3InstrumentRequest {
        plan: Vst3OwnedAuthoredPlan {
            path,
            sample_rate,
            max_frames: 512,
            component_state,
            parameters,
            budget: Vst3ProcessingBudget {
                events: 65536,
                points_per_parameter: 1024,
            },
        },
        invalid_claims,
    })
}

fn resolve_plugin(plugin_id: &str, preset_name: &str) -> Result<(PathBuf, String), String> {
    #[cfg(any(test, feature = "test-support"))]
    if oc_host::vst3_instance::test_support::fake_plugin_bus_count(plugin_id).is_some() {
        return Ok((PathBuf::from(plugin_id), plugin_id.to_owned()));
    }
    find_plugin_info(plugin_id, PluginFormat::Vst3)
        .map(|info| (PathBuf::from(info.path), info.id))
        .ok_or_else(|| {
            format!("VST3 plugin '{plugin_id}' for preset '{preset_name}' was not found")
        })
}
