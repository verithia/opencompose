//! Additive application-main VST2 ownership; compatibility callers stay separate.
use super::*;
use crate::application_main::{self, UiAuthorityError};
use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::AtomicU8;

#[cfg(any(test, feature = "test-support"))]
mod audit;
mod midi;
#[cfg(any(test, feature = "test-support"))]
mod native_audit;
mod preparation;
mod registry;
mod runtime;
#[cfg(any(test, feature = "test-support"))]
pub use audit::{Vst2HeapCounters, Vst2OwnedAudit, audit_owned_vst2_lifecycle};
pub use midi::Vst2MidiError;
#[cfg(any(test, feature = "test-support"))]
pub use native_audit::{
    VST2_NATIVE_GUARD_CASES, Vst2NativeGuardAudit, audit_owned_vst2_native_case,
};
pub use preparation::{
    MAX_VST2_AUTHORED_NAME_BYTES, MAX_VST2_AUTHORED_PARAMETERS, MAX_VST2_AUTHORED_TOTAL_NAME_BYTES,
    Vst2AuthoredClaim, Vst2AuthoredInput, Vst2OwnedAuthoredPlan,
};
pub use runtime::{
    MAX_VST2_RUNTIME_PARAMETER_POINTS, Vst2BlockBatch, Vst2MidiMessage, Vst2ParameterPoint,
    Vst2RuntimeError, Vst2RuntimeParameterBinding, Vst2RuntimeParameterKey,
    Vst2RuntimePreparationPlan,
};

const IDLE: u8 = 0;
const PROCESSING: u8 = 1;
const CONTROL: u8 = 2;
const RETIRED: u8 = 3;
const MAX_INSTANCES: usize = callback_route::CAPACITY;

#[derive(Debug)]
/// Failure from the owned VST2 lifecycle.
pub enum Vst2OwnedError {
    Authority(UiAuthorityError),
    Busy,
    Retired,
    Capacity,
    Preparation(oc_audio::BlockPreparationError),
    Runtime(Vst2RuntimeError),
    Process(Vst2ProcessError),
    Midi(Vst2MidiError),
    Clock(oc_audio::TransportBlockError),
    Parameter(Box<crate::preset_parameters::ParameterApplyError>),
    Native(String),
    Faulted(Arc<Vst2Failure>),
    Control(String),
}
impl From<UiAuthorityError> for Vst2OwnedError {
    fn from(value: UiAuthorityError) -> Self {
        Self::Authority(value)
    }
}
impl std::fmt::Display for Vst2OwnedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(error) => error.fmt(f),
            Self::Busy => f.write_str("VST2 owned lifecycle or processing is busy"),
            Self::Retired => f.write_str("VST2 source was retired"),
            Self::Capacity => f.write_str("VST2 owned instance capacity exhausted"),
            Self::Preparation(error) => write!(f, "VST2 block preparation refused: {error:?}"),
            Self::Runtime(error) => error.fmt(f),
            Self::Process(error) => error.fmt(f),
            Self::Midi(error) => error.fmt(f),
            Self::Clock(error) => error.fmt(f),
            Self::Parameter(error) => error.fmt(f),
            Self::Native(error) | Self::Control(error) => f.write_str(error),
            Self::Faulted(_) => {
                f.write_str("VST2 native ownership is faulted; inspect the retained first cause")
            }
        }
    }
}
impl std::error::Error for Vst2OwnedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authority(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::Process(error) => Some(error),
            Self::Midi(error) => Some(error),
            Self::Parameter(error) => Some(error.as_ref()),
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

thread_local! { static NATIVE_PHASE: Cell<bool> = const { Cell::new(false) }; }
struct Phase;
impl Phase {
    fn enter() -> Result<Self, Vst2OwnedError> {
        NATIVE_PHASE.with(|phase| {
            if phase.replace(true) {
                Err(Vst2OwnedError::Busy)
            } else {
                Ok(Self)
            }
        })
    }
}
impl Drop for Phase {
    fn drop(&mut self) {
        NATIVE_PHASE.with(|phase| phase.set(false));
    }
}

/// A plan to load a VST2 instance on the UI thread.
pub struct Vst2OwnedLoadPlan {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub max_frames: u32,
}
impl Vst2OwnedLoadPlan {
    /// Load on the UI thread and wait for the result.
    pub fn execute_on_ui(self) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
        self.dispatch()?.wait().map_err(Vst2OwnedError::Control)?
    }
    /// Dispatch the load without waiting for it.
    pub fn dispatch(
        self,
    ) -> Result<
        application_main::ApplicationControlJob<Result<Vst2OwnedInstance, Vst2OwnedError>>,
        Vst2OwnedError,
    > {
        dispatch(move || {
            let token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let source = registry::reserve(&token)?;
            let native =
                Vst2Instance::load_owned(&token, &self.path, self.sample_rate, self.max_frames)?;
            registry::adopt(&token, source, native)
        })
    }
}
fn dispatch<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Vst2OwnedError> + Send + 'static,
) -> Result<application_main::ApplicationControlJob<Result<T, Vst2OwnedError>>, Vst2OwnedError> {
    application_main::registered_ui_executor()?
        .dispatch_control(move || Ok(work()))
        .map_err(Vst2OwnedError::Control)
}

#[derive(Debug, Clone)]
/// Metadata captured when a VST2 instance was loaded.
pub struct Vst2OwnedSnapshot {
    pub path: PathBuf,
    pub unique_id: i32,
    pub name: String,
    pub vendor: String,
    pub product: String,
    pub version: i32,
    pub category: PluginCategory,
    pub accepts_midi: bool,
    pub tail_samples: Option<u32>,
    pub surface: crate::params::ParamSurface,
    pub parameters: Vec<Vst2Param>,
    pub inputs: Vec<AudioPort>,
    pub outputs: Vec<AudioPort>,
    pub sample_rate: u32,
    pub max_frames: u32,
    pub latency_samples: u32,
    pub inventory_revision: u64,
}
/// A loaded VST2 processor and its control handle.
pub struct Vst2OwnedInstance {
    pub runtime_parameters: Arc<[Vst2RuntimeParameterBinding]>,
    pub processor: Vst2ProcessorLease,
    pub control: Vst2ControlHandle,
    pub snapshot: Arc<Vst2OwnedSnapshot>,
}
#[derive(Clone)]
/// UI-thread control handle for a VST2 instance.
pub struct Vst2ControlHandle {
    link: Arc<Link>,
}
/// Exclusive processor lease; deliberately not `Sync`.
pub struct Vst2ProcessorLease {
    link: Arc<Link>,
    _not_sync: PhantomData<Cell<()>>,
}
struct Link {
    runtime: Option<runtime::Authority>,
    _runtime_parameters: Arc<[Vst2RuntimeParameterBinding]>,
    runtime_invalid: AtomicBool,
    native: NonNull<Vst2Instance>,
    failure: Arc<Vst2Failure>,
    gate: AtomicU8,
    retire: AtomicBool,
    snapshot: Arc<Vst2OwnedSnapshot>,
}
// SAFETY: The UI registry owns the entire stable native allocation until all
// external strong leases finish. Native access requires the exclusive atomic
// gate; control/lifecycle access additionally requires actual UI authority.
unsafe impl Send for Link {}
// SAFETY: Shared data is immutable/atomic, and sharing this ticket cannot expose
// a native reference. No Weak tickets escape to race last-lease retirement.
unsafe impl Sync for Link {}
struct Permit<'a> {
    link: &'a Link,
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.link.gate.store(IDLE, Ordering::Release);
    }
}
impl Link {
    fn acquire(&self, role: u8) -> Result<Permit<'_>, Vst2OwnedError> {
        if self.failure.failed() {
            return Err(Vst2OwnedError::Faulted(self.failure.clone()));
        }
        if self.retire.load(Ordering::Acquire) {
            return Err(Vst2OwnedError::Retired);
        }
        if role == PROCESSING && self.runtime_invalid.load(Ordering::Acquire) {
            return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::InventoryChanged));
        }
        self.gate
            .compare_exchange(IDLE, role, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Vst2OwnedError::Busy)?;
        let permit = Permit { link: self };
        if self.failure.failed() {
            return Err(Vst2OwnedError::Faulted(self.failure.clone()));
        }
        if self.retire.load(Ordering::Acquire) {
            return Err(Vst2OwnedError::Retired);
        }
        if role == PROCESSING && self.runtime_invalid.load(Ordering::Acquire) {
            return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::InventoryChanged));
        }
        Ok(permit)
    }
}
impl Vst2ProcessorLease {
    /// Shared failure record for this lease.
    pub fn failure(&self) -> &Vst2Failure {
        &self.link.failure
    }
    /// The transport and stereo storage stay borrowed through this prepared call.
    pub fn process_effect_into(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
    ) -> Result<(), Vst2OwnedError> {
        self.process(output, transport, true)
    }
    /// Render an instrument block into `output`.
    pub fn process_instrument_into(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
    ) -> Result<(), Vst2OwnedError> {
        self.process(output, transport, false)
    }
    fn process(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
        input: bool,
    ) -> Result<(), Vst2OwnedError> {
        let _permit = self.link.acquire(PROCESSING)?;
        // SAFETY: The exclusive gate and registry's strong lease retain all
        // native/audio/event backing. Only prepared processing runs on workers.
        let native = unsafe { &mut *self.link.native.as_ptr() };
        native
            .preflight_block(output.len(), self.link.snapshot.sample_rate)
            .map_err(Vst2OwnedError::Process)?;
        if let Some(transport) = transport {
            transport
                .checked_process_block(self.link.snapshot.sample_rate, output.len() / 2)
                .map_err(Vst2OwnedError::Clock)?;
        }
        midi::validate_block(&native.pending_midi, output.len() / 2)
            .map_err(Vst2OwnedError::Midi)?;
        let result =
            native.process_prepared(output, self.link.snapshot.sample_rate, transport, input);
        if self.link.failure.failed() {
            output.fill(0.0);
            return Err(Vst2OwnedError::Faulted(self.link.failure.clone()));
        }
        result.map_err(Vst2OwnedError::Process)
    }
    /// Queue one MIDI event at a block offset.
    pub fn queue_midi(
        &mut self,
        status: u8,
        data1: u8,
        data2: u8,
        sample_offset: i32,
    ) -> Result<(), Vst2OwnedError> {
        let _permit = self.link.acquire(PROCESSING)?;
        // SAFETY: The exclusive gate guards this preallocated MIDI queue; its
        // registry-owned allocation cannot be retired while this lease exists.
        let native = unsafe { &mut *self.link.native.as_ptr() };
        native
            .check_callback_route()
            .map_err(Vst2OwnedError::Process)?;
        if let Some(error) = native.process_error {
            return Err(Vst2OwnedError::Process(error));
        }
        let message = midi::admit(
            PendingMidi {
                status,
                data1,
                data2,
                offset: sample_offset,
            },
            self.link.snapshot.max_frames,
            native.pending_midi.len(),
            native.pending_midi.capacity(),
        )
        .map_err(Vst2OwnedError::Midi)?;
        native.pending_midi.push(message);
        Ok(())
    }
}
impl Drop for Vst2ProcessorLease {
    fn drop(&mut self) {
        // The UI registry keeps a strong ticket. This never allocates, releases
        // native state, locks a queue, or frees the last Arc on a worker.
        self.link.retire.store(true, Ordering::Release);
    }
}
impl Vst2ControlHandle {
    /// Retain this reporting handle before retiring the source.
    pub fn failure(&self) -> Arc<Vst2Failure> {
        self.link.failure.clone()
    }
    /// Capture state on the UI thread.
    pub fn capture_state_on_ui(&self) -> Result<Vec<u8>, Vst2OwnedError> {
        let link = self.link.clone();
        dispatch(move || {
            let _token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let _permit = link.acquire(CONTROL)?;
            // SAFETY: This UI-scoped exclusive control permit owns native access.
            let native = unsafe { &mut *link.native.as_ptr() };
            let result = native.state_bytes();
            if link.failure.failed() {
                return Err(Vst2OwnedError::Faulted(link.failure.clone()));
            }
            result.map_err(Vst2OwnedError::Native)
        })?
        .wait()
        .map_err(Vst2OwnedError::Control)?
    }
    /// Write a parameter on the UI thread.
    pub fn write_parameter_on_ui(&self, name: String, value: f64) -> Result<(), Vst2OwnedError> {
        let link = self.link.clone();
        dispatch(move || {
            let _token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let _permit = link.acquire(CONTROL)?;
            // SAFETY: Registry retention and exclusive UI control exclude workers.
            let native = unsafe { &mut *link.native.as_ptr() };
            if native
                .host
                .authored_inventory_revision
                .load(Ordering::Acquire)
                != link.snapshot.inventory_revision
            {
                return Err(Vst2OwnedError::Native(
                    "VST2 parameter inventory changed".into(),
                ));
            }
            use crate::preset_parameters::{
                AuthoredParameter, AuthoredParameterOwner, ParameterWriteDomain,
            };
            let mut owner = native.authored_parameter_owner().map_err(|kind| {
                Vst2OwnedError::Parameter(Box::new(crate::preset_parameters::ParameterApplyError {
                    format: crate::PluginFormat::Vst2,
                    parameter: name.clone(),
                    kind,
                }))
            })?;
            let result = owner
                .apply_authored_parameters(&[AuthoredParameter {
                    name: &name,
                    value,
                    domain: ParameterWriteDomain::Normalized,
                }])
                .map_err(|error| Vst2OwnedError::Parameter(Box::new(error)));
            if link.failure.failed() {
                return Err(Vst2OwnedError::Faulted(link.failure.clone()));
            }
            result
        })?
        .wait()
        .map_err(Vst2OwnedError::Control)?
    }
}
/// Actual UI runtimes service this automatically from the existing main poll.
pub fn poll_owned_vst2_retirements() -> Result<usize, Vst2OwnedError> {
    let token = application_main::ui_retirement_authority()?;
    registry::drain(&token)
}
