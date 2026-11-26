//! Per-block effect-parameter automation.
//!
//! Each lane is compiled to a point table and a curve per segment. Envelope
//! lanes use absolute sample positions; LFO lanes use cycle phases, but both
//! feed the same analytic evaluator, so there is one interpolation path. The
//! wrapper samples every lane in absolute time: a point keeps its exact sample,
//! a continuous curve moves the device value at every sample, and an optional
//! one-pole stage filters the target before it reaches the device. Absolute-time
//! sampling keeps the result independent of the render block size.
//!
//! A live touch ([`crate::touch`]) is an arbitration layer above the lane: while
//! the hand holds a parameter the lane stops writing and its one-pole is
//! suspended, so it cannot run ahead of the hand; on release a fixed linear
//! crossfade from the hand to the lane hands it back, and the lane's own
//! smoothing resumes from the state it stopped at.

use super::linked_automation::LinkedLane;
use super::live_controls::CcReading;
use super::macro_preview::MacroReading;
use crate::control_link::apply_midi_formula;
use crate::touch::{self, TouchLane, TouchPlan, TouchRegistry};
use oc_audio::{
    AudioEffect, AudioProcessContext, AudioProcessInput, PluginTransport, PreparedParameterHandle,
    PreparedParameterOutcome,
};
use oc_dsl::ast::{AutomationLane, Effect, LinkFormula, ProjectProperties};
use oc_dsl::automation::{LaneCurve, PreparedMapping, smooth_step};
use oc_dsl::timing::{ProjectClock, SampleTempo};
use oc_dsp::schema::DeviceSchema;
use oc_dsp::schema::effect_slot::SlotControl;
use std::sync::Arc;

mod mapped_bounds;
mod preparation;
mod slot_mix;
pub(super) use preparation::AutomationPreparationError;

/// Samples between parameter writes while a release fades back to the lane.
/// 8 samples at 48 kHz is 0.17 ms, so a 20 ms fade is 120 even steps.
const FADE_STEP: u64 = 8;

/// Longest span one automation pass evaluates at once.
///
/// A continuous lane writes one value per sample, so a timeline-sized block
/// would hold `lanes * frames` events at 24 bytes each. Splitting the caller's
/// buffer keeps that scratch list bounded without moving any value: a lane is
/// sampled at absolute positions, so the same samples are written either way.
const MAX_AUTOMATION_BLOCK_FRAMES: usize = 1024;

#[derive(Clone)]
pub(super) enum ParamInput {
    Timeline(Arc<LaneCurve>),
    MidiCc(Arc<CcReading>),
    PresetValue {
        saved: f64,
        preview: Option<Arc<MacroReading>>,
    },
}

impl ParamInput {
    fn timeline(&self) -> Option<&LaneCurve> {
        match self {
            Self::Timeline(curve) => Some(curve),
            Self::MidiCc(_) | Self::PresetValue { .. } => None,
        }
    }

    fn held_value(&self) -> Option<f64> {
        match self {
            Self::MidiCc(reading) => reading.value(),
            Self::PresetValue { saved, preview } => Some(
                preview
                    .as_deref()
                    .and_then(MacroReading::read)
                    .unwrap_or(*saved),
            ),
            Self::Timeline(_) => None,
        }
    }
}

/// One parameter input with a sample cursor, smoother, and touch arbitration.
pub(super) struct ParamLane {
    name: String,
    slot_control: Option<SlotControl>,
    prepared: Option<PreparedParameterHandle>,
    input: ParamInput,
    link: Option<LinkSignal>,
    /// Fraction of the remaining distance covered per sample; `None` keeps the
    /// raw curve values.
    smoothing: Option<f64>,
    /// Next sample to evaluate.
    cursor: u64,
    /// Smoother output; meaningful once the lane has started.
    filtered: f64,
    started: bool,
    /// Last value handed to the device, so an unchanged sample emits no event.
    applied: Option<f64>,
    /// Touch arbitration state for exactly this parameter.
    touch: TouchLane,
}

struct LinkSignal {
    formula: LinkFormula,
    mapping: Option<PreparedMapping>,
    midi: bool,
    source_smoothing: Option<f64>,
    source_value: Option<f64>,
}

impl LinkSignal {
    fn apply(&self, value: f64) -> f64 {
        let normalized = if self.midi {
            apply_midi_formula(self.formula, value)
        } else {
            self.formula.apply(value)
        };
        self.mapping
            .map_or(normalized, |mapping| mapping.apply(normalized))
    }
}

impl ParamLane {
    /// Position the lane at `sample` without replaying earlier samples.
    fn seed(&mut self, sample: u64) {
        self.cursor = sample;
        self.started = false;
        self.filtered = 0.0;
        self.applied = None;
        if let Some(link) = &mut self.link {
            link.source_value = None;
        }
    }

    fn curve_at(&self, sample: u64, tempo: Option<SampleTempo>) -> Option<f64> {
        let curve = self.input.timeline()?;
        if curve.is_lfo() {
            let quarter = tempo.map_or(0.0, |tempo| tempo.quarter_at(sample));
            let cycles = curve.lfo_cycles(quarter, sample)?;
            curve.lfo_value(cycles)
        } else {
            curve.envelope_value(sample as f64)
        }
    }

    /// Target at `sample`; absent before a lane or CC source starts writing.
    fn target_at(&self, sample: u64, tempo: Option<SampleTempo>) -> Option<f64> {
        self.held_target().or_else(|| {
            let value = self.curve_at(sample, tempo)?;
            Some(self.link.as_ref().map_or(value, |link| link.apply(value)))
        })
    }

    fn held_target(&self) -> Option<f64> {
        Some(self.link.as_ref()?.apply(self.input.held_value()?))
    }

    /// Value for the current sample, then advance one sample.
    fn next_value(&mut self, tempo: Option<SampleTempo>, held: Option<f64>) -> Option<f64> {
        let sample = self.cursor;
        self.cursor += 1;
        let target = if let Some(held) = held {
            held
        } else {
            let mut target = self.curve_at(sample, tempo)?;
            if let Some(link) = &mut self.link {
                if let Some(alpha) = link.source_smoothing {
                    target = link
                        .source_value
                        .map_or(target, |prior| smooth_step(prior, target, alpha));
                    link.source_value = Some(target);
                }
                target = link.apply(target);
            }
            target
        };
        let Some(coefficient) = self.smoothing else {
            return Some(target);
        };
        let value = if self.started {
            smooth_step(self.filtered, target, coefficient)
        } else {
            // The first value starts at the source; subsequent changes smooth.
            self.started = true;
            target
        };
        self.filtered = value;
        Some(value)
    }

    /// Advance the lane's clock over `span` samples without moving its one-pole.
    ///
    /// A held touch owns the parameter, so the lane is suspended rather than run
    /// silently: on release it resumes the user's smoothing from the last value
    /// the lane produced. That keeps the release fade a hand-back, not a second
    /// smoother competing with the lane's own time constant.
    fn hold(&mut self, span: u64) {
        self.cursor = self.cursor.saturating_add(span);
    }
}

/// An effect wrapper that applies its parameter lanes every block.
pub(super) struct AutomatedEffect {
    inner: Box<dyn AudioEffect>,
    lanes: Vec<ParamLane>,
    /// One authored value per parameter, shared by all its automation clips.
    static_parameters: Vec<(usize, Option<f64>)>,
    cursor: u64,
    sample_rate: u32,
    /// The project clock; only a tempo-following LFO reads it.
    clock: Arc<ProjectClock>,
    /// Live touch state; `None` on every offline render path.
    touch: Option<Arc<TouchRegistry>>,
    /// Reused per-block change list so a smoothed lane does not allocate.
    events: Vec<(u64, usize, f64)>,
    plans: Vec<TouchPlan>,
    slot: Option<slot_mix::SlotMix>,
}

/// Compiled lanes plus the clock they were resolved against.
pub(super) struct CompiledAutomation {
    pub clock: Arc<ProjectClock>,
    pub lanes: Vec<ParamLane>,
}

impl CompiledAutomation {
    pub(super) fn append_links(
        &mut self,
        effect: &Effect,
        links: &[&LinkedLane],
        sample_rate: u32,
        owner: &str,
        slot: usize,
    ) -> Result<(), String> {
        if links.is_empty() {
            return Ok(());
        }
        let total = self
            .lanes
            .len()
            .checked_add(links.len())
            .ok_or_else(|| AutomationPreparationError::ScratchCapacity.to_string())?;
        preparation::event_capacity(total).map_err(|error| error.to_string())?;
        self.lanes
            .try_reserve_exact(links.len())
            .map_err(|_| AutomationPreparationError::ScratchAllocation.to_string())?;
        let device = crate::dsp::device_for_effect(&effect.effect_type);
        for link in links {
            let parameter = link.parameter.as_str();
            let slot_control = SlotControl::from_id(parameter);
            let (first, last) = link.bounds;
            let midi = matches!(link.input, ParamInput::MidiCc(_));
            // A centered MIDI formula can peak between the two raw endpoints.
            let (mut a, mut b) = if midi {
                (0.0, 1.0)
            } else {
                (link.formula.apply(first), link.formula.apply(last))
            };
            let mapping = link
                .value_mapping
                .map(|mapping| PreparedMapping::new(mapping, a.min(b), a.max(b)))
                .transpose()?;
            if slot_control == Some(SlotControl::Enabled) {
                if mapping.is_some() {
                    return Err(
                        "control link value mapping needs a numeric effect parameter".into(),
                    );
                }
            } else {
                if let Some(mapping) = mapping {
                    a = mapping.apply(a);
                    b = mapping.apply(b);
                }
                mapped_bounds::validate(parameter, device, Some((a.min(b), a.max(b))))?;
            }
            self.lanes.push(ParamLane {
                name: parameter.to_owned(),
                slot_control,
                prepared: None,
                input: link.input.clone(),
                link: Some(LinkSignal {
                    formula: link.formula,
                    mapping,
                    midi,
                    source_smoothing: link.input.timeline().and_then(LaneCurve::smoothing_alpha),
                    source_value: None,
                }),
                smoothing: (link.smoothing_ms > 0).then(|| {
                    crate::smoothing::one_pole_alpha(
                        f64::from(link.smoothing_ms),
                        1.0,
                        f64::from(sample_rate),
                    )
                }),
                cursor: 0,
                filtered: 0.0,
                started: false,
                applied: None,
                touch: TouchLane::new(touch::effect_key(owner, slot, parameter)),
            });
        }
        Ok(())
    }
}

/// Compile an effect's lanes; callers decide whether an error is fatal.
///
/// A muted lane compiles to nothing, so the device keeps its declared value and
/// no parameter event is produced for it.
pub(super) fn compile(
    effect: &Effect,
    lanes: &[&AutomationLane],
    properties: &ProjectProperties,
    sample_rate: u32,
    owner: &str,
    effect_index: usize,
) -> Result<CompiledAutomation, String> {
    let active_lanes = lanes.iter().filter(|lane| !lane.muted).count();
    preparation::event_capacity(active_lanes).map_err(|error| error.to_string())?;
    let clock = Arc::new(ProjectClock::new(properties)?);
    let device = crate::dsp::device_for_effect(&effect.effect_type);
    let mut compiled = Vec::new();
    compiled
        .try_reserve_exact(active_lanes)
        .map_err(|_| AutomationPreparationError::ScratchAllocation.to_string())?;
    for lane in lanes {
        if lane.muted {
            continue;
        }
        let (_, parameter) = oc_dsl::automation::effect_lane(&lane.param)
            .ok_or_else(|| format!("'{}' is not an effect parameter lane", lane.param))?;
        compiled.push(compile_lane(
            parameter,
            lane,
            device,
            &clock,
            sample_rate,
            owner,
            effect_index,
        )?);
    }
    Ok(CompiledAutomation {
        clock,
        lanes: compiled,
    })
}

/// The optional detector input for one processing call.
#[derive(Clone, Copy)]
enum Detector<'a> {
    None,
    /// Interleaved stereo audio key.
    Key(&'a [f32]),
    /// One normalized value per frame.
    Control(&'a [f32]),
}

impl<'a> Detector<'a> {
    /// The same detector restricted to one frame span; a short input ends in
    /// silence, which is what the per-segment checks below do.
    fn slice(self, start: usize, end: usize) -> Self {
        match self {
            Self::None => Self::None,
            Self::Key(key) => Self::Key(key.get(start * 2..end * 2).unwrap_or(&[])),
            Self::Control(control) => Self::Control(control.get(start..end).unwrap_or(&[])),
        }
    }
}

impl AutomatedEffect {
    pub(super) fn new(
        mut inner: Box<dyn AudioEffect>,
        mut lanes: Vec<ParamLane>,
        sample_rate: u32,
        touch: Option<Arc<TouchRegistry>>,
        clock: Arc<ProjectClock>,
    ) -> Result<Self, AutomationPreparationError> {
        let event_capacity = preparation::event_capacity(lanes.len())?;
        let mut events = Vec::new();
        events
            .try_reserve_exact(event_capacity)
            .map_err(|_| AutomationPreparationError::ScratchAllocation)?;
        let mut plans = Vec::new();
        plans
            .try_reserve_exact(lanes.len())
            .map_err(|_| AutomationPreparationError::ScratchAllocation)?;
        let mut static_parameters: Vec<(usize, Option<f64>)> = Vec::new();
        static_parameters
            .try_reserve_exact(lanes.len())
            .map_err(|_| AutomationPreparationError::ScratchAllocation)?;
        for index in 0..lanes.len() {
            if lanes[index].slot_control.is_some() {
                continue;
            }
            let name = &lanes[index].name;
            if let Some(&(first, _)) = static_parameters
                .iter()
                .find(|&&(first, _)| lanes[first].name == *name)
            {
                lanes[index].prepared = lanes[first].prepared;
                continue;
            }
            let prepared = inner.prepare_parameter(name);
            static_parameters.push((
                index,
                inner.get_param(name).filter(|value| value.is_finite()),
            ));
            lanes[index].prepared = prepared;
        }
        Ok(Self {
            inner,
            lanes,
            static_parameters,
            cursor: 0,
            sample_rate,
            clock,
            touch,
            events,
            plans,
            slot: None,
        })
    }

    pub(super) fn with_slot_mix(
        mut self,
        wet: f64,
        enabled: bool,
    ) -> Result<Self, oc_audio::BlockPreparationError> {
        self.slot = Some(slot_mix::SlotMix::new(
            wet,
            enabled,
            self.inner.latency_samples(),
        )?);
        Ok(self)
    }

    /// The one arbitration point: per parameter, per poll span, the lane value
    /// or the hand value wins. A release is noticed within one poll span so the
    /// 20 ms fade stays well inside the 50 ms hand-back budget.
    fn collect_events(
        &mut self,
        events: &mut Vec<(u64, usize, f64)>,
        start: u64,
        frames: u64,
        sample_rate: u32,
    ) {
        let span_frames = if self.touch.is_some() {
            touch::POLL_FRAMES
        } else {
            frames
        };
        for lane in &mut self.lanes {
            if lane.cursor != start {
                lane.seed(start);
            }
        }
        let mut offset = 0u64;
        let follows_tempo = self.lanes.iter().any(|lane| {
            lane.input
                .timeline()
                .is_some_and(LaneCurve::lfo_follows_tempo)
        });
        while offset < frames {
            let at = start + offset;
            let tempo = follows_tempo.then(|| {
                self.clock
                    .sample_tempo_at(at, sample_rate)
                    .expect("validated render clock and sample rate")
            });
            let mut span = (frames - offset).min(span_frames).max(1);
            // A tempo or meter change starts a new span, so the phase step is
            // exact on both sides of the step.
            if let Some(change) = self
                .clock
                .next_change_sample(at, sample_rate)
                .expect("validated render clock and sample rate")
            {
                span = span.min(change.saturating_sub(at)).max(1);
            }
            self.plans.clear();
            for lane in self.lanes.iter_mut() {
                let status = self
                    .touch
                    .as_ref()
                    .and_then(|registry| registry.status(lane.touch.key()));
                self.plans
                    .push(lane.touch.plan(status, span as u32, sample_rate));
            }

            for (index, lane) in self.lanes.iter_mut().enumerate() {
                let plan = self.plans[index];
                if let TouchPlan::Hand(hand) = plan {
                    // The hand is the only writer; the lane keeps its clock but
                    // holds its smoothing state for the release.
                    if lane.applied != Some(hand) {
                        events.push((offset, index, hand));
                        lane.applied = Some(hand);
                    }
                    lane.hold(span);
                    continue;
                }
                let fade_end = match plan {
                    TouchPlan::Release { .. } => u64::from(plan.fade_end().unwrap_or(0)).min(span),
                    _ => 0,
                };
                let held = lane.held_target();
                if !matches!(lane.input, ParamInput::Timeline(_)) {
                    let settled = held == lane.applied
                        && (lane.smoothing.is_none() || held == Some(lane.filtered));
                    if held.is_none() || (settled && matches!(plan, TouchPlan::Lane)) {
                        lane.hold(span);
                        continue;
                    }
                }
                let mut next_step = 0;
                while next_step < span {
                    let inactive = lane.input.timeline().map_or(0, |curve| {
                        curve.inactive_envelope_prefix(lane.cursor, span - next_step)
                    });
                    if inactive > 0 {
                        // An absent target leaves both smoothing and the last
                        // applied value untouched, including during touch release.
                        lane.cursor += inactive;
                        next_step += inactive;
                        continue;
                    }
                    let step = next_step;
                    next_step += 1;
                    let Some(lane_value) = lane.next_value(tempo, held) else {
                        continue;
                    };
                    if step < fade_end {
                        // The hand-back writes on the fixed fade grid; between
                        // grid points the device keeps the last value.
                        if step % FADE_STEP != 0 {
                            continue;
                        }
                        let Some(value) = plan.value(lane_value, step as u32) else {
                            continue;
                        };
                        if lane.applied != Some(value) {
                            events.push((offset + step, index, value));
                            lane.applied = Some(value);
                        }
                        continue;
                    }
                    if lane.applied != Some(lane_value) {
                        events.push((offset + step, index, lane_value));
                        lane.applied = Some(lane_value);
                    }
                }
            }
            offset += span;
        }
    }

    /// Apply one block in bounded spans.
    ///
    /// The caller may hand over a whole timeline buffer when the project has no
    /// tempo/meter map. Each span collects and applies its own events, so the
    /// scratch list stays bounded; a lane is sampled at absolute positions, so
    /// the written values do not depend on where a span ends.
    fn process_block(
        &mut self,
        buffer: &mut [f32],
        input: Detector<'_>,
        transport: Option<&PluginTransport>,
    ) {
        if self.lanes.is_empty() && self.slot.is_none() {
            self.process_span(buffer, input, transport);
            return;
        }
        let frames = buffer.len() / 2;
        let limit =
            (self.inner.max_block_frames().max(1) as usize).min(MAX_AUTOMATION_BLOCK_FRAMES);
        let mut position = 0usize;
        while position < frames {
            let chunk_end = (position + limit).min(frames);
            let chunk_transport = transport
                .map(|transport| transport.at_sample(transport.sample_pos + position as u64));
            self.process_span(
                &mut buffer[position * 2..chunk_end * 2],
                input.slice(position, chunk_end),
                chunk_transport.as_ref(),
            );
            if self.inner.processing_failed() {
                buffer.fill(0.0);
                return;
            }
            position = chunk_end;
        }
    }

    /// Apply one bounded span; event offsets are relative to `buffer`.
    fn process_span(
        &mut self,
        buffer: &mut [f32],
        input: Detector<'_>,
        transport: Option<&PluginTransport>,
    ) {
        let sample_rate = self.sample_rate;
        let frames = buffer.len() / 2;
        if frames == 0 {
            return;
        }
        let start = transport.as_ref().map_or(self.cursor, |t| t.sample_pos);
        if let Some(slot) = &mut self.slot
            && start != self.cursor
        {
            slot.seek(start, &self.lanes, &self.clock, sample_rate);
        }
        self.cursor = start + frames as u64;

        // Move the scratch list out so processing can borrow `self` freely.
        let mut events = std::mem::take(&mut self.events);
        events.clear();
        debug_assert!(events.capacity() >= self.lanes.len() * frames);
        self.collect_events(&mut events, start, frames as u64, sample_rate);
        // Lanes emit at most one event per sample. Lane index preserves the
        // declared writer order at equal offsets without a stable-sort buffer.
        events.sort_unstable_by_key(|(offset, lane, _)| (*offset, *lane));
        if let Some(slot) = &mut self.slot {
            slot.capture(buffer, &mut events, &self.lanes);
        }

        if self.inner.supports_param_offsets() {
            // Queue points per device-sized chunk: hosts only accept offsets
            // inside the block they are about to process.
            let limit = (self.inner.max_block_frames().max(1) as usize).min(frames);
            let mut position = 0usize;
            let mut index = 0usize;
            while position < frames {
                let segment_transport =
                    transport.map(|transport| transport.at_sample(start + position as u64));
                let maximum = limit.min(frames - position);
                let chunk_frames = segment_transport
                    .as_ref()
                    .map(|transport| transport.block_frames(maximum))
                    .unwrap_or(maximum)
                    .max(1);
                let chunk_end = (position + chunk_frames).min(frames);
                while let Some((offset, lane, value)) = events.get(index) {
                    if *offset >= chunk_end as u64 {
                        break;
                    }
                    let relative = u32::try_from(*offset as usize - position).unwrap_or(u32::MAX);
                    self.inner
                        .queue_param_at(&self.lanes[*lane].name, *value, relative);
                    index += 1;
                }
                let segment = &mut buffer[position * 2..chunk_end * 2];
                self.process_chunk(
                    segment,
                    input,
                    position,
                    chunk_end,
                    segment_transport.as_ref(),
                );
                position = chunk_end;
            }
            self.events = events;
            self.blend_slot(buffer);
            return;
        }

        let mut position = 0usize;
        let mut index = 0usize;
        while position < frames {
            while let Some((offset, lane, value)) = events.get(index) {
                if *offset > position as u64 {
                    break;
                }
                if !write_parameter(&mut *self.inner, &self.lanes[*lane], *value) {
                    buffer.fill(0.0);
                    self.events = events;
                    return;
                }
                index += 1;
            }
            let next = events
                .get(index)
                .map_or(frames, |(offset, _, _)| (*offset as usize).min(frames));
            let segment_end = next.max(position + 1);
            let segment = &mut buffer[position * 2..segment_end * 2];
            let segment_transport =
                transport.map(|transport| transport.at_sample(start + position as u64));
            match input {
                Detector::Key(sidechain) if sidechain.len() >= segment_end * 2 => {
                    let key = &sidechain[position * 2..segment_end * 2];
                    self.inner.process_with_context(
                        segment,
                        AudioProcessContext {
                            sample_rate,
                            transport: segment_transport.as_ref(),
                            input: AudioProcessInput::Sidechain(key),
                        },
                    );
                }
                Detector::Control(control) => {
                    let span = control.get(position..segment_end).unwrap_or(&[]);
                    self.inner.process_with_context(
                        segment,
                        AudioProcessContext {
                            sample_rate,
                            transport: segment_transport.as_ref(),
                            input: AudioProcessInput::Control(span),
                        },
                    );
                }
                _ => self.inner.process_with_context(
                    segment,
                    AudioProcessContext {
                        sample_rate,
                        transport: segment_transport.as_ref(),
                        input: AudioProcessInput::Main,
                    },
                ),
            }
            position = segment_end;
        }
        self.events = events;
        self.blend_slot(buffer);
    }

    fn blend_slot(&self, buffer: &mut [f32]) {
        if let Some(slot) = &self.slot
            && !self.inner.processing_failed()
        {
            slot.blend(buffer);
        }
    }

    fn process_chunk(
        &mut self,
        segment: &mut [f32],
        input: Detector<'_>,
        position: usize,
        chunk_end: usize,
        transport: Option<&PluginTransport>,
    ) {
        match input {
            Detector::Key(sidechain) if sidechain.len() >= chunk_end * 2 => {
                let key = &sidechain[position * 2..chunk_end * 2];
                self.inner.process_with_context(
                    segment,
                    AudioProcessContext {
                        sample_rate: self.sample_rate,
                        transport,
                        input: AudioProcessInput::Sidechain(key),
                    },
                );
            }
            Detector::Control(control) => {
                let span = control.get(position..chunk_end).unwrap_or(&[]);
                self.inner.process_with_context(
                    segment,
                    AudioProcessContext {
                        sample_rate: self.sample_rate,
                        transport,
                        input: AudioProcessInput::Control(span),
                    },
                );
            }
            _ => self.inner.process_with_context(
                segment,
                AudioProcessContext {
                    sample_rate: self.sample_rate,
                    transport,
                    input: AudioProcessInput::Main,
                },
            ),
        }
    }
}

/// Resolve one effect lane and its output bounds before real-time processing.
fn compile_lane(
    parameter: &str,
    lane: &AutomationLane,
    device: Option<&DeviceSchema>,
    clock: &ProjectClock,
    sample_rate: u32,
    owner: &str,
    effect_index: usize,
) -> Result<ParamLane, String> {
    let slot_control = SlotControl::from_id(parameter);
    if let Some(control) = slot_control {
        oc_dsl::automation::validate_slot_lane(lane, control)?;
    }
    let curve = LaneCurve::new(lane, clock, sample_rate, |value| {
        crate::automation::effect_curve_point_value(lane, parameter, value, device)
    })?;
    if lane.value_mapping.is_some() {
        mapped_bounds::validate(parameter, device, curve.value_bounds())?;
    }
    let smoothing = curve.smoothing_alpha();
    Ok(ParamLane {
        name: parameter.to_string(),
        slot_control,
        prepared: None,
        input: ParamInput::Timeline(Arc::new(curve)),
        link: None,
        smoothing,
        cursor: 0,
        filtered: 0.0,
        started: false,
        applied: None,
        touch: TouchLane::new(touch::effect_key(owner, effect_index, parameter)),
    })
}

fn write_parameter(effect: &mut dyn AudioEffect, lane: &ParamLane, value: f64) -> bool {
    let outcome = lane
        .prepared
        .map_or(PreparedParameterOutcome::Unsupported, |handle| {
            effect.set_prepared_parameter(handle, value)
        });
    match outcome {
        PreparedParameterOutcome::Unsupported => {
            effect.set_param(&lane.name, value);
            true
        }
        PreparedParameterOutcome::Applied => true,
        PreparedParameterOutcome::Invalidated(_) | PreparedParameterOutcome::Failed => false,
    }
}

impl AudioEffect for AutomatedEffect {
    fn preserves_channel_equality(&self) -> bool {
        self.inner.sidechain_source().is_none()
            && self.inner.control_source().is_none()
            && self.inner.preserves_channel_equality()
    }

    fn configure_midi_input_latency(&mut self, samples: u32) {
        self.inner.configure_midi_input_latency(samples);
    }
    fn processing_failed(&self) -> bool {
        self.inner.processing_failed()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        self.inner.take_processing_failure()
    }

    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.inner.prepare_block(max_frames)?;
        if let Some(slot) = &mut self.slot {
            slot.prepare_latency(self.inner.latency_samples())?;
        }
        Ok(())
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        let detector = match context.input {
            AudioProcessInput::Main => Detector::None,
            AudioProcessInput::Sidechain(key) => Detector::Key(key),
            AudioProcessInput::Control(values) => Detector::Control(values),
        };
        self.process_block(buffer, detector, context.transport);
    }

    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.process_block(buffer, Detector::None, None);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_block(buffer, Detector::None, transport.as_ref());
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_block(buffer, Detector::Key(sidechain), transport.as_ref());
    }

    fn supports_sidechain(&self) -> bool {
        self.inner.supports_sidechain()
    }

    fn sidechain_source(&self) -> Option<&str> {
        self.inner.sidechain_source()
    }

    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        _sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_block(buffer, Detector::Control(control), transport.as_ref());
    }

    fn supports_control_input(&self) -> bool {
        self.inner.supports_control_input()
    }

    fn control_source(&self) -> Option<&str> {
        self.inner.control_source()
    }

    fn configure_sidechain_alignment(&mut self, main_latency: u32, sidechain_latency: u32) -> u32 {
        self.inner
            .configure_sidechain_alignment(main_latency, sidechain_latency)
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match (SlotControl::from_id(name), &mut self.slot) {
            (Some(control), Some(slot)) => slot.write(control, value),
            _ => {
                self.inner.set_param(name, value);
                if value.is_finite() {
                    for (index, stored) in &mut self.static_parameters {
                        if self.lanes[*index].name == name {
                            *stored = Some(value);
                        }
                    }
                }
            }
        }
    }

    fn prepare_parameter(&mut self, name: &str) -> Option<PreparedParameterHandle> {
        self.inner.prepare_parameter(name)
    }

    fn set_prepared_parameter(
        &mut self,
        handle: PreparedParameterHandle,
        value: f64,
    ) -> PreparedParameterOutcome {
        let outcome = self.inner.set_prepared_parameter(handle, value);
        if outcome == PreparedParameterOutcome::Applied && value.is_finite() {
            for (index, stored) in &mut self.static_parameters {
                if self.lanes[*index].prepared == Some(handle) {
                    *stored = Some(value);
                }
            }
        }
        outcome
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
        match (SlotControl::from_id(name), &self.slot) {
            (Some(control), Some(slot)) => Some(slot.read(control)),
            _ => self.inner.get_param(name),
        }
    }

    fn reset(&mut self) {
        self.inner.reset();
        // An automation clip writes nothing before its first point. Restore
        // the authored setting explicitly so that region plays as on a fresh run.
        for &(index, value) in &self.static_parameters {
            if let Some(value) = value
                && !write_parameter(&mut *self.inner, &self.lanes[index], value)
            {
                break;
            }
        }
        if let Some(slot) = &mut self.slot {
            slot.reset();
        }
        self.cursor = 0;
        for lane in &mut self.lanes {
            lane.seed(0);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AudioGraph;
    use oc_dsl::ast::{
        AutomationLane, AutomationPoint, BarBeat, Curve, EffectType, LaneMode, LfoSpeed,
        ParamValue, TimeRange,
    };
    use std::sync::{Arc, Mutex};

    include!("effect_automation/inactive_span_tests.rs");
    include!("effect_automation/prepared_parameter_tests.rs");

    /// Default lane values: no smoothing, linear segments.
    fn lane(param: &str, ppq: u32, points: &[(BarBeat, ParamValue)]) -> AutomationLane {
        lane_with(param, ppq, 0.0, points)
    }

    fn lane_with(
        param: &str,
        ppq: u32,
        smoothing_ms: f64,
        points: &[(BarBeat, ParamValue)],
    ) -> AutomationLane {
        AutomationLane {
            param: param.to_string(),
            ppq,
            smoothing_ms,
            points: points
                .iter()
                .map(|(position, value)| AutomationPoint {
                    curvature: None,
                    position: *position,
                    value: value.clone(),
                    curve: Curve::Linear,
                    tension: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn stairs(lane: &mut AutomationLane) {
        for point in &mut lane.points {
            point.curve = Curve::Stairs;
        }
    }

    fn project(source: &str) -> oc_dsl::ast::Project {
        oc_dsl::parse(source).expect("test project parses")
    }

    fn plugin_effect() -> Effect {
        Effect {
            effect_type: EffectType::Plugin("vst3.test.fake".into()),
            params: Vec::new(),
            bypass: false,
            sidechain_source: None,
        }
    }

    /// A plain device that records the value applied to each render segment.
    struct Recorder {
        current: f64,
        segments: Arc<Mutex<Vec<(u64, usize, f64)>>>,
    }

    impl AudioEffect for Recorder {
        fn process(&mut self, _buffer: &mut [f32], _sample_rate: u32) {}
        fn process_with_transport(
            &mut self,
            buffer: &mut [f32],
            _sample_rate: u32,
            transport: Option<PluginTransport>,
        ) {
            if let Some(transport) = transport {
                self.segments.lock().unwrap().push((
                    transport.sample_pos,
                    buffer.len() / 2,
                    self.current,
                ));
            }
        }
        fn set_param(&mut self, _name: &str, value: f64) {
            self.current = value;
        }
        fn get_param(&self, _name: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "recorder"
        }
    }

    /// The value the device held at every sample of one render.
    fn recorded_samples(lane: &AutomationLane, frames: usize, block: usize) -> Vec<f64> {
        recorded_samples_in("@project \"auto\"\n  tempo: 120\n", lane, frames, block)
    }

    fn recorded_samples_in(
        source: &str,
        lane: &AutomationLane,
        frames: usize,
        block: usize,
    ) -> Vec<f64> {
        let project = project(source);
        let effect = plugin_effect();
        let compiled =
            compile(&effect, &[lane], &project.properties, 48_000, "track:0", 0).unwrap();
        let segments = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            current: 0.0,
            segments: segments.clone(),
        };
        let mut automated = AutomatedEffect::new(
            Box::new(recorder),
            compiled.lanes,
            48_000,
            None,
            compiled.clock,
        )
        .unwrap();
        for start in (0..frames).step_by(block) {
            let length = block.min(frames - start);
            let mut buffer = vec![0.0f32; length * 2];
            automated.process_with_transport(
                &mut buffer,
                48_000,
                Some(PluginTransport::new(start as u64, 48_000, 120.0)),
            );
        }
        drop(automated);
        let segments = Arc::try_unwrap(segments).unwrap().into_inner().unwrap();
        let mut values = vec![f64::NAN; frames];
        for (start, length, value) in segments {
            for sample in values
                .iter_mut()
                .take((start as usize + length).min(frames))
                .skip(start as usize)
            {
                *sample = value;
            }
        }
        values
    }

    /// A timeline-sized block must not grow the event scratch list with the
    /// block; the values written are the same as with small blocks.
    #[test]
    fn a_long_block_bounds_the_event_list_and_matches_small_blocks() {
        let lane = lane(
            "filter.cutoff",
            0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.1)),
                (BarBeat::new(1, 3, 0), ParamValue::Number(0.9)),
            ],
        );
        let project = project("@project \"auto\"\n  tempo: 120\n");
        let effect = plugin_effect();
        let compiled =
            compile(&effect, &[&lane], &project.properties, 48_000, "track:0", 0).unwrap();
        let segments = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            current: 0.0,
            segments: segments.clone(),
        };
        let mut automated = AutomatedEffect::new(
            Box::new(recorder),
            compiled.lanes,
            48_000,
            None,
            compiled.clock,
        )
        .unwrap();

        let frames = 16_384;
        let mut buffer = vec![0.0f32; frames * 2];
        automated.process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(0, 48_000, 120.0)),
        );
        assert!(
            automated.events.capacity() <= MAX_AUTOMATION_BLOCK_FRAMES,
            "a {frames}-frame block kept {} event slots",
            automated.events.capacity()
        );

        drop(automated);
        let segments = Arc::try_unwrap(segments).unwrap().into_inner().unwrap();
        let mut large = vec![f64::NAN; frames];
        for (start, length, value) in segments {
            for sample in large
                .iter_mut()
                .take((start as usize + length).min(frames))
                .skip(start as usize)
            {
                *sample = value;
            }
        }
        assert_eq!(large, recorded_samples(&lane, frames, 128));
    }

    /// A block larger than one span keeps a tempo-following LFO's phase; the
    /// spans continue the phase instead of restarting it.
    #[test]
    fn a_long_block_keeps_a_tempo_following_lfo_phase() {
        let lane = wobble_lane(LfoSpeed::Note {
            numerator: 1,
            denominator: 4,
        });
        let frames = 8_192;
        let long = recorded_samples(&lane, frames, frames);
        let spans = recorded_samples(&lane, frames, MAX_AUTOMATION_BLOCK_FRAMES);
        for (index, (long, span)) in long.iter().zip(&spans).enumerate() {
            assert!(
                (long - span).abs() < 1e-9,
                "sample {index}: long block {long} != bounded spans {span}"
            );
        }
    }

    /// One cycle of a four-point waveform: 0 → 1 → 0 → -1 around a zero midpoint.
    fn wobble_lane(speed: LfoSpeed) -> AutomationLane {
        let mut lane = lane(
            "filter.cutoff",
            0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.0)),
                (BarBeat::new(1, 2, 0), ParamValue::Number(1.0)),
                (BarBeat::new(1, 3, 0), ParamValue::Number(0.0)),
                (BarBeat::new(1, 4, 0), ParamValue::Number(-1.0)),
            ],
        );
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = speed;
        lane
    }

    /// The closed version: a 0 → 1 → 0 → -1 → 0 wave whose interior anchors are
    /// the extremes, so depth and peak tests hit them exactly at a round period.
    fn closed_wobble_lane(speed: LfoSpeed) -> AutomationLane {
        let mut lane = lane(
            "filter.cutoff",
            0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.0)),
                (BarBeat::new(1, 2, 0), ParamValue::Number(1.0)),
                (BarBeat::new(1, 3, 0), ParamValue::Number(0.0)),
                (BarBeat::new(1, 4, 0), ParamValue::Number(-1.0)),
                (BarBeat::new(2, 1, 0), ParamValue::Number(0.0)),
            ],
        );
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = speed;
        lane
    }

    /// Sample indexes where the waveform rises through its midpoint.
    fn rising_crossings(values: &[f64]) -> Vec<usize> {
        let mut crossings = Vec::new();
        for index in 1..values.len() {
            if values[index - 1] <= 0.0 && values[index] > 0.0 {
                crossings.push(index);
            }
        }
        crossings
    }

    /// Median gap between rising crossings, in samples: one waveform period.
    fn measured_period(values: &[f64]) -> f64 {
        let crossings = rising_crossings(values);
        assert!(crossings.len() >= 3, "not enough crossings: {crossings:?}");
        let mut gaps: Vec<f64> = crossings
            .windows(2)
            .map(|pair| (pair[1] - pair[0]) as f64)
            .collect();
        gaps.sort_by(f64::total_cmp);
        gaps[gaps.len() / 2]
    }

    /// A four-point LFO lane's period equals the configured frequency's period.
    #[test]
    fn lfo_period_matches_the_configured_hz_speed() {
        let cases = [(1.0, 48_000.0), (2.0, 24_000.0), (5.0, 9_600.0)];
        for (hz, expected) in cases {
            let lane = wobble_lane(LfoSpeed::Hz { hz });
            let values = recorded_samples(&lane, 192_000, 512);
            let period = measured_period(&values[..]);
            assert!(
                (period - expected).abs() <= 1.0,
                "{hz} Hz: measured {period}, expected {expected}"
            );
        }
    }

    /// A note-value speed is one cycle per note, so a tempo change moves the
    /// period with the tempo. One cycle per quarter note is 24 000 samples at
    /// 120 bpm and 48 000 at 60 bpm.
    #[test]
    fn lfo_note_speed_follows_a_tempo_map() {
        let source = "@project \"auto\"\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n";
        let lane = wobble_lane(LfoSpeed::Note {
            numerator: 1,
            denominator: 4,
        });
        let values = recorded_samples_in(source, &lane, 192_000, 512);
        let crossings = rising_crossings(&values);
        let gaps: Vec<usize> = crossings.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.len() >= 4, "{crossings:?}");
        // The first quarter notes are at 120 bpm, the rest at 60 bpm.
        assert!(
            gaps[..2].iter().all(|gap| (1_500..=25_000).contains(gap)),
            "before the change: {gaps:?}"
        );
        assert!(
            gaps[2..].iter().all(|gap| (45_000..=49_000).contains(gap)),
            "after the change: {gaps:?}"
        );

        // Without the tempo map the same lane keeps the 120 bpm period.
        let steady = wobble_lane(LfoSpeed::Note {
            numerator: 1,
            denominator: 4,
        });
        let steady_values = recorded_samples(&steady, 96_000, 512);
        assert!((measured_period(&steady_values) - 24_000.0).abs() <= 1.0);
    }

    /// Amount scales depth around the waveform's midpoint and leaves the period
    /// alone; zero amount is the midpoint itself.
    #[test]
    fn lfo_amount_scales_depth_without_moving_the_period() {
        let full = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
        let quiet = {
            let mut lane = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
            lane.lfo.amount = 0.5;
            lane
        };
        let flat = {
            let mut lane = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
            lane.lfo.amount = 0.0;
            lane
        };
        let full_values = recorded_samples(&full, 144_000, 512);
        let quiet_values = recorded_samples(&quiet, 144_000, 512);
        let flat_values = recorded_samples(&flat, 144_000, 512);
        let span = |values: &[f64]| {
            let low = values.iter().copied().fold(f64::INFINITY, f64::min);
            let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            high - low
        };
        assert!((span(&full_values) - 2.0).abs() < 1e-6);
        assert!(
            (span(&quiet_values) - 1.0).abs() < 1e-6,
            "{}",
            span(&quiet_values)
        );
        assert!(span(&flat_values) < 1e-12, "{}", span(&flat_values));
        assert_eq!(
            measured_period(&full_values),
            measured_period(&quiet_values)
        );
    }

    /// Pulse width and skew move the shape inside the cycle; neither moves the
    /// cycle length. With 0→1→0 at phases 0, 1/3, 2/3 the peak sits at 1/3 of
    /// the shape, so the cycle position is predictable.
    #[test]
    fn lfo_pulse_width_and_skew_move_the_peak_not_the_period() {
        const PERIOD: f64 = 48_000.0;
        let peak_of = |values: &[f64], from: usize, to: usize| {
            let mut best = from;
            for index in from..to {
                if values[index] > values[best] {
                    best = index;
                }
            }
            best as f64
        };

        let plain = recorded_samples(&closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 }), 144_000, 512);
        let plain_peak = peak_of(&plain, 1, PERIOD as usize);
        assert!(
            (plain_peak - PERIOD / 4.0).abs() < 2.0,
            "plain peak at {plain_peak}"
        );

        let mut narrow = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
        narrow.lfo.pulse_width = 0.25;
        let narrow_values = recorded_samples(&narrow, 144_000, 512);
        let narrow_peak = peak_of(&narrow_values, 1, PERIOD as usize);
        assert!(
            (narrow_peak - PERIOD / 8.0).abs() < 2.0,
            "pw=0.25 peak at {narrow_peak}"
        );

        let mut bent = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
        bent.lfo.skew = 1.0;
        let bent_values = recorded_samples(&bent, 144_000, 512);
        let bent_peak = peak_of(&bent_values, 1, PERIOD as usize);
        let expected = PERIOD / 2.0;
        assert!(
            (bent_peak - expected).abs() < 3.0,
            "skew=1 peak at {bent_peak}, expected {expected}"
        );

        assert_eq!(measured_period(&narrow_values), PERIOD);
        assert_eq!(measured_period(&bent_values), PERIOD);
    }

    /// Cycle tension eases each segment towards a smoothstep around the same
    /// anchors: a quarter into the first segment the value drops, the peak
    /// stays at the anchor, and the period is unchanged.
    #[test]
    fn lfo_tension_eases_segments_without_moving_anchors() {
        let mut tense = closed_wobble_lane(LfoSpeed::Hz { hz: 1.0 });
        tense.lfo.tension = 1.0;
        let values = recorded_samples(&tense, 144_000, 512);
        let quarter = 48_000.0 / 4.0 * 0.25;
        let expected = 0.25 * 0.25 * (3.0 - 2.0 * 0.25);
        let at = quarter as usize;
        assert!(
            (values[at] - expected).abs() < 1e-9,
            "tension quarter {at}: {} != {expected}",
            values[at]
        );
        // The anchor itself is untouched, so the peak is still at 1/4.
        let peak = values[1..48_000]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index + 1)
            .unwrap();
        assert!((peak as f64 - 12_000.0).abs() < 2.0, "peak at {peak}");
        assert_eq!(measured_period(&values), 48_000.0);
    }

    /// The same lane in envelope mode holds the old absolute reading: the LFO
    /// controls are inert, and the values are the authored anchors.
    #[test]
    fn envelope_mode_ignores_the_cycle_controls() {
        let mut lane = wobble_lane(LfoSpeed::Hz { hz: 2.0 });
        lane.mode = LaneMode::Envelope;
        lane.lfo.amount = 0.1;
        lane.lfo.pulse_width = 0.2;
        let values = recorded_samples(&lane, 96_000, 512);
        for (index, value) in values.iter().enumerate() {
            // The anchors are one beat apart at 120 bpm: 24 000 samples.
            let expected = if index < 24_000 {
                index as f64 / 24_000.0
            } else if index < 48_000 {
                1.0 - (index - 24_000) as f64 / 24_000.0
            } else if index < 72_000 {
                -((index - 48_000) as f64) / 24_000.0
            } else {
                -1.0
            };
            assert!(
                (value - expected).abs() < 1e-9,
                "envelope sample {index}: {value} != {expected}"
            );
        }
    }

    /// An LFO lane goes through the same touch arbitration as an envelope:
    /// the hand owns the parameter while held and the lane resumes at the
    /// absolute phase it would have had, not from its start.
    #[test]
    fn lfo_lane_respects_touch_arbitration_and_keeps_its_phase() {
        let lane = wobble_lane(LfoSpeed::Hz { hz: 2.0 });
        let project = project("@project \"auto\"\n  tempo: 120\n");
        let effect = plugin_effect();
        let compiled =
            compile(&effect, &[&lane], &project.properties, 48_000, "track:0", 0).unwrap();
        let key = touch::effect_key("track:0", 0, "cutoff");
        let registry = Arc::new(TouchRegistry::new());
        let segments = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            current: 0.0,
            segments: segments.clone(),
        };
        let mut automated = AutomatedEffect::new(
            Box::new(recorder),
            compiled.lanes,
            48_000,
            Some(registry.clone()),
            compiled.clock,
        )
        .unwrap();
        const BLOCK: usize = 256;
        const HOLD_START: u64 = 256 * 48;
        const HOLD_END: u64 = 256 * 117;
        for start in (0..48_000).step_by(BLOCK) {
            let at = start as u64;
            if at == HOLD_START {
                registry.begin(&key, 0.1);
            }
            if at == HOLD_END {
                registry.release(&key);
            }
            let mut buffer = vec![0.0f32; BLOCK * 2];
            automated.process_with_transport(
                &mut buffer,
                48_000,
                Some(PluginTransport::new(at, 48_000, 120.0)),
            );
        }
        drop(automated);
        let segments = Arc::try_unwrap(segments).unwrap().into_inner().unwrap();
        let mut values = vec![f64::NAN; 48_000];
        for (start, length, value) in segments {
            for sample in values
                .iter_mut()
                .take((start as usize + length).min(48_000))
                .skip(start as usize)
            {
                *sample = value;
            }
        }
        for (index, value) in values.iter().enumerate().take(HOLD_START as usize) {
            assert!(!value.is_nan(), "lane written before the hold at {index}");
        }
        for value in values
            .iter()
            .take(HOLD_END as usize)
            .skip(HOLD_START as usize)
        {
            assert!(
                (value - 0.1).abs() < 1e-12,
                "hand value while held: {value}"
            );
        }
        // Release fade is 20 ms = 960 samples; after it the lane is exactly the
        // absolute-position render, which proves the phase kept running.
        let reference = recorded_samples(&lane, 48_000, 256);
        let after_fade = HOLD_END as usize + 1_100;
        for index in after_fade..48_000 {
            assert!(
                (values[index] - reference[index]).abs() < 1e-9,
                "resumed lane at {index}: {} != {}",
                values[index],
                reference[index]
            );
        }
    }

    /// Render one track's gain chain, driven by a single lane, block by block.
    fn render_gain(source: &str, frames: usize, block: usize) -> Vec<f32> {
        let project = project(source);
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        let mut output = vec![0.0f32; frames * 2];
        for start in (0..frames).step_by(block) {
            let length = block.min(frames - start);
            let mut buffer = vec![1.0f32; length * 2];
            graph.track_chains[0].process_with_transport(
                &mut buffer,
                48_000,
                Some(PluginTransport::new(start as u64, 48_000, 120.0)),
            );
            output[start * 2..(start + length) * 2].copy_from_slice(&buffer);
        }
        output
    }

    #[test]
    fn insert_mix_combines_device_automation_with_a_stepped_enable_lane() {
        let source = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=3, bypass=true, slot/wet=25%\n  automation: fx1.slot/enabled {1:2=true, 1:4=false}\n  automation: fx1.slot/wet {1:3=0.5}\n  automation: fx1.factor {1:3=2}\n";
        let frames = 75_000;
        let uneven = render_gain(source, frames, 733);
        assert_eq!(uneven, render_gain(source, frames, 8192));
        assert!(uneven[..24_000 * 2].iter().all(|x| *x == 1.0));
        assert!(uneven[24_000 * 2..72_000 * 2].iter().all(|x| *x == 1.5));
        assert!(uneven[72_000 * 2..].iter().all(|x| *x == 1.0));
    }

    /// The same lane drives a built-in device: stairs split the block at the
    /// point positions without any plugin involved.
    #[test]
    fn builtin_device_changes_at_the_lane_position_inside_a_block() {
        let project = project(
            "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1 curve=stairs, 1:2:0=0.25}\n  @phrase line [1:1 - 2:1]\n    C4 q\n",
        );
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        let mut buffer = vec![1.0f32; 48_000 * 2];
        graph.track_chains[0].process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(0, 48_000, 120.0)),
        );

        // 120 bpm: beat 2 is 24_000 frames, inside the single 48_000-frame block.
        assert_eq!(buffer[0], 1.0);
        assert_eq!(buffer[(24_000 - 1) * 2], 1.0);
        assert_eq!(buffer[24_000 * 2], 0.25);
        assert_eq!(buffer[(48_000 - 1) * 2], 0.25);
    }

    /// A clip writes only inside its span: nothing before its head, the first
    /// point held there, the last value at the exclusive end, and that value
    /// left on the parameter afterwards.
    #[test]
    fn a_clip_writes_nothing_outside_its_span_and_keeps_its_last_value() {
        // 120 bpm at 48 kHz: bar 3 is sample 192 000, bar 4 beat 1 is 288 000,
        // bar 4 beat 3 is 336 000 and bar 5 is the clip's exclusive end.
        let source = "@project \"auto\"\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [3:1 - 5:1] {4:1=0.25 curve=stairs, 4:3=0.75 curve=stairs}\n";
        let output = render_gain(source, 400_000, 512);
        let at = |index: usize| output[index * 2];
        assert!(
            output[..192_000 * 2]
                .iter()
                .all(|sample| (sample - 1.0).abs() < 1e-9),
            "the lane wrote before its head"
        );
        // The first point sits at 4:1, so the clip opens on its value at its head.
        assert_eq!(at(192_000), 0.25);
        assert_eq!(at(287_999), 0.25);
        assert_eq!(at(288_000), 0.25);
        assert_eq!(at(335_999), 0.25);
        assert_eq!(at(336_000), 0.75);
        assert_eq!(at(383_999), 0.75);
        // 5:1 is the exclusive end: nothing is written there or later, so the
        // last value stays instead of reverting to the declared factor.
        assert_eq!(at(384_000), at(383_999));
        assert_eq!(at(399_999), at(383_999));
    }

    #[test]
    fn rewind_restores_authored_parameter_before_a_late_clip() {
        let score = "@project\n  tempo: 120\n@track t | instrument: audio/clip\n  effects:\n    - gain: factor=0.75\n  automation: gain.factor [1:2 - 1:4] {1:2=0.25}\n";
        let project = project(score);
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        let chain = &mut graph.track_chains[0];
        chain.prepare_block(32).unwrap();
        let mut opening = [1.0; 64];
        chain.process(&mut opening, 48_000);
        assert_eq!(opening, [0.75; 64]);

        let mut middle = [1.0; 64];
        chain.process_with_transport(
            &mut middle,
            48_000,
            Some(PluginTransport::new(24_000, 48_000, 120.0)),
        );
        assert_eq!(middle, [0.25; 64]);

        let mut first_after_rewind = [1.0; 64];
        let operations = crate::test_alloc::operations(|| {
            chain.reset();
            chain.process(&mut first_after_rewind, 48_000);
        });
        assert_eq!(operations, 0);
        assert_eq!(first_after_rewind, opening);
    }

    #[test]
    fn rewind_restores_the_latest_mixer_edit_even_during_its_fade() {
        let score = "@project\n  tempo: 120\n@track t | instrument: audio/clip\n  effects:\n    - gain: factor=0.75\n  automation: gain.factor [1:2 - 1:4] {1:2=0.25}\n";
        let project = project(score);
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        let chain = &mut graph.track_chains[0];
        chain.prepare_block(32).unwrap();
        chain
            .fade_params(&[(0, "factor".into(), 0.75, 0.5)], 48_000)
            .unwrap();
        let mut block = [1.0; 64];
        chain.process_with_transport(
            &mut block,
            48_000,
            Some(PluginTransport::new(24_000, 48_000, 120.0)),
        );
        assert_eq!(block[0], 0.25);

        let mut first_after_rewind = [1.0; 64];
        let operations = crate::test_alloc::operations(|| {
            chain.reset();
            chain.process(&mut first_after_rewind, 48_000);
        });
        assert_eq!(operations, 0);
        assert_eq!(first_after_rewind, [0.5; 64]);
    }

    /// A muted lane writes nothing at all, so the device keeps the value its
    /// `effects:` entry declares.
    #[test]
    fn a_muted_lane_leaves_the_declared_value_in_place() {
        let source = "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor muted {1:1=0.25, 2:1=0.5}\n";
        let output = render_gain(source, 48_000, 512);
        assert!(
            output.iter().all(|sample| (sample - 1.0).abs() < 1e-9),
            "a muted lane changed the parameter"
        );
    }

    /// A clip's LFO cycle starts at the clip head, not at its first point, so a
    /// repeated clip restarts its phase instead of carrying a phase offset.
    #[test]
    fn an_lfo_clip_restarts_its_cycle_at_its_head() {
        let mut lane = lane(
            "filter.cutoff",
            0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(1.0)),
                (BarBeat::new(1, 3, 0), ParamValue::Number(-1.0)),
                (BarBeat::new(2, 1, 0), ParamValue::Number(1.0)),
            ],
        );
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = LfoSpeed::Hz { hz: 1.0 };
        // Outside the span the recorder reports the device's own value (0.0),
        // so a lane writing there would be visible.
        let legacy = recorded_samples(&lane, 200_000, 512);
        assert_eq!(legacy[120_000], -1.0, "phase 2.5 without a clip head");

        lane.time_range = Some(TimeRange {
            // 2:2 to 4:1; bar 2 beat 2 is sample 120 000 at 120 bpm.
            start: BarBeat::new(2, 2, 0),
            end: BarBeat::new(4, 1, 0),
        });
        let values = recorded_samples(&lane, 200_000, 512);
        assert_eq!(values[119_999], 0.0, "the lane wrote before its head");
        assert_eq!(values[120_000], 1.0, "the head is phase 0");
        // One cycle is 48 000 samples, half of it is 24 000.
        assert_eq!(values[144_000], -1.0);
        assert_eq!(values[168_000], 1.0);
    }

    /// An absent curve is linear: the value ramps between the points.
    #[test]
    fn default_curve_ramps_between_points() {
        let project = project(
            "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 1:2:0=0}\n",
        );
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        let mut buffer = vec![1.0f32; 48_000 * 2];
        graph.track_chains[0].process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(0, 48_000, 120.0)),
        );
        assert_eq!(buffer[0], 1.0, "the first point is exact");
        assert!((buffer[12_000 * 2] - 0.5).abs() < 1e-6, "half way is 0.5");
        assert_eq!(buffer[24_000 * 2], 0.0, "the second point is exact");
    }

    /// A fake VST3 effect must receive `inputParameterChanges` at a non-zero
    /// block offset, not only at the block boundary.
    #[test]
    fn fake_vst3_effect_receives_parameter_changes_at_block_offsets() {
        use oc_host::vst3_instance::test_support::{
            FAKE_PARAM_ID, FAKE_PARAM_NAME, FakeControl, fake_effect_instance,
        };

        let project = project("@project \"auto\"\n  tempo: 120\n");
        let effect = plugin_effect();
        let mut l = lane(
            &format!("plugin.{FAKE_PARAM_NAME}"),
            480,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.25)),
                (BarBeat::new(1, 1, 2), ParamValue::Number(0.75)),
            ],
        );
        stairs(&mut l);
        let compiled = compile(&effect, &[&l], &project.properties, 48_000, "track:0", 0).unwrap();
        assert_eq!(
            compiled.lanes[0].input.timeline().unwrap().points()[1].position,
            100.0
        );

        let control = Arc::new(FakeControl::default());
        let inner = fake_effect_instance(&control, 2, 512);
        let mut automated = AutomatedEffect::new(
            Box::new(inner),
            compiled.lanes,
            48_000,
            None,
            compiled.clock,
        )
        .unwrap();
        let mut buffer = vec![0.0f32; 512 * 2];
        automated.process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(0, 48_000, 120.0)),
        );

        let changes = control.param_changes.lock().unwrap().clone();
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert_eq!(changes[0].param_id, FAKE_PARAM_ID);
        assert_eq!(changes[0].sample_offset, 0);
        assert!((changes[0].value - 0.25).abs() < 1e-9);
        assert_eq!(changes[1].param_id, FAKE_PARAM_ID);
        assert_eq!(
            changes[1].sample_offset, 100,
            "a point inside the block must keep its own offset"
        );
        assert!((changes[1].value - 0.75).abs() < 1e-9);
    }

    /// Every curve's applied value follows its closed form at each sample.
    #[test]
    fn curve_values_match_their_closed_form_at_sample_positions() {
        // 120 bpm with 480 ppq: one beat is 24_000 samples and the segment
        // from beat 1 to beat 2 covers all of them.
        const FRAMES: usize = 24_000;
        type CurveCase = (Curve, Option<f64>, fn(f64) -> f64);
        let cases: [CurveCase; 5] = [
            (Curve::Linear, None, |u| u),
            (Curve::Stairs, None, |_| 0.0),
            (Curve::HalfSine, None, |u| {
                0.5 - 0.5 * (std::f64::consts::PI * u).cos()
            }),
            (Curve::Smooth, Some(1.0), |u| u * u * (3.0 - 2.0 * u)),
            (Curve::Smooth, Some(0.25), |u| {
                u + (u * u * (3.0 - 2.0 * u) - u) * 0.25
            }),
        ];
        for (curve, tension, expected) in cases {
            let mut lane = lane_with(
                "plugin.Any",
                480,
                0.0,
                &[
                    (BarBeat::new(1, 1, 0), ParamValue::Number(0.0)),
                    (BarBeat::new(1, 2, 0), ParamValue::Number(1.0)),
                ],
            );
            lane.points[0].curve = curve;
            lane.points[0].tension = tension;
            let values = recorded_samples(&lane, FRAMES, 512);
            for (sample, value) in values.iter().enumerate() {
                let u = sample as f64 / FRAMES as f64;
                assert!(
                    (value - expected(u)).abs() < 1e-9,
                    "{curve:?} at sample {sample}: {value} != {}",
                    expected(u)
                );
            }
        }
    }

    /// The one-pole reaches 63 % of a step in `smoothing_ms`, and the same
    /// render split into different block sizes produces identical values.
    #[test]
    fn smoothing_step_time_and_block_size_independence() {
        const FRAMES: usize = 20_000;
        let mut lane = lane_with(
            "plugin.Any",
            480,
            10.0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.0)),
                (BarBeat::new(1, 1, 240), ParamValue::Number(1.0)),
            ],
        );
        stairs(&mut lane);

        let fine = recorded_samples(&lane, FRAMES, 64);
        let coarse = recorded_samples(&lane, FRAMES, 512);
        let first_difference = fine
            .iter()
            .zip(&coarse)
            .position(|(fine, coarse)| fine != coarse);
        assert_eq!(
            first_difference, None,
            "block size must not change the values"
        );

        // The step is at sample 12_000; 10 ms at 48 kHz is 480 samples.
        let step = 1.0 - (-1.0f64).exp();
        let at_tau = fine[12_000 + 479];
        assert!(
            (at_tau - step).abs() < 0.002,
            "63 % point at one time constant: {at_tau}"
        );
        assert!(
            (fine[FRAMES - 1] - 1.0).abs() < 1e-6,
            "settles on the target"
        );
    }

    /// A 1 Hz triangle through the smoother keeps the one-pole's attenuation
    /// of its fundamental.
    #[test]
    fn triangle_through_smoother_has_the_expected_fundamental_attenuation() {
        const RATE: usize = 48_000;
        // One cycle is a second: 0 → 1 → 0 → -1 → 0 in half-beat steps.
        let mut points = Vec::new();
        let mut half_beat = 0usize;
        for bar in 1..=3u32 {
            for beat in 1..=4u32 {
                for subdivision in [0u32, 2] {
                    if bar == 3 && beat == 4 && subdivision == 2 {
                        continue;
                    }
                    let value = [0.0, 1.0, 0.0, -1.0][half_beat % 4];
                    points.push((
                        BarBeat::new(bar, beat, subdivision),
                        ParamValue::Number(value),
                    ));
                    half_beat += 1;
                }
            }
        }
        points.push((BarBeat::new(4, 1, 0), ParamValue::Number(0.0)));
        let lane = lane_with("plugin.Any", 0, 100.0, &points);
        let values = recorded_samples(&lane, RATE * 5, 512);

        // Analyse four whole cycles after one second of settling.
        let window = &values[RATE..RATE * 5];
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (index, value) in window.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * index as f64 / RATE as f64;
            real += value * phase.cos();
            imaginary -= value * phase.sin();
        }
        let measured = 2.0 * (real * real + imaginary * imaginary).sqrt() / window.len() as f64;
        // A peak-1 triangle has an 8/π² fundamental; the exact response of the
        // per-sample one-pole at 1 Hz is (1-a)/sqrt(1 - 2a·cos(ω) + a²).
        let fundamental = 8.0 / (std::f64::consts::PI * std::f64::consts::PI);
        let a = (-1.0 / (0.1 * RATE as f64)).exp();
        let omega = 2.0 * std::f64::consts::PI / RATE as f64;
        let response = (1.0 - a) / (1.0 - 2.0 * a * omega.cos() + a * a).sqrt();
        let expected = fundamental * response;
        assert!(
            (measured - expected).abs() < expected * 0.01,
            "1 Hz fundamental: measured {measured}, expected {expected}"
        );
    }

    /// A staircase with smoothing removes the step discontinuity and differs
    /// measurably from the unsmoothed render.
    #[test]
    fn smoothed_staircase_has_no_click_and_differs_measurably() {
        const FRAMES: usize = 48_000;
        let staircase = |smoothing: f64| {
            format!(
                "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=0\n  automation: gain.factor smoothing={smoothing}ms {{1:1:0=0 curve=stairs, 1:2:0=1}}\n"
            )
        };
        let dry = render_gain(&staircase(0.0), FRAMES, 512);
        let smoothed = render_gain(&staircase(20.0), FRAMES, 512);
        let click = |render: &[f32]| {
            render
                .chunks_exact(2)
                .map(|frame| frame[0])
                .collect::<Vec<_>>()
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0f32, f32::max)
        };
        assert!(
            click(&dry) > 0.9,
            "an unsmoothed staircase really steps: {}",
            click(&dry)
        );
        assert!(
            click(&smoothed) < 0.005,
            "adjacent-sample delta stays below the threshold: {}",
            click(&smoothed)
        );
        let difference: f32 = (dry
            .chunks_exact(2)
            .zip(smoothed.chunks_exact(2))
            .map(|(dry, smoothed)| {
                let delta = dry[0] - smoothed[0];
                delta * delta
            })
            .sum::<f32>()
            / (FRAMES as f32))
            .sqrt();
        assert!(
            difference > 0.05,
            "the smoothed render differs measurably: {difference}"
        );
        assert_eq!(smoothed[0], 0.0);
        assert!((smoothed[(FRAMES - 1) * 2] - 1.0).abs() < 1e-6);
    }

    /// A checked build refuses a lane parameter the device does not expose.
    #[test]
    fn unknown_builtin_parameter_is_rejected_when_building_the_chain() {
        let project = project(
            "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.nope {1:1:0=1}\n",
        );
        let error = AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
            .err()
            .expect("unknown parameter must fail the build");
        assert!(error.contains("has no parameter 'nope'"), "{error}");
    }

    /// Automation does not leak into instrument control lanes.
    #[test]
    fn effect_lanes_do_not_emit_instrument_events() {
        let project = project(
            "@project \"auto\"\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 1:2:0=0.5}\n  @phrase line [1:1 - 2:1]\n    C4 q\n",
        );
        let events = crate::interpret::try_interpret(&project, &Default::default()).unwrap();
        assert!(
            events.iter().all(|event| !matches!(
                &event.event,
                crate::interpret::MidiMessage::PluginParam { .. }
            )),
            "{events:?}"
        );
    }

    /// The shared evaluator reads the same value the renderer applies: the
    /// editor's read-out and the audio cannot disagree at the same position.
    #[test]
    fn shared_value_at_matches_the_rendered_lane_at_ten_positions() {
        use oc_dsl::automation::value_at;
        use oc_dsl::timing::ProjectClock;

        // 120 bpm: one beat is 24 000 samples, so sample / 24 000 is quarters.
        const SAMPLES: [usize; 10] = [
            0, 1_000, 6_000, 12_000, 23_999, 24_000, 30_000, 35_999, 36_000, 47_999,
        ];
        let project = project("@project \"auto\"\n  tempo: 120\n");
        let clock = ProjectClock::new(&project.properties).expect("test clock");
        let read = |lane: &AutomationLane, sample: usize| {
            value_at(lane, &clock, 48_000, sample as f64 / 24_000.0, |value| {
                crate::automation::point_value("filter.cutoff", value, None)
            })
            .expect("lane evaluates")
            .expect("inside the lane")
        };

        for curve in [Curve::Linear, Curve::Stairs, Curve::HalfSine, Curve::Smooth] {
            let mut lane = lane(
                "filter.cutoff",
                0,
                &[
                    (BarBeat::new(1, 1, 0), ParamValue::Number(0.2)),
                    (BarBeat::new(1, 3, 0), ParamValue::Number(0.9)),
                    (BarBeat::new(1, 4, 0), ParamValue::Number(0.4)),
                ],
            );
            for point in &mut lane.points {
                point.curve = curve;
            }
            let rendered = recorded_samples(&lane, 48_000, 512);
            for sample in SAMPLES {
                let expected = read(&lane, sample);
                assert!(
                    (rendered[sample] - expected).abs() < 1e-12,
                    "{curve:?} at {sample}: {} != {expected}",
                    rendered[sample]
                );
            }
        }

        // An LFO reads its absolute phase; the renderer accumulates the phase
        // per sample, so the two agree to floating-point drift.
        let lfo = wobble_lane(LfoSpeed::Hz { hz: 2.0 });
        let rendered = recorded_samples(&lfo, 96_000, 512);
        for sample in SAMPLES {
            let expected = read(&lfo, sample);
            assert!(
                (rendered[sample] - expected).abs() < 1e-9,
                "lfo at {sample}: {} != {expected}",
                rendered[sample]
            );
        }
    }
}
