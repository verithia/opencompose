//! Lazy, single-flight OAuth refresh at credential-resolution time: one
//! write-back, no background timer, and terminal errors require a new sign-in.
use super::auth::{TokenSet, auth_expired, oauth_account};
use super::{BoxFuture, ProviderError, ProviderErrorKind};
use crate::commands::{AiProviderEntry, AuthProvider};
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

mod error;
pub mod openai;

pub use error::{FlowError, SignInError, SignInErrorKind};

#[derive(Debug)]
/// Failure from a provider token refresh.
pub struct RefreshError {
    pub terminal: bool,
    pub kind: ProviderErrorKind,
    pub message: String,
}

impl RefreshError {
    /// A terminal refresh failure requiring a new sign-in.
    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            terminal: true,
            kind: ProviderErrorKind::AuthExpired,
            message: message.into(),
        }
    }

    #[allow(dead_code)]
    /// A retryable refresh failure.
    pub fn transient(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            terminal: false,
            kind,
            message: message.into(),
        }
    }
}

/// Current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

/// Storage seam so the refresh state machine is testable without the OS keyring.
pub trait TokenStore: Send + Sync {
    fn read<'a>(
        &'a self,
        entry: &'a AiProviderEntry,
    ) -> BoxFuture<'a, Result<Option<TokenSet>, String>>;
    fn write<'a>(
        &'a self,
        entry: &'a AiProviderEntry,
        tokens: &'a TokenSet,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn delete<'a>(&'a self, entry: &'a AiProviderEntry) -> BoxFuture<'a, Result<(), String>>;
}

/// Token store backed by the OS credential store.
pub struct KeyringStore;

impl TokenStore for KeyringStore {
    fn read<'a>(
        &'a self,
        entry: &'a AiProviderEntry,
    ) -> BoxFuture<'a, Result<Option<TokenSet>, String>> {
        Box::pin(async move {
            let entry = entry.clone();
            tauri::async_runtime::spawn_blocking(move || super::auth::read_oauth_token(&entry))
                .await
                .map_err(|_| "Credential task failed".to_string())?
        })
    }

    fn write<'a>(
        &'a self,
        entry: &'a AiProviderEntry,
        tokens: &'a TokenSet,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let entry = entry.clone();
            let tokens = tokens.clone();
            tauri::async_runtime::spawn_blocking(move || {
                super::auth::write_oauth_token(&entry, &tokens)
            })
            .await
            .map_err(|_| "Credential task failed".to_string())?
        })
    }

    fn delete<'a>(&'a self, entry: &'a AiProviderEntry) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let entry = entry.clone();
            tauri::async_runtime::spawn_blocking(move || super::auth::delete_oauth_token(&entry))
                .await
                .map_err(|_| "Credential task failed".to_string())?
        })
    }
}

/// One lock per OAuth account. The second caller waits, then re-reads the token
/// the first caller wrote back, so a rotating refresh token is never used twice.
#[derive(Default)]
/// Per-account single-flight token refresh coordinator.
pub struct RefreshCoordinator {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl RefreshCoordinator {
    fn lock_for(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        locks
            .entry(account.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Return a fresh token set, refreshing when due or forced.
    pub async fn ensure(
        &self,
        store: &dyn TokenStore,
        entry: &AiProviderEntry,
        force: bool,
    ) -> Result<TokenSet, ProviderError> {
        self.ensure_with(store, entry, force, refresh).await
    }

    /// Ensure a token set with an injected refresh function.
    pub async fn ensure_with<F, Fut>(
        &self,
        store: &dyn TokenStore,
        entry: &AiProviderEntry,
        force: bool,
        refresh: F,
    ) -> Result<TokenSet, ProviderError>
    where
        F: Fn(AiProviderEntry, TokenSet) -> Fut,
        Fut: Future<Output = Result<TokenSet, RefreshError>>,
    {
        let account = oauth_account(entry).map_err(ProviderError::config)?;
        let lock = self.lock_for(&account);
        let _guard = lock.lock().await;
        let tokens = store
            .read(entry)
            .await
            .map_err(ProviderError::config)?
            .ok_or_else(|| auth_expired(entry))?;
        if !force && !tokens.expires_within(now_ms()) {
            return Ok(tokens);
        }
        match refresh(entry.clone(), tokens.clone()).await {
            Ok(fresh) => {
                store.write(entry, &fresh).await.map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::Network,
                        "Could not save the refreshed OAuth credential.",
                    )
                })?;
                Ok(fresh)
            }
            Err(error) if error.terminal => {
                let _ = store.delete(entry).await;
                Err(ProviderError::new(
                    ProviderErrorKind::AuthExpired,
                    format!("Authentication expired for {}. Sign in again.", entry.name),
                ))
            }
            Err(error) => Err(ProviderError::new(error.kind, error.message)),
        }
    }
}

/// Process-wide OAuth refresh coordinator.
pub static COORDINATOR: std::sync::LazyLock<RefreshCoordinator> =
    std::sync::LazyLock::new(RefreshCoordinator::default);

/// Dispatches to the provider-specific token endpoint. Config validation
/// rejects every provider without a refresh implementation.
async fn refresh(entry: AiProviderEntry, tokens: TokenSet) -> Result<TokenSet, RefreshError> {
    match entry.auth_provider {
        Some(AuthProvider::OpenAiChatGpt) => openai::refresh(tokens).await,
        _ => Err(RefreshError::terminal(
            "OAuth refresh is not available for this provider.",
        )),
    }
}

/// Lifetime of an in-memory sign-in attempt.
pub const ATTEMPT_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq)]
/// State of one interactive sign-in attempt.
pub enum AttemptState {
    Pending,
    Complete,
    /// The caller gave up on the attempt. A cancel is not a failure, so it has
    /// no message and the panel returns to its signed-out state.
    Cancelled,
    Failed(String),
    Expired,
}

impl AttemptState {
    /// Wire label for the state.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed(_) => "failed",
            Self::Expired => "expired",
        }
    }

    /// Human-readable message for the state.
    pub fn message(&self) -> &str {
        match self {
            Self::Failed(message) => message,
            Self::Expired => "The sign-in window expired. Start again.",
            _ => "",
        }
    }
}

struct Attempt {
    state: AttemptState,
    abort: Option<AbortHandle>,
    /// The flow's structured failure, kept next to the state so polling is the
    /// only read.
    error: Option<SignInError>,
}

/// One attempt as the panel reads it. `error` carries the structured failure
/// of a failed or expired attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptStatus {
    pub state: String,
    pub message: String,
    pub error: Option<SignInError>,
}

/// In-memory sign-in attempts; nothing secret is stored here.
#[derive(Clone, Default)]
pub struct Attempts(Arc<Mutex<HashMap<String, Attempt>>>);

impl Attempts {
    fn insert(&self, id: &str) {
        self.0.lock().unwrap().insert(
            id.to_owned(),
            Attempt {
                state: AttemptState::Pending,
                abort: None,
                error: None,
            },
        );
    }

    fn attach(&self, id: &str, abort: AbortHandle) {
        let mut attempts = self.0.lock().unwrap();
        match attempts.get_mut(id) {
            // A cancel that landed between the insert and this call already
            // gave the attempt up; the flow must not keep its listener bound.
            Some(attempt) if attempt.state == AttemptState::Pending => {
                attempt.abort = Some(abort);
            }
            _ => abort.abort(),
        }
    }

    fn set(&self, id: &str, state: AttemptState) {
        if let Some(attempt) = self.0.lock().unwrap().get_mut(id) {
            attempt.state = state;
        }
    }

    /// Ends a pending attempt with the flow's structured failure. The state the
    /// panel keys on stays derived from the kind, and a cancel that already won
    /// the race is not overwritten.
    fn fail(&self, id: &str, error: SignInError) {
        let mut attempts = self.0.lock().unwrap();
        let Some(attempt) = attempts.get_mut(id) else {
            return;
        };
        if attempt.state != AttemptState::Pending {
            return;
        }
        attempt.state = if error.kind == SignInErrorKind::Expired {
            AttemptState::Expired
        } else {
            AttemptState::Failed(error.message.clone())
        };
        attempt.error = Some(error);
    }

    /// Read the status of one attempt.
    pub fn status(&self, id: &str) -> Option<AttemptStatus> {
        self.0.lock().unwrap().get(id).map(|attempt| AttemptStatus {
            state: attempt.state.label().into(),
            message: attempt.state.message().into(),
            error: attempt.error.clone(),
        })
    }

    /// Gives the attempt up and closes whatever it holds; the listener lives in
    /// the task's future, so aborting it releases the port. Returns false when
    /// the attempt already ended, which makes a repeated cancel a no-op.
    pub fn cancel(&self, id: &str) -> bool {
        let mut attempts = self.0.lock().unwrap();
        let Some(attempt) = attempts.get_mut(id) else {
            return false;
        };
        if attempt.state != AttemptState::Pending {
            return false;
        }
        if let Some(abort) = attempt.abort.take() {
            abort.abort();
        }
        attempt.state = AttemptState::Cancelled;
        true
    }
}

#[derive(Debug, Serialize)]
/// A started OAuth flow with its URL and expiry.
pub struct OAuthStart {
    pub attempt_id: String,
    pub url: String,
    pub mode: String,
    pub expires_at: i64,
    pub opened: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
}

/// Starts the provider's loopback PKCE sign-in and returns an attempt the UI
/// polls until it is no longer pending.
pub async fn begin(entry: AiProviderEntry, attempts: &Attempts) -> Result<OAuthStart, String> {
    match entry.auth_provider {
        Some(AuthProvider::OpenAiChatGpt) => begin_chatgpt(entry, attempts).await,
        _ => Err("OAuth sign-in is not available for this provider.".into()),
    }
}

/// Starts a PKCE loopback sign-in. The listener binds 127.0.0.1 only and the
/// attempt expires after five minutes; the browser URL is returned to the UI.
async fn begin_chatgpt(entry: AiProviderEntry, attempts: &Attempts) -> Result<OAuthStart, String> {
    if openai::CLIENT_ID.is_empty() {
        return Err("OAuth sign-in is not available in this build.".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|_| "Could not start the sign-in callback listener.".to_string())?;
    begin_chatgpt_on(listener, Arc::new(KeyringStore), entry, attempts).await
}

/// Runs the sign-in on an already bound loopback listener. The attempt's task
/// owns the listener, so cancelling the attempt releases the port.
async fn begin_chatgpt_on(
    listener: TcpListener,
    store: Arc<dyn TokenStore>,
    entry: AiProviderEntry,
    attempts: &Attempts,
) -> Result<OAuthStart, String> {
    let port = listener
        .local_addr()
        .map_err(|_| "Could not read the sign-in callback address.".to_string())?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let state = openai::random_token(32)?;
    let nonce = openai::random_token(32)?;
    let pkce = openai::pkce()?;
    let attempt_id = openai::random_token(16)?;
    let agent_host_id = format!("urn:uuid:{}", openai::random_token(16)?);
    let url = openai::authorize_url(
        &redirect_uri,
        &state,
        &nonce,
        &pkce.challenge,
        &agent_host_id,
    );
    attempts.insert(&attempt_id);
    let registry = attempts.clone();
    let task_id = attempt_id.clone();
    let expires_at = now_ms() + ATTEMPT_TTL.as_millis() as i64;
    let handle = tokio::spawn(async move {
        let result = run_flow(
            listener,
            store.as_ref(),
            &entry,
            &state,
            &redirect_uri,
            &pkce.verifier,
            &nonce,
        )
        .await;
        match result {
            Ok(()) => registry.set(&task_id, AttemptState::Complete),
            Err(error) => registry.fail(&task_id, SignInError::from(error)),
        }
    });
    attempts.attach(&attempt_id, handle.abort_handle());
    Ok(OAuthStart {
        attempt_id,
        url,
        mode: "auto".into(),
        expires_at,
        opened: false,
        user_code: None,
        verification_uri: None,
    })
}

async fn run_flow(
    listener: TcpListener,
    store: &dyn TokenStore,
    entry: &AiProviderEntry,
    state: &str,
    redirect_uri: &str,
    verifier: &str,
    nonce: &str,
) -> Result<(), FlowError> {
    let callback = accept_callback(listener, "/auth/callback", state, ATTEMPT_TTL).await?;
    let client_id = callback.client_id.as_deref().unwrap_or(openai::CLIENT_ID);
    let tokens = openai::exchange_code(
        openai::TOKEN_URL,
        redirect_uri,
        &callback.code,
        verifier,
        client_id,
        nonce,
    )
    .await
    .map_err(FlowError::from)?;
    store.write(entry, &tokens).await.map_err(FlowError::new)
}

/// Serves the loopback redirect. Any other path is rejected; a state mismatch is
/// terminal for the attempt.
pub async fn accept_callback(
    listener: TcpListener,
    callback_path: &str,
    expected_state: &str,
    timeout: Duration,
) -> Result<openai::CallbackParams, FlowError> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FlowError::expired());
        }
        let received = match tokio::time::timeout(remaining, listener.accept()).await {
            Err(_) => return Err(FlowError::expired()),
            Ok(Err(_)) => continue,
            Ok(Ok(received)) => received,
        };
        let (mut stream, _) = received;
        let mut buffer = vec![0u8; 8192];
        let count =
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer)).await {
                Ok(Ok(count)) if count > 0 => count,
                _ => {
                    respond(&mut stream, "400 Bad Request", "<p>Bad request.</p>").await;
                    continue;
                }
            };
        let request = String::from_utf8_lossy(&buffer[..count]);
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != callback_path {
            respond(&mut stream, "404 Not Found", "<p>Not found.</p>").await;
            continue;
        }
        let params = match openai::parse_callback(query) {
            Ok(params) => params,
            Err(message) => {
                respond(&mut stream, "400 Bad Request", "<p>Sign-in failed.</p>").await;
                return Err(FlowError::new(message));
            }
        };
        if !constant_time_eq(&params.state, expected_state) {
            respond(&mut stream, "400 Bad Request", "<p>Sign-in failed.</p>").await;
            return Err(FlowError::new("The sign-in state did not match."));
        }
        respond(
            &mut stream,
            "200 OK",
            "<!doctype html><meta charset=\"utf-8\"><title>OpenCompose</title><p>Sign-in complete. You can close this window.</p>",
        )
        .await;
        return Ok(params);
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Opens only HTTPS URLs, through the platform helper, without a shell.
pub fn open_browser(url: &str) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AiCapabilities, AuthKind, AuthProvider, ProviderKind, Wire};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn entry() -> AiProviderEntry {
        AiProviderEntry {
            id: "chatgpt".into(),
            name: "ChatGPT".into(),
            kind: ProviderKind::OpenAiResponses,
            wire: Some(Wire::OpenAiResponses),
            auth: AuthKind::OAuth,
            auth_provider: Some(AuthProvider::OpenAiChatGpt),
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

    fn tokens(access: &str, expires_at: i64) -> TokenSet {
        TokenSet {
            v: 1,
            access: access.into(),
            refresh: "refresh-1".into(),
            expires_at,
            account_id: Some("acct".into()),
            account_label: None,
            scope: None,
            client_id: None,
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        tokens: Mutex<Option<TokenSet>>,
        writes: AtomicUsize,
        deletes: AtomicUsize,
    }

    impl TokenStore for MemoryStore {
        fn read<'a>(
            &'a self,
            _entry: &'a AiProviderEntry,
        ) -> BoxFuture<'a, Result<Option<TokenSet>, String>> {
            Box::pin(async move { Ok(self.tokens.lock().unwrap().clone()) })
        }

        fn write<'a>(
            &'a self,
            _entry: &'a AiProviderEntry,
            tokens: &'a TokenSet,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.writes.fetch_add(1, Ordering::SeqCst);
                *self.tokens.lock().unwrap() = Some(tokens.clone());
                Ok(())
            })
        }

        fn delete<'a>(&'a self, _entry: &'a AiProviderEntry) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.deletes.fetch_add(1, Ordering::SeqCst);
                *self.tokens.lock().unwrap() = None;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn refreshes_inside_the_five_minute_skew_and_writes_back() {
        for (offset, expected_calls) in
            [(4 * 60 * 1000, 1usize), (-60 * 1000, 1), (6 * 60 * 1000, 0)]
        {
            let store = MemoryStore::default();
            *store.tokens.lock().unwrap() = Some(tokens("old", now_ms() + offset));
            let calls = AtomicUsize::new(0);
            let coordinator = RefreshCoordinator::default();
            let result = coordinator
                .ensure_with(&store, &entry(), false, |_, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { Ok(tokens("fresh", now_ms() + 3_600_000)) }
                })
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
            assert_eq!(
                result.access,
                if expected_calls == 1 { "fresh" } else { "old" }
            );
            assert_eq!(store.writes.load(Ordering::SeqCst), expected_calls);
            assert_eq!(
                store.tokens.lock().unwrap().as_ref().unwrap().refresh,
                "refresh-1"
            );
        }
    }

    #[tokio::test]
    async fn forced_refresh_ignores_a_valid_expiry() {
        let store = MemoryStore::default();
        *store.tokens.lock().unwrap() = Some(tokens("old", now_ms() + 60 * 60 * 1000));
        let coordinator = RefreshCoordinator::default();
        let result = coordinator
            .ensure_with(&store, &entry(), true, |_, _| async {
                Ok(tokens("fresh", now_ms() + 3_600_000))
            })
            .await
            .unwrap();
        assert_eq!(result.access, "fresh");
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let store = Arc::new(MemoryStore::default());
        *store.tokens.lock().unwrap() = Some(tokens("old", now_ms() - 60_000));
        let store_ref: Arc<dyn TokenStore> = store.clone();
        let coordinator = Arc::new(RefreshCoordinator::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let coordinator = coordinator.clone();
            let store = store_ref.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                coordinator
                    .ensure_with(store.as_ref(), &entry(), false, move |_, _| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            Ok(tokens("fresh", now_ms() + 3_600_000))
                        }
                    })
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().access, "fresh");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_terminal_and_transient_tokens_are_distinct() {
        let store = MemoryStore::default();
        let coordinator = RefreshCoordinator::default();
        let missing = coordinator
            .ensure_with(&store, &entry(), false, |_, _| async {
                unreachable!("missing tokens must not refresh")
            })
            .await
            .unwrap_err();
        assert_eq!(missing.kind, ProviderErrorKind::AuthExpired);

        *store.tokens.lock().unwrap() = Some(tokens("old", now_ms() - 60_000));
        let terminal = coordinator
            .ensure_with(&store, &entry(), false, |_, _| async {
                Err(RefreshError::terminal("invalid_grant"))
            })
            .await
            .unwrap_err();
        assert_eq!(terminal.kind, ProviderErrorKind::AuthExpired);
        assert_eq!(store.deletes.load(Ordering::SeqCst), 1);
        assert!(store.tokens.lock().unwrap().is_none());

        *store.tokens.lock().unwrap() = Some(tokens("old", now_ms() - 60_000));
        let transient = coordinator
            .ensure_with(&store, &entry(), false, |_, _| async {
                Err(RefreshError::transient(
                    ProviderErrorKind::Network,
                    "token endpoint unreachable",
                ))
            })
            .await
            .unwrap_err();
        assert_eq!(transient.kind, ProviderErrorKind::Network);
        assert_eq!(store.deletes.load(Ordering::SeqCst), 1);
        assert!(store.tokens.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn loopback_callback_returns_the_code_once() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            accept_callback(
                listener,
                "/auth/callback",
                "state-1",
                Duration::from_secs(5),
            )
            .await
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /auth/callback?code=code-1&state=state-1&client_id=fixture-client HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).contains("200 OK"));
        let params = server.await.unwrap().unwrap();
        assert_eq!(params.code, "code-1");
        assert_eq!(params.state, "state-1");
        assert_eq!(params.client_id.as_deref(), Some("fixture-client"));
    }

    #[tokio::test]
    async fn loopback_callback_rejects_a_state_mismatch() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            accept_callback(
                listener,
                "/auth/callback",
                "state-1",
                Duration::from_secs(5),
            )
            .await
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /auth/callback?code=code-1&state=forged HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).contains("400 Bad Request"));
        let error = server.await.unwrap().unwrap_err();
        assert!(error.message.contains("state"));
    }

    #[test]
    fn sign_in_start_never_carries_a_token() {
        let start = OAuthStart {
            attempt_id: "attempt-1".into(),
            url: "https://github.com/login/device".into(),
            mode: "device".into(),
            expires_at: 0,
            opened: false,
            user_code: Some("ABCD-1234".into()),
            verification_uri: Some("https://github.com/login/device".into()),
        };
        let json = serde_json::to_string(&start).unwrap();
        for key in ["access", "refresh", "token"] {
            assert!(!json.contains(key), "{key}: {json}");
        }
        assert!(json.contains("ABCD-1234"));
    }

    #[tokio::test]
    async fn attempts_complete_and_cancel_are_visible_to_polling() {
        let attempts = Attempts::default();
        attempts.insert("a");
        assert_eq!(attempts.status("a").unwrap().state, "pending");
        assert!(attempts.cancel("a"));
        // A cancel is its own state, not a failure with a sentence.
        assert_eq!(attempts.status("a").unwrap().state, "cancelled");
        assert_eq!(attempts.status("a").unwrap().message, "");
        assert!(!attempts.cancel("a"));
        attempts.insert("b");
        attempts.set("b", AttemptState::Complete);
        assert_eq!(attempts.status("b").unwrap().state, "complete");
        assert_eq!(attempts.status("missing"), None);
        assert!(!open_browser("http://127.0.0.1:1455/auth/callback"));
    }

    /// A port the test alone owns, bound the way production binds its
    /// callback listener.
    async fn bound_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// Starts a ChatGPT attempt on a fresh loopback listener. Nothing reaches
    /// the provider, because every test cancels before a callback arrives.
    async fn begin_on_free_port(
        store: Arc<dyn TokenStore>,
        attempts: &Attempts,
    ) -> (OAuthStart, u16) {
        let (listener, port) = bound_listener().await;
        let start = begin_chatgpt_on(listener, store, entry(), attempts)
            .await
            .expect("the attempt starts");
        (start, port)
    }

    #[tokio::test]
    async fn a_cancelled_attempt_releases_the_loopback_port() {
        let attempts = Attempts::default();
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());

        let (first, port) = begin_on_free_port(store.clone(), &attempts).await;
        assert_eq!(attempts.status(&first.attempt_id).unwrap().state, "pending");
        assert!(
            first.url.contains(&format!("127.0.0.1%3A{port}")),
            "{}",
            first.url
        );
        // While the flow waits for the browser it holds the port.
        assert!(
            TcpListener::bind(("127.0.0.1", port)).await.is_err(),
            "the pending attempt owns the port"
        );

        assert!(attempts.cancel(&first.attempt_id));
        assert_eq!(
            attempts.status(&first.attempt_id).unwrap().state,
            "cancelled"
        );

        // The port is free again as soon as the aborted flow is dropped.
        let waiting = Instant::now();
        let rebound = loop {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => break listener,
                Err(_) => {
                    assert!(
                        waiting.elapsed() < Duration::from_secs(2),
                        "the cancelled attempt kept the port bound"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        };
        // An immediate second sign-in can run on the same port.
        let second = begin_chatgpt_on(rebound, store, entry(), &attempts)
            .await
            .unwrap();
        assert!(TcpListener::bind(("127.0.0.1", port)).await.is_err());
        assert!(attempts.cancel(&second.attempt_id));
    }

    #[tokio::test]
    async fn a_cancelled_attempt_writes_no_credential() {
        let attempts = Attempts::default();
        let store = Arc::new(MemoryStore::default());
        let sink: Arc<dyn TokenStore> = store.clone();

        let (start, _) = begin_on_free_port(sink, &attempts).await;
        // Let the flow reach its first await before giving up on it. An attempt
        // that wrote anything on the way in would have done so now.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(attempts.status(&start.attempt_id).unwrap().state, "pending");
        assert!(attempts.cancel(&start.attempt_id));
        // The cancelled flow is dropped where it waited, before any exchange.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            attempts.status(&start.attempt_id).unwrap().state,
            "cancelled"
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 0);
        assert!(store.tokens.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn cancelling_an_attempt_is_idempotent_in_every_state() {
        let attempts = Attempts::default();
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let (start, _) = begin_on_free_port(store, &attempts).await;

        assert!(attempts.cancel(&start.attempt_id));
        // A second cancel changes nothing and is not an error.
        assert!(!attempts.cancel(&start.attempt_id));
        assert_eq!(
            attempts.status(&start.attempt_id).unwrap().state,
            "cancelled"
        );

        attempts.insert("done");
        attempts.set("done", AttemptState::Complete);
        assert!(!attempts.cancel("done"));
        assert_eq!(attempts.status("done").unwrap().state, "complete");
        assert!(!attempts.cancel("missing"));
    }

    #[tokio::test]
    async fn a_cancel_before_the_task_is_attached_still_aborts_it() {
        // The window between inserting the attempt and attaching its handle: a
        // cancel that lands there must abort the task instead of leaving it to
        // hold the listener until the window expires.
        let attempts = Attempts::default();
        attempts.insert("a");
        assert!(attempts.cancel("a"));

        struct Gone(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Gone {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marker = Gone(dropped.clone());
        let task = tokio::spawn(async move {
            let _marker = marker;
            std::future::pending::<()>().await;
        });
        attempts.attach("a", task.abort_handle());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            dropped.load(Ordering::SeqCst),
            "the flow outlived the cancel and would keep its listener"
        );
        assert_eq!(attempts.status("a").unwrap().state, "cancelled");
    }

    #[tokio::test]
    async fn a_failed_attempt_carries_the_structured_failure_to_the_poll() {
        let attempts = Attempts::default();
        attempts.insert("a");
        attempts.fail(
            "a",
            SignInError::from(FlowError::new("The sign-in state did not match.")),
        );
        let status = attempts.status("a").unwrap();
        // The panel keys on the state and the kind, never on the sentence.
        assert_eq!(status.state, "failed");
        assert_eq!(status.message, "The sign-in state did not match.");
        let error = status.error.expect("the structured failure travels");
        assert_eq!(error.kind, SignInErrorKind::Failed);

        // A lapsed window keeps its own state and its own sentence.
        attempts.insert("b");
        attempts.fail("b", SignInError::from(FlowError::expired()));
        let expired = attempts.status("b").unwrap();
        assert_eq!(expired.state, "expired");
        assert_eq!(expired.message, "The sign-in window expired. Start again.");
        assert_eq!(expired.error.unwrap().kind, SignInErrorKind::Expired);
    }

    #[tokio::test]
    async fn a_cancel_that_won_the_race_is_not_rewritten_by_a_late_failure() {
        let attempts = Attempts::default();
        attempts.insert("a");
        assert!(attempts.cancel("a"));
        attempts.fail("a", SignInError::from(FlowError::new("late")));
        let status = attempts.status("a").unwrap();
        assert_eq!(status.state, "cancelled");
        assert!(status.error.is_none());
    }
}
