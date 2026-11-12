//! Ducked space send: a wet return delayed, widened and reduced by the dry vocal envelope.

use super::AudioEffect;

const MIN_PREDELAY_MS: f64 = 20.0;
const MAX_PREDELAY_MS: f64 = 40.0;
const MIN_DEPTH_DB: f64 = 0.0;
const MAX_DEPTH_DB: f64 = 24.0;
const MIN_ATTACK_MS: f64 = 5.0;
const MAX_ATTACK_MS: f64 = 100.0;
const MIN_RELEASE_MS: f64 = 100.0;
const MAX_RELEASE_MS: f64 = 1_000.0;
const MIN_THRESHOLD_DB: f64 = -60.0;
const MAX_THRESHOLD_DB: f64 = 0.0;
const MIN_WIDTH: f64 = 0.0;
const MAX_WIDTH: f64 = 200.0;
const LEVEL_SMOOTH_MS: f64 = 5.0;
const TIME_TO_95_PERCENT: f64 = 3.0;
const MIN_LEVEL: f64 = 1e-12;

/// A wet space return reduced by the dry vocal envelope.
pub struct DuckedSend {
    delay: [Vec<f32>; 2],
    write_pos: usize,
    predelay_samples: usize,
    predelay_ms: f64,
    depth_db: f64,
    attack_ms: f64,
    release_ms: f64,
    threshold_db: f64,
    width: f64,
    level: f64,
    level_coeff: f64,
    attack_coeff: f64,
    release_coeff: f64,
    duck_db: f64,
    max_reduction_db: f64,
    sample_rate: f64,
}

impl DuckedSend {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f64;
        let max_samples = (sample_rate * MAX_PREDELAY_MS * 0.001).ceil() as usize + 2;
        let mut effect = Self {
            delay: [vec![0.0; max_samples], vec![0.0; max_samples]],
            write_pos: 0,
            predelay_samples: 0,
            predelay_ms: 30.0,
            depth_db: 9.0,
            attack_ms: 20.0,
            release_ms: 320.0,
            threshold_db: -45.0,
            width: 120.0,
            level: 0.0,
            level_coeff: 0.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            duck_db: 0.0,
            max_reduction_db: 0.0,
            sample_rate,
        };
        effect.update_times();
        effect.update_predelay();
        effect
    }

    fn update_times(&mut self) {
        let level_tau = LEVEL_SMOOTH_MS * 0.001;
        self.level_coeff = 1.0 - (-1.0 / (level_tau * self.sample_rate)).exp();
        let attack_tau = (self.attack_ms * 0.001 / TIME_TO_95_PERCENT).max(1.0 / self.sample_rate);
        let release_tau =
            (self.release_ms * 0.001 / TIME_TO_95_PERCENT).max(1.0 / self.sample_rate);
        self.attack_coeff = 1.0 - (-1.0 / (attack_tau * self.sample_rate)).exp();
        self.release_coeff = 1.0 - (-1.0 / (release_tau * self.sample_rate)).exp();
    }

    fn update_predelay(&mut self) {
        self.predelay_samples = (self.predelay_ms * 0.001 * self.sample_rate)
            .round()
            .clamp(0.0, self.delay[0].len() as f64 - 1.0) as usize;
    }

    fn read_delayed(&mut self, input: [f64; 2]) -> [f64; 2] {
        self.delay[0][self.write_pos] = input[0] as f32;
        self.delay[1][self.write_pos] = input[1] as f32;
        let len = self.delay[0].len();
        let read = (self.write_pos + len - self.predelay_samples) % len;
        let delayed = [self.delay[0][read] as f64, self.delay[1][read] as f64];
        self.write_pos = (self.write_pos + 1) % len;
        delayed
    }

    fn process_frames(&mut self, buffer: &mut [f32], sidechain: Option<&[f32]>) {
        let width = self.width * 0.01;
        for (frame_index, frame) in buffer.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let delayed = self.read_delayed([frame[0] as f64, frame[1] as f64]);

            let key = sidechain
                .and_then(|key| {
                    let index = frame_index * 2;
                    key.get(index)
                        .zip(key.get(index + 1))
                        .map(|(left, right)| 0.5 * (left.abs() + right.abs()) as f64)
                })
                .unwrap_or(0.0);
            self.level += (key - self.level) * self.level_coeff;
            let level_db = 20.0 * self.level.max(MIN_LEVEL).log10();
            let target = if sidechain.is_some() && level_db >= self.threshold_db {
                self.depth_db
            } else {
                0.0
            };
            let coeff = if target > self.duck_db {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.duck_db += (target - self.duck_db) * coeff;
            self.max_reduction_db = self.max_reduction_db.max(self.duck_db);
            let gain = 10f64.powf(-self.duck_db / 20.0);

            let mid = 0.5 * (delayed[0] + delayed[1]);
            let side = 0.5 * (delayed[0] - delayed[1]) * width;
            frame[0] = ((mid + side) * gain) as f32;
            frame[1] = ((mid - side) * gain) as f32;
        }
    }
}

impl AudioEffect for DuckedSend {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.process_frames(buffer, None);
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        _transport: Option<oc_audio::PluginTransport>,
    ) {
        self.process_frames(buffer, Some(sidechain));
    }

    fn supports_sidechain(&self) -> bool {
        true
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "predelay" => {
                self.predelay_ms = value.clamp(MIN_PREDELAY_MS, MAX_PREDELAY_MS);
                self.update_predelay();
            }
            "depth" => self.depth_db = value.clamp(MIN_DEPTH_DB, MAX_DEPTH_DB),
            "attack" => {
                self.attack_ms = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                self.update_times();
            }
            "release" => {
                self.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                self.update_times();
            }
            "threshold" => self.threshold_db = value.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            "width" => self.width = value.clamp(MIN_WIDTH, MAX_WIDTH),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "predelay" => Some(self.predelay_ms),
            "depth" => Some(self.depth_db),
            "attack" => Some(self.attack_ms),
            "release" => Some(self.release_ms),
            "threshold" => Some(self.threshold_db),
            "width" => Some(self.width),
            _ => None,
        }
    }

    fn reset(&mut self) {
        for delay in &mut self.delay {
            delay.fill(0.0);
        }
        self.write_pos = 0;
        self.level = 0.0;
        self.duck_db = 0.0;
        self.max_reduction_db = 0.0;
    }

    fn name(&self) -> &str {
        "ducked-send"
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        self.predelay_samples as u32
    }

    fn gain_reduction_db(&self) -> f64 {
        self.duck_db
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.max_reduction_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 48_000;

    fn tone(freq: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let value = (amplitude * (TAU * freq * index as f64 / RATE as f64).sin()) as f32;
            buffer.push(value);
            buffer.push(value);
        }
        buffer
    }

    fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (RATE as f64 * seconds) as usize * 2]
    }

    fn rms(buffer: &[f32], start_frame: usize, end_frame: usize) -> f64 {
        let mut sum = 0.0;
        let mut count = 0;
        for frame in start_frame..end_frame.min(buffer.len() / 2) {
            for channel in 0..2 {
                let value = buffer[frame * 2 + channel] as f64;
                sum += value * value;
                count += 1;
            }
        }
        (sum / count.max(1) as f64).sqrt()
    }

    fn make() -> DuckedSend {
        let mut effect = DuckedSend::new(RATE);
        effect.set_param("predelay", 30.0);
        effect.set_param("depth", 12.0);
        effect.set_param("attack", 20.0);
        effect.set_param("release", 300.0);
        effect.set_param("width", 100.0);
        effect
    }

    #[test]
    fn the_return_drops_by_the_depth_within_the_attack() {
        let wet = tone(1_000.0, 0.25, 1.0);
        let key = tone(500.0, 0.5, 1.0);
        let mut reference = wet.clone();
        let mut effect = make();
        effect.process(&mut reference, RATE);
        let reference_rms = rms(&reference, RATE as usize / 2, RATE as usize);

        let mut ducked = wet.clone();
        let mut effect = make();
        effect.process_with_sidechain(&mut ducked, &key, RATE, None);
        let split = ((0.030 + 0.040) * RATE as f64) as usize;
        let ducked_rms = rms(&ducked, split, split + (RATE as usize / 20));
        let drop_db = 20.0 * (ducked_rms / reference_rms).log10();
        assert!(
            (drop_db + 12.0).abs() < 1.0,
            "the return dropped {drop_db:.2} dB"
        );
    }

    #[test]
    fn the_release_returns_within_one_and_a_half_times_the_release() {
        let wet = tone(1_000.0, 0.25, 2.5);
        let mut key = tone(500.0, 0.5, 1.0);
        key.extend(silence(1.5));
        let mut reference = wet.clone();
        let mut effect = make();
        effect.process(&mut reference, RATE);

        let mut ducked = wet.clone();
        let mut effect = make();
        effect.process_with_sidechain(&mut ducked, &key, RATE, None);
        let key_end = RATE as usize;
        for factor in [1.0, 1.5] {
            let at = key_end + (0.300 * factor * RATE as f64) as usize;
            let reference_rms = rms(&reference, at, at + 480);
            let ducked_rms = rms(&ducked, at, at + 480);
            let drop_db = 20.0 * (ducked_rms / reference_rms.max(1e-12)).log10();
            if factor == 1.0 {
                assert!(drop_db > -1.0, "still {drop_db:.2} dB down at release");
                assert!(drop_db < -0.01, "release moved faster than stated");
            } else {
                assert!(drop_db > -0.2, "still {drop_db:.2} dB down at 1.5x release");
            }
        }
    }

    #[test]
    fn a_steady_dry_tone_does_not_modulate_the_return() {
        let wet = tone(1_000.0, 0.25, 2.0);
        let key = tone(300.0, 0.5, 2.0);
        let mut ducked = wet.clone();
        let mut effect = make();
        effect.process_with_sidechain(&mut ducked, &key, RATE, None);
        let mut levels = Vec::new();
        for window in 0..6 {
            let start = (0.4 * RATE as f64) as usize + window * (RATE as usize / 10);
            levels.push(20.0 * rms(&ducked, start, start + RATE as usize / 10).log10());
        }
        let max = levels.iter().cloned().fold(f64::MIN, f64::max);
        let min = levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(max - min < 0.5, "the return moved by {:.2} dB", max - min);
    }

    #[test]
    fn no_sidechain_leaves_the_delayed_return_unchanged() {
        let wet = tone(1_000.0, 0.25, 0.5);
        let mut output = wet.clone();
        let mut effect = make();
        effect.process(&mut output, RATE);
        let delay = (0.030 * RATE as f64) as usize;
        for frame in delay..wet.len() / 2 {
            for channel in 0..2 {
                let expected = wet[(frame - delay) * 2 + channel];
                assert!(
                    (output[frame * 2 + channel] - expected).abs() < 1e-6,
                    "unexpected change at frame {frame}"
                );
            }
        }
    }

    #[test]
    fn predelay_delays_the_return_and_the_device_reports_no_latency() {
        let mut impulse = vec![0.0f32; RATE as usize * 2];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        let mut output = impulse.clone();
        let mut effect = make();
        effect.process(&mut output, RATE);
        let peak = output
            .chunks_exact(2)
            .enumerate()
            .max_by(|(_, a), (_, b)| a[0].abs().total_cmp(&b[0].abs()))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(peak, (0.030 * RATE as f64) as usize);
        assert_eq!(effect.latency_samples(), 0);
    }

    #[test]
    fn width_is_applied_to_the_wet_only_and_zero_width_is_mono() {
        let mut input = tone(1_000.0, 0.25, 1.0);
        for frame in input.chunks_exact_mut(2) {
            frame[0] *= 0.5;
        }
        let mut narrow = input.clone();
        let mut effect = make();
        effect.set_param("width", 0.0);
        effect.process(&mut narrow, RATE);
        for frame in narrow.chunks_exact(2) {
            assert!((frame[0] - frame[1]).abs() < 1e-6);
        }
        let mut wide = input.clone();
        let mut effect = make();
        effect.set_param("width", 200.0);
        effect.process(&mut wide, RATE);
        let wide_side = {
            let mut sum = 0.0;
            let mut count = 0;
            for frame in wide.chunks_exact(2).skip(RATE as usize / 2) {
                let side = 0.5 * (frame[0] - frame[1]) as f64;
                sum += side * side;
                count += 1;
            }
            (sum / count.max(1) as f64).sqrt()
        };
        assert!(wide_side > 0.01, "width did not widen the return");
    }

    #[test]
    fn zero_depth_is_an_exact_delayed_bypass() {
        let wet = tone(1_000.0, 0.25, 0.5);
        let key = tone(300.0, 0.5, 0.5);
        let mut output = wet.clone();
        let mut effect = make();
        effect.set_param("depth", 0.0);
        effect.process_with_sidechain(&mut output, &key, RATE, None);
        let delay = (0.030 * RATE as f64) as usize;
        for frame in delay..wet.len() / 2 {
            for channel in 0..2 {
                let expected = wet[(frame - delay) * 2 + channel];
                assert!((output[frame * 2 + channel] - expected).abs() < 1e-6);
            }
        }
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let wet = tone(1_000.0, 0.25, 2.5);
        let mut key = tone(500.0, 0.5, 1.0);
        key.extend(silence(1.5));
        let mut ducked = wet.clone();
        let mut effect = make();
        let block = 96;
        let mut reduction = Vec::new();
        let mut offset = 0;
        while offset < ducked.len() {
            let end = (offset + block * 2).min(ducked.len());
            effect.process_with_sidechain(
                &mut ducked[offset..end],
                &key[offset..end.min(key.len())],
                RATE,
                None,
            );
            reduction.push(effect.gain_reduction_db());
            offset = end;
        }
        let depth = 12.0;
        let attack = reduction
            .iter()
            .position(|value| *value >= depth * 0.95)
            .map(|index| index as f64 * block as f64 / RATE as f64 * 1000.0);
        let key_end = (1.0 * RATE as f64) as usize / block;
        let release = reduction
            .iter()
            .enumerate()
            .skip(key_end)
            .find(|(_, value)| **value <= depth * 0.05)
            .map(|(index, _)| (index - key_end) as f64 * block as f64 / RATE as f64 * 1000.0);
        let at_15 = reduction
            .get(key_end + (0.450 * RATE as f64 / block as f64) as usize)
            .copied()
            .unwrap_or(0.0);
        println!(
            "ducked send: depth {depth:.0} dB, 95% attack at {:?} ms, 95% release at {:?} ms, residual at 1.5x release {at_15:.3} dB",
            attack, release
        );

        let mut levels = Vec::new();
        for window in 0..6 {
            let start = (0.4 * RATE as f64) as usize + window * (RATE as usize / 10);
            levels.push(20.0 * rms(&ducked, start, start + RATE as usize / 10).log10());
        }
        let max = levels.iter().cloned().fold(f64::MIN, f64::max);
        let min = levels.iter().cloned().fold(f64::MAX, f64::min);
        println!("ducked send: steady return ripple {:.3} dB", max - min);
        println!(
            "ducked send: predelay {} samples, reported latency {}",
            effect.tail_samples(),
            effect.latency_samples()
        );
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let wet = tone(1_000.0, 0.25, 0.5);
        let key = tone(500.0, 0.5, 0.5);
        let mut small = wet.clone();
        let mut large = wet.clone();
        let mut first = make();
        let mut second = make();
        let mut key_offset = 0;
        for chunk in small.chunks_mut(97 * 2) {
            let frames = chunk.len() / 2;
            first.process_with_sidechain(
                chunk,
                &key[key_offset..(key_offset + frames * 2).min(key.len())],
                RATE,
                None,
            );
            key_offset = (key_offset + frames * 2).min(key.len());
        }
        let mut key_offset = 0;
        for chunk in large.chunks_mut(480 * 2) {
            let frames = chunk.len() / 2;
            second.process_with_sidechain(
                chunk,
                &key[key_offset..(key_offset + frames * 2).min(key.len())],
                RATE,
                None,
            );
            key_offset = (key_offset + frames * 2).min(key.len());
        }
        let difference = small
            .iter()
            .zip(&large)
            .fold(0.0_f32, |diff, (a, b)| diff.max((a - b).abs()));
        assert!(
            difference < 1e-7,
            "block size changed the output by {difference}"
        );
    }
}
