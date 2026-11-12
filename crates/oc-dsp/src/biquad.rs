//! Biquad filter — the building block for EQ, high/low pass, shelving, etc.
//!
//! Standard Direct Form II transposed implementation.
//! Coefficient calculation from Audio EQ Cookbook (Robert Bristow-Johnson).

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq)]
/// The biquad response shape.
pub enum FilterType {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    Peak,
    LowShelf,
    HighShelf,
    /// First-order tilt shelf with a 10^(-G/40) centring term.
    TiltShelf,
}

/// Normalized biquad coefficients (a0 = 1) designed with the RBJ cookbook.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BiquadCoefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadCoefficients {
    /// Design normalized coefficients at `sample_rate`; frequency clamps to
    /// 20 Hz..0.45·rate and Q to at least 0.1.
    pub fn design(
        filter_type: FilterType,
        freq: f64,
        q: f64,
        gain_db: f64,
        sample_rate: f64,
    ) -> Self {
        let freq = freq.clamp(20.0, sample_rate * 0.45);
        let q = q.max(0.1);
        let w0 = 2.0 * PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let a = 10.0_f64.powf(gain_db / 40.0); // amplitude for shelving/peak

        let (b0, b1, b2, a0, a1, a2) = match filter_type {
            FilterType::LowPass => (
                (1.0 - cos_w0) / 2.0,
                1.0 - cos_w0,
                (1.0 - cos_w0) / 2.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            FilterType::HighPass => (
                (1.0 + cos_w0) / 2.0,
                -(1.0 + cos_w0),
                (1.0 + cos_w0) / 2.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            FilterType::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha),
            FilterType::Notch => (
                1.0,
                -2.0 * cos_w0,
                1.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            FilterType::Peak => {
                let alpha_a = alpha * a;
                let alpha_div_a = alpha / a;
                (
                    1.0 + alpha_a,
                    -2.0 * cos_w0,
                    1.0 - alpha_a,
                    1.0 + alpha_div_a,
                    -2.0 * cos_w0,
                    1.0 - alpha_div_a,
                )
            }
            FilterType::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                    (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                    (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                )
            }
            FilterType::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                    (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                    (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                )
            }
            FilterType::TiltShelf => {
                let centring = 10.0_f64.powf(-gain_db / 40.0);
                let low = (w0 * centring * 0.5).tan();
                let high = (w0 / centring * 0.5).tan();
                let scale = centring * high / low;
                (
                    scale * (1.0 + low) / (1.0 + high),
                    scale * (low - 1.0) / (1.0 + high),
                    0.0,
                    1.0,
                    (high - 1.0) / (1.0 + high),
                    0.0,
                )
            }
        };

        Self::from_raw(b0, b1, b2, a0, a1, a2)
    }

    /// Normalize predesigned coefficients by a nonzero `a0`.
    #[inline]
    pub fn from_raw(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response at `freq_hz`, in dB.
    pub fn response_db(&self, freq_hz: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * freq_hz.clamp(0.0, sample_rate * 0.5) / sample_rate;
        let (sin, cos) = w.sin_cos();
        let (z1r, z1i) = (cos, -sin);
        let (z2r, z2i) = (cos * cos - sin * sin, -2.0 * sin * cos);
        let num_r = self.b0 + self.b1 * z1r + self.b2 * z2r;
        let num_i = self.b1 * z1i + self.b2 * z2i;
        let den_r = 1.0 + self.a1 * z1r + self.a2 * z2r;
        let den_i = self.a1 * z1i + self.a2 * z2i;
        let num = (num_r * num_r + num_i * num_i).sqrt();
        let den = (den_r * den_r + den_i * den_i).sqrt();
        if den <= f64::MIN_POSITIVE || !num.is_finite() {
            return 0.0;
        }
        20.0 * (num / den).log10()
    }

    /// Coefficient delta that reaches `target` after `samples` updates.
    pub fn step_toward(&self, target: &Self, samples: f64) -> Self {
        let inv = 1.0 / samples.max(1.0);
        Self {
            b0: (target.b0 - self.b0) * inv,
            b1: (target.b1 - self.b1) * inv,
            b2: (target.b2 - self.b2) * inv,
            a1: (target.a1 - self.a1) * inv,
            a2: (target.a2 - self.a2) * inv,
        }
    }

    /// Move the coefficients one smoothing step toward a target.
    pub fn advance(&mut self, step: &Self) {
        self.b0 += step.b0;
        self.b1 += step.b1;
        self.b2 += step.b2;
        self.a1 += step.a1;
        self.a2 += step.a2;
    }
}

/// Direct Form II transposed state, one instance per channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    #[inline]
    /// Filter one sample and update the state.
    pub fn process(&mut self, coefficients: &BiquadCoefficients, input: f64) -> f64 {
        let output = coefficients.b0 * input + self.z1;
        self.z1 = coefficients.b1 * input - coefficients.a1 * output + self.z2;
        self.z2 = coefficients.b2 * input - coefficients.a2 * output;
        output
    }

    /// Clear the state to silence.
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// A two-channel biquad section with its coefficients and per-channel state.
#[derive(Debug, Clone)]
pub struct BiquadFilter {
    coefficients: BiquadCoefficients,
    // State (per channel)
    z1: [f64; 2],
    z2: [f64; 2],
    // Parameters
    filter_type: FilterType,
    freq: f64,
    q: f64,
    gain_db: f64,
    sample_rate: f64,
}

impl BiquadFilter {
    /// Build a two-channel filter with clamped frequency and Q.
    pub fn new(filter_type: FilterType, freq: f64, q: f64, gain_db: f64, sample_rate: u32) -> Self {
        let mut f = Self {
            coefficients: BiquadCoefficients::default(),
            z1: [0.0; 2],
            z2: [0.0; 2],
            filter_type,
            freq: freq.clamp(20.0, sample_rate as f64 * 0.45),
            q: q.max(0.1),
            gain_db,
            sample_rate: sample_rate as f64,
        };
        f.calculate_coefficients();
        f
    }

    /// Set frequency, Q and shelf/peak gain, then redesign.
    pub fn set_params(&mut self, freq: f64, q: f64, gain_db: f64) {
        self.freq = freq.clamp(20.0, self.sample_rate * 0.45);
        self.q = q.max(0.1);
        self.gain_db = gain_db;
        self.calculate_coefficients();
    }

    /// Change the response shape and redesign from the current parameters.
    pub fn set_type(&mut self, filter_type: FilterType) {
        self.filter_type = filter_type;
        self.calculate_coefficients();
    }

    /// Effective coefficients' parameter state, including any safety bounds.
    pub fn params(&self) -> (f64, f64, f64) {
        (self.freq, self.q, self.gain_db)
    }

    /// Process stereo interleaved buffer in-place.
    pub fn process_stereo(&mut self, buffer: &mut [f32]) {
        let len = buffer.len();
        let mut i = 0;
        while i + 1 < len {
            for ch in 0..2 {
                let x = buffer[i + ch] as f64;
                let y = self.coefficients.b0 * x + self.z1[ch];
                self.z1[ch] = self.coefficients.b1 * x - self.coefficients.a1 * y + self.z2[ch];
                self.z2[ch] = self.coefficients.b2 * x - self.coefficients.a2 * y;
                buffer[i + ch] = y as f32;
            }
            i += 2;
        }
    }

    /// Process one sample of one channel (0 or 1).
    /// Filter one sample on `channel` and update its state.
    pub fn process_sample(&mut self, channel: usize, input: f64) -> f64 {
        let coefficients = &self.coefficients;
        let output = coefficients.b0 * input + self.z1[channel];
        self.z1[channel] = coefficients.b1 * input - coefficients.a1 * output + self.z2[channel];
        self.z2[channel] = coefficients.b2 * input - coefficients.a2 * output;
        output
    }

    /// Clear both channels to silence without changing coefficients.
    pub fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }

    fn calculate_coefficients(&mut self) {
        self.coefficients = BiquadCoefficients::design(
            self.filter_type,
            self.freq,
            self.q,
            self.gain_db,
            self.sample_rate,
        );
    }
}
