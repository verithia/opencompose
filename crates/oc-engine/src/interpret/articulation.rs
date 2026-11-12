//! Expression, articulation and control-noise events.

use super::*;

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_control_noise_trigger(
    control: &str,
    previous_value: f64,
    value: f64,
    profile: &InstrumentProfile,
    channel: u8,
    track_index: usize,
    time_samples: u64,
    events: &mut Vec<MidiEvent>,
) {
    let Some(trigger) = profile.control_noise_trigger(control) else {
        return;
    };
    let entered_range = match (trigger.range_min, trigger.range_max) {
        (Some(min), Some(max)) => {
            let low = min.min(max);
            let high = max.max(min);
            !(low..=high).contains(&previous_value) && (low..=high).contains(&value)
        }
        _ => false,
    };
    let note =
        if entered_range || (previous_value < trigger.threshold && value >= trigger.threshold) {
            trigger.on_note
        } else if previous_value >= trigger.threshold && value < trigger.threshold {
            trigger.off_note
        } else {
            None
        };
    let Some(note) = note else {
        return;
    };

    events.push(MidiEvent {
        time_samples,
        event: MidiMessage::NoteOn {
            channel,
            note,
            velocity: trigger.velocity,
        },
        track_index,
    });
    events.push(MidiEvent {
        time_samples: time_samples + trigger.duration_samples,
        event: MidiMessage::NoteOff { channel, note },
        track_index,
    });
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_note_expression_events(
    note_event: &NoteEvent,
    voice_indices: (usize, usize),
    identified: bool,
    channel: u8,
    note: u8,
    track_index: usize,
    start_beat: f64,
    duration_beats: f64,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    profile: &InstrumentProfile,
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    let onset_sample = events[voice_indices.0].time_samples;
    let note_off_sample = events[voice_indices.1].time_samples;

    let curve_tension = oc_synth::note_pitch_curve::interpolation(&note_event.modifiers)?;
    let points = oc_synth::note_pitch_curve::parse(&note_event.modifiers, duration_beats)?;
    if points.is_some()
        && (note_event.pitch_bend.is_some()
            || note_event.vibrato.is_some()
            || note_event.expression.is_some())
    {
        return Err(
            "pitch_curve cannot combine with channel bend/vibrato/other note expressions".into(),
        );
    }
    // A curve track identifies ordinary notes at a curve's pitch as well, so a
    // plain note-off can never release the curve voice.
    if identified || points.is_some() {
        let token = voice_indices.0 as u64;
        let MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } = events[voice_indices.0].event
        else {
            return Err("curve onset is not a note".into());
        };
        events[voice_indices.0].event =
            MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::On {
                token,
                channel,
                note,
                velocity,
            });
        events[voice_indices.1].event =
            MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::Off { token });
    }
    if let Some(points) = points {
        let horizon = oc_synth::note_pitch_curve::horizon(&note_event.modifiers, duration_beats)?;
        let declared_first = clock.sample_at_quarter(start_beat, settings.sample_rate)?;
        let declared_last = clock.sample_at_quarter(start_beat + horizon, settings.sample_rate)?;
        let first = events[voice_indices.0].time_samples;
        let last = first
            .checked_add(declared_last - declared_first)
            .ok_or("curve horizon overflow")?;
        let token = voice_indices.0 as u64;
        let stride = (settings.sample_rate / 200).max(1) as u64;
        if (last - first) / stride > 120000 {
            return Err("pitch_curve exceeds bounded duration".into());
        }
        // A span with one tempo is linear in samples; a mapped span asks the
        // clock for each frame's quarter position.
        let constant_tempo = !clock.has_tempo_change_between(start_beat, start_beat + horizon);
        for sample in (first..last)
            .step_by(stride as usize)
            .chain(std::iter::once(last))
        {
            let relative = if sample == last {
                horizon
            } else if constant_tempo {
                (sample - first) as f64 / (last - first).max(1) as f64 * horizon
            } else {
                (clock.quarter_at_seconds(sample as f64 / f64::from(settings.sample_rate))?
                    - start_beat)
                    .clamp(0.0, horizon)
            };
            let cents = oc_synth::note_pitch_curve::value_at(&points, relative, curve_tension);
            if !cents.is_finite() || cents.abs() > oc_synth::note_pitch_curve::MAX_SEMITONES * 100.0
            {
                return Err("pitch_curve interpolation exceeds native range".into());
            }
            events.push(MidiEvent {
                time_samples: sample,
                track_index,
                event: MidiMessage::NativeVoice(oc_synth::synth::NativeVoiceEvent::Pitch {
                    token,
                    semitones: cents / 100.0,
                }),
            });
        }
    }

    if note_event.pitch_bend.is_none()
        && note_event.expression.is_none()
        && note_event.vibrato.is_none()
    {
        return Ok(());
    }
    // Physical articulation overlap may extend note-off beyond the intended
    // expression span. A trimmed gate can instead shorten that span.
    let authored_start = clock.sample_at_quarter(start_beat, settings.sample_rate)?;
    let authored_end =
        clock.sample_at_quarter(start_beat + duration_beats, settings.sample_rate)?;
    let release_sample = onset_sample
        .saturating_add(authored_end.saturating_sub(authored_start))
        .min(note_off_sample);

    if let Some(cents) = note_event.pitch_bend {
        events.push(MidiEvent {
            time_samples: onset_sample,
            event: MidiMessage::PitchBend {
                channel,
                value: cents_to_pitch_bend(cents, settings.pitch_bend_range),
            },
            track_index,
        });
        events.push(MidiEvent {
            time_samples: release_sample,
            event: MidiMessage::PitchBend { channel, value: 0 },
            track_index,
        });
    }

    if let Some(curves) = &note_event.expression {
        for curve in curves {
            if let Some(expression) = profile.control_note_expression(&curve.param) {
                if let Some(value) = param_value_to_normalized(&curve.start) {
                    events.push(MidiEvent {
                        time_samples: onset_sample,
                        event: MidiMessage::NoteExpression {
                            channel,
                            note,
                            expression: expression.to_string(),
                            value,
                        },
                        track_index,
                    });
                }
                if let Some(end) = &curve.end
                    && let Some(value) = param_value_to_normalized(end)
                {
                    events.push(MidiEvent {
                        time_samples: release_sample,
                        event: MidiMessage::NoteExpression {
                            channel,
                            note,
                            expression: expression.to_string(),
                            value,
                        },
                        track_index,
                    });
                }
                continue;
            }

            if let Some(param_name) = profile.control_param(&curve.param) {
                if let Some(value) = param_value_to_normalized(&curve.start) {
                    events.push(MidiEvent {
                        time_samples: onset_sample,
                        event: MidiMessage::PluginParam {
                            name: param_name.to_string(),
                            value,
                        },
                        track_index,
                    });
                }
                if let Some(end) = &curve.end
                    && let Some(value) = param_value_to_normalized(end)
                {
                    events.push(MidiEvent {
                        time_samples: release_sample,
                        event: MidiMessage::PluginParam {
                            name: param_name.to_string(),
                            value,
                        },
                        track_index,
                    });
                }
                continue;
            }

            if matches!(
                normalize_param_name(&curve.param).as_str(),
                "pressure" | "aftertouch" | "channelpressure"
            ) {
                if let Some(value) = param_value_to_midi(&curve.start) {
                    events.push(MidiEvent {
                        time_samples: onset_sample,
                        event: MidiMessage::ChannelPressure {
                            channel,
                            pressure: value,
                        },
                        track_index,
                    });
                }
                if let Some(end) = &curve.end
                    && let Some(value) = param_value_to_midi(end)
                {
                    events.push(MidiEvent {
                        time_samples: release_sample,
                        event: MidiMessage::ChannelPressure {
                            channel,
                            pressure: value,
                        },
                        track_index,
                    });
                }
                continue;
            }

            let Some(controller) = profile
                .control_cc(&curve.param)
                .or_else(|| expression_param_to_cc(&curve.param))
            else {
                continue;
            };
            if let Some(value) = param_value_to_midi(&curve.start) {
                events.push(MidiEvent {
                    time_samples: onset_sample,
                    event: MidiMessage::ControlChange {
                        channel,
                        controller,
                        value,
                    },
                    track_index,
                });
            }
            if let Some(end) = &curve.end
                && let Some(value) = param_value_to_midi(end)
            {
                events.push(MidiEvent {
                    time_samples: release_sample,
                    event: MidiMessage::ControlChange {
                        channel,
                        controller,
                        value,
                    },
                    track_index,
                });
            }
        }
    }

    if let Some(vibrato) = &note_event.vibrato {
        if vibrato.rate_hz.is_some_and(|v| !v.is_finite() || v <= 0.0)
            || vibrato.delay_s.is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err("vibrato rate must be finite/positive and delay finite/nonnegative".into());
        }
        let rate_hz = vibrato.rate_hz.unwrap_or(5.0).max(0.1);
        let depth_cents = vibrato.depth_cents.unwrap_or(25.0);
        let rate = f64::from(settings.sample_rate);
        let start_seconds = vibrato.delay_s.unwrap_or(0.0).max(0.0);
        let end_seconds = (release_sample - onset_sample) as f64 / rate;
        if !rate_hz.is_finite() || !start_seconds.is_finite() {
            return Err("non-finite vibrato timing".into());
        }
        if start_seconds < end_seconds {
            // Hz and delay_s are physical time, unaffected by tempo changes.
            let step_seconds = (1.0 / rate_hz / 4.0).max(1.0 / f64::from(settings.sample_rate));
            let mut seconds = start_seconds;
            let mut sign = 1.0;
            while seconds < end_seconds {
                events.push(MidiEvent {
                    time_samples: onset_sample + (seconds * rate) as u64,
                    event: MidiMessage::PitchBend {
                        channel,
                        value: cents_to_pitch_bend(depth_cents * sign, settings.pitch_bend_range),
                    },
                    track_index,
                });
                sign = -sign;
                seconds += step_seconds;
            }
            events.push(MidiEvent {
                time_samples: release_sample,
                event: MidiMessage::PitchBend { channel, value: 0 },
                track_index,
            });
        }
    }
    Ok(())
}

pub(super) fn emit_profile_note_tuning(
    profile: &InstrumentProfile,
    channel: u8,
    note: u8,
    track_index: usize,
    time_samples: u64,
    events: &mut Vec<MidiEvent>,
) {
    let Some(cents) = profile.note_tuning_cents(note) else {
        return;
    };
    events.push(MidiEvent {
        time_samples,
        event: MidiMessage::NoteExpression {
            channel,
            note,
            expression: "tuning".to_string(),
            value: cents / 100.0,
        },
        track_index,
    });
}

pub(super) fn emit_articulation_profile_events(
    articulation: Option<Articulation>,
    occurrence: usize,
    profile: &InstrumentProfile,
    channel: u8,
    track_index: usize,
    note_on_time: u64,
    events: &mut Vec<MidiEvent>,
) {
    let Some(articulation) = articulation else {
        return;
    };
    let duration = profile.keyswitch_duration_samples.max(1);
    let start = note_on_time.saturating_sub(duration);
    if let Some(note) = profile.keyswitch_for_articulation_occurrence(articulation, occurrence) {
        events.push(MidiEvent {
            time_samples: start,
            event: MidiMessage::NoteOn {
                channel,
                note,
                velocity: 1,
            },
            track_index,
        });
        events.push(MidiEvent {
            time_samples: start + duration,
            event: MidiMessage::NoteOff { channel, note },
            track_index,
        });
    }

    if let Some(control) = profile.articulation_control(articulation) {
        events.push(MidiEvent {
            time_samples: start,
            event: MidiMessage::ControlChange {
                channel,
                controller: control.controller,
                value: control.value,
            },
            track_index,
        });
    }

    if let Some(param) = profile.articulation_param(articulation)
        && !param.name.is_empty()
    {
        events.push(MidiEvent {
            time_samples: start,
            event: MidiMessage::PluginParam {
                name: param.name.clone(),
                value: param.value,
            },
            track_index,
        });
    }
}

pub(super) fn next_articulation_occurrence(
    articulation: Option<Articulation>,
    articulation_occurrences: &mut HashMap<String, usize>,
) -> usize {
    let Some(articulation) = articulation else {
        return 0;
    };
    let key = format!("{articulation:?}");
    let occurrence = articulation_occurrences.entry(key).or_default();
    let current = *occurrence;
    *occurrence += 1;
    current
}

pub(super) fn emit_articulation_profile_resets(
    articulation: Option<Articulation>,
    profile: &InstrumentProfile,
    channel: u8,
    track_index: usize,
    time_samples: u64,
    events: &mut Vec<MidiEvent>,
) {
    let Some(articulation) = articulation else {
        return;
    };

    if let Some(control) = profile.articulation_control(articulation)
        && let Some(value) = control.reset_value
    {
        events.push(MidiEvent {
            time_samples,
            event: MidiMessage::ControlChange {
                channel,
                controller: control.controller,
                value,
            },
            track_index,
        });
    }

    if let Some(param) = profile.articulation_param(articulation)
        && let Some(value) = param.reset_value
        && !param.name.is_empty()
    {
        events.push(MidiEvent {
            time_samples,
            event: MidiMessage::PluginParam {
                name: param.name.clone(),
                value,
            },
            track_index,
        });
    }
}

pub(super) fn emit_articulation_release_trigger(
    articulation: Option<Articulation>,
    profile: &InstrumentProfile,
    sustain_timeline: &[SustainPoint],
    channel: u8,
    track_index: usize,
    time_samples: u64,
    events: &mut Vec<MidiEvent>,
) {
    let Some(articulation) = articulation else {
        return;
    };
    let Some(trigger) = profile.articulation_release_trigger(articulation) else {
        return;
    };
    if trigger.note == 0 || trigger.duration_samples == 0 {
        return;
    }
    let time_samples = if trigger.sustain_aware {
        let Some(time_samples) = sustain_release_time(time_samples, sustain_timeline) else {
            return;
        };
        time_samples
    } else {
        time_samples
    };

    events.push(MidiEvent {
        time_samples,
        event: MidiMessage::NoteOn {
            channel,
            note: trigger.note,
            velocity: trigger.velocity,
        },
        track_index,
    });
    events.push(MidiEvent {
        time_samples: time_samples + trigger.duration_samples,
        event: MidiMessage::NoteOff {
            channel,
            note: trigger.note,
        },
        track_index,
    });
}
