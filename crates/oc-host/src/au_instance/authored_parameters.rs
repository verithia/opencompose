//! Checked static AU writes use a complete native scope/element snapshot.
use super::*;
use crate::preset_parameters::{
    AuthoredParameterOwner, ParameterErrorKind, ParameterRange, ParameterWriteDomain,
    PreparedParameterValue, ResolvedParameter,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const PROP_ELEMENT_COUNT: u32 = 11;
const SCOPE_NOTE: u32 = 5;
const INVALID_PROPERTY: i32 = -10879;
const INVALID_SCOPE: i32 = -10866;
const MAX_ELEMENTS: u32 = 256;
const MAX_PARAMETERS: usize = 4096;
const MAX_NATIVE_QUERIES: usize = 65_536;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct ParameterDispatch {
    property_info: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32,
    property: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32,
    set: unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32,
}

#[derive(Debug, Clone, PartialEq)]
struct NativeParameter {
    id: u32,
    scope: u32,
    // A Note-scope target needs an active native NoteID, not a static element.
    element: Option<u32>,
    name: String,
    min: f64,
    max: f64,
    unit: u32,
    writable: bool,
    min_bits: u32,
    max_bits: u32,
    default_bits: u32,
    flags: u32,
    clump: u32,
    unit_name: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ScopeInventory {
    scope: u32,
    ids: Option<Vec<u32>>,
    elements: Option<u32>,
}

#[derive(Clone, Debug)]
/// Identity of an authored AU parameter.
pub struct AuAuthoredParameterKey {
    index: usize,
    authority: Arc<()>,
}
impl PartialEq for AuAuthoredParameterKey {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.authority, &other.authority)
    }
}
impl Eq for AuAuthoredParameterKey {}
impl Hash for AuAuthoredParameterKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        Arc::as_ptr(&self.authority).hash(state);
    }
}

/// Exclusive control preparation; it does not activate, render or queue events.
/// Native targets and parameter values remain in the AU's engineering units.
pub struct AuAuthoredParameterOwner<'a> {
    unit: *mut c_void,
    error: &'a mut Option<AuProcessError>,
    dispatch: ParameterDispatch,
    foreign: Arc<au_guard::Boundary>,
    _api: MutexGuard<'static, ()>,
    identity: Arc<()>,
    inventory: Vec<NativeParameter>,
    names: HashMap<String, Vec<usize>>,
    compact: HashMap<String, Vec<usize>>,
    scopes: Vec<ScopeInventory>,
    queries: std::cell::Cell<usize>,
    verification_cost: usize,
    prepared_values: usize,
    resolutions: usize,
    needs_recheck: bool,
    inventory_changed: bool,
    snapshot_bytes: usize,
    total_snapshot_bytes: std::cell::Cell<usize>,
}

fn native_error(operation: &'static str, status: i32) -> ParameterErrorKind {
    ParameterErrorKind::Native {
        operation,
        source: Box::new(AuProcessError::Native { operation, status }),
    }
}
pub(super) fn parameter_list_len(bytes: u32) -> Option<usize> {
    if !bytes.is_multiple_of(size_of::<u32>() as u32) {
        return None;
    }
    let count = bytes as usize / size_of::<u32>();
    (count <= MAX_PARAMETERS).then_some(count)
}

fn absent(status: i32) -> bool {
    matches!(status, INVALID_PROPERTY | INVALID_SCOPE)
}
fn compact(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

impl AuInstance {
    /// Borrow the authored parameter owner.
    pub fn authored_parameter_owner(
        &mut self,
    ) -> Result<AuAuthoredParameterOwner<'_>, ParameterErrorKind> {
        if !self.initialized {
            return Err(ParameterErrorKind::Native {
                operation: "AU authored parameters",
                source: Box::new(AuProcessError::Uninitialized),
            });
        }
        // SAFETY: the exclusive instance borrow retains the unit until the
        // returned owner ends; dispatch belongs to the same guarded live AudioUnit.
        unsafe {
            AuAuthoredParameterOwner::prepare(
                self.unit,
                &mut self.process_error,
                ParameterDispatch {
                    property_info: self.foreign.api.property_info,
                    property: self.foreign.api.get_property,
                    set: self.foreign.api.set_parameter,
                },
                self.foreign.clone(),
            )
        }
    }
}

impl<'a> AuAuthoredParameterOwner<'a> {
    /// The handle must remain live with matching dispatch until the borrow ends.
    unsafe fn prepare(
        unit: *mut c_void,
        error: &'a mut Option<AuProcessError>,
        dispatch: ParameterDispatch,
        foreign: Arc<au_guard::Boundary>,
    ) -> Result<Self, ParameterErrorKind> {
        let mut owner = Self {
            unit,
            error,
            dispatch,
            foreign,
            _api: api_lock(),
            identity: Arc::new(()),
            inventory: Vec::new(),
            names: HashMap::new(),
            compact: HashMap::new(),
            scopes: Vec::new(),
            queries: std::cell::Cell::new(0),
            verification_cost: 0,
            prepared_values: 0,
            resolutions: 0,
            needs_recheck: false,
            inventory_changed: false,
            snapshot_bytes: 0,
            total_snapshot_bytes: std::cell::Cell::new(0),
        };
        owner.check_parameter_authority()?;
        (owner.inventory, owner.scopes) = owner.read_inventory()?;
        owner.verification_cost = owner.queries.get();
        owner.snapshot_bytes = owner.total_snapshot_bytes.get();
        for (index, parameter) in owner.inventory.iter().enumerate() {
            for (map, name) in [
                (&mut owner.names, parameter.name.trim().to_ascii_lowercase()),
                (&mut owner.compact, compact(&parameter.name)),
            ] {
                if !name.is_empty() {
                    map.entry(name).or_default().push(index);
                }
            }
        }
        Ok(owner)
    }

    fn foreign_error(&self, error: AuForeignError) -> ParameterErrorKind {
        foreign_error(&self.foreign, error)
    }

    fn reserve_query(&self) -> Result<(), ParameterErrorKind> {
        self.foreign
            .check()
            .map_err(|error| self.foreign_error(error))?;
        let next = self
            .queries
            .get()
            .checked_add(1)
            .ok_or(ParameterErrorKind::InvalidMetadata)?;
        if next > MAX_NATIVE_QUERIES {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        self.queries.set(next);
        Ok(())
    }

    fn property<T>(
        &self,
        property: u32,
        scope: u32,
        element: u32,
        output: &mut T,
    ) -> Result<(), ParameterErrorKind> {
        self.reserve_query()?;
        let expected = size_of::<T>() as u32;
        let mut size = expected;
        // SAFETY: the control guard and borrow retain the native unit; output
        // has exactly the advertised writable storage for this property type.
        let status = unsafe {
            self.foreign
                .get_property_with(
                    self.dispatch.property,
                    au_guard::GetProperty {
                        unit: self.unit,
                        property,
                        scope,
                        element,
                        data: (output as *mut T).cast(),
                        size: &mut size,
                    },
                )
                .map_err(|error| self.foreign_error(error))?
        };
        if status != 0 {
            return Err(native_error("AudioUnitGetProperty", status));
        }
        if size != expected {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        Ok(())
    }

    fn ids(&self, scope: u32) -> Result<Option<Vec<u32>>, ParameterErrorKind> {
        self.reserve_query()?;
        let (mut bytes, mut writable) = (0, 0);
        // SAFETY: output pointers are live scalars and discovery does not alter
        // native state; the AU API guard covers the matching dispatch call.
        let status = unsafe {
            self.foreign
                .property_info_with(
                    self.dispatch.property_info,
                    au_guard::PropertyInfo {
                        unit: self.unit,
                        property: PROP_PARAMETER_LIST,
                        scope,
                        element: 0,
                        size: &mut bytes,
                        writable: &mut writable,
                    },
                )
                .map_err(|error| self.foreign_error(error))?
        };
        if absent(status) {
            return Ok(None);
        }
        if status != 0 {
            return Err(native_error("AudioUnitGetPropertyInfo", status));
        }
        let count = parameter_list_len(bytes).ok_or(ParameterErrorKind::InvalidMetadata)?;
        if bytes == 0 {
            return Ok(Some(Vec::new()));
        }
        self.reserve_query()?;
        let expected = bytes;
        let mut ids = vec![0u32; count];
        // SAFETY: the bounded property size equals ids' allocated byte capacity.
        let status = unsafe {
            self.foreign
                .get_property_with(
                    self.dispatch.property,
                    au_guard::GetProperty {
                        unit: self.unit,
                        property: PROP_PARAMETER_LIST,
                        scope,
                        element: 0,
                        data: ids.as_mut_ptr().cast(),
                        size: &mut bytes,
                    },
                )
                .map_err(|error| self.foreign_error(error))?
        };
        if status != 0 {
            return Err(native_error("AudioUnitGetProperty", status));
        }
        if bytes != expected {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        Ok(Some(ids))
    }

    fn read_inventory(
        &self,
    ) -> Result<(Vec<NativeParameter>, Vec<ScopeInventory>), ParameterErrorKind> {
        let mut inventory = Vec::new();
        let mut scopes = Vec::new();
        let mut allocation_bytes = 0usize;
        // These are the SDK's defined scopes, not a probe of reserved scope IDs.
        for scope in 0..=7 {
            let ids = self.ids(scope)?;
            let Some(ids) = ids else {
                scopes.push(ScopeInventory {
                    scope,
                    ids: None,
                    elements: None,
                });
                continue;
            };
            let elements = if scope == SCOPE_GLOBAL {
                Some(1)
            } else if scope == SCOPE_NOTE {
                None
            } else {
                self.reserve_query()?;
                let mut count = 0u32;
                let mut size = size_of::<u32>() as u32;
                // SAFETY: this discovery query supplies exactly one live u32;
                // non-global static element identities are native indexed buses.
                let status = unsafe {
                    self.foreign
                        .get_property_with(
                            self.dispatch.property,
                            au_guard::GetProperty {
                                unit: self.unit,
                                property: PROP_ELEMENT_COUNT,
                                scope,
                                element: 0,
                                data: (&mut count as *mut u32).cast(),
                                size: &mut size,
                            },
                        )
                        .map_err(|error| self.foreign_error(error))?
                };
                if absent(status) && ids.is_empty() {
                    None
                } else {
                    if status != 0 {
                        return Err(native_error("AudioUnitGetProperty", status));
                    }
                    if size != size_of::<u32>() as u32 || count > MAX_ELEMENTS {
                        return Err(ParameterErrorKind::InvalidMetadata);
                    }
                    Some(count)
                }
            };
            let target_count = elements.unwrap_or(1) as usize;
            if inventory
                .len()
                .checked_add(
                    ids.len()
                        .checked_mul(target_count)
                        .ok_or(ParameterErrorKind::InvalidMetadata)?,
                )
                .is_none_or(|total| total > MAX_PARAMETERS)
            {
                return Err(ParameterErrorKind::InvalidMetadata);
            }
            for &id in &ids {
                // ParameterInfo's element selector is the parameter ID; it is
                // not the input/output bus or a dynamic note element.
                // SAFETY: the info record is a plain C layout with nullable refs.
                let mut info: AudioUnitParameterInfo = unsafe { std::mem::zeroed() };
                self.property(PROP_PARAMETER_INFO, scope, id, &mut info)?;
                let (name, unit_name) = parameter_names(&info, &self.foreign)?;
                if !info.min_value.is_finite()
                    || !info.max_value.is_finite()
                    || !info.default_value.is_finite()
                    || info.min_value > info.max_value
                {
                    return Err(ParameterErrorKind::InvalidMetadata);
                }
                let bytes_per_target = size_of::<NativeParameter>()
                    .checked_add(name.len())
                    .and_then(|n| n.checked_add(unit_name.len()))
                    .ok_or(ParameterErrorKind::InvalidMetadata)?;
                allocation_bytes = allocation_bytes
                    .checked_add(
                        bytes_per_target
                            .checked_mul(target_count)
                            .ok_or(ParameterErrorKind::InvalidMetadata)?,
                    )
                    .ok_or(ParameterErrorKind::InvalidMetadata)?;
                if allocation_bytes > MAX_SNAPSHOT_BYTES {
                    return Err(ParameterErrorKind::InvalidMetadata);
                }
                let total = self
                    .total_snapshot_bytes
                    .get()
                    .checked_add(
                        bytes_per_target
                            .checked_mul(target_count)
                            .ok_or(ParameterErrorKind::InvalidMetadata)?,
                    )
                    .ok_or(ParameterErrorKind::InvalidMetadata)?;
                if total > MAX_TOTAL_SNAPSHOT_BYTES {
                    return Err(ParameterErrorKind::InvalidMetadata);
                }
                self.total_snapshot_bytes.set(total);
                for element in 0..target_count {
                    inventory.push(NativeParameter {
                        id,
                        scope,
                        element: elements.map(|_| element as u32),
                        name: name.clone(),
                        min: f64::from(info.min_value),
                        max: f64::from(info.max_value),
                        unit: info.unit,
                        writable: info.flags & PARAM_FLAG_IS_WRITABLE != 0,
                        min_bits: info.min_value.to_bits(),
                        max_bits: info.max_value.to_bits(),
                        default_bits: info.default_value.to_bits(),
                        flags: info.flags,
                        clump: info.clump_id,
                        unit_name: unit_name.clone(),
                    });
                }
            }
            scopes.push(ScopeInventory {
                scope,
                ids: Some(ids),
                elements,
            });
        }
        Ok((inventory, scopes))
    }

    fn bound(&self, key: &AuAuthoredParameterKey) -> Result<&NativeParameter, ParameterErrorKind> {
        if !Arc::ptr_eq(&key.authority, &self.identity) {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        self.inventory
            .get(key.index)
            .ok_or(ParameterErrorKind::InventoryChanged)
    }
}

fn foreign_error(foreign: &au_guard::Boundary, error: AuForeignError) -> ParameterErrorKind {
    let source: Box<dyn std::error::Error + Send + Sync> = match foreign.fault() {
        Some(fault) => Box::new(fault),
        None => Box::new(error),
    };
    ParameterErrorKind::Native {
        operation: error.operation,
        source,
    }
}

pub(super) fn parameter_names(
    info: &AudioUnitParameterInfo,
    foreign: &au_guard::Boundary,
) -> Result<(String, String), ParameterErrorKind> {
    let release = info.flags & PARAM_FLAG_CF_NAME_RELEASE != 0;
    let name_ref = CfOwned::new(
        if release && info.flags & PARAM_FLAG_HAS_CF_NAME_STRING != 0 {
            info.cf_name_string
        } else {
            ptr::null_mut()
        },
        foreign,
    );
    let unit_ref = CfOwned::new(
        if release && info.unit == 26 {
            info.unit_name
        } else {
            ptr::null_mut()
        },
        foreign,
    );
    let convert = |text: *mut c_void| -> Result<String, ParameterErrorKind> {
        if text.is_null() {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        // SAFETY: the descriptor/native reference guards retain each name through the bounded decode.
        unsafe {
            cf_string_to_string_guarded(
                text,
                MAX_CF_STRING_UTF16_UNITS,
                MAX_CF_STRING_UTF8_BYTES,
                foreign,
            )
        }
        .map_err(|error| foreign_error(foreign, error))?
        .ok_or(ParameterErrorKind::InvalidMetadata)
    };
    let result = (|| {
        let name = if info.flags & PARAM_FLAG_HAS_CF_NAME_STRING != 0 {
            convert(info.cf_name_string)?
        } else {
            let length = info
                .name
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(info.name.len());
            let bytes: Vec<u8> = info.name[..length].iter().map(|ch| *ch as u8).collect();
            String::from_utf8(bytes).map_err(|_| ParameterErrorKind::InvalidMetadata)?
        };
        let unit_name = if info.unit == 26 {
            convert(info.unit_name)?
        } else {
            String::new()
        };
        Ok((name, unit_name))
    })();
    // Finalization may execute foreign code. Check it before publishing metadata;
    // the first exception gates the second release and every later native query.
    name_ref
        .close()
        .map_err(|error| foreign_error(foreign, error))?;
    unit_ref
        .close()
        .map_err(|error| foreign_error(foreign, error))?;
    result
}

fn checked_value(parameter: &NativeParameter, value: f64) -> Result<f64, ParameterErrorKind> {
    if !parameter.writable {
        return Err(ParameterErrorKind::ReadOnly);
    }
    if !value.is_finite() {
        return Err(ParameterErrorKind::NonFinite);
    }
    if value < parameter.min || value > parameter.max {
        return Err(ParameterErrorKind::OutOfRange {
            value,
            min: parameter.min,
            max: parameter.max,
        });
    }
    let native = value as f32;
    if !native.is_finite() {
        return Err(ParameterErrorKind::Unrepresentable);
    }
    let converted = f64::from(native);
    if converted < parameter.min || converted > parameter.max {
        return Err(ParameterErrorKind::OutOfRange {
            value: converted,
            min: parameter.min,
            max: parameter.max,
        });
    }
    Ok(converted)
}

impl AuthoredParameterOwner for AuAuthoredParameterOwner<'_> {
    type Key = AuAuthoredParameterKey;
    fn parameter_format(&self) -> crate::PluginFormat {
        crate::PluginFormat::Au
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        self.foreign
            .check()
            .map_err(|error| self.foreign_error(error))?;
        if let Some(error) = *self.error {
            return Err(ParameterErrorKind::Native {
                operation: "AU authored parameters",
                source: Box::new(error),
            });
        }
        if self.inventory_changed {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        if self.needs_recheck {
            let (parameters, scopes) = self.read_inventory()?;
            if parameters != self.inventory || scopes != self.scopes {
                self.inventory_changed = true;
                return Err(ParameterErrorKind::InventoryChanged);
            }
            self.needs_recheck = false;
        }
        Ok(())
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<Self::Key>, ParameterErrorKind> {
        self.check_parameter_authority()?;
        self.resolutions = self
            .resolutions
            .checked_add(1)
            .ok_or(ParameterErrorKind::InvalidMetadata)?;
        if self.resolutions > MAX_NATIVE_QUERIES {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        if name.len() > MAX_CF_STRING_UTF16_UNITS * 4 {
            return Err(ParameterErrorKind::Unknown);
        }
        let mut candidates = self
            .names
            .get(&name.trim().to_ascii_lowercase())
            .into_iter()
            .flatten()
            .chain(self.compact.get(&compact(name)).into_iter().flatten())
            .copied();
        let index = candidates.next().ok_or(ParameterErrorKind::Unknown)?;
        if candidates.any(|candidate| candidate != index) {
            return Err(ParameterErrorKind::Ambiguous);
        }
        let parameter = &self.inventory[index];
        if parameter.element.is_none() {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
        Ok(ResolvedParameter {
            key: AuAuthoredParameterKey {
                index,
                authority: self.identity.clone(),
            },
            writable: parameter.writable,
            plain_range: Some(ParameterRange {
                min: parameter.min,
                max: parameter.max,
            }),
        })
    }
    fn prepare_bound_value(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        self.check_parameter_authority()?;
        // Each effective prepared value reserves one complete post-write
        // descriptor verification before the batch can issue its first write.
        self.prepared_values = self
            .prepared_values
            .checked_add(1)
            .ok_or(ParameterErrorKind::InvalidMetadata)?;
        let reserved = self
            .verification_cost
            .checked_add(1)
            .and_then(|cost| cost.checked_mul(self.prepared_values))
            .and_then(|cost| cost.checked_add(self.queries.get()))
            .ok_or(ParameterErrorKind::InvalidMetadata)?;
        if reserved > MAX_NATIVE_QUERIES {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        if self
            .snapshot_bytes
            .checked_mul(self.prepared_values)
            .and_then(|bytes| bytes.checked_add(self.total_snapshot_bytes.get()))
            .is_none_or(|bytes| bytes > MAX_TOTAL_SNAPSHOT_BYTES)
        {
            return Err(ParameterErrorKind::InvalidMetadata);
        }
        let parameter = self.bound(key)?;
        if parameter.element.is_none() || domain != ParameterWriteDomain::NativePlain {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
        Ok(PreparedParameterValue {
            value: checked_value(parameter, value)?,
            domain,
        })
    }
    fn write_bound_parameter(
        &mut self,
        key: &Self::Key,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        self.check_parameter_authority()?;
        let parameter = self.bound(key)?;
        let element = parameter
            .element
            .ok_or(ParameterErrorKind::UnsupportedDomain)?;
        if domain != ParameterWriteDomain::NativePlain {
            return Err(ParameterErrorKind::UnsupportedDomain);
        }
        let value = checked_value(parameter, value)? as f32;
        self.reserve_query()?;
        // SAFETY: the checked key binds one retained unit/scope/element/ID, and
        // the actual f32 value passes the same checks as batch preflight.
        let status = unsafe {
            self.foreign
                .set_parameter_with(
                    self.dispatch.set,
                    au_guard::SetParameter {
                        unit: self.unit,
                        parameter: parameter.id,
                        scope: parameter.scope,
                        element,
                        value,
                        offset: 0,
                    },
                )
                .map_err(|error| self.foreign_error(error))?
        };
        self.needs_recheck = true;
        if status != 0 {
            let error = AuProcessError::Native {
                operation: "AudioUnitSetParameter",
                status,
            };
            if self.error.is_none() {
                *self.error = Some(error);
            }
            return Err(native_error("AudioUnitSetParameter", status));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
