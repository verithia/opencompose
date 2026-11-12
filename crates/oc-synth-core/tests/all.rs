//! Integration cases share one binary; use `cargo test -p oc-synth-core --test all -- <filter>`.

#[path = "synth_catalog.rs"]
mod synth_catalog;

#[path = "amp_damping.rs"]
mod amp_damping;
