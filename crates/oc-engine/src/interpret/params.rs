//! Humanization, note durations and MIDI value conversion.

use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct HumanizeDelta {
    pub(super) timing_offset: i64,
    pub(super) velocity_offset: i16,
}

pub(super) fn articulation_humanize_for_note(
    articulation: Option<Articulation>,
    profile: &InstrumentProfile,
    track_index: usize,
    channel: u8,
    note: u8,
    note_on_time: u64,
) -> HumanizeDelta {
    let Some(articulation) = articulation else {
        return HumanizeDelta::default();
    };
    let Some(humanize) = profile.articulation_humanize(articulation) else {
        return HumanizeDelta::default();
    };

    let seed = deterministic_note_seed(track_index, channel, note, note_on_time);
    HumanizeDelta {
        timing_offset: centered_i64(seed, humanize.timing_samples as i64),
        velocity_offset: centered_i64(seed.rotate_left(17), humanize.velocity as i64) as i16,
    }
}

pub(super) fn deterministic_note_seed(
    track_index: usize,
    channel: u8,
    note: u8,
    note_on_time: u64,
) -> u64 {
    let mut value = note_on_time
        ^ ((track_index as u64) << 48)
        ^ ((channel as u64) << 40)
        ^ ((note as u64) << 32);
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

pub(super) fn centered_i64(seed: u64, range: i64) -> i64 {
    if range <= 0 {
        return 0;
    }
    (seed % (range as u64 * 2 + 1)) as i64 - range
}

pub(super) fn apply_sample_offset(time_samples: u64, offset: i64) -> u64 {
    if offset >= 0 {
        time_samples.saturating_add(offset as u64)
    } else {
        time_samples.saturating_sub(offset.unsigned_abs())
    }
}

pub(super) fn apply_velocity_offset(velocity: u8, offset: i16) -> u8 {
    (velocity as i16 + offset).clamp(1, 127) as u8
}

pub(super) fn note_duration_for_articulation(note_event: &NoteEvent) -> f64 {
    let base = if note_event.grace {
        0.125
    } else {
        note_event.duration.to_beats()
    };
    match note_event.articulation {
        Some(Articulation::Staccatissimo) => base * 0.25,
        Some(Articulation::Staccato | Articulation::Detached | Articulation::Pizzicato) => {
            base * 0.5
        }
        Some(Articulation::Tenuto | Articulation::Sustained) => base * 0.95,
        Some(Articulation::Legato) => base * 1.05,
        _ => base,
    }
}

pub(super) fn note_duration_for_profile(
    note_event: &NoteEvent,
    profile: &InstrumentProfile,
) -> f64 {
    let duration = note_duration_for_articulation(note_event);
    if let Some(articulation) = note_event.articulation {
        duration
            * profile
                .duration_scale_for_articulation(articulation)
                .unwrap_or(1.0)
    } else {
        duration
    }
}

pub(super) use oc_dsl::automation::{
    automation_param_to_cc, expression_param_to_cc, normalize_param_name,
};

pub use oc_dsl::automation::decode_plugin_automation_param;

pub(super) fn param_value_to_midi(value: &ParamValue) -> Option<u8> {
    match value {
        ParamValue::String(s) => dynamics_name_to_velocity(s),
        ParamValue::Percentage(v) => Some((*v * 127.0 / 100.0).clamp(0.0, 127.0) as u8),
        ParamValue::Db(v) => Some(db_to_midi_volume(*v)),
        ParamValue::Bool(v) => Some(if *v { 127 } else { 0 }),
        _ => value.as_f64().map(|v| {
            if (0.0..=1.0).contains(&v) {
                (v * 127.0) as u8
            } else {
                v.clamp(0.0, 127.0) as u8
            }
        }),
    }
}

pub(super) fn param_value_to_midi14(value: &ParamValue) -> Option<u16> {
    match value {
        ParamValue::String(s) => dynamics_name_to_velocity(s).map(|v| v as u16 * 129),
        ParamValue::Percentage(v) => Some((*v * 16383.0 / 100.0).clamp(0.0, 16383.0) as u16),
        ParamValue::Db(v) => Some(db_to_midi_volume(*v) as u16 * 129),
        ParamValue::Bool(v) => Some(if *v { 16383 } else { 0 }),
        _ => value.as_f64().map(|v| {
            if (0.0..=1.0).contains(&v) {
                (v * 16383.0) as u16
            } else {
                v.clamp(0.0, 16383.0) as u16
            }
        }),
    }
}

/// Normalized 0..1 value the interpreter sends for one lane point.
pub fn param_value_to_normalized(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::String(s) => dynamics_name_to_velocity(s).map(|v| v as f64 / 127.0),
        ParamValue::Percentage(v) => Some((*v / 100.0).clamp(0.0, 1.0)),
        ParamValue::Db(v) => Some(db_to_midi_volume(*v) as f64 / 127.0),
        ParamValue::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
        _ => value.as_f64().map(|v| {
            if (0.0..=1.0).contains(&v) {
                v
            } else {
                (v / 127.0).clamp(0.0, 1.0)
            }
        }),
    }
}

/// True when a lane point carries a physical unit instead of a normalized
/// position (an explicit Hz, s or ct value, or a bare number outside 0..1).
pub fn lane_param_value_is_physical(value: &ParamValue) -> bool {
    matches!(
        value,
        ParamValue::Frequency(_) | ParamValue::Time(_) | ParamValue::Cents(_)
    ) || matches!(value, ParamValue::Number(v) if !(0.0..=1.0).contains(v))
}

/// Normalized value a lane point writes to one instrument parameter.
///
/// On the built-in synth a point that carries a unit is read in the parameter's
/// own unit, the unit its `params:` entry uses; a bare number in 0..1 stays a
/// normalized position, which is what the patch editor writes.
pub fn lane_param_value_to_normalized(
    name: &str,
    value: &ParamValue,
    builtin_synth: bool,
) -> Option<f64> {
    if !builtin_synth {
        return param_value_to_normalized(value);
    }
    let physical = match value {
        value if lane_param_value_is_physical(value) => value.as_f64(),
        _ => None,
    };
    physical
        .and_then(|physical| oc_synth::synth::physical_param_to_normalized(name, physical))
        .or_else(|| param_value_to_normalized(value))
}

pub(super) fn dynamics_name_to_velocity(name: &str) -> Option<u8> {
    Dynamics::from_token(name).map(|dynamics| dynamics.to_velocity())
}

pub(super) fn cents_to_pitch_bend(cents: f64, range_semitones: f64) -> i16 {
    let full_scale_cents = range_semitones.clamp(0.01, 96.0) * 100.0;
    (cents / full_scale_cents * 8192.0)
        .round()
        .clamp(-8192.0, 8191.0) as i16
}

// ============================================================================
// Utility
// ============================================================================

/// Convert beats to sample position
pub fn beats_to_samples(beats: f64, tempo: f64, sample_rate: u32) -> u64 {
    let seconds = beats * 60.0 / tempo;
    (seconds * sample_rate as f64) as u64
}

/// Convert dB volume to MIDI CC value (0-127)
pub(super) fn db_to_midi_volume(db: f64) -> u8 {
    // 0 dB = 100 (roughly), -inf = 0, +6 dB = 127
    let linear = 10.0_f64.powf(db / 20.0);
    (linear * 100.0).clamp(0.0, 127.0) as u8
}

/// Convert pan (-100..+100) to MIDI pan CC (0=left, 64=center, 127=right)
pub(super) fn pan_to_midi(pan: f64) -> u8 {
    ((pan / 100.0 * 63.0) + 64.0).clamp(0.0, 127.0) as u8
}
