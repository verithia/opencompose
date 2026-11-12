//! Credential-isolated AI command surface. Provider payloads, URLs and keys are
//! never logged; keys stay in the OS credential store.
use crate::ai::auth::{self, Credential};
#[cfg(test)]
use crate::ai::chat_completions::Completion;
use crate::ai::credential_store::{self, KeyringSecrets};
use crate::ai::oauth::{self, KeyringStore};
use crate::ai::secret::Secret;
use crate::ai::{
    self, LIMIT, MAX_WIRE_MESSAGES, ProviderError, ProviderErrorKind, ProviderRequest, endpoint,
};
use crate::commands::{AiProviderConfig, AiProviderEntry, AuthKind, AuthProvider, load_ai_config};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, State};
use tokio::sync::oneshot;
use zeroize::Zeroize;

/// The one provider request the backend is running, if any. The slot is claimed
/// before the request is prepared, and the conversation it saves into is filled
/// in once that is known, so a window that reloaded mid-turn can ask whether the
/// turn it started still exists.
#[derive(Default)]
pub struct Requests(Mutex<HashMap<String, ActiveRequest>>);

#[derive(Clone, Default)]
pub struct SeparatorJobs(Arc<Mutex<SeparatorJobState>>);

#[derive(Default)]
struct SeparatorJobState {
    running: HashMap<String, Arc<AtomicBool>>,
    // A Stop IPC may arrive ahead of the tool IPC it refers to.
    early_stops: VecDeque<String>,
}

struct SeparatorJob {
    jobs: SeparatorJobs,
    id: String,
    cancel: Arc<AtomicBool>,
}

impl SeparatorJobs {
    fn begin(&self, id: String) -> Result<SeparatorJob, String> {
        if id.is_empty() || id.len() > 64 {
            return Err("Invalid separator cancellation ID".into());
        }
        let mut state = self.0.lock().map_err(|_| "Separator job lock failed")?;
        if state.running.contains_key(&id) {
            return Err("Separator job is already running".into());
        }
        let early = state.early_stops.iter().position(|stopped| stopped == &id);
        if let Some(index) = early {
            state.early_stops.remove(index);
        }
        let cancel = Arc::new(AtomicBool::new(early.is_some()));
        state.running.insert(id.clone(), Arc::clone(&cancel));
        Ok(SeparatorJob {
            jobs: self.clone(),
            id,
            cancel,
        })
    }

    fn cancel(&self, id: String) -> Result<(), String> {
        if id.is_empty() || id.len() > 64 {
            return Err("Invalid separator cancellation ID".into());
        }
        let mut state = self.0.lock().map_err(|_| "Separator job lock failed")?;
        if let Some(cancel) = state.running.get(&id) {
            cancel.store(true, Ordering::Release);
        } else if !state.early_stops.contains(&id) {
            if state.early_stops.len() == 64 {
                state.early_stops.pop_front();
            }
            state.early_stops.push_back(id);
        }
        Ok(())
    }
}

impl Drop for SeparatorJob {
    fn drop(&mut self) {
        if let Ok(mut state) = self.jobs.0.lock() {
            state.running.remove(&self.id);
        }
    }
}

#[tauri::command]
pub fn ai_cancel_separator(
    cancellation_id: String,
    jobs: State<SeparatorJobs>,
) -> Result<(), String> {
    jobs.cancel(cancellation_id)
}

struct RequestClaim {
    cancel_rx: oneshot::Receiver<()>,
    identity: Arc<()>,
}

struct ActiveRequest {
    cancel: oneshot::Sender<()>,
    session_id: Option<String>,
    identity: Arc<()>,
}

/// A turn the backend is still running, as `ai_active_turn` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveTurn {
    /// The id `ai_cancel` takes.
    pub request_id: String,
    /// The conversation the reply is saved into; absent until it is resolved.
    pub session_id: Option<String>,
}

/// Owns the shared provider slot through headless persistence and cleanup.
pub(crate) struct HeadlessRequest<'a> {
    requests: &'a Requests,
    id: String,
    owner: Arc<()>,
    cancel: Option<oneshot::Receiver<()>>,
}

impl HeadlessRequest<'_> {
    pub(crate) async fn run<T>(
        &mut self,
        operation: impl Future<Output = Result<T, AiError>>,
    ) -> Result<T, AiError> {
        let cancel = self
            .cancel
            .take()
            .ok_or_else(|| AiError::config("Request already started"))?;
        tokio::select! {
            biased;
            _ = cancel => Err(AiError::new("cancelled", "Request cancelled")),
            result = operation => result,
        }
    }
}

impl Drop for HeadlessRequest<'_> {
    fn drop(&mut self) {
        let _ = self.requests.release_owned(&self.id, &self.owner);
    }
}

impl Requests {
    pub(crate) fn claim_headless(
        &self,
        id: &str,
        session: Option<&str>,
    ) -> Result<HeadlessRequest<'_>, AiError> {
        let Some(RequestClaim {
            cancel_rx,
            identity,
        }) = self.claim(id).map_err(AiError::config)?
        else {
            return Err(AiError::new("busy", "Another AI request is active"));
        };
        if let Some(session) = session {
            self.set_session_owned(id, &identity, session);
        }
        Ok(HeadlessRequest {
            requests: self,
            id: id.into(),
            owner: identity,
            cancel: Some(cancel_rx),
        })
    }

    /// Claims the one slot. `Ok(None)` means another request is already running,
    /// so the caller must refuse with `busy` and claim nothing.
    fn claim(&self, request_id: &str) -> Result<Option<RequestClaim>, String> {
        let mut pending = self.0.lock().map_err(|_| "Request lock failed")?;
        if !pending.is_empty() {
            return Ok(None);
        }
        let (tx, rx) = oneshot::channel();
        let identity = Arc::new(());
        pending.insert(
            request_id.to_owned(),
            ActiveRequest {
                cancel: tx,
                session_id: None,
                identity: identity.clone(),
            },
        );
        Ok(Some(RequestClaim {
            cancel_rx: rx,
            identity,
        }))
    }
    fn release_owned(
        &self,
        id: &str,
        owner: &Arc<()>,
    ) -> Result<Option<oneshot::Sender<()>>, String> {
        let mut pending = self.0.lock().map_err(|_| "Request lock failed")?;
        if pending
            .get(id)
            .is_some_and(|request| Arc::ptr_eq(&request.identity, owner))
        {
            Ok(pending.remove(id).map(|request| request.cancel))
        } else {
            Ok(None)
        }
    }
    fn set_session_owned(&self, id: &str, owner: &Arc<()>, session: &str) {
        if let Ok(mut pending) = self.0.lock()
            && let Some(request) = pending.get_mut(id)
            && Arc::ptr_eq(&request.identity, owner)
        {
            request.session_id = Some(session.into());
        }
    }

    /// Names the conversation a running request saves into. A request that has
    /// already ended is not resurrected.
    #[cfg(test)]
    fn set_session(&self, request_id: &str, session_id: &str) {
        if let Ok(mut pending) = self.0.lock()
            && let Some(request) = pending.get_mut(request_id)
        {
            request.session_id = Some(session_id.to_owned());
        }
    }

    /// The request in flight; the slot holds at most one.
    pub(crate) fn active(&self) -> Result<Option<ActiveTurn>, String> {
        let pending = self.0.lock().map_err(|_| "Request lock failed")?;
        Ok(pending.iter().next().map(|(id, request)| ActiveTurn {
            request_id: id.clone(),
            session_id: request.session_id.clone(),
        }))
    }

    /// Ends a request, returning its cancel handle so the caller can send it.
    fn release(&self, request_id: &str) -> Result<Option<oneshot::Sender<()>>, String> {
        let mut pending = self.0.lock().map_err(|_| "Request lock failed")?;
        Ok(pending.remove(request_id).map(|request| request.cancel))
    }
}

async fn cancellable_request<T>(
    requests: &Requests,
    id: &str,
    cancel: oneshot::Receiver<()>,
    owner: Arc<()>,
    request: impl Future<Output = Result<T, AiError>>,
) -> Result<T, AiError> {
    let result = tokio::select! {
        biased;
        _ = cancel => Err(AiError::new("cancelled", "Request cancelled")),
        result = request => result,
    };
    if result.is_err() {
        let _ = requests.release_owned(id, &owner);
    }
    result
}

/// The turn the backend is running, or `null` when it is running none. A window
/// asks after a reload, because a marker that outlived its turn must not paint
/// a turn as running.
#[tauri::command]
pub fn ai_active_turn(requests: State<'_, Requests>) -> Result<Option<ActiveTurn>, String> {
    requests.active()
}

/// Serializable error for the UI. `auth_expired` is distinct from `auth` so the
/// panel can offer a sign-in action instead of a generic failure. `retryable`
/// is the provider layer's verdict on resending the same turn: true for a
/// reply the loop could not use, false for a bad key, endpoint or request.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiError {
    pub kind: String,
    pub message: String,
    pub reauth: bool,
    pub retryable: bool,
}

impl AiError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            reauth: false,
            retryable: false,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new("config", message)
    }
}

impl From<ProviderError> for AiError {
    fn from(error: ProviderError) -> Self {
        Self {
            kind: error.kind.as_str().into(),
            message: error.message,
            reauth: error.kind == ProviderErrorKind::AuthExpired,
            retryable: error.retryable,
        }
    }
}

impl std::fmt::Display for AiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// One entry the catalog cannot describe. Informational only: the catalog is a
/// metadata table, so an unknown id never blocks a save or a request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderWarning {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub message: String,
}

/// Structural rules for the whole config. Unknown model ids are not errors: they
/// come back as warnings, because an offline catalog cannot know a provider's
/// current ids.
pub fn validate_config(config: &AiProviderConfig) -> Result<Vec<ProviderWarning>, String> {
    if config.providers.is_empty() || config.providers.len() > 32 {
        return Err("Expected 1–32 providers".into());
    }
    let mut ids = std::collections::HashSet::new();
    for p in &config.providers {
        if !ids.insert(&p.id) {
            return Err(format!(
                "Provider id \"{}\" is saved twice. Two providers cannot share an id, because the id keys the saved key.",
                p.id
            ));
        }
        validate_entry(config, p)?;
    }
    if !ids.contains(&config.active_provider) {
        return Err("Active provider not found".into());
    }
    Ok(warnings(config))
}

/// Structural rules for one entry: everything about it that makes it unusable,
/// never whether the catalog happens to know its model id.
fn validate_entry(config: &AiProviderConfig, p: &AiProviderEntry) -> Result<(), String> {
    if p.id.is_empty()
        || p.id.len() > 100
        || p.model.len() > 200
        || p.max_tokens == 0
        || p.max_tokens > 262_144
        || !p.temperature.is_finite()
        || !(0.0..=2.0).contains(&p.temperature)
        || p.wire() == crate::commands::Wire::Unknown
        || p.auth == AuthKind::Unknown
    {
        return Err(format!(
            "Invalid provider settings for \"{}\": check the provider id, model id, output limit, temperature and protocol.",
            p.id
        ));
    }
    if p.auth == AuthKind::OAuth
        && p.auth_provider
            .is_none_or(|provider| provider == AuthProvider::Unknown)
    {
        return Err("OAuth requires an auth provider".into());
    }
    if p.endpoint.is_empty() && p.model.is_empty() && p.id != config.active_provider {
        return Ok(()); // Untouched Custom Endpoint placeholder is not a configured provider.
    }
    if p.model.trim().is_empty() {
        return Err("Model ID is required".into());
    }
    if p.models.len() > 32
        || p.models
            .iter()
            .any(|model| model.trim().is_empty() || model.chars().count() > 200)
    {
        return Err("Invalid provider model list".into());
    }
    endpoint(&p.endpoint)?;
    auth::validate_oauth_entry(p)?;
    let allowed = ai::catalog::allowed_thinking_levels(p);
    if !allowed.iter().any(|level| level == &p.thinking_level) {
        return Err(format!(
            "Thinking level \"{}\" is not supported by {}; choose one of {}.",
            p.thinking_level,
            p.model,
            allowed.join(", ")
        ));
    }
    Ok(())
}

fn warnings(config: &AiProviderConfig) -> Vec<ProviderWarning> {
    config
        .providers
        .iter()
        .filter(|p| !ai::catalog::entry_model_described(p))
        .map(|p| ProviderWarning {
            provider_id: p.id.clone(),
            provider_name: p.name.clone(),
            model: p.model.clone(),
            message: ai::catalog::model_warning(p),
        })
        .collect()
}

/// The entry a request is about to use, validated on its own: a structural
/// problem anywhere else in the file is a settings problem, not a chat problem.
pub(crate) fn provider(id: &str) -> Result<AiProviderEntry, String> {
    let config = load_ai_config()?;
    let entry = config
        .providers
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| "Save provider settings first".to_string())?;
    validate_entry(&config, &entry)?;
    Ok(entry)
}
fn key_store() -> KeyringSecrets {
    KeyringSecrets::new(auth::API_KEY_SERVICE)
}

/// What the OS store said about one account. Kept for the life of the process,
/// so a dismissed prompt is never asked again inside a turn.
#[derive(Clone)]
enum KeyVerdict {
    Ready(Secret),
    Absent,
    Refused(String),
}

impl KeyVerdict {
    fn result(self) -> Result<Option<Secret>, String> {
        match self {
            Self::Ready(secret) => Ok(Some(secret)),
            Self::Absent => Ok(None),
            Self::Refused(message) => Err(message),
        }
    }
}

#[derive(Default)]
struct KeyCache {
    verdicts: std::collections::HashMap<String, KeyVerdict>,
    /// One reader per account: two callers must not raise two OS prompts.
    readers: std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
}

/// One keychain read per process per account. macOS asks per binary and per
/// access, and a development build is a new binary on every build, so reading
/// lazily inside a turn put a modal dialog in the middle of a send. The verdict
/// is cached instead; only `ai_warm_key` and `ai_read_key` ask the OS again.
/// `OPENCOMPOSE_AI_KEY` is the escape hatch for headless runs, which start a new
/// process per session and must not touch the keychain at all.
fn key_cache() -> &'static std::sync::Mutex<KeyCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<KeyCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(KeyCache::default()))
}

fn cached_verdict(account: &str) -> Option<KeyVerdict> {
    key_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .verdicts
        .get(account)
        .cloned()
}

fn remember_verdict(account: &str, verdict: KeyVerdict) {
    key_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .verdicts
        .insert(account.to_owned(), verdict);
}

fn account_reader(account: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    key_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .readers
        .entry(account.to_owned())
        .or_default()
        .clone()
}

/// A key given in the environment wins over the keychain: it is an explicit
/// instruction from the operator, and it is the only way a headless session can
/// avoid a dialog per launch.
fn environment_key() -> Option<Secret> {
    environment_key_from(std::env::var("OPENCOMPOSE_AI_KEY").ok().as_deref())
}

fn environment_key_from(value: Option<&str>) -> Option<Secret> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Secret::new)
}

/// Reads one account's key through `store`. Without `force` a cached verdict —
/// including a refusal — answers instead of the OS, so the request path never
/// opens a dialog.
fn read_key_with(
    store: &dyn credential_store::SecretStore,
    p: &AiProviderEntry,
    force: bool,
) -> Result<Option<Secret>, String> {
    let account = auth::api_key_account(p);
    let reader = account_reader(&account);
    let _guard = reader.lock().unwrap_or_else(|error| error.into_inner());
    if !force && let Some(verdict) = cached_verdict(&account) {
        return verdict.result();
    }
    let read = credential_store::load_text(store, &account).map_err(|error| {
        auth::read_message(
            error,
            "Cannot unlock/read OS credential store. No plaintext fallback is used.",
        )
    });
    let verdict = match &read {
        Ok(Some(secret)) => KeyVerdict::Ready(secret.clone()),
        Ok(None) => KeyVerdict::Absent,
        Err(message) => KeyVerdict::Refused(message.clone()),
    };
    remember_verdict(&account, verdict);
    let stored = read?;
    if stored.is_some() {
        return Ok(stored);
    }
    Ok(catalog_env_key(p))
}

type KeyReadResult = Result<Option<Secret>, String>;
type KeyReadReceiver = tokio::sync::watch::Receiver<Option<KeyReadResult>>;
#[derive(Default, Clone)]
struct PendingKeyReads(std::sync::Arc<Mutex<HashMap<String, KeyReadReceiver>>>);

impl PendingKeyReads {
    fn start(
        &self,
        account: String,
        read: impl FnOnce() -> KeyReadResult + Send + 'static,
    ) -> KeyReadReceiver {
        let mut pending = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(receiver) = pending.get(&account) {
            return receiver.clone();
        }
        let (sender, receiver) = tokio::sync::watch::channel(None);
        pending.insert(account.clone(), receiver.clone());
        let jobs = self.clone();
        std::thread::spawn(move || {
            let result = read();
            sender.send_replace(Some(result));
            jobs.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&account);
        });
        receiver
    }
}

fn pending_key_reads() -> &'static PendingKeyReads {
    static READS: std::sync::OnceLock<PendingKeyReads> = std::sync::OnceLock::new();
    READS.get_or_init(PendingKeyReads::default)
}
fn start_key_read(p: AiProviderEntry, force: bool) -> KeyReadReceiver {
    pending_key_reads().start(auth::api_key_account(&p), move || {
        if force {
            read_key_with(&key_store(), &p, true)
        } else {
            read_key(&p)
        }
    })
}
async fn wait_for_key(
    mut receiver: KeyReadReceiver,
    timeout: std::time::Duration,
) -> Result<Option<Secret>, AiError> {
    let result = tokio::time::timeout(timeout, async {
        loop {
            if let Some(result) = receiver.borrow().clone() { return result; }
            if receiver.changed().await.is_err() { return Err("Credential task ended without a result".into()); }
        }
    }).await.map_err(|_| AiError::new("credential_pending", "Timed out waiting for the OS credential store. On macOS, bring OpenCompose forward and respond to the Keychain access prompt, then retry. No request was sent to the provider. The OS prompt may still be waiting; cancelling this request does not dismiss it."))?;
    result.map_err(|message| AiError::from(ProviderError::credential_store(message)))
}
const KEY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The catalog's own environment variable, used only when no key is stored.
fn catalog_env_key(p: &AiProviderEntry) -> Option<Secret> {
    ai::catalog::env_key(p)
        .and_then(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(Secret::new)
}

/// The request path: a cached verdict, or the first read in this process.
fn read_key(p: &AiProviderEntry) -> Result<Option<Secret>, String> {
    if let Some(secret) = environment_key() {
        return Ok(Some(secret));
    }
    read_key_with(&key_store(), p, false)
}
#[tauri::command]
pub async fn ai_key_status(provider_id: String) -> Result<bool, String> {
    wait_for_key(start_key_read(provider(&provider_id)?, false), KEY_WAIT)
        .await
        .map(|key| key.is_some())
        .map_err(|error| error.message)
}
/// Reads the key from the OS store again, prompting if it has to. This is the
/// Settings affordance behind "Read key again": the cached refusal is dropped
/// first, so a fixed permission or a new key is picked up.
#[tauri::command]
pub async fn ai_read_key(provider_id: String) -> Result<bool, String> {
    wait_for_key(start_key_read(provider(&provider_id)?, true), KEY_WAIT)
        .await
        .map(|key| key.is_some())
        .map_err(|error| error.message)
}
/// Unlocks the key now, off the turn path. The panel calls this at startup and
/// whenever the active provider changes, so macOS asks its question before the
/// operator asks for a reply.
#[tauri::command]
pub fn ai_warm_key(provider_id: String) -> Result<(), String> {
    let p = provider(&provider_id)?;
    warm_api_key(&p);
    Ok(())
}
#[tauri::command]
pub async fn ai_set_key(provider_id: String, mut key: String) -> Result<(), String> {
    let valid = !key.trim().is_empty() && key.len() <= 8192 && !key.contains(['\r', '\n', '\0']);
    if !valid {
        key.zeroize();
        return Err("Invalid API key".into());
    }
    let secret = Secret::new(key.trim());
    key.zeroize();
    tauri::async_runtime::spawn_blocking(move || {
        let p = provider(&provider_id)?;
        let account = auth::api_key_account(&p);
        credential_store::store_text(&key_store(), &account, secret.as_str()).map_err(|error| {
            auth::store_message(
                error,
                "Could not save to OS credential store; key was not saved to disk",
            )
        })?;
        remember_verdict(&account, KeyVerdict::Ready(secret.clone()));
        Ok(())
    })
    .await
    .map_err(|_| "Credential task failed")?
}
#[tauri::command]
pub async fn ai_remove_key(provider_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = provider(&provider_id)?;
        let account = auth::api_key_account(&p);
        credential_store::delete_entry(&key_store(), &account)
            .map_err(|error| auth::store_message(error, "Could not remove OS credential"))?;
        remember_verdict(&account, KeyVerdict::Absent);
        Ok(())
    })
    .await
    .map_err(|_| "Credential task failed")?
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthStatus {
    pub state: String,
    pub account: Option<String>,
    pub expires_at: Option<i64>,
    pub reauth_required: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthAttemptStatus {
    pub state: String,
    pub message: String,
    /// The flow's structured refusal, for the panel that words each kind itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<oauth::SignInError>,
}

#[tauri::command]
pub async fn ai_oauth_status(provider_id: String) -> Result<OAuthStatus, String> {
    let p = provider(&provider_id)?;
    if p.auth != AuthKind::OAuth {
        return Ok(OAuthStatus {
            state: "missing".into(),
            account: None,
            expires_at: None,
            reauth_required: false,
            message: "This provider uses an API key.".into(),
        });
    }
    let read = p.clone();
    let token = tauri::async_runtime::spawn_blocking(move || auth::read_oauth_token(&read))
        .await
        .map_err(|_| "Credential task failed")??;
    Ok(match token {
        None => OAuthStatus {
            state: "needs_reauth".into(),
            account: None,
            expires_at: None,
            reauth_required: true,
            message: "Not signed in. Sign in to connect.".into(),
        },
        Some(tokens) => {
            let expired = tokens.expired(oauth::now_ms());
            OAuthStatus {
                state: if expired { "expired" } else { "connected" }.into(),
                account: tokens.account_label.clone().or(tokens.account_id.clone()),
                expires_at: (tokens.expires_at != 0).then_some(tokens.expires_at),
                reauth_required: false,
                message: if expired {
                    "Access token expired; it refreshes on the next request.".into()
                } else {
                    "Connected.".into()
                },
            }
        }
    })
}

#[tauri::command]
pub async fn ai_oauth_begin(
    provider_id: String,
    attempts: State<'_, oauth::Attempts>,
) -> Result<oauth::OAuthStart, String> {
    let p = provider(&provider_id)?;
    if p.auth != AuthKind::OAuth {
        return Err("Choose OAuth authentication for this provider first.".into());
    }
    auth::validate_oauth_entry(&p)?;
    let mut start = oauth::begin(p, &attempts).await?;
    start.opened = oauth::open_browser(&start.url);
    if !start.opened {
        log::warn!("[ai] could not open the browser for OAuth sign-in");
    }
    Ok(start)
}

#[tauri::command]
pub fn ai_oauth_poll(
    attempt_id: String,
    attempts: State<oauth::Attempts>,
) -> Result<OAuthAttemptStatus, String> {
    if attempt_id.is_empty() || attempt_id.len() > 100 {
        return Err("Invalid sign-in attempt".into());
    }
    attempts
        .status(&attempt_id)
        .map(|status| OAuthAttemptStatus {
            state: status.state,
            message: status.message,
            error: status.error,
        })
        .ok_or_else(|| "Unknown sign-in attempt".into())
}

#[tauri::command]
pub fn ai_oauth_cancel(attempt_id: String, attempts: State<oauth::Attempts>) -> Result<(), String> {
    attempts.cancel(&attempt_id);
    Ok(())
}

#[tauri::command]
pub async fn ai_oauth_sign_out(provider_id: String) -> Result<(), String> {
    let p = provider(&provider_id)?;
    tauri::async_runtime::spawn_blocking(move || auth::delete_oauth_token(&p))
        .await
        .map_err(|_| "Credential task failed")?
}

#[tauri::command]
pub fn ai_catalog() -> ai::catalog::Catalog {
    ai::catalog::merged()
}

/// Tool catalog advertised to the built-in panel: enabled DAW tools plus the
/// panel-local tool whose handler lives in the frontend.
#[tauri::command]
pub fn ai_tools(toolsets: Option<Vec<String>>) -> Value {
    Value::Array(crate::tool_registry::panel_tools_for(toolsets.as_deref()))
}

/// The switchboard: bundled sets plus the user file's valid additions.
#[tauri::command]
pub fn ai_toolsets() -> Vec<ai::toolsets::Toolset> {
    ai::toolsets::effective()
}

/// The body-less skill index for the system prompt: the same list `list_skills`
/// returns, so the panel and a headless client read one source.
#[tauri::command]
pub fn ai_skills() -> Value {
    ai::skills::list_value()
}

/// Reads an audio or image file the user picked: validates it, hashes it and
/// reads the duration when it is audio. Only the descriptor travels with the
/// conversation.
#[tauri::command]
pub async fn ai_attach_media(path: String) -> Result<ai::media::AttachedMedia, String> {
    tauri::async_runtime::spawn_blocking(move || ai::media::attach(&path))
        .await
        .map_err(|_| "Media task failed".to_string())?
}

/// Run one panel tool call through the same dispatcher the MCP surface uses.
/// Rendering and analysis can block, so it runs off the UI thread.
///
/// A refusal is the payload `/call` answers with, so the model reads the same
/// `skill` id on this surface instead of a bare string. `conversation` names the
/// panel turn's batch owner, so a `/call` client cannot join or close it.
#[tauri::command]
pub async fn ai_call_tool(
    app: tauri::AppHandle,
    tool: String,
    args: Value,
    toolsets: Option<Vec<String>>,
    conversation: Option<String>,
    cancellation_id: Option<String>,
    jobs: State<'_, SeparatorJobs>,
) -> Result<Value, Value> {
    let caller = crate::state::panel_batch_owner(conversation.as_deref());
    let refusal = crate::ai::skill_hints::refusal;
    let checked = crate::tool_registry::panel_tool_in(&tool, toolsets.as_deref())
        .and_then(|def| crate::tool_registry::validate_args(&def, &args));
    if let Err(message) = checked {
        // A refusal before dispatch changed nothing, so the turn's batch stays.
        return Err(refusal(&message));
    }
    let job = if tool == "separate_stems" {
        cancellation_id
            .map(|id| jobs.begin(id))
            .transpose()
            .map_err(|message| refusal(&message))?
    } else {
        None
    };
    tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        if let Some(job) = job {
            return crate::agent_api::separate_stems_cancellable(&args, Some(&job.cancel));
        }
        let mut result = crate::agent_api::dispatch_panel(&app, &tool, &args, &caller)?;
        if tool == "transaction_rollback"
            && let Err(error) = crate::commands::record_panel_rollback(&app)
        {
            // The rollback already succeeded: never report a mutation failure.
            result["session_warning"] = serde_json::json!(format!(
                "Project rolled back, but session outcome could not be saved: {error}"
            ));
        }
        Ok(result)
    })
    .await
    .map_err(|_| refusal("tool task failed"))?
    .map_err(|message| refusal(&message))
}

#[cfg(test)]
pub fn tools() -> Value {
    ai::legacy_tools()
}

/// Warms one provider's credential off the request path.
///
/// macOS asks the operator to allow a binary to read its own keychain item, and
/// a development build is a new binary every time. Reading lazily inside the
/// first turn puts that dialog in the middle of a send, where it looks like a
/// hung request; this warms the cache when the panel loads or switches provider,
/// so the question arrives before the user asks for anything. A cached verdict
/// answers without touching the OS, and a refusal is cached like any other.
pub fn warm_api_key(p: &AiProviderEntry) {
    if p.auth != AuthKind::ApiKey {
        return;
    }
    let _ = start_key_read(p.clone(), false);
}

/// Resolves the credential for a request. OAuth tokens are refreshed lazily
/// here, inside the five-minute skew, before any request is sent.
pub(crate) async fn resolve_credential(p: &AiProviderEntry) -> Result<Credential, AiError> {
    match p.auth {
        AuthKind::OAuth => {
            let tokens = oauth::COORDINATOR
                .ensure(&KeyringStore, p, false)
                .await
                .map_err(AiError::from)?;
            Ok(Credential::OAuth(tokens))
        }
        _ => {
            let key = wait_for_key(start_key_read(p.clone(), false), KEY_WAIT).await?;
            Ok(Credential::ApiKey(key))
        }
    }
}

async fn force_refresh(p: &AiProviderEntry) -> Result<Credential, AiError> {
    oauth::COORDINATOR
        .ensure(&KeyringStore, p, true)
        .await
        .map(Credential::OAuth)
        .map_err(AiError::from)
}

struct TurnRequest<'a> {
    messages: Value,
    use_tools: bool,
    toolsets: Option<&'a [String]>,
}

#[derive(Default)]
struct TurnChannels {
    delta: Option<tauri::ipc::Channel<String>>,
    thinking: Option<tauri::ipc::Channel<String>>,
    preparing: Option<tauri::ipc::Channel<String>>,
}

async fn send(
    p: AiProviderEntry,
    request: TurnRequest<'_>,
    channels: TurnChannels,
    stats: &ai::timing::TurnStatsHandle,
) -> Result<Value, AiError> {
    endpoint(&p.endpoint).map_err(AiError::config)?;
    let credential = resolve_credential(&p).await?;
    send_http_with_thinking(
        p,
        request,
        channels,
        credential,
        |p| async move { force_refresh(&p).await },
        stats,
    )
    .await
}

/// One request; on a 401 with an OAuth credential and no text already streamed,
/// refresh once and retry once. A second 401 is terminal.
#[cfg(test)]
async fn send_http<F, Fut>(
    p: AiProviderEntry,
    request: TurnRequest<'_>,
    channels: TurnChannels,
    credential: Credential,
    refresh: F,
    stats: &ai::timing::TurnStatsHandle,
) -> Result<Value, AiError>
where
    F: Fn(AiProviderEntry) -> Fut,
    Fut: Future<Output = Result<Credential, AiError>>,
{
    send_http_with_thinking(p, request, channels, credential, refresh, stats).await
}

async fn send_http_with_thinking<F, Fut>(
    p: AiProviderEntry,
    request: TurnRequest<'_>,
    channels: TurnChannels,
    credential: Credential,
    refresh: F,
    stats: &ai::timing::TurnStatsHandle,
) -> Result<Value, AiError>
where
    F: Fn(AiProviderEntry) -> Fut,
    Fut: Future<Output = Result<Credential, AiError>>,
{
    let TurnRequest {
        mut messages,
        use_tools,
        toolsets,
    } = request;
    // History the model cannot take degrades to a text line; only the turn's own
    // attachment can refuse the request below.
    let lowered = ai::limits::lower_unaccepted_media(&p, &mut messages);
    if lowered > 0 {
        log::info!(
            "[ai] {}: lowered {lowered} historical attachment(s) the model does not accept",
            p.model
        );
    }
    ai::limits::gate_input(&p, &messages).map_err(AiError::from)?;
    let (request, notices) =
        ai::request_from_wire(&p, messages, use_tools, toolsets).map_err(AiError::from)?;
    let is_oauth = matches!(credential, Credential::OAuth(_));
    let result = match run_once(&p, &request, &channels, credential, stats).await {
        Ok(value) => Ok(value),
        Err((error, emitted)) if is_oauth && error.kind == ProviderErrorKind::Auth && !emitted => {
            let fresh = refresh(p.clone()).await?;
            stats.retry();
            match run_once(&p, &request, &channels, fresh, stats).await {
                Ok(value) => Ok(value),
                Err((error, _)) if error.kind == ProviderErrorKind::Auth => {
                    Err(AiError::from(auth::auth_expired(&p)))
                }
                Err((error, _)) => Err(AiError::from(error)),
            }
        }
        Err((error, true)) if is_oauth && error.kind == ProviderErrorKind::Auth => {
            Err(AiError::from(auth::auth_expired(&p)))
        }
        Err((error, _)) => Err(AiError::from(error)),
    };
    result.map(|mut value| {
        if !notices.is_empty()
            && let Some(object) = value.as_object_mut()
            && let Ok(notices) = serde_json::to_value(notices)
        {
            object.insert("media_notices".into(), notices);
        }
        value
    })
}

async fn run_once(
    p: &AiProviderEntry,
    request: &ProviderRequest,
    channels: &TurnChannels,
    credential: Credential,
    stats: &ai::timing::TurnStatsHandle,
) -> Result<Value, (ProviderError, bool)> {
    if stats.attempts().len() >= 4 {
        return Err((
            ProviderError::new(ProviderErrorKind::Config, "Provider attempt limit exceeded"),
            false,
        ));
    }
    if let Some(update) = ai::session_usage::checkpoint(stats, false).await
        && !update.saved
    {
        return Err((
            ProviderError::new(
                ProviderErrorKind::Config,
                update
                    .error
                    .map(|error| format!("{}: {}", error.code, error.message))
                    .unwrap_or_else(|| "Session accounting is unavailable".into()),
            ),
            false,
        ));
    }
    let price = ai::prices::resolve(&p.id, p.wire(), &p.model);
    ai::session_usage::prepare_attempt(stats, &p.id, &p.model, price)
        .await
        .map_err(|error| {
            (
                ProviderError::new(
                    ProviderErrorKind::Config,
                    format!("{}: {}", error.code, error.message),
                ),
                false,
            )
        })?;
    let provider = ai::provider_for(p, credential).map_err(|error| (error, false))?;
    let mut emitted = false;
    let result = ai::run_with_activity(
        provider.as_ref(),
        request,
        &channels.delta,
        &channels.thinking,
        &channels.preparing,
        &mut emitted,
        stats,
    )
    .await
    .map_err(|error| (error, emitted));
    // Accounting failure never changes a paid response into a model retry.
    let _ = ai::session_usage::checkpoint(stats, false).await;
    result
}

/// The text budget excludes validated listening payloads, which have their own
/// bounded media budget. Audio remains structured media, never prompt text.
pub(crate) fn validate_chat_messages(messages: &Value) -> Result<(), AiError> {
    let rows = messages
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= MAX_WIRE_MESSAGES)
        .ok_or_else(|| AiError::config("Invalid or oversized AI request"))?;
    let mut audio_bytes = 0usize;
    for row in rows {
        if let Some(audio) = row.get("input_audio") {
            if row["role"] != "user" {
                return Err(AiError::config(
                    "Inline audio must belong to a user message",
                ));
            }
            let size = audio["data"].as_str().map_or(0, str::len);
            audio_bytes = audio_bytes.saturating_add(size);
            if audio_bytes > ai::media::MAX_REQUEST_INLINE_BYTES {
                return Err(AiError::config(
                    "Listening clips exceed the inline audio budget; request shorter or fewer ranges.",
                ));
            }
            // Also checks format, base64 and the decoded per-file bound.
            ai::media::inline_audio(audio).map_err(AiError::from)?;
        }
    }
    if messages.to_string().len().saturating_sub(audio_bytes) > LIMIT {
        return Err(AiError::config(
            "AI request text is too large; shorten or compact the conversation.",
        ));
    }
    Ok(())
}

/// Give a conversation a short title from its first exchange.
///
/// The store opens a session with the first user message as its title, which
/// reads like a prompt in the list. One small request replaces it; a provider
/// that cannot answer leaves the title alone, so naming never fails a turn.
#[tauri::command]
pub async fn ai_name_session(
    app: tauri::AppHandle,
    id: String,
    provider_id: String,
) -> Result<Value, String> {
    let loaded = crate::ai::sessions::Store::config().load(&id)?;
    let text = |value: &Value| value["content"].as_str().unwrap_or_default().to_string();
    let first_user = loaded
        .messages
        .iter()
        .find(|message| message["role"] == "user" && !text(message).trim().is_empty())
        .map(text);
    let first_reply = loaded
        .messages
        .iter()
        .find(|message| message["role"] == "assistant" && !text(message).trim().is_empty())
        .map(text);
    let (Some(question), Some(answer)) = (first_user, first_reply) else {
        return Err("This conversation has no exchange to name yet".into());
    };
    let p = provider(&provider_id)?;
    let stats = ai::timing::TurnStatsHandle::new(&p.id, &p.model, p.wire());
    let title_request = ai::session_usage::title_id()
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    let lease = ai::session_usage::SessionLease::admit(
        ai::sessions::Store::config(),
        &id,
        &title_request,
        ai::session_usage::Purpose::Title,
    )
    .map_err(|error| format!("{}: {}", error.code, error.message))?;
    stats.set_accounting(lease);
    let messages = json!([
        {
            "role": "system",
            "content": "Name this conversation in 2 to 5 words. Answer with the name only, without quotes or a trailing period."
        },
        {
            "role": "user",
            "content": format!(
                "User: {}\n\nAssistant: {}",
                question.chars().take(600).collect::<String>(),
                answer.chars().take(600).collect::<String>()
            )
        }
    ]);
    let outcome = send(
        p,
        TurnRequest {
            messages,
            use_tools: false,
            toolsets: None,
        },
        TurnChannels {
            delta: None,
            thinking: None,
            preparing: None,
        },
        &stats,
    )
    .await;
    let accounting = ai::session_usage::checkpoint_final(
        &stats,
        &app,
        &title_request,
        ai::session_usage::Purpose::Title,
    )
    .await;
    if stats.attempted() || accounting.is_some() {
        let _ = app.emit(
            "ai_stats",
            ai::session_usage::StatsEvent {
                turn: stats.snapshot(),
                client_request_id: title_request,
                purpose: ai::session_usage::Purpose::Title,
                accounting,
            },
        );
    }
    let reply = outcome.map_err(|error| {
        log::warn!("[ai] naming '{id}' failed: {}", error.message);
        error.to_string()
    })?;
    // A reasoning model may put the whole answer in `reasoning` and leave
    // `content` empty; the name is still there, just in the other field.
    let text = reply["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| reply["reasoning"].as_str())
        .unwrap_or_default();
    let title = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '“' || c == '”' || c == '.' || c.is_whitespace()
        })
        .to_string();
    if title.is_empty() {
        log::warn!("[ai] naming '{id}' returned no text");
        return Err("The provider returned no title".into());
    }
    if let Some(lease) = stats.accounting() {
        lease.verify_session().map_err(|error| error.message)?;
    }
    let incarnation = stats.accounting().map(|lease| lease.update().incarnation);
    let meta =
        crate::ai::sessions::Store::config().rename_owned(&id, &title, incarnation.as_deref())?;
    Ok(json!({ "title": meta.title }))
}

#[tauri::command]
#[expect(
    clippy::too_many_arguments,
    reason = "The command preserves the flat IPC request fields and injected Tauri context"
)]
pub async fn ai_chat(
    app: tauri::AppHandle,
    provider_id: String,
    request_id: String,
    session_id: Option<String>,
    messages: Value,
    use_tools: bool,
    toolsets: Option<Vec<String>>,
    on_delta: tauri::ipc::Channel<String>,
    on_thinking: Option<tauri::ipc::JavaScriptChannelId>,
    on_preparing: Option<tauri::ipc::JavaScriptChannelId>,
    webview: tauri::Webview,
    requests: State<'_, Requests>,
) -> Result<Value, AiError> {
    if request_id.is_empty() || request_id.len() > 100 {
        return Err(AiError::config("Invalid AI request identifier"));
    }
    validate_chat_messages(&messages)?;
    let on_thinking = on_thinking.map(|id| id.channel_on(webview.clone()));
    let on_preparing = on_preparing.map(|id| id.channel_on(webview));
    let p = provider(&provider_id).map_err(AiError::config)?;
    let stats = ai::timing::TurnStatsHandle::new(&p.id, &p.model, p.wire());
    let Some(RequestClaim {
        cancel_rx: rx,
        identity: owner,
    }) = requests.claim(&request_id).map_err(AiError::config)?
    else {
        return Err(AiError::new("busy", "Another AI request is active"));
    };
    if let Some(id) = session_id.as_deref() {
        match ai::session_usage::SessionLease::admit(
            ai::sessions::Store::config(),
            id,
            &request_id,
            ai::session_usage::Purpose::Chat,
        ) {
            Ok(lease) => {
                stats.set_accounting(lease);
                requests.set_session_owned(&request_id, &owner, id);
            }
            Err(error) => {
                let _ = requests.release_owned(&request_id, &owner);
                return Err(AiError::new(error.code.as_str(), error.message));
            }
        }
    }
    // A legacy caller that omits sessionId is explicitly ephemeral. Never
    // recapture whichever conversation happened to become active meanwhile.
    let preparation = match session_id.as_ref() {
        Some(session_id) => tauri::async_runtime::spawn_blocking({
            let app = app.clone();
            let p = p.clone();
            let messages = messages.clone();
            let session_id = session_id.clone();
            let incarnation = stats
                .accounting()
                .expect("saved requests have an admitted accounting owner")
                .update()
                .incarnation;
            move || {
                crate::commands::prepare_turn_for(&app, &p, &messages, &session_id, &incarnation)
            }
        })
        .await
        .map_err(|error| format!("Session persistence task failed: {error}"))
        .and_then(|result| result),
        None => Ok(None),
    };
    if session_id.is_some() && preparation.is_err() {
        let error = preparation.err().expect("checked preparation failure");
        let accounting = ai::session_usage::checkpoint_final(
            &stats,
            &app,
            &request_id,
            ai::session_usage::Purpose::Chat,
        )
        .await;
        if accounting.is_some() {
            let _ = app.emit(
                "ai_stats",
                ai::session_usage::StatsEvent {
                    turn: stats.snapshot(),
                    client_request_id: request_id.clone(),
                    purpose: ai::session_usage::Purpose::Chat,
                    accounting,
                },
            );
        }
        let _ = requests.release_owned(&request_id, &owner);
        return Err(AiError::new("session_save", error));
    }
    let prepared = saved_audio_or_fallback(
        preparation,
        messages.as_array().map(Vec::as_slice).unwrap_or_default(),
        &requests,
        &request_id,
    )?;
    if let Some(turn) = prepared.as_ref()
        && let Some(compaction) = turn.compacted.as_ref()
    {
        let _ = app.emit(
            crate::commands::AUTO_SESSION_EVENT,
            crate::commands::compaction_event(turn, compaction, &p),
        );
    }
    // A turn that made room in the audition archive says what it removed: the
    // clips and renders are files the operator could otherwise miss.
    if let Some(turn) = prepared.as_ref()
        && !turn.archive_evicted.is_empty()
    {
        let _ = app.emit(
            crate::commands::AUTO_SESSION_EVENT,
            crate::commands::archive_event(turn),
        );
    }
    // The turn now names the conversation its reply will be saved into, so a
    // window that reloads can ask whether this turn is still running.
    if let Some(turn) = prepared.as_ref() {
        requests.set_session_owned(&request_id, &owner, &turn.session_id);
        if !turn.saved_auditions.is_empty() {
            let _ = app.emit(
                crate::commands::AUTO_SESSION_EVENT,
                json!({
                    "request_id": request_id,
                    "session_id": turn.session_id,
                    "saved_auditions": turn.saved_auditions,
                }),
            );
        }
    }
    let outbound = match &prepared {
        Some(turn) => turn.messages.clone(),
        None => messages,
    };
    let session_provider = p.clone();
    let mut result = cancellable_request(
        &requests,
        &request_id,
        rx,
        owner.clone(),
        send(
            p,
            TurnRequest {
                messages: outbound,
                use_tools,
                toolsets: toolsets.as_deref(),
            },
            TurnChannels {
                delta: Some(on_delta),
                thinking: on_thinking,
                preparing: on_preparing,
            },
            &stats,
        ),
    )
    .await;
    let admission_error = stats.accounting().and_then(|lease| lease.update().error);
    if result.as_ref().is_err_and(|error| error.kind == "config")
        && let Some(error) = admission_error
    {
        result = Err(AiError::new(error.code.as_str(), error.message));
    }
    let accounting = ai::session_usage::checkpoint_final(
        &stats,
        &app,
        &request_id,
        ai::session_usage::Purpose::Chat,
    )
    .await;
    if let (Some(turn), Ok(reply)) = (prepared.as_ref(), result.as_mut()) {
        let session_id = turn.session_id.clone();
        let lease = stats.accounting();
        let saved = tauri::async_runtime::spawn_blocking({
            let app = app.clone();
            let session_id = session_id.clone();
            let reply = reply.clone();
            move || {
                let incarnation = lease.map(|lease| lease.update().incarnation);
                crate::commands::record_reply_for(
                    &app,
                    &session_id,
                    &reply,
                    &session_provider,
                    incarnation.as_deref(),
                )
                .map(|_| ())
            }
        })
        .await
        .map_err(|error| format!("Session persistence task failed: {error}"))
        .and_then(|result| result);
        // A reply the store could not keep stays in this turn: the panel still
        // paints the answer and reports that the conversation lacks it.
        save_reply(&session_id, reply, || saved);
    }
    if stats.attempted() || accounting.is_some() {
        let _ = app.emit(
            "ai_stats",
            ai::session_usage::StatsEvent {
                turn: stats.snapshot(),
                client_request_id: request_id.clone(),
                purpose: ai::session_usage::Purpose::Chat,
                accounting: accounting.clone(),
            },
        );
    }
    if let (Some(update), Ok(reply)) = (accounting, result.as_mut()) {
        ai::session_usage::annotate_reply(reply, &update);
    }
    let _ = requests.release_owned(&request_id, &owner);
    result
}
#[tauri::command]
pub fn ai_cancel(request_id: String, requests: State<Requests>) -> Result<(), String> {
    if let Some(cancel) = requests.release(&request_id)? {
        let _ = cancel.send(());
    }
    Ok(())
}

#[cfg(test)]
static TEST_CONFIG_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
/// Serializes tests that redirect the saved-config path: it is process-global.
#[cfg(test)]
static TEST_CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Redirects the saved config at a fixture file until the guard drops, so a test
/// never reads or writes the operator's `~/.opencompose/ai-providers.yaml`.
#[cfg(test)]
pub(crate) fn config_path_override() -> Option<std::path::PathBuf> {
    TEST_CONFIG_PATH.lock().ok().and_then(|path| path.clone())
}

#[cfg(test)]
pub(crate) fn test_config_path(path: std::path::PathBuf) -> TestConfigPathGuard {
    let lock = TEST_CONFIG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_CONFIG_PATH.lock() {
        *slot = Some(path);
    }
    TestConfigPathGuard { _lock: lock }
}

#[cfg(test)]
pub(crate) struct TestConfigPathGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestConfigPathGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_CONFIG_PATH.lock() {
            *slot = None;
        }
    }
}

/// The operator's file, written by an older build: `deepseek-v4-flash` is our own
/// rename, `gpt-4o` and `gemini-2.5-pro-preview-06-05` are ids the catalog never
/// carried, and `custom` is the untouched placeholder.
#[cfg(test)]
pub(crate) const OPERATOR_CONFIG_YAML: &str = r#"version: 2
active_provider: deepseek-v4-flash
providers:
  - id: deepseek-v4-flash
    name: DeepSeek V4 Flash
    kind: openai_chat_completions
    auth: api_key
    endpoint: https://api.deepseek.com/chat/completions
    model: deepseek-v4-flash
    context_length: 1000000
    max_tokens: 8192
    temperature: 0.3
    capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true }
  - id: deepseek-v4-pro
    name: DeepSeek V4 Pro
    kind: openai_chat_completions
    auth: api_key
    endpoint: https://api.deepseek.com/chat/completions
    model: deepseek-v4-pro
    context_length: 1000000
    max_tokens: 8192
    temperature: 0.3
    capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true }
  - id: openai-gpt4o
    name: OpenAI GPT-4o
    kind: openai_responses
    auth: api_key
    endpoint: https://api.openai.com/v1/responses
    model: gpt-4o
    context_length: 128000
    max_tokens: 16384
    temperature: 0.3
    capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true }
  - id: gemini-2.5-pro
    name: Gemini 2.5 Pro
    kind: gemini_interactions
    auth: api_key
    endpoint: https://generativelanguage.googleapis.com/v1beta/interactions
    model: gemini-2.5-pro-preview-06-05
    context_length: 1048576
    max_tokens: 65536
    temperature: 0.3
    capabilities: { text: true, audio_in: true, audio_out: false, native_tool_calling: true, streaming: true }
  - id: ollama
    name: Ollama (Local)
    kind: openai_chat_completions
    auth: api_key
    endpoint: http://localhost:11434/v1/chat/completions
    model: llama3.1
    context_length: 8192
    max_tokens: 4096
    temperature: 0.3
    capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: true }
  - id: custom
    name: Custom Endpoint
    kind: openai_chat_completions
    auth: api_key
    endpoint: ""
    model: ""
    context_length: 1000000
    max_tokens: 16000
    temperature: 0.3
    capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: false }
"#;

/// Ids the catalog does not describe on hosts it does serve, with a fetched
/// model list on one of them.
#[cfg(test)]
pub(crate) const FOREIGN_ID_CONFIG_YAML: &str = r#"version: 2
active_provider: deepseek-foreign
providers:
  - id: deepseek-foreign
    name: DeepSeek (foreign id)
    kind: openai_chat_completions
    auth: api_key
    endpoint: https://api.deepseek.com/chat/completions
    model: gpt-4o
    models: [gpt-4o, gpt-4o-mini]
    max_tokens: 16384
    temperature: 0.3
  - id: openai-foreign
    name: OpenAI (foreign id)
    kind: openai_responses
    auth: api_key
    endpoint: https://api.openai.com/v1/responses
    model: deepseek-v4-flash
    max_tokens: 8192
    temperature: 0.3
"#;

/// Turns a failed reply write into the warning the turn carries.
///
/// The reply stays in the returned turn, so the live transcript keeps the
/// answer; only a later request sees that the conversation lacks it.
pub(crate) fn save_reply(
    session_id: &str,
    reply: &mut Value,
    save: impl FnOnce() -> Result<(), String>,
) {
    let Err(error) = save() else { return };
    log::error!("[ai] session {session_id}: the reply could not be saved: {error}");
    if let Some(message) = reply.as_object_mut() {
        message.insert(
            "session_warning".into(),
            json!(format!(
                "The reply could not be saved; it will be lost when the window closes. Session {session_id}: {error}"
            )),
        );
    }
}

// Audio must not be sent when its durable archive failed. Text-only requests
// retain the existing best-effort session fallback.
fn saved_audio_or_fallback<T>(
    result: Result<Option<T>, String>,
    messages: &[Value],
    requests: &Requests,
    request_id: &str,
) -> Result<Option<T>, AiError> {
    match result {
        Ok(prepared) => Ok(prepared),
        Err(error)
            if messages
                .iter()
                .any(|message| message.get("input_audio").is_some()) =>
        {
            let _ = requests.release(request_id);
            Err(AiError::new(
                "session_save",
                format!("Could not save audition audio. Nothing was sent: {error}"),
            ))
        }
        Err(error) => {
            log::warn!("[ai] session not saved: {error}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accounting_request_owner_rejects_late_cleanup_after_uuid_reuse() {
        let requests = Requests::default();
        let RequestClaim {
            cancel_rx: mut first,
            identity: old,
        } = requests.claim("same").unwrap().unwrap();
        requests.release("same").unwrap().unwrap().send(()).unwrap();
        let RequestClaim {
            cancel_rx: mut second,
            identity: current,
        } = requests.claim("same").unwrap().unwrap();
        // A resumes only after cancellation and B's admission completed.
        assert_eq!(first.try_recv(), Ok(()));
        assert!(!Arc::ptr_eq(&old, &current));
        requests.set_session_owned("same", &old, "old-session");
        assert!(requests.release_owned("same", &old).unwrap().is_none());
        assert_eq!(second.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        requests.set_session_owned("same", &current, "current-session");
        let active = requests.active().unwrap().unwrap();
        assert_eq!(active.session_id.as_deref(), Some("current-session"));
        assert!(requests.release_owned("same", &current).unwrap().is_some());
    }

    use super::*;

    #[test]
    fn separator_stop_is_latched_even_before_the_job_starts() {
        let jobs = SeparatorJobs::default();
        jobs.cancel("early".into()).unwrap();
        let early = jobs.begin("early".into()).unwrap();
        assert!(early.cancel.load(Ordering::Acquire));
        let later = jobs.begin("later".into()).unwrap();
        assert!(!later.cancel.load(Ordering::Acquire));
        jobs.cancel("later".into()).unwrap();
        assert!(later.cancel.load(Ordering::Acquire));
        drop(later);
        drop(early);
        assert!(jobs.0.lock().unwrap().running.is_empty());
    }
    use crate::commands::Wire;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn a_reply_the_store_cannot_save_keeps_its_text_and_carries_the_store_error() {
        let root = oc_test_support::temp_dir("oc-reply-save", "store");
        let store = crate::ai::sessions::Store::new(root);
        let mut reply = json!({"role": "assistant", "content": "The bass line is in."});
        // The conversation was deleted while the turn ran: the real store error.
        let error = store
            .record_reply(
                "gone",
                &reply,
                &crate::ai::sessions::ProjectFacts::default(),
                "p",
                "m",
            )
            .expect_err("a missing session must fail");
        save_reply("gone", &mut reply, || Err(error.clone()));

        assert_eq!(reply["content"], "The bass line is in.");
        let warning = reply["session_warning"]
            .as_str()
            .expect("the turn carries a warning");
        assert!(warning.contains("gone"), "{warning}");
        assert!(warning.contains(&error), "{warning}");
        assert!(warning.contains("could not be saved"), "{warning}");
        assert!(warning.contains("lost when the window closes"), "{warning}");
    }

    #[test]
    fn a_saved_reply_carries_no_warning() {
        let mut reply = json!({"role": "assistant", "content": "Done."});
        save_reply("s1", &mut reply, || Ok(()));
        assert!(reply.get("session_warning").is_none());
        assert_eq!(reply["content"], "Done.");
    }

    #[test]
    fn audition_persistence_failure_releases_request_and_never_falls_back() {
        let requests = Requests::default();
        let _receiver = requests.claim("listen").unwrap().unwrap();
        let result = saved_audio_or_fallback::<()>(
            Err("disk full".into()),
            &[json!({"role":"user", "input_audio":{"data":"fixture"}})],
            &requests,
            "listen",
        );
        assert!(result.is_err());
        assert!(requests.claim("listen").unwrap().is_some());
        assert!(
            saved_audio_or_fallback::<()>(
                Err("disk full".into()),
                &[json!({"role":"user", "content":"hello"})],
                &requests,
                "text"
            )
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn credential_wait_timeout_releases_slot_and_reuses_pending_os_read() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let reads = PendingKeyReads::default();
        let count = Arc::new(AtomicUsize::new(0));
        let (release, blocked) = std::sync::mpsc::channel();
        let count_read = count.clone();
        let first = reads.start("fixture".into(), move || {
            count_read.fetch_add(1, Ordering::SeqCst);
            blocked.recv().unwrap();
            Ok(None)
        });
        let requests = Requests::default();
        let RequestClaim {
            cancel_rx: rx,
            identity: owner,
        } = requests.claim("first").unwrap().unwrap();
        let error = cancellable_request(
            &requests,
            "first",
            rx,
            owner,
            wait_for_key(first, Duration::from_millis(5)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, "credential_pending");
        assert!(error.message.contains("Keychain"));
        assert!(requests.active().unwrap().is_none());
        let next = reads.start("fixture".into(), || {
            panic!("must share the pending OS read")
        });
        release.send(()).unwrap();
        assert!(
            wait_for_key(next, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn credential_wait_cancel_never_sends_after_late_os_completion() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let reads = PendingKeyReads::default();
        let (release, blocked) = std::sync::mpsc::channel();
        let pending = reads.start("fixture".into(), move || {
            blocked.recv().unwrap();
            Ok(None)
        });
        let late = pending.clone();
        let requests = Requests::default();
        let RequestClaim {
            cancel_rx: rx,
            identity: owner,
        } = requests.claim("listen").unwrap().unwrap();
        let sent = Arc::new(AtomicBool::new(false));
        let sent_request = sent.clone();
        requests
            .release("listen")
            .unwrap()
            .unwrap()
            .send(())
            .unwrap();
        let error = cancellable_request(&requests, "listen", rx, owner, async move {
            wait_for_key(pending, Duration::from_secs(1)).await?;
            sent_request.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind, "cancelled");
        assert!(requests.claim("next").unwrap().is_some());
        release.send(()).unwrap();
        assert!(
            wait_for_key(late, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!sent.load(Ordering::SeqCst));
    }

    /// The panel reads `kind` and `retryable` off this shape: a credential-store
    /// failure is a named configuration error that no Retry can fix.
    #[test]
    fn a_credential_store_failure_reaches_the_panel_as_configuration() {
        let error = AiError::from(ProviderError::credential_store(
            "Cannot unlock/read OS credential store. No plaintext fallback is used.",
        ));
        assert_eq!(error.kind, "config");
        assert!(!error.retryable);
        assert!(
            error.message.starts_with("Configuration error"),
            "{}",
            error.message
        );

        let limited = AiError::from(ai::map_status(429, "fixture"));
        assert_eq!(limited.kind, "rate_limit");
        assert!(limited.retryable);
    }

    #[test]
    fn endpoints_reject_plaintext_remote_and_embedded_secrets() {
        for raw in [
            "http://example.com",
            "http://localhost.evil.test",
            "https://user:key@example.com",
            "https://example.com?key=secret",
            "file:///tmp/x",
            "https://example.com#key",
        ] {
            assert!(endpoint(raw).is_err(), "{raw}");
        }
        for raw in [
            "https://example.com/v1/chat/completions",
            "http://127.0.0.1:1234/v1/chat/completions",
            "http://[::1]:8000",
            "http://localhost:8000",
        ] {
            assert!(endpoint(raw).is_ok(), "{raw}");
        }
    }
    #[test]
    fn tools_are_the_shared_registry_panel_surface() {
        let names: Vec<String> = tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value["function"]["name"].as_str().unwrap().to_owned())
            .collect();
        let expected: Vec<String> = crate::tool_registry::panel_tools_for(None)
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, expected);
        // The independent policy fixture pins identities, including panel-local
        // tools, rather than mistaking a matching count for matching authority.
        let contract: Vec<Value> =
            serde_json::from_str(include_str!("tool_registry/metadata-contract.json")).unwrap();
        let expected_identities: std::collections::BTreeSet<_> = contract
            .iter()
            .filter(|tool| tool["panel"]["enabled"] == true)
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        let actual_identities: std::collections::BTreeSet<_> = names.iter().cloned().collect();
        assert_eq!(actual_identities, expected_identities);
        assert_eq!(
            names.len(),
            actual_identities.len(),
            "duplicate offered identity"
        );
        let wire = tools();
        for (actual, expected) in wire
            .as_array()
            .unwrap()
            .iter()
            .zip(crate::tool_registry::panel_tools_for(None))
        {
            assert_eq!(actual["type"], "function");
            assert_eq!(actual["function"]["name"], expected["name"]);
            assert_eq!(actual["function"]["description"], expected["description"]);
            assert_eq!(actual["function"]["parameters"], expected["inputSchema"]);
        }
        for name in [
            "plugin_pull",
            "sample_search",
            "sample_pull",
            "render_range",
            "audition_range",
            "audition_clip_source",
            "audio_analyze",
            "analyze_notes",
            "separate_stems",
            "audio_compare",
            "transaction_begin",
            "transaction_commit",
            "transaction_rollback",
            "edit_subtree",
            "list_skills",
        ] {
            assert!(names.contains(&name.to_string()), "{name}");
        }
        for name in [
            "render_offline",
            "project_open",
            "project_save",
            "midi_export",
            "plugins_scan",
        ] {
            assert!(!names.contains(&name.to_string()), "{name} must stay out");
        }
    }

    #[test]
    fn streamed_arguments_reassemble_and_truncation_fails_closed() {
        let mut c = Completion::default();
        c.event(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","function":{"name":"read_context","arguments":"{\"path\":"}}]}}]}"#, &None).unwrap();
        c.event(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"project\"}"}}]},"finish_reason":"tool_calls"}]}"#, &None).unwrap();
        let m = c.result().unwrap();
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"project"}"#
        );
        assert!(Completion::default().result().is_err());
        // A `length` stop is refused when the accumulated turn is read, so an
        // empty one can name its finish reason instead of only "truncated".
        let mut truncated = Completion::default();
        truncated
            .event(r#"{"choices":[{"delta":{"content":"Kick i"}}]}"#, &None)
            .unwrap();
        truncated
            .event(r#"{"choices":[{"finish_reason":"length"}]}"#, &None)
            .unwrap();
        assert_eq!(
            truncated.result().unwrap_err().kind,
            ProviderErrorKind::Truncated
        );
        let mut empty = Completion::default();
        empty
            .event(r#"{"choices":[{"finish_reason":"length"}]}"#, &None)
            .unwrap();
        let empty = empty.result().unwrap_err();
        assert!(
            empty.message.contains("empty reply (finish_reason=length"),
            "{}",
            empty.message
        );
        assert!(empty.retryable, "an empty length reply offers Retry");
    }
    #[tokio::test]
    async fn exposed_reasoning_reaches_its_channel_without_changing_the_answer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _ = read_request(&mut socket);
            respond(
                &mut socket,
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"Done.","reasoning_content":"Read the piano notes."},"finish_reason":"stop"}]}"#,
            );
        });
        let received = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = received.clone();
        let channel = tauri::ipc::Channel::<String>::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(text) = body {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str(&text).unwrap());
            }
            Ok(())
        });
        let mut provider = fixture_provider(url);
        provider.capabilities.streaming = false;
        let result = send_http_with_thinking(
            provider,
            TurnRequest {
                messages: json!([{"role":"user","content":"compose"}]),
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                thinking: Some(channel),
                preparing: None,
            },
            Credential::ApiKey(None),
            |_| async { Err(AiError::config("unexpected refresh")) },
            &turn_stats(),
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(*received.lock().unwrap(), vec!["Read the piano notes."]);
        assert_eq!(result["content"], "Done.");
        assert_eq!(result["reasoning_content"], "Read the piano notes.");
        assert_eq!(result["thinking"][0]["text"], "Read the piano notes.");
    }

    fn turn_stats() -> ai::timing::TurnStatsHandle {
        ai::timing::TurnStatsHandle::new(
            "fixture",
            "fixture-model",
            crate::commands::Wire::OpenAiChatCompletions,
        )
    }

    /// A file the media gate sees as present, so it judges the modality.
    fn existing_media(name: &str) -> String {
        oc_test_support::temp_file("oc-ai-service", name, b"fixture bytes")
            .to_string_lossy()
            .into_owned()
    }
    fn fixture_provider(url: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "fixture".into(),
            name: "fixture".into(),
            kind: Default::default(),
            wire: Some(Wire::OpenAiChatCompletions),
            auth: Default::default(),
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: url,
            model: "fixture-model".into(),
            models: Vec::new(),
            context_length: 32000,
            max_tokens: 1024,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }
    fn oauth_provider(url: String) -> AiProviderEntry {
        let mut provider = fixture_provider(url);
        provider.auth = AuthKind::OAuth;
        provider.auth_provider = Some(AuthProvider::OpenAiChatGpt);
        provider.oauth_allowed_hosts = vec!["127.0.0.1".into()];
        provider
    }
    fn oauth_token(access: &str) -> Credential {
        Credential::OAuth(auth::TokenSet {
            v: 1,
            access: access.into(),
            refresh: "refresh-secret".into(),
            expires_at: oauth::now_ms() + 3_600_000,
            account_id: Some("acct".into()),
            account_label: None,
            scope: None,
            client_id: None,
        })
    }
    fn write_wav(path: &std::path::Path, seconds: f64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..(16_000.0 * seconds) as u32 {
            let t = frame as f64 / 16_000.0;
            let sample = (t * 440.0 * std::f64::consts::TAU).sin() * 0.2;
            writer
                .write_sample((sample * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }
    #[tokio::test]
    async fn a_changed_audio_file_is_rehashed_and_the_notice_rides_the_result() {
        let dir = std::env::temp_dir().join(format!("oc-ai-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.wav");
        write_wav(&path, 0.2);
        let stale = ai::media::attach(&path.to_string_lossy()).unwrap().media;
        write_wav(&path, 0.4);
        let fresh = ai::media::attach(&path.to_string_lossy()).unwrap().media;
        let data = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(&path).unwrap())
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut socket);
            respond(
                &mut socket,
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"heard it"},"finish_reason":"stop"}]}"#,
            );
            request
        });
        let mut provider = fixture_provider(url);
        provider.capabilities.audio_in = true;
        let messages = json!([{
            "role": "user",
            "content": "what do you hear?",
            "media": [{
                "kind": "audio", "name": stale.name, "path": stale.path, "mime": stale.mime,
                "bytes": stale.bytes, "duration_secs": stale.duration_secs, "sha256": stale.sha256
            }]
        }]);
        let stats = turn_stats();
        let result = send_http(
            provider,
            TurnRequest {
                messages,
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            Credential::ApiKey(Some("fixture-secret".into())),
            |_| async { Err(AiError::config("unexpected refresh")) },
            &stats,
        )
        .await
        .unwrap();
        assert_eq!(result["content"], "heard it");
        assert_eq!(
            result["media_notices"][0]["sha256"].as_str(),
            Some(fresh.sha256.as_str())
        );
        let request = server.join().unwrap();
        assert!(
            request.contains(&data),
            "the fresh audio bytes were not sent"
        );
        assert!(
            !request.contains(&stale.sha256),
            "a hash is not sent on the wire"
        );
        let body = request.split_once("\r\n\r\n").unwrap().1;
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();
        let audio = content
            .iter()
            .find(|part| part["type"] == "input_audio")
            .expect("the fresh audio part is on the wire");
        assert_eq!(audio["input_audio"]["format"], "wav");
        assert!(
            content[1]["text"]
                .as_str()
                .is_some_and(|text| text.contains("changed on disk")),
            "the model is told the file changed: {body}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn oauth_status_payload_never_carries_a_token() {
        let status = OAuthStatus {
            state: "connected".into(),
            account: Some("fixture@example.test".into()),
            expires_at: None,
            reauth_required: false,
            message: "Connected.".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        for field in ["access", "refresh", "bearer"] {
            assert!(!json.contains(field), "{field}: {json}");
        }
        assert!(json.contains("fixture@example.test"));
    }

    fn gemini_entry() -> AiProviderEntry {
        let mut provider =
            fixture_provider("https://generativelanguage.googleapis.com/v1beta".into());
        provider.wire = Some(Wire::GeminiInteractions);
        provider.model = "gemini-4-argon".into();
        provider
    }

    #[test]
    fn every_declared_thinking_level_saves_and_others_name_the_model() {
        let mut config = AiProviderConfig {
            version: 2,
            active_provider: "fixture".into(),
            providers: vec![gemini_entry()],
            warnings: Vec::new(),
        };
        for level in ["low", "medium", "high"] {
            config.providers[0].thinking_level = level.into();
            assert!(validate_config(&config).is_ok(), "{level}");
        }
        config.providers[0].thinking_level = "maximum".into();
        let error = validate_config(&config).unwrap_err();
        assert!(error.contains("gemini-4-argon"), "{error}");
        assert!(error.contains("low, medium, high"), "{error}");
    }

    #[test]
    fn an_gateway_native_gemini_entry_validates_with_a_bare_argon_id() {
        let mut provider = fixture_provider("https://gateway.example.test/v1beta".into());
        provider.id = "gateway-google".into();
        provider.name = "Gateway-Google".into();
        provider.wire = Some(Wire::GeminiGenerate);
        provider.kind = crate::commands::ProviderKind::GeminiGenerate;
        provider.model = "gemini-4-argon".into();
        provider.models = vec!["gemini-4-argon".into()];
        provider.capabilities.audio_in = true;
        let config = AiProviderConfig {
            version: 2,
            active_provider: "gateway-google".into(),
            providers: vec![provider],
            warnings: Vec::new(),
        };
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn a_model_the_catalog_does_not_know_still_takes_the_common_levels() {
        let mut config = AiProviderConfig {
            version: 2,
            active_provider: "fixture".into(),
            providers: vec![fixture_provider(
                "https://llm.acme.test/v1/chat/completions".into(),
            )],
            warnings: Vec::new(),
        };
        for level in ["low", "medium", "high"] {
            config.providers[0].thinking_level = level.into();
            assert!(validate_config(&config).is_ok(), "{level}");
        }
        config.providers[0].thinking_level = "maximum".into();
        let error = validate_config(&config).unwrap_err();
        assert!(error.contains("fixture-model"), "{error}");
        assert!(error.contains("low, medium, high"), "{error}");
    }

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        oc_test_support::temp_dir("oc-ai-config", name)
    }

    /// Writes `yaml` at a fixture config path the commands read until the guard drops.
    fn config_at(name: &str, yaml: &str) -> (std::path::PathBuf, TestConfigPathGuard) {
        let dir = fixture_dir(name);
        let path = dir.join("ai-providers.yaml");
        crate::private_fs::write_atomic(&path, yaml.as_bytes()).unwrap();
        let guard = test_config_path(path.clone());
        (path, guard)
    }

    #[test]
    fn the_operator_config_saves_and_reports_the_ids_the_catalog_cannot_describe() {
        let (path, _guard) = config_at("operator-save", OPERATOR_CONFIG_YAML);
        let config = crate::commands::load_ai_config().unwrap();
        // The alias is applied on load, so nothing downstream sees the old id.
        let flash = config
            .providers
            .iter()
            .find(|provider| provider.id == "deepseek-v4-flash")
            .unwrap();
        assert_eq!(flash.model, "deepseek-flash");
        let saved = crate::commands::save_ai_config(config).unwrap();
        assert_eq!(saved.path, path.display().to_string());
        let names: Vec<&str> = saved
            .warnings
            .iter()
            .map(|warning| warning.provider_name.as_str())
            .collect();
        assert_eq!(names, ["OpenAI GPT-4o", "Gemini 2.5 Pro"]);
        for warning in &saved.warnings {
            assert!(
                warning.message.contains(&warning.provider_name),
                "{}",
                warning.message
            );
            assert!(
                warning.message.contains(&warning.model),
                "{}",
                warning.message
            );
            assert!(
                warning.message.contains("provider's settings"),
                "{}",
                warning.message
            );
        }
        // The save heals the file itself: the next load already has the current id.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model: deepseek-flash"), "{text}");
        let reloaded = crate::commands::load_ai_config().unwrap();
        assert_eq!(reloaded.warnings.len(), 2);
    }

    #[test]
    fn a_provider_can_be_added_while_a_sibling_stays_undescribed() {
        let (path, _guard) = config_at("operator-add", OPERATOR_CONFIG_YAML);
        let added = crate::commands::ai_add_provider(crate::commands::AiProviderDraft {
            name: "Groq".into(),
            wire: Wire::OpenAiChatCompletions,
            endpoint: "https://api.groq.com/openai/v1/chat/completions".into(),
            auth: AuthKind::ApiKey,
            models: vec!["llama-3.3-70b-versatile".into()],
            limits: std::collections::BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(added.provider_id, "groq");
        assert_eq!(added.config.active_provider, "groq");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("llama-3.3-70b-versatile"), "{text}");
        let reloaded = crate::commands::load_ai_config().unwrap();
        assert_eq!(reloaded.providers.len(), 7);
    }

    #[test]
    fn a_healthy_provider_resolves_while_a_sibling_is_undescribed() {
        let (_path, _guard) = config_at("operator-chat", OPERATOR_CONFIG_YAML);
        let healthy = provider("deepseek-v4-pro").unwrap();
        assert_eq!(healthy.model, "deepseek-v4-pro");
        // Nothing gates the undescribed entry any more: it resolves with its own id.
        let foreign = provider("openai-gpt4o").unwrap();
        assert_eq!(foreign.model, "gpt-4o");
    }

    #[tokio::test]
    async fn an_undescribed_id_is_sent_as_the_caller_typed_it() {
        let (_path, _guard) = config_at("foreign-id", FOREIGN_ID_CONFIG_YAML);
        let config = crate::commands::load_ai_config().unwrap();
        let saved = crate::commands::save_ai_config(config).unwrap();
        assert_eq!(saved.warnings.len(), 2, "{:?}", saved.warnings);
        let mut entry = provider("deepseek-foreign").unwrap();
        assert_eq!(entry.model, "gpt-4o");
        assert_eq!(entry.models, ["gpt-4o", "gpt-4o-mini"]);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        entry.endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut socket);
            respond(
                &mut socket,
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
            );
            request
        });
        let result = send_http(
            entry,
            TurnRequest {
                messages: json!([{"role":"user","content":"hello"}]),
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            Credential::ApiKey(Some("fixture-secret".into())),
            |_| async { Err(AiError::config("unexpected refresh")) },
            &turn_stats(),
        )
        .await
        .unwrap();
        assert_eq!(result["content"], "ok");
        let request = server.join().unwrap();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["max_tokens"], 16_384);
        assert_eq!(body["temperature"], 0.3);
    }

    #[test]
    fn inactive_placeholder_does_not_block_default_settings() {
        let active = fixture_provider("https://example.com/v1/chat/completions".into());
        let mut placeholder = fixture_provider(String::new());
        placeholder.id = "custom".into();
        placeholder.model.clear();
        let mut config = AiProviderConfig {
            version: 2,
            active_provider: "fixture".into(),
            providers: vec![active, placeholder],
            warnings: Vec::new(),
        };
        assert!(validate_config(&config).is_ok());
        config.active_provider = "custom".into();
        assert!(validate_config(&config).is_err());
        let serialized = serde_json::to_string(&config).unwrap();
        assert!(serialized.contains("\"auth\":\"api_key\""));
        assert!(!serialized.to_lowercase().contains("apikey"));
        assert!(!serialized.contains("secret"));
    }
    fn read_request(socket: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut data = Vec::new();
        let mut part = [0u8; 4096];
        loop {
            let n = socket.read(&mut part).unwrap();
            if n == 0 {
                break;
            }
            data.extend_from_slice(&part[..n]);
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&data[..end]);
                let size: usize = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if data.len() >= end + 4 + size {
                    break;
                }
            }
        }
        String::from_utf8(data).unwrap()
    }
    fn respond(socket: &mut std::net::TcpStream, status: u16, body: &str) {
        use std::io::Write;
        write!(
            socket,
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\nLocation: http://127.0.0.1:1/leak\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }
    #[tokio::test]
    async fn transport_auth_tools_and_redacted_errors() {
        for status in [200, 401, 302] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "http://{}/v1/chat/completions",
                listener.local_addr().unwrap()
            );
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut socket);
                assert!(request.contains("Bearer fixture-secret"));
                assert!(request.contains("edit_subtree"));
                let body = if status == 200 {
                    r#"{"choices":[{"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}]}"#
                } else {
                    "fixture-secret must never escape"
                };
                respond(&mut socket, status, body);
            });
            let result = send_http(
                fixture_provider(url),
                TurnRequest {
                    messages: json!([{"role":"user","content":"hello"}]),
                    use_tools: true,
                    toolsets: None,
                },
                TurnChannels {
                    delta: None,
                    ..Default::default()
                },
                Credential::ApiKey(Some("fixture-secret".into())),
                |_| async { Err(AiError::config("unexpected refresh")) },
                &turn_stats(),
            )
            .await;
            if status == 200 {
                assert_eq!(result.unwrap()["content"], "hello");
            } else {
                let error = result.unwrap_err();
                assert!(!error.message.contains("fixture-secret"));
                assert!(!error.message.contains("must never"));
            }
            server.join().unwrap();
        }
    }
    #[tokio::test]
    async fn oauth_401_refreshes_once_and_retries_with_the_new_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                requests.push(read_request(&mut socket));
                if attempt == 0 {
                    respond(&mut socket, 401, "denied");
                } else {
                    respond(
                        &mut socket,
                        200,
                        r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                    );
                }
            }
            requests
        });
        let refreshed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = refreshed.clone();
        let result = send_http(
            oauth_provider(url),
            TurnRequest {
                messages: json!([{"role":"user","content":"hello"}]),
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            oauth_token("old-token"),
            move |_| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(oauth_token("new-token"))
                }
            },
            &turn_stats(),
        )
        .await
        .unwrap();
        assert_eq!(result["content"], "ok");
        assert_eq!(refreshed.load(std::sync::atomic::Ordering::SeqCst), 1);
        let requests = server.join().unwrap();
        assert!(requests[0].contains("Bearer old-token"));
        assert!(requests[1].contains("Bearer new-token"));
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("chatgpt-account-id: acct")
        );
    }
    #[tokio::test]
    async fn a_second_401_is_terminal_and_api_keys_never_refresh() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                read_request(&mut socket);
                respond(&mut socket, 401, "denied");
            }
        });
        let error = send_http(
            oauth_provider(url),
            TurnRequest {
                messages: json!([{"role":"user","content":"hello"}]),
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            oauth_token("old-token"),
            |_| async { Ok(oauth_token("new-token")) },
            &turn_stats(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, "auth_expired");
        assert!(error.reauth);
        assert!(!error.message.contains("old-token"));
        server.join().unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            respond(&mut socket, 401, "denied");
        });
        let error = send_http(
            fixture_provider(url),
            TurnRequest {
                messages: json!([{"role":"user","content":"hello"}]),
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            Credential::ApiKey(Some("fixture-secret".into())),
            |_| async { unreachable!("API keys must never trigger an OAuth refresh") },
            &turn_stats(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, "auth");
        assert!(!error.reauth);
        server.join().unwrap();
    }
    #[test]
    #[ignore = "explicit OS credential-store smoke; creates and removes only a unique fixture account"]
    fn native_credential_roundtrip() {
        let account = format!(
            "test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        );
        let entry = keyring::Entry::new("org.opencompose.ai.test", &account).unwrap();
        entry
            .set_password("temporary-fixture-not-an-api-key")
            .unwrap();
        let result = entry.get_password();
        entry.delete_credential().unwrap();
        assert_eq!(result.unwrap(), "temporary-fixture-not-an-api-key");
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }

    #[tokio::test]
    async fn an_audio_attachment_is_refused_before_any_socket_is_opened() {
        crate::ai::limits::install_for_test(
            "gate-wire-fixture",
            "text-only-wire-model",
            crate::ai::limits::ModelLimits {
                context: 128_000,
                max_output: 4_096,
                input: crate::ai::limits::InputModalities {
                    text: true,
                    ..Default::default()
                },
                price: None,
            },
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let mut provider = fixture_provider(url);
        provider.id = "gate-wire-fixture".into();
        provider.model = "text-only-wire-model".into();
        let path = existing_media("take.wav");
        let messages = json!([{
            "role": "user",
            "content": "Transcribe this",
            "media": [{"kind": "audio", "name": "take.wav", "path": path}]
        }]);
        let stats = turn_stats();
        let error = send_http(
            provider,
            TurnRequest {
                messages,
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            Credential::ApiKey(Some("fixture-secret".into())),
            |_| async { Err(AiError::config("unexpected refresh")) },
            &stats,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, "config");
        assert!(
            error.message.contains("does not accept audio"),
            "{}",
            error.message
        );
        assert!(
            listener.accept().is_err(),
            "the request must be refused before any connection"
        );
    }

    #[tokio::test]
    async fn an_image_attachment_is_refused_before_any_socket_is_opened() {
        crate::ai::limits::install_for_test(
            "image-gate-fixture",
            "text-only-image-model",
            crate::ai::limits::ModelLimits {
                context: 128_000,
                max_output: 4_096,
                input: crate::ai::limits::InputModalities {
                    text: true,
                    image: false,
                    ..Default::default()
                },
                price: None,
            },
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let mut provider = fixture_provider(url);
        provider.id = "image-gate-fixture".into();
        provider.model = "text-only-image-model".into();
        let path = existing_media("cover.png");
        let messages = json!([{
            "role": "user",
            "content": "Describe this",
            "media": [{"kind": "image", "name": "cover.png", "path": path}]
        }]);
        let stats = turn_stats();
        let error = send_http(
            provider,
            TurnRequest {
                messages,
                use_tools: false,
                toolsets: None,
            },
            TurnChannels {
                delta: None,
                ..Default::default()
            },
            Credential::ApiKey(Some("fixture-secret".into())),
            |_| async { Err(AiError::config("unexpected refresh")) },
            &stats,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, "config");
        assert!(
            error.message.contains("does not accept image"),
            "{}",
            error.message
        );
        assert!(
            listener.accept().is_err(),
            "the request must be refused before any connection"
        );
    }
}

#[cfg(test)]
mod key_read_tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers with a canned result and counts how often it was asked, so a
    /// test can prove the OS is read once and never inside a later turn.
    struct CountingStore {
        answer: Result<Option<Vec<u8>>, credential_store::StoreError>,
        reads: RefCell<usize>,
    }

    impl CountingStore {
        fn new(answer: Result<Option<Vec<u8>>, credential_store::StoreError>) -> Self {
            Self {
                answer,
                reads: RefCell::new(0),
            }
        }

        fn reads(&self) -> usize {
            *self.reads.borrow()
        }
    }

    impl credential_store::SecretStore for CountingStore {
        fn get(&self, _account: &str) -> Result<Option<Vec<u8>>, credential_store::StoreError> {
            *self.reads.borrow_mut() += 1;
            self.answer.clone()
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), credential_store::StoreError> {
            Ok(())
        }

        fn delete(&self, _account: &str) -> Result<(), credential_store::StoreError> {
            Ok(())
        }
    }

    /// A provider on a host no catalog serves, so no catalog environment
    /// variable can answer for it.
    fn provider(id: &str) -> AiProviderEntry {
        AiProviderEntry {
            id: id.into(),
            name: id.into(),
            kind: crate::commands::ProviderKind::OpenAiChatCompletions,
            wire: Some(crate::commands::Wire::OpenAiChatCompletions),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: format!("https://{id}.acme.test/v1/chat/completions"),
            model: "fixture-model".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }

    #[test]
    fn a_refused_read_is_remembered_so_the_next_turn_does_not_prompt() {
        let store = CountingStore::new(Err(credential_store::StoreError::Unreadable));
        let p = provider("refused-once");
        for attempt in ["first", "second"] {
            let error = read_key_with(&store, &p, false).unwrap_err();
            assert_eq!(error, crate::ai::auth::STORE_REFUSED, "{attempt}");
        }
        assert_eq!(store.reads(), 1, "the OS was asked twice");
        // The message says how to grant access again instead of only failing.
        assert!(crate::ai::auth::STORE_REFUSED.contains("permission prompt"));
        assert!(crate::ai::auth::STORE_REFUSED.contains("OPENCOMPOSE_AI_KEY"));
    }

    #[test]
    fn a_stored_key_is_read_once_and_reused() {
        let store = CountingStore::new(Ok(Some(b"sk-fixture".to_vec())));
        let p = provider("stored-once");
        for attempt in ["first", "second"] {
            let key = read_key_with(&store, &p, false).unwrap().expect("a key");
            assert_eq!(key.as_str(), "sk-fixture", "{attempt}");
        }
        assert_eq!(store.reads(), 1, "the OS was asked twice");
    }

    #[test]
    fn a_missing_key_is_remembered_as_absent() {
        let store = CountingStore::new(Ok(None));
        let p = provider("absent-once");
        for attempt in ["first", "second"] {
            assert!(
                read_key_with(&store, &p, false).unwrap().is_none(),
                "{attempt}"
            );
        }
        assert_eq!(store.reads(), 1, "the OS was asked twice");
    }

    #[test]
    fn only_an_explicit_read_asks_the_store_again() {
        let store = CountingStore::new(Err(credential_store::StoreError::Unreadable));
        let p = provider("explicit-read");
        read_key_with(&store, &p, false).unwrap_err();
        // "Read key again" in Settings is the one path that may ask the OS twice.
        read_key_with(&store, &p, true).unwrap_err();
        assert_eq!(store.reads(), 2);
    }

    #[test]
    fn a_saved_key_replaces_the_remembered_verdict() {
        let account = "test:cache:account";
        remember_verdict(account, KeyVerdict::Ready(Secret::new("sk-cached")));
        assert_eq!(
            cached_verdict(account)
                .unwrap()
                .result()
                .unwrap()
                .unwrap()
                .as_str(),
            "sk-cached"
        );
        // Removing a key records absence, so the next turn does not ask the OS.
        remember_verdict(account, KeyVerdict::Absent);
        assert!(cached_verdict(account).unwrap().result().unwrap().is_none());
        remember_verdict(account, KeyVerdict::Refused("refused".into()));
        assert_eq!(
            cached_verdict(account).unwrap().result().unwrap_err(),
            "refused"
        );
    }

    /// A reloaded window asks what the backend still runs. The turn names the
    /// conversation only once that is resolved, one request is in flight at a
    /// time, and cancelling it is reaching the request the answer names.
    #[test]
    fn a_running_turn_names_its_conversation_and_holds_one_request() {
        let requests = Requests::default();
        assert_eq!(requests.active().unwrap(), None);

        let RequestClaim {
            cancel_rx: mut cancelled,
            ..
        } = requests.claim("r1").unwrap().expect("the slot is free");
        assert_eq!(
            requests.active().unwrap(),
            Some(ActiveTurn {
                request_id: "r1".into(),
                session_id: None,
            })
        );
        // A second request finds the slot taken, which is the panel's one turn.
        assert!(requests.claim("r2").unwrap().is_none());

        requests.set_session("r1", "s1");
        assert_eq!(
            requests.active().unwrap().unwrap().session_id.as_deref(),
            Some("s1")
        );

        let cancel = requests.release("r1").unwrap().expect("the turn runs");
        cancel.send(()).unwrap();
        assert_eq!(cancelled.try_recv(), Ok(()));
        assert_eq!(requests.active().unwrap(), None);
        // A request that ended before it named a conversation stays gone.
        requests.set_session("r1", "s2");
        assert_eq!(requests.active().unwrap(), None);
    }

    #[test]
    fn an_environment_key_is_trimmed_and_an_empty_one_is_ignored() {
        assert_eq!(
            environment_key_from(Some("  sk-env  ")).unwrap().as_str(),
            "sk-env"
        );
        assert!(environment_key_from(Some("   ")).is_none());
        assert!(environment_key_from(Some("")).is_none());
        assert!(environment_key_from(None).is_none());
    }
}

#[cfg(test)]
mod listening_budget_tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;

    fn clip(bytes: usize) -> Value {
        json!({"role":"user", "content":"Requested listening clip", "input_audio":{
            "format":"wav", "data":base64::engine::general_purpose::STANDARD.encode(vec![0u8;bytes])
        }})
    }

    #[test]
    fn two_listening_clips_use_media_budget_instead_of_text_budget() {
        let messages = json!([clip(960_044), clip(960_044)]);
        assert!(messages.to_string().len() > LIMIT);
        validate_chat_messages(&messages).unwrap();
        let turns = ai::turns_from_wire(&messages).unwrap();
        assert_eq!(turns.len(), 2);
        for turn in turns {
            assert!(
                turn.blocks
                    .iter()
                    .any(|block| matches!(block, ai::Block::Media { .. }))
            );
        }
    }

    #[test]
    fn text_and_malformed_audio_cannot_bypass_their_limits() {
        assert!(
            validate_chat_messages(&json!([{"role":"user","content":"x".repeat(LIMIT+1)}]))
                .is_err()
        );
        assert!(validate_chat_messages(&json!([{"role":"user","content":"", "input_audio":{"format":"wav","data":"not-base64!"}}])).is_err());
        let mut wrong_role = clip(4);
        wrong_role["role"] = json!("assistant");
        assert!(validate_chat_messages(&json!([wrong_role])).is_err());
        let mut wrong_format = clip(4);
        wrong_format["input_audio"]["format"] = json!("txt");
        assert!(validate_chat_messages(&json!([wrong_format])).is_err());
    }

    #[test]
    fn combined_listening_payload_stays_bounded() {
        let messages = json!([clip(8_000_000), clip(8_000_000)]);
        assert!(validate_chat_messages(&messages).is_err());
    }
}

#[cfg(test)]
mod wire_budget_tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    const RESULTS_PER_ROUND: usize = 8;

    /// The fold `agent.ts` performs: the leading system rows and the first user
    /// row stay, the newest `RETAINED_TOOL_PAIRS` calls stay whole, and every
    /// older pair becomes one summary row. Mirrored here so the validator is
    /// exercised against the shape the panel actually sends.
    fn folded(rows: &[Value]) -> Value {
        let mut head = 0;
        while head < rows.len() && rows[head]["role"] == "system" {
            head += 1;
        }
        if let Some(user) = rows.iter().position(|row| row["role"] == "user") {
            head = user + 1;
        }
        let starts: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row["role"] == "assistant"
                    && row["tool_calls"]
                        .as_array()
                        .is_some_and(|calls| !calls.is_empty())
            })
            .map(|(index, _)| index)
            .collect();
        let keep = ai::sessions::RETAINED_TOOL_PAIRS.min(starts.len());
        let cut = starts
            .get(starts.len().saturating_sub(keep))
            .copied()
            .filter(|cut| *cut > head)
            .unwrap_or(head);
        let dropped = starts
            .iter()
            .filter(|start| **start >= head && **start < cut)
            .count();
        let mut out: Vec<Value> = rows[..head].to_vec();
        out.push(json!({
            "role": "user",
            "context": "summary",
            "content": format!(
                "Wire compaction: {dropped} older tool call/result pairs were folded into this line so the request stays inside the provider's message budget."
            ),
        }));
        out.extend_from_slice(&rows[cut..]);
        Value::Array(out)
    }

    /// One round the model asked for: the assistant call and its results.
    fn round(index: usize) -> Vec<Value> {
        let calls: Vec<Value> = (0..RESULTS_PER_ROUND)
            .map(|call| {
                json!({
                    "id": format!("r{index}-{call}"),
                    "type": "function",
                    "function": {"name": "tracks_list", "arguments": "{}"},
                })
            })
            .collect();
        let mut rows = vec![json!({"role": "assistant", "content": null, "tool_calls": calls})];
        rows.extend((0..RESULTS_PER_ROUND).map(|call| {
            json!({
                "role": "tool",
                "tool_call_id": format!("r{index}-{call}"),
                "content": "y".repeat(7_000),
            })
        }));
        rows
    }

    /// Every call a wire carries, with the results that answer it.
    fn answered(rows: &[Value]) -> HashSet<String> {
        let calls: HashSet<String> = rows
            .iter()
            .filter_map(|row| row["tool_calls"].as_array())
            .flatten()
            .filter_map(|call| call["id"].as_str())
            .map(str::to_owned)
            .collect();
        let results: HashSet<String> = rows
            .iter()
            .filter(|row| row["role"] == "tool")
            .filter_map(|row| row["tool_call_id"].as_str())
            .map(str::to_owned)
            .collect();
        assert_eq!(calls, results, "a call and its result were split");
        calls
    }

    #[test]
    fn the_validator_refuses_a_long_turn_that_is_never_folded() {
        let mut wire = vec![json!({"role": "user", "content": "keep reading the project"})];
        let mut refused = None;
        for index in 0..16 {
            let rows = wire.len();
            if rows > ai::MAX_WIRE_MESSAGES {
                refused = Some((index, validate_chat_messages(&Value::Array(wire))));
                break;
            }
            wire.extend(round(index));
        }
        let (index, outcome) = refused.expect("16 rounds must reach the message bound");
        let error = outcome.expect_err("the unfolded wire must be refused");
        assert_eq!(error.kind, "config");
        assert!(!error.retryable);
        assert!(index < 16);
    }

    #[test]
    fn the_validator_accepts_every_wire_the_loop_folds() {
        let mut wire = Value::Array(vec![
            json!({"role": "user", "content": "keep reading the project"}),
        ]);
        let mut folds = 0;
        for index in 0..16 {
            let rows = wire.as_array().unwrap();
            let fold_now = rows.len() > ai::MAX_WIRE_MESSAGES - 8;
            if fold_now {
                // The wire the loop is about to fold is still one the validator
                // accepts; the next round is the one that would not be.
                validate_chat_messages(&wire).unwrap_or_else(|error| {
                    panic!("round {index} was over the budget: {}", error.message)
                });
                wire = folded(rows);
                folds += 1;
            }
            validate_chat_messages(&wire).unwrap_or_else(|error| {
                panic!("round {index} folded wire refused: {}", error.message)
            });
            let rows = wire.as_array().unwrap();
            assert!(rows.len() <= ai::MAX_WIRE_MESSAGES);
            assert_eq!(rows[0]["role"], "user");
            assert_eq!(rows[0]["content"], "keep reading the project");
            let calls = answered(rows);
            if fold_now {
                // The fold keeps the newest pairs whole and nothing older.
                assert_eq!(
                    calls.len(),
                    ai::sessions::RETAINED_TOOL_PAIRS * RESULTS_PER_ROUND
                );
            }
            let mut next = rows.to_vec();
            next.extend(round(index));
            wire = Value::Array(next);
        }
        assert!(folds > 0, "the fixture never needed a fold");
    }
}

#[cfg(test)]
mod headless_request_tests {
    use super::*;

    #[test]
    fn a_headless_request_owns_the_shared_slot_until_its_finalizer_drops() {
        let requests = Requests::default();
        let mut guard = requests
            .claim_headless("http", Some("conversation"))
            .unwrap();
        assert_eq!(
            requests.active().unwrap().unwrap().session_id.as_deref(),
            Some("conversation")
        );
        assert!(requests.claim("panel").unwrap().is_none());
        tauri::async_runtime::block_on(guard.run(async { Ok::<_, AiError>(()) })).unwrap();
        assert!(
            requests.claim("panel").unwrap().is_none(),
            "reply persistence still owns the slot"
        );
        drop(guard);
        assert!(requests.active().unwrap().is_none());
    }

    #[test]
    fn cancelling_a_headless_request_does_not_release_its_replacement() {
        let requests = Requests::default();
        let mut guard = requests
            .claim_headless("same-id", Some("conversation"))
            .unwrap();
        requests
            .release("same-id")
            .unwrap()
            .unwrap()
            .send(())
            .unwrap();
        let replacement = requests
            .claim_headless("same-id", Some("next-conversation"))
            .unwrap();
        let error = tauri::async_runtime::block_on(
            guard.run(std::future::pending::<Result<(), AiError>>()),
        )
        .unwrap_err();
        assert_eq!(error.kind, "cancelled");
        drop(guard);
        assert_eq!(
            requests.active().unwrap().unwrap().session_id.as_deref(),
            Some("next-conversation")
        );
        drop(replacement);
        assert!(requests.active().unwrap().is_none());
    }
}
