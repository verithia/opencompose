//! UI-owned, bounded authority for the image containing the actual entry code.
//! Path aliases do not authorize retrying an entry image after native failure.
use super::{DynamicLibrary, abi, callback_route, failure::Vst2FailureRecord};
use crate::application_main::{self, UiAuthorityError, UiThreadToken};
use crate::native_fault::FirstFault;
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(any(unix, test, feature = "test-support"))]
use std::ffi::CStr;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

const MAX_ALIASES: usize = 4096;
const MAX_ALIAS_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 128 * 1024;
#[cfg(unix)]
const MAX_SYMBOL_BYTES: usize = 1024;

#[derive(Default)]
pub(super) struct ModuleFailure {
    slot: FirstFault<Vst2FailureRecord>,
}
impl ModuleFailure {
    pub(super) fn failed(&self) -> bool {
        self.slot.failed()
    }
    pub(super) fn first(&self) -> Option<Vst2FailureRecord> {
        self.slot.first()
    }
    pub(super) fn take_report(&self) -> Option<Vst2FailureRecord> {
        self.slot.take_report()
    }
    pub(super) fn record(&self, record: Vst2FailureRecord) -> bool {
        self.slot.record(record)
    }
}
impl std::fmt::Debug for ModuleFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleFailure")
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub(super) enum ModuleError {
    Authority(UiAuthorityError),
    Capacity,
    Path,
    Load,
    Entry,
    ImageIdentity,
    Faulted(Arc<ModuleFailure>),
}
impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(error) => error.fmt(f),
            Self::Capacity => f.write_str("VST2 module authority capacity exhausted"),
            Self::Path => f.write_str("VST2 module path could not be resolved within its bounds"),
            Self::Load => f.write_str("VST2 module could not be loaded"),
            Self::Entry => f.write_str("VST2 module has no supported entry symbol"),
            Self::ImageIdentity => f.write_str("VST2 entry image ownership could not be verified"),
            Self::Faulted(_) => {
                f.write_str("VST2 entry image is quarantined after an earlier failure")
            }
        }
    }
}
impl std::error::Error for ModuleError {}

struct Image {
    // Pinned code and source/dependency references are retained for process life.
    // The module/alias budget bounds these references; no native unload occurs
    // during TLS teardown after the application runtime has already closed.
    pins: RefCell<Vec<ManuallyDrop<DynamicLibrary>>>,
    failure: Arc<ModuleFailure>,
}
#[derive(Clone)]
struct Alias {
    image: Rc<Image>,
    entry: abi::PluginMain,
}
#[derive(Default)]
struct Registry {
    images: HashMap<usize, Rc<Image>>,
    aliases: HashMap<PathBuf, Alias>,
    pending: usize,
    alias_bytes: usize,
}
thread_local! { static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default()); }

/// Only UI control holds this code authority. Worker processing observes its
/// separately shared atomic failure record, never clones/releases native pins.
pub(super) struct ModuleAuthority {
    alias: Alias,
}
impl ModuleAuthority {
    pub(super) fn entry(&self) -> abi::PluginMain {
        self.alias.entry
    }
    pub(super) fn failure(&self) -> Arc<ModuleFailure> {
        self.alias.image.failure.clone()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn fixture_symbol(&self, name: &CStr) -> *mut c_void {
        self.alias.image.pins.borrow()[1].symbol(name)
    }
}
struct Reservation {
    bytes: usize,
    committed: bool,
}
impl Reservation {
    fn acquire(bytes: usize) -> Result<Self, ModuleError> {
        REGISTRY.with(|registry| {
            let mut registry = registry
                .try_borrow_mut()
                .map_err(|_| ModuleError::Capacity)?;
            if registry.images.len() + registry.pending >= callback_route::CAPACITY
                || registry.aliases.len() + registry.pending >= MAX_ALIASES
                || bytes > MAX_ALIAS_BYTES.saturating_sub(registry.alias_bytes)
            {
                return Err(ModuleError::Capacity);
            }
            registry.pending += 1;
            registry.alias_bytes += bytes;
            Ok(Self {
                bytes,
                committed: false,
            })
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        REGISTRY.with(|registry| {
            // No native operation runs under registry borrows. If an interrupted
            // internal transition prevents release, retaining the reservation
            // is bounded and cannot grant extra module/alias capacity.
            if let Ok(mut registry) = registry.try_borrow_mut() {
                registry.pending -= 1;
                registry.alias_bytes -= self.bytes;
            }
        });
    }
}

pub(super) fn acquire(
    _token: &UiThreadToken<'_>,
    executable: &Path,
) -> Result<ModuleAuthority, ModuleError> {
    application_main::ui_authority().map_err(ModuleError::Authority)?;
    // Canonicalization is bounded temporary control work. Persistent alias
    // allocation and every new OS load require the reservation below first.
    if executable.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(ModuleError::Path);
    }
    let canonical = std::fs::canonicalize(executable).map_err(|_| ModuleError::Path)?;
    let bytes = canonical.as_os_str().as_encoded_bytes().len();
    if bytes > MAX_PATH_BYTES {
        return Err(ModuleError::Path);
    }
    let known = REGISTRY.with(|registry| {
        registry
            .try_borrow()
            .ok()
            .and_then(|registry| registry.aliases.get(&canonical).cloned())
    });
    if let Some(alias) = known {
        if alias.image.failure.failed() {
            return Err(ModuleError::Faulted(alias.image.failure.clone()));
        }
        return Ok(ModuleAuthority { alias });
    }
    let mut reservation = Reservation::acquire(bytes)?;
    let source_pin = DynamicLibrary::open(&canonical).map_err(|_| ModuleError::Load)?;
    let entry = source_pin.vst2_entry().map_err(|_| ModuleError::Entry)?;
    // source_pin keeps the source image and its required dependencies alive
    // throughout lookup and acquisition of the actual defining image's pin.
    let (identity, entry_pin) = pin_entry_image(entry)?;
    let alias = REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| ModuleError::Capacity)?;
        let image = registry
            .images
            .entry(identity)
            .or_insert_with(|| {
                Rc::new(Image {
                    pins: RefCell::new(Vec::new()),
                    failure: Arc::new(ModuleFailure::default()),
                })
            })
            .clone();
        if image.failure.failed() {
            return Err(ModuleError::Faulted(image.failure.clone()));
        }
        image
            .pins
            .borrow_mut()
            .extend([ManuallyDrop::new(source_pin), ManuallyDrop::new(entry_pin)]);
        let alias = Alias { image, entry };
        registry.aliases.insert(canonical, alias.clone());
        registry.pending -= 1;
        reservation.committed = true;
        Ok(alias)
    })?;
    Ok(ModuleAuthority { alias })
}

/// # Safety
/// The OS must own readable terminated name storage while its image is pinned.
#[cfg(unix)]
unsafe fn bounded_os_name<'a>(
    pointer: *const std::ffi::c_char,
    limit: usize,
) -> Result<&'a CStr, ModuleError> {
    if pointer.is_null() {
        return Err(ModuleError::ImageIdentity);
    }
    for length in 0..limit {
        // SAFETY: dladdr's names remain readable through their terminator while
        // the source/dependency pin is retained; stop before reading past it.
        if unsafe { pointer.add(length).read() } == 0 {
            // SAFETY: The preceding scan established exactly this bounded
            // terminated storage. The caller bounds the borrowed name's use.
            return Ok(unsafe {
                CStr::from_bytes_with_nul_unchecked(std::slice::from_raw_parts(
                    pointer.cast(),
                    length + 1,
                ))
            });
        }
    }
    Err(ModuleError::ImageIdentity)
}
#[cfg(unix)]
fn pin_entry_image(entry: abi::PluginMain) -> Result<(usize, DynamicLibrary), ModuleError> {
    let address = entry as *const c_void;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // SAFETY: address is the admitted loaded entry symbol and output is writable
    // Dl_info storage. The caller retains its source/dependency loader pin.
    if unsafe { libc::dladdr(address, info.as_mut_ptr()) } == 0 {
        return Err(ModuleError::ImageIdentity);
    }
    // SAFETY: Successful dladdr initialized this fixed output record.
    let info = unsafe { info.assume_init() };
    if info.dli_fbase.is_null() || info.dli_saddr.cast_const() != address {
        return Err(ModuleError::ImageIdentity);
    }
    // SAFETY: Both names belong to the still-pinned defining image; these views
    // are consumed before the caller can release the source loader reference.
    let path = unsafe { bounded_os_name(info.dli_fname, MAX_PATH_BYTES)? };
    // SAFETY: Same defining-image storage, with a tighter export-name budget.
    let symbol = unsafe { bounded_os_name(info.dli_sname, MAX_SYMBOL_BYTES)? };
    // SAFETY: This bounded terminated path names an already mapped image.
    // NOLOAD acquires a reference without loading a replacement pathname image.
    let handle = unsafe {
        libc::dlopen(
            path.as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
        )
    };
    if handle.is_null() {
        return Err(ModuleError::ImageIdentity);
    }
    let pin = DynamicLibrary { handle };
    // Verify the acquired reference actually resolves the same defining symbol,
    // not merely a filename that happened to be reported by dladdr.
    if pin.symbol(symbol).cast_const() != address {
        return Err(ModuleError::ImageIdentity);
    }
    Ok((info.dli_fbase as usize, pin))
}
#[cfg(windows)]
fn pin_entry_image(entry: abi::PluginMain) -> Result<(usize, DynamicLibrary), ModuleError> {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleHandleExW,
    };
    let mut handle = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets this argument as an address, not UTF-16
    // text. Without UNCHANGED_REFCOUNT, success acquires a loader reference.
    let status = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            (entry as *const c_void).cast(),
            &mut handle,
        )
    };
    if status == 0 || handle.is_null() {
        return Err(ModuleError::ImageIdentity);
    }
    Ok((handle as usize, DynamicLibrary { handle }))
}
#[cfg(not(any(unix, windows)))]
fn pin_entry_image(_: abi::PluginMain) -> Result<(usize, DynamicLibrary), ModuleError> {
    Err(ModuleError::ImageIdentity)
}
