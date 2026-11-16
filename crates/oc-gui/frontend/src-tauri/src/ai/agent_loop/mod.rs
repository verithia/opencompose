//! The agent turn: provider rounds, tool dispatch and the saved conversation,
//! run by the backend for the desktop panel and for headless callers alike.
//!
//! The loop holds the wire it sends and is the only writer of the turn's
//! saved messages. Every message is recorded the moment it exists: the
//! assistant round before its calls run, each result as its call returns, the
//! round's deliveries after the last result. A native call is marked as
//! started just before it is dispatched. So whatever stops a turn (a person,
//! the round cap, a provider failure or a crash), each completed call keeps
//! its real result, a call that was running reads "outcome unknown", and one
//! that never started reads "not run". This holds for text-protocol calls
//! too: their id travels in the saved messages' metadata. No stop undoes an
//! applied edit: the turn's batch commits on every ending, and a person who
//! wants the project back reverts to before one of their own messages.
//!
//! The loop knows nothing about Tauri. A [`Host`] supplies the provider, the
//! tools, the person (if any) and the store; the desktop and headless hosts
//! differ only in who answers an approval.

pub mod gate;
pub mod host;
pub mod results;
pub mod steering;
pub mod wire;

use crate::ai::sessions::tool_results::FULL_RESULT;
use crate::ai::sessions::turns::APPLICATION_RESULT_PREFIX;
use crate::ai::sessions::{CompactionFailure, MessageKind, SessionPermission, message_meta};
use crate::ai_service::AiError;
use gate::{Need, call_path};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::watch;
use wire::{Call, MAX_TOOL_ROUNDS, ROUNDS_LEFT_NOTICE};

/// The transaction tools; the loop owns the turn's batch, so it never nests one.
const TRANSACTION_TOOLS: [&str; 3] = [
    "transaction_begin",
    "transaction_commit",
    "transaction_rollback",
];
/// The panel-local pager over truncated results.
pub const READ_TOOLCALL: &str = "read_toolcall";
/// The panel-local goal and plan writer.
pub const PLAN_UPDATE: &str = "plan_update";

/// How a running turn is stopped. Cloned into every place that waits.
#[derive(Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Default for Cancel {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the turn is cancelled.
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

/// The document as the loop needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub dsl: String,
    pub revision: u64,
    pub track_count: usize,
}

/// An applied edit: the document it left and the typed facts of what it
/// changed, which the call's row shows.
pub struct Edited {
    pub document: Document,
    pub changes: Option<oc_dsl::change::ChangeSet>,
}

/// The audio a model can take in; `None` in [`TurnSpec`] means it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioShape {
    pub channels: u8,
    pub sample_rate: u32,
}

/// One DSL-modifying call waiting for a person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub args: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The subtree's DSL before the call, when a replacement names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The subtree's DSL a replacement would write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// A question the model asks a person, carrying no project change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserQuestion {
    pub call_id: String,
    pub question: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub free_text: bool,
}

/// A person's answer to [`UserQuestion`]. `option` indexes `options`; `text` is
/// the free-text answer when one was typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub option: Option<usize>,
    #[serde(default)]
    pub text: Option<String>,
    /// Nobody answered in time. Only the host sets it; a view cannot.
    #[serde(skip)]
    pub expired: bool,
}

impl Reply {
    /// The answer a question gets when its wait runs out.
    pub fn expired() -> Self {
        Self {
            expired: true,
            ..Self::default()
        }
    }
}

/// A question a running turn waits on, as a window that reloaded reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingQuestion {
    Approval {
        #[serde(flatten)]
        request: ApprovalRequest,
    },
    UserQuestion {
        #[serde(flatten)]
        request: UserQuestion,
    },
}

/// A person's answer. `target` is the object the card named, so a refusal
/// can say what was refused; `always` allows the tool for the rest of the
/// conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Decision {
    pub approved: bool,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub always: bool,
    /// Nobody answered in time. Only the host sets it; a view cannot.
    #[serde(skip)]
    pub expired: bool,
}

impl Decision {
    /// The refusal a question gets when its wait runs out.
    pub fn expired() -> Self {
        Self {
            expired: true,
            ..Self::default()
        }
    }
}

/// What a view hears while a turn runs. A saved message is sent in its saved
/// form, so a live transcript and a reloaded one render the same rows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// A message the turn just saved.
    Message { message: Value },
    /// A call is about to run.
    Running { call_id: String, tool: String },
    /// A call waits for a person; answered with `ai_turn_answer`.
    Ask { request: ApprovalRequest },
    /// The model asks the person a question; answered with `ai_turn_reply`.
    AskUser { request: UserQuestion },
    /// A question's wait ran out; the call was refused and its card is gone.
    Expired { call_id: String },
    /// A replacement the loop is about to apply under `autoreview`.
    Preview {
        call_id: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        before: String,
        after: String,
    },
    /// The request grew past the provider bounds, so its oldest tool pairs
    /// were folded into one summary row before the send.
    WireFolded {
        before_rows: usize,
        before_bytes: usize,
        after_rows: usize,
        after_bytes: usize,
        /// Tool pairs folded into the summary, and pairs kept whole.
        folded: usize,
        kept: usize,
    },
    /// Attachments the provider layer found changed or missing.
    Media { notices: Value },
}

/// Which step of closing a turn's batch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitFailure {
    /// The batch did not commit; it stays open.
    Commit,
    /// The batch committed; the project file was not written.
    Save,
}

/// A failed batch close: the step and the host's reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitError {
    pub failure: CommitFailure,
    pub message: String,
}

impl CommitError {
    pub fn commit(message: impl Into<String>) -> Self {
        Self {
            failure: CommitFailure::Commit,
            message: message.into(),
        }
    }

    pub fn save(message: impl Into<String>) -> Self {
        Self {
            failure: CommitFailure::Save,
            message: message.into(),
        }
    }
}

/// Facts about a turn that stopped early. The loop reports numbers; a view
/// owns the prose and its translation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TurnStop {
    /// A person cancelled the turn; otherwise the loop stopped it.
    pub cancelled: bool,
    /// Calls that returned, and calls the model asked for.
    pub done: usize,
    pub planned: usize,
    /// Calls the stop left unstarted; each reads "not run" in the transcript.
    pub not_run: usize,
    /// Successful edits in the turn's batch.
    pub edits: usize,
    /// The batch committed where the turn stopped; the edits stayed.
    pub committed: bool,
    pub timed_out: bool,
    /// A failure ended the turn after it had edited; the edits stayed.
    pub failure_kept: bool,
    /// Why the batch did not end in the project file, in the host's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_error: Option<String>,
    /// Which step failed: a `save` failure committed the edits in memory and
    /// in undo, and only the file is behind; a `commit` failure left the
    /// batch open, and the next turn commits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_failure: Option<CommitFailure>,
    /// The call that was running when the cancel arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<String>,
}

/// How a turn ended. `failed` marks a turn that did not answer: `text` says
/// why in English for a headless caller; a view renders `stop` and `error`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TurnOutcome {
    pub text: String,
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<TurnStop>,
    /// The provider or store failure that ended the turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AiError>,
    /// The typed facts the turn's committed batch produced, described once
    /// from its base to its final document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<oc_dsl::change::ChangeSet>,
}

/// What one turn is allowed to do.
#[derive(Debug, Clone)]
pub struct TurnSpec {
    /// System prompt, the saved conversation and the new user message, as
    /// the host prepared them.
    pub wire: Vec<Value>,
    pub permission: SessionPermission,
    /// Label of the turn's one undo entry.
    pub label: String,
    pub audio: Option<AudioShape>,
    /// Scopes `read_toolcall` pages to this conversation.
    pub scope: String,
    /// The conversation's toolset selection; `None` offers the whole panel.
    pub toolsets: Option<Vec<String>>,
    /// The model calls tools by writing `{tool, args}` in its text, because
    /// it has no native tool calling. Off, a text reply is only an answer:
    /// a call quoted in prose never runs.
    pub text_protocol: bool,
    /// The model's input window in tokens; 0 when unknown. The loop folds
    /// the wire to stay inside it and refuses a send that cannot fit.
    pub context_length: u64,
    /// Output tokens the model may write, held back from the compaction
    /// trigger so a summary-sized answer still fits after the request.
    pub max_output: u64,
}

/// Everything outside the loop. Every method that touches the project or
/// the store is the same domain operation the panel and `/call` use.
pub trait Host: Sync {
    /// A running turn may accept user messages without interrupting its work.
    fn steering(&self) -> Option<&steering::Steering> {
        None
    }
    /// One provider round over exactly this wire.
    fn send(
        &self,
        wire: &[Value],
        round: usize,
    ) -> impl Future<Output = Result<Value, AiError>> + Send;
    /// Opens the turn's batch under the turn's own owner.
    fn begin_batch(&self, label: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Commits the turn's batch as one undo entry and writes the project file.
    fn commit_batch(&self) -> impl Future<Output = Result<(), CommitError>> + Send;
    /// The typed facts of the open batch, read before its commit.
    fn batch_changes(&self) -> impl Future<Output = Option<oc_dsl::change::ChangeSet>> + Send {
        std::future::ready(None)
    }
    /// Dispatches one registry tool. A refusal is the payload the model reads.
    fn call(
        &self,
        tool: &str,
        args: &Value,
        cancel: &Cancel,
    ) -> impl Future<Output = Result<Value, Value>> + Send;
    /// Asks the person; `None` when nobody is there to ask.
    fn ask(&self, request: ApprovalRequest) -> impl Future<Output = Option<Decision>> + Send;
    /// Asks the person a question; `None` when nobody is there to ask.
    fn ask_user(&self, _request: UserQuestion) -> impl Future<Output = Option<Reply>> + Send {
        async { None }
    }
    /// Tools this conversation answered "always allow" for, read per call.
    fn allowed(&self) -> Vec<String>;
    /// Appends messages to the saved conversation, refusing a broken pairing,
    /// and returns their saved form: that is what the next request sends.
    fn record(&self, messages: &[Value])
    -> impl Future<Output = Result<Vec<Value>, String>> + Send;
    /// Compacts the saved conversation mid-turn and returns the new saved
    /// view. The host plans, calls the model with no store lock, and commits
    /// one record; a failure leaves the saved conversation unchanged.
    fn compact(&self) -> impl Future<Output = Result<Vec<Value>, CompactionFailure>> + Send {
        std::future::ready(Err(CompactionFailure::ProviderError))
    }
    /// The full result behind the newest saved preview of `call`, read from
    /// the saved conversation when the in-memory cache does not hold it.
    fn stored_result(&self, _call: &str) -> impl Future<Output = Option<String>> + Send {
        std::future::ready(None)
    }
    /// Saves admitted human steering messages with their rewind snapshot.
    fn record_steering(
        &self,
        messages: &[Value],
    ) -> impl Future<Output = Result<Vec<Value>, String>> + Send {
        self.record(messages)
    }
    /// Marks a call as dispatched, before it runs.
    fn started(&self, call_id: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Answers the open calls of the last round and returns those results.
    fn close_round(
        &self,
    ) -> impl Future<Output = Result<Vec<crate::ai::sessions::turns::Closed>, String>> + Send;
    fn emit(&self, event: TurnEvent);
    /// The current document; a host with a view publishes it too.
    fn refresh(&self) -> impl Future<Output = Document> + Send;
    fn validate_edit(
        &self,
        path: &str,
        dsl: &str,
    ) -> impl Future<Output = Result<(), Vec<String>>> + Send;
    fn apply_edit(
        &self,
        path: &str,
        dsl: &str,
        expected: &str,
    ) -> impl Future<Output = Result<Edited, String>> + Send;
    /// A subtree's DSL, or an empty string when it is not there yet.
    fn subtree(&self, path: &str) -> impl Future<Output = String> + Send;
    /// The errors the document `after` has that `before` did not. `Err` means
    /// there is no baseline to compare against.
    fn introduced_errors(
        &self,
        before: &str,
        after: &str,
    ) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    /// Takes back the last edit as one undo step, only while the document is
    /// still at `written`: a change after the reviewed one owns the top entry.
    fn undo(&self, written: u64) -> impl Future<Output = Result<(), String>> + Send;
}

/// Why the rounds ended; the turn closes the same way for each.
enum Ending {
    Answer(String),
    Empty,
    Cancelled,
    Cap,
    Repeated(String),
    Failed(AiError),
}

/// The result of one call before it is shaped for the wire.
struct Ran {
    result: Value,
    failed: bool,
    declined: bool,
    mutates: bool,
    deferred: bool,
    /// Delivery rows the call produced after its result, such as the state
    /// rows a plan_update records for the round.
    rows: Vec<Value>,
}

struct Turn<'a, H: Host> {
    host: &'a H,
    spec: TurnSpec,
    cancel: Cancel,
    wire: Vec<Value>,
    expected: String,
    batch: bool,
    planned: usize,
    done: usize,
    edits: usize,
    previous: Vec<String>,
    last_action: String,
    in_flight: Option<String>,
    /// Unique per turn; names its text-protocol calls.
    nonce: String,
    /// The token budget, calibrated by each round's reported input, and
    /// where the turn's ask sits in the wire.
    budget: Option<wire::TokenBudget>,
    ask: Option<usize>,
    /// Leading system rows, which every resend keeps in front of the saved view.
    system_len: usize,
    /// Successful mid-turn compactions, bounded so a request that cannot
    /// shrink does not call the model again every round.
    mid_compactions: usize,
    /// The context-share thresholds this turn already reported.
    context_warned: [bool; 2],
}

/// How many times one turn may compact its saved conversation.
const MAX_MID_TURN_COMPACTIONS: usize = 2;

/// Runs one turn to its end. Never returns with the batch open unless its
/// commit failed, and never leaves a call of the last round unanswered.
pub async fn run_turn<H: Host>(host: &H, spec: TurnSpec, cancel: Cancel) -> TurnOutcome {
    let mut wire = spec.wire.clone();
    for message in &mut wire {
        if message.get("message_meta").is_none()
            && let Some(object) = message.as_object_mut()
            && matches!(
                object.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "tool")
            )
        {
            let kind = match object["role"].as_str() {
                Some("assistant") => MessageKind::Assistant,
                Some("tool") => MessageKind::Tool,
                _ if object.contains_key("context") => continue,
                _ => MessageKind::User,
            };
            object.insert("message_meta".into(), message_meta(kind));
        }
    }
    let expected = host.refresh().await.dsl;
    let system_len = wire
        .iter()
        .take_while(|message| message["role"] == "system")
        .count();
    let mut turn = Turn {
        host,
        spec,
        cancel,
        wire,
        expected,
        batch: false,
        planned: 0,
        done: 0,
        edits: 0,
        previous: Vec::new(),
        last_action: "none".into(),
        in_flight: None,
        nonce: turn_nonce(),
        budget: None,
        ask: None,
        system_len,
        mid_compactions: 0,
        context_warned: [false; 2],
    };
    turn.budget = wire::TokenBudget::new(
        turn.spec.context_length,
        wire::offered_tool_tokens(turn.spec.toolsets.as_deref(), true),
    )
    .map(|budget| {
        budget.for_compaction(
            turn.spec.max_output,
            crate::ai::compaction_prompt::prompt_tokens(),
        )
    });
    turn.ask = wire::turn_ask(&turn.wire);
    turn.refresh_state_context();
    let ending = match turn.rounds().await {
        Ok(ending) | Err(ending) => ending,
    };
    if let Some(inbox) = host.steering() {
        inbox.close();
    }
    turn.close(ending).await
}

impl<H: Host> Turn<'_, H> {
    async fn rounds(&mut self) -> Result<Ending, Ending> {
        for round in 0..MAX_TOOL_ROUNDS {
            self.stop_if_cancelled()?;
            self.deliver_steering(steering::Boundary::Continue).await?;
            self.compact_saved().await?;
            self.fit_wire()?;
            let reply = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Ending::Cancelled),
                reply = self.host.send(&self.wire, round) => reply,
            };
            let reply = match reply {
                Ok(reply) => {
                    self.observe_usage(&reply);
                    reply
                }
                Err(_) if self.cancel.is_cancelled() => return Err(Ending::Cancelled),
                Err(error) => return Err(Ending::Failed(error)),
            };
            let native = reply["tool_calls"].is_array();
            let calls: Vec<Call> = if native {
                let raw = reply["tool_calls"].as_array().cloned().unwrap_or_default();
                let calls: Vec<Call> = raw.iter().filter_map(Call::from_wire).collect();
                let mut ids: Vec<&str> = calls.iter().map(|call| call.id.as_str()).collect();
                ids.sort_unstable();
                ids.dedup();
                if calls.len() != raw.len() || ids.len() != calls.len() {
                    return Err(Ending::Failed(AiError::new(
                        "provider",
                        "Malformed tool call envelope",
                    )));
                }
                calls
            } else if self.spec.text_protocol {
                let id = wire::text_call_id(&self.nonce, round);
                wire::text_tool_call(reply["content"].as_str().unwrap_or_default(), &id)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            if calls.len() > crate::ai::MAX_TOOL_CALLS_PER_BATCH {
                return Err(Ending::Failed(AiError::new(
                    "provider",
                    format!(
                        "Tool batch has {} calls; limit is {}",
                        calls.len(),
                        crate::ai::MAX_TOOL_CALLS_PER_BATCH
                    ),
                )));
            }
            let assistant = assistant_message(
                &reply,
                native.then_some(&calls),
                calls.first().filter(|_| !native),
            );
            self.push(vec![assistant]).await?;
            if calls.is_empty() {
                self.stop_if_cancelled()?;
                // A message needs a round to be answered in. The last round has
                // none left, so its inbox is not drained: closing the turn
                // refuses what waits, and the caller still holds it.
                let rounds_left = round + 1 < MAX_TOOL_ROUNDS;
                if rounds_left && self.deliver_steering(steering::Boundary::Finish).await? {
                    continue;
                }
                let text = reply["content"].as_str().unwrap_or_default();
                return Ok(if text.trim().is_empty() {
                    Ending::Empty
                } else {
                    Ending::Answer(text.to_owned())
                });
            }
            // A reply that arrived after Stop is saved, and none of its calls run.
            self.stop_if_cancelled()?;
            self.planned += calls.len();
            let keys: Vec<String> = calls.iter().map(Call::key).collect();
            if let Some(repeated) = calls.iter().zip(&keys).find_map(|(call, key)| {
                (self.previous.contains(key) && crate::tool_registry::is_mutation_tool(&call.name))
                    .then_some(call)
            }) {
                let text = format!(
                    "the model repeated the same call twice ({}); stopping instead of looping",
                    repeated.key()
                );
                let message =
                    result_message(repeated, native, &json!({ "error": text }).to_string());
                self.push(vec![message]).await?;
                self.done += 1;
                return Err(Ending::Repeated(text));
            }
            self.previous = keys;
            let mut deliveries = Vec::new();
            for call in &calls {
                self.stop_if_cancelled()?;
                self.last_action = call.name.clone();
                self.host.emit(TurnEvent::Running {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                });
                let ran = self.run_call(call).await?;
                self.finish_call(call, native, ran, &mut deliveries).await?;
                if self.cancel.is_cancelled() {
                    self.in_flight = Some(call.name.clone());
                    return Err(Ending::Cancelled);
                }
            }
            let remaining = MAX_TOOL_ROUNDS - round - 1;
            deliveries.extend(self.budget_notices(remaining));
            if !deliveries.is_empty() {
                self.push(deliveries).await?;
                self.refresh_state_context();
            }
        }
        Ok(Ending::Cap)
    }

    /// Keeps the one derived goal-and-plan row in front of the conversation.
    /// It is rebuilt from the saved state rows after a round writes them, and
    /// it is never saved.
    fn refresh_state_context(&mut self) {
        let (goal, plan) = crate::ai::session_state::current_state(&self.wire);
        let row = crate::ai::session_state::context_row(goal.as_ref(), plan.as_deref());
        let context = crate::ai::session_state::CONTEXT_STATE;
        let existing = self
            .wire
            .iter()
            .position(|message| message["context"] == context);
        match (existing, row) {
            (Some(index), Some(row)) => self.wire[index] = row,
            (Some(index), None) => {
                self.wire.remove(index);
                if let Some(ask) = self.ask.as_mut()
                    && *ask > index
                {
                    *ask -= 1;
                }
            }
            (None, Some(row)) => {
                let head = self
                    .wire
                    .iter()
                    .take_while(|message| message["role"] == "system")
                    .count();
                self.wire.insert(head, row);
                if let Some(ask) = self.ask.as_mut()
                    && *ask >= head
                {
                    *ask += 1;
                }
            }
            (None, None) => {}
        }
    }

    async fn deliver_steering(&mut self, boundary: steering::Boundary) -> Result<bool, Ending> {
        let pending = self
            .host
            .steering()
            .map(|inbox| inbox.take(boundary))
            .unwrap_or_default();
        if pending.is_empty() {
            return Ok(false);
        }
        let messages: Vec<Value> = pending.iter().map(|row| row.message.clone()).collect();
        let result = self
            .host
            .record_steering(&messages)
            .await
            .map(|saved| self.extend_saved(saved));
        for row in pending {
            let _ = row.receipt.send(match &result {
                Ok(()) => Ok(()),
                Err(error) => Err(steering::Undelivered::Save(error.clone())),
            });
        }
        result.map_err(|error| Ending::Failed(AiError::new("session_save", error)))?;
        // A new instruction may intentionally revisit the previous edit.
        self.previous.clear();
        Ok(true)
    }

    fn stop_if_cancelled(&self) -> Result<(), Ending> {
        if self.cancel.is_cancelled() {
            Err(Ending::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Records messages, then extends the wire with their saved form. A
    /// message the store refused is not sent either, so the request and the
    /// saved conversation never diverge.
    async fn push(&mut self, messages: Vec<Value>) -> Result<(), Ending> {
        let saved = self
            .host
            .record(&messages)
            .await
            .map_err(|error| Ending::Failed(AiError::new("session_save", error)))?;
        self.extend_saved(saved);
        Ok(())
    }

    fn extend_saved(&mut self, saved: Vec<Value>) {
        for message in saved {
            self.host.emit(TurnEvent::Message {
                message: without_inline_bytes(&message),
            });
            self.wire.push(message);
        }
    }

    /// Asks the host to compact the saved conversation when the wire reaches
    /// the compaction threshold, then rebuilds the wire from the saved view.
    /// The open round stays verbatim because rounds are saved as they happen.
    async fn compact_saved(&mut self) -> Result<(), Ending> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        if self.mid_compactions >= MAX_MID_TURN_COMPACTIONS {
            return Ok(());
        }
        if budget.measure(&self.wire) <= budget.limit() {
            return Ok(());
        }
        let saved = match self.host.compact().await {
            Ok(saved) if !saved.is_empty() => saved,
            // A failed compaction leaves the saved conversation unchanged; the
            // fold below still keeps the request inside the provider bounds.
            Ok(_) | Err(_) => return Ok(()),
        };
        let mut wire = self.wire[..self.system_len].to_vec();
        wire.extend(saved);
        self.wire = wire;
        self.ask = wire::turn_ask(&self.wire);
        self.mid_compactions += 1;
        Ok(())
    }

    /// Keeps the request under the provider bounds by folding the oldest
    /// rows into one summary row: the row and byte bounds the request
    /// validator enforces, and the model's window measured as the larger of
    /// the estimate and what the provider reported for the last round.
    fn fit_wire(&mut self) -> Result<(), Ending> {
        let limits = wire::FoldLimits::round(self.budget);
        if limits.fits(&self.wire) {
            return Ok(());
        }
        let (rows, bytes) = wire::wire_budget(&self.wire);
        // A window no cut can reach still leaves the row and byte bounds to
        // meet, so the shape-only fold is the fallback.
        let fold = wire::fold_wire(&mut self.wire, &limits, self.ask)
            .or_else(|| wire::fold_wire(&mut self.wire, &wire::FoldLimits::round(None), self.ask));
        if let Some(tokens) = self.budget
            && tokens.measure(&self.wire) > tokens.window
        {
            return Err(Ending::Failed(AiError::new(
                "context_overflow",
                format!(
                    "the request is about {} tokens and the model's window is {}; folding it cannot make it fit",
                    tokens.measure(&self.wire),
                    tokens.window
                ),
            )));
        }
        let Some(fold) = fold else {
            if rows <= crate::ai::MAX_WIRE_MESSAGES && bytes <= crate::ai::LIMIT {
                return Ok(());
            }
            return Err(Ending::Failed(AiError::new(
                "config",
                format!(
                    "the request wire is {rows} messages and {bytes} bytes; folding it cannot bring it under the provider's limits"
                ),
            )));
        };
        self.ask = fold.ask;
        let (after_rows, after_bytes) = wire::wire_budget(&self.wire);
        self.host.emit(TurnEvent::WireFolded {
            before_rows: rows,
            before_bytes: bytes,
            after_rows,
            after_bytes,
            folded: fold.dropped,
            kept: fold.pairs,
        });
        Ok(())
    }

    /// Calibrates the token budget from the input the provider reported for
    /// the wire it was just sent, so the next fold measures what it counts.
    fn observe_usage(&mut self, reply: &Value) {
        if let Some(budget) = self.budget.as_mut()
            && let Some(input) = reply["usage"]["input"].as_u64()
        {
            budget.calibrate(&self.wire, input);
        }
    }

    /// Opens the turn's batch before its first edit.
    async fn open_batch(&mut self) -> Result<(), Value> {
        if self.batch {
            return Ok(());
        }
        self.host
            .begin_batch(&self.spec.label)
            .await
            .map_err(|error| crate::ai::skill_hints::refusal(&error))?;
        self.batch = true;
        Ok(())
    }

    async fn run_call(&mut self, call: &Call) -> Result<Ran, Ending> {
        let name = call.name.as_str();
        let gate = gate::gate(name, self.spec.permission, &self.host.allowed());
        let transaction = TRANSACTION_TOOLS.contains(&name);
        let mut ran = Ran {
            result: Value::Null,
            failed: false,
            declined: false,
            mutates: gate.modifies,
            deferred: false,
            rows: Vec::new(),
        };
        let args = match call.args() {
            Ok(args) => args,
            Err(error) => {
                ran.result = json!({ "error": error });
                ran.failed = true;
                return Ok(ran);
            }
        };
        if name == "ask_user" {
            if let Some(selection) = self.spec.toolsets.as_deref()
                && !crate::ai::toolsets::offers(name, selection)
            {
                ran.result =
                    crate::ai::skill_hints::refusal(&crate::ai::toolsets::refusal(name, selection));
                ran.failed = true;
                return Ok(ran);
            }
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::question::AskUserArgs::decode(&Value::Object(args))
                .map_err(|error| error.to_string());
            let prompt = match request.and_then(|request| request.into_prompt()) {
                Ok(prompt) => prompt,
                Err(error) => {
                    ran.result = json!({ "error": error });
                    ran.failed = true;
                    return Ok(ran);
                }
            };
            self.mark_started(call).await?;
            let options = prompt.options;
            let question = UserQuestion {
                call_id: call.id.clone(),
                question: prompt.question,
                options: options.clone(),
                free_text: prompt.free_text,
            };
            match self.ask_user(question).await? {
                None => ran.result = crate::agent_tools::question::unanswered("no_user"),
                Some(reply) if reply.expired => {
                    ran.result = crate::agent_tools::question::unanswered("expired");
                }
                Some(reply) => {
                    let mut result = json!({ "answered": true });
                    if let Some(option) = reply.option.and_then(|index| options.get(index)) {
                        result["option"] = json!(option);
                    }
                    if let Some(text) = reply.text.filter(|text| !text.trim().is_empty()) {
                        result["text"] = json!(text);
                    }
                    ran.result = result;
                }
            }
            return Ok(ran);
        }
        if name == READ_TOOLCALL {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            let scope = &self.spec.scope;
            let cached = results::with_results(|store| store.full(scope, id).is_some());
            if !cached && let Some(text) = self.host.stored_result(id).await {
                results::with_results(|store| store.remember(scope, id, text));
            }
            let page = results::with_results(|store| {
                store.page(
                    scope,
                    id,
                    args.get("offset").and_then(Value::as_f64),
                    args.get("limit").and_then(Value::as_f64),
                )
            });
            ran.failed = page.is_err();
            ran.result = page.unwrap_or_else(|refusal| refusal);
            return Ok(ran);
        }
        if gate.need == Need::Approval && name != "edit_subtree" {
            let request = ApprovalRequest {
                call_id: call.id.clone(),
                tool: name.to_owned(),
                path: call_path(name, &args),
                args: args.clone(),
                before: None,
                after: None,
            };
            match self.ask(request).await? {
                None => {
                    return Ok(Ran {
                        result: gate::unapproved(name, &args),
                        declined: true,
                        ..ran
                    });
                }
                Some(decision) if decision.expired => {
                    return Ok(Ran {
                        result: gate::expired(name, &args),
                        declined: true,
                        ..ran
                    });
                }
                Some(decision) if !decision.approved => {
                    return Ok(Ran {
                        result: gate::declined(name, &args, decision.target.as_deref()),
                        declined: true,
                        ..ran
                    });
                }
                Some(_) => {}
            }
        }
        // The turn is one batch: a commit from the model would publish it
        // early, so the loop answers that call itself.
        if transaction && self.batch && name != "transaction_rollback" {
            ran.deferred = true;
            ran.result = if name == "transaction_commit" {
                json!({ "committed": false, "note": "this turn commits as one transaction when it ends" })
            } else {
                json!({ "active": true, "label": self.spec.label, "note": "this turn is already one transaction" })
            };
            return Ok(ran);
        }
        if name == PLAN_UPDATE {
            if let Some(selection) = self.spec.toolsets.as_deref()
                && !crate::ai::toolsets::offers(name, selection)
            {
                ran.result =
                    crate::ai::skill_hints::refusal(&crate::ai::toolsets::refusal(name, selection));
                ran.failed = true;
                return Ok(ran);
            }
            match crate::ai::session_state::plan_update_states(&args) {
                Ok(states) => {
                    ran.rows = states
                        .iter()
                        .map(crate::ai::session_state::state_row)
                        .collect();
                    ran.result = json!({ "status": "ok" });
                }
                Err(error) => {
                    ran.result = json!({ "error": error });
                    ran.failed = true;
                }
            }
            return Ok(ran);
        }
        if name == "edit_subtree" {
            if let Some(selection) = self.spec.toolsets.as_deref()
                && !crate::ai::toolsets::offers(name, selection)
            {
                ran.result =
                    crate::ai::skill_hints::refusal(&crate::ai::toolsets::refusal(name, selection));
                ran.failed = true;
                return Ok(ran);
            }
            return self.edit_subtree(call, &args, gate.need).await.map(
                |(result, failed, declined)| Ran {
                    result,
                    failed,
                    declined,
                    ..ran
                },
            );
        }
        if gate.modifies
            && !transaction
            && let Err(refusal) = self.open_batch().await
        {
            return Ok(Ran {
                result: refusal,
                failed: true,
                ..ran
            });
        }
        let (shaped, answered) = shape_audio(name, &args, self.spec.audio);
        if let Some(result) = answered {
            ran.result = result;
            return Ok(ran);
        }
        self.mark_started(call).await?;
        // The document just before this call: a review takes back only what
        // the call itself wrote.
        let before = if gate.need == Need::Review {
            Some(self.host.refresh().await)
        } else {
            None
        };
        match self
            .host
            .call(name, &Value::Object(shaped), &self.cancel)
            .await
        {
            Ok(mut result) => {
                let paired = name == "audio_compare" && args.get("listen") == Some(&json!(true));
                if paired && self.spec.audio.is_none() {
                    result["listening"] = json!({ "status": "unsupported" });
                }
                ran.result = result;
            }
            Err(refusal) => {
                ran.result = refusal;
                ran.failed = true;
            }
        }
        if let Some(before) = before.filter(|_| !ran.failed)
            && let Some(refusal) = self.check_written(&before).await
        {
            ran.result = refusal;
            ran.failed = true;
        }
        Ok(ran)
    }

    async fn ask(&self, request: ApprovalRequest) -> Result<Option<Decision>, Ending> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Ending::Cancelled),
            decision = self.host.ask(request) => Ok(decision),
        }
    }

    async fn ask_user(&self, request: UserQuestion) -> Result<Option<Reply>, Ending> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Ending::Cancelled),
            reply = self.host.ask_user(request) => Ok(reply),
        }
    }

    /// The budget rows this round owes the model: one when the rounds run
    /// low, and one when the measured context share first reaches 60% or 75%.
    /// The two facts share a row when they happen in the same round.
    fn budget_notices(&mut self, remaining: usize) -> Vec<Value> {
        let rounds = (remaining > 0 && remaining <= ROUNDS_LEFT_NOTICE).then_some(remaining as u32);
        let share = self.budget.map(|budget| {
            let measured = budget.measure(&self.wire);
            let percent = (measured as f64 / budget.window as f64 * 100.0).ceil();
            percent.clamp(0.0, 100.0) as u8
        });
        let mut crossed = None;
        if let Some(share) = share {
            for (index, threshold) in [60u8, 75].into_iter().enumerate() {
                if !self.context_warned[index] && share >= threshold {
                    self.context_warned[index] = true;
                    crossed = Some(share);
                }
            }
        }
        if rounds.is_none() && crossed.is_none() {
            return Vec::new();
        }
        vec![wire::budget_notice(rounds, crossed)]
    }

    async fn mark_started(&self, call: &Call) -> Result<(), Ending> {
        self.host
            .started(&call.id)
            .await
            .map_err(|error| Ending::Failed(AiError::new("session_save", error)))
    }

    /// A whole-subtree replacement: validated first, so one that cannot apply
    /// never reaches a person, then asked, previewed or applied by the mode.
    /// With nobody to ask, a validated replacement applies in the turn.
    async fn edit_subtree(
        &mut self,
        call: &Call,
        args: &Map<String, Value>,
        need: Need,
    ) -> Result<(Value, bool, bool), Ending> {
        let (Some(path), Some(dsl)) = (
            args.get("path").and_then(Value::as_str),
            args.get("new_dsl").and_then(Value::as_str),
        ) else {
            return Ok((json!({ "error": "Invalid tool arguments" }), true, false));
        };
        if self.host.refresh().await.dsl != self.expected {
            return Ok((
                json!({ "error": "Project changed during request; retry on the current project." }),
                true,
                false,
            ));
        }
        if let Err(errors) = self.host.validate_edit(path, dsl).await {
            return Ok((json!({ "valid": false, "errors": errors }), false, true));
        }
        let before = self.host.subtree(path).await;
        match need {
            Need::Approval => {
                let request = ApprovalRequest {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                    args: args.clone(),
                    path: Some(path.to_owned()),
                    before: Some(before),
                    after: Some(dsl.to_owned()),
                };
                if let Some(decision) = self.ask(request).await?
                    && !decision.approved
                {
                    if decision.expired {
                        return Ok((gate::expired(&call.name, args), false, true));
                    }
                    let target = decision.target.as_deref().unwrap_or(path);
                    return Ok((gate::declined(&call.name, args, Some(target)), false, true));
                }
            }
            Need::Review => self.host.emit(TurnEvent::Preview {
                call_id: call.id.clone(),
                tool: call.name.clone(),
                path: Some(path.to_owned()),
                before,
                after: dsl.to_owned(),
            }),
            Need::None => {}
        }
        self.stop_if_cancelled()?;
        if let Err(refusal) = self.open_batch().await {
            return Ok((refusal, true, false));
        }
        self.mark_started(call).await?;
        match self.host.apply_edit(path, dsl, &self.expected).await {
            Ok(Edited { document, changes }) => {
                self.expected = document.dsl.clone();
                let mut result = json!({
                    "status": "applied", "applied": true, "path": path,
                    "revision": document.revision, "track_count": document.track_count,
                });
                // `finish_call` moves the facts onto the row, out of the result.
                if let Some(changes) =
                    changes.and_then(|changes| serde_json::to_value(changes).ok())
                {
                    result["changes"] = changes;
                }
                Ok((result, false, false))
            }
            Err(error) => Ok((json!({ "error": error }), true, false)),
        }
    }

    /// Validates the document a reviewed call wrote, and takes the call back
    /// as one undo step when it introduced errors. Only a call that moved the
    /// revision wrote an undo entry, so a call that changed nothing never
    /// undoes an earlier edit. Errors are compared between the document just
    /// before the call and the one after it, without their positions, so
    /// errors the project already had are not the call's.
    async fn check_written(&mut self, before: &Document) -> Option<Value> {
        let document = self.host.refresh().await;
        if document.revision == before.revision
            || document.dsl == before.dsl
            || document.track_count == 0
        {
            return None;
        }
        let errors = self
            .host
            .introduced_errors(&before.dsl, &document.dsl)
            .await
            .ok()?;
        if errors.is_empty() {
            return None;
        }
        if let Err(error) = self.host.undo(document.revision).await {
            return Some(json!({
                "errors": errors,
                "error": format!("the edit did not validate and could not be undone: {error}"),
            }));
        }
        self.expected = self.host.refresh().await.dsl;
        Some(json!({
            "undone": true,
            "error": format!("the edit did not validate and was undone: {}", errors.join("; ")),
            "errors": errors,
        }))
    }

    async fn finish_call(
        &mut self,
        call: &Call,
        native: bool,
        ran: Ran,
        deliveries: &mut Vec<Value>,
    ) -> Result<(), Ending> {
        let name = call.name.as_str();
        let Ran {
            mut result,
            failed,
            declined,
            mutates,
            deferred,
            rows,
        } = ran;
        // Typed edit facts are local message data: they describe the row in
        // the panel and are never part of the tool result the model reads.
        let changes = take_changes(&mut result);
        let transaction = TRANSACTION_TOOLS.contains(&name);
        if result["revision"].is_u64() {
            self.expected = self.host.refresh().await.dsl;
        }
        self.done += 1;
        let edited = mutates && !failed && !transaction && !declined;
        if edited {
            self.edits += 1;
        }
        let mut rolled_back = false;
        if transaction && !failed && !declined && !deferred {
            self.batch = name == "transaction_begin";
            if !self.batch {
                self.edits = 0;
                rolled_back = name == "transaction_rollback";
            }
        }
        let shaped = if (mutates || name == "project_get") && !failed && !declined {
            wire::without_project_ast(&wire::without_inline_audio(&result))
        } else {
            wire::without_inline_audio(&result)
        };
        let mut full = None;
        let content = if name == READ_TOOLCALL {
            shaped.to_string()
        } else {
            let shaped = if name == "devices_catalog" && !failed {
                wire::catalog_columns(&shaped)
            } else {
                shaped
            };
            let (content, keep) = wire::bounded(&shaped, failed, &call.id);
            if keep {
                let text = shaped.to_string();
                let scope = self.spec.scope.clone();
                results::with_results(|store| store.remember(&scope, &call.id, text.clone()));
                full = Some(text);
            }
            content
        };
        let mut message = result_message(call, native, &content);
        if let Some(changes) = changes {
            message["changes"] = changes;
        }
        if let Some(text) = full {
            // The append saves this text beside the conversation and keeps
            // only its hash on the row, so a restart still reads it.
            message[FULL_RESULT] = json!(text);
        }
        self.push(vec![message]).await?;
        if rolled_back {
            deliveries.push(host_notice(
                "Host transaction rollback: every project edit this turn made before the rollback was reverted. Earlier successful results in this turn describe intermediate states, not the current project.",
            ));
        }
        let source = name == "audition_clip_source";
        match (wire::delivered_audio(&result, source), self.spec.audio) {
            (Some(_), None) => deliveries.push(host_notice(if source {
                "This model cannot listen to audio. Use audio_clip_transients to measure source attacks."
            } else {
                "This model cannot listen to audio; the clip was rendered and saved, and its analysis is available through audio_analyze."
            })),
            (Some(audio), Some(_)) => deliveries.push(audio),
            (None, _) => {}
        }
        if name == "audio_compare" && self.spec.audio.is_some() {
            deliveries.extend(wire::delivered_comparison(&result));
        }
        deliveries.extend(rows);
        Ok(())
    }

    /// Closes the turn the same way for every ending: the round's unstarted
    /// calls are answered, then the batch commits.
    async fn close(mut self, ending: Ending) -> TurnOutcome {
        let mut save_error = None;
        let mut not_run = 0;
        match self.host.close_round().await {
            Ok(closed) => {
                // A dispatched call whose result was lost is closed too, but
                // it may have run, so only the calls never started count.
                not_run = closed
                    .iter()
                    .filter(|closed| closed.state == crate::ai::sessions::turns::Unanswered::NotRun)
                    .count();
                for message in closed.into_iter().map(|closed| closed.message) {
                    self.host.emit(TurnEvent::Message {
                        message: message.clone(),
                    });
                    self.wire.push(message);
                }
            }
            Err(error) => save_error = Some(error),
        }
        let changes = if self.batch {
            self.host.batch_changes().await
        } else {
            None
        };
        let commit_result = if self.batch {
            let result = self.host.commit_batch().await;
            // A save failure still committed: the batch is closed either way
            // except when the commit itself failed.
            if !matches!(&result, Err(error) if error.failure == CommitFailure::Commit) {
                self.batch = false;
                self.host.refresh().await;
            }
            result.err()
        } else {
            None
        };
        let commit_error = commit_result.as_ref().map(|error| error.message.clone());
        let committed_in_memory = !commit_result
            .as_ref()
            .is_some_and(|error| error.failure == CommitFailure::Commit);
        let mut stop = TurnStop {
            cancelled: false,
            done: self.done,
            planned: self.planned,
            not_run,
            edits: self.edits,
            committed: self.edits > 0 && committed_in_memory,
            commit_error: commit_error.clone(),
            commit_failure: commit_result.as_ref().map(|error| error.failure),
            in_flight: self.in_flight.take(),
            ..TurnStop::default()
        };
        let mut outcome = match ending {
            Ending::Answer(text) if commit_error.is_none() => TurnOutcome {
                text,
                ..TurnOutcome::default()
            },
            Ending::Answer(_) => TurnOutcome {
                text: stop_text(&stop),
                failed: true,
                stop: Some(stop),
                error: None,
                ..TurnOutcome::default()
            },
            Ending::Empty => TurnOutcome {
                text: "Provider returned no text.".into(),
                failed: true,
                stop: (self.edits > 0 || commit_error.is_some()).then_some(stop),
                error: None,
                ..TurnOutcome::default()
            },
            Ending::Cancelled => {
                stop.cancelled = true;
                TurnOutcome {
                    text: stop_text(&stop),
                    failed: true,
                    stop: Some(stop),
                    error: None,
                    ..TurnOutcome::default()
                }
            }
            Ending::Cap => TurnOutcome {
                text: format!(
                    "stopped after {MAX_TOOL_ROUNDS} tool rounds; the work is unfinished; the last action was {}; {}",
                    self.last_action,
                    stop_text(&stop)
                ),
                failed: true,
                stop: Some(stop),
                error: None,
                ..TurnOutcome::default()
            },
            Ending::Repeated(text) => {
                stop.failure_kept = stop.committed;
                TurnOutcome {
                    text: format!("{text}; {}", stop_text(&stop)),
                    failed: true,
                    stop: Some(stop),
                    error: None,
                    ..TurnOutcome::default()
                }
            }
            Ending::Failed(error) => {
                stop.timed_out = error.kind == "timeout" && stop.committed;
                stop.failure_kept = stop.committed && !stop.timed_out;
                let edited = self.edits > 0 || stop.commit_error.is_some() || stop.not_run > 0;
                TurnOutcome {
                    text: if edited {
                        format!("{}; {}", error.message, stop_text(&stop))
                    } else {
                        error.message.clone()
                    },
                    failed: true,
                    stop: edited.then_some(stop),
                    error: Some(error),
                    ..TurnOutcome::default()
                }
            }
        };
        if committed_in_memory {
            outcome.changes = changes;
        }
        if let Some(error) = save_error {
            outcome.failed = true;
            outcome.text = format!(
                "{}; the conversation could not close its last round: {error}",
                outcome.text
            );
            outcome
                .error
                .get_or_insert(AiError::new("session_save", error));
        }
        outcome
    }
}

/// A per-turn tag for text-protocol call ids; unique across a conversation's
/// turns, which is all pairing needs.
fn turn_nonce() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{nanos:x}{count:x}")
}

/// The English line a headless caller reads; a view localizes the facts.
pub fn stop_text(stop: &TurnStop) -> String {
    if let Some(error) = &stop.commit_error {
        return match stop.commit_failure {
            Some(CommitFailure::Save) => format!(
                "{} applied edits are committed, but the project file was not written: {error}",
                stop.edits
            ),
            _ => format!("the turn stopped and its edits could not be committed: {error}"),
        };
    }
    let verb = if stop.cancelled {
        "cancelled"
    } else {
        "stopped"
    };
    if stop.planned == 0 {
        return format!("{verb} before any tool call ran; the project is unchanged");
    }
    let calls = format!("{} of {} requested calls", stop.done, stop.planned);
    if stop.edits == 0 {
        return format!(
            "{verb} after {calls}; no edit had been applied, so the project is unchanged"
        );
    }
    format!(
        "{verb} after {calls}; {} applied edits were kept",
        stop.edits
    )
}

/// The assistant message as saved: content, calls and the provider state a
/// signed round needs replayed.
fn assistant_message(reply: &Value, calls: Option<&Vec<Call>>, text_call: Option<&Call>) -> Value {
    let mut message = json!({
        "role": "assistant",
        "message_meta": message_meta(MessageKind::Assistant),
        "content": reply.get("content").cloned().unwrap_or(Value::Null),
    });
    if let Some(call) = text_call {
        message["message_meta"]["text_call"] = json!({ "id": call.id, "tool": call.name });
    }
    if let Some(calls) = calls {
        message["tool_calls"] = json!(
            calls
                .iter()
                .map(|call| json!({
                    "id": call.id, "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments },
                }))
                .collect::<Vec<_>>()
        );
    }
    for key in [
        "provider_state",
        "thinking",
        "reasoning_content",
        "reasoning",
        "usage",
    ] {
        if let Some(value) = reply.get(key).filter(|value| !value.is_null()) {
            message[key] = value.clone();
        }
    }
    message
}

/// Remove the typed edit facts from a tool result so the model's content stays
/// unchanged; the caller sets them on the saved row instead.
fn take_changes(result: &mut Value) -> Option<Value> {
    result
        .as_object_mut()
        .and_then(|object| object.remove("changes"))
}

fn result_message(call: &Call, native: bool, content: &str) -> Value {
    if native {
        json!({
            "role": "tool",
            "message_meta": message_meta(MessageKind::Tool),
            "tool_call_id": call.id,
            "content": content,
        })
    } else {
        let mut meta = message_meta(MessageKind::ApplicationResult);
        meta["text_call"] = json!({ "id": call.id, "tool": call.name });
        json!({
            "role": "user",
            "message_meta": meta,
            "content": format!("{APPLICATION_RESULT_PREFIX}{content}"),
        })
    }
}

fn host_notice(text: &str) -> Value {
    json!({ "role": "user", "message_meta": message_meta(MessageKind::HostNotice), "content": text })
}

/// A view never needs the clip bytes; the transcript says a clip was heard.
fn without_inline_bytes(message: &Value) -> Value {
    let mut message = message.clone();
    if let Some(audio) = message
        .get_mut("input_audio")
        .and_then(Value::as_object_mut)
    {
        audio.remove("data");
    }
    message
}

/// Shapes a listening call for the model's audio input. Returns the shaped
/// arguments, or the answer when the call needs no dispatch at all.
fn shape_audio(
    name: &str,
    args: &Map<String, Value>,
    audio: Option<AudioShape>,
) -> (Map<String, Value>, Option<Value>) {
    let paired = name == "audio_compare" && args.get("listen") == Some(&json!(true));
    let mut shaped = args.clone();
    match audio {
        None if paired && args.get("listening_only") == Some(&json!(true)) => {
            return (
                shaped,
                Some(json!({ "listening_only": true, "listening": { "status": "unsupported" } })),
            );
        }
        None if paired => {
            shaped.insert("listen".into(), json!(false));
        }
        Some(shape) if paired || name == "audition_range" || name == "audition_clip_source" => {
            let default = if shape.channels == 2 {
                "stereo"
            } else {
                "mono"
            };
            let requested = args
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or(default)
                .trim()
                .to_lowercase();
            let fold =
                paired && shape.channels == 1 && matches!(requested.as_str(), "stereo" | "both");
            let channel = if fold {
                json!("mono")
            } else {
                args.get("channel")
                    .cloned()
                    .unwrap_or_else(|| json!(default))
            };
            shaped.insert("channel".into(), channel);
            let rate = if paired {
                json!(shape.sample_rate)
            } else {
                args.get("clip_sample_rate")
                    .cloned()
                    .unwrap_or_else(|| json!(shape.sample_rate))
            };
            shaped.insert("clip_sample_rate".into(), rate);
        }
        _ => {}
    }
    (shaped, None)
}

#[cfg(test)]
mod tests;
