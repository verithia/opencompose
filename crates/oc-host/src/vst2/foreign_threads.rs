//! Native-thread bootstrap shared by callback-owner tests.
use std::ffi::c_void;

/// Starts a real OS thread without Rust's thread bootstrap. In particular,
/// callback allocation checks include the first Rust work on a foreign thread.
pub(super) struct ForeignThread {
    #[cfg(unix)]
    handle: libc::pthread_t,
    #[cfg(windows)]
    handle: *mut c_void,
}
impl ForeignThread {
    pub(super) fn start<F: FnOnce() + Send + 'static>(work: F) -> Self {
        extern "C" fn unix_entry<F: FnOnce()>(raw: *mut c_void) -> *mut c_void {
            // SAFETY: start transfers exactly one Box<F> to this entrypoint,
            // which is called once by pthread_create with the unchanged pointer.
            let work = unsafe { Box::from_raw(raw.cast::<F>()) };
            work();
            std::ptr::null_mut()
        }
        #[cfg(windows)]
        unsafe extern "system" fn windows_entry<F: FnOnce()>(raw: *mut c_void) -> u32 {
            unix_entry::<F>(raw);
            0
        }
        let raw = Box::into_raw(Box::new(work)).cast();
        #[cfg(unix)]
        {
            let mut handle = std::mem::MaybeUninit::uninit();
            // SAFETY: handle is writable pthread_t storage; the default attrs
            // and matching entrypoint receive the transferred Box<F> pointer.
            let result = unsafe {
                libc::pthread_create(handle.as_mut_ptr(), std::ptr::null(), unix_entry::<F>, raw)
            };
            if result != 0 {
                // SAFETY: A failed pthread_create never transferred ownership.
                drop(unsafe { Box::from_raw(raw.cast::<F>()) });
                panic!("pthread_create refused: {result}");
            }
            Self {
                // SAFETY: Successful pthread_create initialized its output.
                handle: unsafe { handle.assume_init() },
            }
        }
        #[cfg(windows)]
        {
            // SAFETY: Default security/stack/flags, valid one-shot entrypoint
            // and owned context; no thread-id output is requested.
            let handle = unsafe {
                win::CreateThread(
                    std::ptr::null_mut(),
                    0,
                    windows_entry::<F>,
                    raw,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle.is_null() {
                // SAFETY: A failed CreateThread never transferred ownership.
                drop(unsafe { Box::from_raw(raw.cast::<F>()) });
                panic!("CreateThread refused");
            }
            Self { handle }
        }
    }
    pub(super) fn join(self) {
        #[cfg(unix)]
        {
            assert_eq!(
                // SAFETY: This uniquely owned joinable thread has not been
                // joined; null output requests no transferred return pointer.
                unsafe { libc::pthread_join(self.handle, std::ptr::null_mut()) },
                0
            );
        }
        #[cfg(windows)]
        {
            // SAFETY: handle remains owned and open until the wait completes.
            let status = unsafe { win::WaitForSingleObject(self.handle, u32::MAX) };
            // SAFETY: No further operation uses this uniquely owned OS handle.
            let closed = unsafe { win::CloseHandle(self.handle) };
            assert_eq!(status, 0);
            assert_ne!(closed, 0);
        }
    }
}
#[cfg(windows)]
mod win {
    use super::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn CreateThread(
            attrs: *mut c_void,
            stack: usize,
            entry: unsafe extern "system" fn(*mut c_void) -> u32,
            context: *mut c_void,
            flags: u32,
            id: *mut u32,
        ) -> *mut c_void;
        pub(super) fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        pub(super) fn CloseHandle(handle: *mut c_void) -> i32;
    }
}
