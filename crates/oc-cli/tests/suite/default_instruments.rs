//! A bare instrument track renders through its injected default unless an
//! explicit `--soundfont` supplies the legacy fallback; `fmt` never injects.

use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("ocp-default-instrument", name)
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

fn fixture_font() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../oc-engine/tests/fixtures/minimal-test.sf2")
        .canonicalize()
        .expect("the minimal SoundFont fixture exists")
}

fn bare_project(dir: &Path) -> PathBuf {
    let source = dir.join("bare.ocp");
    std::fs::write(
        &source,
        "@ocp \"0.2.0\"\n@project \"keys\"\n  tempo: 120\n  time_signature: 4/4\n\
@track keys | instrument: piano/acoustic_grand\n\
  @phrase line [1:1 - 3:1]\n\
    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n\
    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n",
    )
    .unwrap();
    source
}

fn render(dir: &Path, source: &Path, name: &str, soundfont: Option<&Path>) -> PathBuf {
    let output = dir.join(name);
    let mut args = vec![
        "render",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
        "--no-loudness-normalisation",
    ];
    if let Some(font) = soundfont {
        args.push("--soundfont");
        args.push(font.to_str().unwrap());
    }
    let rendered = run(&args);
    assert!(
        rendered.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    output
}

fn db(level: f64) -> f64 {
    20.0 * level.max(1.0e-12).log10()
}

fn peak_dbfs(path: &Path) -> f64 {
    let mut reader = hound::WavReader::open(path).unwrap();
    let samples: Vec<f64> = reader
        .samples::<i16>()
        .map(|sample| f64::from(sample.unwrap()) / 32768.0)
        .collect();
    db(samples.iter().map(|value| value.abs()).fold(0.0, f64::max))
}

/// RMS of the second bar at 120 BPM, 4/4 (stereo interleaved).
fn second_bar_rms_dbfs(path: &Path) -> f64 {
    let mut reader = hound::WavReader::open(path).unwrap();
    let rate = reader.spec().sample_rate as usize;
    let samples: Vec<f64> = reader
        .samples::<i16>()
        .map(|sample| f64::from(sample.unwrap()) / 32768.0)
        .collect();
    let window = &samples[(rate * 2 * 2).min(samples.len())..(rate * 4 * 2).min(samples.len())];
    let power = window.iter().map(|value| value * value).sum::<f64>() / window.len().max(1) as f64;
    db(power.sqrt())
}

#[test]
fn a_hand_written_bare_track_renders_through_its_default() {
    let dir = temp_dir("render");
    let source = bare_project(&dir);

    // `fmt` is a formatting pass: it must leave the document as written.
    let formatted = run(&["fmt", source.to_str().unwrap()]);
    assert!(
        formatted.status.success(),
        "fmt failed: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    let text = String::from_utf8(formatted.stdout).unwrap();
    assert!(!text.contains("oc_default_"), "{text}");

    let output = render(&dir, &source, "keys.wav", None);
    assert!(
        peak_dbfs(&output) > -40.0,
        "peak {} dBFS",
        peak_dbfs(&output)
    );
    assert!(
        second_bar_rms_dbfs(&output) > -40.0,
        "second bar {} dBFS",
        second_bar_rms_dbfs(&output)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_explicit_soundfont_wins_over_the_injected_default() {
    let dir = temp_dir("soundfont-wins");
    let source = bare_project(&dir);
    let font = fixture_font();

    let injected = render(&dir, &source, "injected.wav", None);
    let fallback = render(&dir, &source, "fallback.wav", Some(&font));
    assert_ne!(
        std::fs::read(&injected).unwrap(),
        std::fs::read(&fallback).unwrap(),
        "the {font:?} fallback must change the audio"
    );
    assert!(
        peak_dbfs(&fallback) > -40.0,
        "the fallback render is silent: peak {} dBFS",
        peak_dbfs(&fallback)
    );

    // The fallback path must match a project that names the same font itself,
    // under the same renderer (`--soundfont` is supplied to both).
    let explicit = dir.join("explicit.ocp");
    std::fs::write(
        &explicit,
        format!(
            "@ocp \"0.2.0\"\n@project \"keys\"\n  tempo: 120\n  time_signature: 4/4\n\
@preset grand\n  file: \"{}\"\n\
@track keys | preset: grand, instrument: piano/acoustic_grand\n\
  @phrase line [1:1 - 3:1]\n\
    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n\
    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n",
            font.display()
        ),
    )
    .unwrap();
    let declared = render(&dir, &explicit, "declared.wav", Some(&font));
    assert_eq!(
        std::fs::read(&fallback).unwrap(),
        std::fs::read(&declared).unwrap(),
        "the legacy fallback and a declared preset file must render the same font"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_explicit_preset_still_wins_over_soundfont() {
    let dir = temp_dir("explicit-preset");
    let source = dir.join("mine.ocp");
    std::fs::write(
        &source,
        "@ocp \"0.2.0\"\n@project \"mine\"\n  tempo: 120\n  time_signature: 4/4\n\
@preset mine\n  source: builtin\n  plugin: \"builtin.open_synth\"\n\
  params: {osc.a.waveform: 3, osc.a.level: 0.5, amp.attack: 0.01s, amp.sustain: 0.8, output.gain: 0.8}\n\
@track keys | preset: mine, instrument: piano/acoustic_grand\n\
  @phrase line [1:1 - 3:1]\n\
    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n\
    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n",
    )
    .unwrap();
    let font = fixture_font();
    let missing = dir.join("does-not-exist.sf2");
    let with_font = render(&dir, &source, "with-font.wav", Some(&font));
    let with_missing = render(&dir, &source, "with-missing.wav", Some(&missing));
    assert_eq!(
        std::fs::read(&with_font).unwrap(),
        std::fs::read(&with_missing).unwrap(),
        "a missing --soundfont file must not be opened when an explicit preset wins"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_preset_file_fails_validate_and_names_the_path() {
    let dir = temp_dir("missing");
    let source = dir.join("grand.ocp");
    std::fs::write(
        &source,
        "@project \"grand\"\n  tempo: 120\n\
@preset grand\n  file: \"media/grand.sf2\"\n\
@track keys | preset: grand, instrument: piano/acoustic_grand\n\
  @phrase line [1:1 - 2:1]\n    C4 q mf\n",
    )
    .unwrap();

    let validated = run(&["validate", source.to_str().unwrap()]);
    assert!(!validated.status.success(), "{validated:?}");
    let error = String::from_utf8_lossy(&validated.stderr);
    assert!(error.contains("preset 'grand'"), "{error}");
    assert!(error.contains("media/grand.sf2"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}
