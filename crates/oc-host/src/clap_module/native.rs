//! Owned OS references and exact-ABI entry calls for the canonical slot driver.
use std::ffi::{CStr, CString, c_void};
use std::mem::ManuallyDrop;
use std::path::Path;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::clap_plugin_factory;

use super::{ModuleDriver, ModuleIdentity, ModuleLoadError, ModuleReleaseError};
use crate::clap_failure::ClapOperation;
use crate::clap_guard::{self, Fault};

pub(crate) struct NativeModuleDriver;

pub(crate) struct NativeModule {
    library: NativeLibrary,
    factory: *const clap_plugin_factory,
    deinit: unsafe extern "C" fn(),
    // Plugins may retain their init path until deinit; it stays at one address.
    init_path: ManuallyDrop<CString>,
}
// SAFETY: these immutable entry/factory addresses remain mapped through the
// owned library reference. The registry serializes init/deinit with all leases;
// CLAP declares factory callbacks thread-safe. Instance callbacks need their
// separate main/audio admission and retain a module lease until destruction.
unsafe impl Send for NativeModule {}
// SAFETY: sharing only exposes the thread-safe factory while the borrowed
// module lease prevents concurrent deinit or OS reference release.
unsafe impl Sync for NativeModule {}
impl NativeModule {
    pub(crate) fn factory(&self) -> *const clap_plugin_factory {
        self.factory
    }
}

impl ModuleDriver for NativeModuleDriver {
    type Module = NativeModule;

    fn load(
        &self,
        identity: &ModuleIdentity,
    ) -> Result<NativeModule, ModuleLoadError<NativeModule>> {
        let init_path = identity.init_path.to_str().ok_or_else(|| {
            ModuleLoadError::Rejected("CLAP initialization requires a UTF-8 plugin path".into())
        })?;
        let init_path = CString::new(init_path)
            .map_err(|_| ModuleLoadError::Rejected("The CLAP path contains a NUL byte".into()))?;
        let mut library =
            NativeLibrary::open(&identity.binary).map_err(ModuleLoadError::Rejected)?;
        let entry = library.symbol(c"clap_entry") as *const clap_plugin_entry;
        if entry.is_null() {
            return Err(ModuleLoadError::Rejected(
                "The library exports no clap_entry".into(),
            ));
        }
        // SAFETY: clap_entry is the documented immutable data export of this
        // mapped native module; unsupported or malicious native ABI memory is
        // outside exception containment and needs process isolation.
        let entry = unsafe { &*entry };
        if !clap_sys::version::clap_version_is_compatible(entry.clap_version) {
            return Err(ModuleLoadError::Rejected(
                "The plugin uses an incompatible CLAP version".into(),
            ));
        }
        let (Some(init), Some(deinit), Some(get_factory)) =
            (entry.init, entry.deinit, entry.get_factory)
        else {
            return Err(ModuleLoadError::Rejected(
                "The CLAP entry is missing a required lifecycle callback".into(),
            ));
        };
        // From the first foreign entry call onward, a Rust unwind or unexpected
        // early drop must retain code rather than unload uncertain workers.
        library.may_release_on_drop = false;
        let mut module = NativeModule {
            library,
            factory: std::ptr::null(),
            deinit,
            init_path: ManuallyDrop::new(init_path),
        };
        // SAFETY: the validated callback belongs to the owned mapped entry; this
        // slot has no other leases while init runs, and the path stays retained.
        match unsafe { clap_guard::entry_init(init, module.init_path.as_ptr()) } {
            Ok(true) => {}
            Ok(false) => {
                // A false init is uninitialized: never call deinit or factory.
                return match self.release(module) {
                    Ok(()) => Err(ModuleLoadError::Rejected(
                        "CLAP entry initialization returned false".into(),
                    )),
                    Err(error) => {
                        log::error!("CLAP uninitialized module release failed: {}", error.reason);
                        Err(ModuleLoadError::Quarantine {
                            operation: ClapOperation::EntryInit,
                            fault: Box::new(Fault::quarantined()),
                            module: error.module,
                        })
                    }
                };
            }
            Err(fault) => {
                return Err(ModuleLoadError::Quarantine {
                    operation: ClapOperation::EntryInit,
                    fault: Box::new(fault),
                    module,
                });
            }
        }
        // SAFETY: successful init admits factory calls and this module remains
        // mapped through the owned reference for the entire typed invocation.
        let factory =
            // SAFETY: Successful entry initialization admits this exact-signature factory callback; the owned module reference and static factory identifier remain live through return.
            match unsafe { clap_guard::factory(get_factory, c"clap.plugin-factory".as_ptr()) } {
                Ok(factory) => factory as *const clap_plugin_factory,
                Err(fault) => {
                    return Err(ModuleLoadError::Quarantine {
                        operation: ClapOperation::GetFactory,
                        fault: Box::new(fault),
                        module,
                    });
                }
            };
        if factory.is_null() {
            return self.reject_initialized(module, "The CLAP entry has no plugin factory");
        }
        module.factory = factory;
        // SAFETY: the factory was returned by the initialized owned module and
        // its immutable descriptor remains valid until this module is deinitialized.
        let callbacks = unsafe { &*factory };
        if callbacks.get_plugin_count.is_none()
            || callbacks.get_plugin_descriptor.is_none()
            || callbacks.create_plugin.is_none()
        {
            return self
                .reject_initialized(module, "The CLAP factory is missing a required callback");
        }
        Ok(module)
    }

    fn deinit(&self, module: &NativeModule) -> Result<(), Fault> {
        // SAFETY: the slot enters Retiring only after every scanner/instance/GUI
        // lease is gone, so no other CLAP call into this DSO can overlap deinit.
        unsafe { clap_guard::entry_void(module.deinit) }
    }

    fn release(&self, mut module: NativeModule) -> Result<(), ModuleReleaseError<NativeModule>> {
        match module.library.close() {
            Ok(()) => {
                // SAFETY: successful deinit (or a rejected false init) retired
                // every native user, and this unique payload frees the path once.
                unsafe { ManuallyDrop::drop(&mut module.init_path) };
                Ok(())
            }
            Err(reason) => Err(ModuleReleaseError { reason, module }),
        }
    }
}
impl NativeModuleDriver {
    fn reject_initialized(
        &self,
        module: NativeModule,
        reason: &str,
    ) -> Result<NativeModule, ModuleLoadError<NativeModule>> {
        if let Err(fault) = self.deinit(&module) {
            return Err(ModuleLoadError::Quarantine {
                operation: ClapOperation::EntryDeinit,
                fault: Box::new(fault),
                module,
            });
        }
        match self.release(module) {
            Ok(()) => Err(ModuleLoadError::Rejected(reason.into())),
            Err(error) => {
                log::error!("CLAP initialized module release failed: {}", error.reason);
                Err(ModuleLoadError::Quarantine {
                    operation: ClapOperation::EntryDeinit,
                    fault: Box::new(Fault::quarantined()),
                    module: error.module,
                })
            }
        }
    }
}

struct NativeLibrary {
    handle: *mut c_void,
    may_release_on_drop: bool,
}
impl NativeLibrary {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| "The CLAP binary path contains a NUL byte".to_owned())?;
            // SAFETY: the NUL-terminated path is borrowed for dlopen only.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                // SAFETY: dlerror returns a thread-local NUL-terminated message.
                let error = unsafe { libc::dlerror() };
                let message = if error.is_null() {
                    "unknown loader failure".into()
                } else {
                    // SAFETY: the non-null loader message is valid until the next loader call.
                    unsafe { CStr::from_ptr(error) }
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(format!("Failed to load CLAP module: {message}"));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self {
                handle,
                may_release_on_drop: true,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: the UTF-16 path is NUL-terminated and borrowed for this call.
            let handle =
                // SAFETY: The local UTF-16 path ends in NUL and remains readable for LoadLibraryW; the returned handle becomes uniquely owned by NativeLibrary.
                unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(path.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "Failed to load CLAP module: {}",
                    std::io::Error::last_os_error()
                ));
            }
            crate::mark_plugin_module_loaded();
            Ok(Self {
                handle,
                may_release_on_drop: true,
            })
        }
    }

    fn symbol(&self, name: &CStr) -> *mut c_void {
        #[cfg(unix)]
        // SAFETY: self owns the live dlopen reference; name is NUL-terminated.
        {
            // SAFETY: self owns the dlopen reference and this CStr name is NUL-terminated; the lookup borrows both until return.
            unsafe { libc::dlsym(self.handle, name.as_ptr()) }
        }
        #[cfg(windows)]
        // SAFETY: self owns the live LoadLibraryW reference; name is NUL-terminated.
        {
            // SAFETY: The owned LoadLibraryW reference keeps self.handle live; the borrowed CStr supplies a NUL-terminated symbol name.
            unsafe {
                windows_sys::Win32::System::LibraryLoader::GetProcAddress(
                    self.handle,
                    name.as_ptr().cast(),
                )
            }
            .map(|address| address as *mut c_void)
            .unwrap_or(std::ptr::null_mut())
        }
    }

    fn close(&mut self) -> Result<(), String> {
        if self.handle.is_null() {
            return Ok(());
        }
        #[cfg(unix)]
        // SAFETY: the caller retires all foreign users before transferring this
        // uniquely owned reference to dlclose. No image-eviction claim is made.
        let released = unsafe { libc::dlclose(self.handle) } == 0;
        #[cfg(windows)]
        // SAFETY: the caller retires all foreign users before releasing this
        // uniquely owned LoadLibraryW reference exactly once.
        let released = unsafe { windows_sys::Win32::Foundation::FreeLibrary(self.handle) } != 0;
        #[cfg(test)]
        OWNED_RELEASES.with(|counts| {
            let (calls, failures) = counts.get();
            counts.set((calls + 1, failures + usize::from(!released)));
        });
        if released {
            self.handle = std::ptr::null_mut();
            Ok(())
        } else {
            Err("The OS refused to release the owned CLAP module reference".into())
        }
    }
}
impl Drop for NativeLibrary {
    fn drop(&mut self) {
        if self.may_release_on_drop
            && let Err(error) = self.close()
        {
            log::error!("CLAP pre-entry module cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
thread_local! {
    static OWNED_RELEASES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clap_module::{ModuleRegistry, SlotState};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        registry: ModuleRegistry<NativeModuleDriver>,
        observer: NativeLibrary,
        root: PathBuf,
        path: PathBuf,
        configure: unsafe extern "C" fn(u32),
        counter: unsafe extern "C" fn(u32) -> u32,
    }
    impl Fixture {
        fn new(mode: u32) -> Self {
            let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "oc-clap-module-{}-{serial}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("fixture.clap");
            std::fs::copy(env!("OC_CLAP_MODULE_FIXTURE_PATH"), &path).unwrap();
            let observer = NativeLibrary::open(&path).unwrap();
            let configure = observer.symbol(c"oc_clap_module_configure");
            let counter = observer.symbol(c"oc_clap_module_counter");
            assert!(!configure.is_null() && !counter.is_null());
            // SAFETY: these named fake-DSO exports have exactly the signatures
            // declared in the committed fixture, and observer retains their code.
            let configure =
                // SAFETY: configure is a non-null export from the owned test DSO and its declared signature is exactly extern C fn(u32); observer retains its code.
                unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32)>(configure) };
            // SAFETY: this fixture's counter export has the exact u32 ABI below.
            let counter = unsafe {
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> u32>(counter)
            };
            // SAFETY: no registry lease or fixture callback exists yet.
            unsafe { configure(mode) };
            Self {
                registry: ModuleRegistry::new(NativeModuleDriver),
                observer,
                root,
                path,
                configure,
                counter,
            }
        }
        fn count(&self, kind: u32) -> u32 {
            // SAFETY: observer owns the loaded fake DSO until Fixture::drop;
            // this exact-signature export only reads its atomic counters.
            unsafe { (self.counter)(kind) }
        }
        fn counts(&self) -> [u32; 3] {
            // SAFETY: observer retains the fixture's atomic no-throw counters.
            unsafe { [(self.counter)(0), (self.counter)(1), (self.counter)(2)] }
        }
        fn configure(&self, mode: u32) {
            // SAFETY: tests call this only between native entry calls.
            unsafe { (self.configure)(mode) };
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.configure(0);
            // Only this synchronous fake DSO is reclaimed after assertions about
            // production quarantine. It has no plugin objects, workers, retained
            // callbacks, or allocations, so its pinned reference can be released.
            let slots = self
                .registry
                .inner
                .slots
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for slot in slots.values() {
                let mut state = slot.state.lock().unwrap_or_else(|error| error.into_inner());
                match std::mem::replace(&mut *state, SlotState::Idle) {
                    SlotState::Quarantined {
                        _module: Some(pinned),
                        ..
                    } => {
                        let module = ManuallyDrop::into_inner(pinned._module);
                        assert!(NativeModuleDriver.release(module).is_ok());
                    }
                    SlotState::Idle | SlotState::Quarantined { _module: None, .. } => {}
                    other => {
                        *state = other;
                        panic!("the native fixture still has an active lease at cleanup");
                    }
                }
            }
            drop(slots);
            self.observer.close().unwrap();
            if let Err(error) = std::fs::remove_dir_all(&self.root)
                && !std::thread::panicking()
            {
                panic!("failed to remove CLAP fixture root: {error}");
            }
        }
    }
    fn releases() -> (usize, usize) {
        OWNED_RELEASES.with(std::cell::Cell::get)
    }

    #[test]
    fn native_module_production_instances_service_offline_callbacks_and_retire_last_strong_lease() {
        crate::clap_main::ClapMainExecutor::with_fixture_current(|executor| {
            let fixture = Fixture::new(11);
            let _offline = crate::clap_main::ClapOfflineMainScope::enter().unwrap();
            let before = releases();
            let mut first = crate::clap_instance::ClapInstance::load(
                &fixture.path,
                "test.fixture.clap.silent",
                48000,
                32,
            )
            .unwrap();
            let second = crate::clap_instance::ClapInstance::load(
                &fixture.path,
                "test.fixture.clap.silent",
                48000,
                32,
            )
            .unwrap();
            assert_eq!(fixture.counts(), [1, 0, 1]);
            let mut output = [Vec::with_capacity(16)];
            first
                .process_block_outputs_into(8, None, &[0], &mut output)
                .unwrap();
            first
                .process_block_outputs_into(8, None, &[0], &mut output)
                .unwrap();
            assert!(output[0].iter().all(|value| *value == 0.0));
            // SAFETY: the observer keeps the optimized DSO counter code live.
            unsafe {
                assert_eq!((fixture.counter)(7), 2); // process
                assert_eq!((fixture.counter)(8), 4); // init x2 + process x2 callback
                assert_eq!((fixture.counter)(13), 0); // main/audio role violations
            }
            // An accepted control plan owns the same native Core as GUI leases.
            let retained = first.state_capture_plan();
            drop(first);
            drop(second);
            assert_eq!(fixture.count(11), 1); // only second destroyed
            assert_eq!(fixture.counts(), [1, 0, 1]);
            assert_eq!(releases(), before);
            drop(retained);
            executor.poll().unwrap();
            assert_eq!(fixture.count(11), 2);
            assert_eq!(fixture.count(13), 0);
            assert_eq!(fixture.counts(), [1, 1, 1]);
            assert_eq!(releases(), (before.0 + 1, before.1));
        });
    }

    #[test]
    fn native_module_transition_callbacks_and_flush_observe_published_main_audio_state() {
        crate::clap_main::ClapMainExecutor::with_fixture_current(|_| {
            let fixture = Fixture::new(13);
            let _offline = crate::clap_main::ClapOfflineMainScope::enter().unwrap();
            let mut instance = crate::clap_instance::ClapInstance::load(
                &fixture.path,
                "test.fixture.clap.silent",
                48000,
                32,
            )
            .unwrap();
            assert_eq!(
                fixture.count(12),
                1,
                "activate flush must use the active audio role"
            );
            let mut output = [Vec::with_capacity(16)];
            instance
                .process_block_outputs_into(8, None, &[0], &mut output)
                .unwrap();
            instance.stop_processing_now();
            assert_eq!(fixture.count(12), 3);
            instance.restart().unwrap();
            assert_eq!(
                fixture.count(12),
                5,
                "deactivate flush must use main; reactivate flush must use audio"
            );
            assert_eq!(fixture.count(13), 0);
            drop(instance);
            assert_eq!(fixture.counts(), [1, 1, 1]);
        });
    }

    #[test]
    fn native_module_without_offline_scope_does_not_poll_during_processing() {
        crate::clap_main::ClapMainExecutor::with_fixture_current(|executor| {
            let fixture = Fixture::new(0);
            let mut instance = crate::clap_instance::ClapInstance::load(
                &fixture.path,
                "test.fixture.clap.silent",
                48000,
                32,
            )
            .unwrap();
            executor.poll().unwrap();
            assert_eq!(fixture.count(8), 1);
            let mut output = [Vec::with_capacity(16)];
            instance
                .process_block_outputs_into(8, None, &[0], &mut output)
                .unwrap();
            crate::clap_main::service_offline_main().unwrap();
            // No main callback runs inside ordinary GUI/worker processing.
            assert_eq!(fixture.count(8), 1);
            executor.poll().unwrap();
            assert_eq!(fixture.count(8), 2);
            assert_eq!(fixture.count(13), 0);
            drop(instance);
            assert_eq!(fixture.counts(), [1, 1, 1]);
        });
    }

    #[test]
    fn native_module_failed_plugin_init_destroys_without_main_callback_or_activation() {
        crate::clap_main::ClapMainExecutor::with_fixture_current(|_| {
            let fixture = Fixture::new(12);
            let _offline = crate::clap_main::ClapOfflineMainScope::enter().unwrap();
            assert!(
                crate::clap_instance::ClapInstance::load(
                    &fixture.path,
                    "test.fixture.clap.silent",
                    48000,
                    32,
                )
                .is_err()
            );
            assert_eq!(fixture.count(5), 0); // activate
            assert_eq!(fixture.count(8), 0); // on_main_thread
            assert_eq!(fixture.count(11), 1); // destroy
            assert_eq!(fixture.counts(), [1, 1, 1]);
        });
    }

    #[test]
    fn native_module_final_lease_deinitializes_and_releases_one_real_os_reference() {
        let fixture = Fixture::new(0);
        let first = fixture.registry.acquire(&fixture.path).unwrap();
        let second = fixture.registry.acquire(&fixture.path).unwrap();
        assert_eq!(fixture.counts(), [1, 0, 1]);
        let before = releases();
        drop(first);
        assert_eq!(releases(), before);
        drop(second);
        assert_eq!(fixture.counts(), [1, 1, 1]);
        assert_eq!(releases(), (before.0 + 1, before.1));
        // The independent observer keeps the image mapped. The assertion is
        // about releasing the driver's reference, not dyld image eviction.
    }

    #[test]
    fn native_module_false_init_never_calls_factory_or_deinit() {
        let fixture = Fixture::new(1);
        let before = releases();
        assert!(fixture.registry.acquire(&fixture.path).is_err());
        assert_eq!(fixture.counts(), [1, 0, 0]);
        assert_eq!(releases(), (before.0 + 1, before.1));
    }

    #[test]
    fn native_module_optimized_init_and_factory_exceptions_pin_the_owned_reference() {
        for (mode, expected, operation) in
            [(2, [1, 0, 0], "EntryInit"), (3, [1, 0, 1], "GetFactory")]
        {
            let fixture = Fixture::new(mode);
            let before = releases();
            let error = fixture.registry.acquire(&fixture.path).err().unwrap();
            assert!(error.contains(operation) && error.contains("fake entry"));
            assert_eq!(fixture.counts(), expected);
            assert_eq!(releases(), before);
            assert!(fixture.registry.acquire(&fixture.path).is_err());
            assert_eq!(fixture.counts(), expected);
        }
    }

    #[test]
    fn native_module_throwing_deinit_reports_original_cause_and_keeps_code_mapped() {
        let fixture = Fixture::new(4);
        let lease = fixture.registry.acquire(&fixture.path).unwrap();
        let failure = lease.failure();
        let before = releases();
        drop(lease);
        assert_eq!(fixture.counts(), [1, 1, 1]);
        assert_eq!(releases(), before);
        let report = failure.take_report().unwrap();
        assert_eq!(report.operation, ClapOperation::EntryDeinit);
        assert!(
            report
                .fault
                .describe_for("CLAP")
                .contains("fake entry deinit exception")
        );
        assert!(failure.take_report().is_none());
        assert!(fixture.registry.acquire(&fixture.path).is_err());
        assert_eq!(fixture.counts(), [1, 1, 1]);
    }

    #[test]
    fn native_module_missing_factory_or_method_retires_initialized_entry_once() {
        for mode in [5, 6] {
            let fixture = Fixture::new(mode);
            let before = releases();
            assert!(fixture.registry.acquire(&fixture.path).is_err());
            assert_eq!(fixture.counts(), [1, 1, 1]);
            assert_eq!(releases(), (before.0 + 1, before.1));
            fixture.configure(0);
            drop(fixture.registry.acquire(&fixture.path).unwrap());
            assert_eq!(fixture.counts(), [1, 1, 1]);
        }
    }

    #[test]
    fn native_module_rejects_incompatible_version_and_missing_entry_methods_before_init() {
        for mode in [7, 8, 9, 10] {
            let fixture = Fixture::new(mode);
            let before = releases();
            assert!(fixture.registry.acquire(&fixture.path).is_err());
            assert_eq!(fixture.counts(), [0, 0, 0]);
            assert_eq!(releases(), (before.0 + 1, before.1));
        }
    }
}
