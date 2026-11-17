//! Workspace keys, records and listings: the directory a conversation lives
//! under, how a moved folder re-associates, and the storage report.

use super::{
    DETACHED_KEY, MESSAGES_FILE, META_FILE, SessionError, SessionErrorKind, SessionMeta, Store,
    VERSION, WORKSPACE_FILE, is_valid_id, private_fs, read_meta,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WorkspaceRecord {
    v: u32,
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    created: String,
    updated: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Workspace(Option<&'a Path>),
    All,
}

#[derive(Debug, Clone)]
pub struct StoredWorkspace {
    pub key: String,
    /// The folder the conversations belong to; absent for `detached` and for a
    /// record that lost it.
    pub path: Option<PathBuf>,
    pub sessions: usize,
    pub bytes: u64,
    /// When a conversation in it last changed.
    pub touched: Option<std::time::SystemTime>,
}

pub(super) fn hashed_key(path: &Path) -> String {
    crate::project_data::path_key(path)
}

pub(super) fn is_valid_key(key: &str) -> bool {
    key == DETACHED_KEY || (key.len() == 16 && is_valid_id(key))
}

pub(super) fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Store {
    pub fn workspace_key(&self, workspace: Option<&Path>) -> String {
        let Some(path) = workspace else {
            return DETACHED_KEY.to_owned();
        };
        let key = hashed_key(path);
        if self.root.join(&key).is_dir() {
            return key;
        }
        let canonical = canonical_dir(path);
        self.recorded_key(&canonical).unwrap_or(key)
    }

    fn recorded_key(&self, canonical: &Path) -> Option<String> {
        let entries = std::fs::read_dir(&self.root).ok()?;
        for entry in entries.flatten().take(512) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_key(&name) {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path().join(WORKSPACE_FILE)) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<WorkspaceRecord>(&bytes) else {
                continue;
            };
            if record.paths.iter().any(|path| Path::new(path) == canonical) {
                return Some(name);
            }
        }
        None
    }

    fn ensure_root(&self) -> Result<(), String> {
        private_fs::ensure_dir(&self.root).map_err(|error| format!("sessions: {error}"))
    }

    pub(super) fn ensure_workspace(
        &self,
        key: &str,
        workspace: Option<&Path>,
        at: &str,
    ) -> Result<(), String> {
        self.ensure_root()?;
        let dir = self.root.join(key);
        private_fs::ensure_dir(&dir).map_err(|error| format!("sessions: {error}"))?;
        if key == DETACHED_KEY {
            return Ok(());
        }
        let file = dir.join(WORKSPACE_FILE);
        let existing = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WorkspaceRecord>(&bytes).ok());
        let mut record = existing.unwrap_or(WorkspaceRecord {
            v: VERSION,
            key: key.to_owned(),
            workspace: None,
            paths: Vec::new(),
            created: at.to_owned(),
            updated: at.to_owned(),
        });
        if let Some(workspace) = workspace {
            let text = canonical_dir(workspace).to_string_lossy().to_string();
            record.workspace.get_or_insert_with(|| text.clone());
            if !record.paths.contains(&text) {
                record.paths.push(text);
                record.paths.truncate(8);
            }
        }
        record.updated = at.to_owned();
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        private_fs::write_atomic(&file, &bytes).map_err(|error| format!("sessions: {error}"))
    }

    pub fn list(&self, scope: Scope<'_>) -> Result<Vec<SessionMeta>, String> {
        let keys = match scope {
            Scope::Workspace(Some(path)) => vec![self.workspace_key(Some(path))],
            Scope::Workspace(None) => vec![DETACHED_KEY.to_owned()],
            Scope::All => self.workspace_keys(),
        };
        let mut sessions = Vec::new();
        for key in keys {
            let Ok(entries) = std::fs::read_dir(self.root.join(&key)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !is_valid_id(&name) {
                    continue;
                }
                if let Some(meta) = read_meta(&entry.path().join(META_FILE)) {
                    sessions.push(meta);
                }
            }
        }
        let recorded = self.workspace_paths();
        for meta in &mut sessions {
            if let Some(path) = recorded.get(&meta.workspace_key) {
                meta.workspace = Some(path.clone());
            }
        }
        sessions.sort_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions)
    }

    pub(super) fn workspace_keys(&self) -> Vec<String> {
        self.lookup.workspaces(&self.root).keys
    }

    fn workspace_paths(&self) -> HashMap<String, String> {
        let mut paths = HashMap::new();
        for key in self.workspace_keys() {
            let Ok(bytes) = std::fs::read(self.root.join(&key).join(WORKSPACE_FILE)) else {
                continue;
            };
            if let Ok(record) = serde_json::from_slice::<WorkspaceRecord>(&bytes)
                && let Some(workspace) = record.workspace
            {
                paths.insert(key, workspace);
            }
        }
        paths
    }

    pub fn workspaces_on_disk(&self) -> Vec<StoredWorkspace> {
        let paths = self.workspace_paths();
        self.workspace_keys()
            .into_iter()
            .map(|key| {
                let dir = self.root.join(&key);
                let mut sessions = 0;
                let mut touched = std::fs::metadata(dir.join(WORKSPACE_FILE))
                    .and_then(|meta| meta.modified())
                    .ok();
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !is_valid_id(&name) {
                        continue;
                    }
                    sessions += 1;
                    let modified = std::fs::metadata(entry.path().join(MESSAGES_FILE))
                        .or_else(|_| std::fs::metadata(entry.path().join(META_FILE)))
                        .and_then(|meta| meta.modified())
                        .ok();
                    touched = touched.max(modified);
                }
                StoredWorkspace {
                    path: paths.get(&key).map(PathBuf::from),
                    bytes: crate::project_data::dir_bytes(&dir),
                    key,
                    sessions,
                    touched,
                }
            })
            .collect()
    }

    pub fn remove_workspace_guarded(
        &self,
        key: &str,
        guard: &dyn Fn(&str) -> Result<(), SessionError>,
    ) -> Result<u64, SessionError> {
        if !is_valid_key(key) || key == DETACHED_KEY {
            return Err("not a workspace of conversations".to_owned().into());
        }
        let _lock = self.mutation_lock()?;
        let dir = self.root.join(key);
        // Every conversation keeps its lock until the workspace directory is
        // gone. A conversation another process has open refuses the whole
        // removal instead of deleting it under that process.
        let mut locks = Vec::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_id(&name) {
                continue;
            }
            guard(&name)?;
            match crate::process_lock::ProcessLock::acquire(self.lock_path(&name)) {
                Ok(lock) => locks.push(lock),
                Err(error) if error.is_in_use() => {
                    return Err(SessionError::new(
                        SessionErrorKind::InUse,
                        "Another OpenCompose process has one of these conversations open.",
                    ));
                }
                Err(error) => return Err(format!("sessions: {error}").into()),
            }
        }
        let bytes = crate::project_data::dir_bytes(&dir);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("sessions: {error}").into()),
        }
        self.collect_unreferenced_locked();
        Ok(bytes)
    }
}
