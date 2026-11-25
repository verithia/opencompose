//! Mix insert output with latency-aligned dry audio after device processing.

use super::{MAX_AUTOMATION_BLOCK_FRAMES, ParamLane, ProjectClock, SlotControl};
use crate::graph::StereoDelayCompensator;
use oc_audio::BlockPreparationError;

const MAX_DRY_DELAY_FRAMES: u32 = 1 << 20;

#[derive(Clone, Copy)]
struct Values {
    wet: f32,
    enabled: bool,
}

impl Values {
    fn update(&mut self, control: SlotControl, value: f64) {
        match control {
            SlotControl::Wet => self.wet = value as f32,
            SlotControl::Enabled => self.enabled = value >= 0.5,
        }
    }

    fn audible_wet(self) -> f32 {
        if self.enabled { self.wet } else { 0.0 }
    }
}

pub(super) struct SlotMix {
    initial: Values,
    current: Values,
    dry: Vec<f32>,
    wet_values: Vec<f32>,
    delay: StereoDelayCompensator,
}

impl SlotMix {
    pub(super) fn new(
        wet: f64,
        enabled: bool,
        latency: u32,
    ) -> Result<Self, BlockPreparationError> {
        let initial = Values {
            wet: wet as f32,
            enabled,
        };
        let mut slot = Self {
            initial,
            current: initial,
            dry: vec![0.0; MAX_AUTOMATION_BLOCK_FRAMES * 2],
            wet_values: vec![0.0; MAX_AUTOMATION_BLOCK_FRAMES],
            delay: StereoDelayCompensator::new(0),
        };
        slot.prepare_latency(latency)?;
        Ok(slot)
    }

    pub(super) fn prepare_latency(&mut self, frames: u32) -> Result<(), BlockPreparationError> {
        if frames > MAX_DRY_DELAY_FRAMES {
            return Err(BlockPreparationError::CapacityExceeded);
        }
        let size = frames as usize * 2;
        if size != self.delay.buffer.len() {
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(size)
                .map_err(|_| BlockPreparationError::CapacityExceeded)?;
            buffer.resize(size, 0.0);
            self.delay = StereoDelayCompensator {
                buffer,
                position: 0,
            };
        }
        Ok(())
    }

    pub(super) fn reset(&mut self) {
        self.current = self.initial;
        self.delay.position = 0;
        self.delay.buffer.fill(0.0);
    }

    pub(super) fn seek(
        &mut self,
        sample: u64,
        lanes: &[ParamLane],
        clock: &ProjectClock,
        rate: u32,
    ) {
        self.reset();
        let Some(previous) = sample.checked_sub(1) else {
            return;
        };
        let mut latest = [None, None];
        for lane in lanes {
            let Some(control) = lane.slot_control else {
                continue;
            };
            let index = if control == SlotControl::Wet { 0 } else { 1 };
            let at = previous.min(
                lane.input
                    .timeline()
                    .and_then(oc_dsl::automation::LaneCurve::end_sample_exclusive)
                    .map_or(previous, |end| end - 1),
            );
            if latest[index].is_some_and(|last| at < last) {
                continue;
            }
            let tempo = lane
                .input
                .timeline()
                .is_some_and(oc_dsl::automation::LaneCurve::lfo_follows_tempo)
                .then(|| clock.sample_tempo_at(at, rate).ok())
                .flatten();
            if let Some(value) = lane.target_at(at, tempo) {
                self.current.update(control, value);
                latest[index] = Some(at);
            }
        }
    }

    pub(super) fn read(&self, control: SlotControl) -> f64 {
        match control {
            SlotControl::Wet => f64::from(self.current.wet),
            SlotControl::Enabled => {
                if self.current.enabled {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    pub(super) fn write(&mut self, control: SlotControl, value: f64) {
        if (0.0..=1.0).contains(&value) {
            self.initial.update(control, value);
            self.current.update(control, value);
        }
    }

    pub(super) fn capture(
        &mut self,
        input: &[f32],
        events: &mut Vec<(u64, usize, f64)>,
        lanes: &[ParamLane],
    ) {
        let dry = &mut self.dry[..input.len()];
        dry.copy_from_slice(input);
        self.delay.process(dry);
        let weights = &mut self.wet_values[..input.len() / 2];
        let mut last = 0;
        events.retain(|(offset, index, value)| {
            let Some(control) = lanes[*index].slot_control else {
                return true;
            };
            let offset = *offset as usize;
            weights[last..offset].fill(self.current.audible_wet());
            last = offset;
            self.current.update(control, *value);
            false
        });
        weights[last..].fill(self.current.audible_wet());
    }

    pub(super) fn blend(&self, buffer: &mut [f32]) {
        for ((frame, dry), &wet) in buffer
            .chunks_exact_mut(2)
            .zip(self.dry.chunks_exact(2))
            .zip(&self.wet_values)
        {
            if wet == 1.0 {
                continue;
            }
            for (sample, dry) in frame.iter_mut().zip(dry) {
                *sample = if wet == 0.0 {
                    *dry
                } else {
                    *dry * (1.0 - wet) + *sample * wet
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::effect_automation::AutomatedEffect;
    use oc_audio::{AudioEffect, PluginTransport};
    use std::sync::Arc;

    struct LatentGain(StereoDelayCompensator);

    impl AudioEffect for LatentGain {
        fn process(&mut self, buffer: &mut [f32], _: u32) {
            self.0.process(buffer);
            for sample in buffer {
                *sample *= 2.0;
            }
        }
        fn latency_samples(&self) -> u32 {
            3
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {
            self.0.buffer.fill(0.0);
            self.0.position = 0;
        }
        fn name(&self) -> &str {
            "latent gain"
        }
    }

    #[test]
    fn insert_mix_dry_signal_shares_the_device_latency_on_both_channels() {
        let clock = Arc::new(ProjectClock::new(&Default::default()).unwrap());
        for (wet, enabled, expected) in [(0.5, true, 1.5), (1.0, false, 1.0)] {
            let inner = Box::new(LatentGain(StereoDelayCompensator::new(3)));
            let mut mix = AutomatedEffect::new(inner, Vec::new(), 48_000, None, clock.clone())
                .unwrap()
                .with_slot_mix(wet, enabled)
                .unwrap();
            assert_eq!(mix.latency_samples(), 3);
            mix.prepare_block(5).unwrap();
            let mut impulse = [1.0, -0.5, 0.0, 0.0];
            mix.process(&mut impulse, 48_000);
            assert_eq!(impulse, [0.0; 4]);
            let mut out = [0.0; 10];
            mix.process(&mut out, 48_000);
            assert_eq!(
                out,
                [
                    0.0,
                    0.0,
                    expected,
                    -0.5 * expected,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0
                ]
            );
            let mut pending = [1.0, -0.5];
            mix.process(&mut pending, 48_000);
            mix.reset();
            let mut out = [0.0; 10];
            mix.process(&mut out, 48_000);
            assert_eq!(out, [0.0; 10]);
        }
    }

    #[test]
    fn insert_mix_seeking_after_a_finished_clip_uses_its_latched_value() {
        let project = oc_dsl::parse("@project\n  tempo: 120\n@bus mix\n  effects:\n    - gain: factor=2\n  automation: fx1.slot/wet [1:2 - 1:3] {1:2=0.75}\n")
            .unwrap();
        let bus = &project.buses[0];
        let compiled = super::super::compile(
            &bus.effects[0],
            &[&bus.automation[0]],
            &project.properties,
            48_000,
            "bus:mix",
            0,
        )
        .unwrap();
        let inner = Box::new(LatentGain(StereoDelayCompensator::new(3)));
        let mut mix = AutomatedEffect::new(inner, compiled.lanes, 48_000, None, compiled.clock)
            .unwrap()
            .with_slot_mix(0.0, true)
            .unwrap();
        let mut buffer = [1.0; 20];
        mix.process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(50_000, 48_000, 120.0)),
        );
        assert_eq!(&buffer[6..], &[1.75; 14]);
        let mut buffer = [1.0; 20];
        mix.reset();
        mix.process_with_transport(
            &mut buffer,
            48_000,
            Some(PluginTransport::new(0, 48_000, 120.0)),
        );
        assert_eq!(&buffer[6..], &[1.0; 14]);
    }
}
