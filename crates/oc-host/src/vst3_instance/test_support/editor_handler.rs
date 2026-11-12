//! Thread-affine COM loans for the in-process editor fixture only.
use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{Weak, atomic::AtomicU64},
    thread::ThreadId,
};

const MAX_RETAINED: usize = 64;
static NEXT: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static RETAINED: Cell<usize> = const { Cell::new(0) };
    static OWNERS: RefCell<HashMap<u64, Rc<Owners>>> = RefCell::new(HashMap::new());
}
struct Budget;
impl Budget {
    fn reserve() -> Option<Self> {
        RETAINED
            .try_with(|count| {
                if count.get() >= MAX_RETAINED {
                    return None;
                }
                count.set(count.get() + 1);
                Some(Self)
            })
            .ok()
            .flatten()
    }
}
impl Drop for Budget {
    fn drop(&mut self) {
        let _ = RETAINED.try_with(|count| count.set(count.get().saturating_sub(1)));
    }
}
pub(super) struct Registration {
    id: u64,
    owner: ThreadId,
    active: AtomicBool,
    control: Weak<FakeControl>,
}
struct Owners {
    _controller: ComPtr<IEditController>,
    handler: ComPtr<IComponentHandler>,
    registration: Weak<Registration>,
    // Last-field drop keeps the budget charged through every COM release.
    _budget: Budget,
}
struct ScopeState {
    active: AtomicBool,
    registrations: Mutex<Vec<Weak<Registration>>>,
}
pub(crate) struct Scope {
    state: Arc<ScopeState>,
}
impl Scope {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(ScopeState {
                active: AtomicBool::new(true),
                registrations: Mutex::new(Vec::new()),
            }),
        }
    }
    pub(crate) fn deactivate(&mut self) {
        if !self.state.active.swap(false, Ordering::AcqRel) {
            return;
        }
        let registrations = std::mem::take(
            &mut *self
                .state
                .registrations
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for registration in registrations
            .into_iter()
            .filter_map(|state| state.upgrade())
        {
            deactivate(&registration);
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.deactivate();
    }
}

pub(super) struct Slot {
    scope: Weak<ScopeState>,
    current: Mutex<Option<Arc<Registration>>>,
    controller: AtomicUsize,
}
impl Slot {
    pub(super) fn new(scope: &Scope) -> Self {
        Self {
            scope: Arc::downgrade(&scope.state),
            current: Mutex::new(None),
            controller: AtomicUsize::new(0),
        }
    }
    pub(super) fn attach_controller(&self, controller: *mut IEditController) {
        self.controller
            .store(controller as usize, Ordering::Release);
    }
    pub(super) fn clear(&self) {
        let previous = self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(previous) = previous {
            deactivate(&previous);
        }
    }
    pub(super) unsafe fn set(
        &self,
        control: &Arc<FakeControl>,
        handler: *mut IComponentHandler,
    ) -> tresult {
        if handler.is_null() {
            self.clear();
            return kResultOk;
        }
        let Some(scope) = self
            .scope
            .upgrade()
            .filter(|scope| scope.active.load(Ordering::Acquire))
        else {
            return kResultFalse;
        };
        let controller = self.controller.load(Ordering::Acquire) as *mut IEditController;
        if controller.is_null() {
            return kResultFalse;
        }
        drain();
        // Charge retention before AddRef can invoke a reentrant implementation.
        let Some(budget) = Budget::reserve() else {
            return kResultFalse;
        };
        let Ok(id) =
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        else {
            return kResultFalse;
        };
        let registration = Arc::new(Registration {
            id,
            owner: std::thread::current().id(),
            active: AtomicBool::new(true),
            control: Arc::downgrade(control),
        });
        {
            let mut registrations = scope
                .registrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            registrations.retain(|state| {
                state
                    .upgrade()
                    .is_some_and(|state| state.active.load(Ordering::Acquire))
            });
            if registrations.len() >= MAX_RETAINED || registrations.try_reserve(1).is_err() {
                return kResultFalse;
            }
            registrations.push(Arc::downgrade(&registration));
        }
        let handler = {
            // SAFETY: The unsafe COM caller loans a live handler for this call.
            // Retain it outside every fixture lock/registry borrow before returning.
            unsafe { ComRef::<IComponentHandler>::from_raw_unchecked(handler) }.to_com_ptr()
        };
        let controller = {
            // SAFETY: This is the same fake controller executing the COM method;
            // its caller retains that interface until this synchronous call returns.
            unsafe { ComRef::<IEditController>::from_raw_unchecked(controller) }.to_com_ptr()
        };
        let owners = Rc::new(Owners {
            _controller: controller,
            handler,
            registration: Arc::downgrade(&registration),
            _budget: budget,
        });
        let inserted = OWNERS
            .try_with(|entries| {
                let mut entries = entries.borrow_mut();
                if entries.try_reserve(1).is_err() {
                    false
                } else {
                    entries.insert(id, owners.clone());
                    true
                }
            })
            .unwrap_or(false);
        if !inserted {
            registration.active.store(false, Ordering::Release);
            return kResultFalse;
        }
        // Source retirement can race this registration. Its tracked weak entry
        // or this recheck deactivates the owner before it can service callbacks.
        if !scope.active.load(Ordering::Acquire) {
            deactivate(&registration);
            return kResultFalse;
        }
        let previous = self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace(registration.clone());
        *control
            .editor_handler
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(registration);
        control
            .last_handler
            .store(owners.handler.as_ptr() as usize, Ordering::Release);
        if let Some(previous) = previous {
            deactivate(&previous);
        }
        kResultOk
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.clear();
    }
}
fn deactivate(registration: &Registration) {
    registration.active.store(false, Ordering::Release);
    if let Some(control) = registration.control.upgrade() {
        let mut current = control
            .editor_handler
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current
            .as_ref()
            .is_some_and(|current| current.id == registration.id)
        {
            control.last_handler.store(0, Ordering::Release);
            current.take();
        }
    }
    if registration.owner == std::thread::current().id() {
        let retired = OWNERS
            .try_with(|entries| entries.borrow_mut().remove(&registration.id))
            .ok()
            .flatten();
        drop(retired);
    }
}
/// Retire inert owners only on the thread that accepted their COM references.
/// Off-thread source retirement leaves at most MAX_RETAINED inert entries until
/// this drain or TLS shutdown; no worker performs a foreign Release.
pub(super) fn drain() {
    let retired = OWNERS
        .try_with(|entries| {
            let mut entries = entries.borrow_mut();
            let retired: Vec<_> = entries
                .iter()
                .filter_map(|(&id, owners)| {
                    (!owners
                        .registration
                        .upgrade()
                        .is_some_and(|state| state.active.load(Ordering::Acquire)))
                    .then_some(id)
                })
                .collect();
            retired
                .into_iter()
                .filter_map(|id| entries.remove(&id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    drop(retired);
}
pub(super) struct Loan {
    owners: Rc<Owners>,
}
impl Loan {
    pub(super) fn handler(&self) -> ComRef<'_, IComponentHandler> {
        self.owners.handler.as_com_ref()
    }
}
pub(super) fn loan(control: &FakeControl) -> Option<Loan> {
    drain();
    let registration = control
        .editor_handler
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()?;
    if registration.owner != std::thread::current().id()
        || !registration.active.load(Ordering::Acquire)
    {
        return None;
    }
    // Rc cloning does not invoke COM code. Neither borrow is held across the
    // callback, and the loan pins both controller and handler across reentry.
    let owners = OWNERS
        .try_with(|entries| entries.borrow().get(&registration.id).cloned())
        .ok()
        .flatten()?;
    registration
        .active
        .load(Ordering::Acquire)
        .then_some(Loan { owners })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::Steinberg::Vst::IComponentHandlerTrait;
    struct ReentrantHandler {
        action: RefCell<Option<Box<dyn FnOnce()>>>,
        drops: Arc<AtomicUsize>,
    }
    impl Class for ReentrantHandler {
        type Interfaces = (IComponentHandler,);
    }
    impl Drop for ReentrantHandler {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl IComponentHandlerTrait for ReentrantHandler {
        unsafe fn beginEdit(&self, _: ParamID) -> tresult {
            kResultOk
        }
        unsafe fn performEdit(&self, _: ParamID, _: f64) -> tresult {
            let action = self.action.borrow_mut().take();
            if let Some(action) = action {
                action();
            }
            assert_eq!(self.drops.load(Ordering::Relaxed), 0);
            kResultOk
        }
        unsafe fn endEdit(&self, _: ParamID) -> tresult {
            kResultOk
        }
        unsafe fn restartComponent(&self, _: i32) -> tresult {
            kResultOk
        }
    }
    fn controller(control: &Arc<FakeControl>, scope: &Scope) -> ComPtr<IEditController> {
        let object = ComWrapper::new(FakeEditController {
            control: control.clone(),
            handler: Slot::new(scope),
        });
        let controller = object.to_com_ptr::<IEditController>().unwrap();
        object.handler.attach_controller(controller.as_ptr());
        controller
    }
    #[test]
    fn fixture_callback_loan_keeps_the_old_handler_and_controller_through_replacement() {
        drain();
        let baseline = RETAINED.with(Cell::get);
        let mut scope = Scope::new();
        let control = Arc::new(FakeControl::default());
        let controller = controller(&control, &scope);
        let replacement = Vst3Host::new().to_com_ptr::<IComponentHandler>().unwrap();
        let during = controller.clone();
        let drops = Arc::new(AtomicUsize::new(0));
        let handler = ComWrapper::new(ReentrantHandler {
            action: RefCell::new(Some(Box::new(move || {
                // SAFETY: The closure owns both interfaces; replacement loans its
                // handler reference only during this synchronous fixture call.
                let result = unsafe { during.setComponentHandler(replacement.as_ptr()) };
                assert_eq!(result, kResultOk);
            }))),
            drops: drops.clone(),
        })
        .to_com_ptr::<IComponentHandler>()
        .unwrap();
        // SAFETY: The controller and handler references are retained on this
        // registration thread throughout the synchronous COM fixture call.
        let result = unsafe { controller.setComponentHandler(handler.as_ptr()) };
        assert_eq!(result, kResultOk);
        drop(handler);
        assert_eq!(
            super::super::editor_perform_edit(&control, FAKE_PARAM_ID, 0.75),
            kResultOk
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            super::super::editor_begin_edit(&control, FAKE_PARAM_ID),
            kResultOk
        );
        scope.deactivate();
        drop(controller);
        drain();
        assert_eq!(RETAINED.with(Cell::get), baseline);
        assert_eq!(
            super::super::editor_begin_edit(&control, FAKE_PARAM_ID),
            kResultFalse
        );
    }
    #[test]
    fn fixture_controller_clear_and_drop_leave_safe_callbacks_inactive() {
        drain();
        let baseline = RETAINED.with(Cell::get);
        let scope = Scope::new();
        let control = Arc::new(FakeControl::default());
        let controller = controller(&control, &scope);
        let handler = Vst3Host::new().to_com_ptr::<IComponentHandler>().unwrap();
        // SAFETY: Both interfaces remain owned on this registration thread.
        let registered = unsafe { controller.setComponentHandler(handler.as_ptr()) };
        assert_eq!(registered, kResultOk);
        drop(handler);
        // SAFETY: The live controller explicitly accepts a null handler to clear
        // its registration; no external pointer is borrowed by this request.
        let cleared = unsafe { controller.setComponentHandler(std::ptr::null_mut()) };
        assert_eq!(cleared, kResultOk);
        drop(controller);
        assert_eq!(control.last_handler.load(Ordering::Acquire), 0);
        assert_eq!(
            super::super::editor_begin_edit(&control, FAKE_PARAM_ID),
            kResultFalse
        );
        drain();
        assert_eq!(RETAINED.with(Cell::get), baseline);
    }
    #[test]
    fn fixture_constructor_unwind_deactivates_registered_owners_without_terminate() {
        drain();
        let baseline = RETAINED.with(Cell::get);
        let control = Arc::new(FakeControl::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let scope = Scope::new();
            let controller = controller(&control, &scope);
            let handler = Vst3Host::new().to_com_ptr::<IComponentHandler>().unwrap();
            // SAFETY: Both owned fixture interfaces live on the current thread
            // and remain retained throughout this synchronous registration.
            let result = unsafe { controller.setComponentHandler(handler.as_ptr()) };
            assert_eq!(result, kResultOk);
            panic!("fixture construction interrupted before instance publication");
        }));
        assert!(result.is_err());
        assert_eq!(
            super::super::editor_begin_edit(&control, FAKE_PARAM_ID),
            kResultFalse
        );
        drain();
        assert_eq!(RETAINED.with(Cell::get), baseline);
    }
    #[test]
    fn fixture_handler_retention_has_a_finite_per_thread_budget_and_drains() {
        drain();
        let baseline = RETAINED.with(Cell::get);
        let mut scopes = Vec::new();
        let mut controllers = Vec::new();
        let handler = Vst3Host::new().to_com_ptr::<IComponentHandler>().unwrap();
        for index in 0..=MAX_RETAINED - baseline {
            let scope = Scope::new();
            let control = Arc::new(FakeControl::default());
            let controller = controller(&control, &scope);
            // SAFETY: Both owned fixture interfaces live on the registration
            // thread; the bounded slot either retains its own loan or refuses.
            let result = unsafe { controller.setComponentHandler(handler.as_ptr()) };
            assert_eq!(
                result,
                if index < MAX_RETAINED - baseline {
                    kResultOk
                } else {
                    kResultFalse
                }
            );
            scopes.push(scope);
            controllers.push(controller);
        }
        assert_eq!(RETAINED.with(Cell::get), MAX_RETAINED);
        drop(scopes);
        drop(controllers);
        drain();
        assert_eq!(RETAINED.with(Cell::get), baseline);
    }
}
