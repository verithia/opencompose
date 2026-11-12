//! Provider-agnostic conversation model, credential-safe transport helpers and
//! the adapter boundary. Native wire formats never leave this module.
pub mod anthropic;
pub mod auth;
pub mod catalog;
pub mod chat_completions;
pub mod credential_lifecycle;
pub mod credential_store;
pub mod effort;
pub mod gemini;
pub mod gemini_generate;
pub mod limits;
pub mod media;
pub mod models;
/// OAuth provider flows.
pub mod oauth;
pub mod prices;
pub mod project_instructions;
pub mod responses;
pub mod route;
pub mod secret;
pub mod session_usage;
pub mod sessions;
pub mod skill_hints;
pub mod skills;
pub mod sse;
pub mod timing;
pub mod toolsets;

#[cfg(test)]
mod dsl_examples;

use crate::commands::{AiProviderEntry, Wire};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Maximum distinct calls in one assistant batch; mirrored by the frontend contract test.
pub const MAX_TOOL_CALLS_PER_BATCH: usize = 32;

/// Messages one request may carry; mirrored by the panel loop's wire budget.
pub const MAX_WIRE_MESSAGES: usize = 100;

pub const LIMIT: usize = 2 * 1024 * 1024;
/// A whole turn may legitimately be far bigger than one frame: a reasoning
/// stream is discarded as it arrives, and a measured mimo turn crossed 2 MiB
/// with nothing wrong. The frame buffer keeps its 2 MiB bound.
pub const RESPONSE_BYTES_LIMIT: usize = 16 * 1024 * 1024;
pub const CONNECT_TIMEOUT_SECS: u64 = 15;
pub const STREAM_TIMEOUT_SECS: u64 = 600;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Thinking {
        text: String,
        /// The provider's signature over this reasoning, required back verbatim
        /// on an assistant turn that also carries tool calls.
        signature: Option<String>,
        /// Encrypted reasoning the provider withheld, replayed instead of text.
        redacted: Option<String>,
    },
    /// Attached image or audio. The saved form is a file reference with a
    /// content hash; base64 exists only inside a request being sent.
    Media {
        kind: media::MediaKind,
        mime: String,
        data: media::MediaData,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Every field of the provider's call object except `id`, `type` and
        /// `function`, carried opaquely. Providers may require these back in
        /// assistant history; nothing here interprets them.
        extra: Option<Value>,
    },
    ToolResult {
        call_id: String,
        name: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,
    /// Opaque provider continuity items (for example encrypted reasoning).
    pub continuity: Option<Value>,
    /// Every top-level field of the provider's message object except `role`,
    /// `content`, `tool_calls` and the fields this layer owns (`provider_state`,
    /// `finish_reason`, `usage`, the `ok` envelope), carried opaquely. A provider
    /// may require its own fields back on the assistant turn.
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StopReason {
    #[default]
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Auth,
    /// A credential was rejected after refresh, or is absent where required.
    AuthExpired,
    Permission,
    RateLimit,
    Timeout,
    Network,
    Redirect,
    Request,
    Server,
    Protocol,
    Truncated,
    Config,
}

impl ProviderErrorKind {
    /// Stable identifier for the UI; messages stay human-readable and English.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::AuthExpired => "auth_expired",
            Self::Permission => "permission",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Redirect => "redirect",
            Self::Request => "request",
            Self::Server => "server",
            Self::Protocol => "protocol",
            Self::Truncated => "truncated",
            Self::Config => "config",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: matches!(
                kind,
                ProviderErrorKind::RateLimit
                    | ProviderErrorKind::Server
                    | ProviderErrorKind::Timeout
                    | ProviderErrorKind::Network
            ),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Protocol, message)
    }

    /// A reply the loop cannot use (malformed tool arguments, empty): sending
    /// the same turn again may land, so the panel offers Retry.
    pub fn unusable(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Protocol,
            message: message.into(),
            retryable: true,
        }
    }

    /// A reply cut off by the output budget; more room or a smaller edit fixes
    /// it, so the panel offers Retry.
    pub fn truncated(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Truncated,
            message: message.into(),
            retryable: true,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Config, message)
    }

    /// A credential-store fault is a configuration error: the key or the store
    /// must be fixed, so resending the same turn cannot succeed.
    pub fn credential_store(detail: impl Into<String>) -> Self {
        Self::new(
            ProviderErrorKind::Config,
            format!(
                "Configuration error: the OS credential store could not be read, so this request has no API key. \
                 Save the API key again in the AI panel, or set OPENCOMPOSE_AI_KEY for a headless session; \
                 a retry cannot succeed until the credential is fixed. {}",
                detail.into()
            ),
        )
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TurnStarted,
    TextDelta(String),
    ThinkingDelta(String),
    /// The tool name is available while its arguments are still streaming.
    ToolPreparing(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// Canonical echo for consumers that replay tool results; wire adapters
    /// return tool results in the subsequent request rather than streaming them.
    #[allow(dead_code)]
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    Usage {
        input: u64,
        output: u64,
        reasoning: u64,
        /// `None` when the provider payload carried no cache field at all.
        cached: Option<u64>,
    },
    /// Provider response id when the payload exposes one; informational only.
    RequestId(String),
    Done {
        reason: StopReason,
    },
    Error {
        kind: ProviderErrorKind,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequest {
    pub system: Option<String>,
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u64,
    pub temperature: f64,
    /// Saved effort; adapters send it only for models whose catalog entry
    /// declares thinking levels.
    pub thinking_level: String,
    pub stream: bool,
}

/// A native protocol adapter. Implementations stream canonical events and
/// return the assistant turn; they never apply edits.
pub trait Provider: Send + Sync {
    fn stream<'a>(
        &'a self,
        request: &'a ProviderRequest,
        sink: &'a mut (dyn FnMut(StreamEvent) + Send),
    ) -> BoxFuture<'a, Result<Turn, ProviderError>>;
}

/// The protocol comes from the entry's catalog `wire`; auth is resolved first
/// and handed to the adapter as a per-request plan, never inferred or swapped.
/// The route (base URL and request path) is data resolved alongside it.
pub fn provider_for(
    entry: &AiProviderEntry,
    credential: auth::Credential,
) -> Result<Box<dyn Provider>, ProviderError> {
    let plan = auth::plan(entry, &credential)?;
    let route = route::Route::for_entry(entry).map_err(ProviderError::config)?;
    provider_routed(entry, route, plan)
}

/// Builds the adapter for an already-resolved route. A test can resolve the
/// route from a catalog fixture, which is how a data-only model addition is
/// proven against a local server.
pub(crate) fn provider_routed(
    entry: &AiProviderEntry,
    route: route::Route,
    plan: auth::AuthPlan,
) -> Result<Box<dyn Provider>, ProviderError> {
    match entry.wire() {
        Wire::OpenAiChatCompletions => Ok(Box::new(chat_completions::ChatCompletions::new(
            entry.clone(),
            route,
            plan,
        ))),
        Wire::OpenAiResponses => Ok(Box::new(responses::Responses::new(
            entry.clone(),
            route,
            plan,
        ))),
        Wire::AnthropicMessages => Ok(Box::new(anthropic::AnthropicMessages::new(
            entry.clone(),
            route,
            plan,
        ))),
        Wire::GeminiInteractions => Ok(Box::new(gemini::GeminiInteractions::new(
            entry.clone(),
            route,
            plan,
        ))),
        Wire::GeminiGenerate => Ok(Box::new(gemini_generate::GeminiGenerate::new(
            entry.clone(),
            route,
            plan,
        ))),
        Wire::Unknown => Err(ProviderError::config(
            "Unknown provider protocol. Save the provider settings again.",
        )),
    }
}

pub fn endpoint(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "Invalid endpoint URL")?;
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Use HTTPS (HTTP only on loopback), with no URL credentials, query or fragment.".into(),
        );
    }
    Ok(url)
}

/// A hosted turn is slow, not stalled: measured turns of the endpoints we use
/// took 144–237 s, so a 120 s *total* timeout cut every slow turn while a
/// stalled connection is what we actually want to notice. The read timeout is
/// the stall detector; the total timeout stays a safety net.
// Reasoning gateways may buffer an entire reasoning phase without SSE bytes.
// Keep a finite idle limit, independently of the ten-minute total request cap.
pub const READ_TIMEOUT_SECS: u64 = 300;

pub fn http_client(timeout_secs: u64) -> Result<reqwest::Client, ProviderError> {
    http_client_for("", timeout_secs)
}

/// A client for one endpoint. A loopback endpoint never goes through a proxy:
/// a local provider or a test listener is not reachable through the machine's
/// egress proxy, and routing it there turns a connection refusal into a 5xx.
pub fn http_client_for(
    endpoint: &str,
    timeout_secs: u64,
) -> Result<reqwest::Client, ProviderError> {
    let loopback = reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .map(|address| address.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .read_timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .timeout(Duration::from_secs(timeout_secs));
    let builder = if loopback {
        builder.no_proxy()
    } else {
        crate::net_proxy::apply(builder)
    };
    builder
        .build()
        .map_err(|_| ProviderError::config("HTTP client initialization failed"))
}

/// A stream that ends in a timeout is not the same failure as one that ends in
/// a reset, and the operator needs to know which: the first is "the provider
/// stopped talking", the second is "the connection broke".
pub fn stream_read_error(timed_out: bool) -> ProviderError {
    if timed_out {
        ProviderError::unusable(format!(
            "Provider request timed out (idle limit {READ_TIMEOUT_SECS} s; total limit {STREAM_TIMEOUT_SECS} s). \
             No complete answer was received. You can retry."
        ))
    } else {
        ProviderError::new(
            ProviderErrorKind::Protocol,
            "Failed reading provider response",
        )
    }
}

pub fn map_status(status: u16, provider_name: &str) -> ProviderError {
    match status {
        401 => ProviderError::new(
            ProviderErrorKind::Auth,
            format!(
                "Provider rejected authentication. Check the saved API key for {provider_name}."
            ),
        ),
        403 => ProviderError::new(
            ProviderErrorKind::Permission,
            "Provider denied permission for this model or endpoint.",
        ),
        429 => ProviderError::new(
            ProviderErrorKind::RateLimit,
            "Provider rate/quota limit reached. Retry later; no automatic billable retry was made.",
        ),
        300..=399 => ProviderError::new(
            ProviderErrorKind::Redirect,
            "Redirect refused. Configure the final HTTPS endpoint explicitly.",
        ),
        400..=499 => ProviderError::new(
            ProviderErrorKind::Request,
            format!(
                "Provider rejected the request (HTTP {status}). Check the model, endpoint and tool support."
            ),
        ),
        500..=599 => ProviderError::new(
            ProviderErrorKind::Server,
            format!("Provider is unavailable (HTTP {status}). Retry later."),
        ),
        _ => ProviderError::new(
            ProviderErrorKind::Protocol,
            format!("Provider returned HTTP {status}."),
        ),
    }
}

pub fn map_reqwest(error: reqwest::Error, provider_name: &str) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::new(ProviderErrorKind::Timeout, "AI request timed out");
    }
    if let Some(status) = error.status() {
        return map_status(status.as_u16(), provider_name);
    }
    ProviderError::new(
        ProviderErrorKind::Network,
        "AI connection failed; check endpoint, network and TLS",
    )
}

pub async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProviderError::protocol("Failed reading provider response"))?
    {
        total += chunk.len();
        if total > RESPONSE_BYTES_LIMIT {
            return Err(ProviderError::protocol(format!(
                "Provider response exceeds {} MiB limit",
                RESPONSE_BYTES_LIMIT / (1024 * 1024)
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Sends the turn and deltas to the channel; `emitted` prevents a 401 retry
/// from replaying a partially streamed turn. `stats` records transport timing;
/// dropping the future mid-stream commits a failed turn with its partial timing.
pub async fn run(
    provider: &dyn Provider,
    request: &ProviderRequest,
    channel: &Option<tauri::ipc::Channel<String>>,
    emitted: &mut bool,
    stats: &timing::TurnStatsHandle,
) -> Result<Value, ProviderError> {
    run_with_thinking(provider, request, channel, &None, emitted, stats).await
}

/// Streams provider-exposed reasoning separately from the answer text.
pub async fn run_with_thinking(
    provider: &dyn Provider,
    request: &ProviderRequest,
    channel: &Option<tauri::ipc::Channel<String>>,
    thinking: &Option<tauri::ipc::Channel<String>>,
    emitted: &mut bool,
    stats: &timing::TurnStatsHandle,
) -> Result<Value, ProviderError> {
    run_with_activity(provider, request, channel, thinking, &None, emitted, stats).await
}

pub async fn run_with_activity(
    provider: &dyn Provider,
    request: &ProviderRequest,
    channel: &Option<tauri::ipc::Channel<String>>,
    thinking: &Option<tauri::ipc::Channel<String>>,
    preparing: &Option<tauri::ipc::Channel<String>>,
    emitted: &mut bool,
    stats: &timing::TurnStatsHandle,
) -> Result<Value, ProviderError> {
    let mut clock = timing::Clock::start(stats);
    let mut usage: Option<timing::Usage> = None;
    let result = {
        let mut sink = |event: StreamEvent| match event {
            StreamEvent::TurnStarted => {
                if let Some(channel) = preparing {
                    let _ = channel.send(String::new());
                }
            }
            StreamEvent::TextDelta(text) => {
                *emitted = true;
                clock.content();
                if let Some(channel) = channel {
                    let _ = channel.send(text);
                }
            }
            StreamEvent::ThinkingDelta(text) => {
                clock.content();
                if let Some(channel) = thinking {
                    *emitted = true;
                    let _ = channel.send(text);
                }
            }
            StreamEvent::ToolPreparing(name) => {
                clock.content();
                if let Some(channel) = preparing {
                    let _ = channel.send(name);
                }
            }
            StreamEvent::RequestId(id) => clock.request_id(&id),
            StreamEvent::Usage {
                input,
                output,
                reasoning,
                cached,
            } => {
                let canonical = timing::Usage {
                    input,
                    cached_input: cached,
                    output,
                    reasoning,
                };
                clock.usage(canonical);
                usage = Some(canonical);
            }
            _ => {}
        };
        sink(StreamEvent::TurnStarted);
        let result = provider.stream(request, &mut sink).await;
        if let Err(error) = &result {
            sink(StreamEvent::Error {
                kind: error.kind,
                message: error.message.clone(),
                retryable: error.retryable,
            });
        }
        result
    };
    match result {
        Ok(turn) => {
            clock.finish(false);
            let mut message = turn_to_wire(&turn);
            if let Some(usage) = usage {
                message["usage"] = serde_json::to_value(usage).unwrap_or(Value::Null);
            }
            Ok(message)
        }
        Err(error) => {
            clock.finish(true);
            Err(error)
        }
    }
}

/// Tool specs handed to the provider, narrowed by the conversation's toolsets;
/// `None` keeps the whole panel surface.
pub fn tool_specs_for(selection: Option<&[String]>) -> Vec<ToolSpec> {
    crate::tool_registry::panel_tools_for(selection)
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool["name"].as_str().unwrap_or_default().to_string(),
            description: tool["description"].as_str().unwrap_or_default().to_string(),
            parameters: tool["inputSchema"].clone(),
        })
        .collect()
}

/// The OpenAI `tools` encoding of a tool list. It is the same shape whether or
/// not the model calls tools natively; the selection decides the contents.
pub fn openai_tools(tools: &[ToolSpec]) -> Value {
    json!(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type":"function",
                    "function":{
                        "name":tool.name,
                        "description":tool.description,
                        "parameters":tool.parameters
                    }
                })
            })
            .collect::<Vec<_>>()
    )
}

/// The whole panel surface in the OpenAI encoding; only tests need it now that
/// a request carries its own selection.
#[cfg(test)]
pub fn legacy_tools() -> Value {
    openai_tools(&tool_specs_for(None))
}

/// Builds the request and resolves attachments. Returns the media notices so
/// the caller can tell the UI that a file changed on disk since it was attached.
pub fn request_from_wire(
    entry: &AiProviderEntry,
    messages: Value,
    use_tools: bool,
    toolsets: Option<&[String]>,
) -> Result<(ProviderRequest, Vec<media::MediaNotice>), ProviderError> {
    let mut turns = turns_from_wire(&messages)?;
    let system = system_text(&turns);
    turns.retain(|turn| turn.role != Role::System);
    media::validate(entry, &turns)?;
    let mut request = ProviderRequest {
        system,
        turns,
        tools: if use_tools {
            tool_specs_for(toolsets)
        } else {
            vec![]
        },
        max_tokens: limits::max_output(entry),
        temperature: entry.temperature,
        thinking_level: entry.thinking_level.clone(),
        stream: entry.capabilities.streaming,
    };
    let notices = media::resolve(&mut request)?;
    Ok((request, notices))
}

pub fn system_text(turns: &[Turn]) -> Option<String> {
    let parts = turns
        .iter()
        .filter(|turn| turn.role == Role::System)
        .flat_map(|turn| {
            turn.blocks.iter().filter_map(|block| match block {
                Block::Text(text) => Some(text.as_str()),
                _ => None,
            })
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub fn turns_from_wire(messages: &Value) -> Result<Vec<Turn>, ProviderError> {
    let messages = messages
        .as_array()
        .ok_or_else(|| ProviderError::config("Invalid AI request messages"))?;
    if messages.is_empty() || messages.len() > MAX_WIRE_MESSAGES {
        return Err(ProviderError::config("Invalid or oversized AI request"));
    }
    let mut names = std::collections::HashMap::new();
    for message in messages {
        if message["role"] == "assistant"
            && let Some(calls) = message["tool_calls"].as_array()
        {
            for call in calls {
                let id = call["id"].as_str().unwrap_or_default();
                let name = call["function"]["name"].as_str().unwrap_or_default();
                if !id.is_empty() && !name.is_empty() {
                    names.insert(id.to_owned(), name.to_owned());
                }
            }
        }
    }
    let mut turns = Vec::with_capacity(messages.len());
    for message in messages {
        let role = message["role"]
            .as_str()
            .ok_or_else(|| ProviderError::config("Invalid AI message role"))?;
        let content = match &message["content"] {
            Value::Null => "",
            Value::String(text) => text.as_str(),
            _ => {
                return Err(ProviderError::config(
                    "Unsupported AI message content shape",
                ));
            }
        };
        match role {
            "system" => turns.push(Turn {
                role: Role::System,
                blocks: vec![Block::Text(content.to_owned())],
                continuity: None,
                extra: None,
            }),
            "user" => {
                let mut blocks = vec![Block::Text(content.to_owned())];
                blocks.extend(media_blocks(message, role)?);
                if let Some(part) = message.get("input_audio") {
                    blocks.push(media::inline_audio(part)?);
                }
                turns.push(Turn {
                    role: Role::User,
                    blocks,
                    continuity: None,
                    extra: None,
                });
            }
            "assistant" => {
                let mut blocks = thinking_from_wire(message);
                if !content.is_empty() {
                    blocks.push(Block::Text(content.to_owned()));
                }
                if let Some(calls) = message["tool_calls"].as_array() {
                    if calls.len() > MAX_TOOL_CALLS_PER_BATCH {
                        return Err(ProviderError::config(format!(
                            "Tool batch has {} calls; limit is {MAX_TOOL_CALLS_PER_BATCH}",
                            calls.len()
                        )));
                    }
                    for call in calls {
                        let id = call["id"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| ProviderError::config("Malformed tool call envelope"))?;
                        let name = call["function"]["name"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .ok_or_else(|| ProviderError::config("Malformed tool call envelope"))?;
                        let arguments = call["function"]["arguments"]
                            .as_str()
                            .ok_or_else(|| ProviderError::config("Malformed tool call envelope"))?;
                        let input = serde_json::from_str(arguments)
                            .map_err(|_| ProviderError::config("Malformed tool call arguments"))?;
                        blocks.push(Block::ToolUse {
                            id: id.to_owned(),
                            name: name.to_owned(),
                            input,
                            extra: tool_call_extra(call),
                        });
                    }
                }
                turns.push(Turn {
                    role: Role::Assistant,
                    blocks,
                    continuity: message.get("provider_state").cloned(),
                    extra: message_extra(message),
                });
            }
            "tool" => {
                let call_id = message["tool_call_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| ProviderError::config("Missing tool call id"))?;
                turns.push(Turn {
                    role: Role::Tool,
                    blocks: vec![Block::ToolResult {
                        call_id: call_id.to_owned(),
                        name: names.get(call_id).cloned().unwrap_or_default(),
                        content: content.to_owned(),
                        is_error: false,
                    }],
                    continuity: None,
                    extra: None,
                });
            }
            _ => return Err(ProviderError::config("Unknown AI message role")),
        }
    }
    Ok(turns)
}

/// Attachments travel beside the text as descriptors, never as inline bytes.
fn media_blocks(message: &Value, role: &str) -> Result<Vec<Block>, ProviderError> {
    let Some(entries) = message.get("media").filter(|entries| !entries.is_null()) else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| ProviderError::config("Invalid attachment list"))?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    if role != "user" {
        return Err(ProviderError::config(
            "Attachments are only allowed on user messages",
        ));
    }
    if entries.len() > media::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ProviderError::config("Too many attachments on one message"));
    }
    entries
        .iter()
        .map(|entry| {
            let (kind, reference) = media::from_wire(entry)?;
            Ok(Block::Media {
                kind,
                mime: reference.mime.clone(),
                data: media::MediaData::File(reference),
            })
        })
        .collect()
}

/// Session and UI metadata are local-only, including unknown future envelope versions.
pub(crate) fn is_local_message_field(key: &str) -> bool {
    matches!(
        key,
        "message_meta"
            | "context"
            | "media"
            | "media_notices"
            | "session_warning"
            | "input_audio"
            | "source_render"
            | "source_render_missing"
            | "source_render_unavailable"
            | "dsl_sha"
    )
}

/// Fields of a wire message object this layer does not interpret, kept so a
/// provider that requires them back in assistant history gets them verbatim.
pub(crate) fn message_extra(message: &Value) -> Option<Value> {
    let object = message.as_object()?;
    // A `thinking` array is read into blocks, so replaying it here too would
    // send the reasoning twice. A provider that spells the field as anything
    // else keeps it opaque and gets it back untouched.
    let structured_thinking = message.get("thinking").is_some_and(Value::is_array);
    let extra: serde_json::Map<String, Value> = object
        .iter()
        .filter(|(key, _)| {
            !(matches!(
                key.as_str(),
                "role"
                    | "content"
                    | "tool_calls"
                    | "provider_state"
                    | "finish_reason"
                    | "usage"
                    | "ok"
            ) || is_local_message_field(key)
                || (structured_thinking && key.as_str() == "thinking"))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (!extra.is_empty()).then_some(Value::Object(extra))
}

/// Fields of a wire call object this layer does not interpret, kept so a
/// provider that requires them back in assistant history gets them verbatim.
pub(crate) fn tool_call_extra(call: &Value) -> Option<Value> {
    let object = call.as_object()?;
    let extra: serde_json::Map<String, Value> = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "type" | "function"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (!extra.is_empty()).then_some(Value::Object(extra))
}

/// The saved call shape plus whatever the provider sent beside it.
pub(crate) fn tool_call_to_wire(
    id: &str,
    name: &str,
    input: &Value,
    extra: &Option<Value>,
) -> Value {
    let mut call = json!({
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": input.to_string()}
    });
    if let (Some(Value::Object(extra)), Some(object)) = (extra, call.as_object_mut()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    call
}

/// The assistant wire message. Reasoning travels in a `thinking` array beside
/// the prose and the tool envelope, so a signed turn can be replayed verbatim.
///
/// The wire keeps the provider's canonical block order — reasoning, then prose,
/// then tool calls — because that is the order a thinking-capable model emits
/// and the order the Anthropic wire requires on an assistant turn with calls.
pub fn turn_to_wire(turn: &Turn) -> Value {
    let text = turn
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let thinking = turn
        .blocks
        .iter()
        .filter_map(thinking_to_wire)
        .collect::<Vec<_>>();
    let calls = turn
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::ToolUse {
                id,
                name,
                input,
                extra,
            } => Some(tool_call_to_wire(id, name, input, extra)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }
    });
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    if !thinking.is_empty() {
        message["thinking"] = json!(thinking);
    }
    if let Some(continuity) = &turn.continuity {
        message["provider_state"] = continuity.clone();
    }
    if let (Some(Value::Object(extra)), Some(object)) = (&turn.extra, message.as_object_mut()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    message
}

/// One `thinking` entry: the reasoning text plus whichever provider state the
/// turn has to give back — a signature, or the encrypted payload of reasoning
/// the provider redacted. An entry with neither would replay nothing.
fn thinking_to_wire(block: &Block) -> Option<Value> {
    let Block::Thinking {
        text,
        signature,
        redacted,
    } = block
    else {
        return None;
    };
    if text.is_empty() && signature.is_none() && redacted.is_none() {
        return None;
    }
    let mut entry = json!({"text": text});
    if let Some(signature) = signature {
        entry["signature"] = json!(signature);
    }
    if let Some(redacted) = redacted {
        entry["redacted"] = json!(redacted);
    }
    Some(entry)
}

/// The reasoning blocks an assistant wire message carries, in the order the
/// provider produced them. A malformed entry is skipped: this layer replays
/// provider state, it does not interpret it.
fn thinking_from_wire(message: &Value) -> Vec<Block> {
    let Some(entries) = message["thinking"].as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let text = entry["text"].as_str().unwrap_or_default();
            let signature = entry["signature"]
                .as_str()
                .filter(|signature| !signature.is_empty())
                .map(str::to_owned);
            let redacted = entry["redacted"]
                .as_str()
                .filter(|redacted| !redacted.is_empty())
                .map(str::to_owned);
            if text.is_empty() && signature.is_none() && redacted.is_none() {
                return None;
            }
            Some(Block::Thinking {
                text: text.to_owned(),
                signature,
                redacted,
            })
        })
        .collect()
}

pub fn assistant_turn_from_wire(message: &Value) -> Result<Turn, ProviderError> {
    let text = message["content"].as_str().unwrap_or_default();
    let mut blocks = thinking_from_wire(message);
    if !text.is_empty() {
        blocks.push(Block::Text(text.to_owned()));
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        if calls.len() > MAX_TOOL_CALLS_PER_BATCH {
            return Err(ProviderError::protocol(format!(
                "Tool batch has {} calls; limit is {MAX_TOOL_CALLS_PER_BATCH}",
                calls.len()
            )));
        }
        for call in calls {
            let id = call["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| ProviderError::protocol("Malformed tool call envelope"))?;
            let name = call["function"]["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| ProviderError::protocol("Malformed tool call envelope"))?;
            let arguments = call["function"]["arguments"]
                .as_str()
                .ok_or_else(|| ProviderError::protocol("Malformed tool call envelope"))?;
            let input = serde_json::from_str(arguments).map_err(|_| {
                ProviderError::unusable("Provider returned malformed tool arguments")
            })?;
            blocks.push(Block::ToolUse {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                extra: tool_call_extra(call),
            });
        }
    }
    Ok(Turn {
        role: Role::Assistant,
        blocks,
        continuity: message.get("provider_state").cloned(),
        extra: message_extra(message),
    })
}

pub fn tool_use_events(turn: &Turn, sink: &mut dyn FnMut(StreamEvent)) {
    for block in &turn.blocks {
        if let Block::ToolUse {
            id, name, input, ..
        } = block
        {
            sink(StreamEvent::ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
        }
    }
}

pub fn block_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Serves one canned HTTP response on loopback and returns the raw request.
    pub fn canned(status: u16, body: &'static str) -> (String, JoinHandle<String>) {
        let (base, server) = canned_bytes(status, body.as_bytes().to_vec());
        (
            base,
            std::thread::spawn(move || String::from_utf8(server.join().unwrap()).unwrap()),
        )
    }

    /// Byte variant for request bodies that are not UTF-8 (for example zstd).
    pub fn canned_bytes(status: u16, body: Vec<u8>) -> (String, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_request(&mut socket);
            write_response(&mut socket, status, &body);
            request
        });
        (base, server)
    }

    /// Serves one response per connection, in order, and returns the raw request
    /// for each. Used by adapters that re-send a request to continue a response.
    pub fn canned_sequence(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                let request = read_request(&mut socket);
                write_response(&mut socket, status, body.as_bytes());
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });
        (base, server)
    }

    /// Serves one response per connection, with the status and body chosen from
    /// the request that arrived. A fixture provider can then reject a replayed
    /// history that lost a field the first response carried.
    pub fn canned_by_request(
        count: usize,
        mut respond: impl FnMut(&str) -> (u16, String) + Send + 'static,
    ) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..count {
                let (mut socket, _) = listener.accept().unwrap();
                let request = read_request(&mut socket);
                let text = String::from_utf8_lossy(&request).to_string();
                let (status, body) = respond(&text);
                write_response(&mut socket, status, body.as_bytes());
                requests.push(text);
            }
            requests
        });
        (base, server)
    }

    fn read_request(socket: &mut TcpStream) -> Vec<u8> {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut data = Vec::new();
        let mut part = [0u8; 4096];
        loop {
            let count = socket.read(&mut part).unwrap();
            if count == 0 {
                break;
            }
            data.extend_from_slice(&part[..count]);
            if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&data[..end]);
                let size = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if data.len() >= end + 4 + size {
                    break;
                }
            }
        }
        data
    }

    fn write_response(socket: &mut TcpStream, status: u16, body: &[u8]) {
        let mut head = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        head.extend_from_slice(body);
        socket.write_all(&head).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::limits::{self, InputModalities, ModelLimits};
    use crate::ai::test_support::canned;
    use crate::commands::{AiCapabilities, AiProviderEntry, Wire};
    use serde_json::json;

    #[test]
    fn null_optional_media_is_not_an_attachment_list() {
        for media in [Value::Null, json!([])] {
            let turns =
                turns_from_wire(&json!([{"role":"user", "content":"Hello", "media":media}]))
                    .unwrap();
            assert_eq!(turns[0].blocks, vec![Block::Text("Hello".into())]);
        }
        assert!(turns_from_wire(&json!([{"role":"user", "content":"Hello", "media":{}}])).is_err());
    }

    #[test]
    fn tool_batch_limit_accepts_large_wire_batches_and_rejects_overflow() {
        let calls: Vec<_> = (0..MAX_TOOL_CALLS_PER_BATCH + 1)
            .map(|i| {
                json!({
                    "id": format!("call-{i}"), "type":"function",
                    "function":{"name":"tracks_list","arguments":"{}"}
                })
            })
            .collect();
        let accepted = assistant_turn_from_wire(&json!({"role":"assistant","content":null,"tool_calls":&calls[..MAX_TOOL_CALLS_PER_BATCH]})).unwrap();
        assert_eq!(accepted.blocks.len(), MAX_TOOL_CALLS_PER_BATCH);
        let replay = turns_from_wire(&json!([{"role":"assistant","content":null,"tool_calls":&calls[..MAX_TOOL_CALLS_PER_BATCH]}])).unwrap();
        assert_eq!(replay[0].blocks.len(), MAX_TOOL_CALLS_PER_BATCH);
        assert!(
            turns_from_wire(&json!([{"role":"assistant","content":null,"tool_calls":calls}]))
                .is_err()
        );
        assert!(
            assistant_turn_from_wire(
                &json!({"role":"assistant","content":null,"tool_calls":calls})
            )
            .is_err()
        );
    }

    /// A signed reasoning block survives the wire round trip ahead of the prose
    /// and the call, so the next request replays the turn the provider signed.
    #[test]
    fn a_signed_thinking_block_survives_the_wire_round_trip() {
        let turn = Turn {
            role: Role::Assistant,
            blocks: vec![
                Block::Thinking {
                    text: "Consider the bass.".into(),
                    signature: Some("sig-1".into()),
                    redacted: None,
                },
                Block::Text("Reading the track.".into()),
                Block::ToolUse {
                    id: "call-1".into(),
                    name: "read_context".into(),
                    input: json!({"path": "track:bass"}),
                    extra: None,
                },
            ],
            continuity: None,
            extra: None,
        };
        let wire = turn_to_wire(&turn);
        assert_eq!(
            wire["thinking"],
            json!([{"text": "Consider the bass.", "signature": "sig-1"}]),
            "{wire}"
        );
        assert_eq!(turns_from_wire(&json!([wire.clone()])).unwrap()[0], turn);
        // The provider-response path reads the same field.
        assert_eq!(assistant_turn_from_wire(&wire).unwrap(), turn);
    }

    /// Reasoning the provider withheld travels as its encrypted payload, in the
    /// same array and the same order as the readable blocks.
    #[test]
    fn redacted_reasoning_round_trips_as_its_payload() {
        let turn = Turn {
            role: Role::Assistant,
            blocks: vec![
                Block::Thinking {
                    text: String::new(),
                    signature: None,
                    redacted: Some("enc-1".into()),
                },
                Block::Thinking {
                    text: "scratch".into(),
                    signature: Some("sig-2".into()),
                    redacted: None,
                },
            ],
            continuity: None,
            extra: None,
        };
        let wire = turn_to_wire(&turn);
        assert_eq!(
            wire["thinking"],
            json!([
                {"text": "", "redacted": "enc-1"},
                {"text": "scratch", "signature": "sig-2"},
            ]),
            "{wire}"
        );
        assert_eq!(turns_from_wire(&json!([wire])).unwrap()[0], turn);
    }

    /// A turn with no reasoning adds no field, so a provider that does not
    /// support thinking receives the message it received before.
    #[test]
    fn a_turn_without_reasoning_sends_no_thinking_field() {
        let turn = Turn {
            role: Role::Assistant,
            blocks: vec![Block::Text("Done".into())],
            continuity: None,
            extra: None,
        };
        let wire = turn_to_wire(&turn);
        assert!(wire.get("thinking").is_none(), "{wire}");
        // A wire message from before the field existed still builds a turn.
        let legacy = json!({"role":"assistant","content":"Done"});
        assert_eq!(turns_from_wire(&json!([legacy])).unwrap()[0], turn);
    }

    fn fixture_entry(id: &str, model: &str, endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: id.into(),
            name: "Wire fixture".into(),
            kind: crate::commands::ProviderKind::OpenAiChatCompletions,
            wire: Some(Wire::OpenAiChatCompletions),
            auth: Default::default(),
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: model.into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 262_144,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                streaming: true,
                ..Default::default()
            },
        }
    }

    fn text_only() -> InputModalities {
        InputModalities {
            text: true,
            ..Default::default()
        }
    }

    fn small_output() -> ModelLimits {
        ModelLimits {
            context: 64_000,
            max_output: 1_234,
            input: text_only(),
            price: None,
        }
    }

    #[test]
    fn request_from_wire_takes_the_output_limit_from_the_merged_catalog() {
        limits::install_for_test("wire-fixture", "third-party-model", small_output());
        let entry = fixture_entry(
            "wire-fixture",
            "third-party-model",
            "https://models.example/v1/chat/completions".into(),
        );
        let (request, _) =
            request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                .unwrap();
        assert_eq!(request.max_tokens, 1_234);
    }

    #[test]
    fn the_request_carries_the_saved_thinking_level() {
        let mut entry = fixture_entry(
            "wire-level",
            "gemini-4-argon",
            "https://generativelanguage.googleapis.com/v1beta/interactions".into(),
        );
        entry.wire = Some(Wire::GeminiInteractions);
        entry.thinking_level = "high".into();
        let (request, _) =
            request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                .unwrap();
        assert_eq!(request.thinking_level, "high");
        assert_eq!(
            catalog::allowed_thinking_levels(&entry),
            ["low", "medium", "high"]
        );
    }

    #[test]
    fn a_model_without_a_merged_entry_keeps_the_saved_output_budget() {
        let entry = fixture_entry(
            "wire-fixture-unlisted",
            "unlisted-model",
            "https://models.example/v1/chat/completions".into(),
        );
        let (request, _) =
            request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                .unwrap();
        assert_eq!(request.max_tokens, 262_144);
    }

    #[tokio::test]
    async fn chat_completions_sends_the_merged_output_limit() {
        const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
            data: [DONE]\n\n";
        limits::install_for_test("chat-wire-fixture", "chat-wire-model", small_output());
        let (base, server) = canned(200, SSE);
        let entry = fixture_entry(
            "chat-wire-fixture",
            "chat-wire-model",
            format!("{base}v1/chat/completions"),
        );
        let (request, _) =
            request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                .unwrap();
        assert_eq!(request.max_tokens, 1_234);
        let provider = provider_for(
            &entry,
            auth::Credential::ApiKey(Some("fixture-secret".into())),
        )
        .unwrap();
        let mut emitted = false;
        let stats = timing::TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
        run(provider.as_ref(), &request, &None, &mut emitted, &stats)
            .await
            .unwrap();
        let raw = server.join().unwrap();
        let body: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["max_tokens"], 1_234);
    }

    /// Every bundled row resolves through the generic path: no model id needs a
    /// code-side registration to build a request.
    #[test]
    fn every_bundled_model_builds_a_request_from_its_catalog_row() {
        for provider in &catalog::catalog().providers {
            for model in &provider.models {
                let mut entry = fixture_entry(
                    &provider.id,
                    &model.id,
                    "http://127.0.0.1:9/placeholder".into(),
                );
                entry.wire = Some(provider.wire);
                let request =
                    request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                        .map(|(request, _)| request)
                        .unwrap_or_else(|error| panic!("{} / {}: {error}", provider.id, model.id));
                assert_eq!(request.max_tokens, model.max_output);
                provider_for(
                    &entry,
                    auth::Credential::ApiKey(Some("fixture-secret".into())),
                )
                .unwrap_or_else(|error| panic!("{} / {}: {error}", provider.id, model.id));
            }
        }
    }

    /// The acceptance shape: a provider + model that exist only in catalog data.
    /// The route, the listing, the model id and the model's request flags all
    /// come from that data; nothing about this model is in code.
    #[tokio::test]
    async fn a_model_that_exists_only_in_catalog_data_is_listed_and_sent() {
        const CATALOG: &str = r#"{
            "version": "fixture",
            "providers": [{
                "id": "acme",
                "name": "Acme",
                "wire": "openai-chat-completions",
                "base_url": "BASE_URL",
                "path": "chat/completions",
                "auth": "bearer",
                "hosts": ["127.0.0.1"],
                "models": [{
                    "id": "acme-reasoner",
                    "name": "Acme Reasoner",
                    "context": 128000,
                    "max_output": 32000,
                    "reasoning": true,
                    "capabilities": {
                        "text": true, "audio_in": false, "audio_out": false,
                        "native_tool_calling": true, "streaming": false
                    },
                    "status": "active"
                }]
            }]
        }"#;
        let (base, server) = test_support::canned_sequence(vec![
            (
                200,
                r#"{"data":[{"id":"acme-reasoner"},{"id":"acme-small"}]}"#,
            ),
            (
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
            ),
        ]);
        let base_url = format!("{base}v1");
        let fixture = catalog::parse(&CATALOG.replace("BASE_URL", &base_url)).unwrap();
        assert_eq!(
            fixture.providers[0].base_url.as_deref(),
            Some(base_url.as_str())
        );
        let provider = fixture.providers.first().unwrap();
        // The model is listed by the catalog data, not by the provider listing.
        assert!(
            provider
                .models
                .iter()
                .any(|model| model.id == "acme-reasoner"),
            "{:?}",
            provider.models
        );

        let mut entry = fixture_entry("acme", "acme-reasoner", provider.base_url.clone().unwrap());
        entry.wire = Some(Wire::OpenAiChatCompletions);
        entry.capabilities.streaming = false;
        let _fixture = catalog::install_for_test(fixture.clone());
        let route = route::Route::for_entry_in(&fixture, &entry).unwrap();
        let key = crate::ai::secret::Secret::new("fixture-secret");

        let listing = crate::ai::models::fetch_route(&entry, &route, Some(key))
            .await
            .unwrap();
        assert_eq!(
            listing
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["acme-reasoner", "acme-small"]
        );

        let plan = auth::plan(
            &entry,
            &auth::Credential::ApiKey(Some("fixture-secret".into())),
        )
        .unwrap();
        let provider = provider_routed(&entry, route, plan).unwrap();
        let (request, _) =
            request_from_wire(&entry, json!([{"role":"user","content":"hi"}]), false, None)
                .unwrap();
        assert_eq!(request.max_tokens, 32_000, "the catalog output limit");
        let mut emitted = false;
        let stats = timing::TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
        run(provider.as_ref(), &request, &None, &mut emitted, &stats)
            .await
            .unwrap();

        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /v1/models"), "{}", requests[0]);
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"),
            "{}",
            requests[0]
        );
        assert!(
            requests[1].starts_with("POST /v1/chat/completions"),
            "{}",
            requests[1]
        );
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"),
            "{}",
            requests[1]
        );
        let body: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "acme-reasoner");
        // `reasoning: true` in the data selects the reasoning request shape, and
        // the saved level is what the request carries.
        assert_eq!(body["max_completion_tokens"], 32_000);
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("max_tokens").is_none(), "{body}");
        assert!(body.get("temperature").is_none(), "{body}");
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    /// A stalled stream and a broken connection are different failures; the
    /// operator has to be able to tell them apart.
    #[test]
    fn a_stream_timeout_says_the_provider_stopped_sending() {
        let timed_out = stream_read_error(true);
        assert!(
            timed_out.message.contains("timed out"),
            "{}",
            timed_out.message
        );
        assert!(timed_out.message.contains(&READ_TIMEOUT_SECS.to_string()));
        assert!(
            timed_out.retryable,
            "a stalled read is resent by the panel, not shown as a config error"
        );
        let broken = stream_read_error(false);
        assert!(broken.message.contains("Failed reading provider response"));
        assert_eq!(broken.kind, ProviderErrorKind::Protocol);
    }

    /// The three provider replies the capability matrix reproduced all end the
    /// turn the same way: one row whose Retry resends the turn. A bad key or a
    /// bad request stays non-retryable, because sending it again cannot help.
    #[test]
    fn unusable_replies_are_retryable_and_configuration_errors_are_not() {
        let malformed = assistant_turn_from_wire(&serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "tracks_list", "arguments": "{not json" }
            }]
        }))
        .unwrap_err();
        assert_eq!(malformed.kind, ProviderErrorKind::Protocol);
        assert!(malformed.retryable, "{}", malformed.message);

        let empty = ProviderError::unusable("provider returned an empty reply");
        assert!(empty.retryable);
        let truncated = ProviderError::truncated("Completion truncated.");
        assert!(truncated.retryable);
        assert_eq!(truncated.kind, ProviderErrorKind::Truncated);

        let auth = map_status(401, "fixture");
        assert!(
            !auth.retryable,
            "a rejected key cannot be retried into working"
        );
        assert!(!ProviderError::config("no endpoint").retryable);
        assert!(!map_status(400, "fixture").retryable);
    }

    /// A missing credential is configuration, not a provider outage: resending
    /// cannot succeed until the key or the store is fixed. A rate limit still
    /// carries Retry.
    #[test]
    fn a_credential_store_failure_is_configuration_and_a_rate_limit_is_retryable() {
        let error = ProviderError::credential_store(
            "Cannot unlock/read OS credential store. No plaintext fallback is used.",
        );
        assert_eq!(error.kind, ProviderErrorKind::Config);
        assert!(!error.retryable, "{}", error.message);
        assert!(
            error.message.contains("Configuration error"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("Save the API key"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("OPENCOMPOSE_AI_KEY"),
            "{}",
            error.message
        );

        let limited = map_status(429, "fixture");
        assert_eq!(limited.kind, ProviderErrorKind::RateLimit);
        assert!(limited.retryable, "a 429 still offers Retry");
    }

    /// The client carries a stall detector as well as a total budget: measured
    /// hosted turns take 144–237 s, so a total timeout alone cuts them.
    #[test]
    fn the_client_allows_a_slow_turn_but_not_a_stalled_one() {
        const { assert!(READ_TIMEOUT_SECS < STREAM_TIMEOUT_SECS) };
        assert!(http_client(STREAM_TIMEOUT_SECS).is_ok());
    }
}

#[cfg(test)]
mod response_limit_tests {
    use super::*;

    /// A reasoning stream is discarded as it arrives; a measured turn crossed
    /// 2 MiB with nothing wrong, so the whole-turn ceiling must be far above a
    /// single frame's.
    #[test]
    fn a_whole_turn_may_be_much_larger_than_one_frame() {
        const { assert!(RESPONSE_BYTES_LIMIT >= 8 * LIMIT) };
        assert_eq!(RESPONSE_BYTES_LIMIT / (1024 * 1024), 16);
    }
}
