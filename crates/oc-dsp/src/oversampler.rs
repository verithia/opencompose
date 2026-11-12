//! Shared 4x FIR oversampler for the nonlinear colour devices.

/// Taps in the base-rate decimator.
const TAPS: usize = 65;
/// Base-rate samples kept; the longest interpolation branch walks back this far.
const HISTORY: usize = 17;
/// Input taps of the longest polyphase branch.
const PHASE_TAPS: usize = TAPS.div_ceil(4);
/// Taps every interpolation branch carries; only phase 0 carries `PHASE_TAPS`.
const SHARED_TAPS: usize = PHASE_TAPS - 1;
/// Partial sums per channel in the decimator dot product.
const DOWN_CHAINS: usize = 8;
pub(crate) const LATENCY: usize = 16; // two 32-high-rate-sample FIR group delays / 4

pub(crate) struct Oversampler4x {
    coefficients: [f64; TAPS],
    /// Phase-major upsample branches, `4 * coefficients[phase + 4 * k]` so the
    /// interpolation gain folds into the tap. Exact: 4 is a power of two.
    up: [[f64; PHASE_TAPS]; 4],
    /// History kept doubled as a mirror, so a tap walks back from the write head
    /// without a modulo and without a bounds check.
    input: [[f64; 2 * HISTORY]; 2],
    shaped: [[f64; 2 * TAPS]; 2],
    input_pos: usize,
    shaped_pos: usize,
}

impl Oversampler4x {
    pub(crate) fn new() -> Self {
        let mut coefficients = std::array::from_fn(|i| {
            let x = i as f64 - 32.0;
            let cutoff = 0.1125; // 90% of Nyquist at 4x
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let phase = 2.0 * std::f64::consts::PI * i as f64 / 64.0;
            sinc * (0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos())
        });
        let sum: f64 = coefficients.iter().sum();
        for value in &mut coefficients {
            *value /= sum;
        }
        let mut up = [[0.0; PHASE_TAPS]; 4];
        for (phase, branch) in up.iter_mut().enumerate() {
            let taps = (TAPS - phase).div_ceil(4);
            for (k, coefficient) in branch.iter_mut().take(taps).enumerate() {
                *coefficient = coefficients[phase + 4 * k] * 4.0;
            }
        }
        Self {
            coefficients,
            up,
            input: [[0.0; 2 * HISTORY]; 2],
            shaped: [[0.0; 2 * TAPS]; 2],
            input_pos: 0,
            shaped_pos: 0,
        }
    }

    /// Push one base-rate frame and shape each of its four high-rate samples.
    /// Returns the base-rate wet frame, delayed by `LATENCY` base samples.
    pub(crate) fn process<F>(&mut self, frame: [f64; 2], shape: F) -> [f64; 2]
    where
        F: FnMut([f64; 2]) -> [f64; 2],
    {
        self.process_with::<DOWN_CHAINS, F>(frame, shape)
    }

    /// The four interpolation branches read the same input window, so one pass
    /// fills them all and their eight partial sums (two channels each)
    /// interleave. `CHAINS` sets how many partial sums the decimator keeps.
    fn process_with<const CHAINS: usize, F>(&mut self, frame: [f64; 2], mut shape: F) -> [f64; 2]
    where
        F: FnMut([f64; 2]) -> [f64; 2],
    {
        for (channel, &value) in frame.iter().enumerate() {
            self.input[channel][self.input_pos] = value;
            self.input[channel][self.input_pos + HISTORY] = value;
        }
        let head = self.input_pos + HISTORY;
        // Each branch adds its taps in the order the four separate loops did, so
        // this pass changes no sum.
        let mut branches = [[0.0; 2]; 4];
        for tap in 0..SHARED_TAPS {
            let left = self.input[0][head - tap];
            let right = self.input[1][head - tap];
            for (branch, sum) in branches.iter_mut().enumerate() {
                let coefficient = self.up[branch][tap];
                sum[0] += coefficient * left;
                sum[1] += coefficient * right;
            }
        }
        let left = self.input[0][head - SHARED_TAPS];
        let right = self.input[1][head - SHARED_TAPS];
        branches[0][0] += self.up[0][SHARED_TAPS] * left;
        branches[0][1] += self.up[0][SHARED_TAPS] * right;

        let mut wet = [0.0; 2];
        for (phase, branch) in branches.into_iter().enumerate() {
            let shaped = shape(branch);
            for (channel, &value) in shaped.iter().enumerate() {
                self.shaped[channel][self.shaped_pos] = value;
                self.shaped[channel][self.shaped_pos + TAPS] = value;
            }
            if phase == 0 {
                wet = dot2::<CHAINS>(
                    &self.coefficients,
                    &self.shaped[0],
                    &self.shaped[1],
                    self.shaped_pos + TAPS,
                );
            }
            self.shaped_pos += 1;
            if self.shaped_pos == TAPS {
                self.shaped_pos = 0;
            }
        }
        self.input_pos += 1;
        if self.input_pos == HISTORY {
            self.input_pos = 0;
        }
        wet
    }

    pub(crate) fn reset(&mut self) {
        self.input = [[0.0; 2 * HISTORY]; 2];
        self.shaped = [[0.0; 2 * TAPS]; 2];
        self.input_pos = 0;
        self.shaped_pos = 0;
    }
}

/// Two-channel FIR dot product over a mirrored window: tap `i` reads
/// `[start - i]`. `CHAINS` partial sums per channel break the dependent add
/// chain; the reduction is a balanced tree, so `CHAINS` is a power of two.
#[inline(always)]
fn dot2<const CHAINS: usize>(
    coefficients: &[f64],
    left: &[f64],
    right: &[f64],
    start: usize,
) -> [f64; 2] {
    debug_assert!(CHAINS.is_power_of_two());
    let taps = coefficients.len();
    let mut sums = [[0.0; 2]; CHAINS];
    let mut tap = 0;
    while tap + CHAINS <= taps {
        for (chain, sum) in sums.iter_mut().enumerate() {
            let coefficient = coefficients[tap + chain];
            let index = start - tap - chain;
            sum[0] += coefficient * left[index];
            sum[1] += coefficient * right[index];
        }
        tap += CHAINS;
    }
    while tap < taps {
        sums[0][0] += coefficients[tap] * left[start - tap];
        sums[0][1] += coefficients[tap] * right[start - tap];
        tap += 1;
    }
    let mut width = CHAINS;
    while width > 1 {
        let half = width / 2;
        for chain in 0..half {
            sums[chain][0] += sums[chain + half][0];
            sums[chain][1] += sums[chain + half][1];
        }
        width = half;
    }
    sums[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-mirror form, kept as the reference the rings have to reproduce.
    struct ModuloForm {
        coefficients: [f64; TAPS],
        input: [[f64; HISTORY]; 2],
        shaped: [[f64; TAPS]; 2],
        input_pos: usize,
        shaped_pos: usize,
    }

    impl ModuloForm {
        fn new(coefficients: [f64; TAPS]) -> Self {
            Self {
                coefficients,
                input: [[0.0; HISTORY]; 2],
                shaped: [[0.0; TAPS]; 2],
                input_pos: 0,
                shaped_pos: 0,
            }
        }

        fn process<F>(&mut self, frame: [f64; 2], mut shape: F) -> [f64; 2]
        where
            F: FnMut([f64; 2]) -> [f64; 2],
        {
            for (channel, &value) in frame.iter().enumerate() {
                self.input[channel][self.input_pos] = value;
            }
            let mut wet = [0.0; 2];
            for phase in 0..4 {
                let mut up = [0.0; 2];
                for (channel, up_channel) in up.iter_mut().enumerate() {
                    for tap in (phase..TAPS).step_by(4) {
                        let index = (self.input_pos + HISTORY - tap / 4) % HISTORY;
                        *up_channel += self.coefficients[tap] * self.input[channel][index] * 4.0;
                    }
                }
                let shaped = shape(up);
                for (channel, &value) in shaped.iter().enumerate() {
                    self.shaped[channel][self.shaped_pos] = value;
                }
                if phase == 0 {
                    for (channel, wet_channel) in wet.iter_mut().enumerate() {
                        for tap in 0..TAPS {
                            *wet_channel += self.coefficients[tap]
                                * self.shaped[channel][(self.shaped_pos + TAPS - tap) % TAPS];
                        }
                    }
                }
                self.shaped_pos = (self.shaped_pos + 1) % TAPS;
            }
            self.input_pos = (self.input_pos + 1) % HISTORY;
            wet
        }
    }

    fn frame_for(index: usize) -> [f64; 2] {
        let left = ((index as f64) * 0.7311).sin() * 0.7;
        let right = ((index as f64) * 0.4127 + 1.3).cos() * 0.5;
        [left, right]
    }

    fn shape(frame: [f64; 2]) -> [f64; 2] {
        // A memoryless nonlinearity of the kind the colour devices use, so an
        // index error anywhere in the history cannot cancel out.
        [
            (frame[0] * 1.7).tanh() + 0.05 * frame[0] * frame[1],
            (frame[1] * 2.3).tanh() - 0.02 * frame[0],
        ]
    }

    #[test]
    fn mirrored_rings_render_the_modulo_form_bit_for_bit() {
        // One partial sum per channel keeps the sequential order the modulo form
        // uses, so the mirrored rings and the single-pass interpolation must
        // match it exactly.
        let mut actual = Oversampler4x::new();
        let mut reference = ModuloForm::new(actual.coefficients);
        for index in 0..4096 {
            let frame = frame_for(index);
            let expected = reference.process(frame, shape);
            let rendered = actual.process_with::<1, _>(frame, shape);
            assert_eq!(expected, rendered, "frame {index} differs");
            assert_eq!(reference.input_pos, actual.input_pos);
            assert_eq!(reference.shaped_pos, actual.shaped_pos);
        }
    }

    #[test]
    fn split_decimator_moves_no_output_sample() {
        // The production split re-associates the 65-tap sum. Its error is far
        // below one 16-bit step, so every sample must quantise to the same word.
        let mut split = Oversampler4x::new();
        let mut sequential = Oversampler4x::new();
        let mut worst: f64 = 0.0;
        for index in 0..8192 {
            let frame = frame_for(index);
            let a = split.process(frame, shape);
            let b = sequential.process_with::<1, _>(frame, shape);
            for channel in 0..2 {
                worst = worst.max((a[channel] - b[channel]).abs());
                let wa = (a[channel] * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                let wb = (b[channel] * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                assert_eq!(
                    wa, wb,
                    "16-bit word differs at frame {index} channel {channel}"
                );
            }
        }
        assert!(
            worst < 1e-14,
            "float drift {worst:e} is too large to be rounding"
        );
    }

    #[test]
    fn tap_rings_are_the_modulo_indices() {
        for position in 0..HISTORY {
            let head = position + HISTORY;
            for phase in 0..4 {
                let taps = (TAPS - phase).div_ceil(4);
                for k in 0..taps {
                    let tap = phase + 4 * k;
                    // The mirrored read sits `HISTORY` later than the ring read
                    // and carries the same sample.
                    assert!(head - k < 2 * HISTORY);
                    assert_eq!(
                        (head - k) % HISTORY,
                        (position + HISTORY - tap / 4) % HISTORY
                    );
                }
            }
        }
        // The downsample window is [position + 1, position + TAPS] in the
        // mirrored buffer; each read is the ring index shifted by one block, and
        // the write path keeps both copies equal.
        for position in 0..TAPS {
            for tap in 0..TAPS {
                let modulo = (position + TAPS - tap) % TAPS;
                let mirrored = position + TAPS - tap;
                assert!(mirrored < 2 * TAPS);
                assert!(mirrored == modulo || mirrored - TAPS == modulo);
            }
        }
    }

    /// Per-frame cost of the upsample/downsample pair, for the profiles that
    /// quote the oversampler's share of a synth or bus track. `KIND` picks the
    /// shape: 0 linear, 1 tanh, 2 the saturator's gain-tanh-normalise law.
    #[test]
    #[ignore = "performance probe; run with --ignored --nocapture"]
    fn oversampler_cost_probe() {
        const FRAMES: u64 = 1_000_000;
        const ROUNDS: usize = 5;
        for _ in 0..2 {
            probe::<1, 2>(FRAMES, ROUNDS);
            probe::<2, 2>(FRAMES, ROUNDS);
            probe::<4, 2>(FRAMES, ROUNDS);
            probe::<8, 2>(FRAMES, ROUNDS);
            probe::<DOWN_CHAINS, 0>(FRAMES, ROUNDS);
            probe::<DOWN_CHAINS, 1>(FRAMES, ROUNDS);
        }
    }

    fn probe<const CHAINS: usize, const KIND: u8>(frames: u64, rounds: usize) {
        let gain = 10.0_f64.powf(0.16 * 24.0 / 20.0);
        let norm = gain.tanh();
        let shape = |frame: [f64; 2]| match KIND {
            0 => frame,
            1 => [frame[0].tanh(), frame[1].tanh()],
            _ => [
                (gain * frame[0]).tanh() / norm,
                (gain * frame[1]).tanh() / norm,
            ],
        };
        let mut oversampler = Oversampler4x::new();
        let mut sink = 0.0;
        for index in 0..4096 {
            let value = ((index % 4800) as f64 / 4800.0).sin() * 0.6;
            sink += oversampler.process_with::<CHAINS, _>([value, -value], shape)[0];
        }
        let mut samples = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let start = std::time::Instant::now();
            for index in 0..frames {
                let value = ((index % 4800) as f64 / 4800.0).sin() * 0.6;
                sink += oversampler.process_with::<CHAINS, _>([value, -value], shape)[0];
            }
            samples.push(start.elapsed().as_secs_f64() * 1e9 / frames as f64);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "chains {CHAINS} {}: median {:.2} ns/frame  min {:.2}  max {:.2}  (sink {sink:e})",
            ["linear   ", "tanh     ", "saturator"][KIND as usize],
            samples[rounds / 2],
            samples[0],
            samples[rounds - 1]
        );
    }
}
