//! Allocation-free FFT execution using a precomputed radix-2 plan.

use std::f64::consts::TAU;

#[derive(Debug, Clone, Copy, Default)]
/// A complex sample used by the in-house FFT.
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    #[inline]
    /// Create a complex value.
    pub fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    #[inline]
    /// The complex conjugate.
    pub fn conjugate(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }

    #[inline]
    /// Component-wise addition.
    pub fn plus(self, other: Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            re: self.re - other.re,
            im: self.im - other.im,
        }
    }

    #[inline]
    /// Complex multiplication.
    pub fn times(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

/// Iterative radix-2 FFT with precomputed twiddles and bit-reversal indices.
pub struct Fft {
    size: usize,
    twiddles: Box<[Complex]>,
    reversed: Box<[u32]>,
}

impl Fft {
    #[inline]
    /// Precompute a radix-2 plan for a power-of-two size.
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two() && size <= u32::MAX as usize);
        let bits = size.trailing_zeros();
        let reversed = (0..size)
            .map(|index| {
                (index as u32)
                    .reverse_bits()
                    .checked_shr(32 - bits)
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let twiddles = (0..size / 2)
            .map(|index| {
                let angle = -TAU * index as f64 / size as f64;
                Complex::new(angle.cos(), angle.sin())
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            size,
            twiddles,
            reversed,
        }
    }

    /// Forward-transform a buffer whose length matches the plan.
    pub fn forward(&self, data: &mut [Complex]) {
        self.run(data, false);
    }

    /// Unscaled inverse transform of a matching buffer.
    pub fn inverse(&self, data: &mut [Complex]) {
        self.run(data, true);
    }

    fn run(&self, data: &mut [Complex], inverse: bool) {
        assert_eq!(data.len(), self.size);
        for index in 0..self.size {
            let target = self.reversed[index] as usize;
            if target > index {
                data.swap(index, target);
            }
        }
        let mut length = 2;
        while length <= self.size {
            let half = length / 2;
            let step = self.size / length;
            for start in (0..self.size).step_by(length) {
                for offset in 0..half {
                    let twiddle = self.twiddles[offset * step];
                    let twiddle = if inverse {
                        twiddle.conjugate()
                    } else {
                        twiddle
                    };
                    let even = data[start + offset];
                    let odd = data[start + offset + half].times(twiddle);
                    data[start + offset] = even.plus(odd);
                    data[start + offset + half] = even.sub(odd);
                }
            }
            length *= 2;
        }
        if inverse {
            let scale = 1.0 / self.size as f64;
            for value in data {
                value.re *= scale;
                value.im *= scale;
            }
        }
    }
}
