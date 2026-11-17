//! Content-addressed, bounded project versions attached at turn admission.

use super::{MessageKind, message_protocol};
use crate::private_fs;
use crate::state::AppStateInner;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// The edit-history position a person's message was sent from: the applied
/// level and the id of the entry at that level, in one project file. A recall
/// that undoes edits moves the cursor back here. Older rows carry none, which
/// reads as "no recorded position".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryAt {
    /// The canonical project path the position belongs to.
    pub project: String,
    /// Applied entries below the cursor when the message was sent.
    pub level: usize,
    /// The entry the cursor rested on; absent at the start of the history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// The largest id the history held here, so a later turn can tell a
    /// pruned history from a rewritten one.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_id: u64,
}

impl HistoryAt {
    /// The open document's position, when it has a file to name.
    pub fn of(inner: &AppStateInner) -> Option<Self> {
        let path = inner.file_path.as_deref()?;
        let mark = inner.undo.mark();
        Some(Self {
            project: super::canonical_path(path).to_string_lossy().into_owned(),
            level: inner.undo.applied_level(),
            at: mark.at,
            max_id: mark.max_id,
        })
    }

    /// Reads a saved row field, rejecting anything this shape cannot mean.
    pub fn from_saved(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let project = object
            .get("project")?
            .as_str()
            .filter(|path| !path.is_empty() && path.chars().count() <= super::MAX_PROJECT_PATH)?;
        let level = usize::try_from(object.get("level")?.as_u64()?).ok()?;
        let at = match object.get("at") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64()?),
        };
        let max_id = match object.get("max_id") {
            None | Some(Value::Null) => 0,
            Some(value) => value.as_u64()?,
        };
        Some(Self {
            project: project.to_owned(),
            level,
            at,
            max_id,
        })
    }
}

fn limit() -> usize {
    let limits = oc_dsl::source_map::SourceLimits::default().document;
    limits.max_encoded_bytes.min(limits.max_decoded_bytes)
}

fn digest(dsl: &str) -> String {
    format!("{:x}", Sha256::digest(dsl.as_bytes()))
}

pub(super) fn save(dir: &Path, dsl: &str) -> Result<String, String> {
    if dsl.len() > limit() {
        return Err("DSL snapshot exceeds the document byte limit".into());
    }
    let hash = digest(dsl);
    let versions = dir.join("versions");
    private_fs::ensure_dir(&versions).map_err(|error| format!("sessions: {error}"))?;
    let path = versions.join(format!("{hash}.ocp"));
    if path.exists() {
        // A corrupt content-addressed entry cannot become a valid checkpoint.
        read(dir, &hash)?;
    } else {
        private_fs::write_atomic(&path, dsl.as_bytes())
            .map_err(|error| format!("sessions: {error}"))?;
    }
    Ok(hash)
}

pub(super) fn read(dir: &Path, hash: &str) -> Result<String, String> {
    if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Not a DSL snapshot id".into());
    }
    let path = dir.join("versions").join(format!("{hash}.ocp"));
    let file = std::fs::File::open(&path).map_err(|error| format!("sessions: {error}"))?;
    if file.metadata().map_err(|error| error.to_string())?.len() > limit() as u64 {
        return Err("DSL snapshot exceeds the document byte limit".into());
    }
    let mut text = String::new();
    file.take(limit() as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("sessions: {error}"))?;
    if text.len() > limit() {
        return Err("DSL snapshot exceeds the document byte limit".into());
    }
    if digest(&text) != hash {
        return Err("The saved DSL version does not match its id".into());
    }
    Ok(text)
}

/// Only newly admitted human turns acquire a snapshot. Replayed history,
/// assistant replies and host deliveries keep their original boundary.
pub(super) fn attach(
    dir: &Path,
    messages: &mut [Value],
    snapshot: Option<&str>,
    project: Option<&Path>,
    history: Option<&HistoryAt>,
) -> Result<(), String> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    // The file the snapshot belongs to, so a revert never writes it into
    // another file of the same workspace.
    let project = project
        .map(|path| super::canonical_path(path).to_string_lossy().into_owned())
        .filter(|path| path.chars().count() <= super::MAX_PROJECT_PATH);
    let mut hash = None;
    for message in messages {
        if message_protocol::for_new(message).map(|meta| meta.kind) != Some(MessageKind::User) {
            continue;
        }
        let hash = match &hash {
            Some(hash) => hash,
            None => hash.insert(save(dir, snapshot)?),
        };
        message["dsl_sha"] = json!(hash);
        if let Some(project) = &project {
            message["dsl_project"] = json!(project);
        }
        if let Some(history) = history {
            message["history_at"] =
                serde_json::to_value(history).map_err(|error| format!("sessions: {error}"))?;
        }
    }
    Ok(())
}

pub(super) fn copy_references(
    parent: &Path,
    child: &Path,
    messages: &[Value],
) -> Result<(), String> {
    let mut copied = std::collections::HashSet::new();
    for hash in messages.iter().filter_map(|row| row["dsl_sha"].as_str()) {
        if copied.insert(hash) {
            let source = read(parent, hash)?;
            save(child, &source)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sessions::{PrepareContext, ProjectFacts, Store};

    #[test]
    fn native_turn_snapshots_survive_reload_without_rebinding_history() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, Some("Fixture"), None).unwrap();
        let facts = ProjectFacts {
            path: None,
            dsl_digest: "fixture".into(),
            track_count: 0,
        };
        let prepare = |messages: &[Value], dsl: &str| {
            store
                .prepare(
                    &meta.id,
                    messages,
                    None,
                    PrepareContext {
                        snapshot: Some(dsl),
                        history: None,
                        overhead: 0,
                        context_length: 1_000_000,
                        max_output: 0,
                        prompt_tokens: 0,
                        facts: &facts,
                        provider: "fixture",
                        model: "fixture",
                        outside: None,
                        surface: None,
                    },
                )
                .unwrap()
        };
        let first = prepare(
            &[json!({"role":"user","content":"Compose"})],
            "first document",
        );
        let hash = first.messages[0]["dsl_sha"].as_str().unwrap();
        assert_eq!(store.dsl_version(&meta.id, hash).unwrap(), "first document");
        let retried = prepare(&first.messages, "mutated document");
        assert_eq!(retried.messages[0]["dsl_sha"], hash);
        let mut next = retried.messages;
        next.push(json!({"role":"assistant","content":"Done"}));
        next.push(json!({"role":"user","content":"Continue","dsl_sha":"f".repeat(64)}));
        let second = prepare(&next, "second document");
        assert_eq!(second.messages[0]["dsl_sha"], hash);
        let new_hash = second.messages.last().unwrap()["dsl_sha"].as_str().unwrap();
        assert_ne!(hash, new_hash);
        let reopened = Store::new(store.root().to_owned());
        assert_eq!(
            reopened.dsl_version(&meta.id, new_hash).unwrap(),
            "second document"
        );
        assert_eq!(reopened.view(&meta.id).unwrap(), second.messages);
    }

    #[test]
    fn admission_writes_a_person_rows_history_position() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, Some("Fixture"), None).unwrap();
        let facts = ProjectFacts {
            path: None,
            dsl_digest: "fixture".into(),
            track_count: 0,
        };
        let history = HistoryAt {
            project: "/songs/song.ocp".into(),
            level: 4,
            at: Some(9),
            max_id: 9,
        };
        let context = || PrepareContext {
            snapshot: Some("first document"),
            history: Some(&history),
            overhead: 0,
            context_length: 1_000_000,
            max_output: 0,
            prompt_tokens: 0,
            facts: &facts,
            provider: "fixture",
            model: "fixture",
            outside: None,
            surface: None,
        };
        let prepared = store
            .prepare(
                &meta.id,
                &[json!({"role":"user","content":"Compose"})],
                None,
                context(),
            )
            .unwrap();
        let expected = serde_json::to_value(&history).unwrap();
        assert_eq!(prepared.messages[0]["history_at"], expected);
        // A replayed row keeps its own position; a later admission does not
        // rewrite the message that was already there.
        let replay = store
            .prepare(&meta.id, &prepared.messages, None, context())
            .unwrap();
        assert_eq!(replay.messages[0]["history_at"], expected);
    }

    #[test]
    fn host_context_has_no_human_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut messages = vec![
            json!({"role":"user","content":"Compose"}),
            json!({"role":"assistant","content":"Done"}),
            json!({"role":"tool","tool_call_id":"call","content":"{}"}),
            json!({"role":"user","context":"audition","content":"Clip"}),
            json!({"role":"user","message_meta":{"version":1,"kind":"round_budget"},"content":"Continue"}),
        ];
        attach(dir.path(), &mut messages, Some("document"), None, None).unwrap();
        assert!(messages[0].get("dsl_sha").is_some());
        assert!(messages[1..].iter().all(|row| row.get("dsl_sha").is_none()));
    }

    #[test]
    fn versions_refuse_corrupt_and_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let hash = save(dir.path(), "document").unwrap();
        let path = dir.path().join("versions").join(format!("{hash}.ocp"));
        std::fs::write(&path, "corrupt").unwrap();
        assert!(read(dir.path(), &hash).is_err());
        assert!(save(dir.path(), "document").is_err());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(limit() as u64 + 1)
            .unwrap();
        assert!(read(dir.path(), &hash).unwrap_err().contains("byte limit"));
        assert!(read(dir.path(), "../other").is_err());
    }

    #[test]
    fn failed_snapshot_does_not_admit_the_turn() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, None, None).unwrap();
        let (dir, _) = store.find(&meta.id).unwrap();
        std::fs::write(dir.join("versions"), "blocked").unwrap();
        let facts = ProjectFacts {
            path: None,
            dsl_digest: "fixture".into(),
            track_count: 0,
        };
        assert!(
            store
                .prepare(
                    &meta.id,
                    &[json!({"role":"user","content":"Compose"})],
                    None,
                    PrepareContext {
                        snapshot: Some("document"),
                        history: None,
                        overhead: 0,
                        context_length: 1_000_000,
                        max_output: 0,
                        prompt_tokens: 0,
                        facts: &facts,
                        provider: "fixture",
                        model: "fixture",
                        outside: None,
                        surface: None,
                    },
                )
                .is_err()
        );
        assert!(store.view(&meta.id).unwrap().is_empty());
    }

    #[test]
    fn a_fork_keeps_its_versions_after_the_parent_is_deleted() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, None, None).unwrap();
        let hash = store.snapshot_dsl(&meta.id, "parent document").unwrap();
        let facts = ProjectFacts {
            path: None,
            dsl_digest: "fixture".into(),
            track_count: 0,
        };
        store
            .prepare(
                &meta.id,
                &[json!({"role":"user","content":"Compose","dsl_sha":hash})],
                None,
                PrepareContext {
                    snapshot: None,
                    history: None,
                    overhead: 0,
                    context_length: 1_000_000,
                    max_output: 0,
                    prompt_tokens: 0,
                    facts: &facts,
                    provider: "fixture",
                    model: "fixture",
                    outside: None,
                    surface: None,
                },
            )
            .unwrap();
        let child = store
            .create(
                None,
                None,
                None,
                Some(super::super::Fork {
                    parent: &meta.id,
                    through: None,
                }),
            )
            .unwrap();
        store.delete(&meta.id).unwrap();
        assert_eq!(
            store.dsl_version(&child.id, &hash).unwrap(),
            "parent document"
        );
        let (dir, _) = store.find(&child.id).unwrap();
        std::fs::remove_file(dir.join("versions").join(format!("{hash}.ocp"))).unwrap();
        assert!(
            store
                .create(
                    None,
                    None,
                    None,
                    Some(super::super::Fork {
                        parent: &child.id,
                        through: None
                    })
                )
                .is_err()
        );
        assert_eq!(store.list(super::super::Scope::All).unwrap().len(), 1);
    }
}
