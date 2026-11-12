//! Hosted plugin integration tests.

#[path = "suite/lv2.rs"]
mod lv2;

#[path = "suite/plugins.rs"]
mod host_callbacks;

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;
