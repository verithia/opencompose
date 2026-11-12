//! Third-party `.ffp` preset import: parsing the stored format and the vendor
//! catalogue wiring.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! two. Each file is still its own module, so test names, assertions and the
//! byte fixtures stay unchanged.

#[path = "suite/plugin_preset_import.rs"]
mod plugin_preset_import;

#[path = "suite/plugin_preset_vendor.rs"]
mod plugin_preset_vendor;
