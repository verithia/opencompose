//! Per-source release bounds and native declared-tail preparation.

use super::*;

pub(super) struct InstrumentTailProbes {
    pub vst3: super::vst3_tail::Vst3TailProbe,
    pub vst2: super::vst2_prepare::Vst2TailProbe,
}

pub(super) fn instrument_tails_by_lane(
    project: &Project,
    authored_presets: &[PresetDef],
    plugin_instances: &mut HashMap<String, ClapInstance>,
    sample_rate: u32,
    resource_root: Option<&Path>,
    probes: InstrumentTailProbes,
) -> Result<Vec<u64>, String> {
    let mut tails = vec![0; project.tracks.len()];
    let mut shared_tails = HashMap::new();
    for (index, track) in project.tracks.iter().enumerate() {
        // MIDI-only lanes never create an audio source in the render plan;
        // their preset state therefore cannot contribute an instrument tail.
        if track.instrument.is_midi_only() {
            continue;
        }
        let Some(preset_name) = track.preset.as_deref() else {
            continue;
        };
        let Some(preset) = project
            .presets
            .iter()
            .find(|preset| preset.name == preset_name)
        else {
            continue;
        };
        let vst3 = preset.source.as_deref() == Some("vst3") || is_sfizz_sample_preset(preset);
        // Parameter overrides belong to each source lane. A preset name alone
        // cannot identify the state whose reported tail determines its horizon.
        let sampled_builtin = preset.source.as_deref() == Some("builtin")
            && matches!(
                preset.plugin.as_deref(),
                Some(oc_synth::sampler::PLUGIN_ID | oc_synth::drumkit::PLUGIN_ID)
            );
        let per_lane = vst3 || preset.source.as_deref() == Some("vst2") || sampled_builtin;
        if !per_lane && let Some(&tail) = shared_tails.get(preset_name) {
            tails[index] = tail;
            continue;
        }
        let reported = match preset.source.as_deref() {
            Some("clap") => {
                if let Some(instance) = plugin_instances.get_mut(preset_name) {
                    instance.tail_samples()
                } else {
                    0
                }
            }
            Some("vst3") | Some("sample")
                if preset.source.as_deref() == Some("vst3") || is_sfizz_sample_preset(preset) =>
            {
                super::vst3_tail::instrument_tail(
                    authored_presets,
                    preset,
                    track,
                    sample_rate,
                    resource_root,
                    probes.vst3,
                )?
            }
            Some("vst2") => super::vst2_prepare::instrument_tail(
                authored_presets,
                preset,
                track,
                sample_rate,
                resource_root,
                probes.vst2,
            )?,
            Some("au") => {
                #[cfg(target_os = "macos")]
                {
                    // The probe owns its unit only for the tail query; dropping
                    // it here is the sole teardown of this instance.
                    (|| {
                        let plugin = preset.plugin.as_deref()?;
                        let mut instance = AuInstance::load(plugin, sample_rate, 512).ok()?;
                        let context =
                            PluginStateContext::new(PluginFormat::Au, Some(plugin), &preset.name);
                        let _ = apply_au_preset_state_at(
                            &mut instance,
                            &preset.params,
                            &context,
                            resource_root,
                        );
                        if let Err(error) = apply_au_preset_params(&mut instance, &preset.params) {
                            crate::render_warnings::push(format!(
                                "AU preset '{}' parameter setup failed: {error}",
                                preset.name
                            ));
                        }
                        Some(instance.tail_samples())
                    })()
                    .unwrap_or(0)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    0
                }
            }
            Some("builtin") if sampled_builtin => {
                let params =
                    crate::preset::builtin_track_params_untrimmed(project, Some(preset), track);
                let seconds = if preset.plugin.as_deref() == Some(oc_synth::drumkit::PLUGIN_ID) {
                    oc_synth::drumkit::params_tail_seconds(&params, resource_root)
                } else {
                    oc_synth::sampler::params_tail_seconds(&params, resource_root)
                };
                seconds
                    .map(|seconds| {
                        (seconds * f64::from(sample_rate))
                            .ceil()
                            .min(f64::from(u32::MAX)) as u32
                    })
                    .unwrap_or(0)
            }
            _ => 0,
        };
        let tail = bounded_tail_samples(reported, sample_rate);
        tails[index] = tail;
        if !per_lane {
            shared_tails.insert(preset_name, tail);
        }
    }
    Ok(tails)
}

pub(super) struct SourceHorizon<'a> {
    pub ends: &'a [u64],
    pub tails: &'a [u64],
    pub audio_end: u64,
    pub fallback_tail: u64,
    pub minimum_end: u64,
}

/// A release belongs to its own source's last command, independently of later
/// notes or audio on another lane. Mixing adds graph tails to this horizon.
pub(super) fn render_horizon(request: SourceHorizon<'_>) -> Result<u64, String> {
    if request.ends.len() != request.tails.len() {
        return Err("instrument horizon lane count mismatch".into());
    }
    let last_event = request.ends.iter().copied().max().unwrap_or(0);
    let mut end = last_event
        .max(request.audio_end)
        .checked_add(request.fallback_tail)
        .ok_or_else(|| "instrument tail render length overflow".to_owned())?;
    for (&source_end, &tail) in request.ends.iter().zip(request.tails) {
        end = end.max(
            source_end
                .checked_add(tail)
                .ok_or_else(|| "instrument tail render length overflow".to_owned())?,
        );
    }
    Ok(end.max(request.minimum_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_midi_only_sources_never_prepare_native_state() {
        for format in ["vst2", "vst3", "au"] {
            let project = oc_dsl::parse(&format!(
                "@project\n@preset missing\n  source: {format}\n  plugin: \"unavailable.plugin\"\n  params: {{state_file: \"unavailable-state.bin\"}}\n@track commands | preset: missing, instrument: midi/out\n"
            )).unwrap();
            let tails = instrument_tails_by_lane(
                &project,
                &project.presets,
                &mut HashMap::new(),
                48_000,
                None,
                InstrumentTailProbes {
                    vst3: |_| panic!("a MIDI-only lane must not prepare a native tail"),
                    vst2: |_| panic!("a MIDI-only lane must not prepare a native tail"),
                },
            )
            .unwrap();
            assert_eq!(tails, [0]);
        }
    }

    #[test]
    fn horizon_keeps_unscored_native_and_additional_output_sources() {
        let mut project = oc_dsl::parse(
            "@project\n@preset source\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n  params: {Gain: 0.1}\n@track empty | preset: source, instrument: synth/lead\n@track output | preset: source, instrument: synth/lead\n",
        ).unwrap();
        project.tracks[1].output_pair = Some(1);
        project.tracks[1].engine_params = Some(vec![("Gain".into(), ParamValue::Number(0.2))]);
        let tails = instrument_tails_by_lane(
            &project,
            &project.presets,
            &mut HashMap::new(),
            48_000,
            None,
            InstrumentTailProbes {
                vst3: |plan| {
                    Ok(
                        (plan.parameters.last().unwrap().native_value.unwrap() * 1000.0).round()
                            as u32,
                    )
                },
                vst2: |_| panic!("fixture uses VST3 only"),
            },
        )
        .unwrap();
        // An empty score is not proof of silence: a source may self-generate,
        // expose another owner's output, or receive later control input.
        assert_eq!(tails, [100, 200]);
    }

    #[test]
    fn horizon_unscored_native_failure_is_not_hidden_as_zero_tail() {
        let project = oc_dsl::parse(
            "@project\n@preset missing\n  source: vst2\n  plugin: \"unavailable.plugin\"\n@track empty | preset: missing, instrument: synth/lead\n",
        ).unwrap();
        assert!(
            instrument_tails_by_lane(
                &project,
                &project.presets,
                &mut HashMap::new(),
                48_000,
                None,
                InstrumentTailProbes {
                    vst3: |_| panic!("fixture uses VST2 only"),
                    vst2: |_| panic!("unresolved plugin must fail before probe dispatch"),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn horizon_preserves_release_arrangement_and_minimum_boundaries() {
        let horizon = |minimum_end| {
            render_horizon(SourceHorizon {
                ends: &[10, 30],
                tails: &[40, 0],
                audio_end: 35,
                fallback_tail: 2,
                minimum_end,
            })
            .unwrap()
        };
        assert_eq!(horizon(0), 50);
        assert_eq!(horizon(70), 70);
    }

    #[test]
    fn horizon_refuses_mismatched_lanes_and_overflow() {
        assert!(
            render_horizon(SourceHorizon {
                ends: &[1],
                tails: &[],
                audio_end: 0,
                fallback_tail: 2,
                minimum_end: 0,
            })
            .is_err()
        );
        for (end, tail, fallback) in [(u64::MAX, 1, 0), (u64::MAX, 0, 2)] {
            assert!(
                render_horizon(SourceHorizon {
                    ends: &[end],
                    tails: &[tail],
                    audio_end: 0,
                    fallback_tail: fallback,
                    minimum_end: 0,
                })
                .is_err()
            );
        }
    }
}
