//! Native callbacks progress between offline blocks on the admitted main thread.

use crate::native_fault::{Fault, RawFault};
use std::cell::Cell;

thread_local! {
    static SERVICING: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" {
    fn oc_application_service_native_main(fault: *mut RawFault) -> bool;
}

pub(super) fn service() -> Result<(), String> {
    if SERVICING.with(|active| active.replace(true)) {
        return Ok(());
    }
    struct Servicing;
    impl Drop for Servicing {
        fn drop(&mut self) {
            SERVICING.with(|active| active.set(false));
        }
    }
    let _servicing = Servicing;
    let mut fault = RawFault {
        kind: 0,
        message: [0; 256],
    };
    // SAFETY: the caller admits the application main thread outside native audio
    // calls. The exception barrier retains this fault buffer and performs one
    // non-waiting native service pass; the TLS guard prevents nested service.
    if unsafe { oc_application_service_native_main(&mut fault) } {
        Ok(())
    } else {
        Err(Fault::from_raw(fault).describe_for("application main events"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[repr(C)]
    struct TimerContext {
        version: isize,
        info: *mut c_void,
        retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<unsafe extern "C" fn(*const c_void)>,
        description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFAbsoluteTimeGetCurrent() -> f64;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopTimerCreate(
            allocator: *const c_void,
            fire_date: f64,
            interval: f64,
            flags: usize,
            order: isize,
            callback: unsafe extern "C" fn(*mut c_void, *mut c_void),
            context: *mut TimerContext,
        ) -> *mut c_void;
        fn CFRunLoopAddTimer(loop_: *mut c_void, timer: *mut c_void, mode: *const c_void);
        fn CFRunLoopTimerInvalidate(timer: *mut c_void);
        fn CFRelease(object: *mut c_void);
    }

    unsafe extern "C" fn fired(_: *mut c_void, context: *mut c_void) {
        // SAFETY: the synchronous test retains this atomic until timer removal.
        unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Relaxed);
    }

    #[test]
    fn offline_native_service_delivers_due_callbacks_and_respects_reentrancy() {
        let delivered = AtomicBool::new(false);
        let mut context = TimerContext {
            version: 0,
            info: (&delivered as *const AtomicBool).cast_mut().cast(),
            retain: None,
            release: None,
            description: None,
        };
        // SAFETY: the current thread owns the run loop, timer and retained
        // callback context, and removes the timer before the atomic is dropped.
        unsafe {
            let timer = CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent() - 1.0,
                0.0,
                0,
                0,
                fired,
                &mut context,
            );
            assert!(!timer.is_null());
            CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopDefaultMode);
            SERVICING.with(|active| active.set(true));
            service().unwrap();
            let nested_delivered = delivered.load(Ordering::Relaxed);
            SERVICING.with(|active| active.set(false));
            let result = service();
            CFRunLoopTimerInvalidate(timer);
            CFRelease(timer);
            assert!(!nested_delivered);
            result.unwrap();
        }
        assert!(delivered.load(Ordering::Relaxed));
        assert!(!SERVICING.with(Cell::get));
    }
}
