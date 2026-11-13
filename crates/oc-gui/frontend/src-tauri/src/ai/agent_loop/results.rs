//! Full tool results behind a truncated preview, so `read_toolcall` can page
//! through them. Results are scoped to the conversation that produced them: a
//! page is never read from another conversation's calls.

use super::wire::{MAX_READ_CHARS, MAX_SENT_CHARS};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

/// Results kept across every conversation; the oldest is evicted first.
pub const MAX_STORED_RESULTS: usize = 64;
/// A result larger than this is not kept; its preview says to query narrower.
pub const MAX_STORED_CHARS: usize = 2_000_000;

#[derive(Default)]
pub struct ResultStore {
    entries: VecDeque<(String, String, String)>,
}

impl ResultStore {
    pub fn remember(&mut self, scope: &str, call: &str, text: String) {
        if text.chars().count() > MAX_STORED_CHARS {
            return;
        }
        self.entries
            .retain(|(owner, id, _)| !(owner == scope && id == call));
        self.entries
            .push_back((scope.to_owned(), call.to_owned(), text));
        while self.entries.len() > MAX_STORED_RESULTS {
            self.entries.pop_front();
        }
    }

    /// One page of a stored result, or the refusal the model reads.
    pub fn page(
        &self,
        scope: &str,
        call: &str,
        offset: Option<f64>,
        limit: Option<f64>,
    ) -> Result<Value, Value> {
        let Some((_, _, text)) = self
            .entries
            .iter()
            .find(|(owner, id, _)| owner == scope && id == call)
        else {
            return Err(json!({
                "error": format!("No stored result for {call}. It may have been evicted; run a narrower query instead."),
            }));
        };
        let chars: Vec<char> = text.chars().collect();
        let whole = |value: Option<f64>| value.filter(|n| n.is_finite()).map(|n| n.trunc());
        let start = whole(offset).unwrap_or(0.0).clamp(0.0, chars.len() as f64) as usize;
        let size = whole(limit)
            .filter(|n| *n >= 1.0)
            .unwrap_or(MAX_SENT_CHARS as f64)
            .min(MAX_READ_CHARS as f64) as usize;
        let end = (start + size).min(chars.len());
        Ok(json!({
            "id": call,
            "offset": start,
            "limit": size,
            "total": chars.len(),
            "text": chars[start..end].iter().collect::<String>(),
        }))
    }

    /// The whole stored result, for the view that renders it in full.
    pub fn full(&self, scope: &str, call: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(owner, id, _)| owner == scope && id == call)
            .map(|(_, _, text)| text.clone())
    }

    /// Drops one conversation's results, for a revert that cuts its calls.
    pub fn forget(&mut self, scope: &str) {
        self.entries.retain(|(owner, _, _)| owner != scope);
    }
}

pub static RESULTS: LazyLock<Mutex<ResultStore>> = LazyLock::new(Mutex::default);

/// Runs `f` on the process-wide store. A poisoned lock still holds valid data.
pub fn with_results<T>(f: impl FnOnce(&mut ResultStore) -> T) -> T {
    f(&mut RESULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_scoped_bounded_and_evicted_oldest_first() {
        let mut store = ResultStore::default();
        store.remember("s1", "c1", "abcdef".into());
        let page = store.page("s1", "c1", Some(2.0), Some(3.0)).unwrap();
        assert_eq!(
            (page["text"].as_str(), page["total"].as_u64()),
            (Some("cde"), Some(6))
        );
        assert_eq!(store.full("s1", "c1").as_deref(), Some("abcdef"));
        assert_eq!(
            store.full("s2", "c1"),
            None,
            "another conversation sees nothing"
        );
        assert!(
            store.page("s2", "c1", None, None).is_err(),
            "another conversation reads nothing"
        );
        let past = store.page("s1", "c1", Some(99.0), Some(1e9)).unwrap();
        assert_eq!(
            (past["text"].as_str(), past["limit"].as_u64()),
            (Some(""), Some(MAX_READ_CHARS as u64))
        );
        for index in 0..MAX_STORED_RESULTS {
            store.remember("s1", &format!("n{index}"), "x".into());
        }
        assert!(store.page("s1", "c1", None, None).is_err());
        store.forget("s1");
        assert!(store.page("s1", "n63", None, None).is_err());
    }
}
