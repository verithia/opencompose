//! Control-rate vocal leveler: slow K-weighted loudness riding with a silence gate.

use super::AudioEffect;
use super::kweight::KWeightChannel;

const CONTROL_PERIOD_S: f64 = 0.010;
const GATE_HOLD_S: f64 = 0.300;
const MIN_LEVEL: f64 = 1e-30;
const MIN_SPEED_DB_S: f64 = 2.0;
const MAX_SPEED_DB_S: f64 = 6.0;
const MIN_WINDOW_MS: f64 = 500.0;
const MAX_WINDOW_MS: f64 = 3_000.0;
const MIN_RANGE_DB: f64 = 12.0;
const MAX_RANGE_DB: f64 = 24.0;
const MIN_TARGET_DB: f64 = -40.0;
const MAX_TARGET_DB: f64 = 0.0;
const MIN_GATE_DB: f64 = -70.0;
const MAX_GATE_DB: f64 = -20.0;
const MIN_GATE_RELEASE_MS: f64 = 1_000.0;
const MAX_GATE_RELEASE_MS: f64 = 2_000.0;

/// A control-rate vocal leveler with a silence gate.
pub struct Leveler {
    target_db: f64,
    speed_db_s: f64,
    range_db: f64,
    window_ms: f64,
    gate_db: f64,
    gate_release_ms: f64,
    sample_rate: u32,
    control_frames: usize,
    frames_left: usize,
    window_coeff: f64,
    gate_coeff: f64,
    filters: [KWeightChannel; 2],
    energy: f64,
    power: f64,
    gain_db: f64,
    gain: f64,
    ramp_target: f64,
    gain_step: f64,
    gate_time_s: f64,
    max_reduction_db: f64,
}

impl Leveler {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let control_frames = ((sample_rate as f64 * CONTROL_PERIOD_S).round() as usize).max(1);
        let mut leveler = Self {
            target_db: -20.0,
            speed_db_s: 3.0,
            range_db: 18.0,
            window_ms: 1_000.0,
            gate_db: -45.0,
            gate_release_ms: 1_500.0,
            sample_rate,
            control_frames,
            frames_left: control_frames,
            window_coeff: 0.0,
            gate_coeff: 0.0,
            filters: [
                KWeightChannel::new(sample_rate as f64),
                KWeightChannel::new(sample_rate as f64),
            ],
            energy: 0.0,
            power: 0.0,
            gain_db: 0.0,
            gain: 1.0,
            ramp_target: 1.0,
            gain_step: 0.0,
            gate_time_s: 0.0,
            max_reduction_db: 0.0,
        };
        leveler.update_coefficients();
        leveler
    }

    /// Frames between control decisions; an offline bake steps the law here.
    pub fn control_frames(&self) -> usize {
        self.control_frames
    }

    /// Gain decided at the last control tick, in dB; positive values boost.
    pub fn current_gain_db(&self) -> f64 {
        self.gain_db
    }

    fn update_coefficients(&mut self) {
        let period_s = self.control_frames as f64 / self.sample_rate as f64;
        let window_s = (self.window_ms * 0.001).max(period_s);
        let gate_release_s = (self.gate_release_ms * 0.001).max(period_s);
        self.window_coeff = 1.0 - (-period_s / window_s).exp();
        self.gate_coeff = 1.0 - (-period_s / gate_release_s).exp();
    }

    fn control_tick(&mut self) {
        self.gain = self.ramp_target;
        let period_s = self.control_frames as f64 / self.sample_rate as f64;
        let mean = self.energy / self.control_frames as f64;
        self.energy = 0.0;
        self.power += (mean - self.power) * self.window_coeff;
        let level_db = 10.0 * self.power.max(MIN_LEVEL).log10();
        let block_db = 10.0 * mean.max(MIN_LEVEL).log10();

        let mut next_db = self.gain_db;
        if block_db < self.gate_db {
            self.gate_time_s += period_s;
            if self.gate_time_s > GATE_HOLD_S {
                next_db += (0.0 - next_db) * self.gate_coeff;
            }
        } else {
            self.gate_time_s = 0.0;
            let desired = self.target_db - level_db;
            let step = self.speed_db_s * period_s;
            next_db += (desired - next_db).clamp(-step, step);
        }
        next_db = next_db.clamp(-self.range_db, self.range_db);

        self.gain_db = next_db;
        self.ramp_target = 10f64.powf(next_db / 20.0);
        self.gain_step = (self.ramp_target - self.gain) / self.control_frames as f64;
        self.max_reduction_db = self.max_reduction_db.max(-next_db);
        self.frames_left = self.control_frames;
    }
}

impl AudioEffect for Leveler {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        for frame in buffer.as_chunks_mut::<2>().0 {
            let left = self.filters[0].process(frame[0] as f64);
            let right = self.filters[1].process(frame[1] as f64);
            self.energy += 0.5 * (left * left + right * right);

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
            "target" => self.target_db = value.clamp(MIN_TARGET_DB, MAX_TARGET_DB),
            "speed" => self.speed_db_s = value.clamp(MIN_SPEED_DB_S, MAX_SPEED_DB_S),
            "range" => self.range_db = value.clamp(MIN_RANGE_DB, MAX_RANGE_DB),
            "window" => {
                self.window_ms = value.clamp(MIN_WINDOW_MS, MAX_WINDOW_MS);
                self.update_coefficients();
            }
            "gate" => self.gate_db = value.clamp(MIN_GATE_DB, MAX_GATE_DB),
            "gate_release" => {
                self.gate_release_ms = value.clamp(MIN_GATE_RELEASE_MS, MAX_GATE_RELEASE_MS);
                self.update_coefficients();
            }
            _ => {}
        }
        self.gain_db = self.gain_db.clamp(-self.range_db, self.range_db);
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "target" => Some(self.target_db),
            "speed" => Some(self.speed_db_s),
            "range" => Some(self.range_db),
            "window" => Some(self.window_ms),
            "gate" => Some(self.gate_db),
            "gate_release" => Some(self.gate_release_ms),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.filters[0].reset();
        self.filters[1].reset();
        self.energy = 0.0;
        self.power = 0.0;
        self.gain_db = 0.0;
        self.gain = 1.0;
        self.ramp_target = 1.0;
        self.gain_step = 0.0;
        self.gate_time_s = 0.0;
        self.frames_left = self.control_frames;
        self.max_reduction_db = 0.0;
    }

    fn gain_reduction_db(&self) -> f64 {
        (-self.gain_db).max(0.0)
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.max_reduction_db
    }

    fn name(&self) -> &str {
        "leveler"
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

    fn run(effect: &mut Leveler, input: &[f32], frames: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(frames * 2) {
            effect.process(chunk, RATE);
        }
        output
    }

    fn run_capturing_gain(effect: &mut Leveler, input: &[f32]) -> Vec<f64> {
        let mut gains = Vec::new();
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(480 * 2) {
            effect.process(chunk, RATE);
            gains.push(effect.gain_db);
        }
        gains
    }

    fn rms_db(samples: &[f32]) -> f64 {
        let power = samples
            .iter()
            .map(|sample| (*sample as f64) * (*sample as f64))
            .sum::<f64>()
            / samples.len() as f64;
        10.0 * power.max(1e-30).log10()
    }

    fn peak(samples: &[f32]) -> f64 {
        samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max((*sample as f64).abs()))
    }

    fn crest_db(samples: &[f32]) -> f64 {
        20.0 * peak(samples).max(1e-30).log10() - rms_db(samples)
    }

    #[test]
    fn rate_limit_moves_the_gain_at_most_speed_db_per_second() {
        for speed in [2.0, 4.0, 6.0] {
            let mut effect = Leveler::new(RATE);
            effect.set_param("target", -20.0);
            effect.set_param("window", 500.0);
            effect.set_param("range", 24.0);
            effect.set_param("gate", -60.0);
            effect.set_param("speed", speed);
            let mut input = tone(0.5, 3.0);
            input.extend(tone(0.05, 2.0));
            let gains = run_capturing_gain(&mut effect, &input);
            let start = 350;
            let moved = (gains[start] - gains[start + 100]).abs();
            assert!(
                moved > speed * 0.7,
                "{speed} dB/s moved only {moved} dB in 1 s"
            );
            for pair in gains.windows(2) {
                let step = (pair[1] - pair[0]).abs();
                assert!(
                    step <= speed * 0.010 + 1e-6,
                    "{speed} dB/s stepped {step} dB in 10 ms"
                );
            }
        }
    }

    #[test]
    fn gate_holds_for_300ms_then_returns_to_unity() {
        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 500.0);
        effect.set_param("speed", 6.0);
        effect.set_param("range", 18.0);
        effect.set_param("gate", -45.0);
        effect.set_param("gate_release", 1_000.0);
        let mut input = tone(0.05, 5.0);
        input.extend(silence(5.0));
        let gains = run_capturing_gain(&mut effect, &input);
        let loud = gains[490];
        assert!(loud > 5.0, "the rider should have boosted, got {loud}");
        let at_200ms = gains[520];
        let at_700ms = gains[570];
        let at_3s = gains[800];
        assert!(
            (at_200ms - loud).abs() < 0.2,
            "gain did not hold: {loud} -> {at_200ms}"
        );
        assert!(
            at_700ms < loud - 1.5,
            "gain did not drift after the hold: {loud} -> {at_700ms}"
        );
        assert!(at_3s.abs() < 1.0, "gain did not return to unity: {at_3s}");
    }

    #[test]
    fn a_steady_tone_moves_less_than_a_tenth_of_a_db() {
        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 3.0);
        effect.set_param("gate", -45.0);
        let output = run(&mut effect, &tone(0.05, 12.0), 480);
        let settled = &output[10 * RATE as usize * 2..];
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for window in settled.chunks(200 * RATE as usize / 1000 * 2) {
            let level = rms_db(window);
            minimum = minimum.min(level);
            maximum = maximum.max(level);
        }
        assert!(
            maximum - minimum < 0.1,
            "steady tone moved {:.3} dB",
            maximum - minimum
        );
    }

    #[test]
    fn crest_factor_is_preserved() {
        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -22.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 2.0);
        effect.set_param("gate", -60.0);
        let mut input = Vec::new();
        let frames = RATE as usize * 2;
        for index in 0..frames {
            let tone = 0.1 * (std::f64::consts::TAU * 1_000.0 * index as f64 / RATE as f64).sin();
            let spike = if index % 4_800 < 10 { 0.7 } else { 0.0 };
            let sample = (tone + spike) as f32;
            input.push(sample);
            input.push(sample);
        }
        let output = run(&mut effect, &input, 480);
        let mut worst = 0.0_f64;
        for frame in (RATE as usize / 2..RATE as usize * 3 / 2).step_by(4_800) {
            let dry = &input[frame * 2..(frame + 4_800) * 2];
            let wet = &output[frame * 2..(frame + 4_800) * 2];
            worst = worst.max((crest_db(dry) - crest_db(wet)).abs());
        }
        assert!(
            worst < 0.5,
            "per-syllable crest factor moved by {worst:.2} dB"
        );
        assert!(
            crest_db(&output[RATE as usize * 2..]) >= crest_db(&input[RATE as usize * 2..]) - 1.0,
            "the rider reduced the crest factor"
        );
    }

    #[test]
    fn a_20db_program_lands_within_two_db() {
        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 6.0);
        effect.set_param("range", 18.0);
        effect.set_param("gate", -45.0);
        let mut input = tone(0.02, 30.0);
        input.extend(tone(0.2, 30.0));
        let output = run(&mut effect, &input, 480);
        let verse = rms_db(&output[25 * RATE as usize * 2..30 * RATE as usize * 2]);
        let chorus = rms_db(&output[55 * RATE as usize * 2..60 * RATE as usize * 2]);
        assert!(
            (verse - chorus).abs() < 2.0,
            "verse {verse:.2} dB and chorus {chorus:.2} dB drifted apart"
        );
        assert!(
            (verse - -20.0).abs() < 2.5 && (chorus - -20.0).abs() < 2.5,
            "sections did not land on target: {verse:.2} / {chorus:.2} dB"
        );
    }

    #[test]
    fn breaths_are_not_lifted_after_the_hold() {
        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 6.0);
        effect.set_param("gate", -45.0);
        effect.set_param("gate_release", 1_000.0);
        let mut input = tone(0.05, 5.0);
        input.extend(silence(4.0));
        let breath = tone(0.003, 0.2);
        let breath_start = input.len();
        input.extend(breath.iter().copied());
        let output = run(&mut effect, &input, 480);
        let input_breath = rms_db(&breath);
        let output_breath = rms_db(&output[breath_start..]);
        assert!(
            (output_breath - input_breath).abs() < 0.5,
            "breath moved from {input_breath:.2} to {output_breath:.2} dB"
        );
    }

    #[test]
    fn zero_latency_impulse_passes_at_its_own_frame() {
        let mut effect = Leveler::new(RATE);
        let mut input = vec![0.0_f32; 256];
        input[0] = 0.5;
        input[1] = 0.5;
        let output = run(&mut effect, &input, 480);
        assert_eq!(output[0], 0.5);
        assert_eq!(output[1], 0.5);
        let tail_peak = output[2..]
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert_eq!(tail_peak, 0.0);
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let input = tone(0.05, 3.0);
        let mut small = Leveler::new(RATE);
        let mut large = Leveler::new(RATE);
        let a = run(&mut small, &input, 97);
        let b = run(&mut large, &input, 480);
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
    #[ignore]
    fn measurements_for_docs() {
        for speed in [2.0, 4.0, 6.0] {
            let mut effect = Leveler::new(RATE);
            effect.set_param("target", -20.0);
            effect.set_param("window", 500.0);
            effect.set_param("range", 24.0);
            effect.set_param("gate", -60.0);
            effect.set_param("speed", speed);
            let mut input = tone(0.5, 3.0);
            input.extend(tone(0.05, 3.0));
            let gains = run_capturing_gain(&mut effect, &input);
            let moved = gains
                .windows(101)
                .map(|window| (window[0] - window[100]).abs())
                .fold(0.0_f64, f64::max);
            println!("rate {speed:.0} dB/s: {moved:.2} dB in 1 s");
        }

        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 500.0);
        effect.set_param("speed", 6.0);
        effect.set_param("gate", -45.0);
        effect.set_param("gate_release", 1_000.0);
        let mut input = tone(0.05, 5.0);
        input.extend(silence(5.0));
        let gains = run_capturing_gain(&mut effect, &input);
        let held = gains[500];
        let drift = gains
            .iter()
            .enumerate()
            .skip(501)
            .find(|(_, gain)| **gain < held - 1.0)
            .map(|(index, _)| index as f64 * 0.010 - 5.0);
        let unity = gains
            .iter()
            .enumerate()
            .skip(501)
            .find(|(_, gain)| **gain < 1.0)
            .map(|(index, _)| index as f64 * 0.010 - 5.0);
        println!(
            "gate: held {held:.2} dB, 1 dB down at {:?} s, unity at {:?} s, 4 s {:.2} dB",
            drift, unity, gains[900]
        );

        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 6.0);
        effect.set_param("gate", -45.0);
        let mut input = tone(0.02, 30.0);
        input.extend(tone(0.2, 30.0));
        let output = run(&mut effect, &input, 480);
        let verse = rms_db(&output[25 * RATE as usize * 2..30 * RATE as usize * 2]);
        let chorus = rms_db(&output[55 * RATE as usize * 2..60 * RATE as usize * 2]);
        println!("verse {verse:.2} dBFS, chorus {chorus:.2} dBFS at target -20");

        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -20.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 3.0);
        effect.set_param("gate", -45.0);
        let output = run(&mut effect, &tone(0.05, 12.0), 480);
        let settled = &output[10 * RATE as usize * 2..];
        let levels = settled
            .chunks(200 * RATE as usize / 1000 * 2)
            .map(rms_db)
            .collect::<Vec<_>>();
        let minimum = levels.iter().cloned().fold(f64::INFINITY, f64::min);
        let maximum = levels.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        println!("steady tone: {:.3} dB peak-to-peak", maximum - minimum);

        let mut effect = Leveler::new(RATE);
        effect.set_param("target", -22.0);
        effect.set_param("window", 1_000.0);
        effect.set_param("speed", 2.0);
        effect.set_param("gate", -60.0);
        let mut input = Vec::new();
        for index in 0..RATE as usize * 2 {
            let tone = 0.1 * (std::f64::consts::TAU * 1_000.0 * index as f64 / RATE as f64).sin();
            let spike = if index % 4_800 < 10 { 0.7 } else { 0.0 };
            input.push((tone + spike) as f32);
            input.push((tone + spike) as f32);
        }
        let output = run(&mut effect, &input, 480);
        let mut worst = 0.0_f64;
        for frame in (RATE as usize / 2..RATE as usize * 3 / 2).step_by(4_800) {
            let dry = crest_db(&input[frame * 2..(frame + 4_800) * 2]);
            let wet = crest_db(&output[frame * 2..(frame + 4_800) * 2]);
            worst = worst.max((dry - wet).abs());
        }
        let dry = crest_db(&input[RATE as usize * 2..]);
        let wet = crest_db(&output[RATE as usize * 2..]);
        println!("crest: per-syllable worst {worst:.3} dB, global {dry:.2} -> {wet:.2} dB");
    }
}
