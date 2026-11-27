//! Source lanes rendered side by side on the block workers.
//!
//! Each lane owns its instrument, event cursors, sounding-note words, insert
//! chain, delay line and output buffers, so lanes render concurrently into
//! lane-owned buffers. The render thread then sums every lane into its track
//! buffer and auxiliary returns in lane order, which keeps each sum's operand
//! order, and so the mixed block, identical to rendering the lanes one by one.

use super::*;
use oc_engine::block_workers::DisjointSlice;

/// Lane state that stays fixed for one block.
struct LaneBlock<'a> {
    events: &'a [MidiEvent],
    events_by_track: &'a [Vec<usize>],
    performance_events: &'a [Vec<oc_synth::performance::ScheduledCommand>],
    transport: &'a PluginTransport,
    sample_rate: u32,
    chunk_start: u64,
    source_end: u64,
    actual_frames: usize,
}

/// One lane's mutable state for one block.
struct LaneParts<'a> {
    renderer: &'a mut SmartSource,
    output: &'a mut SourceOutput,
    cursor: &'a mut usize,
    performance_cursor: &'a mut usize,
    sounding: &'a mut [u64],
    aux_delays: &'a mut [VecDeque<f32>],
    chain: &'a mut oc_engine::graph::EffectChain,
    delay: &'a mut VecDeque<f32>,
    audio: &'a mut Vec<f32>,
    aux: &'a mut Vec<Vec<f32>>,
}

/// Render one lane through its inserts and delay into `parts.audio`, with its
/// auxiliary pairs in `parts.aux` (in `aux_pairs` order).
fn render_lane(block: &LaneBlock<'_>, lane: usize, parts: LaneParts<'_>) -> Result<(), String> {
    let LaneParts {
        renderer,
        output,
        cursor,
        performance_cursor,
        sounding,
        aux_delays,
        chain,
        delay,
        audio,
        aux,
    } = parts;
    let frames_total = block.actual_frames * 2;
    audio.clear();
    let aux_pairs = output.aux_pairs().len();
    if aux.len() != aux_pairs {
        aux.resize_with(aux_pairs, || Vec::with_capacity(CHUNK_FRAMES * 2));
    }
    for buffer in aux.iter_mut() {
        buffer.clear();
        buffer.resize(frames_total, 0.0);
    }
    let track_events = &block.events_by_track[lane];
    let performance = &block.performance_events[lane];
    let chunk_start = block.chunk_start;
    let source_end = block.source_end;
    let mut pos = chunk_start;
    while pos < source_end {
        if renderer.supports_score_offsets() || renderer.performance_uses_offsets() {
            // Same 128-frame hosted processing quantum as offline export.
            // Keep sample offsets; chopping at each note changes plugin
            // block-dependent envelopes and round-robin behavior.
            let transport = block.transport.at_sample(pos);
            let frames = transport.block_frames((source_end - pos).min(128) as usize);
            let end = pos + frames as u64;
            while *performance_cursor < performance.len() {
                let command = &performance[*performance_cursor];
                if command.sample >= end {
                    break;
                }
                renderer
                    .send_performance_at(
                        command,
                        command.sample.saturating_sub(pos) as u32,
                        frames as u32,
                    )
                    .map_err(|error| error.to_string())?;
                track_sounding_command(sounding, command.channel, &command.message);
                *performance_cursor += 1;
            }
            while *cursor < track_events.len() {
                let ev = &block.events[track_events[*cursor]];
                if ev.time_samples >= end {
                    break;
                }
                renderer
                    .send_score_at(&ev.event, ev.time_samples.saturating_sub(pos) as i32)
                    .map_err(|error| error.to_string())?;
                track_sounding(sounding, &ev.event);
                *cursor += 1;
            }
            renderer.render_into(frames, block.sample_rate, transport, output);
            audio.extend_from_slice(output.main());
            accumulate_lane_aux(output, aux_delays, aux, (pos - chunk_start) as usize * 2);
            pos = end;
            continue;
        }
        while *performance_cursor < performance.len() {
            let command = &performance[*performance_cursor];
            if command.sample > pos {
                break;
            }
            renderer
                .send_performance_at(command, 0, 1)
                .map_err(|error| error.to_string())?;
            track_sounding_command(sounding, command.channel, &command.message);
            *performance_cursor += 1;
        }
        // Send all events at or before the current position.
        while *cursor < track_events.len() {
            let ev = &block.events[track_events[*cursor]];
            if ev.time_samples > pos {
                break;
            }
            renderer
                .send(&ev.event)
                .map_err(|error| error.to_string())?;
            track_sounding(sounding, &ev.event);
            *cursor += 1;
        }
        // Render up to the next event, clock change or sub-chunk boundary, so
        // event timing stays sample-accurate.
        let next_event_time = if *cursor < track_events.len() {
            block.events[track_events[*cursor]].time_samples
        } else {
            source_end
        };
        let next_event_time = next_event_time.min(
            performance
                .get(*performance_cursor)
                .map_or(source_end, |event| event.sample),
        );
        let remaining = (source_end - pos) as usize;
        let until_next = (next_event_time - pos) as usize;
        let transport = block.transport.at_sample(pos);
        let clock_limit = transport
            .next_clock_change_sample()
            .map(|end| (end - pos) as usize)
            .unwrap_or(remaining);
        let frames_to_render = SUB_CHUNK_FRAMES
            .min(remaining)
            .min(until_next.max(1))
            .min(clock_limit.max(1));
        renderer.render_into(frames_to_render, block.sample_rate, transport, output);
        audio.extend_from_slice(output.main());
        // Auxiliary outputs bypass the lane chain and the fader.
        accumulate_lane_aux(output, aux_delays, aux, (pos - chunk_start) as usize * 2);
        pos += frames_to_render as u64;
    }

    audio.resize(frames_total, 0.0);
    // Past the program end, delayed auxiliary audio keeps draining.
    let drain_start = source_end.saturating_sub(chunk_start) as usize * 2;
    for (buffer, delay) in aux.iter_mut().zip(aux_delays.iter_mut()) {
        if !delay.is_empty() {
            for out in &mut buffer[drain_start..] {
                delay.push_back(0.0);
                *out += delay.pop_front().unwrap();
            }
        }
    }
    let transport = block.transport.at_sample(chunk_start);
    chain
        .try_process_with_context(
            audio,
            AudioProcessContext {
                sample_rate: block.sample_rate,
                transport: Some(&transport),
                input: AudioProcessInput::Main,
            },
        )
        .map_err(|error| error.to_string())?;
    if !delay.is_empty() {
        for sample in audio.iter_mut() {
            delay.push_back(*sample);
            *sample = delay.pop_front().unwrap();
        }
    }
    Ok(())
}

/// Accumulate a source's auxiliary pairs into the lane's own buffers.
fn accumulate_lane_aux(
    output: &SourceOutput,
    delays: &mut [VecDeque<f32>],
    aux: &mut [Vec<f32>],
    offset: usize,
) {
    for (((_, samples), delay), buffer) in output.aux().zip(delays.iter_mut()).zip(aux.iter_mut()) {
        let start = offset.min(buffer.len());
        let frames = samples.len().min(buffer.len() - start);
        let buffer = &mut buffer[start..start + frames];
        if delay.is_empty() {
            for (out, sample) in buffer.iter_mut().zip(&samples[..frames]) {
                *out += sample;
            }
        } else {
            for (out, sample) in buffer.iter_mut().zip(&samples[..frames]) {
                delay.push_back(*sample);
                *out += delay.pop_front().unwrap();
            }
        }
    }
}

/// Frames a built-in source renders between event checks.
const SUB_CHUNK_FRAMES: usize = 64;

impl RenderState {
    /// Threads lanes and graph chains run on, including the render thread.
    #[cfg(test)]
    pub(super) fn lane_workers(&self) -> usize {
        self.block_workers
            .as_ref()
            .map_or(1, oc_engine::block_workers::BlockWorkers::parallelism)
    }

    /// Size every lane-owned buffer for the current lane set, so the first
    /// block after a load or splice reuses capacity like any other.
    pub(super) fn prepare_lane_buffers(&mut self) {
        self.ensure_lane_buffers();
        for (lane, aux) in self.lane_aux.iter_mut().enumerate() {
            let pairs = self.source_outputs[lane].aux_pairs().len();
            aux.resize_with(pairs, Vec::new);
            for buffer in aux.iter_mut() {
                if buffer.capacity() < CHUNK_FRAMES * 2 {
                    buffer.reserve(CHUNK_FRAMES * 2 - buffer.len());
                }
            }
        }
        for buffer in &mut self.lane_buffers {
            if buffer.capacity() < CHUNK_FRAMES * 2 {
                buffer.reserve(CHUNK_FRAMES * 2 - buffer.len());
            }
        }
    }

    /// Size the per-lane tables to the current lane set. Steady blocks only
    /// reuse them; `prepare_lane_buffers` grows them when the lane set changes.
    fn ensure_lane_buffers(&mut self) {
        let lanes = self.renderers.len();
        if self.lane_buffers.len() != lanes {
            self.lane_buffers
                .resize_with(lanes, || Vec::with_capacity(CHUNK_FRAMES * 2));
            self.lane_aux.resize_with(lanes, Vec::new);
            self.lane_errors.resize(lanes, None);
            self.lane_costs.resize(lanes, 0);
            self.lane_order.clear();
            self.lane_order.extend(0..lanes);
        }
        if self.sounding.len() < lanes * SOUNDING_WORDS {
            self.sounding.resize(lanes * SOUNDING_WORDS, 0);
        }
    }

    /// Render every lane of one block into the track chunks and auxiliary
    /// returns. Returns false after recording a render error.
    pub(super) fn render_lanes_into(
        &mut self,
        track_chunks: &mut [Vec<f32>],
        chunk_start: u64,
        source_end: u64,
        actual_frames: usize,
    ) -> bool {
        self.ensure_lane_buffers();
        let lanes = self.renderers.len();
        if lanes == 0 {
            return true;
        }
        let transport = self
            .transport
            .as_ref()
            .expect("a project with source lanes has a transport");
        // Start the most expensive lanes first so the block does not end on
        // one long lane while the other workers idle.
        let costs = &self.lane_costs;
        self.lane_order
            .sort_unstable_by_key(|&lane| std::cmp::Reverse(costs[lane]));
        let block = LaneBlock {
            events: &self.events,
            events_by_track: &self.events_by_track,
            performance_events: &self.performance_events,
            transport,
            sample_rate: self.sample_rate,
            chunk_start,
            source_end,
            actual_frames,
        };
        let renderers = DisjointSlice::new(&mut self.renderers);
        let outputs = DisjointSlice::new(&mut self.source_outputs);
        let cursors = DisjointSlice::new(&mut self.track_event_cursors);
        let performance_cursors = DisjointSlice::new(&mut self.performance_cursors);
        let sounding = DisjointSlice::new(
            self.sounding[..lanes * SOUNDING_WORDS]
                .as_chunks_mut::<SOUNDING_WORDS>()
                .0,
        );
        let aux_delays = DisjointSlice::new(&mut self.aux_delays);
        let chains = DisjointSlice::new(&mut self.source_chains);
        let delays = DisjointSlice::new(&mut self.source_delays);
        let audio = DisjointSlice::new(&mut self.lane_buffers);
        let aux = DisjointSlice::new(&mut self.lane_aux);
        let errors = DisjointSlice::new(&mut self.lane_errors);
        let lane_costs = DisjointSlice::new(&mut self.lane_costs);
        let order = &self.lane_order;
        let job = |job: usize| {
            let lane = order[job];
            let started = std::time::Instant::now();
            // SAFETY: `lane_order` is a permutation of the lanes and each job
            // index runs once per dispatch, so every lane's elements are
            // borrowed by exactly one job.
            let result = unsafe {
                render_lane(
                    &block,
                    lane,
                    LaneParts {
                        renderer: renderers.get(lane),
                        output: outputs.get(lane),
                        cursor: cursors.get(lane),
                        performance_cursor: performance_cursors.get(lane),
                        sounding: sounding.get(lane),
                        aux_delays: aux_delays.get(lane),
                        chain: chains.get(lane),
                        delay: delays.get(lane),
                        audio: audio.get(lane),
                        aux: aux.get(lane),
                    },
                )
            };
            // SAFETY: as above.
            unsafe {
                *errors.get(lane) = result.err();
                *lane_costs.get(lane) = started.elapsed().as_nanos() as u64;
            }
        };
        let dispatched = match self.block_workers.as_mut() {
            Some(workers) => workers.dispatch(lanes, &job),
            None => {
                (0..lanes).for_each(job);
                Ok(())
            }
        };
        // A panicking instrument unwinds the render thread as it would serially.
        if let Err(error) = dispatched {
            panic!("source lane: {error}");
        }
        if let Some((lane, error)) = self
            .lane_errors
            .iter_mut()
            .enumerate()
            .find_map(|(lane, error)| error.take().map(|error| (lane, error)))
        {
            self.render_error = Some(format!("Track {lane}: {error}"));
            return false;
        }
        for lane in 0..lanes {
            let target = self.source_tracks[lane];
            for (out, sample) in track_chunks[target]
                .iter_mut()
                .zip(self.lane_buffers[lane].iter().copied())
            {
                *out += sample;
            }
            for (&pair, samples) in self.source_outputs[lane]
                .aux_pairs()
                .iter()
                .zip(&self.lane_aux[lane])
            {
                if let Some(bus) = self.aux_buffers.get_mut(&(target, pair)) {
                    for (out, sample) in bus.iter_mut().zip(samples) {
                        *out += sample;
                    }
                }
            }
        }
        true
    }
}
