//! Types shared by the audio graph, the built-in DSP devices and the plugin
//! hosts, plus the output-device owner every stream opens through.

#[cfg(feature = "device")]
pub mod audio_io;
pub mod bounded_file;
pub mod click;
pub mod private_fs;
#[cfg(feature = "device")]
mod stream_owner;
pub mod wav;

pub use oc_audio_types::{
    AudioEffect, AudioProcessContext, AudioProcessInput, BlockPreparationError, PluginTransport,
    ProcessingFailureReport, TransportBlockError, TransportRangeError,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Plugin ABI a hosted plugin speaks.
pub enum PluginFormat {
    Vst3,
    Clap,
    Au,
    Lv2,
    Vst2,
    Internal,
}

/// An interleaved stereo destination must contain complete left/right pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StereoBufferError {
    IncompleteFrame,
}

impl std::fmt::Display for StereoBufferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Stereo output must contain complete left/right sample pairs")
    }
}

impl std::error::Error for StereoBufferError {}

#[cfg(test)]
mod process_context_tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Route {
        Plain,
        Main,
        Sidechain,
        Control,
    }
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Observation {
        route: Route,
        sample_rate: u32,
        samples: usize,
        signal: Option<f32>,
        sample_pos: Option<u64>,
        tempo: Option<f64>,
        meter: Option<(u16, u16)>,
    }
    struct LegacyOverrides {
        calls: [Option<Observation>; 16],
        count: usize,
        rejected: bool,
    }
    impl LegacyOverrides {
        fn new() -> Self {
            Self {
                calls: [None; 16],
                count: 0,
                rejected: false,
            }
        }
        fn record(
            &mut self,
            route: Route,
            buffer: &mut [f32],
            rate: u32,
            transport: Option<&PluginTransport>,
            signal: Option<&[f32]>,
        ) {
            self.calls[self.count] = Some(Observation {
                route,
                sample_rate: rate,
                samples: buffer.len(),
                signal: signal.and_then(|samples| samples.first().copied()),
                sample_pos: transport.map(|t| t.sample_pos),
                tempo: transport.map(PluginTransport::current_tempo),
                meter: transport.map(|t| {
                    let (n, d, _, _) = t.musical_position();
                    (n, d)
                }),
            });
            self.count += 1;
            self.rejected = !buffer.len().is_multiple_of(2);
            if !self.rejected {
                let value = match route {
                    Route::Plain => 1.0,
                    Route::Main => 2.0,
                    Route::Sidechain => 3.0,
                    Route::Control => 4.0,
                };
                buffer.fill(value);
            }
        }
    }
    impl AudioEffect for LegacyOverrides {
        fn process(&mut self, b: &mut [f32], r: u32) {
            self.record(Route::Plain, b, r, None, None);
        }
        fn process_with_transport(&mut self, b: &mut [f32], r: u32, t: Option<PluginTransport>) {
            self.record(Route::Main, b, r, t.as_ref(), None);
        }
        fn process_with_sidechain(
            &mut self,
            b: &mut [f32],
            s: &[f32],
            r: u32,
            t: Option<PluginTransport>,
        ) {
            self.record(Route::Sidechain, b, r, t.as_ref(), Some(s));
        }
        fn process_with_control(
            &mut self,
            b: &mut [f32],
            s: &[f32],
            r: u32,
            t: Option<PluginTransport>,
        ) {
            self.record(Route::Control, b, r, t.as_ref(), Some(s));
        }
        fn processing_failed(&self) -> bool {
            self.rejected
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {
            self.rejected = false;
        }
        fn name(&self) -> &str {
            "legacy overrides"
        }
    }
    struct DefaultEffect {
        calls: usize,
    }
    impl AudioEffect for DefaultEffect {
        fn process(&mut self, buffer: &mut [f32], _: u32) {
            self.calls += 1;
            buffer.fill(0.25);
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "default effect"
        }
    }
    fn mapped_transport() -> PluginTransport {
        let project=oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n  time_sig_map:\n    2:1 = 3/4\n").unwrap();
        PluginTransport::new(96_000, 48_000, 120.0).with_clock(Arc::new(
            oc_dsl::timing::ProjectClock::new(&project.properties).unwrap(),
        ))
    }

    #[test]
    fn borrowed_effect_context_preserves_each_dynamic_legacy_override() {
        let transport = mapped_transport();
        let key = [0.75, 0.5];
        let control = [0.125];
        for timeline in [Some(&transport), None] {
            for input in [
                AudioProcessInput::Main,
                AudioProcessInput::Sidechain(&key),
                AudioProcessInput::Control(&control),
            ] {
                let mut actual = LegacyOverrides::new();
                let mut reference = LegacyOverrides::new();
                let mut output = [9.0; 4];
                let mut expected = output;
                match input {
                    AudioProcessInput::Main => {
                        reference.process_with_transport(&mut expected, 48_000, timeline.cloned())
                    }
                    AudioProcessInput::Sidechain(signal) => reference.process_with_sidechain(
                        &mut expected,
                        signal,
                        48_000,
                        timeline.cloned(),
                    ),
                    AudioProcessInput::Control(signal) => reference.process_with_control(
                        &mut expected,
                        signal,
                        48_000,
                        timeline.cloned(),
                    ),
                }
                let dynamic: &mut dyn AudioEffect = &mut actual;
                assert_eq!(
                    oc_test_support::operations(|| dynamic.process_with_context(
                        &mut output,
                        AudioProcessContext {
                            sample_rate: 48_000,
                            transport: timeline,
                            input
                        }
                    )),
                    0
                );
                assert_eq!(output, expected);
                assert_eq!(actual.calls, reference.calls);
                assert_eq!(actual.count, 1);
                let observed = actual.calls[0].unwrap();
                assert_eq!(observed.sample_pos, timeline.map(|t| t.sample_pos));
                assert_eq!(observed.tempo, timeline.map(|_| 60.0));
                assert_eq!(observed.meter, timeline.map(|_| (3, 4)));
            }
        }
    }

    #[test]
    fn borrowed_effect_context_keeps_sole_caller_clock_through_first_warm_empty_and_refusal() {
        let transport = mapped_transport();
        let weak = Arc::downgrade(transport.clock().unwrap());
        assert_eq!(weak.strong_count(), 1);
        let mut effect = LegacyOverrides::new();
        let dynamic: &mut dyn AudioEffect = &mut effect;
        for samples in [4, 4, 0, 3] {
            let mut buffer = [9.0; 4];
            assert_eq!(
                oc_test_support::operations(|| dynamic.process_with_context(
                    &mut buffer[..samples],
                    AudioProcessContext {
                        sample_rate: 48_000,
                        transport: Some(&transport),
                        input: AudioProcessInput::Main,
                    }
                )),
                0
            );
            assert_eq!(weak.strong_count(), 1, "the adapter retains no clock owner");
            if samples == 3 {
                assert!(dynamic.processing_failed());
                assert_eq!(buffer, [9.0; 4]);
            } else {
                assert!(!dynamic.processing_failed());
            }
        }
        assert_eq!(
            effect.count, 4,
            "empty/rejected input still reaches the legacy override"
        );
        drop(transport);
        assert_eq!(
            weak.strong_count(),
            0,
            "caller retirement alone releases the clock"
        );
    }

    #[test]
    fn borrowed_effect_context_preserves_default_input_fallbacks_and_absent_timeline() {
        let transport = mapped_transport();
        let weak = Arc::downgrade(transport.clock().unwrap());
        let signal = [0.5];
        let mut effect = DefaultEffect { calls: 0 };
        let mut output = [0.0; 4];
        for timeline in [Some(&transport), None] {
            for input in [
                AudioProcessInput::Main,
                AudioProcessInput::Sidechain(&signal),
                AudioProcessInput::Control(&signal),
            ] {
                let dynamic: &mut dyn AudioEffect = &mut effect;
                assert_eq!(
                    oc_test_support::operations(|| dynamic.process_with_context(
                        &mut output,
                        AudioProcessContext {
                            sample_rate: 44_100,
                            transport: timeline,
                            input
                        }
                    )),
                    0
                );
                assert_eq!(output, [0.25; 4]);
                assert_eq!(weak.strong_count(), 1);
            }
        }
        assert_eq!(effect.calls, 6);
        drop(transport);
        assert_eq!(weak.strong_count(), 0);
    }
}
