//! Parsing: the shipped examples, the articulation tokens, the fractional
//! position round-trip and the production example validation.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! four. Each file is still its own module, so test names, assertions and the
//! `CARGO_MANIFEST_DIR` paths to `examples/` are unchanged.

#[path = "suite/articulation_tokens.rs"]
mod articulation_tokens;

#[path = "suite/fractional_position_roundtrip.rs"]
mod fractional_position_roundtrip;

#[path = "suite/gliss.rs"]
mod gliss;

#[path = "suite/markers.rs"]
mod markers;

#[path = "suite/parse_examples.rs"]
mod parse_examples;

#[path = "suite/production_examples_validate.rs"]
mod production_examples_validate;
