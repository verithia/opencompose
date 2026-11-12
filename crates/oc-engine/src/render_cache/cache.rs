//! Incremental render driver and mixing.

use super::*;

impl RenderCache {
    /// Incrementally render only dirty tracks, then remix.
    /// `plugin_instances`: live CLAP plugin instances keyed by preset name.
    /// Tracks with CLAP presets are rendered through their plugin instance.
    /// Other tracks are rendered through the SoundFont player.
    pub fn render_incremental(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
    ) -> Result<Vec<f32>, String> {
        let _timer = oc_diag::perf_counter("cache.incremental.ms").map(oc_diag::Timer::start);
        self.render_scoped(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RenderPass {
                scope: RenderScope::Full,
                workers: self.worker_options.unwrap_or_default(),
            },
        )
    }

    /// Re-hash every track's clips, including each clip's media stamp, and mark
    /// the tracks whose identity changed. A changed clip invalidates its track
    /// even when no caller set a dirty flag, so a replaced media file re-renders
    /// and its cached post-FX snapshot is dropped.
    pub(super) fn refresh_clip_fingerprints(&mut self, project: &Project, sample_rate: u32) {
        let count = project.tracks.len();
        if self.track_clip_fingerprints.len() < count {
            self.track_clip_fingerprints.resize(count, 0);
        }
        if self.track_dirty.len() < count {
            self.track_dirty.resize(count, true);
        }
        let root = self.resource_root.clone();
        for (index, track) in project.tracks.iter().enumerate() {
            let fingerprint = track_clip_fingerprint(project, track, sample_rate, root.as_deref());
            if self.track_clip_fingerprints[index] != fingerprint {
                self.track_clip_fingerprints[index] = fingerprint;
                self.mark_track_dirty(index);
            }
        }
    }

    pub(super) fn render_scoped(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        pass: RenderPass<'_>,
    ) -> Result<Vec<f32>, String> {
        // Retained live CLAP owners may have failed since the last successful
        // render. A clean master is not permission to bypass their authority.
        for instance in plugin_instances.values() {
            if let Err(error) = crate::processing_failure::effect(instance, self.cancel.as_ref()) {
                self.invalidate_failed_render();
                return Err(error.to_string());
            }
        }
        let result =
            self.render_scoped_inner(project, soundfont_path, sample_rate, plugin_instances, pass);
        if result.is_err() {
            self.invalidate_failed_render();
        }
        result
    }

    fn invalidate_failed_render(&mut self) {
        self.render_horizon_samples = 0;
        self.duration_seconds = 0.0;
        self.mark_all_dirty();
        self.master_buffer = None;
        self.track_buffers
            .iter_mut()
            .for_each(|buffer| *buffer = None);
        self.aux_buffers.clear();
        self.pattern_buffers.clear();
        self.pattern_dirty
            .values_mut()
            .for_each(|dirty| *dirty = true);
        if let Some(capture) = &mut self.stem_capture {
            clear_failed_capture(capture);
        }
    }

    fn render_scoped_inner(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        pass: RenderPass<'_>,
    ) -> Result<Vec<f32>, String> {
        project
            .properties
            .checked_automation_normalization()
            .map_err(|error| error.to_string())?;
        let RenderPass {
            scope,
            workers: worker_options,
        } = pass;
        let authored_presets = &project.presets;
        let resolved = oc_dsl::preset::resolve(project)?;
        let expanded = oc_dsl::arrangement::expand(&resolved)?;
        let project = &expanded;
        // Clone the flag once: every loop below can then poll it without
        // borrowing the cache field it is also mutating.
        let cancel = self.cancel.clone();
        if crate::render_cancel::stopped(cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }
        // A pattern's preset overrides the preset of every track that hosts it,
        // for that pattern's clips only. Each placement becomes an independent
        // source lane with its own instrument and inserts, and a track's
        // lanes are summed before its effects. The layout is part of the cache
        // identity: a changed split makes every cached source buffer stale.
        let lanes = crate::source_lanes::plan(project)?;
        let lane_project = lanes.tracks();
        super::clap_prepare::prepare_clap_render_owners(
            lane_project,
            plugin_instances,
            self.resource_root.as_deref(),
        )?;
        let targets = &lanes.targets;
        let num_tracks = project.tracks.len();
        let num_lanes = targets.len();
        let mut source_chains = lane_project
            .tracks
            .iter()
            .map(|lane| crate::source_lanes::effect_chain(project, lane, sample_rate))
            .collect::<Result<Vec<_>, _>>()?;
        self.track_source_tails = vec![0; num_tracks];
        for (lane, chain) in source_chains.iter().enumerate() {
            let target = targets[lane];
            self.track_source_tails[target] =
                self.track_source_tails[target].max(chain.tail_samples());
        }
        let progress_total = num_tracks.max(1);
        let mut progress_done = 0usize;
        if self.lane_targets != *targets {
            self.lane_targets.clone_from(targets);
            self.mark_all_dirty();
            self.aux_buffers.clear();
        }
        if self.sample_rate != sample_rate {
            self.mark_all_dirty();
        }
        self.sample_rate = sample_rate;

        let mut stamps = HashMap::new();
        for track in &lane_project.tracks {
            let preset = lane_project
                .presets
                .iter()
                .find(|p| Some(&p.name) == track.preset.as_ref());
            let params = crate::preset::builtin_track_params_untrimmed(lane_project, preset, track);
            if preset.and_then(|p| p.plugin.as_deref()) == Some(oc_synth::audio_region::PLUGIN_ID) {
                let (document, path) =
                    oc_synth::audio_region::load_document(&params, self.resource_root.as_deref())?;
                stamps.insert(path.clone(), native_state_stamp(&path)?);
                for file in document.media_files()? {
                    let media = path.parent().ok_or("missing region root")?.join(file);
                    stamps.insert(media.clone(), native_state_stamp(&media)?);
                }
            }
            for (_, value) in params.iter().filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "open_synth_state_file"
                        | "wavetable.file"
                        | "sampler.sfz_file"
                        | "sampler.sample_file"
                )
            }) {
                let oc_dsl::ast::ParamValue::String(value) = value else {
                    return Err("native synth resource must be a path string".into());
                };
                let path = Path::new(value);
                let resolved = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.resource_root
                        .as_ref()
                        .ok_or("relative OpenSynth state requires project resource root")?
                        .join(path)
                };
                stamps.insert(resolved.clone(), native_state_stamp(&resolved)?);
            }
            if preset.and_then(|p| p.plugin.as_deref()) == Some(oc_synth::drumkit::PLUGIN_ID) {
                for (_, value) in params
                    .iter()
                    .filter(|(name, _)| name.starts_with("layer.") && name.ends_with(".sample"))
                {
                    let oc_dsl::ast::ParamValue::String(value) = value else {
                        continue;
                    };
                    if value.is_empty() {
                        continue;
                    }
                    let path = Path::new(value);
                    let resolved = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        self.resource_root
                            .as_ref()
                            .ok_or("relative drum-kit sample requires project resource root")?
                            .join(path)
                    };
                    // A missing sample is reported by the instrument and stays silent.
                    if resolved.is_file() {
                        stamps.insert(resolved.clone(), native_state_stamp(&resolved)?);
                    }
                }
            }
        }
        if stamps != self.native_state_stamps {
            self.mark_all_dirty();
            self.native_state_stamps = stamps;
        }

        crate::render::validate_gain_project(project)?;
        // A clean source buffer must not hide an invalid instrument edit.
        for track in &lane_project.tracks {
            if let Some(preset) = track
                .preset
                .as_deref()
                .and_then(|name| lane_project.presets.iter().find(|p| p.name == name))
                && (preset.source.as_deref() == Some("builtin")
                    || preset
                        .plugin
                        .as_deref()
                        .is_some_and(|id| id.starts_with("builtin.")))
            {
                validate_builtin_plugin_id(preset.plugin.as_deref().unwrap_or("builtin.test_tone"))
                    .map_err(|error| {
                        format!(
                            "Preset '{}' on track '{}': {error}",
                            preset.name, track.name
                        )
                    })?;
            }
        }

        // Never substitute a different instrument when the caller failed to
        // instantiate a declared CLAP preset. Check before discovery or audio
        // allocation so this failure is deterministic even without a registry.
        for track in &lane_project.tracks {
            if let Some(preset) = lane_project
                .presets
                .iter()
                .find(|p| Some(&p.name) == track.preset.as_ref())
                && preset.source.as_deref() == Some("clap")
                && !plugin_instances.contains_key(&preset.name)
            {
                return Err(format!(
                    "CLAP preset '{}' has no loaded instance; refusing SoundFont fallback",
                    preset.name
                ));
            }
        }
        if let Some(error) = oc_host::plugin_preset_render_error(lane_project) {
            return Err(error);
        }
        validate_aux_sends(lane_project)?;

        // Resize track arrays if project changed
        while self.track_buffers.len() < num_tracks {
            self.track_buffers.push(None);
            self.track_dirty.push(true);
            self.track_source_latencies.push(0);
        }
        // Trim if tracks were removed
        self.track_buffers.truncate(num_tracks);
        self.track_dirty.truncate(num_tracks);
        self.track_source_latencies.truncate(num_tracks);
        self.track_clip_fingerprints.truncate(num_tracks);
        self.track_ramp_fingerprints.truncate(num_tracks);
        self.aux_pairs.truncate(num_tracks);
        self.aux_buffers.retain(|(track, _), _| *track < num_tracks);

        // A changed clip invalidates its track even when no dirty flag was set.
        self.refresh_clip_fingerprints(project, sample_rate);

        // A changed hairpin changes the sampler's layer mix, so its track must
        // re-render even when no caller cleared a dirty flag.
        while self.track_ramp_fingerprints.len() < num_tracks {
            self.track_ramp_fingerprints.push(0);
        }
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
        for (index, track) in project.tracks.iter().enumerate() {
            let fingerprint = track_ramp_fingerprint(project, track, &clock)?;
            if self.track_ramp_fingerprints[index] != fingerprint {
                self.track_ramp_fingerprints[index] = fingerprint;
                self.track_dirty[index] = true;
            }
        }

        // A changed aux-output request needs a fresh capture from that plugin.
        let aux_pairs = aux_pairs_by_track(project);
        for (index, (previous, current)) in self.aux_pairs.iter().zip(&aux_pairs).enumerate() {
            if previous != current {
                self.track_dirty[index] = true;
            }
        }
        for index in self.aux_pairs.len()..aux_pairs.len() {
            self.track_dirty[index] = true;
        }
        self.aux_pairs = aux_pairs;

        // Build reverse map: pattern name → track indices that use it via clips
        self.pattern_to_tracks.clear();
        for (i, track) in project.tracks.iter().enumerate() {
            for clip in track.pattern_clips() {
                self.pattern_to_tracks
                    .entry(clip.pattern.clone())
                    .or_default()
                    .push(i);
            }
        }

        // Determine total duration from all events
        let interpret_start = std::time::Instant::now();
        let settings = InterpretSettings {
            sample_rate,
            ..Default::default()
        };
        let mut performance =
            prepare_performance_tracks(lane_project, sample_rate, plugin_instances)?;
        // The legacy interpreter remains fail-closed for callers without a
        // faithful backend. Only this checked path separates representations.
        let note_project = note_source_project(lane_project);
        let mut all_events = interpret::try_interpret_render_at(
            &note_project,
            &settings,
            self.resource_root.as_deref(),
        )?;
        // Remove ALL generated defaults for explicit performance tracks.
        all_events.retain(|event| performance[event.track_index].is_none());
        if let Some(tracks) = scope.tracks() {
            for (lane, slot) in performance.iter_mut().enumerate() {
                if !tracks.contains(&targets[lane]) {
                    *slot = None;
                }
            }
        }
        if let Some(scope) = scope.window() {
            let (start, end) = (scope.start_sample, scope.end_sample);
            for commands in performance.iter_mut().flatten() {
                commands.retain(|command| command.sample >= start && command.sample < end);
                for command in commands.iter_mut() {
                    command.sample -= start;
                }
            }
            all_events.retain(|event| event.time_samples >= start && event.time_samples < end);
            for event in &mut all_events {
                event.time_samples -= start;
            }
        }
        // A window shifts its events to frame 0, so anything that reads the
        // timeline (automation, tempo clocks, plugin transport) must be told
        // where the window starts.
        let timeline_start = scope.window().map_or(0, |window| window.start_sample);
        let performance_end = performance
            .iter()
            .flatten()
            .flat_map(|events| events.iter())
            .map(|event| event.sample)
            .max();
        crate::stage_log::push("interpret", interpret_start);
        // Interpret/check timing before allocating any preview audio.
        let t0 = std::time::Instant::now();
        if scope.is_full() && self.pattern_previews {
            let preview_start = std::time::Instant::now();
            self.render_patterns(
                project,
                soundfont_path,
                sample_rate,
                project.properties.tempo,
            )?;
            crate::stage_log::push("pattern previews", preview_start);
        }
        log::info!("Pattern render took {:.2}s", t0.elapsed().as_secs_f64());
        let mut audio_end = crate::audio_clip::project_end_frame(project, sample_rate)?;
        for track in &lane_project.tracks {
            let preset = lane_project
                .presets
                .iter()
                .find(|p| Some(&p.name) == track.preset.as_ref());
            if preset.and_then(|p| p.plugin.as_deref()) == Some(oc_synth::audio_region::PLUGIN_ID) {
                let (document, _) = oc_synth::audio_region::load_document(
                    &crate::preset::builtin_track_params_untrimmed(lane_project, preset, track),
                    self.resource_root.as_deref(),
                )?;
                audio_end = audio_end.max(document.end_frame(sample_rate)?);
            }
        }
        let arrangement_end =
            oc_dsl::arrangement::rendered_end_sample(project, sample_rate)?.unwrap_or(0);
        if all_events.is_empty()
            && audio_end == 0
            && performance_end.is_none()
            && arrangement_end == 0
            && scope.is_full()
        {
            self.render_horizon_samples = 0;
            return Err("No events to render".into());
        }
        let total_frames = if let Some(scope) = scope.window() {
            scope.end_sample - scope.start_sample
        } else {
            let last_event = all_events
                .iter()
                .map(|e| e.time_samples)
                .max()
                .unwrap_or(0)
                .max(audio_end)
                .max(performance_end.unwrap_or(0));
            let declared_tail = project_instrument_tail_samples(
                lane_project,
                authored_presets,
                plugin_instances,
                sample_rate,
                self.resource_root.as_deref(),
                InstrumentTailProbes {
                    vst3: self.vst3_tail_probe,
                    vst2: self.vst2_tail_probe,
                },
            )?;
            let fallback_tail = u64::from(sample_rate) * u64::from(DEFAULT_RENDER_TAIL_SECONDS);
            let tail = declared_tail.max(fallback_tail);
            last_event
                .checked_add(tail)
                .ok_or_else(|| "instrument tail render length overflow".to_string())?
                .max((self.minimum_duration_seconds * f64::from(sample_rate)).ceil() as u64)
                .max(arrangement_end)
        };
        self.duration_seconds = total_frames as f64 / sample_rate as f64;
        let total_samples = usize::try_from(total_frames)
            .map_err(|_| "instrument tail render length exceeds platform size".to_string())?;

        // Split events by source lane. Interpretation runs over the lane
        // project, so its track index is already a lane index.
        let mut events_by_track: Vec<Vec<&MidiEvent>> = vec![Vec::new(); num_lanes];
        for event in &all_events {
            if event.track_index < num_lanes {
                events_by_track[event.track_index].push(event);
            }
        }
        if let Some(tracks) = scope.tracks() {
            for (lane, events) in events_by_track.iter_mut().enumerate() {
                if !tracks.contains(&targets[lane]) {
                    events.clear();
                }
            }
        }

        // Tracks referencing the same external preset share one stateful plugin
        // instance. If any member changes, every output pair from that instance
        // must be regenerated from the same transport/state trajectory. A
        // pattern preset forms its own lane, so the verdict is taken on lanes
        // and folded back onto the hosting track.
        let mut lane_dirty: Vec<bool> = targets
            .iter()
            .map(|&track| self.track_dirty[track])
            .collect();
        propagate_shared_plugin_dirty(lane_project, &mut lane_dirty);
        for (lane, &track) in targets.iter().enumerate() {
            if lane_dirty[lane] {
                self.track_dirty[track] = true;
            }
        }
        if let Some(tracks) = scope.tracks() {
            for index in 0..num_tracks {
                if tracks.contains(&index) {
                    self.track_dirty[index] = true;
                } else {
                    self.track_buffers[index] = Some(vec![0.0; total_samples * 2]);
                    self.track_source_latencies[index] = 0;
                    self.track_dirty[index] = false;
                }
            }
        }
        let lane_dirty: Vec<bool> = targets
            .iter()
            .map(|&track| self.track_dirty[track])
            .collect();

        // Re-render dirty tracks
        let dirty_count: usize = self.track_dirty.iter().filter(|&&d| d).count();
        #[cfg(any(test, feature = "test-support"))]
        {
            if scope.is_full() {
                self.counters.full_render_calls += 1;
            }
            self.counters.source_renders += lane_dirty.iter().filter(|dirty| **dirty).count();
        }
        if dirty_count > 0 {
            log::info!(
                "Incremental render: {}/{} tracks dirty, duration={:.1}s, total_samples={}",
                dirty_count,
                num_tracks,
                self.duration_seconds,
                total_samples,
            );
        }
        let render_start = std::time::Instant::now();

        // Dirty tracks render into a fresh buffer; a clip-only or emptied track
        // must not accumulate on top of its previous render.
        for (index, dirty) in self.track_dirty.iter().enumerate() {
            if *dirty {
                self.track_buffers[index] = None;
                self.track_source_latencies[index] = 0;
            }
        }

        // Sources render per lane; a track's lanes are summed into its cached
        // buffer once every lane has rendered.
        let mut lane_buffers: Vec<Option<Vec<f32>>> = vec![None; num_lanes];
        let mut lane_latencies: Vec<u32> = vec![0; num_lanes];
        let mut lane_aux: AuxOutputBuffers = HashMap::new();

        // Collect which tracks need a SoundFont renderer vs CLAP vs builtin
        let mut soundfont_tracks: Vec<usize> = Vec::new();
        let mut clap_tracks: Vec<usize> = Vec::new();
        let mut vst3_tracks: Vec<usize> = Vec::new();
        let mut vst2_tracks: Vec<usize> = Vec::new();
        let mut au_tracks: Vec<usize> = Vec::new();
        let mut lv2_tracks: Vec<usize> = Vec::new();
        let mut builtin_tracks: Vec<usize> = Vec::new();

        for (i, track) in lane_project.tracks.iter().enumerate() {
            if !lane_dirty[i] {
                continue;
            }

            let preset_name = track.preset.as_deref();
            let preset =
                preset_name.and_then(|n| lane_project.presets.iter().find(|p| p.name == n));
            let source = preset.and_then(|p| p.source.as_deref());
            let plugin_id = preset.and_then(|p| p.plugin.as_deref());

            let is_clap = source == Some("clap")
                && plugin_id
                    .map(|id| !id.starts_with("builtin."))
                    .unwrap_or(false);
            let is_vst3 = source == Some("vst3") || preset.is_some_and(is_sfizz_sample_preset);
            let is_vst2 = source == Some("vst2");
            let is_au = source == Some("au");
            let is_lv2 = source == Some("lv2")
                && plugin_id
                    .map(|id| !id.starts_with("builtin."))
                    .unwrap_or(false);
            let is_builtin = source == Some("builtin")
                || plugin_id
                    .map(|id| id.starts_with("builtin."))
                    .unwrap_or(false);
            let has_clap_instance = preset_name
                .map(|n| plugin_instances.contains_key(n))
                .unwrap_or(false);

            // A shared multi-output track may intentionally contain no MIDI:
            // it still captures a pair driven by another track in its group.
            let has_audio_clips = project
                .audio_clips()
                .iter()
                .any(|clip| clip.track == track.name && !clip.muted);
            let has_musical_events = events_by_track[i].iter().any(|event| {
                matches!(
                    event.event,
                    MidiMessage::NoteOn { .. }
                        | MidiMessage::NoteOff { .. }
                        | MidiMessage::NativeVoice(_)
                )
            });
            if !(has_musical_events
                || performance[i].is_some()
                || has_audio_clips
                || is_clap
                || is_vst3
                || is_vst2
                || is_au
                || is_lv2
                || plugin_id == Some(oc_synth::audio_region::PLUGIN_ID))
            {
                continue;
            }
            // Clip-only tracks need no instrument; the zero buffer is filled below.
            if !has_musical_events
                && performance[i].is_none()
                && has_audio_clips
                && !(is_clap
                    || is_vst3
                    || is_vst2
                    || is_au
                    || is_lv2
                    || plugin_id == Some(oc_synth::audio_region::PLUGIN_ID))
            {
                continue;
            }

            if is_clap && has_clap_instance {
                clap_tracks.push(i);
            } else if is_clap {
                return Err(format!(
                    "CLAP preset '{}' has no loaded instance; refusing SoundFont fallback",
                    preset_name.unwrap_or("")
                ));
            } else if is_vst3 {
                vst3_tracks.push(i);
            } else if is_vst2 {
                vst2_tracks.push(i);
            } else if is_au {
                au_tracks.push(i);
            } else if is_lv2 {
                lv2_tracks.push(i);
            } else if is_builtin {
                builtin_tracks.push(i);
            } else {
                soundfont_tracks.push(i);
            }
        }

        // Build parallel render jobs for SoundFont tracks
        let mut font_groups = std::collections::BTreeMap::<PathBuf, Vec<usize>>::new();
        for i in soundfont_tracks {
            let preset = lane_project.tracks[i]
                .preset
                .as_ref()
                .and_then(|name| lane_project.presets.iter().find(|p| &p.name == name));
            let declared = preset.and_then(|p| p.file.as_deref());
            let path = if let Some(file) = declared {
                let path = Path::new(file);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.resource_root
                        .as_deref()
                        .ok_or("Relative SoundFont requires project resource root")?
                        .join(path)
                }
            } else {
                soundfont_path.to_path_buf()
            };
            let track = &lane_project.tracks[i];
            if path.as_os_str().is_empty() {
                return Err(format!(
                    "Track '{}' needs a SoundFont: declare preset.file in the project or supply a legacy --soundfont fallback",
                    track.name
                ));
            }
            // A declared file that is not there is a project defect, not an I/O
            // accident: name the track, the preset and the path it wanted.
            if !path.is_file() {
                return Err(match declared {
                    Some(file) => {
                        // A relative declaration only tells the reader where to
                        // look once it is resolved against the project root.
                        let looked_for = if path == Path::new(file) {
                            String::new()
                        } else {
                            format!(" (looked for '{}')", path.display())
                        };
                        format!(
                            "Track '{}' needs a SoundFont: preset '{}' names file '{}', which is not a readable file{looked_for}. Add that file, or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
                            track.name,
                            track.preset.as_deref().unwrap_or(""),
                            file,
                        )
                    }
                    None => format!(
                        "Track '{}' needs a SoundFont: the --soundfont fallback '{}' is not a readable file. Add that file, or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
                        track.name,
                        path.display()
                    ),
                });
            }
            font_groups.entry(path).or_default().push(i);
        }
        for (font_path, soundfont_tracks) in font_groups {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            let pool = RenderPool::with_options(font_path, sample_rate, worker_options);
            let tail_seconds = 2.0; // reverb/release tail
            let jobs: Vec<_> = soundfont_tracks
                .iter()
                .map(|&i| {
                    let track = &lane_project.tracks[i];
                    let preset = track.preset.as_deref().and_then(|name| {
                        lane_project
                            .presets
                            .iter()
                            .find(|preset| preset.name == name)
                    });
                    let dynamic_layers = crate::preset::dynamic_layers_enabled(preset, track);
                    // A hairpin's layer crossfade reads the same timeline the
                    // gain clip does, shifted with a window's events.
                    let dynamic_ramps = if dynamic_layers {
                        crate::soundfont::dynamic_layer_ramps(
                            lane_project,
                            track,
                            &lane_project.properties,
                            sample_rate,
                            timeline_start,
                        )?
                    } else {
                        Vec::new()
                    };
                    let is_drum = track.instrument.category == "drums";
                    let (output_gain_db, output_pan) = crate::soundfont::preset_output(preset);
                    // Per-track duration: last event in this track + tail.
                    // This avoids rendering 54s of silence for a track with 3s of content.
                    let track_last_event = events_by_track[i]
                        .iter()
                        .map(|e| e.time_samples)
                        .max()
                        .unwrap_or(0);
                    let track_duration =
                        track_last_event as f64 / sample_rate as f64 + tail_seconds;
                    Ok(crate::render_parallel::TrackRenderJob {
                        track_index: i,
                        events: events_by_track[i].iter().map(|e| (*e).clone()).collect(),
                        total_duration_seconds: track_duration,
                        is_drum_track: is_drum,
                        dynamic_ramps,
                        dynamic_layers,
                        output_gain_db,
                        output_pan,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;

            let source_start = std::time::Instant::now();
            let rendered = pool.render_tracks_with_cancel(jobs, cancel.as_ref())?;
            crate::stage_log::push("soundfont tracks", source_start);
            log::info!(
                "Parallel render of {} SoundFont tracks took {:.2}s",
                soundfont_tracks.len(),
                render_start.elapsed().as_secs_f64()
            );

            for (&lane, mut buf) in soundfont_tracks.iter().zip(rendered) {
                // Pad to full project length for mixing (AudioGraph needs equal-length buffers)
                buf.resize(total_samples * 2, 0.0);
                lane_buffers[lane] = Some(buf);
                lane_latencies[lane] = 0;
            }
            progress_done += soundfont_tracks.len();
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
        }

        // Render each shared CLAP instrument once and fan its requested output
        // pairs into the independently routable track buffers.
        for index in 0..num_tracks {
            if self.track_dirty[index] {
                self.aux_buffers.retain(|(track, _), _| *track != index);
            }
        }
        for group in group_tracks_by_preset(lane_project, &clap_tracks) {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            progress_done = (progress_done + group.len()).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
            let first = group[0];
            let preset_name = lane_project.tracks[first].preset.as_deref().unwrap();
            let instance = plugin_instances.get_mut(preset_name).unwrap();
            // Keep authored GUI state separate from performance-induced state.
            // reset() alone does not reliably reset MIDI controller history.
            let mut offline = if performance[first].is_some() {
                Some(instance.fork_for_offline_render()?)
            } else {
                None
            };
            let instance = offline.as_mut().unwrap_or(instance);
            let ports = instance.output_ports();
            let pairs = resolve_group_pairs(
                lane_project,
                &group,
                "CLAP",
                preset_name,
                sample_rate,
                &ports,
                self.resource_root.as_deref(),
            )?;
            let event_groups: Vec<_> = group
                .iter()
                .map(|&i| events_by_track[i].as_slice())
                .collect();
            let aux_pairs: Vec<&[usize]> = group
                .iter()
                .map(|&i| self.aux_pairs[targets[i]].as_slice())
                .collect();
            let (buffers, aux_buffers) = render_tracks_via_clap(
                instance,
                &group,
                &event_groups,
                total_samples,
                sample_rate,
                project_transport(project, timeline_start, sample_rate),
                &pairs,
                &aux_pairs,
                performance[first].as_deref(),
                self.pre_roll_seconds,
            )?;
            let latency = instance.latency_samples();
            for (&lane, buffer) in group.iter().zip(buffers) {
                lane_latencies[lane] = latency;
                lane_buffers[lane] = Some(buffer);
            }
            for (key, buffer) in aux_buffers {
                lane_aux.insert(key, buffer);
            }
        }

        // Render each shared VST3 instrument once from its explicitly selected
        // bundle, then fan its real output pairs into track buffers.
        for group in group_tracks_by_preset(lane_project, &vst3_tracks) {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            progress_done = (progress_done + group.len()).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
            let first = group[0];
            let preset_name = lane_project.tracks[first].preset.as_deref().unwrap();
            let preset = lane_project
                .presets
                .iter()
                .find(|p| p.name == preset_name)
                .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;
            let mut request = super::vst3_tail::instrument_request(
                authored_presets,
                preset,
                &lane_project.tracks[first],
                sample_rate,
                self.resource_root.as_deref(),
            )?;
            for &index in group.iter().skip(1) {
                let other = super::vst3_tail::instrument_request(
                    authored_presets,
                    preset,
                    &lane_project.tracks[index],
                    sample_rate,
                    self.resource_root.as_deref(),
                )?;
                request.merge_shared(other).map_err(|error|format!(
                    "VST3 shared preset '{}' has incompatible initialization on tracks '{}' and '{}': {error}",
                    preset_name,lane_project.tracks[first].name,lane_project.tracks[index].name))?;
            }
            let mut instance = request
                .execute()
                .map_err(|error| format!("VST3 instrument preset '{preset_name}': {error}"))?;
            let ports = &instance.snapshot.outputs;
            let pairs = resolve_group_pairs(
                lane_project,
                &group,
                "VST3",
                preset_name,
                sample_rate,
                ports,
                self.resource_root.as_deref(),
            )?;
            let profiles: Vec<_> = group
                .iter()
                .map(|&i| {
                    instrument_profile_for_track_at(
                        lane_project,
                        i,
                        sample_rate,
                        self.resource_root.as_deref(),
                    )
                })
                .collect();
            let event_groups: Vec<_> = group
                .iter()
                .map(|&i| events_by_track[i].as_slice())
                .collect();
            let aux_pairs: Vec<&[usize]> = group
                .iter()
                .map(|&i| self.aux_pairs[targets[i]].as_slice())
                .collect();
            let (buffers, aux_buffers) = render_tracks_via_vst3(
                &mut instance,
                &Vst3RenderGroup {
                    track_indices: &group,
                    event_groups: &event_groups,
                    profiles: &profiles,
                    pairs: &pairs,
                    aux_pairs: &aux_pairs,
                },
                Vst3GroupRenderOptions {
                    total_samples,
                    transport: project_transport(project, timeline_start, sample_rate),
                    performance: performance[first].as_deref(),
                    pre_roll_seconds: self.pre_roll_seconds,
                    cancel: cancel.as_ref(),
                },
            )?;
            let latency = instance.snapshot.latency_samples;
            for (&lane, buffer) in group.iter().zip(buffers) {
                lane_latencies[lane] = latency;
                lane_buffers[lane] = Some(buffer);
            }
            for (key, buffer) in aux_buffers {
                lane_aux.insert(key, buffer);
            }
        }

        // VST2 instruments render in process. The interface has one stereo
        // output pair, so every track in the group reads the same pair.
        for group in group_tracks_by_preset(lane_project, &vst2_tracks) {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            progress_done = (progress_done + group.len()).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
            let first = group[0];
            let preset_name = lane_project.tracks[first].preset.as_deref().unwrap();
            let preset = lane_project
                .presets
                .iter()
                .find(|p| p.name == preset_name)
                .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;
            let plugin = super::vst2_group::prepare_plugin(
                authored_presets,
                lane_project,
                preset,
                &group,
                sample_rate,
                self.resource_root.as_deref(),
            )?;
            if let Some(pair) = group
                .iter()
                .flat_map(|&index| self.aux_pairs[targets[index]].iter())
                .find(|pair| **pair > 0)
            {
                return Err(format!(
                    "VST2 preset '{}' exposes only the main stereo pair; output pair {} was requested",
                    preset.name,
                    pair + 1
                ));
            }
            let event_groups: Vec<_> = group
                .iter()
                .map(|&index| events_by_track[index].as_slice())
                .collect();
            let rendered = (self.vst2_group_renderer)(super::vst2_group::Vst2GroupRequest {
                plugin,
                preset,
                tracks: &group,
                events: &event_groups,
                performance: performance[first].as_deref(),
                total_frames: total_samples,
                transport: project_transport(project, timeline_start, sample_rate),
                pre_roll_seconds: self.pre_roll_seconds,
                cancel: cancel.as_ref(),
            })?;
            let latency = rendered.latency;
            let buffers = rendered.buffers;
            for (&lane, buffer) in group.iter().zip(buffers) {
                lane_latencies[lane] = latency;
                lane_buffers[lane] = Some(buffer);
            }
        }

        // AudioUnit instruments render in process on macOS. A unit that cannot
        // initialize keeps the scan-only path: its tracks stay silent and the
        // reason is reported instead of aborting the whole export.
        #[cfg(target_os = "macos")]
        for group in group_tracks_by_preset(lane_project, &au_tracks) {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            progress_done = (progress_done + group.len()).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
            let first = group[0];
            let preset_name = lane_project.tracks[first].preset.as_deref().unwrap();
            let preset = lane_project
                .presets
                .iter()
                .find(|p| p.name == preset_name)
                .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;
            let plugin_id = preset
                .plugin
                .as_deref()
                .ok_or_else(|| format!("AU preset '{}' has no plugin id", preset.name))?;
            require_main_pair_only(
                lane_project,
                &group,
                "AudioUnit",
                &preset.name,
                sample_rate,
                self.resource_root.as_deref(),
            )?;
            if let Some(pair) = group
                .iter()
                .flat_map(|&index| self.aux_pairs[targets[index]].iter())
                .find(|pair| **pair > 0)
            {
                return Err(format!(
                    "AudioUnit preset '{}' exposes only the main stereo pair; output pair {} was requested",
                    preset.name,
                    pair + 1
                ));
            }
            let mut instance = match AuInstance::load(plugin_id, sample_rate, 512) {
                Ok(instance) => instance,
                Err(error) => {
                    crate::render_warnings::push(format!(
                        "AU preset '{}' used by track '{}' cannot render in process: {error}. Its tracks render silence",
                        preset.name, lane_project.tracks[first].name
                    ));
                    continue;
                }
            };
            let context = PluginStateContext::new(PluginFormat::Au, Some(plugin_id), &preset.name);
            if let Err(error) = apply_au_preset_state_at(
                &mut instance,
                &preset.params,
                &context,
                self.resource_root.as_deref(),
            ) {
                crate::render_warnings::push(format!(
                    "AU preset '{}' keeps the unit's default state: {error}",
                    preset.name
                ));
            }
            apply_au_preset_params(&mut instance, &preset.params)?;
            if (!lane_project.properties.tempo_map.is_empty()
                || !lane_project.properties.time_sig_map.is_empty())
                && !instance.host_callbacks_installed()
            {
                crate::render_warnings::push(format!(
                    "AU component '{}' does not accept host tempo callbacks; its tempo-synced internals keep a constant context",
                    instance.component_name()
                ));
            }
            let event_groups: Vec<_> = group
                .iter()
                .map(|&index| events_by_track[index].as_slice())
                .collect();
            let buffers = render_tracks_via_au(
                &mut instance,
                &group,
                &event_groups,
                total_samples,
                project_transport(project, timeline_start, sample_rate),
                self.pre_roll_seconds,
            )?;
            let latency = instance.latency_samples();
            for (&lane, buffer) in group.iter().zip(buffers) {
                lane_latencies[lane] = latency;
                lane_buffers[lane] = Some(buffer);
            }
        }

        // LV2 instruments are instantiated per track and are stereo only.
        for &lane in &lv2_tracks {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            progress_done = (progress_done + 1).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
            let group = [lane];
            let preset_name = lane_project.tracks[lane].preset.as_deref().unwrap();
            let preset = lane_project
                .presets
                .iter()
                .find(|p| p.name == preset_name)
                .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;
            let plugin_id = preset
                .plugin
                .as_deref()
                .ok_or_else(|| format!("LV2 preset '{}' has no plugin id", preset.name))?;
            require_main_pair_only(
                lane_project,
                &group,
                "LV2",
                &preset.name,
                sample_rate,
                self.resource_root.as_deref(),
            )?;
            if let Some(pair) = group
                .iter()
                .flat_map(|&index| self.aux_pairs[index].iter())
                .find(|pair| **pair > 0)
            {
                return Err(format!(
                    "LV2 preset '{}' exposes only the main stereo pair; output pair {} was requested",
                    preset.name,
                    pair + 1
                ));
            }
            let info = find_plugin_info(plugin_id, PluginFormat::Lv2).ok_or_else(|| {
                format!(
                    "LV2 plugin '{}' for preset '{}' was not found",
                    plugin_id, preset.name
                )
            })?;
            let mut instance = oc_host::lv2::Lv2Instance::load(
                std::path::Path::new(&info.path),
                &info.id,
                sample_rate,
                512,
            )
            .map_err(|error| format!("LV2 preset '{}': {error}", preset.name))?;
            let context =
                PluginStateContext::new(PluginFormat::Lv2, Some(info.id.as_str()), &preset.name);
            apply_lv2_preset_state_at(
                &mut instance,
                &preset.params,
                &context,
                self.resource_root.as_deref(),
            )?;
            apply_lv2_preset_params(&mut instance, &preset.params)?;
            let event_groups: Vec<_> = group
                .iter()
                .map(|&index| events_by_track[index].as_slice())
                .collect();
            let buffers = render_tracks_via_lv2(
                &mut instance,
                &group,
                &event_groups,
                total_samples,
                project_transport(project, timeline_start, sample_rate),
                self.pre_roll_seconds,
            )?;
            let latency = instance.latency_samples();
            for (&lane, buffer) in group.iter().zip(buffers) {
                lane_latencies[lane] = latency;
                lane_buffers[lane] = Some(buffer);
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            if !au_tracks.is_empty() {
                crate::render_warnings::push(
                    "AudioUnit hosting requires macOS; tracks that use an AU preset render silence",
                );
            }
        }

        // Built-in synth tracks are independent; render them on all cores.
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
        if !builtin_tracks.is_empty() {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            let jobs: Vec<BuiltinTrackJob> = builtin_tracks
                .iter()
                .map(|&i| {
                    let track = &lane_project.tracks[i];
                    let preset = track
                        .preset
                        .as_deref()
                        .and_then(|n| lane_project.presets.iter().find(|p| p.name == n));
                    BuiltinTrackJob {
                        index: i,
                        preset_name: preset.map_or("", |p| p.name.as_str()).to_owned(),
                        plugin_id: preset
                            .and_then(|p| p.plugin.as_deref())
                            .unwrap_or("builtin.test_tone")
                            .to_string(),
                        params: crate::preset::builtin_track_params_at(
                            lane_project,
                            preset,
                            track,
                            self.resource_root.as_deref(),
                        ),
                        events: events_by_track[i].iter().map(|e| (*e).clone()).collect(),
                        performance: performance[i].clone(),
                    }
                })
                .collect();
            let source_start = std::time::Instant::now();
            let rendered = render_builtin_tracks_parallel(
                jobs,
                BuiltinRenderContext {
                    total_samples,
                    start_sample: scope.window().map_or(0, |scope| scope.start_sample),
                    sample_rate,
                    tempo: lane_project.properties.tempo,
                    resource_root: self.resource_root.as_deref(),
                    clock: &clock,
                    cancel: cancel.as_ref(),
                    worker_options,
                },
            )?;
            crate::stage_log::push("builtin synth tracks", source_start);
            for (lane, buffer, latency) in rendered.into_iter() {
                lane_buffers[lane] = Some(buffer);
                lane_latencies[lane] = latency;
            }
            progress_done = (progress_done + builtin_tracks.len()).min(progress_total);
            self.report_progress(
                crate::render_cancel::RenderStage::Rendering,
                progress_done,
                progress_total,
            );
        }

        // Source inserts precede the hosting mixer chain. Include their
        // latency in the lane's identity before aligning independent sources.
        let instrument_latencies = lane_latencies.clone();
        for (lane, chain) in source_chains.iter_mut().enumerate() {
            if let Some(buffer) = lane_buffers[lane].as_mut() {
                crate::processing_failure::chain(chain, cancel.as_ref())
                    .map_err(|error| error.to_string())?;
                let extra_frames = u64::from(chain.latency_samples())
                    .checked_add(bounded_tail_samples(chain.tail_samples(), sample_rate))
                    .ok_or("Source insert render length overflow")?;
                let extra = usize::try_from(extra_frames)
                    .ok()
                    .and_then(|frames| frames.checked_mul(2))
                    .and_then(|samples| buffer.len().checked_add(samples))
                    .ok_or("Source insert render length overflow")?;
                buffer.resize(extra, 0.0);
                chain.process_with_transport(
                    buffer,
                    sample_rate,
                    Some(project_transport(project, timeline_start, sample_rate)),
                );
                chain.end_processing();
                crate::processing_failure::chain(chain, cancel.as_ref())
                    .map_err(|error| error.to_string())?;
                lane_latencies[lane] = lane_latencies[lane].saturating_add(chain.latency_samples());
            }
        }
        let mut parent_latencies = vec![0u32; num_tracks];
        for (lane, &target) in targets.iter().enumerate() {
            parent_latencies[target] = parent_latencies[target].max(lane_latencies[lane]);
        }
        for (lane, buffer) in lane_buffers.iter_mut().enumerate() {
            if let Some(buffer) = buffer {
                crate::source_lanes::align(
                    buffer,
                    parent_latencies[targets[lane]] - lane_latencies[lane],
                )?;
            }
        }
        for ((lane, _), buffer) in &mut lane_aux {
            crate::source_lanes::align(
                buffer,
                parent_latencies[targets[*lane]].saturating_sub(instrument_latencies[*lane]),
            )?;
        }

        // Fold every track's lanes into the buffer the mixer reads. A lane that
        // rendered nothing contributes silence, not a missing track.
        let mut folded: Vec<Option<Vec<f32>>> = vec![None; num_tracks];
        for (lane, buffer) in lane_buffers.into_iter().enumerate() {
            let Some(samples) = buffer else { continue };
            match &mut folded[targets[lane]] {
                Some(existing) => {
                    if existing.len() < samples.len() {
                        existing.resize(samples.len(), 0.0);
                    }
                    for (out, sample) in existing.iter_mut().zip(&samples) {
                        *out += sample;
                    }
                }
                slot => *slot = Some(samples),
            }
        }
        for (track, buffer) in folded.into_iter().enumerate() {
            if !self.track_dirty[track] {
                continue;
            }
            self.track_buffers[track] = buffer;
            self.track_source_latencies[track] = targets
                .iter()
                .enumerate()
                .filter(|(_, target)| **target == track)
                .map(|(lane, _)| lane_latencies[lane])
                .max()
                .unwrap_or(0);
        }
        for ((lane, pair), buffer) in lane_aux {
            let track = targets[lane];
            if !self.track_dirty[track] {
                continue;
            }
            match self.aux_buffers.entry((track, pair)) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    if existing.len() < buffer.len() {
                        existing.resize(buffer.len(), 0.0);
                    }
                    for (out, sample) in existing.iter_mut().zip(&buffer) {
                        *out += sample;
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(buffer);
                }
            }
        }

        // Mark empty dirty tracks
        let clips_start = std::time::Instant::now();
        for (i, dirty) in self.track_dirty.iter_mut().enumerate() {
            if !*dirty {
                continue;
            }
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            if self.track_buffers[i].is_none() {
                self.track_buffers[i] = Some(vec![0.0; total_samples * 2]);
                self.track_source_latencies[i] = 0;
            }
            let track_name = project.tracks[i].name.clone();
            let mut clip_buffer = None;
            for clip in project
                .audio_clips()
                .iter()
                .filter(|clip| clip.track == track_name)
            {
                {
                    let buffer = clip_buffer.get_or_insert_with(|| vec![0.0; total_samples * 2]);
                    if scope.window().is_some() {
                        crate::audio_clip::mix_into_track_window(
                            buffer,
                            timeline_start,
                            project,
                            clip,
                            sample_rate,
                            self.resource_root.as_deref(),
                        )?;
                    } else {
                        crate::audio_clip::mix_into_track(
                            buffer,
                            project,
                            clip,
                            sample_rate,
                            self.resource_root.as_deref(),
                        )?;
                    }
                }
            }
            if let Some(mut clips) = clip_buffer {
                crate::source_lanes::align(&mut clips, self.track_source_latencies[i])?;
                if let Some(buffer) = self.track_buffers[i].as_mut() {
                    buffer.resize(buffer.len().max(clips.len()), 0.0);
                    for (out, sample) in buffer.iter_mut().zip(clips) {
                        *out += sample;
                    }
                }
            }
            *dirty = false;
        }
        crate::stage_log::push("audio clips", clips_start);

        // Mix all track buffers through the audio graph
        if self.mix_dirty || dirty_count > 0 {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            self.report_progress(
                crate::render_cancel::RenderStage::Mixing,
                progress_total,
                progress_total,
            );
            let mut capture = self.stem_capture.take();
            let mixed = self.mix_cached(
                project,
                GraphMixOptions {
                    sample_rate,
                    total_samples,
                    timeline_start,
                    selection: None,
                    window: None,
                    workers: worker_options,
                },
                &mut capture,
            );
            self.stem_capture = capture;
            let (master, layout) = mixed?;
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            self.render_horizon_samples = layout.output_total;
            self.duration_seconds = layout.output_total as f64 / f64::from(sample_rate);
            self.mix_dirty = false;
            if !self.consume_source_buffers {
                self.master_buffer = Some(master.clone());
            }
            Ok(master)
        } else {
            // Nothing changed, return cached master
            Ok(self.master_buffer.clone().unwrap_or_default())
        }
    }

    /// Build the graph for one pass. `selection` zeroes the source latency of
    /// every track outside it so nothing off-selection can move the alignment.
    pub(super) fn graph_for(
        &self,
        project: &Project,
        sample_rate: u32,
        selection: Option<&[usize]>,
    ) -> Result<AudioGraph, String> {
        let latencies: Vec<u32> = self
            .track_source_latencies
            .iter()
            .enumerate()
            .map(|(index, latency)| {
                if selection.is_none_or(|tracks| tracks.contains(&index)) {
                    *latency
                } else {
                    0
                }
            })
            .collect();
        let mixer_project = crate::graph::graph_document(project);
        AudioGraph::try_from_project_with_source_latencies(&mixer_project, sample_rate, &latencies)
    }

    /// Mix the cached track buffers through the graph.
    fn mix_cached(
        &mut self,
        project: &Project,
        options: GraphMixOptions<'_>,
        stems: &mut Option<crate::graph::StemCapture>,
    ) -> Result<(Vec<f32>, MixLayout), String> {
        let graph = self.graph_for(project, options.sample_rate, options.selection)?;
        self.mix_graph(project, options, stems, graph)
    }

    /// Route cached sources using the same immutable options as the source pass.
    /// Cache-level timeline state is left to the caller.
    pub(super) fn mix_graph(
        &mut self,
        project: &Project,
        options: GraphMixOptions<'_>,
        stems: &mut Option<crate::graph::StemCapture>,
        graph: AudioGraph,
    ) -> Result<(Vec<f32>, MixLayout), String> {
        let result = self.mix_graph_inner(project, options, stems, graph);
        if result.is_err() {
            self.invalidate_failed_render();
            if let Some(capture) = stems {
                clear_failed_capture(capture);
            }
        }
        result
    }

    fn mix_graph_inner(
        &mut self,
        project: &Project,
        options: GraphMixOptions<'_>,
        stems: &mut Option<crate::graph::StemCapture>,
        mut graph: AudioGraph,
    ) -> Result<(Vec<f32>, MixLayout), String> {
        let GraphMixOptions {
            sample_rate,
            total_samples,
            timeline_start,
            selection,
            window,
            workers: worker_options,
        } = options;
        crate::processing_failure::graph(&graph, self.cancel.as_ref())
            .map_err(|error| error.to_string())?;
        let mix_start = std::time::Instant::now();
        let layout = MixLayout::new(&graph, sample_rate, total_samples, &self.track_source_tails)?;
        self.master_latency_samples = layout.master_latency as u32;
        #[cfg(test)]
        if selection.is_some()
            && self.render_horizon_samples > 0
            && total_samples >= self.render_horizon_samples
        {
            self.counters.full_timeline_mix_passes += 1;
        }
        if let Some(window) = window {
            graph.set_track_widths(&window.widths);
        }
        let post_fx = window.map(|window| window.post_fx);
        // A control source reads its feed track even when the caller selected
        // only the destination, so a subset window keeps the full mix's duck.
        // A key reads its source's post-insert buffer the same way. Feed and
        // key tracks are processed but masked out of this pass's routing.
        let mut expanded: Option<Vec<usize>> = selection.map(|tracks| tracks.to_vec());
        let mut control_only = self.control_only.clone();
        if let Some(tracks) = expanded.as_mut() {
            for feed in graph
                .control_source_tracks()
                .into_iter()
                .chain(graph.sidechain_source_tracks())
            {
                if !tracks.contains(&feed) {
                    tracks.push(feed);
                    control_only.push(feed);
                }
            }
        }
        if !control_only.is_empty() {
            graph.set_control_only_tracks(&control_only);
        }
        let selection = expanded.as_deref();
        let cancel = self.cancel.clone();
        if crate::render_cancel::stopped(cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }
        let mut track_bufs: Vec<Vec<f32>> = Vec::with_capacity(self.track_buffers.len());
        for (index, buffer) in self.track_buffers.iter_mut().enumerate() {
            if crate::render_cancel::stopped(cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            let audible = selection.is_none_or(|tracks| tracks.contains(&index));
            let mut samples = if !audible {
                // Leave the index empty: the routing pass skips it instead of
                // materializing and mixing a full-length zero buffer.
                Vec::new()
            } else if let Some(post_fx) = post_fx.and_then(|buffers| buffers.get(&index)) {
                post_fx.clone()
            } else if self.consume_source_buffers {
                buffer.take().unwrap_or_default()
            } else {
                buffer.clone().unwrap_or_default()
            };
            if audible {
                samples.resize(layout.render_stereo(), 0.0);
            }
            track_bufs.push(samples);
        }
        // Apply per-track effects in PARALLEL (the expensive part).
        // Each track's effect chain is independent — no shared state between tracks.
        let fx_start = std::time::Instant::now();
        // A window only processes the selected tracks: the rest stay empty and
        // are skipped by the routing pass. A muted key source still runs so its
        // tap carries the pre-mute signal.
        let fx_indices: Vec<usize> = (0..track_bufs.len())
            .filter(|&index| {
                selection.is_none_or(|tracks| tracks.contains(&index))
                    && post_fx.is_none_or(|buffers| !buffers.contains_key(&index))
                    && (graph.routing.get(index).is_some_and(|route| !route.mute)
                        || graph.is_key_source(index))
            })
            .collect();
        if graph.has_track_inputs() {
            graph.process_track_effects_for(
                &mut track_bufs,
                Some(project_transport(
                    project,
                    timeline_start,
                    graph.sample_rate,
                )),
                &fx_indices,
            );
        } else {
            let fx_set: std::collections::HashSet<usize> = fx_indices.iter().copied().collect();
            let track_chains = std::mem::take(&mut graph.track_chains);
            let mut returned = Vec::with_capacity(track_chains.len());
            let mut worker_panicked = false;
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for (index, (buffer, mut chain)) in
                    track_bufs.iter_mut().zip(track_chains).enumerate()
                {
                    if !fx_set.contains(&index) || chain.effects.is_empty() {
                        returned.push((index, chain));
                        continue;
                    }
                    let sample_rate = graph.sample_rate;
                    let transport = project_transport(project, timeline_start, sample_rate);
                    handles.push(scope.spawn(move || {
                        chain.process_with_transport(buffer, sample_rate, Some(transport));
                        chain.end_processing();
                        (index, chain)
                    }));
                }
                for handle in handles {
                    while oc_host::clap_main::offline_main_is_active() && !handle.is_finished() {
                        // CLI owns the main loop while track effects run in
                        // parallel. GUI workers have no offline-main scope.
                        if let Err(error) = oc_host::clap_main::service_offline_main() {
                            log::warn!("CLAP offline main service: {error}");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    match handle.join() {
                        Ok(chain) => returned.push(chain),
                        Err(_) => worker_panicked = true,
                    }
                }
            });
            returned.sort_unstable_by_key(|(index, _)| *index);
            graph.track_chains = returned.into_iter().map(|(_, chain)| chain).collect();
            if worker_panicked {
                return Err("Render refused: a track effect worker panicked".into());
            }
        }
        for chain in &mut graph.track_chains {
            chain.end_processing();
        }
        crate::processing_failure::graph(&graph, cancel.as_ref())
            .map_err(|error| error.to_string())?;
        crate::stage_log::push("track fx chains", fx_start);
        if crate::render_cancel::stopped(cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }

        // A window keeps what it just processed for the next window on the
        // same tracks, so a repeat request skips the chains entirely.
        if window.is_some_and(|window| window.capture) {
            for &index in selection.unwrap_or_default() {
                if post_fx.is_some_and(|buffers| buffers.contains_key(&index)) {
                    continue;
                }
                if let Some(samples) = track_bufs.get(index) {
                    self.remember_post_fx_snapshot(index, samples.clone());
                }
            }
        }

        // A subset pass mixes only the selected tracks' auxiliary output pairs.
        let mut selected_aux: Option<HashMap<(usize, usize), Vec<f32>>> = selection.map(|tracks| {
            self.aux_buffers
                .iter()
                .filter(|((track, _), _)| tracks.contains(track))
                .map(|(key, buffer)| (*key, buffer.clone()))
                .collect()
        });
        let aux_buffers: &HashMap<(usize, usize), Vec<f32>> = match selected_aux.as_mut() {
            Some(aux) => {
                for buffer in aux.values_mut() {
                    buffer.resize(layout.render_stereo(), 0.0);
                }
                aux
            }
            None => {
                for buffer in self.aux_buffers.values_mut() {
                    buffer.resize(layout.render_stereo(), 0.0);
                }
                &self.aux_buffers
            }
        };

        // Shared routing/PDC path used by offline and streaming renders.
        if crate::render_cancel::stopped(cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }
        let mut master_buf = graph.mix_processed_with_workers(
            &mut track_bufs,
            Some(project_transport(
                project,
                timeline_start,
                graph.sample_rate,
            )),
            stems.as_mut(),
            aux_buffers,
            worker_options.workers(),
        );
        for bus in graph.bus_chains.values_mut() {
            bus.chain.end_processing();
        }
        graph.master_chain.end_processing();
        crate::processing_failure::graph(&graph, cancel.as_ref())
            .map_err(|error| error.to_string())?;
        if crate::render_cancel::stopped(cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }

        // A full pass is the only one that classifies the pan law; a window
        // inherits those widths so its result cannot depend on its own bounds.
        if selection.is_none() {
            self.full_mix_widths = graph.track_widths().to_vec();
        }

        crate::stage_log::push("mix + buses + master", mix_start);
        // Master PDC: drop the chain latency from the head of the aligned
        // buffer; the extra input frames keep the program end intact.
        let master_shift = layout.master_latency * 2;
        if master_shift > 0 && master_buf.len() > master_shift {
            master_buf.copy_within(master_shift.., 0);
        }
        master_buf.truncate(layout.output_total * 2);

        log::info!(
            "Mix + effects took {:.2}s",
            mix_start.elapsed().as_secs_f64()
        );
        #[cfg(feature = "test-support")]
        if oc_diag::perf_enabled() {
            oc_diag::counter_in("render.cache.source_renders", "sources")
                .add(self.counters.source_renders as u64);
        }
        Ok((master_buf, layout))
    }

    /// Render a window. Without `tracks` it is an exact slice of the cached
    /// full render. A subset slices the same timeline with every other track
    /// muted, so it is a true slice and leaves the cached full mix valid.
    pub fn render_range(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        request: RangeRenderRequest,
    ) -> Result<RangeRenderOutput, String> {
        let frames = validate_range(project, &request)?;
        let pass = RangeRenderPass {
            request: &request,
            frames,
            workers: self.worker_options.unwrap_or_default(),
        };
        let Some(tracks) = request.tracks.clone() else {
            return self.render_full_window(
                project,
                soundfont_path,
                sample_rate,
                plugin_instances,
                pass,
            );
        };
        let mut selected = tracks;
        selected.sort_unstable();
        selected.dedup();
        expand_shared_output_groups(project, &mut selected);
        // Keeping every track is the full-mix path: only it may fill the cache.
        if selected.len() == project.tracks.len() {
            let mut all = request.clone();
            all.tracks = None;
            return self.render_full_window(
                project,
                soundfont_path,
                sample_rate,
                plugin_instances,
                RangeRenderPass {
                    request: &all,
                    ..pass
                },
            );
        }
        // A subset window reuses the source and post-FX snapshots instead of
        // re-running the source pass, so check the clip identity — including
        // each media file's stamp — before deciding the cache is clean.
        self.refresh_clip_fingerprints(project, sample_rate);
        let ready =
            !self.is_dirty() && self.master_buffer.is_some() && self.render_horizon_samples > 0;
        if ready {
            return self.render_subset_window(project, sample_rate, &selected, pass);
        }
        // No clean full render to cut from: source only the selected tracks.
        self.render_cold_subset(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            &selected,
            pass,
        )
    }

    /// Full-mix window: slice the cached render, refreshing the cache first.
    fn render_full_window(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        pass: RangeRenderPass<'_>,
    ) -> Result<RangeRenderOutput, String> {
        let RangeRenderPass {
            request,
            frames,
            workers,
        } = pass;
        let clean = !self.is_dirty() && self.master_buffer.is_some();
        if request.stems {
            self.capture_stems(true);
        }
        let cache_hit = clean && !self.is_dirty();
        let master = match self.render_scoped(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RenderPass {
                scope: RenderScope::Full,
                workers,
            },
        ) {
            Ok(master) => master,
            // An empty project is a valid silent range, not a render error.
            Err(error) if error == "No events to render" => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut stems = Vec::new();
        if request.stems
            && let Some(capture) = self.stem_capture.as_ref()
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
            tracks: (0..project.tracks.len()).collect(),
            cache_hit,
            window: None,
        })
    }

    /// Window-only solo render for previews: starts from silence at
    /// `start_sample`, keeps only `tracks`, and invalidates the cache.
    /// Unlike `render_range` this is not a slice of the full-mix timeline.
    pub fn render_solo_window(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
        request: RangeRenderRequest,
    ) -> Result<RangeRenderOutput, String> {
        let frames = validate_range(project, &request)?;
        let selected: Vec<usize> = request
            .tracks
            .clone()
            .map(|mut tracks| {
                tracks.sort_unstable();
                tracks.dedup();
                tracks
            })
            .unwrap_or_else(|| (0..project.tracks.len()).collect());
        self.mark_all_dirty();
        if request.stems {
            self.capture_stems(true);
        }
        let rendered = self.render_scoped(
            project,
            soundfont_path,
            sample_rate,
            plugin_instances,
            RenderPass {
                scope: RenderScope::Window(&request),
                workers: self.worker_options.unwrap_or_default(),
            },
        );
        // Even a failed window can leave some clean tracks with short, shifted buffers.
        self.mark_all_dirty();
        let mut audio = rendered?;
        audio.truncate(frames * 2);
        audio.resize(frames * 2, 0.0);
        let mut stems = Vec::new();
        if request.stems
            && let Some(capture) = self.stem_capture.as_ref()
        {
            for (index, (name, buffer)) in capture.tracks.iter().enumerate() {
                let mut buffer = buffer.clone();
                buffer.truncate(frames * 2);
                buffer.resize(frames * 2, 0.0);
                stems.push((index, name.clone(), buffer));
            }
        }
        Ok(RangeRenderOutput {
            audio,
            stems,
            sample_rate,
            frames,
            tracks: selected,
            cache_hit: false,
            window: None,
        })
    }

    /// Full render (mark all dirty first, then incremental).
    pub fn render_full(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        plugin_instances: &mut HashMap<String, ClapInstance>,
    ) -> Result<Vec<f32>, String> {
        let _timer = oc_diag::perf_counter("cache.full.ms").map(oc_diag::Timer::start);
        self.mark_all_dirty();
        self.render_incremental(project, soundfont_path, sample_rate, plugin_instances)
    }
}

fn validate_range(project: &Project, request: &RangeRenderRequest) -> Result<usize, String> {
    if request.end_sample <= request.start_sample {
        return Err("range end must be after range start".into());
    }
    let frames = usize::try_from(request.end_sample - request.start_sample)
        .map_err(|_| "range length exceeds platform size".to_string())?;
    if let Some(tracks) = &request.tracks {
        if tracks.is_empty() {
            return Err("track subset must not be empty".into());
        }
        if let Some(index) = tracks.iter().find(|index| **index >= project.tracks.len()) {
            return Err(format!("track {index} out of range"));
        }
    }
    Ok(frames)
}

/// Content hash of a track's hairpin spans; `0` when it carries no mark.
fn track_ramp_fingerprint(
    project: &oc_dsl::ast::Project,
    track: &oc_dsl::ast::Track,
    clock: &oc_dsl::timing::ProjectClock,
) -> Result<u64, String> {
    if !oc_dsl::dynamics::track_has_hairpins(project, track) {
        return Ok(0);
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for ramp in oc_dsl::dynamics::track_layer_ramps(project, track, clock)? {
        mix(ramp.start.to_bits());
        mix(ramp.end.to_bits());
        mix(u64::from(ramp.start_velocity));
        mix(u64::from(ramp.target_velocity));
    }
    Ok(hash)
}

fn clear_failed_capture(capture: &mut crate::graph::StemCapture) {
    capture.tracks.clear();
    capture.buses.clear();
    capture.master_inputs.clear();
    capture.pre_master.clear();
}
