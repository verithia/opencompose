//! Audio clips: decoding and stretching, the streaming clip lane, take
//! alignment and touch overrides.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! five. Each file is still its own module, so test names and assertions are
//! unchanged; the timing harness is behind the `perf-tests` feature.

#[path = "suite/audio_clip_perf.rs"]
mod audio_clip_perf;

#[path = "suite/audio_clip_stretch.rs"]
mod audio_clip_stretch;

#[path = "suite/clip_content.rs"]
mod clip_content;

#[path = "suite/clip_lane.rs"]
mod clip_lane;

#[path = "suite/pattern_transpose.rs"]
mod pattern_transpose;

#[path = "suite/take_alignment.rs"]
mod take_alignment;

#[path = "suite/touch_override.rs"]
mod touch_override;
