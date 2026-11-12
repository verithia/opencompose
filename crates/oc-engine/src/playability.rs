//! Early answer to "will this track make sound", using the preset
//! classification the renderer applies.
//!
//! Only the SoundFont branch is decidable here: a track whose preset is a
//! builtin or plugin instrument renders without a font, and one that declares
//! `file:` renders from that filesystem path (the edit layer checks it exists).
//! A plugin preset is taken at its declaration; a missing live CLAP instance is
//! a render-time failure this check does not model.

use std::path::Path;

use oc_dsl::ast::{PresetDef, Project};

/// `None` when the track's instrument can render, otherwise a readable reason
/// that names the missing declaration and both ways out.
pub fn unplayable_reason(
    project: &Project,
    track_index: usize,
    fallback_soundfont: Option<&Path>,
) -> Option<String> {
    let track = project.tracks.get(track_index)?;
    // An audio/clip host has no instrument of its own.
    if track.instrument.category == "audio" {
        return None;
    }

    let resolved = match oc_dsl::preset::resolve(project) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Some(format!("Track '{}' cannot render: {error}", track.name));
        }
    };
    // A clip may play its pattern's preset instead of the track's. That preset
    // must exist: the renderer refuses rather than falling back to the track.
    for clip in track.pattern_clips() {
        let Some(pattern) = resolved.patterns.iter().find(|p| p.name == clip.pattern) else {
            continue;
        };
        let Some(name) = pattern.preset.as_deref() else {
            continue;
        };
        if oc_dsl::instruments::is_injected(name) {
            continue;
        }
        if !resolved.presets.iter().any(|preset| preset.name == name) {
            return Some(format!(
                "Track '{}' cannot render: pattern '{}' references unknown preset '{}'",
                track.name, pattern.name, name
            ));
        }
    }
    let preset = match track.preset.as_deref() {
        Some(name) => match resolved.presets.iter().find(|preset| preset.name == name) {
            Some(preset) => Some(preset),
            None => {
                return Some(format!(
                    "Track '{}' references unknown preset '{}'; it cannot render",
                    track.name, name
                ));
            }
        },
        None => None,
    };

    let source = preset.and_then(|preset| preset.source.as_deref());
    let plugin = preset.and_then(|preset| preset.plugin.as_deref());
    if source == Some("builtin") || plugin.is_some_and(|id| id.starts_with("builtin.")) {
        return None;
    }

    let plugin_backed = matches!(source, Some("clap" | "vst3" | "vst2" | "au" | "lv2"))
        || (source == Some("sample") && plugin.is_some_and(|id| id.eq_ignore_ascii_case("sfizz")));
    if plugin_backed {
        if source == Some("sample") && preset.is_some_and(|preset| !declares_file(preset)) {
            return Some(format!(
                "Track '{}' uses sample preset '{}', which declares no file:; add the sample path or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
                track.name,
                track.preset.as_deref().unwrap_or("")
            ));
        }
        return None;
    }

    if preset.is_some_and(declares_file) {
        return None;
    }
    if fallback_soundfont.is_some_and(|path| !path.as_os_str().is_empty()) {
        return None;
    }
    Some(match preset {
        Some(_) => format!(
            "Track '{}' cannot render: preset '{}' (instrument '{}/{}') declares no file: and no SoundFont fallback is configured. Add file: \"<path>.sf2\" to the preset, or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
            track.name,
            track.preset.as_deref().unwrap_or(""),
            track.instrument.category,
            track.instrument.name
        ),
        None => format!(
            "Track '{}' cannot render: instrument '{}/{}' needs a SoundFont. Declare one with @preset <name> + file: \"<path>.sf2\" and set preset: <name> on the track, or use a built-in synth preset (source: builtin, plugin: \"builtin.open_synth\").",
            track.name, track.instrument.category, track.instrument.name
        ),
    })
}

fn declares_file(preset: &PresetDef) -> bool {
    preset
        .file
        .as_deref()
        .is_some_and(|file| !file.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(source: &str) -> Project {
        oc_dsl::parse(source).unwrap()
    }

    #[test]
    fn builtin_and_declared_files_are_playable_and_bare_soundfonts_are_not() {
        let builtin = project(
            "@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone\n",
        );
        assert_eq!(unplayable_reason(&builtin, 0, None), None);

        let declared = project(
            "@project\n@preset grand\n  file: \"media/grand.sf2\"\n@track keys | preset: grand\n",
        );
        assert_eq!(unplayable_reason(&declared, 0, None), None);

        let mut bare = project(
            "@project\n@track bass | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C2 q mf\n",
        );
        let reason = unplayable_reason(&bare, 0, None).unwrap();
        assert!(reason.contains("bass"), "{reason}");
        assert!(reason.contains("needs a SoundFont"), "{reason}");
        assert!(reason.contains("@preset"), "{reason}");
        assert!(reason.contains("builtin.open_synth"), "{reason}");

        // The render boundary assigns the family default, which makes it playable.
        oc_dsl::instruments::apply_defaults(&mut bare);
        assert_eq!(unplayable_reason(&bare, 0, None), None);

        let fallback = Path::new("/opt/soundfonts/default.sf2");
        let mut legacy = bare.clone();
        legacy.presets.clear();
        legacy.tracks[0].preset = None;
        assert_eq!(unplayable_reason(&legacy, 0, Some(fallback)), None);
    }

    #[test]
    fn a_preset_without_a_file_names_the_preset_and_the_way_out() {
        let project = project(
            "@project\n@preset grand\n  source: soundfont\n  program: 0\n@track keys | preset: grand\n",
        );
        let reason = unplayable_reason(&project, 0, None).unwrap();
        assert!(reason.contains("preset 'grand'"), "{reason}");
        assert!(reason.contains("file:"), "{reason}");
        assert!(reason.contains("builtin.open_synth"), "{reason}");
    }

    #[test]
    fn a_pattern_preset_that_does_not_exist_names_the_pattern() {
        let project = project(
            "@project\n@preset keys\n  source: builtin\n@pattern riff | preset: missing_patch, instrument: piano/acoustic_grand\n  events:\n    @0 C4 w mf\n@track lead | preset: keys, instrument: piano/acoustic_grand\n  clip: riff [1:1 - 2:1]\n",
        );
        let reason = unplayable_reason(&project, 0, None).unwrap();
        assert!(reason.contains("riff"), "{reason}");
        assert!(reason.contains("missing_patch"), "{reason}");
    }
}
