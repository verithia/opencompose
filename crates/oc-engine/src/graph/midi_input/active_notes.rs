//! Bounded note ownership for processors that do not implement channel-mode cleanup.

use oc_audio::{AudioEffect, MidiInputError, MidiInputEvent, MidiInputMessage};

pub(super) struct ActiveNotes {
    counts: [u16; 2048],
}

impl ActiveNotes {
    pub fn new() -> Self {
        Self { counts: [0; 2048] }
    }

    pub fn release_channel(
        &mut self,
        inner: &mut dyn AudioEffect,
        channel: u8,
        sample_offset: u32,
    ) -> Result<(), MidiInputError> {
        for key in 0..128u8 {
            let count = &mut self.counts[usize::from(channel) * 128 + usize::from(key)];
            while *count != 0 {
                inner.queue_midi_input(MidiInputEvent {
                    sample_offset,
                    message: MidiInputMessage::NoteOff { channel, key },
                })?;
                *count -= 1;
            }
        }
        Ok(())
    }

    pub fn release_all(&mut self, inner: &mut dyn AudioEffect) -> Result<(), MidiInputError> {
        for channel in 0..16 {
            self.release_channel(inner, channel, 0)?;
        }
        Ok(())
    }

    pub fn queue(
        &mut self,
        inner: &mut dyn AudioEffect,
        event: MidiInputEvent,
    ) -> Result<(), MidiInputError> {
        event.message.bytes()?;
        if let MidiInputMessage::ControlChange {
            channel,
            controller: 120 | 123,
            ..
        } = event.message
        {
            self.release_channel(inner, channel, event.sample_offset)?;
        }
        inner.queue_midi_input(event)?;
        let (channel, key, on) = match event.message {
            MidiInputMessage::NoteOn {
                channel,
                key,
                velocity,
            } => (channel, key, velocity != 0),
            MidiInputMessage::NoteOff { channel, key } => (channel, key, false),
            _ => return Ok(()),
        };
        let count = &mut self.counts[usize::from(channel) * 128 + usize::from(key)];
        *count = if on {
            count
                .checked_add(1)
                .ok_or(MidiInputError::CapacityExceeded)?
        } else {
            count.saturating_sub(1)
        };
        Ok(())
    }
}
