//! Streaming score adapter for an application-main owned VST2 instrument.
use super::{MidiMessage, SourceOutput};
use oc_audio::ProcessingFailureReport;
use oc_host::vst2::{
    MAX_PENDING_MIDI, Vst2BlockBatch, Vst2MidiMessage, Vst2OutputBlock, Vst2OwnedAuthoredPlan,
    Vst2OwnedError, Vst2OwnedInstance,
};
use std::cell::Cell;

pub struct StreamingVst2 {
    instance: Vst2OwnedInstance,
    midi: Vec<Vst2MidiMessage>,
    failure: Option<Vst2OwnedError>,
    reported: Cell<bool>,
}

impl StreamingVst2 {
    pub(super) fn load(plan: Vst2OwnedAuthoredPlan) -> Result<Self, String> {
        let mut midi = Vec::new();
        midi.try_reserve_exact(MAX_PENDING_MIDI)
            .map_err(|error| format!("Could not prepare VST2 streaming MIDI storage: {error}"))?;
        let instance = plan.execute_on_ui().map_err(|error| error.to_string())?;
        Ok(Self {
            instance,
            midi,
            failure: None,
            reported: Cell::new(false),
        })
    }

    pub(super) fn snapshot(&self) -> &oc_host::vst2::Vst2OwnedSnapshot {
        &self.instance.snapshot
    }

    fn queue(&mut self, packets: &[[u8; 3]], offset: i32) -> Result<(), String> {
        if packets.is_empty() {
            return Ok(());
        }
        if !self.snapshot().accepts_midi {
            return Err("VST2 instrument does not accept MIDI".into());
        }
        if offset < 0 || offset as u32 >= self.snapshot().max_frames {
            return Err("VST2 streaming MIDI offset is outside the prepared block".into());
        }
        if packets.len() > MAX_PENDING_MIDI - self.midi.len() {
            return Err("VST2 streaming MIDI block exceeds its event capacity".into());
        }
        self.midi.extend(
            packets
                .iter()
                .map(|&[status, data1, data2]| Vst2MidiMessage {
                    status,
                    data1,
                    data2,
                    offset,
                }),
        );
        Ok(())
    }

    pub(super) fn send_score(&mut self, event: &MidiMessage, offset: i32) -> Result<(), String> {
        if let MidiMessage::PluginParam { name, value } = event {
            return self
                .instance
                .processor
                .write_parameter_for_block(name, *value)
                .map_err(|error| error.to_string());
        }
        let packets = crate::render_cache::encode_vst2_score(event)?;
        self.queue(&packets.bytes[..packets.count], offset)
    }

    pub(super) fn send_performance(
        &mut self,
        command: &oc_synth::performance::ScheduledCommand,
        offset: i32,
    ) -> Result<(), String> {
        let bytes = command.midi_bytes();
        self.queue(
            &[[bytes[0], bytes[1], bytes.get(2).copied().unwrap_or(0)]],
            offset,
        )
    }

    pub(super) fn render(
        &mut self,
        frames: usize,
        transport: &oc_host::PluginTransport,
        output: &mut SourceOutput,
    ) {
        if self.failed() {
            output.fill_silence(frames);
            return;
        }
        let batch = Vst2BlockBatch {
            parameters: &[],
            midi: &self.midi,
        };
        let result = self.instance.processor.process_instrument_outputs_into(
            Vst2OutputBlock {
                frames,
                pairs: &output.requested,
                buffers: &mut output.buffers,
            },
            Some(transport),
            batch,
        );
        self.midi.clear();
        if let Err(error) = result {
            if self.failure.is_none() {
                self.failure = Some(error);
            }
            output.fill_silence(frames);
        }
    }

    pub(super) fn rewind(&mut self) -> bool {
        self.midi.clear();
        match self.instance.control.reset_processing_on_ui() {
            Ok(()) => {
                self.failure = None;
                self.reported.set(false);
                true
            }
            Err(error) => {
                if self.failure.is_none() {
                    self.failure = Some(error);
                }
                false
            }
        }
    }

    pub(super) fn failed(&self) -> bool {
        self.failure.is_some() || self.instance.processor.failure().failed()
    }

    pub(super) fn take_failure(&self) -> ProcessingFailureReport {
        if !self.failed() {
            return ProcessingFailureReport::Unavailable;
        }
        if self.reported.get() {
            return ProcessingFailureReport::Reported;
        }
        let detail = match self.instance.processor.failure().first_fault() {
            Some(record) => format!(
                "{:?}: {}",
                record.operation,
                record.fault.describe_for("VST2")
            ),
            None => match &self.failure {
                Some(error) => error.to_string(),
                None => return ProcessingFailureReport::Pending,
            },
        };
        self.reported.set(true);
        ProcessingFailureReport::Ready(format!("VST2 {}: {detail}", self.snapshot().name))
    }
}
