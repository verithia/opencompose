use super::super::timing::{Cost, Usage};
use super::*;

fn fixture() -> (tempfile::TempDir, Store, String) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let id = store
        .create(None, None, Some("Accounting"), None)
        .unwrap()
        .id;
    (root, store, id)
}
fn owner(store: &Store, id: &str, request: &str) -> Arc<SessionLease> {
    SessionLease::admit(Store::new(store.root()), id, request, Purpose::Chat).unwrap()
}
fn attempt(ordinal: u32, reported: bool) -> AttemptStats {
    AttemptStats {
        ordinal,
        stats: TurnStats {
            model: "test-model".into(),
            wire: "test".into(),
            request_id: Some("repeated-provider-id".into()),
            usage: Usage {
                input: 100,
                cached_input: Some(40),
                output: 20,
                reasoning: 10,
            },
            cost: Some(Cost {
                amount: 0.25,
                currency: "USD".into(),
            }),
            ..Default::default()
        },
        usage_reported: reported,
        price: Some(Price {
            input: 2.0,
            output: 3.0,
            cached_input: Some(1.0),
            currency: "USD".into(),
        }),
    }
}
fn snapshot(store: &Store, id: &str) -> SessionUsageSnapshot {
    store.load(id).unwrap().usage.unwrap()
}

#[test]
fn accounting_queued_preparation_cannot_mutate_a_replaced_session_incarnation() {
    use super::super::sessions::{PrepareContext, ProjectFacts};

    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "queued-preparation");
    let admitted = lease.update().incarnation;
    let (worker, before, replacement) = store
        .with_usage(&id, |dir| {
            let worker_store = Store::new(store.root());
            let worker_id = id.clone();
            let (queued, ready) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                queued.send(()).unwrap();
                // The store lock is still held by the replacement below.
                worker_store.prepare_owned(
                    &worker_id,
                    &[serde_json::json!({"role":"user", "content":"Old queued request"})],
                    Some(Path::new("old-project.ocp")),
                    PrepareContext {
                        overhead: 0,
                        context_length: 1,
                        facts: &ProjectFacts::default(),
                        provider: "old-provider",
                        model: "old-model",
                    },
                    Some(&admitted),
                )
            });
            ready.recv().unwrap();
            let replacement = Ledger::new(false)?;
            write(dir, &replacement)?;
            let before: BTreeMap<_, _> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect();
            Ok((worker, before, replacement.incarnation))
        })
        .unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "Session incarnation changed"
    );
    let (dir, _) = store.find(&id).unwrap();
    let after: BTreeMap<_, _> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), std::fs::read(entry.path()).unwrap())
        })
        .collect();
    assert_eq!(
        after, before,
        "No transcript, compaction, or metadata writes"
    );
    assert!(store.load(&id).unwrap().messages.is_empty());

    let prepared = store
        .prepare_owned(
            &id,
            &[serde_json::json!({"role":"user", "content":"Current request"})],
            None,
            PrepareContext {
                overhead: 0,
                context_length: 100_000,
                facts: &ProjectFacts::default(),
                provider: "current-provider",
                model: "current-model",
            },
            Some(&replacement),
        )
        .unwrap();
    assert_eq!(prepared.appended, 1);
    assert_eq!(prepared.messages[0]["content"], "Current request");
    assert_eq!(prepared.meta.provider, "current-provider");
}

#[test]
fn accounting_reopen_replay_retry_and_reused_client_id_are_exact() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "local");
    let mut first = attempt(0, true);
    first.stats.failed = true;
    let second = attempt(1, true);
    assert!(lease.commit(&[first.clone()], false).saved);
    assert!(lease.commit(&[first.clone()], false).saved);
    let finished = lease.commit(&[first, second], true);
    assert_eq!(finished.snapshot.totals.attempts, 2);
    assert_eq!(finished.snapshot.totals.failed_attempts, 1);
    assert_eq!(finished.snapshot.totals.input, 200);
    assert_eq!(finished.snapshot.totals.cached_input, 80);
    assert_eq!(finished.snapshot.totals.cache_input, 200);
    assert_eq!(finished.snapshot.totals.costs["USD"].amount, 0.5);
    assert_eq!(finished.snapshot.pending_requests, 0);
    assert_eq!(snapshot(&store, &id), finished.snapshot);
    let newer = owner(&store, &id, "local");
    assert_eq!(lease.commit(&[], true).revision, finished.revision);
    assert!(newer.commit(&[attempt(0, true)], true).saved);
    assert_eq!(snapshot(&store, &id).totals.attempts, 3);
}

#[test]
fn accounting_missing_usage_is_distinct_from_reported_zero_and_title_keeps_footer() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "chat");
    let mut missing = attempt(0, false);
    missing.stats.usage = Usage::default();
    missing.stats.cost = None;
    let mut zero = missing.clone();
    zero.ordinal = 1;
    zero.usage_reported = true;
    let final_chat = lease.commit(&[missing, zero], true);
    assert_eq!(final_chat.snapshot.totals.unreported_usage_attempts, 1);
    let title =
        SessionLease::admit(Store::new(store.root()), &id, "title", Purpose::Title).unwrap();
    let titled = title.commit(&[attempt(0, true)], true);
    assert_eq!(titled.snapshot.totals.requests, 2);
    assert_eq!(titled.snapshot.last_turn, final_chat.snapshot.last_turn);
    assert_eq!(titled.snapshot.totals.input, 100);
}

#[test]
fn accounting_orphan_recovers_once_without_losing_checkpointed_spend() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "orphan");
    lease.commit(&[attempt(0, true)], false);
    drop(lease);
    let recovered = snapshot(&store, &id);
    assert_eq!(recovered.pending_requests, 0);
    assert_eq!(recovered.totals.interrupted_requests, 1);
    assert_eq!(recovered.totals.attempts, 1);
    assert_eq!(snapshot(&store, &id), recovered);
}

#[test]
fn accounting_failed_publish_preserves_reply_last_durable_revision_and_recovery() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "paid");
    let before = lease.update();
    let failed = lease.commit_using(&[attempt(0, true)], true, |_, _| {
        Err(AccountingError::storage("injected write failure"))
    });
    assert!(!failed.saved);
    assert_eq!(failed.revision, before.revision);
    let mut reply = serde_json::json!({"role":"assistant", "content":"Paid answer"});
    annotate_reply(&mut reply, &failed);
    assert_eq!(reply["content"], "Paid answer");
    assert_eq!(
        reply["accounting_status"]["error"]["code"],
        "accounting_storage"
    );
    assert_eq!(snapshot(&store, &id).totals.interrupted_requests, 1);
    assert!(!lease.commit(&[attempt(0, true)], true).saved);
}

#[test]
fn accounting_delete_and_reincarnation_reject_late_owners_without_resurrection() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "late");
    store
        .with_usage(&id, |dir| write(dir, &Ledger::new(false)?))
        .unwrap();
    let changed = lease.commit(&[attempt(0, true)], true);
    assert_eq!(changed.error.unwrap().code, AccountingCode::SessionChanged);
    assert_eq!(snapshot(&store, &id).totals.attempts, 0);
    assert!(
        store
            .rename_owned(&id, "Stale title", Some(&lease.update().incarnation))
            .is_err()
    );
    assert!(
        store
            .record_reply_owned(
                &id,
                &serde_json::json!({"role":"assistant","content":"Stale reply"}),
                &super::super::sessions::ProjectFacts::default(),
                ("test", "model"),
                Some(&lease.update().incarnation)
            )
            .is_err()
    );
    assert!(store.load(&id).unwrap().messages.is_empty());
    let deleted = owner(&store, &id, "deleted");
    store.delete(&id).unwrap();
    assert!(!deleted.commit(&[attempt(0, true)], true).saved);
    assert!(store.find(&id).is_err());
}

#[test]
fn accounting_rewind_and_fork_do_not_rewrite_incurred_spend() {
    let (_root, store, id) = fixture();
    owner(&store, &id, "spent").commit(&[attempt(0, true)], true);
    let before = snapshot(&store, &id);
    let facts = super::super::sessions::ProjectFacts::default();
    store
        .record_reply(
            &id,
            &serde_json::json!({"role":"assistant","content":"First"}),
            &facts,
            "test",
            "model",
        )
        .unwrap();
    store
        .record_reply(
            &id,
            &serde_json::json!({"role":"assistant","content":"Second"}),
            &facts,
            "test",
            "model",
        )
        .unwrap();
    store.rewind(&id, 1).unwrap();
    assert_eq!(snapshot(&store, &id), before);
    let fork = store
        .create(
            None,
            None,
            Some("Fork"),
            Some(super::super::sessions::Fork {
                parent: &id,
                through: None,
            }),
        )
        .unwrap();
    let fresh = snapshot(&store, &fork.id);
    assert_eq!(fresh.totals.attempts, 0);
    assert_ne!(fresh.incarnation, before.incarnation);
}

#[test]
fn accounting_currency_reservations_and_attempt_budget_refuse_before_dispatch() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "bounded");
    for n in 0..MAX_CURRENCIES {
        let currency = if n == 0 {
            "USD".to_string()
        } else {
            format!(
                "A{}{}",
                (b'A' + (n / 26) as u8) as char,
                (b'A' + (n % 26) as u8) as char
            )
        };
        lease
            .reserve_price(Some(&Price {
                input: 1.0,
                output: 1.0,
                cached_input: None,
                currency,
            }))
            .unwrap();
    }
    assert_eq!(
        lease
            .reserve_price(Some(&Price {
                input: 1.0,
                output: 1.0,
                cached_input: None,
                currency: "ZZZ".into()
            }))
            .unwrap_err()
            .code,
        AccountingCode::CurrencyCapacity
    );
    assert_eq!(snapshot(&store, &id).totals.attempts, 0);
    let attempts: Vec<_> = (0..MAX_ATTEMPTS).map(|n| attempt(n as u32, true)).collect();
    assert!(lease.commit(&attempts, false).saved);
    assert_eq!(
        lease.reserve_price(None).unwrap_err().code,
        AccountingCode::AttemptCapacity
    );
    assert_eq!(snapshot(&store, &id).totals.attempts, 4);
}

#[test]
fn accounting_active_receipts_are_bounded_and_retired_after_completion() {
    let (_root, store, id) = fixture();
    let leases: Vec<_> = (0..MAX_RECEIPTS)
        .map(|n| owner(&store, &id, &format!("r{n}")))
        .collect();
    assert_eq!(
        SessionLease::admit(Store::new(store.root()), &id, "overflow", Purpose::Chat)
            .unwrap_err()
            .code,
        AccountingCode::Busy
    );
    for lease in leases {
        assert!(lease.commit(&[], true).saved);
    }
    for n in 0..20 {
        assert!(
            owner(&store, &id, &format!("done{n}"))
                .commit(&[], true)
                .saved
        );
    }
    assert_eq!(snapshot(&store, &id).pending_requests, 0);
}

#[test]
fn accounting_corrupt_oversized_and_nonregular_storage_cannot_admit_paid_work() {
    let (_root, store, id) = fixture();
    let (dir, _) = store.find(&id).unwrap();
    std::fs::write(dir.join(FILE), b"{broken").unwrap();
    assert!(store.load(&id).unwrap().usage_error.is_some());
    assert!(SessionLease::admit(Store::new(store.root()), &id, "bad", Purpose::Chat).is_err());
    std::fs::write(dir.join(FILE), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
    assert_eq!(read(&dir).unwrap_err().code, AccountingCode::Format);
    let oversized = store.load(&id).unwrap().usage_error.unwrap();
    assert_eq!(oversized.code, AccountingCode::Format);
    assert_eq!(
        SessionLease::admit(Store::new(store.root()), &id, "oversized", Purpose::Chat)
            .unwrap_err()
            .code,
        AccountingCode::Format
    );
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::fs::create_dir(dir.join(FILE)).unwrap();
    assert_eq!(read(&dir).unwrap_err().code, AccountingCode::Storage);
    let unreadable = store.load(&id).unwrap().usage_error.unwrap();
    assert_eq!(unreadable.code, AccountingCode::Storage);
    assert_eq!(
        SessionLease::admit(Store::new(store.root()), &id, "directory", Purpose::Chat)
            .unwrap_err()
            .code,
        AccountingCode::Storage
    );
}

#[test]
fn accounting_final_payload_limits_are_not_storage_or_receipt_failures() {
    let (_root, store, id) = fixture();
    let excessive = owner(&store, &id, "too-many-attempts");
    let attempts: Vec<_> = (0..=MAX_ATTEMPTS)
        .map(|n| attempt(n as u32, true))
        .collect();
    let refused = excessive.commit(&attempts, true);
    assert!(!refused.saved);
    assert_eq!(refused.error.unwrap().code, AccountingCode::AttemptCapacity);
    assert_eq!(refused.snapshot.totals.attempts, 0);

    let malformed = owner(&store, &id, "invalid-attempt");
    let mut invalid = attempt(0, true);
    invalid.stats.model = "x".repeat(513);
    let refused = malformed.commit(&[invalid], true);
    assert!(!refused.saved);
    assert_eq!(refused.error.unwrap().code, AccountingCode::Request);
    assert_eq!(refused.snapshot.totals.attempts, 0);
}

#[test]
fn accounting_unknown_price_and_independent_currencies_are_not_converted() {
    let (_root, store, id) = fixture();
    let lease = owner(&store, &id, "prices");
    let mut unknown = attempt(0, true);
    unknown.price = None;
    unknown.stats.cost = None;
    let mut free = attempt(1, true);
    free.stats.cost.as_mut().unwrap().amount = 0.0;
    let mut euro = attempt(2, true);
    euro.stats.cost.as_mut().unwrap().currency = "EUR".into();
    let update = lease.commit(&[unknown, free, euro], true);
    assert!(update.saved);
    assert_eq!(update.snapshot.totals.unknown_price_attempts, 1);
    assert_eq!(update.snapshot.totals.costs["USD"].amount, 0.0);
    assert_eq!(update.snapshot.totals.costs["EUR"].amount, 0.25);
    let wire = serde_json::to_value(update).unwrap();
    assert!(wire["snapshot"]["last_turn"].get("accounting").is_none());
}

#[cfg(windows)]
#[test]
fn accounting_windows_nonregular_handles_are_terminal_without_hiding_regular_files() {
    use super::super::sessions::lookup::Io;
    let (_root, store, id) = fixture();
    let (dir, _) = store.find(&id).unwrap();
    let original = std::fs::read(dir.join(FILE)).unwrap();
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::fs::create_dir(dir.join(FILE)).unwrap();
    let error = read(&dir).unwrap_err();
    assert_eq!(error.code, AccountingCode::Storage);
    assert_eq!(error.retry, RetryDisposition::Terminal);
    std::fs::remove_dir(dir.join(FILE)).unwrap();
    std::fs::write(dir.join(FILE), original).unwrap();
    assert!(read(&dir).unwrap().is_some());

    let metadata_path = dir.join("meta.json");
    let original = std::fs::read(&metadata_path).unwrap();
    std::fs::remove_file(&metadata_path).unwrap();
    std::fs::create_dir(&metadata_path).unwrap();
    let error = super::super::sessions::lookup::Disk
        .metadata(&metadata_path)
        .unwrap_err();
    assert!(matches!(
        error,
        super::super::sessions::lookup::Error::Corrupt(_)
    ));
    std::fs::remove_dir(&metadata_path).unwrap();
    std::fs::write(&metadata_path, original).unwrap();
    assert!(store.find(&id).is_ok());
}
