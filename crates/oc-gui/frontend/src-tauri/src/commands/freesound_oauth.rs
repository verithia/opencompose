//! Freesound OAuth2: authorization code with a loopback redirect.
//!
//! Freesound serves a sound's original file only to an OAuth2 client, so a pull
//! has to carry a bearer token. This module runs the authorization-code flow
//! against `127.0.0.1`, stores the token set beside the sample-market API key,
//! and refreshes it once when a download is refused.
//!
//! Tokens never reach a log, a returned payload or an error message. A status is
//! derived from what the keyring holds, and the access token is only ever put
//! into an `Authorization` header.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ai::credential_store::{self, KeyringSecrets};
use crate::samples::KEY_SERVICE;

/// Freesound authorization endpoint.
pub const AUTHORIZE_URL: &str = "https://freesound.org/apiv2/oauth2/authorize/";
/// Freesound token endpoint.
pub const TOKEN_URL: &str = "https://freesound.org/apiv2/oauth2/access_token/";
/// The path of the loopback redirect registered on the Freesound app.
pub const CALLBACK_PATH: &str = "/callback";
/// The token set, as JSON.
pub const TOKEN_ACCOUNT: &str = "freesound-token";
/// Keyring account for the Freesound client id.
pub const CLIENT_ID_ACCOUNT: &str = "freesound-client-id";
/// Keyring account for the Freesound client secret.
pub const CLIENT_SECRET_ACCOUNT: &str = "freesound-client-secret";
/// Headless overrides, so a session without a keychain can still connect.
pub const TOKEN_ENV: &str = "OPENCOMPOSE_FREESOUND_TOKEN";
/// Headless env override for the Freesound client id.
pub const CLIENT_ID_ENV: &str = "OPENCOMPOSE_FREESOUND_CLIENT_ID";
/// Headless env override for the Freesound client secret.
pub const CLIENT_SECRET_ENV: &str = "OPENCOMPOSE_FREESOUND_CLIENT_SECRET";

/// The browser has this long to come back with the code.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = 8192;

#[derive(Clone, Serialize, Deserialize)]
struct TokenSet {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Epoch milliseconds; absent when the endpoint stated no lifetime.
    #[serde(default)]
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct TokenError {
    error: Option<String>,
    error_description: Option<String>,
}

/// What the preferences panel renders. It carries no credential.
#[derive(Debug, Clone, Serialize)]
pub struct FreesoundStatus {
    /// `connected`, `expired` or `needs_reauth`.
    pub state: String,
    /// Epoch milliseconds of the access token, when the endpoint stated one.
    pub expires_at: Option<i64>,
    /// True when the stored token can refresh itself without another sign-in.
    pub has_refresh_token: bool,
    /// True when a client id and secret are stored or set in the environment.
    pub client_configured: bool,
}

fn store() -> KeyringSecrets {
    KeyringSecrets::new(KEY_SERVICE)
}

fn read_secret(account: &str) -> Option<String> {
    match credential_store::load_text(&store(), account) {
        Ok(secret) => secret.map(|secret| secret.as_str().to_string()),
        Err(error) => {
            log::warn!("[samples] the {account} credential is unreadable: {error:?}");
            None
        }
    }
}

fn write_secret(account: &str, value: &str) -> Result<(), String> {
    credential_store::store_text(&store(), account, value)
        .map_err(|error| format!("the Freesound credential could not be saved: {error:?}"))
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The stored token set. An older build wrote the bare access token, so a
/// value without JSON braces is read as one.
fn read_token_set() -> Option<TokenSet> {
    let text = read_secret(TOKEN_ACCOUNT)?;
    let text = text.trim();
    if text.starts_with('{') {
        return serde_json::from_str::<TokenSet>(text)
            .ok()
            .filter(|set| !set.access_token.is_empty());
    }
    (!text.is_empty()).then(|| TokenSet {
        access_token: text.to_string(),
        refresh_token: None,
        expires_at: None,
    })
}

/// The access token a pull sends, from the environment first so a headless
/// session works.
pub(crate) fn access_token() -> Option<String> {
    env_value(TOKEN_ENV).or_else(|| read_token_set().map(|set| set.access_token))
}

/// The stored client credentials, which a refresh needs.
fn client_credentials() -> Option<(String, String)> {
    let id = env_value(CLIENT_ID_ENV).or_else(|| read_secret(CLIENT_ID_ACCOUNT))?;
    let secret = env_value(CLIENT_SECRET_ENV).or_else(|| read_secret(CLIENT_SECRET_ACCOUNT))?;
    (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

/// The connection state without the credential itself.
pub fn status() -> FreesoundStatus {
    let client_configured = client_credentials().is_some();
    if env_value(TOKEN_ENV).is_some() {
        return FreesoundStatus {
            state: "connected".into(),
            expires_at: None,
            has_refresh_token: false,
            client_configured,
        };
    }
    let Some(set) = read_token_set() else {
        return FreesoundStatus {
            state: "needs_reauth".into(),
            expires_at: None,
            has_refresh_token: false,
            client_configured,
        };
    };
    let expired = set
        .expires_at
        .is_some_and(|expires_at| expires_at <= crate::ai::oauth::now_ms());
    FreesoundStatus {
        state: if expired { "expired" } else { "connected" }.into(),
        expires_at: set.expires_at,
        has_refresh_token: set.refresh_token.is_some() && client_configured,
        client_configured,
    }
}

/// Run the authorization-code flow. Empty arguments reuse the stored client
/// credentials, so a later sign-in needs only the button.
pub fn begin(client_id: &str, client_secret: &str) -> Result<FreesoundStatus, String> {
    let stored = client_credentials();
    let client_id = non_empty(client_id)
        .or_else(|| stored.as_ref().map(|(id, _)| id.clone()))
        .ok_or_else(|| "Enter the Freesound client id and secret first.".to_string())?;
    let client_secret = non_empty(client_secret)
        .or_else(|| stored.map(|(_, secret)| secret))
        .ok_or_else(|| "Enter the Freesound client id and secret first.".to_string())?;

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|_| "Could not start the Freesound sign-in listener.".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "Could not read the Freesound callback address.".to_string())?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let url = authorize_url(&client_id, &redirect_uri);
    if !crate::ai::oauth::open_browser(&url) {
        log::warn!("[samples] could not open the browser for the Freesound sign-in");
    }
    let code = accept_callback(&listener, CALLBACK_PATH, CALLBACK_TIMEOUT)?;
    let tokens = exchange(&[
        ("grant_type", "authorization_code".to_string()),
        ("client_id", client_id.clone()),
        ("client_secret", client_secret.clone()),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ])?;
    // The client credentials are stored first: a refresh without them cannot
    // happen, and a token alone would look connected while being unfixable.
    write_secret(CLIENT_ID_ACCOUNT, &client_id)?;
    write_secret(CLIENT_SECRET_ACCOUNT, &client_secret)?;
    write_token_set(&tokens)?;
    Ok(status())
}

/// Remove the token set and the client credentials.
pub fn sign_out() -> Result<(), String> {
    for account in [TOKEN_ACCOUNT, CLIENT_ID_ACCOUNT, CLIENT_SECRET_ACCOUNT] {
        credential_store::delete_entry(&store(), account)
            .map_err(|error| format!("the Freesound credential could not be removed: {error:?}"))?;
    }
    Ok(())
}

/// Refresh once and return the new access token. The caller retries the
/// request that was refused.
pub(crate) fn refresh_access_token() -> Result<String, String> {
    let (client_id, client_secret) = client_credentials()
        .ok_or_else(|| "Freesound needs its client id and secret to refresh.".to_string())?;
    let set = read_token_set().ok_or_else(|| "Freesound is not connected.".to_string())?;
    let refresh = set
        .refresh_token
        .ok_or_else(|| "Freesound did not return a refresh token; connect again.".to_string())?;
    let fresh = exchange(&[
        ("grant_type", "refresh_token".to_string()),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh.clone()),
    ])?;
    let merged = TokenSet {
        access_token: fresh.access_token,
        // Some endpoints rotate the refresh token; keep the old one otherwise.
        refresh_token: fresh.refresh_token.or(Some(refresh)),
        expires_at: fresh.expires_at,
    };
    write_token_set(&merged)?;
    Ok(merged.access_token)
}

fn write_token_set(tokens: &TokenSet) -> Result<(), String> {
    let body = serde_json::to_string(tokens)
        .map_err(|error| format!("the Freesound token could not be encoded: {error}"))?;
    write_secret(TOKEN_ACCOUNT, &body)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The authorize URL with the loopback redirect as the only registered one.
pub fn authorize_url(client_id: &str, redirect_uri: &str) -> String {
    let mut url = reqwest::Url::parse(AUTHORIZE_URL).expect("bundled Freesound authorize URL");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri);
    url.to_string()
}

/// The `code` in a loopback redirect query. An `error` parameter refuses the
/// attempt with the reason Freesound gave.
pub fn callback_code(query: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{CALLBACK_PATH}?{query}"))
        .map_err(|_| "The Freesound callback was unreadable.".to_string())?;
    let mut code = None;
    let mut error = None;
    let mut description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        let error = sanitize(&error);
        let detail = description.map(|value| sanitize(&value));
        return Err(match detail.filter(|value| !value.is_empty()) {
            Some(detail) => format!("Freesound refused the sign-in ({error}): {detail}"),
            None => format!("Freesound refused the sign-in ({error})."),
        });
    }
    match code.filter(|code| !code.is_empty() && code.len() <= 8192) {
        Some(code) => Ok(code),
        None => Err("The Freesound callback carried no authorization code.".into()),
    }
}

/// The request target of an HTTP request, when it is a `GET`.
fn request_target(request: &str) -> Option<&str> {
    let mut parts = request.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then(|| parts.next()).flatten()
}

fn split_target(target: &str) -> (&str, &str) {
    target.split_once('?').unwrap_or((target, ""))
}

/// Serve the loopback redirect until it carries a code or the window expires.
fn accept_callback(
    listener: &TcpListener,
    path: &str,
    timeout: Duration,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|_| "Could not listen for the Freesound callback.".to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err("The Freesound sign-in window expired. Start again.".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(code) = read_callback(&mut stream, path)? {
                    return Ok(code);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => continue,
        }
    }
}

fn read_callback(stream: &mut TcpStream, path: &str) -> Result<Option<String>, String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = vec![0u8; MAX_REQUEST_BYTES];
    let count = match stream.read(&mut buffer) {
        Ok(count) if count > 0 => count,
        _ => {
            respond(stream, "400 Bad Request", "<p>Bad request.</p>");
            return Ok(None);
        }
    };
    let request = String::from_utf8_lossy(&buffer[..count]);
    let Some(target) = request_target(&request) else {
        respond(stream, "400 Bad Request", "<p>Bad request.</p>");
        return Ok(None);
    };
    let (request_path, query) = split_target(target);
    if request_path != path {
        respond(stream, "404 Not Found", "<p>Not found.</p>");
        return Ok(None);
    }
    match callback_code(query) {
        Ok(code) => {
            respond(
                stream,
                "200 OK",
                "<!doctype html><meta charset=\"utf-8\"><title>OpenCompose</title>\
                 <p>Freesound is connected. You can close this window.</p>",
            );
            Ok(Some(code))
        }
        Err(message) => {
            respond(
                stream,
                "400 Bad Request",
                "<!doctype html><meta charset=\"utf-8\"><title>OpenCompose</title>\
                 <p>The sign-in did not complete. Return to OpenCompose.</p>",
            );
            Err(message)
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// The token endpoint, through the proxy the operator selected.
fn token_client() -> Result<reqwest::blocking::Client, String> {
    let builder = reqwest::blocking::Client::builder()
        .user_agent(concat!(
            "OpenCompose/",
            env!("CARGO_PKG_VERSION"),
            " (Freesound OAuth)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT);
    crate::net_proxy::apply_blocking(builder)
        .build()
        .map_err(|error| format!("Could not create the Freesound token client: {error}"))
}

fn exchange(form: &[(&str, String)]) -> Result<TokenSet, String> {
    let client = token_client()?;
    let response = client
        .post(TOKEN_URL)
        .form(form)
        .send()
        .map_err(|error| format!("The Freesound token request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|_| "The Freesound token response could not be read.".to_string())?;
    if !status.is_success() {
        return Err(token_refusal(status, &body));
    }
    let response: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| "The Freesound token response was unreadable.".to_string())?;
    let access_token = response
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "The Freesound token response carried no access token.".to_string())?;
    let expires_at = response
        .expires_in
        .map(|seconds| crate::ai::oauth::now_ms() + seconds.clamp(60, 86_400) * 1000);
    Ok(TokenSet {
        access_token,
        refresh_token: response.refresh_token.filter(|token| !token.is_empty()),
        expires_at,
    })
}

/// A refusal from the token endpoint, without echoing anything from the
/// request body.
fn token_refusal(status: reqwest::StatusCode, body: &str) -> String {
    let parsed: Option<TokenError> = serde_json::from_str(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|error| error.error.as_deref())
        .map(sanitize)
        .filter(|value| !value.is_empty());
    let description = parsed
        .as_ref()
        .and_then(|error| error.error_description.as_deref())
        .map(sanitize)
        .filter(|value| !value.is_empty());
    match (code, description) {
        (Some(code), Some(description)) => {
            format!("Freesound refused the sign-in ({code}): {description}")
        }
        (Some(code), None) => format!("Freesound refused the sign-in ({code})."),
        (None, Some(description)) => format!("Freesound refused the sign-in: {description}"),
        (None, None) => format!("Freesound refused the sign-in (HTTP {}).", status.as_u16()),
    }
}

/// Keep a server-supplied reason printable and short.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_graphic() || *character == ' ')
        .take(160)
        .collect()
}

#[tauri::command]
/// Start the Freesound loopback sign-in.
pub async fn freesound_oauth_begin(
    client_id: String,
    client_secret: String,
) -> Result<FreesoundStatus, String> {
    crate::commands::shared::run_blocking(move || begin(&client_id, &client_secret)).await
}

#[tauri::command]
/// Read the Freesound connection status.
pub async fn freesound_oauth_status() -> Result<FreesoundStatus, String> {
    crate::commands::shared::run_blocking(|| Ok(status())).await
}

#[tauri::command]
/// Clear the stored Freesound token.
pub async fn freesound_oauth_sign_out() -> Result<FreesoundStatus, String> {
    crate::commands::shared::run_blocking(|| {
        sign_out()?;
        Ok(status())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_authorize_url_names_the_client_and_the_loopback_redirect() {
        let redirect = "http://127.0.0.1:43210/callback";
        let url = authorize_url("abc123", redirect);
        let parsed = reqwest::Url::parse(&url).unwrap();
        let pairs: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.as_str().split('?').next(), Some(AUTHORIZE_URL));
        assert_eq!(pairs["client_id"], "abc123");
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["redirect_uri"], redirect);
    }

    #[test]
    fn a_callback_query_yields_the_code_or_the_refusal() {
        assert_eq!(callback_code("code=abc123").unwrap(), "abc123");
        assert_eq!(callback_code("state=x&code=a%2Fb").unwrap(), "a/b");

        let error =
            callback_code("error=access_denied&error_description=User%20denied").unwrap_err();
        assert!(error.contains("access_denied"), "{error}");
        assert!(error.contains("User denied"), "{error}");

        assert!(callback_code("").is_err());
        assert!(callback_code("state=only").is_err());
        assert!(callback_code(&format!("code={}", "x".repeat(9000))).is_err());
    }

    #[test]
    fn the_request_line_names_the_callback_path_and_its_query() {
        let request = "GET /callback?code=abc HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let target = request_target(request).unwrap();
        assert_eq!(split_target(target), (CALLBACK_PATH, "code=abc"));

        let request = "GET /other?code=abc HTTP/1.1\r\n\r\n";
        assert_eq!(split_target(request_target(request).unwrap()).0, "/other");

        assert!(request_target("POST /callback HTTP/1.1\r\n\r\n").is_none());
        assert!(request_target("").is_none());
    }

    #[test]
    fn a_token_refusal_keeps_the_reason_and_drops_the_rest() {
        let message = token_refusal(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid_client","error_description":"Unknown client"}"#,
        );
        assert!(message.contains("invalid_client"), "{message}");
        assert!(message.contains("Unknown client"), "{message}");

        let message = token_refusal(reqwest::StatusCode::BAD_REQUEST, "not json");
        assert!(message.contains("HTTP 400"), "{message}");
    }
}
