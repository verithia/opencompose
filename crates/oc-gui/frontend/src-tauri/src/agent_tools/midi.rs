//! Typed arguments for MIDI import and export tools.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ProjectRevision, ToolArguments};

#[derive(Debug, Deserialize, JsonSchema)]
/// Import one staged .mid or .midi from handoff/ into the saved project. Supply the current project_get revision. Preserves key-release note lengths and separate CC64 sustain by channel. Imports the MIDI clock for an empty project and keeps the clock of an existing arrangement.
pub struct MidiImportHandoffArgs {
    #[schemars(description = "Project-relative staged MIDI file, for example handoff/Keys.mid")]
    pub path: String,
    /// Current project revision from project_get
    pub expected_revision: ProjectRevision,
}

impl ToolArguments for MidiImportHandoffArgs {
    const ID: ToolId = ToolId::MidiImportHandoff;
    const FIELD_ORDER: &'static [&'static str] = &["path", "expected_revision"];
}

fn inline_schema(value: Value) -> Schema {
    serde_json::from_value(value).expect("an inline tool schema is valid")
}

fn import_options_value() -> Value {
    json!({
        "type": "object",
        "description": "Import options; omitted fields keep the defaults.",
        "properties": {
            "split_channels": {"type": "boolean", "description": "Split every source track into one track per channel"},
            "drop_empty": {"type": "boolean", "description": "Drop tracks/channels without notes (default true)"},
            "expand_sustain": {"type": "boolean", "description": "Extend note ends to CC64 releases (default true)"},
            "import_clock": {"type": "boolean", "description": "Import tempo, meter and key maps (default true)"},
            "mappings": {
                "type": "array",
                "description": "Per source-track/channel overrides",
                "items": {
                    "type": "object",
                    "properties": {
                        "source_track": {"type": "integer", "description": "Zero-based SMF track index"},
                        "channel": {"type": ["integer", "null"], "description": "1-based channel; null keeps all channels"},
                        "target": {
                            "oneOf": [
                                {"type": "string", "enum": ["NewTrack", "Drop"]},
                                {
                                    "type": "object",
                                    "properties": {"ExistingTrack": {"type": "integer", "description": "Project track index"}},
                                    "required": ["ExistingTrack"]
                                }
                            ]
                        },
                        "name": {"type": ["string", "null"], "description": "New track name"}
                    },
                    "required": ["source_track", "target"]
                }
            }
        }
    })
}

fn export_options_value() -> Value {
    json!({
        "type": "object",
        "description": "Export options; omitted fields keep the defaults (format 1, 480 PPQ, whole project).",
        "properties": {
            "format": {"type": "integer", "enum": [0, 1]},
            "ppq": {"type": "integer", "enum": [480, 960]},
            "scope": {
                "oneOf": [
                    {"type": "string", "enum": ["Project"]},
                    {
                        "type": "object",
                        "properties": {"Track": {"type": "integer", "description": "Track index"}},
                        "required": ["Track"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "Tracks": {
                                "type": "object",
                                "properties": {
                                    "start": {"type": "integer", "description": "First track (inclusive)"},
                                    "end": {"type": "integer", "description": "End track (exclusive)"}
                                },
                                "required": ["start", "end"]
                            }
                        },
                        "required": ["Tracks"]
                    }
                ]
            },
            "channels": {
                "type": "array",
                "description": "1-based channel overrides",
                "items": {
                    "type": "object",
                    "properties": {
                        "track": {"type": "integer", "description": "Project track index"},
                        "channel": {"type": "integer", "minimum": 1, "maximum": 16}
                    },
                    "required": ["track", "channel"]
                }
            }
        }
    })
}

struct ImportOptionsSchema;

impl JsonSchema for ImportOptionsSchema {
    fn schema_name() -> String {
        "ImportOptions".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(import_options_value())
    }
}

struct ExportOptionsSchema;

impl JsonSchema for ExportOptionsSchema {
    fn schema_name() -> String {
        "ExportOptions".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(export_options_value())
    }
}

#[derive(Debug, Deserialize)]
/// Options controlling MIDI import.
pub struct ImportOptionsArgs {
    pub split_channels: Option<bool>,
    pub drop_empty: Option<bool>,
    pub expand_sustain: Option<bool>,
    pub import_clock: Option<bool>,
    pub mappings: Option<Vec<TrackMappingArgs>>,
}

impl ImportOptionsArgs {
    fn merged(&self) -> oc_engine::midi::ImportOptions {
        let defaults = oc_engine::midi::ImportOptions::default();
        oc_engine::midi::ImportOptions {
            split_channels: self.split_channels.unwrap_or(defaults.split_channels),
            drop_empty: self.drop_empty.unwrap_or(defaults.drop_empty),
            expand_sustain: self.expand_sustain.unwrap_or(defaults.expand_sustain),
            import_clock: self.import_clock.unwrap_or(defaults.import_clock),
            mappings: self
                .mappings
                .as_ref()
                .map(|mappings| mappings.iter().map(TrackMappingArgs::to_domain).collect())
                .unwrap_or(defaults.mappings),
        }
    }
}

#[derive(Debug, Deserialize)]
/// Mapping from an imported MIDI track to a target.
pub struct TrackMappingArgs {
    pub source_track: usize,
    pub channel: Option<u8>,
    pub target: TrackTargetArgs,
    pub name: Option<String>,
}

impl TrackMappingArgs {
    fn to_domain(&self) -> oc_engine::midi::TrackMapping {
        oc_engine::midi::TrackMapping {
            source_track: self.source_track,
            channel: self.channel,
            target: self.target.to_domain(),
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
/// Where an imported MIDI track is placed.
pub enum TrackTargetArgs {
    NewTrack,
    ExistingTrack(usize),
    Drop,
}

impl TrackTargetArgs {
    fn to_domain(&self) -> oc_engine::midi::TrackTarget {
        match self {
            Self::NewTrack => oc_engine::midi::TrackTarget::NewTrack,
            Self::ExistingTrack(index) => oc_engine::midi::TrackTarget::ExistingTrack(*index),
            Self::Drop => oc_engine::midi::TrackTarget::Drop,
        }
    }
}

#[derive(Debug, Deserialize)]
/// Options controlling MIDI export.
pub struct ExportOptionsArgs {
    pub format: Option<u8>,
    pub ppq: Option<u16>,
    pub scope: Option<ExportScopeArgs>,
    pub channels: Option<Vec<ChannelAssignmentArgs>>,
}

impl ExportOptionsArgs {
    fn merged(&self) -> oc_engine::midi::ExportOptions {
        let defaults = oc_engine::midi::ExportOptions::default();
        oc_engine::midi::ExportOptions {
            format: self.format.unwrap_or(defaults.format),
            ppq: self.ppq.unwrap_or(defaults.ppq),
            scope: self
                .scope
                .as_ref()
                .map(ExportScopeArgs::to_domain)
                .unwrap_or(defaults.scope),
            channels: self
                .channels
                .as_ref()
                .map(|channels| {
                    channels
                        .iter()
                        .map(ChannelAssignmentArgs::to_domain)
                        .collect()
                })
                .unwrap_or(defaults.channels),
        }
    }
}

#[derive(Debug, Deserialize)]
/// What part of the project MIDI export covers.
pub enum ExportScopeArgs {
    Project,
    Track(usize),
    Tracks { start: usize, end: usize },
}

impl ExportScopeArgs {
    fn to_domain(&self) -> oc_engine::midi::ExportScope {
        match self {
            Self::Project => oc_engine::midi::ExportScope::Project,
            Self::Track(index) => oc_engine::midi::ExportScope::Track(*index),
            Self::Tracks { start, end } => oc_engine::midi::ExportScope::Tracks {
                start: *start,
                end: *end,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
/// MIDI channel assignment for an exported track.
pub struct ChannelAssignmentArgs {
    pub track: usize,
    pub channel: u8,
}

impl ChannelAssignmentArgs {
    fn to_domain(&self) -> oc_engine::midi::ChannelAssignment {
        oc_engine::midi::ChannelAssignment {
            track: self.track,
            channel: self.channel,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Export a committed saved project or track selection to a new MIDI file in handoff/. Supply the current project_get revision. Returns the relative path and export loss report. Commit the current transaction before exporting.
pub struct MidiExportHandoffArgs {
    /// Current committed project revision from project_get
    pub expected_revision: ProjectRevision,
    #[schemars(with = "Option<ExportOptionsSchema>")]
    pub options: Option<ExportOptionsArgs>,
}

impl MidiExportHandoffArgs {
    /// Convert the wire options into engine export options.
    pub fn export_options(&self) -> oc_engine::midi::ExportOptions {
        self.options
            .as_ref()
            .map(ExportOptionsArgs::merged)
            .unwrap_or_default()
    }
}

impl ToolArguments for MidiExportHandoffArgs {
    const ID: ToolId = ToolId::MidiExportHandoff;
    const FIELD_ORDER: &'static [&'static str] = &["expected_revision"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Preview a Standard MIDI File import without changing the project. Returns the import summary (tracks, ignored events, tick error).
pub struct MidiImportPreviewArgs {
    #[schemars(description = "Absolute path to a .mid/.midi file")]
    pub path: String,
    #[schemars(with = "Option<ImportOptionsSchema>")]
    pub options: Option<ImportOptionsArgs>,
}

impl MidiImportPreviewArgs {
    /// Convert the wire options into engine import options.
    pub fn import_options(&self) -> oc_engine::midi::ImportOptions {
        self.options
            .as_ref()
            .map(ImportOptionsArgs::merged)
            .unwrap_or_default()
    }
}

impl ToolArguments for MidiImportPreviewArgs {
    const ID: ToolId = ToolId::MidiImportPreview;
    const FIELD_ORDER: &'static [&'static str] = &["path"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Import a Standard MIDI File into the project using the mapping options. Returns the summary and new track count.
pub struct MidiImportApplyArgs {
    #[schemars(description = "Absolute path to a .mid/.midi file")]
    pub path: String,
    #[schemars(with = "Option<ImportOptionsSchema>")]
    pub options: Option<ImportOptionsArgs>,
}

impl MidiImportApplyArgs {
    /// Convert the wire options into engine import options.
    pub fn import_options(&self) -> oc_engine::midi::ImportOptions {
        self.options
            .as_ref()
            .map(ImportOptionsArgs::merged)
            .unwrap_or_default()
    }
}

impl ToolArguments for MidiImportApplyArgs {
    const ID: ToolId = ToolId::MidiImportApply;
    const FIELD_ORDER: &'static [&'static str] = &["path"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Export the project (or a selection) as a Standard MIDI File and return the loss report.
pub struct MidiExportArgs {
    #[schemars(description = "Output .mid/.midi path")]
    pub path: String,
    #[schemars(with = "Option<ExportOptionsSchema>")]
    pub options: Option<ExportOptionsArgs>,
}

impl MidiExportArgs {
    /// Convert the wire options into engine export options.
    pub fn export_options(&self) -> oc_engine::midi::ExportOptions {
        self.options
            .as_ref()
            .map(ExportOptionsArgs::merged)
            .unwrap_or_default()
    }
}

impl ToolArguments for MidiExportArgs {
    const ID: ToolId = ToolId::MidiExport;
    const FIELD_ORDER: &'static [&'static str] = &["path"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_midi_handoff_schema_preserves_the_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::MidiImportHandoff,
            ToolId::MidiExportHandoff,
            ToolId::MidiImportPreview,
            ToolId::MidiImportApply,
            ToolId::MidiExport,
        ] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_midi_handoff_requires_a_path_and_revision() {
        let request = MidiImportHandoffArgs::decode(&json!({
            "path": "handoff/Keys.mid",
            "expected_revision": 7,
        }))
        .unwrap();
        assert_eq!(request.path, "handoff/Keys.mid");
        assert_eq!(request.expected_revision.0, 7);
        let error =
            MidiImportHandoffArgs::decode(&json!({"path": "handoff/Keys.mid"})).unwrap_err();
        assert_eq!(error.field.as_deref(), Some("expected_revision"));
    }
}
