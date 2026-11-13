//! VST3 plugin instance — loads a VST3 plugin, sends MIDI, gets audio.
//!
//! Uses the `vst3` crate (coupler-rs) for COM bindings.
//! Supports both instrument and effect plugins.

mod authored_parameters;
pub use authored_parameters::{
    Vst3AuthoredParameterKey, Vst3AuthoredParameterOwner, Vst3ParameterRefusal,
};
mod owned_lifecycle;
pub use owned_lifecycle::{
    MAX_VST3_AUTHORED_NAME_BYTES, MAX_VST3_AUTHORED_PARAMETERS, MAX_VST3_AUTHORED_TOTAL_NAME_BYTES,
    Vst3AuthoredInput, Vst3ControlHandle, Vst3MetadataReadPlan, Vst3MidiEvent, Vst3MidiMessage,
    Vst3NormalizedBindingsPlan, Vst3NormalizedEvent, Vst3NormalizedParameter, Vst3OrderedEvent,
    Vst3OwnedAuthoredPlan, Vst3OwnedError, Vst3OwnedInstance, Vst3OwnedInstrumentBlock,
    Vst3OwnedLoadJob, Vst3OwnedLoadPlan, Vst3OwnedParameter, Vst3OwnedProcessBlock,
    Vst3OwnedSnapshot, Vst3ParameterWritePlan, Vst3ProcessLeaseError, Vst3ProcessingBudget,
    Vst3ProcessorLease, Vst3StateCapturePlan, poll_owned_vst3_retirements,
};
#[cfg(any(test, feature = "test-support"))]
pub use owned_lifecycle::{
    Vst3ActivationAudit, Vst3AuthoredPreparationAudit, Vst3AuthoredRuntimeAudit,
    Vst3InitialPreparationAudit, Vst3MidiIngressAudit, audit_checked_owned_activation,
    audit_owned_authored_preparation, audit_owned_authored_runtime,
    audit_owned_initial_preparation, audit_owned_midi_ingress,
};
mod factory;
mod note_ids;
use note_ids::Vst3NoteIds;
mod note_expression;
pub use note_expression::{
    Vst3NoteExpressionError, Vst3NoteExpressionValue, Vst3StandardNoteExpression,
};
mod foreign_ref;
mod module_loader;
#[cfg(any(test, feature = "test-support"))]
use crate::native_fault::FaultKind;
use crate::native_fault::{Fault, FirstFault};
use crate::vst3_guard::{self, Operation};
pub use crate::vst3_guard::{Operation as Vst3Operation, Vst3ProcessError};
pub use foreign_ref::Vst3ForeignRef;
#[cfg(target_os = "macos")]
use foreign_ref::{EditorControllerLease, HostFrame, drain_editor_retirements};
use foreign_ref::{HostBacking, InitializationGuard};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
/// Test-only native VST3 fixtures.
pub mod native_fixture;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
use objc2::MainThreadOnly;

use serde::{Deserialize, Serialize};
// The editor path (macOS) and the test fixtures are the only users of these.
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
use vst3::ComRef;
#[cfg(any(test, feature = "test-support"))]
use vst3::Steinberg::IPluginBaseTrait;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, BusInfo, Chord, Event,
    Event_::EventTypes_, Event__type0, FrameRate, IAudioProcessor, IComponent, IComponentHandler,
    IConnectionPoint, IEditController, IEditControllerTrait, IEventList, IEventListTrait,
    IHostApplication, IKeyswitchController, IKeyswitchControllerTrait, IMidiMapping,
    IMidiMappingTrait, INoteExpressionController, INoteExpressionControllerTrait, IParamValueQueue,
    IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, IProgramListData,
    IProgramListDataTrait, IUnitInfo, IUnitInfoTrait, KeyswitchInfo, MediaTypes_,
    NoteExpressionTypeID, NoteExpressionTypeInfo,
    NoteExpressionTypeInfo_::NoteExpressionTypeFlags_, NoteExpressionValueEvent, NoteOffEvent,
    NoteOnEvent, ParamID, ParamValue, ParameterInfo, ParameterInfo_::ParameterFlags_,
    PolyPressureEvent, ProcessContext, ProcessContext_::StatesAndFlags_, ProcessData, ProcessSetup,
    ProgramListInfo, RestartFlags_, Sample32, SpeakerArr, SpeakerArrangement, SymbolicSampleSizes_,
};
#[cfg(any(test, feature = "test-support"))]
use vst3::Steinberg::Vst::{IComponentTrait, IConnectionPointTrait};
use vst3::Steinberg::{
    IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, IPluginFactory, IPluginFactory2, char16,
    int32, int64, kNotImplemented, kResultOk, tresult,
};
#[cfg(target_os = "macos")]
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait,
    IPlugViewTrait, ViewRect, kInvalidArgument, kPlatformTypeNSView,
};
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
use vst3::Steinberg::{IPlugView, kResultFalse};
use vst3::{Class, ComPtr, ComWrapper, Interface};

use crate::PluginTransport;
use crate::host_app::Vst3Host;
use crate::param_edit::{ParamEditBus, ParamEditReceiver};
use crate::ports::{self, AudioPort};
use oc_dsp::AudioEffect;
#[cfg(any(test, feature = "test-support"))]
use vst3::Steinberg::Vst::IAudioProcessorTrait;

/// Parameter info from a VST3 plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vst3Param {
    pub id: u32,
    pub name: String,
    pub default: f64,
    pub step_count: i32,
    pub unit: String,
}

/// One parameter as the name lookup needs it.
#[derive(Clone, Copy, Debug)]
struct CachedParam {
    id: ParamID,
    flags: i32,
}

/// Normalized-name lookup over a plugin's parameter list.
///
/// A plugin parameter change event resolves one name per automation step, so
/// enumerating and allocating every parameter for each step is too expensive.
/// The cache holds only the fields a write needs; a title change invalidates it.
#[derive(Default)]
struct ParamCache {
    by_name: HashMap<String, CachedParam>,
    valid: bool,
}

/// Parameter detail for host-side inspection: the formatted value range and
/// the raw VST3 flags, which [`Vst3Param`] does not carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vst3ParamDetail {
    pub id: u32,
    pub name: String,
    pub unit: String,
    pub default_normalized: f64,
    /// `getParamStringByValue` at normalized 0.0.
    pub min_text: String,
    /// `getParamStringByValue` at normalized 1.0.
    pub max_text: String,
    /// `getParamStringByValue` at the default normalized value.
    pub default_text: String,
    pub step_count: i32,
    /// Formatted labels for stepped parameters, sampled evenly over 0..=1.
    pub step_texts: Vec<String>,
    pub flags: i32,
    pub read_only: bool,
    pub hidden: bool,
    pub list: bool,
    pub can_automate: bool,
}

/// Metadata discovered from a VST3 bundle.
#[derive(Debug, Clone)]
pub struct Vst3ClassInfo {
    pub name: String,
    pub category: String,
    pub sub_categories: String,
    pub vendor: String,
    pub version: String,
    pub cid: [i8; 16],
}

/// Capabilities reported by a VST3's native editor view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst3EditorInfo {
    pub width: i32,
    pub height: i32,
    pub resizable: bool,
    pub platform_supported: bool,
}

/// A change a plugin reported through `IComponentHandler::restartComponent`.
///
/// The host re-reads the affected state when it receives one, so the parameter
/// values and the bus/latency view a caller holds never stay stale after a
/// program change, a bus change or a latency change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vst3HostEvent {
    /// `kParamValuesChanged`: re-read every parameter value.
    ParamValuesChanged,
    /// `kParamTitlesChanged`: the parameter names may have changed.
    ParamTitlesChanged,
    /// `kIoChanged`: the audio buses changed; re-negotiate the output layout.
    IoChanged,
    /// `kLatencyChanged`: re-read `getLatencySamples`.
    LatencyChanged,
    /// `kMidiCCAssignmentChanged`: MIDI controller assignments changed.
    MidiCcAssignmentChanged,
    /// `kNoteExpressionChanged`: the note-expression surface changed.
    NoteExpressionChanged,
    /// `kKeyswitchChanged`: the keyswitch surface changed.
    KeyswitchChanged,
    /// `kReloadComponent`: the plugin asks to be reloaded.
    ReloadComponent,
}

/// Point where a test makes a hosted instance fail the way a plugin would.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// The plugin throws from `process`.
    Process,
    /// The plugin throws from `getState`.
    GetState,
    /// The plugin throws from `setState`.
    SetState,
    /// The plugin throws from `IEditController::setComponentState`.
    SetComponentState,
    /// The plugin throws from `createView`.
    CreateView,
    /// The plugin throws from `terminate`, during teardown.
    Terminate,
}

/// One keyswitch a sampler exposes through `IKeyswitchController`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst3Keyswitch {
    /// Index in the controller's keyswitch list.
    pub index: i32,
    pub name: String,
    pub short_name: String,
    /// First MIDI key of the switch's range.
    pub min_key: i32,
    /// Last MIDI key of the switch's range.
    pub max_key: i32,
    /// Key the plugin remaps the switch onto, when it remaps one.
    pub remapped_key: Option<i32>,
    pub unit_id: i32,
    pub flags: i32,
}

/// One note expression a plugin exposes through `INoteExpressionController`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vst3NoteExpression {
    pub type_id: NoteExpressionTypeID,
    pub name: String,
    pub short_name: String,
    pub unit: String,
    pub unit_id: i32,
    pub default_value: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub step_count: i32,
    /// Parameter the host can automate instead of sending note expressions.
    pub associated_parameter: ParamID,
    pub bipolar: bool,
    pub one_shot: bool,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct Vst3EditorHandles {
    failure: Arc<Vst3Failure>,
    view: AtomicUsize,
    window: AtomicUsize,
    opening: AtomicBool,
    attach_generation: AtomicUsize,
    /// Set while `open` is inside `IPlugView::attached`.
    attaching: AtomicBool,
    /// Call `onSize` synchronously from `resizeView` even while attaching.
    sync_resize: AtomicBool,
    /// Size requested by `resizeView` while `attaching`; applied after attach.
    pending_size: Mutex<Option<(i32, i32)>>,
    /// Size of the last window this handle opened. It survives `close`, so the
    /// next attach hands the fresh view the size the previous one had.
    last_size: Mutex<Option<(i32, i32)>>,
}

#[cfg(target_os = "macos")]
impl Vst3EditorHandles {
    fn new(failure: Arc<Vst3Failure>) -> Self {
        Self {
            failure,
            ..Self::default()
        }
    }
    fn cancel_attach(&self) {
        self.opening.store(false, Ordering::Release);
        // Exhaustion never wraps: subsequent open admission is refused. The
        // opening flag and removed view/window also revoke a final-generation ticket.
        let _ = self.attach_generation.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |generation| generation.checked_add(1),
        );
    }
    fn check(&self) -> Result<(), String> {
        if self.failure.failed() {
            Err(self
                .failure
                .message()
                .unwrap_or_else(|| "VST3 instance is quarantined".into()))
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
fn guard_editor_call<T>(
    handles: &Vst3EditorHandles,
    work: impl FnOnce() -> T,
) -> Result<T, String> {
    handles.check()?;
    drain_editor_retirements()?;
    // Unknown foreign exceptions in generated view slots remain a separate gap;
    // this gate only prevents reentry after an already known shared failure.
    crate::ffi_guard::guard_ui(work)
}

#[cfg(target_os = "macos")]
struct EditorOpening {
    handles: Arc<Vst3EditorHandles>,
    generation: usize,
}
#[cfg(target_os = "macos")]
impl EditorOpening {
    fn new(handles: &Arc<Vst3EditorHandles>) -> Result<Self, String> {
        handles
            .opening
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "VST3 editor is already attaching".to_owned())?;
        let generation = match handles.attach_generation.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |value| value.checked_add(1),
        ) {
            Ok(previous) => previous + 1,
            Err(_) => {
                handles.opening.store(false, Ordering::Release);
                return Err("VST3 editor attach generation exhausted".into());
            }
        };
        Ok(Self {
            handles: Arc::clone(handles),
            generation,
        })
    }
    fn check(&self) -> Result<(), String> {
        self.handles.check()?;
        if !self.handles.opening.load(Ordering::Acquire)
            || self.handles.attach_generation.load(Ordering::Acquire) != self.generation
        {
            return Err("VST3 editor was closed while attaching".into());
        }
        Ok(())
    }
}
#[cfg(target_os = "macos")]
impl Drop for EditorOpening {
    fn drop(&mut self) {
        if self.handles.attach_generation.load(Ordering::Acquire) == self.generation {
            self.handles.opening.store(false, Ordering::Release);
            self.handles.attaching.store(false, Ordering::Release);
            self.handles.sync_resize.store(false, Ordering::Release);
        }
    }
}

#[cfg(target_os = "macos")]
struct EditorParentLease {
    value: ManuallyDrop<Option<objc2::rc::Retained<objc2_app_kit::NSView>>>,
    failure: Arc<Vst3Failure>,
}
#[cfg(target_os = "macos")]
impl std::ops::Deref for EditorParentLease {
    type Target = Option<objc2::rc::Retained<objc2_app_kit::NSView>>;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
#[cfg(target_os = "macos")]
impl std::ops::DerefMut for EditorParentLease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}
#[cfg(target_os = "macos")]
impl Drop for EditorParentLease {
    fn drop(&mut self) {
        if !self.failure.failed() {
            // SAFETY: the lease owns this optional retain once; successful
            // detach or transfer to the window hierarchy precedes healthy drop.
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }
    }
}
#[cfg(target_os = "macos")]
struct EditorAttachedLease<'a> {
    view: &'a Vst3ForeignRef<IPlugView>,
    armed: bool,
}
#[cfg(target_os = "macos")]
impl Drop for EditorAttachedLease<'_> {
    fn drop(&mut self) {
        if !self.armed || self.view.failure.failed() {
            return;
        }
        // SAFETY: this main-thread borrow pins the successfully attached view;
        // parent/window leases remain live until this balanced rollback finishes.
        if editor_cleanup_result(&self.view.failure, unsafe {
            vst3_guard::view_removed(self.view.as_ptr())
        })
        .is_err()
        {
            return;
        }
        // SAFETY: the same main-thread loan retains the now-detached view;
        // a null host frame clears its callback binding before healthy release.
        let _ = editor_cleanup_result(&self.view.failure, unsafe {
            vst3_guard::view_set_frame(self.view.as_ptr(), ptr::null_mut())
        });
    }
}

#[cfg(target_os = "macos")]
fn editor_cleanup_result(
    failure: &Vst3Failure,
    result: Result<tresult, Fault>,
) -> Result<(), String> {
    match result {
        Ok(code) if code == kResultOk => {}
        Ok(code) => {
            failure
                .slot
                .record(Vst3ProcessError::refused(Operation::Legacy, code));
        }
        Err(fault) => {
            failure
                .slot
                .record(Vst3ProcessError::native(Operation::Legacy, fault));
        }
    }
    if failure.failed() {
        return Err(failure
            .message()
            .unwrap_or_else(|| "VST3 editor is quarantined".into()));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct EditorWindowLease {
    window: ManuallyDrop<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
    handles: Arc<Vst3EditorHandles>,
    published: bool,
    committed: bool,
}
#[cfg(target_os = "macos")]
impl std::ops::Deref for EditorWindowLease {
    type Target = objc2::rc::Retained<objc2_app_kit::NSWindow>;
    fn deref(&self) -> &Self::Target {
        &self.window
    }
}
#[cfg(target_os = "macos")]
impl EditorWindowLease {
    fn publish(&mut self) -> Result<(), String> {
        // Publication owns an independent retain. Nested close consumes only
        // that retain; this local lease remains live across the nested run loop.
        let published = objc2::rc::Retained::clone(&self.window);
        let pointer = objc2::rc::Retained::into_raw(published) as usize;
        if self
            .handles
            .window
            .compare_exchange(0, pointer, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // SAFETY: the failed publication still owns exactly this extra retain.
            drop(unsafe {
                objc2::rc::Retained::<objc2_app_kit::NSWindow>::from_raw(pointer as *mut _)
            });
            return Err("Another VST3 editor window was published".into());
        }
        self.published = true;
        Ok(())
    }
    fn commit(mut self) {
        self.committed = true;
    }
}
#[cfg(target_os = "macos")]
impl Drop for EditorWindowLease {
    fn drop(&mut self) {
        if self.handles.failure.failed() {
            return;
        }
        if !self.committed {
            let pointer = objc2::rc::Retained::as_ptr(&self.window) as usize;
            if self.published
                && self
                    .handles
                    .window
                    .compare_exchange(pointer, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                // SAFETY: the successful exchange recovers this publication's
                // extra retain; nested close cannot consume it a second time.
                drop(unsafe {
                    objc2::rc::Retained::<objc2_app_kit::NSWindow>::from_raw(pointer as *mut _)
                });
            }
            self.window.close();
        }
        // SAFETY: the local lease owns a separate retain from any published
        // pointer, and healthy cleanup drops exactly that retain once.
        unsafe { ManuallyDrop::drop(&mut self.window) };
    }
}

#[cfg(target_os = "macos")]
struct MainThreadViewOwner {
    view: ManuallyDrop<Vst3ForeignRef<IPlugView>>,
    _frame: ManuallyDrop<Arc<HostFrame>>,
    _controller: ManuallyDrop<Arc<EditorControllerLease>>,
    _handles: ManuallyDrop<Arc<Vst3EditorHandles>>,
}
#[cfg(target_os = "macos")]
impl Drop for MainThreadViewOwner {
    fn drop(&mut self) {
        let failure = Arc::clone(&self.view.failure);
        if failure.failed() {
            return;
        }
        // SAFETY: this main-only owner holds one view reference. Native Release
        // is guarded by Vst3ForeignRef and records failure before returning.
        unsafe { ManuallyDrop::drop(&mut self.view) };
        if failure.failed() {
            return;
        }
        // SAFETY: all view use has retired; drop the controller while frame/host
        // callback backing is still retained in case controller Release faults.
        unsafe { ManuallyDrop::drop(&mut self._controller) };
        if failure.failed() {
            return;
        }
        // SAFETY: native view/controller retirement completed without a fault;
        // these host Arc fields are owned here and are released exactly once.
        unsafe {
            ManuallyDrop::drop(&mut self._frame);
            ManuallyDrop::drop(&mut self._handles);
        }
    }
}
#[cfg(target_os = "macos")]
thread_local! {static EDITOR_VIEWS:RefCell<HashMap<usize,ManuallyDrop<Rc<MainThreadViewOwner>>>>=RefCell::new(HashMap::new());}
#[cfg(target_os = "macos")]
fn publish_editor_view(
    handles: Arc<Vst3EditorHandles>,
    view: Vst3ForeignRef<IPlugView>,
    frame: Arc<HostFrame>,
    controller: Arc<EditorControllerLease>,
) {
    assert!(
        is_main_thread(),
        "editor view ownership requires AppKit main"
    );
    let key = Arc::as_ptr(&handles) as usize;
    let pointer = view.as_ptr() as usize;
    let owner = Rc::new(MainThreadViewOwner {
        view: ManuallyDrop::new(view),
        _frame: ManuallyDrop::new(frame),
        _controller: ManuallyDrop::new(controller),
        _handles: ManuallyDrop::new(Arc::clone(&handles)),
    });
    let previous =
        EDITOR_VIEWS.with(|views| views.borrow_mut().insert(key, ManuallyDrop::new(owner)));
    handles.view.store(pointer, Ordering::Release);
    // Release only after both registry and handle publication. A foreign release
    // may reenter close; no trailing store may resurrect its retired pointer.
    if let Some(previous) = previous {
        drop(ManuallyDrop::into_inner(previous));
    }
}
#[cfg(target_os = "macos")]
fn take_editor_view(handles: &Arc<Vst3EditorHandles>) -> Option<Rc<MainThreadViewOwner>> {
    assert!(
        is_main_thread(),
        "editor view retirement requires AppKit main"
    );
    handles.cancel_attach();
    let key = Arc::as_ptr(handles) as usize;
    EDITOR_VIEWS
        .with(|views| views.borrow_mut().remove(&key))
        .map(ManuallyDrop::into_inner)
}

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
fn loan_editor_view(handles: &Arc<Vst3EditorHandles>) -> Result<Rc<MainThreadViewOwner>, String> {
    if objc2::MainThreadMarker::new().is_none() {
        return Err("VST3 editor loan requires AppKit main".into());
    }
    let key = Arc::as_ptr(handles) as usize;
    // Clone only a host Rc while borrowed. Foreign AddRef/release can reenter
    // the registry, so no COM operation may occur under this RefCell borrow.
    EDITOR_VIEWS.with(|views| {
        let views = views
            .try_borrow()
            .map_err(|_| "VST3 editor registry is busy")?;
        views
            .get(&key)
            .map(|owner| Rc::clone(owner))
            .ok_or_else(|| "No registered VST3 editor view".into())
    })
}

/// Host-side `IPlugFrame`: resizes the editor window and declines other interfaces.
/// `IRunLoop` is refused because AppKit's main run loop already serves the view.
#[cfg(target_os = "macos")]
struct Vst3PlugFrame {
    handles: Arc<Vst3EditorHandles>,
}

#[cfg(target_os = "macos")]
impl Vst3PlugFrame {
    fn new(handles: Arc<Vst3EditorHandles>) -> Self {
        Self { handles }
    }
}

#[cfg(target_os = "macos")]
impl IPlugFrameTrait for Vst3PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSWindow;
        use objc2_foundation::NSSize;

        if self.handles.failure.failed() {
            return kResultFalse;
        }
        if new_size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: VST3 guarantees a valid `ViewRect` for the duration of the call.
        let rect = unsafe { &*new_size };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            // A degenerate rect is a plugin error: refuse it instead of
            // collapsing the window to nothing.
            return kResultFalse;
        }
        // Never re-enter the plugin while it is still inside `attached`, unless
        // the host variation explicitly tests that callback order.
        if self.handles.attaching.load(Ordering::Acquire)
            && !self.handles.sync_resize.load(Ordering::Acquire)
        {
            *self.handles.pending_size.lock().unwrap() = Some((width, height));
            return kResultOk;
        }
        let window = self.handles.window.load(Ordering::Acquire);
        if window == 0 || MainThreadMarker::new().is_none() {
            return kResultOk;
        }
        // SAFETY: the window is retained by the handle from `open` until `close`.
        let window = unsafe { &*(window as *const NSWindow) };
        let target = NSSize::new(width as f64, height as f64);
        let current = window
            .contentView()
            .map(|content| content.frame().size)
            .unwrap_or(NSSize::new(0.0, 0.0));
        if current.width == target.width && current.height == target.height {
            return kResultOk;
        }
        window.setContentSize(target);
        if !view.is_null() && !self.handles.failure.failed() {
            // SAFETY: the caller passes its own live view and rect for this notification.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            // SAFETY: The callback retains its view and writable rect through this synchronous notification on main.
            let _ = crate::ffi_guard::guard(|| unsafe { view.onSize(new_size) });
        }
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl Class for Vst3PlugFrame {
    type Interfaces = (IPlugFrame,);
}

/// Host-side variation of the editor attach sequence.
///
/// The default is the shipped path. Every other variant is a host capability
/// other VST3 hosts provide; the headless probe uses them to isolate what an
/// editor needs before `attached`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditorAttachVariant {
    #[default]
    Baseline,
    /// Make the parent view layer-backed before attach.
    Layer,
    /// Attach to a host-owned flipped content view.
    ContentView,
    /// Install the plug frame after attach instead of before.
    FrameLate,
    /// Run attach on a later main-run-loop turn.
    Async,
    /// Forward the window backing scale to the view before attach.
    Scale,
    /// Queue every resize callback until attach returns.
    FrameNoResize,
    /// Attach to a child view of a window shown before attach.
    ChildWindow,
    /// Deliver `onSize` synchronously from inside `resizeView` during attach.
    OnSizeSync,
    /// Deliver the initial size to `onSize` before attach.
    OnSizeEarly,
    /// Attach to an `NSView` that is not in a window yet.
    Detached,
}

// Host-owned container that lays out its children from the top left.
#[cfg(target_os = "macos")]
objc2::define_class!(
    #[unsafe(super(objc2_app_kit::NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct Vst3EditorContainer;

    impl Vst3EditorContainer {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            objc2::runtime::Bool::YES
        }
    }
);

/// State of an attach call deferred to a later main-run-loop turn.
#[cfg(target_os = "macos")]
struct PendingAttach {
    failure: Arc<Vst3Failure>,
    view: *mut IPlugView,
    parent: *mut c_void,
    result: Cell<tresult>,
    done: Cell<bool>,
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn attach_on_main_run_loop(context: *mut c_void) {
    // SAFETY: the unsafe caller retains PendingAttach for this synchronous callback;
    // the scheduler invalidates its target before the borrowed state can leave scope.
    let pending = unsafe { &*(context as *const PendingAttach) };
    if pending.failure.failed() {
        pending.result.set(kResultFalse);
        pending.done.set(true);
        return;
    }
    // SAFETY: the scheduler borrows the live view and NSView parent until completion
    // or cancellation on main. The native wrapper catches before returning to Rust.
    match unsafe { vst3_guard::view_attached(pending.view, pending.parent, kPlatformTypeNSView) } {
        Ok(result) => pending.result.set(result),
        Err(fault) => {
            pending
                .failure
                .slot
                .record(Vst3ProcessError::native(Operation::Legacy, fault));
            pending.result.set(kResultFalse);
        }
    }
    pending.done.set(true);
}

#[cfg(target_os = "macos")]
mod deferred_attach;
#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
pub use deferred_attach::{
    exercise_deferred_attach_for_test, validate_deferred_attach_delay_for_test,
};

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
/// Weak probe of an editor backing, for lifecycle tests.
pub struct Vst3EditorBackingProbe {
    frame: std::sync::Weak<HostFrame>,
    controller: std::sync::Weak<EditorControllerLease>,
    handles: std::sync::Weak<Vst3EditorHandles>,
}
#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
impl Vst3EditorBackingProbe {
    /// Whether frame, controller and handles are all still retained.
    pub fn alive(&self) -> bool {
        self.frame.strong_count() > 0
            && self.controller.strong_count() > 0
            && self.handles.strong_count() > 0
    }
}

/// Main-thread handle for a VST3 editor owned by a live [`Vst3Instance`].
///
/// The controller and plug frame references keep the editor valid while the
/// instance is in `AppState`; the shared handles prevent duplicate windows.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct Vst3GuiHandle {
    controller: Arc<EditorControllerLease>,
    frame: Arc<HostFrame>,
    handles: Arc<Vst3EditorHandles>,
    /// Why this editor must not be attached, when it is known to crash the host.
    blocked: Option<String>,
}

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
/// Drain queued editor retirements; tests only.
pub fn drain_editor_retirements_for_test() -> Result<(), String> {
    drain_editor_retirements()
}

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
/// Retire one registered editor view on the main thread; tests only.
pub fn retire_registered_editor_view_for_test(key: usize) -> Result<(), String> {
    if !is_main_thread() {
        return Err("editor view retirement requires AppKit main".into());
    }
    let owner = EDITOR_VIEWS
        .with(|views| views.borrow_mut().remove(&key))
        .ok_or("no registered editor view")?;
    let owner = ManuallyDrop::into_inner(owner);
    owner._handles.view.store(0, Ordering::Release);
    owner._handles.cancel_attach();
    drop(owner);
    drain_editor_retirements()
}

/// Size the host opens an editor window with when the view cannot report one
/// before it is attached. The plugin replaces it from `getSize`/`resizeView`.
#[cfg(target_os = "macos")]
const DEFAULT_EDITOR_SIZE: (i32, i32) = (1024, 768);

/// The view's size, or `None` while it reports that it is not initialized.
#[cfg(target_os = "macos")]
fn view_size(view: &Vst3ForeignRef<IPlugView>) -> Option<(i32, i32)> {
    if view.failure.failed() {
        return None;
    }
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: The foreign reference retains the view; rect is an exclusive inline output for the synchronous call.
    if crate::ffi_guard::guard(|| unsafe { view.getSize(&mut rect) }).ok()? != kResultOk {
        return None;
    }
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(target_os = "macos")]
impl Vst3GuiHandle {
    /// Attach the plugin's NSView editor to a host-owned NSWindow.
    /// Must be called on the AppKit main thread.
    pub fn open(&self, title: &str) -> Result<(u32, u32), String> {
        if self.controller.failure.failed() {
            return Err(self
                .controller
                .failure
                .message()
                .unwrap_or_else(|| "VST3 instance is quarantined".into()));
        }
        self.open_impl(title, false, EditorAttachVariant::Baseline, None)
    }

    /// Same as [`Self::open`] but keeps the window transparent and unfocused.
    /// Headless lifecycle tests use it so the editor never covers the desktop.
    pub fn open_hidden(&self, title: &str) -> Result<(u32, u32), String> {
        if self.controller.failure.failed() {
            return Err(self
                .controller
                .failure
                .message()
                .unwrap_or_else(|| "VST3 instance is quarantined".into()));
        }
        self.open_impl(title, true, EditorAttachVariant::Baseline, None)
    }

    /// Open with a host variation; the headless probe uses it to isolate what an
    /// editor needs before `attached`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_with_variant(
        &self,
        title: &str,
        hidden: bool,
        variant: EditorAttachVariant,
    ) -> Result<(u32, u32), String> {
        if self.controller.failure.failed() {
            return Err(self
                .controller
                .failure
                .message()
                .unwrap_or_else(|| "VST3 instance is quarantined".into()));
        }
        self.open_impl(title, hidden, variant, None)
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Open an editor with a controllable delay and pre-dispatch hook.
    pub fn open_deferred_for_test(
        &self,
        title: &str,
        delay: Option<&str>,
        before_dispatch: Option<Box<dyn FnOnce()>>,
        after_schedule: Option<Box<dyn FnOnce()>>,
    ) -> Result<(u32, u32), String> {
        self.open_impl(
            title,
            true,
            EditorAttachVariant::Async,
            Some(deferred_attach::Options {
                timing: deferred_attach::Timing::parse(delay)?,
                before_dispatch,
                after_schedule,
            }),
        )
    }

    fn open_impl(
        &self,
        title: &str,
        hidden: bool,
        variant: EditorAttachVariant,
        deferred_override: Option<deferred_attach::Options>,
    ) -> Result<(u32, u32), String> {
        use objc2::rc::Retained;
        use objc2::{MainThreadMarker, MainThreadOnly};
        use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
        use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

        let _timer = oc_diag::perf_counter("plugin.vst3.editor.open.ms").map(oc_diag::Timer::start);
        if self.handles.opening.load(Ordering::Acquire) {
            return Err("VST3 editor is already attaching".into());
        }
        if let Some(reason) = &self.blocked {
            oc_diag::diag_warn!("plugin: vst3 editor refused: {reason}");
            return Err(reason.clone());
        }
        if self.handles.view.load(Ordering::Acquire) != 0
            || self.handles.window.load(Ordering::Acquire) != 0
        {
            // The window's own close button (or the OS) closes it without
            // telling the host, leaving this handle set. A window that is gone
            // is a closed editor: detach it, then open a new one.
            if !self.window_gone() {
                return Err("VST3 editor is already open".to_string());
            }
            oc_diag::diag_info!("plugin: vst3 editor window was closed; reopening");
            self.close()?;
        }
        oc_diag::diag_info!(
            "plugin: vst3 editor open title={title:?} hidden={hidden} variant={variant:?}"
        );
        let mtm = MainThreadMarker::new()
            .ok_or("VST3 editor must be opened on the AppKit main thread")?;
        let deferred_options = if variant == EditorAttachVariant::Async {
            Some(match deferred_override {
                Some(options) => options,
                None => deferred_attach::Options::from_env()?,
            })
        } else {
            None
        };
        let opening = EditorOpening::new(&self.handles)?;
        self.handles.sync_resize.store(false, Ordering::Release);

        // SAFETY: AppKit main was checked above; the retained controller receives a static NUL-terminated view name.
        let raw_view = guard_editor_call(&self.handles, || unsafe {
            self.controller.createView(c"editor".as_ptr())
        })?;
        // SAFETY: createView transfers one owned interface reference, or null; from_raw adopts it without AddRef.
        let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
            oc_diag::diag_warn!("plugin: vst3 createView returned null");
            return Err("VST3 plugin did not create an editor view".to_string());
        };
        let view = Vst3ForeignRef::adopt(view, Arc::clone(&self.handles.failure))
            .with_editor_parent(&self.controller);
        oc_diag::diag_debug!("plugin: vst3 createView ok");
        // SAFETY: The adopted view remains retained on main and the platform identifier is the SDK static C string.
        if guard_editor_call(&self.handles, || unsafe {
            view.isPlatformTypeSupported(kPlatformTypeNSView)
        })? != kResultOk
        {
            return Err("VST3 editor does not support the macOS NSView platform".to_string());
        }
        // A frame-no-resize host never touches AppKit from a view callback.
        if variant == EditorAttachVariant::FrameNoResize {
            self.handles.attaching.store(true, Ordering::Release);
        }
        if variant != EditorAttachVariant::FrameLate
            // SAFETY: The main-thread view and host frame are both retained by this editor through attachment.
            && guard_editor_call(&self.handles, || unsafe {
                view.setFrame(self.frame.as_ptr())
            })? != kResultOk
        {
            self.handles.attaching.store(false, Ordering::Release);
            return Err("VST3 editor rejected the host plug frame".to_string());
        }
        // A view is not required to know its size before `attached`: a Qt editor
        // answers `kNotInitialized` until its widget is in a window, and then
        // reports the real size from `resizeView`. Open the window at the host
        // default and take the plugin's own size once it attaches.
        let reported = view_size(&view);
        // A window this handle already had keeps its size across a re-attach;
        // the fresh view is told about it after `attached`.
        let remembered = *self.handles.last_size.lock().unwrap();
        // Only a window that opened at the host default adopts the size the
        // view reports once it is attached: a remembered size is a host
        // decision and outranks what the view answers.
        let opened_at_default = reported.is_none() && remembered.is_none();
        let (width, height) = remembered.or(reported).unwrap_or(DEFAULT_EDITOR_SIZE);
        if reported.is_none() && remembered.is_none() {
            oc_diag::diag_info!(
                "plugin: vst3 editor size is unavailable before attach; opening {width}x{height}"
            );
        }

        let frame_rect = NSRect::new(
            NSPoint::new(100.0, 100.0),
            NSSize::new(width as f64, height as f64),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            // SAFETY: The adopted view remains live on the checked AppKit main thread through this query.
            | if guard_editor_call(&self.handles, || unsafe { view.canResize() })? == kResultOk {
                NSWindowStyleMask::Resizable
            } else {
                NSWindowStyleMask::empty()
            };
        // SAFETY: NSWindow is created on the main thread, checked above.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        let mut window = EditorWindowLease {
            window: ManuallyDrop::new(window),
            handles: Arc::clone(&self.handles),
            published: false,
            committed: false,
        };
        // We own a retain through `handles.window`; close must not consume it
        // again, or AppKit deallocates the window while its close animation runs.
        // SAFETY: The main-thread window lease retains this window; disabling automatic release keeps lease ownership valid.
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setTitle(&NSString::from_str(title));
        if hidden {
            window.setAlphaValue(0.0);
        }
        let Some(default_content) = window.contentView() else {
            self.handles.attaching.store(false, Ordering::Release);
            window.close();
            return Err("NSWindow contentView is null".to_string());
        };

        if variant == EditorAttachVariant::Layer {
            default_content.setWantsLayer(true);
        }
        // Keep an alternate parent alive until `attached` returns.
        let mut attached_parent = EditorParentLease {
            value: ManuallyDrop::new(None),
            failure: Arc::clone(&self.handles.failure),
        };
        let parent = if variant == EditorAttachVariant::ContentView {
            // SAFETY: The class is an NSView subclass allocated on main; initWithFrame receives a valid by-value NSRect.
            let container: Retained<Vst3EditorContainer> = unsafe {
                objc2::msg_send![Vst3EditorContainer::alloc(mtm), initWithFrame: frame_rect]
            };
            window.setContentView(Some(&container));
            let container = container.into_super();
            let pointer = Retained::as_ptr(&container) as *mut c_void;
            *attached_parent = Some(container);
            pointer
        } else if variant == EditorAttachVariant::ChildWindow {
            let child = NSView::initWithFrame(NSView::alloc(mtm), frame_rect);
            default_content.addSubview(&child);
            // A visible window makes `[parent window]` live during attach.
            window.makeKeyAndOrderFront(None);
            window.displayIfNeeded();
            let pointer = Retained::as_ptr(&child) as *mut c_void;
            *attached_parent = Some(child);
            pointer
        } else if variant == EditorAttachVariant::Detached {
            let child = NSView::initWithFrame(NSView::alloc(mtm), frame_rect);
            let pointer = Retained::as_ptr(&child) as *mut c_void;
            *attached_parent = Some(child);
            pointer
        } else {
            Retained::as_ptr(&default_content) as *mut c_void
        };

        if variant == EditorAttachVariant::Scale
            && let Some(scale) = view.cast::<IPlugViewContentScaleSupport>()
        {
            let factor = window.backingScaleFactor() as f32;
            self.handles.check()?;
            // SAFETY: optional scale interface remains owned for this UI-thread call.
            let _ = unsafe { scale.setContentScaleFactor(factor) };
        }

        if variant == EditorAttachVariant::OnSizeEarly {
            let mut early = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            self.handles.check()?;
            // SAFETY: retained native view and stack rectangle outlive this UI call.
            let _ = unsafe { view.onSize(&mut early) };
        }

        // Publish the window before attach: `attached` may request a resize.
        opening.check()?;
        window.publish()?;
        if variant == EditorAttachVariant::OnSizeSync {
            self.handles.sync_resize.store(true, Ordering::Release);
        }
        if variant != EditorAttachVariant::FrameNoResize {
            self.handles.attaching.store(true, Ordering::Release);
        }
        oc_diag::diag_debug!("plugin: vst3 attached(NSView) start parent={parent:p}");
        let attached = if let Some(options) = deferred_options {
            deferred_attach::run(
                deferred_attach::Request {
                    handles: &self.handles,
                    view: view.as_ptr(),
                    parent,
                    main: mtm,
                    cancellation: deferred_attach::Cancellation::opening(
                        &self.handles,
                        opening.generation,
                        Retained::as_ptr(&window) as usize,
                    ),
                },
                options,
            )?
        } else {
            // SAFETY: The checked-main editor retains the view and its selected NSView parent through attached.
            guard_editor_call(&self.handles, || unsafe {
                view.attached(parent, kPlatformTypeNSView)
            })?
        };
        let mut attachment = EditorAttachedLease {
            view: &view,
            armed: attached == kResultOk,
        };
        opening.check()?;
        self.handles.attaching.store(false, Ordering::Release);
        if attached != kResultOk {
            *self.handles.pending_size.lock().unwrap() = None;
            return Err("VST3 editor failed to attach to the host NSView".to_string());
        }
        if variant == EditorAttachVariant::FrameLate
            // SAFETY: The editor retains both interfaces on main; setFrame borrows the host frame for the live view.
            && guard_editor_call(&self.handles, || unsafe {
                view.setFrame(self.frame.as_ptr())
            })? != kResultOk
        {
            opening.check()?;
            *self.handles.pending_size.lock().unwrap() = None;
            return Err("VST3 editor rejected the host plug frame".to_string());
        }
        opening.check()?;
        let mut final_width = width;
        let mut final_height = height;
        let mut deferred = false;
        // The size the view currently knows about, so the re-attach rule below
        // does not repeat a notification it already has.
        let mut delivered: Option<(i32, i32)> = None;
        if let Some((width, height)) = self.handles.pending_size.lock().unwrap().take() {
            deferred = true;
            // Apply the size the editor asked for while it was attaching.
            window.setContentSize(NSSize::new(width as f64, height as f64));
            opening.check()?;
            let mut pending = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // SAFETY: The main-thread view stays retained and pending is a writable stack ViewRect for this call.
            let _ = guard_editor_call(&self.handles, || unsafe { view.onSize(&mut pending) });
            opening.check()?;
            delivered = Some((width, height));
            final_width = width;
            final_height = height;
        }
        // Prefer a size the view only reports once it is attached over the
        // default this window was opened with. A resize the editor requested
        // during `attached` is already applied and wins: querying the view
        // again would overwrite it with the size it had before.
        if !deferred
            && opened_at_default
            && let Some((width, height)) = view_size(&view)
            && (width, height) != (final_width, final_height)
        {
            opening.check()?;
            window.setContentSize(NSSize::new(width as f64, height as f64));
            opening.check()?;
            let mut reported = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // SAFETY: The main-thread view stays retained and reported is a writable stack ViewRect for this call.
            let _ = guard_editor_call(&self.handles, || unsafe { view.onSize(&mut reported) });
            opening.check()?;
            delivered = Some((width, height));
            final_width = width;
            final_height = height;
        }
        // A plugin that could not answer `canResize` before attach may allow it
        // once its view exists.
        opening.check()?;
        // SAFETY: the local owner retains the view on main through this query.
        let resizable = guard_editor_call(&self.handles, || unsafe { view.canResize() })?;
        opening.check()?;
        if resizable == kResultOk {
            window.setStyleMask(window.styleMask() | NSWindowStyleMask::Resizable);
        }
        opening.check()?;
        // A re-attached view is new: hand it the size this handle's window was
        // left at, or it lays out at its own default inside a differently sized
        // window.
        if remembered.is_some() && delivered != Some((final_width, final_height)) {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: final_width,
                bottom: final_height,
            };
            // SAFETY: The retained main-thread view borrows the writable rect only for this notification.
            let _ = guard_editor_call(&self.handles, || unsafe { view.onSize(&mut rect) });
            opening.check()?;
        }
        opening.check()?;
        *self.handles.last_size.lock().unwrap() = Some((final_width, final_height));
        if hidden {
            window.orderFront(None);
        } else {
            window.makeKeyAndOrderFront(None);
        }
        opening.check()?;
        if variant == EditorAttachVariant::Detached
            && let Some(child) = attached_parent.as_ref()
        {
            default_content.addSubview(child);
        }
        opening.check()?;
        self.handles.sync_resize.store(false, Ordering::Release);

        opening.check()?;
        attachment.armed = false;
        drop(attachment);
        publish_editor_view(
            Arc::clone(&self.handles),
            view,
            Arc::clone(&self.frame),
            Arc::clone(&self.controller),
        );
        opening.check()?;
        window.commit();
        Ok((final_width as u32, final_height as u32))
    }

    /// Detach and destroy the host window. Must run on the AppKit main thread.
    pub fn close(&self) -> Result<(), String> {
        if !is_main_thread() {
            return Err("VST3 editor must be closed on the AppKit main thread".into());
        }
        self.handles.cancel_attach();
        if self.controller.failure.failed() {
            return Err(self
                .controller
                .failure
                .message()
                .unwrap_or_else(|| "VST3 instance is quarantined".into()));
        }
        use objc2::rc::Retained;
        use objc2_app_kit::NSWindow;

        let view = self.handles.view.swap(0, Ordering::AcqRel);
        let window = self.handles.window.swap(0, Ordering::AcqRel);
        if window != 0 {
            // Remember the size the user left the window at, so reopening the
            // editor does not shrink it back to the plugin's default.
            // SAFETY: `open` stored a retained NSWindow pointer and the caller
            // is on the AppKit main thread.
            let window = unsafe { &*(window as *const objc2_app_kit::NSWindow) };
            if let Some(content) = window.contentView() {
                let size = content.frame().size;
                *self.handles.last_size.lock().unwrap() =
                    Some((size.width as i32, size.height as i32));
            }
        }
        let view_owner = take_editor_view(&self.handles);
        if view != 0
            && let Some(owner) = view_owner
        {
            // SAFETY: the main-only Rc owner pins the view, frame/controller and
            // callback backing throughout the typed native detach call.
            let removed = unsafe { vst3_guard::view_removed(owner.view.as_ptr()) };
            if let Err(error) = editor_cleanup_result(&self.handles.failure, removed) {
                std::mem::forget(owner);
                return Err(error);
            }
            // SAFETY: the retained view detached successfully; clearing its host
            // frame borrows the same complete owner until the native call returns.
            let frame = unsafe { vst3_guard::view_set_frame(owner.view.as_ptr(), ptr::null_mut()) };
            if let Err(error) = editor_cleanup_result(&self.handles.failure, frame) {
                std::mem::forget(owner);
                return Err(error);
            }
        }
        self.handles.check()?;
        if window != 0 {
            // SAFETY: `open` transferred exactly one Objective-C retain here.
            if let Some(window) = unsafe { Retained::<NSWindow>::from_raw(window as *mut NSWindow) }
            {
                window.close();
            }
        }
        Ok(())
    }

    /// Whether the editor is on screen. AppKit main thread only.
    pub fn is_open(&self) -> bool {
        self.handles.view.load(Ordering::Acquire) != 0
            && self.handles.window.load(Ordering::Acquire) != 0
            && !self.window_gone()
    }

    /// Whether the host window was closed from its title bar or by the OS.
    ///
    /// `close` is the only path that clears the handle, so a window the user
    /// closed still looks open. Minimized windows are still open. A handle that
    /// never opened a window counts as gone.
    /// AppKit main thread only: a caller on another thread must dispatch here.
    pub fn window_gone(&self) -> bool {
        if self.handles.failure.failed() {
            return false;
        }
        if !is_main_thread() {
            return false;
        }
        let pointer = self.handles.window.load(Ordering::Acquire);
        if pointer == 0 {
            return true;
        }
        // SAFETY: `open` stores a retained NSWindow and the handle keeps that
        // retain while it holds the pointer, so a closed window stays alive.
        // The caller is on the AppKit main thread.
        let window = unsafe { &*(pointer as *const objc2_app_kit::NSWindow) };
        !window.isVisible() && !window.isMiniaturized()
    }

    /// Close the host window the way its own close button does, leaving the
    /// handle untouched as AppKit does. Headless lifecycle tests use it.
    /// AppKit main thread only.
    pub fn simulate_close_button(&self) {
        if self.handles.failure.failed() {
            return;
        }
        if !is_main_thread() {
            return;
        }
        let pointer = self.handles.window.load(Ordering::Acquire);
        if pointer == 0 {
            return;
        }
        // SAFETY: `open` stored a retained NSWindow pointer; the caller is on
        // the thread that owns it.
        let window = unsafe { &*(pointer as *const objc2_app_kit::NSWindow) };
        window.performClose(None);
    }

    /// Window number for external capture tools. AppKit main thread only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn window_number(&self) -> Option<i64> {
        if self.handles.failure.failed() {
            return None;
        }
        if !is_main_thread() {
            return None;
        }
        let pointer = self.handles.window.load(Ordering::Acquire);
        if pointer == 0 {
            return None;
        }
        // SAFETY: `open` stored a retained NSWindow pointer; the caller is on
        // the thread that owns it.
        let window = unsafe { &*(pointer as *const objc2_app_kit::NSWindow) };
        Some(window.windowNumber() as i64)
    }

    /// Why this editor must not be attached, when the plugin is known to crash.
    pub fn blocked_reason(&self) -> Option<&str> {
        self.blocked.as_deref()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Lease the editor backing and return a lifetime probe.
    pub fn backing_probe_for_test(&self) -> Result<Vst3EditorBackingProbe, String> {
        let owner = loan_editor_view(&self.handles)?;
        Ok(Vst3EditorBackingProbe {
            frame: Arc::downgrade(&owner._frame),
            controller: Arc::downgrade(&owner._controller),
            handles: Arc::downgrade(&owner._handles),
        })
    }

    /// Handle around a host-owned controller, for exercising the lifecycle
    /// without loading a plugin bundle. The handle owns the controller reference.
    pub fn for_controller(controller: ComPtr<IEditController>) -> Self {
        let failure = Arc::new(Vst3Failure::default());
        let handles = Arc::new(Vst3EditorHandles::new(Arc::clone(&failure)));
        let frame = ComWrapper::new(Vst3PlugFrame::new(Arc::clone(&handles)))
            .to_com_ptr::<IPlugFrame>()
            .expect("IPlugFrame wrapper exposes its interface");
        Self {
            controller: Arc::new(EditorControllerLease::new(Vst3ForeignRef::adopt(
                controller,
                Arc::clone(&failure),
            ))),
            frame: Arc::new(HostFrame {
                frame: ManuallyDrop::new(frame),
                failure,
            }),
            handles,
            blocked: None,
        }
    }
}

impl Vst3ClassInfo {
    /// Check if this is an instrument (synth) based on subcategories.
    pub fn is_instrument(&self) -> bool {
        let sub = self.sub_categories.to_lowercase();
        sub.contains("instrument") || sub.contains("synth") || sub.contains("generator")
    }
}

/// Editors that must stay out of the host process, keyed by class name.
///
/// The table is empty now that host services supply `IMessage`; it remains as
/// the containment hook for an editor that regresses in-process.
/// `OCP_VST3_EDITOR_FORCE=1` overrides an entry for re-measurement.
#[cfg(any(target_os = "macos", test))]
const UNHOSTABLE_EDITORS: &[&str] = &[];

#[cfg(target_os = "macos")]
fn unhostable_editor_reason(info: &Vst3ClassInfo) -> Option<String> {
    if std::env::var_os("OCP_VST3_EDITOR_FORCE").is_some() {
        return None;
    }
    blocked_editor_reason(info)
}

#[cfg(any(target_os = "macos", test))]
fn blocked_editor_reason(info: &Vst3ClassInfo) -> Option<String> {
    blocked_editor_reason_in(info, UNHOSTABLE_EDITORS)
}

#[cfg(any(target_os = "macos", test))]
fn blocked_editor_reason_in(info: &Vst3ClassInfo, table: &[&str]) -> Option<String> {
    table
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&info.name))
        .then(|| {
            "This plugin's editor crashes inside the plugin before it can be shown; \
             edit its parameters in the inspector instead."
                .to_string()
        })
}

struct Vst3LoadContext {
    host_context: *mut vst3::Steinberg::FUnknown,
    component_handler: *mut IComponentHandler,
    transfer_initial_state: bool,
    activate_on_load: bool,
    failure: Arc<Vst3Failure>,
}

struct Vst3LoadingParts {
    component: Vst3ForeignRef<IComponent>,
    processor: Vst3ForeignRef<IAudioProcessor>,
    controller: Option<Vst3ForeignRef<IEditController>>,
    controller_is_separate: bool,
    owned_host: Option<ComWrapper<Vst3Host>>,
    component_handler: *mut IComponentHandler,
    factory: Option<Vst3ForeignRef<IPluginFactory>>,
    factory2: Option<Vst3ForeignRef<IPluginFactory2>>,
    lib_handle: *mut c_void,
    class_info: Vst3ClassInfo,
    failure: Arc<Vst3Failure>,
    component_initialized: bool,
    controller_initialized: bool,
}

/// The last failure a hosted plugin reported, kept after the instance is gone.
///
/// A teardown failure happens while the instance is being dropped, so the
/// caller cannot read it from [`Vst3Instance::take_fault`]. Hosts keep this
/// handle before dropping the instance and report it afterwards.
#[derive(Default)]
pub struct Vst3Failure {
    slot: FirstFault<Vst3ProcessError>,
}
impl Vst3Failure {
    /// Whether a guarded plugin call failed.
    pub fn failed(&self) -> bool {
        self.slot.failed()
    }
    /// Format the immutable cause on a reporting/control thread.
    pub fn message(&self) -> Option<String> {
        self.slot.first().map(|error| error.to_string())
    }
    /// First recorded process fault.
    pub fn first_fault(&self) -> Option<Vst3ProcessError> {
        self.slot.first()
    }
    /// Take the fault for control-side reporting.
    pub fn take_report(&self) -> Option<Vst3ProcessError> {
        self.slot.take_report()
    }
}

/// A live VST3 plugin instance.
pub struct Vst3Instance {
    _lib_handle: *mut std::ffi::c_void,
    class_info: Box<Vst3ClassInfo>,
    component: ManuallyDrop<Vst3ForeignRef<IComponent>>,
    processor: ManuallyDrop<Vst3ForeignRef<IAudioProcessor>>,
    controller: ManuallyDrop<Option<Vst3ForeignRef<IEditController>>>,
    controller_is_separate: bool,
    component_connection: ManuallyDrop<Option<Vst3ForeignRef<IConnectionPoint>>>,
    controller_connection: ManuallyDrop<Option<Vst3ForeignRef<IConnectionPoint>>>,
    #[allow(dead_code)]
    sample_rate: f64,
    max_frames: i32,
    /// Declared audio input buses, in VST3 order. Instruments have none, and
    /// passing a bus in `ProcessData` that the component never declared is out
    /// of contract.
    input_bus_channels: Vec<i32>,
    /// Sidechain input the project routes into; 0 is the first declared one.
    sidechain_input_index: usize,
    active: bool,
    processing: bool,
    pending_events: Vec<Vst3PendingEvent>,
    note_ids: Vst3NoteIds,
    output_layout: ManuallyDrop<Vst3OutputLayout>,
    #[cfg(target_os = "macos")]
    editor_handles: Arc<Vst3EditorHandles>,
    #[cfg(target_os = "macos")]
    plug_frame: ManuallyDrop<Option<ComPtr<IPlugFrame>>>,
    /// Host services owned by this instance when the caller passed no context.
    _host: ManuallyDrop<Option<ComWrapper<Vst3Host>>>,
    /// Editor edits this instance publishes and drains, keyed by preset name.
    edit_bus: Option<Arc<ParamEditBus>>,
    edit_receiver: Option<ParamEditReceiver>,
    /// Last normalized value the host set for each parameter, so a controller
    /// that refuses a component state can still be synchronized.
    applied_values: Vec<(ParamID, f64)>,
    /// Failure record that outlives the instance, so teardown failures are
    /// reportable too.
    failure: Arc<Vst3Failure>,
    /// Planar input buses, allocated once and reused on every block.
    input_layout: ManuallyDrop<Vst3InputLayout>,
    /// Reused event and parameter scratch, so a block does not allocate.
    event_scratch: Vec<Event>,
    param_scratch: Vec<Vst3ParamEvent>,
    /// Reused `ProcessData` queues; built on the first block.
    process_queues: ManuallyDrop<Option<Vst3ProcessQueues>>,
    /// Name lookup for parameter writes, rebuilt after a title change.
    param_cache: ParamCache,
    authored_parameter_revision: u64,
    /// Fault the next call at this point throws, for the barrier tests.
    #[cfg(any(test, feature = "test-support"))]
    fault_injection: Option<FaultPoint>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fixture_handler_guard: Option<test_support::editor_handler::Scope>,
}

/// Negotiated output buses with reusable planar render buffers.
///
/// The bus descriptors and pointer arrays point into `channels`, which never
/// moves after construction; rebuild the layout instead of resizing in place.
struct Vst3OutputLayout {
    bus_channel_counts: Vec<i32>,
    channels: Vec<Vec<f32>>,
    _bus_pointers: Vec<Vec<*mut Sample32>>,
    buses: Vec<AudioBusBuffers>,
    input_channels: [Vec<f32>; 2],
    /// Stereo scratch the effect path renders into before interleaving.
    effect_outputs: [Vec<f32>; 2],
    frames: usize,
}

impl Vst3OutputLayout {
    fn new(bus_channel_counts: Vec<i32>, frames: usize) -> Self {
        let mut channels = Vec::new();
        let mut bus_pointers = Vec::new();
        let mut buses = Vec::new();
        for &count in &bus_channel_counts {
            let mut pointers = Vec::with_capacity(count.max(0) as usize);
            for _ in 0..count.max(0) {
                channels.push(vec![0.0; frames]);
                pointers.push(channels.last_mut().unwrap().as_mut_ptr());
            }
            let channel_buffers = if pointers.is_empty() {
                ptr::null_mut()
            } else {
                pointers.as_mut_ptr()
            };
            buses.push(AudioBusBuffers {
                numChannels: count,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: channel_buffers,
                },
            });
            bus_pointers.push(pointers);
        }
        Self {
            bus_channel_counts,
            channels,
            _bus_pointers: bus_pointers,
            buses,
            input_channels: [vec![0.0; frames], vec![0.0; frames]],
            effect_outputs: [vec![0.0; frames], vec![0.0; frames]],
            frames,
        }
    }

    fn ensure_frames(&mut self, frames: usize) {
        if frames > self.frames {
            *self = Self::new(self.bus_channel_counts.clone(), frames);
        }
    }

    fn clear(&mut self, frames: usize) {
        for channel in &mut self.channels {
            channel[..frames].fill(0.0);
        }
        for channel in &mut self.input_channels {
            channel[..frames].fill(0.0);
        }
    }

    fn pair_count(&self) -> usize {
        self.channels.len().div_ceil(2)
    }

    fn bus_count(&self) -> usize {
        self.bus_channel_counts.len()
    }
}

/// Declared input buses with reusable planar buffers.
///
/// Every buffer depends only on the declared channel counts and the frame
/// capacity, so [`ensure`](Self::ensure) rebuilds the layout only when one of
/// those changes. The bus descriptors and pointer arrays point into `channels`,
/// which never moves after construction.
struct Vst3InputLayout {
    bus_channel_counts: Vec<i32>,
    /// Per bus, per channel, samples.
    channels: Vec<Vec<Vec<f32>>>,
    _bus_pointers: Vec<Vec<*mut Sample32>>,
    buses: Vec<AudioBusBuffers>,
    frames: usize,
}

impl Vst3InputLayout {
    fn new(bus_channel_counts: Vec<i32>, frames: usize) -> Self {
        let mut channels = Vec::with_capacity(bus_channel_counts.len());
        let mut bus_pointers = Vec::with_capacity(bus_channel_counts.len());
        let mut buses = Vec::with_capacity(bus_channel_counts.len());
        for &count in &bus_channel_counts {
            let channels_in_bus = count.max(0) as usize;
            let mut bus_channels = Vec::with_capacity(channels_in_bus);
            let mut pointers = Vec::with_capacity(channels_in_bus);
            for _ in 0..channels_in_bus {
                let mut data = vec![0.0f32; frames];
                pointers.push(data.as_mut_ptr());
                bus_channels.push(data);
            }
            let channel_buffers = if pointers.is_empty() {
                ptr::null_mut()
            } else {
                pointers.as_mut_ptr()
            };
            buses.push(AudioBusBuffers {
                numChannels: count,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: channel_buffers,
                },
            });
            channels.push(bus_channels);
            bus_pointers.push(pointers);
        }
        Self {
            bus_channel_counts,
            channels,
            _bus_pointers: bus_pointers,
            buses,
            frames,
        }
    }

    /// Grow the layout only when the bus shape or the frame capacity changed.
    fn ensure(&mut self, bus_channel_counts: &[i32], frames: usize) {
        if frames <= self.frames && bus_channel_counts == self.bus_channel_counts.as_slice() {
            return;
        }
        let capacity = frames.max(self.frames);
        *self = Self::new(bus_channel_counts.to_vec(), capacity);
    }

    /// Deinterleave `buffer` into bus 0 and the selected sidechain bus.
    ///
    /// Every other declared input bus stays silent. The caller has already
    /// grown the layout to at least `frames`.
    fn write(
        &mut self,
        buffer: &[f32],
        sidechain: Option<&[f32]>,
        sidechain_bus: Option<usize>,
        frames: usize,
    ) {
        for (bus, channels) in self.channels.iter_mut().enumerate() {
            for (channel, data) in channels.iter_mut().enumerate() {
                let data = &mut data[..frames];
                if bus == 0 {
                    for (frame, sample) in data.iter_mut().enumerate() {
                        *sample = match channel {
                            0 => buffer[frame * 2],
                            1 => buffer[frame * 2 + 1],
                            _ => 0.0,
                        };
                    }
                } else if sidechain_bus == Some(bus) {
                    if let Some(sidechain) = sidechain {
                        for (frame, sample) in data.iter_mut().enumerate() {
                            *sample = sidechain.get(frame * 2 + channel).copied().unwrap_or(0.0);
                        }
                    } else {
                        data.fill(0.0);
                    }
                } else {
                    data.fill(0.0);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Vst3PendingEvent {
    Note(Vst3NoteEvent),
    PolyPressure(Vst3PolyPressureEvent),
    NoteExpression(Vst3NoteExpressionEvent),
    Param(Vst3ParamEvent),
}

#[derive(Clone, Copy)]
struct Vst3NoteEvent {
    on: bool,
    channel: i16,
    key: i16,
    velocity: f32,
    sample_offset: i32,
    note_id: i32,
}

#[derive(Clone, Copy)]
struct Vst3ParamEvent {
    id: ParamID,
    value: ParamValue,
    sample_offset: i32,
}

#[derive(Clone, Copy)]
struct Vst3PolyPressureEvent {
    channel: i16,
    key: i16,
    pressure: f32,
    sample_offset: i32,
    note_id: i32,
}

#[derive(Clone, Copy)]
struct Vst3NoteExpressionEvent {
    type_id: NoteExpressionTypeID,
    value: f64,
    sample_offset: i32,
    note_id: i32,
}

/// Borrowed COM pointers of the held interfaces. Valid while the instance lives.
#[derive(Debug, Clone, Copy)]
pub struct RawInterfaces {
    pub component: *mut c_void,
    pub processor: *mut c_void,
    pub controller: *mut c_void,
}

/// Whether this thread is the AppKit main thread.
///
/// Hosts without AppKit have no main-thread-only plugin contract, so every
/// thread reports as the main one there.
pub fn is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2::MainThreadMarker::new().is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Run `work` on the AppKit main thread, dispatching and waiting when the
/// caller is somewhere else.
///
/// The call blocks until the main thread has run `work`, which is the contract
/// VST3 puts on instantiation and on every editor call. Calling it from the
/// main thread runs `work` inline, so a caller that already hopped pays nothing.
pub fn run_on_main_thread<F, T>(work: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    #[cfg(target_os = "macos")]
    {
        macos_main_thread::run(work)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(work())
    }
}

#[cfg(target_os = "macos")]
mod macos_main_thread {
    use std::cell::Cell;
    use std::ffi::c_void;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
    use objc2::{AnyThread, DefinedClass, msg_send, sel};

    /// Work moved to the main thread, with the slot its result lands in.
    struct Job<F, T> {
        work: Option<F>,
        result: Option<T>,
    }

    /// The callback the main thread runs, monomorphized per job type.
    extern "C" fn run_job<F: FnOnce() -> T, T>(context: *mut c_void) {
        // SAFETY: the dispatching thread keeps the `Job` on its stack until
        // `runJob` has returned.
        let job = unsafe { &mut *(context as *mut Job<F, T>) };
        if let Some(work) = job.work.take() {
            job.result = Some(work());
        }
    }

    struct JobIvars {
        context: Cell<usize>,
        run: Cell<Option<extern "C" fn(*mut c_void)>>,
    }

    objc2::define_class!(
        // Allocated on the calling thread and retained by the dispatch; only
        // `runJob` runs on the main thread.
        #[unsafe(super(objc2_foundation::NSObject))]
        #[ivars = JobIvars]
        struct MainThreadJob;

        unsafe impl NSObjectProtocol for MainThreadJob {}

        impl MainThreadJob {
            #[unsafe(method(runJob))]
            fn run_job(&self) {
                let Some(run) = self.ivars().run.get() else {
                    return;
                };
                // `context` points at the `Job` the dispatching thread keeps
                // alive across the blocking dispatch.
                run(self.ivars().context.get() as *mut c_void);
            }
        }
    );

    pub(super) fn run<F, T>(work: F) -> Result<T, String>
    where
        F: FnOnce() -> T + Send,
        T: Send,
    {
        if super::is_main_thread() {
            debug_assert!(
                super::is_main_thread(),
                "VST3 plugin work must run on the AppKit main thread"
            );
            return Ok(work());
        }
        let mut job = Job {
            work: Some(work),
            result: None,
        };
        let target = MainThreadJob::alloc().set_ivars(JobIvars {
            context: Cell::new(&mut job as *mut Job<F, T> as usize),
            run: Cell::new(Some(run_job::<F, T>)),
        });
        // SAFETY: MainThreadJob is an allocated NSObject subclass with initialized ivars; super init preserves its owned retain.
        let target: Retained<MainThreadJob> = unsafe { msg_send![super(target), init] };
        // SAFETY: `performSelectorOnMainThread:withObject:waitUntilDone:` is an
        // `NSObject` category method; `YES` makes it block until `runJob` has
        // returned, so `job.result` is written before this call returns.
        let _: () = unsafe {
            msg_send![
                &*target,
                performSelectorOnMainThread: sel!(runJob),
                withObject: Option::<&AnyObject>::None,
                waitUntilDone: Bool::YES,
            ]
        };
        job.result
            .ok_or_else(|| "the AppKit main thread did not run the plugin work".to_string())
    }
}

// SAFETY: the host transfers exclusive instance ownership between control and
// processing workers; it does not concurrently mutate a single instance. Native
// callback backing is retained by shared owners until healthy release or quarantine.
unsafe impl Send for Vst3Instance {}

impl Vst3Instance {
    /// Borrowed COM interface pointers of this instance; valid only while the
    /// instance is alive.
    pub fn raw_interfaces(&self) -> RawInterfaces {
        RawInterfaces {
            component: self.component.as_ptr() as *mut c_void,
            processor: self.processor.as_ptr() as *mut c_void,
            controller: self
                .controller
                .as_ref()
                .map(|controller| controller.as_ptr() as *mut c_void)
                .unwrap_or(ptr::null_mut()),
        }
    }

    /// Reported tail in samples; `u32::MAX` after a fault.
    pub fn tail_samples(&self) -> u32 {
        if self.failed() {
            return u32::MAX;
        }
        // SAFETY: this healthy instance retains its processor for the native call.
        self.native_value(Operation::Tail, unsafe {
            vst3_guard::tail(self.processor.as_ptr())
        })
        .unwrap_or(u32::MAX)
    }

    fn processor_latency_samples(&self) -> u32 {
        if self.failed() {
            return 0;
        }
        // SAFETY: this healthy instance retains its processor for the native call.
        self.native_value(Operation::Latency, unsafe {
            vst3_guard::latency(self.processor.as_ptr())
        })
        .unwrap_or(0)
    }

    fn native_value<T>(&self, operation: Operation, result: Result<T, Fault>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(fault) => {
                self.native_fault(operation, fault);
                None
            }
        }
    }

    fn native_result<T>(&self, result: Result<T, Vst3ProcessError>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.failure.slot.record(error);
                None
            }
        }
    }

    /// Declared tail; an infinite tail stays `u32::MAX` so the idle gate
    /// fails closed.
    pub fn plugin_tail_samples(&self) -> Option<u32> {
        Some(self.tail_samples())
    }

    /// Take the last failure a plugin reported by throwing.
    ///
    /// A guarded call that fails on the audio path leaves the block silent and
    /// stores the message here, so the caller can report it instead of the
    /// process aborting.
    pub fn take_fault(&mut self) -> Option<String> {
        self.failure.take_report().map(|error| error.to_string())
    }

    /// Failure record shared with callers that drop the instance.
    pub fn failure(&self) -> Arc<Vst3Failure> {
        Arc::clone(&self.failure)
    }

    /// True once a guarded plugin call, including teardown, failed.
    pub fn failed(&self) -> bool {
        self.failure.failed()
    }

    /// Record and log a guarded call that failed.
    #[cfg(any(test, feature = "test-support"))]
    fn note_failure(&self, message: String) {
        self.failure.slot.record(Vst3ProcessError::native(
            Operation::Legacy,
            Fault::from_message(FaultKind::CppException, &message),
        ));
        oc_diag::diag_warn!("plugin: {message}");
    }
    fn native_fault(&self, operation: Operation, fault: Fault) -> Vst3ProcessError {
        let error = Vst3ProcessError::native(operation, fault);
        self.failure.slot.record(error);
        self.failure.first_fault().unwrap_or(error)
    }
    #[expect(
        clippy::result_large_err,
        reason = "the native cause stays inline so failure handling never allocates on the audio thread"
    )]
    fn healthy(&self) -> Result<(), Vst3ProcessError> {
        if self.failed() {
            Err(self.failure.first_fault().unwrap_or_else(|| {
                Vst3ProcessError::native(Operation::Legacy, Fault::quarantined())
            }))
        } else {
            Ok(())
        }
    }
    fn control_result(
        &self,
        operation: Operation,
        result: Result<tresult, Fault>,
    ) -> Result<tresult, String> {
        result.map_err(|fault| self.native_fault(operation, fault).to_string())
    }
    #[cfg(any(test, feature = "test-support"))]
    fn injected_native_fault(&self, point: FaultPoint) -> Option<Fault> {
        if self.fault_injection != Some(point) {
            return None;
        }
        let text = match point {
            FaultPoint::Process => "the fake plugin threw in process",
            FaultPoint::GetState => "the fake plugin threw in getState",
            FaultPoint::SetState => "the fake plugin threw in setState",
            FaultPoint::SetComponentState => "the fake plugin threw in setComponentState",
            FaultPoint::CreateView => "the fake plugin threw in createView",
            FaultPoint::Terminate => return None,
        };
        Some(Fault::from_message(FaultKind::CppException, text))
    }

    /// Inject a deterministic synthetic failure before native dispatch.
    /// Native exception safety is verified by the optimized C++ fixture instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject_fault(&mut self, point: Option<FaultPoint>) {
        self.fault_injection = point;
    }

    /// Collect the change notifications the plugin sent through
    /// `IComponentHandler::restartComponent` and re-read what they invalidate.
    ///
    /// A plugin that loads a program changes its parameter values behind the
    /// host's back. Ignoring the notification leaves editors, automation and
    /// the preset state on the previous values, so this publishes the fresh
    /// controller values and re-negotiates the buses. UI thread only, and only
    /// for an instance that owns its host services.
    pub fn poll_host_events(&mut self) -> Vec<Vst3HostEvent> {
        if self.failed() {
            return Vec::new();
        }
        let Some(host) = &*self._host else {
            return Vec::new();
        };
        let flags = host.edit_sink().take_restart();
        if flags == 0 {
            return Vec::new();
        }
        let mut events = Vec::new();
        if flags & RestartFlags_::kParamValuesChanged != 0 {
            self.publish_controller_values();
            events.push(Vst3HostEvent::ParamValuesChanged);
        }
        if flags & RestartFlags_::kParamTitlesChanged != 0 {
            self.invalidate_param_cache();
            events.push(Vst3HostEvent::ParamTitlesChanged);
        }
        if flags & (RestartFlags_::kIoChanged | RestartFlags_::kRoutingInfoChanged) != 0 {
            self.reload_output_layout();
            events.push(Vst3HostEvent::IoChanged);
        }
        if flags & RestartFlags_::kLatencyChanged != 0 {
            let _ = self.processor_latency_samples();
            events.push(Vst3HostEvent::LatencyChanged);
        }
        if flags & RestartFlags_::kMidiCCAssignmentChanged != 0 {
            events.push(Vst3HostEvent::MidiCcAssignmentChanged);
        }
        if flags & RestartFlags_::kNoteExpressionChanged != 0 {
            events.push(Vst3HostEvent::NoteExpressionChanged);
        }
        if flags & RestartFlags_::kKeyswitchChanged != 0 {
            events.push(Vst3HostEvent::KeyswitchChanged);
        }
        if flags & RestartFlags_::kReloadComponent != 0 {
            events.push(Vst3HostEvent::ReloadComponent);
        }
        events
    }

    /// Take the plugin's "the document changed" flag from `setDirty`.
    pub fn take_document_dirty(&self) -> bool {
        self._host
            .as_ref()
            .is_some_and(|host| host.edit_sink().take_dirty())
    }

    /// Take the editor a plugin asked the host to open.
    pub fn take_editor_request(&self) -> Option<String> {
        self._host
            .as_ref()
            .and_then(|host| host.edit_sink().take_editor_request())
    }

    /// The controller's `IKeyswitchController`, or the component's when the
    /// plugin implements it there.
    fn keyswitch_controller(&self) -> Option<Vst3ForeignRef<IKeyswitchController>> {
        if self.failed() {
            return None;
        }
        self.controller
            .as_ref()
            .and_then(|controller| controller.cast::<IKeyswitchController>())
            .or_else(|| self.component.cast::<IKeyswitchController>())
    }

    /// Keyswitches the plugin exposes on the default bus and channel.
    pub fn keyswitch_count(&self) -> usize {
        self.keyswitch_count_for(0, 0)
    }

    /// Keyswitches the plugin exposes for one bus and MIDI channel.
    pub fn keyswitch_count_for(&self, bus: i32, channel: i16) -> usize {
        let Some(keyswitch) = self.keyswitch_controller() else {
            return 0;
        };
        // SAFETY: The queried interface retains its component/controller parent during this scalar call; legacy callers own UI affinity.
        crate::ffi_guard::guard(|| unsafe { keyswitch.getKeyswitchCount(bus, channel) })
            .ok()
            .map(|count| count.max(0) as usize)
            .unwrap_or(0)
    }

    /// Every keyswitch on the default bus and channel.
    pub fn keyswitches(&self) -> Vec<Vst3Keyswitch> {
        self.keyswitches_for(0, 0)
    }

    /// Every keyswitch the plugin exposes for one bus and MIDI channel.
    pub fn keyswitches_for(&self, bus: i32, channel: i16) -> Vec<Vst3Keyswitch> {
        let count = self.keyswitch_count_for(bus, channel);
        (0..count as i32)
            .filter_map(|index| self.keyswitch_info(bus, channel, index))
            .collect()
    }

    /// The keyswitch a MIDI key triggers, when the plugin exposes one.
    ///
    /// A plugin may remap a switch onto a key outside its declared range, so the
    /// remapped key is checked first.
    pub fn keyswitch_by_key(&self, key: u8) -> Option<Vst3Keyswitch> {
        let key = i32::from(key);
        self.keyswitches()
            .into_iter()
            .find(|switch| match switch.remapped_key {
                Some(remapped) => remapped == key,
                None => (switch.min_key..=switch.max_key).contains(&key),
            })
    }

    fn keyswitch_info(&self, bus: i32, channel: i16, index: i32) -> Option<Vst3Keyswitch> {
        let keyswitch = self.keyswitch_controller()?;
        // SAFETY: KeyswitchInfo contains SDK scalar fields and inline UTF-16 arrays, all permitting zero initialization.
        let mut info: KeyswitchInfo = unsafe { std::mem::zeroed() };
        // SAFETY: The retained keyswitch interface borrows an exclusive inline output; bus/channel/index are SDK addresses, not Rust pointers.
        let result = crate::ffi_guard::guard(|| unsafe {
            keyswitch.getKeyswitchInfo(bus, channel, index, &mut info)
        });
        if result != Ok(kResultOk) {
            return None;
        }
        Some(Vst3Keyswitch {
            index,
            name: char16_to_string(&info.title),
            short_name: char16_to_string(&info.shortTitle),
            min_key: info.keyswitchMin,
            max_key: info.keyswitchMax,
            remapped_key: (info.keyRemapped >= 0).then_some(info.keyRemapped),
            unit_id: info.unitId,
            flags: info.flags,
        })
    }

    /// The controller's `INoteExpressionController`, or the component's.
    fn note_expression_controller(&self) -> Option<Vst3ForeignRef<INoteExpressionController>> {
        if self.failed() {
            return None;
        }
        self.controller
            .as_ref()
            .and_then(|controller| controller.cast::<INoteExpressionController>())
            .or_else(|| self.component.cast::<INoteExpressionController>())
    }

    /// Note expressions the plugin exposes on the default bus and channel.
    pub fn note_expression_count(&self) -> usize {
        self.note_expression_count_for(0, 0)
    }

    /// Note expressions the plugin exposes for one bus and MIDI channel.
    pub fn note_expression_count_for(&self, bus: i32, channel: i16) -> usize {
        let Some(expressions) = self.note_expression_controller() else {
            return 0;
        };
        // SAFETY: The queried interface retains its native parent during this scalar call; legacy callers own UI affinity.
        crate::ffi_guard::guard(|| unsafe { expressions.getNoteExpressionCount(bus, channel) })
            .ok()
            .map(|count| count.max(0) as usize)
            .unwrap_or(0)
    }

    /// Every note expression on the default bus and channel.
    pub fn note_expressions(&self) -> Vec<Vst3NoteExpression> {
        self.note_expressions_for(0, 0)
    }

    /// Every note expression the plugin exposes for one bus and MIDI channel.
    pub fn note_expressions_for(&self, bus: i32, channel: i16) -> Vec<Vst3NoteExpression> {
        let count = self.note_expression_count_for(bus, channel);
        (0..count as i32)
            .filter_map(|index| self.note_expression_info(bus, channel, index))
            .collect()
    }

    /// The note expression with a given `NoteExpressionTypeID`.
    pub fn note_expression_by_id(
        &self,
        type_id: NoteExpressionTypeID,
    ) -> Option<Vst3NoteExpression> {
        self.note_expressions()
            .into_iter()
            .find(|expression| expression.type_id == type_id)
    }

    fn note_expression_info(
        &self,
        bus: i32,
        channel: i16,
        index: i32,
    ) -> Option<Vst3NoteExpression> {
        let expressions = self.note_expression_controller()?;
        // SAFETY: NoteExpressionTypeInfo contains SDK scalars and inline arrays with valid all-zero representations.
        let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
        // SAFETY: The retained expression interface borrows an exclusive stack descriptor for this synchronous metadata query.
        let result = crate::ffi_guard::guard(|| unsafe {
            expressions.getNoteExpressionInfo(bus, channel, index, &mut info)
        });
        if result != Ok(kResultOk) {
            return None;
        }
        // VST3 enum constants are `c_int` on Windows and `c_uint` elsewhere, while the
        // SDK's flag and event-type fields are `int32`: the casts are no-ops on
        // Windows only.
        #[allow(clippy::unnecessary_cast)]
        const NOTE_EXPRESSION_BIPOLAR: i32 = NoteExpressionTypeFlags_::kIsBipolar as i32;
        #[allow(clippy::unnecessary_cast)]
        const NOTE_EXPRESSION_ONE_SHOT: i32 = NoteExpressionTypeFlags_::kIsOneShot as i32;
        Some(Vst3NoteExpression {
            type_id: info.typeId,
            name: char16_to_string(&info.title),
            short_name: char16_to_string(&info.shortTitle),
            unit: char16_to_string(&info.units),
            unit_id: info.unitId,
            default_value: info.valueDesc.defaultValue,
            minimum: info.valueDesc.minimum,
            maximum: info.valueDesc.maximum,
            step_count: info.valueDesc.stepCount,
            associated_parameter: info.associatedParameterId,
            bipolar: info.flags & NOTE_EXPRESSION_BIPOLAR != 0,
            one_shot: info.flags & NOTE_EXPRESSION_ONE_SHOT != 0,
        })
    }

    /// Create and inspect the native editor without attaching it to a window.
    /// The caller must invoke this on the UI thread, as required by VST3.
    #[cfg(target_os = "macos")]
    pub fn editor_info(&self) -> Result<Option<Vst3EditorInfo>, String> {
        self.healthy().map_err(|error| error.to_string())?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::CreateView) {
            return Err(self.native_fault(Operation::Legacy, fault).to_string());
        }
        let Some(controller) = &*self.controller else {
            return Ok(None);
        };
        // SAFETY: The retained controller receives a static NUL-terminated name; this legacy entry requires caller-owned UI affinity.
        let raw_view = guard_editor_call(&self.editor_handles, || unsafe {
            controller.createView(c"editor".as_ptr())
        })?;
        // SAFETY: A successful createView transfers one owned reference or null, which from_raw adopts exactly once.
        let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
            return Ok(None);
        };
        let view = Vst3ForeignRef::adopt(view, Arc::clone(&self.failure)).with_parent(controller);
        // SAFETY: The adopted view retains its native parent and the SDK platform string is static; caller owns UI affinity.
        let platform_supported = guard_editor_call(&self.editor_handles, || unsafe {
            view.isPlatformTypeSupported(kPlatformTypeNSView)
        })? == kResultOk;
        let frame = self
            .plug_frame
            .as_ref()
            .ok_or("VST3 editor has no host plug frame")?;
        // SAFETY: The view and plug frame remain retained by this instance; this legacy entry requires caller-owned UI affinity.
        if guard_editor_call(&self.editor_handles, || unsafe {
            view.setFrame(frame.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 editor rejected the host plug frame".to_string());
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: The retained view borrows the writable stack rect for the duration of getSize; caller owns UI affinity.
        if guard_editor_call(&self.editor_handles, || unsafe { view.getSize(&mut rect) })?
            != kResultOk
        {
            return Err("VST3 editor did not report a valid initial size".to_string());
        }
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return Err(format!(
                "VST3 editor reported invalid size {width}x{height}"
            ));
        }
        Ok(Some(Vst3EditorInfo {
            width,
            height,
            // SAFETY: The adopted view remains retained through this scalar query; caller owns UI affinity.
            resizable: guard_editor_call(&self.editor_handles, || unsafe { view.canResize() })?
                == kResultOk,
            platform_supported,
        }))
    }

    #[cfg(target_os = "macos")]
    /// The plugin's GUI handle when it exposes an editor.
    pub fn gui_handle(&self) -> Option<Vst3GuiHandle> {
        self.try_gui_handle().ok().flatten()
    }

    #[cfg(target_os = "macos")]
    /// Try to acquire the GUI handle, reporting why it is unavailable.
    pub fn try_gui_handle(&self) -> Result<Option<Vst3GuiHandle>, String> {
        self.healthy().map_err(|error| error.to_string())?;
        let Some(controller) = self.controller.as_ref() else {
            return Ok(None);
        };
        let Some(frame) = self.plug_frame.as_ref() else {
            return Ok(None);
        };
        Ok(Some(Vst3GuiHandle {
            controller: Arc::new(EditorControllerLease::new(
                controller.try_clone().map_err(|error| error.to_string())?,
            )),
            frame: Arc::new(HostFrame {
                frame: ManuallyDrop::new(frame.clone()),
                failure: Arc::clone(&self.failure),
            }),
            handles: Arc::clone(&self.editor_handles),
            blocked: unhostable_editor_reason(&self.class_info),
        }))
    }

    /// The edit controller a platform editor host creates its view from.
    ///
    /// The caller owns the window and the plug frame; it keeps this instance
    /// alive for as long as the view exists.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn editor_controller(&self) -> Option<Vst3ForeignRef<IEditController>> {
        if self.failed() {
            return None;
        }
        self.controller.as_ref()?.try_clone().ok()
    }

    /// Class metadata of the loaded component, including vendor and version.
    pub fn class_info(&self) -> &Vst3ClassInfo {
        &self.class_info
    }

    /// Stereo output pairs negotiated from the component's output buses.
    pub fn output_count(&self) -> usize {
        self.output_layout.pair_count()
    }

    /// Declared audio output bus count (one entry per bus, in VST3 order).
    pub fn output_bus_count(&self) -> usize {
        self.output_layout.bus_count()
    }

    /// Channel counts per declared output bus.
    pub fn output_bus_channels(&self) -> &[i32] {
        &self.output_layout.bus_channel_counts
    }

    /// Declared audio output ports, in VST3 bus order.
    pub fn output_ports(&self) -> Vec<AudioPort> {
        ports::ports_from_channels(&self.output_layout.bus_channel_counts)
    }

    /// Declared audio input ports, in VST3 bus order; empty for instruments.
    pub fn input_ports(&self) -> Vec<AudioPort> {
        if self.input_bus_channels.is_empty() {
            Vec::new()
        } else {
            ports::ports_from_channels(&self.input_bus_channels)
        }
    }

    #[expect(
        clippy::result_large_err,
        reason = "bounded native causes remain inline for allocation-free failure publication"
    )]
    fn query_input_bus_channels(
        component: &Vst3ForeignRef<IComponent>,
    ) -> Result<Vec<i32>, Vst3ProcessError> {
        Self::query_bus_channels(component, BusDirections_::kInput as int32)
    }

    #[expect(
        clippy::result_large_err,
        reason = "bounded native causes remain inline for allocation-free failure publication"
    )]
    fn query_bus_channels(
        component: &Vst3ForeignRef<IComponent>,
        direction: int32,
    ) -> Result<Vec<i32>, Vst3ProcessError> {
        // SAFETY: retained component and primitive query arguments are valid.
        let count = unsafe {
            vst3_guard::bus_count(component.as_ptr(), MediaTypes_::kAudio as int32, direction)
        }
        .map_err(|fault| Vst3ProcessError::native(Operation::BusCount, fault))?
        .max(0);
        let mut channels = Vec::new();
        for index in 0..count {
            // SAFETY: BusInfo consists only of primitive fields and fixed character arrays.
            let mut info = unsafe { std::mem::zeroed::<BusInfo>() };
            // SAFETY: component and writable stack BusInfo outlive the synchronous native call.
            if unsafe {
                vst3_guard::bus_info(
                    component.as_ptr(),
                    MediaTypes_::kAudio as int32,
                    direction,
                    index,
                    &mut info,
                )
            }
            .map_err(|fault| Vst3ProcessError::native(Operation::BusInfo, fault))?
                == kResultOk
            {
                channels.push(if direction == BusDirections_::kInput as int32 {
                    info.channelCount.max(0)
                } else {
                    info.channelCount
                });
            }
        }
        Ok(channels)
    }

    /// Input bus shape the effect path renders into.
    ///
    /// A component that declares no audio input still receives one stereo bus
    /// on this path; that keeps a processed-through effect instance working
    /// when its host routes the dry signal in.
    fn input_bus_shape(input_bus_channels: &[i32]) -> &[i32] {
        if input_bus_channels.is_empty() {
            &[2]
        } else {
            input_bus_channels
        }
    }

    /// The arrangement each declared audio bus declares, for the one
    /// `setBusArrangements` call that agrees the host's bus layout.
    #[expect(
        clippy::result_large_err,
        reason = "bounded native causes remain inline for allocation-free failure publication"
    )]
    fn declared_bus_arrangements(
        component: &Vst3ForeignRef<IComponent>,
        processor: &Vst3ForeignRef<IAudioProcessor>,
    ) -> Result<(Vec<SpeakerArrangement>, Vec<SpeakerArrangement>), Vst3ProcessError> {
        let arrangements = |direction: int32| -> Result<Vec<SpeakerArrangement>, Vst3ProcessError> {
            // SAFETY: both retained interfaces are live through the metadata query.
            let count = unsafe {
                vst3_guard::bus_count(component.as_ptr(), MediaTypes_::kAudio as int32, direction)
            }
            .map_err(|fault| Vst3ProcessError::native(Operation::BusCount, fault))?
            .max(0);
            let mut arrangements = Vec::with_capacity(count as usize);
            for index in 0..count {
                let mut arrangement = SpeakerArr::kStereo;
                // SAFETY: processor and writable stack arrangement outlive the native invocation.
                if unsafe {
                    vst3_guard::bus_arrangement(
                        processor.as_ptr(),
                        direction,
                        index,
                        &mut arrangement,
                    )
                }
                .map_err(|fault| Vst3ProcessError::native(Operation::GetBusArrangement, fault))?
                    != kResultOk
                {
                    arrangement = SpeakerArr::kStereo;
                }
                arrangements.push(arrangement);
            }
            Ok(arrangements)
        };
        Ok((
            arrangements(BusDirections_::kInput as int32)?,
            arrangements(BusDirections_::kOutput as int32)?,
        ))
    }

    #[expect(
        clippy::result_large_err,
        reason = "bounded native causes remain inline for allocation-free failure publication"
    )]
    fn query_output_bus_channels(
        component: &Vst3ForeignRef<IComponent>,
    ) -> Result<Vec<i32>, Vst3ProcessError> {
        let channels = Self::query_bus_channels(component, BusDirections_::kOutput as int32)?;
        Ok(if channels.is_empty() {
            vec![2]
        } else {
            channels
        })
    }

    /// Re-negotiate buses after a state load. Runs once per state apply, never
    /// inside the audio callback.
    fn reload_output_layout(&mut self) {
        if self.failed() {
            return;
        }
        let Some(channels) = self.native_result(Self::query_output_bus_channels(&self.component))
        else {
            return;
        };
        let Some(input_channels) =
            self.native_result(Self::query_input_bus_channels(&self.component))
        else {
            return;
        };
        if input_channels != self.input_bus_channels {
            self.input_layout.ensure(
                Self::input_bus_shape(&input_channels),
                self.max_frames.max(1) as usize,
            );
            self.input_bus_channels = input_channels;
        }
        if channels == self.output_layout.bus_channel_counts {
            return;
        }
        for index in 0..channels.len() {
            // SAFETY: retained component and known declared output bus index are live.
            if self
                // SAFETY: The component is retained and index identifies an output bus from the preceding layout query.
                .native_value(Operation::ActivateBus, unsafe {
                    vst3_guard::activate_bus(
                        self.component.as_ptr(),
                        MediaTypes_::kAudio as int32,
                        BusDirections_::kOutput as int32,
                        index as int32,
                        true,
                    )
                })
                .is_none()
            {
                return;
            }
        }
        let frames = self
            .output_layout
            .frames
            .max(self.max_frames.max(1) as usize);
        *self.output_layout = Vst3OutputLayout::new(channels, frames);
    }

    /// Sidechain inputs declared after the main one.
    pub fn sidechain_input_count(&self) -> usize {
        self.input_bus_channels.iter().skip(1).count()
    }

    /// Absolute input bus index of the `index`-th sidechain input.
    pub fn sidechain_input_bus(&self, index: usize) -> Option<usize> {
        self.input_bus_channels
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, channels)| **channels > 0)
            .nth(index)
            .map(|(bus, _)| bus)
    }

    /// Whether a non-main input bus declares channels.
    pub fn has_sidechain_input(&self) -> bool {
        self.input_bus_channels
            .iter()
            .skip(1)
            .any(|channels| *channels > 0)
    }

    /// Route `process_effect_with_sidechain` audio to a sidechain input index.
    pub fn set_sidechain_input_index(&mut self, index: usize) {
        self.sidechain_input_index = index;
    }

    /// Scan a VST3 bundle and return class info with subcategories.
    pub fn scan_bundle(path: &Path) -> Vec<Vst3ClassInfo> {
        let failure = Arc::new(Vst3Failure::default());
        let (factory, _handle) = match Self::load_factory(path, &failure) {
            Ok(loaded) => loaded,
            Err(error) => {
                oc_diag::diag_warn!("plugin: could not scan '{}': {error}", path.display());
                return Vec::new();
            }
        };
        let scanned = factory
            .try_query::<IPluginFactory2>()
            .map_err(|error| error.to_string())
            .and_then(|factory2| factory::classes(&factory, factory2.as_ref()));
        match scanned {
            Ok(classes) => classes,
            Err(error) => {
                oc_diag::diag_warn!("plugin: could not scan '{}': {error}", path.display());
                Vec::new()
            }
        }
    }

    /// Load and instantiate the first audio processor from a VST3 bundle.
    ///
    /// This is the raw constructor. It does not move the call to the AppKit
    /// main thread, because offline rendering and scanning load plugins on a
    /// worker. A host that owns an editor calls [`Self::load_on_main_thread`].
    pub fn load(vst3_path: &Path, sample_rate: u32, max_frames: u32) -> Result<Self, String> {
        let _timer = oc_diag::perf_counter("plugin.vst3.load.ms").map(oc_diag::Timer::start);
        oc_diag::diag_info!(
            "plugin: vst3 load path={} sample_rate={} max_frames={}",
            vst3_path.display(),
            sample_rate,
            max_frames
        );
        let loaded = Self::load_with_context(
            vst3_path,
            sample_rate,
            max_frames,
            ptr::null_mut(),
            ptr::null_mut(),
            true,
            true,
        );
        match &loaded {
            Ok(instance) => oc_diag::diag_info!(
                "plugin: vst3 loaded name={:?} vendor={:?} params={}",
                instance.class_info.name,
                instance.class_info.vendor,
                instance.get_params().len()
            ),
            Err(error) => oc_diag::diag_warn!("plugin: vst3 load failed: {error}"),
        }
        loaded
    }

    /// Instantiate a bundle for a host that owns an editor.
    ///
    /// This is the one lifecycle entry point for hosted plugins. It performs
    /// instantiation, `setComponentHandler`, the initial `setComponentState`,
    /// bus and arrangement setup, and `activate` — all of it on the AppKit main
    /// thread. A caller on a worker dispatches and waits, so a call site cannot
    /// get the thread contract wrong by forgetting to hop.
    pub fn load_on_main_thread(
        vst3_path: &Path,
        sample_rate: u32,
        max_frames: u32,
    ) -> Result<Self, String> {
        let _timer = oc_diag::perf_counter("plugin.vst3.load.ms").map(oc_diag::Timer::start);
        oc_diag::diag_info!(
            "plugin: vst3 load path={} sample_rate={} max_frames={}",
            vst3_path.display(),
            sample_rate,
            max_frames
        );
        let loaded = run_on_main_thread(|| {
            debug_assert!(
                is_main_thread(),
                "VST3 plugin instantiation must run on the AppKit main thread"
            );
            Self::load_with_context(
                vst3_path,
                sample_rate,
                max_frames,
                ptr::null_mut(),
                ptr::null_mut(),
                true,
                true,
            )
        })?;
        match &loaded {
            Ok(instance) => oc_diag::diag_info!(
                "plugin: vst3 loaded name={:?} vendor={:?} params={}",
                instance.class_info.name,
                instance.class_info.vendor,
                instance.get_params().len()
            ),
            Err(error) => oc_diag::diag_warn!("plugin: vst3 load failed: {error}"),
        }
        loaded
    }

    /// Load with a host application context, which lets the plugin query
    /// host services such as `IComponentHandler`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn load_with_host_context(
        vst3_path: &Path,
        sample_rate: u32,
        max_frames: u32,
        context: *mut vst3::Steinberg::FUnknown,
    ) -> Result<Self, String> {
        Self::load_with_context(
            vst3_path,
            sample_rate,
            max_frames,
            context,
            ptr::null_mut(),
            true,
            true,
        )
    }

    /// Load with explicit initial-state and handler choices.
    ///
    /// The out-of-process editor host uses this to supply its own host services
    /// and to defer activation until it has applied the parent's state.
    pub fn load_with_options(
        vst3_path: &Path,
        sample_rate: u32,
        max_frames: u32,
        host_context: *mut vst3::Steinberg::FUnknown,
        component_handler: *mut vst3::Steinberg::Vst::IComponentHandler,
        transfer_initial_state: bool,
        activate_on_load: bool,
    ) -> Result<Self, String> {
        Self::load_with_context(
            vst3_path,
            sample_rate,
            max_frames,
            host_context,
            component_handler,
            transfer_initial_state,
            activate_on_load,
        )
    }

    fn load_with_context(
        vst3_path: &Path,
        sample_rate: u32,
        max_frames: u32,
        host_context: *mut vst3::Steinberg::FUnknown,
        component_handler: *mut vst3::Steinberg::Vst::IComponentHandler,
        transfer_initial_state: bool,
        activate_on_load: bool,
    ) -> Result<Self, String> {
        let failure = Arc::new(Vst3Failure::default());
        let result = Self::load_with_context_inner(
            vst3_path,
            sample_rate,
            max_frames,
            Vst3LoadContext {
                host_context,
                component_handler,
                transfer_initial_state,
                activate_on_load,
                failure: Arc::clone(&failure),
            },
        );
        Self::finish_acquisition_result(result, &failure)
    }

    fn finish_acquisition_result(
        result: Result<Self, String>,
        failure: &Arc<Vst3Failure>,
    ) -> Result<Self, String> {
        match (result, failure.message()) {
            (Ok(instance), None) => Ok(instance),
            (Ok(_instance), Some(fault)) => Err(fault),
            (Err(primary), Some(fault)) if !primary.contains(&fault) => {
                Err(format!("{primary}; cleanup failed: {fault}"))
            }
            (Err(primary), _) => Err(primary),
        }
    }

    fn load_with_context_inner(
        vst3_path: &Path,
        sample_rate: u32,
        max_frames: u32,
        context: Vst3LoadContext,
    ) -> Result<Self, String> {
        let Vst3LoadContext {
            host_context,
            component_handler,
            transfer_initial_state,
            activate_on_load,
            failure,
        } = context;
        log::debug!("[VST3] loading: {}", vst3_path.display());
        // A plugin without a host application cannot create `IMessage` objects;
        // editors that build their view from messages then dereference null.
        let mut owned_host = None;
        let (host_context, component_handler) = if host_context.is_null() {
            let host = Vst3Host::new();
            let application = host
                .to_com_ptr::<IHostApplication>()
                .ok_or("VST3: host application interface unavailable")?;
            let host_handler = host
                .to_com_ptr::<IComponentHandler>()
                .ok_or("VST3: host component handler unavailable")?;
            let handler = if component_handler.is_null() {
                host_handler.as_ptr()
            } else {
                component_handler
            };
            let pointers = (
                application.as_ptr() as *mut vst3::Steinberg::FUnknown,
                handler,
            );
            owned_host = Some(host);
            pointers
        } else {
            (host_context, component_handler)
        };

        let backing = Arc::new(HostBacking {
            host: ManuallyDrop::new(owned_host.clone()),
            failure: Arc::clone(&failure),
        });
        let (factory, lib_handle) = Self::load_factory(vst3_path, &backing.failure)
            .map_err(|error| format!("Failed to load VST3 {}: {error}", vst3_path.display()))?;
        let factory = factory.with_backing(Arc::clone(&backing));
        let factory2 = factory
            .try_query::<IPluginFactory2>()
            .map_err(|error| error.to_string())?;
        let class_info = factory::audio_class(factory::classes(&factory, factory2.as_ref())?)?;
        let component = factory::create::<IComponent>(&factory, &class_info.cid)?;

        // The guard is declared after component; explicit ordinary-error cleanup
        // runs before any later local owner is released.
        let mut initialization = InitializationGuard::new(component.as_ptr(), Arc::clone(&failure));
        Self::initialize_component_ref(&component, host_context, &mut initialization)?;
        let processor = component
            .try_query::<IAudioProcessor>()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| initialization.error("VST3: no IAudioProcessor".into()))?;
        let direct_controller = component
            .try_query::<IEditController>()
            .map_err(|error| error.to_string())?;
        let controller_is_separate = direct_controller.is_none();
        let controller = if let Some(controller) = direct_controller {
            Some(controller)
        } else if let Some(controller_cid) = factory::controller_class_id(&component)? {
            let controller = factory::create::<IEditController>(&factory, &controller_cid)
                .map_err(|error| initialization.error(error))?;
            Self::initialize_controller_ref(&controller, host_context, &mut initialization)?;
            Some(controller)
        } else {
            None
        };
        let component_initialized = initialization.component_initialized;
        let controller_initialized = initialization.controller_initialized;
        initialization.armed = false;
        Self::finish_load(
            Vst3LoadingParts {
                component,
                processor,
                controller,
                controller_is_separate,
                owned_host,
                component_handler,
                factory: Some(factory),
                factory2,
                lib_handle,
                class_info,
                failure,
                component_initialized,
                controller_initialized,
            },
            sample_rate,
            max_frames,
            transfer_initial_state,
            activate_on_load,
        )
    }

    fn initialize_component_ref(
        component: &Vst3ForeignRef<IComponent>,
        host: *mut vst3::Steinberg::FUnknown,
        guard: &mut InitializationGuard,
    ) -> Result<(), String> {
        // SAFETY: the acquired component and caller's retained host context are live.
        let result = unsafe { vst3_guard::initialize(component.as_ptr().cast(), host.cast()) }
            .map_err(|fault| {
                component
                    .fault(Operation::ComponentInitialize, fault)
                    .to_string()
            })?;
        if result != kResultOk {
            return Err(guard.error(format!("VST3 component initialize refused (0x{result:x})")));
        }
        guard.component_initialized = true;
        Ok(())
    }
    fn initialize_controller_ref(
        controller: &Vst3ForeignRef<IEditController>,
        host: *mut vst3::Steinberg::FUnknown,
        guard: &mut InitializationGuard,
    ) -> Result<(), String> {
        // SAFETY: acquired controller and host callback backing remain live.
        let result = unsafe { vst3_guard::initialize(controller.as_ptr().cast(), host.cast()) }
            .map_err(|fault| {
                controller
                    .fault(Operation::ControllerInitialize, fault)
                    .to_string()
            })?;
        if result != kResultOk {
            return Err(guard.error(format!("VST3 controller initialize refused (0x{result:x})")));
        }
        guard.controller = controller.as_ptr();
        guard.controller_initialized = true;
        Ok(())
    }

    fn finish_load(
        parts: Vst3LoadingParts,
        sample_rate: u32,
        max_frames: u32,
        transfer_initial_state: bool,
        activate_on_load: bool,
    ) -> Result<Self, String> {
        let failure = Arc::clone(&parts.failure);
        let result = Self::finish_load_inner(
            parts,
            sample_rate,
            max_frames,
            transfer_initial_state,
            activate_on_load,
        );
        Self::finish_acquisition_result(result, &failure)
    }

    fn finish_load_inner(
        parts: Vst3LoadingParts,
        sample_rate: u32,
        max_frames: u32,
        transfer_initial_state: bool,
        activate_on_load: bool,
    ) -> Result<Self, String> {
        let Vst3LoadingParts {
            component,
            processor,
            controller,
            controller_is_separate,
            owned_host,
            component_handler,
            factory,
            factory2,
            lib_handle,
            class_info,
            failure,
            component_initialized,
            controller_initialized,
        } = parts;
        let mut initialization = InitializationGuard::new(component.as_ptr(), Arc::clone(&failure));
        initialization.component_initialized = component_initialized;
        initialization.controller = controller
            .as_ref()
            .map_or(ptr::null_mut(), |value| value.as_ptr());
        initialization.controller_initialized = controller_initialized;
        if !component_initialized {
            Self::initialize_component_ref(&component, ptr::null_mut(), &mut initialization)?;
        }
        if controller_is_separate
            && !controller_initialized
            && let Some(controller) = controller.as_ref()
        {
            Self::initialize_controller_ref(controller, ptr::null_mut(), &mut initialization)?;
        }
        let (component_connection, controller_connection) = if controller_is_separate {
            let component_connection = component
                .try_query::<IConnectionPoint>()
                .map_err(|error| error.to_string())?;
            let controller_connection = match controller.as_ref() {
                Some(value) => value
                    .try_query::<IConnectionPoint>()
                    .map_err(|error| error.to_string())?,
                None => None,
            };
            match (component_connection, controller_connection) {
                (Some(component_connection), Some(controller_connection)) => {
                    initialization.component_connection = component_connection.as_ptr();
                    initialization.controller_connection = controller_connection.as_ptr();
                    // SAFETY: both acquired connection refs remain owned until cleanup or transfer.
                    let code = unsafe {
                        vst3_guard::connect(
                            component_connection.as_ptr().cast(),
                            controller_connection.as_ptr().cast(),
                        )
                    }
                    .map_err(|fault| {
                        let error = Vst3ProcessError::native(Operation::Connect, fault);
                        failure.slot.record(error);
                        error.to_string()
                    })?;
                    if code != kResultOk {
                        return Err(initialization
                            .error(format!("VST3 component connect refused (0x{code:x})")));
                    }
                    initialization.component_connected = true;
                    // SAFETY: reverse connection uses the same retained pair of interfaces.
                    let code = unsafe {
                        vst3_guard::connect(
                            controller_connection.as_ptr().cast(),
                            component_connection.as_ptr().cast(),
                        )
                    }
                    .map_err(|fault| {
                        let error = Vst3ProcessError::native(Operation::Connect, fault);
                        failure.slot.record(error);
                        error.to_string()
                    })?;
                    if code != kResultOk {
                        return Err(initialization
                            .error(format!("VST3 controller connect refused (0x{code:x})")));
                    }
                    initialization.controller_connected = true;
                    (Some(component_connection), Some(controller_connection))
                }
                _ => (None, None),
            }
        } else {
            (None, None)
        };
        if let Some(error) = failure.message() {
            return Err(error);
        }
        if !component_handler.is_null()
            && let Some(controller) = &controller
        {
            // SAFETY: The loader retains the controller; the external nonnull handler must remain live, a precondition not enforced by load_with_options.
            let _ = crate::ffi_guard::guard(|| unsafe {
                controller.setComponentHandler(component_handler)
            });
        }
        if let Some(host) = &owned_host
            && let Some(controller) = &controller
        {
            // SAFETY: the instance owns both and drops the controller first.
            unsafe { host.edit_sink().bind_controller(controller.as_ptr()) };
        }
        macro_rules! native_load {
            ($operation:expr, $call:expr) => {
                native_load!(@typed, $call.map_err(|fault|Vst3ProcessError::native($operation,fault)))
            };
            (@typed, $call:expr) => {
                match $call {
                    Ok(result) => result,
                    Err(error) => {
                        failure.slot.record(error);
                        let error = error.to_string();
                        // A native exception invalidates all plugin-owned references. Keep the
                        // host callbacks and module resident too; no Release/terminate is safe.
                        std::mem::forget(component);
                        std::mem::forget(processor);
                        std::mem::forget(controller);
                        std::mem::forget(component_connection);
                        std::mem::forget(controller_connection);
                        std::mem::forget(owned_host);
                        std::mem::forget(factory2);
                        std::mem::forget(factory);
                        return Err(error);
                    }
                }
            };
        }
        if transfer_initial_state && let Some(edit_controller) = &controller {
            let wrapper = ComWrapper::new(Vst3StateStream::new(Vec::new()));
            if let Some(com_stream) = wrapper.to_com_ptr::<IBStream>() {
                // SAFETY: live component and host stream have their original COM ownership.
                let state = native_load!(Operation::ComponentGetState, unsafe {
                    vst3_guard::component_get_state(component.as_ptr(), com_stream.as_ptr())
                });
                if state == kResultOk {
                    // SAFETY: this is our healthy host stream, not plugin code.
                    let _ = unsafe {
                        com_stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut())
                    };
                    // SAFETY: the borrowed controller and host stream outlive the call.
                    let _ = native_load!(Operation::ControllerComponentState, unsafe {
                        vst3_guard::controller_component_state(
                            edit_controller.as_ptr(),
                            com_stream.as_ptr(),
                        )
                    });
                }
            }
        }

        // Setup processing
        let mut setup = ProcessSetup {
            processMode: 0,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            maxSamplesPerBlock: max_frames as int32,
            sampleRate: sample_rate as f64,
        };
        // SAFETY: retained processor and stack setup are live throughout this native call.
        let setup_result = native_load!(Operation::SetupProcessing, unsafe {
            vst3_guard::setup(processor.as_ptr(), &mut setup)
        });
        if setup_result != kResultOk {
            return Err(
                initialization.error(format!("VST3 setupProcessing refused (0x{setup_result:x})"))
            );
        }

        // Agree on the bus arrangement before any bus is activated. The host
        // renders stereo buses, so it proposes the arrangement the component
        // declares and keeps the plugin's default when it refuses.
        let (mut input_arrangements, mut output_arrangements) = native_load!(
            @typed,
            Self::declared_bus_arrangements(&component, &processor)
        );
        // SAFETY: each arrangement pointer addresses its live Vec and exact count.
        let arrangement = native_load!(Operation::BusArrangements, unsafe {
            vst3_guard::bus_arrangements(
                processor.as_ptr(),
                if input_arrangements.is_empty() {
                    ptr::null_mut()
                } else {
                    input_arrangements.as_mut_ptr()
                },
                input_arrangements.len() as int32,
                if output_arrangements.is_empty() {
                    ptr::null_mut()
                } else {
                    output_arrangements.as_mut_ptr()
                },
                output_arrangements.len() as int32,
            )
        });
        if arrangement != kResultOk {
            oc_diag::diag_warn!("plugin: setBusArrangements refused (0x{arrangement:x})");
        }
        for direction in [
            BusDirections_::kOutput as int32,
            BusDirections_::kInput as int32,
        ] {
            // SAFETY: retained component and primitive direction are valid for metadata access.
            let count = native_load!(Operation::BusCount, unsafe {
                vst3_guard::bus_count(component.as_ptr(), MediaTypes_::kAudio as int32, direction)
            });
            for index in 0..count.max(0) {
                // SAFETY: index belongs to the just-declared bus range; component is retained.
                let _ = native_load!(Operation::ActivateBus, unsafe {
                    vst3_guard::activate_bus(
                        component.as_ptr(),
                        MediaTypes_::kAudio as int32,
                        direction,
                        index,
                        true,
                    )
                });
            }
        }
        // SAFETY: same retained component; event bus activation refusal stays optional.
        let _ = native_load!(Operation::ActivateBus, unsafe {
            vst3_guard::activate_bus(
                component.as_ptr(),
                MediaTypes_::kEvent as int32,
                BusDirections_::kInput as int32,
                0,
                true,
            )
        });
        let output_layout = Vst3OutputLayout::new(
            native_load!(
                @typed,
                Self::query_output_bus_channels(&component)
            ),
            max_frames as usize,
        );
        let input_bus_channels = native_load!(
            @typed,
            Self::query_input_bus_channels(&component)
        );
        let input_layout = Vst3InputLayout::new(
            Self::input_bus_shape(&input_bus_channels).to_vec(),
            max_frames as usize,
        );

        if activate_on_load {
            // SAFETY: component is healthy and retained until construction finishes.
            let active_result = native_load!(Operation::SetActive, unsafe {
                vst3_guard::active(component.as_ptr(), true)
            });
            if active_result != kResultOk {
                log::debug!("[VST3] setActive failed (continuing)");
            }
            // SAFETY: setup completed and processor is still owned here.
            let _ = native_load!(Operation::SetProcessing, unsafe {
                vst3_guard::processing(processor.as_ptr(), true)
            });
        }

        #[cfg(target_os = "macos")]
        let editor_handles = Arc::new(Vst3EditorHandles::new(Arc::clone(&failure)));
        #[cfg(target_os = "macos")]
        let plug_frame = controller.as_ref().map(|_| {
            let frame = ComWrapper::new(Vst3PlugFrame::new(Arc::clone(&editor_handles)));
            frame
                .to_com_ptr::<IPlugFrame>()
                .expect("IPlugFrame wrapper exposes its interface")
        });

        log::debug!("[VST3] loaded successfully");
        initialization.armed = false;
        Ok(Self {
            _lib_handle: lib_handle,
            class_info: Box::new(class_info),
            component: ManuallyDrop::new(component),
            processor: ManuallyDrop::new(processor),
            controller: ManuallyDrop::new(controller),
            controller_is_separate,
            component_connection: ManuallyDrop::new(component_connection),
            controller_connection: ManuallyDrop::new(controller_connection),
            sample_rate: sample_rate as f64,
            max_frames: max_frames as int32,
            input_bus_channels,
            sidechain_input_index: 0,
            active: activate_on_load,
            processing: activate_on_load,
            pending_events: Vec::new(),
            note_ids: Vst3NoteIds::default(),
            output_layout: ManuallyDrop::new(output_layout),
            #[cfg(target_os = "macos")]
            editor_handles,
            #[cfg(target_os = "macos")]
            plug_frame: ManuallyDrop::new(plug_frame),
            _host: ManuallyDrop::new(owned_host),
            edit_bus: None,
            edit_receiver: None,
            applied_values: Vec::new(),
            failure: Arc::clone(&failure),
            input_layout: ManuallyDrop::new(input_layout),
            event_scratch: Vec::new(),
            param_scratch: Vec::new(),
            process_queues: ManuallyDrop::new(None),
            param_cache: ParamCache::default(),
            authored_parameter_revision: 0,
            #[cfg(any(test, feature = "test-support"))]
            fault_injection: None,
            #[cfg(any(test, feature = "test-support"))]
            fixture_handler_guard: None,
        })
    }

    /// Build an instance around host-owned interfaces for offline tests.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_test_parts(
        component: ComPtr<IComponent>,
        processor: ComPtr<IAudioProcessor>,
        sample_rate: u32,
        max_frames: u32,
    ) -> Self {
        let failure = Arc::new(Vst3Failure::default());
        let component = Vst3ForeignRef::adopt(component, Arc::clone(&failure));
        let processor = Vst3ForeignRef::adopt(processor, Arc::clone(&failure));
        let output_layout = Vst3OutputLayout::new(
            Self::query_output_bus_channels(&component)
                .expect("healthy test component bus metadata"),
            max_frames as usize,
        );
        let input_bus_channels =
            Self::query_input_bus_channels(&component).expect("healthy test input bus metadata");
        let input_layout = Vst3InputLayout::new(
            Self::input_bus_shape(&input_bus_channels).to_vec(),
            max_frames as usize,
        );
        Self {
            _lib_handle: ptr::null_mut(),
            class_info: Box::new(Vst3ClassInfo {
                name: "test".into(),
                category: "Audio Module Class".into(),
                sub_categories: String::new(),
                vendor: String::new(),
                version: String::new(),
                cid: [0; 16],
            }),
            component: ManuallyDrop::new(component),
            processor: ManuallyDrop::new(processor),
            controller: ManuallyDrop::new(None),
            controller_is_separate: false,
            component_connection: ManuallyDrop::new(None),
            controller_connection: ManuallyDrop::new(None),
            sample_rate: sample_rate as f64,
            max_frames: max_frames as int32,
            input_bus_channels,
            sidechain_input_index: 0,
            active: true,
            processing: true,
            pending_events: Vec::new(),
            note_ids: Vst3NoteIds::default(),
            output_layout: ManuallyDrop::new(output_layout),
            #[cfg(target_os = "macos")]
            editor_handles: Arc::new(Vst3EditorHandles::new(Arc::clone(&failure))),
            #[cfg(target_os = "macos")]
            plug_frame: ManuallyDrop::new(None),
            _host: ManuallyDrop::new(None),
            edit_bus: None,
            edit_receiver: None,
            applied_values: Vec::new(),
            failure: Arc::clone(&failure),
            input_layout: ManuallyDrop::new(input_layout),
            event_scratch: Vec::new(),
            param_scratch: Vec::new(),
            process_queues: ManuallyDrop::new(None),
            param_cache: ParamCache::default(),
            authored_parameter_revision: 0,
            #[cfg(any(test, feature = "test-support"))]
            fault_injection: None,
            #[cfg(any(test, feature = "test-support"))]
            fixture_handler_guard: None,
        }
    }

    /// Same as `from_test_parts`, with a controller so names resolve.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_test_parts_with_controller(
        component: ComPtr<IComponent>,
        processor: ComPtr<IAudioProcessor>,
        controller: ComPtr<IEditController>,
        sample_rate: u32,
        max_frames: u32,
    ) -> Self {
        let mut instance = Self::from_test_parts(component, processor, sample_rate, max_frames);
        instance.controller = ManuallyDrop::new(Some(Vst3ForeignRef::adopt(
            controller,
            Arc::clone(&instance.failure),
        )));
        instance
    }

    /// A test instance with its own host services, so editor-side edits work.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_test_parts_with_host(
        component: ComPtr<IComponent>,
        processor: ComPtr<IAudioProcessor>,
        controller: ComPtr<IEditController>,
        host: ComWrapper<Vst3Host>,
        sample_rate: u32,
        max_frames: u32,
    ) -> Self {
        let mut instance = Self::from_test_parts_with_controller(
            component,
            processor,
            controller,
            sample_rate,
            max_frames,
        );
        if let Some(controller) = instance.controller.as_ref() {
            // SAFETY: the instance owns both and drops the controller first.
            unsafe { host.edit_sink().bind_controller(controller.as_ptr()) };
        }
        let backing = Arc::new(HostBacking {
            host: ManuallyDrop::new(Some(host.clone())),
            failure: Arc::clone(&instance.failure),
        });
        instance.component.set_backing(Arc::clone(&backing));
        instance.processor.set_backing(Arc::clone(&backing));
        if let Some(controller) = instance.controller.as_mut() {
            controller.set_backing(backing);
        }
        instance._host = ManuallyDrop::new(Some(host));
        instance
    }

    /// Enumerate parameters; empty after a fault.
    pub fn get_params(&self) -> Vec<Vst3Param> {
        if self.failed() {
            return Vec::new();
        }
        let mut params = Vec::new();
        let Some(ctrl) = self.controller.as_ref() else {
            return params;
        };
        // SAFETY: self retains the controller during this scalar query; the legacy API leaves UI affinity to its caller.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: ParameterInfo contains only SDK scalars and inline UTF-16 arrays; all-zero initialization is valid.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: ctrl stays retained; i comes from its count query and info is an exclusive output until the call returns.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            params.push(Vst3Param {
                id: info.id,
                name: char16_to_string(&info.title),
                default: info.defaultNormalizedValue,
                step_count: info.stepCount,
                unit: char16_to_string(&info.units),
            });
        }
        params
    }

    /// The edit controller owns the program list in most plugins; a
    /// component-local control surface may expose it directly, so both are
    /// queried. `None` means the plugin has no `IUnitInfo` at all.
    fn unit_info(&self) -> Option<Vst3ForeignRef<IUnitInfo>> {
        if self.failed() {
            return None;
        }
        self.controller
            .as_ref()
            .and_then(|ctrl| ctrl.cast::<IUnitInfo>())
            .or_else(|| self.component.cast::<IUnitInfo>())
    }

    /// Same lookup rule for the program-data side of the interface.
    fn program_list_data(&self) -> Option<Vst3ForeignRef<IProgramListData>> {
        if self.failed() {
            return None;
        }
        self.controller
            .as_ref()
            .and_then(|ctrl| ctrl.cast::<IProgramListData>())
            .or_else(|| self.component.cast::<IProgramListData>())
    }

    /// Programs the plugin reports through `IUnitInfo`, in list order. An empty
    /// vector is the honest answer when the plugin has no list or no
    /// `IUnitInfo`: callers consult [`crate::programs::capability`] to tell an
    /// empty native list from a format that never had one.
    pub fn list_programs(&self) -> Vec<crate::programs::NativeProgram> {
        let Some(info) = self.unit_info() else {
            return Vec::new();
        };
        let mut programs = Vec::new();
        // SAFETY: The queried unit interface retains its native parent; the legacy caller must serialize UI access.
        let list_count = unsafe { info.getProgramListCount() };
        for list_index in 0..list_count.max(0) {
            // SAFETY: ProgramListInfo contains SDK integer fields and an inline UTF-16 array, whose zero representations are valid.
            let mut list: ProgramListInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The unit interface remains retained and list is an exclusive stack output for the queried list index.
            if unsafe { info.getProgramListInfo(list_index, &mut list) } != kResultOk {
                continue;
            }
            let list_name = char16_to_string(&list.name);
            for program_index in 0..list.programCount.max(0) {
                let mut name: [char16; 128] = [0; 128];
                // SAFETY: The native list supplies both indices and name is an exclusive 128-element SDK UTF-16 output array.
                if unsafe { info.getProgramName(list.id, program_index, &mut name) } != kResultOk {
                    continue;
                }
                programs.push(crate::programs::NativeProgram {
                    index: program_index as u32,
                    list_id: list.id,
                    list_name: list_name.clone(),
                    name: char16_to_string(&name),
                });
            }
        }
        programs
    }

    /// The plugin's own state for one program, or `None` when it exposes no
    /// `IProgramListData` (or refuses that program). Returning `None` is what
    /// makes the caller fall back to a supplied state blob instead of guessing.
    pub fn program_data(&self, list_id: i32, program_index: u32) -> Option<Vec<u8>> {
        let data = self.program_list_data()?;
        let stream = Vst3StateStream::new(Vec::new());
        let bytes = Rc::clone(&stream.data);
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper.to_com_ptr::<IBStream>()?;
        let result =
            // SAFETY: The program-data interface and COM stream wrapper stay retained through this synchronous output-stream call.
            unsafe { data.getProgramData(list_id, program_index as i32, com_stream.as_ptr()) };
        if result != kResultOk {
            return None;
        }
        let bytes = bytes.borrow().clone();
        (!bytes.is_empty()).then_some(bytes)
    }

    /// Parameter inventory with current normalized values in one controller pass.
    pub fn get_params_with_values(&self) -> Vec<(Vst3Param, f64)> {
        if self.failed() {
            return Vec::new();
        }
        let mut params = Vec::new();
        let Some(ctrl) = self.controller.as_ref() else {
            return params;
        };
        // SAFETY: self retains ctrl for the synchronous query; UI-thread admission remains the legacy caller responsibility.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: ParameterInfo consists of SDK scalars and inline UTF-16 arrays with valid all-zero representations.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The retained controller receives a queried index and an exclusive stack ParameterInfo output.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            // SAFETY: info.id came from this retained controller; this scalar read exposes no Rust pointer.
            let current = unsafe { ctrl.getParamNormalized(info.id) };
            params.push((
                Vst3Param {
                    id: info.id,
                    name: char16_to_string(&info.title),
                    default: info.defaultNormalizedValue,
                    step_count: info.stepCount,
                    unit: char16_to_string(&info.units),
                },
                current,
            ));
        }
        params
    }

    /// Parameter inventory including formatted ranges and VST3 flags.
    pub fn get_param_details(&self) -> Vec<Vst3ParamDetail> {
        if self.failed() {
            return Vec::new();
        }
        let mut params = Vec::new();
        let Some(ctrl) = self.controller.as_ref() else {
            return params;
        };
        // SAFETY: self retains ctrl through this query; the legacy caller must serialize controller access on UI.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: ParameterInfo contains only SDK scalars and inline arrays, all permitting zero initialization.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The queried index and exclusive stack descriptor remain valid through the retained controller call.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            let text = |normalized: f64| {
                let mut raw: [char16; 128] = [0; 128];
                // SAFETY: info.id is from this controller and raw is the full writable SDK String128 array for this call.
                if unsafe { ctrl.getParamStringByValue(info.id, normalized, &mut raw) } == kResultOk
                {
                    char16_to_string(&raw)
                } else {
                    String::new()
                }
            };
            let min_text = text(0.0);
            let max_text = text(1.0);
            let default_text = text(info.defaultNormalizedValue);
            let step_texts = if (1..=64).contains(&info.stepCount) {
                (0..=info.stepCount)
                    .map(|step| text(f64::from(step) / f64::from(info.stepCount)))
                    .collect()
            } else {
                Vec::new()
            };
            params.push(Vst3ParamDetail {
                id: info.id,
                name: char16_to_string(&info.title),
                unit: char16_to_string(&info.units),
                default_normalized: info.defaultNormalizedValue,
                min_text,
                max_text,
                default_text,
                step_count: info.stepCount,
                step_texts,
                flags: info.flags,
                read_only: info.flags & ParameterFlags_::kIsReadOnly != 0,
                hidden: info.flags & ParameterFlags_::kIsHidden != 0,
                list: info.flags & ParameterFlags_::kIsList != 0,
                can_automate: parameter_can_automate(info.flags),
            });
        }
        params
    }

    /// Set a parameter by name; false when unknown or failed.
    pub fn set_param_by_name(&mut self, name: &str, value: f64) -> bool {
        if self.failed() {
            return false;
        }
        let Some((id, normalized)) = self.param_id_and_normalized_value(name, value, false) else {
            return false;
        };
        let Some(ctrl) = self.controller.as_ref() else {
            return false;
        };
        // SAFETY: The retained controller receives the ID/value resolved above; this legacy API leaves UI serialization to its caller.
        let result = crate::ffi_guard::guard(|| unsafe { ctrl.setParamNormalized(id, normalized) });
        if result != Ok(kResultOk) {
            return false;
        }
        // The edit controller is the host/UI-side value owner. The audio
        // processor receives parameter changes only through ProcessData, so a
        // static project/preset assignment must also be queued for its first
        // processing block.
        self.remember_value(id, normalized);
        self.pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id,
                value: normalized,
                sample_offset: 0,
            }));
        true
    }

    /// Queue a normalized parameter write at a sample offset.
    pub fn queue_param_by_name(&mut self, name: &str, value: f64, sample_offset: i32) -> bool {
        if self.failed() {
            return false;
        }
        let Some((id, normalized)) = self.param_id_and_normalized_value(name, value, true) else {
            return false;
        };
        self.pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id,
                value: normalized,
                sample_offset,
            }));
        true
    }

    /// Queue a parameter change in the plugin's plain domain at a sample
    /// offset inside the next block.
    pub fn queue_param_plain_by_name(
        &mut self,
        name: &str,
        plain: f64,
        sample_offset: i32,
    ) -> bool {
        if self.failed() {
            return false;
        }
        let Some(info) = self.param_info_by_name(name) else {
            return false;
        };
        if !parameter_can_automate(info.flags) {
            return false;
        }
        let Some(ctrl) = self.controller.as_ref() else {
            return false;
        };
        let Ok(plain) =
            // SAFETY: info.id came from this retained controller and conversion passes scalar values only; caller owns UI affinity.
            crate::ffi_guard::guard(|| unsafe { ctrl.plainParamToNormalized(info.id, plain) })
        else {
            return false;
        };
        let normalized = plain.clamp(0.0, 1.0);
        self.pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id: info.id,
                value: normalized,
                sample_offset,
            }));
        true
    }

    /// Set a parameter from the plug-in plain domain (`plainParamToNormalized`).
    ///
    /// [`Self::set_param_by_name`] treats 0..=1 as already normalised, which is
    /// wrong for `.ffp` presets: those store both 0..1 curve values and
    /// engineering values, and only the plug-in knows which is which.
    pub fn set_param_plain_by_name(&mut self, name: &str, plain: f64) -> bool {
        if self.failed() {
            return false;
        }
        let Some(info) = self.param_info_by_name(name) else {
            return false;
        };
        let Some(ctrl) = self.controller.as_ref() else {
            return false;
        };
        let Ok(plain) =
            // SAFETY: info.id came from this retained controller and conversion passes scalar values only; caller owns UI affinity.
            crate::ffi_guard::guard(|| unsafe { ctrl.plainParamToNormalized(info.id, plain) })
        else {
            return false;
        };
        let normalized = plain.clamp(0.0, 1.0);
        // SAFETY: The retained controller receives the converted scalar ID/value; the legacy caller must serialize UI access.
        if crate::ffi_guard::guard(|| unsafe { ctrl.setParamNormalized(info.id, normalized) })
            != Ok(kResultOk)
        {
            return false;
        }
        self.remember_value(info.id, normalized);
        self.pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id: info.id,
                value: normalized,
                sample_offset: 0,
            }));
        true
    }

    /// Read a parameter back in the plug-in plain domain.
    pub fn get_param_plain_by_name(&self, name: &str) -> Option<f64> {
        if self.failed() {
            return None;
        }
        let info = self.param_info_by_name(name)?;
        let ctrl = self.controller.as_ref()?;
        let normalized =
            // SAFETY: The queried descriptor and retained controller identify this scalar value; legacy caller owns UI affinity.
            crate::ffi_guard::guard(|| unsafe { ctrl.getParamNormalized(info.id) }).ok()?;
        // SAFETY: The retained controller converts its own parameter value using scalar arguments; legacy caller owns UI affinity.
        crate::ffi_guard::guard(|| unsafe { ctrl.normalizedParamToPlain(info.id, normalized) }).ok()
    }

    /// Current normalized value of a named parameter.
    pub fn get_param_by_name(&self, name: &str) -> Option<f64> {
        if self.failed() {
            return None;
        }
        let ctrl = self.controller.as_ref()?;
        let target = normalize_param_name(name);
        // SAFETY: The controller borrow retains the native object during this query; legacy caller owns UI affinity.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: ParameterInfo has valid all-zero SDK scalar and inline-array representations.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The controller remains retained while writing the exclusive stack descriptor for the queried index.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            if normalize_param_name(&char16_to_string(&info.title)) == target {
                // SAFETY: The matched ID was reported by this retained controller; the synchronous result is a scalar.
                return crate::ffi_guard::guard(|| unsafe { ctrl.getParamNormalized(info.id) })
                    .ok();
            }
        }
        None
    }

    /// Parameter ids this instance's controller exposes, in controller order.
    pub fn param_ids(&self) -> Vec<ParamID> {
        if self.failed() {
            return Vec::new();
        }
        let Some(controller) = self.controller.as_ref() else {
            return Vec::new();
        };
        // SAFETY: The controller remains retained during the scalar count query; UI affinity is a legacy caller precondition.
        let count = unsafe { controller.getParameterCount() }.max(0);
        let mut ids = Vec::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: ParameterInfo is composed of SDK scalars and inline UTF-16 arrays, permitting zero initialization.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: index comes from the count query and info is an exclusive stack output under the retained controller borrow.
            if unsafe { controller.getParameterInfo(index, &mut info) } == kResultOk {
                ids.push(info.id);
            }
        }
        ids
    }

    /// Current normalized value of each id in `ids`, in the same order.
    ///
    /// Reads values only, so a caller that already holds the inventory
    /// refreshes it without paying `getParameterInfo` per parameter. A value
    /// the controller cannot answer comes back as `NaN`.
    pub fn param_values(&self, ids: &[ParamID]) -> Vec<f64> {
        if self.failed() {
            return Vec::new();
        }
        let Some(controller) = self.controller.as_ref() else {
            return Vec::new();
        };
        ids.iter()
            .map(|&id| {
                // SAFETY: The controller remains retained through each scalar ID query; callers own UI affinity and serialization.
                crate::ffi_guard::guard(|| unsafe { controller.getParamNormalized(id) })
                    .unwrap_or(f64::NAN)
            })
            .collect()
    }

    /// Attach this instance to the editor edits of `preset`.
    ///
    /// A processor drains the bus before every block; an editor-owning instance
    /// also publishes its `IComponentHandler` edits into it. The parameter
    /// layout is fixed by the first instance that binds.
    pub fn bind_preset_edits(&mut self, preset: &str) {
        let ids = self.param_ids();
        if ids.is_empty() {
            return;
        }
        let bus = crate::param_edit::bus_for_preset(preset, ids.iter().copied());
        self.pending_events.reserve(ids.len());
        if let Some(host) = &*self._host {
            host.edit_sink().bind_bus(Arc::clone(&bus));
        }
        self.edit_receiver = Some(bus.receiver());
        self.edit_bus = Some(bus);
    }

    /// The `IComponentHandler` a plug-in editor reports edits to.
    #[cfg(any(test, feature = "test-support"))]
    pub fn edit_handler(&self) -> Option<ComPtr<IComponentHandler>> {
        self._host.as_ref()?.to_com_ptr::<IComponentHandler>()
    }

    /// Forward this instance's editor edits to the preset's live processors.
    #[cfg(unix)]
    pub fn bind_edit_socket(&mut self, socket: crate::param_edit::EditSocket) -> bool {
        let Some(host) = &*self._host else {
            return false;
        };
        host.edit_sink().bind_socket(socket);
        true
    }

    /// Publish this controller's values so later subscribers start from them.
    pub fn publish_controller_values(&self) -> usize {
        if self.failed() {
            return 0;
        }
        let (Some(bus), Some(controller)) = (self.edit_bus.as_ref(), self.controller.as_ref())
        else {
            return 0;
        };
        // SAFETY: This instance retains the controller throughout enumeration; the legacy caller owns UI affinity.
        let count = unsafe { controller.getParameterCount() }.max(0);
        let mut published = 0;
        for index in 0..count {
            // SAFETY: All ParameterInfo fields are SDK scalars or inline UTF-16 arrays with valid zero representations.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The controller stays retained and writes only the live stack descriptor for the queried index.
            if unsafe { controller.getParameterInfo(index, &mut info) } == kResultOk
                && let Ok(value) =
                    // SAFETY: info.id was just reported by the retained controller; this query returns a scalar value.
                    crate::ffi_guard::guard(|| unsafe {
                        controller.getParamNormalized(info.id)
                    })
                && bus.publish(info.id, value)
            {
                published += 1;
            }
        }
        published
    }

    /// Host-side edit on the editor's instance: publish the controller value
    /// so live processors follow it too.
    pub fn publish_controller_value(&self, name: &str) -> bool {
        if self.failed() {
            return false;
        }
        let (Some(bus), Some(controller)) = (self.edit_bus.as_ref(), self.controller.as_ref())
        else {
            return false;
        };
        let Some(info) = self.param_info_by_name(name) else {
            return false;
        };
        // SAFETY: The descriptor ID and controller remain live through this scalar read; caller owns UI affinity.
        bus.publish(info.id, unsafe { controller.getParamNormalized(info.id) })
    }

    /// Save path on the UI thread: apply forwarded edits to this controller.
    pub fn absorb_editor_edits(&mut self) -> usize {
        if self.failed() {
            return 0;
        }
        let (Some(receiver), Some(controller)) =
            (self.edit_receiver.as_mut(), self.controller.as_ref())
        else {
            return 0;
        };
        let applied = &mut self.applied_values;
        receiver.drain(|id, value| {
            // SAFETY: `setParamNormalized` on the UI thread, as VST3 requires.
            let result =
                crate::ffi_guard::guard(|| unsafe { controller.setParamNormalized(id, value) });
            if result != Ok(kResultOk) {
                oc_diag::diag_warn!("plugin: setParamNormalized({id}) failed");
                return;
            }
            match applied.iter_mut().find(|(existing, _)| *existing == id) {
                Some((_, existing)) => *existing = value,
                None => applied.push((id, value)),
            }
        })
    }

    /// Push every controller value through the processor so its state and the
    /// next block match what the editor shows. UI thread only.
    pub fn sync_processor_from_controller(&mut self) -> usize {
        if self.failed() {
            return 0;
        }
        let Some(controller) = self.controller.as_ref() else {
            return 0;
        };
        // SAFETY: The retained controller is borrowed throughout this UI-only synchronization operation.
        let count = unsafe { controller.getParameterCount() }.max(0);
        let mut events = Vec::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: ParameterInfo contains only zero-valid SDK scalar fields and inline arrays.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The retained controller borrows the writable stack descriptor for this queried index.
            if unsafe { controller.getParameterInfo(index, &mut info) } != kResultOk {
                continue;
            }
            let Ok(value) =
                // SAFETY: The freshly queried native ID and retained controller remain live for this scalar read.
                crate::ffi_guard::guard(|| unsafe { controller.getParamNormalized(info.id) })
            else {
                continue;
            };
            events.push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id: info.id,
                value,
                sample_offset: 0,
            }));
        }
        let synced = events.len();
        self.pending_events.extend(events);
        if self.active && self.processing && !self.failed() {
            let _ = self.render_block(self.max_frames.max(1) as u32, None);
        }
        synced
    }

    /// Move editor edits into the parameter queue: no lock, one point per
    /// parameter per block.
    fn drain_editor_edits(&mut self) {
        let Some(receiver) = self.edit_receiver.as_mut() else {
            return;
        };
        let pending = &mut self.pending_events;
        receiver.drain(|id, value| {
            pending.push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id,
                value,
                sample_offset: 0,
            }));
        });
    }

    /// Apply processor state only, without touching the editor's state.
    pub fn set_component_state_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.set_component_state_without_layout(bytes)?;
        self.reload_output_layout();
        self.healthy().map_err(|error| error.to_string())
    }

    /// Apply processor state and leave the bus layout alone. A plugin that
    /// loads a large multi asynchronously can be mid-load when a host asks for
    /// its buses, so a caller that knows the layout is unchanged uses this.
    pub fn set_component_state_without_layout(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.healthy().map_err(|error| error.to_string())?;
        let stream = Vst3StateStream::new(bytes.to_vec());
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::SetState) {
            return Err(self
                .native_fault(Operation::ComponentSetState, fault)
                .to_string());
        }
        // SAFETY: both the retained component and host stream outlive the native call.
        let result = self.control_result(Operation::ComponentSetState, unsafe {
            vst3_guard::component_set_state(self.component.as_ptr(), com_stream.as_ptr())
        })?;
        if result != kResultOk {
            return Err(format!("VST3 setState failed (0x{result:x})"));
        }
        self.invalidate_param_cache();
        Ok(())
    }

    /// Apply processor and controller state, so the editor reflects the preset.
    ///
    /// `IEditController::setComponentState` is optional: a processor-only plugin
    /// answers `kNotImplemented`, which is not a failure. A controller that
    /// implements the call and still rejects the blob is synchronized from the
    /// parameter values the host has already applied, so the load does not fail
    /// on a controller that cannot read its own processor's state.
    pub fn set_state_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.healthy().map_err(|error| error.to_string())?;
        self.set_component_state_bytes(bytes)?;
        let Some(controller) = self.controller.as_ref() else {
            return Ok(());
        };
        let stream = Vst3StateStream::new(bytes.to_vec());
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        let _ =
            // SAFETY: com_stream retains the host-owned stream; seeking from its start accepts the SDK optional null result pointer.
            unsafe { com_stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut()) };
        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::SetComponentState) {
            return Err(self
                .native_fault(Operation::ControllerComponentState, fault)
                .to_string());
        }
        // SAFETY: the borrowed controller and live host stream are retained for the call.
        let result = self.control_result(Operation::ControllerComponentState, unsafe {
            vst3_guard::controller_component_state(controller.as_ptr(), com_stream.as_ptr())
        })?;
        if result == kResultOk {
            return Ok(());
        }
        if result == kNotImplemented {
            oc_diag::diag_info!("plugin: the controller has no component state");
            return Ok(());
        }
        oc_diag::diag_warn!(
            "plugin: the controller rejected the component state (0x{result:x}); \
             re-applying the parameter values the host already set"
        );
        self.reapply_known_values(controller);
        Ok(())
    }

    /// Push the values the host already set into a controller that refused a
    /// component state.
    fn reapply_known_values(&self, controller: &Vst3ForeignRef<IEditController>) {
        for (id, value) in &self.applied_values {
            // SAFETY: The instance retains the controller and each stored ID/value was previously assigned by this host; caller owns UI affinity.
            if crate::ffi_guard::guard(|| unsafe { controller.setParamNormalized(*id, *value) })
                != Ok(kResultOk)
            {
                oc_diag::diag_warn!("plugin: could not re-apply parameter {id}");
            }
        }
    }

    /// Record the last normalized value the host set for a parameter.
    fn remember_value(&mut self, id: ParamID, value: f64) {
        match self
            .applied_values
            .iter_mut()
            .find(|(existing, _)| *existing == id)
        {
            Some((_, existing)) => *existing = value,
            None => self.applied_values.push((id, value)),
        }
    }

    /// Round-trip the controller's own state, for host experiments.
    #[cfg(any(test, feature = "test-support"))]
    pub fn roundtrip_controller_state(&self) -> Result<usize, String> {
        self.healthy().map_err(|error| error.to_string())?;
        let controller = self.controller.as_ref().ok_or("VST3: no edit controller")?;
        let stream = Vst3StateStream::new(Vec::new());
        let data = stream.data.clone();
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        // SAFETY: the healthy retained controller and host stream outlive this control call.
        if self.control_result(Operation::ControllerGetState, unsafe {
            vst3_guard::controller_get_state(controller.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 controller getState failed".to_string());
        }
        let bytes = data.borrow().clone();
        let length = bytes.len();
        let wrapper = ComWrapper::new(Vst3StateStream::new(bytes));
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        // SAFETY: the healthy retained controller and host stream outlive this control call.
        if self.control_result(Operation::ControllerSetState, unsafe {
            vst3_guard::controller_set_state(controller.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 controller setState failed".to_string());
        }
        Ok(length)
    }

    /// Feed a component-state blob to the edit controller, for editor experiments.
    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_controller_component_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.healthy().map_err(|error| error.to_string())?;
        let controller = self.controller.as_ref().ok_or("VST3: no edit controller")?;
        let stream = Vst3StateStream::new(bytes.to_vec());
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        // SAFETY: the healthy retained controller and host stream outlive this control call.
        if self.control_result(Operation::ControllerComponentState, unsafe {
            vst3_guard::controller_component_state(controller.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 controller setComponentState failed".to_string());
        }
        Ok(())
    }

    /// Feed a blob to `IEditController::setState`, for editor experiments.
    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_controller_own_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.healthy().map_err(|error| error.to_string())?;
        let controller = self.controller.as_ref().ok_or("VST3: no edit controller")?;
        let stream = Vst3StateStream::new(bytes.to_vec());
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        // SAFETY: the healthy retained controller and host stream outlive this control call.
        if self.control_result(Operation::ControllerSetState, unsafe {
            vst3_guard::controller_set_state(controller.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 controller setState failed".to_string());
        }
        Ok(())
    }

    /// Capture the controller's own state without applying it back.
    #[cfg(any(test, feature = "test-support"))]
    pub fn controller_state_bytes(&self) -> Result<Vec<u8>, String> {
        self.healthy().map_err(|error| error.to_string())?;
        let controller = self.controller.as_ref().ok_or("VST3: no edit controller")?;
        let stream = Vst3StateStream::new(Vec::new());
        let data = stream.data.clone();
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;
        // SAFETY: the healthy retained controller and host stream outlive this control call.
        if self.control_result(Operation::ControllerGetState, unsafe {
            vst3_guard::controller_get_state(controller.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 controller getState failed".to_string());
        }
        Ok(data.borrow().clone())
    }

    /// Capture the component state.
    pub fn state_bytes(&self) -> Result<Vec<u8>, String> {
        self.healthy().map_err(|error| error.to_string())?;
        let stream = Vst3StateStream::new(Vec::new());
        let data = stream.data.clone();
        let wrapper = ComWrapper::new(stream);
        let com_stream = wrapper
            .to_com_ptr::<IBStream>()
            .ok_or("VST3: failed to create state stream")?;

        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::GetState) {
            return Err(self
                .native_fault(Operation::ComponentGetState, fault)
                .to_string());
        }
        // SAFETY: the retained component and host stream remain live through the native call.
        if self.control_result(Operation::ComponentGetState, unsafe {
            vst3_guard::component_get_state(self.component.as_ptr(), com_stream.as_ptr())
        })? != kResultOk
        {
            return Err("VST3 getState failed".to_string());
        }

        Ok(data.borrow().clone())
    }

    /// Connect a host-side connection point to the component and controller.
    ///
    /// # Safety
    /// `host` must point to a live `IConnectionPoint` that outlives the calls.
    #[cfg(any(test, feature = "test-support"))]
    pub unsafe fn connect_host_connection_point(&mut self, host: *mut IConnectionPoint) {
        if self.failed() {
            return;
        }
        if let Some(component) = self.component.cast::<IConnectionPoint>() {
            // SAFETY: The unsafe caller guarantees a live host connection point; the queried component interface is retained through connect.
            let _ = unsafe { component.connect(host) };
        }
        if let Some(controller) = &*self.controller
            && let Some(controller) = controller.cast::<IConnectionPoint>()
        {
            // SAFETY: The unsafe caller guarantees a live host connection point; the queried controller interface is retained through connect.
            let _ = unsafe { controller.connect(host) };
        }
    }

    /// Set the controller's component handler for host-side editor experiments.
    ///
    /// # Safety
    /// `handler` must point to a live `IComponentHandler` that outlives the call.
    #[cfg(any(test, feature = "test-support"))]
    pub unsafe fn set_component_handler_for_test(
        &self,
        handler: *mut vst3::Steinberg::Vst::IComponentHandler,
    ) -> vst3::Steinberg::tresult {
        match &*self.controller {
            // SAFETY: The unsafe caller guarantees handler validity during the call; self retains the controller interface.
            Some(controller) => unsafe { controller.setComponentHandler(handler) },
            None => kResultOk,
        }
    }

    /// Cycle `setProcessing` for host-side load experiments.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_processing_for_test(&mut self, on: bool) {
        if self.failed() {
            return;
        }
        // SAFETY: the healthy retained processor is valid for this control call.
        match unsafe { vst3_guard::processing(self.processor.as_ptr(), on) } {
            Ok(code) if code == kResultOk => self.processing = on,
            Ok(_) => {}
            Err(fault) => {
                self.native_fault(Operation::SetProcessing, fault);
            }
        }
    }

    /// Return the component to its initial processing state without reloading
    /// it. Returns `false` when the component refuses the stop/start cycle, so
    /// the caller can rebuild the lane instead.
    pub fn reset(&mut self) -> bool {
        if self.failed() {
            return false;
        }
        self.pending_events.clear();
        self.note_ids.clear();
        if !self.processing {
            return true;
        }
        // SAFETY: this healthy instance retains its processor throughout the stop/start cycle.
        let stopped = unsafe { vst3_guard::processing(self.processor.as_ptr(), false) };
        match stopped {
            Ok(code) if code == kResultOk => {}
            Ok(_) => return false,
            Err(fault) => {
                self.native_fault(Operation::SetProcessing, fault);
                return false;
            }
        }
        // SAFETY: the stop succeeded without a native fault and the processor is retained.
        match unsafe { vst3_guard::processing(self.processor.as_ptr(), true) } {
            Ok(code) => code == kResultOk,
            Err(fault) => {
                self.native_fault(Operation::SetProcessing, fault);
                false
            }
        }
    }

    /// Cycle `setActive` for host-side load experiments.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_active_for_test(&mut self, on: bool) {
        if self.failed() {
            return;
        }
        // SAFETY: the healthy retained component is valid for this control call.
        match unsafe { vst3_guard::active(self.component.as_ptr(), on) } {
            Ok(code) if code == kResultOk => self.active = on,
            Ok(_) => {}
            Err(fault) => {
                self.native_fault(Operation::SetActive, fault);
            }
        }
    }

    /// Queue a note-on on channel 0.
    pub fn note_on(&mut self, key: u8, velocity: f32, sample_offset: i32) {
        self.note_on_channel(0, key, velocity, sample_offset);
    }

    /// Queue a note-on with a sample offset.
    pub fn note_on_channel(&mut self, channel: u8, key: u8, velocity: f32, sample_offset: i32) {
        let channel = channel as i16;
        let key = key as i16;
        let note_id = self.note_ids.note_on(channel, key);
        self.pending_events
            .push(Vst3PendingEvent::Note(Vst3NoteEvent {
                on: true,
                channel,
                key,
                velocity,
                sample_offset,
                note_id,
            }));
    }

    /// Queue a note-off on channel 0.
    pub fn note_off(&mut self, key: u8, sample_offset: i32) {
        self.note_off_channel(0, key, sample_offset);
    }

    /// Queue a note-off with a sample offset.
    pub fn note_off_channel(&mut self, channel: u8, key: u8, sample_offset: i32) {
        self.note_off_channel_velocity(channel, key, 0, sample_offset);
    }

    /// Queue a note-off with an explicit release velocity.
    pub fn note_off_channel_velocity(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        sample_offset: i32,
    ) -> bool {
        if channel > 15 || key > 127 || velocity > 127 || sample_offset < 0 {
            return false;
        }
        let channel = channel as i16;
        let key = key as i16;
        let note_id = self.note_ids.note_off(channel, key);
        self.pending_events
            .push(Vst3PendingEvent::Note(Vst3NoteEvent {
                on: false,
                channel,
                key,
                velocity: f32::from(velocity) / 127.0,
                sample_offset,
                note_id,
            }));
        true
    }

    /// Queue a mapped control change.
    pub fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.queue_mapped_midi(
            channel,
            i16::from(controller),
            i32::from(value),
            sample_offset,
        );
    }

    /// Queue a mapped program change.
    pub fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.queue_mapped_midi(channel, 130, i32::from(program), sample_offset);
    }

    /// Queue a mapped pitch bend.
    pub fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.queue_mapped_midi(channel, 129, i32::from(value), sample_offset);
    }

    /// Queue mapped channel pressure.
    pub fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.queue_mapped_midi(channel, 128, i32::from(pressure), sample_offset);
    }

    /// Checked MIDI controller mapping for event input bus zero. False means
    /// unsupported/invalid: authoritative performance callers must reject it.
    /// Legacy void wrappers above retain their API but now deliver mapped data.
    pub fn queue_mapped_midi(
        &mut self,
        channel: u8,
        controller: i16,
        value: i32,
        sample_offset: i32,
    ) -> bool {
        if channel > 15 || !(0..=130).contains(&controller) || sample_offset < 0 {
            return false;
        }
        let Some(ctrl) = self.controller.as_ref() else {
            return false;
        };
        let Some(mapping) = ctrl.cast::<IMidiMapping>() else {
            return false;
        };
        let mut id = 0;
        // SAFETY: The retained mapping interface receives a live scalar output; channel/controller were range-checked above.
        if unsafe {
            mapping.getMidiControllerAssignment(0, i16::from(channel), controller, &mut id)
        } != kResultOk
        {
            return false;
        }
        let mut steps = None;
        if controller == 130 {
            // SAFETY: ctrl is retained through this scalar query; the legacy MIDI mapper leaves UI affinity to its caller.
            for index in 0..unsafe { ctrl.getParameterCount() } {
                // SAFETY: ParameterInfo contains SDK scalars and inline arrays whose all-zero representation is valid.
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                // SAFETY: index comes from the retained controller count and info is an exclusive stack output for this call.
                if unsafe { ctrl.getParameterInfo(index, &mut info) } == kResultOk && info.id == id
                {
                    steps = Some(info.stepCount);
                    break;
                }
            }
        }
        let Some(normalized) = mapped_midi_normalized(controller, value, steps) else {
            return false;
        };
        self.pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id,
                value: normalized,
                sample_offset,
            }));
        true
    }

    /// Queue poly pressure for an active note.
    pub fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        let channel = channel as i16;
        let key = note as i16;
        let note_id = self.note_ids.active_note_id(channel, key);
        self.pending_events
            .push(Vst3PendingEvent::PolyPressure(Vst3PolyPressureEvent {
                channel,
                key,
                pressure: pressure as f32 / 127.0,
                sample_offset,
                note_id,
            }));
    }

    /// Queue a note expression by name.
    pub fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    ) {
        if let Some(kind) = Vst3StandardNoteExpression::from_name(expression) {
            let value = if kind == Vst3StandardNoteExpression::Tuning {
                Vst3NoteExpressionValue::TuningSemitones(value)
            } else {
                Vst3NoteExpressionValue::Normalized(value)
            };
            // The legacy void adapter retains its signature. Checked callers use
            // queue_note_expression to observe domain/address refusal explicitly.
            let _ = self.queue_note_expression(channel, note, kind, value, sample_offset);
        }
    }

    /// Queue a typed note expression at a sample offset.
    pub fn queue_note_expression(
        &mut self,
        channel: u8,
        note: u8,
        kind: Vst3StandardNoteExpression,
        value: Vst3NoteExpressionValue,
        sample_offset: i32,
    ) -> Result<(), Vst3NoteExpressionError> {
        if channel > 15 || note > 127 || sample_offset < 0 {
            return Err(Vst3NoteExpressionError::InvalidAddress);
        }
        let value = value.normalized_for(kind)?;
        let note_id = self
            .note_ids
            .active_note_id(i16::from(channel), i16::from(note));
        self.pending_events
            .push(Vst3PendingEvent::NoteExpression(Vst3NoteExpressionEvent {
                type_id: kind.native_type_id(),
                value,
                sample_offset,
                note_id,
            }));
        Ok(())
    }

    fn parameter_inventory_revision(&self) -> u64 {
        self._host
            .as_ref()
            .map_or(0, |host| host.edit_sink().parameter_inventory_revision())
    }

    /// Drop the parameter name lookup after the plugin changed its list.
    fn invalidate_param_cache(&mut self) {
        self.authored_parameter_revision = self.authored_parameter_revision.wrapping_add(1);
        self.param_cache.valid = false;
    }

    fn rebuild_param_cache(&mut self) {
        let mut by_name = HashMap::new();
        if let Some(ctrl) = self.controller.as_ref() {
            // SAFETY: The instance retains ctrl throughout this metadata query; the legacy cache caller owns UI affinity.
            let count = unsafe { ctrl.getParameterCount() }.max(0);
            for index in 0..count {
                // SAFETY: ParameterInfo is composed of zero-valid SDK scalars and inline UTF-16 arrays.
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                // SAFETY: The retained controller receives a queried index and an exclusive writable descriptor until return.
                if unsafe { ctrl.getParameterInfo(index, &mut info) } != kResultOk {
                    continue;
                }
                let key = normalize_param_name(&char16_to_string(&info.title));
                by_name.entry(key).or_insert(CachedParam {
                    id: info.id,
                    flags: info.flags,
                });
            }
        }
        self.param_cache.by_name = by_name;
        self.param_cache.valid = true;
    }

    fn ensure_param_cache(&mut self) {
        if !self.param_cache.valid {
            self.rebuild_param_cache();
        }
    }

    fn cached_param(&mut self, name: &str) -> Option<CachedParam> {
        self.ensure_param_cache();
        self.param_cache
            .by_name
            .get(&normalize_param_name(name))
            .copied()
    }

    fn param_info_by_name(&self, name: &str) -> Option<ParameterInfo> {
        let ctrl = self.controller.as_ref()?;
        let target = normalize_param_name(name);
        // SAFETY: The retained controller remains live during the count query; the legacy accessor requires caller UI affinity.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: ParameterInfo fields are SDK scalar values and inline arrays with valid all-zero representations.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The retained controller writes only the exclusive stack descriptor for this queried index.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            if normalize_param_name(&char16_to_string(&info.title)) == target {
                return Some(info);
            }
        }
        None
    }

    fn param_id_and_normalized_value(
        &mut self,
        name: &str,
        value: f64,
        require_automatable: bool,
    ) -> Option<(ParamID, ParamValue)> {
        let Some(param) = self.cached_param(name) else {
            return self.param_id_and_normalized_value_uncached(name, value, require_automatable);
        };
        if require_automatable && !parameter_can_automate(param.flags) {
            return None;
        }
        let ctrl = self.controller.as_ref()?;
        let normalized = if (0.0..=1.0).contains(&value) {
            value
        } else {
            // SAFETY: The cached ID belongs to the retained controller and only scalars cross this call; legacy caller owns UI affinity.
            unsafe { ctrl.plainParamToNormalized(param.id, value) }
        }
        .clamp(0.0, 1.0);
        Some((param.id, normalized))
    }

    /// Full enumeration for a name the cache does not know.
    fn param_id_and_normalized_value_uncached(
        &self,
        name: &str,
        value: f64,
        require_automatable: bool,
    ) -> Option<(ParamID, ParamValue)> {
        let ctrl = self.controller.as_ref()?;
        let target = normalize_param_name(name);
        // SAFETY: The retained controller stays live during enumeration; UI affinity remains the legacy caller precondition.
        let count = unsafe { ctrl.getParameterCount() };
        for i in 0..count {
            // SAFETY: All ParameterInfo scalar and inline-array fields permit an all-zero representation.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: The retained controller receives the queried index and a live exclusive stack descriptor.
            if unsafe { ctrl.getParameterInfo(i, &mut info) } != kResultOk {
                continue;
            }
            if normalize_param_name(&char16_to_string(&info.title)) != target {
                continue;
            }
            if require_automatable && !parameter_can_automate(info.flags) {
                return None;
            }

            let normalized = if (0.0..=1.0).contains(&value) {
                value
            } else {
                // SAFETY: The matching descriptor belongs to this retained controller; scalar conversion requires caller-owned UI affinity.
                unsafe { ctrl.plainParamToNormalized(info.id, value) }
            }
            .clamp(0.0, 1.0);
            return Some((info.id, normalized));
        }
        None
    }

    /// Process a block of audio. Returns stereo interleaved f32.
    pub fn process_block(&mut self, num_frames: u32) -> Vec<f32> {
        self.process_block_with_transport(num_frames, None)
    }

    /// Process a block of audio with host timeline context.
    pub fn process_block_with_transport(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        let mut output = Vec::with_capacity((num_frames * 2) as usize);
        if self
            .process_block_outputs_into(
                num_frames,
                transport,
                &[0],
                std::slice::from_mut(&mut output),
            )
            .is_err()
        {
            return vec![0.0; (num_frames * 2) as usize];
        }
        output
    }

    /// Render one block and return one stereo output pair.
    pub fn process_block_with_transport_and_output_pair(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
        output_pair: usize,
    ) -> Vec<f32> {
        self.process_block_with_transport_and_output_pairs(num_frames, transport, &[output_pair])
            .pop()
            .unwrap_or_else(|| vec![0.0; (num_frames * 2) as usize])
    }

    /// Process once and fan several declared stereo output pairs from the same
    /// VST3 block so shared instruments are not advanced once per stem.
    pub fn process_block_with_transport_and_output_pairs(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
        output_pairs: &[usize],
    ) -> Vec<Vec<f32>> {
        let mut outputs: Vec<Vec<f32>> = output_pairs
            .iter()
            .map(|_| Vec::with_capacity((num_frames * 2) as usize))
            .collect();
        if self
            .process_block_outputs_into(num_frames, transport, output_pairs, &mut outputs)
            .is_err()
        {
            return output_pairs
                .iter()
                .map(|_| vec![0.0; (num_frames * 2) as usize])
                .collect();
        }
        outputs
    }

    /// Render one block and append every requested stereo pair to `outputs`.
    ///
    /// Pair 0 is the main output bus; auxiliary pairs follow the component's
    /// declared bus order. Callers must pre-reserve `outputs` to stay
    /// allocation-free in the audio callback.
    #[expect(
        clippy::result_large_err,
        reason = "the native cause stays inline so failure handling never allocates on the audio thread"
    )]
    /// Render one block into caller-owned output buffers.
    pub fn process_block_outputs_into(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
        output_pairs: &[usize],
        outputs: &mut [Vec<f32>],
    ) -> Result<(), Vst3ProcessError> {
        if outputs.len() != output_pairs.len() {
            return Err(Vst3ProcessError::host_contract(
                "VST3 output request count mismatch",
            ));
        }
        if num_frames == 0 {
            return Ok(());
        }
        if self.failed() {
            for output in outputs.iter_mut() {
                output.extend(std::iter::repeat_n(0.0, (num_frames * 2) as usize));
            }
            return self.healthy();
        }
        if !self.active || !self.processing {
            for output in outputs.iter_mut() {
                output.extend(std::iter::repeat_n(0.0, (num_frames * 2) as usize));
            }
            return Ok(());
        }

        self.output_layout.ensure_frames(num_frames as usize);
        if let Err(error) = self.render_block(num_frames, transport) {
            for output in outputs.iter_mut() {
                output.extend(std::iter::repeat_n(0.0, (num_frames * 2) as usize));
            }
            return Err(error);
        }
        append_output_pairs(
            &self.output_layout.channels,
            output_pairs,
            num_frames as usize,
            outputs,
        );
        Ok(())
    }

    #[expect(
        clippy::result_large_err,
        reason = "the native cause stays inline so failure handling never allocates on the audio thread"
    )]
    fn render_block(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
    ) -> Result<(), Vst3ProcessError> {
        self.healthy()?;
        let frames = num_frames as usize;
        self.output_layout.clear(frames);
        self.drain_editor_edits();

        // Build VST3 note events and sample-accurate parameter queues in the
        // instance-owned scratch, so a warm block does not allocate.
        self.event_scratch.clear();
        self.param_scratch.clear();
        for pending in self.pending_events.drain(..) {
            match pending {
                Vst3PendingEvent::Note(n) => self.event_scratch.push(note_event_to_vst3(n)),
                Vst3PendingEvent::PolyPressure(p) => {
                    self.event_scratch.push(poly_pressure_event_to_vst3(p))
                }
                Vst3PendingEvent::NoteExpression(e) => {
                    self.event_scratch.push(note_expression_event_to_vst3(e))
                }
                Vst3PendingEvent::Param(param) => self.param_scratch.push(param),
            }
        }

        let queues = self
            .process_queues
            .get_or_insert_with(Vst3ProcessQueues::new);
        queues.load_events(&mut self.event_scratch);
        queues.load_parameters(&mut self.param_scratch);
        let input_events = queues.events_ptr.as_ptr();
        let input_parameter_changes = queues.parameters_ptr.as_ptr();

        let mut input_ptrs: [*mut Sample32; 2] = [
            self.output_layout.input_channels[0].as_mut_ptr(),
            self.output_layout.input_channels[1].as_mut_ptr(),
        ];
        let mut input_bus = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_ptrs.as_mut_ptr(),
            },
        };

        let has_audio_input = !self.input_bus_channels.is_empty();
        let mut context = transport.map(vst3_process_context);
        let mut process_data = ProcessData {
            processMode: 0,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: num_frames as int32,
            numInputs: i32::from(has_audio_input),
            numOutputs: self.output_layout.buses.len() as int32,
            inputs: if has_audio_input {
                &mut input_bus
            } else {
                ptr::null_mut()
            },
            outputs: self.output_layout.buses.as_mut_ptr(),
            inputParameterChanges: input_parameter_changes,
            outputParameterChanges: ptr::null_mut(),
            inputEvents: input_events,
            outputEvents: ptr::null_mut(),
            processContext: context
                .as_mut()
                .map_or(ptr::null_mut(), |context| context as *mut ProcessContext),
        };

        // A plugin that throws on the audio thread becomes an error the caller
        // can report, not a process abort.
        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::Process) {
            return Err(self.native_fault(Operation::Process, fault));
        }
        // SAFETY: the instance retains its processor and all process buffers.
        let result = unsafe { vst3_guard::process(self.processor.as_ptr(), &mut process_data) }
            .map_err(|fault| self.native_fault(Operation::Process, fault))?;
        if result != kResultOk {
            let error = Vst3ProcessError::process_result(result);
            self.failure.slot.record(error);
            return Err(error);
        }
        Ok(())
    }

    /// Process audio in-place (for effect plugins).
    pub fn process_effect(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.process_effect_with_transport(buffer, _sample_rate, None);
    }

    /// Process an interleaved effect block with transport.
    pub fn process_effect_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_effect_with_sidechain(buffer, None, sample_rate, transport)
    }

    /// Process an effect block with an optional sidechain.
    pub fn process_effect_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: Option<&[f32]>,
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let num_frames = buffer.len() / 2;
        if self.failed() {
            buffer.fill(0.0);
            return;
        }
        if num_frames == 0 || !self.active || !self.processing {
            return;
        }

        self.drain_editor_edits();
        // Effects get no MIDI on this path: drop queued note events instead of
        // leaking them into blocks whose inputEvents stays null.
        let mut param_events = std::mem::take(&mut self.param_scratch);
        param_events.clear();
        for event in self.pending_events.drain(..) {
            if let Vst3PendingEvent::Param(param) = event {
                param_events.push(param);
            }
        }
        for event in &mut param_events {
            event.sample_offset = event.sample_offset.max(0);
        }

        let max_frames = self.max_frames.max(1) as usize;
        let first_frames = transport
            .as_ref()
            .map(|t| t.block_frames(max_frames))
            .unwrap_or(max_frames);
        if num_frames > first_frames {
            let origin = transport.as_ref().map(|t| t.sample_pos).unwrap_or(0);
            let mut offset = 0;
            while offset < num_frames {
                let chunk_transport = transport
                    .as_ref()
                    .map(|t| t.at_sample(origin + offset as u64));
                let maximum = max_frames.min(num_frames - offset);
                let frames = chunk_transport
                    .as_ref()
                    .map(|t| t.block_frames(maximum))
                    .unwrap_or(maximum);
                let start = offset * 2;
                let chunk = &mut buffer[start..start + frames * 2];
                let side_chunk = sidechain.and_then(|side| side.get(start..start + frames * 2));
                let mut chunk_params =
                    take_param_events_for_span(&mut param_events, offset as i32, frames as i32);
                self.process_effect_block(
                    chunk,
                    side_chunk,
                    sample_rate,
                    chunk_transport,
                    &mut chunk_params,
                );
                if self.failed() {
                    buffer.fill(0.0);
                    break;
                }
                offset += frames;
            }
            // Events past this buffer stay queued, rebased to the next call.
            for event in &mut param_events {
                event.sample_offset = (event.sample_offset - num_frames as i32).max(0);
            }
            self.pending_events
                .extend(param_events.drain(..).map(Vst3PendingEvent::Param));
            self.param_scratch = param_events;
            return;
        }

        self.process_effect_block(buffer, sidechain, sample_rate, transport, &mut param_events);
        self.param_scratch = param_events;
    }

    fn process_effect_block(
        &mut self,
        buffer: &mut [f32],
        sidechain: Option<&[f32]>,
        _sample_rate: u32,
        transport: Option<PluginTransport>,
        param_events: &mut Vec<Vst3ParamEvent>,
    ) {
        let num_frames = buffer.len() / 2;
        let last_frame = num_frames.saturating_sub(1) as i32;
        for event in param_events.iter_mut() {
            event.sample_offset = event.sample_offset.clamp(0, last_frame);
        }

        // Deinterleave input into the declared input buses. Bus 0 is the main
        // input; the selected sidechain bus receives the sidechain audio and
        // every other declared bus stays silent. The planar buffers are
        // allocated once and only rebuilt when the bus shape grows.
        self.input_layout
            .ensure(Self::input_bus_shape(&self.input_bus_channels), num_frames);
        self.output_layout.ensure_frames(num_frames);
        let sidechain_bus =
            sidechain.and_then(|_| self.sidechain_input_bus(self.sidechain_input_index));
        self.input_layout
            .write(buffer, sidechain, sidechain_bus, num_frames);

        let (out_l, out_r) = {
            let [left, right] = &mut self.output_layout.effect_outputs;
            left[..num_frames].fill(0.0);
            right[..num_frames].fill(0.0);
            (left.as_mut_ptr(), right.as_mut_ptr())
        };
        let mut out_ptrs: [*mut Sample32; 2] = [out_l, out_r];

        let mut output_bus = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: out_ptrs.as_mut_ptr(),
            },
        };

        let queues = self
            .process_queues
            .get_or_insert_with(Vst3ProcessQueues::new);
        queues.load_parameters(param_events);
        let input_parameter_changes = queues.parameters_ptr.as_ptr();
        let input_buses = self.input_layout.buses.as_mut_ptr();
        let input_bus_count = self.input_layout.buses.len() as i32;

        let mut process_data = ProcessData {
            processMode: 0,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: num_frames as int32,
            numInputs: input_bus_count,
            numOutputs: 1,
            inputs: input_buses,
            outputs: &mut output_bus,
            inputParameterChanges: input_parameter_changes,
            // Null is the documented opt-out: this path has no parameter
            // feedback consumer and no MIDI input to emit changes from.
            outputParameterChanges: ptr::null_mut(),
            inputEvents: ptr::null_mut(),
            outputEvents: ptr::null_mut(),
            processContext: ptr::null_mut(),
        };
        let process_context = transport.map(vst3_process_context);
        if let Some(context) = process_context.as_ref() {
            process_data.processContext = context as *const ProcessContext as *mut ProcessContext;
        }

        #[cfg(any(test, feature = "test-support"))]
        if let Some(fault) = self.injected_native_fault(FaultPoint::Process) {
            self.native_fault(Operation::Process, fault);
            buffer.fill(0.0);
            return;
        }
        // SAFETY: the instance retains its processor and the block's input/output buffers.
        match unsafe { vst3_guard::process(self.processor.as_ptr(), &mut process_data) } {
            Ok(result) if result == kResultOk => {}
            Ok(result) => {
                self.failure
                    .slot
                    .record(Vst3ProcessError::process_result(result));
                buffer.fill(0.0);
                return;
            }
            Err(fault) => {
                self.native_fault(Operation::Process, fault);
                buffer.fill(0.0);
                return;
            }
        }

        // Re-interleave output
        for i in 0..num_frames {
            buffer[i * 2] = self.output_layout.effect_outputs[0][i];
            buffer[i * 2 + 1] = self.output_layout.effect_outputs[1][i];
        }
    }

    /// Load the module and its factory, reporting why the loader refused it.
    ///
    /// The reason is the loader's own message, so a host that cannot load a
    /// bundle (a missing executable, a bundle with no slice for this
    /// architecture, a module that fails its entry call) says which one it is
    /// instead of a generic failure.
    fn load_factory(
        path: &Path,
        failure: &Arc<Vst3Failure>,
    ) -> Result<(Vst3ForeignRef<IPluginFactory>, *mut c_void), String> {
        module_loader::load_factory(path, failure)
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(scope) = &mut self.fixture_handler_guard {
            scope.deactivate();
        }
        // A poisoned instance must not re-enter plugin code, including Release.
        // ManuallyDrop keeps every foreign interface and callback owner resident.
        if self.failed() {
            return;
        }
        if self.processing {
            // SAFETY: the healthy processor reference is retained through this call.
            match unsafe { vst3_guard::processing(self.processor.as_ptr(), false) } {
                Ok(code) if code == kResultOk => self.processing = false,
                Ok(code) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::refused(Operation::SetProcessing, code));
                    return;
                }
                Err(fault) => {
                    self.native_fault(Operation::SetProcessing, fault);
                    return;
                }
            }
        }
        if self.active {
            // SAFETY: no previous native call failed and component is retained.
            match unsafe { vst3_guard::active(self.component.as_ptr(), false) } {
                Ok(code) if code == kResultOk => self.active = false,
                Ok(code) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::refused(Operation::SetActive, code));
                    return;
                }
                Err(fault) => {
                    self.native_fault(Operation::SetActive, fault);
                    return;
                }
            }
        }
        if let (Some(component), Some(controller)) =
            (&*self.component_connection, &*self.controller_connection)
        {
            for (pointer, peer) in [
                (controller.as_ptr(), component.as_ptr()),
                (component.as_ptr(), controller.as_ptr()),
            ] {
                // SAFETY: both retained connection points remain live and stopped.
                match unsafe { vst3_guard::disconnect(pointer.cast(), peer.cast()) } {
                    Ok(code) if code == kResultOk => {}
                    Ok(code) => {
                        self.failure
                            .slot
                            .record(Vst3ProcessError::refused(Operation::Disconnect, code));
                        return;
                    }
                    Err(fault) => {
                        self.native_fault(Operation::Disconnect, fault);
                        return;
                    }
                }
            }
        }
        if self.controller_is_separate
            && let Some(controller) = &*self.controller
        {
            // SAFETY: the retained separate controller is stopped and disconnected.
            match unsafe { vst3_guard::terminate(controller.as_ptr().cast()) } {
                Ok(code) if code == kResultOk => {}
                Ok(code) => {
                    self.failure.slot.record(Vst3ProcessError::refused(
                        Operation::ControllerTerminate,
                        code,
                    ));
                    return;
                }
                Err(fault) => {
                    self.native_fault(Operation::ControllerTerminate, fault);
                    return;
                }
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.fault_injection == Some(FaultPoint::Terminate) {
            self.note_failure("terminate threw: the fake plugin threw in terminate".into());
            return;
        }
        // SAFETY: the owned component is stopped, deactivated and disconnected.
        match unsafe { vst3_guard::terminate(self.component.as_ptr().cast()) } {
            Ok(code) if code == kResultOk => {}
            Ok(code) => {
                self.failure.slot.record(Vst3ProcessError::refused(
                    Operation::ComponentTerminate,
                    code,
                ));
                return;
            }
            Err(fault) => {
                self.native_fault(Operation::ComponentTerminate, fault);
                return;
            }
        }
        // SAFETY: all lifecycle calls succeeded. Each ManuallyDrop field is
        // released exactly once, while host services and buffers remain live.
        unsafe {
            ManuallyDrop::drop(&mut self.component_connection);
            ManuallyDrop::drop(&mut self.controller_connection);
            ManuallyDrop::drop(&mut self.controller);
            ManuallyDrop::drop(&mut self.processor);
            ManuallyDrop::drop(&mut self.component);
            if self.failed() {
                return;
            }
            #[cfg(target_os = "macos")]
            ManuallyDrop::drop(&mut self.plug_frame);
            ManuallyDrop::drop(&mut self.process_queues);
            ManuallyDrop::drop(&mut self.input_layout);
            ManuallyDrop::drop(&mut self.output_layout);
            ManuallyDrop::drop(&mut self._host);
        }
    }
}

fn vst3_process_context(transport: PluginTransport) -> ProcessContext {
    let quarter_notes = transport.quarter_notes();
    let (numerator, denominator, bar_position, _) = transport.musical_position();
    let mut state = StatesAndFlags_::kProjectTimeMusicValid
        | StatesAndFlags_::kBarPositionValid
        | StatesAndFlags_::kTempoValid
        | StatesAndFlags_::kTimeSigValid
        | StatesAndFlags_::kContTimeValid;
    if transport.playing {
        state |= StatesAndFlags_::kPlaying;
    }

    // The bindings use signed enum constants on Windows; the ABI field is uint32.
    #[cfg(target_os = "windows")]
    let state = state as u32;
    ProcessContext {
        state,
        sampleRate: transport.sample_rate as f64,
        projectTimeSamples: transport.sample_pos as i64,
        systemTime: 0,
        continousTimeSamples: transport.sample_pos as i64,
        projectTimeMusic: quarter_notes,
        barPositionMusic: bar_position,
        cycleStartMusic: 0.0,
        cycleEndMusic: 0.0,
        tempo: transport.current_tempo(),
        timeSigNumerator: numerator as int32,
        timeSigDenominator: denominator as int32,
        chord: Chord {
            keyNote: 0,
            rootNote: 0,
            chordMask: 0,
        },
        smpteOffsetSubframes: 0,
        frameRate: FrameRate {
            framesPerSecond: 0,
            flags: 0,
        },
        samplesToNextClock: 0,
    }
}

impl AudioEffect for Vst3Instance {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_effect(buffer, sample_rate);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_effect_with_transport(buffer, sample_rate, transport);
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_effect_with_sidechain(buffer, Some(sidechain), sample_rate, transport);
    }
    fn supports_sidechain(&self) -> bool {
        self.has_sidechain_input()
    }
    fn set_param(&mut self, name: &str, value: f64) {
        self.set_param_by_name(name, value);
    }
    fn queue_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        self.queue_param_plain_by_name(name, value, sample_offset as i32)
    }
    fn supports_param_offsets(&self) -> bool {
        true
    }
    fn max_block_frames(&self) -> u32 {
        self.max_frames.max(1) as u32
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.get_param_by_name(name)
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        self.processor_latency_samples()
    }
    fn tail_samples(&self) -> u32 {
        Vst3Instance::tail_samples(self)
    }
    fn plugin_tail_samples(&self) -> Option<u32> {
        Vst3Instance::plugin_tail_samples(self)
    }
    fn name(&self) -> &str {
        "VST3 Plugin"
    }
}

/// Split queued parameter events into one block span and rebase their offsets.
fn take_param_events_for_span(
    pending: &mut Vec<Vst3ParamEvent>,
    start: i32,
    frames: i32,
) -> Vec<Vst3ParamEvent> {
    let end = start + frames;
    let (current, remaining): (Vec<_>, Vec<_>) = std::mem::take(pending)
        .into_iter()
        .partition(|event| event.sample_offset >= start && event.sample_offset < end);
    *pending = remaining;
    current
        .into_iter()
        .map(|mut event| {
            event.sample_offset -= start;
            event
        })
        .collect()
}

fn append_output_pairs(
    channels: &[Vec<f32>],
    output_pairs: &[usize],
    num_frames: usize,
    outputs: &mut [Vec<f32>],
) {
    for (output, &pair) in outputs.iter_mut().zip(output_pairs) {
        let left = channels.get(pair.saturating_mul(2));
        let right = channels.get(pair.saturating_mul(2).saturating_add(1));
        output.reserve(num_frames.saturating_mul(2));
        for index in 0..num_frames {
            output.push(left.map_or(0.0, |channel| channel[index]));
            output.push(right.map_or(0.0, |channel| channel[index]));
        }
    }
}

// ============================================================================
// Simple IBStream implementation for VST3 state chunks
// ============================================================================

struct Vst3StateStream {
    data: Rc<RefCell<Vec<u8>>>,
    position: Cell<usize>,
}

impl Vst3StateStream {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data: Rc::new(RefCell::new(data)),
            position: Cell::new(0),
        }
    }
}

impl Class for Vst3StateStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Vst3StateStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> vst3::Steinberg::tresult {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }

        let data = self.data.borrow();
        let position = self.position.get().min(data.len());
        let count = (num_bytes as usize).min(data.len().saturating_sub(position));
        if count > 0 {
            // SAFETY: The SDK caller supplies writable nonoverlapping storage for num_bytes; count is bounded by that and this private vector suffix.
            unsafe {
                ptr::copy_nonoverlapping(data[position..].as_ptr(), buffer as *mut u8, count);
            }
        }
        self.position.set(position + count);
        if !num_bytes_read.is_null() {
            // SAFETY: The optional nonnull SDK output points to writable aligned int32 storage; count cannot exceed nonnegative num_bytes.
            unsafe { ptr::write(num_bytes_read, count as int32) };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> vst3::Steinberg::tresult {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }

        let count = num_bytes as usize;
        let position = self.position.get();
        let mut data = self.data.borrow_mut();
        let Some(end) = position.checked_add(count) else {
            return -1;
        };
        if data.len() < end {
            data.resize(end, 0);
        }
        if count > 0 {
            // SAFETY: The SDK input contains count readable bytes, distinct from private storage; checked end and resize cover the destination.
            unsafe {
                ptr::copy_nonoverlapping(buffer as *const u8, data[position..].as_mut_ptr(), count);
            }
        }
        self.position.set(position + count);
        if !num_bytes_written.is_null() {
            // SAFETY: The optional nonnull SDK output is writable aligned int32 storage, and count came from nonnegative num_bytes.
            unsafe { ptr::write(num_bytes_written, count as int32) };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> vst3::Steinberg::tresult {
        let len = self.data.borrow().len() as int64;
        let base = match mode {
            mode if mode == IStreamSeekMode_::kIBSeekSet as int32 => 0,
            mode if mode == IStreamSeekMode_::kIBSeekCur as int32 => self.position.get() as int64,
            mode if mode == IStreamSeekMode_::kIBSeekEnd as int32 => len,
            _ => return -1,
        };
        let Some(new_position) = base.checked_add(pos) else {
            return -1;
        };
        if new_position < 0 {
            return -1;
        }

        self.position.set(new_position as usize);
        if !result.is_null() {
            // SAFETY: The optional nonnull SDK result is writable aligned int64 storage for the checked new position.
            unsafe { ptr::write(result, new_position) };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> vst3::Steinberg::tresult {
        if !pos.is_null() {
            // SAFETY: The nonnull optional SDK output points to writable aligned int64 storage for this scalar position.
            unsafe { ptr::write(pos, self.position.get() as int64) };
        }
        kResultOk
    }
}

// ============================================================================
// Simple IEventList implementation for passing events to VST3 process()
// ============================================================================

struct SimpleEventList {
    events: RefCell<Vec<Event>>,
}

impl Class for SimpleEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for SimpleEventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.borrow().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> vst3::Steinberg::tresult {
        let events = self.events.borrow();
        if e.is_null() || index < 0 || index as usize >= events.len() {
            return -1;
        }
        // SAFETY: The SDK caller supplies writable aligned Event storage; the checked index selects a live retained event copied by value.
        unsafe { ptr::write(e, events[index as usize]) };
        kResultOk
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> vst3::Steinberg::tresult {
        kResultOk // output events — ignore
    }
}

// ============================================================================
// Simple IParameterChanges/IParamValueQueue implementation for automation
// ============================================================================

struct SimpleParameterChanges {
    queues: RefCell<Vec<ComPtr<IParamValueQueue>>>,
}

impl SimpleParameterChanges {
    fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            queues: RefCell::new(Vec::new()),
        })
    }
}

impl Class for SimpleParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for SimpleParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.queues.borrow().len() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let queues = self.queues.borrow();
        if index < 0 || index as usize >= queues.len() {
            return ptr::null_mut();
        }
        queues[index as usize].as_ptr()
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if !index.is_null() {
            // SAFETY: The nonnull optional SDK index output is writable aligned int32 storage; no queue reference is returned.
            unsafe { ptr::write(index, -1) };
        }
        ptr::null_mut()
    }
}

struct SimpleParamValueQueue {
    id: ParamID,
    points: RefCell<Vec<(int32, ParamValue, usize)>>,
}

impl Class for SimpleParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for SimpleParamValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points.borrow().len() as int32
    }

    unsafe fn getPoint(
        &self,
        index: int32,
        sample_offset: *mut int32,
        value: *mut ParamValue,
    ) -> vst3::Steinberg::tresult {
        let points = self.points.borrow();
        if index < 0 || index as usize >= points.len() {
            return -1;
        }
        let (point_offset, point_value, _) = points[index as usize];
        if !sample_offset.is_null() {
            // SAFETY: The nonnull optional SDK offset output is writable aligned int32 storage; the point index was checked above.
            unsafe { ptr::write(sample_offset, point_offset) };
        }
        if !value.is_null() {
            // SAFETY: The nonnull optional SDK value output is writable aligned ParamValue storage; the selected point is copied by value.
            unsafe { ptr::write(value, point_value) };
        }
        kResultOk
    }

    unsafe fn addPoint(
        &self,
        _sample_offset: int32,
        _value: ParamValue,
        index: *mut int32,
    ) -> vst3::Steinberg::tresult {
        if !index.is_null() {
            // SAFETY: The optional nonnull SDK index output is writable aligned int32 storage for the refusal sentinel.
            unsafe { ptr::write(index, -1) };
        }
        -1
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Vst3PackingWork {
    queue_resets: usize,
    events: usize,
    lookups: usize,
    comparisons: usize,
    queue_visits: usize,
    sort_key_reads: usize,
    discovered: usize,
}
#[cfg(test)]
#[path = "vst3_instance/queue_packing_tests.rs"]
mod queue_packing_tests;

/// Retained process queues preserve descriptor/first-seen queue ordering.
/// Prepared instances build their index and point storage before processing.
/// Legacy discovery may allocate when a genuinely new native ID first appears.
struct Vst3ProcessQueues {
    events: ComWrapper<SimpleEventList>,
    events_ptr: ComPtr<IEventList>,
    parameters: ComWrapper<SimpleParameterChanges>,
    parameters_ptr: ComPtr<IParameterChanges>,
    value_queues: Vec<ComWrapper<SimpleParamValueQueue>>,
    queue_ids: Vec<ParamID>,
    queue_index: Vec<(ParamID, usize)>,
    packing_work: Vst3PackingWork,
}

impl Vst3ProcessQueues {
    fn new() -> Self {
        let events = ComWrapper::new(SimpleEventList {
            events: RefCell::new(Vec::new()),
        });
        let events_ptr = events.to_com_ptr::<IEventList>().unwrap();
        let parameters = SimpleParameterChanges::new();
        let parameters_ptr = parameters.to_com_ptr::<IParameterChanges>().unwrap();
        Self {
            events,
            events_ptr,
            parameters,
            parameters_ptr,
            value_queues: Vec::new(),
            queue_ids: Vec::new(),
            queue_index: Vec::new(),
            packing_work: Vst3PackingWork::default(),
        }
    }

    fn prepare_parameter_index(&mut self) {
        self.queue_index = self
            .queue_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        self.queue_index.sort_unstable_by_key(|(id, _)| *id);
    }
    fn parameter_slot(&mut self, id: ParamID) -> Result<usize, usize> {
        self.packing_work.lookups += 1;
        let (mut low, mut high) = (0, self.queue_index.len());
        while low < high {
            self.packing_work.comparisons += 1;
            let middle = low + (high - low) / 2;
            match self.queue_index[middle].0.cmp(&id) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(self.queue_index[middle].1),
            }
        }
        Err(low)
    }

    /// Move the block's events into the reusable event list.
    fn load_events(&self, events: &mut Vec<Event>) {
        let mut slot = self.events.events.borrow_mut();
        slot.clear();
        slot.append(events);
    }

    /// Group the block's parameter events into the reusable value queues.
    fn load_parameters(&mut self, events: &mut Vec<Vst3ParamEvent>) {
        self.packing_work = Vst3PackingWork::default();
        for queue in &self.value_queues {
            queue.points.borrow_mut().clear();
            self.packing_work.queue_resets += 1;
        }
        for (ordinal, event) in events.drain(..).enumerate() {
            self.packing_work.events += 1;
            let index = match self.parameter_slot(event.id) {
                Ok(index) => index,
                Err(position) => {
                    self.queue_ids.push(event.id);
                    self.value_queues
                        .push(ComWrapper::new(SimpleParamValueQueue {
                            id: event.id,
                            points: RefCell::new(Vec::new()),
                        }));
                    let index = self.value_queues.len() - 1;
                    self.queue_index.insert(position, (event.id, index));
                    self.packing_work.discovered += 1;
                    index
                }
            };
            self.value_queues[index].points.borrow_mut().push((
                event.sample_offset,
                event.value,
                ordinal,
            ));
        }
        let mut queues = self.parameters.queues.borrow_mut();
        queues.clear();
        for queue in &self.value_queues {
            self.packing_work.queue_visits += 1;
            let points = queue.points.borrow();
            if points.is_empty() {
                continue;
            }
            drop(points);
            // Unique source ordinals give equal-time points a total order.
            // The unstable algorithm stays allocation-free while preserving
            // authored/MIDI winner semantics at the native queue boundary.
            queue
                .points
                .borrow_mut()
                .sort_unstable_by_key(|(sample_offset, _, ordinal)| {
                    self.packing_work.sort_key_reads += 1;
                    (*sample_offset, *ordinal)
                });
            queues.push(queue.to_com_ptr::<IParamValueQueue>().unwrap());
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn note_event_to_vst3(n: Vst3NoteEvent) -> Event {
    // SAFETY: SDK Event uses zero-valid scalars and a raw-pointer union; its selected note payload is initialized before publication.
    let mut event: Event = unsafe { std::mem::zeroed() };
    event.busIndex = 0;
    event.sampleOffset = n.sample_offset;
    event.flags = 0;
    if n.on {
        event.r#type = EventTypes_::kNoteOnEvent as u16;
        event.__field0 = Event__type0 {
            noteOn: NoteOnEvent {
                channel: n.channel,
                pitch: n.key,
                tuning: 0.0,
                velocity: n.velocity,
                length: -1,
                noteId: n.note_id,
            },
        };
    } else {
        event.r#type = EventTypes_::kNoteOffEvent as u16;
        event.__field0 = Event__type0 {
            noteOff: NoteOffEvent {
                channel: n.channel,
                pitch: n.key,
                velocity: n.velocity,
                noteId: n.note_id,
                tuning: 0.0,
            },
        };
    }
    event
}

fn poly_pressure_event_to_vst3(p: Vst3PolyPressureEvent) -> Event {
    // SAFETY: SDK Event has zero-valid scalar/union storage; the poly-pressure payload and discriminator are assigned before use.
    let mut event: Event = unsafe { std::mem::zeroed() };
    event.busIndex = 0;
    event.sampleOffset = p.sample_offset;
    event.flags = 0;
    event.r#type = EventTypes_::kPolyPressureEvent as u16;
    event.__field0 = Event__type0 {
        polyPressure: PolyPressureEvent {
            channel: p.channel,
            pitch: p.key,
            pressure: p.pressure,
            noteId: p.note_id,
        },
    };
    event
}

fn note_expression_event_to_vst3(e: Vst3NoteExpressionEvent) -> Event {
    // SAFETY: SDK Event uses zero-valid scalar/union storage; its expression payload and discriminator are assigned before use.
    let mut event: Event = unsafe { std::mem::zeroed() };
    event.busIndex = 0;
    event.sampleOffset = e.sample_offset;
    event.flags = 0;
    event.r#type = EventTypes_::kNoteExpressionValueEvent as u16;
    event.__field0 = Event__type0 {
        noteExpressionValue: NoteExpressionValueEvent {
            typeId: e.type_id,
            noteId: e.note_id,
            value: e.value,
        },
    };
    event
}

fn char16_to_string(chars: &[char16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    let u16s: Vec<u16> = chars[..end].to_vec();
    String::from_utf16_lossy(&u16s)
}

fn normalize_param_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn parameter_can_automate(flags: i32) -> bool {
    let permission = ParameterFlags_::kCanAutomate | ParameterFlags_::kIsReadOnly;
    flags & permission == ParameterFlags_::kCanAutomate
}

fn mapped_midi_normalized(controller: i16, value: i32, steps: Option<i32>) -> Option<f64> {
    match controller {
        0..=128 if (0..=127).contains(&value) => Some(f64::from(value) / 127.0),
        129 if (-8192..=8191).contains(&value) => Some(f64::from(value + 8192) / 16383.0),
        130 if (0..=127).contains(&value) => {
            let steps = steps?;
            if steps == 0 && value == 0 {
                Some(0.0)
            } else if steps > 0 && value <= steps {
                Some(f64::from(value) / f64::from(steps))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// In-process fake VST3 component used by host and render tests.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

#[cfg(test)]
mod tests;
