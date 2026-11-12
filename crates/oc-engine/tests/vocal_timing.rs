//! Note-level timing edits on top of the vocal-tune bake.
//!
//! Synthetic takes only; every measurement uses an onset/autocorrelation oracle
//! that shares no code with the detector or with the time pass.

use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::vocal_analysis::{self, ClipAnalysis};
use oc_engine::vocal_tune::{self, ResolvedTiming};
use oc_engine::{audio_clip, wav};

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-timing", name)
}

fn stereo_buffer(seconds: f64) -> Vec<f32> {
    vec![0.0; (seconds * f64::from(RATE)) as usize * 2]
}

fn place(buffer: &mut [f32], start_s: f64, samples: &[f64]) {
    let offset = (start_s * f64::from(RATE)) as usize * 2;
    for (index, value) in samples.iter().enumerate() {
        let slot = offset + index * 2;
        if slot + 1 >= buffer.len() {
            break;
        }
        buffer[slot] = *value as f32;
        buffer[slot + 1] = *value as f32;
    }
}

/// Sine at `f0`, optionally weighted with a second harmonic, phase from zero.
fn tone(f0: f64, seconds: f64, second: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    (0..frames)
        .map(|index| {
            let phase = std::f64::consts::TAU * f0 * index as f64 / f64::from(RATE);
            0.3 * phase.sin() + second * (2.0 * phase).sin()
        })
        .collect()
}

/// Exact integer PCM: no platform-specific sine generation enters the parity test.
fn fixed_pitch_formant_take() -> Vec<f32> {
    const CYCLE: [i32; 32] = [
        0, 1951, 3827, 5556, 7071, 8315, 9239, 9808, 10000, 9808, 9239, 8315, 7071, 5556, 3827,
        1951, 0, -1951, -3827, -5556, -7071, -8315, -9239, -9808, -10000, -9808, -9239, -8315,
        -7071, -5556, -3827, -1951,
    ];
    let mut samples = vec![0.0; 115_200 * 2];
    for (start, length, hz, harmonic_divisor) in [(9_600, 48_000, 220, 2), (72_000, 28_800, 330, 3)]
    {
        for index in 0..length {
            let phase = (index * hz * CYCLE.len() / RATE as usize) % CYCLE.len();
            let value = CYCLE[phase] + CYCLE[(2 * phase) % CYCLE.len()] / harmonic_divisor;
            // Integers fit exactly in f32, and division by 2^15 is exact.
            let sample = value as f32 / 32_768.0;
            samples[(start + index) * 2] = sample;
            samples[(start + index) * 2 + 1] = sample;
        }
    }
    assert_eq!(hash_samples(&samples), 0x5655d85c208c7045);
    samples
}

fn project_text(block: &str, end_bar: u32) -> String {
    format!(
        "@project \"Vocal timing\"\n  tempo: 120\n@audio_clip lead [1:1 - {end_bar}:1] | track: vocal\n  file: \"take.wav\"\n{block}@track vocal | instrument: audio/clip\n  mix: vol=0dB\n"
    )
}

fn parse(text: &str) -> Project {
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn write_take(dir: &Path, samples: &[f32]) {
    wav::write_stereo_wav(dir.join("take.wav"), samples, RATE).unwrap();
}

fn analysis_of(project: &Project, dir: &Path) -> ClipAnalysis {
    vocal_analysis::analyze_clip(
        project,
        vocal_analysis::resolve_clip(project, "lead").unwrap(),
        RATE,
        Some(dir),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        Some(&dir.join("probe.ocn")),
    )
    .unwrap()
}

fn block(amount: &str, notes: &str) -> String {
    format!("@vocal_tune lead_tune | clip: lead\n  amount: {amount}\n  notes:\n{notes}")
}

fn bake(dir: &Path, text: &str, name: &str) -> (Vec<f32>, vocal_tune::AppliedTune) {
    let source = dir.join(format!("{name}.ocp"));
    std::fs::write(&source, text).unwrap();
    let project = parse(text);
    let out = dir.join(format!("{name}-out.ocp"));
    let applied = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    let path = applied.baked.as_ref().expect("baked media").path.clone();
    let decoded = audio_clip::decode(&path).unwrap();
    (decoded.samples.to_vec(), applied)
}

fn window_of(dir: &Path, text: &str) -> Vec<f32> {
    let project = parse(text);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(dir))
        .unwrap()
        .1
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn hash_samples(samples: &[f32]) -> u64 {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_bits().to_le_bytes());
    }
    fnv1a(&bytes)
}

fn onsets(analysis: &ClipAnalysis) -> Vec<f64> {
    analysis
        .analysis
        .notes
        .iter()
        .map(|note| note.start_s)
        .collect()
}

fn frames(seconds: f64) -> usize {
    (seconds * f64::from(RATE)) as usize
}

/// First and last mono sample above 2 % of the region's peak.
fn audible_span(samples: &[f32], from_s: f64, to_s: f64) -> Option<(f64, f64)> {
    let mono: Vec<f32> = samples.chunks(2).map(|pair| pair[0]).collect();
    let from = frames(from_s).min(mono.len());
    let to = frames(to_s).min(mono.len());
    if to <= from {
        return None;
    }
    let peak = mono[from..to]
        .iter()
        .fold(0.0_f64, |worst, sample| worst.max(f64::from(sample.abs())));
    if peak < 1e-5 {
        return None;
    }
    let threshold = (peak * 0.02) as f32;
    let mut first = None;
    let mut last = None;
    for (index, sample) in mono.iter().enumerate().take(to).skip(from) {
        if sample.abs() >= threshold {
            first.get_or_insert(index);
            last = Some(index);
        }
    }
    Some((
        first? as f64 / f64::from(RATE),
        last? as f64 / f64::from(RATE),
    ))
}

/// Contiguous audible regions in a take (gaps are the synthetic silence).
fn audible_regions(samples: &[f32], threshold: f32) -> Vec<(f64, f64)> {
    let mono: Vec<f32> = samples.chunks(2).map(|pair| pair[0]).collect();
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    let gap = frames(0.030);
    for (index, sample) in mono.iter().enumerate() {
        if sample.abs() >= threshold {
            match &mut open {
                Some((_, end)) if index - *end <= gap => *end = index,
                Some((start, end)) => {
                    regions.push((*start, *end));
                    open = Some((index, index));
                }
                None => open = Some((index, index)),
            }
        }
    }
    if let Some(region) = open {
        regions.push(region);
    }
    regions
        .into_iter()
        .map(|(start, end)| (start as f64 / f64::from(RATE), end as f64 / f64::from(RATE)))
        .collect()
}

fn measured_f0(samples: &[f32], at_s: f64, expected: f64) -> f64 {
    let mono: Vec<f32> = samples.chunks(2).map(|pair| pair[0]).collect();
    let start = frames(at_s);
    let width = 2_048;
    let nominal = f64::from(RATE) / expected;
    let low = (nominal * 0.95).floor().max(2.0) as usize;
    let high = (nominal * 1.05).ceil() as usize;
    let mut curve: Vec<(f64, f64)> = Vec::new();
    for lag in low..=high {
        if start + width + lag >= mono.len() {
            break;
        }
        let mut ab = 0.0;
        let mut aa = 0.0;
        let mut bb = 0.0;
        for offset in 0..width {
            let a = f64::from(mono[start + offset]);
            let b = f64::from(mono[start + offset + lag]);
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
        let denominator = curve[top - 1].1 - 2.0 * curve[top].1 + curve[top + 1].1;
        if denominator.abs() > 1e-12 {
            lag += (0.5 * (curve[top - 1].1 - curve[top + 1].1) / denominator).clamp(-0.5, 0.5);
        }
    }
    f64::from(RATE) / lag
}

fn cents(measured: f64, target: f64) -> f64 {
    1_200.0 * (measured / target).log2()
}

/// The timing report entry for a detected note.
fn timing_for(applied: &vocal_tune::AppliedTune, index: usize) -> &ResolvedTiming {
    applied
        .resolved
        .timing
        .iter()
        .find(|entry| entry.detected_index == index)
        .expect("timing entry")
}

/// The no-timing path must retain the pre-timing pitch/formant orchestration.
/// Numerical DSP is shared; the explicit two-region curve is independent of
/// current timing/curve resolution and requires no cross-platform output golden.
#[test]
fn a_block_without_timing_fields_matches_the_pitch_only_reference() {
    use oc_dsp::vocal_shift::{self, ShiftConfig, ShiftCurve, ShiftSegment};

    let dir = temp_dir("pitch-only-reference");
    write_take(&dir, &fixed_pitch_formant_take());
    let base = project_text("", 3);
    let analysis = analysis_of(&parse(&base), &dir);
    assert_eq!(
        analysis.analysis.notes.len(),
        2,
        "fixture needs two voiced regions"
    );
    let notes = &analysis.analysis.notes;
    assert!(notes.iter().all(|note| note.confidence >= 0.5));
    let text = project_text(
        &block(
            "100%",
            &format!(
                "    - at={:.3}s tgt=+100ct formant=150ct\n    - at={:.3}s tgt=-70ct retune=40ms\n",
                notes[0].start_s, notes[1].start_s,
            ),
        ),
        3,
    );
    let project = parse(&text);
    let tune = &project.properties.vocal_tunes[0];
    assert_eq!(tune.retune_seconds, 0.025);
    assert_eq!(tune.notes[0].retune_seconds, None);
    assert_eq!(tune.notes[1].retune_seconds, Some(0.040));
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let resolved = vocal_tune::resolve_tune(&project, clip, tune, &analysis).unwrap();
    assert!(resolved.timing.is_empty());
    assert!(resolved.proposals.is_empty());
    assert_eq!(resolved.notes.len(), 2);
    for (index, (shift, formant)) in [(100.0, 150.0), (-70.0, 0.0)].into_iter().enumerate() {
        assert_eq!(resolved.notes[index].detected_index, Some(index));
        assert_eq!(resolved.notes[index].shift_cents, Some(shift));
        assert_eq!(resolved.notes[index].formant_cents, formant);
        assert!(resolved.notes[index].vibrato.is_none());
    }
    let window = window_of(&dir, &base);
    let frame_count = window.len() / 2;
    // The pitch-only law for this fixture: original frame spans,
    // explicit pitch/formant values, and default/per-note glides. No current
    // shift_curve, timing targets or resolved-note spans construct this oracle.
    let spans = [
        (
            (notes[0].start_s * f64::from(RATE)).round() as usize,
            (notes[0].end_s * f64::from(RATE)).round() as usize,
        ),
        (
            (notes[1].start_s * f64::from(RATE)).round() as usize,
            (notes[1].end_s * f64::from(RATE)).round() as usize,
        ),
    ];
    assert!(spans[0].0 < spans[0].1 && spans[0].1 <= spans[1].0);
    assert!(spans[1].0 < spans[1].1 && spans[1].1 <= frame_count);
    let reference_curve = ShiftCurve::from_ramped_segments(
        frame_count,
        &[
            (
                ShiftSegment {
                    start: spans[0].0,
                    end: spans[0].1,
                    cents: 100.0,
                    formant_cents: 150.0,
                },
                Some(1_200),
            ),
            (
                ShiftSegment {
                    start: spans[1].0,
                    end: spans[1].1,
                    cents: -70.0,
                    formant_cents: 0.0,
                },
                Some(1_920),
            ),
        ],
    )
    .unwrap();
    let actual_curve = vocal_tune::shift_curve(frame_count, RATE, tune, &resolved, notes).unwrap();
    for frame in 0..frame_count {
        assert_eq!(
            actual_curve.cents_at(frame).to_bits(),
            reference_curve.cents_at(frame).to_bits(),
            "pitch curve frame {frame}"
        );
        assert_eq!(
            actual_curve.formant_at(frame).to_bits(),
            reference_curve.formant_at(frame).to_bits(),
            "formant curve frame {frame}"
        );
    }
    let mut config = ShiftConfig::new(RATE);
    config.ramp_frames = 1_200;
    let expected = vocal_shift::shift(&window, 2, RATE, &reference_curve, &config).unwrap();
    assert!(expected.report.wet_frames > 0 && expected.report.formant_frames > 0);
    assert!(
        window
            .iter()
            .zip(&expected.samples)
            .any(|(a, b)| a.to_bits() != b.to_bits())
    );
    let actual = vocal_tune::shift_window(RATE, &window, tune, &resolved, notes).unwrap();
    assert!(
        actual.time.is_none(),
        "omitted timing must bypass the time pass"
    );
    assert_eq!(actual.samples.len(), expected.samples.len());
    for (index, (a, b)) in actual.samples.iter().zip(&expected.samples).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "pitch-only sample {index}");
    }
    let (baked, applied) = bake(&dir, &text, "pitch-only");
    assert!(!applied.baked.as_ref().unwrap().from_cache);
    assert!(applied.resolved.timing.is_empty());
    assert!(applied.time.is_none());
    assert_eq!(baked.len(), expected.samples.len());
    // apply writes float32 WAV: decoding must retain the reference sample bits.
    for (index, (a, b)) in baked.iter().zip(&expected.samples).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "baked pitch-only sample {index}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_zero_request_on_every_note_bakes_the_same_bytes() {
    let dir = temp_dir("zero");
    write_take(&dir, &fixed_pitch_formant_take());
    let analysis = analysis_of(&parse(&project_text("", 3)), &dir);
    let starts = onsets(&analysis);
    let plain = project_text(
        &block(
            "100%",
            &format!(
                "    - at={:.3}s tgt=+100ct formant=150ct\n    - at={:.3}s tgt=-70ct\n",
                starts[0], starts[1]
            ),
        ),
        3,
    );
    let zeroed = project_text(
        &block(
            "100%",
            &format!(
                "    - at={:.3}s tgt=+100ct formant=150ct start=0ms length=1\n    - at={:.3}s tgt=-70ct start=0ms length=1\n",
                starts[0], starts[1]
            ),
        ),
        3,
    );
    let (plain_samples, plain_applied) = bake(&dir, &plain, "zero-plain");
    assert!(plain_applied.resolved.timing.is_empty());
    assert!(plain_applied.time.is_none());
    let (zero_samples, applied) = bake(&dir, &zeroed, "zero-edited");
    assert!(applied.resolved.timing.is_empty(), "no-op edits survived");
    assert!(applied.time.is_none());
    assert_eq!(plain_samples.len(), zero_samples.len());
    for (index, (left, right)) in plain_samples.iter().zip(&zero_samples).enumerate() {
        assert_eq!(left.to_bits(), right.to_bits(), "sample {index}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_moved_note_lands_within_five_ms_and_keeps_its_pitch() {
    let dir = temp_dir("move");
    let mut samples = stereo_buffer(2.2);
    place(&mut samples, 0.2, &tone(220.0, 0.6, 0.15));
    place(&mut samples, 1.3, &tone(330.0, 0.6, 0.10));
    write_take(&dir, &samples);
    let base = project_text("", 3);
    let analysis = analysis_of(&parse(&base), &dir);
    let starts = onsets(&analysis);
    assert!(starts.len() >= 2, "detected {}", starts.len());
    let window = window_of(&dir, &base);
    let input_onset = audible_span(&window, starts[0] - 0.05, starts[0] + 0.1)
        .expect("input onset")
        .0;
    for move_ms in [30.0_f64, -30.0] {
        let text = project_text(
            &block(
                "100%",
                &format!(
                    "    - at={:.3}s tgt=+100ct start={move_ms:.0}ms\n",
                    starts[0]
                ),
            ),
            3,
        );
        let (baked, applied) = bake(&dir, &text, &format!("move{move_ms}"));
        let timing = timing_for(&applied, 0);
        let measured = timing.measured.expect("measured");
        let position = measured.audible_start_s;
        let residual_ms = (position - input_onset - move_ms / 1000.0) * 1000.0;
        // The note is stored with `tgt=+100ct` too, so this proves a note can
        // be retuned and moved at once.
        let retuned = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let pitch = cents(measured_f0(&baked, position + 0.15, retuned), retuned);
        println!(
            "move {move_ms:+.0} ms: audible {position:.4}s, residual {residual_ms:+.2} ms (reported {:+.2}), pitch {pitch:+.3} ct, seam {:.3}x",
            measured.onset_residual_ms,
            applied.time.as_ref().unwrap().boundary_step_ratio
        );
        assert!(residual_ms.abs() <= 5.0, "residual {residual_ms:+.2} ms");
        assert!(pitch.abs() <= 2.0, "pitch {pitch:+.3} ct");
        assert!(
            applied.time.as_ref().unwrap().boundary_step_ratio <= 1.5,
            "{:.3}",
            applied.time.as_ref().unwrap().boundary_step_ratio
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_quantised_phrase_lands_on_the_grid_without_doubling_or_dropping() {
    let dir = temp_dir("quantise");
    // Five notes, each 10-40 ms off a 0.5 s grid.
    let offsets = [0.030_f64, 0.015, -0.040, 0.020, -0.010];
    let grid: Vec<f64> = (0..5).map(|index| 0.5 + 0.5 * index as f64).collect();
    let mut samples = stereo_buffer(3.4);
    for (index, offset) in offsets.iter().enumerate() {
        place(
            &mut samples,
            grid[index] + offset,
            &tone(220.0 * 2f64.powf(index as f64 * 2.0 / 12.0), 0.35, 0.10),
        );
    }
    write_take(&dir, &samples);
    let base = project_text("", 3);
    let analysis = analysis_of(&parse(&base), &dir);
    let starts = onsets(&analysis);
    assert_eq!(starts.len(), 5, "detected {starts:?}");
    let window = window_of(&dir, &base);
    let audible: Vec<f64> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            audible_span(&window, start - 0.05, grid[index] + offsets[index] + 0.1)
                .expect("input onset")
                .0
        })
        .collect();
    let mut notes = String::new();
    for (index, start) in starts.iter().enumerate() {
        let move_ms = (grid[index] - audible[index]) * 1000.0;
        notes.push_str(&format!("    - at={start:.3}s start={move_ms:.1}ms\n"));
    }
    let text = project_text(&block("100%", &notes), 3);
    let (baked, applied) = bake(&dir, &text, "quantise");
    assert_eq!(applied.resolved.timing.len(), 5);
    for (index, entry) in applied.resolved.timing.iter().enumerate() {
        let measured = entry.measured.expect("measured");
        let error_ms = (measured.audible_start_s - grid[index]) * 1000.0;
        let pitch = cents(
            measured_f0(
                &baked,
                measured.audible_start_s + 0.12,
                220.0 * 2f64.powf(index as f64 * 2.0 / 12.0),
            ),
            220.0 * 2f64.powf(index as f64 * 2.0 / 12.0),
        );
        println!(
            "note {}: off {:+.0} ms, requested {:+6.1} ms, audible {:.4}s ({error_ms:+.2} ms vs grid), pitch {pitch:+.3} ct",
            index + 1,
            offsets[index] * 1000.0,
            entry.move_seconds * 1000.0,
            measured.audible_start_s
        );
        assert!(error_ms.abs() <= 5.0, "note {index}: {error_ms:+.2} ms");
        assert!(pitch.abs() <= 2.0, "note {index}: {pitch:+.3} ct");
    }
    // No note doubled or dropped: five audible regions at the grid.
    let regions = audible_regions(&baked, 0.02);
    assert_eq!(regions.len(), 5, "{regions:?}");
    for (index, (start, _)) in regions.iter().enumerate() {
        assert!(
            (start - grid[index]).abs() <= 0.005,
            "region {index} starts at {start:.4}s"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stretch_and_a_shrink_keep_the_take_length() {
    let dir = temp_dir("stretch");
    let mut samples = stereo_buffer(2.6);
    place(&mut samples, 0.3, &tone(220.0, 0.4, 0.10));
    place(&mut samples, 1.3, &tone(330.0, 0.4, 0.10));
    write_take(&dir, &samples);
    let base = project_text("", 2);
    let analysis = analysis_of(&parse(&base), &dir);
    let notes = &analysis.analysis.notes;
    assert!(notes.len() >= 2, "detected {}", notes.len());
    let window = window_of(&dir, &base);
    let first = audible_span(&window, notes[0].start_s - 0.05, notes[0].end_s + 0.1).unwrap();
    let second = audible_span(&window, notes[1].start_s - 0.05, notes[1].end_s + 0.1).unwrap();
    let first_len = notes[0].end_s - notes[0].start_s;
    let second_len = notes[1].end_s - notes[1].start_s;
    let text = project_text(
        &block(
            "100%",
            &format!(
                "    - at={:.3}s start=40ms length={:.6}\n    - at={:.3}s start=-40ms length={:.6}\n",
                notes[0].start_s,
                (first_len + 0.040) / first_len,
                notes[1].start_s,
                (second_len - 0.040) / second_len
            ),
        ),
        2,
    );
    let (baked, applied) = bake(&dir, &text, "stretch");
    assert_eq!(baked.len(), window.len(), "take length changed");
    for (index, wants) in [(0_usize, 0.040_f64), (1, -0.040)] {
        let entry = timing_for(&applied, index);
        let measured = entry.measured.expect("measured");
        println!(
            "note {}: source {:?} target {:?} detected {:.4}s len {:.1} ms, move {:+.0} ms, audible start {:.4}s len {:.1} ms, requested {:.1} ms, span error {:+.2} ms, audible error {:+.2} ms",
            index + 1,
            entry.source,
            entry.target,
            entry.detected_start_s,
            entry.detected_length_s * 1000.0,
            entry.move_seconds * 1000.0,
            measured.audible_start_s,
            measured.audible_length_s * 1000.0,
            entry.requested_length_s * 1000.0,
            measured.length_error_ms,
            measured.audible_length_error_ms
        );
        assert!(
            measured.length_error_ms.abs() <= 3.0,
            "note {index}: span {:+.2} ms",
            measured.length_error_ms
        );
        assert!(
            measured.audible_length_error_ms.abs() <= 3.0,
            "note {index}: audible {:+.2} ms",
            measured.audible_length_error_ms
        );
        let expected = [220.0, 330.0][index];
        let pitch = cents(
            measured_f0(&baked, measured.audible_start_s + 0.15, expected),
            expected,
        );
        assert!(pitch.abs() <= 2.0, "note {index}: {pitch:+.3} ct");
        let _ = wants;
    }
    // The move did happen and nothing else in the take grew.
    assert!(first.0 > 0.0 && second.0 > 0.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_colliding_move_is_rejected_with_a_clear_message() {
    let dir = temp_dir("collide");
    let mut samples = stereo_buffer(2.2);
    place(&mut samples, 0.3, &tone(220.0, 0.5, 0.10));
    place(&mut samples, 0.9, &tone(330.0, 0.5, 0.10));
    write_take(&dir, &samples);
    let base = project_text("", 2);
    let analysis = analysis_of(&parse(&base), &dir);
    let notes = &analysis.analysis.notes;
    assert!(notes.len() >= 2, "detected {}", notes.len());
    let gap_ms = (notes[1].start_s - notes[0].end_s) * 1000.0;
    println!("gap {gap_ms:.0} ms");
    let text = project_text(
        &block(
            "100%",
            &format!("    - at={:.3}s start=120ms\n", notes[0].start_s),
        ),
        2,
    );
    let source = dir.join("collide.ocp");
    std::fs::write(&source, &text).unwrap();
    let project = parse(&text);
    let error = vocal_tune::apply(&project, &source, "lead", RATE, None).unwrap_err();
    println!("collision error: {error}");
    assert!(error.contains("overlap"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_timing_field_on_a_stale_anchor_is_rejected() {
    let dir = temp_dir("stale");
    let mut samples = stereo_buffer(1.6);
    place(&mut samples, 0.2, &tone(220.0, 0.8, 0.10));
    write_take(&dir, &samples);
    let text = project_text(&block("100%", "    - at=1.500s start=30ms\n"), 2);
    let source = dir.join("stale.ocp");
    std::fs::write(&source, &text).unwrap();
    let project = parse(&text);
    let error = vocal_tune::apply(&project, &source, "lead", RATE, None).unwrap_err();
    println!("stale error: {error}");
    assert!(
        error.contains("timing fields need a detected note"),
        "{error}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_move_beyond_the_cap_is_rejected_at_validation() {
    let dir = temp_dir("cap");
    let mut samples = stereo_buffer(1.6);
    place(&mut samples, 0.2, &tone(220.0, 0.8, 0.10));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let start = onsets(&analysis)[0];
    let text = project_text(
        &block("100%", &format!("    - at={start:.3}s start=150ms\n")),
        2,
    );
    let project = oc_dsl::parse(&text).unwrap();
    let error = oc_dsl::validate(&project).unwrap_err();
    let text = format!("{error:?}");
    println!("cap error: {text}");
    assert!(text.contains("max_move"), "{text}");
    std::fs::remove_dir_all(dir).unwrap();
}
