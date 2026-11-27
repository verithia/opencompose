//! Source lanes rendered side by side on the block workers.
//!
//! Each lane owns its instrument, event cursors, sounding-note words, insert
//! chain, delay line and output buffers, so lanes render concurrently into
//! lane-owned buffers. Each track then sums its lanes in lane order, followed
//! by its clips in clip order, which keeps every sum's operand order, and so
//! the mixed block, identical to rendering the lanes one by one.
//!
//! When the graph can schedule them, lanes are jobs of the graph's own block
//! ([`oc_engine::graph::BlockSources`]): a track mixes its lanes as soon as
//! they are done, and a bus starts once the tracks it hears are done. Other
//! graphs render every lane first and mix them on the render thread.

use super::*;
use oc_engine::block_workers::{BlockWorkers, DisjointSlice};
use oc_engine::graph::BlockSources;

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
        self.clip_errors.clear();
        self.clip_errors.resize(self.num_tracks, None);
        self.track_sources.reserve(
            self.num_tracks,
            self.renderers.len(),
            self.clip_lanes.len(),
            self.aux_buffers.len(),
        );
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

    /// Borrow every lane's state for one block, beside the workers and the
    /// graph, which the caller may use at the same time.
    fn lane_sources<'a>(
        &'a mut self,
        transport: &'a PluginTransport,
        chunk_start: u64,
        source_end: u64,
        actual_frames: usize,
        meter: bool,
    ) -> (
        LaneSources<'a>,
        Option<&'a mut BlockWorkers>,
        Option<&'a mut AudioGraph>,
    ) {
        let lanes = self.renderers.len();
        let sources = LaneSources {
            block: LaneBlock {
                events: &self.events,
                events_by_track: &self.events_by_track,
                performance_events: &self.performance_events,
                transport,
                sample_rate: self.sample_rate,
                chunk_start,
                source_end,
                actual_frames,
            },
            renderers: DisjointSlice::new(&mut self.renderers),
            outputs: DisjointSlice::new(&mut self.source_outputs),
            cursors: DisjointSlice::new(&mut self.track_event_cursors),
            performance_cursors: DisjointSlice::new(&mut self.performance_cursors),
            sounding: DisjointSlice::new(
                self.sounding[..lanes * SOUNDING_WORDS]
                    .as_chunks_mut::<SOUNDING_WORDS>()
                    .0,
            ),
            aux_delays: DisjointSlice::new(&mut self.aux_delays),
            chains: DisjointSlice::new(&mut self.source_chains),
            delays: DisjointSlice::new(&mut self.source_delays),
            audio: DisjointSlice::new(&mut self.lane_buffers),
            aux: DisjointSlice::new(&mut self.lane_aux),
            errors: DisjointSlice::new(&mut self.lane_errors),
            costs: DisjointSlice::new(&mut self.lane_costs),
            source_tracks: &self.source_tracks,
            tables: &self.track_sources,
            clip_errors: DisjointSlice::new(&mut self.clip_errors),
            peaks: meter.then(|| DisjointSlice::new(&mut self.meter_input_peaks)),
        };
        (sources, self.block_workers.as_mut(), self.graph.as_mut())
    }

    /// Record the first lane or clip failure of a block as its render error.
    /// Returns false when one was recorded.
    fn take_source_errors(&mut self) -> bool {
        if let Some((lane, error)) = self
            .lane_errors
            .iter_mut()
            .enumerate()
            .find_map(|(lane, error)| error.take().map(|error| (lane, error)))
        {
            self.render_error = Some(format!("Track {lane}: {error}"));
            return false;
        }
        if let Some(error) = self.clip_errors.iter_mut().find_map(Option::take) {
            self.render_error = Some(error);
            return false;
        }
        true
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
            .take()
            .expect("a project with source lanes has a transport");
        // Start the most expensive lanes first so the block does not end on
        // one long lane while the other workers idle.
        let costs = &self.lane_costs;
        self.lane_order
            .sort_unstable_by_key(|&lane| std::cmp::Reverse(costs[lane]));
        let order = std::mem::take(&mut self.lane_order);
        {
            let (sources, workers, _) =
                self.lane_sources(&transport, chunk_start, source_end, actual_frames, false);
            // SAFETY: `order` is a permutation of the lanes and each job index
            // runs once per dispatch, so every lane renders once.
            let job = |job: usize| unsafe { sources.render(order[job]) };
            let dispatched = match workers {
                Some(workers) => workers.dispatch(lanes, &job),
                None => {
                    (0..lanes).for_each(job);
                    Ok(())
                }
            };
            // A panicking instrument unwinds the render thread as it would
            // serially.
            if let Err(error) = dispatched {
                panic!("source lane: {error}");
            }
        }
        self.lane_order = order;
        self.transport = Some(transport);
        if !self.take_source_errors() {
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

    /// Render the lanes, tracks and buses of one block as one schedule on
    /// the block workers, leaving the graph output in `mixed`.
    ///
    /// Returns `None`, with no lane, clip or graph state touched, when the
    /// graph cannot schedule its sources; the caller renders the block in
    /// stages then. `Some(false)` means a render error was recorded.
    pub(super) fn render_scheduled_block(
        &mut self,
        track_chunks: &mut [Vec<f32>],
        chunk_start: u64,
        source_end: u64,
        actual_frames: usize,
        mixed: &mut Vec<f32>,
    ) -> Option<bool> {
        if self
            .block_workers
            .as_ref()
            .is_none_or(|workers| workers.parallelism() <= 1)
            || self.graph.is_none()
            || self.transport.is_none()
        {
            return None;
        }
        self.ensure_lane_buffers();
        if self.clip_errors.len() < track_chunks.len()
            || !self.track_sources.bind(
                &self.source_tracks,
                &self.clip_lanes,
                &mut self.aux_buffers,
                track_chunks.len(),
            )
        {
            return None;
        }
        let transport = self.transport.take().expect("checked above");
        let graph_transport = transport.at_sample(self.dsp_cursor);
        mixed.clear();
        mixed.resize(actual_frames * 2, 0.0);
        let meter = self.meter_hub.is_some();
        let processed = {
            let (sources, workers, graph) =
                self.lane_sources(&transport, chunk_start, source_end, actual_frames, meter);
            let (Some(workers), Some(graph)) = (workers, graph) else {
                unreachable!("checked above");
            };
            graph.process_prepared_with_sources(
                track_chunks,
                Some(&graph_transport),
                mixed,
                workers,
                &sources,
            )
        };
        self.track_sources.unbind();
        self.transport = Some(transport);
        match processed {
            Ok(false) => None,
            Err(error) => {
                self.render_error = Some(error.to_string());
                Some(false)
            }
            Ok(true) => Some(self.take_source_errors()),
        }
    }
}

/// Where each track finds its lanes, clips and auxiliary returns during one
/// block. Capacity is reserved when the lane set is prepared, so binding a
/// block does not allocate.
#[derive(Default)]
pub(super) struct TrackSources {
    /// `lanes[lane_starts[t]..lane_starts[t + 1]]` feed track `t`, ascending.
    lane_starts: Vec<usize>,
    lanes: Vec<usize>,
    clip_starts: Vec<usize>,
    clips: Vec<usize>,
    /// Clip lanes by index, addressed for one block only.
    clip_lanes: Vec<ClipRef>,
    /// Auxiliary returns by track: `aux[aux_starts[t]..aux_starts[t + 1]]`.
    aux_starts: Vec<usize>,
    aux: Vec<AuxReturn>,
    /// Next free slot per track while grouping.
    cursor: Vec<usize>,
}

/// One clip lane, addressed for one block only.
#[derive(Clone, Copy)]
struct ClipRef(*const oc_engine::clip_lane::ClipLane);

// A clip lane is shared by reference with the track job that mixes it.
const _: () = {
    const fn shared<T: Sync>() {}
    shared::<oc_engine::clip_lane::ClipLane>();
};

// SAFETY: an address is set only while the clip lanes are borrowed for one
// block and cleared before that borrow ends; clip lanes are `Sync` (checked
// above) and only read.
unsafe impl Send for ClipRef {}
// SAFETY: as above.
unsafe impl Sync for ClipRef {}

/// One auxiliary return buffer, addressed for one block only.
struct AuxReturn {
    pair: usize,
    buffer: *mut Vec<f32>,
}

// SAFETY: an address is set only while `aux_buffers` is exclusively borrowed
// for one block and cleared before that borrow ends. Its track's job is the
// only writer, and bus jobs read it only after that job finished.
unsafe impl Send for AuxReturn {}
// SAFETY: as above.
unsafe impl Sync for AuxReturn {}

impl TrackSources {
    /// Reserve every table for this many tracks, lanes, clips and returns.
    pub(super) fn reserve(&mut self, tracks: usize, lanes: usize, clips: usize, returns: usize) {
        for starts in [
            &mut self.lane_starts,
            &mut self.clip_starts,
            &mut self.aux_starts,
        ] {
            starts.reserve(tracks + 1);
        }
        self.cursor.reserve(tracks);
        self.lanes.reserve(lanes);
        self.clips.reserve(clips);
        self.clip_lanes.reserve(clips);
        self.aux.reserve(returns);
    }

    /// Group this block's lanes, clips and auxiliary returns by track.
    /// Returns false, with nothing bound, when a table lacks capacity.
    fn bind(
        &mut self,
        source_tracks: &[usize],
        clip_lanes: &ClipLanes,
        aux_buffers: &mut HashMap<(usize, usize), Vec<f32>>,
        tracks: usize,
    ) -> bool {
        let fits = |starts: &Vec<usize>| starts.capacity() > tracks;
        if !fits(&self.lane_starts)
            || !fits(&self.clip_starts)
            || !fits(&self.aux_starts)
            || self.cursor.capacity() < tracks
            || self.lanes.capacity() < source_tracks.len()
            || self.clips.capacity() < clip_lanes.len()
            || self.clip_lanes.capacity() < clip_lanes.len()
            || self.aux.capacity() < aux_buffers.len()
            || source_tracks.iter().any(|&track| track >= tracks)
            || (0..clip_lanes.len()).any(|clip| {
                clip_lanes
                    .get(clip)
                    .is_none_or(|lane| lane.track() >= tracks)
            })
            || aux_buffers.keys().any(|&(track, _)| track >= tracks)
        {
            return false;
        }
        group(
            &mut self.lane_starts,
            &mut self.lanes,
            &mut self.cursor,
            tracks,
            source_tracks.iter().copied().enumerate(),
        );
        group(
            &mut self.clip_starts,
            &mut self.clips,
            &mut self.cursor,
            tracks,
            (0..clip_lanes.len())
                .map(|clip| (clip, clip_lanes.get(clip).map_or(0, |lane| lane.track()))),
        );
        self.clip_lanes.clear();
        self.clip_lanes.extend(
            (0..clip_lanes.len())
                .filter_map(|clip| clip_lanes.get(clip).map(|lane| ClipRef(lane as *const _))),
        );
        self.aux_starts.clear();
        self.aux_starts.resize(tracks + 1, 0);
        for &(track, _) in aux_buffers.keys() {
            self.aux_starts[track + 1] += 1;
        }
        for track in 0..tracks {
            self.aux_starts[track + 1] += self.aux_starts[track];
        }
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.aux_starts[..tracks]);
        self.aux.clear();
        self.aux.resize_with(aux_buffers.len(), || AuxReturn {
            pair: 0,
            buffer: std::ptr::null_mut(),
        });
        for (&(track, pair), buffer) in aux_buffers.iter_mut() {
            self.aux[self.cursor[track]] = AuxReturn {
                pair,
                buffer: buffer as *mut _,
            };
            self.cursor[track] += 1;
        }
        true
    }

    /// Drop every per-block address.
    fn unbind(&mut self) {
        self.aux.clear();
        self.clip_lanes.clear();
    }

    fn lanes_of(&self, track: usize) -> &[usize] {
        &self.lanes[self.lane_starts[track]..self.lane_starts[track + 1]]
    }

    fn clips_of(&self, track: usize) -> &[usize] {
        &self.clips[self.clip_starts[track]..self.clip_starts[track + 1]]
    }

    fn aux_of(&self, track: usize, pair: usize) -> Option<*mut Vec<f32>> {
        self.aux[self.aux_starts[track]..self.aux_starts[track + 1]]
            .iter()
            .find(|aux| aux.pair == pair)
            .map(|aux| aux.buffer)
    }
}

/// Group `(item, track)` pairs by track in a counting sort, keeping item
/// order within each track, into `items[starts[t]..starts[t + 1]]`.
fn group(
    starts: &mut Vec<usize>,
    items: &mut Vec<usize>,
    cursor: &mut Vec<usize>,
    tracks: usize,
    pairs: impl Iterator<Item = (usize, usize)> + Clone,
) {
    starts.clear();
    starts.resize(tracks + 1, 0);
    let mut count = 0;
    for (_, track) in pairs.clone() {
        starts[track + 1] += 1;
        count += 1;
    }
    for track in 0..tracks {
        starts[track + 1] += starts[track];
    }
    cursor.clear();
    cursor.extend_from_slice(&starts[..tracks]);
    items.clear();
    items.resize(count, 0);
    for (index, track) in pairs {
        items[cursor[track]] = index;
        cursor[track] += 1;
    }
}

/// Every lane's state for one block, handed out per lane and per track by
/// the [`BlockSources`] contract.
pub(super) struct LaneSources<'a> {
    block: LaneBlock<'a>,
    renderers: DisjointSlice<'a, SmartSource>,
    outputs: DisjointSlice<'a, SourceOutput>,
    cursors: DisjointSlice<'a, usize>,
    performance_cursors: DisjointSlice<'a, usize>,
    sounding: DisjointSlice<'a, [u64; SOUNDING_WORDS]>,
    aux_delays: DisjointSlice<'a, Vec<VecDeque<f32>>>,
    chains: DisjointSlice<'a, oc_engine::graph::EffectChain>,
    delays: DisjointSlice<'a, VecDeque<f32>>,
    audio: DisjointSlice<'a, Vec<f32>>,
    aux: DisjointSlice<'a, Vec<Vec<f32>>>,
    errors: DisjointSlice<'a, Option<String>>,
    costs: DisjointSlice<'a, u64>,
    source_tracks: &'a [usize],
    tables: &'a TrackSources,
    clip_errors: DisjointSlice<'a, Option<String>>,
    /// Pre-graph input peak per track, when meters are on.
    peaks: Option<DisjointSlice<'a, f32>>,
}

impl BlockSources for LaneSources<'_> {
    fn source_tracks(&self) -> &[usize] {
        self.source_tracks
    }

    unsafe fn render(&self, lane: usize) {
        let started = std::time::Instant::now();
        // SAFETY: the caller renders each lane once per block and touches
        // none of its state concurrently, so every element is borrowed by
        // this call alone.
        let result = unsafe {
            render_lane(
                &self.block,
                lane,
                LaneParts {
                    renderer: self.renderers.get(lane),
                    output: self.outputs.get(lane),
                    cursor: self.cursors.get(lane),
                    performance_cursor: self.performance_cursors.get(lane),
                    sounding: self.sounding.get(lane),
                    aux_delays: self.aux_delays.get(lane),
                    chain: self.chains.get(lane),
                    delay: self.delays.get(lane),
                    audio: self.audio.get(lane),
                    aux: self.aux.get(lane),
                },
            )
        };
        // SAFETY: as above.
        unsafe {
            *self.errors.get(lane) = result.err();
            *self.costs.get(lane) = started.elapsed().as_nanos() as u64;
        }
    }

    unsafe fn mix_track(&self, track: usize, buffer: &mut [f32]) {
        for &lane in self.tables.lanes_of(track) {
            // SAFETY: the lane has rendered, and only this track's job reads
            // it until the next block.
            let (audio, output, aux) = unsafe {
                (
                    self.audio.get(lane),
                    self.outputs.get(lane),
                    self.aux.get(lane),
                )
            };
            for (out, sample) in buffer.iter_mut().zip(audio.iter().copied()) {
                *out += sample;
            }
            for (&pair, samples) in output.aux_pairs().iter().zip(aux.iter()) {
                if let Some(target) = self.tables.aux_of(track, pair) {
                    // SAFETY: this track's job is the return's only writer.
                    let target = unsafe { &mut *target };
                    for (out, sample) in target.iter_mut().zip(samples) {
                        *out += sample;
                    }
                }
            }
        }
        for &clip in self.tables.clips_of(track) {
            // SAFETY: bound for this block; clip lanes are only read.
            let lane = unsafe { &*self.tables.clip_lanes[clip].0 };
            if let Err(error) = lane.mix_into(self.block.chunk_start, buffer) {
                // SAFETY: one job per track.
                unsafe { *self.clip_errors.get(track) = Some(error) };
                break;
            }
        }
        if let Some(peaks) = &self.peaks
            && track < peaks.len()
        {
            // SAFETY: one job per track.
            unsafe { *peaks.get(track) = peak_of(buffer) };
        }
    }

    unsafe fn aux(&self, track: usize, pair: usize) -> Option<&[f32]> {
        // SAFETY: the track has mixed its lanes; its return is only read now.
        self.tables
            .aux_of(track, pair)
            .map(|buffer| unsafe { (*buffer).as_slice() })
    }
}
