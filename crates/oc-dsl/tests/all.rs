//! Integration cases share one binary; use `cargo test -p oc-dsl --test all -- <filter>`.

#[path = "parse.rs"]
mod parse;

#[path = "semantics.rs"]
mod semantics;

#[path = "source_map.rs"]
mod source_map;
