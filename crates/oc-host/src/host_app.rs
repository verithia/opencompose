//! Host application and component handler services for a VST3 instance.
//!
//! Some editors create `IMessage` objects through
//! `IHostApplication::createInstance` while they build their view. A host that
//! answers `kNotImplemented` hands the plugin null messages and the editor can
//! crash during `attached`. This object supplies messages, attributes, the
//! component-handler callbacks (edits, restart requests, dirty state, editor
//! requests) and the plug-interface query.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::Vst::{
    IAttributeList, IComponentHandler, IComponentHandler2, IComponentHandler2Trait,
    IComponentHandler3, IComponentHandler3Trait, IComponentHandlerTrait, IContextMenu,
    IEditController, IEditControllerTrait, IHostApplication, IHostApplicationTrait, IMessage,
    IPlugInterfaceSupport, IPlugInterfaceSupportTrait, ParamID, String128,
};
use vst3::Steinberg::{
    FIDString, TUID, char16, kInvalidArgument, kNotImplemented, kResultOk, tresult,
};
use vst3::{Class, ComRef, ComWrapper, Interface};

use crate::param_edit::ParamEditBus;

/// Host services for one plugin instance.
pub struct Vst3Host {
    edits: Arc<EditSink>,
}

impl Vst3Host {
    /// Create the host callbacks component.
    pub fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            edits: Arc::new(EditSink::default()),
        })
    }

    /// The editor-edit endpoint this instance's controller reports to.
    pub fn edit_sink(&self) -> &Arc<EditSink> {
        &self.edits
    }
}

/// Routes `IComponentHandler` edits to the preset's live processors.
///
/// The editor calls this on the UI thread; a socket forwarder handles the
/// out-of-process editor. Processors only ever read the bus.
#[derive(Default)]
pub struct EditSink {
    bus: Mutex<Option<Arc<ParamEditBus>>>,
    controller: AtomicPtr<IEditController>,
    /// Last value each parameter performed, used when a drag ends.
    performed: Mutex<HashMap<ParamID, f64>>,
    /// Union of the `restartComponent` flags the plugin reported and the host
    /// has not acted on yet. Written from any thread, drained by the owner.
    restart: AtomicI32,
    /// Non-consuming authority for prepared static parameter bindings.
    parameter_inventory_revision: AtomicU64,
    /// Set while the plugin has unsaved changes.
    dirty: AtomicBool,
    /// Editor name a plugin asked the host to open.
    open_editor: Mutex<Option<String>>,
    #[cfg(unix)]
    socket: Mutex<Option<crate::param_edit::EditSocket>>,
}

impl EditSink {
    /// Report editor edits into a parameter edit bus.
    pub fn bind_bus(&self, bus: Arc<ParamEditBus>) {
        *self.bus.lock().unwrap_or_else(|error| error.into_inner()) = Some(bus);
    }

    /// # Safety
    /// `controller` must outlive this sink, which the owning instance guarantees.
    pub unsafe fn bind_controller(&self, controller: *mut IEditController) {
        self.controller.store(controller, Ordering::Release);
    }

    #[cfg(unix)]
    /// Report editor edits over a socket.
    pub fn bind_socket(&self, socket: crate::param_edit::EditSocket) {
        *self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(socket);
    }

    fn bus(&self) -> Option<Arc<ParamEditBus>> {
        self.bus
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Record the flags of one `IComponentHandler::restartComponent` call.
    ///
    /// The plugin may call this from the audio thread, so the flags accumulate
    /// atomically and the owning instance drains them on its own thread.
    pub fn restart(&self, flags: i32) {
        if flags != 0 {
            self.restart.fetch_or(flags, Ordering::AcqRel);
            use vst3::Steinberg::Vst::RestartFlags_;
            // Prepared bindings retain bus layouts and latency. Either restart
            // expires processing admission without querying metadata on the DSP path.
            let inventory_flags = RestartFlags_::kParamTitlesChanged
                | RestartFlags_::kReloadComponent
                | RestartFlags_::kIoChanged
                | RestartFlags_::kLatencyChanged
                | RestartFlags_::kRoutingInfoChanged
                | RestartFlags_::kMidiCCAssignmentChanged
                | RestartFlags_::kNoteExpressionChanged;
            if flags & inventory_flags != 0 {
                self.parameter_inventory_revision
                    .fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Observe inventory invalidation without consuming the pending UI events.
    pub fn parameter_inventory_revision(&self) -> u64 {
        self.parameter_inventory_revision.load(Ordering::Acquire)
    }

    /// Take the restart flags accumulated since the last call.
    pub fn take_restart(&self) -> i32 {
        self.restart.swap(0, Ordering::AcqRel)
    }

    /// Record that the plugin changed the document behind the host's back.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Take the pending dirty flag, reporting whether it was set.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// Record an editor the plugin asked the host to open.
    pub fn request_editor(&self, name: &str) {
        *self
            .open_editor
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(name.to_owned());
    }

    /// Take the editor a plugin asked the host to open.
    pub fn take_editor_request(&self) -> Option<String> {
        self.open_editor
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// The controller's current value, which a gesture boundary publishes.
    fn current(&self, id: ParamID) -> Option<f64> {
        let controller = self.controller.load(Ordering::Acquire);
        if controller.is_null() {
            return None;
        }
        // SAFETY: the owning instance keeps the controller alive for the sink.
        let controller = unsafe { ComRef::<IEditController>::from_raw_unchecked(controller) };
        // SAFETY: The registered controller remains borrowed for this synchronous component-handler callback; this getter lends no buffers.
        Some(unsafe { controller.getParamNormalized(id) })
    }

    /// The entry point `IComponentHandler::performEdit` calls.
    pub fn perform(&self, id: ParamID, value: f64) {
        self.performed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id, value);
        self.publish_only(id, value);
    }

    /// Publish a value without recording it as a performed edit.
    fn publish_only(&self, id: ParamID, value: f64) {
        if let Some(bus) = self.bus() {
            bus.publish(id, value);
        }
        #[cfg(unix)]
        if let Ok(socket) = self.socket.lock()
            && let Some(socket) = socket.as_ref()
        {
            socket.publish(id, value);
        }
    }

    /// The entry point `IComponentHandler::beginEdit` calls.
    pub fn begin(&self, id: Option<ParamID>) {
        if let Some(id) = id {
            self.performed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            if let Some(value) = self.current(id) {
                self.publish_only(id, value);
            }
        }
        if let Some(bus) = self.bus() {
            bus.begin_gesture();
        }
        #[cfg(unix)]
        if let Some(id) = id
            && let Ok(socket) = self.socket.lock()
            && let Some(socket) = socket.as_ref()
        {
            socket.begin(id);
        }
    }

    /// The entry point `IComponentHandler::endEdit` calls.
    pub fn end(&self, id: Option<ParamID>) {
        if let Some(id) = id {
            let performed = self
                .performed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            if let Some(value) = performed.or_else(|| self.current(id)) {
                self.publish_only(id, value);
            }
        }
        if let Some(bus) = self.bus() {
            bus.end_gesture();
        }
        #[cfg(unix)]
        if let Some(id) = id
            && let Ok(socket) = self.socket.lock()
            && let Some(socket) = socket.as_ref()
        {
            socket.end(id);
        }
    }
}

impl IHostApplicationTrait for Vst3Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let text = "OpenCompose";
        for (index, unit) in text.encode_utf16().take(127).enumerate() {
            // SAFETY: VST3 passes a writable 128-unit name buffer.
            unsafe { (*name)[index] = unit as char16 };
        }
        // SAFETY: `text` is shorter than the buffer.
        unsafe { (*name)[text.len().min(127)] = 0 };
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the caller passes two readable TUIDs and one writable pointer.
        let cid = unsafe { std::slice::from_raw_parts(cid as *const u8, 16) };
        // SAFETY: same contract as above.
        let iid = unsafe { std::slice::from_raw_parts(iid as *const u8, 16) };
        if cid == IMessage::IID && iid == IMessage::IID {
            // SAFETY: `obj` is writable and receives one owned reference.
            unsafe { *obj = crate::host_message::create_message() as *mut c_void };
            return kResultOk;
        }
        if cid == IAttributeList::IID || iid == IAttributeList::IID {
            // SAFETY: `obj` is writable and receives one owned reference.
            unsafe { *obj = crate::host_message::create_attribute_list() as *mut c_void };
            return kResultOk;
        }
        kNotImplemented
    }
}

impl IComponentHandlerTrait for Vst3Host {
    unsafe fn beginEdit(&self, id: u32) -> tresult {
        self.edits.begin(Some(id));
        kResultOk
    }

    unsafe fn performEdit(&self, id: u32, value: f64) -> tresult {
        self.edits.perform(id, value);
        kResultOk
    }

    unsafe fn endEdit(&self, id: u32) -> tresult {
        self.edits.end(Some(id));
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        self.edits.restart(flags);
        kResultOk
    }
}

impl IComponentHandler2Trait for Vst3Host {
    unsafe fn setDirty(&self, state: vst3::Steinberg::TBool) -> tresult {
        if state != 0 {
            self.edits.mark_dirty();
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, name: FIDString) -> tresult {
        // The spec allows a null name, which means the default editor.
        let name = if name.is_null() {
            "editor".to_string()
        } else {
            // SAFETY: the caller passes a NUL-terminated string for this call.
            unsafe { std::ffi::CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        self.edits.request_editor(&name);
        kResultOk
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        self.edits.begin(None);
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        self.edits.end(None);
        kResultOk
    }
}

impl IComponentHandler3Trait for Vst3Host {
    unsafe fn createContextMenu(
        &self,
        _view: *mut vst3::Steinberg::IPlugView,
        _param: *const ParamID,
    ) -> *mut IContextMenu {
        std::ptr::null_mut()
    }
}

impl IPlugInterfaceSupportTrait for Vst3Host {
    unsafe fn isPlugInterfaceSupported(&self, _iid: *const TUID) -> tresult {
        kResultOk
    }
}

impl Class for Vst3Host {
    type Interfaces = (
        IHostApplication,
        IComponentHandler,
        IComponentHandler2,
        IComponentHandler3,
        IPlugInterfaceSupport,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::ComPtr;

    #[test]
    fn parameter_inventory_revision_is_independent_of_restart_event_consumption() {
        use vst3::Steinberg::Vst::RestartFlags_;
        let sink = EditSink::default();
        let titles = RestartFlags_::kParamTitlesChanged;
        let values = RestartFlags_::kParamValuesChanged;
        let reload = RestartFlags_::kReloadComponent;
        assert_eq!(sink.parameter_inventory_revision(), 0);
        sink.restart(values);
        assert_eq!(sink.parameter_inventory_revision(), 0);
        sink.restart(titles);
        assert_eq!(sink.parameter_inventory_revision(), 1);
        assert_eq!(sink.take_restart(), titles | values);
        assert_eq!(sink.take_restart(), 0);
        assert_eq!(sink.parameter_inventory_revision(), 1);
        sink.restart(reload);
        assert_eq!(sink.parameter_inventory_revision(), 2);
        sink.restart(0);
        assert_eq!(sink.parameter_inventory_revision(), 2);
        assert_eq!(sink.take_restart(), reload);
        for (index, flags) in [
            RestartFlags_::kIoChanged,
            RestartFlags_::kLatencyChanged,
            RestartFlags_::kRoutingInfoChanged,
            RestartFlags_::kMidiCCAssignmentChanged,
            RestartFlags_::kNoteExpressionChanged,
        ]
        .into_iter()
        .enumerate()
        {
            sink.restart(flags);
            assert_eq!(sink.parameter_inventory_revision(), 3 + index as u64);
            assert_eq!(sink.take_restart(), flags);
            assert_eq!(sink.parameter_inventory_revision(), 3 + index as u64);
        }
    }

    #[test]
    fn host_creates_message_instances() {
        let host = Vst3Host::new();
        let application = host
            .to_com_ptr::<IHostApplication>()
            .expect("host application");
        let mut object: *mut c_void = std::ptr::null_mut();
        // SAFETY: The retained in-process host interface receives valid IID arrays and a writable object-pointer output for this synchronous call.
        let result = unsafe {
            application.createInstance(
                IMessage::IID.as_ptr() as *mut TUID,
                IMessage::IID.as_ptr() as *mut TUID,
                &mut object,
            )
        };
        assert_eq!(result, kResultOk);
        assert!(!object.is_null());
        let message =
            // SAFETY: Successful createInstance transfers one IMessage reference through the checked non-null output; this ComPtr adopts it once.
            unsafe { ComPtr::<IMessage>::from_raw(object as *mut IMessage) }.expect("message");
        drop(message);
    }
}
