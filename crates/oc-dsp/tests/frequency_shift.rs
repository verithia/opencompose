use std::f64::consts::TAU;

use oc_dsp::AudioEffect;
use oc_dsp::frequency_shift::{FrequencyShifter, QUADRATURE_LATENCY};

fn magnitude(audio: &[f32], channel: usize, rate: u32, hz: f64) -> f64 {
    let mut sine = 0.0;
    let mut cosine = 0.0;
    for (index, sample) in audio.iter().skip(channel).step_by(2).enumerate() {
        let (s, c) = (TAU * hz * index as f64 / f64::from(rate)).sin_cos();
        sine += f64::from(*sample) * s;
        cosine += f64::from(*sample) * c;
    }
    sine.hypot(cosine) * 4.0 / audio.len() as f64
}

#[test]
fn opposite_stereo_sidebands_translate_the_same_input_in_hertz() {
    for rate in [44_100, 48_000] {
        let mut audio = Vec::with_capacity(rate as usize);
        for n in 0..rate / 2 {
            let sample = (TAU * 4_000.0 * f64::from(n) / f64::from(rate)).sin() as f32;
            audio.extend_from_slice(&[sample, sample]);
        }
        let mut effect = FrequencyShifter::default();
        effect.set_param("shift", 600.0);
        effect.set_param("sideband_right", -100.0);
        effect.process(&mut audio, rate);
        let stable = &audio[(rate as usize / 5)..];
        for (channel, wanted, rejected) in [(0, 4_600.0, 3_400.0), (1, 3_400.0, 4_600.0)] {
            assert!(magnitude(stable, channel, rate, wanted) > 0.95);
            assert!(magnitude(stable, channel, rate, rejected) < 0.04);
            assert!(magnitude(stable, channel, rate, 4_000.0) < 0.005);
        }
    }
}

#[test]
fn reported_latency_aligns_the_dry_blend_and_reset_restores_the_same_history() {
    let mut effect = FrequencyShifter::default();
    let mut impulse = [0.0; 256];
    impulse[0] = 0.75;
    impulse[1] = -0.25;
    let mut result = impulse;
    effect.set_param("mix", 35.0);
    effect.process(&mut result, 48_000);
    assert_eq!(effect.latency_samples() as usize, QUADRATURE_LATENCY);
    let offset = QUADRATURE_LATENCY * 2;
    let mut expected = [0.0; 256];
    expected[offset..offset + 2].copy_from_slice(&impulse[..2]);
    assert_eq!(result, expected);

    effect.set_param("shift", -300.0);
    effect.set_param("feedback", 35.0);
    effect.set_param("stereo_phase", 12.5);
    let count = oc_test_support::operations(|| {
        effect.reset();
        effect.process(&mut impulse, 48_000);
        effect.set_param("mix", 25.0);
    });
    assert_eq!(count, 0);
    let mut partitioned = [0.0; 256];
    partitioned[..2].copy_from_slice(&[0.75, -0.25]);
    effect.set_param("mix", 35.0);
    effect.reset();
    for block in partitioned.chunks_mut(34) {
        effect.process(block, 48_000);
    }
    assert_eq!(partitioned, impulse);
}
