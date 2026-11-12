//! The 1.3 pipeline: analysis → resolve → bake on synthetic takes.
//!
//! Every signal is generated here. The accuracy figures are measured on the
//! baked window with an autocorrelation oracle that shares no code with the
//! detector, so a detector bug cannot make its own output look correct.

use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::vocal_analysis::{self, ClipAnalysis};
use oc_engine::{audio_clip, vocal_tune, wav};

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-shift", name)
}

/// Equal-tempered frequency of a MIDI note.
fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

fn cents(measured: f64, target: f64) -> f64 {
    1_200.0 * (measured / target).log2()
}

/// Stereo buffer of `seconds`, silent, with `push` writing notes into it.
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

/// Sine with a vibrato of `depth_cents` at `vibrato_hz`, integrated phase.
fn vibrato_tone(f0: f64, seconds: f64, depth_cents: f64, vibrato_hz: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames);
    for index in 0..frames {
        let time = index as f64 / f64::from(RATE);
        let frequency = f0
            * 2f64.powf(depth_cents * (std::f64::consts::TAU * vibrato_hz * time).sin() / 1_200.0);
        out.push(0.3 * phase.sin());
        phase += std::f64::consts::TAU * frequency / f64::from(RATE);
    }
    out
}

/// Continuous-phase two-note take: no click at the note change itself.
fn sequence(notes: &[(f64, f64)], amplitude: f64) -> Vec<f64> {
    let total: f64 = notes.iter().map(|(_, seconds)| seconds).sum();
    let frames = (total * f64::from(RATE)) as usize;
    let mut samples = Vec::with_capacity(frames);
    let mut phase = 0.0_f64;
    let mut note = 0;
    let mut remaining = (notes[0].1 * f64::from(RATE)) as usize;
    for _ in 0..frames {
        while remaining == 0 && note + 1 < notes.len() {
            note += 1;
            remaining = (notes[note].1 * f64::from(RATE)) as usize;
        }
        samples.push(amplitude * phase.sin());
        phase += std::f64::consts::TAU * notes[note].0 / f64::from(RATE);
        remaining = remaining.saturating_sub(1);
    }
    samples
}

fn project_text(block: &str, end_bar: u32) -> String {
    format!(
        "@project \"Vocal shift\"\n  tempo: 120\n@audio_clip lead [1:1 - {end_bar}:1] | track: vocal\n  file: \"take.wav\"\n{block}@track vocal | instrument: audio/clip\n  mix: vol=0dB\n"
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

/// Analysis of the clip read from `dir`, without writing a project file.
fn analysis_of(project: &Project, dir: &Path) -> ClipAnalysis {
    let clip = vocal_analysis::resolve_clip(project, "lead").unwrap();
    vocal_analysis::analyze_clip(
        project,
        clip,
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

fn window_of(dir: &Path, text: &str) -> Vec<f32> {
    let project = parse(text);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(dir))
        .unwrap()
        .1
}

fn assert_same_bits(left: &[f32], right: &[f32], what: &str) {
    assert_eq!(left.len(), right.len(), "{what}: length");
    for (index, (a, b)) in left.iter().zip(right).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "{what}: sample {index}");
    }
}

fn assert_finite(samples: &[f32], what: &str) {
    assert!(
        samples.iter().all(|sample| sample.is_finite()),
        "{what}: non-finite sample"
    );
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

/// Per-frame cents error over `[start_s, end_s]` at a 5 ms hop.
fn cents_track(samples: &[f32], start_s: f64, end_s: f64, expected: f64) -> Vec<f64> {
    let hop = (f64::from(RATE) * 0.005) as usize;
    let width = (f64::from(RATE) * 2.0 / expected) as usize;
    let from = (start_s * f64::from(RATE)) as usize;
    let to = (end_s * f64::from(RATE)) as usize;
    let mut track = Vec::new();
    let mut at = from;
    while at + width + width < to {
        let measured = measured_f0(samples, at, expected, width);
        track.push(cents(measured, expected));
        at += hop;
    }
    track
}

/// Left channel of an interleaved stereo buffer.
fn mono(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(2).copied().collect()
}

fn blackman_harris(index: usize, length: usize) -> f64 {
    let phase = std::f64::consts::TAU * index as f64 / (length as f64 - 1.0);
    0.358_75 - 0.488_29 * phase.cos() + 0.141_28 * (2.0 * phase).cos()
        - 0.011_68 * (3.0 * phase).cos()
}

/// Magnitudes and bin width of one windowed spectrum.
fn spectrum(samples: &[f32], rate: u32) -> (Vec<f64>, f64) {
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

    let nfft = samples.len().next_power_of_two();
    let mut buffer: Vec<Complex<f64>> = (0..nfft)
        .map(|index| {
            let value = if index < samples.len() {
                f64::from(samples[index]) * blackman_harris(index, samples.len())
            } else {
                0.0
            };
            Complex::new(value, 0.0)
        })
        .collect();
    FftPlanner::new()
        .plan_fft_forward(nfft)
        .process(&mut buffer);
    let magnitudes = buffer
        .iter()
        .take(nfft / 2)
        .map(|value| value.norm())
        .collect();
    (magnitudes, f64::from(rate) / nfft as f64)
}

/// Loudest spectral peak that is not a harmonic of `f0`, in dBc.
fn worst_spur_db(magnitudes: &[f64], bin_hz: f64, f0: f64) -> (f64, f64) {
    let bin = (f0 / bin_hz).round() as usize;
    let low = bin.saturating_sub(3);
    let high = (bin + 3).min(magnitudes.len() - 1);
    let fundamental = magnitudes[low..=high]
        .iter()
        .fold(0.0_f64, |worst, value| worst.max(*value));
    let mut worst = f64::NEG_INFINITY;
    let mut worst_hz = 0.0;
    for index in 2..magnitudes.len() - 2 {
        let hz = index as f64 * bin_hz;
        if hz < f0 * 0.5 {
            continue;
        }
        let nearest = (hz / f0).round().max(1.0) * f0;
        if (hz - nearest).abs() <= 4.0 * bin_hz {
            continue;
        }
        if magnitudes[index] > magnitudes[index - 1] && magnitudes[index] >= magnitudes[index + 1] {
            let dbc = 20.0 * (magnitudes[index] / fundamental).log10();
            if dbc > worst {
                worst = dbc;
                worst_hz = hz;
            }
        }
    }
    (worst, worst_hz)
}

fn percentile(values: &mut [f64], which: f64) -> f64 {
    values.sort_by(|left, right| left.total_cmp(right));
    let index = ((values.len() as f64 - 1.0) * which).round() as usize;
    values[index]
}

/// Note interior: away from the glide at both edges and the detector window.
fn interior(note: &oc_dsp::vocal_analysis::VocalNote, retune_s: f64) -> (f64, f64) {
    let margin = retune_s + 0.020;
    (note.start_s + margin, note.end_s - margin)
}

fn onsets(analysis: &ClipAnalysis) -> Vec<f64> {
    analysis
        .analysis
        .notes
        .iter()
        .map(|note| note.start_s)
        .collect()
}

#[test]
fn a_hundred_cent_correction_lands_within_two_cents() {
    let dir = temp_dir("hundred");
    let mut samples = stereo_buffer(1.8);
    place(&mut samples, 0.2, &tone(220.0, 1.4, 0.15));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = onsets(&analysis)[0];
    let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
    let text = project_text(
        &block("100%", &format!("    - at={anchor:.3}s tgt=+100ct\n")),
        2,
    );

    let (baked, applied) = bake(&dir, &text, "hundred");
    assert_finite(&baked, "hundred");
    assert_eq!(applied.resolved.shifted_notes(), 1);
    let report = applied.report.expect("shift report");
    assert!(report.wet_frames > 0, "no wet frames");

    let note = &analysis.analysis.notes[0];
    let (from, to) = interior(note, 0.025);
    let mut errors = cents_track(&mono(&baked), from, to, target);
    assert!(errors.len() > 100, "{} frames", errors.len());
    let p95 = percentile(&mut errors, 0.95).abs();
    println!(
        "pipeline +100 ct: p95 {p95:.3} cents over {} frames",
        errors.len()
    );
    assert!(p95 <= 2.0, "p95 {p95:.3} cents");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn eight_detuned_notes_land_within_five_cents() {
    let dir = temp_dir("scale");
    let degrees = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0];
    let mut samples = stereo_buffer(4.2);
    for (index, midi) in degrees.iter().enumerate() {
        let start = 0.2 + index as f64 * 0.5;
        place(
            &mut samples,
            start,
            &tone(hz(*midi) * 2f64.powf(-40.0 / 1_200.0), 0.45, 0.05),
        );
    }
    write_take(&dir, &samples);
    let base = parse(&project_text("", 3));
    let analysis = analysis_of(&base, &dir);
    assert_eq!(
        analysis.analysis.notes.len(),
        degrees.len(),
        "detected {:?}",
        onsets(&analysis)
    );
    let mut notes = String::new();
    for (index, anchor) in onsets(&analysis).iter().enumerate() {
        let degree = index + 1;
        if degree <= 7 {
            notes.push_str(&format!("    - at={anchor:.3}s tgt={degree}@4\n"));
        } else {
            notes.push_str(&format!("    - at={anchor:.3}s tgt=1@5\n"));
        }
    }
    let text = project_text(
        &format!(
            "@vocal_tune lead_tune | clip: lead\n  scale: C major\n  amount: 100%\n  notes:\n{notes}"
        ),
        4,
    );
    let (baked, applied) = bake(&dir, &text, "scale");
    assert_finite(&baked, "scale");
    assert_eq!(applied.resolved.shifted_notes(), degrees.len());

    let mut worst = 0.0_f64;
    for (index, note) in analysis.analysis.notes.iter().enumerate() {
        let expected = hz(degrees[index]);
        let (from, _) = interior(note, 0.025);
        let measured = measured_f0(
            &mono(&baked),
            (from * f64::from(RATE)) as usize,
            expected,
            (f64::from(RATE) * 2.0 / expected) as usize,
        );
        let error = cents(measured, expected);
        println!("degree {}: {error:+.3} cents", index + 1);
        worst = worst.max(error.abs());
    }
    println!("eight notes: worst {worst:.3} cents");
    assert!(worst <= 5.0, "worst {worst:.3} cents");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn vibrato_survives_the_pipeline() {
    let dir = temp_dir("vibrato");
    let mut samples = stereo_buffer(1.8);
    place(&mut samples, 0.2, &vibrato_tone(220.0, 1.4, 25.0, 6.0));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = onsets(&analysis)[0];
    let text = project_text(
        &block("100%", &format!("    - at={anchor:.3}s tgt=+100ct\n")),
        2,
    );
    let source_window = window_of(&dir, &text);
    let (baked, _) = bake(&dir, &text, "vibrato");
    assert_finite(&baked, "vibrato");

    let note = &analysis.analysis.notes[0];
    let (from, to) = interior(note, 0.025);
    // The shift moves the note by a semitone, so the oracle searches around
    // the shifted pitch; only the depth matters, not the offset.
    let shifted = 220.0 * 2f64.powf(100.0 / 1_200.0);
    let mut input = cents_track(&mono(&source_window), from, to, 220.0);
    let mut output = cents_track(&mono(&baked), from, to, shifted);
    let depth = |track: &mut Vec<f64>| (percentile(track, 0.95) - percentile(track, 0.05)) * 0.5;
    let input_depth = depth(&mut input);
    let output_depth = depth(&mut output);
    let drift = (output_depth - input_depth).abs() / input_depth;
    println!(
        "vibrato depth: input {input_depth:.2} ct, output {output_depth:.2} ct, drift {:.1}%",
        drift * 100.0
    );
    assert!(input_depth > 20.0, "input depth {input_depth:.2} ct");
    assert!(drift <= 0.10, "depth drift {:.1}%", drift * 100.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sidebands_after_the_bake_stay_below_the_budget() {
    let dir = temp_dir("sidebands");
    let mut samples = stereo_buffer(1.8);
    place(&mut samples, 0.2, &tone(220.0, 1.4, 0.15));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = onsets(&analysis)[0];
    let text = project_text(
        &block("100%", &format!("    - at={anchor:.3}s tgt=+100ct\n")),
        2,
    );
    let (baked, _) = bake(&dir, &text, "sidebands");

    let note = &analysis.analysis.notes[0];
    let (from, _) = interior(note, 0.025);
    let steady = mono(&baked);
    let from = (from * f64::from(RATE)) as usize;
    let window = f64::from(RATE) * 1.0;
    let steady = &steady[from..from + window as usize];
    let (magnitudes, bin_hz) = spectrum(steady, RATE);
    let f0 = 220.0 * 2f64.powf(100.0 / 1_200.0);
    let (dbc, worst_hz) = worst_spur_db(&magnitudes, bin_hz, f0);
    println!("worst non-harmonic peak {dbc:.1} dBc at {worst_hz:.0} Hz");
    assert!(dbc <= -55.0, "sidebands {dbc:.1} dBc");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn only_the_shifted_note_moves() {
    let dir = temp_dir("spans");
    let mut samples = stereo_buffer(2.2);
    place(&mut samples, 0.2, &tone(220.0, 0.9, 0.0));
    place(&mut samples, 1.4, &tone(330.0, 0.7, 0.0));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 3)), &dir);
    let notes = &analysis.analysis.notes;
    assert!(notes.len() >= 2, "detected {}", notes.len());
    let text = project_text(
        &block(
            "100%",
            &format!("    - at={:.3}s tgt=+100ct\n", notes[1].start_s),
        ),
        3,
    );
    let window = window_of(&dir, &text);
    let (baked, applied) = bake(&dir, &text, "spans");
    assert_eq!(applied.resolved.shifted_notes(), 1);

    let frames = |seconds: f64| (seconds * f64::from(RATE)) as usize;
    // The untouched first note and the silence before, between and after the
    // notes are bit-identical to the input window.
    let untouched = [
        (0.0, notes[0].start_s - 0.001),
        (notes[0].start_s, notes[0].end_s),
        (notes[0].end_s, notes[1].start_s - 0.001),
        (notes[1].end_s + 0.001, 2.2),
    ];
    for (from, to) in untouched {
        let range = frames(from) * 2..frames(to) * 2;
        assert_same_bits(&window[range.clone()], &baked[range], "dry span");
    }
    // The shifted note did move.
    let span = frames(notes[1].start_s + 0.1) * 2..frames(notes[1].end_s - 0.1) * 2;
    assert!(
        window[span.clone()]
            .iter()
            .zip(&baked[span])
            .any(|(left, right)| left.to_bits() != right.to_bits()),
        "the shifted note did not move"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_low_confidence_or_bypassed_note_passes_through_dry() {
    // Resolution-level guard: a note that is bypassed or below the confidence
    // floor contributes no curve segment, so the bake cannot touch it.
    let dir = temp_dir("dry");
    let mut samples = stereo_buffer(1.8);
    place(&mut samples, 0.2, &tone(220.0, 1.4, 0.0));
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = onsets(&analysis)[0];
    let text = project_text(
        &block(
            "100%",
            &format!("    - at={anchor:.3}s tgt=+100ct bypass=true\n"),
        ),
        2,
    );
    let window = window_of(&dir, &text);
    let (baked, applied) = bake(&dir, &text, "dry");
    assert_eq!(
        applied.resolved.notes[0].outcome,
        vocal_tune::NoteOutcome::Bypassed
    );
    assert!(applied.resolved.pass_through());
    let report = applied.report.expect("shift report");
    assert_eq!(report.wet_frames, 0);
    assert_same_bits(&window, &baked, "bypassed note");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_shifted_note_boundary_stays_within_the_click_budget() {
    let dir = temp_dir("boundary");
    let mut samples = stereo_buffer(2.2);
    // One continuous take: the note change itself has no input transient.
    place(
        &mut samples,
        0.2,
        &sequence(&[(220.0, 0.8), (330.0, 0.9)], 0.3),
    );
    write_take(&dir, &samples);
    let analysis = analysis_of(&parse(&project_text("", 3)), &dir);
    let notes = &analysis.analysis.notes;
    assert!(notes.len() >= 2, "detected {}", notes.len());
    let text = project_text(
        &block(
            "100%",
            &format!(
                "    - at={:.3}s tgt=+100ct\n    - at={:.3}s tgt=-100ct\n",
                notes[0].start_s, notes[1].start_s
            ),
        ),
        3,
    );
    let (baked, applied) = bake(&dir, &text, "boundary");
    assert_finite(&baked, "boundary");
    assert_eq!(applied.resolved.shifted_notes(), 2);
    let report = applied.report.expect("shift report");
    println!(
        "two shifted notes: boundary step {:.3}x, wet {} frames",
        report.boundary_step_ratio, report.wet_frames
    );
    assert!(
        report.boundary_step_ratio <= 1.5,
        "{:.3}",
        report.boundary_step_ratio
    );
    assert!(report.peak <= 2.0, "peak {:.3}", report.peak);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- formant control -----------------------------------------------------

fn lcg_unit(state: &mut u32) -> f64 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (f64::from(*state) / f64::from(u32::MAX)) * 2.0 - 1.0
}

/// Source-filter vowel: jittered glottal pulses through three resonators plus
/// a breath floor. The same generator as the DSP acceptance test.
fn vowel(f0: f64, seconds: f64, breath: f64) -> Vec<f64> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut state = 0x2545_f491_u32;
    let mut noise = vec![0.0_f64; frames];
    for sample in &mut noise {
        *sample = lcg_unit(&mut state);
    }
    let mut pulses = vec![0.0_f64; frames];
    let mut position = 0.0_f64;
    while position < frames as f64 {
        let period = (1.0 / f0) * (1.0 + 0.005 * lcg_unit(&mut state));
        let index = position.round() as usize;
        if index < frames {
            pulses[index] += 1.0 + 0.02 * lcg_unit(&mut state);
        }
        position += period * f64::from(RATE);
    }
    let mut out = pulses;
    for (centre, bandwidth) in [(700.0_f64, 80.0), (1_220.0, 90.0), (2_600.0, 120.0)] {
        let radius = (-std::f64::consts::PI * bandwidth / f64::from(RATE)).exp();
        let theta = std::f64::consts::TAU * centre / f64::from(RATE);
        let (b0, feedback, feedforward) =
            (1.0 - radius, 2.0 * radius * theta.cos(), -radius * radius);
        let mut previous = 0.0;
        let mut before = 0.0;
        for sample in &mut out {
            let value = b0 * *sample + feedback * previous + feedforward * before;
            before = previous;
            previous = value;
            *sample = value;
        }
    }
    let rms = (out.iter().map(|value| value * value).sum::<f64>() / frames as f64).sqrt();
    let scale = 0.1 / rms.max(1e-12);
    for (sample, noise) in out.iter_mut().zip(&noise) {
        *sample = *sample * scale + breath * noise;
    }
    out
}

/// 170 ms cepstral envelope of a buffer, on a fixed frequency grid.
fn long_envelope(samples: &[f32], rate: u32) -> (Vec<f64>, f64) {
    use rustfft::num_complex::Complex;
    let n = 8_192;
    let start = samples.len() / 3;
    assert!(
        start + n <= samples.len(),
        "buffer too short for the oracle"
    );
    let mut spectrum: Vec<Complex<f64>> = (0..n)
        .map(|index| {
            let phase = std::f64::consts::TAU * index as f64 / n as f64;
            let window = 0.5 - 0.5 * phase.cos();
            Complex::new(f64::from(samples[start + index]) * window, 0.0)
        })
        .collect();
    let mut planner = rustfft::FftPlanner::<f64>::new();
    planner.plan_fft_forward(n).process(&mut spectrum);
    let bins = n / 2 + 1;
    let mut log_full = vec![0.0_f64; n];
    let peak = spectrum
        .iter()
        .take(bins)
        .fold(0.0_f64, |worst, value| worst.max(value.norm()));
    let floor = peak * 1e-5;
    for bin in 0..bins {
        log_full[bin] = spectrum[bin].norm().max(floor).ln();
    }
    for bin in 1..n / 2 {
        log_full[n - bin] = log_full[bin];
    }
    let mut cepstrum: Vec<Complex<f64>> = log_full
        .iter()
        .map(|value| Complex::new(*value, 0.0))
        .collect();
    planner.plan_fft_inverse(n).process(&mut cepstrum);
    for value in &mut cepstrum {
        *value /= n as f64;
    }
    for value in cepstrum.iter_mut().take(n - 192).skip(193) {
        *value = Complex::new(0.0, 0.0);
    }
    planner.plan_fft_forward(n).process(&mut cepstrum);
    (
        cepstrum.iter().take(bins).map(|value| value.re).collect(),
        f64::from(rate) / n as f64,
    )
}

/// First two formants from the long-window envelope. The LPC root finder is
/// biased by the 140 Hz comb once a formant lands between harmonics, so the
/// bake is measured on the envelope instead.
fn formants(samples: &[f32]) -> Vec<f64> {
    let (envelope, bin_hz) = long_envelope(samples, RATE);
    let mut peaks: Vec<f64> = Vec::new();
    for bin in 1..envelope.len() - 1 {
        let hz = bin as f64 * bin_hz;
        if !(200.0..3_500.0).contains(&hz) {
            continue;
        }
        if envelope[bin] > envelope[bin - 1] && envelope[bin] >= envelope[bin + 1] {
            if let Some(last) = peaks.last()
                && hz - last < 120.0
            {
                continue;
            }
            peaks.push(hz);
        }
    }
    peaks.truncate(2);
    peaks
}

fn vowel_take(dir: &Path) -> Vec<f32> {
    let mut samples = stereo_buffer(1.8);
    let voice = vowel(140.0, 1.4, 0.0015);
    let converted: Vec<f64> = voice.clone();
    place(&mut samples, 0.2, &converted);
    write_take(dir, &samples);
    samples
}

fn mono_window(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(2).copied().collect()
}

#[test]
fn a_formant_block_moves_f1_f2_and_keeps_the_pitch() {
    let dir = temp_dir("formant");
    let take = vowel_take(&dir);
    let base = parse(&project_text("", 2));
    let analysis = analysis_of(&base, &dir);
    println!(
        "detected {} notes: {:?}",
        analysis.analysis.notes.len(),
        analysis
            .analysis
            .notes
            .iter()
            .map(|note| (note.start_s, note.end_s))
            .collect::<Vec<_>>()
    );
    let (from, to) = (
        analysis.analysis.notes[0].start_s + 0.10,
        analysis.analysis.notes.last().unwrap().end_s - 0.10,
    );
    println!("measuring {from:.3}..{to:.3}s");
    let mut overrides = String::new();
    for note in &analysis.analysis.notes {
        overrides.push_str(&format!("    - at={:.3}s tgt=+0ct\n", note.start_s));
    }
    let source = mono_window(&take);
    let source_from = (from * f64::from(RATE)) as usize;
    let source_to = (to * f64::from(RATE)) as usize;
    let before = formants(&source[source_from..source_to]);
    assert_eq!(before.len(), 2, "source formants {before:?}");

    for requested in [-150.0_f64, 150.0] {
        let text = project_text(
            &format!(
                "@vocal_tune lead_tune | clip: lead\n  formant: {requested:+.0}ct\n  amount: 100%\n  notes:\n{overrides}"
            ),
            2,
        );
        let (baked, applied) = bake(&dir, &text, &format!("formant{requested:.0}"));
        assert_finite(&baked, "formant bake");
        assert_eq!(
            applied.resolved.shifted_notes(),
            analysis.analysis.notes.len()
        );
        let resolved = &applied.resolved.notes[0];
        assert!((resolved.formant_cents - requested).abs() < 1e-9);
        assert_eq!(resolved.outcome, vocal_tune::NoteOutcome::Shift);

        let baked_mono = mono_window(&baked);
        let from_at = (from * f64::from(RATE)) as usize;
        let to_at = (to * f64::from(RATE)) as usize;
        let after = formants(&baked_mono[from_at..to_at]);
        assert_eq!(after.len(), 2, "{requested}: {after:?}");
        let ratio = 2f64.powf(requested / 1_200.0);
        for (was, now) in before.iter().zip(&after) {
            let expected = was * ratio;
            let error = (now - expected) / expected;
            println!(
                "bake formant {requested:+.0} ct: {was:.1} -> {now:.1} Hz (want {expected:.1}, {:+.2} %)",
                error * 100.0
            );
            assert!(
                error.abs() <= 0.05,
                "{requested} ct: F {was:.1} Hz moved to {now:.1}, want {expected:.1}"
            );
        }
        // Pitch lands on the detected pitch, i.e. within two cents of itself.
        let pitch = measured_f0(
            &baked_mono,
            (from * f64::from(RATE)) as usize,
            140.0,
            (f64::from(RATE) * 0.03) as usize,
        );
        let error = cents(pitch, 140.0);
        assert!(error.abs() <= 2.0, "pitch moved {error:+.2} cents");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn formant_zero_renders_like_a_block_without_the_field() {
    let dir = temp_dir("formant-zero");
    let _ = vowel_take(&dir);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = analysis.analysis.notes[0].start_s;
    let with_field = project_text(
        &format!(
            "@vocal_tune lead_tune | clip: lead\n  formant: 0ct\n  amount: 100%\n  notes:\n    - at={anchor:.3}s tgt=+100ct\n"
        ),
        2,
    );
    let without = project_text(
        &format!(
            "@vocal_tune lead_tune | clip: lead\n  amount: 100%\n  notes:\n    - at={anchor:.3}s tgt=+100ct\n"
        ),
        2,
    );
    let (zero, _) = bake(&dir, &with_field, "zero-field");
    let (plain, _) = bake(&dir, &without, "no-field");
    assert_same_bits(&zero, &plain, "formant: 0ct");
    let dry = window_of(&dir, &project_text("", 2));
    assert!(
        zero.iter()
            .zip(&dry)
            .any(|(left, right)| left.to_bits() != right.to_bits()),
        "the pitch shift did not move the bake, so the guard proves nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn formant_decisions_rebake_and_are_deterministic() {
    let dir = temp_dir("formant-key");
    let _ = vowel_take(&dir);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = analysis.analysis.notes[0].start_s;
    let block = |formant: f64| {
        project_text(
            &format!(
                "@vocal_tune lead_tune | clip: lead\n  formant: {formant:+.0}ct\n  amount: 100%\n  notes:\n    - at={anchor:.3}s tgt=+0ct\n"
            ),
            2,
        )
    };
    let (first, one) = bake(&dir, &block(-150.0), "f150");
    let (again, one_again) = bake(&dir, &block(-150.0), "f150");
    assert_same_bits(&first, &again, "formant determinism");
    assert_eq!(
        one.baked.as_ref().unwrap().path,
        one_again.baked.as_ref().unwrap().path
    );
    assert!(one_again.baked.as_ref().unwrap().from_cache);
    let (other, two) = bake(&dir, &block(-100.0), "f100");
    assert_ne!(
        one.baked.as_ref().unwrap().path,
        two.baked.as_ref().unwrap().path,
        "a formant edit kept the bake key"
    );
    assert!(
        first
            .iter()
            .zip(&other)
            .any(|(left, right)| left.to_bits() != right.to_bits()),
        "a formant edit did not move the bake"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_formant_request_above_the_cap_clamps_with_a_warning() {
    let dir = temp_dir("formant-cap");
    let _ = vowel_take(&dir);
    let analysis = analysis_of(&parse(&project_text("", 2)), &dir);
    let anchor = analysis.analysis.notes[0].start_s;
    let text = |formant: f64| {
        project_text(
            &format!(
                "@vocal_tune lead_tune | clip: lead\n  formant: {formant:+.0}ct\n  amount: 100%\n  notes:\n    - at={anchor:.3}s tgt=+0ct\n"
            ),
            2,
        )
    };
    let (capped, applied) = bake(&dir, &text(600.0), "cap-600");
    let note = &applied.resolved.notes[0];
    assert!(
        (note.formant_cents - 300.0).abs() < 1e-9,
        "{}",
        note.formant_cents
    );
    assert!(
        note.warnings
            .iter()
            .any(|warning| warning.contains("clamped")),
        "{:?}",
        note.warnings
    );
    let (at_cap, _) = bake(&dir, &text(300.0), "cap-300");
    assert_same_bits(&capped, &at_cap, "clamped bake");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Half the 5th..95th percentile spread of a per-frame cents track.
fn vibrato_depth(track: &mut [f64]) -> f64 {
    (percentile(track, 0.95) - percentile(track, 0.05)) * 0.5
}

/// Depth the analyzer's own frame track sees between two times.
fn frame_vibrato_depth(samples: &[f32], from: f64, to: f64) -> f64 {
    let analysis = oc_dsp::vocal_analysis::analyze(
        samples,
        RATE,
        &oc_dsp::vocal_analysis::DetectorParams::default(),
    )
    .unwrap();
    let mut values: Vec<f64> = analysis
        .frames
        .iter()
        .filter(|frame| frame.voiced && frame.time_s >= from && frame.time_s < to)
        .map(|frame| oc_engine::vocal_tune::hz_to_midi_cents(frame.f0_hz))
        .collect();
    if values.len() < 2 {
        return 0.0;
    }
    vibrato_depth(&mut values)
}

/// One 6 Hz, ±40 cent vibrato note baked under each vibrato mode.
#[test]
fn vibrato_modes_set_the_residual_depth() {
    for mode in ["preserve", "flatten", "scale"] {
        let dir = temp_dir(&format!("vibrato-{mode}"));
        let mut samples = stereo_buffer(1.8);
        place(&mut samples, 0.2, &vibrato_tone(220.0, 1.4, 40.0, 6.0));
        write_take(&dir, &samples);
        let base = parse(&project_text("", 2));
        let analysis = analysis_of(&base, &dir);
        assert_eq!(
            analysis.analysis.notes.len(),
            1,
            "{mode}: detected {:?}",
            onsets(&analysis)
        );
        let note = &analysis.analysis.notes[0];
        let text = project_text(
            &format!(
                "@vocal_tune lead_tune | clip: lead\n  vibrato: {mode}\n  snap: off\n  amount: 100%\n"
            ),
            2,
        );
        let source_window = window_of(&dir, &text);
        let (baked, applied) = bake(&dir, &text, &format!("vibrato-{mode}"));
        assert_finite(&baked, mode);

        let (from, to) = (note.start_s + 0.06, note.end_s - 0.06);
        let mut input_track = cents_track(&mono(&source_window), from, to, 220.0);
        let mut output_track = cents_track(&mono(&baked), from, to, 220.0);
        let input_depth = vibrato_depth(&mut input_track);
        let output_depth = vibrato_depth(&mut output_track);
        let frame_input = frame_vibrato_depth(&mono(&source_window), from, to);
        let frame_output = frame_vibrato_depth(&mono(&baked), from, to);
        println!(
            "{mode}: oracle input {input_depth:.2} ct, output {output_depth:.2} ct over {} frames; frames {frame_input:.2} -> {frame_output:.2} ct",
            output_track.len()
        );
        if let Some(edit) = applied
            .resolved
            .proposals
            .iter()
            .chain(&applied.resolved.notes)
            .find_map(|note| note.vibrato.as_ref())
        {
            let (peak_index, peak) =
                edit.cents
                    .iter()
                    .enumerate()
                    .fold((0_usize, 0.0_f32), |worst, (index, cents)| {
                        if cents.abs() > worst.1 {
                            (index, cents.abs())
                        } else {
                            worst
                        }
                    });
            println!(
                "{mode}: measured depth {:.2} ct, {} frames, contour peak {peak:.2} ct at {:.3}s (note {:.3}-{:.3})",
                edit.depth_cents,
                edit.cents.len(),
                edit.start_seconds + peak_index as f64 * edit.hop_seconds,
                note.start_s,
                note.end_s,
            );
        }
        assert!(
            input_depth > 30.0,
            "{mode}: input depth {input_depth:.2} ct"
        );

        match mode {
            "preserve" => {
                assert!(
                    (output_depth - 40.0).abs() <= 5.0,
                    "preserve depth {output_depth:.2} ct"
                );
                assert!(applied.resolved.pass_through());
                assert_same_bits(&baked, &source_window, "preserve bake");
            }
            "flatten" => {
                assert!(output_depth < 8.0, "flatten depth {output_depth:.2} ct");
                assert_eq!(applied.resolved.shifted_notes(), 1);
            }
            "scale" => {
                assert!(
                    (output_depth - 20.0).abs() <= 5.0,
                    "scale depth {output_depth:.2} ct"
                );
                assert_eq!(applied.resolved.shifted_notes(), 1);
            }
            _ => unreachable!(),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Two vibrato notes, one carrying its own mode: only that note changes.
#[test]
fn a_per_note_vibrato_override_changes_only_that_note() {
    let dir = temp_dir("vibrato-note");
    let mut samples = stereo_buffer(2.4);
    place(&mut samples, 0.2, &vibrato_tone(220.0, 0.8, 40.0, 6.0));
    place(&mut samples, 1.15, &vibrato_tone(330.0, 0.8, 40.0, 6.0));
    write_take(&dir, &samples);
    let base = parse(&project_text("", 2));
    let analysis = analysis_of(&base, &dir);
    assert_eq!(
        analysis.analysis.notes.len(),
        2,
        "detected {:?}",
        onsets(&analysis)
    );
    let first = &analysis.analysis.notes[0];
    let second = &analysis.analysis.notes[1];
    let depth = |baked: &[f32], note: &oc_dsp::vocal_analysis::VocalNote| {
        let (from, to) = (note.start_s + 0.05, note.end_s - 0.05);
        let mut track = cents_track(&mono(baked), from, to, note.f0_median_hz);
        vibrato_depth(&mut track)
    };
    let text = |block_mode: &str, anchor: f64, note_mode: &str| {
        project_text(
            &format!(
                "@vocal_tune lead_tune | clip: lead\n  vibrato: {block_mode}\n  snap: off\n  amount: 100%\n  notes:\n    - at={anchor:.3}s vibrato={note_mode}\n"
            ),
            2,
        )
    };

    // The block preserves; the first note flattens itself.
    let (baked, applied) = bake(
        &dir,
        &text("preserve", first.start_s, "flatten"),
        "vib-first",
    );
    assert_eq!(applied.resolved.shifted_notes(), 1);
    let first_depth = depth(&baked, first);
    let second_depth = depth(&baked, second);
    println!("override on first: {first_depth:.2} / {second_depth:.2} ct");
    assert!(first_depth < 8.0, "first note {first_depth:.2} ct");
    assert!(
        (second_depth - 40.0).abs() <= 5.0,
        "second note {second_depth:.2} ct"
    );

    // The block flattens; the second note preserves itself.
    let (baked, applied) = bake(
        &dir,
        &text("flatten", second.start_s, "preserve"),
        "vib-second",
    );
    assert_eq!(applied.resolved.shifted_notes(), 1);
    let first_depth = depth(&baked, first);
    let second_depth = depth(&baked, second);
    println!("override on second: {first_depth:.2} / {second_depth:.2} ct");
    assert!(first_depth < 8.0, "first note {first_depth:.2} ct");
    assert!(
        (second_depth - 40.0).abs() <= 5.0,
        "second note {second_depth:.2} ct"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
