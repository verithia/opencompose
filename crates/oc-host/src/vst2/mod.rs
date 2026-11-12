//! VST2 instrument/effect host.
//!
//! `Vst2Instance` loads a bundle with the plugin's own entry point, drives it
//! through the dispatcher, and exposes it as an [`oc_audio::AudioEffect`].
//! Scanning and directory discovery live in `scan`; the raw interface in
//! `abi`.
//!
//! An instance is moved between host threads, never shared. Plugins may call
//! the host from native helper threads; optional time information is therefore
//! published coherently to bounded, independent callback-thread records.

mod authored_parameters;
mod call_scratch;
mod callback_route;
mod callback_thunks;
mod candidate;
mod failure;
mod guard;
mod module_authority;
pub use failure::{Vst2Failure, Vst2FailureRecord, Vst2Operation};
mod owned_lifecycle;
pub use owned_lifecycle::*;
#[cfg(test)]
mod foreign_threads;
mod time_info;
pub use authored_parameters::{Vst2AuthoredParameterKey, Vst2AuthoredParameterOwner};

pub mod abi;
pub mod scan;

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use oc_audio::{AudioEffect, PluginTransport};

use crate::PluginCategory;
use crate::ports::AudioPort;

use abi::plugin_op as op;

/// Parameters beyond this are not exposed; real plugins stay far below it.
const MAX_REPORTED_PARAMS: i32 = 4096;
/// Channel counts above this are treated as a broken descriptor.
const MAX_CHANNELS: i32 = 32;
/// Bounds native i32 frame arguments and the retained channel-plane budget.
/// At the maximum 32 input and 32 output channels, audio scratch is at most 16 MiB.
pub const MAX_PREPARED_BLOCK_FRAMES: usize = 65_536;

fn checked_block_frames(frames: usize) -> Result<u32, oc_audio::BlockPreparationError> {
    let frames = frames.max(1);
    if frames > i32::MAX as usize || frames > MAX_PREPARED_BLOCK_FRAMES {
        return Err(oc_audio::BlockPreparationError::CapacityExceeded);
    }
    u32::try_from(frames).map_err(|_| oc_audio::BlockPreparationError::CapacityExceeded)
}

fn checked_state_chunk_size(size: usize) -> Result<(), String> {
    if size > oc_audio::bounded_file::MAX_PLUGIN_STATE_BYTES || size > isize::MAX as usize {
        return Err(format!(
            "VST2 state chunk exceeds the {}-byte limit",
            oc_audio::bounded_file::MAX_PLUGIN_STATE_BYTES
        ));
    }
    Ok(())
}

/// Bounded MIDI/event storage prepared before processing.
pub const MAX_PENDING_MIDI: usize = 1024;

/// A prepared process block was refused, or its plugin requested new routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vst2ProcessError {
    IncompleteFrame,
    CapacityExceeded,
    EventCapacityExceeded,
    TopologyChanged,
    SampleRateChanged,
    UnavailableProcessor,
    CallbackAdmissionOverflow,
    NativeFault,
}

impl std::fmt::Display for Vst2ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IncompleteFrame => "VST2 output must contain complete stereo frames",
            Self::CapacityExceeded => "VST2 block exceeds prepared frame capacity",
            Self::EventCapacityExceeded => "VST2 MIDI queue exceeds its prepared event capacity",
            Self::TopologyChanged => {
                "VST2 routing changed; prepare the instance again before processing"
            }
            Self::SampleRateChanged => {
                "VST2 sample rate changed; prepare the instance outside processing"
            }
            Self::UnavailableProcessor => "VST2 plugin has no supported process callback",
            Self::NativeFault => "VST2 native ownership is faulted",
            Self::CallbackAdmissionOverflow => {
                "VST2 concurrent host callbacks exceeded their prepared capacity"
            }
        })
    }
}
impl std::error::Error for Vst2ProcessError {}

/// One automatable parameter as the host sees it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Vst2Param {
    pub index: i32,
    pub name: String,
    pub label: String,
    pub display: String,
    /// Normalized 0..1 value.
    pub value: f64,
    pub default: f64,
    pub can_automate: bool,
}

/// The generic panel's descriptor for one VST2 parameter.
///
/// VST2 has no factory default and no group tree: `default` stays `None` so the
/// panel falls back to the value the instance had when it opened, and every
/// parameter lands in the panel's single ungrouped section.
pub fn descriptor(param: &Vst2Param, steps: Option<u32>) -> crate::params::ParamDescriptor {
    crate::params::ParamDescriptor {
        index: param.index as u32,
        name: param.name.clone(),
        unit: param.label.clone(),
        min: 0.0,
        max: 1.0,
        current: param.value,
        display: param.display.clone(),
        normalized: crate::params::normalize(param.value, 0.0, 1.0),
        default: None,
        steps,
        automatable: param.can_automate,
        by_passing: crate::params::names_bypass(&param.name) && param.value >= 0.5,
        group: None,
    }
}

/// One queued MIDI message with a block-relative frame offset.
#[derive(Debug, Clone, Copy)]
struct PendingMidi {
    status: u8,
    data1: u8,
    data2: u8,
    offset: i32,
}

/// State shared with the plugin through the `audioMaster` callback.
///
/// A plugin may call back from a helper thread it started. Mutable scalar
/// callbacks use atomics; time publication never overwrites a record borrowed
/// by another callback thread.
struct HostContext {
    sample_rate: AtomicU32,
    block_size: AtomicU32,
    process_level: AtomicI32,
    latency: AtomicU32,
    num_params: AtomicI32,
    values: Box<[AtomicU32]>,
    tempo_bits: AtomicU64,
    time_info: time_info::TimeInfoPublisher,
    io_changed: AtomicBool,
    authored_inventory_revision: AtomicU64,
    idle_count: AtomicU32,
    failure: Arc<Vst2Failure>,
    #[cfg(any(test, feature = "test-support"))]
    audit_entry_thread: AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    audit_close_thread: AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    audit_process_calls: AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    audit_event_calls: AtomicU64,
}

impl HostContext {
    #[cfg(test)]
    fn new(sample_rate: u32, block_size: u32, num_params: i32) -> Self {
        Self::with_failure(
            sample_rate,
            block_size,
            num_params,
            Arc::new(Vst2Failure::default()),
        )
    }
    fn with_failure(
        sample_rate: u32,
        block_size: u32,
        num_params: i32,
        failure: Arc<Vst2Failure>,
    ) -> Self {
        // Entry-time callbacks and later callbacks retain the same backing.
        // Reserve the bounded parameter table before entering native code.
        let values = (0..MAX_REPORTED_PARAMS)
            .map(|_| AtomicU32::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            sample_rate: AtomicU32::new(sample_rate),
            block_size: AtomicU32::new(block_size),
            process_level: AtomicI32::new(abi::process_level::USER),
            latency: AtomicU32::new(0),
            num_params: AtomicI32::new(num_params.clamp(0, MAX_REPORTED_PARAMS)),
            values,
            tempo_bits: AtomicU64::new(120.0_f64.to_bits()),
            time_info: time_info::TimeInfoPublisher::new(sample_rate),
            io_changed: AtomicBool::new(false),
            authored_inventory_revision: AtomicU64::new(0),
            idle_count: AtomicU32::new(0),
            failure,
            #[cfg(any(test, feature = "test-support"))]
            audit_entry_thread: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            audit_close_thread: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            audit_process_calls: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            audit_event_calls: AtomicU64::new(0),
        }
    }

    fn value(&self, index: i32) -> Option<f32> {
        if !(0..self.num_params.load(Ordering::Acquire)).contains(&index) {
            return None;
        }
        let slot = self.values.get(index as usize)?;
        Some(f32::from_bits(slot.load(Ordering::Relaxed)))
    }

    fn store_value(&self, index: i32, value: f32) {
        if !(0..self.num_params.load(Ordering::Acquire)).contains(&index) {
            return;
        }
        if let Some(slot) = self.values.get(index as usize) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    fn set_time(&self, transport: Option<&PluginTransport>) -> abi::VstTimeInfo {
        let mut info = abi::VstTimeInfo {
            sample_rate: f64::from(self.sample_rate.load(Ordering::Relaxed)),
            ..abi::VstTimeInfo::default()
        };
        if let Some(transport) = transport {
            let (numerator, denominator, bar_start_quarter, _) = transport.musical_position();
            info.sample_pos = transport.sample_pos as f64;
            info.sample_rate = f64::from(transport.sample_rate);
            info.ppq_pos = transport.quarter_notes();
            info.tempo = transport.current_tempo();
            info.bar_start_pos = bar_start_quarter;
            info.time_sig_numerator = i32::from(numerator);
            info.time_sig_denominator = i32::from(denominator);
            info.flags = abi::TIME_PPQ_POS_VALID
                | abi::TIME_TEMPO_VALID
                | abi::TIME_BARS_VALID
                | abi::TIME_TIME_SIG_VALID
                | if transport.playing {
                    abi::TIME_TRANSPORT_PLAYING
                } else {
                    0
                };
            self.tempo_bits
                .store(info.tempo.to_bits(), Ordering::Relaxed);
        }
        self.time_info.publish(info);
        info
    }
}

thread_local! {
    /// Host context for callbacks made before it can be stored on the plugin.
    static ENTRY_HOST: std::cell::Cell<*mut HostContext> = const {
        std::cell::Cell::new(std::ptr::null_mut())
    };
    static ACTIVE_HOST: std::cell::Cell<*mut HostContext> = const {
        std::cell::Cell::new(std::ptr::null_mut())
    };
}

/// The `audioMaster` callback a plugin keeps for its lifetime.
unsafe extern "C" fn audio_master(
    effect: *mut abi::AEffect,
    opcode: i32,
    index: i32,
    _value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    // SAFETY: The compatibility callback ABI requires a still-live descriptor
    // and its host-installed reserved1 backing (or the scoped entry fallback).
    // Consume this borrow only inside this callback. This adapter does not
    // protect an invalid late callback after native destruction; owned caller
    // migration and native exception containment remain separate requirements.
    let Some(context) = (unsafe { legacy_host_context(effect) }) else {
        return 0;
    };
    host_opcode(context, opcode, index, ptr, opt)
}

#[cfg(test)]
unsafe extern "C" fn owned_audio_master(
    effect: *mut abi::AEffect,
    opcode: i32,
    index: i32,
    _value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    let Some(access) = owned_host_context(effect) else {
        return 0;
    };
    host_opcode(access.context(), opcode, index, ptr, opt)
}

fn host_opcode(
    context: &HostContext,
    opcode: i32,
    index: i32,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    use abi::host_op as host;
    match opcode {
        #[cfg(any(test, feature = "test-support"))]
        0x4f43_0001 => {
            context.audit_entry_thread.store(
                time_info::native_thread_id().unwrap_or(0),
                Ordering::Relaxed,
            );
            0
        }
        #[cfg(any(test, feature = "test-support"))]
        0x4f43_0002 => {
            context.audit_close_thread.store(
                time_info::native_thread_id().unwrap_or(0),
                Ordering::Relaxed,
            );
            0
        }
        #[cfg(any(test, feature = "test-support"))]
        0x4f43_0003 => {
            context.audit_process_calls.fetch_add(1, Ordering::Relaxed);
            0
        }
        #[cfg(any(test, feature = "test-support"))]
        0x4f43_0004 => {
            context.audit_event_calls.fetch_add(1, Ordering::Relaxed);
            0
        }
        #[cfg(any(test, feature = "test-support"))]
        0x4f43_0010 if context.failure.has_module() => {
            panic!("controlled owned VST2 callback panic")
        }
        host::AUTOMATE => {
            if index >= 0 && opt.is_finite() {
                context.store_value(index, opt.clamp(0.0, 1.0));
            }
            0
        }
        host::VERSION => abi::HOST_VST_VERSION as isize,
        // Shell plugins are not supported: one bundle is one plugin.
        host::CURRENT_ID => 0,
        host::IDLE => {
            context.idle_count.fetch_add(1, Ordering::Relaxed);
            0
        }
        host::WANT_MIDI => 1,
        host::GET_TIME => {
            // SAFETY: GET_TIME returns a read-only borrow on the requesting
            // native thread, until its next request or instance retirement.
            // Publication cannot overwrite another thread's borrowed record.
            (unsafe { context.time_info.callback_pointer() }) as isize
        }
        host::PROCESS_EVENTS => 0,
        host::TEMPO_AT => {
            let tempo = f64::from_bits(context.tempo_bits.load(Ordering::Relaxed));
            (tempo * 10_000.0) as isize
        }
        host::GET_NUM_AUTOMATABLE_PARAMETERS => context.num_params.load(Ordering::Acquire) as isize,
        host::GET_PARAMETER_QUANTIZATION => 1,
        host::IO_CHANGED => {
            context
                .authored_inventory_revision
                .fetch_add(1, Ordering::AcqRel);
            context.io_changed.store(true, Ordering::Release);
            1
        }
        host::NEED_IDLE => 1,
        // No plugin window is ever opened, so a resize request is declined.
        host::SIZE_WINDOW => 0,
        host::GET_SAMPLE_RATE => context.sample_rate.load(Ordering::Relaxed) as isize,
        host::GET_BLOCK_SIZE => context.block_size.load(Ordering::Relaxed) as isize,
        host::GET_INPUT_LATENCY | host::GET_OUTPUT_LATENCY => {
            context.latency.load(Ordering::Relaxed) as isize
        }
        host::WILL_REPLACE_OR_ACCUMULATE => 1,
        host::GET_CURRENT_PROCESS_LEVEL => context.process_level.load(Ordering::Relaxed) as isize,
        host::GET_AUTOMATION_STATE => 1,
        host::GET_VENDOR_STRING => {
            // SAFETY: this VST2 opcode requires a caller-owned 64-byte writable vendor
            // buffer; the static source text is shorter and disjoint from that native output.
            unsafe { abi::write_c_string(ptr, "OpenCompose", 64) };
            1
        }
        host::GET_PRODUCT_STRING => {
            // SAFETY: the product-string opcode supplies 64 writable bytes, separate
            // from this static source; write_c_string bounds both copy and terminator to that capacity.
            unsafe { abi::write_c_string(ptr, "OpenCompose DAW", 64) };
            1
        }
        host::GET_VENDOR_VERSION => 1,
        host::CAN_DO => {
            // SAFETY: The opcode supplies a readable NUL-terminated query for
            // this call. Read only through its first NUL or the longest supported
            // name plus terminator; do not assume a short string fills the budget.
            let supported = unsafe { supported_host_feature(ptr.cast()) };
            if supported { 1 } else { 0 }
        }
        host::GET_LANGUAGE => 1,
        host::GET_DIRECTORY => 0,
        host::UPDATE_DISPLAY => {
            context
                .authored_inventory_revision
                .fetch_add(1, Ordering::AcqRel);
            0
        }
        host::BEGIN_EDIT | host::END_EDIT => 0,
        _ => 0,
    }
}

unsafe fn supported_host_feature(ptr: *const u8) -> bool {
    if ptr.is_null() {
        return false;
    }
    // The longest accepted name is reportConnectionChanges (23 bytes). Longer
    // queries cannot match, so neither finding their NUL nor UTF-8 decoding is needed.
    let mut bytes = [0u8; b"reportConnectionChanges".len() + 1];
    for index in 0..bytes.len() {
        // SAFETY: The caller guarantees a native C string. Previous bytes were
        // non-NUL; this single byte is still within that string's readable span.
        let byte = unsafe { ptr.add(index).read() };
        if byte == 0 {
            return matches!(
                &bytes[..index],
                b"sendVstEvents"
                    | b"sendVstMidiEvent"
                    | b"sendVstTimeInfo"
                    | b"receiveVstEvents"
                    | b"receiveVstMidiEvent"
                    | b"receiveVstTimeInfo"
                    | b"reportConnectionChanges"
                    | b"acceptIOChanges"
                    | b"startStopProcess"
            );
        }
        bytes[index] = byte;
    }
    false
}

#[cfg(test)]
enum HostAccess {
    Routed(callback_route::Reader<'static>),
    Scoped(*const HostContext),
}
#[cfg(test)]
impl HostAccess {
    fn context(&self) -> &HostContext {
        match self {
            Self::Routed(reader) => reader,
            Self::Scoped(pointer) => {
                // SAFETY: Only the current native-call scope installs this
                // pointer. This private access is consumed synchronously inside
                // audio_master and cannot outlive that enclosing scope.
                unsafe { &**pointer }
            }
        }
    }
}
#[cfg(test)]
fn owned_host_context(effect: *mut abi::AEffect) -> Option<HostAccess> {
    if !effect.is_null() {
        match callback_route::lookup(effect as usize) {
            callback_route::Lookup::Ready(reader) => return Some(HostAccess::Routed(reader)),
            callback_route::Lookup::Unavailable => return None,
            callback_route::Lookup::Missing => {}
        }
        // Only native entry can legitimately supply a not-yet-registered
        // descriptor. A stale address must not borrow another active instance.
        ENTRY_HOST.with(|cell| {
            let pointer = cell.get();
            (!pointer.is_null()).then_some(HostAccess::Scoped(pointer))
        })
    } else {
        ACTIVE_HOST.with(|cell| {
            let pointer = cell.get();
            (!pointer.is_null()).then_some(HostAccess::Scoped(pointer))
        })
    }
}

/// Compatibility-only backing lookup; it does not validate native lifetime.
///
/// # Safety
/// A non-null effect must name a readable AEffect for this callback. Nonzero
/// reserved1 must be the HostContext installed by this host and still retained
/// by its instance. The returned reference must not escape audio_master despite
/// its legacy static signature; no validity after CLOSE is provided.
unsafe fn legacy_host_context(effect: *mut abi::AEffect) -> Option<&'static HostContext> {
    if !effect.is_null() {
        // SAFETY: The compatibility ABI caller supplies the live descriptor
        // required above. This read is not a test of pointer validity.
        let reserved = unsafe { (*effect).reserved1 };
        if reserved != 0 {
            // SAFETY: Only this host installs a nonzero reserved1 context;
            // the callback contract retains it and bounds this borrow's use.
            return Some(unsafe { &*(reserved as *const HostContext) });
        }
    }
    ACTIVE_HOST.with(|cell| {
        let pointer = cell.get();
        if pointer.is_null() {
            None
        } else {
            // SAFETY: ACTIVE_HOST is installed from retained HostContext
            // backing only for the enclosing native call on this same thread.
            // Its private caller consumes the reference before callback return.
            Some(unsafe { &*pointer })
        }
    })
}

enum CallbackDomain {
    Legacy,
    Owned {
        module: module_authority::ModuleAuthority,
        failure: Arc<Vst2Failure>,
    },
}

#[derive(Clone, Copy)]
enum LoadActivation {
    Active,
    Inactive,
}
enum CallbackBinding {
    Legacy,
    Owned(callback_route::RouteOwner),
}
impl CallbackBinding {
    fn callback(&self) -> Result<abi::AudioMasterCallback, String> {
        match self {
            Self::Legacy => Ok(audio_master),
            Self::Owned(route) => route.callback().map_err(|error| error.to_string()),
        }
    }
    fn bind(&mut self, effect: *mut abi::AEffect, host: &HostContext) -> Result<(), String> {
        match self {
            Self::Legacy => {
                // SAFETY: The admitted live descriptor keeps this legacy host
                // pointer until CLOSE, matching the compatibility callback ABI.
                unsafe { (*effect).reserved1 = std::ptr::from_ref(host) as isize };
                Ok(())
            }
            Self::Owned(route) => route
                .bind(effect as usize)
                .map_err(|error| error.to_string()),
        }
    }
    fn quarantine(&mut self) {
        if let Self::Owned(route) = self {
            route.quarantine();
        }
    }
    fn complete(&mut self) {
        if let Self::Owned(route) = self {
            route.complete();
        }
    }
    fn fault(&self) -> Option<callback_route::RouteFault> {
        match self {
            Self::Legacy => None,
            Self::Owned(route) => route.fault(),
        }
    }
}

/// A loaded VST2 plugin.
pub struct Vst2Instance {
    effect: *mut abi::AEffect,
    close_dispatcher: abi::DispatcherProc,
    host: Arc<HostContext>,
    binding: CallbackBinding,
    module: Option<module_authority::ModuleAuthority>,
    call_scratch: Option<Box<std::cell::UnsafeCell<call_scratch::CallScratch>>>,
    path: PathBuf,
    name: String,
    vendor: String,
    product: String,
    version: i32,
    category: PluginCategory,
    flags: i32,
    num_inputs: i32,
    num_outputs: i32,
    num_params: i32,
    param_names: Vec<String>,
    param_labels: Vec<String>,
    param_defaults: Vec<f64>,
    param_can_automate: Vec<bool>,
    sample_rate: u32,
    max_block_frames: u32,
    accepts_midi: bool,
    supports_double: bool,
    active: bool,
    pending_midi: Vec<PendingMidi>,
    midi_events: Vec<abi::VstMidiEvent>,
    event_pointers: Vec<*mut abi::VstEvent>,
    event_list: Vec<u8>,
    input_channels: Vec<Vec<f32>>,
    output_channels: Vec<Vec<f32>>,
    last_time_info: abi::VstTimeInfo,
    process_error: Option<Vst2ProcessError>,
    error_reported: std::cell::Cell<bool>,
}

// SAFETY: an instance owns its plugin and is moved between threads, never
// shared. The raw pointers inside belong to that single owner.
unsafe impl Send for Vst2Instance {}

impl Vst2Instance {
    /// Open `path` and prepare it for processing at `sample_rate`.
    ///
    /// `path` is a macOS `.vst` bundle, a Windows `.dll`, or a Linux `.so`.
    pub fn load(path: &Path, sample_rate: u32, max_frames: u32) -> Result<Self, String> {
        Self::load_domain(
            path,
            sample_rate,
            max_frames,
            CallbackDomain::Legacy,
            LoadActivation::Active,
        )
        .map(|candidate| candidate.into_legacy())
    }

    fn load_owned(
        token: &crate::application_main::UiThreadToken<'_>,
        path: &Path,
        sample_rate: u32,
        max_frames: u32,
    ) -> Result<candidate::OwnedCandidate, Vst2OwnedError> {
        Self::load_owned_mode(token, path, sample_rate, max_frames, LoadActivation::Active)
    }

    fn load_owned_mode(
        token: &crate::application_main::UiThreadToken<'_>,
        path: &Path,
        sample_rate: u32,
        max_frames: u32,
        activation: LoadActivation,
    ) -> Result<candidate::OwnedCandidate, Vst2OwnedError> {
        crate::application_main::ui_authority()?;
        if sample_rate == 0 {
            return Err(Vst2OwnedError::Native(
                "VST2 sample rate must be positive".into(),
            ));
        }
        checked_block_frames(max_frames as usize).map_err(|error| {
            Vst2OwnedError::Native(format!("VST2 block preparation refused: {error:?}"))
        })?;
        let executable = resolve_executable(path).map_err(Vst2OwnedError::Native)?;
        let module =
            module_authority::acquire(token, &executable).map_err(|error| match error {
                module_authority::ModuleError::Faulted(module) => {
                    Vst2OwnedError::Faulted(Arc::new(Vst2Failure::with_module(module)))
                }
                module_authority::ModuleError::Capacity => Vst2OwnedError::Capacity,
                error => Vst2OwnedError::Native(error.to_string()),
            })?;
        let failure = Arc::new(Vst2Failure::with_module(module.failure()));
        Self::load_domain(
            path,
            sample_rate,
            max_frames,
            CallbackDomain::Owned {
                module,
                failure: failure.clone(),
            },
            activation,
        )
        .map_err(|message| {
            if failure.failed() {
                Vst2OwnedError::Faulted(failure)
            } else {
                Vst2OwnedError::Native(message)
            }
        })
    }

    fn load_domain(
        path: &Path,
        sample_rate: u32,
        max_frames: u32,
        domain: CallbackDomain,
        activation: LoadActivation,
    ) -> Result<candidate::OwnedCandidate, String> {
        if sample_rate == 0 {
            return Err("VST2 sample rate must be positive".into());
        }
        let max_frames = checked_block_frames(max_frames as usize)
            .map_err(|error| format!("VST2 block preparation refused: {error:?}"))?;
        let executable = resolve_executable(path)?;
        let (entry, module, failure, _legacy_pin) = match domain {
            CallbackDomain::Legacy => {
                let library = DynamicLibrary::open(&executable)?;
                let entry = library.vst2_entry()?;
                (
                    entry,
                    None,
                    Arc::new(Vst2Failure::default()),
                    Some(library.into_process_pinned()),
                )
            }
            CallbackDomain::Owned { module, failure } => {
                (module.entry(), Some(module), failure, None)
            }
        };
        let owned = module.is_some();

        // The plugin may call back while the entry point runs; park the host
        // context where the callback can find it before the AEffect exists.
        let host = Arc::new(HostContext::with_failure(
            sample_rate,
            max_frames,
            0,
            failure.clone(),
        ));
        let mut binding = if owned {
            CallbackBinding::Owned(
                callback_route::RouteOwner::reserve(&host).map_err(|error| error.to_string())?,
            )
        } else {
            CallbackBinding::Legacy
        };
        let callback = binding.callback()?;
        let bootstrap_pointer = Arc::as_ptr(&host).cast_mut();
        let effect = ACTIVE_HOST.with(|cell| {
            let previous = cell.replace(bootstrap_pointer);
            let previous_entry = ENTRY_HOST.with(|entry| {
                entry.replace(if owned {
                    bootstrap_pointer
                } else {
                    std::ptr::null_mut()
                })
            });
            // SAFETY: Fixed-symbol lookup admitted the VSTPluginMain C ABI.
            // Its module is process-pinned; this call retains host backing and
            // scoped callbacks throughout entry. Owned calls use the typed C++
            // barrier below; the compatibility branch remains unchecked.
            let effect = if owned {
                if failure.failed() {
                    std::ptr::null_mut()
                } else {
                    // SAFETY: Verified pinned entry and fixed reservation thunk;
                    // the typed C++ call contains all native exception unwinding.
                    match unsafe { guard::entry(entry, callback) } {
                        Ok(effect) => effect,
                        Err(fault) => {
                            failure.record(Vst2Operation::Entry, fault);
                            std::ptr::null_mut()
                        }
                    }
                }
            } else {
                // SAFETY: Compatibility entry uses the existing unchecked ABI;
                // caller migration and its exception gap remain separate.
                unsafe { entry(callback) }
            };
            ENTRY_HOST.with(|entry| entry.set(previous_entry));
            cell.set(previous);
            effect
        });
        if owned && failure.failed() {
            return Err("VST2 entry image faulted".into());
        }
        failure.finish_entry();
        if effect.is_null() {
            binding.complete();
            return Err(format!(
                "VST2 entry point returned no plugin: {}",
                executable.display()
            ));
        }
        binding.bind(effect, &host)?;
        if owned && !effect.is_aligned() {
            failure.record(
                Vst2Operation::Entry,
                crate::native_fault::Fault::host_contract("Unaligned VST2 entry descriptor"),
            );
            return Err("VST2 entry descriptor is unaligned".into());
        }
        // SAFETY: VSTPluginMain's non-null-result contract supplies a readable
        // AEffect header until CLOSE. This is an ABI assumption, not a memory
        // validity guarantee established by the null/magic checks themselves.
        let magic = unsafe { (*effect).magic };
        if magic != abi::EFFECT_MAGIC {
            if owned {
                failure.record(
                    Vst2Operation::Entry,
                    crate::native_fault::Fault::host_contract(
                        "Invalid VST2 entry descriptor magic",
                    ),
                );
            }
            return Err(format!(
                "not a VST2 plugin (bad magic 0x{:08x}): {}",
                magic,
                executable.display()
            ));
        }
        // SAFETY: An admitted descriptor must expose its mandatory dispatcher
        // before the host can promise that normal teardown will close it.
        let close_dispatcher = unsafe { (*effect).dispatcher }.ok_or_else(|| {
            if owned {
                failure.record(
                    Vst2Operation::Entry,
                    crate::native_fault::Fault::host_contract("Missing VST2 dispatcher"),
                );
            }
            "VST2 plugin has no dispatcher".to_owned()
        })?;
        // SAFETY: This fixed scalar belongs to the admitted live header;
        // clamp its count before any parameter table indexing or iteration.
        let num_params = unsafe { (*effect).num_params }.clamp(0, MAX_REPORTED_PARAMS);
        host.num_params.store(num_params, Ordering::Release);
        // SAFETY: Copy the input count from that same retained ABI header;
        // layout bounds are checked below before allocating channel planes.
        let num_inputs = unsafe { (*effect).num_inputs };
        // SAFETY: Output count is another fixed header scalar and is checked
        // with the input count before channel allocation or processing.
        let num_outputs = unsafe { (*effect).num_outputs };
        // SAFETY: The returned live header owns this scalar capability mask;
        // reading it calls no foreign code and creates no native ownership.
        let flags = unsafe { (*effect).flags };
        if !(0..=MAX_CHANNELS).contains(&num_inputs) || !(0..=MAX_CHANNELS).contains(&num_outputs) {
            // SAFETY: The admitted original dispatcher owns teardown even if
            // later metadata callbacks change the descriptor's callback fields.
            let closed = if owned {
                guarded_close(effect, close_dispatcher, &host, false)
            } else {
                // SAFETY: The failed candidate retains its admitted effect, original dispatcher and host until this one synchronous close completes.
                unsafe { close_effect(effect, close_dispatcher, &host, false) };
                true
            };
            if closed {
                binding.complete();
            }
            return Err(format!(
                "VST2 plugin reports unsupported channel layout ({num_inputs} in / {num_outputs} out): {}",
                executable.display()
            ));
        }

        let mut instance = candidate::OwnedCandidate::new(Box::new(Self {
            effect,
            close_dispatcher,
            host,
            binding,
            module,
            call_scratch: if owned {
                Some(call_scratch::CallScratch::new())
            } else {
                None
            },
            path: executable,
            name: String::new(),
            vendor: String::new(),
            product: String::new(),
            version: 0,
            category: PluginCategory::Effect,
            flags,
            num_inputs,
            num_outputs,
            num_params,
            param_names: Vec::new(),
            param_labels: Vec::new(),
            param_defaults: Vec::new(),
            param_can_automate: Vec::new(),
            sample_rate,
            max_block_frames: max_frames,
            accepts_midi: false,
            supports_double: false,
            active: false,
            pending_midi: Vec::new(),
            midi_events: Vec::new(),
            event_pointers: Vec::new(),
            event_list: Vec::new(),
            input_channels: Vec::new(),
            output_channels: Vec::new(),
            last_time_info: abi::VstTimeInfo::default(),
            process_error: None,
            error_reported: std::cell::Cell::new(false),
        }));

        instance.dispatch(op::OPEN, 0, 0, std::ptr::null_mut());
        // The identity handshake is optional, but 2.0-era plugins expect it.
        instance.dispatch(
            op::IDENTIFY,
            0,
            abi::IDENTIFY_CHALLENGE,
            std::ptr::null_mut(),
        );
        instance
            .set_sample_rate(sample_rate)
            .map_err(|_| "VST2 sample rate must be positive".to_owned())?;
        instance.set_block_size(max_frames);

        instance.name = instance.query_string(op::GET_EFFECT_NAME);
        if instance.name.is_empty() {
            instance.name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Unknown VST2")
                .to_string();
        }
        instance.vendor = instance.query_string(op::GET_VENDOR_STRING);
        instance.product = instance.query_string(op::GET_PRODUCT_STRING);
        instance.version =
            instance.dispatch(op::GET_VENDOR_VERSION, 0, 0, std::ptr::null_mut()) as i32;
        instance.category = category_from_plug_category(instance.dispatch(
            op::GET_PLUG_CATEGORY,
            0,
            0,
            std::ptr::null_mut(),
        ) as i32);
        if flags & abi::FLAG_IS_SYNTH != 0 {
            instance.category = PluginCategory::Instrument;
        }
        instance.accepts_midi = instance.can_do(abi::can_do::RECEIVE_VST_MIDI_EVENT)
            || instance.can_do(abi::can_do::RECEIVE_VST_EVENTS);
        instance.supports_double = instance.can_do(abi::can_do::SUPPORTS_DOUBLE_PRECISION);

        instance.read_param_metadata();
        instance
            .check_callback_route()
            .map_err(|error| error.to_string())?;
        instance.host.latency.store(
            // SAFETY: The instance retains its admitted descriptor after OPEN
            // and metadata calls. Clamp its signed delay before host storage.
            unsafe { (*instance.effect).initial_delay }.max(0) as u32,
            Ordering::Relaxed,
        );
        instance
            .check_callback_route()
            .map_err(|error| error.to_string())?;
        if matches!(activation, LoadActivation::Active) {
            instance.activate();
        }
        instance
            .prepare_block_buffers(max_frames as usize)
            .map_err(|error| format!("VST2 preparation failed: {error:?}"))?;
        Ok(instance)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    pub fn product(&self) -> &str {
        &self.product
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Vendor integer version, e.g. 10300.
    pub fn version(&self) -> i32 {
        self.version
    }

    pub fn category(&self) -> PluginCategory {
        self.category
    }

    pub fn is_instrument(&self) -> bool {
        self.category == PluginCategory::Instrument
    }

    pub fn num_inputs(&self) -> i32 {
        self.num_inputs
    }

    pub fn num_outputs(&self) -> i32 {
        self.num_outputs
    }

    pub fn param_count(&self) -> i32 {
        self.num_params
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn accepts_midi(&self) -> bool {
        self.accepts_midi
    }

    pub fn supports_double_precision(&self) -> bool {
        self.supports_double
    }

    /// Host timeline the plugin saw during its last process call.
    pub fn last_time_info(&self) -> abi::VstTimeInfo {
        self.last_time_info
    }

    pub fn latency_samples(&self) -> u32 {
        self.host.latency.load(Ordering::Relaxed)
    }

    /// Release tail reported by the plugin, in samples (0 when unknown).
    pub fn tail_samples(&self) -> u32 {
        self.plugin_tail_samples().unwrap_or(0)
    }

    /// Declared tail, or `None` when the plugin reports an unknown or
    /// unbounded release.
    pub fn plugin_tail_samples(&self) -> Option<u32> {
        let reported = self.dispatch(op::GET_TAIL_SAMPLES, 0, 0, std::ptr::null_mut()) as i32;
        // 1 means "unbounded" in the 2.x interface; the host cannot wait for it.
        if reported <= 1 {
            None
        } else {
            Some(reported as u32)
        }
    }

    /// VST2 has one stereo output pair; auxiliary pairs are a VST3 feature.
    pub fn output_count(&self) -> usize {
        1
    }

    /// Declared audio output ports; VST2 exposes the one stereo pair.
    pub fn output_ports(&self) -> Vec<AudioPort> {
        vec![AudioPort::STEREO]
    }

    /// Declared audio input ports; effects take the main stereo pair.
    pub fn input_ports(&self) -> Vec<AudioPort> {
        if self.flags & abi::FLAG_IS_SYNTH == 0 {
            vec![AudioPort::STEREO]
        } else {
            Vec::new()
        }
    }

    fn is_owned(&self) -> bool {
        self.module.is_some()
    }
    fn record_call<T>(
        &self,
        operation: Vst2Operation,
        result: Result<T, crate::native_fault::Fault>,
    ) -> Option<T> {
        match result {
            Ok(value) if !self.host.failure.failed() => Some(value),
            Ok(_) => None,
            Err(fault) => {
                self.host.failure.record(operation, fault);
                None
            }
        }
    }
    fn write_parameter_callback(&self, callback: abi::SetParameterProc, index: i32, value: f32) {
        if self.is_owned() {
            if self.host.failure.failed() {
                return;
            }
            // SAFETY: The caller owns the admitted setter/index and exclusive
            // control; the typed shim catches native unwinding before Rust.
            self.record_call(Vst2Operation::SetParameter(index), unsafe {
                guard::set(callback, self.effect, index, value)
            });
        } else {
            // SAFETY: Compatibility behavior retains its unchecked setter ABI;
            // this path is not the owned native-fault authority.
            unsafe { callback(self.effect, index, value) };
        }
    }
    fn retire_owned(
        &mut self,
        _token: &crate::application_main::UiRetirementToken<'_>,
    ) -> Result<(), Arc<Vst2Failure>> {
        if self.effect.is_null() {
            return Ok(());
        }
        if self.host.failure.failed() {
            self.binding.quarantine();
            return Err(self.host.failure.clone());
        }
        let host = Arc::as_ptr(&self.host).cast_mut();
        let healthy = ACTIVE_HOST.with(|cell| {
            let previous = cell.replace(host);
            let healthy =
                guarded_close(self.effect, self.close_dispatcher, &self.host, self.active);
            cell.set(previous);
            healthy
        });
        if !healthy {
            self.binding.quarantine();
            return Err(self.host.failure.clone());
        }
        self.active = false;
        self.effect = std::ptr::null_mut();
        self.binding.complete();
        Ok(())
    }

    fn owned_scratch(&self) -> Option<*mut call_scratch::CallScratch> {
        self.call_scratch.as_ref().map(|packet| packet.get())
    }

    /// Editor rectangle reported without creating a window.
    pub fn editor_size(&self) -> Option<(i32, i32)> {
        if self.is_owned() && self.host.failure.failed() {
            return None;
        }
        if self.flags & abi::FLAG_HAS_EDITOR == 0 {
            return None;
        }
        let mut rect: *mut abi::ERect = std::ptr::null_mut();
        let output = if let Some(packet) = self.owned_scratch() {
            // SAFETY: Owned UI control is exclusive; the retained packet has no
            // overlapping metadata loan and remains allocated through retirement.
            unsafe {
                (*packet).rectangle_output = std::ptr::null_mut();
                std::ptr::addr_of_mut!((*packet).rectangle_output).cast()
            }
        } else {
            (&mut rect as *mut *mut abi::ERect).cast()
        };
        let status = self.dispatch(op::EDIT_GET_RECT, 0, 0, output);
        if self.host.failure.failed() {
            return None;
        }
        if let Some(packet) = self.owned_scratch() {
            // SAFETY: The same exclusive output loan ended with normal return.
            rect = unsafe { (*packet).rectangle_output };
        }
        if status <= 0 || rect.is_null() || !rect.is_aligned() {
            return None;
        }
        // SAFETY: a successful effEditGetRect supplies a borrowed ERect with
        // the SDK layout, retained by the live plugin until its next dispatcher call.
        // Null/refused/misaligned outputs are rejected before this read.
        let rect = unsafe { &*rect };
        let width = i32::from(rect.right) - i32::from(rect.left);
        let height = i32::from(rect.bottom) - i32::from(rect.top);
        (width > 0 && height > 0).then_some((width, height))
    }

    /// Editor hosting is not implemented: the editor path needs a native child
    /// window and a platform run loop.
    pub fn open_editor(&mut self) -> Result<(u32, u32), String> {
        Err("VST2 plugin editors are not supported in this build".into())
    }

    /// Tell the plugin its editor closed. Safe to call when no editor opened.
    pub fn close_editor(&mut self) {
        if self.flags & abi::FLAG_HAS_EDITOR != 0 {
            self.dispatch(op::EDIT_CLOSE, 0, 0, std::ptr::null_mut());
        }
    }

    /// Editor repaint tick; only meaningful while an editor is open.
    pub fn idle_editor(&mut self) {
        if self.flags & abi::FLAG_HAS_EDITOR != 0 {
            self.dispatch(op::EDIT_IDLE, 0, 0, std::ptr::null_mut());
        }
    }

    /// The generic panel's view of every parameter this plugin exposes.
    ///
    /// VST2 reports no factory default, so `default` stays `None`; the panel
    /// falls back to the value the instance had when it opened.
    pub fn parameter_surface(&self) -> crate::params::ParamSurface {
        let descriptors = self
            .get_params()
            .iter()
            .map(|param| descriptor(param, self.param_steps(param.index)))
            .collect();
        crate::params::surface(descriptors)
    }

    /// Step count from the optional 2.4 parameter descriptor.
    fn param_steps(&self, index: i32) -> Option<u32> {
        if self.is_owned() && self.host.failure.failed() {
            return None;
        }
        if !(0..self.num_params).contains(&index) {
            return None;
        }
        let mut properties = abi::VstParameterProperties {
            step_float: 0.0,
            small_step_float: 0.0,
            large_step_float: 0.0,
            label: [0; 64],
            flags: 0,
            min_integer: 0,
            max_integer: 0,
            step_integer: 0,
            large_step_integer: 0,
            short_label: [0; 8],
            display_index: 0,
            category: 0,
            num_parameters_in_category: 0,
            reserved: 0,
            category_label: [0; 24],
            future: [0; 16],
        };
        let output = if let Some(packet) = self.owned_scratch() {
            // SAFETY: Exclusive owned metadata control prepares one retained
            // property output; no reference into this packet escapes the call.
            unsafe {
                (*packet).properties = properties;
                std::ptr::addr_of_mut!((*packet).properties).cast()
            }
        } else {
            (&mut properties as *mut abi::VstParameterProperties).cast()
        };
        let supported = self.dispatch(op::GET_PARAMETER_PROPERTIES, index, 0, output);
        if self.host.failure.failed() {
            return None;
        }
        if let Some(packet) = self.owned_scratch() {
            // SAFETY: The retained output was initialized before the native
            // call; consume its Copy record only after a nonfaulted return.
            properties = unsafe { (*packet).properties };
        }
        if supported == 0 {
            return None;
        }
        use abi::parameter_flags as flags;
        if properties.flags & flags::USES_INTEGER_MIN_MAX != 0 {
            return crate::params::integer_steps(
                f64::from(properties.min_integer),
                f64::from(properties.max_integer),
            );
        }
        if properties.flags & flags::IS_SWITCH != 0 {
            return Some(2);
        }
        None
    }

    pub fn get_params(&self) -> Vec<Vst2Param> {
        self.param_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let index = index as i32;
                let value = self
                    .host
                    .value(index)
                    .map(f64::from)
                    .unwrap_or_else(|| f64::from(self.read_param(index)));
                Vst2Param {
                    index,
                    name: name.clone(),
                    label: self
                        .param_labels
                        .get(index as usize)
                        .cloned()
                        .unwrap_or_default(),
                    display: self.query_indexed_string(op::GET_PARAM_DISPLAY, index),
                    value,
                    default: self
                        .param_defaults
                        .get(index as usize)
                        .copied()
                        .unwrap_or(0.0),
                    can_automate: self
                        .param_can_automate
                        .get(index as usize)
                        .copied()
                        .unwrap_or(true),
                }
            })
            .collect()
    }

    pub fn get_param_by_name(&self, name: &str) -> Option<f64> {
        let index = self.param_index(name)?;
        self.host.value(index).map(f64::from)
    }

    /// Set a normalized 0..1 parameter by name, id or index.
    pub fn set_param_by_name(&mut self, name: &str, value: f64) -> bool {
        if self.check_callback_route().is_err() {
            return false;
        }
        let Some(index) = self.param_index(name) else {
            return false;
        };
        self.set_param_index(index, value);
        true
    }

    pub fn set_param_index(&mut self, index: i32, value: f64) {
        if self.check_callback_route().is_err() {
            return;
        }
        if !(0..self.num_params).contains(&index) || !value.is_finite() {
            return;
        }
        let value = value.clamp(0.0, 1.0) as f32;
        // SAFETY: Callback admission succeeded and this retained live descriptor remains borrowed while its setter pointer is copied.
        if let Some(set) = unsafe { (*self.effect).set_parameter } {
            self.write_parameter_callback(set, index, value);
        }
        if !self.is_owned() || !self.host.failure.failed() {
            self.host.store_value(index, value);
        }
    }

    /// Bank state blob (`effGetChunk` index 0).
    pub fn state_bytes(&self) -> Result<Vec<u8>, String> {
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        if self.flags & abi::FLAG_PROGRAM_CHUNKS == 0 {
            return Err(format!(
                "VST2 plugin '{}' does not expose program chunks",
                self.name
            ));
        }
        let mut pointer: *mut c_void = std::ptr::null_mut();
        let output = if let Some(packet) = self.owned_scratch() {
            // SAFETY: This UI-exclusive output slot belongs to the stable call
            // packet and is retained even if native ownership becomes uncertain.
            unsafe {
                (*packet).chunk_output = std::ptr::null_mut();
                std::ptr::addr_of_mut!((*packet).chunk_output).cast()
            }
        } else {
            (&mut pointer as *mut *mut c_void).cast()
        };
        let size = self.dispatch(op::GET_CHUNK, 0, 0, output);
        if self.host.failure.failed() {
            return Err("VST2 native ownership is faulted".into());
        }
        if let Some(packet) = self.owned_scratch() {
            // SAFETY: This output loan has returned without a recorded fault.
            pointer = unsafe { (*packet).chunk_output };
        }
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        if size <= 0 || pointer.is_null() {
            return Err(format!(
                "VST2 plugin '{}' returned an empty state chunk",
                self.name
            ));
        }
        let size = usize::try_from(size).map_err(|_| "VST2 state size is out of range")?;
        checked_state_chunk_size(size)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(size)
            .map_err(|_| "could not allocate bounded VST2 state")?;
        // SAFETY: a positive effGetChunk byte count and non-null pointer describe
        // a plugin-owned readable chunk, borrowed until the next dispatcher call. Copy
        // while this retained instance is still borrowed; no ownership is taken from the plugin.
        let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, size) };
        output.extend_from_slice(bytes);
        Ok(output)
    }

    pub fn set_state_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        checked_state_chunk_size(bytes.len())?;
        if self.flags & abi::FLAG_PROGRAM_CHUNKS == 0 {
            return Err(format!(
                "VST2 plugin '{}' does not expose program chunks",
                self.name
            ));
        }
        let input = if let Some(packet) = self.owned_scratch() {
            // SAFETY: Exclusive UI state control prepares a bounded retained
            // input copy before lending it, never a caller-owned temporary.
            let packet = unsafe { &mut *packet };
            packet.prepare_state_input(bytes)?;
            packet.state_input.as_ptr().cast_mut().cast()
        } else {
            bytes.as_ptr().cast_mut().cast()
        };
        let result = self.dispatch(op::SET_CHUNK, 0, bytes.len() as isize, input);
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        if result != 0 {
            return Err(format!(
                "VST2 plugin '{}' rejected its state chunk",
                self.name
            ));
        }
        // Keep the host mirror in step with the restored bank.
        for index in 0..self.num_params {
            self.host.store_value(index, self.read_param(index));
        }
        Ok(())
    }

    pub fn note_on(&mut self, key: u8, velocity: f32, sample_offset: i32) {
        self.note_on_channel(0, key, velocity, sample_offset);
    }

    pub fn note_on_channel(&mut self, channel: u8, key: u8, velocity: f32, sample_offset: i32) {
        let velocity = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
        self.queue_midi(
            abi::midi_status::NOTE_ON | (channel & 0x0f),
            key,
            velocity,
            sample_offset,
        );
    }

    pub fn note_off(&mut self, key: u8, sample_offset: i32) {
        self.note_off_channel(0, key, sample_offset);
    }

    pub fn note_off_channel(&mut self, channel: u8, key: u8, sample_offset: i32) {
        self.queue_midi(
            abi::midi_status::NOTE_OFF | (channel & 0x0f),
            key,
            0,
            sample_offset,
        );
    }

    pub fn control_change(&mut self, controller: u8, value: u8, sample_offset: i32) {
        self.control_change_channel(0, controller, value, sample_offset);
    }

    pub fn control_change_channel(
        &mut self,
        channel: u8,
        controller: u8,
        value: u8,
        sample_offset: i32,
    ) {
        self.queue_midi(
            abi::midi_status::CONTROL_CHANGE | (channel & 0x0f),
            controller,
            value,
            sample_offset,
        );
    }

    pub fn program_change(&mut self, program: u8, sample_offset: i32) {
        self.program_change_channel(0, program, sample_offset);
    }

    pub fn program_change_channel(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.queue_midi(
            abi::midi_status::PROGRAM_CHANGE | (channel & 0x0f),
            program,
            0,
            sample_offset,
        );
    }

    pub fn channel_pressure(&mut self, pressure: u8, sample_offset: i32) {
        self.channel_pressure_channel(0, pressure, sample_offset);
    }

    pub fn channel_pressure_channel(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.queue_midi(
            abi::midi_status::CHANNEL_PRESSURE | (channel & 0x0f),
            pressure,
            0,
            sample_offset,
        );
    }

    pub fn poly_pressure(&mut self, key: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure_channel(0, key, pressure, sample_offset);
    }

    pub fn poly_pressure_channel(
        &mut self,
        channel: u8,
        key: u8,
        pressure: u8,
        sample_offset: i32,
    ) {
        self.queue_midi(
            abi::midi_status::POLY_PRESSURE | (channel & 0x0f),
            key,
            pressure,
            sample_offset,
        );
    }

    pub fn pitch_bend(&mut self, value: i16, sample_offset: i32) {
        self.pitch_bend_channel(0, value, sample_offset);
    }

    pub fn pitch_bend_channel(&mut self, channel: u8, value: i16, sample_offset: i32) {
        let raw = (i32::from(value) + 8192).clamp(0, 16383) as u16;
        self.queue_midi(
            abi::midi_status::PITCH_BEND | (channel & 0x0f),
            (raw & 0x7f) as u8,
            (raw >> 7) as u8,
            sample_offset,
        );
    }

    /// Queue one channel-voice MIDI message.
    pub fn queue_midi(&mut self, status: u8, data1: u8, data2: u8, sample_offset: i32) {
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return;
        }
        if self.pending_midi.len() == MAX_PENDING_MIDI {
            self.note_process_error(Vst2ProcessError::EventCapacityExceeded);
            return;
        }
        self.pending_midi.push(PendingMidi {
            status,
            data1: data1 & 0x7f,
            data2: data2 & 0x7f,
            offset: sample_offset.max(0),
        });
    }

    fn check_callback_route(&self) -> Result<(), Vst2ProcessError> {
        if self.module.is_some() && self.host.failure.failed() {
            return Err(Vst2ProcessError::NativeFault);
        }
        match self.binding.fault() {
            Some(callback_route::RouteFault::AdmissionOverflow) => {
                Err(Vst2ProcessError::CallbackAdmissionOverflow)
            }
            None => Ok(()),
        }
    }

    fn note_process_error(&mut self, error: Vst2ProcessError) {
        if self.process_error.is_none() {
            self.process_error = Some(error);
            self.error_reported.set(false);
        }
    }

    /// Reserve all host-owned audio and MIDI scratch outside processing.
    /// Parameters, queued MIDI and plugin signal history are preserved. An
    /// explicit preparation also accepts a pending topology notification.
    pub fn prepare_block_buffers(
        &mut self,
        max_frames: usize,
    ) -> Result<(), oc_audio::BlockPreparationError> {
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return Err(oc_audio::BlockPreparationError::CapacityExceeded);
        }
        let frames = checked_block_frames(max_frames)?;
        if max_frames > isize::MAX as usize / std::mem::size_of::<f32>() {
            return Err(oc_audio::BlockPreparationError::CapacityExceeded);
        }
        if self.host.io_changed.swap(false, Ordering::AcqRel)
            && let Err(error) = self.refresh_io()
        {
            self.host.io_changed.store(true, Ordering::Release);
            return Err(error);
        }
        if self.input_channels.len() != self.num_inputs.max(1) as usize
            || self.output_channels.len() != self.num_outputs.max(0) as usize
        {
            self.allocate_channels();
        }
        fn reserve<T>(
            values: &mut Vec<T>,
            count: usize,
        ) -> Result<(), oc_audio::BlockPreparationError> {
            if values.capacity() < count {
                values
                    .try_reserve_exact(count - values.len())
                    .map_err(|_| oc_audio::BlockPreparationError::CapacityExceeded)?;
            }
            Ok(())
        }
        let frames = frames.max(self.max_block_frames);
        for channel in self
            .input_channels
            .iter_mut()
            .chain(&mut self.output_channels)
        {
            reserve(channel, frames as usize)?;
        }
        reserve(&mut self.pending_midi, MAX_PENDING_MIDI)?;
        reserve(&mut self.midi_events, MAX_PENDING_MIDI)?;
        reserve(&mut self.event_pointers, MAX_PENDING_MIDI)?;
        reserve(
            &mut self.event_list,
            abi::VST_EVENTS_POINTER_OFFSET
                + MAX_PENDING_MIDI * std::mem::size_of::<*mut abi::VstEvent>(),
        )?;
        if frames > self.max_block_frames {
            self.set_block_size(frames);
        }
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return Err(oc_audio::BlockPreparationError::CapacityExceeded);
        }
        // Overflow means events were lost; only a reset can recover it.
        if self.process_error != Some(Vst2ProcessError::EventCapacityExceeded) {
            self.process_error = None;
            self.error_reported.set(false);
        }
        Ok(())
    }

    fn preflight_block(&self, samples: usize, sample_rate: u32) -> Result<(), Vst2ProcessError> {
        if let Some(error) = self.process_error {
            return Err(error);
        }
        self.check_callback_route()?;
        if !samples.is_multiple_of(2) {
            return Err(Vst2ProcessError::IncompleteFrame);
        }
        let frames = samples / 2;
        if sample_rate != self.sample_rate {
            return Err(Vst2ProcessError::SampleRateChanged);
        }
        if self.host.io_changed.load(Ordering::Acquire)
            || self.input_channels.len() != self.num_inputs.max(1) as usize
            || self.output_channels.len() != self.num_outputs.max(0) as usize
        {
            return Err(Vst2ProcessError::TopologyChanged);
        }
        if frames > self.max_block_frames as usize
            || self
                .input_channels
                .iter()
                .chain(&self.output_channels)
                .any(|channel| channel.capacity() < frames)
        {
            return Err(Vst2ProcessError::CapacityExceeded);
        }
        if self.active
            // SAFETY: the retained instance keeps its admitted AEffect readable for
            // this exclusive prepared-block check; inspecting callback fields calls no native code.
            && unsafe {
                (*self.effect).process_replacing.is_none()
                    || self.flags & abi::FLAG_CAN_REPLACING == 0
            }
            // SAFETY: this same retained descriptor stays readable while checking
            // the fallback callback slot; the pointer is not dereferenced after effClose.
            && unsafe { (*self.effect).process.is_none() }
        {
            return Err(Vst2ProcessError::UnavailableProcessor);
        }
        Ok(())
    }

    /// Render one prepared block to caller-owned stereo storage. Rejection
    /// before processing preserves the destination, MIDI queue and plugin DSP.
    /// A topology change raised during processing discards that block instead
    /// of publishing audio from a stale layout.
    pub fn process_block_into(
        &mut self,
        output: &mut [f32],
        transport: Option<PluginTransport>,
    ) -> Result<(), Vst2ProcessError> {
        self.process_prepared(output, self.sample_rate, transport.as_ref(), false)
    }

    pub fn process_effect_into(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) -> Result<(), Vst2ProcessError> {
        self.process_prepared(buffer, sample_rate, transport.as_ref(), true)
    }

    fn process_prepared(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<&PluginTransport>,
        input: bool,
    ) -> Result<(), Vst2ProcessError> {
        if let Err(error) = self.preflight_block(buffer.len(), sample_rate) {
            self.note_process_error(error);
            return Err(error);
        }
        let frames = buffer.len() / 2;
        if frames == 0 {
            return Ok(());
        }
        self.prepare(frames, transport);
        self.process_audio(buffer, input)
    }

    fn process_audio(&mut self, buffer: &mut [f32], input: bool) -> Result<(), Vst2ProcessError> {
        let frames = buffer.len() / 2;
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return Err(error);
        }
        if self.host.io_changed.load(Ordering::Acquire) {
            self.note_process_error(Vst2ProcessError::TopologyChanged);
            return Err(Vst2ProcessError::TopologyChanged);
        }
        if input {
            for frame in 0..frames {
                self.input_channels[0][frame] = buffer[frame * 2];
                if self.input_channels.len() > 1 {
                    self.input_channels[1][frame] = buffer[frame * 2 + 1];
                }
            }
        }
        self.run_process(frames);
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return Err(error);
        }
        if self.host.io_changed.load(Ordering::Acquire) {
            self.note_process_error(Vst2ProcessError::TopologyChanged);
            return Err(Vst2ProcessError::TopologyChanged);
        }
        self.read_output(frames, buffer);
        Ok(())
    }

    /// Render one block into an interleaved stereo buffer. Requests above the
    /// preparation limit record CapacityExceeded and return an empty buffer
    /// before allocating caller-sized output.
    pub fn process_block_with_transport(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        let frames = num_frames as usize;
        if checked_block_frames(frames).is_err() {
            self.note_process_error(Vst2ProcessError::CapacityExceeded);
            return Vec::new();
        }
        let mut output = vec![0.0; frames * 2];
        if self.prepare_block_buffers(frames).is_err() {
            self.note_process_error(Vst2ProcessError::CapacityExceeded);
            return output;
        }
        let _ = self.process_block_into(&mut output, transport);
        output
    }

    /// Process an interleaved buffer in place (effect path).
    pub fn process_effect_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        if let Err(error) = self.ensure_sample_rate(sample_rate) {
            self.note_process_error(error);
            return;
        }
        if self.prepare_block_buffers(buffer.len() / 2).is_err() {
            self.note_process_error(Vst2ProcessError::CapacityExceeded);
            return;
        }
        let _ = self.process_effect_into(buffer, sample_rate, transport);
    }

    /// Deactivate processing; the instance renders silence until reactivated.
    pub fn deactivate(&mut self) {
        if self.active {
            self.dispatch(op::MAINS_CHANGED, 0, 0, std::ptr::null_mut());
            self.active = false;
        }
    }

    /// Return the effect to its initial state: drop queued events and run the
    /// suspend/resume cycle every VST2 host uses to reset a plugin.
    pub fn reset(&mut self) {
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return;
        }
        self.process_error = None;
        self.error_reported.set(false);
        self.pending_midi.clear();
        self.deactivate();
        self.activate();
    }

    fn activate(&mut self) {
        if self.check_callback_route().is_err() {
            return;
        }
        if !self.active {
            self.dispatch(op::MAINS_CHANGED, 0, 1, std::ptr::null_mut());
            if !self.is_owned() || !self.host.failure.failed() {
                self.active = true;
            }
        }
    }

    fn set_sample_rate(&mut self, sample_rate: u32) -> Result<(), Vst2ProcessError> {
        self.check_callback_route()?;
        if sample_rate == 0 {
            return Err(Vst2ProcessError::SampleRateChanged);
        }
        self.sample_rate = sample_rate;
        self.host.sample_rate.store(sample_rate, Ordering::Relaxed);
        self.host.set_time(None);
        // VST2 passes the rate as the float argument of the same opcode.
        self.dispatch_float(
            op::SET_SAMPLE_RATE,
            0,
            0,
            std::ptr::null_mut(),
            sample_rate as f32,
        );
        Ok(())
    }

    fn set_block_size(&mut self, frames: u32) {
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return;
        }
        self.max_block_frames = frames.max(1);
        self.host
            .block_size
            .store(self.max_block_frames, Ordering::Relaxed);
        self.dispatch(
            op::SET_BLOCK_SIZE,
            0,
            self.max_block_frames as isize,
            std::ptr::null_mut(),
        );
    }

    fn ensure_sample_rate(&mut self, sample_rate: u32) -> Result<(), Vst2ProcessError> {
        if sample_rate == 0 || sample_rate != self.sample_rate {
            self.set_sample_rate(sample_rate)
        } else {
            Ok(())
        }
    }

    fn allocate_channels(&mut self) {
        let inputs = self.num_inputs.max(1) as usize;
        // Always hand the plugin valid channel pointers, even for a synth
        // declaring zero inputs: some plugins read inputs[0] unconditionally.
        self.input_channels = (0..inputs).map(|_| Vec::new()).collect();
        self.output_channels = (0..self.num_outputs.max(0) as usize)
            .map(|_| Vec::new())
            .collect();
    }

    fn prepare(&mut self, frames: usize, transport: Option<&PluginTransport>) {
        self.prepare_without_events(frames, transport);
        self.send_events();
    }

    fn prepare_without_events(&mut self, frames: usize, transport: Option<&PluginTransport>) {
        if frames as u32 > self.max_block_frames {
            self.set_block_size(frames as u32);
        }
        self.allocate_block(frames);
        for channel in &mut self.input_channels {
            channel.fill(0.0);
        }
        self.last_time_info = self.host.set_time(transport);
    }

    fn allocate_block(&mut self, frames: usize) {
        if self.output_channels.len() != self.num_outputs.max(0) as usize {
            self.allocate_channels();
        }
        for channel in &mut self.input_channels {
            if channel.len() != frames {
                channel.resize(frames, 0.0);
            }
        }
        for channel in &mut self.output_channels {
            channel.clear();
            channel.resize(frames, 0.0);
        }
    }

    fn send_events(&mut self) {
        if self.pending_midi.is_empty() {
            return;
        }
        self.midi_events.clear();
        self.event_pointers.clear();
        self.midi_events
            .extend(self.pending_midi.drain(..).map(|event| {
                abi::VstMidiEvent::new([event.status, event.data1, event.data2, 0], event.offset)
            }));
        self.event_pointers.extend(
            self.midi_events
                .iter_mut()
                .map(|event| event as *mut abi::VstMidiEvent as *mut abi::VstEvent),
        );

        let count = self.event_pointers.len();
        let header = abi::VST_EVENTS_POINTER_OFFSET;
        self.event_list.clear();
        self.event_list.resize(
            header + count * std::mem::size_of::<*mut abi::VstEvent>(),
            0,
        );
        self.event_list[0..4].copy_from_slice(&(count as i32).to_ne_bytes());
        for (index, pointer) in self.event_pointers.iter().enumerate() {
            let offset = header + index * std::mem::size_of::<*mut abi::VstEvent>();
            self.event_list[offset..offset + std::mem::size_of::<usize>()]
                .copy_from_slice(&(*pointer as usize).to_ne_bytes());
        }
        if self.accepts_midi {
            let events = self.event_list.as_mut_ptr() as *mut c_void;
            self.dispatch(op::PROCESS_EVENTS, 0, 0, events);
        }
    }

    fn run_process(&mut self, frames: usize) {
        if !self.active {
            for channel in &mut self.output_channels {
                channel.fill(0.0);
            }
            return;
        }
        let effect = self.effect;
        let can_replacing = self.flags & abi::FLAG_CAN_REPLACING != 0;
        // SAFETY: the live instance retains its admitted AEffect and pinned code
        // while taking a copy of the replacing callback pointer; no foreign method is called here.
        let replacing = unsafe { (*effect).process_replacing };
        // SAFETY: the same retained descriptor supplies the optional accumulating
        // callback slot; native code remains process-pinned for any copied function pointer.
        let accumulating = unsafe { (*effect).process };
        let use_replacing = can_replacing && replacing.is_some();
        if !use_replacing && accumulating.is_none() {
            for channel in &mut self.output_channels {
                channel.fill(0.0);
            }
            return;
        }
        if !use_replacing {
            // Accumulating mode adds into the output bus; start from silence.
            for channel in &mut self.output_channels {
                channel.fill(0.0);
            }
        }
        // Fixed pointer arrays keep the audio path allocation-free.
        let mut input_pointers: [*const f32; MAX_CHANNELS as usize] =
            [std::ptr::null(); MAX_CHANNELS as usize];
        for (slot, channel) in input_pointers.iter_mut().zip(&self.input_channels) {
            *slot = channel.as_ptr();
        }
        let mut output_pointers: [*mut f32; MAX_CHANNELS as usize] =
            [std::ptr::null_mut(); MAX_CHANNELS as usize];
        for (slot, channel) in output_pointers.iter_mut().zip(&mut self.output_channels) {
            *slot = channel.as_mut_ptr();
        }
        let (input_array, output_array) = if let Some(packet) = self.owned_scratch() {
            // SAFETY: The exclusive prepared process permit owns this retained
            // argument packet. Raw arrays outlive the native call/fault and no
            // Rust reference to them remains live while native code executes.
            unsafe {
                (*packet).input_pointers = input_pointers;
                (*packet).output_pointers = output_pointers;
                (
                    (*packet).input_pointers.as_ptr(),
                    (*packet).output_pointers.as_mut_ptr(),
                )
            }
        } else {
            (input_pointers.as_ptr(), output_pointers.as_mut_ptr())
        };
        let inputs = if self.num_inputs == 0 {
            std::ptr::null()
        } else {
            input_array
        };
        let outputs = if self.output_channels.is_empty() {
            std::ptr::null_mut()
        } else {
            output_array
        };
        let host = &*self.host as *const HostContext as *mut HostContext;
        self.host
            .process_level
            .store(abi::process_level::REALTIME, Ordering::Relaxed);
        ACTIVE_HOST.with(|cell| {
            let previous = cell.replace(host);
            if let Some(process) = if use_replacing {
                replacing
            } else {
                accumulating
            } {
                if self.is_owned() {
                    if !self.host.failure.failed() {
                        // SAFETY: The prepared exclusive processor owns all
                        // retained pointer arrays/planes for the checked frames.
                        self.record_call(
                            if use_replacing {
                                Vst2Operation::ProcessReplacing
                            } else {
                                Vst2Operation::ProcessAccumulating
                            },
                            unsafe {
                                guard::process(process, effect, inputs, outputs, frames as i32)
                            },
                        );
                    }
                } else {
                    // SAFETY: Compatibility processing retains its existing
                    // raw callback contract; no owned safety claim applies.
                    unsafe { process(effect, inputs, outputs, frames as i32) };
                }
            }
            cell.set(previous);
        });
        self.host
            .process_level
            .store(abi::process_level::USER, Ordering::Relaxed);
    }

    fn refresh_io(&mut self) -> Result<(), oc_audio::BlockPreparationError> {
        // SAFETY: this exclusively owned instance still retains its AEffect;
        // only ABI scalar routing/latency fields are read before bounds-checking channel counts.
        let (inputs, outputs, latency) = unsafe {
            (
                (*self.effect).num_inputs,
                (*self.effect).num_outputs,
                (*self.effect).initial_delay,
            )
        };
        if (0..=MAX_CHANNELS).contains(&inputs) && (0..=MAX_CHANNELS).contains(&outputs) {
            self.num_inputs = inputs;
            self.num_outputs = outputs;
            self.host
                .latency
                .store(latency.max(0) as u32, Ordering::Relaxed);
            self.allocate_channels();
            Ok(())
        } else {
            Err(oc_audio::BlockPreparationError::CapacityExceeded)
        }
    }

    fn read_output(&self, frames: usize, output: &mut [f32]) {
        if self.output_channels.is_empty() {
            output[..frames * 2].fill(0.0);
            return;
        }
        let left = &self.output_channels[0];
        let right = self.output_channels.get(1).unwrap_or(left);
        let frames = frames.min(output.len() / 2);
        for frame in 0..frames {
            output[frame * 2] = left[frame];
            output[frame * 2 + 1] = right[frame];
        }
    }

    fn read_param(&self, index: i32) -> f32 {
        if self.is_owned() && self.host.failure.failed() {
            return 0.0;
        }
        // SAFETY: The exclusively owned admitted descriptor remains readable;
        // a failed authority returns before any native pointer access above.
        let callback = unsafe { (*self.effect).get_parameter };
        match callback {
            Some(get) if self.is_owned() => {
                // SAFETY: The retained getter and index are admitted by this
                // exclusive metadata/control scope, using the typed native shim.
                self.record_call(Vst2Operation::GetParameter(index), unsafe {
                    guard::get(get, self.effect, index)
                })
                .unwrap_or(0.0)
                .clamp(0.0, 1.0)
            }
            Some(get) => {
                // SAFETY: This is the compatibility getter's existing ABI.
                unsafe { get(self.effect, index) }.clamp(0.0, 1.0)
            }
            None => 0.0,
        }
    }

    fn read_param_metadata(&mut self) {
        self.param_names.clear();
        self.param_labels.clear();
        self.param_defaults.clear();
        self.param_can_automate.clear();
        for index in 0..self.num_params {
            if self.is_owned() && self.host.failure.failed() {
                return;
            }
            let name = self.query_indexed_string(op::GET_PARAM_NAME, index);
            self.param_names.push(if name.is_empty() {
                format!("Param {index}")
            } else {
                name
            });
            self.param_labels
                .push(self.query_indexed_string(op::GET_PARAM_LABEL, index));
            let value = self.read_param(index);
            if self.is_owned() && self.host.failure.failed() {
                return;
            }
            self.host.store_value(index, value);
            self.param_defaults.push(f64::from(value));
            let automatable = self.dispatch(op::CAN_BE_AUTOMATED, index, 0, std::ptr::null_mut());
            self.param_can_automate.push(automatable > 0);
        }
    }

    fn query_string(&self, opcode: i32) -> String {
        if self.is_owned() && self.host.failure.failed() {
            return String::new();
        }
        if self.call_scratch.is_some() {
            return self.query_indexed_string(opcode, 0);
        }
        let mut buffer = [0u8; abi::MAX_NAME_STR];
        self.dispatch(opcode, 0, 0, buffer.as_mut_ptr() as *mut c_void);
        trim_c_string(&buffer)
    }

    fn query_indexed_string(&self, opcode: i32, index: i32) -> String {
        if self.is_owned() && self.host.failure.failed() {
            return String::new();
        }
        if let Some(packet) = self.owned_scratch() {
            // SAFETY: Exclusive UI metadata control borrows one stable packet;
            // it is never freed/replaced while native code can hold its output.
            let output = unsafe {
                (*packet).text.fill(0);
                (*packet).text.as_mut_ptr()
            };
            self.dispatch(opcode, index, 0, output.cast());
            if self.host.failure.failed() {
                return String::new();
            }
            // SAFETY: Native output returned without a recorded fault, within
            // the prepared fixed buffer; trim makes an owned bounded copy.
            return trim_c_string(unsafe { &(*packet).text });
        }
        let mut buffer = [0u8; abi::MAX_PARAM_STR];
        self.dispatch(opcode, index, 0, buffer.as_mut_ptr() as *mut c_void);
        trim_c_string(&buffer)
    }

    fn can_do(&self, feature: &str) -> bool {
        if self.is_owned() && self.host.failure.failed() {
            return false;
        }
        if let Some(packet) = self.owned_scratch() {
            // SAFETY: UI-exclusive feature probing owns the retained input loan;
            // prior healthy calls ended before this packet is prepared again.
            let packet = unsafe { &mut *packet };
            if packet.prepare_query_input(feature.as_bytes()).is_err() {
                return false;
            }
            let pointer = packet.query_input.as_mut_ptr().cast();
            return self.dispatch(op::CAN_DO, 0, 0, pointer) > 0;
        }
        let Ok(feature) = CString::new(feature) else {
            return false;
        };
        self.dispatch(op::CAN_DO, 0, 0, feature.as_ptr() as *mut c_void) > 0
    }

    fn param_index(&self, name: &str) -> Option<i32> {
        let name = name.trim();
        if let Some(index) = name
            .strip_prefix('#')
            .and_then(|rest| rest.parse::<i32>().ok())
        {
            return (0..self.num_params).contains(&index).then_some(index);
        }
        self.param_names
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .or_else(|| {
                self.param_names.iter().position(|candidate| {
                    normalized_param_chars(candidate).eq(normalized_param_chars(name))
                })
            })
            .map(|index| index as i32)
    }

    fn dispatch(&self, opcode: i32, index: i32, value: isize, pointer: *mut c_void) -> isize {
        self.dispatch_float(opcode, index, value, pointer, 0.0)
    }

    fn dispatch_float(
        &self,
        opcode: i32,
        index: i32,
        value: isize,
        pointer: *mut c_void,
        opt: f32,
    ) -> isize {
        if self.is_owned() && self.host.failure.failed() {
            return 0;
        }
        // SAFETY: the retained instance owns its non-null AEffect until effClose;
        // copying the dispatcher slot does not itself enter plugin code.
        let Some(dispatcher) = (unsafe { (*self.effect).dispatcher }) else {
            return 0;
        };
        let host = &*self.host as *const HostContext as *mut HostContext;
        ACTIVE_HOST.with(|cell| {
            let previous = cell.replace(host);
            let result = if self.is_owned() {
                // SAFETY: Exclusive control/process admission retains callback,
                // descriptor and stable argument packet through this typed call.
                self.record_call(Vst2Operation::Dispatcher(opcode), unsafe {
                    guard::dispatch(
                        dispatcher,
                        self.effect,
                        guard::DispatchArgs {
                            opcode,
                            index,
                            value,
                            ptr: pointer,
                            opt,
                        },
                    )
                })
                .unwrap_or(0)
            } else {
                // SAFETY: The compatibility dispatcher keeps its prior ABI;
                // its caller migration/native exception gap is separate.
                unsafe { dispatcher(self.effect, opcode, index, value, pointer, opt) }
            };
            cell.set(previous);
            result
        })
    }
}

impl Drop for Vst2Instance {
    fn drop(&mut self) {
        if self.effect.is_null() {
            return;
        }
        if self.is_owned() {
            // OwnedCandidate alone owns this private construction path. It
            // closes before releasing this Box or retains the entire Box;
            // public compatibility construction never creates an owned value.
            return;
        }
        // A host callback quota fault refuses work, but does not imply native
        // teardown is broken. Preserve backing and callback admission through
        // ordinary healthy cleanup using the originally admitted dispatcher.
        // SAFETY: The live instance and pinned module retain this descriptor,
        // dispatcher and host context through synchronous suspend/CLOSE.
        unsafe { close_effect(self.effect, self.close_dispatcher, &self.host, self.active) };
        self.binding.complete();
        self.effect = std::ptr::null_mut();
        // The module stays loaded: a plugin may still own worker threads and
        // unloading it after effClose can fault. Matches the CLAP host.
    }
}

impl AudioEffect for Vst2Instance {
    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.prepare_block_buffers(max_frames)
    }
    fn processing_failed(&self) -> bool {
        self.process_error.is_some() || self.check_callback_route().is_err()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        match self
            .process_error
            .or_else(|| self.check_callback_route().err())
        {
            None => oc_audio::ProcessingFailureReport::Unavailable,
            Some(_) if self.error_reported.replace(true) => {
                oc_audio::ProcessingFailureReport::Reported
            }
            Some(error) => {
                oc_audio::ProcessingFailureReport::Ready(format!("VST2 {}: {error}", self.name))
            }
        }
    }
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let _ = self.process_effect_into(buffer, sample_rate, None);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let _ = self.process_effect_into(buffer, sample_rate, transport);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        self.set_param_by_name(name, value);
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        self.get_param_by_name(name)
    }

    fn reset(&mut self) {
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return;
        }
        self.process_error = None;
        self.error_reported.set(false);
        self.pending_midi.clear();
        for channel in &mut self.output_channels {
            channel.fill(0.0);
        }
    }

    fn max_block_frames(&self) -> u32 {
        self.max_block_frames
    }

    fn latency_samples(&self) -> u32 {
        Vst2Instance::latency_samples(self)
    }

    fn tail_samples(&self) -> u32 {
        Vst2Instance::tail_samples(self)
    }

    fn plugin_tail_samples(&self) -> Option<u32> {
        Vst2Instance::plugin_tail_samples(self)
    }

    fn end_processing(&mut self) {
        self.deactivate();
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Convert the plugin's category reply.
pub fn category_from_plug_category(value: i32) -> PluginCategory {
    match value {
        abi::plug_category::SYNTH | abi::plug_category::GENERATOR => PluginCategory::Instrument,
        abi::plug_category::ANALYSIS => PluginCategory::Analyzer,
        _ => PluginCategory::Effect,
    }
}

/// Path of the loadable binary inside a bundle, or the file itself.
pub fn resolve_executable(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.exists() {
        return Err(format!(
            "VST2 plugin path does not exist: {}",
            path.display()
        ));
    }
    let macos = path.join("Contents/MacOS");
    let Ok(entries) = std::fs::read_dir(&macos) else {
        return Err(format!(
            "VST2 bundle has no Contents/MacOS directory: {}",
            path.display()
        ));
    };
    let expected = path.file_stem().and_then(|stem| stem.to_str());
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|entry| entry.is_file())
        .collect();
    candidates.sort();
    if let Some(expected) = expected
        && let Some(matching) = candidates.iter().find(|candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some(expected)
        })
    {
        return Ok(matching.clone());
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("VST2 bundle contains no binary: {}", path.display()))
}

fn trim_c_string(buffer: &[u8]) -> String {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).trim().to_string()
}

fn normalized_param_chars(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
}

#[cfg(unix)]
fn module_path_cstring(path: &Path) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| "VST2 path contains a NUL byte".into())
}

/// Owned loader reference until any plugin entry code has run.
struct DynamicLibrary {
    handle: *mut c_void,
}

/// VST2 exposes no proof that plugin workers have stopped at effClose. Keep
/// invoked modules mapped until process exit; isolated hosting can reclaim them.
/// Code residency neither retains callback context nor authorizes callbacks
/// after the native lifetime contract ends. OS-loader constructors/destructors
/// also remain outside the owned entry/dispatcher exception barriers.
struct ProcessPinnedLibrary {
    _library: std::mem::ManuallyDrop<DynamicLibrary>,
}

impl DynamicLibrary {
    fn into_process_pinned(self) -> ProcessPinnedLibrary {
        ProcessPinnedLibrary {
            _library: std::mem::ManuallyDrop::new(self),
        }
    }

    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let path = module_path_cstring(path)?;
            // SAFETY: path is a terminated C string retained through dlopen; this
            // owner tracks the returned loader reference before any plugin entry invocation.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                // SAFETY: dlerror returns null or its own terminated diagnostic storage;
                // the next code copies that storage before another loader operation can replace it.
                let error = unsafe { libc::dlerror() };
                let message = if error.is_null() {
                    "unknown dlopen error".to_string()
                } else {
                    // SAFETY: this non-null dlerror result remains valid until the next
                    // loader call; create the owned diagnostic immediately while it is still current.
                    unsafe { CStr::from_ptr(error) }
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(format!("failed to load VST2 plugin: {message}"));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self { handle })
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: wide retains a terminated UTF-16 path through LoadLibraryW;
            // a successful handle is owned by DynamicLibrary until released or process-pinned.
            let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "failed to load VST2 plugin: {}",
                    std::io::Error::last_os_error()
                ));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self { handle })
        }
    }

    fn symbol(&self, name: &CStr) -> *mut c_void {
        #[cfg(unix)]
        {
            // SAFETY: self owns a live dlopen reference and name is a terminated C
            // symbol name retained through this synchronous lookup.
            unsafe { libc::dlsym(self.handle, name.as_ptr()) }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
            // SAFETY: self owns this successful LoadLibraryW reference and name is
            // a terminated export name; GetProcAddress only borrows both during lookup.
            unsafe { GetProcAddress(self.handle as _, name.as_ptr() as *const u8) }
                .map(|function| function as *mut c_void)
                .unwrap_or(std::ptr::null_mut())
        }
    }

    /// `VSTPluginMain`, falling back to the older `main` export.
    fn vst2_entry(&self) -> Result<abi::PluginMain, String> {
        for name in [c"VSTPluginMain", c"main"] {
            let symbol = self.symbol(name);
            if !symbol.is_null() {
                // SAFETY: this is the documented entry point and signature.
                return Ok(unsafe { std::mem::transmute::<*mut c_void, abi::PluginMain>(symbol) });
            }
        }
        Err("VST2 plugin exports neither VSTPluginMain nor main".into())
    }
}

#[cfg(test)]
thread_local! {
    static MODULE_RELEASE_COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Release exactly the owned OS loader reference, without promising image eviction.
///
/// # Safety
/// handle must be a live loader reference exclusively owned by the caller.
unsafe fn release_module_reference(handle: *mut c_void) {
    #[cfg(unix)]
    // SAFETY: the caller transfers one live dlopen reference to this release.
    let released = unsafe { libc::dlclose(handle) } == 0;
    #[cfg(windows)]
    // SAFETY: the caller transfers one live LoadLibraryW reference to this release.
    let released = unsafe { windows_sys::Win32::Foundation::FreeLibrary(handle) } != 0;
    #[cfg(test)]
    MODULE_RELEASE_COUNTS.with(|counts| {
        let (calls, failures) = counts.get();
        counts.set((calls + 1, failures + usize::from(!released)));
    });
    #[cfg(not(test))]
    let _ = released;
}

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        // SAFETY: open acquired this unique reference; a pinned module's
        // ManuallyDrop prevents reaching this pre-entry release path.
        unsafe { release_module_reference(self.handle) };
    }
}

fn guarded_close(
    effect: *mut abi::AEffect,
    dispatcher: abi::DispatcherProc,
    host: &HostContext,
    active: bool,
) -> bool {
    if host.failure.failed() {
        return false;
    }
    let phases = [
        (active, Vst2Operation::Deactivate, op::MAINS_CHANGED),
        (true, Vst2Operation::Close, op::CLOSE),
    ];
    for (run, operation, opcode) in phases {
        if !run {
            continue;
        }
        // SAFETY: The UI candidate/registry retains descriptor, original cleanup
        // callback, module and complete argument backing until guarded return.
        match unsafe {
            guard::dispatch(
                dispatcher,
                effect,
                guard::DispatchArgs {
                    opcode,
                    index: 0,
                    value: 0,
                    ptr: std::ptr::null_mut(),
                    opt: 0.0,
                },
            )
        } {
            Ok(_) if !host.failure.failed() => {}
            Ok(_) => return false,
            Err(fault) => {
                host.failure.record(operation, fault);
                return false;
            }
        }
    }
    true
}

unsafe fn close_effect(
    effect: *mut abi::AEffect,
    dispatcher: abi::DispatcherProc,
    host: &HostContext,
    active: bool,
) {
    ACTIVE_HOST.with(|cell| {
        let previous = cell.replace(std::ptr::from_ref(host).cast_mut());
        if active {
            // SAFETY: The admitted original dispatcher and live effect remain
            // owned through this synchronous deactivation with no pointer args.
            unsafe { dispatcher(effect, op::MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0) };
        }
        // SAFETY: The same original dispatcher owns destruction of this live
        // instance; all host callback/process backing is retained until return.
        unsafe { dispatcher(effect, op::CLOSE, 0, 0, std::ptr::null_mut(), 0.0) };
        cell.set(previous);
    });
}

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    /// Exercise the actual host callback without a third-party plugin.
    pub fn notify_io_changed(instance: &super::Vst2Instance) {
        // SAFETY: a loaded instance retains this host context and AEffect for
        // the entire callback; the callback only publishes an atomic flag.
        unsafe {
            super::audio_master(
                instance.effect,
                super::abi::host_op::IO_CHANGED,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            );
        }
    }

    pub fn fixture_path(name: &str) -> Option<std::path::PathBuf> {
        let root = std::path::Path::new(option_env!("OC_VST2_FIXTURE_DIR")?);
        let extension = if cfg!(target_os = "macos") {
            "vst"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        };
        let path = root.join(format!("{name}.{extension}"));
        path.exists().then_some(path)
    }
}
