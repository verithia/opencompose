//! Checked static VST2 authoring, separate from realtime convenience setters.
use super::*;
use crate::preset_parameters::{
    AuthoredParameterOwner, ParameterErrorKind, ParameterWriteDomain, PreparedParameterValue,
    ResolvedParameter,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Debug)]
/// Identity of an authored VST2 parameter.
pub struct Vst2AuthoredParameterKey {
    index: i32,
    authority: Arc<()>,
}
impl PartialEq for Vst2AuthoredParameterKey {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.authority, &other.authority)
    }
}
impl Eq for Vst2AuthoredParameterKey {}
impl Hash for Vst2AuthoredParameterKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        Arc::as_ptr(&self.authority).hash(state);
    }
}

/// VST2 has no standard static read-only bit: automation eligibility is not
/// writability. Every native index is writable when setParameter is present.
pub struct Vst2AuthoredParameterOwner<'a> {
    instance: &'a mut Vst2Instance,
    identity: Arc<()>,
    names: HashMap<String, Vec<i32>>,
    compact_names: HashMap<String, Vec<i32>>,
    count: i32,
    setter: Option<abi::SetParameterProc>,
    dispatcher: abi::DispatcherProc,
    revision: u64,
}
impl Vst2Instance {
    /// Borrow the authored parameter owner.
    pub fn authored_parameter_owner(
        &mut self,
    ) -> Result<Vst2AuthoredParameterOwner<'_>, ParameterErrorKind> {
        Vst2AuthoredParameterOwner::new(self)
    }
    pub(super) fn claimed_parameter_owner(
        &mut self,
    ) -> Result<Vst2AuthoredParameterOwner<'_>, ParameterErrorKind> {
        Vst2AuthoredParameterOwner::new_with_requirement(self, SetterRequirement::Optional)
    }
}
#[derive(Clone, Copy)]
enum SetterRequirement {
    Required,
    Optional,
}
impl<'a> Vst2AuthoredParameterOwner<'a> {
    fn new(instance: &'a mut Vst2Instance) -> Result<Self, ParameterErrorKind> {
        Self::new_with_requirement(instance, SetterRequirement::Required)
    }
    fn new_with_requirement(
        instance: &'a mut Vst2Instance,
        requirement: SetterRequirement,
    ) -> Result<Self, ParameterErrorKind> {
        instance
            .check_callback_route()
            .map_err(|error| ParameterErrorKind::Native {
                operation: "VST2 callback admission",
                source: Box::new(error),
            })?;
        // SAFETY: the loaded instance retains its AEffect through this exclusive
        // synchronous preparation; descriptor reads do not call the plugin.
        let descriptor = unsafe { &*instance.effect };
        let setter = descriptor.set_parameter;
        if matches!(requirement, SetterRequirement::Required) && setter.is_none() {
            return Err(ParameterErrorKind::MissingCallback);
        }
        let dispatcher = descriptor
            .dispatcher
            .ok_or(ParameterErrorKind::MissingCallback)?;
        let count = descriptor.num_params;
        if count < 0 || count != instance.num_params || count > MAX_REPORTED_PARAMS {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        let revision = instance
            .host
            .authored_inventory_revision
            .load(Ordering::Acquire);
        let mut owner = Self {
            instance,
            identity: Arc::new(()),
            names: HashMap::new(),
            compact_names: HashMap::new(),
            count,
            setter,
            dispatcher,
            revision,
        };
        owner.check_parameter_authority()?;
        for index in 0..count {
            let name = owner
                .instance
                .query_indexed_string(op::GET_PARAM_NAME, index);
            owner.check_parameter_authority()?;
            let name = if name.is_empty() {
                format!("Param {index}")
            } else {
                name
            };
            for (names, key) in [
                (&mut owner.names, name.trim().to_ascii_lowercase()),
                (
                    &mut owner.compact_names,
                    normalized_param_chars(&name).collect::<String>(),
                ),
            ] {
                // Empty legacy compaction must not alias unrelated Unicode names.
                if key.is_empty() {
                    continue;
                }
                let ids = names.entry(key).or_default();
                if !ids.contains(&index) {
                    ids.push(index);
                }
            }
        }
        owner.check_parameter_authority()?;
        Ok(owner)
    }
    pub(super) fn apply_claimed_parameters(
        &mut self,
        rows: &[Vst2AuthoredInput],
    ) -> Result<(), crate::preset_parameters::ParameterApplyError> {
        use crate::preset_parameters::{
            OwnedAuthoredParameter, ParameterApplyError, classify_host_parameter,
        };
        let fail = |name: &str, kind| ParameterApplyError {
            format: crate::PluginFormat::Vst2,
            parameter: name.into(),
            kind,
        };
        let mut native = Vec::with_capacity(rows.len());
        for row in rows {
            self.check_parameter_authority()
                .map_err(|kind| fail(&row.name, kind))?;
            if classify_host_parameter(crate::PluginFormat::Vst2, &row.name)
                .map_err(|kind| fail(&row.name, kind))?
                .is_some()
            {
                return Err(fail(&row.name, ParameterErrorKind::ReservedNameCollision));
            }
            let matches_native = match self.resolve(&row.name) {
                Ok(_) => true,
                Err(ParameterErrorKind::Unknown) => false,
                Err(kind) => return Err(fail(&row.name, kind)),
            };
            match (matches_native, row.claim) {
                (true, Vst2AuthoredClaim::InstrumentOutputRouting) => {
                    return Err(fail(&row.name, ParameterErrorKind::Ambiguous));
                }
                (false, Vst2AuthoredClaim::NativeOnly) => {
                    return Err(fail(&row.name, ParameterErrorKind::Unknown));
                }
                (false, Vst2AuthoredClaim::InstrumentOutputRouting) => {}
                (true, Vst2AuthoredClaim::NativeOnly) => native.push(OwnedAuthoredParameter {
                    name: row.name.clone(),
                    value: row.native_value,
                    domain: row.domain,
                }),
            }
        }
        self.check_parameter_authority()
            .map_err(|kind| fail("<batch>", kind))?;
        if native.is_empty() {
            return Ok(());
        }
        if self.setter.is_none() {
            return Err(fail("<batch>", ParameterErrorKind::MissingCallback));
        }
        self.apply_owned_authored_parameters(&native)
    }
    fn bound_index(&self, key: &Vst2AuthoredParameterKey) -> Result<i32, ParameterErrorKind> {
        if !Arc::ptr_eq(&key.authority, &self.identity) || !(0..self.count).contains(&key.index) {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        Ok(key.index)
    }
    fn resolve(&self, name: &str) -> Result<i32, ParameterErrorKind> {
        // Union explicit #index with exact and existing compacted-name aliases.
        // Neither index spelling nor an exact match may override a collision.
        let mut candidate = name
            .trim()
            .strip_prefix('#')
            .and_then(|index| index.parse::<i32>().ok())
            .filter(|index| (0..self.count).contains(index));
        let exact = name.trim().to_ascii_lowercase();
        let compact = normalized_param_chars(name).collect::<String>();
        for (names, key) in [(&self.names, &exact), (&self.compact_names, &compact)] {
            if key.is_empty() {
                continue;
            }
            if let Some(ids) = names.get(key) {
                for index in ids {
                    if candidate.is_some_and(|existing| existing != *index) {
                        return Err(ParameterErrorKind::Ambiguous);
                    }
                    candidate = Some(*index);
                }
            }
        }
        candidate.ok_or(ParameterErrorKind::Unknown)
    }
}
impl AuthoredParameterOwner for Vst2AuthoredParameterOwner<'_> {
    type Key = Vst2AuthoredParameterKey;
    fn parameter_format(&self) -> crate::PluginFormat {
        crate::PluginFormat::Vst2
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        self.instance
            .check_callback_route()
            .map_err(|error| ParameterErrorKind::Native {
                operation: "VST2 callback admission",
                source: Box::new(error),
            })?;
        if let Some(error) = self.instance.process_error {
            return Err(ParameterErrorKind::Native {
                operation: "VST2 prepared instance",
                source: Box::new(error),
            });
        }
        if self.instance.host.io_changed.load(Ordering::Acquire)
            || self
                .instance
                .host
                .authored_inventory_revision
                .load(Ordering::Acquire)
                != self.revision
        {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        // SAFETY: the exclusive owner keeps the AEffect descriptor live. This
        // validates callback availability/identity before each prepared write.
        let descriptor = unsafe { &*self.instance.effect };
        let setter = descriptor.set_parameter;
        if self.setter.is_some() && setter.is_none() {
            return Err(ParameterErrorKind::MissingCallback);
        }
        let dispatcher = descriptor
            .dispatcher
            .ok_or(ParameterErrorKind::MissingCallback)?;
        if descriptor.num_params != self.count
            || setter.map(|setter| setter as usize) != self.setter.map(|setter| setter as usize)
            || dispatcher as usize != self.dispatcher as usize
        {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        Ok(())
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<Self::Key>, ParameterErrorKind> {
        self.check_parameter_authority()?;
        let index = self.resolve(name)?;
        Ok(ResolvedParameter {
            key: Vst2AuthoredParameterKey {
                index,
                authority: Arc::clone(&self.identity),
            },
            writable: true,
            plain_range: None,
        })
    }
    fn prepare_bound_value(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        self.check_parameter_authority()?;
        self.bound_index(key)?;
        if domain != ParameterWriteDomain::Normalized {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
        let native = checked_normalized_value(value)?;
        Ok(PreparedParameterValue {
            value: f64::from(native),
            domain,
        })
    }
    fn write_bound_parameter(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        let prepared = self.prepare_bound_value(key, value, domain)?;
        let index = self.bound_index(key)?;
        let value = prepared.value as f32;
        // SAFETY: immediate numeric/authority validation established a live
        // native index and unchanged present callback on the exclusive
        // control owner. VST2's setter is void; success means actual invocation.
        // Its existing foreign-exception boundary is outside this adapter slice.
        self.instance.write_parameter_callback(
            self.setter.ok_or(ParameterErrorKind::MissingCallback)?,
            index,
            value,
        );
        self.check_parameter_authority()?;
        self.instance.host.store_value(index, value);
        Ok(())
    }
}

/// Pure normalized conversion shared by static preparation and numeric runtime points.
pub(super) fn checked_normalized_value(value: f64) -> Result<f32, ParameterErrorKind> {
    if !value.is_finite() {
        return Err(ParameterErrorKind::NonFinite);
    }
    if !(0.0..=1.0).contains(&value) {
        return Err(ParameterErrorKind::OutOfRange {
            value,
            min: 0.0,
            max: 1.0,
        });
    }
    let native = value as f32;
    if !native.is_finite() {
        return Err(ParameterErrorKind::Unrepresentable);
    }
    Ok(native)
}
pub(super) struct RuntimeBindings {
    pub indices: Vec<i32>,
    pub setter: Option<abi::SetParameterProc>,
    pub dispatcher: abi::DispatcherProc,
    pub count: i32,
}
pub(super) fn runtime_bindings(
    instance: &mut Vst2Instance,
    names: &[String],
) -> Result<RuntimeBindings, crate::preset_parameters::ParameterApplyError> {
    use crate::preset_parameters::{ParameterApplyError, classify_host_parameter};
    let fail = |name: &str, kind| ParameterApplyError {
        format: crate::PluginFormat::Vst2,
        parameter: name.into(),
        kind,
    };
    if names.is_empty() {
        instance.check_callback_route().map_err(|error| {
            fail(
                "<runtime>",
                ParameterErrorKind::Native {
                    operation: "VST2 runtime callback admission",
                    source: Box::new(error),
                },
            )
        })?;
        // SAFETY: UI preparation retains the healthy candidate and takes only
        // scalar/callback copies; no metadata borrow crosses a foreign call.
        let descriptor = unsafe { &*instance.effect };
        if descriptor.num_params != instance.num_params
            || !(0..=MAX_REPORTED_PARAMS).contains(&descriptor.num_params)
        {
            return Err(fail("<runtime>", ParameterErrorKind::InvalidMetadata));
        }
        return Ok(RuntimeBindings {
            indices: Vec::new(),
            setter: descriptor.set_parameter,
            dispatcher: descriptor
                .dispatcher
                .ok_or_else(|| fail("<runtime>", ParameterErrorKind::MissingCallback))?,
            count: descriptor.num_params,
        });
    }
    let mut owner =
        Vst2AuthoredParameterOwner::new(instance).map_err(|kind| fail("<runtime>", kind))?;
    let mut indices = Vec::with_capacity(names.len());
    for name in names {
        owner
            .check_parameter_authority()
            .map_err(|kind| fail(name, kind))?;
        if classify_host_parameter(crate::PluginFormat::Vst2, name)
            .map_err(|kind| fail(name, kind))?
            .is_some()
        {
            return Err(fail(name, ParameterErrorKind::ReservedNameCollision));
        }
        indices.push(owner.resolve(name).map_err(|kind| fail(name, kind))?);
    }
    owner
        .check_parameter_authority()
        .map_err(|kind| fail("<runtime>", kind))?;
    Ok(RuntimeBindings {
        indices,
        setter: owner.setter,
        dispatcher: owner.dispatcher,
        count: owner.count,
    })
}

#[cfg(test)]
mod tests;
