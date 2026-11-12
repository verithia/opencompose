//! Ordered channel commands. Unlike note spans, these do not infer voice pairing.
//!
//! This is an explicit event-editing representation, not a second hidden note
//! renderer. Track integration must choose either this timeline or note-derived
//! commands, never silently play both. Source order breaks equal-time ties.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Sorted MIDI performance events on a project clock.
pub struct PerformanceTimeline {
    pub events: Vec<PerformanceEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// One channel command at an absolute quarter-note position.
pub struct PerformanceEvent {
    /// Absolute quarter notes, integrated through the owning project's clock.
    pub quarter: f64,
    /// Original zero-based MIDI channel, independent of the GUI track index.
    pub channel: u8,
    pub message: ChannelCommand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// A MIDI channel command.
pub enum ChannelCommand {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8, velocity: u8 },
    ControlChange { controller: u8, value: u8 },
    ProgramChange { program: u8 },
    PitchBend { value: i16 },
    ChannelPressure { pressure: u8 },
    PolyPressure { note: u8, pressure: u8 },
}

impl PerformanceTimeline {
    /// Do not sort, clamp, discard redundant releases, or fabricate track-end
    /// releases. Such edits change the performance and must be explicit.
    pub fn validate(&self) -> Result<(), String> {
        let mut previous = 0.0;
        for (index, event) in self.events.iter().enumerate() {
            if !event.quarter.is_finite() || event.quarter < previous || event.channel > 15 {
                return Err(format!(
                    "invalid performance position/channel at event {index}"
                ));
            }
            previous = event.quarter;
            let valid = match event.message {
                ChannelCommand::NoteOn { note, velocity }
                | ChannelCommand::NoteOff { note, velocity } => note < 128 && velocity < 128,
                ChannelCommand::ControlChange { controller, value } => {
                    controller < 128 && value < 128
                }
                ChannelCommand::ProgramChange { program } => program < 128,
                ChannelCommand::PitchBend { value } => (-8192..=8191).contains(&value),
                ChannelCommand::ChannelPressure { pressure } => pressure < 128,
                ChannelCommand::PolyPressure { note, pressure } => note < 128 && pressure < 128,
            };
            if !valid {
                return Err(format!("invalid performance message at event {index}"));
            }
        }
        Ok(())
    }
}

pub(crate) fn parse_event_fields(fields: &[String]) -> Result<PerformanceEvent, String> {
    if !(4..=5).contains(&fields.len()) {
        return Err("invalid performance event arity".into());
    }
    let quarter = fields[0].parse::<f64>().map_err(|_| "invalid quarter")?;
    let channel = fields[1].parse::<u8>().map_err(|_| "invalid channel")?;
    let a = fields[3]
        .parse::<i16>()
        .map_err(|_| "invalid command value")?;
    let b = fields
        .get(4)
        .map(|s| s.parse::<u8>().map_err(|_| "invalid second value"))
        .transpose()?;
    let u = || u8::try_from(a).map_err(|_| "invalid byte value");
    let message = match (fields[2].as_str(), b) {
        ("note_on", Some(velocity)) => ChannelCommand::NoteOn {
            note: u()?,
            velocity,
        },
        ("note_off", Some(velocity)) => ChannelCommand::NoteOff {
            note: u()?,
            velocity,
        },
        ("cc", Some(value)) => ChannelCommand::ControlChange {
            controller: u()?,
            value,
        },
        ("program", None) => ChannelCommand::ProgramChange { program: u()? },
        ("bend", None) => ChannelCommand::PitchBend { value: a },
        ("pressure", None) => ChannelCommand::ChannelPressure { pressure: u()? },
        ("poly_pressure", Some(pressure)) => ChannelCommand::PolyPressure {
            note: u()?,
            pressure,
        },
        _ => return Err("unknown performance command or invalid arity".into()),
    };
    Ok(PerformanceEvent {
        quarter,
        channel,
        message,
    })
}

pub(crate) fn format_command(message: &ChannelCommand) -> String {
    match message {
        ChannelCommand::NoteOn { note, velocity } => format!("note_on {note} {velocity}"),
        ChannelCommand::NoteOff { note, velocity } => format!("note_off {note} {velocity}"),
        ChannelCommand::ControlChange { controller, value } => format!("cc {controller} {value}"),
        ChannelCommand::ProgramChange { program } => format!("program {program}"),
        ChannelCommand::PitchBend { value } => format!("bend {value}"),
        ChannelCommand::ChannelPressure { pressure } => format!("pressure {pressure}"),
        ChannelCommand::PolyPressure { note, pressure } => {
            format!("poly_pressure {note} {pressure}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_text_roundtrip_preserves_commands_and_rejects_mixed_content() {
        let text = "@project\n@track t | instrument: piano/acoustic_grand\n  performance: {\n    @0 ch2 program 65;\n    @0 ch2 cc 64 127;\n    @0.125 ch2 note_on 60 90;\n    @0.125 ch2 bend -8192;\n    @1 ch2 pressure 33;\n    @1 ch2 poly_pressure 60 12;\n    @1.25 ch2 note_off 60 37;\n  }\n";
        let p = crate::parse(text).unwrap();
        crate::validate(&p).unwrap();
        assert_eq!(p, crate::parse(&crate::serialize(&p)).unwrap());
        assert_eq!(p.tracks[0].performance.as_ref().unwrap().events.len(), 7);
        let mixed = format!("{text}  @phrase extra [1:1 - 2:1]\n    C4 q mf\n");
        assert!(crate::validate(&crate::parse(&mixed).unwrap()).is_err());
        for command in [
            "program 128",
            "cc 64",
            "bend -8193",
            "unknown 1",
            "note_off 60 128",
        ] {
            let bad = format!(
                "@project\n@track t | instrument: piano/acoustic_grand\n  performance: {{ @0 ch2 {command}; }}\n"
            );
            assert!(crate::parse(&bad).is_err(), "{command}");
        }
        let duplicate = format!("{text}  performance: {{}}\n");
        assert!(crate::parse(&duplicate).is_err());
    }

    #[test]
    fn fractional_tick_positions_survive_json_bit_exactly() {
        for tick in 0..100000 {
            let quarter = f64::from(tick) / 480.0;
            let restored: f64 =
                serde_json::from_str(&serde_json::to_string(&quarter).unwrap()).unwrap();
            assert_eq!(quarter.to_bits(), restored.to_bits(), "tick {tick}");
        }
    }

    #[test]
    fn lossless_json_keeps_zero_gate_redundant_release_and_tie_order() {
        let timeline = PerformanceTimeline {
            events: vec![
                PerformanceEvent {
                    quarter: 1.0 / 480.0,
                    channel: 2,
                    message: ChannelCommand::NoteOn {
                        note: 60,
                        velocity: 90,
                    },
                },
                PerformanceEvent {
                    quarter: 1.0 / 480.0,
                    channel: 2,
                    message: ChannelCommand::NoteOff {
                        note: 60,
                        velocity: 37,
                    },
                },
                PerformanceEvent {
                    quarter: 1.0 / 480.0,
                    channel: 2,
                    message: ChannelCommand::ProgramChange { program: 65 },
                },
                PerformanceEvent {
                    quarter: 2.0,
                    channel: 2,
                    message: ChannelCommand::NoteOff {
                        note: 60,
                        velocity: 12,
                    },
                },
            ],
        };
        timeline.validate().unwrap();
        let restored: PerformanceTimeline =
            serde_json::from_str(&serde_json::to_string(&timeline).unwrap()).unwrap();
        assert_eq!(timeline, restored);
    }

    #[test]
    fn invalid_or_reordered_events_reject_without_repair() {
        let event = PerformanceEvent {
            quarter: 1.0,
            channel: 0,
            message: ChannelCommand::PitchBend { value: 8191 },
        };
        for quarter in [-1.0, f64::NAN, f64::INFINITY, 0.5] {
            let mut bad = event.clone();
            bad.quarter = quarter;
            assert!(
                PerformanceTimeline {
                    events: vec![event.clone(), bad]
                }
                .validate()
                .is_err()
            );
        }
        let mut bad = event;
        bad.message = ChannelCommand::NoteOff {
            note: 60,
            velocity: 128,
        };
        assert!(
            PerformanceTimeline { events: vec![bad] }
                .validate()
                .is_err()
        );
    }
}
