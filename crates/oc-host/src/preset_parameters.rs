//! Checked control-plane application of authored plugin parameters.
//!
//! Concrete backends supply authoritative bindings and native fault checks. A
//! batch binds and validates its effective last assignments before any write;
//! private bindings never escape to another owner or preparation task.

use crate::PluginFormat;
use oc_dsl::ast::ParamValue;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How an authored value is interpreted.
pub enum ParameterWriteDomain {
    NativePlain,
    Normalized,
    /// Existing VST3 authored values in 0..=1 are normalized; others are plain.
    LegacyVst3Auto,
}

#[derive(Debug, Clone, Copy)]
/// One authored parameter write.
pub struct AuthoredParameter<'a> {
    pub name: &'a str,
    pub value: f64,
    pub domain: ParameterWriteDomain,
}

/// Raw owned values remain unvalidated until canonical native alias winners exist.
#[derive(Debug, Clone)]
pub struct OwnedAuthoredParameter {
    pub name: String,
    pub value: Option<f64>,
    pub domain: ParameterWriteDomain,
}
trait AssignmentView {
    fn name(&self) -> &str;
    fn value(&self) -> Option<f64>;
    fn domain(&self) -> ParameterWriteDomain;
}
impl AssignmentView for AuthoredParameter<'_> {
    fn name(&self) -> &str {
        self.name
    }
    fn value(&self) -> Option<f64> {
        Some(self.value)
    }
    fn domain(&self) -> ParameterWriteDomain {
        self.domain
    }
}
impl AssignmentView for OwnedAuthoredParameter {
    fn name(&self) -> &str {
        &self.name
    }
    fn value(&self) -> Option<f64> {
        self.value
    }
    fn domain(&self) -> ParameterWriteDomain {
        self.domain
    }
}

#[derive(Debug, Clone, Copy)]
/// Inclusive native parameter range.
pub struct ParameterRange {
    pub min: f64,
    pub max: f64,
}

/// A static binding from the backend's complete inventory, including hidden
/// writable controls. UI visibility and automation eligibility do not imply
/// that a stored static assignment is forbidden.
#[derive(Debug)]
pub struct ResolvedParameter<K> {
    pub key: K,
    pub writable: bool,
    /// Required only when the selected write domain is native plain units.
    pub plain_range: Option<ParameterRange>,
}

#[derive(Debug, thiserror::Error)]
/// Why a parameter write was refused.
pub enum ParameterErrorKind {
    #[error("unknown parameter")]
    Unknown,
    #[error("parameter name is ambiguous")]
    Ambiguous,
    #[error("parameter is read-only")]
    ReadOnly,
    #[error("parameter value is not finite")]
    NonFinite,
    #[error("parameter requires a numeric value")]
    NonNumeric,
    #[error("value {value} is outside {min}..={max}")]
    OutOfRange { value: f64, min: f64, max: f64 },
    #[error("parameter range metadata is missing or invalid")]
    InvalidMetadata,
    #[error("value cannot be represented by the native parameter type")]
    Unrepresentable,
    #[error("the write domain is unsupported by this plugin format")]
    UnsupportedDomain,
    #[error("parameter inventory changed during preparation")]
    InventoryChanged,
    #[error("native parameter callback is unavailable")]
    MissingCallback,
    #[error("reserved host parameter requires a string; state inputs must not be empty")]
    InvalidHostValue,
    #[error("reserved host parameter belongs to {expected:?}, not {actual:?}")]
    WrongBackend {
        expected: PluginFormat,
        actual: PluginFormat,
    },
    #[error("parameter collides with a reserved host metadata name")]
    ReservedNameCollision,
    #[error("{operation}: {source}")]
    Native {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

/// A fully checked value in the backend's actual dispatch domain.
#[derive(Debug, Clone, Copy)]
pub struct PreparedParameterValue {
    pub value: f64,
    pub domain: ParameterWriteDomain,
}

/// Original typed native errors remain available through `Error::source`.
#[derive(Debug)]
pub struct ParameterApplyError {
    pub format: PluginFormat,
    pub parameter: String,
    pub kind: ParameterErrorKind,
}
impl fmt::Display for ParameterApplyError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{:?} parameter '{}': {}",
            self.format, self.parameter, self.kind
        )
    }
}
impl Error for ParameterApplyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.kind)
    }
}

/// Preparation stays synchronous on the backend's authorized control owner.
/// `check_parameter_authority` must refuse an existing native fault or a
/// reentrant inventory invalidation; it must not refresh IDs midway through a
/// batch. On native failure the caller discards this candidate owner.
pub trait AuthoredParameterOwner {
    type Key: Clone + Eq + Hash;

    fn parameter_format(&self) -> PluginFormat;
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind>;
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<Self::Key>, ParameterErrorKind>;
    /// Validate native conversion without changing the parameter. VST3 plain
    /// conversion occurs here, before the first value in the batch is written.
    fn prepare_bound_value(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind>;
    /// Dispatch the prevalidated native value without silently clamping or
    /// ignoring rejected native calls.
    fn write_bound_parameter(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind>;

    fn admit_authored_batch(&mut self, _assignments: usize) -> Result<(), ParameterErrorKind> {
        Ok(())
    }
    fn apply_authored_parameters(
        &mut self,
        assignments: &[AuthoredParameter<'_>],
    ) -> Result<(), ParameterApplyError> {
        apply_batch(self, assignments)
    }
    fn apply_owned_authored_parameters(
        &mut self,
        assignments: &[OwnedAuthoredParameter],
    ) -> Result<(), ParameterApplyError> {
        apply_batch(self, assignments)
    }
}
fn apply_batch<O: AuthoredParameterOwner + ?Sized, A: AssignmentView>(
    owner: &mut O,
    assignments: &[A],
) -> Result<(), ParameterApplyError> {
    let format = owner.parameter_format();
    let fail = |name: &str, kind| ParameterApplyError {
        format,
        parameter: name.to_owned(),
        kind,
    };
    owner
        .check_parameter_authority()
        .map_err(|kind| fail("<batch>", kind))?;
    owner
        .admit_authored_batch(assignments.len())
        .map_err(|kind| fail("<batch>", kind))?;
    let writes = preflight(owner, assignments)?;
    for write in writes {
        owner
            .check_parameter_authority()
            .map_err(|kind| fail(write.name, kind))?;
        owner
            .write_bound_parameter(&write.key, write.value, write.domain)
            .map_err(|kind| fail(write.name, kind))?;
        owner
            .check_parameter_authority()
            .map_err(|kind| fail(write.name, kind))?;
    }
    owner
        .check_parameter_authority()
        .map_err(|kind| fail("<batch>", kind))
}

struct PreparedWrite<'a, K> {
    key: K,
    name: &'a str,
    value: f64,
    domain: ParameterWriteDomain,
}

fn preflight<'a, O: AuthoredParameterOwner + ?Sized, A: AssignmentView>(
    owner: &mut O,
    assignments: &'a [A],
) -> Result<Vec<PreparedWrite<'a, O::Key>>, ParameterApplyError> {
    let format = owner.parameter_format();
    let mut effective = HashMap::new();
    for (ordinal, assignment) in assignments.iter().enumerate() {
        let fail = |kind| ParameterApplyError {
            format,
            parameter: assignment.name().to_owned(),
            kind,
        };
        owner.check_parameter_authority().map_err(fail)?;
        if classify_host_parameter(format, assignment.name())
            .map_err(fail)?
            .is_some()
        {
            return Err(fail(ParameterErrorKind::ReservedNameCollision));
        }
        let binding = owner
            .resolve_authored_parameter(assignment.name())
            .map_err(fail)?;
        owner.check_parameter_authority().map_err(fail)?;
        effective.insert(binding.key.clone(), (ordinal, assignment, binding));
    }
    let mut effective: Vec<_> = effective.into_values().collect();
    effective.sort_unstable_by_key(|(ordinal, _, _)| *ordinal);
    effective
        .into_iter()
        .map(|(_, assignment, binding)| {
            let assignment = AuthoredParameter {
                name: assignment.name(),
                value: assignment.value().ok_or_else(|| ParameterApplyError {
                    format,
                    parameter: assignment.name().to_owned(),
                    kind: ParameterErrorKind::NonNumeric,
                })?,
                domain: assignment.domain(),
            };
            let domain = validate_assignment(format, &assignment, &binding).map_err(|kind| {
                ParameterApplyError {
                    format,
                    parameter: assignment.name.to_owned(),
                    kind,
                }
            })?;
            let fail = |kind| ParameterApplyError {
                format,
                parameter: assignment.name.to_owned(),
                kind,
            };
            let prepared = owner
                .prepare_bound_value(&binding.key, assignment.value, domain)
                .map_err(fail)?;
            owner.check_parameter_authority().map_err(fail)?;
            if prepared.domain == ParameterWriteDomain::LegacyVst3Auto {
                return Err(fail(ParameterErrorKind::UnsupportedDomain));
            }
            let converted = AuthoredParameter {
                name: assignment.name,
                value: prepared.value,
                domain: prepared.domain,
            };
            let domain = validate_assignment(format, &converted, &binding).map_err(fail)?;
            Ok(PreparedWrite {
                key: binding.key,
                name: assignment.name,
                value: prepared.value,
                domain,
            })
        })
        .collect()
}

fn validate_assignment<K>(
    format: PluginFormat,
    assignment: &AuthoredParameter<'_>,
    binding: &ResolvedParameter<K>,
) -> Result<ParameterWriteDomain, ParameterErrorKind> {
    if !binding.writable {
        return Err(ParameterErrorKind::ReadOnly);
    }
    if !assignment.value.is_finite() {
        return Err(ParameterErrorKind::NonFinite);
    }
    use ParameterWriteDomain::{LegacyVst3Auto, NativePlain, Normalized};
    let domain = match (format, assignment.domain) {
        (PluginFormat::Vst3, LegacyVst3Auto) => {
            if (0.0..=1.0).contains(&assignment.value) {
                Normalized
            } else {
                NativePlain
            }
        }
        (PluginFormat::Vst3, NativePlain | Normalized)
        | (PluginFormat::Vst2, Normalized)
        | (PluginFormat::Clap | PluginFormat::Au | PluginFormat::Lv2, NativePlain) => {
            assignment.domain
        }
        _ => return Err(ParameterErrorKind::UnsupportedDomain),
    };
    let range = match domain {
        Normalized => ParameterRange { min: 0.0, max: 1.0 },
        NativePlain => binding
            .plain_range
            .ok_or(ParameterErrorKind::InvalidMetadata)?,
        LegacyVst3Auto => unreachable!("the authored domain was resolved"),
    };
    if !range.min.is_finite() || !range.max.is_finite() || range.min > range.max {
        return Err(ParameterErrorKind::InvalidMetadata);
    }
    if assignment.value < range.min || assignment.value > range.max {
        return Err(ParameterErrorKind::OutOfRange {
            value: assignment.value,
            min: range.min,
            max: range.max,
        });
    }
    if matches!(
        format,
        PluginFormat::Vst2 | PluginFormat::Au | PluginFormat::Lv2
    ) && !(assignment.value as f32).is_finite()
    {
        return Err(ParameterErrorKind::Unrepresentable);
    }
    Ok(domain)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Last-write-wins consumer for host-owned values.
pub enum HostParameterConsumer {
    State,
    PluginVersion,
    PluginArchitecture,
    PluginStateAbi,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How host-owned state is encoded.
pub enum HostStateEncoding {
    File,
    Hex,
    ImportedVst3Resource,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A host-owned parameter that is not a plugin control.
pub enum HostOwnedParameter {
    State(HostStateEncoding),
    PluginVersion,
    PluginArchitecture,
    PluginStateAbi,
}
impl HostOwnedParameter {
    /// Synonyms and state representations share one last-write-wins consumer.
    pub fn consumer(self) -> HostParameterConsumer {
        match self {
            Self::State(_) => HostParameterConsumer::State,
            Self::PluginVersion => HostParameterConsumer::PluginVersion,
            Self::PluginArchitecture => HostParameterConsumer::PluginArchitecture,
            Self::PluginStateAbi => HostParameterConsumer::PluginStateAbi,
        }
    }
    /// Validate a host-owned parameter value.
    pub fn validate_value(self, value: &ParamValue) -> Result<(), ParameterErrorKind> {
        match value {
            ParamValue::String(text)
                if !matches!(self, Self::State(_)) || !text.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err(ParameterErrorKind::InvalidHostValue),
        }
    }
}

/// Compact parameter separators and ASCII case without interpreting plugin
/// identities, filename extensions, paths, or backend prefixes.
pub fn host_parameter_key(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Exact reserved-key authority. Parameter-key normalization preserves the
/// documented separator aliases without stripping plugin identity syntax.
pub fn classify_host_parameter(
    format: PluginFormat,
    name: &str,
) -> Result<Option<HostOwnedParameter>, ParameterErrorKind> {
    use HostOwnedParameter::{PluginArchitecture, PluginStateAbi, PluginVersion, State};
    use HostStateEncoding::{File, Hex, ImportedVst3Resource};
    let key = host_parameter_key(name);
    let (owner, parameter) = match key.as_str() {
        "statefile" | "pluginstatefile" => (None, State(File)),
        "statehex" | "pluginstatehex" => (None, State(Hex)),
        "clapstatefile" => (Some(PluginFormat::Clap), State(File)),
        "clapstatehex" => (Some(PluginFormat::Clap), State(Hex)),
        "vst3statefile" => (Some(PluginFormat::Vst3), State(File)),
        "vst3statehex" => (Some(PluginFormat::Vst3), State(Hex)),
        "vst2statefile" => (Some(PluginFormat::Vst2), State(File)),
        "vst2statehex" => (Some(PluginFormat::Vst2), State(Hex)),
        "austatefile" => (Some(PluginFormat::Au), State(File)),
        "austatehex" => (Some(PluginFormat::Au), State(Hex)),
        "lv2statefile" => (Some(PluginFormat::Lv2), State(File)),
        "lv2statehex" => (Some(PluginFormat::Lv2), State(Hex)),
        "serumfile" => (Some(PluginFormat::Vst3), State(ImportedVst3Resource)),
        "pluginversion" => (None, PluginVersion),
        "pluginarchitecture" => (None, PluginArchitecture),
        "pluginstateabi" => (None, PluginStateAbi),
        _ => return Ok(None),
    };
    if let Some(expected) = owner
        && expected != format
    {
        return Err(ParameterErrorKind::WrongBackend {
            expected,
            actual: format,
        });
    }
    if format == PluginFormat::Internal {
        return Err(ParameterErrorKind::UnsupportedDomain);
    }
    Ok(Some(parameter))
}

#[cfg(test)]
mod tests;
