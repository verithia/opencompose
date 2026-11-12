#[derive(Debug)]
struct FaultLookup(std::sync::atomic::AtomicU8);
impl super::super::sessions::lookup::Io for FaultLookup {
    fn workspaces(&self, root: &Path) -> super::super::sessions::lookup::Workspaces {
        use super::super::sessions::lookup::{Disk, Error, Workspaces};
        if self.0.load(std::sync::atomic::Ordering::SeqCst) == 1 {
            Workspaces {
                keys: Vec::new(),
                error: Some(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected directory read failure",
                ))),
            }
        } else {
            Disk.workspaces(root)
        }
    }
    fn metadata(
        &self,
        path: &Path,
    ) -> Result<Option<super::super::sessions::SessionMeta>, super::super::sessions::lookup::Error>
    {
        use super::super::sessions::lookup::{Disk, Error};
        if self.0.load(std::sync::atomic::Ordering::SeqCst) == 2 {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected metadata read failure",
            )))
        } else {
            Disk.metadata(path)
        }
    }
}
#[test]
fn accounting_lookup_directory_and_metadata_io_retain_known_final_usage_until_recovery() {
    for fault in [1, 2] {
        let (_root, store, id) = fixture();
        let lookup = Arc::new(FaultLookup(std::sync::atomic::AtomicU8::new(0)));
        let lease = SessionLease::admit(
            Store::with_lookup(store.root(), lookup.clone()),
            &id,
            "lookup-retry",
            Purpose::Chat,
        )
        .unwrap();
        lookup.0.store(fault, std::sync::atomic::Ordering::SeqCst);
        let failed = lease.commit(&[attempt("known-paid")], true);
        assert!(!failed.saved);
        assert_eq!(failed.error.unwrap().retry, RetryDisposition::Io);
        assert!(ACTIVE.lock().unwrap().contains(&lease.token));
        assert!(lease.start_retry());
        assert!(!lease.retry_using(write).saved);
        lookup.0.store(0, std::sync::atomic::Ordering::SeqCst);
        let saved = lease.retry_using(write);
        assert!(saved.saved);
        assert_eq!(saved.snapshot.totals.attempts, 1);
        assert_eq!(saved.snapshot.last_turn.unwrap().model, "known-paid");
        assert!(!ACTIVE.lock().unwrap().contains(&lease.token));
    }
}
#[test]
fn accounting_unrelated_corrupt_metadata_cannot_hide_a_matching_session() {
    let (_root, store, id) = fixture();
    let bad = store.root().join("0000000000000000").join(&id);
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("meta.json"), b"{broken").unwrap();
    let lease = admit(&store, &id, "valid", Purpose::Chat);
    let saved = lease.commit(&[attempt("valid")], true);
    assert!(saved.saved);
    assert_eq!(saved.snapshot.totals.attempts, 1);
}

#[test]
fn accounting_retained_retry_admission_authority_is_bounded_at_sixty_four() {
    let mut active = HashSet::new();
    for n in 0..MAX_ACTIVE {
        admit_active(&mut active, &format!("owner{n}")).unwrap();
    }
    assert_eq!(
        admit_active(&mut active, "new-paid-owner")
            .unwrap_err()
            .code,
        AccountingCode::Busy
    );
    assert_eq!(active.len(), 64);
    active.remove("owner0");
    admit_active(&mut active, "ack-freed-owner").unwrap();
    assert_eq!(active.len(), 64);
}

use super::super::timing::Usage;
use super::*;
fn fixture() -> (tempfile::TempDir, Store, String) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let id = store.create(None, None, Some("Retry"), None).unwrap().id;
    (root, store, id)
}
fn attempt(model: &str) -> AttemptStats {
    AttemptStats {
        ordinal: 0,
        stats: TurnStats {
            model: model.into(),
            usage: Usage {
                input: 123,
                output: 45,
                ..Default::default()
            },
            ..Default::default()
        },
        usage_reported: true,
        price: None,
    }
}
fn io_failure(_: &Path, _: &Ledger) -> Result<(), AccountingError> {
    Err(AccountingError::io(std::io::Error::new(
        std::io::ErrorKind::WriteZero,
        "injected disk failure",
    )))
}
fn admit(store: &Store, id: &str, request: &str, purpose: Purpose) -> Arc<SessionLease> {
    SessionLease::admit(Store::new(store.root()), id, request, purpose).unwrap()
}
#[test]
fn accounting_final_io_retry_keeps_known_usage_frozen_until_acknowledged_once() {
    let (_root, store, id) = fixture();
    let lease = admit(&store, &id, "cancelled", Purpose::Chat);
    let mut final_attempt = attempt("original");
    final_attempt.stats.failed = true;
    let failed = lease.commit_using(&[final_attempt.clone()], true, io_failure);
    assert!(!failed.saved);
    assert!(ACTIVE.lock().unwrap().contains(&lease.token));
    assert!(lease.start_retry());
    assert!(!lease.start_retry());
    let late = lease.commit(&[attempt("wrong-late-owner")], true);
    assert_eq!(late.revision, failed.revision);
    assert!(!late.saved);
    for _ in 0..3 {
        assert!(!lease.retry_using(io_failure).saved);
    }
    let before_ack = store.load(&id).unwrap().usage.unwrap();
    assert_eq!(before_ack.pending_requests, 1);
    assert_eq!(before_ack.totals.interrupted_requests, 0);
    let ack = lease.retry_using(write);
    assert!(ack.saved);
    assert!(ack.revision > failed.revision);
    assert_eq!(ack.snapshot.totals.attempts, 1);
    assert_eq!(ack.snapshot.totals.input, 123);
    assert_eq!(ack.snapshot.totals.failed_attempts, 1);
    assert_eq!(ack.snapshot.last_turn.unwrap().model, "original");
    assert!(!ACTIVE.lock().unwrap().contains(&lease.token));
    assert_eq!(lease.retry_using(write).revision, ack.revision);
    assert_eq!(store.load(&id).unwrap().usage.unwrap().totals.attempts, 1);
}
#[test]
fn accounting_older_retry_counts_spend_without_replacing_newer_footer() {
    let (_root, store, id) = fixture();
    let old = admit(&store, &id, "old", Purpose::Chat);
    assert!(
        !old.commit_using(&[attempt("old-model")], true, io_failure)
            .saved
    );
    let newer = admit(&store, &id, "new", Purpose::Chat);
    assert!(newer.commit(&[attempt("new-model")], true).saved);
    let ack = old.retry_using(write);
    assert!(ack.saved);
    assert_eq!(ack.snapshot.totals.attempts, 2);
    assert_eq!(ack.snapshot.last_turn.unwrap().model, "new-model");
    let title = admit(&store, &id, "title", Purpose::Title);
    assert!(
        !title
            .commit_using(&[attempt("title-model")], true, io_failure)
            .saved
    );
    let titled = title.retry_using(write);
    assert!(titled.saved);
    assert_eq!(titled.snapshot.last_turn.unwrap().model, "new-model");
    assert_eq!(titled.snapshot.totals.attempts, 3);
}
#[test]
fn accounting_retry_terminal_delete_and_malformed_storage_retire_without_recreation() {
    let (_root, store, id) = fixture();
    let lease = admit(&store, &id, "deleted", Purpose::Chat);
    lease.commit_using(&[attempt("paid")], true, io_failure);
    store.delete(&id).unwrap();
    let stopped = lease.retry_using(write);
    assert!(!stopped.saved);
    assert_eq!(stopped.error.unwrap().code, AccountingCode::SessionMissing);
    assert!(!ACTIVE.lock().unwrap().contains(&lease.token));
    assert!(store.find(&id).is_err());
    let id = store
        .create(None, None, Some("Malformed"), None)
        .unwrap()
        .id;
    let lease = admit(&store, &id, "malformed", Purpose::Chat);
    lease.commit_using(&[attempt("paid")], true, io_failure);
    let (dir, _) = store.find(&id).unwrap();
    std::fs::write(dir.join(FILE), b"{broken").unwrap();
    let stopped = lease.retry_using(write);
    assert!(!stopped.saved);
    assert_eq!(stopped.error.unwrap().retry, RetryDisposition::Terminal);
    assert!(!ACTIVE.lock().unwrap().contains(&lease.token));
}
#[test]
fn accounting_retry_backoff_is_bounded_and_process_loss_recovers_honest_unknown() {
    let mut backoff = RetryBackoff::default();
    let delays: Vec<_> = (0..10).map(|_| backoff.next_delay().as_secs()).collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30, 30, 30]);
    let (_root, store, id) = fixture();
    let lease = admit(&store, &id, "lost", Purpose::Chat);
    lease.commit_using(&[attempt("paid")], true, io_failure);
    drop(lease);
    let restored = store.load(&id).unwrap().usage.unwrap();
    assert_eq!(restored.totals.attempts, 0);
    assert_eq!(restored.totals.interrupted_requests, 1);
    assert_eq!(restored.pending_requests, 0);
}
