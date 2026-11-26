//! Two-phase windowed-sinc oversampling with a fixed integer latency.

use std::f64::consts::PI;
use std::sync::LazyLock;

// Choose a one-millisecond filter delay at the standard design rate. The
// filter is measured in samples and keeps that delay at any project rate.
const DESIGN_RATE: usize = 44_100;
pub(crate) const FIR_2X_LATENCY: usize = DESIGN_RATE / 1_000;
const PHASE_TAPS: usize = FIR_2X_LATENCY * 2;
const WINDOW_TAPS: usize = PHASE_TAPS * 2;

struct Decimator {
    odd: [f64; PHASE_TAPS],
    center: f64,
}

fn halfband() -> Decimator {
    // Read at the second high-rate frame of each pair. Deliver the center tap
    // after an integer number of project frames, using the odd tap grid.
    let center = (FIR_2X_LATENCY * 2 + 1) as f64;
    let window = |index: f64| (PI * index / WINDOW_TAPS as f64).sin();
    Decimator {
        odd: std::array::from_fn(|index| {
            let tap = (index * 2) as f64;
            let angle = (tap - center) * PI * 0.5;
            0.5 * angle.sin() / angle * window(tap)
        }),
        center: 0.5 * window(center),
    }
}

static DECIMATOR: LazyLock<Decimator> = LazyLock::new(halfband);

pub(crate) struct Fir2x {
    coefficients: &'static Decimator,
    even: [[f64; 2]; PHASE_TAPS],
    // Store a mirrored copy so the convolution can walk a contiguous window.
    odd: [[f64; 2]; 2 * PHASE_TAPS],
    cursor: usize,
}

impl Fir2x {
    pub(crate) fn new() -> Self {
        Self {
            coefficients: &DECIMATOR,
            even: [[0.0; 2]; PHASE_TAPS],
            odd: [[0.0; 2]; 2 * PHASE_TAPS],
            cursor: 0,
        }
    }

    pub(crate) fn interpolate(input: [f64; 2]) -> [[f64; 2]; 2] {
        // For linear filters the reconstruction low-pass can follow the
        // high-rate processor. Zero insertion leaves unity passband gain.
        [input.map(|sample| sample * 2.0), [0.0; 2]]
    }

    pub(crate) fn decimate(&mut self, [even, odd]: [[f64; 2]; 2]) -> [f64; 2] {
        let head = self.cursor + PHASE_TAPS;
        self.even[self.cursor] = even;
        self.odd[self.cursor] = odd;
        self.odd[head] = odd;
        let original_phase = self.even[(head - FIR_2X_LATENCY) % PHASE_TAPS];
        let mut result = original_phase.map(|value| value * self.coefficients.center);
        // The odd-index kernel taps are zero except at its center. Accumulate
        // the complete remaining phase against consecutive pairs from newest.
        for ([left, right], &weight) in self.odd[head + 1 - PHASE_TAPS..=head]
            .iter()
            .rev()
            .zip(&self.coefficients.odd)
        {
            result[0] += left * weight;
            result[1] += right * weight;
        }
        self.cursor = (self.cursor + 1) % PHASE_TAPS;
        result
    }

    pub(crate) fn reset(&mut self) {
        self.even.fill([0.0; 2]);
        self.odd.fill([0.0; 2]);
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fir_reconstruction_delays_both_channels_by_the_reported_frames() {
        let mut device = Fir2x::new();
        let output: Vec<_> = (0..2 * PHASE_TAPS)
            .map(|index| {
                let input = if index == 0 { [1.0, -0.5] } else { [0.0; 2] };
                device.decimate(Fir2x::interpolate(input))
            })
            .collect();
        for (index, frame) in output.iter().enumerate() {
            if index == FIR_2X_LATENCY {
                assert!((frame[0] - 1.0).abs() < 0.001);
                assert_eq!(frame[1], -0.5 * frame[0]);
            } else {
                assert_eq!(*frame, [0.0; 2]);
            }
        }
    }
}
