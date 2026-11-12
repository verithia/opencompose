//! MIDI event routing into plugin instances.

use super::*;

pub(crate) trait PluginMidiEventSink {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32);
    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32);
    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32);
    fn control_change_14(&mut self, channel: u8, controller: u8, value: u16, sample_offset: i32) {
        let msb = ((value >> 7) & 0x7f) as u8;
        let lsb = (value & 0x7f) as u8;
        self.control_change(channel, controller, msb, sample_offset);
        if controller <= 31 {
            self.control_change(channel, controller + 32, lsb, sample_offset);
        }
    }
    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32);
    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32);
    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32);
    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32);
    fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    );
    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32);
}

pub(crate) fn queue_plugin_midi_event<S: PluginMidiEventSink>(
    sink: &mut S,
    event: &MidiMessage,
    sample_offset: i32,
) -> Result<(), String> {
    match event {
        MidiMessage::NativeVoice(_) => {
            return Err("native voice events require builtin.open_synth".into());
        }
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } => sink.note_on(*channel, *note, *velocity, sample_offset),
        MidiMessage::NoteOff { channel, note } => sink.note_off(*channel, *note, sample_offset),
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        } => sink.control_change(*channel, *controller, *value, sample_offset),
        MidiMessage::ControlChange14 {
            channel,
            controller,
            value,
        } => sink.control_change_14(*channel, *controller, *value, sample_offset),
        MidiMessage::ProgramChange { channel, program } => {
            sink.program_change(*channel, *program, sample_offset)
        }
        MidiMessage::PitchBend { channel, value } => {
            sink.pitch_bend(*channel, *value, sample_offset)
        }
        MidiMessage::ChannelPressure { channel, pressure } => {
            sink.channel_pressure(*channel, *pressure, sample_offset)
        }
        MidiMessage::PolyPressure {
            channel,
            note,
            pressure,
        } => sink.poly_pressure(*channel, *note, *pressure, sample_offset),
        MidiMessage::NoteExpression {
            channel,
            note,
            expression,
            value,
        } => sink.note_expression(*channel, *note, expression, *value, sample_offset),
        MidiMessage::PluginParam { name, value } => sink.plugin_param(name, *value, sample_offset),
    }
    Ok(())
}

pub(super) fn queue_vst3_profile_event<S: PluginMidiEventSink>(
    sink: &mut S,
    event: &MidiMessage,
    sample_offset: i32,
    profile: &InstrumentProfile,
) -> Result<(), String> {
    if let MidiMessage::ControlChange {
        controller, value, ..
    } = event
        && let Some(param_name) = profile.cc_param(*controller)
    {
        sink.plugin_param(param_name, *value as f64 / 127.0, sample_offset);
        return Ok(());
    }

    if let MidiMessage::ControlChange14 {
        controller, value, ..
    } = event
        && let Some(param_name) = profile.cc_param(*controller)
    {
        sink.plugin_param(param_name, *value as f64 / 16383.0, sample_offset);
        return Ok(());
    }

    queue_plugin_midi_event(sink, event, sample_offset)
}

impl PluginMidiEventSink for ClapInstance {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.note_on_channel(channel, note, velocity as f64 / 127.0, sample_offset as u32);
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.note_off_channel(channel, note, sample_offset as u32);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.control_change(channel, controller, value, sample_offset as u32);
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.program_change(channel, program, sample_offset as u32);
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.pitch_bend(channel, value, sample_offset as u32);
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.channel_pressure(channel, pressure, sample_offset as u32);
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure(channel, note, pressure, sample_offset as u32);
    }

    fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    ) {
        self.note_expression(channel, note, expression, value, sample_offset as u32);
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        self.queue_param_normalized_by_name(name, value, sample_offset.max(0) as u32);
    }
}

#[cfg(target_os = "macos")]
impl PluginMidiEventSink for oc_host::au_instance::AuInstance {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.note_on_channel(channel, note, f32::from(velocity) / 127.0, sample_offset);
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.note_off_channel(channel, note, sample_offset);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.control_change(channel, controller, value, sample_offset);
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.program_change(channel, program, sample_offset);
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.pitch_bend(channel, value, sample_offset);
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.channel_pressure(channel, pressure, sample_offset);
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure(channel, note, pressure, sample_offset);
    }

    fn note_expression(
        &mut self,
        _channel: u8,
        _note: u8,
        _expression: &str,
        _value: f64,
        _sample_offset: i32,
    ) {
        // AUv2 has no per-note expression entry point; MPE still reaches the
        // unit through channel pressure and pitch bend.
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        self.queue_param_by_name(name, value, sample_offset.max(0) as u32);
    }
}

impl PluginMidiEventSink for oc_host::lv2::Lv2Instance {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.note_on_channel(
            channel,
            note,
            f64::from(velocity) / 127.0,
            sample_offset.max(0) as u32,
        );
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.note_off_channel(channel, note, sample_offset.max(0) as u32);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.control_change(channel, controller, value, sample_offset.max(0) as u32);
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.program_change(channel, program, sample_offset.max(0) as u32);
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.pitch_bend(channel, value, sample_offset.max(0) as u32);
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.channel_pressure(channel, pressure, sample_offset.max(0) as u32);
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure(channel, note, pressure, sample_offset.max(0) as u32);
    }

    fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    ) {
        self.note_expression(
            channel,
            note,
            expression,
            value,
            sample_offset.max(0) as u32,
        );
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        self.queue_param_by_name(name, value, sample_offset.max(0) as u32);
    }
}

impl PluginMidiEventSink for oc_host::vst2::Vst2Instance {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.note_on_channel(channel, note, f32::from(velocity) / 127.0, sample_offset);
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.note_off_channel(channel, note, sample_offset);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.control_change_channel(channel, controller, value, sample_offset);
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.program_change_channel(channel, program, sample_offset);
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.pitch_bend_channel(channel, value, sample_offset);
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.channel_pressure_channel(channel, pressure, sample_offset);
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure_channel(channel, note, pressure, sample_offset);
    }

    fn note_expression(
        &mut self,
        _channel: u8,
        _note: u8,
        _expression: &str,
        _value: f64,
        _sample_offset: i32,
    ) {
        // VST2 has no per-note expression entry point; MPE reaches the plugin
        // through channel pressure and pitch bend.
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        // VST2 parameters are block-quantized: apply at the block start.
        let _ = sample_offset;
        self.set_param_by_name(name, value);
    }
}

impl PluginMidiEventSink for Vst3Instance {
    fn note_on(&mut self, channel: u8, note: u8, velocity: u8, sample_offset: i32) {
        self.note_on_channel(channel, note, velocity as f32 / 127.0, sample_offset);
    }

    fn note_off(&mut self, channel: u8, note: u8, sample_offset: i32) {
        self.note_off_channel(channel, note, sample_offset);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.control_change(channel, controller, value, sample_offset);
    }

    fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.program_change(channel, program, sample_offset);
    }

    fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        self.pitch_bend(channel, value, sample_offset);
    }

    fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.channel_pressure(channel, pressure, sample_offset);
    }

    fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.poly_pressure(channel, note, pressure, sample_offset);
    }

    fn note_expression(
        &mut self,
        channel: u8,
        note: u8,
        expression: &str,
        value: f64,
        sample_offset: i32,
    ) {
        self.note_expression(channel, note, expression, value, sample_offset);
    }

    fn plugin_param(&mut self, name: &str, value: f64, sample_offset: i32) {
        self.queue_param_by_name(name, value, sample_offset);
    }
}
