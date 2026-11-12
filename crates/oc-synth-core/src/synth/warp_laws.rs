//! Pre-optimisation warp stage, kept as the bit-identity reference for the fast
//! paths. Test builds only; the audio path never calls this.

use super::*;

/// Original `warp_mod::frac`: unconditional `rem_euclid`.
fn frac(value: f64) -> f64 {
    value.rem_euclid(1.0)
}

/// Original `warp_mod::phase_warp`, before the guarded wrap.
fn phase_warp(mode: WarpMode, amount: f64, phase: f64) -> (f64, f64) {
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

/// Original `oscillator_sample_with_warp` body, calling the unchanged helpers.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn stage(
    waveform: Waveform,
    phase: f64,
    dt: f64,
    output_dt: f64,
    pulse_width: f64,
    wavetable_position: f64,
    custom_wavetable: Option<&CustomWavetable>,
    warp: WarpParams,
    window: f64,
    mip: &mut MipState,
) -> f64 {
    if warp.bypass() {
        return read_warped(
            waveform,
            phase,
            dt,
            output_dt,
            pulse_width,
            wavetable_position,
            custom_wavetable,
            mip,
            IMPORTED_READOUT_DELAY * output_dt,
        );
    }
    let phase = if phase.is_finite() { phase } else { 0.0 };
    let readout_delay = match custom_wavetable {
        Some(table) if table.imported_readout => IMPORTED_READOUT_DELAY * output_dt,
        _ => 0.0,
    };
    let mut warped = (phase - readout_delay).rem_euclid(1.0);
    let mut slope = 1.0;
    let mut gain = 1.0;
    for slot in [warp.slot2, warp.slot1] {
        if slot.has(WARP_PHASE) {
            let (next, local) = phase_warp(slot.mode, slot.amount, warped);
            warped = next;
            slope *= local;
        }
        if slot.has(WARP_FLIP) {
            gain *= warp_mod::flip_sign(warped, slot.amount);
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_SELFPD) {
            let mut scratch = *mip;
            let source = read_warped(
                waveform,
                warped,
                dt,
                output_dt,
                pulse_width,
                wavetable_position,
                custom_wavetable,
                &mut scratch,
                0.0,
            );
            warped = (warped + slot.amount * source).rem_euclid(1.0);
        }
    }
    let slope = if slope.is_finite() && slope > 0.0 {
        slope
    } else {
        1.0
    };
    let read_dt = output_dt * slope;
    let mut output = read_warped(
        waveform,
        warped,
        dt * slope,
        read_dt,
        pulse_width,
        wavetable_position,
        custom_wavetable,
        mip,
        0.0,
    );
    // Bit-identical to the fast path's corner correction (see `pwm_corner_correction`).
    if waveform == Waveform::Wavetable
        && custom_wavetable.is_none()
        && let Some(amount) = pwm_slot_amount(warp)
    {
        let boundary_slope = default_boundary_slope(mip.level, mip.bin, wavetable_position);
        output += pwm_corner_correction(phase, dt, amount, boundary_slope);
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_EVENODD) {
            let mut scratch = *mip;
            let second = read_warped(
                waveform,
                (warped + 0.5).rem_euclid(1.0),
                dt,
                read_dt,
                pulse_width,
                wavetable_position,
                custom_wavetable,
                &mut scratch,
                0.0,
            );
            output += (second + second) * (slot.amount - 0.5);
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_DIST) {
            output = warp_mod::shaper(output, slot.amount, slot.mode.dist_id());
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_SYNC) {
            gain *= warp_mod::sync_window(window, slot.var);
        }
        if slot.has(WARP_RM_GAIN) {
            gain *= 1.0 - slot.amount;
        }
    }
    output * gain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases() -> Vec<f64> {
        let mut values = vec![
            0.0,
            1.0e-12,
            0.25,
            0.499_999_999_999,
            0.5,
            0.500_000_000_001,
            0.75,
            0.999_999_999_999,
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            values.push((state >> 11) as f64 / (1u64 << 53) as f64);
        }
        values
    }

    /// Every stage path must keep the exact bits of the pre-optimisation stage.
    #[test]
    fn fast_paths_match_the_reference_stage() {
        let phases = phases();
        let offsets = [(0.0041, 0.0082), (0.4, 0.5), (0.0, 0.0)];
        let waveforms = [
            Waveform::Saw,
            Waveform::Square,
            Waveform::Sine,
            Waveform::Triangle,
            Waveform::Wavetable,
        ];
        let second_slots = [
            WarpMode::Off,
            WarpMode::BendPos,
            WarpMode::AsymNeg,
            WarpMode::EvenOdd,
            WarpMode::DistTube,
            WarpMode::SelfPd,
            WarpMode::Sync,
            WarpMode::RmOsc,
            WarpMode::Flip,
        ];
        let mut mip = MipState::default();
        let mut reference_mip = MipState::default();
        let mut checked = 0;
        for &first in &WarpMode::ALL {
            for amount in [0.0, 0.3, 0.7, 1.0] {
                for &(dt, output_dt) in &offsets {
                    for &waveform in &waveforms {
                        for phase in phases.iter().copied() {
                            let warp = WarpParams::new(first, amount, 0.6, WarpMode::Off, 0.0, 0.0);
                            let got = oscillator_sample_with_warp(
                                waveform, phase, dt, output_dt, 0.37, 0.4, None, warp, 0.3,
                                &mut mip,
                            );
                            let want = stage(
                                waveform,
                                phase,
                                dt,
                                output_dt,
                                0.37,
                                0.4,
                                None,
                                warp,
                                0.3,
                                &mut reference_mip,
                            );
                            assert_eq!(
                                got.to_bits(),
                                want.to_bits(),
                                "{first:?} amount {amount} phase {phase}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        // Two slots, so the slot-order loops and the gain product are covered.
        for &first in &[
            WarpMode::BendPos,
            WarpMode::AsymPos,
            WarpMode::Dlm,
            WarpMode::Pwm,
            WarpMode::Quantize,
            WarpMode::Remap2,
        ] {
            for &second in &second_slots {
                for amount in [0.0, 0.5, 1.0] {
                    for phase in phases.iter().copied() {
                        let warp = WarpParams::new(first, 0.7, 0.2, second, amount, 0.6);
                        let got = oscillator_sample_with_warp(
                            Waveform::Saw,
                            phase,
                            0.0041,
                            0.0082,
                            0.37,
                            0.4,
                            None,
                            warp,
                            0.25,
                            &mut mip,
                        );
                        let want = stage(
                            Waveform::Saw,
                            phase,
                            0.0041,
                            0.0082,
                            0.37,
                            0.4,
                            None,
                            warp,
                            0.25,
                            &mut reference_mip,
                        );
                        assert_eq!(
                            got.to_bits(),
                            want.to_bits(),
                            "{first:?}/{second:?} {amount} {phase}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 40_000, "checked {checked} stage evaluations");
    }
}
