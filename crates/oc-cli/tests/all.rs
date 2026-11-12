//! Integration cases that share one test binary.
//!
//! `autotests = false` keeps cargo from linking every file under `tests/` as
//! its own executable; the files listed here are modules of this binary.
//! Tests that need their own process (a global allocator, a custom harness)
//! stay declared as separate `[[test]]` targets.
//!
//! Run a subset with the libtest filter:
//! `cargo test -p <crate> --test all -- <name>`.

#[path = "pipeline.rs"]
mod pipeline;
#[path = "plugin_render_exit.rs"]
mod plugin_render_exit;
#[path = "randomize.rs"]
mod randomize;
#[path = "render.rs"]
mod render;

use std::process::Command;

/// The `ocp` under test, with its media and derived caches in a per-process
/// scratch directory. The child is the normal binary, so `cfg(test)` never
/// reaches it; the harness supplies the existing overrides instead.
pub fn ocp() -> Command {
    let root = oc_audio::private_fs::test_root().join("cli-process");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocp"));
    command
        .env("OCP_MEDIA_CACHE_DIR", root.join("media"))
        .env("OCP_DERIVED_CACHE_DIR", root.join("derived"))
        .env(
            "OCP_PLUGIN_SCAN_RECEIPTS",
            root.join("plugin-scan-receipts.json"),
        );
    command
}
