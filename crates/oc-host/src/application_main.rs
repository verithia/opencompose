//! Shared application-main execution for plugin control and native ownership.
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, ThreadId};

/// Work item dispatched to the application main thread.
pub type ApplicationMainTask = Box<dyn FnOnce() + Send + 'static>;
type Dispatch = dyn Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync;
const MAX_PENDING_TASKS: usize = 4096;
const CONTROL_FAILURE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// A runtime adapter must dispatch onto the program's GUI/event-loop thread.
/// Registration happens on that thread, before any plugin instance is created.
#[derive(Clone)]
pub struct ApplicationMainExecutor {
    inner: Arc<ExecutorInner>,
}
struct ExecutorInner {
    thread: ThreadId,
    runtime: Option<MainRuntime>,
    dispatch: Box<Dispatch>,
    failed: Arc<AtomicBool>,
    control_open: Arc<AtomicBool>,
    polling: AtomicBool,
    pending: Mutex<Vec<Weak<PendingTask>>>,
    deferred_retirements: Mutex<Vec<Arc<PendingTask>>>,
    services: Mutex<Vec<Weak<dyn MainService>>>,
}

/// The audited executable/runtime bootstrap that owns the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainRuntime {
    Gui,
    CommandLine,
    EditorHelper,
    #[cfg(any(test, feature = "test-support"))]
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a UI authority request was refused.
pub enum UiAuthorityError {
    NotRegistered,
    AlreadyRegistered,
    DifferentExecutor,
    UntrustedRuntime,
    WrongThread,
    DeliveryFailed,
    ShuttingDown,
    NativeMainThreadRequired,
}
impl std::fmt::Display for UiAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotRegistered => "The application UI executor is not registered",
            Self::AlreadyRegistered => "The application main executor is already registered",
            Self::DifferentExecutor => "This executor does not own the registered application UI",
            Self::UntrustedRuntime => {
                "The executor was not registered by a trusted runtime bootstrap"
            }
            Self::WrongThread => "UI control requires the registered application thread",
            Self::DeliveryFailed => "The application UI executor failed delivery",
            Self::ShuttingDown => "The application UI executor is shutting down",
            Self::NativeMainThreadRequired => {
                "The native runtime requires the application main thread"
            }
        })
    }
}
impl std::error::Error for UiAuthorityError {}

/// A UI admission capability cannot leave its current-thread control scope.
pub(crate) struct UiThreadToken<'a> {
    _executor: &'a ApplicationMainExecutor,
    _not_send: PhantomData<Rc<()>>,
}

impl UiThreadToken<'_> {
    pub(crate) fn executor(&self) -> &ApplicationMainExecutor {
        self._executor
    }
}

/// Retirement remains admissible after control closes, on the same trusted UI owner.
pub(crate) struct UiRetirementToken<'a> {
    _executor: &'a ApplicationMainExecutor,
    _not_send: PhantomData<Rc<()>>,
}

pub(crate) fn ui_retirement_authority() -> Result<UiRetirementToken<'static>, UiAuthorityError> {
    let executor = MAIN_EXECUTOR.get().ok_or(UiAuthorityError::NotRegistered)?;
    executor.validate_ui_in(&MAIN_EXECUTOR, false)?;
    Ok(UiRetirementToken {
        _executor: executor,
        _not_send: PhantomData,
    })
}

/// Selects the real runtime for owned dispatch; this does not admit the caller to native UI.
pub(crate) fn registered_ui_executor() -> Result<ApplicationMainExecutor, UiAuthorityError> {
    let executor = MAIN_EXECUTOR.get().ok_or(UiAuthorityError::NotRegistered)?;
    if executor.inner.runtime.is_none() {
        return Err(UiAuthorityError::UntrustedRuntime);
    }
    if executor.inner.failed.load(Ordering::Acquire) {
        return Err(UiAuthorityError::DeliveryFailed);
    }
    if !executor.inner.control_open.load(Ordering::Acquire) {
        return Err(UiAuthorityError::ShuttingDown);
    }
    Ok(executor.clone())
}

/// Checks the actual process registry; CLAP fixture substitution is not UI proof.
pub fn check_ui_authority() -> Result<(), UiAuthorityError> {
    ui_authority().map(|_| ())
}

pub(crate) fn ui_authority() -> Result<UiThreadToken<'static>, UiAuthorityError> {
    MAIN_EXECUTOR
        .get()
        .ok_or(UiAuthorityError::NotRegistered)?
        .enter_ui_in(&MAIN_EXECUTOR)
}

/// Only a successful thread check creates this non-transferable capability.
pub(crate) struct MainThreadToken<'a> {
    _executor: &'a ApplicationMainExecutor,
    _not_send: PhantomData<Rc<()>>,
}

/// Pending host callback flags are polled on the main event loop. Setting a flag
/// from the audio thread never dispatches a closure, locks, or allocates.
pub(crate) trait MainService: Send + Sync {
    fn service_main(&self, token: &MainThreadToken<'_>);
}

struct PendingTask {
    task: Mutex<Option<ApplicationMainTask>>,
    failed: Arc<AtomicBool>,
}
impl Drop for PendingTask {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // The runtime rejected or discarded delivery. Running or dropping
            // foreign ownership here could execute cleanup on an arbitrary
            // worker; retain that ownership until process shutdown instead.
            if !self.failed.swap(true, Ordering::AcqRel) {
                log::error!("CLAP main delivery was discarded; native ownership is quarantined");
            }
            std::mem::forget(task);
        }
    }
}

static MAIN_EXECUTOR: OnceLock<ApplicationMainExecutor> = OnceLock::new();

thread_local! {
    static OFFLINE_MAIN_SERVICE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    #[cfg(test)]
    static FIXTURE_EXECUTOR: std::cell::RefCell<Option<ApplicationMainExecutor>> = const { std::cell::RefCell::new(None) };
}

/// CLI-only control scope. Never enter this scope from realtime processing or
/// GUI workers. Foreign calls finish and relinquish audio admission before its
/// service boundary runs application-main callbacks.
pub struct ApplicationOfflineMainScope {
    previous: bool,
    _not_send: PhantomData<Rc<()>>,
}
impl ApplicationOfflineMainScope {
    pub fn enter() -> Result<Self, String> {
        let executor = ApplicationMainExecutor::registered()?;
        let _main = executor.enter()?;
        let previous = OFFLINE_MAIN_SERVICE.with(|enabled| enabled.replace(true));
        Ok(Self {
            previous,
            _not_send: PhantomData,
        })
    }
}
impl Drop for ApplicationOfflineMainScope {
    fn drop(&mut self) {
        OFFLINE_MAIN_SERVICE.with(|enabled| enabled.set(self.previous));
    }
}

/// Non-RT offline boundary only. On GUI and audio workers the TLS gate returns
/// before touching the executor, allocating, locking, or invoking native code.
pub fn offline_main_is_active() -> bool {
    OFFLINE_MAIN_SERVICE.with(std::cell::Cell::get)
}

pub fn service_offline_main() -> Result<(), String> {
    if !offline_main_is_active() {
        return Ok(());
    }
    ApplicationMainExecutor::registered()?.poll()
}

/// Dropping an unstarted request cancels its queued work. A request already
/// executing completes on the main thread, where an abandoned result is retired.
pub struct ApplicationControlJob<T> {
    receiver: std::sync::mpsc::Receiver<Result<T, String>>,
    cancelled: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}
impl<T> ApplicationControlJob<T> {
    /// Wait only from a control worker after releasing document/application
    /// locks. The main event loop must remain free to process the queued work.
    /// Failed delivery is observed within one 50 ms receive interval plus
    /// scheduling latency, even when quarantined ownership retains the sender.
    pub fn wait(self) -> Result<T, String> {
        loop {
            match self.receiver.recv_timeout(CONTROL_FAILURE_POLL) {
                Ok(result) => return result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("The CLAP main control request was cancelled".to_owned());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // A quarantined foreign task retains its sender. Observe
                    // owner failure without taking or dropping that ownership;
                    // dropping this job cancels any not-yet-started work.
                    if self.failed.load(Ordering::Acquire) {
                        return Err(UiAuthorityError::DeliveryFailed.to_string());
                    }
                }
            }
        }
    }
}
impl<T> Drop for ApplicationControlJob<T> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl ApplicationMainExecutor {
    /// Register the application event-loop adapter. Windows/Linux callers must
    /// invoke this from their actual application main loop, not a load worker.
    pub fn register_current(
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::verify_native_main()?;
        Self::install_current(&MAIN_EXECUTOR, dispatch, None)
            .map_err(|_| "The CLAP main executor is already registered".to_owned())
    }

    /// Registers the actual application event-loop owner before native loading.
    ///
    /// # Safety
    /// The caller is an audited executable/runtime bootstrap on its genuine
    /// UI/control thread. It owns and services `dispatch` there until shutdown;
    /// constructors, render workers and test substitutions cannot assert this.
    pub unsafe fn register_runtime_current(
        runtime: MainRuntime,
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, UiAuthorityError> {
        Self::verify_native_main().map_err(|_| UiAuthorityError::NativeMainThreadRequired)?;
        Self::install_current(&MAIN_EXECUTOR, dispatch, Some(runtime))
    }

    /// Install a process-wide test executor on a dedicated pumped thread.
    /// Only compiled for tests and the `test-support` feature.
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_test_executor() -> Result<Self, UiAuthorityError> {
        if MAIN_EXECUTOR.get().is_some() {
            return Err(UiAuthorityError::AlreadyRegistered);
        }
        let (dispatch_tx, dispatch_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let worker = thread::Builder::new()
            .name("oc-test-application-main".into())
            .spawn(move || {
                let executor = Self::new_current_with_runtime(
                    move |task| {
                        dispatch_tx
                            .send(task)
                            .map_err(|_| "test application main stopped".to_owned())
                    },
                    Some(MainRuntime::Test),
                );
                if ready_tx.send(executor.clone()).is_err() {
                    return;
                }
                loop {
                    if stop_rx.try_recv().is_ok() {
                        return;
                    }
                    while let Ok(task) = dispatch_rx.try_recv() {
                        task();
                    }
                    let _ = executor.poll();
                    thread::sleep(std::time::Duration::from_millis(1));
                }
            })
            .map_err(|_| UiAuthorityError::DeliveryFailed)?;
        let executor = ready_rx
            .recv()
            .map_err(|_| UiAuthorityError::DeliveryFailed)?;
        if MAIN_EXECUTOR.set(executor.clone()).is_err() {
            let _ = stop_tx.send(());
            let _ = worker.join();
            return Err(UiAuthorityError::AlreadyRegistered);
        }
        std::mem::forget(worker);
        Ok(executor)
    }

    /// Install the process-wide test executor, or reuse the test executor an
    /// earlier test already installed. A non-test executor is never reused.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ensure_test_executor() -> Result<Self, UiAuthorityError> {
        if let Some(executor) = MAIN_EXECUTOR.get() {
            return if executor.inner.runtime == Some(MainRuntime::Test) {
                Ok(executor.clone())
            } else {
                Err(UiAuthorityError::AlreadyRegistered)
            };
        }
        match Self::install_test_executor() {
            Ok(executor) => Ok(executor),
            Err(UiAuthorityError::AlreadyRegistered) => MAIN_EXECUTOR
                .get()
                .filter(|executor| executor.inner.runtime == Some(MainRuntime::Test))
                .cloned()
                .ok_or(UiAuthorityError::AlreadyRegistered),
            Err(error) => Err(error),
        }
    }

    fn install_current(
        registry: &OnceLock<Self>,
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
        runtime: Option<MainRuntime>,
    ) -> Result<Self, UiAuthorityError> {
        let executor = Self::new_current_with_runtime(dispatch, runtime);
        registry
            .set(executor.clone())
            .map_err(|_| UiAuthorityError::AlreadyRegistered)?;
        Ok(executor)
    }

    /// Checks current runtime admission without accepting a fixture executor.
    pub fn check_ui_authority(&self) -> Result<(), UiAuthorityError> {
        self.enter_ui_in(&MAIN_EXECUTOR).map(|_| ())
    }

    fn enter_ui_in(
        &self,
        registry: &OnceLock<Self>,
    ) -> Result<UiThreadToken<'_>, UiAuthorityError> {
        self.validate_ui_in(registry, true)?;
        Ok(UiThreadToken {
            _executor: self,
            _not_send: PhantomData,
        })
    }

    fn validate_ui_in(
        &self,
        registry: &OnceLock<Self>,
        require_control_open: bool,
    ) -> Result<(), UiAuthorityError> {
        let registered = registry.get().ok_or(UiAuthorityError::NotRegistered)?;
        if !Arc::ptr_eq(&registered.inner, &self.inner) {
            return Err(UiAuthorityError::DifferentExecutor);
        }
        if self.inner.runtime.is_none() {
            return Err(UiAuthorityError::UntrustedRuntime);
        }
        if self.inner.failed.load(Ordering::Acquire) {
            return Err(UiAuthorityError::DeliveryFailed);
        }
        if !self.is_current() {
            return Err(UiAuthorityError::WrongThread);
        }
        if require_control_open && !self.inner.control_open.load(Ordering::Acquire) {
            return Err(UiAuthorityError::ShuttingDown);
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.inner.runtime == Some(MainRuntime::Test) {
            return Ok(());
        }
        Self::verify_native_main().map_err(|_| UiAuthorityError::NativeMainThreadRequired)
    }

    /// The event-loop adapter reports that it can no longer deliver work.
    /// Accepted foreign ownership is quarantined by the existing task guards.
    pub fn delivery_failed(&self) {
        self.inner.control_open.store(false, Ordering::Release);
        self.inner.failed.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn fixture_current(
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self::new_current(dispatch)
    }

    /// Explicit unit-test executor substitution; production registration still
    /// verifies the native main thread. The real DSO/loader/lifecycle are unchanged.
    #[cfg(test)]
    pub(crate) fn with_fixture_current<T>(work: impl FnOnce(Self) -> T) -> T {
        struct Restore(Option<ApplicationMainExecutor>);
        impl Drop for Restore {
            fn drop(&mut self) {
                FIXTURE_EXECUTOR.with(|current| *current.borrow_mut() = self.0.take());
            }
        }
        let executor = Self::fixture_current(|_| Err("unexpected worker dispatch".into()));
        let _restore =
            Restore(FIXTURE_EXECUTOR.with(|current| current.replace(Some(executor.clone()))));
        work(executor)
    }

    #[cfg(test)]
    fn new_current(
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self::new_current_with_runtime(dispatch, None)
    }

    fn new_current_with_runtime(
        dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
        runtime: Option<MainRuntime>,
    ) -> Self {
        Self {
            inner: Arc::new(ExecutorInner {
                thread: thread::current().id(),
                runtime,
                dispatch: Box::new(dispatch),
                failed: Arc::new(AtomicBool::new(false)),
                control_open: Arc::new(AtomicBool::new(true)),
                polling: AtomicBool::new(false),
                pending: Mutex::new(Vec::new()),
                deferred_retirements: Mutex::new(Vec::new()),
                services: Mutex::new(Vec::new()),
            }),
        }
    }

    fn verify_native_main() -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: pthread_main_np has no arguments and only queries the OS.
            if unsafe { libc::pthread_main_np() } == 0 {
                return Err("Register the CLAP executor on the application main thread".into());
            }
        }
        Ok(())
    }

    pub fn registered() -> Result<Self, String> {
        #[cfg(test)]
        if let Some(executor) = FIXTURE_EXECUTOR.with(|current| current.borrow().clone()) {
            return Ok(executor);
        }
        MAIN_EXECUTOR
            .get()
            .cloned()
            .ok_or_else(|| "The application has not registered a CLAP main executor".into())
    }

    pub fn accepts_preparation(&self) -> bool {
        self.inner.control_open.load(Ordering::Acquire)
            && !self.inner.failed.load(Ordering::Acquire)
    }

    pub fn is_current(&self) -> bool {
        thread::current().id() == self.inner.thread
    }

    pub(crate) fn enter(&self) -> Result<MainThreadToken<'_>, String> {
        if self.inner.failed.load(Ordering::Acquire) {
            return Err("The CLAP main executor failed delivery; restart the application before loading plugins".into());
        }
        if !self.is_current() {
            return Err("Run CLAP control work on the registered application main executor".into());
        }
        Ok(MainThreadToken {
            _executor: self,
            _not_send: PhantomData,
        })
    }

    /// On dispatch failure the original task is returned without running it.
    /// Resource owners can retain/quarantine it rather than destructing foreign
    /// state on the wrong thread. Late callbacks from failed dispatches are inert.
    pub(crate) fn dispatch(
        &self,
        task: ApplicationMainTask,
    ) -> Result<(), (String, ApplicationMainTask)> {
        if self.inner.failed.load(Ordering::Acquire) {
            return Err(("The CLAP main executor failed delivery".into(), task));
        }
        if self.is_current() {
            task();
            return Ok(());
        }
        let pending = Arc::new(PendingTask {
            task: Mutex::new(Some(task)),
            failed: Arc::clone(&self.inner.failed),
        });
        {
            let mut registry = self
                .inner
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            registry.retain(|pending| {
                pending.upgrade().is_some_and(|pending| {
                    pending
                        .task
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .is_some()
                })
            });
            if registry.len() >= MAX_PENDING_TASKS {
                let task = pending
                    .task
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .expect("unsubmitted task");
                return Err((
                    "The CLAP main queue is full; retry after pending work finishes".into(),
                    task,
                ));
            }
            registry.push(Arc::downgrade(&pending));
        }
        let callback_pending = Arc::clone(&pending);
        let executor = self.clone();
        let result = (self.inner.dispatch)(Box::new(move || {
            if executor.inner.failed.load(Ordering::Acquire) {
                return;
            }
            // A broken runtime adapter cannot run a foreign finalizer on a
            // worker. Preserve the task for the error path or its owner.
            if !executor.is_current() {
                if !executor.inner.failed.swap(true, Ordering::AcqRel) {
                    log::error!(
                        "CLAP main dispatch ran on a worker; native ownership is quarantined"
                    );
                }
                return;
            }
            let task = callback_pending
                .task
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(task) = task {
                task();
            }
        }));
        if let Err(error) = result {
            if self.inner.runtime.is_some() {
                self.delivery_failed();
            }
            let task = pending
                .task
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(task) = task {
                return Err((error, task));
            }
        }
        Ok(())
    }

    /// Queue an explicit preparation/control operation. Synchronous plugin
    /// APIs never hide a dispatch-and-wait behind a borrowed document lock.
    pub fn dispatch_control<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<ApplicationControlJob<T>, String> {
        if !self.inner.control_open.load(Ordering::Acquire) {
            return Err("The CLAP main executor is shutting down".into());
        }
        let control_open = Arc::clone(&self.inner.control_open);
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let admission = Arc::clone(&cancelled);
        let task: ApplicationMainTask = Box::new(move || {
            if !admission.load(Ordering::Acquire) {
                let result = if control_open.load(Ordering::Acquire) {
                    work()
                } else {
                    Err("The CLAP main executor is shutting down".into())
                };
                let _ = sender.send(result);
            }
        });
        if let Err((error, task)) = self.dispatch(task) {
            drop(task);
            return Err(error);
        }
        Ok(ApplicationControlJob {
            receiver,
            cancelled,
            failed: Arc::clone(&self.inner.failed),
        })
    }

    pub(crate) fn register_service(&self, service: Weak<dyn MainService>) {
        let mut services = self
            .inner
            .services
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        services.retain(|service| service.strong_count() != 0);
        services.push(service);
    }

    /// Queue retirement without running inline, even on main. Adapters may
    /// call this while removing an owner under their document mutex; poll runs
    /// its foreign teardown only after that mutation has released the mutex.
    pub(crate) fn defer_retirement(
        &self,
        task: ApplicationMainTask,
    ) -> Result<(), (String, ApplicationMainTask)> {
        let mut queue = self
            .inner
            .deferred_retirements
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if queue.len() >= MAX_PENDING_TASKS {
            self.inner.control_open.store(false, Ordering::Release);
            return Err((
                "The CLAP retirement queue is full; restart before loading more plugins".into(),
                task,
            ));
        }
        queue.push(Arc::new(PendingTask {
            task: Mutex::new(Some(task)),
            failed: Arc::clone(&self.inner.failed),
        }));
        Ok(())
    }

    /// Stop admitting preparation while still allowing native retirement tasks.
    pub fn begin_shutdown(&self) -> Result<(), String> {
        let _token = self.enter()?;
        self.inner.control_open.store(false, Ordering::Release);
        Ok(())
    }

    /// Call from the application main-loop timer, including during playback.
    /// It also drains accepted work when runtime callbacks have not run yet.
    pub fn poll(&self) -> Result<(), String> {
        let token = self.enter()?;
        if self
            .inner
            .polling
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        struct Polling<'a>(&'a AtomicBool);
        impl Drop for Polling<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _polling = Polling(&self.inner.polling);
        let pending: Vec<_> = {
            let mut registry = self
                .inner
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let pending: Vec<_> = registry.iter().filter_map(Weak::upgrade).collect();
            registry.clear();
            pending
        };
        let deferred = {
            let mut queue = self
                .inner
                .deferred_retirements
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *queue)
        };
        for pending in pending.into_iter().chain(deferred) {
            let task = pending
                .task
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(task) = task {
                task();
            }
        }
        let services: Vec<_> = {
            let mut registry = self
                .inner
                .services
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            registry.retain(|service| service.strong_count() != 0);
            registry.iter().filter_map(Weak::upgrade).collect()
        };
        for service in services {
            service.service_main(&token);
        }
        Ok(())
    }
}

/// A CLI main loop can drain this queue while a render worker is running.
/// The loop must keep pumping until native instance finalizers have returned.
pub struct ApplicationMainQueue {
    tasks: Arc<Mutex<VecDeque<ApplicationMainTask>>>,
    executor: ApplicationMainExecutor,
}
impl ApplicationMainQueue {
    pub fn register_current() -> Result<Self, String> {
        Self::register_queue(ApplicationMainExecutor::register_current)
    }

    /// Installs the queue on the trusted executable's main/control thread.
    ///
    /// # Safety
    /// The literal executable/runtime bootstrap owns this thread and pumps the
    /// queue until shutdown. Never call this to bless a plugin/load worker.
    pub unsafe fn register_runtime_current(runtime: MainRuntime) -> Result<Self, UiAuthorityError> {
        Self::register_queue(|dispatch| {
            // SAFETY: the caller supplies the same audited bootstrap contract.
            unsafe { ApplicationMainExecutor::register_runtime_current(runtime, dispatch) }
        })
    }

    fn register_queue<E>(
        register: impl FnOnce(Box<Dispatch>) -> Result<ApplicationMainExecutor, E>,
    ) -> Result<Self, E> {
        let tasks: Arc<Mutex<VecDeque<ApplicationMainTask>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let pending = Arc::clone(&tasks);
        let executor = register(Box::new(move |task| {
            pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push_back(task);
            Ok(())
        }))?;
        Ok(Self { tasks, executor })
    }

    pub fn executor(&self) -> ApplicationMainExecutor {
        self.executor.clone()
    }

    pub fn pump(&self) -> Result<(), String> {
        let _token = self.executor.enter()?;
        // Snapshot the queue so recursive scheduling cannot monopolize the loop.
        let tasks = {
            let mut pending = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *pending)
        };
        for task in tasks {
            task();
        }
        self.executor.poll()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn executor_rejects_worker_admission_and_delivers_control_on_its_bound_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        let executor = ApplicationMainExecutor::new_current(move |task| {
            tx.send(task).map_err(|_| "Main loop closed".to_owned())
        });
        let counter = Arc::new(AtomicUsize::new(0));
        let worker_executor = executor.clone();
        let task_counter = Arc::clone(&counter);
        std::thread::spawn(move || {
            assert!(worker_executor.enter().is_err());
            assert!(worker_executor.poll().is_err());
            assert!(
                worker_executor
                    .dispatch(Box::new(move || {
                        task_counter.fetch_add(1, Ordering::Relaxed);
                    }))
                    .is_ok()
            );
        })
        .join()
        .unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        rx.recv().unwrap()();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejected_delivery_returns_the_original_and_late_callback_is_inert() {
        let (tx, rx) = std::sync::mpsc::channel();
        let executor = ApplicationMainExecutor::new_current(move |task| {
            tx.send(task).unwrap();
            Err("Main loop rejected the task".to_owned())
        });
        let counter = Arc::new(AtomicUsize::new(0));
        let task_counter = Arc::clone(&counter);
        let task = std::thread::spawn(move || {
            let (error, task) = executor
                .dispatch(Box::new(move || {
                    task_counter.fetch_add(1, Ordering::Relaxed);
                }))
                .err()
                .unwrap();
            assert!(error.contains("rejected"));
            task
        })
        .join()
        .unwrap();
        rx.recv().unwrap()();
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        task();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn incorrect_worker_dispatch_quarantines_executor_without_running_native_work() {
        let executor = ApplicationMainExecutor::new_current(|task| {
            task();
            Ok(())
        });
        let worker_executor = executor.clone();
        std::thread::spawn(move || {
            // This zero-capture task owns no resources; quarantine intentionally
            // retains it instead of running an unsafe foreign finalizer.
            assert!(
                worker_executor
                    .dispatch(Box::new(|| panic!("wrong-thread work ran")))
                    .is_ok()
            );
        })
        .join()
        .unwrap();
        assert!(executor.enter().err().unwrap().contains("failed delivery"));
        assert!(
            executor
                .dispatch(Box::new(|| panic!("quarantined executor ran")))
                .is_err()
        );
    }

    #[test]
    fn dropped_runtime_delivery_quarantines_executor() {
        let (tx, rx) = std::sync::mpsc::channel();
        let executor = ApplicationMainExecutor::new_current(move |task| {
            tx.send(task).map_err(|_| "Closed".into())
        });
        let worker_executor = executor.clone();
        std::thread::spawn(move || {
            assert!(
                worker_executor
                    .dispatch(Box::new(|| panic!("discarded work ran")))
                    .is_ok()
            );
        })
        .join()
        .unwrap();
        drop(rx.recv().unwrap());
        assert!(executor.enter().is_err());
    }

    #[test]
    fn main_poll_drains_finalizers_even_when_runtime_callback_has_not_run() {
        let (tx, rx) = std::sync::mpsc::channel();
        let executor = ApplicationMainExecutor::new_current(move |task| {
            tx.send(task).map_err(|_| "Closed".into())
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let finalizer_calls = Arc::clone(&calls);
        let worker_executor = executor.clone();
        std::thread::spawn(move || {
            assert!(
                worker_executor
                    .dispatch(Box::new(move || {
                        finalizer_calls.fetch_add(1, Ordering::Relaxed);
                    }))
                    .is_ok()
            );
        })
        .join()
        .unwrap();
        executor.begin_shutdown().unwrap();
        executor.poll().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        rx.recv().unwrap()();
        executor.poll().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(executor.enter().is_ok());
        assert!(executor.dispatch_control(|| Ok(())).is_err());
    }

    struct PendingService {
        executor: ApplicationMainExecutor,
        pending: std::sync::atomic::AtomicBool,
        serviced: AtomicUsize,
    }
    impl MainService for PendingService {
        fn service_main(&self, _: &MainThreadToken<'_>) {
            assert!(self.executor.inner.services.try_lock().is_ok());
            if self.pending.swap(false, Ordering::AcqRel) {
                self.serviced.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn callback_service_is_polled_outside_registry_lock_and_uses_weak_ownership() {
        let executor = ApplicationMainExecutor::new_current(|_| unreachable!());
        let service = Arc::new(PendingService {
            executor: executor.clone(),
            pending: std::sync::atomic::AtomicBool::new(true),
            serviced: AtomicUsize::new(0),
        });
        let erased: Arc<dyn MainService> = service.clone();
        executor.register_service(Arc::downgrade(&erased));
        drop(erased);
        executor.poll().unwrap();
        executor.poll().unwrap();
        assert_eq!(service.serviced.load(Ordering::Relaxed), 1);
        drop(service);
        executor.poll().unwrap();
        assert!(executor.inner.services.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod control_job_tests {
    use super::*;

    #[test]
    fn abandoned_queued_control_does_not_create_foreign_state() {
        let (tx, rx) = std::sync::mpsc::channel();
        let executor = ApplicationMainExecutor::new_current(move |task| {
            tx.send(task).map_err(|_| "Closed".into())
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_calls = Arc::clone(&calls);
        std::thread::spawn(move || {
            let job = executor
                .dispatch_control(move || {
                    request_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
                .unwrap();
            drop(job);
        })
        .join()
        .unwrap();
        rx.recv().unwrap()();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod authority_tests;
