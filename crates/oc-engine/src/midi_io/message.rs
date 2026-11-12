//! Wire-level MIDI parsing and packing, plus conversion to engine messages.
//!
//! `LiveEvent` is `Copy` and fixed-size so it can live in the audio-thread queue
//! without heap traffic. Timestamps are kept out of the hot path.

use crate::interpret::MidiMessage;

use super::error::MidiError;

/// A received or transmitted realtime/channel MIDI message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEvent {
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    PolyPressure {
        channel: u8,
        note: u8,
        pressure: u8,
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
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    PitchBend {
        channel: u8,
        value: i16,
    },
    Clock,
    Start,
    Continue,
    Stop,
}

impl Default for LiveEvent {
    fn default() -> Self {
        Self::NoteOff {
            channel: 0,
            note: 0,
            velocity: 0,
        }
    }
}

/// Raw bytes as delivered by the backend, capped at one three-byte message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawMidi {
    bytes: [u8; 3],
    len: u8,
}

impl RawMidi {
    /// Parse up to three raw MIDI bytes.
    pub fn new(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > 3 {
            return None;
        }
        let mut raw = Self {
            bytes: [0; 3],
            len: bytes.len() as u8,
        };
        raw.bytes[..bytes.len()].copy_from_slice(bytes);
        Some(raw)
    }

    /// The raw bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl LiveEvent {
    /// Parse one complete channel or realtime message. Unsupported/system data
    /// messages are rejected so callers can drop them without panicking.
    pub fn parse(bytes: &[u8]) -> Result<Self, MidiError> {
        let Some(&status) = bytes.first() else {
            return Err(MidiError::invalid("empty MIDI message"));
        };
        let data = |index: usize, name: &str| -> Result<u8, MidiError> {
            let value = *bytes
                .get(index)
                .ok_or_else(|| MidiError::invalid(format!("truncated {name} message")))?;
            if value > 127 {
                return Err(MidiError::invalid(format!("{name} data byte out of range")));
            }
            Ok(value)
        };
        if status >= 0xF0 {
            return match status {
                0xF8 => Ok(Self::Clock),
                0xFA => Ok(Self::Start),
                0xFB => Ok(Self::Continue),
                0xFC => Ok(Self::Stop),
                _ => Err(MidiError::invalid(format!(
                    "unsupported system message 0x{status:02X}"
                ))),
            };
        }
        let channel = status & 0x0F;
        match status & 0xF0 {
            0x80 => Ok(Self::NoteOff {
                channel,
                note: data(1, "note off")?,
                velocity: data(2, "note off")?,
            }),
            0x90 => Ok(Self::NoteOn {
                channel,
                note: data(1, "note on")?,
                velocity: data(2, "note on")?,
            }),
            0xA0 => Ok(Self::PolyPressure {
                channel,
                note: data(1, "poly pressure")?,
                pressure: data(2, "poly pressure")?,
            }),
            0xB0 => Ok(Self::ControlChange {
                channel,
                controller: data(1, "control change")?,
                value: data(2, "control change")?,
            }),
            0xC0 => Ok(Self::ProgramChange {
                channel,
                program: data(1, "program change")?,
            }),
            0xD0 => Ok(Self::ChannelPressure {
                channel,
                pressure: data(1, "channel pressure")?,
            }),
            0xE0 => {
                let lsb = data(1, "pitch bend")?;
                let msb = data(2, "pitch bend")?;
                Ok(Self::PitchBend {
                    channel,
                    value: ((u16::from(msb) << 7) | u16::from(lsb)) as i16 - 8192,
                })
            }
            _ => Err(MidiError::invalid(format!("unknown status 0x{status:02X}"))),
        }
    }

    /// Write the wire bytes and return their length (1-3).
    pub fn pack_into(self, out: &mut [u8; 3]) -> usize {
        let channel = |ch: u8| 0x0F & ch.min(15);
        let data = |value: u8| 0x7F & value;
        match self {
            Self::NoteOff {
                channel: ch,
                note,
                velocity,
            } => {
                out[0] = 0x80 | channel(ch);
                out[1] = data(note);
                out[2] = data(velocity);
                3
            }
            Self::NoteOn {
                channel: ch,
                note,
                velocity,
            } => {
                out[0] = 0x90 | channel(ch);
                out[1] = data(note);
                out[2] = data(velocity);
                3
            }
            Self::PolyPressure {
                channel: ch,
                note,
                pressure,
            } => {
                out[0] = 0xA0 | channel(ch);
                out[1] = data(note);
                out[2] = data(pressure);
                3
            }
            Self::ControlChange {
                channel: ch,
                controller,
                value,
            } => {
                out[0] = 0xB0 | channel(ch);
                out[1] = data(controller);
                out[2] = data(value);
                3
            }
            Self::ProgramChange {
                channel: ch,
                program,
            } => {
                out[0] = 0xC0 | channel(ch);
                out[1] = data(program);
                2
            }
            Self::ChannelPressure {
                channel: ch,
                pressure,
            } => {
                out[0] = 0xD0 | channel(ch);
                out[1] = data(pressure);
                2
            }
            Self::PitchBend { channel: ch, value } => {
                let raw = (i32::from(value) + 8192).clamp(0, 16383) as u16;
                out[0] = 0xE0 | channel(ch);
                out[1] = (raw & 0x7F) as u8;
                out[2] = ((raw >> 7) & 0x7F) as u8;
                3
            }
            Self::Clock => {
                out[0] = 0xF8;
                1
            }
            Self::Start => {
                out[0] = 0xFA;
                1
            }
            Self::Continue => {
                out[0] = 0xFB;
                1
            }
            Self::Stop => {
                out[0] = 0xFC;
                1
            }
        }
    }

    /// Convert to the engine's event type. Clock/transport have no engine event;
    /// note-on with velocity zero becomes note-off.
    pub fn to_engine_message(self) -> Option<MidiMessage> {
        match self {
            Self::NoteOff {
                channel,
                note,
                velocity,
            } => {
                let _ = velocity;
                Some(MidiMessage::NoteOff { channel, note })
            }
            Self::NoteOn {
                channel,
                note,
                velocity: 0,
            } => Some(MidiMessage::NoteOff { channel, note }),
            Self::NoteOn {
                channel,
                note,
                velocity,
            } => Some(MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            }),
            Self::PolyPressure {
                channel,
                note,
                pressure,
            } => Some(MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            }),
            Self::ControlChange {
                channel,
                controller,
                value,
            } => Some(MidiMessage::ControlChange {
                channel,
                controller,
                value,
            }),
            Self::ProgramChange { channel, program } => {
                Some(MidiMessage::ProgramChange { channel, program })
            }
            Self::ChannelPressure { channel, pressure } => {
                Some(MidiMessage::ChannelPressure { channel, pressure })
            }
            Self::PitchBend { channel, value } => Some(MidiMessage::PitchBend { channel, value }),
            Self::Clock | Self::Start | Self::Continue | Self::Stop => None,
        }
    }
}

/// Encode an engine message into at most two wire events (14-bit CC).
/// Returns 0 for messages that have no MIDI wire representation.
pub fn encode_engine_message(message: &MidiMessage, out: &mut [LiveEvent; 2]) -> usize {
    use MidiMessage::*;
    let event = match message {
        NoteOn {
            channel,
            note,
            velocity,
        } => LiveEvent::NoteOn {
            channel: *channel,
            note: *note,
            velocity: *velocity,
        },
        NoteOff { channel, note } => LiveEvent::NoteOff {
            channel: *channel,
            note: *note,
            velocity: 0,
        },
        PolyPressure {
            channel,
            note,
            pressure,
        } => LiveEvent::PolyPressure {
            channel: *channel,
            note: *note,
            pressure: *pressure,
        },
        ControlChange {
            channel,
            controller,
            value,
        } => LiveEvent::ControlChange {
            channel: *channel,
            controller: *controller,
            value: *value,
        },
        ControlChange14 {
            channel,
            controller,
            value,
        } => {
            let msb = (*value >> 7) as u8 & 0x7F;
            let lsb = (*value & 0x7F) as u8;
            out[0] = LiveEvent::ControlChange {
                channel: *channel,
                controller: *controller & 0x1F,
                value: msb,
            };
            out[1] = LiveEvent::ControlChange {
                channel: *channel,
                controller: (*controller & 0x1F) + 32,
                value: lsb,
            };
            return 2;
        }
        PitchBend { channel, value } => LiveEvent::PitchBend {
            channel: *channel,
            value: *value,
        },
        ChannelPressure { channel, pressure } => LiveEvent::ChannelPressure {
            channel: *channel,
            pressure: *pressure,
        },
        ProgramChange { channel, program } => LiveEvent::ProgramChange {
            channel: *channel,
            program: *program,
        },
        NativeVoice(_) | NoteExpression { .. } | PluginParam { .. } => return 0,
    };
    out[0] = event;
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(event: LiveEvent) {
        let mut bytes = [0u8; 3];
        let len = event.pack_into(&mut bytes);
        assert_eq!(LiveEvent::parse(&bytes[..len]), Ok(event));
    }

    #[test]
    fn channel_and_clock_messages_round_trip() {
        for event in [
            LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
            LiveEvent::NoteOff {
                channel: 15,
                note: 127,
                velocity: 64,
            },
            LiveEvent::PolyPressure {
                channel: 3,
                note: 72,
                pressure: 33,
            },
            LiveEvent::ControlChange {
                channel: 7,
                controller: 74,
                value: 12,
            },
            LiveEvent::ProgramChange {
                channel: 9,
                program: 42,
            },
            LiveEvent::ChannelPressure {
                channel: 1,
                pressure: 99,
            },
            LiveEvent::PitchBend {
                channel: 2,
                value: -8192,
            },
            LiveEvent::PitchBend {
                channel: 2,
                value: 8191,
            },
            LiveEvent::Clock,
            LiveEvent::Start,
            LiveEvent::Continue,
            LiveEvent::Stop,
        ] {
            round_trip(event);
        }
    }

    #[test]
    fn note_on_zero_velocity_maps_to_note_off() {
        let event = LiveEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 0,
        };
        assert_eq!(
            event.to_engine_message(),
            Some(MidiMessage::NoteOff {
                channel: 0,
                note: 60
            })
        );
        assert!(LiveEvent::Clock.to_engine_message().is_none());
    }

    #[test]
    fn engine_messages_encode_for_output() {
        let mut out = [LiveEvent::default(); 2];
        assert_eq!(
            encode_engine_message(
                &MidiMessage::NoteOn {
                    channel: 1,
                    note: 64,
                    velocity: 90,
                },
                &mut out
            ),
            1
        );
        assert!(matches!(
            out[0],
            LiveEvent::NoteOn {
                channel: 1,
                note: 64,
                velocity: 90
            }
        ));
        assert_eq!(
            encode_engine_message(
                &MidiMessage::ControlChange14 {
                    channel: 0,
                    controller: 1,
                    value: 0x1234,
                },
                &mut out
            ),
            2
        );
        assert_eq!(
            out[0],
            LiveEvent::ControlChange {
                channel: 0,
                controller: 1,
                value: 0x24
            }
        );
        assert_eq!(
            out[1],
            LiveEvent::ControlChange {
                channel: 0,
                controller: 33,
                value: 0x34
            }
        );
        assert_eq!(
            encode_engine_message(
                &MidiMessage::PluginParam {
                    name: String::from("cutoff"),
                    value: 0.5,
                },
                &mut out
            ),
            0
        );
    }

    #[test]
    fn malformed_messages_are_typed_errors() {
        assert!(LiveEvent::parse(&[]).is_err());
        assert!(LiveEvent::parse(&[0x90, 60]).is_err());
        assert!(LiveEvent::parse(&[0x90, 60, 200]).is_err());
        assert!(LiveEvent::parse(&[0xF1, 0]).is_err());
        assert!(LiveEvent::parse(&[0xE0, 0, 0]).is_ok());
    }
}
