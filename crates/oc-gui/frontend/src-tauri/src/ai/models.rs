//! Model listing for the add-provider dialog: one authenticated GET against the
//! provider's `/models` endpoint, ids plus the limits a provider reports. The
//! key travels in memory and is never echoed in a result, an error message or a
//! log line.
use super::anthropic::ANTHROPIC_VERSION;
use super::auth::{self, Credential};
use super::secret::Secret;
use super::{http_client, read_limited};
use crate::commands::{AiProviderEntry, Wire};
use serde::Serialize;
use serde_json::Value;

/// Model listing request timeout.
pub const FETCH_TIMEOUT_SECS: u64 = 20;
const MAX_MODELS: usize = 500;
const MAX_MODEL_ID_CHARS: usize = 200;

/// One listed id, with the window the provider reports for it. A missing limit
/// stays `None`, so the modal keeps its own default instead of a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
}

/// A parsed listing. `total` counts the distinct ids before the cap, so a
/// truncated answer is reportable without keeping the dropped entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelListing {
    pub models: Vec<ListedModel>,
    pub total: usize,
}

/// Accepts the listing shapes the wires return: `data[].id` (OpenAI, Anthropic),
/// `models[].name` (Gemini, `models/` prefix stripped) or a bare array.
/// Gemini entries also carry `inputTokenLimit`/`outputTokenLimit`.
pub fn parse_models(text: &str) -> Result<ModelListing, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| "The provider's model list is not JSON.".to_string())?;
    let entries = match &value {
        Value::Array(entries) => entries.as_slice(),
        Value::Object(map) => map
            .get("data")
            .or_else(|| map.get("models"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };
    let mut models = Vec::new();
    for entry in entries {
        let (id, context, max_output) = match entry {
            Value::String(id) => (Some(id.as_str()), None, None),
            Value::Object(map) => (
                map.get("id")
                    .or_else(|| map.get("name"))
                    .and_then(Value::as_str),
                reported_limit(map, "inputTokenLimit"),
                reported_limit(map, "outputTokenLimit"),
            ),
            _ => (None, None, None),
        };
        let Some(id) = id else { continue };
        let id = id.trim();
        let id = id.strip_prefix("models/").unwrap_or(id);
        if !id.is_empty() && id.chars().count() <= MAX_MODEL_ID_CHARS {
            models.push(ListedModel {
                id: id.to_string(),
                context,
                max_output,
            });
        }
    }
    // Alphabetical order is predictable; a provider's own order is not stable
    // across listings, so it is not preserved.
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    let total = models.len();
    models.truncate(MAX_MODELS);
    if models.is_empty() {
        return Err("The provider returned no models. Add model ids by hand.".into());
    }
    Ok(ModelListing { models, total })
}

/// A positive token window from a listing entry, when the wire reports one.
fn reported_limit(map: &serde_json::Map<String, Value>, field: &str) -> Option<u64> {
    map.get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn status_error(status: u16) -> String {
    match status {
        400 | 422 => format!(
            "The provider rejected the listing request (HTTP {status}). Check that the base URL speaks this protocol; some providers also require a version header."
        ),
        401 | 403 => format!("The provider rejected the API key (HTTP {status})."),
        404 => "No model list at this address (HTTP 404). Check the base URL.".into(),
        429 => format!("The provider is rate limiting model listing (HTTP {status}). Retry later."),
        _ => format!("The provider answered HTTP {status} when listing models."),
    }
}

fn transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "Listing models timed out. Check the base URL and network.".into()
    } else if error.is_connect() {
        "Could not connect to the provider. Check the base URL.".into()
    } else {
        "Could not reach the provider. Check the base URL, network and TLS.".into()
    }
}

/// Fetch the provider's model listing.
pub async fn fetch(entry: &AiProviderEntry, key: Option<Secret>) -> Result<ModelListing, String> {
    let route = super::route::Route::for_entry(entry)?;
    fetch_route(entry, &route, key).await
}

/// The same listing against a route the caller resolved, so a test can point a
/// catalog entry at a local server.
pub(crate) async fn fetch_route(
    entry: &AiProviderEntry,
    route: &super::route::Route,
    key: Option<Secret>,
) -> Result<ModelListing, String> {
    let url = route.list_url()?;
    let plan = auth::plan(entry, &Credential::ApiKey(key)).map_err(|error| error.message)?;
    let mut headers = reqwest::header::HeaderMap::new();
    plan.apply(&mut headers).map_err(|error| error.message)?;
    // The chat adapter adds the protocol version itself; the listing path has to
    // declare it too, or Anthropic rejects the request as malformed.
    if entry.wire() == Wire::AnthropicMessages {
        headers.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static(ANTHROPIC_VERSION),
        );
    }
    let client = http_client(FETCH_TIMEOUT_SECS).map_err(|error| error.message)?;
    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(transport_error)?;
    if !response.status().is_success() {
        return Err(status_error(response.status().as_u16()));
    }
    let body = read_limited(response)
        .await
        .map_err(|error| error.message)?;
    parse_models(&String::from_utf8_lossy(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support;
    use crate::commands::{AiCapabilities, AuthKind, Wire};

    fn entry(endpoint: String, wire: Wire) -> AiProviderEntry {
        AiProviderEntry {
            id: "draft".into(),
            name: "Acme".into(),
            kind: wire.kind().unwrap_or_default(),
            wire: Some(wire),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: String::new(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities::default(),
        }
    }

    fn ids(listing: &ModelListing) -> Vec<&str> {
        listing
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect()
    }

    #[test]
    fn both_listing_shapes_and_a_bare_array_are_read() {
        let openai =
            parse_models(r#"{"data":[{"id":"b-model"},{"id":"a-model"},{"id":"a-model"}]}"#)
                .unwrap();
        assert_eq!(ids(&openai), ["a-model", "b-model"]);
        assert_eq!(openai.total, 2);
        let gemini = parse_models(
            r#"{"models":[{"name":"models/gemini-4-argon"},{"name":"models/gemini-4-flash"}]}"#,
        )
        .unwrap();
        assert_eq!(ids(&gemini), ["gemini-4-argon", "gemini-4-flash"]);
        let bare = parse_models(r#"["z-model","y-model"]"#).unwrap();
        assert_eq!(ids(&bare), ["y-model", "z-model"]);
        assert!(
            parse_models(r#"{"data":[]}"#)
                .unwrap_err()
                .contains("no models")
        );
        assert!(
            parse_models("<html>sign in</html>")
                .unwrap_err()
                .contains("not JSON")
        );
    }

    #[test]
    fn a_gemini_listing_carries_the_limits_it_reports() {
        let listing = parse_models(
            r#"{"models":[
                {"name":"models/gemini-plain"},
                {"name":"models/gemini-large","inputTokenLimit":2000000,"outputTokenLimit":64000},
                {"name":"models/gemini-zero","inputTokenLimit":0,"outputTokenLimit":0}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            listing.models,
            vec![
                ListedModel {
                    id: "gemini-large".into(),
                    context: Some(2_000_000),
                    max_output: Some(64_000),
                },
                ListedModel {
                    id: "gemini-plain".into(),
                    context: None,
                    max_output: None,
                },
                // A zero window is not a limit: the modal keeps its default.
                ListedModel {
                    id: "gemini-zero".into(),
                    context: None,
                    max_output: None,
                },
            ]
        );
        // The other shapes get the same struct, with no limits reported.
        let openai = parse_models(r#"{"data":[{"id":"acme-large"}]}"#).unwrap();
        assert_eq!(openai.models[0].context, None);
        assert_eq!(openai.models[0].max_output, None);
    }

    #[test]
    fn a_listing_longer_than_the_cap_reports_how_many_it_cut() {
        let entries: Vec<String> = (0..501)
            .map(|index| format!(r#"{{"id":"m{index:03}"}}"#))
            .collect();
        let listing = parse_models(&format!("[{}]", entries.join(","))).unwrap();
        assert_eq!(listing.total, 501);
        assert_eq!(listing.models.len(), 500);
        assert_eq!(listing.models.first().unwrap().id, "m000");
        // The cap keeps the first 500 of the sorted ids, not an arbitrary slice.
        assert_eq!(listing.models.last().unwrap().id, "m499");
    }

    #[tokio::test]
    async fn a_listing_is_fetched_with_the_saved_key_and_never_echoes_it() {
        let (base, server) =
            test_support::canned(200, r#"{"data":[{"id":"acme-large"},{"id":"acme-small"}]}"#);
        let listing = fetch(
            &entry(
                format!("{base}v1/chat/completions"),
                Wire::OpenAiChatCompletions,
            ),
            Some(Secret::new("fixture-secret")),
        )
        .await
        .unwrap();
        assert_eq!(ids(&listing), ["acme-large", "acme-small"]);
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /v1/models"), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn the_listing_uses_the_header_the_wire_declares() {
        let (base, server) = test_support::canned(200, r#"{"data":[{"id":"claude-x"}]}"#);
        let listing = fetch(
            &entry(format!("{base}v1/messages"), Wire::AnthropicMessages),
            Some(Secret::new("fixture-secret")),
        )
        .await
        .unwrap();
        assert_eq!(ids(&listing), ["claude-x"]);
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /v1/models"), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: fixture-secret"), "{request}");
        assert!(!lower.contains("authorization"), "{request}");
        // The listing declares the protocol version exactly like the chat path.
        assert!(
            lower.contains(&format!("anthropic-version: {ANTHROPIC_VERSION}")),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_protocol_mismatch_names_the_address_the_protocol_and_the_version_header() {
        let (base, server) = test_support::canned(400, "fixture-secret must never escape");
        let error = fetch(
            &entry(
                format!("{base}v1/chat/completions"),
                Wire::OpenAiChatCompletions,
            ),
            Some(Secret::new("fixture-secret")),
        )
        .await
        .unwrap_err();
        for needle in ["400", "base URL", "protocol", "version header"] {
            assert!(error.contains(needle), "{needle}: {error}");
        }
        assert!(!error.contains("fixture-secret"), "{error}");
        server.join().unwrap();
        // A 422 is the same actionable case.
        let error = status_error(422);
        assert!(
            error.contains("base URL") && error.contains("version header"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_without_the_key_or_the_body() {
        let (base, server) = test_support::canned(401, "fixture-secret must never escape");
        let error = fetch(
            &entry(
                format!("{base}v1/chat/completions"),
                Wire::OpenAiChatCompletions,
            ),
            Some(Secret::new("fixture-secret")),
        )
        .await
        .unwrap_err();
        assert!(error.contains("401"), "{error}");
        assert!(!error.contains("fixture-secret"), "{error}");
        let request = server.join().unwrap();
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_a_listing_is_a_readable_error() {
        let (base, server) = test_support::canned(200, "<html>login</html>");
        let error = fetch(
            &entry(
                format!("{base}v1/chat/completions"),
                Wire::OpenAiChatCompletions,
            ),
            Some(Secret::new("fixture-secret")),
        )
        .await
        .unwrap_err();
        assert!(error.contains("not JSON"), "{error}");
        assert!(!error.contains("fixture-secret"), "{error}");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn an_empty_listing_points_at_manual_ids() {
        let (base, server) = test_support::canned(200, r#"{"data":[]}"#);
        let error = fetch(
            &entry(
                format!("{base}v1/chat/completions"),
                Wire::OpenAiChatCompletions,
            ),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("by hand"), "{error}");
        server.join().unwrap();
    }
}
