//! Sample marketplaces: the agent tools and the panel's backend.
//!
//! This module is a client of other people's catalogues. `sample_search` asks
//! several markets at once and reports every provider it knows, available or
//! not; `sample_pull` resolves one id and writes one file into
//! `<project>/media/samples/` beside a `.json` sidecar that carries its licence
//! and credit line.
//!
//! Only three decisions live here: where a project keeps its samples, where a
//! market's key is found, and how a refusal is shaped for a tool result. The
//! catalogue work itself is [`oc_sample`].

use std::path::{Path, PathBuf};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

use oc_sample::http::HttpFetch;
use oc_sample::model::{FailureKind, Quality, SampleError, SampleItem, SampleKind, SampleQuery};
use oc_sample::provider::{SampleFetch, SampleProvider};
use oc_sample::provider_for_with;
use oc_sample::registry::{self, ProviderKeys};
use oc_sample::runtime::{LocalRuntime, ProviderRuntime};
use oc_sample::store::{SampleReceipt, SampleStore};

use crate::ai::credential_store::{self, KeyringSecrets};
use crate::state::AppState;

mod breaker;
mod cursor;
pub(crate) mod local;
mod merge;
pub(crate) mod preview_cache;

use breaker::{Breakers, Observation, SkipReason};
use cursor::{CursorError, SearchCursor};

/// Folder a pull writes into, relative to the project file.
pub const SAMPLES_SUBDIR: &str = "media/samples";
/// Keyring service for sample-market keys; the account is the provider id.
pub(crate) const KEY_SERVICE: &str = "org.opencompose.samples";
/// Environment overrides so a headless session needs no keychain.
const JAMENDO_KEY_ENV: &str = "JAMENDO_CLIENT_ID";

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SearchArgs {
    #[schemars(description = "Words to search for, e.g. snare, vinyl loop, choir ahh")]
    pub(crate) query: String,
    #[schemars(description = "Optional kind filter: drum, loop, vocal, fx, instrument, any")]
    pub(crate) kind: Option<String>,
    #[schemars(description = "Optional inclusive lower bound on duration in seconds")]
    pub(crate) min_seconds: Option<f64>,
    #[schemars(description = "Optional inclusive upper bound on duration in seconds")]
    pub(crate) max_seconds: Option<f64>,
    #[schemars(description = "Optional channel count, usually 1 for mono or 2 for stereo")]
    pub(crate) channels: Option<u16>,
    #[serde(default)]
    #[schemars(description = "Optional tags a result must relate to, such as acoustic or drums")]
    pub(crate) tags: Vec<String>,
    #[schemars(
        description = "Deprecated single-market alias for providers, kept for older callers"
    )]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Market ids to search, e.g. [\"openverse\", \"internet-archive\"]. Omit to search every available market. Unknown ids are reported in the result."
    )]
    pub(crate) providers: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Result offset for every market, default 0. Prefer cursor; next_offset is kept for older callers"
    )]
    pub(crate) offset: usize,
    #[serde(default = "default_search_limit")]
    #[schemars(description = "Results per page per market, default 20, clamped to 1..50")]
    pub(crate) limit: usize,
    #[serde(default)]
    #[schemars(
        description = "Keep only results whose licence allows commercial use; non-commercial and unclassified licences are dropped. Default false: no licence filter"
    )]
    pub(crate) commercial_ok: bool,
    #[schemars(
        description = "Opaque cursor from the previous result's cursor field. Continues each market that still has results at its own position and overrides offset"
    )]
    pub(crate) cursor: Option<String>,
}

fn default_search_limit() -> usize {
    20
}

impl ToolArguments for SearchArgs {
    const ID: ToolId = ToolId::SampleSearch;
    const FIELD_ORDER: &'static [&'static str] = &[
        "query",
        "kind",
        "min_seconds",
        "max_seconds",
        "channels",
        "tags",
        "provider",
        "providers",
        "offset",
        "limit",
        "commercial_ok",
        "cursor",
    ];
}

/// Keys resolved once per call. Owned strings, so the borrowed view passed to
/// `oc_sample` never outlives them.
pub(crate) struct Keys {
    freesound: Option<String>,
    /// An OAuth2 access token, which unlocks Freesound's original file.
    freesound_token: Option<String>,
    jamendo: Option<String>,
    pixabay: Option<String>,
}

impl Keys {
    /// The Freesound access token, when one is stored.
    pub(crate) fn freesound_token(&self) -> Option<&str> {
        self.freesound_token.as_deref()
    }

    pub(crate) fn view(&self) -> ProviderKeys<'_> {
        ProviderKeys {
            freesound: self.freesound.as_deref(),
            jamendo: self.jamendo.as_deref(),
            pixabay: self.pixabay.as_deref(),
        }
    }
}

/// A saved key, from the environment first so a headless session works.
fn saved_key(account: &str, env: &str) -> Option<String> {
    if let Ok(key) = std::env::var(env) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    let store = KeyringSecrets::new(KEY_SERVICE);
    match credential_store::load_text(&store, account) {
        Ok(secret) => secret.map(|secret| secret.as_str().to_string()),
        Err(error) => {
            log::warn!("[samples] the {account} key is unreadable: {error:?}");
            None
        }
    }
}

/// Report the source of a configured key without returning its value.
pub(crate) fn key_status(provider: &str) -> Result<&'static str, String> {
    let (account, env) = key_configuration(provider)?;
    if std::env::var(env).is_ok_and(|key| !key.trim().is_empty()) {
        return Ok("environment");
    }
    let store = KeyringSecrets::new(KEY_SERVICE);
    credential_store::load_text(&store, account)
        .map(|key| if key.is_some() { "saved" } else { "missing" })
        .map_err(|error| format!("the key could not be read: {error:?}"))
}

pub(crate) fn remove_key(provider: &str) -> Result<(), String> {
    let (account, _) = key_configuration(provider)?;
    credential_store::delete_entry(&KeyringSecrets::new(KEY_SERVICE), account)
        .map_err(|error| format!("the key could not be removed: {error:?}"))
}

fn key_configuration(provider: &str) -> Result<(&'static str, &'static str), String> {
    let spec =
        registry::spec(provider).ok_or_else(|| format!("Unknown sample provider '{provider}'."))?;
    match (spec.key_account, spec.key_env) {
        (Some(account), Some(env)) if spec.search => Ok((account, env)),
        _ => Err(format!(
            "{} has no configurable search credentials.",
            spec.label
        )),
    }
}

/// Save a market's key in the OS keyring, under the service and account the
/// search reads back.
pub(crate) fn save_key(provider: &str, value: &str) -> Result<(), String> {
    let (account, _) = key_configuration(provider)?;
    let value = value.trim();
    if value.is_empty() {
        return Err("The key is empty.".to_string());
    }
    let store = KeyringSecrets::new(KEY_SERVICE);
    credential_store::store_text(&store, account, value)
        .map_err(|error| format!("the key could not be saved: {error:?}"))
}

/// Every key this build can use, resolved once.
pub(crate) fn load_keys() -> Keys {
    Keys {
        freesound: oc_sample::local_plugin::is_installed("freesound")
            .then(|| saved_key("freesound", oc_sample::FREESOUND_KEY_ENV))
            .flatten(),
        freesound_token: oc_sample::local_plugin::is_installed("freesound")
            .then(crate::commands::freesound_oauth::access_token)
            .flatten(),
        jamendo: oc_sample::local_plugin::is_installed("jamendo")
            .then(|| saved_key("jamendo", JAMENDO_KEY_ENV))
            .flatten(),
        pixabay: None,
    }
}

/// The one HTTP client a tool call or a panel command builds.
/// The proxy preference as the sample crate wants it, with the URL owned for
/// the duration of the call.
enum Proxy {
    System,
    Manual(String),
    None,
}

fn proxy_preference() -> Proxy {
    let network = crate::gui_settings::load().network;
    match network.proxy_mode {
        crate::gui_settings::ProxyMode::Manual => match network.manual_url() {
            Some(url) => Proxy::Manual(url.to_string()),
            None => Proxy::System,
        },
        crate::gui_settings::ProxyMode::None => Proxy::None,
        crate::gui_settings::ProxyMode::System => Proxy::System,
    }
}

fn build_fetch(timeout: std::time::Duration) -> Result<HttpFetch, String> {
    let proxy = proxy_preference();
    let choice = match &proxy {
        Proxy::System => oc_sample::http::ProxyChoice::System,
        Proxy::Manual(url) => oc_sample::http::ProxyChoice::Manual(url),
        Proxy::None => oc_sample::http::ProxyChoice::None,
    };
    HttpFetch::with_proxy(timeout, choice).map_err(|error| format!("network: {error}"))
}

/// The client for a resolve and download, with the shared download deadline.
pub(crate) fn http_fetch() -> Result<HttpFetch, String> {
    build_fetch(oc_sample::http::DOWNLOAD_TIMEOUT)
}

/// The client for a search, with the shared search deadline: a market that is
/// down must not hold every other market's results for the long one.
fn search_fetch() -> Result<HttpFetch, String> {
    build_fetch(oc_sample::http::SEARCH_TIMEOUT)
}

/// What a page's `error_kind` says about a market that returned no results.
/// The wire spelling is snake_case; the panel reads it instead of `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PageErrorKind {
    /// The market needs a key, or rejected the one it was given.
    NeedsKey,
    /// The market cannot be searched in this build.
    ProviderUnavailable,
    /// The market was not asked; `skip_reason` says why.
    Skipped,
    RateLimited,
    NotFound,
    Unavailable,
    Timeout,
    Network,
    Parse,
    Other,
}

impl PageErrorKind {
    /// The kind for a provider's error. Refusal codes are stable wire codes,
    /// so they are matched as identifiers, never as text.
    fn of(error: &SampleError) -> Self {
        match error {
            SampleError::Refused {
                code: "key_required",
                ..
            } => PageErrorKind::NeedsKey,
            SampleError::Refused {
                code: "provider_unavailable" | "provider_unknown",
                ..
            } => PageErrorKind::ProviderUnavailable,
            SampleError::Refused { .. } => PageErrorKind::Other,
            SampleError::Failed(failure) => match failure.kind {
                FailureKind::RateLimited => PageErrorKind::RateLimited,
                FailureKind::Unauthorized => PageErrorKind::NeedsKey,
                FailureKind::NotFound => PageErrorKind::NotFound,
                FailureKind::Unavailable => PageErrorKind::Unavailable,
                FailureKind::Timeout => PageErrorKind::Timeout,
                FailureKind::Network => PageErrorKind::Network,
                FailureKind::Parse => PageErrorKind::Parse,
                FailureKind::Other => PageErrorKind::Other,
            },
        }
    }
}

/// One market's page entry when it returned no results.
struct PageFailure<'a> {
    kind: PageErrorKind,
    message: &'a str,
    retry_after_secs: Option<u64>,
    skip_reason: Option<SkipReason>,
    http_status: Option<u16>,
}

fn failed_page(
    provider: &str,
    label: &str,
    offset: Option<usize>,
    failure: PageFailure<'_>,
) -> Value {
    let mut page = json!({
        "provider": provider,
        "provider_name": label,
        "offset": offset,
        "count": 0,
        "next_offset": Value::Null,
        "error": failure.message,
        "error_kind": failure.kind,
        "skip_reason": failure.skip_reason,
        "retry_after_secs": failure.retry_after_secs,
        "http_status": failure.http_status,
    });
    if failure.skip_reason.is_some() {
        page["skipped"] = json!(true);
    }
    page
}

/// One market's answer, or the reason it could not answer. `missing` carries a
/// provider row for an id no spec knows, which the caller reports beside the
/// market list instead of as a page.
struct MarketOutcome {
    /// The registry id, which the cursor records.
    provider: Option<&'static str>,
    label: &'static str,
    page: Value,
    items: Vec<SampleItem>,
    next_offset: Option<usize>,
    missing: Option<Value>,
}

impl MarketOutcome {
    fn failed(spec: &oc_sample::registry::ProviderSpec, page: Value) -> Self {
        Self {
            provider: Some(spec.id),
            label: spec.label,
            page,
            items: Vec::new(),
            next_offset: None,
            missing: None,
        }
    }
}

fn search_market(
    id: &str,
    query: &SampleQuery,
    keys: ProviderKeys<'_>,
    fetch: &dyn SampleFetch,
    runtime: &dyn ProviderRuntime,
    breakers: &Breakers,
) -> MarketOutcome {
    let Some(spec) = registry::spec(id) else {
        return MarketOutcome {
            provider: None,
            label: "",
            page: Value::Null,
            items: Vec::new(),
            next_offset: None,
            missing: Some(json!({
                "id": id,
                "label": Value::Null,
                "requires_key": false,
                "available": false,
                "reason": "not a provider this build offers",
            })),
        };
    };
    let row = registry::availability_of_with(spec, keys, runtime);
    let provider = match provider_for_with(Some(spec.id), keys, runtime) {
        Ok(provider) if row.available => provider,
        result => {
            let kind = match &result {
                Err(error) => PageErrorKind::of(error),
                Ok(_) => PageErrorKind::ProviderUnavailable,
            };
            let message = match (&row.reason, &result) {
                (Some(reason), _) if !row.available => reason.clone(),
                (_, Err(error)) => error.message().to_string(),
                _ => "not available".to_string(),
            };
            return MarketOutcome::failed(
                spec,
                failed_page(
                    spec.id,
                    spec.label,
                    None,
                    PageFailure {
                        kind,
                        message: &message,
                        retry_after_secs: None,
                        skip_reason: None,
                        http_status: None,
                    },
                ),
            );
        }
    };
    if let Some(skip) = breakers.skip(spec.id, Instant::now()) {
        // Rounded up, so a caller that waits this long finds the market open.
        let wait = (skip.remaining.as_secs() + u64::from(skip.remaining.subsec_nanos() > 0)).max(1);
        let message = match skip.reason {
            SkipReason::CircuitOpen => format!(
                "skipped after {} consecutive failures; retrying in {wait}s",
                breaker::BREAKER_FAILURES
            ),
            SkipReason::RateLimited => {
                format!("skipped while the market asks to slow down; retrying in {wait}s")
            }
        };
        return MarketOutcome::failed(
            spec,
            failed_page(
                spec.id,
                spec.label,
                Some(query.offset),
                PageFailure {
                    kind: PageErrorKind::Skipped,
                    message: &message,
                    retry_after_secs: Some(wait),
                    skip_reason: Some(skip.reason),
                    http_status: None,
                },
            ),
        );
    }
    match provider.search(query, fetch) {
        Ok(page) => {
            breakers.observe(spec.id, Observation::Answered, Instant::now());
            MarketOutcome {
                provider: Some(spec.id),
                label: spec.label,
                page: json!({
                    "provider": page.provider,
                    "provider_name": provider.label(),
                    "offset": query.offset,
                    "count": page.items.len(),
                    "total": page.total,
                    "next_offset": page.next_offset,
                    "error": Value::Null,
                    "error_kind": Value::Null,
                    "skip_reason": Value::Null,
                    "retry_after_secs": Value::Null,
                    "http_status": Value::Null,
                }),
                next_offset: page.next_offset,
                items: page.items,
                missing: None,
            }
        }
        Err(error) => {
            let failure = error.failure_detail();
            if let Some(failure) = failure {
                breakers.observe(
                    spec.id,
                    Observation::Failed {
                        kind: failure.kind,
                        retry_after_secs: failure.retry_after_secs,
                    },
                    Instant::now(),
                );
            }
            // A throttled market reports the wait this search applies, which
            // is the source's `Retry-After` bounded, or the default.
            let retry_after_secs = match failure {
                Some(failure) if failure.kind == FailureKind::RateLimited => {
                    Some(breaker::rate_limit_wait(failure.retry_after_secs).as_secs())
                }
                Some(failure) => failure.retry_after_secs,
                None => None,
            };
            MarketOutcome::failed(
                spec,
                failed_page(
                    provider.id(),
                    provider.label(),
                    Some(query.offset),
                    PageFailure {
                        kind: PageErrorKind::of(&error),
                        message: error.message(),
                        retry_after_secs,
                        skip_reason: None,
                        http_status: failure.and_then(|failure| failure.status),
                    },
                ),
            )
        }
    }
}

/// `sample_search` for the agent tool: one page across the markets the caller
/// names, or every available market. Providers that cannot answer are
/// reported in the result, never dropped; a refusal reads as its sentence.
pub(crate) fn search_request(request: &SearchArgs) -> Result<Value, String> {
    search_request_typed(request).map_err(|error| error.message().to_string())
}

/// The panel's search: a refusal keeps its code.
pub(crate) fn search_request_typed(request: &SearchArgs) -> Result<Value, SampleError> {
    let keys = load_keys();
    let fetch = search_fetch().map_err(SampleError::failed)?;
    search_with_request(
        request,
        &keys,
        &fetch,
        &LocalRuntime::from_process_configuration(),
        breaker::shared(),
    )
}

#[cfg(test)]
fn search_with(
    args: &Value,
    keys: &Keys,
    fetch: &dyn SampleFetch,
    runtime: &dyn ProviderRuntime,
) -> Result<Value, String> {
    let request = SearchArgs::decode(args).map_err(|error| error.to_string())?;
    search_with_request(&request, keys, fetch, runtime, &Breakers::default())
        .map_err(|error| error.message().to_string())
}

/// The market a search asks next, and the provider offset it starts at.
struct Planned {
    id: String,
    offset: usize,
}

/// The markets this request asks, in order. Without a cursor every requested
/// market starts at `offset`. With one, only the markets it names are asked,
/// each at its own offset, and an explicit `providers` list narrows them.
fn plan_markets(
    args: &SearchArgs,
    cursor: Option<&SearchCursor>,
    offset: usize,
    keys: ProviderKeys<'_>,
    runtime: &dyn ProviderRuntime,
) -> Vec<Planned> {
    let requested = requested_providers(args, keys, runtime);
    let Some(cursor) = cursor else {
        return requested
            .into_iter()
            .map(|id| Planned { id, offset })
            .collect();
    };
    let explicit = !args.providers.is_empty() || args.provider.is_some();
    let named: Vec<&'static str> = requested
        .iter()
        .filter_map(|id| registry::spec(id).map(|spec| spec.id))
        .collect();
    cursor
        .markets()
        .filter(|market| !explicit || named.contains(market))
        .filter_map(|market| {
            cursor.offset_of(market).map(|offset| Planned {
                id: market.to_string(),
                offset,
            })
        })
        .collect()
}

fn search_with_request(
    args: &SearchArgs,
    keys: &Keys,
    fetch: &dyn SampleFetch,
    runtime: &dyn ProviderRuntime,
    breakers: &Breakers,
) -> Result<Value, SampleError> {
    let kind = match args
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    {
        Some(value) => Some(SampleKind::parse(value).ok_or_else(|| {
            SampleError::refused(
                "invalid_kind",
                format!(
                    "unknown sample kind '{value}'; accepted: drum, loop, vocal, fx, instrument, any"
                ),
            )
        })?),
        None => None,
    };
    let cursor = match args
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
    {
        Some(text) => Some(
            SearchCursor::decode(text)
                .map_err(|error| SampleError::refused(CursorError::CODE, error.message()))?,
        ),
        None => None,
    };
    let keys_view = keys.view();
    let base = SampleQuery {
        text: args.query.clone(),
        kind,
        min_seconds: args.min_seconds,
        max_seconds: args.max_seconds,
        channels: args.channels,
        tags: args.tags.clone(),
        offset: args.offset,
        limit: args.limit,
        commercial_ok: args.commercial_ok,
    }
    .normalized();
    let plan = plan_markets(args, cursor.as_ref(), base.offset, keys_view, runtime);
    let mut providers: Vec<Value> = registry::availability_with(keys_view, runtime)
        .into_iter()
        .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
        .collect();
    // Markets answer in parallel: one that is down must not delay the rest by
    // its own timeout, and the requested order is kept for the result.
    let outcomes: Vec<MarketOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = plan
            .iter()
            .map(|planned| {
                let query = SampleQuery {
                    offset: planned.offset,
                    ..base.clone()
                };
                let id = planned.id.as_str();
                scope.spawn(move || search_market(id, &query, keys_view, fetch, runtime, breakers))
            })
            .collect();
        handles
            .into_iter()
            .zip(&plan)
            .map(|(handle, planned)| {
                handle.join().unwrap_or_else(|_| {
                    let spec = registry::spec(&planned.id);
                    MarketOutcome {
                        provider: spec.map(|spec| spec.id),
                        label: spec.map_or("", |spec| spec.label),
                        page: failed_page(
                            &planned.id,
                            spec.map_or("", |spec| spec.label),
                            Some(planned.offset),
                            PageFailure {
                                kind: PageErrorKind::Other,
                                message: "the market's search panicked",
                                retry_after_secs: None,
                                skip_reason: None,
                                http_status: None,
                            },
                        ),
                        items: Vec::new(),
                        next_offset: None,
                        missing: None,
                    }
                })
            })
            .collect()
    });
    let mut pages: Vec<Value> = Vec::new();
    let mut lists: Vec<Vec<SampleItem>> = Vec::new();
    let mut labels: Vec<&'static str> = Vec::new();
    let mut next = SearchCursor::default();
    let mut more = false;
    let mut raw_count = 0usize;
    for outcome in outcomes {
        if let Some(missing) = outcome.missing {
            providers.push(missing);
            continue;
        }
        if let (Some(provider), Some(offset)) = (outcome.provider, outcome.next_offset) {
            more = true;
            next.push(provider, offset);
        }
        raw_count += outcome.items.len();
        pages.push(outcome.page);
        labels.push(outcome.label);
        lists.push(outcome.items);
    }
    let merged = merge::interleave(lists);
    let duplicates = raw_count - merged.len();
    let items: Vec<Value> = merged
        .iter()
        .map(|(index, item)| item_json(item, labels[*index]))
        .collect();
    // The shared offset only means something when every market started at it;
    // a cursor search continues through `cursor` alone.
    let next_offset =
        (more && cursor.is_none()).then(|| base.offset.saturating_add(base.page_size()));
    Ok(json!({
        "status": "ok",
        "query": base.text,
        "kind": kind.map(|kind| kind.as_str()),
        "commercial_ok": base.commercial_ok,
        "offset": base.offset,
        "limit": base.page_size(),
        "providers": providers,
        "pages": pages,
        "count": items.len(),
        "duplicates": duplicates,
        "next_offset": next_offset,
        "cursor": next.encode(),
        "items": items,
    }))
}

/// The markets a search asks. An explicit list keeps unknown ids so the result
/// can report them; an empty list means every market that can answer now.
fn requested_providers(
    args: &SearchArgs,
    keys: ProviderKeys<'_>,
    runtime: &dyn ProviderRuntime,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        let id = id.trim();
        if id.is_empty() || ids.iter().any(|seen| seen.eq_ignore_ascii_case(id)) {
            return;
        }
        ids.push(id.to_string());
    };
    for id in &args.providers {
        push(id);
    }
    if let Some(id) = &args.provider {
        push(id);
    }
    if ids.is_empty() {
        // Openverse search is slow and often times out, so it is not part of the
        // default fan-out; naming it still searches it.
        return registry::default_search_ids_with(keys, runtime)
            .into_iter()
            .map(str::to_string)
            .collect();
    }
    ids
}

/// `sample_pull`: resolve one id and download it into the project.
pub fn pull(
    state: &AppState,
    request: &crate::agent_tools::sample::SamplePullArgs,
) -> Result<Value, String> {
    let keys = load_keys();
    pull_with(
        state,
        request,
        &keys,
        &LocalRuntime::from_process_configuration(),
    )
}

fn pull_with(
    state: &AppState,
    request: &crate::agent_tools::sample::SamplePullArgs,
    keys: &Keys,
    runtime: &dyn ProviderRuntime,
) -> Result<Value, String> {
    if request.id.trim().is_empty() {
        return Err("sample_pull needs an id from sample_search".into());
    }
    let provider = match provider_for_args(
        request.provider.as_deref(),
        request.id.trim(),
        keys,
        runtime,
    ) {
        Ok(provider) => provider,
        Err(error) => return Ok(error.to_json()),
    };
    let project_root = project_root(state);
    let dir = match samples_dir(project_root.as_deref(), request.dest.as_deref()) {
        Ok(dir) => dir,
        Err(error) => return Ok(error.to_json()),
    };
    let fetch = http_fetch()?;
    let item = match provider.resolve(request.id.trim(), &fetch) {
        Ok(item) => item,
        Err(error @ SampleError::Refused { .. }) => return Ok(error.to_json()),
        Err(error) => return Err(error.message().to_string()),
    };
    let store = SampleStore::new(&dir);
    let receipt = match pull_item(
        &store,
        &item,
        &fetch,
        request.accept_licence,
        keys.freesound_token(),
    ) {
        Ok(receipt) => receipt,
        Err(error @ SampleError::Refused { .. }) => return Ok(error.to_json()),
        Err(error) => return Err(error.message().to_string()),
    };
    Ok(pulled_json(
        &receipt,
        project_root.as_deref(),
        provider.as_ref(),
    ))
}

/// Pull one item. A Freesound item with a token downloads the original file;
/// anything else keeps the URL the provider resolved.
///
/// A refused original falls back to the preview the search returned, so a pull
/// still lands a legal clip. The receipt keeps `quality: preview_mp3` unless
/// the original bytes were actually fetched.
pub(crate) fn pull_item(
    store: &SampleStore,
    item: &SampleItem,
    fetch: &dyn SampleFetch,
    accept_licence: bool,
    token: Option<&str>,
) -> Result<SampleReceipt, SampleError> {
    pull_item_with(store, item, fetch, accept_licence, token, || {
        crate::commands::freesound_oauth::refresh_access_token()
    })
}

fn pull_item_with(
    store: &SampleStore,
    item: &SampleItem,
    fetch: &dyn SampleFetch,
    accept_licence: bool,
    token: Option<&str>,
    refresh: impl Fn() -> Result<String, String>,
) -> Result<SampleReceipt, SampleError> {
    let token = token
        .map(str::trim)
        .filter(|token| !token.is_empty() && item.provider == "freesound");
    let Some(token) = token else {
        return store.pull(item, fetch, accept_licence);
    };
    let mut original = item.clone();
    original.download_url = freesound_original_url(&original.id);
    original.quality = Quality::Original;
    let bearer = format!("Bearer {token}");
    let first = store.pull_with_headers(
        &original,
        fetch,
        accept_licence,
        &[("Authorization", &bearer)],
    );
    let original = match first {
        Ok(receipt) => return Ok(receipt),
        Err(error) if is_unauthorized(&error) => match refresh() {
            Ok(fresh) => {
                let bearer = format!("Bearer {fresh}");
                store.pull_with_headers(
                    &original,
                    fetch,
                    accept_licence,
                    &[("Authorization", &bearer)],
                )
            }
            Err(_) => Err(error),
        },
        Err(error) => Err(error),
    };
    match original {
        Ok(receipt) => Ok(receipt),
        Err(original_error) => {
            log::warn!("[samples] the Freesound original was refused; pulling the preview");
            match store.pull(item, fetch, accept_licence) {
                Ok(receipt) => Ok(receipt),
                Err(_) => Err(original_error),
            }
        }
    }
}

/// Freesound's authenticated download endpoint, which serves the original.
fn freesound_original_url(id: &str) -> String {
    let number = id.trim_start_matches("freesound:");
    format!("https://freesound.org/apiv2/sounds/{number}/download/")
}

/// A 401 is the one refusal worth a refresh; every other status stays the
/// reason the original was not used.
fn is_unauthorized(error: &SampleError) -> bool {
    error
        .failure_detail()
        .is_some_and(|failure| failure.status == Some(401))
}

/// Resolve the market a pull names or the id implies. An unknown market is
/// `provider_unknown`; a page-only one is `provider_unavailable`.
fn provider_for_args(
    requested: Option<&str>,
    id: &str,
    keys: &Keys,
    runtime: &dyn ProviderRuntime,
) -> Result<Box<dyn SampleProvider>, SampleError> {
    let named = requested.map(str::trim).filter(|id| !id.is_empty());
    let market = named
        .map(str::to_string)
        .or_else(|| oc_sample::provider_of_id(id).map(|spec| spec.id.to_string()));
    let Some(market) = market else {
        return Err(SampleError::refused(
            "provider_unknown",
            format!(
                "Cannot tell which market '{id}' came from. Pass the provider from the search result, for example \"provider\": \"openverse\"."
            ),
        ));
    };
    provider_for_with(Some(&market), keys.view(), runtime)
}

fn pulled_json(
    receipt: &SampleReceipt,
    project_root: Option<&Path>,
    provider: &dyn SampleProvider,
) -> Value {
    let media_path = project_root
        .and_then(|root| Path::new(&receipt.path).strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let mut value = json!({
        "status": "ok",
        "provider": receipt.provider,
        "provider_name": provider.label(),
        "id": receipt.id,
        "name": receipt.name,
        "kind": receipt.kind,
        "duration_seconds": receipt.duration_seconds,
        "channels": receipt.channels,
        "sample_rate": receipt.sample_rate,
        "quality": receipt.quality,
        "file": receipt.file,
        "path": receipt.path,
        "media_path": media_path,
        "bytes": receipt.bytes,
        "sha256": receipt.sha256,
        "license": receipt.license,
        "attribution": receipt.attribution,
        "source_url": receipt.source_url,
    });
    if receipt.quality == Quality::PreviewMp3 {
        value["warning"] = json!(format!(
            "This is a transcoded preview served by {}, not the master file; the licence and credit still apply.",
            provider.label()
        ));
    }
    if let Some(path) = &media_path {
        value["next_step"] = json!(format!(
            "Add it to a track with an audio clip: file: \"{path}\""
        ));
    }
    value
}

/// The directory a project's samples live in, plus the project root used to
/// report a portable `media/...` path.
fn samples_dir(project_root: Option<&Path>, dest: Option<&str>) -> Result<PathBuf, SampleError> {
    match dest.map(str::trim).filter(|dest| !dest.is_empty()) {
        Some(dest) => {
            let path = Path::new(dest);
            if path.is_absolute() {
                return Ok(path.to_path_buf());
            }
            match project_root {
                Some(root) => Ok(root.join(path)),
                None => Err(SampleError::refused(
                    "project_unsaved",
                    "Save the project first: a relative sample destination needs a project folder, or pass an absolute path.",
                )),
            }
        }
        None => match project_root {
            Some(root) => Ok(root.join(SAMPLES_SUBDIR)),
            None => Err(SampleError::refused(
                "project_unsaved",
                "Save the project first: a sample needs a media folder.",
            )),
        },
    }
}

/// The directory holding the open project, or `None` when it has never been
/// saved.
fn project_root(state: &AppState) -> Option<PathBuf> {
    let snapshot = {
        let inner = state.lock_inner();
        crate::commands::snapshot(&inner)
    };
    let path = snapshot.file_path?;
    Path::new(&path).parent().map(Path::to_path_buf)
}

/// The store a pull writes into, plus the project root that makes its paths
/// portable.
pub(crate) fn store_for(state: &AppState) -> Result<(SampleStore, Option<PathBuf>), SampleError> {
    let root = project_root(state);
    let dir = samples_dir(root.as_deref(), None)?;
    Ok((SampleStore::new(dir), root))
}

/// Resolve one id through a named market, for the panel's preview and add.
pub(crate) fn resolve_item(
    provider: Option<&str>,
    id: &str,
    keys: &Keys,
    fetch: &dyn SampleFetch,
) -> Result<(String, SampleItem), SampleError> {
    let provider = provider_for_args(
        provider,
        id,
        keys,
        &LocalRuntime::from_process_configuration(),
    )?;
    let item = provider.resolve(id, fetch)?;
    Ok((provider.id().to_string(), item))
}

/// A URL as a result should show it: scheme, host and path, without the signed
/// query a market hangs on a temporary file (`?token=…`, `?trackid=…`). The
/// pull resolves its own URL from the provider, so no caller reads these back.
fn short_url(url: &str) -> String {
    let head = &url[..url.find(['?', '#']).unwrap_or(url.len())];
    let body = match head.strip_suffix('/') {
        Some(trimmed) if trimmed.split('/').count() <= 3 => format!("{trimmed}/…"),
        _ => head.to_string(),
    };
    if body.chars().count() <= 160 {
        return body;
    }
    let mut cut: String = body.chars().take(157).collect();
    cut.push('…');
    cut
}

fn item_json(item: &SampleItem, provider_label: &str) -> Value {
    let mut value = serde_json::to_value(item).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        for key in ["source_url", "preview_url", "download_url"] {
            if let Some(Value::String(url)) = map.get_mut(key) {
                *url = short_url(url);
            }
        }
        map.insert("license_ok".into(), json!(item.license_ok()));
        map.insert(
            "license_notice".into(),
            if item.license.needs_notice() {
                json!(item.license.notice())
            } else {
                Value::Null
            },
        );
        map.insert("provider_name".into(), json!(provider_label));
    }
    value
}

#[cfg(test)]
mod url_tests {
    use super::*;

    fn item(source: &str, preview: &str, download: &str) -> SampleItem {
        SampleItem {
            id: "freesound:1".into(),
            provider: "freesound".into(),
            name: "snare".into(),
            kind: SampleKind::DrumOneShot,
            duration_seconds: Some(1.0),
            channels: Some(2),
            sample_rate: Some(44_100),
            license: oc_sample::license::License::cc0(),
            attribution: "snare by someone".into(),
            source_url: source.into(),
            preview_url: Some(preview.into()),
            download_url: download.into(),
            sha256: None,
            bytes: None,
            quality: Quality::PreviewMp3,
            tags: Vec::new(),
        }
    }

    #[test]
    fn a_signed_query_is_dropped_from_every_url() {
        let value = item_json(
            &item(
                "https://freesound.org/people/x/sounds/1/",
                "https://cdn.freesound.org/previews/1/1-hq.mp3?token=abc123&expires=17",
                "https://cdn.freesound.org/previews/1/1-hq.mp3?token=abc123&expires=17",
            ),
            "Freesound",
        );
        assert_eq!(
            value["preview_url"],
            json!("https://cdn.freesound.org/previews/1/1-hq.mp3")
        );
        assert_eq!(
            value["download_url"],
            json!("https://cdn.freesound.org/previews/1/1-hq.mp3")
        );
        assert!(
            !serde_json::to_string(&value).unwrap().contains("token="),
            "no token survives into the result"
        );
    }

    #[test]
    fn a_query_that_is_the_whole_address_keeps_the_host() {
        assert_eq!(
            short_url("https://prod-1.storage.jamendo.com/?trackid=1&format=mp31&from=x"),
            "https://prod-1.storage.jamendo.com/…"
        );
    }

    #[test]
    fn a_long_path_is_cut_with_an_ellipsis() {
        let long = format!("https://example.org/{}", "a".repeat(400));
        let short = short_url(&long);
        assert!(short.chars().count() <= 160, "{short}");
        assert!(short.ends_with('…'));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_search_with(
        args: &Value,
        keys: &Keys,
        fetch: &dyn SampleFetch,
    ) -> Result<Value, String> {
        search_with(
            args,
            keys,
            fetch,
            &oc_sample::runtime::FixtureBuiltinRuntime,
        )
    }
    fn fixture_provider_for_args(
        requested: Option<&str>,
        id: &str,
        keys: &Keys,
    ) -> Result<Box<dyn SampleProvider>, SampleError> {
        provider_for_args(
            requested,
            id,
            keys,
            &oc_sample::runtime::FixtureBuiltinRuntime,
        )
    }

    use std::sync::Mutex;

    #[test]
    fn explicit_empty_local_runtime_reports_unavailable_without_using_adapter_fixtures() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = LocalRuntime::from_directory(directory.path());
        let keys = no_keys();
        let fetch = fixture(vec![]);
        let value = search_with(
            &json!({"query": "snare", "provider": "commons"}),
            &keys,
            &fetch,
            &runtime,
        )
        .unwrap();
        let commons = value["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "wikimedia-commons")
            .unwrap();
        assert_eq!(commons["available"], false);
        assert!(
            commons["reason"]
                .as_str()
                .unwrap()
                .contains("no local sample-search plugin")
        );
        assert!(value["items"].as_array().unwrap().is_empty());
        assert!(fetch.urls.lock().unwrap().is_empty());
        assert_eq!(
            provider_for_args(Some("commons"), "commons:fixture", &keys, &runtime)
                .err()
                .unwrap()
                .code(),
            Some("provider_unavailable")
        );
    }

    /// Serves one recorded body per URL fragment, so a multi-market search can
    /// be exercised without a network.
    struct FixtureFetch {
        routes: Vec<(&'static str, &'static [u8])>,
        urls: Mutex<Vec<String>>,
    }

    impl SampleFetch for FixtureFetch {
        fn get(
            &self,
            url: &str,
            _max_bytes: usize,
        ) -> Result<oc_sample::provider::FetchResponse, oc_sample::provider::FetchError> {
            self.urls.lock().unwrap().push(url.to_string());
            let body = self
                .routes
                .iter()
                .find(|(fragment, _)| url.contains(fragment))
                .map(|(_, body)| *body)
                .ok_or_else(|| format!("no fixture for {url}"))?;
            Ok(oc_sample::provider::FetchResponse {
                status: 200,
                body: body.to_vec(),
            })
        }
    }

    fn fixture(routes: Vec<(&'static str, &'static [u8])>) -> FixtureFetch {
        FixtureFetch {
            routes,
            urls: Mutex::new(Vec::new()),
        }
    }

    const OPENVERSE: &[u8] =
        include_bytes!("../../../../oc-sample/tests/fixtures/openverse-search.json");
    const COMMONS: &[u8] =
        include_bytes!("../../../../oc-sample/tests/fixtures/commons-search.json");
    const ARCHIVE: &[u8] =
        include_bytes!("../../../../oc-sample/tests/fixtures/internet-archive-search.json");

    fn no_keys() -> Keys {
        Keys {
            freesound: None,
            freesound_token: None,
            jamendo: None,
            pixabay: None,
        }
    }

    #[test]
    fn a_search_names_the_provider_on_every_item() {
        let keys = no_keys();
        let fetch = fixture(vec![("api.openverse.org", OPENVERSE)]);
        let args = json!({
            "query": "snare",
            "providers": ["openverse"],
            "limit": 5
        });
        let value = fixture_search_with(&args, &keys, &fetch).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["count"], 3);
        assert_eq!(value["items"][0]["provider"], "openverse");
        assert_eq!(value["items"][0]["provider_name"], "Openverse");
        // The recorded Openverse page is three CC BY results, so every item is
        // pullable; a licence that needs a confirmation is covered by the
        // refusal test below with an inline body.
        assert_eq!(value["items"][0]["license_ok"], true);
        assert_eq!(value["items"][1]["license_ok"], true);
        assert_eq!(value["pages"][0]["count"], 3);
        assert!(value["pages"][0]["total"].is_number());
    }

    #[test]
    fn an_unavailable_market_is_reported_in_the_result_not_dropped() {
        let keys = no_keys();
        let fetch = fixture(vec![("api.openverse.org", OPENVERSE)]);
        let args = json!({
            "query": "snare",
            "providers": ["openverse", "mixkit", "soundbible"],
            "limit": 5
        });
        let value = fixture_search_with(&args, &keys, &fetch).unwrap();
        let providers = value["providers"].as_array().unwrap();
        let mixkit = providers.iter().find(|row| row["id"] == "mixkit").unwrap();
        assert_eq!(mixkit["available"], false);
        assert!(mixkit["reason"].as_str().unwrap().contains("JavaScript"));
        let pages = value["pages"].as_array().unwrap();
        assert!(pages.iter().any(|page| page["provider"] == "mixkit"
            && page["error"].as_str().unwrap().contains("JavaScript")));
        // The available market still answered.
        assert_eq!(value["count"], 3);
    }

    #[test]
    fn a_search_reports_an_unknown_provider_instead_of_dropping_it() {
        let keys = no_keys();
        let fetch = fixture(vec![("api.openverse.org", OPENVERSE)]);
        let args = json!({
            "query": "snare",
            "providers": ["openverse", "loopcloud"],
            "limit": 5
        });
        let value = fixture_search_with(&args, &keys, &fetch).unwrap();
        let providers = value["providers"].as_array().unwrap();
        let unknown = providers
            .iter()
            .find(|row| row["id"] == "loopcloud")
            .unwrap();
        assert_eq!(unknown["available"], false);
        assert!(
            unknown["reason"]
                .as_str()
                .unwrap()
                .contains("not a provider this build offers")
        );
        // A website-only market is still reported.
        let splice = providers.iter().find(|row| row["id"] == "splice").unwrap();
        assert_eq!(splice["available"], false);
    }

    #[test]
    fn an_empty_provider_list_searches_every_available_market() {
        let keys = no_keys();
        let fetch = fixture(vec![
            ("api.openverse.org", OPENVERSE),
            ("commons.wikimedia.org", COMMONS),
            ("archive.org/advancedsearch", ARCHIVE),
        ]);
        let args = json!({ "query": "snare", "limit": 5 });
        let value = fixture_search_with(&args, &keys, &fetch).unwrap();
        let pages = value["pages"].as_array().unwrap();
        let searched: Vec<&str> = pages
            .iter()
            .map(|page| page["provider"].as_str().unwrap())
            .collect();
        assert_eq!(searched, vec!["wikimedia-commons", "internet-archive"]);
        // A keyed market without its key is still listed as unavailable.
        let providers = value["providers"].as_array().unwrap();
        let freesound = providers
            .iter()
            .find(|row| row["id"] == "freesound")
            .unwrap();
        assert_eq!(freesound["available"], false);
        assert!(!searched.contains(&"freesound"));
    }

    #[test]
    fn a_failing_market_is_reported_and_the_others_still_answer() {
        let keys = no_keys();
        let fetch = fixture(vec![("api.openverse.org", OPENVERSE)]);
        let args = json!({
            "query": "snare",
            "providers": ["openverse", "internet-archive"],
            "limit": 5
        });
        let value = fixture_search_with(&args, &keys, &fetch).unwrap();
        let pages = value["pages"].as_array().unwrap();
        let archive = pages
            .iter()
            .find(|page| page["provider"] == "internet-archive")
            .unwrap();
        assert!(archive["error"].is_string());
        assert_eq!(value["count"], 3);
    }

    #[test]
    fn a_pull_infers_the_market_from_the_id_prefix() {
        let keys = no_keys();
        let provider = fixture_provider_for_args(None, "openverse:abc", &keys).unwrap();
        assert_eq!(provider.id(), "openverse");
        let provider = fixture_provider_for_args(None, "ia:item", &keys).unwrap();
        assert_eq!(provider.id(), "internet-archive");
    }

    #[test]
    fn a_pull_refuses_an_unknown_or_unavailable_market() {
        let keys = no_keys();
        let error = fixture_provider_for_args(Some("loopcloud"), "loopcloud:1", &keys)
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("provider_unknown"));
        let error = fixture_provider_for_args(Some("mixkit"), "mixkit:1", &keys)
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("provider_unavailable"));
        assert!(error.to_string().contains("JavaScript"), "{error}");
        // An id with no known prefix and no provider hint is refused too.
        let error = fixture_provider_for_args(None, "mystery:1", &keys)
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("provider_unknown"));
        assert!(error.to_string().contains("Pass the provider"), "{error}");
    }

    #[test]
    fn a_keyed_market_without_a_key_refuses_with_its_env_and_keyring_item() {
        let keys = no_keys();
        let error = fixture_provider_for_args(Some("freesound"), "freesound:1", &keys)
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("key_required"));
        assert!(error.to_string().contains("FREESOUND_API_KEY"), "{error}");
        let error = fixture_provider_for_args(Some("jamendo"), "jamendo:1", &keys)
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("key_required"));
        assert!(error.to_string().contains("JAMENDO_CLIENT_ID"), "{error}");
    }

    #[test]
    fn an_unknown_kind_is_refused_with_the_accepted_words() {
        let keys = no_keys();
        let fetch = fixture(vec![]);
        let error =
            fixture_search_with(&json!({ "query": "snare", "kind": "banjo" }), &keys, &fetch)
                .err()
                .unwrap();
        assert!(error.contains("accepted: drum, loop, vocal, fx, instrument, any"));
    }

    fn run(
        args: Value,
        fetch: &dyn SampleFetch,
        breakers: &Breakers,
    ) -> Result<Value, SampleError> {
        let request = SearchArgs::decode(&args).unwrap();
        search_with_request(
            &request,
            &no_keys(),
            fetch,
            &oc_sample::runtime::FixtureBuiltinRuntime,
            breakers,
        )
    }

    /// Fails every request the way the HTTP client reports `error`.
    struct FailingFetch {
        error: oc_sample::provider::FetchError,
        calls: Mutex<usize>,
    }

    impl FailingFetch {
        fn new(kind: FailureKind, status: Option<u16>, retry_after_secs: Option<u64>) -> Self {
            Self {
                error: oc_sample::provider::FetchError {
                    kind,
                    status,
                    retry_after_secs,
                    message: "fixture failure".into(),
                },
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl SampleFetch for FailingFetch {
        fn get(
            &self,
            _url: &str,
            _max_bytes: usize,
        ) -> Result<oc_sample::provider::FetchResponse, oc_sample::provider::FetchError> {
            *self.calls.lock().unwrap() += 1;
            Err(self.error.clone())
        }
    }

    fn openverse_only() -> Value {
        json!({ "query": "snare", "providers": ["openverse"], "limit": 5 })
    }

    #[test]
    fn a_page_reports_a_typed_error_kind_beside_its_message() {
        let cases = [
            (FailureKind::Unauthorized, Some(401), "needs_key"),
            (FailureKind::NotFound, Some(404), "not_found"),
            (FailureKind::Unavailable, Some(503), "unavailable"),
            (FailureKind::Timeout, None, "timeout"),
            (FailureKind::Network, None, "network"),
            (FailureKind::Parse, None, "parse"),
            (FailureKind::Other, None, "other"),
        ];
        for (kind, status, expected) in cases {
            let fetch = FailingFetch::new(kind, status, None);
            let value = run(openverse_only(), &fetch, &Breakers::default()).unwrap();
            let page = &value["pages"][0];
            assert_eq!(page["provider"], "openverse");
            assert_eq!(page["error_kind"], expected, "{kind:?}");
            assert!(page["error"].as_str().unwrap().contains("fixture failure"));
            assert_eq!(page["http_status"], json!(status));
            assert!(page["skip_reason"].is_null());
        }
        // A market without its key is the same `needs_key` state.
        let value = run(
            json!({ "query": "snare", "providers": ["freesound"] }),
            &fixture(vec![]),
            &Breakers::default(),
        )
        .unwrap();
        assert_eq!(value["pages"][0]["error_kind"], "needs_key");
        let value = run(
            json!({ "query": "snare", "providers": ["mixkit"] }),
            &fixture(vec![]),
            &Breakers::default(),
        )
        .unwrap();
        assert_eq!(value["pages"][0]["error_kind"], "provider_unavailable");
    }

    #[test]
    fn two_timeouts_skip_the_market_and_a_bad_key_never_does() {
        let breakers = Breakers::default();
        let timeout = FailingFetch::new(FailureKind::Timeout, None, None);
        run(openverse_only(), &timeout, &breakers).unwrap();
        run(openverse_only(), &timeout, &breakers).unwrap();
        let value = run(openverse_only(), &timeout, &breakers).unwrap();
        assert_eq!(
            timeout.calls(),
            2,
            "the third search does not reach the market"
        );
        let page = &value["pages"][0];
        assert_eq!(page["skipped"], true);
        assert_eq!(page["error_kind"], "skipped");
        assert_eq!(page["skip_reason"], "circuit_open");
        assert_eq!(
            page["retry_after_secs"],
            breaker::BREAKER_COOLDOWN.as_secs()
        );

        let breakers = Breakers::default();
        let unauthorized = FailingFetch::new(FailureKind::Unauthorized, Some(401), None);
        for _ in 0..3 {
            let value = run(openverse_only(), &unauthorized, &breakers).unwrap();
            assert_eq!(value["pages"][0]["error_kind"], "needs_key");
        }
        assert_eq!(
            unauthorized.calls(),
            3,
            "a bad key is asked again every time"
        );
    }

    #[test]
    fn a_throttled_market_waits_for_its_retry_after() {
        let breakers = Breakers::default();
        let throttled = FailingFetch::new(FailureKind::RateLimited, Some(429), Some(90));
        let first = run(openverse_only(), &throttled, &breakers).unwrap();
        assert_eq!(first["pages"][0]["error_kind"], "rate_limited");
        assert_eq!(first["pages"][0]["retry_after_secs"], 90);
        let second = run(openverse_only(), &throttled, &breakers).unwrap();
        assert_eq!(throttled.calls(), 1);
        assert_eq!(second["pages"][0]["skip_reason"], "rate_limited");
        let wait = second["pages"][0]["retry_after_secs"].as_u64().unwrap();
        assert!((1..=90).contains(&wait), "{wait}");

        // Without a Retry-After the default wait applies, and a huge one is
        // capped.
        let breakers = Breakers::default();
        let bare = FailingFetch::new(FailureKind::RateLimited, Some(429), None);
        let value = run(openverse_only(), &bare, &breakers).unwrap();
        assert_eq!(
            value["pages"][0]["retry_after_secs"],
            breaker::RATE_LIMIT_DEFAULT.as_secs()
        );
        let breakers = Breakers::default();
        let long = FailingFetch::new(FailureKind::RateLimited, Some(429), Some(86_400));
        let value = run(openverse_only(), &long, &breakers).unwrap();
        assert_eq!(
            value["pages"][0]["retry_after_secs"],
            breaker::RATE_LIMIT_MAX.as_secs()
        );
    }

    fn decode_cursor(value: &Value) -> SearchCursor {
        SearchCursor::decode(value["cursor"].as_str().expect("a cursor")).unwrap()
    }

    #[test]
    fn a_cursor_continues_each_market_at_its_own_offset() {
        let fetch = fixture(vec![
            ("api.openverse.org", OPENVERSE),
            ("archive.org/advancedsearch", ARCHIVE),
            ("commons.wikimedia.org", COMMONS),
        ]);
        let breakers = Breakers::default();
        let first = run(
            json!({
                "query": "snare",
                "providers": ["openverse", "wikimedia-commons", "internet-archive"],
                "limit": 5
            }),
            &fetch,
            &breakers,
        )
        .unwrap();
        let cursor = decode_cursor(&first);
        // Commons returned its last page, so the cursor leaves it out.
        assert_eq!(
            cursor.markets().collect::<Vec<_>>(),
            vec!["openverse", "internet-archive"]
        );
        assert_eq!(cursor.offset_of("openverse"), Some(3));
        assert_eq!(cursor.offset_of("internet-archive"), Some(3));
        // An old caller still gets the shared offset.
        assert_eq!(first["next_offset"], 5);

        let next = run(
            json!({ "query": "snare", "cursor": first["cursor"], "limit": 5 }),
            &fetch,
            &breakers,
        )
        .unwrap();
        let asked: Vec<(&str, u64)> = next["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| {
                (
                    page["provider"].as_str().unwrap(),
                    page["offset"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(asked, vec![("openverse", 3), ("internet-archive", 3)]);
        assert!(
            next["next_offset"].is_null(),
            "a cursor search pages by cursor"
        );

        // An explicit market list narrows what the cursor continues.
        let narrowed = run(
            json!({ "query": "snare", "cursor": first["cursor"], "providers": ["ia"] }),
            &fetch,
            &breakers,
        )
        .unwrap();
        assert_eq!(narrowed["pages"].as_array().unwrap().len(), 1);
        assert_eq!(narrowed["pages"][0]["provider"], "internet-archive");
    }

    #[test]
    fn an_exhausted_search_returns_no_cursor() {
        let body: &'static [u8] = br#"{"result_count":1,"page_count":1,"page":1,"results":[
            {"id":"a","title":"Kick","license":"cc0","license_url":"https://creativecommons.org/publicdomain/zero/1.0/","url":"https://example.org/kick.wav"}
        ]}"#;
        let fetch = fixture(vec![("api.openverse.org", body)]);
        let value = run(openverse_only(), &fetch, &Breakers::default()).unwrap();
        assert!(value["cursor"].is_null());
        assert!(value["next_offset"].is_null());
    }

    #[test]
    fn a_malformed_cursor_is_refused_before_any_market_is_asked() {
        let fetch = fixture(vec![("api.openverse.org", OPENVERSE)]);
        for cursor in ["%%%", "eyJ2Ijo5LCJtIjpbXX0", &"A".repeat(5000)] {
            let error = run(
                json!({ "query": "snare", "providers": ["openverse"], "cursor": cursor }),
                &fetch,
                &Breakers::default(),
            )
            .unwrap_err();
            assert_eq!(error.code(), Some("invalid_cursor"), "{cursor}");
        }
        assert!(fetch.urls.lock().unwrap().is_empty());
        // The agent tool reads the same refusal as its sentence.
        let message = fixture_search_with(
            &json!({ "query": "snare", "cursor": "%%%" }),
            &no_keys(),
            &fetch,
        )
        .unwrap_err();
        assert!(message.contains("search cursor"), "{message}");
    }

    #[test]
    fn markets_interleave_in_the_requested_order() {
        let fetch = fixture(vec![
            ("api.openverse.org", OPENVERSE),
            ("archive.org/advancedsearch", ARCHIVE),
        ]);
        let value = run(
            json!({
                "query": "snare",
                "providers": ["internet-archive", "openverse"],
                "limit": 5
            }),
            &fetch,
            &Breakers::default(),
        )
        .unwrap();
        let providers: Vec<&str> = value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["provider"].as_str().unwrap())
            .collect();
        assert_eq!(
            providers,
            vec![
                "internet-archive",
                "openverse",
                "internet-archive",
                "openverse",
                "internet-archive",
                "openverse"
            ]
        );
        assert_eq!(value["duplicates"], 0);
    }

    #[test]
    fn the_licence_filter_reaches_every_market() {
        let body: &'static [u8] = br#"{"result_count":2,"page_count":1,"page":1,"results":[
            {"id":"nc","title":"Night Drive","license":"by-nc","license_version":"4.0","license_url":"https://creativecommons.org/licenses/by-nc/4.0/","url":"https://example.org/night.mp3"},
            {"id":"ok","title":"Kick","license":"cc0","license_url":"https://creativecommons.org/publicdomain/zero/1.0/","url":"https://example.org/kick.wav"}
        ]}"#;
        let fetch = fixture(vec![("api.openverse.org", body)]);
        let all = run(openverse_only(), &fetch, &Breakers::default()).unwrap();
        assert_eq!(all["count"], 2);
        assert_eq!(all["commercial_ok"], false);
        let mut args = openverse_only();
        args["commercial_ok"] = json!(true);
        let filtered = run(args, &fetch, &Breakers::default()).unwrap();
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["items"][0]["id"], "openverse:ok");
        assert_eq!(filtered["commercial_ok"], true);
    }

    /// Answers the authenticated download endpoint by bearer token: a stale
    /// token gets 401, an unknown one 403, and only the fresh token serves the
    /// original. Every other URL is the preview.
    struct TokenFetch {
        original: Vec<u8>,
        preview: Vec<u8>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl TokenFetch {
        fn new() -> Self {
            let mut original = std::io::Cursor::new(Vec::new());
            {
                let mut writer = hound::WavWriter::new(
                    &mut original,
                    hound::WavSpec {
                        channels: 1,
                        sample_rate: 44_100,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    },
                )
                .unwrap();
                for sample in [0i16, 1000, -1000] {
                    writer.write_sample(sample).unwrap();
                }
                writer.finalize().unwrap();
            }
            // A complete frame-sized MPEG-1 Layer III download fixture. These
            // tests exercise container identification and OAuth, not decoding.
            let mut preview = vec![0; 417];
            preview[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
            Self {
                original: original.into_inner(),
                preview,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SampleFetch for TokenFetch {
        fn get(
            &self,
            url: &str,
            max_bytes: usize,
        ) -> Result<oc_sample::provider::FetchResponse, oc_sample::provider::FetchError> {
            self.get_with(url, max_bytes, &[])
        }

        fn get_with(
            &self,
            url: &str,
            _max_bytes: usize,
            headers: &[(&str, &str)],
        ) -> Result<oc_sample::provider::FetchResponse, oc_sample::provider::FetchError> {
            let bearer = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.to_string())
                .unwrap_or_default();
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), bearer.clone()));
            if url.contains("/download/") {
                return match bearer.as_str() {
                    "Bearer stale" => Err(status_error(401, url)),
                    "Bearer fresh" => Ok(oc_sample::provider::FetchResponse {
                        status: 200,
                        body: self.original.clone(),
                    }),
                    _ => Err(status_error(403, url)),
                };
            }
            Ok(oc_sample::provider::FetchResponse {
                status: 200,
                body: self.preview.clone(),
            })
        }
    }

    /// The failure the HTTP client reports for a refused status.
    fn status_error(status: u16, url: &str) -> oc_sample::provider::FetchError {
        oc_sample::provider::FetchError {
            kind: oc_sample::model::FailureKind::Unauthorized,
            status: Some(status),
            retry_after_secs: None,
            message: format!("HTTP {status} from {url}"),
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "oc-samples-pull-{label}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn store(&self) -> SampleStore {
            SampleStore::new(self.0.join(SAMPLES_SUBDIR))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn freesound_item() -> SampleItem {
        let preview = "https://cdn.freesound.org/previews/14/14854_100-hq.mp3";
        SampleItem {
            id: "freesound:14854".into(),
            provider: "freesound".into(),
            name: "snare_01.wav".into(),
            kind: SampleKind::DrumOneShot,
            duration_seconds: Some(0.42),
            channels: Some(1),
            sample_rate: Some(44_100),
            license: oc_sample::license::License::cc0(),
            attribution: "bassmosphere — CC0 1.0 — https://freesound.org/s/14854/".into(),
            source_url: "https://freesound.org/s/14854/".into(),
            preview_url: Some(preview.into()),
            download_url: preview.into(),
            sha256: None,
            bytes: None,
            quality: Quality::PreviewMp3,
            tags: vec!["snare".into()],
        }
    }

    #[test]
    fn a_freesound_token_pulls_the_original_and_sends_the_bearer_header() {
        let temp = TempDir::new("original");
        let fetch = TokenFetch::new();
        let receipt = pull_item_with(
            &temp.store(),
            &freesound_item(),
            &fetch,
            false,
            Some("fresh"),
            || Ok("fresh".into()),
        )
        .unwrap();

        assert_eq!(receipt.quality, Quality::Original);
        assert_eq!(receipt.bytes, fetch.original.len() as u64);
        let calls = fetch.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(
            calls[0].0.ends_with("/apiv2/sounds/14854/download/"),
            "{calls:?}"
        );
        assert_eq!(calls[0].1, "Bearer fresh");
    }

    #[test]
    fn a_refused_original_falls_back_to_the_preview() {
        let temp = TempDir::new("fallback");
        let fetch = TokenFetch::new();
        let receipt = pull_item_with(
            &temp.store(),
            &freesound_item(),
            &fetch,
            false,
            Some("revoked"),
            || panic!("a 403 is not a reason to refresh"),
        )
        .unwrap();

        assert_eq!(receipt.quality, Quality::PreviewMp3);
        assert_eq!(receipt.bytes, fetch.preview.len() as u64);
        assert!(receipt.file.ends_with(".mp3"), "{}", receipt.file);
        let calls = fetch.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].0.contains("/download/"), "{calls:?}");
        assert!(calls[1].0.contains("previews"), "{calls:?}");
    }

    #[test]
    fn a_401_refreshes_once_and_retries_the_original() {
        let temp = TempDir::new("refresh");
        let fetch = TokenFetch::new();
        let receipt = pull_item_with(
            &temp.store(),
            &freesound_item(),
            &fetch,
            false,
            Some("stale"),
            || Ok("fresh".into()),
        )
        .unwrap();

        assert_eq!(receipt.quality, Quality::Original);
        assert_eq!(receipt.bytes, fetch.original.len() as u64);
        let calls = fetch.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].1, "Bearer stale");
        assert_eq!(calls[1].1, "Bearer fresh");
        assert!(calls.iter().all(|(url, _)| url.contains("/download/")));
    }
}
