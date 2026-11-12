//! Native process-main acceptance for the UI-owned host, without a plugin window.
use super::*;
use crate::application_main;
use std::time::{Duration, Instant};

/// Rust host allocator operations observed on one processing worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnedHeapCounters {
    pub allocations: usize,
    pub reallocations: usize,
    pub deallocations: usize,
}
impl OwnedHeapCounters {
    fn since(self, before: Self) -> Self {
        Self {
            allocations: self
                .allocations
                .checked_sub(before.allocations)
                .expect("monotonic allocation counter"),
            reallocations: self
                .reallocations
                .checked_sub(before.reallocations)
                .expect("monotonic reallocation counter"),
            deallocations: self
                .deallocations
                .checked_sub(before.deallocations)
                .expect("monotonic deallocation counter"),
        }
    }
}
const WARM_BLOCKS: usize = 64;
type HeapCounter = fn() -> OwnedHeapCounters;

#[derive(Debug)]
/// Coverage of the owned plugin lifecycle.
pub struct OwnedLifecycleAudit {
    pub warm_blocks_per_branch: usize,
    pub warm_instrument_heap: Option<OwnedHeapCounters>,
    pub warm_effect_buffer_heap: Option<OwnedHeapCounters>,
    pub last_external_worker_drop_heap: Option<OwnedHeapCounters>,
    pub rejected_mapped_transport_heap: Option<OwnedHeapCounters>,
    pub metadata_parameters: usize,
    pub worker_process_calls: u32,
    pub busy_refused_before_native_call: bool,
    pub worker_drop_performed_no_native_cleanup: bool,
    pub ui_violations: u32,
    pub healthy_refs_after_retirement: u32,
    pub faulted_owner_quarantined: bool,
    pub contradictory_readonly_refused_before_native_call: bool,
    pub unroutable_sidechain_refused_before_native_call: bool,
}

struct AuditFixture {
    fixture: ManuallyDrop<NativeFixture>,
    control: Option<Vst3ControlHandle>,
}
impl AuditFixture {
    fn new() -> Result<(Self, Vst3ProcessorLease, Arc<Vst3OwnedSnapshot>), Vst3OwnedError> {
        Self::with_parameter_mode(5)
    }
    fn with_parameter_mode(
        mode: u32,
    ) -> Result<(Self, Vst3ProcessorLease, Arc<Vst3OwnedSnapshot>), Vst3OwnedError> {
        let token = application_main::ui_authority()?;
        owned_lifecycle::check_fixture_capacity(&token)?;
        // The actual post-acquisition loader initializes both separate native
        // interfaces, transfers state, configures buses, and activates processing.
        let (mut fixture, loaded) = NativeFixture::load_with_fault(22, 0);
        loaded.map_err(Vst3OwnedError::Control)?;
        fixture.configure_parameters_for_test(mode);
        let instance = fixture.instance.take().expect("loaded audit instance");
        let owned = owned_lifecycle::adopt_native_fixture(&token, instance)?;
        Ok((
            Self {
                fixture: ManuallyDrop::new(fixture),
                control: Some(owned.control),
            },
            owned.processor,
            owned.snapshot,
        ))
    }
    fn control(&self) -> &Vst3ControlHandle {
        self.control.as_ref().expect("audit control is live")
    }
    fn retire(&mut self) -> Result<(), Vst3OwnedError> {
        self.control.take();
        poll_owned_vst3_retirements()?;
        Ok(())
    }
    fn hold(&self, hold: bool) {
        // SAFETY: the fake owner outlives the scoped processing worker; only
        // native atomics are accessed by this instrumentation call.
        unsafe { oc_vst3_fixture_hold_process(self.fixture.handle, hold) };
    }
    fn entered(&self) -> bool {
        // SAFETY: the live fake owner exposes an acquire atomic observation.
        unsafe { oc_vst3_fixture_process_entered(self.fixture.handle) }
    }
    fn violations(&self) -> u32 {
        // SAFETY: the explicit owner is live; this counter is atomic.
        unsafe { oc_vst3_fixture_ui_violations(self.fixture.handle) }
    }
    fn on_ui(&self, point: u32) -> bool {
        // SAFETY: a bounded operation index reads only atomic fixture metadata.
        unsafe { oc_vst3_fixture_last_call_on_ui(self.fixture.handle, point) }
    }
}
impl Drop for AuditFixture {
    fn drop(&mut self) {
        self.control.take();
        if poll_owned_vst3_retirements().is_ok() {
            // SAFETY: scoped workers and every external ticket have gone before
            // this guard. Drain removes this fake owner from the UI registry;
            // retained faulted interfaces are inert and never called again.
            unsafe { ManuallyDrop::drop(&mut self.fixture) };
        }
        // Failed UI admission retains the fake owner as well as native ownership.
    }
}
fn require(condition: bool, message: &'static str) -> Result<(), Vst3OwnedError> {
    if condition {
        Ok(())
    } else {
        Err(Vst3OwnedError::InvalidRequest(message))
    }
}
#[expect(
    clippy::result_large_err,
    reason = "native audit preserves the realtime API's inline fault result"
)]
fn block(
    processor: &mut Vst3ProcessorLease,
    effect_buffer: bool,
    transport: Option<&PluginTransport>,
) -> Result<(), Vst3ProcessLeaseError> {
    let mut output = [0.0; 128];
    let input = [0.5; 128];
    processor.process_stereo_into(Vst3OwnedProcessBlock {
        frames: 64,
        transport,
        parameters: &[Vst3NormalizedEvent {
            id: 100,
            value: 0.75,
            sample_offset: 17,
        }],
        input: effect_buffer.then_some(&input),
        sidechain: None,
        output: &mut output,
    })?;
    if output[0] != 0.25 || output[1] != 0.5 {
        return Err(Vst3ProcessLeaseError::InvalidBlock);
    }
    Ok(())
}

/// Invoke only after the executable's real UI bootstrap. Ordinary libtest
/// workers intentionally fail admission before constructing a native object.
/// This audit is CPU-only and never opens a window or an audio device.
pub fn audit_owned_ui_lifecycle() -> Result<OwnedLifecycleAudit, Vst3OwnedError> {
    audit(None)
}

/// The counter snapshots are taken on the processing worker itself, outside
/// thread startup/exit. They must monotonically count that thread's Rust heap
/// allocations, reallocations and deallocations, without allocating themselves.
pub fn audit_owned_ui_lifecycle_with_heap_counter(
    counter: HeapCounter,
) -> Result<OwnedLifecycleAudit, Vst3OwnedError> {
    audit(Some(counter))
}

fn audit(counter: Option<HeapCounter>) -> Result<OwnedLifecycleAudit, Vst3OwnedError> {
    application_main::ui_authority()?;
    let (mut fixture, mut processor, snapshot) = AuditFixture::new()?;
    require(
        snapshot.parameters.len() == 8,
        "Native parameter inventory mismatch",
    )?;
    require(
        snapshot.latency_samples == 31 && snapshot.tail_samples == 37,
        "Native processor metadata mismatch",
    )?;
    let before_sidechain_process = fixture.fixture.calls(1);
    let (returned, sidechain_refused) = std::thread::spawn(move || {
        let mut output = [0.625; 128];
        let input = [0.5; 128];
        let sidechain = [0.25; 128];
        let mut refused = true;
        for main_input in [None, Some(input.as_slice())] {
            let result = processor.process_stereo_into(Vst3OwnedProcessBlock {
                frames: 64,
                transport: None,
                parameters: &[Vst3NormalizedEvent {
                    id: 100,
                    value: 0.75,
                    sample_offset: 0,
                }],
                input: main_input,
                sidechain: Some(&sidechain),
                output: &mut output,
            });
            refused &= matches!(result, Err(Vst3ProcessLeaseError::InvalidBlock))
                && output.iter().all(|sample| *sample == 0.625);
        }
        (processor, refused)
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Sidechain admission audit worker panicked".into()))?;
    processor = returned;
    let sidechain_refused = sidechain_refused
        && fixture.fixture.calls(1) == before_sidechain_process
        && fixture
            .control()
            .capture_state_plan()
            .execute_on_ui()?
            .len()
            == 8;
    require(
        sidechain_refused,
        "Unroutable sidechain entered native processing or queued parameter changes",
    )?;
    let metadata = fixture.control().metadata_plan().execute_on_ui()?;
    require(
        metadata.parameters.len() == 8,
        "Owned metadata refresh mismatch",
    )?;
    fixture
        .control()
        .parameter_write_plan(Vst3NormalizedParameter {
            id: 100,
            value: 0.75,
        })
        .execute_on_ui()?;
    require(
        matches!(
            fixture.control().capture_state_plan().execute_on_ui(),
            Err(Vst3OwnedError::Busy)
        ),
        "Unconsumed numeric parameters must prevent stale state capture",
    )?;
    // Moving the sole clock owner onto the worker makes the warm measurement
    // catch an accidental final Arc release inside the processing API.
    let mapped_transport = PluginTransport::new(0, 48_000, 120.0).with_clock(Arc::new(
        oc_dsl::timing::ProjectClock::new(&oc_dsl::ast::ProjectProperties::default())
            .map_err(Vst3OwnedError::Control)?,
    ));
    let (returned, first_result) = std::thread::spawn(move || {
        // Warm the worker's TLS/counter and each processing branch before
        // measuring only repeated numeric processing through the owned lease.
        if let Some(read) = counter {
            let _ = read();
        }
        #[expect(
            clippy::result_large_err,
            reason = "measured native processing preserves the inline realtime fault instead of boxing it"
        )]
        let result = (|| {
            block(&mut processor, false, Some(&mapped_transport))?;
            let before = counter.map(|read| read());
            for _ in 0..WARM_BLOCKS {
                block(&mut processor, false, Some(&mapped_transport))?;
            }
            let instrument_heap = counter
                .zip(before)
                .map(|(read, before)| read().since(before));
            block(&mut processor, true, Some(&mapped_transport))?;
            let before = counter.map(|read| read());
            for _ in 0..WARM_BLOCKS {
                block(&mut processor, true, Some(&mapped_transport))?;
            }
            let effect_heap = counter
                .zip(before)
                .map(|(read, before)| read().since(before));
            let mut zero_rate = mapped_transport.clone();
            zero_rate.sample_rate = 0;
            let mut invalid_tempo = mapped_transport.clone();
            invalid_tempo.tempo = f64::NAN;
            let before = counter.map(|read| read());
            let mut output = [0.0; 128];
            for attempt in 0..WARM_BLOCKS {
                let (frames, transport) = match attempt % 3 {
                    0 => (0, &mapped_transport),
                    1 => (64, &zero_rate),
                    _ => (64, &invalid_tempo),
                };
                let refused = processor.process_stereo_into(Vst3OwnedProcessBlock {
                    frames,
                    transport: Some(transport),
                    parameters: &[],
                    input: None,
                    sidechain: None,
                    output: &mut output,
                });
                let expected = matches!((attempt % 3, &refused),
                    (0,Err(Vst3ProcessLeaseError::InvalidBlock))
                    | (1,Err(Vst3ProcessLeaseError::Clock(oc_audio::TransportBlockError::InvalidSampleRate)))
                    | (2,Err(Vst3ProcessLeaseError::Clock(oc_audio::TransportBlockError::InvalidTempo))));
                assert!(expected,"clock rejection {attempt} returned {refused:?}");
                assert!(output.iter().all(|sample| *sample == 0.0),"rejected clock changed caller output");
            }
            let rejected_heap = counter
                .zip(before)
                .map(|(read, before)| read().since(before));
            Ok::<_, Vst3ProcessLeaseError>((instrument_heap, effect_heap, rejected_heap))
        })();
        (processor, result)
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Native processing audit worker panicked".into()))?;
    let (warm_instrument_heap, warm_effect_buffer_heap, rejected_mapped_transport_heap) =
        first_result.map_err(|error| {
            Vst3OwnedError::Control(format!("Native processing audit failed: {error:?}"))
        })?;
    processor = returned;
    let state = fixture.control().capture_state_plan().execute_on_ui()?;
    require(state.len() == 8, "Owned component state capture mismatch")?;
    let metadata_calls = fixture.fixture.calls(37);
    let cleanup_calls = fixture.fixture.calls(11);
    fixture.hold(true);
    let (busy, processed) = std::thread::scope(|scope| {
        #[expect(
            clippy::result_large_err,
            reason = "the worker carries the real inline processor fault across join without heap allocation"
        )]
        let worker = scope.spawn(move || {
            let result = block(&mut processor, false, None);
            // Final processor release is only an atomic retirement request.
            drop(processor);
            result
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.entered() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let busy = fixture.entered()
            && matches!(
                fixture.control().metadata_plan().execute_on_ui(),
                Err(Vst3OwnedError::Busy)
            )
            && fixture.fixture.calls(37) == metadata_calls;
        fixture.hold(false);
        (busy, worker.join())
    });
    processed
        .map_err(|_| Vst3OwnedError::Control("Native processing audit worker panicked".into()))?
        .map_err(|error| {
            Vst3OwnedError::Control(format!("Native processing audit failed: {error:?}"))
        })?;
    require(busy, "Competing UI control must refuse before native entry")?;
    require(
        fixture.fixture.calls(11) == cleanup_calls,
        "Worker release performed native reference cleanup",
    )?;
    require(
        !fixture.on_ui(1),
        "Native processor did not run on the worker",
    )?;
    require(
        fixture.on_ui(21)
            && fixture.on_ui(22)
            && fixture.on_ui(2)
            && fixture.on_ui(37)
            && fixture.on_ui(38)
            && fixture.on_ui(41)
            && fixture.on_ui(6),
        "Native lifecycle or controller call left the UI owner",
    )?;
    // The processor ticket has retired, so further controls cannot reanimate it.
    require(
        matches!(
            fixture.control().metadata_plan().execute_on_ui(),
            Err(Vst3OwnedError::Retired)
        ),
        "Retired processor allowed further controller access",
    )?;
    let process_calls = fixture.fixture.calls(1);
    fixture.retire()?;
    require(
        fixture.on_ui(27) && fixture.on_ui(28) && fixture.on_ui(11),
        "Native shutdown or release left the UI owner",
    )?;
    require(
        fixture.fixture.refs() == 0 && fixture.violations() == 0,
        "Healthy native retirement leaked references or violated UI affinity",
    )?;

    let (mut last_external, processor, _) = AuditFixture::new()?;
    let last_releases = last_external.fixture.calls(11);
    // No external control ticket survives: processor Drop is the final
    // external Arc release, while the UI registry still retains the native owner.
    drop(last_external.control.take());
    let last_external_worker_drop_heap = std::thread::spawn(move || {
        if let Some(read) = counter {
            let _ = read();
        }
        let before = counter.map(|read| read());
        drop(processor);
        counter
            .zip(before)
            .map(|(read, before)| read().since(before))
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Native retirement audit worker panicked".into()))?;
    require(
        last_external.fixture.calls(11) == last_releases,
        "Last external worker release performed native cleanup",
    )?;
    last_external.retire()?;
    require(
        last_external.fixture.refs() == 0 && last_external.violations() == 0,
        "Last external lease did not retire on the UI owner",
    )?;

    let (mut contradictory, mut processor, contradictory_snapshot) =
        AuditFixture::with_parameter_mode(6)?;
    require(
        contradictory_snapshot.parameters.iter().any(|parameter| {
            parameter.id == 102
                && parameter.flags & ParameterFlags_::kCanAutomate != 0
                && parameter.flags & ParameterFlags_::kIsReadOnly != 0
        }),
        "Native fixture did not advertise contradictory readonly automation",
    )?;
    let before_process = contradictory.fixture.calls(1);
    let before_write = contradictory.fixture.calls(41);
    let before_state = contradictory
        .control()
        .capture_state_plan()
        .execute_on_ui()?;
    let (returned, refused, unchanged) = std::thread::spawn(move || {
        let mut output = [0.625; 128];
        // A writable event before the readonly one must not be queued if any
        // event fails preparation; the complete block is refused before FFI.
        let result = processor.process_stereo_into(Vst3OwnedProcessBlock {
            frames: 64,
            transport: None,
            parameters: &[
                Vst3NormalizedEvent {
                    id: 100,
                    value: 0.75,
                    sample_offset: 0,
                },
                Vst3NormalizedEvent {
                    id: 102,
                    value: 0.75,
                    sample_offset: 17,
                },
            ],
            input: None,
            sidechain: None,
            output: &mut output,
        });
        (
            processor,
            matches!(result, Err(Vst3ProcessLeaseError::UnpreparedParameter)),
            output.iter().all(|sample| *sample == 0.625),
        )
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Readonly automation audit worker panicked".into()))?;
    let write_refused = matches!(
        contradictory
            .control()
            .parameter_write_plan(Vst3NormalizedParameter {
                id: 102,
                value: 0.75
            })
            .execute_on_ui(),
        Err(Vst3OwnedError::InvalidRequest(_))
    );
    let readonly_refused = refused
        && unchanged
        && write_refused
        && contradictory.fixture.calls(1) == before_process
        && contradictory.fixture.calls(41) == before_write
        && contradictory
            .control()
            .capture_state_plan()
            .execute_on_ui()?
            == before_state;
    require(
        readonly_refused,
        "Contradictory readonly automation changed native state or entered FFI",
    )?;
    drop(returned);
    contradictory.retire()?;
    require(
        contradictory.fixture.refs() == 0 && contradictory.violations() == 0,
        "Readonly refusal poisoned or leaked healthy native ownership",
    )?;

    let (mut faulted, processor, _) = AuditFixture::new()?;
    faulted.fixture.set_fault(38, 1);
    require(
        matches!(faulted.control().metadata_plan().execute_on_ui(), Err(Vst3OwnedError::Native(error))
        if error.operation == Operation::ParameterInfo),
        "Native metadata fault lost its operation",
    )?;
    let releases = faulted.fixture.calls(11);
    drop(processor);
    faulted.retire()?;
    let quarantined = faulted.fixture.calls(11) == releases && faulted.fixture.refs() > 0;
    require(
        quarantined && faulted.violations() == 0,
        "Faulted native retirement reentered or released the quarantined object",
    )?;
    Ok(OwnedLifecycleAudit {
        warm_blocks_per_branch: WARM_BLOCKS,
        warm_instrument_heap,
        warm_effect_buffer_heap,
        last_external_worker_drop_heap,
        rejected_mapped_transport_heap,
        metadata_parameters: snapshot.parameters.len(),
        worker_process_calls: process_calls,
        busy_refused_before_native_call: busy,
        worker_drop_performed_no_native_cleanup: true,
        ui_violations: fixture.violations()
            + last_external.violations()
            + contradictory.violations()
            + faulted.violations(),
        healthy_refs_after_retirement: fixture.fixture.refs(),
        faulted_owner_quarantined: quarantined,
        contradictory_readonly_refused_before_native_call: readonly_refused,
        unroutable_sidechain_refused_before_native_call: sidechain_refused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_runtime_never_constructs_a_native_owned_fixture() {
        // SAFETY: a thread-local instrumentation counter has no native lifetime dependency.
        let before = unsafe { oc_vst3_fixture_thread_constructions() };
        assert!(matches!(
            audit_owned_ui_lifecycle(),
            Err(Vst3OwnedError::Authority(_))
        ));
        // SAFETY: the same thread-local counter cannot race parallel tests.
        assert_eq!(unsafe { oc_vst3_fixture_thread_constructions() }, before);
    }
}
