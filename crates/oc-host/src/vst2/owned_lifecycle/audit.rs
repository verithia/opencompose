//! CPU-only native fixture audit, admitted only by the real process UI bootstrap.
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Allocator counters captured around a VST2 operation.
pub struct Vst2HeapCounters {
    pub allocations: usize,
    pub reallocations: usize,
    pub deallocations: usize,
}
impl Vst2HeapCounters {
    fn since(self, before: Self) -> Self {
        Self {
            allocations: self.allocations - before.allocations,
            reallocations: self.reallocations - before.reallocations,
            deallocations: self.deallocations - before.deallocations,
        }
    }
}
#[derive(Debug)]
/// Audit results for the owned VST2 lifecycle.
pub struct Vst2OwnedAudit {
    pub process_calls: u64,
    pub worker_heap: Vst2HeapCounters,
    pub midi_validation_heap: Vst2HeapCounters,
    pub clock_validation_heap: Vst2HeapCounters,
    pub clock_refusals_preserved_state: bool,
    pub midi_refusals_preserved_state: bool,
    pub failed_publications_retained: bool,
    pub retained_publication_slots: usize,
    pub route_capacity: usize,
    pub main_entry_and_close: bool,
    pub reentrant_refused: bool,
    pub queued_cancellation_refused: bool,
    pub shutdown_retired: bool,
    pub failed_delivery_retained: bool,
}
fn same_clock(a: abi::VstTimeInfo, b: abi::VstTimeInfo) -> bool {
    a.sample_pos == b.sample_pos
        && a.sample_rate == b.sample_rate
        && a.nano_seconds == b.nano_seconds
        && a.ppq_pos == b.ppq_pos
        && a.tempo == b.tempo
        && a.bar_start_pos == b.bar_start_pos
        && a.cycle_start_pos == b.cycle_start_pos
        && a.cycle_end_pos == b.cycle_end_pos
        && a.time_sig_numerator == b.time_sig_numerator
        && a.time_sig_denominator == b.time_sig_denominator
        && a.smpte_offset == b.smpte_offset
        && a.smpte_frame_rate == b.smpte_frame_rate
        && a.samples_to_next_clock == b.samples_to_next_clock
        && a.flags == b.flags
}
fn observer(instance: &Vst2OwnedInstance) -> Arc<HostContext> {
    // SAFETY: Actual UI caller retains control+processor tickets. No worker has
    // access yet; this observer retains only host counters, not native objects.
    unsafe { &*instance.processor.link.native.as_ptr() }
        .host
        .clone()
}
fn plan(path: &Path) -> Vst2OwnedLoadPlan {
    Vst2OwnedLoadPlan {
        path: path.to_owned(),
        sample_rate: 48_000,
        max_frames: 64,
    }
}
fn pump_until_finished<T>(worker: &std::thread::JoinHandle<T>) -> Result<(), Vst2OwnedError> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !worker.is_finished() {
        if std::time::Instant::now() >= until {
            return Err(Vst2OwnedError::Control(
                "VST2 audit worker timed out".into(),
            ));
        }
        application_main::ApplicationMainExecutor::registered()
            .map_err(Vst2OwnedError::Control)?
            .poll()
            .map_err(Vst2OwnedError::Control)?;
        std::thread::yield_now();
    }
    Ok(())
}
/// Audit lifecycle safety with a heap-counter source.
pub fn audit_owned_vst2_lifecycle(
    heap: fn() -> Vst2HeapCounters,
) -> Result<Vst2OwnedAudit, Vst2OwnedError> {
    let _token = application_main::ui_authority()?;
    let main_thread = time_info::native_thread_id()
        .ok_or_else(|| Vst2OwnedError::Control("Native thread identity unavailable".into()))?;
    let path = test_support::fixture_path("OpenCompose Gain Fixture")
        .ok_or_else(|| Vst2OwnedError::Control("Controlled VST2 fixture missing".into()))?;
    let instance = plan(&path).execute_on_ui()?;
    let host = observer(&instance);
    instance
        .control
        .write_parameter_on_ui("Gain".into(), 0.75)?;
    if instance.control.capture_state_on_ui()?.is_empty() {
        return Err(Vst2OwnedError::Control(
            "Native fixture state was empty".into(),
        ));
    }
    let reentrant_refused = {
        let _phase = Phase::enter()?;
        matches!(plan(&path).execute_on_ui(), Err(Vst2OwnedError::Busy))
    };
    let Vst2OwnedInstance {
        runtime_parameters: _,
        mut processor,
        control,
        snapshot,
    } = instance;
    drop(control);
    drop(snapshot);
    let midi_observer = host.clone();
    let worker = std::thread::spawn(move || {
        let mut output = [0.25; 128];
        processor.queue_midi(0x90, 60, 100, 63)?;
        // SAFETY: This exclusive worker owns the processor, with no control
        // ticket or in-flight native operation. The UI registry retains backing.
        let initial_clock = unsafe { &*processor.link.native.as_ptr() }
            .host
            .time_info
            .snapshot();
        let mut bad_numerator = PluginTransport::new(0, 48_000, 120.0);
        bad_numerator.time_sig_numerator = 0;
        let mut bad_denominator = PluginTransport::new(0, 48_000, 120.0);
        bad_denominator.time_sig_denominator = 0;
        use oc_audio::{TransportBlockError, TransportRangeError};
        let cases = [
            (
                PluginTransport::new(0, 44_100, 120.0),
                TransportBlockError::SampleRateMismatch {
                    prepared: 48_000,
                    transport: 44_100,
                },
            ),
            (
                PluginTransport::new(0, 0, 120.0),
                TransportBlockError::InvalidSampleRate,
            ),
            (
                PluginTransport::new(0, 48_000, f64::NAN),
                TransportBlockError::InvalidTempo,
            ),
            (bad_numerator, TransportBlockError::InvalidMeter),
            (bad_denominator, TransportBlockError::InvalidMeter),
            (
                PluginTransport::new(u64::MAX, 48_000, 120.0),
                TransportBlockError::Range(TransportRangeError::SamplePositionOverflow {
                    start: u64::MAX,
                    frames: 64,
                }),
            ),
        ];
        let before_clock = heap();
        let mut clock_refusals_preserved_state = true;
        for (transport, expected) in &cases {
            clock_refusals_preserved_state &= matches!(processor.process_effect_into(&mut output, Some(transport)), Err(Vst2OwnedError::Clock(error)) if error == *expected);
            clock_refusals_preserved_state &= output == [0.25; 128];
        }
        let clock_validation_heap = heap().since(before_clock);
        // SAFETY: Rejected synchronous calls have ended without native entry;
        // this remains the sole processor owner and registry backing is retained.
        let native = unsafe { &*processor.link.native.as_ptr() };
        clock_refusals_preserved_state &= native.pending_midi.len() == 1
            && native.pending_midi[0].status == 0x90
            && native.pending_midi[0].data1 == 60
            && native.pending_midi[0].data2 == 100
            && native.pending_midi[0].offset == 63
            && native.process_error.is_none();
        clock_refusals_preserved_state &= matches!((initial_clock, native.host.time_info.snapshot()), (Some(before), Some(after)) if same_clock(before, after));
        clock_refusals_preserved_state &= midi_observer.audit_process_calls.load(Ordering::Relaxed)
            == 0
            && midi_observer.audit_event_calls.load(Ordering::Relaxed) == 0;
        let before_midi = heap();
        let mut midi_refusals_preserved_state = true;
        for (status, data1, data2, offset, expected) in [
            (0xf0, 0, 0, 0, Vst2MidiError::Status),
            (0xff, 0, 0, 0, Vst2MidiError::Status),
            (0x90, 128, 0, 0, Vst2MidiError::DataByte),
            (0x90, 0, 128, 0, Vst2MidiError::DataByte),
            (0xc0, 1, 2, 0, Vst2MidiError::UnusedDataByte),
            (0xd0, 1, 2, 0, Vst2MidiError::UnusedDataByte),
            (0x90, 0, 0, -1, Vst2MidiError::Offset),
            (0x90, 0, 0, 64, Vst2MidiError::Offset),
        ] {
            midi_refusals_preserved_state &= matches!(processor.queue_midi(status, data1, data2, offset), Err(Vst2OwnedError::Midi(error)) if error == expected);
        }
        let mut short = [0.25; 64];
        midi_refusals_preserved_state &= matches!(
            processor.process_effect_into(&mut short, None),
            Err(Vst2OwnedError::Midi(Vst2MidiError::Offset))
        );
        midi_refusals_preserved_state &= short == [0.25; 64];
        // SAFETY: This sole processor lease owns worker access, no control
        // ticket remains, and no native operation is in progress here.
        let native = unsafe { &*processor.link.native.as_ptr() };
        midi_refusals_preserved_state &= native.pending_midi.len() == 1
            && native.pending_midi[0].offset == 63
            && native.process_error.is_none();
        midi_refusals_preserved_state &= midi_observer.audit_process_calls.load(Ordering::Relaxed)
            == 0
            && midi_observer.audit_event_calls.load(Ordering::Relaxed) == 0;
        for _ in 1..MAX_PENDING_MIDI {
            processor.queue_midi(0x80, 60, 0, 0)?;
        }
        midi_refusals_preserved_state &= matches!(
            processor.queue_midi(0x90, 60, 100, 0),
            Err(Vst2OwnedError::Midi(Vst2MidiError::Capacity))
        );
        // SAFETY: Same exclusive worker scope after the synchronous queue call.
        let native = unsafe { &*processor.link.native.as_ptr() };
        midi_refusals_preserved_state &=
            native.pending_midi.len() == MAX_PENDING_MIDI && native.process_error.is_none();
        let midi_validation_heap = heap().since(before_midi);
        processor.process_effect_into(&mut output, None)?;
        let before = heap();
        for _ in 0..64 {
            output.fill(0.25);
            processor.process_effect_into(&mut output, None)?;
            if output.iter().any(|sample| (*sample - 0.375).abs() > 1e-6) {
                return Err(Vst2OwnedError::Control(
                    "Native fixture PCM mismatch".into(),
                ));
            }
        }
        drop(processor);
        Ok::<_, Vst2OwnedError>((
            heap().since(before),
            midi_validation_heap,
            midi_refusals_preserved_state,
            clock_validation_heap,
            clock_refusals_preserved_state,
        ))
    });
    pump_until_finished(&worker)?;
    let (
        worker_heap,
        midi_validation_heap,
        midi_refusals_preserved_state,
        clock_validation_heap,
        clock_refusals_preserved_state,
    ) = worker
        .join()
        .map_err(|_| Vst2OwnedError::Control("VST2 audit worker panicked".into()))??;
    poll_owned_vst2_retirements()?;
    let process_calls = host.audit_process_calls.load(Ordering::Relaxed);
    let main_entry_and_close = host.audit_entry_thread.load(Ordering::Relaxed) == main_thread
        && host.audit_close_thread.load(Ordering::Relaxed) == main_thread;

    let (before_publication, route_capacity) = callback_route::occupied_budget_for_audit();
    let mut failed_publications_retained = true;
    for _ in 0..2 {
        let token = application_main::ui_authority()?;
        let _phase = Phase::enter()?;
        let source = registry::reserve(&token)?;
        let native = Vst2Instance::load_owned(&token, &path, 48_000, 64)?;
        let unpublished_host = native.host.clone();
        // Actual native load/preparation is complete before failing the shared
        // pure-publication boundary. No registry borrow spans native calls.
        registry::refuse_publication_for_audit(&token, source, native)?;
        failed_publications_retained &= unpublished_host.audit_entry_thread.load(Ordering::Relaxed)
            == main_thread
            && unpublished_host.audit_close_thread.load(Ordering::Relaxed) == 0;
    }
    let (after_publication, _) = callback_route::occupied_budget_for_audit();
    let retained_publication_slots = after_publication.saturating_sub(before_publication);
    failed_publications_retained &=
        retained_publication_slots == 2 && registry::entries_for_audit() == 0;

    let before_cancel = registry::next_source_for_audit();
    let cancel_path = path.clone();
    let cancelled = std::thread::spawn(move || plan(&cancel_path).dispatch().map(drop));
    // dispatch is nonblocking; cancel the actual queued request before pumping.
    cancelled
        .join()
        .map_err(|_| Vst2OwnedError::Control("Cancellation worker panicked".into()))??;
    application_main::ApplicationMainExecutor::registered()
        .map_err(Vst2OwnedError::Control)?
        .poll()
        .map_err(Vst2OwnedError::Control)?;
    let queued_cancellation_refused = registry::next_source_for_audit() == before_cancel;

    let shutdown = plan(&path).execute_on_ui()?;
    let shutdown_host = observer(&shutdown);
    let retained = plan(&path).execute_on_ui()?;
    let retained_host = observer(&retained);
    let executor = application_main::registered_ui_executor()?;
    executor.begin_shutdown().map_err(Vst2OwnedError::Control)?;
    drop(shutdown);
    poll_owned_vst2_retirements()?;
    let shutdown_retired = shutdown_host.audit_close_thread.load(Ordering::Relaxed) == main_thread;
    executor.delivery_failed();
    let retired_worker = std::thread::spawn(move || drop(retained));
    retired_worker
        .join()
        .map_err(|_| Vst2OwnedError::Control("Retirement worker panicked".into()))?;
    let failed_delivery_retained = poll_owned_vst2_retirements().is_err()
        && retained_host.audit_close_thread.load(Ordering::Relaxed) == 0
        && registry::entries_for_audit() == 1;
    Ok(Vst2OwnedAudit {
        process_calls,
        worker_heap,
        midi_validation_heap,
        clock_validation_heap,
        clock_refusals_preserved_state,
        midi_refusals_preserved_state,
        failed_publications_retained,
        retained_publication_slots,
        route_capacity,
        main_entry_and_close,
        reentrant_refused,
        queued_cancellation_refused,
        shutdown_retired,
        failed_delivery_retained,
    })
}
