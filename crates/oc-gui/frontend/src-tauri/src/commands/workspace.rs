//! The workspace commands: which directory the operator works in, the project
//! files it holds, and the recent directories the start surface lists.
//!
//! The current workspace lives on [`AppStateInner`], not in the panel: closing
//! the sidebar or switching tabs unmounts the panel, and the scope must survive
//! that. Conversations are scoped by [`scoped_workspace`], which is the one
//! place that decides between the chosen directory, the open project's
//! directory, and `detached`.
use crate::commands::sessions;
use crate::state::{AppState, AppStateInner, WorkspaceChoice};
use crate::workspaces::{self, Registry, WorkspaceEntry};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

/// How many `.ocp` files one directory lists. A folder past this reports the
/// total it found, and the extra files stay reachable by path.
pub const MAX_PROJECTS: usize = 200;

/// The workspace, as the commands answer: the directory conversations are
/// scoped to, whether the operator chose to work without one, and the project
/// files to open. A view with no path and `detached: false` is the start
/// surface's state: nothing chosen yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceView {
    pub path: Option<String>,
    pub name: Option<String>,
    pub detached: bool,
    /// True when a directory was chosen, not merely adopted from the open file.
    pub chosen: bool,
    /// False when the directory is gone, so the chooser can say so.
    pub exists: bool,
    pub projects: Vec<WorkspaceProject>,
    /// The `.ocp` files the directory holds before [`MAX_PROJECTS`] cut the list.
    pub total_projects: usize,
}

/// One `.ocp` file in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceProject {
    pub name: String,
    pub path: String,
}

/// Recent workspaces, newest first.
#[tauri::command]
pub fn workspaces_list() -> Result<Vec<WorkspaceEntry>, String> {
    Registry::config().list()
}

/// Current workspace for the active project.
#[tauri::command]
pub fn workspace_current(state: State<AppState>) -> WorkspaceView {
    current_for(&state)
}

/// Adopts a directory, or the directory of a project file, as the workspace and
/// remembers it. The caller lists the answer's `projects` to open one.
#[tauri::command]
pub fn workspace_open(path: String, state: State<AppState>) -> Result<WorkspaceView, String> {
    open_for(&state, &path)
}

/// Continues without a workspace: conversations live under `detached` until a
/// project is opened, which scopes them to its directory as before.
#[tauri::command]
pub fn workspace_detached(state: State<AppState>) -> WorkspaceView {
    detached_for(&state)
}

/// Drops one directory from the recents and answers with the new list.
#[tauri::command]
pub fn workspace_forget(path: String) -> Result<Vec<WorkspaceEntry>, String> {
    let registry = Registry::config();
    registry.forget(Path::new(&path))?;
    registry.list()
}

/// The directory conversations are scoped to.
///
/// `requested` is a project a caller named explicitly: it names its own
/// workspace, because a request about one file is about that file's directory.
/// Without one, the chosen directory wins; without that, the open project's
/// directory; with neither there is no workspace, and conversations live under
/// `detached`.
pub fn scoped_workspace(inner: &AppStateInner, requested: Option<&Path>) -> Option<PathBuf> {
    if let Some(project) = requested {
        return sessions::workspace_of(Some(project));
    }
    match &inner.workspace {
        Some(WorkspaceChoice::Folder(dir)) => Some(dir.clone()),
        Some(WorkspaceChoice::Detached) | None => {
            sessions::workspace_of(inner.file_path.as_deref())
        }
    }
}

/// Current workspace for a project.
pub fn current_for(state: &AppState) -> WorkspaceView {
    let inner = state.lock_inner();
    view(&inner)
}

/// Open a workspace for a project.
pub fn open_for(state: &AppState, path: &str) -> Result<WorkspaceView, String> {
    let dir = adopt(path)?;
    // The recents are a convenience: a registry that cannot be written must not
    // stop the operator from working in the directory.
    if let Err(error) = Registry::config().remember(&dir, &crate::ai::sessions::now()) {
        log::warn!("[workspace] recents not updated: {error}");
    }
    let mut inner = state.lock_inner();
    inner.workspace = Some(WorkspaceChoice::Folder(dir));
    Ok(view(&inner))
}

/// Detached workspace for a project.
pub fn detached_for(state: &AppState) -> WorkspaceView {
    let mut inner = state.lock_inner();
    inner.workspace = Some(WorkspaceChoice::Detached);
    view(&inner)
}

/// The directory a workspace request names: the path itself, or the directory of
/// a project file. It must exist.
fn adopt(path: &str) -> Result<PathBuf, String> {
    let named = Path::new(path);
    let metadata = std::fs::metadata(named)
        .map_err(|error| format!("workspace {}: {error}", named.display()))?;
    let dir = if metadata.is_dir() {
        named.to_path_buf()
    } else {
        named
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("workspace {}: has no directory", named.display()))?
    };
    workspaces::canonical_dir(&dir)
}

fn view(inner: &AppStateInner) -> WorkspaceView {
    // The view is the workspace, not the open file: a chosen directory wins, and
    // only an unchosen one falls back to the project's, canonicalised so it compares.
    let path =
        scoped_workspace(inner, None).map(|path| workspaces::canonical_dir(&path).unwrap_or(path));
    let name = path.as_deref().map(workspaces::display_name);
    let (projects, total_projects) = path.as_deref().map(list_projects).unwrap_or_default();
    let exists = path.as_deref().is_some_and(Path::is_dir);
    WorkspaceView {
        exists,
        path: path.map(|path| path.to_string_lossy().to_string()),
        name,
        detached: matches!(inner.workspace, Some(WorkspaceChoice::Detached)),
        chosen: matches!(inner.workspace, Some(WorkspaceChoice::Folder(_))),
        projects,
        total_projects,
    }
}

/// The `.ocp` files in `dir`, by name, plus how many there were. A directory
/// that cannot be read lists nothing: a workspace whose folder is gone must not
/// fail the whole answer.
fn list_projects(dir: &Path) -> (Vec<WorkspaceProject>, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };
    let mut projects: Vec<WorkspaceProject> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ocp"))
        })
        .map(|entry| WorkspaceProject {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
        })
        .collect();
    projects.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    let total = projects.len();
    projects.truncate(MAX_PROJECTS);
    (projects, total)
}
