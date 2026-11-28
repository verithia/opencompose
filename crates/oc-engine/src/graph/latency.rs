//! Destination-referenced latency compensation.
//!
//! `delay(src -> dst) = latency(dst) - latency(src)`: a route waits for the
//! destination's accumulated latency instead of for the graph's slowest path.
//! The master merge is the exception: every path has to arrive on the same
//! sample there, so it keeps referencing the global maximum, which is the
//! smallest reference that aligns the mix without advancing a path.
//!
//! A per-track manual offset is a signed sample count added to the track's
//! reported latency. It may only redistribute the delay the plan already
//! applies: a route may not be pushed past its destination reference (that
//! would re-reference the graph and grow the render window) and the effective
//! latency may not go negative (audio cannot be advanced). Anything outside
//! that budget is clamped, with one warning per track.
//!
//! Signed the other way the difference is negative when the destination is
//! already more latent than the source; such a route cannot be made up at the
//! source, so it becomes a [`LatencyDiagnostic`] instead of a silent clamp.

use std::collections::HashMap;

use serde::Serialize;

use super::{AudioGraph, EffectChain, StereoDelayCompensator, effective_bus_routes};
use oc_dsl::ast::{LatencyCompensation, Project};

/// Seconds one track's insert chain adds before and after a bounce range.
///
/// The chain is the one a mix builds for the track: preset effects first, then
/// the track's own list. A bounce reads this before it renders, so the media it
/// publishes covers the delay the chain adds and the tail it rings past the
/// range end.
#[derive(Clone, Copy, Debug)]
pub struct ChainTailSeconds {
    /// Serial delay the chain adds, in seconds.
    pub latency: f64,
    /// Ring-out the chain declares for the full-mix length, in seconds.
    pub ring: f64,
    /// Ring-out only a bounce needs, in seconds; a reverb's decay.
    pub bounce_ring: f64,
}

/// Serial delay and ring-out of one track's insert chain, in seconds.
pub fn track_chain_tail_seconds(
    project: &Project,
    track: usize,
    sample_rate: u32,
) -> Result<ChainTailSeconds, String> {
    track_chain_tail_seconds_with_resources(project, track, sample_rate, None)
}

pub(crate) fn track_chain_tail_seconds_with_resources(
    project: &Project,
    track: usize,
    sample_rate: u32,
    resource_root: Option<&std::path::Path>,
) -> Result<ChainTailSeconds, String> {
    let linked = super::linked_automation::LinkedAutomation::for_track(
        project,
        sample_rate,
        track,
        resource_root,
    )?;
    let chain = super::build_track_chain(
        project,
        track,
        super::ChainContext {
            sample_rate,
            checked: false,
            touch: None,
            resource_root,
            hosted: None,
        },
        &linked,
    )?;
    let rate = f64::from(sample_rate.max(1));
    Ok(ChainTailSeconds {
        latency: f64::from(chain.latency_samples()) / rate,
        ring: f64::from(chain.tail_samples()) / rate,
        bounce_ring: f64::from(chain.bounce_tail_samples()) / rate,
    })
}

/// One route whose destination already carries more latency than its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatencyDiagnostic {
    /// Track or bus the route starts at.
    pub source: String,
    /// Bus the route feeds.
    pub destination: String,
    /// `latency(source) - latency(destination)`, negative.
    pub delta: i64,
}

impl std::fmt::Display for LatencyDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "latency compensation: '{}' -> '{}': the destination carries {} samples more \
             latency than the source (delta {}), so the route is referenced to the \
             destination; a make-up delay belongs in the source track",
            self.source, self.destination, -self.delta, self.delta
        )
    }
}

impl std::error::Error for LatencyDiagnostic {}

/// One destination a track's main output feeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DestinationLatency {
    /// `master` or a bus name.
    pub destination: String,
    /// Latency the destination already carries when the route lands: the
    /// master reference, or the latest source arrival at the bus input.
    pub reference_samples: u32,
    /// Delay the plan applies to this route after the manual offset.
    pub compensation_samples: u32,
    /// The route's surplus when the destination path is later, which is the
    /// value the readable diagnostic reports (`None` when the route is fine).
    pub diagnostic_delta: Option<i64>,
}

/// Three-part latency readout for one track: reported latency, manual offset
/// and the compensation applied at each destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrackLatencyReadout {
    pub track: String,
    /// Latency reported by the rendered source and its insert chain.
    pub reported_samples: u32,
    /// Offset the project asked for, in samples.
    pub requested_offset_samples: i32,
    /// Offset the plan applied after the range clamp, in samples.
    pub applied_offset_samples: i32,
    /// True when the requested offset did not fit the plan's delay budget.
    pub offset_clamped: bool,
    /// The latency the plan used: reported plus the applied offset.
    pub effective_samples: u32,
    /// True when this pass bypasses the track's compensation policy.
    pub bypassed: bool,
    /// Where this track's main output goes and what is applied there.
    pub destinations: Vec<DestinationLatency>,
}

/// Reference a route to its destination: how much later the destination already
/// is than the source's own output.
fn destination_referenced_delay(source_latency: u32, destination_latency: u32) -> u32 {
    destination_latency.saturating_sub(source_latency)
}

/// Advance through the serial effects using the arrival time at each input.
fn align_keyed_chain(
    chain: &mut EffectChain,
    mut main: u32,
    key_latency: impl Fn(&str) -> Option<u32>,
) -> u32 {
    for effect in &mut chain.effects {
        if let Some(source) = effect.sidechain_source()
            && let Some(key) = key_latency(source)
        {
            effect.configure_sidechain_alignment(main, key);
        }
        main = main.saturating_add(effect.latency_samples());
    }
    main
}

fn record_diagnostic(
    diagnostics: &mut Vec<LatencyDiagnostic>,
    source: &str,
    destination: &str,
    source_latency: u32,
    destination_latency: u32,
) {
    let delta = i64::from(source_latency) - i64::from(destination_latency);
    if delta < 0 {
        diagnostics.push(LatencyDiagnostic {
            source: source.to_string(),
            destination: destination.to_string(),
            delta,
        });
    }
}

impl AudioGraph {
    /// Patch one track's latency-compensation policy into a live graph.
    pub fn set_latency_compensation(&mut self, index: usize, policy: LatencyCompensation) {
        let Some(slot) = self.latency_compensation.get_mut(index) else {
            return;
        };
        if *slot == policy {
            return;
        }
        *slot = policy;
        self.invalidate_latency_plan();
    }

    /// Patch one track's manual latency offset into a live graph.
    pub fn set_latency_offset(&mut self, index: usize, offset: i32) {
        let Some(slot) = self.latency_offsets.get_mut(index) else {
            return;
        };
        if *slot == offset {
            return;
        }
        *slot = offset;
        self.invalidate_latency_plan();
    }

    /// Mark the current pass as a recording pass, which is what
    /// [`LatencyCompensation::WhenRecording`] reacts to.
    pub fn set_recording(&mut self, recording: bool) {
        if self.recording == recording {
            return;
        }
        self.recording = recording;
        self.invalidate_latency_plan();
    }

    /// True when this pass runs as a recording pass.
    pub fn recording(&self) -> bool {
        self.recording
    }

    /// Routes whose destination already carries more latency than the source.
    pub fn latency_diagnostics(&self) -> &[LatencyDiagnostic] {
        &self.latency_diagnostics
    }

    /// The three-part latency readout of every track, in declaration order.
    pub fn latency_readout(&self) -> &[TrackLatencyReadout] {
        &self.latency_readout
    }

    /// The readout of the track with this name.
    pub fn latency_readout_for(&self, track: &str) -> Option<&TrackLatencyReadout> {
        self.latency_readout
            .iter()
            .find(|readout| readout.track == track)
    }

    /// Validation view of the last planned PDC: a route the source cannot make
    /// up is a typed error. The render itself still aligns (the destination's
    /// reference absorbs it), so this refuses nothing on its own.
    pub fn validate_latency_compensation(&self) -> Result<(), Vec<LatencyDiagnostic>> {
        if self.latency_diagnostics.is_empty() {
            Ok(())
        } else {
            Err(self.latency_diagnostics.clone())
        }
    }

    /// True when this track's routes skip the compensation for this pass.
    fn latency_bypassed(&self, index: usize) -> bool {
        self.latency_compensation
            .get(index)
            .copied()
            .unwrap_or_default()
            .bypasses(self.recording)
    }

    /// Largest offset this track can take before one of its routes would have
    /// to advance its source.
    fn offset_budget(
        &self,
        index: usize,
        reported_latency: u32,
        bus_in: &HashMap<String, u32>,
        master_reference: u32,
    ) -> i64 {
        let Some(route) = self.routing.get(index) else {
            return 0;
        };
        let mut reference: Option<u32> = None;
        let mut consider = |candidate: u32| {
            reference = Some(reference.map_or(candidate, |current| current.min(candidate)));
        };
        if route.direct_to_master {
            consider(master_reference);
        }
        for send in &route.sends {
            consider(bus_in.get(&send.bus).copied().unwrap_or(0));
        }
        // Auxiliary plugin outputs bypass the track output, so they carry no
        // offset and do not constrain it.
        i64::from(reference.map_or(0, |value| value.saturating_sub(reported_latency)))
    }

    /// Clamp every requested offset into the plan's delay budget. Returns the
    /// applied offsets and the effective (reported + applied) latencies.
    fn apply_latency_offsets(
        &self,
        reported_latencies: &[u32],
        bus_in: &HashMap<String, u32>,
        master_reference: u32,
    ) -> (Vec<i32>, Vec<u32>) {
        let mut applied = vec![0_i32; reported_latencies.len()];
        let mut effective = reported_latencies.to_vec();
        for (index, &reported) in reported_latencies.iter().enumerate() {
            let headroom = self.offset_budget(index, reported, bus_in, master_reference);
            let requested = self.latency_offsets.get(index).copied().unwrap_or(0);
            let name = self.track_names.get(index).map_or("track", String::as_str);
            let (clamped, warning) = clamp_offset(name, requested, -i64::from(reported), headroom);
            if let Some(warning) = warning {
                crate::render_warnings::push(warning);
            }
            applied[index] = clamped;
            effective[index] = (i64::from(reported) + i64::from(clamped)) as u32;
        }
        (applied, effective)
    }

    /// Track keys read post-insert taps in dependency order.
    pub(super) fn configure_sidechain_alignment(&mut self) -> Vec<u32> {
        let mut taps = vec![None; self.track_chains.len()];
        let names = &self.track_names;
        let planned = self.key_plan.order.len() == self.track_chains.len();
        for position in 0..self.track_chains.len() {
            let index = if planned {
                self.key_plan.order[position]
            } else {
                position
            };
            let main = self.source_latencies.get(index).copied().unwrap_or(0);
            let output = align_keyed_chain(&mut self.track_chains[index], main, |key| {
                names
                    .iter()
                    .position(|name| name == key)
                    .and_then(|index| taps.get(index).copied().flatten())
            });
            taps[index] = Some(output);
        }
        taps.into_iter().map(Option::unwrap_or_default).collect()
    }

    pub(super) fn rebuild_latency_compensation(&mut self, preserve_route_history: bool) {
        self.rebuild_bus_plan();
        let reported_latencies = self.configure_sidechain_alignment();
        let track_names = &self.track_names;
        let track_key_latency = |key: &str| {
            track_names
                .iter()
                .position(|name| name == key)
                .and_then(|index| reported_latencies.get(index).copied())
        };
        let bypassed: Vec<bool> = (0..self.routing.len())
            .map(|index| self.latency_bypassed(index))
            .collect();

        // The master reference: the global maximum, used only where every path
        // has to meet at the same sample. This is what makes the final mix
        // sample-aligned.
        let mut master_reference = 0_u32;
        for (index, route) in self.routing.iter().enumerate() {
            if route.direct_to_master {
                master_reference =
                    master_reference.max(reported_latencies.get(index).copied().unwrap_or(0));
            }
        }

        // What each destination already carries when a route lands: the latest
        // arrival of its sources. Every route into it waits for that arrival.
        let mut bus_in: HashMap<String, u32> = HashMap::new();
        let mut bus_out: HashMap<String, u32> = HashMap::new();
        for name in &self.bus_order {
            let mut arrival = 0_u32;
            let mut has_input = false;
            for (index, route) in self.routing.iter().enumerate() {
                let source_latency = reported_latencies.get(index).copied().unwrap_or(0);
                for send in &route.sends {
                    if &send.bus == name {
                        has_input = true;
                        arrival = arrival.max(source_latency);
                    }
                }
                let aux_latency = self.source_latencies.get(index).copied().unwrap_or(0);
                for send in &route.aux_sends {
                    if &send.bus == name {
                        has_input = true;
                        arrival = arrival.max(aux_latency);
                    }
                }
            }
            for other in &self.bus_order {
                if other == name {
                    continue;
                }
                let Some(source) = self.bus_chains.get(other) else {
                    continue;
                };
                if effective_bus_routes(source)
                    .iter()
                    .any(|target| target == name)
                {
                    has_input = true;
                    arrival = arrival.max(bus_out.get(other).copied().unwrap_or(0));
                }
            }
            let output = self.bus_chains.get_mut(name).map_or(arrival, |bus| {
                align_keyed_chain(&mut bus.chain, arrival, |key| {
                    track_key_latency(key).or_else(|| bus_out.get(key).copied())
                })
            });
            // A bus nothing feeds carries nothing, so it must not stretch the
            // master reference.
            let output = if has_input { output } else { 0 };
            if self.bus_chains.get(name).is_some_and(|bus| {
                effective_bus_routes(bus)
                    .iter()
                    .any(|target| target == "master")
            }) {
                master_reference = master_reference.max(output);
            }
            bus_in.insert(name.clone(), arrival);
            bus_out.insert(name.clone(), output);
        }
        align_keyed_chain(&mut self.master_chain, master_reference, |key| {
            track_key_latency(key).or_else(|| bus_out.get(key).copied())
        });
        self.rebuild_bus_downstream_latencies();
        self.pre_master_latency_samples = master_reference;
        // Score events reach each processor with its aligned audio input.
        for (index, chain) in self.track_chains.iter_mut().enumerate() {
            chain.configure_midi_input_latency(
                self.source_latencies.get(index).copied().unwrap_or(0),
            );
        }
        for (name, bus) in &mut self.bus_chains {
            bus.chain
                .configure_midi_input_latency(bus_in.get(name).copied().unwrap_or(0));
        }
        self.master_chain
            .configure_midi_input_latency(master_reference);

        // Manual offsets only redistribute the delay above; the reference and
        // the render window stay exactly where the reported latencies put them.
        let (applied_offsets, effective_latencies) =
            self.apply_latency_offsets(&reported_latencies, &bus_in, master_reference);

        let mut diagnostics = Vec::new();
        if !preserve_route_history {
            self.direct_delays.clear();
            self.send_delays.clear();
            self.aux_send_delays.clear();
            self.bus_route_delays.clear();
        }
        self.direct_delays
            .resize_with(effective_latencies.len(), || StereoDelayCompensator::new(0));
        for (index, (line, latency)) in self
            .direct_delays
            .iter_mut()
            .zip(&effective_latencies)
            .enumerate()
        {
            let delay =
                applied_delay_of(master_reference.saturating_sub(*latency), bypassed[index]);
            line.set_frames(delay);
        }
        let mut readout = Vec::with_capacity(reported_latencies.len());
        for (index, &reported) in reported_latencies.iter().enumerate() {
            let bypass = bypassed.get(index).copied().unwrap_or(false);
            let mut destinations = Vec::new();
            if self
                .routing
                .get(index)
                .is_some_and(|route| route.direct_to_master)
            {
                destinations.push(DestinationLatency {
                    destination: "master".to_string(),
                    reference_samples: master_reference,
                    compensation_samples: applied_delay_of(
                        master_reference.saturating_sub(effective_latencies[index]),
                        bypass,
                    ),
                    diagnostic_delta: None,
                });
            }
            if let Some(route) = self.routing.get(index) {
                for send in &route.sends {
                    let reference = bus_in.get(&send.bus).copied().unwrap_or(0);
                    let delta = i64::from(effective_latencies[index])
                        - i64::from(self.bus_path_latency(&send.bus));
                    destinations.push(DestinationLatency {
                        destination: send.bus.clone(),
                        reference_samples: reference,
                        compensation_samples: applied_delay_of(
                            destination_referenced_delay(effective_latencies[index], reference),
                            bypass,
                        ),
                        diagnostic_delta: (!bypass && delta < 0).then_some(delta),
                    });
                }
            }
            readout.push(TrackLatencyReadout {
                track: self
                    .track_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("track {index}")),
                reported_samples: reported,
                requested_offset_samples: self.latency_offsets.get(index).copied().unwrap_or(0),
                applied_offset_samples: applied_offsets[index],
                offset_clamped: i64::from(applied_offsets[index])
                    != i64::from(self.latency_offsets.get(index).copied().unwrap_or(0)),
                effective_samples: effective_latencies[index],
                bypassed: bypass,
                destinations,
            });
        }
        self.latency_readout = readout;

        for (index, route) in self.routing.iter().enumerate() {
            let source_latency = effective_latencies.get(index).copied().unwrap_or(0);
            let bypass = bypassed.get(index).copied().unwrap_or(false);
            let source_name = self.track_names.get(index).map_or("track", String::as_str);
            for (send_index, send) in route.sends.iter().enumerate() {
                let destination = bus_in.get(&send.bus).copied().unwrap_or(0);
                let delay = if bypass {
                    0
                } else {
                    destination_referenced_delay(source_latency, destination)
                };
                self.send_delays
                    .entry((index, send_index))
                    .or_insert_with(|| StereoDelayCompensator::new(0))
                    .set_frames(delay);
                if !bypass {
                    record_diagnostic(
                        &mut diagnostics,
                        source_name,
                        &send.bus,
                        source_latency,
                        self.bus_path_latency(&send.bus),
                    );
                }
            }
        }
        for (index, route) in self.routing.iter().enumerate() {
            let source_latency = self.source_latencies.get(index).copied().unwrap_or(0);
            let bypass = bypassed.get(index).copied().unwrap_or(false);
            let source_name = self.track_names.get(index).map_or("track", String::as_str);
            for (send_index, send) in route.aux_sends.iter().enumerate() {
                let destination = bus_in.get(&send.bus).copied().unwrap_or(0);
                let delay = if bypass {
                    0
                } else {
                    destination_referenced_delay(source_latency, destination)
                };
                self.aux_send_delays
                    .entry((index, send_index))
                    .or_insert_with(|| StereoDelayCompensator::new(0))
                    .set_frames(delay);
                if !bypass {
                    record_diagnostic(
                        &mut diagnostics,
                        source_name,
                        &send.bus,
                        source_latency,
                        self.bus_path_latency(&send.bus),
                    );
                }
            }
        }

        let mut bus_delays: Vec<((String, String), u32)> = Vec::new();
        for (source_name, source) in &self.bus_chains {
            let source_out = bus_out.get(source_name).copied().unwrap_or(0);
            for target in effective_bus_routes(source) {
                let delay = if target == "master" {
                    // Only the master merge uses the global maximum.
                    master_reference.saturating_sub(source_out)
                } else {
                    destination_referenced_delay(
                        source_out,
                        bus_in.get(&target).copied().unwrap_or(0),
                    )
                };
                if target != "master" {
                    record_diagnostic(
                        &mut diagnostics,
                        source_name,
                        &target,
                        source_out,
                        self.bus_path_latency(&target),
                    );
                }
                bus_delays.push(((source_name.clone(), target), delay));
            }
        }
        for (route, delay) in bus_delays {
            self.bus_route_delays
                .entry(route)
                .or_insert_with(|| StereoDelayCompensator::new(0))
                .set_frames(delay);
        }
        self.latency_diagnostics = diagnostics;
        self.latency_signature = self.current_latency_signature();
        for diagnostic in &self.latency_diagnostics {
            crate::render_warnings::push(diagnostic.to_string());
        }
    }
}

/// A bypassed route applies no delay; otherwise the planned delay.
fn applied_delay_of(delay: u32, bypassed: bool) -> u32 {
    if bypassed { 0 } else { delay }
}

/// Clamp a requested offset into `[lowest, highest]`, with the warning text the
/// render reports when it does not fit.
pub(super) fn clamp_offset(
    name: &str,
    requested: i32,
    lowest: i64,
    highest: i64,
) -> (i32, Option<String>) {
    let clamped = i64::from(requested).clamp(lowest, highest);
    let warning = (clamped != i64::from(requested)).then(|| {
        format!(
            "latency offset on '{name}' clamped from {requested} to {clamped} samples: \
             the plan's delay budget is {lowest}..={highest} samples"
        )
    });
    (clamped as i32, warning)
}
