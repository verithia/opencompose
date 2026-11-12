//! Pitch-synchronous overlap-add primitives shared by the realtime corrector
//! and the offline shifter.
//!
//! The grain is a Hann window of `2 * radius` samples centred on an analysis
//! epoch. The half-sample phase puts the weight of sample `offset` at
//! `PI * (offset + 0.5) / radius`, which makes neighbour grains at hop `radius`
//! sum to one; overlap-add then only has to divide by the accumulated weight.

use std::f64::consts::PI;

/// Hann weight at sample offset `t` from the start of a `2 * radius` window.
pub fn grain_window(t: f64, radius: usize) -> f64 {
    let phase = PI * (t + 0.5) / radius as f64;
    0.5 - 0.5 * phase.cos()
}

/// Sub-sample offset of the parabola through three samples, clamped to ±0.5.
pub fn parabolic_offset(left: f64, center: f64, right: f64) -> f64 {
    let denominator = left - 2.0 * center + right;
    if denominator.abs() > 1e-12 {
        (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
    } else {
        0.0
    }
}

/// Frequency ratio for a shift in cents.
pub fn cents_ratio(cents: f64) -> f64 {
    2f64.powf(cents / 1_200.0)
}

/// Synthesis hop for one analysis period: two samples is the floor that keeps
/// the epoch pointer moving when the period is unknown or tiny.
pub fn synth_hop(period_in: f64, ratio: f64) -> f64 {
    (period_in / ratio).max(2.0)
}

/// Grain radius (half the window length) in samples. The window spans two
/// synthesis periods, so a downward shift still overlaps its neighbour.
pub fn grain_radius(period_in: f64, period_out: f64, scale: f64) -> usize {
    (period_in.max(period_out) * scale).round().max(2.0) as usize
}
