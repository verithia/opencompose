//! Built-in synth and sampler track rendering.

use super::*;

/// Validate referenced built-ins before reusing a cached source buffer.
/// Hosted built-ins use the host's factory as their authoritative registry.
pub(super) fn validate_builtin_plugin_id(plugin_id: &str) -> Result<(), String> {
    if matches!(
        plugin_id,
        oc_synth::synth::PLUGIN_ID
            | oc_synth::sampler::PLUGIN_ID
            | oc_synth::drumkit::PLUGIN_ID
            | oc_synth::audio_region::PLUGIN_ID
    ) || oc_host::builtin::create_builtin(plugin_id).is_some()
    {
        Ok(())
    } else {
        Err(format!("Unknown built-in instrument plugin '{plugin_id}'"))
    }
}

/// Render a track through a builtin plugin (test tone, etc.).
/// Owned data for one built-in track render job.
#[derive(Clone)]
pub(super) struct BuiltinTrackJob {
    pub(super) index: usize,
    pub(super) plugin_id: String,
    pub(super) preset_name: String,
    pub(super) params: Vec<(String, ParamValue)>,
    pub(super) events: Vec<MidiEvent>,
    pub(super) performance: Option<Vec<oc_synth::performance::ScheduledCommand>>,
}

/// Slot for one built-in track's rendered buffer and latency.
type BuiltinRender = Mutex<Option<Result<(Vec<f32>, u32), String>>>;

/// Scheduling only: voice-seconds times unison width, which tracks the
/// per-sample voice work. Never affects the rendered audio.
fn job_cost(job: &BuiltinTrackJob) -> f64 {
    if job.performance.is_some() {
        return f64::MAX;
    }
    let mut sounding: Vec<((u8, u8), u64)> = Vec::new();
    let mut voice_samples = 0_u64;
    for event in &job.events {
        match &event.event {
            MidiMessage::NoteOn { channel, note, .. } => {
                sounding.push(((*channel, *note), event.time_samples));
            }
            MidiMessage::NoteOff { channel, note } => {
                let key = (*channel, *note);
                if let Some(position) = sounding.iter().rposition(|(open, _)| *open == key) {
                    let (_, started) = sounding.remove(position);
                    voice_samples =
                        voice_samples.saturating_add(event.time_samples.saturating_sub(started));
                }
            }
            _ => {}
        }
    }
    let unison = job
        .params
        .iter()
        .find(|(name, _)| name == "unison.voices")
        .and_then(|(_, value)| param_value_as_f64(value))
        .unwrap_or(1.0)
        .max(1.0);
    voice_samples as f64 * unison
}

pub(super) struct BuiltinRenderContext<'a> {
    pub total_samples: usize,
    pub start_sample: u64,
    pub sample_rate: u32,
    pub tempo: f64,
    pub resource_root: Option<&'a Path>,
    pub clock: &'a oc_dsl::timing::ProjectClock,
    pub cancel: Option<&'a crate::render_cancel::CancelToken>,
    pub worker_options: crate::render_parallel::RenderWorkerOptions,
}

/// Render independent built-in tracks in parallel, preserving track order.
/// Each track also reports the algorithmic latency of its instrument rack so
/// the mixer can delay-compensate the other tracks.
pub(super) fn render_builtin_tracks_parallel(
    jobs: Vec<BuiltinTrackJob>,
    context: BuiltinRenderContext<'_>,
) -> Result<Vec<(usize, Vec<f32>, u32)>, String> {
    let BuiltinRenderContext {
        total_samples,
        start_sample,
        sample_rate,
        tempo,
        resource_root,
        clock,
        cancel,
        worker_options,
    } = context;
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    let workers = worker_options.workers().min(jobs.len()).max(1);
    let next = AtomicUsize::new(0);
    let results: Vec<BuiltinRender> = (0..jobs.len()).map(|_| Mutex::new(None)).collect();
    // Start the most expensive tracks first so the last wave is not one heavy
    // track; the queue is still shared, so workers stay busy.
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    let costs: Vec<f64> = jobs.iter().map(job_cost).collect();
    order.sort_by(|a, b| {
        costs[*b]
            .partial_cmp(&costs[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if crate::render_cancel::stopped(cancel) {
                        break;
                    }
                    let slot = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&index) = order.get(slot) else {
                        break;
                    };
                    let job = &jobs[index];
                    let started = std::time::Instant::now();
                    let rendered = (|| -> Result<(Vec<f32>, u32), String> {
                        let events: Vec<&MidiEvent> = job.events.iter().collect();
                        if let Some(commands) = &job.performance {
                            if job.plugin_id != oc_synth::synth::PLUGIN_ID {
                                return Err(format!("Built-in instrument '{}' cannot render native performance; expected builtin.open_synth", job.plugin_id));
                            }
                            let binding =
                                oc_synth::native_performance::program_binding(&job.params)?;
                            let patch =
                                oc_synth::synth::load_project_patch(&job.params, resource_root)?;
                            let latency = patch.fx_latency_samples(sample_rate) as u32;
                            let buffer = oc_synth::native_performance::render(
                                commands,
                                total_samples,
                                sample_rate,
                                patch,
                                binding,
                                clock,
                            )?;
                            Ok((buffer, latency))
                        } else {
                            render_builtin_project_with_latency(
                                &job.plugin_id,
                                Some(&job.params),
                                &events,
                                total_samples,
                                start_sample,
                                sample_rate,
                                tempo,
                                resource_root,
                                Some(clock),
                                cancel,
                            )
                        }
                    })().map_err(|error| format!("Built-in plugin '{}' for preset '{}' on render lane {}: {error}", job.plugin_id, job.preset_name, job.index));
                    crate::stage_log::push_worker_track("builtin track", index, started);
                    *results[index].lock().unwrap() = Some(rendered);
                }
            });
        }
    });

    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }

    let mut output = Vec::with_capacity(jobs.len());
    for (job, slot) in jobs.iter().zip(results) {
        let (buffer, latency) = slot
            .into_inner()
            .unwrap()
            .ok_or_else(|| format!("builtin track {} was not rendered", job.index))??;
        output.push((job.index, buffer, latency));
    }
    Ok(output)
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_builtin_project(
    plugin_id: &str,
    params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&MidiEvent],
    total_samples: usize,
    start_sample: u64,
    sample_rate: u32,
    tempo: f64,
    resource_root: Option<&Path>,
    clock: Option<&oc_dsl::timing::ProjectClock>,
    cancel: Option<&crate::render_cancel::CancelToken>,
) -> Result<Vec<f32>, String> {
    render_builtin_project_with_latency(
        plugin_id,
        params,
        events,
        total_samples,
        start_sample,
        sample_rate,
        tempo,
        resource_root,
        clock,
        cancel,
    )
    .map(|(buffer, _)| buffer)
}

/// Render one built-in track and report its instrument-rack latency.
// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_builtin_project_with_latency(
    plugin_id: &str,
    params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&MidiEvent],
    total_samples: usize,
    start_sample: u64,
    sample_rate: u32,
    tempo: f64,
    resource_root: Option<&Path>,
    clock: Option<&oc_dsl::timing::ProjectClock>,
    cancel: Option<&crate::render_cancel::CancelToken>,
) -> Result<(Vec<f32>, u32), String> {
    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    if plugin_id == oc_synth::audio_region::PLUGIN_ID {
        let (document, path) = oc_synth::audio_region::load_document(
            params.map(Vec::as_slice).unwrap_or(&[]),
            resource_root,
        )?;
        let buffer = document.render_window(&path, start_sample, total_samples, sample_rate)?;
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        return Ok((buffer, 0));
    }
    if plugin_id == oc_synth::sampler::PLUGIN_ID {
        let buffer = oc_synth::sampler::try_render_track(
            params.map(Vec::as_slice).unwrap_or(&[]),
            events,
            total_samples,
            sample_rate,
            resource_root,
        )?;
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        return Ok((buffer, 0));
    }
    if plugin_id == oc_synth::drumkit::PLUGIN_ID {
        let (buffer, warnings) = oc_synth::drumkit::try_render_track_with_warnings(
            params.map(Vec::as_slice).unwrap_or(&[]),
            events,
            total_samples,
            sample_rate,
            resource_root,
        )?;
        for warning in warnings {
            crate::render_warnings::push(warning);
        }
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        return Ok((buffer, 0));
    }
    if plugin_id == oc_synth::synth::PLUGIN_ID {
        let patch = oc_synth::synth::load_project_patch(
            params.map(Vec::as_slice).unwrap_or(&[]),
            resource_root,
        )?;
        let latency = patch.fx_latency_samples(sample_rate) as u32;
        let buffer = oc_synth::synth::try_render_track_with_patch_and_clock_cancellable(
            events,
            total_samples,
            sample_rate,
            patch,
            tempo,
            clock,
            cancel.map(crate::render_cancel::CancelToken::atomic),
        )?;
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        return Ok((buffer, latency));
    }
    if events
        .iter()
        .any(|event| matches!(event.event, MidiMessage::NativeVoice(_)))
    {
        return Err("native voice events require builtin.open_synth".into());
    }
    Ok((
        try_render_track_via_builtin(
            plugin_id,
            params,
            events,
            total_samples,
            sample_rate,
            tempo,
            cancel,
        )?,
        0,
    ))
}

/// Render a track through a builtin plugin (test tone, etc.).
/// Builtins are continuous generators — they produce audio when `process()` is called,
/// gated by MIDI note-on/note-off events.
pub(super) fn render_track_via_builtin(
    plugin_id: &str,
    preset_params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    tempo: f64,
    cancel: Option<&crate::render_cancel::CancelToken>,
) -> Result<Vec<f32>, String> {
    try_render_track_via_builtin(
        plugin_id,
        preset_params,
        events,
        total_samples,
        sample_rate,
        tempo,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn try_render_track_via_builtin(
    plugin_id: &str,
    preset_params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    tempo: f64,
    cancel: Option<&crate::render_cancel::CancelToken>,
) -> Result<Vec<f32>, String> {
    if crate::render_cancel::stopped(cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    if plugin_id == oc_synth::sampler::PLUGIN_ID {
        return oc_synth::sampler::try_render_track(
            preset_params.map(Vec::as_slice).unwrap_or(&[]),
            events,
            total_samples,
            sample_rate,
            None,
        );
    }
    if plugin_id == oc_synth::drumkit::PLUGIN_ID {
        let (buffer, warnings) = oc_synth::drumkit::try_render_track_with_warnings(
            preset_params.map(Vec::as_slice).unwrap_or(&[]),
            events,
            total_samples,
            sample_rate,
            None,
        )?;
        for warning in warnings {
            crate::render_warnings::push(warning);
        }
        return Ok(buffer);
    }
    if plugin_id == oc_synth::synth::PLUGIN_ID {
        return Ok(oc_synth::synth::render_track_with_tempo(
            events,
            total_samples,
            sample_rate,
            preset_params,
            tempo,
        ));
    }
    use oc_host::builtin::create_builtin;

    let mut plugin = match create_builtin(plugin_id) {
        Some(p) => p,
        None => return Err(format!("Unknown built-in instrument plugin '{plugin_id}'")),
    };

    // Apply preset params
    if let Some(params) = preset_params {
        for (name, value) in params {
            let v = match param_value_as_f64(value) {
                Some(value) => value,
                None => continue,
            };
            plugin.set_param(name, v);
        }
    }

    let mut buffer = vec![0.0f32; total_samples * 2];
    let mut active = false;
    let mut event_idx = 0;
    let chunk_size = 256;
    let mut pos = 0usize;

    while pos < total_samples {
        // A block is 256 frames, so a cancelled render stops in a few
        // milliseconds at 48 kHz.
        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        // Process events at this position
        while event_idx < events.len() && (events[event_idx].time_samples as usize) <= pos {
            match &events[event_idx].event {
                MidiMessage::NoteOn { .. } => active = true,
                MidiMessage::NoteOff { .. } => active = false,
                _ => {}
            }
            event_idx += 1;
        }

        // Do not quantize note boundaries to the 256-frame processing block.
        let next_event = events
            .get(event_idx)
            .map(|event| event.time_samples as usize)
            .unwrap_or(total_samples);
        let end = (pos + chunk_size).min(total_samples).min(next_event);
        let stereo_start = pos * 2;
        let stereo_end = end * 2;

        if active {
            plugin.process(&mut buffer[stereo_start..stereo_end], sample_rate);
        }

        pos = end;
    }

    Ok(buffer)
}

/// Render a track through a built-in plugin.
pub fn render_track_via_builtin_pub(
    plugin_id: &str,
    preset_params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&crate::interpret::MidiEvent],
    total_samples: usize,
    sample_rate: u32,
) -> Result<Vec<f32>, String> {
    render_track_via_builtin(
        plugin_id,
        preset_params,
        events,
        total_samples,
        sample_rate,
        120.0,
        None,
    )
}

/// Project-aware entry point. The legacy wrapper uses 120 BPM for compatibility.
pub fn render_track_via_builtin_with_tempo(
    plugin_id: &str,
    preset_params: Option<&Vec<(String, oc_dsl::ast::ParamValue)>>,
    events: &[&crate::interpret::MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    tempo: f64,
) -> Result<Vec<f32>, String> {
    render_track_via_builtin(
        plugin_id,
        preset_params,
        events,
        total_samples,
        sample_rate,
        tempo,
        None,
    )
}
