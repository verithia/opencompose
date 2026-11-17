//! Questions a running turn waits on, and the answers a view delivers.

use super::{ApprovalRequest, Decision, Reply, UserQuestion};
use crate::ai::agent_loop::PendingQuestion;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use tokio::sync::oneshot;

/// Pending questions one turn may hold; a running turn asks one at a time.
pub(super) const MAX_PENDING: usize = 4;
/// How long a question waits for a person. A closed panel leaves the card
/// for a reopened one; past this the call is refused, so a turn nobody
/// returns to does not hold its slot forever.
pub(super) const QUESTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A question's key: the turn's request id and the call.
pub(super) type QuestionKey = (String, String);
/// A question waiting for a person, kept so a window that reloaded can ask
/// it again.
pub(super) enum Waiting {
    Approval {
        answer: oneshot::Sender<Decision>,
        request: ApprovalRequest,
    },
    UserQuestion {
        answer: oneshot::Sender<Reply>,
        request: UserQuestion,
    },
}
/// Questions waiting for a person.
pub(super) static PENDING: LazyLock<Mutex<HashMap<QuestionKey, Waiting>>> =
    LazyLock::new(Mutex::default);

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Removes its question from [`PENDING`] when the wait ends or is dropped.
pub(super) struct AskedQuestion(pub(super) QuestionKey);

impl Drop for AskedQuestion {
    fn drop(&mut self) {
        lock(&PENDING).remove(&self.0);
    }
}

/// A person's answer, or a refusal: when nobody answers in time, and when
/// the question was dropped unanswered. A person was asked, so this is never
/// the "nobody attends" answer that lets a replacement apply.
pub(super) async fn await_answer(
    receiver: oneshot::Receiver<Decision>,
    wait: std::time::Duration,
) -> Decision {
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) => Decision::default(),
        Err(_) => Decision::expired(),
    }
}

/// A person's reply, or the refusal the same wait produces.
pub(super) async fn await_reply(
    receiver: oneshot::Receiver<Reply>,
    wait: std::time::Duration,
) -> Reply {
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) | Err(_) => Reply::expired(),
    }
}

/// Answers an approval a running turn asked. A stale answer changes nothing.
pub fn answer(request_id: &str, call_id: &str, decision: Decision) -> Result<(), String> {
    let key = (request_id.to_owned(), call_id.to_owned());
    let question = {
        let mut pending = lock(&PENDING);
        if !matches!(pending.get(&key), Some(Waiting::Approval { .. })) {
            return Err("Nothing is waiting for an answer to this question".into());
        }
        pending.remove(&key)
    };
    let Some(Waiting::Approval { answer, .. }) = question else {
        return Err("Nothing is waiting for an answer to this question".into());
    };
    answer
        .send(decision)
        .map_err(|_| "The turn stopped before the answer arrived".to_string())
}

/// Why a model question's answer was refused. The answer is not delivered and
/// the question stays open, so a view can correct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyError {
    /// No question waits under this turn and call id.
    NotWaiting,
    /// The option index names no offered choice.
    OptionOutOfRange,
    /// The question offers no free text.
    TextNotAccepted,
    /// The text is over the answer bound.
    TextTooLong,
    /// The waiting turn ended before the answer arrived.
    TurnStopped,
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotWaiting => "Nothing is waiting for an answer to this question",
            Self::OptionOutOfRange => "The chosen option is not offered by this question",
            Self::TextNotAccepted => "This question accepts no free-text answer",
            Self::TextTooLong => "The answer is too long",
            Self::TurnStopped => "The turn stopped before the answer arrived",
        })
    }
}

/// Answers a question a running turn asked. An answer outside the question's
/// bounds is refused before anything is removed, so the card can try again.
pub fn reply(request_id: &str, call_id: &str, reply: Reply) -> Result<(), ReplyError> {
    let key = (request_id.to_owned(), call_id.to_owned());
    let mut pending = lock(&PENDING);
    let Some(Waiting::UserQuestion { request, .. }) = pending.get(&key) else {
        return Err(ReplyError::NotWaiting);
    };
    validate_reply(&reply, request)?;
    let Some(Waiting::UserQuestion { answer, .. }) = pending.remove(&key) else {
        return Err(ReplyError::NotWaiting);
    };
    answer.send(reply).map_err(|_| ReplyError::TurnStopped)
}

/// Checks an answer against the question it answers: a choice must be one of
/// the offered options, and text must be allowed and within the bound.
fn validate_reply(reply: &Reply, request: &UserQuestion) -> Result<(), ReplyError> {
    if reply
        .option
        .is_some_and(|index| index >= request.options.len())
    {
        return Err(ReplyError::OptionOutOfRange);
    }
    if let Some(text) = reply
        .text
        .as_deref()
        .filter(|text| !crate::ai::session_state::is_blank_text(text))
    {
        if !request.free_text {
            return Err(ReplyError::TextNotAccepted);
        }
        if text.chars().count() > request.max_chars {
            return Err(ReplyError::TextTooLong);
        }
    }
    Ok(())
}

/// The approvals a running turn is waiting on, for a window that reloaded
/// while the card was open.
#[cfg(test)]
pub fn questions(request_id: &str) -> Vec<ApprovalRequest> {
    lock(&PENDING)
        .iter()
        .filter(|((turn, _), _)| turn == request_id)
        .filter_map(|(_, question)| match question {
            Waiting::Approval { request, .. } => Some(request.clone()),
            Waiting::UserQuestion { .. } => None,
        })
        .collect()
}

/// Every question a running turn is waiting on, typed for the wire.
pub fn pending_questions(request_id: &str) -> Vec<PendingQuestion> {
    lock(&PENDING)
        .iter()
        .filter(|((turn, _), _)| turn == request_id)
        .map(|(_, question)| match question {
            Waiting::Approval { request, .. } => PendingQuestion::Approval {
                request: request.clone(),
            },
            Waiting::UserQuestion { request, .. } => PendingQuestion::UserQuestion {
                request: request.clone(),
            },
        })
        .collect()
}
