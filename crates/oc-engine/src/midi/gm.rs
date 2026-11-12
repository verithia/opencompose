//! General MIDI names, instrument suggestions and controller conventions.

use oc_dsl::ast::{Accidental, InstrumentPath, KeyMode, KeySignature, NoteName};

pub(crate) fn gm_program_name(program: u8) -> &'static str {
    oc_dsl::gm::program(program)
        .map(|entry| entry.name)
        .unwrap_or("Unknown GM program")
}

pub(crate) fn gm_program_instrument(program: u8) -> InstrumentPath {
    oc_dsl::gm::suggested_instrument(program)
        .unwrap_or_else(|| InstrumentPath::new("piano", "acoustic_grand"))
}

/// Use the same persisted-path playback mapping as the interpreter.
pub(crate) fn instrument_gm_program(path: &InstrumentPath) -> Option<u8> {
    oc_dsl::gm::playback_program(&path.category, &path.name)
}

pub(crate) fn is_drum_instrument(path: &InstrumentPath) -> bool {
    path.category == "drums"
}

pub(crate) fn drum_name_to_midi(name: &str) -> u8 {
    match normalize_param(name).as_str() {
        "kick" | "bassdrum" => 36,
        "rim" | "rimshot" | "crossstick" => 37,
        "snare" | "snaredrum" => 38,
        "clap" | "handclap" => 39,
        "tomlow" | "lowtom" => 41,
        "hihat" | "closedhihat" | "hihatclosed" => 42,
        "tommid" | "midtom" => 45,
        "hihatopen" | "openhihat" | "openhat" => 46,
        "tomhigh" | "hightom" => 48,
        "crash" | "crashcymbal" => 49,
        "ride" | "ridecymbal" => 51,
        "china" | "chinacymbal" => 52,
        "tambourine" => 54,
        "splash" | "splashcymbal" => 55,
        "cowbell" => 56,
        "cabasa" => 69,
        "shaker" => 70,
        "guiro" => 73,
        "woodblock" => 75,
        _ => 38,
    }
}

/// CC controllers that become named automation lanes on import.
pub(crate) fn cc_to_lane(controller: u8) -> Option<&'static str> {
    Some(match controller {
        1 => "mod",
        7 => "volume",
        10 => "pan",
        11 => "expression",
        64 => "sustain",
        71 => "resonance",
        74 => "brightness",
        _ => return None,
    })
}

pub(crate) fn lane_to_cc(param: &str) -> Option<u8> {
    Some(match normalize_param(param).as_str() {
        "mod" | "modulation" | "vibrato" => 1,
        "volume" | "vol" => 7,
        "pan" => 10,
        "expression" | "dynamics" => 11,
        "sustain" | "sustainpedal" | "damper" | "damperpedal" | "halfpedal" => 64,
        "resonance" => 71,
        "brightness" | "timbre" => 74,
        _ => return None,
    })
}

pub(crate) fn is_pitch_bend_lane(param: &str) -> bool {
    matches!(normalize_param(param).as_str(), "pitchbend" | "bend")
}

pub(crate) fn is_pressure_lane(param: &str) -> bool {
    normalize_param(param) == "pressure"
}

pub(crate) fn normalize_param(param: &str) -> String {
    param
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub(crate) fn pitch_bend_to_semitones(raw: i16, range: f64) -> f64 {
    f64::from(raw) / 8192.0 * range
}

pub(crate) fn semitones_to_pitch_bend(semitones: f64, range: f64) -> i16 {
    (semitones / range * 8192.0).round().clamp(-8192.0, 8191.0) as i16
}

pub(crate) fn midi_key_signature(sharps_flats: i8, minor: bool) -> KeySignature {
    let (root, accidental) = key_spelling(sharps_flats, minor);
    KeySignature {
        root,
        accidental,
        mode: if minor {
            KeyMode::Minor
        } else {
            KeyMode::Major
        },
    }
}

/// Returns the MIDI key signature, or the nearest enharmonic spelling.
pub(crate) fn key_signature_midi(key: &KeySignature) -> (i8, bool) {
    let minor = key.mode == KeyMode::Minor;
    for sf in -7..=7 {
        if key_spelling(sf, minor) == (key.root, key.accidental) {
            return (sf, minor);
        }
    }
    let pitch = pitch_class(key);
    let mut best = 0;
    for sf in -7..=7 {
        let (root, accidental) = key_spelling(sf, minor);
        if pitch_class(&KeySignature {
            root,
            accidental,
            mode: key.mode,
        }) == pitch
        {
            best = sf;
            if accidental == key.accidental {
                break;
            }
        }
    }
    (best, minor)
}

fn pitch_class(key: &KeySignature) -> i16 {
    let base: i16 = match key.root {
        NoteName::C => 0,
        NoteName::D => 2,
        NoteName::E => 4,
        NoteName::F => 5,
        NoteName::G => 7,
        NoteName::A => 9,
        NoteName::B => 11,
    };
    let offset: i16 = match key.accidental {
        Accidental::Natural => 0,
        Accidental::Sharp => 1,
        Accidental::DoubleSharp => 2,
        Accidental::Flat => -1,
        Accidental::DoubleFlat => -2,
    };
    (base + offset).rem_euclid(12)
}

fn key_spelling(sharps_flats: i8, minor: bool) -> (NoteName, Accidental) {
    let index = (i16::from(sharps_flats) + 7).clamp(0, 14) as usize;
    if minor {
        const MINOR: [(NoteName, Accidental); 15] = [
            (NoteName::A, Accidental::Flat),
            (NoteName::E, Accidental::Flat),
            (NoteName::B, Accidental::Flat),
            (NoteName::F, Accidental::Natural),
            (NoteName::C, Accidental::Natural),
            (NoteName::G, Accidental::Natural),
            (NoteName::D, Accidental::Natural),
            (NoteName::A, Accidental::Natural),
            (NoteName::E, Accidental::Natural),
            (NoteName::B, Accidental::Natural),
            (NoteName::F, Accidental::Sharp),
            (NoteName::C, Accidental::Sharp),
            (NoteName::G, Accidental::Sharp),
            (NoteName::D, Accidental::Sharp),
            (NoteName::A, Accidental::Sharp),
        ];
        MINOR[index]
    } else {
        const MAJOR: [(NoteName, Accidental); 15] = [
            (NoteName::C, Accidental::Flat),
            (NoteName::G, Accidental::Flat),
            (NoteName::D, Accidental::Flat),
            (NoteName::A, Accidental::Flat),
            (NoteName::E, Accidental::Flat),
            (NoteName::B, Accidental::Flat),
            (NoteName::F, Accidental::Natural),
            (NoteName::C, Accidental::Natural),
            (NoteName::G, Accidental::Natural),
            (NoteName::D, Accidental::Natural),
            (NoteName::A, Accidental::Natural),
            (NoteName::E, Accidental::Natural),
            (NoteName::B, Accidental::Natural),
            (NoteName::F, Accidental::Sharp),
            (NoteName::C, Accidental::Sharp),
        ];
        MAJOR[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_spellings_round_trip_the_midi_range() {
        for minor in [false, true] {
            for sf in -7..=7 {
                let key = midi_key_signature(sf, minor);
                assert_eq!(key_signature_midi(&key), (sf, minor));
            }
        }
    }

    #[test]
    fn lane_and_program_maps_are_stable() {
        assert_eq!(lane_to_cc("brightness"), Some(74));
        assert_eq!(lane_to_cc("resonance"), Some(71));
        assert_eq!(cc_to_lane(64), Some("sustain"));
        assert_eq!(gm_program_name(40), "Violin");
        assert_eq!(instrument_gm_program(&gm_program_instrument(40)), Some(40));
        assert_eq!(drum_name_to_midi("kick"), 36);
    }
}
