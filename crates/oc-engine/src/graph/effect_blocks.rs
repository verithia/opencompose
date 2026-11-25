//! Every effect call respects the device's prepared frame capacity.

use super::{AudioEffect, AudioProcessContext, AudioProcessInput, PluginTransport};

pub(super) fn process_effect_context(
    effect: &mut dyn AudioEffect,
    buffer: &mut [f32],
    sample_rate: u32,
    transport: Option<&PluginTransport>,
    input: AudioProcessInput<'_>,
) {
    let frames = (effect.max_block_frames().max(1) as usize).min(usize::MAX / 2);
    if buffer.len() <= frames * 2 {
        effect.process_with_context(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport,
                input,
            },
        );
        return;
    }
    for (index, block) in buffer.chunks_mut(frames * 2).enumerate() {
        let offset = index * frames;
        let end = offset + block.len() / 2;
        let position = transport.map(|base| base.at_sample(base.sample_pos + offset as u64));
        let input = match input {
            AudioProcessInput::Main => AudioProcessInput::Main,
            AudioProcessInput::Sidechain(key) => {
                AudioProcessInput::Sidechain(key.get(offset * 2..end * 2).unwrap_or(&[]))
            }
            AudioProcessInput::Control(control) => {
                AudioProcessInput::Control(control.get(offset..end).unwrap_or(&[]))
            }
        };
        effect.process_with_context(
            block,
            AudioProcessContext {
                sample_rate,
                transport: position.as_ref(),
                input,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LimitedEffect {
        calls: usize,
        offset: usize,
        start: Option<u64>,
    }
    impl AudioEffect for LimitedEffect {
        fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
            assert!(buffer.len() <= 6);
            assert_eq!(
                context.transport.map(|t| t.sample_pos),
                self.start.map(|start| start + self.offset as u64)
            );
            if let Some(transport) = context.transport.filter(|base| base.has_clock()) {
                let expected = if transport.sample_pos < 48_000 {
                    120.0
                } else {
                    60.0
                };
                assert_eq!(transport.current_tempo(), expected);
                assert_eq!(
                    transport
                        .at_sample(transport.sample_pos + buffer.len() as u64 / 2 - 1)
                        .current_tempo(),
                    expected
                );
            }
            for (i, frame) in buffer.chunks_exact_mut(2).enumerate() {
                let expected = (self.offset + i) as f32;
                assert_eq!(frame, [expected, expected]);
                match context.input {
                    AudioProcessInput::Main => {}
                    AudioProcessInput::Sidechain(key) => {
                        assert_eq!(&key[i * 2..i * 2 + 2], &[expected, expected])
                    }
                    AudioProcessInput::Control(control) => assert_eq!(control[i], expected),
                }
                frame.fill(expected + 1.0);
            }
            self.offset += buffer.len() / 2;
            self.calls += 1;
        }
        fn process(&mut self, _: &mut [f32], _: u32) {
            unreachable!()
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn latency_samples(&self) -> u32 {
            0
        }
        fn name(&self) -> &str {
            "limited-effect"
        }
        fn max_block_frames(&self) -> u32 {
            3
        }
    }

    #[test]
    fn finite_capacity_splits_every_input_without_allocating_or_losing_frames() {
        let original: [f32; 22] = std::array::from_fn(|i| (i / 2) as f32);
        let control: [f32; 11] = std::array::from_fn(|i| i as f32);
        let clock = std::sync::Arc::new(
            oc_dsl::timing::ProjectClock::new(
                &oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n")
                    .unwrap()
                    .properties,
            )
            .unwrap(),
        );
        let plain = PluginTransport::new(117, 48_000, 120.0);
        let mapped = plain.clone().with_clock(clock);
        let crossing = mapped.at_sample(47_995);
        for transport in [None, Some(&plain), Some(&mapped), Some(&crossing)] {
            for input in [
                AudioProcessInput::Main,
                AudioProcessInput::Sidechain(&original),
                AudioProcessInput::Control(&control),
            ] {
                for wrapper in 0..3 {
                    let effect = LimitedEffect {
                        calls: 0,
                        offset: 0,
                        start: transport.map(|base| base.sample_pos),
                    };
                    let effect: Box<dyn AudioEffect> = match wrapper {
                        0 => Box::new(effect),
                        1 => Box::new(super::super::SidechainEffect::new(
                            Box::new(effect),
                            "detector".into(),
                        )),
                        _ => Box::new(super::super::ControlEffect::new(
                            Box::new(effect),
                            "control".into(),
                        )),
                    };
                    assert_eq!(effect.max_block_frames(), 3);
                    let mut chain = super::super::EffectChain {
                        effects: vec![effect],
                        ramps: Vec::new(),
                        boundary: Default::default(),
                    };
                    chain.prepare_block(11).unwrap();
                    let mut output = original;
                    assert_eq!(
                        crate::test_alloc::operations(|| {
                            chain
                                .try_process_with_context(
                                    &mut output,
                                    AudioProcessContext {
                                        sample_rate: 48_000,
                                        transport,
                                        input,
                                    },
                                )
                                .unwrap();
                        }),
                        0
                    );
                    assert_eq!(output, original.map(|value| value + 1.0));
                }
            }
        }
    }
}
