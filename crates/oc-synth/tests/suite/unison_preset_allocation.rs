//! The unison preset must allocate every voice it asks for.

use oc_synth::synth::{OpenSynth, load_project_patch};
use std::f64::consts::TAU;

const PRESET: &str = "@project
@preset ours_unison
  source: builtin
  plugin: \"builtin.open_synth\"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.65, unison.voices: 7, unison.detune: 14ct, unison.width: 0.7, filter.mode: 0, filter.cutoff: 9000Hz, filter.resonance: 0.1, amp.attack: 0.02s, amp.decay: 0.5s, amp.sustain: 1, amp.release: 0.3s, quality.oversampling: 2, output.gain: 0.22}
";

const SR: f64 = 48_000.0;
const SECONDS: usize = 4;

/// Hann-windowed amplitude of one frequency, scaled to a full-scale sinusoid.
fn amplitude(samples: &[[f32; 2]], frequency: f64) -> f64 {
    let length = samples.len();
    let (mut re, mut im, mut norm) = (0.0, 0.0, 0.0);
    for (index, frame) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (TAU * index as f64 / length as f64).cos();
        let value = f64::from(frame[0] + frame[1]) * window;
        let angle = TAU * frequency * index as f64 / SR;
        re += value * angle.cos();
        im += value * angle.sin();
        norm += window;
    }
    2.0 * re.hypot(im) / norm
}

fn db(amplitude: f64) -> f64 {
    20.0 * (amplitude + 1e-15).log10()
}

#[test]
fn unison_preset_allocates_all_seven_voices() {
    let project = oc_dsl::parse(PRESET).expect("preset parses");
    let patch = load_project_patch(&project.presets[0].params, None).expect("patch loads");
    assert_eq!(patch.unison_voices, 7, "the preset asks for seven voices");
    assert_eq!(patch.detune_cents, 14.0, "the preset asks for 14 cents");

    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let frames: Vec<[f32; 2]> = (0..SR as usize * SECONDS)
        .map(|_| synth.next_stereo(SR as u32))
        .collect();

    // A seven-voice stack at +/-14 cents puts one partial every 14/3 cents.
    let center = 440.0 * 2f64.powf((60.0 - 69.0) / 12.0);
    let step = 28.0 / 6.0;
    let voices: Vec<f64> = (-3..=3)
        .map(|voice| {
            db(amplitude(
                &frames,
                center * 2f64.powf(voice as f64 * step / 1200.0),
            ))
        })
        .collect();
    let gaps: Vec<f64> = (-4..=3)
        .map(|gap| {
            db(amplitude(
                &frames,
                center * 2f64.powf((gap as f64 + 0.5) * step / 1200.0),
            ))
        })
        .collect();

    let loudest = voices.iter().copied().fold(f64::MIN, f64::max);
    let quietest = voices.iter().copied().fold(f64::MAX, f64::min);
    // Not equal, and the gap grows with width: the centre voice sits the
    // measured 2.45 dB below the outer voices and the width stage lifts the
    // panned ones by 1 + 0.258 w^2, so at width 0.7 the gap is 2.45 + 1.04 =
    // 3.49 dB before partial interference (this preset measures 3.57).
    assert!(
        loudest - quietest < 3.8,
        "the seven partials must sit within the measured balance, spread {:.2} dB",
        loudest - quietest
    );
    // Every requested voice must sit clearly above the gaps between them; a
    // one- or three-voice stack leaves these bins at leakage level.
    let above_gap = quietest - gaps.iter().copied().fold(f64::MIN, f64::max);
    // The centre voice is the quietest partial, and the width lift moved it a
    // further 1.04 dB down relative to the outers, so the margin over the
    // leakage bins between 1.2 Hz-spaced partials is 3.5 dB. A one- or
    // three-voice stack leaves those bins at leakage level, so 3 dB still
    // discriminates.
    assert!(
        above_gap > 3.0,
        "each of the seven partials must stand {above_gap:.1} dB above the gaps"
    );
}

/// With the pan at zero the mono sum shows the voice weights themselves: the
/// centre voice sits the measured 2.45 dB below the outer voices.
#[test]
fn unison_centre_voice_is_lighter_than_the_outer_voices_at_zero_width() {
    let mono_preset = PRESET.replace("unison.width: 0.7", "unison.width: 0");
    let project = oc_dsl::parse(&mono_preset).expect("preset parses");
    let patch = load_project_patch(&project.presets[0].params, None).expect("patch loads");
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 60, 100);
    let frames: Vec<[f32; 2]> = (0..SR as usize * SECONDS)
        .map(|_| synth.next_stereo(SR as u32))
        .collect();

    let centre_hz = 440.0 * 2f64.powf((60.0 - 69.0) / 12.0);
    let step = 28.0 / 6.0;
    let levels: Vec<f64> = (-3..=3)
        .map(|voice| {
            db(amplitude(
                &frames,
                centre_hz * 2f64.powf(voice as f64 * step / 1200.0),
            ))
        })
        .collect();
    let centre = levels[3];
    let outer_mean: f64 = levels
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 3)
        .map(|(_, level)| *level)
        .sum::<f64>()
        / 6.0;
    assert!(
        (outer_mean - centre - 2.45).abs() < 0.35,
        "the centre partial must sit 2.45 dB below the outer mean, got {:.2} dB",
        outer_mean - centre
    );
}

/// The detuned preset must not revert to a coherent stack: a coherent start
/// multiplies the attack by the voice count and swings the envelope far more
/// than the preset's own beating.
#[test]
fn unison_preset_keeps_a_controlled_onset() {
    let project = oc_dsl::parse(PRESET).expect("preset parses");
    let patch = load_project_patch(&project.presets[0].params, None).expect("patch loads");
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, 48, 100);
    let mono: Vec<f64> = (0..SR as usize * 2)
        .map(|_| {
            let [left, right] = synth.next_stereo(SR as u32);
            0.5 * (f64::from(left) + f64::from(right))
        })
        .collect();

    let rms = |from: f64, to: f64| {
        let start = (from * SR) as usize;
        let end = (to * SR) as usize;
        let sum: f64 = mono[start..end].iter().map(|sample| sample * sample).sum();
        (sum / (end - start) as f64).sqrt()
    };
    let steady = rms(0.25, 1.5);
    let onset_peak = mono[..(0.05 * SR) as usize]
        .iter()
        .fold(0.0f64, |peak, sample| peak.max(sample.abs()));
    let onset_crest = 20.0 * (onset_peak / steady).log10();

    // 10 ms RMS envelope peak-to-peak over the sustained part.
    let window = (0.01 * SR) as usize;
    let hop = (0.01 * SR) as usize;
    let mut high = f64::MIN;
    let mut low = f64::MAX;
    for frame in (0..mono.len().saturating_sub(window)).step_by(hop) {
        let start = frame.max((0.25 * SR) as usize);
        if start + window > mono.len() {
            break;
        }
        let sum: f64 = mono[start..start + window]
            .iter()
            .map(|sample| sample * sample)
            .sum();
        let level = 10.0 * (sum / window as f64).log10();
        high = high.max(level);
        low = low.min(level);
    }
    let swing = high - low;
    assert!(
        onset_crest < 13.0,
        "the onset crest must stay below a coherent stack's spike, got {onset_crest:.2} dB"
    );
    // A sanity bound, not a target: the fixed phase layout makes every note
    // share one beat shape, which measures about 0.3 dB rougher here. The A/B
    // pair as heard moves the other way, so the cost is accepted rather than
    // tuned away; docs/synth-character.md records both numbers.
    assert!(
        swing < 11.0,
        "the detuned stack's 10 ms envelope must stay under 11 dB, got {swing:.2} dB"
    );
}
