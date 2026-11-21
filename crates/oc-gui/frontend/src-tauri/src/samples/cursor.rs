//! The opaque continuation a multi-market search hands back.
//!
//! Each market pages on its own: one may have returned twenty results while
//! another filtered half of its page away, so one shared offset would skip or
//! repeat results. The cursor records, per market and in the order the
//! markets were asked, the provider offset the next page starts at. A market
//! that has no next page is left out, so a follow-up search asks only the
//! markets that still have results.
//!
//! The wire form is unpadded base64url of `{"v":1,"m":[["<id>",<offset>],..]}`.
//! It comes back from a caller, so it is bounded and validated before any
//! market is asked.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// The cursor format this build writes and reads.
const VERSION: u32 = 1;
/// Longest encoded cursor accepted. A cursor for every market this build
/// knows is a few hundred characters.
pub(crate) const MAX_CURSOR_CHARS: usize = 2048;
/// Most markets one cursor may name.
pub(crate) const MAX_CURSOR_MARKETS: usize = 32;
/// Longest market id in a cursor.
const MAX_MARKET_ID_CHARS: usize = 64;
/// Largest provider offset in a cursor. No market pages this deep, and the
/// bound keeps the offset arithmetic far from overflow.
pub(crate) const MAX_CURSOR_OFFSET: usize = 1_000_000;

/// Why a cursor was rejected. `code` is the stable wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    TooLong,
    Malformed,
    UnsupportedVersion,
    TooManyMarkets,
    InvalidMarket,
    OffsetOutOfRange,
}

impl CursorError {
    /// The refusal code every cursor rejection shares; the variant only
    /// changes the sentence.
    pub(crate) const CODE: &'static str = "invalid_cursor";

    pub(crate) fn message(self) -> &'static str {
        match self {
            CursorError::TooLong => "The search cursor is longer than any this build writes.",
            CursorError::Malformed => {
                "The search cursor is not one this build wrote; start the search again."
            }
            CursorError::UnsupportedVersion => {
                "The search cursor comes from another version; start the search again."
            }
            CursorError::TooManyMarkets => "The search cursor names too many markets.",
            CursorError::InvalidMarket => "The search cursor names an invalid market id.",
            CursorError::OffsetOutOfRange => "The search cursor's offset is out of range.",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Wire {
    v: u32,
    m: Vec<(String, usize)>,
}

/// Per-market continuation, in the order the markets were asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchCursor {
    markets: Vec<(String, usize)>,
}

impl SearchCursor {
    /// Record that `market` continues at `offset`. A repeated market keeps
    /// its first entry.
    pub(crate) fn push(&mut self, market: &str, offset: usize) {
        if self.markets.len() >= MAX_CURSOR_MARKETS
            || self.markets.iter().any(|(seen, _)| seen == market)
        {
            return;
        }
        self.markets
            .push((market.to_string(), offset.min(MAX_CURSOR_OFFSET)));
    }

    /// The offset `market` continues at, or `None` when it has no next page.
    pub(crate) fn offset_of(&self, market: &str) -> Option<usize> {
        self.markets
            .iter()
            .find(|(seen, _)| seen == market)
            .map(|(_, offset)| *offset)
    }

    /// The markets in the order they were asked.
    pub(crate) fn markets(&self) -> impl Iterator<Item = &str> {
        self.markets.iter().map(|(market, _)| market.as_str())
    }

    /// The opaque string a result carries, or `None` when no market has a
    /// next page.
    pub(crate) fn encode(&self) -> Option<String> {
        if self.markets.is_empty() {
            return None;
        }
        let json = serde_json::to_vec(&Wire {
            v: VERSION,
            m: self.markets.clone(),
        })
        .ok()?;
        Some(URL_SAFE_NO_PAD.encode(json))
    }

    /// Read a cursor a caller sent back.
    pub(crate) fn decode(text: &str) -> Result<Self, CursorError> {
        let text = text.trim();
        if text.len() > MAX_CURSOR_CHARS {
            return Err(CursorError::TooLong);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| CursorError::Malformed)?;
        let wire: Wire = serde_json::from_slice(&bytes).map_err(|_| CursorError::Malformed)?;
        if wire.v != VERSION {
            return Err(CursorError::UnsupportedVersion);
        }
        if wire.m.len() > MAX_CURSOR_MARKETS {
            return Err(CursorError::TooManyMarkets);
        }
        let mut cursor = SearchCursor::default();
        for (market, offset) in wire.m {
            if market.is_empty()
                || market.len() > MAX_MARKET_ID_CHARS
                || !market
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || cursor.offset_of(&market).is_some()
            {
                return Err(CursorError::InvalidMarket);
            }
            if offset > MAX_CURSOR_OFFSET {
                return Err(CursorError::OffsetOutOfRange);
            }
            cursor.markets.push((market, offset));
        }
        Ok(cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[test]
    fn a_cursor_round_trips_its_markets_in_order() {
        let mut cursor = SearchCursor::default();
        cursor.push("openverse", 20);
        cursor.push("internet-archive", 7);
        cursor.push("openverse", 99);
        let text = cursor.encode().unwrap();
        assert!(
            text.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "URL-safe and unpadded: {text}"
        );
        let back = SearchCursor::decode(&text).unwrap();
        assert_eq!(back, cursor);
        assert_eq!(
            back.markets().collect::<Vec<_>>(),
            vec!["openverse", "internet-archive"]
        );
        assert_eq!(back.offset_of("openverse"), Some(20));
        assert_eq!(back.offset_of("freesound"), None);
        assert_eq!(
            String::from_utf8(URL_SAFE_NO_PAD.decode(&text).unwrap()).unwrap(),
            r#"{"v":1,"m":[["openverse",20],["internet-archive",7]]}"#
        );
    }

    #[test]
    fn an_exhausted_search_has_no_cursor() {
        assert_eq!(SearchCursor::default().encode(), None);
    }

    #[test]
    fn a_malformed_or_oversized_cursor_is_rejected_with_its_reason() {
        let cases = [
            ("x".repeat(MAX_CURSOR_CHARS + 1), CursorError::TooLong),
            ("!!not base64!!".to_string(), CursorError::Malformed),
            (raw("not json"), CursorError::Malformed),
            (raw(r#"{"v":1,"m":{"a":1}}"#), CursorError::Malformed),
            (raw(r#"{"v":1,"m":[["a",-1]]}"#), CursorError::Malformed),
            (raw(r#"{"v":2,"m":[]}"#), CursorError::UnsupportedVersion),
            (
                raw(r#"{"v":1,"m":[["../x",1]]}"#),
                CursorError::InvalidMarket,
            ),
            (raw(r#"{"v":1,"m":[["",1]]}"#), CursorError::InvalidMarket),
            (
                raw(r#"{"v":1,"m":[["a",1],["a",2]]}"#),
                CursorError::InvalidMarket,
            ),
            (
                raw(r#"{"v":1,"m":[["a",1000001]]}"#),
                CursorError::OffsetOutOfRange,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(SearchCursor::decode(&text), Err(expected), "{text}");
        }
        let many: Vec<String> = (0..=MAX_CURSOR_MARKETS)
            .map(|n| format!("[\"m{n}\",1]"))
            .collect();
        assert_eq!(
            SearchCursor::decode(&raw(&format!(r#"{{"v":1,"m":[{}]}}"#, many.join(",")))),
            Err(CursorError::TooManyMarkets)
        );
    }
}
