//! Actual-main publication and receipt lifetime proof for authored preparation.
use super::super::native_fixture::NativeFixture;
use super::preparation::{self, InitialParameters};
use super::*;
use crate::preset_parameters::ParameterWriteDomain;
use std::time::{Duration, Instant};

#[derive(Debug)]
/// Audit results for authored runtime updates.
pub struct Vst3AuthoredRuntimeAudit {
    pub published_state_before_authored: bool,
    pub failed_publications: usize,
    pub worker_drop_retired_on_ui: bool,
    pub cancelled_before_admission: bool,
    pub abandoned_delivery_retired_on_ui: bool,
}
fn row() -> Vst3AuthoredInput {
    Vst3AuthoredInput {
        name: "Gain".into(),
        native_value: Some(75.0),
        domain: ParameterWriteDomain::LegacyVst3Auto,
        external_claim: false,
    }
}
fn budget() -> Vst3ProcessingBudget {
    Vst3ProcessingBudget {
        events: 8,
        points_per_parameter: 2,
    }
}
fn plan() -> Vst3OwnedAuthoredPlan {
    Vst3OwnedAuthoredPlan {
        path: "test.fake.one_bus".into(),
        sample_rate: 48_000,
        max_frames: 512,
        component_state: None,
        parameters: vec![],
        budget: budget(),
    }
}
fn prepare_native() -> Result<(NativeFixture, preparation::PreparedCandidate), Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let (fixture, candidate) = super::authored_preparation_audit::fixture(&token)?;
    let prepared = preparation::complete_candidate(
        &token,
        candidate,
        Some(0.2f64.to_le_bytes().to_vec()),
        InitialParameters::Authored(vec![row()]),
    )?;
    Ok((fixture, prepared))
}

/// The supplied queue is the actual process-main runtime's registered owner.
/// The existing native harness pumps it; no surrogate UI authority is created.
pub fn audit_owned_authored_runtime(
    queue: &application_main::ApplicationMainQueue,
) -> Result<Vst3AuthoredRuntimeAudit, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let (native, prepared) = prepare_native()?;
    assert_eq!(
        native.controller_value_for_test(),
        0.75,
        "controller state must precede authored writes"
    );
    assert_eq!(native.calls(5), 1);
    assert_eq!(
        native.calls(41),
        1,
        "publication must not replay authored setters"
    );
    let owned = preparation::publish_runtime(&token, prepared, budget())?;
    assert_eq!(
        native.calls(41),
        1,
        "runtime publication must not replay authored setters"
    );
    assert_eq!(owned.snapshot.latency_samples, 106);
    assert_eq!(owned.snapshot.tail_samples, 37);
    assert_eq!(
        native.calls(1),
        0,
        "publication must not process warmup audio"
    );
    let output = std::thread::spawn(move || {
        let mut owned = owned;
        let mut output = [0.0; 128 * 2];
        owned
            .processor
            .process_stereo_into(Vst3OwnedProcessBlock {
                frames: 128,
                transport: None,
                parameters: &[],
                input: None,
                sidechain: None,
                output: &mut output,
            })
            .unwrap();
        drop(owned);
        output
    })
    .join()
    .expect("authored processing worker");
    assert!(output.iter().any(|sample| *sample != 0.0));
    assert!(
        native.refs() > 0,
        "worker drop must not invoke native cleanup"
    );
    poll_owned_vst3_retirements()?;
    assert_eq!(native.refs(), 0);
    assert_eq!(native.ui_violations_for_test(), 0);
    assert!(native.last_call_on_ui_for_test(11));
    drop(native);

    let mut failed_publications = 0;
    for point in [4, 38] {
        let (native, candidate) = super::authored_preparation_audit::fixture(&token)?;
        native.set_fault(point, 1);
        let failure = candidate.native.failure();
        let result = preparation::complete_candidate(
            &token,
            candidate,
            None,
            InitialParameters::Authored(vec![row()]),
        )
        .and_then(|prepared| preparation::publish_runtime(&token, prepared, budget()));
        assert!(result.is_err());
        let first = failure
            .first_fault()
            .expect("native preparation fault retained");
        assert!(matches!(
            first.operation,
            Operation::SetActive | Operation::ParameterInfo
        ));
        assert!(native.refs() > 0);
        assert_eq!(native.calls(10), 0);
        assert_eq!(native.ui_violations_for_test(), 0);
        failed_publications += 1;
    }
    // Exercise the actual public authored dispatch plan, cancelled while still queued.
    let before = registry::reserved_slots_for_audit();
    let cancelled = std::thread::spawn(|| plan().dispatch_on_ui())
        .join()
        .unwrap()?;
    drop(cancelled);
    queue.pump().map_err(Vst3OwnedError::Control)?;
    assert_eq!(registry::reserved_slots_for_audit(), before);
    // A successfully delivered public plan also retains only leases on the worker.
    let delivered = std::thread::spawn(|| plan().dispatch_on_ui())
        .join()
        .unwrap()?;
    queue.pump().map_err(Vst3OwnedError::Control)?;
    // The completed response is still inside its receipt. Dropping that receipt
    // on a worker must not destroy its native object there.
    std::thread::spawn(move || drop(delivered)).join().unwrap();
    queue.pump().map_err(Vst3OwnedError::Control)?;
    assert_eq!(registry::reserved_slots_for_audit(), before);
    let synchronous = plan().execute_on_ui()?;
    std::thread::spawn(move || drop(synchronous))
        .join()
        .unwrap();
    queue.pump().map_err(Vst3OwnedError::Control)?;
    assert_eq!(registry::reserved_slots_for_audit(), before);

    // Abandon an already admitted result while native UI work is running. The
    // receipt's existing cancellation mechanism owns delivery; no second flag exists.
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (fixture_tx, fixture_rx) = std::sync::mpsc::channel();
    let executor = queue.executor();
    let waiter = std::thread::spawn(move || {
        let job = executor
            .dispatch_control(move || {
                let token = application_main::ui_authority().map_err(|error| error.to_string())?;
                let (native, prepared) = prepare_native().map_err(|error| error.to_string())?;
                let owned = preparation::publish_runtime(&token, prepared, budget())
                    .map_err(|error| error.to_string())?;
                // Transfer the independent fixture allocation back to this same UI
                // thread after its registry owner retires; no worker dereferences it.
                let pointer = Box::into_raw(Box::new(native)) as usize;
                fixture_tx.send(pointer).unwrap();
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(owned)
            })
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(job);
        release_tx.send(()).unwrap();
    });
    // Ensure the worker enqueues before taking the bounded queue snapshot.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !waiter.is_finished() && Instant::now() < deadline {
        queue.pump().map_err(Vst3OwnedError::Control)?;
        if let Ok(pointer) = fixture_rx.try_recv() {
            waiter.join().unwrap();
            poll_owned_vst3_retirements()?;
            // SAFETY: exactly the Box allocated in the UI task above, recovered
            // on that same UI after every native processing/control ticket retires.
            let native = unsafe { Box::from_raw(pointer as *mut NativeFixture) };
            assert_eq!(native.refs(), 0);
            assert_eq!(native.ui_violations_for_test(), 0);
            assert!(native.last_call_on_ui_for_test(11));
            assert_eq!(registry::reserved_slots_for_audit(), before);
            return Ok(Vst3AuthoredRuntimeAudit {
                published_state_before_authored: true,
                failed_publications,
                worker_drop_retired_on_ui: true,
                cancelled_before_admission: true,
                abandoned_delivery_retired_on_ui: true,
            });
        }
        std::thread::yield_now();
    }
    Err(Vst3OwnedError::InvalidRequest(
        "admitted authored delivery did not produce its UI fixture",
    ))
}
