//! Persistent workers for real-time block rendering.
//!
//! A streaming block fans independent jobs (one per source lane, for example)
//! out to threads that live as long as the renderer. Dispatch neither spawns,
//! allocates nor locks: the caller publishes a job count and a borrowed job,
//! wakes the workers, claims indices from a shared counter like every worker,
//! and returns once each claimed job has finished. Idle workers spin briefly
//! between blocks and then park, so back-to-back blocks avoid a wake-up
//! round trip while a stopped transport costs no CPU.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

/// How long an idle worker keeps polling for the next block before parking.
const SPIN_BEFORE_PARK: Duration = Duration::from_micros(200);

/// The job of one dispatch, erased to a thin pointer for the workers.
type JobFn<'a> = dyn Fn(usize) + Sync + 'a;

struct Shared {
    /// Incremented once per dispatch; workers wait for it to change.
    generation: AtomicU64,
    /// Points at the caller's `&JobFn` for the current dispatch.
    job: AtomicPtr<()>,
    jobs: AtomicUsize,
    next: AtomicUsize,
    /// Workers that have not yet finished the current dispatch.
    running: AtomicUsize,
    panicked: AtomicBool,
    shutdown: AtomicBool,
    /// The dispatching thread, unparked by the last worker to finish.
    caller: AtomicPtr<Thread>,
}

impl Shared {
    /// Claim and run jobs until none remain.
    fn drain(&self) {
        let job = self.job.load(Ordering::Acquire) as *const &JobFn<'static>;
        let jobs = self.jobs.load(Ordering::Acquire);
        loop {
            let index = self.next.fetch_add(1, Ordering::AcqRel);
            if index >= jobs {
                return;
            }
            // SAFETY: `dispatch` stores a pointer to a `&JobFn` that lives on its
            // stack and does not return until `running` reaches zero, which a
            // worker only signals after this loop ends, so the job outlives
            // every call made through it.
            let job = unsafe { &*job };
            if catch_unwind(AssertUnwindSafe(|| job(index))).is_err() {
                self.panicked.store(true, Ordering::Release);
            }
        }
    }
}

/// Why a dispatch did not complete normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockWorkError {
    /// At least one job panicked; the others still ran.
    JobPanicked,
}

impl std::fmt::Display for BlockWorkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JobPanicked => f.write_str("a block job panicked"),
        }
    }
}

impl std::error::Error for BlockWorkError {}

/// A fixed set of block workers. The dispatching thread works too, so a pool
/// built for `n` threads of parallelism spawns `n - 1` workers.
pub struct BlockWorkers {
    shared: Arc<Shared>,
    threads: Vec<(Thread, JoinHandle<()>)>,
    caller: Thread,
}

impl BlockWorkers {
    /// Spawn the workers for `parallelism` threads, including the caller.
    /// `init` runs once on each worker before it accepts jobs, so a worker can
    /// adopt per-thread host state that the dispatching thread holds.
    pub fn new(
        parallelism: usize,
        name: &str,
        init: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            job: AtomicPtr::new(std::ptr::null_mut()),
            jobs: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            panicked: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            caller: AtomicPtr::new(std::ptr::null_mut()),
        });
        let init = Arc::new(init);
        let mut threads = Vec::with_capacity(parallelism.saturating_sub(1));
        for index in 1..parallelism.max(1) {
            let shared = Arc::clone(&shared);
            let init = Arc::clone(&init);
            let handle = std::thread::Builder::new()
                .name(format!("{name}-{index}"))
                .spawn(move || {
                    init();
                    worker_loop(&shared);
                })?;
            threads.push((handle.thread().clone(), handle));
        }
        Ok(Self {
            shared,
            threads,
            caller: std::thread::current(),
        })
    }

    /// Threads that run jobs during a dispatch, including the caller.
    pub fn parallelism(&self) -> usize {
        self.threads.len() + 1
    }

    /// Run `job(0..jobs)` across the workers and the calling thread, and return
    /// once every index has run. Indices are claimed in ascending order, so a
    /// caller that wants the longest jobs to start first orders them that way.
    pub fn dispatch(&mut self, jobs: usize, job: &JobFn<'_>) -> Result<(), BlockWorkError> {
        if jobs == 0 {
            return Ok(());
        }
        let shared = &*self.shared;
        shared.panicked.store(false, Ordering::Relaxed);
        if self.threads.is_empty() || jobs == 1 {
            for index in 0..jobs {
                if catch_unwind(AssertUnwindSafe(|| job(index))).is_err() {
                    shared.panicked.store(true, Ordering::Relaxed);
                }
            }
            return self.outcome();
        }
        // The current thread is the one that will park; `&mut self` makes
        // dispatches exclusive, so the pointer stays valid for this call.
        self.caller = std::thread::current();
        shared.caller.store(
            &self.caller as *const Thread as *mut Thread,
            Ordering::Release,
        );
        let erased: &JobFn<'_> = job;
        shared
            .job
            .store(&erased as *const &JobFn<'_> as *mut (), Ordering::Release);
        shared.jobs.store(jobs, Ordering::Release);
        shared.next.store(0, Ordering::Release);
        shared.running.store(self.threads.len(), Ordering::Release);
        shared.generation.fetch_add(1, Ordering::AcqRel);
        for (thread, _) in &self.threads {
            thread.unpark();
        }
        shared.drain();
        let spin_until = Instant::now() + SPIN_BEFORE_PARK;
        while shared.running.load(Ordering::Acquire) != 0 {
            if Instant::now() < spin_until {
                std::hint::spin_loop();
            } else {
                std::thread::park_timeout(Duration::from_millis(1));
            }
        }
        shared.job.store(std::ptr::null_mut(), Ordering::Release);
        self.outcome()
    }

    fn outcome(&self) -> Result<(), BlockWorkError> {
        if self.shared.panicked.load(Ordering::Acquire) {
            Err(BlockWorkError::JobPanicked)
        } else {
            Ok(())
        }
    }
}

fn worker_loop(shared: &Shared) {
    // Generations start at zero and a dispatch waits for every worker, so a
    // worker that starts late still sees the first dispatch as new.
    let mut seen = 0;
    loop {
        let spin_until = Instant::now() + SPIN_BEFORE_PARK;
        loop {
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            let generation = shared.generation.load(Ordering::Acquire);
            if generation != seen {
                seen = generation;
                break;
            }
            if Instant::now() < spin_until {
                std::hint::spin_loop();
            } else {
                std::thread::park();
            }
        }
        shared.drain();
        if shared.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            let caller = shared.caller.load(Ordering::Acquire);
            // SAFETY: the caller stores a pointer to its own `Thread` handle
            // before publishing the generation and waits for `running` to
            // reach zero before it can move or drop the pool.
            unsafe { (*caller).unpark() };
        }
    }
}

impl Drop for BlockWorkers {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        for (thread, _) in &self.threads {
            thread.unpark();
        }
        for (_, handle) in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Hands each job exclusive access to one element of a slice.
///
/// `BlockWorkers` runs every index exactly once per dispatch, so job `i` may
/// borrow element `i` mutably while other jobs borrow their own elements.
pub struct DisjointSlice<'a, T> {
    base: *mut T,
    len: usize,
    _borrow: std::marker::PhantomData<&'a mut [T]>,
}

// SAFETY: each element is reached by one job at a time (see `get`), so sharing
// the wrapper only moves `&mut T` across threads, which `T: Send` permits.
unsafe impl<T: Send> Sync for DisjointSlice<'_, T> {}

impl<'a, T> DisjointSlice<'a, T> {
    pub fn new(slice: &'a mut [T]) -> Self {
        Self {
            base: slice.as_mut_ptr(),
            len: slice.len(),
            _borrow: std::marker::PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrow element `index`.
    ///
    /// # Safety
    /// No other live reference to element `index` may exist. Calling this once
    /// per index per dispatch satisfies that.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self, index: usize) -> &mut T {
        assert!(index < self.len, "disjoint slice index out of range");
        // SAFETY: the index is in bounds and the caller guarantees exclusivity.
        unsafe { &mut *self.base.add(index) }
    }

    /// Read element `index`, which several jobs may do at once.
    ///
    /// # Safety
    /// No mutable reference to element `index` may be live, and none may be
    /// created while the returned reference is.
    pub unsafe fn get_ref(&self, index: usize) -> &T
    where
        T: Sync,
    {
        assert!(index < self.len, "disjoint slice index out of range");
        // SAFETY: the index is in bounds and the caller excludes writers.
        unsafe { &*self.base.add(index) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn every_index_runs_exactly_once_per_dispatch() {
        let mut workers = BlockWorkers::new(4, "test-block", || {}).unwrap();
        assert_eq!(workers.parallelism(), 4);
        let counts: Vec<AtomicU32> = (0..257).map(|_| AtomicU32::new(0)).collect();
        for _ in 0..200 {
            workers
                .dispatch(counts.len(), &|index| {
                    counts[index].fetch_add(1, Ordering::Relaxed);
                })
                .unwrap();
        }
        assert!(
            counts
                .iter()
                .all(|count| count.load(Ordering::Relaxed) == 200)
        );
    }

    #[test]
    fn jobs_mutate_their_own_elements_in_parallel() {
        let mut workers = BlockWorkers::new(3, "test-block", || {}).unwrap();
        let mut values: Vec<u64> = (0..64).collect();
        let slice = DisjointSlice::new(&mut values);
        workers
            .dispatch(slice.len(), &|index| {
                // SAFETY: each index runs once per dispatch.
                let value = unsafe { slice.get(index) };
                *value *= 3;
            })
            .unwrap();
        assert_eq!(values, (0..64).map(|v| v * 3).collect::<Vec<_>>());
    }

    #[test]
    fn work_actually_spreads_over_the_workers() {
        let mut workers = BlockWorkers::new(4, "test-block", || {}).unwrap();
        let threads: Vec<AtomicU64> = (0..16).map(|_| AtomicU64::new(0)).collect();
        let ids = std::sync::Mutex::new(std::collections::HashSet::new());
        workers
            .dispatch(threads.len(), &|_| {
                ids.lock().unwrap().insert(std::thread::current().id());
                std::thread::sleep(Duration::from_millis(5));
            })
            .unwrap();
        assert!(ids.lock().unwrap().len() > 1, "all jobs ran on one thread");
    }

    #[test]
    fn a_panicking_job_reports_and_the_pool_keeps_working() {
        let mut workers = BlockWorkers::new(2, "test-block", || {}).unwrap();
        let ran = AtomicU32::new(0);
        let result = workers.dispatch(8, &|index| {
            ran.fetch_add(1, Ordering::Relaxed);
            if index == 3 {
                panic!("job failure");
            }
        });
        assert_eq!(result, Err(BlockWorkError::JobPanicked));
        assert_eq!(ran.load(Ordering::Relaxed), 8);
        workers.dispatch(4, &|_| {}).unwrap();
    }

    #[test]
    fn init_runs_on_every_worker_before_jobs() {
        let started = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&started);
        let mut workers = BlockWorkers::new(5, "test-block", move || {
            counter.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap();
        workers.dispatch(32, &|_| {}).unwrap();
        // A worker may not have claimed a job, but it has started.
        let deadline = Instant::now() + Duration::from_secs(5);
        while started.load(Ordering::Acquire) < 4 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
}
