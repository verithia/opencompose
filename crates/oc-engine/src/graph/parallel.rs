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

use super::prepared::{PreparedBus, PreparedGraph, SignalSlot};
use super::*;
use crate::block_workers::{BlockWorkers, DisjointSlice};
use gain_ramp::GainRamp;
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
}

/// Bus dependencies and the per-block state that schedules them.
struct BusPlan {
    /// Bus-to-bus routes by target: `edges[edge_starts[t]..edge_starts[t + 1]]`
    /// lists target `t`'s sources in ascending order.
    edge_starts: Vec<usize>,
    edges: Vec<BusEdge>,
    /// Buses each bus must wait for: its route sources and key sources.
    dependencies: Vec<usize>,
    /// Buses waiting on each bus:
    /// `dependents[dependent_starts[b]..dependent_starts[b + 1]]`.
    dependent_starts: Vec<usize>,
    dependents: Vec<usize>,
    /// Longest chain of dependent buses, for diagnostics.
    depth: usize,
    /// Prepared bus index by name.
    bus_slots: HashMap<String, usize>,
    /// Route scratch for each target that has an incoming route.
    scratch: Vec<Vec<f32>>,
    /// Whether each bus reached its routes this block.
    live: Vec<bool>,
    /// Addresses valid for one block only, cleared before it returns.
    nodes: BusTable,
    edge_delays: DelayTable,
    ramps: RampTable,
    taps: Vec<SignalSlot>,
    /// Remaining path cost of each bus and the buses sorted by it, highest
    /// first; refreshed from `queue.cost` before each block.
    rank: Vec<u64>,
    order: Vec<usize>,
    queue: BusQueue,
}

/// Scheduling state shared by the workers during one block.
struct BusQueue {
    /// Dependencies still running, per bus.
    pending: Vec<AtomicUsize>,
    state: Vec<AtomicU8>,
    /// Buses not yet finished.
    remaining: AtomicUsize,
    /// Set when a chain panics, so the other workers stop waiting.
    aborted: AtomicBool,
    live: Vec<AtomicBool>,
    /// Running average of each bus's job time in nanoseconds.
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

/// Bus node addresses for one block. They are filled from `bus_chains` while
/// it is exclusively borrowed and cleared before the borrow ends.
struct BusTable(Vec<*mut BusNode>);

// SAFETY: the table only holds addresses while the graph is exclusively
// borrowed by one block. A node is written only by the job of its own bus and
// read by its dependents after that job handed them on with release ordering;
// `BusNode` itself is `Send`, which the offline bus workers already rely on.
unsafe impl Send for BusTable {}
// SAFETY: as above.
unsafe impl Sync for BusTable {}

/// Route delay addresses for one block, filled and cleared like `BusTable`.
struct DelayTable(Vec<*mut StereoDelayCompensator>);

// SAFETY: each edge's delay line is processed only by the job that gathers
// the edge's target, while the graph is exclusively borrowed by the block.
unsafe impl Send for DelayTable {}
// SAFETY: as above.
unsafe impl Sync for DelayTable {}

/// Return fader addresses for one block, filled and cleared like `BusTable`.
struct RampTable(Vec<*mut GainRamp>);

// SAFETY: each ramp is touched only by the job of its own bus while the graph
// is exclusively borrowed by the block.
unsafe impl Send for RampTable {}
// SAFETY: as above.
unsafe impl Sync for RampTable {}

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
        Self {
            free_tracks,
            is_free,
            buses: BusPlan::new(graph, buses),
        }
    }
}

impl BusPlan {
    fn new(graph: &AudioGraph, buses: &[PreparedBus]) -> Option<Self> {
        if buses.len() <= 1 {
            return None;
        }
        let slots: HashMap<&str, usize> = buses
            .iter()
            .enumerate()
            .map(|(index, bus)| (bus.name.as_str(), index))
            .collect();
        let mut incoming: Vec<Vec<BusEdge>> = (0..buses.len()).map(|_| Vec::new()).collect();
        let mut waits_on: Vec<Vec<usize>> = vec![Vec::new(); buses.len()];
        for (index, bus) in buses.iter().enumerate() {
            let node = graph.bus_chains.get(&bus.name)?;
            for key in node
                .chain
                .sidechain_sources()
                .chain(node.chain.control_sources())
            {
                if key == bus.name {
                    continue;
                }
                if let Some(&source) = slots.get(key) {
                    // The serial order reads a later bus's key from the
                    // previous block; only that order can reproduce it.
                    if source >= index {
                        return None;
                    }
                    waits_on[index].push(source);
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
                    waits_on[target].push(index);
                }
            }
        }
        let mut edge_starts = vec![0];
        let mut edges = Vec::new();
        for list in incoming {
            edges.extend(list);
            edge_starts.push(edges.len());
        }
        let mut dependencies = vec![0; buses.len()];
        let mut feeds: Vec<Vec<usize>> = vec![Vec::new(); buses.len()];
        let mut levels = vec![0usize; buses.len()];
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
                if edge_starts[target + 1] > edge_starts[target] {
                    Vec::with_capacity(capacity)
                } else {
                    Vec::new()
                }
            })
            .collect();
        let cost = buses
            .iter()
            .map(|bus| {
                let devices = graph
                    .bus_chains
                    .get(&bus.name)
                    .map_or(0, |node| node.chain.effects.len())
                    as u64;
                AtomicU64::new((devices + 1) * SEED_COST_PER_DEVICE_NS)
            })
            .collect();
        let count = buses.len();
        Some(Self {
            depth: levels.iter().copied().max().unwrap_or(0) + 1,
            edge_delays: DelayTable(Vec::with_capacity(edges.len())),
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
            live: vec![false; count],
            nodes: BusTable(Vec::with_capacity(count)),
            ramps: RampTable(Vec::with_capacity(count)),
            taps: Vec::with_capacity(count),
            rank: vec![0; count],
            order: (0..count).collect(),
            queue: BusQueue {
                pending: (0..count).map(|_| AtomicUsize::new(0)).collect(),
                state: (0..count).map(|_| AtomicU8::new(WAITING)).collect(),
                remaining: AtomicUsize::new(0),
                aborted: AtomicBool::new(false),
                live: (0..count).map(|_| AtomicBool::new(false)).collect(),
                cost,
            },
        })
    }

    fn dependents_of(&self, bus: usize) -> &[usize] {
        &self.dependents[self.dependent_starts[bus]..self.dependent_starts[bus + 1]]
    }

    /// Rank every bus by the costliest path from it to the master and order
    /// the buses by rank. Targets follow their sources in index order, so one
    /// backward pass sees every dependent's rank first.
    fn rank_buses(&mut self) {
        for bus in (0..self.rank.len()).rev() {
            let downstream = self
                .dependents_of(bus)
                .iter()
                .map(|&dependent| self.rank[dependent])
                .max()
                .unwrap_or(0);
            let own = self.queue.cost[bus].load(Ordering::Relaxed);
            self.rank[bus] = own.saturating_add(downstream);
        }
        let rank = &self.rank;
        // In-place and without allocation; ties keep a stable bus order.
        self.order
            .sort_unstable_by_key(|&bus| (std::cmp::Reverse(rank[bus]), bus));
    }

    fn reset_queue(&self) {
        let queue = &self.queue;
        queue
            .remaining
            .store(self.dependencies.len(), Ordering::Relaxed);
        queue.aborted.store(false, Ordering::Relaxed);
        for (bus, &count) in self.dependencies.iter().enumerate() {
            queue.pending[bus].store(count, Ordering::Relaxed);
            queue.live[bus].store(false, Ordering::Relaxed);
            let state = if count == 0 { READY } else { WAITING };
            queue.state[bus].store(state, Ordering::Relaxed);
        }
    }
}

impl BusQueue {
    /// Claim the ready bus with the highest rank.
    fn take(&self, order: &[usize]) -> Option<usize> {
        order.iter().copied().find(|&bus| {
            self.state[bus].load(Ordering::Relaxed) == READY
                && self.state[bus]
                    .compare_exchange(READY, TAKEN, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
        })
    }

    /// Fold one measured job time into the bus's running average.
    fn record_cost(&self, bus: usize, nanos: u64) {
        let previous = self.cost[bus].load(Ordering::Relaxed);
        let average = previous - previous / 8 + nanos / 8;
        self.cost[bus].store(average.max(1), Ordering::Relaxed);
    }

    /// Run ready buses until every bus has finished or one has panicked.
    ///
    /// Every worker of the block runs this loop. A bus that finishes marks
    /// each dependent whose last dependency it was as ready, and the same
    /// worker immediately looks for the best ready bus, so a chain of single
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
            let Some(bus) = self.take(order) else {
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
            run(bus);
            self.record_cost(bus, started.elapsed().as_nanos() as u64);
            for &dependent in dependents(bus) {
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
        let count = prepared.buses.len();
        plan.nodes.0.clear();
        plan.nodes.0.resize(count, std::ptr::null_mut());
        for (name, node) in self.bus_chains.iter_mut() {
            if let Some(&slot) = plan.bus_slots.get(name) {
                plan.nodes.0[slot] = node;
            }
        }
        if plan.nodes.0.iter().any(|node| node.is_null()) {
            plan.nodes.0.clear();
            prepared.parallel = Some(parallel);
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
        plan.rank_buses();
        plan.reset_queue();
        let sample_rate = self.sample_rate;
        // Moved out for the dispatch so each job can borrow its own entry while
        // the rest of the plan is shared.
        let mut scratch = std::mem::take(&mut plan.scratch);
        {
            let plan = &*plan;
            let scratch = DisjointSlice::new(&mut scratch);
            let taps = &prepared.taps;
            let controls = &prepared.controls;
            let queue = &plan.queue;
            let run = |target: usize| {
                // SAFETY: a bus is taken once per block, after every bus it
                // gathers from or reads a key of handed it on; nothing else
                // touches its node, scratch, ramp, tap or incoming delays
                // until it hands on its own dependents.
                unsafe {
                    run_bus(
                        plan,
                        target,
                        &scratch,
                        taps,
                        controls,
                        sample_rate,
                        transport,
                    );
                    let node: *mut BusNode = plan.nodes.0[target];
                    let node = &mut *node;
                    let tap = plan.taps[target];
                    if !tap.is_none() {
                        // A key is the bus before its fader, as the serial
                        // block publishes it.
                        tap.set(&node.buffer);
                    }
                    let live =
                        Self::bus_gain_block(plan.ramps.0[target].as_mut(), node, ramp_frames);
                    queue.live[target].store(live, Ordering::Release);
                }
            };
            let result = workers.dispatch(workers.parallelism(), &|_| {
                queue.work(&plan.order, |bus| plan.dependents_of(bus), run);
            });
            // A panicking chain unwinds the render thread as it would serially.
            if let Err(error) = result {
                panic!("bus chain: {error}");
            }
        }
        plan.scratch = scratch;
        for (live, flag) in plan.live.iter_mut().zip(&plan.queue.live) {
            *live = flag.load(Ordering::Acquire);
        }
        // The master gathers every return in bus order, as the serial block
        // sums them.
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
        plan.nodes.0.clear();
        plan.edge_delays.0.clear();
        plan.ramps.0.clear();
        plan.taps.clear();
        prepared.parallel = Some(parallel);
        true
    }
}

/// Gather a bus's sources in ascending order, then run its chain.
///
/// # Safety
/// The caller holds the only reference to `target`'s node, scratch entry and
/// incoming delay lines, and every source of `target` has finished.
unsafe fn run_bus(
    plan: &BusPlan,
    target: usize,
    scratch: &DisjointSlice<'_, Vec<f32>>,
    taps: &prepared::PreparedSignals,
    controls: &prepared::PreparedSignals,
    sample_rate: u32,
    transport: Option<&PluginTransport>,
) {
    let node: *mut BusNode = plan.nodes.0[target];
    // SAFETY: guaranteed by the caller.
    let bus = unsafe { &mut *node };
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
            // SAFETY: guaranteed by the caller.
            let routed = unsafe { scratch.get(target) };
            routed.clear();
            routed.extend_from_slice(source);
            // SAFETY: only the target's job uses this edge's delay line.
            unsafe { (*delay).process(routed) };
            sum_buffer(&mut bus.buffer, routed, 1.0);
        }
    }
    bus.chain
        .process_with_lookup(&mut bus.buffer, taps, controls, sample_rate, transport);
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
            scheduled_buses: plan
                .buses
                .as_ref()
                .map_or(0, |buses| buses.dependencies.len()),
            bus_depth: plan.buses.as_ref().map_or(0, |buses| buses.depth),
        }
    }
}

#[cfg(test)]
#[path = "parallel_tests.rs"]
mod tests;
