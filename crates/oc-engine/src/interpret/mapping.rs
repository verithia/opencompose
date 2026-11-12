//! GM program and drum-note mapping.

use super::*;

/// Resolve the GM program for a track: check preset first, fallback to instrument path.
pub(super) fn resolve_gm_program(track: &Track, presets: &[PresetDef]) -> u8 {
    if let Some(preset_name) = &track.preset
        && let Some(preset) = presets.iter().find(|p| p.name == *preset_name)
    {
        // If preset has an explicit program number, use it
        if let Some(program) = preset.program {
            return program;
        }
        // If preset has source=soundfont but no program, default to 0
        if preset.source.as_deref() == Some("soundfont") {
            return 0;
        }
        // For non-soundfont presets (clap, builtin, sample), use program 0
        // The actual rendering will be handled by the plugin system
        return 0;
    }
    // Fallback to instrument path mapping
    instrument_to_gm_program(&track.instrument)
}

/// Map an OpenCompose instrument path to a General MIDI program number (0-127)
pub fn instrument_to_gm_program(path: &InstrumentPath) -> u8 {
    oc_dsl::gm::instrument_to_program(path)
}

/// True when `name` is a General MIDI percussion name this build knows.
pub fn is_gm_drum_name(name: &str) -> bool {
    gm_drum_note(name).is_some()
}

fn gm_drum_note(name: &str) -> Option<u8> {
    Some(match name {
        "kick" => 36,
        "snare" => 38,
        "hihat" => 42, // Closed Hi-Hat (default)
        "tom_high" => 50,
        "tom_mid" => 47,
        "tom_low" => 41,
        "crash" => 49,
        "ride" => 51,
        "splash" => 55,
        "china" => 52,
        "cowbell" => 56,
        "clap" => 39,
        "rim" => 37,
        "shaker" => 70,
        "tambourine" => 54,
        "cabasa" => 69,
        "guiro" => 73,
        "woodblock" => 76,
        _ => return None,
    })
}

/// Map an OpenCompose drum name to a General MIDI percussion note number
pub fn drum_to_gm_note(name: &str) -> u8 {
    gm_drum_note(name).unwrap_or(38) // default to snare
}

pub(super) fn drum_to_gm_note_with_articulation(
    name: &str,
    articulation: Option<DrumArticulation>,
) -> u8 {
    match (name, articulation) {
        ("hihat", Some(DrumArticulation::Open)) => 46,
        ("hihat", Some(DrumArticulation::HalfOpen)) => 44,
        ("hihat", Some(DrumArticulation::Closed)) => 42,
        ("snare", Some(DrumArticulation::RimShot)) => 37,
        ("snare", Some(DrumArticulation::CrossStick)) => 37,
        _ => drum_to_gm_note(name),
    }
}

pub(super) fn drum_to_profile_or_gm_note(
    name: &str,
    articulation: Option<DrumArticulation>,
    profile: &InstrumentProfile,
    track: &str,
) -> u8 {
    if let Some(note) = profile.drum_note(name, articulation.map(drum_articulation_key)) {
        return note;
    }
    let note = drum_to_gm_note_with_articulation(name, articulation);
    // A pad grid is addressed by pad name. A name it does not declare falls back
    // to a General MIDI note, which silently plays the wrong pad (or none).
    if profile.has_pad_grid() && !(is_gm_drum_name(name) && profile.has_pad_note(note)) {
        let warning = match (is_gm_drum_name(name), profile.has_pad_note(note)) {
            (true, false) => format!(
                "track '{track}': drum hit '{name}' plays General MIDI note {note}, which no pad of this kit has; the hit is silent"
            ),
            (false, true) => format!(
                "track '{track}': drum hit '{name}' names no pad of this kit and no General MIDI drum; it plays note {note}, the pad that holds that note"
            ),
            (false, false) => format!(
                "track '{track}': drum hit '{name}' names no pad of this kit and no General MIDI drum, and note {note} is not a pad either; the hit is silent"
            ),
            (true, true) => unreachable!("filtered above"),
        };
        crate::render_warnings::push(warning);
    }
    note
}

pub(super) fn drum_articulation_key(articulation: DrumArticulation) -> &'static str {
    match articulation {
        DrumArticulation::Open => "open",
        DrumArticulation::Closed => "closed",
        DrumArticulation::HalfOpen => "halfopen",
        DrumArticulation::RimShot => "rimshot",
        DrumArticulation::CrossStick => "crossstick",
        DrumArticulation::GhostNote => "ghostnote",
        DrumArticulation::Flam => "flam",
        DrumArticulation::Roll => "roll",
    }
}

pub(super) fn apply_drum_articulation_velocity(
    velocity: u8,
    articulation: Option<DrumArticulation>,
) -> u8 {
    match articulation {
        Some(DrumArticulation::GhostNote) => velocity.saturating_sub(40).max(1),
        Some(DrumArticulation::RimShot) => velocity.saturating_add(12).min(127),
        _ => velocity,
    }
}

pub(super) fn apply_articulation_velocity(velocity: u8, articulation: Option<Articulation>) -> u8 {
    match articulation {
        Some(Articulation::Accent | Articulation::Marcato | Articulation::Sforzando) => {
            velocity.saturating_add(15).min(127)
        }
        Some(Articulation::GhostNote) => velocity.saturating_sub(40).max(1),
        _ => velocity,
    }
}
