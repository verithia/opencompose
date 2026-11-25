//! Integration cases share one binary; use `cargo test -p oc-dsl --test all -- <filter>`.

#[path = "parse.rs"]
mod parse;

#[path = "semantics.rs"]
mod semantics;

#[path = "search.rs"]
mod search;

#[path = "source_map.rs"]
mod source_map;

#[path = "suite/effect_strings.rs"]
mod effect_strings;

#[path = "suite/midi_routes.rs"]
mod midi_routes;
