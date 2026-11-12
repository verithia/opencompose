//! Static per-reservation host callback identity, including native entry failure.
use super::{Vst2Operation, abi, callback_route, host_opcode};
use crate::native_fault::Fault;
use std::ffi::c_void;
unsafe extern "C" {
    fn oc_vst2_host_callback_for_slot(slot: u32) -> Option<abi::AudioMasterCallback>;
    fn oc_vst2_host_callback_capacity() -> u32;
}
pub(super) fn get(slot: usize) -> Option<abi::AudioMasterCallback> {
    // SAFETY: These native helpers only inspect their immutable static table;
    // no plugin method is entered and no pointer input is dereferenced.
    let capacity = unsafe { oc_vst2_host_callback_capacity() } as usize;
    if capacity != callback_route::CAPACITY || slot >= capacity {
        return None;
    }
    // SAFETY: The checked table index returns null or the exact C callback ABI.
    unsafe { oc_vst2_host_callback_for_slot(slot as u32) }
}
#[unsafe(no_mangle)]
extern "C" fn oc_vst2_owned_callback_bridge(
    slot: u32,
    _effect: *mut abi::AEffect,
    opcode: i32,
    index: i32,
    _value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    let callback_route::Lookup::Ready(reader) = callback_route::lookup_slot(slot as usize) else {
        return 0;
    };
    if reader.failure.failed() {
        return 0;
    }
    crate::native_callback::callback(
        0,
        || host_opcode(&reader, opcode, index, ptr, opt),
        || {
            reader
                .failure
                .record(Vst2Operation::HostCallback(opcode), Fault::callback_panic());
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_callback_table_has_exact_unique_bounded_identity() {
        let pointers: std::collections::HashSet<_> = (0..callback_route::CAPACITY)
            .map(|slot| get(slot).expect("reserved callback") as usize)
            .collect();
        assert_eq!(pointers.len(), callback_route::CAPACITY);
        assert!(get(callback_route::CAPACITY).is_none());
        assert!(get(usize::MAX).is_none());
        // SAFETY: The selector itself bounds untrusted indices and calls no plugin.
        assert!(unsafe { oc_vst2_host_callback_for_slot(u32::MAX) }.is_none());
    }
}

#[cfg(test)]
mod route_tests {
    use super::super::{HostContext, foreign_threads::ForeignThread};
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[test]
    fn reserved_slot_callback_identifies_foreign_entry_thread_without_a_descriptor() {
        let context = Arc::new(HostContext::new(48_000, 64, 0));
        let mut owner = callback_route::RouteOwner::reserve(&context).unwrap();
        let callback = owner.callback().unwrap();
        let observed = Arc::new(AtomicU64::new(0));
        let worker = ForeignThread::start({
            let observed = observed.clone();
            move || {
                let mut value = 0;
                let operations = oc_test_support::operations(|| {
                    // SAFETY: This reservation and its context remain alive
                    // through join. The slot identifies entry even with no
                    // bound/returned AEffect; no descriptor is dereferenced.
                    let reply = unsafe {
                        callback(
                            std::ptr::without_provenance_mut(1),
                            abi::host_op::GET_TIME,
                            0,
                            0,
                            std::ptr::null_mut(),
                            0.0,
                        )
                    };
                    if reply != 0 {
                        // SAFETY: Read the returned borrow only on its native
                        // origin thread before that thread's next GET_TIME.
                        value = unsafe { (reply as *const abi::VstTimeInfo).read() }
                            .sample_rate
                            .to_bits();
                    }
                });
                observed.store(if operations == 0 { value } else { 0 }, Ordering::Release);
            }
        });
        worker.join();
        assert_eq!(f64::from_bits(observed.load(Ordering::Acquire)), 48_000.0);
        owner.complete();
    }
    #[test]
    fn abandoned_unreturned_entry_callback_cannot_borrow_another_slot_context() {
        let a = Arc::new(HostContext::new(48_000, 64, 0));
        let b = Arc::new(HostContext::new(96_000, 64, 0));
        let owner_a = callback_route::RouteOwner::reserve(&a).unwrap();
        let mut owner_b = callback_route::RouteOwner::reserve(&b).unwrap();
        let callback_a = owner_a.callback().unwrap();
        let callback_b = owner_b.callback().unwrap();
        assert_ne!(callback_a as usize, callback_b as usize);
        drop(owner_a); // No returned descriptor: its reservation is quarantined.
        // SAFETY: Both calls use their immutable source thunks. The same bad
        // effect address cannot choose or dereference another slot's context.
        let old = unsafe {
            callback_a(
                std::ptr::without_provenance_mut(1),
                abi::host_op::GET_TIME,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            )
        };
        assert_eq!(old, 0);
        // SAFETY: B remains retained; consume its same-thread borrowed record
        // before another request, independently of A's supplied effect address.
        let current = unsafe {
            callback_b(
                std::ptr::without_provenance_mut(1),
                abi::host_op::GET_TIME,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            )
        };
        assert_ne!(current, 0);
        assert_eq!(
            // SAFETY: This is B's live same-thread result, not an AEffect read.
            unsafe { (current as *const abi::VstTimeInfo).read() }.sample_rate,
            96_000.0
        );
        owner_b.complete();
    }
    #[test]
    fn native_thunk_preserves_callback_index_and_float_abi() {
        let context = Arc::new(HostContext::new(48_000, 64, 2));
        let mut owner = callback_route::RouteOwner::reserve(&context).unwrap();
        let callback = owner.callback().unwrap();
        // SAFETY: The retained reservation identifies its own host; this C ABI
        // call only publishes a bounded atomic parameter value with no pointers.
        unsafe {
            callback(
                std::ptr::null_mut(),
                abi::host_op::AUTOMATE,
                1,
                isize::MAX,
                std::ptr::null_mut(),
                0.375,
            )
        };
        assert_eq!(context.value(0), Some(0.0));
        assert_eq!(context.value(1), Some(0.375));
        owner.complete();
    }
}
