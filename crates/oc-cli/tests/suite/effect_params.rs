//! `ocp validate` refuses an effect value the renderer refuses.

use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-cli-effect", name)
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

/// The value a capability run wrote (`eq: high_gain=+2dB@8kHz`) is a retired
/// shorthand's shape on a current control. Validation and render must both
/// refuse it, and the message must name the value and one spelling the device
/// accepts.
#[test]
fn validate_and_render_both_refuse_a_typed_effect_value_and_name_it() {
    let dir = temp_dir("typed-value");
    let project = dir.join("project.ocp");
    std::fs::write(
        &project,
        r#"@project "Typed effect"
  tempo: 120
@track lead | instrument: synth/melody
  effects:
    - eq: high_gain=+2dB@8kHz
  @phrase riff [1:1:0 - 2:1:0]
    C4 q mf
"#,
    )
    .unwrap();

    let validated = run(&["validate", project.to_str().unwrap()]);
    assert!(
        !validated.status.success(),
        "validate accepted a value the render refuses"
    );
    let stderr = String::from_utf8_lossy(&validated.stderr);
    assert!(stderr.contains("high_gain=+2dB@8kHz"), "{stderr}");
    assert!(stderr.contains("high_gain=0dB"), "{stderr}");

    let output = dir.join("out.wav");
    let rendered = run(&[
        "render",
        project.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);
    assert!(!rendered.status.success());
    let stderr = String::from_utf8_lossy(&rendered.stderr);
    assert!(stderr.contains("high_gain=+2dB@8kHz"), "{stderr}");
    assert!(stderr.contains("high_gain=0dB"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}
