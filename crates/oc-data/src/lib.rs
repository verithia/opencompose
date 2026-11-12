//! # OpenCompose Data Pipeline
//!
//! Tools for generating batch data: (DSL text, audio WAV) pairs.
//!
//! ## Pipeline
//! ```text
//! MIDI files → midi2dsl → AST → serializer → .ocp files
//!                                    ↓
//!                              engine render → .wav files
//!                                    ↓
//!                         (DSL text, WAV audio) pairs → batch dataset
//! ```

pub mod generate_midi;
pub mod midi2dsl;
pub mod pipeline;
