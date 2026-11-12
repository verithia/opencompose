//! Mode-dependent multiband crossover.
//!
//! The split tree is progressive: split 1 separates band 1 from the rest, split
//! 2 separates band 2 from the rest, and so on. Every split carries a
//! complementary allpass for the bands below it, so the sum of the bands is a
//! cascade of allpasses and therefore flat in magnitude.
//!
//! Phase modes:
//! - `Minimum` uses the IIR splits directly; zero latency.
//! - `Dynamic` adds a program-dependent allpass blend to each compensation path.
//!   The blend falls to zero on stationary program, so the steady-state sum is
//!   the same as `Minimum`; on transients it bends the low-band group delay.
//! - `Linear` replaces the split with per-band linear-phase FIRs designed from
//!   the same magnitude shapes. The sum of the FIRs is an exact delta, so the
//!   band sum is flat, at the cost of group delay and pre-ringing.
//!
//! The IIR topology is our own: a Butterworth base chain squared (Linkwitz-Riley
//! alignment) for 12 dB/oct and steeper, and a single first-order section for
//! 6 dB/oct. The compensation allpass is the complementary sum of the same
//! sections, which reduces to a cascade of Butterworth-pole allpass sections.

use crate::fft::{Complex, Fft};
use std::f64::consts::PI;

/// Largest number of bands a crossover can split into.
pub const MAX_BANDS: usize = 4;
/// Lowest crossover frequency the design accepts.
pub const MIN_FREQ_HZ: f64 = 20.0;
/// Splits are clamped below this fraction of the sample rate.
const MAX_FREQ_RATIO: f64 = 0.45;

/// Linear-mode FIR length (odd, centred at `LINEAR_DELAY`).
pub const LINEAR_TAPS: usize = 2049;
/// Latency of the linear-phase FIR in samples.
pub const LINEAR_DELAY: usize = (LINEAR_TAPS - 1) / 2;
/// FFT size the linear splitter uses.
pub const LINEAR_FFT: usize = 4096;
/// Largest block the linear splitter accepts without re-planning.
pub const LINEAR_MAX_BLOCK: usize = LINEAR_FFT - LINEAR_TAPS + 1;
const DESIGN_FFT: usize = 16_384;
const LINEAR_KAISER_BETA: f64 = 10.0;

/// Program-dependent alignment constants for `Dynamic` mode.
const DYNAMIC_CORNER_HZ: f64 = 90.0;
const DYNAMIC_FAST_ATTACK_MS: f64 = 1.0;
const DYNAMIC_FAST_RELEASE_MS: f64 = 80.0;
const DYNAMIC_SLOW_ATTACK_MS: f64 = 80.0;
const DYNAMIC_SLOW_RELEASE_MS: f64 = 500.0;
/// Fast/slow energy ratio that maps to full alignment.
const DYNAMIC_SENSITIVITY_DB: f64 = 6.0;
/// Level changes below this fast/slow difference are left alone, so stationary
/// program never modulates the alignment phase.
const DYNAMIC_DEADBAND_DB: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How crossover bands are aligned in phase.
pub enum PhaseMode {
    Minimum,
    Dynamic,
    Linear,
}

impl PhaseMode {
    /// Every phase mode in selector order.
    pub const ALL: [Self; 3] = [Self::Minimum, Self::Dynamic, Self::Linear];

    /// Resolve the numeric selector stored in a patch.
    pub fn from_code(code: f64) -> Option<Self> {
        match code.round() as i32 {
            0 => Some(Self::Minimum),
            1 => Some(Self::Dynamic),
            2 => Some(Self::Linear),
            _ => None,
        }
    }

    /// The numeric selector stored in a patch.
    pub fn code(self) -> f64 {
        match self {
            Self::Minimum => 0.0,
            Self::Dynamic => 1.0,
            Self::Linear => 2.0,
        }
    }

    /// Whether this mode uses the linear-phase FIR path.
    pub fn is_linear(self) -> bool {
        self == Self::Linear
    }
}

/// Band split slope in dB per octave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slope {
    Oct6,
    Oct12,
    Oct24,
    Oct36,
    Oct48,
}

impl Slope {
    /// Every slope in selector order.
    pub const ALL: [Self; 5] = [
        Self::Oct6,
        Self::Oct12,
        Self::Oct24,
        Self::Oct36,
        Self::Oct48,
    ];

    /// Resolve the numeric selector stored in a patch.
    pub fn from_code(code: f64) -> Option<Self> {
        match code.round() as i32 {
            0 => Some(Self::Oct6),
            1 => Some(Self::Oct12),
            2 => Some(Self::Oct24),
            3 => Some(Self::Oct36),
            4 => Some(Self::Oct48),
            _ => None,
        }
    }

    /// The numeric selector stored in a patch.
    pub fn code(self) -> f64 {
        match self {
            Self::Oct6 => 0.0,
            Self::Oct12 => 1.0,
            Self::Oct24 => 2.0,
            Self::Oct36 => 3.0,
            Self::Oct48 => 4.0,
        }
    }

    /// Filter order in dB/oct divided by six.
    pub fn order(self) -> usize {
        match self {
            Self::Oct6 => 1,
            Self::Oct12 => 2,
            Self::Oct24 => 4,
            Self::Oct36 => 6,
            Self::Oct48 => 8,
        }
    }

    /// The slope as a signed dB-per-octave magnitude.
    pub fn db_per_octave(self) -> f64 {
        self.order() as f64 * 6.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    LowPass,
    HighPass,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SectionSpec {
    FirstOrder,
    Biquad { q: f64 },
}

/// One first- or second-order section, transposed direct form II.
#[derive(Debug, Clone, Copy, Default)]
struct Section {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Section {
    fn process(&mut self, channel: usize, input: f64) -> f64 {
        let output = self.b0 * input + self.z1[channel];
        self.z1[channel] = self.b1 * input - self.a1 * output + self.z2[channel];
        self.z2[channel] = self.b2 * input - self.a2 * output;
        output
    }

    fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }
}

const MAX_SECTIONS: usize = 8;

#[derive(Debug, Clone, Copy, Default)]
struct Chain {
    sections: [Section; MAX_SECTIONS],
    count: usize,
}

impl Chain {
    fn process(&mut self, channel: usize, input: f64) -> f64 {
        let mut value = input;
        for section in self.sections[..self.count].iter_mut() {
            value = section.process(channel, value);
        }
        value
    }

    fn reset(&mut self) {
        for section in &mut self.sections {
            section.reset();
        }
    }

    fn clear(&mut self) {
        self.count = 0;
        self.reset();
    }

    fn design(&mut self, freq: f64, order: usize, kind: FilterKind, squared: bool, sr: f64) {
        self.clear();
        let mut specs = [SectionSpec::FirstOrder; 4];
        let count = butterworth_specs(order, &mut specs);
        let copies = if squared { 2 } else { 1 };
        for _ in 0..copies {
            for spec in specs.iter().take(count) {
                let section = &mut self.sections[self.count];
                *section = Section::default();
                match *spec {
                    SectionSpec::FirstOrder => {
                        design_first_order(section, freq, kind, sr);
                    }
                    SectionSpec::Biquad { q } => {
                        design_biquad(section, freq, q, kind, sr);
                    }
                }
                self.count += 1;
            }
        }
    }

    fn design_allpass(&mut self, freq: f64, order: usize, sr: f64) {
        self.clear();
        let mut specs = [SectionSpec::FirstOrder; 4];
        let count = butterworth_specs(order, &mut specs);
        for spec in specs.iter().take(count) {
            let section = &mut self.sections[self.count];
            *section = Section::default();
            match *spec {
                SectionSpec::FirstOrder => design_first_order_allpass(section, freq, sr),
                SectionSpec::Biquad { q } => design_biquad_allpass(section, freq, q, sr),
            }
            self.count += 1;
        }
    }
}

/// Butterworth pole sections for orders 1..=4.
///
/// The Q values are `1 / (2 cos θ)` over the Butterworth pole angles, so no
/// tabulated coefficients enter the design.
fn butterworth_specs(order: usize, out: &mut [SectionSpec; 4]) -> usize {
    if order == 0 {
        return 0;
    }
    if order % 2 == 1 {
        out[0] = SectionSpec::FirstOrder;
        let pairs = (order - 1) / 2;
        for index in 0..pairs {
            let k = (index + 1) as f64;
            let q = 1.0 / (2.0 * (k * PI / order as f64).cos());
            out[index + 1] = SectionSpec::Biquad { q };
        }
        1 + pairs
    } else {
        let pairs = order / 2;
        for (index, slot) in out[..pairs].iter_mut().enumerate() {
            let angle = (2 * index + 1) as f64 * PI / (2.0 * order as f64);
            let q = 1.0 / (2.0 * angle.cos());
            *slot = SectionSpec::Biquad { q };
        }
        pairs
    }
}

fn design_first_order(section: &mut Section, freq: f64, kind: FilterKind, sr: f64) {
    let k = (PI * freq / sr).tan();
    let norm = 1.0 + k;
    match kind {
        FilterKind::LowPass => {
            section.b0 = k / norm;
            section.b1 = k / norm;
        }
        FilterKind::HighPass => {
            section.b0 = 1.0 / norm;
            section.b1 = -1.0 / norm;
        }
    }
    section.b2 = 0.0;
    section.a1 = (k - 1.0) / norm;
    section.a2 = 0.0;
}

fn design_biquad(section: &mut Section, freq: f64, q: f64, kind: FilterKind, sr: f64) {
    let k = (PI * freq / sr).tan();
    let norm = 1.0 + k / q + k * k;
    let (b0, b1, b2) = match kind {
        FilterKind::LowPass => (k * k / norm, 2.0 * k * k / norm, k * k / norm),
        FilterKind::HighPass => (1.0 / norm, -2.0 / norm, 1.0 / norm),
    };
    section.b0 = b0;
    section.b1 = b1;
    section.b2 = b2;
    section.a1 = 2.0 * (k * k - 1.0) / norm;
    section.a2 = (1.0 - k / q + k * k) / norm;
}

fn design_first_order_allpass(section: &mut Section, freq: f64, sr: f64) {
    let k = (PI * freq / sr).tan();
    let a1 = (k - 1.0) / (k + 1.0);
    section.b0 = a1;
    section.b1 = 1.0;
    section.b2 = 0.0;
    section.a1 = a1;
    section.a2 = 0.0;
}

fn design_biquad_allpass(section: &mut Section, freq: f64, q: f64, sr: f64) {
    let k = (PI * freq / sr).tan();
    let a = 1.0 / q;
    let norm = 1.0 + a * k + k * k;
    let a1 = 2.0 * (k * k - 1.0) / norm;
    let a2 = (1.0 - a * k + k * k) / norm;
    section.b0 = a2;
    section.b1 = a1;
    section.b2 = 1.0;
    section.a1 = a1;
    section.a2 = a2;
}

#[derive(Debug, Clone, Copy, Default)]
struct Split {
    low: Chain,
    high: Chain,
    high_sign: f64,
}

impl Split {
    fn configure(&mut self, freq: f64, slope: Slope, sr: f64) {
        let order = slope.order();
        if order == 1 {
            self.low.design(freq, 1, FilterKind::LowPass, false, sr);
            self.high.design(freq, 1, FilterKind::HighPass, false, sr);
            self.high_sign = 1.0;
        } else {
            let base = order / 2;
            self.low.design(freq, base, FilterKind::LowPass, true, sr);
            self.high.design(freq, base, FilterKind::HighPass, true, sr);
            self.high_sign = if base % 2 == 1 { -1.0 } else { 1.0 };
        }
        self.reset();
    }

    fn process(&mut self, left: f64, right: f64) -> ([f64; 2], [f64; 2]) {
        let low = [self.low.process(0, left), self.low.process(1, right)];
        let high = [
            self.high_sign * self.high.process(0, left),
            self.high_sign * self.high.process(1, right),
        ];
        (low, high)
    }

    fn reset(&mut self) {
        self.low.reset();
        self.high.reset();
    }
}

/// Fixed first-order allpass used as the dynamic alignment target.
#[derive(Debug, Clone, Copy, Default)]
struct DynamicStage {
    section: Section,
}

impl DynamicStage {
    fn configure(&mut self, sr: f64) {
        self.section = Section::default();
        design_first_order_allpass(&mut self.section, DYNAMIC_CORNER_HZ, sr);
    }

    fn process(&mut self, channel: usize, input: f64) -> f64 {
        self.section.process(channel, input)
    }

    fn reset(&mut self) {
        self.section.reset();
    }
}

/// Fast/slow peak tracker that returns a 0..1 transient control.
#[derive(Debug, Clone, Copy, Default)]
struct TransientControl {
    fast: f64,
    slow: f64,
    fast_attack: f64,
    fast_release: f64,
    slow_attack: f64,
    slow_release: f64,
}

impl TransientControl {
    fn configure(&mut self, sr: f64) {
        self.fast_attack = time_coefficient(DYNAMIC_FAST_ATTACK_MS, sr);
        self.fast_release = time_coefficient(DYNAMIC_FAST_RELEASE_MS, sr);
        self.slow_attack = time_coefficient(DYNAMIC_SLOW_ATTACK_MS, sr);
        self.slow_release = time_coefficient(DYNAMIC_SLOW_RELEASE_MS, sr);
        self.reset();
    }

    fn process(&mut self, left: f64, right: f64) -> f64 {
        // Mean-square energy: a steady periodic signal settles both stages to
        // the same value, so only level changes produce a control signal.
        let level = 0.5 * (left * left + right * right);
        self.fast = follow(self.fast, level, self.fast_attack, self.fast_release);
        self.slow = follow(self.slow, level, self.slow_attack, self.slow_release);
        if self.slow <= 1e-15 {
            return 0.0;
        }
        let difference_db = 10.0 * (self.fast / self.slow).log10();
        ((difference_db - DYNAMIC_DEADBAND_DB) / DYNAMIC_SENSITIVITY_DB).clamp(0.0, 1.0)
    }

    fn reset(&mut self) {
        self.fast = 0.0;
        self.slow = 0.0;
    }
}

fn follow(value: f64, target: f64, attack: f64, release: f64) -> f64 {
    let coefficient = if target > value { attack } else { release };
    coefficient * value + (1.0 - coefficient) * target
}

fn time_coefficient(ms: f64, sr: f64) -> f64 {
    (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp()
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1.0;
    loop {
        let factor = x / (2.0 * k);
        term *= factor * factor;
        sum += term;
        if term <= 1e-14 * sum {
            return sum;
        }
        k += 1.0;
    }
}

fn kaiser_window(length: usize, beta: f64) -> Vec<f64> {
    let denominator = bessel_i0(beta);
    let half = (length - 1) as f64 / 2.0;
    (0..length)
        .map(|index| {
            let x = (index as f64 - half) / half;
            bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / denominator
        })
        .collect()
}

struct LinearSplitter {
    sample_rate: f64,
    bands: usize,
    freqs: [f64; MAX_BANDS - 1],
    slope: Slope,
    fft: Fft,
    design_fft: Fft,
    input: Vec<Complex>,
    scratch: Vec<Complex>,
    h_freq: Vec<Complex>,
    overlap: Vec<f64>,
    design_time: Vec<Complex>,
    design_mask: Vec<f64>,
    window: Vec<f64>,
}

impl LinearSplitter {
    fn new(sample_rate: u32) -> Self {
        let mut splitter = Self {
            sample_rate: sample_rate as f64,
            bands: 2,
            freqs: [1_000.0; MAX_BANDS - 1],
            slope: Slope::Oct24,
            fft: Fft::new(LINEAR_FFT),
            design_fft: Fft::new(DESIGN_FFT),
            input: vec![Complex::default(); 2 * LINEAR_FFT],
            scratch: vec![Complex::default(); LINEAR_FFT],
            h_freq: vec![Complex::default(); MAX_BANDS * LINEAR_FFT],
            overlap: vec![0.0; MAX_BANDS * 2 * (LINEAR_TAPS - 1)],
            design_time: vec![Complex::default(); DESIGN_FFT],
            design_mask: vec![0.0; DESIGN_FFT / 2 + 1],
            window: kaiser_window(LINEAR_TAPS, LINEAR_KAISER_BETA),
        };
        splitter.configure(2, [1_000.0; MAX_BANDS - 1], Slope::Oct24);
        splitter
    }

    fn configure(&mut self, bands: usize, freqs: [f64; MAX_BANDS - 1], slope: Slope) {
        self.bands = bands.clamp(2, MAX_BANDS);
        self.freqs = freqs;
        self.slope = slope;
        for band in 0..MAX_BANDS {
            self.design_band(band);
        }
        self.reset_state();
    }

    fn design_band(&mut self, band: usize) {
        let offset = band * LINEAR_FFT;
        if band >= self.bands {
            self.h_freq[offset..offset + LINEAR_FFT].fill(Complex::default());
            return;
        }
        let order = self.slope.order() as f64;
        for index in 0..=DESIGN_FFT / 2 {
            let freq = index as f64 * self.sample_rate / DESIGN_FFT as f64;
            let mut gain = 1.0;
            for split in 0..band {
                gain *= high_factor(freq, self.freqs[split], order);
            }
            if band + 1 < self.bands {
                gain *= low_factor(freq, self.freqs[band], order);
            }
            self.design_mask[index] = gain;
            self.design_time[index] = Complex { re: gain, im: 0.0 };
            if index > 0 && index < DESIGN_FFT / 2 {
                self.design_time[DESIGN_FFT - index] = Complex { re: gain, im: 0.0 };
            }
        }
        self.design_fft.inverse(&mut self.design_time);
        for value in self.scratch.iter_mut() {
            *value = Complex::default();
        }
        for tap in 0..LINEAR_TAPS {
            let index =
                (tap as isize - LINEAR_DELAY as isize).rem_euclid(DESIGN_FFT as isize) as usize;
            self.scratch[tap].re = self.design_time[index].re * self.window[tap];
        }
        self.fft.forward(&mut self.scratch);
        self.h_freq[offset..offset + LINEAR_FFT].copy_from_slice(&self.scratch);
    }

    fn reset_state(&mut self) {
        self.input.fill(Complex::default());
        self.scratch.fill(Complex::default());
        self.overlap.fill(0.0);
    }

    /// Split one block. `out` uses a `[band][channel][LINEAR_MAX_BLOCK]` layout.
    fn split(&mut self, left: &[f64], right: &[f64], out: &mut [f64], frames: usize) {
        if frames == 0 {
            return;
        }
        debug_assert!(frames <= LINEAR_MAX_BLOCK);
        for channel in 0..2 {
            let input = if channel == 0 { left } else { right };
            let input_offset = channel * LINEAR_FFT;
            for value in self.input[input_offset..input_offset + LINEAR_FFT].iter_mut() {
                *value = Complex::default();
            }
            for (slot, &value) in self.input[input_offset..input_offset + frames]
                .iter_mut()
                .zip(input[..frames].iter())
            {
                slot.re = value;
            }
            self.fft
                .forward(&mut self.input[input_offset..input_offset + LINEAR_FFT]);
            for band in 0..self.bands {
                let h_offset = band * LINEAR_FFT;
                for index in 0..LINEAR_FFT {
                    let sample = self.input[input_offset + index];
                    let response = self.h_freq[h_offset + index];
                    self.scratch[index] = sample.times(response);
                }
                self.fft.inverse(&mut self.scratch);
                let overlap_offset = (band * 2 + channel) * (LINEAR_TAPS - 1);
                let out_offset = (band * 2 + channel) * LINEAR_MAX_BLOCK;
                for frame in 0..frames {
                    out[out_offset + frame] =
                        self.scratch[frame].re + self.overlap[overlap_offset + frame];
                }
                for tap in 0..(LINEAR_TAPS - 1) {
                    let convolution = self.scratch[frames + tap].re;
                    let previous = if frames + tap < LINEAR_TAPS - 1 {
                        self.overlap[overlap_offset + frames + tap]
                    } else {
                        0.0
                    };
                    self.overlap[overlap_offset + tap] = convolution + previous;
                }
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        LINEAR_DELAY as u32
    }
}

fn low_factor(freq: f64, corner: f64, order: f64) -> f64 {
    if freq <= 0.0 {
        return 1.0;
    }
    let ratio = (freq / corner).powf(order);
    1.0 / (1.0 + ratio)
}

fn high_factor(freq: f64, corner: f64, order: f64) -> f64 {
    if freq <= 0.0 {
        return 0.0;
    }
    let ratio = (freq / corner).powf(order);
    ratio / (1.0 + ratio)
}

/// A two-to-four band splitter with minimum, dynamic or linear phase alignment.
pub struct MultibandCrossover {
    sample_rate: f64,
    bands: usize,
    freqs: [f64; MAX_BANDS - 1],
    slope: Slope,
    phase: PhaseMode,
    splits: [Split; MAX_BANDS - 1],
    allpass: [[Chain; MAX_BANDS]; MAX_BANDS - 1],
    dynamic_stages: [[DynamicStage; MAX_BANDS]; MAX_BANDS - 1],
    transient: TransientControl,
    linear: LinearSplitter,
}

impl MultibandCrossover {
    /// A two-band 1 kHz 24 dB/oct minimum-phase splitter.
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let mut crossover = Self {
            sample_rate: sr,
            bands: 2,
            freqs: [1_000.0; MAX_BANDS - 1],
            slope: Slope::Oct24,
            phase: PhaseMode::Minimum,
            splits: [Split::default(); MAX_BANDS - 1],
            allpass: [[Chain::default(); MAX_BANDS]; MAX_BANDS - 1],
            dynamic_stages: [[DynamicStage::default(); MAX_BANDS]; MAX_BANDS - 1],
            transient: TransientControl::default(),
            linear: LinearSplitter::new(sample_rate),
        };
        crossover.transient.configure(sr);
        for row in &mut crossover.dynamic_stages {
            for stage in row {
                stage.configure(sr);
            }
        }
        crossover.configure(
            2,
            [1_000.0; MAX_BANDS - 1],
            Slope::Oct24,
            PhaseMode::Minimum,
        );
        crossover
    }

    /// Set band count, ascending crossover frequencies, slope and phase mode;
    /// values are clamped and sorted.
    pub fn configure(
        &mut self,
        bands: usize,
        freqs: [f64; MAX_BANDS - 1],
        slope: Slope,
        phase: PhaseMode,
    ) {
        self.bands = bands.clamp(2, MAX_BANDS);
        let mut sorted = freqs;
        for freq in &mut sorted {
            *freq = freq.clamp(MIN_FREQ_HZ, self.sample_rate * MAX_FREQ_RATIO);
        }
        let active = self.bands - 1;
        sorted[..active].sort_by(f64::total_cmp);
        self.freqs = sorted;
        self.slope = slope;
        self.phase = phase;

        for split in 0..active {
            self.splits[split].configure(self.freqs[split], slope, self.sample_rate);
        }
        for split in 0..MAX_BANDS - 1 {
            for band in 0..MAX_BANDS {
                self.allpass[split][band].clear();
            }
        }
        let allpass_order = if slope.order() == 1 {
            0
        } else {
            slope.order() / 2
        };
        for split in 0..active {
            for band in 0..split {
                self.allpass[split][band].design_allpass(
                    self.freqs[split],
                    allpass_order,
                    self.sample_rate,
                );
            }
        }
        for row in &mut self.dynamic_stages {
            for stage in row {
                stage.reset();
            }
        }
        self.transient.reset();
        if phase.is_linear() {
            self.linear.configure(self.bands, self.freqs, self.slope);
        } else {
            self.linear.reset_state();
        }
    }

    /// Process one stereo frame with the IIR split (Minimum or Dynamic).
    pub fn process_frame(&mut self, left: f64, right: f64) -> [[f64; 2]; MAX_BANDS] {
        let mut bands = [[0.0; 2]; MAX_BANDS];
        let mut lows = [[0.0; 2]; MAX_BANDS - 1];
        let mut stage = [left, right];
        let split_count = self.bands - 1;
        for (split, low_slot) in self.splits[..split_count].iter_mut().zip(lows.iter_mut()) {
            let (low, high) = split.process(stage[0], stage[1]);
            *low_slot = low;
            stage = high;
        }
        let control = if self.phase == PhaseMode::Dynamic {
            self.transient.process(left, right)
        } else {
            0.0
        };
        for band in 0..self.bands {
            let mut frame = if band < self.bands - 1 {
                lows[band]
            } else {
                stage
            };
            for split in (band + 1)..(self.bands - 1) {
                if self.allpass[split][band].count > 0 {
                    frame = [
                        self.allpass[split][band].process(0, frame[0]),
                        self.allpass[split][band].process(1, frame[1]),
                    ];
                }
                if self.phase == PhaseMode::Dynamic {
                    let aligned = [
                        self.dynamic_stages[split][band].process(0, frame[0]),
                        self.dynamic_stages[split][band].process(1, frame[1]),
                    ];
                    frame = [
                        frame[0] + control * (aligned[0] - frame[0]),
                        frame[1] + control * (aligned[1] - frame[1]),
                    ];
                }
            }
            bands[band] = frame;
        }
        bands
    }

    /// Split one block for `Linear` mode. `out` uses a
    /// `[band][channel][LINEAR_MAX_BLOCK]` layout.
    pub fn linear_split(&mut self, left: &[f64], right: &[f64], out: &mut [f64], frames: usize) {
        self.linear.split(left, right, out, frames);
    }

    /// Reported latency in samples, zero unless the phase mode is linear.
    pub fn latency_samples(&self) -> u32 {
        if self.phase.is_linear() {
            self.linear.latency_samples()
        } else {
            0
        }
    }

    /// Clear every split, all-pass stage and linear buffer.
    pub fn reset(&mut self) {
        for split in &mut self.splits {
            split.reset();
        }
        for row in &mut self.allpass {
            for chain in row {
                chain.reset();
            }
        }
        for row in &mut self.dynamic_stages {
            for stage in row {
                stage.reset();
            }
        }
        self.transient.reset();
        self.linear.reset_state();
    }

    /// Configured band count.
    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Configured phase mode.
    pub fn phase(&self) -> PhaseMode {
        self.phase
    }

    /// Configured split slope.
    pub fn slope(&self) -> Slope {
        self.slope
    }

    /// Configured crossover frequencies, ascending.
    pub fn freqs(&self) -> [f64; MAX_BANDS - 1] {
        self.freqs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amplitude: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|frame| {
                (std::f64::consts::TAU * freq * frame as f64 / SR as f64).sin() * amplitude
            })
            .collect()
    }

    fn rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn level_db(samples: &[f64], start: usize) -> f64 {
        20.0 * rms(&samples[start..]).log10()
    }

    fn configured(bands: usize, slope: Slope, phase: PhaseMode) -> MultibandCrossover {
        let mut crossover = MultibandCrossover::new(SR);
        crossover.configure(bands, [250.0, 1_000.0, 6_000.0], slope, phase);
        crossover
    }

    fn iir_band(
        crossover: &mut MultibandCrossover,
        freq: f64,
        amplitude: f64,
        frames: usize,
        band: usize,
    ) -> Vec<f64> {
        let input = sine(freq, amplitude, frames);
        input
            .iter()
            .map(|sample| {
                let split = crossover.process_frame(*sample, *sample);
                split[band][0]
            })
            .collect()
    }

    fn linear_band(
        crossover: &mut MultibandCrossover,
        freq: f64,
        amplitude: f64,
        frames: usize,
        band: usize,
    ) -> Vec<f64> {
        let input = sine(freq, amplitude, frames);
        let mut output = vec![0.0; frames];
        let mut buffer = vec![0.0; MAX_BANDS * 2 * LINEAR_MAX_BLOCK];
        let mut offset = 0;
        while offset < frames {
            let block = (frames - offset).min(LINEAR_MAX_BLOCK);
            crossover.linear_split(
                &input[offset..offset + block],
                &input[offset..offset + block],
                &mut buffer,
                block,
            );
            for frame in 0..block {
                output[offset + frame] = buffer[(band * 2) * LINEAR_MAX_BLOCK + frame];
            }
            offset += block;
        }
        output
    }

    fn band(
        crossover: &mut MultibandCrossover,
        freq: f64,
        amplitude: f64,
        frames: usize,
        band: usize,
    ) -> Vec<f64> {
        if crossover.phase().is_linear() {
            linear_band(crossover, freq, amplitude, frames, band)
        } else {
            iir_band(crossover, freq, amplitude, frames, band)
        }
    }

    fn sum_bands(
        crossover: &mut MultibandCrossover,
        freq: f64,
        amplitude: f64,
        frames: usize,
    ) -> Vec<f64> {
        let bands = crossover.bands();
        let mut output = vec![0.0; frames];
        for index in 0..bands {
            let band = band(crossover, freq, amplitude, frames, index);
            for (target, value) in output.iter_mut().zip(band) {
                *target += value;
            }
        }
        output
    }

    #[test]
    fn crossover_sums_flat_for_every_slope_and_phase_mode() {
        let frequencies = [60.0, 250.0, 900.0, 2_500.0, 9_000.0, 16_000.0];
        let mut worst: f64 = 0.0;
        for phase in PhaseMode::ALL {
            for slope in Slope::ALL {
                let mut crossover = configured(3, slope, phase);
                for freq in frequencies {
                    let frames = if phase == PhaseMode::Dynamic {
                        72_000
                    } else {
                        48_000
                    };
                    let input = sine(freq, 0.2, frames);
                    let sum = sum_bands(&mut crossover, freq, 0.2, frames);
                    let latency = crossover.latency_samples() as usize;
                    let start = frames / 2;
                    let deviation = level_db(&sum, start + latency) - level_db(&input, start);
                    worst = worst.max(deviation.abs());
                    assert!(
                        deviation.abs() <= 0.05,
                        "{phase:?} {slope:?} at {freq} Hz summed to {deviation} dB"
                    );
                }
            }
        }
        println!("crossover flatness: worst deviation {worst:.5} dB");
    }

    #[test]
    fn slope_matches_the_nominal_value_for_12_24_and_48() {
        for phase in PhaseMode::ALL {
            for (slope, low, high) in [
                (Slope::Oct12, 1_000.0, 2_000.0),
                (Slope::Oct24, 2_000.0, 4_000.0),
                (Slope::Oct48, 1_500.0, 3_000.0),
            ] {
                let mut crossover = configured(2, slope, phase);
                let frames = if phase == PhaseMode::Dynamic {
                    72_000
                } else {
                    48_000
                };
                let low_db = level_db(&band(&mut crossover, low, 0.25, frames, 0), frames / 2);
                let high_db = level_db(&band(&mut crossover, high, 0.25, frames, 0), frames / 2);
                let measured = low_db - high_db;
                let nominal = slope.db_per_octave();
                println!(
                    "slope {phase:?} {:?}: {measured:.3} dB/oct (nominal {nominal})",
                    slope
                );
                assert!(
                    (measured - nominal).abs() <= 1.0,
                    "{phase:?} {slope:?} measured {measured} dB/oct, nominal {nominal}"
                );
            }
        }
    }

    #[test]
    fn linear_sum_is_the_delayed_input() {
        let mut crossover = configured(4, Slope::Oct48, PhaseMode::Linear);
        let frames = 24_000;
        let input = sine(700.0, 0.4, frames);
        let mut sum = vec![0.0; frames];
        let mut buffer = vec![0.0; MAX_BANDS * 2 * LINEAR_MAX_BLOCK];
        let mut offset = 0;
        while offset < frames {
            let block = (frames - offset).min(LINEAR_MAX_BLOCK);
            crossover.linear_split(
                &input[offset..offset + block],
                &input[offset..offset + block],
                &mut buffer,
                block,
            );
            for frame in 0..block {
                for band in 0..4 {
                    sum[offset + frame] += buffer[(band * 2) * LINEAR_MAX_BLOCK + frame];
                }
            }
            offset += block;
        }
        let latency = LINEAR_DELAY;
        for frame in latency..frames {
            let error = (sum[frame] - input[frame - latency]).abs();
            assert!(
                error < 1e-9,
                "reconstruction error {error} at frame {frame}"
            );
        }
    }

    #[test]
    fn latency_is_per_phase_mode() {
        for phase in PhaseMode::ALL {
            let crossover = configured(2, Slope::Oct24, phase);
            let expected = if phase == PhaseMode::Linear {
                LINEAR_DELAY as u32
            } else {
                0
            };
            assert_eq!(crossover.latency_samples(), expected);
        }
    }

    #[test]
    fn block_splitting_does_not_change_the_linear_result() {
        let input = sine(330.0, 0.3, 20_000);
        let mut single = configured(3, Slope::Oct24, PhaseMode::Linear);
        let mut one_shot = vec![0.0; input.len()];
        let mut buffer = vec![0.0; MAX_BANDS * 2 * LINEAR_MAX_BLOCK];
        let mut offset = 0;
        while offset < input.len() {
            let block = (input.len() - offset).min(LINEAR_MAX_BLOCK);
            single.linear_split(
                &input[offset..offset + block],
                &input[offset..offset + block],
                &mut buffer,
                block,
            );
            for frame in 0..block {
                for band in 0..3 {
                    one_shot[offset + frame] += buffer[(band * 2) * LINEAR_MAX_BLOCK + frame];
                }
            }
            offset += block;
        }

        let mut piecewise = configured(3, Slope::Oct24, PhaseMode::Linear);
        let mut pieces = vec![0.0; input.len()];
        let mut offset = 0;
        let sizes = [37usize, 500, 1_000, 1_024];
        let mut index = 0;
        while offset < input.len() {
            let block = sizes[index % sizes.len()];
            index += 1;
            let end = (offset + block).min(input.len());
            let count = end - offset;
            piecewise.linear_split(&input[offset..end], &input[offset..end], &mut buffer, count);
            for frame in 0..count {
                for band in 0..3 {
                    pieces[offset + frame] += buffer[(band * 2) * LINEAR_MAX_BLOCK + frame];
                }
            }
            offset = end;
        }
        for frame in 0..input.len() {
            assert!(
                (one_shot[frame] - pieces[frame]).abs() < 1e-9,
                "block-size dependence at frame {frame}"
            );
        }
    }
}
