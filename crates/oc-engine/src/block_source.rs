//! Stateful instrument sources for bounded playback. No whole-song audio cache.
use crate::dsp::AudioEffect;
use crate::idle_gate;
use crate::interpret::MidiMessage;
use crate::soundfont::SoundFontRenderer;
use oc_dsl::ast::{ParamValue, PresetDef, Project, ProjectProperties, Track};
use oc_synth::synth::OpenSynth;
use std::path::Path;

/// Resolve project-owned resources without mutating the saved document.
pub fn prepare_project(
    project: &oc_dsl::ast::Project,
    root: Option<&Path>,
) -> Result<oc_dsl::ast::Project, String> {
    let mut project = oc_dsl::preset::resolve(project)?;
    let resolve = |text: &mut String| -> Result<(), String> {
        if Path::new(text).is_relative() {
            *text = root
                .ok_or("Relative instrument asset requires a saved project directory")?
                .join(&*text)
                .to_string_lossy()
                .into_owned();
        }
        Ok(())
    };
    fn file_param(key: &str) -> bool {
        let key = key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>();
        matches!(
            key.as_str(),
            "opensynthstatefile"
                | "wavetablefile"
                | "samplersfzfile"
                | "samplersamplefile"
                | "samplerroot"
                | "clapstatefile"
                | "vst3statefile"
                | "austatefile"
                | "lv2statefile"
                | "pluginstatefile"
                | "statefile"
        )
    }
    for preset in &mut project.presets {
        if let Some(file) = &mut preset.file {
            resolve(file)?;
        }
        for (key, value) in &mut preset.params {
            // Kit layers name their sample per pad: `layer.<pad>.<n>.sample`.
            let kit_sample = key.starts_with("layer.") && key.ends_with(".sample");
            if (file_param(key) || kit_sample)
                && let ParamValue::String(file) = value
            {
                resolve(file)?;
            }
        }
    }
    for track in &mut project.tracks {
        for (key, value) in track.engine_params.iter_mut().flatten() {
            if file_param(key)
                && let ParamValue::String(file) = value
            {
                resolve(file)?;
            }
        }
    }
    for clip in project.audio_clips_mut() {
        if Path::new(&clip.file).is_relative() {
            let root = root.ok_or_else(|| {
                format!(
                    "audio clip '{}': relative media path requires a saved project directory",
                    clip.name
                )
            })?;
            clip.file = root.join(&clip.file).to_string_lossy().into_owned();
        }
    }
    Ok(project)
}

/// Largest block a streaming source renders in one call. Buffers reserve this
/// so the render path only reuses memory.
pub const MAX_SOURCE_BLOCK_FRAMES: usize = 128;

/// Interleaved main pair plus declared auxiliary pairs for one source lane.
/// Pairs are planned when the lane is built; buffers are reused every block.
pub struct SourceOutput {
    /// Port pair the track reads as its main output.
    main_pair: usize,
    /// Auxiliary pairs beyond the main pair, sorted and deduplicated.
    aux_pairs: Vec<usize>,
    /// Buffer index for each auxiliary pair; a pair may share the main buffer.
    aux_slots: Vec<usize>,
    /// One plugin call fans the main pair first, then every auxiliary pair.
    requested: Vec<usize>,
    /// `buffers[0]` is the main pair, `buffers[1..]` follow `aux_pairs`.
    buffers: Vec<Vec<f32>>,
}

impl SourceOutput {
    /// `main_pair` is the track's resolved output port pair; 0 is the pair-less
    /// default every project had before ports existed.
    pub fn new(main_pair: usize, aux_pairs: &[usize]) -> Self {
        let mut requested = Vec::with_capacity(aux_pairs.len() + 1);
        requested.push(main_pair);
        let mut aux_slots = Vec::with_capacity(aux_pairs.len());
        for &pair in aux_pairs {
            let slot = requested
                .iter()
                .position(|&known| known == pair)
                .unwrap_or_else(|| {
                    requested.push(pair);
                    requested.len() - 1
                });
            aux_slots.push(slot);
        }
        let buffers = requested
            .iter()
            .map(|_| Vec::with_capacity(MAX_SOURCE_BLOCK_FRAMES * 2))
            .collect();
        Self {
            main_pair,
            aux_pairs: aux_pairs.to_vec(),
            aux_slots,
            requested,
            buffers,
        }
    }

    /// Port pair this source feeds as its main output.
    pub fn main_pair(&self) -> usize {
        self.main_pair
    }

    /// Auxiliary port pairs beyond the main pair.
    pub fn aux_pairs(&self) -> &[usize] {
        &self.aux_pairs
    }

    /// The main stereo buffer.
    pub fn main(&self) -> &[f32] {
        &self.buffers[0]
    }

    /// Iterate auxiliary pair buffers with their pair index.
    pub fn aux(&self) -> impl Iterator<Item = (usize, &[f32])> {
        self.aux_pairs.iter().copied().zip(
            self.aux_slots
                .iter()
                .map(|&slot| self.buffers[slot].as_slice()),
        )
    }

    fn clear(&mut self) {
        for buffer in &mut self.buffers {
            buffer.clear();
        }
    }

    fn fill_silence(&mut self, frames: usize) {
        for buffer in &mut self.buffers {
            buffer.clear();
            buffer.resize(frames * 2, 0.0);
        }
    }

    /// Zero every buffer without changing its length.
    fn zero(&mut self) {
        for buffer in &mut self.buffers {
            buffer.fill(0.0);
        }
    }

    /// Peak across the main pair and every auxiliary pair.
    fn peak(&self) -> f32 {
        self.buffers
            .iter()
            .map(|buffer| idle_gate::buffer_peak(buffer))
            .fold(0.0_f32, f32::max)
    }

    /// Apply one pause/resume fade to every pair, so no output clicks.
    fn apply_ramp(&mut self, ramp: idle_gate::GateRamp) {
        for buffer in &mut self.buffers {
            idle_gate::apply_ramp(buffer, ramp);
        }
    }
}

/// Immutable control-plane preparation for one streaming score lane.
#[derive(Clone, Copy)]
pub struct StreamingSourcePreparation<'a> {
    pub events: &'a [&'a MidiMessage],
    pub workers: crate::render_parallel::RenderWorkerOptions,
}

/// Instrument implementation behind one streaming lane.
pub enum BlockSource {
    Synth(Box<OpenSynth>),
    Performance(Box<oc_synth::native_performance::PerformanceRenderer>),
    Sampler(Box<oc_synth::sampler::Sampler>),
    DrumKit(Box<oc_synth::drumkit::DrumKit>),
    SoundFont(Box<SoundFontRenderer>),
    Clap(Box<oc_host::clap_instance::ClapInstance>),
    Vst3(Box<oc_host::vst3_instance::Vst3Instance>),
    Lv2(Box<oc_host::lv2::Lv2Instance>),
    Vst2(Box<oc_host::vst2::Vst2Instance>),
    #[cfg(target_os = "macos")]
    Au(Box<oc_host::au_instance::AuInstance>),
    /// Clip-only track: clip lanes supply the audio, no instrument is built.
    Silent,
    Builtin {
        plugin: Box<dyn AudioEffect>,
        active: bool,
    },
}

impl BlockSource {
    /// Construct an explicitly scheduled performance source without score defaults.
    /// Commands must already exclude CCs delegated to graph send envelopes.
    pub fn new_performance(
        preset: Option<&PresetDef>,
        params: &Vec<(String, ParamValue)>,
        fallback_font: &Path,
        sample_rate: u32,
        tempo: f64,
        commands: &[oc_synth::performance::ScheduledCommand],
        root: Option<&Path>,
    ) -> Result<Self, String> {
        if sample_rate == 0 || !tempo.is_finite() || tempo <= 0.0 {
            return Err("performance requires a valid sample rate and tempo".into());
        }
        if commands
            .windows(2)
            .any(|pair| pair[0].sample > pair[1].sample)
        {
            return Err("performance commands must be ordered".into());
        }
        oc_dsl::performance::PerformanceTimeline {
            events: commands
                .iter()
                .map(|command| oc_dsl::performance::PerformanceEvent {
                    quarter: 0.0,
                    channel: command.channel,
                    message: command.message.clone(),
                })
                .collect(),
        }
        .validate()?;
        let preset = preset.ok_or("performance requires an explicit preset")?;
        if preset.source.as_deref() == Some("builtin")
            && preset.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)
        {
            let patch = oc_synth::synth::load_project_patch(params, root)?;
            let binding = oc_synth::native_performance::program_binding(params)?;
            return Ok(Self::Performance(Box::new(
                oc_synth::native_performance::PerformanceRenderer::new(
                    patch, commands, binding, tempo,
                )?,
            )));
        }
        if !matches!(preset.source.as_deref(), Some("clap" | "vst2" | "vst3")) {
            return Err(
                "performance streaming requires OpenSynth or MIDI-capable CLAP/VST2/VST3".into(),
            );
        }
        let source = Self::new(
            Some(preset),
            params,
            fallback_font,
            sample_rate,
            tempo,
            root,
        )?;
        match &source {
            Self::Clap(p) if !p.supports_midi1_input() => {
                return Err("CLAP input port zero does not support MIDI1".into());
            }
            Self::Vst2(p) if !p.accepts_midi() => {
                return Err("VST2 instrument does not accept MIDI".into());
            }
            _ => {}
        }
        Ok(source)
    }

    /// Native performance is split at event boundaries; hosted sources queue offsets.
    pub fn send_performance_at(
        &mut self,
        command: &oc_synth::performance::ScheduledCommand,
        offset: u32,
        block_frames: u32,
    ) -> Result<(), String> {
        if block_frames == 0 || offset >= block_frames || offset > i32::MAX as u32 {
            return Err("performance event outside processing block".into());
        }
        match self {
            Self::Performance(p) if offset == 0 => p.send(command),
            Self::Performance(_) => {
                Err("native performance must dispatch at an event boundary".into())
            }
            Self::Clap(p) => p.queue_performance_command(command, offset, block_frames),
            Self::Vst2(p) => {
                crate::render_cache::queue_vst2_performance_command(p, command, offset as i32)
            }
            Self::Vst3(p) => {
                crate::render_cache::queue_vst3_performance_command(p, command, offset as i32)
            }
            _ => Err("source does not support explicit performance commands".into()),
        }
    }

    /// Build a source from a preset, parameters and optional project root.
    pub fn new(
        preset: Option<&PresetDef>,
        params: &Vec<(String, ParamValue)>,
        fallback_font: &Path,
        sample_rate: u32,
        tempo: f64,
        root: Option<&Path>,
    ) -> Result<Self, String> {
        Self::new_prepared(
            preset,
            params,
            fallback_font,
            sample_rate,
            tempo,
            root,
            None,
        )
    }

    /// Prepare only SoundFont resources required by this complete score lane.
    /// The event plan includes initial channel setup and future program changes;
    /// playback never decodes a sample from its MIDI dispatch or audio callback.
    pub fn new_streaming(
        preset: Option<&PresetDef>,
        params: &Vec<(String, ParamValue)>,
        fallback_font: &Path,
        sample_rate: u32,
        tempo: f64,
        root: Option<&Path>,
        preparation: StreamingSourcePreparation<'_>,
    ) -> Result<Self, String> {
        Self::new_prepared(
            preset,
            params,
            fallback_font,
            sample_rate,
            tempo,
            root,
            Some(preparation),
        )
    }

    fn new_prepared(
        preset: Option<&PresetDef>,
        params: &Vec<(String, ParamValue)>,
        fallback_font: &Path,
        sample_rate: u32,
        tempo: f64,
        root: Option<&Path>,
        preparation: Option<StreamingSourcePreparation<'_>>,
    ) -> Result<Self, String> {
        if let Some(p) = preset.filter(|p| {
            matches!(
                p.source.as_deref(),
                Some("clap" | "vst3" | "vst2" | "au" | "lv2")
            ) || (p.source.as_deref() == Some("sample") && p.plugin.as_deref() == Some("sfizz"))
        }) {
            use crate::render_cache::*;
            use oc_host::{PluginFormat, find_plugin_info};
            let id = p
                .plugin
                .as_deref()
                .ok_or("Plugin preset is missing plugin id")?;
            if p.source.as_deref() == Some("lv2") {
                let info = find_plugin_info(id, PluginFormat::Lv2)
                    .ok_or_else(|| format!("LV2 plugin {id} is not installed"))?;
                let mut plugin = oc_host::lv2::Lv2Instance::load(
                    Path::new(&info.path),
                    &info.id,
                    sample_rate,
                    4096,
                )
                .map_err(|error| error.to_string())?;
                let context = oc_host::PluginStateContext::new(
                    PluginFormat::Lv2,
                    Some(info.id.as_str()),
                    &p.name,
                );
                crate::render_cache::apply_lv2_preset_state_at(
                    &mut plugin,
                    params,
                    &context,
                    root,
                )?;
                crate::render_cache::apply_lv2_preset_params(&mut plugin, params)?;
                return Ok(Self::Lv2(Box::new(plugin)));
            }
            if p.source.as_deref() == Some("au") {
                #[cfg(target_os = "macos")]
                {
                    let mut plugin = oc_host::au_instance::AuInstance::load(id, sample_rate, 4096)
                        .map_err(|error| error.to_string())?;
                    let context =
                        oc_host::PluginStateContext::new(PluginFormat::Au, Some(id), &p.name);
                    crate::render_cache::apply_au_preset_state_at(
                        &mut plugin,
                        params,
                        &context,
                        root,
                    )?;
                    crate::render_cache::apply_au_preset_params(&mut plugin, params)?;
                    return Ok(Self::Au(Box::new(plugin)));
                }
                #[cfg(not(target_os = "macos"))]
                {
                    return Err(format!("AU preset '{id}' needs macOS AudioUnit hosting"));
                }
            }
            if p.source.as_deref() == Some("vst2") {
                let info = find_plugin_info(id, PluginFormat::Vst2)
                    .ok_or_else(|| format!("VST2 plugin {id} is not installed"))?;
                let mut plugin =
                    oc_host::vst2::Vst2Instance::load(Path::new(&info.path), sample_rate, 4096)?;
                let context = oc_host::PluginStateContext::new(
                    PluginFormat::Vst2,
                    Some(info.id.as_str()),
                    &p.name,
                );
                apply_vst2_preset_state_at(&mut plugin, params, &context, root)?;
                apply_vst2_preset_params(&mut plugin, params)?;
                return Ok(Self::Vst2(Box::new(plugin)));
            }
            let format = if p.source.as_deref() == Some("clap") {
                PluginFormat::Clap
            } else {
                PluginFormat::Vst3
            };
            // In-process fake components stand in for installed multi-output
            // instruments in streaming/offline parity tests. They honour preset
            // state like a real component so a selected program survives the
            // render path instead of only living on the live instance.
            #[cfg(any(test, feature = "test-support"))]
            if format == PluginFormat::Vst3
                && let Some(mut instance) =
                    oc_host::vst3_instance::test_support::fake_instance_for_id(id, 4096)
            {
                let context =
                    oc_host::PluginStateContext::new(PluginFormat::Vst3, Some(id), &p.name);
                apply_vst3_preset_state_at(&mut instance, params, &context, root)?;
                apply_vst3_preset_params(&mut instance, params)?;
                return Ok(Self::Vst3(Box::new(instance)));
            }
            let profile = std::env::var_os("OCP_PROFILE_PLUGIN_LOAD").is_some();
            let lookup_start = std::time::Instant::now();
            let info = find_plugin_info(id, format)
                .ok_or_else(|| format!("Plugin {id} is not installed"))?;
            if profile {
                eprintln!(
                    "plugin_lookup_ms={:.3}",
                    lookup_start.elapsed().as_secs_f64() * 1000.0
                );
            }
            if p.source.as_deref() == Some("clap") {
                let params = params.to_vec();
                let resource_root = root.map(Path::to_path_buf);
                let preset_name = p.name.clone();
                let plugin_id = info.id.clone();
                let plugin = oc_host::clap_instance::ClapInstance::load_prepared_on_main(
                    Path::new(&info.path),
                    &info.id,
                    sample_rate,
                    4096,
                    move |plugin| {
                        let context = oc_host::PluginStateContext::new(
                            PluginFormat::Clap,
                            Some(plugin_id.as_str()),
                            &preset_name,
                        );
                        apply_clap_preset_state_at(
                            plugin,
                            &params,
                            &context,
                            resource_root.as_deref(),
                        )?;
                        apply_clap_preset_params(plugin, &params)?;
                        Ok(())
                    },
                )?;
                return Ok(Self::Clap(Box::new(plugin)));
            }
            let load_start = std::time::Instant::now();
            let mut plugin = oc_host::vst3_instance::Vst3Instance::load(
                Path::new(&info.path),
                sample_rate,
                4096,
            )?;
            if profile {
                eprintln!(
                    "plugin_instance_ms={:.3}",
                    load_start.elapsed().as_secs_f64() * 1000.0
                );
            }
            let state_start = std::time::Instant::now();
            if p.source.as_deref() == Some("sample") {
                plugin.set_state_bytes(&sfizz_vst3_state_bytes(p, root)?)?;
            } else {
                let context = oc_host::PluginStateContext::new(
                    PluginFormat::Vst3,
                    Some(info.id.as_str()),
                    &p.name,
                );
                apply_vst3_preset_state_at(&mut plugin, params, &context, root)?;
            }
            apply_vst3_preset_params(&mut plugin, params)?;
            plugin.bind_preset_edits(&p.name);
            if profile {
                eprintln!(
                    "plugin_state_ms={:.3}",
                    state_start.elapsed().as_secs_f64() * 1000.0
                );
            }
            return Ok(Self::Vst3(Box::new(plugin)));
        }
        match preset.and_then(|p| p.plugin.as_deref()) {
            Some("builtin.test_tone") => {
                let mut plugin = oc_host::builtin::create_builtin("builtin.test_tone").unwrap();
                for (key, value) in params {
                    if let Some(value) = value.as_f64() {
                        plugin.set_param(key, value);
                    }
                }
                Ok(Self::Builtin {
                    plugin,
                    active: false,
                })
            }
            Some(oc_synth::sampler::PLUGIN_ID) => {
                let sampler = oc_synth::sampler::Sampler::from_params(params, root)?;
                Ok(Self::Sampler(Box::new(sampler)))
            }
            Some(oc_synth::drumkit::PLUGIN_ID) => {
                let kit = oc_synth::drumkit::DrumKit::from_params(params, root, sample_rate)?;
                // A pad that cannot sound is a document mistake, not a render step.
                for warning in kit.warnings() {
                    crate::render_warnings::push(warning.clone());
                }
                Ok(Self::DrumKit(Box::new(kit)))
            }
            Some(oc_synth::synth::PLUGIN_ID) => {
                let patch = oc_synth::synth::load_project_patch(params, root)?;
                Ok(Self::Synth(Box::new(OpenSynth::new_with_tempo(
                    patch, tempo,
                ))))
            }
            _ if preset.is_none_or(|p| {
                p.source.as_deref().is_none_or(|s| s == "soundfont") && p.plugin.is_none()
            }) =>
            {
                let path = preset
                    .and_then(|p| p.file.as_deref())
                    .map(|file| match root {
                        Some(root) if Path::new(file).is_relative() => root.join(file),
                        _ => Path::new(file).to_path_buf(),
                    })
                    .unwrap_or_else(|| fallback_font.to_path_buf());
                if path.as_os_str().is_empty() {
                    return Err("SoundFont source requires a project preset file".into());
                }
                let mut renderer = match preparation {
                    Some(preparation) => SoundFontRenderer::new_streaming_with_options(
                        sample_rate,
                        &path,
                        preparation.events.iter().copied(),
                        true,
                        preparation.workers,
                    )?,
                    None => SoundFontRenderer::new(sample_rate, &path)?,
                };
                renderer.apply_preset_output(preset);
                renderer.enable_drum_channel();
                Ok(Self::SoundFont(Box::new(renderer)))
            }
            Some(id)
                if preset.is_some_and(|p| p.source.as_deref() == Some("builtin"))
                    || id.starts_with("builtin.") =>
            {
                Err(format!(
                    "Built-in instrument plugin '{id}' for preset '{}' has no block source adapter",
                    preset.map_or("", |p| p.name.as_str()),
                ))
            }
            _ => Err(
                "Instrument has no block source adapter; refusing SoundFont substitution".into(),
            ),
        }
    }

    /// Send a native voice event to the built-in synth.
    pub fn send_native_voice(
        &mut self,
        event: &oc_synth::synth::NativeVoiceEvent,
    ) -> Result<bool, String> {
        match self {
            Self::Synth(synth) => synth.send_native_voice(event),
            _ => Err("native voice events require builtin.open_synth".into()),
        }
    }

    /// Install a track's hairpin layer ramps and its `dynamic_layers` opt-out
    /// on a streaming lane, and place the lane at `offset` project frames.
    /// Sources other than SoundFont ignore it.
    pub fn install_hairpin_layers(
        &mut self,
        preset: Option<&PresetDef>,
        project: &Project,
        track: &Track,
        properties: &ProjectProperties,
        offset: u64,
    ) -> Result<(), String> {
        match self {
            Self::SoundFont(renderer) => {
                renderer.install_hairpin_layers(preset, project, track, properties, offset)
            }
            _ => Ok(()),
        }
    }

    /// Move the lane's clock to an absolute project frame. Only a SoundFont
    /// lane reads it; every other source is driven by the block transport.
    pub fn set_project_frame(&mut self, frame: u64) {
        if let Self::SoundFont(renderer) = self {
            renderer.set_project_frame(frame);
        }
    }

    /// Return the source to its construction state, keeping the instance and
    /// every loaded asset. Returns `false` when the source cannot restore its
    /// own DSP state, so the caller rebuilds that lane alone.
    pub fn rewind(&mut self) -> bool {
        match self {
            Self::Synth(synth) => {
                synth.reset();
                true
            }
            Self::Performance(performance) => {
                performance.reset();
                true
            }
            Self::Sampler(sampler) => {
                sampler.reset();
                true
            }
            Self::DrumKit(kit) => {
                kit.reset();
                true
            }
            Self::SoundFont(renderer) => {
                renderer.reset();
                true
            }
            Self::Clap(plugin) => plugin.reset(),
            Self::Vst3(plugin) => plugin.reset(),
            Self::Lv2(plugin) => {
                plugin.reset_state();
                true
            }
            Self::Vst2(plugin) => {
                plugin.reset();
                true
            }
            #[cfg(target_os = "macos")]
            Self::Au(plugin) => {
                plugin.reset();
                true
            }
            Self::Builtin { plugin, active } => {
                plugin.reset();
                *active = false;
                true
            }
            Self::Silent => true,
        }
    }

    /// Dispatch a score MIDI event to the source.
    pub fn send(&mut self, event: &MidiMessage) -> Result<(), String> {
        if let MidiMessage::NativeVoice(event) = event {
            return self.send_native_voice(event).map(|_| ());
        }
        if matches!(self, Self::Performance(_)) {
            return Err("performance source refuses score-derived MIDI events".into());
        }
        let Self::Synth(s) = self else {
            match self {
                Self::SoundFont(f) => f.send(event)?,
                Self::Clap(p) => {
                    crate::render_cache::queue_plugin_midi_event(p.as_mut(), event, 0)?
                }
                Self::Vst3(p) => {
                    crate::render_cache::queue_plugin_midi_event(p.as_mut(), event, 0)?
                }
                Self::Lv2(p) => crate::render_cache::queue_plugin_midi_event(p.as_mut(), event, 0)?,
                Self::Vst2(p) => {
                    crate::render_cache::queue_plugin_midi_event(p.as_mut(), event, 0)?
                }
                #[cfg(target_os = "macos")]
                Self::Au(p) => crate::render_cache::queue_plugin_midi_event(p.as_mut(), event, 0)?,
                Self::Builtin { active, .. } => match event {
                    MidiMessage::NoteOn { .. } => *active = true,
                    MidiMessage::NoteOff { .. } => *active = false,
                    _ => {}
                },
                Self::Sampler(sampler) => sampler.send_midi(event),
                Self::DrumKit(kit) => match event {
                    MidiMessage::NoteOn { note, velocity, .. } => {
                        kit.note_on(*note, *velocity);
                    }
                    MidiMessage::NoteOff { note, .. } => kit.note_off(*note),
                    _ => {}
                },
                _ => {}
            }
            return Ok(());
        };
        s.send_midi(event)
    }

    /// Queue VST3 events relative to the upcoming process block, as in export.
    pub fn send_vst3_at(&mut self, event: &MidiMessage, offset: i32) -> Result<(), String> {
        match self {
            Self::Vst3(plugin) if offset >= 0 => {
                crate::render_cache::queue_plugin_midi_event(plugin.as_mut(), event, offset)
            }
            _ => Err("offset event requires a VST3 source and nonnegative offset".into()),
        }
    }

    /// Reported latency of the hosted instrument.
    pub fn latency_samples(&self) -> u32 {
        match self {
            Self::Clap(p) => p.latency_samples(),
            Self::Vst3(p) => p.latency_samples(),
            Self::Lv2(p) => p.latency_samples(),
            Self::Vst2(p) => p.latency_samples(),
            #[cfg(target_os = "macos")]
            Self::Au(p) => p.latency_samples(),
            _ => 0,
        }
    }

    /// Declared output ports of the hosted instrument, in order.
    pub fn output_ports(&self) -> Vec<oc_host::ports::AudioPort> {
        match self {
            Self::Clap(p) => p.output_ports(),
            Self::Vst3(p) => p.output_ports(),
            Self::Lv2(p) => p.output_ports(),
            Self::Vst2(p) => p.output_ports(),
            #[cfg(target_os = "macos")]
            Self::Au(p) => p.output_ports(),
            _ => Vec::new(),
        }
    }

    /// Stereo output pairs the source can fan out beyond the main pair.
    /// `None` means the source hosts no plugin outputs at all.
    pub fn aux_capacity(&self) -> Option<usize> {
        match self {
            Self::Clap(p) => Some(p.output_count()),
            Self::Vst3(p) => Some(p.output_count()),
            Self::Lv2(_) => None,
            Self::Vst2(p) => Some(p.output_count()),
            #[cfg(target_os = "macos")]
            Self::Au(p) => Some(p.output_count()),
            _ => None,
        }
    }

    /// Render one block into caller-reserved buffers.
    ///
    /// Plugin sources fan the main pair and every planned auxiliary pair out of
    /// one process call. Buffers must come from `SourceOutput::new` so steady
    /// blocks only reuse memory.
    pub fn render_into(
        &mut self,
        frames: usize,
        sample_rate: u32,
        transport: oc_host::PluginTransport,
        output: &mut SourceOutput,
    ) {
        output.clear();
        match self {
            Self::Synth(s) => {
                s.set_tempo(transport.current_tempo(), sample_rate)
                    .expect("validated project tempo");
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                s.render_interleaved(main, sample_rate);
            }
            Self::Performance(p) => {
                p.set_tempo(transport.current_tempo(), sample_rate)
                    .expect("validated project tempo");
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                for frame in main.chunks_exact_mut(2) {
                    frame.copy_from_slice(&p.next_stereo(sample_rate));
                }
            }
            Self::Sampler(sampler) => {
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                sampler
                    .render_into(main, sample_rate)
                    .expect("complete stereo source frames");
            }
            Self::DrumKit(kit) => {
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                kit.render_into(main, sample_rate)
                    .expect("complete stereo source frames");
            }
            Self::SoundFont(f) => {
                // Render straight into the reserved main pair: no per-block Vec.
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                f.render_into(main);
            }
            Self::Clap(p) => {
                if p.process_block_outputs_into(
                    frames as u32,
                    Some(transport),
                    &output.requested,
                    &mut output.buffers,
                )
                .is_err()
                {
                    output.fill_silence(frames);
                }
            }
            Self::Vst3(p) => {
                if p.process_block_outputs_into(
                    frames as u32,
                    Some(transport),
                    &output.requested,
                    &mut output.buffers,
                )
                .is_err()
                {
                    output.fill_silence(frames);
                }
            }
            Self::Lv2(p) => {
                let rendered = p.process_block_with_transport(frames as u32, Some(transport));
                output.buffers[0].extend_from_slice(rendered);
            }
            Self::Vst2(p) => {
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                let _ = p.process_block_into(main, Some(transport));
            }
            #[cfg(target_os = "macos")]
            Self::Au(p) => {
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                let _ = p.process_block_into(main, Some(&transport));
            }
            Self::Silent => output.buffers[0].resize(frames * 2, 0.0),
            Self::Builtin { plugin, active } => {
                let main = &mut output.buffers[0];
                main.resize(frames * 2, 0.0);
                if *active {
                    plugin.process_with_transport(main, sample_rate, Some(transport));
                }
            }
        }
    }

    /// Render one block into a fresh main buffer.
    pub fn render(
        &mut self,
        frames: usize,
        sample_rate: u32,
        transport: oc_host::PluginTransport,
    ) -> Vec<f32> {
        let mut output = SourceOutput::new(0, &[]);
        self.render_into(frames, sample_rate, transport, &mut output);
        output.main().to_vec()
    }

    /// Hosted instrument tail used by the idle gate. `None` when this source
    /// hosts no plugin or its format reports no tail.
    pub fn plugin_tail_samples(&self) -> Option<u32> {
        match self {
            Self::Clap(plugin) => plugin.plugin_tail_samples(),
            Self::Vst3(plugin) => plugin.plugin_tail_samples(),
            Self::Vst2(plugin) => plugin.plugin_tail_samples(),
            #[cfg(target_os = "macos")]
            Self::Au(plugin) => plugin.plugin_tail_samples(),
            _ => None,
        }
    }
}

/// Streaming lane source with the smart-disable gate for its hosted instrument.
///
/// A hosted instrument that renders silence for [`idle_gate::IDLE_SECONDS`]
/// while it receives no event stops processing until an event arrives. The
/// offline renderer builds its own instances and never uses this type.
pub struct SmartSource {
    source: BlockSource,
    gate: idle_gate::IdleGate,
    /// An instrument event arrived since the last block.
    input: bool,
    /// Test hook: the lane stands in for a plugin that cannot restore its DSP
    /// state, so a rewind must rebuild it.
    #[cfg(any(test, feature = "test-support"))]
    non_rewindable: bool,
    /// Test hook: distinguishes a kept instance from a rebuilt one.
    #[cfg(any(test, feature = "test-support"))]
    id: u64,
}

#[cfg(any(test, feature = "test-support"))]
static NEXT_SOURCE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl SmartSource {
    /// Poll a failure authority; this never enters a native plugin.
    pub fn processing_failed(&self) -> bool {
        match &self.source {
            BlockSource::Clap(instance) => instance.processing_failed(),
            BlockSource::Vst2(instance) => instance.processing_failed(),
            #[cfg(target_os = "macos")]
            BlockSource::Au(instance) => instance.processing_failed(),
            BlockSource::Builtin { plugin, .. } => plugin.processing_failed(),
            _ => false,
        }
    }

    /// Consume a detailed failure only after leaving audio processing.
    pub fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        match &self.source {
            BlockSource::Clap(instance) => instance.take_processing_failure(),
            BlockSource::Vst2(instance) => instance.take_processing_failure(),
            #[cfg(target_os = "macos")]
            BlockSource::Au(instance) => instance.take_processing_failure(),
            BlockSource::Builtin { plugin, .. } => plugin.take_processing_failure(),
            _ => oc_audio::ProcessingFailureReport::Unavailable,
        }
    }

    /// Enable the gate unless the lane opts out. Built-in sources have no
    /// reported tail and keep processing.
    pub fn new(source: BlockSource, sample_rate: u32, smart_disable: bool) -> Self {
        let mut gate = match source.plugin_tail_samples() {
            Some(tail) => idle_gate::IdleGate::new(sample_rate, Some(tail), !smart_disable),
            None => idle_gate::IdleGate::disabled(),
        };
        gate.set_streaming(true);
        Self {
            source,
            gate,
            input: false,
            #[cfg(any(test, feature = "test-support"))]
            non_rewindable: false,
            #[cfg(any(test, feature = "test-support"))]
            id: NEXT_SOURCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Mark this lane as unable to restore its own state, for the rebuild path.
    #[cfg(any(test, feature = "test-support"))]
    pub fn mark_non_rewindable(&mut self) {
        self.non_rewindable = true;
    }

    /// Identity of this lane's instance, so a test can tell a kept lane from a
    /// rebuilt one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn instance_id(&self) -> u64 {
        self.id
    }

    /// Return the lane to its construction state, keeping the instance.
    /// Returns `false` when the caller must rebuild this lane.
    pub fn rewind(&mut self) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if self.non_rewindable {
            return false;
        }
        self.input = false;
        self.gate.rewind();
        self.source.rewind()
    }

    /// Whether the lane hosts a VST3 instance.
    pub fn is_vst3(&self) -> bool {
        matches!(self.source, BlockSource::Vst3(_))
    }

    /// Hosted performance sources preserve plugin processing quantum and event offsets.
    pub fn performance_uses_offsets(&self) -> bool {
        matches!(
            self.source,
            BlockSource::Clap(_) | BlockSource::Vst2(_) | BlockSource::Vst3(_)
        )
    }

    /// Send a performance command through the idle gate.
    pub fn send_performance_at(
        &mut self,
        command: &oc_synth::performance::ScheduledCommand,
        offset: u32,
        block_frames: u32,
    ) -> Result<(), String> {
        self.input = true;
        self.source
            .send_performance_at(command, offset, block_frames)
    }

    /// Whether the lane has no instrument source.
    pub fn is_silent(&self) -> bool {
        matches!(self.source, BlockSource::Silent)
    }

    /// Reported source latency.
    pub fn latency_samples(&self) -> u32 {
        self.source.latency_samples()
    }

    /// Declared output ports.
    pub fn output_ports(&self) -> Vec<oc_host::ports::AudioPort> {
        self.source.output_ports()
    }

    /// Fan-out capacity of the source.
    pub fn aux_capacity(&self) -> Option<usize> {
        self.source.aux_capacity()
    }

    /// Feed one instrument event; the first event of a paused lane resumes it.
    pub fn send(&mut self, event: &MidiMessage) -> Result<(), String> {
        self.input = true;
        self.source.send(event)
    }

    /// Queue a VST3 event at a block-relative offset.
    pub fn send_vst3_at(&mut self, event: &MidiMessage, offset: i32) -> Result<(), String> {
        self.input = true;
        self.source.send_vst3_at(event, offset)
    }

    /// Install the lane's hairpin layer ramps; see [`BlockSource`].
    pub fn install_hairpin_layers(
        &mut self,
        preset: Option<&PresetDef>,
        project: &Project,
        track: &Track,
        properties: &ProjectProperties,
        offset: u64,
    ) -> Result<(), String> {
        self.source
            .install_hairpin_layers(preset, project, track, properties, offset)
    }

    /// Move the lane's clock to an absolute project frame; see [`BlockSource`].
    pub fn set_project_frame(&mut self, frame: u64) {
        self.source.set_project_frame(frame);
    }

    /// Render one block, skipping a paused plugin's process call.
    pub fn render_into(
        &mut self,
        frames: usize,
        sample_rate: u32,
        transport: oc_host::PluginTransport,
        output: &mut SourceOutput,
    ) {
        if !self.gate.is_active() {
            self.source
                .render_into(frames, sample_rate, transport, output);
            self.input = false;
            return;
        }
        let input_silent = !std::mem::take(&mut self.input);
        match self.gate.begin(input_silent) {
            idle_gate::GateAction::Skip => output.fill_silence(frames),
            // Resume block: run the plugin to advance its own state, but keep
            // its contribution out of the mix until the fade-in block.
            idle_gate::GateAction::Mute => {
                self.source
                    .render_into(frames, sample_rate, transport, output);
                output.zero();
            }
            idle_gate::GateAction::Process => {
                self.source
                    .render_into(frames, sample_rate, transport, output);
                let peak = output.peak();
                if let Some(ramp) = self.gate.end(input_silent, peak, frames) {
                    output.apply_ramp(ramp);
                }
            }
        }
    }

    /// Render one block, respecting the idle gate.
    pub fn render(
        &mut self,
        frames: usize,
        sample_rate: u32,
        transport: oc_host::PluginTransport,
    ) -> Vec<f32> {
        let mut output = SourceOutput::new(0, &[]);
        self.render_into(frames, sample_rate, transport, &mut output);
        output.main().to_vec()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn performance_streaming_matches_offline_across_blocks_and_seek_replay() {
        use super::{BlockSource, SourceOutput};
        use oc_dsl::performance::ChannelCommand::*;
        use oc_synth::performance::ScheduledCommand;
        const RATE: u32 = 8000;
        const FRAMES: usize = 1536;
        let project = oc_dsl::parse("@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n").unwrap();
        let commands: Vec<_> = [
            (
                0,
                0,
                NoteOn {
                    note: 60,
                    velocity: 100,
                },
            ),
            (
                31,
                1,
                NoteOn {
                    note: 67,
                    velocity: 90,
                },
            ),
            (
                127,
                0,
                ControlChange {
                    controller: 10,
                    value: 0,
                },
            ),
            (
                128,
                1,
                ControlChange {
                    controller: 11,
                    value: 64,
                },
            ),
            (
                179,
                0,
                ControlChange {
                    controller: 64,
                    value: 127,
                },
            ),
            (
                255,
                0,
                NoteOff {
                    note: 60,
                    velocity: 0,
                },
            ),
            (
                255,
                0,
                NoteOn {
                    note: 64,
                    velocity: 105,
                },
            ),
            (301, 1, PitchBend { value: 1100 }),
            (
                411,
                0,
                ControlChange {
                    controller: 7,
                    value: 80,
                },
            ),
            (
                563,
                0,
                ControlChange {
                    controller: 64,
                    value: 0,
                },
            ),
            (
                702,
                1,
                ControlChange {
                    controller: 121,
                    value: 0,
                },
            ),
            (
                913,
                0,
                NoteOff {
                    note: 64,
                    velocity: 0,
                },
            ),
            (
                1001,
                1,
                NoteOff {
                    note: 67,
                    velocity: 0,
                },
            ),
        ]
        .into_iter()
        .map(|(sample, channel, message)| ScheduledCommand {
            sample,
            channel,
            message,
        })
        .collect();
        let preset = &project.presets[0];
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let reference = oc_synth::native_performance::render(
            &commands,
            FRAMES,
            RATE,
            oc_synth::synth::load_project_patch(&preset.params, None).unwrap(),
            None,
            &clock,
        )
        .unwrap();
        let stream = |max_block: usize, seek: usize| {
            let mut source = BlockSource::new_performance(
                Some(preset),
                &preset.params,
                std::path::Path::new("unused.sf2"),
                RATE,
                120.0,
                &commands,
                None,
            )
            .unwrap();
            assert!(source.send_performance_at(&commands[0], 1, 128).is_err());
            assert!(
                source
                    .send(&crate::interpret::MidiMessage::NoteOn {
                        channel: 0,
                        note: 60,
                        velocity: 100
                    })
                    .is_err()
            );
            let mut position = 0;
            let mut index = 0;
            let mut output = SourceOutput::new(0, &[]);
            let mut result = Vec::new();
            while position < FRAMES {
                while index < commands.len() && commands[index].sample == position as u64 {
                    source.send_performance_at(&commands[index], 0, 1).unwrap();
                    index += 1;
                }
                let next = commands
                    .get(index)
                    .map_or(FRAMES, |event| event.sample as usize);
                let end = (position + max_block).min(next).min(FRAMES);
                source.render_into(
                    end - position,
                    RATE,
                    oc_host::PluginTransport::new(position as u64, RATE, 120.0),
                    &mut output,
                );
                let keep = seek.saturating_sub(position).min(end - position);
                result.extend_from_slice(&output.main()[keep * 2..]);
                position = end;
            }
            result
        };
        assert!(reference.iter().any(|sample| sample.abs() > 0.001));
        assert_eq!(stream(128, 0), reference);
        assert_eq!(stream(37, 0), reference);
        assert_eq!(stream(73, 607), reference[607 * 2..]);
    }

    #[test]
    fn wavetable_paths_follow_project_root() {
        let mut project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n",
        )
        .unwrap();
        project.presets[0].params.push((
            "wavetable.file".into(),
            oc_dsl::ast::ParamValue::String("assets/table.wav".into()),
        ));
        assert!(super::prepare_project(&project, None).is_err());
        let resolved =
            super::prepare_project(&project, Some(std::path::Path::new("/project"))).unwrap();
        assert_eq!(
            resolved.presets[0].params[0].1,
            oc_dsl::ast::ParamValue::String("/project/assets/table.wav".into())
        );
        assert_eq!(
            project.presets[0].params[0].1,
            oc_dsl::ast::ParamValue::String("assets/table.wav".into())
        );
    }

    #[test]
    fn project_resources_resolve_without_environment_or_document_mutation() {
        let project = oc_dsl::parse(
            "@project\n@preset p\n  source: soundfont\n  file: \"assets/piano.sf2\"\n",
        )
        .unwrap();
        assert!(super::prepare_project(&project, None).is_err());
        let resolved =
            super::prepare_project(&project, Some(std::path::Path::new("/project"))).unwrap();
        assert_eq!(
            resolved.presets[0].file.as_deref(),
            Some("/project/assets/piano.sf2")
        );
        assert_eq!(project.presets[0].file.as_deref(), Some("assets/piano.sf2"));
    }

    #[test]
    fn audio_clip_media_paths_follow_project_root() {
        let project = oc_dsl::parse(
            "@project\n@audio_clip hit [1:1 - 2:1] | track: tape\n  file: \"media/hit.wav\"\n@track tape | instrument: audio/clip\n",
        )
        .unwrap();
        assert!(super::prepare_project(&project, None).is_err());
        let resolved =
            super::prepare_project(&project, Some(std::path::Path::new("/project"))).unwrap();
        assert_eq!(
            resolved.audio_clip(0).unwrap().file,
            "/project/media/hit.wav"
        );
        assert_eq!(project.audio_clip(0).unwrap().file, "media/hit.wav");
    }

    mod smart_disable {
        use super::super::{BlockSource, SmartSource, SourceOutput};
        use crate::interpret::MidiMessage;
        use oc_host::vst3_instance::test_support::{FakeControl, fake_instance};
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        const BLOCK: usize = 128;
        const RATE: u32 = 48_000;

        fn transport(position: u64) -> oc_host::PluginTransport {
            oc_host::PluginTransport::new(position, RATE, 120.0)
        }

        fn note_on() -> MidiMessage {
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            }
        }

        fn note_off() -> MidiMessage {
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            }
        }

        /// Hosted instrument that renders only while a note is held.
        fn gated_source(control: &Arc<FakeControl>, smart_disable: bool) -> SmartSource {
            control.note_gated.store(true, Ordering::Relaxed);
            let instance = fake_instance(control, 2, 512);
            SmartSource::new(BlockSource::Vst3(Box::new(instance)), RATE, smart_disable)
        }

        fn calls(control: &FakeControl) -> usize {
            control.process_calls.load(Ordering::Relaxed)
        }

        fn idle_blocks() -> usize {
            (RATE as usize * 4) / BLOCK
        }

        #[test]
        fn a_silent_instrument_pauses_and_a_transient_resumes_it() {
            let control = Arc::new(FakeControl::default());
            let mut source = gated_source(&control, true);
            let mut output = SourceOutput::new(0, &[]);

            source.send(&note_on()).unwrap();
            source.render_into(BLOCK, RATE, transport(0), &mut output);
            assert_eq!(calls(&control), 1);
            assert!(
                output.main().iter().any(|sample| sample.abs() > 0.1),
                "the held note sounds"
            );

            // Release: this block carries input, so the window restarts here.
            source.send(&note_off()).unwrap();
            source.render_into(BLOCK, RATE, transport(BLOCK as u64), &mut output);
            assert!(output.main().iter().all(|sample| *sample == 0.0));
            let after_release = calls(&control);

            // Six seconds of silence: four to pause, two more skipped.
            let six_seconds = idle_blocks() * 3 / 2;
            for block in 0..six_seconds {
                let position = ((block + 2) * BLOCK) as u64;
                source.render_into(BLOCK, RATE, transport(position), &mut output);
            }
            let paused = calls(&control);
            assert_eq!(
                paused,
                after_release + idle_blocks(),
                "the plugin stops after four seconds of silence"
            );
            assert!(
                output.main().iter().all(|sample| *sample == 0.0),
                "a skipped block is silence"
            );

            // A transient resumes processing on its own block, muted, then
            // fades in one block later.
            let resume_position = ((six_seconds + 2) * BLOCK) as u64;
            source.send(&note_on()).unwrap();
            source.render_into(BLOCK, RATE, transport(resume_position), &mut output);
            assert_eq!(
                calls(&control),
                paused + 1,
                "the transient resumes processing within one block"
            );
            assert!(
                output.main().iter().all(|sample| *sample == 0.0),
                "the resume block is muted"
            );

            source.render_into(
                BLOCK,
                RATE,
                transport(resume_position + BLOCK as u64),
                &mut output,
            );
            assert_eq!(calls(&control), paused + 2);
            assert_eq!(output.main()[0], 0.0, "the fade-in starts at silence");
            assert!(
                output.main()[(BLOCK - 1) * 2].abs() > 0.1,
                "the fade-in reaches the instrument output"
            );
        }

        #[test]
        fn a_long_tail_instrument_keeps_processing_through_silence() {
            let control = Arc::new(FakeControl::default());
            control.tail_samples.store(10 * RATE, Ordering::Relaxed);
            let mut source = gated_source(&control, true);
            let mut output = SourceOutput::new(0, &[]);
            for block in 0..idle_blocks() + 8 {
                source.render_into(BLOCK, RATE, transport((block * BLOCK) as u64), &mut output);
            }
            assert_eq!(
                calls(&control),
                idle_blocks() + 8,
                "a tail longer than the window keeps the plugin running"
            );
        }

        #[test]
        fn an_opted_out_instrument_keeps_processing() {
            let control = Arc::new(FakeControl::default());
            let mut source = gated_source(&control, false);
            let mut output = SourceOutput::new(0, &[]);
            for block in 0..idle_blocks() + 8 {
                source.render_into(BLOCK, RATE, transport((block * BLOCK) as u64), &mut output);
            }
            assert_eq!(calls(&control), idle_blocks() + 8);
        }

        #[test]
        fn a_source_without_tail_information_keeps_rendering() {
            // A built-in source reports no plugin tail, so the gate stays inert
            // and a held note keeps rendering past the idle window.
            let mut source = SmartSource::new(
                BlockSource::Synth(Box::new(oc_synth::synth::OpenSynth::new(
                    oc_synth::synth::SynthPatch::default(),
                ))),
                RATE,
                true,
            );
            source.send(&note_on()).unwrap();
            let mut output = SourceOutput::new(0, &[]);
            let mut peak = 0.0_f32;
            for block in 0..idle_blocks() + 8 {
                source.render_into(BLOCK, RATE, transport((block * BLOCK) as u64), &mut output);
                for sample in output.main() {
                    peak = peak.max(sample.abs());
                }
            }
            assert!(peak > 0.0, "a source with no tail report keeps rendering");
        }
    }
}

#[cfg(test)]
mod prepared_sample_tests {
    use super::{BlockSource, SourceOutput};
    use crate::interpret::MidiMessage;
    use oc_dsl::ast::ParamValue;
    use std::path::Path;

    const RATE: u32 = 8_000;

    struct SampleFixture(tempfile::NamedTempFile);

    impl SampleFixture {
        fn new() -> Self {
            let fixture = Self(tempfile::NamedTempFile::new().unwrap());
            let mut writer = hound::WavWriter::new(
                fixture.0.reopen().unwrap(),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for frame in 0..4096 {
                let value = (std::f64::consts::TAU * 440.0 * frame as f64 / RATE as f64).sin();
                writer
                    .write_sample((value * 0.6 * i16::MAX as f64) as i16)
                    .unwrap();
            }
            writer.finalize().unwrap();
            fixture
        }
    }

    fn source(drum: bool, file: &Path) -> BlockSource {
        if drum {
            let params = vec![
                ("pad.tone.note".into(), ParamValue::Number(60.0)),
                ("pad.tone.one_shot".into(), ParamValue::Bool(false)),
                ("layer.tone.0.low".into(), ParamValue::Number(1.0)),
                ("layer.tone.0.high".into(), ParamValue::Number(127.0)),
                (
                    "layer.tone.0.sample".into(),
                    ParamValue::String(file.display().to_string()),
                ),
            ];
            BlockSource::DrumKit(Box::new(
                oc_synth::drumkit::DrumKit::from_params(&params, None, RATE).unwrap(),
            ))
        } else {
            let params = vec![(
                "sampler.sample_file".into(),
                ParamValue::String(file.display().to_string()),
            )];
            BlockSource::Sampler(Box::new(
                oc_synth::sampler::Sampler::from_params(&params, None).unwrap(),
            ))
        }
    }

    fn scalar_reference(source: &mut BlockSource, frames: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let pair = match source {
                BlockSource::Sampler(sampler) => sampler.next_stereo(RATE),
                BlockSource::DrumKit(kit) => kit.next_stereo(RATE),
                _ => unreachable!(),
            };
            output.extend_from_slice(&pair);
        }
        output
    }

    fn note_on() -> MidiMessage {
        MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        }
    }

    #[test]
    fn prepared_sample_sources_first_event_partial_blocks_and_release_are_heap_free() {
        let sample = SampleFixture::new();
        for drum in [false, true] {
            let mut actual = source(drum, sample.0.path());
            let mut reference = source(drum, sample.0.path());
            let mut output = SourceOutput::new(0, &[]);
            let mut cursor = 0;
            for (block, frames) in [1, 127, 0, 31, 128, 17, 128].into_iter().enumerate() {
                let event = match block {
                    0 | 5 => Some(note_on()),
                    3 => Some(MidiMessage::NoteOff {
                        channel: 0,
                        note: 60,
                    }),
                    _ => None,
                };
                if let Some(event) = &event {
                    reference.send(event).unwrap();
                }
                let expected = scalar_reference(&mut reference, frames);
                let operations = crate::test_alloc::operations(|| {
                    if let Some(event) = &event {
                        actual.send(event).unwrap();
                    }
                    actual.render_into(
                        frames,
                        RATE,
                        oc_host::PluginTransport::new(cursor, RATE, 120.0),
                        &mut output,
                    );
                });
                assert_eq!(operations, 0, "drum={drum}, block={block}");
                assert_eq!(output.main(), expected, "drum={drum}, block={block}");
                if block == 1 {
                    assert!(output.main().iter().any(|sample| *sample != 0.0));
                }
                cursor += frames as u64;
            }
        }
    }

    #[test]
    fn prepared_sample_slice_validation_preserves_output_and_voice_state() {
        let sample = SampleFixture::new();
        for drum in [false, true] {
            let mut actual = source(drum, sample.0.path());
            let mut reference = source(drum, sample.0.path());
            actual.send(&note_on()).unwrap();
            reference.send(&note_on()).unwrap();
            let mut odd = [9.0; 7];
            let operations = crate::test_alloc::operations(|| {
                let result = match &mut actual {
                    BlockSource::Sampler(sampler) => sampler.render_into(&mut odd, RATE),
                    BlockSource::DrumKit(kit) => kit.render_into(&mut odd, RATE),
                    _ => unreachable!(),
                };
                assert_eq!(result, Err(oc_audio::StereoBufferError::IncompleteFrame));
                let result = match &mut actual {
                    BlockSource::Sampler(sampler) => sampler.render_into(&mut [], RATE),
                    BlockSource::DrumKit(kit) => kit.render_into(&mut [], RATE),
                    _ => unreachable!(),
                };
                assert_eq!(result, Ok(()));
            });
            assert_eq!(operations, 0);
            assert_eq!(odd, [9.0; 7]);
            for frames in [1, 31, 128] {
                let expected = scalar_reference(&mut reference, frames);
                let actual = match &mut actual {
                    BlockSource::Sampler(sampler) => sampler.render_interleaved(frames, RATE),
                    BlockSource::DrumKit(kit) => kit.render_interleaved(frames, RATE),
                    _ => unreachable!(),
                };
                assert_eq!(actual, expected, "drum={drum}, frames={frames}");
            }
        }
    }
}

#[cfg(test)]
mod prepared_vst2_tests {
    use super::{BlockSource, SmartSource, SourceOutput};
    use oc_audio::{AudioEffect, PluginTransport, ProcessingFailureReport};
    use oc_host::vst2::{MAX_PENDING_MIDI, Vst2Instance, Vst2ProcessError};

    const RATE: u32 = 48_000;
    fn fixture(name: &str) -> Vst2Instance {
        let path = oc_host::vst2::test_support::fixture_path(name)
            .expect("the host build must provide its native VST2 fixture");
        Vst2Instance::load(&path, RATE, 128).unwrap()
    }

    #[test]
    fn prepared_vst2_first_note_partial_reset_and_source_adapter_have_zero_heap_operations() {
        let mut plugin = fixture("OpenCompose Tone Fixture");
        let mut output = [0.0; 256];
        assert_eq!(
            crate::test_alloc::operations(|| {
                plugin.note_on(60, 1.0, 0);
                plugin
                    .process_block_into(&mut output, Some(PluginTransport::new(0, RATE, 137.0)))
                    .unwrap();
            }),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.5));
        assert_eq!(plugin.last_time_info().tempo, 137.0);
        for frames in [1, 31, 0, 128] {
            assert_eq!(
                crate::test_alloc::operations(|| plugin
                    .process_block_into(&mut output[..frames * 2], None)
                    .unwrap()),
                0
            );
            assert!(output[..frames * 2].iter().all(|sample| *sample == 0.5));
        }
        assert_eq!(
            crate::test_alloc::operations(|| {
                plugin.note_off(60, 0);
                plugin.process_block_into(&mut output, None).unwrap();
                plugin.reset();
                plugin.note_on(64, 1.0, 0);
                plugin.process_block_into(&mut output, None).unwrap();
            }),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.5));
        let mut source = SmartSource::new(BlockSource::Vst2(Box::new(plugin)), RATE, false);
        let mut pairs = SourceOutput::new(0, &[]);
        assert_eq!(
            crate::test_alloc::operations(|| source.render_into(
                128,
                RATE,
                PluginTransport::new(256, RATE, 137.0),
                &mut pairs
            )),
            0
        );
        assert_eq!(pairs.main(), output);
        assert!(!source.processing_failed());
    }

    #[test]
    fn prepared_vst2_effect_named_parameter_and_reprepare_preserve_exact_audio_without_heap() {
        let mut plugin = fixture("OpenCompose Gain Fixture");
        let mut output = [0.25; 256];
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert!(plugin.set_param_by_name(" gain ", 0.75));
                assert!(plugin.set_param_by_name("dc_offset", 0.5));
                plugin.process_effect_into(&mut output, RATE, None).unwrap();
            }),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.5));
        assert_eq!(
            crate::test_alloc::operations(|| {
                AudioEffect::reset(&mut plugin);
                plugin.prepare_block_buffers(128).unwrap();
                plugin
                    .process_effect_into(&mut output[..62], RATE, None)
                    .unwrap();
            }),
            0
        );
        assert!(output[..62].iter().all(|sample| *sample == 0.875));
        let expected = plugin.process_block_with_transport(31, None);
        let mut actual = [0.0; 62];
        assert_eq!(
            crate::test_alloc::operations(|| plugin.process_block_into(&mut actual, None).unwrap()),
            0
        );
        assert_eq!(actual.as_slice(), expected);
    }

    #[test]
    fn prepared_vst2_rejections_leave_output_and_queued_first_note_intact() {
        let mut plugin = fixture("OpenCompose Tone Fixture");
        plugin.note_on(60, 1.0, 0);
        let mut oversized = [9.0; 258];
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert_eq!(
                    plugin.process_block_into(&mut oversized, None),
                    Err(Vst2ProcessError::CapacityExceeded)
                );
            }),
            0
        );
        assert_eq!(oversized, [9.0; 258]);
        assert!(plugin.processing_failed());
        assert!(matches!(
            plugin.take_processing_failure(),
            ProcessingFailureReport::Ready(_)
        ));
        assert_eq!(
            plugin.take_processing_failure(),
            ProcessingFailureReport::Reported
        );
        plugin.prepare_block_buffers(129).unwrap();
        assert_eq!(
            crate::test_alloc::operations(|| plugin
                .process_block_into(&mut oversized, None)
                .unwrap()),
            0
        );
        assert!(
            oversized.iter().all(|sample| *sample == 0.5),
            "refused block did not consume note-on"
        );
        let mut odd = [7.0; 3];
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert_eq!(
                    plugin.process_block_into(&mut odd, None),
                    Err(Vst2ProcessError::IncompleteFrame)
                );
            }),
            0
        );
        assert_eq!(odd, [7.0; 3]);
        plugin.prepare_block_buffers(129).unwrap();
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert_eq!(
                    plugin.process_effect_into(&mut oversized, 44_100, None),
                    Err(Vst2ProcessError::SampleRateChanged)
                );
            }),
            0
        );
        assert!(oversized.iter().all(|sample| *sample == 0.5));
        assert_eq!(
            plugin.prepare_block_buffers(usize::MAX),
            Err(oc_audio::BlockPreparationError::CapacityExceeded)
        );
    }

    #[test]
    fn prepared_vst2_topology_callback_refuses_block_until_control_plane_prepares() {
        let mut plugin = fixture("OpenCompose Tone Fixture");
        plugin.note_on(60, 1.0, 0);
        let mut output = [9.0; 256];
        assert_eq!(
            crate::test_alloc::operations(|| {
                oc_host::vst2::test_support::notify_io_changed(&plugin);
                assert_eq!(
                    plugin.process_block_into(&mut output, None),
                    Err(Vst2ProcessError::TopologyChanged)
                );
            }),
            0
        );
        assert_eq!(output, [9.0; 256]);
        plugin.prepare_block_buffers(128).unwrap();
        assert_eq!(
            crate::test_alloc::operations(|| plugin.process_block_into(&mut output, None).unwrap()),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn prepared_vst2_event_capacity_is_explicit_and_recovers_only_after_reset() {
        let mut plugin = fixture("OpenCompose Tone Fixture");
        assert_eq!(
            crate::test_alloc::operations(|| {
                for _ in 0..MAX_PENDING_MIDI {
                    plugin.note_on(60, 1.0, 0);
                }
                plugin.note_on(64, 1.0, 0);
            }),
            0
        );
        let mut output = [9.0; 256];
        assert_eq!(
            crate::test_alloc::operations(|| {
                assert_eq!(
                    plugin.process_block_into(&mut output, None),
                    Err(Vst2ProcessError::EventCapacityExceeded)
                );
                plugin.prepare_block_buffers(128).unwrap();
                assert_eq!(
                    plugin.process_block_into(&mut output, None),
                    Err(Vst2ProcessError::EventCapacityExceeded)
                );
                plugin.reset();
                plugin.process_block_into(&mut output, None).unwrap();
            }),
            0
        );
        assert!(output.iter().all(|sample| *sample == 0.0));
    }
}
