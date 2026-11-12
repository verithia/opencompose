//! Shared dynamics primitives: linear/dB conversion.

pub(crate) const MIN_LEVEL: f64 = 1e-30;

pub(crate) fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

pub(crate) fn linear_to_db(linear: f64) -> f64 {
    20.0 * linear.max(MIN_LEVEL).log10()
}
