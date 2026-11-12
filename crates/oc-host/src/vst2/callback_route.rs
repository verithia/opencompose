//! Stable callback backing, independent of a native descriptor's lifetime.
//!
//! Slots and callback work are bounded. Only non-callback owners reserve,
//! collect or retire slots. A callback's final reader never releases an Arc.
//! Faulted entries and entries that fail to return normally retain their slot
//! and thunk permanently. Healthy retirement permits reuse only under the VST2
//! lifetime contract: no callback after completed CLOSE (or normal null entry).
//! The finite thunk ABI carries no generation cookie, so a protocol-violating
//! callback after healthy slot reuse cannot be distinguished from its new owner.
//! Keeping module code mapped does not authorize those callbacks.
use super::HostContext;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

include!(concat!(env!("OUT_DIR"), "/vst2_callback_capacity.rs"));
const READER_CAPACITY: usize = 64;
const FREE: u64 = 0;
const RESERVED: u64 = 1;
const OPEN: u64 = 2;
const RETIRED: u64 = 3;
const QUARANTINED: u64 = 4;
const KIND_MASK: u64 = 7;
static ROUTES: Registry<CAPACITY> = Registry::new();

struct Slot {
    address: AtomicUsize,
    state: AtomicU64,
    readers: [AtomicBool; READER_CAPACITY],
    overflow_generation: AtomicU64,
    context: AtomicPtr<HostContext>,
}
impl Slot {
    const fn new() -> Self {
        Self {
            address: AtomicUsize::new(0),
            state: AtomicU64::new(FREE),
            readers: [const { AtomicBool::new(false) }; READER_CAPACITY],
            overflow_generation: AtomicU64::new(0),
            context: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}
struct Registry<const N: usize> {
    control: Mutex<()>,
    next_generation: AtomicU64,
    slots: [Slot; N],
}
#[derive(Clone, Copy)]
struct Handle {
    index: usize,
    generation: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteError {
    Capacity,
    AddressInUse,
    InvalidTransition,
}
impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "VST2 callback route capacity exhausted",
            Self::AddressInUse => "VST2 callback address is already owned or quarantined",
            Self::InvalidTransition => "VST2 callback route transition refused",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteFault {
    AdmissionOverflow,
}

pub(super) enum Lookup<'a> {
    #[cfg(test)]
    Missing,
    Unavailable,
    Ready(Reader<'a>),
}
pub(super) struct Reader<'a> {
    permit: &'a AtomicBool,
    context: *const HostContext,
}
impl Deref for Reader<'_> {
    type Target = HostContext;
    fn deref(&self) -> &HostContext {
        // SAFETY: Admission acquires a reader permit before checking the exact open
        // generation. Retirement cannot release backing until this permit ends.
        unsafe { &*self.context }
    }
}
impl Drop for Reader<'_> {
    fn drop(&mut self) {
        // No ownership release, allocation, collection or native work here.
        self.permit.store(false, Ordering::SeqCst);
    }
}
impl<const N: usize> Registry<N> {
    const fn new() -> Self {
        Self {
            control: Mutex::new(()),
            next_generation: AtomicU64::new(1),
            slots: [const { Slot::new() }; N],
        }
    }
    fn control(&self) -> MutexGuard<'_, ()> {
        // The mutex protects only atomic transitions; no user/native code runs
        // under it. An interrupted transition may retain a slot, but cannot
        // expose uninitialized backing or transfer an Arc twice.
        self.control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn reserve(&self, context: &Arc<HostContext>) -> Result<Handle, RouteError> {
        let _control = self.control();
        self.collect_unlocked();
        let generation = self.next_generation.load(Ordering::SeqCst);
        if generation > (u64::MAX >> 3) {
            return Err(RouteError::Capacity);
        }
        let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.state.load(Ordering::SeqCst) == FREE)
        else {
            return Err(RouteError::Capacity);
        };
        self.next_generation.store(generation + 1, Ordering::SeqCst);
        let slot = &self.slots[index];
        slot.context.store(
            Arc::into_raw(Arc::clone(context)).cast_mut(),
            Ordering::SeqCst,
        );
        slot.state
            .store((generation << 3) | RESERVED, Ordering::SeqCst);
        Ok(Handle { index, generation })
    }
    fn bind(&self, handle: Handle, address: usize) -> Result<(), RouteError> {
        let _control = self.control();
        // Another owner can finish CLOSE while this reservation is inside
        // native entry. Collect again before judging a reused allocation.
        self.collect_unlocked();
        let slot = &self.slots[handle.index];
        if address == 0 || slot.state.load(Ordering::SeqCst) != (handle.generation << 3) | RESERVED
        {
            return Err(RouteError::InvalidTransition);
        }
        if self
            .slots
            .iter()
            .any(|other| other.address.load(Ordering::SeqCst) == address)
        {
            return Err(RouteError::AddressInUse);
        }
        slot.address.store(address, Ordering::SeqCst);
        slot.state
            .store((handle.generation << 3) | OPEN, Ordering::SeqCst);
        Ok(())
    }
    fn retire(&self, handle: Handle, quarantine: bool) {
        let _control = self.control();
        let slot = &self.slots[handle.index];
        let state = slot.state.load(Ordering::SeqCst);
        if state >> 3 != handle.generation || !matches!(state & KIND_MASK, RESERVED | OPEN) {
            return;
        }
        slot.state.store(
            (handle.generation << 3) | if quarantine { QUARANTINED } else { RETIRED },
            Ordering::SeqCst,
        );
    }
    fn collect_unlocked(&self) {
        for slot in &self.slots {
            if slot.state.load(Ordering::SeqCst) & KIND_MASK != RETIRED
                || slot
                    .readers
                    .iter()
                    .any(|permit| permit.load(Ordering::SeqCst))
            {
                continue;
            }
            let context = slot.context.swap(std::ptr::null_mut(), Ordering::SeqCst);
            slot.address.store(0, Ordering::SeqCst);
            slot.state.store(FREE, Ordering::SeqCst);
            if !context.is_null() {
                // SAFETY: reserve transferred exactly one Arc into this slot.
                // Admission is closed and no admitted reader remains; only this
                // serialized non-callback collector consumes that ownership.
                drop(unsafe { Arc::from_raw(context) });
            }
        }
    }
    fn fault(&self, handle: Handle) -> Option<RouteFault> {
        (self.slots[handle.index]
            .overflow_generation
            .load(Ordering::SeqCst)
            == handle.generation)
            .then_some(RouteFault::AdmissionOverflow)
    }
    fn borrow_slot(slot: &Slot, state: u64) -> Lookup<'_> {
        // SC admission/close ordering ensures either the borrower observes
        // retirement or the collector observes its permit. Reuse never resets
        // flags that may belong to an in-flight failed admission.
        for permit in &slot.readers {
            if permit
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                continue;
            }
            if slot.state.load(Ordering::SeqCst) != state {
                permit.store(false, Ordering::SeqCst);
                return Lookup::Unavailable;
            }
            let context = slot.context.load(Ordering::SeqCst);
            return Lookup::Ready(Reader { permit, context });
        }
        slot.overflow_generation
            .fetch_max(state >> 3, Ordering::SeqCst);
        Lookup::Unavailable
    }
    fn lookup_slot(&self, index: usize) -> Lookup<'_> {
        let Some(slot) = self.slots.get(index) else {
            return Lookup::Unavailable;
        };
        let state = slot.state.load(Ordering::SeqCst);
        if !matches!(state & KIND_MASK, RESERVED | OPEN) {
            return Lookup::Unavailable;
        }
        Self::borrow_slot(slot, state)
    }
    #[cfg(test)]
    fn lookup(&self, address: usize) -> Lookup<'_> {
        if address == 0 {
            return Lookup::Missing;
        }
        for slot in &self.slots {
            if slot.address.load(Ordering::SeqCst) != address {
                continue;
            }
            let state = slot.state.load(Ordering::SeqCst);
            if state & KIND_MASK != OPEN {
                return Lookup::Unavailable;
            }
            return Self::borrow_slot(slot, state);
        }
        Lookup::Missing
    }
}
impl<const N: usize> Drop for Registry<N> {
    fn drop(&mut self) {
        // The product registry is static and never reaches here. Local fixture
        // registries can be destroyed only after all borrowed Readers finish.
        for slot in &mut self.slots {
            let context = *slot.context.get_mut();
            if !context.is_null() {
                // SAFETY: Exclusive registry ownership excludes its borrowed
                // Readers, and each non-null slot owns exactly one Arc.
                drop(unsafe { Arc::from_raw(context) });
            }
        }
    }
}

/// An abandoned owner defaults to quarantine, never optimistic reclamation.
pub(super) struct RouteOwner {
    handle: Option<Handle>,
}
impl RouteOwner {
    pub(super) fn reserve(context: &Arc<HostContext>) -> Result<Self, RouteError> {
        ROUTES.reserve(context).map(|handle| Self {
            handle: Some(handle),
        })
    }
    pub(super) fn callback(&self) -> Result<super::abi::AudioMasterCallback, RouteError> {
        let handle = self.handle.ok_or(RouteError::InvalidTransition)?;
        super::callback_thunks::get(handle.index).ok_or(RouteError::InvalidTransition)
    }
    pub(super) fn bind(&mut self, address: usize) -> Result<(), RouteError> {
        ROUTES.bind(self.handle.ok_or(RouteError::InvalidTransition)?, address)
    }
    pub(super) fn fault(&self) -> Option<RouteFault> {
        self.handle.and_then(|handle| ROUTES.fault(handle))
    }
    pub(super) fn quarantine(&mut self) {
        if let Some(handle) = self.handle.take() {
            ROUTES.retire(handle, true);
        }
    }
    /// Only call after a normal, complete native CLOSE (or a normal null entry).
    /// The caller relies on the native lifetime contract forbidding subsequent
    /// callbacks; this permits thunk reuse and provides no post-CLOSE identity
    /// isolation against a plugin that violates that contract. Faulted or
    /// nonreturning entry/CLOSE paths must quarantine instead.
    pub(super) fn complete(&mut self) {
        if let Some(handle) = self.handle.take() {
            ROUTES.retire(handle, false);
        }
    }
}
impl Drop for RouteOwner {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            ROUTES.retire(handle, true);
        }
    }
}
#[cfg(test)]
pub(super) fn lookup(address: usize) -> Lookup<'static> {
    ROUTES.lookup(address)
}

#[cfg(test)]
#[path = "callback_route_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub(super) fn occupied_budget_for_audit() -> (usize, usize) {
    let _control = ROUTES.control();
    ROUTES.collect_unlocked();
    (
        ROUTES
            .slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::SeqCst) != FREE)
            .count(),
        CAPACITY,
    )
}

pub(super) fn check_capacity() -> Result<(), RouteError> {
    let _control = ROUTES.control();
    ROUTES.collect_unlocked();
    if ROUTES
        .slots
        .iter()
        .any(|slot| slot.state.load(Ordering::SeqCst) == FREE)
    {
        Ok(())
    } else {
        Err(RouteError::Capacity)
    }
}

pub(super) fn lookup_slot(index: usize) -> Lookup<'static> {
    ROUTES.lookup_slot(index)
}
