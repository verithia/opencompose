//! Real-time FFT analyser tap for the EQ panels.
//! 2048-point Hann FFT at 50 % overlap folded into 128 log bands; `push` allocates nothing.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::Fft;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Spectrum FFT size.
pub const SPECTRUM_FFT_SIZE: usize = 2048;
/// Samples between analyses.
pub const SPECTRUM_HOP: usize = 1024;
/// Display band count.
pub const SPECTRUM_BANDS: usize = 128;
/// Lowest displayed frequency.
pub const SPECTRUM_MIN_HZ: f64 = 20.0;
/// Highest displayed frequency.
pub const SPECTRUM_MAX_HZ: f64 = 20_000.0;
/// Band attack smoothing time.
pub const SPECTRUM_ATTACK_MS: f64 = 40.0;
/// Band release smoothing time.
pub const SPECTRUM_RELEASE_MS: f64 = 250.0;
/// Display floor in dBFS; quieter bands are clamped.
pub const SPECTRUM_FLOOR_DB: f32 = -96.0;

/// Node whose signal feeds the analyser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpectrumTarget {
    Track(usize),
    Bus(String),
    Master,
}

/// Log-band FFT analyser for the EQ panels.
pub struct SpectrumAnalyser {
    sample_rate: u32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    write: usize,
    since_hop: usize,
    scratch: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    /// Inclusive-exclusive FFT bin range for every display band.
    bin_ranges: Vec<(usize, usize)>,
    bands: [f32; SPECTRUM_BANDS],
    attack: f32,
    release: f32,
    analyses: u64,
}

impl SpectrumAnalyser {
    /// Prepare the window, FFT plan and band ranges.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(SPECTRUM_FFT_SIZE);
        let fft_scratch_len = fft.get_inplace_scratch_len();
        let window = (0..SPECTRUM_FFT_SIZE)
            .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f32 / SPECTRUM_FFT_SIZE as f32).cos())
            .collect();
        let bin_hz = sample_rate as f64 / SPECTRUM_FFT_SIZE as f64;
        let bins = SPECTRUM_FFT_SIZE / 2 + 1;
        let mut bin_ranges = Vec::with_capacity(SPECTRUM_BANDS);
        for band in 0..SPECTRUM_BANDS {
            let (low, high) = band_bounds(band);
            let mut first = ((low / bin_hz).ceil() as usize).max(1).min(bins - 1);
            let mut last = ((high / bin_hz).ceil() as usize).max(first + 1).min(bins);
            if last <= first {
                first = first.min(bins - 2);
                last = first + 1;
            }
            bin_ranges.push((first, last));
        }
        let hop_s = SPECTRUM_HOP as f64 / sample_rate as f64;
        let attack = 1.0 - (-hop_s / (SPECTRUM_ATTACK_MS * 0.001)).exp();
        let release = 1.0 - (-hop_s / (SPECTRUM_RELEASE_MS * 0.001)).exp();
        Self {
            sample_rate,
            fft,
            window,
            ring: vec![0.0; SPECTRUM_FFT_SIZE],
            write: 0,
            since_hop: 0,
            scratch: vec![Complex::new(0.0, 0.0); SPECTRUM_FFT_SIZE],
            fft_scratch: vec![Complex::new(0.0, 0.0); fft_scratch_len],
            bin_ranges,
            bands: [SPECTRUM_FLOOR_DB; SPECTRUM_BANDS],
            attack: attack as f32,
            release: release as f32,
            analyses: 0,
        }
    }

    /// Analysis sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Latest band levels in dBFS.
    pub fn bands(&self) -> &[f32; SPECTRUM_BANDS] {
        &self.bands
    }

    /// Completed analyses since reset.
    pub fn analyses(&self) -> u64 {
        self.analyses
    }

    /// Clear history, levels and the analysis counter.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.since_hop = 0;
        self.bands.fill(SPECTRUM_FLOOR_DB);
        self.analyses = 0;
    }

    /// Feed interleaved stereo frames; the left/right pair is averaged to mono.
    pub fn push_interleaved(&mut self, buffer: &[f32]) {
        for frame in buffer.chunks_exact(2) {
            self.ring[self.write] = (frame[0] + frame[1]) * 0.5;
            self.write = (self.write + 1) % SPECTRUM_FFT_SIZE;
            self.since_hop += 1;
            if self.since_hop == SPECTRUM_HOP {
                self.since_hop = 0;
                self.analyse();
            }
        }
    }

    fn analyse(&mut self) {
        for (index, slot) in self.scratch.iter_mut().enumerate() {
            let ring_index = (self.write + index) % SPECTRUM_FFT_SIZE;
            *slot = Complex::new(self.ring[ring_index] * self.window[index], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.scratch, &mut self.fft_scratch);
        let coherent_gain: f32 = self.window.iter().sum();
        let scale = 2.0 / (coherent_gain * coherent_gain);
        for (band, &(first, last)) in self.bin_ranges.iter().enumerate() {
            // Peak bin in the band: a windowed tone reads its own RMS level and
            // neighbouring bins do not sum the Hann main lobe twice.
            let power = self.scratch[first..last]
                .iter()
                .map(|bin| bin.norm_sqr())
                .fold(0.0f32, f32::max)
                * scale;
            let target = (10.0 * power.max(1e-12).log10()).max(SPECTRUM_FLOOR_DB);
            let coefficient = if target > self.bands[band] {
                self.attack
            } else {
                self.release
            };
            self.bands[band] += coefficient * (target - self.bands[band]);
        }
        self.analyses += 1;
    }
}

/// Lower and upper edge of display band `index`, in Hz.
pub fn band_bounds(index: usize) -> (f64, f64) {
    let ratio = SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ;
    let step = ratio.powf(1.0 / SPECTRUM_BANDS as f64);
    let low = SPECTRUM_MIN_HZ * step.powi(index as i32);
    let high = if index + 1 == SPECTRUM_BANDS {
        SPECTRUM_MAX_HZ
    } else {
        low * step
    };
    (low, high)
}

/// Geometric center of display band `index`, in Hz.
pub fn band_center(index: usize) -> f64 {
    let (low, high) = band_bounds(index);
    (low * high).sqrt()
}

/// Display band that contains `freq`, or the nearest end band outside the range.
pub fn band_index_for(freq: f64) -> usize {
    if freq <= SPECTRUM_MIN_HZ {
        return 0;
    }
    if freq >= SPECTRUM_MAX_HZ {
        return SPECTRUM_BANDS - 1;
    }
    let ratio = SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ;
    let position = (freq / SPECTRUM_MIN_HZ).ln() / ratio.ln() * SPECTRUM_BANDS as f64;
    (position.floor() as usize).min(SPECTRUM_BANDS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    fn tone(freq: f64, amplitude: f64, frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let value = (TAU * freq * frame as f64 / SR as f64).sin() as f32 * amplitude as f32;
            buffer[frame * 2] = value;
            buffer[frame * 2 + 1] = value;
        }
        buffer
    }

    #[test]
    fn display_bands_are_contiguous_from_20_hz_to_20_khz() {
        assert_eq!(band_bounds(0).0, SPECTRUM_MIN_HZ);
        assert_eq!(band_bounds(SPECTRUM_BANDS - 1).1, SPECTRUM_MAX_HZ);
        for index in 1..SPECTRUM_BANDS {
            assert!((band_bounds(index).0 - band_bounds(index - 1).1).abs() < 1e-9);
            assert!(band_center(index) > band_center(index - 1));
        }
        assert_eq!(band_index_for(20.0), 0);
        assert_eq!(band_index_for(20_000.0), SPECTRUM_BANDS - 1);
        let at_1k = band_index_for(1_000.0);
        let (low, high) = band_bounds(at_1k);
        assert!(low <= 1_000.0 && 1_000.0 < high);
    }

    #[test]
    fn a_tone_lands_in_the_band_that_contains_its_frequency() {
        let mut analyser = SpectrumAnalyser::new(SR);
        analyser.push_interleaved(&tone(1_000.0, 0.5, SR as usize * 2));
        let peak = analyser
            .bands()
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(peak, band_index_for(1_000.0), "peak band {peak}");
        let level = analyser.bands()[peak];
        assert!(
            (level + 9.03).abs() < 1.5,
            "0.5 amplitude tone read {level:.2} dBFS"
        );
        assert!(analyser.bands().iter().all(|value| value.is_finite()));
    }

    #[test]
    fn smoothing_is_deterministic_and_monotonic_on_steps() {
        let signal = tone(1_000.0, 0.5, SPECTRUM_HOP * 8);
        let mut first = SpectrumAnalyser::new(SR);
        let mut second = SpectrumAnalyser::new(SR);
        first.push_interleaved(&signal);
        second.push_interleaved(&signal);
        assert_eq!(first.bands(), second.bands());
        assert_eq!(first.analyses(), 8);

        let band = band_index_for(1_000.0);
        let mut rising = SpectrumAnalyser::new(SR);
        let mut previous = rising.bands()[band];
        for _ in 0..16 {
            rising.push_interleaved(&tone(1_000.0, 0.5, SPECTRUM_HOP));
            let current = rising.bands()[band];
            assert!(current >= previous, "rise was not monotonic");
            previous = current;
        }

        let mut falling = SpectrumAnalyser::new(SR);
        falling.push_interleaved(&tone(1_000.0, 0.5, SPECTRUM_HOP * 32));
        let mut previous = falling.bands()[band];
        for _ in 0..64 {
            falling.push_interleaved(&vec![0.0; SPECTRUM_HOP * 2]);
            let current = falling.bands()[band];
            assert!(current <= previous, "release was not monotonic");
            previous = current;
        }
        assert!(falling.bands()[band] < -60.0);
    }

    #[test]
    fn reset_returns_every_band_to_the_floor() {
        let mut analyser = SpectrumAnalyser::new(SR);
        analyser.push_interleaved(&tone(1_000.0, 0.5, SR as usize));
        analyser.reset();
        assert!(
            analyser
                .bands()
                .iter()
                .all(|value| *value == SPECTRUM_FLOOR_DB)
        );
        assert_eq!(analyser.analyses(), 0);
    }

    #[test]
    #[ignore = "manual CPU sanity"]
    fn spectrum_cpu_sanity() {
        let mut analyser = SpectrumAnalyser::new(SR);
        let buffer = tone(440.0, 0.5, SPECTRUM_HOP);
        let iterations = SR as usize * 10 / SPECTRUM_HOP;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            analyser.push_interleaved(&buffer);
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "spectrum: {:.3}x realtime, {:.2} us per analysis",
            (iterations * SPECTRUM_HOP) as f64 / SR as f64 / elapsed,
            elapsed * 1e6 / analyser.analyses() as f64
        );
    }
}
