//! Prebound numeric processing capabilities and atomic borrowed block admission.
use super::*;
use crate::preset_parameters::ParameterErrorKind;

/// Largest number of runtime parameter points per block.
pub const MAX_VST2_RUNTIME_PARAMETER_POINTS: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A binding generation is immutable for this unique, non-reused source id.
/// Display refresh never rebinds a key; a new native owner receives a new id.
pub struct Vst2RuntimeParameterKey {
    source: u64,
    slot: u32,
}
#[derive(Debug, Clone)]
/// A parameter name bound to an immutable runtime key.
pub struct Vst2RuntimeParameterBinding {
    pub name: String,
    pub key: Vst2RuntimeParameterKey,
}
#[derive(Clone, Copy, Debug)]
/// One parameter value applied for a block.
pub struct Vst2ParameterPoint {
    pub key: Vst2RuntimeParameterKey,
    pub value: f64,
}
#[derive(Clone, Copy, Debug)]
/// One raw MIDI event with a block offset.
pub struct Vst2MidiMessage {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
    pub offset: i32,
}
#[derive(Clone, Copy, Debug, Default)]
/// Parameter and MIDI events for one block.
pub struct Vst2BlockBatch<'a> {
    pub parameters: &'a [Vst2ParameterPoint],
    pub midi: &'a [Vst2MidiMessage],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Why a runtime parameter or MIDI batch was refused.
pub enum Vst2RuntimeError {
    Capacity,
    Unprepared,
    WrongSource,
    Binding,
    NonFinite,
    OutOfRange,
    Unrepresentable,
    InventoryChanged,
    EmptyBlock,
    MidiUnsupported,
}
impl std::fmt::Display for Vst2RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "VST2 runtime parameter capacity exceeded",
            Self::Unprepared => "VST2 runtime parameter binding was not prepared",
            Self::WrongSource => "VST2 runtime parameter belongs to another source",
            Self::Binding => "VST2 runtime parameter binding is invalid",
            Self::NonFinite => "VST2 runtime parameter must be finite",
            Self::OutOfRange => "VST2 runtime parameter must be in 0..=1",
            Self::Unrepresentable => "VST2 runtime parameter cannot be represented",
            Self::InventoryChanged => "VST2 runtime structure changed",
            Self::MidiUnsupported => "VST2 source does not accept MIDI",
            Self::EmptyBlock => "VST2 parameter points require a nonempty block",
        })
    }
}
impl std::error::Error for Vst2RuntimeError {}

/// A plan to load a VST2 instance with runtime parameter names.
pub struct Vst2RuntimePreparationPlan {
    pub plugin: Vst2OwnedAuthoredPlan,
    pub parameter_names: Vec<String>,
    pub max_parameter_points: usize,
}
impl Vst2RuntimePreparationPlan {
    /// Prepare the instance on the UI thread.
    pub fn execute_on_ui(self) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
        self.plugin.validate()?;
        validate_names(&self.parameter_names, self.max_parameter_points)?;
        dispatch(move || {
            let token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let source = registry::reserve(&token)?;
            let native = self.plugin.prepare(&token)?;
            registry::adopt_runtime(
                &token,
                source,
                native,
                &self.parameter_names,
                self.max_parameter_points,
            )
        })?
        .wait()
        .map_err(Vst2OwnedError::Control)?
    }
}
fn validate_names(names: &[String], points: usize) -> Result<(), Vst2OwnedError> {
    if points > MAX_VST2_RUNTIME_PARAMETER_POINTS || names.len() > MAX_VST2_AUTHORED_PARAMETERS {
        return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity));
    }
    let mut total = 0usize;
    for name in names {
        if name.len() > MAX_VST2_AUTHORED_NAME_BYTES {
            return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity));
        }
        total = total
            .checked_add(name.len())
            .filter(|sum| *sum <= MAX_VST2_AUTHORED_TOTAL_NAME_BYTES)
            .ok_or(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))?;
    }
    Ok(())
}
pub(super) struct Request<'a> {
    pub names: &'a [String],
    pub points: usize,
}
pub(super) struct Authority {
    source: u64,
    indices: Vec<i32>,
    setter: Option<abi::SetParameterProc>,
    dispatcher: abi::DispatcherProc,
    count: i32,
    inputs: i32,
    outputs: i32,
    flags: i32,
    points: usize,
}
impl Authority {
    pub(super) fn bind(
        native: &mut Vst2Instance,
        source: u64,
        names: &[String],
        points: usize,
    ) -> Result<(Self, Vec<Vst2RuntimeParameterBinding>), Vst2OwnedError> {
        validate_names(names, points)?;
        let super::super::authored_parameters::RuntimeBindings {
            indices,
            setter,
            dispatcher,
            count,
        } = super::super::authored_parameters::runtime_bindings(native, names)
            .map_err(|error| Vst2OwnedError::Parameter(Box::new(error)))?;
        let bindings = names
            .iter()
            .enumerate()
            .map(|(slot, name)| Vst2RuntimeParameterBinding {
                name: name.clone(),
                key: Vst2RuntimeParameterKey {
                    source,
                    slot: slot as u32,
                },
            })
            .collect();
        Ok((
            Self {
                source,
                indices,
                setter,
                dispatcher,
                count,
                inputs: native.num_inputs,
                outputs: native.num_outputs,
                flags: native.flags,
                points,
            },
            bindings,
        ))
    }
    fn check(&self, native: &Vst2Instance) -> Result<(), Vst2RuntimeError> {
        // SAFETY: The exclusive lease retains this admitted descriptor. Callers
        // check immutable native failure before any read, including post-call checks.
        let descriptor = unsafe { &*native.effect };
        if native.host.io_changed.load(Ordering::Acquire)
            || descriptor.num_params != self.count
            || descriptor.num_inputs != self.inputs
            || descriptor.num_outputs != self.outputs
            || descriptor.flags != self.flags
            || descriptor.set_parameter.map(|p| p as usize)
                != self.setter.map(|setter| setter as usize)
            || descriptor.dispatcher.map(|p| p as usize) != Some(self.dispatcher as usize)
        {
            return Err(Vst2RuntimeError::InventoryChanged);
        }
        Ok(())
    }
    fn index(&self, key: Vst2RuntimeParameterKey) -> Result<i32, Vst2RuntimeError> {
        if key.source != self.source {
            return Err(Vst2RuntimeError::WrongSource);
        }
        self.indices
            .get(key.slot as usize)
            .copied()
            .ok_or(Vst2RuntimeError::Binding)
    }
}
fn checked_value(value: f64) -> Result<f32, Vst2RuntimeError> {
    super::super::authored_parameters::checked_normalized_value(value).map_err(
        |error| match error {
            ParameterErrorKind::NonFinite => Vst2RuntimeError::NonFinite,
            ParameterErrorKind::OutOfRange { .. } => Vst2RuntimeError::OutOfRange,
            _ => Vst2RuntimeError::Unrepresentable,
        },
    )
}
impl Vst2ProcessorLease {
    /// Render an instrument block with batched parameter and MIDI events.
    pub fn process_instrument_batch_into(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
        batch: Vst2BlockBatch<'_>,
    ) -> Result<(), Vst2OwnedError> {
        self.process_batch(output, transport, batch, false)
    }
    /// Render an effect block with batched parameter and MIDI events.
    pub fn process_effect_batch_into(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
        batch: Vst2BlockBatch<'_>,
    ) -> Result<(), Vst2OwnedError> {
        self.process_batch(output, transport, batch, true)
    }
    fn process_batch(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
        batch: Vst2BlockBatch<'_>,
        input: bool,
    ) -> Result<(), Vst2OwnedError> {
        let _permit = self.link.acquire(PROCESSING)?;
        // SAFETY: The same exclusive gate used by all processing/control owners
        // retains the complete candidate, scratch and immutable runtime capability.
        let native = unsafe { &mut *self.link.native.as_ptr() };
        native
            .preflight_block(output.len(), self.link.snapshot.sample_rate)
            .map_err(Vst2OwnedError::Process)?;
        let frames = output.len() / 2;
        if let Some(clock) = transport {
            clock
                .checked_process_block(self.link.snapshot.sample_rate, frames)
                .map_err(Vst2OwnedError::Clock)?;
        }
        let authority = self.link.runtime.as_ref();
        if let Some(authority) = authority
            && let Err(error) = authority.check(native)
        {
            self.link.runtime_invalid.store(true, Ordering::Release);
            return Err(Vst2OwnedError::Runtime(error));
        }
        if !batch.parameters.is_empty() {
            let authority =
                authority.ok_or(Vst2OwnedError::Runtime(Vst2RuntimeError::Unprepared))?;
            if batch.parameters.len() > authority.points {
                return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity));
            }
            if frames == 0 {
                return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::EmptyBlock));
            }
            for point in batch.parameters {
                authority
                    .index(point.key)
                    .map_err(Vst2OwnedError::Runtime)?;
                checked_value(point.value).map_err(Vst2OwnedError::Runtime)?;
            }
        }
        midi::validate_block(&native.pending_midi, frames).map_err(Vst2OwnedError::Midi)?;
        let queued = native.pending_midi.len();
        let total = queued
            .checked_add(batch.midi.len())
            .ok_or(Vst2OwnedError::Midi(Vst2MidiError::Capacity))?;
        if total != 0 && !native.accepts_midi {
            return Err(Vst2OwnedError::Runtime(Vst2RuntimeError::MidiUnsupported));
        }
        if total > MAX_PENDING_MIDI.min(native.pending_midi.capacity()) {
            return Err(Vst2OwnedError::Midi(Vst2MidiError::Capacity));
        }
        for (index, message) in batch.midi.iter().enumerate() {
            midi::admit(
                PendingMidi {
                    status: message.status,
                    data1: message.data1,
                    data2: message.data2,
                    offset: message.offset,
                },
                frames as u32,
                queued + index,
                native.pending_midi.capacity(),
            )
            .map_err(Vst2OwnedError::Midi)?;
        }
        if frames == 0 {
            return Ok(());
        }
        native
            .pending_midi
            .extend(batch.midi.iter().map(|m| PendingMidi {
                status: m.status,
                data1: m.data1,
                data2: m.data2,
                offset: m.offset,
            }));
        native.prepare_without_events(frames, transport);
        let previous = native
            .host
            .process_level
            .swap(abi::process_level::REALTIME, Ordering::Relaxed);
        let result = (|| {
            if let Some(authority) = authority {
                for point in batch.parameters {
                    let index = authority
                        .index(point.key)
                        .map_err(Vst2OwnedError::Runtime)?;
                    // The complete batch's values were checked before any mutation.
                    let value = checked_value(point.value).map_err(Vst2OwnedError::Runtime)?;
                    let setter = authority
                        .setter
                        .ok_or(Vst2OwnedError::Runtime(Vst2RuntimeError::Unprepared))?;
                    native.write_parameter_callback(setter, index, value);
                    if self.link.failure.failed() {
                        return Err(Vst2OwnedError::Faulted(self.link.failure.clone()));
                    }
                    if let Err(error) = authority.check(native) {
                        self.link.runtime_invalid.store(true, Ordering::Release);
                        return Err(Vst2OwnedError::Runtime(error));
                    }
                    native.host.store_value(index, value);
                }
            }
            native.send_events();
            if self.link.failure.failed() {
                return Err(Vst2OwnedError::Faulted(self.link.failure.clone()));
            }
            if let Some(authority) = authority
                && let Err(error) = authority.check(native)
            {
                self.link.runtime_invalid.store(true, Ordering::Release);
                return Err(Vst2OwnedError::Runtime(error));
            }
            native
                .process_audio(output, input)
                .map_err(Vst2OwnedError::Process)?;
            if self.link.failure.failed() {
                return Err(Vst2OwnedError::Faulted(self.link.failure.clone()));
            }
            if let Some(authority) = authority
                && let Err(error) = authority.check(native)
            {
                self.link.runtime_invalid.store(true, Ordering::Release);
                return Err(Vst2OwnedError::Runtime(error));
            }
            Ok(())
        })();
        native.host.process_level.store(previous, Ordering::Relaxed);
        if self.link.failure.failed() {
            output.fill(0.0);
            return Err(Vst2OwnedError::Faulted(self.link.failure.clone()));
        }
        if result.is_err() {
            output.fill(0.0);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_budget_and_names_are_bounded_before_runtime_admission() {
        assert!(validate_names(&[], 0).is_ok());
        assert!(matches!(
            validate_names(&[], MAX_VST2_RUNTIME_PARAMETER_POINTS + 1),
            Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))
        ));
        assert!(matches!(
            validate_names(&vec!["x".into(); MAX_VST2_AUTHORED_PARAMETERS + 1], 0),
            Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))
        ));
        assert!(matches!(
            validate_names(&["x".repeat(MAX_VST2_AUTHORED_NAME_BYTES + 1)], 1),
            Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))
        ));
        assert!(matches!(
            validate_names(&vec!["x".repeat(MAX_VST2_AUTHORED_NAME_BYTES); 1025], 1),
            Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))
        ));
    }
    #[test]
    fn runtime_value_validation_uses_the_static_normalized_owner() {
        assert_eq!(checked_value(0.0), Ok(0.0));
        assert_eq!(checked_value(1.0), Ok(1.0));
        assert_eq!(checked_value(f64::NAN), Err(Vst2RuntimeError::NonFinite));
        assert_eq!(
            checked_value(f64::INFINITY),
            Err(Vst2RuntimeError::NonFinite)
        );
        assert_eq!(checked_value(-0.1), Err(Vst2RuntimeError::OutOfRange));
        assert_eq!(checked_value(1.1), Err(Vst2RuntimeError::OutOfRange));
    }
}
