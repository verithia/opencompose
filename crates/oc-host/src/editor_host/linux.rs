//! X11 backend: a mapped top-level window driven by an `XNextEvent` loop.
//!
//! The plugin's view attaches to the window's XID as `X11EmbedWindowID`. The
//! loop is the process's only loop, so it serves the plugin's `IRunLoop` as
//! well: a timer a DPF editor registers here is what repaints it, and a file
//! descriptor it watches is what carries its own events.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, ITimerHandler,
    ITimerHandlerTrait,
};
use vst3::Steinberg::Vst::IEditControllerTrait;
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect,
    kPlatformTypeX11EmbedWindowID, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use crate::x11::{EditorWindow, Event};

/// How long the loop waits for a plugin timer or file descriptor before it
/// drains the X queue again.
const SERVE_SLICE: Duration = Duration::from_millis(10);

/// What the host frame and the event loop share for one attached editor.
#[derive(Default)]
struct FrameState {
    /// X display connection paired with `window`.
    display: AtomicUsize,
    /// XID the view is attached to; 0 before the attach and after the close.
    window: AtomicUsize,
    /// Set while `IPlugView::attached` runs: a resize request is deferred.
    attaching: AtomicBool,
    /// Size the window and the view last agreed on.
    size: Mutex<(i32, i32)>,
    /// Size requested during `attached`, applied once it returns.
    pending: Mutex<Option<(i32, i32)>>,
    /// Handlers the plugin registered through `IRunLoop`.
    run_loop: Mutex<RunLoop>,
}

/// Plugin run-loop registrations. Handlers are COM interface pointers kept as
/// integers so the state stays shareable; the plugin owns them until it
/// unregisters.
#[derive(Default)]
struct RunLoop {
    timers: Vec<(usize, Instant)>,
    fds: Vec<(usize, FileDescriptor)>,
}

impl FrameState {
    fn take_pending(&self) -> Option<(i32, i32)> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// Record `size` as the one the view knows. Returns the previous size so the
    /// caller only forwards a real change.
    fn replace_size(&self, size: (i32, i32)) -> (i32, i32) {
        let mut current = self.size.lock().unwrap_or_else(|error| error.into_inner());
        std::mem::replace(&mut *current, size)
    }
}

/// Host-side `IPlugFrame` for the editor window.
///
/// `IRunLoop` is part of the frame on Linux: a plugin queries it from the frame
/// and refuses to attach when the frame cannot serve one.
struct PlugFrame {
    state: Arc<FrameState>,
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kResultOk;
        }
        // SAFETY: VST3 guarantees a valid `ViewRect` for the duration of the call.
        let rect = unsafe { &*new_size };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return kResultOk;
        }
        if self.state.attaching.load(Ordering::Acquire) {
            *self
                .state
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((width, height));
            return kResultOk;
        }
        let display = self.state.display.load(Ordering::Acquire);
        let window = self.state.window.load(Ordering::Acquire);
        if window != 0 {
            crate::x11::resize(display as *mut c_void, window as u64, width, height);
        }
        self.state.replace_size((width, height));
        if !view.is_null() {
            // SAFETY: the caller passes its own live view and rect.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            let _ = unsafe { view.onSize(new_size) };
        }
        kResultOk
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl IRunLoopTrait for PlugFrame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        if handler.is_null() {
            return vst3::Steinberg::kInvalidArgument;
        }
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.fds.retain(|(known, _)| *known != handler as usize);
        entries.fds.push((handler as usize, fd));
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.fds.retain(|(known, _)| *known != handler as usize);
        kResultOk
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, milliseconds: u64) -> tresult {
        if handler.is_null() || milliseconds == 0 {
            return vst3::Steinberg::kInvalidArgument;
        }
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries
            .timers
            .retain(|(known, _)| *known != handler as usize);
        entries.timers.push((
            handler as usize,
            Instant::now() + Duration::from_millis(milliseconds),
        ));
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries
            .timers
            .retain(|(known, _)| *known != handler as usize);
        kResultOk
    }
}

/// Fire the plugin's due timers and readable file descriptors for at most
/// `slice`, then return so the caller can drain the X queue.
///
/// A plugin that asks for a repaint timer or watches a file descriptor needs
/// these callbacks to fire, so the editor cannot block in `XNextEvent`.
fn serve(state: &FrameState, slice: Duration) {
    let deadline = Instant::now() + slice;
    loop {
        if let Err(error) = super::pump_main() {
            super::fail(&error);
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let wait = deadline.saturating_duration_since(now);
        let (due, fds) = {
            let mut entries = state
                .run_loop
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let now = Instant::now();
            let mut due = Vec::new();
            entries.timers.retain(|(handler, at)| {
                if now >= *at {
                    due.push(*handler);
                    false
                } else {
                    true
                }
            });
            (due, entries.fds.clone())
        };
        for handler in due {
            // SAFETY: the plugin registered this handler and has not
            // unregistered it, so it is alive for the call.
            let handler = unsafe {
                ComRef::<ITimerHandler>::from_raw_unchecked(handler as *mut ITimerHandler)
            };
            unsafe { handler.onTimer() };
        }
        if fds.is_empty() {
            std::thread::sleep(wait.min(Duration::from_millis(2)));
            continue;
        }
        let mut poll_fds: Vec<libc::pollfd> = fds
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `poll_fds` is a valid slice of the reported length.
        let ready = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                wait.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready <= 0 {
            continue;
        }
        for (index, entry) in poll_fds.iter().enumerate() {
            if entry.revents == 0 {
                continue;
            }
            let (handler, fd) = fds[index];
            // SAFETY: the plugin registered this handler and has not
            // unregistered it, so it is alive for the call.
            let handler = unsafe {
                ComRef::<IEventHandler>::from_raw_unchecked(handler as *mut IEventHandler)
            };
            unsafe { handler.onFDIsSet(fd) };
        }
    }
}

/// Run the platform editor event loop.
pub fn run() {
    // The window closes through Xlib, but a plugin that installs its own exit
    // path still must not run its static destructors; see
    // [`crate::guard_plugin_teardown`].
    crate::guard_plugin_teardown();
    let config = match super::parse_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("oc-vst3-editor-host: {error}");
            crate::exit_process(2);
        }
    };
    let instance = match super::load_instance(&config) {
        Ok(instance) => instance,
        Err(error) => super::fail(&error),
    };
    if config.delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.delay_ms));
    }
    let controller = match instance.editor_controller() {
        Some(controller) => controller,
        None => super::fail("plugin has no edit controller"),
    };
    eprintln!("oc-vst3-editor-host: creating editor");
    // SAFETY: the controller is live and this is the thread that will own the view.
    let raw_view = unsafe { controller.createView(c"editor".as_ptr()) };
    let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
        super::fail("the plugin did not create an editor view");
    };
    // The editor must claim the X11 embedding platform before a host can
    // attach it; a Wayland-only editor reports the other platform type.
    // SAFETY: `view` is a live IPlugView.
    if unsafe { view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) } != kResultOk {
        super::fail("the editor does not support the X11EmbedWindowID platform");
    }
    let requested = super::view_size(&view).unwrap_or(super::DEFAULT_EDITOR_SIZE);
    let window = match EditorWindow::open_editor(&config.title, requested.0, requested.1) {
        Ok(window) => window,
        Err(error) => super::fail(&error),
    };
    let state = Arc::new(FrameState::default());
    state
        .display
        .store(window.display() as usize, Ordering::Release);
    let wrapper = ComWrapper::new(PlugFrame {
        state: Arc::clone(&state),
    });
    let Some(frame) = wrapper.to_com_ptr::<IPlugFrame>() else {
        super::fail("could not create the host IPlugFrame");
    };
    // SAFETY: `view` and `frame` are live, and the wrapper outlives the view.
    if unsafe { view.setFrame(frame.as_ptr()) } != kResultOk {
        super::fail("the editor rejected the host plug frame");
    }
    state.window.store(window.xid() as usize, Ordering::Release);
    state.attaching.store(true, Ordering::Release);
    // SAFETY: the window is mapped and stays alive until the loop ends.
    let attached =
        unsafe { view.attached(window.xid() as *mut c_void, kPlatformTypeX11EmbedWindowID) };
    state.attaching.store(false, Ordering::Release);
    if attached != kResultOk {
        super::fail(&format!("attached failed (0x{attached:x})"));
    }
    // A view may report no size before it is attached and ask for one from
    // `resizeView`; that request wins over what it reported earlier.
    let mut size = requested;
    if let Some((width, height)) = state.take_pending() {
        window.resize(width, height);
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: `view` is live and `rect` outlives the call.
        let _ = unsafe { view.onSize(&mut rect) };
        size = (width, height);
    } else if let Some(reported) = super::view_size(&view) {
        if reported != requested {
            window.resize(reported.0, reported.1);
        }
        size = reported;
    }
    state.replace_size(size);
    eprintln!("oc-vst3-editor-host: attached {}x{}", size.0, size.1);
    if let Err(error) = super::write_ready_file(config.ready_file.as_deref(), size) {
        super::fail(&error);
    }
    // The editor reads from the live instance until the window closes, and the
    // process leaves through `exit_process`, so the instance is never dropped.
    std::mem::forget(instance);

    let window_id = window.xid();
    let delete_atom = window.delete_atom();
    let mut closed = false;
    while !closed {
        serve(&state, SERVE_SLICE);
        while window.pending() {
            match window.next_event() {
                Event::Configure {
                    window,
                    width,
                    height,
                } => {
                    if window != window_id || width <= 0 || height <= 0 {
                        continue;
                    }
                    // A resize the host asked for is already known; only a
                    // size the window manager settled on is forwarded again.
                    if state.replace_size((width, height)) == (width, height) {
                        continue;
                    }
                    eprintln!("oc-vst3-editor-host: resized {width}x{height}");
                    let mut rect = ViewRect {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    };
                    // SAFETY: `view` is live until the loop ends.
                    let _ = unsafe { view.onSize(&mut rect) };
                }
                Event::ClientMessage { window, protocol }
                    if window == window_id && delete_atom != 0 && protocol == delete_atom =>
                {
                    closed = true;
                }
                Event::WindowDestroyed { window } if window == window_id => closed = true,
                _ => {}
            }
        }
    }
    state.window.store(0, Ordering::Release);
    // SAFETY: `view` is still attached and owned by this scope.
    let removed = unsafe { view.removed() };
    // SAFETY: `frame` is live and the view no longer references it.
    let _ = unsafe { view.setFrame(ptr::null_mut()) };
    super::shutdown_main();
    eprintln!("oc-vst3-editor-host: editor window closed (removed 0x{removed:x})");
    drop(window);
    crate::exit_process(0);
}
