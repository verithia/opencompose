//! Typed MIDI errors. Backends must map platform failures here, never panic.

use std::fmt;

use thiserror::Error;

use super::backend::PortDirection;

/// Every MIDI I/O failure the engine can report to a caller.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MidiError {
    #[error("MIDI support is unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("MIDI {direction} port '{name}' was not found")]
    PortNotFound {
        direction: PortDirection,
        name: String,
    },
    #[error("MIDI {direction} port '{name}' is already open")]
    AlreadyOpen {
        direction: PortDirection,
        name: String,
    },
    #[error("permission denied for MIDI {direction} port '{name}'")]
    PermissionDenied {
        direction: PortDirection,
        name: String,
    },
    #[error("MIDI {direction} port '{name}' disconnected")]
    Disconnected {
        direction: PortDirection,
        name: String,
    },
    #[error("MIDI input buffer overflow on '{name}'")]
    InputOverflow { name: String },
    #[error("invalid MIDI message: {detail}")]
    InvalidMessage { detail: String },
    #[error("MIDI {direction} failure on '{name}': {detail}")]
    Io {
        direction: PortDirection,
        name: String,
        detail: String,
    },
}

impl MidiError {
    pub(crate) fn unavailable(detail: impl fmt::Display) -> Self {
        Self::Unavailable {
            detail: detail.to_string(),
        }
    }

    pub(crate) fn invalid(detail: impl fmt::Display) -> Self {
        Self::InvalidMessage {
            detail: detail.to_string(),
        }
    }

    /// Classify platform connect errors without relying on backend-specific types.
    pub(crate) fn connect(direction: PortDirection, name: &str, detail: &str) -> Self {
        let lower = detail.to_ascii_lowercase();
        if lower.contains("permission")
            || lower.contains("denied")
            || lower.contains("not authorized")
            || lower.contains("access")
        {
            Self::PermissionDenied {
                direction,
                name: name.to_string(),
            }
        } else {
            Self::Io {
                direction,
                name: name.to_string(),
                detail: detail.to_string(),
            }
        }
    }
}

impl PortDirection {
    /// Wire label for the direction.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

impl fmt::Display for PortDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
