//! Timing unit for the CPU-budget tests.
//!
//! A budget in seconds per audio second is a statement about the host at the
//! time of measurement, so a loaded host makes it fail and a faster one makes
//! it meaningless. The budgets are therefore expressed as a ratio to
//! [`reference_seconds`], a fixed piece of arithmetic measured in the same
//! process: both sides slow down together under load, so the ratio does not.

use std::hint::black_box;
use std::time::Instant;

/// Best-of-three seconds for the fixed reference workload.
pub(crate) fn reference_seconds() -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let started = Instant::now();
        black_box(work());
        best = best.min(started.elapsed().as_secs_f64());
    }
    best
}

/// Four million square roots and multiply-adds; long enough to time, short
/// enough that a test pays milliseconds for it.
fn work() -> f64 {
    let mut acc = 0.0f64;
    for step in 0..4_000_000u32 {
        let value = f64::from(step) * 1.000_001;
        acc += (value * 0.5).sqrt() * 0.25;
    }
    acc
}
