//! MIDI message types shared by the interpreter, the sampler and the synth.

/// A MIDI-like event with an absolute timestamp in samples
#[derive(Debug, Clone, PartialEq)]
pub struct MidiEvent {
    /// Absolute time in samples from start
    pub time_samples: u64,
    /// The event payload
    pub event: MidiMessage,
    /// Which track this event belongs to
    pub track_index: usize,
}

/// MIDI message types we generate
#[derive(Debug, Clone, PartialEq)]
pub enum MidiMessage {
    NativeVoice(crate::synth::NativeVoiceEvent),
    /// Select an instrument program on a channel
    ProgramChange {
        channel: u8,
        program: u8,
    },
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
    },
    /// Control Change (CC)
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// 14-bit MIDI CC value. The controller is the MSB CC number (0-31);
    /// the LSB is emitted as controller + 32 on plain MIDI paths.
    ControlChange14 {
        channel: u8,
        controller: u8,
        value: u16,
    },
    /// Pitch bend (-8192 to +8191)
    PitchBend {
        channel: u8,
        value: i16,
    },
    /// Channel pressure / aftertouch (0-127).
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    /// Polyphonic key pressure / aftertouch (0-127).
    PolyPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    /// Per-note expression, normalized or host-specific depending on expression.
    NoteExpression {
        channel: u8,
        note: u8,
        expression: String,
        value: f64,
    },
    /// Host/plugin parameter automation, normalized 0.0-1.0.
    PluginParam {
        name: String,
        value: f64,
    },
}
