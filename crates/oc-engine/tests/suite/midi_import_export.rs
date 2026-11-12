//! Application MIDI pipeline tests. Fixtures are generated in code.

use std::time::Instant;

use midly::num::{u4, u7, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage, PitchBend, Smf, Timing, TrackEvent, TrackEventKind,
};
use oc_dsl::ast::*;
use oc_dsl::timing::ProjectClock;
use oc_engine::midi::{
    ChannelAssignment, ExportOptions, ExportReport, ExportScope, ImportOptions, LossKind,
    MidiError, TrackMapping, TrackTarget, export_smf, import_smf,
};

// ============================================================================
// Fixture builders
// ============================================================================

fn channel(value: u8) -> u4 {
    u4::try_from(value).unwrap()
}

fn value(value: u8) -> u7 {
    u7::try_from(value).unwrap()
}

fn smf_bytes(
    format: Format,
    ppq: u16,
    tracks: Vec<Vec<(u64, TrackEventKind<'static>)>>,
) -> Vec<u8> {
    let tracks: Vec<Vec<TrackEvent<'static>>> = tracks
        .into_iter()
        .map(|mut events| {
            events.sort_by_key(|(tick, _)| *tick);
            let mut previous = 0_u64;
            events
                .into_iter()
                .map(|(tick, kind)| {
                    let delta = u28::try_from((tick - previous) as u32).unwrap();
                    previous = tick;
                    TrackEvent { delta, kind }
                })
                .collect()
        })
        .collect();
    let smf = Smf {
        header: Header::new(format, Timing::Metrical(ppq.into())),
        tracks,
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes).unwrap();
    bytes
}

fn track_name(name: &'static [u8]) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::TrackName(name))
}

fn tempo(micros: u32) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::Tempo(u24::try_from(micros).unwrap()))
}

fn time_signature(numerator: u8, power: u8) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, power, 24, 8))
}

fn key_signature(sharps_flats: i8, minor: bool) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::KeySignature(sharps_flats, minor))
}

fn end_of_track() -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::EndOfTrack)
}

fn note_on(channel: u8, key: u8, velocity: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::NoteOn {
            key: value(key),
            vel: value(velocity),
        },
    }
}

fn note_off(channel: u8, key: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::NoteOff {
            key: value(key),
            vel: 0.into(),
        },
    }
}

fn program(channel: u8, program: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::ProgramChange {
            program: value(program),
        },
    }
}

fn cc(channel: u8, controller: u8, data: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::Controller {
            controller: value(controller),
            value: value(data),
        },
    }
}

fn bend(channel: u8, bend: i16) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::PitchBend {
            bend: PitchBend::from_int(bend),
        },
    }
}

fn channel_pressure(channel: u8, pressure: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: self::channel(channel),
        message: MidiMessage::ChannelAftertouch {
            vel: value(pressure),
        },
    }
}

/// Two pitched tracks, tempo/meter/key maps, CC, bend, sustain and pressure.
fn standard_fixture() -> Vec<u8> {
    smf_bytes(
        Format::Parallel,
        480,
        vec![
            vec![
                (0, track_name(b"Conductor")),
                (0, tempo(500_000)),
                (0, time_signature(4, 2)),
                (0, key_signature(0, false)),
                (0, TrackEventKind::Meta(MetaMessage::Text(b"ignored"))),
                (1_920, tempo(666_667)),
                (3_840, time_signature(3, 2)),
                (6_000, end_of_track()),
            ],
            vec![
                (0, track_name(b"Piano")),
                (0, program(0, 0)),
                (0, note_on(0, 60, 100)),
                (0, cc(0, 64, 127)),
                (0, cc(0, 1, 0)),
                (0, bend(0, 0)),
                (120, cc(0, 74, 100)),
                (240, cc(0, 1, 64)),
                (240, bend(0, 8191)),
                (480, note_off(0, 60)),
                (480, note_on(0, 64, 90)),
                (960, note_off(0, 64)),
                (960, cc(0, 64, 0)),
                (1_920, end_of_track()),
            ],
            vec![
                (0, track_name(b"Strings")),
                (0, program(2, 48)),
                (0, channel_pressure(2, 30)),
                (240, note_on(2, 67, 100)),
                (240, channel_pressure(2, 80)),
                (480, note_off(2, 67)),
                (960, TrackEventKind::SysEx(&[0x7D])),
                (1_440, end_of_track()),
            ],
        ],
    )
}

// ============================================================================
// Assertion helpers
// ============================================================================

fn notes(track: &Track) -> Vec<(f64, f64, u8, u8)> {
    let mut notes = Vec::new();
    for phrase in &track.phrases {
        for note in &phrase.events {
            let gate = performance_gate_quarters(&note.modifiers)
                .unwrap()
                .unwrap_or_else(|| note.duration.to_beats());
            notes.push((
                note.start_beat,
                gate,
                note.pitch.to_midi(),
                note.velocity.unwrap_or(0),
            ));
        }
    }
    notes.sort_by(|left, right| left.0.total_cmp(&right.0));
    notes
}

fn lane<'a>(track: &'a Track, param: &str) -> &'a AutomationLane {
    track
        .automation()
        .find(|lane| lane.param == param)
        .unwrap_or_else(|| panic!("missing lane {param}"))
}

fn lane_ticks(clock: &ProjectClock, lane: &AutomationLane) -> Vec<(u64, f64)> {
    lane.points
        .iter()
        .map(|point| {
            let quarter = if lane.ppq == 0 {
                clock.quarter_notes_at(&point.position).unwrap()
            } else {
                clock
                    .quarter_notes_at_ppq(&point.position, lane.ppq)
                    .unwrap()
            };
            (
                (quarter * 480.0).round() as u64,
                point.value.as_f64().unwrap_or(0.0),
            )
        })
        .collect()
}

fn import(data: &[u8]) -> oc_engine::midi::ImportResult {
    import_smf(
        data,
        Some("fixture".to_string()),
        &ImportOptions::default(),
        None,
    )
    .unwrap()
}

// ============================================================================
// Import
// ============================================================================

fn first_exported_program(project: &Project) -> u8 {
    let result = export_smf(project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    smf.tracks
        .iter()
        .flatten()
        .find_map(|event| match event.kind {
            TrackEventKind::Midi {
                message: MidiMessage::ProgramChange { program },
                ..
            } => Some(program.as_int()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn all_exact_imported_gm_programs_survive_broad_instrument_suggestions() {
    for id in 0..=127 {
        let bytes = smf_bytes(
            Format::SingleTrack,
            480,
            vec![vec![
                (0, program(0, id)),
                (0, note_on(0, 60, 100)),
                (480, note_off(0, 60)),
                (480, end_of_track()),
            ]],
        );
        let result = import(&bytes);
        let track = &result.project.tracks[0];
        assert_eq!(result.summary.tracks[0].program, Some(id));
        assert!(
            track
                .engine_params
                .iter()
                .flatten()
                .any(|(key, value)| key == "midi_program" && value.as_f64() == Some(f64::from(id)))
        );
        assert_eq!(first_exported_program(&result.project), id, "program {id}");
    }
}

#[test]
fn gm_path_export_matches_existing_playback_and_explicit_program_precedence() {
    for (path, expected) in [
        ("guitar/acoustic_steel", 25),
        ("bass/electric_finger", 33),
        ("guitar/electric_jazz", 26),
        ("strings/violin_section", 48),
    ] {
        let mut project = oc_dsl::parse(&format!("@project\n  tempo: 120\n@track part | instrument: {path}\n  @phrase p [1:1 - 2:1]\n    C4 q mf\n")).unwrap();
        assert_eq!(
            oc_engine::interpret::instrument_to_gm_program(&project.tracks[0].instrument),
            expected
        );
        assert_eq!(first_exported_program(&project), expected);
        project.tracks[0]
            .engine_params
            .get_or_insert_with(Vec::new)
            .push(("midi_program".into(), ParamValue::Number(127.0)));
        assert_eq!(first_exported_program(&project), 127);
    }
}

#[test]
fn low_midi_octave_survives_import_serialization_and_export() {
    let bytes = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, note_on(0, 0, 100)),
            (480, note_off(0, 0)),
            (480, note_on(0, 11, 100)),
            (960, note_off(0, 11)),
            (960, note_on(0, 127, 100)),
            (1440, note_off(0, 127)),
            (1440, end_of_track()),
        ]],
    );
    let result = import(&bytes);
    let project = oc_dsl::parse(&oc_dsl::serialize(&result.project)).unwrap();
    oc_dsl::validate(&project).unwrap();
    assert_eq!(exported_keys(&project), [0, 11, 127]);
    let mut invalid = project;
    invalid.tracks[0].phrases[0].events[0].pitch = Pitch::from_text("Cb-1").unwrap();
    let error = export_smf(&invalid, &ExportOptions::default()).unwrap_err();
    assert!(matches!(error, MidiError::InvalidExport { .. }));
}

#[test]
fn imports_tracks_clock_controllers_bend_and_sustain() {
    let result = import(&standard_fixture());
    oc_dsl::validate(&result.project).unwrap();

    assert_eq!(result.project.name.as_deref(), Some("fixture"));
    assert_eq!(result.project.tracks.len(), 2);
    assert_eq!(result.summary.format, 1);
    assert_eq!(result.summary.source_ppq, 480);
    assert_eq!(result.summary.max_tick_error, 0.0);
    assert_eq!(result.summary.ticks_shifted, 0);
    assert_eq!(result.summary.tempo_changes, 2);
    assert_eq!(result.summary.meter_changes, 2);
    assert_eq!(result.summary.key_changes, 1);
    assert_eq!(result.summary.sustain_expanded_notes, 1);
    assert_eq!(result.summary.overlapping_notes, 0);
    assert_eq!(result.summary.zero_length_notes, 0);
    assert_eq!(result.summary.tracks.len(), 3);

    let piano = &result.project.tracks[0];
    assert_eq!(piano.name, "Piano");
    assert_eq!(piano.instrument.as_path(), "piano/acoustic_grand");
    assert_eq!(notes(piano), vec![(0.0, 2.0, 60, 100), (1.0, 1.0, 64, 90)]);
    assert_eq!(lane(piano, "sustain").points.len(), 2);
    assert_eq!(lane(piano, "mod").points.len(), 2);
    assert_eq!(lane(piano, "brightness").points.len(), 1);
    assert_eq!(lane(piano, "pitch_bend").points.len(), 2);
    assert_eq!(lane(piano, "sustain").ppq, 480);
    let clock = ProjectClock::new(&result.project.properties).unwrap();
    let bends = lane_ticks(&clock, lane(piano, "pitch_bend"));
    assert_eq!(bends[0].0, 0);
    assert!((bends[1].1 - 2.0).abs() < 0.001);
    assert_eq!(
        lane(piano, "mod")
            .points
            .iter()
            .map(|point| point.value.as_f64().unwrap())
            .collect::<Vec<_>>(),
        vec![0.0, 64.0 / 127.0 * 100.0]
    );

    let strings = &result.project.tracks[1];
    assert_eq!(strings.name, "Strings");
    assert_eq!(notes(strings), vec![(0.5, 0.5, 67, 100)]);
    assert_eq!(lane(strings, "pressure").points.len(), 2);

    let summary_piano = result
        .summary
        .tracks
        .iter()
        .find(|track| track.source_track == 1)
        .unwrap();
    assert_eq!(summary_piano.channel, Some(1));
    assert_eq!(summary_piano.program, Some(0));
    assert_eq!(
        summary_piano.program_name.as_deref(),
        Some("Acoustic Grand Piano")
    );
    assert_eq!(summary_piano.first_bar, Some(1));
    assert_eq!(summary_piano.last_bar, Some(1));
    assert_eq!(
        summary_piano.lanes,
        vec!["brightness", "mod", "pitch_bend", "sustain"]
    );
    let summary_strings = result
        .summary
        .tracks
        .iter()
        .find(|track| track.source_track == 2)
        .unwrap();
    assert_eq!(summary_strings.channel, Some(3));
    assert_eq!(summary_strings.program, Some(48));
    assert_eq!(summary_strings.lanes, vec!["pressure"]);

    let conductor = &result.summary.tracks[0];
    assert!(conductor.dropped);
    assert_eq!(conductor.target_track, None);
    assert!(
        conductor
            .ignored
            .iter()
            .any(|event| event.kind == "text" && event.count == 1)
    );
    let strings_ignored = &result
        .summary
        .tracks
        .iter()
        .find(|track| track.source_track == 2)
        .unwrap()
        .ignored;
    assert!(
        strings_ignored
            .iter()
            .any(|event| event.kind == "sysex" && event.count == 1)
    );

    assert_eq!(result.project.properties.tempo, 120.0);
    assert_eq!(result.project.properties.time_signature.numerator, 4);
    assert_eq!(result.project.properties.tempo_map.len(), 1);
    let tempo_point = result.project.properties.tempo_map[0];
    assert_eq!(tempo_point.position, BarBeat::new(2, 1, 0));
    assert_eq!(tempo_point.offset_quarters, 0.0);
    assert!((tempo_point.bpm - 90.0).abs() < 0.001);
    assert_eq!(result.project.properties.time_sig_map.len(), 1);
    assert_eq!(
        result.project.properties.time_sig_map[0].position,
        BarBeat::new(3, 1, 0)
    );
    assert_eq!(result.project.properties.time_sig_map[0].numerator, 3);
    assert_eq!(
        result.project.properties.key,
        Some(KeySignature {
            root: NoteName::C,
            accidental: Accidental::Natural,
            mode: KeyMode::Major,
        })
    );
    assert!(result.project.properties.key_map.is_empty());
}

#[test]
fn maps_channels_to_existing_tracks_and_avoids_name_collisions() {
    let fixture = smf_bytes(
        Format::Parallel,
        480,
        vec![
            vec![(0, track_name(b"Conductor")), (1_920, end_of_track())],
            vec![
                (0, track_name(b"Piano")),
                (0, program(0, 0)),
                (0, note_on(0, 60, 100)),
                (480, note_off(0, 60)),
                (480, note_on(0, 62, 100)),
                (960, note_off(0, 62)),
                (0, program(1, 40)),
                (0, note_on(1, 67, 100)),
                (480, note_off(1, 67)),
                (1_920, end_of_track()),
            ],
            vec![(0, track_name(b"Piano")), (960, end_of_track())],
        ],
    );
    let base = oc_dsl::parse(
        "@project \"base\"\n@preset Violin\n  source: soundfont\n  program: 40\n@track Piano | instrument: piano/acoustic_grand\n",
    )
    .unwrap();

    let mut options = ImportOptions::default();
    let result = import_smf(&fixture, None, &options, Some(&base)).unwrap();
    assert_eq!(result.project.tracks.len(), 2);
    assert_eq!(result.project.tracks[0].name, "Piano");
    assert_eq!(result.project.tracks[1].name, "Piano_2");
    assert_eq!(notes(&result.project.tracks[1]).len(), 3);
    assert_eq!(result.summary.tracks.len(), 3);
    assert!(result.summary.tracks[0].dropped);
    assert_eq!(result.summary.tracks[2].target_track, None);
    assert!(result.summary.tracks[2].dropped);

    options.drop_empty = false;
    let kept = import_smf(&fixture, None, &options, Some(&base)).unwrap();
    assert_eq!(kept.project.tracks.len(), 3);
    assert_eq!(kept.project.tracks[2].name, "Piano_3");
    assert_eq!(kept.project.tracks[2].phrases.len(), 0);

    options.split_channels = true;
    options.drop_empty = true;
    options.mappings = vec![TrackMapping {
        source_track: 1,
        channel: Some(2),
        target: TrackTarget::ExistingTrack(0),
        name: None,
    }];
    let split = import_smf(&fixture, None, &options, Some(&base)).unwrap();
    assert_eq!(split.project.tracks.len(), 2);
    assert_eq!(notes(&split.project.tracks[0]).len(), 1);
    assert_eq!(split.project.tracks[0].name, "Piano");
    assert_eq!(
        split
            .project
            .tracks
            .iter()
            .find(|track| track.name == "Piano_ch1")
            .unwrap()
            .phrases
            .iter()
            .map(|phrase| phrase.events.len())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        split
            .summary
            .tracks
            .iter()
            .find(|track| track.channel == Some(2))
            .unwrap()
            .target_track,
        Some(0)
    );

    options.mappings = vec![TrackMapping {
        source_track: 1,
        channel: None,
        target: TrackTarget::NewTrack,
        name: Some("Custom Name".to_string()),
    }];
    options.split_channels = false;
    let renamed = import_smf(&fixture, None, &options, Some(&base)).unwrap();
    assert!(
        renamed
            .project
            .tracks
            .iter()
            .any(|track| track.name == "Custom_Name")
    );

    options.split_channels = true;
    options.mappings.clear();
    let presets = import_smf(&fixture, None, &options, Some(&base)).unwrap();
    let violin = presets
        .project
        .tracks
        .iter()
        .find(|track| {
            track
                .engine_params
                .iter()
                .flatten()
                .any(|(key, value)| key == "midi_program" && value.as_f64() == Some(40.0))
        })
        .unwrap();
    assert_eq!(violin.preset.as_deref(), Some("Violin"));
    assert!(
        presets
            .summary
            .tracks
            .iter()
            .any(|track| track.program == Some(40) && track.preset.as_deref() == Some("Violin"))
    );
}

/// One bar: a whole note on G4 and a modulation-wheel point at 1:1.
fn one_bar_fixture() -> Vec<u8> {
    smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, track_name(b"Melody")),
            (0, cc(0, 1, 96)),
            (0, note_on(0, 67, 100)),
            (1_920, note_off(0, 67)),
            (1_920, end_of_track()),
        ]],
    )
}

#[test]
fn an_existing_track_target_appends_after_its_last_content() {
    let base = parsed_project(
        r#"@ocp "0.2.0"
@project "Append"
  tempo: 120
  time_signature: 4/4

@pattern hook | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track Piano | instrument: piano/acoustic_grand
  clip: hook [1:1 - 5:1]
"#,
    );
    let before = base.clone();
    let options = ImportOptions {
        mappings: vec![TrackMapping {
            source_track: 0,
            channel: None,
            target: TrackTarget::ExistingTrack(0),
            name: None,
        }],
        ..ImportOptions::default()
    };
    let result = import_smf(
        &one_bar_fixture(),
        Some("take".into()),
        &options,
        Some(&base),
    )
    .unwrap();
    oc_dsl::validate(&result.project).unwrap();

    let track = &result.project.tracks[0];
    assert_eq!(track.pattern_clips().count(), 1, "the existing clip stays");
    assert_eq!(
        track.pattern_clips().next().unwrap().time_range.end,
        BarBeat::new(5, 1, 0)
    );
    assert_eq!(
        result.project.patterns[0].events.len(),
        1,
        "the written pattern is untouched"
    );

    let phrase = track.phrases.first().expect("one appended phrase");
    let range = phrase.time_range.expect("the appended phrase has a range");
    assert_eq!(range.start, BarBeat::new(5, 1, 0));
    assert_eq!(range.end, BarBeat::new(6, 1, 0));

    let clock = ProjectClock::new(&result.project.properties).unwrap();
    let offset = clock.quarter_notes_at(&range.start).unwrap();
    assert_eq!(
        offset + phrase.events[0].start_beat,
        16.0,
        "the new note starts at bar 5"
    );
    assert_eq!(phrase.events[0].pitch.to_midi(), 67);

    // Controller data keeps the file's internal timing too.
    let ticks = lane_ticks(&clock, lane(track, "mod"));
    assert_eq!(ticks, vec![(16 * 480, 96.0 / 127.0 * 100.0)]);

    assert_eq!(
        base, before,
        "the document the import planned against is unchanged"
    );
}

#[test]
fn an_existing_track_mapping_must_name_a_track_that_exists() {
    let base = parsed_project(
        r#"@ocp "0.2.0"
@project "Refuse"
@track Piano | instrument: piano/acoustic_grand
"#,
    );
    let options = ImportOptions {
        mappings: vec![TrackMapping {
            source_track: 0,
            channel: None,
            target: TrackTarget::ExistingTrack(7),
            name: None,
        }],
        ..ImportOptions::default()
    };
    let error = import_smf(&one_bar_fixture(), None, &options, Some(&base)).unwrap_err();
    match error {
        MidiError::InvalidMapping {
            source_track,
            message,
        } => {
            assert_eq!(source_track, 0);
            assert!(
                message.contains("existing track 7 does not exist"),
                "{message}"
            );
        }
        other => panic!("expected a typed mapping refusal, got {other}"),
    }
    assert!(base.tracks[0].phrases.is_empty(), "nothing was applied");
}

#[test]
fn channel_ten_splits_by_default_and_imports_as_drums() {
    let fixture = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, track_name(b"Kit")),
            (0, note_on(0, 60, 100)),
            (0, note_on(9, 36, 120)),
            (480, note_off(0, 60)),
            (480, note_off(9, 36)),
            (960, end_of_track()),
        ]],
    );
    let result = import(&fixture);
    assert_eq!(result.project.tracks.len(), 2);
    let drums = result
        .project
        .tracks
        .iter()
        .find(|track| track.instrument.category == "drums")
        .unwrap();
    assert_eq!(notes(drums), vec![(0.0, 1.0, 36, 120)]);
    let melodic = result
        .project
        .tracks
        .iter()
        .find(|track| track.instrument.category != "drums")
        .unwrap();
    assert_eq!(notes(melodic), vec![(0.0, 1.0, 60, 100)]);
}

#[test]
fn overlapping_and_zero_length_notes_follow_the_declared_policy() {
    let fixture = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, track_name(b"Policy")),
            (0, program(0, 0)),
            (0, note_on(0, 60, 100)),
            (0, note_on(0, 62, 100)),
            (0, note_off(0, 62)),
            (240, note_on(0, 60, 90)),
            (480, note_off(0, 60)),
            (960, end_of_track()),
        ]],
    );
    let result = import(&fixture);
    assert_eq!(result.summary.overlapping_notes, 1);
    assert_eq!(result.summary.zero_length_notes, 1);
    let notes = notes(&result.project.tracks[0]);
    assert_eq!(notes.len(), 3);
    assert_eq!(notes[0], (0.0, 0.5, 60, 100));
    assert_eq!(notes[1], (0.0, 1.0 / 480.0, 62, 100));
    assert_eq!(notes[2], (0.5, 0.5, 60, 90));
}

#[test]
fn type_two_sequences_import_as_simultaneous_tracks() {
    let fixture = smf_bytes(
        Format::Sequential,
        480,
        vec![
            vec![
                (0, track_name(b"First")),
                (0, note_on(0, 60, 100)),
                (480, note_off(0, 60)),
                (960, end_of_track()),
            ],
            vec![
                (0, track_name(b"Second")),
                (0, note_on(1, 64, 90)),
                (240, note_off(1, 64)),
                (960, end_of_track()),
            ],
        ],
    );
    let result = import(&fixture);
    assert_eq!(result.summary.format, 2);
    assert_eq!(result.project.tracks.len(), 2);
    assert_eq!(notes(&result.project.tracks[0]).len(), 1);
    assert_eq!(notes(&result.project.tracks[1]).len(), 1);
}

#[test]
fn non_480_ppq_reports_residual_but_exact_multiples_do_not() {
    let exact = smf_bytes(
        Format::SingleTrack,
        96,
        vec![vec![
            (0, track_name(b"Exact")),
            (0, program(0, 0)),
            (0, note_on(0, 60, 100)),
            (96, note_off(0, 60)),
            (960, end_of_track()),
        ]],
    );
    let exact_result = import(&exact);
    assert_eq!(exact_result.summary.max_tick_error, 0.0);
    assert_eq!(exact_result.summary.ticks_shifted, 0);
    assert_eq!(
        notes(&exact_result.project.tracks[0]),
        vec![(0.0, 1.0, 60, 100)]
    );

    let residual = smf_bytes(
        Format::SingleTrack,
        1000,
        vec![vec![
            (0, track_name(b"Residual")),
            (0, program(0, 0)),
            (0, note_on(0, 60, 100)),
            (1, note_on(0, 62, 100)),
            (2, note_off(0, 60)),
            (3, note_off(0, 62)),
            (960, end_of_track()),
        ]],
    );
    let residual_result = import(&residual);
    assert!(residual_result.summary.max_tick_error > 0.25);
    assert!(residual_result.summary.ticks_shifted > 0);
    assert_eq!(residual_result.summary.source_ppq, 1000);
    assert_eq!(residual_result.summary.target_ppq, 480);
}

// ============================================================================
// Export
// ============================================================================

fn export_project() -> Project {
    oc_dsl::parse(
        r#"@project "Export"
  tempo: 120
  time_signature: 4/4
  key: C major
  tempo_map:
    3:1 = 90 bpm
  time_sig_map:
    5:1 = 3/4
  key_map:
    5:1 = G major

@preset warm
  source: clap
  plugin: "com.example.warm"
  patch: "Pad"

@bus reverb
  gain_automation: linear [1:1 - 2:1] {1:1=1, 2:1=0.5}

@master
  gain_automation: step [1:1 - 2:1] {1:1=0.5}

@track piano | instrument: piano/acoustic_grand, preset: warm
  mix: send(reverb=50%)
  @phrase main [1:1:0 - 5:1:0]
    C4 q mf
    E4 q mf articulation=staccato
  automation: pan [ppq=480] {1:1:0=0%, 1:1:120=100%}
  automation: expression {1:1:0=50%, 2:1:0=100%}
  automation: filter_cutoff {1:1:0=500}

@track drums | instrument: drums/acoustic_kit
  mix: mute=true
  @pattern main [1:1 - 3:1]
    kick 1:0 mf
    snare 1:2 mf
"#,
    )
    .unwrap()
}

fn absolute_events<'a>(track: &'a [TrackEvent<'a>]) -> Vec<(u64, TrackEventKind<'a>)> {
    let mut tick = 0_u64;
    let mut events = Vec::new();
    for event in track {
        tick += u64::from(event.delta.as_int());
        events.push((tick, event.kind));
    }
    events
}

fn controller_values(events: &[(u64, TrackEventKind<'_>)], controller: u8) -> Vec<(u64, u8)> {
    events
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Midi {
                message:
                    MidiMessage::Controller {
                        controller: found,
                        value,
                    },
                ..
            } if found.as_int() == controller => Some((*tick, value.as_int())),
            _ => None,
        })
        .collect()
}

#[test]
fn midi_export_preserves_an_authored_drum_kit_program() {
    let mut project = export_project();
    project.tracks[1]
        .engine_params
        .get_or_insert_with(Vec::new)
        .push(("midi_program".into(), ParamValue::Number(25.0)));
    let result = export_smf(&project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let programs = absolute_events(&smf.tracks[2])
        .into_iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Midi {
                channel,
                message: MidiMessage::ProgramChange { program },
            } => Some((tick, channel.as_int(), program.as_int())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(programs, vec![(0, 9, 25)]);
    assert_eq!(result.report.written[1].program, Some(25));
}

#[test]
fn exports_conductor_channels_cc_program_and_losses() {
    let project = export_project();
    let options = ExportOptions::default();
    let result = export_smf(&project, &options).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    assert_eq!(smf.header.format, Format::Parallel);
    assert_eq!(smf.header.timing, Timing::Metrical(480.into()));
    assert_eq!(smf.tracks.len(), 3);
    assert_eq!(result.report.written.len(), 2);

    let conductor = absolute_events(&smf.tracks[0]);
    let tempos = conductor
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Meta(MetaMessage::Tempo(micros)) => Some((*tick, micros.as_int())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tempos, vec![(0, 500_000), (3_840, 666_667)]);
    let meters = conductor
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, power, _, _)) => {
                Some((*tick, *numerator, 1_u8 << power))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(meters, vec![(0, 4, 4), (7_680, 3, 4)]);
    let keys = conductor
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Meta(MetaMessage::KeySignature(sharps_flats, minor)) => {
                Some((*tick, *sharps_flats, *minor))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(keys, vec![(0, 0, false), (7_680, 1, false)]);

    let piano = absolute_events(&smf.tracks[1]);
    assert!(piano.iter().any(|(_, kind)| matches!(
        kind,
        TrackEventKind::Meta(MetaMessage::TrackName(name)) if **name == b"piano"[..]
    )));
    let piano_channel = piano
        .iter()
        .find_map(|(_, kind)| match kind {
            TrackEventKind::Midi { channel, .. } => Some(channel.as_int()),
            _ => None,
        })
        .unwrap();
    assert_eq!(piano_channel, 0);
    assert!(piano.iter().any(|(tick, kind)| *tick == 0
        && matches!(
            kind,
            TrackEventKind::Midi {
                message: MidiMessage::ProgramChange { program },
                ..
            } if program.as_int() == 0
        )));
    assert_eq!(controller_values(&piano, 10), vec![(0, 0), (120, 127)]);
    assert_eq!(controller_values(&piano, 11), vec![(0, 64), (1_920, 127)]);
    assert_eq!(controller_values(&piano, 74), Vec::new());

    let drums = absolute_events(&smf.tracks[2]);
    let drum_channel = drums
        .iter()
        .find_map(|(_, kind)| match kind {
            TrackEventKind::Midi { channel, .. } => Some(channel.as_int()),
            _ => None,
        })
        .unwrap();
    assert_eq!(drum_channel, 9);
    assert!(!drums.iter().any(|(_, kind)| matches!(
        kind,
        TrackEventKind::Midi {
            message: MidiMessage::ProgramChange { .. },
            ..
        }
    )));

    let report = &result.report;
    assert_eq!(report.written[0].channel, 1);
    assert_eq!(report.written[0].notes, 2);
    assert_eq!(report.written[0].automation_events, 4);
    assert_eq!(report.written[0].program, Some(0));
    assert_eq!(report.written[1].channel, 10);
    assert_eq!(report.written[1].notes, 2);
    assert_eq!(report.written[1].program, None);
    assert_eq!(report.written[0].smf_track, 1);
    assert_eq!(report.written[1].smf_track, 2);

    let loss = |kind: LossKind| {
        report
            .losses
            .iter()
            .find(|loss| loss.kind == kind)
            .map(|loss| loss.count)
            .unwrap_or(0)
    };
    assert_eq!(loss(LossKind::Articulation), 1);
    assert_eq!(loss(LossKind::Automation), 1);
    assert_eq!(loss(LossKind::SendBus), 2);
    assert_eq!(loss(LossKind::ClipEnvelope), 2);
    assert_eq!(loss(LossKind::MuteSolo), 1);
    assert_eq!(loss(LossKind::PluginParameter), 1);
    assert_eq!(loss(LossKind::ProgramChange), 1);

    let again = export_smf(&project, &options).unwrap();
    assert_eq!(result.bytes, again.bytes);
}

fn exported_keys(project: &Project) -> Vec<u8> {
    let result = export_smf(project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    // Track 0 is the conductor; the fixture has one written track.
    absolute_events(&smf.tracks[1])
        .iter()
        .filter_map(|(_, kind)| match kind {
            TrackEventKind::Midi {
                message: MidiMessage::NoteOn { key, .. },
                ..
            } => Some(key.as_int()),
            _ => None,
        })
        .collect()
}

fn exported_velocities(project: &Project) -> Vec<u8> {
    let result = export_smf(project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    absolute_events(&smf.tracks[1])
        .iter()
        .filter_map(|(_, kind)| match kind {
            TrackEventKind::Midi {
                message: MidiMessage::NoteOn { vel, .. },
                ..
            } => Some(vel.as_int()),
            _ => None,
        })
        .collect()
}

#[test]
fn exported_drum_hits_carry_their_exact_velocity() {
    let text = |velocity: &str| {
        format!(
            r#"@ocp "0.2.0"
@project "Hit velocity"
  tempo: 120
@preset kit
  source: soundfont
@pattern beat | instrument: drums/acoustic_kit, preset: kit
  hits:
    kick 1:0 mp{velocity}
@track drums | instrument: drums/acoustic_kit, preset: kit
  clip: beat [1:1 - 2:1]
"#
        )
    };
    let written = oc_dsl::parse(&text("")).unwrap();
    let exact = oc_dsl::parse(&text(" v=120")).unwrap();
    assert_eq!(exported_velocities(&written), vec![64], "mp is 64");
    assert_eq!(
        exported_velocities(&exact),
        vec![120],
        "the exact velocity overrides the written dynamic"
    );
}

#[test]
fn exported_pattern_notes_carry_the_pattern_transposition() {
    let text = |transpose: &str| {
        format!(
            r#"@ocp "0.2.0"
@project "Transpose"
  tempo: 120
@preset keys
  source: soundfont
@pattern riff | instrument: piano/acoustic_grand, preset: keys{transpose}
  events:
    C4 q mf
@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: riff [1:1 - 2:1]
"#
        )
    };
    let written = oc_dsl::parse(&text("")).unwrap();
    let transposed = oc_dsl::parse(&text(", transpose: -2")).unwrap();
    assert_eq!(exported_keys(&written), vec![60], "C4 is written as 60");
    assert_eq!(
        exported_keys(&transposed),
        vec![58],
        "the export carries the transposition, not the written number"
    );
}

#[test]
fn exported_drum_hits_ignore_the_pattern_transposition() {
    let text = |transpose: &str| {
        format!(
            r#"@ocp "0.2.0"
@project "Hits"
  tempo: 120
@preset kit
  source: soundfont
@pattern beat | instrument: drums/acoustic_kit, preset: kit{transpose}
  hits:
    kick 1:0 mf
@track drums | instrument: drums/acoustic_kit, preset: kit
  clip: beat [1:1 - 2:1]
"#
        )
    };
    let written = oc_dsl::parse(&text("")).unwrap();
    let transposed = oc_dsl::parse(&text(", transpose: -2")).unwrap();
    assert_eq!(exported_keys(&written), vec![36], "the kick stays a kick");
    assert_eq!(
        exported_keys(&transposed),
        vec![36],
        "a hit addresses a sample by name, so it is never transposed"
    );
}

#[test]
fn export_scope_and_channel_overrides_are_honoured() {
    let project = export_project();
    let options = ExportOptions {
        format: 0,
        ppq: 960,
        scope: ExportScope::Track(0),
        channels: vec![ChannelAssignment {
            track: 0,
            channel: 5,
        }],
    };
    let result = export_smf(&project, &options).unwrap();
    assert_eq!(result.report.written.len(), 1);
    assert_eq!(result.report.written[0].channel, 5);
    assert_eq!(result.report.written[0].smf_track, 0);
    let smf = Smf::parse(&result.bytes).unwrap();
    assert_eq!(smf.header.format, Format::SingleTrack);
    assert_eq!(smf.header.timing, Timing::Metrical(960.into()));
    assert_eq!(smf.tracks.len(), 1);
    let events = absolute_events(&smf.tracks[0]);
    assert!(events.iter().all(|(_, kind)| match kind {
        TrackEventKind::Midi { channel, .. } => channel.as_int() == 4,
        _ => true,
    }));
    let ticks = events.iter().map(|(tick, _)| *tick).collect::<Vec<_>>();
    assert!(ticks.contains(&960));
}

// ============================================================================
// Arrangement and clip windows
// ============================================================================

fn parsed_project(text: &str) -> Project {
    let project = oc_dsl::parse(text).expect("fixture parses");
    oc_dsl::validate(&project).expect("fixture validates");
    project
}

/// Note onsets and the final tick of the single written track's SMF.
fn export_notes(project: &Project) -> (Vec<(u64, u8)>, u64, ExportReport) {
    let result = export_smf(project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let events = absolute_events(&smf.tracks[1]);
    let onsets = events
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Midi {
                message: MidiMessage::NoteOn { key, .. },
                ..
            } => Some((*tick, key.as_int())),
            _ => None,
        })
        .collect();
    let last_tick = events.last().map(|(tick, _)| *tick).unwrap_or(0);
    (onsets, last_tick, result.report)
}

fn clip_window_loss(report: &ExportReport) -> usize {
    report
        .losses
        .iter()
        .find(|loss| loss.kind == LossKind::ClipWindow)
        .map(|loss| loss.count)
        .unwrap_or(0)
}

fn riff_project(clip: &str) -> Project {
    parsed_project(&format!(
        r#"@ocp "0.2.0"
@project "Clip window"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
{clip}
"#
    ))
}

#[test]
fn a_repeated_section_exports_every_play() {
    let project = parsed_project(
        r#"@ocp "0.2.0"
@project "Repeated arrangement"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [1:1 - 2:1]
  clip: riff [2:1 - 3:1]

@section verse [1:1 - 2:1]
  repeat: 4

@section chorus [2:1 - 3:1]
  repeat: 4
"#,
    );
    assert_eq!(
        oc_dsl::arrangement::rendered_end_quarter(&project).unwrap(),
        Some(32.0),
        "two one-bar sections repeated four times render 32 quarters"
    );
    let (onsets, last_tick, report) = export_notes(&project);
    assert_eq!(report.written[0].notes, 32, "eight passes of four notes");
    assert_eq!(onsets.len(), 32);
    assert_eq!(onsets.first().map(|(tick, _)| *tick), Some(0));
    assert_eq!(onsets.last().map(|(tick, _)| *tick), Some(31 * 480));
    assert_eq!(
        last_tick,
        32 * 480,
        "the file ends at the rendered last beat"
    );
}

#[test]
fn a_slipped_clip_exports_its_content_from_the_offset() {
    let unslipped = riff_project("  clip: riff [2:1 - 6:1]");
    let slipped = riff_project("  clip: riff [2:1 - 6:1] offset: 2q");
    let (before, _, before_report) = export_notes(&unslipped);
    let (after, _, report) = export_notes(&slipped);
    assert_eq!(
        before,
        vec![(1_920, 60), (2_400, 62), (2_880, 64), (3_360, 65)]
    );
    // Slip hides the first two content quarters, so the remaining content
    // starts at the clip head; the renderer places it the same way.
    assert_eq!(after, vec![(1_920, 64), (2_400, 65)]);
    assert_eq!(
        before[2].0 - after[0].0,
        2 * 480,
        "the content that played two quarters after the clip head now starts there"
    );
    assert_eq!(before_report.written[0].notes, 4);
    assert_eq!(report.written[0].notes, 2);
    assert_eq!(clip_window_loss(&report), 2);
    assert_eq!(clip_window_loss(&before_report), 0);
}

#[test]
fn a_looped_clip_exports_every_iteration() {
    let project = riff_project("  clip: riff [1:1 - 5:1] loop: 4q");
    let (onsets, _, report) = export_notes(&project);
    assert_eq!(report.written[0].notes, 16, "four passes of four notes");
    let ticks = onsets.iter().map(|(tick, _)| *tick).collect::<Vec<_>>();
    assert_eq!(
        ticks,
        (0..16).map(|quarter| quarter * 480).collect::<Vec<_>>()
    );
}

#[test]
fn midi_export_swing_matches_the_rendered_onsets_and_pattern_overrides() {
    let mut project = parsed_project(
        r#"@project "Swing parity"
  tempo: 120
  time_signature: 4/4
  swing: 62.5%

@pattern named | instrument: piano/acoustic_grand, swing: 75%
  events:
    @0.5 E4 q mf

@pattern local | instrument: piano/acoustic_grand, swing: 50%
  events:
    @0.5 G4 q mf

@track phrase | instrument: piano/acoustic_grand
  @phrase main [1:1 - 2:1]
    @0.5 C4 q mf

@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1 - 2:1]
    kick 1:2 mf midi_note=36 timing_offset_beats=-0.0625

@track clips | instrument: piano/acoustic_grand
  clip: named [2:1 - 3:1]

@track inline | instrument: piano/acoustic_grand
"#,
    );
    let local = project.patterns.remove(1);
    project.tracks[3].inline_patterns.push(local);
    oc_dsl::validate(&project).unwrap();

    let settings = oc_engine::interpret::InterpretSettings {
        sample_rate: 48_000,
        ..Default::default()
    };
    let mut rendered: Vec<_> = oc_engine::interpret::try_interpret(&project, &settings)
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            oc_engine::interpret::MidiMessage::NoteOn { note, .. } => {
                // At 120 BPM and 480 PPQ, one tick is exactly 50 samples.
                assert_eq!(event.time_samples % 50, 0);
                Some((event.track_index, event.time_samples / 50, note))
            }
            _ => None,
        })
        .collect();
    rendered.sort_unstable();

    let result = export_smf(&project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let exported: Vec<_> = smf.tracks[1..]
        .iter()
        .enumerate()
        .flat_map(|(track_index, track)| {
            absolute_events(track)
                .into_iter()
                .filter_map(move |(tick, kind)| match kind {
                    TrackEventKind::Midi {
                        message: MidiMessage::NoteOn { key, .. },
                        ..
                    } => Some((track_index, tick, key.as_int())),
                    _ => None,
                })
        })
        .collect();

    assert_eq!(exported, rendered);
    assert_eq!(
        exported,
        [(0, 300, 60), (1, 270, 36), (2, 2_280, 64), (3, 240, 67)]
    );
}

#[test]
fn midi_export_written_pedals_match_swing_loops_and_automation_order() {
    let project = parsed_project(
        r#"@project "Pedal parity"
  tempo: 120
  time_signature: 4/4
  swing: 75%

@pattern riff | instrument: piano/acoustic_grand
  events:
    @0 C4 w mf
    @0 pedal down
    @0.25 pedal up
    @0.5 D4 q mf
    @1 pedal down
    @1.5 pedal up
    @1.75 pedal down
    @2 pedal up

@track phrase | instrument: piano/acoustic_grand
  @phrase main [1:1 - 2:1]
    C4 q mf
    @0.5 pedal down
  automation: sustain {1:1:3=false}

@track clips | instrument: piano/acoustic_grand
  clip: riff [2:1 - 3:1] offset: 0.5q loop: 1.5q
"#,
    );
    let settings = oc_engine::interpret::InterpretSettings {
        sample_rate: 48_000,
        ..Default::default()
    };
    let mut rendered = [Vec::new(), Vec::new()];
    for event in oc_engine::interpret::try_interpret(&project, &settings).unwrap() {
        if let oc_engine::interpret::MidiMessage::ControlChange {
            controller: 64,
            value,
            ..
        } = event.event
        {
            assert_eq!(event.time_samples % 50, 0);
            rendered[event.track_index].push((event.time_samples / 50, value));
        }
    }

    let options = ExportOptions {
        channels: vec![ChannelAssignment {
            track: 0,
            channel: 7,
        }],
        ..Default::default()
    };
    let result = export_smf(&project, &options).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let phrase = absolute_events(&smf.tracks[1]);
    assert!(phrase.iter().all(|(_, kind)| match kind {
        TrackEventKind::Midi { channel, .. } => channel.as_int() == 6,
        _ => true,
    }));
    let exported = [
        controller_values(&phrase, 64),
        controller_values(&absolute_events(&smf.tracks[2]), 64),
    ];
    assert_eq!(exported, rendered);
    // The explicit track lane follows the written down at the same tick.
    assert_eq!(exported[0], [(360, 127), (360, 0), (1_920, 0)]);
    // Slipped marks and the mark at the loop's exclusive end stay hidden.
    assert_eq!(
        exported[1],
        [
            (2_280, 127),
            (2_400, 0),
            (2_520, 127),
            (2_880, 127),
            (3_240, 0),
            (3_240, 127),
            (3_720, 127),
            (3_840, 0),
        ]
    );
    assert_eq!(result.report.written[0].automation_events, 3);
    assert_eq!(result.report.written[1].automation_events, 8);
}

#[test]
fn midi_export_orders_a_tied_pedal_press_between_note_off_and_on() {
    let project = parsed_project(
        r#"@project "Pedal tie"
  tempo: 120

@track piano | instrument: piano/acoustic_grand
  @phrase main [1:1 - 2:1]
    C4 q mf
    @1 pedal down
    @1 D4 q mf
    @2 pedal up
"#,
    );
    let result = export_smf(&project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let events = absolute_events(&smf.tracks[1]);
    let at_press = events
        .iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Midi { message, .. } if *tick == 480 => Some(*message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        at_press.as_slice(),
        [
            MidiMessage::NoteOff { key: off, .. },
            MidiMessage::Controller { controller, value },
            MidiMessage::NoteOn { key: on, .. }
        ] if off.as_int() == 60 && controller.as_int() == 64
            && value.as_int() == 127 && on.as_int() == 62
    ));
}

#[test]
fn a_slipped_loop_exports_only_its_source_window_through_the_partial_final_cycle() {
    let project = riff_project("  clip: riff [1:1 - 2:1] offset: 1q loop: 1.5q");
    let (onsets, last_tick, report) = export_notes(&project);
    assert_eq!(
        onsets,
        vec![(0, 62), (480, 64), (720, 62), (1_200, 64), (1_440, 62)]
    );
    assert_eq!(last_tick, 4 * 480);
    assert_eq!(clip_window_loss(&report), 2);
}

#[test]
fn shifted_loop_hits_export_positive_gates_inside_the_final_clip_edge() {
    let project = parsed_project(
        r#"@project "Shifted hits"
  tempo: 120
  time_signature: 4/4

@pattern beat | instrument: drums/acoustic_kit
  hits:
    kick 1:2 mf midi_note=36 timing_offset_beats=-0.125 gate_quarters=1.75
    snare 1:3 mf midi_note=38 timing_offset_beats=0.125
    kick 3:1 mf

@track drums | instrument: drums/acoustic_kit
  clip: beat [1:1 - 1:3] offset: 0.5q loop: 1.625q
"#,
    );
    let result = export_smf(&project, &ExportOptions::default()).unwrap();
    let smf = Smf::parse(&result.bytes).unwrap();
    let gates: Vec<_> = absolute_events(&smf.tracks[1])
        .into_iter()
        .filter_map(|(tick, kind)| match kind {
            TrackEventKind::Midi {
                message: MidiMessage::NoteOn { key, .. },
                ..
            } => Some((tick, key.as_int(), true)),
            TrackEventKind::Midi {
                message: MidiMessage::NoteOff { key, .. },
                ..
            } => Some((tick, key.as_int(), false)),
            _ => None,
        })
        .collect();
    assert_eq!(
        gates,
        [
            (0, 36, true),
            (180, 38, true),
            (240, 38, false),
            (720, 36, true),
            (840, 36, false),
            (960, 36, false),
        ]
    );
}

#[test]
fn drum_grid_edge_hits_sound_and_export_before_the_exclusive_clip_end() {
    let source = format!(
        r#"@project "Fine grid"
  tempo: 120
  swing: 100%

@pattern beat | instrument: drums/acoustic_kit
  hits:
    kick 3:3 mf grid_offset_beats=-0.125
    snare 3:3 mf grid_offset_beats={}
    hihat 3:3 mf timing_offset_beats=-0.125
    clap 3:2 mf

@track drums | instrument: drums/acoustic_kit
  clip: beat [1:1 - 1:3:3]
"#,
        8.0 / 3.0 - 2.75
    );
    let project = parsed_project(&source);
    let settings = oc_engine::interpret::InterpretSettings {
        sample_rate: 48_000,
        ..Default::default()
    };
    let rendered = oc_engine::interpret::try_interpret(&project, &settings)
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            oc_engine::interpret::MidiMessage::NoteOn { note, .. } => {
                Some((event.time_samples, note))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rendered, [(63_000, 36), (64_000, 38)]);
    let (exported, _, _) = export_notes(&project);
    assert_eq!(exported, [(1260, 36), (1280, 38)]);
}

#[test]
fn a_trimmed_clip_exports_only_the_first_bar() {
    let project = parsed_project(
        r#"@ocp "0.2.0"
@project "Trimmed"
  tempo: 120
  time_signature: 4/4

@pattern line | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf
    @4 G4 q mf
    @5 A4 q mf
    @6 B4 q mf
    @7 C5 q mf

@track keys | instrument: piano/acoustic_grand
  clip: line [1:1 - 2:1]
"#,
    );
    let (onsets, _, report) = export_notes(&project);
    assert_eq!(report.written[0].notes, 4, "only the first bar sounds");
    assert_eq!(
        onsets.iter().map(|(tick, _)| *tick).collect::<Vec<_>>(),
        vec![0, 480, 960, 1_440]
    );
    assert_eq!(clip_window_loss(&report), 4);
}

#[test]
fn a_note_outside_every_clip_is_reported() {
    let project = parsed_project(
        r#"@ocp "0.2.0"
@project "Outside"
  tempo: 120
  time_signature: 4/4

@pattern late | instrument: piano/acoustic_grand
  events:
    @4 C4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: late [1:1 - 2:1]
"#,
    );
    let (onsets, _, report) = export_notes(&project);
    assert!(onsets.is_empty(), "the clip ends before the note starts");
    assert_eq!(report.written[0].notes, 0);
    assert_eq!(clip_window_loss(&report), 1);
}

#[test]
fn an_unplaced_pattern_reports_its_content_as_a_clip_window_loss() {
    let project = parsed_project(
        r#"@ocp "0.2.0"
@project "Unplaced"
  tempo: 120
  time_signature: 4/4

@pattern loose | instrument: piano/acoustic_grand
  events:
    @0 C4 q mf
    @4 D4 q mf

@pattern taps | instrument: drums/acoustic_kit
  hits:
    kick 1:0 mf

@track keys | instrument: piano/acoustic_grand
  @phrase lead [1:1 - 2:1]
    @0 E4 q mf
"#,
    );
    let (onsets, _, report) = export_notes(&project);
    assert_eq!(onsets.len(), 1, "only the placed phrase sounds");
    assert_eq!(report.written[0].notes, 1);
    assert_eq!(
        clip_window_loss(&report),
        3,
        "two unplaced notes and one unplaced hit, counted once"
    );
}

// ============================================================================
// Round trip and determinism
// ============================================================================

#[test]
fn import_export_import_preserves_notes_clock_and_cc() {
    let fixture = standard_fixture();
    let first = import(&fixture);
    let options = ExportOptions::default();
    let exported = export_smf(&first.project, &options).unwrap();
    let second = import(&exported.bytes);

    assert_eq!(
        oc_dsl::serialize(&first.project),
        oc_dsl::serialize(&second.project)
    );
    assert_eq!(
        first.project.properties.tempo,
        second.project.properties.tempo
    );
    assert_eq!(
        first.project.properties.tempo_map,
        second.project.properties.tempo_map
    );
    assert_eq!(
        first.project.properties.time_sig_map,
        second.project.properties.time_sig_map
    );
    assert_eq!(first.project.properties.key, second.project.properties.key);

    for (left, right) in first.project.tracks.iter().zip(&second.project.tracks) {
        assert_eq!(left.name, right.name);
        assert_eq!(notes(left), notes(right));
        let clock = ProjectClock::new(&first.project.properties).unwrap();
        for left_lane in left.automation() {
            let mirror = lane(right, &left_lane.param);
            assert_eq!(left_lane.ppq, mirror.ppq);
            let left_ticks = lane_ticks(&clock, left_lane);
            let right_ticks = lane_ticks(&clock, mirror);
            assert_eq!(left_ticks.len(), right_ticks.len());
            for (left, right) in left_ticks.iter().zip(&right_ticks) {
                assert!((left.0 as f64 - right.0 as f64).abs() <= 1.0);
                assert!((left.1 - right.1).abs() < 0.001);
            }
        }
    }

    let ocp = oc_dsl::serialize(&first.project);
    let reopened = oc_dsl::parse(&ocp).unwrap();
    let from_ocp = import_smf(
        &export_smf(&reopened, &options).unwrap().bytes,
        Some("fixture".to_string()),
        &ImportOptions::default(),
        None,
    )
    .unwrap();
    assert_eq!(
        oc_dsl::serialize(&first.project),
        oc_dsl::serialize(&from_ocp.project)
    );
}

#[test]
fn import_and_export_are_deterministic() {
    let fixture = standard_fixture();
    let first = import(&fixture);
    let second = import(&fixture);
    assert_eq!(
        oc_dsl::serialize(&first.project),
        oc_dsl::serialize(&second.project)
    );
    assert_eq!(first.summary, second.summary);
    let exported_a = export_smf(&first.project, &ExportOptions::default()).unwrap();
    let exported_b = export_smf(&second.project, &ExportOptions::default()).unwrap();
    assert_eq!(exported_a.bytes, exported_b.bytes);
}

// ============================================================================
// Errors and scale
// ============================================================================

#[test]
fn malformed_files_return_typed_errors_with_offsets() {
    let bad_magic = b"NOPE\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0".to_vec();
    match import_smf(&bad_magic, None, &ImportOptions::default(), None) {
        Err(error) => {
            assert!(matches!(error, MidiError::Malformed { .. }));
            assert_eq!(error.offset(), Some(0));
        }
        Ok(_) => panic!("bad header must fail"),
    }

    let short = b"MThd\x00\x00\x00\x06".to_vec();
    match import_smf(&short, None, &ImportOptions::default(), None) {
        Err(MidiError::Truncated { offset, .. }) => assert_eq!(offset, 8),
        other => panic!("expected truncated header, got {other:?}"),
    }

    let fixture = standard_fixture();
    let truncated = &fixture[..fixture.len() - 12];
    match import_smf(truncated, None, &ImportOptions::default(), None) {
        Err(MidiError::Truncated { offset, .. }) => assert_eq!(offset, truncated.len()),
        other => panic!("expected truncated chunk, got {other:?}"),
    }

    let mut invalid_event = Vec::new();
    invalid_event.extend_from_slice(b"MThd");
    invalid_event.extend_from_slice(&6_u32.to_be_bytes());
    invalid_event.extend_from_slice(&0_u16.to_be_bytes());
    invalid_event.extend_from_slice(&1_u16.to_be_bytes());
    invalid_event.extend_from_slice(&480_u16.to_be_bytes());
    invalid_event.extend_from_slice(b"MTrk");
    invalid_event.extend_from_slice(&3_u32.to_be_bytes());
    invalid_event.extend_from_slice(&[0x00, 0x90, 0x3C]);
    match import_smf(&invalid_event, None, &ImportOptions::default(), None) {
        Err(MidiError::Malformed { offset, .. }) => assert_eq!(offset, 22),
        other => panic!("expected malformed event, got {other:?}"),
    }
}

#[test]
fn twenty_thousand_notes_import_in_linear_time() {
    let mut events = vec![(0, track_name(b"Synthetic")), (0, program(0, 0))];
    for index in 0..20_000_u64 {
        let start = index * 30;
        let key = 60 + (index % 12) as u8;
        events.push((start, note_on(0, key, 100)));
        events.push((start + 15, note_off(0, key)));
    }
    events.push((600_000, end_of_track()));
    let fixture = smf_bytes(Format::SingleTrack, 480, vec![events]);
    let started = Instant::now();
    let result = import(&fixture);
    let elapsed = started.elapsed();
    assert_eq!(result.project.tracks.len(), 1);
    assert_eq!(notes(&result.project.tracks[0]).len(), 20_000);
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "import took {elapsed:?}, expected linear-ish behavior"
    );
}
