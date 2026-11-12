use super::super::{abi, foreign_threads::ForeignThread, owned_audio_master};
use super::*;
use std::sync::atomic::AtomicBool;

fn context() -> Arc<HostContext> {
    Arc::new(HostContext::new(48_000, 64, 2))
}
fn read<const N: usize>(registry: &Registry<N>, address: usize) -> Reader<'_> {
    match registry.lookup(address) {
        Lookup::Ready(reader) => reader,
        _ => panic!("expected admitted callback"),
    }
}
fn wait_for(signal: &AtomicUsize, value: usize) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while signal.load(Ordering::Acquire) != value {
        assert!(
            std::time::Instant::now() < until,
            "foreign route handshake timed out"
        );
        std::thread::yield_now();
    }
}

#[test]
fn callback_route_retirement_keeps_active_reader_and_defers_final_release_off_callback() {
    let registry = Registry::<1>::new();
    let host = context();
    let weak = Arc::downgrade(&host);
    let handle = registry.reserve(&host).unwrap();
    registry.bind(handle, 1).unwrap();
    let reader = read(&registry, 1);
    registry.retire(handle, false);
    drop(host);
    assert!(matches!(registry.lookup(1), Lookup::Unavailable));
    assert!(matches!(
        registry.reserve(&context()),
        Err(RouteError::Capacity)
    ));
    assert_eq!(reader.value(0), Some(0.0));
    let heap = oc_test_support::operations(|| drop(reader));
    assert_eq!(
        heap, 0,
        "last callback permit must not destroy retained backing"
    );
    assert!(weak.upgrade().is_some());
    let next = registry.reserve(&context()).unwrap();
    assert!(
        weak.upgrade().is_none(),
        "only non-callback collection releases backing"
    );
    registry.retire(next, false);
}

#[test]
fn callback_route_quarantine_retains_address_context_and_finite_capacity() {
    let registry = Registry::<2>::new();
    let host = context();
    let weak = Arc::downgrade(&host);
    let first = registry.reserve(&host).unwrap();
    registry.bind(first, 1).unwrap();
    registry.retire(first, true);
    drop(host);
    assert!(weak.upgrade().is_some());
    assert!(matches!(registry.lookup(1), Lookup::Unavailable));
    let second = registry.reserve(&context()).unwrap();
    assert_eq!(registry.bind(second, 1), Err(RouteError::AddressInUse));
    registry.bind(second, 2).unwrap();
    assert!(matches!(
        registry.reserve(&context()),
        Err(RouteError::Capacity)
    ));
    registry.retire(second, false);
    let third = registry.reserve(&context()).unwrap();
    assert_eq!(registry.bind(third, 1), Err(RouteError::AddressInUse));
    registry.retire(third, false);
}

#[test]
fn callback_route_reentrant_readers_and_busy_count_admission_are_bounded() {
    let registry = Registry::<1>::new();
    let host = context();
    let handle = registry.reserve(&host).unwrap();
    registry.bind(handle, 9).unwrap();
    let outer = read(&registry, 9);
    let inner = read(&registry, 9);
    assert_eq!(outer.sample_rate.load(Ordering::Relaxed), 48_000);
    assert_eq!(inner.sample_rate.load(Ordering::Relaxed), 48_000);
    drop(inner);
    drop(outer);
    for permit in &registry.slots[0].readers {
        permit.store(true, Ordering::SeqCst);
    }
    assert!(matches!(registry.lookup(9), Lookup::Unavailable));
    assert_eq!(registry.fault(handle), Some(RouteFault::AdmissionOverflow));
    for permit in &registry.slots[0].readers {
        permit.store(false, Ordering::SeqCst);
    }
    let reader = read(&registry, 9);
    drop(reader);
    assert_eq!(registry.fault(handle), Some(RouteFault::AdmissionOverflow));
    registry.retire(handle, false);
    let next = registry.reserve(&host).unwrap();
    registry.bind(next, 9).unwrap();
    assert_eq!(registry.fault(next), None);
    registry.retire(next, false);
}

#[test]
fn callback_route_foreign_reader_survives_concurrent_retirement_without_callback_heap() {
    let registry = Arc::new(Registry::<1>::new());
    let host = context();
    let weak = Arc::downgrade(&host);
    let handle = registry.reserve(&host).unwrap();
    registry.bind(handle, 1).unwrap();
    let phase = Arc::new(AtomicUsize::new(0));
    let result = Arc::new(AtomicUsize::new(0));
    let worker = ForeignThread::start({
        let registry = registry.clone();
        let phase = phase.clone();
        let result = result.clone();
        move || {
            let reader = match registry.lookup(1) {
                Lookup::Ready(reader) => Some(reader),
                _ => None,
            };
            phase.store(1, Ordering::Release);
            while phase.load(Ordering::Acquire) != 2 {
                std::hint::spin_loop();
            }
            if let Some(reader) = reader {
                if reader.value(0) != Some(0.0) {
                    result.store(1, Ordering::Release);
                }
                let heap = oc_test_support::operations(|| drop(reader));
                if heap != 0 {
                    result.store(2, Ordering::Release);
                }
            } else {
                result.store(3, Ordering::Release);
            }
        }
    });
    wait_for(&phase, 1);
    registry.retire(handle, false);
    drop(host);
    assert!(matches!(
        registry.reserve(&context()),
        Err(RouteError::Capacity)
    ));
    phase.store(2, Ordering::Release);
    worker.join();
    assert_eq!(result.load(Ordering::Acquire), 0);
    assert!(weak.upgrade().is_some());
    let next = registry.reserve(&context()).unwrap();
    assert!(weak.upgrade().is_none());
    registry.retire(next, false);
}

#[test]
fn callback_route_unknown_descriptor_is_never_dereferenced_or_redirected_to_active_instance() {
    let host = context();
    super::super::ACTIVE_HOST.with(|cell| {
        let previous = cell.replace(Arc::as_ptr(&host).cast_mut());
        // SAFETY: The callback must treat an unregistered non-null address as a
        // lookup key only. Address 1 deliberately has no readable AEffect.
        let reply = unsafe {
            owned_audio_master(
                std::ptr::without_provenance_mut(1),
                abi::host_op::GET_TIME,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            )
        };
        cell.set(previous);
        assert_eq!(reply, 0);
    });
}

#[test]
fn callback_route_actual_foreign_callback_after_descriptor_retirement_is_safe() {
    let host = context();
    // SAFETY: AEffect's scalar, pointer and nullable function-pointer fields
    // permit zero initialization; no plugin callbacks are installed or called.
    let mut descriptor: Box<abi::AEffect> = Box::new(unsafe { std::mem::zeroed() });
    // A callback must not recover context by dereferencing this legacy slot.
    descriptor.reserved1 = 1;
    let address = (&mut *descriptor as *mut abi::AEffect) as usize;
    let mut owner = RouteOwner::reserve(&host).unwrap();
    owner.bind(address).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicBool::new(false));
    let result = Arc::new(AtomicUsize::new(0));
    let worker = ForeignThread::start({
        let ready = ready.clone();
        let retired = retired.clone();
        let result = result.clone();
        move || {
            let mut first = 0;
            let first_heap = oc_test_support::operations(|| {
                // SAFETY: Main retains descriptor/backing until ready; GET_TIME
                // returns a borrowed record which this worker never caches.
                first = unsafe {
                    owned_audio_master(
                        address as *mut abi::AEffect,
                        abi::host_op::GET_TIME,
                        0,
                        0,
                        std::ptr::null_mut(),
                        0.0,
                    )
                };
            });
            if first == 0 || first_heap != 0 {
                result.store(1, Ordering::Release);
            }
            ready.store(1, Ordering::Release);
            while !retired.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let mut late = 1;
            let late_heap = oc_test_support::operations(|| {
                // SAFETY: This is an intentionally retired address, not a
                // descriptor to dereference. The callback must refuse it using
                // registry state alone, with no reserved1 access.
                late = unsafe {
                    owned_audio_master(
                        address as *mut abi::AEffect,
                        abi::host_op::GET_TIME,
                        0,
                        0,
                        std::ptr::null_mut(),
                        0.0,
                    )
                };
            });
            if late != 0 || late_heap != 0 {
                result.store(2, Ordering::Release);
            }
        }
    });
    wait_for(&ready, 1);
    owner.complete();
    drop(host);
    retired.store(true, Ordering::Release);
    worker.join();
    assert_eq!(result.load(Ordering::Acquire), 0);
    drop(descriptor);
}

#[test]
fn callback_route_bind_collects_a_healthy_close_completed_during_native_entry() {
    let registry = Registry::<2>::new();
    let old = registry.reserve(&context()).unwrap();
    registry.bind(old, 1).unwrap();
    let entering = registry.reserve(&context()).unwrap();
    registry.retire(old, false);
    registry.bind(entering, 1).unwrap();
    assert!(matches!(registry.lookup(1), Lookup::Ready(_)));
    assert_eq!(registry.fault(entering), None);
    registry.retire(entering, false);
}

#[test]
fn callback_route_concurrent_native_readers_do_not_spuriously_lose_notifications() {
    let registry = Arc::new(Registry::<1>::new());
    let host = context();
    let handle = registry.reserve(&host).unwrap();
    registry.bind(handle, 1).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::new(AtomicUsize::new(0));
    let finish = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    for _ in 0..4 {
        workers.push(ForeignThread::start({
            let registry = registry.clone();
            let ready = ready.clone();
            let admitted = admitted.clone();
            let finish = finish.clone();
            move || {
                let reader = match registry.lookup(1) {
                    Lookup::Ready(reader) => {
                        admitted.fetch_add(1, Ordering::Relaxed);
                        Some(reader)
                    }
                    _ => None,
                };
                ready.fetch_add(1, Ordering::Release);
                while !finish.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                drop(reader);
            }
        }));
    }
    wait_for(&ready, 4);
    let count = admitted.load(Ordering::Acquire);
    finish.store(true, Ordering::Release);
    for worker in workers {
        worker.join();
    }
    assert_eq!(count, 4);
    assert_eq!(registry.fault(handle), None);
    registry.retire(handle, false);
}
