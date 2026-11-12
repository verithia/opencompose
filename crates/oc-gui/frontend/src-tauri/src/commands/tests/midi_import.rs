//! MIDI import: the append position, the revision guard and the single undo
//! entry an import writes.

use midly::num::{u4, u7, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use oc_dsl::ast;
use oc_engine::midi::{ImportOptions, TrackMapping, TrackTarget, import_smf};

use crate::commands::tests::support::{make_track, test_state};
use crate::midi_commands::commit_import_inner;
use crate::state::AppStateInner;

fn channel(value: u8) -> u4 {
    u4::try_from(value).unwrap()
}

fn value(value: u8) -> u7 {
    u7::try_from(value).unwrap()
}

/// One bar of MIDI: a whole note on G4.
fn one_bar_file() -> Vec<u8> {
    let events = vec![
        TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Midi {
                channel: channel(0),
                message: MidiMessage::NoteOn {
                    key: value(67),
                    vel: value(100),
                },
            },
        },
        TrackEvent {
            delta: u28::try_from(1_920).unwrap(),
            kind: TrackEventKind::Midi {
                channel: channel(0),
                message: MidiMessage::NoteOff {
                    key: value(67),
                    vel: value(0),
                },
            },
        },
        TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ];
    let smf = Smf {
        header: Header::new(Format::SingleTrack, Timing::Metrical(480.into())),
        tracks: vec![events],
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes).unwrap();
    bytes
}

/// A document whose single track already holds a four-bar clip.
fn inner_with_a_four_bar_clip() -> AppStateInner {
    let mut inner = test_state();
    let mut track = make_track("Piano");
    track.push_pattern_clip(ast::Clip {
        pattern: "hook".into(),
        time_range: ast::TimeRange {
            start: ast::BarBeat::new(1, 1, 0),
            end: ast::BarBeat::new(5, 1, 0),
        },
        offset_quarters: 0.0,
        loop_quarters: 0.0,
        id: None,
    });
    inner.project.tracks.push(track);
    inner.project.patterns.push(ast::PatternDef {
        name: "hook".into(),
        instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
        preset: None,
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: Vec::new(),
        events: Vec::new(),
        pedals: Vec::new(),
        hairpins: Vec::new(),
        hits: Vec::new(),
        color: None,
    });
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner
}

fn append_options() -> ImportOptions {
    ImportOptions {
        mappings: vec![TrackMapping {
            source_track: 0,
            channel: None,
            target: TrackTarget::ExistingTrack(0),
            name: None,
        }],
        ..ImportOptions::default()
    }
}

#[test]
fn an_append_lands_after_the_track_content_and_undoes_as_one_entry() {
    let mut inner = inner_with_a_four_bar_clip();
    let before = inner.project.clone();
    let revision = inner.revision;
    let result = import_smf(
        &one_bar_file(),
        Some("take".into()),
        &append_options(),
        Some(&inner.project),
    )
    .unwrap();

    commit_import_inner(&mut inner, result, revision).expect("the append commits");

    let track = &inner.project.tracks[0];
    assert_eq!(track.pattern_clips().count(), 1, "the existing clip stays");
    let phrase = track.phrases.first().expect("one appended phrase");
    let range = phrase.time_range.expect("the appended phrase has a range");
    assert_eq!(range.start, ast::BarBeat::new(5, 1, 0));
    assert_eq!(
        oc_dsl::timing::ProjectClock::new(&inner.project.properties)
            .unwrap()
            .quarter_notes_at(&range.start)
            .unwrap()
            + phrase.events[0].start_beat,
        16.0,
        "the new note starts at bar 5"
    );

    assert_eq!(inner.undo.undo.len(), 1, "an import is one undo entry");
    assert_eq!(inner.undo.undo[0].label, "MIDI import");
    inner
        .undo
        .undo(&mut inner.project)
        .expect("undo restores the pre-import document");
    assert_eq!(inner.project, before);
}

#[test]
fn a_new_track_import_keeps_its_placement_and_one_undo_entry() {
    let mut inner = inner_with_a_four_bar_clip();
    let before = inner.project.clone();
    let revision = inner.revision;
    let result = import_smf(
        &one_bar_file(),
        Some("take".into()),
        &ImportOptions::default(),
        Some(&inner.project),
    )
    .unwrap();

    assert_eq!(result.project.tracks.len(), 2, "a new track is created");
    let added = &result.project.tracks[1];
    assert_eq!(
        added.phrases[0].time_range.expect("a phrase").start,
        ast::BarBeat::new(1, 1, 0),
        "a normal import still starts at 1:1"
    );

    commit_import_inner(&mut inner, result, revision).expect("the import commits");
    assert_eq!(inner.undo.undo.len(), 1, "an import is one undo entry");
    inner.undo.undo(&mut inner.project).expect("undo applies");
    assert_eq!(inner.project, before);
}

#[test]
fn an_edit_during_the_parse_refuses_the_import_and_survives() {
    let mut inner = inner_with_a_four_bar_clip();
    let revision = inner.revision;
    let result = import_smf(
        &one_bar_file(),
        Some("take".into()),
        &append_options(),
        Some(&inner.project),
    )
    .unwrap();

    // An agent edit lands while the file is parsed.
    inner.project.tracks[0].name = "Renamed".into();
    inner.bump_revision();

    let error = commit_import_inner(&mut inner, result, revision).unwrap_err();
    assert!(error.contains("project moved"), "{error}");
    assert!(error.contains(&format!("revision {revision}")), "{error}");
    assert_eq!(inner.project.tracks[0].name, "Renamed", "the edit survives");
    assert!(inner.undo.undo.is_empty(), "nothing was applied");
}
