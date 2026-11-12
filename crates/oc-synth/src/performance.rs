//! Lossless scheduling for explicit performance commands.
//! Backend dispatch must negotiate supported commands before rendering.

use oc_dsl::{
    performance::{ChannelCommand, PerformanceTimeline},
    timing::ProjectClock,
};

/// One channel command placed at an absolute sample.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledCommand {
    pub sample: u64,
    pub channel: u8,
    pub message: ChannelCommand,
}

/// Preserve source order even when distinct musical positions map to one sample.
/// Do not inject GM/mixer defaults, pair voices, or fabricate final releases.
pub fn schedule(
    timeline: &PerformanceTimeline,
    clock: &ProjectClock,
    sample_rate: u32,
) -> Result<Vec<ScheduledCommand>, String> {
    if sample_rate == 0 {
        return Err("sample rate must be positive".into());
    }
    timeline.validate()?;
    timeline
        .events
        .iter()
        .map(|event| {
            Ok(ScheduledCommand {
                sample: clock.sample_at_quarter(event.quarter, sample_rate)?,
                channel: event.channel,
                message: event.message.clone(),
            })
        })
        .collect()
}

impl ScheduledCommand {
    /// Exact channel-voice MIDI1 bytes; no normalized value rounding.
    pub fn midi_bytes(&self) -> Vec<u8> {
        let ch = self.channel;
        match self.message {
            ChannelCommand::NoteOn { note, velocity } => vec![0x90 | ch, note, velocity],
            ChannelCommand::NoteOff { note, velocity } => vec![0x80 | ch, note, velocity],
            ChannelCommand::ControlChange { controller, value } => {
                vec![0xB0 | ch, controller, value]
            }
            ChannelCommand::ProgramChange { program } => vec![0xC0 | ch, program],
            ChannelCommand::PitchBend { value } => {
                let raw = (i32::from(value) + 8192) as u16;
                vec![0xE0 | ch, (raw & 127) as u8, (raw >> 7) as u8]
            }
            ChannelCommand::ChannelPressure { pressure } => vec![0xD0 | ch, pressure],
            ChannelCommand::PolyPressure { note, pressure } => vec![0xA0 | ch, note, pressure],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_scheduler_keeps_zero_gate_order_release_velocity_and_programs() {
        let project = oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n@track t | instrument: piano/acoustic_grand\n  performance: { @0 ch2 program 65; @1.125 ch2 note_on 60 90; @1.125 ch2 note_off 60 37; @1.125 ch2 cc 64 127; @1.125 ch2 bend -8192; @1.125 ch2 pressure 12; @1.125 ch2 poly_pressure 60 11; }\n").unwrap();
        let clock = ProjectClock::new(&project.properties).unwrap();
        let timeline = project.tracks[0].performance.as_ref().unwrap();
        let scheduled = schedule(timeline, &clock, 48000).unwrap();
        assert_eq!(scheduled.len(), 7);
        assert_eq!(scheduled[0].sample, 0);
        assert!(scheduled[1..].iter().all(|e| e.sample == 30000));
        assert_eq!(
            scheduled
                .iter()
                .map(ScheduledCommand::midi_bytes)
                .collect::<Vec<_>>(),
            vec![
                vec![0xC2, 65],
                vec![0x92, 60, 90],
                vec![0x82, 60, 37],
                vec![0xB2, 64, 127],
                vec![0xE2, 0, 0],
                vec![0xD2, 12],
                vec![0xA2, 60, 11],
            ]
        );
        assert!(schedule(timeline, &clock, 0).is_err());
    }
}
