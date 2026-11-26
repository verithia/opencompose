//! Project-relative media a document declares.
//!
//! Only paths that a render actually opens are checked: a preset's `file:`
//! (for a soundfont or sample source), the native-instrument state/wavetable/
//! sample params, an audio clip's `file:`, instrument parameter overrides, and
//! external insert state files. Drum-kit layer samples are declared as
//! optional because the kit tolerates a missing layer and reports it instead
//! of failing the render; Save As still collects the ones that are present.

use std::path::{Path, PathBuf};

use crate::ast::{Effect, EffectType, ParamValue, PresetDef, Project};
use crate::source_map::{SourceOwner, SourcePart, SourceSubject};

/// How the engine reads a declared path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// One file the engine opens.
    File,
    /// A directory the engine searches for samples (`sampler.root`).
    Directory,
    /// A document whose own media sits beside it (`audio_region_state_file`).
    Document,
    /// An OpenSynth state with optional portable sibling samples.
    SynthDocument,
}

/// One local path a document names, with the declaration that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDeclaration {
    pub owner: String,
    /// The document location that declared this path.
    pub subject: SourceSubject,
    pub path: String,
    pub kind: MediaKind,
    /// The render tolerates this path being absent, so validation does not
    /// refuse the document and Save As only warns when it cannot collect it.
    pub optional: bool,
}

/// True when the preset's `file:` is the media the renderer opens. A `file:`
/// left on a plugin preset is not read, so a stale one is not an error.
fn file_is_media(preset: &PresetDef) -> bool {
    matches!(
        preset.source.as_deref(),
        None | Some("soundfont" | "sample")
    )
}

/// Compare parameter names the way the engine does, so `wavetable.file` and
/// `wavetable_file` name the same parameter.
fn normalized(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// A backend qualifier on an authored external plugin state parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStateBackend {
    Au,
    Clap,
    Lv2,
    Vst2,
    Vst3,
}

/// How a project specifies opaque plugin state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStateEncoding {
    File,
    Hex,
    ImportedVst3Resource,
}

/// Look up a parameter key after removing separators and applying ASCII lowercase.
pub fn normalized_plugin_state_parameter(
    key: &str,
) -> Option<(Option<PluginStateBackend>, PluginStateEncoding)> {
    use PluginStateBackend::{Au, Clap, Lv2, Vst2, Vst3};
    use PluginStateEncoding::{File, Hex, ImportedVst3Resource};
    Some(match key {
        "statefile" | "pluginstatefile" => (None, File),
        "statehex" | "pluginstatehex" => (None, Hex),
        "clapstatefile" => (Some(Clap), File),
        "clapstatehex" => (Some(Clap), Hex),
        "vst3statefile" => (Some(Vst3), File),
        "vst3statehex" => (Some(Vst3), Hex),
        "vst2statefile" => (Some(Vst2), File),
        "vst2statehex" => (Some(Vst2), Hex),
        "austatefile" => (Some(Au), File),
        "austatehex" => (Some(Au), Hex),
        "lv2statefile" => (Some(Lv2), File),
        "lv2statehex" => (Some(Lv2), Hex),
        "serumfile" => (Some(Vst3), ImportedVst3Resource),
        _ => return None,
    })
}

/// The file used by this insert, after precedence across state aliases.
fn effect_state_file(effect: &Effect) -> Option<&str> {
    if !matches!(effect.effect_type, EffectType::Plugin(_)) {
        return None;
    }
    let (encoding, value) = effect.params.iter().rev().find_map(|(key, value)| {
        normalized_plugin_state_parameter(&normalized(key)).map(|(_, encoding)| (encoding, value))
    })?;
    match (encoding, value) {
        (PluginStateEncoding::File, ParamValue::String(path)) if !path.trim().is_empty() => {
            Some(path)
        }
        _ => None,
    }
}

/// Sidecars that may be read by the project's effect chains.
pub fn insert_state_files(project: &Project) -> impl Iterator<Item = &str> {
    project
        .presets
        .iter()
        .flat_map(|preset| &preset.effects)
        .chain(project.tracks.iter().flat_map(|track| &track.effects))
        .chain(project.patterns.iter().flat_map(|pattern| &pattern.effects))
        .chain(project.buses.iter().flat_map(|bus| &bus.effects))
        .chain(project.master.iter().flat_map(|master| &master.effects))
        .filter(|effect| !effect.bypass)
        .filter_map(effect_state_file)
}

/// How the engine reads one instrument parameter, or `None` when it never
/// opens the value.
fn param_kind(key: &str) -> Option<MediaKind> {
    let key = normalized(key);
    match key.as_str() {
        "wavetablefile" | "samplersfzfile" | "samplersamplefile" => Some(MediaKind::File),
        "audioregionstatefile" => Some(MediaKind::Document),
        "opensynthstatefile" => Some(MediaKind::SynthDocument),
        "samplerroot" => Some(MediaKind::Directory),
        key if matches!(
            normalized_plugin_state_parameter(key),
            Some((_, PluginStateEncoding::File))
        ) =>
        {
            Some(MediaKind::File)
        }
        _ => None,
    }
}

/// True for the `layer.<pad>.<n>.sample` parameters a drum kit lowers to.
/// The pad name is one identifier segment, so a dotted key names one layer.
fn is_kit_layer_sample(key: &str) -> bool {
    let mut parts = key.split('.');
    parts.next() == Some("layer")
        && parts.next().is_some_and(|pad| !pad.is_empty())
        && parts.next().is_some_and(|index| !index.is_empty())
        && parts.next() == Some("sample")
        && parts.next().is_none()
}

fn push_param(
    out: &mut Vec<MediaDeclaration>,
    owner: &str,
    subject: SourceSubject,
    key: &str,
    value: &ParamValue,
) {
    let optional = is_kit_layer_sample(key);
    let kind = match param_kind(key) {
        Some(kind) => kind,
        None if optional => MediaKind::File,
        None => return,
    };
    let ParamValue::String(path) = value else {
        return;
    };
    if path.trim().is_empty() {
        return;
    }
    out.push(MediaDeclaration {
        owner: format!("{owner} param '{key}'"),
        subject,
        path: path.clone(),
        kind,
        optional,
    });
}

fn push_effect_assets(
    out: &mut Vec<MediaDeclaration>,
    owner: &str,
    subject: SourceOwner,
    effects: &[Effect],
) {
    for (index, effect) in effects.iter().enumerate() {
        let path = if effect.effect_type == EffectType::TransferShaper {
            effect
                .params
                .iter()
                .find_map(|(key, value)| match (key.as_str(), value) {
                    ("curve_file", ParamValue::String(path)) if !path.trim().is_empty() => {
                        Some(path.as_str())
                    }
                    _ => None,
                })
        } else {
            effect_state_file(effect)
        };
        if let Some(path) = path {
            out.push(MediaDeclaration {
                owner: format!("{owner} effect {}", index + 1),
                subject: SourceSubject {
                    owner: subject,
                    part: SourcePart::Whole,
                },
                path: path.to_owned(),
                kind: MediaKind::File,
                optional: effect.bypass,
            });
        }
    }
}

/// Every local media path the document declares, in document order.
pub fn declarations(project: &Project) -> Vec<MediaDeclaration> {
    let mut out = Vec::new();
    for (index, preset) in project.presets.iter().enumerate() {
        let subject = SourceSubject {
            owner: SourceOwner::Preset(index),
            part: SourcePart::Whole,
        };
        if file_is_media(preset)
            && let Some(file) = preset
                .file
                .as_deref()
                .filter(|file| !file.trim().is_empty())
        {
            out.push(MediaDeclaration {
                owner: format!("preset '{}'", preset.name),
                subject,
                path: file.to_string(),
                kind: MediaKind::File,
                optional: false,
            });
        }
        for (key, value) in &preset.params {
            push_param(
                &mut out,
                &format!("preset '{}'", preset.name),
                subject,
                key,
                value,
            );
        }
        push_effect_assets(
            &mut out,
            &format!("preset '{}'", preset.name),
            subject.owner,
            &preset.effects,
        );
    }
    for (index, track) in project.tracks.iter().enumerate() {
        let subject = SourceSubject {
            owner: SourceOwner::Track(index),
            part: SourcePart::Whole,
        };
        for (key, value) in track.engine_params.iter().flatten() {
            push_param(
                &mut out,
                &format!("track '{}'", track.name),
                subject,
                key,
                value,
            );
        }
        push_effect_assets(
            &mut out,
            &format!("track '{}'", track.name),
            subject.owner,
            &track.effects,
        );
    }
    for (index, pattern) in project.patterns.iter().enumerate() {
        push_effect_assets(
            &mut out,
            &format!("pattern '{}'", pattern.name),
            SourceOwner::Pattern(index),
            &pattern.effects,
        );
    }
    for (index, bus) in project.buses.iter().enumerate() {
        push_effect_assets(
            &mut out,
            &format!("bus '{}'", bus.name),
            SourceOwner::Bus(index),
            &bus.effects,
        );
    }
    if let Some(master) = &project.master {
        push_effect_assets(&mut out, "master", SourceOwner::Master, &master.effects);
    }
    for (index, clip) in project.audio_clips().into_iter().enumerate() {
        out.push(MediaDeclaration {
            owner: format!("audio clip '{}'", clip.name),
            subject: SourceSubject {
                owner: SourceOwner::AudioClip(index),
                part: SourcePart::Whole,
            },
            path: clip.file.clone(),
            kind: MediaKind::File,
            optional: false,
        });
    }
    out
}

/// Resolve a declared media path against the project directory.
pub fn resolve(root: &Path, declared: &Path) -> PathBuf {
    if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        root.join(declared)
    }
}

/// One readable error per declared path that is not on disk.
pub fn missing(project: &Project, root: &Path) -> Vec<String> {
    missing_diagnostics(project, root)
        .into_iter()
        .map(|(error, _)| error)
        .collect()
}

/// Missing media with the document location that declared it.
///
/// The first tuple element is the same human-readable message `missing`
/// returns; the second is the declaring preset, track or audio clip.
pub fn missing_diagnostics(project: &Project, root: &Path) -> Vec<(String, SourceSubject)> {
    let mut errors = Vec::new();
    for declaration in declarations(project) {
        let declared = Path::new(&declaration.path);
        let resolved = resolve(root, declared);
        let (wording, present) = match declaration.kind {
            MediaKind::Directory => ("directory", resolved.is_dir()),
            MediaKind::File | MediaKind::Document | MediaKind::SynthDocument => {
                ("file", resolved.is_file())
            }
        };
        if !present {
            if declaration.optional {
                continue;
            }
            errors.push((
                format!(
                    "{}: {wording} '{}' not found (project directory '{}')",
                    declaration.owner,
                    declaration.path,
                    root.display()
                ),
                declaration.subject,
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-media", tag)
    }

    #[test]
    fn declared_files_resolve_against_the_project_directory() {
        let dir = temp_dir("declared");
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::write(dir.join("media/grand.sf2"), b"sf2").unwrap();
        let project = crate::parse(
            "@project\n  tempo: 120\n@preset grand\n  file: \"media/grand.sf2\"\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"media/lead.wav\"\n@track vocal | instrument: audio/clip\n",
        )
        .unwrap();
        let missing = missing(&project, &dir);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].contains("audio clip 'lead'"));
        let diagnostics = missing_diagnostics(&project, &dir);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].1,
            SourceSubject {
                owner: SourceOwner::AudioClip(0),
                part: SourcePart::Whole,
            }
        );
        assert!(missing[0].contains("media/lead.wav"));
        assert!(missing[0].contains(&dir.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_presets_stale_file_is_not_checked() {
        let project = crate::parse(
            "@project\n@preset lead\n  source: clap\n  plugin: \"com.acme.lead\"\n  file: \"missing.wav\"\n",
        )
        .unwrap();
        assert!(declarations(&project).is_empty());
    }

    #[test]
    fn insert_sidecars_follow_the_last_alias_and_report_their_owner() {
        let mut project = crate::parse(
            r#"@project
@track vocal | instrument: audio/clip
  effects:
    - plugin:"vst2.fixture": vst2_state_file="track.bank"
@bus mix
  effects:
    - plugin:"vst3.fixture": plugin_state_file="bus.bank"
@master
  effects:
    - plugin:"clap.fixture": state_file="master.bank"
"#,
        )
        .unwrap();
        project.master.as_mut().unwrap().effects[0]
            .params
            .push(("state_hex".into(), ParamValue::String("00".into())));
        let paths: Vec<_> = insert_state_files(&project).collect();
        assert_eq!(paths, ["track.bank", "bus.bank"]);
        let declarations = declarations(&project);
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].subject.owner, SourceOwner::Track(0));
        assert_eq!(declarations[1].subject.owner, SourceOwner::Bus(0));
        project.buses[0].effects[0].bypass = true;
        assert_eq!(
            insert_state_files(&project).collect::<Vec<_>>(),
            ["track.bank"]
        );
        assert!(super::declarations(&project)[1].optional);
    }

    #[test]
    fn builtin_presets_declare_no_media() {
        let dir = temp_dir("builtin");
        let project = crate::parse(
            "@project\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone\n",
        )
        .unwrap();
        assert!(missing(&project, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_params_and_parameter_spellings_are_declared() {
        let dir = temp_dir("params");
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        std::fs::write(dir.join("table.wav"), b"table").unwrap();
        let project = crate::parse(
            "@project\n  tempo: 120\n@preset table\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {wavetable_file: \"table.wav\"}\n@track lead | preset: table\n  engine_params: {wavetable.file: \"gone.wav\"}\n",
        )
        .unwrap();
        let declared = declarations(&project);
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].path, "table.wav");
        assert!(declared[0].owner.starts_with("preset 'table'"));
        assert!(declared[1].owner.starts_with("track 'lead'"));
        assert!(declared.iter().all(|declaration| !declaration.optional));
        let missing = missing(&project, &dir);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].contains("gone.wav"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sample_root_is_declared_as_a_directory() {
        let dir = temp_dir("root");
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        let project = crate::parse(
            "@project\n  tempo: 120\n@preset sampler\n  source: builtin\n  plugin: \"builtin.sampler\"\n  params: {sampler.root: \"samples\", sampler.sample_file: \"samples/tone.wav\"}\n@track lead | preset: sampler\n",
        )
        .unwrap();
        let declared = declarations(&project);
        assert_eq!(declared[0].kind, MediaKind::Directory);
        let missing = missing(&project, &dir);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].contains("file 'samples/tone.wav'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_audio_region_document_is_declared_as_a_document() {
        let project = crate::parse(
            "@project\n  tempo: 120\n@preset region\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {audio_region.statefile: \"regions.json\"}\n@track lead | preset: region\n",
        )
        .unwrap();
        assert_eq!(declarations(&project)[0].kind, MediaKind::Document);
    }

    #[test]
    fn kit_layer_samples_are_optional_so_validate_accepts_a_missing_layer() {
        let dir = temp_dir("kit-layers");
        let project = crate::parse(
            "@project \"Kit\"\n  tempo: 120\n@kit basic\n  pad kick | note: C2\n  pad snare | note: D2\n  layer kick | low: 1, high: 127, sample: \"media/kick.wav\"\n  layer snare | low: 1, high: 127, sample: \"media/snare.wav\"\n@track drums | instrument: drums/drumkit, preset: basic\n",
        )
        .unwrap();
        crate::validate(&project).expect("the kit validates");
        let declared = declarations(&project);
        assert_eq!(declared.len(), 2);
        assert!(
            declared
                .iter()
                .all(|declaration| declaration.optional && declaration.kind == MediaKind::File)
        );
        let paths: Vec<_> = declared
            .iter()
            .map(|declaration| declaration.path.as_str())
            .collect();
        assert_eq!(paths, ["media/kick.wav", "media/snare.wav"]);
        // `ocp validate` uses exactly this list, so a render-tolerated layer
        // is not a validation error while its sample is absent.
        assert!(missing(&project, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
