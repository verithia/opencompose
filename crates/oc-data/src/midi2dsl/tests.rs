use super::*;
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use oc_dsl::ast::{BarBeat, ParamValue};

fn on(channel: u8, key: u8, velocity: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: channel.into(),
        message: MidiMessage::NoteOn {
            key: key.into(),
            vel: velocity.into(),
        },
    }
}
fn off(channel: u8, key: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: channel.into(),
        message: MidiMessage::NoteOff {
            key: key.into(),
            vel: 0.into(),
        },
    }
}
fn fixture(ppq: u16, format: Format, tracks: Vec<Vec<(u32, TrackEventKind<'static>)>>) -> Vec<u8> {
    let tracks = tracks
        .into_iter()
        .map(|mut events| {
            events.sort_by_key(|(tick, _)| *tick);
            let mut previous = 0;
            events
                .into_iter()
                .map(|(tick, kind)| {
                    let delta = tick - previous;
                    previous = tick;
                    TrackEvent {
                        delta: delta.into(),
                        kind,
                    }
                })
                .collect()
        })
        .collect();
    let smf = Smf {
        header: Header::new(format, Timing::Metrical(ppq.into())),
        tracks,
    };
    let mut data = vec![];
    smf.write_std(&mut data).unwrap();
    data
}
fn number(modifiers: &[(String, ParamValue)], key: &str) -> f64 {
    modifiers
        .iter()
        .find(|(candidate, _)| candidate == key)
        .unwrap()
        .1
        .as_f64()
        .unwrap()
}
fn import(data: &[u8]) -> ImportResult {
    midi_bytes_to_import(data, Some("fixture".into())).unwrap()
}

#[test]
fn dataset_adapter_is_the_shared_engine_profile_not_another_parser() {
    let data = fixture(
        997,
        Format::SingleTrack,
        vec![vec![(487, on(0, 0, 91)), (720, off(0, 0))]],
    );
    let adapter = import(&data);
    let owner = oc_engine::midi::import_smf_dataset(&data, Some("fixture".into())).unwrap();
    assert_eq!(adapter.project, owner.project);
    assert_eq!(adapter.summary, owner.summary);
    assert_eq!(adapter.summary.source_ppq, 997);
    assert_eq!(adapter.summary.target_ppq, 997);
    assert_eq!(adapter.summary.ticks_shifted, 0);
    let app = oc_engine::midi::import_smf(&data, None, &Default::default(), None).unwrap();
    assert_eq!(app.summary.target_ppq, 480);
}

#[test]
fn exact_source_ticks_gate_velocity_and_humanization_survive_serialization_and_interpretation() {
    let data = fixture(
        997,
        Format::SingleTrack,
        vec![vec![(487, on(0, 60, 91)), (720, off(0, 60))]],
    );
    let imported = import(&data);
    validate_dataset_summary(&imported.summary).unwrap();
    let project = oc_dsl::parse(&oc_dsl::serialize(&imported.project)).unwrap();
    let note = &project.tracks[0].phrases[0].events[0];
    assert_eq!(note.velocity, Some(91));
    assert_eq!(note.start_beat, 487.0 / 997.0);
    assert_eq!(number(&note.modifiers, "midi_start_tick"), 487.0);
    assert_eq!(number(&note.modifiers, "midi_duration_ticks"), 233.0);
    assert_eq!(number(&note.modifiers, "gate_quarters"), 233.0 / 997.0);
    assert_eq!(number(&note.modifiers, "duration_beats"), 233.0 / 997.0);
    assert!(
        (number(&note.modifiers, "humanize_offset_beats") - (487.0 / 997.0 - 0.5)).abs() < 1e-15
    );
    let events = oc_engine::interpret::try_interpret(
        &project,
        &oc_engine::interpret::InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    let start = events
        .iter()
        .find(|event| {
            matches!(
                event.event,
                oc_engine::interpret::MidiMessage::NoteOn {
                    note: 60,
                    velocity: 91,
                    ..
                }
            )
        })
        .unwrap()
        .time_samples;
    let end = events
        .iter()
        .find(|event| {
            matches!(
                event.event,
                oc_engine::interpret::MidiMessage::NoteOff { note: 60, .. }
            )
        })
        .unwrap()
        .time_samples;
    assert!(start.abs_diff((487.0_f64 / 997.0 * 24000.0).floor() as u64) <= 1);
    assert!(end.abs_diff((720.0_f64 / 997.0 * 24000.0).floor() as u64) <= 1);
}

#[test]
fn explicit_release_preserves_all_same_key_onsets_and_strict_policy_exposes_missing_releases() {
    let mut track = vec![
        (0, on(0, 60, 37)),
        (0, on(0, 60, 91)),
        (120, on(0, 60, 73)),
        (480, off(0, 60)),
    ];
    let result = import(&fixture(480, Format::SingleTrack, vec![track.clone()]));
    validate_dataset_summary(&result.summary).unwrap();
    let notes = &result.project.tracks[0].phrases[0].events;
    assert_eq!(notes.len(), 3);
    assert_eq!(
        notes
            .iter()
            .map(|note| (
                note.start_beat,
                note.velocity,
                number(&note.modifiers, "gate_quarters")
            ))
            .collect::<Vec<_>>(),
        [
            (0.0, Some(37), 1.0),
            (0.0, Some(91), 1.0),
            (0.25, Some(73), 0.75)
        ]
    );
    track.pop();
    let repaired = import(&fixture(480, Format::SingleTrack, vec![track]));
    assert_eq!(repaired.summary.unterminated_notes, 3);
    assert!(
        validate_dataset_summary(&repaired.summary)
            .unwrap_err()
            .contains("3 unreleased note-ons")
    );
}

#[test]
fn drum_offsets_preserve_one_tick_groove_and_exact_source_key_and_velocity() {
    for tick in [1, 59, 61, 119, 121, 479, 2400, 3959] {
        let result = import(&fixture(
            480,
            Format::SingleTrack,
            vec![vec![(tick, on(9, 46, 37)), (tick + 60, off(9, 46))]],
        ));
        validate_dataset_summary(&result.summary).unwrap();
        let project = oc_dsl::parse(&oc_dsl::serialize(&result.project)).unwrap();
        let pattern = &project.tracks[0].patterns[0];
        assert_eq!(pattern.repeat, 1);
        assert_eq!(pattern.hits.len(), 1);
        let hit = &pattern.hits[0];
        assert!((hit.written_quarters() - f64::from(tick) / 480.0).abs() < 1e-14);
        assert_eq!(number(&hit.modifiers, "midi_note"), 46.0);
        assert_eq!(number(&hit.modifiers, "midi_start_tick"), f64::from(tick));
        assert_eq!(hit.velocity, Some(37));
        assert_eq!(
            result.project.tracks[0].patterns[0].hits[0].velocity,
            Some(37)
        );
        let observe = |project: &Project| {
            oc_engine::interpret::try_interpret(
                project,
                &oc_engine::interpret::InterpretSettings {
                    sample_rate: 48000,
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.event {
                oc_engine::interpret::MidiMessage::NoteOn { note, velocity, .. } => {
                    Some((true, note, velocity, event.time_samples))
                }
                oc_engine::interpret::MidiMessage::NoteOff { note, .. } => {
                    Some((false, note, 0, event.time_samples))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
        };
        let before = observe(&result.project);
        let after = observe(&project);
        assert_eq!(
            before, after,
            "OCP persistence must preserve the actual MIDI performance"
        );
        assert_eq!(after.len(), 2);
        assert_eq!((after[0].0, after[0].1, after[0].2), (true, 46, 37));
        assert_eq!((after[1].0, after[1].1), (false, 46));
        assert!(after[0].3.abs_diff(u64::from(tick) * 50) <= 1);
        assert!(after[1].3.abs_diff(u64::from(tick + 60) * 50) <= 1);
        let exported = oc_engine::midi::export_smf(&project, &Default::default()).unwrap();
        let smf = Smf::parse(&exported.bytes).unwrap();
        let exported_notes = smf
            .tracks
            .iter()
            .flatten()
            .filter_map(|event| match event.kind {
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { key, vel },
                    ..
                } if vel.as_int() != 0 => Some((key.as_int(), vel.as_int())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(exported_notes, [(46, 37)]);
    }
}

#[test]
fn shared_clock_preserves_fractional_tempo_and_meter_changes() {
    let data = fixture(
        480,
        Format::Parallel,
        vec![
            vec![
                (0, TrackEventKind::Meta(MetaMessage::Tempo(500001.into()))),
                (
                    0,
                    TrackEventKind::Meta(MetaMessage::TimeSignature(7, 3, 24, 8)),
                ),
                (
                    480,
                    TrackEventKind::Meta(MetaMessage::Tempo(1000000.into())),
                ),
                (
                    3360,
                    TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
                ),
            ],
            vec![(123, on(0, 60, 73)), (603, off(0, 60))],
        ],
    );
    let result = import(&data);
    validate_dataset_summary(&result.summary).unwrap();
    assert_eq!(result.project.properties.tempo, 60000000.0 / 500001.0);
    assert_eq!(
        result.project.properties.tempo_map[0].position,
        BarBeat::new(1, 3, 0)
    );
    assert_eq!(
        result.project.properties.time_sig_map[0].position,
        BarBeat::new(3, 1, 0)
    );
    assert_eq!(
        result.project.tracks[0].phrases[0].events[0].start_beat,
        123.0 / 480.0
    );
    let restored = oc_dsl::parse(&oc_dsl::serialize(&result.project)).unwrap();
    assert_eq!(restored.properties, result.project.properties);
}

#[test]
fn source_tracks_sharing_a_channel_remain_separate_and_keep_exact_programs() {
    let program = |id: u8| TrackEventKind::Midi {
        channel: 0.into(),
        message: MidiMessage::ProgramChange { program: id.into() },
    };
    let result = import(&fixture(
        480,
        Format::Parallel,
        vec![
            vec![(0, program(25)), (0, on(0, 60, 100)), (480, off(0, 60))],
            vec![(0, program(33)), (0, on(0, 48, 90)), (480, off(0, 48))],
        ],
    ));
    assert_eq!(result.project.tracks.len(), 2);
    assert_eq!(
        result
            .summary
            .tracks
            .iter()
            .filter_map(|track| track.program)
            .collect::<Vec<_>>(),
        [25, 33]
    );
    assert_eq!(
        result
            .summary
            .tracks
            .iter()
            .map(|track| track.source_track)
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn dataset_rejects_asynchronous_and_malformed_input_and_strict_clock_losses() {
    let track = vec![(0, on(0, 60, 100)), (480, off(0, 60))];
    assert!(
        midi_bytes_to_import(&fixture(480, Format::Sequential, vec![track.clone()]), None)
            .unwrap_err()
            .contains("asynchronous")
    );
    assert!(midi_bytes_to_import(b"MThd", None).is_err());
    let bad = fixture(
        480,
        Format::SingleTrack,
        vec![vec![
            (
                0,
                TrackEventKind::Meta(MetaMessage::TimeSignature(4, 255, 24, 8)),
            ),
            (0, on(0, 60, 100)),
            (480, off(0, 60)),
        ]],
    );
    let result = import(&bad);
    assert!(validate_dataset_summary(&result.summary).is_err());
    let conflicts = fixture(
        480,
        Format::SingleTrack,
        vec![vec![
            (0, TrackEventKind::Meta(MetaMessage::Tempo(500000.into()))),
            (0, TrackEventKind::Meta(MetaMessage::Tempo(600000.into()))),
            (0, on(0, 60, 100)),
            (480, off(0, 60)),
        ]],
    );
    assert!(
        midi_bytes_to_import(&conflicts, None)
            .unwrap_err()
            .contains("conflicting MIDI tempo")
    );
}

struct MidiFile(std::path::PathBuf);
impl MidiFile {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let file = Self(std::env::temp_dir().join(format!(
                "oc-data-midi-{}-{}-{}.mid",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )));
        std::fs::write(&file.0, bytes).unwrap();
        file
    }
}
impl Drop for MidiFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn file_facade_is_strict_while_explicit_summary_api_retains_repairs_and_bounds_reads() {
    let good = fixture(
        480,
        Format::SingleTrack,
        vec![vec![(0, on(0, 60, 100)), (480, off(0, 60))]],
    );
    let file = MidiFile::new(&good);
    assert_eq!(
        midi_to_project(&file.0).unwrap(),
        midi_to_import(&file.0).unwrap().project
    );
    assert!(
        read_midi_bytes(&file.0, 8)
            .unwrap_err()
            .contains("midi_bytes_limit")
    );
    let file = MidiFile::new(&fixture(
        480,
        Format::SingleTrack,
        vec![vec![
            (0, on(0, 60, 100)),
            (480, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]],
    ));
    let summary = midi_to_import(&file.0).unwrap().summary;
    assert_eq!(summary.unterminated_notes, 1);
    assert!(
        midi_to_project(&file.0)
            .unwrap_err()
            .contains("1 unreleased note-ons")
    );
}
