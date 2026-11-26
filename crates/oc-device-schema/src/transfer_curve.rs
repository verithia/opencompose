//! Bounded, silence-preserving transfer assets for sample-domain shaping.
use serde::{Deserialize, Serialize};

use crate::{ParamCurve, ParamKind, ParamSchema, ParamUnit};

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SAMPLES: usize = 65_537;
pub const MAX_LEVEL: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurveDomain {
    Odd,
    Bipolar,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferCurve {
    pub version: u8,
    pub domain: CurveDomain,
    pub samples: Vec<f64>,
}

impl Default for TransferCurve {
    fn default() -> Self {
        Self {
            version: 1,
            domain: CurveDomain::Odd,
            samples: vec![0.0, 1.0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveError {
    Bytes,
    Json,
    Version,
    Count,
    Level,
    Zero,
}

impl std::fmt::Display for CurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transfer curve: {self:?}")
    }
}
impl std::error::Error for CurveError {}

impl TransferCurve {
    pub fn decode(source: &str) -> Result<Self, CurveError> {
        if source.len() > MAX_BYTES {
            return Err(CurveError::Bytes);
        }
        let curve: Self = serde_json::from_str(source).map_err(|_| CurveError::Json)?;
        curve.validate()?;
        Ok(curve)
    }

    pub fn validate(&self) -> Result<(), CurveError> {
        if self.version != 1 {
            return Err(CurveError::Version);
        }
        if !(2..=MAX_SAMPLES).contains(&self.samples.len()) {
            return Err(CurveError::Count);
        }
        if self
            .samples
            .iter()
            .any(|v| !v.is_finite() || v.abs() > MAX_LEVEL)
        {
            return Err(CurveError::Level);
        }
        if self.sample(0.0) != 0.0 {
            return Err(CurveError::Zero);
        }
        Ok(())
    }

    /// Linear interpolation on a uniform grid; outside values clamp to endpoints.
    /// The caller prepares and validates the table before processing audio.
    pub fn sample(&self, input: f64) -> f64 {
        let (position, sign) = match self.domain {
            CurveDomain::Odd => (input.abs().min(1.0), if input < 0.0 { -1.0 } else { 1.0 }),
            CurveDomain::Bipolar => ((input.clamp(-1.0, 1.0) + 1.0) * 0.5, 1.0),
        };
        let at = position * (self.samples.len() - 1) as f64;
        let index = (at as usize).min(self.samples.len() - 1);
        let next = (index + 1).min(self.samples.len() - 1);
        sign * (self.samples[index]
            + (self.samples[next] - self.samples[index]) * (at - index as f64))
    }
}

const fn gain(id: &'static str, name: &'static str, default: f64) -> ParamSchema {
    ParamSchema {
        id,
        name,
        unit: ParamUnit::None,
        min: 0.0,
        max: 16.0,
        default,
        curve: ParamCurve::Linear,
        steps: None,
        kind: ParamKind::Continuous,
        options: &[],
        description: "Linear amplitude multiplier.",
        aliases: &[],
    }
}
pub const PARAMS: &[ParamSchema] = &[
    gain("pre_gain", "Input", 1.0),
    gain("dry_gain", "Dry", 0.0),
    gain("wet_gain", "Wet", 1.0),
];

#[derive(Debug, Serialize)]
pub struct CurveAssetSchema {
    pub device: &'static str,
    pub param: &'static str,
    pub encoding: &'static str,
    pub version: u8,
    pub domains: &'static [&'static str],
    pub max_bytes: usize,
    pub max_samples: usize,
    pub max_level: f64,
    pub default_json: &'static str,
}
pub const ASSETS: &[CurveAssetSchema] = &[CurveAssetSchema {
    device: "transfer-shaper",
    param: "curve",
    encoding: "inline_json",
    version: 1,
    domains: &["odd", "bipolar"],
    max_bytes: MAX_BYTES,
    max_samples: MAX_SAMPLES,
    max_level: MAX_LEVEL,
    default_json: r#"{"version":1,"domain":"odd","samples":[0,1]}"#,
}];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assets_are_bounded_and_silence_preserving() {
        let c = TransferCurve::decode(ASSETS[0].default_json).unwrap();
        assert_eq!(c, TransferCurve::default());
        assert_eq!(c.sample(-0.37), -0.37);
        assert_eq!(c.sample(4.0), 1.0);
        for (text, expected) in [
            (
                r#"{"version":2,"domain":"odd","samples":[0,1]}"#,
                CurveError::Version,
            ),
            (
                r#"{"version":1,"domain":"odd","samples":[1,1]}"#,
                CurveError::Zero,
            ),
            (
                r#"{"version":1,"domain":"odd","samples":[0,17]}"#,
                CurveError::Level,
            ),
            (
                r#"{"version":1,"domain":"odd","samples":[]}"#,
                CurveError::Count,
            ),
        ] {
            assert_eq!(TransferCurve::decode(text), Err(expected));
        }
        assert_eq!(
            TransferCurve::decode(&" ".repeat(MAX_BYTES + 1)),
            Err(CurveError::Bytes)
        );
        let c = TransferCurve {
            version: 1,
            domain: CurveDomain::Bipolar,
            samples: vec![-1.0, 0.0, 0.5],
        };
        c.validate().unwrap();
        assert_eq!(c.sample(-0.5), -0.5);
        assert_eq!(c.sample(0.5), 0.25);
        assert!(
            TransferCurve {
                samples: vec![0.0; MAX_SAMPLES + 1],
                ..c.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            TransferCurve {
                samples: vec![f64::NAN, 0.0],
                ..c
            }
            .validate()
            .is_err()
        );
    }
}
