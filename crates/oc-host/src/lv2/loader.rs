//! Dynamic loading for LV2 bundle binaries.

#[cfg(unix)]
use std::ffi::CString;
use std::ffi::{CStr, c_char, c_void};
use std::path::Path;

use super::abi::Descriptor;

/// A dynamically loaded LV2 module.
pub struct Library {
    handle: *mut c_void,
}

// SAFETY: ownership of the handle moves with one instance; moving the handle
// does not move module memory or permit simultaneous access to the instance.
unsafe impl Send for Library {}

impl Library {
    /// Open an LV2 module from a path.
    pub fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let path = CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| format!("LV2 binary path contains a NUL: {}", path.display()))?;
            // SAFETY: the path is NUL-terminated and loader flags are valid.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                // SAFETY: dlerror returns the current thread's loader error buffer.
                let error = unsafe { libc::dlerror() };
                let message = if error.is_null() {
                    "unknown dlopen error".to_string()
                } else {
                    // SAFETY: a non-null dlerror result is a NUL-terminated loader string.
                    unsafe { CStr::from_ptr(error) }
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(format!(
                    "cannot open LV2 binary {}: {message}",
                    path.to_string_lossy()
                ));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self { handle })
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: wide is a NUL-terminated UTF-16 path and lives through the call.
            let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "cannot open LV2 binary {}: error {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self { handle })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err("dynamic loading is not supported on this platform".into())
        }
    }

    /// Resolve a symbol without extending the loaded module's lifetime.
    ///
    /// # Safety
    /// A returned address may only be called with its declared ABI and signature,
    /// and must not be used after this library is dropped.
    pub unsafe fn symbol(&self, name: &CStr) -> *mut c_void {
        #[cfg(unix)]
        {
            // SAFETY: self owns an open handle; name is NUL-terminated.
            unsafe { libc::dlsym(self.handle, name.as_ptr()) }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
            // SAFETY: self owns an open handle; name is NUL-terminated.
            let function = unsafe { GetProcAddress(self.handle as _, name.as_ptr() as *const u8) };
            function.map_or(std::ptr::null_mut(), |function| {
                function as *const () as *mut c_void
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            std::ptr::null_mut()
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: this is the one owned loader reference and Drop runs once.
        unsafe {
            libc::dlclose(self.handle);
        }
        #[cfg(windows)]
        // SAFETY: this is the one owned loader reference and Drop runs once.
        unsafe {
            windows_sys::Win32::Foundation::FreeLibrary(self.handle);
        }
    }
}

/// LV2 descriptor entry point.
pub type DescriptorFn = unsafe extern "C" fn(u32) -> *const Descriptor;

/// Resolve `lv2_descriptor` from a module.
pub fn descriptor_function(library: &Library) -> Result<DescriptorFn, String> {
    let name = c"lv2_descriptor";
    // SAFETY: the address is used below only as the documented LV2 entry signature.
    let symbol = unsafe { library.symbol(name) };
    if symbol.is_null() {
        return Err("LV2 binary does not export lv2_descriptor".into());
    }
    // SAFETY: lv2_descriptor is required to export the DescriptorFn ABI.
    Ok(unsafe { std::mem::transmute::<*mut c_void, DescriptorFn>(symbol) })
}

/// Find the descriptor whose URI matches the manifest entry.
///
/// # Safety
/// `descriptor_fn` must be an LV2 `lv2_descriptor` export from a loaded library.
/// Each non-null result must reference a valid, aligned LV2 descriptor with a
/// NUL-terminated URI. Keep that library loaded while using its returned descriptor.
pub unsafe fn find_descriptor(
    descriptor_fn: DescriptorFn,
    plugin_uri: &str,
) -> Result<*const Descriptor, String> {
    for index in 0..1024u32 {
        // SAFETY: the caller guarantees the LV2 entry ABI and loaded-module lifetime.
        let descriptor = unsafe { descriptor_fn(index) };
        if descriptor.is_null() {
            break;
        }
        // SAFETY: a non-null descriptor is readable by this function's contract.
        let uri = unsafe { (*descriptor).uri };
        if uri.is_null() {
            break;
        }
        // SAFETY: the descriptor contract requires a NUL-terminated URI.
        let Ok(uri) = unsafe { CStr::from_ptr(uri) }.to_str() else {
            continue;
        };
        if uri == plugin_uri {
            // SAFETY: the caller retains the module and guarantees a valid descriptor.
            let descriptor_ref = unsafe { &*descriptor };
            if descriptor_ref.instantiate.is_none() {
                return Err(format!(
                    "LV2 descriptor '{plugin_uri}' has no instantiate function"
                ));
            }
            if descriptor_ref.connect_port.is_none() {
                return Err(format!(
                    "LV2 descriptor '{plugin_uri}' has no connect_port function"
                ));
            }
            if descriptor_ref.run.is_none() {
                return Err(format!("LV2 descriptor '{plugin_uri}' has no run function"));
            }
            return Ok(descriptor);
        }
    }
    Err(format!(
        "LV2 binary does not provide descriptor '{plugin_uri}'"
    ))
}

/// Query an extension owned by the descriptor's loaded module.
///
/// # Safety
/// `descriptor` must be non-null, aligned and valid for shared access. Its
/// extension callback must follow the LV2 ABI. The descriptor's module must stay
/// loaded through this call and all uses of a returned extension interface.
pub unsafe fn get_extension(descriptor: *const Descriptor, uri: &'static CStr) -> *const c_void {
    // SAFETY: the caller guarantees descriptor validity and its module lifetime.
    let Some(extension_data) = (unsafe { &*descriptor }).extension_data else {
        return std::ptr::null();
    };
    // SAFETY: the caller guarantees the callback ABI; uri is a static C string.
    unsafe { extension_data(uri.as_ptr() as *const c_char) }
}
