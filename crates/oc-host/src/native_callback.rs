//! Keep Rust panic unwinding inside a host callback, never across its C ABI.
pub(crate) fn callback<T>(fallback: T, work: impl FnOnce() -> T, on_panic: impl FnOnce()) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(value) => value,
        Err(payload) => {
            on_panic();
            // A custom panic payload may itself panic during destruction.
            if payload.is::<String>() || payload.is::<&'static str>() {
                drop(payload);
            } else {
                std::mem::forget(payload);
            }
            fallback
        }
    }
}
