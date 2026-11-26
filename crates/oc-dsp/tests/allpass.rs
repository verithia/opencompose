//! Causal phase response, independent channels and allocation-free state changes.
use oc_dsp::{AudioEffect, allpass::AllPass};

#[test]
fn allpass_has_unity_magnitude_analytic_phase_and_independent_stereo_state() {
    for coefficient in [-0.999, -0.8, 0.0, 0.6, 0.999] {
        let mut effect = AllPass::default();
        effect.set_param("coefficient", coefficient);
        let mut impulse = vec![0.0f32; 65_536 * 2];
        impulse[0] = 1.0;
        effect.process(&mut impulse, 48_000);
        assert!(impulse.iter().skip(1).step_by(2).all(|x| *x == 0.0));
        let energy: f64 = impulse
            .iter()
            .step_by(2)
            .map(|x| f64::from(*x).powi(2))
            .sum();
        assert!((energy - 1.0).abs() < 2e-6);
        for frequency in [0.03_f64, 0.5, 2.8] {
            let mut measured = [0.0; 2];
            for (n, x) in impulse.iter().step_by(2).enumerate() {
                measured[0] += f64::from(*x) * (frequency * n as f64).cos();
                measured[1] -= f64::from(*x) * (frequency * n as f64).sin();
            }
            let c = frequency.cos();
            let s = -frequency.sin();
            let denominator = 1.0 + 2.0 * coefficient * c + coefficient * coefficient;
            let real =
                ((coefficient + c) * (1.0 + coefficient * c) + coefficient * s * s) / denominator;
            let imag =
                (s * (1.0 + coefficient * c) - (coefficient + c) * coefficient * s) / denominator;
            assert!((measured[0] - real).abs() < 1e-6);
            assert!((measured[1] - imag).abs() < 1e-6);
        }
        if coefficient == 0.0 {
            assert_eq!(&impulse[..6], &[0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
            assert_eq!(effect.tail_samples(), 1);
        }
        assert_eq!(effect.latency_samples(), 0);
        let tail = effect.tail_samples() as usize;
        assert!(tail >= 1);
        assert!(impulse[(tail + 1) * 2..].iter().all(|x| x.abs() < 1e-6));
        effect.reset();
        let mut silence = [0.0; 64];
        effect.process(&mut silence, 8_000);
        assert_eq!(silence, [0.0; 64]);
    }
}

#[test]
fn allpass_rejects_invalid_controls_and_rt_updates_reset_allocate_nothing() {
    let mut effect = AllPass::default();
    effect.set_param("coefficient", 0.4);
    for invalid in [f64::NAN, f64::INFINITY, -1.0, 1.0] {
        effect.set_param("coefficient", invalid);
        assert_eq!(effect.get_param("coefficient"), Some(0.4));
    }
    let mut buffer = [0.25; 256];
    let operations = oc_test_support::operations(|| {
        effect.set_param("coefficient", -0.999);
        effect.process(&mut buffer, 8_000);
        effect.reset();
        effect.set_param("coefficient", 0.999);
        effect.process(&mut buffer, 48_000);
        effect.reset();
    });
    assert_eq!(operations, 0);
}
