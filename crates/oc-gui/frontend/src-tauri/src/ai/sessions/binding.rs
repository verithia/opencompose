//! A conversation's project binding: the file it edits, the notice the model
//! reads when that changes, and the comparison against the open project.

use super::super::host_notice::{BindCause, PendingBinding};
use super::{
    META_FILE, SessionMeta, Store, canonical_dir, canonical_path, hashed_key, is_valid_id, now,
    read_meta, write_meta,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

/// A project file in the canonical form every binding comparison uses. Every
/// value, including one read from disk, is canonicalized when it is built, so
/// two values are equal exactly when they name the same file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundProject(String);

impl BoundProject {
    pub fn of(path: &Path) -> Self {
        Self(canonical_path(path).to_string_lossy().into_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for BoundProject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BoundProject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::of(Path::new(&text)))
    }
}

/// The edit-history cursor preparation saves for the next turn: where the
/// bound project's history stood when this turn started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedHistoryMark {
    /// The project the mark belongs to.
    pub project: BoundProject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    pub max_id: u64,
}

impl SavedHistoryMark {
    pub(super) fn for_project(project: BoundProject, at: Option<u64>, max_id: u64) -> Self {
        Self {
            project,
            at,
            max_id,
        }
    }
}
/// The project a conversation is bound to and the one a prepared turn would
/// run on, both in canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingMismatch {
    pub bound: String,
    pub open: String,
}

impl BindingMismatch {
    /// The English refusal a surface without a localized message shows.
    pub fn message(&self) -> String {
        format!(
            "This conversation is bound to {}, but {} is open. Bind the conversation or start a \
             new one.",
            self.bound, self.open
        )
    }
}
/// Whether a saved conversation works on the open project, so the panel never
/// compares raw paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStatus {
    pub bound: Option<BoundProject>,
    pub open: Option<BoundProject>,
    pub matches: bool,
}

impl Store {
    pub fn bind(
        &self,
        id: &str,
        project: Option<&Path>,
        cause: BindCause,
    ) -> Result<SessionMeta, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let Some(project) = project else {
            return Err("Open a project before binding this conversation.".into());
        };
        let to = BoundProject::of(project);
        if meta.project_path.as_ref() == Some(&to) {
            return Ok(meta);
        }
        let from = meta.project_path.take().map(BoundProject::into_string);
        meta.project_path = Some(to);
        // The binding notice tells the model about the new file; a mark taken
        // on the old one would describe a different history.
        meta.history_mark = None;
        // A second bind before a turn keeps the path the model last saw, so the
        // one notice it reads names where the conversation started.
        if meta.pending_binding.is_none() {
            meta.pending_binding = Some(PendingBinding { from, cause });
        }
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    pub fn binding_status(&self, id: &str, open: Option<&Path>) -> Result<BindingStatus, String> {
        let (_, meta) = self.find(id)?;
        Ok(binding_status(&meta, open))
    }

    pub fn relocate_project(&self, from: &Path, to: &Path) -> Result<usize, String> {
        let _lock = self.mutation_lock()?;
        let from_project = BoundProject::of(from);
        let to_project = BoundProject::of(to);
        let from_folder = from.parent().map(hashed_key);
        let to_folder = to.parent();
        let mut moved = 0;
        for key in self.workspace_keys() {
            let Ok(entries) = std::fs::read_dir(self.root.join(&key)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !is_valid_id(&name) {
                    continue;
                }
                let mut dir = entry.path();
                let Some(mut meta) = read_meta(&dir.join(META_FILE)) else {
                    continue;
                };
                if meta.project_path.as_ref() != Some(&from_project) {
                    continue;
                }
                // Like an explicit bind, a move keeps the path the model last
                // saw, so an unsent binding still names where it started.
                if from_project != to_project && meta.pending_binding.is_none() {
                    meta.pending_binding = Some(PendingBinding {
                        from: Some(from_project.to_string()),
                        cause: BindCause::Moved,
                    });
                }
                meta.project_path = Some(to_project.clone());
                // The moved notice covers this boundary; the old history's
                // cursor does not describe the new file's history.
                meta.history_mark = None;
                if from_folder.as_deref() == Some(key.as_str())
                    && let Some(folder) = to_folder
                {
                    let target_key = hashed_key(folder);
                    if target_key != key {
                        self.ensure_workspace(&target_key, Some(folder), &now())?;
                        let target = self.root.join(&target_key).join(&meta.id);
                        std::fs::rename(&dir, &target)
                            .map_err(|error| format!("sessions: {error}"))?;
                        dir = target;
                        meta.workspace_key = target_key;
                        meta.workspace = Some(canonical_dir(folder).to_string_lossy().to_string());
                    }
                }
                write_meta(&dir, &meta)?;
                moved += 1;
            }
        }
        Ok(moved)
    }
}

pub fn binding_status(meta: &SessionMeta, open: Option<&Path>) -> BindingStatus {
    let bound = meta.project_path.clone();
    let open = open.map(BoundProject::of);
    let matches = bound
        .as_ref()
        .is_none_or(|bound| Some(bound) == open.as_ref());
    BindingStatus {
        bound,
        open,
        matches,
    }
}
