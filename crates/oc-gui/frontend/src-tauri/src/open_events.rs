//! Durable delivery of OS-open requests to the one document window.
//!
//! Events are wakeups only. A request stays queued until the frontend answers
//! its unsaved-document guard and acknowledges it, including Cancel/failure.
use std::{collections::VecDeque, path::Path, sync::Mutex};

use serde::Serialize;
use tauri::{Emitter, Manager};

/// Event emitted when a project open is requested.
pub const PROJECT_OPEN_REQUESTED: &str = "project_open_requested";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// One pending project-open request.
pub struct ProjectOpenRequest {
    pub id: u64,
    pub path: String,
}

#[derive(Default)]
struct Pending {
    next_id: u64,
    requests: VecDeque<ProjectOpenRequest>,
}

#[derive(Default)]
/// FIFO of project-open requests.
pub struct OpenProjectRequests(Mutex<Pending>);

fn project_path(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("Project path must be absolute".into());
    }
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("ocp"))
    {
        return Err("Only .ocp project files can be opened".into());
    }
    // A missing file still reaches Open and its normal actionable error.
    let normalized = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    normalized
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Project path is not valid UTF-8".into())
}

/// Convert a file URL to a path string.
pub fn path_from_url(url: &tauri::Url) -> Result<String, String> {
    if url.scheme() != "file"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_some_and(|host| host != "localhost")
    {
        return Err("Open requires a local project file URL".into());
    }
    let path = url
        .to_file_path()
        .map_err(|_| "Open requires a local project file URL")?;
    project_path(&path)
}

impl OpenProjectRequests {
    /// Enqueue a project path; false when it is already pending.
    pub fn enqueue(&self, path: &Path) -> Result<bool, String> {
        let path = project_path(path)?;
        let mut pending = self.0.lock().map_err(|error| error.to_string())?;
        if pending.requests.iter().any(|request| request.path == path) {
            return Ok(false);
        }
        if pending.requests.len() >= 64 {
            return Err("Too many pending project opens".into());
        }
        pending.next_id += 1;
        let id = pending.next_id;
        pending.requests.push_back(ProjectOpenRequest { id, path });
        Ok(true)
    }

    fn peek(&self) -> Result<Option<ProjectOpenRequest>, String> {
        self.0
            .lock()
            .map(|pending| pending.requests.front().cloned())
            .map_err(|error| error.to_string())
    }

    fn acknowledge(&self, id: u64) -> Result<bool, String> {
        let mut pending = self.0.lock().map_err(|error| error.to_string())?;
        if pending
            .requests
            .front()
            .is_some_and(|request| request.id == id)
        {
            pending.requests.pop_front();
            return Ok(true);
        }
        Ok(false)
    }
}

#[tauri::command]
/// Read the oldest pending request.
pub fn peek_project_open_request(
    state: tauri::State<'_, OpenProjectRequests>,
) -> Result<Option<ProjectOpenRequest>, String> {
    state.peek()
}

#[tauri::command]
/// Acknowledge and remove a pending request.
pub fn acknowledge_project_open_request(
    id: u64,
    state: tauri::State<'_, OpenProjectRequests>,
) -> Result<bool, String> {
    state.acknowledge(id)
}

/// Focus the main window.
pub fn focus_main_window(app: &tauri::AppHandle) {
    if app
        .try_state::<crate::startup::RunMode>()
        .is_some_and(|mode| !mode.is_desktop())
    {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Queue a project open and notify the frontend.
pub fn queue_project(app: &tauri::AppHandle, path: &Path) {
    let Some(state) = app.try_state::<OpenProjectRequests>() else {
        return;
    };
    match state.enqueue(path) {
        Ok(true) => {
            let _ = app.emit(PROJECT_OPEN_REQUESTED, ());
        }
        Ok(false) => {}
        Err(error) => log::warn!("project open: {error}"),
    }
    focus_main_window(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join("opencompose-open-events-tests")
            .join(name)
    }

    #[test]
    fn cold_delivery_remains_until_acknowledged() {
        let state = OpenProjectRequests::default();
        assert!(state.enqueue(&path("a.ocp")).unwrap());
        let request = state.peek().unwrap().unwrap();
        assert_eq!(state.peek().unwrap(), Some(request.clone()));
        assert!(!state.acknowledge(request.id + 1).unwrap());
        assert!(state.acknowledge(request.id).unwrap());
        assert_eq!(state.peek().unwrap(), None);
    }

    #[test]
    fn duplicate_notifications_do_not_duplicate_pending_open() {
        let state = OpenProjectRequests::default();
        assert!(state.enqueue(&path("a.ocp")).unwrap());
        assert!(!state.enqueue(&path("a.ocp")).unwrap());
        assert!(state.enqueue(&path("b.ocp")).unwrap());
        let first = state.peek().unwrap().unwrap();
        state.acknowledge(first.id).unwrap();
        let second = state.peek().unwrap().unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(second.path, path("b.ocp").to_str().unwrap());
    }

    #[test]
    fn invalid_urls_and_nonprojects_are_rejected() {
        for value in [
            "https://example.test/a.ocp",
            "file:///music/a.ocp?query=1",
            "file:///music/a.ocp#fragment",
            "file://server/music/a.ocp",
        ] {
            assert!(path_from_url(&value.parse().unwrap()).is_err(), "{value}");
        }
        assert!(
            OpenProjectRequests::default()
                .enqueue(Path::new("relative.ocp"))
                .is_err()
        );
        assert!(
            OpenProjectRequests::default()
                .enqueue(&path("a.wav"))
                .is_err()
        );
    }

    #[test]
    fn local_urls_decode_spaces_and_accept_uppercase_extension() {
        let file = path("my song.OCP");
        let url = tauri::Url::from_file_path(&file).unwrap();
        assert_eq!(path_from_url(&url).unwrap(), file.to_str().unwrap());
    }

    #[test]
    fn consumed_path_may_be_requested_again_later() {
        let state = OpenProjectRequests::default();
        state.enqueue(&path("a.ocp")).unwrap();
        let first = state.peek().unwrap().unwrap();
        state.acknowledge(first.id).unwrap();
        state.enqueue(&path("a.ocp")).unwrap();
        assert!(state.peek().unwrap().unwrap().id > first.id);
    }
}
