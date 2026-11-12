//! General MIDI program identity and existing instrument playback conventions.
//!
//! A broad instrument suggestion is deliberately not the inverse of playback
//! mapping. Exact MIDI identity belongs to a stored program number.
use serde::Serialize;

// Canonical GM names and stable picker names have distinct display roles.
const PROGRAM_NAMES: [(&str, &str); 128] = [
    ("Acoustic Grand Piano", "Acoustic Grand"),
    ("Bright Acoustic Piano", "Bright Acoustic"),
    ("Electric Grand Piano", "Electric Grand"),
    ("Honky-tonk Piano", "Honky-tonk"),
    ("Electric Piano 1", "Electric Piano 1"),
    ("Electric Piano 2", "Electric Piano 2"),
    ("Harpsichord", "Harpsichord"),
    ("Clavinet", "Clavinet"),
    ("Celesta", "Celesta"),
    ("Glockenspiel", "Glockenspiel"),
    ("Music Box", "Music Box"),
    ("Vibraphone", "Vibraphone"),
    ("Marimba", "Marimba"),
    ("Xylophone", "Xylophone"),
    ("Tubular Bells", "Tubular Bells"),
    ("Dulcimer", "Dulcimer"),
    ("Drawbar Organ", "Drawbar Organ"),
    ("Percussive Organ", "Percussive Organ"),
    ("Rock Organ", "Rock Organ"),
    ("Church Organ", "Church Organ"),
    ("Reed Organ", "Reed Organ"),
    ("Accordion", "Accordion"),
    ("Harmonica", "Harmonica"),
    ("Tango Accordion", "Tango Accordion"),
    ("Acoustic Guitar (nylon)", "Nylon Guitar"),
    ("Acoustic Guitar (steel)", "Steel Guitar"),
    ("Electric Guitar (jazz)", "Jazz Guitar"),
    ("Electric Guitar (clean)", "Clean Electric"),
    ("Electric Guitar (muted)", "Muted Guitar"),
    ("Overdriven Guitar", "Overdrive Guitar"),
    ("Distortion Guitar", "Distortion Guitar"),
    ("Guitar Harmonics", "Guitar Harmonics"),
    ("Acoustic Bass", "Acoustic Bass"),
    ("Electric Bass (finger)", "Finger Bass"),
    ("Electric Bass (pick)", "Pick Bass"),
    ("Fretless Bass", "Fretless Bass"),
    ("Slap Bass 1", "Slap Bass 1"),
    ("Slap Bass 2", "Slap Bass 2"),
    ("Synth Bass 1", "Synth Bass 1"),
    ("Synth Bass 2", "Synth Bass 2"),
    ("Violin", "Violin"),
    ("Viola", "Viola"),
    ("Cello", "Cello"),
    ("Contrabass", "Contrabass"),
    ("Tremolo Strings", "Tremolo Strings"),
    ("Pizzicato Strings", "Pizzicato Strings"),
    ("Orchestral Harp", "Orchestral Harp"),
    ("Timpani", "Timpani"),
    ("String Ensemble 1", "String Ensemble 1"),
    ("String Ensemble 2", "String Ensemble 2"),
    ("Synth Strings 1", "Synth Strings 1"),
    ("Synth Strings 2", "Synth Strings 2"),
    ("Choir Aahs", "Choir Aahs"),
    ("Voice Oohs", "Voice Oohs"),
    ("Synth Voice", "Synth Voice"),
    ("Orchestra Hit", "Orchestra Hit"),
    ("Trumpet", "Trumpet"),
    ("Trombone", "Trombone"),
    ("Tuba", "Tuba"),
    ("Muted Trumpet", "Muted Trumpet"),
    ("French Horn", "French Horn"),
    ("Brass Section", "Brass Section"),
    ("Synth Brass 1", "Synth Brass 1"),
    ("Synth Brass 2", "Synth Brass 2"),
    ("Soprano Sax", "Soprano Sax"),
    ("Alto Sax", "Alto Sax"),
    ("Tenor Sax", "Tenor Sax"),
    ("Baritone Sax", "Baritone Sax"),
    ("Oboe", "Oboe"),
    ("English Horn", "English Horn"),
    ("Bassoon", "Bassoon"),
    ("Clarinet", "Clarinet"),
    ("Piccolo", "Piccolo"),
    ("Flute", "Flute"),
    ("Recorder", "Recorder"),
    ("Pan Flute", "Pan Flute"),
    ("Blown Bottle", "Blown Bottle"),
    ("Shakuhachi", "Shakuhachi"),
    ("Whistle", "Whistle"),
    ("Ocarina", "Ocarina"),
    ("Lead 1 (square)", "Square Lead"),
    ("Lead 2 (sawtooth)", "Sawtooth Lead"),
    ("Lead 3 (calliope)", "Calliope Lead"),
    ("Lead 4 (chiff)", "Chiff Lead"),
    ("Lead 5 (charang)", "Charang Lead"),
    ("Lead 6 (voice)", "Voice Lead"),
    ("Lead 7 (fifths)", "Fifths Lead"),
    ("Lead 8 (bass + lead)", "Bass+Lead"),
    ("Pad 1 (new age)", "New Age Pad"),
    ("Pad 2 (warm)", "Warm Pad"),
    ("Pad 3 (polysynth)", "Polysynth Pad"),
    ("Pad 4 (choir)", "Choir Pad"),
    ("Pad 5 (bowed)", "Bowed Pad"),
    ("Pad 6 (metallic)", "Metallic Pad"),
    ("Pad 7 (halo)", "Halo Pad"),
    ("Pad 8 (sweep)", "Sweep Pad"),
    ("FX 1 (rain)", "Rain"),
    ("FX 2 (soundtrack)", "Soundtrack"),
    ("FX 3 (crystal)", "Crystal"),
    ("FX 4 (atmosphere)", "Atmosphere"),
    ("FX 5 (brightness)", "Brightness"),
    ("FX 6 (goblins)", "Goblins"),
    ("FX 7 (echoes)", "Echoes"),
    ("FX 8 (sci-fi)", "Sci-Fi"),
    ("Sitar", "Sitar"),
    ("Banjo", "Banjo"),
    ("Shamisen", "Shamisen"),
    ("Koto", "Koto"),
    ("Kalimba", "Kalimba"),
    ("Bag pipe", "Bagpipe"),
    ("Fiddle", "Fiddle"),
    ("Shanai", "Shanai"),
    ("Tinkle Bell", "Tinkle Bell"),
    ("Agogo", "Agogo"),
    ("Steel Drums", "Steel Drums"),
    ("Woodblock", "Woodblock"),
    ("Taiko Drum", "Taiko Drum"),
    ("Melodic Tom", "Melodic Tom"),
    ("Synth Drum", "Synth Drum"),
    ("Reverse Cymbal", "Reverse Cymbal"),
    ("Guitar Fret Noise", "Guitar Fret Noise"),
    ("Breath Noise", "Breath Noise"),
    ("Seashore", "Seashore"),
    ("Bird Tweet", "Bird Tweet"),
    ("Telephone Ring", "Telephone Ring"),
    ("Helicopter", "Helicopter"),
    ("Applause", "Applause"),
    ("Gunshot", "Gunshot"),
];

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
/// One General MIDI program with its picker metadata.
pub struct Program {
    pub program: u8,
    pub name: &'static str,
    /// Stable source name retained by picker callbacks and recent items.
    pub picker_name: &'static str,
    pub family: &'static str,
    /// Stable picker category; translated labels and icons are UI concerns.
    pub category: &'static str,
}

const FAMILIES: [(&str, &str); 16] = [
    ("piano", "Piano"),
    ("chromatic_percussion", "Chromatic"),
    ("organ", "Organ"),
    ("guitar", "Guitar"),
    ("bass", "Bass"),
    ("strings", "Strings"),
    ("ensemble", "Ensemble"),
    ("brass", "Brass"),
    ("reed", "Woodwinds"),
    ("pipe", "Woodwinds"),
    ("synth_lead", "Synth"),
    ("synth_pad", "Pad"),
    ("synth_effects", "SynthFX"),
    ("world", "World"),
    ("percussion", "Percussion"),
    ("sound_effects", "SFX"),
];

/// Look up a GM program by seven-bit number.
pub fn program(program: u8) -> Option<Program> {
    let (name, picker_name) = *PROGRAM_NAMES.get(usize::from(program))?;
    let (family, category) = FAMILIES[usize::from(program / 8)];
    Some(Program {
        program,
        name,
        picker_name,
        family,
        category,
    })
}

/// Every GM program in number order.
pub fn programs() -> Vec<Program> {
    (0..=127)
        .map(|id| program(id).expect("seven-bit GM program"))
        .collect()
}

/// Existing interpreter mapping, including historical family fallbacks.
pub fn playback_program(category: &str, name: &str) -> Option<u8> {
    Some(match (category, name) {
        // Piano
        ("piano", "acoustic_grand") => 0,
        ("piano", "bright_acoustic") => 1,
        ("piano", "electric_grand") => 2,
        ("piano", "electric") => 4,
        ("piano", _) => 0,

        // Organ
        ("organ", "drawbar") => 16,
        ("organ", "church") => 19,
        ("organ", _) => 16,

        // Guitar
        ("guitar", "acoustic_nylon") => 24,
        ("guitar", "acoustic_steel") => 25,
        ("guitar", "electric_jazz") => 26,
        ("guitar", "electric_clean") => 27,
        ("guitar", "electric_muted") => 28,
        ("guitar", "overdriven") => 29,
        ("guitar", "distortion") => 30,
        ("guitar", _) => 25,

        // Bass
        ("bass", "acoustic") => 32,
        ("bass", "electric_finger") => 33,
        ("bass", "electric_pick") => 34,
        ("bass", "fretless") => 35,
        ("bass", "slap") => 36,
        ("bass", _) => 33,

        // Strings
        ("strings", "violin") => 40,
        ("strings", "violin_section") => 48, // String Ensemble 1
        ("strings", "viola") => 41,
        ("strings", "viola_section") => 48,
        ("strings", "cello") => 42,
        ("strings", "cello_section") => 48,
        ("strings", "contrabass") => 43,
        ("strings", "section") => 48,
        ("strings", _) => 48,

        // Brass
        ("brass", "trumpet") => 56,
        ("brass", "trombone") => 57,
        ("brass", "tuba") => 58,
        ("brass", "french_horn") => 60,
        ("brass", "section") => 61,
        ("brass", _) => 56,

        // Woodwinds
        ("woodwinds", "flute") => 73,
        ("woodwinds", "piccolo") => 72,
        ("woodwinds", "clarinet") => 71,
        ("woodwinds", "oboe") => 68,
        ("woodwinds", "bassoon") => 70,
        ("woodwinds", "soprano_sax") => 64,
        ("woodwinds", "alto_sax") => 65,
        ("woodwinds", "tenor_sax") => 66,
        ("woodwinds", _) => 73,

        // Synth
        ("synth", "lead_saw") => 81,
        ("synth", "lead_square") => 80,
        ("synth", "pad_warm") => 89,
        ("synth", "pad_choir") => 91,
        ("synth", "bass_sub") => 38,
        ("synth", "fm_electric_piano") => 4,
        ("synth", _) => 81,

        // Drums (program doesn't matter for channel 9, but set anyway)
        ("drums", _) => 0,

        // Percussion
        ("percussion", "timpani") => 47,
        ("percussion", _) => 47,

        // Vocal
        ("vocal", "choir") => 52,
        ("vocal", _) => 54,

        // Fallback
        _ => return None,
    })
}

/// Broad path used when suggesting an instrument for an imported program.
pub fn suggested_instrument(program: u8) -> Option<(&'static str, &'static str)> {
    Some(match program {
        0..=5 => ("piano", "acoustic_grand"),
        6 => ("piano", "harpsichord"),
        7 => ("piano", "clavinet"),
        8..=15 => ("percussion", "chromatic"),
        16..=23 => ("organ", "drawbar"),
        24..=31 => ("guitar", "acoustic_steel"),
        32..=39 => ("bass", "electric_finger"),
        40 => ("strings", "violin"),
        41 => ("strings", "viola"),
        42 => ("strings", "cello"),
        43 => ("strings", "contrabass"),
        44..=51 => ("strings", "section"),
        52..=55 => ("vocal", "choir"),
        56..=59 => ("brass", "trumpet"),
        60 => ("brass", "french_horn"),
        61..=63 => ("brass", "section"),
        64..=67 => ("woodwinds", "alto_sax"),
        68 => ("woodwinds", "oboe"),
        69 => ("woodwinds", "english_horn"),
        70 => ("woodwinds", "bassoon"),
        71 => ("woodwinds", "clarinet"),
        72 => ("woodwinds", "piccolo"),
        73 => ("woodwinds", "flute"),
        74..=79 => ("woodwinds", "flute"),
        80..=87 => ("synth", "lead_saw"),
        88..=95 => ("synth", "pad_warm"),
        96..=103 => ("synth", "lead_saw"),
        104..=111 => ("guitar", "acoustic_steel"),
        112..=119 => ("percussion", "chromatic"),
        120..=127 => ("synth", "pad_warm"),
        _ => return None,
    })
}

/// Render the GM table as the generated TypeScript module.
pub fn render_typescript() -> String {
    let json = serde_json::to_string_pretty(&programs()).expect("GM programs are serializable");
    format!(
        "// @generated by `cargo run -p oc-device-schema --bin gen-device-catalog`.\n// Source of truth: crates/oc-device-schema/src/gm.rs. Do not edit by hand.\n\nexport interface GmProgram {{\n  program: number;\n  name: string;\n  pickerName: string;\n  family: string;\n  category: string;\n}}\nexport const GM_PROGRAMS: GmProgram[] = {json};\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm_program_identity_names_and_families_cover_all_128_programs() {
        let rows = programs();
        assert_eq!(rows.len(), 128);
        let names: std::collections::BTreeSet<_> = rows.iter().map(|row| row.name).collect();
        assert_eq!(names.len(), 128);
        for (id, row) in rows.iter().enumerate() {
            assert_eq!(usize::from(row.program), id);
            assert!(!row.name.is_empty());
            assert_eq!((row.family, row.category), FAMILIES[id / 8]);
            assert!(suggested_instrument(row.program).is_some());
        }
        assert_eq!(program(0).unwrap().name, "Acoustic Grand Piano");
        assert_eq!(program(127).unwrap().name, "Gunshot");
        assert!(program(128).is_none());
        assert!(program(255).is_none());
        assert!(suggested_instrument(128).is_none());
    }
    #[test]
    fn playback_mapping_preserves_the_previous_interpreter_for_known_paths_and_family_fallbacks() {
        for (category, name, expected) in [
            ("piano", "acoustic_grand", 0),
            ("piano", "bright_acoustic", 1),
            ("piano", "electric_grand", 2),
            ("piano", "electric", 4),
            ("organ", "drawbar", 16),
            ("organ", "church", 19),
            ("guitar", "acoustic_nylon", 24),
            ("guitar", "acoustic_steel", 25),
            ("guitar", "electric_jazz", 26),
            ("guitar", "electric_clean", 27),
            ("guitar", "electric_muted", 28),
            ("guitar", "overdriven", 29),
            ("guitar", "distortion", 30),
            ("bass", "acoustic", 32),
            ("bass", "electric_finger", 33),
            ("bass", "electric_pick", 34),
            ("bass", "fretless", 35),
            ("bass", "slap", 36),
            ("strings", "violin", 40),
            ("strings", "violin_section", 48),
            ("strings", "viola", 41),
            ("strings", "viola_section", 48),
            ("strings", "cello", 42),
            ("strings", "cello_section", 48),
            ("strings", "contrabass", 43),
            ("strings", "section", 48),
            ("brass", "trumpet", 56),
            ("brass", "trombone", 57),
            ("brass", "tuba", 58),
            ("brass", "french_horn", 60),
            ("brass", "section", 61),
            ("woodwinds", "flute", 73),
            ("woodwinds", "piccolo", 72),
            ("woodwinds", "clarinet", 71),
            ("woodwinds", "oboe", 68),
            ("woodwinds", "bassoon", 70),
            ("woodwinds", "soprano_sax", 64),
            ("woodwinds", "alto_sax", 65),
            ("woodwinds", "tenor_sax", 66),
            ("synth", "lead_saw", 81),
            ("synth", "lead_square", 80),
            ("synth", "pad_warm", 89),
            ("synth", "pad_choir", 91),
            ("synth", "bass_sub", 38),
            ("synth", "fm_electric_piano", 4),
            ("percussion", "timpani", 47),
            ("vocal", "choir", 52),
            ("piano", "unknown_family_member", 0),
            ("organ", "unknown_family_member", 16),
            ("guitar", "unknown_family_member", 25),
            ("bass", "unknown_family_member", 33),
            ("strings", "unknown_family_member", 48),
            ("brass", "unknown_family_member", 56),
            ("woodwinds", "unknown_family_member", 73),
            ("synth", "unknown_family_member", 81),
            ("drums", "unknown_family_member", 0),
            ("percussion", "unknown_family_member", 47),
            ("vocal", "unknown_family_member", 54),
        ] {
            assert_eq!(
                playback_program(category, name),
                Some(expected),
                "{category}/{name}"
            );
        }
        assert_eq!(playback_program("guitar", "acoustic_steel"), Some(25));
        assert_eq!(playback_program("bass", "electric_finger"), Some(33));
        assert_eq!(playback_program("unknown", "unknown"), None);
        assert_eq!(suggested_instrument(24), suggested_instrument(25));
        assert_ne!(playback_program("guitar", "acoustic_steel"), Some(24));
    }
    #[test]
    fn picker_names_preserve_every_existing_program_alias() {
        let expected: Vec<String> =
            serde_json::from_str(include_str!("../tests/fixtures/gm-picker-names.json")).unwrap();
        assert_eq!(
            programs()
                .iter()
                .map(|entry| entry.picker_name)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(program(0).unwrap().name, "Acoustic Grand Piano");
        assert_eq!(program(0).unwrap().picker_name, "Acoustic Grand");
        assert_eq!(program(25).unwrap().picker_name, "Steel Guitar");
        assert_eq!(program(33).unwrap().picker_name, "Finger Bass");
    }

    #[test]
    fn generated_gm_typescript_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../oc-gui/frontend/src/data/gmPrograms.gen.ts");
        assert_eq!(
            std::fs::read_to_string(path).expect("generated GM programs exist"),
            render_typescript()
        );
    }
}
