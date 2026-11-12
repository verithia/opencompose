//! Shared bounded state-file I/O, available without a native plugin backend.

pub use oc_audio::bounded_file::{
    BoundedRegularFile as StateFile, MAX_PLUGIN_STATE_BYTES, read_regular_state_file,
    read_state_bytes,
};
