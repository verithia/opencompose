//! The Performance page: the offline renderer's worker setting plus the
//! read-only facts the page reports.
//!
//! The worker count is persisted in the private GUI settings file and published
//! to the engine through `OCP_RENDER_THREADS`; the engine reads that variable at
//! the start of every offline render, so a change applies to the next render.

use serde::Serialize;
use tauri::State;

use crate::first_block::{FIRST_BLOCK_BUDGET_MS, FirstBlockSnapshot};
use crate::gui_settings::{self, GuiSettings};
use crate::render_thread::{CHUNK_FRAMES, FIRST_CHUNK_FRAMES, LOOKAHEAD_FRAMES};
use crate::state::AppState;

/// Versions read from the build and the DSL crate, never written here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub version: String,
    pub dsl_version: String,
    pub source_revision: String,
    pub build_profile: String,
    pub os: String,
    pub arch: String,
}

/// What the Help page reports about this build.
#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        dsl_version: oc_dsl::DSL_VERSION.to_string(),
        source_revision: env!("OCP_BUILD_REVISION").into(),
        build_profile: env!("OCP_BUILD_PROFILE").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
    }
}

/// The render-performance setting and the facts beside it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerformanceFacts {
    pub app_version: String,
    pub dsl_version: String,
    /// Cores the machine reports.
    pub cores: usize,
    /// Requested worker count; `null` follows the machine.
    pub render_threads: Option<u32>,
    pub parallel_tracks: bool,
    /// Workers the next offline render uses.
    pub effective_workers: usize,
    /// Frames the producer may queue ahead of the device.
    pub lookahead_frames: usize,
    /// Frames in the short first chunk of a playback generation.
    pub first_chunk_frames: usize,
    /// Frames per steady-state render chunk.
    pub block_frames: usize,
    /// Rate the current output device resolved to.
    pub sample_rate: u32,
    /// The contract a start path is measured against.
    pub first_block_budget_ms: f64,
    /// The last measured first block, when anything has started audio.
    pub first_block: Option<FirstBlockSnapshot>,
}

fn facts(state: &AppState, settings: &GuiSettings) -> PerformanceFacts {
    let (sample_rate, first_block) = {
        let inner = state.lock_inner();
        (inner.audio.sample_rate, inner.audio.first_block_readout())
    };
    PerformanceFacts {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        dsl_version: oc_dsl::DSL_VERSION.to_string(),
        cores: gui_settings::machine_parallelism(),
        render_threads: settings.render_threads,
        parallel_tracks: settings.parallel_tracks,
        effective_workers: settings.effective_workers(),
        lookahead_frames: LOOKAHEAD_FRAMES,
        first_chunk_frames: FIRST_CHUNK_FRAMES,
        block_frames: CHUNK_FRAMES,
        sample_rate,
        first_block_budget_ms: FIRST_BLOCK_BUDGET_MS,
        first_block,
    }
}

/// The current facts; reading them never changes a setting.
#[tauri::command]
pub fn performance_facts(state: State<AppState>) -> PerformanceFacts {
    facts(&state, &gui_settings::load())
}

/// Persist settings; subsequent render requests snapshot them independently.
#[tauri::command]
pub fn set_performance_settings(
    settings: GuiSettings,
    state: State<AppState>,
) -> Result<PerformanceFacts, String> {
    let settings = settings.sanitized();
    gui_settings::save(&settings)?;
    Ok(facts(&state, &settings))
}

#[cfg(test)]
mod build_identity_tests {
    #[test]
    fn about_reports_the_compiled_build_identity() {
        let info = super::app_info();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.dsl_version, oc_dsl::DSL_VERSION);
        assert_eq!(info.source_revision, env!("OCP_BUILD_REVISION"));
        assert_eq!(info.build_profile, env!("OCP_BUILD_PROFILE"));
        assert_eq!(info.os, std::env::consts::OS);
        assert_eq!(info.arch, std::env::consts::ARCH);
        assert!(!info.source_revision.is_empty());
    }
}
