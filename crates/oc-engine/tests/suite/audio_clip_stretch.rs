//! Clip stretch: golden signals, bypass bit-exactness and regression targets.

#[cfg(feature = "plugin-host")]
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::audio_clip;
use oc_engine::derived_cache::DerivedCache;
use oc_engine::media_cache::MediaCache;
#[cfg(feature = "plugin-host")]
use oc_engine::resample::ResampleQuality;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

const RATE: u32 = 48_000;
/// Regression tolerance from the acceptance measurements: every onset within 2 ms.
#[cfg(feature = "plugin-host")]
const ONSET_TOLERANCE_MS: f64 = 2.0;

// ---------------------------------------------------------------------------
// Golden-signal harness
// ---------------------------------------------------------------------------

/// A deterministic test signal with known onsets and pitch.
struct GoldenSignal {
    sample_rate: u32,
    samples: Vec<f32>,
}

impl GoldenSignal {
    /// `count` unit impulses spaced `spacing` frames apart.
    fn click_train(count: usize, spacing: usize, amplitude: f32) -> Self {
        let mut samples = vec![0.0_f32; spacing * (count + 1)];
        for index in 0..count {
            samples[index * spacing] = amplitude;
        }
        Self {
            sample_rate: RATE,
            samples,
        }
    }

    fn tone(seconds: f64, frequency: f64, amplitude: f32) -> Self {
        let frames = (seconds * f64::from(RATE)).round() as usize;
        let samples = (0..frames)
            .map(|frame| {
                (2.0 * PI * frequency * frame as f64 / f64::from(RATE)).sin() as f32 * amplitude
            })
            .collect();
        Self {
            sample_rate: RATE,
            samples,
        }
    }

    #[cfg(feature = "plugin-host")]
    fn frames(&self) -> usize {
        self.samples.len()
    }

    fn write(&self, path: &Path) {
        write_mono_wav(path, self.sample_rate, &self.samples);
    }

    fn onsets(&self) -> Vec<usize> {
        detect_onsets(&self.samples, self.sample_rate)
    }

    fn dominant_frequency(&self) -> f64 {
        dominant_frequency(&self.samples, self.sample_rate)
    }

    /// Fundamental plus two harmonics, equal amplitude and no slow beating.
    #[cfg(feature = "plugin-host")]
    fn harmonic_chord(seconds: f64, amplitude: f32) -> Self {
        let frames = (seconds * f64::from(RATE)).round() as usize;
        let samples = (0..frames)
            .map(|frame| {
                let time = frame as f64 / f64::from(RATE);
                let value = (2.0 * PI * 220.0 * time).sin()
                    + (2.0 * PI * 440.0 * time).sin()
                    + (2.0 * PI * 660.0 * time).sin();
                value as f32 * amplitude
            })
            .collect();
        Self {
            sample_rate: RATE,
            samples,
        }
    }
}

/// Sample indices where short-time energy rises from below the noise floor.
fn detect_onsets(samples: &[f32], sample_rate: u32) -> Vec<usize> {
    let hop = (sample_rate as usize / 1000).max(1);
    let window = hop * 8;
    let peak = samples
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    if peak <= 0.0 {
        return Vec::new();
    }
    let threshold = peak * 0.02;
    let energy = |start: usize| -> f32 {
        samples[start..(start + window).min(samples.len())]
            .iter()
            .map(|sample| sample * sample)
            .sum()
    };
    let mut onsets = Vec::new();
    let mut previous = 0.0;
    let mut frame = 0;
    while frame * hop < samples.len() {
        let current = energy(frame * hop);
        if previous <= threshold * threshold && current > threshold * threshold {
            let start = frame * hop;
            let end = (start + window).min(samples.len());
            let found = samples[start..end]
                .iter()
                .position(|sample| sample.abs() > peak * 0.05);
            onsets.push(found.map_or(start, |offset| start + offset));
        }
        previous = current;
        frame += 1;
    }
    onsets
}

/// Frequency of the largest spectral peak, refined by parabolic interpolation.
fn dominant_frequency(samples: &[f32], sample_rate: u32) -> f64 {
    let len = samples.len();
    assert!(len >= 8, "spectrum needs at least a few samples");
    let mut buffer: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let window = 0.5 - 0.5 * (2.0 * PI * index as f64 / len as f64).cos();
            Complex::new(f64::from(*sample) * window, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(len).process(&mut buffer);
    let magnitude = |bin: usize| buffer[bin].norm();
    let mut peak = 1;
    for bin in 2..len / 2 {
        if magnitude(bin) > magnitude(peak) {
            peak = bin;
        }
    }
    let (left, center, right) = (magnitude(peak - 1), magnitude(peak), magnitude(peak + 1));
    let denominator = left - 2.0 * center + right;
    let offset = if denominator.abs() > 1e-12 {
        0.5 * (left - right) / denominator
    } else {
        0.0
    };
    (peak as f64 + offset) * f64::from(sample_rate) / len as f64
}

/// Magnitude of the largest DFT bin within two bins of `frequency`.
#[cfg(feature = "plugin-host")]
fn partial_level(samples: &[f32], frequency: f64) -> f64 {
    let len = samples.len();
    let mut buffer: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let window = 0.5 - 0.5 * (2.0 * PI * index as f64 / len as f64).cos();
            Complex::new(f64::from(*sample) * window, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(len).process(&mut buffer);
    let center = (frequency / f64::from(RATE) * len as f64).round() as usize;
    let low = center.saturating_sub(2).max(1);
    let high = (center + 3).min(len / 2);
    (low..high)
        .map(|bin| buffer[bin].norm())
        .fold(0.0_f64, f64::max)
}

/// Interpolated frequency of the largest bin within three bins of `near_hz`.
#[cfg(feature = "plugin-host")]
fn peak_frequency(samples: &[f32], near_hz: f64) -> f64 {
    let len = samples.len();
    let mut buffer: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let window = 0.5 - 0.5 * (2.0 * PI * index as f64 / len as f64).cos();
            Complex::new(f64::from(*sample) * window, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(len).process(&mut buffer);
    let center = (near_hz / f64::from(RATE) * len as f64).round() as usize;
    let low = center.saturating_sub(3).max(1);
    let high = (center + 4).min(len / 2 - 1);
    let mut peak = low;
    for bin in low..high {
        if buffer[bin].norm() > buffer[peak].norm() {
            peak = bin;
        }
    }
    let (left, middle, right) = (
        buffer[peak - 1].norm(),
        buffer[peak].norm(),
        buffer[peak + 1].norm(),
    );
    let denominator = left - 2.0 * middle + right;
    let offset = if denominator.abs() > 1e-12 {
        0.5 * (left - right) / denominator
    } else {
        0.0
    };
    (peak as f64 + offset) * f64::from(RATE) / len as f64
}

/// Peak-to-peak level variation of a short-time RMS envelope, in dB.
#[cfg(feature = "plugin-host")]
fn envelope_ripple(samples: &[f32], window: usize) -> f64 {
    let mut levels: Vec<f64> = samples
        .chunks(window)
        .map(|chunk| {
            let power: f64 = chunk.iter().map(|value| f64::from(*value).powi(2)).sum();
            (power / chunk.len() as f64).sqrt()
        })
        .filter(|level| *level > 0.0)
        .map(|level| 20.0 * level.log10())
        .collect();
    levels.sort_by(|left, right| left.total_cmp(right));
    levels[levels.len() - 1] - levels[0]
}

/// Assert that a candidate's onsets follow a reference's onsets at `ratio`.
#[cfg(feature = "plugin-host")]
fn assert_onsets_follow(reference: &[usize], candidate: &[f32], ratio: f64, tolerance_ms: f64) {
    let detected = detect_onsets(candidate, RATE);
    assert_eq!(
        detected.len(),
        reference.len(),
        "expected {} onsets, found {} at {:?}",
        reference.len(),
        detected.len(),
        detected
    );
    let tolerance = (tolerance_ms * f64::from(RATE) / 1000.0) as i64;
    for (index, (input, output)) in reference.iter().zip(&detected).enumerate() {
        let expected = (*input as f64 * ratio).round() as i64;
        let error = *output as i64 - expected;
        assert!(
            error.abs() <= tolerance,
            "onset {index} at {output}, expected {expected} ({error} frames)"
        );
    }
}

// ---------------------------------------------------------------------------
// Project + media helpers
// ---------------------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-stretch", name)
}

fn write_mono_wav(path: &Path, sample_rate: u32, samples: &[f32]) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

/// One clip on its own track; `range` is the musical placement.
fn clip_project(range: &str, properties: &str) -> Project {
    let text = format!(
        r#"@project "Clip stretch"
  tempo: 120
@audio_clip hit {range} | track: tape
{properties}
@track tape | instrument: audio/clip
"#
    );
    let project = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

#[cfg(feature = "plugin-host")]
fn render(root: &Path, project: &Project) -> Vec<f32> {
    oc_engine::render::render_to_buffer_with_plugins_at(
        project,
        Path::new(""),
        RATE,
        &mut HashMap::new(),
        Some(root),
    )
    .unwrap()
}

fn assert_bit_identical(left: &[f32], right: &[f32], context: &str) {
    assert_eq!(left.len(), right.len(), "{context}: lengths differ");
    if let Some(index) = left
        .iter()
        .zip(right)
        .position(|(a, b)| a.to_bits() != b.to_bits())
    {
        panic!(
            "{context}: sample {index} differs: {} vs {}",
            left[index], right[index]
        );
    }
}

// ---------------------------------------------------------------------------
// Harness self-checks
// ---------------------------------------------------------------------------

#[test]
fn harness_recovers_click_onsets() {
    let signal = GoldenSignal::click_train(10, RATE as usize / 10, 1.0);
    assert_eq!(
        signal.onsets(),
        (0..10).map(|i| i * 4_800).collect::<Vec<_>>()
    );
}

#[test]
fn harness_recovers_tone_pitch() {
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    let measured = signal.dominant_frequency();
    assert!((measured - 1_000.0).abs() < 0.05, "measured {measured} Hz");
}

// ---------------------------------------------------------------------------
// 1.0× is a bit-exact bypass
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "plugin-host")]
fn unity_stretch_is_bit_identical_to_no_stretch() {
    let root = temp_dir("unity");
    let signal = GoldenSignal::click_train(4, 4_000, 0.8);
    signal.write(&root.join("hit.wav"));

    let plain = clip_project("[2:1 - 4:1]", "  file: \"hit.wav\"");
    let unity = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hit.wav\"\n  stretch: 1.0\n  mode: musical",
    );
    let reference = render(&root, &plain);
    let stretched = render(&root, &unity);
    assert_bit_identical(&reference, &stretched, "1.0x musical");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn mode_off_with_a_ratio_is_bit_identical() {
    let root = temp_dir("mode-off");
    let signal = GoldenSignal::click_train(4, 4_000, 0.8);
    signal.write(&root.join("hit.wav"));

    let plain = clip_project("[2:1 - 4:1]", "  file: \"hit.wav\"");
    let off = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hit.wav\"\n  stretch: 1.75\n  mode: off",
    );
    let reference = render(&root, &plain);
    let bypassed = render(&root, &off);
    assert_bit_identical(&reference, &bypassed, "mode off");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn tempo_follow_at_unity_is_bit_identical() {
    let root = temp_dir("tempo-follow-unity");
    // [2:1 - 3:1] is two seconds at 120 BPM, exactly the media length.
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("hit.wav"));

    let plain = clip_project("[2:1 - 3:1]", "  file: \"hit.wav\"");
    let follow = clip_project(
        "[2:1 - 3:1]",
        "  file: \"hit.wav\"\n  tempo_follow: true\n  mode: musical",
    );
    let reference = render(&root, &plain);
    let followed = render(&root, &follow);
    assert_bit_identical(&reference, &followed, "tempo-follow at 1.0");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn unity_render_preserves_every_onset() {
    let root = temp_dir("unity-onsets");
    let signal = GoldenSignal::click_train(10, 4_800, 0.8);
    signal.write(&root.join("hit.wav"));
    let project = clip_project("[2:1 - 4:1]", "  file: \"hit.wav\"");
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let reference = signal.onsets();
    assert_eq!(reference.len(), 10);
    let clip: Vec<f32> = rendered[start * 2..(start + signal.frames()) * 2]
        .iter()
        .step_by(2)
        .copied()
        .collect();
    assert_onsets_follow(&reference, &clip, 1.0, ONSET_TOLERANCE_MS);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn unity_render_keeps_the_clip_length() {
    let root = temp_dir("length");
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    signal.write(&root.join("hit.wav"));
    let project = clip_project("[2:1 - 3:1]", "  file: \"hit.wav\"");
    let rendered = render(&root, &project);
    // The clip occupies [2:1, 3:1) = 2 s and must not extend the program.
    let start = 2 * RATE as usize;
    let media_frames =
        audio_clip::load(&root.join("hit.wav"), RATE, ResampleQuality::DEFAULT_RENDER)
            .unwrap()
            .frames();
    assert_eq!(media_frames, RATE as usize);
    let expected_end = start + media_frames;
    assert!(
        rendered[expected_end * 2..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    std::fs::remove_dir_all(root).unwrap();
}

// ---------------------------------------------------------------------------
// Percussive WSOLA: regression targets from the survey
// ---------------------------------------------------------------------------

fn clip_region(rendered: &[f32], start_frame: usize, frames: usize) -> Vec<f32> {
    rendered[start_frame * 2..(start_frame + frames) * 2]
        .iter()
        .step_by(2)
        .copied()
        .collect()
}

#[test]
#[cfg(feature = "plugin-host")]
fn percussive_two_to_one_keeps_every_onset() {
    let root = temp_dir("wsola-onsets");
    let signal = GoldenSignal::click_train(10, RATE as usize / 10, 0.8);
    signal.write(&root.join("hits.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hits.wav\"\n  stretch: 2.0\n  mode: percussive",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = clip_region(&rendered, start, signal.frames() * 2);
    let reference = signal.onsets();
    assert_eq!(reference.len(), 10);
    assert_onsets_follow(&reference, &stretched, 2.0, ONSET_TOLERANCE_MS);

    // No extra energy above -40 dB outside a +/-2 ms window of each onset.
    let peak = stretched
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    let window = (ONSET_TOLERANCE_MS * f64::from(RATE) / 1000.0) as usize;
    let expected: Vec<usize> = reference.iter().map(|frame| frame * 2).collect();
    for (frame, sample) in stretched.iter().enumerate() {
        if expected
            .iter()
            .any(|onset| frame.abs_diff(*onset) <= window)
        {
            continue;
        }
        assert!(
            sample.abs() <= peak * 0.01,
            "extra energy {} at frame {frame} (-{:.1} dB)",
            sample,
            20.0 * (sample.abs() / peak).log10()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn percussive_five_to_four_keeps_pitch() {
    let root = temp_dir("wsola-pitch");
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 1.25\n  mode: percussive",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = clip_region(&rendered, start, RATE as usize * 5 / 4);
    let measured = dominant_frequency(&stretched, RATE);
    let cents = 1_200.0 * (measured / 1_000.0).log2();
    assert!(
        cents.abs() <= 1.0,
        "measured {measured} Hz ({cents:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn percussive_stretch_length_is_rounded_and_the_range_tail_is_silent() {
    let root = temp_dir("wsola-length");
    let signal = GoldenSignal::tone(1.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 1.5\n  mode: percussive",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = (RATE as f64 * 1.5).round() as usize;
    assert_eq!(
        stretched,
        oc_engine::stretch::stretched_frames(RATE as usize, 1.5)
    );
    let end = start + stretched;
    assert!(rendered[end * 2..].iter().all(|sample| *sample == 0.0));
    assert!(
        clip_region(&rendered, start, stretched)
            .iter()
            .any(|sample| sample.abs() > 0.1)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn stretch_render_is_deterministic_and_the_modes_differ() {
    let root = temp_dir("determinism");
    let signal = GoldenSignal::click_train(6, 6_000, 0.7);
    signal.write(&root.join("hits.wav"));
    let percussive = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hits.wav\"\n  stretch: 1.5\n  mode: percussive",
    );
    let musical = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hits.wav\"\n  stretch: 1.5\n  mode: musical",
    );
    let first = render(&root, &percussive);
    let second = render(&root, &percussive);
    assert_bit_identical(&first, &second, "percussive determinism");
    let musical_first = render(&root, &musical);
    let musical_second = render(&root, &musical);
    assert_bit_identical(&musical_first, &musical_second, "musical determinism");
    assert!(
        musical_first
            .iter()
            .zip(&first)
            .any(|(left, right)| left.to_bits() != right.to_bits()),
        "musical must not fall back to the WSOLA path"
    );
    std::fs::remove_dir_all(root).unwrap();
}

// ---------------------------------------------------------------------------
// Musical phase vocoder: regression targets from the survey
// ---------------------------------------------------------------------------

#[cfg(feature = "plugin-host")]
fn cents(measured: f64, target: f64) -> f64 {
    1_200.0 * (measured / target).log2()
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_two_to_one_keeps_every_onset() {
    let root = temp_dir("pv-onsets");
    let signal = GoldenSignal::click_train(10, RATE as usize / 10, 0.8);
    signal.write(&root.join("hits.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"hits.wav\"\n  stretch: 2.0\n  mode: musical",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = clip_region(&rendered, start, signal.frames() * 2);
    let reference = signal.onsets();
    assert_eq!(reference.len(), 10);
    assert_onsets_follow(&reference, &stretched, 2.0, ONSET_TOLERANCE_MS);

    let peak = stretched
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    let window = (ONSET_TOLERANCE_MS * f64::from(RATE) / 1000.0) as usize;
    let expected: Vec<usize> = reference.iter().map(|frame| frame * 2).collect();
    for (frame, sample) in stretched.iter().enumerate() {
        if expected
            .iter()
            .any(|onset| frame.abs_diff(*onset) <= window)
        {
            continue;
        }
        assert!(
            sample.abs() <= peak * 0.01,
            "extra energy {} at frame {frame} (-{:.1} dB)",
            sample,
            20.0 * (sample.abs() / peak).log10()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_five_to_four_keeps_pitch() {
    let root = temp_dir("pv-pitch-up");
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 1.25\n  mode: musical",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = clip_region(&rendered, start, RATE as usize * 5 / 4);
    let measured = dominant_frequency(&stretched, RATE);
    let error = cents(measured, 1_000.0);
    assert!(
        error.abs() <= 1.0,
        "measured {measured} Hz ({error:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_three_to_four_keeps_pitch() {
    let root = temp_dir("pv-pitch-down");
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 0.75\n  mode: musical",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let stretched = clip_region(&rendered, start, RATE as usize * 3 / 4);
    let measured = dominant_frequency(&stretched, RATE);
    let error = cents(measured, 1_000.0);
    assert!(
        error.abs() <= 1.0,
        "measured {measured} Hz ({error:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_level_matches_the_bypass_render() {
    let root = temp_dir("pv-level");
    let signal = GoldenSignal::tone(1.0, 1_000.0, 0.5);
    signal.write(&root.join("tone.wav"));
    let stretched_project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 1.25\n  mode: musical",
    );
    let bypass_project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"tone.wav\"\n  stretch: 1.25\n  mode: off",
    );
    let stretched = render(&root, &stretched_project);
    let bypass = render(&root, &bypass_project);
    let start = 2 * RATE as usize;
    let level = |samples: &[f32]| -> f64 {
        let frames = RATE as usize * 5 / 4;
        let region = clip_region(samples, start, frames);
        let middle = &region[frames / 4..frames * 3 / 4];
        let power: f64 = middle.iter().map(|value| f64::from(*value).powi(2)).sum();
        (power / middle.len() as f64).sqrt()
    };
    let difference = 20.0 * (level(&stretched) / level(&bypass)).log10();
    assert!(
        difference.abs() <= 0.2,
        "level differs by {difference:.3} dB"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_pre_echo_stays_below_thirty_db() {
    let root = temp_dir("pv-pre-echo");
    let mut signal = GoldenSignal::click_train(1, 1, 0.0);
    signal.samples = vec![0.0; 2 * RATE as usize];
    signal.samples[RATE as usize] = 1.0;
    signal.write(&root.join("impulse.wav"));
    let project = clip_project(
        "[2:1 - 4:1]",
        "  file: \"impulse.wav\"\n  stretch: 1.5\n  mode: musical",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let target = start + (RATE as f64 * 1.5) as usize;
    let region = clip_region(&rendered, start, RATE as usize * 2);
    let peak = region
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    let attack = region
        .iter()
        .position(|sample| sample.abs() >= peak * 0.5)
        .expect("impulse survives the stretch");
    let before = region[..target - start - 240]
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    let relative = 20.0 * (before / peak).log10();
    assert!(
        relative <= -30.0,
        "pre-echo {relative:.1} dB before the attack at {attack}"
    );
    assert!(
        attack.abs_diff(target - start) <= 96,
        "attack at {attack}, expected {}",
        target - start
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn musical_chord_stays_phase_stable() {
    for ratio in [0.75, 1.25] {
        let root = temp_dir(&format!("pv-chord-{ratio}"));
        let signal = GoldenSignal::harmonic_chord(1.0, 0.2);
        signal.write(&root.join("chord.wav"));
        let project = clip_project(
            "[2:1 - 4:1]",
            &format!("  file: \"chord.wav\"\n  stretch: {ratio}\n  mode: musical"),
        );
        let rendered = render(&root, &project);
        let start = 2 * RATE as usize;
        let frames = ((RATE as f64 * ratio).round() as usize).max(1);
        let stretched = clip_region(&rendered, start, frames);
        let steady = &stretched[frames / 4..frames * 3 / 4];

        for partial in [220.0, 440.0, 660.0] {
            let measured = peak_frequency(steady, partial);
            let error = cents(measured, partial);
            assert!(
                error.abs() <= 2.0,
                "{ratio}: {partial} Hz measured {measured} ({error:.2} cents)"
            );
        }
        let levels: Vec<f64> = [220.0, 440.0, 660.0]
            .iter()
            .map(|partial| partial_level(steady, *partial))
            .map(|level| 20.0 * level.log10())
            .collect();
        let spread = levels.iter().copied().fold(f64::MIN, f64::max)
            - levels.iter().copied().fold(f64::MAX, f64::min);
        assert!(
            spread <= 1.5,
            "{ratio}: partial levels spread {spread:.2} dB: {levels:?}"
        );
        let ripple = envelope_ripple(steady, RATE as usize / 10);
        assert!(ripple <= 1.0, "{ratio}: envelope ripple {ripple:.2} dB");
        std::fs::remove_dir_all(root).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Cache sharing and the load-time byte budget
// ---------------------------------------------------------------------------

fn clip_only_project(media: &str, clips: &[(&str, f64)]) -> Project {
    let mut text = String::from("@project \"Cache\"\n  tempo: 120\n");
    for (index, (range, ratio)) in clips.iter().enumerate() {
        text.push_str(&format!(
            "@audio_clip take{index} {range} | track: tape\n  file: \"{media}\"\n  stretch: {ratio}\n  mode: percussive\n"
        ));
    }
    text.push_str("@track tape | instrument: audio/clip\n");
    let project = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

#[test]
fn two_clips_with_the_same_variant_decode_and_bake_once() {
    let root = temp_dir("share");
    let signal = GoldenSignal::click_train(4, 4_800, 0.8);
    signal.write(&root.join("hits.wav"));
    let project = clip_only_project("hits.wav", &[("[1:1 - 2:1]", 2.0), ("[3:1 - 4:1]", 2.0)]);
    let cache = oc_engine::media_cache::MediaCache::new(None, 64 * 1024 * 1024, 0);
    // Exactly one variant fits. Reusing the same allocation for two placements
    // must reserve only one variant against the streaming preload budget.
    let budget = oc_engine::derived_cache::stretched_bytes(signal.samples.len(), 2.0);
    let derived = oc_engine::derived_cache::DerivedCache::new(None, budget, 0);
    let lanes = oc_engine::clip_lane::ClipLanes::load_with_caches(
        &cache,
        &derived,
        &project,
        RATE,
        Some(&root),
    )
    .unwrap();
    assert_eq!(lanes.len(), 2);
    assert_eq!(cache.stats().decodes, 1);
    assert_eq!(derived.stats().bakes, 1, "one variant must be shared");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_project_above_the_derived_budget_is_refused_at_load() {
    let root = temp_dir("budget");
    let signal = GoldenSignal::tone(1.0, 440.0, 0.5);
    signal.write(&root.join("tone.wav"));
    let project = clip_only_project("tone.wav", &[("[1:1 - 2:1]", 2.0)]);
    let cache = oc_engine::media_cache::MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = oc_engine::derived_cache::DerivedCache::new(None, 4 * 1024, 0);
    let error = oc_engine::clip_lane::ClipLanes::load_with_caches(
        &cache,
        &derived,
        &project,
        RATE,
        Some(&root),
    )
    .err()
    .expect("the budget must refuse the project");
    assert!(error.contains("derived-media budget"), "{error}");
    assert!(error.contains("MiB"), "{error}");
    assert_eq!(
        derived.stats().bakes,
        0,
        "a refused project must not bake anything"
    );
    std::fs::remove_dir_all(root).unwrap();
}

// ---------------------------------------------------------------------------
// Pitch: a playback-speed resample composed with the stretch
// ---------------------------------------------------------------------------

#[test]
fn spooled_warped_stereo_clip_starts_seeks_and_matches_offline() {
    let root = temp_dir("spooled-warp");
    let path = root.join("stereo.wav");
    let mut wav = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..6 * RATE {
        let time = f64::from(frame) / f64::from(RATE);
        wav.write_sample((time * 220.0 * 2.0 * PI).sin() as f32 * 0.25)
            .unwrap();
        wav.write_sample((time * 330.0 * 2.0 * PI).sin() as f32 * 0.20)
            .unwrap();
    }
    wav.finalize().unwrap();
    let project = oc_dsl::parse(
        r#"@project "Spooled warp"
  tempo: 120
  live_resample: 4

@audio_clip take [1:1 - 5:1] | track: tape
  file: "stereo.wav"
  stretch: 1.25
  mode: musical
  warp: 0.5s -> 0.75s
  warp: 6s -> 8s

@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    let cache = MediaCache::new(None, 4 << 20, 0);
    let cache_dir = root.join("cache");
    let derived = DerivedCache::new(Some(cache_dir.clone()), 64 << 10, 8 << 20);
    let lanes = oc_engine::clip_lane::ClipLanes::load_with_caches(
        &cache,
        &derived,
        &project,
        RATE,
        Some(&root),
    )
    .unwrap();
    assert_eq!(derived.memory_bytes(), 0);
    assert_eq!(derived.stats().bakes, 1);
    assert_eq!(derived.stats().disk_writes, 1);
    let lane = lanes.get(0).unwrap();
    assert_eq!(lane.length_frames(), 8 * RATE as usize);
    let mut offline = vec![0.0; lane.length_frames() * 2];
    let reopened = DerivedCache::new(Some(cache_dir), 64 << 10, 8 << 20);
    audio_clip::mix_into_track_with_caches(
        &cache,
        &reopened,
        &mut offline,
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        Some(&root),
    )
    .unwrap();
    assert_eq!(reopened.memory_bytes(), 0);
    assert_eq!(reopened.stats().disk_hits, 1);
    assert_eq!(reopened.stats().bakes, 0);
    for frame in [
        0,
        36_000,
        36_000 + 4 * RATE as usize - 256,
        7 * RATE as usize,
    ] {
        let mut seeked = vec![0.0; 1024];
        lane.mix_into(frame as u64, &mut seeked).unwrap();
        assert_eq!(seeked, offline[frame * 2..frame * 2 + seeked.len()]);
        assert!(seeked.iter().all(|value| value.is_finite()));
        assert!(seeked.iter().any(|value| value.abs() > 0.05));
        assert!(seeked.chunks_exact(2).any(|pair| pair[0] != pair[1]));
    }
    let seam = (36_000 + 4 * RATE as usize) * 2;
    for channel in 0..2 {
        assert!((offline[seam + channel] - offline[seam + channel - 2]).abs() < 0.08);
    }
    drop(lanes);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "large-file streaming stress test"]
fn over_256_mib_warped_stereo_clip_seeks_from_disk() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("long.wav");
    let mut wav = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..720 * RATE {
        let period = (frame % 128) as i16;
        wav.write_sample((period - 64) * 32).unwrap();
        wav.write_sample((63 - period) * 24).unwrap();
    }
    wav.finalize().unwrap();
    let project = oc_dsl::parse(
        r#"@project "Long stereo warp"
  tempo: 120

@audio_clip take [1:1 - 362:1] | track: tape
  file: "long.wav"
  mode: percussive
  warp: 0.5s -> 0.75s

@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    let cache = MediaCache::new(Some(root.path().join("source-cache")), 512 << 20, 512 << 20);
    let derived = DerivedCache::new(Some(root.path().join("cache")), 256 << 20, 512 << 20);
    let cold_started = std::time::Instant::now();
    let lanes = oc_engine::clip_lane::ClipLanes::load_with_caches(
        &cache,
        &derived,
        &project,
        RATE,
        Some(root.path()),
    )
    .unwrap();
    let cold = cold_started.elapsed();
    assert_eq!(derived.memory_bytes(), 0);
    assert_eq!(derived.stats().bakes, 1);
    assert_eq!(cache.stats().decodes, 1);
    let lane = lanes.get(0).unwrap();
    assert!(lane.length_frames() * 8 > 256 << 20);
    let mut opening = [0.0; 1024];
    lane.mix_into(0, &mut opening).unwrap();
    assert!(opening.iter().any(|sample| sample.abs() > 0.03));
    let mut seeked = [0.0; 1024];
    let late_frame = 700 * RATE as u64;
    lane.mix_into(late_frame, &mut seeked).unwrap();
    for (index, pair) in seeked.chunks_exact(2).enumerate() {
        let phase = ((late_frame + index as u64 - u64::from(RATE / 4)) % 128) as i16;
        assert_eq!(
            pair,
            [
                f32::from((phase - 64) * 32) / 32768.0,
                f32::from((63 - phase) * 24) / 32768.0,
            ]
        );
    }
    drop((lanes, cache, derived));
    let cache = MediaCache::new(Some(root.path().join("source-cache")), 512 << 20, 512 << 20);
    let derived = DerivedCache::new(Some(root.path().join("cache")), 256 << 20, 512 << 20);
    let reopen_started = std::time::Instant::now();
    let lanes = oc_engine::clip_lane::ClipLanes::load_with_caches(
        &cache,
        &derived,
        &project,
        RATE,
        Some(root.path()),
    )
    .unwrap();
    let reopen = reopen_started.elapsed();
    assert_eq!(cache.stats().decodes, 0);
    assert_eq!(derived.stats().bakes, 0);
    assert_eq!(cache.stats().disk_hits, 1);
    assert_eq!(derived.stats().disk_hits, 1);
    let mut reopened = [0.0; 1024];
    lanes
        .get(0)
        .unwrap()
        .mix_into(late_frame, &mut reopened)
        .unwrap();
    assert_eq!(reopened, seeked);
    eprintln!("Long clip preparation: cold {cold:?}, cached reopen {reopen:?}");
}

/// Mix the first clip of a project through the given caches.
fn mix_with(
    cache: &MediaCache,
    derived: &DerivedCache,
    root: &Path,
    project: &Project,
) -> Vec<f32> {
    let clip = project.audio_clip(0).unwrap().clone();
    let (_, length) = audio_clip::range_frames(project, &clip, RATE).unwrap();
    let mut buffer = vec![0.0_f32; length as usize * 2];
    audio_clip::mix_into_track_with_caches(
        cache,
        derived,
        &mut buffer,
        project,
        &clip,
        RATE,
        Some(root),
    )
    .unwrap();
    buffer
}

/// Mix through private caches, so the call always starts from a cold bake.
fn mix_cold(root: &Path, project: &Project) -> Vec<f32> {
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    mix_with(&cache, &derived, root, project)
}

/// Dominant frequency of a rendered clip region, in Hz and in cents from `target`.
fn measured_cents(rendered: &[f32], start: usize, frames: usize, target: f64) -> (f64, f64) {
    let region = clip_region(rendered, start, frames);
    let measured = dominant_frequency(&region, RATE);
    (measured, 1_200.0 * (measured / target).log2())
}

/// A clip still fills exactly `frames` from `start`, and nothing after it.
fn assert_clip_spans(rendered: &[f32], start: usize, frames: usize) {
    let region = clip_region(rendered, start, frames);
    let tail = region[region.len() - 48..]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(tail > 0.05, "the clip ends before its last frame: {tail}");
    assert!(
        rendered[(start + frames) * 2..]
            .iter()
            .all(|sample| *sample == 0.0),
        "the clip runs past {frames} frames"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn pitch_plus_twelve_doubles_the_fundamental_and_keeps_the_length() {
    let root = temp_dir("pitch-up");
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project("[2:1 - 5:1]", "  file: \"tone.wav\"\n  pitch: 12");
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let frames = 2 * RATE as usize;
    assert_clip_spans(&rendered, start, frames);
    let (measured, cents) = measured_cents(&rendered, start, frames, 880.0);
    assert!(
        cents.abs() <= 1.0,
        "measured {measured} Hz ({cents:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn pitch_minus_twelve_halves_the_fundamental_and_keeps_the_length() {
    let root = temp_dir("pitch-down");
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project("[2:1 - 5:1]", "  file: \"tone.wav\"\n  pitch: -12");
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let frames = 2 * RATE as usize;
    assert_clip_spans(&rendered, start, frames);
    let (measured, cents) = measured_cents(&rendered, start, frames, 220.0);
    assert!(
        cents.abs() <= 1.0,
        "measured {measured} Hz ({cents:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn pitch_plus_five_is_a_fractional_ratio() {
    let root = temp_dir("pitch-five");
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project("[2:1 - 5:1]", "  file: \"tone.wav\"\n  pitch: 5");
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let frames = 2 * RATE as usize;
    assert_clip_spans(&rendered, start, frames);
    // 440 Hz raised five semitones.
    let target = 440.0 * 2.0_f64.powf(5.0 / 12.0);
    let (measured, cents) = measured_cents(&rendered, start, frames, target);
    assert!(
        cents.abs() <= 1.0,
        "measured {measured} Hz ({cents:.2} cents from {target:.2})"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pitch_zero_renders_bit_identically_to_the_parent() {
    let root = temp_dir("pitch-zero");
    let signal = GoldenSignal::click_train(10, 4_800, 0.8);
    signal.write(&root.join("hits.wav"));

    for properties in [
        "  file: \"hits.wav\"",
        "  file: \"hits.wav\"\n  stretch: 1.5\n  mode: percussive",
        "  file: \"hits.wav\"\n  stretch: 1.25\n  mode: musical",
    ] {
        let plain = clip_project("[2:1 - 5:1]", properties);
        let zero = clip_project("[2:1 - 5:1]", &format!("{properties}\n  pitch: 0"));
        let expected = mix_cold(&root, &plain);
        let actual = mix_cold(&root, &zero);
        assert_bit_identical(&expected, &actual, properties);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pitch_and_stretch_compose_to_the_stretched_length() {
    let root = temp_dir("pitch-stretch");
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 5:1]",
        "  file: \"tone.wav\"\n  stretch: 1.25\n  pitch: 5",
    );
    let rendered = mix_cold(&root, &project);
    let start = 2 * RATE as usize;
    let frames = (2.0 * f64::from(RATE) * 1.25).round() as usize;
    assert_eq!(frames, 120_000);
    assert_clip_spans(&rendered, start, frames);
    let target = 440.0 * 2.0_f64.powf(5.0 / 12.0);
    let (measured, cents) = measured_cents(&rendered, start, frames, target);
    assert!(
        cents.abs() <= 1.0,
        "measured {measured} Hz ({cents:.2} cents)"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editing_the_pitch_rebakes_only_that_clip() {
    let root = temp_dir("pitch-cache");
    let signal = GoldenSignal::tone(1.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let plain = clip_project("[2:1 - 3:1]", "  file: \"tone.wav\"\n  stretch: 1.5");
    let pitched = clip_project(
        "[2:1 - 3:1]",
        "  file: \"tone.wav\"\n  stretch: 1.5\n  pitch: 5",
    );
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    mix_with(&cache, &derived, &root, &plain);
    assert_eq!(derived.stats().bakes, 1);
    mix_with(&cache, &derived, &root, &plain);
    assert_eq!(derived.stats().bakes, 1, "an unchanged pitch must hit");
    mix_with(&cache, &derived, &root, &pitched);
    assert_eq!(derived.stats().bakes, 2, "the pitch must miss the key");
    mix_with(&cache, &derived, &root, &pitched);
    assert_eq!(
        derived.stats().bakes,
        2,
        "the pitched variant must be cached"
    );
    mix_with(&cache, &derived, &root, &plain);
    assert_eq!(
        derived.stats().bakes,
        2,
        "the unpitched variant is still there"
    );
    assert_eq!(cache.stats().decodes, 1, "one file, one decode");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_pitched_clip_needs_audible_media() {
    let root = temp_dir("pitch-missing");
    let project = clip_project("[2:1 - 4:1]", "  file: \"absent.wav\"\n  pitch: 12");
    let error = audio_clip::validate(&project, Some(&root)).unwrap_err();
    assert!(error.contains("audio clip 'hit'"), "{error}");
    assert!(error.contains("absent.wav"), "{error}");
    assert!(error.contains("file not found"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_pitched_clip_has_no_step_or_spill_at_its_boundaries() {
    let root = temp_dir("pitch-boundaries");
    let signal = GoldenSignal::tone(2.0, 440.0, 0.4);
    signal.write(&root.join("tone.wav"));
    let project = clip_project(
        "[2:1 - 5:1]",
        "  file: \"tone.wav\"\n  pitch: 12\n  fade_in: 5ms\n  fade_out: 5ms",
    );
    let rendered = render(&root, &project);
    let start = 2 * RATE as usize;
    let frames = 2 * RATE as usize;
    let guard = (5.0 * f64::from(RATE) / 1000.0) as usize;
    let peak = clip_region(&rendered, start, frames)
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.1, "the clip is silent: {peak}");
    let floor = peak * 0.01;
    for frame in start - guard..start {
        assert!(
            rendered[frame * 2].abs() <= floor,
            "energy before the clip at {frame}"
        );
    }
    for frame in start + frames..start + frames + guard {
        assert!(
            rendered[frame * 2].abs() <= floor,
            "energy after the clip at {frame}"
        );
    }
    // The fades make the first and last samples zero, so the edge cannot step.
    assert!(
        rendered[start * 2].abs() <= floor,
        "{}",
        rendered[start * 2]
    );
    assert!(
        rendered[(start + frames - 1) * 2].abs() <= floor,
        "{}",
        rendered[(start + frames - 1) * 2]
    );
    std::fs::remove_dir_all(root).unwrap();
}
