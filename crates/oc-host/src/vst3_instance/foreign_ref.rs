//! Foreign COM ownership with native refcount barriers and shared quarantine.
use super::*;
use std::ptr::NonNull;

/// An owned foreign COM reference. Cloning and interface queries are fallible.
/// Generated non-refcount interface methods remain separate audit boundaries.
pub struct Vst3ForeignRef<I: Interface> {
    raw: NonNull<I>,
    pub(super) failure: Arc<Vst3Failure>,
    keepalive: Option<Arc<HostBacking>>,
}
impl<I: Interface> vst3::com_scrape_types::SmartPtr for Vst3ForeignRef<I> {
    type Target = I;
    fn ptr(&self) -> *mut I {
        self.as_ptr()
    }
}
impl<I: Interface> Vst3ForeignRef<I> {
    pub(super) fn adopt(reference: ComPtr<I>, failure: Arc<Vst3Failure>) -> Self {
        // into_raw transfers exactly one owned reference and calls no native method.
        Self {
            raw: NonNull::new(reference.into_raw()).expect("owned COM reference"),
            failure,
            keepalive: None,
        }
    }
    pub(super) fn with_backing(mut self, backing: Arc<HostBacking>) -> Self {
        self.keepalive = Some(backing);
        self
    }
    pub(super) fn with_parent<J: Interface>(mut self, parent: &Vst3ForeignRef<J>) -> Self {
        self.keepalive = parent.keepalive.clone();
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn set_backing(&mut self, backing: Arc<HostBacking>) {
        self.keepalive = Some(backing);
    }
    /// Raw interface pointer retained by this reference.
    pub fn as_ptr(&self) -> *mut I {
        self.raw.as_ptr()
    }
    #[expect(
        clippy::result_large_err,
        reason = "native causes stay inline without audio-path allocation"
    )]
    fn healthy(&self) -> Result<(), Vst3ProcessError> {
        if self.failure.failed() {
            Err(self.failure.first_fault().unwrap_or_else(|| {
                Vst3ProcessError::native(Operation::Legacy, Fault::quarantined())
            }))
        } else {
            Ok(())
        }
    }
    pub(super) fn fault(&self, operation: Operation, fault: Fault) -> Vst3ProcessError {
        let error = Vst3ProcessError::native(operation, fault);
        self.failure.slot.record(error);
        self.failure.first_fault().unwrap_or(error)
    }
    #[expect(
        clippy::result_large_err,
        reason = "native causes stay inline without audio-path allocation"
    )]
    /// Add a native reference and return a second owner.
    pub fn try_clone(&self) -> Result<Self, Vst3ProcessError> {
        self.healthy()?;
        // SAFETY: this owner retains the live interface across the native addRef call.
        unsafe { vst3_guard::add_ref(self.as_ptr().cast()) }
            .map_err(|fault| self.fault(Operation::AddRef, fault))?;
        Ok(Self {
            raw: self.raw,
            failure: Arc::clone(&self.failure),
            keepalive: self.keepalive.clone(),
        })
    }
    #[expect(
        clippy::result_large_err,
        reason = "native causes stay inline without audio-path allocation"
    )]
    pub(super) fn try_query<J: Interface>(
        &self,
    ) -> Result<Option<Vst3ForeignRef<J>>, Vst3ProcessError> {
        self.healthy()?;
        let mut output = ptr::null_mut();
        // SAFETY: the owned interface, requested static IID and writable output
        // slot all outlive the synchronous native queryInterface invocation.
        let code =
            unsafe { vst3_guard::query(self.as_ptr().cast(), J::IID.as_ptr().cast(), &mut output) }
                .map_err(|fault| self.fault(Operation::QueryInterface, fault))?;
        if code != kResultOk {
            if !output.is_null() {
                return Err(self.fault(
                    Operation::QueryInterface,
                    Fault::host_contract("queryInterface refused with a non-null output"),
                ));
            }
            // Keep the existing optional-interface semantics for plugin refusals.
            return Ok(None);
        }
        let Some(raw) = NonNull::new(output.cast::<J>()) else {
            return Err(self.fault(
                Operation::QueryInterface,
                Fault::host_contract("queryInterface succeeded with a null output"),
            ));
        };
        Ok(Some(Vst3ForeignRef {
            raw,
            failure: Arc::clone(&self.failure),
            keepalive: self.keepalive.clone(),
        }))
    }
    pub(super) fn cast<J: Interface>(&self) -> Option<Vst3ForeignRef<J>> {
        self.try_query().ok().flatten()
    }
}
impl<I: Interface> Drop for Vst3ForeignRef<I> {
    fn drop(&mut self) {
        if self.failure.failed() {
            return;
        }
        // SAFETY: this field owns exactly one live native reference. Never retry
        // after an exception: release may already have mutated/deleted the object.
        if let Err(fault) = unsafe { vst3_guard::release(self.as_ptr().cast()) } {
            self.fault(Operation::Release, fault);
        }
    }
}

/// Reverse only lifecycle phases that completed successfully. Native failures
/// poison the authority and prevent all remaining native cleanup/refcount calls.
pub(super) struct InitializationGuard {
    pub component: *mut IComponent,
    pub controller: *mut IEditController,
    pub component_initialized: bool,
    pub controller_initialized: bool,
    pub armed: bool,
    pub component_connection: *mut IConnectionPoint,
    pub controller_connection: *mut IConnectionPoint,
    pub component_connected: bool,
    pub controller_connected: bool,
    pub failure: Arc<Vst3Failure>,
}
impl InitializationGuard {
    /// Guard initialization of a component and controller.
    pub fn new(component: *mut IComponent, failure: Arc<Vst3Failure>) -> Self {
        Self {
            component,
            controller: ptr::null_mut(),
            component_initialized: false,
            controller_initialized: false,
            armed: true,
            component_connection: ptr::null_mut(),
            controller_connection: ptr::null_mut(),
            component_connected: false,
            controller_connected: false,
            failure,
        }
    }
    /// Release whatever initialization acquired.
    pub fn cleanup(&mut self) {
        if !self.armed || self.failure.failed() {
            return;
        }
        self.armed = false;
        for (pointer, peer, connected) in [
            (
                self.controller_connection,
                self.component_connection,
                self.controller_connected,
            ),
            (
                self.component_connection,
                self.controller_connection,
                self.component_connected,
            ),
        ] {
            if !connected {
                continue;
            }
            // SAFETY: both acquired connection refs outlive explicit reverse cleanup.
            match unsafe { vst3_guard::disconnect(pointer.cast(), peer.cast()) } {
                Ok(code) if code == kResultOk => {}
                Ok(code) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::refused(Operation::Disconnect, code));
                    return;
                }
                Err(fault) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::native(Operation::Disconnect, fault));
                    return;
                }
            }
        }
        for (pointer, initialized, operation) in [
            (
                self.controller.cast::<c_void>(),
                self.controller_initialized,
                Operation::ControllerTerminate,
            ),
            (
                self.component.cast::<c_void>(),
                self.component_initialized,
                Operation::ComponentTerminate,
            ),
        ] {
            if !initialized {
                continue;
            }
            // SAFETY: the guard is declared after the owned references, so the
            // initialized interface remains live until this reverse cleanup ends.
            match unsafe { vst3_guard::terminate(pointer) } {
                Ok(code) if code == kResultOk => {}
                Ok(code) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::refused(operation, code));
                    return;
                }
                Err(fault) => {
                    self.failure
                        .slot
                        .record(Vst3ProcessError::native(operation, fault));
                    return;
                }
            }
        }
    }
    /// Clean up and merge a cleanup failure into `primary`.
    pub fn error(&mut self, primary: String) -> String {
        self.cleanup();
        match self.failure.message() {
            Some(cleanup) => format!("{primary}; cleanup failed: {cleanup}"),
            None => primary,
        }
    }
}
impl Drop for InitializationGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Callback backing must remain live when any foreign owner is quarantined.
pub(super) struct HostBacking {
    pub host: ManuallyDrop<Option<ComWrapper<Vst3Host>>>,
    pub failure: Arc<Vst3Failure>,
}
impl Drop for HostBacking {
    fn drop(&mut self) {
        if !self.failure.failed() {
            // SAFETY: all foreign refs declared after this backing were retired
            // without a native failure; this releases the owned host exactly once.
            unsafe { ManuallyDrop::drop(&mut self.host) };
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) struct HostFrame {
    pub frame: ManuallyDrop<ComPtr<IPlugFrame>>,
    pub failure: Arc<Vst3Failure>,
}
#[cfg(target_os = "macos")]
impl std::ops::Deref for HostFrame {
    type Target = ComPtr<IPlugFrame>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
#[cfg(target_os = "macos")]
impl Drop for HostFrame {
    fn drop(&mut self) {
        if !self.failure.failed() {
            // SAFETY: this is the host's own frame reference, released exactly once
            // only after foreign ownership retired without a native failure.
            unsafe { ManuallyDrop::drop(&mut self.frame) };
        }
    }
}

/// A transferable editor-controller token. Its last drop queues retirement;
/// only the AppKit main thread may execute the actual native release.
#[cfg(target_os = "macos")]
pub(super) struct EditorControllerLease {
    record: Option<EditorRetirement>,
    pub failure: Arc<Vst3Failure>,
}
#[cfg(target_os = "macos")]
struct EditorRetirement {
    pointer: usize,
    failure: Arc<Vst3Failure>,
    keepalive: Option<Arc<HostBacking>>,
}
#[cfg(target_os = "macos")]
static EDITOR_RETIREMENTS: Mutex<Vec<EditorRetirement>> = Mutex::new(Vec::new());
#[cfg(target_os = "macos")]
impl EditorControllerLease {
    /// Take ownership of an editor controller reference.
    pub fn new(reference: Vst3ForeignRef<IEditController>) -> Self {
        let reference = ManuallyDrop::new(reference);
        // SAFETY: ownership of these two Rust fields moves out exactly once;
        // the ManuallyDrop source never runs its native reference destructor.
        let (failure, keepalive) = unsafe {
            (
                ptr::read(&reference.failure),
                ptr::read(&reference.keepalive),
            )
        };
        Self {
            record: Some(EditorRetirement {
                pointer: reference.raw.as_ptr() as usize,
                failure: Arc::clone(&failure),
                keepalive,
            }),
            failure,
        }
    }
    fn as_ptr(&self) -> *mut IEditController {
        self.record.as_ref().expect("live editor lease").pointer as *mut IEditController
    }
}
#[cfg(target_os = "macos")]
impl vst3::com_scrape_types::SmartPtr for EditorControllerLease {
    type Target = IEditController;
    fn ptr(&self) -> *mut IEditController {
        self.as_ptr()
    }
}
#[cfg(target_os = "macos")]
fn retire_editor(record: EditorRetirement) {
    debug_assert!(is_main_thread());
    if record.failure.failed() {
        std::mem::forget(record);
        return;
    }
    // SAFETY: the record owns one controller reference; this function executes
    // only on AppKit main, and its backing remains retained through release.
    if let Err(fault) = unsafe { vst3_guard::release(record.pointer as *mut c_void) } {
        record
            .failure
            .slot
            .record(Vst3ProcessError::native(Operation::Release, fault));
        std::mem::forget(record);
    }
}
#[cfg(target_os = "macos")]
impl Drop for EditorControllerLease {
    fn drop(&mut self) {
        let Some(record) = self.record.take() else {
            return;
        };
        if is_main_thread() {
            retire_editor(record);
        } else {
            match EDITOR_RETIREMENTS.lock() {
                Ok(mut pending) => pending.push(record),
                Err(_) => std::mem::forget(record),
            }
        }
    }
}
#[cfg(target_os = "macos")]
pub(super) fn drain_editor_retirements() -> Result<(), String> {
    if !is_main_thread() {
        return Err("editor retirement requires the AppKit main thread".into());
    }
    let pending = {
        let mut pending = EDITOR_RETIREMENTS
            .lock()
            .map_err(|_| "editor retirement queue is unavailable")?;
        std::mem::take(&mut *pending)
    };
    for record in pending {
        retire_editor(record);
    }
    Ok(())
}
#[cfg(target_os = "macos")]
impl<I: Interface> Vst3ForeignRef<I> {
    pub(super) fn with_editor_parent(mut self, parent: &EditorControllerLease) -> Self {
        self.keepalive = parent
            .record
            .as_ref()
            .and_then(|record| record.keepalive.clone());
        self
    }
}
