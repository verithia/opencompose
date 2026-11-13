//! The saved conversation as the agent loop writes it: one message at a time,
//! in order, with every tool call answered exactly once.
//!
//! The loop owns the transcript. It appends the user message, each assistant
//! round, each tool result as soon as the call returns, and the deliveries that
//! follow a round. Nothing is matched by text: the loop says what it appends
//! and the store checks that the result is still a well-formed conversation.
//!
//! A call that started but never returned is the one outcome the store cannot
//! know. The loop marks a call as started before it dispatches it, so a crash
//! leaves a marker; the next admission answers that call with an "outcome
//! unknown" result and every call that never started with "not run".

use super::message_protocol::{self, MessageKind, MessageMeta};
use super::{Store, VERSION, append_records, now, read_view, saved_message, write_meta};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

/// The file that names the calls a running loop has dispatched but not yet
/// answered. One line per call; removed when the round is closed.
const STARTED_FILE: &str = "started.json";
/// Calls one round may hold, mirrored from the provider bound.
const MAX_STARTED: usize = crate::ai::MAX_TOOL_CALLS_PER_BATCH;

/// Why a call has no result of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unanswered {
    /// The loop stopped before it dispatched the call; nothing ran.
    NotRun,
    /// The call was dispatched and its result was lost; it may have changed
    /// the project. The model is told to inspect before repeating it.
    OutcomeUnknown,
}

impl Unanswered {
    /// The result the model reads for a call in this state.
    pub fn result(self, tool: &str) -> Value {
        match self {
            Unanswered::NotRun => json!({
                "error": format!("not run: the turn stopped before {tool} was dispatched; nothing changed"),
                "not_run": true,
            }),
            Unanswered::OutcomeUnknown => json!({
                "error": format!(
                    "outcome unknown: {tool} was dispatched but its result was lost; inspect the project before repeating it"
                ),
                "outcome_unknown": true,
            }),
        }
    }
}

/// A structural defect of a saved conversation. Only well-formed
/// conversations are written, so these are refusals of an append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    /// A tool result names a call no earlier assistant message opened.
    OrphanResult(String),
    /// A call id answered twice.
    DuplicateResult(String),
    /// Two calls share an id.
    DuplicateCall(String),
    /// A non-result message arrived while calls of the last round were open.
    OpenRound(Vec<String>),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairingError::OrphanResult(id) => write!(formatter, "tool result {id} has no call"),
            PairingError::DuplicateResult(id) => {
                write!(formatter, "tool call {id} is answered twice")
            }
            PairingError::DuplicateCall(id) => write!(formatter, "tool call id {id} is used twice"),
            PairingError::OpenRound(ids) => write!(
                formatter,
                "a message follows calls that have no result yet: {}",
                ids.join(", ")
            ),
        }
    }
}

/// The calls of the last round that have no result yet, in call order.
pub fn open_calls(view: &[Value]) -> Vec<(String, String)> {
    let Some(start) = view.iter().rposition(has_calls) else {
        return Vec::new();
    };
    let answered: HashSet<&str> = view[start + 1..]
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| message["tool_call_id"].as_str())
        .collect();
    calls_of(&view[start])
        .into_iter()
        .filter(|(id, _)| !answered.contains(id.as_str()))
        .collect()
}

/// Checks the invariant every saved conversation keeps: each call is answered
/// exactly once, by a result that follows it, before any other kind of message
/// except host deliveries of the same round. The last round may still be open.
pub fn check_pairing(view: &[Value]) -> Result<(), PairingError> {
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut answered: HashSet<String> = HashSet::new();
    let mut open: Vec<String> = Vec::new();
    for message in view {
        if message["role"] == "tool" {
            let id = message["tool_call_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if !seen_calls.contains(&id) {
                return Err(PairingError::OrphanResult(id));
            }
            if !answered.insert(id.clone()) {
                return Err(PairingError::DuplicateResult(id));
            }
            open.retain(|call| call != &id);
            continue;
        }
        // A delivery the round produced (audio, a comparison clip) may follow
        // its results; it never closes or opens calls.
        if is_round_delivery(message) && open.is_empty() {
            continue;
        }
        if !open.is_empty() {
            return Err(PairingError::OpenRound(open));
        }
        for (id, _) in calls_of(message) {
            if !seen_calls.insert(id.clone()) {
                return Err(PairingError::DuplicateCall(id));
            }
            open.push(id);
        }
    }
    Ok(())
}

fn has_calls(message: &Value) -> bool {
    message["role"] == "assistant"
        && message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
}

fn calls_of(message: &Value) -> Vec<(String, String)> {
    if message["role"] != "assistant" {
        return Vec::new();
    }
    message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|call| {
            Some((
                call["id"].as_str()?.to_owned(),
                call["function"]["name"]
                    .as_str()
                    .unwrap_or("the tool")
                    .to_owned(),
            ))
        })
        .collect()
}

/// Host context the loop attaches after a round's results.
fn is_round_delivery(message: &Value) -> bool {
    message["role"] == "user"
        && matches!(
            message_protocol::kind(message),
            Some(
                MessageKind::Audition
                    | MessageKind::ComparisonReference
                    | MessageKind::ComparisonCandidate
                    | MessageKind::HostNotice
                    | MessageKind::RoundBudget
                    | MessageKind::ProposalOutcome
            )
        )
}

/// Answers every open call of the last round. Calls named in the started file
/// get "outcome unknown"; the rest were never dispatched.
pub fn close_open(view: &[Value], started: &HashSet<String>) -> Vec<Value> {
    open_calls(view)
        .into_iter()
        .map(|(id, tool)| {
            let state = if started.contains(&id) {
                Unanswered::OutcomeUnknown
            } else {
                Unanswered::NotRun
            };
            let mut message = json!({
                "role": "tool",
                "tool_call_id": id,
                "content": state.result(&tool).to_string(),
            });
            message_protocol::tag(&mut message, MessageMeta::new(MessageKind::Tool));
            message
        })
        .collect()
}

/// The calls of the open round that were dispatched, so a closing caller
/// can tell a lost result from a call that never ran.
pub fn started_in(dir: &Path) -> HashSet<String> {
    read_started(dir)
}

/// Forgets every started marker, once the round they belong to is closed.
pub fn clear_started(dir: &Path) -> Result<(), String> {
    write_started(dir, &HashSet::new())
}

fn read_started(dir: &Path) -> HashSet<String> {
    std::fs::read(dir.join(STARTED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .map(|ids| ids.into_iter().take(MAX_STARTED).collect())
        .unwrap_or_default()
}

fn write_started(dir: &Path, started: &HashSet<String>) -> Result<(), String> {
    let path = dir.join(STARTED_FILE);
    if started.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("sessions: {error}")),
        };
    }
    let mut ids: Vec<&String> = started.iter().collect();
    ids.sort();
    let bytes = serde_json::to_vec(&ids).map_err(|error| format!("sessions: {error}"))?;
    crate::private_fs::write_atomic(&path, &bytes).map_err(|error| format!("sessions: {error}"))
}

/// Where a revert cuts: the index of a user message and the snapshot that was
/// taken just before it was sent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RevertPoint {
    pub keep: usize,
    pub dsl_sha: String,
}

impl Store {
    /// Appends messages the loop produced, in order. The append is refused,
    /// and nothing is written, when it would break call pairing.
    pub fn append_turn(
        &self,
        id: &str,
        messages: &[Value],
        incarnation: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        super::super::session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        let (mut view, _) = read_view(&dir)?;
        // A delivered clip is archived by content and saved as a reference, so
        // a reload replays the audio the model heard.
        let mut archived = messages.to_vec();
        let settings = crate::gui_settings::load().session_audio;
        super::persist_auditions(&self.root, &mut archived, &settings)?;
        let added: Vec<Value> = archived.iter().filter_map(saved_message).collect();
        if added.is_empty() {
            return Ok(added);
        }
        let start = view.len();
        view.extend(added.iter().cloned());
        check_pairing(&view).map_err(|error| format!("sessions: {error}"))?;
        // A result answers a started call: the marker for it is spent.
        let mut started = read_started(&dir);
        let before = started.len();
        for message in &view[start..] {
            if let Some(call) = message["tool_call_id"].as_str() {
                started.remove(call);
            }
        }
        let record = super::turn_record(&now(), added);
        let saved = record.messages.clone();
        append_records(&dir, &[record])?;
        if started.len() != before {
            write_started(&dir, &started)?;
        }
        let (view, _) = read_view(&dir)?;
        meta.tokens = super::estimate_tokens(&view);
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(saved)
    }

    /// Records that a call is about to be dispatched. A loop that dies before
    /// the result is saved leaves this marker, so the call is reported as
    /// "outcome unknown" instead of "not run".
    pub fn mark_started(&self, id: &str, call: &str) -> Result<(), String> {
        let _lock = self.mutation_lock()?;
        let (dir, _) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        if !open_calls(&view).iter().any(|(open, _)| open == call) {
            return Err(format!("sessions: call {call} is not open"));
        }
        let mut started = read_started(&dir);
        started.insert(call.to_owned());
        write_started(&dir, &started)
    }

    /// Closes a round a stopped or dead loop left open, so the conversation
    /// can take its next message. Returns the results it wrote.
    pub fn close_round(&self, id: &str) -> Result<Vec<Value>, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        self.close_round_locked(&dir, &mut meta)
    }

    fn close_round_locked(
        &self,
        dir: &Path,
        meta: &mut super::SessionMeta,
    ) -> Result<Vec<Value>, String> {
        let (view, _) = read_view(dir)?;
        let started = read_started(dir);
        let closed = close_open(&view, &started);
        if !closed.is_empty() {
            append_records(dir, &[super::turn_record(&now(), closed.clone())])?;
            let (view, _) = read_view(dir)?;
            meta.tokens = super::estimate_tokens(&view);
            meta.updated = now();
            write_meta(dir, meta)?;
        }
        write_started(dir, &HashSet::new())?;
        Ok(closed)
    }

    /// The user message a revert targets, checked against the saved
    /// conversation: it must be a person's message that carries a snapshot.
    pub fn revert_point(&self, id: &str, index: usize) -> Result<RevertPoint, String> {
        let (dir, _) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        revert_point_in(&view, index)
    }

    /// Cuts the conversation just before the user message at `index`. The cut
    /// lands on a user message, so it never separates a call from its result,
    /// and the compaction history behind the view is kept.
    pub fn cut_before_user(&self, id: &str, index: usize) -> Result<usize, String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let (view, _) = read_view(&dir)?;
        revert_point_in(&view, index)?;
        let kept: Vec<Value> = view[..index].to_vec();
        // A trim record replaces the view and keeps the turns before it on
        // disk, as a compaction does, so the original history stays readable.
        let record = super::Record {
            v: VERSION,
            kind: super::RecordKind::Trim,
            at: now(),
            dropped: Some(super::Dropped {
                from: index,
                to: view.len(),
            }),
            tokens_before: Some(super::estimate_tokens(&view)),
            tokens_after: Some(super::estimate_tokens(&kept)),
            truncated: None,
            messages: kept.clone(),
        };
        append_records(&dir, &[record])?;
        write_started(&dir, &HashSet::new())?;
        meta.tokens = super::estimate_tokens(&kept);
        meta.updated = now();
        write_meta(&dir, &meta)?;
        Ok(kept.len())
    }
}

fn revert_point_in(view: &[Value], index: usize) -> Result<RevertPoint, String> {
    let message = view
        .get(index)
        .ok_or_else(|| format!("No message {index} in this conversation"))?;
    if message["role"] != "user" || message_protocol::kind(message) != Some(MessageKind::User) {
        return Err("Only a message you sent can be reverted to".into());
    }
    let dsl_sha = message["dsl_sha"]
        .as_str()
        .ok_or("This message has no project snapshot to restore")?
        .to_owned();
    Ok(RevertPoint {
        keep: index,
        dsl_sha,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
    }
    fn round(ids: &[&str]) -> Value {
        json!({
            "role":"assistant","content":null,
            "tool_calls": ids.iter().map(|id| json!({
                "id": id, "type":"function",
                "function":{"name":"tracks_add","arguments":"{}"}
            })).collect::<Vec<_>>()
        })
    }
    fn result(id: &str) -> Value {
        json!({"role":"tool","tool_call_id":id,"content":"{\"ok\":true}"})
    }

    fn fixture() -> (tempfile::TempDir, Store, String) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let meta = store.create(None, None, Some("Fixture"), None).unwrap();
        (root, store, meta.id)
    }

    #[test]
    fn preparation_after_a_crash_tells_a_lost_result_from_a_call_never_run() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round(&["a", "b"])], None)
            .unwrap();
        store.mark_started(&id, "a").unwrap();
        // The process dies here; the next turn's preparation closes the round.
        let facts = super::super::ProjectFacts::default();
        let prepared = store
            .prepare(
                &id,
                &[user("again")],
                None,
                super::super::PrepareContext {
                    snapshot: None,
                    overhead: 0,
                    context_length: 0,
                    facts: &facts,
                    provider: "p",
                    model: "m",
                },
            )
            .unwrap();
        let result = |call: &str| {
            prepared
                .messages
                .iter()
                .find(|message| message["tool_call_id"] == call)
                .and_then(|message| message["content"].as_str())
                .unwrap()
                .to_owned()
        };
        assert!(result("a").contains("outcome unknown"), "{}", result("a"));
        assert!(result("b").contains("not run"), "{}", result("b"));
        assert_eq!(check_pairing(&store.view(&id).unwrap()), Ok(()));
    }

    #[test]
    fn pairing_accepts_well_formed_rounds_and_an_open_last_round() {
        let view = vec![
            user("go"),
            round(&["a", "b"]),
            result("a"),
            result("b"),
            round(&["c"]),
        ];
        assert_eq!(check_pairing(&view), Ok(()));
        assert_eq!(open_calls(&view), vec![("c".into(), "tracks_add".into())]);
    }

    #[test]
    fn pairing_refuses_every_structural_defect() {
        assert_eq!(
            check_pairing(&[user("go"), result("x")]),
            Err(PairingError::OrphanResult("x".into()))
        );
        assert_eq!(
            check_pairing(&[round(&["a"]), result("a"), result("a")]),
            Err(PairingError::DuplicateResult("a".into()))
        );
        assert_eq!(
            check_pairing(&[round(&["a"]), result("a"), round(&["a"])]),
            Err(PairingError::DuplicateCall("a".into()))
        );
        assert_eq!(
            check_pairing(&[round(&["a", "b"]), result("a"), user("next")]),
            Err(PairingError::OpenRound(vec!["b".into()]))
        );
    }

    /// Every interleaving the loop can write, stopped at every point and then
    /// closed, is a well-formed conversation with each call answered once.
    #[test]
    fn any_stop_point_closes_into_a_well_formed_conversation() {
        let rounds: [&[&str]; 3] = [&["a1", "a2", "a3"], &["b1"], &["c1", "c2"]];
        let mut script = vec![user("go")];
        for calls in rounds {
            script.push(round(calls));
            script.extend(calls.iter().map(|id| result(id)));
        }
        script.push(json!({"role":"assistant","content":"done"}));
        for stop in 1..=script.len() {
            for started_mask in 0..4u8 {
                let mut view = script[..stop].to_vec();
                let open: Vec<String> = open_calls(&view).into_iter().map(|(id, _)| id).collect();
                let started: HashSet<String> = open
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| started_mask & (1 << (index % 2)) != 0)
                    .map(|(_, id)| id.clone())
                    .collect();
                let closed = close_open(&view, &started);
                for message in &closed {
                    let id = message["tool_call_id"].as_str().unwrap();
                    let body: Value =
                        serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
                    let unknown = body["outcome_unknown"] == true;
                    assert_eq!(unknown, started.contains(id), "stop {stop} call {id}");
                }
                view.extend(closed);
                view.push(user("next"));
                assert_eq!(
                    check_pairing(&view),
                    Ok(()),
                    "stop {stop} mask {started_mask}"
                );
                assert!(open_calls(&view).is_empty());
            }
        }
    }

    #[test]
    fn a_refused_append_writes_nothing() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round(&["a"])], None)
            .unwrap();
        let before = store.view(&id).unwrap();
        assert!(store.append_turn(&id, &[user("too early")], None).is_err());
        assert!(store.append_turn(&id, &[result("zzz")], None).is_err());
        assert_eq!(store.view(&id).unwrap(), before);
    }

    #[test]
    fn a_started_call_that_lost_its_result_reads_as_unknown_after_restart() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round(&["a", "b", "c"])], None)
            .unwrap();
        store.mark_started(&id, "a").unwrap();
        store.append_turn(&id, &[result("a")], None).unwrap();
        store.mark_started(&id, "b").unwrap();
        // The loop dies here: b was dispatched, c never was.
        let reopened = Store::new(store.root().to_owned());
        let closed = reopened.close_round(&id).unwrap();
        let states: Vec<(String, bool, bool)> = closed
            .iter()
            .map(|message| {
                let body: Value =
                    serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
                (
                    message["tool_call_id"].as_str().unwrap().to_owned(),
                    body["outcome_unknown"] == true,
                    body["not_run"] == true,
                )
            })
            .collect();
        assert_eq!(
            states,
            vec![("b".into(), true, false), ("c".into(), false, true)]
        );
        let view = reopened.view(&id).unwrap();
        assert_eq!(
            view[2],
            saved_message(&result("a")).unwrap(),
            "the real result stays"
        );
        assert_eq!(check_pairing(&view), Ok(()));
        assert!(
            reopened.close_round(&id).unwrap().is_empty(),
            "closing twice adds nothing"
        );
    }

    #[test]
    fn a_marker_names_only_an_open_call() {
        let (_root, store, id) = fixture();
        store
            .append_turn(&id, &[user("go"), round(&["a"])], None)
            .unwrap();
        assert!(store.mark_started(&id, "nope").is_err());
        store.append_turn(&id, &[result("a")], None).unwrap();
        assert!(
            store.mark_started(&id, "a").is_err(),
            "an answered call is closed"
        );
    }

    #[test]
    fn a_cut_lands_only_before_a_person_s_message_and_keeps_the_original() {
        let (_root, store, id) = fixture();
        let mut first = user("first");
        first["dsl_sha"] = json!("a".repeat(64));
        let mut second = user("second");
        second["dsl_sha"] = json!("b".repeat(64));
        store
            .append_turn(
                &id,
                &[
                    first,
                    round(&["a"]),
                    result("a"),
                    json!({"role":"assistant","content":"done"}),
                    second,
                    json!({"role":"assistant","content":"ok"}),
                ],
                None,
            )
            .unwrap();
        assert!(
            store.revert_point(&id, 2).is_err(),
            "a tool result is not a revert point"
        );
        assert!(
            store.revert_point(&id, 1).is_err(),
            "an assistant round is not one either"
        );
        let point = store.revert_point(&id, 4).unwrap();
        assert_eq!(point.dsl_sha, "b".repeat(64));
        assert_eq!(store.cut_before_user(&id, 4).unwrap(), 4);
        let view = store.view(&id).unwrap();
        assert_eq!(view.len(), 4);
        assert_eq!(check_pairing(&view), Ok(()));
        let (original, _) = store.original(&id).unwrap();
        assert_eq!(original.len(), 6, "the cut turns stay readable on disk");
    }
}
