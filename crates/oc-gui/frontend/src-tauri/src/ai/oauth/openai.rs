//! ChatGPT token-sharing OAuth (dynamic agent client). Endpoints and scopes
//! follow the provider's v2 flow; the backend is undocumented and may change.
use super::{RefreshError, now_ms};
use crate::ai::auth::TokenSet;
use crate::ai::{http_client, read_limited};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// ChatGPT authorization endpoint.
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
/// ChatGPT token endpoint.
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// Dynamic agent client id.
pub const CLIENT_ID: &str = "";
/// Agent name hint sent at authorization.
pub const AGENT_NAME: &str = "OpenCompose";
/// OAuth scopes for ChatGPT token sharing.
pub const SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// ChatGPT API resource.
pub const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";
const TOKEN_TIMEOUT_SECS: u64 = 30;

#[derive(Debug)]
/// PKCE verifier and S256 challenge.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// URL-safe random token of `bytes` bytes.
pub fn random_token(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buffer)
        .map_err(|_| "Secure random is unavailable".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// Create a fresh PKCE verifier and challenge.
pub fn pkce() -> Result<Pkce, String> {
    let verifier = random_token(64)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// Build the authorization URL with state, nonce and PKCE challenge.
pub fn authorize_url(
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
    agent_host_id: &str,
) -> String {
    let mut url = reqwest::Url::parse(AUTHORIZE_URL).expect("bundled authorize URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("agent_name_hint", AGENT_NAME)
        .append_pair("ext_agent_host_id", agent_host_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE)
        .append_pair("resource", RESOURCE)
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed sign-in callback parameters.
pub struct CallbackParams {
    pub code: String,
    pub state: String,
    pub client_id: Option<String>,
}

/// Parse and bound-check a callback query.
pub fn parse_callback(query: &str) -> Result<CallbackParams, String> {
    let url = reqwest::Url::parse(&format!("http://127.0.0.1/callback?{query}"))
        .map_err(|_| "Invalid sign-in callback".to_string())?;
    let mut code = None;
    let mut state = None;
    let mut client_id = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "client_id" => client_id = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if error.is_some() {
        return Err("Sign-in was cancelled or denied".into());
    }
    let code = code.filter(|code| !code.is_empty());
    let state = state.filter(|state| !state.is_empty());
    match (code, state) {
        (Some(code), Some(state)) if code.len() <= 8192 && state.len() <= 256 => {
            Ok(CallbackParams {
                code,
                state,
                client_id: client_id.filter(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                }),
            })
        }
        _ => Err("The sign-in callback was incomplete".into()),
    }
}

/// One token-endpoint answer, shared with the other Google client on this
/// surface.
#[derive(Default, Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: Option<String>,
    pub(super) refresh_token: Option<String>,
    id_token: Option<String>,
    pub(super) expires_in: Option<i64>,
    pub(super) scope: Option<String>,
    error: Option<String>,
}

fn token_error(status: reqwest::StatusCode, code: &str) -> RefreshError {
    let safe: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let terminal = matches!(
        safe.as_str(),
        "invalid_grant"
            | "invalid_client"
            | "invalid_scope"
            | "unauthorized_client"
            | "refresh_token_expired"
            | "refresh_token_reused"
            | "refresh_token_invalidated"
            | "access_denied"
    );
    if terminal {
        RefreshError::terminal(format!(
            "The sign-in is no longer valid ({safe}). Sign in again."
        ))
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        RefreshError::transient(
            crate::ai::ProviderErrorKind::RateLimit,
            "The token endpoint is rate limited. Retry later.",
        )
    } else if status.is_server_error() {
        RefreshError::transient(
            crate::ai::ProviderErrorKind::Server,
            "The token endpoint is unavailable. Retry later.",
        )
    } else {
        RefreshError::terminal(format!(
            "The token endpoint rejected the request ({safe}). Sign in again."
        ))
    }
}

pub(super) async fn post_form(
    token_url: &str,
    form: &[(&str, String)],
) -> Result<TokenResponse, RefreshError> {
    let client = http_client(TOKEN_TIMEOUT_SECS).map_err(|error| {
        RefreshError::transient(crate::ai::ProviderErrorKind::Config, error.message)
    })?;
    let response = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|_| {
            RefreshError::transient(
                crate::ai::ProviderErrorKind::Network,
                "Could not reach the token endpoint.",
            )
        })?;
    let status = response.status();
    let raw = read_limited(response).await.map_err(|_| {
        RefreshError::transient(
            crate::ai::ProviderErrorKind::Network,
            "Could not read the token endpoint response.",
        )
    })?;
    let body: Option<TokenResponse> = serde_json::from_slice(&raw).ok();
    if let Some(error) = body.as_ref().and_then(|body| body.error.as_deref()) {
        return Err(token_error(status, error));
    }
    if !status.is_success() {
        return Err(match status.as_u16() {
            429 => RefreshError::transient(
                crate::ai::ProviderErrorKind::RateLimit,
                "The token endpoint is rate limited. Retry later.",
            ),
            code if (500..=599).contains(&code) => RefreshError::transient(
                crate::ai::ProviderErrorKind::Server,
                "The token endpoint is unavailable. Retry later.",
            ),
            code => RefreshError::terminal(format!(
                "The token endpoint rejected the request (HTTP {code}). Sign in again."
            )),
        });
    }
    body.ok_or_else(|| {
        RefreshError::terminal("The token endpoint returned an unreadable response.")
    })
}

pub(super) fn expires_at(response: &TokenResponse) -> i64 {
    response
        .expires_in
        .unwrap_or(3600)
        .clamp(60, 86_400)
        .saturating_mul(1000)
        .saturating_add(now_ms())
}

/// Exchanges an authorization code. `nonce` binds the ID token to this attempt;
/// only claims are checked, signature verification needs the JWKS endpoint.
pub async fn exchange_code(
    token_url: &str,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
    client_id: &str,
    nonce: &str,
) -> Result<TokenSet, RefreshError> {
    let form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("client_id", client_id.to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_verifier", verifier.to_string()),
        ("resource", RESOURCE.to_string()),
    ];
    let body = post_form(token_url, &form).await?;
    let access = body
        .access_token
        .clone()
        .ok_or_else(|| RefreshError::terminal("The token endpoint returned no access token."))?;
    let refresh = body
        .refresh_token
        .clone()
        .ok_or_else(|| RefreshError::terminal("The token endpoint returned no refresh token."))?;
    let account_id = match body.id_token.as_deref() {
        Some(id_token) => validate_id_token(id_token, client_id, Some(nonce), now_ms())?,
        None => None,
    };
    Ok(TokenSet {
        v: 1,
        access: access.into(),
        refresh: refresh.into(),
        expires_at: expires_at(&body),
        account_id,
        account_label: None,
        scope: body.scope.clone(),
        client_id: Some(client_id.to_string()),
    })
}

/// Refresh ChatGPT tokens at the default endpoint.
pub async fn refresh(tokens: TokenSet) -> Result<TokenSet, RefreshError> {
    if CLIENT_ID.is_empty() {
        return Err(RefreshError::terminal(
            "OAuth sign-in is not available in this build.",
        ));
    }
    refresh_at(TOKEN_URL, tokens).await
}

/// Refresh against an explicit token endpoint.
pub async fn refresh_at(token_url: &str, tokens: TokenSet) -> Result<TokenSet, RefreshError> {
    let client_id = tokens.client_id.clone().unwrap_or_else(|| CLIENT_ID.into());
    let form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("client_id", client_id.clone()),
        ("refresh_token", tokens.refresh.as_str().to_owned()),
        ("resource", RESOURCE.to_string()),
    ];
    let body = post_form(token_url, &form).await?;
    let access = body
        .access_token
        .clone()
        .ok_or_else(|| RefreshError::terminal("The token endpoint returned no access token."))?;
    let account_id = match body.id_token.as_deref() {
        Some(id_token) => {
            validate_id_token(id_token, &client_id, None, now_ms())?.or(tokens.account_id.clone())
        }
        None => tokens.account_id.clone(),
    };
    Ok(TokenSet {
        v: 1,
        access: access.into(),
        refresh: body
            .refresh_token
            .clone()
            .unwrap_or_else(|| tokens.refresh.as_str().to_owned())
            .into(),
        expires_at: expires_at(&body),
        account_id,
        account_label: tokens.account_label.clone(),
        scope: body.scope.clone().or(tokens.scope),
        client_id: Some(client_id),
    })
}

fn claim_account_id(claims: &Value) -> Option<String> {
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .or_else(|| claims["chatgpt_account_id"].as_str())
        .or_else(|| claims["organizations"][0]["id"].as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn audience_matches(audience: &Value, client_id: &str) -> bool {
    match audience {
        Value::String(value) => value == client_id,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(client_id)),
        _ => false,
    }
}

fn validate_id_token(
    id_token: &str,
    client_id: &str,
    nonce: Option<&str>,
    now: i64,
) -> Result<Option<String>, RefreshError> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| RefreshError::terminal("The ID token is malformed."))?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| RefreshError::terminal("The ID token is malformed."))?;
    let claims: Value = serde_json::from_slice(&decoded)
        .map_err(|_| RefreshError::terminal("The ID token is malformed."))?;
    if claims["iss"].as_str() != Some(ISSUER) {
        return Err(RefreshError::terminal(
            "The ID token issuer does not match.",
        ));
    }
    if !audience_matches(&claims["aud"], client_id) {
        return Err(RefreshError::terminal(
            "The ID token audience does not match.",
        ));
    }
    let expires = claims["exp"].as_i64().unwrap_or(0);
    if expires.saturating_mul(1000) <= now {
        return Err(RefreshError::terminal("The ID token has expired."));
    }
    if let Some(expected) = nonce
        && claims["nonce"].as_str() != Some(expected)
    {
        return Err(RefreshError::terminal("The ID token nonce does not match."));
    }
    if claims["sub"].as_str().is_none_or(str::is_empty) {
        return Err(RefreshError::terminal("The ID token has no subject."));
    }
    Ok(claim_account_id(&claims))
}
