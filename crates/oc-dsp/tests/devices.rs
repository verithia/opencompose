//! Built-in devices: per-device CPU, effect dispatch and the EQ response
//! fixture.
//!
//! The files live under `suite/` so cargo links one test executable instead of
//! three. Each file is still its own module, so test names, assertions and the
//! `CARGO_MANIFEST_DIR` fixture path are unchanged.

#[path = "suite/device_cpu.rs"]
mod device_cpu;

#[path = "suite/dispatch.rs"]
mod dispatch;

#[path = "suite/eq_response_fixture.rs"]
mod eq_response_fixture;
