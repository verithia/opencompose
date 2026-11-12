//! Prepared scratch and preflight for bounded graph blocks.

use super::*;

/// A graph block was refused before any audio or DSP state was consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparationError {
    NotPrepared,
    TransportRange(oc_audio::TransportRangeError),
    ChainProcessing(EffectChainProcessError),
    CapacityExceeded,
    InvalidBuffer,
    TopologyChanged,
    LatencyChanged,
    SampleRateChanged,
    DevicePreparation(oc_audio::BlockPreparationError),
}

impl std::fmt::Display for PreparationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TransportRange(_) => "The graph block exceeds the transport sample domain",
            Self::ChainProcessing(_) => "Prepare the effect chain after its processing failure",
            Self::NotPrepared => "Prepare the audio graph before processing",
            Self::CapacityExceeded => "The graph block exceeds its prepared capacity",
            Self::InvalidBuffer => "The graph requires stereo buffers within the output block",
            Self::TopologyChanged => "The graph topology changed and requires preparation",
            Self::LatencyChanged => "The graph latency changed and requires preparation",
            Self::DevicePreparation(_) => "An audio device could not prepare its block capacity",
            Self::SampleRateChanged => {
                "The graph sample rate changed; rebuild its devices before preparation"
            }
        })
    }
}

impl std::error::Error for PreparationError {}

pub(super) trait SignalLookup {
    fn signal(&self, name: &str) -> Option<&[f32]>;
}

impl SignalLookup for HashMap<String, Vec<f32>> {
    fn signal(&self, name: &str) -> Option<&[f32]> {
        self.get(name).map(Vec::as_slice)
    }
}

fn sample_capacity(samples: usize) -> Result<Vec<f32>, PreparationError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(samples)
        .map_err(|_| PreparationError::CapacityExceeded)?;
    Ok(buffer)
}

struct SignalBuffer {
    active: bool,
    samples: Vec<f32>,
}

pub(super) struct PreparedSignals {
    values: HashMap<String, SignalBuffer>,
}

impl PreparedSignals {
    fn new(names: impl Iterator<Item = String>, samples: usize) -> Result<Self, PreparationError> {
        let mut values = HashMap::new();
        for name in names {
            if let std::collections::hash_map::Entry::Vacant(entry) = values.entry(name) {
                entry.insert(SignalBuffer {
                    active: false,
                    samples: sample_capacity(samples)?,
                });
            }
        }
        Ok(Self { values })
    }

    pub(super) fn clear(&mut self) {
        for value in self.values.values_mut() {
            value.active = false;
        }
    }

    pub(super) fn set(&mut self, name: &str, samples: &[f32]) {
        if let Some(value) = self.values.get_mut(name) {
            value.samples.clear();
            value.samples.extend_from_slice(samples);
            value.active = true;
        }
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
}

impl SignalLookup for PreparedSignals {
    fn signal(&self, name: &str) -> Option<&[f32]> {
        self.values
            .get(name)
            .filter(|value| value.active)
            .map(|value| value.samples.as_slice())
    }
}

pub(super) struct PreparedRoute {
    pub(super) target: String,
    pub(super) delay_key: (String, String),
}

pub(super) struct PreparedBus {
    pub(super) name: String,
    pub(super) routes: Vec<PreparedRoute>,
    latency: u32,
    inputs: Vec<(Option<String>, Option<String>)>,
}

pub(super) struct PreparedTrack {
    pub(super) send_keys: Vec<(usize, String)>,
    send_names: Vec<String>,
    direct_to_master: bool,
    aux_routes: Vec<(usize, String)>,
    latency: u32,
    inputs: Vec<(Option<String>, Option<String>)>,
}

pub(super) struct PreparedGraph {
    pub(super) max_samples: usize,
    sample_rate: u32,
    pub(super) master: Vec<f32>,
    pub(super) route_source: Vec<f32>,
    pub(super) routed: Vec<f32>,
    pub(super) send_buffer: Vec<f32>,
    pub(super) track_taps: PreparedSignals,
    pub(super) taps: PreparedSignals,
    pub(super) controls: PreparedSignals,
    pub(super) tracks: Vec<PreparedTrack>,
    pub(super) buses: Vec<PreparedBus>,
    master_inputs: Vec<(Option<String>, Option<String>)>,
    master_latency: u32,
    latency_invalidated: bool,
}

fn inputs(chain: &EffectChain) -> Vec<(Option<String>, Option<String>)> {
    chain
        .effects
        .iter()
        .map(|effect| {
            (
                effect.sidechain_source().map(str::to_owned),
                effect.control_source().map(str::to_owned),
            )
        })
        .collect()
}

fn same_inputs(chain: &EffectChain, prepared: &[(Option<String>, Option<String>)]) -> bool {
    chain.effects.len() == prepared.len()
        && chain
            .effects
            .iter()
            .zip(prepared)
            .all(|(effect, (key, control))| {
                effect.sidechain_source() == key.as_deref()
                    && effect.control_source() == control.as_deref()
            })
}

impl PreparedGraph {
    pub(super) fn new(graph: &AudioGraph, max_samples: usize) -> Result<Self, PreparationError> {
        Ok(Self {
            max_samples,
            sample_rate: graph.sample_rate,
            master: sample_capacity(max_samples)?,
            route_source: sample_capacity(if graph.bus_chains.is_empty() {
                0
            } else {
                max_samples
            })?,
            routed: sample_capacity(if graph.bus_chains.is_empty() {
                0
            } else {
                max_samples
            })?,
            send_buffer: sample_capacity(
                if graph.routing.iter().any(|route| !route.sends.is_empty()) {
                    max_samples
                } else {
                    0
                },
            )?,
            track_taps: PreparedSignals::new(
                graph
                    .key_plan
                    .tapped_tracks()
                    .filter_map(|index| graph.track_names.get(index).cloned()),
                max_samples,
            )?,
            taps: PreparedSignals::new(
                graph
                    .bus_chains
                    .values()
                    .flat_map(|bus| bus.chain.sidechain_sources())
                    .chain(graph.master_chain.sidechain_sources())
                    .map(str::to_owned),
                max_samples,
            )?,
            controls: PreparedSignals::new(
                graph
                    .control_runtimes
                    .iter()
                    .map(|runtime| runtime.name.clone()),
                max_samples / 2,
            )?,
            tracks: graph
                .routing
                .iter()
                .enumerate()
                .map(|(index, route)| PreparedTrack {
                    send_keys: route
                        .sends
                        .iter()
                        .map(|send| (index, send.bus.clone()))
                        .collect(),
                    send_names: route.sends.iter().map(|send| send.bus.clone()).collect(),
                    direct_to_master: route.direct_to_master,
                    aux_routes: route
                        .aux_sends
                        .iter()
                        .map(|send| (send.pair, send.bus.clone()))
                        .collect(),
                    latency: graph
                        .source_latencies
                        .get(index)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(
                            graph
                                .track_chains
                                .get(index)
                                .map_or(0, EffectChain::latency_samples),
                        ),
                    inputs: graph
                        .track_chains
                        .get(index)
                        .map(inputs)
                        .unwrap_or_default(),
                })
                .collect(),
            buses: graph
                .bus_order
                .iter()
                .filter_map(|name| {
                    graph.bus_chains.get(name).map(|bus| PreparedBus {
                        name: name.clone(),
                        routes: effective_bus_routes(bus)
                            .into_iter()
                            .map(|target| PreparedRoute {
                                delay_key: (name.clone(), target.clone()),
                                target,
                            })
                            .collect(),
                        latency: bus.chain.latency_samples(),
                        inputs: inputs(&bus.chain),
                    })
                })
                .collect(),
            master_inputs: inputs(&graph.master_chain),
            master_latency: graph.master_chain.latency_samples(),
            latency_invalidated: graph.latency_signature.is_empty(),
        })
    }

    pub(super) fn preflight(
        &self,
        graph: &AudioGraph,
        tracks: &[Vec<f32>],
        aux: &HashMap<(usize, usize), Vec<f32>>,
        samples: usize,
    ) -> Result<(), PreparationError> {
        if graph.sample_rate != self.sample_rate {
            return Err(PreparationError::SampleRateChanged);
        }
        if samples > self.max_samples {
            return Err(PreparationError::CapacityExceeded);
        }
        if !samples.is_multiple_of(2)
            || tracks.len() != self.tracks.len()
            || tracks
                .iter()
                .any(|track| track.len() > samples || track.len() % 2 != 0)
            || aux
                .values()
                .any(|track| track.len() > samples || track.len() % 2 != 0)
        {
            return Err(PreparationError::InvalidBuffer);
        }
        if !graph.key_plan.cycle.is_empty()
            || graph.routing.len() != self.tracks.len()
            || graph.bus_chains.len() != self.buses.len()
            || graph
                .bus_order
                .iter()
                .zip(&self.buses)
                .any(|(name, bus)| name != &bus.name)
            || !same_inputs(&graph.master_chain, &self.master_inputs)
        {
            return Err(PreparationError::TopologyChanged);
        }
        if graph.master_chain.latency_samples() != self.master_latency
            || graph.latency_signature.is_empty() != self.latency_invalidated
        {
            return Err(PreparationError::LatencyChanged);
        }
        for (index, track) in self.tracks.iter().enumerate() {
            let route = &graph.routing[index];
            if route.direct_to_master != track.direct_to_master
                || route.sends.len() != track.send_names.len()
                || route
                    .sends
                    .iter()
                    .zip(&track.send_names)
                    .any(|(send, name)| &send.bus != name)
                || route.aux_sends.len() != track.aux_routes.len()
                || route
                    .aux_sends
                    .iter()
                    .zip(&track.aux_routes)
                    .any(|(send, (pair, bus))| send.pair != *pair || &send.bus != bus)
                || graph
                    .track_chains
                    .get(index)
                    .is_none_or(|chain| !same_inputs(chain, &track.inputs))
            {
                return Err(PreparationError::TopologyChanged);
            }
            let latency = graph
                .source_latencies
                .get(index)
                .copied()
                .unwrap_or(0)
                .saturating_add(graph.track_chains[index].latency_samples());
            if latency != track.latency {
                return Err(PreparationError::LatencyChanged);
            }
        }
        for bus in &self.buses {
            let Some(node) = graph.bus_chains.get(&bus.name) else {
                return Err(PreparationError::TopologyChanged);
            };
            let routes_match = if node.routes.is_empty() {
                bus.routes.len() == 1 && bus.routes[0].target == "master"
            } else {
                node.routes.len() == bus.routes.len()
                    && node
                        .routes
                        .iter()
                        .zip(&bus.routes)
                        .all(|(name, route)| name == &route.target)
            };
            if node.buffer.capacity() < samples {
                return Err(PreparationError::CapacityExceeded);
            }
            if !routes_match || !same_inputs(&node.chain, &bus.inputs) {
                return Err(PreparationError::TopologyChanged);
            }
            if node.chain.latency_samples() != bus.latency {
                return Err(PreparationError::LatencyChanged);
            }
        }
        Ok(())
    }
}

/// Caller-owned resources for one bounded prepared graph block.
pub struct PreparedGraphBlock<'a> {
    pub tracks: &'a mut [Vec<f32>],
    pub transport: Option<&'a PluginTransport>,
    pub aux: &'a HashMap<(usize, usize), Vec<f32>>,
    pub output: &'a mut [f32],
}

impl AudioGraph {
    /// Allocate block scratch and compile routing lookups outside processing.
    /// Re-run after structural edits or an effect's reported latency changes.
    /// A sample-rate change requires rebuilding the graph's devices first.
    pub fn prepare(&mut self, max_frames: usize) -> Result<(), PreparationError> {
        let max_samples = max_frames
            .checked_mul(2)
            .filter(|samples| *samples <= isize::MAX as usize / std::mem::size_of::<f32>())
            .ok_or(PreparationError::CapacityExceeded)?;
        let feeds = self
            .control_runtimes
            .iter()
            .map(|runtime| (runtime.name.clone(), runtime.source_track))
            .collect();
        let keys = KeyPlan::build(
            &self.track_names,
            &self.track_chains,
            &self.bus_chains,
            &self.master_chain,
            &feeds,
        );
        self.key_plan = keys;
        self.configure_sidechain_alignment();
        self.refresh_latency_compensation();
        for chain in &mut self.track_chains {
            chain
                .prepare_block(max_frames)
                .map_err(PreparationError::DevicePreparation)?;
        }
        self.master_chain
            .prepare_block(max_frames)
            .map_err(PreparationError::DevicePreparation)?;
        for bus in self.bus_chains.values_mut() {
            bus.chain
                .prepare_block(max_frames)
                .map_err(PreparationError::DevicePreparation)?;
            if bus.buffer.capacity() < max_samples {
                bus.buffer
                    .try_reserve(max_samples - bus.buffer.len())
                    .map_err(|_| PreparationError::CapacityExceeded)?;
            }
        }
        for runtime in &mut self.control_runtimes {
            runtime.prepare(max_frames)?;
        }
        self.prepared = Some(PreparedGraph::new(self, max_samples)?);
        Ok(())
    }

    /// Process one bounded stereo block into caller-owned output. A preparation
    /// error leaves the input buffers, output and DSP state untouched.
    pub fn process_into(
        &mut self,
        tracks: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        aux: &HashMap<(usize, usize), Vec<f32>>,
        output: &mut [f32],
    ) -> Result<(), PreparationError> {
        self.process_prepared(PreparedGraphBlock {
            tracks,
            transport: transport.as_ref(),
            aux,
            output,
        })
    }

    /// Process with a caller-borrowed clock, including empty and rejected blocks.
    /// Effect compatibility adapters may clone it temporarily; the caller keeps
    /// its backing alive until all processing has returned.
    pub fn process_prepared(
        &mut self,
        block: PreparedGraphBlock<'_>,
    ) -> Result<(), PreparationError> {
        let PreparedGraphBlock {
            tracks,
            transport,
            aux,
            output,
        } = block;
        let prepared = self
            .prepared
            .as_ref()
            .ok_or(PreparationError::NotPrepared)?;
        prepared.preflight(self, tracks, aux, output.len())?;
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
        if output.is_empty() {
            return Ok(());
        }
        let mut prepared = self.prepared.take().unwrap();
        self.set_streaming_idle_gate(true);
        self.process_track_effects_prepared(tracks, transport, None, &mut prepared);
        self.mix_prepared(
            tracks,
            PreparedMixOptions {
                transport,
                stems: None,
                aux_buffers: aux,
                bus_workers: 1,
                samples: output.len(),
            },
            &mut prepared,
        );
        output.copy_from_slice(&prepared.master);
        self.prepared = Some(prepared);
        Ok(())
    }

    pub(super) fn prepared_controls(&self, prepared: &mut PreparedGraph, frames: usize) {
        prepared.controls.clear();
        for runtime in &self.control_runtimes {
            prepared.controls.set(&runtime.name, runtime.values(frames));
        }
    }
}

impl AudioGraph {
    pub(super) fn ensure_prepared(
        &mut self,
        tracks: &[Vec<f32>],
        aux: &HashMap<(usize, usize), Vec<f32>>,
        samples: usize,
    ) {
        let reusable = self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.preflight(self, tracks, aux, samples).is_ok());
        if !reusable {
            // Vec lengths cannot exceed isize::MAX, so the rounded stereo
            // frame count is representable on every supported platform.
            self.prepare(samples.div_ceil(2))
                .expect("representable graph block capacity");
        }
    }

    pub(super) fn route_prepared_bus(
        &mut self,
        plan: &PreparedBus,
        source: &[f32],
        routed: &mut Vec<f32>,
        master: &mut [f32],
        stems: &mut Option<&mut StemCapture>,
    ) {
        for route in &plan.routes {
            routed.clear();
            routed.extend_from_slice(source);
            if let Some(delay) = self.bus_route_delays.get_mut(&route.delay_key) {
                delay.process(routed);
            }
            if route.target == "master" {
                if let Some(capture) = stems
                    .as_deref_mut()
                    .filter(|capture| capture.capture_master_inputs)
                {
                    capture
                        .master_inputs
                        .push((format!("bus:{}", plan.name), routed.clone()));
                }
                sum_buffer(master, routed, 1.0);
            } else if let Some(target) = self.bus_chains.get_mut(&route.target) {
                sum_buffer(&mut target.buffer, routed, 1.0);
            }
        }
    }
}
