//! `ocp render --soundfont`: the legacy fallback must reach the project mixer.

use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-cli-soundfont", name)
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

/// Minimal one-preset sine font; every fallback render needs a real font file.
fn fixture_font(dir: &Path) -> PathBuf {
    let path = dir.join("minimal-test.sf2");
    std::fs::write(
        &path,
        include_bytes!("../../../oc-engine/tests/fixtures/minimal-test.sf2"),
    )
    .unwrap();
    path
}

fn project_text(volume_db: f64, pan: f64) -> String {
    format!(
        r#"@ocp "0.2.0"

@project "SoundFont CLI"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: soundfont
  program: 0

@track keys | preset: tone, instrument: piano/acoustic_grand
  mix: vol={volume_db}dB, pan={pan}
  @phrase held [1:1 - 2:1]
    C4 w mf
"#
    )
}

fn channel_rms(path: &Path, channel: usize) -> f64 {
    let mut reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().channels, 2);
    let samples: Vec<f64> = reader
        .samples::<i16>()
        .map(|sample| f64::from(sample.unwrap()) / 32768.0)
        .collect();
    let mut sum = 0.0;
    let mut count = 0usize;
    for frame in samples.chunks_exact(2) {
        sum += frame[channel] * frame[channel];
        count += 1;
    }
    (sum / count as f64).sqrt()
}

fn render(dir: &Path, text: &str, font: &Path, output: &Path, extra: &[&str]) {
    let source = dir.join(format!(
        "{}.ocp",
        output.file_stem().unwrap().to_string_lossy()
    ));
    std::fs::write(&source, text).unwrap();
    let mut args = vec![
        "render",
        source.to_str().unwrap(),
        "--soundfont",
        font.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ];
    args.extend_from_slice(extra);
    let output = run(&args);
    assert!(
        output.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn soundfont_render_applies_the_track_fader_and_pan() {
    let dir = temp_dir("mix");
    let font = fixture_font(&dir);
    let neutral_wav = dir.join("neutral.wav");
    let faded_wav = dir.join("faded.wav");
    let panned_wav = dir.join("panned.wav");
    render(&dir, &project_text(0.0, 0.0), &font, &neutral_wav, &[]);
    render(&dir, &project_text(-6.0, 0.0), &font, &faded_wav, &[]);
    render(&dir, &project_text(0.0, -1.0), &font, &panned_wav, &[]);

    let spec = hound::WavReader::open(&neutral_wav).unwrap().spec();
    assert_eq!(spec.sample_rate, 48_000);
    let neutral_left = channel_rms(&neutral_wav, 0);
    let neutral_right = channel_rms(&neutral_wav, 1);
    assert!(neutral_left > 0.001, "fixture must produce audio");
    assert!(neutral_right > 0.001, "fixture must produce audio");

    let expected = 10.0_f64.powf(-6.0 / 20.0);
    for channel in [0, 1] {
        let ratio = channel_rms(&faded_wav, channel) / channel_rms(&neutral_wav, channel);
        assert!(
            (ratio - expected).abs() < 0.01 * expected,
            "--soundfont must honor the fader: ratio {ratio} vs {expected}"
        );
    }
    let panned_right = channel_rms(&panned_wav, 1);
    assert!(
        panned_right < 1e-6,
        "--soundfont must honor pan: right {panned_right} is not muted"
    );
    let panned_left = channel_rms(&panned_wav, 0);
    assert!(
        panned_left >= neutral_left * 0.99,
        "--soundfont hard left must keep the left channel: {panned_left} vs {neutral_left}"
    );
}

#[test]
fn per_track_mix_flag_agrees_with_the_legacy_soundfont_route() {
    let dir = temp_dir("flags");
    let font = fixture_font(&dir);
    let mut ratios = Vec::new();
    for extra in [&[][..], &["--per-track-mix"][..]] {
        let neutral_wav = dir.join(format!("neutral-{}.wav", extra.len()));
        let mixed_wav = dir.join(format!("mixed-{}.wav", extra.len()));
        render(&dir, &project_text(0.0, 0.0), &font, &neutral_wav, extra);
        render(&dir, &project_text(-6.0, 0.0), &font, &mixed_wav, extra);
        ratios.push(channel_rms(&mixed_wav, 0) / channel_rms(&neutral_wav, 0));
    }
    let expected = 10.0_f64.powf(-6.0 / 20.0);
    for ratio in &ratios {
        assert!(
            (ratio - expected).abs() < 0.01 * expected,
            "--soundfont route mismatch: {ratios:?}"
        );
    }
    let rerun_source = dir.join("rerun.ocp");
    std::fs::write(&rerun_source, project_text(-6.0, 0.0)).unwrap();
    let rerun = run(&[
        "render",
        rerun_source.to_str().unwrap(),
        "--soundfont",
        font.to_str().unwrap(),
        "--output",
        dir.join("rerun.wav").to_str().unwrap(),
        "--sample-rate",
        "48000",
        "--reruns",
        "2",
        "--require-identical",
    ]);
    assert!(
        rerun.status.success(),
        "determinism flags failed: {}",
        String::from_utf8_lossy(&rerun.stderr)
    );
}
