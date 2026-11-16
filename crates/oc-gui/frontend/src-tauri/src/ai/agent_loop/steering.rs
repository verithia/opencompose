//! Messages delivered between complete provider/tool rounds, without stopping
//! the request or closing its edit batch. A receipt succeeds only after the
//! message is saved; until then the caller keeps its durable queue entry.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const MAX_PENDING: usize = 8;

#[derive(Clone, Default)]
pub struct Steering(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    closed: bool,
    pending: Vec<Pending>,
}

pub struct Pending {
    pub message: Value,
    pub receipt: oneshot::Sender<Result<(), Undelivered>>,
}

/// Why a message the inbox admitted was not delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undelivered {
    /// The turn ended first; the caller still holds the message.
    Closed,
    /// The conversation store refused it; the turn stops on that failure.
    Save(String),
}

#[derive(Clone, Copy)]
pub enum Boundary {
    Continue,
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Closed,
    Full,
}

impl Steering {
    pub fn enqueue(
        &self,
        message: Value,
    ) -> Result<oneshot::Receiver<Result<(), Undelivered>>, Refusal> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(Refusal::Closed);
        }
        if state.pending.len() >= MAX_PENDING {
            return Err(Refusal::Full);
        }
        let (receipt, receiver) = oneshot::channel();
        state.pending.push(Pending { message, receipt });
        Ok(receiver)
    }

    /// Closing an empty inbox and admitting a message share one lock, so a
    /// final reply cannot race with an acknowledged but undelivered steer.
    pub fn take(&self, boundary: Boundary) -> Vec<Pending> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(boundary, Boundary::Finish) && state.pending.is_empty() {
            state.closed = true;
        }
        std::mem::take(&mut state.pending)
    }

    pub fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        for pending in state.pending.drain(..) {
            let _ = pending.receipt.send(Err(Undelivered::Closed));
        }
    }
}

/// Only admitted human messages acquire a new rewind boundary. Tool results
/// and listening deliveries use the ordinary recorder and keep their tags.
pub fn record(
    store: &crate::ai::sessions::Store,
    id: &str,
    messages: &[Value],
    incarnation: Option<&str>,
    dsl: &str,
    project: Option<&std::path::Path>,
    history: Option<&crate::ai::sessions::HistoryAt>,
) -> Result<Vec<Value>, String> {
    store.append_turn_with_snapshot(
        id,
        messages,
        incarnation,
        crate::ai::sessions::turns::SnapshotAt {
            dsl,
            project,
            history,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn steering_bounds_receipts_and_finish_admission() {
        let queue = Steering::default();
        let mut receipts = Vec::new();
        for n in 0..MAX_PENDING {
            receipts.push(
                queue
                    .enqueue(json!({"role":"user", "content": n.to_string()}))
                    .unwrap(),
            );
        }
        assert!(matches!(queue.enqueue(json!({})), Err(Refusal::Full)));
        let pending = queue.take(Boundary::Finish);
        assert_eq!(pending.len(), MAX_PENDING);
        for (n, row) in pending.into_iter().enumerate() {
            assert_eq!(row.message["content"], n.to_string());
            row.receipt.send(Ok(())).unwrap();
        }
        for receipt in receipts {
            assert!(receipt.await.unwrap().is_ok());
        }
        let receipt = queue
            .enqueue(json!({"role":"user", "content":"last"}))
            .unwrap();
        queue.close();
        assert!(receipt.await.unwrap().is_err());
        assert!(queue.enqueue(json!({})).is_err());
        let empty = Steering::default();
        assert!(empty.take(Boundary::Finish).is_empty());
        assert!(empty.enqueue(json!({})).is_err());
    }
    #[test]
    fn steering_records_its_delivery_snapshot_for_rewind() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::ai::sessions::Store::new(root.path().join("sessions"));
        let id = store.create(None, None, Some("steering"), None).unwrap().id;
        let dsl = "@project \"Delivery point\"\n  tempo: 138\n";
        let messages = [
            json!({"role":"user", "content":"soften highs", "message_meta":crate::ai::sessions::message_meta(crate::ai::sessions::MessageKind::User)}),
        ];
        let history = crate::ai::sessions::HistoryAt {
            project: "/songs/song.ocp".into(),
            level: 2,
            at: Some(7),
        };
        let saved = record(&store, &id, &messages, None, dsl, None, Some(&history)).unwrap();
        let point = store.recall_point(&id, 0).unwrap();
        let sha = saved[0]["dsl_sha"].as_str().unwrap();
        assert_eq!(store.dsl_version(&id, sha).unwrap(), dsl);
        assert_eq!(point.history_at, Some(history));
    }
}
