//! Feature pins that keep the CLI and GUI builds on one artifact set.
//!
//! The GUI's Tauri dependencies enable extra features on crates the CLI shares
//! with it (`log/kv_unstable` from `tauri-plugin-log`, `objc2/exception` from
//! the AppKit bindings, `syn/fold` from `tauri-build`, and so on). Cargo keys
//! artifacts by resolved features, so without these pins alternating
//! `-p oc-cli` and `-p oc-gui` rebuilt 73 units because everything above `log`
//! changed. Both front-ends depend on this crate so the two resolutions match.
//! It is manifest only: nothing here is used at run time, and adding or
//! removing a pin changes no behavior.
