//! The saved form of one message: the role, prose and tool envelope a
//! conversation keeps, with host-context markers and attachments normalized.
//! Provider state and inline audio never survive; a person's snapshot and
//! history position do.

use super::tool_results;
use super::{HistoryAt, MessageKind, MessageMeta, clip, message_protocol};
use serde_json::{Value, json};
use std::path::Path;

/// Marks an audition the host delivered for a listening call. The row is
/// context the model needs, never something the user said.
pub(super) const CONTEXT_AUDITION: &str = "audition";
/// Marks the message that replaces compacted turns.
pub(super) const CONTEXT_SUMMARY: &str = "summary";
/// Longest project path a saved message records with its snapshot.
pub(super) const MAX_PROJECT_PATH: usize = 4096;
/// One message of a turn is refused above this, so a single tool result cannot
/// consume the whole file.
pub const MAX_MESSAGE_CHARS: usize = 400_000;

/// The saved form of a wire message: role, text and tool envelope, with an
/// inline audio clip replaced by a note (its bytes are not saved) and oversized
/// text clipped.
pub(super) fn saved_message(message: &Value) -> Option<Value> {
    let role = message["role"].as_str()?;
    if role != "user" && role != "assistant" && role != "tool" {
        return None;
    }
    let mut saved = serde_json::Map::new();
    saved.insert("role".into(), json!(role));
    if let Some(meta) = message.get("message_meta") {
        saved.insert("message_meta".into(), meta.clone());
    } else if let Some(meta) = message_protocol::for_new(message) {
        saved.insert("message_meta".into(), json!(meta));
    }
    let mut content = message["content"].as_str().unwrap_or_default().to_owned();
    if content.chars().count() > MAX_MESSAGE_CHARS {
        content = clip(&content, MAX_MESSAGE_CHARS);
    }
    if message.get("input_audio").is_some() {
        content.push_str("\n[audio clip not saved]");
    }
    if let Some(media) = message.get("media").filter(|media| media.is_array()) {
        let media = canonical_media(media);
        if media.as_array().is_some_and(|entries| !entries.is_empty()) {
            saved.insert("media".into(), media);
        }
    }
    // The archive is a path rather than a media descriptor, so regular
    // provider replay never reads the whole render into the request.
    if let Some(render) = message
        .get("source_render")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty() && path.chars().count() <= 4096)
    {
        saved.insert("source_render".into(), json!(render));
    }
    if message["source_render_unavailable"].as_bool() == Some(true) {
        saved.insert("source_render_unavailable".into(), json!(true));
    }
    saved.insert("content".into(), json!(content));
    if let Some(context) = message
        .get("context")
        .filter(|value| is_context_marker(value))
    {
        saved.insert("context".into(), context.clone());
    }
    // The snapshot a message was sent against. Restoring the document from the
    // row needs this after a reload, and the sweep treats it as a reference to
    // the version file, so dropping it here deletes that file.
    if let Some(sha) = message["dsl_sha"]
        .as_str()
        .filter(|sha| sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()))
    {
        saved.insert("dsl_sha".into(), json!(sha));
    }
    if let Some(project) = message["dsl_project"]
        .as_str()
        .filter(|path| !path.is_empty() && path.chars().count() <= MAX_PROJECT_PATH)
    {
        saved.insert("dsl_project".into(), json!(project));
    }
    // A person's edit-history position, kept beside the snapshot it belongs to.
    // Only a person's message records one; any other row or shape is dropped.
    if role == "user"
        && message_protocol::kind(message) == Some(MessageKind::User)
        && let Some(history) = message.get("history_at").and_then(HistoryAt::from_saved)
        && let Ok(value) = serde_json::to_value(&history)
    {
        saved.insert("history_at".into(), value);
    }
    // The file holding the full result behind a truncated preview. The sweep
    // reads the hash as that file's owner, so dropping it here deletes it.
    if let Some(sha) = message[tool_results::RESULT_SHA]
        .as_str()
        .filter(|sha| tool_results::is_hash(sha))
    {
        saved.insert(tool_results::RESULT_SHA.into(), json!(sha));
    }
    // Typed edit facts describe the tool row in the panel. They stay local:
    // the provider sees only the tool result content.
    if let Some(changes) = message.get("changes") {
        saved.insert("changes".into(), changes.clone());
    }
    // The signed reasoning of an assistant turn is provider state: a restored
    // conversation that dropped it would replay an unsigned tool round.
    for key in ["tool_calls", "tool_call_id", "provider_state", "thinking"] {
        if let Some(value) = message.get(key) {
            saved.insert(key.into(), value.clone());
        }
    }
    Some(Value::Object(saved))
}

/// The stored form of one message's attachments: canonical descriptors,
/// with a malformed entry dropped rather than failing the whole turn.
pub(super) fn canonical_media(media: &Value) -> Value {
    let entries = media
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| super::super::media::from_saved(entry).ok())
                .map(|(kind, reference)| super::super::media::to_wire(kind, &reference))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Value::Array(entries)
}

/// Rewrites every saved attachment into the canonical shape on read, so a file
/// an older build wrote cannot fail a turn or hide a restored attachment.
pub(super) fn canonicalize_media(messages: &mut [Value]) {
    for message in messages {
        let canonical = match message.get("media").filter(|media| media.is_array()) {
            Some(media) => canonical_media(media),
            None => continue,
        };
        if canonical
            .as_array()
            .is_some_and(|entries| entries.is_empty())
        {
            if let Some(object) = message.as_object_mut() {
                object.remove("media");
            }
        } else if let Some(slot) = message.get_mut("media") {
            *slot = canonical;
        }
    }
}

/// Adds `missing` to a saved attachment whose file is gone, so a restored
/// transcript shows the state before the next request is built. A render whose
/// file the archive evicted keeps its path and is marked `source_render_missing`;
/// nothing resolves a render during a request, so the turn still builds.
pub(super) fn mark_missing_media(messages: &mut [Value]) {
    for message in messages {
        let render_missing = message
            .get("source_render")
            .and_then(Value::as_str)
            .is_some_and(|path| !path.is_empty() && !Path::new(path).is_file());
        if render_missing {
            message["source_render_missing"] = json!(true);
        }
        let Some(media) = message.get_mut("media").and_then(Value::as_array_mut) else {
            continue;
        };
        for entry in media {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !path.is_empty() && !Path::new(path).is_file() {
                entry["missing"] = json!(true);
            }
        }
    }
}

/// Matches normalized prose and tool identity while ignoring provider metadata.
pub(super) fn same_turn(left: &Value, right: &Value) -> bool {
    let calls = |message: &Value| -> Vec<Value> {
        message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|call| {
                json!([
                    call["id"],
                    call["function"]["name"],
                    call["function"]["arguments"]
                ])
            })
            .collect()
    };
    left["role"] == right["role"]
        && message_protocol::for_new(left).map(|meta| meta.kind)
            == message_protocol::for_new(right).map(|meta| meta.kind)
        && left["content"].as_str().unwrap_or_default()
            == right["content"].as_str().unwrap_or_default()
        && left["tool_call_id"] == right["tool_call_id"]
        && calls(left) == calls(right)
        && left["source_render"] == right["source_render"]
        && canonical_media(&left["media"]) == canonical_media(&right["media"])
        && (!(left.get("message_meta").is_some() && message_protocol::read(left).is_none()
            || right.get("message_meta").is_some() && message_protocol::read(right).is_none())
            || left["message_meta"] == right["message_meta"])
}

/// The replay-free form of a prose message. A person's message keeps what is
/// theirs, verbatim: the text, the attachments as canonical descriptors, the
/// snapshot and history position a recall uses (`dsl_sha`, `dsl_project`,
/// `history_at`) and the steering id. Calls, provider state and inline audio
/// never survive.
pub(super) fn plain_message(message: &Value) -> Option<Value> {
    let role = message["role"].as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if message.get("tool_calls").is_some()
        || message.get("provider_state").is_some()
        || message.get("input_audio").is_some()
        || context_of(message).is_some()
    {
        return None;
    }
    let content = message["content"].as_str()?;
    let kind = message_protocol::kind(message)?;
    let media = match message.get("media") {
        Some(media) if role == "user" => canonical_media(media),
        Some(_) => return None,
        None => Value::Array(Vec::new()),
    };
    let has_media = media.as_array().is_some_and(|entries| !entries.is_empty());
    if (content.trim().is_empty() && !has_media)
        || !matches!(kind, MessageKind::User | MessageKind::Assistant)
    {
        return None;
    }
    let mut plain = json!({ "role": role, "content": content });
    let mut meta = MessageMeta::new(kind);
    if kind == MessageKind::User {
        meta.steer_id = message_protocol::read(message).and_then(|meta| meta.steer_id);
        if has_media {
            plain["media"] = media;
        }
        for key in ["dsl_sha", "dsl_project"] {
            if let Some(value) = message.get(key).filter(|value| value.is_string()) {
                plain[key] = value.clone();
            }
        }
        if let Some(value) = message
            .get("history_at")
            .and_then(HistoryAt::from_saved)
            .and_then(|history| serde_json::to_value(history).ok())
        {
            plain["history_at"] = value;
        }
    }
    message_protocol::tag(&mut plain, meta);
    Some(plain)
}

/// Compacted messages retain prose, validated file descriptors and a person's
/// snapshot and history position, never tool calls, provider state or
/// signatures. Audio content remains available on disk.
pub fn strips_replay_state(messages: &[Value]) -> bool {
    messages.iter().all(|message| {
        message.as_object().is_some_and(|object| {
            object.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "role"
                        | "content"
                        | "media"
                        | "context"
                        | "source_render"
                        | "message_meta"
                        | "dsl_sha"
                        | "dsl_project"
                        | "history_at"
                )
            }) && object
                .get("message_meta")
                .is_none_or(|_| message_protocol::read(message).is_some())
                && message_protocol::text_call(message).is_none()
                && ["dsl_sha", "dsl_project", "history_at"].iter().all(|key| {
                    object
                        .get(*key)
                        .is_none_or(|_| message_protocol::kind(message) == Some(MessageKind::User))
                })
                && object.get("context").is_none_or(is_context_marker)
                && object
                    .get("source_render")
                    .is_none_or(|_| super::audition_render_reference(message).is_some())
                && object
                    .get("media")
                    .is_none_or(|media| !canonical_media(media).as_array().unwrap().is_empty())
                && object.get("content").and_then(Value::as_str).is_some()
                && matches!(
                    object.get("role").and_then(Value::as_str),
                    Some("user") | Some("assistant")
                )
        })
    })
}

/// A known host-context marker. A row carrying one is never user prose.
pub(super) fn is_context_marker(value: &Value) -> bool {
    matches!(value.as_str(), Some(CONTEXT_AUDITION | CONTEXT_SUMMARY))
}

pub(super) fn context_of(message: &Value) -> Option<&str> {
    message.get("context").and_then(Value::as_str)
}
