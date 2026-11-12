//! Spectral helpers for the SoundFont playback tests, independent of the
//! engine's own state: a windowed block is transformed and its magnitude
//! spectrum summarised as a centroid.

#![allow(dead_code)]

/// Iterative radix-2 Cooley-Tukey transform, in place.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        let (twiddle_re, twiddle_im) = (angle.cos(), angle.sin());
        let mut start = 0;
        while start < n {
            let (mut w_re, mut w_im) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (ur, ui) = (re[start + k], im[start + k]);
                let (br, bi) = (re[start + k + len / 2], im[start + k + len / 2]);
                let (vr, vi) = (br * w_re - bi * w_im, br * w_im + bi * w_re);
                re[start + k] = ur + vr;
                im[start + k] = ui + vi;
                re[start + k + len / 2] = ur - vr;
                im[start + k + len / 2] = ui - vi;
                let next_re = w_re * twiddle_re - w_im * twiddle_im;
                w_im = w_re * twiddle_im + w_im * twiddle_re;
                w_re = next_re;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// Magnitude-weighted spectral centroid in hertz of one window, ignoring
/// content below `low_hz` and above `high_hz`.
///
/// The block is Hann-windowed and zero-padded to a power of two, so a window
/// that is not a whole number of cycles still reads as one spectral shape.
pub fn spectral_centroid(samples: &[f32], sample_rate: f64, low_hz: f64, high_hz: f64) -> f64 {
    let mut size = 1usize;
    while size < samples.len() {
        size <<= 1;
    }
    let size = size.max(1024);
    let mut re = vec![0.0f64; size];
    let mut im = vec![0.0f64; size];
    let last = samples.len().saturating_sub(1).max(1) as f64;
    for (index, sample) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / last).cos();
        re[index] = f64::from(*sample) * window;
    }
    fft(&mut re, &mut im);
    let bin_hz = sample_rate / size as f64;
    let mut weighted = 0.0f64;
    let mut total = 0.0f64;
    for bin in 1..size / 2 {
        let frequency = bin as f64 * bin_hz;
        if frequency < low_hz || frequency > high_hz {
            continue;
        }
        let magnitude = (re[bin] * re[bin] + im[bin] * im[bin]).sqrt();
        weighted += frequency * magnitude;
        total += magnitude;
    }
    if total > 0.0 { weighted / total } else { 0.0 }
}

/// Time in seconds at which the short-window level first falls `drop_db`
/// below its peak. The window must span a whole period of the lowest note
/// under test, so a 20 ms window suits down to 50 Hz.
pub fn level_drop_seconds(
    samples: &[f32],
    sample_rate: f64,
    window_seconds: f64,
    drop_db: f64,
) -> f64 {
    let window = (sample_rate * window_seconds) as usize;
    if window == 0 {
        return 0.0;
    }
    let levels: Vec<f64> = samples
        .chunks_exact(window)
        .map(|chunk| {
            let sum: f64 = chunk.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            (sum / window as f64).sqrt()
        })
        .collect();
    let Some(peak) = levels
        .iter()
        .cloned()
        .fold(None, |best: Option<f64>, value| {
            Some(best.map_or(value, |best| best.max(value)))
        })
    else {
        return 0.0;
    };
    if peak <= 0.0 {
        return 0.0;
    }
    let threshold = peak * 10f64.powf(-drop_db / 20.0);
    levels
        .iter()
        .position(|level| *level < threshold)
        .map(|index| index as f64 * window as f64 / sample_rate)
        .unwrap_or(samples.len() as f64 / sample_rate)
}
