//! `ocp render --sample-format/--dither`: the file must state its own precision.

use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("ocp-render-format", name)
}

fn project_text(header_extra: &str) -> String {
    project_text_tracks(header_extra, 1)
}

/// The test tone sits at -3 dBFS per track after the pan law, so each extra
/// track adds 3 dB: two tracks clip, four are refused by the clip margin.
fn project_text_tracks(header_extra: &str, tracks: usize) -> String {
    let mut text = format!(
        r#"@ocp "0.2.0"

@project "export format"
  tempo: 120
  time_signature: 4/4
{header_extra}
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 440, amplitude: 1.0, waveform: 0}}
"#
    );
    for index in 1..=tracks {
        text.push_str(&format!(
            r#"
@track tone{index} | preset: tone, instrument: piano/acoustic_grand
  @phrase held [1:1 - 2:1]
    C4 w mf
"#
        ));
    }
    text
}

fn render(dir: &Path, text: &str, name: &str, extra: &[&str]) -> std::process::Output {
    render_as(dir, text, name, &format!("{name}.wav"), extra)
}

/// The same render with an explicit target name, so a test can leave the
/// extension off or name a container.
fn render_as(
    dir: &Path,
    text: &str,
    name: &str,
    output_name: &str,
    extra: &[&str],
) -> std::process::Output {
    let source = dir.join(format!("{name}.ocp"));
    std::fs::write(&source, text).unwrap();
    let output = dir.join(output_name);
    let mut args = vec![
        "render",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
        "--no-loudness-normalisation",
    ];
    args.extend_from_slice(extra);
    crate::ocp().args(&args).output().unwrap()
}

/// Decode a FLAC file to its exact integer samples.
fn flac_pcm(path: &Path) -> (Vec<i32>, u16, u32) {
    let mut reader = claxon::FlacReader::open(path).expect("the FLAC decodes");
    let info = reader.streaminfo();
    let samples = reader.samples().map(|sample| sample.unwrap()).collect();
    (samples, info.bits_per_sample as u16, info.sample_rate)
}

fn spec(path: &Path) -> hound::WavSpec {
    hound::WavReader::open(path).unwrap().spec()
}

#[test]
fn sample_format_selects_the_file_precision() {
    let dir = temp_dir("formats");
    let text = project_text("");
    for (flag, bits, float) in [("i16", 16, false), ("i24", 24, false), ("f32", 32, true)] {
        let output = render(&dir, &text, flag, &["--sample-format", flag]);
        assert!(
            output.status.success(),
            "{flag} render failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let spec = spec(&dir.join(format!("{flag}.wav")));
        assert_eq!(spec.bits_per_sample, bits, "{flag}");
        assert_eq!(
            spec.sample_format,
            if float {
                hound::SampleFormat::Float
            } else {
                hound::SampleFormat::Int
            },
            "{flag}"
        );
    }

    let default = render(&dir, &text, "default", &[]);
    assert!(default.status.success());
    assert_eq!(spec(&dir.join("default.wav")).bits_per_sample, 16);
}

#[test]
fn a_project_export_block_sets_the_default_and_flags_override_it() {
    let dir = temp_dir("project");
    let text = project_text("  export: {format: i24, dither: true}\n");
    let output = render(&dir, &text, "project", &[]);
    assert!(
        output.status.success(),
        "project export render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(spec(&dir.join("project.wav")).bits_per_sample, 24);

    let overridden = render(
        &dir,
        &text,
        "override",
        &["--sample-format", "f32", "--no-dither"],
    );
    assert!(
        overridden.status.success(),
        "override render failed: {}",
        String::from_utf8_lossy(&overridden.stderr)
    );
    let spec = spec(&dir.join("override.wav"));
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
}

#[test]
fn a_render_reports_the_written_path_once() {
    let dir = temp_dir("wrote-path");
    let output = render(&dir, &project_text(""), "wrote-path", &[]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("  Wrote ").count(), 1, "{stdout}");
    assert!(
        stdout.contains(&dir.join("wrote-path.wav").display().to_string()),
        "{stdout}"
    );
}

#[test]
fn dithered_renders_are_reproducible_across_isolated_runs() {
    let dir = temp_dir("dither");
    let text = project_text("");
    let output = render(
        &dir,
        &text,
        "dither",
        &["--dither", "--reruns", "2", "--require-identical"],
    );
    assert!(
        output.status.success(),
        "dithered rerun failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("runs byte-identical"),
        "dither must not break determinism: {stdout}"
    );
}

#[test]
fn float_output_refuses_dither_with_a_readable_message() {
    let dir = temp_dir("refused");
    let output = render(
        &dir,
        &project_text(""),
        "refused",
        &["--sample-format", "f32", "--dither"],
    );
    assert!(!output.status.success(), "float dither must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("meaningless for f32"),
        "unreadable refusal: {stderr}"
    );
    assert!(!dir.join("refused.wav").exists());
}

#[test]
fn the_container_flag_and_bit_depth_alias_select_the_file() {
    let dir = temp_dir("container");
    let output = render(
        &dir,
        &project_text(""),
        "container",
        &["--format", "wav", "--bit-depth", "i24"],
    );
    assert!(
        output.status.success(),
        "container render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Export: wav, i24 samples"), "{stdout}");
    assert_eq!(spec(&dir.join("container.wav")).bits_per_sample, 24);
}

#[test]
fn an_unknown_container_is_refused_by_name_with_the_list_this_build_writes() {
    let dir = temp_dir("unknown-container");
    let refused = render(&dir, &project_text(""), "unknown", &["--format", "ogg"]);
    assert!(!refused.status.success(), "an unknown container must fail");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("unknown export container 'ogg'") && stderr.contains("wav, flac"),
        "{stderr}"
    );
    assert!(!dir.join("unknown.wav").exists());
}

/// FLAC is lossless: the decoded integers are the WAV writer's integers.
#[test]
fn a_flac_render_decodes_to_the_wav_samples_at_the_same_depth() {
    let dir = temp_dir("flac-lossless");
    let text = project_text("");
    let wav = render_as(
        &dir,
        &text,
        "wav24",
        "wav24.wav",
        &["--sample-format", "i24"],
    );
    assert!(
        wav.status.success(),
        "WAV render failed: {}",
        String::from_utf8_lossy(&wav.stderr)
    );
    let flac = render_as(
        &dir,
        &text,
        "flac24",
        "flac24.flac",
        &["--format", "flac", "--sample-format", "i24"],
    );
    assert!(
        flac.status.success(),
        "FLAC render failed: {}",
        String::from_utf8_lossy(&flac.stderr)
    );
    let stdout = String::from_utf8_lossy(&flac.stdout);
    assert!(stdout.contains("Export: flac, i24 samples"), "{stdout}");
    assert!(stdout.contains("FLAC compression: level 5"), "{stdout}");

    let (pcm, bits, rate) = flac_pcm(&dir.join("flac24.flac"));
    assert_eq!(bits, 24);
    assert_eq!(rate, 48_000);
    let mut reader = hound::WavReader::open(dir.join("wav24.wav")).unwrap();
    let from_wav: Vec<i32> = reader
        .samples::<i32>()
        .map(|sample| sample.unwrap())
        .collect();
    assert_eq!(pcm, from_wav, "FLAC and WAV share the integer grid");
}

#[test]
fn the_output_extension_follows_the_container_and_a_mismatch_is_refused() {
    let dir = temp_dir("flac-extension");
    let text = project_text("");
    // No extension: the container appends its own.
    let bare = render_as(&dir, &text, "bare", "bare", &["--format", "flac"]);
    assert!(
        bare.status.success(),
        "extensionless output failed: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(dir.join("bare.flac").is_file());
    assert!(!dir.join("bare.wav").exists());

    let mismatch = render_as(
        &dir,
        &text,
        "mismatch",
        "mismatch.wav",
        &["--format", "flac"],
    );
    assert!(!mismatch.status.success(), "a .wav name cannot hold FLAC");
    let stderr = String::from_utf8_lossy(&mismatch.stderr);
    assert!(
        stderr.contains(".wav") && stderr.contains(".flac"),
        "{stderr}"
    );
    assert!(!dir.join("mismatch.wav").exists());
}

#[test]
fn a_flac_compression_level_changes_the_file_size_not_the_audio() {
    let dir = temp_dir("flac-level");
    let text = project_text("");
    let fast = render_as(
        &dir,
        &text,
        "fast",
        "fast.flac",
        &[
            "--format",
            "flac",
            "--sample-format",
            "i24",
            "--flac-level",
            "0",
        ],
    );
    assert!(
        fast.status.success(),
        "level 0 failed: {}",
        String::from_utf8_lossy(&fast.stderr)
    );
    let best = render_as(
        &dir,
        &text,
        "best",
        "best.flac",
        &[
            "--format",
            "flac",
            "--sample-format",
            "i24",
            "--flac-level",
            "8",
        ],
    );
    assert!(
        best.status.success(),
        "level 8 failed: {}",
        String::from_utf8_lossy(&best.stderr)
    );
    let fast_size = std::fs::metadata(dir.join("fast.flac")).unwrap().len();
    let best_size = std::fs::metadata(dir.join("best.flac")).unwrap().len();
    assert!(
        best_size < fast_size,
        "level 8 ({best_size} B) must be smaller than level 0 ({fast_size} B)"
    );
    assert_eq!(
        flac_pcm(&dir.join("fast.flac")).0,
        flac_pcm(&dir.join("best.flac")).0,
        "the level changes bytes, not audio"
    );
}

#[test]
fn a_sample_rate_outside_the_sampler_range_is_refused() {
    let dir = temp_dir("rate");
    let source = dir.join("rate.ocp");
    std::fs::write(&source, project_text("")).unwrap();
    let output = crate::ocp()
        .args([
            "render",
            source.to_str().unwrap(),
            "--sample-rate",
            "0",
            "--output",
            dir.join("rate.wav").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success(), "rate 0 must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("8000-192000"), "{stderr}");
}

/// Re-reading the written bytes makes a clipped file look like a clean
/// 0.00 dBFS master, so `--report` uses the pre-clamp measurement, which says
/// what the mix did.
#[test]
fn a_clipped_render_reports_the_peak_before_quantisation_and_warns() {
    let dir = temp_dir("clipped");
    let output = render(&dir, &project_text_tracks("", 2), "clipped", &["--report"]);
    assert!(
        output.status.success(),
        "a clipped render still writes: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Mix before quantisation: peak +3.0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("clipped at full scale in the written file"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("was clipped at full scale"), "{stderr}");
    // The written file itself reads 0 dBFS, which is why it cannot report the
    // overload on its own.
    assert_eq!(spec(&dir.join("clipped.wav")).bits_per_sample, 16);
}

/// A float export of the same hot mix keeps its peaks, so it must not report a
/// clamp that never happened.
#[test]
fn a_hot_float_render_reports_peaks_above_full_scale_not_clipping() {
    let dir = temp_dir("float-hot");
    let output = render(
        &dir,
        &project_text_tracks("", 2),
        "float-hot",
        &["--sample-format", "f32", "--report"],
    );
    assert!(
        output.status.success(),
        "a hot float render still writes: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Mix before quantisation: peak +3.0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("peaks above full scale; the float file keeps them"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("clipped"),
        "a float export clamps nothing: {stderr}"
    );
    assert_eq!(
        spec(&dir.join("float-hot.wav")).sample_format,
        hound::SampleFormat::Float
    );
}

#[test]
fn a_mix_far_above_full_scale_is_refused_and_leaves_no_file() {
    let dir = temp_dir("over");
    let output = render(&dir, &project_text_tracks("", 4), "over", &[]);
    assert!(!output.status.success(), "twice full scale must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("more than 6 dB above full scale"),
        "{stderr}"
    );
    assert!(
        !dir.join("over.wav").exists(),
        "a refusal left a file behind"
    );
}
