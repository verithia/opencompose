//! The provider seam: one search, one id resolution, and the transport that
//! every parser reads through.
//!
//! A provider owns three decisions and nothing else: how a [`SampleQuery`]
//! becomes source-specific parameters, how one source record becomes a
//! [`SampleItem`], and what the id of an item is. Downloading, verifying,
//! naming and licensing live in [`crate::store`], so a new source cannot
//! invent its own verification or its own licence rule.

use crate::model::{SampleError, SampleItem, SamplePage, SampleQuery};

/// A body the transport managed to fetch.
#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The only way a provider touches the network. Tests implement this over
/// recorded bytes, so no parser test reaches the internet.
pub trait SampleFetch: Sync {
    /// GET `url`, refusing a body larger than `max_bytes` and any non-2xx
    /// status. The error text reaches the caller unchanged.
    fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, String>;

    /// Local helper processes use the same network configuration as the host.
    fn plugin_network(&self) -> crate::local_plugin::Network {
        crate::local_plugin::Network::default()
    }

    /// GET with request headers, such as provider API authentication.
    fn get_with(
        &self,
        url: &str,
        max_bytes: usize,
        _headers: &[(&str, &str)],
    ) -> Result<FetchResponse, String> {
        self.get(url, max_bytes)
    }
}

/// Largest JSON body a provider accepts. A page of annotated results with
/// per-file technical metadata is well under this; the bound keeps a runaway
/// response from being buffered.
pub const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

/// One searchable catalogue.
pub trait SampleProvider: Sync {
    /// Stable id used in tool arguments and in a receipt.
    fn id(&self) -> &'static str;
    /// Human name shown in results.
    fn label(&self) -> &'static str;

    /// One page of results. `query.offset` is the provider's own cursor, as
    /// reported by the previous page's `next_offset`.
    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError>;

    /// The item behind an id this provider issued, fetched fresh so a pull
    /// never trusts a caller-supplied URL.
    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError>;
}

/// Fetch and parse one JSON document, naming the provider in a failure.
pub fn get_json(
    fetch: &dyn SampleFetch,
    provider: &str,
    url: &str,
) -> Result<serde_json::Value, SampleError> {
    get_json_with_headers(fetch, provider, url, &[])
}

/// Fetch and parse one JSON document with request headers. A market that
/// authenticates by header reads its credential here, so the key never enters
/// the URL that an error repeats.
pub fn get_json_with_headers(
    fetch: &dyn SampleFetch,
    provider: &str,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<serde_json::Value, SampleError> {
    let response = fetch
        .get_with(url, MAX_JSON_BYTES, headers)
        .map_err(|error| SampleError::Failed(format!("{provider}: {error}")))?;
    if response.body.len() > MAX_JSON_BYTES {
        return Err(SampleError::Failed(format!(
            "{provider}: response is larger than {MAX_JSON_BYTES} bytes"
        )));
    }
    serde_json::from_slice(&response.body)
        .map_err(|error| SampleError::Failed(format!("{provider}: unreadable JSON: {error}")))
}

/// Percent-encode one query value. Only the unreserved set survives, which is
/// what both catalogues accept for a search term.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_value_is_percent_encoded() {
        assert_eq!(encode("drum one-shot"), "drum%20one-shot");
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode("café"), "caf%C3%A9");
    }

    /// A fetcher that only implements `get` keeps working through the default
    /// `get_with`, so adding the header seam does not break existing clients.
    #[test]
    fn a_fetcher_that_ignores_headers_still_answers() {
        struct BareFetch;

        impl SampleFetch for BareFetch {
            fn get(&self, _url: &str, _max_bytes: usize) -> Result<FetchResponse, String> {
                Ok(FetchResponse {
                    status: 200,
                    body: br#"{"ok":true}"#.to_vec(),
                })
            }
        }

        let body = get_json_with_headers(
            &BareFetch,
            "Test",
            "https://example.org/",
            &[("Authorization", "Token x")],
        )
        .unwrap();
        assert_eq!(body["ok"], true);
    }
}
