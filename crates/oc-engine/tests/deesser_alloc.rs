//! The de-esser must not allocate while estimating or processing its running band.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::dsp::AudioEffect;
use oc_engine::dsp::deesser::DeEsser;
use oc_test_support::operations;

fn program(frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        let t = frame as f64 / 48_000.0;
        let body = (std::f64::consts::TAU * 1_000.0 * t).sin() * 0.1;
        let sibilant = if (frame / 4_800) % 2 == 0 {
            (std::f64::consts::TAU * 8_000.0 * t).sin() * 0.08
        } else {
            0.0
        };
        let value = (body + sibilant) as f32;
        buffer[frame * 2] = value;
        buffer[frame * 2 + 1] = value;
    }
    buffer
}

#[test]
fn automatic_band_and_segments_do_not_allocate_in_the_audio_path() {
    let mut de_esser = DeEsser::new(48_000);
    de_esser.set_param("band_estimation", 1.0);
    de_esser.set_param("segment_detection", 1.0);
    de_esser.set_param("band_processing", 1.0);
    de_esser.set_param("threshold_mode", 1.0);
    de_esser.set_param("range", 24.0);

    // Warm up past the first band lock so the retune happens before counting.
    let mut buffer = program(24_000);
    for _ in 0..6 {
        de_esser.process(&mut buffer, 48_000);
    }
    assert!(de_esser.detected_band_hz().is_some(), "estimator locked");

    let allocations = operations(|| {
        for _ in 0..16 {
            de_esser.process(&mut buffer, 48_000);
        }
    });

    assert_eq!(allocations, 0, "de-esser allocated in the audio path");
}
