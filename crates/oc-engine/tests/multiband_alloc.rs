//! The multiband audio path must not allocate in either phase mode.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::multiband::MultibandCompressor;
use oc_test_support::operations;

#[test]
fn four_bands_do_not_allocate_in_minimum_or_linear_mode() {
    for phase in [0.0, 2.0] {
        let mut effect = MultibandCompressor::new(48_000);
        effect.set_param("band_count", 2.0);
        effect.set_param("phase_mode", phase);
        effect.set_param("slope", 4.0);
        effect.set_param("crossover_1", 200.0);
        effect.set_param("crossover_2", 1_500.0);
        effect.set_param("crossover_3", 8_000.0);
        effect.set_param("lookahead", 5.0);
        for band in 1..=4 {
            let prefix = format!("band{band}");
            effect.set_param(&format!("{prefix}_enabled"), 1.0);
            effect.set_param(&format!("{prefix}_threshold"), -30.0);
            effect.set_param(&format!("{prefix}_range"), -12.0);
            effect.set_param(&format!("{prefix}_ratio"), 6.0);
            effect.set_param(&format!("{prefix}_attack"), 2.0);
            effect.set_param(&format!("{prefix}_release"), 80.0);
            effect.set_param(&format!("{prefix}_knee"), 6.0);
            effect.set_param(&format!("{prefix}_makeup"), 0.0);
            effect.set_param(&format!("{prefix}_lookahead"), 5.0);
            effect.set_param(&format!("{prefix}_solo"), 1.0);
            effect.set_param(&format!("{prefix}_mute"), 0.0);
        }

        let mut buffer: Vec<f32> = (0..4096)
            .map(|index| (index as f32 * 0.017).sin() * 0.4)
            .collect();
        effect.process(&mut buffer, 48_000);

        let allocations = operations(|| {
            for _ in 0..64 {
                effect.process(&mut buffer, 48_000);
            }
        });

        assert_eq!(allocations, 0, "multiband allocated in phase mode {phase}");
    }
}
