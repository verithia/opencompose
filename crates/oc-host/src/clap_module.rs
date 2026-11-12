//! Canonical CLAP module slots serialize initialization and final retirement.
//!
//! Leases count scanners and native instance cores. GUI handles retain the core,
//! so the final lease cannot retire code that a queued editor operation needs.
#![expect(
    clippy::result_large_err,
    reason = "CLAP faults retain their fixed 256-byte cause inline so native/audio failure publication never allocates"
)]

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};
use std::time::SystemTime;

use crate::clap_failure::{ClapFailure, ClapModuleFailure, ClapOperation};
use crate::clap_guard::Fault;

mod native;
pub(crate) use native::NativeModuleDriver;
pub(crate) type NativeModuleLease = ModuleLease<NativeModuleDriver>;

pub(crate) fn acquire_native_module(path: &Path) -> Result<NativeModuleLease, String> {
    static MODULES: std::sync::OnceLock<ModuleRegistry<NativeModuleDriver>> =
        std::sync::OnceLock::new();
    MODULES
        .get_or_init(|| ModuleRegistry::new(NativeModuleDriver))
        .acquire(path)
}

const MAX_MODULE_SLOTS: usize = 256;
const MAX_MODULE_LEASES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ModuleIdentity {
    pub(crate) binary: PathBuf,
    pub(crate) init_path: PathBuf,
    fingerprint: Fingerprint,
}
impl ModuleIdentity {
    pub(crate) fn resolve(path: &Path) -> Result<Self, String> {
        let init_path = path.canonicalize().map_err(|error| error.to_string())?;
        let binary = crate::bundle::binary_path(&init_path)
            .or_else(|| {
                let stem = init_path.file_stem()?.to_str()?;
                let candidate = init_path.join("Contents/MacOS").join(format!("lib{stem}"));
                candidate.is_file().then_some(candidate)
            })
            .ok_or_else(|| "The CLAP bundle has no loadable binary for this platform".to_owned())?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let metadata = binary.metadata().map_err(|error| error.to_string())?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let fingerprint = Fingerprint {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        };
        Ok(Self {
            binary,
            init_path,
            fingerprint,
        })
    }
}

pub(crate) enum ModuleLoadError<M> {
    /// The driver releases any acquired OS reference before returning rejection.
    Rejected(String),
    /// Native initialization/factory state is uncertain; retain the OS reference.
    Quarantine {
        operation: ClapOperation,
        fault: Box<Fault>,
        module: M,
    },
}

/// Native implementations use exact-signature guards for init/deinit/factory
/// calls. They pin any partially entered module if their Rust frame unwinds.
pub(crate) struct ModuleReleaseError<M> {
    pub(crate) reason: String,
    pub(crate) module: M,
}

pub(crate) trait ModuleDriver: Send + Sync + 'static {
    type Module: Send + Sync + 'static;
    fn load(
        &self,
        identity: &ModuleIdentity,
    ) -> Result<Self::Module, ModuleLoadError<Self::Module>>;
    fn deinit(&self, module: &Self::Module) -> Result<(), Fault>;
    /// Release exactly this driver's acquired OS reference, without panicking.
    fn release(&self, module: Self::Module) -> Result<(), ModuleReleaseError<Self::Module>>;
}

struct Loaded<M> {
    module: M,
    identity: ModuleIdentity,
    authority: Arc<ClapModuleFailure>,
    health: ClapFailure,
}
struct Pinned<M> {
    _module: ManuallyDrop<M>,
}
impl<M> Pinned<M> {
    fn new(module: M) -> Self {
        Self {
            _module: ManuallyDrop::new(module),
        }
    }
}

enum SlotState<M> {
    Idle,
    Loading(ThreadId),
    Ready {
        loaded: Arc<Loaded<M>>,
        leases: usize,
    },
    Retiring(ThreadId),
    Quarantined {
        _module: Option<Pinned<M>>,
        reason: String,
    },
}
struct ModuleSlot<M> {
    state: Mutex<SlotState<M>>,
    changed: Condvar,
}
impl<M> Default for ModuleSlot<M> {
    fn default() -> Self {
        Self {
            state: Mutex::new(SlotState::Idle),
            changed: Condvar::new(),
        }
    }
}

pub(crate) struct ModuleRegistry<D: ModuleDriver> {
    inner: Arc<RegistryInner<D>>,
}
struct RegistryInner<D: ModuleDriver> {
    driver: D,
    // Slots remain canonical even when idle. Removing a slot while a waiter
    // holds it could create a second initialization authority for the same DSO.
    slots: Mutex<HashMap<PathBuf, Arc<ModuleSlot<D::Module>>>>,
}
impl<D: ModuleDriver> ModuleRegistry<D> {
    pub(crate) fn new(driver: D) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                driver,
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn acquire(&self, path: &Path) -> Result<ModuleLease<D>, String> {
        self.acquire_identity(ModuleIdentity::resolve(path)?)
    }

    fn acquire_identity(&self, identity: ModuleIdentity) -> Result<ModuleLease<D>, String> {
        let slot = {
            let mut slots = self
                .inner
                .slots
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(slot) = slots.get(&identity.binary) {
                Arc::clone(slot)
            } else {
                if slots.len() >= MAX_MODULE_SLOTS {
                    return Err(
                        "The CLAP module capacity is reached; restart before loading more modules"
                            .into(),
                    );
                }
                let slot = Arc::new(ModuleSlot::default());
                slots.insert(identity.binary.clone(), Arc::clone(&slot));
                slot
            }
        };
        let current = thread::current().id();
        let mut state = slot.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            match &mut *state {
                SlotState::Ready { loaded, leases } => {
                    if loaded.identity.fingerprint != identity.fingerprint {
                        return Err("The loaded CLAP binary changed on disk; restart before using the replacement".into());
                    }
                    if loaded.health.failed() {
                        return Err("The CLAP module is quarantined after a native fault".into());
                    }
                    if *leases >= MAX_MODULE_LEASES {
                        return Err("The CLAP module instance capacity is reached".into());
                    }
                    *leases += 1;
                    let loaded = Arc::clone(loaded);
                    drop(state);
                    return Ok(ModuleLease {
                        registry: Arc::clone(&self.inner),
                        slot,
                        loaded: Some(loaded),
                    });
                }
                SlotState::Loading(owner) | SlotState::Retiring(owner) => {
                    if *owner == current {
                        return Err(
                            "A CLAP module cannot recursively initialize or retire itself".into(),
                        );
                    }
                    state = slot
                        .changed
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner());
                }
                SlotState::Quarantined { reason, .. } => return Err(reason.clone()),
                SlotState::Idle => {
                    *state = SlotState::Loading(current);
                    break;
                }
            }
        }
        drop(state);
        let mut transition = LoadingTransition {
            slot: Arc::clone(&slot),
            armed: true,
        };
        // Neither the global registry nor its slot mutex is held in plugin code.
        let result = self.inner.driver.load(&identity);
        let mut state = slot.state.lock().unwrap_or_else(|error| error.into_inner());
        let result = match result {
            Ok(module) => {
                let authority = Arc::new(ClapModuleFailure::default());
                let loaded = Arc::new(Loaded {
                    module,
                    identity,
                    health: ClapFailure::with_module(Arc::clone(&authority)),
                    authority,
                });
                *state = SlotState::Ready {
                    loaded: Arc::clone(&loaded),
                    leases: 1,
                };
                Ok(ModuleLease {
                    registry: Arc::clone(&self.inner),
                    slot: Arc::clone(&slot),
                    loaded: Some(loaded),
                })
            }
            Err(ModuleLoadError::Rejected(reason)) => {
                *state = SlotState::Idle;
                Err(reason)
            }
            Err(ModuleLoadError::Quarantine {
                operation,
                fault,
                module,
            }) => {
                let reason = format!("CLAP {operation:?}: {}", fault.describe_for("CLAP"));
                *state = SlotState::Quarantined {
                    _module: Some(Pinned::new(module)),
                    reason: reason.clone(),
                };
                Err(reason)
            }
        };
        transition.armed = false;
        drop(state);
        slot.changed.notify_all();
        result
    }
}

struct LoadingTransition<M> {
    slot: Arc<ModuleSlot<M>>,
    armed: bool,
}
impl<M> Drop for LoadingTransition<M> {
    fn drop(&mut self) {
        if self.armed {
            *self
                .slot
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = SlotState::Quarantined {
                _module: None,
                reason: "CLAP initialization aborted; restart before loading this module".into(),
            };
            self.slot.changed.notify_all();
        }
    }
}

/// Clone/acquire only on preparation/control threads. GUI handles clone their
/// strong instance core; they do not lock this registry from the audio callback.
pub(crate) struct ModuleLease<D: ModuleDriver> {
    registry: Arc<RegistryInner<D>>,
    slot: Arc<ModuleSlot<D::Module>>,
    loaded: Option<Arc<Loaded<D::Module>>>,
}
impl<D: ModuleDriver> ModuleLease<D> {
    pub(crate) fn module(&self) -> &D::Module {
        &self.loaded.as_ref().expect("live module lease").module
    }
    pub(crate) fn failure(&self) -> Arc<ClapFailure> {
        Arc::new(ClapFailure::with_module(Arc::clone(
            &self.loaded.as_ref().expect("live module lease").authority,
        )))
    }
}
impl<D: ModuleDriver> Drop for ModuleLease<D> {
    fn drop(&mut self) {
        let Some(own_loaded) = self.loaded.take() else {
            return;
        };
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let SlotState::Ready { leases, .. } = &mut *state else {
            // This is an internal ownership failure. Preserve executable code.
            std::mem::forget(own_loaded);
            return;
        };
        *leases -= 1;
        if *leases != 0 {
            return;
        }
        let SlotState::Ready { loaded, .. } =
            std::mem::replace(&mut *state, SlotState::Retiring(thread::current().id()))
        else {
            unreachable!()
        };
        drop(state);
        // A peer may have decremented `leases` but not yet dropped its local
        // Arc. Give it a bounded window so retirement does not quarantine a
        // healthy module under scheduling pressure.
        for _ in 0..64 {
            if Arc::strong_count(&loaded) == 1 {
                break;
            }
            std::thread::yield_now();
        }
        drop(own_loaded);
        let loaded = match Arc::try_unwrap(loaded) {
            Ok(loaded) => loaded,
            Err(loaded) => {
                std::mem::forget(loaded);
                *self
                    .slot
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = SlotState::Quarantined {
                    _module: None,
                    reason: "CLAP module ownership did not quiesce before retirement".into(),
                };
                self.slot.changed.notify_all();
                return;
            }
        };
        let mut retirement = RetirementTransition {
            slot: Arc::clone(&self.slot),
            module: Some(loaded.module),
            armed: true,
        };
        let fault = if loaded.health.failed() {
            Some(
                loaded
                    .health
                    .first_fault()
                    .map(|record| record.fault)
                    .unwrap_or_else(Fault::quarantined),
            )
        } else {
            self.registry
                .driver
                .deinit(retirement.module.as_ref().expect("retiring module"))
                .err()
        };
        let state = if let Some(fault) = fault {
            if loaded.authority.record(ClapOperation::EntryDeinit, fault) {
                log::error!(
                    "CLAP module retirement failed: {}",
                    fault.describe_for("CLAP")
                );
            }
            SlotState::Quarantined {
                _module: retirement.module.take().map(Pinned::new),
                reason: format!(
                    "CLAP module retirement is quarantined: {}",
                    fault.describe_for("CLAP")
                ),
            }
        } else {
            match self
                .registry
                .driver
                .release(retirement.module.take().expect("retiring module"))
            {
                Ok(()) => SlotState::Idle,
                Err(error) => {
                    log::error!("CLAP module reference release failed: {}", error.reason);
                    SlotState::Quarantined {
                        _module: Some(Pinned::new(error.module)),
                        reason: error.reason,
                    }
                }
            }
        };
        *self
            .slot
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = state;
        retirement.armed = false;
        self.slot.changed.notify_all();
    }
}
struct RetirementTransition<M> {
    slot: Arc<ModuleSlot<M>>,
    module: Option<M>,
    armed: bool,
}
impl<M> Drop for RetirementTransition<M> {
    fn drop(&mut self) {
        if self.armed {
            *self
                .slot
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = SlotState::Quarantined {
                _module: self.module.take().map(Pinned::new),
                reason: "CLAP module retirement aborted; restart before loading this module".into(),
            };
            self.slot.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counts {
        loads: AtomicUsize,
        deinits: AtomicUsize,
        releases: AtomicUsize,
        reject: AtomicBool,
        quarantine_load: AtomicBool,
        throw_deinit: AtomicBool,
        panic_load: AtomicBool,
        panic_deinit: AtomicBool,
        events: Mutex<Vec<&'static str>>,
        retire_gate: Mutex<Option<Arc<RetireGate>>>,
    }
    struct RetireGate {
        entered: std::sync::mpsc::Sender<()>,
        open: Mutex<bool>,
        changed: Condvar,
    }
    struct Driver(Arc<Counts>);
    impl ModuleDriver for Driver {
        // An integer stands in for a native reference; pinned test modules own
        // no heap allocation, OS handle, or callback storage to leak.
        type Module = usize;
        fn load(&self, _: &ModuleIdentity) -> Result<usize, ModuleLoadError<usize>> {
            assert!(
                !self.0.panic_load.load(Ordering::Relaxed),
                "injected load panic"
            );
            let id = self.0.loads.fetch_add(1, Ordering::Relaxed) + 1;
            self.0.events.lock().unwrap().push("init");
            if self.0.quarantine_load.load(Ordering::Relaxed) {
                return Err(ModuleLoadError::Quarantine {
                    operation: ClapOperation::EntryInit,
                    fault: Box::new(Fault::callback_panic()),
                    module: id,
                });
            }
            if self.0.reject.load(Ordering::Relaxed) {
                assert!(self.release(id).is_ok());
                return Err(ModuleLoadError::Rejected(
                    "Entry init returned false".into(),
                ));
            }
            Ok(id)
        }
        fn deinit(&self, _: &usize) -> Result<(), Fault> {
            self.0.deinits.fetch_add(1, Ordering::Relaxed);
            self.0.events.lock().unwrap().push("deinit.begin");
            assert!(
                !self.0.panic_deinit.load(Ordering::Relaxed),
                "injected deinit panic"
            );
            let gate = self.0.retire_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.entered.send(()).unwrap();
                let mut open = gate.open.lock().unwrap();
                while !*open {
                    open = gate.changed.wait(open).unwrap();
                }
            }
            self.0.events.lock().unwrap().push("deinit.end");
            if self.0.throw_deinit.load(Ordering::Relaxed) {
                Err(Fault::callback_panic())
            } else {
                Ok(())
            }
        }
        fn release(&self, _: usize) -> Result<(), ModuleReleaseError<usize>> {
            self.0.releases.fetch_add(1, Ordering::Relaxed);
            self.0.events.lock().unwrap().push("release");
            Ok(())
        }
    }
    fn identity(name: &str) -> ModuleIdentity {
        ModuleIdentity {
            binary: PathBuf::from(name),
            init_path: PathBuf::from(name),
            fingerprint: Fingerprint {
                length: 100,
                modified: None,
                #[cfg(unix)]
                device: 1,
                #[cfg(unix)]
                inode: 1,
            },
        }
    }
    fn registry() -> (ModuleRegistry<Driver>, Arc<Counts>) {
        let counts = Arc::new(Counts::default());
        (ModuleRegistry::new(Driver(Arc::clone(&counts))), counts)
    }

    #[test]
    fn concurrent_leases_share_one_initialization_and_final_release() {
        let (registry, counts) = registry();
        let registry = Arc::new(registry);
        let acquired = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let registry = Arc::clone(&registry);
                let acquired = Arc::clone(&acquired);
                std::thread::spawn(move || {
                    let lease = registry.acquire_identity(identity("shared.clap")).unwrap();
                    assert_eq!(*lease.module(), 1);
                    acquired.wait();
                    drop(lease);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(counts.loads.load(Ordering::Relaxed), 1);
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 1);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reinitialization_waits_for_last_lease_deinit_and_os_release() {
        let (registry, counts) = registry();
        let registry = Arc::new(registry);
        let lease = registry
            .acquire_identity(identity("retiring.clap"))
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let gate = Arc::new(RetireGate {
            entered: entered_tx,
            open: Mutex::new(false),
            changed: Condvar::new(),
        });
        *counts.retire_gate.lock().unwrap() = Some(Arc::clone(&gate));
        let retiring = std::thread::spawn(move || drop(lease));
        entered_rx.recv().unwrap();
        let next_registry = Arc::clone(&registry);
        let acquiring = std::thread::spawn(move || {
            next_registry
                .acquire_identity(identity("retiring.clap"))
                .unwrap()
        });
        *gate.open.lock().unwrap() = true;
        gate.changed.notify_all();
        retiring.join().unwrap();
        let next = acquiring.join().unwrap();
        assert_eq!(*next.module(), 2);
        assert_eq!(
            *counts.events.lock().unwrap(),
            ["init", "deinit.begin", "deinit.end", "release", "init"]
        );
        *counts.retire_gate.lock().unwrap() = None;
        drop(next);
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 2);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rejected_init_releases_without_deinit_and_allows_retry() {
        let (registry, counts) = registry();
        counts.reject.store(true, Ordering::Relaxed);
        assert!(registry.acquire_identity(identity("reject.clap")).is_err());
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 0);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
        counts.reject.store(false, Ordering::Relaxed);
        drop(registry.acquire_identity(identity("reject.clap")).unwrap());
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 1);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn uncertain_initialization_retains_the_reference_and_cannot_retry() {
        let (registry, counts) = registry();
        counts.quarantine_load.store(true, Ordering::Relaxed);
        let error = registry
            .acquire_identity(identity("init-fault.clap"))
            .err()
            .unwrap();
        assert!(error.contains("EntryInit"));
        counts.quarantine_load.store(false, Ordering::Relaxed);
        assert!(
            registry
                .acquire_identity(identity("init-fault.clap"))
                .is_err()
        );
        assert_eq!(counts.loads.load(Ordering::Relaxed), 1);
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 0);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn factory_uncertainty_faults_all_children_and_prevents_deinit_or_release() {
        let (registry, counts) = registry();
        let first = registry.acquire_identity(identity("faulted.clap")).unwrap();
        let second = registry.acquire_identity(identity("faulted.clap")).unwrap();
        let first_failure = first.failure();
        let second_failure = second.failure();
        first_failure.record(ClapOperation::CreatePlugin, Fault::callback_panic());
        assert!(second_failure.failed());
        assert!(registry.acquire_identity(identity("faulted.clap")).is_err());
        drop(first);
        drop(second);
        assert_eq!(counts.deinits.load(Ordering::Relaxed), 0);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 0);
        assert!(registry.acquire_identity(identity("faulted.clap")).is_err());
        assert_eq!(
            second_failure.first_fault().unwrap().operation,
            ClapOperation::CreatePlugin
        );
    }

    #[test]
    fn throwing_deinit_is_reported_once_and_pins_the_module() {
        let (registry, counts) = registry();
        let lease = registry
            .acquire_identity(identity("deinit-fault.clap"))
            .unwrap();
        let failure = lease.failure();
        counts.throw_deinit.store(true, Ordering::Relaxed);
        drop(lease);
        assert_eq!(
            failure.take_report().unwrap().operation,
            ClapOperation::EntryDeinit
        );
        assert!(failure.take_report().is_none());
        assert_eq!(counts.releases.load(Ordering::Relaxed), 0);
        assert!(
            registry
                .acquire_identity(identity("deinit-fault.clap"))
                .is_err()
        );
    }

    #[test]
    fn live_binary_replacement_is_rejected_without_changing_existing_lease() {
        let (registry, counts) = registry();
        let original = identity("replace.clap");
        let lease = registry.acquire_identity(original.clone()).unwrap();
        let mut replacement = original;
        replacement.fingerprint.length += 1;
        assert!(registry.acquire_identity(replacement.clone()).is_err());
        assert_eq!(*lease.module(), 1);
        drop(lease);
        drop(registry.acquire_identity(replacement).unwrap());
        assert_eq!(counts.loads.load(Ordering::Relaxed), 2);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn aborted_transitions_quarantine_instead_of_stranding_waiters() {
        let (registry, counts) = registry();
        counts.panic_load.store(true, Ordering::Relaxed);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || registry.acquire_identity(identity("load-panic.clap"))
            ))
            .is_err()
        );
        assert!(
            registry
                .acquire_identity(identity("load-panic.clap"))
                .is_err()
        );
        counts.panic_load.store(false, Ordering::Relaxed);
        let lease = registry
            .acquire_identity(identity("retire-panic.clap"))
            .unwrap();
        counts.panic_deinit.store(true, Ordering::Relaxed);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(lease))).is_err());
        assert!(
            registry
                .acquire_identity(identity("retire-panic.clap"))
                .is_err()
        );
        assert_eq!(counts.releases.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn canonical_slot_and_lease_admission_are_bounded() {
        let (registry, _) = registry();
        let mut leases = Vec::new();
        for _ in 0..MAX_MODULE_LEASES {
            leases.push(registry.acquire_identity(identity("bounded.clap")).unwrap());
        }
        assert!(registry.acquire_identity(identity("bounded.clap")).is_err());
        drop(leases);
        for index in 1..MAX_MODULE_SLOTS {
            drop(
                registry
                    .acquire_identity(identity(&format!("bounded-{index}.clap")))
                    .unwrap(),
            );
        }
        assert!(
            registry
                .acquire_identity(identity("too-many.clap"))
                .is_err()
        );
    }
    #[derive(Default)]
    struct RecursiveState {
        registry: Mutex<std::sync::Weak<RegistryInner<RecursiveDriver>>>,
        rejected: AtomicBool,
    }
    struct RecursiveDriver(Arc<RecursiveState>);
    impl ModuleDriver for RecursiveDriver {
        type Module = usize;
        fn load(&self, identity: &ModuleIdentity) -> Result<usize, ModuleLoadError<usize>> {
            let registry = ModuleRegistry {
                inner: self.0.registry.lock().unwrap().upgrade().unwrap(),
            };
            let error = registry.acquire_identity(identity.clone()).err().unwrap();
            self.0
                .rejected
                .store(error.contains("recursively"), Ordering::Relaxed);
            Ok(0)
        }
        fn deinit(&self, _: &usize) -> Result<(), Fault> {
            Ok(())
        }
        fn release(&self, _: usize) -> Result<(), ModuleReleaseError<usize>> {
            Ok(())
        }
    }
    #[test]
    fn recursive_module_initialization_rejects_instead_of_deadlocking() {
        let state = Arc::new(RecursiveState::default());
        let registry = ModuleRegistry::new(RecursiveDriver(Arc::clone(&state)));
        *state.registry.lock().unwrap() = Arc::downgrade(&registry.inner);
        let lease = registry
            .acquire_identity(identity("recursive.clap"))
            .unwrap();
        assert!(state.rejected.load(Ordering::Relaxed));
        drop(lease);
    }
}
