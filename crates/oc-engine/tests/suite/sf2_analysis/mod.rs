//! Small analysis helpers for the SoundFont playback tests (no dependencies).

#![allow(dead_code)]

/// Magnitude of one DFT bin, evaluated with a single-frequency resonator.
pub fn goertzel(samples: &[f32], sample_rate: f64, frequency: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0f64;
    let mut before = 0.0f64;
    for sample in samples {
        let current = f64::from(*sample) + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    (previous * previous + before * before - coefficient * previous * before)
        .max(0.0)
        .sqrt()
        / samples.len() as f64
}

/// Strongest frequency in `[low, high]`, scanned in `step` hertz increments.
pub fn dominant_frequency(
    samples: &[f32],
    sample_rate: f64,
    low: f64,
    high: f64,
    step: f64,
) -> f64 {
    let mut best = (0.0, 0.0);
    let mut frequency = low;
    while frequency <= high {
        let magnitude = goertzel(samples, sample_rate, frequency);
        if magnitude > best.1 {
            best = (frequency, magnitude);
        }
        frequency += step;
    }
    best.0
}

pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt()
}

pub fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |worst, sample| worst.max(sample.abs()))
}

/// Left channel of an interleaved stereo buffer.
pub fn left(interleaved: &[f32]) -> Vec<f32> {
    interleaved.chunks(2).map(|frame| frame[0]).collect()
}

/// Right channel of an interleaved stereo buffer.
pub fn right(interleaved: &[f32]) -> Vec<f32> {
    interleaved
        .chunks(2)
        .map(|frame| frame.get(1).copied().unwrap_or(0.0))
        .collect()
}

/// Average of both channels, as a mono view for spectral tests.
pub fn mono(interleaved: &[f32]) -> Vec<f32> {
    interleaved
        .chunks(2)
        .map(|frame| (frame[0] + frame.get(1).copied().unwrap_or(0.0)) * 0.5)
        .collect()
}

pub fn db(value: f64) -> f64 {
    20.0 * value.max(1e-12).log10()
}

/// Times, in seconds, of the negative-to-positive zero crossings of `samples`.
pub fn rising_crossings(samples: &[f32], sample_rate: f64) -> Vec<f64> {
    let mut times = Vec::new();
    let mut previous = 0.0f32;
    for (index, sample) in samples.iter().enumerate() {
        if previous < 0.0 && *sample >= 0.0 && index > 0 {
            let before = f64::from(previous);
            let after = f64::from(*sample);
            // Linear interpolation between the two samples around the crossing.
            let fraction = if after == before {
                0.0
            } else {
                -before / (after - before)
            };
            times.push((index as f64 - 1.0 + fraction) / sample_rate);
        }
        previous = *sample;
    }
    times
}

/// Pitch deviation in cents, measured as the half peak-to-peak range of the
/// per-cycle period track.
///
/// Period extremes are the two ends of a vibrato and are immune to the sampling
/// bias a median-referenced cents track picks up; the percentiles keep a single
/// missed crossing from setting the number.
pub fn pitch_depth_cents(samples: &[f32], sample_rate: f64) -> f64 {
    let times = rising_crossings(samples, sample_rate);
    let mut periods: Vec<f64> = times
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|period| *period > 0.0)
        .collect();
    if periods.len() < 16 {
        return 0.0;
    }
    periods.sort_by(f64::total_cmp);
    let low = periods[(periods.len() as f64 * 0.005) as usize];
    let high = periods[((periods.len() - 1) as f64 * 0.995) as usize];
    600.0 / std::f64::consts::LN_2 * (high / low).ln()
}

/// Per-cycle pitch in cents relative to the median period, for shape and rate
/// checks.
pub fn cents_track(times: &[f64]) -> Vec<f64> {
    let log_periods: Vec<f64> = times
        .windows(2)
        .filter_map(|pair| {
            let period = pair[1] - pair[0];
            (period > 0.0).then(|| period.ln())
        })
        .collect();
    let mut sorted = log_periods.clone();
    sorted.sort_by(f64::total_cmp);
    let Some(median) = sorted.get(sorted.len() / 2).copied() else {
        return Vec::new();
    };
    log_periods
        .iter()
        .map(|period| -1200.0 / std::f64::consts::LN_2 * (period - median))
        .collect()
}

/// Median-period pitch in cents in a window, relative to `reference_hz`
/// (positive = sharper).
pub fn pitch_cents_at(
    samples: &[f32],
    sample_rate: f64,
    start_seconds: f64,
    window_seconds: f64,
    reference_hz: f64,
) -> f64 {
    let low = ((start_seconds * sample_rate) as usize).min(samples.len());
    let high = (((start_seconds + window_seconds) * sample_rate) as usize).min(samples.len());
    let times = rising_crossings(&samples[low..high], sample_rate);
    let mut periods: Vec<f64> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    if periods.len() < 4 {
        return f64::NAN;
    }
    periods.sort_by(f64::total_cmp);
    let median = periods[periods.len() / 2];
    1200.0 / std::f64::consts::LN_2 * (1.0 / (reference_hz * median)).ln()
}

/// Magnitude of one frequency in a window.
pub fn band_level(
    samples: &[f32],
    sample_rate: f64,
    start_seconds: f64,
    window_seconds: f64,
    hz: f64,
) -> f64 {
    let low = ((start_seconds * sample_rate) as usize).min(samples.len());
    let high = (((start_seconds + window_seconds) * sample_rate) as usize).min(samples.len());
    goertzel(&samples[low..high], sample_rate, hz)
}

/// Level of a window in decibels.
pub fn level_db_at(
    samples: &[f32],
    sample_rate: f64,
    start_seconds: f64,
    window_seconds: f64,
) -> f64 {
    let low = ((start_seconds * sample_rate) as usize).min(samples.len());
    let high = (((start_seconds + window_seconds) * sample_rate) as usize).min(samples.len());
    db(rms(&samples[low..high]))
}

/// Peak-to-peak range of short-window levels between two times, in decibels.
pub fn level_range_db(samples: &[f32], sample_rate: f64, start: f64, end: f64, window: f64) -> f64 {
    let frames = (window * sample_rate) as usize;
    let low = (start * sample_rate) as usize;
    let high = ((end * sample_rate) as usize).min(samples.len());
    let mut levels: Vec<f64> = samples[low..high]
        .chunks(frames)
        .filter(|chunk| chunk.len() == frames)
        .map(|chunk| db(rms(chunk)))
        .collect();
    if levels.len() < 2 {
        return 0.0;
    }
    levels.sort_by(f64::total_cmp);
    levels[levels.len() - 1] - levels[0]
}

/// Magnitude response of the two-pole low-pass the voice builds, in decibels.
///
/// Prewarped analog prototype: |H| = 1 / sqrt((1 - r^2)^2 + (r / Q)^2) with
/// `r = tan(pi f / fs) / tan(pi fc / fs)`.
pub fn lowpass_db(freq: f64, cutoff_hz: f64, q: f64, sample_rate: f64) -> f64 {
    let r = (std::f64::consts::PI * freq / sample_rate).tan()
        / (std::f64::consts::PI * cutoff_hz / sample_rate).tan();
    let magnitude = 1.0 / ((1.0 - r * r).powi(2) + (r / q).powi(2)).sqrt();
    20.0 * magnitude.max(1e-12).log10()
}
