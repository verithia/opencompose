//! Streaming preparation and score submission for translated VST3 instruments.
use super::*;
use oc_host::vst2_bridge::{BridgeMode, BridgedVst3, bridge_mode};
use oc_host::vst3_instance::{
    Vst3AuthoredInput, Vst3MidiEvent, Vst3MidiMessage, Vst3NoteExpressionValue, Vst3OrderedEvent,
    Vst3OwnedAuthoredPlan, Vst3ProcessingBudget, Vst3StandardNoteExpression,
};

pub(crate) fn load_streamed_vst3(
    info: &oc_host::PluginInfo,
    preset: &PresetDef,
    params: &[(String, ParamValue)],
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<BridgedVst3, String> {
    let state = if is_sfizz_sample_preset(preset) {
        Some(sfizz_vst3_state_bytes(preset, root)?)
    } else {
        let context = PluginStateContext::new(PluginFormat::Vst3, Some(&info.id), &preset.name);
        materialize_vst3_preset_state_at(params, &context, root)?
            .map(|state| state.as_ref().clone())
    };
    let claims = crate::render_control::checked_profile_assignments(
        params.iter().map(|(name, value)| (name.as_str(), value)),
        sample_rate,
        root,
    )
    .map_err(|error| error.to_string())?;
    let mut parameters = Vec::new();
    for ((name, value), claim) in params.iter().zip(claims) {
        if oc_host::preset_parameters::classify_host_parameter(PluginFormat::Vst3, name)
            .map_err(|error| format!("VST3 parameter '{name}': {error}"))?
            .is_some()
        {
            continue;
        }
        parameters.push(Vst3AuthoredInput {
            name: name.clone(),
            native_value: param_value_as_f64(value),
            domain: oc_host::preset_parameters::ParameterWriteDomain::LegacyVst3Auto,
            external_claim: matches!(
                claim,
                crate::render_control::ProfileAssignmentClaim::Accepted
            ),
        });
    }
    BridgedVst3::load(
        Vst3OwnedAuthoredPlan {
            path: PathBuf::from(&info.path),
            class_name: Some(info.name.clone()),
            sample_rate,
            max_frames: 512,
            component_state: state,
            parameters,
            budget: Vst3ProcessingBudget::default(),
        },
        bridge_mode().unwrap_or(BridgeMode::Offline),
    )
}

pub(crate) fn queue_translated_vst3_score(
    plugin: &mut BridgedVst3,
    event: &MidiMessage,
    offset: u32,
) -> Result<(), String> {
    let (channel, message) = match *event {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } => (
            channel,
            Vst3MidiMessage::NoteOn {
                key: note,
                velocity,
            },
        ),
        MidiMessage::NoteOff { channel, note } => (
            channel,
            Vst3MidiMessage::NoteOff {
                key: note,
                velocity: 0,
            },
        ),
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        } => (
            channel,
            Vst3MidiMessage::ControlChange { controller, value },
        ),
        MidiMessage::ControlChange14 {
            channel,
            controller,
            value,
        } => (
            channel,
            Vst3MidiMessage::ControlChange14 { controller, value },
        ),
        MidiMessage::ProgramChange { channel, program } => {
            (channel, Vst3MidiMessage::ProgramChange { program })
        }
        MidiMessage::PitchBend { channel, value } => {
            (channel, Vst3MidiMessage::PitchBend { value })
        }
        MidiMessage::ChannelPressure { channel, pressure } => {
            (channel, Vst3MidiMessage::ChannelPressure { pressure })
        }
        MidiMessage::PolyPressure {
            channel,
            note,
            pressure,
        } => (
            channel,
            Vst3MidiMessage::PolyPressure {
                key: note,
                pressure,
            },
        ),
        MidiMessage::NoteExpression {
            channel,
            note,
            ref expression,
            value,
        } => {
            let kind = Vst3StandardNoteExpression::from_name(expression)
                .ok_or_else(|| format!("Unsupported VST3 note expression '{expression}'"))?;
            let value = if kind == Vst3StandardNoteExpression::Tuning {
                Vst3NoteExpressionValue::TuningSemitones(value)
            } else {
                Vst3NoteExpressionValue::Normalized(value)
            };
            value
                .normalized_for(kind)
                .map_err(|error| error.to_string())?;
            (
                channel,
                Vst3MidiMessage::NoteExpression {
                    key: note,
                    kind,
                    value,
                },
            )
        }
        MidiMessage::PluginParam { ref name, value } => {
            return plugin.queue_parameter(name, value, offset);
        }
        MidiMessage::NativeVoice(_) => {
            return Err("native voice events require builtin.open_synth".into());
        }
    };
    plugin.queue_event(Vst3OrderedEvent::Midi(Vst3MidiEvent {
        channel,
        sample_offset: offset,
        message,
    }))
}
