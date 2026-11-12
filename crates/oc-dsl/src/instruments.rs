//! Default instruments for a track that names only an instrument family.
//!
//! `instrument:` is a General MIDI hint, not a sound source. Left alone, a
//! document that declares no preset would fall through to the SoundFont player
//! and refuse to render without a `.sf2`. This pass gives such a track a built-in
//! OpenSynth preset for its family (or the default drum kit) and writes the
//! preset block into the document, so the injected choice is visible in the
//! canonical DSL the GUI and tools return.
//!
//! The pass runs at the boundaries that materialise a project for rendering or
//! editing, never in `ocp fmt`/`export`: a plain formatting pass must not turn a
//! bare track into a different-sounding document. Presets this pass owns use
//! the reserved `oc_default_` namespace, so an explicitly supplied legacy
//! SoundFont can still win over an injected default (see [`clear_injected`]).
//!
//! An explicit preset is never replaced. The pass is idempotent: a second call
//! finds the preset already assigned and changes nothing.

use std::collections::BTreeMap;

use crate::ast::{InstrumentPath, ParamValue, PresetDef, Project, Track};

const SYNTH_PLUGIN: &str = "builtin.open_synth";

/// Prefix reserved for presets this module injects. A user preset that starts
/// with it is treated as injected, so do not use the namespace.
pub const INJECTED_PREFIX: &str = "oc_default_";

/// True when a preset name belongs to the injected namespace.
pub fn is_injected(name: &str) -> bool {
    name.starts_with(INJECTED_PREFIX)
}

/// A family default: the document block and the parameters that make it.
struct Patch {
    name: &'static str,
    plugin: &'static str,
    sound: Sound,
}

enum Sound {
    Synth(&'static [(&'static str, f64)]),
    DrumKit,
}

/// The factory keys patch: damped strings with a hammer, not a held pad.
///
/// The amp envelope has no sustain floor, so every note decays to silence; its
/// shape is exponential (dB-linear) and `amp.damping` shortens the decay and the
/// release with the key and with a short hold. Env3 drives the hammer: a bright
/// layer two octaves up plus a brief filter opening, both gone in 30 ms.
static KEYS: Patch = Patch {
    name: "oc_default_keys",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 3.0),
        ("osc.a.level", 0.55),
        ("osc.b.waveform", 1.0),
        ("osc.b.level", 0.27),
        ("osc.b.semitones", -12.0),
        ("osc.c.waveform", 1.0),
        ("osc.c.level", 0.0),
        ("osc.c.semitones", 24.0),
        ("filter.cutoff", 1800.0),
        ("filter.resonance", 0.1),
        ("filter.env.amount", 1.4),
        ("filter_env.attack", 0.005),
        ("filter_env.decay", 0.6),
        ("filter_env.sustain", 0.25),
        ("filter_env.release", 0.5),
        ("amp.attack", 0.003),
        ("amp.decay", 2.6),
        ("amp.sustain", 0.0),
        ("amp.curve", 0.5),
        ("amp.release", 0.5),
        ("amp.damping", 0.8),
        ("env3.attack", 0.001),
        ("env3.decay", 0.03),
        ("env3.sustain", 0.0),
        ("env3.release", 0.02),
        // Env3 -> oscillator C level (10) is the hammer layer; Env3 -> filter
        // cutoff (0) opens the filter for the same 30 ms so the layer is not
        // swallowed by the note's own low-pass.
        ("mod.1.source", 20.0),
        ("mod.1.destination", 10.0),
        ("mod.1.amount", 0.3),
        ("mod.2.source", 20.0),
        ("mod.2.destination", 0.0),
        ("mod.2.amount", 0.1),
        ("unison.voices", 2.0),
        ("unison.detune", 4.0),
        ("unison.width", 0.3),
        ("voice.polyphony", 8.0),
        ("output.gain", 0.92),
    ]),
};

/// The factory upright patch: mono, low and short enough to leave room.
static BASS: Patch = Patch {
    name: "oc_default_bass",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 3.0),
        ("osc.a.level", 0.8),
        ("osc.b.waveform", 0.0),
        ("osc.b.level", 0.3),
        ("osc.b.semitones", -12.0),
        ("sub.level", 0.15),
        ("filter.cutoff", 900.0),
        ("filter.resonance", 0.1),
        ("filter.env.amount", 1.0),
        ("filter_env.attack", 0.02),
        ("filter_env.decay", 0.4),
        ("filter_env.sustain", 0.3),
        ("filter_env.release", 0.3),
        ("amp.attack", 0.012),
        ("amp.decay", 0.5),
        ("amp.sustain", 0.55),
        ("amp.release", 0.3),
        ("unison.voices", 1.0),
        ("unison.width", 0.0),
        ("voice.mono", 1.0),
        ("voice.legato", 1.0),
        ("voice.glide", 0.04),
        ("output.gain", 0.99),
    ]),
};

/// The factory choir pad: slow, sustained and wide, for strings and winds.
static STRINGS: Patch = Patch {
    name: "oc_default_strings",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 3.0),
        ("osc.a.level", 0.6),
        ("osc.b.waveform", 1.0),
        ("osc.b.level", 0.28),
        ("osc.b.semitones", 12.0),
        ("noise.level", 0.03),
        ("unison.voices", 7.0),
        ("unison.detune", 12.0),
        ("unison.width", 0.8),
        ("filter.cutoff", 3200.0),
        ("filter.resonance", 0.12),
        ("filter.keytrack", 0.0),
        ("filter.env.amount", 0.8),
        ("filter_env.attack", 0.6),
        ("filter_env.decay", 0.6),
        ("filter_env.sustain", 0.7),
        ("filter_env.release", 1.5),
        ("amp.attack", 0.9),
        ("amp.decay", 0.5),
        ("amp.sustain", 0.9),
        ("amp.release", 2.6),
        ("voice.polyphony", 8.0),
        ("lfo.rate", 0.35),
        ("lfo.pitch", 2.0),
        ("output.gain", 0.81),
    ]),
};

/// The factory pulse lead: a sustained single line that cuts through.
static LEAD: Patch = Patch {
    name: "oc_default_lead",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 2.0),
        ("osc.a.level", 0.9),
        ("osc.a.warp_mode", 5.0),
        ("osc.a.warp_amount", 0.55),
        ("osc.a.warp_var", 0.5),
        ("osc.b.waveform", 1.0),
        ("osc.b.level", 0.2),
        ("osc.b.semitones", -12.0),
        ("filter.cutoff", 13000.0),
        ("filter.resonance", 0.18),
        ("unison.voices", 3.0),
        ("unison.detune", 9.0),
        ("unison.width", 0.45),
        ("lfo2.sync", 1.0),
        ("lfo2.sync_div", 16.0),
        ("mod.1.source", 11.0),
        ("mod.1.destination", 0.0),
        ("mod.1.amount", 0.04),
        ("amp.attack", 0.01),
        ("amp.decay", 0.3),
        ("amp.sustain", 0.85),
        ("amp.release", 0.35),
        ("voice.mono", 1.0),
        ("voice.legato", 1.0),
        ("voice.glide", 0.05),
        ("output.gain", 1.0),
    ]),
};

/// The factory pluck: a short wood-like attack for guitars.
static GUITAR: Patch = Patch {
    name: "oc_default_guitar",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 3.0),
        ("osc.a.level", 0.7),
        ("osc.a.warp_mode", 15.0),
        ("osc.a.warp_amount", 0.35),
        ("osc.b.waveform", 1.0),
        ("osc.b.level", 0.15),
        ("noise.level", 0.05),
        ("filter.cutoff", 2600.0),
        ("filter.resonance", 0.3),
        ("filter.env.amount", 3.0),
        ("filter_env.attack", 0.0),
        ("filter_env.decay", 0.12),
        ("filter_env.sustain", 0.0),
        ("filter_env.release", 0.1),
        ("amp.attack", 0.001),
        ("amp.decay", 0.18),
        ("amp.sustain", 0.05),
        ("amp.release", 0.2),
        ("mod.1.source", 2.0),
        ("mod.1.destination", 0.0),
        ("mod.1.amount", 0.3),
        ("unison.voices", 2.0),
        ("unison.detune", 4.0),
        ("unison.width", 0.3),
        ("voice.polyphony", 8.0),
        ("output.gain", 1.0),
    ]),
};

/// The factory brass swell: a mid-forward body with a soft bloom.
static BRASS: Patch = Patch {
    name: "oc_default_brass",
    plugin: SYNTH_PLUGIN,
    sound: Sound::Synth(&[
        ("osc.a.waveform", 1.0),
        ("osc.a.level", 0.6),
        ("osc.b.waveform", 1.0),
        ("osc.b.level", 0.35),
        ("osc.b.semitones", 7.0),
        ("unison.voices", 3.0),
        ("unison.detune", 10.0),
        ("unison.width", 0.4),
        ("filter.cutoff", 3200.0),
        ("filter.resonance", 0.15),
        ("filter.env.amount", 1.6),
        ("filter_env.attack", 0.18),
        ("filter_env.decay", 0.7),
        ("filter_env.sustain", 0.55),
        ("filter_env.release", 0.5),
        ("amp.attack", 0.12),
        ("amp.decay", 0.4),
        ("amp.sustain", 0.88),
        ("amp.release", 0.5),
        ("voice.polyphony", 4.0),
        ("output.gain", 0.5),
    ]),
};

/// The default kit uses the synthesiser fallback voices, so it needs no files.
static DRUMS: Patch = Patch {
    name: "oc_default_drums",
    plugin: crate::DRUMKIT_PLUGIN_ID,
    sound: Sound::DrumKit,
};

/// Plugin the default patch for a bare instrument family uses, without adding
/// it to a document. Validation needs it to know what the track will play.
pub fn default_patch_plugin(instrument: &InstrumentPath) -> Option<&'static str> {
    patch_for(instrument).map(|patch| patch.plugin)
}

/// Give every bare track and named pattern a playable built-in preset.
/// Explicit presets, audio/clip hosts and families with no honest default
/// (`sample`) stay as written.
pub fn apply_defaults(project: &mut Project) {
    let mut assigned: BTreeMap<&'static str, String> = BTreeMap::new();
    for index in 0..project.tracks.len() {
        if project.tracks[index].preset.is_some() {
            continue;
        }
        let patch = {
            let track = &project.tracks[index];
            match track_instrument(project, track).and_then(patch_for) {
                Some(patch) => patch,
                None => continue,
            }
        };
        let name = assign(project, patch, &mut assigned);
        project.tracks[index].preset = Some(name);
    }
    // A named pattern is also a leaf a preview auditions on its own; it carries
    // the family default so that path does not fall back to a SoundFont either.
    for index in 0..project.patterns.len() {
        if project.patterns[index].preset.is_some() {
            continue;
        }
        let Some(patch) = patch_for(&project.patterns[index].instrument) else {
            continue;
        };
        let name = assign(project, patch, &mut assigned);
        project.patterns[index].preset = Some(name);
    }
}

/// Remove the injected defaults, leaving every user preset and every track or
/// pattern that names one. This is the explicit `--soundfont` path: the legacy
/// fallback must win over a preset the document never asked for.
pub fn clear_injected(project: &mut Project) {
    for track in &mut project.tracks {
        if track.preset.as_deref().is_some_and(is_injected) {
            track.preset = None;
        }
    }
    for pattern in &mut project.patterns {
        if pattern.preset.as_deref().is_some_and(is_injected) {
            pattern.preset = None;
        }
    }
    project.presets.retain(|preset| !is_injected(&preset.name));
}

fn assign(
    project: &mut Project,
    patch: &'static Patch,
    assigned: &mut BTreeMap<&'static str, String>,
) -> String {
    match assigned.get(patch.name) {
        Some(name) => name.clone(),
        None => {
            let name = ensure_preset(project, patch);
            assigned.insert(patch.name, name.clone());
            name
        }
    }
}

/// The track's own family, or the family of the first clip/inline pattern when
/// the track itself only declares `instrument: unknown/unknown`.
pub(crate) fn track_instrument<'a>(
    project: &'a Project,
    track: &'a Track,
) -> Option<&'a InstrumentPath> {
    if known(&track.instrument) {
        return Some(&track.instrument);
    }
    for clip in track.pattern_clips() {
        if let Some(pattern) = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
            && known(&pattern.instrument)
        {
            return Some(&pattern.instrument);
        }
    }
    track
        .inline_patterns
        .iter()
        .map(|pattern| &pattern.instrument)
        .find(|instrument| known(instrument))
}

fn known(instrument: &InstrumentPath) -> bool {
    !matches!(
        instrument.category.trim().to_ascii_lowercase().as_str(),
        "" | "unknown" | "audio"
    )
}

fn patch_for(instrument: &InstrumentPath) -> Option<&'static Patch> {
    let category = instrument.category.trim().to_ascii_lowercase();
    let patch = match category.as_str() {
        "piano" | "keys" => &KEYS,
        "bass" => &BASS,
        "organ" | "strings" | "pad" | "vocal" | "woodwinds" => &STRINGS,
        "guitar" | "percussion" => &GUITAR,
        "brass" => &BRASS,
        "lead" | "synth" | "fx" => &LEAD,
        "drums" | "drumkit" | "kit" => &DRUMS,
        _ => return None,
    };
    Some(patch)
}

/// Reuse an identical preset already in the document; otherwise add one under a
/// name that no user preset occupies.
fn ensure_preset(project: &mut Project, patch: &Patch) -> String {
    let wanted = params(patch);
    if let Some(existing) = project.presets.iter().find(|preset| {
        same_sound(preset, patch, &wanted)
            && (preset.name == patch.name
                || preset
                    .name
                    .strip_prefix(patch.name)
                    .is_some_and(|suffix| suffix.starts_with('_')))
    }) {
        return existing.name.clone();
    }
    let mut name = patch.name.to_string();
    let mut serial = 2;
    while project.presets.iter().any(|preset| preset.name == name) {
        name = format!("{}_{serial}", patch.name);
        serial += 1;
    }
    project.presets.push(PresetDef {
        name: name.clone(),
        source: Some("builtin".into()),
        plugin: Some(patch.plugin.into()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: wanted,
        effects: Vec::new(),
        extends: None,
    });
    name
}

fn same_sound(preset: &PresetDef, patch: &Patch, wanted: &[(String, ParamValue)]) -> bool {
    preset.source.as_deref() == Some("builtin")
        && preset.plugin.as_deref() == Some(patch.plugin)
        && preset.params == wanted
}

fn params(patch: &Patch) -> Vec<(String, ParamValue)> {
    match &patch.sound {
        Sound::Synth(values) => values
            .iter()
            .map(|(name, value)| ((*name).to_string(), ParamValue::Number(*value)))
            .collect(),
        Sound::DrumKit => kit_params(),
    }
}

/// Seven pads with explicit notes; each falls back to its synthesised voice,
/// so a kit written from the pad table alone is audible.
fn kit_params() -> Vec<(String, ParamValue)> {
    const PADS: &[(&str, u8, &str)] = &[
        ("kick", 36, "kick"),
        ("snare", 38, "snare"),
        ("hat_closed", 42, "hat_closed"),
        ("hat_open", 46, "hat_open"),
        ("tom", 45, "tom"),
        ("clap", 39, "clap"),
        ("rim", 37, "rim"),
    ];
    let mut params = vec![(
        "kit.max_voices".to_string(),
        ParamValue::Number(f64::from(32u32)),
    )];
    for (name, note, voice) in PADS {
        let prefix = format!("pad.{name}.");
        params.push((
            format!("{prefix}note"),
            ParamValue::Number(f64::from(*note)),
        ));
        params.push((format!("{prefix}gain"), ParamValue::Db(0.0)));
        params.push((format!("{prefix}pan"), ParamValue::Number(0.0)));
        params.push((format!("{prefix}tune"), ParamValue::Number(0.0)));
        params.push((format!("{prefix}cents"), ParamValue::Number(0.0)));
        params.push((format!("{prefix}decay"), ParamValue::Time(0.0)));
        params.push((format!("{prefix}release"), ParamValue::Time(0.005)));
        params.push((format!("{prefix}reverse"), ParamValue::Bool(false)));
        params.push((format!("{prefix}one_shot"), ParamValue::Bool(true)));
        let choke = match *name {
            "hat_closed" | "hat_open" => 2.0,
            _ => 0.0,
        };
        params.push((format!("{prefix}choke"), ParamValue::Number(choke)));
        params.push((
            format!("{prefix}voice"),
            ParamValue::String((*voice).to_string()),
        ));
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(source: &str) -> Project {
        let mut project = crate::parse(source).unwrap();
        apply_defaults(&mut project);
        project
    }

    #[test]
    fn parsing_and_formatting_do_not_inject() {
        let source = "@project\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n";
        let project = crate::parse(source).unwrap();
        assert_eq!(project.tracks[0].preset, None);
        let text = crate::serialize(&project);
        assert!(!text.contains("oc_default_"), "{text}");
    }

    #[test]
    fn clear_injected_restores_the_bare_track_and_keeps_explicit_presets() {
        let mut project = project(
            "@project\n@preset mine\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track keys | instrument: piano/acoustic_grand\n  @phrase k [1:1 - 2:1]\n    C4 w mf\n@track bass | preset: mine, instrument: bass/electric_finger\n  @phrase b [1:1 - 2:1]\n    C2 w mf\n",
        );
        assert_eq!(project.tracks[0].preset.as_deref(), Some("oc_default_keys"));
        clear_injected(&mut project);
        assert_eq!(project.tracks[0].preset, None);
        assert_eq!(project.tracks[1].preset.as_deref(), Some("mine"));
        assert_eq!(project.presets.len(), 1);
        assert_eq!(project.presets[0].name, "mine");
    }

    #[test]
    fn a_bare_track_gets_a_visible_builtin_preset() {
        let project = project(
            "@project\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        );
        let track = &project.tracks[0];
        let name = track.preset.as_deref().expect("preset injected");
        assert_eq!(name, "oc_default_keys");
        let preset = project
            .presets
            .iter()
            .find(|preset| preset.name == name)
            .expect("the injected preset block is in the document");
        assert_eq!(preset.source.as_deref(), Some("builtin"));
        assert_eq!(preset.plugin.as_deref(), Some(SYNTH_PLUGIN));
        assert!(!preset.params.is_empty());
        let text = crate::serialize(&project);
        assert!(text.contains("@preset oc_default_keys"), "{text}");
        assert!(text.contains("preset: oc_default_keys"), "{text}");
    }

    #[test]
    fn an_explicit_preset_is_never_replaced() {
        let project = project(
            "@project\n@preset grand\n  file: \"media/grand.sf2\"\n@track keys | preset: grand, instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        );
        assert_eq!(project.tracks[0].preset.as_deref(), Some("grand"));
        assert_eq!(project.presets.len(), 1);
    }

    #[test]
    fn a_clip_pattern_supplies_the_family_when_the_track_has_none() {
        let project = project(
            "@project\n@pattern riff | instrument: bass/electric_finger\n  events:\n    C2 8th mf\n@track bass\n  clip: riff [1:1 - 2:1]\n",
        );
        assert_eq!(
            project.tracks[0].preset.as_deref(),
            Some("oc_default_bass"),
            "{:?}",
            project.tracks[0].preset
        );
        assert_eq!(
            project.patterns[0].preset.as_deref(),
            Some("oc_default_bass"),
            "a pattern audition must not fall back to a SoundFont"
        );
    }

    #[test]
    fn applying_defaults_twice_changes_nothing() {
        let mut project = project(
            "@project\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        );
        let names: Vec<Option<String>> = project
            .tracks
            .iter()
            .map(|track| track.preset.clone())
            .collect();
        apply_defaults(&mut project);
        assert_eq!(project.presets.len(), 1);
        assert_eq!(
            project
                .tracks
                .iter()
                .map(|track| track.preset.clone())
                .collect::<Vec<_>>(),
            names
        );
    }

    #[test]
    fn two_tracks_of_one_family_share_one_preset() {
        let project = project(
            "@project\n@track a | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n@track b | instrument: keys/electric_piano\n  @phrase p [1:1 - 2:1]\n    E4 w mf\n",
        );
        assert_eq!(project.presets.len(), 1);
        assert_eq!(project.tracks[0].preset, project.tracks[1].preset);
    }

    /// The bank entry the GUI shows and the injected default are one patch.
    #[test]
    fn the_factory_bank_matches_the_injected_keys_patch() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../oc-gui/frontend/src/data/factoryPresets.ts"
        ));
        let start = source
            .find("name: \"Muted Grand\"")
            .expect("Muted Grand is in the factory bank");
        let block = &source[start..];
        let block = &block[block.find("params: [").expect("params block")..];
        let block = &block[..block.find("\n    ],").expect("params block end")];
        let mut bank = std::collections::BTreeMap::new();
        for chunk in block.split("[\"").skip(1) {
            let (key, rest) = chunk.split_once("\", \"").expect("key and value");
            let (value, _) = rest.split_once('"').expect("value ends");
            bank.insert(
                key.to_string(),
                value.parse::<f64>().expect("numeric value"),
            );
        }

        let project = project(
            "@project\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
        );
        let preset = project
            .presets
            .iter()
            .find(|preset| preset.name == "oc_default_keys")
            .expect("the keys default is injected");
        let mut injected = std::collections::BTreeMap::new();
        for (name, value) in &preset.params {
            if let ParamValue::Number(value) = value {
                injected.insert(name.clone(), *value);
            }
        }
        assert_eq!(bank, injected, "Muted Grand and oc_default_keys drifted");
    }

    #[test]
    fn the_drum_default_is_the_kit_route() {
        let project = project(
            "@project\n@track drums | instrument: drums/acoustic_kit\n  @pattern beat [1:1 - 2:1]\n    kick 1:0 ff\n    snare 2:0 mf\n",
        );
        let name = project.tracks[0].preset.as_deref().unwrap();
        assert_eq!(name, "oc_default_drums");
        let preset = project
            .presets
            .iter()
            .find(|preset| preset.name == name)
            .unwrap();
        assert_eq!(preset.plugin.as_deref(), Some(crate::DRUMKIT_PLUGIN_ID));
        let text = crate::serialize(&project);
        assert!(text.contains("@kit oc_default_drums"), "{text}");
    }

    #[test]
    fn a_family_with_no_honest_default_is_left_alone() {
        let project = project("@project\n@track vocal | instrument: sample/take\n");
        assert!(project.presets.is_empty());
        assert_eq!(project.tracks[0].preset, None);
    }
}
