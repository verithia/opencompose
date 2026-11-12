//! Prove a warm VST3 effect block does not allocate.
//!
//! This binary owns a counting global allocator, so it is declared with
//! `harness = false` and its own `[[test]]` target. The fake component's
//! `process` reads its input buses without allocating when
//! `track_input_peaks` is off, so every allocation counted here belongs to the
//! host's process path.

use oc_host::vst3_instance::native_fixture::OwnedHeapCounters;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static HEAP: Cell<OwnedHeapCounters> = const { Cell::new(OwnedHeapCounters {
        allocations: 0, reallocations: 0, deallocations: 0,
    }) };
}
enum HeapOperation {
    Allocate,
    Reallocate,
    Deallocate,
}
fn record_heap(operation: HeapOperation) {
    // Allocator teardown can run after TLS destruction. No fixture observation
    // spans thread exit, and recording never allocates or retains a pointer.
    let _ = HEAP.try_with(|counter| {
        let mut value = counter.get();
        match operation {
            HeapOperation::Allocate => value.allocations += 1,
            HeapOperation::Reallocate => value.reallocations += 1,
            HeapOperation::Deallocate => value.deallocations += 1,
        }
        counter.set(value);
    });
}
fn heap_snapshot() -> OwnedHeapCounters {
    HEAP.with(Cell::get)
}

fn verify_heap_counter() {
    let before = heap_snapshot();
    let mut bytes = Vec::<u8>::with_capacity(1);
    bytes.push(1);
    std::hint::black_box(&mut bytes).reserve_exact(1024);
    drop(std::hint::black_box(bytes));
    let after = heap_snapshot();
    assert!(after.allocations > before.allocations);
    assert!(after.reallocations > before.reallocations);
    assert!(after.deallocations > before.deallocations);
}

// SAFETY: All allocation pointers, layouts and sizes are forwarded unchanged to System; observation does not change allocator ownership or returned storage.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        record_heap(HeapOperation::Allocate);
        // SAFETY: The GlobalAlloc caller supplies a valid allocation layout, which is forwarded unchanged to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        record_heap(HeapOperation::Allocate);
        // SAFETY: the caller's allocation layout is forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_heap(HeapOperation::Deallocate);
        // SAFETY: The caller returns a live System allocation with its original layout; the observer neither accesses nor takes ownership of its bytes.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        record_heap(HeapOperation::Reallocate);
        // SAFETY: The caller supplies a live System allocation and matching layout, with a valid new size; System retains the standard realloc ownership contract.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    if std::env::args().any(|argument| argument == "--owned-vst2-native-guards") {
        let executable = std::env::current_exe().expect("existing native harness path");
        for case in oc_host::vst2::VST2_NATIVE_GUARD_CASES {
            let status = std::process::Command::new(&executable)
                .arg(format!("--owned-vst2-native-case={case}"))
                .status()
                .expect("native guard case child");
            assert!(status.success(), "owned VST2 native guard case {case}");
        }
        let status = std::process::Command::new(executable)
            .arg("--owned-vst2-lifecycle")
            .status()
            .expect("existing lifecycle audit child");
        assert!(
            status.success(),
            "owned VST2 existing lifecycle regressions"
        );
        return;
    }
    if let Some(case) = std::env::args().find_map(|argument| {
        argument
            .strip_prefix("--owned-vst2-native-case=")
            .map(str::to_owned)
    }) {
        // SAFETY: This literal process-main bootstrap owns and pumps the one
        // application executor; the child is isolated only for module fault state.
        let _queue = unsafe {
            oc_host::application_main::ApplicationMainQueue::register_runtime_current(
                oc_host::application_main::MainRuntime::CommandLine,
            )
        }
        .expect("actual main bootstrap");
        verify_heap_counter();
        fn vst2_heap() -> oc_host::vst2::Vst2HeapCounters {
            let value = heap_snapshot();
            oc_host::vst2::Vst2HeapCounters {
                allocations: value.allocations,
                reallocations: value.reallocations,
                deallocations: value.deallocations,
            }
        }
        let audit = oc_host::vst2::audit_owned_vst2_native_case(&case, vst2_heap)
            .expect("actual C++ owned VST2 native guard case");
        assert!(audit.exact_first_cause, "{case}: exact first cause");
        assert!(audit.first_report_once, "{case}: first cause reported once");
        assert!(
            audit.no_later_native_calls,
            "{case}: no native continuation"
        );
        assert!(audit.payload_retained, "{case}: full payload retention");
        assert!(audit.loans_retained, "{case}: retained argument loans");
        assert!(audit.slot_isolation, "{case}: callback slot identity");
        assert_eq!(
            audit.process_heap,
            oc_host::vst2::Vst2HeapCounters::default()
        );
        println!("owned VST2 native guard: {case} passed");
        return;
    }
    if std::env::args().any(|argument| argument == "--owned-vst2-lifecycle") {
        // SAFETY: This literal native executable owns process main and services
        // the existing application queue; no fixture substitutes UI authority.
        let _queue = unsafe {
            oc_host::application_main::ApplicationMainQueue::register_runtime_current(
                oc_host::application_main::MainRuntime::CommandLine,
            )
        }
        .expect("actual main bootstrap");
        verify_heap_counter();
        fn vst2_heap() -> oc_host::vst2::Vst2HeapCounters {
            let value = heap_snapshot();
            oc_host::vst2::Vst2HeapCounters {
                allocations: value.allocations,
                reallocations: value.reallocations,
                deallocations: value.deallocations,
            }
        }
        let audit =
            oc_host::vst2::audit_owned_vst2_lifecycle(vst2_heap).expect("owned VST2 lifecycle");
        assert_eq!(audit.process_calls, 65);
        assert_eq!(
            audit.worker_heap,
            oc_host::vst2::Vst2HeapCounters::default()
        );
        assert_eq!(
            audit.midi_validation_heap,
            oc_host::vst2::Vst2HeapCounters::default()
        );
        assert!(audit.midi_refusals_preserved_state);
        assert_eq!(
            audit.clock_validation_heap,
            oc_host::vst2::Vst2HeapCounters::default()
        );
        assert!(audit.clock_refusals_preserved_state);
        assert!(audit.failed_publications_retained);
        assert_eq!(audit.retained_publication_slots, 2);
        assert_eq!(audit.route_capacity, 1024);
        assert!(audit.main_entry_and_close);
        assert!(audit.reentrant_refused);
        assert!(audit.queued_cancellation_refused);
        assert!(audit.shutdown_retired);
        assert!(audit.failed_delivery_retained);
        println!(
            "owned VST2: 65 native blocks, zero worker heap, UI entry/close, reentrant/cancel/shutdown/retention pass"
        );
        return;
    }
    let tuning = oc_host::vst3_instance::native_fixture::audit_tuning_payload()
        .expect("native tuning normalization and note identity");
    assert_eq!(tuning.physical_cases, 5);
    assert_eq!(tuning.normalized_cases, 3);

    let factory_owner = oc_host::vst3_instance::native_fixture::audit_factory_owner()
        .expect("actual checked factory-owner audit");
    assert_eq!(factory_owner.metadata_cases, 13);
    assert_eq!(factory_owner.creation_cases, 10);
    assert!(factory_owner.quarantined_partial_output);

    let factory_audit = oc_host::vst3_instance::native_fixture::audit_factory_boundary()
        .expect("native factory and module-entry guard audit");
    assert_eq!(factory_audit.guarded_throw_cases, 30);
    assert_eq!(factory_audit.guarded_refusal_cases, 8);
    assert!(factory_audit.partial_reference_retained);

    if std::env::args().any(|argument| argument == "--owned-lifecycle") {
        // SAFETY: this literal test executable owns the process main thread;
        // the CPU-only audit services its own UI work before this queue drains.
        let queue = unsafe {
            oc_host::application_main::ApplicationMainQueue::register_runtime_current(
                oc_host::application_main::MainRuntime::CommandLine,
            )
        }
        .expect("register the actual test executable main thread");
        verify_heap_counter();
        let audit =
            oc_host::vst3_instance::native_fixture::audit_owned_ui_lifecycle_with_heap_counter(
                heap_snapshot,
            )
            .expect("native owned lifecycle audit");
        assert_eq!(audit.metadata_parameters, 8);
        assert_eq!(audit.worker_process_calls, 131);
        assert_eq!(audit.warm_blocks_per_branch, 64);
        assert_eq!(
            audit.warm_instrument_heap,
            Some(OwnedHeapCounters::default())
        );
        assert_eq!(
            audit.warm_effect_buffer_heap,
            Some(OwnedHeapCounters::default())
        );
        assert_eq!(
            audit.last_external_worker_drop_heap,
            Some(OwnedHeapCounters::default())
        );
        assert_eq!(
            audit.rejected_mapped_transport_heap,
            Some(OwnedHeapCounters::default())
        );
        assert!(audit.busy_refused_before_native_call);
        assert!(audit.worker_drop_performed_no_native_cleanup);
        assert_eq!(audit.ui_violations, 0);
        assert_eq!(audit.healthy_refs_after_retirement, 0);
        assert!(audit.faulted_owner_quarantined);
        assert!(audit.contradictory_readonly_refused_before_native_call);
        assert!(audit.unroutable_sidechain_refused_before_native_call);
        let activation = oc_host::vst3_instance::audit_checked_owned_activation()
            .expect("checked owned activation audit");
        assert_eq!(activation.ordinary_refusals, 2);
        assert_eq!(activation.exceptions_quarantined, 2);
        assert!(activation.successful_activation_and_retirement);
        println!("{activation:?}");
        let preparation = oc_host::vst3_instance::audit_owned_initial_preparation()
            .expect("owned initial metadata preparation audit");
        assert_eq!(preparation.latency_samples, 106);
        assert_eq!(preparation.tail_samples, 287);
        assert!(preparation.descriptor_change_refused_before_later_write);
        assert!(preparation.generation_change_refused_before_later_write);
        assert_eq!(preparation.invalid_metadata_refusals, 2);
        assert_eq!(preparation.layout_change_refusals, 2);
        assert!(preparation.teardown_fault_keeps_reservation);
        assert!(preparation.oversized_refused_before_metadata);
        println!("{preparation:?}");
        let authored = oc_host::vst3_instance::audit_owned_authored_preparation()
            .expect("complete authored preparation and probe retirement");
        assert_eq!(authored.tail_samples, 287);
        assert_eq!(authored.latency_samples, 106);
        assert_eq!(authored.typed_refusals_before_write, 6);
        assert!(authored.changed_inventory_stopped_later_write);
        assert!(authored.cleanup_failure_refused_success);
        assert!(authored.synthetic_probe_used_actual_ui);
        println!("{authored:?}");
        let admission =
            oc_host::vst3_instance::native_fixture::audit_owned_admission_with_heap_counter(
                heap_snapshot,
            )
            .expect("bounded owned parameter admission audit");
        println!("{admission:?}");
        let packing =
            oc_host::vst3_instance::native_fixture::audit_owned_queue_packing_with_heap_counter(
                heap_snapshot,
            )
            .expect("full maximum parameter queue packing audit");
        assert_eq!(packing.maximum_distinct.events, 65536);
        assert_eq!(packing.maximum_distinct.native_points, 65536);
        assert_eq!(packing.maximum_repeated.events, 1024);
        assert_eq!(packing.maximum_repeated.native_points, 1024);
        assert!(packing.maximum_distinct.pcm_and_payload_verified);
        assert!(packing.maximum_repeated.pcm_and_payload_verified);
        println!("{packing:?}");
        let midi = oc_host::vst3_instance::audit_owned_midi_ingress(heap_snapshot)
            .expect("bounded owned MIDI ingress and actual native payloads");
        assert_eq!(midi.native_event_payloads, 9);
        assert_eq!(midi.native_parameter_points, 6);
        assert_eq!(midi.atomic_refusals, 20);
        assert_eq!(midi.metadata_refusals, 11);
        assert_eq!(midi.active_note_capacity, 2048);
        assert_eq!(midi.heap_operations, OwnedHeapCounters::default());
        println!("{midi:?}");
        let authored_runtime = oc_host::vst3_instance::audit_owned_authored_runtime(&queue)
            .expect("authored runtime publication and delivery ownership audit");
        assert!(authored_runtime.published_state_before_authored);
        assert_eq!(authored_runtime.failed_publications, 2);
        assert!(authored_runtime.worker_drop_retired_on_ui);
        assert!(authored_runtime.cancelled_before_admission);
        assert!(authored_runtime.abandoned_delivery_retired_on_ui);
        println!("{authored_runtime:?}");
        queue
            .executor()
            .begin_shutdown()
            .expect("close UI admission");
        assert!(
            oc_host::vst3_instance::Vst3OwnedAuthoredPlan {
                path: "test.fake.one_bus".into(),
                sample_rate: 48_000,
                max_frames: 512,
                component_state: None,
                parameters: vec![],
                budget: oc_host::vst3_instance::Vst3ProcessingBudget::default(),
            }
            .dispatch_on_ui()
            .is_err(),
            "shutdown must reject authored work before native admission"
        );

        queue.pump().expect("drain test executable main queue");
        println!("{audit:?}");
        return;
    }

    let (mut instance, control) =
        oc_host::vst3_instance::test_support::fake_instance_with_buses(1, 2, 512);
    control.track_input_peaks.store(false, Ordering::Relaxed);

    let mut buffer = vec![0.25f32; 512 * 2];
    // The first block may build the reusable process queues.
    instance.process_effect(&mut buffer, 48_000);
    assert!(
        buffer.iter().any(|sample| *sample != 0.0),
        "the fake effect did not render"
    );

    let mut smaller = vec![0.25f32; 256 * 2];
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..64 {
        instance.process_effect(&mut buffer, 48_000);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(
        after,
        before,
        "a warm VST3 effect block allocated {} times over 64 blocks",
        after - before
    );

    // A smaller block keeps the same shape and must not rebuild anything.
    instance.process_effect(&mut smaller, 48_000);
    let after_small = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(
        after_small, after,
        "a smaller block with the same bus shape reallocated"
    );

    // The instrument path reuses the same event and parameter queues.
    let (mut instrument, instrument_control) =
        oc_host::vst3_instance::test_support::fake_instance_with_buses(1, 0, 512);
    instrument_control
        .track_input_peaks
        .store(false, Ordering::Relaxed);
    let mut outputs = vec![Vec::with_capacity(512 * 2)];
    instrument
        .process_block_outputs_into(512, None, &[0], &mut outputs)
        .expect("first instrument block");
    let instrument_before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..64 {
        outputs[0].clear();
        instrument
            .process_block_outputs_into(512, None, &[0], &mut outputs)
            .expect("warm instrument block");
    }
    let instrument_after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(
        instrument_after,
        instrument_before,
        "a warm VST3 instrument block allocated {} times over 64 blocks",
        instrument_after - instrument_before
    );

    // An actual optimized native COM plugin throws from its own process slot.
    // Only Rust host allocations are counted; native exception runtime storage
    // belongs to the plugin/C++ runtime. Error formatting stays off this path.
    for effect in [false, true] {
        let mut fixture = oc_host::vst3_instance::native_fixture::NativeFixture::new(effect);
        let mut audio = vec![0.25; 1024];
        let mut native_outputs = [Vec::with_capacity(1024)];
        if effect {
            fixture.instance_mut().process_effect(&mut audio, 48_000);
        } else {
            fixture
                .instance_mut()
                .process_block_outputs_into(512, None, &[0], &mut native_outputs)
                .unwrap();
        }
        fixture.set_fault(1, 1);
        let fault_before = ALLOCATIONS.load(Ordering::Relaxed);
        for _ in 0..64 {
            if effect {
                fixture.instance_mut().process_effect(&mut audio, 48_000);
            } else {
                native_outputs[0].clear();
                assert!(
                    fixture
                        .instance_mut()
                        .process_block_outputs_into(512, None, &[0], &mut native_outputs)
                        .is_err()
                );
            }
        }
        let fault_after = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(
            fault_before, fault_after,
            "native fault/quarantine allocated host memory (effect={effect})"
        );
        assert_eq!(
            fixture.calls(1),
            2,
            "quarantine must not re-enter native process"
        );
        if effect {
            assert!(audio.iter().all(|value| *value == 0.0));
        } else {
            assert!(native_outputs[0].iter().all(|value| *value == 0.0));
        }
    }

    // Latency/tail metadata can be queried by audio-facing engine paths too.
    // A native metadata throw publishes its bounded cause without formatting.
    for point in [12, 13] {
        let fixture = oc_host::vst3_instance::native_fixture::NativeFixture::new(false);
        fixture.set_fault(point, 1);
        let before_metadata = ALLOCATIONS.load(Ordering::Relaxed);
        for _ in 0..64 {
            if point == 12 {
                assert_eq!(oc_dsp::AudioEffect::latency_samples(fixture.instance()), 0);
            } else {
                assert_eq!(fixture.instance().tail_samples(), u32::MAX);
            }
        }
        let after_metadata = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(
            before_metadata, after_metadata,
            "native metadata fault allocated host memory"
        );
        assert_eq!(
            fixture.calls(point),
            1,
            "metadata quarantine must not re-enter native code"
        );
    }

    // Ownership failure capture uses the same bounded native fault publication.
    for point in [11, 19, 20] {
        let mut fixture = oc_host::vst3_instance::native_fixture::NativeFixture::new(false);
        fixture.set_fault(point, 1);
        let before_refcount = ALLOCATIONS.load(Ordering::Relaxed);
        if point == 11 {
            fixture.retire();
        } else {
            for _ in 0..64 {
                assert!(fixture.exercise_reference(point == 20).is_err());
            }
        }
        let after_refcount = ALLOCATIONS.load(Ordering::Relaxed);
        assert_eq!(
            before_refcount, after_refcount,
            "native ownership fault allocated Rust memory"
        );
        assert_eq!(
            fixture.calls(point),
            1,
            "failed refcount/query call was retried"
        );
    }

    println!(
        "vst3_rt_alloc: 64 warm effect blocks, {} allocations; 64 warm instrument blocks, {} \
         allocations; shape-stable block, 0 further allocations",
        after - before,
        instrument_after - instrument_before
    );
}
