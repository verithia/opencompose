//! Project-level rendering and exchange: export formats, MIDI import/export,
//! the LV2 render path, patch/placement semantics and SIMD bit identity.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! nine. Each file is still its own module, so test names, assertions and
//! helper modules are unchanged.

#[path = "suite/export_format.rs"]
mod export_format;

#[path = "suite/legacy_effect_shorthand.rs"]
mod legacy_effect_shorthand;

#[path = "suite/lv2.rs"]
mod lv2;

#[path = "suite/lv2_render.rs"]
#[cfg(feature = "plugin-host")]
mod lv2_render;

#[path = "suite/midi_channels.rs"]
mod midi_channels;

#[path = "suite/midi_import_export.rs"]
mod midi_import_export;

#[path = "suite/patch_duplicate_render.rs"]
#[cfg(feature = "plugin-host")]
mod patch_duplicate_render;

#[path = "suite/phrase_variant_placement.rs"]
mod phrase_variant_placement;

#[path = "suite/simd_bit_identity.rs"]
#[cfg(feature = "plugin-host")]
mod simd_bit_identity;
