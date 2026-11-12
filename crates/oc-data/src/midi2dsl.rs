//! Thin dataset adapter for the engine-owned MIDI importer.
//!
//! Dataset notation keeps source ticks, timing annotations and explicit-release
//! overlapping voices. The application importer owns all SMF parsing, clocks,
//! pitch/program/percussion mapping and event construction. Source tracks remain
//! separate, avoiding the old converter's cross-track channel merge.
use oc_dsl::ast::Project;
use oc_engine::midi::{ImportResult, ImportSummary};
use std::path::Path;

/// Bounded input for dataset and batch conversion (64 MiB per SMF).
pub const MAX_DATASET_MIDI_BYTES: usize = oc_engine::midi::ImportLimits::DEFAULT.max_bytes;

/// Convert a file with the established strict dataset policy.
/// Use `midi_to_import` to inspect the shared importer's repair/loss summary.
pub fn midi_to_project(path: &Path) -> Result<Project, String> {
    let result = midi_to_import(path)?;
    validate_dataset_summary(&result.summary)?;
    Ok(result.project)
}

/// Import one file and expose its repair/loss summary instead of hiding it.
pub fn midi_to_import(path: &Path) -> Result<ImportResult, String> {
    let data = read_midi_bytes(path, MAX_DATASET_MIDI_BYTES)?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned);
    midi_bytes_to_import(&data, name)
}

fn read_midi_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let limits = oc_engine::midi::ImportLimits {
        max_bytes,
        ..Default::default()
    };
    oc_engine::midi::read_smf_file_with_limits(path, &limits).map_err(|error| error.to_string())
}

/// The byte-oriented adapter is useful to dataset jobs that already own input.
pub fn midi_bytes_to_import(data: &[u8], name: Option<String>) -> Result<ImportResult, String> {
    oc_engine::midi::import_smf_dataset(data, name).map_err(|error| error.to_string())
}

/// Strict training-data acceptance is a policy over typed owner summaries;
/// it does not implement another parser or match English warning messages.
pub fn validate_dataset_summary(summary: &ImportSummary) -> Result<(), String> {
    if summary.unterminated_notes != 0 {
        return Err(format!(
            "MIDI has {} unreleased note-ons at track end; refusing fabricated durations",
            summary.unterminated_notes
        ));
    }
    if summary.zero_length_notes != 0 {
        return Err(format!(
            "MIDI has {} zero/negative note durations; refusing fabricated durations",
            summary.zero_length_notes
        ));
    }
    if !summary.warnings.is_empty() {
        return Err(format!(
            "MIDI import requires review: {}",
            summary.warnings.join("; ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
