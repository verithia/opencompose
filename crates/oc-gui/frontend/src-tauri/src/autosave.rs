//! Recovery copies of the open project, written under `~/.opencompose/recovery`.
//!
//! A copy is a whole project document, so restoring one goes through the normal
//! project-load path. Copies never touch the user's file or its directory:
//! every path below the recovery root belongs to this module.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Manager, State};

use crate::private_fs;
use crate::state::AppState;

/// Dirty time between recovery copies.
pub const INTERVAL: Duration = Duration::from_secs(60);
/// Copies kept per project; the oldest is removed beyond this bound.
pub const MAX_COPIES: usize = 10;
/// A recovery directory is swept once its newest copy is this old.
pub const ORPHAN_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Display name of a copy of a never-saved document.
const UNTITLED_NAME: &str = "untitled";
/// Directory the pre-identity build used for every unsaved document.
const LEGACY_UNTITLED_KEY: &str = "untitled";
/// Marker beside a recovery directory naming the project it belongs to.
const OWNER_SUFFIX: &str = ".owner";
/// A copy older than the saved file cannot be restored.
pub const OLDER_THAN_FILE: &str = "The saved project is newer than this copy.";
/// A copy whose bytes do not parse cannot be restored.
pub const UNREADABLE: &str = "This copy cannot be read.";
/// A copy written for another project is refused by name.
const WRONG_PROJECT: &str = "This copy belongs to a different project; it was not restored.";
/// Why a previous-version file whose bytes do not parse cannot be restored.
const BACKUP_UNREADABLE: &str = "The previous version cannot be read";

/// Root of the recovery store for the running app. Tests set
/// `OPENCOMPOSE_RECOVERY_DIR` so no test writes into the operator's home.
pub fn app_root() -> PathBuf {
    std::env::var_os("OPENCOMPOSE_RECOVERY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| private_fs::config_dir().join("recovery"))
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Kind of recovery copy.
pub enum RecoveryKind {
    /// A copy written by the autosave timer.
    Recovery,
    /// The content the last durable write replaced, kept beside the project.
    Backup,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
/// One recovery or backup copy.
pub struct RecoveryEntry {
    pub kind: RecoveryKind,
    pub path: String,
    pub project_name: String,
    /// Copy stamp in Unix milliseconds. It is monotonic per directory, so a
    /// backwards clock step cannot reorder copies.
    pub saved_at_ms: u64,
    pub size: u64,
    pub newer_than_disk: bool,
    /// Why Restore is disabled, or `None` when the copy can be restored.
    pub restore_disabled_reason: Option<String>,
}

/// A token identifying one running app session. Copies of a never-saved
/// document are keyed by it, so two open windows never share a directory.
pub fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!(
        "{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The canonical path, or an absolute path when the file is gone.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|dir| dir.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

/// The key a pre-identity build used: FNV-1a of the canonical path.
fn legacy_key(path: &Path) -> String {
    format!(
        "{:016x}",
        fnv1a(resolved(path).to_string_lossy().as_bytes())
    )
}

fn session_key(session: &str) -> String {
    format!("session-{:016x}", fnv1a(session.as_bytes()))
}

/// The identity key this platform offers, or `None` when it cannot be read.
fn file_identity(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(resolved(path)).ok()?;
    identity_of(&meta)
}

#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("id-{:016x}-{:016x}", meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn identity_of(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::windows::fs::MetadataExt;
    // The standard library exposes no file id on Windows. Creation time and
    // size survive a rename and change when a save replaces the file, which is
    // what the key needs.
    Some(format!(
        "id-{:016x}-{:016x}",
        meta.creation_time(),
        meta.file_size()
    ))
}

#[cfg(not(any(unix, windows)))]
fn identity_of(_meta: &std::fs::Metadata) -> Option<String> {
    None
}

/// Directory name for the document: its file identity where the platform
/// offers one, the legacy path hash otherwise, and the session for an unsaved
/// document.
fn path_key(project_path: Option<&Path>, session: &str) -> String {
    let Some(path) = project_path else {
        return session_key(session);
    };
    file_identity(path).unwrap_or_else(|| legacy_key(path))
}

/// Directories that may hold this document's copies: its identity key and the
/// legacy path key an older build wrote. An unsaved document also reads the
/// shared legacy `untitled` directory, whose copies no session can claim.
fn candidate_keys(project_path: Option<&Path>, session: &str) -> Vec<String> {
    let Some(path) = project_path else {
        return vec![session_key(session), LEGACY_UNTITLED_KEY.to_string()];
    };
    let identity = path_key(Some(path), session);
    let legacy = legacy_key(path);
    if identity == legacy {
        vec![identity]
    } else {
        vec![identity, legacy]
    }
}

fn file_name(saved_at_ms: u64, key: &str) -> String {
    format!("{saved_at_ms:013}-{key}.ocp")
}

/// `(saved_at_ms, key)` from a copy's file name.
fn parse_file_name(name: &str) -> Option<(u64, &str)> {
    let stem = name.strip_suffix(".ocp")?;
    let (stamp, key) = stem.split_once('-')?;
    Some((stamp.parse().ok()?, key))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Timestamp of the saved project, when it exists.
fn file_stamp(project_path: Option<&Path>) -> Option<u64> {
    let meta = std::fs::metadata(project_path?).ok()?;
    let modified = meta.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn digest(source: &str) -> u64 {
    fnv1a(source.as_bytes())
}

fn owner_path(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{key}{OWNER_SUFFIX}"))
}

fn read_owner(root: &Path, key: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(owner_path(root, key)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn write_owner(root: &Path, key: &str, project_path: &Path) {
    let _ = private_fs::write_atomic(
        &owner_path(root, key),
        resolved(project_path).to_string_lossy().as_bytes(),
    );
}

/// Keys whose owner marker names `project_path`. A save clears these too,
/// because an atomic replace gives the file a new identity key.
fn owned_keys(root: &Path, project_path: &Path) -> Vec<String> {
    let wanted = resolved(project_path);
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter_map(|name| name.strip_suffix(OWNER_SUFFIX).map(str::to_owned))
        .filter(|key| read_owner(root, key).is_some_and(|owner| owner == wanted))
        .collect()
}

/// The next copy stamp: above every existing stamp and above the wall clock,
/// so a backwards step cannot make a new copy older than an existing one.
fn next_stamp(directory: &Path, key: &str, wall_ms: u64) -> u64 {
    let newest = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let (stamp, name_key) = parse_file_name(name.to_str()?)?;
            (name_key == key).then_some(stamp)
        })
        .max()
        .unwrap_or(0);
    newest.saturating_add(1).max(wall_ms)
}

/// Keep the newest `MAX_COPIES` copies for `key`, removing the rest.
fn prune(directory: &Path, key: &str) {
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return;
    };
    let mut copies: Vec<(u64, PathBuf)> = read_dir
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name();
            let (stamp, name_key) = parse_file_name(name.to_str()?)?;
            (name_key == key).then_some((stamp, entry.path()))
        })
        .collect();
    copies.sort_by_key(|(stamp, _)| *stamp);
    let remove = copies.len().saturating_sub(MAX_COPIES);
    for (_, path) in copies.into_iter().take(remove) {
        let _ = std::fs::remove_file(path);
    }
}

/// Write one recovery copy and enforce the retention bound. The write is
/// atomic: a failure leaves no partial copy and keeps earlier copies intact.
pub fn write_copy(
    root: &Path,
    project_path: Option<&Path>,
    project_name: Option<&str>,
    session: &str,
    source: &str,
    saved_at_ms: u64,
) -> Result<RecoveryEntry, String> {
    let key = path_key(project_path, session);
    let directory = root.join(&key);
    if let Some(path) = project_path {
        write_owner(root, &key, path);
    }
    let saved_at_ms = next_stamp(&directory, &key, saved_at_ms);
    let path = directory.join(file_name(saved_at_ms, &key));
    private_fs::write_atomic(&path, source.as_bytes())
        .map_err(|error| format!("Could not write the recovery copy: {error}"))?;
    prune(&directory, &key);
    Ok(RecoveryEntry {
        kind: RecoveryKind::Recovery,
        path: path.display().to_string(),
        project_name: project_name.unwrap_or(UNTITLED_NAME).to_string(),
        saved_at_ms,
        size: source.len() as u64,
        newer_than_disk: true,
        restore_disabled_reason: None,
    })
}

/// Every recovery copy of `project_path`, newest first. A missing directory is
/// an empty list, not an error.
pub fn list_copies(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
) -> Result<Vec<RecoveryEntry>, String> {
    let keys = candidate_keys(project_path, session);
    if let Some(path) = project_path {
        // A rename leaves the identity directory in place; refresh the marker
        // so the sweep does not later mistake a live project for an orphan.
        for key in &keys {
            if root.join(key).is_dir() {
                write_owner(root, key, path);
            }
        }
    }
    sweep_except(root, SystemTime::now(), &keys);
    let stamp = file_stamp(project_path);
    let mut entries = Vec::new();
    for key in &keys {
        let directory = root.join(key);
        let read_dir = match std::fs::read_dir(&directory) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Could not list recovery copies: {error}")),
        };
        for entry in read_dir.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry.file_name();
            let Some((saved_at_ms, name_key)) = name.to_str().and_then(parse_file_name) else {
                continue;
            };
            if name_key != key {
                continue;
            }
            let path = entry.path();
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            let source = std::fs::read_to_string(&path).ok();
            let parsed = source.as_deref().and_then(|text| oc_dsl::parse(text).ok());
            let newer_than_disk = stamp.is_none_or(|stamp| saved_at_ms > stamp);
            let project_name = parsed
                .as_ref()
                .and_then(|project| project.name.clone())
                .unwrap_or_else(|| name.to_string_lossy().into_owned());
            let restore_disabled_reason = if source.is_none() || parsed.is_none() {
                Some(UNREADABLE.to_string())
            } else if !newer_than_disk {
                Some(OLDER_THAN_FILE.to_string())
            } else {
                None
            };
            entries.push(RecoveryEntry {
                kind: RecoveryKind::Recovery,
                path: path.display().to_string(),
                project_name,
                saved_at_ms,
                size,
                newer_than_disk,
                restore_disabled_reason,
            });
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.saved_at_ms));
    Ok(entries)
}

/// The content the last durable write replaced, as a recovery entry. `None`
/// when the project has no path or keeps no previous version.
pub fn backup_entry(project_path: Option<&Path>) -> Option<RecoveryEntry> {
    let project_path = project_path?;
    let path = crate::project_persist::backup_path(project_path);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let saved_at_ms = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    let source = std::fs::read_to_string(&path).ok();
    let parsed = source.as_deref().and_then(|text| oc_dsl::parse(text).ok());
    let project_name = parsed
        .as_ref()
        .and_then(|project| project.name.clone())
        .or_else(|| {
            project_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| UNTITLED_NAME.to_string());
    let restore_disabled_reason = match source {
        // A backup that cannot be read is still listed: ignoring it would hide
        // the only copy of the previous version.
        None => Some(format!("{BACKUP_UNREADABLE}: the file could not be read.")),
        Some(text) => oc_dsl::parse(&text)
            .err()
            .map(|error| format!("{BACKUP_UNREADABLE}: {error}")),
    };
    Some(RecoveryEntry {
        kind: RecoveryKind::Backup,
        path: path.display().to_string(),
        project_name,
        saved_at_ms,
        size: meta.len(),
        newer_than_disk: false,
        restore_disabled_reason,
    })
}

/// Every copy of `project_path` plus its previous version, newest first.
pub fn list_entries(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
) -> Result<Vec<RecoveryEntry>, String> {
    let mut entries = list_copies(root, project_path, session)?;
    entries.extend(backup_entry(project_path));
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.saved_at_ms));
    Ok(entries)
}

/// The newest modification time of a recovery directory's copies. `None` when
/// the directory cannot be read, so the sweep keeps it rather than guess.
fn newest_copy(directory: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(directory)
        .and_then(|meta| meta.modified())
        .ok();
    for entry in std::fs::read_dir(directory).ok()?.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if newest.is_none_or(|current| modified > current) {
            newest = Some(modified);
        }
    }
    newest
}

/// Remove recovery directories no live project owns. A directory becomes a
/// candidate once its newest copy is older than [`ORPHAN_AGE`]; it goes when
/// its owner marker names a file that is gone, or one that is neither this
/// directory's identity nor its legacy key. A directory without a marker (a
/// legacy or crashed-session directory) goes on age alone. Returns the removed
/// directories.
#[cfg(test)]
pub fn sweep(root: &Path, now: SystemTime) -> Vec<PathBuf> {
    sweep_except(root, now, &[])
}

fn sweep_except(root: &Path, now: SystemTime, keep: &[String]) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in read_dir.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if keep.contains(&key) {
            continue;
        }
        let Some(newest) = newest_copy(&entry.path()) else {
            continue;
        };
        if now.duration_since(newest).unwrap_or_default() < ORPHAN_AGE {
            continue;
        }
        let orphan = match read_owner(root, &key) {
            Some(owner) if owner.exists() => match file_identity(&owner) {
                // A live project keeps its identity directory and the path-hash
                // directory an older build wrote for it.
                Some(identity) => identity != key && legacy_key(&owner) != key,
                // Present but unreadable: keep rather than guess.
                None => false,
            },
            Some(_) => true,
            None => true,
        };
        if orphan && std::fs::remove_dir_all(entry.path()).is_ok() {
            let _ = std::fs::remove_file(owner_path(root, &key));
            removed.push(entry.path());
        }
    }
    removed
}

fn check_copy<'a>(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
    path: &'a Path,
) -> Result<&'a Path, String> {
    let keys = candidate_keys(project_path, session);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(WRONG_PROJECT)?;
    let matches = keys.iter().any(|key| {
        let directory = root.join(key);
        path.parent() == Some(directory.as_path())
            && parse_file_name(name).is_some_and(|(_, name_key)| name_key == key)
    });
    if !matches {
        return Err(WRONG_PROJECT.to_string());
    }
    Ok(path)
}

/// Parse one copy without touching the open project. The copy is kept.
#[cfg(test)]
pub fn load_copy(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
    path: &Path,
) -> Result<oc_dsl::ast::Project, String> {
    let path = check_copy(root, project_path, session, path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("copy");
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("Could not restore {name}: {error}. The backup is kept."))?;
    oc_dsl::parse(&source)
        .map_err(|error| format!("Could not restore {name}: {error}. The backup is kept."))
}

/// What a restore reads from the live document, captured under the lock.
///
/// The slow half later writes the safety copy and parses the copy from these
/// fields, so it needs neither the document lock nor the runtime thread.
struct RestoreRequest {
    root: PathBuf,
    /// The project file the open document belongs to; `None` when untitled.
    project_path: Option<PathBuf>,
    project_name: Option<String>,
    session: String,
    /// The document the safety copy keeps before a previous version replaces it.
    dsl_source: String,
    /// Revision of the document captured here; an edit during the load refuses
    /// the replacement instead of dropping it.
    revision: u64,
    /// The copy to load.
    path: PathBuf,
    /// True when the copy is the project's previous version, which needs a
    /// safety copy of the current document first.
    previous: bool,
}

/// Capture what a restore needs without touching the filesystem.
fn restore_request(
    inner: &crate::state::AppStateInner,
    root: &Path,
    path: &Path,
) -> Result<RestoreRequest, String> {
    let project_path = inner.file_path.clone();
    let previous = project_path
        .as_deref()
        .is_some_and(|project| crate::project_persist::backup_path(project) == path);
    if !previous {
        // Refuse a path outside this project's recovery copies before reading it.
        check_copy(root, project_path.as_deref(), &inner.session_id, path)?;
    }
    Ok(RestoreRequest {
        root: root.to_path_buf(),
        project_path,
        project_name: inner.project.name.clone(),
        session: inner.session_id.clone(),
        dsl_source: inner.dsl_source.clone(),
        revision: inner.revision,
        path: path.to_path_buf(),
        previous,
    })
}

/// Load the copy a request names and keep the current document first.
///
/// This is the slow half: a safety copy with fsync, then a read and a parse. It
/// runs without the document lock, so a restore does not stall the editor,
/// the meters or the transport. The caller re-checks the revision before it
/// replaces the document.
fn load_restore(request: &RestoreRequest) -> Result<oc_dsl::ast::Project, String> {
    let name = request
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("copy");
    if request.previous {
        let project_path = request
            .project_path
            .as_deref()
            .ok_or_else(|| "the previous version has no project file".to_string())?;
        write_copy(
            &request.root,
            Some(project_path),
            request.project_name.as_deref(),
            &request.session,
            &request.dsl_source,
            now_ms(),
        )
        .map_err(|error| {
            let reason =
                format!("Could not keep the current project before restoring {name}: {error}");
            log::error!("autosave: {reason}");
            reason
        })?;
        let source = std::fs::read_to_string(&request.path)
            .map_err(|error| format!("Could not restore {name}: {error}. The file is kept."))?;
        return oc_dsl::parse(&source)
            .map_err(|error| format!("Could not restore {name}: {error}. The file is kept."));
    }
    let source = std::fs::read_to_string(&request.path)
        .map_err(|error| format!("Could not restore {name}: {error}. The backup is kept."))?;
    oc_dsl::parse(&source)
        .map_err(|error| format!("Could not restore {name}: {error}. The backup is kept."))
}

/// Delete one copy; the caller confirms with the user first.
pub fn discard_copy(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
    path: &Path,
) -> Result<(), String> {
    let path = check_copy(root, project_path, session, path)?;
    std::fs::remove_file(path)
        .map_err(|error| format!("Could not discard {}: {error}", path.display()))
}

/// Remove every copy of a project. A clean save makes them obsolete, so the
/// save path calls this after it publishes the project file. A save replaces
/// the file by rename, so the directories the old identities left behind are
/// cleared through their owner markers too.
pub fn clear_project(
    root: &Path,
    project_path: Option<&Path>,
    session: &str,
) -> Result<(), String> {
    let mut keys = candidate_keys(project_path, session);
    if let Some(path) = project_path {
        keys.extend(owned_keys(root, path));
    }
    keys.sort();
    keys.dedup();
    for key in keys {
        let directory = root.join(&key);
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not clear recovery copies: {error}")),
        }
        let _ = std::fs::remove_file(owner_path(root, &key));
    }
    Ok(())
}

/// A clean save also clears copies made while the project was untitled: this
/// session's directory and the shared directory the pre-identity build used.
pub fn clear_saved(
    root: &Path,
    saved_path: &Path,
    was_untitled: bool,
    session: &str,
) -> Result<(), String> {
    clear_project(root, Some(saved_path), session)?;
    if was_untitled {
        clear_project(root, None, session)?;
    }
    Ok(())
}

/// Write a copy of the open dirty project now. The caller owns the project
/// lock; this never changes the dirty flag, the undo stack or the audio graph.
fn write_current(
    inner: &mut crate::state::AppStateInner,
    root: &Path,
    saved_at_ms: u64,
) -> Result<PathBuf, String> {
    let entry = write_copy(
        root,
        inner.file_path.as_deref(),
        inner.project.name.as_deref(),
        &inner.session_id,
        &inner.dsl_source,
        saved_at_ms,
    )?;
    inner.autosave_digest = digest(&inner.dsl_source);
    Ok(PathBuf::from(entry.path))
}

/// One timer tick. The first dirty tick starts the interval; later ticks write
/// when it has elapsed. Returns `None` when nothing was written.
pub fn tick(
    inner: &mut crate::state::AppStateInner,
    root: &Path,
    now: Instant,
    saved_at_ms: u64,
) -> Result<Option<PathBuf>, String> {
    if !inner.dirty || inner.transaction.is_some() {
        return Ok(None);
    }
    let Some(started) = inner.autosave_at else {
        inner.autosave_at = Some(now);
        return Ok(None);
    };
    if now.saturating_duration_since(started) < INTERVAL {
        return Ok(None);
    }
    inner.autosave_at = Some(now);
    if inner.autosave_digest == digest(&inner.dsl_source) {
        // Unchanged bytes: the newest copy already holds this state.
        return Ok(None);
    }
    write_current(inner, root, saved_at_ms).map(Some)
}

/// Write a recovery copy for the current document, ignoring errors. Used after
/// an agent edit, where a killed process would otherwise lose the turn's work
/// between two timer ticks.
pub(crate) fn flush_dirty(inner: &mut crate::state::AppStateInner) -> Option<PathBuf> {
    if cfg!(test) && std::env::var_os("OPENCOMPOSE_RECOVERY_DIR").is_none() {
        return None;
    }
    if !inner.dirty || inner.transaction.is_some() || inner.previewing() {
        return None;
    }
    if inner.autosave_digest == digest(&inner.dsl_source) {
        return None;
    }
    let saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    write_current(inner, &app_root(), saved_at).ok()
}

/// Write a copy now, regardless of the interval. Used before a render or a
/// project replacement, where losing the current state would hurt most.
pub fn save_now(state: &AppState) -> Result<Option<PathBuf>, String> {
    if cfg!(test) && std::env::var_os("OPENCOMPOSE_RECOVERY_DIR").is_none() {
        // A command test must not write into the operator's home directory.
        return Ok(None);
    }
    let mut inner = state.lock_inner();
    // A preview holds a proposal, not the document; never write it as recovery.
    if !inner.dirty || inner.transaction.is_some() || inner.previewing() {
        return Ok(None);
    }
    inner.autosave_at = Some(Instant::now());
    if inner.autosave_digest == digest(&inner.dsl_source) {
        return Ok(None);
    }
    write_current(&mut inner, &app_root(), now_ms()).map(Some)
}

/// Start the background writer. It wakes once a second and does nothing until
/// the project has been dirty for the interval.
pub fn spawn(app: tauri::AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("autosave".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(state) = app.try_state::<AppState>() else {
                    continue;
                };
                let due = {
                    let inner = state.lock_inner();
                    inner.dirty && inner.transaction.is_none() && !inner.previewing()
                };
                if due {
                    let mut inner = state.lock_inner();
                    if let Err(error) = tick(&mut inner, &app_root(), Instant::now(), now_ms()) {
                        log::warn!("autosave: {error}");
                    }
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("autosave thread not started: {error}");
    }
}

#[tauri::command]
pub fn list_recoveries(state: State<'_, AppState>) -> Result<Vec<RecoveryEntry>, String> {
    let inner = state.lock_inner();
    list_entries(&app_root(), inner.file_path.as_deref(), &inner.session_id)
}

#[tauri::command]
pub fn discard_recovery(path: String, state: State<'_, AppState>) -> Result<(), String> {
    let inner = state.lock_inner();
    discard_copy(
        &app_root(),
        inner.file_path.as_deref(),
        &inner.session_id,
        Path::new(&path),
    )
}

/// Load one copy through the normal project-load path, so undo, previews and
/// caches behave exactly as they do for an opened file. The copy is kept, and
/// the restored project is dirty: only a save retires the backup.
#[tauri::command]
pub async fn restore_recovery(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::commands::ProjectSnapshot, String> {
    let load_epoch = crate::commands::begin_project_load(&state);
    let request = {
        let inner = state.lock_inner();
        restore_request(&inner, &app_root(), Path::new(&path))?
    };
    // The safety copy and the parse run on the blocking pool without the
    // document lock; only the capture above and the check below take it.
    let (project, request) = crate::commands::shared::run_blocking(move || {
        let project = load_restore(&request)?;
        Ok::<_, String>((project, request))
    })
    .await?;
    // An edit that landed while the copy loaded belongs to the current
    // document; replacing it would drop the edit without publishing it.
    {
        let mut inner = state.lock_inner();
        if inner.revision != request.revision {
            return Err(
                "the project changed while the recovery copy was loading; nothing was restored"
                    .into(),
            );
        }
        if request.previous {
            // The safety copy holds this document, so neither the timer nor the
            // replacement's own pre-load copy has anything newer to add.
            inner.autosave_digest = digest(&request.dsl_source);
        }
    }
    let project_path = request.project_path.clone();
    // Recovery's dirty flag is committed under the same load token, never in
    // an unguarded post-await write to whichever project is now current.
    let mut snapshot = crate::commands::replace_project_on_main_thread(
        app.clone(),
        project,
        project_path,
        load_epoch,
        true,
        false,
        None,
    )
    .await?;
    snapshot.restored_from = Some(path);
    // A recovered document is on screen; warm the graph behind the response.
    crate::commands::warm_preload_later(&app, snapshot.revision);
    crate::commands::warm_audio_clip_peaks(
        &snapshot.project,
        snapshot
            .file_path
            .as_deref()
            .and_then(|path| Path::new(path).parent()),
    );
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::Project;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ocp-recovery-{}-{}-{}",
                std::process::id(),
                FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
                now_ms()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        /// A saved project file with a known mtime.
        fn project(&self, name: &str) -> (PathBuf, Project, String) {
            let path = self.0.join(format!("{name}.ocp"));
            let project = oc_dsl::parse(&format!(
                "@project \"{name}\"\n  tempo: 120\n  time_signature: 4/4\n"
            ))
            .unwrap();
            let source = oc_dsl::serialize(&project);
            std::fs::write(&path, &source).unwrap();
            (path, project, source)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn inner_for(project: &Project, source: &str, path: &Path) -> crate::state::AppStateInner {
        let state = AppState::new();
        let mut inner = state.inner.into_inner().unwrap();
        inner.project = project.clone();
        inner.dsl_source = source.to_string();
        inner.file_path = Some(path.to_path_buf());
        inner.dirty = true;
        inner
    }

    /// The synchronous restore the command splits into capture and load.
    fn restore_document(
        inner: &mut crate::state::AppStateInner,
        root: &Path,
        path: &Path,
    ) -> Result<(Project, String), String> {
        let request = restore_request(inner, root, path)?;
        let project = load_restore(&request)?;
        if request.previous {
            inner.autosave_digest = digest(&request.dsl_source);
        }
        Ok((project, path.display().to_string()))
    }

    #[test]
    fn dirty_project_writes_one_copy_after_the_interval_and_clean_does_not() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("dirty");
        let mut inner = inner_for(&project, &source, &path);
        let session = inner.session_id.clone();
        let start = Instant::now();

        assert!(
            tick(&mut inner, &fixture.0, start, 1_000)
                .unwrap()
                .is_none(),
            "first tick only starts the interval"
        );
        assert!(
            tick(
                &mut inner,
                &fixture.0,
                start + Duration::from_secs(59),
                1_059
            )
            .unwrap()
            .is_none()
        );
        let written = tick(&mut inner, &fixture.0, start + INTERVAL, 1_060)
            .unwrap()
            .expect("copy at the interval");
        let loaded = oc_dsl::parse(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert_eq!(loaded, project, "the copy loads back to an equal project");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            source,
            "the user's file is untouched"
        );
        assert!(
            written.starts_with(fixture.0.join(path_key(Some(&path), &session))),
            "copy lives in the recovery store"
        );

        // A clean project writes nothing, however long the timer runs.
        inner.dirty = false;
        assert!(
            tick(
                &mut inner,
                &fixture.0,
                start + Duration::from_secs(3_600),
                3_600_000
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            list_copies(&fixture.0, Some(&path), &session)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_unchanged_dirty_project_does_not_duplicate_its_copy() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("same");
        let mut inner = inner_for(&project, &source, &path);
        let session = inner.session_id.clone();
        let start = Instant::now();
        tick(&mut inner, &fixture.0, start, 0).unwrap();
        tick(&mut inner, &fixture.0, start + INTERVAL, 60_000)
            .unwrap()
            .unwrap();
        assert!(
            tick(&mut inner, &fixture.0, start + INTERVAL * 2, 120_000)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            list_copies(&fixture.0, Some(&path), &session)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_failed_copy_leaves_no_partial_file_and_keeps_earlier_copies() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("failure");
        let first = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            1_000,
        )
        .unwrap();
        let key = path_key(Some(&path), "session");
        // A directory at the next copy's name makes the atomic rename fail.
        let blocked = fixture.0.join(&key).join(file_name(2_000, &key));
        std::fs::create_dir(&blocked).unwrap();

        let error = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            2_000,
        )
        .unwrap_err();
        assert!(error.contains("Could not write"), "{error}");
        assert!(
            blocked.is_dir(),
            "the blocked name still holds the directory"
        );
        assert_eq!(std::fs::read_to_string(&first.path).unwrap(), source);
        let leftovers: Vec<_> = std::fs::read_dir(fixture.0.join(&key))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temporary file survives: {leftovers:?}"
        );
    }

    #[test]
    fn retention_keeps_the_newest_copies_and_drops_the_oldest() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("retain");
        for index in 0..(MAX_COPIES as u64 + 3) {
            write_copy(
                &fixture.0,
                Some(&path),
                project.name.as_deref(),
                "session",
                &source,
                1_000 + index,
            )
            .unwrap();
        }
        let copies = list_copies(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(copies.len(), MAX_COPIES);
        assert_eq!(
            copies.first().unwrap().saved_at_ms,
            1_000 + MAX_COPIES as u64 + 2
        );
        assert!(
            !copies.iter().any(|copy| copy.saved_at_ms == 1_000),
            "the oldest copy is gone"
        );
    }

    #[test]
    fn discovery_marks_a_newer_copy_and_refuses_an_older_one_with_a_reason() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("discover");
        let stamp = file_stamp(Some(&path)).unwrap();
        // The older copy exists first: a new write is always stamped above it.
        write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            stamp - 5_000,
        )
        .unwrap();
        write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            stamp + 5_000,
        )
        .unwrap();

        let copies = list_copies(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].saved_at_ms, stamp + 5_000, "newest first");
        assert!(copies[0].newer_than_disk);
        assert_eq!(copies[0].restore_disabled_reason, None);
        assert!(!copies[1].newer_than_disk);
        assert_eq!(
            copies[1].restore_disabled_reason.as_deref(),
            Some(OLDER_THAN_FILE)
        );
        assert_eq!(copies[0].project_name, "discover");
        assert!(copies[0].size > 0);
    }

    #[test]
    fn a_missing_recovery_directory_lists_nothing() {
        let fixture = Fixture::new();
        let (path, _, _) = fixture.project("empty");
        assert!(
            list_copies(&fixture.0, Some(&path), "session")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn restore_loads_the_copy_keeps_it_and_a_save_clears_it() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("restore");
        let entry = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            now_ms(),
        )
        .unwrap();
        let copy = Path::new(&entry.path);

        let loaded = load_copy(&fixture.0, Some(&path), "session", copy).unwrap();
        assert_eq!(loaded, project);
        assert!(copy.is_file(), "a restore keeps the backup");

        clear_saved(&fixture.0, &path, false, "session").unwrap();
        assert!(!copy.exists(), "a clean save retires the copies");
    }

    #[test]
    fn a_previous_version_is_listed_with_the_recovery_copies_with_its_stamp() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("previous");
        let old =
            oc_dsl::parse("@project \"older\"\n  tempo: 90\n  time_signature: 4/4\n").unwrap();
        let old_source = oc_dsl::serialize(&old);
        let backup = crate::project_persist::backup_path(&path);
        std::fs::write(&backup, &old_source).unwrap();
        let stamp = std::fs::metadata(&backup)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&path),
        )
        .unwrap();

        let entries = list_entries(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(
            entries.len(),
            2,
            "one recovery copy and one previous version"
        );
        let previous = entries
            .iter()
            .find(|entry| entry.kind == RecoveryKind::Backup)
            .expect("the previous version is listed");
        assert_eq!(previous.path, backup.display().to_string());
        assert_eq!(previous.saved_at_ms, stamp);
        assert_eq!(previous.size, old_source.len() as u64);
        assert_eq!(previous.project_name, "older");
        assert!(!previous.newer_than_disk);
        assert_eq!(previous.restore_disabled_reason, None);
        assert!(
            entries
                .iter()
                .any(|entry| entry.kind == RecoveryKind::Recovery),
            "the autosave copy is still listed"
        );
        assert_eq!(
            list_copies(&fixture.0, Some(&path), "session")
                .unwrap()
                .len(),
            1,
            "list_copies remains recovery-only"
        );
    }

    #[test]
    fn a_project_without_a_previous_version_adds_no_recovery_row() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("no-previous");
        write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&path),
        )
        .unwrap();

        let entries = list_entries(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries
                .iter()
                .all(|entry| entry.kind == RecoveryKind::Recovery)
        );
    }

    #[test]
    fn restoring_the_previous_version_writes_the_recovery_copy_first() {
        let fixture = Fixture::new();
        let (path, current, current_source) = fixture.project("current");
        let old =
            oc_dsl::parse("@project \"older\"\n  tempo: 90\n  time_signature: 4/4\n").unwrap();
        let backup = crate::project_persist::backup_path(&path);
        std::fs::write(&backup, oc_dsl::serialize(&old)).unwrap();
        let mut inner = inner_for(&current, &current_source, &path);

        let (restored, restored_from) = restore_document(&mut inner, &fixture.0, &backup).unwrap();

        assert_eq!(restored, old, "the previous document is loaded");
        assert_eq!(restored_from, backup.display().to_string());
        assert!(backup.is_file(), "the previous version is kept");
        let safety = list_copies(&fixture.0, Some(&path), &inner.session_id).unwrap();
        assert_eq!(safety.len(), 1, "one safety copy of the current document");
        assert_eq!(
            std::fs::read_to_string(&safety[0].path).unwrap(),
            current_source,
            "the safety copy holds what the restore replaced"
        );
        assert_eq!(inner.autosave_digest, digest(&current_source));
    }

    #[test]
    fn a_previous_version_that_does_not_parse_is_listed_as_an_unrestorable_recovery_row() {
        let fixture = Fixture::new();
        let (path, _, _) = fixture.project("corrupt-previous");
        let backup = crate::project_persist::backup_path(&path);
        let broken = "this is not a project";
        std::fs::write(&backup, broken).unwrap();
        let parse_error = oc_dsl::parse(broken).unwrap_err().to_string();

        let entries = list_entries(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(entries.len(), 1);
        let previous = &entries[0];
        assert_eq!(previous.kind, RecoveryKind::Backup);
        let reason = previous
            .restore_disabled_reason
            .as_deref()
            .expect("a broken previous version cannot be restored");
        assert!(reason.starts_with(BACKUP_UNREADABLE), "{reason}");
        assert!(
            reason.contains(&parse_error),
            "names the parse error: {reason}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_recovery_root_refuses_the_previous_version_restore() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let (path, current, current_source) = fixture.project("read-only");
        let old =
            oc_dsl::parse("@project \"older\"\n  tempo: 90\n  time_signature: 4/4\n").unwrap();
        let backup = crate::project_persist::backup_path(&path);
        std::fs::write(&backup, oc_dsl::serialize(&old)).unwrap();
        let mut inner = inner_for(&current, &current_source, &path);
        let blocked = fixture.0.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // The recovery root does not exist and cannot be created below a
        // read-only parent, so no safety copy can be written.
        let root = blocked.join("recovery");

        let error = restore_document(&mut inner, &root, &backup).unwrap_err();

        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            error.contains("Could not keep the current project"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            current_source,
            "the project file is untouched"
        );
        assert_eq!(
            inner.dsl_source, current_source,
            "the document is untouched"
        );
        assert_eq!(inner.project, current, "the document is untouched");
    }

    #[test]
    fn a_copy_of_a_different_project_is_refused_with_a_sentence() {
        let fixture = Fixture::new();
        let (first_path, first, first_source) = fixture.project("first");
        let (second_path, second, second_source) = fixture.project("second");
        let foreign = write_copy(
            &fixture.0,
            Some(&second_path),
            second.name.as_deref(),
            "session",
            &second_source,
            5_000,
        )
        .unwrap();

        let error = load_copy(
            &fixture.0,
            Some(&first_path),
            "session",
            Path::new(&foreign.path),
        )
        .unwrap_err();
        assert!(error.contains("different project"), "{error}");

        // A copy of the right project moved into the wrong directory is refused too.
        let entry = write_copy(
            &fixture.0,
            Some(&first_path),
            first.name.as_deref(),
            "session",
            &first_source,
            6_000,
        )
        .unwrap();
        let moved = fixture
            .0
            .join(path_key(Some(&second_path), "session"))
            .join(Path::new(&entry.path).file_name().unwrap());
        assert!(load_copy(&fixture.0, Some(&first_path), "session", &moved).is_err());
    }

    /// A stamp that is certainly newer than the saved file's modification time.
    fn above_file(path: &Path) -> u64 {
        file_stamp(Some(path)).unwrap() + 5_000
    }

    #[test]
    fn two_unsaved_sessions_never_share_a_recovery_key() {
        let fixture = Fixture::new();
        let (_, _, source) = fixture.project("seed");
        let first = write_copy(
            &fixture.0,
            None,
            Some("Untitled"),
            "session-one",
            &source,
            1_000,
        )
        .unwrap();
        let second = write_copy(
            &fixture.0,
            None,
            Some("Untitled"),
            "session-two",
            &source,
            1_000,
        )
        .unwrap();
        assert_ne!(
            Path::new(&first.path).parent(),
            Path::new(&second.path).parent(),
            "each session owns its own directory"
        );

        let listed = list_copies(&fixture.0, None, "session-one").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, first.path);
        let error =
            load_copy(&fixture.0, None, "session-one", Path::new(&second.path)).unwrap_err();
        assert!(error.contains("different project"), "{error}");
        assert!(discard_copy(&fixture.0, None, "session-one", Path::new(&second.path)).is_err());
        assert!(Path::new(&second.path).exists());
    }

    #[test]
    fn a_renamed_project_keeps_its_copies_and_a_new_file_does_not() {
        let fixture = Fixture::new();
        let (old_path, project, source) = fixture.project("before");
        let entry = write_copy(
            &fixture.0,
            Some(&old_path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&old_path),
        )
        .unwrap();
        let new_path = fixture.0.join("after.ocp");
        std::fs::rename(&old_path, &new_path).unwrap();

        let copies = list_copies(&fixture.0, Some(&new_path), "session").unwrap();
        assert_eq!(copies.len(), 1, "the copies followed the same file id");
        assert_eq!(copies[0].path, entry.path);
        assert_eq!(copies[0].restore_disabled_reason, None);

        // A different file at the old path has another identity and sees nothing.
        std::fs::write(&old_path, &source).unwrap();
        assert!(
            list_copies(&fixture.0, Some(&old_path), "session")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_project_is_keyed_by_its_file_identity_with_the_path_hash_as_fallback() {
        let fixture = Fixture::new();
        let (path, _, _) = fixture.project("identity");
        let key = path_key(Some(&path), "session");
        #[cfg(any(unix, windows))]
        assert!(key.starts_with("id-"), "platform file id: {key}");
        #[cfg(not(any(unix, windows)))]
        assert_eq!(key, legacy_key(&path));

        // With the file gone, the key falls back to the legacy path hash.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(path_key(Some(&path), "session"), legacy_key(&path));
    }

    #[test]
    fn a_backwards_clock_cannot_make_a_new_copy_older() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("clock");
        let file = file_stamp(Some(&path)).unwrap();
        let future = file + 3_600_000;
        write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            future,
        )
        .unwrap();
        let earlier = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            file - 3_600_000,
        )
        .unwrap();
        assert!(earlier.saved_at_ms > future);

        let copies = list_copies(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(copies[0].path, earlier.path, "the new copy sorts first");
        assert!(copies[0].newer_than_disk);
        assert_eq!(copies[0].restore_disabled_reason, None);
    }

    #[test]
    fn two_copies_in_one_millisecond_get_distinct_names() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("same-ms");
        let at = above_file(&path);
        let first = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            at,
        )
        .unwrap();
        let second = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            at,
        )
        .unwrap();
        assert_ne!(first.path, second.path);
        assert!(second.saved_at_ms > first.saved_at_ms);
        assert_eq!(
            list_copies(&fixture.0, Some(&path), "session")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_copy_written_by_the_old_build_still_lists_and_restores() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("legacy");
        let key = legacy_key(&path);
        let directory = fixture.0.join(&key);
        std::fs::create_dir_all(&directory).unwrap();
        let stamp = above_file(&path);
        std::fs::write(directory.join(file_name(stamp, &key)), &source).unwrap();

        let copies = list_copies(&fixture.0, Some(&path), "session").unwrap();
        assert_eq!(copies.len(), 1, "the legacy directory is still read");
        assert_eq!(copies[0].saved_at_ms, stamp);
        assert_eq!(copies[0].restore_disabled_reason, None);
        let loaded = load_copy(
            &fixture.0,
            Some(&path),
            "session",
            Path::new(&copies[0].path),
        )
        .unwrap();
        assert_eq!(loaded, project);

        // The project still exists, so its legacy directory outlives the bound.
        let later = SystemTime::now() + ORPHAN_AGE + Duration::from_secs(60);
        assert!(sweep(&fixture.0, later).is_empty());
        assert!(directory.exists());
    }

    #[test]
    fn a_save_clears_the_directory_a_replaced_file_id_left_behind() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("replace");
        let entry = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&path),
        )
        .unwrap();
        let old_key = path_key(Some(&path), "session");

        // A save replaces the file by rename, so the path gets a new file id.
        let replacement = fixture.0.join("replacement.tmp");
        std::fs::write(&replacement, &source).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_ne!(path_key(Some(&path), "session"), old_key);

        clear_saved(&fixture.0, &path, false, "session").unwrap();
        assert!(
            !Path::new(&entry.path).exists(),
            "the old identity directory is cleared"
        );
    }

    #[test]
    fn a_recovery_directory_whose_project_is_gone_is_swept_after_the_bound() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("gone");
        let entry = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&path),
        )
        .unwrap();
        let directory = Path::new(&entry.path).parent().unwrap().to_path_buf();
        std::fs::remove_file(&path).unwrap();

        assert!(
            sweep(&fixture.0, SystemTime::now()).is_empty(),
            "a fresh directory stays"
        );
        let later = SystemTime::now() + ORPHAN_AGE + Duration::from_secs(60);
        assert_eq!(sweep(&fixture.0, later), vec![directory.clone()]);
        assert!(!directory.exists());
        assert!(!directory.with_extension("owner").exists());
    }

    #[test]
    fn a_recovery_directory_of_a_live_project_survives_the_bound() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("live");
        let entry = write_copy(
            &fixture.0,
            Some(&path),
            project.name.as_deref(),
            "session",
            &source,
            above_file(&path),
        )
        .unwrap();
        let later = SystemTime::now() + ORPHAN_AGE + Duration::from_secs(60);
        assert!(sweep(&fixture.0, later).is_empty());
        assert!(Path::new(&entry.path).exists());
    }

    #[test]
    fn an_ownerless_directory_is_swept_only_after_the_bound() {
        let fixture = Fixture::new();
        let (path, _, source) = fixture.project("legacy-sweep");
        let key = legacy_key(&path);
        let directory = fixture.0.join(&key);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(file_name(1_000, &key)), &source).unwrap();

        assert!(sweep(&fixture.0, SystemTime::now()).is_empty());
        let later = SystemTime::now() + ORPHAN_AGE + Duration::from_secs(60);
        assert_eq!(sweep(&fixture.0, later), vec![directory.clone()]);
        assert!(!directory.exists());
    }

    #[test]
    fn saving_an_untitled_project_clears_the_untitled_copies_too() {
        let fixture = Fixture::new();
        let (path, project, source) = fixture.project("untitled-save");
        let session = "untitled-session";
        let untitled =
            write_copy(&fixture.0, None, Some("Untitled"), session, &source, 1_000).unwrap();
        // A copy the pre-identity build left in the shared untitled directory.
        let legacy = fixture.0.join(LEGACY_UNTITLED_KEY);
        std::fs::create_dir_all(&legacy).unwrap();
        let legacy_copy = legacy.join("1000-untitled.ocp");
        std::fs::write(&legacy_copy, &source).unwrap();

        let listed = list_copies(&fixture.0, None, session).unwrap();
        assert_eq!(listed.len(), 2, "a legacy untitled copy still lists");
        assert!(
            listed
                .iter()
                .any(|copy| copy.path == legacy_copy.display().to_string())
        );

        clear_saved(&fixture.0, &path, true, session).unwrap();
        assert!(!Path::new(&untitled.path).exists());
        assert!(!fixture.0.join(session_key(session)).exists());
        assert!(!legacy.exists());
        let _ = project;
    }
}
