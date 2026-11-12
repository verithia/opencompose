//! The SoundFont renderer: channel state, note handling and block mixing.
//!
//! Surface of the module the engine's adapters consume: construct once per
//! track with the output
//! sample rate, [`Sf2Renderer::enable_drum_channel`], feed MIDI with
//! [`Sf2Renderer::send`], then pull interleaved stereo frames.
//!
//! Allocation happens in construction and per note-on zone, never per sample.
//! The two effect sends are one chorus and one reverb bus per player, shared by
//! every voice (see [`super::sends`]).

use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;

use crate::interpret::{MidiEvent, MidiMessage};

use super::font::SoundFont;
use super::modulation::ModInput;
use super::sends::{SEND_BLOCK_FRAMES, SendBuses};
use super::voice::{DynamicLayer, MAX_DYNAMIC_LAYERS, MAX_VOICES, Voice, velocity_gain};

/// Pitch-bend range in semitones, the MIDI default both paths assume.
pub const BEND_RANGE_SEMITONES: f64 = 2.0;

/// MIDI's default channel volume: CC7 = 100 out of 127, squared by the law.
const DEFAULT_VOLUME: f32 = (100.0 / 127.0) * (100.0 / 127.0);

/// Master gain calibrated so a full-scale mono sample at attenuation 0, pan
/// centre, velocity 127 and CC7 127 renders at the fixture's reference level.
pub const MASTER_GAIN: f32 = 0.6722;

/// A hairpin's span in output frames and the two dynamics it moves between.
///
/// The player crossfades the velocity layers between these levels while a note
/// that starts inside the span sounds; the track's gain ramp stays the only
/// thing that moves the level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicRamp {
    pub start_frame: u64,
    pub end_frame: u64,
    pub start_velocity: u8,
    pub target_velocity: u8,
}

impl DynamicRamp {
    /// Gain at `frame` relative to the span's start; the target holds after it.
    pub(crate) fn gain_at(&self, frame: u64) -> f32 {
        let ratio = self.end_gain();
        if frame <= self.start_frame || self.end_frame <= self.start_frame {
            return if frame <= self.start_frame {
                1.0
            } else {
                ratio
            };
        }
        if frame >= self.end_frame {
            return ratio;
        }
        let elapsed = (frame - self.start_frame) as f32;
        let span = (self.end_frame - self.start_frame) as f32;
        // A hairpin runs linear in decibels, so the layer position follows the
        // same exponential law the gain clip is written with.
        ratio.powf(elapsed / span)
    }

    /// Level ratio between the two dynamics, from the velocity square law.
    fn end_gain(&self) -> f32 {
        if self.start_velocity == 0 {
            return 1.0;
        }
        let ratio = f32::from(self.target_velocity) / f32::from(self.start_velocity);
        ratio * ratio
    }
}

/// One MIDI channel's state.
#[derive(Debug, Clone, Copy)]
struct Channel {
    bank: u16,
    /// CC0/CC32 bank select, in force at the next program change.
    bank_msb: u8,
    bank_lsb: u8,
    program: u16,
    /// Resolved preset index, or `None` when the bank has no matching preset.
    preset: Option<usize>,
    drum: bool,
    /// Pitch bend in semitones.
    bend: f64,
    /// CC7 volume as a linear gain.
    volume: f32,
    /// CC11 expression as a linear gain.
    expression: f32,
    /// CC10 pan, -1.0 (left) .. 1.0 (right).
    pan: f32,
    sustain: bool,
    /// Channel pressure, 0 .. 127.
    pressure: u8,
    /// Polyphonic pressure per note, 0 .. 127. A note-on resets its own entry.
    poly_pressure: [u8; 128],
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            bank: 0,
            bank_msb: 0,
            bank_lsb: 0,
            program: 0,
            preset: None,
            drum: false,
            bend: 0.0,
            volume: DEFAULT_VOLUME,
            expression: 1.0,
            pan: 0.0,
            sustain: false,
            pressure: 0,
            poly_pressure: [0; 128],
        }
    }
}

impl Channel {
    fn gain(&self) -> f32 {
        self.volume * self.expression
    }
}

/// A checked program switch requires control-plane resource preparation first.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("SoundFont bank {bank} program {program} requires streaming preparation")]
struct ProgramPreparationNeeded {
    bank: u16,
    program: u8,
}

/// A SoundFont player for one output rate.
pub struct Sf2Renderer {
    font: Arc<SoundFont>,
    /// Preset capabilities prepared off the processing path; None is a full bank.
    admitted_presets: Option<Box<[bool]>>,
    sample_rate: u32,
    channels: [Channel; 16],
    /// Raw controller values per channel, for the modulator sources.
    controllers: [[u8; 128]; 16],
    voices: Vec<Voice>,
    age: u64,
    /// Set by [`Self::enable_drum_channel`]; channel 10 is MIDI channel 9.
    drum_channel: Cell<bool>,
    /// The two effect sends, one chorus and one reverb for every voice.
    sends: SendBuses,
    /// Crossfade velocity layers along a hairpin; on unless a preset opts out.
    dynamic_layers: bool,
    /// The track's hairpin ramps, in output frames. Empty disables the path.
    dynamic_ramps: Vec<DynamicRamp>,
    /// Per-channel gain of the preset's output stage; unity at 0 dB, centre.
    output_gains: (f32, f32),
    /// Frames rendered so far, so a note can place itself on the timeline.
    position: u64,
}

impl Sf2Renderer {
    /// Load a bank and prepare a renderer at `sample_rate`.
    pub fn new(sample_rate: u32, soundfont_path: &Path) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("SoundFont render sample rate must be positive".into());
        }
        let font = SoundFont::load_shared(soundfont_path).map_err(|error| error.to_string())?;
        Ok(Self::from_font(font, sample_rate))
    }

    /// Prepare authored MIDI program resources without decoding unrelated SF3
    /// samples. Channel planning uses the same state transitions as playback.
    pub fn new_streaming<'a>(
        sample_rate: u32,
        soundfont_path: &Path,
        events: impl IntoIterator<Item = &'a MidiMessage>,
        drum_channel: bool,
    ) -> Result<Self, String> {
        Self::new_streaming_with_options(
            sample_rate,
            soundfont_path,
            events,
            drum_channel,
            crate::render_parallel::RenderWorkerOptions::default(),
        )
    }

    /// Streaming preparation with the owning request's worker-count snapshot.
    pub fn new_streaming_with_options<'a>(
        sample_rate: u32,
        soundfont_path: &Path,
        events: impl IntoIterator<Item = &'a MidiMessage>,
        drum_channel: bool,
        workers: crate::render_parallel::RenderWorkerOptions,
    ) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("SoundFont render sample rate must be positive".into());
        }
        // Collected so the admission plan can run more than once: a cached
        // preparation is checked against this project's program set.
        let events: Vec<&MidiMessage> = events.into_iter().collect();
        let prepared = super::font::prepare_streaming(soundfont_path, workers, |font| {
            let mut planner = Self::from_font(font, sample_rate);
            planner.drum_channel.set(drum_channel);
            let mut admitted = vec![false; planner.font.presets.len()];
            let mut any_message = false;
            for message in events.iter().copied() {
                any_message = true;
                let selected_channel = match message {
                    MidiMessage::NoteOn {
                        channel, velocity, ..
                    } if *velocity > 0 => {
                        // Planning refreshes channel-10 bank semantics but never
                        // creates a voice or reads the zero-backed waveform.
                        let channel = usize::from(*channel).min(15);
                        planner.refresh_drum_state(channel);
                        Some(channel)
                    }
                    MidiMessage::NoteOn { .. } => None,
                    _ => {
                        planner.send(message)?;
                        match message {
                            MidiMessage::ProgramChange { channel, .. } => {
                                Some(usize::from(*channel).min(15))
                            }
                            _ => None,
                        }
                    }
                };
                if let Some(channel) = selected_channel
                    && let Some(index) = planner.channels[channel].preset
                {
                    admitted[index] = true;
                }
            }
            // An empty source remains usable for its default live keyboard.
            if !any_message && let Some(index) = planner.channels[0].preset {
                admitted[index] = true;
            }
            Ok(admitted)
        })?;
        let mut renderer = Self::from_font(prepared.font, sample_rate);
        renderer.admitted_presets = prepared.admitted;
        renderer.drum_channel.set(drum_channel);
        Ok(renderer)
    }

    /// Build from an already parsed bank; the [`Arc`] is shared, not copied.
    pub fn from_font(font: Arc<SoundFont>, sample_rate: u32) -> Self {
        let mut renderer = Self {
            font,
            admitted_presets: None,
            sample_rate: sample_rate.max(1),
            channels: [Channel::default(); 16],
            controllers: [power_up_controllers(); 16],
            voices: Vec::with_capacity(MAX_VOICES),
            age: 0,
            drum_channel: Cell::new(false),
            sends: SendBuses::new(sample_rate.max(1)),
            dynamic_layers: true,
            dynamic_ramps: Vec::new(),
            output_gains: (1.0, 1.0),
            position: 0,
        };
        renderer.select_program(0, 0);
        for channel in 1..16 {
            renderer.select_program(channel, 0);
        }
        renderer
    }

    /// Set channel 9 to the MIDI drum channel; the next program change or
    /// note-on on that channel selects bank 128.
    pub fn enable_drum_channel(&self) {
        self.drum_channel.set(true);
    }

    /// Enable or disable the hairpin velocity-layer crossfade. Default: on.
    pub fn set_dynamic_layers(&mut self, enabled: bool) {
        self.dynamic_layers = enabled;
        if !enabled {
            self.dynamic_ramps.clear();
        }
    }

    /// Install the track's hairpin ramps in output frames. An empty list keeps
    /// every note on its single hard-selected layer.
    pub fn set_dynamic_ramps(&mut self, ramps: Vec<DynamicRamp>) {
        self.dynamic_ramps = ramps;
    }

    /// Apply the preset's output stage after the voices and before the track
    /// chain: `gain_db` and a pan in `-1.0..=1.0`, centre unity.
    pub fn set_output(&mut self, gain_db: f64, pan: f64) {
        let gain = if gain_db.is_finite() {
            10.0_f64.powf(gain_db / 20.0)
        } else {
            1.0
        };
        let (left, right) = crate::graph::equal_power_pan_gains(pan);
        self.output_gains = (
            (f64::from(left) * gain) as f32,
            (f64::from(right) * gain) as f32,
        );
    }

    /// Scale a finished block by the preset's output stage.
    fn apply_output(&self, block: &mut [f32]) {
        let (left, right) = self.output_gains;
        if left == 1.0 && right == 1.0 {
            return;
        }
        for frame in block.chunks_exact_mut(2) {
            frame[0] *= left;
            frame[1] *= right;
        }
    }

    /// Place the lane at an absolute output frame. A seek moves the clock only:
    /// the next note reads the ramp covering it, while a voice that already
    /// sounds keeps the ramp it started under.
    pub fn set_position(&mut self, frame: u64) {
        self.position = frame;
    }

    /// Return to the state a freshly constructed player has. The parsed bank
    /// stays shared; channels, voices, sends and the clock reset, and the
    /// lane's drum/dynamic/output settings survive.
    pub fn reset(&mut self) {
        let admitted = self.admitted_presets.take();
        let drum = self.drum_channel.get();
        let dynamic_layers = self.dynamic_layers;
        let ramps = std::mem::take(&mut self.dynamic_ramps);
        let output_gains = self.output_gains;
        *self = Self::from_font(Arc::clone(&self.font), self.sample_rate);
        self.admitted_presets = admitted;
        self.drum_channel.set(drum);
        self.dynamic_layers = dynamic_layers;
        self.dynamic_ramps = ramps;
        self.output_gains = output_gains;
    }

    /// Apply the drum flag to channel 9 before its next note.
    fn refresh_drum_state(&mut self, channel: usize) {
        if channel == 9 && self.drum_channel.get() && !self.channels[channel].drum {
            let program = self.channels[channel].program as u8;
            self.select_program(channel, program);
        }
    }

    /// Renderer sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The parsed bank, for diagnostics and tests.
    pub fn font(&self) -> &SoundFont {
        &self.font
    }

    /// Number of voices currently sounding.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|voice| !voice.finished).count()
    }

    /// Process one MIDI message.
    pub fn send(&mut self, msg: &MidiMessage) -> Result<(), String> {
        match msg {
            MidiMessage::NativeVoice(_) => {
                return Err("native voice events cannot render through a SoundFont".into());
            }
            MidiMessage::ProgramChange { channel, program } => {
                let channel = usize::from(*channel).min(15);
                self.admit_selection(channel, *program)
                    .map_err(|error| error.to_string())?;
                self.select_program(channel, *program);
            }
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                if *velocity == 0 {
                    self.note_off(usize::from(*channel).min(15), *note);
                } else {
                    let channel = usize::from(*channel).min(15);
                    // A first drum note can switch bank without a program
                    // change. Refuse before refreshing channel or voice state.
                    self.admit_current_note(channel)
                        .map_err(|error| error.to_string())?;
                    self.note_on(channel, *note, *velocity);
                }
            }
            MidiMessage::NoteOff { channel, note } => {
                self.note_off(usize::from(*channel).min(15), *note);
            }
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => self.control_change(usize::from(*channel).min(15), *controller, *value),
            MidiMessage::ControlChange14 {
                channel,
                controller,
                value,
            } => {
                let channel = usize::from(*channel).min(15);
                let msb = ((*value >> 7) & 0x7f) as u8;
                let lsb = (*value & 0x7f) as u8;
                self.control_change(channel, *controller, msb);
                if *controller <= 31 {
                    self.control_change(channel, controller + 32, lsb);
                }
            }
            MidiMessage::PitchBend { channel, value } => {
                let channel = usize::from(*channel).min(15);
                let bend = f64::from(*value) / 8192.0 * BEND_RANGE_SEMITONES;
                self.channels[channel].bend = bend;
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|v| usize::from(v.channel) == channel)
                {
                    voice.set_bend(bend);
                }
                self.refresh_modulation(channel, None);
            }
            MidiMessage::ChannelPressure { channel, pressure } => {
                let channel = usize::from(*channel).min(15);
                self.channels[channel].pressure = (*pressure).min(127);
                self.refresh_modulation(channel, None);
            }
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => self.poly_pressure(usize::from(*channel).min(15), *note, *pressure),
            MidiMessage::NoteExpression { .. } | MidiMessage::PluginParam { .. } => {}
        }
        Ok(())
    }

    fn program_selection(&self, channel: usize, program: u8) -> (u16, bool, Option<usize>) {
        let drum = self.channels[channel].drum || (channel == 9 && self.drum_channel.get());
        let bank = if drum {
            128
        } else {
            u16::from(self.channels[channel].bank_msb) << 7
                | u16::from(self.channels[channel].bank_lsb)
        };
        let preset = if drum && self.font.drum_preset_index(u16::from(program)).is_none() {
            None
        } else {
            self.font.find_preset(bank, u16::from(program))
        };
        (bank, drum, preset)
    }

    fn admit_selection(&self, channel: usize, program: u8) -> Result<(), ProgramPreparationNeeded> {
        let (bank, _, preset) = self.program_selection(channel, program);
        if let Some(admitted) = &self.admitted_presets
            && preset.is_some_and(|index| !admitted[index])
        {
            return Err(ProgramPreparationNeeded { bank, program });
        }
        Ok(())
    }

    fn admit_current_note(&self, channel: usize) -> Result<(), ProgramPreparationNeeded> {
        if channel == 9 && self.drum_channel.get() && !self.channels[channel].drum {
            self.admit_selection(channel, self.channels[channel].program as u8)
        } else if let Some(admitted) = &self.admitted_presets
            && self.channels[channel]
                .preset
                .is_some_and(|index| !admitted[index])
        {
            Err(ProgramPreparationNeeded {
                bank: self.channels[channel].bank,
                program: self.channels[channel].program as u8,
            })
        } else {
            Ok(())
        }
    }

    fn select_program(&mut self, channel: usize, program: u8) {
        let (bank, drum, preset) = self.program_selection(channel, program);
        let state = &mut self.channels[channel];
        state.program = u16::from(program);
        state.bank = bank;
        state.drum = drum;
        // A missing drum kit remains silent; melodic lookup keeps its fallback.
        state.preset = preset;
    }

    fn note_on(&mut self, channel: usize, note: u8, velocity: u8) {
        self.refresh_drum_state(channel);
        // A fresh note starts with no polyphonic pressure of its own.
        self.channels[channel].poly_pressure[usize::from(note).min(127)] = 0;
        let Some(preset_index) = self.channels[channel].preset else {
            return;
        };
        let plan = self.dynamic_plan(preset_index, note);
        // One voice per zone; a crossfaded note starts one voice per velocity
        // layer instead of only the layer its own velocity selects.
        let specs: Vec<(usize, u8, Option<usize>)> = {
            let preset = &self.font.presets[preset_index];
            match &plan {
                Some(plan) => plan
                    .layers
                    .iter()
                    .enumerate()
                    .flat_map(|(layer, (probe, zones))| {
                        zones.iter().map(move |&zone| (zone, *probe, Some(layer)))
                    })
                    .collect(),
                None => preset
                    .zones
                    .iter()
                    .enumerate()
                    .filter(|(_, zone)| zone.matches(note, velocity))
                    .map(|(index, _)| (index, velocity, None))
                    .collect(),
            }
        };
        // Choke: a new note in the same exclusive class ends the previous one.
        let mut exclusive_classes: [i16; MAX_VOICES] = [0; MAX_VOICES];
        let mut exclusive_count = 0usize;
        for (index, probe, layer) in specs {
            let zone = &self.font.presets[preset_index].zones[index];
            if zone.exclusive_class != 0 && exclusive_count < exclusive_classes.len() {
                exclusive_classes[exclusive_count] = zone.exclusive_class;
                exclusive_count += 1;
            }
            self.age = self.age.wrapping_add(1);
            let input = ModInput::new(
                &self.controllers[channel],
                self.channels[channel].pressure,
                zone.effective_key(note),
                zone.effective_velocity(probe),
                self.channels[channel].bend,
            )
            .with_poly_pressure(self.channels[channel].poly_pressure[usize::from(note)]);
            let mut voice = Voice::start(
                channel as u8,
                preset_index,
                index,
                zone,
                self.sample_rate,
                self.font.sample_rate_of(zone),
                note,
                probe,
                self.age,
                zone.modulation(&input),
            );
            if let (Some(layer), Some(plan)) = (layer, plan.as_ref()) {
                // The layer decides the spectrum; the note's own velocity stays
                // the level so the track ramp is still the only level move.
                let base = velocity_gain(zone.effective_velocity(velocity));
                let probe = velocity_gain(zone.effective_velocity(probe));
                voice.set_gain_scale((base / probe) as f32);
                voice.set_dynamic_layer(plan.layer(zone.effective_velocity(velocity), layer));
            }
            voice.set_channel_pan(self.channels[channel].pan);
            if voice.send_chorus > 0.0 || voice.send_reverb > 0.0 {
                self.sends.activate();
            }
            push_voice(&mut self.voices, voice);
        }
        if exclusive_count > 0 {
            for voice in &mut self.voices {
                if usize::from(voice.channel) != channel || voice.finished || voice.note == note {
                    continue;
                }
                if exclusive_classes[..exclusive_count].contains(&voice.exclusive_class) {
                    voice.release();
                }
            }
        }
    }

    /// The velocity layers this note crosses while a hairpin ramp is active.
    fn dynamic_plan(&self, preset_index: usize, note: u8) -> Option<DynamicPlan> {
        if !self.dynamic_layers || self.dynamic_ramps.is_empty() {
            return None;
        }
        let frame = self.position;
        let ramp = self
            .dynamic_ramps
            .iter()
            .rev()
            .find(|ramp| frame >= ramp.start_frame && frame < ramp.end_frame)
            .copied()?;
        let end_gain = ramp.end_gain();
        if (end_gain - 1.0).abs() < 1e-6 {
            return None;
        }
        // The whole ladder, not the velocities this note reaches: a note that
        // starts late in the ramp still has to sound the layer the ramp is at.
        let layers = velocity_layers(&self.font.presets[preset_index].zones, note);
        (layers.len() > 1).then_some(DynamicPlan { ramp, layers })
    }

    fn note_off(&mut self, channel: usize, note: u8) {
        let sustain = self.channels[channel].sustain;
        for voice in &mut self.voices {
            if usize::from(voice.channel) != channel || voice.finished || voice.note != note {
                continue;
            }
            if !voice.key_down {
                continue;
            }
            if sustain {
                // The pedal holds the note; `control_change` releases it.
                voice.hold_for_pedal();
            } else {
                voice.release();
            }
        }
    }

    /// Polyphonic pressure: the note's own voices read it, and nothing else.
    fn poly_pressure(&mut self, channel: usize, note: u8, pressure: u8) {
        self.channels[channel].poly_pressure[usize::from(note).min(127)] = pressure.min(127);
        self.refresh_modulation(channel, Some(note));
    }

    fn control_change(&mut self, channel: usize, controller: u8, raw: u8) {
        let raw = raw.min(127);
        self.controllers[channel][usize::from(controller)] = raw;
        let value = f32::from(raw) / 127.0;
        match controller {
            // Bank select: CC0 is the MSB, CC32 the LSB. Both take effect at
            // the next program change, as MIDI defines them.
            0 => {
                self.channels[channel].bank_msb = raw;
            }
            32 => {
                self.channels[channel].bank_lsb = raw;
            }
            7 => {
                self.channels[channel].volume = volume_gain(value);
            }
            10 => {
                // MIDI pan: 64 is centre, 0 and 127 are the hard ends.
                let pan = ((f32::from(raw.min(127)) - 64.0) / 64.0).clamp(-1.0, 1.0);
                self.channels[channel].pan = pan;
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|voice| usize::from(voice.channel) == channel)
                {
                    voice.set_channel_pan(pan);
                }
            }
            11 => {
                self.channels[channel].expression = volume_gain(value);
            }
            64 => {
                let engaged = value >= 64.0 / 127.0;
                let was = self.channels[channel].sustain;
                self.channels[channel].sustain = engaged;
                if was && !engaged {
                    for voice in self
                        .voices
                        .iter_mut()
                        .filter(|voice| usize::from(voice.channel) == channel)
                    {
                        if voice.pedal_held() {
                            voice.release();
                        }
                    }
                }
            }
            120 => {
                self.voices
                    .retain(|voice| usize::from(voice.channel) != channel);
            }
            121 => {
                self.controllers[channel] = power_up_controllers();
                let state = &mut self.channels[channel];
                state.bank_msb = 0;
                state.bank_lsb = 0;
                state.volume = 1.0;
                state.expression = 1.0;
                state.pan = 0.0;
                state.sustain = false;
                state.bend = 0.0;
                state.pressure = 0;
                state.poly_pressure = [0; 128];
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|voice| usize::from(voice.channel) == channel)
                {
                    voice.set_channel_pan(0.0);
                    voice.set_bend(0.0);
                }
            }
            123 => {
                for voice in self
                    .voices
                    .iter_mut()
                    .filter(|voice| usize::from(voice.channel) == channel)
                {
                    voice.release();
                }
            }
            _ => {}
        }
        self.refresh_modulation(channel, None);
    }

    /// Push the channel's current controller state into every sounding voice of
    /// the channel: every modulation depth is a function of the controllers,
    /// pressure and any bank modulator, while each voice keeps its own
    /// oscillator phase.
    fn refresh_modulation(&mut self, channel: usize, only_note: Option<u8>) {
        let font = &self.font;
        let controllers = &self.controllers[channel];
        let state = self.channels[channel];
        for voice in &mut self.voices {
            if usize::from(voice.channel) != channel {
                continue;
            }
            if only_note.is_some_and(|note| voice.note != note) {
                continue;
            }
            let Some(zone) = font
                .presets
                .get(voice.preset)
                .and_then(|preset| preset.zones.get(voice.zone))
            else {
                continue;
            };
            let input = ModInput::new(
                controllers,
                state.pressure,
                voice.effective_key,
                voice.effective_velocity,
                state.bend,
            )
            .with_poly_pressure(state.poly_pressure[usize::from(voice.note)]);
            voice.set_modulation(zone.modulation(&input));
            if voice.send_chorus > 0.0 || voice.send_reverb > 0.0 {
                self.sends.activate();
            }
        }
    }

    /// Render `frames` stereo frames into an interleaved buffer of
    /// `frames * 2` samples, replacing its contents.
    pub fn render_into(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        let block_start = self.position;
        let block_frames = (output.len() / 2) as u64;
        let gains: [f32; 16] = self.channels.map(|channel| channel.gain() * MASTER_GAIN);
        if !self.sends.is_active() {
            let font = &self.font;
            for voice in &mut self.voices {
                if voice.finished {
                    continue;
                }
                let Some(preset) = font.presets.get(voice.preset) else {
                    voice.finished = true;
                    continue;
                };
                let Some(zone) = preset.zones.get(voice.zone) else {
                    voice.finished = true;
                    continue;
                };
                let gain = gains[usize::from(voice.channel)];
                voice.set_frame_base(block_start);
                for frame in output.chunks_exact_mut(2) {
                    voice.render_frame(&font.sample_bank, zone, gain, frame);
                    if voice.finished {
                        break;
                    }
                }
            }
            self.voices.retain(|voice| !voice.finished);
            self.apply_output(output);
            self.position = block_start + block_frames;
            return;
        }
        // A sending player runs the two buses over fixed-size passes: the bus
        // buffers never grow, and the voices keep their own state.
        let mut chunk_offset = 0u64;
        for chunk in output.chunks_mut(SEND_BLOCK_FRAMES * 2) {
            let chunk_frames = (chunk.len() / 2) as u64;
            let (chorus_bus, reverb_bus) = self.sends.begin(chunk.len());
            let font = &self.font;
            for voice in &mut self.voices {
                if voice.finished {
                    continue;
                }
                let Some(preset) = font.presets.get(voice.preset) else {
                    voice.finished = true;
                    continue;
                };
                let Some(zone) = preset.zones.get(voice.zone) else {
                    voice.finished = true;
                    continue;
                };
                let gain = gains[usize::from(voice.channel)];
                voice.set_frame_base(block_start + chunk_offset);
                let buses = chorus_bus
                    .chunks_exact_mut(2)
                    .zip(reverb_bus.chunks_exact_mut(2));
                for (frame, (chorus, reverb)) in chunk.chunks_exact_mut(2).zip(buses) {
                    voice.render_frame_with_sends(
                        &font.sample_bank,
                        zone,
                        gain,
                        frame,
                        (chorus, reverb),
                    );
                    if voice.finished {
                        break;
                    }
                }
            }
            self.sends.finish(chunk);
            self.apply_output(chunk);
            chunk_offset += chunk_frames;
        }
        self.voices.retain(|voice| !voice.finished);
        self.position = block_start + block_frames;
    }

    /// Render `num_samples` stereo frames and return them interleaved.
    pub fn render_samples(&mut self, num_samples: usize) -> Vec<f32> {
        let mut output = vec![0.0; num_samples.saturating_mul(2)];
        self.render_into(&mut output);
        output
    }

    /// Render a whole event list to an interleaved stereo buffer.
    pub fn render_events(&mut self, events: &[MidiEvent], duration_seconds: f64) -> Vec<f32> {
        self.try_render_events(events, duration_seconds)
            .expect("SoundFont render requires compatible events")
    }

    /// Render events, returning an error instead of panicking.
    pub fn try_render_events(
        &mut self,
        events: &[MidiEvent],
        duration_seconds: f64,
    ) -> Result<Vec<f32>, String> {
        let total_samples = (duration_seconds.max(0.0) * f64::from(self.sample_rate)) as usize;
        // Sanity cap: ten minutes, matching the wrapper's guard.
        let total_samples = total_samples.min(self.sample_rate as usize * 600);
        let chunk_frames = 64;
        let mut output = Vec::with_capacity(total_samples * 2);
        let mut event_index = 0;
        let mut sample_position = 0u64;
        let mut block = Vec::with_capacity(chunk_frames * 2);
        while sample_position < total_samples as u64 {
            while event_index < events.len() && events[event_index].time_samples <= sample_position
            {
                self.send(&events[event_index].event)?;
                event_index += 1;
            }
            let next_event = events
                .get(event_index)
                .map(|event| event.time_samples)
                .unwrap_or(total_samples as u64);
            let remaining = total_samples as u64 - sample_position;
            let frames = (chunk_frames as u64)
                .min(remaining)
                .min(next_event.saturating_sub(sample_position).max(1))
                as usize;
            if frames == 0 {
                break;
            }
            block.clear();
            block.resize(frames * 2, 0.0);
            self.render_into(&mut block);
            output.extend_from_slice(&block);
            sample_position += frames as u64;
        }
        Ok(output)
    }
}

/// The velocity layers one note crossfades through and the ramp that moves it.
struct DynamicPlan {
    ramp: DynamicRamp,
    /// Ascending (probe velocity, zone indices) for each layer.
    layers: Vec<(u8, Vec<usize>)>,
}

impl DynamicPlan {
    /// The per-voice crossfade state for one layer of one note.
    fn layer(&self, base_velocity: u8, layer: usize) -> DynamicLayer {
        let ladder: Vec<u8> = self.layers.iter().map(|(velocity, _)| *velocity).collect();
        DynamicLayer::new(base_velocity, self.ramp, &ladder, layer)
    }
}

/// Split `[low, high]` into the runs of zones a note selects, one run per
/// velocity layer. A bank with a single layer yields a single run.
fn velocity_layers(zones: &[crate::sf2::font::Zone], note: u8) -> Vec<(u8, Vec<usize>)> {
    let mut runs: Vec<(u8, u8, Vec<usize>)> = Vec::new();
    let mut current: Option<(u8, u8, Vec<usize>)> = None;
    for velocity in 1..=127 {
        let selected: Vec<usize> = zones
            .iter()
            .enumerate()
            .filter(|(_, zone)| zone.matches(note, velocity))
            .map(|(index, _)| index)
            .collect();
        match current.take() {
            Some((start, _end, previous)) if previous == selected => {
                current = Some((start, velocity, previous))
            }
            Some((start, end, previous)) => {
                runs.push((start, end, previous));
                current = Some((velocity, velocity, selected));
            }
            None => current = Some((velocity, velocity, selected)),
        }
    }
    if let Some(run) = current {
        runs.push(run);
    }
    runs.into_iter()
        .filter(|(_, _, selected)| !selected.is_empty())
        .take(MAX_DYNAMIC_LAYERS)
        .map(|(start, end, selected)| (((u16::from(start) + u16::from(end)) / 2) as u8, selected))
        .collect()
}

/// Push a voice, stealing the oldest one when the pool is full.
fn push_voice(voices: &mut Vec<Voice>, voice: Voice) {
    if voices.len() < MAX_VOICES {
        voices.push(voice);
    } else if let Some(oldest) = voices
        .iter()
        .enumerate()
        .min_by_key(|(_, voice)| voice.age)
        .map(|(index, _)| index)
    {
        voices[oldest] = voice;
    }
}

/// CC7/CC11 amplitude law: the MIDI volume curve the renderer uses.
fn volume_gain(normalized: f32) -> f32 {
    normalized * normalized
}

/// Controller state a channel powers up in: volume 100, expression and pan at
/// their defaults, everything else (CC1 included) at zero.
fn power_up_controllers() -> [u8; 128] {
    let mut controllers = [0u8; 128];
    controllers[7] = 100;
    controllers[10] = 64;
    controllers[11] = 127;
    controllers
}
