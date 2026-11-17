//! A conversation's project binding: the file it edits, the notice the model
//! reads when that changes, and the comparison against the open project.

use super::super::host_notice::{BindCause, PendingBinding};
use super::{
    META_FILE, SessionMeta, Store, canonical_dir, canonical_path, hashed_key, is_valid_id, now,
    read_meta, write_meta,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
/// The edit-history cursor preparation saves for the next turn: where the
/// bound project's history stood when this turn started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedHistoryMark {
    /// The canonical project path the mark belongs to.
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    pub max_id: u64,
}

impl SavedHistoryMark {
    /// A mark for `project`, in the canonical form both writers compare.
    pub(super) fn for_project(project: &str, at: Option<u64>, max_id: u64) -> Self {
        Self {
            project: canonical_path(Path::new(project))
                .to_string_lossy()
                .into_owned(),
            at,
            max_id,
        }
    }
}

pub(super) fn same_project(one: &str, other: &str) -> bool {
    canonical_path(Path::new(one)) == canonical_path(Path::new(other))
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
/// Whether a saved conversation works on the open project, compared in
/// canonical form so the panel never compares raw paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStatus {
    pub bound: Option<String>,
    pub open: Option<String>,
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
        let to = canonical_path(project).to_string_lossy().to_string();
        if meta.project_path.as_deref() == Some(to.as_str()) {
            return Ok(meta);
        }
        let from = meta.project_path.take();
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
        let from_text = canonical_path(from).to_string_lossy().to_string();
        let to_canonical = canonical_path(to);
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
                if meta.project_path.as_deref() != Some(from_text.as_str()) {
                    continue;
                }
                let to_text = to_canonical.to_string_lossy().to_string();
                // Like an explicit bind, a move keeps the path the model last
                // saw, so an unsent binding still names where it started.
                if from_text != to_text && meta.pending_binding.is_none() {
                    meta.pending_binding = Some(PendingBinding {
                        from: Some(from_text.clone()),
                        cause: BindCause::Moved,
                    });
                }
                meta.project_path = Some(to_text);
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
    let bound = meta.project_path.as_deref().map(|path| {
        canonical_path(Path::new(path))
            .to_string_lossy()
            .to_string()
    });
    let open = open.map(|path| canonical_path(path).to_string_lossy().to_string());
    let matches = bound
        .as_ref()
        .is_none_or(|bound| Some(bound) == open.as_ref());
    BindingStatus {
        bound,
        open,
        matches,
    }
}
