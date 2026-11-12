//! Project-scoped preset exchanges for the panel and MCP.

use std::path::Path;

use serde::Serialize;

use super::super::handoff::{HandoffVersion, ensure_handoff_folder, staged_path};
use super::super::shared::resync_with_undo;
use super::super::synth::{load_synth_patch_into_project, param_text};
use super::{import, preset_library, sound_preset_from_project, stage_export};
use crate::state::{AppState, UndoEdit, UndoScope};

#[derive(Serialize)]
pub(crate) struct BundleExport {
    path: String,
    name: String,
    bytes: u64,
    revision: u64,
}

#[derive(Serialize)]
pub(crate) struct BundleImport {
    name: String,
    library_name: String,
    revision: u64,
}

pub(crate) fn export_bundle_for_agent(
    state: &AppState,
    name: &str,
    expected_revision: u64,
) -> Result<BundleExport, String> {
    let (version, preset) = {
        let inner = state.lock_inner();
        inner.refuse_in_transaction().map_err(|_| "Finish the active edit transaction before sharing a preset. In the AI panel, export in a new turn.")?;
        let version = HandoffVersion::capture(&inner, expected_revision, None)?;
        let preset = sound_preset_from_project(&inner.project, name, name, None, vec![])?;
        (version, preset)
    };
    let root = version.project_root()?;
    let folder = ensure_handoff_folder(&root)?;
    let mut staged = stage_export(&preset, Some(&root), &folder)?;
    let bytes = staged
        .as_file()
        .metadata()
        .map_err(|error| error.to_string())?
        .len();
    let inner = state.lock_inner();
    inner.refuse_in_transaction()?;
    version.verify(&inner, None)?;
    if version.project_root()? != root
        || folder.canonicalize().ok().as_deref() != Some(folder.as_path())
    {
        return Err("The project handoff folder moved during preset export.".into());
    }
    for suffix in 1..=1000 {
        let filename = if suffix == 1 {
            "preset.ocppatch".into()
        } else {
            format!("preset-{suffix}.ocppatch")
        };
        match staged.persist_noclobber(folder.join(&filename)) {
            Ok(_) => {
                return Ok(BundleExport {
                    path: format!("handoff/{filename}"),
                    name: name.into(),
                    bytes,
                    revision: inner.revision,
                });
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file
            }
            Err(error) => return Err(error.error.to_string()),
        }
    }
    Err("The handoff folder already has 1000 preset exports. Move older exports before sharing another.".into())
}

pub(crate) fn import_bundle_for_agent(
    state: &AppState,
    path: &Path,
    name: &str,
    expected_revision: u64,
    caller: &str,
) -> Result<BundleImport, String> {
    let version = {
        let inner = state.lock_inner();
        let version = HandoffVersion::capture(&inner, expected_revision, Some(caller))?;
        if !crate::device_catalog::is_open_synth_preset(&inner.project, name) {
            return Err(format!(
                "Choose an existing OpenSynth project preset, then import into it: {name}"
            ));
        }
        version
    };
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ocppatch"))
    {
        return Err("Select a staged .ocppatch bundle in handoff/.".into());
    }
    let root = version.project_root()?;
    let path = staged_path(&root, path)?;
    let imported = import(&path, &preset_library())?;
    let mut inner = state.lock_inner();
    version.verify(&inner, Some(caller))?;
    if version.project_root()? != root {
        return Err("The project directory moved during preset import.".into());
    }
    let mut candidate = inner.project.clone();
    let params = imported
        .params
        .iter()
        .map(|(key, value)| (key.clone(), Some(param_text(value))))
        .collect();
    load_synth_patch_into_project(
        &mut candidate,
        name,
        Some(&imported.name),
        params,
        imported.effects,
    )?;
    oc_dsl::validate(&candidate)
        .map_err(|errors| super::super::shared::validation_text(&errors))?;
    let old = std::mem::replace(&mut inner.project, candidate);
    inner.presets.reload_library();
    inner.audio.invalidate_live_synth(name);
    resync_with_undo(
        &mut inner,
        old,
        UndoEdit::new("load preset bundle", UndoScope::Preset),
    )?;
    Ok(BundleImport {
        name: name.into(),
        library_name: imported.name,
        revision: inner.revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "@project\n@preset Parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.3}\n@preset Child\n  extends: Parent\n  params: {amp.sustain: 0.6}\n";

    fn saved(root: &Path) -> AppState {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(SOURCE).unwrap();
        inner.dsl_source = SOURCE.into();
        inner.file_path = Some(root.join("song.ocp"));
        drop(inner);
        state
    }

    #[test]
    fn bundle_exports_are_unique_and_revision_guarded() {
        let temp = tempfile::tempdir().unwrap();
        let state = saved(temp.path());
        assert!(export_bundle_for_agent(&state, "Child", 1).is_err());
        assert!(!temp.path().join("handoff").exists());
        let first = export_bundle_for_agent(&state, "Child", 0).unwrap();
        let bytes = std::fs::read(temp.path().join(&first.path)).unwrap();
        let second = export_bundle_for_agent(&state, "Child", 0).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(bytes, std::fs::read(temp.path().join(&first.path)).unwrap());
        assert_eq!(first.bytes as usize, bytes.len());
        assert_eq!(state.lock_inner().revision, 0);
        let base = state
            .begin_transaction("Bundle test".into(), "panel".into())
            .unwrap();
        assert!(export_bundle_for_agent(&state, "Child", base).is_err());
        assert!(
            import_bundle_for_agent(&state, Path::new(&first.path), "Child", base, "foreign")
                .is_err()
        );
        import_bundle_for_agent(&state, Path::new(&first.path), "Child", base, "panel").unwrap();
        assert!(state.lock_inner().project.presets[1].extends.is_none());
        state.rollback_transaction().unwrap();
        assert_eq!(
            state.lock_inner().project.presets[1].extends.as_deref(),
            Some("Parent")
        );
    }

    #[test]
    fn bundle_import_rejects_unstaged_or_missing_targets_without_project_edits() {
        let temp = tempfile::tempdir().unwrap();
        let state = saved(temp.path());
        let export = export_bundle_for_agent(&state, "Child", 0).unwrap();
        for path in [
            temp.path().join(&export.path),
            Path::new("../outside.ocppatch").into(),
            Path::new("handoff/../preset.ocppatch").into(),
            Path::new("handoff/unknown.ocppatch").into(),
        ] {
            assert!(import_bundle_for_agent(&state, &path, "Child", 0, "panel").is_err());
        }
        assert!(
            import_bundle_for_agent(&state, Path::new(&export.path), "Missing", 0, "panel")
                .is_err()
        );
        assert!(
            import_bundle_for_agent(&state, Path::new(&export.path), "Child", 1, "panel").is_err()
        );
        assert_eq!(state.lock_inner().dsl_source, SOURCE);
        assert_eq!(state.lock_inner().revision, 0);
    }
}
