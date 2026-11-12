//! Pure authored-input assembly for hosted preparation. No instance is loaded,
//! no state path is read, and no native parameter value is decoded here.
//!
//! Authored native and profile candidates are deliberately not ready writes:
//! activation must resolve native inventory collisions and validate the selected
//! consumer before routing an entry to native binding or engine use.

use oc_dsl::ast::{ParamValue, PresetDef, Track};
use oc_dsl::preset::PresetResolution;
use oc_host::PluginFormat;
use oc_host::preset_parameters::{
    HostOwnedParameter, HostParameterConsumer, ParameterErrorKind, classify_host_parameter,
};
use std::collections::HashMap;

/// Authored preset input for hosted parameter assembly.
pub struct HostedPresetInput<'a> {
    /// Original definitions retain inheritance-layer assignment order.
    pub definitions: &'a [PresetDef],
    pub preset_name: &'a str,
    /// Effective hosting track, independent of a pattern's preset override.
    pub track: Option<&'a Track>,
    /// Explicitly derived engine configuration, never a flattened authored list.
    pub derived: &'a [(String, ParamValue)],
    /// Canonical backend chosen by the existing plugin resolution owner.
    pub format: PluginFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Where a hosted assignment came from.
pub enum AssignmentOrigin<'a> {
    Preset(&'a str),
    TrackOverride(&'a str),
    DerivedEngine,
}
impl AssignmentOrigin<'_> {
    fn describe(self) -> String {
        match self {
            Self::Preset(name) => format!("preset '{name}'"),
            Self::TrackOverride(name) => format!("track '{name}'"),
            Self::DerivedEngine => "derived engine configuration".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Engine-owned consumer for a parameter.
pub enum KnownEngineConsumer {
    /// Exact track.engine_params key consumed by MIDI import/export.
    MidiProgram,
    /// Exact track.engine_params key consumed by MIDI import/export.
    MidiBendRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How an assignment participates in mixing.
pub enum AssignmentAnnotation {
    Host(HostOwnedParameter),
    Engine(KnownEngineConsumer),
    /// Key recognition does not reserve a namespace against native parameters.
    /// Native inventory ambiguity and external resources remain unresolved.
    ProfileCandidate(crate::render_control::ProfileAssignmentCandidate),
    /// Native binding and value validation remain pending.
    UnclassifiedAuthored,
    DerivedEngine,
}

#[derive(Debug, Clone, Copy)]
/// One hosted parameter assignment.
pub struct HostedAssignment<'a> {
    pub name: &'a str,
    pub value: &'a ParamValue,
    pub origin: AssignmentOrigin<'a>,
    pub annotation: AssignmentAnnotation,
}

#[derive(Debug, Clone, Copy)]
/// The winning parameter after assembly.
pub struct EffectiveHostParameter<'a> {
    pub parameter: HostOwnedParameter,
    pub assignment: HostedAssignment<'a>,
}

#[derive(Debug)]
/// Assembled hosted parameters and diagnostics.
pub struct HostedPresetAssembly<'a> {
    /// Metadata follows the DSL owner; do not use its flattened params to
    /// reconstruct native alias precedence. Use the ordered authored entries.
    pub resolution: PresetResolution<'a>,
    pub track: Option<&'a Track>,
    pub authored: Vec<HostedAssignment<'a>>,
    pub derived: Vec<HostedAssignment<'a>>,
    /// Effective state/provenance assignments in their winning source order.
    /// State aliases share one consumer, including file/hex representations.
    pub host_parameters: Vec<EffectiveHostParameter<'a>>,
}

#[derive(Debug, thiserror::Error)]
/// Why hosted parameter assembly failed.
pub enum HostedInputError {
    #[error("{0}")]
    PresetResolution(String),
    #[error("{origin} parameter '{name}': {source}")]
    HostParameter {
        origin: String,
        name: String,
        #[source]
        source: ParameterErrorKind,
    },
}

fn annotate(
    format: PluginFormat,
    name: &str,
    value: &ParamValue,
    origin: AssignmentOrigin<'_>,
) -> Result<AssignmentAnnotation, ParameterErrorKind> {
    if let Some(parameter) = classify_host_parameter(format, name)? {
        return Ok(AssignmentAnnotation::Host(parameter));
    }
    if matches!(origin, AssignmentOrigin::TrackOverride(_)) {
        match name {
            "midi_program" => {
                return Ok(AssignmentAnnotation::Engine(
                    KnownEngineConsumer::MidiProgram,
                ));
            }
            "midi_bend_range" => {
                return Ok(AssignmentAnnotation::Engine(
                    KnownEngineConsumer::MidiBendRange,
                ));
            }
            _ => {}
        }
    }
    if let Some(candidate) = crate::render_control::classify_profile_assignment(name, value) {
        return Ok(AssignmentAnnotation::ProfileCandidate(candidate));
    }
    Ok(AssignmentAnnotation::UnclassifiedAuthored)
}

/// Assemble hosted parameters from preset inputs.
pub fn assemble_hosted_parameters<'a>(
    input: HostedPresetInput<'a>,
) -> Result<HostedPresetAssembly<'a>, HostedInputError> {
    let resolution =
        oc_dsl::preset::resolve_preset_with_layers(input.definitions, input.preset_name)
            .map_err(HostedInputError::PresetResolution)?;
    let mut authored = Vec::new();
    let mut push = |name: &'a str, value: &'a ParamValue, origin: AssignmentOrigin<'a>| {
        let annotation = annotate(input.format, name, value, origin).map_err(|source| {
            HostedInputError::HostParameter {
                origin: origin.describe(),
                name: name.to_owned(),
                source,
            }
        })?;
        authored.push(HostedAssignment {
            name,
            value,
            origin,
            annotation,
        });
        Ok::<_, HostedInputError>(())
    };
    for &preset in &resolution.layers {
        for (name, value) in &preset.params {
            push(name.as_str(), value, AssignmentOrigin::Preset(&preset.name))?;
        }
    }
    if let Some(track) = input.track {
        for (name, value) in track.engine_params.iter().flatten() {
            push(
                name.as_str(),
                value,
                AssignmentOrigin::TrackOverride(&track.name),
            )?;
        }
    }
    let mut effective = HashMap::new();
    for (ordinal, assignment) in authored.iter().copied().enumerate() {
        if let AssignmentAnnotation::Host(parameter) = assignment.annotation {
            effective.insert(
                parameter.consumer(),
                (
                    ordinal,
                    EffectiveHostParameter {
                        parameter,
                        assignment,
                    },
                ),
            );
        }
    }
    let mut effective: Vec<_> = effective.into_values().collect();
    effective.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    let host_parameters = effective
        .into_iter()
        .map(|(_, effective)| {
            effective
                .parameter
                .validate_value(effective.assignment.value)
                .map_err(|source| HostedInputError::HostParameter {
                    origin: effective.assignment.origin.describe(),
                    name: effective.assignment.name.to_owned(),
                    source,
                })?;
            Ok(effective)
        })
        .collect::<Result<Vec<_>, HostedInputError>>()?;
    let derived = input
        .derived
        .iter()
        .map(|(name, value)| HostedAssignment {
            name,
            value,
            origin: AssignmentOrigin::DerivedEngine,
            annotation: AssignmentAnnotation::DerivedEngine,
        })
        .collect();
    Ok(HostedPresetAssembly {
        resolution,
        track: input.track,
        authored,
        derived,
        host_parameters,
    })
}

impl HostedPresetAssembly<'_> {
    /// Look up the effective hosted parameter by name.
    pub fn host_parameter(
        &self,
        consumer: HostParameterConsumer,
    ) -> Option<&EffectiveHostParameter<'_>> {
        self.host_parameters
            .iter()
            .find(|entry| entry.parameter.consumer() == consumer)
    }
}

#[cfg(test)]
mod tests;
