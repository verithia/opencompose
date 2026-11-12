//! Live MIDI input/output for controllers, DAW transport, and external gear.
//!
//! ## Threading
//!
//! Backends deliver input on their own callback thread into a preallocated ring.
//! A MIDI thread drains that ring and pushes `LiveEvent`s into the audio
//! queue (`queue::channel`); the audio callback only pops a bounded number of
//! events and applies them, so it never blocks or allocates. Output events are
//! sent from the MIDI thread, where device I/O is allowed to block.
//!
//! ## Backends
//!
//! `midir_backend::MidirBackend` covers CoreMIDI, WinMM, and ALSA.
//! `fake::FakeBackend` provides deterministic ports for tests and monitoring
//! without hardware.

pub mod backend;
pub mod clock;
pub mod error;
pub mod fake;
pub mod message;
pub mod midir_backend;
pub mod queue;
pub mod scheduler;
pub mod session;

pub use backend::{
    InputId, MidiBackend, MidiInputEvent, MidiPortInfo, OutputId, PortDirection, find_port,
};
pub use clock::{MidiClock, PPQN};
pub use error::MidiError;
pub use fake::FakeBackend;
pub use message::{LiveEvent, RawMidi, encode_engine_message};
pub use midir_backend::MidirBackend;
pub use queue::{LiveEventConsumer, LiveEventProducer, LiveEventTarget, channel as event_queue};
pub use scheduler::OutputScheduler;
pub use session::MidiSession;
