//! Windowed-sinc band-limiting kernels shared by oscillators and oversampled effects.

use std::sync::OnceLock;

/// Taps in the half-band decimator.
pub const HALF_BAND_TAPS: usize = 113;
/// Centre tap of the half-band kernel.
pub const HALF_BAND_CENTER: usize = 56;

/// Normalized sinc function.
pub fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..=32 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
    }
    sum
}

/// Kaiser window value at `index` for `length` and `beta`.
pub fn kaiser(index: usize, length: usize, beta: f64) -> f64 {
    let x = 2.0 * index as f64 / (length - 1) as f64 - 1.0;
    bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / bessel_i0(beta)
}

/// 113-tap half-band decimator: -6 dB at 0.239 fs, DC gain 1.0009.
pub fn half_band_kernel() -> &'static [f64; HALF_BAND_TAPS] {
    static KERNEL: OnceLock<[f64; HALF_BAND_TAPS]> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let beta = 8.0;
        let make = |cutoff: f64| {
            let mut taps = [0.0; HALF_BAND_TAPS];
            for (index, tap) in taps.iter_mut().enumerate() {
                let offset = index as f64 - HALF_BAND_CENTER as f64;
                *tap = 2.0 * cutoff * sinc(2.0 * cutoff * offset);
                *tap *= kaiser(index, HALF_BAND_TAPS, beta);
            }
            taps
        };
        let response = |taps: &[f64; HALF_BAND_TAPS], frequency: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (index, &tap) in taps.iter().enumerate() {
                let angle = -std::f64::consts::TAU * frequency * index as f64;
                re += tap * angle.cos();
                im += tap * angle.sin();
            }
            (re * re + im * im).sqrt()
        };
        let (mut low, mut high) = (0.20, 0.25);
        for _ in 0..60 {
            let middle = 0.5 * (low + high);
            if 20.0 * response(&make(middle), 0.239).log10() < -6.0 {
                low = middle;
            } else {
                high = middle;
            }
        }
        let mut taps = make(0.5 * (low + high));
        let sum: f64 = taps.iter().sum();
        for tap in &mut taps {
            *tap *= 1.0009 / sum;
        }
        taps
    })
}
