//! SoundFont 2 player: parsing, generators, modulation, pressure, render paths
//! and the send table.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! seven. Each file remains its own module, preserving test names/assertions.
//! The all.rs parent owns the shared builders and analysis helpers once; the
//! sample fixture stays unchanged.

#[path = "suite/sf2_generators.rs"]
mod sf2_generators;

#[path = "suite/sf2_modulation.rs"]
mod sf2_modulation;

#[path = "suite/sf2_modulation_env.rs"]
mod sf2_modulation_env;

#[path = "suite/sf2_modulators.rs"]
mod sf2_modulators;

#[path = "suite/sf2_parse.rs"]
mod sf2_parse;

#[path = "suite/sf2_pressure.rs"]
mod sf2_pressure;

#[path = "suite/sf2_render.rs"]
mod sf2_render;

#[path = "suite/sf2_sends.rs"]
mod sf2_sends;

#[path = "suite/sf3.rs"]
mod sf3;
