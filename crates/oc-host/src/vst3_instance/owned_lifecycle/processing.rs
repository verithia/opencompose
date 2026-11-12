use super::*;
mod admission;
#[cfg(any(test, feature = "test-support"))]
pub(in crate::vst3_instance) use admission::AdmissionWork;
pub(super) use admission::PreparedAdmission;

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "processor faults remain inline so failure reporting never allocates on the realtime path"
)]
/// Why a VST3 processing lease refused a block.
pub enum Vst3ProcessLeaseError {
    Busy,
    Retired,
    InventoryChanged,
    InvalidBlock,
    Clock(oc_audio::TransportBlockError),
    /// SDK project/continuous sample clocks and their following cursor are signed.
    NativeSampleRange {
        start: u64,
        end: u64,
    },
    UnpreparedParameter,
    EventCapacity,
    UnsupportedMidi,
    UnknownNote,
    NoteCapacity,
    Native(Vst3ProcessError),
}
/// One exclusive processing owner. Cloning/shared processing is not supported.
pub struct Vst3ProcessorLease {
    link: Arc<ProcessLink>,
    admission: NonNull<PreparedAdmission>,
    _not_sync: PhantomData<Cell<()>>,
}
// SAFETY: the UI registry retains native and admission scratch allocations.
// A mutable lease and atomic gate exclude their writers; no controller API is exposed.
unsafe impl Send for Vst3ProcessorLease {}
impl Vst3ProcessorLease {
    pub(super) fn new(link: Arc<ProcessLink>, admission: NonNull<PreparedAdmission>) -> Self {
        Self {
            link,
            admission,
            _not_sync: PhantomData,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::vst3_instance) fn admission_work_for_test(&self) -> AdmissionWork {
        // SAFETY: this immutable lease borrow excludes its only scratch writer;
        // registry ownership retains the allocation through this ticket.
        unsafe { self.admission.as_ref() }.work()
    }
    #[cfg(any(test, feature = "test-support"))]
    #[expect(
        clippy::result_large_err,
        reason = "audit exercises the actual inline realtime admission result"
    )]
    pub(in crate::vst3_instance) fn preflight_admission_for_test(
        &mut self,
        frames: u32,
        parameters: &[Vst3NormalizedEvent],
    ) -> Result<AdmissionWork, Vst3ProcessLeaseError> {
        let _permit = self
            .link
            .acquire(PROCESSING)
            .map_err(|_| Vst3ProcessLeaseError::Busy)?;
        // SAFETY: this lease holds the gate and registry retains both allocations.
        let instance = unsafe { self.link.instance.as_ref() };
        // SAFETY: the exclusive gate and mutable lease admit the sole scratch writer.
        unsafe { self.admission.as_mut() }.validate(
            &instance.pending_events,
            parameters,
            frames,
            self.link.budget,
        )?;
        Ok(self.admission_work_for_test())
    }
    #[cfg(any(test, feature = "test-support"))]
    #[expect(
        clippy::result_large_err,
        reason = "audit retains the actual inline processor refusal"
    )]
    pub(in crate::vst3_instance) fn packing_work_for_test(
        &mut self,
    ) -> Result<Vst3PackingWork, Vst3ProcessLeaseError> {
        let _permit = self
            .link
            .acquire(PROCESSING)
            .map_err(|_| Vst3ProcessLeaseError::Busy)?;
        // SAFETY: this lease holds the exclusive gate and the registry retains the native allocation.
        let instance = unsafe { self.link.instance.as_ref() };
        instance
            .process_queues
            .as_ref()
            .map(|queues| queues.packing_work)
            .ok_or(Vst3ProcessLeaseError::InvalidBlock)
    }
    /// Copies one bounded block into caller-owned output. Parameters use only
    /// prepared native IDs and normalized values; no name/conversion fallback.
    #[expect(
        clippy::result_large_err,
        reason = "native processing faults stay inline without realtime allocation"
    )]
    /// Process a stereo block without MIDI.
    pub fn process_stereo_into(
        &mut self,
        block: Vst3OwnedProcessBlock<'_>,
    ) -> Result<(), Vst3ProcessLeaseError> {
        self.process_stereo_with_midi_into(block, &[])
    }
    /// MIDI is ordered by sample offset, with equal-offset order preserved.
    /// It is an atomically admitted instrument block; effect MIDI is refused
    /// until the effect processing contract carries native event lists too.
    #[expect(
        clippy::result_large_err,
        reason = "native processing faults stay inline without realtime allocation"
    )]
    /// Process a stereo block with ordered MIDI events.
    pub fn process_stereo_with_midi_into(
        &mut self,
        block: Vst3OwnedProcessBlock<'_>,
        midi: &[Vst3MidiEvent],
    ) -> Result<(), Vst3ProcessLeaseError> {
        let Vst3OwnedProcessBlock {
            frames,
            transport,
            parameters,
            input,
            sidechain,
            output,
        } = block;
        self.process_into(
            ProcessBlock {
                frames,
                transport,
                parameters,
                input,
                sidechain,
                output: OutputTarget::Stereo(output),
            },
            midi,
            None,
        )
    }

    /// Process an instrument once and copy its flattened stereo pairs into
    /// exact-sized caller storage. No output allocation or controller access.
    #[expect(
        clippy::result_large_err,
        reason = "native faults remain inline on the processing path"
    )]
    /// Process an instrument block with MIDI and output selection.
    pub fn process_outputs_with_midi_into(
        &mut self,
        block: Vst3OwnedInstrumentBlock<'_>,
        midi: &[Vst3MidiEvent],
    ) -> Result<(), Vst3ProcessLeaseError> {
        self.process_into(
            ProcessBlock {
                frames: block.frames,
                transport: block.transport,
                parameters: block.parameters,
                input: None,
                sidechain: None,
                output: OutputTarget::Pairs {
                    pairs: block.output_pairs,
                    outputs: block.outputs,
                },
            },
            midi,
            None,
        )
    }

    /// Full source order across normalized and mapped MIDI parameter events.
    /// `block.parameters` must be empty; every submitted event is in `events`.
    #[expect(
        clippy::result_large_err,
        reason = "native faults remain inline on the processing path"
    )]
    /// Process an instrument block with ordered parameter and MIDI events.
    pub fn process_outputs_with_ordered_events_into(
        &mut self,
        block: Vst3OwnedInstrumentBlock<'_>,
        events: &[Vst3OrderedEvent],
    ) -> Result<(), Vst3ProcessLeaseError> {
        if !block.parameters.is_empty() {
            return Err(Vst3ProcessLeaseError::InvalidBlock);
        }
        self.process_into(
            ProcessBlock {
                frames: block.frames,
                transport: block.transport,
                parameters: &[],
                input: None,
                sidechain: None,
                output: OutputTarget::Pairs {
                    pairs: block.output_pairs,
                    outputs: block.outputs,
                },
            },
            &[],
            Some(events),
        )
    }
    #[expect(
        clippy::result_large_err,
        reason = "native faults remain inline on the processing path"
    )]
    fn process_into(
        &mut self,
        block: ProcessBlock<'_>,
        midi: &[Vst3MidiEvent],
        ordered: Option<&[Vst3OrderedEvent]>,
    ) -> Result<(), Vst3ProcessLeaseError> {
        let ProcessBlock {
            frames,
            transport,
            parameters,
            input,
            sidechain,
            mut output,
        } = block;
        if (!midi.is_empty() && input.is_some())
            || frames == 0
            || frames > self.link.snapshot.max_frames
            || !output.valid(frames as usize, &self.link.snapshot.outputs)
            || input.is_some_and(|input| input.len() != frames as usize * 2)
            || sidechain.is_some_and(|input| input.len() != frames as usize * 2)
        {
            return Err(Vst3ProcessLeaseError::InvalidBlock);
        }
        let _permit = self.link.acquire(PROCESSING).map_err(|error| match error {
            Vst3OwnedError::Retired => Vst3ProcessLeaseError::Retired,
            _ => Vst3ProcessLeaseError::Busy,
        })?;
        // SAFETY: this unique mutable processing lease holds the exclusive gate;
        // registry ownership keeps the allocation stable until every lease drops.
        let instance = unsafe { &mut *self.link.instance.as_ptr() };
        if let Some(transport) = transport {
            let end = transport
                .checked_process_block(instance.sample_rate as u32, frames as usize)
                .map_err(Vst3ProcessLeaseError::Clock)?;
            if end > i64::MAX as u64 {
                return Err(Vst3ProcessLeaseError::NativeSampleRange {
                    start: transport.sample_pos,
                    end,
                });
            }
        }
        if let Err(error) = instance.healthy() {
            output.clear();
            return Err(Vst3ProcessLeaseError::Native(error));
        }
        // The instrument path has no sidechain delivery; effect routing must
        // resolve a declared auxiliary bus before any parameters are queued.
        if sidechain.is_some()
            && (input.is_none()
                || instance
                    .sidechain_input_bus(instance.sidechain_input_index)
                    .is_none())
        {
            return Err(Vst3ProcessLeaseError::InvalidBlock);
        }
        if self.link.snapshot.source != self.link.source
            || instance.parameter_inventory_revision() != self.link.snapshot.inventory_revision
        {
            return Err(Vst3ProcessLeaseError::InventoryChanged);
        }
        // SAFETY: the registry retains this scratch Box beside the native owner
        // until all tickets retire. This unique lease holds the exclusive gate.
        unsafe { self.admission.as_mut() }.validate(
            &instance.pending_events,
            parameters,
            frames,
            self.link.budget,
        )?;
        // SAFETY: the same exclusive processing permit retains the prepared scratch.
        let admission = unsafe { self.admission.as_mut() };
        if let Some(events) = ordered {
            admission.stage_ordered(
                &instance.note_ids,
                &self.link.midi,
                events,
                frames,
                instance.pending_events.len(),
                self.link.budget,
            )?;
        } else {
            admission.stage_midi(
                &instance.note_ids,
                &self.link.midi,
                midi,
                frames,
                instance.pending_events.len() + parameters.len(),
                self.link.budget,
            )?;
        }
        admission.commit_midi(
            instance,
            ordered.is_some_and(|e| !e.is_empty()) || !midi.is_empty(),
        );
        for event in parameters {
            instance
                .pending_events
                .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                    id: event.id,
                    value: event.value,
                    sample_offset: event.sample_offset as i32,
                }));
        }
        let result = if let Some(input) = input {
            let OutputTarget::Stereo(output) = &mut output else {
                unreachable!("effect input requires stereo output")
            };
            output.copy_from_slice(input);
            let mut params = std::mem::take(&mut instance.param_scratch);
            params.clear();
            for event in instance.pending_events.drain(..) {
                if let Vst3PendingEvent::Param(event) = event {
                    params.push(event);
                }
            }
            instance.process_effect_block(
                output,
                sidechain,
                instance.sample_rate as u32,
                transport.cloned(),
                &mut params,
            );
            instance.param_scratch = params;
            instance.healthy().map_err(Vst3ProcessLeaseError::Native)
        } else {
            instance
                .render_block(frames, transport.cloned())
                .map_err(|error| {
                    output.clear();
                    Vst3ProcessLeaseError::Native(error)
                })?;
            output.copy_channels(&instance.output_layout.channels);
            Ok(())
        };
        if result.is_ok()
            && instance.parameter_inventory_revision() != self.link.snapshot.inventory_revision
        {
            output.clear();
            return Err(Vst3ProcessLeaseError::InventoryChanged);
        }
        result
    }
}
impl Drop for Vst3ProcessorLease {
    fn drop(&mut self) {
        // The registry's strong record is retained until this Arc and every
        // control ticket have dropped. Only main then destroys/deallocates it.
        self.link.retire.store(true, Ordering::Release);
    }
}
/// Inputs and output for one owned VST3 block.
pub struct Vst3OwnedProcessBlock<'a> {
    pub frames: u32,
    pub transport: Option<&'a PluginTransport>,
    pub parameters: &'a [Vst3NormalizedEvent],
    pub input: Option<&'a [f32]>,
    pub sidechain: Option<&'a [f32]>,
    pub output: &'a mut [f32],
}

/// An instrument-only, multi-output block. Each destination has exactly
/// `frames * 2` samples; pair indices address flattened native channels.
pub struct Vst3OwnedInstrumentBlock<'a> {
    pub frames: u32,
    pub transport: Option<&'a PluginTransport>,
    pub parameters: &'a [Vst3NormalizedEvent],
    pub output_pairs: &'a [usize],
    pub outputs: &'a mut [Vec<f32>],
}
struct ProcessBlock<'a> {
    frames: u32,
    transport: Option<&'a PluginTransport>,
    parameters: &'a [Vst3NormalizedEvent],
    input: Option<&'a [f32]>,
    sidechain: Option<&'a [f32]>,
    output: OutputTarget<'a>,
}
enum OutputTarget<'a> {
    Stereo(&'a mut [f32]),
    Pairs {
        pairs: &'a [usize],
        outputs: &'a mut [Vec<f32>],
    },
}
impl OutputTarget<'_> {
    fn valid(&self, frames: usize, ports: &[AudioPort]) -> bool {
        match self {
            Self::Stereo(output) => {
                ports.len() == 1 && ports[0].channels == 2 && output.len() == frames * 2
            }
            Self::Pairs { pairs, outputs } => {
                let channels: usize = ports.iter().map(|p| p.channels as usize).sum();
                !pairs.is_empty()
                    && pairs.len() == outputs.len()
                    && pairs.iter().all(|p| *p < channels.div_ceil(2))
                    && outputs.iter().all(|o| o.len() == frames * 2)
            }
        }
    }
    fn clear(&mut self) {
        match self {
            Self::Stereo(output) => output.fill(0.0),
            Self::Pairs { outputs, .. } => {
                for output in outputs.iter_mut() {
                    output.fill(0.0);
                }
            }
        }
    }
    fn copy_channels(&mut self, channels: &[Vec<f32>]) {
        fn copy(output: &mut [f32], pair: usize, channels: &[Vec<f32>]) {
            for (frame, samples) in output.chunks_exact_mut(2).enumerate() {
                samples[0] = channels.get(pair * 2).map_or(0.0, |ch| ch[frame]);
                samples[1] = channels.get(pair * 2 + 1).map_or(0.0, |ch| ch[frame]);
            }
        }
        match self {
            Self::Stereo(output) => copy(output, 0, channels),
            Self::Pairs { pairs, outputs } => {
                for (&pair, output) in pairs.iter().zip(outputs.iter_mut()) {
                    copy(output, pair, channels);
                }
            }
        }
    }
}
