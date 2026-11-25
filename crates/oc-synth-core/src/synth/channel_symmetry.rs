use super::{SynthPatch, Waveform};
use oc_audio::{ChannelSymmetry, ChannelSymmetryContext, MidiInputMessage};

impl SynthPatch {
    /// Conservative source-owner proof for a complete, reset render program.
    /// Single-voice unison uses position zero and identical `(1-pan,1+pan)`
    /// gains. Centered oscillator/sub balance paths likewise use equal scalars;
    /// both filter histories start at zero and receive identical operations.
    pub fn channel_symmetry(&self, context: &ChannelSymmetryContext<'_>) -> ChannelSymmetry {
        let mono_wave = |wave| {
            matches!(
                wave,
                Waveform::Sine
                    | Waveform::Saw
                    | Waveform::Square
                    | Waveform::Triangle
                    | Waveform::Wavetable
            )
        };
        if !context.is_complete_and_centered()
            || super::document::validate_signal_patch(self).is_err()
            || self.unison_voices != 1
            || self.oscillator_b_unison != 1
            || self.oscillator_c_unison != 1
            || [
                self.oscillator_a_pan,
                self.oscillator_b_pan,
                self.oscillator_c_pan,
                self.sub_pan,
            ]
            .iter()
            .any(|p| *p != 0.0)
            || ![
                self.oscillator_b_detune_cents,
                self.oscillator_c_detune_cents,
                self.oscillator_b_stereo_width,
                self.oscillator_c_stereo_width,
                self.sub_fine_cents,
                self.sub_initial_phase,
                self.velocity_to_amp,
                self.loudness_gain,
            ]
            .iter()
            .all(|v| v.is_finite())
            || ![
                self.oscillator_a_waveform,
                self.oscillator_b_waveform,
                self.oscillator_c_waveform,
            ]
            .into_iter()
            .all(mono_wave)
            || self.noise_level != 0.0
            || self.lfo_pitch_cents != 0.0
            || self.mod_routes.iter().any(|route| route.amount != 0.0)
            || self.sampler.is_some()
            || self.granular.iter().any(Option::is_some)
            || self.spectral.iter().any(Option::is_some)
            || !self.fx.is_empty()
            || !self.fx_mod_routes.is_empty()
            || self.mpe.is_some()
            || self.arpeggiator.is_some()
            || context
                .parameter_changes
                .iter()
                .any(|key| !scalar_parameter(key))
            || context.midi.iter().any(|event| !safe_midi(event))
        {
            ChannelSymmetry::Unknown
        } else {
            ChannelSymmetry::DualMono
        }
    }
}

fn scalar_parameter(key: &str) -> bool {
    let Some(parameter) = oc_device_schema::synth::find_param(key) else {
        return false;
    };
    let canonical = parameter.ids[0];
    canonical.starts_with("filter.")
        || canonical.starts_with("filter2.")
        || canonical.starts_with("filter_env.")
        || canonical.starts_with("amp.")
        || canonical.starts_with("env3.")
        || canonical.starts_with("env4.")
        || canonical.starts_with("tuning.")
        || matches!(canonical, "output.gain" | "output.loudness_trim_db")
}

fn safe_midi(message: &MidiInputMessage) -> bool {
    match message {
        // Program changes are ignored by OpenSynth, which has no MIDI program bank.
        MidiInputMessage::ProgramChange { .. }
        | MidiInputMessage::NoteOn { .. }
        | MidiInputMessage::NoteOff { .. }
        | MidiInputMessage::PitchBend { .. }
        | MidiInputMessage::ChannelPressure { .. }
        | MidiInputMessage::PolyPressure { .. } => true,
        MidiInputMessage::ControlChange { controller, .. } => {
            matches!(controller, 1 | 7 | 10 | 64 | 74 | 120 | 121 | 123)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::OpenSynth;

    #[test]
    fn symmetry_certificate_matches_polyphonic_pcm_and_ignored_programs() {
        let midi = [
            MidiInputMessage::ProgramChange {
                channel: 0,
                program: 42,
            },
            MidiInputMessage::ControlChange {
                channel: 0,
                controller: 7,
                value: 100,
            },
            MidiInputMessage::ControlChange {
                channel: 0,
                controller: 10,
                value: 127,
            },
        ];
        let parameters = ["filter.cutoff", "amp.attack"];
        let context = ChannelSymmetryContext::complete(&parameters, &midi, &[]);
        for waveform in [
            Waveform::Sine,
            Waveform::Saw,
            Waveform::Square,
            Waveform::Triangle,
            Waveform::Wavetable,
        ] {
            let mut patch = SynthPatch::default();
            patch.oscillator_a_waveform = waveform;
            patch.unison_voices = 1;
            patch.noise_level = 0.0;
            patch.mod_routes.clear();
            patch.lfo_pitch_cents = 0.0;
            assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::DualMono);
            let mut synth = OpenSynth::new(patch);
            let mut audible = false;
            for frame in 0..2400 {
                if frame % 97 == 0 {
                    synth.note_on(
                        0,
                        (12 + frame / 97 * 7 % 100) as u8,
                        (1 + frame / 97 * 31 % 127) as u8,
                    );
                }
                if frame == 1300 {
                    synth.set_param("filter.cutoff", 1733.0);
                }
                let audio = synth.next_stereo(8000);
                assert!(audio[0].is_finite());
                assert_eq!(audio[0].to_bits(), audio[1].to_bits());
                audible |= audio[0] != 0.0;
            }
            assert!(audible);
        }
    }

    #[test]
    fn symmetry_certificate_rejects_late_or_unknown_spatial_controls() {
        let mut patch = SynthPatch::default();
        patch.unison_voices = 1;
        patch.noise_level = 0.0;
        patch.mod_routes.clear();
        patch.lfo_pitch_cents = 0.0;
        let context = ChannelSymmetryContext::complete(&[], &[], &[]);
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::DualMono);
        assert_eq!(
            patch.channel_symmetry(&ChannelSymmetryContext::default()),
            ChannelSymmetry::Unknown
        );
        for key in [
            "osc.a.pan",
            "osc.b.unison",
            "noise.level",
            "fx.0.wet",
            "mod.1.amount",
            "unknown",
        ] {
            assert_eq!(
                patch.channel_symmetry(&ChannelSymmetryContext::complete(&[key], &[], &[])),
                ChannelSymmetry::Unknown
            );
        }
        assert_eq!(
            patch.channel_symmetry(&ChannelSymmetryContext::complete(&[], &[], &[0.0, 0.1])),
            ChannelSymmetry::Unknown
        );
        let pan = [MidiInputMessage::ControlChange {
            channel: 0,
            controller: 99,
            value: 65,
        }];
        assert_eq!(
            patch.channel_symmetry(&ChannelSymmetryContext::complete(&[], &pan, &[])),
            ChannelSymmetry::Unknown
        );
        patch.oscillator_b_pan = 0.01;
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
        patch.oscillator_b_pan = 0.0;
        patch.unison_voices = 2;
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
        patch.unison_voices = 1;
        patch.noise_level = 0.1;
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
        patch.noise_level = 0.0;
        patch.mod_routes = super::super::default_mod_routes();
        if let Some(route) = patch.mod_routes.first_mut() {
            route.destination = super::super::ModDestination::StereoWidth;
            route.amount = 0.5;
            assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
        }
        patch.mod_routes.clear();
        patch.oscillator_c_waveform = Waveform::Granular;
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
        patch.oscillator_c_waveform = Waveform::Sine;
        patch.gain = f64::NAN;
        assert_eq!(patch.channel_symmetry(&context), ChannelSymmetry::Unknown);
    }
}
