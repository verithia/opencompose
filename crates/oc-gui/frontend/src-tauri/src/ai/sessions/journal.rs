//! The append-only transcript journal: one record per model turn, a compaction
//! that replaces the view and a trim that keeps the file under its byte cap.
//! Reads return the conversation a record list adds up to; writes either append
//! whole lines or replace the file after staging the records it keeps.

use super::view;
use super::{
    MAX_SESSION_BYTES, MESSAGES_FILE, MessageKind, MessageMeta, SessionMeta, artifacts,
    canonicalize_media, estimate_tokens, is_compaction_summary, message_protocol,
    newest_pinned_rows, now, tool_results,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::BufRead;
use std::path::Path;

/// The record format this build writes.
pub(super) const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RecordKind {
    /// Messages appended for one model turn.
    Turn,
    /// Replaces the whole conversation with a summary.
    Compaction,
    /// Replaces the whole conversation with the newest messages that fit.
    Trim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Dropped {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Record {
    pub v: u32,
    pub kind: RecordKind,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<Dropped>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default)]
    pub messages: Vec<Value>,
}

pub(super) fn read_meta(path: &Path) -> Option<SessionMeta> {
    super::lookup::read_meta(path).ok().flatten()
}

pub(super) fn write_meta(dir: &Path, meta: &SessionMeta) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(meta).map_err(|error| error.to_string())?;
    crate::private_fs::write_atomic(&dir.join(super::META_FILE), &bytes)
        .map_err(|error| format!("sessions: {error}"))
}

/// The conversation the records add up to, plus how many lines were unreadable.
/// Every turn the file ever recorded, with the compaction records ignored.
/// `read_view` is the conversation the model gets; this is the history the user
/// wrote, so a compacted session can still be inspected in full.
pub fn read_original(dir: &Path) -> Result<(Vec<Value>, u64, u64), String> {
    let path = dir.join(MESSAGES_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), 0, 0));
        }
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let mut messages = Vec::new();
    let mut compactions = 0;
    let mut skipped = 0;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("sessions: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(record) => match record.kind {
                RecordKind::Turn => messages.extend(record.messages),
                RecordKind::Compaction => compactions += 1,
                RecordKind::Trim => {}
            },
            Err(_) => skipped += 1,
        }
    }
    canonicalize_media(&mut messages);
    message_protocol::migrate(&mut messages);
    artifacts::repair(&mut messages);
    Ok((messages, compactions, skipped))
}

/// The metadata of one session directory, for callers that only need its name
/// and size. `None` when the directory is not a session this build can read.
pub fn read_meta_for_storage(dir: &Path) -> Option<SessionMeta> {
    read_meta(&dir.join(super::META_FILE))
}

pub(super) fn read_view(dir: &Path) -> Result<(Vec<Value>, u64), String> {
    read_current(dir).map(|current| (current.messages, current.skipped))
}

/// The repaired view with what the read learned about the file.
pub(super) fn read_current(dir: &Path) -> Result<view::Tail, String> {
    let mut current = view::read_tail(dir)?;
    let view = &mut current.messages;
    canonicalize_media(view);
    message_protocol::migrate(view);
    artifacts::repair(view);
    super::turns::repair_pairing(view);
    Ok(current)
}

/// Every record in order. Unlike [`read_view`] this keeps the compaction and
/// trim records, so a rewrite can decide what to keep.
pub(super) fn read_records(dir: &Path) -> Result<Vec<Record>, String> {
    let path = dir.join(MESSAGES_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let mut records = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("sessions: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Record>(&line) {
            records.push(record);
        }
    }
    Ok(records)
}

/// The record indices of every compaction, oldest first.
pub(super) fn compaction_indices(records: &[Record]) -> Vec<usize> {
    records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.kind == RecordKind::Compaction)
        .map(|(index, _)| index)
        .collect()
}

pub(super) fn write_records(dir: &Path, records: &[Record]) -> Result<(), String> {
    // The index points into the file this replaces; without it the next read
    // scans the new file and indexes it.
    view::forget(dir)?;
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(
            serde_json::to_vec(record)
                .map_err(|error| format!("sessions: {error}"))?
                .as_slice(),
        );
        bytes.push(b'\n');
    }
    crate::private_fs::write_atomic(&dir.join(MESSAGES_FILE), &bytes)
        .map_err(|error| format!("sessions: {error}"))
}

/// What an append did to the saved view.
#[must_use = "count the tokens of the view the append left"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppendOutcome {
    /// The records joined the existing view.
    Appended,
    /// The cap replaced the view with its newest rows; the caller counts these.
    Trimmed { tokens_after: u64 },
}

impl AppendOutcome {
    /// The token count of the view the append left, from the caller's view
    /// after it added the same records.
    pub(super) fn kept_tokens(self, view: &[Value]) -> u64 {
        match self {
            Self::Appended => estimate_tokens(view),
            Self::Trimmed { tokens_after } => tokens_after,
        }
    }
}

/// Appends records as lines, keeping the file under [`MAX_SESSION_BYTES`]. An
/// append that would exceed the cap first replaces the conversation with a
/// `trim` record that states what it dropped.
pub(super) fn append_records(dir: &Path, records: &[Record]) -> Result<AppendOutcome, String> {
    append_records_under(dir, records, MAX_SESSION_BYTES)
}

/// The cap is a parameter so the trimming path is testable without writing a
/// gigabyte.
pub(super) fn append_records_under(
    dir: &Path,
    records: &[Record],
    cap: u64,
) -> Result<AppendOutcome, String> {
    let path = dir.join(MESSAGES_FILE);
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(
            serde_json::to_vec(record)
                .map_err(|error| format!("sessions: {error}"))?
                .as_slice(),
        );
        bytes.push(b'\n');
    }
    let current = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if current + bytes.len() as u64 <= cap {
        crate::private_fs::append_private(&path, &bytes)
            .map_err(|error| format!("sessions: {error}"))?;
        return Ok(AppendOutcome::Appended);
    }
    let (mut view, _) = read_view(dir)?;
    for record in records {
        match record.kind {
            RecordKind::Turn => view.extend(record.messages.clone()),
            RecordKind::Compaction | RecordKind::Trim => view = record.messages.clone(),
        }
    }
    let kept = newest_that_fit(&view, cap / 2);
    if kept.is_empty() {
        return Err("This turn is larger than a session may hold.".into());
    }
    let mut note = json!({
        "role": "user",
        "content": format!(
            "Earlier messages of this session were dropped to keep it under {} bytes. \
             Messages {}..{} are gone; the conversation resumes below.",
            cap,
            view.len() - kept.len(),
            view.len()
        )
    });
    message_protocol::tag(&mut note, MessageMeta::new(MessageKind::HostNotice));
    let mut messages = vec![note];
    messages.extend(kept);
    let tokens_after = estimate_tokens(&messages);
    let trim = Record {
        v: VERSION,
        kind: RecordKind::Trim,
        at: now(),
        dropped: Some(Dropped {
            from: 0,
            to: view.len() - messages.len() + 1,
        }),
        tokens_before: Some(estimate_tokens(&view)),
        tokens_after: Some(tokens_after),
        truncated: Some(true),
        messages,
    };
    write_records(dir, &[trim])?;
    // The rows the trim dropped were the only owners of their results.
    tool_results::collect(dir);
    Ok(AppendOutcome::Trimmed { tokens_after })
}

/// The newest messages of a conversation that fit in `budget` bytes, in order.
/// The latest compaction summary leads them when the cut fell after it: it
/// holds the standing intent and the decisions the dropped turns made.
pub(super) fn newest_that_fit(view: &[Value], budget: u64) -> Vec<Value> {
    let size_of = |message: &Value| {
        serde_json::to_vec(message)
            .map(|bytes| bytes.len())
            .unwrap_or(0) as u64
            + 1
    };
    let summary = view.iter().rposition(is_compaction_summary);
    let pinned = newest_pinned_rows(view);
    let reserved = summary.map_or(0, |index| size_of(&view[index]))
        + pinned.iter().map(|(_, row)| size_of(row)).sum::<u64>();
    let mut kept: Vec<Value> = Vec::new();
    let mut used = 0u64;
    for message in view.iter().rev() {
        let size = size_of(message);
        if used + size + reserved > budget && !kept.is_empty() {
            break;
        }
        used += size;
        kept.push(message.clone());
    }
    kept.reverse();
    // A cut inside a round can keep results whose call fell off the front;
    // dropping them keeps the conversation one every later append accepts.
    while !kept.is_empty() && super::turns::check_pairing(&kept).is_err() {
        kept.remove(0);
    }
    // The summary and the pinned notices are user rows, so they open no call
    // and answer none: in front of a well-paired tail the conversation stays
    // well paired.
    let dropped = view.len() - kept.len();
    if !kept.is_empty() {
        let mut front = Vec::new();
        if let Some(index) = summary
            && dropped > index
        {
            front.push(view[index].clone());
        }
        front.extend(
            pinned
                .into_iter()
                .filter(|(index, _)| dropped > *index)
                .map(|(_, row)| row),
        );
        if !front.is_empty() {
            front.extend(kept);
            return front;
        }
    }
    kept
}
