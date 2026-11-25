//! Integration cases share one binary; use `cargo test -p oc-synth-core --test all -- <filter>`.

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

#[path = "synth_catalog.rs"]
mod synth_catalog;

#[path = "amp_damping.rs"]
mod amp_damping;

#[path = "sampler_release.rs"]
mod sampler_release;

#[path = "granular_safety.rs"]
mod granular_safety;
