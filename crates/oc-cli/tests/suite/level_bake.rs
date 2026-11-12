//! `ocp level` bakes a track leveler into the project's own gain lane.

use std::path::{Path, PathBuf};
use std::process::Output;

const RATE: u32 = 48_000;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-level-bake", name)
}

/// A 20 s take that falls 10 dB, so the leveler has something to ride.
fn fading_take(seconds: f64) -> Vec<f32> {
    let frames = (f64::from(RATE) * seconds) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for index in 0..frames {
        let time = index as f64 / f64::from(RATE);
        let amplitude = 0.5 * 10f64.powf(-10.0 * time / seconds / 20.0);
        let value = (amplitude * (std::f64::consts::TAU * 220.0 * time).sin()) as f32;
        samples.push(value);
        samples.push(value);
    }
    samples
}

const PROJECT: &str = r#"@project "Leveler bake CLI"
  tempo: 120
@audio_clip take [1:1 - 11:1] | track: vocal
  file: "take.wav"
@track vocal | instrument: audio/clip
  mix: vol=0dB
  effects:
    - leveler: target=-18dB, speed=4, window=800ms, range=18, gate=-45dB, gate_release=1200ms
"#;

fn run(args: &[&str], dir: &Path) -> Output {
    crate::ocp()
        .args(args)
        .current_dir(dir)
        .env("OCP_MEDIA_CACHE_DIR", dir.join("media"))
        .env("OCP_DERIVED_CACHE_DIR", dir.join("derived"))
        .output()
        .unwrap()
}

#[test]
fn cli_bakes_the_lane_and_fmt_keeps_it() {
    let dir = temp_dir("write");
    oc_engine::wav::write_stereo_wav(dir.join("take.wav"), &fading_take(20.0), RATE).unwrap();
    let project = dir.join("project.ocp");
    std::fs::write(&project, PROJECT).unwrap();

    let baked = run(
        &[
            "level",
            project.to_str().unwrap(),
            "--track",
            "vocal",
            "--write",
        ],
        &dir,
    );
    assert!(
        baked.status.success(),
        "{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    let text = std::fs::read_to_string(&project).unwrap();
    assert!(
        text.contains("gain_automation: linear ppq=4800"),
        "the baked project has no gain lane:\n{text}"
    );
    assert!(
        !text.contains("leveler:"),
        "the baked project still carries the leveler:\n{text}"
    );

    let formatted = run(&["fmt", project.to_str().unwrap()], &dir);
    assert!(
        formatted.status.success(),
        "{}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    assert_eq!(
        String::from_utf8(formatted.stdout).unwrap(),
        text,
        "ocp fmt changed the baked document"
    );

    let parsed = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&parsed).unwrap();
    let lane = &parsed.tracks[0].gain_automation[0];
    assert!(
        lane.points.len() > 1,
        "the lane kept {} point(s)",
        lane.points.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_refuses_an_unknown_track() {
    let dir = temp_dir("refuse");
    oc_engine::wav::write_stereo_wav(dir.join("take.wav"), &fading_take(20.0), RATE).unwrap();
    let project = dir.join("project.ocp");
    std::fs::write(&project, PROJECT).unwrap();

    let result = run(
        &[
            "level",
            project.to_str().unwrap(),
            "--track",
            "lead",
            "--write",
        ],
        &dir,
    );
    assert!(!result.status.success(), "an unknown track was accepted");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("lead"), "{stderr}");
    assert!(stderr.contains("not found"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&project).unwrap(),
        PROJECT,
        "a refusal changed the project"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
