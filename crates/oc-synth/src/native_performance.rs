//! Explicit, fail-closed native performance rendering. No score conversion.
//! A declared program binds one MIDI program to the supplied patch, not a GM bank.
use crate::performance::ScheduledCommand;
use crate::synth::{OpenSynth, SynthPatch};
use oc_dsl::{performance::ChannelCommand, timing::ProjectClock};

/// The bank and program a performance channel starts from.
#[derive(Clone, Copy, Default)]
pub struct PatchBinding {
    pub program: Option<u8>,
    pub bank_msb: Option<u8>,
    pub bank_lsb: Option<u8>,
}
impl From<Option<u8>> for PatchBinding {
    fn from(program: Option<u8>) -> Self {
        Self {
            program,
            ..Self::default()
        }
    }
}

/// Read the program and bank parameters from a patch.
pub fn program_binding(
    params: &[(String, oc_dsl::ast::ParamValue)],
) -> Result<PatchBinding, String> {
    let mut binding = PatchBinding::default();
    for (name, value) in params {
        let field = match name.as_str() {
            "performance.program" => &mut binding.program,
            "performance.bank_msb" => &mut binding.bank_msb,
            "performance.bank_lsb" => &mut binding.bank_lsb,
            _ => continue,
        };
        match value {
            oc_dsl::ast::ParamValue::Number(v)
                if v.is_finite() && (0.0..=127.0).contains(v) && v.fract() == 0.0 =>
            {
                *field = Some(*v as u8)
            }
            _ => return Err("performance program/bank binding must be an integer 0..127".into()),
        }
    }
    Ok(binding)
}

/// Reject unordered commands, invalid MIDI-1 values, unsupported controllers
/// and program or bank changes that do not match the patch binding.
pub fn validate(
    commands: &[ScheduledCommand],
    binding: impl Into<PatchBinding>,
) -> Result<(), String> {
    let binding = binding.into();
    if commands.windows(2).any(|p| p[0].sample > p[1].sample) {
        return Err("native performance commands must be ordered".into());
    }
    for command in commands {
        if command.channel > 15 {
            return Err("invalid native performance channel".into());
        }
        let invalid = match command.message {
            ChannelCommand::NoteOn { note, velocity }
            | ChannelCommand::NoteOff { note, velocity } => note > 127 || velocity > 127,
            ChannelCommand::ControlChange { controller, value } => controller > 127 || value > 127,
            ChannelCommand::ProgramChange { program } => program > 127,
            ChannelCommand::PitchBend { value } => !(-8192..=8191).contains(&value),
            ChannelCommand::ChannelPressure { pressure } => pressure > 127,
            ChannelCommand::PolyPressure { note, pressure } => note > 127 || pressure > 127,
        };
        if invalid {
            return Err("invalid MIDI1 performance value".into());
        }
        match command.message {
            ChannelCommand::ProgramChange { program: value } if Some(value) != binding.program => {
                return Err(
                    "native performance program requires an explicit matching patch binding".into(),
                );
            }
            ChannelCommand::NoteOff { velocity, .. } if velocity != 0 => {
                return Err("native performance release velocity is not supported".into());
            }
            ChannelCommand::ControlChange {
                controller: 0,
                value,
            } if Some(value) != binding.bank_msb => {
                return Err("native bank MSB requires explicit matching patch binding".into());
            }
            ChannelCommand::ControlChange {
                controller: 32,
                value,
            } if Some(value) != binding.bank_lsb => {
                return Err("native bank LSB requires explicit matching patch binding".into());
            }
            ChannelCommand::ControlChange { controller, .. }
                if ![0, 1, 7, 10, 11, 32, 64, 120, 121, 123].contains(&controller) =>
            {
                return Err(format!(
                    "unsupported native performance controller {controller}"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// MIDI pan is a stereo balance, unity at64; endpoints suppress one side.
fn balance(pan: u8) -> [f32; 2] {
    if pan <= 64 {
        [1., pan as f32 / 64.]
    } else {
        [(127 - pan) as f32 / 63., 1.]
    }
}

/// Stateful channel-preserving performance engine, shared by offline and bounded playback.
/// All channel synths are allocated during construction, never by event dispatch.
pub struct PerformanceRenderer {
    channels: Vec<Option<OpenSynth>>,
    volume: [f32; 16],
    expression: [f32; 16],
    pan: [u8; 16],
    binding: PatchBinding,
}

impl PerformanceRenderer {
    /// Prepare one synth per channel the commands use, allocating every channel
    /// up front.
    pub fn new(
        patch: SynthPatch,
        commands: &[ScheduledCommand],
        binding: impl Into<PatchBinding>,
        tempo: f64,
    ) -> Result<Self, String> {
        let binding = binding.into();
        validate(commands, binding)?;
        let mut channels: Vec<Option<OpenSynth>> = (0..16).map(|_| None).collect();
        for command in commands {
            channels[command.channel as usize]
                .get_or_insert_with(|| OpenSynth::new_with_tempo(patch.clone(), tempo));
        }
        Ok(Self {
            channels,
            volume: [1.; 16],
            expression: [1.; 16],
            pan: [64; 16],
            binding,
        })
    }

    /// Apply a tempo change to every prepared channel.
    pub fn set_tempo(&mut self, tempo: f64, rate: u32) -> Result<(), String> {
        for synth in self.channels.iter_mut().flatten() {
            synth.set_tempo(tempo, rate)?;
        }
        Ok(())
    }

    /// Return every channel to its construction state, keeping the prepared
    /// patches and the channel allocation.
    pub fn reset(&mut self) {
        for synth in self.channels.iter_mut().flatten() {
            synth.reset();
        }
        self.volume = [1.; 16];
        self.expression = [1.; 16];
        self.pan = [64; 16];
    }

    /// Apply one scheduled command to its prepared channel.
    pub fn send(&mut self, command: &ScheduledCommand) -> Result<(), String> {
        validate(std::slice::from_ref(command), self.binding)?;
        let ch = command.channel;
        let slot = ch as usize;
        let synth = self.channels[slot]
            .as_mut()
            .ok_or("performance channel was not prepared")?;
        match command.message {
            ChannelCommand::NoteOn { note, velocity } => synth.note_on(ch, note, velocity),
            ChannelCommand::NoteOff { note, .. } => synth.note_off(ch, note),
            ChannelCommand::PitchBend { value } => synth.set_pitch_bend(ch, value),
            ChannelCommand::ChannelPressure { pressure } => {
                synth.set_channel_pressure(ch, pressure)
            }
            ChannelCommand::PolyPressure { note, pressure } => {
                synth.set_poly_pressure(ch, note, pressure)
            }
            ChannelCommand::ProgramChange { .. } => {} // validated explicit binding
            ChannelCommand::ControlChange { controller, value } => match controller {
                0 | 32 => {} // validated single-patch bank binding, not a GM bank
                7 => self.volume[slot] = value as f32 / 127.,
                10 => self.pan[slot] = value,
                11 => self.expression[slot] = value as f32 / 127.,
                1 => synth.set_mod_wheel(ch, value),
                64 => synth.set_sustain(ch, value >= 64),
                120 => synth.all_sound_off(ch),
                123 => synth.all_notes_off(ch),
                121 => {
                    synth.reset_controllers(ch);
                    self.expression[slot] = 1.;
                }
                _ => unreachable!("validated controller"),
            },
        }
        Ok(())
    }

    /// Mix one stereo frame from every prepared channel.
    pub fn next_stereo(&mut self, rate: u32) -> [f32; 2] {
        let mut output = [0.; 2];
        for (slot, synth) in self.channels.iter_mut().enumerate() {
            if let Some(synth) = synth {
                let sample = synth.next_stereo(rate);
                let gain = self.volume[slot] * self.expression[slot];
                let balance = balance(self.pan[slot]);
                output[0] += sample[0] * gain * balance[0];
                output[1] += sample[1] * gain * balance[1];
            }
        }
        output
    }
}

/// Render bounded interleaved stereo output, applying clock tempo changes at
/// their sample boundaries.
pub fn render(
    commands: &[ScheduledCommand],
    frames: usize,
    rate: u32,
    patch: SynthPatch,
    binding: impl Into<PatchBinding>,
    clock: &ProjectClock,
) -> Result<Vec<f32>, String> {
    let binding = binding.into();
    validate(commands, binding)?;
    if rate == 0
        || frames > rate as usize * 600
        || frames.checked_mul(2).is_none()
        || commands.last().is_some_and(|c| c.sample >= frames as u64)
    {
        return Err("invalid native performance render horizon".into());
    }
    let tempo = clock.position_at_sample(0, rate)?.bpm;
    let mut renderer = PerformanceRenderer::new(patch, commands, binding, tempo)?;
    let mut output = vec![0.; frames * 2];
    let mut index = 0;
    let mut boundary = 0;
    for frame in 0..frames {
        if frame == boundary {
            renderer.set_tempo(clock.position_at_sample(frame as u64, rate)?.bpm, rate)?;
            boundary = clock
                .next_change_sample(frame as u64, rate)?
                .map_or(frames, |v| v.min(frames as u64) as usize);
        }
        while index < commands.len() && commands[index].sample == frame as u64 {
            renderer.send(&commands[index])?;
            index += 1;
        }
        output[frame * 2..frame * 2 + 2].copy_from_slice(&renderer.next_stereo(rate));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(sample: u64, channel: u8, message: ChannelCommand) -> ScheduledCommand {
        ScheduledCommand {
            sample,
            channel,
            message,
        }
    }
    fn clock() -> ProjectClock {
        ProjectClock::new(
            &oc_dsl::parse("@project\n  tempo: 120\n")
                .unwrap()
                .properties,
        )
        .unwrap()
    }
    #[test]
    fn banks_require_explicit_binding_and_cannot_select_another_patch() {
        let commands = vec![
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 0,
                    value: 0,
                },
            ),
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 32,
                    value: 2,
                },
            ),
            command(0, 0, ChannelCommand::ProgramChange { program: 6 }),
        ];
        assert!(validate(&commands, Some(6)).is_err());
        let binding = PatchBinding {
            program: Some(6),
            bank_msb: Some(0),
            bank_lsb: Some(2),
        };
        assert!(validate(&commands, binding).is_ok());
        let mut wrong = commands.clone();
        wrong.push(command(
            1,
            0,
            ChannelCommand::ControlChange {
                controller: 32,
                value: 3,
            },
        ));
        assert!(validate(&wrong, binding).is_err());
        let mut played = commands;
        played.push(command(
            0,
            0,
            ChannelCommand::NoteOn {
                note: 60,
                velocity: 100,
            },
        ));
        let reference = vec![command(
            0,
            0,
            ChannelCommand::NoteOn {
                note: 60,
                velocity: 100,
            },
        )];
        assert_eq!(
            render(
                &played,
                1024,
                8000,
                SynthPatch::default(),
                binding,
                &clock()
            )
            .unwrap(),
            render(
                &reference,
                1024,
                8000,
                SynthPatch::default(),
                None,
                &clock()
            )
            .unwrap()
        );
    }

    #[test]
    fn unsupported_commands_fail_closed() {
        assert!(
            validate(
                &[command(0, 0, ChannelCommand::ProgramChange { program: 6 })],
                None
            )
            .is_err()
        );
        assert!(
            validate(
                &[command(0, 0, ChannelCommand::ProgramChange { program: 6 })],
                Some(6)
            )
            .is_ok()
        );
        assert!(
            validate(
                &[command(
                    0,
                    0,
                    ChannelCommand::ControlChange {
                        controller: 74,
                        value: 1
                    }
                )],
                None
            )
            .is_err()
        );
        assert!(
            validate(
                &[command(
                    0,
                    0,
                    ChannelCommand::NoteOff {
                        note: 60,
                        velocity: 37
                    }
                )],
                None
            )
            .is_err()
        );
    }
    #[test]
    fn volume_expression_pan_and_same_sample_order() {
        let note = command(
            0,
            0,
            ChannelCommand::NoteOn {
                note: 60,
                velocity: 100,
            },
        );
        let full = render(
            std::slice::from_ref(&note),
            1024,
            8000,
            SynthPatch::default(),
            None,
            &clock(),
        )
        .unwrap();
        assert!(full.iter().any(|x| x.abs() > 1e-6));
        let events = vec![
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 7,
                    value: 64,
                },
            ),
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 11,
                    value: 32,
                },
            ),
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 10,
                    value: 0,
                },
            ),
            note,
        ];
        let scaled = render(&events, 1024, 8000, SynthPatch::default(), None, &clock()).unwrap();
        for i in 0..1024 {
            assert!((scaled[i * 2] - full[i * 2] * (64. / 127.) * (32. / 127.)).abs() < 1e-7);
            assert_eq!(scaled[i * 2 + 1], 0.);
        }
    }
    #[test]
    fn controls_are_channel_scoped_and_reset_restores_expression_not_volume() {
        let note = command(
            0,
            1,
            ChannelCommand::NoteOn {
                note: 60,
                velocity: 100,
            },
        );
        let expected = render(
            std::slice::from_ref(&note),
            1024,
            8000,
            SynthPatch::default(),
            None,
            &clock(),
        )
        .unwrap();
        let events = vec![
            command(
                0,
                0,
                ChannelCommand::ControlChange {
                    controller: 7,
                    value: 0,
                },
            ),
            note.clone(),
        ];
        assert_eq!(
            render(&events, 1024, 8000, SynthPatch::default(), None, &clock()).unwrap(),
            expected
        );
        let events = vec![
            command(
                0,
                1,
                ChannelCommand::ControlChange {
                    controller: 7,
                    value: 64,
                },
            ),
            command(
                0,
                1,
                ChannelCommand::ControlChange {
                    controller: 11,
                    value: 0,
                },
            ),
            command(
                0,
                1,
                ChannelCommand::ControlChange {
                    controller: 121,
                    value: 0,
                },
            ),
            note,
        ];
        let actual = render(&events, 1024, 8000, SynthPatch::default(), None, &clock()).unwrap();
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b * (64. / 127.)).abs() < 1e-7);
        }
    }
}
