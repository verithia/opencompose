//! Win32 backend: a titled `HWND` driven by a `GetMessage` loop.
//!
//! The plugin's view attaches to the window handle as `kPlatformTypeHWND` and
//! creates its own child window inside the client area. The window procedure
//! forwards the client size to the view, so a resize reaches the editor, and
//! routes the close button through `IPlugView::removed`.

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::Vst::IEditControllerTrait;
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kPlatformTypeHWND, kResultOk,
    tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, MSG, PostQuitMessage, RegisterClassW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetTimer, SetWindowPos, TranslateMessage, UnregisterClassW, WM_CLOSE, WM_DESTROY, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const MAIN_POLL_TIMER: usize = 0x4f43;

/// One successful OLE initialization count, owned by this helper's UI thread.
struct OleApartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl OleApartment {
    fn initialize() -> Result<Self, String> {
        // SAFETY: null is the documented reserved argument; this helper owns
        // its literal bootstrap thread and initializes before any plugin load.
        let result = unsafe { OleInitialize(ptr::null()) };
        if result < 0 {
            return Err(format!("OleInitialize failed: HRESULT {result:#010x}"));
        }
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for OleApartment {
    fn drop(&mut self) {
        // SAFETY: this non-Send guard owns exactly one successful OLE count,
        // including S_FALSE, and is dropped only on its initialization thread.
        unsafe { OleUninitialize() };
    }
}

/// The window procedure Windows calls; the state it needs is published here for
/// the lifetime of the message loop.
static STATE: AtomicUsize = AtomicUsize::new(0);

/// What the host frame and the window procedure share for one attached editor.
#[derive(Default)]
struct FrameState {
    /// Handle the view is attached to; 0 before the attach and after the close.
    window: AtomicUsize,
    /// View the window procedure forwards size changes to.
    view: AtomicUsize,
    /// Set while `IPlugView::attached` runs: a resize request is deferred.
    attaching: AtomicBool,
    /// Set once the window procedure has called `IPlugView::removed`.
    removed: AtomicBool,
    /// Size the window and the view last agreed on.
    size: Mutex<(i32, i32)>,
    /// Size requested during `attached`, applied once it returns.
    pending: Mutex<Option<(i32, i32)>>,
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

    /// Forward a new client size to the view.
    fn resize(&self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        if self.attaching.load(Ordering::Acquire) {
            *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((width, height));
            return;
        }
        if self.replace_size((width, height)) == (width, height) {
            return;
        }
        eprintln!("oc-vst3-editor-host: resized {width}x{height}");
        let view = self.view.load(Ordering::Acquire) as *mut IPlugView;
        if view.is_null() {
            return;
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: the instance keeps the view alive while the window is open.
        let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
        let _ = unsafe { view.onSize(&mut rect) };
    }

    /// Detach the view and destroy the window it was attached to.
    fn close(&self, window: HWND) {
        let view = self.view.swap(0, Ordering::AcqRel) as *mut IPlugView;
        if !view.is_null() {
            // SAFETY: the instance keeps the view alive until the process exits.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            let _ = unsafe { view.removed() };
        }
        self.removed.store(true, Ordering::Release);
        // `DestroyWindow` reports `WM_DESTROY` before it returns; the handle
        // stays published until then so the procedure recognises its window and
        // ends the message loop.
        // SAFETY: `window` is this thread's live editor window.
        unsafe { DestroyWindow(window) };
    }
}

fn state() -> Option<&'static FrameState> {
    let pointer = STATE.load(Ordering::Acquire) as *const FrameState;
    // SAFETY: `run` keeps the `Arc` alive for the whole message loop and never
    // clears the pointer while a message can still arrive.
    unsafe { pointer.as_ref() }
}

/// Host-side `IPlugFrame` for the editor window. A resize request moves the
/// window; the window procedure then reports the client size to the view.
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
        // The view sizes the client area, so the window needs its frame added.
        let window = self.state.window.load(Ordering::Acquire) as HWND;
        let mut client = (width, height);
        if !window.is_null() {
            // The `WM_SIZE` this posts is the size the host already knows, so it
            // must not reach the view a second time.
            self.state.replace_size(client);
            let (outer_width, outer_height) = outer_size(client);
            // SAFETY: the handle is this thread's editor window.
            unsafe {
                SetWindowPos(
                    window,
                    ptr::null_mut(),
                    0,
                    0,
                    outer_width,
                    outer_height,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            client = client_size(window).unwrap_or(client);
        }
        if self.state.replace_size(client) != client {
            if !view.is_null() {
                // SAFETY: the caller passes its own live view and rect.
                let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
                let _ = unsafe { view.onSize(new_size) };
            }
        }
        kResultOk
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

/// Window size that gives `client` as the client area.
fn outer_size(client: (i32, i32)) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client.0,
        bottom: client.1,
    };
    // SAFETY: `rect` is a valid `RECT` and the style is the one the window uses.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AdjustWindowRect(
            &mut rect,
            WS_OVERLAPPEDWINDOW,
            0,
        )
    };
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn client_size(window: HWND) -> Option<(i32, i32)> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: `window` is a live editor window and `rect` outlives the call.
    if unsafe { GetClientRect(window, &mut rect) } == 0 {
        return None;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    (width > 0 && height > 0).then_some((width, height))
}

/// The window procedure for the editor window.
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(state) = state()
        && state.window.load(Ordering::Acquire) == window as usize
    {
        match message {
            WM_TIMER if wparam == MAIN_POLL_TIMER => {
                if let Err(error) = super::pump_main() {
                    super::fail(&error);
                }
                return 0;
            }
            WM_SIZE => {
                let width = (lparam as u32 & 0xffff) as i32;
                let height = ((lparam as u32 >> 16) & 0xffff) as i32;
                state.resize(width, height);
                return 0;
            }
            WM_CLOSE => {
                state.close(window);
                return 0;
            }
            WM_DESTROY => {
                state.window.store(0, Ordering::Release);
                // SAFETY: the message loop of this thread is running.
                unsafe { PostQuitMessage(0) };
                return 0;
            }
            _ => {}
        }
    }
    // SAFETY: the arguments are the ones Windows passed to this procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Run the platform editor event loop.
pub fn run() {
    // The window closes through Win32, but a plugin that installs its own exit
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
    // Keep the apartment alive through native construction and the entire
    // editor loop. Process-isolated exit retains it with quarantined plugins;
    // ordinary unwinding balances its own successful initialization count.
    let _ole = match OleApartment::initialize() {
        Ok(apartment) => apartment,
        Err(error) => super::fail(&error),
    };
    let instance = match super::load_instance(&config) {
        Ok(instance) => instance,
        Err(error) => super::fail(&error),
    };
    if config.delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(config.delay_ms));
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
    // SAFETY: `view` is a live IPlugView.
    if unsafe { view.isPlatformTypeSupported(kPlatformTypeHWND) } != kResultOk {
        super::fail("the editor does not support the HWND platform");
    }
    let requested = super::view_size(&view).unwrap_or(super::DEFAULT_EDITOR_SIZE);

    let wide = |text: &str| -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;

        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let class_name = wide("OpenComposeEditorHost");
    let window_name = wide(&config.title);
    // SAFETY: the module handle of this process is valid for its whole life.
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    // SAFETY: `class` describes a window procedure that is live for the class.
    if unsafe { RegisterClassW(&class) } == 0 {
        super::fail(&format!(
            "RegisterClassW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let state = Arc::new(FrameState::default());
    STATE.store(Arc::as_ptr(&state) as usize, Ordering::Release);
    let (outer_width, outer_height) = outer_size(requested);
    // SAFETY: the class is registered, the module handle is live, and the
    // title outlives the call.
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            outer_width,
            outer_height,
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    if window.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: the class was registered above and the module handle is live.
        unsafe { UnregisterClassW(class_name.as_ptr(), module) };
        super::fail(&format!(
            "CreateWindowExW failed, so this session cannot host an editor: {error}"
        ));
    }
    state.window.store(window as usize, Ordering::Release);
    // SAFETY: this host window belongs to the current thread. Its timer wakes
    // GetMessage even when no plugin/UI messages are pending.
    if unsafe { SetTimer(window, MAIN_POLL_TIMER, 16, None) } == 0 {
        super::fail(&format!(
            "Could not start main-loop service timer: {}",
            std::io::Error::last_os_error()
        ));
    }
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
    state.view.store(view.as_ptr() as usize, Ordering::Release);
    state.attaching.store(true, Ordering::Release);
    // SAFETY: the window is live and stays alive until the loop ends.
    let attached = unsafe { view.attached(window, kPlatformTypeHWND) };
    state.attaching.store(false, Ordering::Release);
    if attached != kResultOk {
        super::fail(&format!("attached failed (0x{attached:x})"));
    }
    // A view may report no size before it is attached and ask for one from
    // `resizeView`; that request wins over what it reported earlier.
    let mut size = requested;
    if let Some((width, height)) = state.take_pending() {
        let (outer_width, outer_height) = outer_size((width, height));
        // SAFETY: the handle is this thread's live editor window.
        unsafe {
            SetWindowPos(
                window,
                ptr::null_mut(),
                0,
                0,
                outer_width,
                outer_height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        size = client_size(window).unwrap_or((width, height));
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: size.0,
            bottom: size.1,
        };
        // SAFETY: `view` is live and `rect` outlives the call.
        let _ = unsafe { view.onSize(&mut rect) };
    } else if let Some(reported) = super::view_size(&view) {
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

    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `message` is valid and the queue belongs to this thread.
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        // SAFETY: `message` was filled by `GetMessageW`.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if !state.removed.load(Ordering::Acquire) {
        // SAFETY: the view is still attached and owned by this scope.
        let _ = unsafe { view.removed() };
    }
    // SAFETY: `frame` is live and the view no longer references it.
    let _ = unsafe { view.setFrame(ptr::null_mut()) };
    super::shutdown_main();
    eprintln!("oc-vst3-editor-host: editor window closed");
    // SAFETY: the class was registered in this process and has no windows left.
    unsafe { UnregisterClassW(class_name.as_ptr(), module) };
    crate::exit_process(0);
}
