//! Static authored writes use a complete preparation snapshot and checked native
//! flushes; realtime event setters keep their independent normalized contract.

use super::*;
use crate::preset_parameters::{
    AuthoredParameter, AuthoredParameterOwner, ParameterApplyError, ParameterErrorKind,
    ParameterRange, ParameterWriteDomain, PreparedParameterValue, ResolvedParameter,
};
use std::sync::atomic::Ordering;

/// The original immutable CLAP failure remains available through the typed
/// parameter error source, including its native operation and fixed payload.
#[derive(Debug)]
pub enum ClapAuthoredParameterFailure {
    Fault(Box<ClapFailureRecord>),
    PendingFault,
    Control(String),
}
impl std::fmt::Display for ClapAuthoredParameterFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fault(record) => write!(
                out,
                "{:?}: {}",
                record.operation,
                record.fault.describe_for("CLAP")
            ),
            Self::PendingFault => {
                out.write_str("CLAP first-fault diagnostic is pending publication")
            }
            Self::Control(message) => out.write_str(message),
        }
    }
}
impl std::error::Error for ClapAuthoredParameterFailure {}

fn fault_source(record: ClapFailureRecord) -> ClapAuthoredParameterFailure {
    ClapAuthoredParameterFailure::Fault(Box::new(record))
}

fn native_error(source: ClapAuthoredParameterFailure) -> ParameterErrorKind {
    ParameterErrorKind::Native {
        operation: "CLAP authored parameters",
        source: Box::new(source),
    }
}

impl ClapInstance {
    fn authored_parameter_failure(&self) -> Option<ParameterErrorKind> {
        self.failure.failed().then(|| {
            native_error(match self.failure.first_fault() {
                Some(record) => fault_source(record),
                None => ClapAuthoredParameterFailure::PendingFault,
            })
        })
    }

    fn check_authored_parameter_owner(&self) -> Result<(), ParameterErrorKind> {
        if let Some(error) = self.authored_parameter_failure() {
            return Err(error);
        }
        if !self.core.control_is_current() {
            return Err(native_error(ClapAuthoredParameterFailure::Control(
                "Authored CLAP parameters require the registered main control owner".into(),
            )));
        }
        if self.processing
            || self.core.processing.load(Ordering::Acquire)
            || self.core.retired.load(Ordering::Acquire)
            || self.core.host.audio_busy.load(Ordering::Acquire)
        {
            return Err(native_error(ClapAuthoredParameterFailure::Control(
                "Authored CLAP parameters require an idle retained owner".into(),
            )));
        }
        Ok(())
    }

    fn check_authored_parameter_inventory(&self) -> Result<(), ParameterErrorKind> {
        self.check_authored_parameter_owner()?;
        if self.core.host.params_changed.load(Ordering::Acquire) {
            return Err(ParameterErrorKind::InventoryChanged);
        }
        Ok(())
    }

    /// Prepare and apply one static batch on a quiescent control owner. A failed
    /// candidate must not be published; native partial effects cannot be rolled
    /// back by this API. No renderer selects this additive API implicitly.
    pub fn apply_authored_parameters(
        &mut self,
        assignments: &[AuthoredParameter<'_>],
    ) -> Result<(), ParameterApplyError> {
        let fail = |kind| ParameterApplyError {
            format: crate::PluginFormat::Clap,
            parameter: "<batch>".into(),
            kind,
        };
        self.check_authored_parameter_owner().map_err(fail)?;
        if assignments.is_empty() {
            return Ok(());
        }
        self.refresh_control_metadata().map_err(|message| {
            fail(
                self.authored_parameter_failure().unwrap_or_else(|| {
                    native_error(ClapAuthoredParameterFailure::Control(message))
                }),
            )
        })?;
        let extension = self.params_ext().ok_or_else(|| {
            fail(
                self.authored_parameter_failure()
                    .unwrap_or(ParameterErrorKind::MissingCallback),
            )
        })?;
        // SAFETY: The retained Core owns this freshly queried CLAP params table;
        // metadata preparation runs on its control owner before native writes.
        let count = unsafe { (*extension).count }
            .ok_or_else(|| fail(ParameterErrorKind::MissingCallback))?;
        // SAFETY: The exact-signature guarded callback retains the initialized
        // descriptor and borrows no storage beyond this synchronous count call.
        let count = self
            .foreign(ClapOperation::ParameterQuery, || unsafe {
                clap_guard::plugin_u32(count, self.plugin)
            })
            .map_err(|fault| {
                fail(native_error(fault_source(
                    self.failure.first_fault().unwrap_or(ClapFailureRecord {
                        operation: ClapOperation::ParameterQuery,
                        fault,
                    }),
                )))
            })?;
        // The ordinary metadata reader may skip get_info=false. A checked batch
        // requires one successful record for every declared native parameter.
        if count > 16_384 || count as usize != self.prepared_parameters.len() {
            return Err(fail(ParameterErrorKind::InvalidMetadata));
        }
        // SAFETY: The retained Core owns this params callback table throughout
        // the synchronous checked inventory read.
        let get_info = unsafe { (*extension).get_info }
            .ok_or_else(|| fail(ParameterErrorKind::MissingCallback))?;
        let mut ids = std::collections::HashSet::new();
        for index in 0..count {
            self.check_authored_parameter_inventory().map_err(fail)?;
            // SAFETY: The CLAP metadata record contains only inline scalar,
            // pointer and character-array fields; zero is valid output storage.
            let mut info = unsafe { std::mem::zeroed::<clap_sys::ext::params::clap_param_info>() };
            // SAFETY: The guarded exact-signature callback borrows this one
            // writable metadata record only for the synchronous native query.
            let accepted = self
                .foreign(ClapOperation::ParameterQuery, || unsafe {
                    clap_guard::param_info(get_info, self.plugin, index, &mut info)
                })
                .map_err(|fault| {
                    fail(native_error(fault_source(
                        self.failure.first_fault().unwrap_or(ClapFailureRecord {
                            operation: ClapOperation::ParameterQuery,
                            fault,
                        }),
                    )))
                })?;
            self.check_authored_parameter_inventory().map_err(fail)?;
            if !accepted
                || info.id == u32::MAX
                || !ids.insert(info.id)
                || !info.min_value.is_finite()
                || !info.max_value.is_finite()
                || info.min_value > info.max_value
                || !info.default_value.is_finite()
                || info.default_value < info.min_value
                || info.default_value > info.max_value
            {
                return Err(fail(ParameterErrorKind::InvalidMetadata));
            }
            let parameter = &self.prepared_parameters[index as usize];
            if parameter.id != info.id
                || parameter.name != metadata_name(&info.name)
                || parameter.module != metadata_name(&info.module)
                || parameter.min != info.min_value
                || parameter.max != info.max_value
                || parameter.default != info.default_value
                || parameter.flags != info.flags
            {
                return Err(fail(ParameterErrorKind::InventoryChanged));
            }
        }
        self.check_authored_parameter_inventory().map_err(fail)?;
        let inventory = self.prepared_parameters.clone();
        let mut owner = CheckedClapParameters {
            instance: self,
            inventory,
        };
        owner.apply_authored_parameters(assignments)
    }
}

fn metadata_name<const N: usize>(value: &[std::ffi::c_char; N]) -> String {
    let bytes: Vec<_> = value
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

struct CheckedClapParameters<'a> {
    instance: &'a mut ClapInstance,
    inventory: Vec<PluginParam>,
}
impl AuthoredParameterOwner for CheckedClapParameters<'_> {
    type Key = u32;

    fn parameter_format(&self) -> crate::PluginFormat {
        crate::PluginFormat::Clap
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        self.instance.check_authored_parameter_inventory()
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<u32>, ParameterErrorKind> {
        let target = normalize_param_name(name);
        let mut matching = self
            .inventory
            .iter()
            .filter(|parameter| param_keys(&parameter.name, &parameter.module).contains(&target));
        let parameter = matching.next().ok_or(ParameterErrorKind::Unknown)?;
        if matching.any(|other| other.id != parameter.id) {
            return Err(ParameterErrorKind::Ambiguous);
        }
        Ok(ResolvedParameter {
            key: parameter.id,
            writable: parameter.flags & clap_sys::ext::params::CLAP_PARAM_IS_READONLY == 0,
            plain_range: Some(ParameterRange {
                min: parameter.min,
                max: parameter.max,
            }),
        })
    }
    fn prepare_bound_value(
        &mut self,
        _: &u32,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        if !self.instance.core.can_flush.load(Ordering::Acquire) {
            return Err(ParameterErrorKind::MissingCallback);
        }
        Ok(PreparedParameterValue { value, domain })
    }
    fn write_bound_parameter(
        &mut self,
        key: &u32,
        value: f64,
        _: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        let extension = self.instance.params_ext().ok_or_else(|| {
            self.instance
                .authored_parameter_failure()
                .unwrap_or(ParameterErrorKind::MissingCallback)
        })?;
        self.check_parameter_authority()?;
        // SAFETY: The retained Core owns the immutable extension slots; this
        // control-plane batch is synchronous and the instance remains alive.
        let flush = unsafe { (*extension).flush }.ok_or(ParameterErrorKind::MissingCallback)?;
        let event = clap_event_param_value {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_param_value>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: *key,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        };
        let headers = [&event.header as *const clap_event_header];
        let list = EventList {
            failure: &self.instance.failure,
            events: &headers,
        };
        let input = clap_input_events {
            ctx: &list as *const EventList<'_> as *mut c_void,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };
        let output = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_push),
        };
        // SAFETY: The exact-signature shim lends these stack event lists only
        // during this admitted flush and catches foreign exceptions natively.
        self.instance
            // SAFETY: The retained plugin and stack-backed event/list contexts stay live until this synchronous guarded flush returns.
            .foreign(ClapOperation::ParameterFlush, || unsafe {
                clap_guard::flush(flush, self.instance.plugin, &input, &output)
            })
            .map_err(|fault| {
                native_error(fault_source(self.instance.failure.first_fault().unwrap_or(
                    ClapFailureRecord {
                        operation: ClapOperation::ParameterFlush,
                        fault,
                    },
                )))
            })?;
        self.check_parameter_authority()
    }
}
