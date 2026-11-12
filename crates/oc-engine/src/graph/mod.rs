//! Audio graph — routes track audio through effects, buses, and master.
//!
//! ```text
//! Track 1 ──┬── [track effects] ──┬── send ──► Bus A ──[bus fx]──┐
//!            │                     │                              │
//! Track 2 ──┤── [track effects] ──┤── send ──► Bus B ──[bus fx]──┤──► Master ──[master fx]──► Output
//!            │                     │                              │
//! Track 3 ──┘── [track effects] ──┘── direct ────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use crate::dsp::{self, AudioEffect};
use crate::idle_gate;
use crate::spectrum::{SPECTRUM_BANDS, SpectrumAnalyser, SpectrumTarget};
use crate::touch;
use oc_audio::{AudioProcessContext, AudioProcessInput, PluginTransport};
use oc_dsl::ast::{Project, SoloMode};
use oc_dsp::simd;
mod control_source;
mod effect_automation;
mod gain_automation;
mod gain_ramp;
mod latency;
mod prepared;
mod process_boundary;
pub use process_boundary::EffectChainProcessError;
use process_boundary::ProcessBoundary;
#[cfg(test)]
mod prepared_tests;
pub use prepared::{PreparationError, PreparedGraphBlock};
use prepared::{PreparedGraph, SignalLookup};
#[cfg(test)]
mod streaming_gate_tests;

pub use latency::{
    ChainTailSeconds, DestinationLatency, LatencyDiagnostic, TrackLatencyReadout,
    track_chain_tail_seconds,
};
pub use oc_dsl::ast::LatencyCompensation;

use gain_ramp::{GainRamp, MixerRamps};

#[cfg(feature = "plugin-host")]
use oc_host::clap_main::{offline_main_is_active, service_offline_main};
#[cfg(not(feature = "plugin-host"))]
fn offline_main_is_active() -> bool {
    false
}
#[cfg(not(feature = "plugin-host"))]
fn service_offline_main() -> Result<(), String> {
    Ok(())
}

/// Optional offline taps from ONE authoritative graph pass. Track and bus taps
/// overlap in routed graphs; only master_inputs sum to pre_master. Master DSP
/// may be nonlinear, so none of these are independently mastered stems.
#[derive(Clone)]
pub struct StemCapture {
    pub tracks: Vec<(String, Vec<f32>)>,
    pub buses: Vec<(String, Vec<f32>)>,
    pub master_inputs: Vec<(String, Vec<f32>)>,
    pub pre_master: Vec<f32>,
    /// Full routing diagnostics are optional for track/bus-only ZIP exports.
    pub capture_master_inputs: bool,
    /// Capture each track's post-insert, pre-fader chain output instead of the
    /// post-fader tap; offline device analysis reads what an insert sees.
    pub pre_fader_tracks: bool,
}

impl Default for StemCapture {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            buses: Vec::new(),
            master_inputs: Vec::new(),
            pre_master: Vec::new(),
            capture_master_inputs: true,
            pre_fader_tracks: false,
        }
    }
}

/// Channel count of the monitored master bus. The graph mixes interleaved
/// stereo, so a live master is always a stereo signal; the UI refuses a mono
/// check when it sees anything else.
pub const MASTER_CHANNELS: u16 = 2;

/// A fully instantiated audio graph ready for rendering.
pub struct AudioGraph {
    pub track_chains: Vec<EffectChain>,
    pub bus_chains: HashMap<String, BusNode>,
    pub master_chain: EffectChain,
    /// Linear post-effects master fader, applied before export normalization.
    pub master_gain: f32,
    /// Ramp state for the fader, pan, send, return and mute stages.
    ramps: MixerRamps,
    pub routing: Vec<TrackRouting>,
    pub sample_rate: u32,
    /// Measured source width per track; drives the fader/pan law.
    track_widths: Vec<SourceWidth>,
    source_latencies: Vec<u32>,
    /// Per-track latency-compensation policy, patched like `source_latencies`.
    latency_compensation: Vec<LatencyCompensation>,
    /// Per-track manual latency offset in samples, parallel to `routing`.
    latency_offsets: Vec<i32>,
    /// Three-part latency readout built by the last plan, one per track.
    latency_readout: Vec<TrackLatencyReadout>,
    /// True while this pass records, which `WhenRecording` reacts to.
    recording: bool,
    latency_diagnostics: Vec<LatencyDiagnostic>,
    pre_master_latency_samples: u32,
    direct_delays: Vec<StereoDelayCompensator>,
    send_delays: HashMap<(usize, usize), StereoDelayCompensator>,
    aux_send_delays: HashMap<(usize, usize), StereoDelayCompensator>,
    performance_sends: HashMap<(usize, String), crate::performance_send::SendEnvelope>,
    send_cursor: u64,
    bus_route_delays: HashMap<(String, String), StereoDelayCompensator>,
    bus_order: Vec<String>,
    bus_downstream_latencies: HashMap<String, u32>,
    latency_signature: Vec<(String, u32)>,
    track_names: Vec<String>,
    /// Live analyser tap; allocated only while a panel is listening.
    spectrum: Option<SpectrumAnalyser>,
    spectrum_target: Option<SpectrumTarget>,
    /// Audio-derived control signals, in project declaration order.
    control_runtimes: Vec<control_source::ControlRuntime>,
    /// Per-track gain ride, parallel to `routing`.
    control_gains: Vec<Option<control_source::GainRide>>,
    /// Tracks that feed a control source but must not reach the mix themselves.
    control_only: Vec<bool>,
    /// A control source was refreshed by the track-FX pass for this block.
    control_fresh: Vec<bool>,
    /// Track key routing derived once at build time.
    key_plan: KeyPlan,
    prepared: Option<PreparedGraph>,
}

/// Which tracks a key or control edge reads, and in what order their chains
/// must run. A control source is evaluated after its feed track's chain and
/// before its consumer's chain, so a consumer always sees this block's value.
#[derive(Default)]
struct KeyPlan {
    /// Track indices with every key/control source before its consumer.
    order: Vec<usize>,
    /// Track index is tapped as a key or control feed by at least one chain.
    taps: Vec<bool>,
    /// Tracks left unresolved by a dependency cycle.
    cycle: Vec<usize>,
}

impl KeyPlan {
    fn build(
        track_names: &[String],
        track_chains: &[EffectChain],
        bus_chains: &HashMap<String, BusNode>,
        master_chain: &EffectChain,
        control_feeds: &HashMap<String, usize>,
    ) -> Self {
        let index_of = |name: &str| track_names.iter().position(|track| track == name);
        let mut taps = vec![false; track_names.len()];
        let mut consumers = vec![Vec::new(); track_names.len()];
        for (consumer, chain) in track_chains.iter().enumerate() {
            for source in chain.sidechain_sources() {
                let Some(source_index) = index_of(source) else {
                    continue;
                };
                taps[source_index] = true;
                if source_index != consumer {
                    consumers[source_index].push(consumer);
                }
            }
            for source in chain.control_sources() {
                let Some(&feed) = control_feeds.get(source) else {
                    continue;
                };
                taps[feed] = true;
                if feed != consumer {
                    consumers[feed].push(consumer);
                }
            }
        }
        for chain in bus_chains
            .values()
            .map(|bus| &bus.chain)
            .chain(std::iter::once(master_chain))
        {
            for source in chain.sidechain_sources() {
                if let Some(source_index) = index_of(source) {
                    taps[source_index] = true;
                }
            }
        }
        // Kahn with the lowest index first, so an acyclic graph keeps its
        // declaration order. A cycle leaves the rest in index order; checked
        // builds reject it before a render.
        let mut indegree = vec![0_usize; track_names.len()];
        for targets in &consumers {
            for &consumer in targets {
                indegree[consumer] += 1;
            }
        }
        let mut ready = std::collections::BinaryHeap::new();
        for (index, degree) in indegree.iter().enumerate() {
            if *degree == 0 {
                ready.push(std::cmp::Reverse(index));
            }
        }
        let mut order = Vec::with_capacity(track_names.len());
        while let Some(std::cmp::Reverse(index)) = ready.pop() {
            order.push(index);
            for &consumer in &consumers[index] {
                indegree[consumer] -= 1;
                if indegree[consumer] == 0 {
                    ready.push(std::cmp::Reverse(consumer));
                }
            }
        }
        let mut cycle = Vec::new();
        for index in 0..track_names.len() {
            if !order.contains(&index) {
                cycle.push(index);
                order.push(index);
            }
        }
        Self { order, taps, cycle }
    }

    fn is_tapped(&self, index: usize) -> bool {
        self.taps.get(index).copied().unwrap_or(false)
    }

    /// Track indices whose post-insert buffer a key reads.
    fn tapped_tracks(&self) -> impl Iterator<Item = usize> + '_ {
        self.taps
            .iter()
            .enumerate()
            .filter_map(|(index, tapped)| tapped.then_some(index))
    }
}

/// Streaming stereo delay used to align lower-latency parallel graph paths.
struct StereoDelayCompensator {
    buffer: Vec<f32>,
    position: usize,
}

impl StereoDelayCompensator {
    fn new(delay_frames: u32) -> Self {
        Self {
            buffer: vec![0.0; delay_frames as usize * 2],
            position: 0,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        if self.buffer.is_empty() {
            return;
        }
        for sample in samples {
            std::mem::swap(sample, &mut self.buffer[self.position]);
            self.position += 1;
            if self.position == self.buffer.len() {
                self.position = 0;
            }
        }
    }

    /// Push silence through the line without producing output, so a gated
    /// route keeps its place in the compensation timeline.
    fn advance_silent(&mut self, samples: usize) {
        if self.buffer.is_empty() {
            return;
        }
        for _ in 0..samples {
            self.buffer[self.position] = 0.0;
            self.position += 1;
            if self.position == self.buffer.len() {
                self.position = 0;
            }
        }
    }

    /// Sum a delayed stereo copy with one gain per channel.
    fn mix_into_panned(&mut self, source: &[f32], target: &mut [f32], gain_l: f32, gain_r: f32) {
        if self.buffer.is_empty() {
            simd::accumulate_channel_gains(target, source, gain_l, gain_r);
            return;
        }
        for (index, (target, source)) in target.iter_mut().zip(source).enumerate() {
            let delayed = self.buffer[self.position];
            self.buffer[self.position] = *source;
            self.position += 1;
            if self.position == self.buffer.len() {
                self.position = 0;
            }
            *target += delayed * if index % 2 == 0 { gain_l } else { gain_r };
        }
    }

    /// Sum a delayed stereo copy at a level that moves across the block.
    fn mix_into_panned_ramped(
        &mut self,
        source: &[f32],
        target: &mut [f32],
        pan: (f32, f32),
        ramp: &mut GainRamp,
    ) {
        if self.buffer.is_empty() {
            for (index, (target, source)) in target.iter_mut().zip(source).enumerate() {
                let gain = if index % 2 == 0 { pan.0 } else { pan.1 } * ramp.next();
                *target += source * gain;
            }
            return;
        }
        for (index, (target, source)) in target.iter_mut().zip(source).enumerate() {
            let delayed = self.buffer[self.position];
            self.buffer[self.position] = *source;
            self.position += 1;
            if self.position == self.buffer.len() {
                self.position = 0;
            }
            let gain = if index % 2 == 0 { pan.0 } else { pan.1 } * ramp.next();
            *target += delayed * gain;
        }
    }
}

/// Longest effect block while a tempo/meter map is active. Keeping the same
/// quantum as the source lanes makes offline and streaming plugin blocks line
/// up; without a map the chain keeps its one-call-per-buffer path.
const CLOCK_ALIGNED_BLOCK_FRAMES: usize = 128;

fn process_effect_context(
    effect: &mut dyn AudioEffect,
    buffer: &mut [f32],
    sample_rate: u32,
    transport: Option<&PluginTransport>,
    input: AudioProcessInput<'_>,
) {
    effect.process_with_context(
        buffer,
        AudioProcessContext {
            sample_rate,
            transport,
            input,
        },
    );
}

/// Run one effect over spans that each carry a single clock context.
///
/// A block is split at the next authored tempo/meter change and at the block
/// quantum, so every `process` call sees one tempo, PPQ and time signature.
/// Sliding windows slice the caller's buffer, so the split allocates nothing
/// and cannot drop or duplicate a frame.
fn process_effect_aligned(
    effect: &mut dyn AudioEffect,
    buffer: &mut [f32],
    sidechain: Option<&[f32]>,
    sample_rate: u32,
    transport: Option<&PluginTransport>,
) {
    let Some(base) = transport else {
        match sidechain {
            Some(key) => process_effect_context(
                effect,
                buffer,
                sample_rate,
                None,
                AudioProcessInput::Sidechain(key),
            ),
            None => {
                process_effect_context(effect, buffer, sample_rate, None, AudioProcessInput::Main)
            }
        }
        return;
    };
    if !base.has_clock() {
        match sidechain {
            Some(key) => process_effect_context(
                effect,
                buffer,
                sample_rate,
                Some(base),
                AudioProcessInput::Sidechain(key),
            ),
            None => process_effect_context(
                effect,
                buffer,
                sample_rate,
                Some(base),
                AudioProcessInput::Main,
            ),
        }
        return;
    }
    let total = buffer.len() / 2;
    let mut offset = 0;
    while offset < total {
        let segment = base.at_sample(base.sample_pos + offset as u64);
        let limit = (total - offset).min(CLOCK_ALIGNED_BLOCK_FRAMES);
        let frames = segment.block_frames(limit).clamp(1, limit);
        let range = offset * 2..(offset + frames) * 2;
        let key = sidechain.and_then(|side| side.get(range.clone()));
        match key {
            Some(key) => process_effect_context(
                effect,
                &mut buffer[range],
                sample_rate,
                Some(&segment),
                AudioProcessInput::Sidechain(key),
            ),
            None => process_effect_context(
                effect,
                &mut buffer[range],
                sample_rate,
                Some(&segment),
                AudioProcessInput::Main,
            ),
        }
        offset += frames;
    }
}

/// Run one effect over spans that each carry a single clock context, with the
/// control input sliced to the same spans. A short control ends in silence.
fn process_effect_aligned_control(
    effect: &mut dyn AudioEffect,
    buffer: &mut [f32],
    control: &[f32],
    sample_rate: u32,
    transport: Option<&PluginTransport>,
) {
    let Some(base) = transport else {
        process_effect_context(
            effect,
            buffer,
            sample_rate,
            None,
            AudioProcessInput::Control(control),
        );
        return;
    };
    if !base.has_clock() {
        process_effect_context(
            effect,
            buffer,
            sample_rate,
            Some(base),
            AudioProcessInput::Control(control),
        );
        return;
    }
    let total = buffer.len() / 2;
    let mut offset = 0;
    while offset < total {
        let segment = base.at_sample(base.sample_pos + offset as u64);
        let limit = (total - offset).min(CLOCK_ALIGNED_BLOCK_FRAMES);
        let frames = segment.block_frames(limit).clamp(1, limit);
        let range = offset * 2..(offset + frames) * 2;
        let span = control.get(offset..offset + frames).unwrap_or(&[]);
        process_effect_context(
            effect,
            &mut buffer[range],
            sample_rate,
            Some(&segment),
            AudioProcessInput::Control(span),
        );
        offset += frames;
    }
}

/// Effect chain: a series of effects applied in order.
pub struct EffectChain {
    pub effects: Vec<Box<dyn AudioEffect>>,
    /// Parameter changes being faded into the live devices. Empty on every
    /// offline build, so a rendered file never sees a fade.
    ramps: Vec<ParamRamp>,
    boundary: ProcessBoundary,
}

/// Frames one parameter ramp step covers.
///
/// A full-swing 20 ms fade moves at most `1 / 240` per step, under the adjacent
/// sample delta this project treats as a click (0.005), and finer than the
/// 8-sample grid a touch release already writes on. Once a fade is through, the
/// rest of the block is processed in one call, so the split costs one short
/// stretch per edit.
const PARAM_RAMP_STEP: u32 = 4;

/// A live parameter change being faded into one device.
struct ParamRamp {
    /// Position of the device in its chain.
    effect: usize,
    name: String,
    from: f64,
    to: f64,
    /// Samples covered so far, and the whole fade length in samples.
    elapsed: u32,
    total: u32,
}

impl ParamRamp {
    /// The value this fade holds `offset` samples from here.
    ///
    /// The walk is linear in samples and lands exactly on `to` at `total`, so
    /// the step that covers the last samples writes the goal itself.
    fn value_at(&self, offset: u32) -> f64 {
        let elapsed = self.elapsed.saturating_add(offset);
        if elapsed >= self.total {
            return self.to;
        }
        let progress = f64::from(elapsed) / f64::from(self.total);
        self.from + (self.to - self.from) * progress
    }

    fn done(&self) -> bool {
        self.elapsed >= self.total
    }
}

struct EffectInputs<'a> {
    key: Option<&'a [f32]>,
    control: Option<&'a [f32]>,
}

struct PreparedMixOptions<'a> {
    transport: Option<&'a PluginTransport>,
    stems: Option<&'a mut StemCapture>,
    aux_buffers: &'a HashMap<(usize, usize), Vec<f32>>,
    bus_workers: usize,
    samples: usize,
}

struct EffectChainBuild<'a> {
    sample_rate: u32,
    checked: bool,
    properties: &'a oc_dsl::ast::ProjectProperties,
    owner: &'a str,
    place: &'a str,
    touch: Option<&'a Arc<touch::TouchRegistry>>,
}

impl Default for EffectChain {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectChain {
    pub fn try_from_effects(
        effects: &[oc_dsl::ast::Effect],
        sample_rate: u32,
    ) -> Result<Self, String> {
        build_effect_chain(
            effects,
            &[],
            &[],
            EffectChainBuild {
                sample_rate,
                checked: true,
                properties: &oc_dsl::ast::ProjectProperties::default(),
                owner: "",
                place: "",
                touch: None,
            },
        )
    }
    pub fn new() -> Self {
        Self {
            effects: Vec::new(),
            ramps: Vec::new(),
            boundary: ProcessBoundary::default(),
        }
    }

    fn validate_fade_params(&self, changes: &[(usize, String, f64, f64)]) -> Result<(), String> {
        const MAX_PARAM_RAMPS: usize = 1024;
        let mut keys: std::collections::HashSet<_> = self
            .ramps
            .iter()
            .filter(|ramp| !ramp.done())
            .map(|ramp| (ramp.effect, ramp.name.as_str()))
            .collect();
        keys.extend(
            changes
                .iter()
                .map(|(effect, name, _, _)| (*effect, name.as_str())),
        );
        if keys.len() > MAX_PARAM_RAMPS {
            return Err(
                "Too many simultaneous effect parameter ramps; finish or reduce the pending edits"
                    .into(),
            );
        }
        Ok(())
    }

    /// Fade these parameter changes into the live devices, keeping their DSP
    /// state (delay lines, reverb tails, plugin state).
    ///
    /// Each entry is `(chain position, parameter, value before, value now)` in
    /// the device's own unit. A change to a parameter already fading continues
    /// from the value that fade reached, so a stream of edits cannot queue: the
    /// latest value wins. The fade length is the fixed crossfade a released
    /// touch already uses, so a hand-back and a hand edit sound the same.
    pub fn fade_params(
        &mut self,
        changes: &[(usize, String, f64, f64)],
        sample_rate: u32,
    ) -> Result<(), String> {
        self.validate_fade_params(changes)?;
        self.ramps.retain(|ramp| !ramp.done());
        let total = crate::touch::release_fade_samples(sample_rate);
        for (effect, name, before, goal) in changes {
            let from = self
                .ramps
                .iter()
                .find(|ramp| ramp.effect == *effect && ramp.name == *name)
                .map(|ramp| ramp.value_at(0))
                .or_else(|| {
                    self.effects
                        .get(*effect)
                        .and_then(|device| device.get_param(name))
                })
                .unwrap_or(*before);
            let ramp = ParamRamp {
                effect: *effect,
                name: name.clone(),
                from,
                to: *goal,
                elapsed: 0,
                total,
            };
            match self
                .ramps
                .iter_mut()
                .find(|running| running.effect == *effect && running.name == *name)
            {
                Some(running) => *running = ramp,
                None => self.ramps.push(ramp),
            }
        }
        Ok(())
    }

    /// Set these parameter changes on the live devices at once, with no fade.
    ///
    /// A stopped transport has nothing to click, and the next press must render
    /// the new value from its first sample, so it takes this path instead of
    /// [`Self::fade_params`].
    pub fn apply_params(&mut self, changes: &[(usize, String, f64, f64)]) {
        for (effect, name, _, goal) in changes {
            self.ramps
                .retain(|ramp| !(ramp.effect == *effect && ramp.name == *name));
            if let Some(device) = self.effects.get_mut(*effect) {
                device.set_param(name, *goal);
            }
        }
    }

    /// Check processor failure authorities without entering native plugins.
    pub fn processing_failed(&self) -> bool {
        self.boundary.failure().is_some()
            || self.effects.iter().any(|effect| effect.processing_failed())
    }

    /// Consume a detailed first failure on the control plane.
    pub fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        let mut report = self.boundary.report();
        if matches!(report, oc_audio::ProcessingFailureReport::Ready(_)) {
            return report;
        }
        for effect in &self.effects {
            if effect.processing_failed() {
                report = report.merge(effect.take_processing_failure());
            }
            if matches!(report, oc_audio::ProcessingFailureReport::Ready(_)) {
                break;
            }
        }
        report
    }

    /// Reserve each device's block scratch before the chain reaches playback.
    pub fn prepare_block(
        &mut self,
        max_frames: usize,
    ) -> Result<(), oc_audio::BlockPreparationError> {
        for effect in &mut self.effects {
            effect.prepare_block(max_frames)?;
        }
        self.boundary.clear();
        Ok(())
    }

    pub fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        if self.boundary.admit(buffer.len(), None).is_err() {
            return;
        }
        for fx in &mut self.effects {
            fx.process(buffer, sample_rate);
        }
    }

    /// Forward a borrowed processing context through every device in order.
    /// A refusal is retained by the chain's first-failure/report authority;
    /// callers that can handle it immediately use the checked variant.
    pub fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        let _ = self.try_process_with_context(buffer, context);
    }

    /// Refuse invalid sample ranges before entering any device.
    pub fn try_process_with_context(
        &mut self,
        buffer: &mut [f32],
        context: AudioProcessContext<'_>,
    ) -> Result<(), EffectChainProcessError> {
        self.boundary.admit(buffer.len(), context.transport)?;
        for effect in &mut self.effects {
            match context.input {
                AudioProcessInput::Main => process_effect_aligned(
                    effect.as_mut(),
                    buffer,
                    None,
                    context.sample_rate,
                    context.transport,
                ),
                AudioProcessInput::Sidechain(key) => process_effect_aligned(
                    effect.as_mut(),
                    buffer,
                    Some(key),
                    context.sample_rate,
                    context.transport,
                ),
                AudioProcessInput::Control(control) => process_effect_aligned_control(
                    effect.as_mut(),
                    buffer,
                    control,
                    context.sample_rate,
                    context.transport,
                ),
            }
        }
        Ok(())
    }

    pub fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_context(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Main,
            },
        );
    }

    pub fn has_sidechain(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| effect.sidechain_source().is_some())
    }

    fn sidechain_sources(&self) -> impl Iterator<Item = &str> {
        self.effects
            .iter()
            .filter_map(|effect| effect.sidechain_source())
    }

    /// Control sources this chain's devices read.
    fn control_sources(&self) -> impl Iterator<Item = &str> {
        self.effects
            .iter()
            .filter_map(|effect| effect.control_source())
    }

    pub fn has_control_inputs(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| effect.control_source().is_some())
    }

    pub fn process_with_sidechains(
        &mut self,
        buffer: &mut [f32],
        sidechains: &HashMap<String, Vec<f32>>,
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_inputs(buffer, sidechains, &HashMap::new(), sample_rate, transport);
    }

    /// Process the chain with an audio key map and a control map.
    ///
    /// A device with a control input reads the control value for the same
    /// block; that signal never reaches the audio sum. The two inputs are
    /// mutually exclusive per device, which the builder enforces.
    pub fn process_with_inputs(
        &mut self,
        buffer: &mut [f32],
        sidechains: &HashMap<String, Vec<f32>>,
        controls: &HashMap<String, Vec<f32>>,
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        let _ = self.try_process_with_inputs(
            buffer,
            sidechains,
            controls,
            sample_rate,
            transport.as_ref(),
        );
    }

    /// Check the complete sample range before touching devices or pending ramps.
    pub fn try_process_with_inputs(
        &mut self,
        buffer: &mut [f32],
        sidechains: &HashMap<String, Vec<f32>>,
        controls: &HashMap<String, Vec<f32>>,
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) -> Result<(), EffectChainProcessError> {
        self.try_process_with_lookup(buffer, sidechains, controls, sample_rate, transport)
    }

    fn process_with_lookup(
        &mut self,
        buffer: &mut [f32],
        sidechains: &impl SignalLookup,
        controls: &impl SignalLookup,
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) {
        let _ = self.try_process_with_lookup(buffer, sidechains, controls, sample_rate, transport);
    }

    fn try_process_with_lookup(
        &mut self,
        buffer: &mut [f32],
        sidechains: &impl SignalLookup,
        controls: &impl SignalLookup,
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) -> Result<(), EffectChainProcessError> {
        self.boundary.admit(buffer.len(), transport)?;
        let mut ramps = std::mem::take(&mut self.ramps);
        for (index, effect) in self.effects.iter_mut().enumerate() {
            let control = effect
                .control_source()
                .and_then(|source| controls.signal(source));
            let fading = ramps
                .iter()
                .any(|ramp| ramp.effect == index && !ramp.done());
            if let Some(control) = control {
                if fading {
                    Self::process_ramped(
                        effect.as_mut(),
                        &mut ramps,
                        index,
                        buffer,
                        EffectInputs {
                            key: None,
                            control: Some(control),
                        },
                        sample_rate,
                        transport,
                    );
                } else {
                    process_effect_aligned_control(
                        effect.as_mut(),
                        buffer,
                        control,
                        sample_rate,
                        transport,
                    );
                }
                continue;
            }
            let key = effect
                .sidechain_source()
                .and_then(|source| sidechains.signal(source));
            if fading {
                Self::process_ramped(
                    effect.as_mut(),
                    &mut ramps,
                    index,
                    buffer,
                    EffectInputs { key, control: None },
                    sample_rate,
                    transport,
                );
                continue;
            }
            process_effect_aligned(effect.as_mut(), buffer, key, sample_rate, transport);
        }
        self.ramps = ramps;
        Ok(())
    }

    /// Process one device while fading its pending parameter changes.
    ///
    /// The buffer is split at ramp steps because a device only takes a new
    /// value between processing calls; a key or control input is sliced to the
    /// same spans. A chain with no pending ramp never reaches this function.
    fn process_ramped(
        effect: &mut dyn AudioEffect,
        ramps: &mut [ParamRamp],
        index: usize,
        buffer: &mut [f32],
        inputs: EffectInputs<'_>,
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) {
        let EffectInputs { key, control } = inputs;
        let frames = buffer.len() / 2;
        let device_limit = effect.max_block_frames().max(1) as usize;
        let mut position = 0;
        while position < frames {
            // End the span on the last sample of the shortest running fade, so
            // that fade writes its exact goal instead of stopping next to it.
            let remaining = ramps
                .iter()
                .filter(|ramp| ramp.effect == index && !ramp.done())
                .map(|ramp| ramp.total - ramp.elapsed)
                .min();
            let end = match remaining {
                // Every fade for this device is through: hand the rest of the
                // block over in one call.
                None => frames,
                Some(remaining) => {
                    let step = (remaining as usize)
                        .min(PARAM_RAMP_STEP as usize)
                        .min(device_limit);
                    (position + step).min(frames)
                }
            };
            for ramp in ramps
                .iter_mut()
                .filter(|ramp| ramp.effect == index && !ramp.done())
            {
                let span = (end - position) as u32;
                // Write the value the step ends on: the last step then lands on
                // the goal, and every step moves the same distance.
                effect.set_param(&ramp.name, ramp.value_at(span));
                ramp.elapsed += span;
            }
            let range = position * 2..end * 2;
            let span_transport =
                transport.map(|base| base.at_sample(base.sample_pos + position as u64));
            match control {
                Some(control) => {
                    let span = control.get(position..end).unwrap_or(&[]);
                    process_effect_aligned_control(
                        effect,
                        &mut buffer[range],
                        span,
                        sample_rate,
                        span_transport.as_ref(),
                    );
                }
                None => {
                    let span = key.and_then(|key| key.get(range.clone())).unwrap_or(&[]);
                    process_effect_aligned(
                        effect,
                        &mut buffer[range],
                        key.map(|_| span),
                        sample_rate,
                        span_transport.as_ref(),
                    );
                }
            }
            position = end;
        }
    }

    pub fn reset(&mut self) {
        for fx in &mut self.effects {
            fx.reset();
        }
    }

    /// Enable the streaming idle gate for every hosted plugin in the chain.
    /// Disabling it also resumes any paused plugin, so an offline pass never
    /// skips a block.
    pub fn set_streaming_idle_gate(&mut self, enabled: bool) {
        for effect in &mut self.effects {
            effect.set_streaming_idle_gate(enabled);
        }
    }

    /// Release audio-thread state on the thread that processed the chain.
    pub fn end_processing(&mut self) {
        for fx in &mut self.effects {
            fx.end_processing();
        }
    }

    /// Total serial latency of all effects in this chain, in sample frames.
    pub fn latency_samples(&self) -> u32 {
        self.effects.iter().fold(0_u32, |total, effect| {
            total.saturating_add(effect.latency_samples())
        })
    }

    /// Conservative serial tail duration for this chain.
    pub fn tail_samples(&self) -> u32 {
        self.effects.iter().fold(0_u32, |total, effect| {
            total.saturating_add(effect.tail_samples())
        })
    }

    /// Serial ring-out a bounce has to render past its range end, in frames.
    ///
    /// The bounce counterpart of [`Self::tail_samples`]: a reverb declares its
    /// decay here so the media keeps the room, while the full-mix length
    /// policy keeps reading `tail_samples`.
    pub fn bounce_tail_samples(&self) -> u32 {
        self.effects.iter().fold(0_u32, |total, effect| {
            total.saturating_add(effect.bounce_tail_samples())
        })
    }

    /// Serial gain reduction of this chain in dB, for meters.
    pub fn gain_reduction_db(&self) -> f64 {
        self.effects
            .iter()
            .map(|effect| effect.gain_reduction_db())
            .sum()
    }
}

/// The slot a bypassed effect leaves in its chain.
///
/// It processes nothing, reports no latency and has no tail, so the chain
/// renders as if the effect were absent. Keeping the slot keeps chain indices
/// and meters lined up with the project's effect list.
struct Bypassed;

impl AudioEffect for Bypassed {
    fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}

    fn set_param(&mut self, _name: &str, _value: f64) {}

    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }

    fn reset(&mut self) {}

    fn name(&self) -> &str {
        "bypassed"
    }
}

/// A bus node: receives audio from sends + has its own effect chain.
pub struct BusNode {
    pub chain: EffectChain,
    pub buffer: Vec<f32>,
    /// Outgoing bus targets. Empty or `master` routes feed the master output.
    pub routes: Vec<String>,
    /// Post-effects return fader, linear.
    pub gain: f32,
    /// False when the bus is muted or silenced by another channel's solo.
    pub audible: bool,
}

/// One instrument auxiliary output routed to a bus before track effects.
pub struct AuxSendRoute {
    /// Zero-based stereo output pair on the hosting plugin instance.
    pub pair: usize,
    pub bus: String,
    pub amount: f64,
    /// Balance at the bus input, -1.0 to 1.0.
    pub pan: f64,
}

/// Channel width of the audio a track produces before fader and pan.
/// Measured from the buffer that reaches the pan stage, not from the source kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceWidth {
    /// One channel of content carried on both sides: constant-power pan.
    #[default]
    Mono,
    /// Independent left/right content: balance pan.
    Stereo,
}

impl SourceWidth {
    /// A buffer is stereo when any interleaved frame has different channels.
    pub fn of_buffer(buffer: &[f32]) -> Self {
        if buffer.chunks_exact(2).any(|frame| frame[0] != frame[1]) {
            Self::Stereo
        } else {
            Self::Mono
        }
    }
}

/// One post-insert track send into a bus.
#[derive(Clone, Debug, PartialEq)]
pub struct SendRoute {
    pub bus: String,
    /// Linear send gain (0.0 - 4.0, i.e. up to +12 dB).
    pub amount: f64,
    /// True taps before the fader and pan; false taps after both.
    pub pre_fader: bool,
    /// Balance at the bus input, -1.0 to 1.0.
    pub pan: f64,
}

/// How a track's audio is routed.
pub struct TrackRouting {
    pub direct_to_master: bool,
    pub sends: Vec<SendRoute>,
    /// Pre-FX plugin outputs: (pair, bus_name, amount 0.0-1.0)
    pub aux_sends: Vec<AuxSendRoute>,
    /// Volume in linear gain
    pub volume: f64,
    /// Pan: -1.0 (left) to +1.0 (right)
    pub pan: f64,
    /// Muted
    pub mute: bool,
    /// Post-insert channel polarity inversion.
    pub polarity_inverted: bool,
}

/// Per-track mixer state that can be patched into a live graph.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackMix {
    pub volume_db: f64,
    pub pan: f64,
    pub mute: bool,
    pub solo: bool,
    pub solo_safe: bool,
    pub polarity_inverted: bool,
}

impl TrackMix {
    /// Snapshot a project's mixer lanes in track order.
    pub fn from_project(project: &Project) -> Vec<Self> {
        project
            .tracks
            .iter()
            .map(|track| Self {
                volume_db: track.mix.volume_db,
                pan: track.mix.pan,
                mute: track.mix.mute,
                solo: track.mix.solo,
                solo_safe: track.mix.overrides.solo_safe,
                polarity_inverted: track.mix.polarity_inverted,
            })
            .collect()
    }
}

/// One flag per track: audible under the authored mute/solo state.
/// The mode picks which neighbourhood a solo keeps; the path to master always
/// stays open so the soloed node itself is heard.
pub fn audible_tracks(project: &Project) -> Vec<bool> {
    let any_track_solo = project.tracks.iter().any(|track| track.mix.solo);
    let any_bus_solo = project.buses.iter().any(|bus| bus.solo);
    if !any_track_solo && !any_bus_solo {
        return project.tracks.iter().map(|track| !track.mix.mute).collect();
    }
    let sets = solo_sets(project);
    let bus_index = bus_index(project);
    project
        .tracks
        .iter()
        .map(|track| {
            if track.mix.mute {
                return false;
            }
            if track.mix.solo || track.mix.overrides.solo_safe {
                return true;
            }
            if !any_bus_solo {
                return false;
            }
            track_routes_into_kept_bus(track, &bus_index, &sets.sources)
        })
        .collect()
}

/// Buses whose return stays audible under the authored mute/solo state.
pub fn audible_buses(project: &Project) -> Vec<bool> {
    let any_solo = project.tracks.iter().any(|track| track.mix.solo)
        || project.buses.iter().any(|bus| bus.solo);
    if !any_solo {
        return project.buses.iter().map(|bus| !bus.mute).collect();
    }
    let sets = solo_sets(project);
    project
        .buses
        .iter()
        .enumerate()
        .map(|(index, bus)| !bus.mute && (bus.solo || sets.returns[index]))
        .collect()
}

/// Every effect parameter a checked graph build would refuse, worded as that
/// build words it and prefixed with the place the effect sits in.
///
/// This runs the same factory the renderer runs, so `ocp validate` and the
/// panel's validation answer the question `ocp render` answers. External
/// plugins are skipped: loading one is not a parameter check, and the built-in
/// preflight documents that it leaves them to render time.
pub fn validate_effect_parameters(project: &Project, sample_rate: u32) -> Vec<String> {
    let Ok(expanded) = oc_dsl::dynamics::expand(project) else {
        return Vec::new();
    };
    let project = expanded.as_ref();
    let mut refusals = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut check = |effects: &[oc_dsl::ast::Effect], place: &str| {
        for effect in effects {
            if matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_)) {
                continue;
            }
            if let Err(error) =
                dsp::try_create_effect_at(&effect.effect_type, &effect.params, sample_rate, place)
                && seen.insert(format!("{place}: {error}"))
            {
                refusals.push(format!("{place}: {error}"));
            }
        }
    };
    for track in &project.tracks {
        let place = format!("track '{}'", track.name);
        if let Some(preset) = track
            .preset
            .as_ref()
            .and_then(|name| project.presets.iter().find(|preset| &preset.name == name))
        {
            check(&preset.effects, &place);
        }
        check(&track.effects, &place);
    }
    for bus in &project.buses {
        check(&bus.effects, &format!("bus '{}'", bus.name));
    }
    if let Some(master) = &project.master {
        check(&master.effects, "master");
    }
    refusals
}

fn bus_index(project: &Project) -> HashMap<&str, usize> {
    project
        .buses
        .iter()
        .enumerate()
        .map(|(index, bus)| (bus.name.as_str(), index))
        .collect()
}

fn track_routes_into_kept_bus(
    track: &oc_dsl::ast::Track,
    bus_index: &HashMap<&str, usize>,
    keep: &[bool],
) -> bool {
    let send_feeds = track.mix.sends.iter().any(|send| {
        bus_index
            .get(send.target.as_str())
            .is_some_and(|&index| keep[index])
    });
    let output_feeds = track
        .mix
        .output
        .as_deref()
        .and_then(|output| bus_index.get(output))
        .is_some_and(|&index| keep[index]);
    send_feeds || output_feeds
}

/// Bus sets the authored solo keeps alive.
struct SoloSets {
    /// Returns that stay audible: the soloed node, what it propagates to, and
    /// the routing that carries it to master.
    returns: Vec<bool>,
    /// Components whose input channels stay audible: the sources a solo exposes.
    sources: Vec<bool>,
}

fn solo_sets(project: &Project) -> SoloSets {
    let index = bus_index(project);
    let (successors, predecessors) = bus_edges(project, &index);
    let seeds = solo_bus_seeds(project, &index);
    match project.properties.solo_mode {
        SoloMode::Both => {
            let keep = mark_from(&seeds, &undirected(&successors, &predecessors));
            SoloSets {
                sources: keep.clone(),
                returns: keep,
            }
        }
        SoloMode::Upstream => {
            let soloed = soloed_buses(project);
            let sources = mark_from(&soloed, &predecessors);
            let mut returns = mark_from(&seeds, &successors);
            for (kept, source) in returns.iter_mut().zip(&sources) {
                *kept |= *source;
            }
            SoloSets { returns, sources }
        }
    }
}

/// Every bus edge as (source → target) and (target ← sources) lists.
fn bus_edges(
    project: &Project,
    index: &HashMap<&str, usize>,
) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let mut successors = vec![Vec::new(); project.buses.len()];
    let mut predecessors = vec![Vec::new(); project.buses.len()];
    for (source, bus) in project.buses.iter().enumerate() {
        for route in &bus.routes {
            if let Some(&target) = index.get(route.as_str()) {
                successors[source].push(target);
                predecessors[target].push(source);
            }
        }
    }
    (successors, predecessors)
}

fn soloed_buses(project: &Project) -> Vec<usize> {
    project
        .buses
        .iter()
        .enumerate()
        .filter(|(_, bus)| bus.solo)
        .map(|(index, _)| index)
        .collect()
}

/// Bus edges followed in both directions.
fn undirected(successors: &[Vec<usize>], predecessors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut adjacent = vec![Vec::new(); successors.len()];
    for (source, targets) in successors.iter().enumerate() {
        adjacent[source].extend(targets);
    }
    for (target, sources) in predecessors.iter().enumerate() {
        adjacent[target].extend(sources);
    }
    adjacent
}

/// Soloed buses plus the bus targets of every soloed or solo-safe channel.
fn solo_bus_seeds(project: &Project, index: &HashMap<&str, usize>) -> Vec<usize> {
    let mut seeds: Vec<usize> = project
        .buses
        .iter()
        .enumerate()
        .filter(|(_, bus)| bus.solo)
        .map(|(index, _)| index)
        .collect();
    for track in &project.tracks {
        if track.mix.mute || !(track.mix.solo || track.mix.overrides.solo_safe) {
            continue;
        }
        seeds.extend(
            track
                .mix
                .sends
                .iter()
                .filter_map(|send| index.get(send.target.as_str()).copied()),
        );
        seeds.extend(
            track
                .mix
                .output
                .as_deref()
                .and_then(|output| index.get(output).copied()),
        );
    }
    seeds
}

fn mark_from(seeds: &[usize], edges: &[Vec<usize>]) -> Vec<bool> {
    let mut marked = vec![false; edges.len()];
    let mut queue = seeds.to_vec();
    while let Some(node) = queue.pop() {
        if std::mem::replace(&mut marked[node], true) {
            continue;
        }
        queue.extend(edges[node].iter().copied());
    }
    marked
}

impl AudioGraph {
    /// Patch faders, pan, polarity and mute/solo in place; chains stay alive.
    /// Called off the audio callback; rebuilding solo support may allocate.
    pub fn apply_mixer_state(&mut self, project: &Project) {
        let mixes = TrackMix::from_project(project);
        let tracks_audible = audible_tracks(project);
        let buses_audible = audible_buses(project);
        for (index, (routing, mix)) in self.routing.iter_mut().zip(&mixes).enumerate() {
            routing.volume = db_to_linear(mix.volume_db);
            routing.pan = mix.pan;
            routing.mute = !tracks_audible.get(index).copied().unwrap_or(false);
            routing.polarity_inverted = mix.polarity_inverted;
        }
        for (index, bus) in project.buses.iter().enumerate() {
            if let Some(node) = self.bus_chains.get_mut(&bus.name) {
                node.gain = db_to_linear(bus.volume_db) as f32;
                node.audible = buses_audible.get(index).copied().unwrap_or(false);
            }
        }
    }

    /// True when this track's output is gated to silence: it is muted,
    /// solo-silenced, or feeds a control source without reaching the mix.
    pub fn track_is_silenced(&self, index: usize) -> bool {
        self.routing.get(index).is_some_and(|route| route.mute)
            || self.control_only.get(index).copied().unwrap_or(false)
    }

    /// Push silence through a gated track's PDC lines. The channel stays in
    /// the compensation timeline, so unmuting resumes at its correct position.
    fn advance_silent_routes(
        &mut self,
        index: usize,
        track_samples: usize,
        buf_len: usize,
        aux_buffers: &HashMap<(usize, usize), Vec<f32>>,
    ) {
        let Some(route) = self.routing.get(index) else {
            return;
        };
        if route.direct_to_master
            && let Some(delay) = self.direct_delays.get_mut(index)
        {
            delay.advance_silent(track_samples);
        }
        for send_index in 0..route.sends.len() {
            if let Some(delay) = self.send_delays.get_mut(&(index, send_index)) {
                delay.advance_silent(track_samples);
            }
        }
        for send_index in 0..route.aux_sends.len() {
            let pair = route.aux_sends[send_index].pair;
            let Some(aux) = aux_buffers.get(&(index, pair)) else {
                continue;
            };
            if let Some(delay) = self.aux_send_delays.get_mut(&(index, send_index)) {
                delay.advance_silent(aux.len().min(buf_len));
            }
        }
    }

    /// Replace send routes after a mixer-only edit. Called off the audio
    /// callback, so rebuilding the PDC plan may allocate.
    pub fn apply_send_state(&mut self, project: &Project) {
        let mut topology_changed = false;
        for (index, (routing, track)) in self.routing.iter_mut().zip(&project.tracks).enumerate() {
            let (sends, direct_to_master) = track_send_routes(track);
            let old: Vec<(&str, bool)> = routing
                .sends
                .iter()
                .map(|send| (send.bus.as_str(), send.pre_fader))
                .collect();
            let new: Vec<(&str, bool)> = sends
                .iter()
                .map(|send| (send.bus.as_str(), send.pre_fader))
                .collect();
            topology_changed |= old != new;
            routing.sends = sends;
            routing.direct_to_master = direct_to_master;
            if let Some(ramps) = self.ramps.tracks.get_mut(index) {
                ramps.resize_sends(&routing.sends);
            }

            let aux_sends: Vec<AuxSendRoute> = track
                .mix
                .sends
                .iter()
                .filter_map(|send| {
                    send.source.as_ref().map(|source| AuxSendRoute {
                        pair: source.pair(),
                        bus: send.target.clone(),
                        amount: send.amount,
                        pan: send.pan,
                    })
                })
                .collect();
            let old_aux: Vec<(usize, &str)> = routing
                .aux_sends
                .iter()
                .map(|send| (send.pair, send.bus.as_str()))
                .collect();
            let new_aux: Vec<(usize, &str)> = aux_sends
                .iter()
                .map(|send| (send.pair, send.bus.as_str()))
                .collect();
            topology_changed |= old_aux != new_aux;
            routing.aux_sends = aux_sends;
        }
        if topology_changed {
            self.invalidate_latency_plan();
        }
    }
    /// Patch only the master fader; the instrument and effect chains stay alive.
    pub fn set_master_gain_db(&mut self, volume_db: f64) {
        self.master_gain = db_to_linear(volume_db) as f32;
    }

    /// Patch the chains one FX edit changed, without rebuilding any source.
    ///
    /// A chain whose devices and inputs are unchanged only fades its changed
    /// parameters into the live devices, so its DSP state (delay lines, reverb
    /// tails, plugin state) survives the edit. A chain whose shape changed — an
    /// effect added, removed, reordered or bypassed, or a lane rewritten — is
    /// rebuilt. Every other chain is left alone. `previous` is the document the
    /// graph was built from, `next` the document edited from it.
    ///
    /// `fade` asks for the fixed crossfade a released touch already uses. A
    /// stopped transport passes `false`: nothing can click there, and the next
    /// press must render the new value from its first sample.
    ///
    /// [`ChainPatch::Structural`] means the edit changed the graph's node set
    /// (a track or bus came or went, or the master section did) and the caller
    /// must reload the project instead.
    pub fn patch_chains(
        &mut self,
        previous: &Project,
        next: &Project,
        touch: Option<&Arc<touch::TouchRegistry>>,
        fade: bool,
    ) -> Result<ChainPatch, String> {
        if previous.tracks.len() != self.track_chains.len()
            || next.tracks.len() != self.track_chains.len()
            || previous.buses.len() != next.buses.len()
            || next.buses.len() != self.bus_chains.len()
            || next
                .buses
                .iter()
                .any(|bus| !self.bus_chains.contains_key(&bus.name))
            || previous.master.is_some() != next.master.is_some()
        {
            return Ok(ChainPatch::Structural);
        }
        let track_plans: Vec<ChainPlan> = (0..self.track_chains.len())
            .map(|index| {
                chain_plan(
                    &track_shape(&previous.tracks[index]),
                    &track_shape(&next.tracks[index]),
                )
            })
            .collect();
        let bus_plans: Vec<(usize, ChainPlan)> = next
            .buses
            .iter()
            .enumerate()
            .map(|(index, bus)| {
                let before = previous
                    .buses
                    .iter()
                    .find(|before| before.name == bus.name)
                    .ok_or_else(|| format!("Bus '{}' not found", bus.name))?;
                Ok((index, chain_plan(&bus_shape(before), &bus_shape(bus))))
            })
            .collect::<Result<_, String>>()?;
        let master_plan = match (&previous.master, &next.master) {
            (Some(before), Some(after)) => chain_plan(&master_shape(before), &master_shape(after)),
            _ => ChainPlan::Unchanged,
        };

        if fade {
            for (index, plan) in track_plans.iter().enumerate() {
                if let ChainPlan::Fade(changes) = plan {
                    self.track_chains[index].validate_fade_params(changes)?;
                }
            }
            for (index, plan) in &bus_plans {
                if let ChainPlan::Fade(changes) = plan
                    && let Some(node) = self.bus_chains.get(&next.buses[*index].name)
                {
                    node.chain.validate_fade_params(changes)?;
                }
            }
            if let ChainPlan::Fade(changes) = &master_plan {
                self.master_chain.validate_fade_params(changes)?;
            }
        }
        let mut patched = 0;
        let mut rebuilt = false;
        // Built on the first rebuild only: a fade needs no document copy.
        let mut document: Option<Project> = None;
        for (index, plan) in track_plans.into_iter().enumerate() {
            match plan {
                ChainPlan::Unchanged => {}
                ChainPlan::Fade(changes) => {
                    if fade {
                        self.track_chains[index].fade_params(&changes, self.sample_rate)?;
                    } else {
                        self.track_chains[index].apply_params(&changes);
                    }
                    patched += 1;
                }
                ChainPlan::Rebuild => {
                    let document = document.get_or_insert_with(|| graph_document(next));
                    let chain = build_track_chain(document, index, self.sample_rate, true, touch)?;
                    // Release the replaced chain's plugins on the thread that
                    // processed them.
                    self.track_chains[index].end_processing();
                    self.track_chains[index] = chain;
                    rebuilt = true;
                    patched += 1;
                }
            }
        }
        for (index, plan) in bus_plans {
            let name = &next.buses[index].name;
            match plan {
                ChainPlan::Unchanged => {}
                ChainPlan::Fade(changes) => {
                    if let Some(node) = self.bus_chains.get_mut(name) {
                        if fade {
                            node.chain.fade_params(&changes, self.sample_rate)?;
                        } else {
                            node.chain.apply_params(&changes);
                        }
                    }
                    patched += 1;
                }
                ChainPlan::Rebuild => {
                    let document = document.get_or_insert_with(|| graph_document(next));
                    let chain = build_bus_chain(document, index, self.sample_rate, true, touch)?;
                    if let Some(node) = self.bus_chains.get_mut(name) {
                        node.chain.end_processing();
                        node.chain = chain;
                    }
                    rebuilt = true;
                    patched += 1;
                }
            }
        }
        match master_plan {
            ChainPlan::Unchanged => {}
            ChainPlan::Fade(changes) => {
                if fade {
                    self.master_chain.fade_params(&changes, self.sample_rate)?;
                } else {
                    self.master_chain.apply_params(&changes);
                }
                patched += 1;
            }
            ChainPlan::Rebuild => {
                let document = document.get_or_insert_with(|| graph_document(next));
                let chain = build_master_chain(document, self.sample_rate, true, touch)?;
                self.master_chain.end_processing();
                self.master_chain = chain;
                rebuilt = true;
                patched += 1;
            }
        }
        if rebuilt {
            // A rebuilt chain can move a detector edge or a latency. A fade
            // cannot, so it never disturbs another chain's alignment state.
            let feeds: HashMap<String, usize> = self
                .control_runtimes
                .iter()
                .map(|runtime| (runtime.name.clone(), runtime.source_track))
                .collect();
            self.key_plan = KeyPlan::build(
                &self.track_names,
                &self.track_chains,
                &self.bus_chains,
                &self.master_chain,
                &feeds,
            );
            if self.current_latency_signature() != self.latency_signature {
                // Re-planning the alignment flushes its delay lines, so only a
                // chain that actually moved its latency asks for it.
                self.configure_sidechain_alignment();
            }
        }
        self.refresh_latency_compensation();
        Ok(ChainPatch::Patched { chains: patched })
    }

    /// Bind an envelope to an existing send, without creating effects or routes.
    pub fn bind_performance_send(
        &mut self,
        track: usize,
        bus: &str,
        envelope: crate::performance_send::SendEnvelope,
    ) -> Result<(), String> {
        let route = self
            .routing
            .get(track)
            .ok_or("unknown performance send track")?;
        if !self.bus_chains.contains_key(bus)
            || route.sends.iter().filter(|send| send.bus == bus).count() != 1
        {
            return Err("performance send requires one declared route to an existing bus".into());
        }
        if self
            .performance_sends
            .contains_key(&(track, bus.to_owned()))
        {
            return Err("performance send already bound".into());
        }
        self.performance_sends
            .insert((track, bus.to_owned()), envelope);
        Ok(())
    }

    /// Build an audio graph from a Project AST.
    pub fn from_project(project: &Project, sample_rate: u32) -> Self {
        Self::from_project_with_source_latencies(project, sample_rate, &[])
    }

    /// Build a graph with latency already incurred by each rendered track
    /// source (for example a CLAP/VST3 instrument before insert effects).
    pub fn from_project_with_source_latencies(
        project: &Project,
        sample_rate: u32,
        source_latencies: &[u32],
    ) -> Self {
        Self::build_project(project, sample_rate, source_latencies, false, None)
            .expect("legacy graph construction")
    }

    pub fn try_from_project_with_source_latencies(
        project: &Project,
        sample_rate: u32,
        source_latencies: &[u32],
    ) -> Result<Self, String> {
        Self::build_project(project, sample_rate, source_latencies, true, None)
    }

    /// Live variant: automated parameters consult `touch` for the hand-vs-lane
    /// decision. Offline builds pass `None` and never see touch state.
    pub fn try_from_project_with_source_latencies_and_touch(
        project: &Project,
        sample_rate: u32,
        source_latencies: &[u32],
        touch: Arc<touch::TouchRegistry>,
    ) -> Result<Self, String> {
        Self::build_project(project, sample_rate, source_latencies, true, Some(touch))
    }

    fn build_project(
        project: &Project,
        sample_rate: u32,
        source_latencies: &[u32],
        checked: bool,
        touch: Option<Arc<touch::TouchRegistry>>,
    ) -> Result<Self, String> {
        // Authoritative render entry points reject resolution errors. Preserve
        // this legacy infallible graph API for already-validated callers.
        let resolved = if checked {
            Some(oc_dsl::preset::resolve(project)?)
        } else {
            oc_dsl::preset::resolve(project).ok()
        };
        let project = resolved.as_ref().unwrap_or(project);
        // A hairpin is a phrase mark in the source; the graph renders the gain
        // ramp it expands to, so the renderer needs no dynamics of its own.
        let expanded = oc_dsl::dynamics::expand(project)?;
        let project = expanded.as_ref();
        if checked {
            reject_bus_routing_cycles(project)?;
        }
        let mut track_chains = Vec::new();
        let mut routing = Vec::new();
        let tracks_audible = audible_tracks(project);
        let buses_audible = audible_buses(project);

        for (track_index, track) in project.tracks.iter().enumerate() {
            // Build effect chain for this track
            track_chains.push(build_track_chain(
                project,
                track_index,
                sample_rate,
                checked,
                touch.as_ref(),
            )?);

            // Build routing
            let (sends, direct_to_master) = track_send_routes(track);
            let aux_sends: Vec<AuxSendRoute> = track
                .mix
                .sends
                .iter()
                .filter_map(|send| {
                    send.source.as_ref().map(|source| AuxSendRoute {
                        pair: source.pair(),
                        bus: send.target.clone(),
                        amount: send.amount,
                        pan: send.pan,
                    })
                })
                .collect();
            if checked {
                for send in &aux_sends {
                    if send.pair == 0 || !project.buses.iter().any(|bus| bus.name == send.bus) {
                        return Err(format!(
                            "track '{}' routes an invalid plugin auxiliary output",
                            track.name
                        ));
                    }
                }
                for send in &sends {
                    if !project.buses.iter().any(|bus| bus.name == send.bus) {
                        return Err(format!(
                            "track '{}' sends to unknown bus '{}'",
                            track.name, send.bus
                        ));
                    }
                }
            }
            routing.push(TrackRouting {
                direct_to_master,
                sends,
                aux_sends,
                volume: db_to_linear(track.mix.volume_db),
                pan: track.mix.pan,
                mute: !tracks_audible.get(track_index).copied().unwrap_or(true),
                polarity_inverted: track.mix.polarity_inverted,
            });
        }

        // Build bus chains
        let mut bus_chains = HashMap::new();
        for (index, bus) in project.buses.iter().enumerate() {
            let chain = build_bus_chain(project, index, sample_rate, checked, touch.as_ref())?;
            bus_chains.insert(
                bus.name.clone(),
                BusNode {
                    chain,
                    buffer: Vec::new(), // allocated per render call
                    routes: bus.routes.clone(),
                    gain: db_to_linear(bus.volume_db) as f32,
                    audible: buses_audible.get(index).copied().unwrap_or(false),
                },
            );
        }

        // Build master chain
        let master_chain = match &project.master {
            Some(_) => build_master_chain(project, sample_rate, checked, touch.as_ref())?,
            None => EffectChain::new(),
        };

        let mut control_runtimes = Vec::new();
        let mut control_indices: HashMap<&str, usize> = HashMap::new();
        for source in &project.properties.control_sources {
            if checked {
                control_source::check_source(source)?;
            }
            let Some(source_track) = project
                .tracks
                .iter()
                .position(|track| track.name == source.source_track)
            else {
                if checked {
                    return Err(format!(
                        "control source '{}' references unknown track '{}'",
                        source.name, source.source_track
                    ));
                }
                continue;
            };
            let runtime = control_source::ControlRuntime::new(source, source_track, sample_rate)?;
            control_indices.insert(source.name.as_str(), control_runtimes.len());
            control_runtimes.push(runtime);
        }
        let mut control_gains = Vec::with_capacity(project.tracks.len());
        for track in &project.tracks {
            let ride = track.mix.control_gain.as_ref().and_then(|gain| {
                let source = *control_indices.get(gain.source.as_str())?;
                Some(control_source::GainRide::new(source, gain.depth_db))
            });
            if checked && track.mix.control_gain.is_some() && ride.is_none() {
                return Err(format!(
                    "track '{}' drives its gain from unknown control source '{}'",
                    track.name,
                    track
                        .mix
                        .control_gain
                        .as_ref()
                        .map_or("", |gain| &gain.source)
                ));
            }
            control_gains.push(ride);
        }

        let track_names: Vec<String> = project
            .tracks
            .iter()
            .map(|track| track.name.clone())
            .collect();
        let mut control_feeds: HashMap<String, usize> = HashMap::new();
        for source in &project.properties.control_sources {
            if let Some(feed) = track_names
                .iter()
                .position(|name| name == &source.source_track)
            {
                control_feeds.insert(source.name.clone(), feed);
            }
        }
        if checked {
            for (consumer, chain) in track_chains.iter().enumerate() {
                for source in chain.control_sources() {
                    let Some(&feed) = control_feeds.get(source) else {
                        return Err(format!(
                            "track '{}' reads undeclared control source '{}'",
                            track_names[consumer], source
                        ));
                    };
                    if feed == consumer {
                        return Err(format!(
                            "track '{}' cannot feed control source '{}' into its own chain",
                            track_names[consumer], source
                        ));
                    }
                }
            }
            for chain in bus_chains
                .values()
                .map(|bus| &bus.chain)
                .chain(std::iter::once(&master_chain))
            {
                for source in chain.control_sources() {
                    if !control_feeds.contains_key(source) {
                        return Err(format!(
                            "a chain reads undeclared control source '{source}'"
                        ));
                    }
                }
            }
        }
        if checked {
            reject_signal_cycles(&track_names, &track_chains, &bus_chains, &routing)?;
        }
        warn_unresolved_keys(&track_names, &track_chains, &bus_chains, &master_chain);
        let key_plan = KeyPlan::build(
            &track_names,
            &track_chains,
            &bus_chains,
            &master_chain,
            &control_feeds,
        );
        if checked && !key_plan.cycle.is_empty() {
            let names = key_plan
                .cycle
                .iter()
                .map(|index| format!("'{}'", track_names[*index]))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "control routes close a loop between tracks {names}; a control source is evaluated before its consumer"
            ));
        }
        let control_runtime_count = control_runtimes.len();
        let ramps = MixerRamps::for_project(&routing, &bus_chains);
        let mut graph = Self {
            track_chains,
            bus_chains,
            master_chain,
            master_gain: db_to_linear(project.master.as_ref().map_or(0.0, |m| m.volume_db)) as f32,
            ramps,
            routing,
            sample_rate,
            track_widths: project
                .tracks
                .iter()
                .map(|track| {
                    if track.mix.stereo_balance {
                        SourceWidth::Stereo
                    } else {
                        SourceWidth::Mono
                    }
                })
                .collect(),
            source_latencies: (0..project.tracks.len())
                .map(|index| source_latencies.get(index).copied().unwrap_or(0))
                .collect(),
            latency_compensation: project
                .tracks
                .iter()
                .map(|track| track.mix.latency_compensation)
                .collect(),
            latency_offsets: project
                .tracks
                .iter()
                .map(|track| track.mix.latency_offset)
                .collect(),
            recording: false,
            latency_diagnostics: Vec::new(),
            latency_readout: Vec::new(),
            pre_master_latency_samples: 0,
            direct_delays: Vec::new(),
            send_delays: HashMap::new(),
            aux_send_delays: HashMap::new(),
            performance_sends: HashMap::new(),
            send_cursor: 0,
            bus_route_delays: HashMap::new(),
            bus_order: Vec::new(),
            bus_downstream_latencies: HashMap::new(),
            latency_signature: Vec::new(),
            track_names,
            spectrum: None,
            spectrum_target: None,
            control_runtimes,
            control_gains,
            control_only: Vec::new(),
            control_fresh: vec![false; control_runtime_count],
            key_plan,
            prepared: None,
        };
        graph.configure_sidechain_alignment();
        graph.refresh_latency_compensation();
        for index in 0..project.tracks.len() {
            for (_, bus, envelope) in
                crate::performance_send::project_plan(project, index, sample_rate)?
            {
                graph.bind_performance_send(index, &bus, envelope)?;
            }
        }
        Ok(graph)
    }

    /// Align each keyed effect with the source's **post-insert** tap: reported
    /// source latency plus the source chain's own latency. Sources come before
    /// their consumers, so a source's tap latency is final when it is read.
    fn configure_sidechain_alignment(&mut self) {
        let mut tap_latency = vec![0_u32; self.track_names.len()];
        let mut resolved = vec![false; self.track_names.len()];
        let planned = self.key_plan.order.len() == self.track_chains.len();
        for position in 0..self.track_chains.len() {
            let index = if planned {
                self.key_plan.order[position]
            } else {
                position
            };
            let mut chain = std::mem::take(&mut self.track_chains[index]);
            let mut main_latency = self.source_latencies.get(index).copied().unwrap_or(0);
            for effect in &mut chain.effects {
                if let Some(source) = effect.sidechain_source()
                    && let Some(source_index) =
                        self.track_names.iter().position(|name| name == source)
                    && resolved.get(source_index).copied().unwrap_or(false)
                {
                    effect.configure_sidechain_alignment(main_latency, tap_latency[source_index]);
                }
                main_latency = main_latency.saturating_add(effect.latency_samples());
            }
            self.track_chains[index] = chain;
            if let Some(slot) = tap_latency.get_mut(index) {
                *slot = main_latency;
            }
            if let Some(slot) = resolved.get_mut(index) {
                *slot = true;
            }
        }
    }

    pub fn has_track_sidechains(&self) -> bool {
        self.track_chains.iter().any(EffectChain::has_sidechain)
    }

    /// True when a track chain reads another track's signal, by key or control.
    /// Those chains run in dependency order, never in parallel.
    pub fn has_track_inputs(&self) -> bool {
        self.has_track_sidechains()
            || self
                .track_chains
                .iter()
                .any(EffectChain::has_control_inputs)
    }

    /// Advance every control source over one block of post-track-FX audio.
    ///
    /// A source that a track's control input already refreshed in the ordered
    /// FX pass is skipped: advancing it twice would make the follower block
    /// dependent. The flags clear here, once per block.
    fn update_control_sources(&mut self, track_buffers: &[Vec<f32>], len: usize) {
        if self.control_runtimes.is_empty() {
            return;
        }
        let frames = len / 2;
        for index in 0..self.control_runtimes.len() {
            if self.control_fresh.get(index).copied().unwrap_or(false) {
                continue;
            }
            // Routing mute silences the feed; a control-only mask does not: that
            // track is processed exactly so its follower keeps running.
            let source_track = self.control_runtimes[index].source_track;
            let silent = self
                .routing
                .get(source_track)
                .is_none_or(|route| route.mute);
            let source: &[f32] = if silent {
                &[]
            } else {
                track_buffers
                    .get(source_track)
                    .map_or(&[][..], Vec::as_slice)
            };
            self.control_runtimes[index].update(source, frames);
        }
        self.control_fresh.fill(false);
    }

    /// Refresh the followers fed by one track, so a consumer's chain sees this
    /// block's value instead of the previous block's.
    fn refresh_control_sources(&mut self, source_track: usize, source: &[f32], frames: usize) {
        for (index, runtime) in self.control_runtimes.iter_mut().enumerate() {
            if runtime.source_track != source_track {
                continue;
            }
            runtime.update(source, frames);
            if let Some(fresh) = self.control_fresh.get_mut(index) {
                *fresh = true;
            }
        }
    }

    /// Tracks whose audio feeds a control source. A window render must process
    /// them even when the caller did not select them.
    pub fn control_source_tracks(&self) -> Vec<usize> {
        self.control_runtimes
            .iter()
            .map(|runtime| runtime.source_track)
            .collect()
    }

    /// Process these tracks for their control feed only; their own output stays
    /// silent in the next mix.
    pub fn set_control_only_tracks(&mut self, tracks: &[usize]) {
        self.control_only = vec![false; self.routing.len()];
        for &track in tracks {
            if let Some(slot) = self.control_only.get_mut(track) {
                *slot = true;
            }
        }
    }

    /// Last block's control values for a named source, one value per frame.
    pub fn control_values(&self, name: &str) -> Option<&[f32]> {
        self.control_runtimes
            .iter()
            .find(|runtime| runtime.name == name)
            .map(|runtime| runtime.last_values())
    }

    /// Select the node whose pre-effects signal feeds the analyser; None stops
    /// all analysis work. Called off the audio callback.
    pub fn set_spectrum_target(&mut self, target: Option<SpectrumTarget>) {
        if target.is_some() && self.spectrum.is_none() {
            self.spectrum = Some(SpectrumAnalyser::new(self.sample_rate));
        }
        if self.spectrum_target != target
            && let Some(analyser) = &mut self.spectrum
        {
            analyser.reset();
        }
        self.spectrum_target = target;
    }

    pub fn spectrum_bins(&self) -> Option<&[f32; SPECTRUM_BANDS]> {
        self.spectrum_target.as_ref()?;
        self.spectrum.as_ref().map(SpectrumAnalyser::bands)
    }

    fn tap_spectrum(&mut self, target: &SpectrumTarget, buffer: &[f32]) {
        if self.spectrum_target.as_ref() != Some(target) {
            return;
        }
        if let Some(analyser) = &mut self.spectrum {
            analyser.push_interleaved(buffer);
        }
    }

    pub fn process_track_effects(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
    ) {
        self.process_track_effects_selection(track_buffers, transport, None);
    }

    /// Process the effects of `indices` only; other buffers are left untouched.
    ///
    /// A key tap is **post-insert, pre-fader, pre-mute**: the source's buffer
    /// immediately after its own effect chain, before polarity, sends, fader,
    /// pan, ride and mute. Chains therefore run in key order, and a muted key
    /// source still runs its chain so the key carries the signal.
    pub fn process_track_effects_for(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        indices: &[usize],
    ) {
        self.process_track_effects_selection(track_buffers, transport, Some(indices));
    }

    /// Shared body for the all-tracks and selected-tracks entry points.
    fn process_track_effects_selection(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        indices: Option<&[usize]>,
    ) {
        let samples = track_buffers.iter().map(Vec::len).max().unwrap_or(0);
        self.ensure_prepared(track_buffers, &HashMap::new(), samples);
        let mut prepared = self.prepared.take().unwrap();
        self.process_track_effects_prepared(
            track_buffers,
            transport.as_ref(),
            indices,
            &mut prepared,
        );
        self.prepared = Some(prepared);
    }

    fn process_track_effects_prepared(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<&PluginTransport>,
        indices: Option<&[usize]>,
        prepared: &mut PreparedGraph,
    ) {
        prepared.track_taps.clear();
        let planned = self.key_plan.order.len() == track_buffers.len();
        for position in 0..track_buffers.len() {
            let index = if planned {
                self.key_plan.order[position]
            } else {
                position
            };
            if indices.is_some_and(|indices| !indices.contains(&index)) {
                continue;
            }
            let tapped = self.key_plan.is_tapped(index);
            if self.routing.get(index).is_some_and(|route| route.mute) && !tapped {
                continue;
            }
            let Some(buffer) = track_buffers.get_mut(index) else {
                continue;
            };
            self.tap_spectrum(&SpectrumTarget::Track(index), buffer);
            let frames = buffer.len() / 2;
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
            let muted = self.routing.get(index).is_some_and(|route| route.mute);
            self.refresh_control_sources(index, if muted { &[] } else { buffer }, frames);
            if tapped && let Some(name) = self.track_names.get(index) {
                prepared.track_taps.set(name, buffer);
            }
        }
    }

    /// Track names each effect chain of `index` reads as a sidechain key.
    pub fn track_sidechain_sources(&self, index: usize) -> impl Iterator<Item = &str> {
        self.track_chains
            .get(index)
            .into_iter()
            .flat_map(EffectChain::sidechain_sources)
    }

    /// Tracks whose post-insert buffer a key reads.
    pub fn sidechain_source_tracks(&self) -> Vec<usize> {
        self.key_plan.tapped_tracks().collect()
    }

    /// True when a key tap reads this track's post-insert buffer.
    pub fn is_key_source(&self, index: usize) -> bool {
        self.key_plan.is_tapped(index)
    }

    /// Adopt widths classified by an earlier pass, so a window's pan law does
    /// not depend on how much of the track it happens to cover. Explicit
    /// balance stays in effect even when the earlier pass detected mono.
    pub fn set_track_widths(&mut self, widths: &[SourceWidth]) {
        for (width, value) in self.track_widths.iter_mut().zip(widths) {
            if *value == SourceWidth::Stereo {
                *width = SourceWidth::Stereo;
            }
        }
    }

    /// Promote track widths from the buffers about to reach the pan stage.
    /// Promote-only, so per-block streaming cannot flip the pan law.
    pub fn detect_track_widths(&mut self, track_buffers: &[Vec<f32>]) {
        for (index, buffer) in track_buffers.iter().enumerate() {
            let Some(width) = self.track_widths.get_mut(index) else {
                break;
            };
            if *width == SourceWidth::Mono && SourceWidth::of_buffer(buffer) == SourceWidth::Stereo
            {
                *width = SourceWidth::Stereo;
            }
        }
    }

    /// Width currently applied to each track's fader and pan.
    pub fn track_widths(&self) -> &[SourceWidth] {
        &self.track_widths
    }

    fn current_latency_signature(&self) -> Vec<(String, u32)> {
        let mut signature: Vec<(String, u32)> = self
            .track_chains
            .iter()
            .enumerate()
            .map(|(index, chain)| {
                (
                    format!("track:{index}"),
                    self.source_latencies
                        .get(index)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(chain.latency_samples()),
                )
            })
            .collect();
        let mut buses: Vec<_> = self.bus_chains.iter().collect();
        buses.sort_unstable_by_key(|(name, _)| *name);
        signature.extend(
            buses
                .into_iter()
                .map(|(name, bus)| (format!("bus:{name}"), bus.chain.latency_samples())),
        );
        signature
    }

    /// Re-query hosted effect latency and rebuild PDC only when it changed.
    /// Stable plans retain their delay-line history across render blocks.
    fn refresh_latency_compensation(&mut self) -> bool {
        let signature = self.current_latency_signature();
        if signature == self.latency_signature {
            return false;
        }
        self.latency_signature = signature;
        self.rebuild_latency_compensation();
        true
    }

    /// Force the next render block to rebuild PDC (send topology changed).
    fn invalidate_latency_plan(&mut self) {
        self.latency_signature.clear();
    }

    fn bus_path_latency(&self, bus_name: &str) -> u32 {
        self.bus_chains
            .get(bus_name)
            .map(|bus| {
                bus.chain.latency_samples().saturating_add(
                    self.bus_downstream_latencies
                        .get(bus_name)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .unwrap_or(0)
    }

    /// Build a deterministic source-before-target order. Invalid cyclic nodes
    /// are appended for one bounded pass rather than allowing a render to hang;
    /// the DSL validator remains responsible for reporting the cycle.
    fn rebuild_bus_plan(&mut self) {
        let mut indegree: HashMap<String, usize> = self
            .bus_chains
            .keys()
            .cloned()
            .map(|name| (name, 0))
            .collect();
        for bus in self.bus_chains.values() {
            for target in effective_bus_routes(bus) {
                if let Some(value) = indegree.get_mut(&target) {
                    *value += 1;
                }
            }
        }
        // A bus effect that listens to another bus must run after its key
        // source. Treat the detector edge like an audio-routing edge for the
        // scheduling order, without adding the detector audio to the mix.
        for (target, bus) in &self.bus_chains {
            for source in bus.chain.sidechain_sources() {
                if source != target
                    && self.bus_chains.contains_key(source)
                    && let Some(value) = indegree.get_mut(target)
                {
                    *value += 1;
                }
            }
        }
        let mut ready: Vec<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(name, _)| name.clone())
            .collect();
        ready.sort_unstable_by(|a, b| b.cmp(a));
        let mut order = Vec::with_capacity(indegree.len());
        while let Some(name) = ready.pop() {
            order.push(name.clone());
            let Some(bus) = self.bus_chains.get(&name) else {
                continue;
            };
            for target in effective_bus_routes(bus) {
                if let Some(value) = indegree.get_mut(&target) {
                    *value -= 1;
                    if *value == 0 {
                        ready.push(target);
                        ready.sort_unstable_by(|a, b| b.cmp(a));
                    }
                }
            }
            for (target, target_bus) in &self.bus_chains {
                if target_bus
                    .chain
                    .sidechain_sources()
                    .any(|source| source == name)
                    && let Some(value) = indegree.get_mut(target)
                {
                    *value -= 1;
                    if *value == 0 {
                        ready.push(target.clone());
                        ready.sort_unstable_by(|a, b| b.cmp(a));
                    }
                }
            }
        }
        let mut cyclic: Vec<String> = indegree
            .into_iter()
            .filter(|(name, _)| !order.contains(name))
            .map(|(name, _)| name)
            .collect();
        cyclic.sort_unstable();
        order.extend(cyclic);
        self.bus_order = order;

        self.bus_downstream_latencies.clear();
        for name in self.bus_order.iter().rev() {
            let downstream = self
                .bus_chains
                .get(name)
                .map(|bus| {
                    effective_bus_routes(bus)
                        .into_iter()
                        .filter(|target| self.bus_chains.contains_key(target))
                        .map(|target| {
                            self.bus_chains[&target]
                                .chain
                                .latency_samples()
                                .saturating_add(
                                    self.bus_downstream_latencies
                                        .get(&target)
                                        .copied()
                                        .unwrap_or(0),
                                )
                        })
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            self.bus_downstream_latencies
                .insert(name.clone(), downstream);
        }
    }

    /// Maximum latency before the common master chain. Offline renderers use
    /// this to retain audio shifted by PDC rather than truncating the tail.
    pub fn pre_master_latency_samples(&self) -> u32 {
        self.pre_master_latency_samples
    }

    /// Current gain reduction of one track's effect chain, in dB.
    pub fn track_gain_reduction_db(&self, index: usize) -> f64 {
        self.track_chains
            .get(index)
            .map_or(0.0, EffectChain::gain_reduction_db)
    }

    /// Current gain reduction of one bus effect chain, in dB.
    pub fn bus_gain_reduction_db(&self, name: &str) -> f64 {
        self.bus_chains
            .get(name)
            .map_or(0.0, |bus| bus.chain.gain_reduction_db())
    }

    /// Current gain reduction of the master effect chain, in dB.
    pub fn master_gain_reduction_db(&self) -> f64 {
        self.master_chain.gain_reduction_db()
    }

    /// Bus names in processing order, for meter layout.
    pub fn bus_names(&self) -> &[String] {
        &self.bus_order
    }

    /// Maximum effect tail across direct and send-return paths, including the
    /// common master chain. Instrument-source tails are budgeted by RenderCache.
    pub fn max_effect_tail_samples(&self) -> u32 {
        self.max_effect_tail_with_sources(&[])
    }

    /// Include preset chains that a streaming renderer processes before the graph.
    pub fn max_effect_tail_with_sources(&self, source_tails: &[u32]) -> u32 {
        let mut downstream_tails = HashMap::new();
        for name in self.bus_order.iter().rev() {
            let downstream = self
                .bus_chains
                .get(name)
                .map(|bus| {
                    effective_bus_routes(bus)
                        .into_iter()
                        .filter_map(|target| {
                            self.bus_chains.get(&target).map(|node| (target, node))
                        })
                        .map(|(target, node)| {
                            node.chain
                                .tail_samples()
                                .saturating_add(downstream_tails.get(&target).copied().unwrap_or(0))
                        })
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            downstream_tails.insert(name.clone(), downstream);
        }
        let pre_master = self
            .routing
            .iter()
            .enumerate()
            .fold(0_u32, |maximum, (index, route)| {
                let track_tail = self
                    .track_chains
                    .get(index)
                    .map(EffectChain::tail_samples)
                    .unwrap_or(0)
                    .saturating_add(source_tails.get(index).copied().unwrap_or(0));
                let send_tail = route.sends.iter().fold(track_tail, |send_max, send| {
                    let bus = &send.bus;
                    let bus_tail = self.bus_chains.get(bus).map_or(0, |node| {
                        node.chain
                            .tail_samples()
                            .saturating_add(downstream_tails.get(bus).copied().unwrap_or(0))
                    });
                    send_max.max(track_tail.saturating_add(bus_tail))
                });
                // Aux outputs bypass track effects and only incur bus tails.
                let aux_tail = route.aux_sends.iter().fold(send_tail, |aux_max, send| {
                    let bus_tail = self.bus_chains.get(&send.bus).map_or(0, |node| {
                        node.chain
                            .tail_samples()
                            .saturating_add(downstream_tails.get(&send.bus).copied().unwrap_or(0))
                    });
                    aux_max.max(bus_tail)
                });
                maximum.max(aux_tail)
            });
        pre_master.saturating_add(self.master_chain.tail_samples())
    }

    /// Mix per-track audio buffers through the graph → final stereo output.
    ///
    /// `track_buffers`: one stereo interleaved buffer per track (SoundFont or synth).
    /// Returns: final mixed stereo buffer.
    pub fn mix(&mut self, track_buffers: &mut [Vec<f32>]) -> Vec<f32> {
        self.mix_with_transport(track_buffers, None)
    }

    /// Enable or disable the streaming idle gate for every hosted plugin in
    /// this graph. Only the streaming entry point enables it.
    pub fn set_streaming_idle_gate(&mut self, enabled: bool) {
        for chain in self.track_chains.iter_mut() {
            chain.set_streaming_idle_gate(enabled);
        }
        for bus in self.bus_chains.values_mut() {
            bus.chain.set_streaming_idle_gate(enabled);
        }
        self.master_chain.set_streaming_idle_gate(enabled);
    }

    /// Process a timeline-positioned chunk with the same PDC and graph ordering
    /// as offline mixing. Streaming callers must supply their absolute cursor.
    ///
    /// This entry serves offline renders: the streaming idle gate stays off.
    pub fn mix_with_transport(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        self.set_streaming_idle_gate(false);
        self.mix_chunk_with_aux(track_buffers, transport, &HashMap::new())
    }

    /// Mix with pre-FX plugin auxiliary output buffers keyed by
    /// `(hosting track, stereo output pair)`.
    ///
    /// This is the streaming entry point: only here does a hosted plugin in a
    /// silent, inputless lane stop processing.
    pub fn mix_with_transport_and_aux(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        aux_buffers: &HashMap<(usize, usize), Vec<f32>>,
    ) -> Vec<f32> {
        self.set_streaming_idle_gate(true);
        self.mix_chunk_with_aux(track_buffers, transport, aux_buffers)
    }

    fn mix_chunk_with_aux(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        aux_buffers: &HashMap<(usize, usize), Vec<f32>>,
    ) -> Vec<f32> {
        if track_buffers.is_empty() {
            return Vec::new();
        }

        let buf_len = track_buffers.iter().map(|b| b.len()).max().unwrap_or(0);
        if buf_len == 0 {
            return Vec::new();
        }

        self.refresh_latency_compensation();

        self.process_track_effects(track_buffers, transport.clone());
        self.mix_processed_with_stems_and_aux(track_buffers, transport, None, aux_buffers)
    }

    /// Route buffers whose track effects have already been processed. This is
    /// shared by cached/offline and streaming renderers so PDC is identical.
    pub fn mix_processed(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        self.mix_processed_with_stems(track_buffers, transport, None)
    }

    pub fn mix_processed_with_stems(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        stems: Option<&mut StemCapture>,
    ) -> Vec<f32> {
        self.mix_processed_with_stems_and_aux(track_buffers, transport, stems, &HashMap::new())
    }

    pub fn mix_processed_with_stems_and_aux(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        stems: Option<&mut StemCapture>,
        aux_buffers: &HashMap<(usize, usize), Vec<f32>>,
    ) -> Vec<f32> {
        self.mix_processed_with_workers(track_buffers, transport, stems, aux_buffers, 1)
    }

    /// Offline variant: independent bus chains run on up to `bus_workers`
    /// threads; the fan-in into master keeps the declared bus order.
    pub fn mix_processed_with_workers(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        transport: Option<PluginTransport>,
        stems: Option<&mut StemCapture>,
        aux_buffers: &HashMap<(usize, usize), Vec<f32>>,
        bus_workers: usize,
    ) -> Vec<f32> {
        let samples = track_buffers.iter().map(Vec::len).max().unwrap_or(0);
        if samples == 0 {
            return Vec::new();
        }
        self.ensure_prepared(track_buffers, aux_buffers, samples);
        let mut prepared = self.prepared.take().unwrap();
        self.mix_prepared(
            track_buffers,
            PreparedMixOptions {
                transport: transport.as_ref(),
                stems,
                aux_buffers,
                bus_workers,
                samples,
            },
            &mut prepared,
        );
        let output = prepared.master.clone();
        self.prepared = Some(prepared);
        output
    }

    fn mix_prepared(
        &mut self,
        track_buffers: &mut [Vec<f32>],
        options: PreparedMixOptions<'_>,
        prepared: &mut PreparedGraph,
    ) {
        let PreparedMixOptions {
            transport,
            mut stems,
            aux_buffers,
            bus_workers,
            samples: buf_len,
        } = options;
        if let Some(capture) = stems.as_deref_mut() {
            let capture_master_inputs = capture.capture_master_inputs;
            let pre_fader_tracks = capture.pre_fader_tracks;
            *capture = StemCapture {
                capture_master_inputs,
                pre_fader_tracks,
                ..StemCapture::default()
            };
        }
        // The buffers here are already post-track-FX, so this measures the
        // width that actually reaches the pan law.
        self.detect_track_widths(track_buffers);

        // Every gain change written since the last chunk travels over this many
        // frames, starting here.
        let ramp_frames = gain_ramp::ramp_frames(self.sample_rate);

        // Read every control source before any track is routed. Readings come
        // from the same block as the audio, so nothing is one block late.
        self.update_control_sources(track_buffers, buf_len);

        // Initialize bus buffers
        let send_start = transport
            .as_ref()
            .map_or(self.send_cursor, |t| t.sample_pos);
        self.send_cursor = send_start.saturating_add((buf_len / 2) as u64);
        for bus in self.bus_chains.values_mut() {
            bus.buffer.clear();
            bus.buffer.resize(buf_len, 0.0);
        }

        prepared.master.clear();
        prepared.master.resize(buf_len, 0.0);
        prepared.taps.clear();
        self.prepared_controls(prepared, buf_len / 2);
        let mut master_buf = std::mem::take(&mut prepared.master);

        // Process each track
        let track_mix_stage = crate::stage_log::stage("track routing and sends");
        for (i, track_buf) in track_buffers.iter_mut().enumerate() {
            if i >= self.routing.len() {
                break;
            }
            let route = &self.routing[i];

            // A key tap is post-insert, pre-fader, pre-mute: the chain output,
            // taken before polarity, routing, the fader and the mute gate.
            if let Some(name) = self
                .track_names
                .get(i)
                .filter(|name| prepared.taps.has(name))
            {
                prepared.taps.set(name, track_buf);
            }

            // A subset pass leaves unselected tracks empty. They contribute
            // nothing, so skip the gate, fader and send stages entirely;
            // stem capture still reports the track as silence.
            if track_buf.is_empty() {
                if let Some(capture) = stems.as_deref_mut() {
                    capture
                        .tracks
                        .push((self.track_names[i].clone(), vec![0.0; buf_len]));
                }
                continue;
            }

            // A control-feed track was processed for its follower only.
            if self.control_only.get(i).copied().unwrap_or(false) {
                if let Some(capture) = stems.as_deref_mut() {
                    capture
                        .tracks
                        .push((self.track_names[i].clone(), vec![0.0; buf_len]));
                }
                continue;
            }

            // Mute and solo are a gain step, so they move on the same ramp as
            // the fader. A channel that has finished fading out is skipped
            // instead of ramping its silence into the next block.
            let mut ramps = self.ramps.tracks.get_mut(i);
            let silent = match ramps.as_deref_mut() {
                Some(ramps) => ramps.gate_block(!route.mute, track_buf, ramp_frames),
                None => route.mute,
            };
            if silent {
                if let Some(capture) = stems.as_deref_mut() {
                    capture
                        .tracks
                        .push((self.track_names[i].clone(), vec![0.0; buf_len]));
                }
                track_buf.fill(0.0);
                self.advance_silent_routes(i, track_buf.len(), buf_len, aux_buffers);
                continue;
            }

            // Offline device analysis reads the chain output before polarity,
            // routing, the fader and the pan law move it.
            if let Some(capture) = stems.as_deref_mut()
                && capture.pre_fader_tracks
            {
                capture
                    .tracks
                    .push((self.track_names[i].clone(), track_buf.clone()));
            }

            let width = self.track_widths.get(i).copied().unwrap_or_default();
            let track_latency = self
                .source_latencies
                .get(i)
                .copied()
                .unwrap_or(0)
                .saturating_add(
                    self.track_chains
                        .get(i)
                        .map_or(0, EffectChain::latency_samples),
                );

            // Polarity is a channel-input stage, so both send taps inherit it.
            if route.polarity_inverted {
                simd::negate(track_buf);
            }

            // Pre-fader sends tap post-insert, before the fader and pan.
            for (send_index, send) in route.sends.iter().enumerate() {
                if !send.pre_fader {
                    continue;
                }
                let Some(bus) = self.bus_chains.get_mut(&send.bus) else {
                    continue;
                };
                let delay = self.send_delays.get_mut(&(i, send_index));
                let envelope = if self.performance_sends.is_empty() {
                    None
                } else {
                    self.performance_sends
                        .get(&prepared.tracks[i].send_keys[send_index])
                };
                let ramp = ramps
                    .as_deref_mut()
                    .and_then(|ramps| ramps.send(send_index, send.amount as f32, ramp_frames));
                route_send(
                    track_buf,
                    send.amount as f32,
                    ramp,
                    send_pan_gains(send.pan, width),
                    send_start,
                    track_latency,
                    envelope,
                    delay,
                    &mut prepared.send_buffer,
                    &mut bus.buffer,
                );
            }

            // Apply volume and pan
            match ramps.as_deref_mut() {
                Some(ramps) => ramps.fader_block(
                    fader_gains(route.volume, route.pan, width),
                    track_buf,
                    ramp_frames,
                ),
                None => apply_volume_pan(track_buf, route.volume, route.pan, width),
            }
            if let Some(ride) = self.control_gains.get(i).and_then(Option::as_ref) {
                let control = self
                    .control_runtimes
                    .get(ride.source)
                    .map_or(&[][..], |runtime| runtime.values(buf_len / 2));
                ride.apply(track_buf, control);
            }
            if let Some(capture) = stems.as_deref_mut()
                && !capture.pre_fader_tracks
            {
                capture
                    .tracks
                    .push((self.track_names[i].clone(), track_buf.clone()));
            }

            // Post-fader sends tap after the fader and pan.
            for (send_index, send) in route.sends.iter().enumerate() {
                if send.pre_fader {
                    continue;
                }
                let Some(bus) = self.bus_chains.get_mut(&send.bus) else {
                    continue;
                };
                let delay = self.send_delays.get_mut(&(i, send_index));
                let envelope = if self.performance_sends.is_empty() {
                    None
                } else {
                    self.performance_sends
                        .get(&prepared.tracks[i].send_keys[send_index])
                };
                let ramp = ramps
                    .as_deref_mut()
                    .and_then(|ramps| ramps.send(send_index, send.amount as f32, ramp_frames));
                route_send(
                    track_buf,
                    send.amount as f32,
                    ramp,
                    send_pan_gains(send.pan, width),
                    send_start,
                    track_latency,
                    envelope,
                    delay,
                    &mut prepared.send_buffer,
                    &mut bus.buffer,
                );
            }

            // Route pre-FX plugin auxiliary outputs. These bypass the track
            // chain and fader, matching a multi-output instrument's own mixer.
            for (send_index, send) in route.aux_sends.iter().enumerate() {
                let Some(aux_buf) = aux_buffers.get(&(i, send.pair)) else {
                    continue;
                };
                let Some(bus) = self.bus_chains.get_mut(&send.bus) else {
                    continue;
                };
                let frames = aux_buf.len().min(bus.buffer.len());
                let (gain_l, gain_r) = send_pan_gains(send.pan, SourceWidth::Stereo);
                let gain_l = gain_l * send.amount as f32;
                let gain_r = gain_r * send.amount as f32;
                if let Some(delay) = self.aux_send_delays.get_mut(&(i, send_index)) {
                    delay.mix_into_panned(
                        &aux_buf[..frames],
                        &mut bus.buffer[..frames],
                        gain_l,
                        gain_r,
                    );
                } else {
                    simd::accumulate_channel_gains(
                        &mut bus.buffer[..frames],
                        &aux_buf[..frames],
                        gain_l,
                        gain_r,
                    );
                }
            }

            // Direct to master (standard aux-send model: track goes to master at full level,
            // bus effects are 100% wet returns that are summed separately below)
            if route.direct_to_master {
                if let Some(delay) = self.direct_delays.get_mut(i) {
                    delay.process(track_buf);
                }
                if let Some(capture) = stems
                    .as_deref_mut()
                    .filter(|capture| capture.capture_master_inputs)
                {
                    capture
                        .master_inputs
                        .push((format!("track:{}", self.track_names[i]), track_buf.clone()));
                }
                let min_len = track_buf.len().min(master_buf.len());
                simd::accumulate_channel_gains(
                    &mut master_buf[..min_len],
                    &track_buf[..min_len],
                    1.0,
                    1.0,
                );
            }
        }

        drop(track_mix_stage);
        // Process buses source-before-target and fan each return into its
        // declared outgoing buses and/or the master output.
        let bus_stage = crate::stage_log::stage("bus chains");
        if self.buses_can_run_in_parallel(bus_workers) {
            let workers = bus_workers.min(self.bus_order.len()).max(1);
            let mut sources: Vec<(String, BusNode)> = self
                .bus_order
                .iter()
                .filter_map(|name| self.bus_chains.remove(name).map(|bus| (name.clone(), bus)))
                .collect();
            let group = sources.len().div_ceil(workers).max(1);
            let sample_rate = self.sample_rate;
            std::thread::scope(|scope| {
                // Empty Vec does not allocate. Preserve the existing implicit
                // scope joins outside the explicit CLI offline control scope.
                let service_main = offline_main_is_active();
                let mut main_owned_handles = Vec::new();
                for chunk in sources.chunks_mut(group) {
                    let sidechain_taps = &prepared.taps;
                    let controls = &prepared.controls;
                    let handle = scope.spawn(move || {
                        for (_, bus) in chunk.iter_mut() {
                            let _chain_stage = crate::stage_log::stage("bus chain");
                            bus.chain.process_with_lookup(
                                &mut bus.buffer,
                                sidechain_taps,
                                controls,
                                sample_rate,
                                transport,
                            );
                            bus.chain.end_processing();
                        }
                    });
                    if service_main {
                        main_owned_handles.push(handle);
                    }
                }
                let mut failed = None;
                for handle in main_owned_handles {
                    while !handle.is_finished() {
                        if let Err(error) = service_offline_main() {
                            log::warn!("CLAP bus main service: {error}");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    if let Err(error) = handle.join() {
                        failed.get_or_insert(error);
                    }
                }
                if let Some(error) = failed {
                    std::panic::resume_unwind(error);
                }
            });
            // Sum the returns in declaration order so the master mix keeps the
            // same rounding as the serial path.
            let fan_in_stage = crate::stage_log::stage("bus fan-in");
            for (plan, (source_name, mut source)) in prepared.buses.iter().zip(sources) {
                if prepared.taps.has(&source_name)
                    && !self.track_names.iter().any(|name| name == &source_name)
                {
                    prepared.taps.set(&source_name, &source.buffer);
                }
                let live = Self::bus_gain_block(
                    self.ramps.buses.get_mut(&source_name),
                    &mut source,
                    ramp_frames,
                );
                if let Some(capture) = stems.as_deref_mut() {
                    capture
                        .buses
                        .push((source_name.clone(), source.buffer.clone()));
                }
                if live {
                    self.route_prepared_bus(
                        plan,
                        &source.buffer,
                        &mut prepared.routed,
                        &mut master_buf,
                        &mut stems,
                    );
                }
                self.bus_chains.insert(source_name, source);
            }
            drop(fan_in_stage);
        } else {
            for plan in &prepared.buses {
                let Some(source) = self.bus_chains.get_mut(&plan.name) else {
                    continue;
                };
                if matches!(&self.spectrum_target, Some(SpectrumTarget::Bus(name)) if name == &plan.name)
                    && let Some(analyser) = &mut self.spectrum
                {
                    analyser.push_interleaved(&source.buffer);
                }
                source.chain.process_with_lookup(
                    &mut source.buffer,
                    &prepared.taps,
                    &prepared.controls,
                    self.sample_rate,
                    transport,
                );
                if prepared.taps.has(&plan.name)
                    && !self.track_names.iter().any(|name| name == &plan.name)
                {
                    prepared.taps.set(&plan.name, &source.buffer);
                }
                let live =
                    Self::bus_gain_block(self.ramps.buses.get_mut(&plan.name), source, ramp_frames);
                if let Some(capture) = stems.as_deref_mut() {
                    capture
                        .buses
                        .push((plan.name.clone(), source.buffer.clone()));
                }
                if live {
                    prepared.route_source.clear();
                    prepared.route_source.extend_from_slice(&source.buffer);
                    self.route_prepared_bus(
                        plan,
                        &prepared.route_source,
                        &mut prepared.routed,
                        &mut master_buf,
                        &mut stems,
                    );
                }
            }
        }

        drop(bus_stage);
        // Apply master effects
        let master_stage = crate::stage_log::stage("master chain");
        if let Some(capture) = stems.filter(|capture| capture.capture_master_inputs) {
            capture.pre_master = master_buf.clone();
        }
        if self.spectrum_target.as_ref() == Some(&SpectrumTarget::Master)
            && let Some(analyser) = &mut self.spectrum
        {
            analyser.push_interleaved(&master_buf);
        }
        self.master_chain.process_with_lookup(
            &mut master_buf,
            &prepared.taps,
            &prepared.controls,
            self.sample_rate,
            transport,
        );
        let target = self.master_gain;
        self.ramps.master.set_target(target, ramp_frames);
        if self.ramps.master.is_settled() {
            if target != 1.0 {
                simd::apply_uniform_gain(&mut master_buf, target);
            }
        } else {
            self.ramps.master.apply(&mut master_buf);
        }
        drop(master_stage);

        prepared.master = master_buf;
    }

    /// Apply a bus return fader and report whether the return still reaches its
    /// routes this block. A mute ramps to silence and keeps routing the fading
    /// tail; once it has arrived the buffer is zeroed and dropped.
    fn bus_gain_block(ramp: Option<&mut GainRamp>, bus: &mut BusNode, frames: u32) -> bool {
        let Some(ramp) = ramp else {
            if !bus.audible {
                bus.buffer.fill(0.0);
                return false;
            }
            if bus.gain != 1.0 {
                simd::apply_uniform_gain(&mut bus.buffer, bus.gain);
            }
            return true;
        };
        ramp.set_target(if bus.audible { bus.gain } else { 0.0 }, frames);
        if ramp.is_settled() {
            if !bus.audible {
                bus.buffer.fill(0.0);
                return false;
            }
            if bus.gain != 1.0 {
                simd::apply_uniform_gain(&mut bus.buffer, bus.gain);
            }
            return true;
        }
        ramp.apply(&mut bus.buffer);
        true
    }

    /// Buses can chain concurrently only when each one returns straight to
    /// master and none taps another bus, so the ordered fan-in stays exact.
    fn buses_can_run_in_parallel(&self, bus_workers: usize) -> bool {
        bus_workers > 1
            && self.bus_order.len() > 1
            && self.spectrum.is_none()
            && self.bus_order.iter().all(|name| {
                self.bus_chains.get(name).is_some_and(|bus| {
                    effective_bus_routes(bus)
                        .iter()
                        .all(|target| target == "master")
                        && bus
                            .chain
                            .sidechain_sources()
                            .all(|source| !self.bus_chains.contains_key(source))
                })
            })
    }

    pub fn reset(&mut self) {
        for chain in &mut self.track_chains {
            chain.reset();
        }
        for bus in self.bus_chains.values_mut() {
            bus.chain.reset();
        }
        self.master_chain.reset();
        for runtime in &mut self.control_runtimes {
            runtime.reset();
        }
        self.ramps.snap();
        self.send_cursor = 0;
        if let Some(analyser) = &mut self.spectrum {
            analyser.reset();
        }
        for delay in self
            .direct_delays
            .iter_mut()
            .chain(self.send_delays.values_mut())
            .chain(self.aux_send_delays.values_mut())
            .chain(self.bus_route_delays.values_mut())
        {
            delay.buffer.fill(0.0);
            delay.position = 0;
        }
    }
}

/// Refuse a routing graph where bus routes close a cycle. A bounded pass in
/// the renderer keeps legacy callers from hanging, but checked builds must
/// report the cycle instead of silently folding it into the plan.
fn reject_bus_routing_cycles(project: &Project) -> Result<(), String> {
    let adjacent: HashMap<&str, Vec<&str>> = project
        .buses
        .iter()
        .map(|bus| {
            (
                bus.name.as_str(),
                bus.routes
                    .iter()
                    .map(String::as_str)
                    .filter(|target| *target != "master")
                    .collect(),
            )
        })
        .collect();
    let mut indegree: HashMap<&str, usize> = adjacent.keys().map(|name| (*name, 0)).collect();
    for targets in adjacent.values() {
        for target in targets {
            if let Some(degree) = indegree.get_mut(target) {
                *degree += 1;
            }
        }
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut ordered = 0;
    while let Some(name) = ready.pop() {
        ordered += 1;
        if let Some(targets) = adjacent.get(name) {
            for target in targets {
                if let Some(degree) = indegree.get_mut(target) {
                    *degree -= 1;
                    if *degree == 0 {
                        ready.push(target);
                    }
                }
            }
        }
    }
    if ordered == adjacent.len() {
        return Ok(());
    }
    let mut cyclic: Vec<&str> = indegree
        .into_iter()
        .filter(|(_, degree)| *degree > 0)
        .map(|(name, _)| name)
        .collect();
    cyclic.sort_unstable();
    Err(format!("bus routing cycle: {}", cyclic.join(" -> ")))
}

/// A key that no tap can satisfy keeps the device running without a key. Static
/// validation rejects keys written on a track or a bus; a key declared inside a
/// preset only learns its host here, so this is the diagnostic for it.
fn warn_unresolved_keys(
    track_names: &[String],
    track_chains: &[EffectChain],
    bus_chains: &HashMap<String, BusNode>,
    master_chain: &EffectChain,
) {
    let is_track = |name: &str| track_names.iter().any(|track| track == name);
    let is_bus = |name: &str| bus_chains.contains_key(name);
    let report = |owner: &str, chain: &EffectChain, on_track: bool| {
        for source in chain.sidechain_sources() {
            let warning = if is_track(source) && is_bus(source) {
                Some(format!(
                    "{owner}: key '{source}' names both a track and a bus; the track tap is used"
                ))
            } else if is_track(source) {
                None
            } else if on_track && is_bus(source) {
                Some(format!(
                    "{owner}: key '{source}' is a bus; a track effect can only key a track, so the effect runs without a key"
                ))
            } else if !is_bus(source) {
                Some(format!(
                    "{owner}: key '{source}' resolves to no track or bus; the effect runs without a key"
                ))
            } else {
                None
            };
            if let Some(warning) = warning {
                crate::render_warnings::push(warning);
            }
        }
    };
    for (index, chain) in track_chains.iter().enumerate() {
        let Some(name) = track_names.get(index) else {
            continue;
        };
        report(&format!("track '{name}'"), chain, true);
    }
    for (name, bus) in bus_chains {
        report(&format!("bus '{name}'"), &bus.chain, false);
    }
    report("master", master_chain, false);
}

/// Refuse a cycle over the union of audio routes and key edges. Without this the
/// plan tolerates a key cycle by declaration order and reads a stale key.
fn reject_signal_cycles(
    track_names: &[String],
    track_chains: &[EffectChain],
    bus_chains: &HashMap<String, BusNode>,
    routing: &[TrackRouting],
) -> Result<(), String> {
    for (index, chain) in track_chains.iter().enumerate() {
        let Some(name) = track_names.get(index) else {
            continue;
        };
        if chain.sidechain_sources().any(|source| source == name) {
            return Err(format!(
                "track '{name}' keys itself; a post-insert key cannot read its own chain"
            ));
        }
    }
    let mut bus_names: Vec<&str> = bus_chains.keys().map(String::as_str).collect();
    bus_names.sort_unstable();
    let mut nodes: Vec<(String, String)> = track_names
        .iter()
        .map(|name| (name.clone(), format!("track '{name}'")))
        .collect();
    nodes.extend(
        bus_names
            .iter()
            .map(|name| ((*name).to_string(), format!("bus '{name}'"))),
    );
    for name in &bus_names {
        if bus_chains[*name]
            .chain
            .sidechain_sources()
            .any(|source| source == *name)
        {
            return Err(format!(
                "bus '{name}' keys itself; a post-insert key cannot read its own chain"
            ));
        }
    }
    let node_of = |name: &str| nodes.iter().position(|(node, _)| node == name);
    let bus_offset = track_names.len();
    let bus_node = |name: &str| node_of(name).filter(|index| *index >= bus_offset);
    let mut edges = vec![Vec::new(); nodes.len()];
    for (index, route) in routing.iter().enumerate() {
        for send in &route.sends {
            if let Some(target) = bus_node(&send.bus) {
                edges[index].push(target);
            }
        }
    }
    for (offset, name) in bus_names.iter().enumerate() {
        let Some(bus) = bus_chains.get(*name) else {
            continue;
        };
        for target in effective_bus_routes(bus) {
            if let Some(target) = bus_node(&target) {
                edges[bus_offset + offset].push(target);
            }
        }
    }
    let mut add_key = |owner: usize, chain: &EffectChain, tracks_only: bool| {
        for source in chain.sidechain_sources() {
            let Some(source) = node_of(source) else {
                continue;
            };
            if tracks_only && source >= bus_offset {
                continue;
            }
            edges[source].push(owner);
        }
    };
    for (index, chain) in track_chains.iter().enumerate() {
        add_key(index, chain, true);
    }
    for (offset, name) in bus_names.iter().enumerate() {
        if let Some(bus) = bus_chains.get(*name) {
            add_key(bus_offset + offset, &bus.chain, false);
        }
    }
    if let Some(cycle) = find_cycle(&edges) {
        let members: Vec<&str> = cycle.iter().map(|index| nodes[*index].1.as_str()).collect();
        return Err(format!("key cycle: {}", members.join(" -> ")));
    }
    Ok(())
}

/// Nodes that survive peeling every zero-indegree node: each is on a cycle or
/// downstream of one.
fn cyclic_nodes(edges: &[Vec<usize>]) -> Vec<bool> {
    let mut indegree = vec![0_usize; edges.len()];
    for targets in edges {
        for &target in targets {
            indegree[target] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..edges.len())
        .filter(|index| indegree[*index] == 0)
        .collect();
    while let Some(node) = ready.pop() {
        for &next in &edges[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }
    indegree.into_iter().map(|degree| degree > 0).collect()
}

/// One cycle inside the residual graph, closed on its first node. Every node on
/// a cycle keeps an edge into the residual, so a walk reaches one in O(n).
fn cycle_through(start: usize, edges: &[Vec<usize>], cyclic: &[bool]) -> Option<Vec<usize>> {
    let mut path = vec![start];
    let mut visited = vec![usize::MAX; edges.len()];
    visited[start] = 0;
    let mut node = start;
    loop {
        let next = *edges[node].iter().find(|next| cyclic[**next])?;
        if visited[next] != usize::MAX {
            let mut cycle = path[visited[next]..].to_vec();
            cycle.push(next);
            return Some(cycle);
        }
        visited[next] = path.len();
        path.push(next);
        node = next;
    }
}

fn find_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    let cyclic = cyclic_nodes(edges);
    for start in (0..cyclic.len()).filter(|index| cyclic[*index]) {
        if let Some(cycle) = cycle_through(start, edges, &cyclic) {
            return Some(cycle);
        }
    }
    None
}

/// Declared sends plus the non-master main output, which is post-fader.
fn track_send_routes(track: &oc_dsl::ast::Track) -> (Vec<SendRoute>, bool) {
    let mut sends: Vec<SendRoute> = Vec::new();
    for send in track.mix.sends.iter().filter(|send| send.source.is_none()) {
        let route = SendRoute {
            bus: send.target.clone(),
            amount: send.amount,
            pre_fader: send.pre_fader,
            pan: send.pan,
        };
        if let Some(existing) = sends.iter_mut().find(|existing| {
            existing.bus == route.bus
                && existing.pre_fader == route.pre_fader
                && existing.pan.to_bits() == route.pan.to_bits()
        }) {
            existing.amount += route.amount;
        } else {
            sends.push(route);
        }
    }
    let direct_to_master = track.mix.output.as_deref().unwrap_or("master") == "master";
    if let Some(output) = track.mix.output.as_ref().filter(|_| !direct_to_master) {
        let merged = sends
            .iter_mut()
            .find(|route| route.bus == *output && !route.pre_fader && route.pan == 0.0);
        if let Some(route) = merged {
            route.amount += 1.0;
        } else {
            sends.push(SendRoute {
                bus: output.clone(),
                amount: 1.0,
                pre_fader: false,
                pan: 0.0,
            });
        }
    }
    (sends, direct_to_master)
}

/// Constant-power pan gains normalised to unity at centre.
pub(crate) fn equal_power_pan_gains(pan: f64) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan == 0.0 {
        return (1.0, 1.0);
    }
    let angle = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
    (
        (std::f64::consts::SQRT_2 * angle.cos()) as f32,
        (std::f64::consts::SQRT_2 * angle.sin()) as f32,
    )
}

/// Gains for a send balance; centre is exactly unity for every width.
/// Mono is constant power normalised to centre, stereo is the balance law.
fn send_pan_gains(pan: f64, width: SourceWidth) -> (f32, f32) {
    match width {
        SourceWidth::Mono => equal_power_pan_gains(pan),
        SourceWidth::Stereo => {
            let pan = pan.clamp(-1.0, 1.0);
            if pan == 0.0 {
                return (1.0, 1.0);
            }
            (
                (1.0 - pan).clamp(0.0, 1.0) as f32,
                (1.0 + pan).clamp(0.0, 1.0) as f32,
            )
        }
    }
}

/// Sum one send tap into its bus, applying the send pan, performance envelope
/// and PDC. The pan is applied per channel without allocating.
// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
fn route_send(
    track_buf: &[f32],
    amount: f32,
    ramp: Option<&mut GainRamp>,
    pan: (f32, f32),
    send_start: u64,
    latency: u32,
    envelope: Option<&crate::performance_send::SendEnvelope>,
    delay: Option<&mut StereoDelayCompensator>,
    send_buffer: &mut Vec<f32>,
    bus_buffer: &mut [f32],
) {
    // A route without ramp state still applies its written amount.
    let mut settled;
    let ramp = match ramp {
        Some(ramp) => ramp,
        None => {
            settled = GainRamp::settled(amount);
            &mut settled
        }
    };
    let frames = track_buf.len().min(bus_buffer.len());
    let target = &mut bus_buffer[..frames];
    if let Some(envelope) = envelope {
        send_buffer.clear();
        send_buffer.extend_from_slice(&track_buf[..frames]);
        envelope
            .apply(send_buffer, send_start, latency)
            .expect("valid stereo send timeline");
        mix_send(send_buffer, target, pan, ramp, delay);
    } else {
        mix_send(&track_buf[..frames], target, pan, ramp, delay);
    }
}

/// Sum one send tap at its current, possibly ramping, level.
fn mix_send(
    source: &[f32],
    target: &mut [f32],
    pan: (f32, f32),
    ramp: &mut GainRamp,
    delay: Option<&mut StereoDelayCompensator>,
) {
    if ramp.is_settled() {
        let gain = ramp.value();
        let (gain_l, gain_r) = (pan.0 * gain, pan.1 * gain);
        if let Some(delay) = delay {
            delay.mix_into_panned(source, target, gain_l, gain_r);
        } else {
            simd::accumulate_channel_gains(target, source, gain_l, gain_r);
        }
        return;
    }
    match delay {
        Some(delay) => delay.mix_into_panned_ramped(source, target, pan, ramp),
        None => {
            for (index, (target, source)) in target.iter_mut().zip(source).enumerate() {
                let gain = if index % 2 == 0 { pan.0 } else { pan.1 } * ramp.next();
                *target += source * gain;
            }
        }
    }
}

fn effective_bus_routes(bus: &BusNode) -> Vec<String> {
    if bus.routes.is_empty() {
        vec!["master".to_string()]
    } else {
        bus.routes.clone()
    }
}

fn sum_buffer(target: &mut [f32], source: &[f32], gain: f32) {
    simd::accumulate_channel_gains(target, source, gain, gain);
}

// ============================================================================
// Helpers
// ============================================================================

/// Stable seed for a seeded device from its authored location and slot.
///
/// The clock, a pointer and a run counter are not inputs: the same document
/// must render the same audio. Two devices in the same location and slot share
/// a seed; varying a single device needs the seed to become an explicit field.
fn device_seed(place: &str, index: usize) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in place
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain((index as u32).to_le_bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash.max(1)
}

/// What a chain patch could do with an edited document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainPatch {
    /// These chains were patched in place; every other chain kept its DSP state.
    Patched { chains: usize },
    /// The edit changed the graph's node set, so the project must be reloaded.
    Structural,
}

/// The document a streaming graph's chains are built from.
///
/// A preset's effects run on the pattern lanes, never on the mixer chain, so the
/// graph document carries only each track's own effects. A full streaming build
/// and an in-place chain patch both fold the presets away here, so they cannot
/// disagree about what a chain holds.
pub fn graph_document(project: &Project) -> Project {
    let mut document = project.clone();
    for preset in &mut document.presets {
        preset.effects.clear();
    }
    document
}

/// The parts of a chain owner that decide the chain's shape.
struct ChainShape<'a> {
    effects: &'a [oc_dsl::ast::Effect],
    routes: &'a [oc_dsl::ast::InputRoute],
    gain: &'a [oc_dsl::ast::GainAutomation],
    /// Only lanes that address a chain entry shape the chain; a mixer lane is
    /// read by the routing stage, not by the effect chain.
    lanes: Vec<&'a oc_dsl::ast::AutomationLane>,
}

fn effect_lanes<'a>(
    lanes: impl Iterator<Item = &'a oc_dsl::ast::AutomationLane>,
) -> Vec<&'a oc_dsl::ast::AutomationLane> {
    lanes
        .filter(|lane| oc_dsl::automation::effect_lane(&lane.param).is_some())
        .collect()
}

fn track_shape(track: &oc_dsl::ast::Track) -> ChainShape<'_> {
    ChainShape {
        effects: &track.effects,
        routes: &track.input_routes,
        gain: &track.gain_automation,
        lanes: effect_lanes(track.automation()),
    }
}

fn bus_shape(bus: &oc_dsl::ast::Bus) -> ChainShape<'_> {
    ChainShape {
        effects: &bus.effects,
        routes: &bus.input_routes,
        gain: &bus.gain_automation,
        lanes: effect_lanes(bus.automation.iter()),
    }
}

fn master_shape(master: &oc_dsl::ast::Master) -> ChainShape<'_> {
    ChainShape {
        effects: &master.effects,
        routes: &master.input_routes,
        gain: &master.gain_automation,
        lanes: effect_lanes(master.automation.iter()),
    }
}

/// What a chain patch does to one chain.
enum ChainPlan {
    /// Nothing the live chain needs to hear.
    Unchanged,
    /// The same devices; fade these `(position, parameter, before, now)` values.
    Fade(Vec<(usize, String, f64, f64)>),
    /// Different devices or inputs: the chain must be built again.
    Rebuild,
}

fn chain_plan(before: &ChainShape<'_>, after: &ChainShape<'_>) -> ChainPlan {
    let devices_changed = before.effects.len() != after.effects.len()
        || before
            .effects
            .iter()
            .zip(after.effects)
            .any(|(before, after)| {
                before.effect_type != after.effect_type
                    || before.bypass != after.bypass
                    || before.sidechain_source != after.sidechain_source
            });
    // A route, a gain clip or an automation lane changes the wrappers around
    // the devices, so those are rebuilt too.
    let inputs_changed =
        before.routes != after.routes || before.gain != after.gain || before.lanes != after.lanes;
    if devices_changed || inputs_changed {
        return ChainPlan::Rebuild;
    }
    match effect_parameter_changes(before.effects, after.effects) {
        Some(changes) if changes.is_empty() => ChainPlan::Unchanged,
        Some(changes) => ChainPlan::Fade(changes),
        None => ChainPlan::Rebuild,
    }
}

/// Parameters the edit changed, or `None` when a device cannot take a change
/// without being built again.
fn effect_parameter_changes(
    before: &[oc_dsl::ast::Effect],
    after: &[oc_dsl::ast::Effect],
) -> Option<Vec<(usize, String, f64, f64)>> {
    let mut changes = Vec::new();
    for (index, (before, after)) in before.iter().zip(after).enumerate() {
        for (name, value) in &after.params {
            match before.params.iter().find(|(key, _)| key == name) {
                Some((_, previous)) if previous == value => {}
                Some((_, previous)) => changes.push((
                    index,
                    name.clone(),
                    numeric_param(&before.effect_type, name, previous)?,
                    numeric_param(&after.effect_type, name, value)?,
                )),
                // A parameter the device gains fades from what it reports now,
                // which is the value it was built with.
                None => {
                    let goal = numeric_param(&after.effect_type, name, value)?;
                    changes.push((index, name.clone(), goal, goal));
                }
            }
        }
        // A dropped parameter has no live value to fall back to.
        if before
            .params
            .iter()
            .any(|(name, _)| !after.params.iter().any(|(key, _)| key == name))
        {
            return None;
        }
    }
    Some(changes)
}

/// One authored parameter in the device's own unit, or `None` when the value
/// has no number a live device could take.
fn numeric_param(
    effect_type: &oc_dsl::ast::EffectType,
    name: &str,
    value: &oc_dsl::ast::ParamValue,
) -> Option<f64> {
    // A hosted plugin takes the plain number the document wrote.
    if matches!(effect_type, oc_dsl::ast::EffectType::Plugin(_)) {
        return value.as_f64();
    }
    dsp::typed_builtin_value(effect_type, name, value)
}

/// One track's graph chain.
///
/// A streaming build passes a document whose preset effects are folded away:
/// those belong to the pattern lanes, so the same project builds the same chain
/// here and when a single chain is replaced in place.
fn build_track_chain(
    project: &Project,
    index: usize,
    sample_rate: u32,
    checked: bool,
    touch: Option<&Arc<touch::TouchRegistry>>,
) -> Result<EffectChain, String> {
    let track = project
        .tracks
        .get(index)
        .ok_or_else(|| format!("Track {index} out of range"))?;
    let preset_effects = track
        .preset
        .as_ref()
        .and_then(|name| project.presets.iter().find(|p| &p.name == name))
        .map(|p| p.effects.clone())
        .unwrap_or_default();
    let place = format!("track '{}'", track.name);
    let mut chain = build_effect_chain(
        &preset_effects,
        &[],
        &[],
        EffectChainBuild {
            sample_rate,
            checked,
            properties: &project.properties,
            owner: "",
            place: &place,
            touch: None,
        },
    )?;
    chain.effects.extend(
        build_effect_chain(
            &track.effects,
            &track.input_routes,
            track.automation(),
            EffectChainBuild {
                sample_rate,
                checked,
                properties: &project.properties,
                owner: &format!("track:{index}"),
                place: &place,
                touch,
            },
        )?
        .effects,
    );
    if !track.gain_automation.is_empty() {
        chain
            .effects
            .push(Box::new(gain_automation::GainEnvelope::with_project_touch(
                &track.gain_automation,
                &project.properties,
                sample_rate,
                &format!("track:{index}"),
                touch.cloned(),
            )?));
    }
    Ok(chain)
}

/// One bus's graph chain.
fn build_bus_chain(
    project: &Project,
    index: usize,
    sample_rate: u32,
    checked: bool,
    touch: Option<&Arc<touch::TouchRegistry>>,
) -> Result<EffectChain, String> {
    let bus = project
        .buses
        .get(index)
        .ok_or_else(|| format!("Bus {index} out of range"))?;
    let owner = format!("bus:{}", bus.name);
    let mut chain = build_effect_chain(
        &bus.effects,
        &bus.input_routes,
        &bus.automation,
        EffectChainBuild {
            sample_rate,
            checked,
            properties: &project.properties,
            owner: &owner,
            place: &format!("bus '{}'", bus.name),
            touch,
        },
    )?;
    if !bus.gain_automation.is_empty() {
        chain
            .effects
            .push(Box::new(gain_automation::GainEnvelope::with_project_touch(
                &bus.gain_automation,
                &project.properties,
                sample_rate,
                &owner,
                touch.cloned(),
            )?));
    }
    Ok(chain)
}

/// The master graph chain; a document without a master section has none.
fn build_master_chain(
    project: &Project,
    sample_rate: u32,
    checked: bool,
    touch: Option<&Arc<touch::TouchRegistry>>,
) -> Result<EffectChain, String> {
    let master = project
        .master
        .as_ref()
        .ok_or_else(|| "Master not found".to_string())?;
    let mut chain = build_effect_chain(
        &master.effects,
        &master.input_routes,
        &master.automation,
        EffectChainBuild {
            sample_rate,
            checked,
            properties: &project.properties,
            owner: "master",
            place: "master",
            touch,
        },
    )?;
    if !master.gain_automation.is_empty() {
        chain
            .effects
            .push(Box::new(gain_automation::GainEnvelope::with_project_touch(
                &master.gain_automation,
                &project.properties,
                sample_rate,
                "master",
                touch.cloned(),
            )?));
    }
    Ok(chain)
}

fn build_effect_chain<'a>(
    effects: &[oc_dsl::ast::Effect],
    routes: &[oc_dsl::ast::InputRoute],
    automation: impl IntoIterator<Item = &'a oc_dsl::ast::AutomationLane>,
    context: EffectChainBuild<'_>,
) -> Result<EffectChain, String> {
    let EffectChainBuild {
        sample_rate,
        checked,
        properties,
        owner,
        place,
        touch,
    } = context;
    let lanes = effect_lanes_by_index(effects, automation, checked)?;
    let mut audio_routes: HashMap<usize, &str> = HashMap::new();
    let mut control_routes: HashMap<usize, &str> = HashMap::new();
    for route in routes {
        let index = match oc_dsl::automation::resolve_effect(effects, &route.target) {
            Ok(index) => index,
            Err(error) if checked => {
                return Err(format!("route into {owner}: {error}"));
            }
            Err(_) => continue,
        };
        match route.kind {
            oc_dsl::ast::RouteKind::Audio => {
                audio_routes.insert(index, route.source.as_str());
            }
            oc_dsl::ast::RouteKind::Control => {
                control_routes.insert(index, route.source.as_str());
            }
        }
    }
    let mut chain = EffectChain::new();
    for (index, effect) in effects.iter().enumerate() {
        if effect.bypass {
            // A bypassed entry is absent: no device, no latency, no tail. A
            // built-in still has its parameters checked so validate and render
            // agree; a plugin is never loaded.
            if checked && !matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_)) {
                dsp::try_create_effect_at(&effect.effect_type, &effect.params, sample_rate, place)
                    .map_err(|error| {
                        if place.is_empty() {
                            error
                        } else {
                            format!("{place}: {error}")
                        }
                    })?;
            }
            chain.effects.push(Box::new(Bypassed));
            continue;
        }
        let mut fx = if checked {
            dsp::try_create_effect_at(&effect.effect_type, &effect.params, sample_rate, place)
                .map_err(|error| {
                    // A plugin refusal already names the place it sits in.
                    let named = place.is_empty()
                        || matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_));
                    if named {
                        error
                    } else {
                        format!("{place}: {error}")
                    }
                })?
        } else {
            dsp::create_effect(&effect.effect_type, &effect.params, sample_rate)
        };
        // Seeded devices take their sequence from where they sit, not from the
        // clock or an address, so the same project renders the same audio.
        fx.set_seed(device_seed(place, index));
        // The gate must see whole render blocks, so it wraps the finished
        // entry: automation, key and control wrappers stay inside it.
        let idle_gate = fx
            .plugin_tail_samples()
            .map(|tail| idle_gate::IdleGate::new(sample_rate, Some(tail), !smart_disable(effect)));
        if let Some(lanes) = lanes.get(&index) {
            match effect_automation::compile(effect, lanes, properties, sample_rate, owner, index) {
                Ok(compiled) => {
                    fx = Box::new(effect_automation::AutomatedEffect::new(
                        fx,
                        compiled.lanes,
                        sample_rate,
                        touch.cloned(),
                        compiled.clock,
                    ))
                }
                Err(error) if checked => return Err(error),
                // Legacy infallible builds keep the effect without automation.
                Err(_) => {}
            }
        }
        if let Some(source) = &effect.sidechain_source {
            if checked && !fx.supports_sidechain() {
                return Err(format!(
                    "effect {} does not support sidechain input {source}; refusing ignored key",
                    fx.name()
                ));
            }
            fx = Box::new(SidechainEffect::new(fx, source.clone()));
        }
        if let Some(source) = audio_routes.get(&index) {
            if checked && effect.sidechain_source.is_some() {
                return Err(format!(
                    "effect {} has both `sidechain=` and an audio route; keep one",
                    fx.name()
                ));
            }
            if checked && control_routes.contains_key(&index) {
                return Err(format!(
                    "effect {} has both an audio key route and a control route; a device takes one",
                    fx.name()
                ));
            }
            if checked && !fx.supports_sidechain() {
                return Err(format!(
                    "effect {} does not support an audio key input; refusing route from '{source}'",
                    fx.name()
                ));
            }
            fx = Box::new(SidechainEffect::new(fx, (*source).to_string()));
        }
        if let Some(source) = control_routes.get(&index) {
            if checked && !fx.supports_control_input() {
                return Err(format!(
                    "effect {} does not accept a control input; refusing route from '{source}'",
                    fx.name()
                ));
            }
            fx = Box::new(ControlEffect::new(fx, (*source).to_string()));
        }
        if let Some(gate) = idle_gate {
            fx = Box::new(idle_gate::IdleGatedPlugin::new(fx, gate));
        }
        chain.effects.push(fx);
    }
    Ok(chain)
}

/// Reserved effect parameter that keeps a hosted plugin processing while its
/// lane is silent. The gate is enabled unless it is set to `false`.
fn smart_disable(effect: &oc_dsl::ast::Effect) -> bool {
    !effect.params.iter().any(|(name, value)| {
        name == idle_gate::SMART_DISABLE_PARAM && *value == oc_dsl::ast::ParamValue::Bool(false)
    })
}

/// Group effect-parameter lanes by the chain entry they address.
fn effect_lanes_by_index<'a>(
    effects: &[oc_dsl::ast::Effect],
    automation: impl IntoIterator<Item = &'a oc_dsl::ast::AutomationLane>,
    checked: bool,
) -> Result<HashMap<usize, Vec<&'a oc_dsl::ast::AutomationLane>>, String> {
    let mut lanes: HashMap<usize, Vec<&oc_dsl::ast::AutomationLane>> = HashMap::new();
    for lane in automation {
        let Some((target, _)) = oc_dsl::automation::effect_lane(&lane.param) else {
            continue;
        };
        match oc_dsl::automation::resolve_effect(effects, target) {
            Ok(index) => lanes.entry(index).or_default().push(lane),
            Err(error) if checked => {
                return Err(format!("automation lane '{}': {error}", lane.param));
            }
            Err(_) => {}
        }
    }
    Ok(lanes)
}

struct SidechainEffect {
    inner: Box<dyn AudioEffect>,
    source: String,
    main_delay: StereoDelayCompensator,
    key_delay: StereoDelayCompensator,
    main_delay_frames: u32,
    /// Reused aligned key buffer so the render path does not allocate.
    key_buffer: Vec<f32>,
}

impl SidechainEffect {
    fn new(inner: Box<dyn AudioEffect>, source: String) -> Self {
        Self {
            inner,
            source,
            main_delay: StereoDelayCompensator::new(0),
            key_delay: StereoDelayCompensator::new(0),
            main_delay_frames: 0,
            key_buffer: Vec::new(),
        }
    }
}

impl AudioEffect for SidechainEffect {
    fn processing_failed(&self) -> bool {
        self.inner.processing_failed()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        self.inner.take_processing_failure()
    }

    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.inner.prepare_block(max_frames)?;
        let samples = max_frames
            .checked_mul(2)
            .ok_or(oc_audio::BlockPreparationError::CapacityExceeded)?;
        if self.key_buffer.capacity() < samples {
            self.key_buffer
                .try_reserve(samples - self.key_buffer.len())
                .map_err(|_| oc_audio::BlockPreparationError::CapacityExceeded)?;
        }
        Ok(())
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        if let AudioProcessInput::Sidechain(sidechain) = context.input {
            self.main_delay.process(buffer);
            if self.key_buffer.len() != sidechain.len() {
                self.key_buffer.resize(sidechain.len(), 0.0);
            }
            self.key_buffer.copy_from_slice(sidechain);
            self.key_delay.process(&mut self.key_buffer);
            self.inner.process_with_context(
                buffer,
                AudioProcessContext {
                    input: AudioProcessInput::Sidechain(&self.key_buffer),
                    ..context
                },
            );
        } else {
            self.inner.process_with_context(buffer, context);
        }
    }
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.inner.process(buffer, sample_rate);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.inner
            .process_with_transport(buffer, sample_rate, transport);
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.main_delay.process(buffer);
        if self.key_buffer.len() != sidechain.len() {
            self.key_buffer.resize(sidechain.len(), 0.0);
        }
        self.key_buffer.copy_from_slice(sidechain);
        self.key_delay.process(&mut self.key_buffer);
        self.inner
            .process_with_sidechain(buffer, &self.key_buffer, sample_rate, transport);
    }
    fn supports_sidechain(&self) -> bool {
        self.inner.supports_sidechain()
    }
    fn sidechain_source(&self) -> Option<&str> {
        Some(&self.source)
    }
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.inner
            .process_with_control(buffer, control, sample_rate, transport);
    }
    fn supports_control_input(&self) -> bool {
        self.inner.supports_control_input()
    }
    fn control_source(&self) -> Option<&str> {
        self.inner.control_source()
    }
    fn configure_sidechain_alignment(&mut self, main_latency: u32, sidechain_latency: u32) -> u32 {
        self.main_delay_frames = sidechain_latency.saturating_sub(main_latency);
        self.main_delay = StereoDelayCompensator::new(self.main_delay_frames);
        self.key_delay =
            StereoDelayCompensator::new(main_latency.saturating_sub(sidechain_latency));
        self.main_delay_frames
    }
    fn set_param(&mut self, name: &str, value: f64) {
        self.inner.set_param(name, value);
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.inner.get_param(name)
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn latency_samples(&self) -> u32 {
        self.main_delay_frames
            .saturating_add(self.inner.latency_samples())
    }
    fn tail_samples(&self) -> u32 {
        self.inner.tail_samples()
    }
    fn bounce_tail_samples(&self) -> u32 {
        self.inner.bounce_tail_samples()
    }
    fn gain_reduction_db(&self) -> f64 {
        self.inner.gain_reduction_db()
    }
    fn band_count(&self) -> usize {
        self.inner.band_count()
    }
    fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.inner.band_gain_db(index)
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
}

/// Names a device's control input. The chain supplies the per-frame values;
/// this wrapper only records which `@control_source` feeds the device and
/// forwards everything else to it.
struct ControlEffect {
    inner: Box<dyn AudioEffect>,
    source: String,
}

impl ControlEffect {
    fn new(inner: Box<dyn AudioEffect>, source: String) -> Self {
        Self { inner, source }
    }
}

impl AudioEffect for ControlEffect {
    fn processing_failed(&self) -> bool {
        self.inner.processing_failed()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        self.inner.take_processing_failure()
    }

    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.inner.prepare_block(max_frames)
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        self.inner.process_with_context(buffer, context);
    }
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.inner.process(buffer, sample_rate);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.inner
            .process_with_transport(buffer, sample_rate, transport);
    }
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.inner
            .process_with_sidechain(buffer, sidechain, sample_rate, transport);
    }
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.inner
            .process_with_control(buffer, control, sample_rate, transport);
    }
    fn supports_sidechain(&self) -> bool {
        self.inner.supports_sidechain()
    }
    fn sidechain_source(&self) -> Option<&str> {
        self.inner.sidechain_source()
    }
    fn supports_control_input(&self) -> bool {
        self.inner.supports_control_input()
    }
    fn control_source(&self) -> Option<&str> {
        Some(&self.source)
    }
    fn configure_sidechain_alignment(&mut self, main_latency: u32, sidechain_latency: u32) -> u32 {
        self.inner
            .configure_sidechain_alignment(main_latency, sidechain_latency)
    }
    fn set_param(&mut self, name: &str, value: f64) {
        self.inner.set_param(name, value);
    }
    fn queue_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        self.inner.queue_param_at(name, value, sample_offset)
    }
    fn supports_param_offsets(&self) -> bool {
        self.inner.supports_param_offsets()
    }
    fn max_block_frames(&self) -> u32 {
        self.inner.max_block_frames()
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.inner.get_param(name)
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn end_processing(&mut self) {
        self.inner.end_processing();
    }
    fn latency_samples(&self) -> u32 {
        self.inner.latency_samples()
    }
    fn tail_samples(&self) -> u32 {
        self.inner.tail_samples()
    }
    fn bounce_tail_samples(&self) -> u32 {
        self.inner.bounce_tail_samples()
    }
    fn gain_reduction_db(&self) -> f64 {
        self.inner.gain_reduction_db()
    }
    fn band_count(&self) -> usize {
        self.inner.band_count()
    }
    fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.inner.band_gain_db(index)
    }
    fn max_gain_reduction_db(&self) -> f64 {
        self.inner.max_gain_reduction_db()
    }
    fn ceiling_margin_db(&self) -> Option<f64> {
        self.inner.ceiling_margin_db()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
}

/// Apply the track fader and pan law.
pub fn apply_volume_pan(buffer: &mut [f32], volume: f64, pan: f64, width: SourceWidth) {
    let (gain_l, gain_r) = fader_gains(volume, pan, width);
    let even = buffer.len() & !1;
    simd::channel_gains(&mut buffer[..even], gain_l, gain_r);
}

/// Per-channel gains of the track fader and pan law. Mono is constant power
/// (−3.01 dB/side); stereo is balance (unity at centre).
fn fader_gains(volume: f64, pan: f64, width: SourceWidth) -> (f32, f32) {
    let (gain_l, gain_r) = match width {
        SourceWidth::Mono => {
            let angle = (pan + 1.0) * std::f64::consts::FRAC_PI_4; // 0 to pi/2
            (volume * angle.cos(), volume * angle.sin())
        }
        SourceWidth::Stereo => (
            volume * (1.0 - pan).clamp(0.0, 1.0),
            volume * (1.0 + pan).clamp(0.0, 1.0),
        ),
    };
    (gain_l as f32, gain_r as f32)
}

fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

#[cfg(test)]
mod tests;

impl AudioGraph {
    pub fn processing_failed(&self) -> bool {
        self.track_chains.iter().any(EffectChain::processing_failed)
            || self.master_chain.processing_failed()
            || self
                .bus_chains
                .values()
                .any(|bus| bus.chain.processing_failed())
    }

    pub fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        let mut report = oc_audio::ProcessingFailureReport::Unavailable;
        for chain in self
            .track_chains
            .iter()
            .chain(std::iter::once(&self.master_chain))
            .chain(self.bus_chains.values().map(|bus| &bus.chain))
        {
            if chain.processing_failed() {
                report = report.merge(chain.take_processing_failure());
            }
            if matches!(report, oc_audio::ProcessingFailureReport::Ready(_)) {
                break;
            }
        }
        report
    }
}
