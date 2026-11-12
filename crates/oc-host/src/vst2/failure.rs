//! Bounded immutable first cause shared by owned native operations and callbacks.
use super::module_authority::ModuleFailure;
use crate::native_fault::{Fault, FirstFault};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The VST2 operation that faulted.
pub enum Vst2Operation {
    Entry,
    Dispatcher(i32),
    GetParameter(i32),
    SetParameter(i32),
    ProcessReplacing,
    ProcessAccumulating,
    ProcessDouble,
    Deactivate,
    Close,
    HostCallback(i32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One VST2 fault with its operation.
pub struct Vst2FailureRecord {
    pub operation: Vst2Operation,
    pub fault: Fault,
}
/// Shared first-fault record for one VST2 instance.
pub struct Vst2Failure {
    slot: FirstFault<Vst2FailureRecord>,
    module: Option<Arc<ModuleFailure>>,
    entry_active: AtomicBool,
}
impl Default for Vst2Failure {
    fn default() -> Self {
        Self {
            slot: FirstFault::default(),
            module: None,
            entry_active: AtomicBool::new(false),
        }
    }
}
impl std::fmt::Debug for Vst2Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst2Failure")
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}
impl Vst2Failure {
    pub(super) fn with_module(module: Arc<ModuleFailure>) -> Self {
        Self {
            slot: FirstFault::default(),
            module: Some(module),
            entry_active: AtomicBool::new(true),
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn has_module(&self) -> bool {
        self.module.is_some()
    }
    pub(super) fn finish_entry(&self) {
        self.entry_active.store(false, Ordering::Release);
    }
    /// Whether a fault was recorded.
    pub fn failed(&self) -> bool {
        self.slot.failed() || self.module.as_ref().is_some_and(|module| module.failed())
    }
    /// Preserve the instance's first local cause, otherwise its shared entry cause.
    pub fn first_fault(&self) -> Option<Vst2FailureRecord> {
        if self.slot.failed() {
            self.slot.first()
        } else {
            self.module.as_ref().and_then(|module| module.first())
        }
    }
    /// Take the first fault for reporting.
    pub fn take_report(&self) -> Option<Vst2FailureRecord> {
        if self.slot.failed() {
            self.slot.take_report()
        } else {
            self.module.as_ref().and_then(|module| module.take_report())
        }
    }
    pub(super) fn record(&self, operation: Vst2Operation, fault: Fault) -> bool {
        let record = Vst2FailureRecord { operation, fault };
        if let Some(module) = &self.module {
            if operation == Vst2Operation::Entry || self.entry_active.load(Ordering::Acquire) {
                return module.record(record);
            }
            if module.failed() {
                return false;
            }
        }
        self.slot.record(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_callback_cause_precedes_native_entry_exception_for_all_aliases() {
        let module = Arc::new(ModuleFailure::default());
        let first = Vst2Failure::with_module(module.clone());
        let alias = Vst2Failure::with_module(module);
        assert!(first.record(Vst2Operation::HostCallback(7), Fault::callback_panic()));
        assert!(!first.record(Vst2Operation::Entry, Fault::host_contract("later")));
        let expected = Vst2FailureRecord {
            operation: Vst2Operation::HostCallback(7),
            fault: Fault::callback_panic(),
        };
        assert!(alias.failed());
        assert_eq!(first.first_fault(), Some(expected));
        assert_eq!(alias.take_report(), Some(expected));
        assert_eq!(first.take_report(), None);
    }

    #[test]
    fn post_entry_instance_fault_does_not_quarantine_healthy_image() {
        let module = Arc::new(ModuleFailure::default());
        let first = Vst2Failure::with_module(module.clone());
        first.finish_entry();
        assert!(first.record(Vst2Operation::SetParameter(0), Fault::callback_panic()));
        let next = Vst2Failure::with_module(module);
        assert!(!next.failed());
        assert_eq!(next.first_fault(), None);
        assert!(first.failed());
        assert_eq!(
            first.take_report().unwrap().operation,
            Vst2Operation::SetParameter(0)
        );
        assert_eq!(first.take_report(), None);
    }

    #[test]
    fn unpublished_first_cause_still_blocks_without_fabricated_report() {
        let failure = Vst2Failure::default();
        assert!(failure.slot.begin_write_for_test());
        assert!(failure.failed());
        assert_eq!(failure.first_fault(), None);
        assert_eq!(failure.take_report(), None);
        assert!(!failure.record(Vst2Operation::Close, Fault::callback_panic()));
        let expected = Vst2FailureRecord {
            operation: Vst2Operation::ProcessReplacing,
            fault: Fault::process_error(),
        };
        failure.slot.finish_write_for_test(expected);
        assert_eq!(failure.take_report(), Some(expected));
        assert_eq!(failure.take_report(), None);
    }
}
