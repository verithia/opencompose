//! FET vocal compressor: a static feedback loop around a superlinear channel conductance.

use super::AudioEffect;
use super::oversampler::{LATENCY, Oversampler4x};

/// Conductance exponent of the channel; the loop asymptote is `q + 1`.
const ELEMENT_EXPONENT: i32 = 6;
/// Share of the instantaneous terminal voltage that adds to the channel control.
const INSTANT_TERM: f64 = 0.25;
/// Quiescent bias at the low-ratio end and at pinch-off.
const MAX_BIAS: f64 = 2.0;
const MIN_BIAS: f64 = 0.02;
/// Loop gain at the low-ratio end; it rises as the bias approaches pinch-off.
const MIN_BETA: f64 = 0.03;
const MIN_DRIVE_DB: f64 = 0.0;
const MAX_DRIVE_DB: f64 = 24.0;
const MIN_ATTACK_MS: f64 = 0.05;
const MAX_ATTACK_MS: f64 = 20.0;
const MIN_RELEASE_MS: f64 = 20.0;
const MAX_RELEASE_MS: f64 = 2_000.0;
const MIN_OUTPUT_DB: f64 = -12.0;
const MAX_OUTPUT_DB: f64 = 12.0;

/// Loop output from the static conductance law, solving `x = y + beta*(u^q - b^q)*y`.
fn loop_output(x: f64, b: f64, bq: f64, beta: f64, e: f64, q: i32, kappa: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let target = x.abs();
    let mut low = 0.0;
    let mut high = target;
    let guess = target / (1.0 + beta * ((b + e + kappa * target).powi(q) - bq));
    let mut y = if guess.is_finite() && guess > 0.0 {
        guess.min(target)
    } else {
        target * 0.5
    };
    for _ in 0..12 {
        let u = b + e + kappa * y;
        let h = u.powi(q) - bq;
        let f = y + beta * h * y - target;
        if f.abs() <= 1e-15 * target {
            return sign * y;
        }
        if f > 0.0 {
            high = y;
        } else {
            low = y;
        }
        let derivative = 1.0 + beta * (q as f64 * u.powi(q - 1) * kappa * y + h);
        let next = if derivative > 0.0 {
            y - f / derivative
        } else {
            f64::NAN
        };
        y = if next.is_finite() && next > low && next < high {
            next
        } else {
            0.5 * (low + high)
        };
        if high - low <= 1e-12 * target {
            break;
        }
    }
    sign * y
}

/// Self-consistent peak of the same law for a steady sine: `e` equals the output peak.
#[cfg(test)]
fn steady_peak(x: f64, b: f64, beta: f64, q: i32, kappa: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let bq = b.powi(q);
    let mut low = 0.0;
    let mut high = x;
    for _ in 0..60 {
        let y = 0.5 * (low + high);
        let u = b + (1.0 + kappa) * y;
        let f = y + beta * (u.powi(q) - bq) * y - x;
        if f > 0.0 {
            high = y;
        } else {
            low = y;
        }
    }
    0.5 * (low + high)
}

/// Quiescent bias and loop gain for a ratio position in 0..1.
fn bias_state(position: f64) -> (f64, f64) {
    let position = position.clamp(0.0, 1.0);
    let b = MIN_BIAS + (MAX_BIAS - MIN_BIAS) * (1.0 - position).powi(2);
    let beta = MIN_BETA * (MAX_BIAS / b).powi(3);
    (b, beta)
}

pub struct Fet {
    oversampler: Oversampler4x,
    sample_rate: f64,
    drive_db: f64,
    attack_ms: f64,
    release_ms: f64,
    output_db: f64,
    bias: f64,
    element_exponent: i32,
    instant_term: f64,
    b: f64,
    bq: f64,
    beta: f64,
    drive_gain: f64,
    output_gain: f64,
    attack_coeff: f64,
    release_coeff: f64,
    envelope: f64,
    gr_db: f64,
    max_gr_db: f64,
}

impl Fet {
    pub fn new(sample_rate: u32) -> Self {
        let mut fet = Self {
            oversampler: Oversampler4x::new(),
            sample_rate: sample_rate.max(1) as f64,
            drive_db: 0.0,
            attack_ms: 0.3,
            release_ms: 120.0,
            output_db: 0.0,
            bias: 0.25,
            element_exponent: ELEMENT_EXPONENT,
            instant_term: INSTANT_TERM,
            b: 0.0,
            bq: 0.0,
            beta: 0.0,
            drive_gain: 1.0,
            output_gain: 1.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            envelope: 0.0,
            gr_db: 0.0,
            max_gr_db: 0.0,
        };
        fet.update_bias();
        fet.update_coefficients();
        fet
    }

    fn update_bias(&mut self) {
        let (b, beta) = bias_state(self.bias);
        self.b = b;
        self.bq = b.powi(self.element_exponent);
        self.beta = beta;
    }

    fn update_coefficients(&mut self) {
        self.attack_coeff = 1.0 - (-1.0 / (self.attack_ms * 1e-3 * self.sample_rate)).exp();
        self.release_coeff = 1.0 - (-1.0 / (self.release_ms * 1e-3 * self.sample_rate)).exp();
    }

    fn update_gains(&mut self) {
        self.drive_gain = 10f64.powf(self.drive_db / 20.0);
        self.output_gain = 10f64.powf(self.output_db / 20.0);
    }

    #[cfg(test)]
    fn set_element(&mut self, q: i32, kappa: f64) {
        self.element_exponent = q;
        self.instant_term = kappa;
        self.update_bias();
    }

    #[cfg(test)]
    fn envelope(&self) -> f64 {
        self.envelope
    }
}

impl AudioEffect for Fet {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let drive_gain = self.drive_gain;
        let output_gain = self.output_gain;
        let b = self.b;
        let bq = self.bq;
        let beta = self.beta;
        let q = self.element_exponent;
        let kappa = self.instant_term;
        for frame in buffer.as_chunks_mut::<2>().0 {
            let envelope = self.envelope;
            let input = [frame[0] as f64 * drive_gain, frame[1] as f64 * drive_gain];
            let mut input_peak: f64 = 0.0;
            let mut output_peak: f64 = 0.0;
            let wet = self.oversampler.process(input, |high_rate| {
                let y = [
                    loop_output(high_rate[0], b, bq, beta, envelope, q, kappa),
                    loop_output(high_rate[1], b, bq, beta, envelope, q, kappa),
                ];
                for channel in 0..2 {
                    input_peak = input_peak.max(high_rate[channel].abs());
                    output_peak = output_peak.max(y[channel].abs());
                }
                y
            });
            let target = output_peak;
            let coefficient = if target > self.envelope {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.envelope += (target - self.envelope) * coefficient;
            let frame_gr = if output_peak > 0.0 {
                20.0 * (input_peak / output_peak).log10()
            } else {
                0.0
            };
            self.gr_db = frame_gr.max(0.0);
            self.max_gr_db = self.max_gr_db.max(self.gr_db);
            frame[0] = (wet[0] * output_gain) as f32;
            frame[1] = (wet[1] * output_gain) as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "drive" => {
                self.drive_db = value.clamp(MIN_DRIVE_DB, MAX_DRIVE_DB);
                self.update_gains();
            }
            "attack" => {
                self.attack_ms = value.clamp(MIN_ATTACK_MS, MAX_ATTACK_MS);
                self.update_coefficients();
            }
            "release" => {
                self.release_ms = value.clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
                self.update_coefficients();
            }
            "output" => {
                self.output_db = value.clamp(MIN_OUTPUT_DB, MAX_OUTPUT_DB);
                self.update_gains();
            }
            "bias" => {
                self.bias = value.clamp(0.0, 1.0);
                self.update_bias();
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "drive" => Some(self.drive_db),
            "attack" => Some(self.attack_ms),
            "release" => Some(self.release_ms),
            "output" => Some(self.output_db),
            "bias" => Some(self.bias),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.oversampler.reset();
        self.envelope = 0.0;
        self.gr_db = 0.0;
        self.max_gr_db = 0.0;
    }

    fn latency_samples(&self) -> u32 {
        LATENCY as u32
    }

    fn tail_samples(&self) -> u32 {
        32
    }

    fn gain_reduction_db(&self) -> f64 {
        self.gr_db
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.max_gr_db
    }

    fn name(&self) -> &str {
        "fet"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 48_000;

    fn tone(amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let sample = (amplitude * (TAU * 1_000.0 * index as f64 / RATE as f64).sin()) as f32;
            buffer.push(sample);
            buffer.push(sample);
        }
        buffer
    }

    fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (RATE as f64 * seconds) as usize * 2]
    }

    fn step(amplitude: f64, seconds: f64) -> Vec<f32> {
        vec![amplitude as f32; (RATE as f64 * seconds) as usize * 2]
    }

    fn run(effect: &mut Fet, input: &[f32]) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(480 * 2) {
            effect.process(chunk, RATE);
        }
        output
    }

    fn peak(samples: &[f32]) -> f64 {
        samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs() as f64))
    }

    fn rms_db(samples: &[f32]) -> f64 {
        let power = samples
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum::<f64>()
            / samples.len() as f64;
        10.0 * power.max(1e-30).log10()
    }

    fn settled_peak(effect: &mut Fet, amplitude: f64, seconds: f64) -> f64 {
        let output = run(effect, &tone(amplitude, seconds));
        let start = (RATE as f64 * (seconds - 1.0)) as usize * 2;
        peak(&output[start..])
    }

    fn measured_gr(effect: &mut Fet, level_db: f64) -> f64 {
        let amplitude = 10f64.powf(level_db / 20.0);
        let output_peak = settled_peak(effect, amplitude, 3.0);
        20.0 * (amplitude / output_peak).log10()
    }

    fn local_ratio(x: f64, b: f64, beta: f64, q: i32, kappa: f64) -> f64 {
        let step: f64 = 1e-3;
        let low = steady_peak(x * (-step).exp(), b, beta, q, kappa);
        let high = steady_peak(x * step.exp(), b, beta, q, kappa);
        2.0 * step / (high / low).ln()
    }

    fn harmonic(samples: &[f32], frequency: f64) -> f64 {
        let (mut real, mut imaginary) = (0.0, 0.0);
        let mut frames = 0;
        for (index, frame) in samples.chunks_exact(2).enumerate() {
            let phase = TAU * frequency * index as f64 / RATE as f64;
            real += frame[0] as f64 * phase.cos();
            imaginary -= frame[0] as f64 * phase.sin();
            frames += 1;
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / frames as f64
    }

    fn thd(samples: &[f32]) -> f64 {
        let fundamental = harmonic(samples, 1_000.0);
        let harmonics = (2..=10)
            .map(|order| harmonic(samples, 1_000.0 * order as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        harmonics / fundamental.max(1e-30)
    }

    fn settled_thd(effect: &mut Fet, amplitude: f64) -> f64 {
        let output = run(effect, &tone(amplitude, 3.0));
        let start = (RATE as f64 * 1.5) as usize * 2;
        thd(&output[start..])
    }

    fn fit_tau(points: &[(f64, f64)], start_s: f64, end_s: f64) -> f64 {
        let points = points
            .iter()
            .filter(|(time, _)| *time >= start_s && *time <= end_s)
            .collect::<Vec<_>>();
        let n = points.len() as f64;
        let mean_x = points.iter().map(|(time, _)| *time).sum::<f64>() / n;
        let mean_y = points
            .iter()
            .map(|(_, value)| value.max(1e-12).ln())
            .sum::<f64>()
            / n;
        let covariance = points
            .iter()
            .map(|(time, value)| (time - mean_x) * (value.max(1e-12).ln() - mean_y))
            .sum::<f64>();
        let variance = points
            .iter()
            .map(|(time, _)| (time - mean_x).powi(2))
            .sum::<f64>();
        -variance / covariance
    }

    #[test]
    fn a_linear_element_cannot_exceed_two_to_one() {
        for beta in [0.01_f64, 0.1, 1.0, 10.0, 100.0] {
            for x in [1e-3_f64, 1e-2, 0.1, 1.0, 10.0, 100.0] {
                let expected = ((1.0 + 4.0 * beta * x).sqrt() - 1.0) / (2.0 * beta);
                let y = steady_peak(x, 0.0, beta, 1, 0.0);
                assert!(
                    (y - expected).abs() < 1e-6 * expected.max(1.0),
                    "beta {beta}: steady peak {y}, law {expected}"
                );
                let y = loop_output(x, 0.0, 0.0, beta, 0.0, 1, 1.0);
                assert!(
                    (y - expected).abs() < 1e-6 * expected.max(1.0),
                    "beta {beta}: instantaneous solver {y}, law {expected}"
                );
            }
        }
        for beta in [0.01, 0.1, 1.0, 10.0, 100.0] {
            for b in [0.0, 0.1, 1.0, 10.0] {
                for x in [0.1, 1.0, 10.0, 100.0, 1_000.0] {
                    let ratio = local_ratio(x, b, beta, 1, 0.0);
                    assert!(ratio <= 2.0 + 1e-6, "beta {beta}, bias {b}: ratio {ratio}");
                }
            }
        }
    }

    #[test]
    fn the_superlinear_channel_raises_the_ratio_toward_its_exponent() {
        let b = MIN_BIAS;
        let beta = 3.0e4;
        let ratio = local_ratio(1.0, b, beta, ELEMENT_EXPONENT, INSTANT_TERM);
        assert!(
            ratio > 2.5 && ratio < ELEMENT_EXPONENT as f64 + 1.0,
            "ratio {ratio}"
        );
        let lower = local_ratio(1.0, 1.0, 0.5, ELEMENT_EXPONENT, INSTANT_TERM);
        assert!(ratio > lower, "{ratio} vs {lower}");
    }

    #[test]
    fn static_curve_matches_the_law_at_four_bias_positions() {
        for position in [0.25, 0.5, 0.75, 1.0] {
            let (b, beta) = bias_state(position);
            for level in [-30.0, -20.0, -10.0, 0.0] {
                let mut effect = Fet::new(RATE);
                effect.set_param("bias", position);
                let amplitude = 10f64.powf(level / 20.0);
                let measured = measured_gr(&mut effect, level);
                let predicted_peak =
                    steady_peak(amplitude, b, beta, ELEMENT_EXPONENT, INSTANT_TERM);
                let predicted = 20.0 * (amplitude / predicted_peak).log10();
                assert!(
                    (measured - predicted).abs() < 0.25,
                    "position {position}, {level} dBFS: measured {measured:.3}, law {predicted:.3}"
                );
            }
        }
    }

    #[test]
    fn the_ratio_position_raises_the_ratio_and_the_distortion() {
        let amplitude = 0.5;
        let mut ratios = Vec::new();
        let mut distortions = Vec::new();
        for position in [0.25, 0.5, 0.75, 1.0] {
            let (b, beta) = bias_state(position);
            ratios.push(local_ratio(
                amplitude,
                b,
                beta,
                ELEMENT_EXPONENT,
                INSTANT_TERM,
            ));
            let mut effect = Fet::new(RATE);
            effect.set_param("bias", position);
            distortions.push(settled_thd(&mut effect, amplitude));
        }
        for pair in ratios.windows(2) {
            assert!(pair[1] > pair[0], "ratios {ratios:?}");
        }
        for pair in distortions.windows(2) {
            assert!(pair[1] > pair[0], "distortions {distortions:?}");
        }
        assert!(
            ratios[0] > 1.5 && ratios[3] > 2.0,
            "ratio positions {ratios:?}"
        );
    }

    #[test]
    fn the_distortion_comes_from_the_instantaneous_channel_term() {
        let mut effect = Fet::new(RATE);
        effect.set_param("bias", 0.5);
        effect.set_element(ELEMENT_EXPONENT, 0.0);
        let without = settled_thd(&mut effect, 0.5);
        let mut effect = Fet::new(RATE);
        effect.set_param("bias", 0.5);
        let with = settled_thd(&mut effect, 0.5);
        assert!(without < 0.002, "envelope-only distortion {without}");
        assert!(
            with > 0.01 && with > without * 10.0,
            "instantaneous term: {with} vs {without}"
        );
    }

    #[test]
    fn attack_and_release_are_pinned_to_the_detector_time_constants() {
        for (attack_ms, release_ms) in [(0.2, 50.0), (2.0, 200.0), (10.0, 1_000.0)] {
            let mut effect = Fet::new(RATE);
            effect.set_param("attack", attack_ms);
            effect.set_param("release", release_ms);
            effect.set_param("bias", 0.0);
            let mut points = Vec::new();
            let mut silence_block = vec![0.0_f32; 4 * 2];
            effect.process(&mut silence_block, RATE);
            let burst = step(0.01, 0.3);
            let mut time = 0.0;
            for chunk in burst.chunks(4 * 2) {
                let mut block = chunk.to_vec();
                effect.process(&mut block, RATE);
                time += 4.0 / RATE as f64;
                points.push((time, effect.envelope()));
            }
            let final_envelope = effect.envelope();
            let start = points
                .iter()
                .find(|(_, value)| *value > 0.05 * final_envelope)
                .map(|(time, _)| *time)
                .unwrap_or(0.0);
            let end = points
                .iter()
                .find(|(_, value)| *value > 0.95 * final_envelope)
                .map(|(time, _)| *time)
                .unwrap_or(start);
            let rise = (end - start) / 2.944;
            assert!(
                (rise / (attack_ms * 1e-3) - 1.0).abs() < 0.5,
                "attack {attack_ms} ms measured {:.3} ms",
                rise * 1e3
            );

            let quiet = silence(1.5);
            let mut points = Vec::new();
            let mut time = 0.0;
            let mut block = vec![0.0_f32; 4 * 2];
            for chunk in quiet.chunks(4 * 2) {
                block.copy_from_slice(chunk);
                effect.process(&mut block, RATE);
                time += 4.0 / RATE as f64;
                points.push((time, effect.envelope()));
            }
            let tau = fit_tau(&points, 0.02, 0.4);
            assert!(
                (tau / (release_ms * 1e-3) - 1.0).abs() < 0.35,
                "release {release_ms} ms measured {:.1} ms",
                tau * 1e3
            );
        }
    }

    #[test]
    fn zero_drive_is_a_silent_null() {
        for position in [0.0, 0.5, 1.0] {
            for attack in [MIN_ATTACK_MS, MAX_ATTACK_MS] {
                for release in [MIN_RELEASE_MS, MAX_RELEASE_MS] {
                    let mut effect = Fet::new(RATE);
                    effect.set_param("bias", position);
                    effect.set_param("attack", attack);
                    effect.set_param("release", release);
                    let output = run(&mut effect, &silence(0.5));
                    assert!(
                        peak(&output) < 1e-9,
                        "position {position}: {}",
                        peak(&output)
                    );
                }
            }
        }
    }

    #[test]
    fn no_self_oscillation_across_the_control_space() {
        for position in [0.0, 0.5, 1.0] {
            for attack in [MIN_ATTACK_MS, MAX_ATTACK_MS] {
                for release in [MIN_RELEASE_MS, MAX_RELEASE_MS] {
                    for drive in [MIN_DRIVE_DB, MAX_DRIVE_DB] {
                        let mut effect = Fet::new(RATE);
                        effect.set_param("bias", position);
                        effect.set_param("attack", attack);
                        effect.set_param("release", release);
                        effect.set_param("drive", drive);
                        let mut input = tone(0.8, 0.2);
                        input.extend(silence(1.5));
                        let output = run(&mut effect, &input);
                        let tail = &output[(RATE as f64 * 1.2) as usize * 2..];
                        assert!(
                            peak(tail) < 1e-4,
                            "position {position}, attack {attack}, release {release}, drive {drive}: tail {}",
                            peak(tail)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let input = tone(0.2, 3.0);
        let mut small = Fet::new(RATE);
        let mut large = Fet::new(RATE);
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
    fn output_is_an_exact_gain() {
        let input = tone(0.1, 2.0);
        let mut flat = Fet::new(RATE);
        let mut boosted = Fet::new(RATE);
        boosted.set_param("output", 6.0);
        let a = run(&mut flat, &input);
        let b = run(&mut boosted, &input);
        let start = RATE as usize * 2;
        let delta = rms_db(&b[start..]) - rms_db(&a[start..]);
        assert!((delta - 6.0).abs() < 1e-5, "output moved {delta} dB");
    }

    #[test]
    fn latency_is_the_shared_oversampler() {
        let mut effect = Fet::new(RATE);
        effect.set_element(1, 0.0);
        effect.set_param("bias", 0.0);
        let mut impulse = vec![0.0_f32; 128];
        impulse[0] = 0.01;
        effect.process(&mut impulse, RATE);
        let index = impulse
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().partial_cmp(&b.abs()).unwrap())
            .map(|(index, _)| index / 2)
            .unwrap();
        assert_eq!(index, LATENCY);
        assert_eq!(effect.latency_samples(), LATENCY as u32);
    }

    #[test]
    fn reset_clears_the_envelope_and_the_loop() {
        let mut effect = Fet::new(RATE);
        run(&mut effect, &tone(0.5, 1.0));
        assert!(effect.envelope() > 0.0);
        effect.reset();
        assert_eq!(effect.envelope(), 0.0);
        assert_eq!(effect.gain_reduction_db(), 0.0);
        let mut impulse = vec![0.0_f32; 64];
        impulse[0] = 0.5;
        effect.process(&mut impulse, RATE);
        assert!(impulse.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        println!("static curve, gain reduction dB at drive 0");
        for position in [0.25, 0.5, 0.75, 1.0] {
            let (b, beta) = bias_state(position);
            let mut row = Vec::new();
            for level in [-30.0, -24.0, -18.0, -12.0, -6.0, 0.0] {
                let mut effect = Fet::new(RATE);
                effect.set_param("bias", position);
                let measured = measured_gr(&mut effect, level);
                let amplitude = 10f64.powf(level / 20.0);
                let predicted_peak =
                    steady_peak(amplitude, b, beta, ELEMENT_EXPONENT, INSTANT_TERM);
                let predicted = 20.0 * (amplitude / predicted_peak).log10();
                row.push((level, measured, predicted));
            }
            println!("position {position}: {row:?}");
        }
        println!("local ratio at -6 dBFS and its law prediction");
        for position in [0.25, 0.5, 0.75, 1.0] {
            let (b, beta) = bias_state(position);
            let ratio = local_ratio(0.5, b, beta, ELEMENT_EXPONENT, INSTANT_TERM);
            let mut effect = Fet::new(RATE);
            effect.set_param("bias", position);
            let gr_low = measured_gr(&mut effect, -6.5);
            let mut effect = Fet::new(RATE);
            effect.set_param("bias", position);
            let gr_high = measured_gr(&mut effect, -5.5);
            println!(
                "position {position}: law {ratio:.3}, measured {:.3}",
                1.0 / (1.0 - (gr_high - gr_low))
            );
        }
        println!("THD at -6 dBFS, 1 kHz");
        for position in [0.25, 0.5, 0.75, 1.0] {
            let mut effect = Fet::new(RATE);
            effect.set_param("bias", position);
            println!(
                "position {position}: {:.2} %",
                100.0 * settled_thd(&mut effect, 0.5)
            );
        }
        println!("THD with the instantaneous term disabled, -6 dBFS");
        for position in [0.25, 0.5, 0.75, 1.0] {
            let mut effect = Fet::new(RATE);
            effect.set_param("bias", position);
            effect.set_element(ELEMENT_EXPONENT, 0.0);
            println!(
                "position {position}: {:.3} %",
                100.0 * settled_thd(&mut effect, 0.5)
            );
        }
        println!("linear loop closed form vs solver");
        for beta in [0.01_f64, 0.1, 1.0, 10.0] {
            let x = 1.0;
            let law = ((1.0 + 4.0 * beta * x).sqrt() - 1.0) / (2.0 * beta);
            let solver = steady_peak(x, 0.0, beta, 1, 0.0);
            println!("beta {beta}: law {law:.9} solver {solver:.9}");
        }
        println!("ballistics, 63% detector times");
        for (attack_ms, release_ms) in [(0.2, 50.0), (2.0, 200.0), (10.0, 1_000.0)] {
            let mut effect = Fet::new(RATE);
            effect.set_param("attack", attack_ms);
            effect.set_param("release", release_ms);
            effect.set_param("bias", 0.0);
            let mut block = vec![0.0_f32; 4 * 2];
            effect.process(&mut block, RATE);
            let mut points = Vec::new();
            let mut time = 0.0;
            for chunk in step(0.01, 0.3).chunks(4 * 2) {
                let mut block = chunk.to_vec();
                effect.process(&mut block, RATE);
                time += 4.0 / RATE as f64;
                points.push((time, effect.envelope()));
            }
            let final_envelope = effect.envelope();
            let start = points
                .iter()
                .find(|(_, value)| *value > 0.05 * final_envelope)
                .map(|(time, _)| *time)
                .unwrap_or(0.0);
            let end = points
                .iter()
                .find(|(_, value)| *value > 0.95 * final_envelope)
                .map(|(time, _)| *time)
                .unwrap_or(start);
            let rise = (end - start) / 2.944;
            let quiet = silence(1.5);
            let mut points = Vec::new();
            let mut time = 0.0;
            let mut block = vec![0.0_f32; 4 * 2];
            for chunk in quiet.chunks(4 * 2) {
                block.copy_from_slice(chunk);
                effect.process(&mut block, RATE);
                time += 4.0 / RATE as f64;
                points.push((time, effect.envelope()));
            }
            let tau = fit_tau(&points, 0.02, 0.4);
            println!(
                "attack {attack_ms} ms -> {:.3} ms, release {release_ms} ms -> {:.1} ms",
                rise * 1e3,
                tau * 1e3
            );
        }
    }
}
