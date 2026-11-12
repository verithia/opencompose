//! Stereo panner with width control.
//!
//! Parameters:
//!   pan: -1.0 (hard left) to +1.0 (hard right), default 0.0
//!   width: 0.0 (mono) to 2.0 (extra wide), default 1.0

use oc_dsp::AudioEffect;

/// Built-in stereo pan effect.
pub struct StereoPan {
    pan: f64,
    width: f64,
}

impl Default for StereoPan {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoPan {
    /// Create the effect with its default settings.
    pub fn new() -> Self {
        Self {
            pan: 0.0,
            width: 1.0,
        }
    }
}

impl AudioEffect for StereoPan {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        // Constant-power pan
        let angle = (self.pan + 1.0) * std::f64::consts::FRAC_PI_4;
        let gain_l = angle.cos() as f32;
        let gain_r = angle.sin() as f32;

        let width = self.width as f32;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            let l = buffer[i];
            let r = buffer[i + 1];

            // Mid/side processing for width
            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5 * width;

            buffer[i] = (mid + side) * gain_l;
            buffer[i + 1] = (mid - side) * gain_r;
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "pan" => self.pan = value.clamp(-1.0, 1.0),
            "width" => self.width = value.clamp(0.0, 2.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "pan" => Some(self.pan),
            "width" => Some(self.width),
            _ => None,
        }
    }

    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Stereo Pan"
    }
}
