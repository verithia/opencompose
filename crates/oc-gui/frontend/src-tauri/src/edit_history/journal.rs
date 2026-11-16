//! The on-disk edit history of a project, kept in the app's own data folder:
//! `<config>/history/<key>/`, where the key hashes the project's path.
//!
//! - `project.json` is the [`Marker`]: the path the history belongs to and the
//!   SHA-256 of the project file as the app last read or wrote it. The path
//!   finds the history by default; the fingerprint finds it again when the
//!   file was moved or copied outside the app.
//! - `entries.jsonl` holds one [`StoredEntry`] per line. It only grows; an
//!   entry a redo branch or a merge replaced stays as garbage until the
//!   history is rewritten by [`Journal::clear_older`].
//! - `index.bin` lists the live linear history, oldest first, as fixed-size
//!   rows (id, offset, length). Dropping a redo branch truncates it.
//! - `state.json` holds the cursor (how many entries are applied) and the
//!   document text at the cursor, written together in one atomic replace, so
//!   an open can tell whether the file still matches the history.
//!
//! Only the index rows stay in memory; entries are read on demand.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{Author, EditCause};
use crate::state::UndoScopes;
use serde::{Deserialize, Serialize};

/// The folder under the app's data folder that holds every history.
pub const HISTORY_DIR: &str = "history";
const MARKER: &str = "project.json";
const MARKER_VERSION: u32 = 1;
const ENTRIES: &str = "entries.jsonl";
const INDEX: &str = "index.bin";
const STATE: &str = "state.json";
/// Bytes of one `index.bin` row.
pub const INDEX_ROW: usize = 24;
const ROW: usize = INDEX_ROW;
const STATE_VERSION: u32 = 1;

#[cfg(test)]
thread_local! {
    static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The folder every history lives under. A test thread gets its own, removed
/// when the thread exits.
pub fn root() -> PathBuf {
    #[cfg(test)]
    {
        TEST_ROOT.with(|root| {
            root.borrow_mut()
                .get_or_insert_with(|| oc_test_support::temp_dir("oc-history", HISTORY_DIR))
                .clone()
        })
    }
    #[cfg(not(test))]
    {
        crate::private_fs::config_dir().join(HISTORY_DIR)
    }
}

/// The key of `project`: the first 8 bytes of the SHA-256 of its canonical
/// path, in hex. A file that does not exist yet resolves through its folder,
/// so the key is the same before and after the first write.
pub fn key_for(project: &Path) -> String {
    crate::project_data::path_key(project)
}

/// The history directory of `project`.
pub fn dir_for(project: &Path) -> PathBuf {
    root().join(key_for(project))
}

/// What a history directory says about the project it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: u32,
    pub path: PathBuf,
    /// SHA-256 of the project file as the app last read or wrote it; absent
    /// until the file exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Marker {
    pub fn new(path: &Path, sha256: Option<String>) -> Self {
        Self {
            version: MARKER_VERSION,
            path: crate::project_data::canonical_path(path),
            sha256,
        }
    }
}

pub fn read_marker(dir: &Path) -> Option<Marker> {
    let bytes = std::fs::read(dir.join(MARKER)).ok()?;
    serde_json::from_slice::<Marker>(&bytes)
        .ok()
        .filter(|marker| marker.version == MARKER_VERSION)
}

pub fn write_marker(dir: &Path, marker: &Marker) -> io::Result<()> {
    crate::private_fs::ensure_dir(dir)?;
    let bytes = serde_json::to_vec_pretty(marker).map_err(io::Error::other)?;
    crate::private_fs::write_atomic(&dir.join(MARKER), &bytes)
}

/// The content SHA-256 the marker records; see [`crate::project_data`].
pub fn fingerprint(path: &Path) -> Option<String> {
    crate::project_data::content_sha256(path)
}

/// One history directory under [`root`], with its marker.
#[derive(Debug, Clone)]
pub struct Stored {
    pub dir: PathBuf,
    pub marker: Marker,
}

/// Every history directory that carries a readable marker.
pub fn list() -> Vec<Stored> {
    let Ok(read) = std::fs::read_dir(root()) else {
        return Vec::new();
    };
    read.flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(crate::project_data::is_path_key)
        })
        .filter_map(|entry| {
            let dir = entry.path();
            read_marker(&dir).map(|marker| Stored { dir, marker })
        })
        .collect()
}

/// A history whose recorded fingerprint is `sha256`, for a file the app has no
/// history for at its path. One whose project file is gone is preferred: that
/// file was moved, so its history follows it. Otherwise the newest copy wins.
pub fn find_by_fingerprint(sha256: &str, except: &Path) -> Option<Stored> {
    let mut matches: Vec<(Stored, bool, std::time::SystemTime)> = list()
        .into_iter()
        .filter(|stored| stored.dir != except)
        .filter(|stored| stored.marker.sha256.as_deref() == Some(sha256))
        .map(|stored| {
            let gone = !stored.marker.path.exists();
            let at = std::fs::metadata(stored.dir.join(STATE))
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (stored, gone, at)
        })
        .collect();
    matches.sort_by_key(|(_, gone, at)| (*gone, *at));
    matches.pop().map(|(stored, _, _)| stored)
}

/// Move a history directory to `to`, replacing a stale one there.
pub fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to)?;
    }
    std::fs::rename(from, to)
}

/// One history entry as stored. Patches are kept in their forward
/// orientation (older document to newer) whatever side of the cursor the
/// entry is on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEntry {
    pub id: u64,
    pub at_ms: u64,
    /// Why the entry exists when the patches cannot say it; omitted for an
    /// ordinary edit.
    #[serde(default, skip_serializing_if = "EditCause::is_edit")]
    pub cause: EditCause,
    pub author: Author,
    pub scopes: UndoScopes,
    #[serde(with = "b64")]
    pub forward: Vec<u8>,
    #[serde(with = "b64")]
    pub inverse: Vec<u8>,
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    id: u64,
    offset: u64,
    len: u64,
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    applied: usize,
    head: String,
}

/// An open history directory.
pub struct Journal {
    dir: PathBuf,
    rows: Vec<Row>,
    applied: usize,
    head: Option<String>,
    entries_len: u64,
}

impl Journal {
    /// Open or create the history of `project`. A torn tail (an index row
    /// pointing past the entries file) is dropped, and a cursor past the end
    /// is clamped, so a crash mid-write loses at most the unfinished write.
    pub fn open(project: &Path) -> io::Result<Self> {
        let dir = dir_for(project);
        crate::private_fs::ensure_dir(&dir)?;
        if read_marker(&dir).is_none() {
            write_marker(&dir, &Marker::new(project, None))?;
        }
        let entries_len = match std::fs::metadata(dir.join(ENTRIES)) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        let mut rows = Vec::new();
        match std::fs::read(dir.join(INDEX)) {
            Ok(bytes) => {
                for chunk in bytes.chunks_exact(ROW) {
                    let field = |at: usize| {
                        u64::from_le_bytes(chunk[at..at + 8].try_into().expect("8-byte field"))
                    };
                    let row = Row {
                        id: field(0),
                        offset: field(8),
                        len: field(16),
                    };
                    if row.offset.saturating_add(row.len) > entries_len {
                        break;
                    }
                    rows.push(row);
                }
                if bytes.len() != rows.len() * ROW {
                    truncate_index(&dir, rows.len())?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let (applied, head) = match std::fs::read(dir.join(STATE)) {
            Ok(bytes) => match serde_json::from_slice::<StateFile>(&bytes) {
                Ok(state) if state.version == STATE_VERSION => (state.applied, Some(state.head)),
                _ => (rows.len(), None),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => (rows.len(), None),
            Err(error) => return Err(error),
        };
        Ok(Self {
            dir,
            applied: applied.min(rows.len()),
            rows,
            head,
            entries_len,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Entries in the linear history, applied or not.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// How many entries, from the oldest, the document at the cursor has.
    pub fn applied(&self) -> usize {
        self.applied
    }

    /// The document text at the cursor, as the last [`Self::write_state`]
    /// recorded it. `None` for a history that never recorded one.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    pub fn id_at(&self, position: usize) -> Option<u64> {
        self.rows.get(position).map(|row| row.id)
    }

    /// The largest entry id the history holds, so new ids never repeat one.
    pub fn max_id(&self) -> u64 {
        self.rows.iter().map(|row| row.id).max().unwrap_or(0)
    }

    /// Read the entries at `range`, oldest first, with one open of the file.
    pub fn read_range(&self, range: std::ops::Range<usize>) -> io::Result<Vec<StoredEntry>> {
        let rows = self
            .rows
            .get(range)
            .ok_or_else(|| io::Error::other("history position out of range"))?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut file = File::open(self.dir.join(ENTRIES))?;
        rows.iter()
            .map(|row| {
                file.seek(SeekFrom::Start(row.offset))?;
                let mut bytes = vec![0; row.len as usize];
                file.read_exact(&mut bytes)?;
                let entry: StoredEntry = serde_json::from_slice(&bytes)
                    .map_err(|error| io::Error::other(format!("history entry: {error}")))?;
                if entry.id != row.id {
                    return Err(io::Error::other("history index does not match its entries"));
                }
                Ok(entry)
            })
            .collect()
    }

    /// Drop every entry from `len` on; their lines stay in the entries file.
    pub fn truncate(&mut self, len: usize) -> io::Result<()> {
        if len >= self.rows.len() {
            return Ok(());
        }
        truncate_index(&self.dir, len)?;
        self.rows.truncate(len);
        self.applied = self.applied.min(len);
        Ok(())
    }

    /// Append entries after the newest one, durable before this returns.
    pub fn append(&mut self, entries: &[StoredEntry]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut lines = Vec::new();
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
            rows.push(Row {
                id: entry.id,
                offset: self.entries_len + lines.len() as u64,
                len: line.len() as u64,
            });
            line.push(b'\n');
            lines.extend_from_slice(&line);
        }
        let mut file = open_append_private(&self.dir.join(ENTRIES))?;
        // The rows are written against the length the file had when opened; a
        // file that grew elsewhere would make every row point at the wrong line.
        if file.metadata()?.len() != self.entries_len {
            return Err(io::Error::other("history entries changed outside this app"));
        }
        file.write_all(&lines)?;
        file.sync_data()?;
        let mut index = open_append_private(&self.dir.join(INDEX))?;
        let mut bytes = Vec::with_capacity(rows.len() * ROW);
        for row in &rows {
            bytes.extend_from_slice(&row.id.to_le_bytes());
            bytes.extend_from_slice(&row.offset.to_le_bytes());
            bytes.extend_from_slice(&row.len.to_le_bytes());
        }
        index.write_all(&bytes)?;
        index.sync_data()?;
        self.entries_len += lines.len() as u64;
        self.rows.extend(rows);
        Ok(())
    }

    /// Record the cursor and the document at it in one atomic replace.
    pub fn write_state(&mut self, applied: usize, head: &str) -> io::Result<()> {
        let applied = applied.min(self.rows.len());
        if self.applied == applied && self.head.as_deref() == Some(head) {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&StateFile {
            version: STATE_VERSION,
            applied,
            head: head.to_owned(),
        })
        .map_err(io::Error::other)?;
        crate::private_fs::write_atomic(&self.dir.join(STATE), &bytes)?;
        self.applied = applied;
        self.head = Some(head.to_owned());
        Ok(())
    }

    /// Bytes the history directory occupies.
    pub fn bytes(&self) -> u64 {
        dir_bytes(&self.dir)
    }

    /// Keep only the newest `keep` entries, never dropping one above the
    /// cursor. The directory is rewritten beside itself and swapped in, so a
    /// failure leaves the old history whole. Returns how many were dropped.
    pub fn clear_older(&mut self, keep: usize) -> io::Result<usize> {
        let drop = self.rows.len().saturating_sub(keep).min(self.applied);
        if drop == 0 {
            return Ok(0);
        }
        let head = self.head.clone().unwrap_or_default();
        let staging = sibling(&self.dir, "new");
        let _ = std::fs::remove_dir_all(&staging);
        crate::private_fs::ensure_dir(&staging)?;
        let mut fresh = Journal {
            dir: staging.clone(),
            rows: Vec::new(),
            applied: 0,
            head: None,
            entries_len: 0,
        };
        let built = (|| {
            const PAGE: usize = 64;
            let mut at = drop;
            while at < self.rows.len() {
                let end = (at + PAGE).min(self.rows.len());
                fresh.append(&self.read_range(at..end)?)?;
                at = end;
            }
            if let Some(marker) = read_marker(&self.dir) {
                write_marker(&staging, &marker)?;
            }
            fresh.write_state(self.applied - drop, &head)
        })();
        if let Err(error) = built {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let retired = sibling(&self.dir, "old");
        let _ = std::fs::remove_dir_all(&retired);
        std::fs::rename(&self.dir, &retired)?;
        if let Err(error) = std::fs::rename(&staging, &self.dir) {
            let _ = std::fs::rename(&retired, &self.dir);
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(&retired);
        fresh.dir = self.dir.clone();
        *self = fresh;
        Ok(drop)
    }
}

/// An append handle on a private file, created 0600 on Unix like every file
/// under the app's data folder.
fn open_append_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn sibling(dir: &Path, tag: &str) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(format!(".{tag}"));
    PathBuf::from(name)
}

fn truncate_index(dir: &Path, rows: usize) -> io::Result<()> {
    let file = OpenOptions::new().write(true).open(dir.join(INDEX))?;
    file.set_len((rows * ROW) as u64)?;
    file.sync_data()
}

/// Total size of the files under `dir`, recursively.
pub fn dir_bytes(dir: &Path) -> u64 {
    crate::project_data::dir_bytes(dir)
}

/// Copy a history directory for Save As. The copy is assembled beside the
/// destination and renamed into place, replacing any history already there.
/// The copy keeps the source's marker until its owner rewrites it.
pub fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    let staging = sibling(to, "new");
    let _ = std::fs::remove_dir_all(&staging);
    crate::private_fs::ensure_dir(&staging)?;
    let copied = (|| {
        for name in [ENTRIES, INDEX, STATE, MARKER] {
            let source = from.join(name);
            if source.exists() {
                std::fs::copy(&source, staging.join(name))?;
                crate::private_fs::repair_file_mode(&staging.join(name));
            }
        }
        Ok::<_, io::Error>(())
    })();
    if let Err(error) = copied {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if to.exists() {
        std::fs::remove_dir_all(to)?;
    }
    std::fs::rename(&staging, to)
}
