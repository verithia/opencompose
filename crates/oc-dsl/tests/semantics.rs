//! Language semantics: audio clips, control links, mixer routing, MPE and the
//! patch lifecycle.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! six. Each file is still its own module, so test names, assertions and
//! helper functions are unchanged.

#[path = "suite/audio_clip.rs"]
mod audio_clip;

#[path = "suite/clip_content.rs"]
mod clip_content;

#[path = "suite/control_links.rs"]
mod control_links;

#[path = "suite/dynamics.rs"]
mod dynamics;

#[path = "suite/groups.rs"]
mod groups;

#[path = "suite/mixer_routing.rs"]
mod mixer_routing;

#[path = "suite/mpe.rs"]
mod mpe;

#[path = "suite/patch_lifecycle.rs"]
mod patch_lifecycle;
