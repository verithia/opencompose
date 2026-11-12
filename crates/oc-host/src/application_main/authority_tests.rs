use super::*;
use std::sync::atomic::AtomicUsize;

fn runtime(
    dispatch: impl Fn(ApplicationMainTask) -> Result<(), String> + Send + Sync + 'static,
) -> (
    Arc<OnceLock<ApplicationMainExecutor>>,
    ApplicationMainExecutor,
) {
    // Exercise the production registration/identity algorithm in a local slot.
    // The real macOS native-main check remains active at every UI admission.
    let registry = Arc::new(OnceLock::new());
    let executor =
        ApplicationMainExecutor::install_current(&registry, dispatch, Some(MainRuntime::Gui))
            .unwrap();
    (registry, executor)
}

#[test]
fn constructors_and_clap_fixture_substitution_never_grant_ui_authority() {
    let registry = OnceLock::new();
    let executor = ApplicationMainExecutor::new_current(|_| unreachable!());
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::NotRegistered)
    );
    assert!(registry.set(executor.clone()).is_ok());
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::UntrustedRuntime)
    );
    ApplicationMainExecutor::with_fixture_current(|fixture| {
        assert!(Arc::ptr_eq(
            &ApplicationMainExecutor::registered().unwrap().inner,
            &fixture.inner
        ));
        assert!(fixture.check_ui_authority().is_err());
        assert!(check_ui_authority().is_err());
    });
}

#[test]
fn duplicate_registrations_do_not_replace_the_original_identity() {
    let (registry, executor) = runtime(|_| unreachable!());
    assert_eq!(
        ApplicationMainExecutor::install_current(
            &registry,
            |_| unreachable!(),
            Some(MainRuntime::EditorHelper)
        )
        .err(),
        Some(UiAuthorityError::AlreadyRegistered)
    );
    let other_registry = Arc::clone(&registry);
    let refusal = thread::spawn(move || {
        ApplicationMainExecutor::install_current(
            &other_registry,
            |_| unreachable!(),
            Some(MainRuntime::CommandLine),
        )
        .err()
    })
    .join()
    .unwrap();
    assert_eq!(refusal, Some(UiAuthorityError::AlreadyRegistered));
    assert!(Arc::ptr_eq(&registry.get().unwrap().inner, &executor.inner));
    let unrelated = ApplicationMainExecutor::new_current(|_| unreachable!());
    assert_eq!(
        unrelated.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DifferentExecutor)
    );
}

#[test]
fn trusted_ui_admission_requires_current_thread_native_main_and_open_control() {
    let (registry, executor) = runtime(|_| unreachable!());
    #[cfg(target_os = "macos")]
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::NativeMainThreadRequired)
    );
    #[cfg(not(target_os = "macos"))]
    assert!(executor.enter_ui_in(&registry).is_ok());
    let worker_registry = Arc::clone(&registry);
    let worker_executor = executor.clone();
    assert_eq!(
        thread::spawn(move || worker_executor.enter_ui_in(&worker_registry).err())
            .join()
            .unwrap(),
        Some(UiAuthorityError::WrongThread)
    );
    executor.begin_shutdown().unwrap();
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::ShuttingDown)
    );
    // Retirement/poll ownership remains available after preparation closes.
    assert!(executor.enter().is_ok());
    executor.poll().unwrap();
}

#[test]
fn discarded_runtime_delivery_revokes_ui_without_running_foreign_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (registry, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let calls = Arc::new(AtomicUsize::new(0));
    let native_calls = Arc::clone(&calls);
    let worker_executor = executor.clone();
    thread::spawn(move || {
        assert!(
            worker_executor
                .dispatch(Box::new(move || {
                    native_calls.fetch_add(1, Ordering::Relaxed);
                }))
                .is_ok()
        )
    })
    .join()
    .unwrap();
    drop(rx.recv().unwrap());
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DeliveryFailed)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn rejected_delivery_revokes_trusted_ui_and_returns_ownership_for_retirement() {
    let (registry, executor) = runtime(|_| Err("Runtime closed".into()));
    let worker_executor = executor.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let native_calls = Arc::clone(&calls);
    let returned = thread::spawn(move || {
        worker_executor
            .dispatch(Box::new(move || {
                native_calls.fetch_add(1, Ordering::Relaxed);
            }))
            .err()
            .unwrap()
            .1
    })
    .join()
    .unwrap();
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DeliveryFailed)
    );
    assert!(!executor.accepts_preparation());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    // This fixture task owns only an integer; real native owners use quarantine.
    returned();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn an_incorrect_worker_dispatch_never_admits_trusted_ui_work() {
    let (registry, executor) = runtime(|task| {
        task();
        Ok(())
    });
    let worker_executor = executor.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let native_calls = Arc::clone(&calls);
    thread::spawn(move || {
        let job = worker_executor
            .dispatch_control(move || {
                native_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .unwrap();
        drop(job);
    })
    .join()
    .unwrap();
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DeliveryFailed)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn queued_control_checks_shutdown_before_work_and_retirement_can_still_drain() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (_, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let calls = Arc::new(AtomicUsize::new(0));
    let native_calls = Arc::clone(&calls);
    let worker_executor = executor.clone();
    let job = thread::spawn(move || {
        worker_executor
            .dispatch_control(move || {
                native_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .unwrap()
    })
    .join()
    .unwrap();
    executor.begin_shutdown().unwrap();
    rx.recv().unwrap()();
    assert!(job.wait().unwrap_err().contains("shutting down"));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    executor.poll().unwrap();
}

#[test]
#[cfg(not(target_os = "macos"))]
fn owned_dispatch_and_reentrant_ui_control_use_the_designated_runtime_thread() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (registry, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let owner = thread::current().id();
    let worker_executor = executor.clone();
    let registry_for_work = Arc::clone(&registry);
    let inner_executor = executor.clone();
    let job = thread::spawn(move || {
        worker_executor
            .dispatch_control(move || {
                let _ui = inner_executor
                    .enter_ui_in(&registry_for_work)
                    .map_err(|error| error.to_string())?;
                assert_eq!(thread::current().id(), owner);
                let reentrant_executor = inner_executor.clone();
                let registry = Arc::clone(&registry_for_work);
                inner_executor
                    .dispatch_control(move || {
                        let _ui = reentrant_executor
                            .enter_ui_in(&registry)
                            .map_err(|error| error.to_string())?;
                        Ok(thread::current().id())
                    })?
                    .wait()
            })
            .unwrap()
    })
    .join()
    .unwrap();
    rx.recv().unwrap()();
    assert_eq!(job.wait().unwrap(), owner);
}

#[test]
fn discarded_delivery_leaves_other_queued_callbacks_inert_without_foreign_cleanup() {
    struct ForeignOwner(Arc<AtomicUsize>);
    impl Drop for ForeignOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (registry, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let calls = Arc::new(AtomicUsize::new(0));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let worker_executor = executor.clone();
    let calls_for_work = Arc::clone(&calls);
    let cleanup_for_work = Arc::clone(&cleanup);
    thread::spawn(move || {
        for _ in 0..2 {
            let calls = Arc::clone(&calls_for_work);
            let owner = ForeignOwner(Arc::clone(&cleanup_for_work));
            assert!(
                worker_executor
                    .dispatch(Box::new(move || {
                        calls.fetch_add(1, Ordering::Relaxed);
                        drop(owner);
                    }))
                    .is_ok()
            );
        }
    })
    .join()
    .unwrap();
    let first = rx.recv().unwrap();
    let second = rx.recv().unwrap();
    thread::spawn(move || drop(first)).join().unwrap();
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DeliveryFailed)
    );
    second();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        cleanup.load(Ordering::Relaxed),
        0,
        "quarantined native ownership cannot clean up on the discarding worker or a late callback"
    );
}

#[test]
fn a_waiting_worker_wakes_after_discarded_delivery_without_native_work_or_cleanup() {
    struct ForeignOwner(Arc<AtomicUsize>);
    impl Drop for ForeignOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (registry, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let calls = Arc::new(AtomicUsize::new(0));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let calls_for_work = Arc::clone(&calls);
    let owner = ForeignOwner(Arc::clone(&cleanup));
    let worker_executor = executor.clone();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        let job = worker_executor
            .dispatch_control(move || {
                calls_for_work.fetch_add(1, Ordering::Relaxed);
                drop(owner);
                Ok(())
            })
            .unwrap();
        result_tx.send(job.wait()).unwrap();
    });
    let delivery = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    thread::spawn(move || drop(delivery)).join().unwrap();
    let error = result_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("failed delivery must wake the waiter despite a quarantined result sender")
        .unwrap_err();
    assert_eq!(error, UiAuthorityError::DeliveryFailed.to_string());
    waiter.join().unwrap();
    assert_eq!(
        executor.enter_ui_in(&registry).err(),
        Some(UiAuthorityError::DeliveryFailed)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(cleanup.load(Ordering::Relaxed), 0);
}

#[test]
fn a_result_abandoned_after_delivery_failure_is_retired_on_the_execution_owner() {
    struct ResultOwner(Arc<Mutex<Vec<ThreadId>>>);
    impl Drop for ResultOwner {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(thread::current().id());
        }
    }
    let owner = thread::current().id();
    let (tx, rx) = std::sync::mpsc::channel();
    let (_, executor) = runtime(move |task| tx.send(task).map_err(|_| "Closed".into()));
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let cleanup_for_work = Arc::clone(&cleanup);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker_executor = executor.clone();
    let waiter = thread::spawn(move || {
        let job = worker_executor
            .dispatch_control(move || {
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                Ok(ResultOwner(cleanup_for_work))
            })
            .unwrap();
        let error = job
            .wait()
            .err()
            .expect("runtime failure cancels the waiting recipient");
        result_tx.send(error).unwrap();
    });
    let failure_executor = executor.clone();
    let failure = thread::spawn(move || {
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        failure_executor.delivery_failed();
        let error = result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(error, UiAuthorityError::DeliveryFailed.to_string());
        release_tx.send(()).unwrap();
    });
    // The admitted work starts on its registered owner before delivery fails.
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()();
    failure.join().unwrap();
    waiter.join().unwrap();
    assert_eq!(*cleanup.lock().unwrap(), vec![owner]);
}
