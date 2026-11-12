//! `ocp render` / `ocp validate` with `@audio_clip` media.

use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-cli-audio", name)
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn write_constant_wav(path: &Path, sample_rate: u32, frames: usize, value: f32) {
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
    for _ in 0..frames {
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn project_text(file: &str) -> String {
    format!(
        r#"@project "Audio CLI"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "{file}"
  gain: -6dB
@track tape | instrument: audio/clip
"#
    )
}

#[test]
fn cli_render_places_audio_clip_at_the_right_sample_and_gain() {
    let dir = temp_dir("render");
    let media = dir.join("hit.wav");
    write_constant_wav(&media, 48_000, 48_000, 0.5);
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text("hit.wav")).unwrap();
    let output = dir.join("out.wav");

    let rendered = run(&[
        "render",
        path(&project),
        "--output",
        path(&output),
        "--sample-rate",
        "48000",
    ]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let mut reader = hound::WavReader::open(&output).unwrap();
    assert_eq!(reader.spec().sample_rate, 48_000);
    assert_eq!(reader.spec().channels, 2);
    let samples = reader
        .samples::<i16>()
        .map(|sample| sample.unwrap() as f32 / 32767.0)
        .collect::<Vec<_>>();
    let start = 2 * 48_000 * 2;
    assert!(samples[..start].iter().all(|sample| sample.abs() < 1e-4));
    // Constant-power center pan plus -6 dB clip gain.
    let expected = 0.5 * 10.0_f32.powf(-6.0 / 20.0) * std::f32::consts::FRAC_1_SQRT_2;
    for frame in 0..1000 {
        assert!((samples[start + frame * 2] - expected).abs() < 2.0 / 32767.0);
        assert!((samples[start + frame * 2 + 1] - expected).abs() < 2.0 / 32767.0);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_validate_and_render_name_missing_audio_clips() {
    let dir = temp_dir("missing");
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text("missing.wav")).unwrap();

    let validated = run(&["validate", path(&project)]);
    assert!(!validated.status.success());
    let stderr = String::from_utf8_lossy(&validated.stderr);
    assert!(stderr.contains("audio clip 'hit'"), "{stderr}");
    assert!(stderr.contains("missing.wav"), "{stderr}");

    let output = dir.join("out.wav");
    let rendered = run(&[
        "render",
        path(&project),
        "--output",
        path(&output),
        "--sample-rate",
        "48000",
    ]);
    assert!(!rendered.status.success());
    let stderr = String::from_utf8_lossy(&rendered.stderr);
    assert!(stderr.contains("audio clip 'hit'"), "{stderr}");
    assert!(stderr.contains("missing.wav"), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn stretch_project_text(file: &str, stretch: f64) -> String {
    format!(
        r#"@project "Audio CLI stretch"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "{file}"
  stretch: {stretch}
  mode: percussive
@track tape | instrument: audio/clip
"#
    )
}

#[test]
fn cli_render_bakes_a_two_to_one_stretch_into_the_range() {
    let dir = temp_dir("stretch");
    let media = dir.join("hit.wav");
    write_constant_wav(&media, 48_000, 48_000, 0.25);
    let project = dir.join("project.ocp");
    std::fs::write(&project, stretch_project_text("hit.wav", 2.0)).unwrap();
    let output = dir.join("out.wav");

    let rendered = run(&[
        "render",
        path(&project),
        "--output",
        path(&output),
        "--sample-rate",
        "48000",
    ]);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let mut reader = hound::WavReader::open(&output).unwrap();
    let samples = reader
        .samples::<i16>()
        .map(|sample| sample.unwrap() as f32 / 32767.0)
        .collect::<Vec<_>>();
    let start = 2 * 48_000 * 2;
    let end = 4 * 48_000 * 2;
    assert!(samples[..start].iter().all(|sample| sample.abs() < 1e-4));
    // The one-second media now fills the two-second range.
    for frame in [start + 1_000, end - 1_000] {
        assert!(
            samples[frame].abs() > 0.05,
            "stretched clip is silent at {frame}"
        );
    }
    assert!(samples[end..].iter().all(|sample| sample.abs() < 1e-4));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_validate_refuses_an_out_of_range_stretch_ratio() {
    let dir = temp_dir("stretch-range");
    let media = dir.join("hit.wav");
    write_constant_wav(&media, 48_000, 48_000, 0.25);
    let project = dir.join("project.ocp");
    std::fs::write(&project, stretch_project_text("hit.wav", 5.0)).unwrap();

    let validated = run(&["validate", path(&project)]);
    assert!(!validated.status.success());
    let stderr = String::from_utf8_lossy(&validated.stderr);
    assert!(stderr.contains("audio clip 'hit'"), "{stderr}");
    assert!(stderr.contains("outside"), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn write_tone_wav(path: &Path, sample_rate: u32, frames: usize, frequency: f64) {
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
    for frame in 0..frames {
        let value = (2.0 * std::f64::consts::PI * frequency * frame as f64 / f64::from(sample_rate))
            .sin() as f32
            * 0.5;
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn resample_project_text(file: &str, export: Option<u8>) -> String {
    let export = export
        .map(|step| format!("  export: {{resample: {step}}}\n"))
        .unwrap_or_default();
    format!(
        r#"@project "Audio CLI resample"
  tempo: 120
{export}@audio_clip hit [2:1 - 3:1] | track: tape
  file: "{file}"
  gain: -6dB
@track tape | instrument: audio/clip
"#
    )
}

fn render_bytes(project: &Path, output: &Path, quality: Option<u8>) -> Vec<u8> {
    let mut args = vec![
        "render".to_string(),
        path(project).to_string(),
        "--output".to_string(),
        path(output).to_string(),
        "--sample-rate".to_string(),
        "48000".to_string(),
    ];
    if let Some(step) = quality {
        args.push("--resample-quality".to_string());
        args.push(step.to_string());
    }
    let rendered = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    std::fs::read(output).unwrap()
}

#[test]
fn cli_resample_quality_flag_and_project_setting_agree() {
    let dir = temp_dir("resample-quality");
    let media = dir.join("tone.wav");
    // A near-Nyquist tone: the rung decides how much imaging reaches the band.
    write_tone_wav(&media, 44_100, 44_100, 15_000.0);
    let project = dir.join("project.ocp");
    std::fs::write(&project, resample_project_text("tone.wav", None)).unwrap();
    let fast = render_bytes(&project, &dir.join("fast.wav"), Some(0));
    let best = render_bytes(&project, &dir.join("best.wav"), Some(7));
    assert_eq!(
        fast.len(),
        best.len(),
        "the rung must not change the length"
    );
    assert_ne!(fast, best, "different rungs must render different audio");

    let project_with_setting = dir.join("setting.ocp");
    std::fs::write(
        &project_with_setting,
        resample_project_text("tone.wav", Some(0)),
    )
    .unwrap();
    let from_setting = render_bytes(&project_with_setting, &dir.join("setting.wav"), None);
    assert_eq!(
        from_setting, fast,
        "the project setting must select the rung"
    );
    let overridden = render_bytes(&project_with_setting, &dir.join("override.wav"), Some(7));
    assert_eq!(
        overridden, best,
        "the flag must override the project setting"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
