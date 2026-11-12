//! Interpreter: AST → timed MIDI events
//!
//! Walks the AST and produces a flat list of timestamped MIDI events
//! that any backend engine can consume.

use oc_dsl::ast::*;
use std::collections::HashMap;

use crate::render_control::InstrumentProfile;

mod articulation;
mod automation;
mod gliss;
mod mapping;
mod params;
mod pattern;
mod section;

#[cfg(test)]
mod tests;

use self::articulation::*;
use self::automation::*;
use self::gliss::*;
use self::mapping::*;
use self::params::*;
use self::pattern::*;

pub(crate) use self::pattern::{
    pattern_occurrences, placed_pedal_marks, shifted_pattern_onset, swing_offset_quarters,
    transpose_pattern_note,
};

pub use self::mapping::{drum_to_gm_note, instrument_to_gm_program};
pub use self::params::{
    beats_to_samples, decode_plugin_automation_param, lane_param_value_is_physical,
    lane_param_value_to_normalized, param_value_to_normalized,
};

#[derive(Debug, Clone, Copy)]
struct SustainPoint {
    time_samples: u64,
    value: f64,
}

pub use oc_synth::{MidiEvent, MidiMessage};

/// Settings for interpretation
#[derive(Debug, Clone)]
pub struct InterpretSettings {
    pub sample_rate: u32,
    pub default_velocity: u8,
    /// Project MPE zones; `None` keeps the legacy channel assignment.
    pub mpe: Option<MpeConfig>,
    /// Pitch-bend range in semitones for `bend=` and bend automation.
    pub pitch_bend_range: f64,
}

impl Default for InterpretSettings {
    fn default() -> Self {
        Self {
            sample_rate: 44100,
            default_velocity: 80,
            mpe: None,
            pitch_bend_range: 2.0,
        }
    }
}

/// Trusted-AST compatibility wrapper. For user/model input use `try_interpret`
/// so invalid timing is returned as an error instead of a panic.
pub fn interpret(project: &Project, settings: &InterpretSettings) -> Vec<MidiEvent> {
    try_interpret(project, settings)
        .expect("invalid project timing; use try_interpret for fallible input")
}

/// Checked interpretation: errors are propagated before any audio is allocated.
pub fn try_interpret(
    project: &Project,
    settings: &InterpretSettings,
) -> Result<Vec<MidiEvent>, String> {
    try_interpret_at(project, settings, None)
}

/// Interpret profile resources relative to the containing project directory.
pub fn try_interpret_at(
    project: &Project,
    settings: &InterpretSettings,
    resource_root: Option<&std::path::Path>,
) -> Result<Vec<MidiEvent>, String> {
    try_interpret_with_setup(project, settings, resource_root, TrackSetup::GeneralMidi)
}

/// Interpret score events for rendering, resolving profiles at the project directory.
/// Native VST3 and sample-backed sources omit implicit GM/mixer setup; explicit
/// program declarations and authored controllers remain strict events.
pub fn try_interpret_render_at(
    project: &Project,
    settings: &InterpretSettings,
    resource_root: Option<&std::path::Path>,
) -> Result<Vec<MidiEvent>, String> {
    try_interpret_with_setup(project, settings, resource_root, TrackSetup::NativeRender)
}
#[derive(Clone, Copy)]
enum TrackSetup {
    GeneralMidi,
    NativeRender,
}
fn try_interpret_with_setup(
    project: &Project,
    settings: &InterpretSettings,
    resource_root: Option<&std::path::Path>,
    setup: TrackSetup,
) -> Result<Vec<MidiEvent>, String> {
    project
        .properties
        .checked_automation_normalization()
        .map_err(|error| error.to_string())?;
    if settings.sample_rate == 0 {
        return Err("sample rate must be positive".into());
    }
    let mut settings = settings.clone();
    settings.mpe = project.properties.mpe;
    settings.pitch_bend_range = settings
        .mpe
        .map_or(2.0, |config| config.bend_range_semitones());
    let settings = &settings;
    let mut events = Vec::new();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let project_swing = project.properties.swing;
    let section_flags = section::table(project, &clock)?;

    for (track_idx, track) in project.tracks.iter().enumerate() {
        if track.performance.is_some() {
            return Err("performance timeline playback is not yet qualified; refusing silent or doubled rendering".into());
        }
        let channel = track.channel.unwrap_or_else(|| {
            if track.instrument.category == "drums" {
                9 // GM drum channel (MIDI channel 10)
            } else {
                track_idx.min(15) as u8
            }
        });

        // Resolve GM program: preset takes priority over instrument path
        let gm_program = resolve_gm_program(track, &project.presets);
        let profile = InstrumentProfile::from_track(
            &project.presets,
            track,
            settings.sample_rate,
            resource_root,
        );
        let mut articulation_occurrences = HashMap::new();
        let sustain_timeline = sustain_timeline_for_track(track, &clock, settings, &profile)?;
        let gliss_context = GlissContext {
            track: &track.name,
            fixed_pitch: fixed_pitch_source(track, &project.presets),
            clock: &clock,
            key: project.properties.key,
            profile: &profile,
        };
        let curved = curve_pitches(track, project);

        let preset = track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|p| p.name == name));
        let native_score = matches!(setup, TrackSetup::NativeRender)
            && preset.is_some_and(|p| {
                p.source.as_deref() == Some("vst3")
                    || (p.source.as_deref() == Some("sample")
                        && p.plugin
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case("sfizz")))
            });
        // Export retains the GM initialization contract. Native score playback
        // keeps explicit program selection but has no implicit GM fallback.
        if !native_score || preset.is_some_and(|p| p.program.is_some()) {
            // Program change at time 0
            events.push(MidiEvent {
                time_samples: 0,
                event: MidiMessage::ProgramChange {
                    channel,
                    program: gm_program,
                },
                track_index: track_idx,
            });
        }
        if !native_score {
            // Set volume from mix
            let volume_cc = db_to_midi_volume(track.mix.volume_db);
            events.push(MidiEvent {
                time_samples: 0,
                event: MidiMessage::ControlChange {
                    channel,
                    controller: 7, // CC7 = Channel Volume
                    value: volume_cc,
                },
                track_index: track_idx,
            });

            // Set pan from mix
            let pan_cc = pan_to_midi(track.mix.pan);
            events.push(MidiEvent {
                time_samples: 0,
                event: MidiMessage::ControlChange {
                    channel,
                    controller: 10, // CC10 = Pan
                    value: pan_cc,
                },
                track_index: track_idx,
            });
        }

        // Process phrases
        let hairpin_cc11 = oc_dsl::dynamics::track_uses_expression_cc11(project, track);
        for phrase in &track.phrases {
            interpret_phrase(
                phrase,
                phrase.channel.unwrap_or(channel),
                track_idx,
                &clock,
                settings,
                &profile,
                &sustain_timeline,
                &mut articulation_occurrences,
                &gliss_context,
                &curved,
                project_swing,
                project.properties.swing_subdivision,
                hairpin_cc11,
                &section_flags,
                &mut events,
            )?;
        }

        // Process patterns (drums)
        for pattern in &track.patterns {
            interpret_pattern(
                pattern,
                channel,
                track_idx,
                &track.name,
                &clock,
                settings,
                &profile,
                project_swing,
                project.properties.swing_subdivision,
                &mut events,
            )?;
        }

        // Process clips (named pattern references)
        for clip in track.pattern_clips() {
            if let Some(pat_def) = project.patterns.iter().find(|p| p.name == clip.pattern) {
                let clip_start = clock.quarter_notes_at(&clip.time_range.start)?;
                let clip_end = clock.quarter_notes_at(&clip.time_range.end)?;
                interpret_pattern_def(
                    pat_def,
                    channel,
                    track_idx,
                    &track.name,
                    &clock,
                    settings,
                    &profile,
                    &sustain_timeline,
                    &mut articulation_occurrences,
                    &gliss_context,
                    &curved,
                    clip_start,
                    clip.offset_quarters,
                    clip.loop_quarters,
                    Some(clip_end),
                    pat_def.swing.or(project_swing),
                    project.properties.swing_subdivision,
                    &section_flags,
                    &mut events,
                )?;
            }
        }

        // Process inline patterns
        for inline in &track.inline_patterns {
            interpret_pattern_def(
                inline,
                channel,
                track_idx,
                &track.name,
                &clock,
                settings,
                &profile,
                &sustain_timeline,
                &mut articulation_occurrences,
                &gliss_context,
                &curved,
                0.0,
                0.0,
                0.0,
                None,
                inline.swing.or(project_swing),
                project.properties.swing_subdivision,
                &section_flags,
                &mut events,
            )?;
        }

        // Stable timestamp sorting preserves insertion order for ties. Apply
        // track automation after ALL musical containers, matching the historical
        // phrase path. Otherwise the same notes moved into clips see a different
        // CC/parameter state at coincident note boundaries. Keep per-note
        // expression/articulation ordering intact; do not globally sort by type.
        let normalization = crate::automation::patch_normalization(project, track);
        let synth_lanes = SynthLanes {
            builtin: normalization.builtin,
            recalibrate_legacy: normalization.recalibrate_legacy,
        };
        interpret_automation(
            track,
            AutomationContext {
                channel,
                track_index: track_idx,
                clock: &clock,
                settings,
                profile: &profile,
                synth_lanes,
            },
            &mut events,
        )?;
    }

    // Sort all events by time
    events.sort_by_key(|e| e.time_samples);

    // Construction order differs from playback order across phrases/tracks.
    // Rebind temporary event-index tokens once, in actual onset order.
    let mut voice_tokens = HashMap::new();
    let mut next_token = 0u64;
    for e in &events {
        if let MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::On { token, .. }) =
            &e.event
        {
            next_token += 1;
            voice_tokens.insert(*token, next_token);
        }
    }
    for e in &mut events {
        if let MidiMessage::NativeVoice(event) = &mut e.event {
            let token = match event {
                oc_synth::synth::NativeVoiceEvent::On { token, .. }
                | oc_synth::synth::NativeVoiceEvent::Off { token }
                | oc_synth::synth::NativeVoiceEvent::Pitch { token, .. } => token,
            };
            *token = *voice_tokens
                .get(token)
                .ok_or("native curve has no matching onset")?;
        }
    }

    // The drum-event dump is a tracing aid, not a diagnostic: a normal render
    // stays silent and `OCP_TRACE_INTERPRET=1` prints the count and the first
    // 20 events.
    if interpret_trace_enabled()
        && let Some(dump) = drum_events_dump(&events, project)
    {
        eprintln!("{dump}");
    }

    // Curve events need native per-note expression, never silent GM fallback.
    let curve_tracks: std::collections::HashSet<_> = events
        .iter()
        .filter_map(|e| matches!(&e.event, MidiMessage::NativeVoice(_)).then_some(e.track_index))
        .collect();
    for ti in &curve_tracks {
        let native = project.tracks[*ti]
            .preset
            .as_ref()
            .and_then(|name| project.presets.iter().find(|p| &p.name == name))
            .is_some_and(|p| p.plugin.as_deref() == Some("builtin.open_synth"));
        if !native {
            return Err("pitch_curve currently requires builtin.open_synth".into());
        }
    }
    let mut active = std::collections::HashSet::new();
    let identified_pitches: std::collections::HashSet<_> = events
        .iter()
        .filter_map(|e| {
            if let MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::On {
                channel,
                note,
                ..
            }) = e.event
            {
                Some((e.track_index, channel, note))
            } else {
                None
            }
        })
        .collect();
    // Scored notes at a curved pitch carry a voice id. The plain notes left
    // here are keyswitches, triggers or drum hits, which pitch alone cannot name.
    for e in events
        .iter()
        .filter(|e| curve_tracks.contains(&e.track_index))
    {
        match e.event {
            MidiMessage::NoteOn { channel, note, .. } => {
                if identified_pitches.contains(&(e.track_index, channel, note)) {
                    return Err("pitch_curve track: a plain note at a curve's pitch has no voice id (keyswitch, trigger or drum hit)".into());
                }
                if !active.insert((e.track_index, channel, note)) {
                    return Err("pitch_curve track has overlapping plain same-pitch notes; unique voice IDs required".into());
                }
            }
            MidiMessage::NoteOff { channel, note } => {
                active.remove(&(e.track_index, channel, note));
            }
            _ => {}
        }
    }
    Ok(events)
}

/// True when a caller asked for the interpreter's event dump.
fn interpret_trace_enabled() -> bool {
    std::env::var_os("OCP_TRACE_INTERPRET").is_some_and(|value| value != "0")
}

/// The drum-track event dump, or `None` when nothing played on a drum track.
///
/// The header carries the event count and the body stops after 20 events, so
/// the dump describes a track without burying the rest of the output.
fn drum_events_dump(events: &[MidiEvent], project: &Project) -> Option<String> {
    let track_index = project
        .tracks
        .iter()
        .position(|track| track.instrument.category == "drums")?;
    let drum_events: Vec<_> = events
        .iter()
        .filter(|event| event.track_index == track_index)
        .collect();
    if drum_events.is_empty() {
        return None;
    }
    let mut dump = format!("[interpret] drum track events: {}", drum_events.len());
    for event in drum_events.iter().take(20) {
        dump.push_str(&format!(
            "\n  t={} track={} {:?}",
            event.time_samples, event.track_index, event.event
        ));
    }
    Some(dump)
}

/// Pitches a pitch curve sounds on across the track's containers. A scored note
/// at one is identified too, so its note-off cannot release the curve voice.
fn curve_pitches(track: &Track, project: &Project) -> std::collections::HashSet<u8> {
    let mut pitches = std::collections::HashSet::new();
    let mut collect = |events: &[NoteEvent], transpose: Option<i8>| {
        for note in events {
            if note.muted || note.modifiers.iter().any(|(key, _)| key == "rest") {
                continue;
            }
            let curved = note.modifiers.iter().any(|(key, _)| key == "pitch_curve")
                || matches!(
                    oc_dsl::gliss::form(&note.modifiers),
                    Ok(Some(mark)) if !matches!(mark, oc_dsl::gliss::Gliss::Run(_))
                );
            if curved {
                pitches.insert(transpose_pattern_note(note.pitch.to_midi(), transpose));
            }
        }
    };
    for phrase in &track.phrases {
        collect(&phrase.events, None);
    }
    for pattern in &track.inline_patterns {
        collect(&pattern.events, pattern.transpose);
    }
    for clip in track.pattern_clips() {
        if let Some(pattern) = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
        {
            collect(&pattern.events, pattern.transpose);
        }
    }
    pitches
}
