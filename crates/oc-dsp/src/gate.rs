//! Peak-detector noise gate with linear attack/hold/release envelopes.

use super::AudioEffect;

/// A peak-detector noise gate with attack, hold and release.
pub struct Gate {
    threshold_db: f64,
    attack_ms: f64,
    hold_ms: f64,
    release_ms: f64,
    range_db: f64,
    threshold_linear: f64,
    floor: f64,
    gain: f64,
    hold_remaining: u32,
    sample_rate: u32,
}

impl Gate {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let mut gate = Self {
            threshold_db: -40.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            release_ms: 100.0,
            range_db: -60.0,
            threshold_linear: 0.01,
            floor: 0.001,
            gain: 0.001,
            hold_remaining: 0,
            sample_rate: sample_rate.max(1),
        };
        gate.update_levels();
        gate.gain = gate.floor;
        gate
    }

    fn update_levels(&mut self) {
        self.threshold_linear = 10.0_f64.powf(self.threshold_db / 20.0);
        self.floor = 10.0_f64.powf(self.range_db / 20.0);
    }

    fn attack_step(&self) -> f64 {
        let seconds = (self.attack_ms * 0.001).max(1.0 / self.sample_rate as f64);
        1.0 / (seconds * self.sample_rate as f64)
    }

    fn release_step(&self) -> f64 {
        let seconds = (self.release_ms * 0.001).max(1.0 / self.sample_rate as f64);
        (1.0 - self.floor) / (seconds * self.sample_rate as f64)
    }
}

impl AudioEffect for Gate {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let threshold = self.threshold_linear;
        let floor = self.floor;
        let attack = self.attack_step();
        let release = self.release_step();
        let hold_samples = (self.hold_ms * 0.001 * self.sample_rate as f64) as u32;

        for frame in buffer.as_chunks_mut::<2>().0 {
            let peak = (frame[0].abs() as f64).max(frame[1].abs() as f64);
            if peak > threshold {
                self.hold_remaining = hold_samples;
            } else {
                self.hold_remaining = self.hold_remaining.saturating_sub(1);
            }

            let target = if peak > threshold || self.hold_remaining > 0 {
                1.0
            } else {
                floor
            };
            if target > self.gain {
                self.gain = (self.gain + attack).min(target);
            } else {
                self.gain = (self.gain - release).max(target);
            }

            frame[0] = (frame[0] as f64 * self.gain) as f32;
            frame[1] = (frame[1] as f64 * self.gain) as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "threshold" => {
                self.threshold_db = value.clamp(-80.0, 0.0);
                self.update_levels();
            }
            "attack" => self.attack_ms = value.clamp(0.1, 200.0),
            "hold" => self.hold_ms = value.clamp(0.0, 1_000.0),
            "release" => self.release_ms = value.clamp(5.0, 2_000.0),
            "range" => {
                self.range_db = value.clamp(-80.0, 0.0);
                self.update_levels();
                self.gain = self.gain.max(self.floor);
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "threshold" => Some(self.threshold_db),
            "attack" => Some(self.attack_ms),
            "hold" => Some(self.hold_ms),
            "release" => Some(self.release_ms),
            "range" => Some(self.range_db),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.gain = self.floor;
        self.hold_remaining = 0;
    }

    fn gain_reduction_db(&self) -> f64 {
        -20.0 * self.gain.max(1e-6).log10()
    }

    fn name(&self) -> &str {
        "Gate"
    }
}

#[test]
fn gate_opens_on_a_step_and_closes_after_release() {
    let mut gate = Gate::new(48000);
    gate.set_param("threshold", -20.0);
    gate.set_param("attack", 0.1);
    gate.set_param("hold", 0.0);
    gate.set_param("release", 10.0);
    gate.set_param("range", -60.0);

    let mut quiet = vec![0.001_f32; 960];
    gate.process(&mut quiet, 48000);
    assert!(quiet.iter().all(|v| v.abs() < 1e-5));

    let mut loud = vec![0.5_f32; 480];
    gate.process(&mut loud, 48000);
    assert!(loud[200..].iter().all(|v| (*v - 0.5).abs() < 1e-3));

    let mut tail = vec![0.001_f32; 960];
    gate.process(&mut tail, 48000);
    assert!(tail[900..].iter().all(|v| v.abs() < 1e-4));
}

#[test]
fn gate_with_zero_range_is_transparent() {
    let original: Vec<f32> = (0..4800)
        .flat_map(|i| {
            let sample = (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 48000.0).sin() as f32;
            [sample, -sample]
        })
        .collect();
    let mut gate = Gate::new(48000);
    gate.set_param("range", 0.0);
    let mut audio = original.clone();
    gate.process(&mut audio, 48000);
    assert_eq!(audio, original);
}

#[test]
fn gate_stays_finite_at_extreme_settings() {
    let mut gate = Gate::new(48000);
    gate.set_param("threshold", 0.0);
    gate.set_param("attack", 200.0);
    gate.set_param("hold", 1_000.0);
    gate.set_param("release", 2_000.0);
    gate.set_param("range", -80.0);
    let mut audio: Vec<f32> = (0..48000)
        .flat_map(|i| {
            let sample = if i % 1000 < 100 { 1.0 } else { 0.0 };
            [sample, sample]
        })
        .collect();
    gate.process(&mut audio, 48000);
    assert!(audio.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
}
