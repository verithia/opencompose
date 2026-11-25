use oc_audio::AudioEffect;
use oc_device_schema::transfer_curve::{CurveDomain, TransferCurve};
use oc_dsp::transfer_shaper::TransferShaper;

#[test]
fn curve_knots_gains_stereo_and_reset_are_exact_without_allocations() {
    let curve = TransferCurve {
        version: 1,
        domain: CurveDomain::Odd,
        samples: vec![0.0, 0.8, 1.0],
    };
    let mut fx = TransferShaper::new(curve).unwrap();
    fx.set_param("dry_gain", 0.25);
    fx.set_param("wet_gain", 0.5);
    fx.set_param("pre_gain", 2.0);
    let mut audio = [0.25, -0.5, 2.0, 0.0];
    fx.process(&mut audio, 48_000);
    assert_eq!(audio, [0.4625, -0.625, 1.0, 0.0]);
    fx.set_param("pre_gain", f64::NAN);
    fx.set_param("pre_gain", 17.0);
    assert_eq!(fx.get_param("pre_gain"), Some(2.0));
    let mut a = vec![0.0; 8192];
    for (i, v) in a.iter_mut().enumerate() {
        *v = (i as f32 * 0.1).sin() * 0.6;
    }
    let mut b = a.clone();
    fx.process(&mut a, 8_000);
    fx.reset();
    for block in b.chunks_mut(74) {
        fx.process(block, 8_000);
    }
    assert_eq!(a, b);
    assert_eq!(fx.latency_samples(), 0);
    assert_eq!(fx.tail_samples(), 0);
    let mut buffer = [0.25; 128];
    let operations = oc_test_support::operations(|| {
        fx.set_param("wet_gain", 0.3);
        fx.process(&mut buffer, 48_000);
        fx.reset();
    });
    assert_eq!(operations, 0);
}

#[test]
fn bipolar_curve_keeps_asymmetry_and_known_linear_segments() {
    let curve = TransferCurve {
        version: 1,
        domain: CurveDomain::Bipolar,
        samples: vec![-0.5, 0.0, 1.0],
    };
    let mut fx = TransferShaper::new(curve).unwrap();
    let mut audio = [-2.0, -0.5, 0.5, 2.0];
    fx.process(&mut audio, 44_100);
    assert_eq!(audio, [-0.5, -0.25, 0.5, 1.0]);
}
