//! Integration cases that share one test binary.
//!
//! `autotests = false` keeps cargo from linking every file under `tests/` as
//! its own executable; the files listed here are modules of this binary.
//! Tests that need their own process (a global allocator, a custom harness)
//! stay declared as separate `[[test]]` targets.
//!
//! Run a subset with the libtest filter:
//! `cargo test -p <crate> --test all -- <name>`.

#[path = "checks.rs"]
mod checks;
