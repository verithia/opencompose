//! Detached UI-captured MIDI capabilities; processing reads only immutable data.
use super::*;

#[derive(Debug, Clone, Copy)]
/// A MIDI message carried by an owned VST3 event.
pub enum Vst3MidiMessage {
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
        velocity: u8,
    },
    PolyPressure {
        key: u8,
        pressure: u8,
    },
    ControlChange {
        controller: u8,
        value: u8,
    },
    ControlChange14 {
        controller: u8,
        value: u16,
    },
    PitchBend {
        value: i16,
    },
    ChannelPressure {
        pressure: u8,
    },
    ProgramChange {
        program: u8,
    },
    NoteExpression {
        key: u8,
        kind: Vst3StandardNoteExpression,
        value: Vst3NoteExpressionValue,
    },
}
#[derive(Debug, Clone, Copy)]
/// One MIDI message at a block offset.
pub struct Vst3MidiEvent {
    pub channel: u8,
    pub sample_offset: u32,
    pub message: Vst3MidiMessage,
}
#[derive(Clone, Copy)]
pub(super) struct MidiBinding {
    pub id: u32,
    pub steps: i32,
    pub writable: bool,
}
pub(super) struct PreparedMidi {
    event_channels: u8,
    mappings: Vec<Option<MidiBinding>>,
    expressions: [[Option<(f64, f64)>; 6]; 16],
}
#[derive(Clone, Copy)]
pub(super) enum MidiAdmissionError {
    Unsupported,
    InvalidValue,
}
impl From<MidiAdmissionError> for Vst3ProcessLeaseError {
    fn from(error: MidiAdmissionError) -> Self {
        match error {
            MidiAdmissionError::Unsupported => Self::UnsupportedMidi,
            MidiAdmissionError::InvalidValue => Self::InvalidBlock,
        }
    }
}
impl PreparedMidi {
    pub(super) fn supports_channel(&self, channel: u8) -> bool {
        channel < self.event_channels
    }
    pub(super) fn mapped(
        &self,
        channel: u8,
        controller: i16,
        value: i32,
    ) -> Result<(u32, f64), MidiAdmissionError> {
        let binding = self.mappings[usize::from(channel) * 131 + controller as usize]
            .filter(|binding| binding.writable)
            .ok_or(MidiAdmissionError::Unsupported)?;
        let value = mapped_midi_normalized(controller, value, Some(binding.steps))
            .ok_or(MidiAdmissionError::InvalidValue)?;
        Ok((binding.id, value))
    }
    pub(super) fn expression(
        &self,
        channel: u8,
        kind: Vst3StandardNoteExpression,
        value: Vst3NoteExpressionValue,
    ) -> Result<f64, MidiAdmissionError> {
        let (low, high) = self.expressions[usize::from(channel)][kind.native_type_id() as usize]
            .ok_or(MidiAdmissionError::Unsupported)?;
        let normalized = value
            .normalized_for(kind)
            .map_err(|_| MidiAdmissionError::InvalidValue)?;
        if !(low..=high).contains(&normalized) {
            return Err(MidiAdmissionError::InvalidValue);
        }
        Ok(normalized)
    }
}

pub(super) fn active_capacity(budget: Vst3ProcessingBudget) -> usize {
    budget.events.clamp(2048, 65536)
}

fn current(instance: &Vst3Instance, revision: u64) -> Result<(), Vst3OwnedError> {
    application_main::ui_authority()?;
    instance
        .healthy()
        .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
    if instance.parameter_inventory_revision() != revision {
        return Err(Vst3OwnedError::InvalidRequest(
            "VST3 MIDI inventory changed during preparation",
        ));
    }
    Ok(())
}

pub(super) fn capture(
    token: &UiThreadToken<'_>,
    instance: &Vst3Instance,
    snapshot: &Vst3OwnedSnapshot,
) -> Result<PreparedMidi, Vst3OwnedError> {
    application_main::ui_authority()?;
    let revision = snapshot.inventory_revision;
    current(instance, revision)?;
    let mut result = PreparedMidi {
        event_channels: 0,
        mappings: vec![None; 16 * 131],
        expressions: [[None; 6]; 16],
    };
    // Event input bus zero is the semantic address used by every captured map.
    // An audio-only plugin remains usable for parameter-only processing.
    // SAFETY: UI authority retains the component through the guarded count query.
    let buses = unsafe {
        vst3_guard::bus_count(
            instance.component.as_ptr(),
            MediaTypes_::kEvent as int32,
            BusDirections_::kInput as int32,
        )
    }
    .map_err(|fault| {
        Vst3OwnedError::Native(Box::new(instance.native_fault(Operation::BusCount, fault)))
    })?;
    current(instance, revision)?;
    if !(0..=128).contains(&buses) {
        return Err(Vst3OwnedError::InvalidRequest(
            "Invalid VST3 event-input bus count",
        ));
    }
    if buses == 0 {
        return Ok(result);
    }
    // SAFETY: BusInfo has only inline SDK fields and the wrapper writes its fixed-size output.
    let mut bus: BusInfo = unsafe { std::mem::zeroed() };
    // SAFETY: retained UI component, bus zero and exact SDK output remain live.
    let code = unsafe {
        vst3_guard::bus_info(
            instance.component.as_ptr(),
            MediaTypes_::kEvent as int32,
            BusDirections_::kInput as int32,
            0,
            &mut bus,
        )
    }
    .map_err(|fault| {
        Vst3OwnedError::Native(Box::new(instance.native_fault(Operation::BusInfo, fault)))
    })?;
    current(instance, revision)?;
    if code != kResultOk {
        return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
            Operation::BusInfo,
            code,
        ))));
    }
    if bus.mediaType != MediaTypes_::kEvent as int32
        || bus.direction != BusDirections_::kInput as int32
        || !(1..=16).contains(&bus.channelCount)
    {
        return Err(Vst3OwnedError::InvalidRequest(
            "Invalid VST3 event-input bus layout",
        ));
    }
    result.event_channels = bus.channelCount as u8;
    let mut parameters = HashMap::<u32, (i32, bool)>::with_capacity(snapshot.parameters.len());
    for parameter in &snapshot.parameters {
        let writable = parameter.flags & ParameterFlags_::kIsReadOnly == 0;
        parameters
            .entry(parameter.id)
            .and_modify(|prior| prior.1 &= writable)
            .or_insert((parameter.steps, writable));
    }
    let mapping = match instance.controller.as_ref() {
        Some(controller) => controller
            .try_query::<IMidiMapping>()
            .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?,
        None => None,
    };
    if let Some(mapping) = mapping {
        for channel in 0..i16::from(result.event_channels) {
            for controller in 0..=130_i16 {
                current(instance, revision)?;
                let mut id = 0;
                // SAFETY: the UI-owned optional mapping and scalar output stay live.
                let code = unsafe {
                    vst3_guard::midi_assignment(mapping.as_ptr(), 0, channel, controller, &mut id)
                }
                .map_err(|fault| {
                    Vst3OwnedError::Native(Box::new(
                        instance.native_fault(Operation::MidiAssignment, fault),
                    ))
                })?;
                current(instance, revision)?;
                if code != kResultOk {
                    continue;
                }
                let Some(&(steps, writable)) = parameters.get(&id) else {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "MIDI mapping refers to an undeclared parameter",
                    ));
                };
                result.mappings[channel as usize * 131 + controller as usize] = Some(MidiBinding {
                    id,
                    steps,
                    writable,
                });
            }
        }
    }
    let expressions = match instance.controller.as_ref() {
        Some(controller) => controller
            .try_query::<INoteExpressionController>()
            .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?,
        None => None,
    };
    let expressions = match expressions {
        Some(expressions) => Some(expressions),
        None => instance
            .component
            .try_query::<INoteExpressionController>()
            .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?,
    };
    if let Some(expressions) = expressions {
        for channel in 0..i16::from(result.event_channels) {
            current(instance, revision)?;
            // SAFETY: UI ownership retains this optional controller through the guarded call.
            let count = unsafe { vst3_guard::expression_count(expressions.as_ptr(), 0, channel) }
                .map_err(|fault| {
                Vst3OwnedError::Native(Box::new(
                    instance.native_fault(Operation::ExpressionCount, fault),
                ))
            })?;
            current(instance, revision)?;
            if !(0..=256).contains(&count) {
                return Err(Vst3OwnedError::InvalidRequest(
                    "Invalid VST3 note-expression count",
                ));
            }
            for index in 0..count {
                current(instance, revision)?;
                // SAFETY: SDK metadata has inline primitive fields; the wrapper initializes all output.
                let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
                // SAFETY: retained UI-owned controller, bounded index and exact SDK output are live.
                let code = unsafe {
                    vst3_guard::expression_info(expressions.as_ptr(), 0, channel, index, &mut info)
                }
                .map_err(|fault| {
                    Vst3OwnedError::Native(Box::new(
                        instance.native_fault(Operation::ExpressionInfo, fault),
                    ))
                })?;
                current(instance, revision)?;
                if code != kResultOk {
                    return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
                        Operation::ExpressionInfo,
                        code,
                    ))));
                }
                let range = info.valueDesc;
                if !range.minimum.is_finite()
                    || !range.maximum.is_finite()
                    || !range.defaultValue.is_finite()
                    || range.minimum < 0.0
                    || range.maximum > 1.0
                    || range.minimum > range.maximum
                    || !(range.minimum..=range.maximum).contains(&range.defaultValue)
                    || range.stepCount < 0
                {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "Invalid VST3 normalized note-expression range",
                    ));
                }
                if info.typeId < 6 {
                    let slot = &mut result.expressions[channel as usize][info.typeId as usize];
                    if slot.is_some_and(|prior| prior != (range.minimum, range.maximum)) {
                        return Err(Vst3OwnedError::InvalidRequest(
                            "Conflicting VST3 note-expression descriptors",
                        ));
                    }
                    *slot = Some((range.minimum, range.maximum));
                }
            }
        }
    }
    let _ = token;
    instance
        .healthy()
        .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
    if revision != snapshot.inventory_revision
        || revision != instance.parameter_inventory_revision()
    {
        return Err(Vst3OwnedError::InvalidRequest(
            "VST3 MIDI inventory changed during preparation",
        ));
    }
    Ok(result)
}
