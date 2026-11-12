//! Built-in device behaviour: the dynamic EQ, the limiter and the rack FX laws
//! the measurement harnesses in `docs/` cite.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! seven. Each file is still its own module, so test names, assertions and the
//! `#[ignore]`d harnesses are unchanged.

#[path = "suite/device_preset_controls.rs"]
#[cfg(feature = "plugin-host")]
mod device_preset_controls;

#[path = "suite/dynamic_eq_loudness.rs"]
mod dynamic_eq_loudness;

#[path = "suite/dynamic_eq_render.rs"]
#[cfg(feature = "plugin-host")]
mod dynamic_eq_render;

#[path = "suite/fx_law_probe.rs"]
#[cfg(feature = "plugin-host")]
mod fx_law_probe;

#[path = "suite/fx_neutrality.rs"]
#[cfg(feature = "plugin-host")]
mod fx_neutrality;

#[path = "suite/fx_onset_probe.rs"]
#[cfg(feature = "plugin-host")]
mod fx_onset_probe;

#[path = "suite/fx_split_render.rs"]
mod fx_split_render;

#[path = "suite/limiter_master.rs"]
#[cfg(feature = "plugin-host")]
mod limiter_master;
