//! Spectral response, channel independence and callback invariants.

use std::f64::consts::TAU;

use oc_dsp::{AudioEffect, vocoder::Vocoder};
use oc_test_support::operations;

const RATE: u32 = 48_000;

fn tone(frame: usize, frequency: f64) -> f64 {
    (TAU * frequency * frame as f64 / f64::from(RATE)).sin()
}

fn carrier() -> Vec<f32> {
    (0..RATE as usize / 4)
        .flat_map(|frame| {
            let left = 0.2 * (tone(frame, 400.0) + tone(frame, 2_400.0));
            [left as f32, 0.0]
        })
        .collect()
}

fn key(frequency: f64) -> Vec<f32> {
    (0..RATE as usize / 4)
        .flat_map(|frame| [tone(frame, frequency) as f32 * 0.4; 2])
        .collect()
}

fn amplitude(audio: &[f32], frequency: f64) -> f64 {
    let (real, imaginary) = audio
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .skip(RATE as usize / 8)
        .fold((0.0, 0.0), |(real, imaginary), (frame, data)| {
            let phase = TAU * frequency * frame as f64 / f64::from(RATE);
            let sample = f64::from(data[0]);
            (
                real + sample * phase.cos(),
                imaginary + sample * phase.sin(),
            )
        });
    real.hypot(imaginary)
}

#[test]
fn vocoder_follows_the_modulator_spectrum_without_leaking_to_the_other_channel() {
    let mut low = carrier();
    Vocoder::new(RATE).process_with_sidechain(&mut low, &key(400.0), RATE, None);
    let mut high = carrier();
    Vocoder::new(RATE).process_with_sidechain(&mut high, &key(2_400.0), RATE, None);
    let mut inverted = key(400.0);
    for right in inverted.iter_mut().skip(1).step_by(2) {
        *right = -*right;
    }
    let mut anti_phase = carrier();
    Vocoder::new(RATE).process_with_sidechain(&mut anti_phase, &inverted, RATE, None);

    assert_eq!(low, anti_phase);
    assert!(amplitude(&low, 400.0) > 8.0 * amplitude(&low, 2_400.0));
    assert!(amplitude(&high, 2_400.0) > 8.0 * amplitude(&high, 400.0));
    assert!(low.iter().chain(&high).all(|value| value.is_finite()));
    assert!(low.iter().skip(1).step_by(2).all(|sample| *sample == 0.0));
    assert!(high.iter().skip(1).step_by(2).all(|sample| *sample == 0.0));
}

#[test]
fn vocoder_self_input_reset_and_block_partitions_are_repeatable() {
    let original = carrier();
    let mut entire = original.clone();
    let mut blocks = original.clone();
    let mut effect = Vocoder::new(RATE);
    effect.process(&mut entire, RATE);
    assert_ne!(entire, original);
    assert!(entire.iter().any(|sample| sample.abs() > 1e-3));
    effect.reset();
    for block in blocks.chunks_mut(74) {
        effect.process(block, RATE);
    }
    assert_eq!(blocks, entire);
}

#[test]
fn vocoder_routed_silence_does_not_fall_back_and_updates_allocate_nothing() {
    let original = carrier();
    let mut audio = original.clone();
    let mut effect = Vocoder::new(RATE);
    let allocations = operations(|| {
        effect.set_param("bands", 48.0);
        effect.set_param("attack", 15.0);
        effect.set_param("release", 100.0);
        effect.set_param("output_gain", 3.0);
        effect.process_with_sidechain(&mut audio, &[], RATE, None);
        effect.reset();
    });
    assert_eq!(allocations, 0);
    assert!(audio.iter().all(|sample| *sample == 0.0));
    effect.set_param("mix", 0.0);
    audio.copy_from_slice(&original);
    effect.process(&mut audio, RATE);
    assert_eq!(audio, original);
}
