//! The FDN reverb audio path must not allocate, including with Character active.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::reverb_fdn::ReverbFdn;
use oc_test_support::operations;

#[test]
fn process_does_not_allocate_with_every_control_active() {
    let mut effect = ReverbFdn::new(48_000);
    effect.set_param("space", 75.0);
    effect.set_param("decay_rate", 140.0);
    effect.set_param("brightness", 30.0);
    effect.set_param("character", 80.0);
    effect.set_param("distance", 20.0);
    effect.set_param("predelay", 25.0);
    effect.set_param("stereo_width", 110.0);
    effect.set_param("mix", 100.0);

    let mut buffer: Vec<f32> = (0..2048)
        .map(|index| (index as f32 * 0.017).sin() * 0.4)
        .collect();
    effect.process(&mut buffer, 48_000);

    let allocations = operations(|| {
        for _ in 0..64 {
            effect.process(&mut buffer, 48_000);
        }
    });

    assert_eq!(allocations, 0, "FDN reverb allocated in the audio path");
}
