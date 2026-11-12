//! Offline schedule compilation performs strings, sorting and packet expansion
//! before a prepared native source or borrowed processing block is admitted.
use super::super::{MidiEvent, MidiMessage, PluginTransport};
use super::packets;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub(in crate::render_cache) enum ScoreValue {
    Midi([u8; 3]),
    Parameter { slot: usize, value: f64 },
}
#[derive(Debug, Clone, Copy)]
pub(in crate::render_cache) struct TimedScore {
    pub sample: u64,
    pub value: ScoreValue,
}
#[derive(Debug, Clone, Copy)]
pub(in crate::render_cache) struct TimedMidi {
    pub sample: u64,
    pub bytes: [u8; 3],
}

pub(super) struct Limits {
    pub midi_per_block: usize,
    pub parameter_points_per_block: usize,
    pub parameter_names: usize,
    pub name_bytes: usize,
    pub total_name_bytes: usize,
}
#[derive(Debug, Clone, Copy)]
pub(in crate::render_cache) struct BlockSpan {
    pub start: u64,
    pub frames: usize,
}
#[derive(Debug)]
pub(in crate::render_cache) struct Schedule {
    pub blocks: Vec<BlockSpan>,
    pub names: Vec<String>,
    pub score: Vec<TimedScore>,
    pub performance: Vec<TimedMidi>,
    pub max_parameter_points: usize,
}
impl Schedule {
    pub(super) fn compile(
        groups: &[&[&MidiEvent]],
        performance: Option<&[oc_synth::performance::ScheduledCommand]>,
        total_frames: usize,
        limits: Limits,
        transport: &PluginTransport,
    ) -> Result<Self, String> {
        let mut events: Vec<_> = groups
            .iter()
            .flat_map(|group| group.iter().copied())
            .collect();
        events.sort_by_key(|event| event.time_samples);
        let mut names = Vec::new();
        let mut slots = HashMap::new();
        let mut name_bytes = 0usize;
        let mut score = Vec::new();
        let score_capacity = events
            .len()
            .checked_mul(2)
            .ok_or("VST2 score schedule capacity overflow")?;
        score
            .try_reserve_exact(score_capacity)
            .map_err(|error| format!("Could not reserve VST2 score schedule: {error}"))?;
        for event in events {
            if event.time_samples >= total_frames as u64 {
                continue;
            }
            if let MidiMessage::PluginParam { name, value } = &event.event {
                let slot = match slots.get(name.as_str()) {
                    Some(slot) => *slot,
                    None => {
                        if names.len() >= limits.parameter_names || name.len() > limits.name_bytes {
                            return Err("VST2 runtime parameter-name capacity exceeded".into());
                        }
                        name_bytes = name_bytes
                            .checked_add(name.len())
                            .filter(|total| *total <= limits.total_name_bytes)
                            .ok_or("VST2 runtime parameter-name byte capacity exceeded")?;
                        let slot = names.len();
                        names.push(name.clone());
                        slots.insert(name.as_str(), slot);
                        slot
                    }
                };
                score.push(TimedScore {
                    sample: event.time_samples,
                    value: ScoreValue::Parameter {
                        slot,
                        value: *value,
                    },
                });
            } else {
                let packets = packets::encode(&event.event)?;
                for &bytes in &packets.bytes[..packets.count] {
                    score.push(TimedScore {
                        sample: event.time_samples,
                        value: ScoreValue::Midi(bytes),
                    });
                }
            }
        }
        let mut prepared_performance = Vec::new();
        if let Some(commands) = performance {
            prepared_performance
                .try_reserve_exact(commands.len())
                .map_err(|error| format!("Could not reserve VST2 performance schedule: {error}"))?;
            let mut previous = 0;
            for command in commands {
                if command.sample < previous {
                    return Err("VST2 performance events are not ordered".into());
                }
                if command.sample >= total_frames as u64 {
                    return Err("performance events exceeded the render horizon".into());
                }
                previous = command.sample;
                // The existing performance owner defines byte lengths and release
                // velocity. Its temporary allocation is outside processing.
                let encoded = command.midi_bytes();
                if !(2..=3).contains(&encoded.len()) {
                    return Err("VST2 performance command has an invalid MIDI length".into());
                }
                let mut bytes = [0; 3];
                bytes[..encoded.len()].copy_from_slice(&encoded);
                prepared_performance.push(TimedMidi {
                    sample: command.sample,
                    bytes,
                });
            }
        }
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(total_frames.div_ceil(128))
            .map_err(|error| format!("Could not reserve VST2 block plan: {error}"))?;
        let mut start = 0usize;
        while start < total_frames {
            let absolute = transport
                .sample_pos
                .checked_add(start as u64)
                .ok_or("VST2 transport position overflow")?;
            let frames = transport
                .at_sample(absolute)
                .block_frames(128.min(total_frames - start));
            if frames == 0 {
                return Err("VST2 transport produced an empty processing block".into());
            }
            if blocks.len() == blocks.capacity() {
                blocks
                    .try_reserve(1)
                    .map_err(|error| format!("Could not grow VST2 block plan: {error}"))?;
            }
            blocks.push(BlockSpan {
                start: start as u64,
                frames,
            });
            start = start
                .checked_add(frames)
                .ok_or("VST2 block plan overflow")?;
        }
        let mut schedule = Self {
            names,
            score,
            performance: prepared_performance,
            max_parameter_points: 0,
            blocks,
        };
        let mut cursor = Cursor::default();
        for span in &schedule.blocks {
            let block = cursor.block(&schedule, span.start, span.frames)?;
            let midi = block.performance.len()
                + block
                    .score
                    .iter()
                    .filter(|event| matches!(event.value, ScoreValue::Midi(_)))
                    .count();
            let parameters = block
                .score
                .iter()
                .filter(|event| matches!(event.value, ScoreValue::Parameter { .. }))
                .count();
            if midi > limits.midi_per_block {
                return Err("VST2 MIDI density exceeds the prepared block capacity".into());
            }
            if parameters > limits.parameter_points_per_block {
                return Err("VST2 automation density exceeds the prepared block capacity".into());
            }
            schedule.max_parameter_points = schedule.max_parameter_points.max(parameters);
        }
        Ok(schedule)
    }
}

#[derive(Default)]
pub(super) struct Cursor {
    score: usize,
    performance: usize,
}
pub(super) struct BlockEvents<'a> {
    pub score: &'a [TimedScore],
    pub performance: &'a [TimedMidi],
}
impl Cursor {
    /// Events due in one block.
    pub fn block<'a>(
        &mut self,
        schedule: &'a Schedule,
        start: u64,
        frames: usize,
    ) -> Result<BlockEvents<'a>, &'static str> {
        if frames == 0 || frames > 128 {
            return Err("Invalid VST2 schedule block size");
        }
        let end = start
            .checked_add(frames as u64)
            .ok_or("VST2 schedule block end overflow")?;
        let mut performance = self.performance;
        while performance < schedule.performance.len()
            && schedule.performance[performance].sample < end
        {
            if schedule.performance[performance].sample < start {
                return Err("late performance event");
            }
            performance += 1;
        }
        let mut score = self.score;
        while score < schedule.score.len() && schedule.score[score].sample < end {
            if schedule.score[score].sample < start {
                return Err("late VST2 score event");
            }
            score += 1;
        }
        let events = BlockEvents {
            score: &schedule.score[self.score..score],
            performance: &schedule.performance[self.performance..performance],
        };
        self.score = score;
        self.performance = performance;
        Ok(events)
    }
    /// Whether all performance events were consumed.
    pub fn performance_complete(&self, schedule: &Schedule) -> bool {
        self.performance == schedule.performance.len()
    }
}
