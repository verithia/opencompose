//! Offline preparation of phase-aligned, half-rate source levels.

use oc_dsp::kernel::{HALF_BAND_CENTER, HALF_BAND_TAPS, half_band_kernel};

#[derive(Default)]
pub(crate) struct DecimatedSource {
    levels: Vec<Vec<f32>>,
}

impl DecimatedSource {
    pub(crate) fn prepare(source: &[f32], channels: usize) -> Result<Self, String> {
        let mut result = Self::default();
        let mut input = source;
        let mut frame_count = input.len() / channels;
        let kernel = half_band_kernel();
        let gain: f64 = kernel.iter().sum();
        // The total number of additional frames is bounded by the source
        // length plus the rounding at each half-rate stage.
        result
            .levels
            .try_reserve_exact(
                (usize::BITS - frame_count.saturating_sub(1).leading_zeros()) as usize,
            )
            .map_err(|_| "could not allocate granular read levels")?;
        while frame_count > 1 {
            let level = decimate(input, frame_count, channels, kernel, gain)?;
            result.levels.push(level);
            input = result.levels.last().expect("newly prepared level");
            frame_count = input.len() / channels;
        }
        Ok(result)
    }

    #[inline]
    pub(crate) fn read(
        &self,
        position: f64,
        step: f64,
        source_frames: usize,
        channels: usize,
    ) -> Option<(f64, f64)> {
        let rate = step.abs();
        if self.levels.is_empty() || !rate.is_finite() || rate <= 1.0 {
            return None;
        }
        if !position.is_finite() || position >= source_frames as f64 {
            return Some((0.0, 0.0));
        }
        // Select the nearest level whose rate is at most one level frame per
        // output sample. Casting saturates for rates beyond the entire pyramid.
        let lower_power = (rate.ceil() as u64) - 1;
        let shift = (u64::BITS - lower_power.leading_zeros()) as usize;
        let level = shift.min(self.levels.len());
        let data = &self.levels[level - 1];
        let position = position.max(0.0) / (1usize << level) as f64;
        let last = data.len() / channels - 1;
        let first = (position as usize).min(last);
        let second = (first + 1).min(last);
        let fraction = position.fract();
        let channel = |offset| {
            let start = f64::from(data[first * channels + offset]);
            let end = f64::from(data[second * channels + offset]);
            start + (end - start) * fraction
        };
        Some((channel(0), channel(channels - 1)))
    }
}

fn decimate(
    source: &[f32],
    frame_count: usize,
    channels: usize,
    kernel: &[f64; HALF_BAND_TAPS],
    gain: f64,
) -> Result<Vec<f32>, String> {
    let output_frames = frame_count.div_ceil(2);
    let mut result = Vec::new();
    result
        .try_reserve_exact(output_frames * channels)
        .map_err(|_| "could not allocate granular read level")?;
    for frame in 0..output_frames {
        let center = frame * 2;
        let mut total = [0.0; 2];
        for (tap, coefficient) in kernel.iter().enumerate() {
            let input = (center + tap)
                .saturating_sub(HALF_BAND_CENTER)
                .min(frame_count - 1);
            for (channel, acc) in total[..channels].iter_mut().enumerate() {
                *acc += f64::from(source[input * channels + channel]) * coefficient;
            }
        }
        result.extend(total[..channels].iter().map(|value| (value / gain) as f32));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimation_suppresses_aliases_without_shifting_or_mixing_channels() {
        let frames = 4096;
        let phase = |frame, frequency| std::f64::consts::TAU * frame as f64 * frequency;
        let low = |frame| phase(frame, 1.0 / 32.0).sin();
        let high = |frame| phase(frame, 3.0 / 16.0).sin();
        let input: Vec<f32> = (0..frames)
            .flat_map(|i| [(low(i) + high(i)) as f32, (-low(i) + high(i)) as f32])
            .collect();
        let prepared = DecimatedSource::prepare(&input, 2).unwrap();
        let mut error = 0.0;
        for frame in (256..frames - 256).step_by(4) {
            let (left, right) = prepared.read(frame as f64, 4.0, frames, 2).unwrap();
            error += (left - low(frame)).powi(2) + (right + low(frame)).powi(2);
            assert_eq!(
                prepared.read(frame as f64, -4.0, frames, 2),
                Some((left, right))
            );
        }
        assert!(error < 0.002, "alias or phase error: {error}");
        assert_eq!(prepared.read(1.25, 1.0, frames, 2), None);
        assert_eq!(
            prepared.read(frames as f64, 4.0, frames, 2),
            Some((0.0, 0.0))
        );
    }

    #[test]
    fn half_rate_levels_keep_dc_for_short_and_odd_sources() {
        let prepared = DecimatedSource::prepare(&[0.125, 0.125, 0.125], 1).unwrap();
        assert_eq!(prepared.read(2.5, f64::MAX, 3, 1), Some((0.125, 0.125)));
        assert_eq!(prepared.read(0.0, f64::NAN, 3, 1), None);
        let one = DecimatedSource::prepare(&[1.0], 1).unwrap();
        assert_eq!(one.read(0.0, 9.0, 1, 1), None);
    }
}
