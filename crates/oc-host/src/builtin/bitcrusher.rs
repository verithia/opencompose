//! Bitcrusher — reduces bit depth and sample rate for lo-fi effects.
//!
//! Parameters:
//!   bits: 1 to 16 (default 8) — bit depth
//!   downsample: 1 to 64 (default 1) — keep every Nth sample

use oc_dsp::AudioEffect;

/// Built-in bitcrusher effect.
pub struct Bitcrusher {
    bits: u32,
    downsample: u32,
    counter: u32,
    held_l: f32,
    held_r: f32,
    mix: f64,
}

impl Default for Bitcrusher {
    fn default() -> Self {
        Self::new()
    }
}

impl Bitcrusher {
    /// Create the effect with its default settings.
    pub fn new() -> Self {
        Self {
            bits: 8,
            downsample: 1,
            counter: 0,
            held_l: 0.0,
            held_r: 0.0,
            mix: 1.0,
        }
    }

    fn crush(sample: f32, levels: f32) -> f32 {
        (sample * levels).round() / levels
    }
}

impl AudioEffect for Bitcrusher {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let levels = (1u32 << self.bits) as f32;
        let mix = self.mix as f32;
        let dry = 1.0 - mix;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            // Downsample: hold the value for N samples
            if self.counter == 0 {
                self.held_l = Self::crush(buffer[i], levels);
                self.held_r = Self::crush(buffer[i + 1], levels);
            }
            self.counter = (self.counter + 1) % self.downsample;

            buffer[i] = buffer[i] * dry + self.held_l * mix;
            buffer[i + 1] = buffer[i + 1] * dry + self.held_r * mix;
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "bits" => self.bits = (value as u32).clamp(1, 16),
            "downsample" | "rate" => self.downsample = (value as u32).clamp(1, 64),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "bits" => Some(self.bits as f64),
            "downsample" | "rate" => Some(self.downsample as f64),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.counter = 0;
        self.held_l = 0.0;
        self.held_r = 0.0;
    }

    fn name(&self) -> &str {
        "Bitcrusher"
    }
}
