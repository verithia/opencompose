//! Anthropic Messages adapter: top-level system, required max_tokens,
//! input_schema tools, tool_use/tool_result blocks and named SSE events.
use super::auth::AuthPlan;
use super::route::Route;
use super::sse::{self, SseDecoder, SseFrame};
use super::{
    Block, BoxFuture, Provider, ProviderError, ProviderErrorKind, ProviderRequest, Role,
    StopReason, StreamEvent, Turn, http_client, map_reqwest, map_status,
};
use crate::commands::AiProviderEntry;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) const ANTHROPIC_VERSION: &str = "2026-10-21";

/// Anthropic Messages API adapter.
pub struct AnthropicMessages {
    entry: AiProviderEntry,
    route: Route,
    auth: AuthPlan,
}

impl AnthropicMessages {
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
        let budget = super::effort::supported(&self.entry)
            .then(|| super::effort::anthropic_budget(&request.thinking_level, request.max_tokens))
            .flatten();
        let mut body = json!({
            "model": self.entry.model,
            "max_tokens": request.max_tokens,
            "messages": messages(request, &self.entry.model, budget.is_some())?,
            "stream": true
        });
        if let Some(system) = &request.system {
            body["system"] = json!(system);
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters
                    }))
                    .collect::<Vec<_>>()
            );
            body["tool_choice"] = json!({"type": "auto"});
        }
        if let Some(budget) = budget {
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        }
        Ok(body)
    }
}

fn messages(
    request: &ProviderRequest,
    model: &str,
    thinking: bool,
) -> Result<Value, ProviderError> {
    let mut messages: Vec<Value> = Vec::new();
    for (index, turn) in request.turns.iter().enumerate() {
        match turn.role {
            Role::User => {
                let mut content = Vec::new();
                for block in &turn.blocks {
                    match block {
                        Block::Text(text) if !text.is_empty() => {
                            content.push(json!({"type": "text", "text": text}));
                        }
                        Block::Media { .. } => {
                            let (kind, mime, data) = super::media::inline_media(block)?;
                            if kind == super::media::MediaKind::Audio {
                                return Err(no_audio(model));
                            }
                            content.push(json!({
                                "type": "image",
                                "source": {"type": "base64", "media_type": mime, "data": data}
                            }));
                        }
                        _ => {}
                    }
                }
                if content.is_empty() {
                    content.push(json!({"type": "text", "text": ""}));
                }
                push(&mut messages, "user", content);
            }
            Role::Assistant => {
                let mut content = Vec::new();
                for block in &turn.blocks {
                    match block {
                        Block::Text(text) => content.push(json!({"type": "text", "text": text})),
                        Block::Thinking {
                            text,
                            signature,
                            redacted,
                        } => {
                            if let Some(redacted) = redacted {
                                content.push(json!({
                                    "type": "redacted_thinking",
                                    "data": redacted
                                }));
                            } else if let Some(signature) = signature {
                                content.push(json!({
                                    "type": "thinking",
                                    "thinking": text,
                                    "signature": signature
                                }));
                            }
                        }
                        Block::ToolUse {
                            id, name, input, ..
                        } => content.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        })),
                        Block::Media { .. } => return Err(no_audio(model)),
                        Block::ToolResult { .. } => {}
                    }
                }
                if thinking
                    && content.iter().any(|block| block["type"] == "tool_use")
                    && !content.iter().any(|block| {
                        matches!(
                            block["type"].as_str(),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                {
                    // A turn saved without its reasoning still gets sent: the
                    // provider may accept it, and a rejected request is
                    // retryable, while dropping the call would strand a batch.
                    log::warn!(
                        "[ai] assistant turn {index} replays tool calls without a signed thinking \
                         block; a thinking-enabled request may be rejected"
                    );
                }
                push(&mut messages, "assistant", content);
            }
            Role::Tool => {
                let content = turn
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        Block::ToolResult {
                            call_id,
                            content,
                            is_error,
                            ..
                        } => Some(json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": content,
                            "is_error": is_error
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                push(&mut messages, "user", content);
            }
            Role::System => {}
        }
    }
    Ok(json!(messages))
}

/// The Messages wire has no audio content block, so audio is refused by name
/// instead of being lowered into a malformed request.
fn no_audio(model: &str) -> ProviderError {
    ProviderError::config(format!(
        "{model} cannot accept audio input: the Anthropic Messages wire has no audio content \
         block. Remove the attachment or choose a Gemini or OpenAI model."
    ))
}

fn push(messages: &mut Vec<Value>, role: &str, content: Vec<Value>) {
    if content.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last["role"] == role
        && let Some(existing) = last["content"].as_array_mut()
    {
        existing.extend(content);
        return;
    }
    messages.push(json!({"role": role, "content": content}));
}

#[derive(Debug)]
enum BlockState {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    /// Reasoning the provider withheld; the payload is replayed, not read.
    Redacted(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

#[derive(Debug, Default)]
struct State {
    blocks: BTreeMap<i64, BlockState>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: Option<u64>,
    stop: StopReason,
    started: bool,
    terminal: bool,
}

impl State {
    fn consume(
        &mut self,
        frame: SseFrame,
        sink: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), ProviderError> {
        let event: Value = serde_json::from_str(&frame.data)
            .map_err(|_| ProviderError::protocol("Invalid streaming JSON"))?;
        let kind = event["type"]
            .as_str()
            .or(frame.event.as_deref())
            .unwrap_or_default();
        match kind {
            "message_start" => {
                self.started = true;
                let message = &event["message"];
                if let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) {
                    sink(StreamEvent::RequestId(id.to_owned()));
                }
                let usage = &message["usage"];
                let cached = usage["cache_read_input_tokens"].as_u64();
                // Anthropic reports uncached input separately; keep cached a
                // subset of the canonical input.
                self.input_tokens = usage["input_tokens"].as_u64().unwrap_or(0)
                    + cached.unwrap_or(0)
                    + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                self.output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
                self.cached_tokens = cached;
            }
            "content_block_start" => {
                let index = event["index"].as_i64().unwrap_or_default();
                let block = &event["content_block"];
                let state = match block["type"].as_str() {
                    Some("text") => {
                        BlockState::Text(block["text"].as_str().unwrap_or_default().to_owned())
                    }
                    Some("thinking") => BlockState::Thinking {
                        text: block["thinking"].as_str().unwrap_or_default().to_owned(),
                        signature: block["signature"]
                            .as_str()
                            .filter(|signature| !signature.is_empty())
                            .map(str::to_owned),
                    },
                    Some("redacted_thinking") => {
                        BlockState::Redacted(block["data"].as_str().unwrap_or_default().to_owned())
                    }
                    Some("tool_use") => BlockState::Tool {
                        id: block["id"].as_str().unwrap_or_default().to_owned(),
                        name: block["name"].as_str().unwrap_or_default().to_owned(),
                        json: String::new(),
                    },
                    _ => return Ok(()),
                };
                if let BlockState::Tool { name, .. } = &state {
                    sink(StreamEvent::ToolPreparing(name.clone()));
                }
                self.blocks.insert(index, state);
            }
            "content_block_delta" => {
                let index = event["index"].as_i64().unwrap_or_default();
                let delta = &event["delta"];
                if let Some(block) = self.blocks.get_mut(&index) {
                    match (delta["type"].as_str(), block) {
                        (Some("text_delta"), BlockState::Text(text)) => {
                            if let Some(part) = delta["text"].as_str() {
                                text.push_str(part);
                                sink(StreamEvent::TextDelta(part.to_owned()));
                            }
                        }
                        (Some("thinking_delta"), BlockState::Thinking { text, .. }) => {
                            if let Some(part) = delta["thinking"].as_str() {
                                text.push_str(part);
                                sink(StreamEvent::ThinkingDelta(part.to_owned()));
                            }
                        }
                        (Some("signature_delta"), BlockState::Thinking { signature, .. }) => {
                            if let Some(part) = delta["signature"].as_str() {
                                *signature = Some(part.to_owned());
                            }
                        }
                        (Some("input_json_delta"), BlockState::Tool { json, .. }) => {
                            if let Some(part) = delta["partial_json"].as_str() {
                                json.push_str(part);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                self.stop = stop_reason(&event["delta"]["stop_reason"]);
                if let Some(output) = event["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = output;
                }
                sink(self.usage_event());
            }
            "message_stop" => self.terminal = true,
            "error" => return Err(streaming_error(&event)),
            "ping" | "content_block_stop" => {}
            _ => {}
        }
        Ok(())
    }

    fn usage_event(&self) -> StreamEvent {
        StreamEvent::Usage {
            input: self.input_tokens,
            output: self.output_tokens,
            reasoning: 0,
            cached: self.cached_tokens,
        }
    }

    fn turn(self) -> Result<Turn, ProviderError> {
        if !self.started || !self.terminal {
            return Err(ProviderError::protocol(
                "Streaming response ended prematurely; nothing was applied",
            ));
        }
        if self.stop == StopReason::MaxTokens {
            return Err(ProviderError::truncated(
                "Completion truncated. Increase output budget or request a smaller edit.",
            ));
        }
        let mut blocks = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                BlockState::Text(text) => {
                    if !text.is_empty() {
                        blocks.push(Block::Text(text));
                    }
                }
                BlockState::Thinking { text, signature } => {
                    if !text.is_empty() || signature.is_some() {
                        blocks.push(Block::Thinking {
                            text,
                            signature,
                            redacted: None,
                        });
                    }
                }
                BlockState::Redacted(data) => {
                    if !data.is_empty() {
                        blocks.push(Block::Thinking {
                            text: String::new(),
                            signature: None,
                            redacted: Some(data),
                        });
                    }
                }
                BlockState::Tool { id, name, json } => {
                    if id.is_empty() {
                        continue;
                    }
                    let input = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json).map_err(|_| {
                            ProviderError::unusable("Provider returned malformed tool arguments")
                        })?
                    };
                    blocks.push(Block::ToolUse {
                        id,
                        name,
                        input,
                        extra: None,
                    });
                }
            }
        }
        Ok(Turn {
            role: Role::Assistant,
            blocks,
            continuity: None,
            extra: None,
        })
    }
}

fn stop_reason(reason: &Value) -> StopReason {
    match reason.as_str() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("end_turn") | Some("stop_sequence") | None => StopReason::EndTurn,
        Some(other) => StopReason::Other(other.to_owned()),
    }
}

fn streaming_error(event: &Value) -> ProviderError {
    let code = event["error"]["type"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if code.contains("auth") {
        ProviderErrorKind::Auth
    } else if code.contains("rate") {
        ProviderErrorKind::RateLimit
    } else if code.contains("overloaded") {
        ProviderErrorKind::Server
    } else {
        ProviderErrorKind::Protocol
    };
    ProviderError::new(kind, "Provider returned a streaming error.")
}

#[derive(Debug, Default)]
struct Stream {
    decoder: SseDecoder,
    state: State,
}

impl Stream {
    #[cfg(test)]
    fn push(
        &mut self,
        chunk: &[u8],
        sink: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), ProviderError> {
        let Self { decoder, state } = self;
        decoder.push(chunk, &mut |frame| state.consume(frame, &mut *sink))
    }

    fn finish(mut self, sink: &mut dyn FnMut(StreamEvent)) -> Result<Turn, ProviderError> {
        {
            let Self { decoder, state } = &mut self;
            decoder.finish(&mut |frame| state.consume(frame, &mut *sink))?;
        }
        let turn = self.state.turn()?;
        super::tool_use_events(&turn, sink);
        sink(StreamEvent::Done {
            reason: if turn
                .blocks
                .iter()
                .any(|block| matches!(block, Block::ToolUse { .. }))
            {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        });
        Ok(turn)
    }
}

impl Provider for AnthropicMessages {
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
            let client = http_client(super::STREAM_TIMEOUT_SECS)?;
            let mut http_request = client
                .post(url)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .build()
                .map_err(|_| ProviderError::config("Invalid provider request"))?;
            self.auth.apply(http_request.headers_mut())?;
            let response = client
                .execute(http_request)
                .await
                .map_err(|error| map_reqwest(error, &self.entry.name))?;
            if !response.status().is_success() {
                return Err(map_status(response.status().as_u16(), &self.entry.name));
            }
            let mut response = response;
            let mut stream = Stream::default();
            let mut on_frame = |frame: SseFrame| stream.state.consume(frame, sink);
            sse::drive(&mut response, &mut on_frame).await?;
            let _ = on_frame;
            stream.finish(sink)
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
        let body = AnthropicMessages::for_test(
            entry("https://example.test/v1/messages".into()),
            AuthPlan::default(),
        )
        .body(&request)
        .unwrap();
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

    use super::*;
    use crate::ai::{
        ProviderRequest, ToolSpec, assistant_turn_from_wire, test_support::canned,
        test_support::canned_by_request, turn_to_wire, turns_from_wire,
    };
    use crate::commands::Wire;
    use std::sync::{Arc, Mutex};

    const TEXT: &str = include_str!("../../tests/fixtures/anthropic_messages_text.sse");
    const TOOLS: &str = include_str!("../../tests/fixtures/anthropic_messages_tools.sse");

    fn entry(endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            kind: crate::commands::ProviderKind::AnthropicMessages,
            wire: Some(Wire::AnthropicMessages),
            auth: crate::commands::AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "claude-opus-4-6".into(),
            models: Vec::new(),
            context_length: 200000,
            max_tokens: 4096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: crate::commands::AiCapabilities {
                text: true,
                audio_in: false,
                audio_out: false,
                native_tool_calling: true,
                streaming: true,
            },
        }
    }

    fn request() -> ProviderRequest {
        ProviderRequest {
            system: Some("You are OpenCompose Assistant.".into()),
            turns: turns_from_wire(&json!([
                {"role":"user","content":"Read track:bass"},
                {"role":"assistant","content":null,"tool_calls":[{"id":"toolu_0","type":"function","function":{"name":"read_context","arguments":"{\"path\":\"project\"}"}}]},
                {"role":"tool","tool_call_id":"toolu_0","content":"@project"}
            ]))
            .unwrap(),
            tools: vec![ToolSpec {
                name: "read_context".into(),
                description: "Read current project subtree as DSL".into(),
                parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            }],
            max_tokens: 4096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: true,
        }
    }

    fn feed(fixture: &str) -> Result<(Turn, Vec<StreamEvent>), ProviderError> {
        let mut stream = Stream::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let collected = events.clone();
        let mut sink = move |event: StreamEvent| collected.lock().unwrap().push(event);
        for chunk in fixture.as_bytes().chunks(5) {
            stream.push(chunk, &mut sink)?;
        }
        let turn = stream.finish(&mut sink)?;
        let events = events.lock().unwrap().clone();
        Ok((turn, events))
    }

    #[test]
    fn tool_preparation_precedes_argument_completion() {
        let mut stream = Stream::default();
        let mut events = Vec::new();
        let frame = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-a","name":"edit_subtree","input":{}}}"#;
        stream
            .push(format!("data: {frame}\n\n").as_bytes(), &mut |event| {
                events.push(event)
            })
            .unwrap();
        assert!(events.contains(&StreamEvent::ToolPreparing("edit_subtree".into())));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::ToolCall { .. }))
        );
    }

    #[test]
    fn text_fixture_maps_thinking_text_and_usage() {
        let (turn, events) = feed(TEXT).unwrap();
        assert_eq!(
            turn.blocks,
            vec![
                Block::Thinking {
                    text: "Consider the bass.".into(),
                    signature: Some("sig-1".into()),
                    redacted: None,
                },
                Block::Text("Hello!".into()),
            ]
        );
        assert!(events.contains(&StreamEvent::TextDelta("Hello".into())));
        assert!(events.contains(&StreamEvent::ThinkingDelta("Consider the bass.".into())));
        assert!(events.contains(&StreamEvent::Usage {
            input: 14,
            output: 7,
            reasoning: 0,
            cached: Some(2),
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
    }

    #[test]
    fn tool_fixture_reassembles_partial_json() {
        let (turn, events) = feed(TOOLS).unwrap();
        assert_eq!(
            turn.blocks,
            vec![Block::ToolUse {
                id: "toolu_1".into(),
                name: "read_context".into(),
                input: json!({"path":"track:bass"}),
                extra: None,
            }]
        );
        assert!(events.contains(&StreamEvent::ToolCall {
            id: "toolu_1".into(),
            name: "read_context".into(),
            input: json!({"path":"track:bass"}),
        }));
    }

    /// A tool round replayed to the provider carries the signature the first
    /// turn returned. The fixture rejects an assistant turn that lost it, which
    /// is what a thinking-enabled provider does when the block is dropped.
    #[tokio::test]
    async fn a_replayed_tool_round_carries_the_signed_thinking_block() {
        const FIRST: &str = "\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":10}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Check the bass.\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-replay\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"read_context\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"track:bass\\\"}\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        const ACCEPTED: &str = "\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"usage\":{\"input_tokens\":12}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Bass is in.\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let (base, server) = canned_by_request(3, move |request| {
            let replayed = request.contains("\"role\":\"assistant\"");
            if replayed && !request.contains("sig-replay") {
                // What a provider that requires the signed block answers.
                return (
                    400,
                    json!({"type":"error","error":{"type":"invalid_request_error","message":"thinking block is required"}})
                        .to_string(),
                );
            }
            if replayed {
                (200, ACCEPTED.to_string())
            } else {
                (200, FIRST.to_string())
            }
        });
        let provider =
            AnthropicMessages::for_test(entry(format!("{base}v1/messages")), AuthPlan::default());
        let mut sink = |_: StreamEvent| {};
        let mut first = request();
        first.turns =
            turns_from_wire(&json!([{"role":"user","content":"Read track:bass"}])).unwrap();
        let turn = provider.stream(&first, &mut sink).await.unwrap();
        assert_eq!(
            turn.blocks,
            vec![
                Block::Thinking {
                    text: "Check the bass.".into(),
                    signature: Some("sig-replay".into()),
                    redacted: None,
                },
                Block::ToolUse {
                    id: "toolu_9".into(),
                    name: "read_context".into(),
                    input: json!({"path": "track:bass"}),
                    extra: None,
                },
            ]
        );

        let result = turns_from_wire(
            &json!([{"role":"tool","tool_call_id":"toolu_9","content":"@project"}]),
        )
        .unwrap();
        // The unsigned message an older build replayed: text and call only.
        let unsigned = assistant_turn_from_wire(&json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "toolu_9",
                "type": "function",
                "function": {"name": "read_context", "arguments": "{\"path\":\"track:bass\"}"}
            }]
        }))
        .unwrap();
        let mut red = first.clone();
        red.turns.push(unsigned);
        red.turns.extend(result.clone());
        let error = provider.stream(&red, &mut sink).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Request, "{error:?}");

        // The same round through this build's wire keeps the signature.
        let signed = assistant_turn_from_wire(&turn_to_wire(&turn)).unwrap();
        let mut green = first.clone();
        green.turns.push(signed);
        green.turns.extend(result);
        let answer = provider.stream(&green, &mut sink).await.unwrap();
        assert_eq!(answer.blocks, vec![Block::Text("Bass is in.".into())]);

        let requests = server.join().unwrap();
        let body = |raw: &str| -> Value {
            let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
            serde_json::from_str(body).expect("the request body is JSON")
        };
        assert!(
            !body(&requests[1])["messages"]
                .to_string()
                .contains("sig-replay"),
            "the unsigned replay carried no signature"
        );
        assert_eq!(
            body(&requests[2])["messages"][1]["content"],
            json!([
                {"type": "thinking", "thinking": "Check the bass.", "signature": "sig-replay"},
                {"type": "tool_use", "id": "toolu_9", "name": "read_context", "input": {"path": "track:bass"}},
            ])
        );
    }

    #[test]
    fn request_requires_max_tokens_and_uses_input_schema() {
        let body = AnthropicMessages::for_test(
            entry("https://example.test/v1/messages".into()),
            AuthPlan::default(),
        )
        .body(&request())
        .unwrap();
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["system"], "You are OpenCompose Assistant.");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert!(body["tools"][0]["parameters"].is_null());
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_0");
        assert_eq!(body["message"], Value::Null);
        assert!(body["temperature"].is_null());
    }

    /// Extended thinking is requested at the saved level, with a budget that
    /// always leaves room for the reply; a model that declares no levels is
    /// left alone.
    #[test]
    fn the_saved_thinking_level_becomes_a_thinking_budget() {
        let mut with_budget = request();
        with_budget.max_tokens = 32_768;
        let provider = AnthropicMessages::for_test(
            entry("https://example.test/v1/messages".into()),
            AuthPlan::default(),
        );
        for (level, budget) in [("low", 1_024), ("medium", 4_096), ("high", 8_192)] {
            with_budget.thinking_level = level.into();
            let body = provider.body(&with_budget).unwrap();
            assert_eq!(body["thinking"]["type"], "enabled", "{level}: {body}");
            assert_eq!(body["thinking"]["budget_tokens"], budget, "{level}: {body}");
        }
        // A small output allowance still fits a smaller thinking block.
        with_budget.max_tokens = 4_096;
        with_budget.thinking_level = "high".into();
        let body = provider.body(&with_budget).unwrap();
        assert_eq!(body["thinking"]["budget_tokens"], 2_048, "{body}");

        // Too small for any thinking budget: the field is omitted, not rejected.
        with_budget.max_tokens = 1_024;
        let body = provider.body(&with_budget).unwrap();
        assert!(body.get("thinking").is_none(), "{body}");

        // A model with no declared levels never carries the field.
        let mut haiku = entry("https://example.test/v1/messages".into());
        haiku.model = "claude-haiku-4-5".into();
        let body = AnthropicMessages::for_test(haiku, AuthPlan::default())
            .body(&with_budget)
            .unwrap();
        assert!(body.get("thinking").is_none(), "{body}");
    }

    #[test]
    fn malformed_chunk_error_event_and_exit_without_terminal_fail() {
        let error = feed("data: {\"type\":\n\n").unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        let error = feed(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"do not echo\"}}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Server);
        assert!(!error.message.contains("do not echo"));
        let error = feed(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
    }

    #[test]
    fn max_tokens_stop_is_reported_as_truncation() {
        let proxy = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n\
                     event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":2}}\n\n\
                     event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(feed(proxy).unwrap_err().kind, ProviderErrorKind::Truncated);
    }

    #[tokio::test]
    async fn auth_failure_is_distinct_and_headers_are_sent_without_leaking() {
        let (base, server) = canned(401, "fixture-secret must never escape");
        let entry = entry(format!("{base}v1/messages"));
        let plan = crate::ai::auth::plan(
            &entry,
            &crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
        )
        .unwrap();
        let provider = AnthropicMessages::for_test(entry, plan);
        let mut sink = |_: StreamEvent| {};
        let error = provider.stream(&request(), &mut sink).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("fixture-secret"));
        let raw = server.join().unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("post /v1/messages "), "{raw}");
        assert!(raw.contains("x-api-key: fixture-secret"));
        assert!(raw.contains(&format!("anthropic-version: {ANTHROPIC_VERSION}")));
        assert!(!raw.contains("authorization:"));
    }
}
