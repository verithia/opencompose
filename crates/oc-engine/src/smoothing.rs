//! One-pole smoothing shared by every smoothed parameter path.
//!
//! Three callers share the coefficient:
//!
//! * effect-lane smoothing advances one sample at a time,
//! * control-link targets advance one block at a time,
//! * the touch release needs the sample length of its fixed 20 ms fade.
//!
//! [`one_pole_alpha`] is the only implementation of the exponential. The
//! contract is the same for all of them: the returned value is the fraction of
//! the remaining distance to the goal that `frames` samples cover, so the update
//! is always `value += (goal - value) * alpha`. A smoothing time of `0 ms` is a
//! hard over (`1.0`), and one time constant covers `1 - e^-1` of the distance.

/// Fraction of the remaining distance to the goal covered by `frames` samples
/// with a one-pole time constant of `smoothing_ms`.
///
/// The lane evaluator owns the exponential so a rendered value and a read-only
/// read-out cannot disagree; this wrapper keeps the existing call sites.
pub fn one_pole_alpha(smoothing_ms: f64, frames: f64, sample_rate: f64) -> f64 {
    oc_dsl::automation::one_pole_alpha(smoothing_ms, frames, sample_rate)
}

/// Sample count of a linear fade lasting `duration_ms`, at least one sample.
///
/// This is the duration half of the same contract: a fixed fade is specified in
/// milliseconds and has to become a whole number of samples at the device rate.
pub fn frames_for_ms(duration_ms: f64, sample_rate: u32) -> u32 {
    ((f64::from(sample_rate) * duration_ms / 1000.0).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One time constant is 63.2 %, whether it is advanced one sample at a time
    /// or in one block of the same length: the callers only differ in step size.
    #[test]
    fn one_time_constant_always_covers_63_percent() {
        let expected = 1.0 - (-1.0f64).exp();
        assert!((one_pole_alpha(10.0, 480.0, 48_000.0) - expected).abs() < 1e-15);
        let per_sample = one_pole_alpha(10.0, 1.0, 48_000.0);
        let mut value = 0.0;
        for _ in 0..480 {
            value += (1.0 - value) * per_sample;
        }
        assert!((value - expected).abs() < 1e-12);
    }

    /// Zero smoothing is a hard over and zero frames is no movement; a missing
    /// rate cannot produce a coefficient.
    #[test]
    fn the_edges_are_hard_over_no_movement_and_no_rate() {
        assert_eq!(one_pole_alpha(0.0, 1.0, 48_000.0), 1.0);
        assert_eq!(one_pole_alpha(-5.0, 1.0, 48_000.0), 1.0);
        assert_eq!(one_pole_alpha(10.0, 0.0, 48_000.0), 0.0);
        assert_eq!(one_pole_alpha(10.0, 1.0, 0.0), 0.0);
        assert_eq!(one_pole_alpha(10.0, 1.0, -48_000.0), 0.0);
    }

    /// The fade length rounds to the nearest sample and never reaches zero.
    #[test]
    fn a_fade_length_is_a_whole_number_of_samples() {
        assert_eq!(frames_for_ms(20.0, 48_000), 960);
        assert_eq!(frames_for_ms(1.0, 44_100), 44);
        assert_eq!(frames_for_ms(0.0, 48_000), 1);
        assert_eq!(frames_for_ms(20.0, 0), 1);
    }
}
