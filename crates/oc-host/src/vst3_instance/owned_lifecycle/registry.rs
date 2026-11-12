use super::*;
use crate::application_main::{MainService, MainThreadToken};
use std::sync::OnceLock;

static SLOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(super) struct Slot {
    pub(super) failure: Arc<Vst3Failure>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        // Faulted native allocations remain quarantined and keep their budget.
        if !self.failure.failed() {
            SLOTS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
#[allow(
    deprecated,
    reason = "fetch_update supports the minimum Rust version, which predates try_update"
)]
pub(super) fn reserve(
    _token: &UiThreadToken<'_>,
    failure: Arc<Vst3Failure>,
) -> Result<Slot, Vst3OwnedError> {
    application_main::ui_authority()?;
    SLOTS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < MAX_INSTANCES).then_some(count + 1)
        })
        .map_err(|_| Vst3OwnedError::Capacity)?;
    Ok(Slot { failure })
}
struct UiNativeOwner {
    _admission: Box<processing::PreparedAdmission>,
    native: ManuallyDrop<Box<Vst3Instance>>,
    _slot: Slot,
    _not_send: PhantomData<Rc<()>>,
}
struct Entry {
    owner: UiNativeOwner,
    link: Arc<ProcessLink>,
}
struct Registry {
    entries: HashMap<u64, Entry>,
    next: u64,
}
impl Registry {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next: 1,
        }
    }
    fn check_capacity(&self) -> Result<(), Vst3OwnedError> {
        if SLOTS.load(Ordering::Acquire) >= MAX_INSTANCES {
            Err(Vst3OwnedError::Capacity)
        } else {
            Ok(())
        }
    }
}
// Native ownership is explicit: TLS destruction must not invoke third-party
// lifecycle code after the application runtime/apartment has already closed.
impl Drop for Registry {
    fn drop(&mut self) {
        for (_, entry) in self.entries.drain() {
            std::mem::forget(entry);
        }
    }
}
thread_local! {static REGISTRY:RefCell<Registry>=RefCell::new(Registry::new());}
struct RegistryService;
impl MainService for RegistryService {
    fn service_main(&self, _: &MainThreadToken<'_>) {
        if let Ok(token) = application_main::ui_retirement_authority() {
            let _ = drain(&token);
        }
    }
}
static SERVICE: OnceLock<Arc<RegistryService>> = OnceLock::new();
fn ensure_service(token: &UiThreadToken<'_>) {
    SERVICE.get_or_init(|| {
        let service = Arc::new(RegistryService);
        let erased: Arc<dyn MainService> = service.clone();
        token.executor().register_service(Arc::downgrade(&erased));
        service
    });
}
pub(super) fn check_capacity(_token: &UiThreadToken<'_>) -> Result<(), Vst3OwnedError> {
    application_main::ui_authority()?;
    REGISTRY.with(|registry| {
        registry
            .try_borrow()
            .map_err(|_| Vst3OwnedError::Busy)?
            .check_capacity()
    })
}
pub(super) fn validate_initial_budget(
    budget: Vst3ProcessingBudget,
    initial: &[Vst3NormalizedParameter],
) -> Result<(), Vst3OwnedError> {
    budget.validate()?;
    if initial.len() > budget.events {
        return Err(Vst3OwnedError::InvalidRequest(
            "Initial VST3 parameter budget exceeded",
        ));
    }
    let mut counts = HashMap::with_capacity(initial.len());
    for value in initial {
        let count = counts.entry(value.id).or_insert(0usize);
        *count += 1;
        if *count > budget.points_per_parameter {
            return Err(Vst3OwnedError::InvalidRequest(
                "Initial VST3 parameter point budget exceeded",
            ));
        }
    }
    Ok(())
}

// Field order ensures teardown records quarantine before the reservation decides
// whether its capacity can be released, including admission failures.
pub(super) struct Candidate {
    pub(super) native: Box<Vst3Instance>,
    pub(super) slot: Slot,
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn reserved_slots_for_audit() -> usize {
    SLOTS.load(Ordering::Acquire)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn adopt(
    token: &UiThreadToken<'_>,
    instance: Vst3Instance,
    budget: Vst3ProcessingBudget,
    initial: &[Vst3NormalizedParameter],
    slot: Slot,
) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
    let mut candidate = Candidate {
        native: Box::new(instance),
        slot,
    };
    application_main::ui_authority()?;
    validate_initial_budget(budget, initial)?;
    let before = metadata::capture(token, &candidate.native, 0)?;
    metadata::preflight(&before, initial)?;
    let snapshot = metadata::apply_initial(token, &mut candidate.native, before, initial)?;
    adopt_prepared(token, candidate, snapshot, budget)
}
pub(super) fn adopt_prepared(
    token: &UiThreadToken<'_>,
    mut candidate: Candidate,
    mut snapshot: Vst3OwnedSnapshot,
    budget: Vst3ProcessingBudget,
) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
    application_main::ui_authority()?;
    budget.validate()?;
    // A plugin with thousands of parameters cannot reserve the full
    // per-parameter point budget for every one of them; fit the reservation to
    // the inventory so the instance can be prepared at all.
    let budget = budget.fitted_to(snapshot.parameters.len());
    ensure_service(token);
    REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst3OwnedError::Busy)?;
        let source = registry.next;
        registry.next = registry
            .next
            .checked_add(1)
            .ok_or(Vst3OwnedError::Capacity)?;
        snapshot.source = source;
        let snapshot = Arc::new(snapshot);
        let native = &mut candidate.native;
        let midi = midi::capture(token, native, &snapshot)?;
        metadata::prepare_processing(native, &snapshot, budget)?;
        let mut admission = Box::new(processing::PreparedAdmission::with_budget(
            &snapshot, budget,
        ));
        let admission_pointer = NonNull::from(admission.as_mut());
        let failure = native.failure();
        let link = Arc::new(ProcessLink {
            source,
            instance: NonNull::from(native.as_mut()),
            gate: AtomicU8::new(IDLE),
            retire: AtomicBool::new(false),
            failure,
            snapshot: Arc::clone(&snapshot),
            midi,
            budget,
        });
        registry.entries.insert(
            source,
            Entry {
                owner: UiNativeOwner {
                    _admission: admission,
                    native: ManuallyDrop::new(candidate.native),
                    _slot: candidate.slot,
                    _not_send: PhantomData,
                },
                link: Arc::clone(&link),
            },
        );
        Ok(Vst3OwnedInstance {
            processor: Vst3ProcessorLease::new(Arc::clone(&link), admission_pointer),
            control: Vst3ControlHandle { link },
            snapshot,
        })
    })
}
pub(super) fn with_control<T>(
    link: &Arc<ProcessLink>,
    work: impl FnOnce(&UiThreadToken<'_>, &mut Vst3Instance) -> Result<T, Vst3OwnedError>,
) -> Result<T, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let _permit = link.acquire(CONTROL)?;
    REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst3OwnedError::Busy)?;
        let entry = registry
            .entries
            .get_mut(&link.source)
            .ok_or(Vst3OwnedError::Retired)?;
        if !Arc::ptr_eq(&entry.link, link) {
            return Err(Vst3OwnedError::Retired);
        }
        entry
            .owner
            .native
            .healthy()
            .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
        work(&token, &mut entry.owner.native)
    })
}
pub(super) fn drain(_token: &UiRetirementToken<'_>) -> Result<usize, Vst3OwnedError> {
    application_main::ui_retirement_authority()?;
    REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst3OwnedError::Busy)?;
        // No Weak<ProcessLink> escapes. A count of one therefore proves that
        // the worker and all queued control leases have released their tickets.
        let retired: Vec<_> = registry
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.link.retire.load(Ordering::Acquire)
                    && Arc::strong_count(&entry.link) == 1
                    && entry.link.gate.load(Ordering::Acquire) == IDLE
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &retired {
            let entry = registry.entries.remove(id).expect("retained owned entry");
            entry.link.gate.store(RETIRED, Ordering::Release);
            if !entry.link.failure.failed() {
                // The retirement token, idle gate and last-lease proof retain
                // all interfaces through native shutdown on UI.
                let native = ManuallyDrop::into_inner(entry.owner.native);
                drop(native);
            }
        }
        Ok(retired.len())
    })
}
