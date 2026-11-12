//! Windowed-sinc sample-rate conversion with a selectable quality rung.
//!
//! Every rung is a Kaiser-windowed sinc kernel. The window parameter fixes the
//! stopband attenuation, `A = 8.7 + beta / 0.1102` dB, and Kaiser's length
//! estimate fixes the transition width, `df = (A - 8) / (14.36 * taps)` cycles
//! per source sample. The ladder multiplies the tap count by 1.5 from one rung
//! to the next and scales `beta` with the square root of the taps, so each step
//! raises the attenuation and narrows the transition without a jump in cost.
//!
//! Kernel coefficients are evaluated once per distinct delay fraction and
//! reused, so an output frame costs one multiply-add per tap rather than one
//! Bessel evaluation per tap.
//!
//! Runs on the offline and media-loading paths; never in the audio callback.

use std::collections::HashMap;
use std::f64::consts::PI;

use oc_dsl::ast::Project;

/// Rungs in the quality ladder.
pub const STEPS: usize = 8;
/// Tap-count ratio between adjacent rungs.
const TAP_RATIO: f64 = 1.5;
/// Half-width of the rung that preserves the pre-ladder converter.
const PRESERVED_HALF_TAPS: usize = 24;
/// Kaiser parameter of that rung.
const PRESERVED_BETA: f64 = 8.6;
/// Rung index of the preserved kernel.
const PRESERVED_STEP: u8 = 4;
/// Downsampling cutoff as a fraction of the target Nyquist.
const CUTOFF_GUARD: f64 = 0.97;
/// Delay fractions cached per conversion; past this the kernel is evaluated per
/// frame instead (rate pairs whose phase cycle never repeats cheaply).
const MAX_TABLE_ENTRIES: usize = 4096;

/// One rung: tap count, window parameter and cutoff policy.
#[derive(Debug, Clone, Copy)]
struct Rung {
    /// Half the tap count, in source frames.
    half_taps: isize,
    /// Kaiser window parameter.
    beta: f64,
    /// Downsampling cutoff as a fraction of the target Nyquist.
    guard: f64,
}

impl Rung {
    fn of(quality: ResampleQuality) -> Self {
        let exponent = i32::from(quality.step()) - i32::from(PRESERVED_STEP);
        let half_taps = (PRESERVED_HALF_TAPS as f64 * TAP_RATIO.powi(exponent)).round() as isize;
        let taps = (2 * half_taps) as f64;
        let preserved_taps = (2 * PRESERVED_HALF_TAPS) as f64;
        Self {
            half_taps,
            beta: PRESERVED_BETA * (taps / preserved_taps).sqrt(),
            guard: CUTOFF_GUARD,
        }
    }

    fn taps(self) -> usize {
        (2 * self.half_taps) as usize
    }
}

/// One step of the resampling quality ladder, from 0 (fastest) to 7 (best).
///
/// The render and live paths choose independently: the render default keeps the
/// pre-ladder kernel so existing exports stay byte-identical, while the live
/// default sits on a cheaper mid rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResampleQuality(u8);

impl ResampleQuality {
    /// Every rung, in ascending quality order.
    pub const ALL: [Self; STEPS] = [
        Self(0),
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
    ];
    /// Cheapest rung.
    pub const FASTEST: Self = Self(0);
    /// Most accurate rung.
    pub const BEST: Self = Self(7);
    /// Rung whose output matches the pre-ladder converter bit for bit.
    pub const PRESERVED: Self = Self(PRESERVED_STEP);
    /// Offline render default; the preserved rung.
    pub const DEFAULT_RENDER: Self = Self::PRESERVED;
    /// Live playback default; the mid rung, cheap enough for preview.
    pub const DEFAULT_LIVE: Self = Self(3);

    /// Rung for a step index, or `None` past the end of the ladder.
    pub fn from_step(step: u8) -> Option<Self> {
        (usize::from(step) < STEPS).then_some(Self(step))
    }

    /// Zero-based resample step.
    pub fn step(self) -> u8 {
        self.0
    }

    /// Taps per output frame.
    pub fn taps(self) -> usize {
        Rung::of(self).taps()
    }

    /// Kaiser window parameter.
    pub fn beta(self) -> f64 {
        Rung::of(self).beta
    }

    /// Stopband attenuation implied by the window parameter, in dB.
    pub fn stopband_db(self) -> f64 {
        8.7 + self.beta() / 0.1102
    }

    /// Transition width in cycles per source sample.
    pub fn transition_width(self) -> f64 {
        (self.stopband_db() - 8.0) / (14.36 * self.taps() as f64)
    }
}

impl std::fmt::Display for ResampleQuality {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "step {} of {} ({} taps, {:.0} dB stopband)",
            self.step(),
            STEPS - 1,
            self.taps(),
            self.stopband_db()
        )
    }
}

/// Render quality from the project's export settings, else the render default.
pub fn render_quality(project: &Project) -> ResampleQuality {
    project
        .properties
        .export
        .and_then(|export| export.resample)
        .and_then(ResampleQuality::from_step)
        .unwrap_or(ResampleQuality::DEFAULT_RENDER)
}

/// Live playback quality from the project, else the live default.
pub fn live_quality(project: &Project) -> ResampleQuality {
    project
        .properties
        .live_resample
        .and_then(ResampleQuality::from_step)
        .unwrap_or(ResampleQuality::DEFAULT_LIVE)
}

/// Output frame count of one conversion; zero input stays zero.
pub fn converted_frames(frames: usize, source_rate: u32, target_rate: u32) -> usize {
    if frames == 0 || source_rate == 0 || target_rate == 0 {
        return frames;
    }
    ((frames as f64) * f64::from(target_rate) / f64::from(source_rate))
        .round()
        .max(1.0) as usize
}

/// Windowed-sinc resampling at one quality rung. Downsampling moves the cutoff
/// below the destination Nyquist so out-of-band content is filtered instead of
/// aliased; upsampling keeps the source Nyquist.
pub fn resample(
    samples: &[f32],
    source_rate: u32,
    target_rate: u32,
    quality: ResampleQuality,
) -> Result<Vec<f32>, String> {
    resample_cancellable(
        samples,
        source_rate,
        target_rate,
        quality,
        Default::default(),
    )
}

pub(crate) fn resample_cancellable(
    samples: &[f32],
    source_rate: u32,
    target_rate: u32,
    quality: ResampleQuality,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    if source_rate == 0 || target_rate == 0 {
        return Err("resample rates must be positive".into());
    }
    if !samples.len().is_multiple_of(2) {
        return Err("resample input must be interleaved stereo".into());
    }
    let output_frames = converted_frames(samples.len() / 2, source_rate, target_rate);
    cancellation.check()?;
    if source_rate == target_rate {
        return Ok(samples.to_vec());
    }
    resample_slice(
        ResampleSource {
            samples,
            start: 0,
            frames: samples.len() / 2,
            rate: source_rate,
        },
        target_rate,
        quality,
        0..output_frames,
        cancellation,
    )
}

pub(crate) fn resample_pcm_window(
    pcm: &crate::clip_pcm::ClipPcm,
    source_rate: u32,
    target_rate: u32,
    quality: ResampleQuality,
    frames: std::ops::Range<usize>,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    cancellation.check()?;
    if source_rate == target_rate || frames.is_empty() {
        return pcm.window(frames);
    }
    if let crate::clip_pcm::ClipPcm::Resident(samples) = pcm {
        return resample_slice(
            ResampleSource {
                samples,
                start: 0,
                frames: pcm.frames(),
                rate: source_rate,
            },
            target_rate,
            quality,
            frames,
            cancellation,
        );
    }
    resample_read_window(
        pcm.frames(),
        source_rate,
        target_rate,
        quality,
        frames,
        cancellation,
        |range| pcm.window(range),
    )
}

/// Read just the input frames that can contribute to this absolute output range.
pub(crate) fn resample_read_window(
    source_frames: usize,
    source_rate: u32,
    target_rate: u32,
    quality: ResampleQuality,
    frames: std::ops::Range<usize>,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
    mut read: impl FnMut(std::ops::Range<usize>) -> Result<Vec<f32>, String>,
) -> Result<Vec<f32>, String> {
    cancellation.check()?;
    if source_rate == target_rate || frames.is_empty() {
        return read(frames);
    }
    let centre = |frame: usize| {
        (frame as f64 * f64::from(source_rate) / f64::from(target_rate)).floor() as usize
    };
    let radius = Rung::of(quality).half_taps as usize;
    let last_frame = source_frames - 1;
    let start = centre(frames.start).min(last_frame).saturating_sub(radius);
    let end = centre(frames.end - 1)
        .saturating_add(radius + 1)
        .min(source_frames);
    let source = read(start..end)?;
    resample_slice(
        ResampleSource {
            samples: &source,
            start,
            frames: source_frames,
            rate: source_rate,
        },
        target_rate,
        quality,
        frames,
        cancellation,
    )
}

struct ResampleSource<'a> {
    samples: &'a [f32],
    start: usize,
    frames: usize,
    rate: u32,
}

fn resample_slice(
    source: ResampleSource<'_>,
    target_rate: u32,
    quality: ResampleQuality,
    frames: std::ops::Range<usize>,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    let ResampleSource {
        samples,
        start: source_start,
        frames: source_frames,
        rate: source_rate,
    } = source;
    let ratio = f64::from(target_rate) / f64::from(source_rate);
    let rung = Rung::of(quality);
    let cutoff = if ratio < 1.0 { ratio * rung.guard } else { 1.0 };
    let beta_window = bessel_i0(rung.beta);
    let mut table = CoefficientTable {
        entries: HashMap::new(),
        half_taps: rung.half_taps,
        beta: rung.beta,
        beta_window,
        cutoff,
    };
    let mut scratch = Vec::new();
    let mut output = Vec::with_capacity(frames.len() * 2);

    for frame in frames {
        if frame.is_multiple_of(1024) {
            cancellation.check()?;
        }
        let position = frame as f64 * f64::from(source_rate) / f64::from(target_rate);
        let centre = position.floor() as isize;
        let fraction = position - centre as f64;
        let weights = table.weights(fraction, &mut scratch);
        let mut left = 0.0_f64;
        let mut right = 0.0_f64;
        let mut weight_sum = 0.0_f64;
        for (index, weight) in weights.iter().enumerate() {
            let tap = 1 - rung.half_taps + index as isize;
            let source =
                (centre + tap).clamp(0, source_frames as isize - 1) as usize - source_start;
            left += f64::from(samples[source * 2]) * weight;
            right += f64::from(samples[source * 2 + 1]) * weight;
            weight_sum += *weight;
        }
        if weight_sum.abs() > 1e-12 {
            left /= weight_sum;
            right /= weight_sum;
        }
        output.push(left as f32);
        output.push(right as f32);
    }
    Ok(output)
}

/// Kernels keyed by the exact delay fraction they were built for.
struct CoefficientTable {
    entries: HashMap<u64, Box<[f64]>>,
    half_taps: isize,
    beta: f64,
    beta_window: f64,
    cutoff: f64,
}

impl CoefficientTable {
    fn weights<'a>(&'a mut self, fraction: f64, scratch: &'a mut Vec<f64>) -> &'a [f64] {
        let key = fraction.to_bits();
        if !self.entries.contains_key(&key) {
            if self.entries.len() >= MAX_TABLE_ENTRIES {
                build_weights(
                    scratch,
                    fraction,
                    self.half_taps,
                    self.beta,
                    self.beta_window,
                    self.cutoff,
                );
                return scratch.as_slice();
            }
            let mut weights = Vec::with_capacity((2 * self.half_taps) as usize);
            build_weights(
                &mut weights,
                fraction,
                self.half_taps,
                self.beta,
                self.beta_window,
                self.cutoff,
            );
            self.entries.insert(key, weights.into_boxed_slice());
        }
        &self.entries[&key][..]
    }
}

/// Coefficients for one delay fraction, in ascending tap order.
fn build_weights(
    weights: &mut Vec<f64>,
    fraction: f64,
    half_taps: isize,
    beta: f64,
    beta_window: f64,
    cutoff: f64,
) {
    weights.clear();
    for tap in 1 - half_taps..=half_taps {
        let distance = (tap as f64 - fraction) / half_taps as f64;
        if distance.abs() >= 1.0 {
            weights.push(0.0);
            continue;
        }
        let window = bessel_i0(beta * (1.0 - distance * distance).sqrt()) / beta_window;
        let weight = cutoff * sinc(cutoff * (tap as f64 - fraction)) * window;
        weights.push(weight);
    }
}

fn sinc(value: f64) -> f64 {
    if value.abs() < 1e-12 {
        1.0
    } else {
        let angle = PI * value;
        angle.sin() / angle
    }
}

fn bessel_i0(value: f64) -> f64 {
    let half = value / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for order in 1..=48 {
        let scale = half / f64::from(order);
        term *= scale * scale;
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

    /// The pre-ladder converter, kept here as the bit-identity reference.
    fn legacy_resample(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
        const HALF_TAPS: isize = 24;
        const KAISER_BETA: f64 = 8.6;
        if source_rate == target_rate {
            return samples.to_vec();
        }
        let source_frames = samples.len() / 2;
        if source_frames == 0 {
            return Vec::new();
        }
        let output_frames = ((source_frames as f64) * f64::from(target_rate)
            / f64::from(source_rate))
        .round()
        .max(1.0) as usize;
        let ratio = f64::from(target_rate) / f64::from(source_rate);
        let cutoff = if ratio < 1.0 { ratio * 0.97 } else { 1.0 };
        let beta_window = bessel_i0(KAISER_BETA);
        let mut output = Vec::with_capacity(output_frames * 2);
        for frame in 0..output_frames {
            let position = frame as f64 * f64::from(source_rate) / f64::from(target_rate);
            let center = position.floor() as isize;
            let fraction = position - center as f64;
            let mut left = 0.0_f64;
            let mut right = 0.0_f64;
            let mut weight_sum = 0.0_f64;
            for tap in 1 - HALF_TAPS..=HALF_TAPS {
                let distance = (tap as f64 - fraction) / HALF_TAPS as f64;
                if distance.abs() >= 1.0 {
                    continue;
                }
                let window =
                    bessel_i0(KAISER_BETA * (1.0 - distance * distance).sqrt()) / beta_window;
                let weight = cutoff * sinc(cutoff * (tap as f64 - fraction)) * window;
                let source = (center + tap).clamp(0, source_frames as isize - 1) as usize;
                left += f64::from(samples[source * 2]) * weight;
                right += f64::from(samples[source * 2 + 1]) * weight;
                weight_sum += weight;
            }
            if weight_sum.abs() > 1e-12 {
                left /= weight_sum;
                right /= weight_sum;
            }
            output.push(left as f32);
            output.push(right as f32);
        }
        output
    }

    fn tone(rate: u32, frequency: f64, frames: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let value = (TAU * frequency * frame as f64 / f64::from(rate)).sin() as f32 * 0.5;
            samples.push(value);
            samples.push(value);
        }
        samples
    }

    /// Two measurement cases: in-band imaging on upsampling and near-Nyquist
    /// aliasing on downsampling.
    const CASES: [(u32, u32, f64, f64, f64); 2] = [
        (44_100, 48_000, 1_000.0, 20.0, 8_000.0),
        (48_000, 44_100, 19_000.0, 20.0, 20_000.0),
    ];

    /// THD+N of the left channel: fundamental power from a least-squares fit,
    /// residual band power from a Blackman-Harris windowed FFT.
    fn thd_n(
        samples: &[f32],
        rate: u32,
        fundamental: f64,
        low: f64,
        high: f64,
        start: usize,
        length: usize,
    ) -> f64 {
        let count = length as f64;
        let (mut mean, mut cosine, mut sine) = (0.0, 0.0, 0.0);
        for frame in 0..length {
            let value = f64::from(samples[(start + frame) * 2]);
            let angle = TAU * fundamental * frame as f64 / f64::from(rate);
            mean += value;
            cosine += value * angle.cos();
            sine += value * angle.sin();
        }
        mean /= count;
        cosine *= 2.0 / count;
        sine *= 2.0 / count;
        let mut residual = Vec::with_capacity(length);
        let mut window_power = 0.0;
        for frame in 0..length {
            let position = frame as f64 / (length - 1) as f64;
            let window = 0.35875 - 0.48829 * (TAU * position).cos()
                + 0.14128 * (2.0 * TAU * position).cos()
                - 0.01168 * (3.0 * TAU * position).cos();
            window_power += window * window;
            let angle = TAU * fundamental * frame as f64 / f64::from(rate);
            let value = f64::from(samples[(start + frame) * 2])
                - (mean + cosine * angle.cos() + sine * angle.sin());
            residual.push(Complex::new(value * window, 0.0));
        }
        let mut planner = FftPlanner::<f64>::new();
        planner.plan_fft_forward(length).process(&mut residual);
        let bin = f64::from(rate) / count;
        let fundamental_bin = (fundamental / bin).round();
        let mut band_power = 0.0;
        for (index, value) in residual.iter().enumerate().take(length / 2 + 1) {
            let frequency = index as f64 * bin;
            if frequency < low || frequency > high {
                continue;
            }
            if (index as f64 - fundamental_bin).abs() <= 10.0 {
                continue;
            }
            band_power += value.norm_sqr();
        }
        let residual_mean_square = band_power / (count * window_power);
        let fundamental_power = (cosine * cosine + sine * sine) / 2.0;
        10.0 * (residual_mean_square / fundamental_power).log10()
    }

    /// Ideal band-limited interpolation of a signal that is exactly periodic
    /// over its whole buffer: zero-pad the spectrum to the output length and
    /// inverse transform. Returns interleaved stereo.
    fn ideal_reference(samples: &[f32], source: u32, target: u32) -> Vec<f32> {
        let frames = samples.len() / 2;
        let output_frames =
            ((frames as f64) * f64::from(target) / f64::from(source)).round() as usize;
        let mut input: Vec<Complex<f64>> = (0..frames)
            .map(|frame| Complex::new(f64::from(samples[frame * 2]), 0.0))
            .collect();
        let mut planner = FftPlanner::<f64>::new();
        planner.plan_fft_forward(frames).process(&mut input);
        let mut output = vec![Complex::new(0.0, 0.0); output_frames];
        let keep = frames / 2 + 1;
        output[..keep].copy_from_slice(&input[..keep]);
        for bin in 1..frames / 2 {
            output[output_frames - bin] = input[frames - bin];
        }
        planner.plan_fft_inverse(output_frames).process(&mut output);
        let scale = 1.0 / frames as f64;
        let mono: Vec<f32> = output
            .iter()
            .map(|value| (value.re * scale) as f32)
            .collect();
        let mut interleaved = Vec::with_capacity(output_frames * 2);
        for sample in mono {
            interleaved.push(sample);
            interleaved.push(sample);
        }
        interleaved
    }

    fn case_signal(case: (u32, u32, f64, f64, f64)) -> (Vec<f32>, usize, usize) {
        let frames = (f64::from(case.0) * 1.2).round() as usize;
        let signal = tone(case.0, case.2, frames);
        let start = case.1 as usize / 4;
        let length = case.1 as usize / 2;
        (signal, start, length)
    }

    /// THD+N for every rung plus the ideal-sinc reference, per case.
    fn measure_ladder(case: (u32, u32, f64, f64, f64)) -> (Vec<f64>, f64) {
        let (signal, start, length) = case_signal(case);
        let values: Vec<f64> = ResampleQuality::ALL
            .iter()
            .map(|quality| {
                let output = resample(&signal, case.0, case.1, *quality).unwrap();
                thd_n(&output, case.1, case.2, case.3, case.4, start, length)
            })
            .collect();
        let reference = ideal_reference(&signal, case.0, case.1);
        let reference_db = thd_n(&reference, case.1, case.2, case.3, case.4, start, length);
        (values, reference_db)
    }

    #[test]
    fn ladder_rungs_are_ordered_and_anchored() {
        assert_eq!(ResampleQuality::ALL.len(), STEPS);
        assert_eq!(
            ResampleQuality::from_step(0),
            Some(ResampleQuality::FASTEST)
        );
        assert_eq!(ResampleQuality::from_step(7), Some(ResampleQuality::BEST));
        assert_eq!(ResampleQuality::from_step(8), None);
        for pair in ResampleQuality::ALL.windows(2) {
            assert!(pair[1].taps() > pair[0].taps());
            assert!(pair[1].beta() > pair[0].beta());
            assert!(pair[1].stopband_db() > pair[0].stopband_db());
            assert!(pair[1].transition_width() < pair[0].transition_width());
        }
        assert_eq!(ResampleQuality::PRESERVED.taps(), 48);
        assert_eq!(ResampleQuality::PRESERVED.beta(), 8.6);
        assert_eq!(ResampleQuality::DEFAULT_RENDER, ResampleQuality::PRESERVED);
        assert!(ResampleQuality::DEFAULT_LIVE < ResampleQuality::DEFAULT_RENDER);
        // Kaiser's estimate: attenuation and transition width follow the
        // window parameter and the tap count, not a table of magic numbers.
        for quality in ResampleQuality::ALL {
            let expected = 8.7 + quality.beta() / 0.1102;
            assert!((quality.stopband_db() - expected).abs() < 1e-12);
            let expected = (expected - 8.0) / (14.36 * quality.taps() as f64);
            assert!((quality.transition_width() - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn preserved_rung_is_bit_identical_to_the_legacy_converter() {
        let mut seed = 0x1234_5678_u32;
        for (source, target, frames) in [
            (44_100_u32, 48_000_u32, 4096_usize),
            (48_000, 44_100, 4097),
            (96_000, 48_000, 2048),
            (32_000, 48_000, 1000),
        ] {
            let samples: Vec<f32> = (0..frames * 2)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
                })
                .collect();
            let legacy = legacy_resample(&samples, source, target);
            let ladder = resample(&samples, source, target, ResampleQuality::PRESERVED).unwrap();
            assert_eq!(ladder.len(), legacy.len());
            for (index, (actual, expected)) in ladder.iter().zip(&legacy).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "{source} Hz -> {target} Hz, sample {index}"
                );
            }
        }
    }

    #[test]
    fn thd_n_is_monotone_across_the_ladder() {
        for case in CASES {
            let (values, reference) = measure_ladder(case);
            println!(
                "{} Hz -> {} Hz {} Hz: {:?} dB, reference {:.2} dB",
                case.0, case.1, case.2, values, reference
            );
            for pair in values.windows(2) {
                assert!(
                    pair[1] <= pair[0] + 0.1,
                    "{} Hz -> {} Hz {} Hz: THD+N rose from {:.2} to {:.2} dB",
                    case.0,
                    case.1,
                    case.2,
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn top_rung_matches_an_ideal_sinc_reference() {
        const TOLERANCE_DB: f64 = 10.0;
        for case in CASES {
            let (values, reference) = measure_ladder(case);
            let top = *values.last().unwrap();
            assert!(
                top <= reference + TOLERANCE_DB,
                "{} Hz -> {} Hz {} Hz: top rung {:.2} dB is more than {TOLERANCE_DB} dB above the reference {:.2} dB",
                case.0,
                case.1,
                case.2,
                top,
                reference
            );
        }
    }

    #[test]
    #[ignore = "wall-clock measurement for the design note"]
    fn measure_cost_per_rung() {
        let (source, target) = (44_100_u32, 48_000_u32);
        let frames = source as usize * 10;
        let signal = tone(source, 1_000.0, frames);
        let output_frames = (frames as f64 * f64::from(target) / f64::from(source)).round();
        fn median_seconds(mut run: impl FnMut()) -> f64 {
            let mut times = Vec::new();
            for _ in 0..3 {
                let started = std::time::Instant::now();
                run();
                times.push(started.elapsed().as_secs_f64());
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            times[1]
        }
        for quality in ResampleQuality::ALL {
            let seconds = median_seconds(|| {
                std::hint::black_box(resample(&signal, source, target, quality).unwrap());
            });
            println!(
                "step {}: {:3} taps, {:6.0} dB, {:.3} us/frame, {:.3} s",
                quality.step(),
                quality.taps(),
                quality.stopband_db(),
                seconds * 1e6 / output_frames,
                seconds
            );
        }
        let legacy = median_seconds(|| {
            std::hint::black_box(legacy_resample(&signal, source, target));
        });
        let ladder = median_seconds(|| {
            std::hint::black_box(
                resample(&signal, source, target, ResampleQuality::DEFAULT_RENDER).unwrap(),
            );
        });
        println!(
            "legacy: {:.3} us/frame, default rung: {:.3} us/frame, {:.1}% of legacy",
            legacy * 1e6 / output_frames,
            ladder * 1e6 / output_frames,
            legacy / ladder * 100.0
        );
    }
}
