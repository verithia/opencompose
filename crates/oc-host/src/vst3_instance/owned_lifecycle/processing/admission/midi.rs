//! MIDI staging mutates only prepared scratch until the whole block is admitted.
use super::super::super::midi::PreparedMidi;
use super::*;

impl PreparedAdmission {
    fn push_midi(
        &mut self,
        event: Vst3PendingEvent,
        base: usize,
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3ProcessLeaseError> {
        if base
            .checked_add(self.midi_events.len())
            .is_none_or(|count| count >= budget.events)
        {
            return Err(Vst3ProcessLeaseError::EventCapacity);
        }
        if let Vst3PendingEvent::Param(parameter) = event {
            let binding = self
                .lookup(parameter.id)
                .ok_or(Vst3ProcessLeaseError::UnpreparedParameter)?;
            self.increment(binding, budget.points_per_parameter)?;
        }
        self.midi_events.push(event);
        Ok(())
    }
    pub(in crate::vst3_instance::owned_lifecycle::processing) fn stage_midi(
        &mut self,
        current: &Vst3NoteIds,
        capabilities: &PreparedMidi,
        events: &[Vst3MidiEvent],
        frames: u32,
        base: usize,
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3ProcessLeaseError> {
        self.stage_inputs(
            current,
            capabilities,
            events.iter().copied().map(Vst3OrderedEvent::Midi),
            frames,
            base,
            budget,
        )
    }
    pub(in crate::vst3_instance::owned_lifecycle::processing) fn stage_ordered(
        &mut self,
        current: &Vst3NoteIds,
        capabilities: &PreparedMidi,
        events: &[Vst3OrderedEvent],
        frames: u32,
        base: usize,
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3ProcessLeaseError> {
        self.stage_inputs(
            current,
            capabilities,
            events.iter().copied(),
            frames,
            base,
            budget,
        )
    }
    fn stage_inputs(
        &mut self,
        current: &Vst3NoteIds,
        capabilities: &PreparedMidi,
        events: impl ExactSizeIterator<Item = Vst3OrderedEvent> + Clone,
        frames: u32,
        base: usize,
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3ProcessLeaseError> {
        // Every input contributes at least one native event. Reject its exact
        // lower bound before visiting caller memory; CC14 expansion is checked
        // again by push_midi before committing any prepared scratch.
        if base
            .checked_add(events.len())
            .is_none_or(|count| count > budget.events)
        {
            return Err(Vst3ProcessLeaseError::EventCapacity);
        }
        self.midi_events.clear();
        self.midi_notes_staged = false;
        let mut previous_offset = 0;
        for input in events.clone() {
            let offset = match input {
                Vst3OrderedEvent::Midi(event) => {
                    if event.channel > 15 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    if !capabilities.supports_channel(event.channel) {
                        return Err(Vst3ProcessLeaseError::UnsupportedMidi);
                    }
                    event.sample_offset
                }
                Vst3OrderedEvent::Parameter(value) => value.sample_offset,
            };
            if offset >= frames || offset < previous_offset {
                return Err(Vst3ProcessLeaseError::InvalidBlock);
            }
            previous_offset = offset;
        }
        let needs_notes = events.clone().any(|event| {
            matches!(
                event,
                Vst3OrderedEvent::Midi(Vst3MidiEvent {
                    message: Vst3MidiMessage::NoteOn { .. }
                        | Vst3MidiMessage::NoteOff { .. }
                        | Vst3MidiMessage::PolyPressure { .. }
                        | Vst3MidiMessage::NoteExpression { .. },
                    ..
                })
            )
        });
        if needs_notes {
            if current.active_count() > self.active_limit || !self.notes.copy_prepared(current) {
                return Err(Vst3ProcessLeaseError::NoteCapacity);
            }
            self.midi_notes_staged = true;
        }
        for input in events {
            let event = match input {
                Vst3OrderedEvent::Midi(event) => event,
                Vst3OrderedEvent::Parameter(value) => {
                    self.normalized_binding(&value, frames)?;
                    self.push_midi(
                        Vst3PendingEvent::Param(Vst3ParamEvent {
                            id: value.id,
                            value: value.value,
                            sample_offset: value.sample_offset as i32,
                        }),
                        base,
                        budget,
                    )?;
                    continue;
                }
            };
            let channel = i16::from(event.channel);
            let sample_offset = event.sample_offset as i32;
            let mapped = |controller, value| {
                capabilities
                    .mapped(event.channel, controller, value)
                    .map(|(id, value)| {
                        Vst3PendingEvent::Param(Vst3ParamEvent {
                            id,
                            value,
                            sample_offset,
                        })
                    })
            };
            let pending = match event.message {
                Vst3MidiMessage::NoteOn { key, velocity } => {
                    if key > 127 || velocity > 127 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    if self.notes.active_count() >= self.active_limit
                        || !self.notes.identity_available()
                    {
                        return Err(Vst3ProcessLeaseError::NoteCapacity);
                    }
                    let key = i16::from(key);
                    let note_id = self.notes.note_on(channel, key);
                    Vst3PendingEvent::Note(Vst3NoteEvent {
                        on: true,
                        channel,
                        key,
                        velocity: f32::from(velocity) / 127.0,
                        sample_offset,
                        note_id,
                    })
                }
                Vst3MidiMessage::NoteOff { key, velocity } => {
                    if key > 127 || velocity > 127 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    let key = i16::from(key);
                    let note_id = self.notes.note_off(channel, key);
                    Vst3PendingEvent::Note(Vst3NoteEvent {
                        on: false,
                        channel,
                        key,
                        velocity: f32::from(velocity) / 127.0,
                        sample_offset,
                        note_id,
                    })
                }
                Vst3MidiMessage::PolyPressure { key, pressure } => {
                    if key > 127 || pressure > 127 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    let key = i16::from(key);
                    Vst3PendingEvent::PolyPressure(Vst3PolyPressureEvent {
                        channel,
                        key,
                        pressure: f32::from(pressure) / 127.0,
                        sample_offset,
                        note_id: self.notes.active_note_id(channel, key),
                    })
                }
                Vst3MidiMessage::ControlChange { controller, value } => {
                    if controller > 127 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    mapped(i16::from(controller), i32::from(value))?
                }
                Vst3MidiMessage::ControlChange14 { controller, value } => {
                    if controller > 31 || value > 16383 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    // Both mappings resolve before either enters even scratch. Full
                    // block capacity is checked before shared queues are committed.
                    let msb = mapped(i16::from(controller), i32::from(value >> 7))?;
                    let lsb = mapped(i16::from(controller + 32), i32::from(value & 127))?;
                    self.push_midi(msb, base, budget)?;
                    lsb
                }
                Vst3MidiMessage::PitchBend { value } => mapped(129, i32::from(value))?,
                Vst3MidiMessage::ChannelPressure { pressure } => mapped(128, i32::from(pressure))?,
                Vst3MidiMessage::ProgramChange { program } => mapped(130, i32::from(program))?,
                Vst3MidiMessage::NoteExpression { key, kind, value } => {
                    if key > 127 {
                        return Err(Vst3ProcessLeaseError::InvalidBlock);
                    }
                    let note_id = self.notes.active_note_id(channel, i16::from(key));
                    if note_id < 0 {
                        return Err(Vst3ProcessLeaseError::UnknownNote);
                    }
                    Vst3PendingEvent::NoteExpression(Vst3NoteExpressionEvent {
                        type_id: kind.native_type_id(),
                        value: capabilities.expression(event.channel, kind, value)?,
                        sample_offset,
                        note_id,
                    })
                }
            };
            self.push_midi(pending, base, budget)?;
        }
        Ok(())
    }
    pub(in crate::vst3_instance::owned_lifecycle::processing) fn commit_midi(
        &mut self,
        instance: &mut Vst3Instance,
        has_midi: bool,
    ) {
        if has_midi {
            if self.midi_notes_staged {
                std::mem::swap(&mut instance.note_ids, &mut self.notes);
            }
            instance.pending_events.extend_from_slice(&self.midi_events);
        }
    }
}
