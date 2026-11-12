//! CFBundle context lifetime and nonblocking initialization admission.
use super::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        directory: u8,
    ) -> *mut c_void;
    fn CFBundleCreate(allocator: *const c_void, url: *mut c_void) -> *mut c_void;
    fn CFRelease(value: *mut c_void);
}

enum BundleState {
    Entering {
        bundle: usize,
    },
    Ready {
        bundle: usize,
    },
    Quarantined {
        _bundle: usize,
        failure: Arc<Vst3Failure>,
    },
}
static BUNDLES: LazyLock<Mutex<HashMap<PathBuf, BundleState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn bundle_key(path: &Path) -> Option<PathBuf> {
    let directory = path
        .ancestors()
        .find(|parent| parent.extension().is_some_and(|ext| ext == "vst3"))?;
    Some(
        directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf()),
    )
}
fn reserve(key: &Path, failure: &Vst3Failure) -> Result<bool, String> {
    let mut bundles = BUNDLES
        .lock()
        .map_err(|_| "bundle initialization registry is unavailable")?;
    match bundles.get(key) {
        Some(BundleState::Entering { .. }) => {
            Err("bundle initialization is already in progress".into())
        }
        Some(BundleState::Ready { .. }) => Ok(false),
        Some(BundleState::Quarantined { failure: prior, .. }) => {
            if let Some(cause) = prior.first_fault() {
                failure.slot.record(cause);
            }
            healthy(prior)?;
            Ok(false)
        }
        None => {
            bundles.insert(key.to_path_buf(), BundleState::Entering { bundle: 0 });
            Ok(true)
        }
    }
}
fn clear_reservation(key: &Path) {
    if let Ok(mut bundles) = BUNDLES.lock() {
        bundles.remove(key);
    }
}
fn stage(key: PathBuf, bundle: *mut c_void, failure: &Arc<Vst3Failure>) -> Result<(), String> {
    let state = if failure.failed() {
        BundleState::Quarantined {
            _bundle: bundle as usize,
            failure: Arc::clone(failure),
        }
    } else {
        BundleState::Entering {
            bundle: bundle as usize,
        }
    };
    // On lock poisoning the raw bundle and module intentionally remain resident;
    // the existing Entering marker refuses any further initialization attempt.
    BUNDLES
        .lock()
        .map_err(|_| "bundle initialization registry is unavailable")?
        .insert(key, state);
    Ok(())
}
fn quarantine(key: &Path, failure: &Arc<Vst3Failure>) {
    if let Ok(mut bundles) = BUNDLES.lock() {
        let bundle = match bundles.get(key) {
            Some(BundleState::Ready { bundle, .. } | BundleState::Entering { bundle }) => *bundle,
            Some(BundleState::Quarantined { _bundle, .. }) => *_bundle,
            _ => 0,
        };
        bundles.insert(
            key.to_path_buf(),
            BundleState::Quarantined {
                _bundle: bundle,
                failure: Arc::clone(failure),
            },
        );
    }
}

pub(super) struct Initialization {
    key: Option<PathBuf>,
    entered_here: bool,
}

pub(super) fn commit_acquisition(
    initialization: &Initialization,
    failure: &Arc<Vst3Failure>,
) -> Result<(), String> {
    healthy(failure)?;
    if !initialization.entered_here {
        return Ok(());
    }
    let key = initialization
        .key
        .as_ref()
        .expect("completed bundle entry has its captured key");
    let result = (|| {
        let mut bundles = BUNDLES
            .lock()
            .map_err(|_| "bundle initialization registry is unavailable")?;
        let Some(BundleState::Entering { bundle }) = bundles.get(key) else {
            return Err("bundle acquisition lost its initialization reservation");
        };
        let bundle = *bundle;
        bundles.insert(key.clone(), BundleState::Ready { bundle });
        Ok(())
    })();
    result.map_err(|_| {
        record(
            failure,
            Operation::GetPluginFactory,
            Fault::host_contract("bundle acquisition publication failed"),
        )
    })
}

pub(super) fn initialize(
    handle: *mut c_void,
    path: &Path,
    failure: &Arc<Vst3Failure>,
) -> Result<Initialization, String> {
    healthy(failure)?;
    let Some(key) = bundle_key(path) else {
        return Ok(Initialization {
            key: None,
            entered_here: false,
        });
    };
    // SAFETY: caller holds the loader reference and the ABI export name is fixed.
    let entry = unsafe { symbol(handle, ENTRY_NAME) };
    if entry.is_null() || !reserve(&key, failure)? {
        return Ok(Initialization {
            key: Some(key),
            entered_here: false,
        });
    }
    use std::os::unix::ffi::OsStrExt;
    let bytes = key.as_os_str().as_bytes();
    let length = match isize::try_from(bytes.len()) {
        Ok(length) => length,
        Err(_) => {
            clear_reservation(&key);
            return Err("bundle path exceeds platform size".into());
        }
    };
    // SAFETY: exact OS path bytes remain alive for the call and fit CFIndex.
    // The Create rule transfers one owned CFURL reference on success.
    let url =
        unsafe { CFURLCreateFromFileSystemRepresentation(ptr::null(), bytes.as_ptr(), length, 1) };
    if url.is_null() {
        clear_reservation(&key);
        return Err("could not create bundle URL".into());
    }
    // SAFETY: url is the live CFURL above; CFBundleCreate retains/copies its
    // URL context as needed and transfers one independent bundle retain.
    let bundle = unsafe { CFBundleCreate(ptr::null(), url) };
    // SAFETY: exactly the URL's successful Create-rule retain, no longer needed
    // after CFBundleCreate. This release occurs before any plugin entry call.
    unsafe { CFRelease(url) };
    if bundle.is_null() {
        clear_reservation(&key);
        return Err("could not create bundle context".into());
    }
    match enter_symbol(entry, bundle, failure) {
        Ok(true) => {
            if stage(key.clone(), bundle, failure).is_err() {
                // Initialization succeeded but publication failed: do not unload
                // a module whose native entry is still active.
                return Err(record(
                    failure,
                    Operation::BundleEntry,
                    Fault::host_contract("bundle context publication failed"),
                ));
            }
            Ok(Initialization {
                key: Some(key),
                entered_here: true,
            })
        }
        Ok(false) => {
            clear_reservation(&key);
            // SAFETY: ordinary false entry did not initialize the module; no
            // foreign fault occurred and this is our sole bundle Create retain.
            unsafe { CFRelease(bundle) };
            Err("the module rejected its bundle entry call".into())
        }
        Err(error) => {
            let _ = stage(key, bundle, failure);
            // Native entry may retain the bundle or have partially initialized:
            // neither CFRelease nor dlclose is permitted after its fault.
            Err(error)
        }
    }
}

pub(super) fn cleanup_failed_acquisition(
    handle: *mut c_void,
    initialization: &Initialization,
    failure: &Arc<Vst3Failure>,
) -> Result<(), String> {
    let Some(key) = initialization.key.as_deref() else {
        return healthy(failure);
    };
    if failure.failed() {
        quarantine(key, failure);
        return healthy(failure);
    }
    if !initialization.entered_here {
        return Ok(());
    }
    // SAFETY: retained loader handle and the fixed zero-argument bundleExit ABI.
    let exit = unsafe { symbol(handle, EXIT_NAME) };
    if exit.is_null() {
        let error = record(
            failure,
            Operation::ModuleExit,
            Fault::host_contract("initialized bundle has no exit export"),
        );
        quarantine(key, failure);
        return Err(error);
    }
    if let Err(error) = exit_symbol(exit, failure) {
        quarantine(key, failure);
        return Err(error);
    }
    // Remove pure registry state only after successful native cleanup. No lock
    // is held across plugin code or CF release.
    let state = BUNDLES
        .lock()
        .map_err(|_| "bundle initialization registry is unavailable")?
        .remove(key);
    if let Some(BundleState::Entering { bundle }) = state {
        // SAFETY: this fresh module's entry was reversed while healthy; no factory
        // was published. Release its one retained bundle context exactly once.
        unsafe { CFRelease(bundle as *mut c_void) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vst3_instance::native_fixture::factory_audit::FactoryFixture;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_KEY: AtomicU64 = AtomicU64::new(0);
    struct TestReservation(PathBuf);
    impl TestReservation {
        fn new() -> Self {
            Self(PathBuf::from(format!(
                "loader-admission-{}-{}.vst3",
                std::process::id(),
                NEXT_KEY.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for TestReservation {
        fn drop(&mut self) {
            clear_reservation(&self.0);
        }
    }

    #[test]
    fn bundle_reservation_excludes_other_acquisitions_until_first_factory_commits() {
        let key = TestReservation::new();
        let failure = Arc::new(Vst3Failure::default());
        let fixture = FactoryFixture::new();
        fixture.configure(0, 0, 1, false, false);
        assert!(reserve(&key.0, &failure).unwrap());
        assert!(enter_symbol(fixture.symbol(9), fixture.raw().cast(), &failure).unwrap());
        // This fixture address is only registry context metadata, never passed
        // to CoreFoundation. Native entry/factory/refcount calls are real.
        stage(key.0.clone(), fixture.raw().cast(), &failure).unwrap();
        let competing_key = key.0.clone();
        assert!(
            std::thread::spawn(move || reserve(&competing_key, &Vst3Failure::default()))
                .join()
                .unwrap()
                .is_err()
        );
        assert_eq!(fixture.calls(7), 0);
        let first = acquire_factory(fixture.symbol(7), &failure).unwrap();
        let initialization = Initialization {
            key: Some(key.0.clone()),
            entered_here: true,
        };
        commit_acquisition(&initialization, &failure).unwrap();
        assert!(!reserve(&key.0, &Vst3Failure::default()).unwrap());
        let second = acquire_factory(fixture.symbol(7), &Arc::new(Vst3Failure::default())).unwrap();
        assert_eq!(fixture.refs(), 3);
        drop(first);
        drop(second);
        assert_eq!(fixture.refs(), 1);
        assert_eq!(fixture.calls(6), 0);
    }

    #[test]
    fn fresh_factory_refusal_cannot_clean_up_under_a_competing_published_factory() {
        let key = TestReservation::new();
        let failure = Arc::new(Vst3Failure::default());
        let fixture = FactoryFixture::new();
        fixture.configure(0, 0, 1, false, false);
        assert!(reserve(&key.0, &failure).unwrap());
        assert!(enter_symbol(fixture.symbol(9), fixture.raw().cast(), &failure).unwrap());
        stage(key.0.clone(), fixture.raw().cast(), &failure).unwrap();
        fixture.configure(7, 0, 1, false, true);
        assert!(reserve(&key.0, &Vst3Failure::default()).is_err());
        assert_eq!(fixture.calls(7), 0);
        assert!(acquire_factory(fixture.symbol(7), &failure).is_err());
        assert!(!failure.failed());
        // The first attempt remains exclusive through its healthy failure
        // cleanup. Only after exit and reservation removal can another enter.
        assert!(reserve(&key.0, &Vst3Failure::default()).is_err());
        fixture.configure(0, 0, 1, false, false);
        exit_symbol(fixture.symbol(6), &failure).unwrap();
        clear_reservation(&key.0);
        assert!(reserve(&key.0, &Vst3Failure::default()).unwrap());
        assert_eq!(fixture.refs(), 1);
        assert_eq!(fixture.calls(6), 1);
    }
}
