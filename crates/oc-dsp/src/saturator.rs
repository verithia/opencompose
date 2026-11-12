//! Native tanh saturation with 4x FIR oversampling and latency-aligned dry mix.
//! Drive is normalized 0..1 mapped to 0..24 dB input drive. This is not an
//! emulation of a commercial circuit or an assertion of alias-free output.

use super::AudioEffect;
use super::oversampler::{LATENCY, Oversampler4x};

/// A tanh saturator with oversampling and a latency-aligned dry mix.
pub struct Saturator {
    oversampler: Oversampler4x,
    dry: [[f64; LATENCY]; 2],
    dry_pos: usize,
    drive: f64,
    gain: f64,
    mix: f64,
}

impl Default for Saturator {
    fn default() -> Self {
        Self::new()
    }
}

impl Saturator {
    /// Create the processor with its default settings.
    pub fn new() -> Self {
        Self {
            oversampler: Oversampler4x::new(),
            dry: [[0.0; LATENCY]; 2],
            dry_pos: 0,
            drive: 0.0,
            gain: 1.0,
            mix: 1.0,
        }
    }
}

impl AudioEffect for Saturator {
    fn process(&mut self, buffer: &mut [f32], _: u32) {
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input = [frame[0] as f64, frame[1] as f64];
            let gain = self.gain;
            let norm = gain.tanh();
            let wet = self.oversampler.process(input, |up| {
                [(gain * up[0]).tanh() / norm, (gain * up[1]).tanh() / norm]
            });
            for channel in 0..2 {
                let dry = self.dry[channel][self.dry_pos];
                self.dry[channel][self.dry_pos] = input[channel];
                frame[channel] = (dry * (1.0 - self.mix) + wet[channel] * self.mix) as f32;
            }
            self.dry_pos = (self.dry_pos + 1) % LATENCY;
        }
    }
    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "drive" => {
                self.drive = value.clamp(0.0, 1.0);
                self.gain = 10.0_f64.powf(self.drive * 24.0 / 20.0);
            }
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "drive" => Some(self.drive),
            "mix" => Some(self.mix),
            _ => None,
        }
    }
    fn reset(&mut self) {
        self.oversampler.reset();
        self.dry = [[0.0; LATENCY]; 2];
        self.dry_pos = 0;
    }
    fn latency_samples(&self) -> u32 {
        LATENCY as u32
    }
    fn tail_samples(&self) -> u32 {
        32
    }
    fn name(&self) -> &str {
        "Saturator"
    }
}

#[test]
fn saturation_is_nonlinear_partition_invariant_and_latency_aligned() {
    let original: Vec<f32> = (0..4096)
        .map(|i| ((i / 2) as f32 * 0.1).sin() * 0.4)
        .collect();
    let mut whole = Saturator::new();
    whole.set_param("drive", 0.5);
    let mut split = Saturator::new();
    split.set_param("drive", 0.5);
    let mut a = original.clone();
    let mut b = original.clone();
    whole.process(&mut a, 48000);
    for block in b.chunks_mut(74) {
        split.process(block, 48000);
    }
    assert_eq!(a, b);
    assert!(a.iter().all(|v| v.is_finite() && v.abs() < 1.1));
    assert_ne!(a, original);
    whole.reset();
    whole.set_param("mix", 0.0);
    let mut dry = original.clone();
    whole.process(&mut dry, 48000);
    assert!(dry[..LATENCY * 2].iter().all(|v| *v == 0.0));
    assert_eq!(
        &dry[LATENCY * 2..],
        &original[..original.len() - LATENCY * 2]
    );
    whole.reset();
    whole.set_param("mix", 1.0);
    let mut replay = original;
    whole.process(&mut replay, 48000);
    assert_eq!(a, replay);
}

#[test]
fn oversampling_reduces_a_folded_fifth_harmonic() {
    let rate = 48000.0;
    let count = 4800;
    let mut audio: Vec<f32> = (0..count * 2)
        .flat_map(|i| {
            let sample = (2.0 * std::f64::consts::PI * 7000.0 * i as f64 / rate).sin() as f32 * 0.5;
            [sample, sample]
        })
        .collect();
    let gain = 10.0_f64.powf(0.8 * 24.0 / 20.0);
    let naive: Vec<f32> = audio
        .chunks_exact(2)
        .map(|frame| ((gain * frame[0] as f64).tanh() / gain.tanh()) as f32)
        .collect();
    let mut effect = Saturator::new();
    effect.set_param("drive", 0.8);
    effect.process(&mut audio, 48000);
    let processed: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[0]).collect();
    let magnitude = |samples: &[f32]| {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, value) in samples[count..].iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * 13000.0 * i as f64 / rate;
            re += *value as f64 * phase.cos();
            im += *value as f64 * phase.sin();
        }
        re.hypot(im) / count as f64
    };
    // 5 * 7kHz folds to 13kHz at 48kHz. This checks one diagnostic, not an
    // assertion of alias-free output for arbitrary material or drive settings.
    assert!(magnitude(&processed) < magnitude(&naive) * 0.1);
}
