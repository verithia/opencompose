//! Bounded native failure payload and immutable first-fault publication.
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[repr(C)]
pub(crate) struct RawFault {
    pub(crate) kind: i32,
    pub(crate) message: [u8; 256],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Kind of native fault captured at a plugin boundary.
pub enum FaultKind {
    CppException,
    UnknownException,
    CallbackPanic,
    ProcessError,
    Quarantined,
    HostContract,
}

/// Native failure formatting belongs outside the audio callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    message: [u8; 256],
}
impl Fault {
    pub(crate) fn from_raw(raw: RawFault) -> Self {
        Self {
            kind: if raw.kind == 1 {
                FaultKind::CppException
            } else {
                FaultKind::UnknownException
            },
            message: raw.message,
        }
    }
    pub(crate) fn from_message(kind: FaultKind, message: &str) -> Self {
        let mut fault = Self {
            kind,
            message: [0; 256],
        };
        let count = message.len().min(255);
        fault.message[..count].copy_from_slice(&message.as_bytes()[..count]);
        fault
    }
    /// A host-side contract violation.
    pub fn host_contract(message: &'static str) -> Self {
        Self::from_message(FaultKind::HostContract, message)
    }
    /// A fault that quarantines the plugin.
    pub fn quarantined() -> Self {
        Self::from_message(FaultKind::Quarantined, "")
    }
    /// A native process error.
    pub fn process_error() -> Self {
        Self::from_message(FaultKind::ProcessError, "")
    }
    /// A Rust panic crossing a callback boundary.
    pub fn callback_panic() -> Self {
        Self::from_message(FaultKind::CallbackPanic, "")
    }
    /// Human-readable description for a plugin family.
    pub fn describe_for(self, family: &str) -> String {
        let end = self
            .message
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(256);
        let message = String::from_utf8_lossy(&self.message[..end]);
        match self.kind {
            FaultKind::CppException | FaultKind::UnknownException => {
                format!("{family} plugin threw: {message}")
            }
            FaultKind::ProcessError => {
                format!("{family} plugin reported a processing error; the instance was quarantined")
            }
            FaultKind::CallbackPanic => {
                format!("{family} host callback panicked; the instance was quarantined")
            }
            FaultKind::HostContract => format!("{family} control contract: {message}"),
            FaultKind::Quarantined => {
                format!("{family} instance is quarantined after an earlier failure")
            }
        }
    }
}

const HEALTHY: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;

pub(crate) struct FirstFault<T: Copy> {
    state: AtomicU8,
    reported: AtomicBool,
    record: UnsafeCell<MaybeUninit<T>>,
}
impl<T: Copy> Default for FirstFault<T> {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(HEALTHY),
            reported: AtomicBool::new(false),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}
// SAFETY: one publisher owns HEALTHY→WRITING and publishes its immutable Copy
// payload with Release. Readers observe READY with Acquire before accessing it.
unsafe impl<T: Copy + Send + Sync> Sync for FirstFault<T> {}
impl<T: Copy> FirstFault<T> {
    pub(crate) fn failed(&self) -> bool {
        self.state.load(Ordering::Acquire) != HEALTHY
    }
    pub(crate) fn record(&self, record: T) -> bool {
        if self
            .state
            .compare_exchange(HEALTHY, WRITING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        // SAFETY: this publisher exclusively owns WRITING; a fixed Copy write
        // cannot allocate or panic. No reader accesses an unpublished record.
        unsafe {
            (*self.record.get()).write(record);
        }
        self.state.store(READY, Ordering::Release);
        true
    }
    pub(crate) fn first(&self) -> Option<T> {
        if self.state.load(Ordering::Acquire) != READY {
            return None;
        }
        // SAFETY: Acquire observed an initialized payload, never mutated again.
        Some(unsafe { (*self.record.get()).assume_init_read() })
    }
    pub(crate) fn take_report(&self) -> Option<T> {
        let record = self.first()?;
        self.reported
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(record)
    }
    #[cfg(test)]
    pub(crate) fn begin_write_for_test(&self) -> bool {
        self.state
            .compare_exchange(HEALTHY, WRITING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    #[cfg(test)]
    pub(crate) fn finish_write_for_test(&self, record: T) {
        assert_eq!(self.state.load(Ordering::Acquire), WRITING);
        // SAFETY: the fixture has exclusive publication ownership after begin.
        unsafe {
            (*self.record.get()).write(record);
        }
        self.state.store(READY, Ordering::Release);
    }
}
