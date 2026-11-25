//! Causal 2x interpolation and decimation using a two-path elliptic half-band.

use std::f64::consts::PI;
use std::sync::LazyLock;

const SECTIONS: usize = 3;
const POLES: usize = 2 * SECTIONS;
// A transition centered on half Nyquist leaves 80% of the signal band flat
// with three first-order all-pass sections per phase.
const TRANSITION_WIDTH: f64 = 0.10;
static HALF_BAND_POLES: LazyLock<[f64; POLES]> = LazyLock::new(design_poles);

fn elliptic_integral(parameter: f64) -> f64 {
    let mut arithmetic = 1.0;
    let mut geometric = (1.0 - parameter).sqrt();
    for _ in 0..8 {
        let next = (arithmetic + geometric) * 0.5;
        geometric = (arithmetic * geometric).sqrt();
        arithmetic = next;
    }
    PI / (2.0 * arithmetic)
}

fn design_poles() -> [f64; POLES] {
    let modulus = (PI * (0.25 - 0.5 * TRANSITION_WIDTH)).tan().powi(2);
    let parameter = modulus * modulus;
    let nome = (-PI * elliptic_integral(1.0 - parameter) / elliptic_integral(parameter)).exp();
    let order = (2 * POLES + 1) as f64;
    std::array::from_fn(|index| {
        let angle = PI * (index + 1) as f64 / order;
        // Evaluate the odd and even theta series using the elliptic nome.
        let mut odd = angle.sin();
        let mut even = 0.5;
        for harmonic in 1..12_i32 {
            let sign = if harmonic % 2 == 0 { 1.0 } else { -1.0 };
            odd += sign
                * nome.powi(harmonic * (harmonic + 1))
                * ((2 * harmonic + 1) as f64 * angle).sin();
            even += sign * nome.powi(harmonic * harmonic) * ((2 * harmonic) as f64 * angle).cos();
        }
        let ratio = (nome.powf(0.25) * odd / even).powi(2);
        let root = ((1.0 - modulus * ratio) * (1.0 - ratio / modulus)).sqrt() / (1.0 + ratio);
        (1.0 - root) / (1.0 + root)
    })
}

#[derive(Default)]
struct AllPass {
    input: [f64; 2],
    output: [f64; 2],
}

impl AllPass {
    fn process(&mut self, input: [f64; 2], pole: f64) -> [f64; 2] {
        let output = std::array::from_fn(|channel| {
            self.input[channel] + pole * (input[channel] - self.output[channel])
        });
        self.input = input;
        self.output = output;
        output
    }
}

#[derive(Default)]
struct PhasePair {
    even: [AllPass; SECTIONS],
    odd: [AllPass; SECTIONS],
}

impl PhasePair {
    fn process(&mut self, mut phases: [[f64; 2]; 2], poles: &[f64; POLES]) -> [[f64; 2]; 2] {
        for section in 0..SECTIONS {
            phases[0] = self.even[section].process(phases[0], poles[2 * section]);
            phases[1] = self.odd[section].process(phases[1], poles[2 * section + 1]);
        }
        phases
    }
}

pub(crate) struct Polyphase2x {
    poles: [f64; POLES],
    up: PhasePair,
    key: PhasePair,
    down: PhasePair,
}

impl Polyphase2x {
    pub(crate) fn new() -> Self {
        Self {
            poles: *HALF_BAND_POLES,
            up: PhasePair::default(),
            key: PhasePair::default(),
            down: PhasePair::default(),
        }
    }

    pub(crate) fn interpolate(&mut self, input: [f64; 2]) -> [[f64; 2]; 2] {
        self.up.process([input; 2], &self.poles)
    }

    pub(crate) fn interpolate_key(&mut self, input: [f64; 2]) -> [[f64; 2]; 2] {
        self.key.process([input; 2], &self.poles)
    }

    pub(crate) fn decimate(&mut self, [even, odd]: [[f64; 2]; 2]) -> [f64; 2] {
        // Synthesis exchanges the phase pairing so the two paths accumulate
        // the same delay in a round trip.
        let [a, b] = self.down.process([odd, even], &self.poles);
        [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
    }

    pub(crate) fn reset(&mut self) {
        self.up = PhasePair::default();
        self.key = PhasePair::default();
        self.down = PhasePair::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_half_band_rejects_images_and_keeps_the_passband_flat() {
        let poles = design_poles();
        assert!(poles.windows(2).all(|p| 0.0 < p[0] && p[0] < p[1]));
        assert!(poles[POLES - 1] < 1.0);
        let branch_phase = |w: f64, offset: usize| -> f64 {
            poles
                .iter()
                .skip(offset)
                .step_by(2)
                .map(|p| -2.0 * (((1.0 - p) / (1.0 + p)) * w.tan()).atan())
                .sum()
        };
        let magnitude = |w: f64| {
            ((branch_phase(w, 0) - branch_phase(w, 1) + w) * 0.5)
                .cos()
                .abs()
        };
        for i in 0..100 {
            let pass = (0.5 - TRANSITION_WIDTH) * PI * (i as f64 / 100.0);
            assert!((magnitude(pass) - 1.0).abs() < 1e-8);
            assert!(magnitude(PI - pass) < 1e-5);
        }
    }
}
