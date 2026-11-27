//! Streaming graph blocks spread over persistent block workers.
//!
//! The serial block runs track chains in key-plan order, routes every track,
//! then runs bus chains source-before-target. Two kinds of work are
//! independent of their neighbours and run concurrently here:
//!
//! - a track chain that reads no key and no control signal depends only on its
//!   own buffer, so all of them run at once before the serial pass that taps,
//!   refreshes control sources and processes the chains that do read signals;
//! - a bus chain depends only on the buses that route into it and the buses
//!   whose key or control it reads. A bus becomes ready when the last of those
//!   finishes, and every worker takes the ready bus that heads the most
//!   expensive remaining path to the master. A routing graph is often a long
//!   chain of heavy buses beside many light ones; starting that chain first
//!   keeps it from waiting behind work that has slack.
//!
//! Instead of each finished bus pushing its return into its targets, a bus
//! gathers its sources' returns when its own job starts, in ascending source
//! order, and the master gathers every return in bus order at the end. Each
//! destination therefore sums the same operands in the same order as the
//! serial block, so the output is bit-identical whichever thread runs a chain
//! and in whichever order. Dispatch neither allocates nor locks: the plan is
//! built in `prepare` and every per-block table reuses its capacity.
//!
//! When the caller renders its track audio inside the block
//! ([`BlockSources`]), sources and tracks join the same schedule. A source
//! job renders one lane; a track job mixes the track's sources and runs its
//! chain, gate, polarity and fader; a bus job gathers the sends of the tracks
//! it hears before its bus returns, in track order and, within a track,
//! pre-fader, post-fader and auxiliary sends, as the serial routing loop
//! writes them. A bus therefore starts as soon as the tracks it hears are
//! done instead of after every source in the project.

use super::prepared::{PreparedBus, PreparedGraph, SignalSlot};
use super::*;
use crate::block_workers::{BlockWorkers, DisjointSlice};
use gain_ramp::{GainRamp, TrackRamps};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Concurrency fixed when the graph is prepared.
pub(super) struct ParallelPlan {
    /// Tracks whose chains read neither keys nor controls and have devices.
    free_tracks: Vec<usize>,
    /// Membership of `free_tracks` by track index.
    is_free: Vec<bool>,
    /// Bus scheduling, or `None` when the buses run serially: a backward
    /// route or key, or fewer than two buses.
    buses: Option<BusPlan>,
    /// Source, track and bus scheduling for a caller that renders its
    /// sources inside the block, or `None` without one or with a backward
    /// route or key.
    flow: Option<BusPlan>,
}

/// Audio a caller renders inside a graph block: instrument lanes, clips and
/// auxiliary outputs that feed the graph's tracks.
///
/// The graph runs each source as a job, then mixes every track's sources at
/// the start of that track's job. Implementations keep per-source and
/// per-track state that only these calls touch, so the calls' contracts are
/// what make that state exclusive.
pub trait BlockSources: Sync {
    /// Track that each source feeds, by source index.
    fn source_tracks(&self) -> &[usize];

    /// Render source `source` into storage it owns.
    ///
    /// # Safety
    /// Called at most once per block for each source, never concurrently with
    /// another call for the same source or with `mix_track` for its track.
    unsafe fn render(&self, source: usize);

    /// Sum the sources of `track` into `buffer`, which holds the block's
    /// silence, and into the track's auxiliary outputs.
    ///
    /// # Safety
    /// Called once per block for each track, after `render` returned for
    /// every source of the track, never concurrently with another call for
    /// the same track.
    unsafe fn mix_track(&self, track: usize, buffer: &mut [f32]);

    /// Auxiliary output `pair` of `track` for this block.
    ///
    /// # Safety
    /// Called only after `mix_track` returned for `track` in this block.
    /// Concurrent calls only read.
    unsafe fn aux(&self, track: usize, pair: usize) -> Option<&[f32]>;
}

/// Bus dependencies and the per-block state that schedules them; in a flow
/// plan, also the sources and tracks ahead of the buses.
struct BusPlan {
    /// Node `sources + tracks + b` is bus `b`; sources come first, then
    /// tracks. Both counts are zero when only the buses are scheduled.
    sources: usize,
    tracks: usize,
    /// Track each source feeds, as the plan was built for.
    source_tracks: Vec<usize>,
    /// Track sends by target bus, in serial routing order:
    /// `sends[send_starts[b]..send_starts[b + 1]]`.
    send_starts: Vec<usize>,
    sends: Vec<TrackSend>,
    /// Sends to a bus the graph lacks. They mix nothing, but a silent track
    /// still advances their delay lines.
    orphan_sends: Vec<TrackSend>,
    /// Tracks with a pre-fader send keep a copy of the pre-fader buffer.
    pre_tracks: Vec<bool>,
    pre_fader: Vec<Vec<f32>>,
    /// What each track job left for the buses that hear it.
    track_blocks: Vec<TrackBlock>,
    /// Addresses valid for one block only, aligned with `sends` or by track.
    send_ramps: Table<GainRamp>,
    send_delays: Table<StereoDelayCompensator>,
    track_ramps: Table<TrackRamps>,
    track_taps: Vec<SignalSlot>,
    /// Bus-to-bus routes by target: `edges[edge_starts[t]..edge_starts[t + 1]]`
    /// lists target `t`'s sources in ascending order.
    edge_starts: Vec<usize>,
    edges: Vec<BusEdge>,
    /// Nodes each node must wait for: a bus's route sources, key sources and
    /// the tracks it hears; a track's sources.
    dependencies: Vec<usize>,
    /// Nodes waiting on each node:
    /// `dependents[dependent_starts[n]..dependent_starts[n + 1]]`.
    dependent_starts: Vec<usize>,
    dependents: Vec<usize>,
    /// Longest chain of dependent buses, for diagnostics.
    depth: usize,
    /// Most nodes on one dependency level: the workers a block can keep busy.
    /// A graph that is one chain runs on the render thread alone, without
    /// waking workers that would only wait for their turn.
    width: usize,
    /// Prepared bus index by name.
    bus_slots: HashMap<String, usize>,
    /// Route scratch for each target that has an incoming route.
    scratch: Vec<Vec<f32>>,
    /// Whether each bus reached its routes this block.
    live: Vec<bool>,
    /// Addresses valid for one block only, cleared before it returns.
    nodes: Table<BusNode>,
    edge_delays: Table<StereoDelayCompensator>,
    ramps: Table<GainRamp>,
    taps: Vec<SignalSlot>,
    /// Remaining path cost of each node and the nodes sorted by it, highest
    /// first; refreshed from `queue.cost` before each block.
    rank: Vec<u64>,
    order: Vec<usize>,
    queue: BusQueue,
}

/// Scheduling state shared by the workers during one block.
struct BusQueue {
    /// Dependencies still running, per node.
    pending: Vec<AtomicUsize>,
    state: Vec<AtomicU8>,
    /// Nodes not yet finished.
    remaining: AtomicUsize,
    /// Set when a job panics, so the other workers stop waiting.
    aborted: AtomicBool,
    /// Whether each bus reaches its routes this block, by bus.
    live: Vec<AtomicBool>,
    /// Running average of each node's job time in nanoseconds.
    cost: Vec<AtomicU64>,
}

const WAITING: u8 = 0;
const READY: u8 = 1;
const TAKEN: u8 = 2;

/// Polls of an empty ready set before a worker yields its core.
const SPINS_BEFORE_YIELD: u32 = 256;

/// How long a worker waits for a bus to become ready before it leaves the
/// block. Buses become ready only when a running bus finishes, and the thread
/// that ran it keeps working, so leaving never strands a bus; it only frees
/// the core while the graph narrows to a serial chain.
const IDLE_BEFORE_LEAVING: std::time::Duration = std::time::Duration::from_micros(100);

/// Cost assumed for each device of a bus that has not run yet, so the first
/// block already favours long chains of devices.
const SEED_COST_PER_DEVICE_NS: u64 = 20_000;

/// Cost assumed for a source that has not rendered yet: an instrument is
/// usually heavier than one device.
const SEED_SOURCE_COST_NS: u64 = 4 * SEED_COST_PER_DEVICE_NS;

/// Raises the abort flag if a bus job unwinds before handing on its dependents.
struct AbortOnUnwind<'a>(&'a AtomicBool);

impl Drop for AbortOnUnwind<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// One bus return summed into another bus.
struct BusEdge {
    source: usize,
    /// Index into the source's prepared routes.
    route: usize,
}

/// One track send gathered by its target bus.
#[derive(Clone, Copy)]
struct TrackSend {
    track: usize,
    kind: SendKind,
    /// Index into the track's sends, or its auxiliary sends for `Aux`.
    index: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SendKind {
    PreFader,
    PostFader,
    Aux,
}

/// How a track left its routing stage this block.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum TrackState {
    /// Contributes nothing and advances nothing: an empty or control-only
    /// track.
    #[default]
    Skipped,
    /// Gated to silence; its route delay lines advance through silence.
    Silent,
    /// Routes audio.
    Live,
}

#[derive(Clone, Copy, Default)]
struct TrackBlock {
    state: TrackState,
    width: SourceWidth,
    /// Source and chain latency the send envelopes align to.
    latency: u32,
}

/// Addresses valid for one block only. They are filled while the graph is
/// exclusively borrowed and cleared before that borrow ends.
struct Table<T>(Vec<*mut T>);

// SAFETY: the table only holds addresses while the graph is exclusively
// borrowed by one block. An entry is written only by the one job that owns it
// (a bus's node, ramp and incoming delays; a track's ramps; a send's ramp and
// delay) and read by other jobs only after that job handed them on with
// release ordering.
unsafe impl<T> Send for Table<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for Table<T> {}

impl<T> Table<T> {
    fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }
}

/// What the caller hands one scheduled block.
struct FlowInput<'a> {
    tracks: &'a mut [Vec<f32>],
    /// Interleaved samples in the block, which a graph without tracks cannot
    /// read from a track buffer.
    samples: usize,
    transport: Option<&'a PluginTransport>,
    sources: &'a dyn BlockSources,
}

/// Track state shared by one block's track and bus jobs.
struct FlowBlock<'a> {
    sources: &'a dyn BlockSources,
    buffers: DisjointSlice<'a, Vec<f32>>,
    chains: DisjointSlice<'a, EffectChain>,
    widths: DisjointSlice<'a, SourceWidth>,
    blocks: DisjointSlice<'a, TrackBlock>,
    pre_fader: DisjointSlice<'a, Vec<f32>>,
    routing: &'a [TrackRouting],
    key_plan: &'a KeyPlan,
    source_latencies: &'a [u32],
    performance_sends: &'a HashMap<(usize, String), crate::performance_send::SendEnvelope>,
    prepared_tracks: &'a [prepared::PreparedTrack],
    track_taps: &'a prepared::PreparedSignals,
    send_start: u64,
    ramp_frames: u32,
    samples: usize,
}

impl ParallelPlan {
    pub(super) fn new(graph: &AudioGraph, buses: &[PreparedBus]) -> Self {
        let order: Vec<usize> = if graph.key_plan.order.len() == graph.track_chains.len() {
            graph.key_plan.order.clone()
        } else {
            (0..graph.track_chains.len()).collect()
        };
        let mut is_free = vec![false; graph.track_chains.len()];
        let mut free_tracks = Vec::new();
        for index in order {
            let chain = &graph.track_chains[index];
            if chain.sidechain_sources().next().is_none()
                && chain.control_sources().next().is_none()
            {
                is_free[index] = true;
                if !chain.effects.is_empty() {
                    free_tracks.push(index);
                }
            }
        }
        let flow = graph
            .block_sources
            .as_deref()
            .filter(|_| is_free.iter().all(|free| *free))
            .and_then(|sources| BusPlan::new(graph, buses, Some(sources)));
        Self {
            free_tracks,
            is_free,
            buses: BusPlan::new(graph, buses, None),
            flow,
        }
    }
}

impl BusPlan {
    /// Plan the buses alone, or with `sources` the sources and tracks too.
    fn new(graph: &AudioGraph, buses: &[PreparedBus], sources: Option<&[usize]>) -> Option<Self> {
        let (source_count, track_count) = match sources {
            Some(sources) => (sources.len(), graph.routing.len()),
            None if buses.len() <= 1 => return None,
            None => (0, 0),
        };
        if sources.is_some_and(|sources| sources.iter().any(|&track| track >= track_count)) {
            return None;
        }
        let first_bus = source_count + track_count;
        let count = first_bus + buses.len();
        let slots: HashMap<&str, usize> = buses
            .iter()
            .enumerate()
            .map(|(index, bus)| (bus.name.as_str(), index))
            .collect();
        let mut incoming: Vec<Vec<BusEdge>> = (0..buses.len()).map(|_| Vec::new()).collect();
        let mut heard: Vec<Vec<TrackSend>> = (0..buses.len()).map(|_| Vec::new()).collect();
        let mut orphan_sends = Vec::new();
        let mut pre_tracks = vec![false; track_count];
        let mut waits_on: Vec<Vec<usize>> = vec![Vec::new(); count];
        if let Some(sources) = sources {
            for (source, &track) in sources.iter().enumerate() {
                waits_on[source_count + track].push(source);
            }
            for (track, route) in graph.routing.iter().enumerate() {
                let pre = route
                    .sends
                    .iter()
                    .enumerate()
                    .filter(|(_, send)| send.pre_fader);
                let post = route
                    .sends
                    .iter()
                    .enumerate()
                    .filter(|(_, send)| !send.pre_fader);
                let sends = pre
                    .map(|(index, send)| (SendKind::PreFader, index, send.bus.as_str()))
                    .chain(
                        post.map(|(index, send)| (SendKind::PostFader, index, send.bus.as_str())),
                    )
                    .chain(
                        route
                            .aux_sends
                            .iter()
                            .enumerate()
                            .map(|(index, send)| (SendKind::Aux, index, send.bus.as_str())),
                    );
                for (kind, index, bus) in sends {
                    let send = TrackSend { track, kind, index };
                    match slots.get(bus) {
                        Some(&target) => {
                            heard[target].push(send);
                            waits_on[first_bus + target].push(source_count + track);
                            pre_tracks[track] |= kind == SendKind::PreFader;
                        }
                        None => orphan_sends.push(send),
                    }
                }
            }
        }
        for (index, bus) in buses.iter().enumerate() {
            let node = graph.bus_chains.get(&bus.name)?;
            for key in node
                .chain
                .sidechain_sources()
                .chain(node.chain.control_sources())
            {
                if sources.is_some() {
                    // A track publishes its own name's key from its job.
                    for (track, name) in graph.track_names.iter().enumerate().take(track_count) {
                        if name == key {
                            waits_on[first_bus + index].push(source_count + track);
                        }
                    }
                }
                if key == bus.name {
                    continue;
                }
                if let Some(&source) = slots.get(key) {
                    // The serial order reads a later bus's key from the
                    // previous block; only that order can reproduce it.
                    if source >= index {
                        return None;
                    }
                    waits_on[first_bus + index].push(first_bus + source);
                }
            }
            for (route, target) in bus.routes.iter().enumerate() {
                if let Some(&target) = slots.get(target.target.as_str()) {
                    if index >= target {
                        return None;
                    }
                    incoming[target].push(BusEdge {
                        source: index,
                        route,
                    });
                    waits_on[first_bus + target].push(first_bus + index);
                }
            }
        }
        let mut edge_starts = vec![0];
        let mut edges = Vec::new();
        for list in incoming {
            edges.extend(list);
            edge_starts.push(edges.len());
        }
        let mut send_starts = vec![0];
        let mut sends = Vec::new();
        for list in heard {
            sends.extend(list);
            send_starts.push(sends.len());
        }
        let mut dependencies = vec![0; count];
        let mut feeds: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut levels = vec![0usize; count];
        for (target, sources) in waits_on.iter_mut().enumerate() {
            sources.sort_unstable();
            sources.dedup();
            dependencies[target] = sources.len();
            for &source in sources.iter() {
                feeds[source].push(target);
                // Sources precede targets, so a source's level is final here.
                levels[target] = levels[target].max(levels[source] + 1);
            }
        }
        let mut dependent_starts = vec![0];
        let mut dependents = Vec::new();
        for list in feeds {
            dependents.extend(list);
            dependent_starts.push(dependents.len());
        }
        let capacity = buses_capacity(graph);
        let scratch = (0..buses.len())
            .map(|target| {
                if edge_starts[target + 1] > edge_starts[target]
                    || send_starts[target + 1] > send_starts[target]
                {
                    Vec::with_capacity(capacity)
                } else {
                    Vec::new()
                }
            })
            .collect();
        let pre_fader = pre_tracks
            .iter()
            .map(|&pre| {
                if pre {
                    Vec::with_capacity(capacity)
                } else {
                    Vec::new()
                }
            })
            .collect();
        let cost = (0..count)
            .map(|node| {
                let seed = if node < source_count {
                    SEED_SOURCE_COST_NS
                } else {
                    let devices = if node < first_bus {
                        graph
                            .track_chains
                            .get(node - source_count)
                            .map_or(0, |chain| chain.effects.len())
                    } else {
                        graph
                            .bus_chains
                            .get(&buses[node - first_bus].name)
                            .map_or(0, |node| node.chain.effects.len())
                    };
                    (devices as u64 + 1) * SEED_COST_PER_DEVICE_NS
                };
                AtomicU64::new(seed)
            })
            .collect();
        let bus_count = buses.len();
        let mut per_level = vec![0usize; levels.iter().copied().max().map_or(0, |level| level + 1)];
        for &level in &levels {
            per_level[level] += 1;
        }
        let width = per_level.iter().copied().max().unwrap_or(0).max(1);
        let bus_levels = &levels[first_bus..];
        let bus_floor = bus_levels.iter().copied().min().unwrap_or(0);
        Some(Self {
            sources: source_count,
            tracks: track_count,
            source_tracks: sources.map(<[usize]>::to_vec).unwrap_or_default(),
            send_ramps: Table::with_capacity(sends.len()),
            send_delays: Table::with_capacity(sends.len()),
            send_starts,
            sends,
            orphan_sends,
            pre_tracks,
            pre_fader,
            track_blocks: vec![TrackBlock::default(); track_count],
            track_ramps: Table::with_capacity(track_count),
            track_taps: Vec::with_capacity(track_count),
            width,
            depth: bus_levels
                .iter()
                .copied()
                .max()
                .map_or(0, |level| level - bus_floor + 1),
            edge_delays: Table::with_capacity(edges.len()),
            edge_starts,
            edges,
            dependencies,
            dependent_starts,
            dependents,
            bus_slots: buses
                .iter()
                .enumerate()
                .map(|(index, bus)| (bus.name.clone(), index))
                .collect(),
            scratch,
            live: vec![false; bus_count],
            nodes: Table::with_capacity(bus_count),
            ramps: Table::with_capacity(bus_count),
            taps: Vec::with_capacity(bus_count),
            rank: vec![0; count],
            order: (0..count).collect(),
            queue: BusQueue {
                pending: (0..count).map(|_| AtomicUsize::new(0)).collect(),
                state: (0..count).map(|_| AtomicU8::new(WAITING)).collect(),
                remaining: AtomicUsize::new(0),
                aborted: AtomicBool::new(false),
                live: (0..bus_count).map(|_| AtomicBool::new(false)).collect(),
                cost,
            },
        })
    }

    fn first_bus(&self) -> usize {
        self.sources + self.tracks
    }

    fn bus_count(&self) -> usize {
        self.edge_starts.len() - 1
    }

    fn dependents_of(&self, node: usize) -> &[usize] {
        &self.dependents[self.dependent_starts[node]..self.dependent_starts[node + 1]]
    }

    /// Rank every node by the costliest path from it to the master and order
    /// the nodes by rank. Targets follow their sources in index order, so one
    /// backward pass sees every dependent's rank first.
    fn rank_nodes(&mut self) {
        for node in (0..self.rank.len()).rev() {
            let downstream = self
                .dependents_of(node)
                .iter()
                .map(|&dependent| self.rank[dependent])
                .max()
                .unwrap_or(0);
            let own = self.queue.cost[node].load(Ordering::Relaxed);
            self.rank[node] = own.saturating_add(downstream);
        }
        let rank = &self.rank;
        // In-place and without allocation; ties keep a stable node order.
        self.order
            .sort_unstable_by_key(|&node| (std::cmp::Reverse(rank[node]), node));
    }

    fn reset_queue(&self) {
        let queue = &self.queue;
        queue
            .remaining
            .store(self.dependencies.len(), Ordering::Relaxed);
        queue.aborted.store(false, Ordering::Relaxed);
        for (node, &count) in self.dependencies.iter().enumerate() {
            queue.pending[node].store(count, Ordering::Relaxed);
            let state = if count == 0 { READY } else { WAITING };
            queue.state[node].store(state, Ordering::Relaxed);
        }
        for live in &queue.live {
            live.store(false, Ordering::Relaxed);
        }
    }
}

impl BusQueue {
    /// Claim the ready node with the highest rank.
    fn take(&self, order: &[usize]) -> Option<usize> {
        order.iter().copied().find(|&node| {
            self.state[node].load(Ordering::Relaxed) == READY
                && self.state[node]
                    .compare_exchange(READY, TAKEN, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
        })
    }

    /// Fold one measured job time into the node's running average.
    fn record_cost(&self, node: usize, nanos: u64) {
        let previous = self.cost[node].load(Ordering::Relaxed);
        let average = previous - previous / 8 + nanos / 8;
        self.cost[node].store(average.max(1), Ordering::Relaxed);
    }

    /// Run ready nodes until every node has finished or one has panicked.
    ///
    /// Every worker of the block runs this loop. A node that finishes marks
    /// each dependent whose last dependency it was as ready, and the same
    /// worker immediately looks for the best ready node, so a chain of single
    /// dependents stays on one thread instead of waiting for another to wake.
    /// A worker that finds nothing ready for [`IDLE_BEFORE_LEAVING`] returns.
    fn work<'a>(
        &self,
        order: &[usize],
        dependents: impl Fn(usize) -> &'a [usize],
        run: impl Fn(usize),
    ) {
        let mut idle = 0u32;
        let mut idle_since: Option<Instant> = None;
        loop {
            if self.remaining.load(Ordering::Acquire) == 0 || self.aborted.load(Ordering::Acquire) {
                return;
            }
            let Some(node) = self.take(order) else {
                idle = idle.saturating_add(1);
                if idle < SPINS_BEFORE_YIELD {
                    std::hint::spin_loop();
                } else if idle_since.get_or_insert_with(Instant::now).elapsed()
                    >= IDLE_BEFORE_LEAVING
                {
                    return;
                } else {
                    std::thread::yield_now();
                }
                continue;
            };
            idle = 0;
            idle_since = None;
            let abort = AbortOnUnwind(&self.aborted);
            let started = Instant::now();
            run(node);
            self.record_cost(node, started.elapsed().as_nanos() as u64);
            for &dependent in dependents(node) {
                if self.pending[dependent].fetch_sub(1, Ordering::AcqRel) == 1 {
                    self.state[dependent].store(READY, Ordering::Release);
                }
            }
            self.remaining.fetch_sub(1, Ordering::AcqRel);
            std::mem::forget(abort);
        }
    }
}

/// Samples one bus buffer holds; route scratch matches it.
fn buses_capacity(graph: &AudioGraph) -> usize {
    graph
        .bus_chains
        .values()
        .map(|bus| bus.buffer.capacity())
        .max()
        .unwrap_or(0)
}

impl AudioGraph {
    /// Track chains with the free chains spread over `workers`.
    pub(super) fn process_track_effects_parallel(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<&PluginTransport>,
        prepared: &mut PreparedGraph,
        workers: &mut BlockWorkers,
    ) {
        if track_buffers.len() != self.track_chains.len() {
            return self.process_track_effects_prepared(track_buffers, transport, None, prepared);
        }
        let planned = self.key_plan.order.len() == track_buffers.len();
        prepared.track_taps.clear();
        let frames = track_buffers.first().map_or(0, |buffer| buffer.len() / 2);
        // Free chains read no controls, so the block's starting values serve.
        self.prepared_controls(prepared, frames);
        let plan = prepared
            .parallel
            .take()
            .expect("prepared graphs carry a plan");
        for &index in &plan.free_tracks {
            if !self.routing.get(index).is_some_and(|route| route.mute)
                || self.key_plan.is_tapped(index)
            {
                self.tap_spectrum(&SpectrumTarget::Track(index), &track_buffers[index]);
            }
        }
        {
            let chains = DisjointSlice::new(&mut self.track_chains);
            let buffers = DisjointSlice::new(track_buffers);
            let routing = &self.routing;
            let key_plan = &self.key_plan;
            let taps = &prepared.track_taps;
            let controls = &prepared.controls;
            let sample_rate = self.sample_rate;
            let free = &plan.free_tracks;
            let result = workers.dispatch(free.len(), &|job| {
                let index = free[job];
                if routing.get(index).is_some_and(|route| route.mute) && !key_plan.is_tapped(index)
                {
                    return;
                }
                // SAFETY: `free_tracks` names each track once, and each job
                // index runs once per dispatch.
                let (chain, buffer) = unsafe { (chains.get(index), buffers.get(index)) };
                chain.process_with_lookup(buffer, taps, controls, sample_rate, transport);
            });
            // A panicking chain unwinds the render thread as it would serially.
            if let Err(error) = result {
                panic!("track chain: {error}");
            }
        }
        // The serial pass keeps key-plan order for taps, control sources and
        // every chain that reads them.
        for position in 0..track_buffers.len() {
            let index = if planned {
                self.key_plan.order[position]
            } else {
                position
            };
            let tapped = self.key_plan.is_tapped(index);
            if self.routing.get(index).is_some_and(|route| route.mute) && !tapped {
                continue;
            }
            let buffer = &mut track_buffers[index];
            if !plan.is_free[index] {
                self.tap_spectrum(&SpectrumTarget::Track(index), buffer);
                self.prepared_controls(prepared, frames);
                if let Some(chain) = self.track_chains.get_mut(index) {
                    chain.process_with_lookup(
                        buffer,
                        &prepared.track_taps,
                        &prepared.controls,
                        self.sample_rate,
                        transport,
                    );
                }
            } else if self.track_chains[index].effects.is_empty() {
                // An empty chain still taps the spectrum in order.
                self.tap_spectrum(&SpectrumTarget::Track(index), buffer);
            }
            let muted = self.routing.get(index).is_some_and(|route| route.mute);
            self.refresh_control_sources(index, if muted { &[] } else { buffer }, frames);
            if tapped && let Some(name) = self.track_names.get(index) {
                prepared.track_taps.set(name, buffer);
            }
        }
        prepared.parallel = Some(plan);
    }

    /// Run the bus chains on `workers` and gather their master returns.
    /// Returns false when the plan cannot schedule this graph (a backward
    /// route or key, an active analyser), leaving the caller's serial path in
    /// charge.
    pub(super) fn process_buses_parallel(
        &mut self,
        prepared: &mut PreparedGraph,
        master: &mut [f32],
        transport: Option<&PluginTransport>,
        ramp_frames: u32,
        workers: &mut BlockWorkers,
    ) -> bool {
        if self.spectrum.is_some() {
            return false;
        }
        let Some(mut parallel) = prepared.parallel.take() else {
            return false;
        };
        let Some(plan) = parallel.buses.as_mut() else {
            prepared.parallel = Some(parallel);
            return false;
        };
        if !self.bind_bus_tables(plan, prepared) {
            prepared.parallel = Some(parallel);
            return false;
        }
        plan.rank_nodes();
        plan.reset_queue();
        let sample_rate = self.sample_rate;
        // Moved out for the dispatch so each job can borrow its own entry while
        // the rest of the plan is shared.
        let mut scratch = std::mem::take(&mut plan.scratch);
        {
            let plan = &*plan;
            let scratch = DisjointSlice::new(&mut scratch);
            let jobs = BusJobs {
                plan,
                scratch: &scratch,
                flow: None,
                taps: &prepared.taps,
                controls: &prepared.controls,
                sample_rate,
                transport,
                ramp_frames,
            };
            let queue = &plan.queue;
            // SAFETY: the queue hands each bus to one job per block, after
            // every bus it gathers from or reads a key of.
            let run = |bus: usize| unsafe { jobs.run_bus(bus) };
            let result = workers.dispatch(workers.parallelism().min(plan.width), &|_| {
                queue.work(&plan.order, |bus| plan.dependents_of(bus), run);
            });
            // A panicking chain unwinds the render thread as it would serially.
            if let Err(error) = result {
                panic!("bus chain: {error}");
            }
        }
        plan.scratch = scratch;
        self.gather_bus_returns(plan, prepared, master);
        plan.unbind();
        prepared.parallel = Some(parallel);
        true
    }

    /// Point the bus tables at this block's nodes, ramps, delays and taps.
    /// Returns false, with the tables cleared, when a planned bus is missing.
    fn bind_bus_tables(&mut self, plan: &mut BusPlan, prepared: &PreparedGraph) -> bool {
        let count = plan.bus_count();
        plan.nodes.0.clear();
        plan.nodes.0.resize(count, std::ptr::null_mut());
        for (name, node) in self.bus_chains.iter_mut() {
            if let Some(&slot) = plan.bus_slots.get(name) {
                plan.nodes.0[slot] = node;
            }
        }
        if plan.nodes.0.iter().any(|node| node.is_null()) {
            plan.nodes.0.clear();
            return false;
        }
        plan.edge_delays.0.clear();
        for edge in &plan.edges {
            let key = &prepared.buses[edge.source].routes[edge.route].delay_key;
            plan.edge_delays.0.push(
                self.bus_route_delays
                    .get_mut(key)
                    .map_or(std::ptr::null_mut(), |delay| delay as *mut _),
            );
        }
        plan.ramps.0.clear();
        plan.taps.clear();
        for bus in &prepared.buses {
            plan.ramps.0.push(
                self.ramps
                    .buses
                    .get_mut(&bus.name)
                    .map_or(std::ptr::null_mut(), |ramp| ramp as *mut _),
            );
            // A track of the same name publishes that key itself.
            let tapped = !self.track_names.iter().any(|name| name == &bus.name);
            plan.taps.push(if tapped {
                prepared.taps.slot(&bus.name)
            } else {
                SignalSlot::NONE
            });
        }
        true
    }

    /// The master gathers every live bus's return in bus order, as the
    /// serial block sums them.
    fn gather_bus_returns(
        &mut self,
        plan: &mut BusPlan,
        prepared: &mut PreparedGraph,
        master: &mut [f32],
    ) {
        for (live, flag) in plan.live.iter_mut().zip(&plan.queue.live) {
            *live = flag.load(Ordering::Acquire);
        }
        for (index, bus_plan) in prepared.buses.iter().enumerate() {
            if !plan.live[index] {
                continue;
            }
            // SAFETY: no job is running; the node is only read.
            let source = unsafe { &(*plan.nodes.0[index]).buffer };
            for route in &bus_plan.routes {
                if route.target != "master" {
                    continue;
                }
                prepared.routed.clear();
                prepared.routed.extend_from_slice(source);
                if let Some(delay) = self.bus_route_delays.get_mut(&route.delay_key) {
                    delay.process(&mut prepared.routed);
                }
                sum_buffer(master, &prepared.routed, 1.0);
            }
        }
    }

    /// Whether [`Self::process_prepared_with_sources`] can schedule a block of
    /// `tracks` track buffers.
    fn schedules_sources(&self, tracks: usize, sources: &[usize]) -> bool {
        let Some(plan) = self
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.parallel.as_ref())
            .and_then(|parallel| parallel.flow.as_ref())
        else {
            return false;
        };
        // Control sources and analysers read tracks in an order the schedule
        // does not keep, so those graphs render their sources first.
        self.spectrum.is_none()
            && self.control_runtimes.is_empty()
            && self.control_gains.iter().all(Option::is_none)
            && !self.control_only.contains(&true)
            && self.track_chains.len() == tracks
            && self.routing.len() == tracks
            && self.track_widths.len() == tracks
            && plan.tracks == tracks
            && plan.source_tracks == sources
    }

    /// Process one block whose track audio `sources` render inside it: each
    /// source, track and bus is a job on `workers`, and a bus starts once
    /// the tracks it hears are done. The output is identical to rendering
    /// every source, mixing it into `tracks` and calling
    /// [`Self::process_prepared`].
    ///
    /// `tracks` hold the block's silence on entry and the routed track
    /// buffers on return. Returns `Ok(false)` without touching any state or
    /// source when the graph cannot schedule its sources (see
    /// [`Self::set_block_sources`]); the caller then renders them itself.
    pub fn process_prepared_with_sources(
        &mut self,
        tracks: &mut [Vec<f32>],
        transport: Option<&PluginTransport>,
        output: &mut [f32],
        workers: &mut BlockWorkers,
        sources: &dyn BlockSources,
    ) -> Result<bool, PreparationError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or(PreparationError::NotPrepared)?;
        let no_aux = HashMap::new();
        prepared.preflight(self, tracks, &no_aux, output.len())?;
        if let Some(transport) = transport {
            transport
                .checked_block_end(output.len() / 2)
                .map_err(PreparationError::TransportRange)?;
        }
        if let Some(error) = self
            .track_chains
            .iter()
            .chain(self.bus_chains.values().map(|bus| &bus.chain))
            .chain(std::iter::once(&self.master_chain))
            .find_map(|chain| chain.boundary.failure())
        {
            return Err(PreparationError::ChainProcessing(error));
        }
        if output.is_empty()
            || workers.parallelism() <= 1
            || tracks.iter().any(|track| track.len() != output.len())
            || !self.schedules_sources(tracks.len(), sources.source_tracks())
        {
            return Ok(false);
        }
        let mut prepared = self.prepared.take().unwrap();
        let mut parallel = prepared.parallel.take().expect("checked above");
        let plan = parallel.flow.as_mut().expect("checked above");
        if !self.bind_bus_tables(plan, &prepared) {
            prepared.parallel = Some(parallel);
            self.prepared = Some(prepared);
            return Ok(false);
        }
        if let Some(controls) = &self.live_controls {
            controls.latch();
        }
        if let Some(macros) = &self.preset_macros {
            macros.latch();
        }
        self.set_streaming_idle_gate(true);
        self.mix_flow(
            plan,
            FlowInput {
                tracks,
                samples: output.len(),
                transport,
                sources,
            },
            workers,
            &mut prepared,
        );
        plan.unbind();
        prepared.parallel = Some(parallel);
        let mut master = std::mem::take(&mut prepared.master);
        let ramp_frames = gain_ramp::ramp_frames(self.sample_rate);
        self.finish_master(&mut master, &prepared, transport, ramp_frames, None);
        output.copy_from_slice(&master);
        prepared.master = master;
        self.prepared = Some(prepared);
        Ok(true)
    }

    /// Run the sources, tracks and buses of one block and sum the master
    /// input into `prepared.master`.
    fn mix_flow(
        &mut self,
        plan: &mut BusPlan,
        input: FlowInput<'_>,
        workers: &mut BlockWorkers,
        prepared: &mut PreparedGraph,
    ) {
        let FlowInput {
            tracks,
            samples,
            transport,
            sources,
        } = input;
        let frames = samples / 2;
        let ramp_frames = gain_ramp::ramp_frames(self.sample_rate);
        let send_start = transport.map_or(self.send_cursor, |t| t.sample_pos);
        self.send_cursor = send_start.saturating_add(frames as u64);
        prepared.track_taps.clear();
        prepared.taps.clear();
        self.prepared_controls(prepared, frames);
        prepared.master.clear();
        prepared.master.resize(samples, 0.0);

        plan.track_ramps.0.clear();
        plan.track_taps.clear();
        for track in 0..plan.tracks {
            plan.track_ramps.0.push(
                self.ramps
                    .tracks
                    .get_mut(track)
                    .map_or(std::ptr::null_mut(), |ramps| ramps as *mut _),
            );
            let name = &self.track_names[track];
            plan.track_taps.push(if prepared.taps.has(name) {
                prepared.taps.slot(name)
            } else {
                SignalSlot::NONE
            });
        }
        plan.send_ramps.0.clear();
        plan.send_delays.0.clear();
        for send in &plan.sends {
            let key = (send.track, send.index);
            let (ramp, delay) = match send.kind {
                SendKind::Aux => (None, self.aux_send_delays.get_mut(&key)),
                SendKind::PreFader | SendKind::PostFader => (
                    self.ramps
                        .tracks
                        .get_mut(send.track)
                        .and_then(|ramps| ramps.send_ramp(send.index))
                        .map(|ramp| ramp as *mut GainRamp),
                    self.send_delays.get_mut(&key),
                ),
            };
            plan.send_ramps.0.push(ramp.unwrap_or(std::ptr::null_mut()));
            plan.send_delays
                .0
                .push(delay.map_or(std::ptr::null_mut(), |delay| delay as *mut _));
        }
        plan.track_blocks.fill(TrackBlock::default());
        plan.rank_nodes();
        plan.reset_queue();

        let mut scratch = std::mem::take(&mut plan.scratch);
        let mut pre_fader = std::mem::take(&mut plan.pre_fader);
        let mut blocks = std::mem::take(&mut plan.track_blocks);
        {
            let flow = FlowBlock {
                sources,
                buffers: DisjointSlice::new(tracks),
                chains: DisjointSlice::new(&mut self.track_chains),
                widths: DisjointSlice::new(&mut self.track_widths),
                blocks: DisjointSlice::new(&mut blocks),
                pre_fader: DisjointSlice::new(&mut pre_fader),
                routing: &self.routing,
                key_plan: &self.key_plan,
                source_latencies: &self.source_latencies,
                performance_sends: &self.performance_sends,
                prepared_tracks: &prepared.tracks,
                track_taps: &prepared.track_taps,
                send_start,
                ramp_frames,
                samples,
            };
            let plan = &*plan;
            let scratch = DisjointSlice::new(&mut scratch);
            let jobs = BusJobs {
                plan,
                scratch: &scratch,
                flow: Some(&flow),
                taps: &prepared.taps,
                controls: &prepared.controls,
                sample_rate: self.sample_rate,
                transport,
                ramp_frames,
            };
            let first_bus = plan.first_bus();
            let run = |node: usize| {
                // SAFETY: the queue runs each node once per block, a source
                // before its track, and a track before every bus that hears
                // it or reads its key.
                unsafe {
                    if node < plan.sources {
                        sources.render(node);
                    } else if node < first_bus {
                        jobs.run_track(&flow, node - plan.sources);
                    } else {
                        jobs.run_bus(node - first_bus);
                    }
                }
            };
            let queue = &plan.queue;
            let result = workers.dispatch(workers.parallelism().min(plan.width), &|_| {
                queue.work(&plan.order, |node| plan.dependents_of(node), run);
            });
            // A panicking job unwinds the render thread as it would serially.
            if let Err(error) = result {
                panic!("graph block: {error}");
            }
        }
        plan.scratch = scratch;
        plan.pre_fader = pre_fader;
        plan.track_blocks = blocks;

        // Direct track returns reach the master in track order before any bus
        // return, as the serial routing loop sums them.
        let mut master = std::mem::take(&mut prepared.master);
        for (track, buffer) in tracks.iter_mut().enumerate() {
            let route = &self.routing[track];
            match plan.track_blocks[track].state {
                TrackState::Skipped => {}
                TrackState::Silent => {
                    if route.direct_to_master
                        && let Some(delay) = self.direct_delays.get_mut(track)
                    {
                        delay.advance_silent(buffer.len());
                    }
                }
                TrackState::Live => {
                    if route.direct_to_master {
                        if let Some(delay) = self.direct_delays.get_mut(track) {
                            delay.process(buffer);
                        }
                        let len = buffer.len().min(master.len());
                        simd::accumulate_channel_gains(
                            &mut master[..len],
                            &buffer[..len],
                            1.0,
                            1.0,
                        );
                    }
                }
            }
        }
        for send in &plan.orphan_sends {
            if plan.track_blocks[send.track].state != TrackState::Silent {
                continue;
            }
            let key = (send.track, send.index);
            match send.kind {
                SendKind::Aux => {
                    let pair = self.routing[send.track].aux_sends[send.index].pair;
                    // SAFETY: every track job has finished.
                    let aux = unsafe { sources.aux(send.track, pair) };
                    if let (Some(aux), Some(delay)) = (aux, self.aux_send_delays.get_mut(&key)) {
                        delay.advance_silent(aux.len().min(samples));
                    }
                }
                SendKind::PreFader | SendKind::PostFader => {
                    if let Some(delay) = self.send_delays.get_mut(&key) {
                        delay.advance_silent(tracks[send.track].len());
                    }
                }
            }
        }
        self.gather_bus_returns(plan, prepared, &mut master);
        prepared.master = master;
    }

    /// Run sources inside the graph block for a caller whose source `s`
    /// feeds track `tracks[s]`, or stop with `None`. Takes effect at the
    /// next preparation, or at once when the graph is prepared.
    pub fn set_block_sources(&mut self, tracks: Option<&[usize]>) {
        self.block_sources = tracks.map(<[usize]>::to_vec);
        if let Some(mut prepared) = self.prepared.take() {
            if let Some(parallel) = prepared.parallel.as_mut() {
                parallel.flow = self
                    .block_sources
                    .as_deref()
                    .filter(|_| parallel.is_free.iter().all(|free| *free))
                    .and_then(|sources| BusPlan::new(self, &prepared.buses, Some(sources)));
            }
            self.prepared = Some(prepared);
        }
    }
}

impl BusPlan {
    /// Drop every per-block address.
    fn unbind(&mut self) {
        self.nodes.0.clear();
        self.edge_delays.0.clear();
        self.ramps.0.clear();
        self.taps.clear();
        self.send_ramps.0.clear();
        self.send_delays.0.clear();
        self.track_ramps.0.clear();
        self.track_taps.clear();
    }
}

/// What every job of one block shares.
struct BusJobs<'a> {
    plan: &'a BusPlan,
    scratch: &'a DisjointSlice<'a, Vec<f32>>,
    flow: Option<&'a FlowBlock<'a>>,
    taps: &'a prepared::PreparedSignals,
    controls: &'a prepared::PreparedSignals,
    sample_rate: u32,
    transport: Option<&'a PluginTransport>,
    ramp_frames: u32,
}

impl BusJobs<'_> {
    /// Mix a track's sources, then run its chain and routing stage up to the
    /// fader. Sends and the direct return are left to their destinations.
    ///
    /// # Safety
    /// Runs once per block for `track`, after every source of the track and
    /// before any bus that hears it or reads its key.
    unsafe fn run_track(&self, flow: &FlowBlock<'_>, track: usize) {
        // SAFETY: guaranteed by the caller: no other job touches this track's
        // buffer, chain, width, block entry, pre-fader copy or ramps now.
        let (buffer, chain, width, block, pre_fader) = unsafe {
            (
                flow.buffers.get(track),
                flow.chains.get(track),
                flow.widths.get(track),
                flow.blocks.get(track),
                flow.pre_fader.get(track),
            )
        };
        // SAFETY: every source of the track has rendered.
        unsafe { flow.sources.mix_track(track, buffer) };
        let route = &flow.routing[track];
        if !chain.effects.is_empty() && (!route.mute || flow.key_plan.is_tapped(track)) {
            chain.process_with_lookup(
                buffer,
                flow.track_taps,
                self.controls,
                self.sample_rate,
                self.transport,
            );
        }
        if *width == SourceWidth::Mono && SourceWidth::of_buffer(buffer) == SourceWidth::Stereo {
            *width = SourceWidth::Stereo;
        }
        // A key tap is post-insert, pre-fader, pre-mute.
        let tap = self.plan.track_taps[track];
        if !tap.is_none() {
            // SAFETY: only this job publishes the track's key, and its readers
            // wait for this job.
            unsafe { tap.set(buffer) };
        }
        block.width = *width;
        block.latency = flow
            .source_latencies
            .get(track)
            .copied()
            .unwrap_or(0)
            .saturating_add(chain.latency_samples());
        block.state = TrackState::Skipped;
        if buffer.is_empty() {
            return;
        }
        // SAFETY: guaranteed by the caller; the send ramps inside are reached
        // only by the buses that hear this track, after this job.
        let mut ramps = unsafe { self.plan.track_ramps.0[track].as_mut() };
        let silent = match ramps.as_deref_mut() {
            Some(ramps) => ramps.gate_block(!route.mute, buffer, flow.ramp_frames),
            None => route.mute,
        };
        if silent {
            buffer.fill(0.0);
            block.state = TrackState::Silent;
            return;
        }
        if route.polarity_inverted {
            simd::negate(buffer);
        }
        if self.plan.pre_tracks[track] {
            pre_fader.clear();
            pre_fader.extend_from_slice(buffer);
        }
        let width = *width;
        match ramps {
            Some(ramps) => ramps.fader_block(
                fader_gains(route.volume, route.pan, width),
                width,
                buffer,
                flow.ramp_frames,
            ),
            None => apply_volume_pan(buffer, route.volume, route.pan, width),
        }
        block.state = TrackState::Live;
    }

    /// Gather a bus's track sends and bus sources, run its chain, publish its
    /// key and apply its return fader.
    ///
    /// # Safety
    /// Runs once per block for `target`, after every track and bus it hears
    /// or reads a key of.
    unsafe fn run_bus(&self, target: usize) {
        let plan = self.plan;
        let node: *mut BusNode = plan.nodes.0[target];
        // SAFETY: guaranteed by the caller: only this job touches the node,
        // its scratch entry, return ramp, tap and incoming delays and sends.
        let (bus, routed) = unsafe { (&mut *node, self.scratch.get(target)) };
        if let Some(flow) = self.flow {
            bus.buffer.clear();
            bus.buffer.resize(flow.samples, 0.0);
            // SAFETY: as above.
            unsafe { self.gather_sends(flow, target, &mut bus.buffer, routed) };
        }
        let range = plan.edge_starts[target]..plan.edge_starts[target + 1];
        for (edge, &delay) in plan.edges[range.clone()]
            .iter()
            .zip(&plan.edge_delays.0[range])
        {
            if !plan.queue.live[edge.source].load(Ordering::Acquire) {
                continue;
            }
            // SAFETY: a finished source is only read.
            let source = unsafe { &(*plan.nodes.0[edge.source]).buffer };
            if delay.is_null() {
                sum_buffer(&mut bus.buffer, source, 1.0);
            } else {
                routed.clear();
                routed.extend_from_slice(source);
                // SAFETY: only the target's job uses this edge's delay line.
                unsafe { (*delay).process(routed) };
                sum_buffer(&mut bus.buffer, routed, 1.0);
            }
        }
        bus.chain.process_with_lookup(
            &mut bus.buffer,
            self.taps,
            self.controls,
            self.sample_rate,
            self.transport,
        );
        let tap = plan.taps[target];
        if !tap.is_none() {
            // A key is the bus before its fader, as the serial block
            // publishes it.
            // SAFETY: only this job publishes the bus's key, and its readers
            // wait for this job.
            unsafe { tap.set(&bus.buffer) };
        }
        // SAFETY: only this job touches the bus's return ramp.
        let ramp = unsafe { plan.ramps.0[target].as_mut() };
        let live = AudioGraph::bus_gain_block(ramp, bus, self.ramp_frames);
        plan.queue.live[target].store(live, Ordering::Release);
    }

    /// Sum the track sends `target` hears into `buffer` in serial routing
    /// order. A silent track's send delay lines advance instead.
    ///
    /// # Safety
    /// Every track `target` hears has finished, and only this job touches the
    /// ramps and delay lines of the sends into `target`.
    unsafe fn gather_sends(
        &self,
        flow: &FlowBlock<'_>,
        target: usize,
        buffer: &mut [f32],
        scratch: &mut Vec<f32>,
    ) {
        let plan = self.plan;
        let range = plan.send_starts[target]..plan.send_starts[target + 1];
        for ((send, &ramp), &delay) in plan.sends[range.clone()]
            .iter()
            .zip(&plan.send_ramps.0[range.clone()])
            .zip(&plan.send_delays.0[range])
        {
            let track = send.track;
            // SAFETY: the track has finished; its entries are only read now.
            let (block, track_buf) =
                unsafe { (*flow.blocks.get_ref(track), flow.buffers.get_ref(track)) };
            // SAFETY: guaranteed by the caller.
            let delay = unsafe { delay.as_mut() };
            let route = &flow.routing[track];
            if send.kind == SendKind::Aux {
                let config = &route.aux_sends[send.index];
                // SAFETY: the track has finished mixing its sources.
                let Some(aux) = (unsafe { flow.sources.aux(track, config.pair) }) else {
                    continue;
                };
                match block.state {
                    TrackState::Skipped => {}
                    TrackState::Silent => {
                        if let Some(delay) = delay {
                            delay.advance_silent(aux.len().min(flow.samples));
                        }
                    }
                    TrackState::Live => {
                        let frames = aux.len().min(buffer.len());
                        let (gain_l, gain_r) = send_pan_gains(config.pan, SourceWidth::Stereo);
                        let gain_l = gain_l * config.amount as f32;
                        let gain_r = gain_r * config.amount as f32;
                        match delay {
                            Some(delay) => delay.mix_into_panned(
                                &aux[..frames],
                                &mut buffer[..frames],
                                gain_l,
                                gain_r,
                            ),
                            None => simd::accumulate_channel_gains(
                                &mut buffer[..frames],
                                &aux[..frames],
                                gain_l,
                                gain_r,
                            ),
                        }
                    }
                }
                continue;
            }
            match block.state {
                TrackState::Skipped => {}
                TrackState::Silent => {
                    if let Some(delay) = delay {
                        delay.advance_silent(track_buf.len());
                    }
                }
                TrackState::Live => {
                    let config = &route.sends[send.index];
                    let source = if send.kind == SendKind::PreFader {
                        // SAFETY: as for the track buffer.
                        unsafe { flow.pre_fader.get_ref(track) }
                    } else {
                        track_buf
                    };
                    let envelope = if flow.performance_sends.is_empty() {
                        None
                    } else {
                        flow.performance_sends
                            .get(&flow.prepared_tracks[track].send_keys[send.index])
                    };
                    // SAFETY: guaranteed by the caller.
                    let mut ramp = unsafe { ramp.as_mut() };
                    if let Some(ramp) = ramp.as_deref_mut() {
                        ramp.set_target(config.amount as f32, flow.ramp_frames);
                    }
                    route_send(
                        source,
                        config.amount as f32,
                        ramp,
                        send_pan_gains(config.pan, block.width),
                        flow.send_start,
                        block.latency,
                        envelope,
                        delay,
                        scratch,
                        buffer,
                    );
                }
            }
        }
    }
}

/// How a prepared graph's block divides among block workers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParallelShape {
    /// Track chains that run concurrently.
    pub free_tracks: usize,
    /// Track chains that read keys or controls and run in order.
    pub ordered_tracks: usize,
    /// Buses scheduled by dependency; zero when buses run serially.
    pub scheduled_buses: usize,
    /// Buses on the longest dependency chain.
    pub bus_depth: usize,
    /// Sources scheduled with the tracks and buses inside the block; zero
    /// when the caller renders them first.
    pub scheduled_sources: usize,
}

impl AudioGraph {
    /// The concurrency the prepared plan allows, for diagnostics and budgets.
    pub fn parallel_shape(&self) -> ParallelShape {
        let Some(plan) = self
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.parallel.as_ref())
        else {
            return ParallelShape::default();
        };
        ParallelShape {
            free_tracks: plan.free_tracks.len(),
            ordered_tracks: plan.is_free.iter().filter(|free| !**free).count(),
            scheduled_buses: plan.buses.as_ref().map_or(0, BusPlan::bus_count),
            bus_depth: plan.buses.as_ref().map_or(0, |buses| buses.depth),
            scheduled_sources: plan.flow.as_ref().map_or(0, |flow| flow.sources),
        }
    }
}

#[cfg(test)]
#[path = "parallel_tests.rs"]
mod tests;
