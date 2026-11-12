//! Phase-vocoder time-stretch for `mode: musical`.
//! Offline/load time only: the audio callback reads the baked stereo buffer.

use std::f64::consts::{PI, TAU};

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::stretch::{MAX_RATIO, MIN_RATIO};

/// Analysis and synthesis FFT size.
const FFT_SIZE: usize = 2048;
/// Synthesis hop; the analysis hop is `HOP / ratio`.
const HOP: usize = 512;
/// Positive-frequency bins including DC and Nyquist.
const BINS: usize = FFT_SIZE / 2 + 1;

/// Onset detector frame and hop.
const ONSET_FFT: usize = 1024;
const ONSET_HOP: usize = 256;
/// Flux threshold: `LAMBDA * local median + FLOOR` on the normalised flux.
const FLUX_LAMBDA: f64 = 1.5;
const FLUX_FLOOR: f64 = 0.05;
/// Frames either side of the median.
const FLUX_WINDOW: usize = 10;
/// Energy window for the sample-level refinement.
const REFINE_WINDOW: usize = 64;
/// Two onsets closer than this are one onset.
const MIN_ONSET_GAP: usize = 256;

/// Peaks below -60 dB of the frame peak do not lock a region.
const PEAK_FLOOR: f64 = 0.001;
/// One-to-one splice half-length around an onset, in source samples.
const SPLICE_SPAN: usize = FFT_SIZE / 2;
/// Fade into and out of the splice.
const SPLICE_FADE: usize = 128;

/// Stretch interleaved stereo with a 2048/512 STFT; length `round(input x ratio)`.
/// Pitch is unchanged; onsets get a phase reset and a one-to-one splice.
pub fn stretch(samples: &[f32], ratio: f64) -> Result<Vec<f32>, String> {
    stretch_cancellable(samples, ratio, Default::default())
}

pub(crate) fn stretch_cancellable(
    samples: &[f32],
    ratio: f64,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    if !samples.len().is_multiple_of(2) {
        return Err("stretch input must be interleaved stereo".into());
    }
    if !ratio.is_finite() || !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!(
            "stretch ratio {ratio:.4} is outside {MIN_RATIO}..={MAX_RATIO}"
        ));
    }
    if samples.is_empty() || ratio == 1.0 {
        return Ok(samples.to_vec());
    }
    process(samples, ratio, cancellation)
}

fn process(
    samples: &[f32],
    ratio: f64,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    let frames_in = samples.len() / 2;
    let out_frames = crate::stretch::stretched_frames(frames_in, ratio);
    let frames_out = (out_frames + FFT_SIZE / 2).div_ceil(HOP) + 1;
    let mono: Vec<f32> = samples
        .chunks_exact(2)
        .map(|frame| 0.5 * (frame[0] + frame[1]))
        .collect();
    let onsets = detect_onsets_cancellable(&mono, cancellation)?;
    let anchors = anchors(&onsets, frames_in, out_frames, ratio, frames_out);
    let (positions, reset) = analysis_positions(&anchors, frames_out, ratio);
    let mut output = resynthesize(
        samples,
        &positions,
        &reset,
        out_frames,
        frames_in,
        cancellation,
    )?;
    splice_transients(&mut output, samples, &onsets, ratio, frames_in, out_frames);
    Ok(output)
}

/// Source position per synthesis frame from `(source, frame)` anchors; frames
/// between anchors move linearly, so the local ratio is their distance ratio.
fn analysis_positions(
    anchors: &[(usize, usize)],
    frames_out: usize,
    ratio: f64,
) -> (Vec<isize>, Vec<bool>) {
    let mut positions = vec![0_isize; frames_out];
    let mut reset = vec![false; frames_out];
    let mut segment = 0;
    for frame in 0..frames_out {
        while segment + 1 < anchors.len() && anchors[segment + 1].1 <= frame {
            segment += 1;
        }
        let (source, out_frame) = anchors[segment];
        let slope = if segment + 1 < anchors.len() {
            let (next_source, next_frame) = anchors[segment + 1];
            (next_source as f64 - source as f64) / ((next_frame - out_frame) as f64 * HOP as f64)
        } else if segment > 0 {
            let (previous_source, previous_frame) = anchors[segment - 1];
            (source as f64 - previous_source as f64)
                / ((out_frame - previous_frame) as f64 * HOP as f64)
        } else {
            1.0 / ratio
        };
        let position = source as f64 + (frame - out_frame) as f64 * HOP as f64 * slope;
        positions[frame] = position.round() as isize;
        reset[frame] = frame == out_frame;
    }
    (positions, reset)
}

/// Anchors pair an onset with the output frame nearest its stretched time.
fn anchors(
    onsets: &[usize],
    frames_in: usize,
    out_frames: usize,
    ratio: f64,
    frames_out: usize,
) -> Vec<(usize, usize)> {
    let mut anchors = vec![(0_usize, 0_usize)];
    for &onset in onsets {
        if onset == 0 || onset >= frames_in {
            continue;
        }
        let frame = (onset as f64 * ratio / HOP as f64).round() as usize;
        let frame = frame.min(frames_out - 1);
        let (last_source, last_frame) = *anchors.last().expect("anchor zero");
        if frame <= last_frame || onset <= last_source {
            continue;
        }
        anchors.push((onset, frame));
    }
    let end_frame = (out_frames as f64 / HOP as f64).round() as usize;
    let (last_source, last_frame) = *anchors.last().expect("anchor zero");
    if end_frame > last_frame && frames_in > last_source {
        anchors.push((frames_in, end_frame));
    }
    anchors
}

/// Analysis plus overlap-add. Every output sample is divided by the sum of the
/// analysis-times-synthesis windows overlapping it.
fn resynthesize(
    samples: &[f32],
    positions: &[isize],
    reset: &[bool],
    out_frames: usize,
    frames_in: usize,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    let window: Vec<f64> = (0..FFT_SIZE)
        .map(|index| (0.5 - 0.5 * (TAU * index as f64 / FFT_SIZE as f64).cos()).sqrt())
        .collect();
    let weight: Vec<f32> = window.iter().map(|value| (value * value) as f32).collect();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(FFT_SIZE);
    let inverse = planner.plan_fft_inverse(FFT_SIZE);
    let mut analysis_buffer = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut right_buffer = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut synthesis_left = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut synthesis_right = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut magnitude_left = vec![0.0_f64; BINS];
    let mut magnitude_right = vec![0.0_f64; BINS];
    let mut analysis_left = vec![0.0_f64; BINS];
    let mut analysis_right = vec![0.0_f64; BINS];
    let mut phase_left = vec![0.0_f64; BINS];
    let mut phase_right = vec![0.0_f64; BINS];
    let mut previous_left = vec![0.0_f64; BINS];
    let mut previous_right = vec![0.0_f64; BINS];
    let mut mid = vec![0.0_f64; BINS];
    let mut output = vec![0.0_f32; out_frames * 2];
    let mut covered = vec![0.0_f32; out_frames];
    let scale = 1.0 / FFT_SIZE as f64;
    let half = (FFT_SIZE / 2) as isize;
    let mut previous_position = positions.first().copied().unwrap_or(0);

    for (frame, &position) in positions.iter().enumerate() {
        cancellation.check()?;
        let hop = (position - previous_position).max(1) as f64;
        previous_position = position;
        let base = position - half;
        for index in 0..FFT_SIZE {
            let source = base + index as isize;
            let (left, right) = if source >= 0 && (source as usize) < frames_in {
                let source = source as usize * 2;
                (samples[source] as f64, samples[source + 1] as f64)
            } else {
                (0.0, 0.0)
            };
            let window = window[index];
            analysis_buffer[index] = Complex::new(left * window, 0.0);
            right_buffer[index] = Complex::new(right * window, 0.0);
        }
        forward.process(&mut analysis_buffer);
        forward.process(&mut right_buffer);

        for bin in 0..BINS {
            magnitude_left[bin] = analysis_buffer[bin].norm();
            magnitude_right[bin] = right_buffer[bin].norm();
            analysis_left[bin] = analysis_buffer[bin].arg();
            analysis_right[bin] = right_buffer[bin].arg();
            mid[bin] = magnitude_left[bin] + magnitude_right[bin];
        }
        for bin in 0..BINS {
            if reset[frame] {
                phase_left[bin] = analysis_left[bin];
                phase_right[bin] = analysis_right[bin];
            } else {
                let expected = TAU * bin as f64 * hop / FFT_SIZE as f64;
                let deviation = princarg(analysis_left[bin] - previous_left[bin] - expected);
                let omega = TAU * bin as f64 / FFT_SIZE as f64 + deviation / hop;
                phase_left[bin] = princarg(phase_left[bin] + omega * HOP as f64);
                let deviation = princarg(analysis_right[bin] - previous_right[bin] - expected);
                let omega = TAU * bin as f64 / FFT_SIZE as f64 + deviation / hop;
                phase_right[bin] = princarg(phase_right[bin] + omega * HOP as f64);
            }
        }
        lock_regions(
            &mid,
            &analysis_left,
            &analysis_right,
            &mut phase_left,
            &mut phase_right,
        );
        previous_left.copy_from_slice(&analysis_left);
        previous_right.copy_from_slice(&analysis_right);

        for bin in 0..BINS {
            synthesis_left[bin] = Complex::from_polar(magnitude_left[bin], phase_left[bin]);
            synthesis_right[bin] = Complex::from_polar(magnitude_right[bin], phase_right[bin]);
        }
        make_real(&mut synthesis_left);
        make_real(&mut synthesis_right);
        inverse.process(&mut synthesis_left);
        inverse.process(&mut synthesis_right);

        let start = frame as isize * HOP as isize - half;
        for index in 0..FFT_SIZE {
            let destination = start + index as isize;
            if destination < 0 {
                continue;
            }
            let destination = destination as usize;
            if destination >= out_frames {
                break;
            }
            let gain = window[index];
            output[destination * 2] += (synthesis_left[index].re * scale * gain) as f32;
            output[destination * 2 + 1] += (synthesis_right[index].re * scale * gain) as f32;
            covered[destination] += weight[index];
        }
    }
    for (frame, weight) in covered.iter().enumerate() {
        if *weight > 1e-6 {
            let gain = 1.0 / weight;
            output[frame * 2] *= gain;
            output[frame * 2 + 1] *= gain;
        } else {
            output[frame * 2] = 0.0;
            output[frame * 2 + 1] = 0.0;
        }
    }
    Ok(output)
}

/// Identity phase locking: every bin takes its region peak's synthesis phase
/// plus its own analysis offset from that peak.
fn lock_regions(
    mid: &[f64],
    analysis_left: &[f64],
    analysis_right: &[f64],
    phase_left: &mut [f64],
    phase_right: &mut [f64],
) {
    let peak = mid.iter().copied().fold(0.0_f64, f64::max);
    if peak <= 0.0 {
        return;
    }
    let floor = peak * PEAK_FLOOR;
    let mut peaks: Vec<usize> = Vec::new();
    for bin in 1..BINS - 1 {
        if mid[bin] > mid[bin - 1] && mid[bin] >= mid[bin + 1] && mid[bin] > floor {
            peaks.push(bin);
        }
    }
    if peaks.is_empty() {
        return;
    }
    let mut bounds = Vec::with_capacity(peaks.len() + 1);
    bounds.push(0);
    for pair in peaks.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        let mut valley = left + 1;
        for bin in left + 1..right {
            if mid[bin] < mid[valley] {
                valley = bin;
            }
        }
        bounds.push(valley);
    }
    bounds.push(BINS);
    for (index, &region_peak) in peaks.iter().enumerate() {
        let anchor_left = phase_left[region_peak];
        let anchor_right = phase_right[region_peak];
        let offset_left = analysis_left[region_peak];
        let offset_right = analysis_right[region_peak];
        for bin in bounds[index]..bounds[index + 1] {
            if bin == 0 || bin == BINS - 1 {
                continue;
            }
            phase_left[bin] = anchor_left + (analysis_left[bin] - offset_left);
            phase_right[bin] = anchor_right + (analysis_right[bin] - offset_right);
        }
    }
}

/// DC and Nyquist must stay real for the inverse transform.
fn make_real(spectrum: &mut [Complex<f64>]) {
    spectrum[0] = Complex::new(spectrum[0].re, 0.0);
    spectrum[FFT_SIZE / 2] = Complex::new(spectrum[FFT_SIZE / 2].re, 0.0);
    for bin in 1..FFT_SIZE / 2 {
        spectrum[FFT_SIZE - bin] = spectrum[bin].conj();
    }
}

/// Replace the smeared attack with the original samples at their stretched time.
fn splice_transients(
    output: &mut [f32],
    samples: &[f32],
    onsets: &[usize],
    ratio: f64,
    frames_in: usize,
    out_frames: usize,
) {
    for (index, &onset) in onsets.iter().enumerate() {
        let target = (onset as f64 * ratio).round() as usize;
        if target >= out_frames {
            continue;
        }
        let previous = if index == 0 {
            0
        } else {
            (onsets[index - 1] as f64 * ratio).round() as usize
        };
        let next = onsets
            .get(index + 1)
            .map(|&value| (value as f64 * ratio).round() as usize)
            .unwrap_or(out_frames);
        let limit = (SPLICE_SPAN as f64 * ratio).round() as usize + HOP;
        let before = limit.min((target - previous) / 2);
        let after = limit.min((next - target) / 2);
        let fade_in = SPLICE_FADE.min(before).max(1);
        let fade_out = SPLICE_FADE.min(after).max(1);
        for offset in -(before as isize)..after as isize {
            let (Ok(destination), Ok(source)) = (
                usize::try_from(target as isize + offset),
                usize::try_from(onset as isize + offset),
            ) else {
                continue;
            };
            if destination >= out_frames || source >= frames_in {
                continue;
            }
            let gain = if offset < 0 {
                (before as isize + offset) as f32 / fade_in as f32
            } else {
                (after as isize - offset) as f32 / fade_out as f32
            }
            .min(1.0);
            let gain = gain.max(0.0);
            output[destination * 2] =
                output[destination * 2] * (1.0 - gain) + samples[source * 2] * gain;
            output[destination * 2 + 1] =
                output[destination * 2 + 1] * (1.0 - gain) + samples[source * 2 + 1] * gain;
        }
    }
}

fn princarg(phase: f64) -> f64 {
    (phase + PI).rem_euclid(TAU) - PI
}

/// Onset positions in samples: spectral flux on 1024/256 frames plus a 64-sample
/// energy-rise refinement. Threshold `1.5 x the 21-frame median + 0.05`.
pub fn detect_onsets(mono: &[f32]) -> Vec<usize> {
    detect_onsets_cancellable(mono, Default::default()).expect("Onset analysis is not cancellable")
}

fn detect_onsets_cancellable(
    mono: &[f32],
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<usize>, String> {
    let flux = spectral_flux(mono, cancellation)?;
    if flux.is_empty() {
        return Ok(Vec::new());
    }
    let frames = flux.len();
    let mut candidates = Vec::new();
    for frame in 0..frames {
        let center = frame * ONSET_HOP;
        if center + ONSET_FFT / 2 > mono.len() {
            break;
        }
        let low = frame.saturating_sub(FLUX_WINDOW);
        let high = (frame + FLUX_WINDOW + 1).min(frames);
        let mut around = [0.0_f64; 2 * FLUX_WINDOW + 1];
        let length = high - low;
        around[..length].copy_from_slice(&flux[low..high]);
        let median = median(&mut around[..length]);
        if flux[frame] > FLUX_LAMBDA * median + FLUX_FLOOR {
            candidates.push(frame);
        }
    }
    let mut onsets = Vec::with_capacity(candidates.len());
    for frame in candidates {
        let refined = refine(mono, frame);
        if onsets
            .last()
            .is_none_or(|&previous| refined > previous + MIN_ONSET_GAP)
        {
            onsets.push(refined);
        }
    }
    Ok(onsets)
}

/// Positive log-magnitude rise per frame, divided by the frame's log energy.
fn spectral_flux(
    mono: &[f32],
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<Vec<f64>, String> {
    if mono.len() < ONSET_FFT {
        return Ok(Vec::new());
    }
    let frames = (mono.len() + ONSET_HOP / 2).div_ceil(ONSET_HOP);
    let window: Vec<f64> = (0..ONSET_FFT)
        .map(|index| 0.5 - 0.5 * (TAU * index as f64 / ONSET_FFT as f64).cos())
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(ONSET_FFT);
    let mut buffer = vec![Complex::new(0.0, 0.0); ONSET_FFT];
    let mut previous = vec![0.0_f64; ONSET_FFT / 2 + 1];
    let mut flux = vec![0.0_f64; frames];
    for (frame, value) in flux.iter_mut().enumerate() {
        cancellation.check()?;
        let start = frame as isize * ONSET_HOP as isize - (ONSET_FFT / 2) as isize;
        for index in 0..ONSET_FFT {
            let source = start + index as isize;
            let sample = if source >= 0 && (source as usize) < mono.len() {
                f64::from(mono[source as usize])
            } else {
                0.0
            };
            buffer[index] = Complex::new(sample * window[index], 0.0);
        }
        forward.process(&mut buffer);
        let mut rise = 0.0;
        let mut total = 0.0;
        for bin in 0..=ONSET_FFT / 2 {
            let magnitude = (1.0 + buffer[bin].norm()).ln();
            rise += (magnitude - previous[bin]).max(0.0);
            total += magnitude;
            previous[bin] = magnitude;
        }
        *value = rise / (1.0 + total);
    }
    Ok(flux)
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

/// Sample-accurate attack: the last sample of the largest 64-sample energy rise.
fn refine(mono: &[f32], frame: usize) -> usize {
    let center = frame * ONSET_HOP;
    let start = center.saturating_sub(ONSET_FFT / 2);
    let end = (center + ONSET_FFT / 2).min(mono.len());
    let energy = |from: usize, to: usize| -> f64 {
        mono[from..to]
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum()
    };
    let mut best = start;
    let mut best_rise = f64::NEG_INFINITY;
    for sample in start..end {
        let before = energy(sample.saturating_sub(REFINE_WINDOW), sample);
        let after = energy(sample, (sample + REFINE_WINDOW).min(mono.len()));
        let rise = after - before;
        if rise >= best_rise {
            best_rise = rise;
            best = sample;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn tone(frames: usize, frequency: f64, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|frame| (TAU * frequency * frame as f64 / RATE).sin() as f32 * amplitude)
            .collect()
    }

    fn click_train(frames: usize, spacing: usize) -> Vec<f32> {
        let mut mono = vec![0.0_f32; frames];
        for frame in (0..frames).step_by(spacing) {
            mono[frame] = 1.0;
        }
        mono
    }

    fn chord(frames: usize) -> Vec<f32> {
        let mut mono = vec![0.0_f32; frames];
        for (frame, value) in mono.iter_mut().enumerate() {
            let time = frame as f64 / RATE;
            *value = 0.2 * (TAU * 220.0 * time).sin() as f32
                + 0.2 * (TAU * 440.0 * time).sin() as f32
                + 0.2 * (TAU * 660.0 * time).sin() as f32;
        }
        mono
    }

    fn stereo(mono: &[f32]) -> Vec<f32> {
        mono.iter()
            .flat_map(|sample| [*sample, 0.5 * sample])
            .collect()
    }

    /// Unit-ratio positions: every analysis frame on the synthesis grid.
    fn unity_positions(frames: usize) -> (Vec<isize>, Vec<bool>) {
        let count = (frames + FFT_SIZE / 2).div_ceil(HOP) + 1;
        let mut reset = vec![false; count];
        reset[0] = true;
        (
            (0..count).map(|frame| (frame * HOP) as isize).collect(),
            reset,
        )
    }

    #[test]
    fn overlap_add_reconstructs_at_ratio_one() {
        let mono = chord(48_000);
        let input = stereo(&mono);
        let (positions, reset) = unity_positions(mono.len());
        let output = resynthesize(
            &input,
            &positions,
            &reset,
            mono.len(),
            mono.len(),
            Default::default(),
        )
        .unwrap();
        let mut peak = 0.0_f32;
        for (index, value) in output.iter().enumerate() {
            let reference = if index % 2 == 0 {
                mono[index / 2]
            } else {
                0.5 * mono[index / 2]
            };
            peak = peak.max((value - reference).abs());
        }
        assert!(peak < 1e-5, "overlap-add error {peak}");
    }

    #[test]
    fn clicks_are_located_to_the_sample() {
        let mono = click_train(96_000, 4_800);
        let found = detect_onsets(&mono);
        let expected: Vec<usize> = (0..20).map(|index| index * 4_800).collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn steady_material_and_noise_are_not_flagged() {
        assert!(detect_onsets(&tone(48_000, 1_000.0, 0.5)).len() == 1);
        assert_eq!(detect_onsets(&chord(48_000)).len(), 1);
        let mut state = 0x1234_5678_u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / 8_388_608.0 - 1.0) * 0.2
            })
            .collect();
        assert_eq!(detect_onsets(&noise), vec![2]);
    }

    #[test]
    fn an_onset_after_silence_is_refined_to_the_first_samples() {
        let mut mono = vec![0.0_f32; 960];
        mono.extend(tone(47_040, 1_000.0, 0.5));
        let found = detect_onsets(&mono);
        assert_eq!(found.len(), 1);
        assert!(found[0].abs_diff(960) <= 16, "onset at {}", found[0]);
    }

    #[test]
    fn stretching_keeps_the_output_length() {
        for (ratio, expected) in [(1.25, 60_000), (0.75, 36_000), (2.0, 96_000)] {
            let mono = tone(48_000, 1_000.0, 0.5);
            let output = stretch(&stereo(&mono), ratio).unwrap();
            assert_eq!(output.len() / 2, expected, "{ratio}");
            assert_eq!(
                output.len() / 2,
                crate::stretch::stretched_frames(48_000, ratio)
            );
        }
    }

    #[test]
    fn silence_stays_silent() {
        let output = stretch(&vec![0.0_f32; 24_000], 1.75).unwrap();
        assert!(output.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn a_hard_left_signal_keeps_the_right_channel_silent() {
        let mut input = vec![0.0_f32; 48_000 * 2];
        for frame in 0..48_000 {
            input[frame * 2] = tone(48_000, 1_000.0, 0.5)[frame];
        }
        let output = stretch(&input, 1.25).unwrap();
        assert!(
            output
                .iter()
                .skip(1)
                .step_by(2)
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn the_splice_removes_the_vocoder_pre_echo() {
        let mut mono = vec![0.0_f32; 96_000];
        mono[48_000] = 1.0;
        let input = stereo(&mono);
        let out_frames = 144_000;
        let onsets = detect_onsets(&mono);
        assert_eq!(onsets, vec![48_000]);
        let frames_out = (out_frames + FFT_SIZE / 2).div_ceil(HOP) + 1;
        let anchors = anchors(&onsets, mono.len(), out_frames, 1.5, frames_out);
        let (positions, reset) = analysis_positions(&anchors, frames_out, 1.5);
        let raw = resynthesize(
            &input,
            &positions,
            &reset,
            out_frames,
            mono.len(),
            Default::default(),
        )
        .unwrap();
        let spliced = process(&input, 1.5, Default::default()).unwrap();
        let target = 72_000;
        let peak = |samples: &[f32]| {
            samples
                .iter()
                .fold(0.0_f32, |max, sample| max.max(sample.abs()))
        };
        let before = |samples: &[f32]| {
            samples[..(target - 240) * 2]
                .iter()
                .fold(0.0_f32, |max, sample| max.max(sample.abs()))
        };
        let raw_db = 20.0 * (before(&raw) / peak(&raw)).log10();
        let spliced_db = 20.0 * (before(&spliced) / peak(&spliced)).log10();
        assert!(
            raw_db > -40.0,
            "the vocoder alone must smear the attack ({raw_db:.1} dB)"
        );
        assert!(
            spliced_db <= -30.0,
            "spliced pre-echo {spliced_db:.1} dB, raw {raw_db:.1} dB"
        );
        assert!(
            raw_db - spliced_db > 12.0,
            "splice must remove at least 12 dB ({raw_db:.1} -> {spliced_db:.1})"
        );
    }

    #[test]
    fn stretching_is_deterministic() {
        let input = stereo(&click_train(48_000, 4_800));
        let first = stretch(&input, 1.5).unwrap();
        let second = stretch(&input, 1.5).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn invalid_ratios_and_layouts_are_refused() {
        assert!(stretch(&[0.0; 8], 5.0).is_err());
        assert!(stretch(&[0.0; 8], f64::NAN).is_err());
        assert!(stretch(&[0.0; 7], 2.0).is_err());
        assert_eq!(stretch(&[0.0; 8], 1.0).unwrap(), vec![0.0; 8]);
    }
}
