//! Non-realtime ownership receipts for native offline work, including detached
//! blocking tasks. A dropped awaiter does not retire a receipt owned by the task.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_NATIVE_JOBS: usize = 256;
#[derive(Default)]
pub(crate) struct NativeJobs {
    state: Mutex<JobState>,
}
struct JobState {
    open: bool,
    next: u64,
    jobs: HashMap<u64, Arc<AtomicBool>>,
}
impl Default for JobState {
    fn default() -> Self {
        Self {
            open: true,
            next: 0,
            jobs: HashMap::new(),
        }
    }
}
pub(crate) struct NativeJobGuard {
    registry: Arc<NativeJobs>,
    id: u64,
    cancel: Arc<AtomicBool>,
}
impl NativeJobs {
    pub(crate) fn register(
        self: &Arc<Self>,
        cancel: Arc<AtomicBool>,
    ) -> Result<NativeJobGuard, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state.open {
            return Err("The application is shutting down; native rendering is closed".into());
        }
        if state.jobs.len() >= MAX_NATIVE_JOBS {
            return Err("Too many native render jobs are pending".into());
        }
        let id = state.next;
        state.next = id
            .checked_add(1)
            .ok_or("Native render job identities exhausted")?;
        state.jobs.insert(id, Arc::clone(&cancel));
        Ok(NativeJobGuard {
            registry: Arc::clone(self),
            id,
            cancel,
        })
    }
    pub(crate) fn close_and_cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.open = false;
        for cancel in state.jobs.values() {
            cancel.store(true, Ordering::Release);
        }
    }
    pub(crate) fn active(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .len()
    }
}
impl NativeJobGuard {
    pub(crate) fn cancel_token(&self) -> oc_engine::render_cancel::CancelToken {
        oc_engine::render_cancel::CancelToken::from_flag(Arc::clone(&self.cancel))
    }
}

impl Drop for NativeJobGuard {
    fn drop(&mut self) {
        self.registry
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_jobs_shutdown_cancels_all_receipts_and_refuses_late_work() {
        let registry = Arc::new(NativeJobs::default());
        let first_flag = Arc::new(AtomicBool::new(false));
        let second_flag = Arc::new(AtomicBool::new(false));
        let first = registry.register(Arc::clone(&first_flag)).unwrap();
        let second = registry.register(Arc::clone(&second_flag)).unwrap();
        registry.close_and_cancel();
        assert!(first_flag.load(Ordering::Acquire) && second_flag.load(Ordering::Acquire));
        assert!(registry.register(Arc::new(AtomicBool::new(false))).is_err());
        drop(first);
        assert_eq!(registry.active(), 1);
        drop(second);
        assert_eq!(registry.active(), 0);
    }
    #[test]
    fn native_jobs_detached_waiter_does_not_finish_the_workers_receipt() {
        let registry = Arc::new(NativeJobs::default());
        let receipt = registry.register(Arc::new(AtomicBool::new(false))).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (done, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let receipt = receipt;
            wait.recv().unwrap();
            drop(receipt);
            done.send(()).unwrap();
        });
        drop(worker); // Same ownership condition as dropping spawn_blocking's awaiter.
        registry.close_and_cancel();
        assert_eq!(registry.active(), 1);
        release.send(()).unwrap();
        completion.recv().unwrap();
        assert_eq!(registry.active(), 0);
    }
}
