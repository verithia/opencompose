//! Control-link evaluation: one control fanned out to many shaped targets.
//!
//! The record lives in the project AST; this module owns the runtime rule: the
//! control reading is normalized once, each target applies its own formula, and
//! each target advances its own one-pole smoother by the block's frame count.
//! Zero smoothing is a hard over, so a project without smoothing behaves like
//! the plain step automation it replaced.

use crate::smoothing::one_pole_alpha;
use oc_dsl::ast::{ControlLink, ControlLinkSource, LinkFormula, LinkFormulaInput};

/// One target's formula and smoother state, in link order.
struct TargetState {
    formula: LinkFormula,
    smoothing_ms: u32,
    value: f64,
}

/// A live fan-out: feed it control readings, read one normalized value per row.
pub struct ControlLinkRuntime {
    source: ControlLinkSource,
    targets: Vec<TargetState>,
}

impl ControlLinkRuntime {
    /// Build a runtime that starts every target at zero.
    pub fn new(link: &ControlLink) -> Self {
        Self {
            source: link.source.clone(),
            targets: link
                .targets
                .iter()
                .map(|target| TargetState {
                    formula: target.formula,
                    smoothing_ms: target.smoothing_ms,
                    value: 0.0,
                })
                .collect(),
        }
    }

    /// The control source feeding this link.
    pub fn source(&self) -> &ControlLinkSource {
        &self.source
    }

    /// Number of link targets.
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    /// The current smoothed value of one target.
    pub fn value(&self, index: usize) -> Option<f64> {
        self.targets.get(index).map(|target| target.value)
    }

    /// Advance every target by `frames` samples for one control reading and
    /// return the smoothed values in link order. `raw` is in the source's own
    /// domain: 0..127 for a MIDI CC, 0..1 for a macro or lane.
    pub fn process(&mut self, raw: f64, frames: usize, sample_rate: f64) -> Vec<f64> {
        let normalized = normalize_control(&self.source, raw);
        self.targets
            .iter_mut()
            .map(|target| {
                let input = match target.formula.input() {
                    LinkFormulaInput::Normalized => normalized,
                    LinkFormulaInput::Midi7Bit => raw,
                };
                let goal = target.formula.apply(input);
                let coefficient =
                    one_pole_alpha(f64::from(target.smoothing_ms), frames as f64, sample_rate);
                target.value += (goal - target.value) * coefficient;
                target.value
            })
            .collect()
    }

    /// Return every target to the value a fresh runtime starts at.
    pub fn reset(&mut self) {
        for target in &mut self.targets {
            target.value = 0.0;
        }
    }
}

/// The control reading in 0..1; a CC is scaled by its 7-bit range.
pub fn normalize_control(source: &ControlLinkSource, raw: f64) -> f64 {
    if !raw.is_finite() {
        return 0.0;
    }
    match source {
        ControlLinkSource::MidiCc { .. } => (raw / 127.0).clamp(0.0, 1.0),
        ControlLinkSource::Macro { .. } | ControlLinkSource::Lane { .. } => raw.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::{ControlLinkTarget, LinkFormula};

    fn target(param: &str, formula: LinkFormula, smoothing_ms: u32) -> ControlLinkTarget {
        ControlLinkTarget {
            track: "Lead".into(),
            param: param.into(),
            formula,
            smoothing_ms,
        }
    }

    fn link(targets: Vec<ControlLinkTarget>) -> ControlLink {
        ControlLink {
            name: "drive".into(),
            source: ControlLinkSource::MidiCc {
                channel: 1,
                controller: 74,
            },
            targets,
        }
    }

    #[test]
    fn one_cc_drives_cutoff_and_volume_with_their_own_formulas() {
        let mut runtime = ControlLinkRuntime::new(&link(vec![
            target("filter.cutoff", LinkFormula::MidiUnit, 0),
            target("output.gain", LinkFormula::Exponential, 0),
        ]));
        assert_eq!(runtime.target_count(), 2);
        // Rest: both targets sit at zero.
        assert_eq!(runtime.process(0.0, 1, 48_000.0), vec![0.0, 0.0]);
        // Centre: the scaler reaches 64/127, the ease-in only 0.254.
        let middle = runtime.process(64.0, 1, 48_000.0);
        assert!((middle[0] - 64.0 / 127.0).abs() < 1e-12);
        assert!((middle[1] - (64.0f64 / 127.0).powi(2)).abs() < 1e-12);
        // Full: both reach the top, through different curves.
        assert_eq!(runtime.process(127.0, 1, 48_000.0), vec![1.0, 1.0]);
    }

    #[test]
    fn each_target_smooths_on_its_own_time_constant() {
        let mut runtime = ControlLinkRuntime::new(&link(vec![
            target("cutoff", LinkFormula::Linear, 0),
            target("output.gain", LinkFormula::Linear, 100),
        ]));
        // A 100 ms move evaluated over exactly one time constant: 63.2%.
        let values = runtime.process(127.0, 4_800, 48_000.0);
        assert_eq!(values[0], 1.0);
        assert!((values[1] - (1.0 - (-1.0f64).exp())).abs() < 1e-9);
        // Further blocks keep approaching the goal without overshoot.
        for _ in 0..50 {
            let next = runtime.process(127.0, 480, 48_000.0);
            assert_eq!(next[0], 1.0);
            assert!(next[1] > values[1] && next[1] <= 1.0);
        }
        assert!(runtime.value(1).unwrap() > 0.99);
    }

    #[test]
    fn zero_smoothing_is_a_hard_over() {
        let mut runtime =
            ControlLinkRuntime::new(&link(vec![target("cutoff", LinkFormula::Linear, 0)]));
        assert_eq!(runtime.process(127.0, 1, 48_000.0), vec![1.0]);
        assert_eq!(runtime.process(31.75, 1, 48_000.0), vec![0.25]);
    }

    #[test]
    fn macro_and_lane_sources_are_already_normalized() {
        let macro_link = ControlLink {
            name: "macro".into(),
            source: ControlLinkSource::Macro { index: 1 },
            targets: vec![target("cutoff", LinkFormula::Linear, 0)],
        };
        let mut runtime = ControlLinkRuntime::new(&macro_link);
        assert_eq!(runtime.process(1.0, 1, 48_000.0), vec![1.0]);
        assert_eq!(runtime.process(0.5, 1, 48_000.0), vec![0.5]);
        assert_eq!(normalize_control(&macro_link.source, 1.0), 1.0);
        assert_eq!(
            normalize_control(
                &ControlLinkSource::Lane {
                    track: "Lead".into(),
                    param: "pluginparam_61".into()
                },
                127.0
            ),
            1.0
        );
    }
}
