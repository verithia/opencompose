//! Auth plans turn a credential into request headers without the wire adapters
//! knowing whether it is an API key or a token; `Debug` redacts both.
use super::credential_store::{self, KeyringSecrets, StoreError};
use super::secret::Secret;
use super::{ProviderError, ProviderErrorKind};
use crate::commands::{AiProviderEntry, AuthKind, AuthProvider, Wire};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Keyring service for API keys. The account string is stable and must never
/// change, or every saved key is orphaned.
pub const API_KEY_SERVICE: &str = "org.opencompose.ai";
/// Shown when the platform has no usable credential store at all.
pub const STORE_UNAVAILABLE: &str = "OS credential store unavailable";
/// Shown when the store exists but cannot be opened without the user.
pub const STORE_LOCKED: &str = "OS credential store is locked; unlock it and retry";
/// Shown when a read was refused: a dismissed prompt, or a build the store does
/// not trust yet. macOS treats every rebuild as a new application, so the
/// operator has to allow it once per build.
pub const STORE_REFUSED: &str = "The OS credential store did not release the saved key: the permission prompt was dismissed, or this build is not allowed yet (macOS asks once per build). Save the key again in AI settings, or set OPENCOMPOSE_AI_KEY for a headless session.";
/// Separate service for OAuth token sets; API-key accounts are never touched.
pub const OAUTH_SERVICE: &str = "org.opencompose.ai.oauth";
/// Refresh when the access token expires within this window.
pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Persisted OAuth token set with refresh metadata.
pub struct TokenSet {
    pub v: u8,
    pub access: Secret,
    pub refresh: Secret,
    /// Epoch milliseconds; `0` means the provider issues non-expiring tokens.
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Non-secret display label, for example the account email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field("v", &self.v)
            .field("access", &"<redacted>")
            .field("refresh", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("account_label", &self.account_label)
            .field("scope", &self.scope)
            .field("client_id", &self.client_id)
            .finish()
    }
}

impl TokenSet {
    /// Whether the access token expires within the refresh skew.
    pub fn expires_within(&self, now_ms: i64) -> bool {
        self.expires_at != 0 && self.expires_at <= now_ms + REFRESH_SKEW_MS
    }

    /// Whether the access token has expired.
    pub fn expired(&self, now_ms: i64) -> bool {
        self.expires_at != 0 && self.expires_at <= now_ms
    }
}

#[derive(Clone)]
/// API key or OAuth credential for one provider.
pub enum Credential {
    /// `None` is valid only for protocols whose endpoint may be unauthenticated
    /// (local OpenAI-compatible servers); `plan` rejects it elsewhere.
    ApiKey(Option<Secret>),
    OAuth(TokenSet),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(Some(_)) => formatter.write_str("ApiKey(<redacted>)"),
            Self::ApiKey(None) => formatter.write_str("ApiKey(none)"),
            Self::OAuth(tokens) => formatter.debug_tuple("OAuth").field(tokens).finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
/// Request body encoding.
pub enum BodyEncoding {
    #[default]
    Plain,
    Zstd,
}

/// Headers to add, headers to drop and the body encoding for one request.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AuthPlan {
    header: Option<(String, Secret)>,
    extra: Vec<(String, Secret)>,
    remove: Vec<String>,
    body: BodyEncoding,
}

impl std::fmt::Debug for AuthPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = |pairs: &[(String, Secret)]| {
            pairs
                .iter()
                .map(|(name, _)| (name.clone(), "<redacted>"))
                .collect::<Vec<_>>()
        };
        formatter
            .debug_struct("AuthPlan")
            .field(
                "header",
                &self
                    .header
                    .as_ref()
                    .map(|(name, _)| (name.clone(), "<redacted>")),
            )
            .field("extra", &names(&self.extra))
            .field("remove", &self.remove)
            .field("body", &self.body)
            .finish()
    }
}

impl AuthPlan {
    /// Plan a bearer Authorization header.
    pub fn bearer(token: impl Into<Secret>) -> Self {
        Self::scheme("authorization", "Bearer", token)
    }

    /// Plan one header.
    pub fn header(name: impl Into<String>, value: impl Into<Secret>) -> Self {
        Self {
            header: Some((name.into(), value.into())),
            ..Self::default()
        }
    }

    /// Plan an Authorization-style header with a scheme.
    pub fn scheme(name: impl Into<String>, scheme: &str, token: impl Into<Secret>) -> Self {
        let token = token.into();
        Self::header(name, Secret::new(format!("{scheme} {}", token.as_str())))
    }

    /// Add an extra header.
    pub fn extra(mut self, name: impl Into<String>, value: impl Into<Secret>) -> Self {
        self.extra.push((name.into(), value.into()));
        self
    }

    #[allow(dead_code)]
    /// Remove a header name.
    pub fn remove(mut self, name: impl Into<String>) -> Self {
        self.remove.push(name.into());
        self
    }
    /// Set the body encoding.
    pub fn body(mut self, body: BodyEncoding) -> Self {
        self.body = body;
        self
    }

    /// Apply `other` after `self`; a conflicting header name is overridden
    /// because extras are applied after the primary header.
    pub fn and_then(self, other: Self) -> Self {
        let mut merged = self;
        if let Some(header) = other.header {
            if merged.header.is_some() {
                merged.extra.push(header);
            } else {
                merged.header = Some(header);
            }
        }
        merged.extra.extend(other.extra);
        merged.remove.extend(other.remove);
        if other.body != BodyEncoding::Plain {
            merged.body = other.body;
        }
        merged
    }

    /// Keep `self` when it sets a header, otherwise use `other`.
    #[allow(dead_code)]
    pub fn or_else(self, other: Self) -> Self {
        if self.header.is_some() {
            return self;
        }
        let mut merged = other;
        merged.extra.extend(self.extra);
        merged.remove.extend(self.remove);
        merged
    }

    /// Configured body encoding.
    pub fn body_encoding(&self) -> BodyEncoding {
        self.body
    }

    #[allow(dead_code)]
    /// Planned value for a header, if present.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.header
            .as_ref()
            .filter(|(key, _)| key.eq_ignore_ascii_case(&name))
            .map(|(_, value)| value.as_str())
            .or_else(|| {
                self.extra
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                    .map(|(_, value)| value.as_str())
            })
    }

    /// Writes the plan into a request. Invalid header bytes are refused rather
    /// than silently dropped, because a newline here would be header injection.
    pub fn apply(&self, headers: &mut HeaderMap) -> Result<(), ProviderError> {
        for name in &self.remove {
            headers.remove(name.as_str());
        }
        for (name, value) in self.header.iter().chain(self.extra.iter()) {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ProviderError::config("Invalid authentication header name"))?;
            let value = HeaderValue::from_str(value.as_str())
                .map_err(|_| ProviderError::config("Invalid authentication header value"))?;
            headers.insert(name, value);
        }
        Ok(())
    }
}

/// Turns a resolved credential into an auth plan. API keys keep their existing
/// per-protocol headers; OAuth tokens are bound to allowlisted endpoint hosts.
pub fn plan(entry: &AiProviderEntry, credential: &Credential) -> Result<AuthPlan, ProviderError> {
    match credential {
        Credential::ApiKey(key) => api_key_plan(entry, key.as_deref()),
        Credential::OAuth(tokens) => oauth_plan(entry, tokens),
    }
}

fn api_key_plan(entry: &AiProviderEntry, key: Option<&str>) -> Result<AuthPlan, ProviderError> {
    let key = key.filter(|key| !key.is_empty());
    // A keyless request is only sane where the operator names the server: a
    // local model or their own gateway. A catalog vendor host authenticates,
    // so a missing key is a configuration fault, not a 401 to decode.
    let keyless_allowed =
        entry.wire() == Wire::OpenAiChatCompletions && !super::catalog::requires_credential(entry);
    if key.is_none() && !keyless_allowed {
        return Err(ProviderError::new(
            ProviderErrorKind::Auth,
            format!(
                "No API key saved for {}. Add one in the provider settings.",
                entry.name
            ),
        ));
    }
    let Some(key) = key else {
        return Ok(AuthPlan::default());
    };
    let plan = match super::catalog::auth_mode(entry) {
        super::catalog::AuthMode::Bearer => AuthPlan::bearer(key),
        super::catalog::AuthMode::XApiKey => AuthPlan::header("x-api-key", key),
        super::catalog::AuthMode::XGoogApiKey => AuthPlan::header("x-goog-api-key", key),
    };
    // Anthropic's documented header is x-api-key, but Claude-compatible
    // gateways commonly accept only `Authorization: Bearer`. The official
    // host keeps the documented shape; a custom host gets both, so a proxy
    // that reads either one connects without an extra setting.
    if entry.wire() == Wire::AnthropicMessages
        && super::catalog::auth_mode(entry) == super::catalog::AuthMode::XApiKey
        && endpoint_host(&entry.endpoint).is_some_and(|host| {
            host != "api.anthropic.com"
                && !(host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback()))
        })
    {
        return Ok(plan.extra("authorization", format!("Bearer {key}")));
    }
    Ok(plan)
}

fn oauth_plan(entry: &AiProviderEntry, tokens: &TokenSet) -> Result<AuthPlan, ProviderError> {
    let provider = entry
        .auth_provider
        .filter(|provider| *provider != AuthProvider::Unknown)
        .ok_or_else(|| ProviderError::config("OAuth requires an auth provider"))?;
    if tokens.access.is_empty() {
        return Err(auth_expired(entry));
    }
    if !provider.supported() {
        return Err(ProviderError::config(
            "OAuth is not supported for this provider.",
        ));
    }
    check_endpoint(entry, provider)?;
    Ok(match provider {
        AuthProvider::OpenAiChatGpt => {
            let plan = match &tokens.account_id {
                Some(account) => AuthPlan::bearer(&tokens.access)
                    .and_then(AuthPlan::default().extra("chatgpt-account-id", account)),
                None => AuthPlan::bearer(&tokens.access),
            };
            plan.body(BodyEncoding::Zstd)
        }
        _ => {
            return Err(ProviderError::config(
                "OAuth is not supported for this provider.",
            ));
        }
    })
}

/// The provider error for an expired credential.
pub fn auth_expired(entry: &AiProviderEntry) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::AuthExpired,
        format!("Authentication expired for {}. Sign in again.", entry.name),
    )
}

/// Lowercase host of an endpoint URL.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    reqwest::Url::parse(endpoint)
        .ok()?
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_ascii_lowercase())
}

/// Hosts a provider OAuth token may be sent to.
pub fn allowed_hosts(provider: AuthProvider) -> &'static [&'static str] {
    match provider {
        AuthProvider::OpenAiChatGpt => &["api.openai.com", "chatgpt.com"],
        AuthProvider::Anthropic => &["api.anthropic.com"],
        AuthProvider::GeminiCli => &["cloudcode-pa.googleapis.com"],
        AuthProvider::Xai => &["api.x.ai"],
        AuthProvider::Unknown => &[],
    }
}

/// Exact host check against the provider's allowlist.
pub fn host_allowed(provider: AuthProvider, host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    allowed_hosts(provider)
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host))
}

/// A user-entered host is a bare host name; schemes, ports and paths are refused.
fn custom_host(raw: &str) -> Option<String> {
    let host = raw.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && !host.contains(['/', '\\', '@', '?', '#', ' ', '\t', '\r', '\n'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':'));
    valid.then_some(host)
}

fn entry_allows_host(entry: &AiProviderEntry, host: &str) -> bool {
    entry
        .oauth_allowed_hosts
        .iter()
        .filter_map(|allowed| custom_host(allowed))
        .any(|allowed| allowed == host)
}

/// Sends an OAuth token only to an allowlisted host or one the entry names.
fn check_endpoint(entry: &AiProviderEntry, provider: AuthProvider) -> Result<(), ProviderError> {
    let host = endpoint_host(&entry.endpoint)
        .ok_or_else(|| ProviderError::config("Invalid OAuth endpoint"))?;
    if host_allowed(provider, &host) || entry_allows_host(entry, &host) {
        return Ok(());
    }
    log::warn!(
        "[ai] refused to send a {} token to host {host}",
        provider.as_str()
    );
    Err(ProviderError::config(format!(
        "Refusing to send a {} token to {host}; the endpoint host is not allowlisted.",
        provider.as_str()
    )))
}

/// Config-time counterpart of `check_endpoint`, with user-facing messages.
pub fn validate_oauth_entry(entry: &AiProviderEntry) -> Result<(), String> {
    if entry.auth != AuthKind::OAuth {
        return Ok(());
    }
    let provider = entry
        .auth_provider
        .filter(|provider| *provider != AuthProvider::Unknown)
        .ok_or("OAuth requires an auth provider")?;
    if !provider.supported() {
        return Err("OAuth sign-in is not available for this provider yet.".into());
    }
    if provider == AuthProvider::OpenAiChatGpt && entry.wire() != Wire::OpenAiResponses {
        return Err("ChatGPT OAuth requires the OpenAI Responses protocol.".into());
    }
    if entry
        .oauth_allowed_hosts
        .iter()
        .any(|host| custom_host(host).is_none())
    {
        return Err(
            "OAuth allowed hosts must be bare host names such as gateway.example.com.".into(),
        );
    }
    check_endpoint(entry, provider).map_err(|error| error.message)
}

/// Returns the keyring account identifier for an API-key entry: `<id>:<hex sha256>`.
pub fn api_key_account(entry: &AiProviderEntry) -> String {
    format!(
        "{}:{:x}",
        entry.id,
        Sha256::digest(entry.endpoint.as_bytes())
    )
}

/// Stable credential-store account key for a provider entry.
pub fn oauth_account(entry: &AiProviderEntry) -> Result<String, String> {
    let provider = entry
        .auth_provider
        .filter(|provider| *provider != AuthProvider::Unknown)
        .ok_or("OAuth requires an auth provider")?;
    Ok(format!("{}:{}", entry.id, provider.as_str()))
}

fn oauth_store() -> KeyringSecrets {
    KeyringSecrets::new(OAUTH_SERVICE)
}

/// Message for a credential-store failure. The two environmental kinds get a
/// shared, actionable text; everything else keeps the caller's wording.
pub fn store_message(error: StoreError, fallback: &str) -> String {
    match error {
        StoreError::Unavailable => STORE_UNAVAILABLE.to_string(),
        StoreError::Locked => STORE_LOCKED.to_string(),
        _ => fallback.to_string(),
    }
}

/// Message for a failed *read*. An unreadable store is a refusal here: the OS
/// asked and the answer was no, so the text says how to grant access again.
pub fn read_message(error: StoreError, fallback: &str) -> String {
    match error {
        StoreError::Unreadable => STORE_REFUSED.to_string(),
        other => store_message(other, fallback),
    }
}

/// Read the stored OAuth token set, if any.
pub fn read_oauth_token(entry: &AiProviderEntry) -> Result<Option<TokenSet>, String> {
    let account = oauth_account(entry)?;
    let raw = credential_store::load_text(&oauth_store(), &account)
        .map_err(|error| read_message(error, "Cannot unlock/read the OAuth credential store."))?;
    match raw {
        None => Ok(None),
        Some(raw) => serde_json::from_str(raw.as_str())
            .map(Some)
            .map_err(|_| "Stored OAuth credential is unreadable".into()),
    }
}

/// Persist an OAuth token set.
pub fn write_oauth_token(entry: &AiProviderEntry, tokens: &TokenSet) -> Result<(), String> {
    let raw = serde_json::to_string(tokens)
        .map_err(|_| "Could not encode the OAuth credential".to_string())?;
    credential_store::store_text(&oauth_store(), &oauth_account(entry)?, &raw)
        .map_err(|_| "Could not save the OAuth credential; the token was not saved".into())
}

/// Delete the stored OAuth token set.
pub fn delete_oauth_token(entry: &AiProviderEntry) -> Result<(), String> {
    credential_store::delete_entry(&oauth_store(), &oauth_account(entry)?)
        .map_err(|_| "Could not remove the OAuth credential".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AiCapabilities, ProviderKind};

    #[test]
    fn store_failures_have_stable_messages() {
        assert_eq!(
            store_message(StoreError::Unavailable, "fallback"),
            STORE_UNAVAILABLE
        );
        assert_eq!(store_message(StoreError::Locked, "fallback"), STORE_LOCKED);
        assert_eq!(store_message(StoreError::Write, "fallback"), "fallback");
        assert_eq!(
            store_message(StoreError::Unreadable, "fallback"),
            "fallback"
        );
        assert_eq!(store_message(StoreError::Remove, "fallback"), "fallback");
    }

    /// A refused read is the only failure a dismissed prompt can produce, and
    /// the operator has to be told how to grant access again.
    #[test]
    fn a_refused_read_explains_how_to_grant_access_again() {
        assert_eq!(
            read_message(StoreError::Unreadable, "fallback"),
            STORE_REFUSED
        );
        assert!(STORE_REFUSED.contains("permission prompt"));
        assert!(STORE_REFUSED.contains("once per build"));
        assert!(STORE_REFUSED.contains("OPENCOMPOSE_AI_KEY"));
        // The other kinds keep their own text.
        assert_eq!(
            read_message(StoreError::Unavailable, "fallback"),
            STORE_UNAVAILABLE
        );
        assert_eq!(read_message(StoreError::Write, "fallback"), "fallback");
    }

    fn entry(auth: AuthKind, provider: Option<AuthProvider>) -> AiProviderEntry {
        AiProviderEntry {
            id: "fixture".into(),
            name: "Fixture".into(),
            kind: ProviderKind::OpenAiResponses,
            wire: Some(Wire::OpenAiResponses),
            auth,
            auth_provider: provider,
            oauth_allowed_hosts: Vec::new(),
            endpoint: "https://api.openai.com/v1/responses".into(),
            model: "gpt-6.1-sol".into(),
            models: Vec::new(),
            context_length: 128000,
            max_tokens: 4096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities::default(),
        }
    }

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            v: 1,
            access: access.into(),
            refresh: "refresh-secret".into(),
            expires_at: 0,
            account_id: Some("account-1".into()),
            account_label: None,
            scope: Some("chatgpt.tokens.use.direct".into()),
            client_id: None,
        }
    }

    fn plan_with(
        entry: &AiProviderEntry,
        credential: &Credential,
    ) -> Result<AuthPlan, ProviderError> {
        plan(entry, credential)
    }

    #[test]
    fn anthropic_gateways_get_bearer_beside_the_documented_key_header() {
        let mut entry = entry(AuthKind::ApiKey, None);
        entry.wire = Some(Wire::AnthropicMessages);
        entry.endpoint = "https://claude-gateway.example/v1/messages".into();
        let plan = plan_with(&entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
        assert_eq!(plan.header_value("x-api-key"), Some("fixture-secret"));
        assert_eq!(
            plan.header_value("authorization"),
            Some("Bearer fixture-secret")
        );

        // The official host keeps only the header Anthropic documents.
        entry.endpoint = "https://api.anthropic.com/v1/messages".into();
        let official =
            plan_with(&entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
        assert_eq!(official.header_value("x-api-key"), Some("fixture-secret"));
        assert_eq!(official.header_value("authorization"), None);
    }

    #[test]
    fn api_key_plans_keep_each_protocol_header() {
        for (wire, name) in [
            (Wire::AnthropicMessages, "x-api-key"),
            (Wire::GeminiInteractions, "x-goog-api-key"),
            (Wire::OpenAiResponses, "authorization"),
            (Wire::OpenAiChatCompletions, "authorization"),
        ] {
            let mut entry = entry(AuthKind::ApiKey, None);
            entry.wire = Some(wire);
            let plan =
                plan_with(&entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
            let expected = if name == "authorization" {
                "Bearer fixture-secret".to_string()
            } else {
                "fixture-secret".to_string()
            };
            assert_eq!(plan.header_value(name), Some(expected.as_str()), "{wire:?}");
            let mut headers = HeaderMap::new();
            plan.apply(&mut headers).unwrap();
            assert_eq!(headers[name], expected);
        }
    }

    #[test]
    fn catalog_auth_mode_sends_the_opencode_console_token_as_a_bearer() {
        let mut entry = entry(AuthKind::ApiKey, None);
        entry.wire = Some(Wire::GeminiGenerate);
        entry.endpoint = "https://opencode.ai/inference/go/google/v1beta".into();
        entry.model = "gemini-4-argon".into();
        let plan = plan_with(&entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
        assert_eq!(
            plan.header_value("authorization"),
            Some("Bearer fixture-secret")
        );
        assert_eq!(plan.header_value("x-goog-api-key"), None);
    }

    #[test]
    fn keyless_local_chat_is_allowed_but_remote_protocols_are_not() {
        let mut local = entry(AuthKind::ApiKey, None);
        local.wire = Some(Wire::OpenAiChatCompletions);
        local.endpoint = "http://127.0.0.1:11434/v1/chat/completions".into();
        assert!(plan_with(&local, &Credential::ApiKey(None)).is_ok());
        let missing =
            plan_with(&entry(AuthKind::ApiKey, None), &Credential::ApiKey(None)).unwrap_err();
        assert_eq!(missing.kind, ProviderErrorKind::Auth);
    }

    /// Chat Completions is also the protocol every OpenAI-compatible vendor
    /// speaks, so "no key" must not fall through to an unauthenticated request
    /// there: the provider would answer 401 and the panel would blame the key
    /// instead of saying none was saved.
    #[test]
    fn a_keyless_request_to_a_vendor_host_names_the_missing_key() {
        let mut vendor = entry(AuthKind::ApiKey, None);
        vendor.wire = Some(Wire::OpenAiChatCompletions);
        vendor.endpoint = "https://api.deepseek.com/chat/completions".into();
        vendor.model = "deepseek-flash".into();
        let error = plan_with(&vendor, &Credential::ApiKey(None)).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(
            error.message.contains("No API key saved for Fixture"),
            "{}",
            error.message
        );
        // The same endpoint with a key still plans a bearer request.
        assert!(plan_with(&vendor, &Credential::ApiKey(Some("sk-x".into()))).is_ok());
        // A custom gateway stays keyless, because its authentication is its own.
        let mut custom = vendor.clone();
        custom.endpoint = "https://llm.acme.test/v1/chat/completions".into();
        assert!(plan_with(&custom, &Credential::ApiKey(None)).is_ok());
    }

    #[test]
    fn oauth_plan_adds_account_header_and_zstd_body() {
        let entry = entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt));
        let plan = plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).unwrap();
        assert_eq!(
            plan.header_value("authorization"),
            Some("Bearer access-secret")
        );
        assert_eq!(plan.header_value("chatgpt-account-id"), Some("account-1"));
        assert_eq!(plan.body_encoding(), BodyEncoding::Zstd);
    }

    #[test]
    fn a_zero_expiry_token_never_refreshes() {
        let tokens = tokens("access-secret");
        assert_eq!(tokens.expires_at, 0);
        assert!(!tokens.expires_within(i64::MAX / 2));
        assert!(!tokens.expired(i64::MAX / 2));
        let mut expiring = tokens;
        expiring.expires_at = 1_000;
        assert!(expiring.expires_within(1_000));
        assert!(expiring.expired(1_000));
    }

    #[test]
    fn oauth_host_allowlist_compares_whole_hosts_only() {
        for provider in [
            AuthProvider::OpenAiChatGpt,
            AuthProvider::Anthropic,
            AuthProvider::GeminiCli,
            AuthProvider::Xai,
        ] {
            for allowed in allowed_hosts(provider) {
                assert!(host_allowed(provider, allowed), "{provider:?}");
                for host in [
                    format!("{allowed}.evil.test"),
                    format!("evil{allowed}"),
                    format!("sub.{allowed}"),
                ] {
                    assert!(!host_allowed(provider, &host), "{provider:?}: {host}");
                }
            }
        }
    }

    #[test]
    fn oauth_endpoint_host_allowlist_refuses_other_hosts() {
        let mut entry = entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt));
        entry.endpoint = "https://evil.example/v1/responses".into();
        let error = plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Config);
        assert!(!error.message.contains("access-secret"));
        assert!(validate_oauth_entry(&entry).is_err());
        entry.oauth_allowed_hosts = vec!["evil.example".into()];
        assert!(validate_oauth_entry(&entry).is_ok());
        assert!(plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).is_ok());
        entry.oauth_allowed_hosts = vec!["other.example".into()];
        assert!(validate_oauth_entry(&entry).is_err());
        assert!(plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).is_err());
    }

    #[test]
    fn a_legacy_allow_custom_endpoint_flag_no_longer_bypasses_the_allowlist() {
        let yaml = r#"
id: fixture
name: Fixture
kind: openai_responses
auth: oauth
auth_provider: openai_chat_gpt
allow_custom_endpoint: true
endpoint: https://evil.example/v1/responses
model: gpt-6.1-sol
"#;
        let entry: AiProviderEntry = serde_yaml::from_str(yaml).unwrap();
        assert!(entry.oauth_allowed_hosts.is_empty());
        assert!(validate_oauth_entry(&entry).is_err());
        assert!(plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).is_err());
    }

    #[test]
    fn oauth_allowed_hosts_must_be_bare_exact_hosts() {
        let mut entry = entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt));
        entry.endpoint = "https://gateway.example/v1/responses".into();
        for invalid in [
            "https://gateway.example",
            "gateway.example/v1",
            "user@gateway.example",
            "gateway.example?x=1",
            "",
        ] {
            entry.oauth_allowed_hosts = vec![invalid.into()];
            assert!(validate_oauth_entry(&entry).is_err(), "{invalid}");
        }
        entry.oauth_allowed_hosts = vec!["GATEWAY.example".into()];
        assert!(validate_oauth_entry(&entry).is_ok());
        assert!(plan_with(&entry, &Credential::OAuth(tokens("access-secret"))).is_ok());
    }

    #[test]
    fn oauth_validation_rejects_unsupported_provider_and_wrong_protocol() {
        let mut unsupported = entry(AuthKind::OAuth, Some(AuthProvider::Anthropic));
        unsupported.wire = Some(Wire::AnthropicMessages);
        unsupported.endpoint = "https://api.anthropic.com/v1/messages".into();
        assert!(validate_oauth_entry(&unsupported).is_err());
        let mut wrong = entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt));
        wrong.wire = Some(Wire::OpenAiChatCompletions);
        assert!(validate_oauth_entry(&wrong).is_err());
        assert!(
            validate_oauth_entry(&entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt)))
                .is_ok()
        );
        assert!(validate_oauth_entry(&entry(AuthKind::OAuth, None)).is_err());
    }

    #[test]
    fn plan_composition_and_header_injection() {
        let plan = AuthPlan::default()
            .extra("chatgpt-account-id", "acct")
            .and_then(AuthPlan::header("x-a", "1"));
        let mut headers = HeaderMap::new();
        plan.apply(&mut headers).unwrap();
        assert_eq!(headers["chatgpt-account-id"], "acct");
        assert_eq!(headers["x-a"], "1");
        assert_eq!(
            AuthPlan::header("x-a", "1").or_else(AuthPlan::header("x-b", "2")),
            AuthPlan::header("x-a", "1")
        );
        assert_eq!(
            AuthPlan::default().or_else(AuthPlan::header("x-b", "2")),
            AuthPlan::header("x-b", "2")
        );
        let injected = AuthPlan::header("authorization", "Bearer ok\r\nx-evil: 1");
        assert!(injected.apply(&mut HeaderMap::new()).is_err());
        let mut headers = HeaderMap::new();
        headers.insert("x-remove", HeaderValue::from_static("1"));
        AuthPlan::default()
            .remove("x-remove")
            .apply(&mut headers)
            .unwrap();
        assert!(!headers.contains_key("x-remove"));
    }

    #[test]
    fn debug_redacts_tokens() {
        let text = format!("{:?}", Credential::OAuth(tokens("access-secret")).clone());
        assert!(!text.contains("access-secret"));
        assert!(!text.contains("refresh-secret"));
        assert!(!format!("{:?}", tokens("access-secret")).contains("access-secret"));
        let plan = plan_with(
            &entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt)),
            &Credential::OAuth(tokens("access-secret")),
        )
        .unwrap();
        let text = format!("{plan:?}");
        assert!(!text.contains("access-secret"));
        assert!(!text.contains("account-1"));
    }

    #[test]
    fn api_key_account_is_id_and_endpoint_hash_only() {
        let provider_entry = entry(AuthKind::ApiKey, None);
        let expected = format!(
            "fixture:{:x}",
            Sha256::digest("https://api.openai.com/v1/responses".as_bytes())
        );
        assert_eq!(api_key_account(&provider_entry), expected);
        let oauth = entry(AuthKind::OAuth, Some(AuthProvider::OpenAiChatGpt));
        assert_eq!(oauth_account(&oauth).unwrap(), "fixture:openai_chat_gpt");
        assert_ne!(
            api_key_account(&provider_entry),
            oauth_account(&oauth).unwrap()
        );
    }
}
