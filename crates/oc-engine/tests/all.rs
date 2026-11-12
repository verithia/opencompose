//! Integration cases that share one test binary.
//!
//! `autotests = false` keeps cargo from linking every file under `tests/` as
//! its own executable; the files listed here are modules of this binary.
//! The shared allocator counts heap activity only on the thread inside each
//! measured scope, so ordinary and allocation regressions run together.
//!
//! Run a subset with the libtest filter:
//! `cargo test -p oc-engine --test all -- <name>`.

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

// Helpers are instantiated once per aggregate binary. In particular, the
// cache initializer's Once must guard the process-wide caches across modules.
#[path = "support/cache.rs"]
mod cache_support;
#[path = "suite/sf2_analysis/mod.rs"]
mod sf2_analysis;
#[path = "suite/sf2_builder/mod.rs"]
mod sf2_builder;
#[path = "suite/sf2_spectral/mod.rs"]
mod sf2_spectral;

/// The render-warning buffer is process-wide, so cases that clear or drain it
/// take turns instead of stealing each other's warnings.
pub(crate) static RENDER_WARNINGS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[path = "arrangement.rs"]
#[cfg(feature = "plugin-host")]
mod arrangement;
#[path = "audio_clip.rs"]
mod audio_clip;
#[path = "control_route.rs"]
mod control_route;
#[path = "drum_kit.rs"]
mod drum_kit;
#[path = "dynamics.rs"]
#[cfg(feature = "plugin-host")]
mod dynamics;
#[path = "fx.rs"]
mod fx;
#[path = "mixdown.rs"]
mod mixdown;
#[path = "pedal.rs"]
mod pedal;
#[path = "sf2.rs"]
mod sf2;
#[path = "sf2_engine.rs"]
mod sf2_engine;
#[path = "soundfont_resolution.rs"]
#[cfg(feature = "plugin-host")]
mod soundfont_resolution;
#[path = "synth.rs"]
mod synth;
#[path = "vocal_harmony.rs"]
mod vocal_harmony;
#[path = "vocal_snap.rs"]
mod vocal_snap;
#[path = "vocal_timing.rs"]
mod vocal_timing;
#[path = "vocal_tune_shift.rs"]
mod vocal_tune_shift;

#[path = "clip_lane_alloc.rs"]
mod clip_lane_alloc;
#[path = "clips.rs"]
mod clips;
#[path = "control_source_alloc.rs"]
mod control_source_alloc;
#[path = "deesser_alloc.rs"]
mod deesser_alloc;
#[path = "dynamic_eq_alloc.rs"]
mod dynamic_eq_alloc;
#[path = "multiband_alloc.rs"]
mod multiband_alloc;
#[path = "reverb_fdn_alloc.rs"]
mod reverb_fdn_alloc;
#[path = "sf2_sends_alloc.rs"]
mod sf2_sends_alloc;
#[path = "spectrum_alloc.rs"]
mod spectrum_alloc;
#[path = "vocal_tune.rs"]
mod vocal_tune;
