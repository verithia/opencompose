//! What `ocp render` says when a track needs a SoundFont the project names but
//! does not ship, and that a builtin project renders the same bytes as the
//! engine entry point the app's offline render calls.
//!
//! The two verdicts must agree: the app and the CLI refuse the same project, and
//! the message names the preset.

use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-cli-soundfont-refusal", name)
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

/// The reported shape: a builtin preset declared alongside a track whose
/// instrument names a SoundFont instrument, and a `source: soundfont` preset
/// whose file is not on disk.
const BROKEN_KIT: &str = r#"@ocp "0.4.0"

@project "Broken Kit"
  tempo: 110
  time_signature: 4/4

@preset kit
  source: soundfont
  program: 0
  file: "media/missing.sf2"

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@pattern groove | instrument: drums/acoustic_kit, preset: kit
  hits:
    kick  1:0 ff
    snare 2:0 f

@track drums | instrument: drums/acoustic_kit, preset: kit
  clip: groove [1:1 - 3:1]

@track lead | instrument: synth/lead_square, preset: lead
  @phrase hook [1:1 - 3:1]
    C4 q mf
    E4 q mf
"#;

const BUILTIN_WITH_PATTERN: &str = r#"@ocp "0.4.0"

@project "Builtin Pattern"
  tempo: 100
  time_signature: 4/4

@preset pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 1, osc.a.level: 0.5, filter.cutoff: 2200Hz}

@pattern chords | preset: pad, instrument: synth/pad
  events:
    @0 A3 w mp, C4 w mp, E4 w mp
    @4 F3 w mp, A3 w mp, C4 w mp

@track pads | instrument: synth/pad, preset: pad
  clip: chords [1:1 - 3:1]
"#;

fn write_project(dir: &Path, source: &str) -> PathBuf {
    let path = dir.join("song.ocp");
    std::fs::write(&path, source).unwrap();
    path
}

#[test]
fn a_missing_declared_bank_exits_one_and_names_track_preset_and_file() {
    let dir = temp_dir("missing");
    let project = write_project(&dir, BROKEN_KIT);
    let output = dir.join("out.wav");
    let rendered = run(&[
        "render",
        project.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--sample-rate",
        "48000",
    ]);

    assert_eq!(rendered.status.code(), Some(1), "render must fail");
    assert!(!output.exists(), "a refused render must not write a WAV");
    let stderr = String::from_utf8_lossy(&rendered.stderr);
    assert!(
        stderr.contains("Error: Render error: Track 'drums' needs a SoundFont"),
        "stderr did not name the track: {stderr}"
    );
    assert!(stderr.contains("preset 'kit'"), "{stderr}");
    assert!(stderr.contains("media/missing.sf2"), "{stderr}");
    assert!(
        !stderr.contains("Render error: :"),
        "the empty-path refusal came back: {stderr}"
    );
}

#[test]
fn a_builtin_project_renders_byte_identically_to_the_engine_entry_point() {
    let dir = temp_dir("builtin");
    let project_path = write_project(&dir, BUILTIN_WITH_PATTERN);
    let cli_out = dir.join("cli.wav");
    let rendered = run(&[
        "render",
        project_path.to_str().unwrap(),
        "-o",
        cli_out.to_str().unwrap(),
        "--sample-rate",
        "48000",
        "--pre-roll",
        "0",
    ]);
    assert!(
        rendered.status.success(),
        "the CLI refused a builtin project: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );

    // The app's offline render calls this entry point; the app has no
    // SoundFont configured, so the fallback path is empty.
    let project = oc_dsl::parse(BUILTIN_WITH_PATTERN).unwrap();
    let (audio, _cache) = oc_engine::render::render_full_for_export(
        &project,
        Path::new(""),
        48_000,
        0.0,
        &mut std::collections::HashMap::new(),
        Some(&dir),
        0.0,
        0.0,
    )
    .expect("the engine entry point renders a builtin project");
    let spec = oc_engine::encode::EncodeSpec::for_project(&project, 48_000);
    let engine_out = dir.join("engine.wav");
    oc_engine::encode::encode_to_file(&engine_out, &spec, &audio).unwrap();
    assert_eq!(
        std::fs::read(&cli_out).unwrap(),
        std::fs::read(&engine_out).unwrap(),
        "CLI and the app's offline render wrote different bytes"
    );

    // The app's range and audition tools stream through this entry point.
    // A pattern preview must not reach the loader with an empty fallback path.
    let mut streaming = oc_engine::render_cache::RenderCache::new();
    streaming.set_resource_root(Some(dir.clone()));
    let streamed = streaming
        .render_incremental(
            &project,
            Path::new(""),
            48_000,
            &mut std::collections::HashMap::new(),
        )
        .expect("the app's streaming entry point renders a builtin project");
    let stream_out = dir.join("stream.wav");
    oc_engine::encode::encode_to_file(&stream_out, &spec, &streamed).unwrap();
    assert_eq!(
        std::fs::read(&cli_out).unwrap(),
        std::fs::read(&stream_out).unwrap(),
        "CLI and the app's streaming render wrote different bytes"
    );
}
