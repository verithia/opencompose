//! A causal first-order all-pass section, H(z) = (a + z^-1)/(1 + a z^-1).
use crate::AudioEffect;
use oc_device_schema::allpass::COEFFICIENT;

/// Independent stereo state; no lookahead or host alignment latency.
pub struct AllPass {
    coefficient: f64,
    state: [f64; 2],
    tail: u32,
}
impl Default for AllPass {
    fn default() -> Self {
        let mut effect = Self {
            coefficient: COEFFICIENT.default,
            state: [0.0; 2],
            tail: 1,
        };
        effect.update_tail();
        effect
    }
}
impl AllPass {
    fn update_tail(&mut self) {
        let pole = self.coefficient.abs();
        // For unit-bounded input and arbitrary valid coefficient updates,
        // the pole bound limits state by 1/(1-max|a|). Keep decay to -120 dB; the zero pole keeps one frame.
        self.tail = if pole == 0.0 {
            1
        } else {
            ((1e-6_f64 * (1.0 - COEFFICIENT.max.abs().max(COEFFICIENT.min.abs()))).ln() / pole.ln())
                .ceil() as u32
                + 1
        };
    }
}
impl AudioEffect for AllPass {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        for frame in buffer.as_chunks_mut::<2>().0 {
            for (sample, state) in frame.iter_mut().zip(&mut self.state) {
                let input = f64::from(*sample);
                let output = self.coefficient * input + *state;
                *state = input - self.coefficient * output;
                *sample = output as f32;
            }
        }
    }
    fn set_param(&mut self, name: &str, value: f64) {
        if name == COEFFICIENT.id
            && value.is_finite()
            && (COEFFICIENT.min..=COEFFICIENT.max).contains(&value)
        {
            self.coefficient = value;
            self.update_tail();
        }
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        (name == COEFFICIENT.id).then_some(self.coefficient)
    }
    fn reset(&mut self) {
        self.state = [0.0; 2];
    }
    fn name(&self) -> &str {
        "All-pass"
    }
    fn tail_samples(&self) -> u32 {
        self.tail
    }
}
