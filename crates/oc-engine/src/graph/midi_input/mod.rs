//! Score-driven MIDI input for an audio effect, prepared before processing.

mod active_notes;
mod schedule;
#[cfg(test)]
mod tests;
use super::{AudioEffect, AudioProcessContext, AudioProcessInput, PluginTransport};
use crate::interpret::{self, InterpretSettings, MidiMessage};
use active_notes::ActiveNotes;
use oc_audio::{MidiInputError, MidiInputEvent, MidiInputMessage, ProcessingFailureReport};
use oc_dsl::ast::Project;
use schedule::{Schedule, Timed};
use std::cell::Cell;

fn source_events(project: &Project, name: &str, sample_rate: u32) -> Result<Vec<Timed>, String> {
    let index = project
        .tracks
        .iter()
        .position(|t| t.name == name)
        .ok_or_else(|| format!("unknown MIDI source track '{name}'"))?;
    oc_dsl::instruments::validate_midi_only_source(project, &project.tracks[index])?;
    if project.tracks[index].mix.mute {
        return Ok(Vec::new());
    }
    let mut source = project.clone();
    let mut track = source.tracks.remove(index);
    // Projection retains the channel the full score assigns to this track.
    track
        .channel
        .get_or_insert(if track.instrument.category == "drums" {
            9
        } else {
            index.min(15) as u8
        });
    source.tracks = vec![track];
    let settings = InterpretSettings {
        sample_rate,
        ..Default::default()
    };
    let events = interpret::try_interpret_render_at(&source, &settings, None)?;
    let mut output = Vec::new();
    for event in events {
        let message = match event.event {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => MidiInputMessage::NoteOn {
                channel,
                key: note,
                velocity,
            },
            MidiMessage::NoteOff { channel, note } => {
                MidiInputMessage::NoteOff { channel, key: note }
            }
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => MidiInputMessage::ControlChange {
                channel,
                controller,
                value,
            },
            MidiMessage::ControlChange14 {
                channel,
                controller,
                value,
            } => {
                if controller > 31 || value > 16383 {
                    return Err("invalid 14-bit MIDI control input".into());
                }
                output.push(Timed {
                    at: event.time_samples,
                    message: MidiInputMessage::ControlChange {
                        channel,
                        controller,
                        value: (value >> 7) as u8,
                    },
                });
                MidiInputMessage::ControlChange {
                    channel,
                    controller: controller + 32,
                    value: (value & 127) as u8,
                }
            }
            MidiMessage::ProgramChange { channel, program } => {
                MidiInputMessage::ProgramChange { channel, program }
            }
            MidiMessage::PitchBend { channel, value } => {
                MidiInputMessage::PitchBend { channel, value }
            }
            MidiMessage::ChannelPressure { channel, pressure } => {
                MidiInputMessage::ChannelPressure { channel, pressure }
            }
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => MidiInputMessage::PolyPressure {
                channel,
                key: note,
                pressure,
            },
            // Instrument parameter automation is local to the source, not a MIDI message.
            MidiMessage::PluginParam { .. } => continue,
            MidiMessage::NativeVoice(_) | MidiMessage::NoteExpression { .. } => {
                return Err(format!(
                    "MIDI source '{name}' contains voice expression without a MIDI 1 projection"
                ));
            }
        };
        if event.time_samples > u64::MAX - u64::from(u32::MAX) {
            return Err("MIDI input timestamp leaves no latency-alignment headroom".into());
        }
        output.push(Timed {
            at: event.time_samples,
            message,
        });
    }
    output.sort_by_key(|e| e.at);
    Ok(output)
}

pub(super) fn wrap(
    inner: Box<dyn AudioEffect>,
    project: &Project,
    source: &str,
    sample_rate: u32,
) -> Result<Box<dyn AudioEffect>, String> {
    let capacity = inner.midi_input_capacity();
    if capacity == 0 {
        return Err(format!(
            "effect '{}' has no MIDI input; refusing route from '{source}'",
            inner.name()
        ));
    }
    let events = source_events(project, source, sample_rate)?;
    let schedule = Schedule::new(events, inner.max_block_frames(), capacity)?;
    Ok(Box::new(RoutedEffect::new(inner, schedule)))
}

pub(super) fn source_changed(
    before: &Project,
    after: &Project,
    routes: &[oc_dsl::ast::InputRoute],
    sample_rate: u32,
) -> Result<bool, String> {
    for route in routes
        .iter()
        .filter(|r| r.kind == oc_dsl::ast::RouteKind::Midi)
    {
        let next = source_events(after, &route.source, sample_rate)?;
        // A rename or a newly routed source has no previous prepared schedule.
        if source_events(before, &route.source, sample_rate)
            .ok()
            .as_ref()
            != Some(&next)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

struct RoutedEffect {
    inner: Box<dyn AudioEffect>,
    schedule: Schedule,
    cursor: usize,
    expected: Option<u64>,
    input_latency: u32,
    failure: Option<MidiInputError>,
    reported: Cell<bool>,
    active: ActiveNotes,
}

impl RoutedEffect {
    fn new(inner: Box<dyn AudioEffect>, schedule: Schedule) -> Self {
        Self {
            inner,
            schedule,
            cursor: 0,
            expected: None,
            input_latency: 0,
            failure: None,
            reported: Cell::new(false),
            active: ActiveNotes::new(),
        }
    }

    fn send_block(
        &mut self,
        start: u64,
        frames: usize,
        playing: bool,
    ) -> Result<(), MidiInputError> {
        if !playing {
            if self.expected.take().is_some() {
                self.active.release_all(self.inner.as_mut())?;
                self.schedule.chase(0, |message| {
                    self.active.queue(
                        self.inner.as_mut(),
                        MidiInputEvent {
                            sample_offset: 0,
                            message,
                        },
                    )
                })?;
            }
            return Ok(());
        }
        let end = start
            .checked_add(frames as u64)
            .ok_or(MidiInputError::InvalidOffset)?;
        let delay = u64::from(self.input_latency);
        if self.expected != Some(start) {
            self.active.release_all(self.inner.as_mut())?;
            let at = start.saturating_sub(delay);
            self.cursor = if start < delay {
                0
            } else {
                self.schedule.events.partition_point(|e| e.at < at)
            };
            self.schedule.chase(at, |message| {
                self.active.queue(
                    self.inner.as_mut(),
                    MidiInputEvent {
                        sample_offset: 0,
                        message,
                    },
                )
            })?;
        }
        while let Some(event) = self.schedule.events.get(self.cursor) {
            let at = event.at + delay;
            if at >= end {
                break;
            }
            let offset = at.checked_sub(start).ok_or(MidiInputError::InvalidOffset)?;
            self.active.queue(
                self.inner.as_mut(),
                MidiInputEvent {
                    sample_offset: u32::try_from(offset)
                        .map_err(|_| MidiInputError::InvalidOffset)?,
                    message: event.message,
                },
            )?;
            self.cursor += 1;
        }
        self.expected = Some(end);
        Ok(())
    }

    fn run(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        if buffer.is_empty() {
            return;
        }
        let frames = buffer.len() / 2;
        let start = context
            .transport
            .map_or(self.expected.unwrap_or(0), |t| t.sample_pos);
        let playing = context.transport.is_none_or(|t| t.playing);
        let result = if buffer.len() % 2 != 0 || frames > self.inner.max_block_frames() as usize {
            Err(MidiInputError::InvalidOffset)
        } else if let Some(error) = self.failure {
            Err(error)
        } else {
            self.send_block(start, frames, playing)
        };
        if let Err(error) = result {
            self.failure = Some(error);
            buffer.fill(0.0);
            return;
        }
        self.inner.process_with_context(buffer, context);
    }
}

impl AudioEffect for RoutedEffect {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.run(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: None,
                input: AudioProcessInput::Main,
            },
        );
    }
    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        self.run(buffer, context);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.run(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Main,
            },
        );
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.run(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Sidechain(key),
            },
        );
    }
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        values: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.run(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Control(values),
            },
        );
    }
    fn configure_midi_input_latency(&mut self, samples: u32) {
        if self.input_latency != samples {
            self.input_latency = samples;
            self.expected = None;
        }
    }
    fn processing_failed(&self) -> bool {
        self.failure.is_some() || self.inner.processing_failed()
    }
    fn take_processing_failure(&self) -> ProcessingFailureReport {
        match self.failure {
            Some(error) if !self.reported.replace(true) => {
                ProcessingFailureReport::Ready(format!("effect MIDI input failed: {error:?}"))
            }
            Some(_) => ProcessingFailureReport::Reported,
            None => self.inner.take_processing_failure(),
        }
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.cursor = 0;
        self.expected = None;
        self.failure = None;
        self.reported.set(false);
    }
    fn prepare_block(&mut self, frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.inner.prepare_block(frames)
    }
    fn max_block_frames(&self) -> u32 {
        self.inner.max_block_frames()
    }
    fn set_param(&mut self, name: &str, value: f64) {
        self.inner.set_param(name, value);
    }
    fn prepare_parameter(&mut self, name: &str) -> Option<oc_audio::PreparedParameterHandle> {
        self.inner.prepare_parameter(name)
    }
    fn set_prepared_parameter(
        &mut self,
        handle: oc_audio::PreparedParameterHandle,
        value: f64,
    ) -> oc_audio::PreparedParameterOutcome {
        self.inner.set_prepared_parameter(handle, value)
    }
    fn queue_param_at(&mut self, name: &str, value: f64, offset: u32) -> bool {
        self.inner.queue_param_at(name, value, offset)
    }
    fn supports_param_offsets(&self) -> bool {
        self.inner.supports_param_offsets()
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.inner.get_param(name)
    }
    fn supports_sidechain(&self) -> bool {
        self.inner.supports_sidechain()
    }
    fn sidechain_source(&self) -> Option<&str> {
        self.inner.sidechain_source()
    }
    fn supports_control_input(&self) -> bool {
        self.inner.supports_control_input()
    }
    fn control_source(&self) -> Option<&str> {
        self.inner.control_source()
    }
    fn configure_sidechain_alignment(&mut self, main: u32, key: u32) -> u32 {
        self.inner.configure_sidechain_alignment(main, key)
    }
    fn latency_samples(&self) -> u32 {
        self.inner.latency_samples()
    }
    fn tail_samples(&self) -> u32 {
        self.inner.tail_samples()
    }
    fn bounce_tail_samples(&self) -> u32 {
        self.inner.bounce_tail_samples()
    }
    fn plugin_tail_samples(&self) -> Option<u32> {
        self.inner.plugin_tail_samples()
    }
    fn set_streaming_idle_gate(&mut self, enabled: bool) {
        self.inner.set_streaming_idle_gate(enabled);
    }
    fn set_seed(&mut self, seed: u32) {
        self.inner.set_seed(seed);
    }
    fn end_processing(&mut self) {
        self.inner.end_processing();
    }
    fn gain_reduction_db(&self) -> f64 {
        self.inner.gain_reduction_db()
    }
    fn band_count(&self) -> usize {
        self.inner.band_count()
    }
    fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.inner.band_gain_db(index)
    }
    fn max_gain_reduction_db(&self) -> f64 {
        self.inner.max_gain_reduction_db()
    }
    fn ceiling_margin_db(&self) -> Option<f64> {
        self.inner.ceiling_margin_db()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
}
