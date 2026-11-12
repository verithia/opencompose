//! Patch lifecycle: rename, duplicate, save-as, whole-patch load, and the
//! user library bridge.
//!
//! A patch on a track is a project `@preset` block. Its name is the identity
//! inside the project; renaming rewrites every reference in the same undoable
//! step. `patch:` records where the values came from (a factory or library
//! name) and is never used for resolution.

use oc_dsl::ast;
use oc_engine::preset::library::{PresetPreferences, sort_for_browser, unix_now};
use oc_engine::preset::{LibraryEntry, PresetCategory, PresetLibrary, SaveOutcome, SoundPreset};
use serde::Serialize;
use tauri::{Manager, State};

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, resync_with_undo, run_blocking, snapshot};

/// The library after a mutating command, plus what the scan had to skip.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryListing {
    pub patches: Vec<LibraryEntry>,
    pub warnings: Vec<String>,
    /// Set by a save: whether an entry with that name was replaced.
    pub replaced: bool,
}

/// Duplicate a project preset.
#[tauri::command]
pub fn duplicate_preset(
    name: String,
    new_name: String,
    reassign: Option<bool>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    duplicate_preset_inner(
        &mut state.lock_inner(),
        &name,
        &new_name,
        reassign.unwrap_or(true),
    )
}

pub(crate) fn duplicate_preset_inner(
    inner: &mut crate::state::AppStateInner,
    name: &str,
    new_name: &str,
    reassign: bool,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    duplicate_preset_in_project(&mut inner.project, name, new_name, reassign)?;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("preset duplicate", UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

/// Save one track's patch as a new preset and point that track at it.
#[tauri::command]
pub fn save_track_preset_as_new(
    track_index: usize,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    save_track_preset_as_new_in_project(&mut inner.project, track_index)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("save track preset as new", UndoScope::Preset),
    )?;
    Ok(snapshot(&inner))
}

/// Rename a project preset.
#[tauri::command]
pub fn rename_preset(
    name: String,
    new_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    rename_preset_inner(&mut state.lock_inner(), &name, &new_name)
}

pub(crate) fn rename_preset_inner(
    inner: &mut crate::state::AppStateInner,
    name: &str,
    new_name: &str,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    rename_preset_in_project(&mut inner.project, name, new_name)?;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("preset rename", UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

/// List user-library presets.
#[tauri::command]
pub fn list_library_patches(state: State<AppState>) -> Result<LibraryListing, String> {
    let mut inner = state.lock_inner();
    inner.presets.reload_library();
    Ok(listing(false))
}

/// Save a project preset into the user library.
#[tauri::command]
pub async fn save_patch_to_library(
    name: String,
    preset_name: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    overwrite: Option<bool>,
    app: tauri::AppHandle,
) -> Result<LibraryListing, String> {
    let (preset, root) = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        let preset = sound_preset_from_project(
            &inner.project,
            preset_name.as_deref().unwrap_or(&name),
            &name,
            category.as_deref(),
            tags.unwrap_or_default(),
        )?;
        let root = inner
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        (preset, root)
    };
    run_blocking(move || {
        let outcome = preset_library().save_project_patch(
            &preset,
            root.as_deref(),
            overwrite.unwrap_or(false),
        )?;
        let state = app.state::<AppState>();
        state.lock_inner().presets.reload_library();
        Ok(listing(outcome == SaveOutcome::Replaced))
    })
    .await
}

/// Remove a user-library preset.
#[tauri::command]
pub fn remove_library_patch(
    name: String,
    state: State<AppState>,
) -> Result<LibraryListing, String> {
    let mut inner = state.lock_inner();
    if !preset_library().remove(&name)? {
        return Err(format!("Library patch '{name}' not found"));
    }
    inner.presets.reload_library();
    Ok(listing(false))
}

/// Create `new_name` as an independent copy of `name`. With `reassign`, every
/// track and pattern that played the original now plays the copy, which is what
/// "Duplicate" and "Save as" mean in the editor.
pub(crate) fn duplicate_preset_in_project(
    project: &mut ast::Project,
    name: &str,
    new_name: &str,
    reassign: bool,
) -> Result<String, String> {
    let new_name = validate_new_name(project, name, new_name)?;
    let index = project
        .presets
        .iter()
        .position(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    let mut copy = project.presets[index].clone();
    copy.name = new_name.clone();
    project.presets.push(copy);
    if reassign {
        reassign_references(project, name, &new_name);
    }
    Ok(new_name)
}

/// First free `<base> copy`, `<base> copy 2`, ... in the project, the same
/// suffix the patch editor's Duplicate uses. Saving a copy again extends the
/// original base rather than doubling the suffix.
pub(crate) fn preset_copy_name(project: &ast::Project, base: &str) -> String {
    let base = base.strip_suffix(" copy").unwrap_or(base);
    let first = format!("{base} copy");
    if !project.presets.iter().any(|preset| preset.name == first) {
        return first;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} copy {n}");
        if !project
            .presets
            .iter()
            .any(|preset| preset.name == candidate)
        {
            return candidate;
        }
        n += 1;
    }
}

/// Copy the preset a track plays and return the copy's name; the track stays on
/// the original so callers can decide who moves. `None` means no preset.
pub(crate) fn clone_track_preset(
    project: &mut ast::Project,
    track_index: usize,
) -> Result<Option<String>, String> {
    let name = project
        .tracks
        .get(track_index)
        .ok_or_else(|| format!("Track {track_index} out of range"))?
        .preset
        .clone();
    let Some(name) = name else {
        return Ok(None);
    };
    if !project.presets.iter().any(|preset| preset.name == name) {
        return Err(format!("Track {track_index} uses unknown preset '{name}'"));
    }
    let copy = preset_copy_name(project, &name);
    duplicate_preset_in_project(project, &name, &copy, false)?;
    Ok(Some(copy))
}

/// Save-as-new for one track: copy its preset and point only this track at it.
pub(crate) fn fork_track_preset(
    project: &mut ast::Project,
    track_index: usize,
) -> Result<Option<String>, String> {
    let copy = clone_track_preset(project, track_index)?;
    if let Some(copy) = &copy {
        project.tracks[track_index].preset = Some(copy.clone());
    }
    Ok(copy)
}

/// "Save as new preset" for one track: fork the preset, then point only this
/// track at the copy.
pub(crate) fn save_track_preset_as_new_in_project(
    project: &mut ast::Project,
    track_index: usize,
) -> Result<String, String> {
    fork_track_preset(project, track_index)?
        .ok_or_else(|| format!("Track {track_index} has no preset to save"))
}

/// Rename a project preset and every reference to it in one step.
pub(crate) fn rename_preset_in_project(
    project: &mut ast::Project,
    name: &str,
    new_name: &str,
) -> Result<String, String> {
    let new_name = validate_new_name(project, name, new_name)?;
    let index = project
        .presets
        .iter()
        .position(|preset| preset.name == name)
        .ok_or_else(|| format!("Preset '{name}' not found"))?;
    project.presets[index].name = new_name.clone();
    reassign_references(project, name, &new_name);
    for preset in &mut project.presets {
        if preset.extends.as_deref() == Some(name) {
            preset.extends = Some(new_name.clone());
        }
    }
    Ok(new_name)
}

#[cfg(test)]
pub(crate) fn save_patch_to_library_with(
    library: &PresetLibrary,
    preset: &SoundPreset,
    overwrite: bool,
) -> Result<SaveOutcome, String> {
    library.save_project_patch(preset, None, overwrite)
}

/// Build the library entry for one project preset.
pub(crate) fn sound_preset_from_project(
    project: &ast::Project,
    preset_name: &str,
    name: &str,
    category: Option<&str>,
    tags: Vec<String>,
) -> Result<SoundPreset, String> {
    let preset = oc_dsl::preset::resolve_preset(&project.presets, preset_name)?;
    if preset.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
        return Err(format!("Preset '{preset_name}' is not an OpenSynth patch"));
    }
    let category = category
        .map(PresetCategory::from_slug)
        .unwrap_or(PresetCategory::Synth);
    let mut tags: Vec<String> = tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    if !tags.iter().any(|tag| tag == "user") {
        tags.insert(0, "user".into());
    }
    Ok(SoundPreset {
        id: format!("library:{name}"),
        name: name.to_string(),
        category,
        tags,
        backend: oc_engine::preset::PresetBackend::BuiltinPlugin {
            plugin_id: oc_synth::synth::PLUGIN_ID.into(),
        },
        tier: oc_engine::preset::PresetTier::UserLibrary,
        patch: preset.patch,
        params: preset.params,
        effects: preset.effects,
    })
}

fn listing(replaced: bool) -> LibraryListing {
    let scan = preset_library().scan();
    let mut patches: Vec<LibraryEntry> = scan
        .presets
        .iter()
        .map(|preset| LibraryEntry::from_scan(preset, &scan))
        .collect();
    sort_for_browser(&mut patches);
    LibraryListing {
        patches,
        warnings: scan.warnings,
        replaced,
    }
}

/// The user library. A test build never resolves the operator's home: it uses
/// the process scratch root.
pub(crate) fn preset_library() -> PresetLibrary {
    #[cfg(test)]
    {
        PresetLibrary::at(crate::private_fs::config_dir().join("presets"))
    }
    #[cfg(not(test))]
    {
        PresetLibrary::default()
    }
}

/// The factory-patch preference store beside the library. A test build keeps it
/// in the process scratch root.
pub(crate) fn preset_preferences() -> PresetPreferences {
    #[cfg(test)]
    {
        PresetPreferences::at(PresetPreferences::path_beside(
            &crate::private_fs::config_dir().join("presets"),
        ))
    }
    #[cfg(not(test))]
    {
        PresetPreferences::default()
    }
}

/// Factory patches have no file of their own; their favourite and last-used
/// stamps live in `~/.opencompose/preferences.json` keyed by `factory:<name>`.
#[tauri::command]
pub fn list_patch_preferences() -> PresetPreferences {
    preset_preferences().load()
}

/// Toggle one row's favourite flag. `tier` is `user` for library files and
/// anything else for factory rows. Returns whether the row still exists.
#[tauri::command]
pub fn set_patch_favorite(
    name: String,
    tier: Option<String>,
    favorite: bool,
) -> Result<bool, String> {
    if tier.as_deref() == Some("user") {
        return preset_library().set_favorite(name.trim(), favorite);
    }
    let mut preferences = preset_preferences().load();
    preferences.set_factory_favorite(name.trim(), favorite);
    preferences.save()?;
    Ok(true)
}

/// Record that a patch was actually loaded. Auditioning never reaches this.
pub(crate) fn record_patch_use(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let now = unix_now();
    match preset_library().mark_used(name, now) {
        Ok(true) => {}
        Ok(false) => {
            let mut preferences = preset_preferences().load();
            preferences.mark_factory_used(name, now);
            if let Err(error) = preferences.save() {
                log::warn!("Failed to record patch use for '{name}': {error}");
            }
        }
        Err(error) => log::warn!("Failed to record patch use for '{name}': {error}"),
    }
}

fn validate_new_name(project: &ast::Project, name: &str, new_name: &str) -> Result<String, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("Preset name cannot be empty".into());
    }
    if new_name == name {
        return Err(format!("Preset '{name}' already has that name"));
    }
    if project.presets.iter().any(|preset| preset.name == new_name) {
        return Err(format!("Preset '{new_name}' already exists"));
    }
    Ok(new_name.to_string())
}

/// Point tracks and patterns from `old` to `new`; everything else (clips, kit
/// layers, automation) refers to musical objects, not presets.
fn reassign_references(project: &mut ast::Project, old: &str, new: &str) {
    let fix = |slot: &mut Option<String>| {
        if slot.as_deref() == Some(old) {
            *slot = Some(new.to_string());
        }
    };
    for track in &mut project.tracks {
        fix(&mut track.preset);
        for pattern in &mut track.inline_patterns {
            fix(&mut pattern.preset);
        }
    }
    for pattern in &mut project.patterns {
        fix(&mut pattern.preset);
    }
}
