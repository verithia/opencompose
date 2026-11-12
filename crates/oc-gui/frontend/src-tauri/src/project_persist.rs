//! Durable writes of the open project to its own file.
//!
//! The recovery store stays the crash net, on its own schedule. A project that
//! has a path also has a file, and an edit the agent applied belongs there
//! before the process leaves, so every write below publishes the same document
//! the save path publishes: a temporary file, then a rename. The content a
//! write replaces stays in one `<project>.bak` beside it.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oc_dsl::ast::Project;
use tauri::Manager;

use crate::state::{AppState, AppStateInner};

/// Extension of the single backup kept beside a project.
pub const BACKUP_SUFFIX: &str = "bak";

/// Why a write is refused. The operator log names the reason.
const NO_PATH: &str = "the project has no file path yet";

/// Serialises publications, so two writers cannot interleave their backup and
/// rename steps.
///
/// The document lock is always taken before this one: [`flush`] holds the
/// caller's guard and [`flush_state`] releases its guard before publishing.
/// Nothing acquires the document lock while holding this one.
static PUBLISH_LOCK: Mutex<()> = Mutex::new(());

/// `<project>.bak`, the content the last durable write replaced.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{BACKUP_SUFFIX}"));
    PathBuf::from(name)
}

/// What one publication did.
enum Published {
    /// The file already held these bytes; nothing was written.
    Current,
    /// Bytes written to the project file.
    Bytes(usize),
}

/// The document one publication writes, captured under the document lock.
struct Publication {
    project: Project,
    source: String,
    path: PathBuf,
}

/// Publish the open document to its project file after an agent mutation.
///
/// Returns the byte count written. A clean document, a discarded one, an open
/// edit preview and an open transaction write nothing: the first has nothing to
/// publish, the second is a document the operator chose to drop, and the other
/// two hold a document the save path refuses. A refusal or a failed write is an
/// error, so the caller can tell the file is stale.
pub fn flush(inner: &mut AppStateInner) -> Result<Option<usize>, String> {
    let publication = snapshot_for_publish(inner)?;
    publish_captured(publication)
}

/// Publish a dirty project before the process leaves. The window-close path and
/// the application exit path both call this; a clean project is left alone.
///
/// The document lock is held only long enough to clone the document; the read,
/// the backup and the write run after it is released, so the 10 Hz meter and
/// the 20 Hz transport poll never wait for the disk.
pub fn flush_state(state: &AppState) -> Result<Option<usize>, String> {
    let publication = {
        let inner = state.lock_inner();
        snapshot_for_publish(&inner)
    }?;
    publish_captured(publication)
}

/// Decide whether the document may be published and capture what the write
/// needs. Runs under the document lock and does no filesystem work.
fn snapshot_for_publish(inner: &AppStateInner) -> Result<Option<Publication>, String> {
    if !inner.dirty || inner.discard_pending || inner.previewing() || inner.transaction.is_some() {
        return Ok(None);
    }
    let Some(path) = inner.file_path.clone() else {
        report_refusal(None, NO_PATH);
        return Err(NO_PATH.into());
    };
    Ok(Some(Publication {
        project: inner.project.clone(),
        source: inner.dsl_source.clone(),
        path,
    }))
}

/// Publish a captured document. Runs without the document lock.
fn publish_captured(publication: Option<Publication>) -> Result<Option<usize>, String> {
    let Some(Publication {
        project,
        source,
        path,
    }) = publication
    else {
        return Ok(None);
    };
    let _publishing = PUBLISH_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match publish(&project, &source, &path) {
        Ok(Published::Current) => Ok(None),
        Ok(Published::Bytes(bytes)) => {
            report_write(&path, bytes);
            Ok(Some(bytes))
        }
        Err(reason) => {
            report_refusal(Some(&path), &reason);
            Err(reason)
        }
    }
}

/// The exit write may publish the open document only after the panel answered
/// the quit prompt. `quit_now` arms it; a discard and an unanswered prompt both
/// keep it disarmed, so nothing reaches the file behind the operator's answer.
pub(crate) fn exit_write_allowed(inner: &AppStateInner) -> bool {
    inner.exit_ready && !inner.discard_pending
}

/// The Tauri event hook behind [`flush_state`], over the state the event reads.
///
/// The lock is taken and released before the write, so a save holding it cannot
/// meet this one.
pub(crate) fn flush_on_exit_state(state: &AppState) {
    let allowed = {
        let inner = state.lock_inner();
        exit_write_allowed(&inner)
    };
    if !allowed {
        return;
    }
    flush_exit(state);
}

/// The Tauri event hook behind [`flush_state`].
pub fn flush_on_exit(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    flush_on_exit_state(&state);
}

/// Publish before the process leaves. The exit write is the last chance to keep
/// the turn's work, so a failure is an error rather than a warning and gets one
/// more attempt, because a transient lock or a slow disk can clear.
pub(crate) fn flush_exit(state: &AppState) {
    if let Err(reason) = flush_state(state) {
        log::error!("project write failed on exit: {reason}");
        record(format!("error: project write failed on exit: {reason}"));
        if let Err(reason) = flush_state(state) {
            log::error!("project write failed on exit after one retry: {reason}");
            record(format!(
                "error: project write failed on exit after one retry: {reason}"
            ));
        }
    }
}

/// A tool call that published a new document leaves the project file stale.
///
/// History and a discarded batch restore older content, and an open batch is
/// published by its commit, so neither the tools below nor a write inside a
/// transaction reaches the file. A write this call could not make is added to
/// the tool result, so the model knows the file was not written.
pub fn after_tool(state: &AppState, tool: &str, result: &mut serde_json::Value) {
    let publishes = match tool {
        "undo" | "redo" | "transaction_begin" | "transaction_rollback" => false,
        _ => crate::tool_registry::is_mutation_tool(tool) || tool == "transaction_commit",
    };
    if !publishes {
        return;
    }
    if let Err(reason) = flush_state(state) {
        mark_stale(result, &reason);
    }
}

/// Adds the stale-file state to the result the model reads.
fn mark_stale(result: &mut serde_json::Value, reason: &str) {
    let error = format!("the project file was not written: {reason}");
    if let Some(object) = result.as_object_mut() {
        object.insert("project_file_saved".into(), serde_json::json!(false));
        object.insert("project_file_error".into(), serde_json::json!(error));
    } else {
        // A result that is not an object would hide the failure, so it is
        // replaced by one that carries both the value and the state.
        let previous = std::mem::take(result);
        *result = serde_json::json!({
            "result": previous,
            "project_file_saved": false,
            "project_file_error": error,
        });
    }
}

/// Write `source` to `path`, keeping the content it replaces in one backup.
fn publish(project: &Project, source: &str, path: &Path) -> Result<Published, String> {
    // The save path refuses a symlinked project; refuse it before the backup
    // step so a refused write leaves the directory alone.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("refusing to write through a project symlink".into());
    }
    // Bytes already on disk are the durable record: a second exit event, or an
    // edit that changed nothing, must not rewrite the file.
    let previous = std::fs::read(path).ok();
    if previous.as_deref() == Some(source.as_bytes()) {
        return Ok(Published::Current);
    }
    if let Some(previous) = previous {
        replace_file(&backup_path(path), &previous)
            .map_err(|error| format!("could not keep the previous content: {error}"))?;
    }
    crate::project_resources::save(project, source, Some(path), path)
        .map_err(|error| format!("could not write the project file: {error}"))?;
    Ok(Published::Bytes(source.len()))
}

/// Replace `path` through a temporary file beside it, so a failure leaves
/// neither a partial backup nor a temporary file.
///
/// Unlike `private_fs::write_atomic`, this never changes the mode of the
/// directory: that directory is the operator's, not ours.
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".ocp-bak-{}-{nanos}.tmp", std::process::id()));
    let outcome = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

fn report_write(path: &Path, bytes: usize) {
    let line = format!("project write: {} ({bytes} bytes)", path.display());
    log::info!("{line}");
    record(format!("info: {line}"));
}

fn report_refusal(path: Option<&Path>, reason: &str) {
    let line = match path {
        Some(path) => format!("project write refused: {}: {reason}", path.display()),
        None => format!("project write refused: {reason}"),
    };
    log::warn!("{line}");
    record(format!("warn: {line}"));
}

/// Lines reported through the operator log, so a test can assert what the next
/// person reads. Compiled out of a release build.
#[cfg(test)]
pub(crate) fn logged() -> Vec<String> {
    LOG.lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

#[cfg(test)]
static LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn record(line: String) {
    LOG.lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(line);
}

#[cfg(not(test))]
fn record(_line: String) {}
