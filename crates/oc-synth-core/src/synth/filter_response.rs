//! Offline magnitude-response measurement for the voice filters: an impulse
//! through the voice dispatch, evaluated at log-spaced frequencies. Test-only;
//! the ignored report prints the tables summarised in `docs/filter-character.md`.

use super::*;

const PROBE_RATE: f64 = 48_000.0;
/// Small enough that the ladder's soft clip stays in its linear region.
const PROBE_AMPLITUDE: f64 = 1.0e-4;

/// Log-spaced probe frequencies, five per octave from 20 Hz to the top of the
/// cutoff range.
fn probe_frequencies() -> Vec<f64> {
    let mut out = Vec::with_capacity(56);
    let mut f = 20.0;
    while f < 0.45 * PROBE_RATE {
        out.push(f);
        f *= 2f64.powf(1.0 / 5.0);
    }
    out.push(0.45 * PROBE_RATE);
    out
}

fn impulse_response(
    model: FilterModel,
    mode: FilterMode,
    cutoff: f64,
    resonance: f64,
    samples: usize,
) -> Vec<f64> {
    // Mirror the voice's cutoff clamp so a knob position at the top of the
    // range measures what the voice would actually run.
    let cutoff = cutoff.clamp(8.0, 0.45 * PROBE_RATE);
    let coefficients = FilterCoefficients::new(model, cutoff, resonance, PROBE_RATE);
    let mut svf = StateVariableFilter::default();
    let mut ladder = LadderFilter::default();
    let mut moog = MoogLadder::default();
    (0..samples)
        .map(|index| {
            let input = if index == 0 { PROBE_AMPLITUDE } else { 0.0 };
            process_voice_filter(
                model,
                &mut svf,
                &mut ladder,
                &mut moog,
                input,
                0.0,
                coefficients,
                mode,
            )
        })
        .collect()
}

/// Magnitude of `response` at each probe frequency, in dB relative to the
/// impulse amplitude.
fn magnitude_db(response: &[f64], frequencies: &[f64]) -> Vec<f64> {
    frequencies
        .iter()
        .map(|&frequency| {
            let step = 2.0 * std::f64::consts::PI * frequency / PROBE_RATE;
            let (mut real, mut imaginary) = (0.0, 0.0);
            for (index, &sample) in response.iter().enumerate() {
                let phase = step * index as f64;
                real += sample * phase.cos();
                imaginary -= sample * phase.sin();
            }
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            20.0 * (magnitude / PROBE_AMPLITUDE).log10()
        })
        .collect()
}

struct Response {
    response: Vec<f64>,
    frequencies: Vec<f64>,
    db: Vec<f64>,
}

impl Response {
    fn measure(model: FilterModel, mode: FilterMode, cutoff: f64, resonance: f64) -> Self {
        let samples = (2.5 * PROBE_RATE) as usize;
        let response = impulse_response(model, mode, cutoff, resonance, samples);
        let frequencies = probe_frequencies();
        let db = magnitude_db(&response, &frequencies);
        Self {
            response,
            frequencies,
            db,
        }
    }

    /// Gain at one frequency, in dB relative to the impulse amplitude.
    fn magnitude_at(&self, frequency: f64) -> f64 {
        let step = 2.0 * std::f64::consts::PI * frequency / PROBE_RATE;
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (index, &sample) in self.response.iter().enumerate() {
            let phase = step * index as f64;
            real += sample * phase.cos();
            imaginary -= sample * phase.sin();
        }
        20.0 * ((real * real + imaginary * imaginary).sqrt() / PROBE_AMPLITUDE).log10()
    }

    /// Mean gain over the passband, in dB. The band stops where the resonance
    /// shoulder starts to dominate, so the number describes loudness rather
    /// than the peak.
    fn passband_db(&self, cutoff: f64) -> f64 {
        self.band_db(20.0, 0.7 * cutoff)
    }

    /// Mean gain of the flat part of the passband, used as the -3 dB reference.
    fn plateau_db(&self, cutoff: f64) -> f64 {
        self.band_db(20.0, cutoff / 8.0)
    }

    fn band_db(&self, low: f64, high: f64) -> f64 {
        let band: Vec<f64> = self
            .db
            .iter()
            .zip(&self.frequencies)
            .filter(|(_, f)| **f >= low && **f <= high)
            .map(|(&db, _)| db)
            .collect();
        band.iter().sum::<f64>() / band.len() as f64
    }

    /// Peak of the response. The coarse grid locates it, then a fine sweep
    /// around that point resolves narrow resonances that the grid can miss.
    fn peak(&self) -> (f64, f64) {
        let mut best = (0.0, -200.0);
        for (&frequency, &db) in self.frequencies.iter().zip(&self.db) {
            if db > best.1 {
                best = (frequency, db);
            }
        }
        let low = best.0 * 0.85;
        let high = best.0 * 1.18;
        let mut peak = best;
        for step in 0..=80 {
            let frequency = low * (high / low).powf(step as f64 / 80.0);
            let db = self.magnitude_at(frequency);
            if db > peak.1 {
                peak = (frequency, db);
            }
        }
        peak
    }

    /// First frequency above the passband where the response is 3 dB below the
    /// flat part of the passband.
    fn minus_three_db(&self, cutoff: f64) -> f64 {
        let plateau = self.plateau_db(cutoff);
        for (index, &db) in self.db.iter().enumerate() {
            if self.frequencies[index] > cutoff / 8.0 && db < plateau - 3.0 {
                return self.frequencies[index];
            }
        }
        f64::NAN
    }

    /// Least-squares slope in dB/oct over the octave above the corner.
    fn slope_db_per_octave(&self, cutoff: f64) -> f64 {
        let points: Vec<(f64, f64)> = self
            .frequencies
            .iter()
            .zip(&self.db)
            .filter(|(f, _)| **f >= 2.0 * cutoff && **f <= 8.0 * cutoff)
            .map(|(&f, &db)| (f.log2(), db))
            .collect();
        if points.len() < 2 {
            return f64::NAN;
        }
        let n = points.len() as f64;
        let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
        let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
        let covariance = points
            .iter()
            .map(|p| (p.0 - mean_x) * (p.1 - mean_y))
            .sum::<f64>();
        let variance = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum::<f64>();
        covariance / variance
    }
}

fn filter_id_model(id: &str) -> (FilterModel, FilterMode) {
    (super::filter_id::model(id), super::filter_id::mode(id))
}

#[test]
#[ignore = "prints the response tables quoted in docs/filter-character.md"]
fn report_filter_response() {
    let cutoff = 425.0;
    let ids = [
        "L6", "L12", "L18", "L24", "H12", "H24", "B12", "B24", "MgL6", "MgL12", "MgL18", "MgL24",
        "LadderMg", "DirtyMg",
    ];
    println!("\n### Response at fc = {cutoff} Hz (48 kHz)\n");
    println!("| id | res | passband dB | -3 dB Hz | peak dB | peak Hz | slope dB/oct |");
    println!("|---|---|---|---|---|---|---|");
    for id in ids {
        let (model, mode) = filter_id_model(id);
        for resonance in [0.0, 0.7, 0.98] {
            let response = Response::measure(model, mode, cutoff, resonance);
            let (peak_hz, peak_db) = response.peak();
            println!(
                "| {id} | {resonance:.2} | {passband:+.2} | {minus3:.0} | {peak_db:+.2} | {peak_hz:.0} | {slope:+.1} |",
                passband = response.passband_db(cutoff),
                minus3 = response.minus_three_db(cutoff),
                slope = response.slope_db_per_octave(cutoff),
            );
        }
    }

    println!("\n### Passband trim, low-pass two-pole (mean 20 Hz..0.7 fc)\n");
    println!("| fc Hz | res | before dB | after dB |");
    println!("|---|---|---|---|");
    for cutoff in [129.9, 425.0, 4_544.0] {
        for resonance in [0.0, 0.7, 0.98] {
            let response =
                Response::measure(FilterModel::Svf, FilterMode::LowPass, cutoff, resonance);
            let after = response.passband_db(cutoff);
            let before = after - 20.0 * super::svf_input_gain(resonance).log10();
            println!("| {cutoff:.0} | {resonance:.2} | {before:+.2} | {after:+.2} |");
        }
    }

    println!("\n### Peak versus cutoff (res 0.98)\n");
    println!("| id | fc Hz | passband dB | peak dB | peak Hz |");
    println!("|---|---|---|---|---|");
    for id in ids {
        let (model, mode) = filter_id_model(id);
        for cutoff in [129.9, 425.0, 1_390.5, 4_544.0, 11_000.0] {
            let response = Response::measure(model, mode, cutoff, 0.98);
            let (peak_hz, peak_db) = response.peak();
            println!(
                "| {id} | {cutoff:.0} | {:+.2} | {peak_db:+.2} | {peak_hz:.0} |",
                response.passband_db(cutoff)
            );
        }
    }
}

/// The passband trim keeps the loudness of the state-variable family constant
/// as resonance rises. Passband = 20 Hz to one third of the corner.
#[test]
fn passband_level_is_flat_across_resonance() {
    for cutoff in [129.9, 425.0, 4_544.0] {
        let mut levels = Vec::new();
        for resonance in [0.0, 0.7, 0.98] {
            let response =
                Response::measure(FilterModel::Svf, FilterMode::LowPass, cutoff, resonance);
            levels.push(response.passband_db(cutoff));
        }
        for level in &levels[1..] {
            assert!(
                (level - levels[0]).abs() <= 1.0,
                "cutoff {cutoff}: passband {levels:?} drifts across resonance"
            );
        }
        assert!(
            levels.iter().all(|level| level.abs() <= 1.2),
            "cutoff {cutoff}: passband {levels:?} is outside the flatness target"
        );
    }
}

fn cutoff_for_norm(norm: f64) -> f64 {
    8.1758 * 2f64.powf(11.4 * norm)
}

/// The resonance ceiling must not depend on the cutoff: the peak stays at the
/// same level across the whole knob range. The capped peak is deliberately
/// broad, so its exact location is not an invariant.
#[test]
fn peak_is_cutoff_independent_at_full_resonance() {
    let ceiling = 20.0 * super::MAX_RESONANT_GAIN.log10();
    let mut peaks = Vec::new();
    for norm in [0.2, 0.35, 0.5, 0.6655, 0.8, 0.9, 1.0] {
        let cutoff = cutoff_for_norm(norm);
        let response = Response::measure(FilterModel::Svf, FilterMode::LowPass, cutoff, 1.0);
        let (_, peak_db) = response.peak();
        assert!(
            peak_db <= ceiling + 0.5,
            "norm {norm}: peak {peak_db:+.2} dB exceeds {ceiling:+.2} dB"
        );
        peaks.push(peak_db);
    }
    let highest = peaks.iter().copied().fold(f64::MIN, f64::max);
    let lowest = peaks.iter().copied().fold(f64::MAX, f64::min);
    assert!(
        highest - lowest <= 1.0,
        "peak varies {:.2} dB across cutoff: {peaks:?}",
        highest - lowest
    );
}

/// At full resonance the loop still decays: a zero state never rings and an
/// impulse has a finite tail.
#[test]
fn full_resonance_impulse_decays_and_zero_input_stays_silent() {
    for cutoff in [40.0, 425.0, 4_000.0] {
        let coefficients = FilterCoefficients::new(FilterModel::Svf, cutoff, 1.0, PROBE_RATE);
        let mut svf = StateVariableFilter::default();
        let mut ladder = LadderFilter::default();
        let mut moog = MoogLadder::default();
        for _ in 0..4_800 {
            let output = process_voice_filter(
                FilterModel::Svf,
                &mut svf,
                &mut ladder,
                &mut moog,
                0.0,
                0.0,
                coefficients,
                FilterMode::LowPass,
            );
            assert_eq!(output, 0.0, "zero input produced output at {cutoff} Hz");
        }

        let mut svf = StateVariableFilter::default();
        let mut ladder = LadderFilter::default();
        let mut moog = MoogLadder::default();
        let mut tail = 0.0_f64;
        for index in 0..(PROBE_RATE as usize) {
            let input = if index == 0 { 1.0 } else { 0.0 };
            let output = process_voice_filter(
                FilterModel::Svf,
                &mut svf,
                &mut ladder,
                &mut moog,
                input,
                0.0,
                coefficients,
                FilterMode::LowPass,
            );
            if index > (0.75 * PROBE_RATE) as usize {
                tail = tail.max(output.abs());
            }
        }
        assert!(tail < 1e-4, "cutoff {cutoff}: tail {tail:e} did not decay");
    }
}

/// Each cascade adds one fixed stage, so the asymptotic slope grows by
/// 6 dB/oct per pole while the peak stays at the corner and below the ceiling.
#[test]
fn cascades_steepen_the_slope_by_six_db_per_octave() {
    let cutoff = 425.0;
    for (model, poles, expected_slope) in [
        (FilterModel::Svf, 2, 12.0),
        (FilterModel::Svf3, 3, 18.0),
        (FilterModel::Svf4, 4, 24.0),
    ] {
        let response = Response::measure(model, FilterMode::LowPass, cutoff, 0.0);
        let slope = response.slope_db_per_octave(cutoff).abs();
        assert!(
            (slope - expected_slope).abs() <= 2.0,
            "{poles} poles: {slope:.1} dB/oct vs {expected_slope:.0}"
        );
        assert_eq!(model.poles(), poles);

        for resonance in [0.7, 0.98] {
            let response = Response::measure(model, FilterMode::LowPass, cutoff, resonance);
            let (_, peak_db) = response.peak();
            assert!(
                peak_db <= 20.0 * super::MAX_RESONANT_GAIN.log10(),
                "{poles} poles at res {resonance}: peak {peak_db:+.2} dB"
            );
        }
    }
}

const MOOG_TAPS: [(FilterModel, f64); 4] = [
    (FilterModel::Moog6, 6.0),
    (FilterModel::Moog12, 12.0),
    (FilterModel::Moog18, 18.0),
    (FilterModel::Moog24, 24.0),
];

/// The ladder taps keep the one-pole slope of their stage count. The fit over
/// the first octave is shallow in the transition, so allow 3 dB/oct.
#[test]
fn moog_taps_keep_their_slope() {
    for (model, expected) in MOOG_TAPS {
        let response = Response::measure(model, FilterMode::LowPass, 425.0, 0.0);
        let slope = response.slope_db_per_octave(425.0).abs();
        assert!(
            (slope - expected).abs() <= 3.0,
            "{expected:.0} dB tap measured {slope:.1} dB/oct"
        );
    }
}

/// The loop feedback is the same for every tap, so an earlier tap has less
/// loop gain and peaks hotter: the tap order is 6 > 12 > 18 > 24 dB.
#[test]
fn moog_taps_peak_in_pole_order() {
    let mut peaks = Vec::new();
    for (model, _) in MOOG_TAPS {
        let response = Response::measure(model, FilterMode::LowPass, 425.0, 0.98);
        let (peak_hz, peak_db) = response.peak();
        assert!(
            peak_hz > 420.0 && peak_hz < 700.0,
            "{model:?} peaks at {peak_hz:.0} Hz"
        );
        peaks.push(peak_db);
    }
    for pair in peaks.windows(2) {
        assert!(
            pair[0] >= pair[1] - 0.5,
            "later tap should not peak hotter: {peaks:?}"
        );
    }
}

/// The whole resonance range is stable: an impulse decays at every cutoff and
/// a zero state stays silent.
#[test]
fn moog_range_is_stable_at_full_resonance() {
    for (model, _) in MOOG_TAPS {
        for cutoff in [80.0, 425.0, 4_000.0, 16_000.0] {
            let coefficients = FilterCoefficients::new(model, cutoff, 1.0, PROBE_RATE);
            let mut svf = StateVariableFilter::default();
            let mut ladder = LadderFilter::default();
            let mut moog = MoogLadder::default();
            for _ in 0..2_400 {
                let output = process_voice_filter(
                    model,
                    &mut svf,
                    &mut ladder,
                    &mut moog,
                    0.0,
                    0.0,
                    coefficients,
                    FilterMode::LowPass,
                );
                assert_eq!(output, 0.0, "{model:?} rang from a zero state");
            }

            let mut svf = StateVariableFilter::default();
            let mut ladder = LadderFilter::default();
            let mut moog = MoogLadder::default();
            let mut tail = 0.0_f64;
            let mut peak = 0.0_f64;
            for index in 0..(2.0 * PROBE_RATE) as usize {
                let input = if index == 0 { 1e-3 } else { 0.0 };
                let output = process_voice_filter(
                    model,
                    &mut svf,
                    &mut ladder,
                    &mut moog,
                    input,
                    0.0,
                    coefficients,
                    FilterMode::LowPass,
                );
                peak = peak.max(output.abs());
                if index > (PROBE_RATE * 1.5) as usize {
                    tail = tail.max(output.abs());
                }
            }
            assert!(peak.is_finite() && peak < 2.0, "{model:?} peak {peak}");
            assert!(
                tail < 1e-6,
                "{model:?} at {cutoff} Hz still rings at {tail:e}"
            );

            // A full-scale impulse is bounded by the feedback saturation.
            let mut svf = StateVariableFilter::default();
            let mut ladder = LadderFilter::default();
            let mut moog = MoogLadder::default();
            let mut loud = 0.0_f64;
            for index in 0..4_800 {
                let input = if index == 0 { 1.0 } else { 0.0 };
                let output = process_voice_filter(
                    model,
                    &mut svf,
                    &mut ladder,
                    &mut moog,
                    input,
                    0.0,
                    coefficients,
                    FilterMode::LowPass,
                );
                assert!(output.is_finite(), "{model:?} went non-finite");
                loud = loud.max(output.abs());
            }
            assert!(loud < 10.0, "{model:?} drove to {loud}");
        }
    }
}
