//! Spectrum analysis must not allocate once constructed.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_engine::spectrum::{SPECTRUM_HOP, SpectrumAnalyser};
use oc_test_support::operations;

#[test]
fn analyser_push_does_not_allocate() {
    let mut analyser = SpectrumAnalyser::new(48_000);
    let buffer: Vec<f32> = (0..SPECTRUM_HOP * 2)
        .map(|index| (index as f32 * 0.031).sin() * 0.4)
        .collect();
    analyser.push_interleaved(&buffer);

    let allocations = operations(|| {
        for _ in 0..64 {
            analyser.push_interleaved(&buffer);
        }
    });

    assert_eq!(
        allocations, 0,
        "spectrum analyser allocated in the render path"
    );
}
