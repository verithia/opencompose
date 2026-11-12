//! The beat grid a range render was made at, written beside the WAV.
//!
//! `analyze_notes` receives a path, not a project, so a `render_range` output
//! carries its project's opening tempo and meter in a `<name>.wav.grid.json`
//! sidecar. A missing or unreadable sidecar is not an error: the tool falls
//! back to explicit arguments and finally to the engine default, with a
//! warning in each case.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SUFFIX: &str = ".grid.json";

/// What one render assumed. `tempo_changes`/`meter_changes` are counts, not
/// maps: the analysis grid is constant, so a map can only be reported as
/// unrepresentable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RenderGrid {
    pub tempo_bpm: f64,
    pub time_signature: String,
    #[serde(default)]
    pub tempo_changes: usize,
    #[serde(default)]
    pub meter_changes: usize,
}

impl RenderGrid {
    pub(crate) fn from_project(project: &oc_dsl::ast::Project) -> Self {
        Self {
            tempo_bpm: project.properties.tempo,
            time_signature: format!(
                "{}/{}",
                project.properties.time_signature.numerator,
                project.properties.time_signature.denominator
            ),
            tempo_changes: project.properties.tempo_map.len(),
            meter_changes: project.properties.time_sig_map.len(),
        }
    }
}

/// `part.wav` → `part.wav.grid.json`.
pub(crate) fn sidecar_path(wav: &Path) -> PathBuf {
    let mut name = wav.as_os_str().to_os_string();
    name.push(SUFFIX);
    PathBuf::from(name)
}

/// Best effort: a render must not fail because its metadata could not be
/// written, and analysis then warns instead of guessing silently.
pub(crate) fn write(wav: &Path, grid: &RenderGrid) {
    let Ok(json) = serde_json::to_vec_pretty(grid) else {
        return;
    };
    let _ = std::fs::write(sidecar_path(wav), json);
}

pub(crate) fn read(wav: &Path) -> Option<RenderGrid> {
    let text = std::fs::read_to_string(sidecar_path(wav)).ok()?;
    serde_json::from_str(&text).ok()
}
