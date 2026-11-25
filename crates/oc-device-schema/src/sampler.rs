//! Shared controls for sample-region envelopes.

use crate::{ParamCurve, ParamKind, ParamSchema, ParamUnit};

/// Zero preserves a linear release; negative shapes fade faster at the start.
pub const RELEASE_SHAPE: ParamSchema = ParamSchema {
    id: "ampeg_release_shape",
    name: "Release shape",
    unit: ParamUnit::None,
    min: -20.0,
    max: 20.0,
    default: 0.0,
    curve: ParamCurve::Linear,
    steps: None,
    kind: ParamKind::Continuous,
    options: &[],
    description: "Zero is linear; negative fades faster, positive fades slower.",
    aliases: &[],
};

/// Bound the exponent used by the region's prepared release recurrence.
pub fn valid_release_shape(value: f64) -> bool {
    value.is_finite() && (RELEASE_SHAPE.min..=RELEASE_SHAPE.max).contains(&value)
}
