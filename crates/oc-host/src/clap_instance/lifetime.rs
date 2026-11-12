//! Native ownership retained by both the audio owner and queued GUI leases.
use super::*;
use crate::clap_main::{ClapMainExecutor, MainService, MainThreadToken};
use crate::clap_module::NativeModuleLease;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct ClapCore {
    pub(super) plugin: *const clap_plugin,
    pub(super) host: ManuallyDrop<Box<ClapHostData>>,
    pub(super) failure: Arc<ClapFailure>,
    pub(super) session: Arc<Mutex<GuiSession>>,
    pub(super) retired: AtomicBool,
    pub(super) active: AtomicBool,
    pub(super) processing: AtomicBool,
    executor: Option<ClapMainExecutor>,
    module: ManuallyDrop<Option<NativeModuleLease>>,
    main_busy: AtomicBool,
    pub(super) parameters: Mutex<Vec<PluginParam>>,
    pub(super) controls: Mutex<Vec<(u32, f64)>>,
    pub(super) control_pending: AtomicBool,
    pub(super) can_flush: AtomicBool,
    control_thread: std::thread::ThreadId,
}
// SAFETY: a ClapInstance owns the only audio/control access to native processing
// state. GUI leases only call the main-thread GUI API and retain this allocation.
// Final destruction starts only after every strong owner has disappeared.
unsafe impl Send for ClapCore {}
// SAFETY: shared core fields are immutable pointers, atomics, and a GUI session
// mutex. Raw instance access remains private to the audio owner and main GUI API.
unsafe impl Sync for ClapCore {}

impl ClapCore {
    pub(super) fn control_is_current(&self) -> bool {
        self.executor.as_ref().map_or_else(
            || std::thread::current().id() == self.control_thread,
            ClapMainExecutor::is_current,
        )
    }
    pub(super) fn main_call(&self) -> Option<MainCallPermit<'_>> {
        if !self.control_is_current()
            || self
                .main_busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(MainCallPermit(self))
    }
    pub(super) fn audio_call(&self) -> Result<AudioCallPermit<'_>, Fault> {
        if self
            .host
            .audio_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Fault::host_contract(
                "overlapping audio operations require a quiescent native owner",
            ));
        }
        self.host
            .audio_thread
            .store(audio_thread_marker(), Ordering::Release);
        Ok(AudioCallPermit(&self.host))
    }

    pub(super) fn check_control_thread(&self) -> Result<(), String> {
        if let Some(executor) = &self.executor {
            executor.enter().map(|_| ())
        } else if std::thread::current().id() == self.control_thread {
            Ok(())
        } else {
            Err(
                "Run this CLAP control operation on its registered application main executor"
                    .into(),
            )
        }
    }

    pub(super) fn new(
        plugin: *const clap_plugin,
        host: Box<ClapHostData>,
        failure: Arc<ClapFailure>,
        session: Arc<Mutex<GuiSession>>,
        module: Option<NativeModuleLease>,
    ) -> Arc<Self> {
        let core = Arc::new(Self {
            plugin,
            control_thread: host.main_thread,
            host: ManuallyDrop::new(host),
            failure,
            session,
            retired: AtomicBool::new(false),
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            executor: ClapMainExecutor::registered().ok(),
            module: ManuallyDrop::new(module),
            main_busy: AtomicBool::new(false),
            parameters: Mutex::new(Vec::new()),
            controls: Mutex::new(Vec::new()),
            control_pending: AtomicBool::new(false),
            can_flush: AtomicBool::new(false),
        });
        if let Some(executor) = &core.executor {
            let service: Arc<dyn MainService> = core.clone();
            executor.register_service(Arc::downgrade(&service));
        }
        core
    }
}
pub(super) struct MainCallPermit<'a>(&'a ClapCore);
impl Drop for MainCallPermit<'_> {
    fn drop(&mut self) {
        self.0.main_busy.store(false, Ordering::Release);
    }
}
pub(super) struct AudioCallPermit<'a>(&'a ClapHostData);
impl Drop for AudioCallPermit<'_> {
    fn drop(&mut self) {
        self.0
            .audio_thread
            .store(ptr::null_mut(), Ordering::Release);
        self.0.audio_busy.store(false, Ordering::Release);
    }
}
impl MainService for ClapCore {
    fn service_main(&self, _: &MainThreadToken<'_>) {
        if self.retired.load(Ordering::Acquire) || self.failure.failed() {
            return;
        }
        {
            let Some(_permit) = self.main_call() else {
                return;
            };
            if self.host.gui_close_pending.load(Ordering::Acquire)
                && let Ok(mut session) = self.session.try_lock()
                && self.host.gui_close_pending.swap(false, Ordering::AcqRel)
                && session.gui != 0
            {
                // SAFETY: main admission and the strong core retain the cached GUI extension.
                unsafe {
                    GuiPtrs::close_session(self.plugin, session.gui as *const _, &mut session)
                };
            }
            if self
                .host
                .main_callback_pending
                .swap(false, Ordering::AcqRel)
            {
                // SAFETY: the strong service lease retains this initialized
                // descriptor and its executor owns the captured main thread.
                let callback = unsafe { (*self.plugin).on_main_thread };
                let result = match callback {
                    // SAFETY: exact CLAP on_main_thread ABI and main admission.
                    Some(callback) => unsafe { clap_guard::plugin_void(callback, self.plugin) },
                    None => Err(Fault::host_contract(
                        "the plugin requested a main callback but exposes no on_main_thread callback",
                    )),
                };
                if let Err(fault) = result {
                    self.failure.record(ClapOperation::MainCallback, fault);
                }
            }
        }
        if self.failure.failed() {
            return;
        }
        if let Err(error) = self.apply_control_changes() {
            log::error!("CLAP queued control failed: {error}");
        }
    }
}
impl Drop for ClapCore {
    fn drop(&mut self) {
        // SAFETY: the last strong owner has exclusive native ownership. Moving
        // the box into the finalizer keeps every host callback address stable.
        let host = unsafe { ManuallyDrop::take(&mut self.host) };
        let finalizer = NativeFinalizer {
            plugin: self.plugin,
            host: ManuallyDrop::new(host),
            failure: Arc::clone(&self.failure),
            session: Arc::clone(&self.session),
            active: self.active.load(Ordering::Acquire),
            // SAFETY: the last strong core exclusively transfers its module lease.
            module: ManuallyDrop::new(unsafe { ManuallyDrop::take(&mut self.module) }),
        };
        if let Some(executor) = &self.executor {
            if let Err((error, task)) = executor.dispatch(Box::new(move || finalizer.run())) {
                log::error!("CLAP native finalization is retained: {error}");
                std::mem::forget(task);
            }
        } else if std::thread::current().id() == self.control_thread {
            finalizer.run();
        } else {
            log::error!(
                "CLAP native finalization requires its registered main executor; ownership is retained"
            );
            std::mem::forget(finalizer);
        }
    }
}

struct NativeFinalizer {
    plugin: *const clap_plugin,
    host: ManuallyDrop<Box<ClapHostData>>,
    failure: Arc<ClapFailure>,
    session: Arc<Mutex<GuiSession>>,
    active: bool,
    module: ManuallyDrop<Option<NativeModuleLease>>,
}
// SAFETY: the last strong core moved exclusive native ownership here after its
// audio owner stopped. The executor admits teardown only on its bound main thread.
unsafe impl Send for NativeFinalizer {}
impl NativeFinalizer {
    fn invoke(&self, operation: ClapOperation, call: impl FnOnce() -> Result<(), Fault>) -> bool {
        if self.failure.failed() {
            return false;
        }
        match call() {
            Ok(()) => true,
            Err(fault) => {
                self.failure.record(operation, fault);
                false
            }
        }
    }
    fn run(mut self) {
        if self.failure.failed() {
            return;
        }
        {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if session.created && session.gui != 0 {
                // SAFETY: the session cached this extension while the strong core
                // was live; no GUI lease or audio owner remains during finalization.
                unsafe {
                    GuiPtrs::close_session(self.plugin, session.gui as *const _, &mut session)
                };
            }
        }
        if self.failure.failed() {
            return;
        }
        if self.active {
            // SAFETY: no audio owner remains and the executor owns the native
            // control thread; the plugin descriptor remains live until destroy.
            let Some(deactivate) = (unsafe { (*self.plugin).deactivate }) else {
                self.failure
                    .record(ClapOperation::Deactivate, Fault::quarantined());
                return;
            };
            // SAFETY: The Core/finalizer retains the native descriptor and module; its main admission and exact-signature shim keep borrowed metadata/stream arguments live through the call.
            if !self.invoke(ClapOperation::Deactivate, || unsafe {
                clap_guard::plugin_void(deactivate, self.plugin)
            }) {
                return;
            }
        }
        // SAFETY: the initialized plugin descriptor remains owned until this call.
        let Some(destroy) = (unsafe { (*self.plugin).destroy }) else {
            self.failure
                .record(ClapOperation::Destroy, Fault::quarantined());
            return;
        };
        // SAFETY: GUI/audio users are retired and the inactive plugin is uniquely owned.
        if self.invoke(ClapOperation::Destroy, || unsafe {
            clap_guard::plugin_void(destroy, self.plugin)
        }) {
            // SAFETY: successful destroy retires every host callback. The fault
            // path intentionally leaves this ManuallyDrop allocation retained.
            unsafe { ManuallyDrop::drop(&mut self.host) };
            // SAFETY: instance/GUI/audio callbacks are retired before the final
            // module lease can run entry.deinit and release its owned OS reference.
            unsafe { ManuallyDrop::drop(&mut self.module) };
        }
    }
}
