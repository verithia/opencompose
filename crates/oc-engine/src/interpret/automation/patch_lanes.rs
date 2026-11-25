//! Bounded patch-parameter ramps retain their authored curve across clip trims.
use super::{
    AutomationLane, Curve, InterpretSettings, MidiEvent, MidiMessage, automation_quarter, lane_span,
};
use oc_dsl::timing::ProjectClock;

const STEP_SAMPLES: u64 = 64;
const MAX_LANE_EVENTS: usize = 262_144;
const MAX_EVENT_NAME_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct RampContext<'a> {
    pub clock: &'a ProjectClock,
    pub settings: &'a InterpretSettings,
    pub normalization: crate::automation::PatchNormalization,
    pub track_index: usize,
}

struct Point {
    sample: u64,
    value: f64,
    curve: Curve,
    tension: f64,
    curvature: f64,
}

fn value_at(points: &[Point], sample: u64) -> f64 {
    let index = points.partition_point(|point| point.sample <= sample);
    if index == 0 {
        return points[0].value;
    }
    let from = &points[index - 1];
    let Some(to) = points.get(index) else {
        return from.value;
    };
    if from.curve == Curve::Stairs {
        return from.value;
    }
    let position = (sample - from.sample) as f64 / (to.sample - from.sample) as f64;
    from.value
        + (to.value - from.value)
            * oc_dsl::automation::segment_shape(from.curve, from.tension, from.curvature, position)
}

pub(super) fn push(
    param: &str,
    lane: &AutomationLane,
    context: RampContext<'_>,
    events: &mut Vec<MidiEvent>,
) -> Result<(), String> {
    if lane.points.len() > MAX_LANE_EVENTS {
        return Err("patch parameter lane exceeds its point budget".into());
    }
    let mut points = Vec::with_capacity(lane.points.len());
    for point in &lane.points {
        let Some(value) =
            crate::automation::patch_point_value(context.normalization, param, &point.value)
        else {
            continue;
        };
        points.push(Point {
            sample: context.clock.sample_at_quarter(
                automation_quarter(context.clock, lane, &point.position)?,
                context.settings.sample_rate,
            )?,
            value,
            curve: point.curve,
            tension: point.tension.unwrap_or(1.0),
            curvature: point.curvature.unwrap_or(0.0),
        });
    }
    points.sort_by_key(|point| point.sample);
    let Some(first) = points.first() else {
        return Ok(());
    };
    let last = points.last().expect("nonempty point list");
    let (start, end) = match lane_span(lane, context.clock, context.settings.sample_rate)? {
        Some(span) => span,
        None => (
            first.sample,
            last.sample
                .checked_add(1)
                .ok_or("patch parameter time exceeds its sample range")?,
        ),
    };
    // Stage the complete lane so admission failure cannot append partial events.
    let mut prepared: Vec<(u64, f64)> = Vec::new();
    let mut append = |sample: u64, value: f64| -> Result<(), String> {
        if sample < start || sample >= end {
            return Ok(());
        }
        if let Some(previous) = prepared.last_mut()
            && previous.0 == sample
        {
            previous.1 = value;
            return Ok(());
        }
        if prepared.len() >= MAX_LANE_EVENTS {
            return Err("patch parameter lane exceeds its event budget".into());
        }
        prepared.push((sample, value));
        Ok(())
    };
    append(start, value_at(&points, start))?;
    for pair in points.windows(2) {
        let from = &pair[0];
        let to = &pair[1];
        if to.sample <= from.sample || to.sample <= start || from.sample >= end {
            continue;
        }
        if from.curve == Curve::Stairs || from.value == to.value {
            append(from.sample, from.value)?;
            continue;
        }
        let active_start = from.sample.max(start);
        let active_end = to.sample.min(end);
        let span = active_end - active_start;
        let steps = span.div_ceil(STEP_SAMPLES).max(1);
        if steps > MAX_LANE_EVENTS as u64 {
            return Err("patch parameter lane exceeds its event budget".into());
        }
        for step in 0..steps {
            // Wide arithmetic keeps an untrusted sample span from overflowing.
            let offset = (u128::from(span) * u128::from(step) / u128::from(steps)) as u64;
            let sample = active_start + offset;
            if sample < start || sample >= end {
                continue;
            }
            append(sample, value_at(&points, sample))?;
        }
    }
    append(last.sample, last.value)?;
    // The endpoint shapes the last segment but never writes beyond the clip.
    append(end - 1, value_at(&points, end - 1))?;
    if prepared
        .len()
        .checked_mul(param.len())
        .is_none_or(|bytes| bytes > MAX_EVENT_NAME_BYTES)
    {
        return Err("patch parameter lane exceeds its parameter-name budget".into());
    }
    events
        .try_reserve(prepared.len())
        .map_err(|_| "could not reserve patch parameter events".to_string())?;
    events.extend(prepared.into_iter().map(|(time_samples, value)| MidiEvent {
        time_samples,
        event: MidiMessage::PluginParam {
            name: param.to_string(),
            value,
        },
        track_index: context.track_index,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(lane: &str) -> oc_dsl::ast::Project {
        oc_dsl::parse(&format!("@project\n  tempo: 120\n@preset hosted\n  source: vst3\n  plugin: \"example.instrument\"\n@track lead | instrument: synth/lead_saw, preset: hosted\n  automation: pluginparam_4761696e {lane}\n")).unwrap()
    }

    fn prepare(
        project: &oc_dsl::ast::Project,
        param: &str,
        events: &mut Vec<MidiEvent>,
    ) -> Result<(), String> {
        let clock = ProjectClock::new(&project.properties)?;
        let settings = InterpretSettings {
            sample_rate: 8_000,
            ..Default::default()
        };
        push(
            param,
            project.tracks[0].automation().next().unwrap(),
            RampContext {
                clock: &clock,
                settings: &settings,
                normalization: crate::automation::PatchNormalization {
                    builtin: false,
                    recalibrate_legacy: false,
                },
                track_index: 0,
            },
            events,
        )
    }

    #[test]
    fn stairs_hold_the_value_until_the_authored_point() {
        let project = project("[1:1 - 1:3] {1:1=0 curve=stairs, 1:3=1}");
        let mut events = Vec::new();
        prepare(&project, "Gain", &mut events).unwrap();
        assert!(!events.is_empty());
        assert!(events.iter().all(|e| e.time_samples < 8_000));
        assert!(
            events
                .iter()
                .all(|e| matches!(e.event, MidiMessage::PluginParam { value: 0.0, .. }))
        );
    }

    #[test]
    fn compact_exponential_patch_events_follow_the_shared_curve_after_trim() {
        for curvature in [-4.0, 0.0, 4.0] {
            let project = project(&format!(
                "[1:2 - 1:4] {{1:1=0 curve=exponential curvature={curvature}, 2:1=1}}"
            ));
            let mut events = Vec::new();
            prepare(&project, "Gain", &mut events).unwrap();
            let interpreted = crate::interpret::try_interpret(
                &project,
                &InterpretSettings {
                    sample_rate: 8_000,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(interpreted.iter().any(|event| matches!(
                &event.event,
                MidiMessage::PluginParam { name, .. } if name == "Gain"
            )));
            assert!(!events.is_empty());
            assert!(
                events
                    .iter()
                    .all(|event| (4_000..12_000).contains(&event.time_samples))
            );
            for event in events {
                let MidiMessage::PluginParam { value, .. } = event.event else {
                    panic!("expected a parameter event");
                };
                let phase = event.time_samples as f64 / 16_000.0;
                let expected =
                    oc_dsl::automation::segment_shape(Curve::Exponential, 1.0, curvature, phase);
                assert!((value - expected).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn compact_patch_jump_keeps_the_left_curve_and_right_boundary_value() {
        let mut project = project(
            "[1:1 - 1:3] {1:1=0 curve=exponential curvature=2, 1:2=1, 1:2=0 curve=exponential curvature=-2, 1:3=1}",
        );
        project.tracks[0]
            .automation_mut()
            .next()
            .unwrap()
            .preserve_discontinuities = true;
        let mut events = Vec::new();
        prepare(&project, "Gain", &mut events).unwrap();
        let value = |event: &MidiEvent| match event.event {
            MidiMessage::PluginParam { value, .. } => value,
            _ => panic!("expected a parameter event"),
        };
        let before = events
            .iter()
            .rev()
            .find(|event| event.time_samples < 4_000)
            .unwrap();
        assert!(value(before) > 0.9);
        let boundary = events
            .iter()
            .find(|event| event.time_samples == 4_000)
            .unwrap();
        assert_eq!(value(boundary), 0.0);
        assert!(events.iter().all(|event| event.time_samples < 8_000));
    }

    #[test]
    fn a_long_compact_segment_keeps_the_control_update_interval_bounded() {
        let project = project("[1:1 - 11:1] {1:1=0 curve=exponential curvature=4, 11:1=1}");
        let mut events = Vec::new();
        prepare(&project, "Gain", &mut events).unwrap();
        assert!(events.len() > 2048);
        assert_eq!(events.first().unwrap().time_samples, 0);
        assert_eq!(events.last().unwrap().time_samples, 159_999);
        assert!(events.windows(2).all(|pair| {
            pair[1].time_samples > pair[0].time_samples
                && pair[1].time_samples - pair[0].time_samples <= STEP_SAMPLES
        }));
    }

    #[test]
    fn trimming_a_long_curve_keeps_updates_inside_the_short_active_window() {
        let project = project("[1:2 - 1:4] {1:1=0, 1000:1=1}");
        let mut events = Vec::new();
        prepare(&project, "Gain", &mut events).unwrap();
        assert!(events.len() > 100);
        assert!(
            events
                .windows(2)
                .all(|pair| pair[1].time_samples - pair[0].time_samples <= 128)
        );
        assert!(
            events
                .iter()
                .all(|e| (4_000..12_000).contains(&e.time_samples))
        );
    }

    #[test]
    fn anchors_at_the_same_sample_keep_the_last_authored_value() {
        let project = project("[1:1 - 1:2] [ppq=96000] {1:1=0, 1:1:1=1, 1:2=0}");
        let mut events = Vec::new();
        prepare(&project, "Gain", &mut events).unwrap();
        assert_eq!(events[0].time_samples, 0);
        assert!(matches!(
            events[0].event,
            MidiMessage::PluginParam { value: 1.0, .. }
        ));
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].time_samples < pair[1].time_samples)
        );
    }

    #[test]
    fn an_oversized_parameter_name_budget_leaves_existing_events_unchanged() {
        let project = project("{1:1=0, 2:1=1}");
        let mut events = vec![MidiEvent {
            time_samples: 123,
            track_index: 7,
            event: MidiMessage::PluginParam {
                name: "existing".into(),
                value: 0.5,
            },
        }];
        assert!(prepare(&project, &"x".repeat(128 * 1024), &mut events).is_err());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_samples, 123);
        assert_eq!(events[0].track_index, 7);
        assert!(
            matches!(&events[0].event, MidiMessage::PluginParam { name, value: 0.5 } if name == "existing")
        );
    }
}
