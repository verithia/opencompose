//! CLAP/VST2/VST3/AU plugin track rendering.

use super::*;

/// Preflight before rendering: no lossy backend substitution, shadow notes,
/// or ambiguous ordering across tracks sharing one stateful instrument.
pub(super) fn prepare_performance_tracks(
    project: &Project,
    sample_rate: u32,
    instances: &HashMap<String, ClapInstance>,
) -> Result<Vec<Option<Vec<oc_synth::performance::ScheduledCommand>>>, String> {
    let mut result = vec![None; project.tracks.len()];
    if !project
        .tracks
        .iter()
        .any(|track| track.performance.is_some())
    {
        return Ok(result);
    }
    oc_dsl::validate(project)
        .map_err(|errors| format!("Invalid performance project: {errors:?}"))?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    for (index, track) in project.tracks.iter().enumerate() {
        let Some(timeline) = &track.performance else {
            continue;
        };
        let preset = project
            .presets
            .iter()
            .find(|p| Some(&p.name) == track.preset.as_ref())
            .ok_or("performance playback requires an explicit CLAP preset")?;
        if preset.source.as_deref() == Some("builtin")
            && preset.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)
        {
            let delegated = crate::performance_send::project_plan(project, index, sample_rate)?;
            let commands: Vec<_> = oc_synth::performance::schedule(timeline, &clock, sample_rate)?
                .into_iter()
                .filter(|command| {
                    !matches!(command.message,
                    oc_dsl::performance::ChannelCommand::ControlChange { controller, .. }
                    if delegated.iter().any(|(cc, _, _)| *cc == controller))
                })
                .collect();
            let binding = oc_synth::native_performance::program_binding(
                &crate::preset::builtin_track_params_untrimmed(project, Some(preset), track),
            )?;
            oc_synth::native_performance::validate(&commands, binding)?;
            result[index] = Some(commands);
            continue;
        }
        if matches!(preset.source.as_deref(), Some("vst3" | "vst2")) {
            let candidates: Vec<_> = project
                .tracks
                .iter()
                .enumerate()
                .filter_map(|(i, t)| (t.preset.as_ref() == Some(&preset.name)).then_some(i))
                .collect();
            let shared = group_tracks_by_preset(project, &candidates)
                .iter()
                .any(|group| group.contains(&index) && group.len() > 1);
            if shared {
                return Err(
                    "performance playback cannot share a plugin instance across tracks yet".into(),
                );
            }
            result[index] = Some(oc_synth::performance::schedule(
                timeline,
                &clock,
                sample_rate,
            )?);
            continue;
        }
        if preset.source.as_deref() != Some("clap") {
            return Err("performance playback currently requires a MIDI1 CLAP backend".into());
        }
        if project
            .tracks
            .iter()
            .filter(|t| t.preset.as_ref() == Some(&preset.name))
            .count()
            != 1
        {
            return Err(
                "performance playback cannot share an instrument instance across tracks yet".into(),
            );
        }
        let instance = instances
            .get(&preset.name)
            .ok_or("performance CLAP instance is missing")?;
        if !instance.supports_midi1_input() {
            return Err("performance CLAP input port0 does not support MIDI1".into());
        }
        result[index] = Some(oc_synth::performance::schedule(
            timeline,
            &clock,
            sample_rate,
        )?);
    }
    Ok(result)
}

/// Give a plugin's background sample loader real wall-clock time before the
/// project starts. Audio is not processed: that would advance free-running
/// oscillator/LFO phase and make a deterministic plugin differ from a
/// no-pre-roll render. The hosted plugin loads on a background thread, so
/// sleeping works.
pub(super) fn pre_roll_plugin(seconds: f64) {
    if seconds > 0.0 {
        if !oc_host::clap_main::offline_main_is_active() {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
        while std::time::Instant::now() < deadline {
            if let Err(error) = oc_host::clap_main::service_offline_main() {
                log::warn!("CLAP pre-roll main service: {error}");
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(5)),
            );
        }
    }
}

/// Deduplicated output pairs for one process call, plus each track's and aux
/// group's slot into that list. Track pairs come first, so a track that reads a
/// pair an earlier track already asked for reuses the same capture.
pub(super) fn plan_output_pairs(
    pairs: &[usize],
    aux_pairs: &[&[usize]],
) -> (Vec<usize>, Vec<usize>, Vec<Vec<usize>>) {
    let mut requested: Vec<usize> = Vec::with_capacity(pairs.len());
    let mut track_slots = Vec::with_capacity(pairs.len());
    for &pair in pairs {
        let slot = requested
            .iter()
            .position(|requested| *requested == pair)
            .unwrap_or_else(|| {
                requested.push(pair);
                requested.len() - 1
            });
        track_slots.push(slot);
    }
    let mut aux_slots = Vec::with_capacity(aux_pairs.len());
    for pairs in aux_pairs {
        let mut slots = Vec::with_capacity(pairs.len());
        for &pair in *pairs {
            let slot = requested
                .iter()
                .position(|requested| *requested == pair)
                .unwrap_or_else(|| {
                    requested.push(pair);
                    requested.len() - 1
                });
            slots.push(slot);
        }
        aux_slots.push(slots);
    }
    (requested, track_slots, aux_slots)
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_tracks_via_clap(
    instance: &mut ClapInstance,
    track_indices: &[usize],
    event_groups: &[&[&MidiEvent]],
    total_samples: usize,
    _sample_rate: u32,
    transport: PluginTransport,
    pairs: &[usize],
    aux_pairs: &[&[usize]],
    performance: Option<&[oc_synth::performance::ScheduledCommand]>,
    pre_roll_seconds: f64,
) -> Result<(Vec<Vec<f32>>, AuxOutputBuffers), String> {
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    pre_roll_plugin(pre_roll_seconds);
    let block_size: u32 = 128;
    if let Some(pair) = aux_pairs
        .iter()
        .flat_map(|pairs| pairs.iter())
        .find(|pair| **pair >= instance.output_count())
    {
        return Err(format!(
            "CLAP instrument declares {} stereo outputs; auxiliary output {} was requested",
            instance.output_count(),
            pair + 1
        ));
    }
    let mut buffers: Vec<Vec<f32>> = pairs
        .iter()
        .map(|_| Vec::with_capacity(total_samples * 2))
        .collect();
    let mut events: Vec<_> = event_groups
        .iter()
        .enumerate()
        .flat_map(|(group, events)| events.iter().copied().map(move |event| (event, group)))
        .collect();
    events.sort_by_key(|(event, _)| event.time_samples);

    // One process call fans the track pair and every requested aux pair, so a
    // shared instance renders each pair once per block.
    let (requested_pairs, track_slots, aux_slots) = plan_output_pairs(pairs, aux_pairs);
    let mut aux_buffers: AuxOutputBuffers = HashMap::new();
    for (group, pairs) in aux_pairs.iter().enumerate() {
        for &pair in *pairs {
            aux_buffers
                .entry((track_indices[group], pair))
                .or_insert_with(|| Vec::with_capacity(total_samples * 2));
        }
    }

    let mut event_idx = 0;
    let mut performance_idx = 0;
    let mut sample_pos: u64 = 0;
    let total = total_samples as u64;

    // Flush any pending param changes from GUI
    if instance.needs_flush() {
        instance.flush_params();
    }

    while sample_pos < total {
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        // Queue all events that fall inside this processing block, preserving
        // their sample offset within the block.
        let remaining = total - sample_pos;
        let block_transport = transport.at_sample(transport.sample_pos + sample_pos);
        let frames =
            block_transport.block_frames((block_size as u64).min(remaining) as usize) as u32;
        let block_end = sample_pos + frames as u64;

        if let Some(commands) = performance {
            while performance_idx < commands.len() && commands[performance_idx].sample < block_end {
                let command = &commands[performance_idx];
                let offset = command
                    .sample
                    .checked_sub(sample_pos)
                    .ok_or("late performance event")?;
                instance.queue_performance_command(
                    command,
                    u32::try_from(offset).map_err(|_| "performance offset overflow")?,
                    frames,
                )?;
                performance_idx += 1;
            }
        }

        while event_idx < events.len() && events[event_idx].0.time_samples < block_end {
            let (ev, _) = events[event_idx];
            let sample_offset = ev.time_samples.saturating_sub(sample_pos) as i32;
            queue_plugin_midi_event(instance, &ev.event, sample_offset)?;
            event_idx += 1;
        }

        if frames == 0 {
            break;
        }

        let outputs = instance.process_block_with_transport_and_output_pairs(
            frames,
            Some(block_transport),
            &requested_pairs,
        );
        for (buffer, &slot) in buffers.iter_mut().zip(&track_slots) {
            buffer.extend_from_slice(&outputs[slot]);
        }
        for (group, slots) in aux_slots.iter().enumerate() {
            for (&pair, &slot) in aux_pairs[group].iter().zip(slots) {
                if let Some(buffer) = aux_buffers.get_mut(&(track_indices[group], pair)) {
                    buffer.extend_from_slice(&outputs[slot]);
                }
            }
        }
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        sample_pos += frames as u64;
    }

    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    let expected_len = total_samples * 2;
    for buffer in &mut buffers {
        buffer.resize(expected_len, 0.0);
    }
    for buffer in aux_buffers.values_mut() {
        buffer.resize(expected_len, 0.0);
    }
    if performance.is_some_and(|commands| performance_idx != commands.len()) {
        return Err("performance events exceeded the render horizon".into());
    }
    instance.end_processing();
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    Ok((buffers, aux_buffers))
}

/// Render tracks that share one AudioUnit instrument.
///
/// AU multi-bus output is not wired, so every track reads the main stereo pair
/// and an auxiliary pair request is an error instead of silent audio.
#[cfg(target_os = "macos")]
pub(super) fn render_tracks_via_au(
    instance: &mut AuInstance,
    track_indices: &[usize],
    event_groups: &[&[&MidiEvent]],
    total_samples: usize,
    transport: PluginTransport,
    pre_roll_seconds: f64,
) -> Result<Vec<Vec<f32>>, String> {
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    pre_roll_plugin(pre_roll_seconds);
    let block_size: u32 = 128;
    let mut buffers: Vec<Vec<f32>> = track_indices
        .iter()
        .map(|_| Vec::with_capacity(total_samples * 2))
        .collect();
    let mut events: Vec<&MidiEvent> = event_groups
        .iter()
        .flat_map(|events| events.iter().copied())
        .collect();
    events.sort_by_key(|event| event.time_samples);

    let mut output = vec![0.0; block_size as usize * 2];
    let mut event_idx = 0;
    let mut sample_pos: u64 = 0;
    let total = total_samples as u64;
    while sample_pos < total {
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        let remaining = total - sample_pos;
        let block_transport = transport.at_sample(transport.sample_pos + sample_pos);
        let frames =
            block_transport.block_frames((block_size as u64).min(remaining) as usize) as u32;
        let block_end = sample_pos + frames as u64;
        while event_idx < events.len() && events[event_idx].time_samples < block_end {
            let sample_offset = events[event_idx].time_samples.saturating_sub(sample_pos) as i32;
            queue_plugin_midi_event(instance, &events[event_idx].event, sample_offset)?;
            event_idx += 1;
        }
        if frames == 0 {
            break;
        }
        let output = &mut output[..frames as usize * 2];
        instance
            .process_block_into(output, Some(&block_transport))
            .map_err(|error| error.to_string())?;
        for buffer in &mut buffers {
            buffer.extend_from_slice(output);
        }
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        sample_pos += frames as u64;
    }

    for buffer in &mut buffers {
        buffer.resize(total_samples * 2, 0.0);
    }
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    Ok(buffers)
}

/// Render every track that reads one LV2 instrument's stereo output.
///
/// LV2 audio ports are limited to stereo by the host, so an auxiliary output
/// request is rejected instead of silently reading the main pair.
pub(super) fn render_tracks_via_lv2(
    instance: &mut oc_host::lv2::Lv2Instance,
    track_indices: &[usize],
    event_groups: &[&[&MidiEvent]],
    total_samples: usize,
    transport: PluginTransport,
    pre_roll_seconds: f64,
) -> Result<Vec<Vec<f32>>, String> {
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    pre_roll_plugin(pre_roll_seconds);
    let block_size: u32 = 128;
    let mut buffers: Vec<Vec<f32>> = track_indices
        .iter()
        .map(|_| Vec::with_capacity(total_samples * 2))
        .collect();
    let mut events: Vec<&MidiEvent> = event_groups
        .iter()
        .flat_map(|events| events.iter().copied())
        .collect();
    events.sort_by_key(|event| event.time_samples);

    let mut event_idx = 0;
    let mut sample_pos: u64 = 0;
    let total = total_samples as u64;
    while sample_pos < total {
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        let remaining = total - sample_pos;
        let block_transport = transport.at_sample(transport.sample_pos + sample_pos);
        let frames =
            block_transport.block_frames((block_size as u64).min(remaining) as usize) as u32;
        let block_end = sample_pos + frames as u64;
        while event_idx < events.len() && events[event_idx].time_samples < block_end {
            let sample_offset = events[event_idx].time_samples.saturating_sub(sample_pos) as i32;
            queue_plugin_midi_event(instance, &events[event_idx].event, sample_offset)?;
            event_idx += 1;
        }
        if frames == 0 {
            break;
        }
        let output = instance
            .process_block_with_transport(frames, Some(block_transport))
            .to_vec();
        for buffer in &mut buffers {
            buffer.extend_from_slice(&output);
        }
        crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
        sample_pos += frames as u64;
    }

    for buffer in &mut buffers {
        buffer.resize(total_samples * 2, 0.0);
    }
    crate::processing_failure::effect(instance, None).map_err(|error| error.to_string())?;
    Ok(buffers)
}

pub(crate) fn queue_vst2_performance_command(
    instance: &mut Vst2Instance,
    command: &oc_synth::performance::ScheduledCommand,
    sample_offset: i32,
) -> Result<(), String> {
    let bytes = command.midi_bytes();
    instance.queue_midi(
        bytes[0],
        bytes[1],
        bytes.get(2).copied().unwrap_or(0),
        sample_offset,
    );
    Ok(())
}

pub(crate) fn queue_vst3_performance_command(
    instance: &mut Vst3Instance,
    command: &oc_synth::performance::ScheduledCommand,
    sample_offset: i32,
) -> Result<(), String> {
    use oc_dsl::performance::ChannelCommand;
    let channel = command.channel;
    match command.message {
        ChannelCommand::NoteOn { note, velocity } => {
            instance.note_on_channel(channel, note, f32::from(velocity) / 127.0, sample_offset);
        }
        ChannelCommand::NoteOff { note, velocity } => {
            if !instance.note_off_channel_velocity(channel, note, velocity, sample_offset) {
                return Err("invalid VST3 note release".into());
            }
        }
        ChannelCommand::ControlChange { controller, value } => {
            if !instance.queue_mapped_midi(
                channel,
                i16::from(controller),
                i32::from(value),
                sample_offset,
            ) {
                return Err(format!("VST3 does not map MIDI CC {controller}"));
            }
        }
        ChannelCommand::ProgramChange { program } => {
            instance.program_change(channel, program, sample_offset);
        }
        ChannelCommand::PitchBend { value } => {
            instance.pitch_bend(channel, value, sample_offset);
        }
        ChannelCommand::ChannelPressure { pressure } => {
            instance.channel_pressure(channel, pressure, sample_offset);
        }
        ChannelCommand::PolyPressure { note, pressure } => {
            instance.poly_pressure(channel, note, pressure, sample_offset);
        }
    }
    Ok(())
}

/// One shared VST3 instance and the tracks that read its output pairs.
pub(super) struct Vst3RenderGroup<'a> {
    pub(super) track_indices: &'a [usize],
    pub(super) event_groups: &'a [&'a [&'a MidiEvent]],
    pub(super) profiles: &'a [InstrumentProfile],
    /// Resolved stereo pair each track reads, one per track.
    pub(super) pairs: &'a [usize],
    pub(super) aux_pairs: &'a [&'a [usize]],
}

#[cfg(test)]
mod tests {
    use super::plan_output_pairs;

    #[test]
    fn performance_preset_reuse_only_refuses_actual_shared_source_groups() {
        let mut project=oc_dsl::parse("@project\n@preset voice\n  source: vst3\n  plugin: \"controlled.component\"\n@track first | preset: voice, instrument: synth/lead\n@track second | preset: voice, instrument: synth/lead\n").unwrap();
        for track in &mut project.tracks {
            track.performance = Some(oc_dsl::performance::PerformanceTimeline {
                events: vec![
                    oc_dsl::performance::PerformanceEvent {
                        quarter: 0.0,
                        channel: 0,
                        message: oc_dsl::performance::ChannelCommand::NoteOn {
                            note: 60,
                            velocity: 100,
                        },
                    },
                    oc_dsl::performance::PerformanceEvent {
                        quarter: 1.0,
                        channel: 0,
                        message: oc_dsl::performance::ChannelCommand::NoteOff {
                            note: 60,
                            velocity: 64,
                        },
                    },
                ],
            });
        }
        let prepared =
            super::prepare_performance_tracks(&project, 48_000, &std::collections::HashMap::new())
                .unwrap();
        assert_eq!(prepared.len(), 2);
        assert!(prepared.iter().all(|commands| {
            commands
                .as_ref()
                .is_some_and(|commands| commands.len() == 2)
        }));
        project.tracks[0].output_port = Some(0);
        project.tracks[1].output_port = Some(1);
        let error =
            super::prepare_performance_tracks(&project, 48_000, &std::collections::HashMap::new())
                .unwrap_err();
        assert!(error.contains("cannot share a plugin instance"), "{error}");
    }
    #[test]
    fn output_pair_plan_keeps_the_track_pairs_first_and_dedups_aux() {
        let pairs = [0, 2, 2];
        let aux: [&[usize]; 2] = [&[1, 2], &[1, 3]];
        let (requested, track_slots, aux_slots) = plan_output_pairs(&pairs, &aux);
        assert_eq!(requested, vec![0, 2, 1, 3]);
        assert_eq!(track_slots, vec![0, 1, 1]);
        assert_eq!(aux_slots, vec![vec![2, 1], vec![2, 3]]);

        // A main-only track requests exactly its main pair.
        let (requested, track_slots, aux_slots) = plan_output_pairs(&pairs[..1], &[&[]]);
        assert_eq!(requested, vec![0]);
        assert_eq!(track_slots, vec![0]);
        assert!(aux_slots[0].is_empty());
    }
}
