//! `OCP_SIMD` must reach the render path: a forced backend writes the same bytes.

use std::path::{Path, PathBuf};

const PROJECT: &str = r#"@ocp "0.2.0"

@project "SIMD identity"
  tempo: 132
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.35, waveform: 0}

@bus space
  mix: vol=-4.5dB
  routes: [master]

@track lead | preset: tone, instrument: synth/lead_square
  mix: vol=-6.2dB, pan=0.35, send(space=-6dB, pre), polarity=invert
  @phrase line [1:1 - 2:1]
    C4 8th mf
    E4 8th mf
    G4 q mf
    B4 q mf

@master
  volume: -1.5dB
"#;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-simd", name)
}

fn render(dir: &Path, tag: &str, simd: Option<&str>) -> Vec<u8> {
    let project = dir.join("song.ocp");
    std::fs::write(&project, PROJECT).unwrap();
    let output = dir.join(format!("{tag}.wav"));
    let mut command = crate::ocp();
    command.args([
        "render",
        project.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);
    if let Some(level) = simd {
        command.env("OCP_SIMD", level);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(&output).unwrap()
}

#[test]
fn forced_backends_write_the_same_bytes() {
    let dir = temp_dir("cli");
    let default = render(&dir, "default", None);

    let mut reader = hound::WavReader::open(dir.join("default.wav")).unwrap();
    assert!(
        reader.samples::<i16>().any(|sample| sample.unwrap() != 0),
        "the test project must produce audio"
    );

    assert_eq!(default, render(&dir, "scalar", Some("scalar")));
    assert_eq!(default, render(&dir, "sse2", Some("sse2")));

    // An unknown name is ignored instead of failing the render.
    assert_eq!(default, render(&dir, "unknown", Some("avx512")));
}
