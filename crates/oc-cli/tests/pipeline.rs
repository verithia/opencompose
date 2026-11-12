//! CLI asset pipelines: MIDI import, SoundFont rendering and the vocal
//! analysis/tuning commands.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! four. Each file is still its own module, so test names, assertions and
//! `CARGO_BIN_EXE_ocp` are unchanged.

#[path = "suite/level_bake.rs"]
mod level_bake;

#[path = "suite/midi_pipeline.rs"]
mod midi_pipeline;

#[path = "suite/soundfont_refusal.rs"]
mod soundfont_refusal;

#[path = "suite/soundfont_render.rs"]
mod soundfont_render;

#[path = "suite/vocal_analyze.rs"]
mod vocal_analyze;

#[path = "suite/vocal_apply.rs"]
mod vocal_apply;
