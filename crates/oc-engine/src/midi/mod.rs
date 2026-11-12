//! Application MIDI import/export pipeline.
//!
//! The application and dataset profiles share one SMF parser, clock collector
//! and event builder, described by `docs/midi-import-export.md`.
//!
//! - `import_smf` parses an SMF into project tracks plus a UI summary.
//! - `import_smf_dataset` retains source-resolution dataset notation/annotations.
//! - `export_smf` writes a project selection plus a loss report.
//!
//! Both profiles are deterministic. Application import uses 480 PPQ; dataset
//! import retains source PPQ. Automation lanes carry an explicit `ppq` so
//! tick positions survive the project model's quarter-beat grid.

mod export;
mod gm;
mod import;
mod limits;
pub use limits::{
    ImportLimit, ImportLimits, read_smf_file, read_smf_file_cancellable, read_smf_file_with_limits,
};
mod smf;

pub use export::export_smf;
pub use import::{
    import_smf, import_smf_cancellable, import_smf_cancellable_with_limits, import_smf_dataset,
    import_smf_dataset_with_limits, import_smf_with_limits,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Typed MIDI failure. Every parse-family variant carries a byte offset.
#[derive(Debug, Error)]
pub enum MidiError {
    #[error("MIDI import {kind} exceeded at byte {offset}: {actual} > {limit}")]
    LimitExceeded {
        kind: ImportLimit,
        limit: u64,
        actual: u64,
        offset: usize,
    },
    #[error("MIDI input must be a regular file")]
    NonRegularInput,
    #[error("cannot read MIDI input: {message}")]
    InputIo { message: String },
    #[error("malformed MIDI at byte {offset}: {message}")]
    Malformed { offset: usize, message: String },

    #[error("truncated MIDI at byte {offset}: {message}")]
    Truncated { offset: usize, message: String },

    #[error("unsupported MIDI timing at byte {offset}: {message}")]
    UnsupportedTiming { offset: usize, message: String },

    #[error("invalid MIDI event at byte {offset}: {message}")]
    InvalidEvent { offset: usize, message: String },

    #[error("invalid import mapping for source track {source_track}: {message}")]
    InvalidMapping {
        source_track: usize,
        message: String,
    },

    #[error("invalid export: {message}")]
    InvalidExport { message: String },

    #[error("MIDI import cancelled")]
    Cancelled,
}

impl MidiError {
    /// Stable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::LimitExceeded { kind, .. } => kind.code(),
            Self::NonRegularInput => "midi_nonregular_input",
            Self::InputIo { .. } => "midi_input_io",
            Self::Malformed { .. } => "midi_malformed",
            Self::Truncated { .. } => "midi_truncated",
            Self::UnsupportedTiming { .. } => "midi_unsupported_timing",
            Self::InvalidEvent { .. } => "midi_invalid_event",
            Self::InvalidMapping { .. } => "midi_invalid_mapping",
            Self::InvalidExport { .. } => "midi_invalid_export",
            Self::Cancelled => "midi_cancelled",
        }
    }

    /// Byte offset for parse errors; mapping/export errors have no offset.
    pub fn offset(&self) -> Option<usize> {
        match self {
            MidiError::LimitExceeded { offset, .. }
            | MidiError::Malformed { offset, .. }
            | MidiError::Truncated { offset, .. }
            | MidiError::UnsupportedTiming { offset, .. }
            | MidiError::InvalidEvent { offset, .. } => Some(*offset),
            MidiError::NonRegularInput
            | MidiError::InputIo { .. }
            | MidiError::InvalidMapping { .. }
            | MidiError::InvalidExport { .. }
            | MidiError::Cancelled => None,
        }
    }

    pub(crate) fn malformed(offset: usize, message: impl Into<String>) -> Self {
        MidiError::Malformed {
            offset,
            message: message.into(),
        }
    }

    pub(crate) fn truncated(offset: usize, message: impl Into<String>) -> Self {
        MidiError::Truncated {
            offset,
            message: message.into(),
        }
    }
}

/// Import phase reported through the progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    Parse,
    Tracks,
    Clock,
    Notes,
    Params,
    Commit,
}

/// Mapping decisions the import dialog can override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportOptions {
    /// Split every source track into one project track per MIDI channel.
    pub split_channels: bool,
    /// Drop units that contain no notes (channel 10 always remains separate).
    pub drop_empty: bool,
    /// Extend note ends to the sustain-pedal release, then keep CC64 as a lane.
    pub expand_sustain: bool,
    /// Import tempo/meter/key events into the project maps.
    pub import_clock: bool,
    /// Per source-track/channel overrides; unmatched units use the defaults.
    pub mappings: Vec<TrackMapping>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            split_channels: false,
            drop_empty: true,
            expand_sustain: true,
            import_clock: true,
            mappings: Vec::new(),
        }
    }
}

/// One mapping decision. `channel` is 1-based and selects a single channel of
/// the source track, which forces that channel to be split out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackMapping {
    pub source_track: usize,
    pub channel: Option<u8>,
    pub target: TrackTarget,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Where an imported MIDI track is placed.
pub enum TrackTarget {
    NewTrack,
    /// Append notes and lanes after the existing project track index's last
    /// written content, keeping the file's internal timing.
    ExistingTrack(usize),
    Drop,
}

/// Result of an import: the project state plus the dialog summary.
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub project: oc_dsl::ast::Project,
    pub summary: ImportSummary,
}

/// Serialisable import summary. Channel numbers are 1-based.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportSummary {
    pub format: u8,
    pub source_ppq: u16,
    pub target_ppq: u16,
    /// Maximum absolute source-tick error introduced by the 480 PPQ grid.
    pub max_tick_error: f64,
    /// Number of source tick values that moved during conversion.
    pub ticks_shifted: usize,
    pub tempo_changes: usize,
    pub meter_changes: usize,
    pub key_changes: usize,
    pub sustain_expanded_notes: usize,
    pub overlapping_notes: usize,
    pub zero_length_notes: usize,
    pub unterminated_notes: usize,
    pub tracks: Vec<ImportTrackSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Summary of one imported source track.
pub struct ImportTrackSummary {
    pub source_track: usize,
    pub source_track_name: Option<String>,
    pub target_track: Option<usize>,
    pub name: String,
    pub channel: Option<u8>,
    pub program: Option<u8>,
    pub program_name: Option<String>,
    pub preset: Option<String>,
    pub instrument: Option<String>,
    pub note_count: usize,
    pub lanes: Vec<String>,
    pub first_bar: Option<u32>,
    pub last_bar: Option<u32>,
    pub ignored: Vec<IgnoredEvent>,
    pub dropped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Count of events ignored during import.
pub struct IgnoredEvent {
    pub kind: String,
    pub count: usize,
}

/// Export selection, resolution and channel assignment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportOptions {
    /// SMF format: 0 or 1.
    pub format: u8,
    /// Output ticks per quarter note.
    pub ppq: u16,
    pub scope: ExportScope,
    /// 1-based channel overrides; unlisted tracks use the default assignment.
    pub channels: Vec<ChannelAssignment>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: 1,
            ppq: 480,
            scope: ExportScope::Project,
            channels: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Project, track range or single track.
pub enum ExportScope {
    Project,
    /// Zero-based half-open track range.
    Tracks {
        start: usize,
        end: usize,
    },
    Track(usize),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// MIDI channel for an exported track.
pub struct ChannelAssignment {
    pub track: usize,
    pub channel: u8,
}

/// Written MIDI bytes plus the loss report shown before/after writing.
#[derive(Debug, Clone)]
pub struct ExportResult {
    pub bytes: Vec<u8>,
    pub report: ExportReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Written tracks and loss report.
pub struct ExportReport {
    pub format: u8,
    pub ppq: u16,
    pub written: Vec<TrackReport>,
    pub losses: Vec<LossItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Per-track export counts.
pub struct TrackReport {
    pub track: usize,
    pub name: String,
    pub channel: u8,
    pub notes: usize,
    /// Zero-length notes widened to one tick during export.
    pub widened_notes: usize,
    pub automation_events: usize,
    pub program: Option<u8>,
    /// Index of this track in the SMF (0 is the conductor in format 1).
    pub smf_track: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// One kind of loss during export.
pub struct LossItem {
    pub kind: LossKind,
    pub count: usize,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
/// Data an SMF export cannot preserve.
pub enum LossKind {
    Articulation,
    Keyswitch,
    Microtuning,
    PluginParameter,
    Automation,
    AudioClip,
    ClipEnvelope,
    /// Notes a placement's slip, loop end or trim keeps out of the rendered span.
    ClipWindow,
    SendBus,
    MuteSolo,
    TempoRamp,
    ProgramChange,
}
