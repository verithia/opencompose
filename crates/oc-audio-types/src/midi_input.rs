//! Bounded, block-relative MIDI input for audio processors.

/// One MIDI 1 channel message. Channels are zero-based; data bytes are seven-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiInputMessage {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        key: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    PitchBend {
        channel: u8,
        value: i16,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    PolyPressure {
        channel: u8,
        key: u8,
        pressure: u8,
    },
}

impl MidiInputMessage {
    /// Validate without clamping a malformed message into another channel or key.
    pub fn bytes(self) -> Result<[u8; 3], MidiInputError> {
        let (status, channel, a, b) = match self {
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => (0x90, channel, key, velocity),
            Self::NoteOff { channel, key } => (0x80, channel, key, 0),
            Self::ControlChange {
                channel,
                controller,
                value,
            } => (0xb0, channel, controller, value),
            Self::ProgramChange { channel, program } => (0xc0, channel, program, 0),
            Self::PitchBend { channel, value } => {
                if !(-8192..=8191).contains(&value) {
                    return Err(MidiInputError::InvalidMessage);
                }
                let value = (i32::from(value) + 8192) as u16;
                (0xe0, channel, (value & 127) as u8, (value >> 7) as u8)
            }
            Self::ChannelPressure { channel, pressure } => (0xd0, channel, pressure, 0),
            Self::PolyPressure {
                channel,
                key,
                pressure,
            } => (0xa0, channel, key, pressure),
        };
        if channel > 15 || a > 127 || b > 127 {
            return Err(MidiInputError::InvalidMessage);
        }
        Ok([status | channel, a, b])
    }
}

/// A message for the next processing call, at a sample offset inside that call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiInputEvent {
    pub sample_offset: u32,
    pub message: MidiInputMessage,
}

/// A processor refuses input before it writes beyond its prepared event storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiInputError {
    Unsupported,
    ProcessorFailed,
    CapacityExceeded,
    InvalidMessage,
    InvalidOffset,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_preserve_extremes_and_refuse_out_of_range_values() {
        assert_eq!(
            MidiInputMessage::NoteOn {
                channel: 15,
                key: 127,
                velocity: 127
            }
            .bytes(),
            Ok([0x9f, 127, 127])
        );
        for (value, bytes) in [
            (-8192, [0xe2, 0, 0]),
            (0, [0xe2, 0, 64]),
            (8191, [0xe2, 127, 127]),
        ] {
            assert_eq!(
                MidiInputMessage::PitchBend { channel: 2, value }.bytes(),
                Ok(bytes)
            );
        }
        for message in [
            MidiInputMessage::NoteOff {
                channel: 16,
                key: 60,
            },
            MidiInputMessage::NoteOn {
                channel: 0,
                key: 128,
                velocity: 64,
            },
            MidiInputMessage::PitchBend {
                channel: 0,
                value: 8192,
            },
            MidiInputMessage::ControlChange {
                channel: 0,
                controller: 7,
                value: 128,
            },
        ] {
            assert_eq!(message.bytes(), Err(MidiInputError::InvalidMessage));
        }
    }
}
