//! Project/file lifecycle, project properties, tracks, and undo history.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Manager, State};

use oc_dsl::ast::{self, *};

use crate::process_lock::{LockError, ProcessLock};
use crate::state::{AppState, AppStateInner};
use crate::state::{UndoEdit, UndoScope};

use super::arrangement::warm_audio_clip_peaks;
use super::patch::clone_track_preset;
use super::plugin_host::warm_transport_plugins;
use super::shared::{ProjectSnapshot, project_root, resync_with_undo, run_blocking, snapshot};

fn has_active_take(inner: &AppStateInner) -> bool {
    inner.audio.recorder.recording() || inner.audio.midi_recorder.recording()
}

/// Refuse a project replacement while a batch or a take holds the document.
fn refuse_project_change(inner: &AppStateInner) -> Result<(), String> {
    inner.refuse_in_transaction()?;
    if has_active_take(inner) {
        return Err("Stop recording before changing projects.".into());
    }
    Ok(())
}

fn starter_template(template: Option<&str>) -> Result<bool, String> {
    match template {
        None => Ok(false),
        Some("starter") => Ok(true),
        Some(other) => Err(format!("Unknown project template: {other}")),
    }
}

/// Why a project operation was refused. The tag is a stable wire name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectError {
    /// Another process holds this project's lock.
    ProjectInUse { message: String },
    /// Any other refusal or failure.
    Project { message: String },
}

impl ProjectError {
    pub(crate) fn in_use() -> Self {
        Self::ProjectInUse {
            message: "Another OpenCompose process has this project open.".into(),
        }
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self::Project {
            message: message.into(),
        }
    }

    fn from_lock(error: LockError) -> Self {
        if error.is_in_use() {
            Self::in_use()
        } else {
            Self::other(format!("Cannot lock the project: {error}"))
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::ProjectInUse { message } | Self::Project { message } => message,
        }
    }
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ProjectError {}

impl From<String> for ProjectError {
    fn from(message: String) -> Self {
        Self::other(message)
    }
}

impl From<&str> for ProjectError {
    fn from(message: &str) -> Self {
        Self::other(message)
    }
}

impl From<ProjectError> for String {
    fn from(error: ProjectError) -> Self {
        error.message().to_owned()
    }
}

/// Take the lock a project file is opened under.
fn acquire_project_lock(path: &Path) -> Result<ProcessLock, ProjectError> {
    ProcessLock::acquire(crate::project_data::project_lock_path(path))
        .map_err(ProjectError::from_lock)
}

/// File ▸ New at a chosen path: one command creates the document, names it after
/// the file and writes it, so the workspace's New is not New + rename + save.
///
/// The name and the directory are part of the document before the first save,
/// and the graph warms behind the response.
#[tauri::command]
pub async fn new_project_at(
    path: String,
    template: Option<String>,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, ProjectError> {
    let starter = starter_template(template.as_deref())?;
    create_project(app, Some(PathBuf::from(path)), starter).await
}

async fn create_project(
    app: tauri::AppHandle,
    path: Option<PathBuf>,
    starter: bool,
) -> Result<ProjectSnapshot, ProjectError> {
    app.state::<AppState>().ensure_writable()?;
    app.state::<AppState>().refuse_in_transaction()?;
    let load_epoch = begin_project_load(&app.state::<AppState>());
    let snapshot = create_project_on_main_thread(app.clone(), path, load_epoch, starter).await?;
    // The document is on screen; the graph and the device open behind it.
    warm_preload_later(&app, snapshot.revision);
    Ok(snapshot)
}

/// The document File ▸ New installs, named before it is first saved.
pub(crate) fn new_project_document(name: String) -> Project {
    Project {
        name: Some(name),
        ocp_version: Some(oc_dsl::DSL_VERSION.to_string()),
        properties: ProjectProperties::default(),
        tracks: vec![],
        patterns: vec![],
        presets: vec![],
        buses: vec![],
        master: None,
        sections: vec![],
        markers: Vec::new(),
        groups: Vec::new(),
    }
}

/// The New document in one command: name, directory and file.
///
/// The name comes from the file, and the file is written under the same load
/// token that installs the document, so a directory listing taken after the
/// response names the project the panel already shows. The graph stays cold
/// for the caller's warm-up.
pub(crate) fn create_project_in(
    inner: &mut AppStateInner,
    path: Option<PathBuf>,
) -> Result<ProjectSnapshot, ProjectError> {
    let name = path
        .as_deref()
        .and_then(Path::file_stem)
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_string());
    // A New over a path another process holds must leave that file and this
    // document alone, so the lock is taken before the file is claimed.
    let _target_lock = path.as_deref().map(acquire_project_lock).transpose()?;
    refuse_project_change(inner)?;
    let mut project = new_project_document(name);
    let text = match path.as_deref() {
        Some(path) => {
            // The file reaches disk before the document changes, so a refused
            // or failed write leaves the open project and its history alone.
            crate::project_resources::claim_new(path)?;
            oc_dsl::instruments::apply_defaults(&mut project);
            let source = oc_dsl::serialize(&project);
            if let Err(error) = crate::project_resources::write_new(path, &source) {
                let _ = std::fs::remove_file(path);
                return Err(error.into());
            }
            Some(ProjectText {
                source,
                parsed: project.clone(),
            })
        }
        None => None,
    };
    match replace_project_unchecked(inner, project, path.clone(), false, text, path.is_some()) {
        Ok(snapshot) => {
            if let Some(path) = path.as_deref() {
                finish_new_project_file(inner, path);
            }
            Ok(snapshot)
        }
        Err(error) => {
            if let Some(path) = path.as_deref() {
                let _ = std::fs::remove_file(path);
            }
            Err(error)
        }
    }
}

/// Create the complete sample-free starter before replacing the current project.
/// Exclusive creation protects existing files, even if a picker reply is stale.
pub(crate) fn create_starter_project_in(
    inner: &mut AppStateInner,
    path: PathBuf,
) -> Result<ProjectSnapshot, ProjectError> {
    let _target_lock = acquire_project_lock(&path)?;
    refuse_project_change(inner)?;
    let mut project = oc_dsl::parse(include_str!("starter.ocp"))
        .map_err(|error| format!("Invalid bundled starter: {error}"))?;
    project.name = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned());
    oc_dsl::instruments::apply_defaults(&mut project);
    let source = oc_dsl::serialize(&project);
    crate::project_resources::claim_new(&path)
        .map_err(|error| format!("Cannot create starter {error}. Choose a new filename."))?;
    if let Err(error) = crate::project_resources::write_new(&path, &source) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("Cannot write starter {error}").into());
    }
    let text = Some(ProjectText {
        source,
        parsed: project.clone(),
    });
    match replace_project_unchecked(inner, project, Some(path.clone()), false, text, true) {
        Ok(snapshot) => {
            finish_new_project_file(inner, &path);
            Ok(snapshot)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
}

/// Open a project file.
#[tauri::command]
pub async fn open_file(
    path: String,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, ProjectError> {
    app.state::<AppState>().ensure_writable()?;
    app.state::<AppState>().refuse_in_transaction()?;
    let load_epoch = begin_project_load(&app.state::<AppState>());
    let started = std::time::Instant::now();
    // Reading, parsing and plugin resolution are all blocking work; the commit
    // stays on the main thread so the document reaches the panel in one step.
    let read_path = path.clone();
    let (project, text) = run_blocking(move || {
        let source = std::fs::read_to_string(&read_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        let (project, text) = parse_project_source(source)?;
        // Start plugin resolution before the audio build so the first press
        // finds it done; the scan is otherwise paid on the press path.
        warm_transport_plugins(&project);
        Ok::<_, String>((project, text))
    })
    .await?;
    let parsed = started.elapsed();
    let snapshot = replace_project_on_main_thread(
        app.clone(),
        project,
        Some(PathBuf::from(&path)),
        load_epoch,
        false,
        false,
        Some(text),
    )
    .await?;
    log::info!(
        "gui: open_file parse={parsed:?} commit={:?} total={:?}",
        started.elapsed() - parsed,
        started.elapsed()
    );
    // The document is on screen; the graph and the device open behind it.
    warm_preload_later(&app, snapshot.revision);
    warm_audio_clip_peaks(&snapshot.project, Path::new(&path).parent());
    Ok(snapshot)
}

/// A project file's text and the project it parses to.
///
/// The text travels beside its parse so the installer can tell whether the
/// document it installs still describes the file.
pub(crate) struct ProjectText {
    pub(crate) source: String,
    pub(crate) parsed: Project,
}

/// Parse a project file's text and add the version an older file omits.
///
/// Returns the project the loader installs before default injection together
/// with the text and its untouched parse, so the installer can keep the text
/// when the two still agree.
pub(crate) fn parse_project_source(source: String) -> Result<(Project, ProjectText), String> {
    let parsed = oc_dsl::parse(&source).map_err(|e| format!("Parse error: {}", e))?;
    let mut project = parsed.clone();
    if project.ocp_version.is_none() {
        project.ocp_version = Some(oc_dsl::DSL_VERSION.to_string());
    }
    Ok((project, ProjectText { source, parsed }))
}

/// The document a project text installs as: the parse with the added version
/// and the injected defaults the loader applies. The history compares its
/// recorded document through the same steps, so a file that reopens unchanged
/// never reads as an edit made outside the app.
pub(crate) fn installed_project(source: &str) -> Result<Project, String> {
    let (mut project, _) = parse_project_source(source.to_string())?;
    oc_dsl::instruments::apply_defaults(&mut project);
    Ok(project)
}

pub(crate) fn begin_project_load(state: &AppState) -> u64 {
    let mut epoch = state
        .project_load_epoch
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *epoch = epoch.wrapping_add(1);
    *epoch
}

/// Hold request acceptance through the commit: a newer accepted request must
/// never be overwritten by an older parse or queued main-thread callback.
fn commit_project_load<T, E>(
    gate: &std::sync::Mutex<u64>,
    expected: u64,
    apply: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    E: From<String>,
{
    let current = gate.lock().unwrap_or_else(|e| e.into_inner());
    if *current != expected {
        return Err("Project load superseded by a newer request"
            .to_owned()
            .into());
    }
    apply()
}

pub(crate) async fn replace_project_on_main_thread(
    app: tauri::AppHandle,
    project: Project,
    path: Option<PathBuf>,
    load_epoch: u64,
    recovered: bool,
    preload: bool,
    text: Option<ProjectText>,
) -> Result<ProjectSnapshot, ProjectError> {
    // Replacing the project destroys the current undo history; keep a recovery
    // copy of unsaved work before it goes.
    save_before_replacement(&app);
    dispatch_replacement(app, load_epoch, move |inner| {
        replace_project_with(inner, project, path, preload, text, false)?;
        if recovered {
            inner.dirty = true;
            inner.autosave_at = None;
            inner.autosave_digest = 0;
        }
        Ok(())
    })
    .await
}

async fn create_project_on_main_thread(
    app: tauri::AppHandle,
    path: Option<PathBuf>,
    load_epoch: u64,
    starter: bool,
) -> Result<ProjectSnapshot, ProjectError> {
    save_before_replacement(&app);
    dispatch_replacement(app, load_epoch, move |inner| {
        if starter {
            let path = path.ok_or("A starter project needs a filename")?;
            create_starter_project_in(inner, path)?;
        } else {
            create_project_in(inner, path)?;
        }
        Ok(())
    })
    .await
}

/// Keep a recovery copy of unsaved work before a replacement destroys it.
fn save_before_replacement(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>()
        && let Err(error) = crate::autosave::save_now(&state)
    {
        log::warn!("autosave before project replacement: {error}");
    }
}

/// Apply a replacement on the main thread, under the load gate and the state
/// lock, and answer with the snapshot the replacement produced.
async fn dispatch_replacement(
    app: tauri::AppHandle,
    load_epoch: u64,
    apply: impl FnOnce(&mut AppStateInner) -> Result<(), ProjectError> + std::marker::Send + 'static,
) -> Result<ProjectSnapshot, ProjectError> {
    let handle = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let result = commit_project_load(&state.project_load_epoch, load_epoch, || {
            let mut inner = state.lock_inner();
            apply(&mut inner)?;
            let result = snapshot(&inner);
            drop(inner);
            state.discard_range_cache_hold();
            state.audition.invalidate();
            Ok(result)
        });
        let _ = tx.send(result);
    })
    .map_err(|e| ProjectError::other(format!("Failed to dispatch project replacement: {e}")))?;
    rx.await
        .map_err(|e| ProjectError::other(format!("Project replacement channel error: {e}")))?
}

/// Build the graph and open the output device behind the open response.
///
/// Paying this before the snapshot reaches the panel would cost eight hundred
/// milliseconds for a file that parses in seventy. The document is on screen
/// first and the warm runs here, where a newer document wins because the
/// revision is re-checked before any work starts.
pub(crate) fn warm_preload_later(app: &tauri::AppHandle, revision: u64) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        warm_preload_if_current(&state, revision);
    });
}

/// Build the graph and open the output device, unless a newer document landed.
///
/// The revision is re-checked under the same lock the warm-up holds, so a
/// second New or Open, or an edit, is never overwritten by work queued for an
/// older document. Returns whether the warm-up ran.
pub(crate) fn warm_preload_if_current(state: &AppState, revision: u64) -> bool {
    let mut inner = state.lock_inner();
    // Play may already have queued the current graph.
    if inner.revision != revision || !inner.audio.render_dirty {
        return false;
    }
    let project = inner.project.clone();
    let started = std::time::Instant::now();
    inner.audio.preload_project(&project);
    log::info!(
        "gui: preload_project (behind the open) {:?}",
        started.elapsed()
    );
    true
}

/// Main-thread only: editors must be detached before their plugin is destroyed.
#[cfg(test)]
pub(crate) fn replace_project(
    inner: &mut crate::state::AppStateInner,
    project: Project,
    path: Option<PathBuf>,
) -> Result<ProjectSnapshot, String> {
    replace_project_with(inner, project, path, true, None, false).map_err(String::from)
}

/// Install a project file through the open path, without the app handle.
///
/// The command reads and parses off the main thread and dispatches the install;
/// this runs the same steps inline for tests.
#[cfg(test)]
pub(crate) fn open_project(
    inner: &mut crate::state::AppStateInner,
    source: &str,
    path: Option<PathBuf>,
) -> Result<ProjectSnapshot, String> {
    let (project, text) = parse_project_source(source.to_string()).map_err(ProjectError::other)?;
    replace_project_with(inner, project, path, false, Some(text), false).map_err(String::from)
}

/// The replacement, with the audio warm-up under the caller's control.
///
/// A project opened from a file pays the warm-up behind the open instead: the
/// snapshot is returned first and [`warm_preload_later`] builds the graph while
/// the panel paints. Every other replacement keeps it inline.
fn replace_project_with(
    inner: &mut crate::state::AppStateInner,
    project: Project,
    path: Option<PathBuf>,
    preload: bool,
    text: Option<ProjectText>,
    fresh_history: bool,
) -> Result<ProjectSnapshot, ProjectError> {
    // A batch's edits exist only in memory until it commits; replacing the
    // document would drop them and leave the agent committing into nothing.
    refuse_project_change(inner)?;
    replace_project_unchecked(inner, project, path, preload, text, fresh_history)
}

/// The replacement after its refusals, for a caller that already made them.
fn replace_project_unchecked(
    inner: &mut crate::state::AppStateInner,
    mut project: Project,
    path: Option<PathBuf>,
    preload: bool,
    text: Option<ProjectText>,
    fresh_history: bool,
) -> Result<ProjectSnapshot, ProjectError> {
    // The target's lock is taken before the outgoing document is touched and
    // kept in a local guard until the switch commits, so a refusal elsewhere
    // leaves both documents and their histories as they were.
    let next_lock = path.as_deref().map(acquire_project_lock).transpose()?;
    inner.close_editor_helpers();
    #[cfg(target_os = "macos")]
    for instance in inner.vst3_plugin_instances.values() {
        if let Some(gui) = crate::state::lock_vst3_preview(instance).try_gui_handle()? {
            gui.close()?;
        }
    }
    inner.open_clap_editors.clear();
    let retired_clap = std::mem::take(&mut inner.plugin_instances);
    for (_, instance) in retired_clap {
        instance.retire_deferred();
    }
    inner.audio.reset_project();
    inner.vst3_plugin_instances.clear();
    inner.vst2_plugin_instances.clear();
    inner.lv2_plugin_instances.clear();
    #[cfg(target_os = "macos")]
    inner.au_plugin_instances.clear();
    inner.vst3_editor_keepalives.clear();
    inner.plugin_automation_gestures.clear();
    inner.plugin_param_cache.clear();
    oc_host::param_edit::clear_param_edit_buses();
    oc_dsl::instruments::apply_defaults(&mut project);
    // Keep the file's text when it still describes the installed document, so
    // comments and hand formatting survive a save. An injected default or an
    // added version makes the two differ: the source is then regenerated from
    // the AST and the file stays valid.
    let dsl_source = match text {
        Some(text) if text.parsed == project => text.source,
        _ => oc_dsl::serialize(&project),
    };
    inner.file_path = path;
    inner.dirty = false;
    // A replacement is a different document: an earlier Don't Save answer, and
    // the exit write it armed, belong to the one it replaced.
    inner.discard_pending = false;
    inner.exit_ready = false;
    inner.autosave_at = None;
    inner.autosave_digest = 0;
    // The outgoing document's history is complete on disk before it goes.
    inner.sync_history();
    inner.undo.clear();
    inner.history_move = None;
    inner.transaction = None;
    // A replacement is a new document; a preview of the old one cannot survive.
    inner.preview = None;
    // New replaces the target's history only after the outgoing document's
    // history is on disk, so its entries never land in the fresh folder.
    if fresh_history && let Some(path) = inner.file_path.clone() {
        inner.start_history(&path);
    }
    // The new document becomes the locked one here; the outgoing lock drops
    // with the value it is replacing.
    inner.project_lock = next_lock;
    install_document_with(inner, project, dsl_source, preload);
    inner.attach_history();
    Ok(snapshot(inner))
}

/// Publish `project` as the live document and rebuild what depends on it.
///
/// The timeline and the editor read the snapshot, and the audio graph is
/// rebuilt off the UI thread. History, path and the dirty flag belong to the
/// caller: a project replacement resets them, and a preview side swap keeps
/// them so the comparison costs neither an undo entry nor a parse.
pub(crate) fn install_document(
    inner: &mut crate::state::AppStateInner,
    project: Project,
    dsl_source: String,
) {
    install_document_with(inner, project, dsl_source, true);
}

/// Install a document, optionally leaving the warm-up to the caller.
///
/// The warm-up opens the output device and builds the graph. Opening a project
/// returns its snapshot first and warms afterwards; an edit warms inline,
/// because the next press must find the graph ready.
pub(crate) fn install_document_with(
    inner: &mut crate::state::AppStateInner,
    project: Project,
    dsl_source: String,
    preload: bool,
) {
    inner.audio.retire_owned_effect_touches();
    inner.project = project;
    inner.dsl_source = dsl_source;
    inner.bump_revision();
    inner.audio.resource_root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    if !preload {
        return;
    }
    // Build the graph and pre-render the opening lookahead off the UI thread,
    // so the first space press only starts the consumer.
    let started = std::time::Instant::now();
    inner.audio.preload_project(&inner.project);
    log::info!("gui: preload_project {:?}", started.elapsed());
}

/// Save the document: capture it under the lock, write without the lock, then
/// publish the result.
///
/// The write is the slow half — Save As reads and copies up to 256 MiB of
/// resources — so it runs off the webview thread and does not hold the
/// document lock that every other command needs.
#[tauri::command]
pub async fn save_file(
    path: Option<String>,
    app: tauri::AppHandle,
) -> Result<SavedFile, ProjectError> {
    let handle = app.clone();
    let saved = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        save_document(path, &state)
    })
    .await
    .map_err(|error| ProjectError::other(format!("Background task failed: {error}")))??;
    // Save As moved the resources: point playback at the copy behind the
    // response, exactly as opening a project warms its graph.
    if saved.repointed {
        warm_preload_later(&app, saved.revision);
    }
    Ok(saved)
}

/// The save body without a Tauri app handle, for the tests that drive the
/// command layer directly. The command itself is [`save_file`].
#[cfg(test)]
pub fn save_file_inner(path: Option<String>, state: &AppState) -> Result<String, String> {
    save_document(path, state)
        .map(|saved| saved.path)
        .map_err(String::from)
}

/// A save captured under the document lock, written without it.
pub(crate) struct SaveRequest {
    pub(crate) project: Project,
    pub(crate) dsl_source: String,
    /// The path the document had when it was captured; `None` for an untitled
    /// project, whose recovery copies are keyed by absence of a path.
    pub(crate) old_path: Option<PathBuf>,
    pub(crate) save_path: PathBuf,
    pub(crate) revision: u64,
    relocated: bool,
    /// How the project's stores follow the file when the save gives it a new
    /// path. Save As copies; Move Project moves.
    relocation: crate::project_data::Relocation,
    /// The relocation captured here, to run after the document lock drops.
    pending: Option<crate::project_data::PendingRelocation>,
    /// SHA-256 of the bytes the save wrote, recorded in the history marker.
    file_sha: Option<String>,
    /// The destination's lock, taken before the write and installed when the
    /// document moves to that path.
    target_lock: Option<ProcessLock>,
}

impl SaveRequest {
    /// Whether the save gives the project another file.
    fn changes_path(&self) -> bool {
        self.old_path.as_deref() != Some(self.save_path.as_path())
    }
}

/// What a save wrote, and whether playback has to follow it.
#[derive(Serialize)]
pub struct SavedFile {
    pub(crate) path: String,
    /// The destination directory moved, so the graph needs re-pointing.
    #[serde(skip)]
    pub(crate) repointed: bool,
    #[serde(skip)]
    pub(crate) revision: u64,
    snapshot: Option<ProjectSnapshot>,
}

/// Take everything a save needs from the document in one lock scope.
pub(crate) fn snapshot_for_save(
    path: Option<String>,
    state: &AppState,
) -> Result<SaveRequest, ProjectError> {
    let mut inner = state.lock_inner();
    // A preview is not the document: never write either side to disk.
    inner.refuse_in_preview()?;
    // Never persist a half-applied agent batch.
    if inner.transaction.is_some() {
        return Err("commit or roll back the active transaction before saving".into());
    }
    let save_path = match path {
        Some(p) => PathBuf::from(p),
        None => inner
            .file_path
            .clone()
            .ok_or_else(|| "No file path specified".to_string())?,
    };
    if has_active_take(&inner)
        && inner.file_path.as_deref().and_then(Path::parent) != save_path.parent()
    {
        return Err("Stop recording before saving the project in another folder.".into());
    }
    // Save As reserves its destination before it writes, so a path another
    // process holds leaves both files and this document unchanged.
    let target_lock = if inner.file_path.as_deref() != Some(save_path.as_path()) {
        Some(acquire_project_lock(&save_path)?)
    } else {
        None
    };
    // A Save As copies the history from disk, so the disk must be current.
    inner.sync_history();
    Ok(SaveRequest {
        project: inner.project.clone(),
        dsl_source: inner.dsl_source.clone(),
        old_path: inner.file_path.clone(),
        save_path,
        revision: inner.revision,
        relocated: false,
        relocation: crate::project_data::Relocation::Copy,
        pending: None,
        file_sha: None,
        target_lock,
    })
}

/// Write the captured document. Runs without the document lock.
pub(crate) fn write_save(request: &mut SaveRequest) -> Result<(), String> {
    write_save_with_library(request, super::patch::preset_library().dir())
}

pub(crate) fn write_save_with_library(
    request: &mut SaveRequest,
    library: &Path,
) -> Result<(), String> {
    let prepared = crate::project_resources::save_with_assets(
        &request.project,
        &request.dsl_source,
        request.old_path.as_deref(),
        &request.save_path,
        Some(library),
    )?;
    if let Some((project, source)) = prepared {
        request.project = project;
        request.dsl_source = source;
        request.relocated = true;
    }
    request.file_sha = crate::project_data::content_sha256_bytes(request.dsl_source.as_bytes());
    Ok(())
}

/// Publish a written save onto the live document.
///
/// Returns `None` when the document moved on during the write — an edit, a
/// project load, or an agent batch — because the file on disk then holds the
/// older revision and the newer document must stay dirty. Otherwise returns
/// whether the destination moved playback's resource root.
pub(crate) fn publish_save(state: &AppState, request: &mut SaveRequest) -> Option<bool> {
    let mut inner = state.lock_inner();
    if inner.revision != request.revision || inner.transaction.is_some() {
        return None;
    }
    if has_active_take(&inner)
        && inner.file_path.as_deref().and_then(Path::parent) != request.save_path.parent()
    {
        return None;
    }
    if request.relocated {
        inner.project = request.project.clone();
        inner.dsl_source = request.dsl_source.clone();
        inner.bump_revision();
    }
    inner.file_path = Some(request.save_path.clone());
    if let Some(lock) = request.target_lock.take() {
        inner.project_lock = Some(lock);
    }
    inner.dirty = false;
    inner.autosave_at = None;
    inner.autosave_digest = 0;
    if request.changes_path() {
        if let Some(from) = request.old_path.clone() {
            let pending = crate::project_data::PendingRelocation {
                from,
                to: request.save_path.clone(),
                how: request.relocation,
            };
            inner.begin_history_move(pending.clone());
            request.pending = Some(pending);
        } else {
            inner.rehome_history(crate::state::JournalHome::Fresh, request.file_sha.clone());
        }
    } else {
        let path = request.save_path.clone();
        inner.record_written(&path, request.file_sha.clone());
    }
    let root = request.save_path.parent().map(Path::to_path_buf);
    let repointed =
        request.relocated || needs_repoint(inner.audio.resource_root.as_deref(), root.as_deref());
    if repointed {
        inner.audio.resource_root = root;
        // Play can win the lock before the deferred warm-up. Its load must
        // already target the copied media, even with a graph ready to resume.
        inner.audio.mark_dirty();
    }
    Some(repointed)
}

/// Whether playback has to follow a save into another resource directory.
///
/// A plain save writes next to the file it already has, so the graph keeps
/// playing and the save is inaudible.
pub(crate) fn needs_repoint(old_root: Option<&Path>, new_root: Option<&Path>) -> bool {
    old_root != new_root
}

pub(crate) fn save_document(
    path: Option<String>,
    state: &AppState,
) -> Result<SavedFile, ProjectError> {
    let _timer = oc_diag::perf_counter("cmd.save_file.ms").map(oc_diag::Timer::start);
    let mut request = snapshot_for_save(path, state)?;
    // The write runs unlocked; the session key is part of the recovery store's
    // identity, so capture it with the snapshot.
    let session = state.lock_inner().session_id.clone();
    write_save(&mut request)?;
    // A clean save makes the recovery copies obsolete, including any written
    // while the project had no path yet.
    if let Err(error) = crate::autosave::clear_saved(
        &crate::autosave::app_root(),
        &request.save_path,
        request.old_path.is_none(),
        &session,
    ) {
        log::warn!("autosave: {error}");
    }
    let published = publish_save(state, &mut request);
    if let Some(pending) = request.pending.take() {
        let report = pending.run();
        state
            .lock_inner()
            .finish_history_move(report, request.file_sha.clone());
    }
    let revision = request
        .revision
        .saturating_add(u64::from(request.relocated && published.is_some()));
    let saved_snapshot = if request.relocated && published.is_some() {
        let inner = state.lock_inner();
        (inner.revision == revision).then(|| snapshot(&inner))
    } else {
        None
    };
    // The file is on disk either way; an edit that landed meanwhile only keeps
    // its own dirty flag, never a lower revision marked clean.
    Ok(SavedFile {
        path: request.save_path.display().to_string(),
        repointed: published.unwrap_or(false),
        revision,
        snapshot: saved_snapshot,
    })
}

/// Finish the bookkeeping of a New whose file was written before the document
/// changed. The replacement already set the path, revision and history.
fn finish_new_project_file(inner: &mut AppStateInner, path: &Path) {
    if let Err(error) =
        crate::autosave::clear_saved(&crate::autosave::app_root(), path, false, &inner.session_id)
    {
        log::warn!("autosave: {error}");
    }
    inner.autosave_at = None;
    inner.autosave_digest = 0;
    inner.audio.resource_root = inner
        .file_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    inner.audio.mark_dirty();
    inner.dirty = false;
}

/// File > Move Project: give the project another file, taking its history
/// (and its backup) along, and leave nothing at the old place.
///
/// In the same folder the files are renamed. Across folders the project is
/// saved at the destination the way Save As saves it (its media is copied
/// beside it), the history is copied, and only then are the old files
/// removed. Any failure leaves the project where it was.
#[tauri::command]
pub async fn move_project(
    to: String,
    app: tauri::AppHandle,
) -> Result<ProjectSnapshot, ProjectError> {
    let handle = app.clone();
    let (snapshot, repointed) = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        move_project_inner(&state, PathBuf::from(to))
    })
    .await
    .map_err(|error| ProjectError::other(format!("Background task failed: {error}")))??;
    if repointed {
        warm_preload_later(&app, snapshot.revision);
    }
    Ok(snapshot)
}

/// The move without the app handle; returns the snapshot and whether
/// playback has to follow the media to another folder.
pub(crate) fn move_project_inner(
    state: &AppState,
    to: PathBuf,
) -> Result<(ProjectSnapshot, bool), ProjectError> {
    let from = {
        let inner = state.lock_inner();
        inner.refuse_in_preview()?;
        if inner.transaction.is_some() {
            return Err(
                "commit or roll back the active transaction before moving the project".into(),
            );
        }
        if has_active_take(&inner) {
            return Err("Stop recording before moving the project.".into());
        }
        inner
            .file_path
            .clone()
            .ok_or("Save the project before moving it.")?
    };
    if to == from {
        return Ok((snapshot(&state.lock_inner()), false));
    }
    if to.exists() {
        return Err(format!(
            "{} already exists; choose a name that is free.",
            to.display()
        )
        .into());
    }
    let backup = crate::project_persist::backup_path(&from);
    let file_sha = crate::project_data::content_sha256(&from);
    if from.parent() == to.parent() {
        // The destination's lock is taken before the rename; a path another
        // process holds leaves the project where it is.
        let next_lock = acquire_project_lock(&to)?;
        let pending = {
            let mut inner = state.lock_inner();
            inner.sync_history();
            std::fs::rename(&from, &to)
                .map_err(|error| format!("Cannot move the project: {error}"))?;
            inner.file_path = Some(to.clone());
            inner.project_lock = Some(next_lock);
            let pending = crate::project_data::PendingRelocation {
                from: from.clone(),
                to: to.clone(),
                how: crate::project_data::Relocation::Move,
            };
            inner.begin_history_move(pending.clone());
            pending
        };
        let report = pending.run();
        state.lock_inner().finish_history_move(report, file_sha);
        if backup.exists() {
            let _ = std::fs::rename(&backup, crate::project_persist::backup_path(&to));
        }
        return Ok((snapshot(&state.lock_inner()), false));
    }
    let mut request = snapshot_for_save(Some(to.display().to_string()), state)?;
    request.relocation = crate::project_data::Relocation::Move;
    let undo_new_files = |request: &SaveRequest| {
        let _ = std::fs::remove_file(&request.save_path);
        let _ = crate::edit_history::journal::discard(&request.save_path);
    };
    if let Err(error) = write_save(&mut request) {
        undo_new_files(&request);
        return Err(error.into());
    }
    let Some(repointed) = publish_save(state, &mut request) else {
        undo_new_files(&request);
        return Err("The project changed during the move; try again.".into());
    };
    if let Some(pending) = request.pending.take() {
        let report = pending.run();
        state
            .lock_inner()
            .finish_history_move(report, request.file_sha.clone());
    }
    for (old, what) in [(&from, "file"), (&backup, "backup")] {
        if let Err(error) = std::fs::remove_file(old)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "move: the old project {what} stays at {}: {error}",
                old.display()
            );
        }
    }
    let inner = state.lock_inner();
    Ok((snapshot(&inner), repointed))
}

#[tauri::command]
pub fn get_project(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let inner = state.lock_inner();
    Ok(snapshot(&inner))
}

#[tauri::command]
pub fn get_dsl_source(state: State<AppState>) -> Result<String, String> {
    let inner = state.lock_inner();
    Ok(inner.dsl_source.clone())
}

#[tauri::command]
pub fn parse_dsl(source: String) -> Result<ast::Project, String> {
    oc_dsl::parse(&source).map_err(|e| format!("{}", e))
}

#[tauri::command]
pub fn update_dsl(source: String, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let (snapshot, root) = update_dsl_inner(source, &mut inner)?;
    drop(inner);
    warm_audio_clip_peaks(&snapshot.project, root.as_deref());
    Ok(snapshot)
}

/// Replaces the whole document with saved DSL as one labelled undo entry,
/// keeping the saved text exactly. Tests use it to build an entry that a
/// restored snapshot leaves behind. Nothing is written to disk: the caller
/// publishes with [`publish_restored`] once the rest of its step has
/// succeeded, so a step that fails leaves the files as they were.
#[cfg(test)]
pub(crate) fn restore_dsl_with_undo(
    inner: &mut AppStateInner,
    source: String,
    label: &str,
    cause: crate::edit_history::EditCause,
) -> Result<ProjectSnapshot, String> {
    inner.refuse_in_preview()?;
    let mut project = oc_dsl::parse(&source).map_err(|e| format!("{}", e))?;
    oc_dsl::instruments::apply_defaults(&mut project);
    let old_project = std::mem::replace(&mut inner.project, project);
    if let Err(error) = super::shared::resync_restored_with_undo(
        inner,
        old_project.clone(),
        UndoEdit::new(label, UndoScope::Project).cause(cause),
    ) {
        inner.project = old_project;
        return Err(error);
    }
    inner.dsl_source = source;
    Ok(snapshot(&*inner))
}

/// Writes a restored document to autosave and the project file. The restore
/// itself stands either way; the error says the file still holds the old text.
pub(crate) fn publish_restored(inner: &mut AppStateInner) -> Result<(), String> {
    crate::autosave::flush_dirty(inner);
    crate::project_persist::flush(inner)
        .map(|_| ())
        .map_err(|reason| format!("The restored project was not written to its file: {reason}"))
}

/// The command body: parse, apply defaults and keep the caller's exact text.
///
/// Returns the snapshot and the resource root to warm after the lock is gone.
pub(crate) fn update_dsl_inner(
    source: String,
    inner: &mut AppStateInner,
) -> Result<(ProjectSnapshot, Option<PathBuf>), String> {
    inner.refuse_in_preview()?;
    let mut new_project = oc_dsl::parse(&source).map_err(|e| format!("{}", e))?;
    oc_dsl::instruments::apply_defaults(&mut new_project);
    let old_project = std::mem::replace(&mut inner.project, new_project);
    // resync_with_undo will re-serialize, but we want the user's exact source text
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("DSL edit", UndoScope::Project),
    )?;
    // Override the serialized DSL with the user's exact input (preserves
    // formatting), unless parsing injected a default instrument preset: that
    // choice must stay visible in the DSL the user and the model read back.
    let injected = inner.project.presets.iter().any(|preset| {
        !source.contains(&format!("@preset {}", preset.name))
            && !source.contains(&format!("@kit {}", preset.name))
    });
    if !injected {
        inner.dsl_source = source;
    }
    let root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let snapshot = snapshot(&*inner);
    Ok((snapshot, root))
}
#[tauri::command]
pub fn add_track(
    name: String,
    instrument_category: String,
    instrument_name: String,
    preset: Option<String>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    // A preset named here saves the caller a second edit, and the graph rebuild
    // that a second edit costs.
    let mut track = new_track(&name, &instrument_category, &instrument_name);
    track.preset = preset.filter(|name| !name.trim().is_empty());
    inner.project.tracks.push(track);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("add track", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Add a plugin preset and the track that plays it as one document edit.
///
/// Saving the preset, adding the track and assigning the preset as separate
/// edits would rebuild the whole audio graph once per step. One rebuild is
/// enough and one undo entry describes the change.
///
/// The arguments mirror the panel's request fields; grouping them would change
/// the IPC payload.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn add_instrument_track(
    preset_name: String,
    source: String,
    plugin: Option<String>,
    program: Option<u8>,
    track_name: String,
    instrument_category: String,
    instrument_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    add_instrument_track_inner(
        &mut state.lock_inner(),
        preset_name,
        source,
        plugin,
        program,
        track_name,
        instrument_category,
        instrument_name,
    )
}

/// The command body without the Tauri wrapper. Shared with tests; the
/// arguments mirror the command payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_instrument_track_inner(
    inner: &mut AppStateInner,
    preset_name: String,
    source: String,
    plugin: Option<String>,
    program: Option<u8>,
    track_name: String,
    instrument_category: String,
    instrument_name: String,
) -> Result<ProjectSnapshot, String> {
    let track_name = track_name.trim().to_string();
    if track_name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    if inner.project.presets.iter().any(|p| p.name == preset_name) {
        return Err(format!("Preset '{preset_name}' already exists"));
    }
    let old_project = inner.project.clone();
    inner.project.presets.push(ast::PresetDef {
        name: preset_name.clone(),
        source: Some(source),
        plugin,
        program,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    });
    let mut track = new_track(&track_name, &instrument_category, &instrument_name);
    track.preset = Some(preset_name);
    inner.project.tracks.push(track);
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("add instrument track", UndoScope::Project),
    )?;
    Ok(snapshot(&*inner))
}

fn new_track(name: &str, instrument_category: &str, instrument_name: &str) -> Track {
    Track {
        name: name.to_string(),
        instrument: InstrumentPath::new(instrument_category, instrument_name),
        engine: None,
        preset: None,
        mix: MixState::default(),
        effects: vec![],
        phrases: vec![],
        patterns: vec![],
        inline_patterns: vec![],
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: vec![],
    }
}

/// Refuse a candidate that names media which is not on disk, or whose new
/// track has no renderable instrument. Shared by both tool-only create paths.
/// A bare instrument family gets its built-in default before the checks, so the
/// committed project carries the same visible preset the parser would assign.
fn check_candidate(
    inner: &AppStateInner,
    candidate: &mut Project,
    track_index: usize,
) -> Result<(), String> {
    oc_dsl::instruments::apply_defaults(candidate);
    if let Some(root) = project_root(inner) {
        let missing = oc_dsl::media::missing(candidate, &root);
        if !missing.is_empty() {
            return Err(missing.join("\n"));
        }
    }
    if let Some(reason) = oc_engine::playability::unplayable_reason(
        candidate,
        track_index,
        inner.audio.soundfont_path.as_deref(),
    ) {
        return Err(reason);
    }
    Ok(())
}

/// Tool path for `tracks_add`: the track is committed only when it can render.
pub fn add_track_checked(
    inner: &mut AppStateInner,
    name: &str,
    instrument_category: &str,
    instrument_name: &str,
    preset: Option<&str>,
) -> Result<ProjectSnapshot, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    let mut candidate = inner.project.clone();
    candidate
        .tracks
        .push(new_track(name, instrument_category, instrument_name));
    let index = candidate.tracks.len() - 1;
    candidate.tracks[index].preset = preset.map(str::to_string);
    check_candidate(inner, &mut candidate, index)?;

    let old_project = inner.project.clone();
    inner.project = candidate;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("add track", UndoScope::Project),
    )?;
    Ok(snapshot(inner))
}

/// Tool path for `tracks_create`: applies the `@track` block and the optional
/// preset, then commits only when the resulting track can render.
pub fn create_track_checked(
    inner: &mut AppStateInner,
    name: &str,
    instrument_category: &str,
    instrument_name: &str,
    preset: Option<&str>,
    dsl: &str,
) -> Result<ProjectSnapshot, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    let mut candidate = inner.project.clone();
    candidate
        .tracks
        .push(new_track(name, instrument_category, instrument_name));
    let index = candidate.tracks.len() - 1;
    let edit = oc_dsl::ai::AiEdit {
        new_dsl: dsl.to_string(),
        target_path: format!("track:{index}"),
        explanation: None,
    };
    let root = project_root(inner);
    let validation = oc_dsl::ai::validate_edit_in(&edit, &candidate, root.as_deref());
    if !validation.valid {
        return Err(validation.errors.join("\n"));
    }
    oc_dsl::ai::apply_edit_in(&mut candidate, &edit, root.as_deref())?;
    if let Some(preset) = preset {
        candidate.tracks[index].preset = Some(preset.to_string());
    }
    check_candidate(inner, &mut candidate, index)?;

    let old_project = inner.project.clone();
    inner.project = candidate;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("create track", UndoScope::Project),
    )?;
    Ok(snapshot(inner))
}

/// Duplicate a track; the copy gets its own preset so later edits stay local.
#[tauri::command]
pub fn duplicate_track(
    track_index: usize,
    name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    duplicate_track_inner(&mut state.lock_inner(), track_index, &name)
}

/// The track and its preset copy land in one undo entry.
pub(crate) fn duplicate_track_inner(
    inner: &mut crate::state::AppStateInner,
    track_index: usize,
    name: &str,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    duplicate_track_in_project(&mut inner.project, track_index, name)?;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("duplicate track", UndoScope::Project).with_scope(UndoScope::Preset),
    )?;
    Ok(snapshot(inner))
}

/// The copy shares the source's instrument and engine override, and owns an
/// independent preset; mix, clips, and automation are not copied.
pub(crate) fn duplicate_track_in_project(
    project: &mut ast::Project,
    track_index: usize,
    name: &str,
) -> Result<usize, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Track name must not be empty".into());
    }
    let source = project
        .tracks
        .get(track_index)
        .cloned()
        .ok_or_else(|| format!("Track {track_index} out of range"))?;
    let preset = clone_track_preset(project, track_index)?;
    project.tracks.push(Track {
        name: name.to_string(),
        instrument: source.instrument,
        engine: source.engine,
        preset,
        mix: MixState::default(),
        effects: vec![],
        phrases: vec![],
        patterns: vec![],
        inline_patterns: vec![],
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: source.channel,
        output_port: source.output_port,
        output_pair: source.output_pair,
        // Effects are not copied, so control routes into them must not be either.
        input_routes: vec![],
    });
    Ok(project.tracks.len() - 1)
}

#[tauri::command]
pub fn remove_track(track_index: usize, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    if track_index >= inner.project.tracks.len() {
        return Err(format!("Track {} out of range", track_index));
    }
    inner.project.tracks.remove(track_index);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("remove track", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
#[tauri::command]
pub fn set_tempo(tempo: f64, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    if !tempo.is_finite() || !(20.0..=999.0).contains(&tempo) {
        return Err("Tempo must be 20-999 BPM".into());
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.properties.tempo = tempo;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("tempo", UndoScope::Transport),
    )?;
    Ok(snapshot(&inner))
}

/// Replace the project's export choices as one undoable edit, so the `export:`
/// block shows the format the dialog last wrote and a reload keeps it.
#[tauri::command]
pub fn set_export_settings(
    settings: ExportSettings,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    oc_engine::encode::WavOptions {
        depth: settings.wav.depth,
        dither: settings.wav.dither,
        dither_seed: 0,
    }
    .validate()?;
    if settings.container == oc_dsl::ast::ExportContainer::Flac {
        oc_engine::encode::FlacOptions {
            depth: settings.flac.depth,
            level: settings.flac.level,
        }
        .validate()?;
    }
    if let Some(rate) = settings.sample_rate {
        oc_engine::encode::validate_sample_rate(rate)?;
    }
    if let Some(step) = settings.resample
        && step > 7
    {
        return Err(format!("resampling step {step} is outside 0-7"));
    }
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.properties.export = Some(settings);
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("export settings", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}

/// Replace the whole tempo map as one undoable edit. An empty map returns to
/// the project BPM.
#[tauri::command]
pub fn set_tempo_map(
    points: Vec<TempoPoint>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    set_tempo_map_inner(&mut state.lock_inner(), points)
}

pub(crate) fn set_tempo_map_inner(
    inner: &mut crate::state::AppStateInner,
    points: Vec<TempoPoint>,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    let mut project = inner.project.clone();
    apply_tempo_map(&mut project, points)?;
    inner.project = project;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("tempo map", UndoScope::Transport),
    )?;
    Ok(snapshot(inner))
}

pub(crate) fn apply_tempo_map(
    project: &mut ast::Project,
    points: Vec<TempoPoint>,
) -> Result<(), String> {
    let mut candidate = project.properties.clone();
    candidate.tempo_map = points.clone();
    oc_dsl::timing::ProjectClock::new(&candidate)
        .map_err(|error| format!("Invalid tempo map: {error}"))?;
    project.properties.tempo_map = points;
    Ok(())
}

/// Replace the whole meter map as one undoable edit. Arranged content keeps
/// its absolute musical time, as it does for a base-meter change.
#[tauri::command]
pub fn set_time_sig_map(
    points: Vec<TimeSigPoint>,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    set_time_sig_map_inner(&mut state.lock_inner(), points)
}

pub(crate) fn set_time_sig_map_inner(
    inner: &mut crate::state::AppStateInner,
    points: Vec<TimeSigPoint>,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    let mut project = inner.project.clone();
    apply_time_sig_map(&mut project, points)?;
    remap_content_positions(&mut project, &old_project.properties)?;
    inner.project = project;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("time signature map", UndoScope::Transport),
    )?;
    Ok(snapshot(inner))
}

pub(crate) fn apply_time_sig_map(
    project: &mut ast::Project,
    points: Vec<TimeSigPoint>,
) -> Result<(), String> {
    let mut candidate = project.properties.clone();
    candidate.time_sig_map = points.clone();
    oc_dsl::timing::ProjectClock::new(&candidate)
        .map_err(|error| format!("Invalid time signature map: {error}"))?;
    project.properties.time_sig_map = points;
    Ok(())
}

#[tauri::command]
pub fn set_time_signature(
    numerator: u8,
    denominator: u8,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    let mut project = inner.project.clone();
    set_project_time_signature(&mut project, numerator, denominator)?;
    remap_content_positions(&mut project, &old_project.properties)?;
    inner.project = project;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("time signature", UndoScope::Transport),
    )?;
    Ok(snapshot(&inner))
}

/// Set the project's base meter. A bar-1 meter-map point shadows the base meter,
/// so keep it in sync; later map points remain DSL-editable.
pub(crate) fn set_project_time_signature(
    project: &mut ast::Project,
    numerator: u8,
    denominator: u8,
) -> Result<(), String> {
    if numerator == 0 || numerator > 32 {
        return Err("Time signature numerator must be 1-32".into());
    }
    if !matches!(denominator, 1 | 2 | 4 | 8 | 16 | 32 | 64) {
        return Err("Time signature denominator must be 1, 2, 4, 8, 16, 32, or 64".into());
    }
    if let Some(point) = project
        .properties
        .time_sig_map
        .iter_mut()
        .find(|point| point.position.bar == 1)
    {
        point.numerator = numerator;
        point.denominator = denominator;
    }
    project.properties.time_signature = TimeSignature {
        numerator,
        denominator,
    };
    Ok(())
}

/// Keep arranged content at the same absolute musical time when the bar grid
/// changes. Meter/tempo/key map markers stay bar-anchored; only content moves.
pub(crate) fn remap_content_positions(
    project: &mut ast::Project,
    old_properties: &ast::ProjectProperties,
) -> Result<(), String> {
    if project.properties.time_signature == old_properties.time_signature
        && project.properties.time_sig_map == old_properties.time_sig_map
    {
        return Ok(());
    }
    let old_clock = oc_dsl::timing::ProjectClock::new(old_properties)?;
    let new_clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    for track in &mut project.tracks {
        for phrase in &mut track.phrases {
            if let Some(range) = &mut phrase.time_range {
                remap_time_range(range, &old_clock, &new_clock)?;
            }
        }
        for pattern in &mut track.patterns {
            if let Some(range) = &mut pattern.time_range {
                remap_time_range(range, &old_clock, &new_clock)?;
            }
        }
        for clip in track.pattern_clips_mut() {
            remap_time_range(&mut clip.time_range, &old_clock, &new_clock)?;
        }
        for lane in track.automation_mut() {
            for point in &mut lane.points {
                remap_bar_beat(&mut point.position, &old_clock, &new_clock)?;
            }
        }
    }
    for section in &mut project.sections {
        remap_time_range(&mut section.time_range, &old_clock, &new_clock)?;
    }
    for bus in &mut project.buses {
        for clip in &mut bus.gain_automation {
            remap_gain_automation(clip, &old_clock, &new_clock)?;
        }
        for lane in &mut bus.automation {
            for point in &mut lane.points {
                remap_bar_beat(&mut point.position, &old_clock, &new_clock)?;
            }
        }
    }
    if let Some(master) = &mut project.master {
        for clip in &mut master.gain_automation {
            remap_gain_automation(clip, &old_clock, &new_clock)?;
        }
        for lane in &mut master.automation {
            for point in &mut lane.points {
                remap_bar_beat(&mut point.position, &old_clock, &new_clock)?;
            }
        }
    }
    Ok(())
}

fn remap_time_range(
    range: &mut ast::TimeRange,
    old: &oc_dsl::timing::ProjectClock,
    new: &oc_dsl::timing::ProjectClock,
) -> Result<(), String> {
    remap_bar_beat(&mut range.start, old, new)?;
    remap_bar_beat(&mut range.end, old, new)
}

fn remap_bar_beat(
    position: &mut ast::BarBeat,
    old: &oc_dsl::timing::ProjectClock,
    new: &oc_dsl::timing::ProjectClock,
) -> Result<(), String> {
    let label = format!(
        "{}:{}:{}",
        position.bar, position.beat, position.subdivision
    );
    let quarter = old
        .quarter_notes_at(position)
        .map_err(|error| format!("Invalid position {label}: {error}"))?;
    *position = new
        .bar_beat_at_quarter(quarter)
        .map_err(|error| format!("Cannot remap position {label}: {error}"))?;
    Ok(())
}

fn remap_gain_automation(
    clip: &mut ast::GainAutomation,
    old: &oc_dsl::timing::ProjectClock,
    new: &oc_dsl::timing::ProjectClock,
) -> Result<(), String> {
    // Custom-PPQ gain points stay bar-anchored; the default PPQ-4 grid is remapped.
    if clip.ppq != 4 {
        return Ok(());
    }
    remap_time_range(&mut clip.time_range, old, new)?;
    for point in &mut clip.points {
        remap_bar_beat(&mut point.position, old, new)?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_project_name(name: String, state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.name = Some(name.clone());
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("project name", UndoScope::Project),
    )?;
    Ok(snapshot(&inner))
}
/// Undo state sent to frontend for display.
#[derive(Debug, Clone, Serialize)]
pub struct UndoState {
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Undo for the agent's `undo` tool, which may step back inside its own batch.
pub fn undo(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let label = inner.undo_step()?;
    log::info!("Undo: {}", label);
    inner.sync_history();
    Ok(snapshot(&inner))
}

/// Redo for the agent's `redo` tool.
pub fn redo(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    let label = inner.redo_step()?;
    log::info!("Redo: {}", label);
    inner.sync_history();
    Ok(snapshot(&inner))
}

/// Why a history step from the GUI did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRefusal {
    /// An agent turn is running or holds a batch on this project.
    AgentEditing,
    NothingToUndo,
    NothingToRedo,
}

/// The answer to an undo, a redo or a jump from the GUI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HistoryStep {
    Applied {
        snapshot: Box<ProjectSnapshot>,
        /// The entry the step undid or redid; absent for a jump.
        #[serde(skip_serializing_if = "Option::is_none")]
        brief: Option<crate::edit_history::Brief>,
    },
    Refused {
        reason: HistoryRefusal,
    },
}

/// The person's history steps wait while an agent edits the project: a turn
/// that is running, or a batch that is open, owns the project's history.
fn agent_editing(inner: &AppStateInner, requests: &crate::ai_service::Requests) -> bool {
    inner.transaction.is_some() || requests.active().ok().flatten().is_some()
}

#[derive(Clone, Copy)]
enum StepKind {
    Undo,
    Redo,
}

fn history_step(
    state: &AppState,
    requests: &crate::ai_service::Requests,
    kind: StepKind,
) -> Result<HistoryStep, String> {
    let mut inner = state.lock_inner();
    if agent_editing(&inner, requests) {
        return Ok(HistoryStep::Refused {
            reason: HistoryRefusal::AgentEditing,
        });
    }
    let brief = match kind {
        StepKind::Undo if !inner.undo.can_undo() => {
            return Ok(HistoryStep::Refused {
                reason: HistoryRefusal::NothingToUndo,
            });
        }
        StepKind::Redo if !inner.undo.can_redo() => {
            return Ok(HistoryStep::Refused {
                reason: HistoryRefusal::NothingToRedo,
            });
        }
        StepKind::Undo => {
            inner.undo_step()?;
            inner.undo.undone_brief(&inner.project.properties)
        }
        StepKind::Redo => {
            inner.redo_step()?;
            inner.undo.redone_brief(&inner.project.properties)
        }
    };
    inner.sync_history();
    Ok(HistoryStep::Applied {
        snapshot: Box::new(snapshot(&inner)),
        brief,
    })
}

/// Ctrl/Cmd+Z from the GUI.
#[tauri::command]
pub fn history_undo(
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<HistoryStep, String> {
    history_step(&state, &requests, StepKind::Undo)
}

/// Ctrl/Cmd+Shift+Z from the GUI.
#[tauri::command]
pub fn history_redo(
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<HistoryStep, String> {
    history_step(&state, &requests, StepKind::Redo)
}

/// A click on a History entry: move the cursor so `level` entries are applied.
#[tauri::command]
pub fn history_jump(
    level: usize,
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<HistoryStep, String> {
    let mut inner = state.lock_inner();
    if agent_editing(&inner, &requests) {
        return Ok(HistoryStep::Refused {
            reason: HistoryRefusal::AgentEditing,
        });
    }
    inner.restore_level(level)?;
    inner.sync_history();
    Ok(HistoryStep::Applied {
        snapshot: Box::new(snapshot(&inner)),
        brief: None,
    })
}

/// One page of the history, newest first: up to `limit` (at most 64) entries
/// below level `before`, or the newest ones.
#[tauri::command]
pub fn history_page(
    before: Option<usize>,
    limit: Option<usize>,
    state: State<AppState>,
) -> Result<crate::state::HistoryPage, String> {
    let mut inner = state.lock_inner();
    inner.sync_history();
    inner
        .undo
        .history_page(
            before,
            limit.unwrap_or(crate::state::HISTORY_PAGE),
            &inner.project.properties,
        )
        .map_err(String::from)
}

/// Where the history is kept and the space it occupies.
#[tauri::command]
pub fn history_info(state: State<AppState>) -> crate::state::HistoryInfo {
    let mut inner = state.lock_inner();
    inner.sync_history();
    inner.undo.history_info()
}

/// Drop all but the newest `keep` entries (never one that is undone).
#[tauri::command]
pub fn history_clear_older(
    keep: usize,
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<HistoryClearOutcome, String> {
    let mut inner = state.lock_inner();
    if agent_editing(&inner, &requests) {
        return Ok(HistoryClearOutcome::Refused {
            reason: HistoryRefusal::AgentEditing,
        });
    }
    clear_older_history(&mut inner, keep)
}

/// Drop older entries after making the journal current. A failed sync is
/// returned as its real error, so the panel can say why clearing did not run.
pub(crate) fn clear_older_history(
    inner: &mut AppStateInner,
    keep: usize,
) -> Result<HistoryClearOutcome, String> {
    if let Err(error) = inner.try_sync_history() {
        return Ok(HistoryClearOutcome::Failed {
            message: error.to_string(),
        });
    }
    let before = inner.undo.history_info().bytes;
    let dropped = match inner.undo.clear_older(keep) {
        Ok(dropped) => dropped,
        Err(error) => {
            return Ok(HistoryClearOutcome::Failed {
                message: error.to_string(),
            });
        }
    };
    let info = inner.undo.history_info();
    Ok(HistoryClearOutcome::Cleared {
        dropped,
        freed: before.saturating_sub(info.bytes),
        info,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HistoryClearOutcome {
    Cleared {
        dropped: usize,
        freed: u64,
        info: crate::state::HistoryInfo,
    },
    Refused {
        reason: HistoryRefusal,
    },
    /// The history could not be rewritten; the message names the write error.
    Failed {
        message: String,
    },
}

#[tauri::command]
pub fn get_undo_state(state: State<AppState>) -> UndoState {
    let inner = state.lock_inner();
    UndoState {
        can_undo: inner.undo.can_undo(),
        can_redo: inner.undo.can_redo(),
    }
}

/// Open a gesture: every edit until `end_undo_gesture` merges into one entry.
#[tauri::command]
pub fn begin_undo_gesture(scope: UndoScope, state: State<AppState>) {
    let mut inner = state.lock_inner();
    inner.undo.begin_gesture(scope);
}

/// Close one legacy nesting level without consuming owned tokens.
#[tauri::command]
pub fn end_undo_gesture(state: State<AppState>) {
    let mut inner = state.lock_inner();
    inner.undo.end_gesture();
}

/// Join the shared undo gesture with a close token scoped to this stack/document.
#[tauri::command]
pub fn begin_owned_undo_gesture(
    scope: UndoScope,
    state: State<AppState>,
) -> Result<crate::state::UndoGestureToken, String> {
    state
        .lock_inner()
        .undo
        .begin_owned_gesture(scope)
        .map_err(|error| error.to_string())
}

/// Admit a parameter interaction only against the topology it actually observed.
#[tauri::command]
pub fn begin_parameter_gesture(
    scope: UndoScope,
    expected_stamp: crate::state::ParameterTargetStamp,
    state: State<AppState>,
) -> Result<crate::state::UndoGestureToken, String> {
    let mut inner = state.lock_inner();
    super::shared::require_parameter_admission(&inner, &expected_stamp)?;
    inner
        .undo
        .begin_owned_gesture(scope)
        .map_err(|error| error.to_string())
}

/// A stale or repeated close is a harmless false; it never closes another owner.
#[tauri::command]
pub fn end_owned_undo_gesture(
    token: crate::state::UndoGestureToken,
    state: State<AppState>,
) -> bool {
    state.lock_inner().undo.end_owned_gesture(&token)
}

#[cfg(test)]
mod history_step_tests {
    use super::*;

    fn edited() -> AppState {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            let old = inner.project.clone();
            inner.project.properties.tempo = 133.0;
            resync_with_undo(
                &mut inner,
                old,
                UndoEdit::new("tempo", UndoScope::Transport),
            )
            .unwrap();
        }
        state
    }

    fn refused(step: HistoryStep) -> Option<HistoryRefusal> {
        match step {
            HistoryStep::Refused { reason } => Some(reason),
            HistoryStep::Applied { .. } => None,
        }
    }

    #[test]
    fn the_persons_undo_waits_while_an_agent_batch_is_open() {
        let state = edited();
        let requests = crate::ai_service::Requests::default();
        state
            .begin_transaction("turn".into(), crate::state::panel_batch_owner(Some("s")))
            .unwrap();
        for kind in [StepKind::Undo, StepKind::Redo] {
            let step = history_step(&state, &requests, kind).unwrap();
            assert_eq!(refused(step), Some(HistoryRefusal::AgentEditing));
        }
        assert_eq!(state.lock_inner().project.properties.tempo, 133.0);
        state.rollback_transaction().unwrap();
        let step = history_step(&state, &requests, StepKind::Undo).unwrap();
        let HistoryStep::Applied { brief, snapshot } = step else {
            panic!("the undo runs once the batch is gone");
        };
        assert_eq!(snapshot.project.properties.tempo, 120.0);
        let brief = brief.unwrap();
        assert_eq!(
            brief.changes.changes,
            vec![oc_dsl::change::Change::Tempo {
                from: 120.0,
                to: 133.0
            }]
        );
        assert_eq!(brief.cause, crate::edit_history::EditCause::Edit);
        assert_eq!(brief.author, crate::edit_history::Author::Person);
    }

    #[test]
    fn undo_and_redo_briefs_carry_the_same_typed_changes() {
        let state = edited();
        let requests = crate::ai_service::Requests::default();
        let HistoryStep::Applied { brief, .. } =
            history_step(&state, &requests, StepKind::Undo).unwrap()
        else {
            panic!("the undo runs");
        };
        let undo = brief.unwrap();
        assert_eq!(
            undo.changes.changes,
            vec![oc_dsl::change::Change::Tempo {
                from: 120.0,
                to: 133.0
            }]
        );
        let HistoryStep::Applied { brief, .. } =
            history_step(&state, &requests, StepKind::Redo).unwrap()
        else {
            panic!("the redo runs");
        };
        let redo = brief.unwrap();
        assert_eq!(redo.changes, undo.changes);
    }

    #[test]
    fn the_persons_undo_waits_while_a_turn_runs_without_a_batch() {
        let state = edited();
        let requests = crate::ai_service::Requests::default();
        let _claim = requests.claim("turn-1").unwrap().unwrap();
        let step = history_step(&state, &requests, StepKind::Undo).unwrap();
        assert_eq!(refused(step), Some(HistoryRefusal::AgentEditing));
        assert_eq!(state.lock_inner().project.properties.tempo, 133.0);
    }

    #[test]
    fn an_empty_history_answers_with_a_typed_refusal() {
        let state = AppState::new();
        let requests = crate::ai_service::Requests::default();
        let undo = history_step(&state, &requests, StepKind::Undo).unwrap();
        assert_eq!(refused(undo), Some(HistoryRefusal::NothingToUndo));
        let redo = history_step(&state, &requests, StepKind::Redo).unwrap();
        assert_eq!(refused(redo), Some(HistoryRefusal::NothingToRedo));
    }
}

#[cfg(test)]
mod project_load_order_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn owned_undo_late_close_cannot_close_a_replacement_documents_gesture() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        let stale = inner.undo.begin_owned_gesture(UndoScope::Project).unwrap();
        let replacement = oc_dsl::parse("@project \"Replacement\"\n").unwrap();
        replace_project_with(&mut inner, replacement, None, false, None, false).unwrap();
        assert_eq!(inner.project.name.as_deref(), Some("Replacement"));
        assert!(!inner.undo.is_in_gesture());
        let current = inner.undo.begin_owned_gesture(UndoScope::Preset).unwrap();
        assert_ne!(stale, current);
        assert!(!inner.undo.end_owned_gesture(&stale));
        assert!(inner.undo.is_in_gesture());
        assert!(inner.undo.end_owned_gesture(&current));
        assert!(!inner.undo.is_in_gesture());
    }

    #[test]
    fn project_load_order_rejects_late_open_new_and_recovery_commits() {
        for old_kind in ["open", "new", "recovery"] {
            let gate = Mutex::new(2);
            let mut project = "newest";
            let mut dirty = false;
            let result = commit_project_load(&gate, 1, || {
                project = old_kind;
                dirty = old_kind == "recovery";
                Ok::<(), String>(())
            });
            assert!(result.unwrap_err().contains("superseded"));
            assert_eq!(project, "newest");
            assert!(
                !dirty,
                "stale recovery must not mark the newer project dirty"
            );
            commit_project_load(&gate, 2, || {
                project = "accepted";
                Ok::<(), String>(())
            })
            .unwrap();
            assert_eq!(project, "accepted");
        }
    }

    #[test]
    fn project_load_order_serializes_acceptance_with_commit() {
        let gate = Arc::new(Mutex::new(1));
        let other = gate.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = commit_project_load(&gate, 1, || {
            let worker = std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                *other.lock().unwrap() = 2;
                done_tx.send(()).unwrap();
            });
            ready_rx.recv().unwrap();
            assert!(done_rx.try_recv().is_err());
            Ok::<_, String>(worker)
        })
        .unwrap();
        worker.join().unwrap();
        done_rx.recv().unwrap();
        assert!(commit_project_load(&gate, 1, || Ok::<(), String>(())).is_err());
        assert!(commit_project_load(&gate, 2, || Ok::<(), String>(())).is_ok());
    }
}
