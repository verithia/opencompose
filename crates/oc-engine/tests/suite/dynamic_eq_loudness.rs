//! Auto-gain must hold integrated loudness while a dynamic band moves.

use std::f64::consts::TAU;

use oc_engine::analysis::measure_stereo_interleaved;
use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::dynamic_eq::DynamicEq;

const SR: u32 = 48_000;
const TOLERANCE_LU: f64 = 0.5;

fn tone(freq: f64, amplitude: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let value = (TAU * freq * frame as f64 / SR as f64).sin() as f32 * amplitude as f32;
            [value, value]
        })
        .collect()
}

fn dynamic_band(auto_gain: bool) -> DynamicEq {
    let mut eq = DynamicEq::new(SR);
    eq.set_param("band1_enabled", 1.0);
    eq.set_param("band1_type", 0.0);
    eq.set_param("band1_freq", 1_000.0);
    eq.set_param("band1_q", 1.0);
    eq.set_param("band1_gain", 0.0);
    eq.set_param("band1_dynamic", 1.0);
    eq.set_param("band1_threshold", -40.0);
    eq.set_param("band1_range", -12.0);
    eq.set_param("band1_attack", 5.0);
    eq.set_param("band1_release", 100.0);
    eq.set_param("auto_gain", if auto_gain { 1.0 } else { 0.0 });
    eq
}

fn integrated_lufs(samples: &[f32]) -> f64 {
    measure_stereo_interleaved(samples, SR)
        .integrated_lufs
        .expect("integrated loudness")
}

#[test]
fn auto_gain_keeps_integrated_loudness_within_half_a_lu() {
    let input = tone(1_000.0, 0.25, SR as usize * 8);

    let mut compensated = input.clone();
    dynamic_band(true).process(&mut compensated, SR);
    let dry_lufs = integrated_lufs(&input);
    let compensated_lufs = integrated_lufs(&compensated);
    assert!(
        (compensated_lufs - dry_lufs).abs() <= TOLERANCE_LU,
        "auto-gain moved loudness: {dry_lufs:.3} -> {compensated_lufs:.3} LUFS"
    );

    let mut uncompensated = input.clone();
    dynamic_band(false).process(&mut uncompensated, SR);
    let uncompensated_lufs = integrated_lufs(&uncompensated);
    assert!(
        uncompensated_lufs < dry_lufs - 8.0,
        "dynamic band did not move without auto-gain: {uncompensated_lufs:.3} LUFS"
    );
}
