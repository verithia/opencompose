//! Immutable first-fault publication shared by control, GUI and audio callers.
use std::sync::Arc;

use crate::clap_guard::Fault;

use crate::native_fault::FirstFault;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// CLAP entry point that can fault.
pub enum ClapOperation {
    EntryInit,
    EntryDeinit,
    GetFactory,
    FactoryCount,
    FactoryDescriptor,
    CreatePlugin,
    Init,
    Activate,
    StartProcessing,
    StopProcessing,
    Reset,
    Process,
    Deactivate,
    Destroy,
    GetExtension,
    PortQuery,
    ParameterQuery,
    ParameterFlush,
    StateLoad,
    StateSave,
    GuiQuery,
    GuiCreate,
    GuiSetParent,
    GuiShow,
    GuiDestroy,
    HostCallback,
    MainCallback,
}
impl ClapOperation {
    fn affects_module(self) -> bool {
        matches!(
            self,
            Self::EntryInit
                | Self::EntryDeinit
                | Self::GetFactory
                | Self::FactoryCount
                | Self::FactoryDescriptor
                | Self::CreatePlugin
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Recorded CLAP fault.
pub struct ClapFailureRecord {
    pub operation: ClapOperation,
    pub fault: Fault,
}

type FaultSlot = FirstFault<ClapFailureRecord>;

/// Module-level faults affect every instance holding this module authority.
#[derive(Default)]
pub(crate) struct ClapModuleFailure {
    slot: FaultSlot,
}
impl ClapModuleFailure {
    pub(crate) fn record(&self, operation: ClapOperation, fault: Fault) -> bool {
        self.slot.record(ClapFailureRecord { operation, fault })
    }
}

/// Keep this handle when a native instance may be retired. Polling and recording
/// are bounded atomics plus a fixed Copy record, with no RT locks or allocation.
#[derive(Default)]
pub struct ClapFailure {
    local: FaultSlot,
    module: Option<Arc<ClapModuleFailure>>,
}
impl ClapFailure {
    pub(crate) fn with_module(module: Arc<ClapModuleFailure>) -> Self {
        Self {
            local: FaultSlot::default(),
            module: Some(module),
        }
    }
    /// True even while the first publisher is still copying its record.
    pub fn failed(&self) -> bool {
        self.local.failed()
            || self
                .module
                .as_ref()
                .is_some_and(|module| module.slot.failed())
    }
    /// None while publication is in progress; the reporting caller should retry.
    pub fn first_fault(&self) -> Option<ClapFailureRecord> {
        if self.local.failed() {
            return self.local.first();
        }
        self.module.as_ref().and_then(|module| module.slot.first())
    }
    /// Claim one concrete report outside the audio callback. An unavailable
    /// record consumes nothing; module reports are shared across its instances.
    pub fn take_report(&self) -> Option<ClapFailureRecord> {
        if self.local.failed() {
            return self.local.take_report();
        }
        self.module
            .as_ref()
            .and_then(|module| module.slot.take_report())
    }
    pub(crate) fn record(&self, operation: ClapOperation, fault: Fault) -> bool {
        if operation.affects_module()
            && let Some(module) = &self.module
        {
            return module.record(operation, fault);
        }
        if self
            .module
            .as_ref()
            .is_some_and(|module| module.slot.failed())
        {
            return false;
        }
        self.local.record(ClapFailureRecord { operation, fault })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writing_is_failed_but_does_not_consume_a_report() {
        let failure = ClapFailure::default();
        assert!(failure.local.begin_write_for_test());
        assert!(failure.failed());
        assert!(failure.first_fault().is_none());
        assert!(failure.take_report().is_none());
        let record = ClapFailureRecord {
            operation: ClapOperation::Process,
            fault: Fault::process_error(),
        };
        failure.local.finish_write_for_test(record);
        assert_eq!(failure.take_report(), Some(record));
        assert!(failure.take_report().is_none());
        assert_eq!(failure.first_fault(), Some(record));
    }
    #[test]
    fn concurrent_publishers_leave_one_immutable_record_and_one_report() {
        let failure = Arc::new(ClapFailure::default());
        let start = Arc::new(std::sync::Barrier::new(16));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let failure = Arc::clone(&failure);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    failure.record(ClapOperation::HostCallback, Fault::callback_panic())
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        let record = failure.first_fault().unwrap();
        assert!(!failure.record(ClapOperation::Process, Fault::process_error()));
        assert_eq!(failure.first_fault(), Some(record));
        assert_eq!(failure.take_report(), Some(record));
        assert!(failure.take_report().is_none());
    }
    #[test]
    fn a_module_fault_reaches_every_instance_without_duplicate_reports() {
        let module = Arc::new(ClapModuleFailure::default());
        let first = ClapFailure::with_module(Arc::clone(&module));
        let second = ClapFailure::with_module(module);
        assert!(first.record(ClapOperation::CreatePlugin, Fault::callback_panic()));
        assert!(first.failed() && second.failed());
        let record = first.first_fault().unwrap();
        assert_eq!(second.first_fault(), Some(record));
        assert!(!second.record(ClapOperation::Process, Fault::process_error()));
        assert_eq!(first.take_report(), Some(record));
        assert!(second.take_report().is_none());
    }
}
