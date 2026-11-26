//! Compose source-owned channel equality with the actual prepared insert chains.
//!
//! The certificate describes the complete source lifetime. It accompanies the
//! rendered buffer so changing a file after preparation cannot qualify a
//! different source object.
use super::*;
use oc_audio::{ChannelSymmetry, ChannelSymmetryContext, MidiInputMessage};

#[derive(Clone, Default)]
pub(in crate::render_cache) struct SourceProgram {
    parameters: Vec<String>,
    midi: Vec<MidiInputMessage>,
    note_pans: Vec<f64>,
    unclassified: bool,
}

impl SourceProgram {
    pub(in crate::render_cache) fn with_context<T>(
        &self,
        inspect: impl FnOnce(&ChannelSymmetryContext<'_>) -> T,
    ) -> T {
        let names: Vec<_> = self.parameters.iter().map(String::as_str).collect();
        let mut context = ChannelSymmetryContext::complete(&names, &self.midi, &self.note_pans);
        context.has_unclassified_events = self.unclassified;
        inspect(&context)
    }

    fn push(&mut self, event: &MidiMessage) {
        let message = match event {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => MidiInputMessage::NoteOn {
                channel: *channel,
                key: *note,
                velocity: *velocity,
            },
            MidiMessage::NoteOff { channel, note } => MidiInputMessage::NoteOff {
                channel: *channel,
                key: *note,
            },
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => MidiInputMessage::ControlChange {
                channel: *channel,
                controller: *controller,
                value: *value,
            },
            MidiMessage::ProgramChange { channel, program } => MidiInputMessage::ProgramChange {
                channel: *channel,
                program: *program,
            },
            MidiMessage::PitchBend { channel, value } => MidiInputMessage::PitchBend {
                channel: *channel,
                value: *value,
            },
            MidiMessage::ChannelPressure { channel, pressure } => {
                MidiInputMessage::ChannelPressure {
                    channel: *channel,
                    pressure: *pressure,
                }
            }
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => MidiInputMessage::PolyPressure {
                channel: *channel,
                key: *note,
                pressure: *pressure,
            },
            MidiMessage::PluginParam { name, value } => {
                self.unclassified |= !value.is_finite();
                if !self.parameters.contains(name) {
                    self.parameters.push(name.clone());
                }
                return;
            }
            MidiMessage::NoteExpression {
                expression, value, ..
            } if expression == "pan" => {
                self.note_pans.push(*value);
                return;
            }
            _ => {
                self.unclassified = true;
                return;
            }
        };
        // Ordering and multiplicity do not affect the owner's universal proof.
        // Keep only distinct MIDI controls, not one allocation per played note.
        if !self.midi.contains(&message) {
            self.midi.push(message);
        }
    }
}

pub(in crate::render_cache) fn source_programs(
    events: &[MidiEvent],
    count: usize,
) -> Vec<Option<SourceProgram>> {
    let mut programs = vec![Some(SourceProgram::default()); count];
    for event in events {
        if let Some(Some(program)) = programs.get_mut(event.track_index) {
            program.push(&event.event);
        }
    }
    programs
}

pub(in crate::render_cache) fn fold_sources(
    project: &Project,
    targets: &[usize],
    sources: &[ChannelSymmetry],
    chains: &[crate::graph::EffectChain],
) -> Vec<ChannelSymmetry> {
    let mut result = vec![ChannelSymmetry::DualMono; project.tracks.len()];
    let mut seen = vec![false; project.tracks.len()];
    for (lane, &target) in targets.iter().enumerate() {
        let Some(output) = result.get_mut(target) else {
            continue;
        };
        seen[target] = true;
        if sources.get(lane) != Some(&ChannelSymmetry::DualMono)
            || chains
                .get(lane)
                .is_none_or(|chain| !chain.preserves_channel_equality())
        {
            *output = ChannelSymmetry::Unknown;
        }
    }
    for ((output, track), seen) in result.iter_mut().zip(&project.tracks).zip(seen) {
        if !seen || track.audio_clips().next().is_some() || !track.input_routes.is_empty() {
            *output = ChannelSymmetry::Unknown;
        }
    }
    result
}

/// Identical effective sources share only a proof, never oscillator or effect
/// state. Every future copy contributes its complete control program.
pub(in crate::render_cache) struct SourceProofPlan {
    groups: Vec<Option<usize>>,
    parameters: Vec<Vec<(String, ParamValue)>>,
    programs: Vec<Option<SourceProgram>>,
}

impl SourceProofPlan {
    pub(in crate::render_cache) fn new(
        project: &Project,
        programs: Vec<Option<SourceProgram>>,
        _resource_root: Option<&Path>,
    ) -> Self {
        let mut groups: Vec<Option<usize>> = vec![None; project.tracks.len()];
        let mut parameters = vec![Vec::new(); project.tracks.len()];
        let mut plugins = vec![None; project.tracks.len()];
        let mut normalization = vec![None; project.tracks.len()];
        let mut combined: Vec<Option<SourceProgram>> = vec![None; project.tracks.len()];
        for (index, track) in project.tracks.iter().enumerate() {
            let Some(mut program) = programs[index].clone() else {
                continue;
            };
            // Preserve authored device identifiers even when an interpreter
            // cannot lower their values into a supported source event.
            for lane in track
                .automation()
                .filter(|lane| !lane.muted && lane.effect_lane().is_none())
            {
                if let Some(key) = oc_dsl::automation::decode_plugin_automation_param(&lane.param)
                    && !program.parameters.contains(&key)
                {
                    program.parameters.push(key);
                }
            }
            let preset = track
                .preset
                .as_deref()
                .and_then(|name| project.presets.iter().find(|preset| preset.name == name));
            let plugin = preset.and_then(|preset| preset.plugin.as_deref());
            if !matches!(
                plugin,
                Some(oc_synth::synth::PLUGIN_ID | oc_synth::drumkit::PLUGIN_ID)
            ) {
                continue;
            }
            parameters[index] =
                crate::preset::builtin_track_params_untrimmed(project, preset, track);
            normalization[index] = preset.map(crate::loudness::normalization_identity);
            plugins[index] = plugin;
            let group = (0..index)
                .find(|&other| {
                    groups[other].is_some()
                        && plugins[other] == plugin
                        && normalization[other] == normalization[index]
                        && parameters[other] == parameters[index]
                })
                .and_then(|other| groups[other])
                .unwrap_or(index);
            groups[index] = Some(group);
            if let Some(existing) = &mut combined[group] {
                existing
                    .parameters
                    .extend(program.parameters.iter().cloned());
                existing.midi.extend_from_slice(&program.midi);
                existing.note_pans.extend_from_slice(&program.note_pans);
                existing.unclassified |= program.unclassified;
            } else {
                combined[group] = Some(program.clone());
            }
        }
        Self {
            groups,
            parameters,
            programs: combined,
        }
    }

    pub(in crate::render_cache) fn program(&self, lane: usize) -> Option<SourceProgram> {
        self.groups[lane].and_then(|group| self.programs[group].clone())
    }

    pub(in crate::render_cache) fn parameters(
        &self,
        lane: usize,
    ) -> Option<&Vec<(String, ParamValue)>> {
        self.groups[lane].map(|_| &self.parameters[lane])
    }

    pub(in crate::render_cache) fn propagate(
        &self,
        rendered: &[(usize, ChannelSymmetry)],
        output: &mut [ChannelSymmetry],
    ) {
        for (lane, group) in self.groups.iter().enumerate() {
            let Some(group) = group else { continue };
            let matching: Vec<_> = rendered
                .iter()
                .filter(|(index, _)| self.groups[*index] == Some(*group))
                .collect();
            if !matching.is_empty()
                && matching
                    .iter()
                    .all(|(_, symmetry)| *symmetry == ChannelSymmetry::DualMono)
            {
                output[lane] = ChannelSymmetry::DualMono;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_project() -> Project {
        let mut text = String::from(
            r#"@ocp "0.2.0"
@project "Source equality"
  tempo: 120
@preset root_voice
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 4, osc.b.waveform: 0, osc.b.level: 0.3, sub.level: 0.15, noise.level: 0, unison.voices: 1, unison.detune: 4ct, output.gain: 0.2}
  effects:
    - eq: low_gain=-1dB
@preset voice
@pattern keys
  events:
    @0 C4 q mf
    @1 E4 q mf
@pattern beat
  events:
    @0 C2 q mf
    @1 C2 q mf
@track keys | preset: voice, instrument: synth/keys
  mix: pan=0.3
  clip: keys [1:1 - 2:1]
  clip: keys [4:1 - 5:1]
  automation: brightness {1:1=30%, 4:1=70%}
  automation: pan {1:1=-0.2, 4:1=0.4}
"#,
        );
        for index in 0..4 {
            text.push_str(&format!(
                r#"
@kit hits{index}
  pad kick | gain: 6dB, tune: {index}, decay: 0.1s, one_shot: true
  layer kick | low: 1, high: 127, sample: "tests/fixtures/sampler/tone.flac"
@track drums{index} | preset: hits{index}, instrument: drums/drumkit
  mix: pan=0.1
  clip: beat [1:1 - 2:1]
  clip: beat [4:1 - 5:1]
"#
            ));
        }
        let mut project = oc_dsl::parse(&text).unwrap();
        project
            .presets
            .iter_mut()
            .find(|preset| preset.name == "voice")
            .unwrap()
            .extends = Some("root_voice".into());
        project
    }

    fn prefix(project: &Project) -> RenderCache {
        let mut owner = RenderCache::new();
        owner.set_resource_root(Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
        let selected: Vec<_> = (0..project.tracks.len()).collect();
        owner
            .render_selection_scratch(
                project,
                Path::new("unused.sf2"),
                8000,
                &mut HashMap::new(),
                ScratchRenderPass {
                    purpose: ScratchPurpose::Audio,
                    selected: &selected,
                    stems: false,
                    window_end: Some(4000),
                    control_only: &[],
                    workers: crate::render_parallel::RenderWorkerOptions::default(),
                },
            )
            .unwrap()
            .0
    }

    #[test]
    fn prepared_source_proofs_cover_inheritance_and_all_identical_future_copies() {
        let project = source_project();
        let scratch = prefix(&project);
        assert_eq!(
            scratch.track_channel_symmetry,
            vec![ChannelSymmetry::DualMono; 5]
        );
        for index in 0..5 {
            assert_eq!(scratch.full_mix_widths[index], SourceWidth::Mono);
            assert!(
                scratch.track_buffers[index]
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|sample| *sample != 0.0)
            );
            assert!(super::super::width::cold_track_width_is_final(
                &scratch, &project, index
            ));
        }
        let mut full = RenderCache::new();
        full.set_resource_root(Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
        full.set_pattern_previews(false);
        let reference = full
            .render_full(&project, Path::new("unused.sf2"), 8000, &mut HashMap::new())
            .unwrap();
        let mut cold = RenderCache::new();
        cold.set_resource_root(Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
        let request = RangeRenderRequest {
            start_sample: 100,
            end_sample: 2000,
            tracks: None,
            stems: false,
        };
        let output = cold
            .render_range(
                &project,
                Path::new("unused.sf2"),
                8000,
                &mut HashMap::new(),
                request,
            )
            .unwrap();
        assert_eq!(output.audio, reference[200..4000]);
        assert_eq!(
            cold.render_counters().source_renders,
            10,
            "one prefix pass over ten planned lanes, without full-source width witnesses"
        );
        assert!(cold.master_buffer().is_none());
    }

    #[test]
    fn a_future_different_source_or_unclassified_insert_is_not_certified() {
        let mut project = source_project();
        let mut stereo = project
            .presets
            .iter()
            .find(|preset| preset.name == "root_voice")
            .unwrap()
            .clone();
        stereo.name = "future_stereo".into();
        stereo
            .params
            .push(("osc.a.pan".into(), ParamValue::Number(0.5)));
        project.presets.push(stereo);
        let mut future = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == "keys")
            .unwrap()
            .clone();
        future.name = "different".into();
        future.preset = Some("future_stereo".into());
        project.patterns.push(future);
        project.tracks[0]
            .pattern_clips_mut()
            .nth(1)
            .unwrap()
            .pattern = "different".into();
        assert_eq!(
            prefix(&project).track_channel_symmetry[0],
            ChannelSymmetry::Unknown
        );

        let mut unknown = source_project();
        let mut insert =
            oc_dsl::parse("@project\n@preset x\n  effects:\n    - chorus: mix=0%\n").unwrap();
        unknown
            .presets
            .iter_mut()
            .find(|preset| preset.name == "voice")
            .unwrap()
            .effects
            .append(&mut insert.presets[0].effects);
        assert_eq!(
            prefix(&unknown).track_channel_symmetry[0],
            ChannelSymmetry::Unknown
        );
    }

    #[test]
    fn full_future_source_controls_are_checked_before_window_event_filtering() {
        for key in ["osc.a.pan", "unison.voices", "unknown.source.control"] {
            let mut project = source_project();
            let encoded = oc_dsl::automation::encode_plugin_automation_param(key);
            let lane = oc_dsl::parse(&format!(
                "@project\n@track t | instrument: synth/keys\n  automation: {encoded} {{4:1=0.5}}\n"
            ))
            .unwrap()
            .tracks
            .remove(0);
            // Use the owner's clips container API to retain the authored lane.
            for automation in lane.automation().cloned() {
                project.tracks[0].push_automation(automation);
            }
            assert_eq!(
                prefix(&project).track_channel_symmetry[0],
                ChannelSymmetry::Unknown,
                "late {key}"
            );
        }
        let events = [MidiEvent {
            time_samples: 1_000_000,
            track_index: 0,
            event: MidiMessage::NoteExpression {
                channel: 0,
                note: 60,
                expression: "pan".into(),
                value: 0.4,
            },
        }];
        let program = source_programs(&events, 1).remove(0).unwrap();
        assert!(!program.with_context(|context| context.is_complete_and_centered()));
    }

    #[test]
    fn grouping_is_measurement_free_and_retains_normalizer_effect_identity() {
        let mut project = source_project();
        let root = project
            .presets
            .iter_mut()
            .find(|preset| preset.name == "root_voice")
            .unwrap();
        root.params
            .push(("output.gain".into(), ParamValue::Number(0.193721)));
        let resolved = oc_dsl::preset::resolve(&project).unwrap();
        let lanes = crate::source_lanes::plan(&resolved).unwrap();
        let project = lanes.tracks();
        let preset = project
            .presets
            .iter()
            .find(|preset| preset.name == "voice")
            .unwrap();
        assert!(crate::loudness::ready_native_trim(preset).is_none());
        let plan = SourceProofPlan::new(
            project,
            vec![Some(SourceProgram::default()); project.tracks.len()],
            None,
        );
        assert!(crate::loudness::ready_native_trim(preset).is_none());
        assert_eq!(
            plan.groups[0], plan.groups[1],
            "identical future copies share the actual prepared proof"
        );

        let mut modified = project.clone();
        let mut different = preset.clone();
        different.name = "different_normalizer".into();
        different.effects.clear();
        modified.presets.push(different);
        modified.tracks[1].preset = Some("different_normalizer".into());
        let plan = SourceProofPlan::new(
            &modified,
            vec![Some(SourceProgram::default()); modified.tracks.len()],
            None,
        );
        assert_eq!(
            plan.parameters[0], plan.parameters[1],
            "effective instrument params alone do not identify a trim"
        );
        assert_ne!(
            plan.groups[0], plan.groups[1],
            "normalizer FX identity must also match"
        );
    }
}
