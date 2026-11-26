//! Shared admission for inventory scans and on-demand resolution.

use super::{BundleIdentity, PluginFormat, PluginScanReceipt, bundle_identity, scan_workers};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, OnceLock};

type Identity = (String, PluginFormat, BundleIdentity);
type Flight = Arc<OnceLock<PluginScanReceipt>>;

#[derive(Default)]
struct Active {
    requests: usize,
    helpers: usize,
    bundles: HashMap<Identity, Flight>,
}

struct Dispatch {
    active: Mutex<Active>,
    available: Condvar,
    capacity: usize,
}

static DISPATCH: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(scan_workers(usize::MAX)));

pub(super) struct Request<'a>(&'a Dispatch);
struct Permit<'a>(&'a Dispatch);

pub(super) struct Attempt {
    pub receipt: PluginScanReceipt,
    pub launched: bool,
}

impl From<PluginScanReceipt> for Attempt {
    fn from(receipt: PluginScanReceipt) -> Self {
        Self {
            receipt,
            launched: true,
        }
    }
}

impl Dispatch {
    fn new(capacity: usize) -> Self {
        Self {
            active: Mutex::new(Active::default()),
            available: Condvar::new(),
            capacity,
        }
    }

    fn active(&self) -> MutexGuard<'_, Active> {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn request(&self) -> Request<'_> {
        self.active().requests += 1;
        Request(self)
    }

    fn admit(&self) -> Permit<'_> {
        let mut active = self
            .available
            .wait_while(self.active(), |active| active.helpers == self.capacity)
            .unwrap_or_else(|error| error.into_inner());
        active.helpers += 1;
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.active().helpers -= 1;
        self.0.available.notify_one();
    }
}

pub(super) fn request() -> Request<'static> {
    DISPATCH.request()
}

impl Request<'_> {
    pub(super) fn scan(
        &self,
        path: &str,
        format: PluginFormat,
        launch: impl FnOnce() -> PluginScanReceipt,
    ) -> Attempt {
        let key = (path.to_owned(), format, bundle_identity(Path::new(path)));
        let flight = self.0.active().bundles.entry(key).or_default().clone();
        let mut launched = false;
        let receipt = flight
            .get_or_init(|| {
                let _permit = self.0.admit();
                launched = true;
                launch()
            })
            .clone();
        Attempt { receipt, launched }
    }
}

impl Drop for Request<'_> {
    fn drop(&mut self) {
        let mut active = self.0.active();
        active.requests -= 1;
        // A bulk scan publishes receipts at the end of its pass. Retain each
        // result while any overlapping request can still depend on it.
        if active.requests == 0 {
            active.bundles.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{
        ScanStatus, new_receipt, read_receipts, record_receipt, scan_candidate_with_receipts,
    };
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn overlapping_requests_share_one_attempt_until_the_last_request_finishes() {
        let dispatch = Dispatch::new(2);
        let pending_publication = dispatch.request();
        let ready = Barrier::new(4);
        let runs = AtomicUsize::new(0);
        let name = "shared.clap";
        let format = PluginFormat::Clap;
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| {
                    let request = dispatch.request();
                    ready.wait();
                    let outcome = request.scan(name, format, || {
                        runs.fetch_add(1, Ordering::Relaxed);
                        let mut receipt = new_receipt(Path::new(name), format);
                        receipt.status = ScanStatus::TimedOut;
                        receipt
                    });
                    assert_eq!(outcome.receipt.status, ScanStatus::TimedOut);
                });
            }
        });
        assert_eq!(runs.load(Ordering::Relaxed), 1);
        let known = dispatch
            .request()
            .scan(name, format, || panic!("already attempted"));
        assert!(!known.launched);
        let other = dispatch.request().scan("other.clap", format, || {
            new_receipt(Path::new("other.clap"), format)
        });
        assert!(other.launched);
        drop(pending_publication);
        assert!(
            dispatch
                .request()
                .scan(name, format, || new_receipt(Path::new(name), format))
                .launched
        );
    }

    #[test]
    fn concurrent_record_updates_preserve_a_new_quarantine() {
        let directory = oc_test_support::temp_dir("scanner", "receipt-dispatch");
        let receipt_file = directory.join("scan.json");
        let stalled = directory.join("stalled.clap");
        let barrier = Barrier::new(16);
        std::thread::scope(|threads| {
            for i in 0..16 {
                let barrier = &barrier;
                let receipt_file = &receipt_file;
                let bundle = if i == 0 {
                    stalled.clone()
                } else {
                    directory.join(format!("{i}.clap"))
                };
                threads.spawn(move || {
                    let mut receipt = new_receipt(&bundle, PluginFormat::Clap);
                    receipt.status = if i == 0 {
                        ScanStatus::TimedOut
                    } else {
                        ScanStatus::Passed
                    };
                    barrier.wait();
                    record_receipt(receipt_file, &receipt);
                });
            }
        });
        let receipts = read_receipts(&receipt_file);
        assert_eq!(receipts.len(), 16);
        assert!(
            scan_candidate_with_receipts(
                &stalled.to_string_lossy(),
                PluginFormat::Clap,
                &receipts,
                || panic!("known timeout"),
            )
            .is_none()
        );
    }
}
