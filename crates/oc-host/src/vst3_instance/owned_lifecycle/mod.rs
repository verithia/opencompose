//! Additive UI-owned VST3 lifetime and exclusive processor access.
//! Legacy instance entrypoints remain separate from the owned lifecycle.
use super::*;
use crate::application_main::{self, UiAuthorityError, UiRetirementToken, UiThreadToken};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::AtomicU8;

#[cfg(any(test, feature = "test-support"))]
mod authored_preparation_audit;
#[cfg(any(test, feature = "test-support"))]
mod authored_runtime_audit;
#[cfg(any(test, feature = "test-support"))]
pub use authored_preparation_audit::{
    Vst3AuthoredPreparationAudit, audit_owned_authored_preparation,
};
#[cfg(any(test, feature = "test-support"))]
pub use authored_runtime_audit::{Vst3AuthoredRuntimeAudit, audit_owned_authored_runtime};
mod preparation;
pub use preparation::{
    MAX_VST3_AUTHORED_NAME_BYTES, MAX_VST3_AUTHORED_PARAMETERS, MAX_VST3_AUTHORED_TOTAL_NAME_BYTES,
    Vst3AuthoredInput, Vst3OwnedAuthoredPlan, Vst3OwnedLoadJob,
};
mod activation;
#[cfg(any(test, feature = "test-support"))]
pub use activation::{Vst3ActivationAudit, audit_checked_owned_activation};
mod control;
mod metadata;
mod midi;
#[cfg(any(test, feature = "test-support"))]
mod midi_audit;
pub use midi::{Vst3MidiEvent, Vst3MidiMessage};
#[cfg(any(test, feature = "test-support"))]
pub use midi_audit::{Vst3MidiIngressAudit, audit_owned_midi_ingress};
#[cfg(any(test, feature = "test-support"))]
mod preparation_audit;
#[cfg(any(test, feature = "test-support"))]
pub use preparation_audit::{Vst3InitialPreparationAudit, audit_owned_initial_preparation};
mod processing;
mod registry;
pub use control::{
    Vst3MetadataReadPlan, Vst3NormalizedBindingsPlan, Vst3ParameterWritePlan, Vst3StateCapturePlan,
};
#[cfg(any(test, feature = "test-support"))]
pub(super) use processing::AdmissionWork;
pub use processing::{
    Vst3OwnedInstrumentBlock, Vst3OwnedProcessBlock, Vst3ProcessLeaseError, Vst3ProcessorLease,
};

const IDLE: u8 = 0;
const PROCESSING: u8 = 1;
const CONTROL: u8 = 2;
const RETIRED: u8 = 3;
const MAX_INSTANCES: usize = 1024;
const MAX_PARAMETERS: usize = 16384;

#[derive(Debug)]
/// Failure from the owned VST3 lifecycle.
pub enum Vst3OwnedError {
    Authority(UiAuthorityError),
    Busy,
    Retired,
    Capacity,
    InventoryChanged,
    InvalidRequest(&'static str),
    Native(Box<Vst3ProcessError>),
    Control(String),
    Authored(Box<crate::preset_parameters::ParameterApplyError>),
}
impl From<UiAuthorityError> for Vst3OwnedError {
    fn from(error: UiAuthorityError) -> Self {
        Self::Authority(error)
    }
}
impl std::fmt::Display for Vst3OwnedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(error) => error.fmt(f),
            Self::Busy => f.write_str("VST3 processing/control access is busy"),
            Self::Retired => f.write_str("VST3 source was retired"),
            Self::Capacity => f.write_str("VST3 owned-instance capacity is exhausted"),
            Self::InventoryChanged => f.write_str("VST3 inventory changed during metadata capture"),
            Self::InvalidRequest(message) => f.write_str(message),
            Self::Native(error) => error.fmt(f),
            Self::Control(error) => f.write_str(error),
            Self::Authored(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Vst3OwnedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authority(error) => Some(error),
            Self::Native(error) => Some(error.as_ref()),
            Self::Authored(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// Prepared point storage the registry admits for one instance.
pub const PREPARED_PARAMETER_POINTS: usize = 1_048_576;

#[derive(Debug, Clone, Copy)]
/// Per-block event and per-parameter point budgets.
pub struct Vst3ProcessingBudget {
    pub events: usize,
    pub points_per_parameter: usize,
}
impl Default for Vst3ProcessingBudget {
    fn default() -> Self {
        Self {
            events: 128,
            points_per_parameter: 8,
        }
    }
}
impl Vst3ProcessingBudget {
    /// Bound the per-parameter point reservation by the prepared-storage
    /// capacity the registry admits.
    ///
    /// A plugin with thousands of parameters would otherwise reserve the full
    /// per-parameter budget for every one of them and be refused as too large
    /// to prepare before it ever renders a block. Fitting keeps the prepared
    /// storage bounded; a caller that really writes more points than the
    /// fitted reservation allows is still refused, never truncated.
    pub fn fitted_to(self, parameters: usize) -> Self {
        let per_parameter = PREPARED_PARAMETER_POINTS
            .checked_div(parameters)
            .map_or(self.points_per_parameter, |points| {
                points.clamp(1, self.points_per_parameter)
            });
        Self {
            events: self.events,
            points_per_parameter: per_parameter,
        }
    }

    fn validate(self) -> Result<(), Vst3OwnedError> {
        if !(1..=65536).contains(&self.events) || !(1..=1024).contains(&self.points_per_parameter) {
            return Err(Vst3OwnedError::InvalidRequest(
                "Invalid VST3 processing budget",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn fitting_leaves_a_small_inventory_on_its_budget() {
        let budget = Vst3ProcessingBudget {
            events: 65536,
            points_per_parameter: 1024,
        };
        assert_eq!(budget.fitted_to(8).points_per_parameter, 1024);
        assert_eq!(budget.fitted_to(1024).points_per_parameter, 1024);
        assert_eq!(budget.fitted_to(0).points_per_parameter, 1024);
    }

    #[test]
    fn fitting_a_large_inventory_stays_inside_the_prepared_capacity() {
        let budget = Vst3ProcessingBudget {
            events: 65536,
            points_per_parameter: 1024,
        };
        // A 4145-parameter plugin exceeds the prepared capacity divided by the
        // per-parameter budget.
        let fitted = budget.fitted_to(4145);
        assert_eq!(fitted.points_per_parameter, 252);
        assert!(4145 * fitted.points_per_parameter <= PREPARED_PARAMETER_POINTS);
        // A plugin with more parameters than points still gets one point each.
        let huge = budget.fitted_to(PREPARED_PARAMETER_POINTS + 1);
        assert_eq!(huge.points_per_parameter, 1);
    }

    #[test]
    fn fitting_never_raises_a_budget() {
        let budget = Vst3ProcessingBudget {
            events: 128,
            points_per_parameter: 8,
        };
        assert_eq!(budget.fitted_to(1).points_per_parameter, 8);
        assert_eq!(budget.fitted_to(1).events, 128);
    }
}

#[derive(Debug, Clone, Copy)]
/// One normalized parameter value.
pub struct Vst3NormalizedParameter {
    pub id: u32,
    pub value: f64,
}
#[derive(Debug, Clone, Copy)]
/// A normalized parameter event at a block offset.
pub struct Vst3NormalizedEvent {
    pub id: u32,
    pub value: f64,
    pub sample_offset: u32,
}

/// One source-ordered normalized parameter or MIDI event. Equal-offset order
/// is retained even when MIDI maps to the same native parameter ID.
#[derive(Debug, Clone, Copy)]
pub enum Vst3OrderedEvent {
    Parameter(Vst3NormalizedEvent),
    Midi(Vst3MidiEvent),
}

#[derive(Debug, Clone, PartialEq)]
/// One parameter declared by a loaded VST3 instance.
pub struct Vst3OwnedParameter {
    pub id: u32,
    pub name: String,
    pub unit: String,
    pub default_normalized: f64,
    pub flags: i32,
    pub steps: i32,
}
#[derive(Debug, Clone)]
/// Metadata captured when a VST3 instance was loaded.
pub struct Vst3OwnedSnapshot {
    pub parameters: Vec<Vst3OwnedParameter>,
    pub inputs: Vec<AudioPort>,
    pub outputs: Vec<AudioPort>,
    pub latency_samples: u32,
    pub tail_samples: u32,
    pub max_frames: u32,
    source: u64,
    inventory_revision: u64,
}

/// Plan to load a VST3 instance with state and parameters.
pub struct Vst3OwnedLoadPlan {
    pub path: std::path::PathBuf,
    pub sample_rate: u32,
    pub max_frames: u32,
    pub component_state: Option<Vec<u8>>,
    pub parameters: Vec<Vst3NormalizedParameter>,
    pub budget: Vst3ProcessingBudget,
}
impl Vst3OwnedLoadPlan {
    /// Load the instance on the UI thread.
    pub fn execute_on_ui(self) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
        dispatch_owned(move || {
            let token = application_main::ui_authority()?;
            registry::validate_initial_budget(self.budget, &self.parameters)?;
            let prepared = preparation::prepare(
                &token,
                self.path,
                self.sample_rate,
                self.max_frames,
                self.component_state,
                preparation::InitialParameters::Normalized(self.parameters),
            )?;
            registry::adopt_prepared(&token, prepared.candidate, prepared.snapshot, self.budget)
        })
    }
}

/// A loaded VST3 processor and its control handle.
pub struct Vst3OwnedInstance {
    pub processor: Vst3ProcessorLease,
    pub control: Vst3ControlHandle,
    pub snapshot: Arc<Vst3OwnedSnapshot>,
}
#[derive(Clone)]
/// UI-thread control handle for a VST3 instance.
pub struct Vst3ControlHandle {
    link: Arc<ProcessLink>,
}
struct ProcessLink {
    source: u64,
    instance: NonNull<Vst3Instance>,
    gate: AtomicU8,
    retire: AtomicBool,
    failure: Arc<Vst3Failure>,
    snapshot: Arc<Vst3OwnedSnapshot>,
    midi: midi::PreparedMidi,
    budget: Vst3ProcessingBudget,
}
// SAFETY: the UI registry retains the stable native allocation until every
// external strong lease has gone. Only an exclusive atomic processing/control
// permit dereferences it, and controller/lifecycle access additionally requires UI.
unsafe impl Send for ProcessLink {}
// SAFETY: shared fields are immutable or atomic. No native reference is cloned,
// released, exposed, or dereferenced by sharing this ownership record.
unsafe impl Sync for ProcessLink {}
struct Permit<'a>(&'a ProcessLink);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.gate.store(IDLE, Ordering::Release);
    }
}
impl ProcessLink {
    fn acquire(&self, role: u8) -> Result<Permit<'_>, Vst3OwnedError> {
        if self.retire.load(Ordering::Acquire) {
            return Err(Vst3OwnedError::Retired);
        }
        self.gate
            .compare_exchange(IDLE, role, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Vst3OwnedError::Busy)?;
        let permit = Permit(self);
        if self.retire.load(Ordering::Acquire) {
            return Err(Vst3OwnedError::Retired);
        }
        Ok(permit)
    }
}
fn native_or_message(failure: &Vst3Failure, message: String) -> Vst3OwnedError {
    failure.first_fault().map_or_else(
        || Vst3OwnedError::Control(message),
        |error| Vst3OwnedError::Native(Box::new(error)),
    )
}
fn dispatch_owned<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Vst3OwnedError> + Send + 'static,
) -> Result<T, Vst3OwnedError> {
    dispatch_owned_job(work)?
        .wait()
        .map_err(Vst3OwnedError::Control)?
}
fn dispatch_owned_job<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Vst3OwnedError> + Send + 'static,
) -> Result<application_main::ApplicationControlJob<Result<T, Vst3OwnedError>>, Vst3OwnedError> {
    let executor = application_main::registered_ui_executor()?;
    executor
        .dispatch_control(move || Ok(work()))
        .map_err(Vst3OwnedError::Control)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn adopt_native_fixture(
    token: &UiThreadToken<'_>,
    instance: Vst3Instance,
) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
    adopt_native_fixture_with_budget(token, instance, Vst3ProcessingBudget::default())
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn adopt_native_fixture_with_budget(
    token: &UiThreadToken<'_>,
    instance: Vst3Instance,
    budget: Vst3ProcessingBudget,
) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
    let slot = registry::reserve(token, instance.failure())?;
    registry::adopt(token, instance, budget, &[], slot)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn check_fixture_capacity(token: &UiThreadToken<'_>) -> Result<(), Vst3OwnedError> {
    registry::check_capacity(token)
}

/// Explicit non-RT owner drain; normal runtimes call this service from their main poll.
pub fn poll_owned_vst3_retirements() -> Result<usize, Vst3OwnedError> {
    let token = application_main::ui_retirement_authority()?;
    registry::drain(&token)
}

// The checked authored adapter consumes the same bounded native inventory as owned preparation.
pub(super) fn capture_authored_metadata(
    instance: &Vst3Instance,
) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    metadata::capture(&token, instance, 0)
}
pub(super) fn same_authored_inventory(a: &Vst3OwnedSnapshot, b: &Vst3OwnedSnapshot) -> bool {
    a.inventory_revision == b.inventory_revision
        && a.parameters == b.parameters
        && a.inputs == b.inputs
        && a.outputs == b.outputs
        && a.max_frames == b.max_frames
}
pub(super) fn admit_authored_verification(
    parameters: usize,
    assignments: usize,
) -> Result<(), Vst3OwnedError> {
    metadata::authored_verification_queries(parameters, assignments).map(|_| ())
}
