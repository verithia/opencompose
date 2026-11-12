//! X11 parent window for a plugin editor.
//!
//! Only the calls an embedding parent needs: create, resize, destroy, and the
//! event loop an on-screen editor window runs. Xlib is linked directly, so the
//! crate takes no extra dependency.

use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr;
use std::sync::Once;

type Display = c_void;
type Window = c_ulong;
type Atom = c_ulong;

#[cfg(target_pointer_width = "64")]
fn protocol_id(value: c_ulong) -> u64 {
    value
}

#[cfg(target_pointer_width = "32")]
fn protocol_id(value: c_ulong) -> u64 {
    u64::from(value)
}

/// `StructureNotifyMask`: this window's own size and lifecycle events.
const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;

/// Event kinds this module reads out of `XEvent`.
const DESTROY_NOTIFY: c_int = 17;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;

/// Xlib's `XEvent` is a union whose largest member is 24 longs.
#[repr(C)]
struct XEvent {
    raw: [c_long; 24],
}

#[repr(C)]
#[allow(dead_code)] // mirrors the Xlib layout; only some fields are read
struct XConfigureEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    event: Window,
    window: Window,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: Window,
    override_redirect: c_int,
}

#[repr(C)]
#[allow(dead_code)] // mirrors the Xlib layout; only some fields are read
struct XClientMessageEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: Window,
    message_type: Atom,
    format: c_int,
    data: [c_long; 5],
}

#[repr(C)]
#[allow(dead_code)] // mirrors the Xlib layout; only some fields are read
struct XDestroyWindowEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    event: Window,
    window: Window,
}

/// The events an editor window's loop reacts to. Everything else on the
/// connection belongs to the plugin or to the window manager.
pub enum Event {
    /// The window changed size; the editor must be told the new client size.
    Configure {
        window: u64,
        width: i32,
        height: i32,
    },
    /// A window manager protocol message; `protocol` is the atom the sender
    /// put in the message, for example `WM_DELETE_WINDOW`.
    ClientMessage { window: u64, protocol: u64 },
    /// The window is gone.
    WindowDestroyed { window: u64 },
    /// An event this loop leaves to the window manager.
    Other,
}

impl XEvent {
    fn classify(&self) -> Event {
        // SAFETY: `XNextEvent` filled this buffer and every event starts with
        // its kind, which selects the layout below.
        let kind = unsafe { *(self.raw.as_ptr() as *const c_int) };
        match kind {
            CONFIGURE_NOTIFY => {
                // SAFETY: a ConfigureNotify event has the `XConfigureEvent` layout.
                let event = unsafe { &*(self.raw.as_ptr() as *const XConfigureEvent) };
                Event::Configure {
                    window: event.window,
                    width: event.width,
                    height: event.height,
                }
            }
            CLIENT_MESSAGE => {
                // SAFETY: a ClientMessage event has the `XClientMessageEvent`
                // layout, and `format == 32` carries the atom in `data.l[0]`.
                let event = unsafe { &*(self.raw.as_ptr() as *const XClientMessageEvent) };
                let protocol = if event.format == 32 {
                    event.data[0] as u64
                } else {
                    0
                };
                Event::ClientMessage {
                    window: event.window,
                    protocol,
                }
            }
            DESTROY_NOTIFY => {
                // SAFETY: a DestroyNotify event has the `XDestroyWindowEvent` layout.
                let event = unsafe { &*(self.raw.as_ptr() as *const XDestroyWindowEvent) };
                Event::WindowDestroyed {
                    window: event.window,
                }
            }
            _ => Event::Other,
        }
    }
}

#[link(name = "X11")]
unsafe extern "C" {
    fn XInitThreads() -> c_int;
    fn XOpenDisplay(name: *const c_char) -> *mut Display;
    fn XDefaultScreen(display: *mut Display) -> c_int;
    fn XRootWindow(display: *mut Display, screen: c_int) -> Window;
    fn XCreateSimpleWindow(
        display: *mut Display,
        parent: Window,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> Window;
    fn XStoreName(display: *mut Display, window: Window, name: *const c_char) -> c_int;
    fn XMapWindow(display: *mut Display, window: Window) -> c_int;
    fn XResizeWindow(display: *mut Display, window: Window, width: c_uint, height: c_uint)
    -> c_int;
    fn XDestroyWindow(display: *mut Display, window: Window) -> c_int;
    fn XSync(display: *mut Display, discard: c_int) -> c_int;
    fn XFlush(display: *mut Display) -> c_int;
    fn XCloseDisplay(display: *mut Display) -> c_int;
    fn XSelectInput(display: *mut Display, window: Window, event_mask: c_long) -> c_int;
    fn XInternAtom(display: *mut Display, atom_name: *const c_char, only_if_exists: c_int) -> Atom;
    fn XSetWMProtocols(
        display: *mut Display,
        window: Window,
        protocols: *mut Atom,
        count: c_int,
    ) -> c_int;
    fn XPending(display: *mut Display) -> c_int;
    fn XNextEvent(display: *mut Display, event_return: *mut XEvent) -> c_int;
}

/// An X11 window a plugin can reparent its editor into.
pub struct EditorWindow {
    display: *mut Display,
    window: Window,
    /// `WM_DELETE_WINDOW` of this connection; 0 when the window manager does
    /// not support it and a destroy event is the only close signal.
    delete_atom: Atom,
}

impl EditorWindow {
    /// Open a mapped window on the default display.
    ///
    /// The window sits far outside any screen so an editor never covers the
    /// desktop; plugins still need it mapped to build their own child window.
    pub fn open(title: &str, width: i32, height: i32) -> Result<Self, String> {
        static THREADS: Once = Once::new();
        // Xlib is not thread-safe until this runs, and a plugin may call back
        // from its own thread.
        // SAFETY: `XInitThreads` takes no arguments and the `Once` runs it before this
        // process makes any other Xlib call through this path.
        THREADS.call_once(|| unsafe {
            XInitThreads();
        });
        // SAFETY: a null display name means `$DISPLAY`, which is what a host wants.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            let name = std::env::var("DISPLAY").unwrap_or_default();
            return Err(format!("cannot open X display '{name}'"));
        }
        // SAFETY: `display` is a live connection; the window outlives this call.
        let window = unsafe {
            let screen = XDefaultScreen(display);
            let root = XRootWindow(display, screen);
            XCreateSimpleWindow(
                display,
                root,
                -10_000,
                -10_000,
                width.max(1) as c_uint,
                height.max(1) as c_uint,
                0,
                0,
                0,
            )
        };
        if window == 0 {
            // SAFETY: `display` is the live connection opened above and nothing else uses it.
            unsafe { XCloseDisplay(display) };
            return Err("XCreateSimpleWindow returned 0".to_string());
        }
        if let Ok(name) = CString::new(title) {
            // SAFETY: `display` and `window` are live, and `name` outlives the call.
            unsafe { XStoreName(display, window, name.as_ptr()) };
        }
        // SAFETY: `display` and `window` are live connection and window handles.
        unsafe {
            XMapWindow(display, window);
            XSync(display, 0);
        }
        Ok(Self {
            display,
            window,
            delete_atom: 0,
        })
    }

    /// Open a mapped top-level window that shows a hosted editor.
    ///
    /// The window selects the events its loop reads and asks the window manager
    /// to route the close button to [`Self::next_event`] instead of destroying
    /// the window under the host.
    pub fn open_editor(title: &str, width: i32, height: i32) -> Result<Self, String> {
        static THREADS: Once = Once::new();
        // Xlib is not thread-safe until this runs, and a plugin may call back
        // from its own thread.
        // SAFETY: `XInitThreads` takes no arguments and the `Once` runs it before this
        // process makes any other Xlib call through this path.
        THREADS.call_once(|| unsafe {
            XInitThreads();
        });
        // SAFETY: a null display name means `$DISPLAY`, which is what a host wants.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            let name = std::env::var("DISPLAY").unwrap_or_default();
            return Err(format!("cannot open X display '{name}'"));
        }
        // SAFETY: `display` is a live connection; the window outlives this call.
        let window = unsafe {
            let screen = XDefaultScreen(display);
            let root = XRootWindow(display, screen);
            XCreateSimpleWindow(
                display,
                root,
                100,
                100,
                width.max(1) as c_uint,
                height.max(1) as c_uint,
                0,
                0,
                0x00ff_ffff,
            )
        };
        if window == 0 {
            // SAFETY: `display` is the live connection opened above and nothing else uses it.
            unsafe { XCloseDisplay(display) };
            return Err("XCreateSimpleWindow returned 0".to_string());
        }
        if let Ok(name) = CString::new(title) {
            // SAFETY: `display` and `window` are live, and `name` outlives the call.
            unsafe { XStoreName(display, window, name.as_ptr()) };
        }
        // SAFETY: `display` and `window` are live for the whole block.
        let delete_atom = unsafe {
            XSelectInput(display, window, STRUCTURE_NOTIFY_MASK);
            let delete_atom = XInternAtom(display, c"WM_DELETE_WINDOW".as_ptr(), 0);
            if delete_atom != 0 {
                let mut protocols = [delete_atom];
                XSetWMProtocols(display, window, protocols.as_mut_ptr(), 1);
            }
            XMapWindow(display, window);
            XSync(display, 0);
            delete_atom
        };
        Ok(Self {
            display,
            window,
            delete_atom,
        })
    }

    /// Window id to pass to `IPlugView::attached` as `X11EmbedWindowID`.
    pub fn xid(&self) -> u64 {
        protocol_id(self.window)
    }

    /// `WM_DELETE_WINDOW` of this connection, or 0 when the window manager has
    /// no protocol support.
    pub fn delete_atom(&self) -> u64 {
        protocol_id(self.delete_atom)
    }

    /// Whether an event is waiting on this connection.
    pub fn pending(&self) -> bool {
        // SAFETY: `display` is a live connection owned by this window.
        unsafe { XPending(self.display) > 0 }
    }

    /// Read one queued event. Call only when [`Self::pending`] is true.
    pub fn next_event(&self) -> Event {
        let mut event = XEvent { raw: [0; 24] };
        // SAFETY: an event is queued, which `pending` checked, and `event`
        // outlives the call.
        unsafe { XNextEvent(self.display, &mut event) };
        event.classify()
    }

    /// Display connection, for resizing from a plugin callback.
    pub fn display(&self) -> *mut c_void {
        self.display
    }

    /// Resize the parent window; the plugin relayouts its child window.
    pub fn resize(&self, width: i32, height: i32) {
        resize(self.display, self.xid(), width, height);
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        // SAFETY: both handles are live and owned by this value.
        unsafe {
            XDestroyWindow(self.display, self.window);
            XCloseDisplay(self.display);
        }
    }
}

/// Whether a connection to the default X display succeeds.
///
/// A host on a machine without an X server cannot host editors at all, so it
/// reports the capability as absent instead of failing each plugin.
pub fn display_available() -> bool {
    static THREADS: Once = Once::new();
    // SAFETY: `XInitThreads` takes no arguments and the `Once` runs it before this
    // process makes any other Xlib call through this path.
    THREADS.call_once(|| unsafe {
        XInitThreads();
    });
    // SAFETY: a null display name means `$DISPLAY`; the connection is closed here.
    let display = unsafe { XOpenDisplay(ptr::null()) };
    if display.is_null() {
        return false;
    }
    // SAFETY: `display` is the connection opened above, closed exactly once here.
    unsafe { XCloseDisplay(display) };
    true
}

/// Resize an existing window from a raw display pointer and window id.
pub fn resize(display: *mut c_void, window: u64, width: i32, height: i32) {
    if display.is_null() || window == 0 {
        return;
    }
    // SAFETY: the caller passes the display and window id of a live window.
    unsafe {
        XResizeWindow(
            display as *mut Display,
            window as Window,
            width.max(1) as c_uint,
            height.max(1) as c_uint,
        );
        XSync(display as *mut Display, 0);
    }
}

/// Flush pending requests on a raw display pointer.
pub fn flush(display: *mut c_void) {
    if display.is_null() {
        return;
    }
    // SAFETY: the caller passes a live display connection.
    unsafe {
        XFlush(display as *mut Display);
    }
}
