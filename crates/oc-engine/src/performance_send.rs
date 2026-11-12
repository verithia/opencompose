//! Explicit MIDI effect-depth envelope for a declared auxiliary bus.
//! This primitive does not authorize dropping CC messages in a source renderer.
//! Apply before send-path PDC, accounting for latency already in the track audio.
use oc_dsl::performance::ChannelCommand;
use oc_synth::performance::ScheduledCommand;

/// Shared preflight for source dispatch and graph construction. Only these
/// validated commands are delegated; the persisted performance timeline remains
/// intact. Binding metadata lives in the project-owned builtin preset/overrides.
pub fn project_plan(
    project: &oc_dsl::ast::Project,
    track_index: usize,
    rate: u32,
) -> Result<Vec<(u8, String, SendEnvelope)>, String> {
    use oc_dsl::ast::ParamValue;
    let track = project
        .tracks
        .get(track_index)
        .ok_or("unknown send track")?;
    let preset = track
        .preset
        .as_ref()
        .and_then(|name| project.presets.iter().find(|p| &p.name == name));
    // Route metadata is authored on the preset and track. Instrument
    // preparation also measures loudness, which has no bearing on sends.
    let mut params = preset
        .into_iter()
        .flat_map(|preset| &preset.params)
        .chain(track.engine_params.iter().flatten())
        .filter(|(name, _)| name.starts_with("performance.send"))
        .peekable();
    if params.peek().is_none() {
        return Ok(vec![]);
    }
    if !preset.is_some_and(|p| {
        p.source.as_deref() == Some("builtin")
            && p.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)
    }) {
        return Err("performance send delegation requires builtin.open_synth".into());
    }
    let timeline = track
        .performance
        .as_ref()
        .ok_or("send binding requires performance timeline")?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let commands = oc_synth::performance::schedule(timeline, &clock, rate)?;
    let channel = commands
        .first()
        .ok_or("send binding requires nonempty timeline")?
        .channel;
    let mut fields = std::collections::HashMap::new();
    for (name, value) in params {
        if ![
            "performance.send91.bus",
            "performance.send91.initial",
            "performance.send93.bus",
            "performance.send93.initial",
        ]
        .contains(&name.as_str())
        {
            return Err(format!("unknown performance send parameter {name}"));
        }
        fields.insert(name.as_str(), value);
    }
    let mut result = Vec::new();
    for controller in [91, 93] {
        let bus_key = format!("performance.send{controller}.bus");
        let initial_key = format!("performance.send{controller}.initial");
        let bus = fields.get(bus_key.as_str());
        let initial = fields.get(initial_key.as_str());
        if bus.is_none() && initial.is_none() {
            continue;
        }
        let Some(ParamValue::String(bus)) = bus else {
            return Err("performance send requires explicit bus string".into());
        };
        let initial = match initial {
            Some(ParamValue::Number(v))
                if v.is_finite() && (0.0..=127.).contains(v) && v.fract() == 0. =>
            {
                *v as u8
            }
            _ => return Err("performance send requires integer initial depth 0..127".into()),
        };
        if project.buses.iter().filter(|b| &b.name == bus).count() != 1
            || track.mix.sends.iter().filter(|s| &s.target == bus).count() != 1
            || track.mix.output.as_ref() == Some(bus)
            || result.iter().any(|(_, target, _)| target == bus)
        {
            return Err("performance send requires unique explicit auxiliary bus route".into());
        }
        result.push((
            controller,
            bus.clone(),
            SendEnvelope::compile(&commands, channel, controller, initial)?,
        ));
    }
    Ok(result)
}

#[derive(Debug, Clone)]
/// CC-driven send envelope for one rendered track.
pub struct SendEnvelope {
    initial: f32,
    events: Vec<(u64, f32)>,
}

impl SendEnvelope {
    /// Narrow policy: one MIDI channel per rendered track, CC91 or CC93 only.
    /// The caller explicitly supplies initial depth; no implicit GM default.
    /// Values linearly multiply the declared static send amount, not the dry path.
    pub fn compile(
        commands: &[ScheduledCommand],
        channel: u8,
        controller: u8,
        initial: u8,
    ) -> Result<Self, String> {
        if channel > 15 || ![91, 93].contains(&controller) || initial > 127 {
            return Err("invalid explicit performance send binding".into());
        }
        if commands.windows(2).any(|p| p[0].sample > p[1].sample) {
            return Err("performance send commands must be ordered".into());
        }
        let mut events = Vec::new();
        for command in commands {
            if command.channel != channel {
                return Err("performance send requires a single-channel track".into());
            }
            if let ChannelCommand::ControlChange {
                controller: cc,
                value,
            } = command.message
            {
                if cc > 127 || value > 127 {
                    return Err("invalid MIDI1 controller value".into());
                }
                if cc == controller {
                    // Retain event order: the last event at an identical sample wins.
                    events.push((command.sample, value as f32 / 127.));
                }
                // CC121 is intentionally not mapped to an invented send default.
                // This narrow policy preserves effect depth on controller reset.
            }
        }
        Ok(Self {
            initial: initial as f32 / 127.,
            events,
        })
    }

    /// Stateless absolute-time application: block partitioning and backward seeks
    /// cannot retain a stale controller value. Graph delay/effect state still needs
    /// its normal reset/preroll on seek; this does not reconstruct effect tails.
    pub fn apply(
        &self,
        stereo: &mut [f32],
        absolute_start: u64,
        track_latency: u32,
    ) -> Result<(), String> {
        if !stereo.len().is_multiple_of(2) {
            return Err("performance send requires stereo frames".into());
        }
        let frames = (stereo.len() / 2) as u64;
        absolute_start
            .checked_add(frames)
            .ok_or("performance send timeline overflow")?;
        let latency = track_latency as u64;
        let mut index = if absolute_start >= latency {
            self.events
                .partition_point(|(sample, _)| *sample <= absolute_start - latency)
        } else {
            0
        };
        let mut gain = if index == 0 {
            self.initial
        } else {
            self.events[index - 1].1
        };
        for (offset, frame) in stereo.chunks_exact_mut(2).enumerate() {
            let absolute = absolute_start + offset as u64;
            if let Some(source_time) = absolute.checked_sub(latency) {
                while index < self.events.len() && self.events[index].0 <= source_time {
                    gain = self.events[index].1;
                    index += 1;
                }
            }
            frame[0] *= gain;
            frame[1] *= gain;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cc(sample: u64, controller: u8, value: u8) -> ScheduledCommand {
        ScheduledCommand {
            sample,
            channel: 2,
            message: ChannelCommand::ControlChange { controller, value },
        }
    }
    fn envelope() -> SendEnvelope {
        SendEnvelope::compile(
            &[
                cc(0, 91, 0),
                cc(3, 91, 64),
                cc(3, 91, 127),
                cc(5, 93, 0),
                cc(6, 121, 0),
                cc(9, 91, 32),
            ],
            2,
            91,
            64,
        )
        .unwrap()
    }
    #[test]
    fn project_plan_combines_preset_bindings_and_track_overrides() {
        let project = oc_dsl::parse(
            r#"@project
@preset p
  source: builtin
  plugin: "builtin.open_synth"
  params: {performance.send91.bus: "room", performance.send91.initial: 12}
@track t | preset: p, instrument: synth/lead_square
  engine_params: {performance.send91.initial: 65}
  mix: send(room=50%)
  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 0; }
@bus room
  routes: [master]
"#,
        )
        .unwrap();
        let plan = project_plan(&project, 0, 8000).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, 91);
        assert_eq!(plan[0].1, "room");
        assert_eq!(plan[0].2.initial, 65.0 / 127.0);
    }

    #[test]
    fn exact_boundaries_order_and_reset_policy() {
        let mut audio = vec![1.; 24];
        envelope().apply(&mut audio, 0, 0).unwrap();
        assert_eq!(&audio[..6], &[0.; 6]);
        assert_eq!(&audio[6..18], &[1.; 12]);
        assert_eq!(&audio[18..], &[32. / 127.; 6]);
    }
    #[test]
    fn arbitrary_blocks_and_backward_seek_match_whole_render() {
        let envelope = envelope();
        for latency in [0, 2, 15] {
            let input: Vec<f32> = (0..64).map(|v| v as f32 / 64.).collect();
            let mut whole = input.clone();
            envelope.apply(&mut whole, 0, latency).unwrap();
            for block in 1..17 {
                let mut divided = input.clone();
                for (index, chunk) in divided.chunks_mut(block * 2).enumerate() {
                    envelope
                        .apply(chunk, (index * block) as u64, latency)
                        .unwrap();
                }
                assert_eq!(whole, divided);
            }
            for start in [20, 9, 3, 0] {
                let mut chunk = input[start * 2..(start + 4) * 2].to_vec();
                envelope.apply(&mut chunk, start as u64, latency).unwrap();
                assert_eq!(chunk, whole[start * 2..(start + 4) * 2]);
            }
        }
    }
    #[test]
    fn latency_delays_controls_with_track_audio() {
        let envelope = SendEnvelope::compile(&[cc(0, 91, 0), cc(2, 91, 127)], 2, 91, 64).unwrap();
        let mut audio = vec![1.; 12];
        envelope.apply(&mut audio, 0, 3).unwrap();
        assert_eq!(&audio[..6], &[64. / 127.; 6]);
        assert_eq!(&audio[6..10], &[0.; 4]);
        assert_eq!(&audio[10..], &[1.; 2]);
    }
    #[test]
    fn validation_precedes_mutation_and_ambiguous_channels_reject() {
        assert!(SendEnvelope::compile(&[cc(0, 91, 128)], 2, 91, 0).is_err());
        assert!(SendEnvelope::compile(&[cc(2, 91, 0), cc(1, 91, 1)], 2, 91, 0).is_err());
        assert!(SendEnvelope::compile(&[cc(0, 91, 0)], 1, 91, 0).is_err());
        assert!(SendEnvelope::compile(&[], 2, 7, 0).is_err());
        let mut odd = vec![1.; 3];
        assert!(envelope().apply(&mut odd, 0, 0).is_err());
        assert_eq!(odd, vec![1.; 3]);
        let mut overflow = vec![1.; 4];
        assert!(envelope().apply(&mut overflow, u64::MAX, 0).is_err());
        assert_eq!(overflow, vec![1.; 4]);
    }
}
