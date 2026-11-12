//! # OpenCompose Engine
//!
//! Audio rendering, DSP effects, mix graph, and plugin hosting.
//!
//! ## Architecture
//!
//! ```text
//! AST ──► Interpreter ──► events ──► independent source adapters ──► audio
//!                                                                  │
//! DSL effects ──► AudioGraph (tracks → effects → buses → master) ◄─┘
//!                         │
//!                         ▼
//!                    Final stereo mix ──► cpal output / WAV file
//! ```
//!
//! ## Modules
//!
//! - `interpret`: AST → timed MIDI events
//! - `soundfont`: SoundFont track rendering through the in-crate player
//! - `sf2`: in-crate SoundFont 2 bank reader and sample player
//! - `block_source`: stateful native/CLAP/VST2/VST3/LV2/AU/SoundFont playback sources
//!   (`plugin-host`)
//! - `render`: complete offline pipeline, AST → WAV (`plugin-host`)
//! - `dsp`: built-in effects (EQ, compressor, reverb, delay, limiter, chorus)
//! - `graph`: audio routing graph (tracks → buses → master with effect chains)
//! - `midi_io`: live MIDI input/output (CoreMIDI/WinMM/ALSA) and clock
//! - `midi`: Standard MIDI File import/export through the application pipeline
//!
//! ## Crate boundaries
//!
//! Devices live in `oc-dsp`, instruments and sample playback in `oc-synth`, the
//! plugin host in `oc-host`, and the shared trait/types in `oc-audio`.
//!
//! ## Example
//!
//! Rendering a project to a WAV file, with the export settings the project
//! declares:
//!
//! ```no_run
//! use std::path::Path;
//!
//! let source = r#"@project "Example"
//!   tempo: 120
//!
//! @track piano | instrument: piano/acoustic_grand
//!   @phrase intro [1:1 - 3:1]
//!     C4 q mf
//!     E4 q mf
//! "#;
//! let project = oc_dsl::parse(source).expect("the example project parses");
//! let spec = oc_engine::encode::EncodeSpec::for_project(&project, 48_000);
//! oc_engine::render::render_to_wav(
//!     &project,
//!     Path::new("soundfonts/FluidR3Mono_GM.sf3"),
//!     Path::new("/tmp/example.wav"),
//!     spec,
//! )
//! .expect("the render succeeds");
//! ```

pub mod align;
pub mod analysis;
pub mod audio_clip;
pub mod audio_peaks;
pub mod automation;
#[cfg(feature = "plugin-host")]
pub mod block_source;
pub mod clip_lane;
mod clip_pcm;
pub mod control_link;
pub mod derived_cache;
/// Engine DSP building blocks.
pub mod dsp;
/// Audio file encoding.
pub mod encode;
/// Track, bus and master mixing graph.
pub mod graph;
pub mod idle_gate;
pub mod interpret;
#[cfg(feature = "plugin-host")]
pub mod leveler_bake;
#[cfg(feature = "plugin-host")]
pub mod loudness;
pub mod media_cache;
/// MIDI messages and performance types.
pub mod midi;
/// MIDI input and output sessions.
pub mod midi_io;
pub mod mpe;
pub mod note_analysis;
pub mod performance_send;
pub mod phase_vocoder;
pub mod playability;
/// Instrument and effect preset handling.
pub mod preset;
#[cfg(feature = "plugin-host")]
mod processing_failure;
pub mod project_file;
#[cfg(feature = "plugin-host")]
pub mod render;
#[cfg(feature = "plugin-host")]
pub mod render_cache;
pub mod render_cancel;
pub mod render_control;
pub mod render_parallel;
pub mod render_warnings;
pub mod resample;
/// SoundFont 2 synthesis.
pub mod sf2;
pub mod smoothing;
/// SoundFont asset loading.
pub mod soundfont;
pub mod source_lanes;
pub mod spectrum;
pub mod stage_log;
pub mod stems;
pub mod stretch;
pub mod touch;
pub mod vocal_analysis;
pub mod vocal_harmony;
pub mod vocal_snap;
pub mod vocal_tune;
pub mod wav;

#[cfg(test)]
mod test_alloc;
