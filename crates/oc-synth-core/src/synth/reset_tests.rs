//! Reset uses the production voice/FX paths and retains prepared sample assets.
use super::*;

struct SampleAsset(std::path::PathBuf);
impl SampleAsset {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "opencompose-reset-{}-{}.wav",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        write_sampler_tone(&path, 441.0);
        Self(path)
    }
    fn remove(&self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}
impl Drop for SampleAsset {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn render_note(synth: &mut OpenSynth, frames: usize) -> Vec<[f32; 2]> {
    synth.note_on(0, 60, 100);
    (0..frames).map(|_| synth.next_stereo(48_000)).collect()
}

#[test]
fn reset_retains_deleted_sample_sources_and_performs_no_heap_operations() {
    for source in [None, Some(Waveform::Granular), Some(Waveform::Spectral)] {
        let asset = SampleAsset::new();
        let patch = match source {
            Some(waveform) => source_patch(waveform, &asset.0),
            None => sampler_tone_patch(&asset.0, 44_100),
        };
        let mut synth = OpenSynth::new(patch.clone());
        let mut fresh = OpenSynth::new(patch);
        let expected = render_note(&mut fresh, 4096);
        assert!(expected.iter().flatten().any(|sample| sample.abs() > 1e-4));
        asset.remove();
        for _ in 0..3 {
            synth.set_pitch_bend(0, 4000);
            synth.set_mod_wheel(0, 95);
            synth.set_channel_pressure(0, 127);
            synth.set_timbre(0, 100);
            synth.set_sustain(0, true);
            synth.note_on(0, 67, 110);
            for _ in 0..521 {
                synth.next_stereo(44_100);
            }
            let operations = crate::test_alloc::operations(|| {
                for _ in 0..8 {
                    synth.reset();
                }
            });
            assert_eq!(
                operations, 0,
                "reset must not allocate, reallocate or free: {source:?}"
            );
            assert_eq!(render_note(&mut synth, 4096), expected, "{source:?}");
        }
    }
}

#[test]
fn reset_matches_fresh_random_lfo_fx_and_arp_state() {
    let mut patch = SynthPatch {
        lfo_shape_type: LfoShapeType::RandomHold,
        lfo_rate: 27.0,
        noise_level: 0.2,
        sub_level: 0.15,
        sub_contiguous_phase: true,
        arpeggiator: Some(arpeggiator::ArpSettings {
            beats_per_step: 0.03125,
            gate: 0.75,
            descending: true,
        }),
        ..Default::default()
    };
    patch.extra_lfos[0].shape_type = LfoShapeType::ChaosA;
    patch.mod_routes[0] = ModRoute {
        source: ModSource::Lfo,
        destination: ModDestination::FilterCutoff,
        amount: 0.4,
        ..Default::default()
    };
    let mut delay = SynthFx::new(SynthFxType::Delay);
    delay.set_param("time", 0.005);
    delay.set_param("feedback", 0.6);
    let mut reverb = SynthFx::new(SynthFxType::Reverb);
    reverb.set_param("mix", 0.5);
    patch.fx = vec![delay, reverb];
    let mut synth = OpenSynth::new_with_tempo(patch.clone(), 137.0);
    let mut fresh = OpenSynth::new_with_tempo(patch, 137.0);
    let expected = render_note(&mut fresh, 4096);
    assert!(expected.iter().flatten().any(|sample| sample.abs() > 1e-4));
    for _ in 0..3 {
        synth.note_on(0, 72, 120);
        synth.set_sustain(0, true);
        synth.set_pitch_bend(0, -2000);
        for _ in 0..4096 {
            synth.next_stereo(48_000);
        }
        let operations = crate::test_alloc::operations(|| synth.reset());
        assert_eq!(operations, 0);
        assert_eq!(render_note(&mut synth, 4096), expected);
    }
}
