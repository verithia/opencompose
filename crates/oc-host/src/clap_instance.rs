//! CLAP plugin instance — loads a CLAP plugin, sends MIDI, gets audio.
//!
//! This is the runtime host for a single CLAP plugin instance.
//! It manages the plugin lifecycle: create → activate → process → deactivate.

#![expect(
    clippy::result_large_err,
    reason = "CLAP faults retain their fixed 256-byte cause inline so native/audio failure publication never allocates"
)]

pub use crate::clap_failure::{ClapFailure, ClapFailureRecord, ClapOperation};
use crate::clap_guard::{self, Fault};
pub use crate::clap_guard::{Fault as ClapFault, FaultKind as ClapFaultKind};
use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_EXPRESSION, CLAP_EVENT_NOTE_OFF,
    CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT,
    CLAP_NOTE_EXPRESSION_BRIGHTNESS, CLAP_NOTE_EXPRESSION_EXPRESSION, CLAP_NOTE_EXPRESSION_PAN,
    CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING, CLAP_NOTE_EXPRESSION_VIBRATO,
    CLAP_NOTE_EXPRESSION_VOLUME, CLAP_TRANSPORT_HAS_BEATS_TIMELINE,
    CLAP_TRANSPORT_HAS_SECONDS_TIMELINE, CLAP_TRANSPORT_HAS_TEMPO,
    CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_PLAYING, clap_event_header,
    clap_event_midi, clap_event_note, clap_event_note_expression, clap_event_param_value,
    clap_event_transport, clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_plugin_tail};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

use crate::PluginTransport;
use crate::ports::{self, AudioPort};
use oc_dsp::AudioEffect;
use serde::{Deserialize, Serialize};

mod authored_parameters;
pub use authored_parameters::ClapAuthoredParameterFailure;
mod control;
pub use control::{
    ClapOfflineForkPlan, ClapParameterReadPlan, ClapStateCapturePlan, PreparedClapControlParameter,
};
mod lifetime;
use lifetime::ClapCore;

/// Opaque editor lease retaining native ownership across queued main-thread work.
pub struct GuiPtrs {
    plugin: *const clap_plugin,
    gui: *const clap_sys::ext::gui::clap_plugin_gui,
    session: Arc<Mutex<GuiSession>>,
    owner: GuiOwner,
}
enum GuiOwner {
    Instance(Arc<ClapCore>),
    #[cfg(test)]
    Fixture,
}

#[derive(Default)]
struct GuiSession {
    created: bool,
    gui: usize,
    failure: Arc<ClapFailure>,
    /// One owned Objective-C retain, released on the main thread after detach.
    #[cfg(any(target_os = "macos", test))]
    window: usize,
}

// SAFETY: production leases retain the strong native core; private pointers
// cannot escape. GUI entrypoints serialize editor calls through the session.
unsafe impl Send for GuiPtrs {}
// SAFETY: Shared GUI leases expose no raw pointers; native editor operations require captured main admission and serialize through the retained session.
unsafe impl Sync for GuiPtrs {}

#[cfg(test)]
mod gui_lifecycle_tests {
    use super::*;
    use std::cell::Cell;

    struct FakeGui {
        create_ok: bool,
        show_ok: bool,
        creates: Cell<usize>,
        destroys: Cell<usize>,
    }

    unsafe fn data<'a>(plugin: *const clap_plugin) -> &'a FakeGui {
        // SAFETY: The test fixture sets plugin_data to its live FakeGui allocation before callbacks and retains it until the descriptor is destroyed.
        unsafe { &*((*plugin).plugin_data as *const FakeGui) }
    }
    unsafe extern "C" fn supported(
        _: *const clap_plugin,
        _: *const std::ffi::c_char,
        floating: bool,
    ) -> bool {
        floating
    }
    unsafe extern "C" fn create(
        plugin: *const clap_plugin,
        _: *const std::ffi::c_char,
        _: bool,
    ) -> bool {
        // SAFETY: This test plugin descriptor points to its owning FakeGui allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let d = unsafe { data(plugin) };
        d.creates.set(d.creates.get() + 1);
        d.create_ok
    }
    unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
        // SAFETY: This test plugin descriptor points to its owning FakeGui allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let d = unsafe { data(plugin) };
        d.destroys.set(d.destroys.get() + 1);
    }
    unsafe extern "C" fn show(plugin: *const clap_plugin) -> bool {
        // SAFETY: This test plugin descriptor points to its owning FakeGui allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        unsafe { data(plugin).show_ok }
    }

    #[test]
    fn clap_editor_failure_cleanup_and_duplicate_close_are_exact() {
        for (create_ok, show_ok, expected_destroys) in
            [(false, true, 0), (true, false, 1), (true, true, 1)]
        {
            let mut fake = FakeGui {
                create_ok,
                show_ok,
                creates: Cell::new(0),
                destroys: Cell::new(0),
            };
            // CLAP structs contain only raw pointers and optional C callbacks.
            // SAFETY: This CLAP C descriptor contains integers, raw pointers and nullable callbacks; zero initializes valid storage before its fields are populated.
            let mut plugin: clap_plugin = unsafe { std::mem::zeroed() };
            plugin.plugin_data = (&mut fake as *mut FakeGui).cast();
            // SAFETY: This CLAP C descriptor contains integers, raw pointers and nullable callbacks; zero initializes valid storage before its fields are populated.
            let mut api: clap_sys::ext::gui::clap_plugin_gui = unsafe { std::mem::zeroed() };
            api.is_api_supported = Some(supported);
            api.create = Some(create);
            api.destroy = Some(destroy);
            api.show = Some(show);
            let gui = GuiPtrs {
                plugin: &plugin,
                gui: &api,
                session: Arc::default(),
                owner: GuiOwner::Fixture,
            };
            gui.close().unwrap(); // Never-created GUI must not receive destroy.
            assert_eq!(fake.destroys.get(), 0);
            assert_eq!(gui.open().is_ok(), create_ok && show_ok);
            assert_eq!(fake.creates.get(), 1);
            if create_ok && show_ok {
                assert!(gui.open().is_err());
                assert_eq!(fake.creates.get(), 1, "duplicate open must not call create");
            } else {
                assert_eq!(
                    fake.destroys.get(),
                    expected_destroys,
                    "failed open cleans up immediately"
                );
            }
            let second = GuiPtrs {
                plugin: &plugin,
                gui: &api,
                session: Arc::clone(&gui.session),
                owner: GuiOwner::Fixture,
            };
            second.close().unwrap();
            gui.close().unwrap();
            assert_eq!(fake.destroys.get(), expected_destroys);
            assert!(!gui.session.lock().unwrap().created);
            assert_eq!(gui.session.lock().unwrap().window, 0);
        }
    }
}

struct ClapStateInputStream<'a> {
    failure: Option<Arc<ClapFailure>>,
    data: &'a [u8],
    position: usize,
}

impl<'a> ClapStateInputStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            failure: None,
            data,
            position: 0,
        }
    }

    fn as_stream(&mut self) -> clap_istream {
        clap_istream {
            ctx: self as *mut Self as *mut c_void,
            read: Some(clap_state_stream_read),
        }
    }
}

unsafe extern "C" fn clap_state_stream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    let failure = if stream.is_null() {
        None
    } else {
        // SAFETY: a non-null stream belongs to this instance for the callback.
        unsafe { ((*stream).ctx as *const ClapStateInputStream<'_>).as_ref() }
            .and_then(|stream| stream.failure.as_deref())
    };
    clap_guard::callback_tracked(failure, -1, || {
        if stream.is_null() || buffer.is_null() {
            return -1;
        }
        // SAFETY: as_stream installs this live input-stream context; the native read callback is synchronous and has exclusive access until its invocation returns.
        let state_stream = unsafe { &mut *((*stream).ctx as *mut ClapStateInputStream<'_>) };
        let remaining = state_stream
            .data
            .len()
            .saturating_sub(state_stream.position);
        let to_copy = remaining.min(usize::try_from(size).unwrap_or(usize::MAX));
        if to_copy == 0 {
            return 0;
        }
        // SAFETY: The stream bounds this copy by both remaining source bytes and requested destination capacity; the synchronous caller provides a disjoint writable destination.
        unsafe {
            ptr::copy_nonoverlapping(
                state_stream.data[state_stream.position..].as_ptr(),
                buffer as *mut u8,
                to_copy,
            );
        }
        state_stream.position += to_copy;
        to_copy as i64
    })
}

struct ClapStateOutputStream {
    failure: Option<Arc<ClapFailure>>,
    data: Vec<u8>,
}

impl ClapStateOutputStream {
    fn new() -> Self {
        Self {
            failure: None,
            data: Vec::new(),
        }
    }

    fn as_stream(&mut self) -> clap_ostream {
        clap_ostream {
            ctx: self as *mut Self as *mut c_void,
            write: Some(clap_state_stream_write),
        }
    }
}

unsafe extern "C" fn clap_state_stream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    let failure = if stream.is_null() {
        None
    } else {
        // SAFETY: a non-null stream belongs to this instance for the callback.
        unsafe { ((*stream).ctx as *const ClapStateOutputStream).as_ref() }
            .and_then(|stream| stream.failure.as_deref())
    };
    clap_guard::callback_tracked(failure, -1, || {
        if stream.is_null() || buffer.is_null() {
            return -1;
        }
        // SAFETY: as_stream installs this live output-stream context; the native write callback is synchronous and has exclusive access until its invocation returns.
        let state_stream = unsafe { &mut *((*stream).ctx as *mut ClapStateOutputStream) };
        let Ok(size) = usize::try_from(size) else {
            return -1;
        };
        if size > isize::MAX as usize
            || state_stream
                .data
                .len()
                .checked_add(size)
                .is_none_or(|total| total > isize::MAX as usize)
            || state_stream.data.try_reserve(size).is_err()
        {
            return -1;
        }
        // SAFETY: the stream caller supplies size readable bytes; the checked
        // length is representable for a slice and the destination is reserved.
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, size) };
        state_stream.data.extend_from_slice(bytes);
        size as i64
    })
}

impl GuiPtrs {
    pub fn is_for(&self, instance: &ClapInstance) -> bool {
        match &self.owner {
            GuiOwner::Instance(core) => Arc::ptr_eq(core, &instance.core),
            #[cfg(test)]
            GuiOwner::Fixture => false,
        }
    }

    fn foreign<T>(
        session: &mut GuiSession,
        operation: ClapOperation,
        call: impl FnOnce() -> Result<T, Fault>,
    ) -> Result<T, String> {
        if session.failure.failed() {
            return Err(Fault::quarantined().describe_for("CLAP"));
        }
        call().map_err(|fault| {
            session.failure.record(operation, fault);
            fault.describe_for("CLAP")
        })
    }

    /// Open the plugin's GUI. MUST be called on the main thread.
    ///
    /// Tries floating first, then falls back to parented mode
    /// (creates a native window and embeds the plugin view into it).
    pub fn open(&self) -> Result<(u32, u32), String> {
        let _main = match &self.owner {
            GuiOwner::Instance(core) => Some(
                core.main_call()
                    .ok_or("The CLAP editor requires its idle main owner; retry")?,
            ),
            #[cfg(test)]
            GuiOwner::Fixture => None,
        };
        match &self.owner {
            GuiOwner::Instance(core) => {
                core.check_control_thread()?;
                if core.retired.load(std::sync::atomic::Ordering::Acquire) {
                    return Err("This CLAP editor request belongs to a retired instance".into());
                }
            }
            #[cfg(test)]
            GuiOwner::Fixture => {}
        }
        let mut session = self
            .session
            .try_lock()
            .map_err(|_| "The CLAP editor is busy; retry after its current operation finishes")?;
        if session.failure.failed() {
            return Err(Fault::quarantined().describe_for("CLAP"));
        }
        match &self.owner {
            GuiOwner::Instance(core) => {
                if core
                    .host
                    .gui_close_pending
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                {
                    // SAFETY: this lease owns main admission and retains both pointers.
                    unsafe { Self::close_session(self.plugin, self.gui, &mut session) };
                }
                core.host
                    .gui_closed
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            #[cfg(test)]
            GuiOwner::Fixture => {}
        }
        if session.created {
            return Err("CLAP editor is already open".into());
        }
        // A successfully created editor must always have a cleanup path.
        // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
        unsafe { (*self.gui).destroy }.ok_or("Plugin GUI has no destroy function")?;
        log::debug!(
            "[CLAP GUI open] plugin={:?} gui={:?}",
            self.plugin,
            self.gui
        );

        #[cfg(target_os = "macos")]
        let api = clap_sys::ext::gui::CLAP_WINDOW_API_COCOA;
        #[cfg(target_os = "windows")]
        let api = clap_sys::ext::gui::CLAP_WINDOW_API_WIN32;
        #[cfg(target_os = "linux")]
        let api = clap_sys::ext::gui::CLAP_WINDOW_API_X11;

        // Check which modes are supported
        // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
        let is_api_supported_fn = unsafe { (*self.gui).is_api_supported };
        let supports_floating = match is_api_supported_fn {
            // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
            Some(call) => Self::foreign(&mut session, ClapOperation::GuiQuery, || unsafe {
                clap_guard::gui_api(call, self.plugin, api.as_ptr(), true)
            })?,
            None => false,
        };
        let supports_parented = match is_api_supported_fn {
            // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
            Some(call) => Self::foreign(&mut session, ClapOperation::GuiQuery, || unsafe {
                clap_guard::gui_api(call, self.plugin, api.as_ptr(), false)
            })?,
            None => false,
        };
        log::debug!(
            "[CLAP GUI open] floating={}, parented={}",
            supports_floating,
            supports_parented
        );

        if !supports_floating && !supports_parented {
            return Err("Plugin does not support GUI on this platform".into());
        }

        let is_floating = supports_floating;

        // Create the GUI
        // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
        let create_fn = unsafe { (*self.gui).create }.ok_or("Plugin GUI has no create function")?;
        log::debug!(
            "[CLAP GUI open] calling create(floating={})...",
            is_floating
        );
        // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
        let ok = Self::foreign(&mut session, ClapOperation::GuiCreate, || unsafe {
            clap_guard::gui_api(create_fn, self.plugin, api.as_ptr(), is_floating)
        })?;
        log::debug!("[CLAP GUI open] create returned {}", ok);
        if !ok {
            return Err("Plugin GUI create failed".into());
        }
        session.created = true;
        session.gui = self.gui as usize;
        let result = (|| {
            // Get size
            let mut width: u32 = 900;
            let mut height: u32 = 560;
            // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
            if let Some(get_size) = unsafe { (*self.gui).get_size } {
                // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
                Self::foreign(&mut session, ClapOperation::GuiQuery, || unsafe {
                    clap_guard::gui_size(get_size, self.plugin, &mut width, &mut height)
                })?;
                log::debug!("[CLAP GUI open] get_size = {}x{}", width, height);
            }

            // If parented mode, create a native window and set it as parent
            if !is_floating {
                log::debug!(
                    "[CLAP GUI open] parented mode — creating native window {}x{}",
                    width,
                    height
                );
                #[cfg(target_os = "macos")]
                {
                    // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
                    let set_parent = unsafe { (*self.gui).set_parent }
                        .ok_or("Plugin GUI has no set_parent function")?;
                    let (nsview, owned_window) = Self::create_macos_window(width, height)?;
                    session.window = owned_window;
                    let window = clap_sys::ext::gui::clap_window {
                        api: api.as_ptr(),
                        specific: clap_sys::ext::gui::clap_window_handle { cocoa: nsview },
                    };
                    log::debug!(
                        "[CLAP GUI open] calling set_parent with NSView {:?}...",
                        nsview
                    );
                    // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
                    let ok = Self::foreign(&mut session, ClapOperation::GuiSetParent, || unsafe {
                        clap_guard::gui_parent(set_parent, self.plugin, &window)
                    })?;
                    log::debug!("[CLAP GUI open] set_parent returned {}", ok);
                    if !ok {
                        return Err("Plugin GUI set_parent failed".into());
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    return Err("Parented GUI not yet implemented on this platform".into());
                }
            }

            // Show
            // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
            if let Some(show_fn) = unsafe { (*self.gui).show } {
                log::debug!("[CLAP GUI open] calling show()...");
                // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
                let ok = Self::foreign(&mut session, ClapOperation::GuiShow, || unsafe {
                    clap_guard::plugin_bool(show_fn, self.plugin)
                })?;
                log::debug!("[CLAP GUI open] show returned {}", ok);
                if !ok {
                    return Err("Plugin GUI show failed".into());
                }
            } else {
                return Err("Plugin GUI has no show function".into());
            }

            Ok((width, height))
        })();
        if result.is_err() {
            // SAFETY: this GUI lease retains the native core throughout close.
            unsafe { Self::close_session(self.plugin, self.gui, &mut session) };
        }
        result
    }

    /// Create a native macOS NSWindow + NSView for embedding the plugin GUI.
    /// Returns the NSView pointer (as *mut c_void for clap_window_handle.cocoa).
    #[cfg(target_os = "macos")]
    fn create_macos_window(
        width: u32,
        height: u32,
    ) -> Result<(*mut std::ffi::c_void, usize), String> {
        use objc2::{MainThreadMarker, MainThreadOnly};
        use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // SAFETY: This function is only called from the main thread
        // (dispatched via app.run_on_main_thread).
        let mtm = MainThreadMarker::new().ok_or("CLAP editor requires the main thread")?;

        let frame = NSRect::new(
            NSPoint::new(100.0, 100.0),
            NSSize::new(width as f64, height as f64),
        );

        // No Closable — user must close via "Close GUI" button in the DAW
        // (detecting NSWindow close from objc2 requires a delegate which is complex)
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable;

        // SAFETY: MainThreadMarker above verifies AppKit affinity; the allocation and rectangle/style arguments initialize a newly owned NSWindow.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // We own a retain; close must not independently consume it.
        // SAFETY: This newly allocated main-thread window is retained by the session; disabling automatic release prevents close from consuming that owned retain.
        unsafe {
            window.setReleasedWhenClosed(false);
        }

        window.setTitle(&objc2_foundation::NSString::from_str("Plugin"));

        let content_view = window.contentView().ok_or("NSWindow contentView is null")?;

        let view_ptr: *mut std::ffi::c_void = objc2::rc::Retained::as_ptr(&content_view) as *mut _;
        log::debug!(
            "[CLAP GUI macos] NSWindow created, contentView={:?}",
            view_ptr
        );

        // Show the window
        window.makeKeyAndOrderFront(None);

        Ok((view_ptr, objc2::rc::Retained::into_raw(window) as usize))
    }

    /// Close the plugin's GUI. MUST be called on the main thread.
    pub fn close(&self) -> Result<(), String> {
        let _main = match &self.owner {
            GuiOwner::Instance(core) => Some(
                core.main_call()
                    .ok_or("The CLAP editor requires its idle main owner; retry")?,
            ),
            #[cfg(test)]
            GuiOwner::Fixture => None,
        };
        match &self.owner {
            GuiOwner::Instance(core) => core.check_control_thread()?,
            #[cfg(test)]
            GuiOwner::Fixture => {}
        }
        let mut session = self
            .session
            .try_lock()
            .map_err(|_| "The CLAP editor is busy; retry after its current operation finishes")?;
        // SAFETY: this GUI lease retains the native core throughout close.
        unsafe { Self::close_session(self.plugin, self.gui, &mut session) };
        if let Some(fault) = session.failure.first_fault() {
            return Err(fault.fault.describe_for("CLAP"));
        }
        Ok(())
    }

    unsafe fn close_session(
        plugin: *const clap_plugin,
        gui: *const clap_sys::ext::gui::clap_plugin_gui,
        session: &mut GuiSession,
    ) {
        if session.failure.failed() {
            return;
        }
        if session.created {
            // SAFETY: The strong GUI lease/finalizer retains the plugin module and cached GUI descriptor; main admission and the session lock exclude concurrent editor destruction.
            if let Some(destroy) = unsafe { (*gui).destroy }
                // SAFETY: The main-thread GUI lease and locked session retain the plugin/extension; the exact-signature shim borrows these arguments only until the native call returns.
                && Self::foreign(session, ClapOperation::GuiDestroy, || unsafe {
                    clap_guard::plugin_void(destroy, plugin)
                })
                .is_err()
            {
                return;
            }
            session.created = false;
        }
        #[cfg(target_os = "macos")]
        if session.window != 0 {
            // SAFETY: session.window is the single retain transferred from create_macos_window; main-thread close takes it back exactly once before zeroing the stored address.
            if let Some(window) = unsafe {
                objc2::rc::Retained::<objc2_app_kit::NSWindow>::from_raw(
                    session.window as *mut objc2_app_kit::NSWindow,
                )
            } {
                window.close();
            }
            session.window = 0;
        }
    }
}

/// Parameter info from a CLAP plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginParam {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub current: f64,
    pub display: String,
    pub flags: u32,
}

/// One declared audio port, captured while the plugin is deactivated.
///
/// `process()` requires one audio buffer per declared port, in declaration
/// order; a host that sends fewer buffers makes the plugin index past the
/// array it was given.
#[derive(Debug, Clone, Default)]
struct AudioPortInfo {
    channels: u32,
    flags: u32,
}

/// Audio port layout captured while the plugin is deactivated.
///
/// A plugin without the audio-ports extension is assumed to have one stereo
/// input and one stereo output, which is what such plugins expect.
#[derive(Debug, Clone, Default)]
struct AudioPortLayout {
    inputs: Vec<AudioPortInfo>,
    outputs: Vec<AudioPortInfo>,
}

/// One parameter as the name lookup needs it.
#[derive(Clone, Copy, Debug)]
struct CachedParam {
    id: u32,
    min: f64,
    max: f64,
    flags: u32,
}

/// Normalized-name lookup over a plugin's parameter list.
///
/// A plugin parameter change event resolves one name per automation step, so
/// enumerating and allocating every parameter for each step is too expensive.
/// The cache holds only the fields a write needs; a rescan invalidates it.
#[derive(Default)]
struct ParamCache {
    by_name: HashMap<String, CachedParam>,
    valid: bool,
}

impl From<&PluginParam> for CachedParam {
    fn from(param: &PluginParam) -> Self {
        Self {
            id: param.id,
            min: param.min,
            max: param.max,
            flags: param.flags,
        }
    }
}

/// The three names a parameter answers to: bare, module-qualified and
/// module-prefixed. The first match wins, the same order the uncached scan used.
fn param_keys(name: &str, module: &str) -> [String; 3] {
    [
        normalize_param_name(name),
        normalize_param_name(&format!("{module}{name}")),
        normalize_param_name(&format!("{module}_{name}")),
    ]
}

/// A live CLAP plugin instance that can process audio and receive MIDI.
pub struct ClapInstance {
    plugin: *const clap_plugin,
    bundle_path: std::path::PathBuf,
    plugin_id: String,
    core: Arc<ClapCore>,
    failure: Arc<ClapFailure>,
    sample_rate: u32,
    max_frames: u32,
    active: bool,
    processing: bool,
    audio_ports: AudioPortLayout,
    /// Sidechain input the project routes into; 0 is the first declared one.
    sidechain_input_index: usize,
    // Pending events for next process call
    pending_events: Vec<ClapPendingEvent>,
    /// Reused process-call event storage; capacity is retained across blocks.
    event_scratch: Vec<ClapProcessEvent>,
    /// Header pointers into `event_scratch`, also retained across blocks.
    event_header_scratch: Vec<*const clap_event_header>,
    gui_session: Arc<Mutex<GuiSession>>,
    /// Name lookup for parameter writes, rebuilt after a rescan.
    param_cache: ParamCache,
    prepared_tail: Option<u32>,
    prepared_latency: u32,
    prepared_gui: usize,
    prepared_midi1: bool,
    prepared_parameters: Vec<PluginParam>,
}

enum ClapPendingEvent {
    Note(NoteEvent),
    Midi(MidiEvent),
    NoteExpression(NoteExpressionEvent),
    Param(ParamEvent),
}

impl ClapPendingEvent {
    fn sample_offset_mut(&mut self) -> &mut u32 {
        match self {
            Self::Note(event) => &mut event.sample_offset,
            Self::Midi(event) => &mut event.sample_offset,
            Self::NoteExpression(event) => &mut event.sample_offset,
            Self::Param(event) => &mut event.sample_offset,
        }
    }
}

fn pending_events_for_span(
    pending: &mut Vec<ClapPendingEvent>,
    start: u32,
    frames: u32,
) -> Vec<ClapPendingEvent> {
    let mut current = Vec::new();
    let mut remaining = Vec::new();
    for mut event in pending.drain(..) {
        let offset = event.sample_offset_mut();
        if *offset >= start && u64::from(*offset) < u64::from(start) + u64::from(frames) {
            *offset -= start;
            current.push(event);
        } else {
            remaining.push(event);
        }
    }
    *pending = remaining;
    current
}

struct NoteEvent {
    on: bool, // true=NoteOn, false=NoteOff
    key: i16,
    channel: i16,
    velocity: f64,
    sample_offset: u32,
}

struct MidiEvent {
    data: [u8; 3],
    sample_offset: u32,
}

struct NoteExpressionEvent {
    channel: i16,
    key: i16,
    expression_id: i32,
    value: f64,
    sample_offset: u32,
}

struct ParamEvent {
    id: u32,
    value: f64,
    sample_offset: u32,
}

fn param_event_to_clap(param: ParamEvent) -> clap_event_param_value {
    clap_event_param_value {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_param_value>() as u32,
            time: param.sample_offset,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        },
        param_id: param.id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value: param.value,
    }
}

enum ClapProcessEvent {
    Note(clap_event_note),
    Midi(clap_event_midi),
    NoteExpression(clap_event_note_expression),
    Param(clap_event_param_value),
}

impl ClapProcessEvent {
    fn header(&self) -> *const clap_event_header {
        match self {
            Self::Note(event) => &event.header,
            Self::Midi(event) => &event.header,
            Self::NoteExpression(event) => &event.header,
            Self::Param(event) => &event.header,
        }
    }
}

/// Our host data — passed to the plugin via clap_host callbacks.
struct ClapHostData {
    failure: Arc<ClapFailure>,
    host: clap_host,
    /// The thread that created the instance; CLAP's `main-thread`.
    main_thread: std::thread::ThreadId,
    /// Marker for the thread currently acting as CLAP's `audio-thread`.
    audio_thread: std::sync::atomic::AtomicPtr<std::ffi::c_void>,
    audio_busy: std::sync::atomic::AtomicBool,
    main_callback_pending: std::sync::atomic::AtomicBool,
    gui_close_pending: std::sync::atomic::AtomicBool,
    /// Flag set by plugin when it wants a param flush (via request_flush callback)
    flush_requested: std::sync::atomic::AtomicBool,
    /// Flag set when plugin's GUI is closed (by the plugin or native window close)
    gui_closed: std::sync::atomic::AtomicBool,
    /// Flag set when plugin requests a restart (deactivate + reactivate cycle)
    restart_requested: std::sync::atomic::AtomicBool,
    /// Flag set when the plugin's parameter list changed (a rescan request).
    params_changed: std::sync::atomic::AtomicBool,
}

// SAFETY: ClapInstance is only used from a single thread (the audio thread
// or the command thread, never both simultaneously).
unsafe impl Send for ClapInstance {}

impl ClapInstance {
    /// First concrete fault, or None while its bounded publication is in progress.
    pub fn failure(&self) -> Option<Fault> {
        self.failure.first_fault().map(|record| record.fault)
    }

    /// Clone during preparation or control work, then retain for lock-free polling.
    pub fn failure_handle(&self) -> Arc<ClapFailure> {
        Arc::clone(&self.failure)
    }

    fn note_fault(&self, operation: ClapOperation, fault: Fault) {
        self.failure.record(operation, fault);
    }

    // Admission and bookkeeping only. The typed native shim, not this Rust
    // closure, catches foreign exceptions before they reach Rust frames.
    fn foreign<T>(
        &self,
        operation: ClapOperation,
        call: impl FnOnce() -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        if self.failure.failed() {
            return Err(Fault::quarantined());
        }
        let audio = matches!(
            operation,
            ClapOperation::StartProcessing
                | ClapOperation::StopProcessing
                | ClapOperation::Reset
                | ClapOperation::Process
        ) || (operation == ClapOperation::ParameterFlush && self.active);
        let main = matches!(
            operation,
            ClapOperation::Init
                | ClapOperation::Activate
                | ClapOperation::Deactivate
                | ClapOperation::Destroy
                | ClapOperation::PortQuery
                | ClapOperation::ParameterQuery
        ) || (operation == ClapOperation::ParameterFlush && !self.active);
        let _audio = if audio {
            Some(self.core.audio_call().inspect_err(|fault| {
                self.note_fault(operation, *fault);
            })?)
        } else {
            None
        };
        let _main = if main || (audio && self.core.control_is_current()) {
            if main && !self.core.control_is_current() {
                let fault = Fault::host_contract(
                    "native control requires the registered main thread; prepare this owner outside audio processing",
                );
                self.note_fault(operation, fault);
                return Err(fault);
            }
            Some(self.core.main_call().ok_or_else(|| {
                Fault::host_contract(
                    "the native main owner is busy; retry after its current operation",
                )
            })?)
        } else {
            None
        };
        let result = call();
        if let Err(fault) = result {
            self.note_fault(operation, fault);
        }
        // Only the explicit CLI offline scope services callbacks here. Release
        // both symbolic roles first: on_main_thread must never observe an
        // in-flight audio call or recursively collide with its main admission.
        drop(_main);
        drop(_audio);
        // Lifecycle methods publish their new Core state before servicing a
        // requested callback/flush. init(false) must go straight to destroy.
        let transition = matches!(
            operation,
            ClapOperation::Init
                | ClapOperation::Activate
                | ClapOperation::Deactivate
                | ClapOperation::Destroy
                | ClapOperation::StartProcessing
                | ClapOperation::StopProcessing
        );
        if !transition && result.is_ok() && crate::clap_main::service_offline_main().is_err() {
            let fault = Fault::host_contract("the offline CLAP main service failed");
            self.note_fault(operation, fault);
            return Err(fault);
        }
        result
    }

    fn read_latency_samples(&self) -> u32 {
        if self.failure.failed() {
            return 0;
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(get_extension) = (unsafe { (*self.plugin).get_extension }) else {
            return 0;
        };
        let extension = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_extension, self.plugin, CLAP_EXT_LATENCY.as_ptr())
            })
            .unwrap_or(ptr::null());
        if extension.is_null() {
            return 0;
        }
        // SAFETY: The non-null get_extension result has this SDK-defined extension layout and remains owned by the retained initialized plugin until destruction.
        let latency = unsafe { &*(extension as *const clap_plugin_latency) };
        latency
            .get
            .map(|get| {
                // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
                self.foreign(ClapOperation::ParameterQuery, || unsafe {
                    clap_guard::plugin_u32(get, self.plugin)
                })
                .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    pub fn refresh_control_metadata(&mut self) -> Result<(), String> {
        self.core.check_control_thread()?;
        self.prepared_tail = self.read_plugin_tail_samples();
        self.prepared_latency = self.read_latency_samples();
        self.prepared_gui = self.read_gui_ext().map_or(0, |gui| gui as usize);
        self.prepared_midi1 = self.read_supports_midi1_input();
        self.prepared_parameters = self.core.refresh_parameters()?;
        self.core
            .host
            .params_changed
            .store(false, std::sync::atomic::Ordering::Release);
        self.rebuild_param_cache();
        if let Some(fault) = self.failure() {
            return Err(fault.describe_for("CLAP"));
        }
        Ok(())
    }

    /// Scanner id the instance was loaded with.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn tail_samples(&self) -> u32 {
        self.plugin_tail_samples().unwrap_or(0)
    }

    /// Declared tail, or `None` when the plugin exposes no tail extension.
    pub fn plugin_tail_samples(&self) -> Option<u32> {
        self.prepared_tail
    }

    fn read_plugin_tail_samples(&self) -> Option<u32> {
        if self.failure.failed() {
            return None;
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let get_extension = (unsafe { (*self.plugin).get_extension })?;
        let extension = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_extension, self.plugin, CLAP_EXT_TAIL.as_ptr())
            })
            .ok()?;
        if extension.is_null() {
            return None;
        }
        // SAFETY: The non-null get_extension result has this SDK-defined extension layout and remains owned by the retained initialized plugin until destruction.
        let tail = unsafe { &*(extension as *const clap_plugin_tail) };
        tail.get.and_then(|get| {
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            self.foreign(ClapOperation::ParameterQuery, || unsafe {
                clap_guard::plugin_u32(get, self.plugin)
            })
            .ok()
        })
    }

    /// Diagnostic snapshot of the plugin's declared audio ports.
    pub fn debug_audio_ports(&self) -> Vec<(bool, u32, u32, u32, String)> {
        if self.failure.failed() {
            return Vec::new();
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(get_extension) = (unsafe { (*self.plugin).get_extension }) else {
            return Vec::new();
        };
        let extension = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_extension, self.plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
            })
            .unwrap_or(ptr::null());
        if extension.is_null() {
            return Vec::new();
        }
        // SAFETY: The non-null get_extension result has this SDK-defined extension layout and remains owned by the retained initialized plugin until destruction.
        let ports = unsafe { &*(extension as *const clap_plugin_audio_ports) };
        let Some(get) = ports.get else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for is_input in [true, false] {
            let count = ports
                .count
                .map(|count| {
                    // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
                    self.foreign(ClapOperation::PortQuery, || unsafe {
                        clap_guard::ports_count(count, self.plugin, is_input)
                    })
                    .unwrap_or(0)
                })
                .unwrap_or(0);
            for index in 0..count {
                // SAFETY: This CLAP C descriptor contains integers, raw pointers and nullable callbacks; zero initializes valid storage before its fields are populated.
                let mut info = unsafe { std::mem::zeroed::<clap_audio_port_info>() };
                if self
                    // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
                    .foreign(ClapOperation::PortQuery, || unsafe {
                        clap_guard::audio_port(get, self.plugin, index, is_input, &mut info)
                    })
                    .unwrap_or(false)
                {
                    out.push((
                        is_input,
                        index,
                        info.channel_count,
                        info.flags,
                        // SAFETY: The successful CLAP metadata callback supplies a NUL-terminated name in this live C descriptor; the host copies it before releasing the borrowed metadata.
                        unsafe { std::ffi::CStr::from_ptr(info.name.as_ptr()) }
                            .to_string_lossy()
                            .into_owned(),
                    ));
                }
            }
        }
        out
    }

    /// Read the plugin's declared audio ports. Must run while deactivated.
    fn read_audio_port_layout(&self) -> AudioPortLayout {
        if self.failure.failed() {
            return AudioPortLayout::default();
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(get_extension) = (unsafe { (*self.plugin).get_extension }) else {
            return AudioPortLayout {
                inputs: vec![AudioPortInfo {
                    channels: 2,
                    flags: CLAP_AUDIO_PORT_IS_MAIN,
                }],
                outputs: vec![AudioPortInfo {
                    channels: 2,
                    flags: CLAP_AUDIO_PORT_IS_MAIN,
                }],
            };
        };
        let extension = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_extension, self.plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
            })
            .unwrap_or(ptr::null());
        if extension.is_null() {
            return AudioPortLayout {
                inputs: vec![AudioPortInfo {
                    channels: 2,
                    flags: CLAP_AUDIO_PORT_IS_MAIN,
                }],
                outputs: vec![AudioPortInfo {
                    channels: 2,
                    flags: CLAP_AUDIO_PORT_IS_MAIN,
                }],
            };
        }
        // SAFETY: The non-null get_extension result has this SDK-defined extension layout and remains owned by the retained initialized plugin until destruction.
        let ports = unsafe { &*(extension as *const clap_plugin_audio_ports) };
        let Some(get) = ports.get else {
            return AudioPortLayout::default();
        };
        let read = |is_input: bool| -> Vec<AudioPortInfo> {
            let count = ports
                .count
                .map(|count| {
                    // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
                    self.foreign(ClapOperation::PortQuery, || unsafe {
                        clap_guard::ports_count(count, self.plugin, is_input)
                    })
                    .unwrap_or(0)
                })
                .unwrap_or(0);
            (0..count)
                .filter_map(|index| {
                    // SAFETY: This CLAP C descriptor contains integers, raw pointers and nullable callbacks; zero initializes valid storage before its fields are populated.
                    let mut info = unsafe { std::mem::zeroed::<clap_audio_port_info>() };
                    (self
                        // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
                        .foreign(ClapOperation::PortQuery, || unsafe {
                            clap_guard::audio_port(get, self.plugin, index, is_input, &mut info)
                        })
                        .unwrap_or(false))
                    .then_some(AudioPortInfo {
                        channels: info.channel_count,
                        flags: info.flags,
                    })
                })
                .collect()
        };
        AudioPortLayout {
            inputs: read(true),
            outputs: read(false),
        }
    }

    /// Stereo output pairs the plugin declares in its audio-ports extension.
    pub fn output_count(&self) -> usize {
        let channels: usize = self
            .audio_ports
            .outputs
            .iter()
            .map(|port| port.channels as usize)
            .sum();
        channels.div_ceil(2).max(1)
    }

    /// Declared audio output ports, in extension order.
    pub fn output_ports(&self) -> Vec<AudioPort> {
        ports::ports_from_unsigned(
            &self
                .audio_ports
                .outputs
                .iter()
                .map(|port| port.channels)
                .collect::<Vec<_>>(),
        )
    }

    /// Declared audio input ports, in extension order; empty for instruments.
    pub fn input_ports(&self) -> Vec<AudioPort> {
        if self.audio_ports.inputs.is_empty() {
            Vec::new()
        } else {
            ports::ports_from_unsigned(
                &self
                    .audio_ports
                    .inputs
                    .iter()
                    .map(|port| port.channels)
                    .collect::<Vec<_>>(),
            )
        }
    }

    /// Sidechain input ports: declared inputs after the main one.
    pub fn sidechain_input_count(&self) -> usize {
        self.audio_ports
            .inputs
            .iter()
            .skip(1)
            .filter(|port| port.channels > 0 && port.flags & CLAP_AUDIO_PORT_IS_MAIN == 0)
            .count()
    }

    /// Absolute input port index of the `index`-th sidechain port.
    pub fn sidechain_input_port(&self, index: usize) -> Option<usize> {
        self.audio_ports
            .inputs
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, port)| port.channels > 0 && port.flags & CLAP_AUDIO_PORT_IS_MAIN == 0)
            .nth(index)
            .map(|(port_index, _)| port_index)
    }

    /// The first non-main input port, which CLAP hosts use as the sidechain.
    pub fn has_sidechain_input(&self) -> bool {
        self.sidechain_input_port(0).is_some()
    }

    /// Route `process_*_with_sidechain` audio to a sidechain input index.
    pub fn set_sidechain_input_index(&mut self, index: usize) {
        self.sidechain_input_index = index;
    }

    /// Load and instantiate a CLAP plugin from a .clap bundle.
    pub fn load(
        clap_path: &Path,
        plugin_id: &str,
        sample_rate: u32,
        max_frames: u32,
    ) -> Result<Self, String> {
        let executor = crate::clap_main::ClapMainExecutor::registered()?;
        let _main = executor.enter()?;
        if !executor.accepts_preparation() {
            return Err("The CLAP main executor is shutting down".into());
        }
        let module = crate::clap_module::acquire_native_module(clap_path)?;
        let factory = module.module().factory();
        let failure = module.failure();
        let mut host_data = Box::new(ClapHostData {
            failure: Arc::clone(&failure),
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: c"OpenCompose".as_ptr(),
                vendor: c"OpenCompose".as_ptr(),
                url: c"https://github.com/verithia/opencompose".as_ptr(),
                version: c"0.1.0".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            main_thread: std::thread::current().id(),
            audio_thread: std::sync::atomic::AtomicPtr::new(ptr::null_mut()),
            audio_busy: std::sync::atomic::AtomicBool::new(false),
            main_callback_pending: std::sync::atomic::AtomicBool::new(false),
            gui_close_pending: std::sync::atomic::AtomicBool::new(false),
            flush_requested: std::sync::atomic::AtomicBool::new(false),
            gui_closed: std::sync::atomic::AtomicBool::new(false),
            restart_requested: std::sync::atomic::AtomicBool::new(false),
            params_changed: std::sync::atomic::AtomicBool::new(false),
        });
        host_data.host.host_data = &*host_data as *const ClapHostData as *mut _;

        // Create plugin instance
        let id_cstr = CString::new(plugin_id).map_err(|e| e.to_string())?;
        // SAFETY: The initialized module lease retains this immutable CLAP factory descriptor throughout callback lookup.
        let create_fn = unsafe { (*factory).create_plugin }.ok_or("No create_plugin in factory")?;
        // A throwing factory may have retained the host pointer without returning
        // a plugin handle; keep callback storage alive with the pinned module.
        // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
        let plugin = match unsafe {
            clap_guard::create(create_fn, factory, &host_data.host, id_cstr.as_ptr())
        } {
            Ok(plugin) => plugin,
            Err(fault) => {
                failure.record(ClapOperation::CreatePlugin, fault);
                std::mem::forget(host_data);
                return Err(fault.describe_for("CLAP"));
            }
        };
        if plugin.is_null() {
            return Err(format!("Failed to create plugin '{}'", plugin_id));
        }

        let gui_session = Arc::new(Mutex::new(GuiSession {
            failure: Arc::clone(&failure),
            ..GuiSession::default()
        }));
        let mut instance = Self {
            plugin,
            bundle_path: clap_path.to_path_buf(),
            plugin_id: plugin_id.to_owned(),
            core: ClapCore::new(
                plugin,
                host_data,
                Arc::clone(&failure),
                Arc::clone(&gui_session),
                Some(module),
            ),
            failure: Arc::clone(&failure),
            sample_rate,
            max_frames,
            active: false,
            processing: false,
            audio_ports: AudioPortLayout::default(),
            sidechain_input_index: 0,
            pending_events: Vec::new(),
            event_scratch: Vec::new(),
            event_header_scratch: Vec::new(),
            gui_session,
            param_cache: ParamCache::default(),
            prepared_tail: None,
            prepared_latency: 0,
            prepared_gui: 0,
            prepared_midi1: false,
            prepared_parameters: Vec::new(),
        };

        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let init_fn = unsafe { (*plugin).init }.ok_or("No plugin init")?;
        let initialized = instance
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .foreign(ClapOperation::Init, || unsafe {
                clap_guard::plugin_bool(init_fn, plugin)
            })
            .map_err(|fault| fault.describe_for("CLAP"))?;
        if !initialized {
            return Err("Plugin init failed".into());
        }

        // Ports may only be scanned while the plugin is deactivated.
        instance.audio_ports = instance.read_audio_port_layout();
        instance.activate()?;
        instance.refresh_control_metadata()?;

        Ok(instance)
    }

    fn activate(&mut self) -> Result<(), String> {
        if self.failure.failed() {
            return Err(self
                .failure()
                .unwrap_or_else(Fault::quarantined)
                .describe_for("CLAP"));
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let activate_fn = unsafe { (*self.plugin).activate }.ok_or("No activate function")?;
        let ok = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::Activate, || unsafe {
                clap_guard::activate(
                    activate_fn,
                    self.plugin,
                    self.sample_rate as f64,
                    1,
                    self.max_frames,
                )
            })
            .map_err(|fault| fault.describe_for("CLAP"))?;
        if !ok {
            return Err("Plugin activate failed".into());
        }
        self.active = true;
        self.core
            .active
            .store(true, std::sync::atomic::Ordering::Release);
        crate::clap_main::service_offline_main()?;
        Ok(())
    }

    /// CLAP requires `start_processing` on the audio thread, which is whichever
    /// thread calls `process`. Offline rendering may schedule a chain on a
    /// worker thread, so processing is started lazily there instead of on the
    /// thread that loaded the plugin.
    fn begin_processing(&mut self) -> bool {
        if self
            .core
            .control_pending
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.note_fault(
                ClapOperation::StartProcessing,
                Fault::host_contract(
                    "apply pending editor controls on main before starting an audio owner",
                ),
            );
            return false;
        }
        if self.failure.failed() {
            return false;
        }
        if !self.active {
            return false;
        }
        if self.processing {
            return true;
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(start) = (unsafe { (*self.plugin).start_processing }) else {
            return false;
        };
        if !self
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .foreign(ClapOperation::StartProcessing, || unsafe {
                clap_guard::plugin_bool(start, self.plugin)
            })
            .unwrap_or(false)
        {
            return false;
        }
        self.processing = true;
        self.core
            .processing
            .store(true, std::sync::atomic::Ordering::Release);
        if crate::clap_main::service_offline_main().is_err() {
            self.note_fault(
                ClapOperation::MainCallback,
                Fault::host_contract("the offline main service failed after starting processing"),
            );
            return false;
        }
        true
    }

    /// CLAP requires `stop_processing` on the audio thread.
    pub fn stop_processing_now(&mut self) {
        if self.failure.failed() {
            self.processing = false;
            self.core
                .processing
                .store(false, std::sync::atomic::Ordering::Release);
            return;
        }
        if !self.processing {
            return;
        }
        // Service a pending flush before the plugin stops listening.
        if self.needs_flush() {
            self.flush_params();
        }
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        if let Some(stop) = unsafe { (*self.plugin).stop_processing } {
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            let _ = self.foreign(ClapOperation::StopProcessing, || unsafe {
                clap_guard::plugin_void(stop, self.plugin)
            });
        }
        self.processing = false;
        self.core
            .processing
            .store(false, std::sync::atomic::Ordering::Release);
        if crate::clap_main::service_offline_main().is_err() {
            self.note_fault(
                ClapOperation::MainCallback,
                Fault::host_contract("the offline main service failed after stopping processing"),
            );
        }
    }

    /// True when the instance is between `start_processing` and `stop_processing`.
    pub fn is_processing(&self) -> bool {
        self.processing
    }

    /// Return the plugin to its initial processing state without reloading it.
    /// Returns `false` when the plugin offers no reset, so the caller rebuilds
    /// the lane instead.
    pub fn reset(&mut self) -> bool {
        if self.failure.failed() {
            return false;
        }
        self.pending_events.clear();
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(reset) = (unsafe { (*self.plugin).reset }) else {
            return false;
        };
        // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
        self.foreign(ClapOperation::Reset, || unsafe {
            clap_guard::plugin_void(reset, self.plugin)
        })
        .is_ok()
    }

    /// Get the plugin's parameter list (with module paths, current values, display text).
    /// Control/UI metadata snapshot; it never invokes a plugin under app locks.
    pub fn get_params(&self) -> Vec<PluginParam> {
        self.core
            .parameters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|param| param.flags & clap_sys::ext::params::CLAP_PARAM_IS_HIDDEN == 0)
            .cloned()
            .collect()
    }

    pub fn queue_control_parameter(&self, name: &str, value: f64) -> Result<f64, String> {
        let prepared = self.prepare_control_parameter_value(name, value)?;
        let normalized = prepared.normalized_value();
        self.queue_prepared_control_parameter(prepared)?;
        Ok(normalized)
    }

    fn params_ext(&self) -> Option<*const clap_sys::ext::params::clap_plugin_params> {
        if self.failure.failed() {
            return None;
        }
        let ext_id = std::ffi::CString::new("clap.params").unwrap();
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let get_ext = unsafe { (*self.plugin).get_extension }?;
        let ext_ptr = self
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_ext, self.plugin, ext_id.as_ptr())
            })
            .ok()?;
        if ext_ptr.is_null() {
            None
        } else {
            Some(ext_ptr as *const clap_sys::ext::params::clap_plugin_params)
        }
    }

    pub fn set_state_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.core.restore_state(bytes)?;
        self.param_cache.valid = false;
        Ok(())
    }

    pub fn state_bytes(&self) -> Result<Vec<u8>, String> {
        self.core.save_state()
    }

    /// A fresh offline owner with authored state, prepared outside document locks.
    pub fn fork_for_offline_render(&self) -> Result<Self, String> {
        self.offline_fork_plan().execute_on_main()
    }

    /// Find a parameter the slow way, for a name the cache does not know.
    fn uncached_param(&self, name: &str) -> Option<PluginParam> {
        let target = normalize_param_name(name);
        self.prepared_parameters
            .iter()
            .find(|param| {
                normalize_param_name(&param.name) == target
                    || normalize_param_name(&format!("{}{}", param.module, param.name)) == target
                    || normalize_param_name(&format!("{}_{}", param.module, param.name)) == target
            })
            .cloned()
    }

    /// Rebuild the name lookup after the plugin's parameter list changed.
    fn rebuild_param_cache(&mut self) {
        let mut by_name = HashMap::new();
        for param in &self.prepared_parameters {
            let cached = CachedParam::from(param);
            for key in param_keys(&param.name, &param.module) {
                by_name.entry(key).or_insert(cached);
            }
        }
        self.param_cache.by_name = by_name;
        self.param_cache.valid = true;
    }

    fn ensure_param_cache(&mut self) -> bool {
        if self
            .core
            .host
            .params_changed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            if !self.core.control_is_current() {
                self.note_fault(ClapOperation::ParameterQuery, Fault::host_contract("the parameter inventory changed; reprepare the native owner before continuing audio"));
                return false;
            }
            if self.refresh_control_metadata().is_err() {
                return false;
            }
        }
        if !self.param_cache.valid {
            self.rebuild_param_cache();
        }
        true
    }

    fn cached_param(&mut self, name: &str) -> Option<CachedParam> {
        if !self.ensure_param_cache() {
            return None;
        }
        self.param_cache
            .by_name
            .get(&normalize_param_name(name))
            .copied()
    }

    /// Resolve one name through the cache, falling back to a full enumeration
    /// only when the cache does not know it.
    fn resolved_param(&mut self, name: &str) -> Option<CachedParam> {
        match self.cached_param(name) {
            Some(param) => Some(param),
            None => self
                .uncached_param(name)
                .map(|param| CachedParam::from(&param)),
        }
    }

    fn set_cached_param(&mut self, param: CachedParam, value: f64) -> bool {
        if param.flags & clap_sys::ext::params::CLAP_PARAM_IS_READONLY != 0 {
            return false;
        }
        let value = value.clamp(param.min, param.max);
        let Some(params_ext) = self.params_ext() else {
            return false;
        };
        // SAFETY: The strong Core retains the initialized plugin and its params extension while main-thread metadata preparation reads this immutable callback table.
        let Some(flush) = (unsafe { (*params_ext).flush }) else {
            return false;
        };

        let event = clap_event_param_value {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_param_value>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id: param.id,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        };
        let event_headers = [&event.header as *const clap_event_header];
        let event_list = EventList {
            failure: &self.failure,
            events: &event_headers,
        };
        let input_events = clap_input_events {
            ctx: &event_list as *const EventList as *mut _,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };
        let output_events = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_push),
        };
        // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
        let _ = self.foreign(ClapOperation::ParameterFlush, || unsafe {
            clap_guard::flush(flush, self.plugin, &input_events, &output_events)
        });
        true
    }

    fn queue_cached_param(&mut self, param: CachedParam, value: f64, sample_offset: u32) -> bool {
        if param.flags & clap_sys::ext::params::CLAP_PARAM_IS_READONLY != 0
            || param.flags & clap_sys::ext::params::CLAP_PARAM_IS_AUTOMATABLE == 0
        {
            return false;
        }
        self.pending_events
            .push(ClapPendingEvent::Param(ParamEvent {
                id: param.id,
                value: value.clamp(param.min, param.max),
                sample_offset,
            }));
        true
    }

    pub fn set_param_by_name(&mut self, name: &str, value: f64) -> bool {
        let Some(param) = self.resolved_param(name) else {
            return false;
        };
        self.set_cached_param(param, value)
    }

    pub fn get_param_by_name(&self, name: &str) -> Option<f64> {
        let target = normalize_param_name(name);
        self.prepared_parameters
            .iter()
            .find(|param| {
                normalize_param_name(&param.name) == target
                    || normalize_param_name(&format!("{}{}", param.module, param.name)) == target
                    || normalize_param_name(&format!("{}_{}", param.module, param.name)) == target
            })
            .map(|param| param.current)
    }

    /// Queue a sample-offset CLAP parameter event for the next process block.
    /// Unlike `set_param_by_name`, this preserves automation timing within the
    /// block and rejects read-only/non-automatable parameters.
    pub fn queue_param_by_name(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        if sample_offset >= self.max_frames {
            return false;
        }
        let Some(param) = self.resolved_param(name) else {
            return false;
        };
        self.queue_cached_param(param, value, sample_offset)
    }

    /// Queue one normalized automation value, mapped through the cached range.
    ///
    /// This is the `PluginParam` path: the caller writes a 0..=1 lane value and
    /// the device receives its own plain domain value at the same offset.
    pub fn queue_param_normalized_by_name(
        &mut self,
        name: &str,
        value: f64,
        sample_offset: u32,
    ) -> bool {
        if sample_offset >= self.max_frames {
            return false;
        }
        let Some(param) = self.resolved_param(name) else {
            return false;
        };
        let actual = param.min + value.clamp(0.0, 1.0) * (param.max - param.min);
        self.queue_cached_param(param, actual, sample_offset)
    }

    /// Check if the plugin requested a restart (deactivate + reactivate).
    /// Returns true if restart was requested. Clears the flag.
    pub fn needs_restart(&self) -> bool {
        self.core
            .host
            .restart_requested
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Perform a restart cycle: stop_processing → deactivate → rescan ports →
    /// activate. Processing restarts on the next audio thread that calls process.
    pub fn restart(&mut self) -> Result<(), String> {
        if let Some(fault) = self.failure() {
            return Err(fault.describe_for("CLAP"));
        }
        self.stop_processing_now();
        if let Some(fault) = self.failure() {
            return Err(fault.describe_for("CLAP"));
        }
        if self.active {
            // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
            if let Some(deactivate) = unsafe { (*self.plugin).deactivate } {
                let _ =
                    // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
                    self.foreign(ClapOperation::Deactivate, || unsafe { clap_guard::plugin_void(deactivate, self.plugin) });
            }
            self.active = false;
            self.core
                .active
                .store(false, std::sync::atomic::Ordering::Release);
            crate::clap_main::service_offline_main()?;
        }
        self.audio_ports = self.read_audio_port_layout();
        self.activate()
    }

    /// Check if the plugin requested a param flush (set by host callback).
    pub fn needs_flush(&self) -> bool {
        self.core
            .host
            .flush_requested
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Perform a param flush — call the plugin's flush function with empty
    /// input events to let it process any pending GUI-initiated param changes,
    /// then clear the flush flag.
    pub fn flush_params(&mut self) {
        self.core
            .host
            .flush_requested
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let Some(params_ext) = self.params_ext() else {
            return;
        };

        // SAFETY: The strong Core retains the initialized plugin and its params extension while main-thread metadata preparation reads this immutable callback table.
        if let Some(flush) = unsafe { (*params_ext).flush } {
            // Empty input events
            let empty_events: Vec<*const clap_event_header> = vec![];
            let event_list = EventList {
                failure: &self.failure,
                events: &empty_events,
            };
            let input_events = clap_input_events {
                ctx: &event_list as *const EventList as *mut _,
                size: Some(input_events_size),
                get: Some(input_events_get),
            };
            let output_events = clap_output_events {
                ctx: ptr::null_mut(),
                try_push: Some(output_events_push),
            };
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            let _ = self.foreign(ClapOperation::ParameterFlush, || unsafe {
                clap_guard::flush(flush, self.plugin, &input_events, &output_events)
            });
        }
    }

    /// Check if the plugin's GUI was closed externally (via native X button or plugin).
    /// Clears the flag after reading.
    pub fn was_gui_closed(&self) -> bool {
        self.core
            .host
            .gui_closed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Reset the gui_closed flag (call when we open the GUI).
    pub fn clear_gui_closed(&self) {
        self.core
            .host
            .gui_closed
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if the plugin provides a GUI.
    pub fn has_gui(&self) -> bool {
        self.get_gui_ext().is_some()
    }

    fn get_gui_ext(&self) -> Option<*const clap_sys::ext::gui::clap_plugin_gui> {
        (self.prepared_gui != 0).then_some(self.prepared_gui as *const _)
    }

    fn read_gui_ext(&self) -> Option<*const clap_sys::ext::gui::clap_plugin_gui> {
        if self.failure.failed() {
            return None;
        }
        let ext_id = std::ffi::CString::new("clap.gui").unwrap();
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let get_ext = unsafe { (*self.plugin).get_extension }?;
        let ext_ptr = self
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_ext, self.plugin, ext_id.as_ptr())
            })
            .ok()?;
        if ext_ptr.is_null() {
            None
        } else {
            Some(ext_ptr as *const clap_sys::ext::gui::clap_plugin_gui)
        }
    }

    /// Get the raw pointers needed to open the GUI on the main thread.
    /// Returns (plugin_ptr, gui_ext_ptr) that can be sent to the main thread.
    pub fn gui_ptrs(&self) -> Option<GuiPtrs> {
        let gui = self.get_gui_ext()?;
        Some(GuiPtrs {
            plugin: self.plugin,
            gui,
            session: Arc::clone(&self.gui_session),
            owner: GuiOwner::Instance(Arc::clone(&self.core)),
        })
    }

    /// Close the plugin's native GUI. Must be called from the main thread.
    pub fn close_gui(&self) {
        if let Some(gui) = self.gui_ptrs()
            && let Err(error) = gui.close()
        {
            log::warn!("CLAP editor close: {error}");
        }
    }

    /// Queue a note-on event for the next process call.
    pub fn note_on(&mut self, key: u8, velocity: f64, sample_offset: u32) {
        self.note_on_channel(0, key, velocity, sample_offset);
    }

    /// Queue a note-on event for the next process call.
    pub fn note_on_channel(&mut self, channel: u8, key: u8, velocity: f64, sample_offset: u32) {
        self.pending_events.push(ClapPendingEvent::Note(NoteEvent {
            on: true,
            key: key as i16,
            channel: channel as i16,
            velocity,
            sample_offset,
        }));
    }

    /// Queue a note-off event for the next process call.
    pub fn note_off(&mut self, key: u8, sample_offset: u32) {
        self.note_off_channel(0, key, sample_offset);
    }

    /// Queue a note-off event for the next process call.
    pub fn note_off_channel(&mut self, channel: u8, key: u8, sample_offset: u32) {
        self.note_off_channel_velocity(channel, key, 0, sample_offset);
    }

    pub fn note_off_channel_velocity(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        sample_offset: u32,
    ) -> bool {
        if channel > 15 || key > 127 || velocity > 127 {
            return false;
        }
        self.pending_events.push(ClapPendingEvent::Note(NoteEvent {
            on: false,
            key: key as i16,
            channel: channel as i16,
            velocity: f64::from(velocity) / 127.0,
            sample_offset,
        }));
        true
    }

    /// Queue a raw MIDI event for the next process call.
    pub fn supports_midi1_input(&self) -> bool {
        self.prepared_midi1
    }

    fn read_supports_midi1_input(&self) -> bool {
        if self.failure.failed() {
            return false;
        }
        use clap_sys::ext::note_ports::{
            CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_MIDI, clap_note_port_info,
            clap_plugin_note_ports,
        };
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        let Some(get_extension) = (unsafe { (*self.plugin).get_extension }) else {
            return false;
        };
        let extension = self
            // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
            .foreign(ClapOperation::GetExtension, || unsafe {
                clap_guard::extension(get_extension, self.plugin, CLAP_EXT_NOTE_PORTS.as_ptr())
            })
            .unwrap_or(ptr::null());
        if extension.is_null() {
            return false;
        }
        // SAFETY: The non-null get_extension result has this SDK-defined extension layout and remains owned by the retained initialized plugin until destruction.
        let ports = unsafe { &*(extension as *const clap_plugin_note_ports) };
        let (Some(count), Some(get)) = (ports.count, ports.get) else {
            return false;
        };
        if self
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .foreign(ClapOperation::PortQuery, || unsafe {
                clap_guard::ports_count(count, self.plugin, true)
            })
            .unwrap_or(0)
            == 0
        {
            return false;
        }
        // SAFETY: This CLAP C descriptor contains integers, raw pointers and nullable callbacks; zero initializes valid storage before its fields are populated.
        let mut info: clap_note_port_info = unsafe { std::mem::zeroed() };
        // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
        self.foreign(ClapOperation::PortQuery, || unsafe {
            clap_guard::note_port(get, self.plugin, 0, true, &mut info)
        })
        .unwrap_or(false)
            && info.supported_dialects & CLAP_NOTE_DIALECT_MIDI != 0
    }

    /// Checked port-zero performance dispatch. MIDI dialect support is not a
    /// claim that the selected patch implements every controller's semantics.
    pub fn queue_performance_command(
        &mut self,
        command: &oc_synth::performance::ScheduledCommand,
        offset: u32,
        block_frames: u32,
    ) -> Result<(), String> {
        if block_frames == 0 || offset >= block_frames {
            return Err("performance event outside processing block".into());
        }
        if !self.supports_midi1_input() {
            return Err("CLAP input port zero does not support MIDI1".into());
        }
        let timeline = oc_dsl::performance::PerformanceTimeline {
            events: vec![oc_dsl::performance::PerformanceEvent {
                quarter: 0.0,
                channel: command.channel,
                message: command.message.clone(),
            }],
        };
        timeline.validate()?;
        let bytes = command.midi_bytes();
        self.midi(
            bytes[0],
            bytes[1],
            bytes.get(2).copied().unwrap_or(0),
            offset,
        );
        Ok(())
    }

    /// Legacy unchecked raw MIDI queue; explicit performance uses the checked API.
    pub fn midi(&mut self, status: u8, data1: u8, data2: u8, sample_offset: u32) {
        self.pending_events.push(ClapPendingEvent::Midi(MidiEvent {
            data: [status, data1, data2],
            sample_offset,
        }));
    }

    pub fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: u32) {
        self.midi(0xB0 | (channel & 0x0F), controller, value, sample_offset);
    }

    pub fn program_change(&mut self, channel: u8, program: u8, sample_offset: u32) {
        self.midi(0xC0 | (channel & 0x0F), program, 0, sample_offset);
    }

    pub fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: u32) {
        let raw = (value as i32 + 8192).clamp(0, 16383) as u16;
        self.midi(
            0xE0 | (channel & 0x0F),
            (raw & 0x7F) as u8,
            ((raw >> 7) & 0x7F) as u8,
            sample_offset,
        );
    }

    pub fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: u32) {
        self.midi(0xD0 | (channel & 0x0F), pressure, 0, sample_offset);
    }

    pub fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: u32) {
        self.midi(0xA0 | (channel & 0x0F), note, pressure, sample_offset);
    }

    pub fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: u32,
    ) {
        if let Some(expression_id) = clap_note_expression_id(expression) {
            self.pending_events
                .push(ClapPendingEvent::NoteExpression(NoteExpressionEvent {
                    channel: channel as i16,
                    key: note as i16,
                    expression_id,
                    value,
                    sample_offset,
                }));
        }
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
        self.process_block_with_transport_and_output_pair(num_frames, transport, 0)
    }

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

    /// Process the instrument once and fan several declared stereo output
    /// pairs out of that same block. This avoids advancing shared instruments
    /// once per requested stem.
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
    /// Pair 0 is the main output port; auxiliary pairs follow the declared
    /// port order. Callers must pre-reserve `outputs` to stay allocation-free
    /// in the audio callback.
    pub fn process_block_outputs_into(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
        output_pairs: &[usize],
        outputs: &mut [Vec<f32>],
    ) -> Result<(), String> {
        if let Some(fault) = self.failure() {
            return Err(fault.describe_for("CLAP"));
        }
        if outputs.len() != output_pairs.len() {
            return Err("CLAP output request count mismatch".into());
        }
        if num_frames == 0 {
            return Ok(());
        }
        if !self.active || !self.begin_processing() {
            if let Some(fault) = self.failure() {
                return Err(fault.describe_for("CLAP"));
            }
            for output in outputs.iter_mut() {
                output.resize(output.len() + (num_frames * 2) as usize, 0.0);
            }
            return Ok(());
        }
        if self.needs_flush() {
            self.flush_params();
        }
        if let Some(fault) = self.failure() {
            return Err(fault.describe_for("CLAP"));
        }

        // Build input events from pending notes and raw MIDI controls.
        let pending_events = std::mem::take(&mut self.pending_events);
        self.event_scratch.clear();
        self.event_scratch
            .extend(pending_events.into_iter().map(|event| match event {
                ClapPendingEvent::Note(n) => ClapProcessEvent::Note(clap_event_note {
                    header: clap_event_header {
                        size: std::mem::size_of::<clap_event_note>() as u32,
                        time: n.sample_offset,
                        space_id: CLAP_CORE_EVENT_SPACE_ID,
                        type_: if n.on {
                            CLAP_EVENT_NOTE_ON
                        } else {
                            CLAP_EVENT_NOTE_OFF
                        },
                        flags: 0,
                    },
                    note_id: -1,
                    port_index: 0,
                    channel: n.channel,
                    key: n.key,
                    velocity: n.velocity,
                }),
                ClapPendingEvent::Midi(midi) => ClapProcessEvent::Midi(clap_event_midi {
                    header: clap_event_header {
                        size: std::mem::size_of::<clap_event_midi>() as u32,
                        time: midi.sample_offset,
                        space_id: CLAP_CORE_EVENT_SPACE_ID,
                        type_: CLAP_EVENT_MIDI,
                        flags: 0,
                    },
                    port_index: 0,
                    data: midi.data,
                }),
                ClapPendingEvent::NoteExpression(expression) => {
                    ClapProcessEvent::NoteExpression(clap_event_note_expression {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note_expression>() as u32,
                            time: expression.sample_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_EXPRESSION,
                            flags: 0,
                        },
                        expression_id: expression.expression_id,
                        note_id: -1,
                        port_index: 0,
                        channel: expression.channel,
                        key: expression.key,
                        value: expression.value,
                    })
                }
                ClapPendingEvent::Param(param) => {
                    ClapProcessEvent::Param(param_event_to_clap(param))
                }
            }));

        // Wrapper for callbacks
        self.event_header_scratch.clear();
        self.event_header_scratch
            .extend(self.event_scratch.iter().map(ClapProcessEvent::header));
        let event_list = EventList {
            failure: &self.failure,
            events: &self.event_header_scratch,
        };
        let input_events = clap_input_events {
            ctx: &event_list as *const EventList as *mut _,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };

        // Output events (we don't handle output events yet)
        let output_events = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_push),
        };

        // Preserve the plugin's declared port topology. Multi-output plugins
        // usually expose several stereo ports, not one oversized port, and the
        // buffer count must match the declared port count exactly.
        let output_port_channels: Vec<u32> = self
            .audio_ports
            .outputs
            .iter()
            .map(|port| port.channels)
            .collect();
        let output_channel_count: usize = output_port_channels
            .iter()
            .map(|count| *count as usize)
            .sum();
        let mut planar = vec![vec![0.0f32; num_frames as usize]; output_channel_count];
        let mut channel_cursor = 0usize;
        let mut output_ptrs: Vec<Vec<*mut f32>> = output_port_channels
            .iter()
            .map(|count| {
                let start = channel_cursor;
                channel_cursor += *count as usize;
                planar[start..channel_cursor]
                    .iter_mut()
                    .map(|channel| channel.as_mut_ptr())
                    .collect()
            })
            .collect();
        let mut output_buffers: Vec<clap_audio_buffer> = output_ptrs
            .iter_mut()
            .map(|pointers| clap_audio_buffer {
                data32: pointers.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: pointers.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();

        // An instrument generates its own audio, but it may still declare input
        // ports; the host must provide one buffer per declared port.
        let mut input_ports: Vec<Vec<Vec<f32>>> = self
            .audio_ports
            .inputs
            .iter()
            .map(|port| {
                (0..port.channels as usize)
                    .map(|_| vec![0.0f32; num_frames as usize])
                    .collect()
            })
            .collect();
        let mut input_ptrs: Vec<Vec<*mut f32>> = input_ports
            .iter_mut()
            .map(|channels| {
                channels
                    .iter_mut()
                    .map(|channel| channel.as_mut_ptr())
                    .collect()
            })
            .collect();
        let mut input_buffers: Vec<clap_audio_buffer> = input_ptrs
            .iter_mut()
            .map(|pointers| clap_audio_buffer {
                data32: pointers.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: pointers.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();

        let transport = transport.map(clap_transport_event);

        let process = clap_process {
            steady_time: -1,
            frames_count: num_frames,
            transport: transport.as_ref().map_or(ptr::null(), |transport| {
                transport as *const clap_event_transport
            }),
            audio_inputs: if input_buffers.is_empty() {
                ptr::null()
            } else {
                input_buffers.as_mut_ptr()
            },
            audio_outputs: if output_buffers.is_empty() {
                ptr::null_mut()
            } else {
                output_buffers.as_mut_ptr()
            },
            audio_inputs_count: input_buffers.len() as u32,
            audio_outputs_count: output_buffers.len() as u32,
            in_events: &input_events,
            out_events: &output_events,
        };

        // Call plugin process
        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        if let Some(process_fn) = unsafe { (*self.plugin).process } {
            let status = self
                // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
                .foreign(ClapOperation::Process, || unsafe {
                    clap_guard::process(process_fn, self.plugin, &process)
                })
                .map_err(|fault| fault.describe_for("CLAP"))?;
            if status == clap_sys::process::CLAP_PROCESS_ERROR {
                let fault = Fault::process_error();
                self.note_fault(ClapOperation::Process, fault);
                return Err(fault.describe_for("CLAP"));
            }
        }

        interleave_output_pairs_into(&planar, output_pairs, num_frames as usize, outputs);
        Ok(())
    }

    /// Process a block of audio as an effect (with input audio).
    /// Takes stereo interleaved input, returns stereo interleaved output.
    /// Used when ClapInstance is in an effect chain (AudioEffect trait).
    pub fn process_effect_block(&mut self, input: &[f32]) -> Vec<f32> {
        self.process_effect_block_with_transport(input, None)
    }

    pub fn process_effect_block_with_transport(
        &mut self,
        input: &[f32],
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        self.process_effect_block_with_sidechain(input, None, transport)
    }

    pub fn process_effect_block_with_sidechain(
        &mut self,
        input: &[f32],
        sidechain: Option<&[f32]>,
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        let num_frames = (input.len() / 2) as u32;
        if !self.active || num_frames == 0 || !self.begin_processing() {
            return input.to_vec();
        }
        if self.needs_flush() {
            self.flush_params();
        }
        if self.failure().is_some() {
            return vec![0.0; input.len()];
        }

        let maximum = self.max_frames.max(1) as usize;
        let first_frames = transport
            .as_ref()
            .map(|t| t.block_frames(maximum))
            .unwrap_or(maximum);
        if num_frames as usize > first_frames {
            let origin = transport.as_ref().map(|t| t.sample_pos).unwrap_or(0);
            let mut output = Vec::with_capacity(input.len());
            let mut pending = std::mem::take(&mut self.pending_events);
            let mut offset = 0;
            while offset < num_frames as usize {
                let chunk_transport = transport
                    .as_ref()
                    .map(|t| t.at_sample(origin + offset as u64));
                let limit = maximum.min(num_frames as usize - offset);
                let frames = chunk_transport
                    .as_ref()
                    .map(|t| t.block_frames(limit))
                    .unwrap_or(limit);
                let range = offset * 2..(offset + frames) * 2;
                let side = sidechain.and_then(|side| side.get(range.clone()));
                self.pending_events =
                    pending_events_for_span(&mut pending, offset as u32, frames as u32);
                output.extend(self.process_effect_block_with_sidechain(
                    &input[range],
                    side,
                    chunk_transport,
                ));
                offset += frames;
            }
            // Keep future events relative to the next call, not the consumed buffer.
            for event in &mut pending {
                let offset = event.sample_offset_mut();
                *offset = offset.saturating_sub(num_frames);
            }
            self.pending_events.extend(pending);
            return output;
        }

        // Build input events from pending notes and raw MIDI controls (likely empty for effects).
        let clap_events: Vec<ClapProcessEvent> = self
            .pending_events
            .drain(..)
            .map(|event| match event {
                ClapPendingEvent::Note(n) => ClapProcessEvent::Note(clap_event_note {
                    header: clap_event_header {
                        size: std::mem::size_of::<clap_event_note>() as u32,
                        time: n.sample_offset,
                        space_id: CLAP_CORE_EVENT_SPACE_ID,
                        type_: if n.on {
                            CLAP_EVENT_NOTE_ON
                        } else {
                            CLAP_EVENT_NOTE_OFF
                        },
                        flags: 0,
                    },
                    note_id: -1,
                    port_index: 0,
                    channel: n.channel,
                    key: n.key,
                    velocity: n.velocity,
                }),
                ClapPendingEvent::Midi(midi) => ClapProcessEvent::Midi(clap_event_midi {
                    header: clap_event_header {
                        size: std::mem::size_of::<clap_event_midi>() as u32,
                        time: midi.sample_offset,
                        space_id: CLAP_CORE_EVENT_SPACE_ID,
                        type_: CLAP_EVENT_MIDI,
                        flags: 0,
                    },
                    port_index: 0,
                    data: midi.data,
                }),
                ClapPendingEvent::NoteExpression(expression) => {
                    ClapProcessEvent::NoteExpression(clap_event_note_expression {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note_expression>() as u32,
                            time: expression.sample_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_EXPRESSION,
                            flags: 0,
                        },
                        expression_id: expression.expression_id,
                        note_id: -1,
                        port_index: 0,
                        channel: expression.channel,
                        key: expression.key,
                        value: expression.value,
                    })
                }
                ClapPendingEvent::Param(param) => {
                    ClapProcessEvent::Param(param_event_to_clap(param))
                }
            })
            .collect();

        let event_headers: Vec<*const clap_event_header> =
            clap_events.iter().map(ClapProcessEvent::header).collect();
        let event_list = EventList {
            failure: &self.failure,
            events: &event_headers,
        };
        let input_events = clap_input_events {
            ctx: &event_list as *const EventList as *mut _,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };
        let output_events = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_push),
        };

        // One buffer per declared port, in declaration order. CLAP indexes
        // process->audio_inputs and audio_outputs by that order, so omitting an
        // unused port (a sidechain, for instance) makes the plugin read the
        // buffer array past its end.
        let frames = num_frames as usize;
        let layout = self.audio_ports.clone();
        let sidechain_port = self.sidechain_input_port(self.sidechain_input_index);
        let sidechain = sidechain.filter(|side| sidechain_port.is_some() && !side.is_empty());

        let mut input_ports: Vec<Vec<Vec<f32>>> = layout
            .inputs
            .iter()
            .enumerate()
            .map(|(port_index, port)| {
                (0..port.channels as usize)
                    .map(|channel| {
                        let mut data = vec![0.0f32; frames];
                        if port_index == 0 {
                            let mono = port.channels == 1;
                            for (frame, sample) in data.iter_mut().enumerate() {
                                *sample = match (mono, channel) {
                                    (true, 0) => 0.5 * (input[frame * 2] + input[frame * 2 + 1]),
                                    (false, 0) => input[frame * 2],
                                    (false, 1) => input[frame * 2 + 1],
                                    _ => 0.0,
                                };
                            }
                        } else if sidechain_port == Some(port_index)
                            && let Some(sidechain) = sidechain
                        {
                            for (frame, sample) in data.iter_mut().enumerate() {
                                if channel < 2 {
                                    *sample =
                                        sidechain.get(frame * 2 + channel).copied().unwrap_or(0.0);
                                }
                            }
                        }
                        data
                    })
                    .collect()
            })
            .collect();
        let mut input_ptrs: Vec<Vec<*mut f32>> = input_ports
            .iter_mut()
            .map(|channels| {
                channels
                    .iter_mut()
                    .map(|channel| channel.as_mut_ptr())
                    .collect()
            })
            .collect();
        let input_buffers: Vec<clap_audio_buffer> = input_ptrs
            .iter_mut()
            .map(|pointers| clap_audio_buffer {
                data32: pointers.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: pointers.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();

        let mut output_ports: Vec<Vec<Vec<f32>>> = layout
            .outputs
            .iter()
            .map(|port| {
                (0..port.channels as usize)
                    .map(|_| vec![0.0f32; frames])
                    .collect()
            })
            .collect();
        let mut output_ptrs: Vec<Vec<*mut f32>> = output_ports
            .iter_mut()
            .map(|channels| {
                channels
                    .iter_mut()
                    .map(|channel| channel.as_mut_ptr())
                    .collect()
            })
            .collect();
        let mut output_buffers: Vec<clap_audio_buffer> = output_ptrs
            .iter_mut()
            .map(|pointers| clap_audio_buffer {
                data32: pointers.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: pointers.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();

        let transport_event = transport.map(clap_transport_event);

        let process = clap_process {
            steady_time: -1,
            frames_count: num_frames,
            transport: transport_event
                .as_ref()
                .map(|event| event as *const clap_event_transport)
                .unwrap_or(ptr::null()),
            audio_inputs: if input_buffers.is_empty() {
                ptr::null()
            } else {
                input_buffers.as_ptr()
            },
            audio_outputs: if output_buffers.is_empty() {
                ptr::null_mut()
            } else {
                output_buffers.as_mut_ptr()
            },
            audio_inputs_count: input_buffers.len() as u32,
            audio_outputs_count: output_buffers.len() as u32,
            in_events: &input_events,
            out_events: &output_events,
        };

        // SAFETY: The owning Core retains this plugin descriptor and its module until native finalization, so copying this callback does not outlive its code.
        if let Some(process_fn) = unsafe { (*self.plugin).process } {
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            match self.foreign(ClapOperation::Process, || unsafe {
                clap_guard::process(process_fn, self.plugin, &process)
            }) {
                Ok(clap_sys::process::CLAP_PROCESS_ERROR) => {
                    self.note_fault(ClapOperation::Process, Fault::process_error());
                    return vec![0.0; frames * 2];
                }
                Err(_) => return vec![0.0; frames * 2],
                Ok(_) => {}
            }
        }

        // The effect chain is stereo: expose the main output port and mix a
        // single-channel main output into both sides.
        let mut interleaved = vec![0.0f32; frames * 2];
        if let Some(main) = output_ports.first() {
            match main.len() {
                0 => {}
                1 => {
                    for (frame, out) in interleaved.chunks_exact_mut(2).enumerate() {
                        out[0] = main[0][frame];
                        out[1] = main[0][frame];
                    }
                }
                _ => {
                    for (frame, out) in interleaved.chunks_exact_mut(2).enumerate() {
                        out[0] = main[0][frame];
                        out[1] = main[1][frame];
                    }
                }
            }
        }
        interleaved
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        self.stop_processing_now();
        self.core
            .active
            .store(self.active, std::sync::atomic::Ordering::Release);
        self.core
            .retired
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

fn clap_transport_event(transport: PluginTransport) -> clap_event_transport {
    let quarter_notes = transport.quarter_notes();
    let seconds = transport.seconds();
    let (numerator, denominator, bar_start, bar_number) = transport.musical_position();
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO
        | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
        | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
        | CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
    if transport.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }

    clap_event_transport {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_transport>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_TRANSPORT,
            flags: 0,
        },
        flags,
        song_pos_beats: fixed_point_time(quarter_notes, CLAP_BEATTIME_FACTOR),
        song_pos_seconds: fixed_point_time(seconds, CLAP_SECTIME_FACTOR),
        tempo: transport.current_tempo(),
        tempo_inc: 0.0,
        loop_start_beats: 0,
        loop_end_beats: 0,
        loop_start_seconds: 0,
        loop_end_seconds: 0,
        bar_start: fixed_point_time(bar_start, CLAP_BEATTIME_FACTOR),
        bar_number: bar_number as i32,
        tsig_num: numerator,
        tsig_denom: denominator,
    }
}

fn fixed_point_time(value: f64, factor: i64) -> i64 {
    (value * factor as f64).round() as i64
}

// AudioEffect implementation so CLAP instances can go in effect chains
impl AudioEffect for ClapInstance {
    fn processing_failed(&self) -> bool {
        self.failure.failed()
    }

    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        use oc_audio::ProcessingFailureReport;
        if !self.failure.failed() {
            return ProcessingFailureReport::Unavailable;
        }
        if self.failure.first_fault().is_none() {
            return ProcessingFailureReport::Pending;
        }
        match self.failure.take_report() {
            Some(record) => ProcessingFailureReport::Ready(format!(
                "CLAP {:?}: {}",
                record.operation,
                record.fault.describe_for("CLAP")
            )),
            None => ProcessingFailureReport::Reported,
        }
    }

    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        // For effect plugins: pass input audio through the CLAP process call
        let output = self.process_effect_block(buffer);
        let len = buffer.len().min(output.len());
        buffer[..len].copy_from_slice(&output[..len]);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let output = self.process_effect_block_with_transport(buffer, transport);
        let len = buffer.len().min(output.len());
        buffer[..len].copy_from_slice(&output[..len]);
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let output = self.process_effect_block_with_sidechain(buffer, Some(sidechain), transport);
        let len = buffer.len().min(output.len());
        buffer[..len].copy_from_slice(&output[..len]);
    }
    fn supports_sidechain(&self) -> bool {
        self.has_sidechain_input()
    }

    fn set_param(&mut self, name: &str, value: f64) {
        self.set_param_by_name(name, value);
    }
    fn queue_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        self.queue_param_by_name(name, value, sample_offset)
    }
    fn supports_param_offsets(&self) -> bool {
        true
    }
    fn max_block_frames(&self) -> u32 {
        self.max_frames.max(1)
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.get_param_by_name(name)
    }
    fn reset(&mut self) {}
    fn end_processing(&mut self) {
        self.stop_processing_now();
    }
    fn latency_samples(&self) -> u32 {
        self.prepared_latency
    }
    fn tail_samples(&self) -> u32 {
        ClapInstance::tail_samples(self)
    }
    fn plugin_tail_samples(&self) -> Option<u32> {
        ClapInstance::plugin_tail_samples(self)
    }
    fn name(&self) -> &str {
        "CLAP Plugin"
    }
}

fn clap_note_expression_id(expression: &str) -> Option<i32> {
    match normalize_expression_name(expression).as_str() {
        "volume" => Some(CLAP_NOTE_EXPRESSION_VOLUME),
        "pan" => Some(CLAP_NOTE_EXPRESSION_PAN),
        "tuning" | "pitch" => Some(CLAP_NOTE_EXPRESSION_TUNING),
        "vibrato" => Some(CLAP_NOTE_EXPRESSION_VIBRATO),
        "expression" => Some(CLAP_NOTE_EXPRESSION_EXPRESSION),
        "brightness" | "timbre" => Some(CLAP_NOTE_EXPRESSION_BRIGHTNESS),
        "pressure" | "aftertouch" => Some(CLAP_NOTE_EXPRESSION_PRESSURE),
        _ => None,
    }
}

fn normalize_expression_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
fn interleave_output_pairs(
    planar: &[Vec<f32>],
    output_pairs: &[usize],
    num_frames: usize,
) -> Vec<Vec<f32>> {
    let mut pairs: Vec<Vec<f32>> = output_pairs
        .iter()
        .map(|_| Vec::with_capacity(num_frames.saturating_mul(2)))
        .collect();
    interleave_output_pairs_into(planar, output_pairs, num_frames, &mut pairs);
    pairs
}

/// Append each requested pair's interleaved samples to a reserved buffer.
fn interleave_output_pairs_into(
    planar: &[Vec<f32>],
    output_pairs: &[usize],
    num_frames: usize,
    pairs: &mut [Vec<f32>],
) {
    let silence = vec![0.0f32; num_frames];
    for (interleaved, &pair) in pairs.iter_mut().zip(output_pairs) {
        let left = pair.saturating_mul(2);
        let right = left.saturating_add(1);
        let out_l = planar.get(left).unwrap_or(&silence);
        let out_r = planar.get(right).unwrap_or(&silence);
        for index in 0..num_frames {
            interleaved.push(out_l[index]);
            interleaved.push(out_r[index]);
        }
    }
}

// ============================================================================
// Host callbacks
// ============================================================================

/// Static clap_host_params vtable — tells the plugin we support param notifications.
static HOST_PARAMS: clap_sys::ext::params::clap_host_params =
    clap_sys::ext::params::clap_host_params {
        rescan: Some(host_params_rescan),
        clear: Some(host_params_clear),
        request_flush: Some(host_params_request_flush),
    };

thread_local! {
    /// Identity of the calling thread without allocating or locking.
    static AUDIO_THREAD_MARKER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn audio_thread_marker() -> *mut std::ffi::c_void {
    AUDIO_THREAD_MARKER.with(|marker| marker as *const _ as *mut std::ffi::c_void)
}

/// Static clap_host_thread_check vtable — CLAP's main-thread and audio-thread
/// are symbolic; the audio thread is the one that called start_processing.
static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(host_is_main_thread),
    is_audio_thread: Some(host_is_audio_thread),
};

/// Static clap_host_gui vtable — lets the plugin notify us when its GUI closes.
static HOST_GUI: clap_sys::ext::gui::clap_host_gui = clap_sys::ext::gui::clap_host_gui {
    resize_hints_changed: None,
    request_resize: Some(host_gui_request_resize),
    request_show: None,
    request_hide: None,
    closed: Some(host_gui_closed),
};

/// # Safety
/// Non-null host and host_data must belong to a live instance for the borrow.
unsafe fn callback_fault<'a>(host: *const clap_host) -> Option<&'a ClapFailure> {
    if host.is_null() {
        return None;
    }
    // SAFETY: the callback caller retains the instance-owned host descriptor.
    let data = unsafe { (*host).host_data } as *const ClapHostData;
    if data.is_null() {
        return None;
    }
    // SAFETY: host_data points to the boxed callback storage for that instance.
    Some(unsafe { &(*data).failure })
}

unsafe extern "C" fn host_get_extension(
    host: *const clap_host,
    extension_id: *const i8,
) -> *const std::ffi::c_void {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, ptr::null(), || {
        if extension_id.is_null() {
            return ptr::null();
        }
        // SAFETY: CLAP get_extension passes a readable NUL-terminated identifier for the duration of this callback; it is not retained afterward.
        let id = unsafe { std::ffi::CStr::from_ptr(extension_id) };
        if id == clap_sys::ext::params::CLAP_EXT_PARAMS {
            &HOST_PARAMS as *const _ as *const std::ffi::c_void
        } else if id == clap_sys::ext::gui::CLAP_EXT_GUI {
            &HOST_GUI as *const _ as *const std::ffi::c_void
        } else if id == CLAP_EXT_THREAD_CHECK {
            &HOST_THREAD_CHECK as *const _ as *const std::ffi::c_void
        } else {
            ptr::null()
        }
    })
}

unsafe extern "C" fn host_is_main_thread(host: *const clap_host) -> bool {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, false, || {
        if host.is_null() {
            return false;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if host_data.is_null() {
            return false;
        }
        // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
        unsafe { &*host_data }.main_thread == std::thread::current().id()
    })
}

unsafe extern "C" fn host_is_audio_thread(host: *const clap_host) -> bool {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, false, || {
        if host.is_null() {
            return false;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if host_data.is_null() {
            return false;
        }
        // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
        let audio_thread = unsafe { &*host_data }
            .audio_thread
            .load(std::sync::atomic::Ordering::Relaxed);
        !audio_thread.is_null() && audio_thread == audio_thread_marker()
    })
}

unsafe extern "C" fn host_request_restart(host: *const clap_host) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // Plugin requests deactivate + reactivate cycle.
        // Set the restart flag; caller should check needs_restart() before next process.
        if host.is_null() {
            return;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if !host_data.is_null() {
            // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
            unsafe { &*host_data }
                .restart_requested
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
}
unsafe extern "C" fn host_request_process(host: *const clap_host) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {})
}

unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // The main loop services this request independently of parameter flushing.
        if host.is_null() {
            return;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if !host_data.is_null() {
            // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
            unsafe { &*host_data }
                .main_callback_pending
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
}

unsafe extern "C" fn host_params_rescan(
    host: *const clap_host,
    _flags: clap_sys::ext::params::clap_param_rescan_flags,
) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // The plugin changed its parameter layout; drop the name lookup.
        if host.is_null() {
            return;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if !host_data.is_null() {
            // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
            unsafe { &*host_data }
                .params_changed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
}

unsafe extern "C" fn host_params_clear(
    host: *const clap_host,
    _param_id: clap_sys::id::clap_id,
    _flags: clap_sys::ext::params::clap_param_clear_flags,
) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // Plugin wants to clear automation/modulation for a param
    })
}

unsafe extern "C" fn host_params_request_flush(host: *const clap_host) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // Plugin wants us to call flush() so it can send param change events
        if host.is_null() {
            return;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if !host_data.is_null() {
            // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
            unsafe { &*host_data }
                .flush_requested
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
}

// --- clap_host_gui callbacks ---

unsafe extern "C" fn host_gui_request_resize(
    host: *const clap_host,
    _width: u32,
    _height: u32,
) -> bool {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, false, || {
        // Allow resize
        true
    })
}

unsafe extern "C" fn host_gui_closed(host: *const clap_host, _was_destroyed: bool) {
    // SAFETY: CLAP retains host data throughout each host callback.
    let failure = unsafe { callback_fault(host) };
    clap_guard::callback_tracked(failure, (), || {
        // Plugin's GUI was closed (user clicked X or plugin closed it)
        if host.is_null() {
            return;
        }
        // SAFETY: The non-null host descriptor belongs to the live Core, which retains its boxed callback context until plugin destruction completes.
        let host_data = unsafe { (*host).host_data } as *const ClapHostData;
        if !host_data.is_null() {
            // SAFETY: this callback retains the host data until it returns.
            unsafe { &*host_data }
                .gui_close_pending
                .store(true, std::sync::atomic::Ordering::Release);
            // SAFETY: The callback checks this context for null; the Core keeps its boxed host data at a stable address for the entire native callback lifetime.
            unsafe { &*host_data }
                .gui_closed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        log::debug!("[clap host] GUI closed callback");
    })
}

// Event list wrapper for callbacks
struct EventList<'a> {
    failure: &'a ClapFailure,
    events: &'a [*const clap_event_header],
}

unsafe extern "C" fn input_events_size(list: *const clap_input_events) -> u32 {
    let failure = if list.is_null() {
        None
    } else {
        // SAFETY: event lists and their borrowed context live through process/flush.
        unsafe { ((*list).ctx as *const EventList<'_>).as_ref() }.map(|list| list.failure)
    };
    clap_guard::callback_tracked(failure, 0, || {
        // SAFETY: The host creates this event-list descriptor from a live EventList and lends it only during the synchronous process/flush callback.
        let ctx = unsafe { (*list).ctx } as *const EventList;
        // SAFETY: The event-list context points to the host-owned EventList retained throughout the synchronous native call; the plugin only receives borrowed event addresses.
        let events = unsafe { &(*ctx).events };
        events.len() as u32
    })
}

unsafe extern "C" fn input_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    let failure = if list.is_null() {
        None
    } else {
        // SAFETY: event lists and their borrowed context live through process/flush.
        unsafe { ((*list).ctx as *const EventList<'_>).as_ref() }.map(|list| list.failure)
    };
    clap_guard::callback_tracked(failure, ptr::null(), || {
        // SAFETY: The host creates this event-list descriptor from a live EventList and lends it only during the synchronous process/flush callback.
        let ctx = unsafe { (*list).ctx } as *const EventList;
        // SAFETY: The event-list context points to the host-owned EventList retained throughout the synchronous native call; the plugin only receives borrowed event addresses.
        let events = unsafe { &(*ctx).events };
        if (index as usize) < events.len() {
            events[index as usize]
        } else {
            ptr::null()
        }
    })
}

unsafe extern "C" fn output_events_push(
    _list: *const clap_output_events,
    _event: *const clap_event_header,
) -> bool {
    let failure = None;
    clap_guard::callback_tracked(failure, false, || {
        // Accept output events (param changes, etc.) — we re-read values via get_params()
        true
    })
}

fn normalize_param_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ClapStateInputStream, ClapStateOutputStream, ParamEvent, clap_transport_event,
        interleave_output_pairs, normalize_param_name, param_event_to_clap,
    };
    use crate::PluginTransport;
    use clap_sys::events::{
        CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
        CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_PLAYING,
    };
    use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};

    #[test]
    fn fans_out_multiple_output_pairs_in_requested_order() {
        let outputs = vec![
            vec![1.0, 2.0],
            vec![3.0, 4.0],
            vec![5.0, 6.0],
            vec![7.0, 8.0],
        ];

        assert_eq!(
            interleave_output_pairs(&outputs, &[1, 0, 9], 2),
            vec![
                vec![5.0, 7.0, 6.0, 8.0],
                vec![1.0, 3.0, 2.0, 4.0],
                vec![0.0; 4],
            ]
        );
    }

    #[test]
    fn appends_pair_fan_out_into_reserved_buffers() {
        let planar = vec![vec![1.0, 2.0], vec![3.0, 4.0], vec![5.0, 6.0]];
        let mut pairs = vec![Vec::with_capacity(4), Vec::new()];
        super::interleave_output_pairs_into(&planar, &[0, 3], 2, &mut pairs);
        assert_eq!(pairs[0], vec![1.0, 3.0, 2.0, 4.0]);
        assert_eq!(pairs[1], vec![0.0; 4]);
    }

    #[test]
    fn normalizes_plugin_param_names() {
        assert_eq!(normalize_param_name("Filter 1 Cutoff"), "filter1cutoff");
        assert_eq!(normalize_param_name("filter1_cutoff"), "filter1cutoff");
        assert_eq!(normalize_param_name("Osc-1/Level"), "osc1level");
    }

    #[test]
    fn queued_parameter_event_preserves_sample_offset_and_target() {
        let event = param_event_to_clap(ParamEvent {
            id: 42,
            value: 0.75,
            sample_offset: 127,
        });
        assert_eq!(event.header.time, 127);
        assert_eq!(event.header.type_, clap_sys::events::CLAP_EVENT_PARAM_VALUE);
        assert_eq!(event.param_id, 42);
        assert_eq!(event.value, 0.75);
        assert_eq!(event.note_id, -1);
    }

    #[test]
    fn clap_transport_event_reports_tempo_and_position() {
        let transport = clap_transport_event(PluginTransport::new(44_100, 44_100, 120.0));

        assert_ne!(transport.flags & CLAP_TRANSPORT_IS_PLAYING, 0);
        assert_ne!(transport.flags & CLAP_TRANSPORT_HAS_TEMPO, 0);
        assert_ne!(transport.flags & CLAP_TRANSPORT_HAS_BEATS_TIMELINE, 0);
        assert_ne!(transport.flags & CLAP_TRANSPORT_HAS_SECONDS_TIMELINE, 0);
        assert_ne!(transport.flags & CLAP_TRANSPORT_HAS_TIME_SIGNATURE, 0);
        assert_eq!(transport.tempo, 120.0);
        assert_eq!(transport.song_pos_beats, 2 * CLAP_BEATTIME_FACTOR);
        assert_eq!(transport.song_pos_seconds, CLAP_SECTIME_FACTOR);
        assert_eq!(transport.bar_start, 0);
        assert_eq!(transport.bar_number, 0);
        assert_eq!(transport.tsig_num, 4);
        assert_eq!(transport.tsig_denom, 4);
    }

    #[test]
    fn clap_mapped_context_retains_meter_origin_and_integrated_ppq() {
        let transport = clap_transport_event(crate::mapped_transport_fixture().at_sample(312000));
        assert_eq!(transport.tempo, 60.0);
        assert_eq!(
            transport.song_pos_beats,
            (7.5 * CLAP_BEATTIME_FACTOR as f64) as i64
        );
        assert_eq!(transport.bar_start, 7 * CLAP_BEATTIME_FACTOR);
        assert_eq!(transport.bar_number, 2);
        assert_eq!((transport.tsig_num, transport.tsig_denom), (3, 4));
    }

    #[test]
    fn split_effect_blocks_preserve_pending_parameter_offsets() {
        let mut pending = vec![0, 31, 32, 63, 64]
            .into_iter()
            .map(|sample_offset| {
                super::ClapPendingEvent::Param(ParamEvent {
                    id: 1,
                    value: 0.5,
                    sample_offset,
                })
            })
            .collect();
        for (start, expected) in [(0, vec![0, 31]), (32, vec![0, 31]), (64, vec![0])] {
            let mut events = super::pending_events_for_span(&mut pending, start, 32);
            let offsets: Vec<_> = events
                .iter_mut()
                .map(|event| *event.sample_offset_mut())
                .collect();
            assert_eq!(offsets, expected);
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn clap_state_input_stream_reads_bytes_in_chunks() {
        let mut state = ClapStateInputStream::new(b"abcdef");
        let stream = state.as_stream();
        let mut first = [0u8; 4];
        let mut second = [0u8; 4];

        let read = stream.read.unwrap();
        // SAFETY: The local stream context outlives this call and the writable array has exactly the supplied byte capacity.
        let first_len = unsafe { read(&stream, first.as_mut_ptr() as *mut _, first.len() as u64) };
        let second_len =
            // SAFETY: The local stream context outlives this call and the writable array has exactly the supplied byte capacity.
            unsafe { read(&stream, second.as_mut_ptr() as *mut _, second.len() as u64) };
        // SAFETY: The local stream context outlives this call and the writable array has exactly the supplied byte capacity.
        let eof_len = unsafe { read(&stream, second.as_mut_ptr() as *mut _, second.len() as u64) };

        assert_eq!(first_len, 4);
        assert_eq!(&first, b"abcd");
        assert_eq!(second_len, 2);
        assert_eq!(&second[..2], b"ef");
        assert_eq!(eof_len, 0);
    }

    #[test]
    fn clap_state_output_stream_collects_bytes() {
        let mut state = ClapStateOutputStream::new();
        let stream = state.as_stream();
        let write = stream.write.unwrap();

        // SAFETY: The local output-stream context outlives this call and the string literal provides the supplied readable byte count.
        let first_len = unsafe { write(&stream, b"abc".as_ptr() as *const _, 3) };
        // SAFETY: The local output-stream context outlives this call and the string literal provides the supplied readable byte count.
        let second_len = unsafe { write(&stream, b"def".as_ptr() as *const _, 3) };

        assert_eq!(first_len, 3);
        assert_eq!(second_len, 3);
        assert_eq!(state.data, b"abcdef");
    }
}

#[cfg(test)]
mod audio_port_contract_tests {
    use super::*;
    use clap_sys::process::CLAP_PROCESS_CONTINUE;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::AtomicBool;

    /// One parameter the fake plugin declares.
    struct FakeParam {
        id: u32,
        name: &'static str,
        module: &'static str,
        min: f64,
        max: f64,
        flags: u32,
    }

    /// A fake effect that records the audio buffers the host hands to
    /// `process`. Its only contract is CLAP's: one buffer per declared port.
    #[repr(C)]
    struct FakeEffect {
        plugin: clap_plugin,
        inputs: Vec<u32>,
        outputs: Vec<u32>,
        seen_inputs: RefCell<Vec<u32>>,
        seen_outputs: RefCell<Vec<u32>>,
        processed: RefCell<usize>,
        params: Vec<FakeParam>,
        /// `get_info` calls, so a test can prove a name lookup is cached.
        param_info_calls: Cell<usize>,
        value: Cell<f64>,
        flushes: Cell<usize>,
        authored_writes: RefCell<Vec<(u32, f64)>>,
        rescan_on_flush: Cell<bool>,
        host: Cell<*const clap_host>,
        throw_on_flush: Cell<bool>,
        no_flush: Cell<bool>,
        gain_audio: Cell<bool>,
        reject_info: Cell<bool>,
    }

    #[link(name = "oc_clap_guard_fixture", kind = "static")]
    unsafe extern "C" {
        fn oc_clap_fixture_reset(point: u32);
        fn oc_clap_fixture_count() -> u32;
        fn oc_clap_fixture_event(index: u32) -> u32;
        fn oc_clap_fixture_authored_flush(
            plugin: *const clap_plugin,
            input: *const clap_input_events,
            output: *const clap_output_events,
        );
    }

    static THROWING_PARAMS: clap_sys::ext::params::clap_plugin_params =
        clap_sys::ext::params::clap_plugin_params {
            count: Some(fake_params_count),
            get_info: Some(fake_params_get_info),
            get_value: Some(fake_param_value),
            value_to_text: None,
            text_to_value: None,
            flush: Some(oc_clap_fixture_authored_flush),
        };

    static NO_FLUSH_PARAMS: clap_sys::ext::params::clap_plugin_params =
        clap_sys::ext::params::clap_plugin_params {
            count: Some(fake_params_count),
            get_info: Some(fake_params_get_info),
            get_value: Some(fake_param_value),
            value_to_text: None,
            text_to_value: None,
            flush: None,
        };

    static FAKE_PARAMS: clap_sys::ext::params::clap_plugin_params =
        clap_sys::ext::params::clap_plugin_params {
            count: Some(fake_params_count),
            get_info: Some(fake_params_get_info),
            get_value: Some(fake_param_value),
            value_to_text: None,
            text_to_value: None,
            flush: Some(fake_param_flush),
        };

    unsafe extern "C" fn fake_param_value(
        plugin: *const clap_plugin,
        id: u32,
        value: *mut f64,
    ) -> bool {
        // SAFETY: the fixture retains the plugin and writable value for this callback.
        let effect = unsafe { fake(plugin) };
        if id != 7 || value.is_null() {
            return false;
        }
        // SAFETY: the guarded native caller provided writable output storage.
        unsafe { *value = effect.value.get() };
        true
    }
    unsafe extern "C" fn fake_param_flush(
        plugin: *const clap_plugin,
        input: *const clap_input_events,
        _: *const clap_output_events,
    ) {
        // SAFETY: The fixture retains this plugin allocation throughout flush.
        let effect = unsafe { fake(plugin) };
        effect.flushes.set(effect.flushes.get() + 1);
        if effect.rescan_on_flush.get() {
            // SAFETY: fake_instance installs the retained Core's host pointer;
            // flush runs synchronously while both plugin and host remain live.
            unsafe {
                host_params_rescan(
                    effect.host.get(),
                    clap_sys::ext::params::CLAP_PARAM_RESCAN_ALL,
                )
            };
        }
        if input.is_null() {
            return;
        }
        // SAFETY: the production caller borrows this event list for the callback.
        let input = unsafe { &*input };
        let (Some(size), Some(get)) = (input.size, input.get) else {
            return;
        };
        // SAFETY: the input functions and context belong to the borrowed list.
        for index in 0..unsafe { size(input) } {
            // SAFETY: index is in the declared list range.
            let event = unsafe { get(input, index) };
            if event.is_null() {
                continue;
            }
            // SAFETY: the fixture only interprets correctly sized parameter events.
            let header = unsafe { &*event };
            if header.type_ == CLAP_EVENT_PARAM_VALUE
                && header.size as usize >= std::mem::size_of::<clap_event_param_value>()
            {
                // SAFETY: the header declares this event's exact native record layout.
                let event = unsafe { &*event.cast::<clap_event_param_value>() };
                effect
                    .authored_writes
                    .borrow_mut()
                    .push((event.param_id, event.value));
                if event.param_id == 7 {
                    effect.value.set(event.value);
                }
            }
        }
    }

    unsafe extern "C" fn fake_params_count(plugin: *const clap_plugin) -> u32 {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        unsafe { fake(plugin) }.params.len() as u32
    }

    unsafe extern "C" fn fake_params_get_info(
        plugin: *const clap_plugin,
        index: u32,
        info: *mut clap_sys::ext::params::clap_param_info,
    ) -> bool {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let effect = unsafe { fake(plugin) };
        if effect.reject_info.get() {
            return false;
        }
        effect
            .param_info_calls
            .set(effect.param_info_calls.get() + 1);
        let Some(param) = effect.params.get(index as usize) else {
            return false;
        };
        // SAFETY: The host supplies exclusive writable storage for one CLAP metadata record for this synchronous get_info/get_port call.
        let info = unsafe { &mut *info };
        info.id = param.id;
        info.flags = param.flags;
        write_c_name(&mut info.name, param.name);
        write_c_name(&mut info.module, param.module);
        info.min_value = param.min;
        info.max_value = param.max;
        info.default_value = (param.min + param.max) * 0.5;
        true
    }

    fn write_c_name<const N: usize>(target: &mut [std::ffi::c_char; N], value: &str) {
        let bytes = value.as_bytes();
        let length = bytes.len().min(N - 1);
        for (slot, byte) in target.iter_mut().zip(bytes.iter().take(length)) {
            *slot = *byte as std::ffi::c_char;
        }
        target[length] = 0;
    }

    unsafe fn fake<'a>(plugin: *const clap_plugin) -> &'a FakeEffect {
        // SAFETY: The test fixture sets plugin_data to its live FakeEffect allocation before callbacks and retains it until the descriptor is destroyed.
        unsafe { &*((*plugin).plugin_data as *const FakeEffect) }
    }

    static FAKE_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
        count: Some(fake_ports_count),
        get: Some(fake_ports_get),
    };

    unsafe extern "C" fn fake_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let effect = unsafe { fake(plugin) };
        if is_input {
            effect.inputs.len() as u32
        } else {
            effect.outputs.len() as u32
        }
    }

    unsafe extern "C" fn fake_ports_get(
        plugin: *const clap_plugin,
        index: u32,
        is_input: bool,
        info: *mut clap_audio_port_info,
    ) -> bool {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let effect = unsafe { fake(plugin) };
        let ports = if is_input {
            &effect.inputs
        } else {
            &effect.outputs
        };
        let Some(channels) = ports.get(index as usize) else {
            return false;
        };
        // SAFETY: The host supplies exclusive writable storage for one CLAP metadata record for this synchronous get_info/get_port call.
        let info = unsafe { &mut *info };
        info.id = index;
        info.name[0] = 0;
        info.flags = if index == 0 {
            CLAP_AUDIO_PORT_IS_MAIN
        } else {
            0
        };
        info.channel_count = *channels;
        info.port_type = ptr::null();
        info.in_place_pair = u32::MAX;
        true
    }

    unsafe extern "C" fn fake_init(_plugin: *const clap_plugin) -> bool {
        true
    }

    unsafe extern "C" fn fake_activate(
        _plugin: *const clap_plugin,
        _sample_rate: f64,
        _min_frames: u32,
        _max_frames: u32,
    ) -> bool {
        true
    }

    unsafe extern "C" fn fake_deactivate(_plugin: *const clap_plugin) {}

    unsafe extern "C" fn fake_start(_plugin: *const clap_plugin) -> bool {
        true
    }

    unsafe extern "C" fn fake_stop(_plugin: *const clap_plugin) {}

    unsafe extern "C" fn fake_reset(_plugin: *const clap_plugin) {}

    unsafe extern "C" fn fake_process(
        plugin: *const clap_plugin,
        process: *const clap_process,
    ) -> i32 {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let effect = unsafe { fake(plugin) };
        // SAFETY: The host lends this process descriptor and its counted channel arrays only for the synchronous fixture process callback.
        let process = unsafe { &*process };
        *effect.seen_inputs.borrow_mut() = (0..process.audio_inputs_count)
            // SAFETY: The fixture indexes only below the process descriptor's supplied input/output count; the host retains these audio-port arrays throughout process.
            .map(|index| unsafe { (*process.audio_inputs.add(index as usize)).channel_count })
            .collect();
        *effect.seen_outputs.borrow_mut() = (0..process.audio_outputs_count)
            // SAFETY: The fixture indexes only below the process descriptor's supplied input/output count; the host retains these audio-port arrays throughout process.
            .map(|index| unsafe { (*process.audio_outputs.add(index as usize)).channel_count })
            .collect();
        *effect.processed.borrow_mut() += 1;

        if process.audio_inputs_count == 0 || process.audio_outputs_count == 0 {
            return CLAP_PROCESS_CONTINUE;
        }
        // SAFETY: The host lends this process descriptor and its counted channel arrays only for the synchronous fixture process callback.
        let input = unsafe { &*process.audio_inputs };
        // SAFETY: The host lends this process descriptor and its counted channel arrays only for the synchronous fixture process callback.
        let output = unsafe { &*process.audio_outputs };
        for channel in 0..output.channel_count {
            let source = if input.channel_count == 0 {
                ptr::null()
            } else {
                // SAFETY: The fixture bounds the channel by the supplied channel count; host-owned data32 arrays and their frames remain live during process.
                unsafe {
                    *input
                        .data32
                        .add(channel.min(input.channel_count - 1) as usize)
                }
            };
            // SAFETY: The fixture bounds the channel by the supplied channel count; host-owned data32 arrays and their frames remain live during process.
            let target = unsafe { *output.data32.add(channel as usize) };
            if source.is_null() || target.is_null() {
                continue;
            }
            for frame in 0..process.frames_count as usize {
                // SAFETY: Both non-null channel pointers refer to frames_count samples supplied by the host; the frame loop stays within those disjoint input/output buffers.
                unsafe {
                    *target.add(frame) = *source.add(frame)
                        * if effect.gain_audio.get() {
                            effect.value.get() as f32
                        } else {
                            1.0
                        };
                };
            }
        }
        CLAP_PROCESS_CONTINUE
    }

    unsafe extern "C" fn fake_get_extension(
        plugin: *const clap_plugin,
        id: *const std::ffi::c_char,
    ) -> *const std::ffi::c_void {
        // SAFETY: CLAP get_extension passes a readable NUL-terminated identifier for the duration of this callback; it is not retained afterward.
        let id = unsafe { std::ffi::CStr::from_ptr(id) };
        if id == CLAP_EXT_AUDIO_PORTS {
            &FAKE_PORTS as *const _ as *const std::ffi::c_void
        } else if id == clap_sys::ext::params::CLAP_EXT_PARAMS {
            // SAFETY: This callback borrows its still-live fixture descriptor.
            let effect = unsafe { fake(plugin) };
            if effect.no_flush.get() {
                &NO_FLUSH_PARAMS as *const _ as *const std::ffi::c_void
            } else if effect.throw_on_flush.get() {
                &THROWING_PARAMS as *const _ as *const std::ffi::c_void
            } else {
                &FAKE_PARAMS as *const _ as *const std::ffi::c_void
            }
        } else {
            ptr::null()
        }
    }

    unsafe extern "C" fn fake_destroy(plugin: *const clap_plugin) {
        // SAFETY: fake_instance allocated this FakeEffect with Box::into_raw, with its plugin as the first field; destroy consumes that unique allocation exactly once.
        drop(unsafe { Box::from_raw(plugin as *mut FakeEffect) });
    }

    fn fake_instance(inputs: Vec<u32>, outputs: Vec<u32>) -> ClapInstance {
        let vtable = clap_plugin {
            desc: ptr::null(),
            plugin_data: ptr::null_mut(),
            init: Some(fake_init),
            destroy: Some(fake_destroy),
            activate: Some(fake_activate),
            deactivate: Some(fake_deactivate),
            start_processing: Some(fake_start),
            stop_processing: Some(fake_stop),
            reset: Some(fake_reset),
            process: Some(fake_process),
            get_extension: Some(fake_get_extension),
            on_main_thread: None,
        };
        let mut effect = Box::new(FakeEffect {
            plugin: vtable,
            inputs,
            outputs,
            seen_inputs: RefCell::new(Vec::new()),
            seen_outputs: RefCell::new(Vec::new()),
            processed: RefCell::new(0),
            params: vec![FakeParam {
                id: 7,
                name: "Gain",
                module: "",
                min: 0.0,
                max: 1.0,
                flags: clap_sys::ext::params::CLAP_PARAM_IS_AUTOMATABLE,
            }],
            param_info_calls: Cell::new(0),
            value: Cell::new(0.5),
            flushes: Cell::new(0),
            authored_writes: RefCell::new(Vec::new()),
            rescan_on_flush: Cell::new(false),
            host: Cell::new(ptr::null()),
            throw_on_flush: Cell::new(false),
            no_flush: Cell::new(false),
            gain_audio: Cell::new(false),
            reject_info: Cell::new(false),
        });
        let effect_ptr = &mut *effect as *mut FakeEffect;
        effect.plugin.plugin_data = effect_ptr as *mut _;
        let plugin = Box::into_raw(effect) as *const clap_plugin;

        let failure = Arc::new(ClapFailure::default());
        let mut host_data = Box::new(ClapHostData {
            failure: Arc::clone(&failure),
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: c"Test".as_ptr(),
                vendor: c"Test".as_ptr(),
                url: c"test".as_ptr(),
                version: c"1".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            main_thread: std::thread::current().id(),
            audio_thread: std::sync::atomic::AtomicPtr::new(ptr::null_mut()),
            audio_busy: std::sync::atomic::AtomicBool::new(false),
            main_callback_pending: std::sync::atomic::AtomicBool::new(false),
            gui_close_pending: std::sync::atomic::AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            params_changed: AtomicBool::new(false),
        });
        host_data.host.host_data = &*host_data as *const ClapHostData as *mut _;

        let gui_session = Arc::new(Mutex::new(GuiSession {
            failure: Arc::clone(&failure),
            ..GuiSession::default()
        }));
        let mut instance = ClapInstance {
            plugin,
            bundle_path: std::path::PathBuf::from("test.clap"),
            plugin_id: "test.effect".into(),
            core: ClapCore::new(
                plugin,
                host_data,
                Arc::clone(&failure),
                Arc::clone(&gui_session),
                None,
            ),
            failure: Arc::clone(&failure),
            sample_rate: 48_000,
            max_frames: 512,
            active: false,
            processing: false,
            audio_ports: AudioPortLayout::default(),
            sidechain_input_index: 0,
            pending_events: Vec::new(),
            event_scratch: Vec::new(),
            event_header_scratch: Vec::new(),
            gui_session,
            param_cache: ParamCache::default(),
            prepared_tail: None,
            prepared_latency: 0,
            prepared_gui: 0,
            prepared_midi1: false,
            prepared_parameters: Vec::new(),
        };
        // SAFETY: The retained Core's boxed host keeps a stable address until
        // plugin destruction; the fixture uses it only during native flush.
        unsafe { fake(instance.plugin) }
            .host
            .set(&instance.core.host.host);
        instance.audio_ports = instance.read_audio_port_layout();
        instance.prepared_parameters = instance.core.refresh_parameters().unwrap();
        instance.activate().unwrap();
        instance
    }

    #[path = "authored_parameter_tests.rs"]
    mod authored_parameter_tests;

    fn recorded(instance: &ClapInstance) -> (Vec<u32>, Vec<u32>, usize) {
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let effect = unsafe { fake(instance.plugin) };
        (
            effect.seen_inputs.borrow().clone(),
            effect.seen_outputs.borrow().clone(),
            *effect.processed.borrow(),
        )
    }

    #[test]
    fn declared_sidechain_port_gets_a_buffer_when_no_key_is_connected() {
        let mut instance = fake_instance(vec![2, 2], vec![2]);
        let input: Vec<f32> = (0..64).map(|frame| frame as f32 * 0.01).collect();
        let output = instance.process_effect_block(&input);
        assert_eq!(output, input, "main path must stay intact");
        let (inputs, outputs, processed) = recorded(&instance);
        assert_eq!(inputs, vec![2, 2]);
        assert_eq!(outputs, vec![2]);
        assert_eq!(processed, 1);
    }

    #[test]
    fn every_declared_output_port_gets_a_buffer() {
        let mut instance = fake_instance(vec![2], vec![2, 2, 2]);
        let input = vec![0.25f32; 128];
        let output = instance.process_effect_block(&input);
        assert_eq!(output, input);
        let (inputs, outputs, _) = recorded(&instance);
        assert_eq!(inputs, vec![2]);
        assert_eq!(outputs, vec![2, 2, 2]);
    }

    #[test]
    fn mono_ports_are_declared_with_one_channel() {
        let mut instance = fake_instance(vec![1], vec![1]);
        let mut input = vec![0.0f32; 128];
        for frame in 0..64 {
            input[frame * 2] = 1.0;
        }
        let output = instance.process_effect_block(&input);
        let (inputs, outputs, _) = recorded(&instance);
        assert_eq!(inputs, vec![1]);
        assert_eq!(outputs, vec![1]);
        assert!((output[0] - 0.5).abs() < 1e-6);
        assert!((output[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn processing_starts_on_the_thread_that_first_processes() {
        let mut instance = fake_instance(vec![2], vec![2]);
        let host = &instance.core.host.host as *const clap_host;
        assert!(!instance.is_processing());
        // SAFETY: The test owns the Core/host descriptor throughout this synchronous thread-role query; no native owner is retired until afterward.
        assert!(unsafe { host_is_main_thread(host) });
        // SAFETY: The test owns the Core/host descriptor throughout this synchronous thread-role query; no native owner is retired until afterward.
        assert!(!unsafe { host_is_audio_thread(host) });

        let input = vec![0.0f32; 64];
        let output = instance.process_effect_block(&input);
        assert_eq!(output, input);
        assert!(instance.is_processing());
        // SAFETY: The test owns the Core/host descriptor throughout this synchronous thread-role query; no native owner is retired until afterward.
        assert!(!unsafe { host_is_audio_thread(host) });

        instance.stop_processing_now();
        assert!(!instance.is_processing());
        // SAFETY: The test owns the Core/host descriptor throughout this synchronous thread-role query; no native owner is retired until afterward.
        assert!(!unsafe { host_is_audio_thread(host) });
    }

    /// A parameter write must not enumerate the parameter list again; a rescan
    /// is the only event that invalidates the name lookup.
    #[test]
    fn a_parameter_name_is_resolved_once_until_the_plugin_rescans() {
        let mut instance = fake_instance(vec![2], vec![2]);
        let calls =
            // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
            |instance: &ClapInstance| unsafe { fake(instance.plugin) }.param_info_calls.get();
        let before = calls(&instance);

        assert!(instance.queue_param_by_name("Gain", 0.5, 0));
        let built = calls(&instance);
        assert_eq!(
            built, before,
            "the prepared inventory builds the lookup without native enumeration"
        );

        for _ in 0..8 {
            assert!(instance.queue_param_normalized_by_name("Gain", 0.25, 0));
        }
        assert_eq!(calls(&instance), built, "writes reuse the cached lookup");

        instance
            .core
            .host
            .params_changed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(instance.queue_param_by_name("Gain", 0.5, 0));
        assert_eq!(calls(&instance), built + 1, "a rescan rebuilds the cache");
    }
    #[test]
    fn editor_controls_are_validated_queued_and_applied_on_main_before_readback() {
        let instance = fake_instance(vec![2], vec![2]);
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        let calls = unsafe { fake(instance.plugin) }.param_info_calls.get();
        assert_eq!(instance.queue_control_parameter("Gain", 0.8).unwrap(), 0.8);
        // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
        assert_eq!(unsafe { fake(instance.plugin) }.value.get(), 0.5);
        assert_eq!(
            // SAFETY: This test plugin descriptor points to its owning FakeEffect allocation; the test keeps that allocation live and invokes the fixture on its owning thread.
            unsafe { fake(instance.plugin) }.param_info_calls.get(),
            calls
        );
        assert!(instance.queue_control_parameter("unknown", 0.2).is_err());
        assert!(instance.queue_control_parameter("Gain", f64::NAN).is_err());
        let params = instance.parameter_read_plan().execute_on_main().unwrap();
        assert_eq!(params[0].current, 0.8);
        assert_eq!(instance.get_params()[0].current, 0.8);
        assert!(
            !instance
                .core
                .control_pending
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }
    #[test]
    fn prepared_editor_control_preserves_units_and_cannot_cross_owner_or_metadata() {
        let instance = fake_instance(vec![2], vec![2]);
        {
            let mut params = instance.core.parameters.lock().unwrap();
            params[0].min = -12.0;
            params[0].max = 12.0;
            params[0].default = 0.0;
            params[0].flags |= clap_sys::ext::params::CLAP_PARAM_IS_HIDDEN;
        }
        let prepared = instance
            .prepare_control_parameter_value("Gain", -6.0)
            .unwrap();
        assert_eq!(prepared.normalized_value(), 0.25);
        assert!(instance.core.controls.lock().unwrap().is_empty());
        // Current/display readback can change while the binding remains valid.
        instance.core.parameters.lock().unwrap()[0].current = 6.0;
        instance.queue_prepared_control_parameter(prepared).unwrap();
        assert_eq!(instance.core.controls.lock().unwrap()[0].1, -6.0);
        let other = fake_instance(vec![2], vec![2]);
        let prepared = instance
            .prepare_control_parameter_value("Gain", -3.0)
            .unwrap();
        assert!(other.queue_prepared_control_parameter(prepared).is_err());
        assert!(other.core.controls.lock().unwrap().is_empty());
        for field in 0..7 {
            let instance = fake_instance(vec![2], vec![2]);
            let prepared = instance
                .prepare_control_parameter_value("Gain", 0.75)
                .unwrap();
            {
                let mut params = instance.core.parameters.lock().unwrap();
                match field {
                    0 => params[0].id += 1,
                    1 => params[0].name = "Renamed".into(),
                    2 => params[0].min = -1.0,
                    3 => params[0].max = 2.0,
                    4 => params[0].flags ^= clap_sys::ext::params::CLAP_PARAM_IS_READONLY,
                    5 => params[0].module = "Changed".into(),
                    _ => params[0].default = 0.75,
                }
            }
            assert!(
                instance.queue_prepared_control_parameter(prepared).is_err(),
                "descriptor field {field}"
            );
            assert!(instance.core.controls.lock().unwrap().is_empty());
            assert!(
                !instance
                    .core
                    .control_pending
                    .load(std::sync::atomic::Ordering::Acquire)
            );
        }
    }
    #[test]
    fn prepared_editor_control_refuses_invalid_range_before_clamp_or_enqueue() {
        for (min, max) in [(f64::NAN, 1.0), (0.0, f64::INFINITY), (2.0, 1.0)] {
            let instance = fake_instance(vec![2], vec![2]);
            {
                let mut params = instance.core.parameters.lock().unwrap();
                params[0].min = min;
                params[0].max = max;
            }
            assert!(
                instance
                    .prepare_control_parameter_value("Gain", 0.5)
                    .is_err()
            );
            assert!(instance.core.controls.lock().unwrap().is_empty());
            assert!(
                !instance
                    .core
                    .control_pending
                    .load(std::sync::atomic::Ordering::Acquire)
            );
        }
        let instance = fake_instance(vec![2], vec![2]);
        instance.core.parameters.lock().unwrap()[0].flags |=
            clap_sys::ext::params::CLAP_PARAM_IS_READONLY;
        assert!(
            instance
                .prepare_control_parameter_value("Gain", 0.5)
                .is_err()
        );
        assert!(instance.core.controls.lock().unwrap().is_empty());
    }
    #[test]
    fn prepared_editor_control_rechecks_inventory_owner_and_queue_capacity() {
        for rejection in 0..3 {
            let mut instance = fake_instance(vec![2], vec![2]);
            let prepared = instance
                .prepare_control_parameter_value("Gain", 0.75)
                .unwrap();
            match rejection {
                0 => instance
                    .core
                    .host
                    .params_changed
                    .store(true, std::sync::atomic::Ordering::Release),
                1 => instance.processing = true,
                _ => instance
                    .core
                    .controls
                    .lock()
                    .unwrap()
                    .extend((0..256).map(|i| (100000 + i, 0.5))),
            }
            assert!(instance.queue_prepared_control_parameter(prepared).is_err());
            assert!(
                !instance
                    .core
                    .control_pending
                    .load(std::sync::atomic::Ordering::Acquire)
            );
            instance.processing = false;
        }
    }
}

#[cfg(test)]
#[path = "clap_guard_tests.rs"]
mod native_guard_tests;
