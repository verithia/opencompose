//! OpenSynth engine core: voice engine, patch/state types, sampler and MIDI events.
//!
//! The public surface is re-exported by the `oc-synth` facade.

// Recorded measurements are kept verbatim at the precision they were captured.
#![allow(clippy::excessive_precision)]

pub mod midi_event;
pub mod note_pitch_curve;
pub mod sampler;
pub mod synth;

pub use midi_event::{MidiEvent, MidiMessage};

#[cfg(test)]
mod test_alloc;
