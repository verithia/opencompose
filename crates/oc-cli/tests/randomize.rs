//! `ocp randomize` writes seeded, safe variations of a project preset.

use std::path::{Path, PathBuf};
use std::process::Output;

const PROJECT: &str = r#"@ocp "0.2.0"
@project "Randomize probe"
  tempo: 120
  time_signature: 4/4

@preset probe
  source: builtin
  plugin: "builtin.open_synth"
  params: { osc.a.waveform: 1, filter.cutoff: 800, filter.resonance: 0.2, amp.attack: 0.01, amp.sustain: 0.8, output.gain: 0.42 }

@track probe | preset: probe, instrument: synth/virtual_analog
  @phrase figure [1:1 - 2:1]
    C2 q mf
"#;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-randomize", name)
}

fn run(args: &[&str]) -> Output {
    crate::ocp().args(args).output().unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn variants(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ocp"))
        .collect();
    files.sort();
    files
}

#[test]
fn randomize_writes_seeded_variations_and_keeps_the_gain() {
    let dir = temp_dir("seeded");
    let project = dir.join("probe.ocp");
    std::fs::write(&project, PROJECT).unwrap();

    let first = dir.join("first");
    let output = run(&[
        "randomize",
        path(&project),
        "--count",
        "3",
        "--seed",
        "7",
        "--output",
        path(&first),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = variants(&first);
    assert_eq!(files.len(), 3, "{files:?}");
    let bodies: Vec<String> = files
        .iter()
        .map(|file| std::fs::read_to_string(file).unwrap())
        .collect();
    for body in &bodies {
        assert!(body.contains("@preset probe"), "{body}");
        assert!(body.contains("output.gain: 0.42"), "{body}");
    }
    assert!(
        bodies[0] != bodies[1] || bodies[1] != bodies[2],
        "seeds should move the patch"
    );

    let second = dir.join("second");
    let again = run(&[
        "randomize",
        path(&project),
        "--count",
        "3",
        "--seed",
        "7",
        "--output",
        path(&second),
    ]);
    assert!(again.status.success());
    let repeated: Vec<String> = variants(&second)
        .iter()
        .map(|file| std::fs::read_to_string(file).unwrap())
        .collect();
    assert_eq!(
        bodies, repeated,
        "the same seed must rebuild the same files"
    );
}

#[test]
fn randomize_rejects_a_project_without_an_opensynth_preset() {
    let dir = temp_dir("no-preset");
    let project = dir.join("plain.ocp");
    std::fs::write(&project, "@project\n  tempo: 120\n").unwrap();
    let output = run(&[
        "randomize",
        path(&project),
        "--output",
        path(&dir.join("out")),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no OpenSynth preset"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
