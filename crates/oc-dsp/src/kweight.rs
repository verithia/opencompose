//! BS.1770 K-weighting: high shelf plus high pass, one pair per channel.

/// One biquad in transposed direct form II.
#[derive(Debug, Clone, Copy, Default)]
struct KWeightFilter {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl KWeightFilter {
    /// Stage 1 high shelf at 1682 Hz, +4 dB.
    fn high_shelf(sample_rate: f64) -> Self {
        let f0 = 1_681.974_450_955_533;
        let gain_db = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            ..Self::default()
        }
    }

    /// Stage 2 high pass at 38.1 Hz, Q 0.5003.
    fn high_pass(sample_rate: f64) -> Self {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 / a0,
            b2: 1.0 / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            ..Self::default()
        }
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

/// Per-channel K-weighting pair (shelf then high pass).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct KWeightChannel {
    shelf: KWeightFilter,
    high_pass: KWeightFilter,
}

impl KWeightChannel {
    pub(crate) fn new(sample_rate: f64) -> Self {
        Self {
            shelf: KWeightFilter::high_shelf(sample_rate),
            high_pass: KWeightFilter::high_pass(sample_rate),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.shelf.reset();
        self.high_pass.reset();
    }

    pub(crate) fn process(&mut self, input: f64) -> f64 {
        self.high_pass.process(self.shelf.process(input))
    }
}
