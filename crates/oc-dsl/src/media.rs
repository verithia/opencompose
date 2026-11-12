//! Project-relative media a document declares.
//!
//! Only paths that a render actually opens are checked: a preset's `file:`
//! (for a soundfont or sample source), the native-instrument state/wavetable/
//! sample params, an audio clip's `file:`, and the instrument params a track
//! overrides through `engine_params`. Drum-kit layer samples are declared as
//! optional because the kit tolerates a missing layer and reports it instead
//! of failing the render; Save As still collects the ones that are present.

use std::path::{Path, PathBuf};

use crate::ast::{ParamValue, PresetDef, Project};
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

/// How the engine reads one instrument parameter, or `None` when it never
/// opens the value.
fn param_kind(key: &str) -> Option<MediaKind> {
    match normalized(key).as_str() {
        "wavetablefile" | "samplersfzfile" | "samplersamplefile" | "clapstatefile"
        | "vst3statefile" | "austatefile" | "lv2statefile" | "pluginstatefile" | "statefile" => {
            Some(MediaKind::File)
        }
        "audioregionstatefile" => Some(MediaKind::Document),
        "opensynthstatefile" => Some(MediaKind::SynthDocument),
        "samplerroot" => Some(MediaKind::Directory),
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
