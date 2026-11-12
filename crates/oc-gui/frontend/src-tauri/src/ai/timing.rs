//! Transport-level timing and canonical token usage for one model turn.
//!
//! Timestamps are taken at the adapter boundary: `sent` before the stream
//! starts, `first`/`last` on content events, and `completed` when the transport
//! returns. Nothing here estimates or extrapolates: a missing measurement stays
//! missing, and a failed or cancelled turn keeps only what actually happened.
use crate::ai::limits::Price;
use crate::ai::prices;
use crate::commands::Wire;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Canonical usage. `cached_input` is a subset of `input`, `reasoning` a subset
/// of `output`; `cached_input` is `None` when the provider sent no cache field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub cached_input: Option<u64>,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
}

/// What one request cost, in the currency its price was published in. Absent
/// when no price is known for the model, so the panel shows no cost at all
/// rather than a made-up zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

/// Estimated cost of the usage that was actually reported, or `None` when the
/// model has no price. A model priced at zero is known to be free and costs
/// zero; a model with no price is unknown and costs nothing it can report.
pub fn estimate_cost(usage: &Usage, price: Option<&Price>) -> Option<Cost> {
    let price = price?;
    if usage.input == 0 && usage.output == 0 {
        return None;
    }
    // Cached input is billed as a subset of the input tokens. A catalogue that
    // publishes no cached-input price bills them at the input price: an upper
    // bound, never a silent discount.
    let cached = usage.cached_input.unwrap_or(0).min(usage.input);
    let fresh = usage.input - cached;
    let cached_price = price.cached_input.unwrap_or(price.input);
    let amount = (fresh as f64 * price.input
        + cached as f64 * cached_price
        + usage.output as f64 * price.output)
        / 1_000_000.0;
    Some(Cost {
        amount,
        currency: price.currency.clone(),
    })
}

/// One turn's measurements, serialized to the AI panel as the `ai_stats` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TurnStats {
    pub model: String,
    pub wire: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    /// First content event to last content event; `None` for fewer than two.
    #[serde(default)]
    pub decode_ms: Option<u64>,
    /// Request sent to completion, including time after the last token.
    #[serde(default)]
    pub wall_ms: u64,
    /// Output tokens divided by the decode window.
    #[serde(default)]
    pub decode_tps: Option<f64>,
    /// Output tokens divided by first-token latency plus the decode window.
    #[serde(default)]
    pub wall_tps: Option<f64>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub cost: Option<Cost>,
}

/// One actual provider attempt, including failed/cancelled reported usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptStats {
    pub ordinal: u32,
    pub stats: TurnStats,
    pub usage_reported: bool,
    pub price: Option<Price>,
}

#[derive(Debug)]
struct State {
    stats: TurnStats,
    provider_id: String,
    wire: Wire,
    attempted: bool,
    next_price: Option<Option<Price>>,
    attempts: Vec<AttemptStats>,
    accounting: Option<Arc<super::session_usage::SessionLease>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            stats: TurnStats::default(),
            provider_id: String::new(),
            wire: Wire::Unknown,
            attempted: false,
            next_price: None,
            attempts: Vec::new(),
            accounting: None,
        }
    }
}

/// Shared slot for one request; survives cancellation because the frontend
/// still needs the partial timing of a cancelled turn.
#[derive(Debug, Clone, Default)]
pub struct TurnStatsHandle(Arc<Mutex<State>>);

impl TurnStatsHandle {
    /// `provider_id` is the saved provider entry's id, so a price the user set
    /// for that entry wins over the bundled one.
    pub fn new(provider_id: impl Into<String>, model: impl Into<String>, wire: Wire) -> Self {
        let handle = Self::default();
        handle.update(|state| {
            state.stats.model = model.into();
            state.stats.wire = wire_label(wire);
            state.provider_id = provider_id.into();
            state.wire = wire;
        });
        handle
    }

    /// A fresh attempt keeps the model, wire and retry count.
    fn begin_attempt(&self) {
        self.update(|state| {
            let stats = &mut state.stats;
            stats.ttft_ms = None;
            stats.decode_ms = None;
            stats.wall_ms = 0;
            stats.decode_tps = None;
            stats.wall_tps = None;
            stats.usage = Usage::default();
            stats.failed = false;
            stats.request_id = None;
            stats.cost = None;
            state.attempted = true;
        });
    }

    /// Attach the session lease that receives usage.
    pub fn set_accounting(&self, lease: Arc<super::session_usage::SessionLease>) {
        self.lock().accounting = Some(lease);
    }
    /// The attached session lease, if any.
    pub fn accounting(&self) -> Option<Arc<super::session_usage::SessionLease>> {
        self.lock().accounting.clone()
    }
    /// Every provider attempt recorded for this turn.
    pub fn attempts(&self) -> Vec<AttemptStats> {
        self.lock().attempts.clone()
    }

    /// Set the price applied to the next attempt.
    pub fn set_next_price(&self, price: Option<Price>) {
        self.lock().next_price = Some(price);
    }

    /// Record a retry.
    pub fn retry(&self) {
        self.update(|state| state.stats.retries = state.stats.retries.saturating_add(1));
    }

    /// Whether any provider attempt started.
    pub fn attempted(&self) -> bool {
        self.lock().attempted
    }

    /// Current turn statistics.
    pub fn snapshot(&self) -> TurnStats {
        self.lock().stats.clone()
    }

    fn update(&self, apply: impl FnOnce(&mut State)) {
        apply(&mut self.lock());
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

fn wire_label(wire: Wire) -> String {
    serde_json::to_value(wire)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

/// Records one attempt. Dropping before `finish` (a cancelled future) commits a
/// failed turn with whatever timing was observed.
#[derive(Debug)]
pub struct Clock {
    handle: TurnStatsHandle,
    sent: Instant,
    first: Option<Instant>,
    last: Option<Instant>,
    usage: Usage,
    request_id: Option<String>,
    finished: bool,
    usage_reported: bool,
    price: Option<Price>,
    ordinal: u32,
}

impl Clock {
    /// Start one provider attempt and its clock.
    pub fn start(handle: &TurnStatsHandle) -> Self {
        handle.begin_attempt();
        let (provider, model, wire, ordinal, reserved) = {
            let mut state = handle.lock();
            (
                state.provider_id.clone(),
                state.stats.model.clone(),
                state.wire,
                state.attempts.len() as u32,
                state.next_price.take(),
            )
        };
        let price = reserved.unwrap_or_else(|| prices::resolve(&provider, wire, &model));
        Self {
            handle: handle.clone(),
            sent: Instant::now(),
            first: None,
            last: None,
            usage: Usage::default(),
            request_id: None,
            finished: false,
            usage_reported: false,
            price,
            ordinal,
        }
    }

    /// Record a content event.
    pub fn content(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// Record a bounded provider request id.
    pub fn request_id(&mut self, id: &str) {
        if self.request_id.is_none() && !id.is_empty() && id.len() <= 512 {
            self.request_id = Some(id.to_owned());
        }
    }

    /// Record canonical usage for the attempt.
    pub fn usage(&mut self, usage: Usage) {
        self.usage_reported = true;
        self.usage = usage;
    }

    /// Finish the attempt and commit its statistics.
    pub fn finish(mut self, failed: bool) {
        self.commit(failed);
    }

    fn commit(&mut self, failed: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let completed = Instant::now();
        let ttft = self.first.map(|first| first - self.sent);
        let window = match (self.first, self.last) {
            (Some(first), Some(last)) if last > first => Some(last - first),
            _ => None,
        };
        let since_sent = self.last.map(|last| last - self.sent);
        let output = self.usage.output;
        let decode_tps = window
            .map(|window| output as f64 / window.as_secs_f64())
            .filter(|_| output > 0);
        let wall_tps = since_sent
            .filter(|elapsed| !elapsed.is_zero() && output > 0)
            .map(|elapsed| output as f64 / elapsed.as_secs_f64());
        let usage = self.usage;
        let request_id = self.request_id.clone();
        let cost = estimate_cost(&usage, self.price.as_ref());
        let wall_ms = millis(completed - self.sent);
        self.handle.update(|state| {
            let stats = &mut state.stats;
            stats.ttft_ms = ttft.map(millis);
            stats.decode_ms = window.map(millis);
            stats.wall_ms = wall_ms;
            stats.decode_tps = decode_tps;
            stats.wall_tps = wall_tps;
            stats.usage = usage;
            stats.request_id = request_id;
            stats.cost = cost;
            stats.failed = failed;
            // Admission refuses another dispatch at the bounded attempt limit.
            state.attempts.push(AttemptStats {
                ordinal: self.ordinal,
                stats: stats.clone(),
                usage_reported: self.usage_reported,
                price: self.price.clone(),
            });
        });
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.commit(true);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accounting_attempt_prices_are_frozen_and_cancel_keeps_reported_usage() {
        let stats = TurnStatsHandle::new("fixture", "fixture", Wire::OpenAiChatCompletions);
        stats.set_next_price(Some(usd(2.0, 4.0, Some(1.0))));
        let mut first = Clock::start(&stats);
        stats.set_next_price(Some(usd(100.0, 100.0, None)));
        first.usage(Usage {
            input: 1000,
            cached_input: Some(500),
            output: 1000,
            reasoning: 500,
        });
        drop(first);
        let first = stats.attempts();
        assert!(first[0].stats.failed);
        assert!(first[0].usage_reported);
        assert!((first[0].stats.cost.as_ref().unwrap().amount - 0.0055).abs() < 1e-12);
        let second = Clock::start(&stats);
        second.finish(false);
        let attempts = stats.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[1].ordinal, 1);
        assert_eq!(attempts[1].price.as_ref().unwrap().input, 100.0);
        assert!(!attempts[1].usage_reported);
    }

    use super::*;
    use crate::ai::auth::AuthPlan;
    use crate::ai::chat_completions::ChatCompletions;
    use crate::ai::gemini_generate::GeminiGenerate;
    use crate::ai::{Block, ProviderRequest, Role, Turn, test_support::canned};
    use crate::commands::{AiCapabilities, AiProviderEntry, AuthKind, ProviderKind, Wire};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn entry(wire: Wire, endpoint: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "fixture".into(),
            name: "fixture".into(),
            kind: wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions),
            wire: Some(wire),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint,
            model: "fixture-model".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 128,
            temperature: 0.2,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                audio_in: false,
                audio_out: false,
                native_tool_calling: false,
                streaming: true,
            },
        }
    }

    fn request(stream: bool) -> ProviderRequest {
        ProviderRequest {
            system: None,
            turns: vec![Turn {
                role: Role::User,
                blocks: vec![Block::Text("hello".into())],
                continuity: None,
                extra: None,
            }],
            tools: vec![],
            max_tokens: 128,
            temperature: 0.2,
            thinking_level: "medium".into(),
            stream,
        }
    }

    fn read_request(socket: &mut TcpStream) {
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
    }

    fn write_frame(socket: &mut TcpStream, data: &str) {
        let frame = format!("{data}\n\n");
        let _ = write!(socket, "{:x}\r\n{frame}\r\n", frame.len());
        let _ = socket.flush();
    }

    /// Streams three content deltas at 50/60/70 ms, then usage, then closes.
    /// `paced` inserts those delays; the cancel fixture stalls after the first.
    fn chat_stream(cached: Option<u64>, paced: bool) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.flush();
            for (index, delta) in ["a", "b", "c"].iter().enumerate() {
                let delay = if index == 0 {
                    50
                } else if paced {
                    10
                } else {
                    2_000
                };
                thread::sleep(Duration::from_millis(delay));
                let event = json!({
                    "id": "chatcmpl-fixture",
                    "choices": [{"delta": {"content": delta}, "finish_reason": null}]
                });
                write_frame(&mut socket, &format!("data: {event}"));
            }
            let usage = match cached {
                Some(cached) => json!({
                    "prompt_tokens": 1000,
                    "completion_tokens": 3,
                    "prompt_tokens_details": {"cached_tokens": cached}
                }),
                None => json!({"prompt_tokens": 1000, "completion_tokens": 3}),
            };
            let event = json!({
                "id": "chatcmpl-fixture",
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": usage
            });
            write_frame(&mut socket, &format!("data: {event}"));
            write_frame(&mut socket, "data: [DONE]");
            let _ = write!(socket, "0\r\n\r\n");
            let _ = socket.flush();
        });
        (endpoint, server)
    }

    async fn run_chat(
        endpoint: String,
        stats: &TurnStatsHandle,
    ) -> Result<serde_json::Value, crate::ai::ProviderError> {
        let entry = entry(Wire::OpenAiChatCompletions, endpoint);
        let route = crate::ai::route::Route::for_entry(&entry).unwrap();
        let provider = ChatCompletions::new(entry, route, AuthPlan::default());
        let mut emitted = false;
        crate::ai::run(&provider, &request(true), &None, &mut emitted, stats).await
    }

    #[tokio::test]
    async fn timing_is_measured_at_the_transport_and_usage_carries_the_cache() {
        // The first body flush is scheduled 50 ms after the request; allow a
        // generous 40–250 ms window for connection setup and scheduler jitter.
        let stats = TurnStatsHandle::new("fixture", "fixture-model", Wire::OpenAiChatCompletions);
        let (endpoint, server) = chat_stream(Some(620), true);
        let message = run_chat(endpoint, &stats).await.unwrap();
        server.join().unwrap();

        let stats = stats.snapshot();
        assert_eq!(stats.model, "fixture-model");
        assert_eq!(stats.wire, "openai-chat-completions");
        assert_eq!(stats.request_id.as_deref(), Some("chatcmpl-fixture"));
        assert!(!stats.failed);
        assert!(stats.wall_ms > 0);
        let ttft = stats.ttft_ms.expect("first content event was timed");
        assert!((40..250).contains(&ttft), "ttft {ttft} ms");
        let window = stats.decode_ms.expect("two content events define a window");
        assert!((10..120).contains(&window), "decode window {window} ms");
        // TPS comes from the token count and the measured window, never a clock
        // reading taken around the whole request.
        let decode_tps = stats.decode_tps.expect("decode tps");
        let expected = 3.0 / (window as f64 / 1000.0);
        assert!(
            (decode_tps - expected).abs() < expected * 0.15,
            "decode tps {decode_tps}, window-derived {expected}"
        );
        let wall_tps = stats.wall_tps.expect("wall tps");
        assert!(wall_tps < decode_tps, "{wall_tps} < {decode_tps}");
        assert_eq!(stats.usage.input, 1000);
        assert_eq!(stats.usage.output, 3);
        assert_eq!(stats.usage.cached_input, Some(620));
        assert_eq!(message["usage"]["cached_input"], 620);
        assert_eq!(message["content"], "abc");
    }

    #[tokio::test]
    async fn a_chat_completions_response_without_a_cache_field_reports_none() {
        let stats = TurnStatsHandle::new("fixture", "fixture-model", Wire::OpenAiChatCompletions);
        let (endpoint, server) = chat_stream(None, true);
        run_chat(endpoint, &stats).await.unwrap();
        server.join().unwrap();
        let stats = stats.snapshot();
        assert_eq!(stats.usage.input, 1000);
        assert_eq!(stats.usage.cached_input, None);
        assert!(stats.decode_tps.is_some());
    }

    #[tokio::test]
    async fn gemini_generate_counts_thoughts_as_output_and_cached_as_a_subset() {
        let body = "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hel\"}]}}]}\n\n\
                    data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"lo\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1000,\"cachedContentTokenCount\":620,\"candidatesTokenCount\":250,\"thoughtsTokenCount\":60}}\n\n";
        let (base, server) = canned(200, body);
        let entry = entry(Wire::GeminiGenerate, base);
        let route = crate::ai::route::Route::for_entry(&entry).unwrap();
        let provider = GeminiGenerate::new(entry, route, AuthPlan::default());
        let stats = TurnStatsHandle::new("fixture", "fixture-model", Wire::GeminiGenerate);
        let mut emitted = false;
        crate::ai::run(&provider, &request(true), &None, &mut emitted, &stats)
            .await
            .unwrap();
        server.join().unwrap();
        let usage = stats.snapshot().usage;
        assert_eq!(usage.input, 1000);
        assert_eq!(usage.output, 310, "visible + thoughts");
        assert_eq!(usage.reasoning, 60);
        assert_eq!(usage.cached_input, Some(620));
    }

    #[tokio::test]
    async fn cancellation_mid_stream_keeps_ttft_and_fails_without_tps() {
        let stats = TurnStatsHandle::new("fixture", "fixture-model", Wire::OpenAiChatCompletions);
        let (endpoint, _server) = chat_stream(None, false);
        let outcome = tokio::time::timeout(Duration::from_millis(600), async {
            run_chat(endpoint, &stats).await
        })
        .await;
        assert!(outcome.is_err(), "the stalled stream must time out");
        let stats = stats.snapshot();
        assert!(stats.failed);
        assert!(stats.ttft_ms.is_some(), "the first token did arrive");
        assert!(stats.decode_tps.is_none(), "no invented TPS");
        assert!(stats.wall_tps.is_none());
        assert_eq!(stats.usage.output, 0);
    }

    fn usd(input: f64, output: f64, cached_input: Option<f64>) -> Price {
        Price {
            input,
            output,
            cached_input,
            currency: "USD".into(),
        }
    }

    /// Drives one turn through the clock so the recorded cost is the one the
    /// panel would receive.
    fn priced_turn(provider: &str, model: &str, wire: Wire, usage: Usage) -> Option<Cost> {
        let stats = TurnStatsHandle::new(provider, model, wire);
        let mut clock = Clock::start(&stats);
        clock.usage(usage);
        clock.finish(false);
        stats.snapshot().cost
    }

    #[test]
    fn cost_needs_a_price_and_the_per_million_token_math_is_exact() {
        let usage = Usage {
            input: 1_000_000,
            cached_input: None,
            output: 500_000,
            reasoning: 0,
        };
        assert_eq!(estimate_cost(&usage, None), None);
        let price = usd(2.0, 8.0, None);
        assert_eq!(
            estimate_cost(&usage, Some(&price)).map(|cost| cost.amount),
            Some(6.0)
        );
        // No tokens were billed, so there is nothing to cost.
        assert_eq!(estimate_cost(&Usage::default(), Some(&price)), None);
    }

    #[test]
    fn a_priced_model_costs_the_reported_usage_and_names_its_currency() {
        let usage = Usage {
            input: 1_000_000,
            cached_input: Some(200_000),
            output: 500_000,
            reasoning: 0,
        };
        let cost = priced_turn("openai", "gpt-6.1-sol", Wire::OpenAiResponses, usage)
            .expect("the bundled snapshot prices this model");
        // 800k fresh at $2, 200k cached at $0.10, 500k output at $10, per million.
        assert!((cost.amount - 6.62).abs() < 1e-9, "{}", cost.amount);
        assert_eq!(cost.currency, "USD");
    }

    #[test]
    fn cached_tokens_without_a_cache_price_are_billed_at_the_input_rate() {
        let usage = Usage {
            input: 1_000,
            cached_input: Some(400),
            output: 0,
            reasoning: 0,
        };
        let cost = estimate_cost(&usage, Some(&usd(2.0, 10.0, None))).unwrap();
        assert!((cost.amount - 0.002).abs() < 1e-12, "{}", cost.amount);
        let discounted = estimate_cost(&usage, Some(&usd(2.0, 10.0, Some(0.5)))).unwrap();
        assert!(
            (discounted.amount - 0.0014).abs() < 1e-12,
            "{}",
            discounted.amount
        );
    }

    #[test]
    fn a_model_without_a_price_reports_no_cost_rather_than_zero() {
        let usage = Usage {
            input: 1_000,
            cached_input: None,
            output: 100,
            reasoning: 0,
        };
        // Ollama is a local server: the dataset publishes no per-token price.
        assert_eq!(
            priced_turn("ollama", "llama3.1", Wire::OpenAiChatCompletions, usage),
            None
        );
        // The event carries the absence, so the panel can omit the row.
        let stats = TurnStatsHandle::new("ollama", "llama3.1", Wire::OpenAiChatCompletions);
        let mut clock = Clock::start(&stats);
        clock.usage(usage);
        clock.finish(false);
        let event = serde_json::to_value(stats.snapshot()).unwrap();
        assert!(event["cost"].is_null(), "{event}");
    }

    #[test]
    fn a_user_price_beats_the_bundled_price() {
        let provider = "pricing-fixture";
        crate::ai::limits::install_for_test(
            provider,
            "gpt-6.1-sol",
            crate::ai::limits::ModelLimits {
                context: 1_000_000,
                max_output: 64_000,
                input: crate::ai::limits::InputModalities {
                    text: true,
                    ..Default::default()
                },
                price: Some(Price {
                    input: 1.0,
                    output: 2.0,
                    cached_input: Some(0.5),
                    currency: "EUR".into(),
                }),
            },
        );
        let usage = Usage {
            input: 1_000_000,
            cached_input: None,
            output: 500_000,
            reasoning: 0,
        };
        let cost = priced_turn(provider, "gpt-6.1-sol", Wire::OpenAiResponses, usage)
            .expect("the user price applies");
        // $1 in + $2 out per million, not the bundled $2 + $10.
        assert!((cost.amount - 2.0).abs() < 1e-9, "{}", cost.amount);
        assert_eq!(cost.currency, "EUR", "the override carries its currency");
        crate::ai::limits::clear_for_test(provider, "gpt-6.1-sol");
        let bundled = priced_turn(provider, "gpt-6.1-sol", Wire::OpenAiResponses, usage)
            .expect("the bundled price applies again");
        assert!((bundled.amount - 7.0).abs() < 1e-9, "{}", bundled.amount);
    }

    #[test]
    fn an_unknown_model_id_is_unpriced_without_panicking() {
        let usage = Usage {
            input: 10,
            cached_input: None,
            output: 1,
            reasoning: 0,
        };
        assert_eq!(
            priced_turn("openai", "gpt-9-imaginary", Wire::OpenAiResponses, usage),
            None
        );
        assert_eq!(
            estimate_cost(
                &usage,
                crate::ai::prices::resolve("openai", Wire::OpenAiResponses, "gpt-9-imaginary")
                    .as_ref()
            ),
            None
        );
    }
}
