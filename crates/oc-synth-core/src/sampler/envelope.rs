//! Sample-region amplitude shape and heap-free release progression.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
/// ADSR envelope in seconds, with sustain as a fraction.
pub struct AmpEnvelope {
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
    #[serde(default = "default_release_shape")]
    pub release_shape: f64,
}

fn default_release_shape() -> f64 {
    oc_device_schema::sampler::RELEASE_SHAPE.default
}

impl Default for AmpEnvelope {
    fn default() -> Self {
        Self {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: super::DEFAULT_RELEASE_SECONDS,
            release_shape: default_release_shape(),
        }
    }
}

#[derive(Default)]
pub(super) enum ReleaseRamp {
    #[default]
    Idle,
    Linear {
        step: f64,
    },
    Curved {
        multiplier: f64,
        offset: f64,
        remaining: u64,
    },
}

impl ReleaseRamp {
    pub fn new(level: f64, envelope: AmpEnvelope, sample_rate: f64) -> Self {
        let frames = envelope.release * sample_rate;
        if frames <= 0.0 || envelope.release_shape.abs() < 1e-6 {
            return Self::Linear {
                step: if frames <= 0.0 { level } else { level / frames },
            };
        }
        // The normalized decay is 1 - expm1(shape*x) / expm1(shape).
        // Its affine recurrence needs no powers, allocation or I/O per sample.
        let shape = envelope.release_shape;
        let multiplier = (shape / frames).exp();
        let bias = level * shape.exp() / shape.exp_m1();
        Self::Curved {
            multiplier,
            offset: bias * (1.0 - multiplier),
            remaining: frames.ceil() as u64,
        }
    }

    pub fn next(&mut self, level: f64) -> f64 {
        match self {
            Self::Idle => 0.0,
            Self::Linear { step } => (level - *step).max(0.0),
            Self::Curved {
                multiplier,
                offset,
                remaining,
            } => {
                if *remaining <= 1 {
                    *remaining = 0;
                    0.0
                } else {
                    *remaining -= 1;
                    (level * *multiplier + *offset).max(0.0)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_adsr_without_a_shape_keeps_linear_release() {
        let envelope: AmpEnvelope =
            serde_json::from_str(r#"{"attack":0,"decay":0,"sustain":1,"release":0.3}"#).unwrap();
        assert_eq!(envelope.release_shape, default_release_shape());
        let mut ramp = ReleaseRamp::new(1.0, envelope, 100.0);
        let mut level = 1.0;
        for _ in 0..15 {
            level = ramp.next(level);
        }
        assert!((level - 0.5).abs() < 1e-12);
    }

    #[test]
    fn shaped_release_captures_current_level_and_handles_zero_duration() {
        for shape in [-20.0_f64, -1e-8, 0.0, 1e-8, 20.0] {
            let envelope = AmpEnvelope {
                release: 0.1,
                release_shape: shape,
                ..Default::default()
            };
            let mut ramp = ReleaseRamp::new(0.37, envelope, 1_000.0);
            let mut value = 0.37;
            for frame in 1..=101 {
                let next = ramp.next(value);
                assert!(next.is_finite() && next >= 0.0 && next <= value + 1e-12);
                if frame == 25 {
                    let expected = if shape.abs() < 1e-6 {
                        0.75
                    } else {
                        1.0 - (shape * 0.25).exp_m1() / shape.exp_m1()
                    };
                    assert!((next / 0.37 - expected).abs() < 1e-8);
                }
                value = next;
            }
            assert_eq!(value, 0.0);
            let mut immediate = ReleaseRamp::new(
                0.37,
                AmpEnvelope {
                    release: 0.0,
                    release_shape: shape,
                    ..Default::default()
                },
                48_000.0,
            );
            assert_eq!(immediate.next(0.37), 0.0);
        }
    }
}
