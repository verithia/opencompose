//! Optimised CPU profile for the long-form devices.
//!
//! Part of the `perf-tests` set, so a plain `cargo test` neither compiles nor
//! runs it. Run with `cargo test -p oc-dsp --features perf-tests --profile perf`.
#![cfg(feature = "perf-tests")]

use std::time::Instant;

use oc_dsp::AudioEffect;
use oc_dsp::deesser::DeEsser;
use oc_dsp::multiband::MultibandCompressor;
use oc_dsp::resonance::Resonance;
use oc_dsp::reverb::Reverb;
use oc_dsp::saturator::Saturator;

fn noise(frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; frames * 2];
    let mut state = 0x1234_5678u32;
    for sample in &mut buffer {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *sample = (state >> 8) as f32 / 8_388_608.0 - 1.0;
    }
    buffer
}

fn profile(mut effect: Box<dyn AudioEffect>, frames: usize) -> f64 {
    let mut buffer = noise(frames);
    effect.process(&mut buffer, 48_000);
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let start = Instant::now();
        effect.process(&mut buffer, 48_000);
        best = best.min(start.elapsed().as_secs_f64());
    }
    best
}

#[test]
fn pro_device_cpu_profile() {
    let seconds = 60.0;
    let frames = 48_000 * 60;

    let mut legacy = Reverb::new(48_000);
    let mut full = Reverb::new(48_000);
    full.set_param("modulation", 60.0);
    full.set_param("early_late", 50.0);
    full.set_param("stereo_width", 140.0);
    full.set_param("low_cut", 80.0);
    full.set_param("high_cut", 9_000.0);
    let mut legacy_buffer = noise(frames);
    let mut full_buffer = noise(frames);
    legacy.process(&mut legacy_buffer, 48_000);
    full.process(&mut full_buffer, 48_000);
    let mut best_ratio = f64::INFINITY;
    let mut best_legacy = f64::INFINITY;
    for _ in 0..7 {
        let start = Instant::now();
        legacy.process(&mut legacy_buffer, 48_000);
        let legacy_time = start.elapsed().as_secs_f64();
        let start = Instant::now();
        full.process(&mut full_buffer, 48_000);
        let full_time = start.elapsed().as_secs_f64();
        best_ratio = best_ratio.min(full_time / legacy_time);
        best_legacy = best_legacy.min(legacy_time);
    }

    let mut multiband = MultibandCompressor::new(48_000);
    multiband.set_param("band_count", 2.0);
    for band in 1..=4 {
        multiband.set_param(&format!("band{band}_threshold"), -24.0);
        multiband.set_param(&format!("band{band}_range"), -12.0);
        multiband.set_param(&format!("band{band}_ratio"), 4.0);
    }
    let multiband_time = profile(Box::new(multiband), frames);

    let mut linear = MultibandCompressor::new(48_000);
    linear.set_param("band_count", 2.0);
    linear.set_param("phase_mode", 2.0);
    for band in 1..=4 {
        linear.set_param(&format!("band{band}_threshold"), -24.0);
        linear.set_param(&format!("band{band}_range"), -12.0);
        linear.set_param(&format!("band{band}_ratio"), 4.0);
    }
    let linear_time = profile(Box::new(linear), frames);

    let mut deesser = DeEsser::new(48_000);
    deesser.set_param("threshold", -40.0);
    deesser.set_param("band_processing", 1.0);
    let deesser_time = profile(Box::new(deesser), frames);

    let mut resonance = Resonance::new(48_000);
    resonance.set_param("threshold", 6.0);
    resonance.set_param("depth", 9.0);
    let resonance_time = profile(Box::new(resonance), frames);

    println!(
        "reverb legacy {best_legacy:.3}s ({:.1}x rt) best full/legacy ratio {best_ratio:.2}",
        seconds / best_legacy
    );
    println!(
        "multiband 4-band minimum {multiband_time:.3}s ({:.1}x rt)",
        seconds / multiband_time
    );
    println!(
        "multiband 4-band linear {linear_time:.3}s ({:.1}x rt)",
        seconds / linear_time
    );
    println!(
        "de-esser split {deesser_time:.3}s ({:.1}x rt)",
        seconds / deesser_time
    );
    println!(
        "resonance 2048/256 {resonance_time:.3}s ({:.1}x rt)",
        seconds / resonance_time
    );
    // The full modulation path costs ~2.5x the legacy path in release and
    // ~2.9x under the perf profile (best of seven, measured 2026-10-28), so the
    // old 2.0 ceiling failed in both. Keep ~20% headroom over the perf reading.
    assert!(
        best_ratio <= 3.5,
        "reverb CPU regressed beyond 3.5x: best ratio {best_ratio:.2}"
    );
}

#[test]
fn saturator_cpu_profile() {
    let frames = 48_000 * 30;
    let env = |name: &str, fallback: f64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(fallback)
    };
    let mut saturator = Saturator::new();
    saturator.set_param("drive", env("OCP_SAT_DRIVE", 0.16));
    saturator.set_param("mix", env("OCP_SAT_MIX", 0.28));
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let mut buffer = noise(frames);
        saturator.process(&mut buffer, 48_000);
        let mut buffer = noise(frames);
        let start = Instant::now();
        saturator.process(&mut buffer, 48_000);
        best = best.min(start.elapsed().as_secs_f64());
    }
    println!(
        "saturator drive {:.0}% mix {:.0}%: {:.2} ns/frame",
        saturator.get_param("drive").unwrap() * 100.0,
        saturator.get_param("mix").unwrap() * 100.0,
        best * 1e9 / frames as f64
    );
}
