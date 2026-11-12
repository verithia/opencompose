//! MIDI bridge — the only module coupled to `oc_engine::midi` and `midi_io`.
//!
//! Tauri commands, MCP tools and the UI all use these wrappers, so the core
//! modules stay swappable without touching the command layer.

use std::sync::atomic::AtomicBool;

use oc_dsl::ast::{Project, ProjectProperties};
use oc_engine::midi::MidiError as CoreMidiError;

pub use oc_engine::midi::{
    ExportOptions, ExportReport, ExportResult, ImportOptions, ImportResult, ImportStage,
    ImportSummary,
};
#[cfg(test)]
pub use oc_engine::midi_io::{FakeBackend, PortDirection};
pub use oc_engine::midi_io::{
    LiveEvent, MidiBackend, MidiError, MidiInputEvent, MidiPortInfo, MidiSession, MidirBackend,
};

pub fn read_smf_file_cancellable(
    path: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, CoreMidiError> {
    oc_engine::midi::read_smf_file_cancellable(
        path,
        &oc_engine::midi::ImportLimits::default(),
        cancel,
    )
}

pub fn import_smf_cancellable(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: &Project,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportStage, f64),
) -> Result<ImportResult, CoreMidiError> {
    oc_engine::midi::import_smf_cancellable(data, name, options, Some(base), cancel, progress)
}

/// Portable handoffs retain key gates, pedal lanes and controller-only tracks.
pub(crate) fn handoff_import_options(import_clock: bool) -> ImportOptions {
    ImportOptions {
        split_channels: true,
        expand_sustain: false,
        drop_empty: false,
        import_clock,
        ..Default::default()
    }
}

/// Keep project settings that the MIDI conductor does not carry.
pub(crate) fn preserve_non_midi_properties(imported: &mut Project, base: &Project) {
    let ProjectProperties {
        tempo,
        tempo_map,
        time_signature,
        time_sig_map,
        key,
        key_map,
        ..
    } = std::mem::replace(&mut imported.properties, base.properties.clone());
    imported.properties.tempo = tempo;
    imported.properties.tempo_map = tempo_map;
    imported.properties.time_signature = time_signature;
    imported.properties.time_sig_map = time_sig_map;
    imported.properties.key = key;
    imported.properties.key_map = key_map;
}

pub fn export_smf(project: &Project, options: &ExportOptions) -> Result<ExportResult, String> {
    oc_engine::midi::export_smf(project, options).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_engine::midi::{
        ExportScope, LossItem, LossKind, TrackMapping, TrackReport, TrackTarget,
    };

    #[test]
    fn import_options_defaults_match_the_contract() {
        let options = ImportOptions::default();
        assert!(!options.split_channels);
        assert!(options.drop_empty);
        assert!(options.expand_sustain);
        assert!(options.import_clock);
        assert!(options.mappings.is_empty());
    }

    #[test]
    fn mapping_targets_serialize_like_the_core_enum() {
        let existing = TrackMapping {
            source_track: 2,
            channel: Some(10),
            target: TrackTarget::ExistingTrack(1),
            name: None,
        };
        assert_eq!(
            serde_json::to_value(&existing).unwrap(),
            serde_json::json!({
                "source_track": 2,
                "channel": 10,
                "target": { "ExistingTrack": 1 },
                "name": null
            })
        );
        assert_eq!(
            serde_json::to_value(TrackTarget::NewTrack).unwrap(),
            "NewTrack"
        );
        assert_eq!(serde_json::to_value(TrackTarget::Drop).unwrap(), "Drop");
    }

    #[test]
    fn export_scope_and_loss_kind_match_the_core_json() {
        assert_eq!(
            serde_json::to_value(ExportScope::Project).unwrap(),
            "Project"
        );
        assert_eq!(
            serde_json::to_value(ExportScope::Tracks { start: 1, end: 3 }).unwrap(),
            serde_json::json!({ "Tracks": { "start": 1, "end": 3 } })
        );
        assert_eq!(
            serde_json::to_value(ExportScope::Track(4)).unwrap(),
            serde_json::json!({ "Track": 4 })
        );
        assert_eq!(
            serde_json::to_value(LossKind::PluginParameter).unwrap(),
            "plugin_parameter"
        );
        assert_eq!(
            serde_json::to_value(LossKind::MuteSolo).unwrap(),
            "mute_solo"
        );
    }

    fn import(data: &[u8], options: &ImportOptions, base: &Project) -> ImportResult {
        import_smf_cancellable(
            data,
            None,
            options,
            base,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap()
    }

    fn empty_project() -> Project {
        Project {
            name: Some("Test".into()),
            ocp_version: Some("0.2.0".into()),
            properties: Default::default(),
            presets: Vec::new(),
            tracks: Vec::new(),
            patterns: Vec::new(),
            buses: Vec::new(),
            master: None,
            sections: Vec::new(),
            markers: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn one_note_smf() -> Vec<u8> {
        use midly::{
            Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
        };
        let header = Header::new(Format::SingleTrack, Timing::Metrical(480.into()));
        let mut smf = Smf::new(header);
        smf.tracks.push(vec![
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Lead")),
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(500_000.into())),
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message: MidiMessage::ProgramChange { program: 0.into() },
                },
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message: MidiMessage::NoteOn {
                        key: 60.into(),
                        vel: 100.into(),
                    },
                },
            },
            TrackEvent {
                delta: 480.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message: MidiMessage::NoteOff {
                        key: 60.into(),
                        vel: 64.into(),
                    },
                },
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ]);
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn bridge_imports_a_real_smf_and_exports_it_back() {
        let options = ImportOptions {
            mappings: vec![TrackMapping {
                source_track: 0,
                channel: None,
                target: TrackTarget::NewTrack,
                name: Some("Lead".into()),
            }],
            ..ImportOptions::default()
        };
        let imported = import(&one_note_smf(), &options, &empty_project());
        assert_eq!(imported.project.tracks.len(), 1);
        assert_eq!(imported.project.tracks[0].name, "Lead");
        assert_eq!(imported.project.tracks[0].phrases[0].events.len(), 1);
        assert_eq!(imported.summary.tracks.len(), 1);
        assert_eq!(imported.summary.tracks[0].note_count, 1);

        let exported = export_smf(&imported.project, &ExportOptions::default()).unwrap();
        assert!(!exported.bytes.is_empty());
        assert_eq!(exported.report.written.len(), 1);
        assert_eq!(exported.report.written[0].notes, 1);
    }

    #[test]
    fn bridge_maps_an_smf_track_onto_an_existing_project_track() {
        let mut base = empty_project();
        let mut existing = base.tracks.first().cloned().unwrap_or_else(|| {
            let mut track = oc_dsl::ast::Track {
                name: "Existing".into(),
                channel: None,
                instrument: oc_dsl::ast::InstrumentPath::new("piano", "acoustic_grand"),
                preset: None,
                engine: None,
                mix: Default::default(),
                effects: Vec::new(),
                phrases: Vec::new(),
                patterns: Vec::new(),
                inline_patterns: Vec::new(),
                clips: oc_dsl::ast::ClipPool::new(),
                gain_automation: Vec::new(),
                engine_params: None,
                performance: None,
                output_port: None,
                output_pair: None,
                input_routes: vec![],
            };
            track.phrases.clear();
            track
        });
        existing.name = "Existing".into();
        base.tracks.push(existing);

        let options = ImportOptions {
            mappings: vec![TrackMapping {
                source_track: 0,
                channel: Some(1),
                target: TrackTarget::ExistingTrack(0),
                name: None,
            }],
            ..ImportOptions::default()
        };
        let imported = import(&one_note_smf(), &options, &base);
        assert_eq!(imported.project.tracks.len(), 1);
        assert_eq!(imported.project.tracks[0].phrases.len(), 1);
        assert_eq!(imported.summary.tracks[0].target_track, Some(0));
    }

    #[test]
    fn export_report_roundtrips() {
        let report = ExportReport {
            format: 1,
            ppq: 480,
            written: vec![TrackReport {
                track: 0,
                name: "Piano".into(),
                channel: 1,
                notes: 16,
                widened_notes: 0,
                automation_events: 2,
                program: Some(0),
                smf_track: 1,
            }],
            losses: vec![LossItem {
                kind: LossKind::Articulation,
                count: 3,
                detail: "3 notes carry staccato.".into(),
            }],
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["written"][0]["automation_events"], 2);
        assert_eq!(value["losses"][0]["kind"], "articulation");
        assert_eq!(
            serde_json::from_value::<ExportReport>(value).unwrap(),
            report
        );
    }
}
