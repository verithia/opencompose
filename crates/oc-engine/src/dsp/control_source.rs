//! Audio-derived control: a one-pole peak follower with optional shaping.
//!
//! The detector is the larger absolute channel value of the source buffer, so
//! the same code follows a mono or a stereo tap. `attack` and `release` are
//! exponential time constants in seconds: after a step, the state reaches
//! `1 - 1/e` in one time constant. This device is the only implementation of
//! the control law; the DSL just configures it.
//!
//! Shaping order, once per sample:
//!
//! 1. envelope from the attack/release follower,
//! 2. `scale * envelope + offset`, clamped into `[0, 1]`,
//! 3. `curve` (linear, squared, square root or log),
//! 4. `formula` over the result, clamped into `[0, 1]`,
//! 5. `lfo * lfo_depth` added, clamped into `[0, 1]`.

use oc_dsl::ast::{ControlCurve, ControlLfoShape};
use oc_dsl::formula::Formula;

/// Settings a follower runs with. The graph compiles them once per graph.
pub struct ControlSettings {
    pub attack_seconds: f64,
    pub release_seconds: f64,
    pub lfo: Option<(ControlLfoShape, f64)>,
    pub lfo_depth: f64,
    pub formula: Option<Formula>,
    pub scale: f64,
    pub offset: f64,
    pub curve: ControlCurve,
}

/// A per-sample control follower. `process` never allocates.
pub struct ControlFollower {
    attack: f64,
    release: f64,
    state: f64,
    lfo: Option<Lfo>,
    lfo_depth: f64,
    formula: Option<Formula>,
    scale: f64,
    offset: f64,
    curve: ControlCurve,
}

impl ControlFollower {
    /// Create a follower at the sample rate.
    pub fn new(sample_rate: u32, settings: ControlSettings) -> Self {
        let sample_rate = f64::from(sample_rate.max(1));
        Self {
            attack: one_pole_coefficient(settings.attack_seconds, sample_rate),
            release: one_pole_coefficient(settings.release_seconds, sample_rate),
            state: 0.0,
            lfo: settings
                .lfo
                .map(|(shape, rate_hz)| Lfo::new(shape, rate_hz, sample_rate)),
            lfo_depth: settings.lfo_depth,
            formula: settings.formula,
            scale: settings.scale,
            offset: settings.offset,
            curve: settings.curve,
        }
    }

    /// Clear the envelope and the LFO phase.
    pub fn reset(&mut self) {
        self.state = 0.0;
        if let Some(lfo) = &mut self.lfo {
            lfo.phase = 0.0;
        }
    }

    /// Current envelope state before shaping; the detector's last value.
    pub fn state(&self) -> f64 {
        self.state
    }

    /// Follow `interleaved` stereo frames into one control value per frame.
    ///
    /// Frames past the end of `interleaved` read as silence, so the same call
    /// covers a muted source without a scratch buffer; `control` decides how
    /// many frames are produced.
    pub fn process(&mut self, interleaved: &[f32], control: &mut [f32]) {
        let mut frames = interleaved.chunks_exact(2);
        for output in control.iter_mut() {
            let detector = frames
                .next()
                .map(|frame| {
                    let peak = frame[0].abs().max(frame[1].abs());
                    if peak.is_finite() {
                        f64::from(peak)
                    } else {
                        0.0
                    }
                })
                .unwrap_or(0.0);
            *output = self.step(detector) as f32;
        }
    }

    fn step(&mut self, detector: f64) -> f64 {
        let coefficient = if detector > self.state {
            self.attack
        } else {
            self.release
        };
        self.state += (detector - self.state) * coefficient;

        let mut value = (self.state * self.scale + self.offset).clamp(0.0, 1.0);
        value = match self.curve {
            ControlCurve::Linear => value,
            ControlCurve::Squared => value * value,
            ControlCurve::Sqrt => value.sqrt(),
            ControlCurve::Log => (1.0 + 9.0 * value).ln() / 10.0_f64.ln(),
        };
        if let Some(formula) = &self.formula {
            value = formula.eval(value);
        }
        if let Some(lfo) = &mut self.lfo {
            value += lfo.next() * self.lfo_depth;
        }
        value.clamp(0.0, 1.0)
    }
}

fn one_pole_coefficient(seconds: f64, sample_rate: f64) -> f64 {
    if seconds.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return 1.0;
    }
    (1.0 - (-1.0 / (seconds * sample_rate)).exp()).clamp(0.0, 1.0)
}

struct Lfo {
    shape: ControlLfoShape,
    increment: f64,
    phase: f64,
}

impl Lfo {
    fn new(shape: ControlLfoShape, rate_hz: f64, sample_rate: f64) -> Self {
        Self {
            shape,
            increment: (rate_hz / sample_rate).max(0.0),
            phase: 0.0,
        }
    }

    fn next(&mut self) -> f64 {
        let phase = self.phase;
        self.phase = (self.phase + self.increment) % 1.0;
        match self.shape {
            ControlLfoShape::Sine => (std::f64::consts::TAU * phase).sin(),
            ControlLfoShape::Triangle => {
                if phase < 0.25 {
                    4.0 * phase
                } else if phase < 0.75 {
                    2.0 - 4.0 * phase
                } else {
                    4.0 * phase - 4.0
                }
            }
            ControlLfoShape::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            ControlLfoShape::Saw => 2.0 * phase - 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_pole_coefficient_preserves_unordered_and_zero_time_policy() {
        for time in [f64::NAN, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
            assert_eq!(super::one_pole_coefficient(time, 48_000.0), 1.0);
        }
        assert_eq!(super::one_pole_coefficient(f64::INFINITY, 48_000.0), 0.0);
    }

    use super::*;

    fn settings(attack: f64, release: f64) -> ControlSettings {
        ControlSettings {
            attack_seconds: attack,
            release_seconds: release,
            lfo: None,
            lfo_depth: 1.0,
            formula: None,
            scale: 1.0,
            offset: 0.0,
            curve: ControlCurve::Linear,
        }
    }

    fn step_input(frames: usize, at: usize, amplitude: f32) -> Vec<f32> {
        let mut buffer = vec![0.0; frames * 2];
        for frame in at..frames {
            buffer[frame * 2] = amplitude;
            buffer[frame * 2 + 1] = amplitude;
        }
        buffer
    }

    #[test]
    fn attack_and_release_reach_the_time_constant() {
        let sample_rate = 48_000;
        let mut follower = ControlFollower::new(sample_rate, settings(0.005, 0.12));
        let mut control = vec![0.0; 48_000];
        follower.process(&step_input(48_000, 0, 1.0), &mut control);
        let attack = (sample_rate as f64 * 0.005) as usize;
        assert!((control[attack] as f64 - (1.0 - 1.0 / std::f64::consts::E)).abs() < 5e-3);

        // Full-scale state, then silence: after one release constant, 1/e is left.
        let mut follower = ControlFollower::new(sample_rate, settings(0.0, 0.12));
        let mut hold = vec![0.0; sample_rate as usize];
        follower.process(&step_input(sample_rate as usize, 0, 1.0), &mut hold);
        assert_eq!(hold[sample_rate as usize - 1], 1.0);
        let mut silence = vec![0.0; sample_rate as usize];
        follower.process(&[], &mut silence);
        let release = (sample_rate as f64 * 0.12) as usize;
        assert!((silence[release] as f64 - 1.0 / std::f64::consts::E).abs() < 5e-3);
    }

    #[test]
    fn silence_stays_at_zero_and_never_returns_nan() {
        let mut follower = ControlFollower::new(48_000, settings(0.005, 0.12));
        let mut control = vec![1.0; 4_800];
        follower.process(&[], &mut control);
        assert!(control.iter().all(|value| *value == 0.0), "{control:?}");

        // A non-finite input must not poison the envelope.
        let mut follower = ControlFollower::new(48_000, settings(0.005, 0.12));
        let mut control = vec![0.0; 480];
        follower.process(&[f32::NAN; 960], &mut control);
        assert!(
            control
                .iter()
                .all(|value| value.is_finite() && *value == 0.0)
        );
    }

    #[test]
    fn curve_formula_and_lfo_apply_in_the_documented_order() {
        let mut curve = ControlFollower::new(
            48_000,
            ControlSettings {
                formula: Some(Formula::parse("1-x").unwrap()),
                ..settings(0.0, 0.0)
            },
        );
        let mut control = vec![0.0; 4];
        curve.process(&step_input(4, 0, 0.25), &mut control);
        // (0.25 -> squared via formula? no) linear envelope 0.25, formula 1-x = 0.75.
        assert!((control[0] - 0.75).abs() < 1e-6, "{control:?}");

        let mut lfo = ControlFollower::new(
            48_000,
            ControlSettings {
                lfo: Some((ControlLfoShape::Sine, 100.0)),
                lfo_depth: 0.5,
                ..settings(0.0, 0.0)
            },
        );
        let mut control = vec![0.0; 480];
        lfo.process(&[], &mut control);
        let peak = control.iter().fold(0.0_f32, |peak, value| peak.max(*value));
        let trough = control
            .iter()
            .fold(1.0_f32, |trough, value| trough.min(*value));
        assert!((peak - 0.5).abs() < 1e-3, "peak {peak}");
        assert!((trough - 0.0).abs() < 1e-3, "trough {trough}");
    }

    #[test]
    fn curve_maps_the_envelope() {
        for (curve, expected) in [
            (ControlCurve::Linear, 0.25),
            (ControlCurve::Squared, 0.0625),
            (ControlCurve::Sqrt, 0.5),
        ] {
            let mut follower = ControlFollower::new(
                48_000,
                ControlSettings {
                    curve,
                    ..settings(0.0, 0.0)
                },
            );
            let mut control = vec![0.0; 1];
            follower.process(&[0.25, 0.25], &mut control);
            assert!((control[0] - expected).abs() < 1e-6, "{curve:?}");
        }
    }
}
