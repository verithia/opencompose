//! `@kit` pads: a hit name addresses a pad, and a pad that cannot sound says so.

#[cfg(feature = "plugin-host")]
use std::path::{Path, PathBuf};

#[cfg(feature = "plugin-host")]
fn soundfont() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

const KIT: &str = r#"@ocp "0.2.0"

@project "kit pads"
  tempo: 120
  time_signature: 4/4

@kit letters
  pad a | note: C2
  pad b | note: D2
  pad c | note: E2
  pad d | note: F2
  pad e | note: G2

@track drums | instrument: drums/drumkit, preset: letters
  @pattern beat [1:1 - 2:1]
    a 1:0 f
    b 1:1 f
    c 1:2 f
    d 1:3 f
    e 2:0 f
"#;

fn note_ons(project: &oc_dsl::ast::Project) -> Vec<u8> {
    let settings = oc_engine::interpret::InterpretSettings {
        sample_rate: 44_100,
        ..Default::default()
    };
    let events = oc_engine::interpret::try_interpret(project, &settings).expect("the kit plays");
    events
        .iter()
        .filter_map(|event| match event.event {
            oc_engine::interpret::MidiMessage::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect()
}

#[test]
fn a_kit_pad_names_the_note_its_hits_play() {
    let project = oc_dsl::parse(KIT).expect("the letter kit parses");
    oc_dsl::validate(&project).expect("the letter kit validates");
    let notes = note_ons(&project);
    // Each hit plays its own pad; the General MIDI default would send every
    // one of them to note 38 (snare).
    assert_eq!(notes, vec![36, 38, 40, 41, 43]);
    assert!(!notes.iter().all(|note| *note == 38));
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_pad_without_a_layer_or_voice_warns_that_its_hits_are_silent() {
    // A pad the kit cannot sound: no sample layer, and 'crash' names no
    // synthesised voice.
    let source = KIT.replace(
        "@kit letters\n  pad a | note: C2",
        "@kit letters\n  pad crash | note: C#3\n  pad a | note: C2",
    );
    let project = oc_dsl::parse(&source).expect("the crash kit parses");
    oc_dsl::validate(&project).expect("the crash kit validates");
    let _warnings = crate::RENDER_WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    oc_engine::render_warnings::clear();
    let audio = oc_engine::render::render_to_buffer(&project, &soundfont(), 22_050)
        .expect("the kit renders");
    assert!(audio.iter().any(|sample| *sample != 0.0), "the kit sounds");
    let warnings = oc_engine::render_warnings::take();
    let warning = warnings
        .iter()
        .find(|warning| warning.contains("crash"))
        .unwrap_or_else(|| panic!("no warning named the crash pad: {warnings:?}"));
    assert!(
        warning.contains("49"),
        "the warning names the note the pad answers: {warning}"
    );
    assert!(
        warning.contains("hat_closed"),
        "the warning names the voices a pad can use: {warning}"
    );
}

/// A kit with no `crash` pad, so every crash hit falls back to General MIDI
/// note 49 and is silent.
#[cfg(feature = "plugin-host")]
const SILENT_CRASHES: &str = r#"@ocp "0.2.0"

@project "silent hits"
  tempo: 120
  time_signature: 4/4

@kit letters
  pad a | note: C2
  pad b | note: D2

@track drums | instrument: drums/drumkit, preset: letters
  @pattern beat [1:1 - 2:1] x25
    crash 1:0 mf
    crash 2:0 mf
    crash 3:0 mf
    crash 4:0 mf
"#;

#[test]
#[cfg(feature = "plugin-host")]
fn a_hundred_silent_hits_report_one_warning_with_the_count() {
    let project = oc_dsl::parse(SILENT_CRASHES).expect("the silent-hit kit parses");
    oc_dsl::validate(&project).expect("the silent-hit kit validates");
    let _warnings = crate::RENDER_WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    oc_engine::render_warnings::clear();
    oc_engine::render::render_to_buffer(&project, &soundfont(), 22_050).expect("the kit renders");

    let warnings = oc_engine::render_warnings::take();
    let crash: Vec<_> = warnings
        .iter()
        .filter(|warning| warning.contains("drum hit 'crash'"))
        .collect();
    assert_eq!(
        crash.len(),
        1,
        "one line per (track, pad), not one per hit: {warnings:?}"
    );
    let line = crash[0];
    assert!(
        line.contains("track 'drums'"),
        "the line names the track: {line}"
    );
    assert!(
        line.contains("(100 occurrences)"),
        "the line carries the hit count: {line}"
    );
    assert!(
        line.contains("no pad of this kit has"),
        "the line keeps the reason: {line}"
    );
}
