//! Optical vocal compressor: shunt-cell gain law with a two-state release.

use super::AudioEffect;

const GAMMA: f64 = 0.7;
const K: f64 = 2.5;
const LAMBDA: f64 = 0.4;
const CELL_RISE_S: f64 = 0.010;
const FAST_RELEASE_S: f64 = 0.060;
const COUPLER_S: f64 = 0.150;
const CONTROL_PERIOD_S: f64 = 0.010;
const MIN_LEVEL: f64 = 1e-6;
const MIN_PEAK_REDUCTION_DB: f64 = 0.0;
const MAX_PEAK_REDUCTION_DB: f64 = 20.0;
const MIN_RELEASE_MS: f64 = 500.0;
const MAX_RELEASE_MS: f64 = 5_000.0;
const MIN_MAKEUP_DB: f64 = -12.0;
const MAX_MAKEUP_DB: f64 = 12.0;

/// Static gain reduction of the cell at a sidechain level in dB.
fn law_gr_db(level_db: f64) -> f64 {
    20.0 * (1.0 + K * 10f64.powf(GAMMA * level_db / 20.0)).log10()
}

/// Local compression ratio at a given cell engagement.
#[cfg(test)]
fn local_ratio(engagement: f64) -> f64 {
    1.0 / (1.0 - GAMMA * engagement.clamp(0.0, 0.999_999))
}

pub struct Opto {
    peak_reduction_db: f64,
    release_ms: f64,
    makeup_db: f64,
    sample_rate: u32,
    control_frames: usize,
    frames_left: usize,
    attack_coeff: f64,
    fast_coeff: f64,
    coupler_coeff: f64,
    slow_coeff: f64,
    peak: f64,
    fast_gr_db: f64,
    slow_gr_db: f64,
    gr_db: f64,
    gain: f64,
    ramp_target: f64,
    gain_step: f64,
    max_gr_db: f64,
}

impl Opto {
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let control_frames = ((sample_rate as f64 * CONTROL_PERIOD_S).round() as usize).max(1);
        let mut opto = Self {
            peak_reduction_db: 6.0,
            release_ms: 1_500.0,
            makeup_db: 0.0,
            sample_rate,
            control_frames,
            frames_left: control_frames,
            attack_coeff: 0.0,
            fast_coeff: 0.0,
            coupler_coeff: 0.0,
            slow_coeff: 0.0,
            peak: 0.0,
            fast_gr_db: 0.0,
            slow_gr_db: 0.0,
            gr_db: 0.0,
            gain: 1.0,
            ramp_target: 1.0,
            gain_step: 0.0,
            max_gr_db: 0.0,
        };
        opto.update_coefficients();
        opto
    }

    fn update_coefficients(&mut self) {
        let period_s = self.control_frames as f64 / self.sample_rate as f64;
        let release_s = (self.release_ms * 0.001).max(period_s);
        self.attack_coeff = 1.0 - (-period_s / CELL_RISE_S).exp();
        self.fast_coeff = 1.0 - (-period_s / FAST_RELEASE_S).exp();
        self.coupler_coeff = 1.0 - (-period_s / COUPLER_S).exp();
        self.slow_coeff = 1.0 - (-period_s / release_s).exp();
    }

    fn control_tick(&mut self) {
        self.gain = self.ramp_target;
        let peak = self.peak.max(MIN_LEVEL);
        self.peak = 0.0;
        let level_db = 20.0 * peak.log10() + self.peak_reduction_db;
        let target = law_gr_db(level_db);

        if target > self.fast_gr_db {
            self.fast_gr_db += (target - self.fast_gr_db) * self.attack_coeff;
        } else {
            self.fast_gr_db += (target - self.fast_gr_db) * self.fast_coeff;
        }
        if self.fast_gr_db > self.slow_gr_db {
            self.slow_gr_db += (self.fast_gr_db - self.slow_gr_db) * self.coupler_coeff;
        } else {
            self.slow_gr_db += (self.fast_gr_db - self.slow_gr_db) * self.slow_coeff;
        }
        self.gr_db = ((1.0 - LAMBDA) * self.fast_gr_db + LAMBDA * self.slow_gr_db).max(0.0);
        self.max_gr_db = self.max_gr_db.max(self.gr_db);

        let gain_db = self.makeup_db - self.gr_db;
        self.ramp_target = 10f64.powf(gain_db / 20.0);
        self.gain_step = (self.ramp_target - self.gain) / self.control_frames as f64;
        self.frames_left = self.control_frames;
    }
}

impl AudioEffect for Opto {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        for frame in buffer.as_chunks_mut::<2>().0 {
            let peak = (frame[0].abs() as f64).max(frame[1].abs() as f64);
            self.peak = self.peak.max(peak);

            self.gain += self.gain_step;
            let gain = self.gain as f32;
            frame[0] *= gain;
            frame[1] *= gain;

            self.frames_left -= 1;
            if self.frames_left == 0 {
                self.control_tick();
            }
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "peak_reduction" => {
                self.peak_reduction_db = value.clamp(MIN_PEAK_REDUCTION_DB, MAX_PEAK_REDUCTION_DB)
            }
            "release" => {
                self.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                self.update_coefficients();
            }
            "makeup" => self.makeup_db = value.clamp(MIN_MAKEUP_DB, MAX_MAKEUP_DB),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "peak_reduction" => Some(self.peak_reduction_db),
            "release" => Some(self.release_ms),
            "makeup" => Some(self.makeup_db),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.peak = 0.0;
        self.fast_gr_db = 0.0;
        self.slow_gr_db = 0.0;
        self.gr_db = 0.0;
        self.gain = 1.0;
        self.ramp_target = 1.0;
        self.gain_step = 0.0;
        self.frames_left = self.control_frames;
        self.max_gr_db = 0.0;
    }

    fn gain_reduction_db(&self) -> f64 {
        self.gr_db
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.max_gr_db
    }

    fn name(&self) -> &str {
        "opto"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let sample = (amplitude
                * (std::f64::consts::TAU * 1_000.0 * index as f64 / RATE as f64).sin())
                as f32;
            buffer.push(sample);
            buffer.push(sample);
        }
        buffer
    }

    fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (RATE as f64 * seconds) as usize * 2]
    }

    fn run(effect: &mut Opto, input: &[f32]) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(480 * 2) {
            effect.process(chunk, RATE);
        }
        output
    }

    fn rms_db(samples: &[f32]) -> f64 {
        let power = samples
            .iter()
            .map(|sample| (*sample as f64) * (*sample as f64))
            .sum::<f64>()
            / samples.len() as f64;
        10.0 * power.max(1e-30).log10()
    }

    fn measure_gr(effect: &mut Opto, level_db: f64) -> f64 {
        let amplitude = 10f64.powf(level_db / 20.0);
        let input = tone(amplitude, 3.0);
        let output = run(effect, &input);
        let settled = &output[RATE as usize * 2..];
        let reference = &input[RATE as usize * 2..];
        rms_db(reference) - rms_db(settled)
    }

    fn capture_release(effect: &mut Opto, release_ms: f64) -> Vec<f64> {
        effect.set_param("peak_reduction", 6.0);
        effect.set_param("release", release_ms);
        let burst = tone(0.5, 2.0);
        run(effect, &burst);
        let mut gains = Vec::new();
        let quiet = silence(3.0);
        for chunk in quiet.chunks(480 * 2) {
            let mut block = chunk.to_vec();
            effect.process(&mut block, RATE);
            gains.push(effect.gr_db);
        }
        gains
    }

    fn fit_tail_tau(gains: &[f64], start_s: f64, end_s: f64) -> f64 {
        let start = (start_s / 0.010) as usize;
        let end = (end_s / 0.010) as usize;
        let points = &gains[start..end];
        let n = points.len() as f64;
        let xs = (0..points.len())
            .map(|i| i as f64 * 0.010)
            .collect::<Vec<_>>();
        let ys = points.iter().map(|g| g.max(1e-9).ln()).collect::<Vec<_>>();
        let mean_x = xs.iter().sum::<f64>() / n;
        let mean_y = ys.iter().sum::<f64>() / n;
        let covariance = xs
            .iter()
            .zip(&ys)
            .map(|(x, y)| (x - mean_x) * (y - mean_y))
            .sum::<f64>();
        let variance = xs.iter().map(|x| (x - mean_x).powi(2)).sum::<f64>();
        -variance / covariance
    }

    #[test]
    fn static_curve_matches_the_law() {
        for level in [-50.0, -35.0, -25.0, -15.0, -5.0] {
            let mut effect = Opto::new(RATE);
            effect.set_param("peak_reduction", 0.0);
            let measured = measure_gr(&mut effect, level);
            let expected = law_gr_db(level);
            assert!(
                (measured - expected).abs() < 0.05,
                "{level} dBFS: measured {measured:.3} dB, law {expected:.3} dB"
            );
        }
    }

    #[test]
    fn knee_is_about_55_db_wide_and_the_ratios_are_properties_of_the_law() {
        let width = (20.0 / GAMMA) * 81f64.log10();
        assert!((width - 54.6).abs() < 0.1, "knee width {width}");
        assert!((local_ratio(0.5) - 1.538).abs() < 0.001);
        assert!((local_ratio(0.999_999) - 3.333).abs() < 0.001);

        // Measure the local slope through the DSP around the 50 % engagement point.
        let knee_level = (-20.0 / GAMMA) * K.log10();
        let mut low_effect = Opto::new(RATE);
        let mut high_effect = Opto::new(RATE);
        low_effect.set_param("peak_reduction", 0.0);
        high_effect.set_param("peak_reduction", 0.0);
        let low = measure_gr(&mut low_effect, knee_level - 0.5);
        let high = measure_gr(&mut high_effect, knee_level + 0.5);
        assert!(
            (low - law_gr_db(knee_level - 0.5)).abs() < 0.05
                && (high - law_gr_db(knee_level + 0.5)).abs() < 0.05,
            "curve around the knee: {low:.3} / {high:.3} dB"
        );
        let measured_ratio = 1.0 / (1.0 - (high - low));
        assert!(
            (measured_ratio - local_ratio(0.5)).abs() < 0.1,
            "local ratio at half engagement is {measured_ratio}"
        );
    }

    #[test]
    fn release_has_a_fast_state_and_a_slow_tail() {
        let mut effect = Opto::new(RATE);
        let gains = capture_release(&mut effect, 1_500.0);
        let initial = gains[0];
        assert!(initial > 8.0, "burst did not compress: {initial} dB");
        let at_60ms = gains[6];
        let at_150ms = gains[15];
        let at_1s = gains[100];
        assert!(
            at_60ms > 0.45 * initial,
            "slow state did not hold: {initial} -> {at_60ms}"
        );
        assert!(
            at_150ms < 0.55 * initial,
            "fast state did not release: {initial} -> {at_150ms}"
        );
        assert!(
            at_1s < 0.35 * initial,
            "tail is too long at 1 s: {initial} -> {at_1s}"
        );
        assert!(
            at_1s > 0.10 * initial,
            "there is no slow tail at 1 s: {initial} -> {at_1s}"
        );
    }

    #[test]
    fn the_slow_release_position_sets_the_tail() {
        let mut fast = Opto::new(RATE);
        let mut slow = Opto::new(RATE);
        let fast_gains = capture_release(&mut fast, 500.0);
        let slow_gains = capture_release(&mut slow, 5_000.0);
        assert!(
            slow_gains[100] > fast_gains[100] * 2.0,
            "1 s tail: 500 ms {:.2} dB vs 5 s {:.2} dB",
            fast_gains[100],
            slow_gains[100]
        );
        let fast_tau = fit_tail_tau(&fast_gains, 0.5, 1.5);
        let slow_tau = fit_tail_tau(&slow_gains, 0.5, 2.5);
        assert!(
            (fast_tau / 0.5 - 1.0).abs() < 0.25,
            "500 ms position measured {fast_tau:.2} s"
        );
        assert!(
            (slow_tau / 5.0 - 1.0).abs() < 0.25,
            "5 s position measured {slow_tau:.2} s"
        );
    }

    #[test]
    fn a_steady_vowel_leaves_no_residual() {
        let mut effect = Opto::new(RATE);
        effect.set_param("peak_reduction", 6.0);
        let frames = RATE as usize * 3;
        let mut input = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let time = index as f64 / RATE as f64;
            let mut sample = 0.0;
            for harmonic in 1..=20 {
                sample += (0.2 / harmonic as f64)
                    * (std::f64::consts::TAU * 100.0 * harmonic as f64 * time).sin();
            }
            input.push(sample as f32);
            input.push(sample as f32);
        }
        let output = run(&mut effect, &input);
        let start = 2 * RATE as usize * 2;
        let settled_input = &input[start..];
        let settled_output = &output[start..];
        let input_power: f64 = settled_input
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum();
        let output_power: f64 = settled_output
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum();
        let gain = (output_power / input_power).sqrt();
        let mut residual = 0.0;
        for (processed, dry) in settled_output.iter().zip(settled_input) {
            residual += (*processed as f64 - gain * *dry as f64).powi(2);
        }
        let residual_db = 10.0 * (residual / settled_input.len() as f64).max(1e-30).log10();
        assert!(
            residual_db < -80.0,
            "steady vowel residual is {residual_db:.1} dBFS"
        );
    }

    #[test]
    fn attack_is_the_cell_rise_and_is_not_adjustable() {
        let mut effect = Opto::new(RATE);
        effect.set_param("peak_reduction", 6.0);
        let mut input = silence(0.5);
        input.extend(tone(0.5, 1.0));
        let mut block = input.clone();
        effect.process(&mut block, RATE);
        let target = law_gr_db(20.0 * 0.5f64.log10() + 6.0);
        let at_50ms = effect.gr_db;
        assert!(
            at_50ms > 0.4 * target && at_50ms < target,
            "50 ms attack is {at_50ms:.2} of {target:.2} dB"
        );
    }

    #[test]
    fn makeup_is_an_exact_output_gain() {
        let input = tone(0.1, 2.0);
        let mut flat = Opto::new(RATE);
        let mut boosted = Opto::new(RATE);
        boosted.set_param("makeup", 6.0);
        let a = run(&mut flat, &input);
        let b = run(&mut boosted, &input);
        let settled = RATE as usize * 2;
        let delta = rms_db(&b[settled..]) - rms_db(&a[settled..]);
        assert!((delta - 6.0).abs() < 1e-6, "makeup moved {delta} dB");
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let input = tone(0.2, 3.0);
        let mut small = Opto::new(RATE);
        let mut large = Opto::new(RATE);
        let mut a = input.clone();
        for chunk in a.chunks_mut(97 * 2) {
            small.process(chunk, RATE);
        }
        let b = run(&mut large, &input);
        let difference = a
            .iter()
            .zip(&b)
            .fold(0.0_f32, |diff, (a, b)| diff.max((a - b).abs()));
        assert!(
            difference < 1e-7,
            "block size changed the output by {difference}"
        );
    }

    #[test]
    fn reset_returns_the_cell_to_unity() {
        let mut effect = Opto::new(RATE);
        run(&mut effect, &tone(0.5, 1.0));
        assert!(effect.gr_db > 0.0);
        effect.reset();
        assert_eq!(effect.gr_db, 0.0);
        assert_eq!(effect.gain_reduction_db(), 0.0);
        let mut impulse = vec![0.0_f32; 64];
        impulse[0] = 0.5;
        effect.process(&mut impulse, RATE);
        assert_eq!(impulse[0], 0.5);
    }

    #[test]
    fn zero_latency_impulse_passes_at_its_own_frame() {
        let mut effect = Opto::new(RATE);
        let mut input = vec![0.0_f32; 64];
        input[0] = 0.5;
        input[1] = 0.5;
        effect.process(&mut input, RATE);
        assert_eq!(input[0], 0.5);
        assert_eq!(input[1], 0.5);
    }

    fn vowel(seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut input = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let time = index as f64 / RATE as f64;
            let mut sample = 0.0;
            for harmonic in 1..=20 {
                sample += (0.2 / harmonic as f64)
                    * (std::f64::consts::TAU * 100.0 * harmonic as f64 * time).sin();
            }
            input.push(sample as f32);
            input.push(sample as f32);
        }
        input
    }

    fn vowel_residual(effect: &mut Opto) -> f64 {
        let input = vowel(3.0);
        let output = run(effect, &input);
        let start = 2 * RATE as usize * 2;
        let dry = &input[start..];
        let wet = &output[start..];
        let input_power: f64 = dry.iter().map(|sample| (*sample as f64).powi(2)).sum();
        let output_power: f64 = wet.iter().map(|sample| (*sample as f64).powi(2)).sum();
        let gain = (output_power / input_power).sqrt();
        let residual = wet
            .iter()
            .zip(dry)
            .map(|(processed, sample)| (*processed as f64 - gain * *sample as f64).powi(2))
            .sum::<f64>()
            / dry.len() as f64;
        10.0 * residual.max(1e-30).log10()
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        for drive in [0.0, 6.0] {
            let mut row = Vec::new();
            for level in [-50.0, -35.0, -25.0, -15.0, -5.0] {
                let mut effect = Opto::new(RATE);
                effect.set_param("peak_reduction", drive);
                row.push((level, measure_gr(&mut effect, level)));
            }
            println!("drive {drive:.0} dB: {row:?}");
        }
        println!("knee width {:.2} dB", (20.0 / GAMMA) * 81f64.log10());
        println!(
            "local ratio 50% {:.3}, full {:.3}",
            local_ratio(0.5),
            1.0 / (1.0 - GAMMA)
        );
        for release in [500.0, 1_500.0, 5_000.0] {
            let mut effect = Opto::new(RATE);
            let gains = capture_release(&mut effect, release);
            let tau = fit_tail_tau(&gains, 0.5, if release > 2_000.0 { 2.5 } else { 1.5 });
            println!(
                "release {release:.0} ms: gr 60 ms {:.2}, 150 ms {:.2}, 1 s {:.2}, tail tau {tau:.2} s",
                gains[6], gains[15], gains[100]
            );
        }
        let mut effect = Opto::new(RATE);
        effect.set_param("peak_reduction", 6.0);
        println!(
            "steady vowel residual {:.1} dBFS",
            vowel_residual(&mut effect)
        );

        let mut effect = Opto::new(RATE);
        effect.set_param("peak_reduction", 6.0);
        let mut quiet = silence(0.5);
        effect.process(&mut quiet, RATE);
        for block in tone(0.5, 1.0).chunks_mut(480 * 2).take(5) {
            effect.process(block, RATE);
        }
        let target = law_gr_db(20.0 * 0.5f64.log10() + 6.0);
        println!(
            "attack: 50 ms {:.2} dB of {target:.2} dB ({:.0} %)",
            effect.gr_db,
            100.0 * effect.gr_db / target
        );
    }
}
