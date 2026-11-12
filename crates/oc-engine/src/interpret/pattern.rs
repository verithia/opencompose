//! Phrase, pattern and clip visitors.

use super::*;

/// Move a written MIDI key by a pattern's `transpose`.
///
/// Only pitched `events` carry a transposition: a `hits` entry addresses a
/// sample by name, so it is never shifted. Zero and an absent field are both
/// the identity, and the result saturates at the MIDI key range instead of
/// wrapping to the other end of the keyboard.
pub(crate) fn transpose_pattern_note(note: u8, transpose: Option<i8>) -> u8 {
    match transpose {
        Some(semitones) if semitones != 0 => {
            (i16::from(note) + i16::from(semitones)).clamp(0, 127) as u8
        }
        _ => note,
    }
}

/// 50% is straight; the off-beat position of the chosen grid is delayed by
/// `(swing/100 - 0.5)` quarters on the eighth grid, and half that on the
/// sixteenth grid. Triplets are never on either grid and stay untouched.
pub(crate) fn swing_offset_quarters(
    position: f64,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
) -> f64 {
    let Some(percent) = swing else {
        return 0.0;
    };
    let (step, scale) = match subdivision {
        oc_dsl::ast::SwingSubdivision::Eighth => (0.5, 1.0),
        oc_dsl::ast::SwingSubdivision::Sixteenth => (0.25, 0.5),
    };
    let grid = position / step;
    if (grid - grid.round()).abs() > 1e-9 || grid.round() as i64 % 2 == 0 {
        return 0.0;
    }
    (percent / 100.0 - 0.5) * scale
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn interpret_phrase(
    phrase: &Phrase,
    channel: u8,
    track_index: usize,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    profile: &InstrumentProfile,
    sustain_timeline: &[SustainPoint],
    articulation_occurrences: &mut HashMap<String, usize>,
    context: &GlissContext<'_>,
    curved: &std::collections::HashSet<u8>,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
    hairpin_cc11: bool,
    section_flags: &[super::section::SectionFlag],
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    let notes = super::gliss::expand(&phrase.events, context)?;
    let notes = notes.as_ref();
    let ornament_key = context.key;
    let arpeggio_offsets = oc_dsl::ornament::arpeggio_offsets(notes);
    // Phrase start offset in absolute beats
    let phrase_offset = phrase
        .time_range
        .map(|tr| clock.quarter_notes_at(&tr.start))
        .transpose()?
        .unwrap_or(0.0);
    let mut last_note_off = 0u64;
    // A hairpin carries the level of the notes it spans; the mark itself
    // becomes a gain ramp on the track, not a per-note velocity ladder.
    let hairpin_levels: HashMap<usize, u8> = oc_dsl::dynamics::expand_phrase_with(phrase, clock)?
        .into_iter()
        .flat_map(|expansion| expansion.levels)
        .collect();

    if hairpin_cc11 {
        for expansion in oc_dsl::dynamics::expand_phrase_with(phrase, clock)? {
            for (quarter, value) in expansion.expression_points() {
                events.push(MidiEvent {
                    time_samples: clock.sample_at_quarter(quarter, settings.sample_rate)?,
                    event: MidiMessage::ControlChange {
                        channel,
                        controller: 11,
                        value,
                    },
                    track_index,
                });
            }
        }
    }

    for (note_ordinal, note_event) in notes.iter().enumerate() {
        // Check if this is a rest
        let is_rest = note_event.modifiers.iter().any(|(k, _)| k == "rest");

        if is_rest || note_event.muted {
            continue; // rests/muted notes don't produce sound; timing is in start_beat
        }

        // A section flag is a region technique. The note's own articulation is
        // more specific, so it wins when both are present.
        let flagged;
        let note_event = if note_event.articulation.is_none() {
            match super::section::articulation_at(
                section_flags,
                phrase_offset + note_event.start_beat,
            ) {
                Some(flag) => {
                    let mut event = note_event.clone();
                    event.articulation = Some(flag);
                    flagged = Some(event);
                    flagged.as_ref().unwrap()
                }
                None => note_event,
            }
        } else {
            note_event
        };

        let note_channel = settings
            .mpe
            .as_ref()
            .and_then(|mpe| mpe.channel_for_note(note_ordinal))
            .or_else(|| profile.mpe_channel_for_note(note_ordinal))
            .unwrap_or(channel);
        let mut velocity = hairpin_levels
            .get(&note_ordinal)
            .copied()
            .unwrap_or_else(|| {
                note_event.velocity.unwrap_or_else(|| {
                    note_event
                        .dynamics
                        .map(|d| d.to_velocity())
                        .unwrap_or(settings.default_velocity)
                })
            });
        velocity = apply_articulation_velocity(velocity, note_event.articulation);

        let gate = performance_gate_quarters(&note_event.modifiers)?
            .unwrap_or_else(|| note_duration_for_profile(note_event, profile));
        let segments =
            oc_dsl::ornament::segments(note_event, ornament_key, gate).map_err(|message| {
                format!(
                    "{message} at @{} in track '{}'",
                    note_event.start_beat, context.track
                )
            })?;
        let articulation_occurrence =
            next_articulation_occurrence(note_event.articulation, articulation_occurrences);

        for segment in segments {
            let note = segment.pitch.to_midi();
            let identified = curved.contains(&note);
            let abs_beat = phrase_offset
                + note_event.start_beat
                + arpeggio_offsets[note_ordinal]
                + segment.offset_quarters;
            let swung_beat = abs_beat + swing_offset_quarters(abs_beat, swing, subdivision);
            let mut note_on_time = clock.sample_at_quarter(swung_beat, settings.sample_rate)?;
            let note_duration_beats = segment.duration_quarters;
            let mut note_off_time =
                clock.sample_at_quarter(swung_beat + note_duration_beats, settings.sample_rate)?;
            if let Some(articulation) = note_event.articulation {
                note_off_time += profile
                    .overlap_samples_for_articulation(articulation)
                    .unwrap_or(0);
            }
            let humanize = articulation_humanize_for_note(
                note_event.articulation,
                profile,
                track_index,
                note_channel,
                note,
                note_on_time,
            );
            note_on_time = apply_sample_offset(note_on_time, humanize.timing_offset);
            note_off_time = positive_gate_end(
                note_on_time,
                apply_sample_offset(note_off_time, humanize.timing_offset),
            );
            let velocity = apply_velocity_offset(velocity, humanize.velocity_offset);
            last_note_off = last_note_off.max(note_off_time);

            emit_articulation_profile_events(
                note_event.articulation,
                articulation_occurrence,
                profile,
                channel,
                track_index,
                note_on_time,
                events,
            );

            let curve_on_index = events.len();
            events.push(MidiEvent {
                time_samples: note_on_time,
                event: MidiMessage::NoteOn {
                    channel: note_channel,
                    note,
                    velocity,
                },
                track_index,
            });

            emit_profile_note_tuning(
                profile,
                note_channel,
                note,
                track_index,
                note_on_time,
                events,
            );

            let curve_off_index = events.len();
            events.push(MidiEvent {
                time_samples: note_off_time,
                event: MidiMessage::NoteOff {
                    channel: note_channel,
                    note,
                },
                track_index,
            });

            emit_articulation_profile_resets(
                note_event.articulation,
                profile,
                note_channel,
                track_index,
                note_off_time,
                events,
            );
            emit_articulation_release_trigger(
                note_event.articulation,
                profile,
                sustain_timeline,
                note_channel,
                track_index,
                note_off_time,
                events,
            );

            emit_note_expression_events(
                note_event,
                (curve_on_index, curve_off_index),
                identified,
                note_channel,
                note,
                track_index,
                swung_beat,
                note_duration_beats,
                clock,
                settings,
                profile,
                events,
            )?;
        }
    }

    let last_pedal_down = emit_pedal_marks(
        &phrase.pedals,
        phrase_offset,
        0.0,
        0.0,
        None,
        channel,
        track_index,
        clock,
        settings,
        swing,
        subdivision,
        events,
    )?;
    if let Some(last_down) = last_pedal_down {
        let phrase_end = phrase
            .time_range
            .map(|range| clock.quarter_notes_at(&range.end))
            .transpose()?
            .map(|quarter| clock.sample_at_quarter(quarter, settings.sample_rate))
            .transpose()?
            .unwrap_or(last_note_off);
        emit_pedal_lift(
            channel,
            track_index,
            phrase_end.max(last_note_off).max(last_down),
            events,
        );
    }
    Ok(())
}

/// Playback beats of one content position. A loop repeats the half-open
/// source window `[content_offset, content_offset + loop_quarters)` until the
/// placement ends. A single pass plays each eligible onset once.
pub(crate) fn pattern_occurrences(
    clip_start: f64,
    content_offset: f64,
    clip_end: Option<f64>,
    loop_quarters: f64,
    content_beat: f64,
    limit: usize,
) -> impl Iterator<Item = f64> {
    const EPSILON: f64 = 1e-9;
    let local = content_beat - content_offset;
    let looping = loop_quarters > 0.0;
    let count = if local < -EPSILON || (looping && local >= loop_quarters - EPSILON) {
        0
    } else if looping {
        limit
    } else {
        limit.min(1)
    };
    let first = clip_start + local;
    (0..count)
        .map(move |occurrence| first + occurrence as f64 * loop_quarters)
        .take_while(move |&start| clip_end.is_none_or(|end| start < end - EPSILON))
}

/// Place an eligible onset after swing or an explicit timing offset. Keep a
/// pickup at the clip head and skip an onset shifted past the final edge.
pub(crate) fn shifted_pattern_onset(
    beat: f64,
    delay: f64,
    placement_start: f64,
    placement_end: Option<f64>,
) -> Option<f64> {
    let shifted = beat + delay;
    let onset = if shifted < placement_start {
        placement_start
    } else {
        shifted
    };
    if placement_end.is_some_and(|end| onset >= end - 1e-9) {
        return None;
    }
    Some(onset)
}

fn positive_gate_end(onset: u64, end: u64) -> u64 {
    end.max(onset.saturating_add(1))
}

/// Place written pedal marks on the timeline before converting them to
/// samples or MIDI ticks. Swing and the source window follow note onsets.
pub(crate) fn placed_pedal_marks(
    pedals: &[PedalEvent],
    clip_start: f64,
    content_offset: f64,
    loop_quarters: f64,
    clip_end: Option<f64>,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
) -> impl Iterator<Item = (f64, bool)> + '_ {
    pedals.iter().flat_map(move |pedal| {
        pattern_occurrences(
            clip_start,
            content_offset,
            clip_end,
            loop_quarters,
            pedal.beat,
            1024,
        )
        .filter_map(move |beat| {
            shifted_pattern_onset(
                beat,
                swing_offset_quarters(beat, swing, subdivision),
                clip_start,
                clip_end,
            )
            .map(|onset| (onset, pedal.down))
        })
    })
}

/// Sustain pedal marks emit CC64 127 for `down` and 0 for `up`.
/// Returns the time of the final emitted mark if it leaves the pedal down.
#[allow(clippy::too_many_arguments)]
fn emit_pedal_marks(
    pedals: &[PedalEvent],
    clip_start: f64,
    content_offset: f64,
    loop_quarters: f64,
    clip_end: Option<f64>,
    channel: u8,
    track_index: usize,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
    events: &mut Vec<MidiEvent>,
) -> Result<Option<u64>, String> {
    if pedals.is_empty() {
        return Ok(None);
    }
    let end_sample = clip_end
        .map(|end| clock.sample_at_quarter(end, settings.sample_rate))
        .transpose()?;
    let mut marks = Vec::new();
    for (onset, down) in placed_pedal_marks(
        pedals,
        clip_start,
        content_offset,
        loop_quarters,
        clip_end,
        swing,
        subdivision,
    ) {
        let sample = clock.sample_at_quarter(onset, settings.sample_rate)?;
        if end_sample.is_none_or(|end| sample < end) {
            marks.push((sample, down));
        }
    }
    marks.sort_by_key(|&(sample, _)| sample);
    let last_down = marks
        .last()
        .and_then(|&(sample, down)| down.then_some(sample));
    for (time_samples, down) in marks {
        events.push(MidiEvent {
            time_samples,
            event: MidiMessage::ControlChange {
                channel,
                controller: 64,
                value: if down { 127 } else { 0 },
            },
            track_index,
        });
    }
    Ok(last_down)
}

fn emit_pedal_lift(
    channel: u8,
    track_index: usize,
    time_samples: u64,
    events: &mut Vec<MidiEvent>,
) {
    events.push(MidiEvent {
        time_samples,
        event: MidiMessage::ControlChange {
            channel,
            controller: 64,
            value: 0,
        },
        track_index,
    });
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn interpret_pattern(
    pattern: &Pattern,
    _channel: u8,
    track_index: usize,
    track_name: &str,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    profile: &InstrumentProfile,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    let start_beat = pattern
        .time_range
        .map(|tr| clock.quarter_notes_at(&tr.start))
        .transpose()?
        .unwrap_or(0.0);

    let first_bar = pattern.time_range.map(|range| range.start.bar).unwrap_or(1);

    let mut drum_choke_notes = HashMap::new();
    let mut hit_events = Vec::new();

    for rep in 0..pattern.repeat {
        let bar = first_bar
            .checked_add(rep)
            .ok_or("pattern repeat bar overflow")?;
        let first_bar_start = clock.quarter_notes_at(&BarBeat::new(first_bar, 1, 0))?;
        let bar_start = clock.quarter_notes_at(&BarBeat::new(bar, 1, 0))?;
        let rep_offset = bar_start - first_bar_start;

        for hit in &pattern.hits {
            let written_beat = start_beat + rep_offset + hit.written_quarters();
            let hit_beat = written_beat
                + swing_offset_quarters(written_beat, swing, subdivision)
                + hit_timing_offset(hit)?;
            let hit_beat = if hit_beat < 0.0 { 0.0 } else { hit_beat };

            let note = hit_midi_value(hit, "midi_note", 0).unwrap_or_else(|| {
                drum_to_profile_or_gm_note(&hit.drum, hit.articulation, profile, track_name)
            });
            let velocity = apply_drum_articulation_velocity(hit_velocity(hit), hit.articulation);

            let hit_time = clock.sample_at_quarter(hit_beat, settings.sample_rate)?;
            // Drum hits: short note (1/16th)
            let gate = performance_gate_quarters(&hit.modifiers)?.unwrap_or(0.125);
            let off_time = positive_gate_end(
                hit_time,
                clock.sample_at_quarter(hit_beat + gate, settings.sample_rate)?,
            );

            hit_events.push((hit_time, off_time, note, velocity, hit));
        }
    }

    hit_events.sort_by_key(|(hit_time, _, _, _, _)| *hit_time);
    for (hit_time, off_time, note, velocity, hit) in hit_events {
        emit_drum_hit(
            hit,
            note,
            velocity,
            track_index,
            hit_time,
            off_time,
            profile,
            &mut drum_choke_notes,
            events,
        );
    }
    Ok(())
}

/// Interpret a PatternDef (named/inline pattern) into MIDI events at a given offset.
/// `content_offset` hides that many quarter notes of the pattern's head; the
/// placement's start and length stay where the clip puts them.
// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn interpret_pattern_def(
    pat_def: &PatternDef,
    channel: u8,
    track_index: usize,
    track_name: &str,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    profile: &InstrumentProfile,
    sustain_timeline: &[SustainPoint],
    articulation_occurrences: &mut HashMap<String, usize>,
    context: &GlissContext<'_>,
    curved: &std::collections::HashSet<u8>,
    clip_start: f64,
    content_offset: f64,
    loop_quarters: f64,
    clip_end: Option<f64>,
    swing: Option<f64>,
    subdivision: oc_dsl::ast::SwingSubdivision,
    section_flags: &[super::section::SectionFlag],
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    // A looped placement may not render more repeats than a whole song holds.
    const MAX_OCCURRENCES: usize = 1024;
    let clip_start_samples = clock.sample_at_quarter(clip_start, settings.sample_rate)?;
    let clip_end_samples = clip_end
        .map(|end| clock.sample_at_quarter(end, settings.sample_rate))
        .transpose()?;
    let gate_end = |onset, end| {
        let end = positive_gate_end(onset, end);
        if loop_quarters > 0.0 {
            clip_end_samples.map_or(end, |limit| end.min(limit))
        } else {
            end
        }
    };
    let starts_for = |content_beat: f64| {
        pattern_occurrences(
            clip_start,
            content_offset,
            clip_end,
            loop_quarters,
            content_beat,
            MAX_OCCURRENCES,
        )
    };
    // A hit is a sample key, so a transposition that reaches one was written
    // for the pitched events. Say so once rather than move a kick to a tom.
    if !pat_def.hits.is_empty()
        && let Some(semitones) = pat_def.transpose.filter(|value| *value != 0)
    {
        crate::render_warnings::push(format!(
            "pattern '{}': transpose {} moves its events but not its hits; a hit addresses a sample by name",
            pat_def.name, semitones
        ));
    }
    // A pattern hairpin carries the level of the notes it spans, exactly as a
    // phrase mark does. The mark is relative to the pattern's content, so a
    // slipped head drops the marks it hides.
    let hairpin_levels: HashMap<usize, u8> =
        oc_dsl::dynamics::expand_pattern_with(pat_def, clock, 0.0, None, Some(content_offset))?
            .into_iter()
            .flat_map(|expansion| expansion.levels)
            .collect();
    // Process note events
    let notes = super::gliss::expand(&pat_def.events, context)?;
    let notes = notes.as_ref();
    let ornament_key = context.key;
    let arpeggio_offsets = oc_dsl::ornament::arpeggio_offsets(notes);
    let mut last_note_off = 0u64;
    for (note_ordinal, note_event) in notes.iter().enumerate() {
        let is_rest = note_event.modifiers.iter().any(|(k, _)| k == "rest");
        if is_rest || note_event.muted {
            continue;
        }
        let base_velocity = hairpin_levels
            .get(&note_ordinal)
            .copied()
            .unwrap_or_else(|| {
                note_event
                    .velocity
                    .unwrap_or_else(|| note_event.dynamics.map(|d| d.to_velocity()).unwrap_or(80))
            });
        // A slipped head is silent: its notes sit before the placement starts.
        for start_beat in starts_for(note_event.start_beat) {
            let onset = start_beat + arpeggio_offsets[note_ordinal];
            let Some(swung_onset) = shifted_pattern_onset(
                onset,
                swing_offset_quarters(start_beat, swing, subdivision),
                clip_start,
                clip_end,
            ) else {
                continue;
            };
            let flag = if note_event.articulation.is_none() {
                // Section membership follows the written placement, not the
                // swung attack: swing moves the performance, not the score.
                let placed =
                    shifted_pattern_onset(onset, 0.0, clip_start, clip_end).unwrap_or(swung_onset);
                super::section::articulation_at(section_flags, placed)
            } else {
                None
            };
            let flagged = flag.map(|flag| {
                let mut event = note_event.clone();
                event.articulation = Some(flag);
                event
            });
            let effective = flagged.as_ref().unwrap_or(note_event);
            let velocity = apply_articulation_velocity(base_velocity, effective.articulation);
            let gate = performance_gate_quarters(&effective.modifiers)?
                .unwrap_or_else(|| note_duration_for_profile(effective, profile));
            let segments =
                oc_dsl::ornament::segments(effective, ornament_key, gate).map_err(|message| {
                    format!(
                        "{message} at @{} in pattern '{}'",
                        effective.start_beat, pat_def.name
                    )
                })?;
            for segment in &segments {
                let duration_beat = segment.duration_quarters;
                let note = transpose_pattern_note(segment.pitch.to_midi(), pat_def.transpose);
                let identified = curved.contains(&note);
                let note_channel = settings
                    .mpe
                    .as_ref()
                    .and_then(|mpe| mpe.channel_for_note(note_ordinal))
                    .or_else(|| profile.mpe_channel_for_note(note_ordinal))
                    .unwrap_or(channel);
                let written = onset + segment.offset_quarters;
                let Some(swung_beat) = shifted_pattern_onset(
                    written,
                    swing_offset_quarters(start_beat, swing, subdivision),
                    clip_start,
                    clip_end,
                ) else {
                    continue;
                };
                let mut note_on = clock.sample_at_quarter(swung_beat, settings.sample_rate)?;
                let mut note_off =
                    clock.sample_at_quarter(swung_beat + duration_beat, settings.sample_rate)?;
                if let Some(articulation) = effective.articulation {
                    note_off += profile
                        .overlap_samples_for_articulation(articulation)
                        .unwrap_or(0);
                }
                let humanize = articulation_humanize_for_note(
                    effective.articulation,
                    profile,
                    track_index,
                    note_channel,
                    note,
                    note_on,
                );
                let headroom = note_on.saturating_sub(clip_start_samples);
                let offset = humanize
                    .timing_offset
                    .max(-i64::try_from(headroom).unwrap_or(i64::MAX));
                note_on = apply_sample_offset(note_on, offset);
                if clip_end_samples.is_some_and(|end| note_on >= end) {
                    continue;
                }
                note_off = gate_end(note_on, apply_sample_offset(note_off, offset));
                let velocity = apply_velocity_offset(velocity, humanize.velocity_offset);
                last_note_off = last_note_off.max(note_off);
                let articulation_occurrence =
                    next_articulation_occurrence(effective.articulation, articulation_occurrences);

                emit_articulation_profile_events(
                    effective.articulation,
                    articulation_occurrence,
                    profile,
                    channel,
                    track_index,
                    note_on,
                    events,
                );
                let curve_on_index = events.len();
                events.push(MidiEvent {
                    time_samples: note_on,
                    event: MidiMessage::NoteOn {
                        channel: note_channel,
                        note,
                        velocity,
                    },
                    track_index,
                });
                emit_profile_note_tuning(profile, note_channel, note, track_index, note_on, events);
                let curve_off_index = events.len();
                events.push(MidiEvent {
                    time_samples: note_off,
                    event: MidiMessage::NoteOff {
                        channel: note_channel,
                        note,
                    },
                    track_index,
                });

                emit_articulation_profile_resets(
                    effective.articulation,
                    profile,
                    note_channel,
                    track_index,
                    note_off,
                    events,
                );
                emit_articulation_release_trigger(
                    effective.articulation,
                    profile,
                    sustain_timeline,
                    note_channel,
                    track_index,
                    note_off,
                    events,
                );

                emit_note_expression_events(
                    effective,
                    (curve_on_index, curve_off_index),
                    identified,
                    note_channel,
                    note,
                    track_index,
                    swung_beat,
                    duration_beat,
                    clock,
                    settings,
                    profile,
                    events,
                )?;
            }
        }
    }

    // Process drum hits
    let mut drum_choke_notes = HashMap::new();
    let mut hit_events = Vec::new();
    for hit in &pat_def.hits {
        let note = hit_midi_value(hit, "midi_note", 0).unwrap_or_else(|| {
            drum_to_profile_or_gm_note(&hit.drum, hit.articulation, profile, track_name)
        });
        let velocity = apply_drum_articulation_velocity(hit_velocity(hit), hit.articulation);
        let content_beat = hit.written_quarters();
        for start_beat in starts_for(content_beat) {
            let Some(hit_beat) = shifted_pattern_onset(
                start_beat,
                swing_offset_quarters(start_beat, swing, subdivision) + hit_timing_offset(hit)?,
                clip_start,
                clip_end,
            ) else {
                continue;
            };
            let hit_time = clock.sample_at_quarter(hit_beat, settings.sample_rate)?;
            if clip_end_samples.is_some_and(|end| hit_time >= end) {
                continue;
            }
            let gate = performance_gate_quarters(&hit.modifiers)?.unwrap_or(0.125);
            let off_time = gate_end(
                hit_time,
                clock.sample_at_quarter(hit_beat + gate, settings.sample_rate)?,
            );
            hit_events.push((hit_time, off_time, note, velocity, hit));
        }
    }

    hit_events.sort_by_key(|(hit_time, _, _, _, _)| *hit_time);
    let mut last_off = last_note_off;
    for (hit_time, off_time, note, velocity, hit) in hit_events {
        last_off = last_off.max(off_time);
        emit_drum_hit(
            hit,
            note,
            velocity,
            track_index,
            hit_time,
            off_time,
            profile,
            &mut drum_choke_notes,
            events,
        );
    }

    let last_pedal_down = emit_pedal_marks(
        &pat_def.pedals,
        clip_start,
        content_offset,
        loop_quarters,
        clip_end,
        channel,
        track_index,
        clock,
        settings,
        swing,
        subdivision,
        events,
    )?;
    if let Some(last_down) = last_pedal_down {
        emit_pedal_lift(channel, track_index, last_off.max(last_down), events);
    }
    Ok(())
}

// Explicit imported/edited MIDI values override descriptive drum names and
// coarse dynamics. Articulation velocity shaping still follows this base.
pub(super) fn hit_timing_offset(hit: &HitEvent) -> Result<f64, String> {
    let mut values = hit
        .modifiers
        .iter()
        .filter(|(key, _)| key == "timing_offset_beats");
    let Some((_, value)) = values.next() else {
        return Ok(0.0);
    };
    let local = hit.written_quarters();
    match value {
        ParamValue::Number(offset)
            if values.next().is_none()
                && offset.is_finite()
                && (-0.125..=0.125).contains(offset)
                && local + offset >= 0.0 =>
        {
            Ok(*offset)
        }
        _ => Err("invalid or duplicate drum timing_offset_beats".into()),
    }
}

/// A hit's loudness: the typed `v=NN`, then the imported `velocity=` modifier,
/// then the written dynamic. The first value wins, as it does for a note.
pub(super) fn hit_velocity(hit: &HitEvent) -> u8 {
    hit.velocity
        .or_else(|| hit_midi_value(hit, "velocity", 1))
        .unwrap_or_else(|| hit.dynamics.map(|d| d.to_velocity()).unwrap_or(100))
}

pub(super) fn hit_midi_value(hit: &HitEvent, key: &str, minimum: u8) -> Option<u8> {
    hit.modifiers.iter().find_map(|(name, value)| {
        if name != key {
            return None;
        }
        match value {
            ParamValue::Number(number)
                if number.is_finite()
                    && number.fract() == 0.0
                    && *number >= f64::from(minimum)
                    && *number <= 127.0 =>
            {
                Some(*number as u8)
            }
            _ => None,
        }
    })
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_drum_hit(
    hit: &HitEvent,
    note: u8,
    velocity: u8,
    track_index: usize,
    hit_time: u64,
    off_time: u64,
    profile: &InstrumentProfile,
    drum_choke_notes: &mut HashMap<String, Vec<u8>>,
    events: &mut Vec<MidiEvent>,
) {
    if let Some(group) =
        profile.drum_choke_group(&hit.drum, hit.articulation.map(drum_articulation_key))
    {
        let choke_release = profile.drum_choke_release_samples(
            &hit.drum,
            hit.articulation.map(drum_articulation_key),
            group,
        );
        for active_note in drum_choke_notes.remove(group).unwrap_or_default() {
            events.push(MidiEvent {
                time_samples: hit_time.saturating_add(choke_release),
                event: MidiMessage::NoteOff {
                    channel: 9,
                    note: active_note,
                },
                track_index,
            });
        }
        drum_choke_notes
            .entry(group.to_string())
            .or_default()
            .push(note);
    }

    events.push(MidiEvent {
        time_samples: hit_time,
        event: MidiMessage::NoteOn {
            channel: 9,
            note,
            velocity,
        },
        track_index,
    });
    events.push(MidiEvent {
        time_samples: off_time,
        event: MidiMessage::NoteOff { channel: 9, note },
        track_index,
    });
}
