//! Gemini `generateContent` adapter: `contents`/`parts`, `functionDeclarations`,
//! `thinkingConfig`, SSE usage normalization, continuation and thought-signature
//! replay. The route supplies the streamGenerateContent URL; the model id is
//! the only part this adapter contributes to it.
use super::auth::AuthPlan;
use super::route::Route;
use super::sse::{self, SseFrame};
use super::{
    Block, BoxFuture, Provider, ProviderError, ProviderErrorKind, ProviderRequest, Role,
    StopReason, StreamEvent, Turn, map_reqwest, map_status,
};
use crate::commands::AiProviderEntry;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A response split across requests is still one answer; this bounds a gateway
/// that keeps returning tokens without finishing.
const MAX_CONTINUATIONS: usize = 8;
/// Google's documented sentinel for replaying a function call whose original
/// thought signature is gone (an older turn or another provider's history).
const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";
const CONTINUATION: &str = "CONTINUATION";

static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

/// Gemini generate-content adapter.
pub struct GeminiGenerate {
    entry: AiProviderEntry,
    route: Route,
    auth: AuthPlan,
}

impl GeminiGenerate {
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
        let sampling = super::catalog::model(self.entry.wire(), &self.entry.model)
            .map(|model| model.temperature)
            .unwrap_or(true);
        let mut generation = json!({"maxOutputTokens": request.max_tokens});
        // Gemini 3+/4 use `thinkingLevel` rather than a token budget. Models the
        // catalog gives no levels take no thinking config at all, so the saved
        // default is never sent as a guess.
        if !super::catalog::thinking_levels(self.entry.wire(), &self.entry.model).is_empty() {
            generation["thinkingConfig"] = json!({
                "thinkingLevel": request.thinking_level,
                "includeThoughts": true
            });
        }
        if sampling {
            generation["temperature"] = json!(request.temperature);
        }
        let mut body = json!({
            "contents": contents(request, &self.entry.model)?,
            "generationConfig": generation
        });
        if let Some(system) = &request.system {
            body["systemInstruction"] = json!({"parts": [{"text": system}]});
        }
        if !request.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parametersJsonSchema": tool.parameters
                    }))
                    .collect::<Vec<_>>()
            }]);
            body["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}});
        }
        Ok(body)
    }
}

pub(super) fn contents(request: &ProviderRequest, model: &str) -> Result<Value, ProviderError> {
    let omit_ids = omits_function_call_ids(model);
    let mut contents = Vec::new();
    for turn in &request.turns {
        let parts = match turn.role {
            Role::System => continue,
            Role::User => user_parts(turn)?,
            Role::Assistant => assistant_parts(turn, model, omit_ids),
            Role::Tool => tool_parts(turn, omit_ids),
        };
        if !parts.is_empty() {
            push_content(&mut contents, turn.role, parts);
        }
    }
    Ok(Value::Array(contents))
}

fn user_parts(turn: &Turn) -> Result<Vec<Value>, ProviderError> {
    let mut parts = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text(text) if !text.is_empty() => parts.push(json!({"text": text})),
            Block::Media { .. } => {
                let (_, mime, data) = super::media::inline_media(block)?;
                parts.push(json!({"inlineData": {"mimeType": mime, "data": data}}));
            }
            _ => {}
        }
    }
    Ok(parts)
}

/// Gemini requires every response to one parallel call batch in a single user
/// turn, so consecutive tool turns merge into the open function-response turn.
fn push_content(contents: &mut Vec<Value>, role: Role, parts: Vec<Value>) {
    let merges = role == Role::Tool
        && contents.last().is_some_and(|last| {
            last["role"] == "user"
                && last["parts"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part.get("functionResponse").is_some())
                })
        });
    if merges {
        if let Some(existing) = contents
            .last_mut()
            .and_then(|last| last["parts"].as_array_mut())
        {
            existing.extend(parts);
        }
        return;
    }
    let role = if role == Role::Assistant {
        "model"
    } else {
        "user"
    };
    contents.push(json!({"role": role, "parts": parts}));
}

fn assistant_parts(turn: &Turn, model: &str, omit_ids: bool) -> Vec<Value> {
    let signatures = signatures(turn);
    let fallback = requires_signature_fallback(model);
    let mut has_signed_call = false;
    let mut parts = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text(text) if !text.is_empty() => {
                let mut part = json!({"text": text});
                if let Some(signature) = signatures.get("text") {
                    part["thoughtSignature"] = signature.clone();
                }
                parts.push(part);
            }
            Block::Thinking {
                text, signature, ..
            } => {
                if text.is_empty() {
                    continue;
                }
                let mut part = json!({"text": text, "thought": true});
                if let Some(signature) = signature {
                    part["thoughtSignature"] = json!(signature);
                }
                parts.push(part);
            }
            Block::ToolUse {
                id, name, input, ..
            } => {
                let signature = signatures
                    .get(&format!("tool_use:{id}"))
                    .and_then(Value::as_str);
                let mut call = json!({"name": name, "args": input});
                if !omit_ids {
                    call["id"] = json!(id);
                }
                let mut part = json!({"functionCall": call});
                if let Some(signature) = signature {
                    has_signed_call = true;
                    part["thoughtSignature"] = json!(signature);
                } else if fallback && !has_signed_call {
                    // A signature may be attached to one call of a parallel
                    // batch. Until that call is seen, replay needs the sentinel.
                    part["thoughtSignature"] = json!(SKIP_THOUGHT_SIGNATURE_VALIDATOR);
                }
                parts.push(part);
            }
            _ => {}
        }
    }
    parts
}

fn tool_parts(turn: &Turn, omit_ids: bool) -> Vec<Value> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            Block::ToolResult {
                call_id,
                name,
                content,
                ..
            } => {
                let mut response = json!({
                    "name": name,
                    "response": {"name": name, "content": content}
                });
                if !omit_ids {
                    response["id"] = json!(call_id);
                }
                Some(json!({"functionResponse": response}))
            }
            _ => None,
        })
        .collect()
}

fn signatures(turn: &Turn) -> serde_json::Map<String, Value> {
    turn.continuity
        .as_ref()
        .and_then(|continuity| continuity.get("thought_signatures"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Gemini 1.x/2.x ignore or reject explicit function call ids.
fn omits_function_call_ids(model: &str) -> bool {
    let model = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let Some(rest) = model
        .strip_prefix("gemini-live-")
        .or_else(|| model.strip_prefix("gemini-"))
    else {
        return false;
    };
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse::<u32>().is_ok_and(|version| version < 3)
}

/// Gemini 3+ rejects a replayed function call without a thought signature.
/// Unknown aliases inherit the current generation's behaviour.
fn requires_signature_fallback(model: &str) -> bool {
    let model = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let Some(rest) = model.strip_prefix("gemini-") else {
        return false;
    };
    if rest == "pro" || rest == "pro-vision" || rest.starts_with("robotics-er-1.5") {
        return false;
    }
    !(rest == "1" || rest == "2" || rest.starts_with("1.") || rest.starts_with("2."))
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Usage {
    prompt: u64,
    cached: Option<u64>,
    candidates: u64,
    thoughts: u64,
}

impl Usage {
    pub(super) fn event(self) -> StreamEvent {
        StreamEvent::Usage {
            input: self.prompt,
            cached: self.cached,
            output: self.candidates + self.thoughts,
            reasoning: self.thoughts,
        }
    }
}

/// Usage is cumulative within one request; a continuation is a new request that
/// re-bills the output so far as input. Report the first request's input plus
/// the summed output, which keeps input + output equal to the final context.
#[derive(Debug, Default)]
struct Totals {
    prompt: u64,
    cached: Option<u64>,
    candidates: u64,
    thoughts: u64,
    requests: usize,
}

impl Totals {
    fn fold(&mut self, usage: Option<Usage>) {
        let Some(usage) = usage else { return };
        if self.requests == 0 {
            self.prompt = usage.prompt;
            self.cached = usage.cached;
        }
        self.candidates += usage.candidates;
        self.thoughts += usage.thoughts;
        self.requests += 1;
    }

    fn event(&self) -> Option<StreamEvent> {
        (self.requests > 0).then(|| {
            Usage {
                prompt: self.prompt,
                cached: self.cached,
                candidates: self.candidates,
                thoughts: self.thoughts,
            }
            .event()
        })
    }
}

#[derive(Debug)]
enum Open {
    Text {
        text: String,
        signature: Option<String>,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

#[derive(Debug, Default)]
struct State {
    blocks: Vec<Block>,
    open: Option<Open>,
    signatures: BTreeMap<String, String>,
    usage: Option<Usage>,
    finish_reason: Option<String>,
    prompt_block: Option<String>,
    continuation: Option<String>,
    has_tool_calls: bool,
    started: bool,
}

impl State {
    fn consume(
        &mut self,
        frame: SseFrame,
        sink: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), ProviderError> {
        let event: Value = serde_json::from_str(&frame.data)
            .map_err(|_| ProviderError::protocol("Invalid streaming JSON"))?;
        self.started = true;
        if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
            return Err(streaming_error(error));
        }
        if let Some(reason) = event["promptFeedback"]["blockReason"].as_str() {
            self.prompt_block = Some(reason.to_owned());
        }
        if let Some(usage) = usage_counts(&event["usageMetadata"]) {
            self.usage = Some(usage);
        }
        let Some(candidate) = event["candidates"]
            .as_array()
            .and_then(|candidates| candidates.first())
        else {
            return Ok(());
        };
        if let Some(reason) = candidate["finishReason"].as_str() {
            if maps_to_error(reason) {
                return Err(ProviderError::protocol(format!(
                    "Gemini stopped with {reason}"
                )));
            }
            self.finish_reason = Some(reason.to_owned());
        }
        if let Some(token) = candidate["continuationToken"].as_str() {
            self.continuation = Some(token.to_owned());
        } else if candidate["finishReason"].as_str().is_some() {
            self.continuation = None;
        }
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                self.part(part, sink)?;
            }
        }
        Ok(())
    }

    fn part(
        &mut self,
        part: &Value,
        sink: &mut dyn FnMut(StreamEvent),
    ) -> Result<(), ProviderError> {
        if let Some(call) = part.get("functionCall").filter(|call| !call.is_null()) {
            self.flush();
            let name = call["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    ProviderError::protocol("Gemini returned a tool call without a name")
                })?;
            let id = call["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(next_call_id);
            let input = call
                .get("args")
                .filter(|args| !args.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Some(signature) = part["thoughtSignature"].as_str() {
                self.signatures
                    .insert(format!("tool_use:{id}"), signature.to_owned());
            }
            self.has_tool_calls = true;
            sink(StreamEvent::ToolPreparing(name.to_owned()));
            sink(StreamEvent::ToolCall {
                id: id.clone(),
                name: name.to_owned(),
                input: input.clone(),
            });
            self.blocks.push(Block::ToolUse {
                id,
                name: name.to_owned(),
                input,
                extra: None,
            });
            return Ok(());
        }
        let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) else {
            return Ok(());
        };
        let thought = part["thought"].as_bool() == Some(true);
        self.push_text(thought, text, part["thoughtSignature"].as_str());
        sink(if thought {
            StreamEvent::ThinkingDelta(text.to_owned())
        } else {
            StreamEvent::TextDelta(text.to_owned())
        });
        Ok(())
    }

    fn push_text(&mut self, thought: bool, text: &str, signature: Option<&str>) {
        let same = matches!(
            (&self.open, thought),
            (Some(Open::Text { .. }), false) | (Some(Open::Thinking { .. }), true)
        );
        if !same {
            self.flush();
            self.open = Some(if thought {
                Open::Thinking {
                    text: String::new(),
                    signature: None,
                }
            } else {
                Open::Text {
                    text: String::new(),
                    signature: None,
                }
            });
        }
        if let Some(
            Open::Text {
                text: buffer,
                signature: held,
            }
            | Open::Thinking {
                text: buffer,
                signature: held,
            },
        ) = self.open.as_mut()
        {
            buffer.push_str(text);
            if let Some(signature) = signature {
                *held = Some(signature.to_owned());
            }
        }
    }

    fn flush(&mut self) {
        match self.open.take() {
            Some(Open::Text { text, signature }) => {
                if let Some(signature) = signature {
                    self.signatures.insert("text".into(), signature);
                }
                if !text.is_empty() {
                    self.blocks.push(Block::Text(text));
                }
            }
            Some(Open::Thinking { text, signature }) if !text.is_empty() => {
                if let Some(signature) = &signature {
                    self.signatures.insert("thinking".into(), signature.clone());
                }
                self.blocks.push(Block::Thinking {
                    text,
                    signature,
                    redacted: None,
                });
            }
            Some(Open::Thinking { .. }) | None => {}
        }
    }

    /// A continuation is a new request: usage restarts, and so does the terminal
    /// finish reason, while blocks and signatures accumulate.
    fn next_request(&mut self) {
        self.finish_reason = None;
        self.prompt_block = None;
        self.usage = None;
        self.continuation = None;
    }

    fn stop_reason(&self) -> StopReason {
        let reason = self
            .finish_reason
            .as_deref()
            .or(self.prompt_block.as_deref());
        match reason {
            Some(reason) if content_filtered(reason) => StopReason::Other("content-filter".into()),
            Some("MAX_TOKENS") => StopReason::MaxTokens,
            Some("STOP") | None => {
                if self.has_tool_calls {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
            Some(reason) => StopReason::Other(reason.to_owned()),
        }
    }

    fn into_turn(mut self, terminal: bool) -> Result<Turn, ProviderError> {
        self.flush();
        if !terminal {
            return Err(ProviderError::protocol(
                "Streaming response ended prematurely; nothing was applied",
            ));
        }
        let mut continuity = serde_json::Map::new();
        if !self.signatures.is_empty() {
            continuity.insert("thought_signatures".into(), json!(self.signatures));
        }
        Ok(Turn {
            role: Role::Assistant,
            blocks: self.blocks,
            continuity: (!continuity.is_empty()).then_some(Value::Object(continuity)),
            extra: None,
        })
    }
}

pub(super) fn next_call_id() -> String {
    format!(
        "gemini_call_{}",
        NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) fn usage_counts(metadata: &Value) -> Option<Usage> {
    metadata
        .as_object()
        .filter(|object| !object.is_empty())
        .map(|_| Usage {
            prompt: metadata["promptTokenCount"].as_u64().unwrap_or(0),
            cached: metadata["cachedContentTokenCount"].as_u64(),
            candidates: metadata["candidatesTokenCount"].as_u64().unwrap_or(0),
            thoughts: metadata["thoughtsTokenCount"].as_u64().unwrap_or(0),
        })
}

pub(super) fn content_filtered(reason: &str) -> bool {
    matches!(
        reason,
        "SAFETY"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
    )
}

pub(super) fn maps_to_error(reason: &str) -> bool {
    matches!(
        reason,
        "MALFORMED_FUNCTION_CALL"
            | "UNEXPECTED_TOOL_CALL"
            | "MALFORMED_RESPONSE"
            | "MISSING_THOUGHT_SIGNATURE"
            | "TOO_MANY_TOOL_CALLS"
            | "NO_IMAGE"
    )
}

pub(super) fn streaming_error(error: &Value) -> ProviderError {
    let code = error["status"]
        .as_str()
        .or_else(|| error["type"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    let lower = code.to_ascii_lowercase();
    let kind = if lower.contains("auth") || lower.contains("permission") {
        ProviderErrorKind::Auth
    } else if lower.contains("rate") || lower.contains("resource_exhausted") {
        ProviderErrorKind::RateLimit
    } else if lower.contains("unavailable") || lower.contains("internal") {
        ProviderErrorKind::Server
    } else {
        ProviderErrorKind::Protocol
    };
    ProviderError::new(
        kind,
        format!("Provider returned a streaming error ({code})."),
    )
}

impl Provider for GeminiGenerate {
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
            let mut state = State::default();
            let mut totals = Totals::default();
            let mut token: Option<String> = None;
            let mut continuations = 0usize;
            loop {
                let mut payload = body.clone();
                if let Some(token) = &token {
                    payload["continuationToken"] = json!(token);
                }
                let mut http_request = client
                    .post(url.clone())
                    .header("accept", "text/event-stream")
                    .json(&payload)
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
                {
                    let mut on_frame = |frame: SseFrame| state.consume(frame, sink);
                    sse::drive(&mut response, &mut on_frame).await?;
                }
                totals.fold(state.usage);
                let next = state.continuation.clone().filter(|_| {
                    state
                        .finish_reason
                        .as_deref()
                        .is_none_or(|reason| reason == CONTINUATION)
                });
                if let Some(next) = next {
                    if continuations >= MAX_CONTINUATIONS {
                        return Err(ProviderError::protocol(
                            "Gemini kept continuing the response; giving up after the limit.",
                        ));
                    }
                    continuations += 1;
                    token = Some(next);
                    state.next_request();
                    continue;
                }
                if state.finish_reason.as_deref() == Some(CONTINUATION) {
                    return Err(ProviderError::protocol(
                        "Gemini continued the response without a token",
                    ));
                }
                let terminal = state.started
                    && (state.finish_reason.is_some() || state.prompt_block.is_some());
                let reason = state.stop_reason();
                let turn = state.into_turn(terminal)?;
                if let Some(usage) = totals.event() {
                    sink(usage);
                }
                sink(StreamEvent::Done { reason });
                return Ok(turn);
            }
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
        let body = provider(entry("https://example.test/v1beta".into()))
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
    use crate::ai::auth::{self, Credential};
    use crate::ai::sse::{SseDecoder, SseFrame};
    use crate::ai::test_support::{canned, canned_sequence};
    use crate::ai::{ToolSpec, turns_from_wire};
    use crate::commands::{AiCapabilities, AuthKind, ProviderKind, Wire};
    use std::sync::{Arc, Mutex};

    const TEXT: &str = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]}}]}

data: {"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":12,"cachedContentTokenCount":4,"candidatesTokenCount":7,"thoughtsTokenCount":3,"totalTokenCount":22}}

"#;

    const TOOLS: &str = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Reading.","thought":true,"thoughtSignature":"sig-thought"},{"functionCall":{"id":"call_1","name":"read_context","args":{"path":"track:bass"}},"thoughtSignature":"sig-call"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":120,"cachedContentTokenCount":20,"candidatesTokenCount":15,"thoughtsTokenCount":30}}

"#;

    const FIRST_HALF: &str = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]},"finishReason":"CONTINUATION","continuationToken":"tok-2"}],"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":10,"candidatesTokenCount":10,"thoughtsTokenCount":5}}

"#;

    const SECOND_HALF: &str = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":110,"cachedContentTokenCount":10,"candidatesTokenCount":4,"thoughtsTokenCount":2}}

"#;

    fn entry(endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "opencode-go".into(),
            name: "OpenCode Go".into(),
            kind: ProviderKind::GeminiGenerate,
            wire: Some(Wire::GeminiGenerate),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "gemini-4-argon".into(),
            models: Vec::new(),
            context_length: 1_048_576,
            max_tokens: 65_536,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                audio_in: true,
                audio_out: false,
                native_tool_calling: true,
                streaming: true,
            },
        }
    }

    fn provider(entry: AiProviderEntry) -> GeminiGenerate {
        let plan = auth::plan(&entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
        GeminiGenerate::for_test(entry, plan)
    }

    fn provider_at(base: &str) -> GeminiGenerate {
        provider(entry(format!("{base}v1beta")))
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
            max_tokens: 65_536,
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: true,
        }
    }

    fn feed(fixture: &str) -> Result<(Turn, Vec<StreamEvent>), ProviderError> {
        let mut state = State::default();
        let mut decoder = SseDecoder::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let collected = events.clone();
        let mut sink = move |event: StreamEvent| collected.lock().unwrap().push(event);
        {
            let mut on_frame = |frame: SseFrame| state.consume(frame, &mut sink);
            for chunk in fixture.as_bytes().chunks(9) {
                decoder.push(chunk, &mut on_frame)?;
            }
            decoder.finish(&mut on_frame)?;
        }
        state.flush();
        let terminal =
            state.started && (state.finish_reason.is_some() || state.prompt_block.is_some());
        let reason = state.stop_reason();
        let usage = state.usage;
        let turn = state.into_turn(terminal)?;
        if let Some(usage) = usage {
            sink(usage.event());
        }
        sink(StreamEvent::Done { reason });
        let events = events.lock().unwrap().clone();
        Ok((turn, events))
    }

    fn finish_frame(reason: &str) -> String {
        format!(
            "data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":\"x\"}}]}},\"finishReason\":\"{reason}\"}}]}}\n\n"
        )
    }

    fn request_body(raw: &str) -> Value {
        let body = raw.split("\r\n\r\n").nth(1).expect("request body");
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn request_lowers_contents_system_tools_and_generation_config() {
        let provider = provider(entry(
            "https://opencode.ai/inference/go/google/v1beta".into(),
        ));
        let body = provider.body(&request()).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are OpenCompose Assistant."
        );
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Read track:bass");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["name"],
            "read_context"
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["args"]["path"],
            "project"
        );
        assert_eq!(body["contents"][2]["role"], "user");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "read_context"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["content"],
            "@project"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "read_context"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "object"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 65_536);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "medium"
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
        assert!(
            body["generationConfig"].get("temperature").is_none(),
            "gemini-4-argon rejects sampling parameters"
        );
    }

    #[test]
    fn sampling_parameters_follow_the_catalog_per_model() {
        let argon = provider(entry(
            "https://opencode.ai/inference/go/google/v1beta".into(),
        ))
        .body(&request())
        .unwrap();
        assert!(argon["generationConfig"].get("temperature").is_none());
        let mut flash = entry("https://example.test/v1beta".into());
        flash.model = "gemini-3.8-flash".into();
        let flash = provider(flash).body(&request()).unwrap();
        assert_eq!(flash["generationConfig"]["temperature"], 0.3);
    }

    #[test]
    fn the_body_carries_the_requested_thinking_level() {
        let mut request = request();
        request.thinking_level = "low".into();
        let body = provider(entry(
            "https://opencode.ai/inference/go/google/v1beta".into(),
        ))
        .body(&request)
        .unwrap();
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn a_model_without_declared_levels_sends_no_thinking_config() {
        let mut entry = entry("https://example.test/v1beta".into());
        entry.model = "gemini-2.5-pro".into();
        let body = provider(entry).body(&request()).unwrap();
        assert!(
            body["generationConfig"].get("thinkingConfig").is_none(),
            "a model the catalog gives no levels never receives a guessed level"
        );
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 65_536);
    }

    #[test]
    fn replaying_an_unsigned_tool_call_inserts_the_thought_signature_sentinel() {
        let provider = provider(entry(
            "https://opencode.ai/inference/go/google/v1beta".into(),
        ));
        let body = provider.body(&request()).unwrap();
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE_VALIDATOR
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["id"],
            "call_0"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["id"],
            "call_0"
        );

        let mut signed = request();
        signed.turns[1].continuity =
            Some(json!({"thought_signatures": {"tool_use:call_0": "sig-call"}}));
        let body = provider.body(&signed).unwrap();
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            "sig-call"
        );
    }

    #[test]
    fn older_gemini_models_receive_no_call_ids_and_no_sentinel() {
        let mut entry = entry("https://generativelanguage.googleapis.com/v1beta".into());
        entry.model = "gemini-2.5-pro".into();
        let body = provider(entry).body(&request()).unwrap();
        assert!(
            body["contents"][1]["parts"][0]["functionCall"]
                .get("id")
                .is_none()
        );
        assert!(
            body["contents"][1]["parts"][0]
                .get("thoughtSignature")
                .is_none()
        );
        assert!(
            body["contents"][2]["parts"][0]["functionResponse"]
                .get("id")
                .is_none()
        );
    }

    #[test]
    fn parallel_tool_results_share_one_user_turn() {
        let mut request = request();
        request.turns = turns_from_wire(&json!([
            {"role":"user","content":"go"},
            {"role":"assistant","content":null,"tool_calls":[
                {"id":"c1","type":"function","function":{"name":"a","arguments":"{}"}},
                {"id":"c2","type":"function","function":{"name":"b","arguments":"{}"}}
            ]},
            {"role":"tool","tool_call_id":"c1","content":"one"},
            {"role":"tool","tool_call_id":"c2","content":"two"}
        ]))
        .unwrap();
        let partner = provider(entry("https://example.test/v1beta".into()));
        let body = partner.body(&request).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "a");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["name"], "b");
    }

    #[test]
    fn text_fixture_maps_to_one_block_and_normalized_usage() {
        let (turn, events) = feed(TEXT).unwrap();
        assert_eq!(turn.blocks, vec![Block::Text("Hello".into())]);
        assert!(events.contains(&StreamEvent::TextDelta("Hel".into())));
        assert!(events.contains(&StreamEvent::TextDelta("lo".into())));
        assert!(events.contains(&StreamEvent::Usage {
            input: 12,
            cached: Some(4),
            output: 10,
            reasoning: 3,
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
        assert!(turn.continuity.is_none());
    }

    #[test]
    fn tool_fixture_keeps_thinking_signature_and_call_arguments() {
        let (turn, events) = feed(TOOLS).unwrap();
        assert_eq!(
            turn.blocks,
            vec![
                Block::Thinking {
                    text: "Reading.".into(),
                    signature: Some("sig-thought".into()),
                    redacted: None,
                },
                Block::ToolUse {
                    id: "call_1".into(),
                    name: "read_context".into(),
                    input: json!({"path": "track:bass"}),
                    extra: None,
                },
            ]
        );
        assert!(events.contains(&StreamEvent::ThinkingDelta("Reading.".into())));
        assert!(events.contains(&StreamEvent::ToolPreparing("read_context".into())));
        assert!(events.contains(&StreamEvent::ToolCall {
            id: "call_1".into(),
            name: "read_context".into(),
            input: json!({"path": "track:bass"}),
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::ToolUse,
        }));
        let continuity = turn.continuity.unwrap();
        assert_eq!(
            continuity["thought_signatures"]["tool_use:call_1"],
            "sig-call"
        );
        assert_eq!(continuity["thought_signatures"]["thinking"], "sig-thought");
    }

    #[test]
    fn finish_reasons_map_to_canonical_stops_and_errors() {
        let (_, events) = feed(&finish_frame("STOP")).unwrap();
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
        let (_, events) = feed(&finish_frame("MAX_TOKENS")).unwrap();
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::MaxTokens,
        }));
        let (_, events) = feed(&finish_frame("SAFETY")).unwrap();
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::Other("content-filter".into()),
        }));
        for reason in [
            "MALFORMED_FUNCTION_CALL",
            "MISSING_THOUGHT_SIGNATURE",
            "MALFORMED_RESPONSE",
        ] {
            assert_eq!(
                feed(&finish_frame(reason)).unwrap_err().kind,
                ProviderErrorKind::Protocol,
                "{reason}"
            );
        }
    }

    #[test]
    fn prompt_feedback_blocks_map_to_content_filter() {
        let (_, events) = feed(r#"data: {"promptFeedback":{"blockReason":"SAFETY"}}"#).unwrap();
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::Other("content-filter".into()),
        }));
    }

    #[test]
    fn streaming_errors_are_classified_without_echoing_the_provider_message() {
        let error =
            feed(r#"data: {"error":{"status":"INVALID_ARGUMENT","message":"do not echo"}}"#)
                .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        assert!(!error.message.contains("do not echo"));
        let error = feed(r#"data: {"error":{"status":"UNAUTHENTICATED","message":"do not echo"}}"#)
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
    }

    #[test]
    fn a_stream_without_a_terminal_finish_is_an_error() {
        let error = feed(r#"data: {"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#)
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
    }

    #[test]
    fn gemini_generation_rules_follow_the_model_id() {
        assert!(requires_signature_fallback("gemini-4-argon"));
        assert!(requires_signature_fallback("gemini-3.8-flash"));
        assert!(!requires_signature_fallback("gemini-2.5-pro"));
        assert!(!requires_signature_fallback("gemini-pro"));
        assert!(!requires_signature_fallback("gemini-pro-vision"));
        assert!(!requires_signature_fallback(
            "gemini-robotics-er-1.5-preview"
        ));
        assert!(omits_function_call_ids("gemini-2.5-pro"));
        assert!(omits_function_call_ids("gemini-1.5-flash"));
        assert!(omits_function_call_ids("gemini-live-2.5-flash"));
        assert!(!omits_function_call_ids("gemini-4-argon"));
        assert!(!omits_function_call_ids("gemini-3.8-flash"));
    }

    #[tokio::test]
    async fn fake_gateway_receives_the_generate_content_path_query_and_bearer_token() {
        let (base, server) = canned(200, TEXT);
        let mut sink = |_: StreamEvent| {};
        let turn = provider_at(&base)
            .stream(&request(), &mut sink)
            .await
            .unwrap();
        assert_eq!(turn.blocks, vec![Block::Text("Hello".into())]);
        let raw = server.join().unwrap();
        let lower = raw.to_ascii_lowercase();
        assert!(
            lower.contains(
                "post /v1beta/models/gemini-4-argon:streamgeneratecontent?alt=sse http/1.1"
            ),
            "{lower}"
        );
        assert!(
            lower.contains("authorization: bearer fixture-secret"),
            "{lower}"
        );
        assert!(lower.contains("accept: text/event-stream"), "{lower}");
        assert!(!lower.contains("x-goog-api-key"), "{lower}");
        let body = request_body(&raw);
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are OpenCompose Assistant."
        );
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Read track:bass");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "read_context"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "medium"
        );
        assert!(body["generationConfig"].get("temperature").is_none());
    }

    #[tokio::test]
    async fn a_continuation_token_is_replayed_in_a_second_request() {
        let (base, server) = canned_sequence(vec![(200, FIRST_HALF), (200, SECOND_HALF)]);
        let mut events = Vec::new();
        let mut sink = |event: StreamEvent| events.push(event);
        let turn = provider_at(&base)
            .stream(&request(), &mut sink)
            .await
            .unwrap();
        assert_eq!(turn.blocks, vec![Block::Text("Hello".into())]);
        assert!(events.contains(&StreamEvent::Usage {
            input: 100,
            cached: Some(10),
            output: 21,
            reasoning: 7,
        }));
        assert!(events.contains(&StreamEvent::Done {
            reason: StopReason::EndTurn,
        }));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        let first = request_body(&requests[0]);
        let second = request_body(&requests[1]);
        assert!(first.get("continuationToken").is_none());
        assert_eq!(second["continuationToken"], "tok-2");
        assert_eq!(first["contents"], second["contents"]);
        assert_eq!(first["generationConfig"], second["generationConfig"]);
        assert_eq!(first["tools"], second["tools"]);
    }

    #[tokio::test]
    async fn a_continuation_finish_without_a_token_is_an_error() {
        let proxy = r#"data: {"candidates":[{"content":{"parts":[{"text":"partial"}]},"finishReason":"CONTINUATION"}]}

"#;
        let (base, server) = canned(200, proxy);
        let mut sink = |_: StreamEvent| {};
        let error = provider_at(&base)
            .stream(&request(), &mut sink)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Protocol);
        assert!(error.message.contains("without a token"));
        assert_eq!(
            server
                .join()
                .unwrap()
                .to_ascii_lowercase()
                .matches("post ")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn auth_failure_is_distinct_and_headers_are_sent_without_leaking() {
        let (base, server) = canned(401, "fixture-secret must never escape");
        let mut sink = |_: StreamEvent| {};
        let error = provider_at(&base)
            .stream(&request(), &mut sink)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("fixture-secret"));
        let raw = server.join().unwrap().to_ascii_lowercase();
        assert!(
            raw.contains("authorization: bearer fixture-secret"),
            "{raw}"
        );
    }

    /// Live gateway smoke test. Ignored by default because it requires an API token
    /// and network access:
    ///
    /// ```sh
    /// OPENCODE_API_KEY=... cargo test -p oc-gui --lib live_opencode_go_gateway_tool_call -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "live network: needs OPENCODE_API_KEY and writes a temporary smoke note"]
    async fn live_opencode_go_gateway_tool_call() {
        fn trace_path() -> std::path::PathBuf {
            std::env::var_os("AIGO_SMOKE_TRACE")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("opencompose-aigo-smoke.md"))
        }
        let key = std::env::var("OPENCODE_API_KEY")
            .expect("OPENCODE_API_KEY is not set; export the OpenCode console token");
        let catalog = crate::ai::catalog::provider("opencode-go").expect("catalog entry");
        let base = catalog.base_url.clone().expect("opencode-go base_url");
        let mut entry = entry(base.clone());
        entry.model = "gemini-4-argon".into();
        let plan = auth::plan(&entry, &Credential::ApiKey(Some(key.into()))).unwrap();
        let provider = GeminiGenerate::for_test(entry, plan);
        let tools = vec![ToolSpec {
            name: "read_context".into(),
            description: "Read the current project subtree as DSL".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        }];
        let first = ProviderRequest {
            system: Some(
                "You are a smoke test. Call read_context once with path \"project\".".into(),
            ),
            turns: turns_from_wire(&json!([
                {"role":"user","content":"Call read_context for the project."}
            ]))
            .unwrap(),
            tools: tools.clone(),
            max_tokens: 2048,
            temperature: 0.2,
            thinking_level: "medium".into(),
            stream: true,
        };
        let started = std::time::Instant::now();
        let mut events = Vec::new();
        let mut sink = |event: StreamEvent| events.push(event);
        let turn = match provider.stream(&first, &mut sink).await {
            Ok(turn) => turn,
            Err(error) => {
                let _ = std::fs::write(
                    trace_path(),
                    format!(
                        "# aigo live smoke — opencode-go / gemini-4-argon\n\n- first request failed: {} ({})\n",
                        error.message,
                        error.kind.as_str()
                    ),
                );
                panic!("first request failed: {error}");
            }
        };
        let calls = turn
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse {
                    id, name, input, ..
                } => Some(json!({"id": id, "name": name, "args": input})),
                _ => None,
            })
            .collect::<Vec<_>>();
        if calls.is_empty() {
            let _ = std::fs::write(
                trace_path(),
                "# aigo live smoke — opencode-go / gemini-4-argon\n\n- no tool call was returned\n",
            );
            panic!("the gateway returned no tool call");
        }
        let mut turns = first.turns.clone();
        turns.push(turn);
        for call in &calls {
            turns.push(Turn {
                role: Role::Tool,
                blocks: vec![Block::ToolResult {
                    call_id: call["id"].as_str().unwrap_or_default().to_owned(),
                    name: call["name"].as_str().unwrap_or_default().to_owned(),
                    content: "{\"ok\":true}".into(),
                    is_error: false,
                }],
                continuity: None,
                extra: None,
            });
        }
        let second = ProviderRequest {
            system: first.system.clone(),
            turns,
            tools,
            max_tokens: 2048,
            temperature: 0.2,
            thinking_level: "medium".into(),
            stream: true,
        };
        let mut second_events = Vec::new();
        let mut sink = |event: StreamEvent| second_events.push(event);
        let final_turn = match provider.stream(&second, &mut sink).await {
            Ok(turn) => turn,
            Err(error) => {
                let _ = std::fs::write(
                    trace_path(),
                    format!(
                        "# aigo live smoke — opencode-go / gemini-4-argon\n\n- tool call: {calls:?}\n- second request failed: {} ({})\n",
                        error.message,
                        error.kind.as_str()
                    ),
                );
                panic!("second request failed: {error}");
            }
        };
        let text = final_turn
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        let usage = |events: &[StreamEvent]| {
            events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::Usage {
                        input,
                        output,
                        reasoning,
                        cached,
                    } => Some(format!(
                        "input {input}, output {output}, reasoning {reasoning}, cached {cached:?}"
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let trace = format!(
            "# aigo live smoke — opencode-go / gemini-4-argon\n\n\
             - base URL: {base}\n\
             - auth: bearer\n\
             - tool call: {calls:?}\n\
             - first usage: {}\n\
             - second usage: {}\n\
             - final text: {text}\n\
             - elapsed: {:?}\n",
            usage(&events).join("; "),
            usage(&second_events).join("; "),
            started.elapsed()
        );
        std::fs::write(trace_path(), trace).expect("write the smoke trace");
        assert!(
            !text.trim().is_empty(),
            "the gateway returned no final text"
        );
    }

    #[tokio::test]
    async fn merged_output_limit_reaches_the_generate_content_body() {
        crate::ai::limits::install_for_test(
            "gemini-wire-fixture",
            "gemini-wire-model",
            crate::ai::limits::ModelLimits {
                context: 128_000,
                max_output: 1_234,
                input: crate::ai::limits::InputModalities {
                    text: true,
                    ..Default::default()
                },
                price: None,
            },
        );
        let (base, server) = canned(200, TEXT);
        let mut entry = entry(format!("{base}v1beta"));
        entry.id = "gemini-wire-fixture".into();
        entry.model = "gemini-wire-model".into();
        let request = ProviderRequest {
            system: Some("You are OpenCompose Assistant.".into()),
            turns: turns_from_wire(&json!([{"role":"user","content":"hello"}])).unwrap(),
            tools: Vec::new(),
            max_tokens: crate::ai::limits::max_output(&entry),
            temperature: 0.3,
            thinking_level: "medium".into(),
            stream: true,
        };
        assert_eq!(request.max_tokens, 1_234);
        let mut sink = |_: StreamEvent| {};
        provider(entry).stream(&request, &mut sink).await.unwrap();
        let body = request_body(&server.join().unwrap());
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1_234);
    }
}
