//! Fixed packet conversion delegates routing and MIDI packing to their owners.
use super::super::{MidiMessage, PluginMidiEventSink, queue_plugin_midi_event};
use crate::midi_io::LiveEvent;

#[derive(Debug, Default)]
pub(super) struct ScorePackets {
    pub bytes: [[u8; 3]; 2],
    pub count: usize,
    unsupported: bool,
}
impl ScorePackets {
    fn push(&mut self, event: LiveEvent) {
        let Some(output) = self.bytes.get_mut(self.count) else {
            self.unsupported = true;
            return;
        };
        event.pack_into(output);
        self.count += 1;
    }
}
pub(super) fn encode(event: &MidiMessage) -> Result<ScorePackets, String> {
    let mut packets = ScorePackets::default();
    queue_plugin_midi_event(&mut packets, event, 0)?;
    if packets.unsupported {
        return Err("VST2 score event cannot be represented by its prepared MIDI packets".into());
    }
    Ok(packets)
}
impl PluginMidiEventSink for ScorePackets {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, _: i32) {
        self.push(LiveEvent::NoteOn {
            channel: channel & 15,
            note,
            velocity,
        });
    }
    fn note_off(&mut self, channel: u8, note: u8, _: i32) {
        self.push(LiveEvent::NoteOff {
            channel: channel & 15,
            note,
            velocity: 0,
        });
    }
    fn control_change(&mut self, channel: u8, controller: u8, value: u8, _: i32) {
        self.push(LiveEvent::ControlChange {
            channel: channel & 15,
            controller,
            value,
        });
    }
    fn program_change(&mut self, channel: u8, program: u8, _: i32) {
        self.push(LiveEvent::ProgramChange {
            channel: channel & 15,
            program,
        });
    }
    fn pitch_bend(&mut self, channel: u8, value: i16, _: i32) {
        self.push(LiveEvent::PitchBend {
            channel: channel & 15,
            value,
        });
    }
    fn channel_pressure(&mut self, channel: u8, pressure: u8, _: i32) {
        self.push(LiveEvent::ChannelPressure {
            channel: channel & 15,
            pressure,
        });
    }
    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, _: i32) {
        self.push(LiveEvent::PolyPressure {
            channel: channel & 15,
            note,
            pressure,
        });
    }
    fn note_expression(&mut self, _: u8, _: u8, _: &str, _: f64, _: i32) {
        // The VST2 compatibility route has no note-expression implementation.
    }
    fn plugin_param(&mut self, _: &str, _: f64, _: i32) {
        self.unsupported = true;
    }
}
