//! Synth voices and audio-derived control: voice events, the pitch curve, the
//! control follower, normalization, FX expansion and the timing harnesses.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! eight. Each file is still its own module, so test names and assertions are
//! unchanged; the timing harnesses are behind the `perf-tests` feature.

#[path = "suite/bass_sub_h4_probe.rs"]
mod bass_sub_h4_probe;

#[path = "suite/control_source.rs"]
mod control_source;

#[path = "suite/default_instruments.rs"]
#[cfg(feature = "plugin-host")]
mod default_instruments;

#[path = "suite/gliss.rs"]
mod gliss;

#[path = "suite/native_voice_events.rs"]
mod native_voice_events;

#[path = "suite/note_pitch_curve.rs"]
mod note_pitch_curve;

#[path = "suite/range_window_perf.rs"]
#[cfg(feature = "plugin-host")]
mod range_window_perf;

#[path = "suite/synth_fx_expansion.rs"]
mod synth_fx_expansion;

#[path = "suite/synth_normalization.rs"]
mod synth_normalization;

#[path = "suite/synth_perf.rs"]
mod synth_perf;
