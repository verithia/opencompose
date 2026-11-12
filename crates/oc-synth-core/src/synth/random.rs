//! Constrained patch randomisation: a nearby new direction, not noise.
//!
//! The safety set is `CONSTRAINTS`: the draw and the clamp both read that
//! table, so a new bound is one row and cannot drift between call sites.
//! Randomisation happens at edit time, never on the audio path.
//!
//! Determinism: each parameter draws from its own SplitMix64 stream keyed by
//! `seed ^ hash(param)`, so the same seed always rebuilds the same patch and a
//! new row cannot reshuffle the other fields.

use oc_dsl::ast::ParamValue;

/// How one parameter may move around its current value.
#[derive(Debug, Clone, Copy)]
pub enum Rule {
    /// Pick one of these numeric ids.
    Choice(&'static [f64]),
    /// Re-pick only within the current peak class; a class outside `draw`
    /// (a sine sub, a square) is a deliberate character and stays put.
    Waveform {
        draw: &'static [f64],
        allowed: &'static [f64],
    },
    /// Multiply the current value by `2^d`. `draw` is the typical step and
    /// `span` the safety bound the clamp enforces.
    Log {
        span: f64,
        draw: f64,
        min: f64,
        max: f64,
    },
    /// Add `d` uniform in `[-delta, delta]`, clamp, then round to `step`.
    Linear {
        delta: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    /// Never randomised; the current value is clamped into `[min, max]`.
    Keep { min: f64, max: f64 },
    /// Re-pick a `working` catalog id in the current family. `safe` lists the
    /// DC-free ids a draw may introduce; an unsafe current id stays put.
    CatalogWarp { safe: &'static [f64] },
}

/// One row of the safety set.
#[derive(Debug, Clone, Copy)]
pub struct Constraint {
    pub param: &'static str,
    pub rule: Rule,
    /// Voice default used when the preset does not name the parameter.
    pub default: f64,
    /// Relative weight in the change plan; `Keep` rows use `0.0`.
    pub chance: f64,
    pub why: &'static str,
}

/// The safety set for a nearby variation. Bounds are deliberately narrower
/// than the engine's own ranges; see the notes for the measurement behind them.
pub const CONSTRAINTS: &[Constraint] = &[
    Constraint {
        param: "osc.a.waveform",
        rule: Rule::Waveform {
            draw: &[1.0, 3.0],
            allowed: &[1.0, 2.0, 3.0],
        },
        default: 1.0,
        chance: 0.0,
        why: "saw/square/triangle are the only safe draws; a source outside the set stays put",
    },
    Constraint {
        param: "osc.b.waveform",
        rule: Rule::Waveform {
            draw: &[1.0, 3.0],
            allowed: &[1.0, 2.0, 3.0],
        },
        default: 2.0,
        chance: 0.0,
        why: "saw/square/triangle are the only safe draws; a source outside the set stays put",
    },
    Constraint {
        param: "osc.c.waveform",
        rule: Rule::Waveform {
            draw: &[1.0, 3.0],
            allowed: &[1.0, 2.0, 3.0],
        },
        default: 0.0,
        chance: 0.0,
        why: "saw/square/triangle are the only safe draws; a source outside the set stays put",
    },
    Constraint {
        param: "filter.model",
        rule: Rule::Keep {
            min: 0.0,
            max: 11.0,
        },
        default: 0.0,
        chance: 0.0,
        why: "topology is a character decision; randomising it can reach unstable feedback",
    },
    Constraint {
        param: "filter.cutoff",
        rule: Rule::Log {
            span: 1.5,
            draw: 0.5,
            min: 20.0,
            max: 20_000.0,
        },
        default: 12_000.0,
        chance: 0.7,
        why: "±1.5 octaves in the log domain keeps the register",
    },
    Constraint {
        param: "filter.resonance",
        rule: Rule::Linear {
            delta: 0.1,
            min: 0.0,
            max: 0.5,
            step: 0.001,
        },
        default: 0.15,
        chance: 0.5,
        why: "above 0.5 the resonant peak dominates the voice",
    },
    Constraint {
        param: "filter.drive",
        rule: Rule::Linear {
            delta: 0.1,
            min: 0.0,
            max: 0.6,
            step: 0.001,
        },
        default: 0.0,
        chance: 0.25,
        why: "bounded saturation keeps the level predictable",
    },
    Constraint {
        param: "filter.env.amount",
        rule: Rule::Linear {
            delta: 0.5,
            min: -8.0,
            max: 8.0,
            step: 0.01,
        },
        default: 0.0,
        chance: 0.4,
        why: "a bounded sweep stays musical",
    },
    Constraint {
        param: "filter_env.attack",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.001,
            max: 4.0,
        },
        default: 0.01,
        chance: 0.3,
        why: "filter attack inside the bank's modulation window",
    },
    Constraint {
        param: "filter_env.decay",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.02,
            max: 4.0,
        },
        default: 0.2,
        chance: 0.3,
        why: "filter decay inside the bank's decay window",
    },
    Constraint {
        param: "filter_env.sustain",
        rule: Rule::Linear {
            delta: 0.08,
            min: 0.0,
            max: 1.0,
            step: 0.001,
        },
        default: 0.0,
        chance: 0.3,
        why: "filter sustain stays a fraction of the envelope",
    },
    Constraint {
        param: "filter_env.release",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.02,
            max: 8.0,
        },
        default: 0.25,
        chance: 0.3,
        why: "filter release inside the bank's tail window",
    },
    Constraint {
        param: "amp.attack",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.001,
            max: 4.0,
        },
        default: 0.008,
        chance: 0.3,
        why: "amplitude attack inside the bank's attack window",
    },
    Constraint {
        param: "amp.decay",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.02,
            max: 4.0,
        },
        default: 0.16,
        chance: 0.3,
        why: "amplitude decay inside the bank's decay window",
    },
    Constraint {
        param: "amp.sustain",
        rule: Rule::Linear {
            delta: 0.08,
            min: 0.05,
            max: 1.0,
            step: 0.001,
        },
        default: 0.72,
        chance: 0.3,
        why: "amplitude sustain stays a fraction of the envelope",
    },
    Constraint {
        param: "amp.release",
        rule: Rule::Log {
            span: 0.5,
            draw: 0.25,
            min: 0.02,
            max: 8.0,
        },
        default: 0.28,
        chance: 0.3,
        why: "amplitude release inside the bank's tail window",
    },
    Constraint {
        param: "osc.a.warp_mode",
        rule: Rule::CatalogWarp { safe: WARP_SAFE },
        default: 0.0,
        chance: 0.0,
        why: "only ids the preset catalog marks working",
    },
    Constraint {
        param: "osc.a.warp_amount",
        rule: Rule::Linear {
            delta: 0.05,
            min: 0.0,
            max: 0.6,
            step: 0.001,
        },
        default: 0.0,
        chance: 0.2,
        why: "bounded warp depth",
    },
    Constraint {
        param: "osc.b.warp_mode",
        rule: Rule::CatalogWarp { safe: WARP_SAFE },
        default: 0.0,
        chance: 0.0,
        why: "only ids the preset catalog marks working",
    },
    Constraint {
        param: "osc.b.warp_amount",
        rule: Rule::Linear {
            delta: 0.05,
            min: 0.0,
            max: 0.6,
            step: 0.001,
        },
        default: 0.0,
        chance: 0.2,
        why: "bounded warp depth",
    },
    Constraint {
        param: "osc.c.warp_mode",
        rule: Rule::CatalogWarp { safe: WARP_SAFE },
        default: 0.0,
        chance: 0.0,
        why: "only ids the preset catalog marks working",
    },
    Constraint {
        param: "osc.c.warp_amount",
        rule: Rule::Linear {
            delta: 0.05,
            min: 0.0,
            max: 0.6,
            step: 0.001,
        },
        default: 0.0,
        chance: 0.2,
        why: "bounded warp depth",
    },
    Constraint {
        param: "unison.voices",
        rule: Rule::Linear {
            delta: 1.0,
            min: 1.0,
            max: 8.0,
            step: 1.0,
        },
        default: 2.0,
        chance: 0.0,
        why: "a bounded stack keeps the voice count and level predictable",
    },
    Constraint {
        param: "unison.detune",
        rule: Rule::Linear {
            delta: 5.0,
            min: 0.0,
            max: 40.0,
            step: 0.1,
        },
        default: 7.0,
        chance: 0.0,
        why: "bounded detune stays a chorus, not a smear",
    },
    Constraint {
        param: "unison.width",
        rule: Rule::Linear {
            delta: 0.1,
            min: 0.0,
            max: 0.9,
            step: 0.001,
        },
        default: 0.55,
        chance: 0.0,
        why: "bounded spread keeps mono compatibility",
    },
    Constraint {
        param: "output.gain",
        rule: Rule::Keep { min: 0.0, max: 1.0 },
        default: 0.22,
        chance: 0.0,
        why: "level is the preset's decision, not a variation axis",
    },
];

fn hash_param(param: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in param.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// SplitMix64: the stream is stable across platforms and dependency updates.
struct Rng(u64);

impl Rng {
    fn for_param(seed: u64, param: &str) -> Self {
        Self(seed ^ hash_param(param))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
}

/// The warp ids the synth catalog marks fully implemented.
pub fn effective_warp_ids() -> Vec<f64> {
    oc_device_schema::synth::synth_catalog()
        .warp_modes
        .options
        .iter()
        .filter(|option| option.status == oc_device_schema::synth::SupportStatus::Working)
        .map(|option| option.id as f64)
        .collect()
}

/// Warp families for a nearby draw. A fold is not a nearby step from a
/// saturation (it can add 6 dB), so a draw stays inside the current family;
/// every candidate is still filtered to the catalog's working set.
const WARP_FAMILIES: &[(f64, f64)] = &[
    (0.0, 0.0),   // Off
    (1.0, 16.0),  // phase / readout
    (19.0, 23.0), // saturation and diodes
    (24.0, 25.0), // folds
    (26.0, 32.0), // clips, shapers, tape
    (66.0, 69.0), // ring modulation
];

/// Working warp ids a draw may introduce: the symmetric, DC-free readout and
/// shaper modes. Sync/ASYM/Flip/DLM, the asymmetric shapers and the folds are
/// measured to add DC or up to 6 dB, so a patch already using one keeps it.
const WARP_SAFE: &[f64] = &[
    0.0, 2.0, 3.0, 4.0, 5.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, // readout
    19.0, 20.0, 21.0, 22.0, 30.0, 31.0, 32.0, // symmetric shapers
];

fn warp_family(id: f64) -> (f64, f64) {
    WARP_FAMILIES
        .iter()
        .find(|(low, high)| (*low..=*high).contains(&id))
        .copied()
        .unwrap_or((id, id))
}

/// Every working id in the same family: the clamp's allowed set.
fn warp_choices(base: f64) -> Vec<f64> {
    let (low, high) = warp_family(base);
    let mut ids: Vec<f64> = effective_warp_ids()
        .into_iter()
        .filter(|id| (low..=high).contains(id))
        .collect();
    if ids.is_empty() {
        ids.push(base);
    }
    ids
}

/// The draw's conservative subset: a patch already outside it keeps its mode.
fn warp_draw_choices(base: f64, safe: &[f64]) -> Vec<f64> {
    if !safe.iter().any(|id| (id - base).abs() < 1e-9) {
        return vec![base];
    }
    warp_choices(base)
        .into_iter()
        .filter(|id| safe.iter().any(|candidate| (candidate - id).abs() < 1e-9))
        .collect()
}

fn nearest(values: &[f64], value: f64) -> f64 {
    values
        .iter()
        .copied()
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
        .unwrap_or(value)
}

fn quantize(value: f64, step: f64) -> f64 {
    if step > 0.0 {
        (value / step).round() * step
    } else {
        value
    }
}

impl Rule {
    fn changes(self) -> bool {
        !matches!(self, Rule::Keep { .. })
    }

    /// Draw a candidate value; `base` is the current value or the row default.
    fn sample(self, base: f64, rng: &mut Rng) -> f64 {
        match self {
            Rule::Choice(values) => values[(rng.next_u64() % values.len() as u64) as usize],
            Rule::Waveform { draw, .. } => {
                if draw.iter().any(|value| (value - base).abs() < 1e-9) {
                    draw[(rng.next_u64() % draw.len() as u64) as usize]
                } else {
                    base
                }
            }
            Rule::Log { draw, min, max, .. } => {
                let base = base.clamp(min, max);
                (base * 2f64.powf(rng.uniform(-draw, draw))).clamp(min, max)
            }
            Rule::Linear {
                delta,
                min,
                max,
                step,
            } => {
                let base = base.clamp(min, max);
                let low = (base - delta).max(min);
                let high = (base + delta).min(max);
                quantize(rng.uniform(low, high), step).clamp(min, max)
            }
            Rule::Keep { .. } => base,
            Rule::CatalogWarp { safe } => {
                let ids = warp_draw_choices(base, safe);
                ids[(rng.next_u64() % ids.len() as u64) as usize]
            }
        }
    }

    /// Project a value back onto the allowed set for `base`.
    fn clamp(self, value: f64, base: f64) -> f64 {
        match self {
            Rule::Choice(values) => nearest(values, value),
            Rule::Waveform { allowed, .. } => {
                let base_is_allowed = allowed
                    .iter()
                    .any(|candidate| (candidate - base).abs() < 1e-9);
                if (value - base).abs() < 1e-9 && !base_is_allowed {
                    base
                } else if allowed
                    .iter()
                    .any(|candidate| (candidate - value).abs() < 1e-9)
                {
                    value
                } else {
                    nearest(allowed, value)
                }
            }
            Rule::Log { span, min, max, .. } => {
                let base = base.clamp(min, max);
                let low = (base * 2f64.powf(-span)).max(min);
                let high = (base * 2f64.powf(span)).min(max);
                value.clamp(low, high)
            }
            Rule::Linear {
                delta,
                min,
                max,
                step,
            } => {
                let base = base.clamp(min, max);
                let low = (base - delta).max(min);
                let high = (base + delta).min(max);
                quantize(value.clamp(low, high), step).clamp(min, max)
            }
            Rule::Keep { min, max } => base.clamp(min, max),
            Rule::CatalogWarp { .. } => {
                let allowed = warp_choices(base);
                if allowed
                    .iter()
                    .any(|candidate| (candidate - value).abs() < 1e-9)
                {
                    value
                } else {
                    nearest(&allowed, value)
                }
            }
        }
    }

    /// Whether `value` is already on the allowed set for `base`.
    #[cfg(test)]
    fn allows(self, value: f64, base: f64) -> bool {
        (self.clamp(value, base) - value).abs() <= 1e-9
    }
}

fn value_of(params: &[(String, ParamValue)], name: &str) -> Option<f64> {
    params
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .and_then(|(_, value)| value.as_f64())
}

fn upsert(params: &mut Vec<(String, ParamValue)>, name: &str, value: f64) {
    if let Some((_, slot)) = params.iter_mut().rev().find(|(key, _)| key == name) {
        *slot = ParamValue::Number(value);
    } else {
        params.push((name.to_string(), ParamValue::Number(value)));
    }
}

/// Draw a deterministic variation of `base`. The result always satisfies
/// every row of `CONSTRAINTS`.
pub fn randomize_params(base: &[(String, ParamValue)], seed: u64) -> Vec<(String, ParamValue)> {
    let mut out = base.to_vec();
    for index in change_plan(seed) {
        let constraint = &CONSTRAINTS[index];
        let mut rng = Rng::for_param(seed, constraint.param);
        let current = value_of(base, constraint.param).unwrap_or(constraint.default);
        upsert(
            &mut out,
            constraint.param,
            constraint.rule.sample(current, &mut rng),
        );
    }
    clamp_params(base, &mut out);
    out
}

/// Up to [`MAX_CHANGES`] fields per seed, weighted by the row `chance`, so a
/// variation stays nearby instead of rewriting the whole patch at once.
const MAX_CHANGES: usize = 2;

fn change_plan(seed: u64) -> Vec<usize> {
    let mut rng = Rng::for_param(seed, "plan");
    let mut pool: Vec<usize> = CONSTRAINTS
        .iter()
        .enumerate()
        .filter(|(_, constraint)| constraint.rule.changes() && constraint.chance > 0.0)
        .map(|(index, _)| index)
        .collect();
    let target = 1 + (rng.next_u64() % MAX_CHANGES as u64) as usize;
    let mut plan = Vec::with_capacity(target);
    while plan.len() < target && !pool.is_empty() {
        let total: f64 = pool.iter().map(|&index| CONSTRAINTS[index].chance).sum();
        let mut roll = rng.uniform(0.0, total);
        let mut chosen = pool[0];
        for &index in &pool {
            roll -= CONSTRAINTS[index].chance;
            if roll <= 0.0 {
                chosen = index;
                break;
            }
        }
        plan.push(chosen);
        pool.retain(|&index| index != chosen);
    }
    plan
}

/// Enforce every row of `CONSTRAINTS` on `candidate`. `Keep` rows restore the
/// base value, so a level written elsewhere cannot become a variation axis.
pub fn clamp_params(base: &[(String, ParamValue)], candidate: &mut Vec<(String, ParamValue)>) {
    for constraint in CONSTRAINTS {
        let base_value = value_of(base, constraint.param).unwrap_or(constraint.default);
        match constraint.rule {
            Rule::Keep { .. } => {
                if value_of(base, constraint.param).is_some()
                    || value_of(candidate, constraint.param).is_some()
                {
                    upsert(
                        candidate,
                        constraint.param,
                        constraint.rule.clamp(base_value, base_value),
                    );
                }
            }
            _ => {
                if let Some(value) = value_of(candidate, constraint.param) {
                    let clamped = constraint.rule.clamp(value, base_value);
                    if (clamped - value).abs() > 1e-9 {
                        upsert(candidate, constraint.param, clamped);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_params() -> Vec<(String, ParamValue)> {
        vec![
            ("osc.a.waveform".into(), ParamValue::Number(1.0)),
            ("osc.a.level".into(), ParamValue::Number(0.8)),
            ("filter.cutoff".into(), ParamValue::Number(800.0)),
            ("filter.resonance".into(), ParamValue::Number(0.2)),
            ("filter.model".into(), ParamValue::Number(1.0)),
            ("amp.attack".into(), ParamValue::Number(0.01)),
            ("amp.decay".into(), ParamValue::Number(0.2)),
            ("amp.sustain".into(), ParamValue::Number(0.8)),
            ("amp.release".into(), ParamValue::Number(0.3)),
            ("unison.voices".into(), ParamValue::Number(2.0)),
            ("unison.detune".into(), ParamValue::Number(7.0)),
            ("unison.width".into(), ParamValue::Number(0.5)),
            ("osc.a.warp_mode".into(), ParamValue::Number(15.0)),
            ("osc.b.warp_mode".into(), ParamValue::Number(19.0)),
            ("osc.c.warp_mode".into(), ParamValue::Number(1.0)),
            ("output.gain".into(), ParamValue::Number(0.37)),
        ]
    }

    #[test]
    fn every_bound_clamps_a_violation() {
        for constraint in CONSTRAINTS {
            let base = vec![(
                constraint.param.to_string(),
                ParamValue::Number(constraint.default),
            )];
            let base_value = constraint.default;
            for violation in [1.0e9, -1.0e9, 1.0e3] {
                let mut candidate =
                    vec![(constraint.param.to_string(), ParamValue::Number(violation))];
                clamp_params(&base, &mut candidate);
                let value = value_of(&candidate, constraint.param)
                    .unwrap_or_else(|| panic!("{} was dropped", constraint.param));
                assert!(
                    constraint.rule.allows(value, base_value),
                    "{}: {violation} -> {value} is outside {:?}",
                    constraint.param,
                    constraint.rule
                );
            }
        }
    }

    #[test]
    fn a_base_outside_the_window_clamps_without_panicking() {
        for (param, base) in [
            ("amp.attack", 1.5),
            ("amp.decay", 2.2),
            ("amp.release", 4.0),
            ("filter_env.attack", 2.8),
            ("filter_env.release", 3.0),
            ("filter.env.amount", 7.5),
            ("unison.voices", 7.0),
            ("unison.width", 0.9),
            ("filter.resonance", 0.55),
        ] {
            let seed_base = vec![(param.to_string(), ParamValue::Number(base))];
            let constraint = CONSTRAINTS
                .iter()
                .find(|constraint| constraint.param == param)
                .unwrap();
            for seed in 0..64 {
                let out = randomize_params(&seed_base, seed);
                let value = value_of(&out, param).unwrap();
                assert!(constraint.rule.allows(value, base), "{param} got {value}");
            }
        }
    }

    #[test]
    fn a_seed_picks_one_or_two_distinct_fields() {
        for seed in 0..256 {
            let plan = change_plan(seed);
            assert!((1..=2).contains(&plan.len()), "seed {seed}: {plan:?}");
            if let [first, second] = plan[..] {
                assert_ne!(first, second);
            }
        }
    }

    #[test]
    fn a_seed_changes_at_most_two_fields() {
        let base = base_params();
        for seed in 0..256 {
            let out = randomize_params(&base, seed);
            let changed = CONSTRAINTS
                .iter()
                .filter(|constraint| {
                    let before = value_of(&base, constraint.param).unwrap_or(constraint.default);
                    value_of(&out, constraint.param)
                        .is_some_and(|value| (value - before).abs() > 1e-9)
                })
                .count();
            assert!(
                changed <= MAX_CHANGES,
                "seed {seed} changed {changed} fields"
            );
        }
    }

    #[test]
    fn same_seed_rebuilds_the_patch_and_new_seeds_differ() {
        let base = base_params();
        let first = randomize_params(&base, 42);
        let second = randomize_params(&base, 42);
        assert_eq!(first, second);
        let different = randomize_params(&base, 43);
        assert_ne!(first, different);
    }

    #[test]
    fn every_draw_satisfies_every_bound() {
        let base = base_params();
        for seed in 0..512 {
            let out = randomize_params(&base, seed);
            for constraint in CONSTRAINTS {
                let Some(value) = value_of(&out, constraint.param) else {
                    continue;
                };
                let base_value = value_of(&base, constraint.param).unwrap_or(constraint.default);
                assert!(
                    constraint.rule.allows(value, base_value),
                    "seed {seed} {} = {value} outside {:?}",
                    constraint.param,
                    constraint.rule
                );
            }
        }
    }

    #[test]
    fn output_gain_is_never_a_variation_axis() {
        let base = base_params();
        for seed in 0..256 {
            let out = randomize_params(&base, seed);
            assert_eq!(value_of(&out, "output.gain"), Some(0.37), "seed {seed}");
        }
    }

    #[test]
    fn warp_ids_come_from_the_catalog_working_set() {
        let base = base_params();
        let working = effective_warp_ids();
        assert!(working.contains(&1.0), "catalog lost the Sync id");
        for seed in 0..256 {
            let out = randomize_params(&base, seed);
            for param in ["osc.a.warp_mode", "osc.b.warp_mode", "osc.c.warp_mode"] {
                let Some(value) = value_of(&out, param) else {
                    continue;
                };
                assert!(
                    working.iter().any(|id| (id - value).abs() < 1e-9),
                    "seed {seed} {param} = {value} is not a working warp id"
                );
            }
        }
    }

    #[test]
    fn a_readout_warp_never_becomes_a_fold() {
        let base = vec![("osc.a.warp_mode".into(), ParamValue::Number(15.0))];
        for seed in 0..256 {
            let out = randomize_params(&base, seed);
            let id = value_of(&out, "osc.a.warp_mode").unwrap();
            assert!((1.0..=16.0).contains(&id), "seed {seed}: {id}");
        }
    }

    #[test]
    fn waveform_stays_in_the_analog_set() {
        let mut base = base_params();
        base.push(("osc.b.waveform".into(), ParamValue::Number(2.0)));
        base.push(("osc.c.waveform".into(), ParamValue::Number(3.0)));
        for seed in 0..256 {
            let out = randomize_params(&base, seed);
            for param in ["osc.a.waveform", "osc.b.waveform", "osc.c.waveform"] {
                let value = value_of(&out, param).unwrap_or_else(|| panic!("{param} missing"));
                assert!(
                    [1.0, 2.0, 3.0].iter().any(|id| (id - value).abs() < 1e-9),
                    "seed {seed} {param} = {value} is not saw/square/triangle"
                );
            }
        }
    }

    #[test]
    fn a_sine_source_keeps_its_shape() {
        let base = vec![("osc.a.waveform".into(), ParamValue::Number(0.0))];
        for seed in 0..128 {
            let out = randomize_params(&base, seed);
            assert_eq!(value_of(&out, "osc.a.waveform"), Some(0.0), "seed {seed}");
        }
    }

    #[test]
    fn waveform_clamp_still_projects_a_violation_into_the_set() {
        let base = vec![("osc.a.waveform".into(), ParamValue::Number(0.0))];
        let mut candidate = vec![("osc.a.waveform".into(), ParamValue::Number(999.0))];
        clamp_params(&base, &mut candidate);
        assert_eq!(value_of(&candidate, "osc.a.waveform"), Some(3.0));
    }

    #[test]
    fn cutoff_window_is_one_and_a_half_octaves_around_the_base() {
        let base = vec![("filter.cutoff".into(), ParamValue::Number(800.0))];
        let mut candidate = vec![("filter.cutoff".into(), ParamValue::Number(20_000.0))];
        clamp_params(&base, &mut candidate);
        let value = value_of(&candidate, "filter.cutoff").unwrap();
        let expected = 800.0 * 2f64.powf(1.5);
        assert!((value - expected).abs() < 1e-6, "got {value}");
    }

    #[test]
    fn filter_model_and_gain_survive_as_the_base_values() {
        let base = vec![
            ("filter.model".into(), ParamValue::Number(2.0)),
            ("output.gain".into(), ParamValue::Number(0.37)),
        ];
        let mut candidate = base.clone();
        candidate.push(("filter.model".into(), ParamValue::Number(999.0)));
        candidate.push(("output.gain".into(), ParamValue::Number(9.0)));
        clamp_params(&base, &mut candidate);
        assert_eq!(value_of(&candidate, "filter.model"), Some(2.0));
        assert_eq!(value_of(&candidate, "output.gain"), Some(0.37));
    }

    #[test]
    fn absent_fields_are_left_absent_for_keep_rows() {
        let base: Vec<(String, ParamValue)> = Vec::new();
        let mut candidate: Vec<(String, ParamValue)> = Vec::new();
        clamp_params(&base, &mut candidate);
        assert!(value_of(&candidate, "output.gain").is_none());
        assert!(value_of(&candidate, "filter.model").is_none());
    }
}
