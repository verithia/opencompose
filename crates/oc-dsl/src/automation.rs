//! Effect-parameter automation lanes: `<effect>.<parameter>` lane names.
//!
//! A lane targets one effect of the owner's own `effects:` list either by
//! effect type (`compression`) or by one-based chain position (`fx2`).

use crate::ast::{
    AutomationLane, BarBeat, Curve, Effect, LaneLfo, LaneMode, LfoSpeed, MAX_LFO_AMOUNT,
    MAX_LFO_MULT, MIN_LFO_SPEED_HZ, ParamValue,
};
use crate::timing::ProjectClock;

impl AutomationLane {
    /// Effect target and parameter name when this is an effect-parameter lane.
    /// Plain instrument lanes return `None`.
    pub fn effect_lane(&self) -> Option<(&str, &str)> {
        effect_lane(&self.param)
    }

    /// True when the point sequence is one cycle of a waveform.
    pub fn is_lfo(&self) -> bool {
        self.mode == LaneMode::Lfo
    }
}

/// Lane name → MIDI controller the interpreter sends for an expression control.
pub fn expression_param_to_cc(param: &str) -> Option<u8> {
    match normalize_param_name(param).as_str() {
        "expression" | "dynamics" | "volume" => Some(11),
        "breath" | "breathiness" => Some(2),
        "brightness" | "timbre" => Some(74),
        "vibrato" | "mod" | "modulation" => Some(1),
        "sustain" | "sustainpedal" | "damper" | "damperpedal" | "halfpedal" => Some(64),
        "sostenuto" | "sostenutopedal" => Some(66),
        "unacorda" | "softpedal" => Some(67),
        _ => None,
    }
}

/// Lane name → MIDI controller for a control lane, including the channel set.
pub fn automation_param_to_cc(param: &str) -> Option<u8> {
    match normalize_param_name(param).as_str() {
        "volume" | "vol" => Some(7),
        "pan" => Some(10),
        other => expression_param_to_cc(other),
    }
}

/// Case- and punctuation-insensitive lane name, so `filter.cutoff`, `Filter
/// Cutoff` and `filter_cutoff` name one control.
pub fn normalize_param_name(param: &str) -> String {
    param
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Encode a patch parameter name as the `pluginparam_<hex>` lane name the
/// interpreter reads, so validation can show the written form.
pub fn encode_plugin_automation_param(param: &str) -> String {
    let mut lane = String::from("pluginparam_");
    for byte in param.bytes() {
        lane.push_str(&format!("{byte:02x}"));
    }
    lane
}

/// Decode a `pluginparam_<hex>` lane back to its parameter name.
pub fn decode_plugin_automation_param(lane: &str) -> Option<String> {
    let encoded = lane.strip_prefix("pluginparam_")?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

/// Check the cycle controls of a lane: ranges only, so the DSL validator and
/// the editor share one rule set. The default cycle is always valid.
pub fn validate_lane_lfo(lfo: &LaneLfo) -> Result<(), String> {
    match lfo.speed {
        LfoSpeed::Hz { hz } => {
            if !hz.is_finite() || !(MIN_LFO_SPEED_HZ..=1000.0).contains(&hz) {
                return Err(format!(
                    "LFO speed must be within {MIN_LFO_SPEED_HZ}..=1000 Hz"
                ));
            }
        }
        LfoSpeed::Note {
            numerator,
            denominator,
        } => {
            if numerator == 0 || denominator == 0 || numerator > 64 || denominator > 64 {
                return Err("LFO note speed must be a fraction with terms in 1..=64".into());
            }
        }
    }
    if !lfo.mult.is_finite() || lfo.mult <= 0.0 || lfo.mult > MAX_LFO_MULT {
        return Err(format!("LFO cycles must be within 0..={MAX_LFO_MULT}"));
    }
    if !(lfo.pulse_width.is_finite() && 0.0 < lfo.pulse_width && lfo.pulse_width < 1.0) {
        return Err("LFO pulse width must be inside 0..1".into());
    }
    if !lfo.skew.is_finite() || !(-1.0..=1.0).contains(&lfo.skew) {
        return Err("LFO skew must be within -1..=1".into());
    }
    if !lfo.tension.is_finite() || !(0.0..=1.0).contains(&lfo.tension) {
        return Err("LFO tension must be within 0..=1".into());
    }
    if !lfo.amount.is_finite() || lfo.amount < 0.0 || lfo.amount > MAX_LFO_AMOUNT {
        return Err(format!("LFO amount must be within 0..={MAX_LFO_AMOUNT}"));
    }
    Ok(())
}

/// Split `<effect>.<parameter>`; plain instrument lanes return `None`.
pub fn effect_lane(param: &str) -> Option<(&str, &str)> {
    param.split_once('.')
}

/// Resolve a lane target against one owner's declared effects.
pub fn resolve_effect(effects: &[Effect], target: &str) -> Result<usize, String> {
    if let Some(rest) = target.strip_prefix("fx")
        && !rest.is_empty()
        && rest.bytes().all(|byte| byte.is_ascii_digit())
    {
        let index = rest
            .parse::<usize>()
            .map_err(|_| format!("effect index {rest} is out of range"))?;
        if index == 0 || index > effects.len() {
            return Err(format!(
                "effect index {index} is out of range; the chain has {} effect(s)",
                effects.len()
            ));
        }
        return Ok(index - 1);
    }

    let matches: Vec<usize> = effects
        .iter()
        .enumerate()
        .filter(|(_, effect)| effect.effect_type.lane_name() == target)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] if effects.is_empty() => Err(format!(
            "unknown effect '{target}'; the chain has no effects"
        )),
        [] => Err(format!(
            "unknown effect '{target}'; the chain has: {}",
            target_names(effects)
        )),
        [first, .., last] => Err(format!(
            "effect '{target}' matches {} chain entries; address one by position (fx{}..fx{})",
            matches.len(),
            first + 1,
            last + 1
        )),
    }
}

fn target_names(effects: &[Effect]) -> String {
    effects
        .iter()
        .map(|effect| effect.effect_type.lane_name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check that a built-in effect exposes `param`; plugin parameters are the
/// plugin's own contract and are accepted here.
pub fn validate_device_param(effect: &Effect, param: &str) -> Result<(), String> {
    let Some(device) = effect
        .effect_type
        .device_id()
        .and_then(oc_device_schema::find_device)
    else {
        return Ok(());
    };
    if device.params.iter().any(|schema| schema.id == param) {
        return Ok(());
    }
    let expected = device
        .params
        .iter()
        .map(|schema| schema.id)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "effect '{}' has no parameter '{param}'; expected one of: {expected}",
        device.id
    ))
}

// ============================================================================
// Compiled lanes and the one evaluator
// ============================================================================

/// One point of a compiled lane, in the lane's own coordinate system.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LanePoint {
    /// Absolute samples for an envelope; cycle phase in 0..1 for an LFO.
    pub position: f64,
    /// The point's value in the caller's parameter domain.
    pub value: f64,
    pub curve: Curve,
    /// Segment tension in 0..1, already defaulted to `1.0`.
    pub tension: f64,
}

/// The cycle controls of an LFO lane, with the shape's midpoint and origin.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LfoShape {
    /// Cycle length in quarter notes when the rate follows the tempo.
    cycle_quarters: Option<f64>,
    /// Cycles per second when the rate is absolute.
    hz: Option<f64>,
    mult: f64,
    pulse_width: f64,
    skew: f64,
    tension: f64,
    amount: f64,
    /// Midpoint of the authored values; `amount` scales around it.
    center: f64,
    /// Position of the first point, which is phase 0.
    origin_sample: u64,
    origin_quarter: f64,
}

/// One lane compiled for evaluation, independent of any parameter domain.
///
/// The caller converts each authored point into the parameter's own unit, so a
/// renderer and an editor read-out interpolate through the same code. Smoothing
/// is declared here and applied by the caller: the per-sample one-pole state
/// belongs to whoever renders, not to this stateless value.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneCurve {
    points: Vec<LanePoint>,
    mode: LaneMode,
    lfo: Option<LfoShape>,
    smoothing_ms: f64,
    sample_rate: u32,
    /// Sample span a clip writes in, `[start, end)`. `None` writes from the
    /// first point on; either way the value holds after the last point.
    span: Option<(f64, f64)>,
}

/// One point before the lane chooses its coordinate system.
struct RawLanePoint {
    quarter: f64,
    sample: u64,
    value: f64,
    curve: Curve,
    tension: f64,
}

/// A clip's span: where it starts in both coordinates and where it stops.
///
/// The end is exclusive, so a clip and the next clip may share an edge.
struct ClipSpan {
    start_quarter: f64,
    start_sample: u64,
    end_sample: u64,
}

/// Musical position of one lane point, honouring the lane's own grid.
pub fn lane_quarter(
    clock: &ProjectClock,
    lane: &AutomationLane,
    position: &BarBeat,
) -> Result<f64, String> {
    if lane.ppq == 0 {
        clock.quarter_notes_at(position)
    } else {
        clock.quarter_notes_at_ppq(position, lane.ppq)
    }
}

impl LaneCurve {
    /// Compile `lane` against `clock`; `value_of` converts one authored point
    /// into the unit the parameter receives.
    pub fn new<F>(
        lane: &AutomationLane,
        clock: &ProjectClock,
        sample_rate: u32,
        mut value_of: F,
    ) -> Result<Self, String>
    where
        F: FnMut(&ParamValue) -> Result<f64, String>,
    {
        if sample_rate == 0 {
            return Err("sample rate must be positive".into());
        }
        if lane.is_lfo() {
            validate_lane_lfo(&lane.lfo)?;
        }
        // A clip's span is authoritative: it is the only thing that decides
        // where the lane stops writing.
        let clip = match &lane.time_range {
            Some(range) => {
                let start_quarter = lane_quarter(clock, lane, &range.start)?;
                let start_sample = clock.sample_at_quarter(start_quarter, sample_rate)?;
                let end_sample =
                    clock.sample_at_quarter(lane_quarter(clock, lane, &range.end)?, sample_rate)?;
                if end_sample <= start_sample {
                    return Err(format!(
                        "automation lane '{}' must end after its start",
                        lane.param
                    ));
                }
                Some(ClipSpan {
                    start_quarter,
                    start_sample,
                    end_sample,
                })
            }
            None => None,
        };
        let mut raw = Vec::with_capacity(lane.points.len());
        for point in &lane.points {
            let quarter = lane_quarter(clock, lane, &point.position)?;
            raw.push(RawLanePoint {
                quarter,
                sample: clock.sample_at_quarter(quarter, sample_rate)?,
                value: value_of(&point.value)?,
                curve: point.curve,
                tension: point.tension.unwrap_or(1.0),
            });
        }
        let (points, lfo) = if lane.is_lfo() {
            // A clip restarts its cycle at its head, not at its first point.
            lfo_parts(
                raw,
                &lane.lfo,
                clip.as_ref()
                    .map(|clip| (clip.start_quarter, clip.start_sample)),
            )
        } else {
            (envelope_points(raw), None)
        };
        Ok(Self {
            points,
            mode: lane.mode,
            lfo,
            smoothing_ms: lane.smoothing_ms,
            sample_rate,
            span: clip.map(|clip| (clip.start_sample as f64, clip.end_sample as f64)),
        })
    }

    /// True when the lane repeats one cycle instead of holding an envelope.
    pub fn is_lfo(&self) -> bool {
        self.mode == LaneMode::Lfo
    }

    /// The lane's automation mode.
    pub fn mode(&self) -> LaneMode {
        self.mode
    }

    /// The compiled points, in evaluation order.
    pub fn points(&self) -> &[LanePoint] {
        &self.points
    }

    /// The lane's one-pole time constant in milliseconds; 0 keeps raw values.
    pub fn smoothing_ms(&self) -> f64 {
        self.smoothing_ms
    }

    /// Fraction of the remaining distance covered per sample, or `None` when
    /// the lane declares no smoothing.
    pub fn smoothing_alpha(&self) -> Option<f64> {
        (self.smoothing_ms.is_finite() && self.smoothing_ms > 0.0)
            .then(|| one_pole_alpha(self.smoothing_ms, 1.0, f64::from(self.sample_rate)))
    }

    /// True when the LFO's cycle length is in quarter notes.
    pub fn lfo_follows_tempo(&self) -> bool {
        self.lfo
            .as_ref()
            .is_some_and(|lfo| lfo.cycle_quarters.is_some())
    }

    /// Quarter notes a tempo-following LFO advances per sample.
    pub fn lfo_quarter_step(&self, tempo: f64, sample_rate: u32) -> f64 {
        if self.lfo_follows_tempo() {
            tempo / 60.0 / f64::from(sample_rate)
        } else {
            0.0
        }
    }

    /// Envelope value at an absolute sample.
    ///
    /// `None` means the lane writes nothing: outside a clip's span, or before
    /// the first point of a lane that has no span. Inside a span the first
    /// point is held from the clip's head, so a clip whose points start later
    /// still writes its opening value there.
    pub fn envelope_value(&self, sample: f64) -> Option<f64> {
        if self.mode != LaneMode::Envelope {
            return None;
        }
        if let Some((start, end)) = self.span
            && (sample < start || sample >= end)
        {
            return None;
        }
        match point_value(&self.points, sample, 0.0) {
            Some(value) => Some(value),
            None if self.span.is_some() => self.points.first().map(|point| point.value),
            None => None,
        }
    }

    /// Cycle count of an LFO at `sample`; `None` where the lane writes nothing.
    ///
    /// A clip's cycle starts at its head, so `time_range.start` replaces the
    /// first point as the phase origin and a repeated clip restarts in phase.
    pub fn lfo_cycles(&self, quarter: f64, sample: u64) -> Option<f64> {
        let lfo = self.lfo.as_ref()?;
        match self.span {
            Some((start, end)) if (sample as f64) < start || (sample as f64) >= end => return None,
            Some(_) => {}
            None if sample < lfo.origin_sample => return None,
            None => {}
        }
        Some(match (lfo.cycle_quarters, lfo.hz) {
            (Some(cycle), _) => (quarter - lfo.origin_quarter) / cycle * lfo.mult,
            (_, Some(hz)) => {
                (sample - lfo.origin_sample) as f64 / f64::from(self.sample_rate) * hz * lfo.mult
            }
            _ => 0.0,
        })
    }

    /// LFO value for a cycle count: the shape at that phase, scaled around the
    /// shape's midpoint by the lane's amount.
    pub fn lfo_value(&self, cycles: f64) -> Option<f64> {
        let lfo = self.lfo.as_ref()?;
        let phase = warped_phase(cycles.max(0.0), lfo.skew, lfo.pulse_width);
        let value = point_value(&self.points, phase, lfo.tension)?;
        Some(lfo.center + (value - lfo.center) * lfo.amount)
    }
}

/// Stateless value of `lane` at `quarter`, before the renderer's smoothing.
///
/// `value_of` converts one authored point into the parameter's own unit, so a
/// renderer and a read-only caller agree at the same position. Returns `None`
/// wherever the lane writes nothing: a muted lane, before the first point of a
/// lane without a range, or outside a clip's span.
pub fn value_at<F>(
    lane: &AutomationLane,
    clock: &ProjectClock,
    sample_rate: u32,
    quarter: f64,
    value_of: F,
) -> Result<Option<f64>, String>
where
    F: FnMut(&ParamValue) -> Result<f64, String>,
{
    if lane.muted {
        return Ok(None);
    }
    let curve = LaneCurve::new(lane, clock, sample_rate, value_of)?;
    let sample = clock.sample_at_quarter(quarter, sample_rate)?;
    Ok(match curve.mode() {
        LaneMode::Envelope => curve.envelope_value(sample as f64),
        LaneMode::Lfo => curve
            .lfo_cycles(quarter, sample)
            .and_then(|cycles| curve.lfo_value(cycles)),
    })
}

/// Value of a sorted point table at `position`; `ease` warps the segment
/// progress towards a smoothstep before the segment's own curve applies.
fn point_value(points: &[LanePoint], position: f64, ease: f64) -> Option<f64> {
    let first = points.first()?;
    if first.position > position {
        return None;
    }
    let index = points
        .partition_point(|point| point.position <= position)
        .saturating_sub(1);
    let point = &points[index];
    let Some(next) = points.get(index + 1) else {
        return Some(point.value);
    };
    let span = next.position - point.position;
    let progress = (position - point.position) / span;
    let progress = if ease == 0.0 {
        progress
    } else {
        progress + (progress * progress * (3.0 - 2.0 * progress) - progress) * ease
    };
    Some(point.value + (next.value - point.value) * shape(point.curve, point.tension, progress))
}

/// Envelope points: absolute samples, one value per sample position.
fn envelope_points(mut raw: Vec<RawLanePoint>) -> Vec<LanePoint> {
    raw.sort_by_key(|point| point.sample);
    let mut unique: Vec<LanePoint> = Vec::with_capacity(raw.len());
    for point in raw {
        let point = LanePoint {
            position: point.sample as f64,
            value: point.value,
            curve: point.curve,
            tension: point.tension,
        };
        match unique.last_mut() {
            Some(last) if last.position == point.position => *last = point,
            _ => unique.push(point),
        }
    }
    unique
}

/// LFO points and cycle controls: each point's musical distance from the first
/// is its phase, and the shape scales around the authored values' midpoint.
///
/// `cycle_origin` moves the phase origin without moving the shape: a clip
/// passes its head, so the cycle restarts there while the authored points still
/// span one cycle.
fn lfo_parts(
    mut raw: Vec<RawLanePoint>,
    lfo: &LaneLfo,
    cycle_origin: Option<(f64, u64)>,
) -> (Vec<LanePoint>, Option<LfoShape>) {
    raw.sort_by(|a, b| a.quarter.total_cmp(&b.quarter));
    let mut unique: Vec<RawLanePoint> = Vec::with_capacity(raw.len());
    for point in raw {
        match unique.last_mut() {
            Some(last) if last.quarter == point.quarter => *last = point,
            _ => unique.push(point),
        }
    }
    let first_quarter = unique.first().map_or(0.0, |point| point.quarter);
    let first_sample = unique.first().map_or(0, |point| point.sample);
    let span = unique.last().map_or(0.0, |point| point.quarter) - first_quarter;
    let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in &unique {
        low = low.min(point.value);
        high = high.max(point.value);
    }
    let center = if unique.is_empty() {
        0.0
    } else {
        0.5 * (low + high)
    };
    let points = unique
        .into_iter()
        .map(|point| LanePoint {
            position: if span > 0.0 {
                (point.quarter - first_quarter) / span
            } else {
                0.0
            },
            value: point.value,
            curve: point.curve,
            tension: point.tension,
        })
        .collect();
    let (cycle_quarters, hz) = match lfo.speed {
        LfoSpeed::Hz { hz } => (None, Some(hz)),
        LfoSpeed::Note { .. } => (lfo.speed.cycle_quarters(), None),
    };
    // The origin is musical, but a Hz cycle is measured in samples, so a clip
    // passes both of its head's coordinates.
    let (origin_quarter, origin_sample) = cycle_origin.unwrap_or((first_quarter, first_sample));
    (
        points,
        Some(LfoShape {
            cycle_quarters,
            hz,
            mult: lfo.mult,
            pulse_width: lfo.pulse_width,
            skew: lfo.skew,
            tension: lfo.tension,
            amount: lfo.amount,
            center,
            origin_sample,
            origin_quarter,
        }),
    )
}

/// Segment shape at `position` in 0..1, from this point to the next.
pub fn shape(curve: Curve, tension: f64, position: f64) -> f64 {
    match curve {
        Curve::Linear => position,
        Curve::Stairs => 0.0,
        Curve::HalfSine => 0.5 - 0.5 * (std::f64::consts::PI * position).cos(),
        Curve::Smooth => {
            let smoothstep = position * position * (3.0 - 2.0 * position);
            position + (smoothstep - position) * tension
        }
    }
}

/// Cycle phase after the skew bend and the pulse-width split, in 0..1.
///
/// `skew` raises the phase to `2^skew`, which moves the shape's motion earlier
/// (negative) or later (positive) without moving its endpoints. `pulse_width`
/// then maps the shape's first half into the first `pulse_width` of the cycle.
pub fn warped_phase(total: f64, skew: f64, pulse_width: f64) -> f64 {
    let phase = total - total.floor();
    let phase = if skew == 0.0 {
        phase
    } else {
        phase.powf(2f64.powf(skew))
    };
    if pulse_width == 0.5 {
        phase
    } else if phase < pulse_width {
        phase * 0.5 / pulse_width
    } else {
        0.5 + (phase - pulse_width) * 0.5 / (1.0 - pulse_width)
    }
}

/// Fraction of the remaining distance to the goal covered by `frames` samples
/// with a one-pole time constant of `smoothing_ms`.
///
/// `1 - exp(-frames / (tau * sample_rate))` with `tau = smoothing_ms / 1000`.
/// A non-positive `smoothing_ms` returns `1.0`: the goal is taken in one step,
/// which is the hard over a disabled smoother means. `frames == 0` or a
/// non-positive sample rate returns `0.0`: no time passed, no movement.
pub fn one_pole_alpha(smoothing_ms: f64, frames: f64, sample_rate: f64) -> f64 {
    if smoothing_ms.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return 1.0;
    }
    if frames.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        || sample_rate.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
    {
        return 0.0;
    }
    let seconds = smoothing_ms / 1000.0;
    1.0 - (-frames / (seconds * sample_rate)).exp()
}

/// Relative gap below which a lane smoother snaps onto its target.
const SETTLE_EPSILON: f64 = 1e-9;

/// One step of a lane's one-pole smoother towards `target`.
pub fn smooth_step(previous: f64, target: f64, alpha: f64) -> f64 {
    if (target - previous).abs() > SETTLE_EPSILON * target.abs().max(1.0) {
        previous + (target - previous) * alpha
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EffectType, ParamValue};

    fn effect(effect_type: EffectType) -> Effect {
        Effect {
            effect_type,
            params: Vec::new(),
            bypass: false,
            sidechain_source: None,
        }
    }

    #[test]
    fn chain_positions_are_one_based_and_bounded() {
        let effects = [effect(EffectType::Gain), effect(EffectType::Eq)];
        assert_eq!(resolve_effect(&effects, "fx1"), Ok(0));
        assert_eq!(resolve_effect(&effects, "fx2"), Ok(1));
        assert!(
            resolve_effect(&effects, "fx0")
                .unwrap_err()
                .contains("out of range")
        );
        assert!(
            resolve_effect(&effects, "fx3")
                .unwrap_err()
                .contains("out of range")
        );
        assert!(
            resolve_effect(&effects, "fx")
                .unwrap_err()
                .contains("unknown effect")
        );
    }

    #[test]
    fn type_names_resolve_only_when_unique() {
        let effects = [
            effect(EffectType::Gain),
            effect(EffectType::Compression),
            effect(EffectType::Compression),
        ];
        assert_eq!(resolve_effect(&effects, "gain"), Ok(0));
        let ambiguous = resolve_effect(&effects, "compression").unwrap_err();
        assert!(ambiguous.contains("matches 2"), "{ambiguous}");
        assert!(ambiguous.contains("fx2..fx3"), "{ambiguous}");
        let unknown = resolve_effect(&effects, "verb").unwrap_err();
        assert!(
            unknown.contains("chain has: gain, compression, compression"),
            "{unknown}"
        );
    }

    #[test]
    fn built_in_parameters_come_from_the_device_schema() {
        let gain = effect(EffectType::Gain);
        assert!(validate_device_param(&gain, "factor").is_ok());
        let error = validate_device_param(&gain, "mix").unwrap_err();
        assert!(error.contains("has no parameter 'mix'"), "{error}");
        assert!(error.contains("factor"), "{error}");
    }

    #[test]
    fn plugin_parameters_are_not_checked_here() {
        let plugin = effect(EffectType::Plugin("vst3.example".into()));
        assert!(validate_device_param(&plugin, "Whatever").is_ok());
    }

    #[test]
    fn lane_names_round_trip_through_the_effect_lane_splitter() {
        let lane = AutomationLane {
            param: "compression.threshold".into(),
            ppq: 0,
            smoothing_ms: 0.0,
            points: vec![crate::ast::AutomationPoint {
                position: crate::ast::BarBeat::new(1, 1, 0),
                value: ParamValue::Db(-24.0),
                curve: crate::ast::Curve::Linear,
                tension: None,
            }],
            ..Default::default()
        };
        assert_eq!(lane.effect_lane(), Some(("compression", "threshold")));
        let plain = AutomationLane {
            param: "brightness".into(),
            ppq: 0,
            smoothing_ms: 0.0,
            points: Vec::new(),
            ..Default::default()
        };
        assert_eq!(plain.effect_lane(), None);
    }

    // ============================================================================
    // Evaluator
    // ============================================================================

    /// A 120 bpm 4/4 clock: one beat is one quarter note and 24 000 samples.
    fn evaluator_clock() -> ProjectClock {
        let project = crate::parse("@project \"eval\"\n  tempo: 120\n  time_signature: 4/4\n")
            .expect("test project parses");
        ProjectClock::new(&project.properties).expect("test clock")
    }

    /// A lane whose points sit on consecutive beats of bar 1.
    fn beat_lane(points: &[(u32, f64)]) -> AutomationLane {
        AutomationLane {
            param: "filter.cutoff".into(),
            ppq: 0,
            smoothing_ms: 0.0,
            points: points
                .iter()
                .map(|(beat, value)| crate::ast::AutomationPoint {
                    position: BarBeat::new(1, *beat, 0),
                    value: ParamValue::Number(*value),
                    curve: Curve::Linear,
                    tension: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn convert(value: &ParamValue) -> Result<f64, String> {
        value
            .as_f64()
            .ok_or_else(|| format!("{value:?} is not numeric"))
    }

    /// Value of `lane` at a quarter-note position, in the lane's own units.
    fn read(lane: &AutomationLane, quarter: f64) -> Option<f64> {
        value_at(lane, &evaluator_clock(), 48_000, quarter, convert).expect("lane evaluates")
    }

    #[test]
    fn every_curve_shape_has_its_closed_form_at_the_same_position() {
        // 0 -> 1 over one beat; a quarter of the segment is 6 000 samples.
        let cases = [
            (Curve::Linear, 0.25),
            (Curve::Stairs, 0.0),
            (
                Curve::HalfSine,
                0.5 - 0.5 * (std::f64::consts::PI * 0.25).cos(),
            ),
            (Curve::Smooth, 0.15625),
        ];
        for (curve, expected) in cases {
            let mut lane = beat_lane(&[(1, 0.0), (2, 1.0)]);
            lane.points[0].curve = curve;
            let value = read(&lane, 0.25).expect("inside the lane");
            assert!(
                (value - expected).abs() < 1e-12,
                "{curve:?}: {value} != {expected}"
            );
        }
    }

    #[test]
    fn per_point_tension_blends_only_the_smooth_curve() {
        let mut smooth = beat_lane(&[(1, 0.0), (2, 1.0)]);
        smooth.points[0].curve = Curve::Smooth;
        smooth.points[0].tension = Some(0.5);
        let value = read(&smooth, 0.25).expect("inside the lane");
        assert!((value - 0.203125).abs() < 1e-12, "{value}");

        // A tension on a linear point does not bend it.
        let mut linear = beat_lane(&[(1, 0.0), (2, 1.0)]);
        linear.points[0].tension = Some(0.5);
        let value = read(&linear, 0.25).expect("inside the lane");
        assert!((value - 0.25).abs() < 1e-12, "{value}");
    }

    #[test]
    fn one_pole_alpha_keeps_unordered_and_disabled_clock_semantics() {
        assert_eq!(one_pole_alpha(f64::NAN, 1.0, 48_000.0), 1.0);
        assert_eq!(one_pole_alpha(0.0, 0.0, 0.0), 1.0);
        assert_eq!(one_pole_alpha(10.0, f64::NAN, 48_000.0), 0.0);
        assert_eq!(one_pole_alpha(10.0, 1.0, f64::NAN), 0.0);
        assert_eq!(one_pole_alpha(f64::INFINITY, 1.0, 48_000.0), 0.0);
        assert_eq!(one_pole_alpha(10.0, f64::INFINITY, 48_000.0), 1.0);
    }

    #[test]
    fn the_lane_declares_the_smoothing_the_renderer_applies() {
        let mut lane = beat_lane(&[(1, 0.0), (2, 1.0)]);
        lane.smoothing_ms = 10.0;
        let curve = LaneCurve::new(&lane, &evaluator_clock(), 48_000, convert).expect("compiles");
        assert_eq!(curve.smoothing_ms(), 10.0);
        let alpha = curve.smoothing_alpha().expect("smoothed lane");
        let expected = 1.0 - (-1.0f64 / (0.01 * 48_000.0)).exp();
        assert!((alpha - expected).abs() < 1e-15, "{alpha} != {expected}");

        // One time constant is 63.2 % of the distance, stepped with the shared law.
        let mut value = 0.0;
        for _ in 0..480 {
            value = smooth_step(value, 1.0, alpha);
        }
        assert!((value - (1.0 - (-1.0f64).exp())).abs() < 1e-12, "{value}");

        // A gap below the settle epsilon snaps exactly onto the target.
        let settled = 1.0 + 1e-12;
        assert_eq!(smooth_step(1.0, settled, alpha), settled);

        // No declared smoothing means the renderer takes the value in one step.
        let raw = beat_lane(&[(1, 0.0), (2, 1.0)]);
        let curve = LaneCurve::new(&raw, &evaluator_clock(), 48_000, convert).expect("compiles");
        assert_eq!(curve.smoothing_alpha(), None);
    }

    #[test]
    fn an_lfo_cycle_reads_the_same_shape_at_a_phase() {
        let mut lane = beat_lane(&[(1, 0.0), (2, 1.0), (3, 0.0), (4, -1.0)]);
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = LfoSpeed::Hz { hz: 1.0 };
        // 1 Hz at 120 bpm: one cycle is 48 000 samples, so a quarter beat is 1/8.
        let value = read(&lane, 0.25).expect("inside the lane");
        assert!((value - 0.375).abs() < 1e-12, "{value}");
    }

    #[test]
    fn an_lfo_note_speed_follows_the_clock() {
        let mut lane = beat_lane(&[(1, 0.0), (2, 1.0), (3, 0.0), (4, -1.0)]);
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = LfoSpeed::Note {
            numerator: 1,
            denominator: 4,
        };
        // One cycle per quarter note: a quarter beat is phase 0.25.
        let value = read(&lane, 0.25).expect("inside the lane");
        assert!((value - 0.75).abs() < 1e-12, "{value}");

        let curve = LaneCurve::new(&lane, &evaluator_clock(), 48_000, convert).expect("compiles");
        assert!(curve.lfo_follows_tempo());
        assert!((curve.lfo_quarter_step(120.0, 48_000) - 1.0 / 24_000.0).abs() < 1e-15);
    }

    #[test]
    fn lfo_controls_warp_the_same_cycle_without_moving_the_origin() {
        let mut lane = beat_lane(&[(1, 0.0), (2, 1.0), (3, 0.0), (4, -1.0)]);
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = LfoSpeed::Hz { hz: 1.0 };
        let plain = read(&lane, 0.25).expect("inside the lane");

        // Tension eases the phase towards a smoothstep: 0.375 -> 0.31640625.
        lane.lfo.tension = 1.0;
        let tense = read(&lane, 0.25).expect("inside the lane");
        assert!((tense - 0.31640625).abs() < 1e-12, "{tense}");
        lane.lfo.tension = 0.0;

        // Amount scales around the authored midpoint, which is 0 here.
        lane.lfo.amount = 0.5;
        let quiet = read(&lane, 0.25).expect("inside the lane");
        assert!((quiet - plain * 0.5).abs() < 1e-12, "{quiet}");
        lane.lfo.amount = 1.0;

        // Pulse width maps the shape's first half into the first quarter of the
        // cycle: phase 0.125 becomes 0.25, and the segment progress 0.75.
        lane.lfo.pulse_width = 0.25;
        let narrow = read(&lane, 0.25).expect("inside the lane");
        assert!((narrow - 0.75).abs() < 1e-12, "{narrow}");
        lane.lfo.pulse_width = 0.5;

        // Skew raises the phase to a power: 0.125^2, so progress 0.046875.
        lane.lfo.skew = 1.0;
        let bent = read(&lane, 0.25).expect("inside the lane");
        assert!((bent - 0.046875).abs() < 1e-12, "{bent}");
    }

    #[test]
    fn the_lane_holds_its_endpoints_and_writes_nothing_before_them() {
        let lane = beat_lane(&[(2, 0.25), (3, 0.75)]);
        // Before the first point the renderer writes nothing.
        assert_eq!(read(&lane, 0.5), None);
        assert_eq!(read(&lane, 1.0), Some(0.25));
        assert_eq!(read(&lane, 1.5), Some(0.5));
        assert_eq!(read(&lane, 2.0), Some(0.75));
        // After the last point the last value holds.
        assert_eq!(read(&lane, 9.0), Some(0.75));
    }

    #[test]
    fn an_lfo_holds_nothing_before_its_first_point() {
        let mut lane = beat_lane(&[(2, 0.0), (3, 1.0), (4, 0.0)]);
        lane.mode = LaneMode::Lfo;
        lane.lfo.speed = LfoSpeed::Hz { hz: 1.0 };
        assert_eq!(read(&lane, 0.5), None);
        assert_eq!(read(&lane, 1.0), Some(0.0));
    }

    #[test]
    fn a_lane_whose_values_do_not_convert_is_refused() {
        let mut lane = beat_lane(&[(1, 0.0), (2, 1.0)]);
        lane.points[0].value = ParamValue::String("wide".into());
        let error = value_at(&lane, &evaluator_clock(), 48_000, 0.5, convert).unwrap_err();
        assert!(error.contains("wide"), "{error}");
    }

    /// A clip writes only inside its span: nothing before its head, the first
    /// point held from the head on, the last value held to the exclusive end,
    /// and nothing after it.
    #[test]
    fn a_ranged_lane_writes_only_inside_its_span() {
        let mut lane = AutomationLane {
            param: "filter.cutoff".into(),
            time_range: Some(crate::ast::TimeRange {
                // Bar 1 beat 3 to bar 3 beat 1: quarters 2 to 8.
                start: BarBeat::new(1, 3, 0),
                end: BarBeat::new(3, 1, 0),
            }),
            points: [(BarBeat::new(2, 2, 0), 0.2), (BarBeat::new(2, 4, 0), 0.8)]
                .into_iter()
                .map(|(position, value)| crate::ast::AutomationPoint {
                    position,
                    value: ParamValue::Number(value),
                    curve: Curve::Linear,
                    tension: None,
                })
                .collect(),
            ..Default::default()
        };
        // 1:2 and 2:1 are before the span, where nothing is written.
        assert_eq!(read(&lane, 1.0), None);
        assert_eq!(read(&lane, 1.5), None);
        // 1:3 is the head: the first point is later, so it is held here.
        assert_eq!(read(&lane, 2.0), Some(0.2));
        // 2:2 and 2:3 lie between the two points: 0.2 then halfway to 0.8.
        assert_eq!(read(&lane, 5.0), Some(0.2));
        let halfway = read(&lane, 6.0).expect("inside the span");
        assert!((halfway - 0.5).abs() < 1e-12, "{halfway}");
        // 2:4 is the last point, and the value holds to the end of the span.
        assert_eq!(read(&lane, 7.0), Some(0.8));
        assert_eq!(read(&lane, 7.9), Some(0.8));
        // 3:1 is the exclusive end; the renderer stops writing there, so a
        // read-out reports no lane value even though the last value stays.
        assert_eq!(read(&lane, 8.0), None);
        assert_eq!(read(&lane, 20.0), None);

        lane.muted = true;
        assert_eq!(read(&lane, 5.0), None);
    }

    /// A clip's LFO cycle starts at the clip head, not at the first point.
    #[test]
    fn an_lfo_clip_starts_its_cycle_at_its_head() {
        let points = [(BarBeat::new(1, 1, 0), 0.0), (BarBeat::new(2, 1, 0), 1.0)]
            .into_iter()
            .map(|(position, value)| crate::ast::AutomationPoint {
                position,
                value: ParamValue::Number(value),
                curve: Curve::Linear,
                tension: None,
            })
            .collect();
        let mut lane = AutomationLane {
            param: "filter.cutoff".into(),
            mode: LaneMode::Lfo,
            points,
            ..Default::default()
        };
        lane.lfo.speed = LfoSpeed::Hz { hz: 1.0 };
        // One cycle is 2 quarters at 120 bpm, so quarter 5 is 2.5 cycles from
        // the first point: phase 0.5 without a span.
        assert_eq!(read(&lane, 5.0), Some(0.5));

        // The same lane as a clip starting at 2:2 restarts its phase there.
        lane.time_range = Some(crate::ast::TimeRange {
            start: BarBeat::new(2, 2, 0),
            end: BarBeat::new(4, 1, 0),
        });
        assert_eq!(read(&lane, 5.0), Some(0.0));
        assert_eq!(read(&lane, 5.25), Some(0.125));
        // Nothing is written before the head, and the cycle keeps running after
        // the shape's own points: 2:4 is one cycle after the head.
        assert_eq!(read(&lane, 4.9), None);
        assert_eq!(read(&lane, 7.0), Some(0.0));
    }
}
