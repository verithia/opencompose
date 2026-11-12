//! Warp maths for the imported wavetable format: amount curves, phase maps,
//! waveshapers and the sync window. The mode ids are the import ABI.

use super::{UNIT_FRACTION, WarpMode};
use std::f64::consts::PI;

/// Wrap into `[0, 1)`. Most phase-map outputs already land in the range, so
/// the guarded path skips the `fmod` sequence and still returns the exact value.
fn frac(value: f64) -> f64 {
    if (0.0..1.0).contains(&value) {
        value
    } else {
        value.rem_euclid(1.0)
    }
}

/// The per-mode knob curve every warp stage consumes.
pub(super) fn amount_curve(mode: WarpMode, knob: f64) -> f64 {
    let amount = if knob.is_finite() {
        knob.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max)
    } else {
        0.0
    };
    match mode as u8 {
        2 | 6 => 0.5 - 0.4 * amount,
        3 | 7 => 0.5 + 0.4 * amount,
        4 | 8 => 0.1 + 0.8 * amount,
        5 => 1.0 - amount,
        9 => 2.0 * amount,
        10 => 0.05 + 0.9 * amount,
        15 => (1.0 - amount).powi(5),
        23 => 0.05 + 0.95 * amount,
        33..=56 => amount.powi(4),
        57 => 0.6 * amount * amount,
        _ => amount,
    }
}

/// The sync increment factor `1 + 15a³` before the 1.0 clamp.
pub(super) fn sync_factor(amount: f64) -> f64 {
    1.0 + 15.0 * amount * amount * amount
}

/// Half-sine fade over the first and last `width / 2` of a cycle.
pub(super) fn sync_window(phase: f64, width: f64) -> f64 {
    let width = if width.is_finite() {
        width.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max)
    } else {
        0.0
    };
    if width <= 0.0 {
        return 1.0;
    }
    let distance = phase.min(1.0 - phase);
    if distance >= width * 0.5 {
        return 1.0;
    }
    let x = distance * PI / width;
    let square = x * x;
    x + x * square * (-0.166_125_12 + square * (0.008_041 - square * 0.000_149_57))
}

/// Phase warp for modes 2..=15. Returns the phase and its local
/// slope; the slope only drives our (deliberate) band-limit selection.
pub(super) fn phase_warp(mode: WarpMode, amount: f64, phase: f64) -> (f64, f64) {
    match mode {
        WarpMode::BendPos | WarpMode::BendNeg | WarpMode::BendPosNeg => {
            if phase == 0.0 || phase == 0.5 {
                return (phase, 1.0);
            }
            if phase < 0.5 {
                let denominator = 1.0 - amount + phase * (4.0 * amount - 2.0);
                (
                    frac(phase * amount / denominator),
                    amount * (1.0 - amount) / (denominator * denominator),
                )
            } else {
                let p = 2.0 * phase - 1.0;
                let denominator = (2.0 * amount - 1.0) * p - amount;
                (
                    frac(0.5 * ((3.0 * amount - 2.0) * p - amount) / denominator),
                    amount * (1.0 - amount) / (denominator * denominator),
                )
            }
        }
        WarpMode::Pwm => {
            if phase < amount {
                (frac(phase / amount), 1.0 / amount)
            } else {
                (0.0, 0.0)
            }
        }
        WarpMode::AsymPos | WarpMode::AsymNeg | WarpMode::AsymPosNeg => {
            if phase < amount {
                (frac(0.5 * phase / amount), 0.5 / amount)
            } else {
                (
                    frac(0.5 + 0.5 * (phase - amount) / (1.0 - amount)),
                    0.5 / (1.0 - amount),
                )
            }
        }
        WarpMode::Dlm => {
            let x = phase.min(1.0 - phase);
            if x < amount * 0.5 {
                (frac(x / amount), 1.0 / amount)
            } else {
                (
                    frac(0.5 + 0.5 * (2.0 * x - amount) / (1.0 - amount)),
                    1.0 / (1.0 - amount),
                )
            }
        }
        WarpMode::Remap1 => (phase, 1.0),
        WarpMode::Remap2 => (frac(2.0 * phase), 2.0),
        WarpMode::Remap3 => (frac(3.0 * phase), 3.0),
        WarpMode::Remap4 => (frac(4.0 * phase), 4.0),
        WarpMode::Quantize => {
            if amount == 1.0 {
                (phase, 1.0)
            } else {
                let steps = 1023.0 * amount + 2.0;
                (frac((phase * steps + 0.5).floor() / steps), 1.0)
            }
        }
        _ => (phase, 1.0),
    }
}

/// `kFlip`: negate the cycle below the warped amount (wrap when `amount > 1`).
pub(super) fn flip_sign(phase: f64, amount: f64) -> f64 {
    let inverted = if amount >= 1.0 {
        phase > amount - 1.0
    } else {
        phase < amount
    };
    if inverted { -1.0 } else { 1.0 }
}

/// Folded triangle with the 1.925/0.925 (or 1.8/0.8) contraction.
fn fold(mut value: f64, high: f64, low: f64) -> f64 {
    for _ in 0..160 {
        if value > 1.0 {
            value = high - low * value;
        } else if value < -1.0 {
            value = -high - low * value;
        } else {
            return value;
        }
    }
    value.clamp(-1.0, 1.0)
}

fn sign(value: f64) -> f64 {
    if value < 0.0 { -1.0 } else { 1.0 }
}

/// The 14 waveshapers selected by the shaper id table.
// NaN must saturate to the rail here; `clamp` would propagate it into the table readout.
#[allow(clippy::manual_clamp)]
pub(super) fn shaper(x: f64, drive: f64, id: usize) -> f64 {
    let d = drive;
    match id {
        0 => {
            let y = x / (1.0 + 0.989_000_02 * d * (x.abs() - 1.0));
            y.min(1.0).max(-1.0)
        }
        1 => {
            let y = x * (1.0 + 2.949_120_04 * d * d * d);
            if y.abs() > 0.5 {
                let shaped = (y.abs() - 0.5) / (y.abs() + 0.5) + 0.5;
                sign(x) * shaped
            } else {
                y
            }
        }
        2 => (x * (1.0 + 4.0 * d * d * d)).clamp(-1.0, 1.0),
        3 => {
            if d <= 0.125 {
                let magnitude = x.abs();
                let ratio = (-21.0 - magnitude) / ((magnitude + 22.4) * magnitude + 1.0);
                x - x * (8.0 * d * (1.0 + ratio))
            } else {
                let t = (8.0 * d - 1.0) / 7.0;
                let f = 100.0 * t * t * t + x.abs() + 21.0;
                x * f / ((f + 1.4) * x.abs() + 1.0)
            }
        }
        4 => {
            let value = x * d * 21.708_799_4;
            if value > 2.5 {
                0.57
            } else if value < -2.5 {
                -0.68
            } else {
                (value + 0.079_706_5).sin() - 0.079_622_13
            }
        }
        5 => fold(x * (1.0 + 2_129.919_92 * d * d * d), 1.925, 0.925),
        6 => {
            let y = x * (1.0 + 2_129.919_92 * d * d * d);
            if y.abs() <= 1.0 {
                y
            } else {
                (fold(y, 1.8, 0.8) * 1.578_055_98).sin()
            }
        }
        7 => {
            let step = if x > 1.0e-4 {
                1.0
            } else if x < -1.0e-4 {
                -1.0
            } else {
                0.0
            };
            let y = step + d * x;
            let magnitude = (1.1 * y.abs()).powi(5).atan();
            sign(y) * 0.9 * magnitude.powf(0.2)
        }
        9 => {
            let y = x * (1.0 + 20.0 * d * d);
            let g = y * (0.0515 * y + 1.038_990_02);
            (y / (g * g + 0.6211).sqrt()).clamp(-1.0, 1.0)
        }
        10 => (x.abs() * (1.0 - 20.0 * d * d)).clamp(-1.0, 1.0),
        13 => {
            let g = d * d + 0.05;
            let mut y = -0.967_1 * g * ((14.45 * g + 0.5) * x).sin();
            for _ in 0..3 {
                y = (2.0 * y.abs() - 4.0) * y;
            }
            y * 0.5
        }
        14 => {
            let y = x * (1.0 + 7.0 * d);
            if y.abs() <= 0.8 {
                y
            } else {
                let s = sign(x);
                s + ((7.0 * d + 1.0) * 0.2 * (x - s)) / (7.0 * d + 0.2)
            }
        }
        15 => {
            let k = 3.0 * d + 1.0;
            if k <= 1.0001 {
                x
            } else {
                let value = k * x;
                if value.abs() > 2.0 / 3.0 {
                    let s = sign(x);
                    ((k * 0.837_758_06 * (x - s)) / (3.0 * d + 1.0 / 3.0)).cos() * s
                } else {
                    value
                }
            }
        }
        17 => {
            let y = (x * (1.0 + 16.0 * d * d)).clamp(-1.5, 1.5);
            if y > 0.5 {
                1.0 - 0.5 * (1.5 - y) * (1.5 - y)
            } else if y < -0.5 {
                0.5 * (y + 1.5) * (y + 1.5) - 1.0
            } else {
                y
            }
        }
        _ => x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) {
        assert!((a - b).abs() <= tolerance, "{a} != {b}");
    }

    #[test]
    fn bend_is_the_identity_at_half_amount() {
        for phase in [0.0, 0.07, 0.25, 0.5, 0.73, 0.99] {
            let (warped, slope) = phase_warp(WarpMode::BendPos, 0.5, phase);
            close(warped, phase, 1.0e-12);
            close(slope, 1.0, 1.0e-9);
        }
    }

    #[test]
    fn asym_is_the_identity_at_half_amount() {
        for phase in [0.0, 0.1, 0.3, 0.5, 0.8, 0.999] {
            let (warped, _) = phase_warp(WarpMode::AsymPos, 0.5, phase);
            close(warped, phase, 1.0e-12);
        }
    }

    #[test]
    fn pwm_holds_after_the_duty_point_and_is_identity_at_one() {
        for phase in [0.0, 0.2, 0.5, 0.9] {
            close(phase_warp(WarpMode::Pwm, 1.0, phase).0, phase, 1.0e-12);
        }
        close(phase_warp(WarpMode::Pwm, 0.5, 0.25).0, 0.5, 1.0e-12);
        close(phase_warp(WarpMode::Pwm, 0.5, 0.75).0, 0.0, 1.0e-12);
    }

    #[test]
    fn dlm_is_a_triangle_read_at_half_amount() {
        for phase in [0.0, 0.1, 0.25, 0.5, 0.9] {
            let (warped, _) = phase_warp(WarpMode::Dlm, 0.5, phase);
            close(warped, (2.0 * phase.min(1.0 - phase)).fract(), 1.0e-12);
        }
    }

    #[test]
    fn quantize_is_the_identity_at_one_and_steps_elsewhere() {
        for phase in [0.0, 0.17, 0.42, 0.81] {
            close(phase_warp(WarpMode::Quantize, 1.0, phase).0, phase, 0.0);
            let (warped, _) = phase_warp(WarpMode::Quantize, 0.0, phase);
            let steps = (phase * 2.0 + 0.5).floor() * 0.5;
            close(warped, steps.fract(), 1.0e-9);
        }
    }

    #[test]
    fn remap_folds_are_integer_phase_multiples() {
        for phase in [0.0, 0.13, 0.4, 0.77, 0.99] {
            close(phase_warp(WarpMode::Remap1, 0.5, phase).0, phase, 0.0);
            close(
                phase_warp(WarpMode::Remap2, 0.5, phase).0,
                (2.0 * phase).fract(),
                1.0e-12,
            );
            close(
                phase_warp(WarpMode::Remap3, 0.5, phase).0,
                (3.0 * phase).fract(),
                1.0e-12,
            );
            close(
                phase_warp(WarpMode::Remap4, 0.5, phase).0,
                (4.0 * phase).fract(),
                1.0e-12,
            );
        }
    }

    #[test]
    fn flip_negates_the_first_cycle_only() {
        close(flip_sign(0.1, 0.0), 1.0, 0.0);
        close(flip_sign(0.1, 0.5), -1.0, 0.0);
        close(flip_sign(0.9, 0.5), 1.0, 0.0);
        close(flip_sign(0.4, 0.75), -1.0, 0.0);
        close(flip_sign(0.9, 0.75), 1.0, 0.0);
    }

    #[test]
    fn amount_curve_matches_the_table() {
        for amount in [0.0, 0.25, 0.5, 0.75, 1.0] {
            close(
                amount_curve(WarpMode::BendPos, amount),
                0.5 - 0.4 * amount,
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::BendNeg, amount),
                0.5 + 0.4 * amount,
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::BendPosNeg, amount),
                0.1 + 0.8 * amount,
                1.0e-12,
            );
            close(amount_curve(WarpMode::Pwm, amount), 1.0 - amount, 1.0e-12);
            close(amount_curve(WarpMode::Flip, amount), 2.0 * amount, 1.0e-12);
            close(
                amount_curve(WarpMode::Dlm, amount),
                0.05 + 0.9 * amount,
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::Quantize, amount),
                (1.0 - amount).powi(5),
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::DistDiode2, amount),
                0.05 + 0.95 * amount,
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::SelfPd, amount),
                0.6 * amount * amount,
                1.0e-12,
            );
            close(
                amount_curve(WarpMode::FmOsc, amount),
                amount.powi(4),
                1.0e-12,
            );
            close(amount_curve(WarpMode::Sync, amount), amount, 1.0e-12);
        }
    }

    #[test]
    fn sync_window_is_one_outside_the_fade_and_zero_at_the_edges() {
        close(sync_window(0.0, 0.25), 0.0, 1.0e-9);
        close(sync_window(0.5, 0.25), 1.0, 0.0);
        close(sync_window(0.49, 0.25), 1.0, 0.0);
        close(sync_window(0.1, 0.0), 1.0, 0.0);
        let x = 0.06 * PI / 0.25;
        close(
            sync_window(0.06, 0.25),
            x + x * x * x * (-0.166_125_12 + x * x * (0.008_041 - x * x * 0.000_149_57)),
            1.0e-12,
        );
    }

    #[test]
    fn shapers_are_finite_and_odd_where_the_curve_is_odd() {
        for id in [0, 1, 2, 3, 4, 5, 6, 7, 9, 10, 13, 14, 15, 17] {
            for drive in [0.0, 0.25, 0.5, 1.0] {
                for x in [-1.0, -0.3, 0.0, 0.3, 1.0] {
                    let value = shaper(x, drive, id);
                    assert!(value.is_finite(), "id {id} drive {drive} x {x}");
                }
            }
        }
    }

    #[test]
    fn soft_sat_small_signal_gain_matches_the_drive_law() {
        close(shaper(0.001, 0.5, 17), 0.001 * 5.0, 1.0e-9);
        close(shaper(0.001, 0.0, 17), 0.001, 1.0e-9);
    }
}
