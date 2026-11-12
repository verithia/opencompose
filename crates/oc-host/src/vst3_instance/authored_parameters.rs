//! Additive control-owner backend for authored VST3 static parameters.
use super::*;
use crate::preset_parameters::{
    AuthoredParameterOwner, ParameterErrorKind, ParameterRange, ParameterWriteDomain,
    PreparedParameterValue, ResolvedParameter,
};
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
/// Identity of an authored VST3 parameter.
pub struct Vst3AuthoredParameterKey {
    id: ParamID,
    authority: Arc<()>,
}
impl PartialEq for Vst3AuthoredParameterKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.authority, &other.authority)
    }
}
impl Eq for Vst3AuthoredParameterKey {}
impl Hash for Vst3AuthoredParameterKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        Arc::as_ptr(&self.authority).hash(state);
    }
}

/// A native refusal is not a C++ exception and does not poison an otherwise
/// healthy plugin. Its exact result remains available through Error::source.
#[derive(Debug, Clone, Copy)]
pub struct Vst3ParameterRefusal {
    pub operation: Operation,
    pub result: tresult,
}
impl std::fmt::Display for Vst3ParameterRefusal {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "VST3 {} refused (0x{:x})",
            self.operation.label(),
            self.result
        )
    }
}
impl std::error::Error for Vst3ParameterRefusal {}

#[derive(Default)]
struct ParameterInventory {
    primary_names: HashMap<String, Vec<ParamID>>,
    legacy_names: HashMap<String, Vec<ParamID>>,
    writable: HashMap<ParamID, bool>,
    automatable: HashMap<ParamID, bool>,
}
impl ParameterInventory {
    #[cfg(test)]
    fn add(&mut self, info: &ParameterInfo) {
        self.add_values(info.id, &char16_to_string(&info.title), info.flags);
    }
    fn add_values(&mut self, id: ParamID, title: &str, flags: i32) {
        let automatable = flags & ParameterFlags_::kCanAutomate != 0;
        self.automatable
            .entry(id)
            .and_modify(|v| *v &= automatable)
            .or_insert(automatable);
        let writable = flags & ParameterFlags_::kIsReadOnly == 0;
        self.writable
            .entry(id)
            .and_modify(|existing| *existing &= writable)
            .or_insert(writable);
        for (names, key) in [
            (&mut self.primary_names, unicode_parameter_name(title)),
            (&mut self.legacy_names, normalize_param_name(title)),
        ] {
            // The existing ASCII normalizer maps all-non-ASCII names to empty.
            // Empty aliases must not merge unrelated native parameters.
            if key.is_empty() {
                continue;
            }
            let ids = names.entry(key).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    fn resolve(&self, name: &str) -> Result<ParamID, ParameterErrorKind> {
        // Checked names admit Unicode titles while retaining the exact existing
        // ASCII-compacted persisted aliases. Unlike the legacy realtime lookup,
        // this path unions both candidate sets and refuses every different-ID
        // collision; an exact Unicode match never overrides an ambiguous alias.
        let primary = unicode_parameter_name(name);
        let legacy = normalize_param_name(name);
        let mut candidate = None;
        for (names, key) in [
            (&self.primary_names, &primary),
            (&self.legacy_names, &legacy),
        ] {
            if key.is_empty() {
                continue;
            }
            if let Some(ids) = names.get(key) {
                for id in ids {
                    if candidate.is_some_and(|existing| existing != *id) {
                        return Err(ParameterErrorKind::Ambiguous);
                    }
                    candidate = Some(*id);
                }
            }
        }
        candidate.ok_or(ParameterErrorKind::Unknown)
    }
}
fn unicode_parameter_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// One synchronous UI-thread preparation. Keys carry a private live authority
/// identity; snapshots never refresh halfway through a preflight/write batch.
pub struct Vst3AuthoredParameterOwner<'a> {
    instance: &'a mut Vst3Instance,
    identity: Arc<()>,
    inventory: ParameterInventory,
    ranges: HashMap<ParamID, Option<ParameterRange>>,
    local_revision: u64,
    host_revision: u64,
    refusal: Option<Vst3ParameterRefusal>,
    snapshot: owned_lifecycle::Vst3OwnedSnapshot,
    needs_recheck: bool,
}
fn native_error(error: Vst3ProcessError) -> ParameterErrorKind {
    ParameterErrorKind::Native {
        operation: error.operation.label(),
        source: Box::new(error),
    }
}
fn refusal_error(error: Vst3ParameterRefusal) -> ParameterErrorKind {
    ParameterErrorKind::Native {
        operation: error.operation.label(),
        source: Box::new(error),
    }
}
fn ui_authority() -> Result<(), ParameterErrorKind> {
    crate::application_main::ui_authority()
        .map(|_| ())
        .map_err(|error| ParameterErrorKind::Native {
            operation: "controller UI authority",
            source: Box::new(error),
        })
}
fn metadata_error(error: owned_lifecycle::Vst3OwnedError) -> ParameterErrorKind {
    if matches!(error, owned_lifecycle::Vst3OwnedError::InventoryChanged) {
        return ParameterErrorKind::InventoryChanged;
    }
    if let owned_lifecycle::Vst3OwnedError::Native(error) = error {
        return match error.plugin_result {
            Some(result) => refusal_error(Vst3ParameterRefusal {
                operation: error.operation,
                result,
            }),
            None => native_error(*error),
        };
    }
    ParameterErrorKind::Native {
        operation: "VST3 authored metadata",
        source: Box::new(error),
    }
}
impl Vst3Instance {
    pub fn authored_parameter_owner(
        &mut self,
    ) -> Result<Vst3AuthoredParameterOwner<'_>, ParameterErrorKind> {
        Vst3AuthoredParameterOwner::new(self)
    }
}
impl<'a> Vst3AuthoredParameterOwner<'a> {
    fn new(instance: &'a mut Vst3Instance) -> Result<Self, ParameterErrorKind> {
        ui_authority()?;
        instance.healthy().map_err(native_error)?;
        let snapshot =
            owned_lifecycle::capture_authored_metadata(instance).map_err(metadata_error)?;
        let mut inventory = ParameterInventory::default();
        for info in &snapshot.parameters {
            inventory.add_values(info.id, &info.name, info.flags);
        }
        Ok(Self {
            local_revision: instance.authored_parameter_revision,
            host_revision: instance.parameter_inventory_revision(),
            instance,
            identity: Arc::new(()),
            inventory,
            ranges: HashMap::new(),
            refusal: None,
            snapshot,
            needs_recheck: false,
        })
    }
    /// Runtime automation uses normalized values and must not call native
    /// physical converters. Resolve through the same complete alias inventory.
    pub(super) fn inventory_matches(&self, snapshot: &owned_lifecycle::Vst3OwnedSnapshot) -> bool {
        owned_lifecycle::same_authored_inventory(&self.snapshot, snapshot)
    }
    pub(super) fn normalized_binding(&mut self, name: &str) -> Result<ParamID, ParameterErrorKind> {
        self.check_parameter_authority()?;
        let id = self.inventory.resolve(name)?;
        if !self.inventory.writable[&id] {
            return Err(ParameterErrorKind::ReadOnly);
        }
        if !self.inventory.automatable[&id] {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
        Ok(id)
    }
    pub(super) fn matches_native(&self, name: &str) -> Result<bool, ParameterErrorKind> {
        match self.inventory.resolve(name) {
            Ok(_) => Ok(true),
            Err(ParameterErrorKind::Unknown) => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn controller(&self) -> Result<*mut IEditController, ParameterErrorKind> {
        self.instance
            .controller
            .as_ref()
            .map(|c| c.as_ptr())
            .ok_or(ParameterErrorKind::MissingCallback)
    }
    fn native<T>(
        &self,
        operation: Operation,
        result: Result<T, Fault>,
    ) -> Result<T, ParameterErrorKind> {
        result.map_err(|fault| native_error(self.instance.native_fault(operation, fault)))
    }
    fn bound_id(&self, key: &Vst3AuthoredParameterKey) -> Result<ParamID, ParameterErrorKind> {
        if !Arc::ptr_eq(&key.authority, &self.identity)
            || !self.inventory.writable.contains_key(&key.id)
        {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        Ok(key.id)
    }
    fn plain_range(&mut self, id: ParamID) -> Result<Option<ParameterRange>, ParameterErrorKind> {
        if let Some(range) = self.ranges.get(&id) {
            return Ok(*range);
        }
        let controller = self.controller()?;
        // SAFETY: the adapter owns the UI-authorized controller borrow; these
        // numeric conversions read endpoints without setting parameter values.
        self.needs_recheck = true;
        // SAFETY: The checked UI controller borrow and bound ID remain live during this guarded scalar endpoint conversion.
        let first = self.native(Operation::NormalizedToPlain, unsafe {
            vst3_guard::normalized_to_plain(controller, id, 0.0)
        })?;
        self.check_parameter_authority()?;
        // SAFETY: same live controller and stable bound ID after authority check.
        self.needs_recheck = true;
        // SAFETY: Authority was rechecked; the same retained UI controller and bound ID are valid for this guarded endpoint conversion.
        let last = self.native(Operation::NormalizedToPlain, unsafe {
            vst3_guard::normalized_to_plain(controller, id, 1.0)
        })?;
        self.check_parameter_authority()?;
        let range = if first.is_finite() && last.is_finite() {
            Some(ParameterRange {
                min: first.min(last),
                max: first.max(last),
            })
        } else {
            None
        };
        self.ranges.insert(id, range);
        Ok(range)
    }
}
impl AuthoredParameterOwner for Vst3AuthoredParameterOwner<'_> {
    type Key = Vst3AuthoredParameterKey;
    fn parameter_format(&self) -> crate::PluginFormat {
        crate::PluginFormat::Vst3
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        ui_authority()?;
        self.instance.healthy().map_err(native_error)?;
        if let Some(refusal) = self.refusal {
            return Err(refusal_error(refusal));
        }
        if self.local_revision != self.instance.authored_parameter_revision
            || self.host_revision != self.instance.parameter_inventory_revision()
        {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        if self.needs_recheck {
            let current = owned_lifecycle::capture_authored_metadata(self.instance)
                .map_err(metadata_error)?;
            if !owned_lifecycle::same_authored_inventory(&self.snapshot, &current) {
                return Err(ParameterErrorKind::InventoryChanged);
            }
            self.needs_recheck = false;
        }
        Ok(())
    }
    fn admit_authored_batch(&mut self, assignments: usize) -> Result<(), ParameterErrorKind> {
        owned_lifecycle::admit_authored_verification(self.snapshot.parameters.len(), assignments)
            .map_err(metadata_error)
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<Self::Key>, ParameterErrorKind> {
        self.check_parameter_authority()?;
        let id = self.inventory.resolve(name)?;
        let writable = self.inventory.writable[&id];
        let plain_range = if writable {
            self.plain_range(id)?
        } else {
            None
        };
        Ok(ResolvedParameter {
            key: Vst3AuthoredParameterKey {
                id,
                authority: Arc::clone(&self.identity),
            },
            writable,
            plain_range,
        })
    }
    fn prepare_bound_value(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        self.check_parameter_authority()?;
        let id = self.bound_id(key)?;
        if !self.inventory.writable[&id] {
            return Err(ParameterErrorKind::ReadOnly);
        }
        if !value.is_finite() {
            return Err(ParameterErrorKind::NonFinite);
        }
        if domain == ParameterWriteDomain::NativePlain {
            let range = self
                .plain_range(id)?
                .ok_or(ParameterErrorKind::InvalidMetadata)?;
            if value < range.min || value > range.max {
                return Err(ParameterErrorKind::OutOfRange {
                    value,
                    min: range.min,
                    max: range.max,
                });
            }
        }
        let normalized = match domain {
            ParameterWriteDomain::Normalized => value,
            ParameterWriteDomain::NativePlain => {
                let controller = self.controller()?;
                // SAFETY: UI ownership and private key authority were checked;
                // the native numeric conversion does not write a parameter.
                self.needs_recheck = true;
                // SAFETY: The UI controller borrow and private key authority were checked; this guarded conversion lends no external buffers.
                self.native(Operation::PlainToNormalized, unsafe {
                    vst3_guard::plain_to_normalized(controller, id, value)
                })?
            }
            ParameterWriteDomain::LegacyVst3Auto => {
                return Err(ParameterErrorKind::UnsupportedDomain);
            }
        };
        self.check_parameter_authority()?;
        if !normalized.is_finite() || !(0.0..=1.0).contains(&normalized) {
            return Err(ParameterErrorKind::Unrepresentable);
        }
        Ok(PreparedParameterValue {
            value: normalized,
            domain: ParameterWriteDomain::Normalized,
        })
    }
    fn write_bound_parameter(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        self.check_parameter_authority()?;
        let id = self.bound_id(key)?;
        if !self.inventory.writable[&id] {
            return Err(ParameterErrorKind::ReadOnly);
        }
        if domain != ParameterWriteDomain::Normalized {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
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
        let controller = self.controller()?;
        // SAFETY: this adapter retains the UI-owned controller and the private
        // binding still belongs to this preparation's stable inventory.
        self.needs_recheck = true;
        // SAFETY: The retained UI controller and private binding still belong to this checked inventory during the guarded scalar setter.
        let code = self.native(Operation::SetNormalized, unsafe {
            vst3_guard::set_normalized(controller, id, value)
        })?;
        if code != kResultOk {
            let refusal = Vst3ParameterRefusal {
                operation: Operation::SetNormalized,
                result: code,
            };
            self.refusal = Some(refusal);
            return Err(refusal_error(refusal));
        }
        self.check_parameter_authority()?;
        self.instance.remember_value(id, value);
        self.instance
            .pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id,
                value,
                sample_offset: 0,
            }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: u32, name: &str, flags: i32) -> ParameterInfo {
        // SAFETY: fixed primitive metadata record, initialized before use.
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        info.id = id;
        info.flags = flags;
        for (slot, c) in info.title.iter_mut().zip(name.encode_utf16()) {
            *slot = c;
        }
        info
    }
    #[test]
    fn inventory_uses_native_flags_preserves_aliases_and_rejects_ambiguous_names() {
        let mut inventory = ParameterInventory::default();
        inventory.add(&row(1, "Gain", 0));
        inventory.add(&row(1, "Gain Amount", 16));
        inventory.add(&row(2, "Hidden Depth", 16));
        inventory.add(&row(3, "Read Only", 2));
        inventory.add(&row(4, "Rate Hz", 0));
        inventory.add(&row(5, "Rate-Hz", 0));
        inventory.add(&row(6, "明るさ", 0));
        assert_eq!(inventory.resolve("gain_amount").unwrap(), 1);
        assert!(inventory.writable[&1] && inventory.writable[&2]);
        assert!(!inventory.writable[&3]);
        assert!(matches!(
            inventory.resolve("Rate Hz"),
            Err(ParameterErrorKind::Ambiguous)
        ));
        assert_eq!(inventory.resolve("明るさ").unwrap(), 6);
        assert!(matches!(
            inventory.resolve("unknown"),
            Err(ParameterErrorKind::Unknown)
        ));
        inventory.add(&row(1, "Gain Read Only Alias", 2));
        assert!(!inventory.writable[&1]);
    }
    #[test]
    fn checked_names_preserve_existing_ascii_aliases_without_hiding_collisions() {
        let mut inventory = ParameterInventory::default();
        inventory.add(&row(10, "Über Gain", 0));
        assert_eq!(inventory.resolve("über_gain").unwrap(), 10);
        assert_eq!(
            inventory.resolve("ber_gain").unwrap(),
            10,
            "persisted ASCII-compacted legacy alias remains accepted"
        );
        inventory.add(&row(11, "Ber Gain", 0));
        for name in ["Über Gain", "über_gain", "Ber Gain", "ber_gain"] {
            assert!(
                matches!(inventory.resolve(name), Err(ParameterErrorKind::Ambiguous)),
                "{name}: primary/legacy union must not select a winner"
            );
        }
    }
    #[test]
    fn non_ascii_titles_do_not_share_an_empty_legacy_alias() {
        let mut inventory = ParameterInventory::default();
        inventory.add(&row(20, "明るさ", 0));
        inventory.add(&row(21, "亮度", 0));
        assert_eq!(inventory.resolve("明るさ").unwrap(), 20);
        assert_eq!(inventory.resolve("亮度").unwrap(), 21);
        assert!(!inventory.legacy_names.contains_key(""));
        assert!(matches!(
            inventory.resolve("!!!"),
            Err(ParameterErrorKind::Unknown)
        ));
        assert!(matches!(
            inventory.resolve(""),
            Err(ParameterErrorKind::Unknown)
        ));
    }

    #[test]
    fn checked_owner_refuses_an_unregistered_controller_thread_before_native_metadata() {
        if cfg!(target_os = "macos") && is_main_thread() {
            return;
        }
        let mut fixture = native_fixture::NativeFixture::new(false);
        let error = match fixture.instance_mut().authored_parameter_owner() {
            Err(error) => error,
            Ok(_) => panic!("unregistered UI thread was admitted"),
        };
        assert!(matches!(
            error,
            ParameterErrorKind::Native {
                operation: "controller UI authority",
                ..
            }
        ));
        assert_eq!(fixture.calls(37), 0);
    }

    #[test]
    fn checked_metadata_adapter_preserves_typed_refusal_and_failure_causes() {
        assert!(matches!(
            metadata_error(owned_lifecycle::Vst3OwnedError::InventoryChanged),
            ParameterErrorKind::InventoryChanged
        ));
        let refusal = metadata_error(owned_lifecycle::Vst3OwnedError::Native(Box::new(
            Vst3ProcessError::refused(Operation::ParameterInfo, kResultFalse),
        )));
        let ParameterErrorKind::Native { operation, source } = refusal else {
            panic!("metadata refusal lost its native category");
        };
        assert_eq!(operation, Operation::ParameterInfo.label());
        let refusal = source.downcast_ref::<Vst3ParameterRefusal>().unwrap();
        assert_eq!(refusal.operation, Operation::ParameterInfo);
        assert_eq!(refusal.result, kResultFalse);

        let failure = Vst3ProcessError::host_contract("controlled metadata failure");
        let mapped = metadata_error(owned_lifecycle::Vst3OwnedError::Native(Box::new(failure)));
        let ParameterErrorKind::Native { source, .. } = mapped else {
            panic!("metadata failure lost its native category");
        };
        assert_eq!(source.downcast_ref::<Vst3ProcessError>(), Some(&failure));
    }
}
