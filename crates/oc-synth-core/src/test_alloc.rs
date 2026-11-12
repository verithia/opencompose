//! Shared allocator instrumentation for this library's runtime tests.

pub(crate) use oc_test_support::operations;

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;
