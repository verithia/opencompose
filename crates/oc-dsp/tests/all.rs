//! Integration cases share one binary; use `cargo test -p oc-dsp --test all -- <filter>`.

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

#[path = "devices.rs"]
mod devices;

#[path = "plosive_alloc.rs"]
mod plosive_alloc;

#[path = "allpass.rs"]
mod allpass;

#[path = "delay.rs"]
mod delay;

#[path = "transfer_shaper.rs"]
mod transfer_shaper;

#[path = "frequency_shift.rs"]
mod frequency_shift;

#[path = "vocoder.rs"]
mod vocoder;
