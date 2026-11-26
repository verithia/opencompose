//! Normalized digital coefficient for a first-order all-pass section.
use crate::{ParamCurve, ParamKind, ParamSchema, ParamUnit};

/// Keeping the pole strictly inside the unit circle bounds its ring-out.
pub const COEFFICIENT: ParamSchema = ParamSchema {
    id: "coefficient",
    name: "Coefficient",
    unit: ParamUnit::None,
    min: -0.999,
    max: 0.999,
    default: 0.0,
    curve: ParamCurve::Linear,
    steps: None,
    kind: ParamKind::Continuous,
    options: &[],
    description: "Normalized digital coefficient. Changes phase and group delay while preserving magnitude; zero delays the signal by one sample.",
    aliases: &[],
};
