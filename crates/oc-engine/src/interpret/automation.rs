//! Track automation, sustain and pedal timeline.

use super::*;

pub(super) fn automation_quarter(
    clock: &oc_dsl::timing::ProjectClock,
    lane: &oc_dsl::ast::AutomationLane,
    position: &oc_dsl::ast::BarBeat,
) -> Result<f64, String> {
    oc_dsl::automation::lane_quarter(clock, lane, position)
}

/// Sample span a clip writes in; `None` keeps the legacy whole-timeline lane.
///
/// The end is exclusive, so the clip stops writing there and the last value it
/// wrote stays on the parameter.
fn lane_span(
    lane: &AutomationLane,
    clock: &oc_dsl::timing::ProjectClock,
    sample_rate: u32,
) -> Result<Option<(u64, u64)>, String> {
    let Some(range) = &lane.time_range else {
        return Ok(None);
    };
    let start =
        clock.sample_at_quarter(automation_quarter(clock, lane, &range.start)?, sample_rate)?;
    let end = clock.sample_at_quarter(automation_quarter(clock, lane, &range.end)?, sample_rate)?;
    if end <= start {
        return Err(format!(
            "automation lane '{}' must end after its start",
            lane.param
        ));
    }
    Ok(Some((start, end)))
}

/// A lane's points with its clip applied, as sample positions.
///
/// A clip writes nothing outside its span. When its first point sits after its
/// head, the first value is written at the head, so the clip opens on the value
/// it was authored with instead of waiting for the first anchor.
fn lane_points<'a>(
    lane: &'a AutomationLane,
    clock: &oc_dsl::timing::ProjectClock,
    sample_rate: u32,
) -> Result<Vec<(u64, &'a AutomationPoint)>, String> {
    let span = lane_span(lane, clock, sample_rate)?;
    let mut points = Vec::with_capacity(lane.points.len());
    for point in &lane.points {
        let sample = clock.sample_at_quarter(
            automation_quarter(clock, lane, &point.position)?,
            sample_rate,
        )?;
        if span.is_some_and(|(start, end)| sample < start || sample >= end) {
            continue;
        }
        points.push((sample, point));
    }
    if let Some((start, _)) = span
        && let Some((sample, point)) = points.iter().copied().min_by_key(|(sample, _)| *sample)
        && sample > start
    {
        points.push((start, point));
    }
    Ok(points)
}

/// How the interpreter reads one track's patch-parameter lanes.
#[derive(Clone, Copy, Default)]
pub(super) struct SynthLanes {
    /// The track plays `builtin.open_synth`, so a unit is the patch unit.
    pub(super) builtin: bool,
    /// The document predates the widened normalized table.
    pub(super) recalibrate_legacy: bool,
}

mod patch_lanes;

pub(super) struct AutomationContext<'a> {
    pub channel: u8,
    pub track_index: usize,
    pub clock: &'a oc_dsl::timing::ProjectClock,
    pub settings: &'a InterpretSettings,
    pub profile: &'a InstrumentProfile,
    pub synth_lanes: SynthLanes,
}

pub(super) fn interpret_automation(
    track: &Track,
    context: AutomationContext<'_>,
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    let AutomationContext {
        channel,
        track_index,
        clock,
        settings,
        profile,
        synth_lanes,
    } = context;
    let normalization = crate::automation::PatchNormalization {
        builtin: synth_lanes.builtin,
        recalibrate_legacy: synth_lanes.recalibrate_legacy,
    };
    for lane in track.automation() {
        if lane.muted || lane.effect_lane().is_some() {
            continue;
        }
        let encoded_param = decode_plugin_automation_param(&lane.param);
        let patch_param = encoded_param
            .clone()
            .or_else(|| profile.control_param(&lane.param).map(str::to_string));
        if let Some(param_name) = &patch_param
            && ((synth_lanes.builtin && oc_synth::synth::patch_param_is_continuous(param_name))
                || (!synth_lanes.builtin && encoded_param.is_some()))
        {
            patch_lanes::push(
                param_name,
                lane,
                patch_lanes::RampContext {
                    clock,
                    settings,
                    normalization,
                    track_index,
                },
                events,
            )?;
            continue;
        }
        let mut previous_value = 0.0;
        for (time_samples, point) in lane_points(lane, clock, settings.sample_rate)? {
            if let Some(value) = param_value_to_normalized(&point.value) {
                emit_control_noise_trigger(
                    &lane.param,
                    previous_value,
                    value,
                    profile,
                    channel,
                    track_index,
                    time_samples,
                    events,
                );
                previous_value = value;
            }
            if let Some(param_name) = &patch_param {
                if let Some(value) =
                    crate::automation::patch_point_value(normalization, param_name, &point.value)
                {
                    events.push(MidiEvent {
                        time_samples,
                        event: MidiMessage::PluginParam {
                            name: param_name.clone(),
                            value,
                        },
                        track_index,
                    });
                }
            } else if let Some(controller) = profile.control_cc14(&lane.param) {
                if let Some(value) = param_value_to_midi14(&point.value) {
                    events.push(MidiEvent {
                        time_samples,
                        event: MidiMessage::ControlChange14 {
                            channel,
                            controller,
                            value,
                        },
                        track_index,
                    });
                }
            } else if let Some(controller) = profile
                .control_cc(&lane.param)
                .or_else(|| automation_param_to_cc(&lane.param))
            {
                if let Some(value) = param_value_to_midi(&point.value) {
                    events.push(MidiEvent {
                        time_samples,
                        event: MidiMessage::ControlChange {
                            channel,
                            controller,
                            value,
                        },
                        track_index,
                    });
                }
            } else if matches!(
                normalize_param_name(&lane.param).as_str(),
                "pressure" | "aftertouch" | "channelpressure"
            ) {
                if let Some(pressure) = param_value_to_midi(&point.value) {
                    events.push(MidiEvent {
                        time_samples,
                        event: MidiMessage::ChannelPressure { channel, pressure },
                        track_index,
                    });
                }
            } else if matches!(lane.param.as_str(), "pitch_bend" | "bend")
                && let Some(value) = point.value.as_f64()
            {
                events.push(MidiEvent {
                    time_samples,
                    event: MidiMessage::PitchBend {
                        channel,
                        value: cents_to_pitch_bend(value, settings.pitch_bend_range),
                    },
                    track_index,
                });
            }
        }
    }
    Ok(())
}

pub(super) fn sustain_timeline_for_track(
    track: &Track,
    clock: &oc_dsl::timing::ProjectClock,
    settings: &InterpretSettings,
    profile: &InstrumentProfile,
) -> Result<Vec<SustainPoint>, String> {
    let mut points = Vec::new();
    for lane in track.automation() {
        if lane.muted || lane.effect_lane().is_some() {
            continue;
        }
        let controller = profile
            .control_cc(&lane.param)
            .or_else(|| automation_param_to_cc(&lane.param));
        if controller != Some(64) {
            continue;
        }
        for (time_samples, point) in lane_points(lane, clock, settings.sample_rate)? {
            let Some(value) = param_value_to_normalized(&point.value) else {
                continue;
            };
            points.push(SustainPoint {
                time_samples,
                value,
            });
        }
    }
    points.sort_by_key(|point| point.time_samples);
    Ok(points)
}

pub(super) fn sustain_release_time(
    note_off_time: u64,
    sustain_timeline: &[SustainPoint],
) -> Option<u64> {
    let mut held = false;
    for point in sustain_timeline {
        if point.time_samples > note_off_time {
            break;
        }
        held = point.value >= 0.5;
    }
    if !held {
        return Some(note_off_time);
    }

    sustain_timeline
        .iter()
        .find(|point| point.time_samples > note_off_time && point.value < 0.5)
        .map(|point| point.time_samples)
}
