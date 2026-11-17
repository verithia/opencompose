//! Data the app keeps for projects and workspace folders that are gone.
//!
//! An item is **gone** when the file or folder it belongs to does not exist
//! on a reachable volume ([`super::is_gone`]); a file on a volume that is not
//! mounted is kept. A gone item is **due** once nothing touched it for
//! [`STALE_AFTER`]: the wait leaves time for a file moved outside the app to be
//! opened again and recognised by its content. The startup sweep removes what
//! is due; the Storage page lists every gone item and removes one on request.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ai::{project_instructions, sessions};
use crate::edit_history::journal;

/// How long a gone item waits before the sweep removes it.
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How long after startup the sweep runs, so it never competes with opening
/// the first project.
const SWEEP_DELAY: Duration = Duration::from_secs(60);

/// What one item of project data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    /// A project's edit history.
    History,
    /// A project's AI note.
    Note,
    /// A project's recovery copies.
    Recovery,
    /// The conversations of a workspace folder.
    Conversations,
}

/// One item of project data on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataItem {
    pub kind: DataKind,
    /// The store's key for the item: the path key, or a recovery directory.
    pub key: String,
    /// The project file, or for conversations the workspace folder; absent
    /// when the store does not record it.
    pub path: Option<String>,
    pub bytes: u64,
    /// Entries of a history, copies of a recovery directory, conversations of
    /// a workspace; 1 for a note.
    pub count: usize,
    /// When the item last changed, in ms since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touched_ms: Option<u64>,
    /// The file or folder it belongs to is gone.
    pub gone: bool,
    /// Gone and untouched for [`STALE_AFTER`]: the sweep removes it.
    pub due: bool,
}

fn ms(time: Option<SystemTime>) -> Option<u64> {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

struct Found {
    kind: DataKind,
    key: String,
    path: Option<PathBuf>,
    bytes: u64,
    count: usize,
    touched: Option<SystemTime>,
}

fn item(found: Found, now: SystemTime) -> DataItem {
    let Found {
        kind,
        key,
        path,
        bytes,
        count,
        touched,
    } = found;
    let gone = path.as_deref().is_some_and(super::is_gone);
    let idle = touched
        .and_then(|touched| now.duration_since(touched).ok())
        .unwrap_or_default();
    DataItem {
        kind,
        key,
        path: path.map(|path| path.display().to_string()),
        bytes,
        count,
        touched_ms: ms(touched),
        gone,
        due: gone && idle >= STALE_AFTER,
    }
}

/// Why a stored item could not be removed.
#[derive(Debug)]
pub enum DataRemoveError {
    /// No item of that kind and key is on disk.
    NotFound,
    /// The item's project or folder still exists.
    NotGone,
    /// The open project owns the item.
    Open,
    /// Another process holds the item's project.
    InUse,
    /// The item's store failed to remove it.
    Store(String),
}

impl std::fmt::Display for DataRemoveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("That project data is not on disk."),
            Self::NotGone => formatter
                .write_str("Only data whose project or folder is gone can be removed here."),
            Self::Open => formatter.write_str("This is the open project's data."),
            Self::InUse => {
                formatter.write_str("Another OpenCompose process is using this project's data.")
            }
            Self::Store(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DataRemoveError {}

impl From<DataRemoveError> for String {
    fn from(error: DataRemoveError) -> Self {
        error.to_string()
    }
}

/// Every item of project data the app keeps, gone or not.
pub fn inventory(now: SystemTime) -> Vec<DataItem> {
    let mut items = Vec::new();
    for stored in journal::list() {
        let key = stored
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        items.push(item(
            Found {
                kind: DataKind::History,
                key,
                path: Some(stored.marker.path.clone()),
                bytes: stored.bytes(),
                count: stored.entry_count(),
                touched: stored.touched(),
            },
            now,
        ));
    }
    for note in project_instructions::Store::config().notes() {
        items.push(item(
            Found {
                kind: DataKind::Note,
                key: note.key,
                path: note.path,
                bytes: note.bytes,
                count: 1,
                touched: note.modified,
            },
            now,
        ));
    }
    for copies in crate::autosave::stored(&crate::autosave::app_root()) {
        items.push(item(
            Found {
                kind: DataKind::Recovery,
                key: copies.key,
                path: copies.path,
                bytes: copies.bytes,
                count: copies.files,
                touched: copies.touched,
            },
            now,
        ));
    }
    for workspace in sessions::Store::config().workspaces_on_disk() {
        items.push(item(
            Found {
                kind: DataKind::Conversations,
                key: workspace.key,
                path: workspace.path,
                bytes: workspace.bytes,
                count: workspace.sessions,
                touched: workspace.touched,
            },
            now,
        ));
    }
    items
}

/// Remove one item. Only a gone item may go this way, and never the open
/// project's: its data is in use.
pub fn remove(
    kind: DataKind,
    key: &str,
    open: Option<&Path>,
    busy: &dyn Fn(&str) -> bool,
) -> Result<u64, DataRemoveError> {
    let found = inventory(SystemTime::now())
        .into_iter()
        .find(|item| item.kind == kind && item.key == key)
        .ok_or(DataRemoveError::NotFound)?;
    if !found.gone {
        return Err(DataRemoveError::NotGone);
    }
    if let (Some(open), Some(path)) = (open, found.path.as_deref())
        && super::canonical_path(open) == super::canonical_path(Path::new(path))
    {
        return Err(DataRemoveError::Open);
    }
    remove_item(&found, busy)?;
    Ok(found.bytes)
}

/// Take the lock of the project an item belongs to, so a sweep never deletes
/// data another process is using. Data without a project path is guarded by
/// its own store.
fn project_guard(
    item: &DataItem,
) -> Result<Option<crate::process_lock::ProcessLock>, DataRemoveError> {
    let path = match item.kind {
        DataKind::History | DataKind::Note | DataKind::Recovery => item.path.as_deref(),
        DataKind::Conversations => None,
    };
    let Some(path) = path else {
        return Ok(None);
    };
    match crate::process_lock::ProcessLock::acquire(super::project_lock_path(Path::new(path))) {
        Ok(lock) => Ok(Some(lock)),
        Err(error) if error.is_in_use() => Err(DataRemoveError::InUse),
        Err(error) => Err(DataRemoveError::Store(format!(
            "Cannot lock the project's data: {error}"
        ))),
    }
}

fn remove_item(item: &DataItem, busy: &dyn Fn(&str) -> bool) -> Result<(), DataRemoveError> {
    let _lock = project_guard(item)?;
    match item.kind {
        DataKind::History => {
            if !super::is_path_key(&item.key) {
                return Err(DataRemoveError::Store("not a history".into()));
            }
            let stored = journal::list()
                .into_iter()
                .find(|stored| {
                    stored
                        .dir
                        .file_name()
                        .is_some_and(|name| name == item.key.as_str())
                })
                .ok_or(DataRemoveError::NotFound)?;
            stored.remove().map_err(|error| {
                DataRemoveError::Store(format!("cannot remove the history: {error}"))
            })
        }
        DataKind::Note => project_instructions::Store::config()
            .remove_key(&item.key)
            .map_err(DataRemoveError::Store),
        DataKind::Recovery => {
            crate::autosave::remove_stored(&crate::autosave::app_root(), &item.key)
                .map_err(DataRemoveError::Store)
        }
        DataKind::Conversations => sessions::Store::config()
            .remove_workspace_guarded(&item.key, &|id| {
                if busy(id) {
                    Err(sessions::SessionError::new(
                        sessions::SessionErrorKind::Busy,
                        "A turn is running in one of these conversations.",
                    ))
                } else {
                    Ok(())
                }
            })
            .map(|_| ())
            .map_err(|error| DataRemoveError::Store(error.message)),
    }
}

/// Remove every due item except the open project's. Answers what went.
pub fn sweep(now: SystemTime, open: Option<&Path>, busy: &dyn Fn(&str) -> bool) -> Vec<DataItem> {
    let open = open.map(super::canonical_path);
    let mut removed = Vec::new();
    for item in inventory(now) {
        if !item.due {
            continue;
        }
        if let (Some(open), Some(path)) = (open.as_deref(), item.path.as_deref())
            && open == super::canonical_path(Path::new(path))
        {
            continue;
        }
        match remove_item(&item, busy) {
            Ok(()) => removed.push(item),
            Err(error) => log::warn!("[project data] stale {:?} {}: {error}", item.kind, item.key),
        }
    }
    // Recovery directories without a marker go on age alone.
    crate::autosave::sweep(&crate::autosave::app_root(), now);
    removed
}

/// Run the sweep once, a minute after startup, off the UI and audio threads.
pub fn spawn(app: tauri::AppHandle) {
    use tauri::Manager;
    let spawned = std::thread::Builder::new()
        .name("stale-sweep".into())
        .spawn(move || {
            std::thread::sleep(SWEEP_DELAY);
            let open = app
                .try_state::<crate::state::AppState>()
                .and_then(|state| state.lock_inner().file_path.clone());
            let requests = app.try_state::<crate::ai_service::Requests>();
            let busy = |id: &str| {
                requests
                    .as_ref()
                    .and_then(|requests| requests.session_busy(id).ok())
                    .unwrap_or(true)
            };
            let removed = sweep(SystemTime::now(), open.as_deref(), &busy);
            if !removed.is_empty() {
                let bytes: u64 = removed.iter().map(|item| item.bytes).sum();
                log::info!(
                    "[project data] removed {} stale item(s), {bytes} bytes",
                    removed.len()
                );
            }
        });
    if let Err(error) = spawned {
        log::warn!("stale sweep not started: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    struct Roots {
        _dir: tempfile::TempDir,
        _sessions: sessions::TestRootGuard,
        _notes: project_instructions::TestRootGuard,
        folder: PathBuf,
    }

    fn roots() -> Roots {
        let dir = tempfile::tempdir().unwrap();
        let sessions = sessions::test_root(dir.path().join("sessions"));
        let notes = project_instructions::test_root(&dir.path().join("notes"));
        let folder = dir.path().join("songs");
        std::fs::create_dir(&folder).unwrap();
        Roots {
            _dir: dir,
            _sessions: sessions,
            _notes: notes,
            folder,
        }
    }

    fn history_for(project: &Path) {
        journal::Marker::new(project, None)
            .write(&journal::dir_for(project))
            .unwrap();
    }

    fn kinds_of(items: &[DataItem], path: &Path) -> Vec<DataKind> {
        let text = path.display().to_string();
        let mut kinds: Vec<DataKind> = items
            .iter()
            .filter(|item| {
                item.path
                    .as_deref()
                    .is_some_and(|p| p.ends_with(&text) || text.ends_with(p))
            })
            .map(|item| item.kind)
            .collect();
        kinds.sort_by_key(|kind| *kind as u8);
        kinds
    }

    #[test]
    fn data_of_a_gone_project_waits_then_goes_and_a_live_one_stays() {
        let roots = roots();
        let live = roots.folder.join("live.ocp");
        std::fs::write(&live, "x").unwrap();
        let gone = roots.folder.join("gone.ocp");
        std::fs::write(&gone, "x").unwrap();
        for project in [&live, &gone] {
            history_for(project);
            project_instructions::Store::config()
                .save(project, "note")
                .unwrap();
        }
        std::fs::remove_file(&gone).unwrap();

        let now = SystemTime::now();
        let items = inventory(now);
        let gone_items: Vec<&DataItem> = items.iter().filter(|item| item.gone).collect();
        assert_eq!(gone_items.len(), 2, "{items:?}");
        assert!(
            gone_items.iter().all(|item| !item.due),
            "the wait has not passed"
        );
        assert!(sweep(now, None, &|_| false).is_empty());

        let removed = sweep(now + STALE_AFTER + DAY, None, &|_| false);
        assert_eq!(removed.len(), 2, "{removed:?}");
        let left = inventory(now);
        assert!(left.iter().all(|item| !item.gone));
        assert_eq!(kinds_of(&left, &live), [DataKind::History, DataKind::Note]);
    }

    #[test]
    fn a_project_on_an_unmounted_volume_and_the_open_project_are_kept() {
        let roots = roots();
        let unmounted = PathBuf::from("/Volumes/OcTestNotMounted/song.ocp");
        history_for(&unmounted);
        let open = roots.folder.join("open.ocp");
        history_for(&open);
        let later = SystemTime::now() + STALE_AFTER + DAY;
        assert!(sweep(later, Some(&open), &|_| false).is_empty());
        assert!(journal::dir_for(&unmounted).exists());
        assert!(journal::dir_for(&open).exists());
        assert!(
            sweep(later, None, &|_| false).len() == 1,
            "the open project only waits while open"
        );
    }

    #[test]
    fn conversations_of_a_gone_folder_go_with_it() {
        let roots = roots();
        let project = roots.folder.join("song.ocp");
        std::fs::write(&project, "x").unwrap();
        let store = sessions::Store::config();
        let meta = store
            .create(Some(&roots.folder), Some(&project), Some("Bass"), None)
            .unwrap();
        let later = SystemTime::now() + STALE_AFTER + DAY;
        assert!(
            sweep(later, None, &|_| false).is_empty(),
            "the folder is there"
        );
        std::fs::remove_dir_all(&roots.folder).unwrap();
        let removed = sweep(later, None, &|_| false);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].kind, DataKind::Conversations);
        assert!(store.find(&meta.id).is_err());
    }

    #[test]
    fn removing_by_hand_needs_a_gone_project() {
        let roots = roots();
        let project = roots.folder.join("song.ocp");
        std::fs::write(&project, "x").unwrap();
        history_for(&project);
        let key = super::super::path_key(&project);
        assert!(remove(DataKind::History, &key, None, &|_| false).is_err());
        std::fs::remove_file(&project).unwrap();
        assert!(
            remove(DataKind::History, &key, Some(&project), &|_| false).is_err(),
            "it is open"
        );
        assert!(remove(DataKind::History, &key, None, &|_| false).is_ok());
        assert!(!journal::dir_for(&project).exists());
    }

    #[test]
    fn data_another_process_holds_is_skipped_by_the_sweep() {
        let roots = roots();
        let gone = roots.folder.join("held.ocp");
        std::fs::write(&gone, "x").unwrap();
        history_for(&gone);
        std::fs::remove_file(&gone).unwrap();
        let lock_path = crate::project_data::project_lock_path(&gone);
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let foreign = crate::private_fs::lock(
            &lock_path,
            crate::private_fs::LockMode::Exclusive,
            crate::private_fs::LockWait::NoWait,
        )
        .unwrap();

        let later = SystemTime::now() + STALE_AFTER + DAY;
        assert!(sweep(later, None, &|_| false).is_empty());
        assert!(journal::dir_for(&gone).exists());

        drop(foreign);
        assert_eq!(sweep(later, None, &|_| false).len(), 1);
        assert!(!journal::dir_for(&gone).exists());
    }
}
