//! Run host calls into a plugin behind a C++ exception barrier.
//!
//! Rust's `catch_unwind` only handles Rust panics. A plugin that throws a C++
//! exception while the host calls it unwinds into Rust frames, where the
//! runtime aborts with "Rust cannot catch foreign exceptions" instead of
//! returning an error. A plugin is allowed to fail; it is not allowed to take
//! the host down, so process, state, bus, editor and teardown calls are made
//! through the barrier in `ffi_guard.cc`. The shim is built on macOS, Linux and
//! Windows.

/// Run `work`, turning a C++ exception into an error message.
pub(crate) fn guard<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    struct Job<F, T> {
        work: Option<F>,
        result: Option<T>,
    }

    // `C-unwind` on purpose: a foreign exception has to be able to unwind
    // through this frame to reach the C++ handler. With a plain `extern "C"`
    // frame the unwinder aborts here instead ("a function that cannot unwind").
    extern "C-unwind" fn run<F: FnOnce() -> T, T>(context: *mut std::ffi::c_void) {
        // SAFETY: `context` points at the `Job` that `dispatch` keeps alive for
        // the whole call, and the C++ side runs this callback on this thread.
        let job = unsafe { &mut *(context as *mut Job<F, T>) };
        if let Some(work) = job.work.take() {
            job.result = Some(work());
        }
    }

    fn dispatch<F: FnOnce() -> T, T>(work: F) -> Result<T, String> {
        use std::ffi::{c_char, c_void};

        unsafe extern "C" {
            fn oc_host_guard_call(
                work: extern "C-unwind" fn(*mut c_void),
                context: *mut c_void,
                message: *mut c_char,
                capacity: usize,
            ) -> i32;
        }

        let mut job = Job {
            work: Some(work),
            result: None,
        };
        let mut message = [0 as c_char; 256];
        // SAFETY: the callback only touches `job`, which outlives the call, and
        // `capacity` is the length of `message`.
        let status = unsafe {
            oc_host_guard_call(
                run::<F, T>,
                (&mut job as *mut Job<F, T>).cast(),
                message.as_mut_ptr(),
                message.len(),
            )
        };
        if status == 0 {
            return job
                .result
                .ok_or_else(|| "the guard returned without running its callback".to_string());
        }
        // SAFETY: the C++ side always terminates the buffer.
        let text = unsafe { std::ffi::CStr::from_ptr(message.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if text.is_empty() {
            Err(format!("the plugin threw a C++ exception (code {status})"))
        } else {
            Err(format!("the plugin threw a C++ exception: {text}"))
        }
    }

    dispatch(work)
}

/// Run an editor or controller call behind the barrier on the AppKit main
/// thread.
///
/// `createView`, `attached`, `removed` and the controller's parameter calls are
/// main-thread-only; a call from anywhere else is a plugin contract violation
/// even when it does not throw, so debug builds catch it here.
#[cfg(target_os = "macos")]
pub(crate) fn guard_ui<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    #[cfg(target_os = "macos")]
    debug_assert!(
        crate::vst3_instance::is_main_thread(),
        "a VST3 editor or controller call ran off the AppKit main thread"
    );
    guard(work)
}
