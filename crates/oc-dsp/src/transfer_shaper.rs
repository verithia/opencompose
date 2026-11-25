//! Prepared table shaping at the base sample rate, with independent dry and wet gains.
use super::AudioEffect;
use oc_device_schema::transfer_curve::{PARAMS, TransferCurve};

pub struct TransferShaper {
    curve: TransferCurve,
    gains: [f64; 3],
}

impl TransferShaper {
    pub fn new(curve: TransferCurve) -> Result<Self, oc_device_schema::transfer_curve::CurveError> {
        curve.validate()?;
        Ok(Self {
            curve,
            gains: std::array::from_fn(|index| PARAMS[index].default),
        })
    }
}

impl Default for TransferShaper {
    fn default() -> Self {
        Self::new(TransferCurve::default()).expect("default curve is valid")
    }
}

impl AudioEffect for TransferShaper {
    fn process(&mut self, buffer: &mut [f32], _: u32) {
        for value in buffer {
            let input = f64::from(*value);
            *value = (self.gains[1] * input
                + self.gains[2] * self.curve.sample(self.gains[0] * input))
                as f32;
        }
    }
    fn set_param(&mut self, name: &str, value: f64) {
        if let Some((index, param)) = PARAMS.iter().enumerate().find(|(_, p)| p.id == name)
            && value.is_finite()
            && (param.min..=param.max).contains(&value)
        {
            self.gains[index] = value;
        }
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        PARAMS
            .iter()
            .position(|p| p.id == name)
            .map(|i| self.gains[i])
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Transfer Shaper"
    }
}
