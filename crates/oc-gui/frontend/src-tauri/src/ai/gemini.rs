//! Gemini Interactions adapter: `input`/`steps`, flat function tools,
//! function_result replay, `event_type` SSE and thinking levels.
use super::auth::AuthPlan;
use super::route::Route;
use super::sse::{self, SseDecoder, SseFrame};
use super::{
    Block, BoxFuture, Provider, ProviderError, ProviderErrorKind, ProviderRequest, Role,
    StopReason, StreamEvent, Turn, block_text, map_reqwest, map_status,
};
use crate::commands::AiProviderEntry;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Gemini interactions adapter.
pub struct GeminiInteractions {
    entry: AiProviderEntry,
    route: Route,
    auth: AuthPlan,
}

impl GeminiInteractions {
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
        let mut generation = json!({"max_output_tokens": request.max_tokens});
        // A model the catalog gives no levels takes no `thinking_level` field at
        // all; the saved default is never sent as a guess.
        if !super::catalog::thinking_levels(self.entry.wire(), &self.entry.model).is_empty() {
            generation["thinking_level"] = json!(request.thinking_level);
        }
        if !request.tools.is_empty() {
            generation["tool_choice"] = json!("auto");
        }
        let mut body = json!({
            "model": self.entry.model,
            "input": input(request)?,
            "generation_config": generation,
            "store": false,
            "stream": true
        });
        if let Some(system) = &request.system {
            body["system_instruction"] = json!(system);
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters
                    }))
                    .collect::<Vec<_>>()
            );
        }
        Ok(body)
    }
}

fn input(request: &ProviderRequest) -> Result<Value, ProviderError> {
    let mut steps = Vec::new();
    for turn in &request.turns {
        match turn.role {
            Role::User => steps.push(json!({
                "type": "user_input",
                "content": user_content(turn)?
            })),
            Role::Assistant => {
                let text = block_text(&turn.blocks);
                if !text.is_empty() {
                    steps.push(json!({
                        "type": "model_output",
                        "content": [{"type": "text", "text": text}]
                    }));
                }
                for block in &turn.blocks {
                    if let Block::ToolUse {
                        id, name, input, ..
                    } = block
                    {
                        steps.push(json!({
                            "type": "function_call",
                            "id": id,
                            "name": name,
                            "arguments": input
                        }));
                    }
                }
            }
            Role::Tool => {
                for block in &turn.blocks {
                    if let Block::ToolResult {
                        call_id,
                        name,
                        content,
                        ..
                    } = block
                    {
                        steps.push(json!({
                            "type": "function_result",
                            "call_id": call_id,
                            "name": name,
                            "result": content
                        }));
                    }
                }
            }
            Role::System => {}
        }
    }
    Ok(json!(steps))
}

/// One `user_input` step carries text and audio content parts; the wire's
/// content union has no standalone text step.
fn user_content(turn: &Turn) -> Result<Value, ProviderError> {
    let mut content = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text(text) if !text.is_empty() => {
                content.push(json!({"type": "text", "text": text}))
            }
            Block::Media { .. } => {
                let (kind, mime, data) = super::media::inline_media(block)?;
                let content_type = if kind == super::media::MediaKind::Audio {
                    "audio"
                } else {
                    "image"
                };
                content.push(json!({"type": content_type, "data": data, "mime_type": mime}));
            }
            _ => {}
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }
    Ok(Value::Array(content))
}

#[derive(Debug)]
enum StepState {
    Text,
    Function {
        id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Debug, Default)]
struct State {
    steps: BTreeMap<i64, StepState>,
    text: String,
    thinking: String,
    interaction: Option<Value>,
    usage: Option<StreamEvent>,
    request_id: Option<String>,
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
        if self.request_id.is_none()
            && let Some(id) = event["interaction"]["id"]
                .as_str()
                .or_else(|| event["id"].as_str())
                .filter(|id| !id.is_empty())
        {
            self.request_id = Some(id.to_owned());
            sink(StreamEvent::RequestId(id.to_owned()));
        }
        let kind = event["event_type"]
            .as_str()
            .or_else(|| event["type"].as_str())
            .or(frame.event.as_deref())
            .unwrap_or_default();
        match kind {
            "interaction.created" => self.started = true,
            "interaction.status_update" => {
                self.started = true;
                if let Some(usage) = event
                    .get("interaction")
                    .and_then(|value| value.get("usage"))
                    && self.usage.is_none()
                {
                    self.usage = Some(usage_event(usage));
                }
                match event["status"]
                    .as_str()
                    .or_else(|| event["interaction"]["status"].as_str())
                    .unwrap_or_default()
                {
                    "failed" => {
                        self.terminal = true;
                        return Err(ProviderError::protocol(
                            "Provider reported a failed interaction.",
                        ));
                    }
                    "cancelled" => {
                        self.terminal = true;
                        return Err(ProviderError::protocol(
                            "Provider cancelled the interaction.",
                        ));
                    }
                    "incomplete" | "budget_exceeded" => {
                        self.terminal = true;
                        return Err(ProviderError::truncated(
                            "Completion truncated. Increase output budget or request a smaller edit.",
                        ));
                    }
                    _ => {}
                }
            }
            "step.start" => {
                let index = event["index"].as_i64().unwrap_or_default();
                let step = &event["step"];
                let state = match step["type"].as_str() {
                    Some("model_output") => StepState::Text,
                    Some("function_call") => StepState::Function {
                        id: step["id"].as_str().unwrap_or_default().to_owned(),
                        name: step["name"].as_str().unwrap_or_default().to_owned(),
                        arguments: String::new(),
                    },
                    _ => return Ok(()),
                };
                if let StepState::Function { name, .. } = &state {
                    sink(StreamEvent::ToolPreparing(name.clone()));
                }
                self.steps.insert(index, state);
            }
            "step.delta" => {
                let index = event["index"].as_i64().unwrap_or_default();
                let delta = &event["delta"];
                match (delta["type"].as_str(), self.steps.get_mut(&index)) {
                    (Some("text"), Some(StepState::Text)) => {
                        if let Some(part) = delta["text"].as_str() {
                            self.text.push_str(part);
                            sink(StreamEvent::TextDelta(part.to_owned()));
                        }
                    }
                    (Some("thought" | "thinking"), _) => {
                        if let Some(part) = delta["text"].as_str() {
                            self.thinking.push_str(part);
                            sink(StreamEvent::ThinkingDelta(part.to_owned()));
                        }
                    }
                    (Some("arguments"), Some(StepState::Function { arguments, .. })) => {
                        if let Some(part) = delta["arguments"].as_str() {
                            arguments.push_str(part);
                        }
                    }
                    _ => {}
                }
            }
            "step.stop" => {
                if let Some(usage) = event.get("step").and_then(|step| step.get("usage")) {
                    self.usage = Some(usage_event(usage));
                }
            }
            "interaction.completed" => {
                self.terminal = true;
                let interaction = event["interaction"].clone();
                if let Some(usage) = interaction.get("usage") {
                    self.usage = Some(usage_event(usage));
                }
                self.interaction = Some(interaction);
            }
            "error" => return Err(streaming_error(&event)),
            _ => {}
        }
        Ok(())
    }

    fn turn(self, sink: &mut dyn FnMut(StreamEvent)) -> Result<Turn, ProviderError> {
        if !self.started || !self.terminal {
            return Err(ProviderError::protocol(
                "Streaming response ended prematurely; nothing was applied",
            ));
        }
        let steps = self
            .interaction
            .as_ref()
            .and_then(|interaction| interaction["steps"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut blocks = Vec::new();
        if steps.is_empty() {
            if !self.thinking.is_empty() {
                blocks.push(Block::Thinking {
                    text: self.thinking,
                    signature: None,
                    redacted: None,
                });
            }
            if !self.text.is_empty() {
                blocks.push(Block::Text(self.text));
            }
            for step in self.steps.into_values() {
                if let StepState::Function {
                    id,
                    name,
                    arguments,
                } = step
                {
                    if id.is_empty() {
                        continue;
                    }
                    let input = if arguments.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&arguments).map_err(|_| {
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
        } else {
            for step in &steps {
                match step["type"].as_str() {
                    Some("model_output") => {
                        let text = step["content"]
                            .as_array()
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|part| part["text"].as_str())
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        if !text.is_empty() {
                            blocks.push(Block::Text(text));
                        }
                    }
                    Some("function_call") => {
                        let id = step["id"].as_str().unwrap_or_default();
                        if id.is_empty() {
                            continue;
                        }
                        blocks.push(Block::ToolUse {
                            id: id.to_owned(),
                            name: step["name"].as_str().unwrap_or_default().to_owned(),
                            input: step_arguments(&step["arguments"])?,
                            extra: None,
                        });
                    }
                    Some("thought" | "thinking") => {
                        let text = step["content"]
                            .as_array()
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|part| part["text"].as_str())
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        if !text.is_empty() {
                            blocks.push(Block::Thinking {
                                text,
                                signature: None,
                                redacted: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(usage) = self.usage {
            sink(usage);
        }
        super::tool_use_events(
            &Turn {
                role: Role::Assistant,
                blocks: blocks.clone(),
                continuity: None,
                extra: None,
            },
            sink,
        );
        sink(StreamEvent::Done {
            reason: if blocks
                .iter()
                .any(|block| matches!(block, Block::ToolUse { .. }))
            {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        });
        Ok(Turn {
            role: Role::Assistant,
            blocks,
            continuity: None,
            extra: None,
        })
    }
}

fn step_arguments(value: &Value) -> Result<Value, ProviderError> {
    match value {
        Value::Object(_) => Ok(value.clone()),
        Value::String(raw) if !raw.trim().is_empty() => serde_json::from_str(raw)
            .map_err(|_| ProviderError::unusable("Provider returned malformed tool arguments")),
        _ => Ok(json!({})),
    }
}

fn streaming_error(event: &Value) -> ProviderError {
    let code = event["error"]["status"]
        .as_str()
        .or_else(|| event["error"]["type"].as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if code.contains("auth") {
        ProviderErrorKind::Auth
    } else if code.contains("rate") {
        ProviderErrorKind::RateLimit
    } else if code.contains("unavailable") {
        ProviderErrorKind::Server
    } else {
        ProviderErrorKind::Protocol
    };
    ProviderError::new(kind, "Provider returned a streaming error.")
}

fn usage_event(usage: &Value) -> StreamEvent {
    StreamEvent::Usage {
        input: usage["total_input_tokens"].as_u64().unwrap_or(0),
        output: usage["total_output_tokens"].as_u64().unwrap_or(0),
        reasoning: usage["total_thought_tokens"].as_u64().unwrap_or(0),
        cached: usage["total_cached_tokens"].as_u64(),
    }
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
        self.state.turn(sink)
    }
}

impl Provider for GeminiInteractions {
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
            let client = super::http_client_for(url.as_str(), super::STREAM_TIMEOUT_SECS)?;
            let mut http_request = client
                .post(url)
                .header("accept", "text/event-stream")
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
        let body = GeminiInteractions::for_test(
            entry("https://example.test/v1beta/interactions".into()),
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
    use crate::ai::{ProviderRequest, ToolSpec, test_support::canned, turns_from_wire};
    use crate::commands::Wire;
    use std::sync::{Arc, Mutex};

    const TEXT: &str = include_str!("../../tests/fixtures/gemini_interactions_text.sse");
    const TOOLS: &str = include_str!("../../tests/fixtures/gemini_interactions_tools.sse");

    fn entry(endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "gemini".into(),
            name: "Gemini".into(),
            kind: crate::commands::ProviderKind::GeminiInteractions,
            wire: Some(Wire::GeminiInteractions),
            auth: crate::commands::AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "gemini-4-argon".into(),
            models: Vec::new(),
            context_length: 1048576,
            max_tokens: 65536,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: crate::commands::AiCapabilities {
                text: true,
                audio_in: true,
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
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_0","type":"function","function":{"name":"read_context","arguments":"{\"path\":\"project\"}"}}]},
                {"role":"tool","tool_call_id":"call_0","content":"@project"}
            ]))
            .unwrap(),
            tools: vec![ToolSpec {
                name: "read_context".into(),
                description: "Read current project subtree as DSL".into(),
                parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            }],
            max_tokens: 65536,
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
        for chunk in fixture.as_bytes().chunks(9) {
            stream.push(chunk, &mut sink)?;
        }
        let turn = stream.finish(&mut sink)?;
        let events = events.lock().unwrap().clone();
        Ok((turn, events))
    }

    #[test]
    fn text_fixture_maps_to_canonical_turn_and_usage() {
        let (turn, events) = feed(TEXT).unwrap();
        assert_eq!(turn.blocks, vec![Block::Text("Hello!".into())]);
        assert!(events.contains(&StreamEvent::TextDelta("Hello".into())));
        assert!(events.contains(&StreamEvent::Usage {
            input: 12,
            output: 7,
            reasoning: 3,
            cached: None,
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
    }

    #[test]
    fn streamed_thinking_survives_a_completion_without_steps() {
        let fixture = concat!(
            "data: {\"type\":\"interaction.created\"}\n\n",
            "data: {\"type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"model_output\"}}\n\n",
            "data: {\"type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"thought\",\"text\":\"Check the drums.\"}}\n\n",
            "data: {\"type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"Done.\"}}\n\n",
            "data: {\"type\":\"interaction.completed\",\"interaction\":{}}\n\n",
        );
        let (turn, _) = feed(fixture).unwrap();
        assert!(turn.blocks.contains(&Block::Thinking {
            text: "Check the drums.".into(),
            signature: None,
            redacted: None
        }));
    }

    #[test]
    fn tool_fixture_maps_object_arguments() {
        let (turn, events) = feed(TOOLS).unwrap();
        assert!(events.contains(&StreamEvent::ToolPreparing("read_context".into())));
        assert_eq!(
            turn.blocks,
            vec![Block::ToolUse {
                id: "call_1".into(),
                name: "read_context".into(),
                input: json!({"path":"track:bass"}),
                extra: None,
            }]
        );
        assert!(events.contains(&StreamEvent::ToolCall {
            id: "call_1".into(),
            name: "read_context".into(),
            input: json!({"path":"track:bass"}),
        }));
    }

    #[test]
    fn request_replays_steps_with_thinking_level_and_flat_tools() {
        let body = GeminiInteractions::for_test(
            entry("https://example.test/v1beta/interactions".into()),
            AuthPlan::default(),
        )
        .body(&request())
        .unwrap();
        assert_eq!(body["model"], "gemini-4-argon");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system_instruction"], "You are OpenCompose Assistant.");
        assert_eq!(body["generation_config"]["thinking_level"], "medium");
        assert_eq!(body["generation_config"]["max_output_tokens"], 65536);
        assert_eq!(body["generation_config"]["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_context");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][1]["arguments"]["path"], "project");
        assert_eq!(body["input"][2]["type"], "function_result");
        assert_eq!(body["input"][2]["call_id"], "call_0");
        assert_eq!(body["input"][2]["name"], "read_context");
        assert!(body["messages"].is_null());
    }

    #[test]
    fn the_body_carries_the_requested_thinking_level() {
        let mut request = request();
        request.thinking_level = "high".into();
        let body = GeminiInteractions::for_test(
            entry("https://example.test/v1beta/interactions".into()),
            AuthPlan::default(),
        )
        .body(&request)
        .unwrap();
        assert_eq!(body["generation_config"]["thinking_level"], "high");
    }

    #[test]
    fn a_model_without_declared_levels_omits_the_thinking_level() {
        let mut entry = entry("https://example.test/v1beta/interactions".into());
        entry.model = "gemini-2.5-pro".into();
        let body = GeminiInteractions::for_test(entry, AuthPlan::default())
            .body(&request())
            .unwrap();
        assert!(
            body["generation_config"].get("thinking_level").is_none(),
            "a model the catalog gives no levels never receives a guessed level"
        );
        assert_eq!(body["generation_config"]["max_output_tokens"], 65536);
    }

    #[test]
    fn malformed_chunk_error_event_and_exit_without_terminal_fail() {
        let error = feed("data: {\"event_type\":\n\n").unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        let error = feed(
            "data: {\"event_type\":\"error\",\"error\":{\"status\":\"auth_error\",\"message\":\"do not echo\"}}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("do not echo"));
        let error = feed(
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"partial\"}}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
    }

    #[test]
    fn incomplete_interaction_is_reported_as_truncation() {
        let proxy = "data: {\"event_type\":\"interaction.created\",\"interaction\":{}}\n\n\
                     data: {\"event_type\":\"interaction.status_update\",\"status\":\"incomplete\"}\n\n";
        assert_eq!(feed(proxy).unwrap_err().kind, ProviderErrorKind::Truncated);
    }

    #[tokio::test]
    async fn auth_failure_is_distinct_and_headers_are_sent_without_leaking() {
        let (base, server) = canned(401, "fixture-secret must never escape");
        let entry = entry(format!("{base}v1beta/interactions"));
        let plan = crate::ai::auth::plan(
            &entry,
            &crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
        )
        .unwrap();
        let provider = GeminiInteractions::for_test(entry, plan);
        let mut sink = |_: StreamEvent| {};
        let error = provider.stream(&request(), &mut sink).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("fixture-secret"));
        let raw = server.join().unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("post /v1beta/interactions "), "{raw}");
        assert!(raw.contains("x-goog-api-key: fixture-secret"));
        assert!(raw.contains("accept: text/event-stream"));
        assert!(!raw.contains("authorization:"));
    }
}
