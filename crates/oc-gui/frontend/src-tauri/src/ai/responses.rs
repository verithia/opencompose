//! OpenAI Responses adapter: input items, flat function tools, typed SSE and
//! `store: false` with reasoning items replayed from the canonical turn.
use super::auth::{AuthPlan, BodyEncoding};
use super::route::Route;
use super::sse::{self, SseDecoder, SseFrame};
use super::{
    Block, BoxFuture, Provider, ProviderError, ProviderErrorKind, ProviderRequest, Role,
    StopReason, StreamEvent, Turn, block_text, map_reqwest, map_status,
};
use crate::commands::AiProviderEntry;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Responses API provider adapter.
pub struct Responses {
    entry: AiProviderEntry,
    route: Route,
    auth: AuthPlan,
}

impl Responses {
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
        let mut body = json!({
            "model": self.entry.model,
            "input": input_items(request)?,
            "max_output_tokens": request.max_tokens,
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"]
        });
        if let Some(system) = &request.system {
            body["instructions"] = json!(system);
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
                        "parameters": tool.parameters,
                        "strict": false
                    }))
                    .collect::<Vec<_>>()
            );
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(true);
        }
        if reasoning_model(&self.entry, &self.entry.model) {
            body["reasoning"] = json!({
                "effort": super::effort::effort(&request.thinking_level),
                "summary": "auto"
            });
        } else {
            body["temperature"] = json!(request.temperature);
        }
        Ok(body)
    }
}

/// Reasoning models take an effort and a summary instead of sampling. The
/// catalog decides for a known id; the family rule is the fallback for one it
/// does not carry.
fn reasoning_model(entry: &AiProviderEntry, model: &str) -> bool {
    super::catalog::reasoning(entry.wire(), model).unwrap_or_else(|| {
        ["gpt-5", "gpt-6", "o1", "o3", "o4"]
            .iter()
            .any(|prefix| model.starts_with(prefix))
    })
}

fn input_items(request: &ProviderRequest) -> Result<Value, ProviderError> {
    let mut items = Vec::new();
    for turn in &request.turns {
        match turn.role {
            Role::User => items.push(json!({
                "type": "message",
                "role": "user",
                "content": user_content(turn)?
            })),
            Role::Assistant => {
                if let Some(Value::Array(continuity)) = &turn.continuity {
                    items.extend(continuity.iter().cloned());
                }
                let text = block_text(&turn.blocks);
                if !text.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}]
                    }));
                }
                for block in &turn.blocks {
                    if let Block::ToolUse {
                        id, name, input, ..
                    } = block
                    {
                        items.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": input.to_string()
                        }));
                    }
                }
            }
            Role::Tool => {
                for block in &turn.blocks {
                    if let Block::ToolResult {
                        call_id, content, ..
                    } = block
                    {
                        items.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": content
                        }));
                    }
                }
            }
            Role::System => {}
        }
    }
    Ok(json!(items))
}

/// Responses content parts: `input_text` plus `input_audio` when a clip rides
/// with the prompt.
fn user_content(turn: &Turn) -> Result<Value, ProviderError> {
    let mut content = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text(text) if !text.is_empty() => {
                content.push(json!({"type": "input_text", "text": text}));
            }
            Block::Media { .. } => {
                let (kind, mime, data) = super::media::inline_media(block)?;
                if kind == super::media::MediaKind::Audio {
                    let format = audio_format(block)?;
                    content.push(json!({
                        "type": "input_audio",
                        "input_audio": {"data": data, "format": format}
                    }));
                } else {
                    content.push(json!({
                        "type": "input_image",
                        "image_url": format!("data:{mime};base64,{data}")
                    }));
                }
            }
            _ => {}
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "input_text", "text": ""}));
    }
    Ok(Value::Array(content))
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
            "OpenAI Responses accepts wav and mp3 audio, not {name}."
        )))
    }
}

#[derive(Debug, Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
struct State {
    text: String,
    calls: BTreeMap<String, Call>,
    reasoning: Vec<Value>,
    reasoning_ids: BTreeSet<String>,
    terminal: bool,
    stop: StopReason,
    request_id: Option<String>,
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
            && let Some(id) = event["response"]["id"].as_str().filter(|id| !id.is_empty())
        {
            self.request_id = Some(id.to_owned());
            sink(StreamEvent::RequestId(id.to_owned()));
        }
        let kind = event["type"]
            .as_str()
            .or(frame.event.as_deref())
            .unwrap_or_default();
        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = event["delta"].as_str() {
                    self.text.push_str(delta);
                    sink(StreamEvent::TextDelta(delta.to_owned()));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = event["delta"].as_str() {
                    sink(StreamEvent::ThinkingDelta(delta.to_owned()));
                }
            }
            "response.output_item.added" => {
                let item = &event["item"];
                self.add_call(item);
                if item["type"] == "function_call"
                    && let Some(name) = item["name"].as_str().filter(|name| !name.is_empty())
                {
                    sink(StreamEvent::ToolPreparing(name.to_owned()));
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(item_id), Some(delta)) =
                    (event["item_id"].as_str(), event["delta"].as_str())
                {
                    self.calls
                        .entry(item_id.to_owned())
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                if let (Some(item_id), Some(arguments)) =
                    (event["item_id"].as_str(), event["arguments"].as_str())
                {
                    self.calls.entry(item_id.to_owned()).or_default().arguments =
                        arguments.to_owned();
                }
            }
            "response.output_item.done" => {
                let item = &event["item"];
                if item["type"] == "function_call" {
                    self.add_call(item);
                    if let Some(arguments) = item["arguments"].as_str() {
                        let call = self
                            .calls
                            .entry(item["id"].as_str().unwrap_or_default().to_owned())
                            .or_default();
                        call.arguments = arguments.to_owned();
                    }
                } else if item["type"] == "reasoning" {
                    self.capture_reasoning(item);
                }
            }
            "response.completed" => {
                self.apply_response(&event["response"]);
                self.terminal = true;
                self.stop = if self.calls.is_empty() {
                    StopReason::EndTurn
                } else {
                    StopReason::ToolUse
                };
                if let Some(usage) = event["response"].get("usage") {
                    sink(usage_event(usage));
                }
            }
            "response.incomplete" => {
                if let Some(usage) = event["response"].get("usage") {
                    sink(usage_event(usage));
                }
                return Err(ProviderError::truncated(
                    "Completion truncated. Increase output budget or request a smaller edit.",
                ));
            }
            "response.failed" => {
                self.terminal = true;
                return Err(ProviderError::protocol(
                    "Provider reported a failed response.",
                ));
            }
            "error" => return Err(streaming_error(&event)),
            _ => {}
        }
        Ok(())
    }

    fn add_call(&mut self, item: &Value) {
        if item["type"] != "function_call" {
            return;
        }
        let key = item["id"]
            .as_str()
            .or_else(|| item["call_id"].as_str())
            .unwrap_or_default();
        if key.is_empty() {
            return;
        }
        let call = self.calls.entry(key.to_owned()).or_default();
        if let Some(call_id) = item["call_id"].as_str() {
            call.id = call_id.to_owned();
        }
        if let Some(name) = item["name"].as_str() {
            call.name = name.to_owned();
        }
    }

    fn capture_reasoning(&mut self, item: &Value) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        let Some(encrypted) = item["encrypted_content"].as_str() else {
            return;
        };
        if !self.reasoning_ids.insert(id.to_owned()) {
            return;
        }
        let mut reasoning = json!({
            "type": "reasoning",
            "id": id,
            "encrypted_content": encrypted
        });
        if let Some(summary) = item.get("summary") {
            reasoning["summary"] = summary.clone();
        }
        self.reasoning.push(reasoning);
    }

    fn apply_response(&mut self, response: &Value) {
        let Some(output) = response["output"].as_array() else {
            return;
        };
        let mut text = String::new();
        let mut calls = BTreeMap::new();
        let mut found = false;
        for item in output {
            match item["type"].as_str() {
                Some("message") => {
                    found = true;
                    if let Some(content) = item["content"].as_array() {
                        for part in content {
                            if part["type"] == "output_text"
                                && let Some(part) = part["text"].as_str()
                            {
                                text.push_str(part);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    found = true;
                    let key = item["id"]
                        .as_str()
                        .or_else(|| item["call_id"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    calls.insert(
                        key,
                        Call {
                            id: item["call_id"].as_str().unwrap_or_default().to_owned(),
                            name: item["name"].as_str().unwrap_or_default().to_owned(),
                            arguments: item["arguments"].as_str().unwrap_or_default().to_owned(),
                        },
                    );
                }
                Some("reasoning") => self.capture_reasoning(item),
                _ => {}
            }
        }
        if found {
            self.text = text;
            self.calls = calls;
        }
    }

    fn turn(self) -> Result<Turn, ProviderError> {
        if !self.terminal {
            return Err(ProviderError::protocol(
                "Streaming response ended prematurely; nothing was applied",
            ));
        }
        let mut blocks = Vec::new();
        if !self.text.is_empty() {
            blocks.push(Block::Text(self.text));
        }
        for call in self.calls.into_values() {
            if call.id.is_empty() {
                continue;
            }
            let input = serde_json::from_str(&call.arguments).map_err(|_| {
                ProviderError::unusable("Provider returned malformed tool arguments")
            })?;
            blocks.push(Block::ToolUse {
                id: call.id,
                name: call.name,
                input,
                extra: None,
            });
        }
        Ok(Turn {
            role: Role::Assistant,
            blocks,
            continuity: (!self.reasoning.is_empty()).then(|| json!(self.reasoning)),
            extra: None,
        })
    }
}

fn streaming_error(event: &Value) -> ProviderError {
    let code = event["error"]["type"]
        .as_str()
        .or_else(|| event["error"]["code"].as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if code.contains("auth") {
        ProviderErrorKind::Auth
    } else if code.contains("rate") {
        ProviderErrorKind::RateLimit
    } else {
        ProviderErrorKind::Protocol
    };
    ProviderError::new(kind, "Provider returned a streaming error.")
}

fn usage_event(usage: &Value) -> StreamEvent {
    StreamEvent::Usage {
        input: usage["input_tokens"].as_u64().unwrap_or(0),
        output: usage["output_tokens"].as_u64().unwrap_or(0),
        reasoning: usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0),
        cached: usage["input_tokens_details"]["cached_tokens"].as_u64(),
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
        let stop = self.state.stop.clone();
        let turn = self.state.turn()?;
        super::tool_use_events(&turn, sink);
        sink(StreamEvent::Done { reason: stop });
        Ok(turn)
    }
}

impl Provider for Responses {
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
            let builder = match self.auth.body_encoding() {
                BodyEncoding::Plain => client.post(url).json(&body),
                BodyEncoding::Zstd => {
                    let bytes = serde_json::to_vec(&body)
                        .map_err(|_| ProviderError::protocol("Could not encode request body"))?;
                    let compressed = zstd::stream::encode_all(bytes.as_slice(), 3)
                        .map_err(|_| ProviderError::protocol("Could not compress request body"))?;
                    client
                        .post(url)
                        .header("content-type", "application/json")
                        .header("content-encoding", "zstd")
                        .body(compressed)
                }
            };
            let mut http_request = builder
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
        let body = Responses::for_test(
            entry("https://example.test/v1/responses".into()),
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

    const TEXT: &str = include_str!("../../tests/fixtures/openai_responses_text.sse");
    const TOOLS: &str = include_str!("../../tests/fixtures/openai_responses_tools.sse");

    fn entry(endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "openai".into(),
            name: "OpenAI".into(),
            kind: crate::commands::ProviderKind::OpenAiResponses,
            wire: Some(Wire::OpenAiResponses),
            auth: crate::commands::AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "gpt-6.1-sol".into(),
            models: Vec::new(),
            context_length: 128000,
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
                {"role":"user","content":"Read track:bass"}
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
        for chunk in fixture.as_bytes().chunks(7) {
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
        let frame = r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item-a","call_id":"call-a","name":"edit_subtree","arguments":""}}"#;
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
    fn text_fixture_maps_to_canonical_turn_and_usage() {
        let (turn, events) = feed(TEXT).unwrap();
        assert_eq!(turn.blocks, vec![Block::Text("Hello!".into())]);
        assert!(turn.continuity.is_none());
        assert!(events.contains(&StreamEvent::TextDelta("Hello".into())));
        assert!(events.contains(&StreamEvent::Usage {
            input: 12,
            output: 7,
            reasoning: 3,
            cached: Some(2),
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
    }

    #[test]
    fn tool_fixture_maps_arguments_and_replays_reasoning() {
        let (turn, events) = feed(TOOLS).unwrap();
        assert_eq!(
            turn.blocks,
            vec![Block::ToolUse {
                id: "call_1".into(),
                name: "read_context".into(),
                input: json!({"path":"track:bass"}),
                extra: None,
            }]
        );
        let continuity = turn.continuity.unwrap();
        assert_eq!(continuity[0]["type"], "reasoning");
        assert_eq!(continuity[0]["encrypted_content"], "encrypted-reasoning");
        assert!(!events.is_empty());
    }

    #[test]
    fn request_uses_items_flat_tools_and_store_false() {
        let body = Responses::for_test(
            entry("https://example.test/v1/responses".into()),
            AuthPlan::default(),
        )
        .body(&request())
        .unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 4096);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_context");
        assert_eq!(body["tools"][0]["parameters"]["type"], "object");
        assert_eq!(body["instructions"], "You are OpenCompose Assistant.");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert!(body["messages"].is_null());
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    /// The saved level reaches the Responses body; the selector is not cosmetic.
    #[test]
    fn the_saved_thinking_level_reaches_the_reasoning_block() {
        let entry = entry("https://api.openai.test/v1/responses".into());
        let provider = Responses::for_test(entry, AuthPlan::default());
        for level in ["low", "medium", "high"] {
            let mut request = request();
            request.thinking_level = level.into();
            let body = provider.body(&request).unwrap();
            assert_eq!(body["reasoning"]["effort"], level, "{body}");
        }
    }

    #[test]
    fn malformed_chunk_error_event_and_exit_without_terminal_fail() {
        let error = feed("data: {\"type\":\n\n").unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        let error = feed(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"auth_error\",\"message\":\"do not echo\"}}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("do not echo"));
        let error = feed(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        let truncated = feed(
            "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
        )
        .unwrap_err();
        assert_eq!(truncated.kind, ProviderErrorKind::Truncated);
    }

    #[tokio::test]
    async fn auth_failure_is_distinct_and_headers_are_sent_without_leaking() {
        let (base, server) = canned(401, "fixture-secret must never escape");
        let entry = entry(format!("{base}v1/responses"));
        let plan = crate::ai::auth::plan(
            &entry,
            &crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
        )
        .unwrap();
        let provider = Responses::for_test(entry, plan);
        let mut sink = |_: StreamEvent| {};
        let error = provider.stream(&request(), &mut sink).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("fixture-secret"));
        let raw = server.join().unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("post /v1/responses "), "{raw}");
        assert!(raw.contains("authorization: bearer fixture-secret"));
        assert!(!raw.contains("x-api-key"));
    }

    #[tokio::test]
    async fn chatgpt_oauth_sends_account_header_and_zstd_body() {
        use crate::ai::auth::{Credential, TokenSet, plan};
        use crate::commands::{AuthKind, AuthProvider};
        let (base, server) = crate::ai::test_support::canned_bytes(401, b"denied".to_vec());
        let mut entry = entry(format!("{base}v1/responses"));
        entry.auth = AuthKind::OAuth;
        entry.auth_provider = Some(AuthProvider::OpenAiChatGpt);
        entry.oauth_allowed_hosts = vec!["127.0.0.1".into()];
        let tokens = TokenSet {
            v: 1,
            access: "access-secret".into(),
            refresh: "refresh-secret".into(),
            expires_at: crate::ai::oauth::now_ms() + 3_600_000,
            account_id: Some("acct-1".into()),
            account_label: None,
            scope: None,
            client_id: None,
        };
        let plan = plan(&entry, &Credential::OAuth(tokens)).unwrap();
        let provider = Responses::for_test(entry, plan);
        let mut sink = |_: StreamEvent| {};
        let error = provider.stream(&request(), &mut sink).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        let raw = server.join().unwrap();
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(text.contains("content-encoding: zstd"), "{text}");
        assert!(text.contains("chatgpt-account-id: acct-1"));
        assert!(text.contains("authorization: bearer access-secret"));
        assert!(!text.contains("refresh-secret"));
        let end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let decoded = zstd::decode_all(&raw[end + 4..]).unwrap();
        let json: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(json["model"], "gpt-6.1-sol");
        assert_eq!(json["stream"], true);
    }
}
