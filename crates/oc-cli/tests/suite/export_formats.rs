//! CLI export formats and the top-level command surface.

use std::path::{Path, PathBuf};
use std::process::Output;

use midly::{Format, MidiMessage, Smf, TrackEventKind};

const PROJECT: &str = r#"@project
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: synth/lead_square
  events:
    C4 q mf
    E4 q mf
    G4 h mf

@track lead | instrument: synth/lead_square
  clip: riff [1:1 - 2:1]
"#;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-export", name)
}

fn run(args: &[&str]) -> Output {
    crate::ocp().args(args).output().unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn export_midi_uses_the_same_smf_writer_as_midi_export() {
    let dir = temp_dir("midi");
    let ocp = dir.join("song.ocp");
    let export = dir.join("export.mid");
    let direct = dir.join("direct.mid");
    std::fs::write(&ocp, PROJECT).unwrap();

    let exported = run(&[
        "export",
        path(&ocp),
        "--format",
        "midi",
        "-o",
        path(&export),
    ]);
    assert!(
        exported.status.success(),
        "export --format midi failed: {}",
        stderr(&exported)
    );

    let directed = run(&["midi", "export", path(&ocp), "--out", path(&direct)]);
    assert!(
        directed.status.success(),
        "midi export failed: {}",
        stderr(&directed)
    );

    let bytes = std::fs::read(&export).unwrap();
    assert_eq!(
        bytes,
        std::fs::read(&direct).unwrap(),
        "export and midi export must share one SMF writer"
    );

    let smf = Smf::parse(&bytes).unwrap();
    assert_eq!(smf.header.format, Format::Parallel);
    let mut keys = Vec::new();
    let mut note_offs = 0;
    for track in &smf.tracks {
        for event in track {
            match event.kind {
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { key, .. },
                    ..
                } => keys.push(u8::from(key)),
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOff { .. },
                    ..
                } => note_offs += 1,
                _ => {}
            }
        }
    }
    keys.sort_unstable();
    assert_eq!(keys, vec![60, 64, 67], "written pitches must round-trip");
    assert_eq!(note_offs, 3);
}

#[test]
fn export_midi_requires_an_output_path() {
    let dir = temp_dir("midi-no-output");
    let ocp = dir.join("song.ocp");
    std::fs::write(&ocp, PROJECT).unwrap();

    let output = run(&["export", path(&ocp), "--format", "midi"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--output"),
        "missing output must be explained: {}",
        stderr(&output)
    );
}

#[test]
fn export_musicxml_fails_at_argument_parsing() {
    let dir = temp_dir("musicxml");
    let ocp = dir.join("song.ocp");
    std::fs::write(&ocp, PROJECT).unwrap();

    let output = run(&[
        "export",
        path(&ocp),
        "--format",
        "musicxml",
        "-o",
        path(&dir.join("song.xml")),
    ]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("invalid value 'musicxml'"),
        "clap must reject the removed format: {message}"
    );
    assert!(
        !message.contains("not implemented"),
        "no runtime bail for an advertised format: {message}"
    );
}

#[test]
fn export_help_lists_only_implemented_formats() {
    let output = run(&["export", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("json"), "{help}");
    assert!(help.contains("midi"), "{help}");
    assert!(!help.contains("musicxml"), "{help}");
}

#[test]
fn serve_is_not_advertised_as_a_command() {
    let output = run(&["serve"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("unrecognized subcommand") && message.contains("serve"),
        "removed server must fail as an unknown command: {message}"
    );

    let help = run(&["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(!help.contains("Serve"), "{help}");
}
