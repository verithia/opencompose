//! Observed attacks and plausible constant-tempo pulses for a selected passage.

use serde::Serialize;

use super::{AudioBuffer, detect_onsets, onset_flux_floor};

const HOP: f64 = 240.0;
const WINDOW_CENTRE: f64 = 512.0;
const MIN_TEMPO: f64 = 55.0;
const MAX_TEMPO: f64 = 200.0;
const MIN_ATTACKS: usize = 5;
const MAX_ANCHORS: usize = 24;

#[derive(Debug, Serialize)]
/// Beat evidence over one source window.
pub struct BeatEvidence {
    pub start_s: f64,
    pub end_s: f64,
    pub attack_count: usize,
    pub candidates: Vec<TempoCandidate>,
}

#[derive(Debug, Serialize)]
/// One tempo candidate with its pulse evidence.
pub struct TempoCandidate {
    pub tempo_bpm: f64,
    /// Relative strength of the pulse evidence, not a probability.
    pub score: f64,
    /// Estimated pulse phase expressed as a source timestamp near the first attack.
    pub pulse_origin_s: f64,
    /// Observed source timestamps near the pulse, not generated grid lines.
    pub anchors_s: Vec<f64>,
}

/// Estimate a pulse in a bounded audio passage without inferring its meter,
/// downbeat, or tempo map. Weak or nonperiodic passages can yield no candidates.
pub fn estimate(buffer: &AudioBuffer, start_s: f64) -> BeatEvidence {
    let mut report = BeatEvidence {
        start_s,
        end_s: start_s + buffer.duration_s(),
        attack_count: 0,
        candidates: Vec::new(),
    };
    if buffer.sample_rate < 8_000 || !(4.0..=30.01).contains(&buffer.duration_s()) {
        return report;
    }

    let rate = f64::from(buffer.sample_rate) / HOP;
    let flux = onset_flux_floor(buffer, 1e-7);
    if flux.iter().all(|&strength| strength <= f64::EPSILON) {
        return report;
    }
    let attacks = detect_onsets(&flux, rate);
    report.attack_count = attacks.len();
    if attacks.len() < MIN_ATTACKS {
        return report;
    }
    let mut pulses = vec![0.0_f64; flux.len()];
    let radius = (0.024 * rate).ceil() as usize;
    for &attack in &attacks {
        let from = attack.saturating_sub(radius);
        let to = (attack + radius + 1).min(pulses.len());
        for (delta, value) in pulses[from..to].iter_mut().enumerate() {
            let distance = attack.abs_diff(from + delta) as f64 / radius as f64;
            *value = value.max(1.0 - distance);
        }
    }

    let low = (60.0 * rate / MAX_TEMPO).floor().max(1.0) as usize;
    let high = (60.0 * rate / MIN_TEMPO).ceil() as usize;
    let scores: Vec<f64> = (low - 1..=high + 1)
        .map(|lag| 0.7 * similarity(&pulses, lag) + 0.3 * similarity(&pulses, lag * 2))
        .collect();
    let mut peaks: Vec<_> = (1..scores.len() - 1)
        .filter(|&i| scores[i] >= 0.22 && scores[i] >= scores[i - 1] && scores[i] > scores[i + 1])
        .collect();
    peaks.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    for index in peaks.into_iter().take(12) {
        let curvature = scores[index - 1] - 2.0 * scores[index] + scores[index + 1];
        let refinement = if curvature < 0.0 {
            (0.5 * (scores[index - 1] - scores[index + 1]) / curvature).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let period = (low - 1 + index) as f64 + refinement;
        let tempo = 60.0 * rate / period;
        if !(MIN_TEMPO..=MAX_TEMPO).contains(&tempo) {
            continue;
        }
        let tolerance = (0.040 * rate).max(1.0);
        let phase_distance = |frame: usize, origin: usize| {
            let phase = (frame as f64 - origin as f64) / period;
            (phase - phase.round()).abs() * period
        };
        // Pick the phase with the most nearby measured attacks, allowing a
        // kick and a delayed snare to support different phase hypotheses.
        let origin = attacks
            .iter()
            .copied()
            .max_by(|&a, &b| {
                let support = |origin| {
                    attacks
                        .iter()
                        .map(|&attack| (1.0 - phase_distance(attack, origin) / tolerance).max(0.0))
                        .sum::<f64>()
                };
                support(a).total_cmp(&support(b))
            })
            .expect("the passage has enough attacks");

        let aligned: Vec<_> = attacks
            .iter()
            .copied()
            .filter(|&attack| phase_distance(attack, origin) <= tolerance)
            .collect();
        let first = attacks[0];
        let last = attacks[attacks.len() - 1];
        let expected = (1.0 + (last - first) as f64 / period).round();
        let coverage = (aligned.len() as f64 / expected).min(1.0);
        if aligned.len() < 4 || coverage < 0.35 {
            continue;
        }
        let score = 0.65 * scores[index]
            + 0.20 * coverage
            + 0.15 * aligned.len() as f64 / attacks.len() as f64;
        let frame_to_source = |frame: f64| {
            start_s + (frame * HOP + WINDOW_CENTRE + HOP / 2.0) / f64::from(buffer.sample_rate)
        };
        let first_cycle = ((first as f64 - origin as f64) / period).round();
        let mut pulse_origin_s = frame_to_source(origin as f64 + first_cycle * period);
        if pulse_origin_s < start_s {
            pulse_origin_s += period / rate;
        }
        let count = aligned.len().min(MAX_ANCHORS);
        let anchors_s = (0..count)
            .map(|index| {
                let selected = index * (aligned.len() - 1) / (count - 1);
                frame_to_source(aligned[selected] as f64)
            })
            .collect();
        report.candidates.push(TempoCandidate {
            tempo_bpm: tempo,
            score,
            pulse_origin_s,
            anchors_s,
        });
    }
    report
        .candidates
        .sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut selected: Vec<TempoCandidate> = Vec::new();
    for candidate in report.candidates.drain(..) {
        if selected
            .iter()
            .all(|other| (other.tempo_bpm - candidate.tempo_bpm).abs() > 0.04 * candidate.tempo_bpm)
        {
            selected.push(candidate);
        }
        if selected.len() == 3 {
            break;
        }
    }
    report.candidates = selected;
    report
}

fn similarity(pulses: &[f64], lag: usize) -> f64 {
    if lag >= pulses.len() {
        return 0.0;
    }
    let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
    for (&x, &y) in pulses.iter().zip(&pulses[lag..]) {
        xy += x * y;
        xx += x * x;
        yy += y * y;
    }
    if xx > 0.0 && yy > 0.0 {
        xy / f64::sqrt(xx * yy)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicks(rate: u32, seconds: usize, times: impl Iterator<Item = f64>) -> AudioBuffer {
        let mut samples = vec![0.0; rate as usize * seconds];
        for time in times {
            let offset = (time * f64::from(rate)).round() as usize;
            for (index, sample) in samples[offset..].iter_mut().take(192).enumerate() {
                *sample += 0.7 * (index as f64 * 0.4).sin() * (-0.02 * index as f64).exp();
            }
        }
        AudioBuffer {
            sample_rate: rate,
            channels: vec![samples],
        }
    }

    #[test]
    fn detects_the_pulse_and_reports_attacks_in_source_seconds() {
        for (rate, tempo) in [(24_000, 127.0), (48_000, 86.0)] {
            let period = 60.0 / tempo;
            let times = (0..18).map(|index| 0.27 + index as f64 * period);
            let audio = clicks(rate, 15, times);
            let evidence = estimate(&audio, 42.0);
            let pulse = &evidence.candidates[0];
            assert!((pulse.tempo_bpm - tempo).abs() < 1.0, "{pulse:?}");
            assert!(pulse.anchors_s.len() >= 12, "{pulse:?}");
            assert!((pulse.anchors_s[0] - 42.27).abs() < 0.04, "{pulse:?}");
            assert!(pulse.anchors_s.iter().all(|&time| {
                let phase = (time - 42.27) / period;
                (phase - phase.round()).abs() * period < 0.04
            }));
        }
    }

    #[test]
    fn leaves_silence_and_sparse_hits_without_a_grid() {
        for times in [vec![], vec![0.2, 1.2, 3.2, 5.8]] {
            let audio = clicks(24_000, 8, times.into_iter());
            let evidence = estimate(&audio, 0.0);
            assert!(evidence.candidates.is_empty(), "{evidence:?}");
        }
    }
}
