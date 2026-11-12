//! Simple gain plugin — amplifies or attenuates audio.
//!
//! Parameters:
//!   gain_db: -60.0 to +24.0 (default 0.0)

use oc_dsp::AudioEffect;

/// Built-in gain effect.
pub struct Gain {
    gain_db: f64,
    gain_linear: f64,
}

impl Default for Gain {
    fn default() -> Self {
        Self::new()
    }
}

impl Gain {
    /// Create the effect with its default settings.
    pub fn new() -> Self {
        Self {
            gain_db: 0.0,
            gain_linear: 1.0,
        }
    }
}

impl AudioEffect for Gain {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let g = self.gain_linear as f32;
        for sample in buffer.iter_mut() {
            *sample *= g;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if name == "gain_db" || name == "gain" {
            self.gain_db = value.clamp(-60.0, 24.0);
            self.gain_linear = 10.0_f64.powf(self.gain_db / 20.0);
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "gain_db" | "gain" => Some(self.gain_db),
            _ => None,
        }
    }

    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Gain"
    }
}
