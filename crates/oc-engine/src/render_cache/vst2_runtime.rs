//! Offline VST2 schedule preparation and borrowed, bounded block processing.
use super::*;
use oc_host::vst2::{
    MAX_PENDING_MIDI, MAX_VST2_AUTHORED_NAME_BYTES, MAX_VST2_AUTHORED_PARAMETERS,
    MAX_VST2_AUTHORED_TOTAL_NAME_BYTES, MAX_VST2_RUNTIME_PARAMETER_POINTS, Vst2BlockBatch,
    Vst2MidiMessage, Vst2OwnedInstance, Vst2ParameterPoint, Vst2RuntimeParameterKey,
};
mod packets;
mod schedule;
use schedule::{Cursor, Schedule, ScoreValue};

#[cfg(test)]
mod tests;

pub(super) fn compile(
    events: &[&[&MidiEvent]],
    performance: Option<&[oc_synth::performance::ScheduledCommand]>,
    total_frames: usize,
    transport: &PluginTransport,
) -> Result<Schedule, String> {
    Schedule::compile(
        events,
        performance,
        total_frames,
        schedule::Limits {
            midi_per_block: MAX_PENDING_MIDI,
            parameter_points_per_block: MAX_VST2_RUNTIME_PARAMETER_POINTS,
            parameter_names: MAX_VST2_AUTHORED_PARAMETERS,
            name_bytes: MAX_VST2_AUTHORED_NAME_BYTES,
            total_name_bytes: MAX_VST2_AUTHORED_TOTAL_NAME_BYTES,
        },
        transport,
    )
}

pub(super) fn render(
    instance: &mut Vst2OwnedInstance,
    schedule: &Schedule,
    track_count: usize,
    frames: usize,
    transport: PluginTransport,
    cancel: Option<&crate::render_cancel::CancelToken>,
) -> Result<Vec<Vec<f32>>, String> {
    let samples = frames
        .checked_mul(2)
        .ok_or("VST2 render output size overflow")?;
    let mut buffers = Vec::new();
    for _ in 0..track_count {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(samples)
            .map_err(|error| format!("Could not reserve VST2 render output: {error}"))?;
        buffer.resize(samples, 0.0);
        buffers.push(buffer);
    }
    let keys = bound_keys(instance, schedule)?;
    let mut parameters = Vec::new();
    parameters
        .try_reserve_exact(schedule.max_parameter_points)
        .map_err(|error| format!("Could not reserve VST2 parameter batch: {error}"))?;
    let mut midi = Vec::new();
    midi.try_reserve_exact(MAX_PENDING_MIDI)
        .map_err(|error| format!("Could not reserve VST2 MIDI batch: {error}"))?;
    let mut cursor = Cursor::default();
    let mut output = [0.0f32; 128 * 2];
    for span in &schedule.blocks {
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        let position =
            usize::try_from(span.start).map_err(|_| "VST2 block position exceeds platform size")?;
        let absolute = transport
            .sample_pos
            .checked_add(span.start)
            .ok_or("VST2 transport position overflow")?;
        let block_transport = transport.at_sample(absolute);
        let count = span.frames;
        let events = cursor.block(schedule, position as u64, count)?;
        parameters.clear();
        midi.clear();
        // The compatibility renderer queues performance packets before score
        // packets in each block, even when their offsets are interleaved.
        for event in events.performance {
            append_midi(&mut midi, event.bytes, event.sample, position as u64)?;
        }
        for event in events.score {
            match event.value {
                ScoreValue::Midi(bytes) => {
                    append_midi(&mut midi, bytes, event.sample, position as u64)?
                }
                ScoreValue::Parameter { slot, value } => {
                    if parameters.len() == parameters.capacity() {
                        return Err("VST2 prepared parameter batch capacity exceeded".into());
                    }
                    parameters.push(Vst2ParameterPoint {
                        key: keys[slot],
                        value,
                    });
                }
            }
        }
        let block = &mut output[..count * 2];
        instance
            .processor
            .process_instrument_batch_into(
                block,
                Some(&block_transport),
                Vst2BlockBatch {
                    parameters: &parameters,
                    midi: &midi,
                },
            )
            .map_err(|error| {
                format!(
                    "VST2 offline source processing: {}",
                    crate::processing_failure::vst2_owned(&error, cancel)
                )
            })?;
        let start = position * 2;
        for buffer in &mut buffers {
            buffer[start..start + block.len()].copy_from_slice(block);
        }
    }
    if !cursor.performance_complete(schedule) {
        return Err("performance events exceeded the render horizon".into());
    }
    Ok(buffers)
}
fn bound_keys(
    instance: &Vst2OwnedInstance,
    schedule: &Schedule,
) -> Result<Vec<Vst2RuntimeParameterKey>, String> {
    let inventory: HashMap<_, _> = instance
        .runtime_parameters
        .iter()
        .map(|binding| (binding.name.as_str(), binding.key))
        .collect();
    schedule
        .names
        .iter()
        .map(|name| {
            inventory
                .get(name.as_str())
                .copied()
                .ok_or_else(|| format!("VST2 runtime parameter '{name}' was not prepared"))
        })
        .collect()
}
fn append_midi(
    output: &mut Vec<Vst2MidiMessage>,
    bytes: [u8; 3],
    sample: u64,
    start: u64,
) -> Result<(), &'static str> {
    if output.len() == output.capacity() {
        return Err("VST2 prepared MIDI batch capacity exceeded");
    }
    let offset = sample
        .checked_sub(start)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or("VST2 MIDI offset is outside the prepared block")?;
    output.push(Vst2MidiMessage {
        status: bytes[0],
        data1: bytes[1],
        data2: bytes[2],
        offset,
    });
    Ok(())
}
