//! Typed CLAP C-ABI invocation; no foreign exception traverses a Rust frame.
#![expect(
    clippy::result_large_err,
    reason = "CLAP faults retain their fixed 256-byte cause inline so native/audio failure publication never allocates"
)]

#[cfg(target_os = "macos")]
use clap_sys::ext::gui::clap_window;
use clap_sys::{
    events::{clap_input_events, clap_output_events},
    ext::{
        audio_ports::clap_audio_port_info, note_ports::clap_note_port_info, params::clap_param_info,
    },
    factory::plugin_factory::clap_plugin_factory,
    host::clap_host,
    plugin::{clap_plugin, clap_plugin_descriptor},
    process::clap_process,
    stream::{clap_istream, clap_ostream},
};
use std::ffi::{c_char, c_void};

use crate::native_fault::RawFault;
pub use crate::native_fault::{Fault, FaultKind};

#[cfg(test)]
thread_local! { static PANIC_NEXT_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
thread_local! { static CALLBACK_PANICS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
/// Keep Rust unwinding inside the host callback, never in plugin or C++ frames.
/// Requires panic=unwind; aborts, memory faults and SEH require process isolation.
pub(crate) fn callback<T>(fallback: T, work: impl FnOnce() -> T) -> T {
    crate::native_callback::callback(
        fallback,
        || {
            #[cfg(test)]
            if PANIC_NEXT_CALLBACK.with(|next| next.replace(false)) {
                panic!("injected CLAP host callback panic");
            }
            work()
        },
        || CALLBACK_PANICS.with(|serial| serial.set(serial.get().wrapping_add(1))),
    )
}

pub(crate) fn callback_tracked<T>(
    failure: Option<&crate::clap_failure::ClapFailure>,
    fallback: T,
    work: impl FnOnce() -> T,
) -> T {
    if failure.is_some_and(|failure| failure.failed()) {
        return fallback;
    }
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let result = callback(fallback, work);
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before
        && let Some(failure) = failure
    {
        failure.record(
            crate::clap_failure::ClapOperation::HostCallback,
            Fault::callback_panic(),
        );
    }
    result
}

unsafe extern "C" {
    fn oc_clap_entry_init(
        call: unsafe extern "C" fn(*const c_char) -> bool,
        path: *const c_char,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn entry_init(
    call: unsafe extern "C" fn(*const c_char) -> bool,
    path: *const c_char,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_entry_init(call, path, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_entry_void(call: unsafe extern "C" fn(), fault: *mut RawFault) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn entry_void(call: unsafe extern "C" fn()) -> Result<(), Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_entry_void(call, &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    Ok(())
}

unsafe extern "C" {
    fn oc_clap_factory(
        call: unsafe extern "C" fn(*const c_char) -> *const c_void,
        id: *const c_char,
        result: *mut *const c_void,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn factory(
    call: unsafe extern "C" fn(*const c_char) -> *const c_void,
    id: *const c_char,
) -> Result<*const c_void, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<*const c_void>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_factory(call, id, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_factory_count(
        call: unsafe extern "C" fn(*const clap_plugin_factory) -> u32,
        factory: *const clap_plugin_factory,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn factory_count(
    call: unsafe extern "C" fn(*const clap_plugin_factory) -> u32,
    factory: *const clap_plugin_factory,
) -> Result<u32, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<u32>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_factory_count(call, factory, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_factory_descriptor(
        call: unsafe extern "C" fn(
            *const clap_plugin_factory,
            u32,
        ) -> *const clap_plugin_descriptor,
        factory: *const clap_plugin_factory,
        index: u32,
        result: *mut *const clap_plugin_descriptor,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn factory_descriptor(
    call: unsafe extern "C" fn(*const clap_plugin_factory, u32) -> *const clap_plugin_descriptor,
    factory: *const clap_plugin_factory,
    index: u32,
) -> Result<*const clap_plugin_descriptor, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<*const clap_plugin_descriptor>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok =
        // SAFETY: The caller supplies a live factory and exact count/index ABI; result and fault storage are writable locals borrowed by the C++ shim only until return.
        unsafe { oc_clap_factory_descriptor(call, factory, index, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_create(
        call: unsafe extern "C" fn(
            *const clap_plugin_factory,
            *const clap_host,
            *const c_char,
        ) -> *const clap_plugin,
        factory: *const clap_plugin_factory,
        host: *const clap_host,
        id: *const c_char,
        result: *mut *const clap_plugin,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn create(
    call: unsafe extern "C" fn(
        *const clap_plugin_factory,
        *const clap_host,
        *const c_char,
    ) -> *const clap_plugin,
    factory: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> Result<*const clap_plugin, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<*const clap_plugin>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_create(call, factory, host, id, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_plugin_bool(
        call: unsafe extern "C" fn(*const clap_plugin) -> bool,
        plugin: *const clap_plugin,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn plugin_bool(
    call: unsafe extern "C" fn(*const clap_plugin) -> bool,
    plugin: *const clap_plugin,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_plugin_bool(call, plugin, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_plugin_void(
        call: unsafe extern "C" fn(*const clap_plugin),
        plugin: *const clap_plugin,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn plugin_void(
    call: unsafe extern "C" fn(*const clap_plugin),
    plugin: *const clap_plugin,
) -> Result<(), Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_plugin_void(call, plugin, &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    Ok(())
}

unsafe extern "C" {
    fn oc_clap_plugin_u32(
        call: unsafe extern "C" fn(*const clap_plugin) -> u32,
        plugin: *const clap_plugin,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn plugin_u32(
    call: unsafe extern "C" fn(*const clap_plugin) -> u32,
    plugin: *const clap_plugin,
) -> Result<u32, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<u32>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_plugin_u32(call, plugin, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_activate(
        call: unsafe extern "C" fn(*const clap_plugin, f64, u32, u32) -> bool,
        plugin: *const clap_plugin,
        rate: f64,
        minimum: u32,
        maximum: u32,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn activate(
    call: unsafe extern "C" fn(*const clap_plugin, f64, u32, u32) -> bool,
    plugin: *const clap_plugin,
    rate: f64,
    minimum: u32,
    maximum: u32,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe {
        oc_clap_activate(
            call,
            plugin,
            rate,
            minimum,
            maximum,
            result.as_mut_ptr(),
            &mut raw,
        )
    };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_extension(
        call: unsafe extern "C" fn(*const clap_plugin, *const c_char) -> *const c_void,
        plugin: *const clap_plugin,
        id: *const c_char,
        result: *mut *const c_void,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn extension(
    call: unsafe extern "C" fn(*const clap_plugin, *const c_char) -> *const c_void,
    plugin: *const clap_plugin,
    id: *const c_char,
) -> Result<*const c_void, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<*const c_void>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_extension(call, plugin, id, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_process(
        call: unsafe extern "C" fn(*const clap_plugin, *const clap_process) -> i32,
        plugin: *const clap_plugin,
        process: *const clap_process,
        result: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn process(
    call: unsafe extern "C" fn(*const clap_plugin, *const clap_process) -> i32,
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> Result<i32, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<i32>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_process(call, plugin, process, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_ports_count(
        call: unsafe extern "C" fn(*const clap_plugin, bool) -> u32,
        plugin: *const clap_plugin,
        input: bool,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn ports_count(
    call: unsafe extern "C" fn(*const clap_plugin, bool) -> u32,
    plugin: *const clap_plugin,
    input: bool,
) -> Result<u32, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<u32>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_ports_count(call, plugin, input, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_audio_port(
        call: unsafe extern "C" fn(
            *const clap_plugin,
            u32,
            bool,
            *mut clap_audio_port_info,
        ) -> bool,
        plugin: *const clap_plugin,
        index: u32,
        input: bool,
        info: *mut clap_audio_port_info,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn audio_port(
    call: unsafe extern "C" fn(*const clap_plugin, u32, bool, *mut clap_audio_port_info) -> bool,
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    info: *mut clap_audio_port_info,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe {
        oc_clap_audio_port(
            call,
            plugin,
            index,
            input,
            info,
            result.as_mut_ptr(),
            &mut raw,
        )
    };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_note_port(
        call: unsafe extern "C" fn(*const clap_plugin, u32, bool, *mut clap_note_port_info) -> bool,
        plugin: *const clap_plugin,
        index: u32,
        input: bool,
        info: *mut clap_note_port_info,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn note_port(
    call: unsafe extern "C" fn(*const clap_plugin, u32, bool, *mut clap_note_port_info) -> bool,
    plugin: *const clap_plugin,
    index: u32,
    input: bool,
    info: *mut clap_note_port_info,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe {
        oc_clap_note_port(
            call,
            plugin,
            index,
            input,
            info,
            result.as_mut_ptr(),
            &mut raw,
        )
    };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_param_info(
        call: unsafe extern "C" fn(*const clap_plugin, u32, *mut clap_param_info) -> bool,
        plugin: *const clap_plugin,
        index: u32,
        info: *mut clap_param_info,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn param_info(
    call: unsafe extern "C" fn(*const clap_plugin, u32, *mut clap_param_info) -> bool,
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok =
        // SAFETY: The caller retains the plugin and exclusive writable info record; the exact C++ shim borrows result/fault locals only for this metadata call.
        unsafe { oc_clap_param_info(call, plugin, index, info, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_param_value(
        call: unsafe extern "C" fn(*const clap_plugin, u32, *mut f64) -> bool,
        plugin: *const clap_plugin,
        id: u32,
        value: *mut f64,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn param_value(
    call: unsafe extern "C" fn(*const clap_plugin, u32, *mut f64) -> bool,
    plugin: *const clap_plugin,
    id: u32,
    value: *mut f64,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_param_value(call, plugin, id, value, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_param_text(
        call: unsafe extern "C" fn(*const clap_plugin, u32, f64, *mut c_char, u32) -> bool,
        plugin: *const clap_plugin,
        id: u32,
        value: f64,
        text: *mut c_char,
        capacity: u32,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn param_text(
    call: unsafe extern "C" fn(*const clap_plugin, u32, f64, *mut c_char, u32) -> bool,
    plugin: *const clap_plugin,
    id: u32,
    value: f64,
    text: *mut c_char,
    capacity: u32,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe {
        oc_clap_param_text(
            call,
            plugin,
            id,
            value,
            text,
            capacity,
            result.as_mut_ptr(),
            &mut raw,
        )
    };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_flush(
        call: unsafe extern "C" fn(
            *const clap_plugin,
            *const clap_input_events,
            *const clap_output_events,
        ),
        plugin: *const clap_plugin,
        input: *const clap_input_events,
        output: *const clap_output_events,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn flush(
    call: unsafe extern "C" fn(
        *const clap_plugin,
        *const clap_input_events,
        *const clap_output_events,
    ),
    plugin: *const clap_plugin,
    input: *const clap_input_events,
    output: *const clap_output_events,
) -> Result<(), Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_flush(call, plugin, input, output, &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    Ok(())
}

unsafe extern "C" {
    fn oc_clap_load(
        call: unsafe extern "C" fn(*const clap_plugin, *const clap_istream) -> bool,
        plugin: *const clap_plugin,
        stream: *const clap_istream,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn load(
    call: unsafe extern "C" fn(*const clap_plugin, *const clap_istream) -> bool,
    plugin: *const clap_plugin,
    stream: *const clap_istream,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_load(call, plugin, stream, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_save(
        call: unsafe extern "C" fn(*const clap_plugin, *const clap_ostream) -> bool,
        plugin: *const clap_plugin,
        stream: *const clap_ostream,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn save(
    call: unsafe extern "C" fn(*const clap_plugin, *const clap_ostream) -> bool,
    plugin: *const clap_plugin,
    stream: *const clap_ostream,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_save(call, plugin, stream, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_gui_api(
        call: unsafe extern "C" fn(*const clap_plugin, *const c_char, bool) -> bool,
        plugin: *const clap_plugin,
        api: *const c_char,
        floating: bool,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn gui_api(
    call: unsafe extern "C" fn(*const clap_plugin, *const c_char, bool) -> bool,
    plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_gui_api(call, plugin, api, floating, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

unsafe extern "C" {
    fn oc_clap_gui_size(
        call: unsafe extern "C" fn(*const clap_plugin, *mut u32, *mut u32) -> bool,
        plugin: *const clap_plugin,
        width: *mut u32,
        height: *mut u32,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
pub(crate) unsafe fn gui_size(
    call: unsafe extern "C" fn(*const clap_plugin, *mut u32, *mut u32) -> bool,
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok =
        // SAFETY: The caller admits this GUI callback on main with a live plugin and writable width/height pointers; the exact C++ shim borrows output/fault locals until return.
        unsafe { oc_clap_gui_size(call, plugin, width, height, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn oc_clap_gui_parent(
        call: unsafe extern "C" fn(*const clap_plugin, *const clap_window) -> bool,
        plugin: *const clap_plugin,
        window: *const clap_window,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
}
/// # Safety
/// callback and pointers must satisfy the CLAP ABI, lifetime and thread contracts.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn gui_parent(
    call: unsafe extern "C" fn(*const clap_plugin, *const clap_window) -> bool,
    plugin: *const clap_plugin,
    window: *const clap_window,
) -> Result<bool, Fault> {
    let before = CALLBACK_PANICS.with(std::cell::Cell::get);
    let mut raw = RawFault {
        kind: 0,
        message: [0; 256],
    };
    let mut result = std::mem::MaybeUninit::<bool>::uninit();
    // SAFETY: the caller supplies the exact callback signature and valid pointers.
    let ok = unsafe { oc_clap_gui_parent(call, plugin, window, result.as_mut_ptr(), &mut raw) };
    if !ok {
        return Err(Fault::from_raw(raw));
    }
    if CALLBACK_PANICS.with(std::cell::Cell::get) != before {
        return Err(Fault::callback_panic());
    }
    // SAFETY: the native shim initializes result only on the successful path.
    Ok(unsafe { result.assume_init() })
}

#[cfg(test)]
pub(crate) fn panic_on_next_callback() {
    PANIC_NEXT_CALLBACK.with(|next| next.set(true));
}
