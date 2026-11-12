//! Built-in DSP devices — pure Rust, no plugin or engine dependencies.
//!
//! Every device implements `oc_audio::AudioEffect` and processes stereo
//! interleaved f32 in place. The engine keeps the effect factory and maps DSL
//! parameters onto these devices.

// Recorded measurements are kept verbatim at the precision they were captured.
#![allow(clippy::excessive_precision)]

pub use oc_audio::{AudioEffect, AudioProcessContext, AudioProcessInput};

pub mod biquad;
pub mod breath_split;
pub mod chorus;
pub mod compander;
pub mod compressor;
pub mod crossover;
pub mod deesser;
pub mod delay;
pub mod detector;
pub mod distortion;
pub mod doubler;
pub mod ducked_send;
pub mod dynamic_eq;
pub mod eq;
pub mod exciter;
pub mod fet;
pub mod fft;
pub mod flanger;
pub mod gate;
pub mod iron;
pub mod kernel;
mod kweight;
pub mod leveler;
pub mod limiter;
pub mod loudness;
pub mod multiband;
pub mod opto;
mod oversampler;
pub mod passive_eq;
pub mod phaser;
pub mod plosive;
pub mod psola;
pub mod resonance;
pub mod reverb;
pub mod reverb_fdn;
pub mod saturator;
pub use oc_device_schema as schema;
pub mod simd;
pub mod stereo_width;
pub mod tuner;
pub mod vocal_analysis;
pub mod vocal_shift;

#[cfg(all(test, feature = "perf-tests"))]
mod test_support;

#[cfg(test)]
mod golden_references;
