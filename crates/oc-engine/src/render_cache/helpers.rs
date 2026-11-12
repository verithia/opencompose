//! Cache keys, shared-group and transport helpers.

use super::*;

pub(super) fn bounded_tail_samples(reported: u32, sample_rate: u32) -> u64 {
    let cap = u64::from(sample_rate) * u64::from(MAX_DECLARED_PLUGIN_TAIL_SECONDS);
    u64::from(reported).min(cap)
}

pub(super) struct InstrumentTailProbes {
    pub vst3: super::vst3_tail::Vst3TailProbe,
    pub vst2: super::vst2_prepare::Vst2TailProbe,
}

pub(super) fn project_instrument_tail_samples(
    project: &Project,
    authored_presets: &[PresetDef],
    plugin_instances: &mut HashMap<String, ClapInstance>,
    sample_rate: u32,
    resource_root: Option<&Path>,
    probes: InstrumentTailProbes,
) -> Result<u64, String> {
    let mut maximum = 0_u64;
    let mut seen = std::collections::HashSet::new();
    for track in &project.tracks {
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
        if !vst3 && preset.source.as_deref() != Some("vst2") && !seen.insert(preset_name) {
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
            Some("builtin") if preset.plugin.as_deref() == Some(oc_synth::drumkit::PLUGIN_ID) => {
                oc_synth::drumkit::params_tail_seconds(&preset.params, resource_root)
                    .map(|seconds| {
                        (seconds * f64::from(sample_rate))
                            .ceil()
                            .min(f64::from(u32::MAX)) as u32
                    })
                    .unwrap_or(0)
            }
            Some("builtin") if preset.plugin.as_deref() == Some(oc_synth::sampler::PLUGIN_ID) => {
                oc_synth::sampler::params_tail_seconds(&preset.params, resource_root)
                    .map(|seconds| {
                        (seconds * f64::from(sample_rate))
                            .ceil()
                            .min(f64::from(u32::MAX)) as u32
                    })
                    .unwrap_or(0)
            }
            _ => 0,
        };
        maximum = maximum.max(bounded_tail_samples(reported, sample_rate));
    }
    Ok(maximum)
}

pub(crate) fn project_transport(
    project: &Project,
    sample_pos: u64,
    sample_rate: u32,
) -> PluginTransport {
    let transport = PluginTransport::new(sample_pos, sample_rate, project.properties.tempo)
        .with_time_signature(
            project.properties.time_signature.numerator as u16,
            project.properties.time_signature.denominator as u16,
        );
    if project.properties.tempo_map.is_empty() && project.properties.time_sig_map.is_empty() {
        transport
    } else {
        transport.with_clock(std::sync::Arc::new(
            oc_dsl::timing::ProjectClock::new(&project.properties)
                .expect("render timing preflight must succeed"),
        ))
    }
}

pub(super) fn group_tracks_by_preset(
    project: &Project,
    track_indices: &[usize],
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for &index in track_indices {
        let Some(preset) = project.tracks[index].preset.as_deref() else {
            continue;
        };
        // A reusable preset is not a shared instrument instance. Only explicit
        // multi-output routing opts tracks into a shared source; otherwise each
        // track must render its own notes, as the streaming source lanes do.
        let explicit_output = crate::source_lanes::shared_output(project, &project.tracks[index]);
        if !explicit_output {
            groups.push(vec![index]);
            continue;
        }
        if let Some(&position) = positions.get(preset) {
            groups[position].push(index);
        } else {
            positions.insert(preset, groups.len());
            groups.push(vec![index]);
        }
    }
    groups
}

fn shares_declared_plugin_output(preset: &PresetDef) -> bool {
    matches!(preset.source.as_deref(), Some("clap" | "vst2" | "vst3"))
        || is_sfizz_sample_preset(preset)
}

/// Expand a track selection so a shared multi-output plugin group is always
/// selected as a unit: one instance renders every output pair in one call.
pub(super) fn expand_shared_output_groups(project: &Project, selected: &mut Vec<usize>) {
    let external: Vec<usize> = project
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            let preset = project
                .presets
                .iter()
                .find(|preset| Some(&preset.name) == track.preset.as_ref())?;
            shares_declared_plugin_output(preset).then_some(index)
        })
        .collect();
    for group in group_tracks_by_preset(project, &external) {
        if !group.iter().any(|index| selected.contains(index)) {
            continue;
        }
        for index in group {
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
    }
    selected.sort_unstable();
}

pub(super) fn propagate_shared_plugin_dirty(project: &Project, track_dirty: &mut [bool]) {
    let external: Vec<_> = project
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            let preset_name = track.preset.as_deref()?;
            let preset = project
                .presets
                .iter()
                .find(|preset| preset.name == preset_name)?;
            (shares_declared_plugin_output(preset) || preset.source.as_deref() == Some("au"))
                .then_some(index)
        })
        .collect();
    for group in group_tracks_by_preset(project, &external) {
        if group.iter().any(|&index| track_dirty[index]) {
            for index in group {
                track_dirty[index] = true;
            }
        }
    }
}

pub(super) fn instrument_profile_for_track(
    project: &Project,
    track_index: usize,
    sample_rate: u32,
) -> InstrumentProfile {
    instrument_profile_for_track_at(project, track_index, sample_rate, None)
}

pub(super) fn instrument_profile_for_track_at(
    project: &Project,
    track_index: usize,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> InstrumentProfile {
    InstrumentProfile::from_track(
        &project.presets,
        &project.tracks[track_index],
        sample_rate,
        resource_root,
    )
}

/// Stereo pair a track reads from its instrument's declared output ports.
///
/// `plugin_output_pair` stays the absolute legacy selection; a declared port
/// selects the port's default pair, or a pair inside it when `pair:` is set.
pub fn resolve_output_pair(
    ports: &[AudioPort],
    profile: &InstrumentProfile,
    format: &str,
    preset: &str,
    track: &str,
) -> Result<usize, String> {
    let total_pairs: usize = ports.iter().map(AudioPort::pair_count).sum();
    let single_pair = ports.len() == 1 && ports[0].channels <= 2;
    let refuse = |what: &str, index: usize| {
        if single_pair {
            format!(
                "{format} preset '{preset}' exposes only the main stereo pair; track '{track}' requests output {what} {}",
                index + 1
            )
        } else {
            format!(
                "{format} preset '{preset}' declares {} output ports ({} stereo pairs); track '{track}' requests output {what} {}",
                ports.len(),
                total_pairs,
                index + 1
            )
        }
    };
    let pair = match profile.output_port() {
        Some(port) => {
            let declared = ports.get(port).ok_or_else(|| refuse("port", port))?;
            let offset = profile.output_port_pair().unwrap_or(0);
            if offset >= declared.pair_count() {
                return Err(format!(
                    "{format} preset '{preset}' port {} has {} stereo pair(s); track '{track}' requests pair {}",
                    port + 1,
                    declared.pair_count(),
                    offset + 1
                ));
            }
            declared.first_pair + offset
        }
        // Without a port, a first-class pair is absolute and takes precedence
        // over the legacy parameter, matching non-main-output admission.
        None => profile
            .output_port_pair()
            .unwrap_or_else(|| profile.output_pair()),
    };
    if pair >= total_pairs {
        return Err(refuse("pair", pair));
    }
    Ok(pair)
}

/// Resolve every track in one shared-instrument group to the pair it reads.
pub(super) fn resolve_group_pairs(
    project: &Project,
    group: &[usize],
    format: &str,
    preset: &str,
    sample_rate: u32,
    ports: &[AudioPort],
    resource_root: Option<&Path>,
) -> Result<Vec<usize>, String> {
    group
        .iter()
        .map(|&index| {
            let profile =
                instrument_profile_for_track_at(project, index, sample_rate, resource_root);
            resolve_output_pair(ports, &profile, format, preset, &project.tracks[index].name)
        })
        .collect()
}

/// Refuse a port/pair request a one-pair host cannot serve, before it loads.
pub(super) fn require_main_pair_only(
    project: &Project,
    group: &[usize],
    format: &str,
    preset: &str,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    resolve_group_pairs(
        project,
        group,
        format,
        preset,
        sample_rate,
        &[AudioPort::STEREO],
        resource_root,
    )
    .map(|_| ())
}

/// Stereo pair a track reads from its instrument's declared output ports.
///
/// Streaming playback plans lanes from this; offline rendering resolves the
/// same way per shared group.
pub fn track_output_pair(
    project: &Project,
    track_index: usize,
    ports: &[AudioPort],
    sample_rate: u32,
) -> Result<usize, String> {
    let profile = instrument_profile_for_track(project, track_index, sample_rate);
    let preset = project.tracks[track_index].preset.as_deref().unwrap_or("");
    resolve_output_pair(
        ports,
        &profile,
        "Plugin",
        preset,
        &project.tracks[track_index].name,
    )
}

/// True when a track asks for an output other than its instrument's first pair.
///
/// Streaming playback falls back to the main pair when the instrument cannot
/// serve the request. That fallback only costs the track something when it
/// asked for a later pair; asking for the main pair and playing it is not a
/// routing problem.
pub fn track_requests_non_main_output(
    project: &Project,
    track_index: usize,
    sample_rate: u32,
) -> bool {
    instrument_profile_for_track(project, track_index, sample_rate).requests_non_main_output()
}

/// Stereo output pairs each track requests from its hosted instrument.
pub fn aux_pairs_by_track(project: &Project) -> Vec<Vec<usize>> {
    project
        .tracks
        .iter()
        .map(|track| {
            let mut pairs: Vec<usize> = track
                .mix
                .sends
                .iter()
                .filter_map(|send| send.source.as_ref().map(|source| source.pair()))
                .collect();
            pairs.sort_unstable();
            pairs.dedup();
            pairs
        })
        .collect()
}

/// Auxiliary outputs are host buffers captured from a CLAP or VST3 instrument.
pub fn validate_aux_sends(project: &Project) -> Result<(), String> {
    for track in &project.tracks {
        if !track.mix.sends.iter().any(|send| send.source.is_some()) {
            continue;
        }
        let Some(preset) = track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
        else {
            return Err(format!(
                "track '{}' auxiliary plugin outputs require a hosted CLAP or VST3 preset",
                track.name
            ));
        };
        if !matches!(preset.source.as_deref(), Some("clap") | Some("vst3"))
            && !is_sfizz_sample_preset(preset)
        {
            return Err(format!(
                "track '{}' auxiliary plugin outputs require a CLAP or VST3 instrument",
                track.name
            ));
        }
    }
    Ok(())
}

pub(super) fn native_state_stamp(path: &Path) -> Result<(u64, std::time::SystemTime), String> {
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("OpenSynth state {}: {e}", path.display()))?;
    Ok((
        metadata.len(),
        metadata.modified().map_err(|e| e.to_string())?,
    ))
}

/// Copy `frames` interleaved stereo samples starting at `start_sample`.
/// Samples outside the rendered length are zero, never a re-render.
pub(super) fn slice_stereo_range(buffer: &[f32], start_sample: u64, frames: usize) -> Vec<f32> {
    let mut output = vec![0.0; frames.saturating_mul(2)];
    let start = usize::try_from(start_sample).unwrap_or(usize::MAX);
    let available = buffer.len() / 2;
    if start < available {
        let take = frames.min(available - start);
        output[..take * 2].copy_from_slice(&buffer[start * 2..(start + take) * 2]);
    }
    output
}

/// Content hash of one track's audio clips, including the frames the clock
/// derives, the media stamp of each clip's file, and the preset that plays each
/// of its musical containers.
///
/// A pattern's preset overrides the hosting track's preset, so it belongs to
/// the track's cache key: a changed pattern preset must not reuse the audio
/// rendered with the old instrument.
pub(super) fn track_clip_fingerprint(
    project: &Project,
    track: &oc_dsl::ast::Track,
    sample_rate: u32,
    root: Option<&std::path::Path>,
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    // Source content and inserts can change without changing lane count or a
    // preset name. Keep mixer-only effects and faders outside this identity.
    hash_source_value(
        &mut hash,
        &(
            &track.instrument,
            &track.engine,
            &track.engine_params,
            &track.phrases,
            &track.patterns,
            &track.inline_patterns,
            &track.performance,
            track.channel,
            track.output_port,
            track.output_pair,
        ),
    );
    hash_source_preset(&mut hash, project, track.preset.as_deref());
    hash_preset(&mut hash, track.preset.as_deref());
    for clip in track.pattern_clips() {
        let pattern = project.patterns.iter().find(|p| p.name == clip.pattern);
        hash_source_value(&mut hash, clip);
        hash_source_value(&mut hash, &pattern);
        hash_source_preset(
            &mut hash,
            project,
            pattern
                .and_then(|p| p.preset.as_deref())
                .or(track.preset.as_deref()),
        );
        hash_preset(
            &mut hash,
            pattern
                .and_then(|p| p.preset.as_deref())
                .or(track.preset.as_deref()),
        );
        // Slip and loop move the audible content without moving the clip.
        hash_u64(&mut hash, clip.offset_quarters.to_bits());
        hash_u64(&mut hash, clip.loop_quarters.to_bits());
    }
    for inline in &track.inline_patterns {
        hash_source_preset(
            &mut hash,
            project,
            inline.preset.as_deref().or(track.preset.as_deref()),
        );
        hash_preset(
            &mut hash,
            inline.preset.as_deref().or(track.preset.as_deref()),
        );
    }
    let quality = crate::resample::render_quality(project);
    for clip in project
        .audio_clips()
        .iter()
        .filter(|clip| clip.track == track.name)
    {
        hash_audio_clip(&mut hash, clip, root);
        // A different rung is different audio for this clip.
        hash_u64(&mut hash, u64::from(quality.step()));
        if let Some(tune) = crate::vocal_tune::tune_for_clip(project, clip) {
            hash_bytes(
                &mut hash,
                crate::vocal_tune::decision_stamp(tune).as_bytes(),
            );
            // A reference take is part of this clip's sound: if its media,
            // range or stretch changes, this clip has to re-bake.
            if let Some(reference) = crate::vocal_tune::reference_clip(project, tune) {
                hash_audio_clip(&mut hash, reference, root);
            }
        }
        // A harmony voice is part of this clip's audio, not a separate lane.
        if let Some(harmony) = crate::vocal_harmony::harmony_for_clip(project, clip) {
            hash_bytes(
                &mut hash,
                crate::vocal_harmony::voice_stamp(harmony).as_bytes(),
            );
        }
        let placement = crate::audio_clip::range_frames(project, clip, sample_rate);
        let (start, length) = placement.unwrap_or((u64::MAX, u64::MAX));
        hash_u64(&mut hash, start);
        hash_u64(&mut hash, length);
    }
    hash
}

fn hash_source_value(hash: &mut u64, value: &impl serde::Serialize) {
    // These AST types contain string-keyed objects and serializable scalars.
    // A serialization error is a distinct identity rather than a reused value.
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            hash_u64(hash, bytes.len() as u64);
            hash_bytes(hash, &bytes);
        }
        Err(error) => hash_bytes(hash, error.to_string().as_bytes()),
    }
}

fn hash_source_preset(hash: &mut u64, project: &Project, name: Option<&str>) {
    hash_source_value(
        hash,
        &name.and_then(|name| project.presets.iter().find(|preset| preset.name == name)),
    );
}

/// One clip's identity: the file it names and that file's content stamp.
///
/// A replaced file at the same path must invalidate the track, and a missing
/// file is an identity of its own so the entry is invalidated instead of
/// serving the audio the file used to hold.
fn hash_audio_clip(hash: &mut u64, clip: &oc_dsl::ast::AudioClip, root: Option<&std::path::Path>) {
    hash_bytes(hash, clip.name.as_bytes());
    hash_bytes(hash, clip.file.as_bytes());
    match crate::audio_clip::media_stamp(&clip.file, root) {
        Some((length, modified)) => {
            hash_u64(hash, 1);
            hash_u64(hash, length);
            hash_time(hash, modified);
        }
        None => hash_u64(hash, 0),
    }
    if clip.muted {
        hash_u64(hash, 1);
    }
    for beat in [clip.time_range.start, clip.time_range.end] {
        hash_u64(hash, u64::from(beat.bar));
        hash_u64(hash, u64::from(beat.beat));
        hash_u64(hash, u64::from(beat.subdivision));
    }
    for value in [
        clip.placement.start_offset_seconds,
        clip.placement.end_offset_seconds,
        clip.offset_seconds,
        clip.gain_db,
        clip.fade_in_seconds,
        clip.fade_out_seconds,
        clip.crossfade_seconds,
        clip.stretch,
        clip.pitch,
        clip.loop_seconds,
    ] {
        hash_u64(hash, value.to_bits());
    }
    hash_u64(hash, clip.stretch_mode as u64);
    hash_u64(hash, u64::from(clip.tempo_follow));
    if clip.bypass_processing {
        hash_bytes(hash, b"bypass_processing");
    }
    if clip.formant != 0.0 || clip.preserve_formants {
        hash_u64(hash, clip.formant.to_bits());
        hash_u64(hash, u64::from(clip.preserve_formants));
    }
    if !clip.warp_markers.is_empty() {
        hash_u64(hash, clip.warp_markers.len() as u64);
        for marker in &clip.warp_markers {
            hash_u64(hash, marker.source_seconds.to_bits());
            hash_u64(hash, marker.target_seconds.to_bits());
        }
    }
    hash_u64(hash, u64::from(clip.invert));
    hash_u64(hash, clip.gain_envelope.len() as u64);
    for point in &clip.gain_envelope {
        hash_u64(hash, point.position.to_bits());
        hash_u64(hash, point.gain_db.to_bits());
    }
}

fn mix_byte(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(0x0100_0000_01b3);
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in (bytes.len() as u64).to_le_bytes() {
        mix_byte(hash, byte);
    }
    for byte in bytes {
        mix_byte(hash, *byte);
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        mix_byte(hash, byte);
    }
}

/// Hash a file timestamp at full resolution, with its sign relative to the epoch.
fn hash_time(hash: &mut u64, time: std::time::SystemTime) {
    let (sign, elapsed) = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => (1, elapsed),
        Err(error) => (0, error.duration()),
    };
    hash_u64(hash, sign);
    hash_u64(hash, elapsed.as_secs());
    hash_u64(hash, u64::from(elapsed.subsec_nanos()));
}

fn hash_preset(hash: &mut u64, preset: Option<&str>) {
    match preset {
        Some(name) => {
            hash_u64(hash, 1);
            hash_bytes(hash, name.as_bytes());
        }
        None => hash_u64(hash, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(block: &str) -> Project {
        oc_dsl::parse(&format!(
            "@project \"Fingerprint\"\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n{block}@track vocal | instrument: audio/clip\n"
        ))
        .unwrap()
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("oc-clip-media-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn fingerprint(project: &Project, root: Option<&std::path::Path>) -> u64 {
        track_clip_fingerprint(project, &project.tracks[0], 48_000, root)
    }

    #[test]
    fn a_replaced_media_file_invalidates_the_clip() {
        let root = temp_root("replace");
        let media = root.join("lead.wav");
        std::fs::write(&media, b"take one!!").unwrap();
        let project = project("");
        let original = fingerprint(&project, Some(&root));
        assert_eq!(
            original,
            fingerprint(&project, Some(&root)),
            "an unchanged media file must keep the identity"
        );

        // A same-length replacement is only visible through the modification
        // time, and an in-place sample swap usually keeps the length.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&media, b"take two!!").unwrap();
        assert_eq!(std::fs::metadata(&media).unwrap().len(), 10);
        let replaced = fingerprint(&project, Some(&root));
        assert_ne!(
            original, replaced,
            "a replaced media file must invalidate the clip"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_media_file_invalidates_and_stays_stable() {
        let root = temp_root("missing");
        let media = root.join("lead.wav");
        std::fs::write(&media, b"take one!!").unwrap();
        let project = project("");
        let present = fingerprint(&project, Some(&root));
        std::fs::remove_file(&media).unwrap();
        let missing = fingerprint(&project, Some(&root));
        assert_ne!(
            present, missing,
            "a deleted media file must invalidate the clip"
        );
        assert_eq!(
            missing,
            fingerprint(&project, Some(&root)),
            "a missing file is an identity of its own, not a changing one"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn moving_the_project_keeps_the_media_identity() {
        let root = temp_root("move");
        std::fs::write(root.join("lead.wav"), b"take one!!").unwrap();
        let project = project("");
        let before = fingerprint(&project, Some(&root));
        let moved = root.with_file_name(format!(
            "{}-moved",
            root.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&root, &moved).unwrap();
        assert_eq!(
            before,
            fingerprint(&project, Some(&moved)),
            "moving the project must not invalidate identical media"
        );
        let _ = std::fs::remove_dir_all(moved);
    }

    #[test]
    fn a_changed_reference_take_invalidates_the_tuned_clip() {
        let reference = |file: &str| {
            oc_dsl::parse(&format!(
                "@project \"Fingerprint\"\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@audio_clip guide [1:1 - 2:1] | track: guide\n  file: \"{file}\"\n@vocal_tune t | clip: lead\n  snap: reference\n  reference: guide\n@track vocal | instrument: audio/clip\n@track guide | instrument: audio/clip\n"
            ))
            .unwrap()
        };
        let first = reference("guide-a.wav");
        let second = reference("guide-b.wav");
        assert_ne!(
            track_clip_fingerprint(&first, &first.tracks[0], 48_000, None),
            track_clip_fingerprint(&second, &second.tracks[0], 48_000, None),
            "a different reference take did not invalidate the take"
        );
    }

    #[test]
    fn tune_decisions_invalidate_the_clip_fingerprint() {
        let without = project("");
        let zero = project("@vocal_tune t | clip: lead\n  amount: 0%\n");
        let tuned =
            project("@vocal_tune t | clip: lead\n  amount: 50%\n  notes:\n    - at=0.2s tgt=A4\n");
        let baseline = track_clip_fingerprint(&without, &without.tracks[0], 48_000, None);
        let zero_print = track_clip_fingerprint(&zero, &zero.tracks[0], 48_000, None);
        let tuned_print = track_clip_fingerprint(&tuned, &tuned.tracks[0], 48_000, None);
        assert_ne!(baseline, zero_print, "adding a block did not invalidate");
        assert_ne!(
            zero_print, tuned_print,
            "editing a decision did not invalidate"
        );
    }
}
