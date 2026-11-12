//! Semantic validation for OpenCompose ASTs.

use crate::ast::*;
use crate::diagnostics::ValidationDiagnostic;
use crate::error::ValidationError;
use crate::source_map::{ReferenceKind, SourceOwner, SourcePart, SourceSubject};
mod diagnostics;
use diagnostics::Emissions;
use std::collections::{HashMap, HashSet};

/// Validate a project, returning every error.
pub fn validate(project: &Project) -> Result<(), Vec<ValidationError>> {
    let errors = validate_inner(project, &mut Emissions::default());
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(crate) fn validate_diagnostics(project: &Project) -> Vec<ValidationDiagnostic> {
    let mut emissions = Emissions::enabled();
    let errors = validate_inner(project, &mut emissions);
    emissions.finish(errors)
}

fn validate_inner(project: &Project, emissions: &mut Emissions) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    validate_project_properties(&project.properties, &mut errors);
    if let Err(error) = crate::timing::ProjectClock::new(&project.properties) {
        errors.push(ValidationError::InvalidValue {
            field: "project.clock".into(),
            value: error,
        });
    }

    let mut preset_names = HashSet::new();
    if let Err(error) = crate::preset::resolve_checked(project) {
        let validation = ValidationError::InvalidValue {
            field: "preset.extends".into(),
            value: error.to_string(),
        };
        if let Some(index) = error.parent_reference_owner() {
            emissions.push(
                &mut errors,
                validation,
                SourceSubject {
                    owner: SourceOwner::Preset(index),
                    part: SourcePart::Reference {
                        kind: ReferenceKind::ParentPreset,
                        occurrence: 0,
                    },
                },
            );
        } else {
            errors.push(validation);
        }
    }
    for (index, preset) in project.presets.iter().enumerate() {
        if !preset_names.insert(preset.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicatePreset {
                    name: preset.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::Preset(index),
                    part: SourcePart::Name,
                },
            );
        }
    }

    let mut track_names = HashSet::new();
    for (index, track) in project.tracks.iter().enumerate() {
        if !track_names.insert(track.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicateTrack {
                    name: track.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::Track(index),
                    part: SourcePart::Name,
                },
            );
        }
    }

    let mut bus_names = HashSet::new();
    for (index, bus) in project.buses.iter().enumerate() {
        if !bus_names.insert(bus.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicateBus {
                    name: bus.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::Bus(index),
                    part: SourcePart::Name,
                },
            );
        }
    }

    for (index, preset) in project.presets.iter().enumerate() {
        validate_preset(
            preset,
            &track_names,
            &bus_names,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Preset(index),
                part: SourcePart::Whole,
            },
        );
    }

    for (index, preset) in project.presets.iter().enumerate() {
        if let Some(parent) = &preset.extends
            && !preset_names.contains(parent.as_str())
        {
            emissions.push(
                &mut errors,
                ValidationError::UnknownPreset {
                    owner: format!("preset '{}'", preset.name),
                    preset: parent.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::Preset(index),
                    part: SourcePart::Reference {
                        kind: ReferenceKind::ParentPreset,
                        occurrence: 0,
                    },
                },
            );
        }
    }

    let mut control_names = HashSet::new();
    for (index, source) in project.properties.control_sources.iter().enumerate() {
        if !control_names.insert(source.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicateControlSource {
                    name: source.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::ControlSource(index),
                    part: SourcePart::Name,
                },
            );
        }
        validate_control_source(
            source,
            SourceOwner::ControlSource(index),
            &track_names,
            &mut errors,
            emissions,
        );
    }
    validate_control_links(
        &project.properties.control_links,
        project,
        &track_names,
        &mut errors,
        emissions,
    );
    for track in &project.tracks {
        let Some(gain) = &track.mix.control_gain else {
            continue;
        };
        if gain.source.trim().is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: format!("track '{}' control", track.name),
                value: "missing control source".into(),
            });
        } else if !control_names.contains(gain.source.as_str()) {
            errors.push(ValidationError::UnknownControlSource {
                track: track.name.clone(),
                control: gain.source.clone(),
            });
        }
        if !gain.depth_db.is_finite() {
            errors.push(ValidationError::InvalidValue {
                field: format!("track '{}' control depth", track.name),
                value: gain.depth_db.to_string(),
            });
        }
    }

    let mut pattern_names = HashSet::new();
    for (index, pattern) in project.patterns.iter().enumerate() {
        if !pattern_names.insert(pattern.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicatePattern {
                    name: pattern.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::Pattern(index),
                    part: SourcePart::Name,
                },
            );
        }
        validate_pattern_def(
            pattern,
            Some(SourceOwner::Pattern(index)),
            &project.properties,
            &preset_names,
            &mut errors,
            emissions,
        );
    }

    let track_context = TrackValidationContext {
        project,
        preset_names: &preset_names,
        pattern_names: &pattern_names,
        track_names: &track_names,
        bus_names: &bus_names,
    };
    for (index, track) in project.tracks.iter().enumerate() {
        validate_track(
            track,
            SourceOwner::Track(index),
            &track_context,
            &mut errors,
            emissions,
        );
    }

    let mut audio_clip_names = HashSet::new();
    let mut audio_clips = HashMap::new();
    for (index, clip) in project.audio_clips().into_iter().enumerate() {
        if !audio_clip_names.insert(clip.name.as_str()) {
            emissions.push(
                &mut errors,
                ValidationError::DuplicateAudioClip {
                    name: clip.name.clone(),
                },
                SourceSubject {
                    owner: SourceOwner::AudioClip(index),
                    part: SourcePart::Name,
                },
            );
        }
        audio_clips.insert(clip.name.as_str(), clip);
        validate_audio_clip(
            clip,
            SourceOwner::AudioClip(index),
            &project.properties,
            &track_names,
            &mut errors,
            emissions,
        );
    }
    validate_vocal_tunes(
        &project.properties,
        &track_names,
        &audio_clips,
        &mut errors,
        emissions,
    );
    validate_vocal_harmonies(
        &project.properties,
        &track_names,
        &audio_clips,
        &mut errors,
        emissions,
    );

    for (bus_index, bus) in project.buses.iter().enumerate() {
        validate_bus(
            bus,
            &track_names,
            &bus_names,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Bus(bus_index),
                part: SourcePart::Whole,
            },
        );
        validate_automation_lanes(
            &bus.automation,
            &bus.effects,
            &project.properties,
            &format!("bus '{}'", bus.name),
            None,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Bus(bus_index),
                part: SourcePart::Whole,
            },
        );
        let gain_start = errors.len();
        validate_gain_automation(
            &bus.gain_automation,
            &project.properties,
            &bus.name,
            &mut errors,
        );
        emissions.attribute(
            gain_start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::Bus(bus_index),
                part: SourcePart::Whole,
            },
        );
    }
    validate_routing_cycles(&project.buses, &mut errors, emissions);
    validate_signal_cycles(project, &track_names, &bus_names, &mut errors, emissions);

    let control_sources: HashMap<&str, &ControlSource> = project
        .properties
        .control_sources
        .iter()
        .map(|source| (source.name.as_str(), source))
        .collect();
    let route_sources = RouteSources {
        track_names: &track_names,
        bus_names: &bus_names,
        control_sources: &control_sources,
    };
    for (index, track) in project.tracks.iter().enumerate() {
        validate_input_routes(
            &format!("track '{}'", track.name),
            Some(track.name.as_str()),
            &track.effects,
            &track.input_routes,
            &route_sources,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Track(index),
                part: SourcePart::Whole,
            },
        );
    }
    for (index, bus) in project.buses.iter().enumerate() {
        validate_input_routes(
            &format!("bus '{}'", bus.name),
            None,
            &bus.effects,
            &bus.input_routes,
            &route_sources,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Bus(index),
                part: SourcePart::Whole,
            },
        );
    }
    validate_control_route_cycles(project, &control_sources, &mut errors, emissions);

    if let Some(master) = &project.master {
        validate_automation_lanes(
            &master.automation,
            &master.effects,
            &project.properties,
            "master",
            None,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Master,
                part: SourcePart::Whole,
            },
        );
        let gain_start = errors.len();
        validate_gain_automation(
            &master.gain_automation,
            &project.properties,
            "master",
            &mut errors,
        );
        emissions.attribute(
            gain_start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::Master,
                part: SourcePart::Whole,
            },
        );
        for effect in &master.effects {
            validate_effect(
                effect,
                EffectKeyOwner::Master,
                &track_names,
                &bus_names,
                &mut errors,
                emissions,
                SourceSubject {
                    owner: SourceOwner::Master,
                    part: SourcePart::Whole,
                },
            );
        }
        validate_input_routes(
            "master",
            None,
            &master.effects,
            &master.input_routes,
            &route_sources,
            &mut errors,
            emissions,
            SourceSubject {
                owner: SourceOwner::Master,
                part: SourcePart::Whole,
            },
        );
        if let Some(loudness) = master.loudness_target {
            validate_finite_range("master.loudness_target", loudness, -80.0, 0.0, &mut errors);
        }
        validate_finite_range(
            "master.volume_db",
            master.volume_db,
            -120.0,
            24.0,
            &mut errors,
        );
    }

    for (index, section) in project.sections.iter().enumerate() {
        let start = errors.len();
        if section.name.trim().is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: "section.name".to_string(),
                value: section.name.clone(),
            });
        }
        validate_time_range(
            &section.time_range,
            &format!("section '{}'", section.name),
            &mut errors,
        );
        if matches!(section.repeat, Some(0)) {
            errors.push(ValidationError::InvalidValue {
                field: format!("section '{}'.repeat", section.name),
                value: "0".to_string(),
            });
        }
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::Section(index),
                part: SourcePart::Whole,
            },
        );
    }

    let mut marker_names = HashSet::new();
    for (index, marker) in project.markers.iter().enumerate() {
        let start = errors.len();
        if marker.name.trim().is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: "marker.name".to_string(),
                value: "a marker needs a name; write @marker \"verse\" at 9:1".to_string(),
            });
        } else if !marker_names.insert(marker.name.as_str()) {
            errors.push(ValidationError::InvalidValue {
                field: format!("marker '{}'.name", marker.name),
                value: format!("another marker already uses the name '{}'", marker.name),
            });
        }
        let position = &marker.position;
        if position.bar == 0 || position.beat == 0 {
            errors.push(ValidationError::InvalidValue {
                field: format!("marker '{}'.position", marker.name),
                value: format!(
                    "{}:{}:{} uses 0; bars and beats are 1-based",
                    position.bar, position.beat, position.subdivision
                ),
            });
        } else if position.subdivision >= 4 {
            errors.push(ValidationError::InvalidValue {
                field: format!("marker '{}'.position", marker.name),
                value: format!(
                    "subdivision {} is past the beat; a beat holds subdivisions 0-3",
                    position.subdivision
                ),
            });
        }
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::Marker(index),
                part: SourcePart::Whole,
            },
        );
    }

    if project.sections.len() > 1
        && let Ok(clock) = crate::timing::ProjectClock::new(&project.properties)
    {
        let mut previous_end = 0.0_f64;
        for (index, section) in project.sections.iter().enumerate() {
            let section_start = errors.len();
            let (Ok(start), Ok(end)) = (
                clock.quarter_notes_at(&section.time_range.start),
                clock.quarter_notes_at(&section.time_range.end),
            ) else {
                continue;
            };
            if end > start && start < previous_end - 1e-9 {
                errors.push(ValidationError::InvalidValue {
                    field: format!("section '{}'.time_range", section.name),
                    value: "sections must be ordered and must not overlap".to_string(),
                });
            }
            emissions.attribute(
                section_start,
                errors.len(),
                SourceSubject {
                    owner: SourceOwner::Section(index),
                    part: SourcePart::Whole,
                },
            );
            previous_end = previous_end.max(end);
        }
    }

    validate_groups(project, &mut errors, emissions);

    errors
}

/// Groups reference clip identifiers, so a member must resolve and an
/// identifier must be unique. A group below the minimum size is a stale stub:
/// the editing commands prune it instead of writing it.
fn validate_groups(
    project: &Project,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let mut clip_ids = HashSet::new();
    let mut duplicate_clip_ids = HashSet::new();
    for clip in project
        .tracks
        .iter()
        .flat_map(|track| track.pattern_clips())
    {
        if let Some(id) = clip.id.as_deref() {
            if id.trim().is_empty() {
                errors.push(ValidationError::InvalidValue {
                    field: "clip.id".to_string(),
                    value: "a clip identifier cannot be empty".to_string(),
                });
            } else if !clip_ids.insert(id) {
                duplicate_clip_ids.insert(id);
            }
        }
    }
    for id in duplicate_clip_ids {
        errors.push(ValidationError::InvalidValue {
            field: "clip.id".to_string(),
            value: format!("two clips use the identifier '{id}'; identifiers are unique"),
        });
    }

    let mut group_ids = HashSet::new();
    for (index, group) in project.groups.iter().enumerate() {
        let start = errors.len();
        let subject = SourceSubject {
            owner: SourceOwner::Group(index),
            part: SourcePart::Whole,
        };
        if group.id.trim().is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: "group.id".to_string(),
                value: "a group needs an identifier; write @group \"g1\" { members: … }"
                    .to_string(),
            });
            emissions.attribute(start, errors.len(), subject);
            continue;
        }
        if !group_ids.insert(group.id.as_str()) {
            errors.push(ValidationError::InvalidValue {
                field: format!("group '{}'.id", group.id),
                value: format!("two groups use the identifier '{}'", group.id),
            });
        }
        if group.members.len() < crate::ast::MIN_GROUP_MEMBERS {
            errors.push(ValidationError::InvalidValue {
                field: format!("group '{}'.members", group.id),
                value: format!(
                    "a group needs at least {} clips; it lists {}",
                    crate::ast::MIN_GROUP_MEMBERS,
                    group.members.len()
                ),
            });
        }
        let mut members = HashSet::new();
        for member in &group.members {
            if !members.insert(member.as_str()) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("group '{}'.members", group.id),
                    value: format!("'{member}' is listed twice"),
                });
            } else if !clip_ids.contains(member.as_str()) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("group '{}'.members", group.id),
                    value: format!(
                        "'{member}' names no clip; a member must carry clip id: {member}"
                    ),
                });
            }
        }
        emissions.attribute(start, errors.len(), subject);
    }
}

fn validate_project_properties(properties: &ProjectProperties, errors: &mut Vec<ValidationError>) {
    if let Err(error) = properties.checked_automation_normalization() {
        errors.push(ValidationError::InvalidValue {
            field: "automation_normalization".into(),
            value: error.to_string(),
        });
    }
    validate_finite_range("tempo", properties.tempo, 1.0, 400.0, errors);
    validate_time_signature(
        properties.time_signature.numerator,
        properties.time_signature.denominator,
        "time_signature",
        errors,
    );
    if let Some(settings) = &properties.picture_clock
        && let Err(error) = crate::timecode::TimecodeClock::from_settings(settings)
    {
        errors.push(ValidationError::InvalidValue {
            field: "picture_clock".into(),
            value: error,
        });
    }
    if let Some(swing) = properties.swing {
        validate_finite_range("swing", swing, 0.0, 100.0, errors);
    }
    if let Some(rate) = properties.export.and_then(|export| export.sample_rate)
        && !(8_000..=192_000).contains(&rate)
    {
        errors.push(ValidationError::InvalidValue {
            field: "export.sample_rate".into(),
            value: format!("sample rate {rate} Hz is outside 8000-192000"),
        });
    }
    if let Some(step) = properties.export.and_then(|export| export.resample)
        && step > 7
    {
        errors.push(ValidationError::InvalidValue {
            field: "export.resample".into(),
            value: format!("resampling step {step} is outside 0-7"),
        });
    }
    if let Some(export) = properties.export {
        if export.flac.level > MAX_FLAC_LEVEL {
            errors.push(ValidationError::InvalidValue {
                field: "export.flac.level".into(),
                value: format!(
                    "compression level {} is outside {MIN_FLAC_LEVEL}-{MAX_FLAC_LEVEL}",
                    export.flac.level
                ),
            });
        }
        if !export.flac.depth.is_integer() {
            errors.push(ValidationError::InvalidValue {
                field: "export.flac.depth".into(),
                value: format!(
                    "{} is not a FLAC depth; FLAC stores 16- or 24-bit integers",
                    export.flac.depth
                ),
            });
        }
        // The exact table depends on the MPEG version, which the encoder knows;
        // here the widest range every version accepts is refused early.
        if !(MIN_MP3_BITRATE_KBPS..=MAX_MP3_BITRATE_KBPS).contains(&export.mp3.bitrate) {
            errors.push(ValidationError::InvalidValue {
                field: "export.mp3.bitrate".into(),
                value: format!(
                    "bitrate {} kbps is outside {}-{} kbps",
                    export.mp3.bitrate, MIN_MP3_BITRATE_KBPS, MAX_MP3_BITRATE_KBPS
                ),
            });
        }
        if let Some(quality) = export.mp3.vbr_quality
            && quality > MAX_MP3_VBR_QUALITY
        {
            errors.push(ValidationError::InvalidValue {
                field: "export.mp3.vbr_quality".into(),
                value: format!(
                    "VBR quality {quality} is outside {MIN_MP3_VBR_QUALITY}-{MAX_MP3_VBR_QUALITY}; 0 is best and 9 smallest"
                ),
            });
        }
    }
    if let Some(step) = properties.live_resample
        && step > 7
    {
        errors.push(ValidationError::InvalidValue {
            field: "live_resample".into(),
            value: format!("resampling step {step} is outside 0-7"),
        });
    }
    for point in &properties.tempo_map {
        validate_bar_beat(&point.position, "tempo_map.position", errors);
        validate_finite_range("tempo_map.bpm", point.bpm, 1.0, 400.0, errors);
    }
    for point in &properties.time_sig_map {
        validate_bar_beat(&point.position, "time_sig_map.position", errors);
        validate_time_signature(point.numerator, point.denominator, "time_sig_map", errors);
    }
    for point in &properties.key_map {
        validate_bar_beat(&point.position, "key_map.position", errors);
    }
    if let Some(mpe) = &properties.mpe {
        if let Some(range) = mpe.lower {
            validate_mpe_range("mpe.lower", range, 2, 16, errors);
        }
        if let Some(range) = mpe.upper {
            validate_mpe_range("mpe.upper", range, 1, 15, errors);
        }
        if let Some(bend_range) = mpe.bend_range {
            validate_finite_range("mpe.bend_range", bend_range, 0.0, 96.0, errors);
        }
        if let (Some(lower), Some(upper)) = (mpe.lower, mpe.upper) {
            let lower_high = lower.first.max(lower.last);
            let upper_low = upper.first.min(upper.last);
            if lower_high >= upper_low {
                errors.push(ValidationError::InvalidValue {
                    field: "mpe.zones".to_string(),
                    value: "lower and upper member ranges overlap".to_string(),
                });
            }
        }
    }
}

fn validate_mpe_range(
    field: &str,
    range: MpeMemberRange,
    minimum: u8,
    maximum: u8,
    errors: &mut Vec<ValidationError>,
) {
    for channel in [range.first, range.last] {
        if !(minimum..=maximum).contains(&channel) {
            errors.push(ValidationError::InvalidValue {
                field: field.to_string(),
                value: format!("channel {channel} outside {minimum}..={maximum}"),
            });
        }
    }
}

fn validate_preset(
    preset: &PresetDef,
    track_names: &HashSet<&str>,
    bus_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
    subject: SourceSubject,
) {
    if preset.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "preset.name".to_string(),
            value: preset.name.clone(),
        });
    }
    if let Some(pitch) = &preset.root_note {
        validate_pitch_octave(
            &format!("preset '{}'.root_note", preset.name),
            pitch,
            errors,
        );
    }
    if matches!(preset.source.as_deref(), Some("soundfont")) && preset.program.is_none() {
        errors.push(ValidationError::InvalidValue {
            field: format!("preset '{}'.program", preset.name),
            value: "missing for soundfont source".to_string(),
        });
    }
    for effect in &preset.effects {
        validate_effect(
            effect,
            EffectKeyOwner::Preset(preset.name.as_str()),
            track_names,
            bus_names,
            errors,
            emissions,
            subject,
        );
    }
    validate_params(&format!("preset '{}'", preset.name), &preset.params, errors);
    if crate::serializer::is_kit_preset(preset) {
        validate_kit(preset, errors);
    }
}

/// Check the `pad.*` / `layer.*` document a `@kit` lowers to.
fn validate_kit(preset: &PresetDef, errors: &mut Vec<ValidationError>) {
    let owner = format!("kit '{}'", preset.name);
    let pads: HashSet<&str> = preset
        .params
        .iter()
        .filter_map(|(key, _)| {
            key.strip_prefix("pad.")
                .and_then(|rest| rest.split_once('.').map(|(pad, _)| pad))
        })
        .collect();
    let mut ranges: Vec<(String, u8, u8)> = Vec::new();
    for (key, value) in &preset.params {
        let Some(rest) = key.strip_prefix("layer.") else {
            continue;
        };
        let Some((pad, field)) = rest.split_once('.') else {
            continue;
        };
        let Some((index, field)) = field.split_once('.') else {
            continue;
        };
        if !pads.contains(pad) {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.{key}"),
                value: "layer targets an undeclared pad".to_string(),
            });
            continue;
        }
        if field == "low" || field == "high" {
            let number = value.as_f64().unwrap_or(-1.0);
            if !(0.0..=127.0).contains(&number) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.{key}"),
                    value: value.as_f64().map(|n| n.to_string()).unwrap_or_default(),
                });
                continue;
            }
            let Some(entry) = ranges
                .iter_mut()
                .find(|(name, _, _)| name == &format!("{pad}.{index}"))
            else {
                ranges.push((format!("{pad}.{index}"), number as u8, number as u8));
                continue;
            };
            if field == "low" {
                entry.1 = number as u8;
            } else {
                entry.2 = number as u8;
            }
        }
    }
    for (layer, low, high) in ranges {
        if low > high {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.layer.{layer}"),
                value: format!("inverted velocity range {low}-{high}"),
            });
        }
    }
}

fn validate_control_source(
    source: &ControlSource,
    owner: SourceOwner,
    track_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    if !track_names.contains(source.source_track.as_str()) {
        emissions.push(
            errors,
            ValidationError::UnknownControlSourceTrack {
                control: source.name.clone(),
                track: source.source_track.clone(),
            },
            SourceSubject {
                owner,
                part: SourcePart::Reference {
                    kind: ReferenceKind::ControlTap,
                    occurrence: 0,
                },
            },
        );
    }
    for (field, value, non_negative) in [
        ("attack", source.attack, true),
        ("release", source.release, true),
        ("scale", source.scale, false),
        ("offset", source.offset, false),
        ("lfo_depth", source.lfo_depth, false),
    ] {
        if !value.is_finite() || (non_negative && value < 0.0) {
            errors.push(ValidationError::InvalidValue {
                field: format!("control source '{}' {field}", source.name),
                value: value.to_string(),
            });
        }
    }
    if let Some(lfo) = &source.lfo
        && (!lfo.rate_hz.is_finite() || lfo.rate_hz <= 0.0)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("control source '{}' lfo rate", source.name),
            value: lfo.rate_hz.to_string(),
        });
    }
    if let Some(formula) = &source.formula
        && let Err(error) = crate::formula::Formula::parse(formula)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("control source '{}' formula", source.name),
            value: error,
        });
    }
}

fn validate_control_links(
    links: &[ControlLink],
    project: &Project,
    track_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let mut link_names = HashSet::new();
    for (index, link) in links.iter().enumerate() {
        let start = errors.len();
        if !link_names.insert(link.name.as_str()) {
            errors.push(ValidationError::InvalidValue {
                field: "control_link.name".into(),
                value: format!("'{}' is defined more than once", link.name),
            });
        }
        match &link.source {
            ControlLinkSource::MidiCc {
                channel,
                controller,
            } => {
                if !(1..=16).contains(channel) {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' channel", link.name),
                        value: channel.to_string(),
                    });
                }
                if *controller > 127 {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' controller", link.name),
                        value: controller.to_string(),
                    });
                }
            }
            ControlLinkSource::Macro { index } => {
                if !(1..=8).contains(index) {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' macro", link.name),
                        value: index.to_string(),
                    });
                }
            }
            ControlLinkSource::Lane { track, param } => {
                if !track_names.contains(track.as_str()) {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' lane track", link.name),
                        value: track.clone(),
                    });
                } else if param.trim().is_empty() {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' lane param", link.name),
                        value: "missing lane parameter".into(),
                    });
                } else if !project
                    .tracks
                    .iter()
                    .find(|candidate| &candidate.name == track)
                    .is_some_and(|candidate| {
                        candidate.automation().any(|lane| &lane.param == param)
                    })
                {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("control link '{}' lane", link.name),
                        value: format!("track '{track}' has no automation lane '{param}'"),
                    });
                }
            }
        }
        if link.targets.is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: format!("control link '{}' targets", link.name),
                value: "a link needs at least one target".into(),
            });
        }
        let mut seen = HashSet::new();
        for target in &link.targets {
            if !track_names.contains(target.track.as_str()) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("control link '{}' target track", link.name),
                    value: target.track.clone(),
                });
            }
            if target.param.trim().is_empty() {
                errors.push(ValidationError::InvalidValue {
                    field: format!("control link '{}' target param", link.name),
                    value: "missing parameter".into(),
                });
            }
            if !seen.insert((target.track.as_str(), target.param.as_str())) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("control link '{}' targets", link.name),
                    value: format!("'{}:{}' is bound twice", target.track, target.param),
                });
            }
            if target.formula.input() == LinkFormulaInput::Midi7Bit && !link.source.is_midi() {
                errors.push(ValidationError::InvalidValue {
                    field: format!("control link '{}' formula", link.name),
                    value: format!(
                        "'{}' reads raw MIDI values; the source is not a MIDI CC",
                        target.formula
                    ),
                });
            }
            if target.smoothing_ms > 60_000 {
                errors.push(ValidationError::InvalidValue {
                    field: format!("control link '{}' smoothing", link.name),
                    value: format!("{}ms is longer than 60s", target.smoothing_ms),
                });
            }
        }
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::ControlLink(index),
                part: SourcePart::Whole,
            },
        );
    }
}

fn validate_audio_clip(
    clip: &AudioClip,
    owner: SourceOwner,
    properties: &ProjectProperties,
    track_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    if clip.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "audio_clip.name".to_string(),
            value: clip.name.clone(),
        });
    }
    if !track_names.contains(clip.track.as_str()) {
        emissions.push(
            errors,
            ValidationError::UnknownAudioClipTrack {
                clip: clip.name.clone(),
                track: clip.track.clone(),
            },
            SourceSubject {
                owner,
                part: SourcePart::Reference {
                    kind: ReferenceKind::Track,
                    occurrence: 0,
                },
            },
        );
    }
    if clip.file.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.file", clip.name),
            value: "empty path".to_string(),
        });
    } else if std::path::Path::new(&clip.file).is_absolute() {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.file", clip.name),
            value: "path must be relative to the project".to_string(),
        });
    }
    for (field, value) in [
        ("offset", clip.offset_seconds),
        ("loop", clip.loop_seconds),
        ("gain", clip.gain_db),
        ("fade_in", clip.fade_in_seconds),
        ("fade_out", clip.fade_out_seconds),
        ("crossfade", clip.crossfade_seconds),
    ] {
        let nonnegative = field != "gain";
        if !value.is_finite() || (nonnegative && value < 0.0) {
            errors.push(ValidationError::InvalidValue {
                field: format!("audio clip '{}'.{field}", clip.name),
                value: value.to_string(),
            });
        }
    }
    if !clip.stretch.is_finite() || clip.stretch <= 0.0 {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.stretch", clip.name),
            value: clip.stretch.to_string(),
        });
    }
    if !clip.pitch.is_finite() || clip.pitch.abs() > MAX_PITCH_SEMITONES {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.pitch", clip.name),
            value: clip.pitch.to_string(),
        });
    }
    if clip.stretch_mode == StretchMode::Varispeed && !clip.varispeed_controls_compatible() {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.mode", clip.name),
            value: "varispeed requires zero independent pitch and formant shifts, no formant preservation, and no warp markers".into(),
        });
    }
    if !clip.warp_markers.is_empty() {
        let mut source = 0.0;
        let mut target = 0.0;
        let valid = clip.warp_markers.len() <= 128
            && clip.stretch_mode != StretchMode::Off
            && !clip.tempo_follow
            && clip.warp_markers.iter().all(|marker| {
                let ordered = marker.source_seconds.is_finite()
                    && marker.target_seconds.is_finite()
                    && marker.source_seconds > source
                    && marker.target_seconds > target;
                source = marker.source_seconds;
                target = marker.target_seconds;
                ordered
            });
        if !valid {
            errors.push(ValidationError::InvalidValue {
                field: format!("audio clip '{}'.warp", clip.name),
                value: "use up to 128 increasing source and target anchors with stretching on and tempo follow off".into(),
            });
        }
    }
    let correction = clip.formant
        - if clip.preserve_formants {
            clip.pitch
        } else {
            0.0
        };
    if !clip.formant.is_finite() || clip.formant.abs() > 12.0 || correction.abs() > 12.0 {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.formant", clip.name),
            value: "formant shift and pitch compensation must each be within ±12 semitones".into(),
        });
    }
    if clip.stretch_mode == StretchMode::Off && clip.formant != 0.0 {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.formant", clip.name),
            value: "formant shift needs a stretch mode other than off".into(),
        });
    }
    if clip
        .take_group
        .as_deref()
        .is_some_and(|group| group.trim().is_empty())
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.take_group", clip.name),
            value: "empty group".to_string(),
        });
    }
    if let Err(value) = crate::timing::ProjectClock::new(properties)
        .and_then(|clock| clock.audio_clip_range_seconds(clip))
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.placement", clip.name),
            value,
        });
    }
    validate_gain_envelope(clip, errors);
    if clip.take == Some(0) {
        errors.push(ValidationError::InvalidValue {
            field: format!("audio clip '{}'.take", clip.name),
            value: "take numbers start at 1".to_string(),
        });
    }
}

/// A gain envelope is either absent or a ramp that spans the whole clip: two
/// points at least, from the head to the tail, in strictly increasing order.
fn validate_gain_envelope(clip: &AudioClip, errors: &mut Vec<ValidationError>) {
    if clip.gain_envelope.is_empty() {
        return;
    }
    let field = format!("audio clip '{}'.gain_envelope", clip.name);
    let refuse = |errors: &mut Vec<ValidationError>, value: String| {
        errors.push(ValidationError::InvalidValue {
            field: field.clone(),
            value,
        });
    };
    if clip.gain_envelope.len() < 2 {
        refuse(errors, "an envelope needs at least two points".to_string());
        return;
    }
    let positions: Vec<f64> = clip.gain_envelope.iter().map(|p| p.position).collect();
    if positions[0] != 0.0 || positions[positions.len() - 1] != 1.0 {
        refuse(
            errors,
            "the first position must be 0 and the last 1".to_string(),
        );
    }
    for point in &clip.gain_envelope {
        if !point.position.is_finite() || !(0.0..=1.0).contains(&point.position) {
            refuse(errors, format!("position {}", point.position));
        }
        if !point.gain_db.is_finite() {
            refuse(errors, format!("gain {}dB", point.gain_db));
        }
    }
    for pair in positions.windows(2) {
        if pair[1] <= pair[0] {
            refuse(
                errors,
                format!("positions {} and {} are not ascending", pair[0], pair[1]),
            );
        }
    }
}

fn validate_vocal_tunes(
    properties: &ProjectProperties,
    track_names: &HashSet<&str>,
    audio_clips: &HashMap<&str, &AudioClip>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let mut clips_per_track: HashMap<&str, Vec<&str>> = HashMap::new();
    for clip in audio_clips.values() {
        clips_per_track
            .entry(clip.track.as_str())
            .or_default()
            .push(clip.name.as_str());
    }
    let mut names: HashSet<String> = HashSet::new();
    let mut tuned_clips: HashMap<String, String> = HashMap::new();
    for (index, tune) in properties.vocal_tunes.iter().enumerate() {
        let start = errors.len();
        let mut check = |tune: &VocalTune| {
            if tune.name.trim().is_empty() {
                errors.push(ValidationError::InvalidValue {
                    field: "vocal_tune.name".to_string(),
                    value: tune.name.clone(),
                });
            }
            if !names.insert(tune.name.clone()) {
                errors.push(ValidationError::DuplicateVocalTuneName {
                    name: tune.name.clone(),
                });
            }
            let clip_name = match (&tune.clip, &tune.track) {
                (Some(clip), None) => {
                    if !audio_clips.contains_key(clip.as_str()) {
                        errors.push(ValidationError::UnknownVocalTuneClip {
                            tune: tune.name.clone(),
                            clip: clip.clone(),
                        });
                        return;
                    }
                    clip.clone()
                }
                (None, Some(track)) => {
                    if !track_names.contains(track.as_str()) {
                        errors.push(ValidationError::UnknownVocalTuneTrack {
                            tune: tune.name.clone(),
                            track: track.clone(),
                        });
                        return;
                    }
                    let clips = clips_per_track
                        .get(track.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if clips.len() != 1 {
                        errors.push(ValidationError::MissingVocalTuneClip {
                            tune: tune.name.clone(),
                            track: track.clone(),
                        });
                        return;
                    }
                    clips[0].to_string()
                }
                (Some(clip), Some(track)) => {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("vocal tune '{}'.selector", tune.name),
                        value: format!("clip '{clip}' and track '{track}' are mutually exclusive"),
                    });
                    return;
                }
                (None, None) => {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("vocal tune '{}'.selector", tune.name),
                        value: "needs clip: or track:".to_string(),
                    });
                    return;
                }
            };
            validate_vocal_snap(tune, &clip_name, audio_clips, errors);
            if let Some(existing) = tuned_clips.insert(clip_name.clone(), tune.name.clone()) {
                errors.push(ValidationError::DuplicateVocalTuneClip {
                    tune: tune.name.clone(),
                    clip: clip_name,
                    existing,
                });
            }
            validate_finite_range(
                &format!("vocal tune '{}'.amount", tune.name),
                tune.amount,
                0.0,
                100.0,
                errors,
            );
            validate_finite_range(
                &format!("vocal tune '{}'.retune", tune.name),
                tune.retune_seconds,
                0.0,
                1.0,
                errors,
            );
            validate_finite_range(
                &format!("vocal tune '{}'.formant", tune.name),
                tune.formant_cents,
                -1200.0,
                1200.0,
                errors,
            );
            validate_finite_range(
                &format!("vocal tune '{}'.max_move", tune.name),
                tune.max_move_seconds,
                0.0,
                0.5,
                errors,
            );
            let mut last = f64::NEG_INFINITY;
            for (index, note) in tune.notes.iter().enumerate() {
                let field = format!("vocal tune '{}'.note[{}]", tune.name, index);
                if !note.at.is_finite() || note.at < 0.0 {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("{field}.at"),
                        value: note.at.to_string(),
                    });
                } else if note.at <= last {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("{field}.at"),
                        value: format!("{}: anchors must be strictly ascending", note.at),
                    });
                }
                last = note.at;
                if let Some(amount) = note.amount {
                    validate_finite_range(&format!("{field}.amount"), amount, 0.0, 100.0, errors);
                }
                if let Some(retune) = note.retune_seconds {
                    validate_finite_range(&format!("{field}.retune"), retune, 0.0, 1.0, errors);
                }
                if let Some(formant) = note.formant_cents {
                    validate_finite_range(
                        &format!("{field}.formant"),
                        formant,
                        -1200.0,
                        1200.0,
                        errors,
                    );
                }
                if let Some(start) = note.start_seconds
                    && (!start.is_finite() || start.abs() > tune.max_move_seconds)
                {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("{field}.start"),
                        value: format!(
                            "{start}s: the block moves at most ±{}s (max_move)",
                            tune.max_move_seconds
                        ),
                    });
                }
                if let Some(length) = note.length {
                    validate_finite_range(&format!("{field}.length"), length, 0.25, 4.0, errors);
                }
                if let Some(target) = note.target {
                    validate_vocal_target(&target, tune, properties, &field, errors);
                }
            }
        };
        check(tune);
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::VocalTune(index),
                part: SourcePart::Whole,
            },
        );
    }
}

/// Harmony blocks: the source clip is real, one voice per clip, and the
/// interval, level and pan are inside the ranges the bake can honour.
fn validate_vocal_harmonies(
    properties: &ProjectProperties,
    track_names: &HashSet<&str>,
    audio_clips: &HashMap<&str, &AudioClip>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let mut clips_per_track: HashMap<&str, Vec<&str>> = HashMap::new();
    for clip in audio_clips.values() {
        clips_per_track
            .entry(clip.track.as_str())
            .or_default()
            .push(clip.name.as_str());
    }
    let mut names: HashSet<String> = HashSet::new();
    let mut voiced_clips: HashMap<String, String> = HashMap::new();
    for (index, harmony) in properties.vocal_harmonies.iter().enumerate() {
        let start = errors.len();
        let mut check = |harmony: &VocalHarmony| {
            if harmony.name.trim().is_empty() {
                errors.push(ValidationError::InvalidValue {
                    field: "vocal_harmony.name".to_string(),
                    value: harmony.name.clone(),
                });
            }
            if !names.insert(harmony.name.clone()) {
                errors.push(ValidationError::DuplicateVocalHarmonyName {
                    name: harmony.name.clone(),
                });
            }
            let clip_name = match (&harmony.clip, &harmony.track) {
                (Some(clip), None) => {
                    if !audio_clips.contains_key(clip.as_str()) {
                        errors.push(ValidationError::UnknownVocalHarmonyClip {
                            harmony: harmony.name.clone(),
                            clip: clip.clone(),
                        });
                        return;
                    }
                    clip.clone()
                }
                (None, Some(track)) => {
                    if !track_names.contains(track.as_str()) {
                        errors.push(ValidationError::UnknownVocalHarmonyTrack {
                            harmony: harmony.name.clone(),
                            track: track.clone(),
                        });
                        return;
                    }
                    let clips = clips_per_track
                        .get(track.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if clips.len() != 1 {
                        errors.push(ValidationError::MissingVocalHarmonyClip {
                            harmony: harmony.name.clone(),
                            track: track.clone(),
                        });
                        return;
                    }
                    clips[0].to_string()
                }
                (Some(clip), Some(track)) => {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("vocal harmony '{}'.selector", harmony.name),
                        value: format!("clip '{clip}' and track '{track}' are mutually exclusive"),
                    });
                    return;
                }
                (None, None) => {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("vocal harmony '{}'.selector", harmony.name),
                        value: "needs clip: or track:".to_string(),
                    });
                    return;
                }
            };
            if let Some(existing) = voiced_clips.insert(clip_name.clone(), harmony.name.clone()) {
                errors.push(ValidationError::DuplicateVocalHarmonyClip {
                    harmony: harmony.name.clone(),
                    clip: clip_name,
                    existing,
                });
            }
            let field = format!("vocal harmony '{}'", harmony.name);
            if !(-12..=12).contains(&harmony.interval) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.interval"),
                    value: format!(
                        "{}: the block accepts -12..=12 diatonic steps",
                        harmony.interval
                    ),
                });
            }
            if harmony.interval == 0 {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.interval"),
                    value: "0: a unison copy is not a harmony".to_string(),
                });
            }
            validate_finite_range(
                &format!("{field}.level"),
                harmony.level_db,
                -60.0,
                12.0,
                errors,
            );
            validate_finite_range(&format!("{field}.pan"), harmony.pan, -1.0, 1.0, errors);
        };
        check(harmony);
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::VocalHarmony(index),
                part: SourcePart::Whole,
            },
        );
    }
}

/// Snap fields: the reference must be a real other clip, and a policy that
/// names one automatic source cannot also name the other.
fn validate_vocal_snap(
    tune: &VocalTune,
    clip_name: &str,
    audio_clips: &HashMap<&str, &AudioClip>,
    errors: &mut Vec<ValidationError>,
) {
    let field = |item: &str| format!("vocal tune '{}'.{item}", tune.name);
    if let Some(reference) = &tune.reference {
        if reference == clip_name {
            errors.push(ValidationError::InvalidValue {
                field: field("reference"),
                value: format!("'{reference}' is the tuned clip; it cannot reference itself"),
            });
        } else if !audio_clips.contains_key(reference.as_str()) {
            errors.push(ValidationError::UnknownVocalTuneReference {
                tune: tune.name.clone(),
                reference: reference.clone(),
            });
        }
        if tune.snap == VocalSnapPolicy::Scale {
            errors.push(ValidationError::InvalidValue {
                field: field("reference"),
                value: "snap: scale keeps the scale; drop reference: or use snap: auto".to_string(),
            });
        }
    }
    if tune.snap == VocalSnapPolicy::Reference && tune.reference.is_none() {
        errors.push(ValidationError::InvalidValue {
            field: field("snap"),
            value: "snap: reference needs reference: <audio clip>".to_string(),
        });
    }
    validate_finite_range(&field("cap"), tune.snap_cap_cents, 0.0, 1200.0, errors);
    validate_finite_range(&field("window"), tune.snap_window_cents, 0.0, 200.0, errors);
}

fn validate_vocal_target(
    target: &VocalTarget,
    tune: &VocalTune,
    properties: &ProjectProperties,
    field: &str,
    errors: &mut Vec<ValidationError>,
) {
    match target {
        VocalTarget::Pitch(midi) => {
            if Pitch::from_midi(*midi).is_none() {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.tgt"),
                    value: format!("MIDI note {midi} must be within 0..127"),
                });
            }
        }
        VocalTarget::Cents(cents) => {
            validate_finite_range(&format!("{field}.tgt"), *cents, 0.0, 12_700.0, errors);
        }
        VocalTarget::Semitones(semitones) => {
            validate_finite_range(&format!("{field}.tgt"), *semitones, -48.0, 48.0, errors);
        }
        VocalTarget::OffsetCents(cents) => {
            validate_finite_range(&format!("{field}.tgt"), *cents, -4800.0, 4800.0, errors);
        }
        VocalTarget::Degree { degree, octave } => {
            if !(1..=7).contains(degree) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.tgt"),
                    value: format!("degree {degree} must be 1..=7"),
                });
            }
            if !(-1..=9).contains(octave) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.tgt"),
                    value: format!("octave {octave} must be -1..=9"),
                });
            }
            if tune.scale.is_none() && properties.key.is_none() && properties.key_map.is_empty() {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{field}.tgt"),
                    value: "a degree target needs a scale; set scale: or @project key:".to_string(),
                });
            }
        }
    }
}

struct TrackValidationContext<'a> {
    project: &'a Project,
    preset_names: &'a HashSet<&'a str>,
    pattern_names: &'a HashSet<&'a str>,
    track_names: &'a HashSet<&'a str>,
    bus_names: &'a HashSet<&'a str>,
}

fn validate_track(
    track: &Track,
    owner: SourceOwner,
    context: &TrackValidationContext<'_>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let TrackValidationContext {
        project,
        preset_names,
        pattern_names,
        track_names,
        bus_names,
    } = *context;
    let properties = &project.properties;
    if track.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "track.name".to_string(),
            value: track.name.clone(),
        });
    }
    if let Some(timeline) = &track.performance {
        if let Err(value) = timeline.validate() {
            errors.push(ValidationError::InvalidValue {
                field: format!("{}.performance", track.name),
                value,
            });
        }
        if !track.phrases.is_empty()
            || !track.patterns.is_empty()
            || !track.inline_patterns.is_empty()
            || track.pattern_clip_count() > 0
            || track.automation_count() > 0
        {
            errors.push(ValidationError::InvalidValue {
                field: format!("{}.performance", track.name),
                value: "performance commands cannot coexist with note-derived content or automation; for a sustain pedal write `@2.0 pedal down` in the phrase"
                    .into(),
            });
        }
    }
    validate_instrument_path(&track.name, &track.instrument, errors);
    if let Some(preset) = &track.preset
        && !preset_names.contains(preset.as_str())
    {
        emissions.push(
            errors,
            ValidationError::UnknownPreset {
                owner: format!("track '{}'", track.name),
                preset: preset.clone(),
            },
            SourceSubject {
                owner,
                part: SourcePart::Reference {
                    kind: ReferenceKind::Preset,
                    occurrence: 0,
                },
            },
        );
    }
    validate_mix(track, bus_names, errors);
    if let Some(channel) = track.channel
        && channel > 15
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("track '{}'.channel", track.name),
            value: channel.to_string(),
        });
    }
    for (field, value) in [("port", track.output_port), ("pair", track.output_pair)] {
        if let Some(value) = value
            && value > 63
        {
            errors.push(ValidationError::InvalidValue {
                field: format!("track '{}'.{field}", track.name),
                value: value.to_string(),
            });
        }
    }
    for effect in &track.effects {
        validate_effect(
            effect,
            EffectKeyOwner::Track(track.name.as_str()),
            track_names,
            bus_names,
            errors,
            emissions,
            SourceSubject {
                owner,
                part: SourcePart::Whole,
            },
        );
    }
    for phrase in &track.phrases {
        validate_phrase(&track.name, phrase, properties, errors);
    }
    for pattern in &track.patterns {
        validate_pattern(&track.name, pattern, errors);
    }
    for pattern in &track.inline_patterns {
        validate_pattern_def(pattern, None, properties, preset_names, errors, emissions);
    }
    for clip in track.pattern_clips() {
        if !pattern_names.contains(clip.pattern.as_str()) {
            errors.push(ValidationError::UnknownPattern {
                track: track.name.clone(),
                pattern: clip.pattern.clone(),
            });
        }
        validate_time_range(
            &clip.time_range,
            &format!("clip '{}' on track '{}'", clip.pattern, track.name),
            errors,
        );
        for (field, value) in [
            ("offset", clip.offset_quarters),
            ("loop", clip.loop_quarters),
        ] {
            if !value.is_finite() || value < 0.0 {
                errors.push(ValidationError::InvalidValue {
                    field: format!("clip '{}' on track '{}'.{field}", clip.pattern, track.name),
                    value: value.to_string(),
                });
            }
        }
        // A loop is the block's repeat period, so it cannot span the whole block.
        if clip.loop_quarters > 0.0
            && let Ok(clock) = crate::timing::ProjectClock::new(properties)
            && let (Ok(start), Ok(end)) = (
                clock.quarter_notes_at(&clip.time_range.start),
                clock.quarter_notes_at(&clip.time_range.end),
            )
            && clip.loop_quarters >= end - start - 1e-9
        {
            errors.push(ValidationError::InvalidValue {
                field: format!("clip '{}' on track '{}'.loop", clip.pattern, track.name),
                value: format!(
                    "{} beats is not shorter than the clip's {} beats",
                    clip.loop_quarters,
                    end - start
                ),
            });
        }
    }
    validate_automation_lanes(
        track.automation(),
        &track.effects,
        properties,
        &format!("track '{}'", track.name),
        synth_controls(project, track).as_ref(),
        errors,
        emissions,
        SourceSubject {
            owner,
            part: SourcePart::Whole,
        },
    );
    let gain_start = errors.len();
    validate_gain_automation(
        &track.gain_automation,
        properties,
        &format!("track '{}'", track.name),
        errors,
    );
    emissions.attribute(
        gain_start,
        errors.len(),
        SourceSubject {
            owner,
            part: SourcePart::Whole,
        },
    );
    validate_hairpins(track, project, properties, errors);
    if let Some(params) = &track.engine_params {
        validate_params(
            &format!("track '{}'.engine_params", track.name),
            params,
            errors,
        );
    }
}

fn validate_pattern_def(
    pattern: &PatternDef,
    owner: Option<SourceOwner>,
    properties: &ProjectProperties,
    preset_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    if pattern.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "pattern.name".to_string(),
            value: pattern.name.clone(),
        });
    }
    validate_instrument_path(
        &format!("pattern '{}'", pattern.name),
        &pattern.instrument,
        errors,
    );
    if let Some(preset) = &pattern.preset
        && !preset_names.contains(preset.as_str())
    {
        let error = ValidationError::UnknownPreset {
            owner: format!("pattern '{}'", pattern.name),
            preset: preset.clone(),
        };
        if let Some(owner) = owner {
            emissions.push(
                errors,
                error,
                SourceSubject {
                    owner,
                    part: SourcePart::Reference {
                        kind: ReferenceKind::Preset,
                        occurrence: 0,
                    },
                },
            );
        } else {
            errors.push(error);
        }
    }
    if let Some(transpose) = pattern.transpose
        && !(-127..=127).contains(&transpose)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("pattern '{}'.transpose", pattern.name),
            value: format!("{transpose} is outside -127..=127 semitones"),
        });
    }
    if let Some(swing) = pattern.swing {
        validate_finite_range(
            &format!("pattern '{}'.swing", pattern.name),
            swing,
            0.0,
            100.0,
            errors,
        );
    }
    if let Some(bars) = pattern.bars
        && (!bars.is_finite() || bars <= 0.0)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("pattern '{}'.bars", pattern.name),
            value: format!("{bars} is not a length above zero"),
        });
    }
    for event in &pattern.events {
        validate_note_event(&format!("pattern '{}'", pattern.name), event, errors);
    }
    let mut content_end: f64 = 0.0;
    for event in &pattern.events {
        let gate = performance_gate_quarters(&event.modifiers)
            .ok()
            .flatten()
            .unwrap_or(event.duration.to_beats());
        content_end = content_end.max(event.start_beat + gate);
    }
    validate_pedals(
        &format!("pattern '{}'", pattern.name),
        "pattern",
        &pattern.pedals,
        (content_end > 0.0).then_some(content_end),
        &pattern.events,
        errors,
    );
    let hairpin_field = format!("pattern '{}'.hairpins", pattern.name);
    for mark in &pattern.hairpins {
        if !mark.beat.is_finite() || mark.beat < 0.0 {
            errors.push(ValidationError::InvalidValue {
                field: hairpin_field.clone(),
                value: format!(
                    "{} at beat {}: the position must be finite and at or after 0",
                    mark.kind.keyword(),
                    mark.beat
                ),
            });
        }
    }
    if !pattern.hairpins.is_empty()
        && let Ok(clock) = crate::timing::ProjectClock::new(properties)
        && let Err(value) =
            crate::dynamics::expand_pattern_with(pattern, &clock, 0.0, None, Some(0.0))
    {
        errors.push(ValidationError::InvalidValue {
            field: hairpin_field,
            value,
        });
    }
    for hit in &pattern.hits {
        validate_hit_event(&format!("pattern '{}'", pattern.name), hit, errors);
    }
}

/// Refuse hairpins that cannot expand, and expansions that collide with the
/// track's own gain clips.
fn validate_hairpins(
    track: &Track,
    project: &Project,
    properties: &ProjectProperties,
    errors: &mut Vec<ValidationError>,
) {
    let mut generated = Vec::new();
    for phrase in &track.phrases {
        if phrase.hairpins.is_empty() {
            continue;
        }
        let field = format!(
            "phrase '{}' on track '{}'.hairpins",
            phrase.name, track.name
        );
        for mark in &phrase.hairpins {
            if !mark.beat.is_finite() || mark.beat < 0.0 {
                errors.push(ValidationError::InvalidValue {
                    field: field.clone(),
                    value: format!(
                        "{} at beat {}: the position must be finite and at or after 0",
                        mark.kind.keyword(),
                        mark.beat
                    ),
                });
            }
        }
        match crate::dynamics::expand_phrase(phrase, properties) {
            Ok(expansions) => generated.extend(expansions.into_iter().map(|one| one.clip)),
            Err(value) => errors.push(ValidationError::InvalidValue { field, value }),
        }
    }
    if let Ok(clock) = crate::timing::ProjectClock::new(properties) {
        match crate::dynamics::pattern_placement_clips(track, project, &clock) {
            Ok(clips) => generated.extend(clips),
            // A mark that cannot expand was already named with its pattern; do
            // not repeat it for every placement. A loop-seam overlap is new.
            Err(value) => {
                let reported = errors.iter().any(|error| {
                    matches!(error, ValidationError::InvalidValue { value: seen, .. } if seen == &value)
                });
                if !reported {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("track '{}'.patterns.hairpins", track.name),
                        value,
                    });
                }
            }
        }
    }
    if generated.is_empty() {
        return;
    }
    if let Err(value) = crate::dynamics::merge_clips(&track.name, &track.gain_automation, generated)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("track '{}'.gain_automation", track.name),
            value,
        });
    }
}

fn validate_phrase(
    track: &str,
    phrase: &Phrase,
    properties: &ProjectProperties,
    errors: &mut Vec<ValidationError>,
) {
    if phrase.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "phrase.name".to_string(),
            value: phrase.name.clone(),
        });
    }
    if let Some(channel) = phrase.channel
        && channel > 15
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("phrase '{}'.channel", phrase.name),
            value: channel.to_string(),
        });
    }
    let mut phrase_beats = None;
    if let Some(range) = &phrase.time_range {
        validate_time_range(
            range,
            &format!("phrase '{}' on track '{}'", phrase.name, track),
            errors,
        );
        let beats = match crate::timing::ProjectClock::new(properties).and_then(|clock| {
            Ok(clock.quarter_notes_at(&range.end)? - clock.quarter_notes_at(&range.start)?)
        }) {
            Ok(beats) => beats,
            Err(error) => {
                errors.push(ValidationError::InvalidValue {
                    field: format!("phrase '{}'.time_range", phrase.name),
                    value: error,
                });
                return;
            }
        };
        phrase_beats = Some(beats);
        for event in &phrase.events {
            let gate = performance_gate_quarters(&event.modifiers)
                .ok()
                .flatten()
                .unwrap_or(event.duration.to_beats());
            if event.start_beat + gate > beats + 0.000_1 {
                errors.push(ValidationError::TimeRangeOutOfBounds {
                    track: track.to_string(),
                    phrase: phrase.name.clone(),
                });
                break;
            }
        }
    }
    for event in &phrase.events {
        validate_note_event(
            &format!("phrase '{}' on track '{}'", phrase.name, track),
            event,
            errors,
        );
    }
    validate_pedals(
        &format!("phrase '{}' on track '{}'", phrase.name, track),
        "phrase",
        &phrase.pedals,
        phrase_beats,
        &phrase.events,
        errors,
    );
}

/// Refuse pedal marks that cannot play: a lift with nothing down, a second
/// press while down, and a mark past the content it would hold.
fn validate_pedals(
    label: &str,
    kind: &str,
    pedals: &[PedalEvent],
    content_beats: Option<f64>,
    events: &[NoteEvent],
    errors: &mut Vec<ValidationError>,
) {
    let field = format!("{label}.pedals");
    let mut down_at = None;
    for pedal in pedals {
        let state = if pedal.down { "down" } else { "up" };
        if !pedal.beat.is_finite() || pedal.beat < 0.0 {
            errors.push(ValidationError::InvalidValue {
                field: field.clone(),
                value: format!(
                    "pedal {state} at beat {}: the position must be finite and at or after 0",
                    pedal.beat
                ),
            });
            continue;
        }
        if let Some(limit) = content_beats
            && pedal.beat > limit + 1e-6
        {
            errors.push(ValidationError::InvalidValue {
                field: field.clone(),
                value: format!(
                    "pedal {state} at beat {} is past the end of the {kind} ({limit} beats)",
                    pedal.beat
                ),
            });
            continue;
        }
        match (pedal.down, down_at) {
            (true, Some(since)) => errors.push(ValidationError::InvalidValue {
                field: field.clone(),
                value: format!(
                    "pedal down at beat {} while the pedal is already down since beat {since}",
                    pedal.beat
                ),
            }),
            (false, None) => errors.push(ValidationError::InvalidValue {
                field: field.clone(),
                value: format!(
                    "pedal up at beat {} without a pedal down before it",
                    pedal.beat
                ),
            }),
            (true, None) => down_at = Some(pedal.beat),
            (false, Some(_)) => down_at = None,
        }
    }
    if !pedals.is_empty() && events.is_empty() {
        errors.push(ValidationError::InvalidValue {
            field,
            value: "pedal needs notes to hold: put it where the container has note events".into(),
        });
    }
}

fn validate_pattern(track: &str, pattern: &Pattern, errors: &mut Vec<ValidationError>) {
    if pattern.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "pattern.name".to_string(),
            value: pattern.name.clone(),
        });
    }
    if pattern.repeat == 0 {
        errors.push(ValidationError::InvalidValue {
            field: format!("track '{}'.pattern '{}'.repeat", track, pattern.name),
            value: "0".to_string(),
        });
    }
    if let Some(range) = &pattern.time_range {
        validate_time_range(
            range,
            &format!("pattern '{}' on track '{}'", pattern.name, track),
            errors,
        );
    }
    for hit in &pattern.hits {
        validate_hit_event(
            &format!("pattern '{}' on track '{}'", pattern.name, track),
            hit,
            errors,
        );
    }
}

// The lane collection plus its owner/attribution context is one validation unit.
#[allow(clippy::too_many_arguments)]
fn validate_automation_lanes<'a>(
    lanes: impl IntoIterator<Item = &'a AutomationLane>,
    effects: &[Effect],
    properties: &ProjectProperties,
    owner: &str,
    synth: Option<&SynthControls>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
    subject: SourceSubject,
) {
    let start = errors.len();
    let lanes: Vec<&AutomationLane> = lanes.into_iter().collect();
    for lane in &lanes {
        let field = format!("{owner}.automation.{}", lane.param);
        if lane.ppq > 96000 {
            errors.push(ValidationError::InvalidValue {
                field: format!("{field}.ppq"),
                value: lane.ppq.to_string(),
            });
        }
        if !lane.smoothing_ms.is_finite() || !(0.0..=MAX_SMOOTHING_MS).contains(&lane.smoothing_ms)
        {
            errors.push(ValidationError::InvalidValue {
                field: format!("{field}.smoothing"),
                value: lane.smoothing_ms.to_string(),
            });
        }
        // A muted clip is silent, not exempt: its span and its points still
        // have to make sense once the mute is lifted.
        if let Some(range) = &lane.time_range {
            validate_time_range(range, &field, errors);
        }
        let effect_lane = lane.param.contains('.');
        if effect_lane {
            validate_effect_lane(lane, effects, owner, synth, errors);
        } else if lane.smoothing_ms != 0.0
            || lane.points.iter().any(|point| point.curve != Curve::Linear)
        {
            errors.push(ValidationError::InvalidAutomationLane {
                owner: owner.to_string(),
                lane: lane.param.clone(),
                reason:
                    "curves and smoothing apply to effect-parameter lanes; a control lane is an event step list"
                        .to_string(),
            });
        }
        if !effect_lane && let Some(reason) = synth.and_then(|synth| synth_lane_reason(lane, synth))
        {
            errors.push(ValidationError::InvalidAutomationLane {
                owner: owner.to_string(),
                lane: lane.param.clone(),
                reason,
            });
        }
        if lane.is_lfo() {
            if !effect_lane {
                errors.push(ValidationError::InvalidAutomationLane {
                    owner: owner.to_string(),
                    lane: lane.param.clone(),
                    reason:
                        "LFO mode applies to effect-parameter lanes; a control lane is an event step list"
                            .to_string(),
                });
            }
            if let Err(reason) = crate::automation::validate_lane_lfo(&lane.lfo) {
                errors.push(ValidationError::InvalidAutomationLane {
                    owner: owner.to_string(),
                    lane: lane.param.clone(),
                    reason,
                });
            }
        }
        let tick_clock = if lane.ppq == 0 {
            None
        } else {
            crate::timing::ProjectClock::new(properties).ok()
        };
        for point in &lane.points {
            if lane.ppq != 0 {
                if let Some(clock) = &tick_clock
                    && let Err(value) = clock.quarter_notes_at_ppq(&point.position, lane.ppq)
                {
                    errors.push(ValidationError::InvalidValue {
                        field: field.clone(),
                        value,
                    });
                }
            } else {
                validate_bar_beat(&point.position, &field, errors);
            }
            validate_param_value(&field, &point.value, errors);
            if let Some(tension) = point.tension {
                if point.curve != Curve::Smooth {
                    errors.push(ValidationError::InvalidAutomationLane {
                        owner: owner.to_string(),
                        lane: lane.param.clone(),
                        reason: "tension needs curve=smooth".to_string(),
                    });
                } else if !tension.is_finite() || !(0.0..=1.0).contains(&tension) {
                    errors.push(ValidationError::InvalidValue {
                        field: format!("{field}.tension"),
                        value: tension.to_string(),
                    });
                }
            }
        }
    }
    validate_automation_spans(&lanes, properties, owner, errors);
    emissions.attribute(start, errors.len(), subject);
}

/// Two lanes that address one target must not write at the same time.
///
/// A lane without a range drives the whole timeline from its first point on, so
/// it excludes every other lane on its target. Adjacent clips are allowed:
/// `[1:1 - 3:1]` and `[3:1 - 5:1]` touch without overlapping, which is what
/// lets a verse clip hand over to a chorus clip.
fn validate_automation_spans(
    lanes: &[&AutomationLane],
    properties: &ProjectProperties,
    owner: &str,
    errors: &mut Vec<ValidationError>,
) {
    if lanes.len() < 2 {
        return;
    }
    let Ok(clock) = crate::timing::ProjectClock::new(properties) else {
        return;
    };
    let text = |range: Option<TimeRange>| match range {
        Some(range) => format!(
            "[{} - {}]",
            crate::serializer::format_bar_beat(&range.start),
            crate::serializer::format_bar_beat(&range.end)
        ),
        None => "over the whole timeline".to_string(),
    };
    for (index, lane) in lanes.iter().enumerate() {
        for other in &lanes[index + 1..] {
            if lane.param != other.param {
                continue;
            }
            let overlap = match (lane.time_range, other.time_range) {
                (None, _) | (_, None) => true,
                (Some(left), Some(right)) => {
                    let bounds = (
                        crate::automation::lane_quarter(&clock, lane, &left.start),
                        crate::automation::lane_quarter(&clock, lane, &left.end),
                        crate::automation::lane_quarter(&clock, other, &right.start),
                        crate::automation::lane_quarter(&clock, other, &right.end),
                    );
                    let (Ok(left_start), Ok(left_end), Ok(right_start), Ok(right_end)) = bounds
                    else {
                        // An unreadable position is already reported for its own lane.
                        continue;
                    };
                    left_start < right_end && right_start < left_end
                }
            };
            if overlap {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.automation.{}", lane.param),
                    value: format!(
                        "lanes {} and {} overlap; one target is driven by one lane at a time",
                        text(lane.time_range),
                        text(other.time_range)
                    ),
                });
            }
        }
    }
}

/// General MIDI control names a control lane may use on a built-in synth track.
/// They are the vocabulary MIDI import and export share, so a lane keeps its
/// meaning across a round trip; a name outside it is refused.
fn is_gm_control_name(normalized: &str) -> bool {
    matches!(
        normalized,
        "mod"
            | "modulation"
            | "vibrato"
            | "volume"
            | "vol"
            | "pan"
            | "expression"
            | "dynamics"
            | "sustain"
            | "sustainpedal"
            | "damper"
            | "damperpedal"
            | "halfpedal"
            | "resonance"
            | "brightness"
            | "timbre"
            | "pressure"
            | "aftertouch"
            | "channelpressure"
    )
}

/// The control names a built-in OpenSynth track can automate: the preset's own
/// control declarations, which the interpreter turns into patch parameters.
struct SynthControls {
    declared: HashSet<String>,
}

/// Control declarations a built-in synth track resolves, or None when the
/// track plays another sound source.
fn synth_controls(project: &Project, track: &Track) -> Option<SynthControls> {
    let params: &[(String, ParamValue)] = match track.preset.as_deref() {
        Some(name) => {
            let preset = project.presets.iter().find(|preset| preset.name == name)?;
            if preset.plugin.as_deref() != Some(oc_device_schema::synth::SYNTH_PLUGIN_ID) {
                return None;
            }
            &preset.params
        }
        None => {
            let instrument = crate::instruments::track_instrument(project, track)?;
            if crate::instruments::default_patch_plugin(instrument)
                != Some(oc_device_schema::synth::SYNTH_PLUGIN_ID)
            {
                return None;
            }
            return Some(SynthControls {
                declared: HashSet::new(),
            });
        }
    };
    let mut declared = HashSet::new();
    for (name, _) in params {
        let normalized = crate::automation::normalize_param_name(name);
        // `control.<lane>.param` and `control.<lane>.cc` map a lane name onto
        // something the synth does read.
        if let Some(control) = normalized
            .strip_prefix("control")
            .and_then(|rest| {
                rest.strip_suffix("param")
                    .or_else(|| rest.strip_suffix("cc14"))
                    .or_else(|| rest.strip_suffix("cc"))
            })
            .filter(|control| !control.is_empty())
        {
            declared.insert(control.to_string());
        }
    }
    Some(SynthControls { declared })
}

/// Why a control lane on a built-in synth track cannot sound.
fn synth_lane_reason(lane: &AutomationLane, synth: &SynthControls) -> Option<String> {
    let normalized = crate::automation::normalize_param_name(&lane.param);
    if matches!(normalized.as_str(), "pitchbend" | "bend") {
        return None;
    }
    if crate::automation::decode_plugin_automation_param(&lane.param).is_some() {
        return None;
    }
    if synth.declared.contains(&normalized) {
        return None;
    }
    if is_gm_control_name(&normalized) {
        return None;
    }
    const WORKS: &str = "the General MIDI controls (mod, volume, pan, expression, sustain, resonance, brightness, pressure), pitch_bend and patch parameters written as pluginparam_<hex>";
    Some(format!(
        "the built-in synth has no control named '{}', so this lane is silent; it reads {WORKS}",
        lane.param
    ))
}

/// `<effect>.<parameter>` lanes must name one declared effect of the owner and
/// a parameter that effect exposes.
fn validate_effect_lane(
    lane: &AutomationLane,
    effects: &[Effect],
    owner: &str,
    synth: Option<&SynthControls>,
    errors: &mut Vec<ValidationError>,
) {
    if synth.is_some() && oc_device_schema::synth::is_known_param(&lane.param) {
        errors.push(ValidationError::InvalidAutomationLane {
            owner: owner.to_string(),
            lane: lane.param.clone(),
            reason: format!(
                "the built-in synth patch parameter '{}' is not an effect or control lane; write it as {}",
                lane.param,
                crate::automation::encode_plugin_automation_param(&lane.param)
            ),
        });
        return;
    }
    let reason = match crate::automation::effect_lane(&lane.param) {
        Some((target, parameter))
            if is_lane_identifier(target) && is_lane_identifier(parameter) =>
        {
            match crate::automation::resolve_effect(effects, target) {
                Ok(index) => {
                    crate::automation::validate_device_param(&effects[index], parameter).err()
                }
                Err(reason) => Some(reason),
            }
        }
        _ => Some(
            "effect lanes are <effect>.<parameter> with identifier names, or fx<N>.<parameter>"
                .to_string(),
        ),
    };
    if let Some(reason) = reason {
        errors.push(ValidationError::InvalidAutomationLane {
            owner: owner.to_string(),
            lane: lane.param.clone(),
            reason,
        });
    }
}

fn is_lane_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_alphabetic() || first == b'_' || first == b'/' => {}
        _ => return false,
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'/')
}

fn validate_gain_automation(
    clips: &[GainAutomation],
    properties: &ProjectProperties,
    owner: &str,
    errors: &mut Vec<ValidationError>,
) {
    if clips.is_empty() {
        return;
    }
    let clock = match crate::timing::ProjectClock::new(properties) {
        Ok(clock) => clock,
        Err(value) => {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.gain_automation"),
                value,
            });
            return;
        }
    };
    let mut last_end = None;
    for clip in clips {
        if clip.ppq == 0 || clip.ppq > 96000 {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.gain_automation.ppq"),
                value: clip.ppq.to_string(),
            });
            continue;
        }
        let mut invalid_position = false;
        for position in std::iter::once(&clip.time_range.start)
            .chain(std::iter::once(&clip.time_range.end))
            .chain(clip.points.iter().map(|p| &p.position))
        {
            if let Err(value) = clock.quarter_notes_at_ppq(position, clip.ppq) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.gain_automation"),
                    value,
                });
                invalid_position = true;
            }
        }
        if invalid_position {
            continue;
        }
        let beat = |p: &BarBeat| {
            clock
                .quarter_notes_at_ppq(p, clip.ppq)
                .expect("checked gain position")
        };
        validate_time_range(&clip.time_range, owner, errors);
        let start = beat(&clip.time_range.start);
        let end = beat(&clip.time_range.end);
        if last_end.is_some_and(|previous| start < previous) || clip.points.is_empty() {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.gain_automation"),
                value: "overlapping/unsorted or empty clips".into(),
            });
        }
        last_end = Some(end);
        let mut previous = None;
        for p in &clip.points {
            validate_bar_beat(&p.position, owner, errors);
            let position = beat(&p.position);
            // A point on the range end is the last interpolated value, not an
            // out-of-range position, so it stays legal.
            if position < start || position > end || previous.is_some_and(|v| position <= v) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.gain_automation.points"),
                    value: "out of range or not strictly increasing".into(),
                });
            }
            previous = Some(position);
            match p.value {
                ParamValue::Number(v) => validate_finite_range(owner, v, 0.0, 16.0, errors),
                _ => errors.push(ValidationError::InvalidValue {
                    field: owner.into(),
                    value: "gain must be a linear numeric factor".into(),
                }),
            }
        }
    }
}

fn validate_bus(
    bus: &Bus,
    track_names: &HashSet<&str>,
    bus_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
    subject: SourceSubject,
) {
    if bus.name.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: "bus.name".to_string(),
            value: bus.name.clone(),
        });
    }
    validate_finite_range(
        &format!("bus '{}'.volume_db", bus.name),
        bus.volume_db,
        -120.0,
        24.0,
        errors,
    );
    for route in &bus.routes {
        if route != "master" && !bus_names.contains(route.as_str()) {
            errors.push(ValidationError::UnknownRoute {
                bus: bus.name.clone(),
                route: route.clone(),
            });
        }
    }
    for effect in &bus.effects {
        validate_effect(
            effect,
            EffectKeyOwner::Bus(bus.name.as_str()),
            track_names,
            bus_names,
            errors,
            emissions,
            subject,
        );
    }
}

struct RouteSources<'a> {
    track_names: &'a HashSet<&'a str>,
    bus_names: &'a HashSet<&'a str>,
    control_sources: &'a HashMap<&'a str, &'a ControlSource>,
}

/// Refuse a cycle over the union of audio routes and key edges. Bus-route cycles
/// keep their own message; a cycle that only closes through a key edge is
/// reported here, naming its members.
/// Validate the device-input routes of one owner's chain.
///
/// A control route must name a declared `@control_source` and a device that
/// declares a control port; an audio route must name a track (or, on a bus, a
/// bus) and a device with an audio key port. The control route is the one edge
/// kind that does not form a signal loop: control is evaluated before its
/// consumer, so only the feed track reading its own output is refused.
// The route collection plus its owner/attribution context is one validation unit.
#[allow(clippy::too_many_arguments)]
fn validate_input_routes(
    owner: &str,
    consumer_track: Option<&str>,
    effects: &[Effect],
    routes: &[InputRoute],
    sources: &RouteSources<'_>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
    subject: SourceSubject,
) {
    let start = errors.len();
    let RouteSources {
        track_names,
        bus_names,
        control_sources,
    } = *sources;
    let mut seen: HashMap<(RouteKind, String), ()> = HashMap::new();
    for route in routes {
        if seen
            .insert((route.kind, route.target.clone()), ())
            .is_some()
        {
            errors.push(ValidationError::DuplicateInputRoute {
                owner: owner.to_string(),
                target: route.target.clone(),
                kind: route.kind.as_str().to_string(),
            });
        }
        let effect = match crate::automation::resolve_effect(effects, &route.target) {
            Ok(index) => &effects[index],
            Err(reason) => {
                errors.push(ValidationError::InvalidRouteTarget {
                    owner: owner.to_string(),
                    target: route.target.clone(),
                    reason,
                });
                continue;
            }
        };
        let effect_name = effect.effect_type.lane_name().to_string();
        let device = effect
            .effect_type
            .device_id()
            .and_then(oc_device_schema::find_device);
        match route.kind {
            RouteKind::Audio => {
                if route.source == consumer_track.unwrap_or("") {
                    errors.push(ValidationError::SidechainSelfKey {
                        owner: owner.to_string(),
                        key: route.source.clone(),
                    });
                    continue;
                }
                let is_track = track_names.contains(route.source.as_str());
                let is_bus = bus_names.contains(route.source.as_str());
                if !is_track && !is_bus {
                    errors.push(ValidationError::UnknownRouteSource {
                        owner: owner.to_string(),
                        kind: "audio".into(),
                        name: route.source.clone(),
                    });
                    continue;
                }
                if consumer_track.is_some() && is_bus {
                    errors.push(ValidationError::SidechainKeyBusOnTrack {
                        owner: owner.to_string(),
                        key: route.source.clone(),
                    });
                    continue;
                }
                if let Some(device) = device
                    && !device
                        .ports
                        .iter()
                        .any(|port| port.kind == oc_device_schema::PortKind::Audio && !port.sums)
                {
                    errors.push(ValidationError::InvalidRouteTarget {
                        owner: owner.to_string(),
                        target: route.target.clone(),
                        reason: format!("{effect_name} has no audio key input"),
                    });
                }
            }
            RouteKind::Control => {
                let Some(source) = control_sources.get(route.source.as_str()) else {
                    errors.push(ValidationError::UnknownRouteSource {
                        owner: owner.to_string(),
                        kind: "control".into(),
                        name: route.source.clone(),
                    });
                    continue;
                };
                if let Some(track) = consumer_track
                    && source.source_track == track
                {
                    errors.push(ValidationError::ControlRouteCycle {
                        owner: owner.to_string(),
                        control: source.name.clone(),
                        reason: format!(
                            "control source '{}' reads track '{}', which is the track using it",
                            source.name, source.source_track
                        ),
                    });
                    continue;
                }
                if let Some(device) = device {
                    if !device
                        .ports
                        .iter()
                        .any(|port| port.kind == oc_device_schema::PortKind::Control)
                    {
                        let ports = device
                            .ports
                            .iter()
                            .map(|port| port.id)
                            .collect::<Vec<_>>()
                            .join(", ");
                        errors.push(ValidationError::ControlInputRefused {
                            owner: owner.to_string(),
                            effect: effect_name,
                            ports,
                        });
                    }
                } else {
                    errors.push(ValidationError::ControlInputRefused {
                        owner: owner.to_string(),
                        effect: effect_name,
                        ports: "plugin (no control input)".into(),
                    });
                }
            }
        }
    }
    emissions.attribute(start, errors.len(), subject);
}

/// Refuse a loop over control edges. A control edge is evaluated before its
/// consumer, so a feed track downstream of its own consumer would be a loop.
/// A track reading its own control input is reported per route, not here.
fn validate_control_route_cycles(
    project: &Project,
    control_sources: &HashMap<&str, &ControlSource>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let error_start = errors.len();
    let mut names: Vec<&str> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for track in &project.tracks {
        if !index.contains_key(track.name.as_str()) {
            index.insert(track.name.as_str(), names.len());
            names.push(track.name.as_str());
        }
    }
    let mut edges = vec![Vec::new(); names.len()];
    for track in &project.tracks {
        let Some(&consumer) = index.get(track.name.as_str()) else {
            continue;
        };
        for route in track
            .input_routes
            .iter()
            .filter(|route| route.kind == RouteKind::Control)
        {
            let Some(source) = control_sources.get(route.source.as_str()) else {
                continue;
            };
            let Some(&feed) = index.get(source.source_track.as_str()) else {
                continue;
            };
            if feed != consumer {
                edges[feed].push(consumer);
            }
        }
    }
    let cyclic = cyclic_nodes(&edges);
    let Some(start) = (0..cyclic.len()).find(|node| cyclic[*node]) else {
        return;
    };
    let Some(cycle) = cycle_through(start, &edges, &cyclic) else {
        return;
    };
    let labels: Vec<String> = cycle
        .iter()
        .map(|node| format!("track '{}'", names[*node]))
        .collect();
    errors.push(ValidationError::ControlRouteLoop {
        cycle: labels.join(" -> "),
    });
    emissions.attribute(
        error_start,
        errors.len(),
        SourceSubject {
            owner: SourceOwner::Project,
            part: SourcePart::Whole,
        },
    );
}

fn validate_signal_cycles(
    project: &Project,
    track_names: &HashSet<&str>,
    bus_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let start = errors.len();
    let mut nodes: Vec<&str> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for name in project
        .tracks
        .iter()
        .map(|track| track.name.as_str())
        .chain(project.buses.iter().map(|bus| bus.name.as_str()))
    {
        if !index.contains_key(name) {
            index.insert(name, nodes.len());
            nodes.push(name);
        }
    }
    let mut edges = vec![Vec::new(); nodes.len()];
    for track in &project.tracks {
        let Some(&from) = index.get(track.name.as_str()) else {
            continue;
        };
        let targets = track
            .mix
            .sends
            .iter()
            .map(|send| send.target.as_str())
            .chain(
                track
                    .mix
                    .output
                    .as_deref()
                    .filter(|output| *output != "master"),
            );
        for target in targets {
            if let Some(&to) = index.get(target) {
                edges[from].push(to);
            }
        }
    }
    for bus in &project.buses {
        let Some(&from) = index.get(bus.name.as_str()) else {
            continue;
        };
        for route in &bus.routes {
            if route != "master"
                && let Some(&to) = index.get(route.as_str())
            {
                edges[from].push(to);
            }
        }
    }
    let mut key_edges: Vec<(usize, usize)> = Vec::new();
    let mut add_key = |from: Option<usize>, to: Option<usize>| {
        if let (Some(from), Some(to)) = (from, to) {
            edges[from].push(to);
            key_edges.push((from, to));
        }
    };
    for track in &project.tracks {
        for effect in &track.effects {
            let Some(source) = &effect.sidechain_source else {
                continue;
            };
            if track_names.contains(source.as_str()) && !bus_names.contains(source.as_str()) {
                add_key(
                    index.get(source.as_str()).copied(),
                    index.get(track.name.as_str()).copied(),
                );
            }
        }
        for route in track
            .input_routes
            .iter()
            .filter(|route| route.kind == RouteKind::Audio)
        {
            if track_names.contains(route.source.as_str())
                && !bus_names.contains(route.source.as_str())
            {
                add_key(
                    index.get(route.source.as_str()).copied(),
                    index.get(track.name.as_str()).copied(),
                );
            }
        }
    }
    for bus in &project.buses {
        for effect in &bus.effects {
            let Some(source) = &effect.sidechain_source else {
                continue;
            };
            if source != &bus.name
                && (track_names.contains(source.as_str()) || bus_names.contains(source.as_str()))
            {
                add_key(
                    index.get(source.as_str()).copied(),
                    index.get(bus.name.as_str()).copied(),
                );
            }
        }
        for route in bus
            .input_routes
            .iter()
            .filter(|route| route.kind == RouteKind::Audio)
        {
            if route.source != bus.name
                && (track_names.contains(route.source.as_str())
                    || bus_names.contains(route.source.as_str()))
            {
                add_key(
                    index.get(route.source.as_str()).copied(),
                    index.get(bus.name.as_str()).copied(),
                );
            }
        }
    }
    let mut cyclic = cyclic_nodes(&edges);
    let mut reported: HashSet<Vec<usize>> = HashSet::new();
    while let Some(start) = (0..cyclic.len()).find(|node| cyclic[*node]) {
        let Some(cycle) = cycle_through(start, &edges, &cyclic) else {
            cyclic[start] = false;
            continue;
        };
        let mut members: Vec<usize> = cycle[..cycle.len() - 1].to_vec();
        members.sort_unstable();
        let through_key = cycle
            .windows(2)
            .any(|pair| key_edges.contains(&(pair[0], pair[1])));
        if through_key && reported.insert(members) {
            let labels: Vec<String> = cycle
                .iter()
                .map(|node| {
                    let name = nodes[*node];
                    if bus_names.contains(name) {
                        format!("bus '{name}'")
                    } else {
                        format!("track '{name}'")
                    }
                })
                .collect();
            errors.push(ValidationError::SidechainCycle {
                cycle: labels.join(" -> "),
            });
        }
        for node in cycle {
            cyclic[node] = false;
        }
    }
    emissions.attribute(
        start,
        errors.len(),
        SourceSubject {
            owner: SourceOwner::Project,
            part: SourcePart::Whole,
        },
    );
}

/// Nodes that survive peeling every zero-indegree node: each is on a cycle or
/// downstream of one.
fn cyclic_nodes(edges: &[Vec<usize>]) -> Vec<bool> {
    let mut indegree = vec![0_usize; edges.len()];
    for targets in edges {
        for &target in targets {
            indegree[target] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..edges.len())
        .filter(|index| indegree[*index] == 0)
        .collect();
    while let Some(node) = ready.pop() {
        for &next in &edges[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }
    indegree.into_iter().map(|degree| degree > 0).collect()
}

/// One cycle inside the residual graph, closed on its first node.
fn cycle_through(start: usize, edges: &[Vec<usize>], cyclic: &[bool]) -> Option<Vec<usize>> {
    let mut path = vec![start];
    let mut visited = vec![usize::MAX; edges.len()];
    visited[start] = 0;
    let mut node = start;
    loop {
        let next = *edges[node].iter().find(|next| cyclic[**next])?;
        if visited[next] != usize::MAX {
            let mut cycle = path[visited[next]..].to_vec();
            cycle.push(next);
            return Some(cycle);
        }
        visited[next] = path.len();
        path.push(next);
        node = next;
    }
}

fn validate_routing_cycles(
    buses: &[Bus],
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
) {
    let routes: HashMap<&str, Vec<&str>> = buses
        .iter()
        .map(|bus| {
            (
                bus.name.as_str(),
                bus.routes.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    for (index, bus) in buses.iter().enumerate() {
        let start = errors.len();
        let mut stack = Vec::new();
        let mut visiting = HashSet::new();
        if has_cycle(
            bus.name.as_str(),
            bus.name.as_str(),
            &routes,
            &mut stack,
            &mut visiting,
        ) {
            stack.push(bus.name.as_str());
            errors.push(ValidationError::RoutingCycle {
                cycle: stack.join(" -> "),
            });
        }
        emissions.attribute(
            start,
            errors.len(),
            SourceSubject {
                owner: SourceOwner::Bus(index),
                part: SourcePart::Whole,
            },
        );
    }
}

fn has_cycle<'a>(
    start: &'a str,
    current: &'a str,
    routes: &HashMap<&'a str, Vec<&'a str>>,
    stack: &mut Vec<&'a str>,
    visiting: &mut HashSet<&'a str>,
) -> bool {
    if !visiting.insert(current) {
        return false;
    }
    if let Some(next) = routes.get(current) {
        for route in next {
            if *route == start {
                stack.push(current);
                return true;
            }
            if routes.contains_key(route) && has_cycle(start, route, routes, stack, visiting) {
                stack.push(current);
                return true;
            }
        }
    }
    visiting.remove(current);
    false
}

fn validate_mix(track: &Track, bus_names: &HashSet<&str>, errors: &mut Vec<ValidationError>) {
    let mix = &track.mix;
    let name = &track.name;
    validate_finite_range(
        &format!("track '{}'.volume_db", name),
        mix.volume_db,
        -120.0,
        24.0,
        errors,
    );
    validate_finite_range(&format!("track '{}'.pan", name), mix.pan, -1.0, 1.0, errors);
    if let Some(output) = &mix.output
        && output != "master"
        && !bus_names.contains(output.as_str())
    {
        errors.push(ValidationError::UnknownBus {
            track: name.to_string(),
            bus: output.clone(),
        });
    }
    for send in &mix.sends {
        if !bus_names.contains(send.target.as_str()) {
            errors.push(ValidationError::UnknownBus {
                track: name.to_string(),
                bus: send.target.clone(),
            });
        }
        validate_finite_range(
            &format!("track '{}'.send '{}'.amount", name, send.target),
            send.amount,
            0.0,
            4.0,
            errors,
        );
        validate_finite_range(
            &format!("track '{}'.send '{}'.pan", name, send.target),
            send.pan,
            -1.0,
            1.0,
            errors,
        );
        if let Some(source) = &send.source {
            let hosts_plugin = source.owner == track.name
                || track.preset.as_deref() == Some(source.owner.as_str());
            if !hosts_plugin {
                errors.push(ValidationError::InvalidValue {
                    field: format!("track '{}'.send '{}'.source", name, send.target),
                    value: source.owner.clone(),
                });
            }
            if source.output < 2 {
                errors.push(ValidationError::InvalidValue {
                    field: format!("track '{}'.send '{}'.source", name, send.target),
                    value: format!("{}.out{}", source.owner, source.output),
                });
            }
        }
    }
    for (field, value) in [
        ("display_name", &mix.overrides.display_name),
        ("color", &mix.overrides.color),
    ] {
        if let Some(value) = value
            && (value.trim().is_empty() || value.len() > 64 || value.chars().any(char::is_control))
        {
            errors.push(ValidationError::InvalidValue {
                field: format!("track '{}'.{field}", name),
                value: value.clone(),
            });
        }
    }
}

/// The node whose effect chain carries a key, which fixes what may key it.
#[derive(Clone, Copy)]
enum EffectKeyOwner<'a> {
    /// A track chain reads only another track's post-insert buffer.
    Track(&'a str),
    /// A bus chain may read a track or another bus.
    Bus(&'a str),
    /// The master chain may read a track or a bus.
    Master,
    /// A preset chain takes its host at render time, so only existence is known.
    Preset(&'a str),
}

impl EffectKeyOwner<'_> {
    fn label(&self) -> String {
        match self {
            Self::Track(name) => format!("track '{name}'"),
            Self::Bus(name) => format!("bus '{name}'"),
            Self::Master => "master".to_string(),
            Self::Preset(name) => format!("preset '{name}'"),
        }
    }

    fn own_name(&self) -> Option<&str> {
        match *self {
            Self::Track(name) | Self::Bus(name) => Some(name),
            Self::Master | Self::Preset(_) => None,
        }
    }
}

fn validate_effect(
    effect: &Effect,
    owner: EffectKeyOwner<'_>,
    track_names: &HashSet<&str>,
    bus_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
    emissions: &mut Emissions,
    subject: SourceSubject,
) {
    let start = errors.len();
    let label = owner.label();
    if let EffectType::Plugin(plugin) = &effect.effect_type
        && plugin.trim().is_empty()
    {
        errors.push(ValidationError::InvalidEffectParam {
            effect: "plugin".to_string(),
            param: "id".to_string(),
        });
    }
    if effect.effect_type == EffectType::Gain {
        for (name, value) in &effect.params {
            match (name.as_str(), value.as_f64()) {
                ("factor", Some(value)) => {
                    validate_finite_range("gain.factor", value, 0.0, 16.0, errors)
                }
                _ => errors.push(ValidationError::InvalidEffectParam {
                    effect: "gain".into(),
                    param: name.clone(),
                }),
            }
        }
    }
    if effect.effect_type == EffectType::DynamicEq {
        validate_dynamic_eq(&effect.params, errors);
    }
    if let Some(source) = &effect.sidechain_source {
        validate_sidechain_key(source, owner, &label, track_names, bus_names, errors);
    }
    validate_params(&format!("{label}.effect"), &effect.params, errors);
    emissions.attribute(start, errors.len(), subject);
}

fn validate_sidechain_key(
    source: &str,
    owner: EffectKeyOwner<'_>,
    label: &str,
    track_names: &HashSet<&str>,
    bus_names: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
) {
    let is_track = track_names.contains(source);
    let is_bus = bus_names.contains(source);
    if owner.own_name() == Some(source) {
        errors.push(ValidationError::SidechainSelfKey {
            owner: label.to_string(),
            key: source.to_string(),
        });
        return;
    }
    if is_track && is_bus {
        errors.push(ValidationError::AmbiguousSidechainKey {
            owner: label.to_string(),
            key: source.to_string(),
        });
        return;
    }
    if let EffectKeyOwner::Preset(_) = owner {
        if !is_track && !is_bus {
            errors.push(ValidationError::UnknownSidechainKey {
                owner: label.to_string(),
                key: source.to_string(),
            });
        }
        return;
    }
    if let EffectKeyOwner::Track(_) = owner
        && is_bus
    {
        errors.push(ValidationError::SidechainKeyBusOnTrack {
            owner: label.to_string(),
            key: source.to_string(),
        });
        return;
    }
    if !is_track && !is_bus {
        errors.push(ValidationError::UnknownSidechainKey {
            owner: label.to_string(),
            key: source.to_string(),
        });
    }
}

enum DynamicEqField {
    Switch,
    BandType,
    Channel,
    Frequency,
    Q,
    Gain,
    Threshold,
    Range,
    Attack,
    Release,
}

fn dynamic_eq_field(name: &str) -> Option<DynamicEqField> {
    if let Some(rest) = name
        .strip_prefix("low_cut_")
        .or_else(|| name.strip_prefix("high_cut_"))
    {
        return match rest {
            "enabled" => Some(DynamicEqField::Switch),
            "freq" => Some(DynamicEqField::Frequency),
            "q" => Some(DynamicEqField::Q),
            _ => None,
        };
    }
    let rest = name.strip_prefix("band")?;
    let (index, field) = rest.split_once('_')?;
    let index: usize = index.parse().ok()?;
    if !(1..=6).contains(&index) {
        return None;
    }
    match field {
        "enabled" | "dynamic" | "solo" => Some(DynamicEqField::Switch),
        "type" => Some(DynamicEqField::BandType),
        "channel" => Some(DynamicEqField::Channel),
        "freq" => Some(DynamicEqField::Frequency),
        "q" => Some(DynamicEqField::Q),
        "gain" => Some(DynamicEqField::Gain),
        "threshold" => Some(DynamicEqField::Threshold),
        "range" => Some(DynamicEqField::Range),
        "attack" => Some(DynamicEqField::Attack),
        "release" => Some(DynamicEqField::Release),
        _ => None,
    }
}

fn dynamic_eq_band_kind(name: &str) -> bool {
    matches!(
        name,
        "bell"
            | "peak"
            | "peaking"
            | "low_shelf"
            | "lowshelf"
            | "high_shelf"
            | "highshelf"
            | "low_cut"
            | "lowcut"
            | "high_pass"
            | "band_pass"
            | "bandpass"
            | "high_cut"
            | "highcut"
            | "low_pass"
            | "notch"
    )
}

fn dynamic_eq_channel(name: &str) -> bool {
    matches!(name, "stereo" | "mid" | "side" | "left" | "right")
}

fn validate_dynamic_eq(params: &[(String, ParamValue)], errors: &mut Vec<ValidationError>) {
    for (name, value) in params {
        let Some(field) = dynamic_eq_field(name) else {
            errors.push(ValidationError::InvalidEffectParam {
                effect: "dynamic_eq".into(),
                param: name.clone(),
            });
            continue;
        };
        let field_name = format!("dynamic_eq.{name}");
        let invalid = |errors: &mut Vec<ValidationError>| {
            errors.push(ValidationError::InvalidEffectParam {
                effect: "dynamic_eq".into(),
                param: name.clone(),
            });
        };
        match field {
            DynamicEqField::Switch => {
                let valid = match value {
                    ParamValue::Bool(_) => true,
                    ParamValue::Number(number) => {
                        number.is_finite() && (*number == 0.0 || *number == 1.0)
                    }
                    _ => false,
                };
                if !valid {
                    invalid(errors);
                }
            }
            DynamicEqField::BandType => match value {
                ParamValue::String(kind) if dynamic_eq_band_kind(kind) => {}
                _ => invalid(errors),
            },
            DynamicEqField::Channel => match value {
                ParamValue::String(channel) if dynamic_eq_channel(channel) => {}
                ParamValue::Number(number) => {
                    if !(number.is_finite() && (0.0..=4.0).contains(number)) {
                        invalid(errors);
                    }
                }
                _ => invalid(errors),
            },
            DynamicEqField::Frequency => {
                if let Some(number) = unit_number(value, Unit::Frequency) {
                    validate_finite_range(&field_name, number, 20.0, 20_000.0, errors);
                } else {
                    invalid(errors);
                }
            }
            DynamicEqField::Q => {
                if let Some(number) = unit_number(value, Unit::Number) {
                    validate_finite_range(&field_name, number, 0.1, 20.0, errors);
                } else {
                    invalid(errors);
                }
            }
            DynamicEqField::Gain => {
                if let Some(number) = unit_number(value, Unit::Db) {
                    validate_finite_range(&field_name, number, -24.0, 24.0, errors);
                } else {
                    invalid(errors);
                }
            }
            DynamicEqField::Threshold => {
                if let Some(number) = unit_number(value, Unit::Db) {
                    validate_finite_range(&field_name, number, -60.0, 0.0, errors);
                } else {
                    invalid(errors);
                }
            }
            DynamicEqField::Range => {
                if let Some(number) = unit_number(value, Unit::Db) {
                    validate_finite_range(&field_name, number, -24.0, 24.0, errors);
                } else {
                    invalid(errors);
                }
            }
            DynamicEqField::Attack | DynamicEqField::Release => {
                if let Some(milliseconds) = unit_number(value, Unit::Milliseconds) {
                    let (min, max) = if matches!(field, DynamicEqField::Attack) {
                        (0.1, 200.0)
                    } else {
                        (5.0, 2000.0)
                    };
                    validate_finite_range(&field_name, milliseconds, min, max, errors);
                } else {
                    invalid(errors);
                }
            }
        }
    }
}

enum Unit {
    Frequency,
    Db,
    Number,
    Milliseconds,
}

fn unit_number(value: &ParamValue, unit: Unit) -> Option<f64> {
    match (value, unit) {
        (ParamValue::Number(number), _) => Some(*number),
        (ParamValue::Frequency(number), Unit::Frequency) => Some(*number),
        (ParamValue::Db(number), Unit::Db) => Some(*number),
        (ParamValue::Time(seconds), Unit::Milliseconds) => Some(seconds * 1000.0),
        _ => None,
    }
}

fn validate_pitch_octave(field: &str, pitch: &Pitch, errors: &mut Vec<ValidationError>) {
    if !(-1..=9).contains(&pitch.octave) {
        errors.push(ValidationError::InvalidValue {
            field: field.into(),
            value: format!("pitch octave {} must be -1..=9", pitch.octave),
        });
    } else if pitch.semitone_number() < 0 {
        errors.push(ValidationError::InvalidValue {
            field: field.into(),
            value: "pitch must not fall below C-1".into(),
        });
    }
}

fn validate_note_event(owner: &str, event: &NoteEvent, errors: &mut Vec<ValidationError>) {
    validate_pitch_octave(&format!("{owner}.note.pitch"), &event.pitch, errors);
    if let Err(value) = performance_gate_quarters(&event.modifiers) {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.note.gate_quarters"),
            value,
        });
    }
    validate_finite_range(
        &format!("{owner}.note.start_beat"),
        event.start_beat,
        0.0,
        f64::MAX,
        errors,
    );
    if event.duration.to_beats() <= 0.0 || !event.duration.to_beats().is_finite() {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.note.duration"),
            value: format!("{} beats", event.duration.to_beats()),
        });
    }
    if let Some(velocity) = event.velocity
        && (velocity == 0 || velocity > 127)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.note.velocity"),
            value: velocity.to_string(),
        });
    }
    if let Some(bend) = event.pitch_bend {
        validate_finite_range(
            &format!("{owner}.note.pitch_bend"),
            bend,
            -2400.0,
            2400.0,
            errors,
        );
    }
    for (key, value) in &event.modifiers {
        validate_param_value(&format!("{owner}.note.{key}"), value, errors);
    }
    if let Err(message) = crate::gliss::form(&event.modifiers) {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.note.{}", crate::gliss::MODIFIER),
            value: message,
        });
    }
    match crate::ornament::form(&event.modifiers) {
        Ok(Some(ornament)) => {
            if let Err(message) = crate::ornament::check_combinations(ornament, event) {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.note.{}", crate::ornament::MODIFIER),
                    value: message,
                });
            }
        }
        Ok(None) => {}
        Err(message) => errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.note.{}", crate::ornament::MODIFIER),
            value: message,
        }),
    }
}

fn validate_hit_event(owner: &str, hit: &HitEvent, errors: &mut Vec<ValidationError>) {
    if let Err(value) = performance_gate_quarters(&hit.modifiers) {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.hit.gate_quarters"),
            value,
        });
    }
    if hit.drum.trim().is_empty() {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.hit.drum"),
            value: hit.drum.clone(),
        });
    }
    if hit.position.beat == 0 {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.hit.beat"),
            value: "0".to_string(),
        });
    }
    if let Some(velocity) = hit.velocity
        && (velocity == 0 || velocity > 127)
    {
        errors.push(ValidationError::InvalidValue {
            field: format!("{owner}.hit.velocity"),
            value: format!("{velocity} is outside 1..=127"),
        });
    }
    let mut midi_fields = HashSet::new();
    for (key, value) in &hit.modifiers {
        if matches!(key.as_str(), "grid_offset_beats" | "timing_offset_beats") {
            let local = if key == "grid_offset_beats" {
                hit.anchor_quarters()
            } else {
                hit.written_quarters()
            };
            let valid = matches!(value, ParamValue::Number(number)
                if number.is_finite() && (-0.125..=0.125).contains(number) && local + number >= 0.0);
            if !midi_fields.insert(key) || !valid {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.hit.{key}"),
                    value: format!("{value:?}"),
                });
            }
        }
        if matches!(key.as_str(), "midi_note" | "velocity") {
            let minimum = if key == "velocity" { 1.0 } else { 0.0 };
            let valid = matches!(value, ParamValue::Number(number)
                if number.is_finite() && number.fract() == 0.0 && *number >= minimum && *number <= 127.0);
            if !midi_fields.insert(key) || !valid {
                errors.push(ValidationError::InvalidValue {
                    field: format!("{owner}.hit.{key}"),
                    value: format!("{value:?}"),
                });
            }
        }
        validate_param_value(&format!("{owner}.hit.{key}"), value, errors);
    }
}

fn validate_instrument_path(
    track: &str,
    instrument: &InstrumentPath,
    errors: &mut Vec<ValidationError>,
) {
    if instrument.category.trim().is_empty() || instrument.name.trim().is_empty() {
        errors.push(ValidationError::UnknownInstrument {
            track: track.to_string(),
            path: instrument.as_path(),
        });
    }
}

fn validate_time_signature(
    numerator: u8,
    denominator: u8,
    field: &str,
    errors: &mut Vec<ValidationError>,
) {
    if numerator == 0 || numerator > 32 {
        errors.push(ValidationError::InvalidValue {
            field: format!("{field}.numerator"),
            value: numerator.to_string(),
        });
    }
    if !matches!(denominator, 1 | 2 | 4 | 8 | 16 | 32 | 64) {
        errors.push(ValidationError::InvalidValue {
            field: format!("{field}.denominator"),
            value: denominator.to_string(),
        });
    }
}

fn validate_time_range(range: &TimeRange, owner: &str, errors: &mut Vec<ValidationError>) {
    validate_bar_beat(&range.start, &format!("{owner}.start"), errors);
    validate_bar_beat(&range.end, &format!("{owner}.end"), errors);
    if bar_beat_order(&range.end) <= bar_beat_order(&range.start) {
        errors.push(ValidationError::InvalidTimeRange {
            owner: owner.to_string(),
        });
    }
}

fn validate_bar_beat(position: &BarBeat, field: &str, errors: &mut Vec<ValidationError>) {
    if position.bar == 0 || position.beat == 0 {
        errors.push(ValidationError::InvalidValue {
            field: field.to_string(),
            value: format!(
                "{}:{}:{}",
                position.bar, position.beat, position.subdivision
            ),
        });
    }
}

fn validate_params(
    owner: &str,
    params: &[(String, ParamValue)],
    errors: &mut Vec<ValidationError>,
) {
    let mut names = HashSet::new();
    for (key, value) in params {
        if key.trim().is_empty() || !names.insert(key.as_str()) {
            errors.push(ValidationError::InvalidValue {
                field: format!("{owner}.param"),
                value: key.clone(),
            });
        }
        validate_param_value(&format!("{owner}.{key}"), value, errors);
    }
}

fn validate_param_value(field: &str, value: &ParamValue, errors: &mut Vec<ValidationError>) {
    if let Some(value) = value.as_f64()
        && !value.is_finite()
    {
        errors.push(ValidationError::InvalidValue {
            field: field.to_string(),
            value: value.to_string(),
        });
    }
}

fn validate_finite_range(
    field: &str,
    value: f64,
    min: f64,
    max: f64,
    errors: &mut Vec<ValidationError>,
) {
    if !value.is_finite() || value < min || value > max {
        errors.push(ValidationError::OutOfRange {
            field: field.to_string(),
            value,
            min,
            max,
        });
    }
}

fn bar_beat_order(position: &BarBeat) -> u64 {
    position.bar as u64 * 1_000_000 + position.beat as u64 * 1_000 + position.subdivision as u64
}

#[cfg(test)]
mod tests {
    #[test]
    fn root_reference_inline_pattern_does_not_borrow_a_root_or_track_subject() {
        let mut project = crate::parse("@project\n@pattern p | preset: absent, instrument: synth/lead\n  events:\n    @0 C4 q\n@track t\n").unwrap();
        let pattern = project.patterns.pop().unwrap();
        project.tracks[0].inline_patterns.push(pattern);
        let diagnostics = validate_diagnostics(&project);
        let reference = diagnostics
            .iter()
            .find(|diagnostic| matches!(diagnostic.error, ValidationError::UnknownPreset { .. }))
            .unwrap();
        assert_eq!(
            reference.subject,
            crate::diagnostics::DiagnosticSubject::UnknownSubject
        );
        let legacy = validate(&project).unwrap_err();
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| format!("{:?}", diagnostic.error))
                .collect::<Vec<_>>(),
            legacy
                .iter()
                .map(|error| format!("{error:?}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn root_reference_audio_programmatic_mismatch_keeps_exact_owner_and_legacy_order() {
        let mut project = crate::parse("@project\n@audio_clip same [1:1 - 2:1] | track: b\n  file: \"a.wav\"\n@audio_clip same [1:1 - 2:1] | track: a\n  file: \"b.wav\"\n@track a\n@track b\n").unwrap();
        for clip in project.audio_clips_mut() {
            clip.track = "absent".into();
        }
        let diagnostics = validate_diagnostics(&project);
        let legacy = validate(&project).unwrap_err();
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| format!("{:?}", diagnostic.error))
                .collect::<Vec<_>>(),
            legacy
                .iter()
                .map(|error| format!("{error:?}"))
                .collect::<Vec<_>>()
        );
        let subjects: Vec<_> = diagnostics
            .iter()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.error,
                    ValidationError::UnknownAudioClipTrack { .. }
                )
            })
            .map(|diagnostic| diagnostic.subject)
            .collect();
        assert_eq!(
            subjects,
            (0..2)
                .map(
                    |index| crate::diagnostics::DiagnosticSubject::Located(SourceSubject {
                        owner: SourceOwner::AudioClip(index),
                        part: SourcePart::Reference {
                            kind: ReferenceKind::Track,
                            occurrence: 0
                        }
                    })
                )
                .collect::<Vec<_>>()
        );
    }

    use super::*;

    #[test]
    fn the_project_export_rate_must_sit_inside_the_sampler_range() {
        for (rate, accepted) in [
            (8_000u32, true),
            (44_100, true),
            (192_000, true),
            (0, false),
            (7_999, false),
            (192_001, false),
        ] {
            let project =
                crate::parse(&format!("@project\n  export: {{sample_rate: {rate}}}\n")).unwrap();
            let result = validate(&project);
            assert_eq!(result.is_ok(), accepted, "{rate}: {result:?}");
            if !accepted {
                let messages = result
                    .unwrap_err()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                assert!(
                    messages
                        .iter()
                        .any(|message| message.contains("export.sample_rate")),
                    "{rate}: {messages:?}"
                );
            }
        }
    }

    #[test]
    fn mp3_export_bitrate_and_vbr_quality_are_validated_at_the_boundary() {
        for bitrate in [8u32, 192, 320] {
            let project = crate::parse(&format!(
                "@project\n  export: {{container: mp3, mp3: {{bitrate: {bitrate}}}}}\n"
            ))
            .unwrap();
            assert!(validate(&project).is_ok(), "{bitrate}: {project:?}");
        }
        for bitrate in ["0", "321", "9999"] {
            let project = crate::parse(&format!(
                "@project\n  export: {{container: mp3, mp3: {{bitrate: {bitrate}}}}}\n"
            ))
            .unwrap();
            let messages = validate(&project)
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("export.mp3.bitrate")),
                "{bitrate}: {messages:?}"
            );
        }

        let project =
            crate::parse("@project\n  export: {container: mp3, mp3: {vbr_quality: 10}}\n").unwrap();
        let messages = validate(&project)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("export.mp3.vbr_quality")),
            "{messages:?}"
        );
    }

    #[test]
    fn flac_export_depth_and_level_are_validated_at_the_boundary() {
        for level in [0u8, 5, 8] {
            let project = crate::parse(&format!(
                "@project\n  export: {{flac: {{level: {level}}}}}\n"
            ))
            .unwrap();
            assert!(validate(&project).is_ok(), "{level}: {project:?}");
        }
        for level in ["9", "999"] {
            let project = crate::parse(&format!(
                "@project\n  export: {{flac: {{level: {level}}}}}\n"
            ))
            .unwrap();
            let messages = validate(&project)
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("export.flac.level")),
                "{level}: {messages:?}"
            );
        }

        let project = crate::parse("@project\n  export: {flac: {depth: f32}}\n").unwrap();
        let messages = validate(&project)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("export.flac.depth")),
            "{messages:?}"
        );
    }

    #[test]
    fn audio_clip_pitch_validates_the_semitone_range() {
        let source = "@project \"Pitch\"\n  tempo: 120\n@audio_clip hit [1:1 - 3:1] | track: tape\n  file: \"hit.wav\"\n@track tape | instrument: audio/clip\n";
        for (value, accepted) in [
            (24.0, true),
            (-24.0, true),
            (0.25, true),
            (24.01, false),
            (-24.01, false),
            (f64::NAN, false),
            (f64::INFINITY, false),
        ] {
            let mut project = crate::parse(source).unwrap();
            project.audio_clip_mut(0).unwrap().pitch = value;
            let result = validate(&project);
            assert_eq!(result.is_ok(), accepted, "{value}: {result:?}");
            if !accepted {
                let messages = result
                    .unwrap_err()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                assert!(
                    messages.iter().any(|message| message.contains("pitch")),
                    "{value}: {messages:?}"
                );
            }
        }
    }

    #[test]
    fn audio_clip_gain_envelope_must_span_the_whole_clip() {
        let with_envelope = |envelope: &str| {
            format!(
                "@project \"Envelope\"\n  tempo: 120\n@audio_clip hit [1:1 - 3:1] | track: tape\n  file: \"hit.wav\"\n  gain_envelope: {envelope}\n@track tape | instrument: audio/clip\n"
            )
        };
        let accepted = ["0:0dB, 1:0dB", "0:0dB, 0.5:-6dB, 1:0dB"];
        for envelope in accepted {
            let project = crate::parse(&with_envelope(envelope)).unwrap();
            assert!(validate(&project).is_ok(), "{envelope}: {project:?}");
        }
        let refused = [
            "0:0dB",
            "0:0dB, 0.4:-6dB",
            "0.5:-6dB, 1:0dB",
            "0:0dB, 1.5:-6dB",
            "0:0dB, 0.5:-6dB, 0.5:0dB, 1:0dB",
            "0:0dB, 0.75:0dB, 0.25:-6dB, 1:0dB",
        ];
        for envelope in refused {
            let project = crate::parse(&with_envelope(envelope)).unwrap();
            let messages = validate(&project)
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("gain_envelope")),
                "{envelope}: {messages:?}"
            );
        }
        let mut empty = crate::parse(&with_envelope("0:0dB, 1:0dB")).unwrap();
        empty.audio_clip_mut(0).unwrap().gain_envelope.clear();
        assert!(validate(&empty).is_ok());
    }

    #[test]
    fn dynamic_eq_validates_numeric_ranges_and_param_names() {
        let valid = crate::parse(
            "@project\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - dynamic_eq: band1_enabled=true, band1_type=bell, band1_freq=1kHz, band1_q=1, band1_gain=+3dB, band1_dynamic=true, band1_threshold=-30dB, band1_range=-6dB, band1_attack=10ms, band1_release=120ms, low_cut_enabled=true, low_cut_freq=30Hz, low_cut_q=0.707, high_cut_enabled=true, high_cut_freq=18kHz\n",
        )
        .unwrap();
        assert!(validate(&valid).is_ok());

        for param in [
            "band1_freq=19Hz",
            "band1_freq=21kHz",
            "band1_q=0.05",
            "band1_q=21",
            "band1_gain=25dB",
            "band1_gain=-25dB",
            "band1_threshold=1dB",
            "band1_threshold=-61dB",
            "band1_range=25dB",
            "band1_attack=0.05ms",
            "band1_attack=201ms",
            "band1_release=4ms",
            "band1_release=2001ms",
            "band1_type=wobble",
            "band1_enabled=2",
            "band1_wobble=1",
        ] {
            let project = crate::parse(&format!(
                "@project\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - dynamic_eq: {param}\n"
            ))
            .unwrap();
            assert!(validate(&project).is_err(), "accepted {param}");
        }
    }

    #[test]
    fn drum_microtiming_rejects_invalid_domains_and_pre_origin_hits() {
        for value in ["0.126", "-0.01", "true", "50%"] {
            let p = crate::parse(&format!("@project\n@track drums | instrument: drums/acoustic_kit\n  @pattern main [1:1 - 2:1]\n    kick 1:0 mf timing_offset_beats={value}\n")).unwrap();
            assert!(validate(&p).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn drum_grid_offsets_reject_invalid_domains_and_duplicates() {
        for modifiers in [
            "grid_offset_beats=-0.01",
            "grid_offset_beats=0.126",
            "grid_offset_beats=50%",
            "grid_offset_beats=0.1 grid_offset_beats=0.02",
            "grid_offset_beats=0.01 timing_offset_beats=-0.02",
        ] {
            let source = format!(
                "@project\n@track drums | instrument: drums/acoustic_kit\n  @pattern main [1:1 - 2:1]\n    kick 1:0 mf {modifiers}\n"
            );
            let project = crate::parse(&source).unwrap();
            assert!(validate(&project).is_err(), "accepted {modifiers}");
        }
    }

    fn pedal_phrase(marks: &str) -> Vec<ValidationError> {
        let source = format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 3:1]\n    C4 h mf\n    E4 h mf\n{marks}"
        );
        validate(&crate::parse(&source).unwrap()).expect_err("the pedal should be refused")
    }

    fn pedal_message(errors: &[ValidationError]) -> String {
        errors
            .iter()
            .map(|error| error.to_string())
            .find(|message| message.contains("pedal"))
            .unwrap_or_else(|| panic!("no pedal refusal in {errors:?}"))
    }

    #[test]
    fn the_performance_refusal_names_the_pedal_form() {
        let project = crate::parse(
            "@project\n@track piano | instrument: piano/acoustic_grand\n  performance: { @0.0 ch 1 cc 64 127; }\n  @phrase p [1:1 - 3:1]\n    C4 h mf\n",
        )
        .unwrap();
        let errors = validate(&project).unwrap_err();
        let message = errors
            .iter()
            .map(|error| error.to_string())
            .find(|message| message.contains("performance commands"))
            .expect("the performance refusal fires");
        assert!(message.contains("`@2.0 pedal down`"), "{message}");
    }

    #[test]
    fn pedal_up_before_down_is_refused() {
        let message = pedal_message(&pedal_phrase("    @0 pedal up\n"));
        assert!(message.contains("pedal up at beat 0"), "{message}");
        assert!(message.contains("without a pedal down"), "{message}");
    }

    #[test]
    fn a_second_pedal_down_while_down_is_refused() {
        let message = pedal_message(&pedal_phrase(
            "    @0 pedal down\n    @2 pedal down\n    @3 pedal up\n",
        ));
        assert!(message.contains("pedal down at beat 2"), "{message}");
        assert!(message.contains("already down since beat 0"), "{message}");
    }

    #[test]
    fn a_pedal_past_the_phrase_range_is_refused() {
        let message = pedal_message(&pedal_phrase("    @0 pedal down\n    @9 pedal up\n"));
        assert!(message.contains("past the end of the"), "{message}");
        assert!(message.contains("phrase"), "{message}");
        assert!(message.contains("8 beats"), "{message}");
    }

    #[test]
    fn a_well_formed_pedal_validates() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 3:1]\n    C4 h mf\n    @1 pedal down\n    E4 h mf\n    @3 pedal up\n    rest h\n";
        let project = crate::parse(source).unwrap();
        assert!(validate(&project).is_ok());
    }

    #[test]
    fn phrase_bounds_follow_denominator_and_meter_map() {
        let mut project = crate::parse("@project\n  time_signature: 3/2\n  time_sig_map:\n    2:1 = 7/8\n@track a | instrument: piano/acoustic_grand\n  @phrase main [1:1 - 3:1]\n    @8 C4 q mf\n").unwrap();
        assert!(validate(&project).is_ok()); // 6 + 3.5 quarters, not 3 + 3.
        project.tracks[0].phrases[0].events[0].start_beat = 9.0;
        assert!(
            validate(&project)
                .unwrap_err()
                .iter()
                .any(|e| matches!(e, ValidationError::TimeRangeOutOfBounds { .. }))
        );
    }

    #[test]
    fn validates_explicit_drum_midi_fields_without_clamping() {
        for (key, value) in [
            ("midi_note", -1.0),
            ("midi_note", 128.0),
            ("midi_note", 35.5),
            ("velocity", 0.0),
            ("velocity", 128.0),
            ("velocity", f64::NAN),
        ] {
            let hit = HitEvent {
                drum: "kick".into(),
                position: BeatPosition {
                    beat: 1,
                    subdivision: 0,
                },
                dynamics: None,
                velocity: None,
                articulation: None,
                modifiers: vec![(key.into(), ParamValue::Number(value))],
            };
            let mut errors = Vec::new();
            validate_hit_event("test", &hit, &mut errors);
            assert!(!errors.is_empty(), "accepted {key}={value}");
        }
        let mut hit = HitEvent {
            drum: "kick".into(),
            position: BeatPosition {
                beat: 1,
                subdivision: 0,
            },
            dynamics: None,
            velocity: None,
            articulation: None,
            modifiers: vec![
                ("midi_note".into(), ParamValue::Number(0.0)),
                ("velocity".into(), ParamValue::Number(127.0)),
            ],
        };
        let mut errors = Vec::new();
        validate_hit_event("test", &hit, &mut errors);
        assert!(errors.is_empty());
        hit.modifiers
            .push(("velocity".into(), ParamValue::Number(20.0)));
        validate_hit_event("test", &hit, &mut errors);
        assert!(!errors.is_empty());
    }

    fn empty_project() -> Project {
        Project {
            name: None,
            ocp_version: None,
            properties: ProjectProperties::default(),
            presets: Vec::new(),
            tracks: Vec::new(),
            patterns: Vec::new(),
            buses: Vec::new(),
            master: None,
            sections: Vec::new(),
            markers: Vec::new(),
            groups: Vec::new(),
        }
    }

    #[test]
    fn accepts_minimal_project() {
        assert!(validate(&empty_project()).is_ok());
    }

    #[test]
    fn reports_duplicate_tracks_and_unknown_references() {
        let mut project = empty_project();
        project.tracks.push(Track {
            name: "piano".to_string(),
            instrument: InstrumentPath::new("piano", "acoustic_grand"),
            preset: Some("missing".to_string()),
            engine: None,
            mix: MixState {
                sends: vec![Send {
                    target: "verb".to_string(),
                    amount: 0.5,
                    source: None,
                    pre_fader: false,
                    pan: 0.0,
                }],
                ..MixState::default()
            },
            effects: Vec::new(),
            phrases: Vec::new(),
            patterns: Vec::new(),
            inline_patterns: Vec::new(),
            clips: {
                let mut pool = ClipPool::new();
                pool.push_pattern(Clip {
                    pattern: "missing_pattern".to_string(),
                    time_range: TimeRange {
                        start: BarBeat::new(1, 1, 0),
                        end: BarBeat::new(2, 1, 0),
                    },
                    offset_quarters: 0.0,
                    loop_quarters: 0.0,
                    id: None,
                });
                pool
            },
            gain_automation: Vec::new(),
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        });
        project.tracks.push(project.tracks[0].clone());

        let messages = validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(messages.contains("Track 'piano' is defined multiple times"));
        assert!(messages.contains("Unknown preset reference: 'missing'"));
        assert!(messages.contains("Unknown bus reference: 'verb'"));
        assert!(messages.contains("Unknown pattern reference: 'missing_pattern'"));
    }

    #[test]
    fn reports_invalid_ranges_and_values() {
        let mut project = empty_project();
        project.properties.tempo = f64::NAN;
        project.properties.time_signature.denominator = 3;
        project.sections.push(Section {
            name: "bad".to_string(),
            time_range: TimeRange {
                start: BarBeat::new(2, 1, 0),
                end: BarBeat::new(1, 1, 0),
            },
            repeat: Some(0),
            label: None,
            flags: None,
        });

        let messages = validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(messages.contains("Invalid value for tempo"));
        assert!(messages.contains("time_signature.denominator"));
        assert!(messages.contains("Invalid time range"));
        assert!(messages.contains("section 'bad'.repeat"));
    }

    #[test]
    fn pan_out_of_range_names_the_value_and_the_legal_range() {
        let project =
            crate::parse("@project\n@track t | instrument: piano/acoustic_grand\n  mix: pan=18\n")
                .unwrap();
        let messages = validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            messages.contains("track 't'.pan: 18 (valid range -1..=1)"),
            "{messages}"
        );
    }

    #[test]
    fn accepts_master_and_bus_targets_but_rejects_routing_cycles() {
        let mut project = empty_project();
        project.buses = vec![
            Bus {
                gain_automation: Vec::new(),
                automation: Vec::new(),
                name: "room".to_string(),
                routes: vec!["glue".to_string()],
                effects: vec![],
                volume_db: 0.0,
                mute: false,
                solo: false,
                input_routes: vec![],
            },
            Bus {
                gain_automation: Vec::new(),
                automation: Vec::new(),
                name: "glue".to_string(),
                routes: vec!["master".to_string()],
                effects: vec![],
                volume_db: 0.0,
                mute: false,
                solo: false,
                input_routes: vec![],
            },
        ];
        assert!(validate(&project).is_ok());

        project.buses[1].routes = vec!["room".to_string()];
        let messages = validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(messages.contains("Bus routing cycle"));
    }

    fn messages(text: &str) -> String {
        let project = crate::parse(text).unwrap();
        validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn effect_lane_naming_an_unknown_effect_is_rejected() {
        let text = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: verb.mix {1:1:0=1}\n",
        );
        assert!(
            text.contains("Invalid automation lane 'verb.mix'"),
            "{text}"
        );
        assert!(text.contains("unknown effect 'verb'"), "{text}");
        assert!(text.contains("the chain has: gain"), "{text}");
    }

    #[test]
    fn effect_lane_naming_an_unknown_parameter_is_rejected() {
        let text = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.mix {1:1:0=1}\n",
        );
        assert!(
            text.contains("Invalid automation lane 'gain.mix'"),
            "{text}"
        );
        assert!(
            text.contains("effect 'gain' has no parameter 'mix'"),
            "{text}"
        );
        assert!(text.contains("expected one of: factor"), "{text}");
    }

    #[test]
    fn effect_lane_needs_a_unique_type_or_a_chain_position() {
        let ambiguous = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n    - gain: factor=0.5\n  automation: gain.factor {1:1:0=1}\n",
        );
        assert!(ambiguous.contains("matches 2 chain entries"), "{ambiguous}");
        assert!(ambiguous.contains("fx1..fx2"), "{ambiguous}");

        let out_of_range = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: fx2.factor {1:1:0=1}\n",
        );
        assert!(
            out_of_range.contains("effect index 2 is out of range"),
            "{out_of_range}"
        );
    }

    #[test]
    fn valid_effect_lanes_pass_on_track_bus_master_and_plugin_effects() {
        let project = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n    - plugin:\"vst3.test.fake\"\n  automation: gain.factor {1:1:0=1}\n  automation: fx2.Cutoff {1:1:0=0.5}\n  automation: plugin.Mix {1:1:0=0.5}\n@bus b\n  routes: [master]\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=0.5}\n@master\n  effects:\n    - limiter: ceiling=-1dB\n  automation: limiter.ceiling {1:1:0=-3dB}\n",
        )
        .unwrap();
        assert!(validate(&project).is_ok());
    }

    #[test]
    fn tension_needs_the_smooth_curve_and_stays_in_range() {
        let mismatched = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1 tension=0.5}\n",
        );
        assert!(
            mismatched.contains("tension needs curve=smooth"),
            "{mismatched}"
        );

        let out_of_range = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1 curve=smooth tension=1.5}\n",
        );
        assert!(out_of_range.contains("tension"), "{out_of_range}");
    }

    #[test]
    fn a_builtin_synth_lane_that_can_change_nothing_is_refused_with_what_works() {
        let unknown = messages(
            "@project\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  automation: filter_cutoff {1:1=20%, 3:1=80%}\n",
        );
        assert!(
            unknown.contains("no control named 'filter_cutoff'"),
            "{unknown}"
        );
        assert!(unknown.contains("volume"), "{unknown}");
        assert!(
            unknown.contains("pluginparam_<hex>"),
            "the refusal names the working form: {unknown}"
        );

        let patch = messages(
            "@project\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  automation: filter.cutoff {1:1=20%, 3:1=80%}\n",
        );
        assert!(
            patch.contains("pluginparam_66696c7465722e6375746f6666"),
            "the refusal shows the patch parameter's lane name: {patch}"
        );

        for lane in [
            "volume",
            "pan",
            "expression",
            "modulation",
            "sustain",
            // General MIDI names outlive the track's instrument, so a project
            // keeps them across a MIDI export and import.
            "brightness",
            "resonance",
            "pressure",
            "pitch_bend",
            "pluginparam_66696c7465722e6375746f6666",
        ] {
            let project = crate::parse(&format!(
                "@project\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  automation: {lane} {{1:1=0.2, 3:1=0.8}}\n"
            ))
            .unwrap();
            assert!(
                validate(&project).is_ok(),
                "{lane} is a lane the synth reads"
            );
        }

        // A preset can declare a readable lane name for a patch parameter.
        let declared = crate::parse(
            "@project\n  tempo: 120\n@preset lead\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {control_cutoff_param: \"filter.cutoff\"}\n@track lead | instrument: synth/lead_square, preset: lead\n  automation: cutoff {1:1=0.2, 3:1=0.8}\n",
        )
        .unwrap();
        assert!(validate(&declared).is_ok());

        // A SoundFont track keeps accepting a controller lane the synth ignores.
        let soundfont = crate::parse(
            "@project\n  tempo: 120\n@preset grand\n  source: soundfont\n  program: 0\n@track keys | instrument: piano/acoustic_grand, preset: grand\n  automation: brightness {1:1=20%, 3:1=80%}\n",
        )
        .unwrap();
        assert!(validate(&soundfont).is_ok());
    }

    #[test]
    fn curves_and_smoothing_need_an_effect_parameter_lane() {
        let smoothing = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  automation: modulation smoothing=25ms {1:1:0=1}\n",
        );
        assert!(
            smoothing.contains("curves and smoothing apply to effect-parameter lanes"),
            "{smoothing}"
        );

        let curve = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  automation: modulation {1:1:0=1 curve=half_sine}\n",
        );
        assert!(
            curve.contains("curves and smoothing apply to effect-parameter lanes"),
            "{curve}"
        );

        let linear = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  automation: modulation {1:1:0=1, 2:1:0=0.5}\n",
        )
        .unwrap();
        assert!(validate(&linear).is_ok());
    }

    #[test]
    fn smoothing_time_is_bounded() {
        let text = messages(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor smoothing=120s {1:1:0=1}\n",
        );
        assert!(text.contains("smoothing"), "{text}");
    }

    /// One target carries clips that hand over at an edge, and never two lanes
    /// that write at once. A lane without a span owns the whole timeline.
    #[test]
    fn clips_on_one_target_must_not_overlap() {
        let header = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n";
        let check = |lanes: &str| {
            let project = crate::parse(&format!("{header}{lanes}")).unwrap();
            validate(&project)
        };
        for accepted in [
            // Adjacent clips share an edge.
            "  automation: gain.factor [1:1 - 3:1] {1:1=1}\n  automation: gain.factor [3:1 - 5:1] {3:1=0.5}\n",
            // Muted lanes are still valid documents.
            "  automation: gain.factor [1:1 - 3:1] muted {1:1=1}\n",
            // Different targets never collide.
            "  automation: gain.factor [1:1 - 3:1] {1:1=1}\n",
        ] {
            let result = check(accepted);
            assert!(result.is_ok(), "{accepted}: {result:?}");
        }
        for (rejected, named) in [
            (
                "  automation: gain.factor [1:1 - 3:1] {1:1=1}\n  automation: gain.factor [2:1 - 4:1] {2:1=0.5}\n",
                "lanes [1:1 - 3:1] and [2:1 - 4:1] overlap",
            ),
            (
                "  automation: gain.factor [1:1 - 5:1] {1:1=1}\n  automation: gain.factor [2:1 - 3:1] {2:1=0.5}\n",
                "lanes [1:1 - 5:1] and [2:1 - 3:1] overlap",
            ),
            (
                "  automation: gain.factor {1:1=1}\n  automation: gain.factor [2:1 - 3:1] {2:1=0.5}\n",
                "lanes over the whole timeline and [2:1 - 3:1] overlap",
            ),
            // A mute hides the lane but does not excuse the overlap.
            (
                "  automation: gain.factor [1:1 - 3:1] muted {1:1=1}\n  automation: gain.factor [2:1 - 4:1] {2:1=0.5}\n",
                "lanes [1:1 - 3:1] and [2:1 - 4:1] overlap",
            ),
            // A muted lane's span is still a real span.
            (
                "  automation: gain.factor [3:1 - 1:1] muted {1:1=1}\n",
                "end must be after start",
            ),
        ] {
            let result = check(rejected);
            assert!(result.is_err(), "{rejected} should be refused");
            let text = result
                .unwrap_err()
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(named), "{named}: {text}");
        }
    }

    #[test]
    fn bus_lane_parameter_comes_from_its_own_chain() {
        let text = messages(
            "@project\n  tempo: 120\n@bus b\n  routes: [master]\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1}\n@master\n  effects:\n    - gain: factor=1\n  automation: gain.threshold {1:1:0=1}\n",
        );
        assert!(
            text.contains("Invalid automation lane 'gain.threshold'"),
            "{text}"
        );
        assert!(text.contains("has no parameter 'threshold'"), "{text}");
    }

    fn vocal_header() -> &'static str {
        "@project\n  tempo: 120\n  key: A major\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n"
    }

    #[test]
    fn vocal_tune_references_a_real_clip() {
        let text = messages(&format!(
            "{}{}@track vocal | instrument: audio/clip\n",
            vocal_header(),
            "@vocal_tune t | clip: missing\n  amount: 0%\n"
        ));
        assert!(
            text.contains("Unknown audio clip reference: 'missing'"),
            "{text}"
        );

        let text = messages(&format!(
            "{}{}@track vocal | instrument: audio/clip\n",
            vocal_header(),
            "@vocal_tune t | track: ghost\n  amount: 0%\n"
        ));
        assert!(text.contains("Unknown track reference: 'ghost'"), "{text}");
    }

    #[test]
    fn vocal_tune_needs_exactly_one_selector() {
        let none = messages(&format!(
            "{}@vocal_tune t\n  amount: 0%\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(none.contains("needs clip: or track:"), "{none}");

        let both = messages(&format!(
            "{}@vocal_tune t | clip: lead, track: vocal\n  amount: 0%\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(both.contains("mutually exclusive"), "{both}");
    }

    #[test]
    fn one_clip_gets_one_tune_block() {
        let text = messages(&format!(
            "{}{}{}@track vocal | instrument: audio/clip\n",
            vocal_header(),
            "@vocal_tune first | clip: lead\n  amount: 0%\n",
            "@vocal_tune second | clip: lead\n  amount: 0%\n"
        ));
        assert!(text.contains("already has a vocal tune"), "{text}");
    }

    #[test]
    fn vocal_tune_ranges_are_bounded() {
        let amount = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 120%\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(amount.contains("vocal tune 't'.amount"), "{amount}");

        let cents = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  notes:\n    - at=0s tgt=20000ct\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(cents.contains("vocal tune 't'.note[0].tgt"), "{cents}");

        let retune = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  retune: 2s\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(retune.contains("vocal tune 't'.retune"), "{retune}");
    }

    #[test]
    fn vocal_timing_fields_are_bounded_statically() {
        let start = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  notes:\n    - at=0s start=150ms\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(start.contains("vocal tune 't'.note[0].start"), "{start}");
        assert!(start.contains("max_move"), "{start}");

        let under_cap = format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  max_move: 200ms\n  notes:\n    - at=0s start=150ms\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        );
        let project = crate::parse(&under_cap).unwrap();
        validate(&project).unwrap();

        let length = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  notes:\n    - at=0s length=0.1\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(length.contains("vocal tune 't'.note[0].length"), "{length}");

        let cap = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  max_move: 2s\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(cap.contains("vocal tune 't'.max_move"), "{cap}");
    }

    #[test]
    fn vocal_snap_needs_a_real_other_clip() {
        let unknown = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  snap: reference\n  reference: ghost\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(
            unknown.contains("Unknown reference clip: 'ghost'"),
            "{unknown}"
        );

        let own = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  snap: reference\n  reference: lead\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(own.contains("cannot reference itself"), "{own}");

        let no_reference = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  snap: reference\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(
            no_reference.contains("snap: reference needs reference:"),
            "{no_reference}"
        );

        let both = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  snap: scale\n  reference: guide\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(both.contains("snap: scale keeps the scale"), "{both}");
    }

    #[test]
    fn vocal_snap_ranges_are_bounded() {
        for (field, value) in [("cap", "-5ct"), ("window", "250ct")] {
            let text = messages(&format!(
                "{}@vocal_tune t | clip: lead\n  {field}: {value}\n@track vocal | instrument: audio/clip\n",
                vocal_header()
            ));
            assert!(text.contains(&format!("vocal tune 't'.{field}")), "{text}");
        }
    }

    #[test]
    fn vocal_tune_anchors_must_be_strictly_ascending() {
        let text = messages(&format!(
            "{}@vocal_tune t | clip: lead\n  amount: 0%\n  notes:\n    - at=1s tgt=A3\n    - at=0.5s tgt=A4\n@track vocal | instrument: audio/clip\n",
            vocal_header()
        ));
        assert!(
            text.contains("anchors must be strictly ascending"),
            "{text}"
        );
    }

    #[test]
    fn degree_targets_need_a_scale_and_a_track_needs_one_clip() {
        let no_scale = messages(
            "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  amount: 0%\n  notes:\n    - at=0s tgt=5@4\n@track vocal | instrument: audio/clip\n",
        );
        assert!(
            no_scale.contains("a degree target needs a scale"),
            "{no_scale}"
        );

        let two_clips = messages(
            "@project\n  tempo: 120\n  key: A major\n@audio_clip one [1:1 - 2:1] | track: vocal\n  file: \"one.wav\"\n@audio_clip two [2:1 - 3:1] | track: vocal\n  file: \"two.wav\"\n@vocal_tune t | track: vocal\n  amount: 0%\n@track vocal | instrument: audio/clip\n",
        );
        assert!(
            two_clips.contains("must own exactly one audio clip"),
            "{two_clips}"
        );
    }

    #[test]
    fn a_track_effect_key_must_name_a_track() {
        let text = messages(
            "@project\n@bus drums\n  routes: [master]\n@track bass | instrument: bass/electric_bass\n  effects:\n    - compression: sidechain=drums, ratio=4:1\n",
        );
        assert!(text.contains("bass") && text.contains("drums"), "{text}");
        assert!(text.contains("is a bus"), "{text}");

        let unknown = messages(
            "@project\n@track bass | instrument: bass/electric_bass\n  effects:\n    - compression: sidechain=ghost\n",
        );
        assert!(
            unknown.contains("Unknown sidechain key 'ghost'"),
            "{unknown}"
        );

        let ok = crate::parse(
            "@project\n@track kick | instrument: drums/acoustic_kit\n@track bass | instrument: bass/electric_bass\n  effects:\n    - compression: sidechain=kick\n",
        )
        .unwrap();
        assert!(validate(&ok).is_ok());
    }

    #[test]
    fn key_cycles_and_self_keys_are_rejected_with_their_members() {
        let mutual = messages(
            "@project\n@bus a\n  routes: [master]\n  effects:\n    - compression: sidechain=b\n@bus b\n  routes: [master]\n  effects:\n    - compression: sidechain=a\n",
        );
        assert!(mutual.contains("Sidechain key cycle"), "{mutual}");
        assert!(mutual.contains("'a'") && mutual.contains("'b'"), "{mutual}");

        let union = messages(
            "@project\n@bus a\n  routes: [b]\n  effects:\n    - compression: sidechain=b\n@bus b\n  routes: [master]\n",
        );
        assert!(union.contains("Sidechain key cycle"), "{union}");
        assert!(
            union.contains("bus 'a'") && union.contains("bus 'b'"),
            "{union}"
        );

        let self_key = messages(
            "@project\n@bus a\n  routes: [master]\n  effects:\n    - compression: sidechain=a\n",
        );
        assert!(self_key.contains("names its own chain"), "{self_key}");

        let track_self = messages(
            "@project\n@track a | instrument: keys/piano\n  effects:\n    - compression: sidechain=a\n",
        );
        assert!(track_self.contains("names its own chain"), "{track_self}");
    }
}
