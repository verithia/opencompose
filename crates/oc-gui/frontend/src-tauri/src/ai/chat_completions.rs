//! OpenAI Chat Completions transport, preserved for DeepSeek, Ollama and
//! custom OpenAI-compatible endpoints.
use super::auth::AuthPlan;
use super::media::MediaKind;
use super::route::Route;
use super::{
    Block, BoxFuture, MAX_TOOL_CALLS_PER_BATCH, Provider, ProviderError, ProviderErrorKind,
    ProviderRequest, Role, StopReason, StreamEvent, Turn, assistant_turn_from_wire, map_reqwest,
    map_status, read_limited, sse,
};
use crate::commands::AiProviderEntry;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tauri::ipc::Channel;

/// OpenAI chat-completions adapter.
pub struct ChatCompletions {
    entry: AiProviderEntry,
    route: Route,
    auth: AuthPlan,
}

impl ChatCompletions {
    /// Create the adapter from an entry, route and auth plan.
    pub fn new(entry: AiProviderEntry, route: Route, auth: AuthPlan) -> Self {
        Self { entry, route, auth }
    }

    /// Fixture construction: a test names a saved endpoint and the route is
    /// resolved from the bundled catalog exactly as production does.
    #[cfg(test)]
    pub(crate) fn for_test(entry: AiProviderEntry, auth: AuthPlan) -> Self {
        let route = Route::for_entry(&entry).expect("fixture endpoint is a valid route");
        Self::new(entry, route, auth)
    }

    fn body(&self, request: &ProviderRequest) -> Result<Value, ProviderError> {
        self.body_with(request, false)
    }

    /// Rebuilds the same request without provider-private history fields. A
    /// conversation carried across gateways can contain signatures and
    /// reasoning payloads the new converter refuses as an unsupported shape.
    fn sanitized_body(&self, request: &ProviderRequest) -> Result<Value, ProviderError> {
        self.body_with(request, true)
    }

    fn body_with(&self, request: &ProviderRequest, sanitize: bool) -> Result<Value, ProviderError> {
        let model = &self.entry.model;
        let mut body = json!({
            "model": model,
            "messages": chat_messages(request, model, sanitize)?,
            "stream": request.stream
        });
        if reasoning_model(&self.entry, model) {
            body["max_completion_tokens"] = json!(request.max_tokens);
            body["reasoning_effort"] = json!(super::effort::effort(&request.thinking_level));
        } else {
            body["max_tokens"] = json!(request.max_tokens);
            body["temperature"] = json!(request.temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = super::openai_tools(&request.tools);
            body["tool_choice"] = json!("auto");
        }
        // A streamed reply carries no usage unless the request asks for it, and
        // the panel's token figures come from that final chunk. A custom
        // gateway's conversion layer often rejects `stream_options`, so only
        // send it to a vendor route that expects the OpenAI shape.
        if request.stream && super::catalog::requires_credential(&self.entry) {
            body["stream_options"] = json!({ "include_usage": true });
        }
        Ok(body)
    }

    /// Keeps the tools and the output budget while dropping the reasoning
    /// parameters a compatible gateway rejects. Tool calling is the point of
    /// this surface; the last-resort body below gives it up only after this
    /// shape is refused too.
    fn compatible_body(&self, request: &ProviderRequest) -> Result<Value, ProviderError> {
        let messages = chat_messages(request, &self.entry.model, true)?;
        let mut body = json!({
            "model": self.entry.model,
            "messages": compatible_messages(messages.as_array().map(Vec::as_slice).unwrap_or_default()),
            "stream": request.stream,
            "max_tokens": request.max_tokens
        });
        if !request.tools.is_empty() {
            body["tools"] = super::openai_tools(&request.tools);
            body["tool_choice"] = json!("auto");
        }
        Ok(body)
    }

    /// The smallest shape a compatible endpoint must accept: model, messages
    /// and the streaming flag. This is the last resort after both the
    /// sanitized and the tool-preserving bodies were refused.
    fn minimal_body(&self, request: &ProviderRequest) -> Result<Value, ProviderError> {
        let messages = chat_messages(request, &self.entry.model, true)?;
        Ok(json!({
            "model": self.entry.model,
            "messages": compatible_messages(messages.as_array().map(Vec::as_slice).unwrap_or_default()),
            "stream": request.stream
        }))
    }
}

/// A 400 body that names an unsupported request parameter or history shape.
fn unsupported_shape(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("unsupported")
        && (text.contains("parameter") || text.contains("shape") || text.contains("conversion"))
}

/// Reasoning models report the budget as `max_completion_tokens` and take an
/// effort instead of sampling. The catalog decides for a known id; the family
/// rule is the fallback for one it does not carry.
fn reasoning_model(entry: &AiProviderEntry, model: &str) -> bool {
    super::catalog::reasoning(entry.wire(), model).unwrap_or_else(|| {
        ["gpt-5", "gpt-6", "o1", "o3", "o4"]
            .iter()
            .any(|prefix| model.starts_with(prefix))
    })
}

/// The smallest message shape a chat-completions converter accepts: one user
/// turn at a time and no null assistant content. Media stays intact; a gateway
/// that refuses audio is routed to its native Gemini endpoint instead of
/// silently losing the clip.
fn compatible_messages(messages: &[Value]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or_default();
        let mut content = message["content"].clone();
        if role == "user"
            && let Some(previous) = out.last_mut().filter(|row| row["role"] == "user")
        {
            previous["content"] = merge_user_content(&previous["content"], &content);
            continue;
        }
        let mut row = message.clone();
        if role == "assistant" && content.is_null() {
            content = json!("");
        }
        row["content"] = content;
        out.push(row);
    }
    json!(out)
}

fn merge_user_content(previous: &Value, current: &Value) -> Value {
    match (previous, current) {
        (Value::String(left), Value::String(right)) => json!(format!("{left}\n{right}")),
        (Value::Array(left), Value::Array(right)) => {
            json!(left.iter().chain(right.iter()).cloned().collect::<Vec<_>>())
        }
        (Value::Array(left), Value::String(right)) => {
            let mut parts = left.clone();
            parts.push(json!({"type": "text", "text": right}));
            json!(parts)
        }
        (Value::String(left), Value::Array(right)) => {
            let mut parts = vec![json!({"type": "text", "text": left})];
            parts.extend(right.iter().cloned());
            json!(parts)
        }
        (_, right) => right.clone(),
    }
}

fn chat_messages(
    request: &ProviderRequest,
    model: &str,
    sanitize: bool,
) -> Result<Value, ProviderError> {
    let mut messages = Vec::new();
    // A tool result is only valid immediately after the assistant message that
    // called it. A stopped or steered turn can leave a late result after a
    // user message; sending that order makes a compatible gateway reject the
    // whole request as an unsupported shape.
    let mut pending: BTreeMap<String, ()> = BTreeMap::new();
    if let Some(system) = &request.system {
        messages.push(json!({"role":"system","content":system}));
    }
    for turn in &request.turns {
        match turn.role {
            Role::User => {
                close_pending_results(&mut messages, &mut pending);
                messages.push(json!({"role":"user","content":user_content(turn, model)?}));
            }
            Role::Assistant => {
                close_pending_results(&mut messages, &mut pending);
                let text = super::block_text(&turn.blocks);
                let calls = turn
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        Block::ToolUse {
                            id,
                            name,
                            input,
                            extra,
                        } => Some(super::tool_call_to_wire(
                            id,
                            name,
                            input,
                            if sanitize { &None } else { extra },
                        )),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let mut message = json!({
                    "role": "assistant",
                    "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }
                });
                if !calls.is_empty() {
                    for call in &calls {
                        if let Some(id) = call["id"].as_str() {
                            pending.insert(id.to_owned(), ());
                        }
                    }
                    message["tool_calls"] = json!(calls);
                }
                if !sanitize
                    && let (Some(Value::Object(extra)), Some(object)) =
                        (&turn.extra, message.as_object_mut())
                {
                    for (key, value) in extra {
                        if !super::is_local_message_field(key) {
                            object.insert(key.clone(), value.clone());
                        }
                    }
                }
                messages.push(message);
            }
            Role::Tool => {
                for block in &turn.blocks {
                    if let Block::ToolResult {
                        call_id, content, ..
                    } = block
                    {
                        if pending.remove(call_id).is_none() {
                            log::warn!("ai: dropped a tool result that does not follow its call");
                            continue;
                        }
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": clipped_tool_result(content)
                        }));
                    }
                }
            }
            Role::System => {}
        }
    }
    close_pending_results(&mut messages, &mut pending);
    Ok(json!(messages))
}

/// Answers calls that never received a result before a user or assistant turn.
/// A provider rejects an unanswered call just as it rejects a late result.
fn close_pending_results(messages: &mut Vec<Value>, pending: &mut BTreeMap<String, ()>) {
    for (call_id, ()) in std::mem::take(pending) {
        messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": "Not run: the turn stopped before this call returned, so it has no result."
        }));
    }
}

/// A tool result the model sees is bounded. A search that returns forty items is
/// tens of kilobytes; sending it whole fills the window and forces a compaction
/// after one turn. The transcript keeps the full result for the user.
const MAX_TOOL_RESULT_CHARS: usize = 16_000;

fn clipped_tool_result(content: &str) -> String {
    if content.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return content.to_string();
    }
    let kept: String = content.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    format!(
        "{kept}\n[truncated: {} characters in total. Narrow the query or page with offset for the rest]",
        content.chars().count()
    )
}

/// Text stays a plain string; a turn with audio becomes a content-part array so
/// `input_audio` can ride beside the prompt.
fn user_content(turn: &Turn, model: &str) -> Result<Value, ProviderError> {
    let mut parts = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text(text) if !text.is_empty() => {
                parts.push(json!({"type": "text", "text": text}))
            }
            Block::Media { .. } => {
                let (kind, mime, data) = super::media::inline_media(block)?;
                if kind == super::media::MediaKind::Audio {
                    let format = audio_format(block)?;
                    // MiMo's documented audio input is a MIME data URI, including
                    // when its model ID is namespaced by an OpenAI-compatible gateway.
                    // https://mimo.mi.com/docs/en-US/quick-start/usage-guide/multimodal-understanding/audio-understanding
                    let audio = if model
                        .rsplit('/')
                        .next()
                        .unwrap_or(model)
                        .starts_with("mimo-")
                    {
                        json!({"data": format!("data:{mime};base64,{data}")})
                    } else {
                        json!({"data": data, "format": format})
                    };
                    parts.push(json!({"type": "input_audio", "input_audio": audio}));
                } else {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{mime};base64,{data}")}
                    }));
                }
            }
            _ => {}
        }
    }
    if parts.iter().all(|part| part["type"] == "text") {
        return Ok(json!(super::block_text(&turn.blocks)));
    }
    Ok(Value::Array(parts))
}

fn audio_format(block: &Block) -> Result<String, ProviderError> {
    let Block::Media { data, .. } = block else {
        return Err(ProviderError::config("Expected a media block"));
    };
    let name = &data.source().name;
    let format = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(format.as_str(), "wav" | "mp3") {
        Ok(format)
    } else {
        Err(ProviderError::config(format!(
            "OpenAI Chat Completions accepts wav and mp3 audio, not {name}."
        )))
    }
}

/// Provider fields on a streamed call arrive complete or as string fragments;
/// merging by index keeps both shapes without inventing a field.
fn merge_extra(target: &mut Value, incoming: &Value) {
    match (target, incoming) {
        (Value::String(existing), Value::String(part)) => existing.push_str(part),
        (Value::Object(existing), Value::Object(parts)) => {
            for (key, value) in parts {
                merge_extra(existing.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (slot, incoming) => *slot = incoming.clone(),
    }
}

#[derive(Default)]
pub(crate) struct Completion {
    text: String,
    reasoning: String,
    calls: BTreeMap<u64, Value>,
    finished: bool,
    stop: Option<String>,
    request_id: Option<String>,
    events: Vec<StreamEvent>,
}

/// Only plain text explicitly exposed by the provider is displayable.
fn exposed_reasoning(message: &Value) -> Option<&str> {
    message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| message.get("reasoning").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

impl Completion {
    /// Index magnitude is not a call-count limit: compatible gateways may
    /// preserve sparse/global indexes. An omitted index is usable only when a
    /// previously established ID identifies exactly one assembled call.
    fn tool_index(&self, call: &Value) -> Result<u64, String> {
        let id = call["id"].as_str().filter(|id| !id.is_empty());
        let by_id = id.and_then(|id| {
            self.calls
                .iter()
                .find_map(|(&index, value)| (value["id"].as_str() == Some(id)).then_some(index))
        });
        let index = match call.get("index") {
            None | Some(Value::Null) => by_id.ok_or(
                "Streaming tool fragment omitted its index without an established call ID",
            )?,
            Some(Value::String(value))
                if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                value
                    .parse::<u64>()
                    .map_err(|_| "Invalid streaming tool index: expected a nonnegative integer")?
            }
            Some(value) => value
                .as_u64()
                .ok_or("Invalid streaming tool index: expected a nonnegative integer")?,
        };
        if by_id.is_some_and(|known| known != index) {
            return Err("Streaming tool call ID changed index".into());
        }
        if let Some(existing) = self.calls.get(&index) {
            if let (Some(id), Some(known)) = (
                id,
                existing["id"].as_str().filter(|known| !known.is_empty()),
            ) && id != known
            {
                return Err("Streaming tool index changed call ID".into());
            }
        } else if self.calls.len() >= MAX_TOOL_CALLS_PER_BATCH {
            return Err(format!(
                "Streaming response exceeded the limit of {MAX_TOOL_CALLS_PER_BATCH} tool calls ({} existing indexes; incoming index {index})",
                self.calls.len()
            ));
        }
        Ok(index)
    }

    pub(crate) fn event(
        &mut self,
        data: &str,
        channel: &Option<Channel<String>>,
    ) -> Result<(), String> {
        if data == "[DONE]" {
            return Ok(());
        }
        let event: Value = serde_json::from_str(data).map_err(|_| "Invalid streaming JSON")?;
        if event.get("error").is_some() {
            return Err("Provider returned a streaming error".into());
        }
        if self.request_id.is_none()
            && let Some(id) = event["id"].as_str().filter(|id| !id.is_empty())
        {
            self.request_id = Some(id.to_owned());
            self.events.push(StreamEvent::RequestId(id.to_owned()));
        }
        if let Some(usage) = event.get("usage").filter(|usage| usage.is_object()) {
            self.events.push(usage_event(usage));
        }
        let Some(choice) = event.get("choices").and_then(|v| v.get(0)) else {
            return Ok(());
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finished = true;
            self.stop = Some(reason.to_owned());
        }
        let delta = &choice["delta"];
        if let Some(text) = exposed_reasoning(delta) {
            self.reasoning.push_str(text);
            self.events
                .push(StreamEvent::ThinkingDelta(text.to_owned()));
        }
        if let Some(text) = delta["content"].as_str() {
            self.text.push_str(text);
            self.events.push(StreamEvent::TextDelta(text.to_owned()));
            if let Some(channel) = channel {
                let _ = channel.send(text.to_owned());
            }
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = self.tool_index(call)?;
                let target = self.calls.entry(index).or_insert_with(
                    || json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                );
                if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                    target["id"] = json!(id);
                }
                for field in ["name", "arguments"] {
                    if let Some(part) = call["function"][field].as_str() {
                        let existing = target["function"][field].as_str().unwrap_or("");
                        target["function"][field] = json!(format!("{existing}{part}"));
                        if field == "name" && !part.is_empty() {
                            self.events.push(StreamEvent::ToolPreparing(
                                target["function"]["name"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                            ));
                        }
                    }
                }
                if let Some(object) = call.as_object() {
                    for (key, value) in object {
                        if matches!(key.as_str(), "index" | "id" | "type" | "function") {
                            continue;
                        }
                        merge_extra(&mut target[key.as_str()], value);
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn take_events(&mut self) -> Vec<StreamEvent> {
        std::mem::take(&mut self.events)
    }

    pub(crate) fn stop_reason(&self) -> StopReason {
        match self.stop.as_deref() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some(other) => StopReason::Other(other.to_owned()),
            None => StopReason::EndTurn,
        }
    }

    pub(crate) fn result(self) -> Result<Value, ProviderError> {
        if !self.finished {
            return Err(ProviderError::protocol(
                "Streaming response ended prematurely; nothing was applied",
            ));
        }
        let stop = self.stop.as_deref().unwrap_or_default();
        if self.text.is_empty() && self.calls.is_empty() {
            return Err(ProviderError::unusable(empty_reply(stop)));
        }
        if stop == "length" {
            return Err(ProviderError::truncated(
                "Completion truncated. Request a smaller edit.",
            ));
        }
        let mut message = json!({"role":"assistant","content":self.text});
        if !self.reasoning.is_empty() {
            message["thinking"] = json!([{"text": self.reasoning}]);
        }
        if !self.calls.is_empty() {
            message["tool_calls"] = json!(self.calls.into_values().collect::<Vec<_>>());
        }
        Ok(message)
    }
}

fn usage_event(usage: &Value) -> StreamEvent {
    StreamEvent::Usage {
        input: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output: usage["completion_tokens"].as_u64().unwrap_or(0),
        reasoning: usage["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0),
        cached: usage["prompt_tokens_details"]["cached_tokens"].as_u64(),
    }
}

/// A normal end of turn with nothing to show is a failure, not a reply: a
/// reasoning model can spend the whole output budget before writing any text.
fn empty_reply(finish_reason: &str) -> String {
    let finish_reason = if finish_reason.is_empty() {
        "none"
    } else {
        finish_reason
    };
    format!(
        "provider returned an empty reply (finish_reason={finish_reason}, no tool calls); the request may need a larger max_tokens because reasoning tokens count against it"
    )
}

/// What the provider said about the refusal, when it said anything. Bounded and
/// single-line, so an error reply cannot become the payload.
const PROVIDER_MESSAGE_LIMIT: usize = 2000;

fn provider_message(bytes: &[u8]) -> Option<String> {
    let body: Value = serde_json::from_slice(bytes).ok()?;
    let error = body.get("error")?;
    let message = match error {
        Value::String(message) => message.clone(),
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    (!message.is_empty()).then(|| message.chars().take(PROVIDER_MESSAGE_LIMIT).collect())
}

impl ChatCompletions {
    async fn post(
        &self,
        url: reqwest::Url,
        body: &Value,
    ) -> Result<reqwest::Response, ProviderError> {
        let client = super::http_client_for(url.as_str(), super::STREAM_TIMEOUT_SECS)?;
        let mut request = client
            .post(url)
            .json(body)
            .build()
            .map_err(|_| ProviderError::config("Invalid provider request"))?;
        self.auth.apply(request.headers_mut())?;
        client
            .execute(request)
            .await
            .map_err(|error| map_reqwest(error, &self.entry.name))
    }
}

async fn status_error(response: reqwest::Response, provider_name: &str) -> ProviderError {
    let status = response.status().as_u16();
    let bytes = read_limited(response).await.unwrap_or_default();
    error_from_body(status, &bytes, provider_name)
}

fn error_from_body(status: u16, bytes: &[u8], provider_name: &str) -> ProviderError {
    let mut error = map_status(status, provider_name);
    // A structured provider message is shown once; the log keeps the raw body
    // so a failing provider can be diagnosed without asking for a screenshot.
    let body: String = String::from_utf8_lossy(bytes).chars().take(600).collect();
    log::warn!("ai: {provider_name} returned {status}: {body}");
    if let Some(message) = provider_message(bytes) {
        error.message = format!("{} {message}", error.message);
    }
    error
}

/// Audio is the one shape the OpenAI chat conversion on this gateway refuses;
/// the same host exposes Gemini's native generateContent, which accepts it.
fn has_audio(request: &ProviderRequest) -> bool {
    request.turns.iter().any(|turn| {
        turn.blocks.iter().any(|block| {
            matches!(
                block,
                Block::Media {
                    kind: MediaKind::Audio,
                    ..
                }
            )
        })
    })
}

/// A Gemini-family entry that was refused audio on the OpenAI chat route can
/// use the gateway's native Gemini surface instead; the adapter only names the
/// fix, it does not switch routes behind the operator's back.
fn gemini_audio_hint(entry: &AiProviderEntry) -> Option<&'static str> {
    let family = format!("{} {} {}", entry.id, entry.name, entry.model).to_ascii_lowercase();
    (family.contains("google") || family.contains("gemini")).then_some(
        "If you are using standard chat completions for Google Gemini models, switch to a Gemini generateContent entry at /v1beta with the bare model id (`gemini-4-argon`, not `google/gemini-4-argon`); that route accepts audio input.",
    )
}

impl Provider for ChatCompletions {
    fn stream<'a>(
        &'a self,
        request: &'a ProviderRequest,
        sink: &'a mut (dyn FnMut(StreamEvent) + Send),
    ) -> BoxFuture<'a, Result<Turn, ProviderError>> {
        Box::pin(async move {
            let url = self
                .route
                .request_url(&self.entry.model)
                .map_err(ProviderError::config)?;
            let body = self.body(request)?;
            let mut response = self.post(url.clone(), &body).await?;
            if response.status().as_u16() == 400 {
                let bytes = response.bytes().await.unwrap_or_default();
                if unsupported_shape(&String::from_utf8_lossy(&bytes)) {
                    if has_audio(request)
                        && let Some(hint) = gemini_audio_hint(&self.entry)
                    {
                        log::warn!(
                            "ai: {} rejected audio on chat completions; naming the Gemini native endpoint",
                            self.entry.name
                        );
                        return Err(ProviderError::new(ProviderErrorKind::Request, hint));
                    }
                    log::warn!(
                        "ai: {} rejected the request shape; retrying without optional fields",
                        self.entry.name
                    );
                    let sanitized = self.sanitized_body(request)?;
                    let retry_response = self.post(url.clone(), &sanitized).await?;
                    if retry_response.status().is_success() {
                        response = retry_response;
                    } else if retry_response.status().as_u16() == 400 {
                        let retry_bytes = retry_response.bytes().await.unwrap_or_default();
                        if !unsupported_shape(&String::from_utf8_lossy(&retry_bytes)) {
                            return Err(error_from_body(400, &retry_bytes, &self.entry.name));
                        }
                        // A reasoning parameter is a common gateway refusal;
                        // drop it without dropping the tools the turn needs.
                        let compatible = self.compatible_body(request)?;
                        let compatible_response = self.post(url.clone(), &compatible).await?;
                        if compatible_response.status().is_success() {
                            response = compatible_response;
                        } else if compatible_response.status().as_u16() == 400 {
                            let compatible_bytes =
                                compatible_response.bytes().await.unwrap_or_default();
                            if !unsupported_shape(&String::from_utf8_lossy(&compatible_bytes)) {
                                return Err(error_from_body(
                                    400,
                                    &compatible_bytes,
                                    &self.entry.name,
                                ));
                            }
                            let minimal = self.minimal_body(request)?;
                            let minimal_response = self.post(url, &minimal).await?;
                            if minimal_response.status().is_success() {
                                response = minimal_response;
                            } else {
                                return Err(status_error(minimal_response, &self.entry.name).await);
                            }
                        } else {
                            return Err(status_error(compatible_response, &self.entry.name).await);
                        }
                    } else {
                        return Err(status_error(retry_response, &self.entry.name).await);
                    }
                } else {
                    return Err(error_from_body(400, &bytes, &self.entry.name));
                }
            }
            if !response.status().is_success() {
                return Err(status_error(response, &self.entry.name).await);
            }
            if !request.stream {
                let bytes = read_limited(response).await?;
                let result: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| ProviderError::protocol("Provider returned invalid JSON"))?;
                let choice = result
                    .get("choices")
                    .and_then(|value| value.get(0))
                    .ok_or_else(|| ProviderError::protocol("Provider returned no completion"))?;
                if let Some(id) = result["id"].as_str().filter(|id| !id.is_empty()) {
                    sink(StreamEvent::RequestId(id.to_owned()));
                }
                if let Some(usage) = result.get("usage").filter(|usage| usage.is_object()) {
                    sink(usage_event(usage));
                }
                let message = choice
                    .get("message")
                    .filter(|message| message.is_object())
                    .ok_or_else(|| ProviderError::protocol("Invalid provider message"))?;
                if let Some(text) = exposed_reasoning(message) {
                    sink(StreamEvent::ThinkingDelta(text.to_owned()));
                }
                let mut message = message.clone();
                if !message["thinking"].is_array()
                    && let Some(text) = exposed_reasoning(&message).map(str::to_owned)
                {
                    message["thinking"] = json!([{"text": text}]);
                }
                let turn = assistant_turn_from_wire(&message)?;
                let stop = choice["finish_reason"].as_str().unwrap_or_default();
                if super::block_text(&turn.blocks).is_empty()
                    && !turn
                        .blocks
                        .iter()
                        .any(|block| matches!(block, Block::ToolUse { .. }))
                {
                    return Err(ProviderError::unusable(empty_reply(stop)));
                }
                if stop == "length" {
                    return Err(ProviderError::truncated(
                        "Completion truncated. Increase output budget or request a smaller edit.",
                    ));
                }
                super::tool_use_events(&turn, sink);
                sink(StreamEvent::Done {
                    reason: StopReason::EndTurn,
                });
                return Ok(turn);
            }
            let mut completion = Completion::default();
            let mut on_frame = |frame: sse::SseFrame| -> Result<(), ProviderError> {
                completion
                    .event(&frame.data, &None)
                    .map_err(ProviderError::protocol)?;
                for event in completion.take_events() {
                    sink(event);
                }
                Ok(())
            };
            sse::drive(&mut response, &mut on_frame).await?;
            let stop = completion.stop_reason();
            let message = completion.result()?;
            let turn = assistant_turn_from_wire(&message)?;
            super::tool_use_events(&turn, sink);
            sink(StreamEvent::Done { reason: stop });
            Ok(turn)
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_message_metadata_never_reaches_the_provider_body() {
        let request = super::super::ProviderRequest {
            system: None,
            turns: super::super::turns_from_wire(&serde_json::json!([
                {"role":"user","content":"Read the project", "message_meta":{"version":1,"kind":"user","standing_intent":"LOCAL_PROTOCOL_SENTINEL"}},
                {"role":"assistant","content":null,"message_meta":{"version":1,"kind":"assistant"},
                 "thinking":[{"text":"Exposed thought", "signature":"fixture-thought-signature"}],
                 "tool_calls":[{"id":"local-call","type":"function","function":{"name":"project_get","arguments":"{}"}}]},
                {"role":"tool","tool_call_id":"local-call","content":"{\"ok\":true}","message_meta":{"version":1,"kind":"tool"}}
            ])).unwrap(),
            tools: Vec::new(), max_tokens: 4096, temperature: 0.3,
            thinking_level: "medium".into(), stream: true,
        };
        assert!(
            request.turns[1]
                .blocks
                .iter()
                .any(|block| matches!(block, super::super::Block::Thinking { .. }))
        );
        let body = chat_messages(&request, "fixture-model", false).unwrap();
        let captured = serde_json::to_string(&body).unwrap();
        assert!(!captured.contains("message_meta"), "{captured}");
        assert!(!captured.contains("standing_intent"), "{captured}");
        assert!(!captured.contains("LOCAL_PROTOCOL_SENTINEL"), "{captured}");
        assert!(
            captured.contains("project_get"),
            "the tool call is preserved"
        );
        assert!(
            captured.contains("local-call"),
            "the call/result identity is preserved"
        );
        assert!(captured.contains("Read the project"));
    }

    #[test]
    fn local_fields_are_filtered_even_from_an_opaque_assistant_extra_object() {
        for (text, calls) in [(None, true), (Some("Read the project"), false)] {
            let mut wire = serde_json::json!({"role":"assistant", "content":text,
                "thinking":[{"text":"Exposed reasoning","signature":"signed"}]});
            if calls {
                wire["tool_calls"] = serde_json::json!([{"id":"local-call","type":"function","function":{"name":"project_get","arguments":"{}"},"extra_content":{"google":{"thought_signature":"call-signature"}}}]);
            }
            let mut turns = super::super::turns_from_wire(&serde_json::json!([wire])).unwrap();
            turns[0].extra = Some(
                serde_json::json!({"message_meta":{"version":9,"opaque":"LOCAL_PROTOCOL_SENTINEL"},"context":"summary","dsl_sha":"LOCAL_SNAPSHOT_SENTINEL","source_render":"/private/host-render.wav","reasoning_content":"Provider reasoning","signature":"provider-signature"}),
            );
            let request = super::super::ProviderRequest {
                system: None,
                turns,
                tools: Vec::new(),
                max_tokens: 1024,
                temperature: 0.3,
                thinking_level: "medium".into(),
                stream: true,
            };
            let body = chat_messages(&request, "fixture-model", false).unwrap();
            let sent = &body[0];
            for key in ["message_meta", "context", "dsl_sha", "source_render"] {
                assert!(
                    sent.get(key).is_none(),
                    "local field {key} reached provider: {sent}"
                );
            }
            assert_eq!(sent["reasoning_content"], "Provider reasoning");
            assert_eq!(sent["signature"], "provider-signature");
            if calls {
                assert_eq!(sent["content"], serde_json::Value::Null);
                assert_eq!(sent["tool_calls"][0]["id"], "local-call");
                assert_eq!(
                    sent["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
                    "call-signature"
                );
            } else {
                assert_eq!(sent["content"], "Read the project");
            }
        }
    }

    use super::*;

    #[test]
    fn a_large_tool_result_is_clipped_before_it_reaches_the_model() {
        let small = "{\"count\":1}";
        assert_eq!(clipped_tool_result(small), small);

        let huge = "x".repeat(MAX_TOOL_RESULT_CHARS + 500);
        let clipped = clipped_tool_result(&huge);
        assert!(clipped.starts_with(&"x".repeat(MAX_TOOL_RESULT_CHARS)));
        assert!(clipped.contains("truncated:"));
        assert!(clipped.chars().count() < huge.chars().count());
        // The marker states the true size, not the kept size.
        assert!(clipped.contains(&format!("{} characters", MAX_TOOL_RESULT_CHARS + 500)));
    }

    #[test]
    fn a_gemini_audio_refusal_names_the_v1beta_endpoint() {
        let entry = provider_entry("https://gateway.example.test/v1".into(), true);
        let hint = gemini_audio_hint(&entry).expect("a google/gemini model gets the hint");
        assert!(hint.contains("/v1beta"), "{hint}");
        assert!(hint.contains("bare model id"), "{hint}");
        let mut other = entry;
        other.model = "deepseek-flash".into();
        assert!(gemini_audio_hint(&other).is_none());
    }

    #[test]
    fn a_compatible_body_merges_user_turns_and_keeps_audio() {
        let messages = json!([
            {"role":"user","content":[
                {"type":"text","text":"listen"},
                {"type":"input_audio","input_audio":{"data":"AA","format":"wav"}}
            ]},
            {"role":"user","content":"continue"},
            {"role":"assistant","content":null,"tool_calls":[
                {"id":"call-1","type":"function","function":{"name":"tracks_list","arguments":"{}"}}
            ]}
        ]);
        let compatible = compatible_messages(messages.as_array().unwrap());
        let rows = compatible.as_array().unwrap();
        assert_eq!(rows.len(), 2, "{compatible}");
        assert_eq!(rows[0]["role"], "user");
        let parts = rows[0]["content"].as_array().unwrap();
        assert!(
            parts
                .iter()
                .any(|part| part["type"] == "text" && part["text"] == "listen")
        );
        assert!(
            parts
                .iter()
                .any(|part| part["type"] == "text" && part["text"] == "continue")
        );
        assert!(parts.iter().any(|part| part["type"] == "input_audio"));
        assert_eq!(rows[1]["content"], "");
        assert_eq!(rows[1]["tool_calls"][0]["id"], "call-1");
    }

    #[test]
    fn a_late_tool_result_does_not_break_the_request_shape() {
        fn turn(role: Role, blocks: Vec<Block>) -> Turn {
            Turn {
                role,
                blocks,
                continuity: None,
                extra: None,
            }
        }
        fn result() -> Turn {
            turn(
                Role::Tool,
                vec![Block::ToolResult {
                    call_id: "call-1".into(),
                    name: "project_dsl".into(),
                    content: "{\"ok\":true}".into(),
                    is_error: false,
                }],
            )
        }
        let request = ProviderRequest {
            system: None,
            turns: vec![
                turn(Role::User, vec![Block::Text("go".into())]),
                turn(
                    Role::Assistant,
                    vec![Block::ToolUse {
                        id: "call-1".into(),
                        name: "project_dsl".into(),
                        input: json!({}),
                        extra: None,
                    }],
                ),
                result(),
                turn(Role::User, vec![Block::Text("rollback".into())]),
                // The same call arrives again after the user message: the
                // gateway refuses this order, so it is dropped.
                result(),
                turn(Role::User, vec![Block::Text("continue".into())]),
            ],
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: false,
        };
        let body = chat_messages(&request, "gemini-4-argon", true).unwrap();
        let roles = body
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roles, ["user", "assistant", "tool", "user", "user"]);
    }

    #[test]
    fn a_user_message_closes_an_unanswered_call() {
        fn turn(role: Role, blocks: Vec<Block>) -> Turn {
            Turn {
                role,
                blocks,
                continuity: None,
                extra: None,
            }
        }
        let request = ProviderRequest {
            system: None,
            turns: vec![
                turn(Role::User, vec![Block::Text("go".into())]),
                turn(
                    Role::Assistant,
                    vec![Block::ToolUse {
                        id: "call-1".into(),
                        name: "project_dsl".into(),
                        input: json!({}),
                        extra: None,
                    }],
                ),
                turn(Role::User, vec![Block::Text("actually stop".into())]),
                turn(
                    Role::Tool,
                    vec![Block::ToolResult {
                        call_id: "call-1".into(),
                        name: "project_dsl".into(),
                        content: "{\"ok\":true}".into(),
                        is_error: false,
                    }],
                ),
            ],
            tools: Vec::new(),
            max_tokens: 1024,
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: false,
        };
        let body = chat_messages(&request, "gemini-4-argon", true).unwrap();
        let messages = body.as_array().unwrap();
        let roles = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roles, ["user", "assistant", "tool", "user"]);
        assert!(
            messages[2]["content"]
                .as_str()
                .unwrap()
                .starts_with("Not run:")
        );
    }

    use crate::ai::auth::Credential;
    use crate::ai::test_support::{canned, canned_by_request};
    use crate::ai::timing::TurnStatsHandle;
    use crate::ai::{self, ProviderErrorKind, provider_for, request_from_wire};
    use crate::commands::{AiCapabilities, ProviderKind, Wire};

    #[test]
    fn tool_preparation_precedes_argument_completion() {
        let mut completion = Completion::default();
        completion.event(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-a","function":{"name":"sample_","arguments":""}}]}}]}"#, &None).unwrap();
        assert!(
            completion
                .take_events()
                .contains(&StreamEvent::ToolPreparing("sample_".into()))
        );
        completion.event(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"search","arguments":"{\"query\":"}}]}}]}"#, &None).unwrap();
        let events = completion.take_events();
        assert!(events.contains(&StreamEvent::ToolPreparing("sample_search".into())));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolCall { .. }))
        );
    }

    #[test]
    fn exposed_reasoning_is_a_separate_event_not_answer_text() {
        let mut completion = Completion::default();
        completion
            .event(
                r#"{"choices":[{"delta":{"reasoning_content":"Choose a quiet piano."}}]}"#,
                &None,
            )
            .unwrap();
        completion
            .event(
                r#"{"choices":[{"delta":{"content":"Done."},"finish_reason":"stop"}]}"#,
                &None,
            )
            .unwrap();
        let events = completion.take_events();
        assert!(events.contains(&StreamEvent::ThinkingDelta("Choose a quiet piano.".into())));
        assert!(events.contains(&StreamEvent::TextDelta("Done.".into())));
        let message = completion.result().unwrap();
        assert_eq!(message["content"], "Done.");
        assert_eq!(message["thinking"][0]["text"], "Choose a quiet piano.");
    }

    #[test]
    fn exposed_reasoning_never_decodes_opaque_provider_state() {
        assert_eq!(
            exposed_reasoning(&json!({"reasoning":"A short public note"})),
            Some("A short public note")
        );
        assert_eq!(
            exposed_reasoning(
                &json!({"reasoning":{"encrypted":"secret"},"provider_state":"opaque"})
            ),
            None
        );
        assert_eq!(exposed_reasoning(&json!({"reasoning_content":""})), None);
    }

    const ARGON_REJECTION: &str = r#"{"error":{"message":"Gemini 4+ requires the original thought signature on the first tool call (set_track_gain) of each step in the current turn; preserve tool_calls[].extra_content.google.thought_signature or tool_calls[].signature in assistant history.","type":"invalid_request_error"}}"#;
    const SET_GAIN_CALL: &str = r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"set_track_gain","arguments":"{\"track\":\"kick\",\"gain_db\":-6}"},"extra_content":{"google":{"thought_signature":"sig-abc"}},"signature":"sig-abc"}],"provider_specific_fields":{"refusal":null},"extra_content":{"google":{"thought_signature":"sig-abc"}}},"finish_reason":"tool_calls"}]}"#;
    /// The same call with nothing beside it: a provider that needs no signature.
    const PLAIN_CALL: &str = r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"set_track_gain","arguments":"{\"track\":\"kick\",\"gain_db\":-6}"}}]},"finish_reason":"tool_calls"}]}"#;
    const SET_GAIN_DONE: &str = r#"{"id":"chatcmpl-2","choices":[{"message":{"role":"assistant","content":"Kick is at -6 dB."},"finish_reason":"stop"}]}"#;
    const STREAMED_CALL: &str = "data: {\"id\":\"chatcmpl-s1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"set_track_gain\",\"arguments\":\"{\\\"track\\\":\\\"kick\\\",\\\"gain_db\\\":-6}\"},\"extra_content\":{\"google\":{\"thought_signature\":\"sig-stream\"}},\"signature\":\"sig-stream\"}]}}]}\n\n\
        data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
        data: [DONE]\n\n";
    const STREAMED_DONE: &str = "data: {\"id\":\"chatcmpl-s2\",\"choices\":[{\"delta\":{\"content\":\"Kick is at -6 dB.\"}}]}\n\n\
        data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
        data: [DONE]\n\n";

    fn provider_entry(endpoint: String, streaming: bool) -> AiProviderEntry {
        AiProviderEntry {
            id: "gateway-fixture".into(),
            name: "Gateway fixture".into(),
            kind: ProviderKind::OpenAiChatCompletions,
            wire: Some(Wire::OpenAiChatCompletions),
            auth: Default::default(),
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "google/gemini-4-argon".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                streaming,
                native_tool_calling: true,
                ..Default::default()
            },
        }
    }

    /// True when some assistant history message replays the call with both
    /// carriers of the signature the first response sent.
    fn replayed_signature(request: &str, signature: &str) -> bool {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let Ok(body) = serde_json::from_str::<Value>(body) else {
            return false;
        };
        body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "assistant")
            .filter_map(|message| message["tool_calls"].as_array())
            .flatten()
            .any(|call| {
                call["extra_content"]["google"]["thought_signature"].as_str() == Some(signature)
                    && call["signature"].as_str() == Some(signature)
            })
    }

    async fn run_turn(entry: &AiProviderEntry, messages: Value) -> Result<Value, ProviderError> {
        run_turn_with(
            entry,
            messages,
            Credential::ApiKey(Some("fixture-secret".into())),
            false,
        )
        .await
    }

    async fn run_turn_with(
        entry: &AiProviderEntry,
        messages: Value,
        credential: Credential,
        use_tools: bool,
    ) -> Result<Value, ProviderError> {
        let (request, _) = request_from_wire(entry, messages, use_tools, None)?;
        let provider = provider_for(entry, credential)?;
        let mut emitted = false;
        let stats = TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
        ai::run(provider.as_ref(), &request, &None, &mut emitted, &stats).await
    }

    /// The assistant message a request replayed at `index`, decoded.
    fn replayed_message(request: &str, index: usize) -> Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        body["messages"][index].clone()
    }

    fn keys(value: &Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort_unstable();
        keys
    }

    #[tokio::test]
    async fn the_providers_message_fields_are_replayed_with_its_tool_calls() {
        let mut turn = 0;
        let (base, server) = canned_by_request(2, move |_| {
            turn += 1;
            if turn == 1 {
                (200, SET_GAIN_CALL.to_owned())
            } else {
                (200, SET_GAIN_DONE.to_owned())
            }
        });
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let first = run_turn(
            &entry,
            json!([{"role":"user","content":"pull the kick to -6 dB"}]),
        )
        .await
        .unwrap();
        // The stored assistant message is the provider's message, not a rebuild.
        assert_eq!(first["provider_specific_fields"]["refusal"], Value::Null);
        assert_eq!(
            first["extra_content"]["google"]["thought_signature"],
            "sig-abc"
        );
        let messages = json!([
            {"role":"user","content":"pull the kick to -6 dB"},
            first,
            {"role":"tool","tool_call_id":"call_1","content":"gain set"}
        ]);
        run_turn(&entry, messages).await.unwrap();
        let requests = server.join().unwrap();
        let replayed = replayed_message(&requests[1], 1);
        assert_eq!(replayed["role"], "assistant");
        assert_eq!(replayed["provider_specific_fields"]["refusal"], Value::Null);
        assert_eq!(
            replayed["extra_content"]["google"]["thought_signature"],
            "sig-abc"
        );
        assert_eq!(
            replayed["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-abc"
        );
    }

    #[tokio::test]
    async fn a_provider_that_sends_no_extra_fields_gets_none_back() {
        let mut turn = 0;
        let (base, server) = canned_by_request(2, move |_| {
            turn += 1;
            if turn == 1 {
                (200, PLAIN_CALL.to_owned())
            } else {
                (200, SET_GAIN_DONE.to_owned())
            }
        });
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let first = run_turn(
            &entry,
            json!([{"role":"user","content":"pull the kick to -6 dB"}]),
        )
        .await
        .unwrap();
        assert_eq!(keys(&first), ["content", "role", "tool_calls"]);
        let messages = json!([
            {"role":"user","content":"pull the kick to -6 dB"},
            first,
            {"role":"tool","tool_call_id":"call_1","content":"gain set"}
        ]);
        run_turn(&entry, messages).await.unwrap();
        let requests = server.join().unwrap();
        let replayed = replayed_message(&requests[1], 1);
        assert_eq!(keys(&replayed), ["content", "role", "tool_calls"]);
        assert_eq!(keys(&replayed["tool_calls"][0]), ["function", "id", "type"]);
    }

    #[test]
    fn audio_encoding_mimo_uses_documented_data_uri_without_changing_audio_bytes() {
        use base64::Engine as _;
        for model in ["mimo-v2.6-pro", "xiaomi/mimo-v2.6-pro", "mimo-v2.6-flash"] {
            let mut entry =
                provider_entry("https://gw.acme.test/v1/chat/completions".into(), false);
            entry.model = model.into();
            entry.capabilities.audio_in = true;
            let provider = ChatCompletions::for_test(entry.clone(), AuthPlan::default());
            let raw = b"RIFF fixture audio bytes";
            let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
            let (request, _) = crate::ai::request_from_wire(
                &entry,
                json!([{
                    "role":"user", "content":"Listen", "input_audio":{"format":"wav","data":encoded}
                }]),
                false,
                None,
            )
            .unwrap();
            let body = provider.body(&request).unwrap();
            let audio = &body["messages"][0]["content"][1]["input_audio"];
            let data = audio["data"].as_str().unwrap();
            let payload = data
                .strip_prefix("data:audio/wav;base64,")
                .expect("MiMo requires a MIME data URI");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .unwrap(),
                raw
            );
            assert!(audio.get("format").is_none());
        }
    }

    #[test]
    fn audio_encoding_other_models_keep_openai_base64_and_format() {
        let mut entry = provider_entry("https://gw.acme.test/v1/chat/completions".into(), false);
        entry.model = "gpt-4o-audio-preview".into();
        entry.capabilities.audio_in = true;
        let provider = ChatCompletions::for_test(entry.clone(), AuthPlan::default());
        let (request, _) = crate::ai::request_from_wire(
            &entry,
            json!([{
                "role":"user", "content":"Listen", "input_audio":{"format":"wav","data":"UklGRg=="}
            }]),
            false,
            None,
        )
        .unwrap();
        let body = provider.body(&request).unwrap();
        assert_eq!(
            body["messages"][0]["content"][1]["input_audio"],
            json!({"data":"UklGRg==","format":"wav"})
        );
    }

    /// A reasoning model takes the level the user saved, not a literal: the
    /// selector cannot be decorative.
    #[test]
    fn the_saved_thinking_level_reaches_a_reasoning_request() {
        use crate::ai::auth::AuthPlan;
        let mut entry = provider_entry("https://gw.acme.test/v1/chat/completions".into(), false);
        entry.model = "gpt-5.5".into();
        let provider = ChatCompletions::for_test(entry.clone(), AuthPlan::default());
        for (level, expected) in [("low", "low"), ("medium", "medium"), ("high", "high")] {
            let (mut request, _) = crate::ai::request_from_wire(
                &entry,
                json!([{"role":"user","content":"hi"}]),
                true,
                None,
            )
            .unwrap();
            request.thinking_level = level.into();
            let body = provider.body(&request).unwrap();
            assert_eq!(body["reasoning_effort"], expected, "{level}: {body}");
            assert!(body.get("temperature").is_none(), "{body}");
        }
    }

    /// The conversation's toolset selection narrows what is offered; sending the
    /// whole panel surface makes the switch do nothing on this wire.
    #[test]
    fn a_narrowed_toolset_is_the_only_thing_offered() {
        use crate::ai::auth::AuthPlan;
        let entry = provider_entry("https://gw.acme.test/v1/chat/completions".into(), false);
        let selection = vec!["dsl-edit".to_string()];
        let (request, _) = crate::ai::request_from_wire(
            &entry,
            json!([{"role":"user","content":"hi"}]),
            true,
            Some(&selection),
        )
        .unwrap();
        assert!(!request.tools.is_empty());
        let provider = ChatCompletions::for_test(entry.clone(), AuthPlan::default());
        let body = provider.body(&request).unwrap();
        let sent = body["tools"].as_array().expect("tools were sent");
        let names: Vec<&str> = sent
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        let wanted: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, wanted, "the request offered a different set");
        assert!(
            !names.contains(&"tracks_mix"),
            "an unselected tool was offered"
        );
        assert!(
            sent.len() < crate::ai::legacy_tools().as_array().unwrap().len(),
            "the whole panel surface was offered"
        );
        // Same encoding as before: the legacy `{type, function}` object.
        for tool in sent {
            assert_eq!(tool["type"], "function");
            assert!(tool["function"]["description"].is_string());
            assert!(tool["function"]["parameters"].is_object());
        }

        // With no tools selected nothing is advertised, so a model without
        // native tool calling keeps answering in prose.
        let (without, _) = crate::ai::request_from_wire(
            &entry,
            json!([{"role":"user","content":"hi"}]),
            false,
            None,
        )
        .unwrap();
        let body = provider.body(&without).unwrap();
        assert!(body.get("tools").is_none(), "tools were sent unasked");
        assert!(body.get("tool_choice").is_none());
    }

    /// The provider's own explanation is what a caller can act on; the mapped
    /// status alone ("rejected the request") hides it.
    #[tokio::test]
    async fn a_rejected_request_reports_the_providers_own_message() {
        let (base, server) = canned(400, ARGON_REJECTION);
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let error = run_turn(&entry, json!([{"role":"user","content":"hi"}]))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.kind, ProviderErrorKind::Request);
        assert!(error.message.contains("HTTP 400"), "{}", error.message);
        assert!(
            error.message.contains("thought signature"),
            "the provider's text was dropped: {}",
            error.message
        );
    }

    const SHAPE_REJECTION: &str = r#"{"error":{"message":"unsupported chat conversion request shape or parameter","type":"invalid_request_error"}}"#;

    #[tokio::test]
    async fn a_gemini_audio_refusal_returns_the_v1beta_hint_without_a_retry() {
        use crate::ai::media::{MediaData, MediaRef};
        let (base, server) = canned(400, SHAPE_REJECTION);
        let mut entry = provider_entry(format!("{base}v1/chat/completions"), false);
        entry.capabilities.audio_in = true;
        let source = MediaRef {
            local_reference: false,
            name: "probe.wav".into(),
            path: "/tmp/probe.wav".into(),
            mime: "audio/wav".into(),
            bytes: 1,
            duration_secs: Some(0.1),
            sha256: "00".repeat(32),
        };
        let request = ProviderRequest {
            system: None,
            turns: vec![Turn {
                role: Role::User,
                blocks: vec![
                    Block::Text("listen".into()),
                    Block::Media {
                        kind: MediaKind::Audio,
                        mime: "audio/wav".into(),
                        data: MediaData::Inline {
                            source,
                            base64: "AA==".into(),
                        },
                    },
                ],
                continuity: None,
                extra: None,
            }],
            tools: Vec::new(),
            max_tokens: 64,
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: false,
        };
        let provider = ChatCompletions::for_test(entry, AuthPlan::bearer("fixture-secret"));
        let mut sink = |_event: StreamEvent| {};
        let error = provider
            .stream(&request, &mut sink)
            .await
            .expect_err("audio on chat completions should be refused");
        // Exactly one request reached the gateway: the hint replaces the
        // sanitized/compatible retry chain for an audio refusal.
        server.join().unwrap();
        assert_eq!(error.kind, ProviderErrorKind::Request);
        assert!(error.message.contains("/v1beta"), "{}", error.message);
        assert!(error.message.contains("bare model id"), "{}", error.message);
    }

    #[tokio::test]
    async fn an_unsupported_history_shape_retries_without_private_fields() {
        let mut calls = 0;
        let (base, server) = canned_by_request(2, move |_| {
            calls += 1;
            if calls == 1 {
                (400, SHAPE_REJECTION.to_owned())
            } else {
                (200, SET_GAIN_DONE.to_owned())
            }
        });
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let messages = json!([
            {"role":"user","content":"pull the kick to -6 dB"},
            {
                "role":"assistant",
                "content":null,
                "reasoning_details":[{"type":"reasoning.encrypted","data":"secret"}],
                "tool_calls":[{
                    "id":"call_1",
                    "type":"function",
                    "function":{"name":"set_track_gain","arguments":"{\"track\":\"kick\",\"gain_db\":-6}"},
                    "signature":"sig-abc",
                    "extra_content":{"google":{"thought_signature":"sig-abc"}}
                }]
            },
            {"role":"tool","tool_call_id":"call_1","content":"gain set"}
        ]);
        let result = run_turn(&entry, messages).await.unwrap();
        assert_eq!(result["content"], "Kick is at -6 dB.");
        let requests = server.join().unwrap();
        let retry: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let call = &retry["messages"][1]["tool_calls"][0];
        assert!(call.get("signature").is_none(), "{retry}");
        assert!(call.get("extra_content").is_none(), "{retry}");
        assert!(
            retry["messages"][1].get("reasoning_details").is_none(),
            "{retry}"
        );
    }

    #[tokio::test]
    async fn a_reasoning_parameter_refusal_keeps_tools_and_max_tokens() {
        let mut calls = 0;
        let (base, server) = canned_by_request(3, move |_| {
            calls += 1;
            if calls < 3 {
                (400, SHAPE_REJECTION.to_owned())
            } else {
                (200, SET_GAIN_DONE.to_owned())
            }
        });
        let mut entry = provider_entry(format!("{base}v1/chat/completions"), false);
        entry.model = "gpt-5".into();
        let result = run_turn_with(
            &entry,
            json!([{"role":"user","content":"pull the kick to -6 dB"}]),
            Credential::ApiKey(Some("fixture-secret".into())),
            true,
        )
        .await
        .unwrap();
        assert_eq!(result["content"], "Kick is at -6 dB.");
        let requests = server.join().unwrap();
        let body: Value =
            serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(
            body["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        );
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("max_completion_tokens").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
    }

    #[tokio::test]
    async fn a_tool_call_signature_survives_into_the_next_turn() {
        let mut turn = 0;
        let (base, server) = canned_by_request(2, move |request| {
            turn += 1;
            if turn == 1 {
                return (200, SET_GAIN_CALL.to_owned());
            }
            if replayed_signature(request, "sig-abc") {
                (200, SET_GAIN_DONE.to_owned())
            } else {
                (400, ARGON_REJECTION.to_owned())
            }
        });
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let first = run_turn(
            &entry,
            json!([{"role":"user","content":"pull the kick to -6 dB"}]),
        )
        .await
        .unwrap();
        assert_eq!(first["tool_calls"][0]["id"], "call_1");
        let messages = json!([
            {"role":"user","content":"pull the kick to -6 dB"},
            first,
            {"role":"tool","tool_call_id":"call_1","content":"gain set"}
        ]);
        let second = run_turn(&entry, messages).await;
        assert!(
            second.is_ok(),
            "the provider rejected the second turn: {}",
            second
                .as_ref()
                .err()
                .map(|error| error.message.clone())
                .unwrap_or_default()
        );
        assert_eq!(second.unwrap()["content"], "Kick is at -6 dB.");
        // The emitted assistant message carries the provider's fields too, so the
        // panel's own history can replay them.
        assert_eq!(
            first["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-abc"
        );
        assert_eq!(first["tool_calls"][0]["signature"], "sig-abc");
        let requests = server.join().unwrap();
        let second_body: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            second_body["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-abc"
        );
        assert_eq!(
            second_body["messages"][1]["tool_calls"][0]["signature"],
            "sig-abc"
        );
        assert_eq!(
            second_body["messages"][1]["tool_calls"][0]["type"],
            "function"
        );
    }

    #[tokio::test]
    async fn a_streamed_tool_call_signature_survives_into_the_next_turn() {
        let mut turn = 0;
        let (base, server) = canned_by_request(2, move |request| {
            turn += 1;
            if turn == 1 {
                return (200, STREAMED_CALL.to_owned());
            }
            if replayed_signature(request, "sig-stream") {
                (200, STREAMED_DONE.to_owned())
            } else {
                (400, ARGON_REJECTION.to_owned())
            }
        });
        let entry = provider_entry(format!("{base}v1/chat/completions"), true);
        let first = run_turn(
            &entry,
            json!([{"role":"user","content":"pull the kick to -6 dB"}]),
        )
        .await
        .unwrap();
        assert_eq!(
            first["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-stream"
        );
        let messages = json!([
            {"role":"user","content":"pull the kick to -6 dB"},
            first,
            {"role":"tool","tool_call_id":"call_1","content":"gain set"}
        ]);
        let second = run_turn(&entry, messages).await;
        assert!(
            second.is_ok(),
            "the provider rejected the second turn: {}",
            second
                .as_ref()
                .err()
                .map(|error| error.message.clone())
                .unwrap_or_default()
        );
        server.join().unwrap();
    }

    /// A reasoning model can spend the whole output budget before writing
    /// anything; `length` with nothing to show is an empty reply too, and it
    /// names the budget that ran out.
    #[tokio::test]
    async fn an_empty_length_reply_names_the_budget() {
        const EMPTY: &str = r#"{"id":"chatcmpl-empty","choices":[{"message":{"role":"assistant","content":""},"finish_reason":"length"}]}"#;
        let (base, server) = canned(200, EMPTY);
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let error = run_turn(&entry, json!([{"role":"user","content":"hi"}]))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(
            error
                .message
                .contains("empty reply (finish_reason=length, no tool calls)"),
            "{}",
            error.message
        );
        assert!(error.message.contains("max_tokens"), "{}", error.message);
        assert!(error.retryable, "an empty length reply offers Retry");

        let mut streamed = Completion::default();
        streamed
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
                &None,
            )
            .unwrap();
        let error = streamed.result().unwrap_err();
        assert!(
            error
                .message
                .contains("empty reply (finish_reason=length, no tool calls)"),
            "{}",
            error.message
        );
        assert!(error.retryable, "{}", error.message);
    }

    /// The truncation contract for a reply that did produce text is unchanged,
    /// so the empty check cannot swallow it.
    #[tokio::test]
    async fn a_truncated_reply_that_produced_text_stays_a_truncation() {
        const PARTIAL: &str = r#"{"id":"chatcmpl-partial","choices":[{"message":{"role":"assistant","content":"Kick is"},"finish_reason":"length"}]}"#;
        let (base, server) = canned(200, PARTIAL);
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let error = run_turn(&entry, json!([{"role":"user","content":"hi"}]))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(
            error.message.starts_with("Completion truncated"),
            "{}",
            error.message
        );
        assert!(error.retryable, "a truncated reply offers Retry");

        let mut streamed = Completion::default();
        streamed
            .event(r#"{"choices":[{"delta":{"content":"Kick is"}}]}"#, &None)
            .unwrap();
        streamed
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
                &None,
            )
            .unwrap();
        let error = streamed.result().unwrap_err();
        assert!(error.message.starts_with("Completion truncated"));
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn an_empty_stop_reply_is_an_error_but_a_tool_turn_is_not() {
        const EMPTY: &str = r#"{"id":"chatcmpl-empty","choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#;
        let (base, server) = canned(200, EMPTY);
        let entry = provider_entry(format!("{base}v1/chat/completions"), false);
        let error = run_turn(&entry, json!([{"role":"user","content":"hi"}]))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(
            error
                .message
                .contains("provider returned an empty reply (finish_reason=stop, no tool calls)"),
            "{}",
            error.message
        );
        assert!(error.message.contains("max_tokens"), "{}", error.message);

        let mut streamed = Completion::default();
        streamed
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                &None,
            )
            .unwrap();
        let error = streamed.result().unwrap_err();
        assert!(
            error
                .message
                .contains("provider returned an empty reply (finish_reason=stop, no tool calls)"),
            "{}",
            error.message
        );

        // A tool turn has nothing to show either, and must stay a success.
        let mut calls = Completion::default();
        calls
            .event(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"tracks_mix","arguments":"{}"}}]}}]}"#,
                &None,
            )
            .unwrap();
        calls
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                &None,
            )
            .unwrap();
        assert!(calls.result().is_ok());
    }
    fn tool_chunk(call: Value) -> String {
        json!({"choices":[{"delta":{"tool_calls":[call]}}]}).to_string()
    }

    #[test]
    fn streaming_tool_index_accepts_sparse_indexes_and_keeps_arguments_separate() {
        let mut completion = Completion::default();
        for (index, id, args) in [(42, "later", "{\"b\":"), (8, "first", "{\"a\":")] {
            completion
                .event(
                    &tool_chunk(
                        json!({"index":index,"id":id,"function":{"name":"read", "arguments":args}}),
                    ),
                    &None,
                )
                .unwrap();
        }
        completion
            .event(
                &tool_chunk(json!({"index":"8","id":"","function":{"arguments":"1}"}})),
                &None,
            )
            .unwrap();
        completion
            .event(
                &tool_chunk(
                    json!({"id":"later","function":{"arguments":"2}"},"signature":"opaque"}),
                ),
                &None,
            )
            .unwrap();
        completion
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                &None,
            )
            .unwrap();
        let message = completion.result().unwrap();
        assert_eq!(message["tool_calls"][0]["id"], "first");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            r#"{"a":1}"#
        );
        assert_eq!(
            message["tool_calls"][1]["function"]["arguments"],
            r#"{"b":2}"#
        );
        assert_eq!(message["tool_calls"][1]["signature"], "opaque");
    }

    #[test]
    fn streaming_tool_index_limits_distinct_calls_not_index_magnitude() {
        let mut completion = Completion::default();
        for index in 100..100 + MAX_TOOL_CALLS_PER_BATCH as u64 {
            completion.event(&tool_chunk(json!({"index":index,"id":format!("call{index}"),"function":{"name":"read","arguments":"{}"}})), &None).unwrap();
        }
        // Existing fragments still fit when all slots are occupied.
        completion
            .event(&tool_chunk(json!({"index":107,"signature":"tail"})), &None)
            .unwrap();
        assert!(
            completion
                .event(
                    &tool_chunk(json!({"index":100 + MAX_TOOL_CALLS_PER_BATCH,"id":"overflow"})),
                    &None
                )
                .unwrap_err()
                .contains(&format!("limit of {MAX_TOOL_CALLS_PER_BATCH}"))
        );
        assert_eq!(completion.calls.len(), MAX_TOOL_CALLS_PER_BATCH);
    }

    #[test]
    fn streaming_tool_index_refuses_ambiguous_missing_indexes() {
        let mut completion = Completion::default();
        completion
            .event(&tool_chunk(json!({"index":0,"id":"known"})), &None)
            .unwrap();
        for call in [
            json!({"function":{"arguments":"{}"}}),
            json!({"id":"unknown"}),
            json!({"index":null}),
        ] {
            assert!(
                completion
                    .event(&tool_chunk(call), &None)
                    .unwrap_err()
                    .contains("established call ID")
            );
        }
        completion
            .event(
                &tool_chunk(json!({"index":null,"id":"known","signature":"tail"})),
                &None,
            )
            .unwrap();
    }

    #[test]
    fn streaming_tool_index_refuses_malformed_numbers_and_identity_changes() {
        let mut completion = Completion::default();
        completion
            .event(&tool_chunk(json!({"index":0,"id":"known"})), &None)
            .unwrap();
        for index in [
            json!(-1),
            json!(1.5),
            json!(true),
            json!("-1"),
            json!("1.0"),
            json!(" 1"),
            json!("18446744073709551616"),
        ] {
            assert!(
                completion
                    .event(&tool_chunk(json!({"index":index,"id":"known"})), &None)
                    .unwrap_err()
                    .contains("nonnegative integer")
            );
        }
        assert!(
            completion
                .event(&tool_chunk(json!({"index":1,"id":"known"})), &None)
                .unwrap_err()
                .contains("changed index")
        );
        assert!(
            completion
                .event(&tool_chunk(json!({"index":0,"id":"different"})), &None)
                .unwrap_err()
                .contains("changed call ID")
        );
        assert_eq!(completion.calls.len(), 1);
    }
}
