//! Local agent control surface for OpenCompose.
//!
//! The Tauri command layer is normally only reachable from the webview. This
//! module exposes the same backend over a token-protected HTTP/JSON API and an
//! MCP-compatible endpoint on loopback, so an external agent can drive the
//! project, transport, plugins, and renderer without screen automation.
//!
//! Design constraints:
//! - Loopback only (`127.0.0.1`), never a public interface.
//! - A random bearer token is written to `~/.opencompose/agent.json` with 0600.
//! - No shell execution; file access and installation use application commands.
//! - All mutations go through the same command layer as the UI, so the project
//!   state, undo stack, and render cache stay consistent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

#[cfg(test)]
use crate::agent_tools::project_query::validation_value;
use crate::agent_tools::separation::{SeparateStemsArgs, SeparateStemsMode};
use crate::ai_service::AiError;
use crate::commands;
use crate::midi_commands;
use crate::state::{AppState, HTTP_BATCH_OWNER};
use crate::tool_registry::arguments::ToolArguments;
use crate::tool_registry::{ToolId, is_mutation_tool, mcp_tools};

mod access_token;
mod saved_completion;
use access_token::make_token;
mod audition_media;
mod dispatch_policy;
use dispatch_policy::InstallPolicy;
mod separator_process;

const DEFAULT_PORT: u16 = 38642;
// Media permits 20 MB of inline base64; leave bounded headroom for JSON and
// conversation context. The media layer still enforces its own smaller limits.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// Concurrent client connections served at once. A connection past the cap is
/// refused instead of queued, so one client cannot pin the process.
const MAX_CONNECTIONS: usize = 64;
/// How long a handler waits for the main thread before it reports it as busy.
const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The port this process bound, or zero before the listener is available.
static ENDPOINT_PORT: AtomicU16 = AtomicU16::new(0);

pub(crate) fn audition_render_url(path: &std::path::Path) -> Result<String, String> {
    let port = ENDPOINT_PORT.load(Ordering::Relaxed);
    if port == 0 {
        return Err("Saved audio playback is unavailable".into());
    }
    audition_media::url(port, path)
}

/// Start the loopback agent server. Safe to call once from Tauri setup.
pub fn spawn(app: AppHandle) {
    let token = match make_token() {
        Ok(token) => token,
        Err(access_token::TokenError::EntropyUnavailable) => {
            log::error!("[agent] could not generate an access token; server not started");
            return;
        }
    };
    let (listener, port) = match bind_listener() {
        Ok(value) => value,
        Err(error) => {
            log::error!("[agent] could not bind loopback server: {error}");
            return;
        }
    };
    ENDPOINT_PORT.store(port, Ordering::Relaxed);
    if let Err(error) = write_endpoint_file(port, &token) {
        log::warn!("[agent] could not write ~/.opencompose/agent.json: {error}");
    }
    log::info!("[agent] listening on http://127.0.0.1:{port}");
    eprintln!("[opencompose-agent] http://127.0.0.1:{port}");

    let token = Arc::new(token);
    let connections = Arc::new(AtomicUsize::new(0));
    let _ = std::thread::Builder::new()
        .name("opencompose-agent".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let Some(slot) = ConnectionSlot::acquire(&connections) else {
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                            let _ = write_response(
                                &mut stream,
                                503,
                                &error_body("agent server is at its connection limit"),
                            );
                            continue;
                        };
                        let app = app.clone();
                        let token = Arc::clone(&token);
                        let _ = std::thread::Builder::new()
                            .name("opencompose-agent-conn".into())
                            .spawn(move || {
                                // The slot returns to the cap when this thread ends,
                                // including on an early panic.
                                let _slot = slot;
                                let mut stream = stream;
                                if let Err(error) = handle_connection(&mut stream, &app, &token) {
                                    log::debug!("[agent] connection error: {error}");
                                }
                            });
                    }
                    Err(error) => log::warn!("[agent] accept error: {error}"),
                }
            }
        });
}

/// One connection's place under [`MAX_CONNECTIONS`], released on drop.
struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    /// `None` when every slot is taken.
    fn acquire(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONNECTIONS).then_some(active + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(count)))
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Serializes `/ai/complete` turns. The endpoint has no session of its own, so
/// a driver that sends several must not run two provider turns at once.
static AI_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the turn lock; a turn that panicked must not wedge the endpoint.
fn lock_ai_turn(lock: &std::sync::Mutex<()>) -> std::sync::MutexGuard<'_, ()> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the panel is running a turn. Its turns claim the single slot that
/// `ai_service::Requests` owns, which this module cannot claim for itself.
fn panel_turn_active(app: &AppHandle) -> bool {
    app.state::<crate::ai_service::Requests>()
        .active()
        .is_ok_and(|active| active.is_some())
}

fn bind_listener() -> Result<(TcpListener, u16), String> {
    let base = std::env::var("OPENCOMPOSE_AGENT_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let mut last_error = String::new();
    for offset in 0..20u16 {
        let port = base.saturating_add(offset);
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                return Ok((listener, port));
            }
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(format!("no free port near {base}: {last_error}"))
}

#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
    version: String,
}

fn endpoint_path() -> std::path::PathBuf {
    crate::private_fs::config_dir().join("agent.json")
}

fn write_endpoint_file(port: u16, token: &str) -> std::io::Result<()> {
    let path = endpoint_path();
    let endpoint = Endpoint {
        port,
        token: token.to_string(),
        pid: std::process::id(),
        version: VERSION.to_string(),
    };
    let body = serde_json::to_string_pretty(&endpoint).map_err(std::io::Error::other)?;
    crate::private_fs::write_atomic(&path, body.as_bytes())
}

/// Remove the endpoint file at exit, so a later client does not connect to a
/// port that stopped listening. Called from the app's `Exit` event.
pub fn shutdown() {
    let port = ENDPOINT_PORT.load(Ordering::Relaxed);
    if port == 0 {
        return;
    }
    match remove_owned_endpoint(&endpoint_path(), port) {
        Ok(true) => log::debug!("[agent] removed the endpoint file"),
        Ok(false) => log::debug!("[agent] endpoint file was not this process's to remove"),
        Err(error) => log::warn!("[agent] could not remove ~/.opencompose/agent.json: {error}"),
    }
}

/// Remove `path` only when it still records this process on `port`: a later
/// instance overwrites the file, and removing that one would strand its agent.
fn remove_owned_endpoint(path: &std::path::Path, port: u16) -> std::io::Result<bool> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        // Nothing was written, or another exit already removed it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&body) else {
        // Content that does not parse is not provably ours; leave it alone.
        return Ok(false);
    };
    if endpoint.pid != std::process::id() || endpoint.port != port {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A connection refused before any handler ran, with the status it answers.
#[derive(Debug)]
enum RequestError {
    BadRequest(String),
    LengthRequired(String),
    PayloadTooLarge(String),
}

impl RequestError {
    fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::LengthRequired(_) => 411,
            Self::PayloadTooLarge(_) => 413,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(message)
            | Self::LengthRequired(message)
            | Self::PayloadTooLarge(message) => message,
        }
    }
}

impl From<std::io::Error> for RequestError {
    fn from(error: std::io::Error) -> Self {
        Self::BadRequest(error.to_string())
    }
}

fn handle_connection(stream: &mut TcpStream, app: &AppHandle, token: &str) -> std::io::Result<()> {
    handle_connection_with(stream, |request| route(app, request, token))
}

/// The connection path with the router passed in, so the body bound and the
/// status each refusal answers are exercisable without a Tauri app.
fn handle_connection_with<F>(stream: &mut TcpStream, respond: F) -> std::io::Result<()>
where
    F: FnOnce(&Request) -> (u16, String),
{
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => return write_response(stream, error.status(), &error_body(error.message())),
    };
    if audition_media::matches(&request.path) {
        let range = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.as_str());
        let store = crate::ai::sessions::Store::config();
        return audition_media::serve(stream, store.root(), &request.method, &request.path, range);
    }
    let (status, body) = respond(&request);
    write_response(stream, status, &body)
}

fn read_request(stream: &mut TcpStream) -> Result<Request, RequestError> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(RequestError::BadRequest("empty request".into()));
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err(RequestError::BadRequest("invalid request line".into()));
    }

    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                // A length that does not parse is not zero: refuse instead of
                // running the handler on a body the client never saw arrive.
                content_length = Some(value.parse().map_err(|_| {
                    RequestError::BadRequest(format!("invalid Content-Length: {value}"))
                })?);
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                chunked = true;
            }
            headers.push((name.to_string(), value.to_string()));
        }
    }

    // Only a Content-Length body is decoded. A chunked body would otherwise read
    // as empty, and the handler would answer a request it never received.
    if chunked {
        return Err(RequestError::LengthRequired(
            "Transfer-Encoding is not supported; send Content-Length".into(),
        ));
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_BODY {
        return Err(RequestError::PayloadTooLarge(format!(
            "body exceeds {MAX_BODY} bytes"
        )));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let header = if status == 204 {
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
            body.len()
        )
    };
    stream.write_all(header.as_bytes())?;
    if status != 204 {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}

fn route(app: &AppHandle, request: &Request, token: &str) -> (u16, String) {
    route_with(
        request,
        token,
        |body| handle_call(app, body),
        |body| {
            // A headless turn must not run beside the panel's own turn, which
            // holds the single slot `ai_chat` claims.
            if panel_turn_active(app) {
                return complete_failure(AiError::new("busy", "Another AI request is active"));
            }
            handle_ai_complete_for(body, Some(app))
        },
        |body| handle_mcp(app, body),
    )
}

/// The router with the app-state handlers passed in, so everything else (the
/// token check, the body bound, `/ai/complete`) is exercisable without a Tauri
/// app.
fn route_with<C, A, M>(
    request: &Request,
    token: &str,
    call: C,
    complete: A,
    mcp: M,
) -> (u16, String)
where
    C: Fn(&[u8]) -> (u16, String),
    A: Fn(&[u8]) -> (u16, String),
    M: Fn(&[u8]) -> (u16, String),
{
    let path = request.path.split('?').next().unwrap_or(&request.path);
    if request.method == "OPTIONS" {
        return (204, String::new());
    }
    if path == "/health" && request.method == "GET" {
        return (
            200,
            json!({
                "ok": true,
                "service": "opencompose-agent",
                "version": VERSION,
            })
            .to_string(),
        );
    }
    if !authorized(&request.headers, token) {
        return (401, error_body("unauthorized"));
    }
    match (request.method.as_str(), path) {
        ("GET", "/tools") => (200, json!({ "tools": mcp_tools() }).to_string()),
        // The panel's own surface, from the same registry call the `ai_tools`
        // command answers: a headless client must be offered what the panel is
        // offered, not the wider MCP catalog.
        ("GET", "/ai/tools") => (200, panel_tools_body(&request.path)),
        ("GET", "/ai/toolsets") => (
            200,
            json!({ "toolsets": crate::ai::toolsets::effective() }).to_string(),
        ),
        ("POST", "/call") => call(&request.body),
        ("POST", "/ai/complete") => complete(&request.body),
        ("POST", "/mcp") => mcp(&request.body),
        _ => (404, error_body("not found")),
    }
}

/// `?toolsets=a,b` narrows to those sets; no query keeps the whole surface,
/// the same default the panel's own call has.
fn panel_tools_body(path: &str) -> String {
    let selection = path.split_once('?').and_then(|(_, query)| {
        let raw = query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "toolsets").then_some(value)
        })?;
        Some(
            raw.split(',')
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>(),
        )
    });
    json!({ "tools": crate::tool_registry::panel_tools_for(selection.as_deref()) }).to_string()
}

/// `POST /ai/complete`: one provider turn through the same encoder the panel
/// uses, for a headless driver. Body: `{provider, messages, tools?, max_tokens?}`;
/// answer: the decoded wire message with `ok` beside it. A failure carries the
/// class-derived status and the provider layer's verdict. The endpoint never logs
/// the request or the credential.
#[cfg(test)]
fn handle_ai_complete(body: &[u8]) -> (u16, String) {
    handle_ai_complete_for(body, None)
}

fn handle_ai_complete_for(body: &[u8], app: Option<&AppHandle>) -> (u16, String) {
    let request = match complete_request(body) {
        Ok(request) => request,
        Err(error) => return complete_failure(AiError::new("request", error)),
    };
    // Held until the turn ends, on every path including a panic. A malformed
    // request is refused above without waiting behind a running turn.
    let _turn = lock_ai_turn(&AI_TURN);
    match block_on(complete_turn(&request, app)) {
        Ok(message) => {
            let mut payload = match message {
                Value::Object(object) => object,
                other => return (200, json!({ "ok": true, "content": other }).to_string()),
            };
            payload.insert("ok".into(), Value::Bool(true));
            (200, Value::Object(payload).to_string())
        }
        Err(error) => complete_failure(error),
    }
}

/// The HTTP status a failed `/ai/complete` answers with. Only a malformed
/// request is 400; everything else names what the caller has to change.
fn complete_status(kind: &str) -> u16 {
    match kind {
        "request" => 400,
        "auth" | "auth_expired" => 401,
        "permission" => 403,
        "rate_limit" => 429,
        // Another turn already holds the slot, on either surface.
        "busy" | "cancelled" => 409,
        // The saved endpoint or provider has to change before a retry can work.
        "config" | "redirect" => 409,
        "timeout" | "network" | "server" => 503,
        "protocol" | "truncated" => 502,
        _ => 502,
    }
}

/// The failure body. `error` stays the message string every existing caller
/// reads; `kind`, `retryable` and `reauth` are the class the panel's own error
/// type carries, so a headless driver can tell a bad request from a bad key.
fn complete_failure(error: AiError) -> (u16, String) {
    let status = complete_status(&error.kind);
    let mut payload = crate::ai::skill_hints::refusal(&error.message);
    payload["kind"] = json!(error.kind);
    payload["retryable"] = json!(error.retryable);
    payload["reauth"] = json!(error.reauth);
    (status, payload.to_string())
}

struct CompleteRequest {
    provider: String,
    messages: Value,
    tools: bool,
    max_tokens: Option<u64>,
    saved: Option<saved_completion::Binding>,
}

fn complete_request(body: &[u8]) -> Result<CompleteRequest, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| format!("invalid JSON: {error}"))?;
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= 100)
        .ok_or("missing 'provider'")?
        .to_owned();
    let messages = value
        .get("messages")
        .filter(|messages| messages.is_array())
        .cloned()
        .ok_or("missing 'messages'")?;
    crate::ai_service::validate_chat_messages(&messages).map_err(|error| error.message)?;
    crate::ai::turns_from_wire(&messages).map_err(|error| error.message)?;
    let tools = match value.get("tools") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(tools)) => *tools,
        Some(_) => return Err("'tools' must be a boolean".into()),
    };
    let max_tokens = match value.get("max_tokens") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(
            number
                .as_u64()
                .filter(|tokens| (1..=262_144).contains(tokens))
                .ok_or("'max_tokens' must be between 1 and 262144")?,
        ),
        Some(_) => return Err("'max_tokens' must be an integer".into()),
    };
    Ok(CompleteRequest {
        provider,
        messages,
        tools,
        max_tokens,
        saved: saved_completion::Binding::parse(&value)?,
    })
}

async fn complete_turn(
    request: &CompleteRequest,
    app: Option<&AppHandle>,
) -> Result<Value, AiError> {
    let entry = crate::ai_service::provider(&request.provider).map_err(AiError::config)?;
    crate::ai::endpoint(&entry.endpoint).map_err(AiError::config)?;
    let stats = crate::ai::timing::TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
    let token =
        access_token::make_token().map_err(|_| AiError::config("Request identity unavailable"))?;
    let request_id = format!("headless-{token}");
    let requests = app.map(|app| app.state::<crate::ai_service::Requests>());
    let mut guard = requests
        .as_ref()
        .map(|requests| {
            requests.claim_headless(
                &request_id,
                request.saved.as_ref().map(saved_completion::Binding::id),
            )
        })
        .transpose()?;
    let mut saved = None;
    let operation = async {
        if let Some(binding) = request.saved.as_ref() {
            saved = Some(
                saved_completion::SavedTurn::prepare(
                    app.ok_or_else(|| AiError::config("Saved turns require an application host"))?,
                    binding,
                    &entry,
                    &request.messages,
                    &stats,
                    &request_id,
                )
                .await?,
            );
        }
        let outbound = saved
            .as_ref()
            .map_or(&request.messages, |saved| &saved.messages);
        let credential = crate::ai_service::resolve_credential(&entry).await?;
        let (mut turn, _notices) =
            crate::ai::request_from_wire(&entry, outbound.clone(), request.tools, None)
                .map_err(|error| AiError::new("request", error.message))?;
        if let Some(max_tokens) = request.max_tokens {
            turn.max_tokens = max_tokens;
        }
        let provider = crate::ai::provider_for(&entry, credential).map_err(AiError::from)?;
        let mut emitted = false;
        crate::ai::run(provider.as_ref(), &turn, &None, &mut emitted, &stats)
            .await
            .map_err(AiError::from)
    };
    let mut result = match guard.as_mut() {
        Some(guard) => guard.run(operation).await,
        None => operation.await,
    };
    if let Some(saved) = saved {
        saved.finish(&entry, &stats, result.as_mut().ok()).await;
    } else if let Some(app) = app {
        crate::ai::session_usage::checkpoint_final(
            &stats,
            app,
            &request_id,
            crate::ai::session_usage::Purpose::Chat,
        )
        .await;
    }
    let mut message = result?;
    // Our canonical turn keeps no raw finish string, so the outcome we decoded
    // is what this endpoint reports: a turn with calls asked for them.
    let reason = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    message["finish_reason"] = json!(reason);
    Ok(message)
}

fn authorized(headers: &[(String, String)], token: &str) -> bool {
    headers.iter().any(|(name, value)| {
        if name.eq_ignore_ascii_case("x-opencompose-token") {
            value == token
        } else if name.eq_ignore_ascii_case("authorization") {
            value
                .strip_prefix("Bearer ")
                .map(|value| value.trim() == token)
                .unwrap_or(false)
        } else {
            false
        }
    })
}

fn ok_body(result: Value) -> String {
    // `ok` is derived, not assumed: a validation answer carries `ok: false` inside
    // the result, and the envelope must not overrule it.
    tool_response(result).to_string()
}

/// A refusal, as every JSON surface answers it: the message verbatim, plus the
/// skill id that explains it when one does.
fn error_body(message: impl Into<String>) -> String {
    crate::ai::skill_hints::refusal(&message.into()).to_string()
}

/// An MCP tool failure. The first content block is the error text, unchanged;
/// the prose call that reads the skill that explains it, when one does, is a
/// second block.
fn mcp_error(message: String) -> Value {
    let skill = crate::ai::skill_hints::for_message(&message);
    let mut content = vec![json!({ "type": "text", "text": message })];
    if let Some(skill) = skill {
        content.push(json!({
            "type": "text",
            "text": crate::ai::skill_hints::hint_text(skill),
        }));
    }
    json!({ "content": content, "isError": true })
}

/// Wrap a handler result for the HTTP envelope. A handler reports a structured
/// failure by returning `ok: false` in its payload, so the envelope must not
/// claim success for it. The failure's `error` (and the skill id and prose hint
/// that explain it) is repeated at the top level, where a caller looks for it.
fn tool_response(result: Value) -> Value {
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let mut envelope = json!({ "ok": ok, "result": result });
    if !ok {
        let inner = envelope["result"].clone();
        for key in ["error", "skill", "hint"] {
            if let Some(value) = inner.get(key) {
                envelope[key] = value.clone();
            }
        }
    }
    envelope
}

fn handle_call(app: &AppHandle, body: &[u8]) -> (u16, String) {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => return (400, error_body(format!("invalid JSON: {error}"))),
    };
    let Some(tool) = value
        .get("tool")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
    else {
        return (400, error_body("missing 'tool'"));
    };
    let args = value
        .get("args")
        .or_else(|| value.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let binding = match saved_completion::Binding::parse(&value) {
        Ok(binding) => binding,
        Err(error) => return (400, error_body(error)),
    };
    let caller = match binding {
        Some(binding) => match binding.caller(app) {
            Ok(caller) => caller,
            Err(error) => return (400, error_body(error)),
        },
        None => HTTP_BATCH_OWNER.to_owned(),
    };
    match dispatch(app, tool, &args, &caller) {
        Ok(result) => (200, ok_body(result)),
        Err(error) => (400, error_body(error)),
    }
}

fn handle_mcp(app: &AppHandle, body: &[u8]) -> (u16, String) {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => {
            return (
                400,
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") }
                })
                .to_string(),
            );
        }
    };
    let id = value.get("id").cloned();
    let method = value.get("method").and_then(Value::as_str).unwrap_or("");
    if id.is_none() {
        // JSON-RPC notification (e.g. notifications/initialized).
        return (202, String::new());
    }

    let response: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "opencompose", "version": VERSION }
        })),
        "tools/list" => Ok(json!({ "tools": mcp_tools() })),
        "tools/call" => {
            let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match dispatch(app, name, &args, HTTP_BATCH_OWNER) {
                Ok(result) => {
                    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(true);
                    let text = match result {
                        Value::String(text) => text,
                        other => serde_json::to_string_pretty(&other)
                            .unwrap_or_else(|_| other.to_string()),
                    };
                    Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": !ok
                    }))
                }
                Err(error) => Ok(mcp_error(error)),
            }
        }
        "ping" => Ok(json!({})),
        other => Err((-32601, format!("method not found: {other}"))),
    };

    let payload = match response {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    };
    (200, payload.to_string())
}

// ============================================================================
// Argument helpers
// ============================================================================

fn state(app: &AppHandle) -> tauri::State<'_, AppState> {
    app.state::<AppState>()
}

/// Run plugin work on the main thread.
///
/// The agent surface runs on its own HTTP thread, and a plugin that builds a Qt
/// application while it is instantiated asserts off the main dispatch queue.
/// The assert traps the process, so every tool that can load a module hops to
/// the main thread here instead of calling a loader directly.
fn on_main<T, F>(app: &AppHandle, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> Result<T, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let _ = tx.send(work(&state));
    })
    .map_err(|error| format!("plugin work could not reach the main thread: {error}"))?;
    await_main(&rx, MAIN_THREAD_TIMEOUT)
}

/// Wait for the main thread's answer, so a main thread that never runs the work
/// fails the call instead of pinning the connection thread forever.
fn await_main<T>(rx: &Receiver<Result<T, String>>, timeout: Duration) -> Result<T, String> {
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "main thread busy: plugin work got no answer within {} s; retry when the main thread is free",
            timeout.as_secs()
        )),
        Err(RecvTimeoutError::Disconnected) => {
            Err("plugin work ended without a result".to_string())
        }
    }
}

fn index(app: &AppHandle) -> tauri::State<'_, crate::plugin_index::IndexClient> {
    app.state::<crate::plugin_index::IndexClient>()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tauri::async_runtime::block_on(future)
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// The shapes `presets_set_params` accepts, spelled once for every refusal.
const PARAMS_EXPECTED: &str =
    "expected [name, value] pairs or an object map {name: value}, or a JSON string of either";
const PARAMS_OBJECT_EXAMPLE: &str = r#"e.g. {"amp.decay": "0.45s"}"#;
const PARAMS_PAIRS_EXAMPLE: &str = r#"e.g. [["amp.decay", "0.45s"]]"#;

/// `params` in the three shapes the tool description promises: an object map, an
/// array of `[name, value]` pairs, or a JSON string that decodes to either. Any
/// other shape is refused with what arrived and one correct example, so the
/// caller can fix the call instead of guessing.
fn params_to_pairs(value: &Value) -> Result<Vec<(String, Option<String>)>, String> {
    if let Some(items) = value.as_array() {
        return params_from_pairs(items);
    }
    if let Some(map) = value.as_object() {
        return Ok(map
            .iter()
            .map(|(name, value)| (name.clone(), value_to_optional_string(value)))
            .collect());
    }
    if let Some(text) = value.as_str() {
        let decoded: Value = serde_json::from_str(text).map_err(|_| {
            format!(
                "params: received a string that is not JSON, {PARAMS_EXPECTED}, {PARAMS_OBJECT_EXAMPLE}"
            )
        })?;
        if decoded.is_string() {
            return Err(format!(
                "params: received a JSON string of a string, {PARAMS_EXPECTED}, {PARAMS_OBJECT_EXAMPLE}"
            ));
        }
        return params_to_pairs(&decoded);
    }
    Err(format!(
        "params: received {}, {PARAMS_EXPECTED}, {PARAMS_OBJECT_EXAMPLE}",
        shape_name(value)
    ))
}

fn params_from_pairs(items: &[Value]) -> Result<Vec<(String, Option<String>)>, String> {
    let mut pairs = Vec::with_capacity(items.len());
    for item in items {
        let Some(pair) = item.as_array() else {
            return Err(format!(
                "params: received {}, {PARAMS_EXPECTED}, {PARAMS_PAIRS_EXAMPLE}",
                array_shape_name(items)
            ));
        };
        if pair.len() != 2 {
            return Err(format!(
                "params: received an array of {}-item arrays, {PARAMS_EXPECTED}, {PARAMS_PAIRS_EXAMPLE}",
                pair.len()
            ));
        }
        let Some(name) = pair[0].as_str() else {
            return Err(format!(
                "params: received a pair whose name is {}, {PARAMS_EXPECTED}, {PARAMS_PAIRS_EXAMPLE}",
                shape_name(&pair[0])
            ));
        };
        pairs.push((name.to_string(), value_to_optional_string(&pair[1])));
    }
    Ok(pairs)
}

fn array_shape_name(items: &[Value]) -> &'static str {
    if items.iter().all(Value::is_number) {
        "an array of numbers"
    } else if items.iter().all(Value::is_string) {
        "an array of strings"
    } else if items.iter().all(Value::is_array) {
        "an array of arrays"
    } else {
        "an array of mixed values"
    }
}

fn shape_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(items) => array_shape_name(items),
        Value::Object(_) => "an object",
    }
}

fn value_to_optional_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        other => Some(other.to_string()),
    }
}

// ============================================================================
// Dispatch
// ============================================================================

pub fn transaction_begin(
    state: &AppState,
    label: Option<String>,
    owner: &str,
) -> Result<Value, String> {
    let label = label.unwrap_or_else(|| "agent transaction".to_string());
    let revision = state.begin_transaction(label.clone(), owner.to_string())?;
    Ok(json!({
        "active": true,
        "label": label,
        "owner": owner,
        "revision": revision,
    }))
}

pub fn transaction_commit(state: &AppState, owner: &str) -> Result<Value, String> {
    guard_batch_owner(state, owner)?;
    let (label, revision) = state.commit_transaction()?;
    let inner = state.lock_inner();
    Ok(json!({
        "committed": true,
        "label": label,
        "revision": revision,
        "dirty": inner.dirty,
        "track_count": inner.project.tracks.len(),
    }))
}

pub fn transaction_rollback(state: &AppState, owner: &str) -> Result<Value, String> {
    guard_batch_owner(state, owner)?;
    let (label, revision) = state.rollback_transaction()?;
    let inner = state.lock_inner();
    Ok(json!({
        "rolled_back": true,
        "label": label,
        "revision": revision,
        "dirty": inner.dirty,
        "track_count": inner.project.tracks.len(),
    }))
}

/// Refuse to close a batch the caller did not open. A rollback by another
/// caller would discard edits it never owned, and a commit would publish them.
fn guard_batch_owner(state: &AppState, caller: &str) -> Result<(), String> {
    let Some(open) = state.batch_owner() else {
        return Ok(());
    };
    if open == caller {
        return Ok(());
    }
    Err(foreign_batch_error(&open, caller))
}

fn foreign_batch_error(open: &str, caller: &str) -> String {
    format!(
        "the open project transaction belongs to {open}; {caller} cannot edit or close it. \
         Wait for it to commit or roll back, then retry."
    )
}

/// Entry point shared by MCP and the panel. A refused call returns its error and
/// leaves the batch open, so the turn keeps the edits that already landed.
pub fn dispatch(app: &AppHandle, tool: &str, args: &Value, caller: &str) -> Result<Value, String> {
    dispatch_with_policy(app, tool, args, caller, InstallPolicy::External)
}

/// The panel's installation authority is selected by its trusted entry point,
/// independently of tool arguments or the conversation's batch owner.
pub(crate) fn dispatch_panel(
    app: &AppHandle,
    tool: &str,
    args: &Value,
    caller: &str,
) -> Result<Value, String> {
    dispatch_with_policy(app, tool, args, caller, InstallPolicy::Panel)
}

fn dispatch_with_policy(
    app: &AppHandle,
    tool: &str,
    args: &Value,
    caller: &str,
    install_policy: InstallPolicy,
) -> Result<Value, String> {
    let tool = tool.replace('.', "_");
    let state = state(app);
    guard_mutation(&state, &tool, args, caller)?;
    let mut result = dispatch_tool(app, &tool, args, caller, install_policy)?;
    // A call that published a document leaves the project file stale, and the
    // file is the durable record the next process reads. A write that failed is
    // put into the result, so the model does not report a save that did not happen.
    crate::project_persist::after_tool(&state, &tool, &mut result);
    let notification = external_project_notification(&state.lock_inner(), &tool, caller, &result);
    if let Some(notification) = notification
        && let Err(error) = app.emit("agent_project_changed", notification)
    {
        log::warn!("External project edit could not notify the GUI: {error}");
        if let Some(object) = result.as_object_mut() {
            object.insert("ui_sync_warning".into(), json!(error.to_string()));
        }
    }
    Ok(result)
}

/// External batches publish one committed snapshot; the panel publishes its
/// own snapshots after each tool and must not race a duplicate event.
fn external_project_notification(
    inner: &crate::state::AppStateInner,
    tool: &str,
    caller: &str,
    result: &Value,
) -> Option<Value> {
    if caller != HTTP_BATCH_OWNER
        || !is_mutation_tool(tool)
        || inner.transaction.is_some()
        || result.get("ok").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    Some(json!({
        "snapshot": commands::shared::snapshot(inner),
        "replaces_document": matches!(tool, "project_new" | "project_open"),
    }))
}

/// Reject a mutation planned against an older project version, or one a caller
/// that does not own the open batch sends. The refusal changed nothing, so it
/// must not touch the batch the call would have joined.
fn guard_mutation(state: &AppState, tool: &str, args: &Value, caller: &str) -> Result<(), String> {
    let Some(id) = ToolId::parse(tool).filter(|id| id.metadata().mutation) else {
        return Ok(());
    };
    // An open edit preview holds the document read-only; the registry's mutating
    // set is exactly the set this refusal covers.
    state.ensure_writable()?;
    if let Some(open) = state.batch_owner()
        && open != caller
    {
        return Err(foreign_batch_error(&open, caller));
    }
    let revision = crate::tool_registry::arguments::RevisionGuard::decode(id, args)
        .map_err(|error| error.to_string())?;
    state.lock_inner().check_revision(revision.expected())
}

/// The read-only skill tools answer from the loader alone. `/call`, `/mcp` and
/// the panel all reach them here, and none of them needs project state.
fn dispatch_skill(tool: ToolId, args: &Value) -> Option<Result<Value, String>> {
    crate::agent_tools::reference::dispatch(tool, args)
}

/// The `tracks_list` payload: one row per track, in project order.
pub fn tracks_list(state: &AppState) -> Result<Value, String> {
    let snapshot = crate::commands::snapshot(&state.lock_inner());
    let tracks: Vec<Value> = snapshot
        .project
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            json!({
                "index": index,
                "name": track.name,
                "preset": track.preset,
                "instrument": track.instrument.as_path(),
                "phrases": track.phrases.len(),
                "patterns": track.patterns.len() + track.inline_patterns.len(),
                "clips": track.pattern_clip_count(),
                "audio_clips": track.audio_clips().count(),
                "effects": track.effects.len(),
                "volume_db": track.mix.volume_db,
                "pan": track.mix.pan,
                "pan_law": if track.mix.stereo_balance { "balance" } else { "auto" },
                "mute": track.mix.mute,
                "solo": track.mix.solo,
                "output": track.mix.output,
            })
        })
        .collect();
    Ok(json!({ "tracks": tracks, "count": tracks.len() }))
}

/// The `analyze_notes` payload: no app state, so MCP and the panel share it.
/// Analyze attached media through the same decoder as imported audio clips.
/// Keep the existing WAV path unchanged, including its source channel count.
pub fn audio_analyze_tool(args: &Value) -> Result<Value, String> {
    crate::agent_tools::audio_analysis::dispatch(ToolId::AudioAnalyze, args)
}

pub(crate) fn audio_analyze_request(
    args: crate::agent_tools::audio_analysis::AnalyzeArgs,
) -> Result<Value, String> {
    let path = args.path;
    let beat_grid = args.beat_grid;
    let transients = args.transients;
    let level_window = args.level_window;
    if let Some(window) = args.mono_fold {
        if beat_grid.is_some() || transients.is_some() || level_window.is_some() {
            return Err("Choose one of mono_fold, level_window, beat_grid or transients".into());
        }
        let start = window.start.unwrap_or(0.0);
        let duration = window.duration;
        if !start.is_finite() || start < 0.0 || !(0.5..=30.0).contains(&duration) {
            return Err(
                "Mono fold analysis needs a nonnegative start and a duration of 0.5 to 30 seconds"
                    .into(),
            );
        }
        let (buffer, decoded, start) = read_comparison_input(&path, start, Some(duration))
            .map_err(|error| format!("Mono fold analysis failed: {error}"))?;
        let report = oc_engine::analysis::mono_fold_octaves(&buffer)?;
        let warnings = if decoded {
            vec![
                "Non-WAV media was decoded to two channels. The report describes that decoded pair.",
            ]
        } else {
            vec![]
        };
        return Ok(json!({
            "path": path,
            "window": { "start_s": start, "end_s": start + buffer.duration_s() },
            "sample_rate": buffer.sample_rate,
            "mono_fold": report,
            "warnings": warnings,
        }));
    }
    if let Some(window) = level_window {
        if beat_grid.is_some() || transients.is_some() {
            return Err("Choose one of level_window, beat_grid or transients".into());
        }
        let start = window.start.unwrap_or(0.0);
        let duration = window.duration;
        if !start.is_finite() || start < 0.0 || !(0.0 < duration && duration <= 30.0) {
            return Err(
                "Level analysis needs a nonnegative start and a duration up to 30 seconds".into(),
            );
        }
        let (buffer, decoded, start) = read_comparison_input(&path, start, Some(duration))
            .map_err(|error| format!("Level analysis failed: {error}"))?;
        let warnings = if decoded {
            vec!["Media decoded to stereo for level analysis."]
        } else {
            vec![]
        };
        return Ok(json!({
            "path": path,
            "level_window": { "start_s": start, "end_s": start + buffer.duration_s() },
            "level": oc_engine::analysis::level_metrics(&buffer),
            "sample_rate": buffer.sample_rate,
            "channels": buffer.channel_count(),
            "warnings": warnings,
        }));
    }
    if let Some(window) = transients {
        if beat_grid.is_some() {
            return Err("Choose either beat_grid or transients".into());
        }
        let start = window.start.unwrap_or(0.0);
        let duration = window.duration;
        let gap = window.min_gap_ms.unwrap_or(35.0);
        let limit = window.limit.unwrap_or(24);
        if !(start.is_finite()
            && start >= 0.0
            && 0.0 < duration
            && duration <= 20.0
            && (1.0..=250.0).contains(&gap)
            && (1..=128).contains(&limit))
        {
            return Err("Transient analysis needs a nonnegative start, up to 20 seconds, 1–250 ms spacing and 1–128 results".into());
        }
        let (buffer, _, actual) = read_passage_input(
            std::path::Path::new(&path),
            start,
            duration,
            oc_engine::audio_peaks::FILE_TRANSIENT_BUCKET_FRAMES,
        )
        .map_err(|error| format!("Transient analysis failed: {error}"))?;
        let evidence = oc_engine::audio_peaks::file_transients(
            &buffer,
            actual,
            start.max(actual),
            gap,
            limit,
        )?;
        return Ok(json!({ "path": path, "transients": evidence }));
    }
    if let Some(window) = beat_grid {
        let start = window.start.unwrap_or(0.0);
        let duration = window.duration;
        if !start.is_finite() || start < 0.0 || !(4.0..=30.0).contains(&duration) {
            return Err(
                "Beat grid needs a nonnegative start and a duration of 4 to 30 seconds".into(),
            );
        }
        let (buffer, _, start) = read_comparison_input(&path, start, Some(duration))
            .map_err(|error| format!("Beat grid analysis failed: {error}"))?;
        let evidence = oc_engine::analysis::beat_evidence::estimate(&buffer, start);
        return Ok(json!({
            "path": path,
            "beat_grid": evidence,
            "downbeat_confirmed": false,
            "time_signature": null,
            "guidance": "Scores rank pulse evidence and are not probabilities. Check a later passage and confirm the pulse, meter and downbeat before applying a project grid. A half- or double-time interpretation may fit the same attacks.",
        }));
    }
    let top_peaks: usize = args.top_peaks.unwrap_or(8);
    if top_peaks > 64 {
        return Err("top_peaks must be between 0 and 64".into());
    }
    let (buffer, decoded_media) = read_analysis_media(std::path::Path::new(&path))
        .map_err(|error| format!("analysis failed: {error}"))?;
    let mut report = oc_engine::analysis::analyze_buffer_with_peaks(&path, &buffer, top_peaks);
    if decoded_media {
        report.warnings.push("Media decoded to stereo for analysis; channels describes the analyzed signal, not the source channel count.".into());
    }
    to_value(report)
}

fn read_analysis_media(
    source: &std::path::Path,
) -> Result<(oc_engine::analysis::AudioBuffer, bool), String> {
    if source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        return oc_engine::analysis::read_wav(source).map(|buffer| (buffer, false));
    }
    let decoded = oc_engine::audio_clip::decode(source)?;
    Ok((
        stereo_analysis_buffer(decoded.sample_rate, &decoded.samples),
        true,
    ))
}

fn stereo_analysis_buffer(sample_rate: u32, samples: &[f32]) -> oc_engine::analysis::AudioBuffer {
    let mut left = Vec::with_capacity(samples.len() / 2);
    let mut right = Vec::with_capacity(samples.len() / 2);
    for frame in samples.chunks_exact(2) {
        left.push(f64::from(frame[0]));
        right.push(f64::from(frame[1]));
    }
    oc_engine::analysis::AudioBuffer {
        sample_rate,
        channels: vec![left, right],
    }
}

fn read_comparison_input(
    path: &str,
    start: f64,
    duration: Option<f64>,
) -> Result<(oc_engine::analysis::AudioBuffer, bool, f64), String> {
    let path = std::path::Path::new(path);
    let Some(duration) = duration else {
        return read_analysis_media(path).map(|(buffer, decoded)| (buffer, decoded, 0.0));
    };
    read_passage_input(path, start, duration, 0)
}

fn read_passage_input(
    path: &std::path::Path,
    start: f64,
    duration: f64,
    lead_frames: usize,
) -> Result<(oc_engine::analysis::AudioBuffer, bool, f64), String> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return oc_engine::analysis::read_wav_window_with_lead(path, start, duration, lead_frames)
            .map(|(buffer, actual)| (buffer, false, actual));
    }
    let passage =
        oc_engine::audio_clip::decode_passage_with_lead(path, start, duration, lead_frames)?;
    let rate = passage.audio.sample_rate;
    if passage.audio.frames() != passage.requested_frames {
        let total = passage
            .source_frames
            .expect("a short decode reached the end");
        return Err(format!(
            "Audio passage exceeds the source duration of {:.6} s",
            total as f64 / f64::from(rate)
        ));
    }
    let actual = passage.first_frame as f64 / f64::from(rate);
    Ok((
        stereo_analysis_buffer(rate, &passage.audio.samples),
        true,
        actual,
    ))
}

pub fn audio_compare_tool(args: &Value) -> Result<Value, String> {
    crate::agent_tools::audio_analysis::dispatch(ToolId::AudioCompare, args)
}

pub(crate) fn audio_compare_request(
    args: crate::agent_tools::audio_analysis::CompareArgs,
) -> Result<Value, String> {
    let reference = args.reference;
    let candidate = args.candidate;
    let options = oc_engine::analysis::CompareOptions {
        stems: args.stems.unwrap_or(false),
        allow_mismatch: args.allow_mismatch.unwrap_or(false),
    };
    let reference_start = args.reference_start;
    let candidate_start = args.candidate_start;
    let duration = args.duration;
    let listen: bool = args.listen.unwrap_or(false);
    let listening_only: bool = args.listening_only.unwrap_or(false);
    if listening_only && !listen {
        return Err("listening_only requires listen=true".into());
    }
    let listening_format = if listen {
        if !duration.is_some_and(|seconds| (0.4..=commands::PAIR_MAX_SECONDS).contains(&seconds)) {
            return Err("Paired listening requires a duration of 0.4 to 8 seconds".into());
        }
        let channel = commands::AuditionChannel::parse(&args.channel.unwrap_or_default())?;
        let rate = args
            .clip_sample_rate
            .unwrap_or(commands::AUDITION_SAMPLE_RATE);
        Some((channel, rate.clamp(8_000, 48_000)))
    } else {
        None
    };
    if duration.is_none() && (reference_start.is_some() || candidate_start.is_some()) {
        return Err("Set duration when selecting comparison offsets".into());
    }
    if duration.is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0 || seconds > 30.0) {
        return Err("Comparison duration must be greater than 0 and at most 30 seconds".into());
    }
    if [reference_start, candidate_start]
        .into_iter()
        .flatten()
        .any(|start| !start.is_finite() || start < 0.0)
    {
        return Err("Comparison starts must be finite and nonnegative".into());
    }
    let (reference_buffer, reference_decoded, reference_start) =
        read_comparison_input(&reference, reference_start.unwrap_or(0.0), duration)
            .map_err(|error| format!("comparison reference failed: {error}"))?;
    let (candidate_buffer, candidate_decoded, candidate_start) =
        read_comparison_input(&candidate, candidate_start.unwrap_or(0.0), duration)
            .map_err(|error| format!("comparison candidate failed: {error}"))?;
    let mut result = if listening_only {
        json!({"listening_only": true, "reference": {"path": reference}, "candidate": {"path": candidate}})
    } else {
        let mut report = oc_engine::analysis::compare_buffers(
            &reference,
            &reference_buffer,
            &candidate,
            &candidate_buffer,
            options,
        )
        .map_err(|error| format!("comparison failed: {error}"))?;
        for (role, decoded) in [
            ("Reference", reference_decoded),
            ("Candidate", candidate_decoded),
        ] {
            if decoded {
                report.warnings.push(format!("{role} media decoded to stereo for comparison; channels describes the analyzed signal, not the source channel count."));
            }
        }
        to_value(report)?
    };
    if duration.is_some() {
        result["window"] = json!({
            "reference_start_s": reference_start,
            "reference_end_s": reference_start + reference_buffer.duration_s(),
            "candidate_start_s": candidate_start,
            "candidate_end_s": candidate_start + candidate_buffer.duration_s(),
        });
    }
    if let Some((channel, rate)) = listening_format {
        result["listening"] = to_value(commands::listening_pair(
            &reference_buffer,
            &candidate_buffer,
            channel,
            rate,
        )?)?;
    }
    Ok(result)
}

pub fn analyze_notes_tool(args: &Value) -> Result<Value, String> {
    crate::agent_tools::audio_analysis::dispatch(ToolId::AnalyzeNotes, args)
}

pub(crate) fn analyze_notes_request(
    args: crate::agent_tools::audio_analysis::NotesArgs,
) -> Result<Value, String> {
    let path = args.path;
    let resolved = crate::beat_grid::resolve(&path, args.tempo, args.time_signature, args.project)?;
    let options = oc_engine::note_analysis::NoteOptions {
        start_s: args.start,
        end_s: args.end,
        min_confidence: args.min_confidence.unwrap_or(0.0),
        max_notes: args.max_notes,
        grid: resolved.grid,
        quantize: args.quantize.unwrap_or(false),
    };
    let mut report = oc_engine::note_analysis::analyze_notes(&path, &options)
        .map_err(|error| format!("note analysis failed: {error}"))?;
    report.warnings.splice(0..0, resolved.warnings);
    to_value(report)
}

/// Separation CLIs `separate_stems` can drive, with the arguments before the
/// output directory and the source path.
const SEPARATION_CLIS: [(&str, &[&str]); 2] =
    [("demucs", &["-o"]), ("spleeter", &["separate", "-o"])];

/// What every band-mode response says about itself.
const BAND_STEM_WARNING: &str =
    "band mode is a 200 Hz low/high filter split — an approximation, not a source separation";

/// The `separate_stems` payload: no app state, so MCP and the panel share it.
pub fn separate_stems_tool(args: &Value) -> Result<Value, String> {
    let request = SeparateStemsArgs::decode(args).map_err(|error| error.to_string())?;
    separate_stems_cancellable_request(&request, None)
}

pub(crate) fn separate_stems_cancellable(
    args: &Value,
    cancel: Option<&AtomicBool>,
) -> Result<Value, String> {
    let request = SeparateStemsArgs::decode(args).map_err(|error| error.to_string())?;
    separate_stems_cancellable_request(&request, cancel)
}

fn separate_stems_cancellable_request(
    request: &SeparateStemsArgs,
    cancel: Option<&AtomicBool>,
) -> Result<Value, String> {
    separator_process::check_cancelled(cancel)?;
    let managed = if request.mode == SeparateStemsMode::Band {
        None
    } else {
        crate::plugin_store::separator_runtime::resolve(request.runtime_id.as_deref())
    };
    separate_stems_with_provider_cancellable(
        request,
        std::env::var_os("PATH").as_deref(),
        managed.as_ref(),
        cancel,
    )
}

#[cfg(test)]
fn separate_stems_with(
    args: &Value,
    search_path: Option<&std::ffi::OsStr>,
) -> Result<Value, String> {
    let request = SeparateStemsArgs::decode(args).map_err(|error| error.to_string())?;
    separate_stems_with_provider_cancellable(&request, search_path, None, None)
}

#[cfg(test)]
fn separate_stems_with_provider(
    args: &Value,
    search_path: Option<&std::ffi::OsStr>,
    managed: Option<&crate::plugin_store::separator_runtime::ResolvedRuntime>,
) -> Result<Value, String> {
    let request = SeparateStemsArgs::decode(args).map_err(|error| error.to_string())?;
    separate_stems_with_provider_cancellable(&request, search_path, managed, None)
}

fn separate_stems_with_provider_cancellable(
    request: &SeparateStemsArgs,
    search_path: Option<&std::ffi::OsStr>,
    managed: Option<&crate::plugin_store::separator_runtime::ResolvedRuntime>,
    cancel: Option<&AtomicBool>,
) -> Result<Value, String> {
    separator_process::check_cancelled(cancel)?;
    let path = request.path.clone();
    let mode = request.mode;
    if let Some(id) = request.runtime_id.as_deref() {
        if mode == SeparateStemsMode::Band {
            return Err("runtime_id cannot be used with band mode".into());
        }
        if managed.is_none_or(|runtime| runtime.id != id) {
            return Err(format!(
                "refused: managed separator '{id}' is not ready; use plugin_search and install it with plugin_pull"
            ));
        }
    }
    let source = std::path::Path::new(&path);
    if !source.is_file() {
        return Err(format!("could not read {}: no such file", source.display()));
    }
    let (out_dir, implicit_out_dir) = match request.out_dir.as_deref() {
        Some(dir) => (std::path::PathBuf::from(dir), None),
        None => {
            let directory = tempfile::Builder::new()
                .prefix("opencompose-stems-")
                .tempdir()
                .map_err(|error| format!("could not create stem output directory: {error}"))?;
            (directory.path().to_path_buf(), Some(directory))
        }
    };
    std::fs::create_dir_all(&out_dir)
        .map_err(|error| format!("could not create out_dir {}: {error}", out_dir.display()))?;
    let provider = managed
        .map(|runtime| {
            let mut command = runtime.command();
            command.args(["separate", "-o"]);
            ("spleeter", command)
        })
        .or_else(|| {
            separation_cli(search_path).map(|(name, prefix, program)| {
                let mut command = oc_host::child_process::CommandSpec::new(program);
                command.args(prefix);
                (name, command)
            })
        });
    let decoded_input = if managed.is_some()
        && mode != SeparateStemsMode::Band
        && !source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        Some(decode_separation_input(source, &out_dir)?)
    } else {
        None
    };
    let input_source = decoded_input
        .as_ref()
        .map(|input| input.path.as_path())
        .unwrap_or(source);
    separator_process::check_cancelled(cancel)?;
    let mut warnings: Vec<String> = Vec::new();
    let mode = if request.runtime_id.is_some() {
        SeparateStemsMode::External
    } else {
        mode
    };
    let (method, stems) = match mode {
        SeparateStemsMode::Band => (
            "band".to_owned(),
            band_stems(input_source, &out_dir, &mut warnings, cancel)?,
        ),
        SeparateStemsMode::External => {
            let Some((name, command)) = provider else {
                return Err("refused: no separation CLI on PATH (looked for demucs, spleeter); install one, or call separate_stems with mode \"band\" for the approximate built-in split".to_owned());
            };
            let stems = run_separation(
                name,
                command,
                input_source,
                &out_dir,
                managed.is_some(),
                cancel,
            )
            .map_err(|error| format!("refused: external separation with {name} failed: {error}"))?;
            (format!("external:{name}"), stems)
        }
        _ => match provider {
            Some((name, command)) => {
                match run_separation(
                    name,
                    command,
                    input_source,
                    &out_dir,
                    managed.is_some(),
                    cancel,
                ) {
                    Ok(stems) => (format!("external:{name}"), stems),
                    Err(error) => {
                        separator_process::check_cancelled(cancel)?;
                        warnings.push(format!(
                            "{name} failed ({error}); using the band split instead"
                        ));
                        (
                            "band".to_owned(),
                            band_stems(input_source, &out_dir, &mut warnings, cancel)?,
                        )
                    }
                }
            }
            None => (
                "band".to_owned(),
                band_stems(input_source, &out_dir, &mut warnings, cancel)?,
            ),
        },
    };
    if let Some(grid) = crate::render_grid::read(source) {
        for stem in &stems {
            if let Some(stem_path) = stem["path"].as_str() {
                crate::render_grid::write(std::path::Path::new(stem_path), &grid);
            }
        }
    }
    if let Some(directory) = implicit_out_dir {
        let _ = directory.keep();
    }
    Ok(json!({
        "path": request.path,
        "input_conversion": decoded_input.as_ref().map(|input| json!({"format":"wav", "sample_rate":input.sample_rate, "channels":2, "frames":input.frames, "temporary":true})),
        "method": method,
        "runtime_id": if method == "external:spleeter" { managed.map(|runtime| runtime.id.as_str()) } else { None },
        "runtime_version": if method == "external:spleeter" { managed.map(|runtime| runtime.version.as_str()) } else { None },
        "stems": stems,
        "warnings": warnings,
    }))
}

struct SeparationInput {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    sample_rate: u32,
    frames: usize,
}
fn decode_separation_input(
    source: &std::path::Path,
    out_dir: &std::path::Path,
) -> Result<SeparationInput, String> {
    if source
        .metadata()
        .map_err(|e| format!("Cannot read separation source: {e}"))?
        .len()
        > 256 * 1024 * 1024
    {
        return Err("Compressed separation source exceeds the 256 MiB input limit".into());
    }
    let decoded = oc_engine::audio_clip::decode(source)
        .map_err(|e| format!("Cannot decode separation source {}: {e}", source.display()))?;
    if decoded.samples.len() > 512 * 1024 * 1024 / std::mem::size_of::<f32>() {
        return Err("Decoded separation source exceeds the 512 MiB PCM limit".into());
    }
    let directory = tempfile::Builder::new()
        .prefix(".decoded-")
        .tempdir_in(out_dir)
        .map_err(|e| format!("Cannot stage decoded separation input: {e}"))?;
    let mut filename = source
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("source"))
        .to_os_string();
    filename.push(".wav");
    let path = directory.path().join(filename);
    let input = SeparationInput {
        _directory: directory,
        path,
        sample_rate: decoded.sample_rate,
        frames: decoded.frames(),
    };
    let mut writer = hound::WavWriter::create(
        &input.path,
        hound::WavSpec {
            channels: 2,
            sample_rate: decoded.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(|e| format!("Cannot create decoded separation WAV: {e}"))?;
    for &sample in decoded.samples.iter() {
        writer
            .write_sample(sample)
            .map_err(|e| format!("Cannot write decoded separation WAV: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("Cannot finalize decoded separation WAV: {e}"))?;
    Ok(input)
}

/// The first separation CLI on `search_path`, in preference order.
fn separation_cli(
    search_path: Option<&std::ffi::OsStr>,
) -> Option<(&'static str, &'static [&'static str], std::path::PathBuf)> {
    let search = search_path?;
    for &(name, prefix) in &SEPARATION_CLIS {
        for dir in std::env::split_paths(search) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            if is_executable(&candidate) {
                return Some((name, prefix, candidate));
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Run one provider in a fresh directory under `out_dir` and return the WAVs
/// it produced, so a previous run's stems are never reported again.
fn run_separation(
    name: &str,
    mut command: oc_host::child_process::CommandSpec,
    source: &std::path::Path,
    out_dir: &std::path::Path,
    managed: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, String> {
    let run_dir = stage_stems(out_dir)?;
    command.arg(run_dir.path()).arg(source);
    separator_process::run(name, command, separator_process::RUN_TIMEOUT, cancel)?;
    let files = wav_files_under(run_dir.path());
    if files.is_empty() {
        return Err(format!(
            "{name} produced no WAV files under {}",
            run_dir.path().display()
        ));
    }
    let stems = files
        .into_iter()
        .map(|file| {
            let stem = file
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "stem".to_owned());
            let buffer = oc_engine::analysis::read_wav(&file)
                .map_err(|error| format!("{name} produced an invalid stem: {error}"))?;
            if buffer.frames() == 0 {
                return Err(format!("{name} produced an empty stem: {}", file.display()));
            }
            let peak = oc_engine::stems::peak_dbfs(&buffer.channels);
            Ok(json!({ "name": stem, "path": file.to_string_lossy(), "peak_dbfs": peak }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if managed
        && (stems.len() != 2
            || !["vocals", "accompaniment"]
                .iter()
                .all(|name| stems.iter().any(|stem| stem["name"] == *name)))
    {
        return Err(format!(
            "{name} did not produce the required vocals and accompaniment stems"
        ));
    }
    separator_process::check_cancelled(cancel)?;
    let _ = run_dir.keep();
    Ok(stems)
}

fn stage_stems(out_dir: &std::path::Path) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("separate-")
        .tempdir_in(out_dir)
        .map_err(|error| {
            format!(
                "could not stage separation in {}: {error}",
                out_dir.display()
            )
        })
}

fn wav_files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, depth: usize, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth < 4 {
                    walk(&path, depth + 1, files);
                }
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
            {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, 0, &mut files);
    files.sort();
    files
}

/// The band fallback: low, high, and the mid for a stereo source.
fn band_stems(
    source: &std::path::Path,
    out_dir: &std::path::Path,
    warnings: &mut Vec<String>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, String> {
    separator_process::check_cancelled(cancel)?;
    let buffer = oc_engine::analysis::read_wav(source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?;
    let split = oc_engine::stems::band_split(&buffer)?;
    separator_process::check_cancelled(cancel)?;
    let run_dir = stage_stems(out_dir)?;
    warnings.push(BAND_STEM_WARNING.to_owned());
    let mut stems = vec![
        write_band_stem(run_dir.path(), "low", &split.low, split.sample_rate)?,
        write_band_stem(run_dir.path(), "high", &split.high, split.sample_rate)?,
    ];
    if let Some(centre) = &split.centre {
        warnings
            .push("centre is the mono mid (L+R)/2, not a phase-based centre extractor".to_owned());
        stems.push(write_band_stem(
            run_dir.path(),
            "centre",
            centre,
            split.sample_rate,
        )?);
    }
    separator_process::check_cancelled(cancel)?;
    let _ = run_dir.keep();
    Ok(stems)
}

fn write_band_stem(
    out_dir: &std::path::Path,
    name: &str,
    channels: &[Vec<f64>],
    sample_rate: u32,
) -> Result<Value, String> {
    let path = out_dir.join(format!("{name}.wav"));
    let peak = oc_engine::stems::peak_dbfs(channels);
    oc_engine::stems::write_stem_wav(&path, channels, sample_rate)?;
    Ok(json!({ "name": name, "path": path.to_string_lossy(), "peak_dbfs": peak }))
}

// ============================================================================
// Producer tools: tempo and meter maps, track and preset copies, history, stems
// ============================================================================

fn describe_tempo_point(point: &oc_dsl::ast::TempoPoint) -> String {
    format!(
        "{}:{} = {} BPM",
        point.position.bar, point.position.beat, point.bpm
    )
}

/// A rejected map plus the points that were sent, so the caller can see which.
fn map_refusal(error: String, described: &[String]) -> String {
    if described.is_empty() {
        error
    } else {
        format!("{error} (points: {})", described.join(", "))
    }
}

fn tempo_point_values(points: &[oc_dsl::ast::TempoPoint]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|point| {
                json!({
                    "bar": point.position.bar,
                    "beat": point.position.beat,
                    "subdivision": point.position.subdivision,
                    "offset_quarters": point.offset_quarters,
                    "bpm": point.bpm,
                })
            })
            .collect(),
    )
}

fn meter_point_values(points: &[oc_dsl::ast::TimeSigPoint]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|point| {
                json!({
                    "bar": point.position.bar,
                    "numerator": point.numerator,
                    "denominator": point.denominator,
                })
            })
            .collect(),
    )
}

/// `tempo_map_set`: write the map, then read it back from the project.
pub fn tempo_map_set(
    state: &AppState,
    request: &crate::agent_tools::transport::TempoMapSetArgs,
) -> Result<Value, String> {
    let points = request.points();
    let described: Vec<String> = points.iter().map(describe_tempo_point).collect();
    let snapshot = commands::set_tempo_map_inner(&mut state.lock_inner(), points)
        .map_err(|error| map_refusal(error, &described))?;
    Ok(json!({
        "tempo": snapshot.project.properties.tempo,
        "tempo_map": tempo_point_values(&snapshot.project.properties.tempo_map),
        "revision": snapshot.revision,
    }))
}

/// `time_signature_set`: write the meter map, then read it back.
pub fn time_signature_set(
    state: &AppState,
    request: &crate::agent_tools::transport::TimeSignatureSetArgs,
) -> Result<Value, String> {
    let points = request.points();
    let described: Vec<String> = points
        .iter()
        .map(|point| {
            format!(
                "bar {} = {}/{}",
                point.position.bar, point.numerator, point.denominator
            )
        })
        .collect();
    let snapshot = commands::set_time_sig_map_inner(&mut state.lock_inner(), points)
        .map_err(|error| map_refusal(error, &described))?;
    let properties = &snapshot.project.properties;
    Ok(json!({
        "time_signature": {
            "numerator": properties.time_signature.numerator,
            "denominator": properties.time_signature.denominator,
        },
        "time_sig_map": meter_point_values(&properties.time_sig_map),
        "revision": snapshot.revision,
    }))
}

fn tempo_map_set_request(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let request = crate::agent_tools::transport::TempoMapSetArgs::decode(args)
        .map_err(|error| error.to_string())?;
    tempo_map_set(state, &request)
}

fn time_signature_set_request(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let request = crate::agent_tools::transport::TimeSignatureSetArgs::decode(args)
        .map_err(|error| error.to_string())?;
    time_signature_set(state, &request)
}

/// `tracks_duplicate`: the copy keeps the sound and gets its own preset.
pub fn tracks_duplicate(
    state: &AppState,
    request: &crate::agent_tools::tracks::TracksDuplicateArgs,
) -> Result<Value, String> {
    let snapshot =
        commands::duplicate_track_inner(&mut state.lock_inner(), request.index, &request.name)?;
    let copy_index = snapshot.project.tracks.len() - 1;
    let copy = &snapshot.project.tracks[copy_index];
    Ok(json!({
        "index": copy_index,
        "name": copy.name,
        "preset": copy.preset,
        "track_count": snapshot.project.tracks.len(),
        "revision": snapshot.revision,
    }))
}

#[cfg(test)]
fn tracks_duplicate_json(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let request = crate::agent_tools::tracks::TracksDuplicateArgs::decode(args)
        .map_err(|error| error.to_string())?;
    tracks_duplicate(state, &request)
}

/// The references a preset rename rewrites: tracks, then patterns.
fn preset_details(project: &oc_dsl::ast::Project, name: &str) -> Result<Value, String> {
    let authored = project
        .presets
        .iter()
        .find(|preset| preset.name == name)
        .ok_or_else(|| format!("Unknown preset {name}"))?;
    let effective = oc_dsl::preset::resolve_preset(&project.presets, name)?;
    Ok(json!({ "presets": [authored], "effective_preset": effective }))
}

fn preset_references(state: &AppState, name: &str) -> (Vec<String>, Vec<String>) {
    let inner = state.lock_inner();
    let project = &inner.project;
    let tracks: Vec<String> = project
        .tracks
        .iter()
        .filter(|track| track.preset.as_deref() == Some(name))
        .map(|track| track.name.clone())
        .collect();
    let mut patterns: Vec<String> = project
        .patterns
        .iter()
        .filter(|pattern| pattern.preset.as_deref() == Some(name))
        .map(|pattern| pattern.name.clone())
        .collect();
    for track in &project.tracks {
        for pattern in &track.inline_patterns {
            if pattern.preset.as_deref() == Some(name) {
                patterns.push(format!("{}:{}", track.name, pattern.name));
            }
        }
    }
    (tracks, patterns)
}

/// `presets_rename`: one edit renames the preset and every reference to it.
pub fn presets_rename(
    state: &AppState,
    request: &crate::agent_tools::presets::PresetsRenameArgs,
) -> Result<Value, String> {
    let (tracks, patterns) = preset_references(state, &request.name);
    let snapshot =
        commands::rename_preset_inner(&mut state.lock_inner(), &request.name, &request.new_name)?;
    Ok(json!({
        "name": request.new_name.trim(),
        "renamed_from": request.name,
        "tracks": tracks,
        "patterns": patterns,
        "revision": snapshot.revision,
    }))
}

#[cfg(test)]
fn presets_rename_json(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let request = crate::agent_tools::presets::PresetsRenameArgs::decode(args)
        .map_err(|error| error.to_string())?;
    presets_rename(state, &request)
}

/// `presets_duplicate`: a copy, optionally taking the references with it.
pub fn presets_duplicate(
    state: &AppState,
    request: &crate::agent_tools::presets::PresetsDuplicateArgs,
) -> Result<Value, String> {
    let reassign = request.reassign.unwrap_or(true);
    let snapshot = commands::duplicate_preset_inner(
        &mut state.lock_inner(),
        &request.name,
        &request.new_name,
        reassign,
    )?;
    Ok(json!({
        "name": request.new_name.trim(),
        "copied_from": request.name,
        "reassigned": reassign,
        "revision": snapshot.revision,
    }))
}

#[cfg(test)]
fn presets_duplicate_json(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let request = crate::agent_tools::presets::PresetsDuplicateArgs::decode(args)
        .map_err(|error| error.to_string())?;
    presets_duplicate(state, &request)
}

/// `undo_history`: the stack as a list; the cursor never moves.
pub fn undo_history(state: &AppState) -> Result<Value, String> {
    let entries = state.lock_inner().undo.history();
    let applied = entries.iter().filter(|entry| entry.applied).count();
    let redoable = entries.len() - applied;
    Ok(json!({
        "entries": serde_json::to_value(&entries).map_err(|error| error.to_string())?,
        "applied": applied,
        "redoable": redoable,
    }))
}

/// The stem set the export published, flattened with a path per file.
fn stems_payload(directory: &str, result: Value) -> Result<Value, String> {
    let manifest_path = result["manifest"]
        .as_str()
        .ok_or("the stem export returned no manifest")?
        .to_string();
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read '{manifest_path}': {error}"))?;
    let mut manifest: Value = serde_json::from_str(&text)
        .map_err(|error| format!("'{manifest_path}' is not valid JSON: {error}"))?;
    let base = std::path::Path::new(directory);
    let with_path = |slot: &mut Value| {
        if let Some(file) = slot["file"].as_str() {
            slot["path"] = json!(base.join(file).display().to_string());
        }
    };
    with_path(&mut manifest["mix"]);
    if let Some(stems) = manifest["stems"].as_array_mut() {
        for stem in stems {
            with_path(stem);
        }
    }
    Ok(json!({
        "directory": directory,
        "manifest": manifest_path,
        "sample_rate": manifest["sample_rate"],
        "channels": manifest["channels"],
        "container": manifest["container"],
        "sample_format": manifest["sample_format"],
        "dithered": manifest["dithered"],
        "frames": manifest["frames"],
        "routing": manifest["routing"],
        "mix": manifest["mix"],
        "stems": manifest["stems"],
        "loudness": result["loudness"],
    }))
}

/// `render_stems`: the toolbar's Export stems, for an existing directory.
pub async fn render_stems(state: &AppState, args: &Value) -> Result<Value, String> {
    use crate::tool_registry::arguments::ToolArguments;
    let directory = crate::agent_tools::render::RenderStemsArgs::decode(args)
        .map_err(|error| error.to_string())?
        .directory;
    if !std::path::Path::new(&directory).is_dir() {
        return Err(format!(
            "render_stems needs an existing directory to write the stem set into; '{directory}' is not a directory"
        ));
    }
    let result = commands::render_stems_off_thread(
        state,
        directory.clone(),
        None,
        commands::render_stems_snapshot,
    )
    .await?;
    stems_payload(&directory, result)
}

fn audio_clip_set_placement(
    state: &AppState,
    request: &crate::agent_tools::audio_clip::AudioClipSetPlacementArgs,
    expected_revision: Option<u64>,
    caller: &str,
) -> Result<Value, String> {
    let mut inner = state.lock_inner();
    if let Some(transaction) = &inner.transaction
        && transaction.owner != caller
    {
        return Err(foreign_batch_error(&transaction.owner, caller));
    }
    inner.check_revision(expected_revision)?;
    commands::set_audio_clip_placement_inner(
        &mut inner,
        request.clip_index,
        request.start_seconds,
        request.nudge_ms,
    )?;
    Ok(
        json!({"revision": inner.revision, "clip_index": request.clip_index,
        "clip": inner.project.audio_clip(request.clip_index)}),
    )
}

fn audio_clip_set_placement_request(
    state: &AppState,
    args: &Value,
    caller: &str,
) -> Result<Value, String> {
    use crate::tool_registry::arguments::{RevisionGuard, ToolArguments};
    let request = crate::agent_tools::audio_clip::AudioClipSetPlacementArgs::decode(args)
        .map_err(|error| error.to_string())?;
    let guard = RevisionGuard::decode(ToolId::AudioClipSetPlacement, args)
        .map_err(|error| error.to_string())?;
    audio_clip_set_placement(state, &request, guard.expected(), caller)
}

fn audio_clip_set_playback(
    state: &AppState,
    request: crate::agent_tools::audio_clip::AudioClipSetPlaybackArgs,
    expected_revision: Option<u64>,
    caller: &str,
) -> Result<Value, String> {
    let clip_index = request.clip_index;
    let playback = request.playback.into();
    let guard = |inner: &crate::state::AppStateInner| {
        if let Some(transaction) = &inner.transaction
            && transaction.owner != caller
        {
            return Err(foreign_batch_error(&transaction.owner, caller));
        }
        inner.check_revision(expected_revision)
    };
    let edit = {
        let inner = state.lock_inner();
        guard(&inner)?;
        commands::ClipPlaybackEdit::capture(&inner)
    };
    let edit = edit.prepare(clip_index, playback)?;
    let mut inner = state.lock_inner();
    guard(&inner)?;
    edit.commit(&mut inner)?;
    Ok(json!({
        "revision": inner.revision,
        "clip_index": clip_index,
        "clip": inner.project.audio_clip(clip_index),
    }))
}

fn audio_clip_set_playback_request(
    state: &AppState,
    args: &Value,
    caller: &str,
) -> Result<Value, String> {
    use crate::tool_registry::arguments::{RevisionGuard, ToolArguments};
    let request = crate::agent_tools::audio_clip::AudioClipSetPlaybackArgs::decode(args)
        .map_err(|error| error.to_string())?;
    let guard = RevisionGuard::decode(ToolId::AudioClipSetPlayback, args)
        .map_err(|error| error.to_string())?;
    audio_clip_set_playback(state, request, guard.expected(), caller)
}

fn effects_set_bypass(
    state: &AppState,
    request: &crate::agent_tools::effects::EffectsSetBypassArgs,
    expected_revision: Option<u64>,
    caller: &str,
) -> Result<Value, String> {
    let mut inner = state.lock_inner();
    if let Some(transaction) = &inner.transaction
        && transaction.owner != caller
    {
        return Err(foreign_batch_error(&transaction.owner, caller));
    }
    inner.check_revision(expected_revision)?;
    let effect = commands::set_effect_bypass_inner(
        &mut inner,
        &request.target,
        request.index,
        request.bypass,
    )?;
    let effect = match &effect {
        oc_dsl::ast::EffectType::Plugin(id) => id,
        builtin => builtin.lane_name(),
    };
    Ok(json!({
        "target": request.target,
        "index": request.index,
        "effect": effect,
        "bypass": request.bypass,
        "revision": inner.revision,
    }))
}

#[cfg(test)]
fn effects_set_bypass_json(state: &AppState, args: &Value, caller: &str) -> Result<Value, String> {
    let request = crate::agent_tools::effects::EffectsSetBypassArgs::decode(args)
        .map_err(|error| error.to_string())?;
    let guard =
        crate::tool_registry::arguments::RevisionGuard::decode(ToolId::EffectsSetBypass, args)
            .map_err(|error| error.to_string())?;
    effects_set_bypass(state, &request, guard.expected(), caller)
}

fn conversation_audio_source(
    store: &crate::ai::sessions::Store,
    caller: &str,
    path: &str,
) -> Result<(std::path::PathBuf, bool), String> {
    use crate::ai::sessions::audition_render_reference;
    use std::path::Path;

    let refusal = "Use a local audio reference or saved render from the current conversation, or use clip_index";
    let id = caller
        .strip_prefix(crate::state::PANEL_BATCH_OWNER_PREFIX)
        .filter(|_| Path::new(path).is_absolute())
        .ok_or(refusal)?;
    let find = |messages: &[Value]| {
        if messages
            .iter()
            .any(|row| audition_render_reference(row) == Some(path))
        {
            return Some(true);
        }
        messages
            .iter()
            .any(|message| {
                message["media"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|entry| {
                        entry["kind"] == "audio"
                            && entry["local_reference"] == true
                            && entry["path"].as_str() == Some(path)
                    })
            })
            .then_some(false)
    };
    let archived = store
        .view(id)
        .ok()
        .and_then(|messages| find(&messages))
        .or_else(|| {
            store
                .original(id)
                .ok()
                .and_then(|(messages, _)| find(&messages))
        })
        .ok_or(refusal)?;
    if archived {
        return commands::saved_audition_render_path(store.root(), Path::new(path))
            .map(|path| (path, true));
    }
    let path = std::path::PathBuf::from(path);
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Cannot open the local audio reference: {error}"))?;
    if !metadata.is_file() || metadata.len() > crate::ai::media::MAX_LOCAL_AUDIO_BYTES {
        return Err("The local audio reference is no longer a supported file size".into());
    }
    Ok((path, false))
}

fn dispatch_tool(
    app: &AppHandle,
    tool: &str,
    args: &Value,
    caller: &str,
    install_policy: InstallPolicy,
) -> Result<Value, String> {
    let id = ToolId::parse(tool).ok_or_else(|| format!("unknown tool '{tool}'"))?;
    match id {
        ToolId::ListSkills | ToolId::ReadSkill => dispatch_skill(id, args)
            .ok_or_else(|| format!("No reference handler for '{}'", id.name()))?,
        ToolId::EditSubtree => Err(format!("unknown tool '{}'", id.name())),
        // ---- system -----------------------------------------------------
        ToolId::SystemCapabilities => {
            let _ = crate::agent_tools::misc::SystemCapabilitiesArgs::decode(args)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "service": "opencompose-agent",
                "version": VERSION,
                "platform": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "plugin_formats": ["clap", "vst3", "au (metadata only)"],
                "audio_devices": commands::list_audio_devices(state(app)),
            }))

            // ---- saved conversations ---------------------------------------
        }
        ToolId::SessionsList => {
            let request = crate::agent_tools::session::SessionsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::commands::list_for(
                &state(app),
                request.project.as_deref(),
                request.all.unwrap_or(false),
            )?)
        }
        ToolId::SessionsNew => {
            let request = crate::agent_tools::session::SessionsNewArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::commands::new_for(
                &state(app),
                request.project.as_deref(),
                request.title.as_deref(),
                request.parent.as_deref(),
            )?)
        }
        ToolId::SessionsLoad => {
            let request = crate::agent_tools::session::SessionsLoadArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::commands::sessions_load(request.id)?)
        }
        ToolId::SessionsRename => {
            let request = crate::agent_tools::session::SessionsRenameArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::commands::sessions_rename(request.id, request.title)?)
        }
        ToolId::SessionsDelete => {
            let request = crate::agent_tools::session::SessionsDeleteArgs::decode(args)
                .map_err(|error| error.to_string())?;
            crate::commands::sessions_delete(request.id)?;
            Ok(json!({ "deleted": true }))
        }
        ToolId::SessionsCompact => {
            let request = crate::agent_tools::session::SessionsCompactArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let id = request.id;
            let target_tokens = request.target_tokens;
            to_value(crate::commands::compact_for(
                &state(app),
                &id,
                target_tokens,
            )?)
        }

        // ---- transactions / revision -----------------------------------
        ToolId::TransactionBegin => {
            let request = crate::agent_tools::session::TransactionBeginArgs::decode(args)
                .map_err(|error| error.to_string())?;
            transaction_begin(&state(app), request.label, caller)
        }
        ToolId::TransactionCommit => {
            let _ = crate::agent_tools::session::TransactionCommitArgs::decode(args)
                .map_err(|error| error.to_string())?;
            transaction_commit(&state(app), caller)
        }
        ToolId::TransactionRollback => {
            let _ = crate::agent_tools::session::TransactionRollbackArgs::decode(args)
                .map_err(|error| error.to_string())?;
            transaction_rollback(&state(app), caller)
        }

        // ---- project ----------------------------------------------------
        ToolId::AudioClipsList => {
            let _ = crate::agent_tools::misc::AudioClipsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let state = state(app);
            let inner = state.lock_inner();
            let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
            let clips = inner
                .project
                .audio_clips()
                .into_iter()
                .enumerate()
                .map(|(index, clip)| {
                    let (start_seconds, end_seconds) = clock.audio_clip_range_seconds(clip)?;
                    Ok(json!({"clip_index":index,"clip":clip,
                        "start_seconds":start_seconds,"end_seconds":end_seconds}))
                })
                .collect::<Result<Vec<Value>, String>>()?;
            Ok(json!({ "revision": inner.revision, "clips": clips }))
        }
        ToolId::AuditionClipSource => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::render::AuditionClipSourceArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let clip_index = request.clip_index;
            let (path, archived) = match (clip_index, request.path.as_deref()) {
                (Some(index), None) => (
                    commands::audio_clip_source_path(state(app).inner(), index)?,
                    false,
                ),
                (None, Some(path)) => {
                    conversation_audio_source(&crate::ai::sessions::Store::config(), caller, path)?
                }
                _ => {
                    return Err("Use exactly one of clip_index or a conversation audio path".into());
                }
            };
            let start = request.source_start_seconds.unwrap_or(0.0);
            let duration = request.duration_seconds.unwrap_or(8.0);
            let channel = commands::AuditionChannel::parse(&request.channel.unwrap_or_default())?;
            let target_rate = request
                .clip_sample_rate
                .unwrap_or(commands::AUDITION_SAMPLE_RATE);
            let mut result = to_value(commands::audition_source_window(
                &path,
                clip_index,
                start,
                duration,
                channel,
                target_rate.clamp(8_000, 48_000),
            )?)?;
            if archived {
                result["archived_render"] = json!(true);
            }
            Ok(result)
        }
        ToolId::AudioClipTransients => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::audio_clip::AudioClipTransientsArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let path = commands::audio_clip_source_path(state(app).inner(), request.clip_index)?;
            let result = commands::measure_audio_clip_transients(
                &path,
                request.source_start_seconds.unwrap_or(0.0),
                request.duration_seconds.unwrap_or(30.0),
                request.limit.unwrap_or(24),
            )?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        ToolId::AudioClipEditRange => {
            use crate::tool_registry::arguments::{RevisionGuard, ToolArguments};
            let request = crate::agent_tools::audio_clip::AudioClipEditRangeArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let guard = RevisionGuard::decode(ToolId::AudioClipEditRange, args)
                .map_err(|error| error.to_string())?;
            let state = state(app);
            let mut inner = state.lock_inner();
            inner.check_revision(guard.expected())?;
            match (request.split_at, request.trim_to) {
                (Some(at), None) => {
                    commands::split_audio_clip_inner(&mut inner, request.clip_index, at)?
                }
                (None, Some(range)) => commands::trim_audio_clip_edge_inner(
                    &mut inner,
                    request.clip_index,
                    range.start,
                    range.end,
                    request.trim_edge.map(|edge| edge.as_str()),
                )?,
                _ => return Err("Provide exactly one of split_at or trim_to.".into()),
            }
            Ok(json!({ "revision": inner.revision }))
        }
        ToolId::AudioClipSetPlacement => {
            audio_clip_set_placement_request(state(app).inner(), args, caller)
        }
        ToolId::AudioClipSetPlayback => {
            audio_clip_set_playback_request(state(app).inner(), args, caller)
        }
        ToolId::ImportAlignedStems => {
            use crate::tool_registry::arguments::ToolArguments;
            let request =
                crate::agent_tools::project_mutation::ImportAlignedStemsArgs::decode(args)
                    .map_err(|error| error.to_string())?;
            to_value(commands::import_aligned_stems_for_agent(
                state(app).inner(),
                request
                    .paths
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .collect(),
                request.expected_revision.0,
                caller,
            )?)
        }
        ToolId::ProjectNew => {
            use crate::tool_registry::arguments::ToolArguments;
            let _ = crate::agent_tools::project_mutation::ProjectNewArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(block_on(commands::new_project(app.clone()))?)
        }
        ToolId::ProjectOpen => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::project_mutation::ProjectOpenArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(block_on(commands::open_file(request.path, app.clone()))?)
        }
        ToolId::ProjectSave => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::project_mutation::ProjectSaveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            Ok(json!({ "path": block_on(commands::save_file(request.path, app.clone()))? }))
        }
        ToolId::ProjectSetTempo
        | ToolId::ProjectSetName
        | ToolId::ProjectEdit
        | ToolId::Undo
        | ToolId::Redo => {
            let request = crate::agent_tools::project_mutation::Request::decode(id, args)
                .map_err(|error| error.to_string())?;
            crate::agent_tools::project_mutation::execute(state(app), request)
        }
        ToolId::TempoMapSet => tempo_map_set_request(state(app).inner(), args),
        ToolId::PictureClockSet => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::picture::PictureClockSetArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let snapshot = commands::set_picture_clock_inner(
                &mut state(app).lock_inner(),
                request.clock,
                request.expected_revision.0,
            )?;
            Ok(
                json!({"picture_clock": snapshot.project.properties.picture_clock, "revision": snapshot.revision}),
            )
        }
        ToolId::PictureTimecode => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::picture::PictureTimecodeArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::resolve_picture_timecode(
                state(app).inner(),
                request.0,
            )?)
        }
        ToolId::TimeSignatureSet => time_signature_set_request(state(app).inner(), args),

        // ---- MIDI import / export --------------------------------------
        ToolId::MidiImportHandoff => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::midi::MidiImportHandoffArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let (summary, revision) = midi_commands::import_handoff_for_agent(
                state(app).inner(),
                &std::path::PathBuf::from(&request.path),
                request.expected_revision.0,
                caller,
            )?;
            Ok(json!({ "summary": summary, "revision": revision }))
        }
        ToolId::MidiExportHandoff => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::midi::MidiExportHandoffArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(midi_commands::export_handoff_for_agent(
                state(app).inner(),
                request.expected_revision.0,
                request.export_options(),
            )?)
        }
        ToolId::MidiImportPreview => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::midi::MidiImportPreviewArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let summary =
                midi_commands::preview_for_agent(app, &request.path, request.import_options())?;
            to_value(summary)
        }
        ToolId::MidiImportApply => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::midi::MidiImportApplyArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let (summary, snapshot) =
                midi_commands::apply_for_agent(app, &request.path, request.import_options())?;
            Ok(json!({
                "summary": summary,
                "track_count": snapshot.track_count,
                "dirty": snapshot.dirty,
            }))
        }
        ToolId::MidiExport => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::midi::MidiExportArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let report =
                midi_commands::export_for_agent(app, &request.path, request.export_options())?;
            to_value(report)
        }

        // ---- DSL context / edits ---------------------------------------
        ToolId::ProjectGet
        | ToolId::ProjectDsl
        | ToolId::ProjectContext
        | ToolId::ProjectGenerateContext
        | ToolId::ProjectValidate
        | ToolId::ProjectExtract
        | ToolId::ProjectDiff => {
            let request = crate::agent_tools::project_query::Request::decode(id, args)
                .map_err(|error| error.to_string())?;
            crate::agent_tools::project_query::execute(state(app).inner(), request)
        }
        // ---- tracks -----------------------------------------------------
        ToolId::TracksList => {
            let _ = crate::agent_tools::misc::TracksListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            tracks_list(state(app).inner())
        }
        ToolId::TracksAdd => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let app_state = state(app);
            let mut inner = app_state.lock_inner();
            to_value(commands::add_track_checked(
                &mut inner,
                &request.name,
                request.instrument_category.as_deref().unwrap_or("synth"),
                request.instrument_name.as_deref().unwrap_or("default"),
                request.preset.as_deref(),
            )?)
        }
        ToolId::TracksCreate => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksCreateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let app_state = state(app);
            let mut inner = app_state.lock_inner();
            to_value(commands::create_track_checked(
                &mut inner,
                &request.name,
                request.instrument_category.as_deref().unwrap_or("synth"),
                request.instrument_name.as_deref().unwrap_or("default"),
                request.preset.as_deref(),
                &request.dsl,
            )?)
        }
        ToolId::TracksRemove => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::remove_track(request.index, state(app))?)
        }
        ToolId::TracksDuplicate => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksDuplicateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            tracks_duplicate(state(app).inner(), &request)
        }
        ToolId::TracksRename => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksRenameArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::rename_track(
                request.index,
                request.name,
                state(app),
            )?)
        }
        ToolId::TracksMix => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksMixArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::update_track_mix(
                request.index,
                request.volume_db,
                request.pan,
                state(app),
            )?)
        }
        ToolId::TracksMute => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksMuteArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_track_mute(
                request.index,
                request.mute,
                state(app),
            )?)
        }
        ToolId::TracksSolo => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksSoloArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_track_solo(
                request.index,
                request.solo,
                state(app),
            )?)
        }
        ToolId::TracksPreset => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksPresetArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_track_preset_blocking(
                &state(app),
                request.index,
                request.preset,
            )?)
        }
        ToolId::TracksOutput => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksOutputArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_track_output(
                request.index,
                request.output,
                state(app),
            )?)
        }
        ToolId::TracksSendAdd => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksSendAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::add_send(
                request.index,
                request.bus,
                request.amount.unwrap_or(1.0),
                request.pre_fader,
                request.source_owner,
                request.source_output,
                request.pan,
                state(app),
            )?)
        }
        ToolId::TracksSendRemove => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::tracks::TracksSendRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::remove_send(
                request.index,
                request.bus,
                request.source_owner,
                request.source_output,
                state(app),
            )?)
        }

        // ---- arrangement sections --------------------------------------
        ToolId::SectionsList => {
            let _ = crate::agent_tools::misc::SectionsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let app_state = state(app);
            let inner = app_state.lock_inner();
            commands::summary(&inner.project, inner.revision)
        }
        ToolId::SectionsAdd => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::sections::SectionsAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let start = commands::parse_position(&request.start)?;
            let end = commands::parse_position(&request.end)?;
            let snapshot = commands::add_section(
                request.name,
                start,
                end,
                request.repeat,
                request.label,
                state(app),
            )?;
            commands::summary(&snapshot.project, snapshot.revision)
        }
        ToolId::SectionsSet => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::sections::SectionsSetArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let start = request
                .start
                .as_deref()
                .map(commands::parse_position)
                .transpose()?;
            let end = request
                .end
                .as_deref()
                .map(commands::parse_position)
                .transpose()?;
            let repeat = request.repeat_edit();
            let label = request.label_edit();
            let snapshot = commands::set_section(
                request.name,
                request.new_name,
                start,
                end,
                repeat,
                label,
                state(app),
            )?;
            commands::summary(&snapshot.project, snapshot.revision)
        }
        ToolId::SectionsRemove => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::sections::SectionsRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let snapshot = commands::remove_section(request.name, state(app))?;
            commands::summary(&snapshot.project, snapshot.revision)
        }

        // ---- content libraries ------------------------------------------
        ToolId::ContentLibraries => {
            let request = crate::agent_tools::content::ContentLibrariesArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let mut listing = to_value(crate::content_library::list(
                &crate::content_library::resolve_catalogue_path(request.catalogue.as_deref()),
            )?)?;
            listing["soundfonts"] = json!(
                crate::soundfont_store::installed(&crate::soundfont_store::dir())
                    .unwrap_or_default()
            );
            listing["discovered_soundfonts"] = json!(crate::soundfont_store::discover(
                &crate::soundfont_store::dir()
            ));
            Ok(listing)
        }
        ToolId::ContentLibraryRegister => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::content::ContentLibraryRegisterArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::content_library::register(
                &crate::content_library::resolve_catalogue_path(request.catalogue.as_deref()),
                std::path::Path::new(&request.path),
                request.name.as_deref(),
            )?)
        }
        ToolId::ContentLibraryRemove => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::content::ContentLibraryRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::content_library::remove(
                &crate::content_library::resolve_catalogue_path(request.catalogue.as_deref()),
                &request.id,
            )?)
        }
        ToolId::ContentLibraryRescan => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::content::ContentLibraryRescanArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::content_library::rescan(
                &crate::content_library::resolve_catalogue_path(request.catalogue.as_deref()),
                request.id.as_deref(),
            )?)
        }

        // ---- presets ----------------------------------------------------
        ToolId::PresetsList => {
            let request = crate::agent_tools::content::PresetsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            if let Some(name) = request.name.as_deref() {
                let state = state(app);
                let inner = state.lock_inner();
                return preset_details(&inner.project, name);
            }
            let snapshot = commands::get_project(state(app))?;
            // A pack is named by an absolute path, so the listing carries both
            // the rows and the folder they live in.
            let soundfonts = crate::soundfont_store::installed(&crate::soundfont_store::dir())
                .unwrap_or_default();
            Ok(json!({
                "presets": snapshot.project.presets,
                "soundfonts": soundfonts,
                "discovered_soundfonts": crate::soundfont_store::discover(&crate::soundfont_store::dir()),
                "soundfonts_dir": crate::soundfont_store::dir().display().to_string(),
            }))
        }
        ToolId::PresetBundleExport => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetBundleExportArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::export_bundle_for_agent(
                state(app).inner(),
                &request.name,
                request.expected_revision.0,
            )?)
        }
        ToolId::PresetBundleImport => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetBundleImportArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::import_bundle_for_agent(
                state(app).inner(),
                &std::path::PathBuf::from(&request.path),
                &request.name,
                request.expected_revision.0,
                caller,
            )?)
        }
        ToolId::PresetsAdd => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::add_preset(
                request.name,
                request.source,
                request.program,
                request.plugin,
                request.patch,
                request.file,
                state(app),
            )?)
        }
        ToolId::PresetsRemove => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::remove_preset(request.name, state(app))?)
        }
        ToolId::PresetsRename => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsRenameArgs::decode(args)
                .map_err(|error| error.to_string())?;
            presets_rename(state(app).inner(), &request)
        }
        ToolId::PresetsDuplicate => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsDuplicateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            presets_duplicate(state(app).inner(), &request)
        }
        ToolId::PresetsSetParam => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsSetParamArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_preset_param(
                request.name,
                request.param,
                request.value,
                state(app),
            )?)
        }
        ToolId::PresetsSetParams => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::presets::PresetsSetParamsArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let params = params_to_pairs(&request.params)?;
            let replace_synth = request.replace_synth.unwrap_or(false);
            let ticket = replace_synth.then(|| state(app).patch_load.begin());
            to_value(commands::set_preset_params_blocking(
                &state(app),
                request.name,
                params,
                replace_synth,
                Vec::new(),
                None,
                ticket,
            )?)
        }

        // ---- plugins ----------------------------------------------------
        ToolId::PluginSearch => {
            let request = crate::agent_tools::plugin_search::PluginSearchArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let client = index(app);
            let view = client.view()?;
            to_value(crate::plugin_store::search(
                view.entries.as_slice(),
                view.source.as_str(),
                request.query.as_deref(),
                request.tags.as_deref(),
                request.platform.as_deref(),
                request.format.as_deref(),
                request.installed_only.unwrap_or(false),
                request.limit,
            )?)
        }
        ToolId::PluginPull => {
            let request = crate::agent_tools::plugin_pull::PluginPullArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let client = index(app);
            let view = client.view()?;
            install_policy.authorize(|| {
                crate::plugin_store::is_panel_install(
                    view.entries.as_slice(),
                    view.source.as_str(),
                    &request.id,
                    request.platform.as_deref(),
                    request.format.as_deref(),
                )
            })?;
            to_value(block_on(crate::plugin_store::pull(
                &view.authority,
                view.entries.as_slice(),
                &request.id,
                request.platform.as_deref(),
                request.format.as_deref(),
                request.dry_run.unwrap_or(true),
                None,
            ))?)
        }
        ToolId::PluginIndexStatus => to_value(index(app).status()),
        ToolId::PluginIndexRefresh => {
            let request = crate::agent_tools::plugin_index::PluginIndexRefreshArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let client = index(app);
            let fetch = crate::plugin_index::HttpFetch::new()
                .map_err(|error| format!("network: {error}"))?;
            to_value(
                block_on(client.refresh(&fetch, request.force.unwrap_or(false)))
                    .map_err(|error| error.to_string())?,
            )
        }
        ToolId::SoundfontsList => {
            let request = crate::agent_tools::soundfonts::SoundfontsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            if let Some(path) = request.path.as_deref() {
                Ok(json!({ "inspection": crate::soundfont_store::inspect_page(
                    std::path::Path::new(path),
                    request.bank,
                    request.query.as_deref(),
                    request.offset.unwrap_or(0),
                    request.limit,
                )? }))
            } else {
                let view = index(app).view()?;
                to_value(crate::soundfont_store::listing(
                    &crate::soundfont_store::dir(),
                    view.packs.as_slice(),
                )?)
            }
        }
        ToolId::SoundfontInstall => {
            let request = crate::agent_tools::soundfonts::SoundfontInstallArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let view = index(app).view()?;
            to_value(block_on(crate::soundfont_store::install(
                &view.authority,
                &crate::soundfont_store::dir(),
                view.packs.as_slice(),
                &request.id,
                |_, _| {},
            ))?)
        }
        ToolId::SoundfontsRemove => {
            let request = crate::agent_tools::soundfonts::SoundfontsRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::soundfont_store::remove(
                &crate::soundfont_store::dir(),
                &request.id,
            )?)
        }
        ToolId::SampleSearch => {
            let request =
                crate::samples::SearchArgs::decode(args).map_err(|error| error.to_string())?;
            crate::samples::search_request(&request)
        }
        ToolId::SamplePull => {
            let request = crate::agent_tools::sample::SamplePullArgs::decode(args)
                .map_err(|error| error.to_string())?;
            crate::samples::pull(&state(app), &request)
        }
        ToolId::PluginsScan => {
            let request = crate::agent_tools::plugins::PluginsScanArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::scan_plugins_blocking(request.rescan))
        }
        ToolId::PluginsLoad => {
            let request = crate::agent_tools::plugins::PluginsLoadArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(on_main(app, move |state| {
                commands::load_plugin_for_preset_blocking(state, &request.preset)
            })?)
        }
        ToolId::PluginsParams => {
            let request = crate::agent_tools::plugins::PluginsParamsArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::get_plugin_params(request.preset, state(app))?)
        }
        ToolId::PluginsOpenGui => {
            let request = crate::agent_tools::plugins::PluginsOpenGuiArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let size = block_on(commands::open_plugin_gui(request.preset, app.clone()))?;
            Ok(json!({ "width": size.0, "height": size.1 }))
        }
        ToolId::PluginsCloseGui => {
            let request = crate::agent_tools::plugins::PluginsCloseGuiArgs::decode(args)
                .map_err(|error| error.to_string())?;
            block_on(commands::close_plugin_gui(request.preset, app.clone()))?;
            Ok(json!({ "closed": true }))
        }
        ToolId::PluginsHasGui => {
            let request = crate::agent_tools::plugins::PluginsHasGuiArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::plugin_has_gui(request.preset, state(app))?)
        }
        ToolId::PluginsSaveState => {
            let request = crate::agent_tools::plugins::PluginsSaveStateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            match request.mode.as_str() {
                "embedded" => to_value(on_main(app, move |state| {
                    commands::save_plugin_state_embedded_inner(state, &request.preset)
                })?),
                "file" => {
                    let path = request.path.clone().ok_or("mode=file needs path")?;
                    to_value(on_main(app, move |state| {
                        commands::save_plugin_state_file_inner(state, &request.preset, &path)
                    })?)
                }
                _ => Err("mode must be 'embedded' or 'file'".into()),
            }
        }

        ToolId::PluginsProgramCapabilities => {
            let _ = crate::agent_tools::plugins::PluginsProgramCapabilitiesArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::program_capability_table())
        }
        ToolId::PluginsPrograms => {
            let request = crate::agent_tools::plugins::PluginsProgramsArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(on_main(app, move |state| {
                commands::plugin_program_listing(state, &request.preset)
            })?)
        }
        ToolId::PluginsSelectProgram => {
            let request = crate::agent_tools::plugins::PluginsSelectProgramArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let selection = commands::ProgramSelectRequest {
                index: request.index,
                name: request.name,
                state_file: request.state_file,
                mode: request.mode,
                path: request.path,
            };
            to_value(on_main(app, move |state| {
                commands::select_plugin_program_request(state, &request.preset, selection)
            })?)
        }
        ToolId::PluginsReadState => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::plugins::PluginsReadStateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let preset = request.preset;
            let max_bytes = request.max_bytes.map(|value| value as usize);
            to_value(on_main(app, move |state| {
                commands::read_plugin_state_value(state, &preset, max_bytes)
            })?)
        }
        ToolId::PluginsApplyState => {
            let request = crate::agent_tools::plugins::PluginsApplyStateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::apply_plugin_state_value(
                &state(app),
                &request.preset,
                &request.state_hex,
            )?)
        }

        // ---- transport --------------------------------------------------
        ToolId::TransportPlay
        | ToolId::TransportPause
        | ToolId::TransportStop
        | ToolId::TransportSeek
        | ToolId::TransportState
        | ToolId::TransportMute
        | ToolId::TransportUnmute => {
            let request = crate::agent_tools::transport::Request::decode(id, args)
                .map_err(|error| error.to_string())?;
            crate::agent_tools::transport::execute(app, request)
        }

        // ---- render / audio --------------------------------------------
        ToolId::RenderOffline => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::render::RenderOfflineArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let pre_roll = request.pre_roll.or(request.pre_roll_alias);
            let sample_format = request
                .sample_format
                .map(|value| {
                    serde_json::from_value::<oc_dsl::ast::SampleFormat>(json!(value))
                        .map_err(|error| format!("argument 'sample_format': {error}"))
                })
                .transpose()?;
            Ok(json!({ "path": block_on(commands::render_offline(
                request.path, pre_roll, sample_format, request.dither, state(app)
            ))? }))
        }
        ToolId::RenderRange => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::render::RenderRangeArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let range = commands::RangeRequest {
                start_s: request.start,
                end_s: request.end,
                start_bar: request.start_bar,
                end_bar: request.end_bar,
                tracks: request.tracks,
                stems: request.stems.unwrap_or(false),
                pre_roll: request.pre_roll.or(request.pre_roll_alias),
                float_wav: request.float_wav.unwrap_or(false),
            };
            to_value(commands::render_range_for_agent(
                request.path,
                range,
                request.bus,
                &state(app),
            )?)
        }
        ToolId::RenderCompatReport => {
            use crate::tool_registry::arguments::ToolArguments;
            let _ = crate::agent_tools::render::RenderCompatReportArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::project_compatibility_report_blocking(&state(app)))
        }
        ToolId::RenderStems => block_on(render_stems(state(app).inner(), args)),
        ToolId::AuditionRange => {
            use crate::tool_registry::arguments::ToolArguments;
            let request = crate::agent_tools::render::AuditionRangeArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let range = commands::RangeRequest {
                start_s: request.start,
                end_s: request.end,
                start_bar: request.start_bar,
                end_bar: request.end_bar,
                tracks: request.tracks,
                stems: false,
                pre_roll: None,
                float_wav: request.float_wav.unwrap_or(false),
            };
            let channel = commands::AuditionChannel::parse(
                &request.channel.unwrap_or_else(|| "mono".into()),
            )?;
            // The clip is shaped for the active model's audio input: a provider
            // that documents mono at a fixed rate gets exactly that, and the
            // full-rate render stays on disk for the session archive.
            let target_rate = request
                .clip_sample_rate
                .unwrap_or(commands::AUDITION_SAMPLE_RATE);
            to_value(commands::audition_range_for_agent(
                request.path,
                range,
                channel,
                target_rate.clamp(8_000, 48_000),
                &state(app),
                caller
                    .strip_prefix(crate::state::PANEL_BATCH_OWNER_PREFIX)
                    .is_some_and(|id| id != "unsaved"),
            )?)
        }
        ToolId::AudioAnalyze => audio_analyze_tool(args),
        ToolId::AnalyzeNotes => analyze_notes_tool(args),
        ToolId::SeparateStems => separate_stems_tool(args),
        ToolId::AudioCompare => audio_compare_tool(args),
        ToolId::AudioDevices => {
            let _ = crate::agent_tools::misc::AudioDevicesArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::list_audio_devices(state(app)))
        }
        ToolId::AudioConfig => {
            let _ = crate::agent_tools::misc::AudioConfigArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::get_audio_config(state(app))?)
        }

        // ---- undo -------------------------------------------------------
        ToolId::UndoState => {
            let _ = crate::agent_tools::misc::UndoStateArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::get_undo_state(state(app)))
        }
        ToolId::UndoHistory => {
            let _ = crate::agent_tools::misc::UndoHistoryArgs::decode(args)
                .map_err(|error| error.to_string())?;
            undo_history(state(app).inner())
        }

        // ---- sampler assets --------------------------------------------
        ToolId::SamplerAssets => {
            let _ = crate::agent_tools::misc::SamplerAssetsArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::list_sampler_assets_blocking()?)
        }

        // ---- effects / buses -------------------------------------------
        ToolId::MeterSnapshot => {
            let request = crate::agent_tools::metering::decode(id, args)
                .map_err(|error| error.to_string())?;
            crate::agent_tools::metering::execute(state(app).inner(), request)
        }
        ToolId::DevicesCatalog => {
            let request =
                crate::device_catalog::Query::decode(args).map_err(|error| error.to_string())?;
            crate::device_catalog::catalog_request(&request)
        }
        ToolId::DevicePresetsList => {
            let request = crate::agent_tools::device::DevicePresetsListArgs::decode(args)
                .map_err(|error| error.to_string())?;
            crate::device_presets::list_value(
                &crate::device_presets::user_path(),
                request.device.as_deref(),
            )
        }
        ToolId::DevicePresetApply => {
            let request = crate::agent_tools::device::DevicePresetApplyArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(crate::device_presets::apply_to(
                &state(app),
                &request.id,
                &request.target,
                request.effect_index,
                &request.device,
            )?)
        }
        ToolId::EffectsAdd => {
            let request = crate::agent_tools::effects::EffectsAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::add_effect(
                request.target,
                request.effect_type,
                state(app),
            )?)
        }
        ToolId::EffectsRemove => {
            let request = crate::agent_tools::effects::EffectsRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::remove_effect(
                request.target,
                request.index,
                state(app),
            )?)
        }
        ToolId::EffectsSetBypass => {
            let request = crate::agent_tools::effects::EffectsSetBypassArgs::decode(args)
                .map_err(|error| error.to_string())?;
            let guard = crate::tool_registry::arguments::RevisionGuard::decode(
                ToolId::EffectsSetBypass,
                args,
            )
            .map_err(|error| error.to_string())?;
            effects_set_bypass(state(app).inner(), &request, guard.expected(), caller)
        }
        ToolId::EffectsSetParam => {
            let request = crate::agent_tools::effects::EffectsSetParamArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_effect_param(
                request.target,
                request.index,
                request.param,
                request.value,
                state(app),
            )?)
        }
        ToolId::BusesAdd => {
            let request = crate::agent_tools::bus::BusesAddArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::add_bus(
                request.name,
                request.routes.unwrap_or_default(),
                state(app),
            )?)
        }
        ToolId::BusesRoutes => {
            let request = crate::agent_tools::bus::BusesRoutesArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::set_bus_routes(
                request.name,
                request.routes,
                state(app),
            )?)
        }
        ToolId::BusesMix => {
            let request = crate::agent_tools::bus::BusesMixArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::update_bus_mix(
                request.name,
                request.volume_db,
                request.mute,
                request.solo,
                state(app),
            )?)
        }
        ToolId::BusesRemove => {
            let request = crate::agent_tools::bus::BusesRemoveArgs::decode(args)
                .map_err(|error| error.to_string())?;
            to_value(commands::remove_bus(
                request.name,
                request.reroute,
                state(app),
            )?)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<Value> {
        mcp_tools()
    }

    /// The exact refusal the provider sends when a replayed turn lost the
    /// signature (verbatim, from the live probe).
    const ARGON_REJECTION: &str = r#"{"error":{"message":"litellm.BadRequestError: OpenAIException - Gemini 4+ requires the original thought signature on the first tool call (get_tempo) of each step in the current turn; preserve tool_calls[].extra_content.google.thought_signature or tool_calls[].signature in assistant history."}}"#;
    const SET_GAIN_CALL: &str = r#"{"id":"chatcmpl-1","usage":{"prompt_tokens":10,"completion_tokens":2},"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"set_track_gain","arguments":"{\"track\":\"kick\",\"gain_db\":-6}"},"extra_content":{"google":{"thought_signature":"sig-endpoint"}},"signature":"sig-endpoint"}],"provider_specific_fields":{"refusal":null}},"finish_reason":"tool_calls"}]}"#;
    const SET_GAIN_DONE: &str = r#"{"id":"chatcmpl-2","choices":[{"message":{"role":"assistant","content":"Kick is at -6 dB."},"finish_reason":"stop"}]}"#;

    /// A fixture provider saved in the shape a gateway uses (`…/v1`), installed
    /// for the guard's lifetime exactly as the panel's tests do.
    fn provider_config(
        name: &str,
        id: &str,
        model: &str,
        endpoint: &str,
    ) -> (std::path::PathBuf, crate::ai_service::TestConfigPathGuard) {
        let dir = oc_test_support::temp_dir("oc-agent-complete", name);
        let path = dir.join("ai-providers.yaml");
        let yaml = format!(
            r#"version: 2
active_provider: {id}
providers:
- id: {id}
  name: Fixture
  kind: openai_chat_completions
  wire: openai-chat-completions
  auth: api_key
  endpoint: {endpoint}
  model: {model}
  context_length: 32000
  max_tokens: 4096
  temperature: 0.3
  thinking_level: medium
  capabilities:
    text: true
    streaming: false
    native_tool_calling: true
"#
        );
        crate::private_fs::write_atomic(&path, yaml.as_bytes()).unwrap();
        let guard = crate::ai_service::test_config_path(path.clone());
        (path, guard)
    }

    fn complete(body: Value) -> (u16, Value) {
        let (status, text) = handle_ai_complete(body.to_string().as_bytes());
        let value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
        (status, value)
    }

    #[test]
    fn preset_details_returns_the_authored_child_and_the_effective_sound() {
        let project = oc_dsl::parse(
            "@project\n@preset parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.33, output.gain: 0.2}\n  effects:\n    - gain: factor=0.5\n@preset child\n  extends: parent\n  params: {output.gain: 0.17}\n  effects:\n    - gain: factor=0.25\n",
        ).unwrap();
        let report = preset_details(&project, "child").unwrap();
        let authored: oc_dsl::ast::PresetDef =
            serde_json::from_value(report["presets"][0].clone()).unwrap();
        let effective: oc_dsl::ast::PresetDef =
            serde_json::from_value(report["effective_preset"].clone()).unwrap();
        assert_eq!(authored, project.presets[1]);
        assert_eq!(effective.plugin.as_deref(), Some("builtin.open_synth"));
        assert_eq!(effective.params[0].0, "osc.a.level");
        assert_eq!(effective.params[1].1, oc_dsl::ast::ParamValue::Number(0.17));
        assert_eq!(
            effective.effects,
            [
                project.presets[0].effects.clone(),
                project.presets[1].effects.clone()
            ]
            .concat()
        );
        assert!(preset_details(&project, "absent").is_err());
    }

    fn http_request(method: &str, path: &str, token: Option<&str>, body: &str) -> Request {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(token) = token {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    /// The handler runs on a connection thread in production, so this test is
    /// sync too: it drives the real `block_on` the server uses.
    #[test]
    fn the_ai_endpoint_returns_the_message_and_its_signature_survives_a_replay() {
        let mut turn = 0;
        let (base, server) = crate::ai::test_support::canned_by_request(2, move |request| {
            turn += 1;
            if turn == 1 {
                return (200, SET_GAIN_CALL.to_owned());
            }
            if request.contains("sig-endpoint") {
                (200, SET_GAIN_DONE.to_owned())
            } else {
                (400, ARGON_REJECTION.to_owned())
            }
        });
        // The saved form is the gateway base; the router adds the wire's path.
        let (_path, _guard) = provider_config(
            "replay",
            "fixture",
            "google/gemini-4-argon",
            &format!("{base}v1"),
        );
        let (status, first) = complete(json!({
            "provider": "fixture",
            "messages": [{"role":"user","content":"pull the kick to -6 dB"}],
            "tools": true,
            "max_tokens": 256
        }));
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["ok"], true);
        assert_eq!(first["finish_reason"], "tool_calls");
        assert_eq!(
            first["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-endpoint"
        );
        assert_eq!(first["provider_specific_fields"]["refusal"], Value::Null);
        // Canonical usage, as the panel receives it.
        assert_eq!(first["usage"]["input"], 10);
        assert_eq!(first["usage"]["output"], 2);

        // Replaying the object the endpoint answered with is what a driver does.
        let (status, second) = complete(json!({
            "provider": "fixture",
            "messages": [
                {"role":"user","content":"pull the kick to -6 dB"},
                first,
                {"role":"tool","tool_call_id":"call_1","content":"gain set"}
            ],
            "tools": true
        }));
        assert_eq!(status, 200, "{second}");
        assert_eq!(second["content"], "Kick is at -6 dB.");
        assert_eq!(second["finish_reason"], "stop");

        let requests = server.join().unwrap();
        let replayed: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let message = &replayed["messages"][1];
        assert_eq!(
            message["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig-endpoint"
        );
        assert_eq!(message["provider_specific_fields"]["refusal"], Value::Null);
        // Our envelope and derived report are not message fields.
        for key in ["ok", "finish_reason", "usage"] {
            assert!(message.get(key).is_none(), "{key} leaked into the message");
        }
    }

    /// The handler runs on a connection thread in production, so this test is
    /// sync too: it drives the real `block_on` the server uses.
    #[test]
    fn the_ai_endpoint_answers_a_provider_refusal_with_its_text() {
        let (base, server) = crate::ai::test_support::canned(400, ARGON_REJECTION);
        let (_path, _guard) = provider_config(
            "refusal",
            "fixture",
            "google/gemini-4-argon",
            &format!("{base}v1"),
        );
        let (status, body) = complete(json!({
            "provider": "fixture",
            "messages": [{"role":"user","content":"hi"}]
        }));
        server.join().unwrap();
        assert_eq!(status, 400);
        assert_eq!(body["ok"], false);
        // The old string is what every existing caller reads.
        let error = body["error"].as_str().expect("an error string");
        assert!(
            error.contains("thought signature"),
            "the provider's text was dropped: {error}"
        );
        // A provider 400 is a malformed request, not a missing key or a limit.
        assert_eq!(body["kind"], "request");
        assert_eq!(body["retryable"], false);
        assert_eq!(body["reauth"], false);
    }

    /// One saved provider entry per fake status, driven through the real HTTP
    /// handler, so each class is proved where a headless caller sees it.
    fn complete_against(
        name: &str,
        endpoint: &str,
        status: u16,
        body: &'static str,
    ) -> (u16, Value) {
        let (base, server) = crate::ai::test_support::canned(status, body);
        let (_path, _guard) = provider_config(
            name,
            "fixture",
            "google/gemini-4-argon",
            &format!("{base}{endpoint}"),
        );
        let (code, answer) = complete(json!({
            "provider": "fixture",
            "messages": [{"role":"user","content":"hi"}]
        }));
        server.join().unwrap();
        (code, answer)
    }

    #[test]
    fn the_ai_endpoint_derives_the_status_from_the_error_class() {
        // 401: the saved key is wrong; resending it cannot help.
        let (status, body) = complete_against("auth", "v1", 401, "{}");
        assert_eq!(status, 401);
        assert_eq!(body["kind"], "auth");
        assert_eq!(body["retryable"], false);

        // 403: the key is valid but the provider denies this model.
        let (status, body) = complete_against("permission", "v1", 403, "{}");
        assert_eq!(status, 403);
        assert_eq!(body["kind"], "permission");

        // 429: a rate limit is retryable.
        let (status, body) = complete_against("limit", "v1", 429, "{}");
        assert_eq!(status, 429);
        assert_eq!(body["kind"], "rate_limit");
        assert_eq!(body["retryable"], true);

        // 500: the provider is down; 503 tells the driver to come back.
        let (status, body) = complete_against("server", "v1", 500, "{}");
        assert_eq!(status, 503);
        assert_eq!(body["kind"], "server");
        assert_eq!(body["retryable"], true);

        // 200 with a body that is not the wire: a protocol failure, not a turn.
        let (status, body) = complete_against("protocol", "v1", 200, "not json");
        assert_eq!(status, 502);
        assert_eq!(body["kind"], "protocol");
    }

    #[test]
    fn the_ai_endpoint_answers_a_configuration_error_with_409() {
        // The endpoint is plain HTTP off loopback, so the saved entry is unusable.
        let (_path, _guard) = provider_config(
            "config",
            "fixture",
            "google/gemini-4-argon",
            "http://example.test/v1",
        );
        let (status, body) = complete(json!({
            "provider": "fixture",
            "messages": [{"role":"user","content":"hi"}]
        }));
        assert_eq!(status, 409, "{body}");
        assert_eq!(body["kind"], "config");
        assert_eq!(body["retryable"], false);
    }

    #[test]
    fn the_ai_endpoint_answers_a_dropped_connection_with_503() {
        // Nothing listens on this loopback port, so the turn never leaves.
        let (_path, _guard) = provider_config(
            "network",
            "fixture",
            "google/gemini-4-argon",
            "http://127.0.0.1:1/v1",
        );
        let (status, body) = complete(json!({
            "provider": "fixture",
            "messages": [{"role":"user","content":"hi"}]
        }));
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["kind"], "network");
        assert_eq!(body["retryable"], true);
    }

    #[test]
    fn the_ai_endpoint_answers_a_malformed_body_with_400_and_the_request_class() {
        let (status, body) = complete(json!({ "provider": "fixture" }));
        assert_eq!(status, 400);
        assert_eq!(body["ok"], false);
        assert_eq!(body["kind"], "request");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("messages")
        );
    }

    #[test]
    fn saved_http_requests_validate_messages_before_session_admission() {
        let absolute = std::env::temp_dir().join("song.ocp");
        for messages in [
            json!([]),
            json!([{"role":"invalid","content":"bad role"}]),
            json!([{"role":"user","content":"", "input_audio":{"format":"wav","data":"not-base64!"}}]),
            json!([{"role":"user","content":"x".repeat(crate::ai::LIMIT + 1)}]),
            json!(vec![
                json!({"role":"user","content":"hi"});
                crate::ai::MAX_WIRE_MESSAGES + 1
            ]),
        ] {
            let (status, body) = complete(json!({
                "provider":"not-configured",
                "session_id":"missing-session",
                "project_path":absolute,
                "messages":messages,
            }));
            assert_eq!(status, 400, "{body}");
            assert_eq!(body["kind"], "request");
        }
    }

    /// Every class the provider layer can produce maps to one of the six
    /// statuses; the two kinds a fake provider cannot stage quickly are here.
    #[test]
    fn every_error_class_has_a_status_and_reports_its_verdict() {
        for (kind, status) in [
            ("request", 400),
            ("cancelled", 409),
            ("auth", 401),
            ("auth_expired", 401),
            ("permission", 403),
            ("rate_limit", 429),
            ("config", 409),
            ("redirect", 409),
            ("timeout", 503),
            ("network", 503),
            ("server", 503),
            ("protocol", 502),
            ("truncated", 502),
        ] {
            assert_eq!(complete_status(kind), status, "{kind}");
        }
        let (status, text) = complete_failure(AiError {
            kind: "auth_expired".into(),
            message: "Sign in again".into(),
            reauth: true,
            retryable: false,
        });
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(status, 401);
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "Sign in again");
        assert_eq!(body["kind"], "auth_expired");
        assert_eq!(body["reauth"], true);
        assert_eq!(body["retryable"], false);
    }

    #[test]
    fn the_ai_endpoint_is_behind_the_bearer_token() {
        let unreachable = |_: &[u8]| -> (u16, String) { panic!("app route reached") };
        let (status, body) = route_with(
            &http_request("POST", "/ai/complete", None, "{}"),
            "secret",
            unreachable,
            handle_ai_complete,
            unreachable,
        );
        assert_eq!(status, 401);
        assert!(body.contains("unauthorized"));

        let (status, body) = route_with(
            &http_request("POST", "/ai/complete", Some("secret"), "{}"),
            "secret",
            unreachable,
            handle_ai_complete,
            unreachable,
        );
        assert_eq!(status, 400);
        assert!(body.contains("missing 'provider'"), "{body}");

        // The app-state routes still reach their own handlers.
        let (status, body) = route_with(
            &http_request("POST", "/call", Some("secret"), "{}"),
            "secret",
            |_| (200, "call".into()),
            unreachable,
            |_| (200, "mcp".into()),
        );
        assert_eq!((status, body.as_str()), (200, "call"));
    }

    #[test]
    fn audio_compare_compressed_inputs_preserve_paths_samples_and_wav_behavior() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../oc-engine/tests/fixtures/sf3-tone-22050.ogg");
        let dir = temp_dir("compare-compressed");
        let staged = decode_separation_input(&source, &dir).unwrap();
        for (reference, candidate, warning) in [
            (&source, &staged.path, "Reference"),
            (&staged.path, &source, "Candidate"),
        ] {
            let result =
                audio_compare_tool(&json!({"reference":reference,"candidate":candidate})).unwrap();
            assert_eq!(
                result["reference"]["path"],
                reference.to_string_lossy().as_ref()
            );
            assert_eq!(
                result["candidate"]["path"],
                candidate.to_string_lossy().as_ref()
            );
            assert_eq!(result["sample"]["max_abs_difference"], 0.0);
            assert!(result["warnings"].as_array().unwrap().iter().any(|v| {
                v.as_str()
                    .is_some_and(|w| w.starts_with(warning) && w.contains("decoded to stereo"))
            }));
        }
        let actual = audio_compare_tool(
            &json!({"reference":staged.path,"candidate":staged.path,"stems":true}),
        )
        .unwrap();
        let expected = serde_json::to_value(
            oc_engine::analysis::compare_wavs(
                &staged.path,
                &staged.path,
                oc_engine::analysis::CompareOptions {
                    stems: true,
                    allow_mismatch: false,
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(actual, expected);
        let duration = read_analysis_media(&staged.path).unwrap().0.duration_s() / 2.0;
        let excerpt = audio_compare_tool(&json!({
            "reference": source, "candidate": staged.path, "duration": duration,
            "reference_start": duration / 2.0, "candidate_start": duration / 2.0,
        }))
        .unwrap();
        assert_eq!(excerpt["sample"]["max_abs_difference"], 0.0);
        assert!(excerpt["window"]["reference_start_s"].as_f64().unwrap() > 0.0);
        let short = dir.join("short.wav");
        write_two_tone_wav(&short, 2.0);
        assert!(audio_compare_tool(&json!({"reference":source,"candidate":short})).is_err());
        assert!(
            audio_compare_tool(
                &json!({"reference":source,"candidate":short,"allow_mismatch":true})
            )
            .is_ok()
        );
        let invalid = dir.join("invalid.mp3");
        std::fs::write(&invalid, b"not audio").unwrap();
        assert!(
            audio_compare_tool(&json!({"reference":invalid,"candidate":short}))
                .unwrap_err()
                .starts_with("comparison reference failed:")
        );
        drop(staged);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn audio_compare_can_match_a_late_wav_passage_at_independent_offsets() {
        use std::io::{Seek as _, Write as _};
        let dir = oc_audio::private_fs::test_root().join("compare-passage");
        std::fs::create_dir_all(&dir).unwrap();
        let rate = 16_000;
        let phrase: Vec<f32> = (0..rate * 2)
            .map(|frame| {
                let t = f64::from(frame) / f64::from(rate);
                (0.2 * (std::f64::consts::TAU * (130.0 * t + 33.0 * t * t)).sin()) as f32
            })
            .collect();
        let wav = oc_engine::wav::pcm_wav_bytes(&phrase, rate, 1).unwrap();
        let reference = dir.join("whole.wav");
        let candidate = dir.join("take.wav");
        for (path, start) in [(&reference, 40), (&candidate, 1)] {
            let offset = start * rate * 2;
            let data = offset + phrase.len() as u32 * 2;
            let mut header = wav[..44].to_vec();
            header[4..8].copy_from_slice(&(data + 36).to_le_bytes());
            header[40..44].copy_from_slice(&data.to_le_bytes());
            let mut file = std::fs::File::create(path).unwrap();
            file.write_all(&header).unwrap();
            file.seek(std::io::SeekFrom::Start(u64::from(44 + offset)))
                .unwrap();
            file.write_all(&wav[44..]).unwrap();
        }
        let mut args = json!({
            "reference": reference, "candidate": candidate,
            "reference_start": 40, "candidate_start": 1, "duration": 2,
            "listen": true, "channel": "stereo", "clip_sample_rate": 24_000,
        });
        let started = std::time::Instant::now();
        let result = audio_compare_tool(&args).unwrap();
        let full_time = started.elapsed();
        assert_eq!(result["sample"]["max_abs_difference"], 0.0);
        assert_eq!(result["reference"]["channels"], 1);
        let pair = &result["listening"];
        assert_eq!(pair["status"], "matched");
        assert_eq!(pair["duration_s"], 2.0);
        assert_eq!(pair["reference_audio"]["sample_rate"], 24_000);
        assert_eq!(pair["reference_audio"]["channels"], 2);
        assert_eq!(
            pair["reference_audio"]["base64"],
            pair["candidate_audio"]["base64"]
        );
        assert_eq!(
            result["window"],
            json!({
                "reference_start_s": 40.0, "reference_end_s": 42.0,
                "candidate_start_s": 1.0, "candidate_end_s": 3.0,
            })
        );
        args["listening_only"] = json!(true);
        let started = std::time::Instant::now();
        let fast = audio_compare_tool(&args).unwrap();
        eprintln!(
            "Matched two-second pair: full report {full_time:?}, listening only {:?}",
            started.elapsed()
        );
        assert_eq!(fast["listening_only"], true);
        assert_eq!(fast["window"], result["window"]);
        assert_eq!(fast["listening"], *pair);
        assert!(fast.get("alignment").is_none() && fast.get("score").is_none());
        args["listen"] = json!(false);
        assert_eq!(
            audio_compare_tool(&args).unwrap_err(),
            "listening_only requires listen=true"
        );
        assert!(
            audio_compare_tool(&json!({
                "reference": reference, "candidate": candidate,
                "reference_start": 41, "candidate_start": 1, "duration": 2, "allow_mismatch": true,
            }))
            .unwrap_err()
            .contains("comparison reference failed")
        );
        assert!(
            audio_compare_tool(&json!({
                "reference": reference, "candidate": candidate, "reference_start": 1,
            }))
            .unwrap_err()
            .contains("Set duration")
        );
    }

    #[test]
    #[ignore = "Opt-in original media decoder validation without full-song comparison"]
    fn audio_compare_original_mp3_loader() {
        let source = std::path::PathBuf::from(
            std::env::var_os("OCP_TEST_COMPARE_SOURCE").expect("media path"),
        );
        let (buffer, decoded) = read_analysis_media(&source).unwrap();
        assert!(decoded && buffer.frames() > 0 && buffer.sample_rate > 0);
        assert_eq!(buffer.channel_count(), 2);
    }

    #[test]
    fn a_saved_long_audio_reference_allows_a_late_bounded_audition_in_its_session() {
        use std::io::{Seek as _, Write as _};
        let root = oc_audio::private_fs::test_root().join("local-song-audition");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("reference.wav");
        let mut header = oc_engine::wav::pcm_wav_bytes(&[], 48_000, 2).unwrap();
        let data = 48_000u32 * 4 * 80;
        header[4..8].copy_from_slice(&(data + 36).to_le_bytes());
        header[40..44].copy_from_slice(&data.to_le_bytes());
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&header).unwrap();
        file.set_len(u64::from(data) + 44).unwrap();
        file.seek(std::io::SeekFrom::Start(44 + 48_000 * 4 * 70))
            .unwrap();
        for _ in 0..4_800 {
            file.write_all(&[0, 32, 0, 32]).unwrap();
        }
        drop(file);

        let store = crate::ai::sessions::Store::new(root.join("sessions"));
        let session = store.create(None, None, None, None).unwrap();
        let attachment = crate::ai::media::attach(path.to_str().unwrap()).unwrap();
        assert!(attachment.media.local_reference);
        let incoming =
            [json!({"role":"user","content":"Listen to the ending","media":[attachment]})];
        store
            .prepare(
                &session.id,
                &incoming,
                None,
                crate::ai::sessions::PrepareContext {
                    overhead: 0,
                    context_length: 1_000_000,
                    facts: &crate::ai::sessions::ProjectFacts::default(),
                    provider: "fixture",
                    model: "fixture",
                },
            )
            .unwrap();
        let caller = crate::state::panel_batch_owner(Some(&session.id));
        assert!(conversation_audio_source(&store, "http", path.to_str().unwrap()).is_err());
        assert!(conversation_audio_source(&store, &caller, "/unattached.wav").is_err());
        let (path, archived) =
            conversation_audio_source(&store, &caller, path.to_str().unwrap()).unwrap();
        assert!(!archived);
        let result = commands::audition_source_window(
            &path,
            None,
            70.0,
            0.1,
            commands::AuditionChannel::Mono,
            16_000,
        )
        .unwrap();
        assert_eq!(result.clip_index, None);
        assert_eq!(result.start_s, 70.0);
        assert!((result.duration_s - 0.1).abs() < 1e-6);
        use base64::Engine as _;
        let wav = base64::engine::general_purpose::STANDARD
            .decode(result.audio.base64)
            .unwrap();
        let reader = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap();
        assert!(
            reader
                .into_samples::<i16>()
                .any(|sample| sample.unwrap().abs() > 1000)
        );
    }

    #[test]
    fn saved_render_passages_are_available_only_to_the_owning_conversation() {
        use crate::ai::sessions::{ProjectFacts, Store, describe_audition_renders};
        let store = Store::new(oc_audio::private_fs::test_root().join("archived-audition-replay"));
        let session = store.create(None, None, None, None).unwrap();
        let other = store.create(None, None, None, None).unwrap();
        let dir = store.root().join("audition-renders");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{}.wav", "a".repeat(64)));
        let audio: Vec<_> = (0..32_000)
            .map(|frame| if frame < 28_800 { 0.0 } else { 0.4 })
            .collect();
        std::fs::write(
            &file,
            oc_engine::wav::mono_wav_bytes(&audio, 8_000).unwrap(),
        )
        .unwrap();
        let path = file.to_str().unwrap();
        let row = json!({
            "role": "user", "context": "audition", "source_render": path,
            "content": "This is the saved listening window."
        });
        let facts = ProjectFacts::default();
        store
            .prepare(
                &session.id,
                std::slice::from_ref(&row),
                None,
                crate::ai::sessions::PrepareContext {
                    overhead: 0,
                    context_length: 0,
                    facts: &facts,
                    provider: "test",
                    model: "test",
                },
            )
            .unwrap();
        store
            .prepare(
                &other.id,
                &[json!({"role":"user", "source_render": path})],
                None,
                crate::ai::sessions::PrepareContext {
                    overhead: 0,
                    context_length: 0,
                    facts: &facts,
                    provider: "test",
                    model: "test",
                },
            )
            .unwrap();

        let reopened = Store::new(store.root().to_path_buf());
        let caller = crate::state::panel_batch_owner(Some(&session.id));
        let other_caller = crate::state::panel_batch_owner(Some(&other.id));
        assert!(conversation_audio_source(&reopened, &other_caller, path).is_err());
        assert!(conversation_audio_source(&reopened, "http", path).is_err());
        let mut outbound = reopened.view(&session.id).unwrap();
        describe_audition_renders(reopened.root(), &mut outbound);
        assert!(outbound[0]["content"].as_str().unwrap().contains(path));
        assert!(outbound[0].get("media").is_none());
        assert_eq!(
            reopened.view(&session.id).unwrap()[0]["content"],
            row["content"]
        );

        let (source, archived) = conversation_audio_source(&reopened, &caller, path).unwrap();
        assert!(archived);
        let result = commands::audition_source_window(
            &source,
            None,
            3.8,
            0.4,
            commands::AuditionChannel::Mono,
            8_000,
        )
        .unwrap();
        assert_eq!(
            (result.start_s, result.end_s, result.source_duration_s),
            (3.8, 4.0, Some(4.0))
        );
        assert!(result.truncated);
        use base64::Engine as _;
        let wav = base64::engine::general_purpose::STANDARD
            .decode(result.audio.base64)
            .unwrap();
        let reader = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap();
        assert!(
            reader
                .into_samples::<i16>()
                .all(|sample| sample.unwrap() > 1000)
        );

        std::fs::remove_file(&file).unwrap();
        let mut outbound = reopened.view(&session.id).unwrap();
        describe_audition_renders(reopened.root(), &mut outbound);
        assert_eq!(outbound[0]["content"], row["content"]);
        assert!(conversation_audio_source(&reopened, &caller, path).is_err());
    }

    #[test]
    fn audio_analyze_resolves_close_file_attacks_without_a_false_window_attack() {
        let path = oc_audio::private_fs::test_root().join("processed-transients.wav");
        let rate = 48_000;
        let mut samples = vec![0.0; rate * 2];
        let mut fill = |first: usize, frames: usize, level: f32| {
            for frame in samples[2 * first..2 * (first + frames)].chunks_exact_mut(2) {
                frame.copy_from_slice(&[level, -level]);
            }
        };
        fill(9_600, 3_840, 0.25); // Sustained across the start of the analysis window.
        fill(16_800, 32, 0.5);
        fill(17_376, 32, 0.7);
        oc_engine::wav::write_stereo_f32_wav(&path, &samples, rate as u32).unwrap();

        let mut args = json!({
            "path": path, "transients": { "start": 0.25, "duration": 0.25, "min_gap_ms": 5 }
        });
        let report = audio_analyze_tool(&args).unwrap();
        let evidence = &report["transients"];
        assert_eq!(evidence["start_s"], 0.25);
        assert_eq!(evidence["end_s"], 0.5);
        let resolution = evidence["resolution_seconds"].as_f64().unwrap();
        assert_eq!(resolution, 64.0 / rate as f64);
        assert_eq!(evidence["effective_gap_ms"], 4.0 * resolution * 1000.0);
        let attacks = evidence["attacks"].as_array().unwrap();
        assert_eq!(attacks.len(), 2, "{evidence}");
        for (attack, expected) in attacks.iter().zip([0.362, 0.35]) {
            let timestamp = attack["file_seconds"].as_f64().unwrap();
            assert!(timestamp <= expected && expected - timestamp < resolution);
        }

        args["transients"]["min_gap_ms"] = Value::Null;
        let report = audio_analyze_tool(&args).unwrap();
        assert_eq!(report["transients"]["attacks"].as_array().unwrap().len(), 1);
        args["transients"]["start"] = json!(0.35);
        args["transients"]["min_gap_ms"] = json!(5);
        let report = audio_analyze_tool(&args).unwrap();
        let boundary = report["transients"]["attacks"][1]["file_seconds"]
            .as_f64()
            .unwrap();
        assert!((boundary - 0.35).abs() < 1.0e-12);
        args["transients"]["start"] = json!(0.750006);
        let report = audio_analyze_tool(&args).unwrap();
        assert_eq!(report["transients"]["end_s"], 1.0);
        args["transients"]["start"] = json!(0.9);
        assert!(
            audio_analyze_tool(&args)
                .unwrap_err()
                .contains("exceeds the source duration")
        );
        args["beat_grid"] = json!({ "duration": 4 });
        assert!(
            audio_analyze_tool(&args)
                .unwrap_err()
                .contains("either beat_grid or transients")
        );
    }

    #[test]
    fn audio_analyze_reports_warped_attacks_relative_to_a_saved_range_render() {
        let root = oc_audio::private_fs::test_root().join("rendered-attack-evidence");
        std::fs::create_dir_all(&root).unwrap();
        let rate = 48_000;
        let mut samples = vec![0.0; rate * 5 / 4];
        for (frame, gain) in [(0, 0.75), (9_600, 0.18), (19_200, 0.75), (38_400, 0.75)] {
            samples[frame..frame + 32].fill(gain);
        }
        std::fs::write(
            root.join("drums.wav"),
            oc_engine::wav::mono_wav_bytes(&samples, rate as u32).unwrap(),
        )
        .unwrap();
        let state = panel_state(
            r#"@project "Warp attack evidence"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "drums.wav"
  loop: 1.125s
@track tape | instrument: audio/clip
"#,
        );
        state.lock_inner().file_path = Some(root.join("song.ocp"));
        let check_attacks = |file: &str, expected: [f64; 4]| {
            let render = commands::render_range(
                root.join(file).display().to_string(),
                commands::RangeRequest {
                    start_s: Some(2.125),
                    end_s: Some(3.25),
                    tracks: Some(vec![0]),
                    float_wav: true,
                    ..Default::default()
                },
                &state,
            )
            .unwrap();
            let evidence = audio_analyze_tool(&json!({
                "path": render.path, "transients": {"duration": render.duration_s, "min_gap_ms": 5}
            }))
            .unwrap();
            let attacks = evidence["transients"]["attacks"].as_array().unwrap();
            let resolution = evidence["transients"]["resolution_seconds"]
                .as_f64()
                .unwrap();
            assert_eq!(attacks.len(), expected.len(), "{file}: {evidence}");
            for target in expected {
                assert!(
                    attacks.iter().any(|attack| {
                        let project_time =
                            render.start_s + attack["file_seconds"].as_f64().unwrap();
                        (project_time - target).abs() <= resolution + 0.002
                    }),
                    "{file}: expected an attack at {target} s: {evidence}"
                );
            }
        };
        check_attacks("before.wav", [2.2, 2.4, 2.8, 3.125]);

        let revision = state.lock_inner().revision;
        audio_clip_set_playback_request(
            &state,
            &json!({
                "clip_index": 0, "expected_revision": revision,
                "playback": {"stretch": 1.25, "stretch_mode": "percussive", "warp_markers": [
                    {"source_seconds": 0.4, "target_seconds": 0.5},
                    {"source_seconds": 0.8, "target_seconds": 1.0}
                ]}
            }),
            "panel:test",
        )
        .unwrap();
        // The ghost hit sits halfway to the first anchor. The repeat still
        // contributes one downbeat at the fixed 1.125 s loop boundary.
        check_attacks("after.wav", [2.25, 2.5, 3.0, 3.125]);
    }

    #[test]
    fn audio_analyze_measures_a_short_antiphase_window_and_an_actual_dropout() {
        let path = oc_audio::private_fs::test_root().join("short-level-window.wav");
        let mut samples = vec![0.0; 48_000 * 2];
        for frame in samples[24_000..27_840].chunks_exact_mut(2) {
            frame.copy_from_slice(&[0.5, -0.5]);
        }
        oc_engine::wav::write_stereo_f32_wav(&path, &samples, 48_000).unwrap();
        let args =
            |start| json!({"path": path, "level_window": {"start": start, "duration": 0.04}});
        let report = audio_analyze_tool(&args(0.25)).unwrap();
        assert_eq!(
            report["level_window"],
            json!({"start_s": 0.25, "end_s": 0.29})
        );
        assert_eq!(report["channels"], 2);
        let rms = report["level"]["rms_dbfs"].as_f64().unwrap();
        assert!((rms + 6.0206).abs() < 0.001);
        let dropout = audio_analyze_tool(&args(0.5)).unwrap();
        assert!(dropout["level"]["rms_dbfs"].as_f64().unwrap() < -100.0);
        assert!(
            audio_analyze_tool(&args(0.98))
                .unwrap_err()
                .contains("exceeds the source duration")
        );
        let mut conflicting = args(0.25);
        conflicting["transients"] = json!({"duration": 1});
        assert!(
            audio_analyze_tool(&conflicting)
                .unwrap_err()
                .contains("Choose one")
        );
    }

    #[test]
    fn audio_analyze_mono_fold_measures_band_loss_without_level_matching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fold.wav");
        let rate = 48_000;
        let samples: Vec<_> = (0..2 * rate)
            .flat_map(|frame| {
                let t = frame as f32 / rate as f32;
                let sub = 0.2 * (std::f32::consts::TAU * 63.0 * t).sin();
                let upper = 0.2 * (std::f32::consts::TAU * 500.0 * t).sin();
                if frame < rate {
                    [sub + upper, -sub + upper]
                } else {
                    [sub + upper, upper]
                }
            })
            .collect();
        oc_engine::wav::write_stereo_f32_wav(&path, &samples, rate).unwrap();
        let args = |start| json!({"path": path, "mono_fold": {"start": start, "duration": 1}});
        let first = audio_analyze_tool(&args(0)).unwrap();
        let second = audio_analyze_tool(&args(1)).unwrap();
        assert_eq!(second["window"], json!({"start_s": 1.0, "end_s": 2.0}));
        let band = |report: &Value, hz: f64| {
            report["mono_fold"]["bands"]
                .as_array()
                .unwrap()
                .iter()
                .find(|band| band["center_hz"] == hz)
                .unwrap()
                .clone()
        };
        let null = band(&first, 63.0);
        assert_eq!(null["mono_at_floor"], true);
        assert!(null["loss_db"].as_f64().unwrap() > 100.0);
        assert!(band(&first, 500.0)["loss_db"].as_f64().unwrap() < 0.0001);
        let one_side = band(&second, 63.0);
        assert_eq!(one_side["mono_at_floor"], false);
        assert!((one_side["loss_db"].as_f64().unwrap() - 3.0103).abs() < 0.0001);
        let mut conflicting = args(0);
        conflicting["level_window"] = json!({"duration": 0.04});
        assert!(
            audio_analyze_tool(&conflicting)
                .unwrap_err()
                .contains("Choose one")
        );
    }

    #[test]
    fn vocal_gap_analysis_separates_a_printed_tail_from_mixer_reverb() {
        use std::path::Path;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vocal.wav");
        let rate = 48_000;
        let mut samples = vec![0.0; rate * 2];
        for (start, end) in [(9_600, 21_600), (48_000, 60_000)] {
            for (frame, sample) in samples.iter_mut().enumerate().take(end).skip(start) {
                *sample = (frame as f32 * 0.0576).sin() * 0.35;
            }
        }
        // Print decay into the first phrase only. The second is a dry control.
        for (i, sample) in samples[21_600..38_400].iter_mut().enumerate() {
            *sample = (i as f32 * 0.0576).sin() * 0.08 * (-0.00025 * i as f32).exp();
        }
        std::fs::write(
            &source,
            oc_engine::wav::mono_wav_bytes(&samples, rate as u32).unwrap(),
        )
        .unwrap();
        let state = panel_state(
            r#"@project "Vocal tail evidence"
  tempo: 120
@audio_clip vocal [2:1 - 3:1] | track: vox
  file: "vocal.wav"
@track vox | instrument: audio/clip
  effects:
    - reverb: type=hall, decay=1.1s, mix=40%, bypass=true
"#,
        );
        state.lock_inner().file_path = Some(dir.path().join("song.ocp"));
        let render = |name: &str| {
            commands::render_range(
                dir.path().join(name).display().to_string(),
                commands::RangeRequest {
                    start_s: Some(2.125),
                    end_s: Some(3.875),
                    tracks: Some(vec![0]),
                    float_wav: true,
                    ..Default::default()
                },
                &state,
            )
            .unwrap()
        };
        let bypassed = render("bypassed.wav");
        let revision = state.lock_inner().revision;
        let args = json!({"target": "track:0", "index": 0, "bypass": false, "expected_revision": revision});
        effects_set_bypass_json(&state, &args, "panel:test").unwrap();
        let wet = render("wet.wav");
        let revision = state.lock_inner().revision;
        let args =
            json!({"target": "track:0", "index": 0, "bypass": true, "expected_revision": revision});
        effects_set_bypass_json(&state, &args, "panel:test").unwrap();
        assert!(state.lock_inner().project.tracks[0].effects[0].bypass);

        let gap_relative_to_vowel = |path: &Path, file_origin: f64, windows: [f64; 3]| {
            let [vowel, early, late] = windows.map(|start| {
                let report = audio_analyze_tool(&json!({
                    "path": path, "level_window": {"start": start - file_origin, "duration": 0.04}
                }))
                .unwrap();
                report["level"]["rms_dbfs"].as_f64().unwrap()
            });
            [early - vowel, late - vowel]
        };
        for (windows, has_printed_tail) in [([2.32, 2.50, 2.66], true), ([3.12, 3.31, 3.48], false)]
        {
            let raw = gap_relative_to_vowel(&source, 2.0, windows);
            let dry = gap_relative_to_vowel(Path::new(&bypassed.path), bypassed.start_s, windows);
            if has_printed_tail {
                for (raw, dry) in raw.into_iter().zip(dry) {
                    assert!((raw - dry).abs() < 0.3, "raw={raw}, bypassed={dry}");
                }
                assert!(raw[0] > -30.0 && raw[1] < raw[0] - 6.0, "raw={raw:?}");
            } else {
                assert!(raw.into_iter().chain(dry).all(|db| db < -100.0));
                let wet_gaps = gap_relative_to_vowel(Path::new(&wet.path), wet.start_s, windows);
                assert!(
                    wet_gaps[0] > -60.0 && wet_gaps[1] < wet_gaps[0] - 3.0,
                    "wet={wet_gaps:?}"
                );
            }
        }
    }

    #[test]
    fn effects_set_bypass_preserves_parameters_and_refuses_stale_or_foreign_edits() {
        let state = panel_state(
            "@project\n  tempo: 120\n@track vox | instrument: audio/clip\n  effects:\n    - reverb: decay=1.1s, mix=40%\n",
        );
        let (before, revision) = {
            let inner = state.lock_inner();
            (inner.project.clone(), inner.revision)
        };
        let mut args =
            json!({"target": "master", "index": 0, "bypass": true, "expected_revision": revision});
        assert!(effects_set_bypass_json(&state, &args, "panel:test").is_err());
        assert_eq!(state.lock_inner().project, before);

        transaction_begin(&state, None, "panel:test").unwrap();
        args["target"] = json!("track:0");
        assert!(effects_set_bypass_json(&state, &args, "other").is_err());
        effects_set_bypass_json(&state, &args, "panel:test").unwrap();
        transaction_commit(&state, "panel:test").unwrap();
        assert!(effects_set_bypass_json(&state, &args, "panel:test").is_err());

        let inner = state.lock_inner();
        let mut expected = before;
        expected.tracks[0].effects[0].bypass = true;
        assert_eq!(inner.project, expected);
        let revision = inner.revision;
        let undo_count = inner.undo.undo.len();
        drop(inner);
        args["expected_revision"] = json!(revision);
        let receipt = effects_set_bypass_json(&state, &args, "panel:test").unwrap();
        assert_eq!(
            receipt,
            json!({"target": "track:0", "index": 0, "effect": "reverb", "bypass": true, "revision": revision})
        );
        let inner = state.lock_inner();
        assert_eq!(inner.revision, revision);
        assert_eq!(inner.undo.undo.len(), undo_count);
    }

    #[test]
    fn audio_analyze_returns_beat_candidates_in_the_requested_source_window() {
        let dir = oc_audio::private_fs::test_root().join("beat-evidence");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reference.wav");
        let rate = 24_000;
        let mut samples = vec![0.0; 18 * rate];
        for hit in 0..16 {
            let offset = 5 * rate + rate / 4 + hit * rate / 2;
            for (i, sample) in samples[offset..].iter_mut().take(192).enumerate() {
                *sample = (i as f32 * 0.4).sin() * (-0.02 * i as f32).exp() * 0.7;
            }
        }
        std::fs::write(
            &path,
            oc_engine::wav::mono_wav_bytes(&samples, rate as u32).unwrap(),
        )
        .unwrap();
        let report = audio_analyze_tool(&json!({
            "path": path, "beat_grid": { "start": 5, "duration": 9 }
        }))
        .unwrap();
        assert_eq!(report["beat_grid"]["start_s"], 5.0);
        assert_eq!(report["beat_grid"]["end_s"], 14.0);
        let pulse = &report["beat_grid"]["candidates"][0];
        assert!((pulse["tempo_bpm"].as_f64().unwrap() - 120.0).abs() < 1.0);
        assert!((pulse["anchors_s"][0].as_f64().unwrap() - 5.25).abs() < 0.04);
        assert_eq!(report["downbeat_confirmed"], false);
        assert_eq!(report["time_signature"], Value::Null);
        assert!(report.get("level").is_none());
        assert!(
            audio_analyze_tool(&json!({
                "path": path, "beat_grid": { "start": 15, "duration": 8 }
            }))
            .unwrap_err()
            .contains("exceeds the source duration")
        );
    }

    #[test]
    fn audio_analyze_attached_media_uses_shared_decoder_and_preserves_wav() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../oc-engine/tests/fixtures/sampler/tone.flac");
        let decoded = oc_engine::audio_clip::decode(&source).unwrap();
        let report = audio_analyze_tool(&json!({"path": source, "top_peaks": 0})).unwrap();
        assert_eq!(report["sample_rate"], decoded.sample_rate);
        assert_eq!(report["channels"], 2);
        assert_eq!(
            report["duration_s"],
            decoded.frames() as f64 / f64::from(decoded.sample_rate)
        );
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().unwrap().contains("decoded to stereo"))
        );
        let dir = std::env::temp_dir().join(format!("oc-analyze-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("mono.wav");
        let mut writer = hound::WavWriter::create(
            &wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..1600 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        let actual = audio_analyze_tool(&json!({"path": wav, "top_peaks": 0})).unwrap();
        let expected =
            to_value(oc_engine::analysis::analyze_wav_with_peaks(&wav, 0).unwrap()).unwrap();
        assert_eq!(actual, expected);
        let invalid = dir.join("bad.mp3");
        std::fs::write(&invalid, b"not audio").unwrap();
        assert!(
            audio_analyze_tool(&json!({"path": invalid}))
                .unwrap_err()
                .contains("analysis failed")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_body_above_eight_mib_accepts_inline_audio_within_the_media_budget() {
        // A full-song 16 kHz mono WAV can exceed 8 MiB once base64 encoded.
        // Stream repeated chunks: only the parser allocates the complete body.
        let length = 11 * 1024 * 1024;
        assert!(length < crate::ai::media::MAX_REQUEST_INLINE_BYTES);
        const { assert!(MAX_BODY > crate::ai::media::MAX_REQUEST_INLINE_BYTES) };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let request = read_request(&mut socket).unwrap();
            assert_eq!(request.path, "/ai/complete");
            assert_eq!(request.body.len(), length);
            assert!(request.body.iter().all(|byte| *byte == b'A'));
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            client,
            "POST /ai/complete HTTP/1.1\r\nContent-Length: {length}\r\n\r\n"
        )
        .unwrap();
        let chunk = [b'A'; 16 * 1024];
        for _ in 0..length / chunk.len() {
            client.write_all(&chunk).unwrap();
        }
        client.flush().unwrap();
        server.join().unwrap();
    }

    /// Drive one raw request through the connection path and report what the
    /// client reads back, plus whether the router ran at all.
    fn connection_answer(request: &str) -> (String, bool) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let routed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = Arc::clone(&routed);
            handle_connection_with(&mut socket, move |_| {
                flag.store(true, Ordering::Relaxed);
                (200, "{}".to_string())
            })
            .unwrap();
            routed.load(Ordering::Relaxed)
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        client.write_all(request.as_bytes()).unwrap();
        client.flush().unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        (response, server.join().unwrap())
    }

    #[test]
    fn a_body_over_the_limit_answers_413_before_it_is_read() {
        let (response, routed) = connection_answer(&format!(
            "POST /ai/complete HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        ));
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(!routed, "the handler ran on a body that was never read");
    }

    #[test]
    fn a_chunked_body_is_refused_instead_of_read_as_empty() {
        let (response, routed) = connection_answer(
            "POST /ai/complete HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n\
             4\r\nbody\r\n0\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 411"), "{response}");
        assert!(!routed, "the handler ran on a body it never read");
    }

    #[test]
    fn a_content_length_that_does_not_parse_is_refused() {
        let (response, routed) =
            connection_answer("POST /ai/complete HTTP/1.1\r\nContent-Length: 12a\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!routed, "the handler ran without a body length");
    }

    #[test]
    fn the_connection_cap_refuses_past_the_limit_and_returns_slots() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut slots: Vec<ConnectionSlot> = (0..MAX_CONNECTIONS)
            .map(|_| ConnectionSlot::acquire(&count).expect("a slot below the cap"))
            .collect();
        assert_eq!(count.load(Ordering::Acquire), MAX_CONNECTIONS);
        assert!(ConnectionSlot::acquire(&count).is_none());
        assert_eq!(count.load(Ordering::Acquire), MAX_CONNECTIONS);

        // A connection that ends returns its slot to the next caller.
        slots.pop();
        assert_eq!(count.load(Ordering::Acquire), MAX_CONNECTIONS - 1);
        let reused = ConnectionSlot::acquire(&count).expect("the returned slot is reusable");
        drop(reused);
        drop(slots);
        assert_eq!(count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_poisoned_turn_lock_does_not_stop_later_turns() {
        let lock = std::sync::Mutex::new(());
        drop(lock_ai_turn(&lock));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock_ai_turn(&lock);
            panic!("a turn panicked");
        }));
        // The next turn still runs: one panicked provider turn must not wedge
        // the endpoint for every later caller.
        drop(lock_ai_turn(&lock));
    }

    #[test]
    fn plugin_work_times_out_instead_of_pinning_the_connection() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
        let error = await_main(&rx, Duration::from_millis(10)).unwrap_err();
        assert!(error.starts_with("main thread busy"), "{error}");

        tx.send(Ok(7)).unwrap();
        assert_eq!(await_main(&rx, Duration::from_secs(5)).unwrap(), 7);

        // A main thread that dropped the sender without answering.
        drop(tx);
        let error = await_main(&rx, Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("ended without a result"), "{error}");
    }

    const REFUSAL_PROJECT: &str = r#"@ocp "0.2.0"

@project "refusal fixture"
  tempo: 120
  time_signature: 4/4

@track piano | instrument: piano/acoustic_grand
  @phrase body [1:1 - 2:1]
    C4 q
"#;

    #[test]
    fn the_skill_tools_answer_through_this_dispatcher() {
        let index = dispatch_skill(ToolId::ListSkills, &json!({}))
            .expect("list_skills must be dispatched here")
            .unwrap();
        let entries = index.as_array().expect("list_skills returns an array");
        assert!(entries.len() >= 6, "{entries:?}");
        let mut keys: Vec<&str> = entries[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["category", "id", "source", "title", "version", "when"]
        );
        assert!(
            !index.to_string().contains("A subtree path"),
            "the index carried a body"
        );
        let id = entries
            .iter()
            .find(|entry| entry["id"] == "dsl-subtree-paths")
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .unwrap();
        let read = dispatch_skill(ToolId::ReadSkill, &json!({ "id": id }))
            .expect("read_skill must be dispatched here")
            .unwrap();
        let mut keys: Vec<&str> = read
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["body", "id", "title", "tools", "version"]);
        assert!(read["body"].as_str().unwrap().contains("subtree"));
        assert_eq!(read["tools"][0], "project_extract");
        assert!(
            dispatch_skill(ToolId::ReadSkill, &json!({}))
                .unwrap()
                .is_err()
        );
        assert!(
            dispatch_skill(ToolId::ReadSkill, &json!({ "id": "nope" }))
                .unwrap()
                .is_err()
        );
        let exercise = dispatch_skill(
            ToolId::ReadSkill,
            &json!({"id": "production-curriculum", "topic": "Singeli"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(exercise["title"], "Singeli");
        assert!(exercise["body"].as_str().unwrap().contains("MC phrase"));
        assert!(
            dispatch_skill(
                ToolId::ReadSkill,
                &json!({"id": "writing-notes", "topic": "Singeli"}),
            )
            .unwrap()
            .is_err()
        );
        assert!(dispatch_skill(ToolId::TracksMix, &json!({})).is_none());
    }

    #[test]
    fn a_path_refusal_names_the_path_skill_and_keeps_the_error_exact() {
        // Each string comes from the oc_dsl call the handler already makes, so a
        // reworded refusal stops matching this table instead of rotting quietly.
        let project = oc_dsl::parse(REFUSAL_PROJECT).unwrap();
        let mut candidate = project.clone();
        let refusals = [
            oc_dsl::ops::extract(&project, "doc").unwrap_err(),
            oc_dsl::ops::replace(&mut candidate, "automation", "automation: volume {1:1=0}")
                .unwrap_err(),
            oc_dsl::ops::replace(&mut candidate, "track:piano.properties", "tempo: 90")
                .unwrap_err(),
        ];
        assert_eq!(
            refusals,
            [
                "Unknown path segment: doc".to_owned(),
                "Unsupported replace path".to_owned(),
                "Invalid track child for replace".to_owned(),
            ]
        );
        for (message, expected) in refusals.iter().zip([
            "Unknown path segment: doc",
            "Unsupported replace path",
            "Invalid track child for replace",
        ]) {
            let payload: Value = serde_json::from_str(&error_body(message.clone())).unwrap();
            assert_eq!(payload["error"], expected);
            assert_eq!(payload["skill"], "dsl-subtree-paths", "{message}");
            assert_eq!(payload["ok"], false);
        }
    }

    #[test]
    fn a_successful_call_carries_no_skill_field() {
        let body: Value =
            serde_json::from_str(&ok_body(json!({ "path": "/tmp/range.wav" }))).unwrap();
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["ok", "result"]);
        assert_eq!(body["ok"], true);
        // A refusal no skill explains keeps the old two-key payload.
        let plain: Value = serde_json::from_str(&error_body("track 4 out of range")).unwrap();
        assert!(plain.get("skill").is_none(), "{plain}");
        assert!(plain.get("hint").is_none(), "{plain}");
    }

    #[test]
    fn the_mcp_failure_block_keeps_the_error_and_adds_the_hint() {
        let hinted = mcp_error("Track 'piano' needs a SoundFont: declare preset.file".to_owned());
        assert_eq!(hinted["isError"], true);
        assert_eq!(
            hinted["content"][0]["text"],
            "Track 'piano' needs a SoundFont: declare preset.file"
        );
        assert_eq!(
            hinted["content"][1]["text"],
            "read_skill('playable-instruments') teaches this"
        );
        assert_eq!(hinted["content"].as_array().unwrap().len(), 2);

        let plain = mcp_error("track 4 out of range".to_owned());
        assert_eq!(plain["content"].as_array().unwrap().len(), 1);
        assert_eq!(plain["content"][0]["text"], "track 4 out of range");
    }

    #[test]
    fn token_is_hex_and_stable_length() {
        let token = make_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn auth_accepts_bearer_and_header() {
        let headers = vec![("Authorization".to_string(), "Bearer abc".to_string())];
        assert!(authorized(&headers, "abc"));
        let headers = vec![("X-OpenCompose-Token".to_string(), "abc".to_string())];
        assert!(authorized(&headers, "abc"));
        assert!(!authorized(&headers, "def"));
    }

    #[test]
    fn params_accept_object_and_pairs() {
        let pairs = params_to_pairs(&json!({"cc1": 64, "state_file": "/tmp/x"})).unwrap();
        assert!(
            pairs
                .iter()
                .any(|(key, value)| key == "cc1" && value.as_deref() == Some("64"))
        );
        let pairs = params_to_pairs(&json!([["cc1", 64], ["mute", null]])).unwrap();
        assert_eq!(pairs[0].0, "cc1");
        assert_eq!(pairs[1].1, None);
    }

    #[test]
    fn preset_params_accept_every_shape_its_description_promises() {
        const PROJECT: &str = "@project \"Params\"\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {amp.decay: 0.2s}\n@track lead | preset: tone, instrument: synth/lead\n  @phrase riff [1:1:0 - 2:1:0]\n    C4 q mf\n";
        // An object, an array of pairs, and the JSON string a model produces
        // when it serializes the object itself.
        let shapes = [
            json!({"amp.decay": "0.45s"}),
            json!([["amp.decay", "0.45s"]]),
            json!("{\"amp.decay\": \"0.45s\"}"),
        ];
        let mut rendered = Vec::new();
        for shape in shapes {
            let pairs = params_to_pairs(&shape).unwrap();
            assert_eq!(
                pairs,
                vec![("amp.decay".to_string(), Some("0.45s".to_string()))]
            );
            let mut project = oc_dsl::parse(PROJECT).unwrap();
            commands::apply_preset_params(&mut project, "tone", pairs).unwrap();
            rendered.push(oc_dsl::serialize(&project));
        }
        assert_eq!(rendered[0], rendered[1]);
        assert_eq!(rendered[1], rendered[2]);
        assert!(rendered[0].contains("amp.decay: 450ms"), "{}", rendered[0]);
    }

    #[test]
    fn preset_params_refusals_name_what_arrived_and_show_one_example() {
        let refused = [
            params_to_pairs(&json!("amp.decay=0.45s")).unwrap_err(),
            params_to_pairs(&json!([1, 2])).unwrap_err(),
            params_to_pairs(&json!([["amp.decay"]])).unwrap_err(),
        ];
        assert_eq!(
            refused[0],
            "params: received a string that is not JSON, expected [name, value] pairs or an object map {name: value}, or a JSON string of either, e.g. {\"amp.decay\": \"0.45s\"}"
        );
        assert_eq!(
            refused[1],
            "params: received an array of numbers, expected [name, value] pairs or an object map {name: value}, or a JSON string of either, e.g. [[\"amp.decay\", \"0.45s\"]]"
        );
        assert_eq!(
            refused[2],
            "params: received an array of 1-item arrays, expected [name, value] pairs or an object map {name: value}, or a JSON string of either, e.g. [[\"amp.decay\", \"0.45s\"]]"
        );
    }

    #[test]
    fn render_range_schema_accepts_seconds_or_bars() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "render_range")
            .expect("render_range is listed");
        assert_eq!(tool["inputSchema"]["required"], json!(["path"]));
        let properties = &tool["inputSchema"]["properties"];
        assert_eq!(properties["start"]["type"], "number");
        assert_eq!(properties["end"]["type"], "number");
        assert_eq!(properties["start_bar"]["type"], "integer");
        assert_eq!(properties["end_bar"]["type"], "integer");
        assert_eq!(properties["stems"]["type"], "boolean");
    }

    #[test]
    fn audition_schema_takes_the_same_window_without_a_required_path() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool["name"] == "audition_range")
            .expect("audition_range is listed");
        assert_eq!(tool["inputSchema"]["required"], json!([]));
        let properties = &tool["inputSchema"]["properties"];
        assert_eq!(properties["path"]["type"], "string");
        assert_eq!(properties["start"]["type"], "number");
        assert_eq!(properties["end_bar"]["type"], "integer");
        assert_eq!(properties["tracks"]["type"], "array");
        assert!(properties["stems"].is_null());
    }

    #[test]
    fn analysis_schemas_require_paths_and_expose_options() {
        let tools = tools();
        let analyze = tools
            .iter()
            .find(|tool| tool["name"] == "audio_analyze")
            .expect("audio_analyze is listed");
        assert_eq!(analyze["inputSchema"]["required"], json!(["path"]));
        assert_eq!(
            analyze["inputSchema"]["properties"]["top_peaks"]["type"],
            "integer"
        );
        let compare = tools
            .iter()
            .find(|tool| tool["name"] == "audio_compare")
            .expect("audio_compare is listed");
        assert_eq!(
            compare["inputSchema"]["required"],
            json!(["reference", "candidate"])
        );
        assert_eq!(
            compare["inputSchema"]["properties"]["allow_mismatch"]["type"],
            "boolean"
        );
    }

    #[test]
    fn project_version_sensitive_tools_advertise_the_revision_guard() {
        let guarded_exports = ["midi_export_handoff", "preset_bundle_export"];
        for entry in tools() {
            let name = entry["name"].as_str().unwrap();
            let property = &entry["inputSchema"]["properties"]["expected_revision"];
            if is_mutation_tool(name) || guarded_exports.contains(&name) {
                assert_eq!(property["type"], "integer", "{name}");
            } else {
                assert!(
                    property.is_null(),
                    "{name} unexpectedly takes a project revision"
                );
            }
            if guarded_exports.contains(&name) {
                assert!(
                    !is_mutation_tool(name),
                    "{name} exports a snapshot without editing the project"
                );
                assert!(
                    entry["inputSchema"]["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|field| field == "expected_revision"),
                    "{name} must identify the committed version it exports"
                );
            }
        }
        assert!(is_mutation_tool("tracks_mix"));
        assert!(is_mutation_tool("project_edit"));
        assert!(!is_mutation_tool("audio_compare"));
        assert!(!is_mutation_tool("render_range"));
        assert!(!is_mutation_tool("audition_range"));
    }

    #[test]
    fn an_invalid_validation_is_not_an_ok_tool_response() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  @phrase riff [1:1:0 - 2:1:0]\n    C4 q mf\n",
        );
        let invalid = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "@track x | instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 q mf\n".into(),
                "doc".into(),
            ))
        };
        let envelope = tool_response(invalid);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert!(!envelope["result"]["errors"].as_array().unwrap().is_empty());

        let valid = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "tempo: 132".into(),
                "project.properties".into(),
            ))
        };
        let envelope = tool_response(valid);
        assert_eq!(envelope["ok"], json!(true));
        assert_eq!(envelope["result"]["valid"], json!(true));

        // project_edit's pre-check runs the same validation with a project
        // directory: a missing @preset file is a structured failure, not a write.
        let dir = temp_dir("validation-media");
        state.lock_inner().file_path = Some(dir.join("song.ocp"));
        let missing = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "@preset grand\n  file: \"media/grand.sf2\"\n".into(),
                "preset:grand".into(),
            ))
        };
        let envelope = tool_response(missing);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert!(
            envelope["result"]["errors"][0]
                .as_str()
                .unwrap()
                .contains("media/grand.sf2")
        );
        assert!(
            envelope["result"]["error"]
                .as_str()
                .unwrap()
                .contains("media/grand.sf2"),
            "the joined error must name the path: {}",
            envelope["result"]["error"]
        );
        assert_eq!(
            envelope["result"]["skill"],
            json!("playable-instruments"),
            "{envelope}"
        );
        assert!(
            envelope["result"]["hint"]
                .as_str()
                .unwrap()
                .contains("read_skill('playable-instruments')"),
            "{}",
            envelope["result"]["hint"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_invalid_validation_carries_the_joined_errors_as_a_top_level_error() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track lead | instrument: synth/lead_square\n  @phrase riff [1:1:0 - 2:1:0]\n    C4 q mf\n",
        );
        // Bare words are not a track: a parse failure, not a path failure.
        let invalid = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "not DSL at all".into(),
                "track:lead".into(),
            ))
        };
        let envelope = tool_response(invalid);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert_eq!(envelope["result"]["parsed_dsl"], Value::Null);
        let errors = envelope["result"]["errors"].as_array().unwrap();
        assert!(!errors.is_empty());
        let joined = errors
            .iter()
            .map(|error| error.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(!joined.is_empty());
        // `/call` callers read `error` at the top level; MCP callers read the
        // result text, so both carry the joined message.
        assert_eq!(envelope["error"], json!(joined));
        assert_eq!(envelope["result"]["error"], json!(joined));
        // A parse failure is a syntax failure, so it names the note manual.
        assert_eq!(envelope["skill"], json!("writing-notes"));
        assert_eq!(envelope["result"]["skill"], json!("writing-notes"));
        // The hint is the prose call, at both levels a caller reads.
        assert_eq!(
            envelope["hint"],
            json!("read_skill('writing-notes') teaches this")
        );
        assert_eq!(
            envelope["result"]["hint"],
            json!("read_skill('writing-notes') teaches this")
        );

        let valid = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "tempo: 132".into(),
                "project.properties".into(),
            ))
        };
        let envelope = tool_response(valid);
        assert_eq!(envelope["ok"], json!(true));
        assert_eq!(envelope["result"]["valid"], json!(true));
        assert!(envelope.get("error").is_none(), "{envelope}");
        assert!(envelope["result"].get("error").is_none(), "{envelope}");
    }

    #[test]
    fn a_path_validation_failure_keeps_the_subtree_skill() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track lead | instrument: synth/lead_square\n",
        );
        let refused = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "tempo: 110".into(),
                "doc".into(),
            ))
        };
        let envelope = tool_response(refused);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert_eq!(envelope["error"], json!("Unknown path segment: doc"));
        assert_eq!(
            envelope["result"]["errors"],
            json!(["Unknown path segment: doc"])
        );
        assert_eq!(envelope["skill"], json!("dsl-subtree-paths"));
        assert_eq!(envelope["result"]["skill"], json!("dsl-subtree-paths"));
        assert_eq!(
            envelope["hint"],
            json!("read_skill('dsl-subtree-paths') teaches this")
        );
        assert_eq!(
            envelope["result"]["hint"],
            json!("read_skill('dsl-subtree-paths') teaches this")
        );
    }

    #[test]
    fn a_parse_refusal_names_writing_notes_through_the_validation_envelope() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track reference | instrument: synth/lead\n  @phrase riff [1:1:0 - 2:1:0]\n    C4 q mf\n",
        );
        // The wrapper `oc_dsl::ops` adds is four lines, so the bad pitch sits at
        // line 8, column 5, exactly where the live model hit it.
        let refused = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  @phrase test [1:1:0 - 5:1:0]\n    H4 q mf\n".into(),
                "track:reference".into(),
            ))
        };
        let envelope = tool_response(refused);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert_eq!(
            envelope["error"],
            json!(
                "Parse error: Syntax error at line 8, column 5: expected EOI, top_level_block, track_content, or phrase_event"
            )
        );
        assert_eq!(envelope["error"], envelope["result"]["error"]);
        assert_eq!(envelope["skill"], json!("writing-notes"));
        assert_eq!(envelope["result"]["skill"], json!("writing-notes"));
        // The manual the refusal names answers the message it carries.
        let manual = crate::ai::skills::read("writing-notes").expect("bundled");
        assert!(manual.body.contains("Parse error"), "{}", manual.body);
    }

    /// Invalid effect parameters carry the skill hint at both envelope levels.
    #[test]
    fn an_effect_param_refusal_names_the_effects_skill_and_the_hint() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track reference | instrument: audio/clip\n  mix: vol=-3dB\n",
        );
        let bad = "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  \
                   effects:\n    - eq:\n        low_freq: 300Hz\n";
        let refused = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                bad.into(),
                "track:reference".into(),
            ))
        };
        let envelope = tool_response(refused);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        assert_eq!(
            envelope["error"],
            json!("Parse error: Syntax error at line 9, column 9: expected effect_param")
        );
        assert_eq!(envelope["error"], envelope["result"]["error"]);
        assert_eq!(envelope["skill"], json!("effects-and-devices"));
        assert_eq!(envelope["result"]["skill"], json!("effects-and-devices"));
        assert_eq!(
            envelope["hint"],
            json!("read_skill('effects-and-devices') teaches this")
        );
        assert_eq!(
            envelope["result"]["hint"],
            json!("read_skill('effects-and-devices') teaches this")
        );
    }

    /// The capability run wrote `eq: high_gain=+2dB@8kHz`: the DSL validator
    /// accepted it and `ocp render` refused it. `project_validate` must refuse
    /// it too, and the refusal must name the value and an accepted spelling.
    #[test]
    fn project_validate_refuses_a_typed_effect_value_the_render_refuses() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track reference | instrument: synth/melody\n",
        );
        let bad = "@track reference | instrument: synth/melody\n  \
                   effects:\n    - eq: high_gain=+2dB@8kHz\n";
        let refused = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                bad.into(),
                "track:reference".into(),
            ))
        };
        let envelope = tool_response(refused);
        assert_eq!(envelope["ok"], json!(false));
        assert_eq!(envelope["result"]["valid"], json!(false));
        let error = envelope["error"].as_str().expect("an error string");
        assert!(error.contains("high_gain=+2dB@8kHz"), "{error}");
        assert!(error.contains("high_gain=0dB"), "{error}");
        // The refusal is still an effect-parameter one, so the panel still
        // points the model at the manual that teaches the accepted names.
        assert_eq!(envelope["skill"], json!("effects-and-devices"));
    }

    #[test]
    fn a_clean_effect_edit_still_validates() {
        let state = panel_state(
            "@project \"Panel\"\n  tempo: 120\n@track reference | instrument: synth/melody\n",
        );
        let good = "@track reference | instrument: synth/melody\n  \
                    effects:\n    - eq: high_gain=+2dB, high_freq=8kHz\n";
        let accepted = {
            let inner = state.lock_inner();
            validation_value(commands::validate_edit_in_project(
                &inner,
                good.into(),
                "track:reference".into(),
            ))
        };
        let envelope = tool_response(accepted);
        assert_eq!(envelope["ok"], json!(true));
        assert_eq!(envelope["result"]["valid"], json!(true));
    }

    #[test]
    fn tool_names_are_unique() {
        let mut names = std::collections::HashSet::new();
        for tool in tools() {
            let name = tool["name"].as_str().unwrap();
            assert!(names.insert(name.to_string()), "duplicate tool {name}");
        }
    }

    #[test]
    fn authenticated_tools_expose_the_expected_identities_and_registry_metadata() {
        let unexpected_call =
            |_: &[u8]| -> (u16, String) { panic!("listing tools executed a call") };
        let unauthorized = route_with(
            &http_request("GET", "/tools", None, ""),
            "contract-token",
            unexpected_call,
            unexpected_call,
            unexpected_call,
        );
        assert_eq!(unauthorized.0, 401);
        let (status, body) = route_with(
            &http_request("GET", "/tools", Some("contract-token"), ""),
            "contract-token",
            unexpected_call,
            unexpected_call,
            unexpected_call,
        );
        assert_eq!(status, 200);
        let body: Value = serde_json::from_str(&body).expect("tool-list JSON");
        let exposed = body["tools"].as_array().expect("tool definitions");
        let mut names: Vec<_> = exposed
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        let expected_names = [
            "analyze_notes",
            "audio_analyze",
            "audio_clip_edit_range",
            "audio_clip_set_placement",
            "audio_clip_set_playback",
            "audio_clip_transients",
            "audio_clips_list",
            "audio_compare",
            "audio_config",
            "audio_devices",
            "audition_clip_source",
            "audition_range",
            "buses_add",
            "buses_mix",
            "buses_remove",
            "buses_routes",
            "content_libraries",
            "content_library_register",
            "content_library_remove",
            "content_library_rescan",
            "device_preset_apply",
            "device_presets_list",
            "devices_catalog",
            "effects_add",
            "effects_remove",
            "effects_set_bypass",
            "effects_set_param",
            "import_aligned_stems",
            "list_skills",
            "meter_snapshot",
            "midi_export",
            "midi_export_handoff",
            "midi_import_apply",
            "midi_import_handoff",
            "midi_import_preview",
            "picture_clock_set",
            "picture_timecode",
            "plugin_index_refresh",
            "plugin_pull",
            "plugin_search",
            "plugins_apply_state",
            "plugins_close_gui",
            "plugins_has_gui",
            "plugins_load",
            "plugins_open_gui",
            "plugins_params",
            "plugins_program_capabilities",
            "plugins_programs",
            "plugins_read_state",
            "plugins_save_state",
            "plugins_scan",
            "plugins_select_program",
            "preset_bundle_export",
            "preset_bundle_import",
            "presets_add",
            "presets_duplicate",
            "presets_list",
            "presets_remove",
            "presets_rename",
            "presets_set_param",
            "presets_set_params",
            "project_context",
            "project_diff",
            "project_dsl",
            "project_edit",
            "project_extract",
            "project_generate_context",
            "project_get",
            "project_new",
            "project_open",
            "project_save",
            "project_set_name",
            "project_set_tempo",
            "project_validate",
            "read_skill",
            "redo",
            "render_compat_report",
            "render_offline",
            "render_range",
            "render_stems",
            "sample_pull",
            "sample_search",
            "sampler_assets",
            "sections_add",
            "sections_list",
            "sections_remove",
            "sections_set",
            "separate_stems",
            "sessions_compact",
            "sessions_delete",
            "sessions_list",
            "sessions_load",
            "sessions_new",
            "sessions_rename",
            "soundfont_install",
            "soundfonts_list",
            "soundfonts_remove",
            "system_capabilities",
            "tempo_map_set",
            "time_signature_set",
            "tracks_add",
            "tracks_create",
            "tracks_duplicate",
            "tracks_list",
            "tracks_mix",
            "tracks_mute",
            "tracks_output",
            "tracks_preset",
            "tracks_remove",
            "tracks_rename",
            "tracks_send_add",
            "tracks_send_remove",
            "tracks_solo",
            "transaction_begin",
            "transaction_commit",
            "transaction_rollback",
            "transport_mute",
            "transport_pause",
            "transport_play",
            "transport_seek",
            "transport_state",
            "transport_stop",
            "transport_unmute",
            "undo",
            "undo_history",
            "undo_state",
        ];
        assert_eq!(names, expected_names, "tool identity contract changed");
        let definitions = crate::tool_registry::catalog();
        for tool in exposed {
            let name = tool["name"].as_str().unwrap();
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .expect("every public tool has an owning definition");
            assert_eq!(
                tool["description"], definition.description,
                "{name} description"
            );
            assert_eq!(tool["inputSchema"], definition.schema, "{name} schema");
            let mut fields: Vec<_> = tool
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            fields.sort_unstable();
            assert_eq!(
                fields,
                ["description", "inputSchema", "name"],
                "{name} public fields"
            );
        }
    }

    #[test]
    fn the_endpoint_file_is_removed_only_by_the_process_that_wrote_it() {
        let path = crate::private_fs::config_dir()
            .join(format!("agent-test-endpoint-{}.json", std::process::id()));
        let write = |pid: u32, port: u16| {
            let endpoint = Endpoint {
                port,
                token: "fixture".into(),
                pid,
                version: VERSION.into(),
            };
            crate::private_fs::write_atomic(&path, &serde_json::to_vec(&endpoint).unwrap())
                .unwrap();
        };

        // Another process's live endpoint stays for its owner.
        write(std::process::id() + 1, 40000);
        assert!(!remove_owned_endpoint(&path, 40000).unwrap());
        assert!(path.exists());

        // Our pid on another port is a different instance's record too.
        write(std::process::id(), 40001);
        assert!(!remove_owned_endpoint(&path, 40000).unwrap());
        assert!(path.exists());

        // Our own record is removed; a second exit finds nothing left.
        assert!(remove_owned_endpoint(&path, 40001).unwrap());
        assert!(!path.exists());
        assert!(!remove_owned_endpoint(&path, 40001).unwrap());

        // A file that does not parse is not provably ours.
        crate::private_fs::write_atomic(&path, b"not an endpoint").unwrap();
        assert!(!remove_owned_endpoint(&path, 40001).unwrap());
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    fn tool_names(body: &str) -> Vec<String> {
        let value: Value = serde_json::from_str(body).unwrap();
        value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn the_headless_tool_surface_is_the_panel_surface() {
        // No query is the panel's default: enabled tools plus its local one.
        let names = tool_names(&panel_tools_body("/ai/tools"));
        let expected: Vec<String> = crate::tool_registry::panel_tools_for(None)
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(names, expected);
        assert!(names.contains(&"edit_subtree".to_string()));
        assert!(names.contains(&"audition_range".to_string()));
        // The MCP-only catalog stays out: the panel refuses these.
        assert!(!names.contains(&"render_offline".to_string()));
        assert!(!names.contains(&"project_open".to_string()));

        // A selection narrows it exactly as the command's argument does.
        let narrowed = tool_names(&panel_tools_body("/ai/tools?toolsets=dsl-edit"));
        assert_eq!(
            narrowed,
            crate::ai::toolsets::resolve(&["dsl-edit".to_string()])
                .into_iter()
                .filter(|name| names.contains(name))
                .collect::<Vec<_>>()
        );
        assert!(narrowed.contains(&"read_skill".to_string()));
        assert!(!narrowed.contains(&"audition_range".to_string()));

        // `?toolsets=` is an explicit empty selection, not the whole surface.
        assert!(tool_names(&panel_tools_body("/ai/tools?toolsets=")).is_empty());
    }

    fn panel_state(project: &str) -> AppState {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(project).unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        state
    }

    /// A preview fixture: one pattern, one clip, and a proposal that bumps a note.
    const PREVIEW_SONG: &str = r#"@ocp "0.2.0"

@project "agent preview"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf

@track keys | instrument: piano/acoustic_grand
  clip: riff [1:1 - 3:1]
"#;

    const PREVIEW_BUMPER: &str = r#"@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 A4 q mf
"#;

    /// Open a preview on `state` so the guard has something to refuse.
    fn open_preview(state: &AppState) {
        let mut inner = state.lock_inner();
        let base = inner.dsl_source.clone();
        crate::commands::preview::begin_inner(
            &mut inner,
            PREVIEW_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base,
        )
        .expect("the preview opens");
    }

    #[test]
    fn typed_revision_guard_matches_the_existing_optional_integer_decoder() {
        use crate::tool_registry::arguments::RevisionGuard;
        let mut cases = vec![
            Value::Null,
            json!([]),
            json!("legacy"),
            json!(true),
            json!({}),
            json!({"expected_revision":null}),
            json!({"expected_revision":0}),
            json!({"expected_revision":u64::MAX}),
            json!({"expected_revision":-1}),
            json!({"expected_revision":1.5}),
            json!({"expected_revision":"1"}),
            json!({"expected_revision":true}),
            json!({"expected_revision":[]}),
            json!({"expected_revision":{}}),
        ];
        cases.push(serde_json::from_str(r#"{"expected_revision":18446744073709551616}"#).unwrap());
        // The pre-migration helper this guard replaced.
        fn old_guard(value: &Value) -> Result<Option<u64>, String> {
            match value.get("expected_revision") {
                None | Some(Value::Null) => Ok(None),
                Some(raw) => serde_json::from_value(raw.clone())
                    .map(Some)
                    .map_err(|error| format!("argument 'expected_revision': {error}")),
            }
        }
        for value in cases {
            let old = old_guard(&value);
            let typed = RevisionGuard::decode(ToolId::ProjectSetName, &value)
                .map(RevisionGuard::expected)
                .map_err(|error| error.to_string());
            assert_eq!(typed, old, "payload {value}");
        }
    }

    #[test]
    fn typed_revision_decode_stays_after_preview_and_batch_policy() {
        let malformed = json!({"expected_revision":"not a number"});
        let preview = panel_state(PREVIEW_SONG);
        open_preview(&preview);
        assert_eq!(
            guard_mutation(&preview, "project_set_name", &malformed, "caller").unwrap_err(),
            crate::preview::READ_ONLY
        );
        let state = panel_state(PREVIEW_SONG);
        let before = state.lock_inner().dsl_source.clone();
        state
            .begin_transaction("Owner batch".into(), "owner".into())
            .unwrap();
        let refused = guard_mutation(&state, "project_set_name", &malformed, "other").unwrap_err();
        assert_eq!(refused, foreign_batch_error("owner", "other"));
        assert_eq!(state.lock_inner().dsl_source, before);
        let error = guard_mutation(&state, "project_set_name", &malformed, "owner").unwrap_err();
        assert!(error.starts_with("argument 'expected_revision': "));
        assert_eq!(state.batch_owner().as_deref(), Some("owner"));
    }

    #[test]
    fn an_open_preview_refuses_every_mutating_tool() {
        let state = panel_state(PREVIEW_SONG);
        open_preview(&state);
        // The registry's mutating set is exactly what the guard covers.
        for tool in ["tracks_add", "project_edit", "undo", "transaction_begin"] {
            assert_eq!(
                guard_mutation(&state, tool, &json!({}), "test").unwrap_err(),
                crate::preview::READ_ONLY,
                "{tool}"
            );
        }
        // A read-only tool is never refused, and the refusal outranks a revision
        // check that would otherwise pass.
        assert!(guard_mutation(&state, "tracks_list", &json!({}), "test").is_ok());
        assert_eq!(
            guard_mutation(
                &state,
                "tracks_mute",
                &json!({ "expected_revision": 0 }),
                "test"
            )
            .unwrap_err(),
            crate::preview::READ_ONLY
        );
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oc-panel-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn panel_range_render_analyze_and_compare_round_trip() {
        let state = panel_state(
            "@project \"Panel test\"\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase riff [1:1:0 - 3:1:0]\n    C4 q mf\n",
        );
        for name in [
            "render_range",
            "audio_analyze",
            "audio_compare",
            "transaction_begin",
            "transaction_commit",
            "transaction_rollback",
        ] {
            assert!(crate::tool_registry::panel_tool(name).is_ok(), "{name}");
        }
        let dir = temp_dir("range");
        let master = dir.join("range.wav");
        let args = json!({
            "path": master.to_string_lossy(),
            "start": 0.0,
            "end": 1.0,
            "stems": true,
        });
        let def = crate::tool_registry::panel_tool("render_range").unwrap();
        crate::tool_registry::validate_args(&def, &args).unwrap();
        let rendered = commands::render_range(
            args["path"].as_str().unwrap().to_string(),
            commands::RangeRequest {
                start_s: Some(0.0),
                end_s: Some(1.0),
                stems: true,
                ..Default::default()
            },
            &state,
        )
        .unwrap();
        assert_eq!(rendered.sample_rate, 44_100);
        assert_eq!(rendered.frames, 44_100);
        assert_eq!(rendered.stems.len(), 1);
        assert!(std::path::Path::new(&rendered.stems[0].path).exists());
        assert!(!rendered.cache_hit, "the first render has no warm cache");

        // Bars resolve through the project clock and reuse the warm render.
        let bar_path = dir.join("bars.wav");
        let bar_args = json!({
            "path": bar_path.to_string_lossy(),
            "start_bar": 1,
            "end_bar": 2,
        });
        crate::tool_registry::validate_args(&def, &bar_args).unwrap();
        let bar = commands::render_range(
            bar_args["path"].as_str().unwrap().to_string(),
            commands::RangeRequest {
                start_bar: Some(1),
                end_bar: Some(2),
                ..Default::default()
            },
            &state,
        )
        .unwrap();
        assert_eq!(bar.start_s, 0.0);
        assert_eq!(bar.end_s, 2.0);
        assert_eq!(bar.frames, 2 * 44_100);
        assert!(bar.cache_hit, "a second window must not re-render");

        let analyze = crate::tool_registry::panel_tool("audio_analyze").unwrap();
        let analyze_args = json!({ "path": master.to_string_lossy() });
        crate::tool_registry::validate_args(&analyze, &analyze_args).unwrap();
        let report = oc_engine::analysis::analyze_wav(master.to_string_lossy().as_ref()).unwrap();
        assert_eq!(report.sample_rate, 44_100);
        assert!((report.duration_s - 1.0).abs() < 0.01);
        assert!(report.level.peak_dbfs.is_finite());
        assert!(report.true_peak_dbfs.is_finite());
        assert!(!report.spectral_peaks.peaks.is_empty());
        assert!(report.warnings.len() < 10);

        let compare = crate::tool_registry::panel_tool("audio_compare").unwrap();
        let compare_args = json!({
            "reference": master.to_string_lossy(),
            "candidate": rendered.stems[0].path,
        });
        crate::tool_registry::validate_args(&compare, &compare_args).unwrap();
        let report = oc_engine::analysis::compare_wavs(
            compare_args["reference"].as_str().unwrap(),
            compare_args["candidate"].as_str().unwrap(),
            oc_engine::analysis::CompareOptions::default(),
        )
        .unwrap();
        assert!(report.score.value.is_finite());
        assert!(report.score.value > 0.0);
        assert!(report.sample.max_abs_difference >= 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panel_transaction_guards_and_rolls_back() {
        let state = panel_state("@project \"Batch\"\n  tempo: 120\n");
        let begin = transaction_begin(&state, Some("panel batch".into()), "test").unwrap();
        assert_eq!(begin["active"], true);
        let base = begin["revision"].as_u64().unwrap();
        {
            let mut inner = state.lock_inner();
            inner.check_revision(Some(base)).unwrap();
            inner.project.properties.tempo = 128.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        assert!(state.lock_inner().check_revision(Some(base)).is_ok());
        let rolled_back = transaction_rollback(&state, "test").unwrap();
        assert_eq!(rolled_back["rolled_back"], true);
        assert_eq!(
            state.lock_inner().project.properties.tempo,
            120.0,
            "rollback restores the pre-batch project"
        );
        assert!(state.lock_inner().check_revision(Some(base)).is_err());

        let begin = transaction_begin(&state, None, "test").unwrap();
        let base = begin["revision"].as_u64().unwrap();
        {
            let mut inner = state.lock_inner();
            inner.check_revision(Some(base)).unwrap();
            inner.project.properties.tempo = 128.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        let committed = transaction_commit(&state, "test").unwrap();
        assert_eq!(committed["committed"], true);
        assert_eq!(committed["label"], "agent transaction");
        assert_eq!(committed["revision"], base + 1);
        assert_eq!(state.lock_inner().project.properties.tempo, 128.0);
        assert_eq!(state.lock_inner().undo.undo.len(), 1);
        assert!(
            transaction_commit(&state, "test")
                .unwrap_err()
                .contains("no active")
        );
    }

    #[test]
    fn a_caller_that_does_not_own_the_batch_cannot_edit_or_close_it() {
        let state = panel_state("@project \"Owner\"\n  tempo: 120\n");
        let base =
            transaction_begin(&state, Some("panel batch".into()), "panel:a").unwrap()["revision"]
                .as_u64()
                .unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project.properties.tempo = 140.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }

        // An external caller's mutation is refused and changes nothing.
        let refused = guard_mutation(
            &state,
            "tracks_mute",
            &json!({ "expected_revision": base }),
            HTTP_BATCH_OWNER,
        )
        .unwrap_err();
        assert!(refused.contains("panel:a"), "{refused}");
        assert!(refused.contains(HTTP_BATCH_OWNER), "{refused}");
        assert_eq!(
            state.lock_inner().project.properties.tempo,
            140.0,
            "the refusal left the owner's edit alone"
        );
        // Nor may the external caller close the owner's batch.
        assert!(
            transaction_rollback(&state, HTTP_BATCH_OWNER)
                .unwrap_err()
                .contains("panel:a")
        );
        assert!(
            transaction_commit(&state, HTTP_BATCH_OWNER)
                .unwrap_err()
                .contains("panel:a")
        );

        // The owner still closes its batch, and its edit is published.
        assert_eq!(
            transaction_commit(&state, "panel:a").unwrap()["committed"],
            true
        );
        assert_eq!(state.lock_inner().project.properties.tempo, 140.0);
        assert!(state.lock_inner().transaction.is_none());
    }

    #[test]
    fn recovery_closes_only_a_panel_batch() {
        let state = panel_state("@project \"Recover\"\n  tempo: 120\n");

        // No batch: recovery is a no-op and opens nothing.
        assert!(
            crate::commands::recover_panel_batch(&state)
                .unwrap()
                .is_none()
        );

        // A batch an external caller opened is that caller's to finish.
        transaction_begin(&state, Some("http batch".into()), HTTP_BATCH_OWNER).unwrap();
        assert!(
            crate::commands::recover_panel_batch(&state)
                .unwrap()
                .is_none()
        );
        assert!(
            state.batch_owner().is_some(),
            "a foreign batch is not ours to close"
        );
        transaction_rollback(&state, HTTP_BATCH_OWNER).unwrap();

        // A panel batch is recovered, and a second recovery is a no-op.
        let base =
            transaction_begin(&state, Some("panel batch".into()), "panel:a").unwrap()["revision"]
                .as_u64()
                .unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project.properties.tempo = 150.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        let recovered = crate::commands::recover_panel_batch(&state)
            .unwrap()
            .expect("the panel batch is recovered");
        assert_eq!(recovered.project.properties.tempo, 120.0);
        assert_eq!(recovered.revision, base + 1);
        assert!(state.lock_inner().transaction.is_none());
        assert!(
            crate::commands::recover_panel_batch(&state)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_external_caller_can_begin_its_own_batch() {
        let state = panel_state("@project \"External\"\n  tempo: 120\n");
        let begin = transaction_begin(&state, Some("http batch".into()), HTTP_BATCH_OWNER).unwrap();
        assert_eq!(begin["owner"], json!(HTTP_BATCH_OWNER));
        assert_eq!(state.batch_owner().as_deref(), Some(HTTP_BATCH_OWNER));
        // A second begin is still refused; the slot holds one batch.
        assert!(
            transaction_begin(&state, Some("again".into()), HTTP_BATCH_OWNER)
                .unwrap_err()
                .contains("already active")
        );
        assert_eq!(
            transaction_commit(&state, HTTP_BATCH_OWNER).unwrap()["committed"],
            true
        );
    }

    #[test]
    fn a_refused_mutation_keeps_the_batch_and_the_edits_before_it() {
        let state = panel_state("@project \"Batch\"\n  tempo: 120\n");
        // Turn 1 commits an edit, so the project moves past the revision it began on.
        let base = transaction_begin(&state, Some("turn 1".into()), "test").unwrap()["revision"]
            .as_u64()
            .unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project.properties.tempo = 100.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        transaction_commit(&state, "test").unwrap();
        let entries_before = state.lock_inner().undo.undo.len();

        // Turn 2 edits, then asks for another write planned against turn 1's revision.
        transaction_begin(&state, Some("turn 2".into()), "test").unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project.properties.tempo = 140.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        let refused = guard_mutation(
            &state,
            "presets_set_params",
            &json!({ "expected_revision": base }),
            "test",
        );
        let error = refused.unwrap_err();
        assert!(error.contains("stale revision"), "{error}");
        let revision = {
            let inner = state.lock_inner();
            assert_eq!(
                inner.project.properties.tempo, 140.0,
                "a refusal must not undo the edits before it"
            );
            assert!(inner.transaction.is_some(), "a refusal must keep the batch");
            inner.revision
        };

        // The model adapts with the revision the project is at, in the same batch.
        guard_mutation(
            &state,
            "project_set_tempo",
            &json!({ "expected_revision": revision }),
            "test",
        )
        .unwrap();
        transaction_commit(&state, "test").unwrap();
        let inner = state.lock_inner();
        assert_eq!(inner.project.properties.tempo, 140.0);
        assert_eq!(
            inner.undo.undo.len(),
            entries_before + 1,
            "the turn is one undo entry"
        );
        assert_eq!(
            inner.revision,
            revision + 1,
            "the turn publishes one revision"
        );
    }

    #[test]
    fn a_turn_of_refusals_leaves_no_undo_entry() {
        let state = panel_state("@project \"Refused\"\n  tempo: 120\n");
        let base = transaction_begin(&state, Some("turn".into()), "test").unwrap()["revision"]
            .as_u64()
            .unwrap();
        guard_mutation(
            &state,
            "project_set_tempo",
            &json!({ "expected_revision": base + 1 }),
            "test",
        )
        .unwrap_err();
        guard_mutation(
            &state,
            "tracks_mix",
            &json!({ "expected_revision": base + 1 }),
            "test",
        )
        .unwrap_err();
        transaction_commit(&state, "test").unwrap();
        let inner = state.lock_inner();
        assert!(inner.transaction.is_none());
        assert!(inner.undo.undo.is_empty(), "nothing landed, so no entry");
        assert_eq!(inner.revision, base, "nothing landed, so no revision");
    }

    #[test]
    fn a_refused_write_does_not_block_the_next_turn() {
        let dir = temp_dir("refused-write");
        let state = panel_state("@project \"Refused\"\n  tempo: 120\n");
        let base = transaction_begin(&state, Some("turn 1".into()), "test").unwrap()["revision"]
            .as_u64()
            .unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project.properties.tempo = 100.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        transaction_commit(&state, "test").unwrap();

        // Turn 2 opens a batch; its first write is planned against turn 1's base.
        transaction_begin(&state, Some("turn 2".into()), "test").unwrap();
        let refused = guard_mutation(
            &state,
            "presets_set_params",
            &json!({ "expected_revision": base }),
            "test",
        );
        let error = refused.unwrap_err();
        assert!(error.contains("stale revision"), "{error}");
        // The turn ends as any turn does: the loop commits the batch it kept.
        transaction_commit(&state, "test").unwrap();

        // Turn 3 begins, edits, commits, and saves: nothing was left blocking it.
        let begin = transaction_begin(&state, Some("turn 3".into()), "test").unwrap();
        let revision = begin["revision"].as_u64().unwrap();
        {
            let mut inner = state.lock_inner();
            inner.check_revision(Some(revision)).unwrap();
            inner.project.properties.tempo = 140.0;
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.bump_revision();
        }
        transaction_commit(&state, "test").unwrap();
        let path = dir.join("final.ocp");
        let saved = crate::commands::save_file_inner(Some(path.display().to_string()), &state)
            .expect("a refused write must not block the next save");
        assert!(std::path::Path::new(&saved).is_file());
        assert_eq!(state.lock_inner().project.properties.tempo, 140.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (frequency Hz, start s, duration s) per note, 48 kHz 16-bit mono.
    fn write_sine_wav(path: &std::path::Path, notes: &[(f64, f64, f64)], seconds: f64) {
        const RATE: u32 = 48_000;
        let frames = (seconds * f64::from(RATE)).ceil() as usize;
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..frames {
            let time = frame as f64 / f64::from(RATE);
            let mut sample = 0.0f64;
            for &(frequency, start, duration) in notes {
                if time >= start && time < start + duration {
                    let local = time - start;
                    let fade = (local / 0.005).min((duration - local) / 0.005).min(1.0);
                    sample += 0.5 * fade * (std::f64::consts::TAU * frequency * local).sin();
                }
            }
            let value = (sample.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16;
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn note_theme(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("theme.wav");
        write_sine_wav(
            &path,
            &[
                (440.0, 0.0, 0.5),
                (523.251_130_5, 0.55, 0.5),
                (659.255_113, 1.10, 0.5),
                (440.0, 1.65, 0.5),
            ],
            2.2,
        );
        path
    }

    #[test]
    fn analyze_notes_transcribes_a_four_note_phrase() {
        let dir = temp_dir("notes-phrase");
        let path = note_theme(&dir);
        let value = analyze_notes_tool(&json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(value["polyphonic"], false, "{value}");
        let reported = value["notes"].as_array().unwrap();
        assert_eq!(reported.len(), 4, "{value}");
        for (index, expected) in [69, 72, 76, 69].iter().enumerate() {
            assert_eq!(reported[index]["midi"], *expected, "{value}");
            let cents = reported[index]["cents"].as_f64().unwrap();
            assert!(cents.abs() <= 20.0, "{value}");
            let onset = reported[index]["start_s"].as_f64().unwrap();
            let target = [0.0, 0.55, 1.10, 1.65][index];
            assert!(
                (onset - target).abs() <= 0.03,
                "onset {onset} is not within 30 ms of {target}: {value}"
            );
        }
        for note in reported {
            let name = note["name"].as_str().unwrap();
            let midi = note["midi"].as_i64().unwrap();
            assert_eq!(oc_engine::note_analysis::note_name(midi as i32), name);
            assert!(note["end_s"].as_f64().unwrap() > note["start_s"].as_f64().unwrap());
        }
        assert_eq!(value["analysis_rate_hz"], 16_000.0);
        assert_eq!(value["tempo_bpm"], 120.0);
        assert_eq!(value["time_signature"], "4/4");
        assert_eq!(value["beats_per_bar"], 4.0);
        assert_eq!(value["beat_seconds"], 0.5);
        assert!(reported[0]["start_beat"].is_number(), "{value}");
        assert!(reported[0]["duration_beats"].is_number(), "{value}");
        assert!(reported[0]["bar"].is_number(), "{value}");
        assert!(reported[0]["suggested_duration"].is_string(), "{value}");
        let warnings = value["warnings"].as_array().unwrap();
        // The theme's onsets are deliberately off the grid (0.55 s is 1.1
        // beats at 120 BPM), so the default-grid warning and the off-grid
        // warning are both expected.
        assert_eq!(warnings.len(), 2, "{value}");
        let assumed = warnings[0].as_str().unwrap();
        assert!(
            assumed.contains("engine default 120 BPM 4/4"),
            "the default grid has to name its assumption: {assumed}"
        );
        assert!(
            warnings[1].as_str().unwrap().contains("off the 1/16 grid"),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_window_reports_times_relative_to_its_start() {
        let dir = temp_dir("notes-window");
        let path = note_theme(&dir);
        let value = analyze_notes_tool(&json!({
            "path": path.to_string_lossy(),
            "start": 0.5,
            "end": 1.5,
        }))
        .unwrap();
        assert_eq!(value["start_s"], 0.5);
        let reported = value["notes"].as_array().unwrap();
        assert_eq!(reported.len(), 2, "{value}");
        assert_eq!(reported[0]["midi"], 72, "{value}");
        assert_eq!(reported[1]["midi"], 76, "{value}");
        let first = reported[0]["start_s"].as_f64().unwrap();
        let second = reported[1]["start_s"].as_f64().unwrap();
        assert!((first - 0.05).abs() <= 0.03, "{value}");
        assert!((second - 0.60).abs() <= 0.03, "{value}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_voices_a_fifth_apart_are_flagged_polyphonic() {
        let dir = temp_dir("notes-chord");
        let path = dir.join("chord.wav");
        write_sine_wav(&path, &[(220.0, 0.0, 1.0), (330.0, 0.0, 1.0)], 1.0);
        let value = analyze_notes_tool(&json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(value["polyphonic"], true, "{value}");
        assert_eq!(value["notes"].as_array().unwrap().len(), 1, "{value}");
        let warnings = value["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|warning| warning
                .as_str()
                .unwrap()
                .contains("only the strongest voice")),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_silent_wav_is_an_empty_report_not_an_error() {
        let dir = temp_dir("notes-silence");
        let path = dir.join("silence.wav");
        write_sine_wav(&path, &[], 1.0);
        let value = analyze_notes_tool(&json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(value["notes"], json!([]));
        assert_eq!(value["polyphonic"], false);
        let warnings = value["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2, "{value}");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("engine default")),
            "{value}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("no notes")),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_refuses_bad_input_readably() {
        let dir = temp_dir("notes-refusals");
        let missing = dir.join("missing.wav");
        let error = analyze_notes_tool(&json!({"path": missing.to_string_lossy()})).unwrap_err();
        assert!(error.contains("could not read"), "{error}");
        let garbage = dir.join("garbage.wav");
        std::fs::write(&garbage, b"not a wav at all").unwrap();
        let error = analyze_notes_tool(&json!({"path": garbage.to_string_lossy()})).unwrap_err();
        assert!(error.contains("could not read"), "{error}");
        let good = dir.join("good.wav");
        write_sine_wav(&good, &[(440.0, 0.0, 0.3)], 0.3);
        let path = good.to_string_lossy().to_string();
        for (args, needle) in [
            (
                json!({"path": path, "start": 0.2, "end": 0.1}),
                "end must be after start",
            ),
            (
                json!({"path": path, "start": -1.0}),
                "start must be at least 0",
            ),
            (
                json!({"path": path, "min_confidence": 1.5}),
                "min_confidence",
            ),
            (json!({"path": path, "max_notes": 0}), "max_notes"),
            (json!({"path": path, "max_notes": 5000}), "max_notes"),
            (json!({}), "path"),
        ] {
            let error = analyze_notes_tool(&args).unwrap_err();
            assert!(error.contains(needle), "{args}: {error}");
        }
        // A window past the file end is an empty report, not an error.
        let value = analyze_notes_tool(&json!({"path": path, "start": 5.0, "end": 6.0})).unwrap();
        assert_eq!(value["notes"], json!([]));
        assert!(!value["warnings"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_truncates_to_max_notes_and_says_so() {
        let dir = temp_dir("notes-cap");
        let path = note_theme(&dir);
        let value = analyze_notes_tool(&json!({
            "path": path.to_string_lossy(),
            "max_notes": 2,
        }))
        .unwrap();
        assert_eq!(value["notes"].as_array().unwrap().len(), 2, "{value}");
        assert_eq!(value["notes"][0]["midi"], 69);
        assert_eq!(value["notes"][1]["midi"], 72);
        assert!(
            value["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("max_notes=2")),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_takes_the_grid_from_its_arguments() {
        let dir = temp_dir("notes-grid-args");
        let path = dir.join("pulse.wav");
        write_sine_wav(&path, &[(220.0, 0.2, 0.5)], 1.0);
        let value = analyze_notes_tool(&json!({
            "path": path.to_string_lossy(),
            "tempo": 90,
            "time_signature": "3/4",
        }))
        .unwrap();
        assert_eq!(value["tempo_bpm"], 90.0, "{value}");
        assert_eq!(value["time_signature"], "3/4", "{value}");
        assert_eq!(value["beats_per_bar"], 3.0, "{value}");
        assert!((value["beat_seconds"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-12);
        let note = &value["notes"][0];
        // 0.2 s at 90 BPM is 0.3 beats; the onset may smear by half a window.
        let start_beat = note["start_beat"].as_f64().unwrap();
        assert!(
            (start_beat - 0.3).abs() <= 0.040 / (2.0 / 3.0),
            "start beat {start_beat} is not 0.3: {value}"
        );
        assert_eq!(note["bar"], 1, "{value}");
        assert_eq!(note["suggested_duration"], "8th.", "{value}");
        assert!(
            value["warnings"].as_array().unwrap().is_empty(),
            "an explicit grid needs no assumption warning: {value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_reads_the_grid_from_a_project_file() {
        let dir = temp_dir("notes-grid-project");
        let path = dir.join("pulse.wav");
        write_sine_wav(&path, &[(220.0, 0.2, 0.5)], 1.0);
        let project = dir.join("grid.ocp");
        std::fs::write(
            &project,
            "@project \"Grid\"\n  tempo: 90\n  time_signature: 3/4\n\n\
             @track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 2:1:0]\n    C4 q mf\n",
        )
        .unwrap();
        let value = analyze_notes_tool(&json!({
            "path": path.to_string_lossy(),
            "project": project.to_string_lossy(),
        }))
        .unwrap();
        assert_eq!(value["tempo_bpm"], 90.0, "{value}");
        assert_eq!(value["time_signature"], "3/4", "{value}");
        let warnings = value["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|warning| {
                let text = warning.as_str().unwrap();
                text.contains("came from the project") && text.contains("grid.ocp")
            }),
            "the warning has to name the project: {value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_reads_the_grid_a_range_render_wrote() {
        let state = panel_state(
            "@project \"Grid render\"\n  tempo: 90\n  time_signature: 3/4\n\
             @preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n\
             @track lead | preset: tone, instrument: synth/lead\n  @phrase riff [1:1:0 - 3:1:0]\n    C4 q mf\n",
        );
        let dir = temp_dir("notes-grid-render");
        let wav = dir.join("range.wav");
        let rendered = commands::render_range(
            wav.to_string_lossy().to_string(),
            commands::RangeRequest {
                start_s: Some(0.0),
                end_s: Some(1.0),
                ..Default::default()
            },
            &state,
        )
        .unwrap();
        assert!(crate::render_grid::read(&wav).is_some());
        let value = analyze_notes_tool(&json!({"path": rendered.path})).unwrap();
        assert_eq!(value["tempo_bpm"], 90.0, "{value}");
        assert_eq!(value["time_signature"], "3/4", "{value}");
        let warnings = value["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|warning| warning
                .as_str()
                .unwrap()
                .contains("render metadata written by render_range")),
            "the warning has to name the render metadata: {value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_refuses_an_impossible_grid_readably() {
        let dir = temp_dir("notes-grid-refusals");
        let path = dir.join("tone.wav");
        write_sine_wav(&path, &[(220.0, 0.0, 0.3)], 0.3);
        let path = path.to_string_lossy().to_string();
        for (args, needle) in [
            (json!({"path": path, "tempo": 0}), "tempo must be inside"),
            (
                json!({"path": path, "time_signature": "4/3"}),
                "denominator",
            ),
            (
                json!({"path": path, "time_signature": "four"}),
                "must look like",
            ),
            (
                json!({"path": path, "project": dir.join("missing.ocp").to_string_lossy()}),
                "could not read project",
            ),
        ] {
            let error = analyze_notes_tool(&args).unwrap_err();
            assert!(error.contains(needle), "{args}: {error}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn analyze_notes_quantize_reports_the_snapped_values() {
        let dir = temp_dir("notes-grid-quantize");
        let path = dir.join("pulse.wav");
        write_sine_wav(&path, &[(220.0, 0.4, 0.5)], 1.0);
        let path = path.to_string_lossy().to_string();
        let raw = analyze_notes_tool(&json!({"path": path, "tempo": 120})).unwrap();
        assert!(raw["notes"][0].get("start_beat_quantised").is_none());
        assert!(raw["notes"][0].get("duration_beats_quantised").is_none());
        let snapped =
            analyze_notes_tool(&json!({"path": path, "tempo": 120, "quantize": true})).unwrap();
        let note = &snapped["notes"][0];
        assert_eq!(note["start_beat_quantised"], 0.75, "{snapped}");
        assert_eq!(note["duration_beats_quantised"], 1.0, "{snapped}");
        let raw_start = note["start_beat"].as_f64().unwrap();
        assert!(
            (raw_start - 0.75).abs() > 0.01,
            "the raw start {raw_start} has to stay the measurement: {snapped}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A 200 Hz tone at 0.4 plus a 3 kHz tone at 0.3, 48 kHz 16-bit.
    fn write_two_tone_wav(path: &std::path::Path, seconds: f64) {
        const RATE: u32 = 48_000;
        let frames = (seconds * f64::from(RATE)) as usize;
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..frames {
            let time = frame as f64 / f64::from(RATE);
            let tau = std::f64::consts::TAU;
            let sample = 0.4 * (tau * 200.0 * time).sin() + 0.3 * (tau * 3_000.0 * time).sin();
            writer
                .write_sample((sample * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    /// Amplitude of one frequency, Hann-windowed so a tone needs no exact bin.
    fn tone_amplitude(samples: &[f64], hz: f64) -> f64 {
        let window: Vec<f64> = (0..samples.len())
            .map(|index| {
                0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / samples.len() as f64).cos()
            })
            .collect();
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (index, (sample, weight)) in samples.iter().zip(&window).enumerate() {
            let angle = std::f64::consts::TAU * hz * index as f64 / 48_000.0;
            real += weight * sample * angle.cos();
            imaginary -= weight * sample * angle.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / window.iter().sum::<f64>()
    }

    fn mono_rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn band_warnings(value: &serde_json::Value) -> Vec<String> {
        value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn separate_stems_band_mode_writes_low_and_high_with_the_tone_split() {
        let dir = temp_dir("stems-band");
        let source = dir.join("mix.wav");
        write_two_tone_wav(&source, 2.0);
        let grid = crate::render_grid::RenderGrid {
            tempo_bpm: 90.0,
            time_signature: "3/4".to_owned(),
            tempo_changes: 0,
            meter_changes: 0,
        };
        crate::render_grid::write(&source, &grid);
        let out = dir.join("stems");
        let value = separate_stems_tool(&json!({
            "path": source.to_string_lossy(),
            "mode": "band",
            "out_dir": out.to_string_lossy(),
        }))
        .unwrap();
        assert_eq!(value["path"], json!(source.to_string_lossy()));
        assert_eq!(value["method"], "band", "{value}");
        let stems = value["stems"].as_array().unwrap();
        assert_eq!(stems.len(), 2, "a mono source has low and high: {value}");
        assert_eq!(stems[0]["name"], "low");
        assert_eq!(stems[1]["name"], "high");
        for stem in value["stems"].as_array().unwrap() {
            let target = std::path::Path::new(stem["path"].as_str().unwrap());
            assert_eq!(
                crate::render_grid::read(target),
                Some(grid.clone()),
                "a stem has to inherit the source render's grid: {stem}"
            );
        }

        let low = oc_engine::analysis::read_wav(stems[0]["path"].as_str().unwrap()).unwrap();
        let high = oc_engine::analysis::read_wav(stems[1]["path"].as_str().unwrap()).unwrap();
        for stem in stems {
            let file = stem["path"].as_str().unwrap();
            let peak = stem["peak_dbfs"].as_f64().unwrap();
            assert!(peak < 0.0 && peak > -40.0, "not a non-silent stem: {stem}");
            let reader = hound::WavReader::open(file).unwrap();
            assert_eq!(reader.spec().bits_per_sample, 16);
            assert_eq!(reader.spec().sample_rate, 48_000);
            assert_eq!(reader.spec().channels, 1);
        }
        let low_200 = tone_amplitude(&low.mono(), 200.0);
        let low_3k = tone_amplitude(&low.mono(), 3_000.0);
        let high_200 = tone_amplitude(&high.mono(), 200.0);
        let high_3k = tone_amplitude(&high.mono(), 3_000.0);
        assert!(low_200 > 0.1, "the low stem is silent: {low_200}");
        assert!(
            low_200 > low_3k * 100.0,
            "low stem: 200 Hz {low_200} vs 3 kHz {low_3k}"
        );
        assert!(high_3k > 0.1, "the high stem is silent: {high_3k}");
        assert!(
            high_3k > high_200 * 1.4,
            "high stem: 3 kHz {high_3k} vs 200 Hz {high_200}"
        );

        // The Linkwitz-Riley pair sums to an allpass, so the stems add back in
        // level (RMS within 0.5 dB) but not sample for sample.
        let input = oc_engine::analysis::read_wav(&source).unwrap();
        let sum: Vec<f64> = low
            .mono()
            .iter()
            .zip(high.mono())
            .map(|(low, high)| low + high)
            .collect();
        let deviation = 20.0 * (mono_rms(&sum) / mono_rms(&input.mono())).log10();
        assert!(
            deviation.abs() < 0.5,
            "the band sum is {deviation} dB from the input"
        );
        assert!(
            band_warnings(&value).iter().any(|warning| {
                warning.contains("approximation") && warning.contains("not a source separation")
            }),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn separate_stems_band_runs_keep_distinct_outputs_in_a_shared_folder() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        std::fs::create_dir(&output).unwrap();
        let existing = output.join("low.wav");
        std::fs::write(&existing, b"keep this file").unwrap();
        let short = dir.path().join("short.wav");
        let long = dir.path().join("long.wav");
        write_two_tone_wav(&short, 0.25);
        write_two_tone_wav(&long, 0.5);
        let split = |path: &std::path::Path| {
            separate_stems_tool(&json!({"path": path, "mode": "band", "out_dir": output})).unwrap()
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| split(&short));
            let second = scope.spawn(|| split(&long));
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_ne!(first["stems"][0]["path"], second["stems"][0]["path"]);
        for (result, frames) in [(first, 12_000), (second, 24_000)] {
            for stem in result["stems"].as_array().unwrap() {
                let wav = hound::WavReader::open(stem["path"].as_str().unwrap()).unwrap();
                assert_eq!(wav.duration(), frames);
            }
        }
        assert_eq!(std::fs::read(existing).unwrap(), b"keep this file");
    }

    #[test]
    fn separate_stems_band_mode_adds_the_mid_for_a_stereo_source() {
        let dir = temp_dir("stems-stereo");
        let source = dir.join("wide.wav");
        let frames = 4_800;
        let mut writer = hound::WavWriter::create(
            &source,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..frames {
            let sample = 0.5
                * (std::f64::consts::TAU * 200.0 * frame as f64 / 48_000.0).sin()
                * f64::from(i16::MAX);
            writer.write_sample(sample as i16).unwrap();
            writer.write_sample(-(sample as i16)).unwrap();
        }
        writer.finalize().unwrap();
        let value = separate_stems_tool(&json!({
            "path": source.to_string_lossy(),
            "mode": "band",
            "out_dir": dir.join("stems").to_string_lossy(),
        }))
        .unwrap();
        let names: Vec<&str> = value["stems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stem| stem["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["low", "high", "centre"], "{value}");
        let centre =
            oc_engine::analysis::read_wav(value["stems"][2]["path"].as_str().unwrap()).unwrap();
        assert!(
            centre.mono().iter().all(|sample| sample.abs() < 1e-6),
            "an anti-phase pair has no mid"
        );
        assert!(
            band_warnings(&value)
                .iter()
                .any(|warning| warning.contains("mono mid")),
            "{value}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn separate_stems_decodes_compressed_source_and_cleans_temporary_input() {
        use std::os::unix::fs::PermissionsExt;
        let source = std::env::var_os("OCP_TEST_SEPARATION_SOURCE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../oc-engine/tests/fixtures/sampler/tone.flac")
            });
        let decoded = oc_engine::audio_clip::decode(&source).unwrap();
        let original = std::fs::read(&source).unwrap();
        let dir = temp_dir("compressed-separator");
        let launcher = dir.join("spleeter");
        std::fs::write(&launcher, "#!/bin/sh\ncase \"$4\" in *.wav) ;; *) exit 5;; esac\n/bin/cp \"$4\" \"$3/vocals.wav\"\n/bin/cp \"$4\" \"$3/accompaniment.wav\"\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = crate::plugin_store::separator_runtime::ResolvedRuntime {
            id: "deezer.spleeter".into(),
            launcher: launcher.clone(),
            version: "2.4.2".into(),
            python_runner: None,
        };
        let out = dir.join("out");
        let result = separate_stems_with_provider(
            &json!({"path":source,"mode":"external","out_dir":out}),
            None,
            Some(&provider),
        )
        .unwrap();
        assert_eq!(result["path"], source.to_string_lossy().as_ref());
        assert_eq!(result["method"], "external:spleeter");
        assert_eq!(result["input_conversion"]["frames"], decoded.frames());
        assert_eq!(
            result["input_conversion"]["sample_rate"],
            decoded.sample_rate
        );
        let wav = hound::WavReader::open(result["stems"][0]["path"].as_str().unwrap()).unwrap();
        assert_eq!(wav.spec().channels, 2);
        assert_eq!(
            wav.into_samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            *decoded.samples
        );
        assert!(
            !std::fs::read_dir(&out)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".decoded-"))
        );
        std::fs::write(&launcher, "#!/bin/sh\nexit 6\n").unwrap();
        assert!(
            separate_stems_with_provider(
                &json!({"path":source,"mode":"external","out_dir":out}),
                None,
                Some(&provider)
            )
            .unwrap_err()
            .contains("exited")
        );
        assert!(
            !std::fs::read_dir(&out)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".decoded-"))
        );
        assert_eq!(std::fs::read(&source).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn separate_stems_managed_runtime_works_without_path_and_reports_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("managed-separator");
        let source = dir.join("mix.wav");
        write_two_tone_wav(&source, 0.3);
        let launcher = dir.join("spleeter");
        std::fs::write(&launcher, "#!/bin/sh\n[ \"$1\" = separate ] && [ \"$2\" = -o ] || exit 4\n/bin/cp \"$4\" \"$3/vocals.wav\"\n/bin/cp \"$4\" \"$3/accompaniment.wav\"\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = crate::plugin_store::separator_runtime::ResolvedRuntime {
            id: "deezer.spleeter".into(),
            launcher,
            version: "2.4.2".into(),
            python_runner: None,
        };
        let mut args =
            json!({"path":source,"runtime_id":"other.separator","out_dir":dir.join("out")});
        assert!(
            separate_stems_with_provider(&args, Some(dir.as_os_str()), Some(&provider))
                .unwrap_err()
                .contains("not ready")
        );
        assert!(!dir.join("out").exists());
        args["runtime_id"] = json!("deezer.spleeter");
        let result = separate_stems_with_provider(&args, None, Some(&provider)).unwrap();
        assert_eq!(result["method"], "external:spleeter");
        assert_eq!(result["runtime_id"], "deezer.spleeter");
        assert_eq!(result["runtime_version"], "2.4.2");
        let stems = result["stems"].as_array().unwrap();
        assert_eq!(stems.len(), 2);
        for (script, refusal) in [
            ("/bin/cp \"$4\" \"$3/vocals.wav\"\nexit 6", "exited"),
            (
                "/bin/cp \"$4\" \"$3/vocals.wav\"",
                "required vocals and accompaniment",
            ),
            (
                "/bin/cp \"$4\" \"$3/vocals.wav\"\nprintf broken > \"$3/accompaniment.wav\"",
                "invalid stem",
            ),
            ("/bin/ln -s \"$4\" \"$3/vocals.wav\"", "no WAV files"),
        ] {
            std::fs::write(&provider.launcher, format!("#!/bin/sh\n{script}\n")).unwrap();
            let error = separate_stems_with_provider(&args, None, Some(&provider)).unwrap_err();
            assert!(error.contains(refusal), "{error}");
            assert_eq!(std::fs::read_dir(dir.join("out")).unwrap().count(), 1);
            assert!(
                stems
                    .iter()
                    .all(|stem| std::path::Path::new(stem["path"].as_str().unwrap()).is_file())
            );
        }
        args["mode"] = json!("band");
        assert!(
            separate_stems_with_provider(&args, None, Some(&provider))
                .unwrap_err()
                .contains("cannot be used with band")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn separate_stems_cancellation_removes_partial_output_without_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mix.wav");
        write_two_tone_wav(&source, 0.3);
        let launcher = dir.path().join("separator");
        std::fs::write(&launcher, "#!/bin/sh\n/bin/cp \"$4\" \"$3/vocals.wav\"\nprintf ready > \"$0.ready\"\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = crate::plugin_store::separator_runtime::ResolvedRuntime {
            id: "test.separator".into(),
            launcher,
            version: "1".into(),
            python_runner: None,
        };
        let output = dir.path().join("out");
        let args = json!({"path": source, "out_dir": output});
        let request = SeparateStemsArgs::decode(&args).unwrap();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                separate_stems_with_provider_cancellable(
                    &request,
                    None,
                    Some(&runtime),
                    Some(&cancel),
                )
            });
            let ready = dir.path().join("separator.ready");
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !ready.is_file() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.store(true, Ordering::Release);
            assert!(ready.is_file(), "separator never started");
            let error = running.join().unwrap().unwrap_err();
            assert_eq!(error, "separation cancelled");
        });
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
    }

    #[test]
    #[ignore = "requires an installed managed separator and OCP_TEST_SEPARATOR_EXCERPT"]
    fn named_managed_separator_runs_on_a_real_compressed_excerpt() {
        use std::path::{Path, PathBuf};

        let root = PathBuf::from(
            std::env::var_os("OCP_TEST_SEPARATOR_ROOT").expect("set the runtime store path"),
        );
        let source = PathBuf::from(
            std::env::var_os("OCP_TEST_SEPARATOR_EXCERPT")
                .expect("set the compressed excerpt path"),
        );
        let id = std::env::var("OCP_TEST_SEPARATOR_ID").expect("set the catalog runtime ID");
        let runtime = crate::plugin_store::separator_runtime::resolve_in(&root, Some(&id))
            .expect("the selected runtime must pass receipt and file verification");
        let output = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let result = separate_stems_with_provider(
            &json!({"path": source, "runtime_id": id, "out_dir": output.path()}),
            None,
            Some(&runtime),
        )
        .unwrap();
        assert_eq!(result["runtime_id"], id);
        assert_eq!(result["runtime_version"], runtime.version);
        assert_eq!(result["method"], "external:spleeter");
        assert_eq!(result["input_conversion"]["format"], "wav");
        assert!(result["warnings"].as_array().unwrap().is_empty());
        let stems = result["stems"].as_array().unwrap();
        assert_eq!(stems.len(), 2);
        let mut lengths = Vec::new();
        for name in ["vocals", "accompaniment"] {
            let stem = stems.iter().find(|stem| stem["name"] == name).unwrap();
            let buffer =
                oc_engine::analysis::read_wav(Path::new(stem["path"].as_str().unwrap())).unwrap();
            assert_eq!(buffer.channel_count(), 2);
            assert_eq!(buffer.sample_rate, 44_100);
            assert!(buffer.duration_s() > 1.0);
            assert!(
                buffer
                    .channels
                    .iter()
                    .flatten()
                    .all(|sample| sample.is_finite())
            );
            let peak = stem["peak_dbfs"].as_f64().unwrap();
            assert!(peak > -60.0, "{name} is too quiet for this fixture");
            lengths.push(buffer.frames());
            eprintln!("{name}: {} frames, {:.2} dBFS peak", buffer.frames(), peak);
        }
        assert_eq!(lengths[0], lengths[1]);
        assert!(
            !std::fs::read_dir(output.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".decoded-"))
        );
        eprintln!("{id} {} finished in {:?}", runtime.version, start.elapsed());
    }

    #[test]
    #[cfg(windows)]
    fn windows_separator_path_finds_console_executables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demucs.exe");
        std::fs::write(&path, b"fixture").unwrap();
        let (name, _, found) = separation_cli(Some(dir.path().as_os_str())).unwrap();
        assert_eq!(name, "demucs");
        assert_eq!(found, path);
    }

    #[test]
    fn separate_stems_external_without_a_provider_is_a_readable_refusal() {
        let dir = temp_dir("stems-refusal");
        let source = dir.join("mix.wav");
        write_two_tone_wav(&source, 0.3);
        let empty = dir.join("empty-path");
        std::fs::create_dir_all(&empty).unwrap();
        let args = json!({ "path": source.to_string_lossy(), "mode": "external" });
        for search in [Some(empty.as_os_str()), None] {
            let error = separate_stems_with(&args, search).unwrap_err();
            assert!(error.starts_with("refused:"), "{error}");
            assert!(
                error.contains("demucs") && error.contains("spleeter"),
                "{error}"
            );
            assert!(error.contains("mode \"band\""), "{error}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn separate_stems_auto_reports_the_path_it_took() {
        let dir = temp_dir("stems-auto");
        let source = dir.join("mix.wav");
        write_two_tone_wav(&source, 0.3);
        let empty = dir.join("empty-path");
        std::fs::create_dir_all(&empty).unwrap();
        // Nothing installed: auto is the band fallback, not an error.
        let value = separate_stems_with(
            &json!({
                "path": source.to_string_lossy(),
                "out_dir": dir.join("auto-band").to_string_lossy(),
            }),
            Some(empty.as_os_str()),
        )
        .unwrap();
        assert_eq!(value["method"], "band", "{value}");
        assert!(
            band_warnings(&value)
                .iter()
                .any(|warning| warning.contains("not a source separation")),
            "{value}"
        );
        let implicit = separate_stems_with(
            &json!({ "path": source.to_string_lossy() }),
            Some(empty.as_os_str()),
        )
        .unwrap();
        assert_eq!(implicit["method"], "band");
        let stem = std::path::Path::new(implicit["stems"][0]["path"].as_str().unwrap());
        std::fs::remove_dir_all(stem.parent().unwrap().parent().unwrap()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let providers = dir.join("providers");
            std::fs::create_dir_all(&providers).unwrap();
            let stub = providers.join("demucs");
            std::fs::write(
                &stub,
                "#!/bin/sh\nmkdir -p \"$2/stub\"\ncp \"$3\" \"$2/stub/bass.wav\"\ncp \"$3\" \"$2/stub/vocals.wav\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            let value = separate_stems_with(
                &json!({
                    "path": source.to_string_lossy(),
                    "out_dir": dir.join("auto-external").to_string_lossy(),
                }),
                Some(providers.as_os_str()),
            )
            .unwrap();
            assert_eq!(value["method"], "external:demucs", "{value}");
            let names: Vec<&str> = value["stems"]
                .as_array()
                .unwrap()
                .iter()
                .map(|stem| stem["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, ["bass", "vocals"], "{value}");
            for stem in value["stems"].as_array().unwrap() {
                assert!(stem["peak_dbfs"].as_f64().unwrap() < 0.0, "{stem}");
                assert!(std::path::Path::new(stem["path"].as_str().unwrap()).is_file());
            }
            assert!(band_warnings(&value).is_empty(), "{value}");

            // A provider that starts and fails falls back in auto, and refuses
            // in external mode.
            std::fs::write(&stub, "#!/bin/sh\necho boom >&2\nexit 2\n").unwrap();
            let value = separate_stems_with(
                &json!({
                    "path": source.to_string_lossy(),
                    "out_dir": dir.join("auto-failed").to_string_lossy(),
                }),
                Some(providers.as_os_str()),
            )
            .unwrap();
            assert_eq!(value["method"], "band", "{value}");
            let warnings = band_warnings(&value);
            assert!(
                warnings.iter().any(|warning| warning.contains("boom")),
                "{warnings:?}"
            );
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("not a source separation")),
                "{warnings:?}"
            );
            let error = separate_stems_with(
                &json!({
                    "path": source.to_string_lossy(),
                    "mode": "external",
                    "out_dir": dir.join("external-failed").to_string_lossy(),
                }),
                Some(providers.as_os_str()),
            )
            .unwrap_err();
            assert!(
                error.contains("demucs") && error.contains("boom"),
                "{error}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn separate_stems_refuses_bad_input_readably() {
        let dir = temp_dir("stems-bad");
        let missing = dir.join("missing.wav");
        let error = separate_stems_tool(&json!({ "path": missing.to_string_lossy() })).unwrap_err();
        assert!(error.contains("could not read"), "{error}");
        let source = dir.join("mix.wav");
        write_two_tone_wav(&source, 0.2);
        for (args, needle) in [
            (
                json!({ "path": source.to_string_lossy(), "mode": "spleeter" }),
                "mode must be",
            ),
            (json!({}), "path"),
        ] {
            let error = separate_stems_tool(&args).unwrap_err();
            assert!(error.contains(needle), "{args}: {error}");
        }
        let garbage = dir.join("garbage.wav");
        std::fs::write(&garbage, b"not a wav at all").unwrap();
        let error = separate_stems_tool(&json!({
            "path": garbage.to_string_lossy(),
            "mode": "band",
            "out_dir": dir.join("garbage-stems").to_string_lossy(),
        }))
        .unwrap_err();
        assert!(error.contains("could not read"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- producer tools -------------------------------------------------

    const PRODUCER_PROJECT: &str = r#"@ocp "0.2.0"

@project "producer fixture"
  tempo: 120
  time_signature: 4/4

@preset lead
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@preset pad
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@track lead | preset: lead, instrument: synth/lead_square
  @phrase riff [1:1:0 - 3:1:0]
    C4 q mf

@track pad | preset: pad, instrument: synth/pad
  @phrase bed [1:1:0 - 3:1:0]
    C3 w mp
"#;

    #[test]
    fn external_project_notifications_publish_only_successful_committed_external_mutations() {
        let state = panel_state("@project \"Timing\"\n  tempo: 120\n");
        let mut inner = state.lock_inner();
        let ok = json!({"revision":inner.revision});
        assert!(external_project_notification(&inner, "tracks_mix", "panel:test", &ok).is_none());
        assert!(
            external_project_notification(&inner, "project_get", HTTP_BATCH_OWNER, &ok).is_none()
        );
        assert!(
            external_project_notification(
                &inner,
                "tracks_mix",
                HTTP_BATCH_OWNER,
                &json!({"ok":false})
            )
            .is_none()
        );
        let edit =
            external_project_notification(&inner, "tracks_mix", HTTP_BATCH_OWNER, &ok).unwrap();
        assert_eq!(edit["snapshot"]["revision"], inner.revision);
        assert_eq!(edit["replaces_document"], false);
        assert_eq!(
            external_project_notification(&inner, "project_open", HTTP_BATCH_OWNER, &ok).unwrap()["replaces_document"],
            true
        );
        inner
            .begin_transaction("batch".into(), HTTP_BATCH_OWNER.into())
            .unwrap();
        assert!(
            external_project_notification(&inner, "tracks_mix", HTTP_BATCH_OWNER, &ok).is_none()
        );
    }

    #[test]
    fn audio_clip_placement_agent_honours_revision_and_transaction_ownership() {
        let state = panel_state(
            r#"@project "Timing"
  tempo: 120
@track drum | instrument: audio/clip
@audio_clip hit [1:2 - 2:2] | track: drum
  file: "source.wav"
"#,
        );
        let revision = state.lock_inner().revision;
        assert!(
            audio_clip_set_placement_request(
                &state,
                &json!({"clip_index":0,"nudge_ms":1,"expected_revision":revision+1}),
                "panel:test"
            )
            .is_err()
        );
        let started = transaction_begin(&state, None, "panel:test").unwrap();
        let args = json!({"clip_index":0,"nudge_ms":1,"expected_revision":started["revision"]});
        assert!(audio_clip_set_placement_request(&state, &args, "other").is_err());
        let result = audio_clip_set_placement_request(&state, &args, "panel:test").unwrap();
        assert_eq!(
            result["clip"]["placement"]["start_offset_seconds"],
            json!(0.001)
        );
        transaction_commit(&state, "panel:test").unwrap();
        let inner = state.lock_inner();
        assert_eq!(inner.project.audio_clip(0).unwrap().offset_seconds, 0.0);
        assert_eq!(inner.undo.undo.len(), 1);
    }

    #[test]
    fn audio_clip_playback_agent_can_edit_inside_its_own_transaction() {
        let dir = temp_dir("clip-playback-agent");
        write_two_tone_wav(&dir.join("source.wav"), 1.0);
        let state = panel_state(
            r#"@project "Warp test"
  tempo: 120

@track drums | instrument: audio/clip

@audio_clip source [1:1 - 2:1] | track: drums
  file: "source.wav"
"#,
        );
        state.lock_inner().file_path = Some(dir.join("test.ocp"));
        let started = transaction_begin(&state, None, "panel:test").unwrap();
        let args = json!({
            "clip_index": 0,
            "expected_revision": started["revision"],
            "playback": {"warp_markers": [{"source_seconds": 0.5, "target_seconds": 0.55}]},
        });
        assert!(audio_clip_set_playback_request(&state, &args, "other").is_err());
        let edited = audio_clip_set_playback_request(&state, &args, "panel:test").unwrap();
        assert_eq!(
            edited["clip"]["warp_markers"][0]["target_seconds"],
            json!(0.55)
        );
        let committed = transaction_commit(&state, "panel:test").unwrap();
        assert_eq!(
            committed["revision"],
            json!(started["revision"].as_u64().unwrap() + 1)
        );
        assert_eq!(
            state
                .lock_inner()
                .project
                .audio_clip(0)
                .unwrap()
                .warp_markers
                .len(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracks_list_counts_audio_clips_separately() {
        let state = panel_state(&format!(
            r#"{PREVIEW_SONG}

@track vocals | instrument: audio/clip

@audio_clip backing [1:1 - 2:1] | track: keys
  file: "backing.wav"

@audio_clip lead [1:1 - 2:1] | track: vocals
  file: "lead.wav"
"#
        ));
        let tracks = tracks_list(&state).unwrap();
        assert_eq!(tracks["tracks"][0]["clips"], json!(1));
        assert_eq!(tracks["tracks"][0]["audio_clips"], json!(1));
        assert_eq!(tracks["tracks"][1]["clips"], json!(0));
        assert_eq!(tracks["tracks"][1]["audio_clips"], json!(1));
    }

    #[test]
    fn a_tempo_map_point_is_written_and_read_back() {
        let state = panel_state(PRODUCER_PROJECT);
        let saved = tempo_map_set_request(
            &state,
            &json!({"points": [
                {"bar": 9, "beat": 1, "bpm": 90.0},
                {"bar": 13, "bpm": 140.0}
            ]}),
        )
        .unwrap();
        assert_eq!(saved["tempo"], json!(120.0));
        assert_eq!(saved["tempo_map"][0]["bar"], json!(9));
        assert_eq!(saved["tempo_map"][0]["bpm"], json!(90.0));
        assert_eq!(saved["tempo_map"][1]["bar"], json!(13));
        assert_eq!(saved["tempo_map"][1]["beat"], json!(1));
        assert_eq!(saved["tempo_map"][1]["bpm"], json!(140.0));
        {
            let inner = state.lock_inner();
            assert_eq!(inner.project.properties.tempo_map.len(), 2);
            assert_eq!(inner.project.properties.tempo_map[0].position.bar, 9);
            assert!(
                inner.dsl_source.contains("tempo_map:"),
                "{}",
                inner.dsl_source
            );
            assert_eq!(inner.undo.undo_labels(), vec!["tempo map"]);
            assert_eq!(inner.revision, saved["revision"].as_u64().unwrap());
        }
        // An empty list returns the song to the project tempo.
        let cleared = tempo_map_set_request(&state, &json!({"points": []})).unwrap();
        assert_eq!(cleared["tempo_map"], json!([]));
        assert!(state.lock_inner().project.properties.tempo_map.is_empty());
    }

    #[test]
    fn an_out_of_order_tempo_map_is_refused_with_the_points_that_were_sent() {
        let state = panel_state(PRODUCER_PROJECT);
        let error = tempo_map_set_request(
            &state,
            &json!({"points": [
                {"bar": 13, "bpm": 140.0},
                {"bar": 9, "bpm": 90.0}
            ]}),
        )
        .unwrap_err();
        assert!(error.contains("Invalid tempo map"), "{error}");
        assert!(error.contains("13:1 = 140 BPM"), "{error}");
        assert!(error.contains("9:1 = 90 BPM"), "{error}");
        let duplicate = tempo_map_set_request(
            &state,
            &json!({"points": [{"bar": 5, "bpm": 100.0}, {"bar": 5, "bpm": 110.0}]}),
        )
        .unwrap_err();
        assert!(duplicate.contains("unique ascending"), "{duplicate}");
        let range = tempo_map_set_request(&state, &json!({"points": [{"bar": 5, "bpm": 700.0}]}))
            .unwrap_err();
        assert!(range.contains("valid tempos"), "{range}");
        let inner = state.lock_inner();
        assert!(inner.project.properties.tempo_map.is_empty());
        assert!(inner.undo.undo.is_empty(), "a refusal adds no history");
    }

    #[test]
    fn a_meter_map_point_is_written_and_read_back() {
        let state = panel_state(PRODUCER_PROJECT);
        let saved = time_signature_set_request(
            &state,
            &json!({"points": [{"bar": 5, "numerator": 3, "denominator": 4}]}),
        )
        .unwrap();
        assert_eq!(saved["time_sig_map"][0]["bar"], json!(5));
        assert_eq!(saved["time_sig_map"][0]["numerator"], json!(3));
        assert_eq!(saved["time_sig_map"][0]["denominator"], json!(4));
        // The base meter is a separate field and stays where it was.
        assert_eq!(saved["time_signature"]["numerator"], json!(4));
        {
            let inner = state.lock_inner();
            assert_eq!(inner.project.properties.time_sig_map.len(), 1);
            assert!(inner.dsl_source.contains("time_sig_map:"));
            assert_eq!(inner.undo.undo_labels(), vec!["time signature map"]);
            assert_eq!(inner.revision, saved["revision"].as_u64().unwrap());
        }
    }

    #[test]
    fn an_out_of_range_meter_is_refused_with_the_points_that_were_sent() {
        for points in [
            json!([{"bar": 5, "numerator": 0, "denominator": 4}]),
            json!([{"bar": 5, "numerator": 4, "denominator": 5}]),
            json!([{"bar": 0, "numerator": 3, "denominator": 4}]),
            json!([
                {"bar": 9, "numerator": 3, "denominator": 4},
                {"bar": 5, "numerator": 4, "denominator": 4}
            ]),
        ] {
            let state = panel_state(PRODUCER_PROJECT);
            let error =
                time_signature_set_request(&state, &json!({ "points": points })).unwrap_err();
            assert!(
                error.contains("Invalid time signature map"),
                "{points}: {error}"
            );
            assert!(error.contains("bar "), "{points}: {error}");
            let inner = state.lock_inner();
            assert!(inner.project.properties.time_sig_map.is_empty());
            assert!(inner.undo.undo.is_empty());
        }
    }

    #[test]
    fn duplicating_a_track_appends_a_named_copy_with_its_own_preset() {
        let state = panel_state(PRODUCER_PROJECT);
        let saved = tracks_duplicate_json(&state, &json!({"index": 0, "name": "lead 2"})).unwrap();
        assert_eq!(saved["index"], json!(2));
        assert_eq!(saved["name"], json!("lead 2"));
        assert_eq!(saved["preset"], json!("lead copy"));
        assert_eq!(saved["track_count"], json!(3));
        let inner = state.lock_inner();
        let copy = &inner.project.tracks[2];
        assert_eq!(copy.name, "lead 2");
        assert_eq!(copy.instrument, inner.project.tracks[0].instrument);
        // The source keeps its patch; the copy owns an independent one.
        assert_eq!(inner.project.tracks[0].preset.as_deref(), Some("lead"));
        assert_eq!(copy.preset.as_deref(), Some("lead copy"));
        assert!(inner.project.presets.iter().any(|p| p.name == "lead copy"));
        // An empty copy, exactly like the GUI's duplicate.
        assert!(copy.pattern_clip_count() == 0 && copy.effects.is_empty());
        assert_eq!(inner.undo.undo_labels(), vec!["duplicate track"]);
        assert_eq!(inner.revision, saved["revision"].as_u64().unwrap());
    }

    #[test]
    fn duplicating_an_unknown_track_or_without_a_name_is_refused() {
        let state = panel_state(PRODUCER_PROJECT);
        assert_eq!(
            tracks_duplicate_json(&state, &json!({"index": 9, "name": "x"})).unwrap_err(),
            "Track 9 out of range"
        );
        assert_eq!(
            tracks_duplicate_json(&state, &json!({"index": 0, "name": "  "})).unwrap_err(),
            "Track name must not be empty"
        );
        assert_eq!(
            tracks_duplicate_json(&state, &json!({"index": 0})).unwrap_err(),
            "missing argument 'name'"
        );
        let inner = state.lock_inner();
        assert_eq!(inner.project.tracks.len(), 2);
        assert!(inner.undo.undo.is_empty());
    }

    #[test]
    fn renaming_a_preset_rewrites_the_tracks_that_used_it() {
        let state = panel_state(PRODUCER_PROJECT);
        let saved =
            presets_rename_json(&state, &json!({"name": "lead", "new_name": "lead warm"})).unwrap();
        assert_eq!(saved["name"], json!("lead warm"));
        assert_eq!(saved["renamed_from"], json!("lead"));
        assert_eq!(saved["tracks"], json!(["lead"]));
        assert_eq!(saved["patterns"], json!([]));
        let inner = state.lock_inner();
        assert!(inner.project.presets.iter().any(|p| p.name == "lead warm"));
        assert!(!inner.project.presets.iter().any(|p| p.name == "lead"));
        assert_eq!(inner.project.tracks[0].preset.as_deref(), Some("lead warm"));
        assert_eq!(inner.undo.undo_labels(), vec!["preset rename"]);
        assert_eq!(inner.revision, saved["revision"].as_u64().unwrap());
    }

    #[test]
    fn a_preset_rename_onto_a_taken_name_is_refused_and_changes_nothing() {
        let state = panel_state(PRODUCER_PROJECT);
        let before = state.lock_inner().project.clone();
        assert_eq!(
            presets_rename_json(&state, &json!({"name": "lead", "new_name": "pad"})).unwrap_err(),
            "Preset 'pad' already exists"
        );
        assert_eq!(
            presets_rename_json(&state, &json!({"name": "gone", "new_name": "fresh"})).unwrap_err(),
            "Preset 'gone' not found"
        );
        assert_eq!(
            presets_rename_json(&state, &json!({"name": "lead", "new_name": "  "})).unwrap_err(),
            "Preset name cannot be empty"
        );
        let inner = state.lock_inner();
        assert_eq!(inner.project, before);
        assert!(inner.undo.undo.is_empty());
    }

    #[test]
    fn duplicating_a_preset_moves_the_references_by_default() {
        let state = panel_state(PRODUCER_PROJECT);
        let saved =
            presets_duplicate_json(&state, &json!({"name": "lead", "new_name": "lead copy"}))
                .unwrap();
        assert_eq!(saved["name"], json!("lead copy"));
        assert_eq!(saved["copied_from"], json!("lead"));
        assert_eq!(saved["reassigned"], json!(true));
        {
            let inner = state.lock_inner();
            assert_eq!(inner.project.tracks[0].preset.as_deref(), Some("lead copy"));
            assert!(inner.project.presets.iter().any(|p| p.name == "lead"));
            assert_eq!(inner.undo.undo_labels(), vec!["preset duplicate"]);
        }
        // reassign=false leaves the references where they were.
        let state = panel_state(PRODUCER_PROJECT);
        let saved = presets_duplicate_json(
            &state,
            &json!({"name": "lead", "new_name": "lead keep", "reassign": false}),
        )
        .unwrap();
        assert_eq!(saved["reassigned"], json!(false));
        assert_eq!(
            state.lock_inner().project.tracks[0].preset.as_deref(),
            Some("lead")
        );
    }

    #[test]
    fn a_preset_copy_onto_a_taken_name_is_refused() {
        let state = panel_state(PRODUCER_PROJECT);
        assert_eq!(
            presets_duplicate_json(&state, &json!({"name": "lead", "new_name": "pad"}))
                .unwrap_err(),
            "Preset 'pad' already exists"
        );
        assert_eq!(
            presets_duplicate_json(&state, &json!({"name": "gone", "new_name": "fresh"}))
                .unwrap_err(),
            "Preset 'gone' not found"
        );
        let inner = state.lock_inner();
        assert_eq!(inner.project.presets.len(), 2);
        assert!(inner.undo.undo.is_empty());
    }

    #[test]
    fn the_undo_history_lists_labels_and_scopes_without_moving_the_cursor() {
        let state = panel_state(PRODUCER_PROJECT);
        tempo_map_set_request(&state, &json!({"points": [{"bar": 5, "bpm": 100.0}]})).unwrap();
        presets_rename_json(&state, &json!({"name": "lead", "new_name": "lead warm"})).unwrap();
        let history = undo_history(&state).unwrap();
        let entries = history["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["index"], json!(1));
        assert_eq!(entries[0]["name"], json!("tempo map"));
        assert_eq!(entries[0]["scopes"], json!(["transport"]));
        assert_eq!(entries[0]["applied"], json!(true));
        assert_eq!(entries[1]["index"], json!(2));
        assert_eq!(entries[1]["name"], json!("preset rename"));
        assert_eq!(entries[1]["scopes"], json!(["preset"]));
        assert_eq!(history["applied"], json!(2));
        assert_eq!(history["redoable"], json!(0));
        // Read-only: nothing was undone and no entry was added.
        assert_eq!(state.lock_inner().undo.undo.len(), 2);
    }

    #[test]
    fn a_stem_export_returns_the_files_it_wrote() {
        let state = panel_state(PRODUCER_PROJECT);
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let rate = state.lock_inner().audio.sample_rate;
        let saved = tauri::async_runtime::block_on(render_stems(
            &state,
            &json!({ "directory": dir.to_string_lossy() }),
        ))
        .unwrap();
        // The export runs at the app's output rate, not a rate of its own.
        assert_eq!(saved["sample_rate"], json!(rate));
        let manifest = saved["manifest"].as_str().unwrap();
        assert!(std::path::Path::new(manifest).is_file(), "{manifest}");
        assert!(std::path::Path::new(&manifest).ends_with("stems.json"));
        let mix = saved["mix"]["path"].as_str().expect("the mix path");
        assert!(std::path::Path::new(mix).is_file(), "{mix}");
        assert_eq!(saved["container"], "wav");
        assert_eq!(saved["sample_format"], "i16");
        assert_eq!(saved["dithered"], false);
        let stems = saved["stems"].as_array().expect("a stem list");
        assert!(stems.len() >= 2, "{stems:?}");
        for entry in std::iter::once(&saved["mix"]).chain(stems) {
            let path = entry["path"].as_str().expect("an audio file path");
            let wav = hound::WavReader::open(path).unwrap();
            assert_eq!(entry["frames"], wav.duration());
            assert_eq!(saved["sample_rate"], wav.spec().sample_rate);
            assert_eq!(saved["channels"], wav.spec().channels);
            assert!(entry["peak_dbfs"].is_number(), "{entry}");
        }
        // No loudness target on this project, so the mix is not normalised.
        assert!(saved["loudness"].is_null(), "{}", saved["loudness"]);
    }

    #[test]
    fn a_stem_export_needs_an_existing_directory() {
        let state = panel_state(PRODUCER_PROJECT);
        let root = temp_dir("stems-missing");
        let dir = root.join("not-created");
        let error = tauri::async_runtime::block_on(render_stems(
            &state,
            &json!({ "directory": dir.to_string_lossy() }),
        ))
        .unwrap_err();
        assert!(error.contains("is not a directory"), "{error}");
        assert!(!dir.exists(), "the refusal must not create the directory");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn beat_grid_analysis_reports_unconfirmed_evidence() {
        use crate::agent_tools::audio_analysis::{AnalyzeArgs, BeatGridWindow};
        let rate = 48_000u32;
        let mut samples = vec![0.0f32; rate as usize * 8];
        for second in 0..8 {
            samples[second as usize * rate as usize] = 0.8;
        }
        let bytes = oc_engine::wav::pcm_wav_bytes(&samples, rate, 1).unwrap();
        let path = oc_test_support::temp_file("oc-analyze", "beat-grid.wav", &bytes);
        let report = audio_analyze_request(AnalyzeArgs {
            path: path.to_string_lossy().into_owned(),
            top_peaks: None,
            mono_fold: None,
            level_window: None,
            transients: None,
            beat_grid: Some(BeatGridWindow {
                start: Some(0.0),
                duration: 8.0,
            }),
        })
        .unwrap();
        assert!(
            report.get("beat_grid").is_some_and(Value::is_object),
            "{report}"
        );
        assert_eq!(report["downbeat_confirmed"], false, "{report}");
        assert!(report["time_signature"].is_null(), "{report}");
        assert!(
            report["guidance"]
                .as_str()
                .is_some_and(|text| text.contains("not probabilities")),
            "{report}"
        );
    }

    #[test]
    fn mono_fold_analysis_reports_band_fields() {
        use crate::agent_tools::audio_analysis::{AnalyzeArgs, MonoFoldWindow};
        let rate = 48_000u32;
        let mut samples = Vec::with_capacity(rate as usize * 2);
        for frame in 0..rate {
            let value = (std::f32::consts::TAU * 440.0 * frame as f32 / rate as f32).sin() * 0.5;
            samples.push(value);
            samples.push(-value);
        }
        let bytes = oc_engine::wav::pcm_wav_bytes(&samples, rate, 2).unwrap();
        let path = oc_test_support::temp_file("oc-analyze", "mono-fold.wav", &bytes);
        let report = audio_analyze_request(AnalyzeArgs {
            path: path.to_string_lossy().into_owned(),
            top_peaks: None,
            mono_fold: Some(MonoFoldWindow {
                start: Some(0.0),
                duration: 1.0,
            }),
            level_window: None,
            transients: None,
            beat_grid: None,
        })
        .unwrap();
        let fold = &report["mono_fold"];
        assert!(
            fold["resolution_hz"].as_f64().is_some_and(|v| v > 0.0),
            "{report}"
        );
        assert!(fold["floor_dbfs"].as_f64().is_some(), "{report}");
        let bands = fold["bands"].as_array().expect("bands array");
        assert!(!bands.is_empty(), "{report}");
        for band in bands {
            assert!(
                band["center_hz"].as_f64().is_some_and(|v| v > 0.0),
                "{report}"
            );
            assert!(band["stereo_dbfs"].as_f64().is_some(), "{report}");
            assert!(band["mono_dbfs"].as_f64().is_some(), "{report}");
            assert!(band["mono_at_floor"].is_boolean(), "{report}");
            assert!(
                band["loss_db"].is_number() || band["loss_db"].is_null(),
                "{report}"
            );
        }
    }

    #[test]
    fn transient_analysis_reports_attack_fields() {
        use crate::agent_tools::audio_analysis::{AnalyzeArgs, TransientWindow};
        let rate = 48_000u32;
        let mut samples = vec![0.0f32; rate as usize * 2];
        for frame in (0..rate as usize).step_by(rate as usize / 4) {
            samples[frame] = 0.8;
        }
        let bytes = oc_engine::wav::pcm_wav_bytes(&samples, rate, 1).unwrap();
        let path = oc_test_support::temp_file("oc-analyze", "transients.wav", &bytes);
        let report = audio_analyze_request(AnalyzeArgs {
            path: path.to_string_lossy().into_owned(),
            top_peaks: None,
            mono_fold: None,
            level_window: None,
            transients: Some(TransientWindow {
                start: Some(0.0),
                duration: 2.0,
                min_gap_ms: Some(5.0),
                limit: Some(8),
            }),
            beat_grid: None,
        })
        .unwrap();
        let transients = &report["transients"];
        assert!(transients["start_s"].as_f64().is_some(), "{report}");
        assert!(transients["end_s"].as_f64().is_some(), "{report}");
        assert!(
            transients["resolution_seconds"]
                .as_f64()
                .is_some_and(|v| v > 0.0),
            "{report}"
        );
        assert!(
            transients["effective_gap_ms"].as_f64().is_some(),
            "{report}"
        );
        let attacks = transients["attacks"].as_array().expect("attacks array");
        assert!(!attacks.is_empty(), "{report}");
        for attack in attacks {
            assert!(attack["file_seconds"].as_f64().is_some(), "{report}");
            assert!(attack["strength"].as_f64().is_some(), "{report}");
        }
    }

    #[test]
    fn listening_comparison_returns_a_matched_pair_without_metrics() {
        use crate::agent_tools::audio_analysis::CompareArgs;
        let rate = 8_000u32;
        let samples: Vec<f32> = (0..rate)
            .map(|frame| (std::f32::consts::TAU * 220.0 * frame as f32 / rate as f32).sin() * 0.5)
            .collect();
        let bytes = oc_engine::wav::pcm_wav_bytes(&samples, rate, 1).unwrap();
        let reference = oc_test_support::temp_file("oc-compare", "reference.wav", &bytes);
        let candidate = oc_test_support::temp_file("oc-compare", "candidate.wav", &bytes);
        let report = audio_compare_request(CompareArgs {
            reference: reference.to_string_lossy().into_owned(),
            candidate: candidate.to_string_lossy().into_owned(),
            reference_start: None,
            candidate_start: None,
            duration: Some(1.0),
            listen: Some(true),
            listening_only: Some(true),
            channel: Some("mono".to_string()),
            clip_sample_rate: Some(16_000),
            stems: Some(false),
            allow_mismatch: Some(false),
        })
        .unwrap();
        assert_eq!(report["listening_only"], true, "{report}");
        assert!(report.get("score").is_none(), "{report}");
        let listening = &report["listening"];
        assert!(
            listening["status"]
                .as_str()
                .is_some_and(|status| matches!(status, "matched" | "unmatched" | "unmeasured")),
            "{report}"
        );
        assert!(
            listening["duration_s"]
                .as_f64()
                .is_some_and(|value| (value - 1.0).abs() < 1e-3),
            "{report}"
        );
        for side in ["reference_audio", "candidate_audio"] {
            let audio = &listening[side];
            assert_eq!(audio["mime"], "audio/wav", "{report}");
            assert_eq!(audio["sample_rate"], 16_000, "{report}");
            assert_eq!(audio["channels"], 1, "{report}");
            assert!(
                audio["base64"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{report}"
            );
        }
    }

    #[test]
    fn note_analysis_reports_beat_grid_fields_in_the_tool_payload() {
        use crate::agent_tools::audio_analysis::NotesArgs;
        let rate = 8_000u32;
        let samples: Vec<f32> = (0..rate * 2)
            .map(|frame| (std::f32::consts::TAU * 220.0 * frame as f32 / rate as f32).sin() * 0.6)
            .collect();
        let bytes = oc_engine::wav::pcm_wav_bytes(&samples, rate, 1).unwrap();
        let path = oc_test_support::temp_file("oc-notes", "beat-grid.wav", &bytes);
        let report = analyze_notes_request(NotesArgs {
            path: path.to_string_lossy().into_owned(),
            start: Some(0.0),
            end: Some(2.0),
            min_confidence: None,
            max_notes: None,
            tempo: Some(120.0),
            time_signature: Some("4/4".to_string()),
            quantize: Some(true),
            project: None,
        })
        .unwrap();
        let notes = report["notes"].as_array().expect("notes array");
        assert!(!notes.is_empty(), "{report}");
        let note = &notes[0];
        assert!(note["start_beat"].as_f64().is_some(), "{report}");
        assert!(note["duration_beats"].as_f64().is_some(), "{report}");
        assert!(note["bar"].as_u64().is_some(), "{report}");
        assert!(
            note["suggested_duration"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            "{report}"
        );
        assert!(note["start_beat_quantised"].as_f64().is_some(), "{report}");
        assert!(
            note["duration_beats_quantised"].as_f64().is_some(),
            "{report}"
        );
    }
}
