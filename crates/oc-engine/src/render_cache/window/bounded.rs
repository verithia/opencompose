//! Qualified partial timelines, separate from the complete render cache.
use super::*;
use std::fmt::Write;

const AUDIO_BUDGET: usize = 128 * 1024 * 1024;
const PROJECT_TEXT_BUDGET: usize = 1024 * 1024;

pub(in crate::render_cache) struct BoundedPrefix {
    project: Project,
    source: Box<RenderCache>,
    end: u64,
    selected: Option<Vec<usize>>,
    soundfont: PathBuf,
    soundfont_stamp: Option<(u64, std::time::SystemTime)>,
    resource_root: Option<PathBuf>,
    pre_roll: f64,
    minimum_duration: f64,
}

/// Limit metadata before taking its typed, exact-equality snapshot. Debug text
/// only limits admission size; it never identifies a project or a cache hit.
struct MetadataBudget(usize);
impl std::fmt::Write for MetadataBudget {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0 = self.0.checked_sub(text.len()).ok_or(std::fmt::Error)?;
        Ok(())
    }
}

fn eligible(project: &Project) -> bool {
    let known = |preset: &PresetDef| {
        preset.source.as_deref() == Some("builtin")
            && matches!(
                preset.plugin.as_deref(),
                Some(oc_synth::synth::PLUGIN_ID | "builtin.test_tone")
            )
            && preset.file.is_none()
            && preset.patch.is_none()
            && !preset
                .params
                .iter()
                .any(|(_, value)| matches!(value, ParamValue::String(_)))
    };
    // Unknown resource owners cannot be qualified from document equality alone.
    if project
        .presets
        .iter()
        .any(|preset| preset.extends.is_none() && !known(preset))
        || !project.audio_clips().is_empty()
        || project.tracks.iter().any(|track| {
            !track.input_routes.is_empty()
                || track
                    .engine_params
                    .iter()
                    .flatten()
                    .any(|(_, value)| matches!(value, ParamValue::String(_)))
        })
        || project
            .tracks
            .iter()
            .flat_map(|track| &track.effects)
            .chain(project.presets.iter().flat_map(|preset| &preset.effects))
            .chain(project.buses.iter().flat_map(|bus| &bus.effects))
            .chain(project.master.iter().flat_map(|master| &master.effects))
            .any(|effect| {
                effect.effect_type.device_id().is_none()
                    || effect
                        .params
                        .iter()
                        .any(|(_, value)| matches!(value, ParamValue::String(_)))
            })
    {
        return false;
    }
    let Ok(resolved) = oc_dsl::preset::resolve(project) else {
        return false;
    };
    let Ok(expanded) = oc_dsl::arrangement::expand(&resolved) else {
        return false;
    };
    let Ok(lanes) = crate::source_lanes::plan(&expanded) else {
        return false;
    };
    lanes.tracks().tracks.iter().all(|track| {
        let preset = track
            .preset
            .as_deref()
            .and_then(|name| expanded.presets.iter().find(|preset| preset.name == name));
        preset.is_some_and(known)
            && preset.is_some_and(|preset| {
                let params = crate::preset::builtin_track_params_untrimmed(
                    lanes.tracks(),
                    Some(preset),
                    track,
                );
                oc_synth::native_performance::program_binding(&params).is_ok_and(|binding| {
                    binding.program.is_none()
                        && binding.bank_msb.is_none()
                        && binding.bank_lsb.is_none()
                })
            })
    })
}

fn audio_bytes(source: &RenderCache) -> Option<usize> {
    source
        .track_buffers
        .iter()
        .flatten()
        .chain(source.master_buffer.iter())
        .chain(source.aux_buffers.values())
        .chain(source.pattern_buffers.values())
        .chain(
            source
                .post_fx_snapshots
                .iter()
                .flatten()
                .map(|snapshot| &snapshot.samples),
        )
        .try_fold(0usize, |sum, buffer| {
            sum.checked_add(buffer.capacity().checked_mul(std::mem::size_of::<f32>())?)
        })
}

impl RenderCache {
    /// A bounded result can survive an application cache handoff while the
    /// complete project remains dirty. It cannot answer a full-render request.
    pub fn has_bounded_range(&self) -> bool {
        self.bounded_range
            .as_ref()
            .is_some_and(|entry| !entry.source.is_dirty())
    }

    pub(in crate::render_cache) fn remember_bounded_prefix(
        &mut self,
        project: &Project,
        soundfont: &Path,
        end: u64,
        selected: Option<Vec<usize>>,
        mut source: RenderCache,
    ) {
        if source.is_dirty()
            || source.stem_capture.is_some()
            || source.master_buffer.is_none()
            || !audio_bytes(&source).is_some_and(|bytes| bytes <= AUDIO_BUDGET)
            || write!(&mut MetadataBudget(PROJECT_TEXT_BUDGET), "{project:?}").is_err()
            || !eligible(project)
        {
            return;
        }
        source.bounded_range = None;
        self.bounded_range = Some(BoundedPrefix {
            project: project.clone(),
            source: Box::new(source),
            end,
            selected,
            soundfont: soundfont.to_path_buf(),
            soundfont_stamp: native_state_stamp(soundfont).ok(),
            resource_root: self.resource_root.clone(),
            pre_roll: self.pre_roll_seconds,
            minimum_duration: self.minimum_duration_seconds,
        });
    }

    pub(in crate::render_cache) fn try_bounded_range(
        &mut self,
        project: &Project,
        soundfont: &Path,
        rate: u32,
        instances: &HashMap<String, ClapInstance>,
        pass: RangeRenderPass<'_>,
    ) -> Result<Option<RangeRenderOutput>, String> {
        let Some(mut entry) = self.bounded_range.take() else {
            return Ok(None);
        };
        if crate::render_cancel::stopped(self.cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }
        for instance in instances.values() {
            crate::processing_failure::effect(instance, self.cancel.as_ref())
                .map_err(|error| error.to_string())?;
        }
        let mut selected = pass.request.tracks.clone();
        if let Some(selected) = &mut selected {
            selected.sort_unstable();
            selected.dedup();
            expand_shared_output_groups(project, selected);
        }
        if selected
            .as_ref()
            .is_some_and(|tracks| tracks.len() == project.tracks.len())
        {
            selected = None;
        }
        if entry.project != *project
            || entry.source.sample_rate != rate
            || entry.soundfont != soundfont
            || entry.soundfont_stamp != native_state_stamp(soundfont).ok()
            || entry.resource_root != self.resource_root
            || entry.pre_roll != self.pre_roll_seconds
            || entry.minimum_duration != self.minimum_duration_seconds
            || entry.source.is_dirty()
            || pass.request.end_sample > entry.end
            || pass.request.stems
            || self.stem_capture.is_some()
            || (entry.selected.is_some() && entry.selected != selected)
        {
            return Ok(None);
        }
        entry.source.cancel = self.cancel.clone();
        let output = if selected == entry.selected {
            RangeRenderOutput {
                audio: slice_stereo_range(
                    entry.source.master_buffer.as_deref().unwrap_or(&[]),
                    pass.request.start_sample,
                    pass.frames,
                ),
                stems: Vec::new(),
                sample_rate: rate,
                frames: pass.frames,
                tracks: selected.unwrap_or_else(|| (0..project.tracks.len()).collect()),
                cache_hit: true,
                window: entry.selected.as_ref().map(|_| RangeWindowPath::Snapshot),
            }
        } else if let Some(selected) = selected {
            entry.source.progress = self.progress.take();
            let result = entry
                .source
                .render_subset_window(project, rate, &selected, pass);
            self.progress = entry.source.progress.take();
            result?
        } else {
            return Ok(None);
        };
        if crate::render_cancel::stopped(self.cancel.as_ref()) {
            return Err(crate::render_cancel::cancelled());
        }
        self.bounded_range = Some(entry);
        Ok(Some(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> Project {
        oc_dsl::parse(
            r#"@ocp "0.2.0"
@project "Bounded cache"
  tempo: 120
@preset a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.05}
@preset b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.05}
@track a | preset: a, instrument: synth/keys
  @phrase early [1:1 - 2:1]
    C4 w mf
  @phrase late [4:1 - 5:1]
    C4 q mf
@track b | preset: b, instrument: synth/keys
  @phrase early [1:1 - 2:1]
    E4 w mf
  @phrase late [4:1 - 5:1]
    E4 q mf
"#,
        )
        .unwrap()
    }
    fn request(tracks: Option<Vec<usize>>) -> RangeRenderRequest {
        RangeRenderRequest {
            start_sample: 1000,
            end_sample: 3000,
            tracks,
            stems: false,
        }
    }
    fn range(
        cache: &mut RenderCache,
        project: &Project,
        request: RangeRenderRequest,
    ) -> RangeRenderOutput {
        cache
            .render_range(
                project,
                Path::new("unused.sf2"),
                8000,
                &mut HashMap::new(),
                request,
            )
            .unwrap()
    }

    #[test]
    fn bounded_repeat_and_subset_reuse_sources_without_a_full_timeline() {
        let project = project();
        let mut cache = RenderCache::new();
        let first = range(&mut cache, &project, request(None));
        assert!(!first.cache_hit);
        assert!(cache.has_bounded_range());
        let count = cache.render_counters().source_renders;
        let repeat = range(&mut cache, &project, request(None));
        assert!(repeat.cache_hit);
        assert_eq!(repeat.audio, first.audio);
        assert_eq!(cache.render_counters().source_renders, count);
        assert!(cache.is_dirty());
        assert!(cache.master_buffer().is_none());
        let subset = range(&mut cache, &project, request(Some(vec![1])));
        assert!(subset.cache_hit);
        assert_eq!(cache.render_counters().source_renders, count);
        let mut muted = project.clone();
        muted.tracks[0].mix.mute = true;
        let mut reference = RenderCache::new();
        reference.set_pattern_previews(false);
        let full = reference
            .render_full(&muted, Path::new("unused.sf2"), 8000, &mut HashMap::new())
            .unwrap();
        assert_eq!(subset.audio, slice_stereo_range(&full, 1000, 2000));
        assert_eq!(
            range(&mut cache, &project, request(None)).audio,
            first.audio,
            "a subset remix cannot replace the qualified full-mix prefix"
        );
    }

    #[test]
    fn bounded_identity_covers_project_settings_selection_and_missing_resources() {
        let mut project = project();
        let mut cache = RenderCache::new();
        range(&mut cache, &project, request(None));
        project.tracks[0].mix.volume_db = -9.0;
        let changed = range(&mut cache, &project, request(None));
        assert!(!changed.cache_hit);
        cache.set_pre_roll_seconds(0.001).unwrap();
        assert!(!cache.has_bounded_range());
        range(&mut cache, &project, request(None));
        cache.set_minimum_duration(0.01).unwrap();
        assert!(!cache.has_bounded_range());
        range(&mut cache, &project, request(None));
        cache.set_resource_root(Some(PathBuf::from("a-different-root")));
        assert!(!cache.has_bounded_range());
        range(&mut cache, &project, request(None));
        let wider = RangeRenderRequest {
            end_sample: 4000,
            ..request(None)
        };
        assert!(!range(&mut cache, &project, wider).cache_hit);
        let mut selected = RenderCache::new();
        range(&mut selected, &project, request(Some(vec![0])));
        assert!(!range(&mut selected, &project, request(Some(vec![1]))).cache_hit);

        let dir = tempfile::tempdir().unwrap();
        let font = dir.path().join("unused.sf2");
        let mut cache = RenderCache::new();
        cache
            .render_range(&project, &font, 8000, &mut HashMap::new(), request(None))
            .unwrap();
        std::fs::write(&font, b"newly present resource").unwrap();
        let changed = cache
            .render_range(&project, &font, 8000, &mut HashMap::new(), request(None))
            .unwrap();
        assert!(!changed.cache_hit);
        std::fs::remove_file(&font).unwrap();
        let missing = cache
            .render_range(&project, &font, 8000, &mut HashMap::new(), request(None))
            .unwrap();
        assert!(!missing.cache_hit);
    }

    #[test]
    fn bounded_cache_refuses_unknown_resources_and_keeps_capture_requests_separate() {
        let mut project = project();
        assert!(eligible(&project));
        project.presets[0].params.push((
            "wavetable.file".into(),
            ParamValue::String("missing.wav".into()),
        ));
        assert!(!eligible(&project));
        project.presets[0].params.pop();
        project.presets[0].source = Some("vst3".into());
        assert!(!eligible(&project));
        let project = self::project();
        let mut cache = RenderCache::new();
        range(&mut cache, &project, request(None));
        cache.capture_export_stems();
        assert!(!cache.has_bounded_range());
        let mut cache = RenderCache::new();
        range(&mut cache, &project, request(None));
        let stopped = crate::render_cancel::CancelToken::default();
        stopped.cancel();
        cache.set_cancel_token(stopped);
        assert!(
            cache
                .render_range(
                    &project,
                    Path::new("unused.sf2"),
                    8000,
                    &mut HashMap::new(),
                    request(None)
                )
                .is_err()
        );
        assert!(!cache.has_bounded_range());
    }

    #[test]
    fn bounded_admission_rejects_capacity_and_metadata_budget_without_changing_output() {
        let project = project();
        let mut owner = RenderCache::new();
        let mut source = RenderCache::new();
        source.mix_dirty = false;
        source.master_buffer = Some(Vec::with_capacity(AUDIO_BUDGET / 4 + 1));
        owner.remember_bounded_prefix(&project, Path::new("unused.sf2"), 3000, None, source);
        assert!(!owner.has_bounded_range());
        let mut source = RenderCache::new();
        source.mix_dirty = false;
        source.master_buffer = Some(vec![0.0; 2]);
        let mut huge = project.clone();
        huge.name = Some("x".repeat(PROJECT_TEXT_BUDGET + 1));
        owner.remember_bounded_prefix(&huge, Path::new("unused.sf2"), 3000, None, source);
        assert!(!owner.has_bounded_range());
    }
}
