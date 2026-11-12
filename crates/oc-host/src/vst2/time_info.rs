//! Bounded, nonblocking publication of optional VST2 time information.
//!
//! Each native callback thread receives its own record. Publishing the next
//! processing slice never overwrites a pointer previously returned to a helper
//! thread. Slots are retained for the instance lifetime, with no callback-time
//! allocation or eviction; exhaustion makes the optional GET_TIME reply NULL.

use super::abi::VstTimeInfo;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct Slot {
    owner: AtomicU64,
    writing: AtomicBool,
    value: UnsafeCell<VstTimeInfo>,
}

pub(super) struct TimeInfoPublisher<const THREADS: usize = 64> {
    writing: AtomicBool,
    available: AtomicBool,
    current: UnsafeCell<VstTimeInfo>,
    slots: [Slot; THREADS],
}

// SAFETY: The shared current record is accessed only with the exclusive atomic
// permit. Output records are written only by their owning native thread while
// its slot permit is held; the unsafe callback API limits borrowed reads to that
// thread and excludes reads overlapping its next request. No slot is evicted.
unsafe impl<const THREADS: usize> Sync for TimeInfoPublisher<THREADS> {}

struct Permit<'a>(&'a AtomicBool);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn try_permit(flag: &AtomicBool) -> Option<Permit<'_>> {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .ok()
        .map(|_| Permit(flag))
}

impl<const THREADS: usize> TimeInfoPublisher<THREADS> {
    pub(super) fn new(sample_rate: u32) -> Self {
        let initial = VstTimeInfo {
            sample_rate: f64::from(sample_rate),
            ..VstTimeInfo::default()
        };
        Self {
            writing: AtomicBool::new(false),
            available: AtomicBool::new(sample_rate != 0),
            current: UnsafeCell::new(initial),
            slots: std::array::from_fn(|_| Slot {
                owner: AtomicU64::new(0),
                writing: AtomicBool::new(false),
                value: UnsafeCell::new(initial),
            }),
        }
    }

    /// One bounded attempt; refusal invalidates the previous processing slice.
    pub(super) fn publish(&self, value: VstTimeInfo) -> bool {
        self.available.store(false, Ordering::Release);
        if !value.sample_rate.is_finite()
            || value.sample_rate <= 0.0
            || !value.sample_pos.is_finite()
        {
            return false;
        }
        let Some(_permit) = try_permit(&self.writing) else {
            return false;
        };
        // SAFETY: This permit exclusively excludes both readers and publishers
        // of the current record; output slots are independent allocations.
        unsafe { self.current.get().write(value) };
        self.available.store(true, Ordering::Release);
        true
    }

    pub(super) fn snapshot(&self) -> Option<VstTimeInfo> {
        if !self.available.load(Ordering::Acquire) {
            return None;
        }
        let _permit = try_permit(&self.writing)?;
        // SAFETY: The exclusive permit prevents any concurrent current write.
        let value = unsafe { self.current.get().read() };
        self.available.load(Ordering::Acquire).then_some(value)
    }

    /// Return an optional, read-only callback-thread borrow.
    ///
    /// # Safety
    /// The caller must keep this publisher alive while the plugin reads the
    /// result. The result may only be read on the calling native thread, with
    /// no reads overlapping that thread's next request or instance retirement.
    /// Plugins must not mutate the returned record or transfer its pointer to
    /// another thread. Other threads must make their own GET_TIME request.
    pub(super) unsafe fn callback_pointer(&self) -> *const VstTimeInfo {
        let Some(value) = self.snapshot() else {
            return std::ptr::null();
        };
        let Some(thread) = native_thread_id() else {
            return std::ptr::null();
        };
        for slot in &self.slots {
            let owner = slot.owner.load(Ordering::Acquire);
            if owner != thread
                && (owner != 0
                    || slot
                        .owner
                        .compare_exchange(0, thread, Ordering::AcqRel, Ordering::Acquire)
                        .is_err())
            {
                continue;
            }
            let Some(_permit) = try_permit(&slot.writing) else {
                return std::ptr::null();
            };
            // SAFETY: Native thread ownership is exclusive while the thread
            // lives, and the permit rejects reentrant slot writes. The caller's
            // contract excludes reads overlapping its next request. Dead-thread
            // ID reuse cannot overlap a permitted read by that dead thread.
            unsafe { slot.value.get().write(value) };
            return slot.value.get().cast_const();
        }
        std::ptr::null()
    }
}

pub(super) fn native_thread_id() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut id = 0;
        // SAFETY: A zero pthread requests the current thread; id is a writable
        // u64 output matching pthread_threadid_np's uint64_t ABI.
        let status = unsafe { libc::pthread_threadid_np(0, &mut id) };
        (status == 0 && id != 0).then_some(id)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid has no pointer inputs or ownership transfer.
        let id = unsafe { libc::gettid() };
        (id > 0).then_some(id as u64)
    }
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThreadId() -> u32;
        }
        // SAFETY: The OS returns the calling thread's nonzero ID with no inputs.
        let id = unsafe { GetCurrentThreadId() };
        (id != 0).then_some(u64::from(id))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(test)]
#[path = "time_info_tests.rs"]
mod tests;
