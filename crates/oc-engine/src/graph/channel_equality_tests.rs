//! Reset/equal-history certificates are checked through actual factory devices.
use super::*;
use oc_dsl::ast::EffectType;

fn dual_mono(frames: usize) -> Vec<f32> {
    let mut seed = 0x6139_4a27_u32;
    (0..frames)
        .flat_map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let x = (seed as f64 / u32::MAX as f64 - 0.5) as f32;
            [x, x]
        })
        .collect()
}
fn equal_channels(audio: &[f32]) {
    assert!(audio.iter().all(|x| x.is_finite()));
    assert!(
        audio
            .chunks_exact(2)
            .all(|pair| pair[0].to_bits() == pair[1].to_bits())
    );
}

#[test]
fn channel_equality_real_gain_and_eq_preserve_random_history_and_shared_program_changes() {
    for ty in [EffectType::Gain, EffectType::Eq] {
        let mut effect = dsp::try_create_effect(&ty, &[], 48_000).unwrap();
        assert!(effect.preserves_channel_equality());
        let device = dsp::device_for_effect(&ty).unwrap();
        effect.reset();
        for step in 0..5 {
            for param in device.params {
                let value = [
                    param.min,
                    param.max,
                    param.default,
                    param.min * 0.75 + param.max * 0.25,
                    param.min * 0.25 + param.max * 0.75,
                ][step];
                effect.set_param(param.id, value);
            }
            let mut pcm = dual_mono(2048);
            effect.process(&mut pcm, 48_000);
            equal_channels(&pcm);
            assert!(effect.preserves_channel_equality());
        }
        effect.reset();
        let mut pcm = dual_mono(2048);
        effect.process(&mut pcm, 48_000);
        equal_channels(&pcm);
    }
}

#[test]
fn channel_equality_actual_automation_wrapper_preserves_certified_shared_controls() {
    let project=oc_dsl::parse("@project\n  tempo: 120\n@track t | instrument: audio/clip\n  effects:\n    - gain: factor=0.8\n    - eq: mid_freq=1kHz, mid_gain=0dB\n  automation: fx1.factor {1:1=0.2, 1:2=1.4}\n  automation: fx2.mid_gain {1:1=-12dB, 1:2=12dB}\n").unwrap();
    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
    assert!(graph.track_chains[0].preserves_channel_equality());
    assert!(
        graph.track_chains[0]
            .effects
            .iter()
            .all(|effect| effect.preserves_channel_equality())
    );
    let input = dual_mono(30_000);
    let mut pcm = input.clone();
    graph.track_chains[0].process_with_transport(
        &mut pcm,
        48_000,
        Some(PluginTransport::new(0, 48_000, 120.)),
    );
    equal_channels(&pcm);
    assert_ne!(pcm, input);
    graph.track_chains[0].reset();
    let mut replay = input;
    graph.track_chains[0].process_with_transport(
        &mut replay,
        48_000,
        Some(PluginTransport::new(0, 48_000, 120.)),
    );
    assert_eq!(pcm, replay);
}

struct Unknown;
impl AudioEffect for Unknown {
    fn process(&mut self, _: &mut [f32], _: u32) {}
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Unknown test device"
    }
}
struct ClaimedDependency {
    control: bool,
}
impl AudioEffect for ClaimedDependency {
    fn preserves_channel_equality(&self) -> bool {
        true
    }
    fn sidechain_source(&self) -> Option<&str> {
        (!self.control).then_some("key")
    }
    fn control_source(&self) -> Option<&str> {
        self.control.then_some("follower")
    }
    fn process(&mut self, _: &mut [f32], _: u32) {}
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test dependency"
    }
}
#[test]
fn channel_equality_unknown_stereo_bypass_and_signal_dependencies_are_conservative() {
    assert!(EffectChain::new().preserves_channel_equality());
    assert!(!Unknown.preserves_channel_equality());
    for ty in [
        EffectType::StereoWidth,
        EffectType::Chorus,
        EffectType::AllPass,
    ] {
        assert!(
            !dsp::try_create_effect(&ty, &[], 48_000)
                .unwrap()
                .preserves_channel_equality()
        );
    }
    for control in [false, true] {
        let mut chain = EffectChain::new();
        chain.effects.push(Box::new(ClaimedDependency { control }));
        assert!(!chain.preserves_channel_equality());
    }
    let project =
        oc_dsl::parse("@project\n@master\n  effects:\n    - gain: factor=1, bypass=true\n")
            .unwrap();
    let chain = EffectChain::try_from_effects(&project.master.unwrap().effects, 48_000).unwrap();
    assert!(!chain.preserves_channel_equality());
}
