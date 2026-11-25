//! Fixed-capacity cut sections shared by the parametric bands and dedicated cuts.

use std::f64::consts::{PI, SQRT_2};

use oc_device_schema::DYNAMIC_EQ_CUT_SLOPES;

use crate::biquad::{BiquadCoefficients, BiquadHistory, FilterType};

use super::FILTER_RATE_CEILING;

const MAX_EXTRA: usize = DYNAMIC_EQ_CUT_SLOPES.len() - 1;

/// The number of additional second-order sections, stored as the schema index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CutSlope(usize);

impl CutSlope {
    pub(super) fn from_parameter(value: f64) -> Option<Self> {
        let code = value.round();
        if code >= 0.0 && code < DYNAMIC_EQ_CUT_SLOPES.len() as f64 {
            Some(Self(code as usize))
        } else {
            None
        }
    }

    pub(super) fn parameter(self) -> f64 {
        self.0 as f64
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ExtraSections {
    coefficients: [BiquadCoefficients; MAX_EXTRA],
    len: usize,
}

impl ExtraSections {
    pub(super) fn design(
        shape: FilterType,
        freq: f64,
        q: f64,
        gain_db: f64,
        sample_rate: f64,
        slope: CutSlope,
    ) -> (BiquadCoefficients, Self) {
        let mut extra = Self::default();
        if slope.0 == 0 || !matches!(shape, FilterType::HighPass | FilterType::LowPass) {
            return (
                BiquadCoefficients::design_with_ceiling(
                    shape,
                    freq,
                    q,
                    gain_db,
                    sample_rate,
                    FILTER_RATE_CEILING,
                ),
                extra,
            );
        }
        extra.len = slope.0;
        let sections = extra.len + 1;
        // Butterworth poles set the slope. Distribute the user's Q adjustment
        // so the total amplitude at the corner is the same at every order.
        let scale = (SQRT_2 * q).powf(1.0 / sections as f64);
        let section = |index: usize| {
            let angle = (2 * index + 1) as f64 * PI / (4 * sections) as f64;
            let q = scale / (2.0 * angle.cos());
            BiquadCoefficients::design_with_ceiling(
                shape,
                freq,
                q,
                0.0,
                sample_rate,
                FILTER_RATE_CEILING,
            )
        };
        for (index, coefficients) in extra.coefficients[..extra.len].iter_mut().enumerate() {
            *coefficients = section(index + 1);
        }
        (section(0), extra)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FilterState {
    first: BiquadHistory,
    extra: [BiquadHistory; MAX_EXTRA],
}

impl FilterState {
    #[inline]
    pub(super) fn process(
        &mut self,
        first: &BiquadCoefficients,
        extra: &ExtraSections,
        input: f64,
    ) -> f64 {
        let mut value = self.first.process(first, input);
        for (state, coefficients) in self.extra.iter_mut().zip(&extra.coefficients[..extra.len]) {
            value = state.process(coefficients, value);
        }
        value
    }
}
