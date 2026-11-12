//! Automatic pitch targets (scale snap and reference matching) through
//! the vocal pipeline: analysis → resolve → bake on synthetic takes.
//!
//! Every signal is generated here. The pitch figures are measured on the baked
//! window with an autocorrelation oracle that shares no code with the detector.

use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::vocal_analysis::{self, ClipAnalysis};
use oc_engine::vocal_tune::{self, NoteOutcome};
use oc_engine::{audio_clip, wav};

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-snap", name)
}

fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

fn cents(measured: f64, target: f64) -> f64 {
    1_200.0 * (measured / target).log2()
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

/// Continuous-phase tone with one harmonic, starting at phase zero.
fn tone(f0: f64, seconds: f64, second: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    (0..frames)
        .map(|index| {
            let phase = std::f64::consts::TAU * f0 * index as f64 / f64::from(RATE);
            0.3 * phase.sin() + second * (2.0 * phase).sin()
        })
        .collect()
}

fn write_take(dir: &Path, samples: &[f32]) {
    wav::write_stereo_wav(dir.join("take.wav"), samples, RATE).unwrap();
}

fn project_text(block: &str, end_bar: u32) -> String {
    format!(
        "@project \"Vocal snap\"\n  tempo: 120\n  key: C major\n@audio_clip lead [1:1 - {end_bar}:1] | track: vocal\n  file: \"take.wav\"\n{block}@track vocal | instrument: audio/clip\n  mix: vol=0dB\n"
    )
}

fn reference_text(block: &str) -> String {
    format!(
        "@project \"Vocal snap reference\"\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"take.wav\"\n@audio_clip guide [1:1 - 2:1] | track: guide\n  file: \"guide.wav\"\n{block}@track vocal | instrument: audio/clip\n@track guide | instrument: audio/clip\n"
    )
}

fn parse(text: &str) -> Project {
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn analysis_of(project: &Project, dir: &Path, selector: &str) -> ClipAnalysis {
    let clip = vocal_analysis::resolve_clip(project, selector).unwrap();
    vocal_analysis::analyze_clip(
        project,
        clip,
        RATE,
        Some(dir),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        Some(&dir.join(format!("{selector}-probe.ocn"))),
    )
    .unwrap()
}

fn window_of(dir: &Path, text: &str) -> Vec<f32> {
    let project = parse(text);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(dir))
        .unwrap()
        .1
}

/// Bake the block through the real pipeline and return the baked samples.
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

fn assert_same_bits(left: &[f32], right: &[f32], what: &str) {
    assert_eq!(left.len(), right.len(), "{what}: length");
    for (index, (a, b)) in left.iter().zip(right).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "{what}: sample {index}");
    }
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

/// MIDI cents sequence of the automatic proposals.
fn proposal_cents(applied: &vocal_tune::AppliedTune) -> Vec<f64> {
    applied
        .resolved
        .proposals
        .iter()
        .map(|note| note.target_cents.expect("a proposal target"))
        .collect()
}

#[test]
fn a_five_note_melody_snaps_to_the_right_degrees_without_zigzag() {
    let dir = temp_dir("melody");
    // C4 E4 D4 F4 E4, each 20-40 cents sharp; the contour is up, down, up, down.
    let degrees = [60.0, 64.0, 62.0, 65.0, 64.0];
    let detune = [30.0, 25.0, 40.0, 20.0, 35.0];
    let mut samples = stereo_buffer(3.0);
    for (index, (midi, sharp)) in degrees.iter().zip(detune).enumerate() {
        let start = 0.2 + index as f64 * 0.5;
        place(
            &mut samples,
            start,
            &tone(hz(*midi) * 2f64.powf(sharp / 1_200.0), 0.45, 0.05),
        );
    }
    write_take(&dir, &samples);
    let text = project_text("@vocal_tune lead_tune | clip: lead\n", 4);
    let project = parse(&text);
    let analysis = analysis_of(&project, &dir, "lead");
    assert_eq!(
        analysis.analysis.notes.len(),
        degrees.len(),
        "detected {:?}",
        analysis
            .analysis
            .notes
            .iter()
            .map(|note| note.start_s)
            .collect::<Vec<_>>()
    );

    // The proposal is deterministic: the same project and analysis twice.
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let tune = vocal_tune::tune_for_clip(&project, clip).unwrap();
    let first = vocal_tune::resolve_tune(&project, clip, tune, &analysis).unwrap();
    let second = vocal_tune::resolve_tune(&project, clip, tune, &analysis).unwrap();
    assert_eq!(first.proposals, second.proposals, "proposals moved");

    let (baked, applied) = bake(&dir, &text, "melody");
    let chosen = proposal_cents(&applied);
    assert_eq!(
        chosen,
        vec![6_000.0, 6_400.0, 6_200.0, 6_500.0, 6_400.0],
        "chosen degrees"
    );
    // No zigzag: the chosen contour moves the same way as the performed one.
    for window in 0..chosen.len() - 1 {
        let performed = analysis.analysis.notes[window + 1].f0_median_hz
            - analysis.analysis.notes[window].f0_median_hz;
        let picked = chosen[window + 1] - chosen[window];
        assert!(
            performed.signum() == picked.signum(),
            "note {} flips: performed {performed}, picked {picked}",
            window
        );
    }

    // Every note lands within 5 cents of its scale degree.
    let mut worst = 0.0_f64;
    for (index, note) in analysis.analysis.notes.iter().enumerate() {
        let expected = hz(degrees[index]);
        let from = note.start_s + 0.045;
        let width = (f64::from(RATE) * 2.0 / expected) as usize;
        let measured = measured_f0(
            &mono(&baked),
            (from * f64::from(RATE)) as usize,
            expected,
            width,
        );
        let error = cents(measured, expected);
        worst = worst.max(error.abs());
    }
    println!("five-note snap: worst {worst:.3} cents");
    assert!(worst <= 5.0, "worst {worst:.3} cents");
    assert_eq!(applied.resolved.reference_notes, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_in_tune_note_is_untouched_bit_for_bit() {
    let dir = temp_dir("in-tune");
    let mut samples = stereo_buffer(2.0);
    // A4 is in C major, so a 440 Hz note needs no correction.
    place(&mut samples, 0.2, &tone(440.0, 1.4, 0.05));
    write_take(&dir, &samples);
    let text = project_text("@vocal_tune lead_tune | clip: lead\n  scale: C major\n", 2);
    let (baked, applied) = bake(&dir, &text, "in-tune");
    assert_eq!(applied.resolved.proposals.len(), 1);
    assert_eq!(applied.resolved.proposals[0].outcome, NoteOutcome::InTune);
    assert_eq!(applied.resolved.proposals[0].shift_cents, Some(0.0));
    assert_eq!(applied.resolved.shifted_notes(), 0);
    assert_same_bits(&baked, &window_of(&dir, &text), "in-tune bake");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_capped_note_is_left_alone_and_reported() {
    let dir = temp_dir("capped");
    let mut samples = stereo_buffer(2.0);
    // 90 cents above C4; a 50-cent cap refuses the move instead of crossing
    // the semitone boundary.
    place(
        &mut samples,
        0.2,
        &tone(hz(60.0) * 2f64.powf(90.0 / 1_200.0), 1.4, 0.05),
    );
    write_take(&dir, &samples);
    let text = project_text(
        "@vocal_tune lead_tune | clip: lead\n  scale: C major\n  cap: 50ct\n",
        2,
    );
    let (baked, applied) = bake(&dir, &text, "capped");
    assert_eq!(applied.resolved.proposals.len(), 1);
    let proposal = &applied.resolved.proposals[0];
    assert_eq!(proposal.outcome, NoteOutcome::Capped);
    assert_eq!(proposal.target_cents, None);
    assert_eq!(proposal.shift_cents, None);
    assert_eq!(applied.resolved.shifted_notes(), 0);
    assert_same_bits(&baked, &window_of(&dir, &text), "capped bake");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_reference_octave_jump_does_not_drag_the_take_down_an_octave() {
    let dir = temp_dir("octave-jump");
    let mut take = stereo_buffer(2.0);
    // A3 + 30 cents: the take is a little sharp.
    place(
        &mut take,
        0.2,
        &tone(220.0 * 2f64.powf(30.0 / 1_200.0), 1.4, 0.05),
    );
    write_take(&dir, &take);
    // The guide sings the same melody an octave up.
    let mut guide = stereo_buffer(2.0);
    place(&mut guide, 0.2, &tone(440.0, 1.4, 0.05));
    wav::write_stereo_wav(dir.join("guide.wav"), &guide, RATE).unwrap();
    let text = reference_text("@vocal_tune lead_tune | clip: lead\n  reference: guide\n");
    let (baked, applied) = bake(&dir, &text, "octave-jump");
    assert_eq!(applied.resolved.snap.as_str(), "reference");
    assert_eq!(applied.resolved.proposals.len(), 1);
    let proposal = &applied.resolved.proposals[0];
    assert_eq!(proposal.outcome, NoteOutcome::Snapped);
    // A4 (6900) folded down to A3 (5700), not targeted at A4. The detector's
    // median is a few hundredths of a cent off Equal Temperament, so compare
    // with a tolerance.
    let target = proposal.target_cents.expect("reference target");
    assert!((target - 5_700.0).abs() < 1.0, "target {target}");
    let shift = proposal.shift_cents.unwrap();
    assert!(shift.abs() < 100.0, "shift {shift}");
    assert_eq!(applied.resolved.reference_notes, 1);

    let expected = 220.0;
    let note = &applied.resolved.proposals[0];
    let from = note.detected_start_s.unwrap() + 0.045;
    let width = (f64::from(RATE) * 2.0 / expected) as usize;
    let measured = measured_f0(
        &mono(&baked),
        (from * f64::from(RATE)) as usize,
        expected,
        width,
    );
    let error = cents(measured, expected);
    println!("reference octave fold: {error:+.3} cents");
    assert!(error.abs() <= 5.0, "{error:+.3} cents");
    std::fs::remove_dir_all(dir).unwrap();
}
