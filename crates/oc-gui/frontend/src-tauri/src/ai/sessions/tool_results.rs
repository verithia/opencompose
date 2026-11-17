//! Full tool results behind a truncated preview, kept on disk beside the
//! conversation that produced them.
//!
//! The loop hands a recorded tool row its full text in the transient
//! `full_result` field. The append that saves the row writes that text to
//! `results/<sha256>.txt` under the store lock, before the transcript line, and
//! saves `result_sha` in its place. The row naming the hash is the file's owner
//! record: the sweep removes a result file once no saved line of its session
//! names it, so a reload, a compaction and a restart keep every result a
//! message still refers to, and nothing else.

use super::{MESSAGES_FILE, sweep};
use crate::ai::agent_loop::results::MAX_STORED_CHARS;
use crate::private_fs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

/// The folder of a session that holds its full results.
pub(super) const DIR: &str = "results";
/// The transient field that carries a full result into the append.
pub const FULL_RESULT: &str = "full_result";
/// The saved field that names a row's full result.
pub const RESULT_SHA: &str = "result_sha";

pub(super) fn is_hash(value: &str) -> bool {
    crate::project_data::is_sha256(value)
}

fn path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(DIR).join(format!("{hash}.txt"))
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Whether the file at `file` holds exactly the text `hash` names. The read is
/// bounded like [`read`]: a damaged file cannot grow memory, and one over the
/// stored bound is not reused. A file whose bytes changed under its name is not
/// reused either: the next write replaces it.
fn holds(file: &Path, hash: &str) -> bool {
    let Ok(file) = std::fs::File::open(file) else {
        return false;
    };
    let cap = (MAX_STORED_CHARS * 4) as u64;
    let mut bytes = Vec::new();
    if file.take(cap + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > cap {
        return false;
    }
    format!("{:x}", Sha256::digest(&bytes)) == hash
}

/// Writes the full result each message carries and swaps it for its hash. A
/// result over the stored bound is dropped: its preview already tells the
/// model to query narrower.
pub(super) fn archive(dir: &Path, messages: &mut [Value]) -> Result<(), String> {
    for message in messages {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        let Some(full) = object.remove(FULL_RESULT) else {
            continue;
        };
        let Some(text) = full.as_str() else { continue };
        if text.chars().count() > MAX_STORED_CHARS {
            continue;
        }
        let hash = digest(text);
        let file = path(dir, &hash);
        if !holds(&file, &hash) {
            private_fs::ensure_dir(&dir.join(DIR)).map_err(|error| format!("sessions: {error}"))?;
            private_fs::write_atomic(&file, text.as_bytes())
                .map_err(|error| format!("sessions: {error}"))?;
        }
        object.insert(RESULT_SHA.into(), json!(hash));
    }
    Ok(())
}

/// The full result a saved hash names. The hash is validated before it becomes
/// a file name, the read is bounded, so a damaged file cannot grow memory, and
/// the text must hash to its name: a file whose bytes changed reads as missing
/// rather than as the result the row recorded.
pub(super) fn read(dir: &Path, hash: &str) -> Result<Option<String>, String> {
    if !is_hash(hash) {
        return Ok(None);
    }
    let hash = hash.to_ascii_lowercase();
    let file = match std::fs::File::open(path(dir, &hash)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("sessions: {error}")),
    };
    // Four bytes per character is the widest UTF-8 encoding.
    let cap = (MAX_STORED_CHARS * 4) as u64;
    let mut text = String::new();
    file.take(cap + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("sessions: {error}"))?;
    if text.len() as u64 > cap || digest(&text) != hash {
        return Ok(None);
    }
    Ok(Some(text))
}

/// The call a saved result row answers: a native tool row by its
/// `tool_call_id`, a text-protocol result by the call id in its metadata.
#[cfg(test)]
fn answered(row: &Value) -> Option<&str> {
    row["tool_call_id"]
        .as_str()
        .or_else(|| row["message_meta"]["text_call"]["id"].as_str())
}

/// The hash of the newest result for `call` in the current view. A provider may
/// reuse a call id across rounds; the newest row is the one the model just saw.
/// A matching row without a stored hash answers "not stored" instead of falling
/// back to an older result, and turns a compaction or recall cut off the view
/// no longer answer at all.
#[cfg(test)]
pub(super) fn newest_for(dir: &Path, call: &str) -> Result<Option<String>, String> {
    let (view, _) = super::read_view(dir)?;
    let mut found = None;
    for row in &view {
        if answered(row) == Some(call) {
            found = row[RESULT_SHA]
                .as_str()
                .filter(|hash| is_hash(hash))
                .map(str::to_ascii_lowercase);
        }
    }
    Ok(found)
}

/// Copies the result files a fork's inherited rows name.
pub(super) fn copy_references(
    parent: &Path,
    child: &Path,
    messages: &[Value],
) -> Result<(), String> {
    for hash in messages
        .iter()
        .filter_map(|row| row[RESULT_SHA].as_str())
        .filter(|hash| is_hash(hash))
    {
        let hash = hash.to_ascii_lowercase();
        let target = path(child, &hash);
        if holds(&target, &hash) {
            continue;
        }
        if let Some(text) = read(parent, &hash)? {
            private_fs::ensure_dir(&child.join(DIR))
                .map_err(|error| format!("sessions: {error}"))?;
            private_fs::write_atomic(&target, text.as_bytes())
                .map_err(|error| format!("sessions: {error}"))?;
        }
    }
    Ok(())
}

/// Removes the result files no saved line of this session names. Results are
/// per session, so the session's own file is the whole owner set.
pub(super) fn collect(dir: &Path) -> u64 {
    let results = dir.join(DIR);
    if !results.is_dir() {
        return 0;
    }
    let Ok(file) = std::fs::File::open(dir.join(MESSAGES_FILE)) else {
        return 0;
    };
    let mut referenced = std::collections::HashSet::new();
    for line in std::io::BufReader::new(file).lines() {
        // An unreadable file keeps every result rather than guessing.
        let Ok(line) = line else { return 0 };
        referenced.extend(
            line.split(|c: char| !c.is_ascii_hexdigit())
                .filter(|token| token.len() == 64)
                .map(str::to_ascii_lowercase),
        );
    }
    let mut removed = 0;
    sweep(&results, &referenced, &mut removed);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sessions::{Fork, Store};

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
    }
    fn round(id: &str) -> Value {
        json!({"role":"assistant","content":null,"tool_calls":[{
            "id": id, "type":"function", "function":{"name":"project_get","arguments":"{}"}
        }]})
    }
    fn truncated(id: &str, full: &str) -> Value {
        json!({
            "role":"tool","tool_call_id":id,
            "content": json!({"result_truncated":true,"tool_call_id":id,"preview":"x"}).to_string(),
            FULL_RESULT: full,
        })
    }
    fn fixture() -> (tempfile::TempDir, Store, String) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, Some("Fixture"), None).unwrap();
        (root, store, meta.id)
    }
    fn files(store: &Store, id: &str) -> usize {
        let (dir, _) = store.find(id).unwrap();
        std::fs::read_dir(dir.join(DIR))
            .map(|it| it.count())
            .unwrap_or(0)
    }

    #[test]
    fn a_full_result_is_saved_by_hash_and_read_back_from_a_fresh_store() {
        let (root, store, id) = fixture();
        let saved = store
            .append_turn(
                &id,
                &[user("go"), round("c1"), truncated("c1", "FULL-ONE")],
                None,
            )
            .unwrap();
        let row = &saved[2];
        assert!(
            row.get(FULL_RESULT).is_none(),
            "the text never lands on the row"
        );
        assert!(is_hash(row[RESULT_SHA].as_str().unwrap()));
        // A new process has an empty cache; the store reads the file.
        let reopened = Store::new(root.path().join("sessions"));
        assert_eq!(
            reopened.tool_result(&id, "c1").unwrap().as_deref(),
            Some("FULL-ONE")
        );
        assert_eq!(reopened.tool_result(&id, "missing").unwrap(), None);
    }

    #[test]
    fn a_reused_call_id_reads_the_newest_result() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "OLD")], None)
            .unwrap();
        store
            .append_turn(&id, &[round("c"), truncated("c", "NEW")], None)
            .unwrap();
        assert_eq!(store.tool_result(&id, "c").unwrap().as_deref(), Some("NEW"));
        assert_eq!(files(&store, &id), 2);
    }

    #[test]
    fn a_reused_call_id_with_an_unstored_newer_result_reads_as_not_stored() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "OLD")], None)
            .unwrap();
        let huge = "y".repeat(MAX_STORED_CHARS + 1);
        store
            .append_turn(&id, &[round("c"), truncated("c", &huge)], None)
            .unwrap();
        assert_eq!(
            store.tool_result(&id, "c").unwrap(),
            None,
            "the newest row answers, and it kept no file"
        );
        assert_eq!(files(&store, &id), 1, "the older result file stays owned");
    }

    #[test]
    fn a_recall_cut_removes_a_result_the_file_still_holds() {
        let (_root, store, id) = fixture();
        let saved = store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "CUT")], None)
            .unwrap();
        assert!(saved[2][RESULT_SHA].as_str().is_some());
        let point = store.recall_point(&id, 0).unwrap();
        store.cut_before_user(&id, &point).unwrap();
        assert_eq!(files(&store, &id), 1, "the turn's line and its file remain");
        assert_eq!(store.tool_result(&id, "c").unwrap(), None);
    }

    #[test]
    fn a_file_over_the_stored_bound_is_not_held() {
        let (_root, store, id) = fixture();
        let (dir, _) = store.find(&id).unwrap();
        private_fs::ensure_dir(&dir.join(DIR)).unwrap();
        let text = "y".repeat(MAX_STORED_CHARS * 4 + 1);
        let hash = digest(&text);
        private_fs::write_atomic(&path(&dir, &hash), text.as_bytes()).unwrap();
        assert!(!holds(&path(&dir, &hash), &hash));
    }

    #[test]
    fn a_result_over_the_stored_bound_is_not_written() {
        let (_root, store, id) = fixture();
        let huge = "y".repeat(MAX_STORED_CHARS + 1);
        let saved = store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", &huge)], None)
            .unwrap();
        assert!(saved[2].get(RESULT_SHA).is_none());
        assert_eq!(files(&store, &id), 0);
    }

    #[test]
    fn a_hash_that_is_not_hex_never_becomes_a_path() {
        let (_root, store, id) = fixture();
        let (dir, _) = store.find(&id).unwrap();
        assert_eq!(read(&dir, "../../meta").unwrap(), None);
        assert_eq!(read(&dir, &"g".repeat(64)).unwrap(), None);
    }

    #[test]
    fn a_result_whose_bytes_changed_under_its_hash_reads_as_missing() {
        let (_root, store, id) = fixture();
        let saved = store
            .append_turn(
                &id,
                &[user("go"), round("c"), truncated("c", "ORIGINAL")],
                None,
            )
            .unwrap();
        let hash = saved[2][RESULT_SHA].as_str().unwrap().to_string();
        let (dir, _) = store.find(&id).unwrap();
        std::fs::write(path(&dir, &hash), "{\"rows\":[\"TAMPERED\"]}").unwrap();
        assert_eq!(store.tool_result(&id, "c").unwrap(), None);
        assert_eq!(read(&dir, &hash.to_ascii_uppercase()).unwrap(), None);
    }

    #[test]
    fn saving_the_same_result_again_replaces_a_file_whose_bytes_changed() {
        let (_root, store, id) = fixture();
        let saved = store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "SAME")], None)
            .unwrap();
        let hash = saved[2][RESULT_SHA].as_str().unwrap().to_string();
        let (dir, _) = store.find(&id).unwrap();
        std::fs::write(path(&dir, &hash), "TAMPERED").unwrap();
        store
            .append_turn(&id, &[round("d"), truncated("d", "SAME")], None)
            .unwrap();
        assert_eq!(
            store.tool_result(&id, "c").unwrap().as_deref(),
            Some("SAME")
        );
        assert_eq!(
            store.tool_result(&id, "d").unwrap().as_deref(),
            Some("SAME")
        );
    }

    #[test]
    fn a_fork_replaces_a_changed_copy_of_an_inherited_result() {
        let (_root, store, id) = fixture();
        let saved = store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "KEPT")], None)
            .unwrap();
        let hash = saved[2][RESULT_SHA].as_str().unwrap().to_string();
        let (parent, _) = store.find(&id).unwrap();
        let child = tempfile::tempdir().unwrap();
        private_fs::ensure_dir(&child.path().join(DIR)).unwrap();
        std::fs::write(path(child.path(), &hash), "TAMPERED").unwrap();
        copy_references(&parent, child.path(), &saved).unwrap();
        assert_eq!(read(child.path(), &hash).unwrap().as_deref(), Some("KEPT"));
    }

    #[test]
    fn a_fork_copies_the_results_its_rows_name_and_deleting_the_parent_keeps_them() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round("c"), truncated("c", "KEPT")], None)
            .unwrap();
        let child = store
            .create(
                None,
                None,
                Some("Fork"),
                Some(Fork {
                    parent: &id,
                    through: None,
                }),
            )
            .unwrap();
        store.delete_guarded(&id, &|| Ok(())).unwrap();
        assert_eq!(
            store.tool_result(&child.id, "c").unwrap().as_deref(),
            Some("KEPT")
        );
    }

    #[test]
    fn a_result_no_saved_line_names_is_collected() {
        let (_root, store, id) = fixture();
        store
            .append_turn(
                &id,
                &[user("go"), round("c"), truncated("c", "OWNED")],
                None,
            )
            .unwrap();
        let (dir, _) = store.find(&id).unwrap();
        // A write that crashed before its line was appended leaves an orphan.
        private_fs::write_atomic(&path(&dir, &"a".repeat(64)), b"ORPHAN").unwrap();
        assert_eq!(files(&store, &id), 2);
        assert_eq!(collect(&dir), 1);
        assert_eq!(
            store.tool_result(&id, "c").unwrap().as_deref(),
            Some("OWNED")
        );
    }
}
