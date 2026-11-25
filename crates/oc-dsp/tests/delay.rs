use oc_audio::{AudioEffect, AudioProcessContext, AudioProcessInput, PluginTransport};
use oc_device_schema::delay::{DelayRouting, FeedbackFilter};
use oc_dsp::delay::StereoDelay;
use std::f64::consts::TAU;

const RATE: u32 = 1_000;

#[test]
fn delay_routing_controls_first_echo_and_feedback_independently() {
    for (routing, expected) in [
        (
            DelayRouting::Stereo,
            [[0.5, 0.0], [0.25, 0.0], [0.125, 0.0]],
        ),
        (
            DelayRouting::Mono,
            [[0.25, 0.25], [0.125, 0.125], [0.0625, 0.0625]],
        ),
        (
            DelayRouting::Swapped,
            [[0.0, 0.5], [0.0, 0.25], [0.0, 0.125]],
        ),
        (
            DelayRouting::PingPong,
            [[0.5, 0.0], [0.0, 0.25], [0.125, 0.0]],
        ),
    ] {
        let mut fx = StereoDelay::new(RATE);
        for (name, value) in [
            ("time_l", 10.0),
            ("time_r", 10.0),
            ("feedback", 0.5),
            ("mix", 0.5),
            ("dry_gain", 150.0),
            ("wet_gain", 200.0),
            ("routing", routing.code()),
        ] {
            fx.set_param(name, value);
        }
        let mut pcm = [0.0; 62];
        pcm[0] = 0.5;
        fx.process(&mut pcm, RATE);
        assert_eq!(&pcm[..2], &[0.375, 0.0]);
        for (index, frame) in pcm.chunks_exact(2).enumerate().skip(1) {
            let expected = if index % 10 == 0 {
                expected[index / 10 - 1]
            } else {
                [0.0; 2]
            };
            assert_eq!(frame, expected, "routing={routing:?} frame={index}");
        }
    }
}

#[test]
fn delay_tempo_and_reset_reuse_storage_with_fractional_taps() {
    let mut fx = StereoDelay::new(RATE);
    for (name, value) in [
        ("mix", 1.0),
        ("feedback", 0.0),
        ("tempo_sync", 1.0),
        ("interpolate", 1.0),
        ("quarters_l", 0.25),
        ("quarters_r", 0.25),
    ] {
        fx.set_param(name, value);
    }
    let mut pcm = [0.0; 254];
    for (tempo, at, taps) in [(120.0, 125, [1.0, 0.0]), (240.0, 62, [0.5, 0.5])] {
        let context = PluginTransport::new(0, RATE, tempo);
        pcm.fill(0.0);
        pcm[0] = 1.0;
        assert_eq!(
            oc_test_support::operations(|| {
                fx.reset();
                fx.process_with_context(
                    &mut pcm,
                    AudioProcessContext {
                        sample_rate: RATE,
                        transport: Some(&context),
                        input: AudioProcessInput::Main,
                    },
                );
            }),
            0
        );
        assert_eq!(pcm[at * 2], taps[0]);
        assert_eq!(pcm[(at + 1) * 2], taps[1]);
        assert_eq!(pcm.iter().sum::<f32>(), 1.0);
    }
}

#[test]
fn delay_feedback_filter_changes_repeats_and_clears_on_reset() {
    const SR: u32 = 8_000;
    const TAP: usize = 400;
    const TONE: usize = 240;
    for (filter, pass, stop) in [
        (FeedbackFilter::LowPass, 200.0, 2_400.0),
        (FeedbackFilter::HighPass, 2_400.0, 200.0),
        (FeedbackFilter::BandPass, 800.0, 2_400.0),
    ] {
        let measure = |frequency: f64| {
            let mut fx = StereoDelay::new(SR);
            for (name, value) in [
                ("time_l", 50.0),
                ("time_r", 50.0),
                ("feedback", 0.5),
                ("mix", 1.0),
                ("feedback_cutoff", 800.0),
                ("feedback_filter", filter.code()),
            ] {
                fx.set_param(name, value);
            }
            let mut pcm = [0.0; (2 * TAP + TONE) * 2];
            let source = std::array::from_fn::<_, TONE, _>(|i| {
                (TAU * frequency * i as f64 / f64::from(SR)).sin() as f32
            });
            for (i, sample) in source.iter().enumerate() {
                pcm[2 * i] = *sample;
            }
            assert_eq!(oc_test_support::operations(|| fx.process(&mut pcm, SR)), 0);
            for (i, sample) in source.iter().enumerate() {
                assert_eq!(pcm[2 * (TAP + i)], *sample);
            }
            let power = pcm[4 * TAP + 2 * 64..]
                .iter()
                .map(|sample| sample * sample)
                .sum::<f32>();
            pcm.fill(0.0);
            fx.reset();
            fx.process(&mut pcm, SR);
            assert!(pcm.iter().all(|sample| *sample == 0.0));
            power
        };
        assert!(measure(pass) > 8.0 * measure(stop), "{filter:?}");
    }
}
