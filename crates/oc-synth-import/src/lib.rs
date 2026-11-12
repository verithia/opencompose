//! Preset and project-state import mapped onto OpenSynth patch types.

pub mod foreign_preset;

pub use foreign_preset::ForeignPreset;

use oc_dsl::ast::ParamValue;
use oc_synth_core::synth::{CustomWavetable, SynthPatch, document};

/// Patch parameter names that hold project-relative asset paths.
pub const PROJECT_ASSET_KEYS: [&str; 4] = [
    "open_synth_state_file",
    "serum_file",
    "serum_root",
    "wavetable.file",
];

/// Resolve one preset asset against the project directory.
///
/// A relative asset needs the directory that holds the project; an absolute
/// path is used as written. The refusal names the resolved path, so a project
/// separated from its assets reports where the load looked.
fn resolve_asset_path(
    value: &ParamValue,
    root: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let ParamValue::String(text) = value else {
        return Err("Invalid synth resource path".into());
    };
    let path = std::path::PathBuf::from(text);
    Ok(if path.is_absolute() {
        path
    } else {
        root.ok_or("Relative synth asset requires a saved project directory")?
            .join(path)
    })
}

fn asset_path(
    value: &ParamValue,
    root: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let resolved = resolve_asset_path(value, root)?;
    if !resolved.is_file() {
        return Err(format!("Synth asset not found: {}", resolved.display()));
    }
    Ok(resolved)
}

/// Anchor the effective OpenSynth asset references before a preset leaves its
/// project.
pub fn anchor_project_patch_assets(
    params: &mut [(String, ParamValue)],
    root: Option<&std::path::Path>,
) -> Result<(), String> {
    let mut anchored = Vec::new();
    for key in PROJECT_ASSET_KEYS {
        let Some(index) = params.iter().rposition(|(name, _)| name == key) else {
            continue;
        };
        let value = &params[index].1;
        let path = if key == "serum_root" {
            let path = resolve_asset_path(value, root)?;
            if !path.is_dir() {
                return Err(format!(
                    "Synth content directory not found: {}",
                    path.display()
                ));
            }
            path
        } else {
            asset_path(value, root)?
        };
        let absolute = std::path::absolute(&path)
            .map_err(|error| format!("Cannot resolve synth asset {}: {error}", path.display()))?;
        let text = absolute
            .to_str()
            .ok_or("Synth resource path is not UTF-8")?;
        anchored.push((index, ParamValue::String(text.to_owned())));
    }
    for (index, value) in anchored {
        params[index].1 = value;
    }
    Ok(())
}

/// A legacy library patch can lack the project directory for one of its paths.
pub fn has_relative_project_patch_assets(params: &[(String, ParamValue)]) -> bool {
    PROJECT_ASSET_KEYS.into_iter().any(|key| {
        params.iter().rev().find(|(name, _)| name == key).is_some_and(|(_, value)| {
            matches!(value, ParamValue::String(path) if std::path::Path::new(path).is_relative())
        })
    })
}

/// Build a patch from preset parameters, including an external state or preset file.
pub fn load_project_patch(
    params: &[(String, ParamValue)],
    root: Option<&std::path::Path>,
) -> Result<SynthPatch, String> {
    use std::io::Read;
    let path = |value: &ParamValue| asset_path(value, root);
    let mut patch = SynthPatch::default();
    if let Some((_, value)) = params
        .iter()
        .rev()
        .find(|(k, _)| k == "open_synth_state_file")
    {
        let document_path = path(value)?;
        let mut bytes = Vec::new();
        std::fs::File::open(&document_path)
            .map_err(|e| e.to_string())?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        patch = document::PatchDocument::from_json(&bytes)?.into_patch_at(&document_path)?;
    }
    if let Some((_, value)) = params.iter().rev().find(|(k, _)| k == "serum_file") {
        let file = path(value)?;
        let content_root = params
            .iter()
            .rev()
            .find(|(key, _)| key == "serum_root")
            .and_then(|(_, value)| {
                let ParamValue::String(text) = value else {
                    return None;
                };
                let path = std::path::PathBuf::from(text);
                Some(if path.is_absolute() {
                    path
                } else {
                    root.map(|root| root.join(&path)).unwrap_or(path)
                })
            });
        let content_root = content_root.or_else(|| foreign_preset::infer_content_root(&file));
        let preset = foreign_preset::ForeignPreset::load(&file)?;
        patch = preset.to_synth_patch(content_root.as_deref());
    }
    if let Some((_, value)) = params.iter().rev().find(|(k, _)| k == "wavetable.file") {
        let file = path(value)?;
        patch.custom_wavetable = Some(CustomWavetable::load(
            file.to_str().ok_or("Synth resource path is not UTF-8")?,
        )?);
    }
    let numeric = params
        .iter()
        .filter(|(key, _)| key != "wavetable.file")
        .cloned()
        .collect();
    patch.apply_params(Some(&numeric));
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::load_project_patch;
    use oc_dsl::ast::ParamValue;
    use oc_synth_core::synth::LfoShapeType;
    use std::path::{Path, PathBuf};

    /// The engine's per-patch trim is a runtime gain: it leaves the documented
    /// `output.gain` untouched and does not change the patch's own field.
    #[test]
    fn a_loudness_trim_is_applied_as_a_separate_gain() {
        let patch = load_project_patch(
            &[
                ("output.gain".into(), ParamValue::Number(0.5)),
                ("output.loudness_trim_db".into(), ParamValue::Number(-6.0)),
            ],
            None,
        )
        .expect("the patch loads");
        assert_eq!(patch.gain, 0.5, "the trim rewrote output.gain");
        assert!(
            (patch.loudness_gain - 10.0_f64.powf(-6.0 / 20.0)).abs() < 1e-12,
            "the trim did not reach the runtime gain: {}",
            patch.loudness_gain
        );
    }

    /// Fresh directory for one case; the caller removes it.
    fn temp_dir(tag: &str) -> PathBuf {
        oc_test_support::temp_dir("opencompose-synth-asset", tag)
    }

    /// One 2048-sample frame the wavetable loader accepts.
    fn write_wavetable(path: &Path) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for frame in 0..2048 {
            writer
                .write_sample((std::f32::consts::TAU * frame as f32 / 2048.0).sin())
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn wavetable_params(file: &str) -> Vec<(String, ParamValue)> {
        vec![(
            "wavetable.file".to_string(),
            ParamValue::String(file.to_string()),
        )]
    }

    #[test]
    fn anchoring_preserves_external_asset_locations_and_reports_missing_paths() {
        let root = temp_dir("anchored");
        std::fs::create_dir(root.join("content")).unwrap();
        std::fs::write(root.join("state.json"), b"test").unwrap();
        std::fs::write(root.join("source.preset"), b"test").unwrap();
        let mut params = vec![
            (
                "open_synth_state_file".into(),
                ParamValue::String("state.json".into()),
            ),
            (
                "serum_file".into(),
                ParamValue::String("source.preset".into()),
            ),
            ("serum_root".into(), ParamValue::String("content".into())),
        ];
        let error = super::anchor_project_patch_assets(&mut params, None).unwrap_err();
        assert!(error.contains("saved project directory"));
        super::anchor_project_patch_assets(&mut params, Some(&root)).unwrap();
        for ((_, value), file) in params
            .iter()
            .zip(["state.json", "source.preset", "content"])
        {
            assert_eq!(
                value,
                &ParamValue::String(root.join(file).to_str().unwrap().into())
            );
        }
        let mut missing = wavetable_params("missing.wav");
        let error = super::anchor_project_patch_assets(&mut missing, Some(&root)).unwrap_err();
        assert!(error.contains("missing.wav"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_relative_wavetable_resolves_against_the_project_directory() {
        let root = temp_dir("relative");
        std::fs::create_dir_all(root.join("media/tables")).unwrap();
        write_wavetable(&root.join("media/tables/square.wav"));
        let patch =
            load_project_patch(&wavetable_params("media/tables/square.wav"), Some(&root)).unwrap();
        assert!(patch.custom_wavetable.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_absolute_wavetable_loads_without_a_project_directory() {
        let root = temp_dir("absolute");
        let file = root.join("table.wav");
        write_wavetable(&file);
        let patch =
            load_project_patch(&wavetable_params(&file.display().to_string()), None).unwrap();
        assert!(patch.custom_wavetable.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_wavetable_names_the_resolved_path() {
        let root = temp_dir("missing");
        let error = load_project_patch(&wavetable_params("media/tables/gone.wav"), Some(&root))
            .unwrap_err();
        let tried = root.join("media/tables/gone.wav");
        assert!(error.contains(&tried.display().to_string()), "{error}");
        assert!(error.contains("not found"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_relative_wavetable_without_a_project_directory_is_refused() {
        let error =
            load_project_patch(&wavetable_params("media/tables/square.wav"), None).unwrap_err();
        assert!(
            error.contains("Relative synth asset requires a saved project directory"),
            "{error}"
        );
    }

    #[test]
    fn shape_arrays_import_from_dsl_preset_params() {
        let project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { lfo.shape: 1, lfo.shape.points: 4, lfo.shape.x: [0, 0.25, 0.5, 1], lfo.shape.y: [0.1, 1, 0.2, 0.9], lfo.shape.curve: [0.5, 0.3, 0.7, 0.5], lfo.phase: 45 }\n",
        )
        .unwrap();
        let patch = load_project_patch(&project.presets[0].params, None).unwrap();
        assert_eq!(patch.lfo_shape_type, LfoShapeType::Path);
        assert_eq!(patch.lfo_shape.points, 4);
        assert_eq!(patch.lfo_shape.x[..4], [0.0, 0.25, 0.5, 1.0]);
        assert_eq!(patch.lfo_shape.y[..4], [0.1, 1.0, 0.2, 0.9]);
        assert_eq!(patch.lfo_shape.curve[..4], [0.5, 0.3, 0.7, 0.5]);
        assert_eq!(patch.lfo_phase, 45.0);
        // The same params survive a text round-trip with arrays intact.
        let text = oc_dsl::serialize(&project);
        let reopened = oc_dsl::parse(&text).unwrap();
        assert_eq!(project.presets[0].params, reopened.presets[0].params);
    }
}
