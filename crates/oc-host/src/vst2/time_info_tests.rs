use super::super::foreign_threads::ForeignThread;
use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn wait_for(signal: &AtomicUsize, expected: usize) {
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while signal.load(Ordering::Acquire) != expected {
        assert!(
            std::time::Instant::now() < limit,
            "foreign thread handshake timed out"
        );
        std::thread::yield_now();
    }
}
fn publisher<const N: usize>() -> TimeInfoPublisher<N> {
    TimeInfoPublisher::new(48_000)
}
fn slice(position: u32) -> VstTimeInfo {
    VstTimeInfo {
        sample_pos: f64::from(position),
        sample_rate: 48_000.0,
        ppq_pos: f64::from(position) * 2.0,
        tempo: f64::from(position) + 100.0,
        ..VstTimeInfo::default()
    }
}

#[test]
fn time_info_busy_and_invalid_publication_fail_closed_without_waiting() {
    let unavailable = TimeInfoPublisher::<2>::new(0);
    assert!(unavailable.snapshot().is_none());
    let clock = publisher::<2>();
    assert_eq!(clock.snapshot().unwrap().sample_rate, 48_000.0);
    let permit = try_permit(&clock.writing).unwrap();
    assert!(clock.snapshot().is_none());
    // SAFETY: No returned record is read; this exercises optional busy refusal.
    assert!(unsafe { clock.callback_pointer() }.is_null());
    assert!(!clock.publish(slice(1)));
    drop(permit);
    assert!(
        clock.snapshot().is_none(),
        "failed update must not serve a previous slice"
    );
    assert!(clock.publish(slice(2)));
    assert_eq!(clock.snapshot().unwrap().sample_pos, 2.0);
    let mut invalid = slice(3);
    invalid.sample_rate = 0.0;
    assert!(!clock.publish(invalid));
    assert!(clock.snapshot().is_none());
}

#[test]
fn time_info_reentrant_output_slot_refuses_without_overwriting() {
    let clock = publisher::<1>();
    // SAFETY: Main owns and reads its callback record before the next request.
    let pointer = unsafe { clock.callback_pointer() };
    assert!(!pointer.is_null());
    // SAFETY: The record belongs to this calling native thread.
    let before = unsafe { pointer.read() };
    let permit = try_permit(&clock.slots[0].writing).unwrap();
    assert!(clock.publish(slice(8)));
    // SAFETY: No borrowed read overlaps the request. The held slot permit models
    // a reentrant request and must prevent writes to the prior result.
    assert!(unsafe { clock.callback_pointer() }.is_null());
    // SAFETY: Refused requests did not overwrite the calling thread's record.
    assert_eq!(unsafe { pointer.read() }.sample_pos, before.sample_pos);
    drop(permit);
    // SAFETY: All prior reads have finished before the successful next request.
    assert!(!unsafe { clock.callback_pointer() }.is_null());
}

#[test]
fn time_info_foreign_thread_pointer_survives_publication_and_first_callback_has_no_heap() {
    let clock = Arc::new(publisher::<2>());
    let phase = Arc::new(AtomicUsize::new(0));
    let result = Arc::new(AtomicUsize::new(0));
    let worker = ForeignThread::start({
        let clock = clock.clone();
        let phase = phase.clone();
        let result = result.clone();
        move || {
            let mut pointer = std::ptr::null();
            let heap = oc_test_support::operations(|| {
                // SAFETY: The worker reads only on this thread, before its next
                // request, and its Arc keeps all record backing alive.
                pointer = unsafe { clock.callback_pointer() };
            });
            if pointer.is_null() || heap != 0 {
                result.store(1, Ordering::Release);
            }
            phase.store(1, Ordering::Release);
            while phase.load(Ordering::Acquire) != 2 {
                std::hint::spin_loop();
            }
            if !pointer.is_null() {
                // SAFETY: No request on this worker has overwritten its slot;
                // publications only affect the independent current record.
                let before = unsafe { pointer.read() };
                if before.sample_pos != 0.0 || before.sample_rate != 48_000.0 {
                    result.store(2, Ordering::Release);
                }
                // SAFETY: Prior pointer reads have completed on this thread.
                let next = unsafe { clock.callback_pointer() };
                if next.is_null() {
                    result.store(3, Ordering::Release);
                } else {
                    // SAFETY: Same-thread borrowed read before another request.
                    if unsafe { next.read() }.sample_pos != 42.0 {
                        result.store(4, Ordering::Release);
                    }
                }
            }
        }
    });
    wait_for(&phase, 1);
    assert!(clock.publish(slice(42)));
    phase.store(2, Ordering::Release);
    worker.join();
    assert_eq!(result.load(Ordering::Acquire), 0);
}

#[test]
fn time_info_live_native_thread_slots_are_bounded_and_not_evicted() {
    let clock = Arc::new(publisher::<2>());
    let ready = Arc::new(AtomicUsize::new(0));
    let finish = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..3 {
        workers.push(ForeignThread::start({
            let clock = clock.clone();
            let ready = ready.clone();
            let finish = finish.clone();
            let accepted = accepted.clone();
            move || {
                // SAFETY: Read-only callback-thread borrow backed by this Arc.
                let pointer = unsafe { clock.callback_pointer() };
                if !pointer.is_null() {
                    accepted.fetch_add(1, Ordering::Relaxed);
                }
                ready.fetch_add(1, Ordering::Release);
                while !finish.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            }
        }));
    }
    wait_for(&ready, 3);
    let count = accepted.load(Ordering::Acquire);
    finish.store(true, Ordering::Release);
    for worker in workers {
        worker.join();
    }
    assert_eq!(
        count, 2,
        "third simultaneously live thread must receive NULL"
    );
}

#[test]
fn time_info_concurrent_foreign_callbacks_never_observe_torn_slices() {
    let clock = Arc::new(publisher::<4>());
    assert!(clock.publish(slice(0)));
    let stop = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..2 {
        workers.push(ForeignThread::start({
            let clock = clock.clone();
            let stop = stop.clone();
            let errors = errors.clone();
            let ready = ready.clone();
            move || {
                ready.fetch_add(1, Ordering::Release);
                while !stop.load(Ordering::Acquire) {
                    // SAFETY: Each borrowed record is read immediately by its
                    // native owner, entirely before this worker's next request.
                    let pointer = unsafe { clock.callback_pointer() };
                    if !pointer.is_null() {
                        // SAFETY: The record belongs to this live thread.
                        let info = unsafe { pointer.read() };
                        if info.sample_rate != 48_000.0
                            || info.ppq_pos != info.sample_pos * 2.0
                            || info.tempo != info.sample_pos + 100.0
                        {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }
    wait_for(&ready, 2);
    for position in 1..10_000 {
        clock.publish(slice(position));
    }
    stop.store(true, Ordering::Release);
    for worker in workers {
        worker.join();
    }
    assert_eq!(errors.load(Ordering::Acquire), 0);
    assert!(clock.publish(slice(10_000)));
    assert_eq!(clock.snapshot().unwrap().sample_pos, 10_000.0);
}

#[test]
fn time_info_actual_audio_master_foreign_callback_is_stable_and_allocation_free() {
    use super::super::{HostContext, abi, audio_master};
    let context = Arc::new(HostContext::new(44_100, 64, 0));
    // SAFETY: AEffect contains scalar fields, raw pointers and nullable function
    // pointers; all-zero representations are valid before filling host backing.
    let mut descriptor: Box<abi::AEffect> = Box::new(unsafe { std::mem::zeroed() });
    descriptor.reserved1 = (&*context as *const HostContext) as isize;
    let address = (&mut *descriptor as *mut abi::AEffect) as usize;

    let phase = Arc::new(AtomicUsize::new(0));
    let result = Arc::new(AtomicUsize::new(0));
    let worker = ForeignThread::start({
        let phase = phase.clone();
        let result = result.clone();
        move || {
            let mut reply = 0;
            let heap = oc_test_support::operations(|| {
                // SAFETY: The test main retains the valid descriptor and its
                // context through join; GET_TIME only borrows host output.
                reply = unsafe {
                    audio_master(
                        address as *mut abi::AEffect,
                        abi::host_op::GET_TIME,
                        0,
                        0,
                        std::ptr::null_mut(),
                        0.0,
                    )
                };
            });
            if reply == 0 || heap != 0 {
                result.store(1, Ordering::Release);
            }
            phase.store(1, Ordering::Release);
            while phase.load(Ordering::Acquire) != 2 {
                std::hint::spin_loop();
            }
            if reply != 0 {
                // SAFETY: The pointer is read only by its originating native
                // thread, before another request, while main retains backing.
                let info = unsafe { (reply as *const abi::VstTimeInfo).read() };
                if info.sample_pos != 0.0 || info.sample_rate != 44_100.0 || info.flags != 0 {
                    result.store(2, Ordering::Release);
                }
            }
        }
    });
    wait_for(&phase, 1);
    context.time_info.publish(slice(99));
    phase.store(2, Ordering::Release);
    worker.join();
    assert_eq!(result.load(Ordering::Acquire), 0);
    // Descriptor must stay live through join, even though only its address was
    // transferred to the foreign callback thread.
    drop(descriptor);
    drop(context);
}
