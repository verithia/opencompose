//! Whole-program post-insert width witnesses for bounded cold mixes.
use super::*;

pub(in crate::render_cache) struct WidthReference {
    pub(in crate::render_cache) source_latencies: Vec<u32>,
    pub(in crate::render_cache) source_tails: Vec<u32>,
}

impl RenderCache {
    /// Resolve only audible mono prefixes against their full source/insert
    /// history. Routing and master processing do not participate in a track's
    /// width and are omitted from this witness pass.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_mono_widths(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        prefix: &RenderCache,
        ambiguous: &[usize],
        workers: crate::render_parallel::RenderWorkerOptions,
    ) -> Result<bool, String> {
        // Input-silent source inserts can have a different admission path when
        // future events arrive. Keep their existing full fallback.
        if ambiguous.iter().any(|&index| {
            prefix
                .track_buffers
                .get(index)
                .and_then(Option::as_deref)
                .is_none_or(is_silent)
        }) {
            return Ok(false);
        }
        // The prefix already prepared native chains. Rebuilding unrelated
        // native graph owners merely to learn a width is not a safe shortcut.
        let hosted = |effect: &oc_dsl::ast::Effect| {
            matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_))
        };
        if project
            .tracks
            .iter()
            .flat_map(|track| &track.effects)
            .chain(project.presets.iter().flat_map(|preset| &preset.effects))
            .chain(project.buses.iter().flat_map(|bus| &bus.effects))
            .chain(project.master.iter().flat_map(|master| &master.effects))
            .any(hosted)
        {
            return Ok(false);
        }
        let graph = prefix.graph_for(project, sample_rate, None)?;
        let mut selected = ambiguous.to_vec();
        // Including every declared feed is a conservative transitive closure:
        // key processing follows the graph's existing topological order.
        selected.extend(graph.control_source_tracks());
        selected.extend(graph.sidechain_source_tracks());
        expand_shared_output_groups(project, &mut selected);
        selected.sort_unstable();
        selected.dedup();
        if selected.iter().any(|&index| {
            project
                .tracks
                .get(index)
                .is_none_or(|track| !track.input_routes.is_empty())
        }) || has_unisolated_clap(project, &selected)?
        {
            return Ok(false);
        }
        let control_only: Vec<_> = selected
            .iter()
            .copied()
            .filter(|index| !ambiguous.contains(index))
            .collect();
        let (witness, audio) = self.render_selection_scratch(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            ScratchRenderPass {
                purpose: ScratchPurpose::TrackWidths { reference: prefix },
                selected: &selected,
                stems: false,
                window_end: None,
                control_only: &control_only,
                workers,
            },
        )?;
        debug_assert!(audio.is_empty());
        if selected.iter().any(|&index| {
            witness.track_source_latencies.get(index) != prefix.track_source_latencies.get(index)
                || witness.track_source_tails.get(index) != prefix.track_source_tails.get(index)
        }) {
            return Ok(false);
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.counters.source_renders += witness.counters.source_renders;
        }
        Ok(ambiguous
            .iter()
            .all(|&index| witness.full_mix_widths.get(index) == Some(&SourceWidth::Mono)))
    }
}

/// Ordinary MIDI CLAP uses a caller-owned instance. A speculative prefix may
/// advance its unsaved oscillator state before a fallback, so it uses the one
/// ordinary full pass instead. Performance streams have a request-owned fork.
pub(super) fn has_unisolated_clap(project: &Project, selected: &[usize]) -> Result<bool, String> {
    let is_clap = |preset: &oc_dsl::ast::PresetDef| {
        preset.source.as_deref() == Some("clap")
            && preset
                .plugin
                .as_deref()
                .is_some_and(|id| !id.starts_with("builtin."))
    };
    if !project.presets.iter().any(is_clap) {
        return Ok(false);
    }
    let resolved = oc_dsl::preset::resolve(project)?;
    let lanes = crate::source_lanes::plan(&resolved)?;
    Ok(lanes
        .tracks()
        .tracks
        .iter()
        .enumerate()
        .any(|(index, track)| {
            selected.contains(&lanes.targets[index])
                && track.performance.is_none()
                && track
                    .preset
                    .as_deref()
                    .and_then(|name| resolved.presets.iter().find(|p| p.name == name))
                    .is_some_and(is_clap)
        }))
}

/// Stereo is a promote-only pan classification. A non-silent mono prefix may
/// become stereo later and change the full mix's earlier pan gains; that case
/// requires the full-program fallback instead of assuming future width.
/// Raw silence is evidence only without inserts: an input-silent effect may
/// generate audible mono now and become stereo when future source audio arrives.
pub(super) fn cold_window_matches_full(
    scratch: &RenderCache,
    project: &Project,
    selected: &[usize],
    guard: u64,
    control_only: &[usize],
) -> bool {
    control_only.is_empty()
        && u64::from(scratch.master_latency_samples) <= guard
        && selected
            .iter()
            .all(|&index| cold_track_width_is_final(scratch, project, index))
}

pub(super) fn cold_track_width_is_final(
    scratch: &RenderCache,
    project: &Project,
    index: usize,
) -> bool {
    scratch.full_mix_widths.get(index).copied() == Some(SourceWidth::Stereo)
        || (scratch.full_mix_widths.get(index).copied() == Some(SourceWidth::Mono)
            && scratch.track_channel_symmetry.get(index)
                == Some(&oc_audio::ChannelSymmetry::DualMono))
        || (project.tracks.get(index).is_some_and(|track| {
            track.effects.is_empty()
                && track.input_routes.is_empty()
                && !preset_has_inserts(project, track.preset.as_deref())
        }) && scratch
            .track_buffers
            .get(index)
            .and_then(Option::as_deref)
            .is_none_or(is_silent))
}

fn preset_has_inserts<'a>(project: &'a Project, mut name: Option<&'a str>) -> bool {
    for _ in 0..=project.presets.len() {
        let Some(current) = name else {
            return false;
        };
        let Some(preset) = project.presets.iter().find(|preset| preset.name == current) else {
            return true;
        };
        if !preset.effects.is_empty() {
            return true;
        }
        name = preset.extends.as_deref();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "perf-tests")]
    #[test]
    fn reference_prefix_reports_unproved_pan_widths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join("benchmarks/session-20tracks.ocp");
        let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let selected: Vec<_> = (0..project.tracks.len()).collect();
        let mut cache = RenderCache::new();
        cache.set_resource_root(path.parent().map(Path::to_path_buf));
        let started = std::time::Instant::now();
        let (scratch, _) = cache
            .render_selection_scratch(
                &project,
                &root.join("crates/oc-engine/tests/fixtures/minimal-test.sf2"),
                48_000,
                &mut HashMap::new(),
                ScratchRenderPass {
                    purpose: ScratchPurpose::Audio,
                    selected: &selected,
                    stems: false,
                    window_end: Some(528_000),
                    control_only: &[],
                    workers: crate::render_parallel::RenderWorkerOptions::default(),
                },
            )
            .unwrap();
        println!("eleven-second prefix elapsed: {:?}", started.elapsed());
        for &index in &selected {
            let silent = scratch
                .track_buffers
                .get(index)
                .and_then(Option::as_deref)
                .is_none_or(is_silent);
            if scratch.full_mix_widths.get(index) != Some(&SourceWidth::Stereo) && !silent {
                println!(
                    "unproved mono width: track {index} {}",
                    project.tracks[index].name
                );
            }
        }
        for (index, buffer) in scratch.track_buffers.iter().enumerate() {
            if let Some(buffer) = buffer {
                println!("prefix source frames {index}: {}", buffer.len() / 2);
            }
        }
        assert!(
            scratch.duration_seconds < 30.0,
            "the prefix plus bounded tails is not the sixty-second program"
        );
    }

    #[test]
    fn silent_input_is_not_a_width_proof_for_mono_generating_inserts() {
        struct MonoBias;
        impl crate::dsp::AudioEffect for MonoBias {
            fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
                for sample in buffer {
                    *sample += 0.1;
                }
            }
            fn set_param(&mut self, _name: &str, _value: f64) {}
            fn get_param(&self, _name: &str) -> Option<f64> {
                None
            }
            fn reset(&mut self) {}
            fn name(&self) -> &str {
                "test mono bias"
            }
        }
        for effects_on_preset in [false, true] {
            let source = if effects_on_preset {
                "@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  effects:\n    - gain: factor=1\n@track t | preset: tone, instrument: audio/clip\n  mix: pan=0.4\n"
            } else {
                "@project\n@track t | instrument: audio/clip\n  mix: pan=0.4\n  effects:\n    - gain: factor=1\n"
            };
            let project = oc_dsl::parse(source).unwrap();
            let graph = || {
                let mut graph =
                    AudioGraph::try_from_project_with_source_latencies(&project, 8_000, &[])
                        .unwrap();
                graph.track_chains[0].effects = vec![Box::new(MonoBias)];
                graph
            };
            let raw_prefix = vec![0.0; 1_000];
            let mut prefix_graph = graph();
            let prefix = prefix_graph.mix(&mut [raw_prefix.clone()]);
            let mut full_source = raw_prefix.clone();
            full_source.extend([0.25, -0.25].repeat(500));
            let full = graph().mix(&mut [full_source]);
            assert_ne!(
                prefix,
                full[..1_000],
                "future stereo changes the insert's earlier pan law"
            );
            let mut scratch = RenderCache::new();
            scratch.track_buffers = vec![Some(raw_prefix)];
            scratch.full_mix_widths = prefix_graph.track_widths().to_vec();
            assert_eq!(scratch.full_mix_widths, [SourceWidth::Mono]);
            assert!(!cold_window_matches_full(
                &scratch,
                &project,
                &[0],
                8_000,
                &[]
            ));
        }
    }

    #[test]
    fn unisolated_clap_skips_the_speculative_prefix_before_touching_an_instance() {
        let project = oc_dsl::parse("@project\n@preset synth\n  source: clap\n  plugin: \"test.fixture.clap.silent\"\n@track live | preset: synth, instrument: synth/lead_square\n  mix: pan_law=balance\n  @phrase p [1:1 - 10:1]\n    C4 w\n@track other | instrument: audio/clip\n").unwrap();
        assert!(has_unisolated_clap(&project, &[0]).unwrap());
        assert!(!has_unisolated_clap(&project, &[1]).unwrap());
        let mut cache = RenderCache::new();
        let request = RangeRenderRequest {
            start_sample: 0,
            end_sample: 8_000,
            tracks: None,
            stems: false,
        };
        let output = cache
            .try_cold_full_window(
                &project,
                Path::new("unused.sf2"),
                8_000,
                &mut HashMap::new(),
                RangeRenderPass {
                    request: &request,
                    frames: 8_000,
                    workers: Default::default(),
                },
            )
            .unwrap();
        assert!(
            output.is_none(),
            "an absent live instance is not even looked up by a speculative pass"
        );
        let performance = oc_dsl::parse("@project\n@preset synth\n  source: clap\n  plugin: \"test.fixture.clap.silent\"\n@track live | preset: synth, instrument: synth/lead_square\n  performance: { @0 ch0 note_on 60 100; @1 ch0 note_off 60 0; }\n").unwrap();
        assert!(!has_unisolated_clap(&performance, &[0]).unwrap());
    }

    #[test]
    fn width_only_graph_processes_track_inserts_without_bus_or_master_pcm() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct CountBias(Arc<AtomicUsize>);
        impl crate::dsp::AudioEffect for CountBias {
            fn process(&mut self, buffer: &mut [f32], _rate: u32) {
                self.0.fetch_add(1, Ordering::Relaxed);
                for value in buffer {
                    *value += 0.1;
                }
            }
            fn set_param(&mut self, _: &str, _: f64) {}
            fn get_param(&self, _: &str) -> Option<f64> {
                None
            }
            fn reset(&mut self) {}
            fn name(&self) -> &str {
                "test counting bias"
            }
        }
        let project = oc_dsl::parse("@project\n@bus room\n  routes: [master]\n@track t | instrument: audio/clip\n  mix: send(room=50%)\n").unwrap();
        let track_calls = Arc::new(AtomicUsize::new(0));
        let bus_calls = Arc::new(AtomicUsize::new(0));
        let master_calls = Arc::new(AtomicUsize::new(0));
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 8_000, &[]).unwrap();
        graph.track_chains[0]
            .effects
            .push(Box::new(CountBias(track_calls.clone())));
        graph
            .bus_chains
            .get_mut("room")
            .unwrap()
            .chain
            .effects
            .push(Box::new(CountBias(bus_calls.clone())));
        graph
            .master_chain
            .effects
            .push(Box::new(CountBias(master_calls.clone())));
        let mut cache = RenderCache::new();
        cache.track_buffers = vec![Some([0.25, -0.25].repeat(128))];
        let mut capture = None;
        let (audio, _) = cache
            .mix_graph(
                &project,
                GraphMixOptions {
                    purpose: GraphMixPurpose::TrackWidths,
                    sample_rate: 8_000,
                    total_samples: 128,
                    timeline_start: 0,
                    selection: Some(&[0]),
                    window: None,
                    workers: Default::default(),
                },
                &mut capture,
                graph,
            )
            .unwrap();
        assert!(audio.is_empty());
        assert_eq!(cache.full_mix_widths, [SourceWidth::Stereo]);
        assert!(track_calls.load(Ordering::Relaxed) > 0);
        assert_eq!(bus_calls.load(Ordering::Relaxed), 0);
        assert_eq!(master_calls.load(Ordering::Relaxed), 0);
        assert!(cache.master_buffer().is_none());
    }
    #[test]
    fn width_witness_omits_unrelated_native_graph_preparation() {
        let project = oc_dsl::parse("@project\n@bus native\n  effects:\n    - plugin:\"vst3.test.unavailable\"\n@track t | instrument: audio/clip\n").unwrap();
        let mut prefix = RenderCache::new();
        prefix.track_buffers = vec![Some(vec![0.1; 128])];
        prefix.full_mix_widths = vec![SourceWidth::Mono];
        let mut cache = RenderCache::new();
        assert!(
            !cache
                .resolve_mono_widths(
                    &project,
                    Path::new("unused.sf2"),
                    8_000,
                    &mut HashMap::new(),
                    &prefix,
                    &[0],
                    Default::default()
                )
                .unwrap()
        );
        assert!(cache.master_buffer().is_none());
    }

    #[test]
    fn width_scope_matches_full_width_and_never_publishes_an_audio_cache() {
        let project = oc_dsl::parse("@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track a | preset: tone, instrument: synth/lead_square\n  @phrase p [1:1 - 2:1]\n    C4 w\n@track b | preset: tone, instrument: synth/lead_square\n  @phrase p [1:1 - 2:1]\n    G4 w\n").unwrap();
        let mut reference = RenderCache::new();
        reference
            .render_full(
                &project,
                Path::new("unused.sf2"),
                8_000,
                &mut HashMap::new(),
            )
            .unwrap();
        let mut cache = RenderCache::new();
        let (witness, audio) = cache
            .render_selection_scratch(
                &project,
                Path::new("unused.sf2"),
                8_000,
                &mut HashMap::new(),
                ScratchRenderPass {
                    purpose: ScratchPurpose::TrackWidths {
                        reference: &reference,
                    },
                    selected: &[0],
                    stems: false,
                    window_end: None,
                    control_only: &[],
                    workers: Default::default(),
                },
            )
            .unwrap();
        assert!(audio.is_empty());
        assert_eq!(witness.full_mix_widths[0], reference.full_mix_widths[0]);
        assert_eq!(
            witness.track_source_latencies[1],
            reference.track_source_latencies[1]
        );
        assert_eq!(
            witness.track_source_tails[1],
            reference.track_source_tails[1]
        );
        assert!(witness.track_buffers[1].as_ref().unwrap().is_empty());
        assert!(witness.master_buffer().is_none());
        assert_eq!(witness.render_horizon_samples, 0);
        assert!(witness.is_dirty());
        let token = crate::render_cancel::CancelToken::new();
        token.cancel();
        cache.set_cancel_token(token);
        cache.set_progress(Box::new(|_| {}));
        let result = cache.render_selection_scratch(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            ScratchRenderPass {
                purpose: ScratchPurpose::TrackWidths {
                    reference: &reference,
                },
                selected: &[0],
                stems: false,
                window_end: None,
                control_only: &[],
                workers: Default::default(),
            },
        );
        assert!(matches!(result, Err(error) if error == crate::render_cancel::CANCELLED_MESSAGE));
        assert!(cache.master_buffer().is_none());
        assert!(cache.progress.is_some());
    }
}
