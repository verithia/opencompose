//! The dynamic EQ audio path must not allocate, even with every band active.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::dynamic_eq::DynamicEq;
use oc_test_support::operations;

#[test]
fn six_bands_and_cuts_do_not_allocate_in_the_audio_path() {
    let mut eq = DynamicEq::new(48_000);
    for band in 1..=6 {
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
    eq.set_param("high_cut_enabled", 1.0);
    eq.set_param("high_cut_freq", 18_000.0);

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
    });

    assert_eq!(allocations, 0, "dynamic EQ allocated in the audio path");
}
