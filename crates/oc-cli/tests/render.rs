//! CLI rendering: audio clips, clip alignment, export formats, render format
//! selection, effect parameter preflight and SIMD bit identity.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! five. Each file is still its own module, so test names, assertions and
//! `CARGO_BIN_EXE_ocp` are unchanged.

#[path = "suite/audio_clip_render.rs"]
mod audio_clip_render;

#[path = "suite/clip_align.rs"]
mod clip_align;

#[path = "suite/default_instruments.rs"]
mod default_instruments;

#[path = "suite/effect_params.rs"]
mod effect_params;

#[path = "suite/export_formats.rs"]
mod export_formats;

#[path = "suite/render_format.rs"]
mod render_format;

#[path = "suite/project_snapshots.rs"]
mod project_snapshots;

#[path = "suite/simd_identity.rs"]
mod simd_identity;
