//! `ocp vocal analyze` works headless on a project that references an
//! `@audio_clip`, prints the frame/note contract and reuses its `.ocn` cache.

use std::path::{Path, PathBuf};
use std::process::Output;

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-analyze", name)
}

fn two_note_take(seconds: f64) -> Vec<f32> {
    let frames = (f64::from(RATE) * seconds) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    let mut phase: f64 = 0.0;
    for index in 0..frames {
        let time = index as f64 / f64::from(RATE);
        let f0 = if time < seconds / 2.0 { 220.0 } else { 330.0 };
        let value = (0.3 * phase.sin()) as f32;
        samples.push(value);
        samples.push(value);
        phase += std::f64::consts::TAU * f0 / f64::from(RATE);
    }
    samples
}

fn project_text() -> &'static str {
    r#"@project "Vocal analysis CLI"
  tempo: 120
@audio_clip lead [1:1 - 2:1] | track: vocal
  file: "take.wav"
@track vocal | instrument: audio/clip
  mix: vol=0dB
"#
}

fn run(args: &[&str], dir: &Path) -> Output {
    crate::ocp()
        .args(args)
        .env("OCP_MEDIA_CACHE_DIR", dir.join("media"))
        .env("OCP_DERIVED_CACHE_DIR", dir.join("derived"))
        .output()
        .unwrap()
}

fn analyze_json(project: &Path, selector: &str, output: &Path) -> serde_json::Value {
    let result = run(
        &[
            "vocal",
            "analyze",
            project.to_str().unwrap(),
            selector,
            "--json",
            "--sample-rate",
            "48000",
            "--output",
            output.to_str().unwrap(),
        ],
        project.parent().unwrap(),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

#[test]
fn cli_analyses_a_clip_and_reuses_the_ocn_cache() {
    let dir = temp_dir("cache");
    let samples = two_note_take(2.0);
    oc_engine::wav::write_stereo_wav(dir.join("take.wav"), &samples, RATE).unwrap();
    let project = dir.join("project.ocp");
    std::fs::write(&project, project_text()).unwrap();
    let output = dir.join("lead.ocn");

    let first = analyze_json(&project, "lead", &output);
    assert_eq!(first["clip"], "lead");
    assert_eq!(first["track"], "vocal");
    assert_eq!(first["sample_rate"], RATE);
    assert_eq!(
        first["analyzer_version"],
        oc_dsp::vocal_analysis::ANALYZER_VERSION
    );
    assert_eq!(first["cache"], "miss");
    assert_eq!(first["output"], output.display().to_string());
    assert!(output.exists(), "the .ocn file was not written");
    assert!(
        first["frames"]
            .as_array()
            .is_some_and(|frames| frames.len() > 300),
        "expected a 5 ms frame grid, got {}",
        first["frames"].as_array().map_or(0, Vec::len)
    );
    let notes = first["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "a two-note take reported {notes:?}");
    let first_f0 = notes[0]["f0_median_hz"].as_f64().unwrap();
    let second_f0 = notes[1]["f0_median_hz"].as_f64().unwrap();
    assert!((first_f0 - 220.0).abs() < 1.0, "first note {first_f0}");
    assert!((second_f0 - 330.0).abs() < 1.0, "second note {second_f0}");
    let boundary = notes[1]["start_seconds"].as_f64().unwrap();
    assert!(
        (boundary - 1.0).abs() <= 0.02,
        "second note starts at {boundary}"
    );

    let second = analyze_json(&project, "lead", &output);
    assert_eq!(second["cache"], "hit");
    assert_eq!(
        second["frames"], first["frames"],
        "cache changed the frames"
    );
    assert_eq!(second["notes"], first["notes"], "cache changed the notes");

    // A track name with exactly one clip resolves to that clip.
    let by_track = analyze_json(&project, "vocal", &output);
    assert_eq!(by_track["clip"], "lead");

    // Without --output the file lands in the derived cache directory.
    let default_cache = run(
        &[
            "vocal",
            "analyze",
            project.to_str().unwrap(),
            "lead",
            "--json",
            "--sample-rate",
            "48000",
        ],
        &dir,
    );
    assert!(
        default_cache.status.success(),
        "{}",
        String::from_utf8_lossy(&default_cache.stderr)
    );
    let default_report: serde_json::Value = serde_json::from_slice(&default_cache.stdout).unwrap();
    let cache_path = default_report["output"].as_str().unwrap();
    assert!(
        cache_path.starts_with(dir.join("derived").to_str().unwrap()),
        "cache file {cache_path}"
    );
    assert!(Path::new(cache_path).exists());

    // The table surface is the default and still reports the notes.
    let table = run(
        &[
            "vocal",
            "analyze",
            project.to_str().unwrap(),
            "lead",
            "--sample-rate",
            "48000",
        ],
        &dir,
    );
    assert!(
        table.status.success(),
        "{}",
        String::from_utf8_lossy(&table.stderr)
    );
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.contains("clip 'lead'"), "{text}");
    assert!(text.contains("2 notes"), "{text}");

    let missing = run(
        &[
            "vocal",
            "analyze",
            project.to_str().unwrap(),
            "nope",
            "--json",
        ],
        &dir,
    );
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("audio clip or track 'nope' not found"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    std::fs::remove_dir_all(dir).unwrap();
}
