//! `@vocal_harmony` through the real pipeline: analysis → resolve → bake → mix.
//!
//! Every signal is generated here. The harmony's pitch, onset and level are
//! measured on the rendered audio with an autocorrelation and an energy oracle
//! that share no code with the detector or the resolver.

use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::vocal_analysis::{self, ClipAnalysis};
use oc_engine::{audio_clip, vocal_harmony, vocal_tune, wav};

const RATE: u32 = 48_000;
/// C4 D4 E4 F4 G4: a third above each note is E4 F4 G4 A4 B4.
const MELODY: [f64; 5] = [60.0, 62.0, 64.0, 65.0, 67.0];
const THIRD_ABOVE: [f64; 5] = [64.0, 65.0, 67.0, 69.0, 71.0];

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-harmony", name)
}

fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

fn cents(measured: f64, target: f64) -> f64 {
    1_200.0 * (measured / target).log2()
}

/// Formant-ish tone: fundamental plus a weaker second and third harmonic.
fn tone(f0: f64, seconds: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    (0..frames)
        .map(|index| {
            let phase = std::f64::consts::TAU * f0 * index as f64 / f64::from(RATE);
            0.30 * phase.sin() + 0.12 * (2.0 * phase).sin() + 0.06 * (3.0 * phase).sin()
        })
        .collect()
}

/// A five-note take, one note every 0.5 s, written to `take.wav`.
fn write_melody(dir: &Path, detune_cents: f64) {
    let mut samples = vec![0.0_f32; (3.2 * f64::from(RATE)) as usize * 2];
    for (index, midi) in MELODY.iter().enumerate() {
        let start = 0.2 + index as f64 * 0.5;
        let offset = (start * f64::from(RATE)) as usize * 2;
        for (slot, value) in tone(hz(*midi) * 2f64.powf(detune_cents / 1_200.0), 0.45)
            .into_iter()
            .enumerate()
        {
            let at = offset + slot * 2;
            if at + 1 >= samples.len() {
                break;
            }
            samples[at] = value as f32;
            samples[at + 1] = value as f32;
        }
    }
    wav::write_stereo_wav(dir.join("take.wav"), &samples, RATE).unwrap();
}

fn parse(text: &str) -> Project {
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn project_text(block: &str) -> String {
    format!(
        "@project \"Vocal harmony\"\n  tempo: 120\n  key: C major\n@audio_clip lead [1:1 - 3:1] | track: vocal\n  file: \"take.wav\"\n@vocal_tune lead_tune | clip: lead\n  scale: C major\n{block}@track vocal | instrument: audio/clip\n"
    )
}

fn analysis_of(project: &Project, dir: &Path) -> ClipAnalysis {
    let clip = vocal_analysis::resolve_clip(project, "lead").unwrap();
    vocal_analysis::analyze_clip(
        project,
        clip,
        RATE,
        Some(dir),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        None,
    )
    .unwrap()
}

fn lead_window(project: &Project, dir: &Path) -> Vec<f32> {
    let clip = vocal_analysis::resolve_clip(project, "lead").unwrap();
    vocal_analysis::clip_stereo_playback(project, clip, RATE, Some(dir))
        .unwrap()
        .1
}

fn harmony_window(project: &Project, dir: &Path) -> Vec<f32> {
    let clip = vocal_analysis::resolve_clip(project, "lead").unwrap();
    vocal_harmony::render_harmony_window(project, clip, RATE, Some(dir), None)
        .unwrap()
        .expect("a harmony voice")
}

fn mono(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(2).copied().collect()
}

/// Normalised autocorrelation peak inside ±5 % of `expected`.
fn measured_f0(samples: &[f32], start: usize, expected: f64, width: usize) -> f64 {
    let rate = f64::from(RATE);
    let nominal = rate / expected;
    let low = (nominal * 0.95).floor().max(2.0) as usize;
    let high = (nominal * 1.05).ceil() as usize;
    let mut curve: Vec<(f64, f64)> = Vec::new();
    for lag in low..=high {
        if start + width + lag >= samples.len() {
            break;
        }
        let mut ab = 0.0;
        let mut aa = 0.0;
        let mut bb = 0.0;
        for offset in 0..width {
            let a = f64::from(samples[start + offset]);
            let b = f64::from(samples[start + offset + lag]);
            ab += a * b;
            aa += a * a;
            bb += b * b;
        }
        curve.push((lag as f64, ab / (aa * bb).sqrt().max(1e-18)));
    }
    let Some(top) = curve
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.1.total_cmp(&right.1.1))
        .map(|(index, _)| index)
    else {
        return expected;
    };
    let mut lag = curve[top].0;
    if top > 0 && top + 1 < curve.len() {
        let (left, mid, right) = (curve[top - 1].1, curve[top].1, curve[top + 1].1);
        let denominator = left - 2.0 * mid + right;
        if denominator.abs() > 1e-18 {
            lag += 0.5 * (left - right) / denominator;
        }
    }
    rate / lag
}

/// First frame above 2 % of the window's peak, in seconds.
fn energy_onset(samples: &[f32], from_s: f64, to_s: f64) -> f64 {
    let frames = samples.len() / 2;
    let from = ((from_s * f64::from(RATE)) as usize).min(frames);
    let to = ((to_s * f64::from(RATE)) as usize).min(frames);
    let level = |frame: usize| samples[frame * 2].abs().max(samples[frame * 2 + 1].abs());
    let peak = (from..to).fold(0.0_f32, |worst, frame| worst.max(level(frame)));
    let threshold = peak * 0.02;
    for frame in from..to {
        if level(frame) >= threshold {
            return frame as f64 / f64::from(RATE);
        }
    }
    f64::NAN
}

/// RMS of one span, all channels.
fn span_rms(samples: &[f32], from_s: f64, to_s: f64) -> f64 {
    let frames = samples.len() / 2;
    let from = ((from_s * f64::from(RATE)) as usize).min(frames);
    let to = ((to_s * f64::from(RATE)) as usize).min(frames);
    let mut energy = 0.0_f64;
    let mut count = 0usize;
    for frame in from..to {
        for channel in 0..2 {
            let value = f64::from(samples[frame * 2 + channel]);
            energy += value * value;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    (energy / count as f64).sqrt()
}

/// The acceptance table: pitch, onset and level of the harmony against E4 F4 G4
/// A4 B4, the lead's onsets and the lead's RMS.
#[test]
fn a_third_above_a_c_major_melody_lands_on_the_scale_with_the_lead() {
    let dir = temp_dir("third");
    write_melody(&dir, 0.0);
    let text = project_text("@vocal_harmony third | clip: lead\n  interval: 2\n  level: -6dB\n");
    let project = parse(&text);
    let analysis = analysis_of(&project, &dir);
    assert_eq!(
        analysis.analysis.notes.len(),
        MELODY.len(),
        "detected {:?}",
        analysis
            .analysis
            .notes
            .iter()
            .map(|note| note.start_s)
            .collect::<Vec<_>>()
    );

    let lead = lead_window(&project, &dir);
    let harmony = harmony_window(&project, &dir);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let block = vocal_harmony::harmony_for_clip(&project, clip).unwrap();
    let plan = vocal_harmony::resolve_harmony(&project, clip, block, &analysis).unwrap();
    assert!(!plan.chromatic, "the clip has a scale");
    assert_eq!(plan.notes.len(), MELODY.len());
    assert!(
        plan.warnings.is_empty(),
        "unexpected warnings: {:?}",
        plan.warnings
    );

    let harmony_mono = mono(&harmony);
    println!("note | target | measured | error ct | onset ms | level dB");
    let mut worst_cents = 0.0_f64;
    let mut worst_onset_ms = 0.0_f64;
    let mut worst_level_db = 0.0_f64;
    for (index, note) in analysis.analysis.notes.iter().enumerate() {
        let expected = hz(THIRD_ABOVE[index]);
        let from = note.start_s + 0.045;
        let width = (f64::from(RATE) * 2.0 / expected) as usize;
        let measured = measured_f0(
            &harmony_mono,
            (from * f64::from(RATE)) as usize,
            expected,
            width,
        );
        let error = cents(measured, expected);
        worst_cents = worst_cents.max(error.abs());

        let lead_onset = energy_onset(&lead, note.start_s - 0.02, note.start_s + 0.15);
        let harmony_onset = energy_onset(&harmony, note.start_s - 0.02, note.start_s + 0.15);
        let onset_ms = (harmony_onset - lead_onset) * 1000.0;
        worst_onset_ms = worst_onset_ms.max(onset_ms.abs());

        let lead_rms = span_rms(&lead, note.start_s, note.end_s);
        let harmony_rms = span_rms(&harmony, note.start_s, note.end_s);
        let level_db = 20.0 * (harmony_rms / lead_rms.max(1e-12)).log10();
        worst_level_db = worst_level_db.max((level_db + 6.0).abs());

        println!(
            "{} | {:.1} Hz | {:.1} Hz | {:+.3} | {:+.2} | {:+.2}",
            index, expected, measured, error, onset_ms, level_db
        );
        assert!(
            error.abs() <= 10.0,
            "note {index}: {error:+.2} cents from {expected:.1} Hz"
        );
        assert!(
            onset_ms.abs() <= 2.0,
            "note {index}: onset {onset_ms:+.2} ms from the lead"
        );
        assert!(
            (level_db + 6.0).abs() <= 0.2,
            "note {index}: {level_db:+.2} dB, requested -6 dB"
        );
        assert_eq!(
            plan.notes[index].target_cents,
            THIRD_ABOVE[index] * 100.0,
            "note {index}: resolved target"
        );
    }
    println!("worst: {worst_cents:.3} cents, {worst_onset_ms:.2} ms, {worst_level_db:.2} dB");

    // The lead is untouched, and the mix is exactly the lead plus the voice.
    let plain = parse(&project_text(""));
    let plain_clip = vocal_analysis::resolve_clip(&plain, "lead").unwrap();
    let plain_window = vocal_tune::render_window(&plain, plain_clip, RATE, Some(&dir))
        .unwrap()
        .expect("the tune block bakes");
    let tuned_clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let tuned_window = vocal_tune::render_window(&project, tuned_clip, RATE, Some(&dir))
        .unwrap()
        .expect("the tune block bakes");
    assert_eq!(plain_window.len(), tuned_window.len());
    for (index, (left, right)) in plain_window.iter().zip(&tuned_window).enumerate() {
        assert_eq!(
            left.to_bits(),
            right.to_bits(),
            "the harmony changed lead sample {index}"
        );
    }
    let mut plain_mix = vec![0.0_f32; tuned_window.len()];
    audio_clip::mix_into_track_with_caches(
        oc_engine::media_cache::shared(),
        oc_engine::derived_cache::shared(),
        &mut plain_mix,
        &plain,
        plain_clip,
        RATE,
        Some(&dir),
    )
    .unwrap();
    let mut with_harmony = vec![0.0_f32; tuned_window.len()];
    audio_clip::mix_into_track_with_caches(
        oc_engine::media_cache::shared(),
        oc_engine::derived_cache::shared(),
        &mut with_harmony,
        &project,
        tuned_clip,
        RATE,
        Some(&dir),
    )
    .unwrap();
    for (index, (mixed, (lead_sample, voice))) in with_harmony
        .iter()
        .zip(plain_mix.iter().zip(&harmony))
        .enumerate()
    {
        let expected = lead_sample + voice;
        assert_eq!(
            mixed.to_bits(),
            expected.to_bits(),
            "sample {index}: the mix is not the lead plus the voice"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_same_project_bakes_the_same_bytes() {
    let dir = temp_dir("determinism");
    write_melody(&dir, 0.0);
    let text = project_text("@vocal_harmony third | clip: lead\n  interval: 2\n");
    let project = parse(&text);
    let first = harmony_window(&project, &dir);
    // Remove only this fixture's bake; other tests share the process cache.
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let (key, _) = vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(&dir)).unwrap();
    let lead_key = vocal_tune::BakeKey::new(
        key,
        vocal_tune::tune_for_clip(&project, clip).unwrap(),
        None,
    );
    let key = vocal_harmony::HarmonyKey::new(
        lead_key,
        vocal_harmony::harmony_for_clip(&project, clip).unwrap(),
    );
    let cached = vocal_analysis::default_cache_dir()
        .unwrap()
        .join("bake")
        .join(key.dir_name())
        .join(key.file_name(&clip.name));
    std::fs::remove_file(cached).unwrap();
    let second = harmony_window(&project, &dir);
    assert_eq!(first.len(), second.len());
    for (index, (left, right)) in first.iter().zip(&second).enumerate() {
        assert_eq!(left.to_bits(), right.to_bits(), "sample {index} moved");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_interval_beyond_twelve_semitones_names_the_note() {
    let dir = temp_dir("range");
    write_melody(&dir, 0.0);
    let text = project_text("@vocal_harmony wide | clip: lead\n  interval: 8\n");
    let project = parse(&text);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let error =
        vocal_harmony::render_harmony_window(&project, clip, RATE, Some(&dir), None).unwrap_err();
    println!("refusal: {error}");
    assert!(error.contains("C4 at "), "{error}");
    assert!(error.contains("would move"), "{error}");
    assert!(error.contains("+1400 cents"), "{error}");
    assert!(error.contains("±1200 cents"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn without_a_scale_the_interval_counts_chromatic_semitones() {
    let dir = temp_dir("chromatic");
    write_melody(&dir, 20.0);
    let text = "@project \"Chromatic\"\n  tempo: 120\n@audio_clip lead [1:1 - 3:1] | track: vocal\n  file: \"take.wav\"\n@vocal_harmony two | clip: lead\n  interval: 2\n@track vocal | instrument: audio/clip\n";
    let project = parse(text);
    let _warnings = crate::RENDER_WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    oc_engine::render_warnings::clear();
    let harmony = harmony_window(&project, &dir);
    let warnings = oc_engine::render_warnings::take();
    assert!(
        warnings.iter().any(|warning| warning
            .contains("no scale is active for clip 'lead'; interval 2 counts chromatic semitones")),
        "{warnings:?}"
    );
    let analysis = analysis_of(&project, &dir);
    let mono_harmony = mono(&harmony);
    for (index, note) in analysis.analysis.notes.iter().enumerate() {
        // The voice keeps the lead's 20 cent deviation: nothing quantises it.
        let expected = hz(MELODY[index] + 2.0) * 2f64.powf(20.0 / 1_200.0);
        let from = note.start_s + 0.045;
        let width = (f64::from(RATE) * 2.0 / expected) as usize;
        let measured = measured_f0(
            &mono_harmony,
            (from * f64::from(RATE)) as usize,
            expected,
            width,
        );
        let error = cents(measured, expected);
        assert!(error.abs() <= 10.0, "note {index}: {error:+.2} cents");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_harmony_naming_a_missing_clip_is_a_readable_error() {
    let text = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"take.wav\"\n@vocal_harmony third | clip: ghost\n@track vocal | instrument: audio/clip\n";
    let project = oc_dsl::parse(text).unwrap();
    let error = oc_dsl::validate(&project).unwrap_err();
    let message = error
        .iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    println!("error: {message}");
    assert!(
        message.contains("Unknown audio clip reference: 'ghost' (in vocal harmony 'third')"),
        "{message}"
    );
}

#[test]
fn a_low_confidence_note_is_left_out_of_the_voice() {
    let dir = temp_dir("confidence");
    write_melody(&dir, 0.0);
    let text = project_text("@vocal_harmony third | clip: lead\n  interval: 2\n");
    let project = parse(&text);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let mut analysis = analysis_of(&project, &dir);
    let block = vocal_harmony::harmony_for_clip(&project, clip).unwrap();
    let all = vocal_harmony::resolve_harmony(&project, clip, block, &analysis).unwrap();
    assert_eq!(all.notes.len(), MELODY.len());
    // Sink one note's confidence and the voice skips it.
    analysis.analysis.notes[2].confidence = 0.2;
    let partial = vocal_harmony::resolve_harmony(&project, clip, block, &analysis).unwrap();
    assert_eq!(partial.notes.len(), MELODY.len() - 1);
    assert_eq!(partial.notes[2].detected_index, 3);
    std::fs::remove_dir_all(dir).unwrap();
}
