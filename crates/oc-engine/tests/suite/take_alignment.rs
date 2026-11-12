//! Take alignment: synthetic shifts, tempo drift, wrong notes and cache invalidation.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oc_dsl::ast::{Project, StretchMode};
use oc_engine::audio_clip;
use oc_engine::derived_cache::{DerivedCache, StretchKey};
use oc_engine::media_cache::{self, MediaCache};
use oc_engine::resample::ResampleQuality;
use oc_engine::stretch::StretchPlan;
use oc_engine::{align, stretch};

const RATE: u32 = 48_000;
const ONSET_TOLERANCE_MS: f64 = 2.0;
const LAST_ONSET_TOLERANCE_MS: f64 = 3.0;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-align", name)
}

fn reference_times() -> Vec<f64> {
    (0..12).map(|index| 0.1 + index as f64 * 0.25).collect()
}

fn click_train(onset_seconds: &[f64], duration_seconds: f64) -> Vec<f32> {
    let frames = (duration_seconds * f64::from(RATE)).ceil() as usize;
    let mut samples = vec![0.0_f32; frames * 2];
    for &at in onset_seconds {
        let frame = (at * f64::from(RATE)).round() as usize;
        if frame < frames {
            samples[frame * 2] = 0.9;
            samples[frame * 2 + 1] = 0.9;
        }
    }
    samples
}

fn write_wav(path: &Path, samples: &[f32]) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in samples.chunks_exact(2) {
        writer.write_sample(frame[0]).unwrap();
    }
    writer.finalize().unwrap();
}

fn load(path: &Path) -> Vec<f32> {
    audio_clip::load(path, RATE, ResampleQuality::DEFAULT_RENDER)
        .unwrap()
        .samples
        .as_ref()
        .clone()
}

fn project_with_take(properties: &str) -> Project {
    let text = format!(
        r#"@project "Take alignment"
  tempo: 120
@audio_clip take [1:1 - 5:1] | track: tape
  file: "take.wav"
{properties}
@track tape | instrument: audio/clip
"#
    );
    let project = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn render_take(root: &Path, project: &Project) -> Vec<f32> {
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let clip = project.audio_clip(0).unwrap().clone();
    let (_, length) = audio_clip::range_frames(project, &clip, RATE).unwrap();
    let mut buffer = vec![0.0_f32; length as usize * 2];
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut buffer,
        project,
        &clip,
        RATE,
        Some(root),
    )
    .unwrap();
    buffer
}

fn assert_onsets_match(
    expected_seconds: &[f64],
    detected: &[usize],
    tolerance_ms: f64,
    context: &str,
) {
    assert_eq!(
        detected.len(),
        expected_seconds.len(),
        "{context}: expected {} onsets, found {} at {detected:?}",
        expected_seconds.len(),
        detected.len()
    );
    let tolerance = (tolerance_ms * f64::from(RATE) / 1000.0).round() as i64;
    let mut worst = 0_i64;
    for (index, (&expected, &actual)) in expected_seconds.iter().zip(detected).enumerate() {
        let expected_frame = (expected * f64::from(RATE)).round() as i64;
        let error = actual as i64 - expected_frame;
        worst = worst.max(error.abs());
        assert!(
            error.abs() <= tolerance,
            "{context}: onset {index} at {actual}, expected {expected_frame} ({error} frames)"
        );
    }
    eprintln!(
        "{context}: {} onsets, max error {worst} frames ({:.3} ms)",
        detected.len(),
        worst as f64 * 1000.0 / f64::from(RATE)
    );
}

#[test]
fn the_detector_finds_impulse_samples() {
    let times = reference_times();
    let signal = click_train(&times, 3.2);
    let expected: Vec<usize> = times
        .iter()
        .map(|at| (at * f64::from(RATE)).round() as usize)
        .collect();
    assert_eq!(align::detect_onsets(&signal, RATE), expected);
}

#[test]
fn a_thirty_millisecond_shift_aligns_within_two_ms() {
    let root = temp_dir("shift");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at + 0.030).collect();
    let take = click_train(&shifted, 3.25);
    write_wav(&root.join("ref.wav"), &reference);
    write_wav(&root.join("take.wav"), &take);

    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    assert!(
        (alignment.tempo_ratio - 1.0).abs() < 1e-9,
        "tempo {}",
        alignment.tempo_ratio
    );
    assert!(
        (alignment.head_trim_seconds - 0.030).abs() < 1e-9,
        "trim {}",
        alignment.head_trim_seconds
    );
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    assert_eq!(correction.ratio, 1.0);
    assert!((correction.offset_seconds - 0.030).abs() < 1e-9);

    let mut project = project_with_take("");
    align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap();
    let rendered = render_take(&root, &project);
    assert_onsets_match(
        &times,
        &align::detect_onsets(&rendered, RATE),
        ONSET_TOLERANCE_MS,
        "shifted take",
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_one_and_a_half_percent_stretch_matches_the_last_onset_within_three_ms() {
    let root = temp_dir("stretch");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let stretched: Vec<f64> = times.iter().map(|at| at * 1.015).collect();
    let take = click_train(&stretched, 3.3);
    write_wav(&root.join("ref.wav"), &reference);
    write_wav(&root.join("take.wav"), &take);

    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    assert!(
        (alignment.tempo_ratio - 1.015).abs() < 1e-4,
        "tempo {}",
        alignment.tempo_ratio
    );
    assert!(
        (alignment.ratio - 1.0 / 1.015).abs() < 1e-4,
        "ratio {}",
        alignment.ratio
    );
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    assert!((correction.offset_seconds.abs() < 1e-9), "trim");

    let mut project = project_with_take("");
    align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap();
    let rendered = render_take(&root, &project);
    let detected = align::detect_onsets(&rendered, RATE);
    assert_onsets_match(&times, &detected, ONSET_TOLERANCE_MS, "stretched take");
    let last_error = detected
        .last()
        .unwrap()
        .abs_diff((times.last().unwrap() * f64::from(RATE)).round() as usize);
    assert!(
        last_error as f64 * 1000.0 / f64::from(RATE) <= LAST_ONSET_TOLERANCE_MS,
        "last onset off by {last_error} frames"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_wrong_note_is_ignored_instead_of_bending_the_take() {
    let root = temp_dir("wrong-note");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let mut shifted: Vec<f64> = times.iter().map(|at| at + 0.025).collect();
    // A second attack 35 ms after the fifth note: a wrong rhythm, not a shift.
    shifted.push(shifted[4] + 0.035);
    shifted.sort_by(|left, right| left.total_cmp(right));
    let take = click_train(&shifted, 3.25);
    write_wav(&root.join("ref.wav"), &reference);
    write_wav(&root.join("take.wav"), &take);

    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    assert!(
        (alignment.tempo_ratio - 1.0).abs() < 1e-3,
        "tempo {}",
        alignment.tempo_ratio
    );
    assert!(
        (alignment.head_trim_seconds - 0.025).abs() < 1e-3,
        "trim {}",
        alignment.head_trim_seconds
    );
    assert!(
        alignment.max_residual_seconds < 0.002,
        "residual {}",
        alignment.max_residual_seconds
    );
    assert_eq!(
        alignment.matched,
        times.len(),
        "the extra attack must be an outlier"
    );
    eprintln!(
        "wrong-note take: tempo {:.6}, trim {:.1} ms, {} inliers, residual {:.3} ms",
        alignment.tempo_ratio,
        alignment.head_trim_seconds * 1000.0,
        alignment.matched,
        alignment.max_residual_seconds * 1000.0,
    );
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    let mut project = project_with_take("");
    align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap();
    let rendered = render_take(&root, &project);
    let detected = align::detect_onsets(&rendered, RATE);
    let tolerance = (ONSET_TOLERANCE_MS * f64::from(RATE) / 1000.0).round() as i64;
    for (index, &expected) in times.iter().enumerate() {
        let expected_frame = (expected * f64::from(RATE)).round() as i64;
        let nearest = detected
            .iter()
            .map(|actual| (*actual as i64 - expected_frame).abs())
            .min()
            .unwrap();
        assert!(
            nearest <= tolerance,
            "reference onset {index} has no aligned attack ({nearest} frames)"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn aligning_an_already_aligned_take_is_a_bit_exact_no_op() {
    let root = temp_dir("identity");
    let times = reference_times();
    let signal = click_train(&times, 3.2);
    write_wav(&root.join("ref.wav"), &signal);
    write_wav(&root.join("take.wav"), &signal);

    let alignment = align::analyze(&signal, &signal, RATE).unwrap();
    assert!(alignment.is_identity(), "{alignment:?}");
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    assert!(correction.is_identity());

    let mut project = project_with_take("");
    let before = render_take(&root, &project);
    align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap();
    assert_eq!(project.audio_clip(0).unwrap().offset_seconds, 0.0);
    assert_eq!(project.audio_clip(0).unwrap().stretch, 1.0);
    let after = render_take(&root, &project);
    assert_eq!(before.len(), after.len());
    assert!(
        before
            .iter()
            .zip(&after)
            .all(|(left, right)| left.to_bits() == right.to_bits())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_alignment_change_is_a_new_derived_cache_key_and_a_new_bake() {
    let root = temp_dir("cache");
    let times = reference_times();
    let take_times: Vec<f64> = times.iter().map(|at| at * 1.015).collect();
    let take = click_train(&take_times, 3.3);
    write_wav(&root.join("ref_a.wav"), &click_train(&times, 3.2));
    let slower: Vec<f64> = times.iter().map(|at| at * 1.02).collect();
    write_wav(&root.join("ref_b.wav"), &click_train(&slower, 3.3));
    write_wav(&root.join("take.wav"), &take);
    let take_path = root.join("take.wav");

    let alignment_a = align::analyze(&load(&root.join("ref_a.wav")), &take, RATE).unwrap();
    let alignment_b = align::analyze(&load(&root.join("ref_b.wav")), &take, RATE).unwrap();
    assert_ne!(alignment_a.ratio, alignment_b.ratio);

    let media = media_cache::media_key(&take_path).unwrap();
    let source = Arc::new(load(&take_path));
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let mode = StretchMode::Percussive;
    let key_a = StretchKey::new(
        media.clone(),
        RATE,
        alignment_a.ratio,
        mode,
        stretch::ALGO_VERSION,
    );
    let key_b = StretchKey::new(
        media.clone(),
        RATE,
        alignment_b.ratio,
        mode,
        stretch::ALGO_VERSION,
    );
    assert_ne!(key_a, key_b);
    let baked_a = derived
        .stretch(
            &media,
            source.clone(),
            RATE,
            &StretchPlan::stretched(alignment_a.ratio, mode),
            ResampleQuality::DEFAULT_RENDER,
        )
        .unwrap();
    assert_eq!(derived.stats().bakes, 1);
    let baked_b = derived
        .stretch(
            &media,
            source,
            RATE,
            &StretchPlan::stretched(alignment_b.ratio, mode),
            ResampleQuality::DEFAULT_RENDER,
        )
        .unwrap();
    assert_eq!(
        derived.stats().bakes,
        2,
        "the second alignment must bake instead of serving the first variant"
    );
    assert!(
        baked_a.len() != baked_b.len()
            || baked_a
                .iter()
                .zip(baked_b.iter())
                .any(|(left, right)| left.to_bits() != right.to_bits())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_out_of_range_correction_fails_instead_of_clamping() {
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let fast: Vec<f64> = times.iter().map(|at| at * 0.2).collect();
    let take = click_train(&fast, 0.75);
    let error = align::analyze(&reference, &take, RATE).unwrap_err();
    assert!(error.contains("outside 0.25..=4"), "{error}");
    assert!(error.contains("5.0"), "{error}");

    let slow: Vec<f64> = times.iter().map(|at| at * 5.0).collect();
    let take = click_train(&slow, 15.0);
    let error = align::analyze(&reference, &take, RATE).unwrap_err();
    assert!(error.contains("outside 0.25..=4"), "{error}");
}

#[test]
fn a_negative_head_trim_is_refused() {
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let early: Vec<f64> = times.iter().map(|at| at - 0.030).collect();
    let take = click_train(&early, 3.2);
    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    let error = align::correction(&alignment, 0.0, 0.0, RATE).unwrap_err();
    assert!(error.contains("offset cannot be negative"), "{error}");
}

#[test]
fn writing_checks_the_stretch_surface() {
    let mut project = project_with_take("  mode: off");
    let correction = align::ClipCorrection {
        ratio: 1.01,
        offset_seconds: 0.0,
    };
    let error = align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap_err();
    assert!(error.contains("stretch mode"), "{error}");

    let mut project = project_with_take("  tempo_follow: true");
    let error = align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap_err();
    assert!(error.contains("tempo_follow"), "{error}");
    let shift_only = align::ClipCorrection {
        ratio: 1.0,
        offset_seconds: 0.01,
    };
    let error = align::write_into(project.audio_clip_mut(0).unwrap(), &shift_only).unwrap_err();
    assert!(error.contains("tempo_follow"), "{error}");
}

#[test]
fn a_later_clip_start_needs_more_head_trim() {
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at + 0.030).collect();
    let take = click_train(&shifted, 3.25);
    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    let correction = align::correction(&alignment, 0.0, 0.1, RATE).unwrap();
    assert!((correction.offset_seconds - 0.130).abs() < 1e-9);
    let error = align::correction(&alignment, 0.1, 0.0, RATE).unwrap_err();
    assert!(error.contains("offset cannot be negative"), "{error}");
}

#[test]
fn a_shift_and_a_stretch_apply_together() {
    let root = temp_dir("shift-stretch");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at * 1.015 + 0.020).collect();
    let take = click_train(&shifted, 3.3);
    write_wav(&root.join("ref.wav"), &reference);
    write_wav(&root.join("take.wav"), &take);

    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    assert!((alignment.tempo_ratio - 1.015).abs() < 1e-4);
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    assert!((correction.offset_seconds - 0.020 / 1.015).abs() < 1e-4);

    let mut project = project_with_take("");
    align::write_into(project.audio_clip_mut(0).unwrap(), &correction).unwrap();
    let rendered = render_take(&root, &project);
    assert_onsets_match(
        &times,
        &align::detect_onsets(&rendered, RATE),
        ONSET_TOLERANCE_MS,
        "shifted and stretched take",
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_silent_take_has_nothing_to_align() {
    let silence = vec![0.0_f32; RATE as usize * 2];
    let error = align::analyze(&silence, &silence, RATE).unwrap_err();
    assert!(error.contains("no detected onsets"), "{error}");
}

fn polarity_project() -> Project {
    let text = r#"@project "Take polarity"
  tempo: 120
@audio_clip ref [1:1 - 5:1] | track: ref
  file: "ref.wav"
@audio_clip take [1:1 - 5:1] | track: take
  file: "take.wav"
@track ref | instrument: audio/clip
@track take | instrument: audio/clip
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn render_clip(root: &Path, project: &Project, index: usize) -> Vec<f32> {
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let clip = project.audio_clip(index).unwrap().clone();
    let (start, length) = audio_clip::range_frames(project, &clip, RATE).unwrap();
    let mut buffer = vec![0.0_f32; (start + length) as usize * 2];
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut buffer,
        project,
        &clip,
        RATE,
        Some(root),
    )
    .unwrap();
    buffer
}

fn correlation(reference: &[f32], rendered: &[f32]) -> f64 {
    let frames = (reference.len() / 2).min(rendered.len() / 2);
    let mut dot = 0.0_f64;
    let mut reference_energy = 0.0_f64;
    let mut rendered_energy = 0.0_f64;
    for frame in 0..frames {
        for channel in 0..2 {
            let a = f64::from(reference[frame * 2 + channel]);
            let b = f64::from(rendered[frame * 2 + channel]);
            dot += a * b;
            reference_energy += a * a;
            rendered_energy += b * b;
        }
    }
    dot / (reference_energy.sqrt() * rendered_energy.sqrt())
}

#[test]
fn an_inverted_late_take_measures_negative_and_the_flag_restores_polarity() {
    let root = temp_dir("polarity");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at + 0.030).collect();
    let mut take = click_train(&shifted, 3.25);
    for sample in &mut take {
        *sample = -*sample;
    }
    write_wav(&root.join("ref.wav"), &reference);
    write_wav(&root.join("take.wav"), &take);

    let alignment = align::analyze(&reference, &take, RATE).unwrap();
    assert!(
        alignment.correlation < -0.99,
        "correlation {}",
        alignment.correlation
    );
    assert!(alignment.is_inverted());
    let correction = align::correction(&alignment, 0.0, 0.0, RATE).unwrap();
    assert!((correction.offset_seconds - 0.030).abs() < 1e-9);

    let mut project = polarity_project();
    align::write_into(project.audio_clip_mut(1).unwrap(), &correction).unwrap();
    let raw = render_clip(&root, &project, 1);
    let raw_correlation = correlation(&reference, &raw);
    assert!(
        raw_correlation < -0.99,
        "unflipped correlation {raw_correlation}"
    );

    project.audio_clip_mut(1).unwrap().invert = true;
    let flipped = render_clip(&root, &project, 1);
    let flipped_correlation = correlation(&reference, &flipped);
    assert!(
        flipped_correlation > 0.99,
        "flipped correlation {flipped_correlation}"
    );
    eprintln!(
        "polarity: correlation {:.3}, unflipped {raw_correlation:.3}, flipped {flipped_correlation:.3}",
        alignment.correlation
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn crossfade_project(crossfade: &str) -> Project {
    let text = format!(
        r#"@project "Comp crossfade"
  tempo: 375
  time_signature: 16/16
@audio_clip left [1:1:0 - 1:3:0] | track: tape
  file: "left.wav"
{crossfade}@audio_clip right [1:2:3 - 1:4:0] | track: tape
  file: "right.wav"
{crossfade}@track tape | instrument: audio/clip
"#
    );
    let project = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn render_track(root: &Path, project: &Project) -> Vec<f32> {
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let length = audio_clip::project_end_frame(project, RATE).unwrap() as usize;
    let mut buffer = vec![0.0_f32; length * 2];
    for clip in project.audio_clips() {
        audio_clip::mix_into_track_with_caches(
            &cache,
            &derived,
            &mut buffer,
            project,
            clip,
            RATE,
            Some(root),
        )
        .unwrap();
    }
    buffer
}

fn sine(frames: usize, frequency: f64, amplitude: f64, phase: f64) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let value =
            amplitude * (2.0 * PI * frequency * frame as f64 / f64::from(RATE) + phase).sin();
        samples.push(value as f32);
        samples.push(value as f32);
    }
    samples
}

fn peak_db(samples: &[f32], start: usize, end: usize) -> f64 {
    let peak = samples[start * 2..end * 2]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    20.0 * f64::from(peak).max(1e-12).log10()
}

#[test]
fn a_ten_millisecond_crossfade_keeps_the_seam_level_and_removes_the_step() {
    let root = temp_dir("crossfade");
    let left_frames = (0.080 * f64::from(RATE)).round() as usize;
    let right_frames = (0.050 * f64::from(RATE)).round() as usize;
    write_wav(
        &root.join("left.wav"),
        &sine(left_frames, 100.0, 0.001, 0.0),
    );
    // 70 ms is seven periods of 100 Hz, so the second clip is the cosine there.
    write_wav(
        &root.join("right.wav"),
        &sine(right_frames, 100.0, 0.001, PI / 2.0),
    );

    let project = crossfade_project("  crossfade: 10ms\n");
    let mixed = render_track(&root, &project);

    let window = (0.020 * f64::from(RATE)) as usize;
    let mut levels = Vec::new();
    let mut start = (0.010 * f64::from(RATE)) as usize;
    while start + window <= mixed.len() / 2 {
        levels.push(peak_db(&mixed, start, start + window));
        start += window;
    }
    let lowest = levels.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (highest - lowest).abs() <= 0.5,
        "seam level varies {:.3} dB: {levels:?}",
        highest - lowest
    );

    let seam_start = (0.070 * f64::from(RATE)) as usize;
    let seam_end = (0.080 * f64::from(RATE)) as usize;
    let mut seam_step = 0.0_f32;
    for frame in seam_start..seam_end {
        for channel in 0..2 {
            let step = (mixed[frame * 2 + channel] - mixed[(frame - 1) * 2 + channel]).abs();
            seam_step = seam_step.max(step);
        }
    }
    assert!(
        seam_step < 1e-4,
        "seam sample step {:.1} dBFS",
        20.0 * f64::from(seam_step).log10()
    );

    let summed = render_track(&root, &crossfade_project(""));
    let crossfaded_level = peak_db(&mixed, seam_start, seam_end);
    let summed_level = peak_db(&summed, seam_start, seam_end);
    assert!(
        summed_level - crossfaded_level > 0.5,
        "control did not bump: {crossfaded_level:.3} vs {summed_level:.3}"
    );
    let mut summed_step = 0.0_f32;
    for frame in seam_start..seam_end {
        for channel in 0..2 {
            let step = (summed[frame * 2 + channel] - summed[(frame - 1) * 2 + channel]).abs();
            summed_step = summed_step.max(step);
        }
    }
    assert!(
        summed_step > 1e-4,
        "control had no seam step: {summed_step}"
    );
    eprintln!(
        "crossfade: levels {lowest:.3}..{highest:.3} dBFS, seam step {:.1} dBFS; summed bump {:+.2} dB, step {:.1} dBFS",
        20.0 * f64::from(seam_step).log10(),
        summed_level - crossfaded_level,
        20.0 * f64::from(summed_step).log10()
    );
    std::fs::remove_dir_all(root).unwrap();
}
