//! Offline groups use only UI-prepared leases; no legacy acquisition fallback.
use super::plugin_tracks::{Vst3RenderGroup, plan_output_pairs, pre_roll_plugin};
use super::*;
use oc_host::vst3_instance::{
    Vst3MidiEvent, Vst3MidiMessage, Vst3NormalizedEvent, Vst3NoteExpressionValue, Vst3OrderedEvent,
    Vst3OwnedInstance, Vst3OwnedInstrumentBlock, Vst3StandardNoteExpression,
};

const MAX_TIMELINE_EVENTS: usize = 4 * 1024 * 1024;
struct TimedEvent {
    sample: u64,
    message: Message,
}
#[derive(Clone, Copy)]
enum Message {
    Midi(Vst3MidiEvent),
    Parameter { name: usize, value: f64 },
}
#[derive(Default)]
struct PreparedEvents {
    events: Vec<TimedEvent>,
    names: Vec<String>,
    names_by_key: HashMap<String, usize>,
    sample: u64,
    error: Option<String>,
    name_bytes: usize,
}
impl PreparedEvents {
    fn push(&mut self, message: Message) {
        if self.error.is_some() {
            return;
        }
        if self.events.len() == MAX_TIMELINE_EVENTS {
            self.error = Some("VST3 group event timeline exceeds its preparation budget".into());
            return;
        }
        self.events.push(TimedEvent {
            sample: self.sample,
            message,
        });
    }
    fn midi(&mut self, channel: u8, message: Vst3MidiMessage) {
        self.push(Message::Midi(Vst3MidiEvent {
            channel,
            sample_offset: 0,
            message,
        }));
    }
}
impl PluginMidiEventSink for PreparedEvents {
    fn note_on(&mut self, c: u8, k: u8, v: u8, _: i32) {
        self.midi(
            c,
            Vst3MidiMessage::NoteOn {
                key: k,
                velocity: v,
            },
        );
    }
    fn note_off(&mut self, c: u8, k: u8, _: i32) {
        self.midi(
            c,
            Vst3MidiMessage::NoteOff {
                key: k,
                velocity: 0,
            },
        );
    }
    fn control_change(&mut self, c: u8, k: u8, v: u8, _: i32) {
        self.midi(
            c,
            Vst3MidiMessage::ControlChange {
                controller: k,
                value: v,
            },
        );
    }
    fn control_change_14(&mut self, c: u8, k: u8, v: u16, _: i32) {
        self.midi(
            c,
            Vst3MidiMessage::ControlChange14 {
                controller: k,
                value: v,
            },
        );
    }
    fn program_change(&mut self, c: u8, p: u8, _: i32) {
        self.midi(c, Vst3MidiMessage::ProgramChange { program: p });
    }
    fn pitch_bend(&mut self, c: u8, v: i16, _: i32) {
        self.midi(c, Vst3MidiMessage::PitchBend { value: v });
    }
    fn channel_pressure(&mut self, c: u8, p: u8, _: i32) {
        self.midi(c, Vst3MidiMessage::ChannelPressure { pressure: p });
    }
    fn poly_pressure(&mut self, c: u8, k: u8, p: u8, _: i32) {
        self.midi(
            c,
            Vst3MidiMessage::PolyPressure {
                key: k,
                pressure: p,
            },
        );
    }
    fn note_expression(&mut self, c: u8, k: u8, name: &str, value: f64, _: i32) {
        let Some(kind) = Vst3StandardNoteExpression::from_name(name) else {
            self.error = Some(format!("Unsupported VST3 note expression '{name}'"));
            return;
        };
        let value = if kind == Vst3StandardNoteExpression::Tuning {
            Vst3NoteExpressionValue::TuningSemitones(value)
        } else {
            Vst3NoteExpressionValue::Normalized(value)
        };
        if let Err(error) = value.normalized_for(kind) {
            self.error = Some(error.to_string());
            return;
        }
        self.midi(
            c,
            Vst3MidiMessage::NoteExpression {
                key: k,
                kind,
                value,
            },
        );
    }
    fn plugin_param(&mut self, name: &str, value: f64, _: i32) {
        if self.error.is_some() {
            return;
        }
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            self.error = Some(format!(
                "VST3 normalized parameter '{name}' is outside 0..=1"
            ));
            return;
        }
        let index = if let Some(&index) = self.names_by_key.get(name) {
            index
        } else {
            if self.names.len() == oc_host::vst3_instance::MAX_VST3_AUTHORED_PARAMETERS
                || name.len() > oc_host::vst3_instance::MAX_VST3_AUTHORED_NAME_BYTES
                || name.len()
                    > oc_host::vst3_instance::MAX_VST3_AUTHORED_TOTAL_NAME_BYTES - self.name_bytes
            {
                self.error = Some("VST3 runtime names exceed their preparation budget".into());
                return;
            }
            self.name_bytes += name.len();
            let index = self.names.len();
            self.names.push(name.to_owned());
            self.names_by_key.insert(name.to_owned(), index);
            index
        };
        self.push(Message::Parameter { name: index, value });
    }
}
fn prepare_events(
    groups: &[&[&MidiEvent]],
    profiles: &[InstrumentProfile],
    performance: Option<&[oc_synth::performance::ScheduledCommand]>,
) -> Result<PreparedEvents, String> {
    let mut prepared = PreparedEvents::default();
    for (events, profile) in groups.iter().zip(profiles) {
        for event in *events {
            prepared.sample = event.time_samples;
            queue_vst3_profile_event(&mut prepared, &event.event, 0, profile)?;
            if let Some(error) = prepared.error.take() {
                return Err(error);
            }
        }
    }
    if let Some(commands) = performance {
        use oc_dsl::performance::ChannelCommand;
        for command in commands {
            prepared.sample = command.sample;
            let message = match command.message {
                ChannelCommand::NoteOn { note, velocity } => Vst3MidiMessage::NoteOn {
                    key: note,
                    velocity,
                },
                ChannelCommand::NoteOff { note, velocity } => Vst3MidiMessage::NoteOff {
                    key: note,
                    velocity,
                },
                ChannelCommand::ControlChange { controller, value } => {
                    Vst3MidiMessage::ControlChange { controller, value }
                }
                ChannelCommand::ProgramChange { program } => {
                    Vst3MidiMessage::ProgramChange { program }
                }
                ChannelCommand::PitchBend { value } => Vst3MidiMessage::PitchBend { value },
                ChannelCommand::ChannelPressure { pressure } => {
                    Vst3MidiMessage::ChannelPressure { pressure }
                }
                ChannelCommand::PolyPressure { note, pressure } => Vst3MidiMessage::PolyPressure {
                    key: note,
                    pressure,
                },
            };
            prepared.midi(command.channel, message);
            if let Some(error) = prepared.error.take() {
                return Err(error);
            }
        }
    }
    // Stable sort: equal-time score events retain track/source order, followed
    // by performance events. Both sources otherwise interleave by actual time.
    prepared.events.sort_by_key(|event| event.sample);
    Ok(prepared)
}
fn output_count(ports: &[AudioPort]) -> usize {
    ports
        .iter()
        .map(|p| p.channels as usize)
        .sum::<usize>()
        .div_ceil(2)
}

/// Control-side resources and bounded scope for one offline source group.
pub(super) struct Vst3GroupRenderOptions<'a> {
    pub total_samples: usize,
    pub transport: PluginTransport,
    pub performance: Option<&'a [oc_synth::performance::ScheduledCommand]>,
    pub pre_roll_seconds: f64,
    pub cancel: Option<&'a crate::render_cancel::CancelToken>,
}
pub(super) fn render_tracks_via_vst3(
    instance: &mut Vst3OwnedInstance,
    group: &Vst3RenderGroup<'_>,
    options: Vst3GroupRenderOptions<'_>,
) -> Result<(Vec<Vec<f32>>, AuxOutputBuffers), String> {
    let Vst3GroupRenderOptions {
        total_samples,
        transport,
        performance,
        pre_roll_seconds,
        cancel,
    } = options;
    pre_roll_plugin(pre_roll_seconds);
    let track_indices = group.track_indices;
    let event_groups = group.event_groups;
    let profiles = group.profiles;
    let pairs = group.pairs;
    let aux_pairs = group.aux_pairs;
    let block_size: u32 = 128;
    if let Some(pair) = aux_pairs
        .iter()
        .flat_map(|pairs| pairs.iter())
        .find(|pair| **pair >= output_count(&instance.snapshot.outputs))
    {
        return Err(format!(
            "VST3 instrument declares {} stereo outputs; auxiliary output {} was requested",
            output_count(&instance.snapshot.outputs),
            pair + 1
        ));
    }
    let mut buffers: Vec<Vec<f32>> = pairs
        .iter()
        .map(|_| Vec::with_capacity(total_samples * 2))
        .collect();
    let prepared = prepare_events(event_groups, profiles, performance)?;
    let ids = instance
        .control
        .normalized_bindings_plan(prepared.names)
        .execute_on_ui()
        .map_err(|error| format!("VST3 runtime parameter binding: {error}"))?;
    let events = prepared.events;
    let mut ordered = Vec::with_capacity(65536);

    // One process call fans the track pair and every requested aux pair. Aux
    // slots are deduplicated so a shared instance renders each pair once.
    let (requested_pairs, track_slots, aux_slots) = plan_output_pairs(pairs, aux_pairs);
    let mut aux_buffers: AuxOutputBuffers = HashMap::new();
    for (group, pairs) in aux_pairs.iter().enumerate() {
        for &pair in *pairs {
            aux_buffers
                .entry((track_indices[group], pair))
                .or_insert_with(|| Vec::with_capacity(total_samples * 2));
        }
    }
    let mut outputs: Vec<Vec<f32>> = requested_pairs
        .iter()
        .map(|_| Vec::with_capacity((block_size * 2) as usize))
        .collect();

    let mut event_idx = 0;
    let mut sample_pos: u64 = 0;
    let total = total_samples as u64;

    while sample_pos < total {
        let remaining = total - sample_pos;
        let block_transport = transport.at_sample(transport.sample_pos + sample_pos);
        let frames =
            block_transport.block_frames((block_size as u64).min(remaining) as usize) as u32;
        let block_end = sample_pos + frames as u64;

        if crate::render_cancel::stopped(cancel) {
            return Err(crate::render_cancel::cancelled());
        }
        ordered.clear();
        while event_idx < events.len() && events[event_idx].sample < block_end {
            let event = &events[event_idx];
            let offset = u32::try_from(
                event
                    .sample
                    .checked_sub(sample_pos)
                    .ok_or("late VST3 event")?,
            )
            .map_err(|_| "VST3 event offset overflow")?;
            if ordered.len() >= 65536 {
                return Err("VST3 block event budget exceeded".into());
            }
            match event.message {
                Message::Midi(mut value) => {
                    value.sample_offset = offset;
                    ordered.push(Vst3OrderedEvent::Midi(value));
                }
                Message::Parameter { name, value } => {
                    ordered.push(Vst3OrderedEvent::Parameter(Vst3NormalizedEvent {
                        id: ids[name],
                        value,
                        sample_offset: offset,
                    }))
                }
            }
            event_idx += 1;
        }

        if frames == 0 {
            break;
        }

        for output in &mut outputs {
            output.resize(frames as usize * 2, 0.0);
        }
        instance
            .processor
            .process_outputs_with_ordered_events_into(
                Vst3OwnedInstrumentBlock {
                    frames,
                    transport: Some(&block_transport),
                    parameters: &[],
                    output_pairs: &requested_pairs,
                    outputs: &mut outputs,
                },
                &ordered,
            )
            .map_err(|error| format!("VST3 owned instrument processing: {error:?}"))?;
        for (buffer, &slot) in buffers.iter_mut().zip(&track_slots) {
            buffer.extend_from_slice(&outputs[slot]);
        }
        for (group, slots) in aux_slots.iter().enumerate() {
            for (&pair, &slot) in aux_pairs[group].iter().zip(slots) {
                aux_buffers
                    .get_mut(&(track_indices[group], pair))
                    .expect("aux buffer was reserved")
                    .extend_from_slice(&outputs[slot]);
            }
        }
        sample_pos += frames as u64;
    }

    for buffer in &mut buffers {
        buffer.resize(total_samples * 2, 0.0);
    }
    for buffer in aux_buffers.values_mut() {
        buffer.resize(total_samples * 2, 0.0);
    }
    if event_idx != events.len() {
        return Err("VST3 events exceeded the render horizon".into());
    }
    Ok((buffers, aux_buffers))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(sample: u64, message: MidiMessage) -> MidiEvent {
        MidiEvent {
            time_samples: sample,
            event: message,
            track_index: 0,
        }
    }
    #[test]
    fn score_parameter_precedes_equal_time_performance_cc_in_the_ordered_native_input() {
        let score = [event(
            7,
            MidiMessage::PluginParam {
                name: "Gain".into(),
                value: 0.2,
            },
        )];
        let refs: Vec<_> = score.iter().collect();
        let profile = InstrumentProfile::from_preset_params(&[], 48_000);
        let commands = [oc_synth::performance::ScheduledCommand {
            sample: 7,
            channel: 2,
            message: oc_dsl::performance::ChannelCommand::ControlChange {
                controller: 1,
                value: 127,
            },
        }];
        let prepared = prepare_events(&[&refs], &[profile], Some(&commands)).unwrap();
        assert!(matches!(
            prepared.events[0].message,
            Message::Parameter {
                name: 0,
                value: 0.2
            }
        ));
        assert!(matches!(
            prepared.events[1].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::ControlChange {
                    controller: 1,
                    value: 127
                },
                ..
            })
        ));
    }
    #[test]
    fn score_and_performance_offsets_merge_stably_and_keep_release_velocity() {
        let score = [
            event(
                9,
                MidiMessage::NoteOff {
                    channel: 2,
                    note: 60,
                },
            ),
            event(
                3,
                MidiMessage::NoteOn {
                    channel: 2,
                    note: 60,
                    velocity: 90,
                },
            ),
        ];
        let events: Vec<_> = score.iter().collect();
        let profile = InstrumentProfile::from_preset_params(&[], 48_000);
        let commands = [
            oc_synth::performance::ScheduledCommand {
                sample: 5,
                channel: 2,
                message: oc_dsl::performance::ChannelCommand::NoteOff {
                    note: 60,
                    velocity: 73,
                },
            },
            oc_synth::performance::ScheduledCommand {
                sample: 3,
                channel: 2,
                message: oc_dsl::performance::ChannelCommand::NoteOn {
                    note: 64,
                    velocity: 100,
                },
            },
        ];
        let prepared = prepare_events(&[&events], &[profile], Some(&commands)).unwrap();
        assert_eq!(
            prepared.events.iter().map(|e| e.sample).collect::<Vec<_>>(),
            [3, 3, 5, 9]
        );
        assert!(matches!(
            prepared.events[0].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::NoteOn { key: 60, .. },
                ..
            })
        ));
        assert!(matches!(
            prepared.events[1].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::NoteOn { key: 64, .. },
                ..
            })
        ));
        assert!(matches!(
            prepared.events[2].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::NoteOff {
                    key: 60,
                    velocity: 73
                },
                ..
            })
        ));
    }
    #[test]
    fn mapped_cc14_stays_one_normalized_point_and_unmapped_stays_typed_cc14() {
        let score = [
            event(
                0,
                MidiMessage::ControlChange14 {
                    channel: 0,
                    controller: 1,
                    value: 12288,
                },
            ),
            event(
                1,
                MidiMessage::ControlChange14 {
                    channel: 0,
                    controller: 2,
                    value: 8192,
                },
            ),
        ];
        let events: Vec<_> = score.iter().collect();
        let profile = InstrumentProfile::from_preset_params(
            &[("cc.1.param".into(), ParamValue::String("Gain".into()))],
            48_000,
        );
        let prepared = prepare_events(&[&events], &[profile], None).unwrap();
        assert_eq!(prepared.events.len(), 2);
        assert_eq!(prepared.names, ["Gain"]);
        assert!(
            matches!(prepared.events[0].message,Message::Parameter{name:0,value} if value == 12288.0/16383.0)
        );
        assert!(matches!(
            prepared.events[1].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::ControlChange14 {
                    controller: 2,
                    value: 8192
                },
                ..
            })
        ));
    }
    #[test]
    fn invalid_normalized_parameters_and_unknown_expressions_fail_before_processing() {
        let profile = InstrumentProfile::from_preset_params(&[], 48_000);
        for message in [
            MidiMessage::PluginParam {
                name: "Gain".into(),
                value: f64::NAN,
            },
            MidiMessage::PluginParam {
                name: "Gain".into(),
                value: 1.1,
            },
            MidiMessage::NoteExpression {
                channel: 0,
                note: 60,
                expression: "unknown".into(),
                value: 0.5,
            },
        ] {
            let score = [event(0, message)];
            let refs: Vec<_> = score.iter().collect();
            assert!(prepare_events(&[&refs], std::slice::from_ref(&profile), None).is_err());
        }
    }
    #[test]
    fn tuning_keeps_physical_semitones_for_the_shared_native_converter() {
        let score = [event(
            0,
            MidiMessage::NoteExpression {
                channel: 0,
                note: 60,
                expression: "tuning".into(),
                value: 12.0,
            },
        )];
        let refs: Vec<_> = score.iter().collect();
        let profile = InstrumentProfile::from_preset_params(&[], 48_000);
        let prepared = prepare_events(&[&refs], &[profile], None).unwrap();
        assert!(matches!(
            prepared.events[0].message,
            Message::Midi(Vst3MidiEvent {
                message: Vst3MidiMessage::NoteExpression {
                    key: 60,
                    kind: Vst3StandardNoteExpression::Tuning,
                    value: Vst3NoteExpressionValue::TuningSemitones(12.0)
                },
                ..
            })
        ));
    }
}
