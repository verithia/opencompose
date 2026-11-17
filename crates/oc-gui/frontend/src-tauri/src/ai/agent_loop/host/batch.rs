//! The one batch a turn's project edits join, and its commit.

use super::super::CommitError;
use crate::ai::sessions::Store;
use crate::state::AppState;

/// A turn reports success only after its committed edits reach the working file.
/// Commits a batch a gone turn left open, so the edits that landed stay, and
/// then writes the project file. `None` means no loop-owned batch is open; a
/// `/call` client's open batch is its live transaction and is left alone. The
/// inner `Err` is why the file was not written; the commit stands either way.
pub(crate) fn commit_stranded(state: &AppState) -> Result<Option<Result<(), String>>, String> {
    let Some(owner) = state
        .batch_owner()
        .filter(|open| stranded_loop_batch(Some(open)))
    else {
        return Ok(None);
    };
    crate::agent_api::transaction_commit(state, &owner)?;
    Ok(Some(crate::project_persist::flush_state(state).map(|_| ())))
}

/// Opens a turn's batch, committing a stranded one first. A stranded batch
/// whose file write fails still closed, so the new batch opens: the turn's own
/// commit writes the whole project again and reports a file it cannot write.
/// Returns what happened to a stranded batch, if there was one.
pub(crate) fn begin_loop_batch(
    state: &AppState,
    label: String,
    owner: String,
    session: &str,
) -> Result<Option<Result<(), String>>, String> {
    let stranded = commit_stranded(state)?;
    // The binding and the batch open under the same state lock, so a file
    // opened meanwhile cannot receive the batch the conversation did not bind.
    let mut inner = state.lock_inner();
    let open = inner.file_path.clone();
    let (_, meta) = Store::config()
        .find(session)
        .map_err(|error| format!("The conversation's binding could not be read: {error}"))?;
    let status = crate::ai::sessions::binding_status(&meta, open.as_deref());
    if !status.matches {
        let mismatch = crate::ai::sessions::BindingMismatch {
            bound: status.bound.unwrap_or_default(),
            open: status.open.unwrap_or_default(),
        };
        return Err(format!(
            "{} This turn cannot edit the open file; stop it and bind the conversation to that \
             file before trying again.",
            mismatch.message()
        ));
    }
    inner.begin_transaction(label, owner)?;
    Ok(stranded)
}

pub(crate) fn commit_batch_and_save(state: &AppState, owner: &str) -> Result<(), CommitError> {
    crate::agent_api::transaction_commit(state, owner).map_err(CommitError::commit)?;
    crate::project_persist::flush_state(state)
        .map(|_| ())
        .map_err(|reason| {
            CommitError::save(format!(
                "The edits are committed in memory but the project file was not written: {reason}"
            ))
        })
}

/// Whether the open batch was left by a backend turn that is gone. It is
/// asked by a turn that holds the request slot, and a turn holds the slot for
/// as long as its batch is open, so every loop-owned batch it sees is
/// stranded, whatever conversation it belongs to. A `/call` client's batch is
/// still in use.
fn stranded_loop_batch(open: Option<&str>) -> bool {
    open.is_some_and(crate::state::loop_batch_owner)
}
