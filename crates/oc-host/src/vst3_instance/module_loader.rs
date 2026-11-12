//! Platform module acquisition and guarded VST3 entry/factory admission.
//!
//! Successful modules retain the existing process residency policy. OS loader
//! constructors are not covered by the explicit VST3 entrypoint barriers here.
use super::*;
use std::ffi::CStr;

fn healthy(failure: &Vst3Failure) -> Result<(), String> {
    if failure.failed() {
        Err(failure
            .message()
            .unwrap_or_else(|| "VST3 module is quarantined".into()))
    } else {
        Ok(())
    }
}
fn record(failure: &Vst3Failure, operation: Operation, fault: Fault) -> String {
    failure
        .slot
        .record(Vst3ProcessError::native(operation, fault));
    failure
        .message()
        .unwrap_or_else(|| "VST3 module is quarantined".into())
}

#[cfg(unix)]
fn path_cstring(path: &Path) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "module path contains a NUL byte".into())
}

fn open_module(path: &Path) -> Result<*mut c_void, String> {
    #[cfg(unix)]
    {
        let path = path_cstring(path)?;
        // SAFETY: the exact OS path bytes are NUL-terminated through dlopen;
        // the returned loader reference is retained or explicitly closed below.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
        if !handle.is_null() {
            return Ok(handle);
        }
        // SAFETY: dlerror returns null or a loader-owned terminated diagnostic;
        // it is copied before another loader operation can invalidate it.
        let error = unsafe { libc::dlerror() };
        Err(if error.is_null() {
            "the loader did not report a reason".into()
        } else {
            // SAFETY: dlerror's non-null loader-owned C string stays live
            // until the next loader operation; copy it synchronously here.
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err("module path contains a NUL code unit".into());
        }
        wide.push(0);
        // SAFETY: wide is an exact terminated UTF-16 OS path and stays live;
        // the resulting module reference is retained or explicitly freed below.
        let handle =
            unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(handle)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err("VST3 hosting is not implemented on this platform".into())
    }
}

/// Only fixed ABI symbol names are passed by this module.
unsafe fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
    #[cfg(unix)]
    {
        // SAFETY: caller retains this successful loader handle and name is terminated.
        unsafe { libc::dlsym(handle, name.as_ptr()) }
    }
    #[cfg(windows)]
    {
        // SAFETY: caller retains this module; fixed ASCII export name is terminated.
        match unsafe {
            windows_sys::Win32::System::LibraryLoader::GetProcAddress(handle, name.as_ptr().cast())
        } {
            Some(function) => function as *const () as *mut c_void,
            None => ptr::null_mut(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (handle, name);
        ptr::null_mut()
    }
}
unsafe fn close_module(handle: *mut c_void) {
    #[cfg(unix)]
    // SAFETY: caller owns this loader reference, has completed healthy module
    // cleanup, and has not published any foreign reference from this acquisition.
    unsafe {
        libc::dlclose(handle)
    };
    #[cfg(windows)]
    // SAFETY: same successful LoadLibraryW reference and healthy unpublished state.
    unsafe {
        windows_sys::Win32::Foundation::FreeLibrary(handle)
    };
    #[cfg(not(any(unix, windows)))]
    let _ = handle;
}

fn enter_symbol(
    entry: *mut c_void,
    context: *mut c_void,
    failure: &Vst3Failure,
) -> Result<bool, String> {
    healthy(failure)?;
    #[cfg(target_os = "macos")]
    let (operation, result) = {
        // SAFETY: fixed bundleEntry ABI takes the retained CFBundle context;
        // all foreign exception handling is inside the native C++ guard.
        (
            Operation::BundleEntry,
            unsafe { vst3_guard::bundle_entry(entry, context) }.map(|value| value != 0),
        )
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (operation, result) = {
        // SAFETY: fixed Linux ModuleEntry ABI takes this retained dlopen handle.
        (Operation::ModuleEntry, unsafe {
            vst3_guard::module_entry(entry, context)
        })
    };
    #[cfg(windows)]
    let (operation, result) = {
        let _ = context;
        // SAFETY: Windows VST3 InitDll has no arguments; no ModuleEntry(handle)
        // ABI cast is used. The native guard owns exception containment.
        (Operation::ModuleInit, unsafe {
            vst3_guard::module_init(entry)
        })
    };
    #[cfg(not(any(unix, windows)))]
    return {
        let _ = (entry, context);
        Err("unsupported module entry platform".into())
    };
    #[cfg(any(unix, windows))]
    {
        let entered = result.map_err(|fault| record(failure, operation, fault))?;
        healthy(failure)?;
        Ok(entered)
    }
}

fn exit_symbol(exit: *mut c_void, failure: &Vst3Failure) -> Result<(), String> {
    healthy(failure)?;
    // SAFETY: fixed ModuleExit/ExitDll/bundleExit exports all have bool(void)
    // ABI; the module remains loaded and completed entry successfully.
    let accepted = unsafe { vst3_guard::module_exit(exit) }
        .map_err(|fault| record(failure, Operation::ModuleExit, fault))?;
    healthy(failure)?;
    if !accepted {
        return Err(record(
            failure,
            Operation::ModuleExit,
            Fault::host_contract("module exit refused cleanup"),
        ));
    }
    Ok(())
}

fn acquire_factory(
    entry: *mut c_void,
    failure: &Arc<Vst3Failure>,
) -> Result<Vst3ForeignRef<IPluginFactory>, String> {
    healthy(failure)?;
    // SAFETY: entry is the fixed GetPluginFactory export of a retained module;
    // the native guard receives the possibly-partial output without Rust unwinding.
    let raw = unsafe { vst3_guard::get_factory(entry) }
        .map_err(|fault| record(failure, Operation::GetPluginFactory, fault))?;
    healthy(failure)?;
    // SAFETY: only a successful guarded GetPluginFactory transfers one owned
    // factory reference. Construction calls no addRef; immediate adoption prevents
    // generated Release from running on later shared failure.
    let reference = unsafe { ComPtr::from_raw(raw) }
        .ok_or_else(|| "GetPluginFactory returned a null factory".to_string())?;
    Ok(Vst3ForeignRef::adopt(reference, Arc::clone(failure)))
}

#[cfg(target_os = "macos")]
const ENTRY_NAME: &CStr = c"bundleEntry";
#[cfg(all(unix, not(target_os = "macos")))]
const ENTRY_NAME: &CStr = c"ModuleEntry";
#[cfg(windows)]
const ENTRY_NAME: &CStr = c"InitDll";
#[cfg(not(any(unix, windows)))]
const ENTRY_NAME: &CStr = c"ModuleEntry";
#[cfg(target_os = "macos")]
const EXIT_NAME: &CStr = c"bundleExit";
#[cfg(all(unix, not(target_os = "macos")))]
const EXIT_NAME: &CStr = c"ModuleExit";
#[cfg(windows)]
const EXIT_NAME: &CStr = c"ExitDll";
#[cfg(not(any(unix, windows)))]
const EXIT_NAME: &CStr = c"ModuleExit";

pub(super) fn load_factory(
    path: &Path,
    failure: &Arc<Vst3Failure>,
) -> Result<(Vst3ForeignRef<IPluginFactory>, *mut c_void), String> {
    healthy(failure)?;
    let binary = crate::bundle::binary_path(path).ok_or_else(|| {
        if path.exists() {
            format!(
                "the bundle has no library for this platform: {}",
                path.display()
            )
        } else {
            format!("the bundle does not exist: {}", path.display())
        }
    })?;
    let handle = open_module(&binary)?;
    crate::mark_plugin_module_loaded();
    #[cfg(target_os = "macos")]
    let initialization = macos::initialize(handle, path, failure);
    #[cfg(not(target_os = "macos"))]
    let initialization = initialize_module(handle, failure);
    let entered = match initialization {
        Ok(entered) => entered,
        Err(error) => {
            if !failure.failed() {
                // SAFETY: initialization did not publish a reference or poison
                // the module; ordinary entry refusal has no completed exit phase.
                unsafe { close_module(handle) };
            }
            return Err(error);
        }
    };
    // SAFETY: this loader reference remains live and the export name is fixed.
    let export = unsafe { symbol(handle, c"GetPluginFactory") };
    let result = if export.is_null() {
        Err("the module does not export GetPluginFactory".into())
    } else {
        acquire_factory(export, failure)
    };
    match result {
        Ok(factory) => {
            #[cfg(target_os = "macos")]
            if let Err(error) = macos::commit_acquisition(&entered, failure) {
                // The shared poison prevents this unpublished reference from
                // calling Release after failed context publication.
                drop(factory);
                return Err(error);
            }
            Ok((factory, handle))
        }
        Err(error) => {
            #[cfg(target_os = "macos")]
            let cleaned = macos::cleanup_failed_acquisition(handle, &entered, failure);
            #[cfg(not(target_os = "macos"))]
            let cleaned = cleanup_failed_acquisition(handle, entered, failure);
            if cleaned.is_ok() && !failure.failed() {
                // SAFETY: no factory was published, all completed lifecycle
                // phases were reversed successfully, and no foreign fault occurred.
                unsafe { close_module(handle) };
            }
            Err(failure
                .message()
                .unwrap_or_else(|| cleaned.err().unwrap_or(error)))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn initialize_module(handle: *mut c_void, failure: &Vst3Failure) -> Result<bool, String> {
    healthy(failure)?;
    // SAFETY: retained loader handle and compile-time ABI export name.
    let entry = unsafe { symbol(handle, ENTRY_NAME) };
    if entry.is_null() {
        return Ok(false);
    }
    if !enter_symbol(entry, handle, failure)? {
        return Err("the module rejected its entry call".into());
    }
    Ok(true)
}
#[cfg(not(target_os = "macos"))]
fn cleanup_failed_acquisition(
    handle: *mut c_void,
    entered: bool,
    failure: &Vst3Failure,
) -> Result<(), String> {
    healthy(failure)?;
    if !entered {
        return Ok(());
    }
    // SAFETY: retained loader handle and compile-time ABI export name.
    let exit = unsafe { symbol(handle, EXIT_NAME) };
    if exit.is_null() {
        return Err(record(
            failure,
            Operation::ModuleExit,
            Fault::host_contract("initialized module has no exit export"),
        ));
    }
    exit_symbol(exit, failure)
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
