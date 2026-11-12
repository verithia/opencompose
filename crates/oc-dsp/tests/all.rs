//! Integration cases share one binary; use `cargo test -p oc-dsp --test all -- <filter>`.

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

#[path = "devices.rs"]
mod devices;

#[path = "plosive_alloc.rs"]
mod plosive_alloc;
