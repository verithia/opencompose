use super::*;
use crate::application_main::{MainService, MainThreadToken, UiRetirementToken, UiThreadToken};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;

struct NativeOwner {
    // Failed publication retains the whole Box, including its RouteOwner.
    // The fixed callback-route budget therefore remains consumed as well.
    native: ManuallyDrop<candidate::OwnedCandidate>,
    _not_send: PhantomData<Rc<()>>,
}
struct Entry {
    owner: NativeOwner,
    link: Arc<Link>,
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
}
impl Drop for Registry {
    fn drop(&mut self) {
        // Runtime/TLS teardown must not invoke plugin CLOSE after its GUI/COM
        // apartment has gone. Retain the complete payload, not only callbacks.
        for (_, entry) in self.entries.drain() {
            std::mem::forget(entry);
        }
    }
}
thread_local! { static REGISTRY: RefCell<Registry> = RefCell::new(Registry::new()); }
struct Service;
impl MainService for Service {
    fn service_main(&self, _: &MainThreadToken<'_>) {
        if let Ok(token) = application_main::ui_retirement_authority() {
            let _ = drain(&token);
        }
    }
}
static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
fn ensure_service(token: &UiThreadToken<'_>) {
    SERVICE.get_or_init(|| {
        let service = Arc::new(Service);
        let erased: Arc<dyn MainService> = service.clone();
        token.executor().register_service(Arc::downgrade(&erased));
        service
    });
}
pub(super) fn reserve(token: &UiThreadToken<'_>) -> Result<u64, Vst2OwnedError> {
    application_main::ui_authority()?;
    ensure_service(token);
    // Fail before module/native load when unpublished retained payloads have
    // consumed the same authoritative budget as visible registry entries.
    callback_route::check_capacity().map_err(|_| Vst2OwnedError::Capacity)?;
    REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst2OwnedError::Busy)?;
        if registry.entries.len() >= MAX_INSTANCES {
            return Err(Vst2OwnedError::Capacity);
        }
        let source = registry.next;
        registry.next = source.checked_add(1).ok_or(Vst2OwnedError::Capacity)?;
        Ok(source)
    })
}
fn prepare_publication(
    _token: &UiThreadToken<'_>,
    source: u64,
    native: candidate::OwnedCandidate,
) -> Result<Publication, Vst2OwnedError> {
    prepare_publication_with_runtime(_token, source, native, None)
}
fn prepare_publication_with_runtime(
    _token: &UiThreadToken<'_>,
    source: u64,
    native: candidate::OwnedCandidate,
    request: Option<super::runtime::Request<'_>>,
) -> Result<Publication, Vst2OwnedError> {
    application_main::ui_authority()?;
    let mut native = native;
    let snapshot = match super::preparation::capture_snapshot(&mut native) {
        Ok(snapshot) => Arc::new(snapshot),
        Err(error) => return Err(super::preparation::retire_error(native, error)),
    };
    let prepared = match request {
        Some(request) => {
            super::runtime::Authority::bind(&mut native, source, request.names, request.points)
                .map(|(authority, bindings)| (Some(authority), bindings))
        }
        None => Ok((None, Vec::new())),
    };
    let (runtime, bindings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let error = if native.host.failure.failed() {
                Vst2OwnedError::Faulted(native.host.failure.clone())
            } else {
                error
            };
            return Err(super::preparation::retire_error(native, error));
        }
    };
    if native
        .host
        .authored_inventory_revision
        .load(Ordering::Acquire)
        != snapshot.inventory_revision
    {
        return Err(super::preparation::retire_error(
            native,
            Vst2OwnedError::Runtime(Vst2RuntimeError::InventoryChanged),
        ));
    }
    let runtime_parameters: Arc<[Vst2RuntimeParameterBinding]> = bindings.into();
    let link = Arc::new(Link {
        runtime,
        _runtime_parameters: runtime_parameters.clone(),
        runtime_invalid: AtomicBool::new(false),
        native: NonNull::from(&mut *native),
        failure: native.host.failure.clone(),
        gate: AtomicU8::new(IDLE),
        retire: AtomicBool::new(false),
        snapshot: snapshot.clone(),
    });
    let entry = Entry {
        owner: NativeOwner {
            native: ManuallyDrop::new(native),
            _not_send: PhantomData,
        },
        link: link.clone(),
    };
    Ok(Publication {
        source,
        entry,
        instance: Vst2OwnedInstance {
            runtime_parameters,
            processor: Vst2ProcessorLease {
                link: link.clone(),
                _not_sync: PhantomData,
            },
            control: Vst2ControlHandle { link },
            snapshot,
        },
    })
}
struct Publication {
    source: u64,
    entry: Entry,
    instance: Vst2OwnedInstance,
}
fn publish(publication: Publication) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
    let Publication {
        source,
        entry,
        instance,
    } = publication;
    REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst2OwnedError::Busy)?;
        if registry.entries.len() >= MAX_INSTANCES {
            return Err(Vst2OwnedError::Capacity);
        }
        registry.entries.insert(source, entry);
        Ok::<_, Vst2OwnedError>(())
    })?;
    Ok(instance)
}
pub(super) fn adopt(
    token: &UiThreadToken<'_>,
    source: u64,
    native: candidate::OwnedCandidate,
) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
    publish(prepare_publication(token, source, native)?)
}

pub(super) fn adopt_runtime(
    token: &UiThreadToken<'_>,
    source: u64,
    native: candidate::OwnedCandidate,
    names: &[String],
    points: usize,
) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
    publish(prepare_publication_with_runtime(
        token,
        source,
        native,
        Some(super::runtime::Request { names, points }),
    )?)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn refuse_publication_for_audit(
    token: &UiThreadToken<'_>,
    source: u64,
    native: candidate::OwnedCandidate,
) -> Result<(), Vst2OwnedError> {
    let publication = prepare_publication(token, source, native)?;
    // Hold a registry borrow only over the pure publication boundary. All
    // native metadata preparation has completed; no foreign calls run here.
    REGISTRY.with(|registry| {
        let _borrow = registry
            .try_borrow_mut()
            .map_err(|_| Vst2OwnedError::Busy)?;
        match publish(publication) {
            Err(Vst2OwnedError::Busy) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(Vst2OwnedError::Control(
                "Publication unexpectedly succeeded".into(),
            )),
        }
    })
}

pub(super) fn drain(_token: &UiRetirementToken<'_>) -> Result<usize, Vst2OwnedError> {
    application_main::ui_retirement_authority()?;
    let _phase = Phase::enter()?;
    let entries = REGISTRY.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| Vst2OwnedError::Busy)?;
        let ids: Vec<_> = registry
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.link.retire.load(Ordering::Acquire)
                    && Arc::strong_count(&entry.link) == 1
                    && entry.link.gate.load(Ordering::Acquire) == IDLE
            })
            .map(|(id, _)| *id)
            .collect();
        Ok::<_, Vst2OwnedError>(
            ids.into_iter()
                .filter_map(|id| registry.entries.remove(&id).map(|entry| (id, entry)))
                .collect::<Vec<_>>(),
        )
    })?;
    let count = entries.len();
    // No registry borrow or mutex is held across native teardown. The phase
    // guard refuses nested owned entry until CLOSE AND route completion finish.
    for (id, mut entry) in entries {
        entry.link.gate.store(RETIRED, Ordering::Release);
        if entry.owner.native.retire(_token).is_ok() {
            drop(ManuallyDrop::into_inner(entry.owner.native));
        } else {
            entry.owner.native.quarantine();
            REGISTRY.with(|registry| {
                if let Ok(mut registry) = registry.try_borrow_mut() {
                    registry.entries.insert(id, entry);
                } else {
                    std::mem::forget(entry);
                }
            });
        }
    }
    Ok(count)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn next_source_for_audit() -> u64 {
    REGISTRY.with(|registry| registry.borrow().next)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn entries_for_audit() -> usize {
    REGISTRY.with(|registry| registry.borrow().entries.len())
}
