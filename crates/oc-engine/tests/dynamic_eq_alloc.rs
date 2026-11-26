//! The dynamic EQ audio path must not allocate, even with every band active.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::dynamic_eq::{BAND_COUNT, DynamicEq};
use oc_test_support::operations;

#[test]
fn all_bands_and_cuts_do_not_allocate_in_the_audio_path() {
    let mut eq = DynamicEq::new(48_000);
    for band in 1..=BAND_COUNT {
        let prefix = format!("band{band}");
        eq.set_param(&format!("{prefix}_enabled"), 1.0);
        eq.set_param(&format!("{prefix}_type"), 0.0);
        eq.set_param(&format!("{prefix}_freq"), 80.0 * band as f64);
        eq.set_param(&format!("{prefix}_q"), 1.0);
        eq.set_param(&format!("{prefix}_gain"), 3.0);
        eq.set_param(&format!("{prefix}_dynamic"), 1.0);
        eq.set_param(&format!("{prefix}_threshold"), -30.0);
        eq.set_param(&format!("{prefix}_range"), -6.0);
        eq.set_param(&format!("{prefix}_attack"), 5.0);
        eq.set_param(&format!("{prefix}_release"), 100.0);
    }
    eq.set_param("band1_sidechain", 1.0);
    eq.set_param("auto_gain", 1.0);
    eq.set_param("low_cut_enabled", 1.0);
    eq.set_param("low_cut_freq", 30.0);
    eq.set_param("low_cut_slope", 3.0);
    eq.set_param("high_cut_enabled", 1.0);
    eq.set_param("high_cut_freq", 18_000.0);
    eq.set_param("high_cut_slope", 3.0);
    eq.set_param("band7_type", 3.0);
    eq.set_param("band7_slope", 3.0);
    eq.set_param("band7_sidechain", 1.0);

    let mut buffer: Vec<f32> = (0..2048)
        .map(|index| (index as f32 * 0.017).sin() * 0.4)
        .collect();
    let key: Vec<f32> = (0..2048)
        .map(|index| (index as f32 * 0.011).sin() * 0.5)
        .collect();
    eq.process_with_sidechain(&mut buffer, &key, 48_000, None);

    let allocations = operations(|| {
        for _ in 0..64 {
            eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
        }
        eq.set_param("band7_slope", 1.0);
        eq.set_param("output_gain", -6.0);
        eq.reset();
        eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
        eq.set_param("oversample", 1.0);
        for _ in 0..16 {
            eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
        }
        eq.reset();
        eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
        eq.set_param("oversample_fir", 1.0);
        for _ in 0..16 {
            eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
        }
        for (index, (samples, sidechain)) in buffer
            .chunks_exact_mut(2)
            .zip(key.chunks_exact(2))
            .enumerate()
        {
            eq.set_param("band12_freq", 100.0 + index as f64 * 10.0);
            eq.process_with_sidechain(samples, sidechain, 48_000, None);
        }
        eq.reset();
        eq.process_with_sidechain(&mut buffer, &key, 48_000, None);
    });

    assert_eq!(allocations, 0, "dynamic EQ allocated in the audio path");
}
