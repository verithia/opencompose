//! Strict owned ingress; the compatibility convenience API keeps its old clamps.
use super::super::{MAX_PENDING_MIDI, PendingMidi};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a VST2 MIDI message was refused.
pub enum Vst2MidiError {
    Status,
    DataByte,
    UnusedDataByte,
    Offset,
    Capacity,
}
impl std::fmt::Display for Vst2MidiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Status => "VST2 owned MIDI requires a channel-voice status",
            Self::DataByte => "VST2 MIDI data bytes must be seven-bit values",
            Self::UnusedDataByte => "VST2 two-byte messages require zero unused data",
            Self::Offset => "VST2 MIDI offset is outside the processing block",
            Self::Capacity => "VST2 MIDI queue capacity exceeded",
        })
    }
}
impl std::error::Error for Vst2MidiError {}

pub(super) fn admit(
    message: PendingMidi,
    frames: u32,
    queued: usize,
    capacity: usize,
) -> Result<PendingMidi, Vst2MidiError> {
    if !(0x80..=0xef).contains(&message.status) {
        return Err(Vst2MidiError::Status);
    }
    if message.data1 > 0x7f || message.data2 > 0x7f {
        return Err(Vst2MidiError::DataByte);
    }
    if matches!(message.status & 0xf0, 0xc0 | 0xd0) && message.data2 != 0 {
        return Err(Vst2MidiError::UnusedDataByte);
    }
    if message.offset < 0 || message.offset as u32 >= frames {
        return Err(Vst2MidiError::Offset);
    }
    if queued >= MAX_PENDING_MIDI.min(capacity) {
        return Err(Vst2MidiError::Capacity);
    }
    Ok(message)
}
pub(super) fn validate_block(messages: &[PendingMidi], frames: usize) -> Result<(), Vst2MidiError> {
    if messages
        .iter()
        .any(|message| message.offset < 0 || message.offset as usize >= frames)
    {
        return Err(Vst2MidiError::Offset);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(status: u8, data1: u8, data2: u8, offset: i32) -> PendingMidi {
        PendingMidi {
            status,
            data1,
            data2,
            offset,
        }
    }
    #[test]
    fn owned_midi_channel_voice_lengths_data_and_frame_bounds_are_explicit() {
        for class in 0x8..=0xe {
            for channel in [0, 15] {
                let data2 = if matches!(class, 0xc | 0xd) { 0 } else { 127 };
                let valid = message(class << 4 | channel, 127, data2, 63);
                assert!(admit(valid, 64, 0, MAX_PENDING_MIDI).is_ok());
            }
        }
        for status in [0, 0x7f, 0xf0, 0xf1, 0xf7, 0xf8, 0xff] {
            assert_eq!(
                admit(message(status, 0, 0, 0), 64, 0, MAX_PENDING_MIDI).unwrap_err(),
                Vst2MidiError::Status
            );
        }
        for msg in [message(0x90, 128, 0, 0), message(0x90, 0, 255, 0)] {
            assert_eq!(
                admit(msg, 64, 0, MAX_PENDING_MIDI).unwrap_err(),
                Vst2MidiError::DataByte
            );
        }
        for status in [0xc0, 0xdf] {
            assert_eq!(
                admit(message(status, 1, 1, 0), 64, 0, MAX_PENDING_MIDI).unwrap_err(),
                Vst2MidiError::UnusedDataByte
            );
        }
        for offset in [-1, 64, i32::MAX] {
            assert_eq!(
                admit(message(0x90, 0, 0, offset), 64, 0, MAX_PENDING_MIDI).unwrap_err(),
                Vst2MidiError::Offset
            );
        }
        assert_eq!(
            admit(
                message(0x90, 0, 0, 0),
                64,
                MAX_PENDING_MIDI,
                MAX_PENDING_MIDI
            )
            .unwrap_err(),
            Vst2MidiError::Capacity
        );
        assert_eq!(
            admit(message(0x90, 0, 0, 0), 64, 0, 0).unwrap_err(),
            Vst2MidiError::Capacity
        );
    }
    #[test]
    fn owned_midi_shorter_block_preflight_preserves_pending_messages() {
        let pending = [message(0x90, 60, 100, 63)];
        assert_eq!(validate_block(&pending, 32), Err(Vst2MidiError::Offset));
        assert_eq!(pending[0].offset, 63);
        assert_eq!(validate_block(&pending, 64), Ok(()));
        assert_eq!(validate_block(&pending, 0), Err(Vst2MidiError::Offset));
        assert_eq!(validate_block(&[], 0), Ok(()));
    }
}
