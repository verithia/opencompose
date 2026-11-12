//! Tauri commands — the bridge between frontend and Rust core.
//!
//! Command handlers are grouped by domain; this module re-exports the public
//! surface so callers keep using `commands::<name>`.

mod ai;
mod arrangement;
pub(crate) mod audition_scratch;
mod automation;
mod automation_clips;
mod bounce;
mod control_links;
pub(crate) mod freesound_oauth;
mod fx;
mod groups;
mod handoff;
mod markers;
mod mixer;
mod network;
pub(crate) mod notes;
mod patch;
mod patch_bundle;
mod performance;
mod picture;
mod plugin_catalog;
pub(crate) mod plugin_host;
mod preferences;
pub(crate) mod preview;
mod project;
mod render;
mod sample_market;
mod sections;
mod sessions;
pub(crate) mod shared;
mod soundfont;
mod storage;
mod synth;
mod touch;
mod transport;
mod workspace;

#[cfg(test)]
mod tests;

pub use ai::*;
pub use arrangement::*;
pub use automation::*;
pub use automation_clips::*;
pub use bounce::*;
pub use control_links::*;
pub use fx::*;
pub use groups::*;
pub use handoff::*;
pub use markers::*;
pub use mixer::*;
pub use network::*;
pub use notes::*;
pub use patch::*;
pub use patch_bundle::*;
pub use performance::*;
pub use picture::*;
pub use plugin_catalog::*;
pub use plugin_host::*;
pub use preferences::*;
pub use preview::*;
pub use project::*;
pub use render::*;
pub use sample_market::*;
pub use sections::*;
pub use sessions::*;
pub use shared::*;
pub use soundfont::*;
pub use storage::*;
pub use synth::*;
pub use touch::*;
pub use transport::*;
pub use workspace::*;
