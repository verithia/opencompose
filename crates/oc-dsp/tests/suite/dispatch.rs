//! Dispatch contract: a forced backend is honoured only when the CPU can run it,
//! and every accepted backend reproduces the scalar result bit for bit.
//!
//! Kept in its own test binary because the selected level is process-wide.

use oc_dsp::simd::{self, SimdLevel};

fn test_buffer(len: usize) -> Vec<f32> {
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    (0..len)
        .map(|index| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
            match index % 11 {
                0 => 0.0,
                1 => -0.0,
                2 => f32::from_bits(0x0000_0001),
                3 => f32::from_bits(0x8000_0001),
                _ => (unit * 2.0 - 1.0) as f32,
            }
        })
        .collect()
}

fn assert_same_bits(expected: &[f32], actual: &[f32], context: &str) {
    assert_eq!(expected.len(), actual.len(), "{context}");
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        assert_eq!(
            expected.to_bits(),
            actual.to_bits(),
            "{context} differs at {index}: {expected} vs {actual}"
        );
    }
}

/// Run every public kernel once and compare against the scalar reference.
fn assert_kernels_match_scalar() {
    let source = test_buffer(1000);
    let target = test_buffer(1003);

    let mut expected = source.clone();
    simd::channel_gains(&mut expected, 0.25, -1.5);
    let mut reference = source.clone();
    for (index, sample) in reference.iter_mut().enumerate() {
        *sample *= if index % 2 == 0 { 0.25 } else { -1.5 };
    }
    assert_same_bits(&reference, &expected, "channel_gains");

    let mut expected = target.clone();
    simd::accumulate_channel_gains(&mut expected, &source, 0.75, -0.125);
    let mut reference = target.clone();
    for (index, (target, source)) in reference.iter_mut().zip(&source).enumerate() {
        *target += *source * if index % 2 == 0 { 0.75 } else { -0.125 };
    }
    assert_same_bits(
        &reference[..source.len()],
        &expected[..source.len()],
        "accumulate_channel_gains",
    );

    let mut expected = source.clone();
    simd::apply_uniform_gain(&mut expected, 0.333);
    let mut reference = source.clone();
    for sample in &mut reference {
        *sample *= 0.333;
    }
    assert_same_bits(&reference, &expected, "apply_uniform_gain");

    let mut expected = source.clone();
    simd::negate(&mut expected);
    let reference: Vec<f32> = source.iter().map(|sample| -*sample).collect();
    assert_same_bits(&reference, &expected, "negate");

    let mut expected = source.clone();
    simd::apply_mid_side_width(&mut expected, 0.6);
    let mut reference = source.clone();
    for frame in reference.chunks_exact_mut(2) {
        let left = frame[0] as f64;
        let right = frame[1] as f64;
        let mid = (left + right) * 0.5;
        let side = (left - right) * 0.5;
        frame[0] = (mid + side * 0.6) as f32;
        frame[1] = (mid - side * 0.6) as f32;
    }
    assert_same_bits(&reference, &expected, "apply_mid_side_width");
}

#[test]
fn forced_levels_are_honoured_or_refused() {
    assert_eq!(simd::level(), SimdLevel::detect());
    let available: Vec<&str> = [
        SimdLevel::Scalar,
        SimdLevel::Sse2,
        SimdLevel::Avx2,
        SimdLevel::Neon,
    ]
    .into_iter()
    .filter(|level| simd::available(*level))
    .map(|level| level.name())
    .collect();
    println!(
        "selected {} (detected {}); available {available:?}",
        simd::level().name(),
        SimdLevel::detect().name()
    );

    assert!(simd::select(SimdLevel::Scalar));
    assert_eq!(simd::level(), SimdLevel::Scalar);
    assert_kernels_match_scalar();

    for level in [SimdLevel::Sse2, SimdLevel::Avx2, SimdLevel::Neon] {
        let accepted = simd::select(level);
        assert_eq!(
            accepted,
            simd::available(level),
            "{} availability disagrees with selection",
            level.name()
        );
        if accepted {
            assert_eq!(simd::level(), level, "{} was not selected", level.name());
            assert_kernels_match_scalar();
        }
    }

    let detected = SimdLevel::detect();
    assert!(simd::select(detected));
    assert_eq!(simd::level(), detected);
}
