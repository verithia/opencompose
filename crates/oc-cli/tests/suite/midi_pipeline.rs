//! CLI surface for the application MIDI pipeline.

use std::path::{Path, PathBuf};

use midly::num::{u7, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-midi", name)
}

fn fixture() -> Vec<u8> {
    let events = vec![
        (0, TrackEventKind::Meta(MetaMessage::TrackName(b"CLI"))),
        (
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
        ),
        (
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
        ),
        (
            0,
            TrackEventKind::Midi {
                channel: 0.into(),
                message: MidiMessage::ProgramChange {
                    program: u7::new(0),
                },
            },
        ),
        (
            0,
            TrackEventKind::Midi {
                channel: 0.into(),
                message: MidiMessage::NoteOn {
                    key: u7::new(60),
                    vel: u7::new(100),
                },
            },
        ),
        (
            480,
            TrackEventKind::Midi {
                channel: 0.into(),
                message: MidiMessage::NoteOff {
                    key: u7::new(60),
                    vel: u7::new(0),
                },
            },
        ),
        (480, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];
    let mut previous = 0_u64;
    let events = events
        .into_iter()
        .map(|(tick, kind)| {
            let delta = u28::try_from((tick - previous) as u32).unwrap();
            previous = tick;
            TrackEvent { delta, kind }
        })
        .collect();
    let smf = Smf {
        header: Header::new(Format::SingleTrack, Timing::Metrical(480.into())),
        tracks: vec![events],
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes).unwrap();
    bytes
}

fn run(args: &[&str]) -> std::process::Output {
    crate::ocp().args(args).output().unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn cli_midi_import_export_and_legacy_converter_still_work() {
    let dir = temp_dir("pipeline");
    let midi = dir.join("input.mid");
    let ocp = dir.join("project.ocp");
    let exported = dir.join("exported.mid");
    let legacy = dir.join("legacy.ocp");
    std::fs::write(&midi, fixture()).unwrap();

    let imported = run(&[
        "midi",
        "import",
        path(&midi),
        "--out",
        path(&ocp),
        "--json-summary",
    ]);
    assert!(
        imported.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(summary["target_ppq"], 480);
    assert_eq!(summary["tracks"][0]["note_count"], 1);
    assert!(std::fs::read_to_string(&ocp).unwrap().contains("@track"));

    let exported_result = run(&[
        "midi",
        "export",
        path(&ocp),
        "--out",
        path(&exported),
        "--json-report",
    ]);
    assert!(
        exported_result.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&exported_result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&exported_result.stdout).unwrap();
    assert_eq!(report["written"][0]["notes"], 1);
    assert_eq!(report["format"], 1);
    let bytes = std::fs::read(&exported).unwrap();
    let smf = Smf::parse(&bytes).unwrap();
    assert_eq!(smf.header.format, Format::Parallel);
    assert_eq!(smf.tracks.len(), 2);

    let legacy_result = run(&["midi2ocp", path(&midi), "--output", path(&legacy)]);
    assert!(
        legacy_result.status.success(),
        "midi2ocp failed: {}",
        String::from_utf8_lossy(&legacy_result.stderr)
    );
    assert!(!std::fs::read_to_string(&legacy).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
