//! Track-subset windows: post-track-FX snapshots and bounded graph passes.
//!
//! A `tracks` window is a slice of the same timeline, so it has to reproduce
//! every track, bus and master effect from frame 0. What it does not have to do
//! is re-synthesise the other tracks or re-run their chains: a warm cache keeps
//! the selected tracks' post-FX buffers as snapshots, and the graph pass stops
//! at the window end plus the master chain's latency.

use super::*;

/// Track buffers a window mixes instead of re-running their chains.
pub(super) struct WindowMix<'a> {
    /// Widths the cached full mix classified, so the pan law cannot depend on
    /// how much of a track the window happens to cover.
    pub(super) widths: Vec<SourceWidth>,
    /// Post-track-FX buffers keyed by track index; these skip the chain.
    pub(super) post_fx: &'a HashMap<usize, Vec<f32>>,
    /// Keep freshly processed buffers for the next window on the same tracks.
    pub(super) capture: bool,
}

impl RenderCache {
    /// Store one track's post-FX buffer, evicting least-recently-used tracks
    /// until it fits the byte budget. A buffer larger than the budget is dropped.
    pub(super) fn remember_post_fx_snapshot(&mut self, track: usize, samples: Vec<f32>) {
        if self.post_fx_budget_bytes == 0 || samples.is_empty() {
            return;
        }
        let bytes = samples.len() * std::mem::size_of::<f32>();
        if bytes > self.post_fx_budget_bytes {
            return;
        }
        if self.post_fx_snapshots.len() <= track {
            self.post_fx_snapshots.resize_with(track + 1, || None);
        }
        self.forget_post_fx_snapshot(track);
        while self.post_fx_bytes + bytes > self.post_fx_budget_bytes {
            let victim = self
                .post_fx_snapshots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| slot.as_ref().map(|snapshot| (index, snapshot.used)))
                .min_by_key(|(_, used)| *used)
                .map(|(index, _)| index);
            let Some(victim) = victim else { break };
            self.forget_post_fx_snapshot(victim);
        }
        self.post_fx_clock = self.post_fx_clock.wrapping_add(1);
        self.post_fx_snapshots[track] = Some(PostFxSnapshot {
            frames: samples.len() / 2,
            samples,
            used: self.post_fx_clock,
        });
        self.post_fx_bytes += bytes;
    }

    /// Drop one track's snapshot.
    pub(super) fn forget_post_fx_snapshot(&mut self, track: usize) {
        if let Some(slot) = self.post_fx_snapshots.get_mut(track)
            && let Some(previous) = slot.take()
        {
            self.post_fx_bytes = self
                .post_fx_bytes
                .saturating_sub(previous.samples.len() * std::mem::size_of::<f32>());
        }
    }

    /// Drop every snapshot (a full re-render invalidates all of them).
    pub(super) fn clear_post_fx_snapshots(&mut self) {
        self.post_fx_snapshots.clear();
        self.post_fx_bytes = 0;
    }

    /// Evict down to the current budget; `0` keeps nothing.
    pub(super) fn trim_post_fx_snapshots(&mut self) {
        while self.post_fx_bytes > self.post_fx_budget_bytes {
            let victim = self
                .post_fx_snapshots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| slot.as_ref().map(|snapshot| (index, snapshot.used)))
                .min_by_key(|(_, used)| *used)
                .map(|(index, _)| index);
            let Some(victim) = victim else { break };
            self.forget_post_fx_snapshot(victim);
        }
    }

    /// A track-subset window against a clean cache: reuse post-FX snapshots
    /// where they cover the window, otherwise re-run only the selected tracks'
    /// chains, and stop the graph pass at the window end.
    pub(super) fn render_subset_window(
        &mut self,
        project: &Project,
        sample_rate: u32,
        selected: &[usize],
        pass: RangeRenderPass<'_>,
    ) -> Result<RangeRenderOutput, String> {
        let RangeRenderPass {
            request,
            frames,
            workers,
        } = pass;
        let start = request.start_sample;
        let horizon = self.render_horizon_samples;
        // A window past the rendered timeline is silence; never re-mix for it.
        if start >= horizon as u64 && !request.stems {
            return Ok(RangeRenderOutput {
                audio: vec![0.0; frames * 2],
                stems: Vec::new(),
                sample_rate,
                frames,
                tracks: selected.to_vec(),
                // The clean cached horizon answered; nothing was rendered.
                cache_hit: true,
                window: Some(RangeWindowPath::Silent),
            });
        }
        let mix_end = usize::try_from(request.end_sample.min(horizon as u64))
            .map_err(|_| "range length exceeds platform size".to_string())?;
        let graph = self.graph_for(project, sample_rate, Some(selected))?;
        let layout = MixLayout::new(&graph, sample_rate, mix_end, &self.track_source_tails)?;
        // Post-FX frames that can still reach the output: the program plus the
        // master chain's latency.
        let needed = mix_end + layout.master_latency;

        // A track whose chain reads another track's signal only has a valid
        // snapshot when that source is selected too; otherwise recompute.
        let sources: Vec<Vec<usize>> = (0..project.tracks.len())
            .map(|index| {
                graph
                    .track_sidechain_sources(index)
                    .filter_map(|source| {
                        project.tracks.iter().position(|track| track.name == source)
                    })
                    .collect()
            })
            .collect();
        let mut post_fx = HashMap::new();
        for &index in selected {
            let Some(covered) = self.post_fx_snapshot_frames(index) else {
                continue;
            };
            if covered < needed {
                continue;
            }
            if sources
                .get(index)
                .is_some_and(|sources| sources.iter().any(|source| !selected.contains(source)))
            {
                continue;
            }
            if let Some(snapshot) = self.post_fx_snapshots.get_mut(index).and_then(Option::take) {
                self.post_fx_bytes = self
                    .post_fx_bytes
                    .saturating_sub(snapshot.samples.len() * std::mem::size_of::<f32>());
                post_fx.insert(index, snapshot.samples);
            }
        }
        let from_snapshots = post_fx.len() == selected.len();
        let window = WindowMix {
            widths: self.full_mix_widths.clone(),
            post_fx: &post_fx,
            capture: true,
        };
        let mut capture = request.stems.then(|| crate::graph::StemCapture {
            pre_fader_tracks: self.pre_fader_capture(),
            ..Default::default()
        });
        // The cached track buffers are the timeline from frame 0, so this pass
        // reads automation and clocks from the project start, not the window.
        let mixed = self.mix_graph(
            project,
            GraphMixOptions {
                sample_rate,
                total_samples: mix_end,
                timeline_start: 0,
                selection: Some(selected),
                window: Some(&window),
                workers,
            },
            &mut capture,
            graph,
        );
        // Restore snapshots only after the whole graph succeeded. A failed
        // source/insert/master must not repopulate the invalidated cache.
        let (master, _) = mixed?;
        for (index, samples) in post_fx {
            self.remember_post_fx_snapshot(index, samples);
        }
        let audio = slice_stereo_range(&master, start, frames);
        let mut stems = Vec::new();
        if let Some(capture) = capture {
            for (index, (name, buffer)) in capture.tracks.into_iter().enumerate() {
                stems.push((index, name, slice_stereo_range(&buffer, start, frames)));
            }
        }
        Ok(RangeRenderOutput {
            audio,
            stems,
            sample_rate,
            frames,
            tracks: selected.to_vec(),
            // The timeline came from the clean cached render: snapshots or the
            // cached source buffers. Only a cold cache re-synthesises sources.
            cache_hit: true,
            window: Some(if from_snapshots {
                RangeWindowPath::Snapshot
            } else {
                RangeWindowPath::Recomputed
            }),
        })
    }

    /// No clean full render: render only the selected tracks into a scratch
    /// cache and leave the caller's cache untouched. A window-bounded render is
    /// used when it provably matches the whole-program one; otherwise the
    /// selection is rendered over the whole program, which is still cheaper
    /// than rendering every track.
    pub(super) fn render_cold_subset(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        selected: &[usize],
        pass: RangeRenderPass<'_>,
    ) -> Result<RangeRenderOutput, String> {
        let RangeRenderPass {
            request,
            frames,
            workers,
        } = pass;
        let guard = (f64::from(sample_rate) * COLD_WINDOW_GUARD_SECONDS).round() as u64;
        let bounded_end = request.end_sample.saturating_add(guard);
        // Control feeds must be synthesised so the duck matches the full mix;
        // their own output is masked out of the pass.
        let control_only: Vec<usize> = project
            .properties
            .control_sources
            .iter()
            .filter_map(|source| {
                project
                    .tracks
                    .iter()
                    .position(|track| track.name == source.source_track)
            })
            .filter(|index| !selected.contains(index))
            .collect();
        let mut render_selection = selected.to_vec();
        render_selection.extend(control_only.iter().copied());
        let (first, master) = self.render_selection_scratch(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            ScratchRenderPass {
                selected: &render_selection,
                stems: request.stems,
                window_end: Some(bounded_end),
                control_only: &control_only,
                workers,
            },
        )?;
        // A bounded render is exact when no track's pan-law class can still
        // change past the window: a track classified stereo here is stereo in
        // the full render, and a silent one adds nothing to this window. The
        // guard must also cover the master chain's latency. A control feed that
        // is outside the window selection forces the whole program, because the
        // follower's state at the window start depends on the audio before it.
        let exact = control_only.is_empty()
            && u64::from(first.master_latency_samples) <= guard
            && selected.iter().all(|&index| {
                first.full_mix_widths.get(index).copied() == Some(SourceWidth::Stereo)
                    || first
                        .track_buffers
                        .get(index)
                        .and_then(Option::as_deref)
                        .is_none_or(is_silent)
            });
        let (scratch, master) = if exact {
            (first, master)
        } else {
            self.render_selection_scratch(
                project,
                soundfont_path,
                sample_rate,
                plugin_instances,
                ScratchRenderPass {
                    selected: &render_selection,
                    stems: request.stems,
                    window_end: None,
                    control_only: &control_only,
                    workers,
                },
            )?
        };
        let mut stems = Vec::new();
        if request.stems
            && let Some(capture) = scratch.stem_capture.as_ref()
        {
            for (index, (name, buffer)) in capture.tracks.iter().enumerate() {
                stems.push((
                    index,
                    name.clone(),
                    slice_stereo_range(buffer, request.start_sample, frames),
                ));
            }
        }
        Ok(RangeRenderOutput {
            audio: slice_stereo_range(&master, request.start_sample, frames),
            stems,
            sample_rate,
            frames,
            tracks: selected.to_vec(),
            cache_hit: false,
            window: Some(RangeWindowPath::Cold),
        })
    }

    /// Render only `selected` in a fresh cache; `window_end` bounds the program
    /// (a window), `None` keeps the whole program.
    fn render_selection_scratch(
        &self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        pass: ScratchRenderPass<'_>,
    ) -> Result<(RenderCache, Vec<f32>), String> {
        let ScratchRenderPass {
            selected,
            stems,
            window_end,
            control_only,
            workers,
        } = pass;
        let mut scratch = RenderCache::new();
        scratch.sample_rate = sample_rate;
        scratch.set_worker_options(workers);
        scratch.cancel = self.cancel.clone();
        scratch.control_only = control_only.to_vec();
        scratch.set_resource_root(self.resource_root.clone());
        scratch.set_pre_roll_seconds(self.pre_roll_seconds)?;
        scratch.set_pattern_previews(false);
        scratch.set_post_fx_snapshot_budget(0);
        if stems {
            // A pre-fader request keeps its tap point on the scratch pass.
            if self.pre_fader_capture() {
                scratch.capture_track_pre_fader();
            } else {
                scratch.capture_stems(true);
            }
        }
        let window = window_end.map(|end| RangeRenderRequest {
            start_sample: 0,
            end_sample: end,
            tracks: Some(selected.to_vec()),
            stems,
        });
        let scope = match &window {
            Some(request) => RenderScope::Window(request),
            None => RenderScope::Tracks(selected),
        };
        let master = match scratch.render_scoped(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RenderPass { scope, workers },
        ) {
            Ok(master) => master,
            // An empty project is a valid silent range, not a render error.
            Err(error) if error == "No events to render" => Vec::new(),
            Err(error) => return Err(error),
        };
        Ok((scratch, master))
    }
}

/// A track is silent for these purposes when its pre-FX buffer never leaves 0.
fn is_silent(buffer: &[f32]) -> bool {
    buffer.iter().all(|sample| *sample == 0.0)
}

/// Cold windows render a guard past the window end so the master chain's
/// latency cannot pull future source into the window.
const COLD_WINDOW_GUARD_SECONDS: f64 = 1.0;

#[cfg(test)]
mod worker_snapshot_tests {
    use super::*;

    #[test]
    fn scratch_pass_retains_request_workers_and_cancellation() {
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
@track lead | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 2:1]
    C4 q
"#,
        )
        .unwrap();
        let inherited = crate::render_parallel::RenderWorkerOptions::from_pin(Some("1"), || 8);
        let requested = crate::render_parallel::RenderWorkerOptions::from_pin(Some("3"), || 8);
        let mut cache = RenderCache::new();
        cache.set_worker_options(inherited);
        for window_end in [Some(2400), None] {
            let (scratch, audio) = cache
                .render_selection_scratch(
                    &project,
                    Path::new(""),
                    12_000,
                    &mut HashMap::new(),
                    ScratchRenderPass {
                        selected: &[0],
                        stems: false,
                        window_end,
                        control_only: &[],
                        workers: requested,
                    },
                )
                .unwrap();
            assert_eq!(scratch.worker_options, Some(requested));
            assert_eq!(cache.worker_options, Some(inherited));
            assert!(audio.iter().any(|sample| *sample != 0.0));
        }
        let token = crate::render_cancel::CancelToken::new();
        token.cancel();
        cache.set_cancel_token(token);
        let result = cache.render_selection_scratch(
            &project,
            Path::new(""),
            12_000,
            &mut HashMap::new(),
            ScratchRenderPass {
                selected: &[0],
                stems: false,
                window_end: Some(2400),
                control_only: &[],
                workers: requested,
            },
        );
        assert!(matches!(result, Err(error) if error == crate::render_cancel::CANCELLED_MESSAGE));
    }
}
