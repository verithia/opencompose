//! Where a protocol request goes. The protocol owns the request path it speaks;
//! the catalog owns each provider's base URL, path override and query. A saved
//! endpoint that is on a declared route is rebuilt from that data; anything else
//! is its own route, so a custom gateway keeps working exactly as saved.
use super::catalog::{self, Catalog};
use super::endpoint;
use crate::commands::{AiProviderEntry, Wire};

/// One provider's network address, resolved from catalog data or from a saved
/// endpoint. `path` is empty when the base already is the request address (a
/// custom endpoint saved in full); `list` still points at the base's sibling
/// `/models`, which is where the catalog lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    base: reqwest::Url,
    path: String,
    query: Vec<(String, String)>,
}

impl Route {
    /// Resolves the route of a saved entry against the bundled catalog.
    pub fn for_entry(entry: &AiProviderEntry) -> Result<Self, String> {
        Self::for_entry_in(catalog::catalog(), entry)
    }

    /// The same resolution against an explicit catalog, so a test can prove that
    /// a provider or model added to the data is enough.
    pub fn for_entry_in(catalog: &Catalog, entry: &AiProviderEntry) -> Result<Self, String> {
        let wire = entry.wire();
        if let Some(provider) = catalog.serving(wire, &entry.endpoint)
            && let Some(base_url) = provider.base_url.as_deref()
        {
            let candidate = Self {
                base: endpoint(base_url)?,
                path: provider.request_path().to_owned(),
                query: provider.request_query(),
            };
            // The entry is on the declared route when it saves the base URL or
            // the request URL that route builds. A user who edited the endpoint
            // to another address keeps that address.
            if same_url(&entry.endpoint, base_url)
                || same_url(
                    &entry.endpoint,
                    candidate.request_url(&entry.model)?.as_str(),
                )
            {
                return Ok(candidate);
            }
        }
        Self::custom(entry)
    }

    /// A route the catalog does not declare: the saved endpoint is the request
    /// address, except for generateContent, whose path carries the model id.
    fn custom(entry: &AiProviderEntry) -> Result<Self, String> {
        let wire = entry.wire();
        let saved = endpoint(&entry.endpoint)?;
        if wire == Wire::GeminiGenerate {
            return Ok(Self {
                base: saved,
                path: wire.default_path().to_owned(),
                query: wire.default_query(),
            });
        }
        let segments = path_segments(&saved);
        let drop = request_path_tail(&segments);
        if drop == 0 {
            // A base saved at a version segment carries no operation path; the
            // wire appends its own. Anything else is the address as saved.
            let base = version_segment(segments.last().map(String::as_str));
            return Ok(Self {
                base: saved,
                path: if base {
                    wire.default_path().to_owned()
                } else {
                    String::new()
                },
                query: if base {
                    wire.default_query()
                } else {
                    Vec::new()
                },
            });
        }
        let path = segments[segments.len() - drop..].join("/");
        Ok(Self {
            base: without_tail(saved, drop)?,
            path,
            // A wire whose request streams only with a query keeps it, so a
            // saved full request URL still streams.
            query: wire.default_query(),
        })
    }

    /// `POST` address for one model. `{model}` in the declared path is replaced;
    /// the model id is percent-encoded as one path segment.
    pub fn request_url(&self, model: &str) -> Result<reqwest::Url, String> {
        let mut url = self.base.clone();
        if !self.path.is_empty() {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "Invalid endpoint URL".to_string())?;
            segments.pop_if_empty();
            for segment in self.path.split('/').filter(|segment| !segment.is_empty()) {
                segments.push(&segment.replace("{model}", model));
            }
        }
        for (key, value) in &self.query {
            url.query_pairs_mut().append_pair(key, value);
        }
        Ok(url)
    }

    /// `GET {base}/models`, the listing every protocol exposes next to its
    /// request path. The route's own query and path never leak into it.
    pub fn list_url(&self) -> Result<reqwest::Url, String> {
        let mut url = self.base.clone();
        url.set_query(None);
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Invalid endpoint URL".to_string())?;
        segments.pop_if_empty();
        segments.push("models");
        drop(segments);
        Ok(url)
    }
}

/// Normalises a URL for comparison: scheme/host case and one trailing slash do
/// not make a different address.
fn same_url(left: &str, right: &str) -> bool {
    match (reqwest::Url::parse(left), reqwest::Url::parse(right)) {
        (Ok(left), Ok(right)) => {
            left.as_str().trim_end_matches('/') == right.as_str().trim_end_matches('/')
        }
        _ => left.trim_end_matches('/') == right.trim_end_matches('/'),
    }
}

fn path_segments(url: &reqwest::Url) -> Vec<String> {
    url.path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Request paths the saved endpoint may end with. Listing lives next to them, so
/// the base URL is the endpoint without its request path plus `/models`. This is
/// one rule for every protocol: the same gateway prefix serves both.
const REQUEST_PATHS: &[&str] = &[
    "models",
    "responses",
    "messages",
    "interactions",
    "v1internal:streamGenerateContent",
];

/// A version segment (`v1`, `v2`, `v1beta`) names an API base, not an operation,
/// so the wire's request path still has to follow it.
fn version_segment(segment: Option<&str>) -> bool {
    let Some(segment) = segment else {
        return false;
    };
    let mut chars = segment.chars();
    matches!(chars.next(), Some('v'))
        && chars.next().is_some_and(|digit| digit.is_ascii_digit())
        && chars.all(|rest| rest.is_ascii_alphanumeric())
}

/// How many trailing segments name a request path rather than a base prefix.
/// A saved endpoint is the request URL when it ends with a path a protocol
/// publishes.
fn request_path_tail(segments: &[String]) -> usize {
    match segments {
        [.., last] if REQUEST_PATHS.contains(&last.as_str()) => 1,
        [.., first, last] if first == "chat" && last == "completions" => 2,
        _ => 0,
    }
}

fn without_tail(url: reqwest::Url, drop: usize) -> Result<reqwest::Url, String> {
    let mut url = url;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Invalid endpoint URL".to_string())?;
        segments.pop_if_empty();
        for _ in 0..drop {
            segments.pop();
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AuthKind, ProviderKind, Wire};

    fn entry(wire: Wire, endpoint: &str, model: &str) -> AiProviderEntry {
        AiProviderEntry {
            id: "fixture".into(),
            name: "Fixture".into(),
            kind: wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions),
            wire: Some(wire),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: endpoint.into(),
            model: model.into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }

    fn request(wire: Wire, endpoint: &str, model: &str) -> String {
        Route::for_entry(&entry(wire, endpoint, model))
            .unwrap()
            .request_url(model)
            .unwrap()
            .to_string()
    }

    fn listing(wire: Wire, endpoint: &str) -> String {
        Route::for_entry(&entry(wire, endpoint, "m"))
            .unwrap()
            .list_url()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_saved_endpoint_that_is_the_request_address_is_split_for_listing() {
        let cases = [
            (
                Wire::OpenAiChatCompletions,
                "https://api.openai.com/v1/chat/completions",
                "https://api.openai.com/v1/models",
            ),
            (
                Wire::OpenAiResponses,
                "https://api.openai.com/v1/responses",
                "https://api.openai.com/v1/models",
            ),
            (
                Wire::OpenAiChatCompletions,
                "https://api.deepseek.com/chat/completions",
                "https://api.deepseek.com/models",
            ),
            (
                Wire::AnthropicMessages,
                "https://api.anthropic.com/v1/messages",
                "https://api.anthropic.com/v1/models",
            ),
            (
                Wire::GeminiInteractions,
                "https://generativelanguage.googleapis.com/v1beta/interactions",
                "https://generativelanguage.googleapis.com/v1beta/models",
            ),
            (
                Wire::GeminiGenerate,
                "https://opencode.ai/inference/go/google/v1beta",
                "https://opencode.ai/inference/go/google/v1beta/models",
            ),
            (
                Wire::OpenAiChatCompletions,
                "https://llm.acme.test/v1/chat/completions/",
                "https://llm.acme.test/v1/models",
            ),
            (
                Wire::OpenAiChatCompletions,
                "https://llm.acme.test/v1/models",
                "https://llm.acme.test/v1/models",
            ),
            (
                Wire::OpenAiChatCompletions,
                "http://localhost:1234",
                "http://localhost:1234/models",
            ),
        ];
        for (wire, endpoint, expected) in cases {
            assert_eq!(listing(wire, endpoint), expected, "{endpoint}");
        }
        assert!(
            Route::for_entry(&entry(
                Wire::OpenAiChatCompletions,
                "http://llm.acme.test/v1/chat/completions",
                "m"
            ))
            .is_err()
        );
        assert!(Route::for_entry(&entry(Wire::OpenAiChatCompletions, "not a url", "m")).is_err());
    }

    #[test]
    fn a_saved_endpoint_is_its_own_request_address() {
        // A saved request URL, or a path that is not a version base, is kept.
        let wire = Wire::OpenAiChatCompletions;
        assert_eq!(
            request(wire, "https://llm.acme.test/v1/chat/completions", "m"),
            "https://llm.acme.test/v1/chat/completions"
        );
        assert_eq!(
            request(wire, "https://llm.acme.test/api", "m"),
            "https://llm.acme.test/api"
        );
        assert_eq!(
            request(wire, "https://gw.acme.test/v1beta/openai", "m"),
            "https://gw.acme.test/v1beta/openai"
        );
        assert_eq!(
            request(wire, "http://localhost:11434/v1/chat/completions", "m"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    /// A saved endpoint that ends at a version segment is a base: it carries no
    /// operation path, so each wire's own request path is appended.
    #[test]
    fn a_saved_version_base_gets_the_wires_own_operation_path() {
        let cases = [
            (
                Wire::OpenAiChatCompletions,
                "https://proxy.acme.test:8443/v1",
                "https://proxy.acme.test:8443/v1/chat/completions",
            ),
            (
                Wire::OpenAiChatCompletions,
                "https://proxy.acme.test:8443/v1/",
                "https://proxy.acme.test:8443/v1/chat/completions",
            ),
            (
                Wire::OpenAiResponses,
                "https://gw.acme.test/v1",
                "https://gw.acme.test/v1/responses",
            ),
            (
                Wire::AnthropicMessages,
                "https://gw.acme.test/v2",
                "https://gw.acme.test/v2/messages",
            ),
            (
                Wire::GeminiInteractions,
                "https://gw.acme.test/v1beta",
                "https://gw.acme.test/v1beta/interactions",
            ),
            (
                Wire::GeminiGenerate,
                "https://gw.acme.test/v1beta",
                "https://gw.acme.test/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse",
            ),
        ];
        for (wire, endpoint, expected) in cases {
            assert_eq!(
                request(wire, endpoint, "gemini-4-argon"),
                expected,
                "{endpoint}"
            );
        }
        // The listing stays the base's sibling, not the appended operation.
        assert_eq!(
            listing(
                Wire::OpenAiChatCompletions,
                "https://proxy.acme.test:8443/v1"
            ),
            "https://proxy.acme.test:8443/v1/models"
        );
    }

    #[test]
    fn gemini_generate_builds_the_model_path_and_stream_query() {
        assert_eq!(
            request(
                Wire::GeminiGenerate,
                "https://opencode.ai/inference/go/google/v1beta",
                "gemini-4-argon"
            ),
            "https://opencode.ai/inference/go/google/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse"
        );
        // A model id stays one segment, so a slash cannot escape the path.
        assert_eq!(
            request(
                Wire::GeminiGenerate,
                "https://gw.example/v1beta",
                "publishers/acme/models/vision"
            ),
            "https://gw.example/v1beta/models/publishers%2Facme%2Fmodels%2Fvision:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn catalog_routes_win_when_the_entry_saves_the_declared_base() {
        let catalog = catalog::parse(
            r#"{
                "version": "fixture",
                "providers": [{
                    "id": "acme",
                    "name": "Acme",
                    "wire": "openai-chat-completions",
                    "base_url": "https://gw.acme.test/v1",
                    "path": "chat/completions",
                    "query": [["api-version", "2026-01-01"]],
                    "hosts": ["gw.acme.test"],
                    "models": []
                }]
            }"#,
        )
        .unwrap();
        let saved = entry(Wire::OpenAiChatCompletions, "https://gw.acme.test/v1", "m");
        let route = Route::for_entry_in(&catalog, &saved).unwrap();
        assert_eq!(
            route.request_url("m").unwrap().as_str(),
            "https://gw.acme.test/v1/chat/completions?api-version=2026-01-01"
        );
        assert_eq!(
            route.list_url().unwrap().as_str(),
            "https://gw.acme.test/v1/models"
        );
        // A user-edited address is not overridden by the catalog.
        let edited = entry(
            Wire::OpenAiChatCompletions,
            "https://proxy.acme.test/v1/chat/completions",
            "m",
        );
        let route = Route::for_entry_in(&catalog, &edited).unwrap();
        assert_eq!(
            route.request_url("m").unwrap().as_str(),
            "https://proxy.acme.test/v1/chat/completions"
        );
    }
}
