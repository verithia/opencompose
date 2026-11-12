//! Shared caller-thread allocation instrumentation for GUI library tests.
pub(crate) use oc_test_support::operations;
#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;
