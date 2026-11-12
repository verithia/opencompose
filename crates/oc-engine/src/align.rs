//! Offline take alignment: onset detection, monotonic matching, affine correction.
//! Runs at load/offline time; the audio callback never calls this.

use oc_dsl::ast::{AudioClip, StretchMode};

use crate::stretch::{MAX_RATIO, MIN_RATIO};

/// Largest tempo difference we consider musically usable before artefacts dominate.
pub const USABLE_TEMPO_DRIFT: f64 = 0.10;

/// A take whose aligned correlation is below this is treated as out of phase.
pub const POLARITY_THRESHOLD: f64 = -0.25;

/// Cost of leaving one onset unmatched on either side, in seconds.
const GAP_COST: f64 = 0.05;
/// A matched pair this far apart is never cheaper than skipping both.
const MAX_MATCH_COST: f64 = 0.20;
/// Inlier window of the robust affine fit, in seconds.
const FIT_TOLERANCE: f64 = 0.005;
/// Onsets closer than this belong to one event, in seconds.
const MIN_ONSET_GAP: f64 = 0.015;
/// Minimum log-energy rise for a frame to count as an onset.
const MIN_STRENGTH: f64 = 0.05;
/// A slope this close to one is treated as one, keeping an aligned take untouched.
const IDENTITY_SLOPE: f64 = 1e-6;

/// One matched onset pair, in the reference's and the take's media time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorrectionPoint {
    pub reference_seconds: f64,
    pub take_seconds: f64,
}

/// Affine map from reference media time to take media time: `take = slope * reference + shift`.
///
/// `ratio` and `head_trim_seconds` are the values the clip surface consumes;
/// `points` are the inlier matches, kept for reporting rather than interpolation.
#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    pub tempo_ratio: f64,
    pub shift_seconds: f64,
    pub ratio: f64,
    pub head_trim_seconds: f64,
    pub matched: usize,
    pub reference_onsets: usize,
    pub take_onsets: usize,
    pub max_residual_seconds: f64,
    /// Normalised correlation of the aligned overlap, in `-1..=1`.
    pub correlation: f64,
    pub points: Vec<CorrectionPoint>,
}

impl Alignment {
    /// True when the take already matches the reference and nothing must be written.
    pub fn is_identity(&self) -> bool {
        self.ratio == 1.0 && self.head_trim_seconds == 0.0
    }

    /// False when the tempo correction exceeds the band we trust the stretch with.
    pub fn within_usable_tempo(&self) -> bool {
        (self.tempo_ratio - 1.0).abs() <= USABLE_TEMPO_DRIFT
    }

    /// True when the aligned take is out of phase with the reference.
    pub fn is_inverted(&self) -> bool {
        self.correlation < POLARITY_THRESHOLD
    }
}

/// The two clip fields an alignment writes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipCorrection {
    /// Output duration over media duration, written to `stretch:`.
    pub ratio: f64,
    /// Seconds trimmed from the start of the stretched media, written to `offset:`.
    pub offset_seconds: f64,
}

impl ClipCorrection {
    /// Whether the clip needs no correction.
    pub fn is_identity(&self) -> bool {
        self.ratio == 1.0 && self.offset_seconds == 0.0
    }
}

/// Detect attack samples on interleaved stereo.
///
/// A 10 ms energy window with a 2.5 ms hop gives a log-energy rise per frame;
/// the threshold follows the local median and each attack is refined to a sample.
pub fn detect_onsets(samples: &[f32], sample_rate: u32) -> Vec<usize> {
    if sample_rate == 0 || !samples.len().is_multiple_of(2) {
        return Vec::new();
    }
    let mono: Vec<f32> = samples
        .chunks_exact(2)
        .map(|frame| 0.5 * (frame[0] + frame[1]))
        .collect();
    let peak = mono
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    if peak <= 1e-6 {
        return Vec::new();
    }
    let frame = (sample_rate as usize * 10 / 1000).max(8);
    let hop = (sample_rate as usize * 5 / 2000).max(2);
    let energies = frame_energies(&mono, frame, hop);
    if energies.len() < 3 {
        let floor = peak * 0.1;
        return (0..mono.len())
            .find(|&index| mono[index].abs() > floor)
            .into_iter()
            .collect();
    }
    let max_energy = energies.iter().copied().fold(0.0_f64, f64::max);
    let mut onsets = Vec::new();
    if max_energy > 0.0
        && energies[0] >= max_energy * 0.01
        && let Some(onset) =
            refine_onset(&mono, 0, (hop + frame).min(mono.len()), sample_rate, peak)
    {
        onsets.push(onset);
    }
    let strengths: Vec<f64> = (1..energies.len())
        .map(|index| (energies[index] + 1e-12).ln() - (energies[index - 1] + 1e-12).ln())
        .map(|value| value.max(0.0))
        .collect();
    let min_gap = (MIN_ONSET_GAP * f64::from(sample_rate)).round() as usize;
    for index in 0..strengths.len() {
        let value = strengths[index];
        if value < MIN_STRENGTH {
            continue;
        }
        let low = index.saturating_sub(12);
        let high = (index + 13).min(strengths.len());
        let mut local: Vec<f64> = strengths[low..high].to_vec();
        let threshold = 1.5 * median(&mut local);
        if value < threshold {
            continue;
        }
        let previous = index.checked_sub(1).map_or(0.0, |at| strengths[at]);
        let next = strengths.get(index + 1).copied().unwrap_or(0.0);
        if value < previous || value < next {
            continue;
        }
        let frame_index = index + 1;
        let start = (frame_index - 1) * hop;
        let end = (frame_index * hop + frame).min(mono.len());
        let Some(onset) = refine_onset(&mono, start, end, sample_rate, peak) else {
            continue;
        };
        if onsets.last().is_some_and(|last| onset <= last + min_gap) {
            continue;
        }
        onsets.push(onset);
    }
    onsets
}

/// Fit an affine map between two onset lists and validate it for the clip surface.
pub fn analyze(reference: &[f32], take: &[f32], sample_rate: u32) -> Result<Alignment, String> {
    if sample_rate == 0 {
        return Err("alignment sample rate must be positive".into());
    }
    if !reference.len().is_multiple_of(2) || !take.len().is_multiple_of(2) {
        return Err("alignment input must be interleaved stereo".into());
    }
    let reference_onsets = detect_onsets(reference, sample_rate);
    let take_onsets = detect_onsets(take, sample_rate);
    if reference_onsets.is_empty() {
        return Err("reference has no detected onsets".into());
    }
    if take_onsets.is_empty() {
        return Err("take has no detected onsets".into());
    }
    let rate = f64::from(sample_rate);
    let reference_seconds: Vec<f64> = reference_onsets
        .iter()
        .map(|&at| at as f64 / rate)
        .collect();
    let take_seconds: Vec<f64> = take_onsets.iter().map(|&at| at as f64 / rate).collect();
    let mut pairs = warped_pairs(
        &reference_seconds,
        &take_seconds,
        coarse_fit(&reference_seconds, &take_seconds),
    );
    if pairs.is_empty() {
        pairs = monotonic_pairs(&reference_seconds, &take_seconds);
    }
    let mut fit =
        robust_fit(&pairs).ok_or_else(|| "take has no monotonic onset match".to_string())?;
    for _ in 0..2 {
        let next = warped_pairs(
            &reference_seconds,
            &take_seconds,
            Some((fit.slope, fit.intercept)),
        );
        let Some(refined) = robust_fit(&next) else {
            break;
        };
        let settled = (refined.slope - fit.slope).abs() < 1e-9
            && (refined.intercept - fit.intercept).abs() < 1e-9;
        fit = refined;
        if settled {
            break;
        }
    }
    if fit.intercept.abs() <= 0.5 / rate {
        fit.intercept = 0.0;
    }
    let ratio = 1.0 / fit.slope;
    if !ratio.is_finite() || !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!(
            "alignment needs stretch ratio {ratio:.4} (tempo ratio {:.4}), outside {MIN_RATIO}..={MAX_RATIO:.1}",
            fit.slope
        ));
    }
    let points = fit
        .inliers
        .iter()
        .map(|&(reference_seconds, take_seconds)| CorrectionPoint {
            reference_seconds,
            take_seconds,
        })
        .collect();
    let correlation = aligned_correlation(reference, take, sample_rate, fit.slope, fit.intercept);
    Ok(Alignment {
        tempo_ratio: fit.slope,
        shift_seconds: fit.intercept,
        ratio,
        head_trim_seconds: fit.intercept / fit.slope,
        matched: fit.inliers.len(),
        reference_onsets: reference_onsets.len(),
        take_onsets: take_onsets.len(),
        max_residual_seconds: fit.max_residual,
        correlation,
        points,
    })
}

/// Normalised correlation over the reference frames that map inside the take.
///
/// The take is linearly interpolated on the fitted map, which is enough to
/// decide polarity; a large stretch makes the measured value conservative.
fn aligned_correlation(
    reference: &[f32],
    take: &[f32],
    sample_rate: u32,
    slope: f64,
    shift: f64,
) -> f64 {
    let rate = f64::from(sample_rate);
    let reference_frames = reference.len() / 2;
    let take_frames = take.len() / 2;
    if reference_frames == 0 || take_frames == 0 || slope <= 0.0 {
        return 0.0;
    }
    let first = (-shift * rate / slope).max(0.0).ceil() as usize;
    let last = ((take_frames as f64 - 1.0 - shift * rate) / slope)
        .min(reference_frames as f64 - 1.0)
        .floor() as usize;
    if last <= first {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut reference_energy = 0.0_f64;
    let mut take_energy = 0.0_f64;
    for frame in first..=last {
        let position = slope * frame as f64 + shift * rate;
        let index = position.floor() as usize;
        if index >= take_frames {
            break;
        }
        let fraction = position - index as f64;
        let next = (index + 1).min(take_frames - 1);
        for channel in 0..2 {
            let reference_sample = f64::from(reference[frame * 2 + channel]);
            let take_sample = f64::from(take[index * 2 + channel]) * (1.0 - fraction)
                + f64::from(take[next * 2 + channel]) * fraction;
            dot += reference_sample * take_sample;
            reference_energy += reference_sample * reference_sample;
            take_energy += take_sample * take_sample;
        }
    }
    if reference_energy <= 0.0 || take_energy <= 0.0 {
        return 0.0;
    }
    (dot / (reference_energy.sqrt() * take_energy.sqrt())).clamp(-1.0, 1.0)
}

/// Resolve an alignment into clip-local output time.
///
/// The take's media time maps to `(media - shift) / tempo_ratio` on the
/// reference's timeline; a later clip start needs that much more head trim.
pub fn correction(
    alignment: &Alignment,
    reference_start_seconds: f64,
    take_start_seconds: f64,
    sample_rate: u32,
) -> Result<ClipCorrection, String> {
    if sample_rate == 0 {
        return Err("alignment sample rate must be positive".into());
    }
    let trim = alignment.head_trim_seconds + take_start_seconds - reference_start_seconds;
    if !trim.is_finite() {
        return Err("alignment offset is not finite".into());
    }
    let snap = 0.5 / f64::from(sample_rate);
    let trim = if trim.abs() <= snap { 0.0 } else { trim };
    if trim < 0.0 {
        return Err(format!(
            "alignment needs a {:.1} ms head trim; offset cannot be negative, place the take later",
            -trim * 1000.0
        ));
    }
    Ok(ClipCorrection {
        ratio: alignment.ratio,
        offset_seconds: trim,
    })
}

/// Write a correction into an audio clip, checking that the render path will honour it.
pub fn write_into(clip: &mut AudioClip, correction: &ClipCorrection) -> Result<(), String> {
    if correction.is_identity() {
        return Ok(());
    }
    if clip.tempo_follow {
        return Err(format!(
            "audio clip '{}': alignment cannot write a correction while tempo_follow is on",
            clip.name
        ));
    }
    if correction.ratio != 1.0 && clip.stretch_mode == StretchMode::Off {
        return Err(format!(
            "audio clip '{}': alignment needs stretch mode musical or percussive, not off",
            clip.name
        ));
    }
    clip.offset_seconds = correction.offset_seconds;
    clip.stretch = correction.ratio;
    Ok(())
}

#[derive(Debug, Clone)]
struct Fit {
    slope: f64,
    intercept: f64,
    inliers: Vec<(f64, f64)>,
    max_residual: f64,
}

/// Sample `(slope, intercept)` hypotheses from far-apart onset pairs and keep
/// the one that matches the most onsets. DTW cannot bridge a large tempo gap on
/// its own: nearest-in-time pairing biases the slope towards one.
fn coarse_fit(reference: &[f64], take: &[f64]) -> Option<(f64, f64)> {
    if reference.len() < 2 || take.len() < 2 {
        return Some((1.0, take.first()? - reference.first()?));
    }
    let reference_span = (reference.last()? - reference.first()?).max(1e-6);
    let take_span = (take.last()? - take.first()?).max(1e-6);
    let duration = reference_span.max(take_span);
    let minimum_separation = duration * 0.10;
    let tolerance = (duration * 0.005).clamp(0.005, 0.025);
    let mut random = Lcg::new(0x9e37_79b9_7f4a_7c15);
    let mut best: Option<(f64, f64, usize)> = None;
    for _ in 0..1024 {
        let first = random.below(reference.len());
        let second = random.below(reference.len());
        if first == second {
            continue;
        }
        let (left, right) = if reference[first] <= reference[second] {
            (first, second)
        } else {
            (second, first)
        };
        if reference[right] - reference[left] < minimum_separation {
            continue;
        }
        let third = random.below(take.len());
        let fourth = random.below(take.len());
        if third == fourth {
            continue;
        }
        let (early, late) = if take[third] <= take[fourth] {
            (third, fourth)
        } else {
            (fourth, third)
        };
        if take[late] - take[early] < take_span * 0.10 {
            continue;
        }
        let slope = (take[late] - take[early]) / (reference[right] - reference[left]);
        if !slope.is_finite() || !(0.05..=20.0).contains(&slope) {
            continue;
        }
        let intercept = take[early] - slope * reference[left];
        if !intercept.is_finite() || intercept.abs() > duration + 10.0 {
            continue;
        }
        let support = count_support(reference, take, slope, intercept, tolerance);
        if best.is_none_or(|(_, _, previous)| support > previous) {
            best = Some((slope, intercept, support));
        }
    }
    best.map(|(slope, intercept, _)| (slope, intercept))
}

/// Map the take onto the reference timeline with `coarse`, then align monotonically.
fn warped_pairs(reference: &[f64], take: &[f64], coarse: Option<(f64, f64)>) -> Vec<(f64, f64)> {
    let Some((slope, intercept)) = coarse else {
        return Vec::new();
    };
    if !slope.is_finite() || slope <= 0.0 {
        return Vec::new();
    }
    let warped: Vec<f64> = take.iter().map(|&at| (at - intercept) / slope).collect();
    monotonic_pairs(reference, &warped)
        .into_iter()
        .map(|(reference, warped)| (reference, slope * warped + intercept))
        .collect()
}

fn count_support(
    reference: &[f64],
    take: &[f64],
    slope: f64,
    intercept: f64,
    tolerance: f64,
) -> usize {
    let mut support = 0;
    let mut index = 0;
    for &reference_time in reference {
        let expected = slope * reference_time + intercept;
        while index < take.len() && take[index] < expected - tolerance {
            index += 1;
        }
        if index < take.len() && (take[index] - expected).abs() <= tolerance {
            support += 1;
            index += 1;
        }
    }
    support
}

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as usize % bound
    }
}

fn monotonic_pairs(reference: &[f64], take: &[f64]) -> Vec<(f64, f64)> {
    let rows = reference.len();
    let columns = take.len();
    let width = columns + 1;
    let mut direction = vec![0_u8; (rows + 1) * width];
    let mut previous = vec![0.0_f64; width];
    let mut current = vec![0.0_f64; width];
    for column in 1..=columns {
        previous[column] = previous[column - 1] + GAP_COST;
        direction[column] = 2;
    }
    for row in 1..=rows {
        current[0] = previous[0] + GAP_COST;
        direction[row * width] = 1;
        for column in 1..=columns {
            let distance = (reference[row - 1] - take[column - 1]).abs();
            let diagonal = previous[column - 1] + distance.min(MAX_MATCH_COST);
            let up = previous[column] + GAP_COST;
            let left = current[column - 1] + GAP_COST;
            let (cost, step) = if diagonal <= up && diagonal <= left {
                (diagonal, 0)
            } else if up <= left {
                (up, 1)
            } else {
                (left, 2)
            };
            current[column] = cost;
            direction[row * width + column] = step;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let mut pairs = Vec::new();
    let (mut row, mut column) = (rows, columns);
    while row > 0 || column > 0 {
        match direction[row * width + column] {
            0 => {
                pairs.push((reference[row - 1], take[column - 1]));
                row -= 1;
                column -= 1;
            }
            1 => row -= 1,
            _ => column -= 1,
        }
    }
    pairs.reverse();
    pairs
}

fn robust_fit(pairs: &[(f64, f64)]) -> Option<Fit> {
    if pairs.is_empty() {
        return None;
    }
    let mut slope = 1.0;
    let mut intercept = pairs[0].1 - pairs[0].0;
    if pairs.len() >= 2 {
        let mut slopes = Vec::new();
        for first in 0..pairs.len() {
            for second in (first + 1)..pairs.len() {
                let delta = pairs[second].0 - pairs[first].0;
                if delta.abs() >= 0.05 {
                    slopes.push((pairs[second].1 - pairs[first].1) / delta);
                }
            }
        }
        if !slopes.is_empty() {
            slope = median(&mut slopes);
            let mut intercepts: Vec<f64> = pairs
                .iter()
                .map(|&(reference, take)| take - slope * reference)
                .collect();
            intercept = median(&mut intercepts);
        }
    }
    let mut inliers: Vec<(f64, f64)> = pairs
        .iter()
        .copied()
        .filter(|&(reference, take)| {
            (take - (slope * reference + intercept)).abs() <= FIT_TOLERANCE
        })
        .collect();
    if inliers.len() < 2 {
        slope = 1.0;
        let mut intercepts: Vec<f64> = pairs
            .iter()
            .map(|&(reference, take)| take - reference)
            .collect();
        intercept = median(&mut intercepts);
        inliers = pairs.to_vec();
    } else {
        let (fitted_slope, fitted_intercept) = least_squares(&inliers);
        slope = fitted_slope;
        intercept = fitted_intercept;
        inliers.retain(|&(reference, take)| {
            (take - (slope * reference + intercept)).abs() <= FIT_TOLERANCE
        });
        if inliers.len() >= 2 {
            let (fitted_slope, fitted_intercept) = least_squares(&inliers);
            slope = fitted_slope;
            intercept = fitted_intercept;
        }
    }
    if (slope - 1.0).abs() < IDENTITY_SLOPE {
        slope = 1.0;
    }
    let max_residual = inliers
        .iter()
        .map(|&(reference, take)| (take - (slope * reference + intercept)).abs())
        .fold(0.0_f64, f64::max);
    Some(Fit {
        slope,
        intercept,
        inliers,
        max_residual,
    })
}

fn least_squares(pairs: &[(f64, f64)]) -> (f64, f64) {
    let count = pairs.len() as f64;
    let mean_reference = pairs.iter().map(|pair| pair.0).sum::<f64>() / count;
    let mean_take = pairs.iter().map(|pair| pair.1).sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for &(reference, take) in pairs {
        covariance += (reference - mean_reference) * (take - mean_take);
        variance += (reference - mean_reference).powi(2);
    }
    if variance <= 1e-12 {
        return (1.0, mean_take - mean_reference);
    }
    let slope = covariance / variance;
    (slope, mean_take - slope * mean_reference)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[middle - 1] + values[middle])
    } else {
        values[middle]
    }
}

fn frame_energies(mono: &[f32], frame: usize, hop: usize) -> Vec<f64> {
    if mono.len() < frame {
        return Vec::new();
    }
    let count = (mono.len() - frame) / hop + 1;
    let mut energies = Vec::with_capacity(count);
    let mut sum: f64 = mono[..frame]
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum();
    energies.push(sum);
    for index in 1..count {
        let leaving = (index - 1) * hop;
        let entering = leaving + frame;
        for offset in 0..hop {
            sum -= f64::from(mono[leaving + offset]).powi(2);
            sum += f64::from(mono[entering + offset]).powi(2);
        }
        energies.push(sum);
    }
    energies
}

fn refine_onset(
    mono: &[f32],
    start: usize,
    end: usize,
    sample_rate: u32,
    peak: f32,
) -> Option<usize> {
    let block = (sample_rate as usize / 3000).max(8);
    let end = end.min(mono.len());
    let floor = peak * 0.05;
    if start + block > end {
        return (start..end).find(|&index| mono[index].abs() > floor);
    }
    let mut previous = block_energy(mono, start.saturating_sub(block), block);
    let mut best = start;
    let mut best_rise = f64::NEG_INFINITY;
    let mut position = start;
    while position + block <= end {
        let current = block_energy(mono, position, block);
        let rise = current - previous;
        if rise > best_rise {
            best_rise = rise;
            best = position;
        }
        previous = current;
        position += block;
    }
    let search_end = (best + 2 * block).min(end);
    (best..search_end).find(|&index| mono[index].abs() > floor)
}

fn block_energy(mono: &[f32], start: usize, block: usize) -> f64 {
    let end = (start + block).min(mono.len());
    mono[start.min(end)..end]
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn clicks(frames: usize, positions: &[usize], amplitude: f32) -> Vec<f32> {
        let mut samples = vec![0.0_f32; frames * 2];
        for &position in positions {
            samples[position * 2] = amplitude;
            samples[position * 2 + 1] = amplitude;
        }
        samples
    }

    #[test]
    fn aligned_correlation_reports_polarity() {
        let positions: Vec<usize> = (0..8).map(|index| 480 + index * 2400).collect();
        let reference = clicks(24_000, &positions, 0.9);
        let shifted: Vec<usize> = positions.iter().map(|position| position + 1440).collect();
        let take = clicks(25_440, &shifted, 0.9);
        let inverted = clicks(25_440, &shifted, -0.9);

        let in_phase = aligned_correlation(&reference, &take, RATE, 1.0, 0.030);
        let out_of_phase = aligned_correlation(&reference, &inverted, RATE, 1.0, 0.030);
        assert!((in_phase - 1.0).abs() < 1e-4, "{in_phase}");
        assert!((out_of_phase + 1.0).abs() < 1e-4, "{out_of_phase}");
        assert!(
            !Alignment {
                tempo_ratio: 1.0,
                shift_seconds: 0.030,
                ratio: 1.0,
                head_trim_seconds: 0.030,
                matched: 8,
                reference_onsets: 8,
                take_onsets: 8,
                max_residual_seconds: 0.0,
                correlation: in_phase,
                points: Vec::new(),
            }
            .is_inverted()
        );
        assert!(
            Alignment {
                tempo_ratio: 1.0,
                shift_seconds: 0.030,
                ratio: 1.0,
                head_trim_seconds: 0.030,
                matched: 8,
                reference_onsets: 8,
                take_onsets: 8,
                max_residual_seconds: 0.0,
                correlation: out_of_phase,
                points: Vec::new(),
            }
            .is_inverted()
        );
    }
}
