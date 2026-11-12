//! The application main-loop adapter for CLAP control and deferred retirement.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use oc_host::application_main::{ApplicationMainExecutor as ClapMainExecutor, MainRuntime};
use tauri::{AppHandle, Manager, Runtime};

struct PollDelivery {
    executor: ClapMainExecutor,
    pending: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    delivered: bool,
}
impl Drop for PollDelivery {
    fn drop(&mut self) {
        if !self.delivered {
            self.executor.delivery_failed();
            self.stop.store(true, Ordering::Release);
        }
        self.pending.store(false, Ordering::Release);
    }
}

pub(crate) struct ClapRuntime {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl ClapRuntime {
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    pub(crate) fn shutdown<R: Runtime>(app: &AppHandle<R>) {
        let Ok(executor) = ClapMainExecutor::registered() else {
            return;
        };
        let native_jobs = app.try_state::<crate::state::AppState>().map(|state| {
            let jobs = Arc::clone(&state.native_jobs);
            jobs.close_and_cancel();
            jobs
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        if let Err(error) = executor.begin_shutdown() {
            log::warn!("CLAP shutdown admission: {error}");
        }
        let retired = app.try_state::<crate::state::AppState>().map(|state| {
            state.request_export_cancel();
            state.audition.cancel_all();
            let mut inner = state.lock_inner();
            inner.open_clap_editors.clear();
            (
                inner.audio.take_render_owners_for_exit(),
                std::mem::take(&mut inner.plugin_instances),
            )
        });
        if let Some((workers, instances)) = retired {
            // Native calls themselves need process isolation to bound a hung
            // plugin. This deadline bounds waiting on cooperative source workers.
            for worker in workers {
                let timeout = deadline.saturating_duration_since(std::time::Instant::now());
                if let Err(error) = worker.shutdown_for_exit(&executor, timeout) {
                    log::warn!("CLAP render retirement: {error}");
                }
            }
            drop(instances);
        }
        if let Some(jobs) = native_jobs {
            // No document/native registry mutex is retained while servicing main
            // requests. Detached blocking tasks own their receipts until all of
            // their native instances have queued retirement.
            while jobs.active() != 0 {
                if let Err(error) = executor.poll() {
                    log::warn!("CLAP export retirement: {error}");
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    log::warn!(
                        "{} native render jobs did not finish during shutdown; their ownership remains retained",
                        jobs.active()
                    );
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        if let Err(error) = executor.poll() {
            log::warn!("CLAP finalizer drain: {error}");
        }
        if let Some(runtime) = app.try_state::<Self>() {
            runtime.stop();
        }
    }

    /// Called by Tauri setup on the actual GUI/event-loop thread.
    pub(crate) fn install<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
        let dispatch = app.clone();
        // SAFETY: the only caller is Tauri's setup callback on its UI thread;
        // AppHandle dispatches work back to that runtime, retained until shutdown.
        let executor = unsafe {
            ClapMainExecutor::register_runtime_current(MainRuntime::Gui, move |task| {
                dispatch
                    .run_on_main_thread(task)
                    .map_err(|error| error.to_string())
            })
        }
        .map_err(|error| error.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let pending = Arc::new(AtomicBool::new(false));
        let handle = app.clone();
        let startup_executor = executor.clone();
        let worker = std::thread::Builder::new()
            .name("clap-main-poll".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    // Coalesce ticks when a plugin's main call is slow. Audio host
                    // callbacks only set flags; all dispatch allocation occurs here.
                    if pending
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        let mut delivery = PollDelivery {
                            executor: executor.clone(),
                            pending: Arc::clone(&pending),
                            stop: Arc::clone(&worker_stop),
                            delivered: false,
                        };
                        let poll_app = handle.clone();
                        if let Err(error) = handle.run_on_main_thread(move || {
                            delivery.delivered = true;
                            let document_free = poll_app
                                .try_state::<crate::state::AppState>()
                                .is_none_or(|state| state.inner.try_lock().is_ok());
                            if !document_free {
                                return;
                            }
                            if let Err(error) = delivery.executor.poll() {
                                delivery.executor.delivery_failed();
                                delivery.stop.store(true, Ordering::Release);
                                log::warn!("CLAP main poll stopped: {error}");
                            }
                        }) {
                            executor.delivery_failed();
                            log::warn!("CLAP main loop stopped accepting work: {error}");
                            break;
                        }
                    }
                    std::thread::park_timeout(std::time::Duration::from_millis(16));
                }
            })
            .map_err(|error| {
                startup_executor.delivery_failed();
                format!("Could not start CLAP control polling: {error}")
            })?;
        app.manage(Self {
            stop,
            worker: Some(worker),
        });
        Ok(())
    }
}
impl Drop for ClapRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if worker.thread().id() != std::thread::current().id()
                && let Err(error) = worker.join()
            {
                log::warn!("CLAP main-loop poller stopped unexpectedly: {error:?}");
            }
        }
    }
}
