//! Project → SMF export.

use std::collections::{BTreeMap, BTreeSet};

use midly::{Format, MetaMessage, MidiMessage, PitchBend, TrackEvent, TrackEventKind};
use oc_dsl::ast::*;
use oc_dsl::timing::ProjectClock;

use crate::interpret::{
    pattern_occurrences, placed_pedal_marks, shifted_pattern_onset, swing_offset_quarters,
};

use super::{
    ChannelAssignment, ExportOptions, ExportReport, ExportResult, ExportScope, LossItem, LossKind,
    MidiError, TrackReport,
};
use super::{gm, smf};

const RANK_META: u8 = 0;
const RANK_PROGRAM: u8 = 1;
const RANK_CONTROL: u8 = 2;
const RANK_NOTE_OFF: u8 = 3;
const RANK_SUSTAIN: u8 = 4;
const RANK_NOTE_ON: u8 = 5;

/// Export a project as an SMF.
pub fn export_smf(project: &Project, options: &ExportOptions) -> Result<ExportResult, MidiError> {
    if !matches!(options.format, 0 | 1) {
        return Err(MidiError::InvalidExport {
            message: format!("format {} is not 0 or 1", options.format),
        });
    }
    if options.ppq == 0 || options.ppq > 32767 {
        return Err(MidiError::InvalidExport {
            message: format!("PPQ {} is outside 1..=32767", options.ppq),
        });
    }
    // The renderer expands sections and repeats before it interprets, so the
    // export walks the same rendered timeline. An identity arrangement returns
    // the authored project unchanged.
    let expanded =
        oc_dsl::arrangement::expand(project).map_err(|message| MidiError::InvalidExport {
            message: format!("arrangement: {message}"),
        })?;
    let project = &expanded;
    let clock =
        ProjectClock::new(&project.properties).map_err(|message| MidiError::InvalidExport {
            message: format!("project clock: {message}"),
        })?;
    let selected = selected_tracks(project, &options.scope)?;
    let channels = assign_channels(project, &selected, &options.channels)?;
    let mut losses = LossBook::default();
    let mut payloads = Vec::with_capacity(selected.len());
    let mut reports = Vec::with_capacity(selected.len());

    for &index in selected.iter() {
        let track = &project.tracks[index];
        let channel = channels[&index];
        let mut events = Vec::new();
        let mut sequence = 0_usize;
        let program = track_program(track);
        if let Some(program) = program {
            events.push(PayloadEvent::new(
                0,
                RANK_PROGRAM,
                &mut sequence,
                channel,
                EventKind::Program(program),
            ));
        }
        let bend_range = bend_range(track);
        if (bend_range - 2.0).abs() > f64::EPSILON
            && track
                .automation()
                .any(|lane| gm::is_pitch_bend_lane(&lane.param))
        {
            push_rpn_range(&mut events, &mut sequence, channel, bend_range);
        }

        let TrackMusic {
            notes,
            pedals,
            expression,
        } = collect_music(project, index, &clock, options.ppq, &mut losses)?;
        for note in &notes {
            events.push(PayloadEvent::new(
                note.tick,
                RANK_NOTE_ON,
                &mut sequence,
                channel,
                EventKind::NoteOn {
                    key: note.key,
                    velocity: note.velocity,
                },
            ));
            events.push(PayloadEvent::new(
                note.tick + note.duration,
                RANK_NOTE_OFF,
                &mut sequence,
                channel,
                EventKind::NoteOff { key: note.key },
            ));
        }
        // Track automation has final say when it shares a tick with a written
        // pedal mark, matching the renderer's insertion order.
        for &(tick, down) in &pedals {
            events.push(PayloadEvent::new(
                tick,
                RANK_SUSTAIN,
                &mut sequence,
                channel,
                EventKind::Cc {
                    controller: 64,
                    value: if down { 127 } else { 0 },
                },
            ));
        }
        for &(tick, value) in &expression {
            events.push(PayloadEvent::new(
                tick,
                RANK_CONTROL,
                &mut sequence,
                channel,
                EventKind::Cc {
                    controller: 11,
                    value,
                },
            ));
        }
        let automation = pedals.len()
            + expression.len()
            + collect_automation(
                track,
                channel,
                &clock,
                options.ppq,
                bend_range,
                &mut EventSink {
                    events: &mut events,
                    sequence: &mut sequence,
                    losses: &mut losses,
                },
            )?;
        let name = track.name.clone();
        payloads.push(TrackPayload {
            name: name.clone(),
            events,
        });
        reports.push(TrackReport {
            track: index,
            name,
            channel,
            notes: notes.len(),
            widened_notes: notes.iter().filter(|note| note.widened).count(),
            automation_events: automation,
            program,
            smf_track: 0,
        });
    }

    collect_selection_losses(project, &selected, &mut losses);
    collect_unplaced_pattern_losses(project, &mut losses);

    let format = if options.format == 0 {
        Format::SingleTrack
    } else {
        Format::Parallel
    };
    let header = smf::header(format, options.ppq)?;
    let mut events = Vec::new();
    let mut sequence = 0_usize;
    push_clock_events(project, &clock, options.ppq, &mut events, &mut sequence)?;
    let mut names = Vec::with_capacity(payloads.len() + 1);
    let mut tracks = Vec::with_capacity(payloads.len() + 1);

    if options.format == 1 {
        names.push(
            project
                .name
                .clone()
                .unwrap_or_else(|| "Conductor".to_string())
                .into_bytes(),
        );
        for payload in &payloads {
            names.push(payload.name.as_bytes().to_vec());
        }
        tracks.push(encode_track(events, &names[0])?);
        for (index, payload) in payloads.iter().enumerate() {
            reports[index].smf_track = index + 1;
            tracks.push(encode_track(payload.events.clone(), &names[index + 1])?);
        }
    } else {
        names.push(
            project
                .name
                .clone()
                .unwrap_or_else(|| "OpenCompose".to_string())
                .into_bytes(),
        );
        for payload in &payloads {
            for event in &payload.events {
                events.push(PayloadEvent::new(
                    event.tick,
                    event.rank,
                    &mut sequence,
                    event.channel,
                    event.kind.clone(),
                ));
            }
        }
        tracks.push(encode_track(events, &names[0])?);
    }

    let midi = midly::Smf { header, tracks };
    let bytes = smf::write(&midi)?;
    Ok(ExportResult {
        bytes,
        report: ExportReport {
            format: options.format,
            ppq: options.ppq,
            written: reports,
            losses: losses.into_items(),
        },
    })
}

// ============================================================================
// Selection and channels
// ============================================================================

fn selected_tracks(project: &Project, scope: &ExportScope) -> Result<Vec<usize>, MidiError> {
    let count = project.tracks.len();
    let selected = match scope {
        ExportScope::Project => (0..count).collect::<Vec<_>>(),
        ExportScope::Tracks { start, end } => {
            if start >= end {
                return Err(MidiError::InvalidExport {
                    message: format!("empty track range {start}..{end}"),
                });
            }
            if *end > count {
                return Err(MidiError::InvalidExport {
                    message: format!("track range {start}..{end} exceeds {count} tracks"),
                });
            }
            (*start..*end).collect()
        }
        ExportScope::Track(index) => {
            if *index >= count {
                return Err(MidiError::InvalidExport {
                    message: format!("track {index} does not exist"),
                });
            }
            vec![*index]
        }
    };
    Ok(selected)
}

fn assign_channels(
    project: &Project,
    selected: &[usize],
    overrides: &[ChannelAssignment],
) -> Result<BTreeMap<usize, u8>, MidiError> {
    let mut assigned = BTreeMap::new();
    for assignment in overrides {
        if assignment.track >= project.tracks.len() {
            return Err(MidiError::InvalidExport {
                message: format!("channel override for missing track {}", assignment.track),
            });
        }
        if !(1..=16).contains(&assignment.channel) {
            return Err(MidiError::InvalidExport {
                message: format!("channel {} is outside 1..=16", assignment.channel),
            });
        }
        assigned.insert(assignment.track, assignment.channel);
    }
    let mut melodic = 0_usize;
    for &index in selected {
        if assigned.contains_key(&index) {
            continue;
        }
        let channel = if gm::is_drum_instrument(&project.tracks[index].instrument) {
            10
        } else {
            let channel = melodic_channel(melodic);
            melodic += 1;
            channel
        };
        assigned.insert(index, channel);
    }
    Ok(assigned)
}

fn melodic_channel(index: usize) -> u8 {
    let offset = index % 15;
    if offset < 9 {
        offset as u8 + 1
    } else {
        offset as u8 + 2
    }
}

fn track_program(track: &Track) -> Option<u8> {
    if let Some(value) = track
        .engine_params
        .iter()
        .flatten()
        .find(|(key, _)| key == "midi_program")
        .and_then(|(_, value)| value.as_f64())
    {
        return Some(value.round().clamp(0.0, 127.0) as u8);
    }
    // Playback selects a default kit, but exporting an implicit program change
    // would reset the recipient's kit. Only an authored drum program is emitted.
    if gm::is_drum_instrument(&track.instrument) {
        None
    } else {
        gm::instrument_gm_program(&track.instrument)
    }
}

fn bend_range(track: &Track) -> f64 {
    track
        .engine_params
        .iter()
        .flatten()
        .find(|(key, _)| key == "midi_bend_range")
        .and_then(|(_, value)| value.as_f64())
        .filter(|range| range.is_finite() && *range > 0.0)
        .unwrap_or(2.0)
}

// ============================================================================
// Notes
// ============================================================================

struct NoteOut {
    tick: u64,
    duration: u64,
    key: u8,
    velocity: u8,
    widened: bool,
}

#[derive(Default)]
struct TrackMusic {
    notes: Vec<NoteOut>,
    pedals: Vec<(u64, bool)>,
    /// CC11 values from a hosted phrase hairpin, already in ticks.
    expression: Vec<(u64, u8)>,
}

/// Count the content of named patterns no clip places. Such a pattern is never
/// exported, so its notes and hits are a clip-window loss like any other.
fn collect_unplaced_pattern_losses(project: &Project, losses: &mut LossBook) {
    let placed: BTreeSet<&str> = project
        .tracks
        .iter()
        .flat_map(|track| track.pattern_clips())
        .map(|clip| clip.pattern.as_str())
        .collect();
    for pattern in &project.patterns {
        if placed.contains(pattern.name.as_str()) {
            continue;
        }
        let notes = pattern
            .events
            .iter()
            .filter(|note| !is_rest(note) && !note.muted)
            .count();
        if notes > 0 {
            losses.add(
                LossKind::ClipWindow,
                notes,
                format!("notes in unplaced pattern '{}'", pattern.name),
            );
        }
        if !pattern.hits.is_empty() {
            losses.add(
                LossKind::ClipWindow,
                pattern.hits.len(),
                format!("hits in unplaced pattern '{}'", pattern.name),
            );
        }
    }
}

fn collect_music(
    project: &Project,
    track_index: usize,
    clock: &ProjectClock,
    ppq: u16,
    losses: &mut LossBook,
) -> Result<TrackMusic, MidiError> {
    let track = &project.tracks[track_index];
    let project_swing = project.properties.swing;
    let mut music = TrackMusic::default();
    for phrase in &track.phrases {
        let first_note = music.notes.len();
        let offset = phrase
            .time_range
            .map(|range| quarter_at(clock, &range.start))
            .transpose()?
            .unwrap_or(0.0);
        let arpeggio_offsets = oc_dsl::ornament::arpeggio_offsets(&phrase.events);
        for (index, note) in phrase.events.iter().enumerate() {
            if is_rest(note) {
                continue;
            }
            if note.muted {
                losses.add(LossKind::MuteSolo, 1, "muted notes");
                continue;
            }
            if note.articulation.is_some() {
                losses.add(LossKind::Articulation, 1, "articulations");
            }
            let segments =
                oc_dsl::ornament::segments(note, project.properties.key, note_duration(note))
                    .map_err(|message| MidiError::InvalidExport { message })?;
            let velocity = note
                .velocity
                .or_else(|| note.dynamics.map(|dynamics| dynamics.to_velocity()))
                .unwrap_or(80);
            for segment in segments {
                let written =
                    offset + note.start_beat + arpeggio_offsets[index] + segment.offset_quarters;
                push_note(
                    &mut music.notes,
                    ppq,
                    written
                        + swing_offset_quarters(
                            offset + note.start_beat,
                            project_swing,
                            project.properties.swing_subdivision,
                        ),
                    segment.duration_quarters,
                    segment
                        .pitch
                        .checked_to_midi()
                        .ok_or_else(|| MidiError::InvalidExport {
                            message: format!(
                                "pitch {} is outside MIDI 0..127",
                                segment.pitch.to_text()
                            ),
                        })?,
                    velocity,
                    None,
                )?;
            }
        }
        if oc_dsl::dynamics::track_uses_expression_cc11(project, track) {
            for expansion in oc_dsl::dynamics::expand_phrase_with(phrase, clock)
                .map_err(|message| MidiError::InvalidExport { message })?
            {
                for (quarter, value) in expansion.expression_points() {
                    music
                        .expression
                        .push(((quarter * f64::from(ppq)).round() as u64, value));
                }
            }
        }
        if !phrase.pedals.is_empty() {
            let phrase_end = phrase
                .time_range
                .map(|range| quarter_at(clock, &range.end))
                .transpose()?;
            collect_written_pedals(
                &phrase.pedals,
                &ClipWindow {
                    start: offset,
                    offset: 0.0,
                    loop_quarters: 0.0,
                    end: None,
                    swing: project_swing,
                    subdivision: project.properties.swing_subdivision,
                },
                ppq,
                &music.notes[first_note..],
                phrase_end,
                &mut music.pedals,
            )?;
        }
    }
    for pattern in &track.patterns {
        collect_drum_pattern(
            pattern,
            clock,
            ppq,
            project_swing,
            project.properties.swing_subdivision,
            &mut music.notes,
            losses,
        )?;
    }
    for clip in track.pattern_clips() {
        if let Some(pattern) = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
        {
            let window = ClipWindow {
                start: quarter_at(clock, &clip.time_range.start)?,
                offset: clip.offset_quarters,
                loop_quarters: clip.loop_quarters,
                end: Some(quarter_at(clock, &clip.time_range.end)?),
                swing: pattern.swing.or(project_swing),
                subdivision: project.properties.swing_subdivision,
            };
            collect_pattern_def(
                pattern,
                window,
                ppq,
                project.properties.key,
                &mut music,
                losses,
            )?;
        }
    }
    for pattern in &track.inline_patterns {
        collect_pattern_def(
            pattern,
            ClipWindow {
                start: 0.0,
                offset: 0.0,
                loop_quarters: 0.0,
                end: None,
                swing: pattern.swing.or(project_swing),
                subdivision: project.properties.swing_subdivision,
            },
            ppq,
            project.properties.key,
            &mut music,
            losses,
        )?;
    }
    music.notes.sort_by_key(|note| (note.tick, note.key));
    Ok(music)
}

fn collect_drum_pattern(
    pattern: &Pattern,
    clock: &ProjectClock,
    ppq: u16,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
    notes: &mut Vec<NoteOut>,
    losses: &mut LossBook,
) -> Result<(), MidiError> {
    let first_bar = pattern.time_range.map(|range| range.start.bar).unwrap_or(1);
    let first_start = quarter_at(clock, &BarBeat::new(first_bar, 1, 0))?;
    let start = pattern
        .time_range
        .map(|range| quarter_at(clock, &range.start))
        .transpose()?
        .unwrap_or(0.0);
    for repetition in 0..pattern.repeat {
        let bar = first_bar
            .checked_add(repetition)
            .ok_or_else(|| MidiError::InvalidExport {
                message: "pattern repeat bar overflow".into(),
            })?;
        let repeat_offset = quarter_at(clock, &BarBeat::new(bar, 1, 0))? - first_start;
        for hit in &pattern.hits {
            let written = start + repeat_offset + hit.written_quarters();
            let beat =
                written + swing_offset_quarters(written, swing, subdivision) + hit_offset(hit);
            let beat = if beat < 0.0 { 0.0 } else { beat };
            let key = hit_value(hit, "midi_note")
                .map(|value| value as u8)
                .unwrap_or_else(|| gm::drum_name_to_midi(&hit.drum));
            let velocity = hit
                .velocity
                .or_else(|| hit_value(hit, "velocity").map(|value| value as u8))
                .or_else(|| hit.dynamics.map(|dynamics| dynamics.to_velocity()))
                .unwrap_or(100);
            let duration = performance_gate_quarters(&hit.modifiers)
                .map_err(|message| MidiError::InvalidExport { message })?
                .unwrap_or(0.125);
            push_note(notes, ppq, beat, duration, key, velocity, None)?;
            if hit.articulation.is_some() {
                losses.add(LossKind::Articulation, 1, "drum articulations");
            }
        }
    }
    Ok(())
}

/// One pattern placement: where it starts, how much of the pattern head it
/// hides, how far its content repeats, and where it stops.
#[derive(Clone, Copy)]
struct ClipWindow {
    start: f64,
    offset: f64,
    loop_quarters: f64,
    end: Option<f64>,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
}

impl ClipWindow {
    /// A loop clamps its last note at the placement end; a single pass keeps
    /// the written length, as the renderer does.
    fn duration(&self, start: f64, written: f64) -> f64 {
        match self.end {
            Some(end) if self.loop_quarters > 0.0 => written.min((end - start).max(0.0)),
            _ => written,
        }
    }
}

/// Push every rendered start of one content beat, returning how many were
/// written. Zero means the placement hides the event before its first play.
#[allow(clippy::too_many_arguments)]
fn push_occurrences(
    window: &ClipWindow,
    content_beat: f64,
    ppq: u16,
    delay: f64,
    duration: f64,
    key: u8,
    velocity: u8,
    notes: &mut Vec<NoteOut>,
) -> Result<usize, MidiError> {
    // A looped placement may not render more repeats than a whole song holds.
    const MAX_OCCURRENCES: usize = 1024;
    let mut emitted = 0;
    for start in pattern_occurrences(
        window.start,
        window.offset,
        window.end,
        window.loop_quarters,
        content_beat,
        MAX_OCCURRENCES,
    ) {
        let delay = swing_offset_quarters(start, window.swing, window.subdivision) + delay;
        let Some(beat) = shifted_pattern_onset(start, delay, window.start, window.end) else {
            continue;
        };
        if push_note(
            notes,
            ppq,
            beat,
            window.duration(beat, duration),
            key,
            velocity,
            Some(window),
        )? {
            emitted += 1;
        }
    }
    Ok(emitted)
}

fn collect_pattern_def(
    pattern: &PatternDef,
    window: ClipWindow,
    ppq: u16,
    project_key: Option<KeySignature>,
    music: &mut TrackMusic,
    losses: &mut LossBook,
) -> Result<(), MidiError> {
    let first_note = music.notes.len();
    let arpeggio_offsets = oc_dsl::ornament::arpeggio_offsets(&pattern.events);
    for (index, note) in pattern.events.iter().enumerate() {
        if is_rest(note) {
            continue;
        }
        if note.muted {
            losses.add(LossKind::MuteSolo, 1, "muted notes");
            continue;
        }
        if note.articulation.is_some() {
            losses.add(LossKind::Articulation, 1, "articulations");
        }
        let segments = oc_dsl::ornament::segments(note, project_key, note_duration(note))
            .map_err(|message| MidiError::InvalidExport { message })?;
        let velocity = note
            .velocity
            .or_else(|| note.dynamics.map(|dynamics| dynamics.to_velocity()))
            .unwrap_or(80);
        let mut emitted = 0;
        for segment in segments {
            let midi_key = crate::interpret::transpose_pattern_note(
                segment
                    .pitch
                    .checked_to_midi()
                    .ok_or_else(|| MidiError::InvalidExport {
                        message: format!(
                            "pitch {} is outside MIDI 0..127",
                            segment.pitch.to_text()
                        ),
                    })?,
                pattern.transpose,
            );
            emitted += push_occurrences(
                &window,
                note.start_beat + arpeggio_offsets[index] + segment.offset_quarters,
                ppq,
                0.0,
                segment.duration_quarters,
                midi_key,
                velocity,
                &mut music.notes,
            )?;
        }
        if emitted == 0 {
            losses.add(
                LossKind::ClipWindow,
                1,
                format!("notes outside the clip window in '{}'", pattern.name),
            );
        }
    }
    for hit in &pattern.hits {
        let content_beat = hit.written_quarters();
        let key = hit_value(hit, "midi_note")
            .map(|value| value as u8)
            .unwrap_or_else(|| gm::drum_name_to_midi(&hit.drum));
        let velocity = hit
            .velocity
            .or_else(|| hit_value(hit, "velocity").map(|value| value as u8))
            .or_else(|| hit.dynamics.map(|dynamics| dynamics.to_velocity()))
            .unwrap_or(100);
        let duration = performance_gate_quarters(&hit.modifiers)
            .map_err(|message| MidiError::InvalidExport { message })?
            .unwrap_or(0.125);
        let delay = hit_offset(hit);
        let emitted = push_occurrences(
            &window,
            content_beat,
            ppq,
            delay,
            duration,
            key,
            velocity,
            &mut music.notes,
        )?;
        if emitted == 0 {
            losses.add(
                LossKind::ClipWindow,
                1,
                format!("hits outside the clip window in '{}'", pattern.name),
            );
        }
        if hit.articulation.is_some() {
            losses.add(LossKind::Articulation, 1, "drum articulations");
        }
    }
    collect_written_pedals(
        &pattern.pedals,
        &window,
        ppq,
        &music.notes[first_note..],
        None,
        &mut music.pedals,
    )
}

fn collect_written_pedals(
    pedals: &[PedalEvent],
    window: &ClipWindow,
    ppq: u16,
    notes: &[NoteOut],
    phrase_end: Option<f64>,
    out: &mut Vec<(u64, bool)>,
) -> Result<(), MidiError> {
    if pedals.is_empty() {
        return Ok(());
    }
    let tick_at = |beat: f64| (beat * f64::from(ppq)).round() as u64;
    let clip_end = window.end.map(tick_at);
    let first_mark = out.len();
    for (beat, down) in placed_pedal_marks(
        pedals,
        window.start,
        window.offset,
        window.loop_quarters,
        window.end,
        window.swing,
        window.subdivision,
    ) {
        if !beat.is_finite() || beat < 0.0 {
            return Err(MidiError::InvalidExport {
                message: "pedal position is not a finite nonnegative beat".into(),
            });
        }
        let tick = tick_at(beat);
        if clip_end.is_none_or(|end| tick < end) {
            out.push((tick, down));
        }
    }
    let marks = &mut out[first_mark..];
    marks.sort_by_key(|&(tick, _)| tick);
    if let Some(&(last_down, true)) = marks.last() {
        let release = notes
            .iter()
            .map(|note| note.tick.saturating_add(note.duration))
            .max()
            .unwrap_or(0)
            .max(phrase_end.map_or(0, tick_at))
            .max(last_down);
        out.push((release, false));
    }
    Ok(())
}

fn push_note(
    notes: &mut Vec<NoteOut>,
    ppq: u16,
    start_quarter: f64,
    duration_quarter: f64,
    key: u8,
    velocity: u8,
    window: Option<&ClipWindow>,
) -> Result<bool, MidiError> {
    if !start_quarter.is_finite() || start_quarter < 0.0 || !duration_quarter.is_finite() {
        return Err(MidiError::InvalidExport {
            message: "note position is not a finite nonnegative beat".into(),
        });
    }
    let ppq = f64::from(ppq);
    let tick = (start_quarter * ppq).round() as u64;
    let end_tick = window
        .and_then(|window| window.end)
        .map(|end| (end * ppq).round() as u64);
    if end_tick.is_some_and(|end| tick >= end) {
        return Ok(false);
    }
    let exact_duration = (duration_quarter * ppq).round();
    let widened = exact_duration < 1.0;
    let mut duration = exact_duration.max(1.0) as u64;
    if let Some(end) = end_tick
        && window.is_some_and(|window| window.loop_quarters > 0.0)
    {
        duration = duration.min(end - tick);
    }
    notes.push(NoteOut {
        tick,
        duration,
        key: key.min(127),
        velocity: velocity.clamp(1, 127),
        widened,
    });
    Ok(true)
}

fn note_duration(note: &NoteEvent) -> f64 {
    performance_gate_quarters(&note.modifiers)
        .ok()
        .flatten()
        .unwrap_or_else(|| note.duration.to_beats())
}

fn is_rest(note: &NoteEvent) -> bool {
    note.modifiers.iter().any(|(key, _)| key == "rest")
}

fn hit_offset(hit: &HitEvent) -> f64 {
    hit.modifiers
        .iter()
        .find(|(key, _)| key == "timing_offset_beats")
        .and_then(|(_, value)| value.as_f64())
        .filter(|offset| offset.is_finite() && (-0.125..=0.125).contains(offset))
        .unwrap_or(0.0)
}

fn hit_value(hit: &HitEvent, key: &str) -> Option<i32> {
    let value = hit
        .modifiers
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_f64())?;
    (value.is_finite() && value.fract() == 0.0 && (0.0..=127.0).contains(&value))
        .then_some(value as i32)
}

// ============================================================================
// Automation
// ============================================================================

struct EventSink<'a> {
    events: &'a mut Vec<PayloadEvent>,
    sequence: &'a mut usize,
    losses: &'a mut LossBook,
}

fn collect_automation(
    track: &Track,
    channel: u8,
    clock: &ProjectClock,
    ppq: u16,
    bend_range: f64,
    sink: &mut EventSink<'_>,
) -> Result<usize, MidiError> {
    let mut written = 0;
    for lane in track.automation() {
        if lane.effect_lane().is_some() {
            continue;
        }
        if let Some(controller) = gm::lane_to_cc(&lane.param) {
            for point in &lane.points {
                let Some(value) = param_to_cc(&point.value) else {
                    continue;
                };
                let tick = lane_tick(clock, lane, &point.position, ppq)?;
                sink.events.push(PayloadEvent::new(
                    tick,
                    if controller == 64 {
                        RANK_SUSTAIN
                    } else {
                        RANK_CONTROL
                    },
                    sink.sequence,
                    channel,
                    EventKind::Cc { controller, value },
                ));
                written += 1;
            }
            continue;
        }
        if gm::is_pitch_bend_lane(&lane.param) {
            for point in &lane.points {
                let Some(semitones) = point.value.as_f64() else {
                    continue;
                };
                let tick = lane_tick(clock, lane, &point.position, ppq)?;
                sink.events.push(PayloadEvent::new(
                    tick,
                    RANK_CONTROL,
                    sink.sequence,
                    channel,
                    EventKind::Bend(gm::semitones_to_pitch_bend(semitones, bend_range)),
                ));
                written += 1;
            }
            continue;
        }
        if gm::is_pressure_lane(&lane.param) {
            for point in &lane.points {
                let Some(value) = param_to_normalized(&point.value) else {
                    continue;
                };
                let tick = lane_tick(clock, lane, &point.position, ppq)?;
                sink.events.push(PayloadEvent::new(
                    tick,
                    RANK_CONTROL,
                    sink.sequence,
                    channel,
                    EventKind::Pressure((value * 127.0).round().clamp(0.0, 127.0) as u8),
                ));
                written += 1;
            }
            continue;
        }
        if !lane.points.is_empty() {
            sink.losses.add(
                LossKind::Automation,
                lane.points.len(),
                format!("{} ({} points)", lane.param, lane.points.len()),
            );
        }
    }
    Ok(written)
}

fn lane_tick(
    clock: &ProjectClock,
    lane: &AutomationLane,
    position: &BarBeat,
    ppq: u16,
) -> Result<u64, MidiError> {
    let quarter = if lane.ppq == 0 {
        clock.quarter_notes_at(position)
    } else {
        clock.quarter_notes_at_ppq(position, lane.ppq)
    }
    .map_err(|message| MidiError::InvalidExport { message })?;
    Ok((quarter * f64::from(ppq)).round().max(0.0) as u64)
}

fn push_rpn_range(events: &mut Vec<PayloadEvent>, sequence: &mut usize, channel: u8, range: f64) {
    let range = range.round().clamp(1.0, 127.0) as u8;
    for (controller, value) in [(101, 0), (100, 0), (6, range), (101, 127), (100, 127)] {
        events.push(PayloadEvent::new(
            0,
            RANK_CONTROL,
            sequence,
            channel,
            EventKind::Cc { controller, value },
        ));
    }
}

fn param_to_cc(value: &ParamValue) -> Option<u8> {
    match value {
        ParamValue::Percentage(percent) => Some((percent * 127.0 / 100.0).round() as u8),
        ParamValue::Db(db) => Some(db_to_midi_volume(*db)),
        ParamValue::Bool(value) => Some(if *value { 127 } else { 0 }),
        ParamValue::String(name) => dynamics_velocity(name),
        _ => value.as_f64().map(|value| {
            if (0.0..=1.0).contains(&value) {
                (value * 127.0).round() as u8
            } else {
                value.round().clamp(0.0, 127.0) as u8
            }
        }),
    }
}

fn param_to_normalized(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Percentage(percent) => Some((percent / 100.0).clamp(0.0, 1.0)),
        ParamValue::Db(db) => Some(f64::from(db_to_midi_volume(*db)) / 127.0),
        ParamValue::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        ParamValue::String(name) => dynamics_velocity(name).map(|value| f64::from(value) / 127.0),
        _ => value.as_f64().map(|value| {
            if (0.0..=1.0).contains(&value) {
                value
            } else {
                (value / 127.0).clamp(0.0, 1.0)
            }
        }),
    }
}

fn dynamics_velocity(name: &str) -> Option<u8> {
    Dynamics::from_token(name).map(|dynamics| dynamics.to_velocity())
}

fn db_to_midi_volume(db: f64) -> u8 {
    let linear = 10.0_f64.powf(db / 20.0);
    (linear * 100.0).clamp(0.0, 127.0) as u8
}

// ============================================================================
// Conductor events
// ============================================================================

fn push_clock_events(
    project: &Project,
    clock: &ProjectClock,
    ppq: u16,
    events: &mut Vec<PayloadEvent>,
    sequence: &mut usize,
) -> Result<(), MidiError> {
    let properties = &project.properties;
    let mut tempos: BTreeMap<u64, u32> = BTreeMap::new();
    tempos.insert(0, tempo_micros(properties.tempo)?);
    for point in &properties.tempo_map {
        let quarter = quarter_at(clock, &point.position)? + point.offset_quarters;
        let tick = (quarter * f64::from(ppq)).round().max(0.0) as u64;
        tempos.insert(tick, tempo_micros(point.bpm)?);
    }
    for (tick, micros) in tempos {
        events.push(PayloadEvent::new(
            tick,
            RANK_META,
            sequence,
            1,
            EventKind::Tempo(micros),
        ));
    }

    let mut meters: BTreeMap<u64, (u8, u8)> = BTreeMap::new();
    let signature = properties.time_signature;
    meters.insert(0, (signature.numerator, signature.denominator));
    for point in &properties.time_sig_map {
        let tick = (quarter_at(clock, &point.position)? * f64::from(ppq)).round() as u64;
        meters.insert(tick, (point.numerator, point.denominator));
    }
    for (tick, (numerator, denominator)) in meters {
        events.push(PayloadEvent::new(
            tick,
            RANK_META,
            sequence,
            1,
            EventKind::TimeSignature(numerator, denominator),
        ));
    }

    let mut keys: BTreeMap<u64, (i8, bool)> = BTreeMap::new();
    if let Some(key) = &properties.key {
        keys.insert(0, gm::key_signature_midi(key));
    }
    for point in &properties.key_map {
        let tick = (quarter_at(clock, &point.position)? * f64::from(ppq)).round() as u64;
        keys.insert(tick, gm::key_signature_midi(&point.key));
    }
    for (tick, (sharps_flats, minor)) in keys {
        events.push(PayloadEvent::new(
            tick,
            RANK_META,
            sequence,
            1,
            EventKind::KeySignature(sharps_flats, minor),
        ));
    }
    Ok(())
}

fn tempo_micros(bpm: f64) -> Result<u32, MidiError> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err(MidiError::InvalidExport {
            message: format!("invalid tempo {bpm}"),
        });
    }
    let micros = (60_000_000.0 / bpm).round();
    if !(1.0..=16_777_215.0).contains(&micros) {
        return Err(MidiError::InvalidExport {
            message: format!("tempo {bpm} bpm needs more than 24 bits of microseconds"),
        });
    }
    Ok(micros as u32)
}

fn quarter_at(clock: &ProjectClock, position: &BarBeat) -> Result<f64, MidiError> {
    clock
        .quarter_notes_at(position)
        .map_err(|message| MidiError::InvalidExport { message })
}

// ============================================================================
// Losses
// ============================================================================

#[derive(Default)]
struct LossBook {
    items: BTreeMap<LossKind, (usize, BTreeSet<String>)>,
}

impl LossBook {
    fn add(&mut self, kind: LossKind, count: usize, detail: impl Into<String>) {
        if count == 0 {
            return;
        }
        let entry = self.items.entry(kind).or_default();
        entry.0 += count;
        entry.1.insert(detail.into());
    }

    fn into_items(self) -> Vec<LossItem> {
        self.items
            .into_iter()
            .map(|(kind, (count, details))| LossItem {
                kind,
                count,
                detail: details.into_iter().collect::<Vec<_>>().join("; "),
            })
            .collect()
    }
}

fn collect_selection_losses(project: &Project, selected: &[usize], losses: &mut LossBook) {
    let mut plugin_tracks = Vec::new();
    let mut microtuned = 0;
    for &index in selected {
        let track = &project.tracks[index];
        if track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
            .is_some_and(|preset| {
                matches!(
                    preset.source.as_deref(),
                    Some("clap" | "vst3" | "builtin" | "sample")
                )
            })
        {
            plugin_tracks.push(track.name.clone());
        }
        let tuning = track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
            .is_some_and(|preset| {
                preset.params.iter().any(|(key, _)| {
                    let key = gm::normalize_param(key);
                    key.contains("scala") || key.contains("tuning") || key.contains("tunfile")
                })
            });
        if tuning {
            microtuned += 1;
        }
        if track.mix.mute || track.mix.solo {
            losses.add(
                LossKind::MuteSolo,
                1,
                format!("mute/solo state on '{}'", track.name),
            );
        }
        if !track.mix.sends.is_empty() {
            losses.add(LossKind::SendBus, 1, format!("sends on '{}'", track.name));
        }
        if track_program(track).is_none() {
            losses.add(
                LossKind::ProgramChange,
                1,
                format!("no GM program for '{}'", track.name),
            );
        }
    }
    if !plugin_tracks.is_empty() {
        losses.add(
            LossKind::PluginParameter,
            plugin_tracks.len(),
            format!("plugin tracks: {}", plugin_tracks.join(", ")),
        );
    }
    if microtuned != 0 {
        losses.add(
            LossKind::Microtuning,
            microtuned,
            "preset tuning parameters",
        );
    }
    let mut bus_sends = Vec::new();
    for bus in &project.buses {
        bus_sends.push(bus.name.clone());
    }
    if !bus_sends.is_empty() {
        losses.add(
            LossKind::SendBus,
            bus_sends.len(),
            format!("buses: {}", bus_sends.join(", ")),
        );
    }
    let mut envelopes = project
        .buses
        .iter()
        .map(|bus| bus.gain_automation.len())
        .chain(
            project
                .tracks
                .iter()
                .map(|track| track.gain_automation.len() + hairpin_count(track)),
        )
        .sum::<usize>();
    if let Some(master) = &project.master {
        envelopes += master.gain_automation.len();
    }
    if envelopes != 0 {
        losses.add(
            LossKind::ClipEnvelope,
            envelopes,
            "bus, master and track gain envelopes",
        );
    }
}

/// Hairpins that will become gain envelopes on the render, counted for the loss
/// report because a Standard MIDI File has no gain lane to carry either form.
fn hairpin_count(track: &oc_dsl::ast::Track) -> usize {
    track
        .phrases
        .iter()
        .map(|phrase| phrase.hairpins.len())
        .sum()
}

// ============================================================================
// SMF encoding
// ============================================================================

#[derive(Clone)]
enum EventKind {
    Tempo(u32),
    TimeSignature(u8, u8),
    KeySignature(i8, bool),
    Program(u8),
    Cc { controller: u8, value: u8 },
    Bend(i16),
    Pressure(u8),
    NoteOn { key: u8, velocity: u8 },
    NoteOff { key: u8 },
}

#[derive(Clone)]
struct PayloadEvent {
    tick: u64,
    rank: u8,
    sequence: usize,
    channel: u8,
    kind: EventKind,
}

impl PayloadEvent {
    fn new(tick: u64, rank: u8, sequence: &mut usize, channel: u8, kind: EventKind) -> Self {
        let event = Self {
            tick,
            rank,
            sequence: *sequence,
            channel,
            kind,
        };
        *sequence += 1;
        event
    }
}

struct TrackPayload {
    name: String,
    events: Vec<PayloadEvent>,
}

fn encode_track<'a>(
    mut events: Vec<PayloadEvent>,
    name: &'a [u8],
) -> Result<Vec<TrackEvent<'a>>, MidiError> {
    events.sort_by_key(|event| (event.tick, event.rank, event.sequence));
    let end_tick = events.iter().map(|event| event.tick).max().unwrap_or(0);
    let mut encoded = Vec::with_capacity(events.len() + 2);
    encoded.push(TrackEvent {
        delta: 0.into(),
        kind: TrackEventKind::Meta(MetaMessage::TrackName(name)),
    });
    let mut previous = 0_u64;
    for event in events {
        let delta = event.tick.saturating_sub(previous);
        previous = event.tick;
        encoded.push(TrackEvent {
            delta: delta_u28(delta)?,
            kind: to_midly(event.kind, event.channel)?,
        });
    }
    encoded.push(TrackEvent {
        delta: delta_u28(end_tick.saturating_sub(previous))?,
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    Ok(encoded)
}

fn to_midly(kind: EventKind, channel: u8) -> Result<TrackEventKind<'static>, MidiError> {
    let channel = u4_value(channel)?;
    Ok(match kind {
        EventKind::Tempo(micros) => TrackEventKind::Meta(MetaMessage::Tempo(u24_value(micros)?)),
        EventKind::TimeSignature(numerator, denominator) => {
            let power = denominator_power(denominator)?;
            TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, power, 24, 8))
        }
        EventKind::KeySignature(sharps_flats, minor) => {
            TrackEventKind::Meta(MetaMessage::KeySignature(sharps_flats, minor))
        }
        EventKind::Program(program) => TrackEventKind::Midi {
            channel,
            message: MidiMessage::ProgramChange {
                program: u7_value(program)?,
            },
        },
        EventKind::Cc { controller, value } => TrackEventKind::Midi {
            channel,
            message: MidiMessage::Controller {
                controller: u7_value(controller)?,
                value: u7_value(value)?,
            },
        },
        EventKind::Bend(value) => TrackEventKind::Midi {
            channel,
            message: MidiMessage::PitchBend {
                bend: PitchBend::from_int(value),
            },
        },
        EventKind::Pressure(value) => TrackEventKind::Midi {
            channel,
            message: MidiMessage::ChannelAftertouch {
                vel: u7_value(value)?,
            },
        },
        EventKind::NoteOn { key, velocity } => TrackEventKind::Midi {
            channel,
            message: MidiMessage::NoteOn {
                key: u7_value(key)?,
                vel: u7_value(velocity)?,
            },
        },
        EventKind::NoteOff { key } => TrackEventKind::Midi {
            channel,
            message: MidiMessage::NoteOff {
                key: u7_value(key)?,
                vel: 0.into(),
            },
        },
    })
}

fn delta_u28(delta: u64) -> Result<midly::num::u28, MidiError> {
    let delta = u32::try_from(delta).map_err(|_| MidiError::InvalidExport {
        message: format!("tick distance {delta} does not fit in an SMF delta"),
    })?;
    midly::num::u28::try_from(delta).ok_or_else(|| MidiError::InvalidExport {
        message: format!("tick distance {delta} does not fit in an SMF delta"),
    })
}

fn u4_value(value: u8) -> Result<midly::num::u4, MidiError> {
    if !(1..=16).contains(&value) {
        return Err(MidiError::InvalidExport {
            message: format!("channel {value} is invalid"),
        });
    }
    midly::num::u4::try_from(value.saturating_sub(1)).ok_or_else(|| MidiError::InvalidExport {
        message: "invalid MIDI channel".into(),
    })
}

fn u7_value(value: u8) -> Result<midly::num::u7, MidiError> {
    midly::num::u7::try_from(value).ok_or_else(|| MidiError::InvalidExport {
        message: format!("MIDI value {value} exceeds 127"),
    })
}

fn u24_value(value: u32) -> Result<midly::num::u24, MidiError> {
    midly::num::u24::try_from(value).ok_or_else(|| MidiError::InvalidExport {
        message: format!("MIDI value {value} exceeds 24 bits"),
    })
}

fn denominator_power(denominator: u8) -> Result<u8, MidiError> {
    match denominator {
        1 => Ok(0),
        2 => Ok(1),
        4 => Ok(2),
        8 => Ok(3),
        16 => Ok(4),
        32 => Ok(5),
        64 => Ok(6),
        _ => Err(MidiError::InvalidExport {
            message: format!("meter denominator {denominator} is unsupported"),
        }),
    }
}
