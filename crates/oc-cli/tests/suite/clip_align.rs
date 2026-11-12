//! `ocp clip align` writes the correction into the project and render follows it.

use std::path::{Path, PathBuf};

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-clip-align", name)
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

fn project_text() -> &'static str {
    r#"@project "Comping CLI"
  tempo: 120
@audio_clip ref [1:1 - 5:1] | track: ref
  file: "ref.wav"
@audio_clip take [1:1 - 5:1] | track: take
  file: "take.wav"
@track ref | instrument: audio/clip
@track take | instrument: audio/clip
  mix: solo=true
"#
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

#[test]
fn cli_align_writes_a_shift_and_the_render_matches_the_reference() {
    let dir = temp_dir("shift");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at + 0.030).collect();
    let take = click_train(&shifted, 3.25);
    write_wav(&dir.join("ref.wav"), &reference);
    write_wav(&dir.join("take.wav"), &take);
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text()).unwrap();
    let before = std::fs::read(&project).unwrap();

    let dry = run(&[
        "clip",
        "align",
        project.to_str().unwrap(),
        "ref",
        "take",
        "--sample-rate",
        "48000",
        "--dry-run",
    ]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(
        std::fs::read(&project).unwrap(),
        before,
        "--dry-run must not write"
    );

    let aligned = run(&[
        "clip",
        "align",
        project.to_str().unwrap(),
        "ref",
        "take",
        "--sample-rate",
        "48000",
    ]);
    assert!(
        aligned.status.success(),
        "{}",
        String::from_utf8_lossy(&aligned.stderr)
    );
    let text = std::fs::read_to_string(&project).unwrap();
    let parsed = oc_dsl::parse(&text).unwrap();
    let take_clip = parsed
        .audio_clips()
        .into_iter()
        .find(|clip| clip.name == "take")
        .unwrap();
    assert!((take_clip.offset_seconds - 0.030).abs() < 1e-6);
    assert_eq!(take_clip.stretch, 1.0);

    let output = dir.join("out.wav");
    let rendered = run(&[
        "render",
        project.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let mut reader = hound::WavReader::open(&output).unwrap();
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|sample| sample.unwrap() as f32 / 32768.0)
        .collect();
    let detected = oc_engine::align::detect_onsets(&samples, RATE);
    assert_eq!(detected.len(), times.len(), "detected {detected:?}");
    for (expected, actual) in times.iter().zip(&detected) {
        let expected_frame = (expected * f64::from(RATE)).round() as i64;
        let error = *actual as i64 - expected_frame;
        assert!(
            error.abs() <= (2.0 * f64::from(RATE) / 1000.0) as i64,
            "onset at {actual}, expected {expected_frame} ({error} frames)"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_align_refuses_an_out_of_range_correction_and_leaves_the_project_alone() {
    let dir = temp_dir("range");
    let times = reference_times();
    write_wav(&dir.join("ref.wav"), &click_train(&times, 3.2));
    let fast: Vec<f64> = times.iter().map(|at| at * 0.2).collect();
    write_wav(&dir.join("take.wav"), &click_train(&fast, 0.75));
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text()).unwrap();
    let before = std::fs::read(&project).unwrap();

    let failed = run(&[
        "clip",
        "align",
        project.to_str().unwrap(),
        "ref",
        "take",
        "--sample-rate",
        "48000",
    ]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("outside 0.25..=4"), "{stderr}");
    assert!(stderr.contains("take 'take'"), "{stderr}");
    assert_eq!(std::fs::read(&project).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
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

fn rendered_samples(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    reader
        .samples::<i16>()
        .map(|sample| sample.unwrap() as f32 / 32768.0)
        .collect()
}

#[test]
fn cli_align_invert_flips_an_out_of_phase_take() {
    let dir = temp_dir("invert");
    let times = reference_times();
    let reference = click_train(&times, 3.2);
    let shifted: Vec<f64> = times.iter().map(|at| at + 0.030).collect();
    let mut take = click_train(&shifted, 3.25);
    for sample in &mut take {
        *sample = -*sample;
    }
    write_wav(&dir.join("ref.wav"), &reference);
    write_wav(&dir.join("take.wav"), &take);
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text()).unwrap();

    let plain = run(&[
        "clip",
        "align",
        project.to_str().unwrap(),
        "ref",
        "take",
        "--sample-rate",
        "48000",
    ]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(stdout.contains("correlation -1.000"), "{stdout}");
    let plain_text = std::fs::read_to_string(&project).unwrap();
    let plain_project = oc_dsl::parse(&plain_text).unwrap();
    assert!(!plain_project.audio_clip(1).unwrap().invert);

    let plain_output = dir.join("plain.wav");
    let rendered = run(&[
        "render",
        project.to_str().unwrap(),
        "--output",
        plain_output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let raw_correlation = correlation(&reference, &rendered_samples(&plain_output));
    assert!(raw_correlation < -0.99, "{raw_correlation}");

    let aligned = run(&[
        "clip",
        "align",
        project.to_str().unwrap(),
        "ref",
        "take",
        "--sample-rate",
        "48000",
        "--invert",
    ]);
    assert!(
        aligned.status.success(),
        "{}",
        String::from_utf8_lossy(&aligned.stderr)
    );
    let text = std::fs::read_to_string(&project).unwrap();
    let parsed = oc_dsl::parse(&text).unwrap();
    assert!(parsed.audio_clip(1).unwrap().invert, "{text}");

    let output = dir.join("inverted.wav");
    let rendered = run(&[
        "render",
        project.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let flipped_correlation = correlation(&reference, &rendered_samples(&output));
    assert!(flipped_correlation > 0.99, "{flipped_correlation}");
    eprintln!("cli polarity: raw {raw_correlation:.3}, flipped {flipped_correlation:.3}");
    std::fs::remove_dir_all(dir).unwrap();
}
