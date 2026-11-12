//! Mid/side stereo width control; 100 % is the exact identity.

use super::AudioEffect;

/// A mid/side stereo width control; 100% is the exact identity.
pub struct StereoWidth {
    width: f64,
}

impl Default for StereoWidth {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoWidth {
    /// Create the processor with its default settings.
    pub fn new() -> Self {
        Self { width: 1.0 }
    }
}

impl AudioEffect for StereoWidth {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        if (self.width - 1.0).abs() < 1e-12 {
            return;
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            let left = frame[0] as f64;
            let right = frame[1] as f64;
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;
            frame[0] = (mid + side * self.width) as f32;
            frame[1] = (mid - side * self.width) as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        if name == "amount" {
            self.width = value.clamp(0.0, 2.0);
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        (name == "amount").then_some(self.width)
    }

    fn reset(&mut self) {}

    fn name(&self) -> &str {
        "Stereo Width"
    }
}

#[test]
fn width_100_is_the_exact_identity() {
    let original: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.017).sin() * 0.5).collect();
    let mut effect = StereoWidth::new();
    let mut audio = original.clone();
    effect.process(&mut audio, 48000);
    assert_eq!(audio, original);
}

#[test]
fn zero_width_is_mono_and_two_hundred_percent_stays_finite() {
    let mut effect = StereoWidth::new();
    let mut audio = vec![0.5, -0.5, 0.25, 0.0, -0.75, 0.25];
    effect.set_param("amount", 0.0);
    effect.process(&mut audio, 48000);
    assert_eq!(audio.as_slice(), &[0.0, 0.0, 0.125, 0.125, -0.25, -0.25]);

    effect.set_param("amount", 2.0);
    let mut audio: Vec<f32> = (0..4800)
        .flat_map(|i| {
            let left = (i as f32 * 0.013).sin() * 0.5;
            let right = (i as f32 * 0.011).cos() * 0.25;
            [left, right]
        })
        .collect();
    effect.process(&mut audio, 48000);
    assert!(audio.iter().all(|v| v.is_finite() && v.abs() <= 1.5));
}
