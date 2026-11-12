//! Remote plugin-index client: signed manifest, zstd catalog, atomic disk cache.
//!
//! The marketplace owner authenticates and bounds every manifest and catalog,
//! validates clocks and compatibility, and enforces scoped rollback protection.
//! Both cache reads and network replies use that policy before publication.
//!
//! The client never starts a fetch on its own. It is created from the disk cache
//! at application setup, and a fetch happens only when the plugin browser opens
//! or a caller asks for a refresh.

use oc_market::index::hex_decode;
#[cfg(test)]
use oc_market::{key_id, sha256_hex};
#[cfg(test)]
use std::fs;
use std::future::Future;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use oc_market::index::IndexError;
#[cfg(test)]
use oc_market::index::{CacheState, MANIFEST_FILE};
use oc_market::index::{
    FetchSession, IndexStore, Policy, Response as VerifiedResponse, StoredIndex, VerifiedIndex,
};
use serde::Serialize;

use crate::plugin_store::{self, CatalogEntry, PackEntry};

pub use oc_market::index::{FALLBACK_BASE_URL, PRIMARY_BASE_URL};
#[cfg(test)]
pub const STATE_FILE: &str = "state.json";
/// How long a verified catalog counts as fresh: a browsing session inside it
/// answers from the cache.
pub const REFRESH_INTERVAL_SECONDS: u64 = 6 * 60 * 60;
pub const MANUAL_FLOOR_SECONDS: u64 = 5 * 60;
pub const STALE_WARNING_DAYS: u64 = 14;

#[cfg(test)]
const CACHE_DIR: &str = "last-good";
const USER_AGENT: &str = concat!("OpenCompose/", env!("CARGO_PKG_VERSION"), " plugin-index");

// ============================================================================
// Transport
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
}

type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<FetchResponse, String>> + Send + 'a>>;

/// Minimal GET transport so tests never touch the network.
pub trait IndexFetch: Sync {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        if_none_match: Option<&'a str>,
        max_bytes: usize,
    ) -> FetchFuture<'a>;
}

pub struct HttpFetch {
    client: reqwest::Client,
}

impl HttpFetch {
    pub fn new() -> Result<Self, String> {
        let client = crate::net_proxy::apply(reqwest::Client::builder())
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("Failed to create plugin-index HTTP client: {error}"))?;
        Ok(Self { client })
    }
}

impl IndexFetch for HttpFetch {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        if_none_match: Option<&'a str>,
        max_bytes: usize,
    ) -> FetchFuture<'a> {
        Box::pin(async move {
            let mut request = self
                .client
                .get(url)
                .header(reqwest::header::CACHE_CONTROL, "no-cache")
                .header(
                    reqwest::header::ACCEPT,
                    "application/json, application/octet-stream",
                );
            if let Some(etag) = if_none_match {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            let response = request
                .send()
                .await
                .map_err(|error| format!("request failed: {error}"))?;
            let status = response.status().as_u16();
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            if status == 304 {
                return Ok(FetchResponse {
                    status,
                    body: Vec::new(),
                    etag,
                });
            }
            if !response.status().is_success() {
                return Err(format!("HTTP {status} from {url}"));
            }
            if let Some(length) = response.content_length()
                && length > max_bytes as u64
            {
                return Err(format!("response is larger than {max_bytes} bytes"));
            }
            let mut body = Vec::new();
            let mut stream = response;
            while let Some(chunk) = stream
                .chunk()
                .await
                .map_err(|error| format!("read failed: {error}"))?
            {
                if body.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(format!("response is larger than {max_bytes} bytes"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(FetchResponse { status, body, etag })
        })
    }
}

// ============================================================================
// Errors and status
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexSource {
    Bundled,
    Cache,
    Remote,
}

impl IndexSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Cache => "cache",
            Self::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexTrust {
    Unconfigured,
    Configured,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginIndexStatus {
    pub trust: IndexTrust,
    pub source: String,
    pub generated_at: Option<String>,
    pub fetched_at: Option<u64>,
    pub stale_days: Option<u64>,
    pub stale_warning: bool,
    pub last_error: Option<String>,
    pub catalog_version: Option<String>,
    pub min_app_version: Option<String>,
    pub next_refresh_in_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshOutcome {
    pub performed: bool,
    pub changed: bool,
    pub status: PluginIndexStatus,
}

/// The catalog currently served to the store, with provenance for the UI.
#[derive(Clone)]
pub struct CatalogView {
    pub entries: Arc<Vec<CatalogEntry>>,
    pub packs: Arc<Vec<PackEntry>>,
    pub source: IndexSource,
    pub(crate) authority: CatalogAuthority,
}

#[derive(Clone)]
pub enum CatalogAuthority {
    Bundled,
    Signed {
        store: Arc<IndexStore>,
        index: Arc<VerifiedIndex>,
    },
}
impl CatalogAuthority {
    pub(crate) fn publication_guard(
        &self,
    ) -> Result<Option<oc_market::index::PublicationGuard>, String> {
        match self {
            Self::Bundled => Ok(None),
            Self::Signed { store, index } => {
                store.authorize(index).map(Some).map_err(|e| e.to_string())
            }
        }
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.publication_guard().map(drop)
    }
}

// ============================================================================
// Manifest and cache types
// ============================================================================

#[derive(Clone)]
struct ActiveCatalog {
    verified: VerifiedIndex,
    entries: Arc<Vec<CatalogEntry>>,
    packs: Arc<Vec<PackEntry>>,
    source: IndexSource,
    generated_at: String,
    catalog_version: String,
    min_app_version: String,
    fetched_at: u64,
    checked_at: u64,
    manifest_sha256: String,
}

type BundledCatalog = Arc<Vec<CatalogEntry>>;

// ============================================================================
// Client
// ============================================================================

pub struct IndexClient {
    dir: PathBuf,
    public_key: Option<Vec<u8>>,
    primary_base: String,
    fallback_base: String,
    active: Mutex<Option<ActiveCatalog>>,
    last_attempt: Mutex<Option<u64>>,
    last_error: Mutex<Option<String>>,
    bundled: Mutex<Option<BundledCatalog>>,
}

/// A development index served locally. Both overrides are compiled in
/// only for debug builds: a release binary trusts the pinned key and the real
/// origin, so a stray environment variable cannot redirect the store.
fn override_base_urls() -> (String, String) {
    #[cfg(debug_assertions)]
    if let Ok(base) = std::env::var("OPENCOMPOSE_INDEX_BASE_URL") {
        let base = base.trim().to_string();
        if !base.is_empty() {
            let base = if base.ends_with('/') {
                base
            } else {
                format!("{base}/")
            };
            return (base.clone(), base);
        }
    }
    (PRIMARY_BASE_URL.to_string(), FALLBACK_BASE_URL.to_string())
}

fn override_public_key() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(key) = std::env::var("OPENCOMPOSE_INDEX_PUBLIC_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

impl IndexClient {
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        let public_key = match override_public_key() {
            Some(key) => {
                let key = hex_decode(&key)
                    .map_err(|error| format!("Configured plugin-index key is invalid: {error}"))?;
                if key.len() != 32 {
                    return Err("Configured plugin-index key must be 32 bytes".into());
                }
                Some(key)
            }
            None => oc_market::configured_public_key()?,
        };
        let (primary, fallback) = override_base_urls();
        Ok(Self::with_optional_key(dir, public_key, primary, fallback))
    }

    #[cfg(test)]
    fn with_key(dir: PathBuf, public_key: Vec<u8>) -> Self {
        Self::with_bases(
            dir,
            public_key,
            PRIMARY_BASE_URL.to_string(),
            FALLBACK_BASE_URL.to_string(),
        )
    }

    #[cfg(test)]
    fn with_bases(
        dir: PathBuf,
        public_key: Vec<u8>,
        primary_base: String,
        fallback_base: String,
    ) -> Self {
        Self::with_optional_key(dir, Some(public_key), primary_base, fallback_base)
    }

    fn with_optional_key(
        dir: PathBuf,
        public_key: Option<Vec<u8>>,
        primary_base: String,
        fallback_base: String,
    ) -> Self {
        let client = Self {
            dir,
            public_key,
            primary_base,
            fallback_base,
            active: Mutex::new(None),
            last_attempt: Mutex::new(None),
            last_error: Mutex::new(None),
            bundled: Mutex::new(None),
        };
        if client.public_key.is_some() {
            let loaded = client.index_store().and_then(|store| {
                let active = store
                    .load()?
                    .map(|stored| active_catalog(stored, IndexSource::Cache))
                    .transpose()?;
                if store.cache_dir()?.is_none()
                    && let Some(active) = &active
                {
                    store.commit(&active.verified, active.checked_at)?;
                }
                Ok(active)
            });
            match loaded {
                Ok(Some(active)) => *lock(&client.active) = Some(active),
                Ok(None) => {}
                Err(error) => *lock(&client.last_error) = Some(format!("cache: {error}")),
            }
        }
        client
    }

    pub fn view(&self) -> Result<CatalogView, String> {
        if let Some(active) = lock(&self.active).clone() {
            return Ok(CatalogView {
                entries: active.entries,
                packs: active.packs,
                source: active.source,
                authority: CatalogAuthority::Signed {
                    store: Arc::new(self.index_store().map_err(|e| e.to_string())?),
                    index: Arc::new(active.verified),
                },
            });
        }
        Ok(CatalogView {
            entries: self.bundled()?,
            packs: Arc::new(Vec::new()),
            source: IndexSource::Bundled,
            authority: CatalogAuthority::Bundled,
        })
    }

    pub fn status(&self) -> PluginIndexStatus {
        self.status_at(unix_now())
    }

    /// Fetch the index. `force` comes from the explicit refresh action; the
    /// plugin browser opens with `force = false`, which answers from the cache
    /// while the last check is inside [`REFRESH_INTERVAL_SECONDS`].
    pub async fn refresh(
        &self,
        fetch: &dyn IndexFetch,
        force: bool,
    ) -> Result<RefreshOutcome, IndexError> {
        self.refresh_at(fetch, force, unix_now()).await
    }

    fn status_at(&self, now: u64) -> PluginIndexStatus {
        let active = lock(&self.active).clone();
        let last_error = lock(&self.last_error).clone().or_else(|| self.public_key.is_none().then(||
            "Remote plugin-index trust is not configured; the bundled catalog remains available".into()));
        let (source, generated_at, fetched_at, catalog_version, min_app_version, stale_days) =
            match &active {
                Some(active) => (
                    active.source.as_str(),
                    Some(active.generated_at.clone()),
                    Some(active.fetched_at),
                    Some(active.catalog_version.clone()),
                    Some(active.min_app_version.clone()),
                    Some(now.saturating_sub(active.fetched_at) / 86_400),
                ),
                None => (IndexSource::Bundled.as_str(), None, None, None, None, None),
            };
        let floor_at = lock(&self.last_attempt).or(active.as_ref().map(|active| active.checked_at));
        let next_refresh_in_seconds = floor_at
            .map(|last| MANUAL_FLOOR_SECONDS.saturating_sub(now.saturating_sub(last)))
            .unwrap_or(0);
        PluginIndexStatus {
            trust: if self.public_key.is_some() {
                IndexTrust::Configured
            } else {
                IndexTrust::Unconfigured
            },
            source: source.to_string(),
            generated_at,
            fetched_at,
            stale_days,
            stale_warning: stale_days.is_some_and(|days| days >= STALE_WARNING_DAYS),
            last_error,
            catalog_version,
            min_app_version,
            next_refresh_in_seconds,
        }
    }

    fn bundled(&self) -> Result<BundledCatalog, String> {
        let mut guard = lock(&self.bundled);
        if let Some(bundled) = guard.as_ref() {
            return Ok(bundled.clone());
        }
        let (entries, _) = plugin_store::bundled_catalog()?;
        let bundled = Arc::new(entries);
        *guard = Some(bundled.clone());
        Ok(bundled)
    }

    async fn refresh_at(
        &self,
        fetch: &dyn IndexFetch,
        force: bool,
        now: u64,
    ) -> Result<RefreshOutcome, IndexError> {
        if self.public_key.is_none() {
            return Err(IndexError::new(
                "trust",
                "Remote plugin-index trust is not configured; the bundled catalog remains available",
            ));
        }
        let active = lock(&self.active).clone();
        {
            let mut last_attempt = lock(&self.last_attempt);
            let floor = last_attempt.or(active.as_ref().map(|active| active.checked_at));
            if let Some(last) = floor {
                let elapsed = now.saturating_sub(last);
                if force && elapsed < MANUAL_FLOOR_SECONDS {
                    return Err(IndexError::new(
                        "policy",
                        format!(
                            "Plugin catalog refresh is limited to once every {} minutes; try again in {} seconds",
                            MANUAL_FLOOR_SECONDS / 60,
                            MANUAL_FLOOR_SECONDS - elapsed
                        ),
                    ));
                }
            }
            if !force && let Some(active) = &active {
                let due = active.checked_at.saturating_add(REFRESH_INTERVAL_SECONDS);
                if now < due {
                    drop(last_attempt);
                    let result = self.index_store()?.authorize(&active.verified);
                    if let Err(error) = result {
                        *lock(&self.last_error) = Some(error.to_string());
                        return Err(error);
                    }
                    *lock(&self.last_error) = None;
                    return Ok(RefreshOutcome {
                        performed: false,
                        changed: false,
                        status: self.status_at(now),
                    });
                }
            }
            *last_attempt = Some(now);
        }

        let result = self.refresh_inner(fetch, &active, now).await;
        match &result {
            Ok(_) => *lock(&self.last_error) = None,
            Err(error) => *lock(&self.last_error) = Some(error.to_string()),
        }
        result
    }

    fn policy(&self) -> Result<Policy, IndexError> {
        Ok(Policy {
            public_key: self.public_key.clone().ok_or_else(|| {
                IndexError::new("trust", "Remote plugin-index trust is not configured")
            })?,
            origins: vec![self.primary_base.clone(), self.fallback_base.clone()],
            app_version: Some(env!("CARGO_PKG_VERSION").into()),
        })
    }

    fn index_store(&self) -> Result<IndexStore, IndexError> {
        Ok(IndexStore::new(self.dir.clone(), self.policy()?))
    }

    async fn refresh_inner(
        &self,
        fetch: &dyn IndexFetch,
        cached: &Option<ActiveCatalog>,
        now: u64,
    ) -> Result<RefreshOutcome, IndexError> {
        let store = self.index_store()?;
        let mut session = FetchSession::new(
            self.policy()?,
            cached.as_ref().map(|active| active.verified.clone()),
            store.watermark()?,
        )?;
        while let Some(request) = session.request()? {
            let response = fetch
                .fetch(
                    &request.url,
                    request.if_none_match.as_deref(),
                    request.max_bytes,
                )
                .await
                .map(|response| VerifiedResponse {
                    status: response.status,
                    body: response.body,
                    etag: response.etag,
                });
            session.accept(response)?;
        }
        let index = session.into_verified()?;
        let same = cached
            .as_ref()
            .is_some_and(|old| old.manifest_sha256 == index.digest());
        // Surface-specific entry validation completes before trust or catalog publication.
        let active = active_catalog(
            StoredIndex {
                fetched_at: if same {
                    cached.as_ref().map_or(now, |old| old.fetched_at)
                } else {
                    now
                },
                checked_at: now,
                index,
            },
            if same {
                cached
                    .as_ref()
                    .map_or(IndexSource::Remote, |old| old.source)
            } else {
                IndexSource::Remote
            },
        )?;
        store.commit(&active.verified, now)?;
        *lock(&self.active) = Some(active);
        Ok(RefreshOutcome {
            performed: true,
            changed: !same,
            status: self.status_at(now),
        })
    }
}

/// Default cache root: `~/.opencompose/plugin-index/`. A test build keeps it in
/// the process scratch root.
pub fn index_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("OCP_PLUGIN_INDEX_DIR") {
        return PathBuf::from(path);
    }
    crate::private_fs::config_dir().join("plugin-index")
}

// ============================================================================
// Verification
// ============================================================================

fn active_catalog(stored: StoredIndex, source: IndexSource) -> Result<ActiveCatalog, IndexError> {
    let catalog = plugin_store::from_shared_catalog((*stored.index.document).clone());
    let index = stored.index;
    Ok(ActiveCatalog {
        entries: Arc::new(catalog.plugins),
        packs: Arc::new(catalog.packs),
        source,
        generated_at: index.manifest.generated_at.clone(),
        catalog_version: index.manifest.catalog_version.clone(),
        min_app_version: index.manifest.min_app_version.clone(),
        fetched_at: stored.fetched_at,
        checked_at: stored.checked_at,
        manifest_sha256: index.digest(),
        verified: index,
    })
}

// ============================================================================
// Helpers
// ============================================================================

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
use oc_market::index::{MAX_CATALOG_BYTES, version_newer};

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-plugin-index-client-{tag}-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakeFetch {
        responses: Mutex<VecDeque<FetchResponse>>,
        requests: Mutex<Vec<(String, Option<String>)>>,
    }

    impl FakeFetch {
        fn new(responses: Vec<FetchResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl IndexFetch for FakeFetch {
        fn fetch<'a>(
            &'a self,
            url: &'a str,
            if_none_match: Option<&'a str>,
            max_bytes: usize,
        ) -> FetchFuture<'a> {
            Box::pin(async move {
                self.requests
                    .lock()
                    .unwrap()
                    .push((url.to_string(), if_none_match.map(str::to_string)));
                let response = self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("fake fetch ran out of scripted responses");
                if response.body.len() > max_bytes {
                    return Err(format!("response is larger than {max_bytes} bytes"));
                }
                Ok(response)
            })
        }
    }

    struct Fixture {
        pkcs8: Vec<u8>,
        public_key: Vec<u8>,
        manifest: Vec<u8>,
        signature: Vec<u8>,
        catalog: Vec<u8>,
        catalog_path: String,
    }

    fn keypair() -> (Vec<u8>, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        (pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec())
    }

    fn sign(pkcs8: &[u8], message: &[u8]) -> Vec<u8> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8).unwrap();
        pair.sign(message).as_ref().to_vec()
    }

    fn catalog_json() -> Value {
        json!({
            "schema": 1,
            "generated_at": "2026-10-22T08:00:00Z",
            "entries": [{
                "id": "example.reverb-1",
                "name": "Example Reverb",
                "vendor": "Example Audio",
                "description": "A lush algorithmic reverb.",
                "tags": ["reverb", "freeware"],
                "installs": [{
                    "kind": "direct",
                    "platform": "macos-aarch64",
                    "format": "vst3",
                    "url": "https://plugins.example.com/example-reverb.zip"
                }]
            }]
        })
    }

    fn fixture(min_app_version: &str) -> Fixture {
        fixture_with(catalog_json(), min_app_version)
    }

    fn fixture_with(catalog: Value, min_app_version: &str) -> Fixture {
        let (pkcs8, public_key) = keypair();
        let raw = serde_json::to_vec_pretty(&catalog).unwrap();
        let compressed = zstd::bulk::compress(&raw, 9).unwrap();
        let sha256 = sha256_hex(&compressed);
        let catalog_path = format!("catalog-{sha256}.json.zst");
        let manifest = json!({
            "catalog": {
                "bytes": compressed.len() as u64,
                "compression_level": 9,
                "encoding": "zstd",
                "path": catalog_path,
                "sha256": sha256,
                "uncompressed_bytes": raw.len() as u64
            },
            "catalog_version": "2026.10.22.1",
            "generated_at": "2026-10-22T08:00:00Z",
            "min_app_version": min_app_version,
            "mirrors": [],
            "schema": 1,
            "signature": {
                "algorithm": "ed25519",
                "encoding": "raw",
                "key_id": key_id(&public_key),
                "path": "index.json.sig"
            }
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let signature = sign(&pkcs8, &manifest_bytes);
        Fixture {
            pkcs8,
            public_key,
            manifest: manifest_bytes,
            signature,
            catalog: compressed,
            catalog_path,
        }
    }

    impl Fixture {
        fn responses(&self) -> Vec<FetchResponse> {
            vec![
                FetchResponse {
                    status: 200,
                    body: self.manifest.clone(),
                    etag: Some("\"manifest-v1\"".into()),
                },
                FetchResponse {
                    status: 200,
                    body: self.signature.clone(),
                    etag: None,
                },
                FetchResponse {
                    status: 200,
                    body: self.catalog.clone(),
                    etag: None,
                },
            ]
        }

        fn client(&self, dir: &Path) -> IndexClient {
            IndexClient::with_key(dir.to_path_buf(), self.public_key.clone())
        }

        fn mutate_manifest(&self, mutate: impl FnOnce(&mut Value)) -> Fixture {
            let mut value: Value = serde_json::from_slice(&self.manifest).unwrap();
            mutate(&mut value);
            let manifest = serde_json::to_vec_pretty(&value).unwrap();
            let signature = sign(&self.pkcs8, &manifest);
            Fixture {
                pkcs8: self.pkcs8.clone(),
                public_key: self.public_key.clone(),
                manifest,
                signature,
                catalog: self.catalog.clone(),
                catalog_path: self.catalog_path.clone(),
            }
        }
    }

    fn cache_path(dir: &Path) -> PathBuf {
        if let Ok(scopes) = fs::read_dir(dir.join("scopes")) {
            for scope in scopes.flatten() {
                if let Ok(digest) = fs::read_to_string(scope.path().join("head")) {
                    return scope.path().join("catalogs").join(digest);
                }
            }
        }
        dir.join(CACHE_DIR)
    }

    fn cache_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(cache_path(dir))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = fs::read_dir(cache_path(dir))
            .unwrap()
            .flatten()
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[test]
    fn unconfigured_trust_keeps_startup_bundled_and_never_fetches() {
        let dir = TempDir::new("unconfigured-trust");
        let client = IndexClient::with_optional_key(
            dir.0.clone(),
            None,
            PRIMARY_BASE_URL.into(),
            FALLBACK_BASE_URL.into(),
        );
        let status = client.status();
        assert_eq!(status.trust, IndexTrust::Unconfigured);
        assert_eq!(status.source, "bundled");
        assert!(
            status
                .last_error
                .unwrap()
                .contains("trust is not configured")
        );
        assert_eq!(client.view().unwrap().source, IndexSource::Bundled);
        let fetch = FakeFetch::new(Vec::new());
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, true)).unwrap_err();
        assert_eq!(error.stage, "trust");
        assert!(lock(&fetch.requests).is_empty());
    }

    #[test]
    fn cache_is_bound_to_configured_authority_and_origins() {
        let dir = TempDir::new("cache-trust-scope");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh(&FakeFetch::new(fixture.responses()), true))
            .unwrap();
        assert_eq!(fixture.client(&dir.0).status().source, "cache");
        let (_, other_key) = keypair();
        let foreign_key = IndexClient::with_key(dir.0.clone(), other_key);
        assert_eq!(foreign_key.status().source, "bundled");
        assert!(foreign_key.status().last_error.is_none());
        let foreign_origin = IndexClient::with_bases(
            dir.0.clone(),
            fixture.public_key.clone(),
            "https://other.example.com/index/".into(),
            FALLBACK_BASE_URL.into(),
        );
        assert_eq!(foreign_origin.status().source, "bundled");
        assert!(foreign_origin.status().last_error.is_none());
        let state_path = cache_path(&dir.0).join(STATE_FILE);
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state.as_object_mut().unwrap().remove("trust_scope");
        fs::write(state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let unscoped = fixture.client(&dir.0);
        assert_eq!(unscoped.status().source, "bundled");
        assert!(
            unscoped
                .status()
                .last_error
                .unwrap()
                .contains("different authority or origin")
        );
    }

    #[test]
    fn same_timestamp_different_manifest_is_refused_without_replacing_the_view() {
        let dir = TempDir::new("equivocation");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let before = snapshot(&dir.0);
        let conflict =
            fixture.mutate_manifest(|value| value["catalog_version"] = "conflict".into());
        let fetch = FakeFetch::new(conflict.responses());
        let error =
            tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400)).unwrap_err();
        assert_eq!(error.stage, "equivocation");
        assert_eq!(lock(&fetch.requests).len(), 2);
        assert_eq!(snapshot(&dir.0), before);
        assert_eq!(
            client.status().catalog_version.as_deref(),
            Some("2026.10.22.1")
        );
    }

    #[test]
    fn malformed_signed_generated_at_cannot_disable_rollback_checks() {
        let dir = TempDir::new("invalid-date");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let invalid =
            fixture.mutate_manifest(|value| value["generated_at"] = "2026-02-30T00:00:00Z".into());
        let before = snapshot(&dir.0);
        let fetch = FakeFetch::new(invalid.responses());
        let error =
            tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400)).unwrap_err();
        assert_eq!(error.stage, "schema");
        assert_eq!(lock(&fetch.requests).len(), 2);
        assert_eq!(snapshot(&dir.0), before);
    }

    #[test]
    fn legacy_scoped_cache_migrates_after_full_catalog_validation() {
        let dir = TempDir::new("legacy-migration");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh(&FakeFetch::new(fixture.responses()), true))
            .unwrap();
        let legacy = dir.0.join(CACHE_DIR);
        fs::create_dir(&legacy).unwrap();
        let before = snapshot(&dir.0);
        for (name, bytes) in &before {
            fs::write(legacy.join(name), bytes).unwrap();
        }
        fs::remove_dir_all(dir.0.join("scopes")).unwrap();
        let restarted = fixture.client(&dir.0);
        assert_eq!(restarted.status().source, "cache");
        assert!(dir.0.join("scopes").is_dir());
        for (name, bytes) in before {
            assert_eq!(fs::read(legacy.join(name)).unwrap(), bytes);
        }
    }

    #[test]
    fn remote_refresh_writes_cache_and_serves_entries() {
        let dir = TempDir::new("refresh");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        assert_eq!(client.status().source, "bundled");
        let fetch = FakeFetch::new(fixture.responses());
        let outcome = tauri::async_runtime::block_on(client.refresh(&fetch, true)).unwrap();
        assert!(outcome.performed && outcome.changed);
        assert_eq!(outcome.status.source, "remote");
        assert_eq!(
            outcome.status.catalog_version.as_deref(),
            Some("2026.10.22.1")
        );
        let view = client.view().unwrap();
        assert_eq!(view.source, IndexSource::Remote);
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].id, "example.reverb-1");
        assert!(cache_files(&dir.0).contains(&fixture.catalog_path));
    }

    #[test]
    fn cached_catalog_loads_after_restart() {
        let dir = TempDir::new("restart");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh(&FakeFetch::new(fixture.responses()), true))
            .unwrap();
        let restarted = fixture.client(&dir.0);
        assert_eq!(restarted.status().source, "cache");
        assert_eq!(restarted.view().unwrap().entries.len(), 1);
    }

    /// Application setup builds the client and the panel paints from the disk
    /// cache; only opening the browser asks the index for anything.
    #[test]
    fn startup_asks_for_nothing_and_opening_the_browser_asks_once() {
        let dir = TempDir::new("demand");
        let fixture = fixture("0.1.0");
        let fetch = FakeFetch::new(fixture.responses());
        // What setup does, then what the first paint of the panel reads.
        let client = fixture.client(&dir.0);
        let _ = client.view().unwrap();
        let _ = client.status();
        assert!(
            fetch.requests.lock().unwrap().is_empty(),
            "construction and the first paint must not fetch the index"
        );

        let outcome = tauri::async_runtime::block_on(client.refresh(&fetch, false)).unwrap();
        assert!(outcome.performed && outcome.changed);
        assert_eq!(outcome.status.source, "remote");
        let requests = fetch.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "one open fetches the manifest, its signature and the catalog"
        );
        assert!(requests[0].0.ends_with(MANIFEST_FILE));
        assert!(requests[0].0.starts_with(PRIMARY_BASE_URL));
    }

    /// The failure of an asked-for fetch is reported where the person sees it:
    /// `status.last_error` feeds the panel banner, and nothing logs it.
    #[test]
    fn a_failed_open_is_reported_in_the_status() {
        let dir = TempDir::new("failed-open");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        let fetch = FakeFetch::new(vec![
            FetchResponse {
                status: 404,
                body: Vec::new(),
                etag: None,
            },
            FetchResponse {
                status: 404,
                body: Vec::new(),
                etag: None,
            },
        ]);
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, false))
            .expect_err("both bases answer 404");
        assert_eq!(error.stage, "network");
        assert!(error.message.contains("HTTP 404"));
        let status = client.status();
        assert_eq!(status.source, "bundled");
        assert_eq!(
            status.last_error.as_deref(),
            Some(error.to_string().as_str())
        );
        assert_eq!(client.view().unwrap().source, IndexSource::Bundled);
    }

    #[test]
    fn not_modified_keeps_cache_and_updates_checked_at() {
        let dir = TempDir::new("not-modified");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let before = snapshot(&dir.0);
        let fetch = FakeFetch::new(vec![FetchResponse {
            status: 304,
            body: Vec::new(),
            etag: Some("\"manifest-v2\"".into()),
        }]);
        let outcome =
            tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400)).unwrap();
        assert!(!outcome.changed);
        assert_eq!(outcome.status.source, "remote");
        let stable = |files: &[(String, Vec<u8>)]| {
            files
                .iter()
                .filter(|(name, _)| name != STATE_FILE)
                .cloned()
                .collect::<Vec<_>>()
        };
        let after = snapshot(&dir.0);
        assert_eq!(stable(&after), stable(&before));
        let state: CacheState = serde_json::from_slice(
            &after
                .iter()
                .find(|(name, _)| name == STATE_FILE)
                .expect("state file")
                .1,
        )
        .unwrap();
        assert_eq!(state.checked_at, 1_000_400);
        assert_eq!(
            fetch.requests.lock().unwrap()[0].1.as_deref(),
            Some("\"manifest-v1\"")
        );
    }

    #[test]
    fn tampered_signature_rejected_and_cache_kept() {
        let dir = TempDir::new("tampered-signature");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let before = snapshot(&dir.0);
        let mut tampered = fixture.mutate_manifest(|value| {
            value["catalog_version"] = json!("2026.10.22.2");
        });
        tampered.signature[0] ^= 0x01;
        let fetch = FakeFetch::new(tampered.responses());
        let error = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400))
            .expect_err("tampered signature");
        assert_eq!(error.stage, "signature");
        assert_eq!(snapshot(&dir.0), before);
        assert_eq!(client.view().unwrap().source, IndexSource::Remote);
        assert!(client.status().last_error.is_some());
    }

    #[test]
    fn tampered_manifest_rejected_and_cache_kept() {
        let dir = TempDir::new("tampered-manifest");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let before = snapshot(&dir.0);
        let mut manifest = String::from_utf8(fixture.manifest.clone()).unwrap();
        manifest = manifest.replace("2026.10.22.1", "2026.10.22.2");
        let tampered = Fixture {
            manifest: manifest.into_bytes(),
            ..fixture
        };
        let fetch = FakeFetch::new(tampered.responses());
        let error = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400))
            .expect_err("tampered manifest");
        assert_eq!(error.stage, "signature");
        assert_eq!(snapshot(&dir.0), before);
    }

    #[test]
    fn wrong_pinned_key_rejected() {
        let dir = TempDir::new("wrong-pinned-key");
        let fixture = fixture("0.1.0");
        let (_, other_public_key) = keypair();
        let client = IndexClient::with_key(dir.0.clone(), other_public_key);
        let fetch = FakeFetch::new(fixture.responses());
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, true))
            .expect_err("wrong pinned key");
        assert_eq!(error.stage, "key_id");
        assert!(error.message.contains("does not match the pinned key"));
        assert_eq!(client.status().source, "bundled");
    }

    #[test]
    fn unsigned_manifest_rejected() {
        let dir = TempDir::new("unsigned");
        let fixture = fixture("0.1.0");
        let mut value: Value = serde_json::from_slice(&fixture.manifest).unwrap();
        value.as_object_mut().unwrap().remove("signature");
        let unsigned = Fixture {
            manifest: serde_json::to_vec_pretty(&value).unwrap(),
            ..fixture
        };
        let client = unsigned.client(&dir.0);
        let error = tauri::async_runtime::block_on(
            client.refresh(&FakeFetch::new(unsigned.responses()), true),
        )
        .expect_err("unsigned manifest");
        assert_eq!(error.stage, "signature");
    }

    #[test]
    fn catalog_sha256_mismatch_rejected() {
        let dir = TempDir::new("sha-mismatch");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        let mut responses = fixture.responses();
        let tampered = responses.pop().unwrap();
        let mut body = tampered.body;
        body[0] ^= 0xff;
        responses.push(FetchResponse {
            status: 200,
            body,
            etag: None,
        });
        let error =
            tauri::async_runtime::block_on(client.refresh(&FakeFetch::new(responses), true))
                .expect_err("sha mismatch");
        assert_eq!(error.stage, "catalog_hash");
        assert!(!dir.0.join(CACHE_DIR).join(MANIFEST_FILE).exists());
    }

    #[test]
    fn oversized_decompression_rejected() {
        let dir = TempDir::new("bomb");
        let fixture = fixture("0.1.0");
        let bomb = zstd::bulk::compress(&vec![0u8; MAX_CATALOG_BYTES + 1024], 9).unwrap();
        let mut manifest_value: Value = serde_json::from_slice(&fixture.manifest).unwrap();
        manifest_value["catalog"]["sha256"] = json!(sha256_hex(&bomb));
        manifest_value["catalog"]["path"] =
            json!(format!("catalog-{}.json.zst", sha256_hex(&bomb)));
        manifest_value["catalog"]["bytes"] = json!(bomb.len() as u64);
        manifest_value["catalog"]["uncompressed_bytes"] = json!(1024u64);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest_value).unwrap();
        let signature = sign(&fixture.pkcs8, &manifest_bytes);
        let fetch = FakeFetch::new(vec![
            FetchResponse {
                status: 200,
                body: manifest_bytes,
                etag: None,
            },
            FetchResponse {
                status: 200,
                body: signature,
                etag: None,
            },
            FetchResponse {
                status: 200,
                body: bomb,
                etag: None,
            },
        ]);
        let error = tauri::async_runtime::block_on(fixture.client(&dir.0).refresh(&fetch, true))
            .expect_err("compression bomb");
        assert_eq!(error.stage, "decompression");
        assert!(error.message.contains("ceiling"));
    }

    #[test]
    fn newer_min_app_version_refused_and_cache_kept() {
        let dir = TempDir::new("min-version");
        let base = fixture("0.1.0");
        let client = base.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(base.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let before = snapshot(&dir.0);
        let newer = base.mutate_manifest(|value| {
            value["min_app_version"] = json!("99.0.0");
        });
        let error = tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(newer.responses()),
            true,
            1_000_400,
        ))
        .expect_err("newer min_app_version");
        assert_eq!(error.stage, "min_app_version");
        assert!(error.message.contains("99.0.0"));
        assert_eq!(snapshot(&dir.0), before);
        assert_eq!(client.view().unwrap().source, IndexSource::Remote);
    }

    #[test]
    fn newer_manifest_schema_refused() {
        let dir = TempDir::new("schema");
        let fixture = fixture("0.1.0");
        let mut value: Value = serde_json::from_slice(&fixture.manifest).unwrap();
        value["schema"] = json!(2);
        let manifest = serde_json::to_vec_pretty(&value).unwrap();
        let signature = sign(&fixture.pkcs8, &manifest);
        let fetch = FakeFetch::new(vec![
            FetchResponse {
                status: 200,
                body: manifest,
                etag: None,
            },
            FetchResponse {
                status: 200,
                body: signature,
                etag: None,
            },
        ]);
        let error = tauri::async_runtime::block_on(fixture.client(&dir.0).refresh(&fetch, true))
            .expect_err("newer schema");
        assert_eq!(error.stage, "schema");
    }

    #[test]
    fn bundled_fallback_when_cache_empty() {
        let dir = TempDir::new("bundled");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        let view = client.view().unwrap();
        assert_eq!(view.source, IndexSource::Bundled);
        assert!(!view.entries.is_empty());
        let status = client.status();
        assert_eq!(status.source, "bundled");
        assert!(status.fetched_at.is_none());
        assert!(status.stale_days.is_none());
    }

    #[test]
    fn manual_refresh_floor_honoured() {
        let dir = TempDir::new("floor");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            1_000_000,
        ))
        .unwrap();
        let error = tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(Vec::new()),
            true,
            1_000_120,
        ))
        .expect_err("manual floor");
        assert_eq!(error.stage, "policy");
        assert!(error.message.contains("try again in 180 seconds"));
        let status = client.status_at(1_000_120);
        assert_eq!(status.next_refresh_in_seconds, 180);
    }

    /// Reopening the browser inside the interval paints from the cache and
    /// leaves the origin alone; the fake panics if it is asked for a response.
    #[test]
    fn a_second_open_inside_the_interval_makes_no_request() {
        let dir = TempDir::new("open-again");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            false,
            1_000_000,
        ))
        .unwrap();
        let fetch = FakeFetch::new(Vec::new());
        let outcome =
            tauri::async_runtime::block_on(client.refresh_at(&fetch, false, 1_000_060)).unwrap();
        assert!(!outcome.performed);
        assert_eq!(outcome.status.source, "remote");
        assert!(fetch.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn status_reports_staleness() {
        let dir = TempDir::new("staleness");
        let fixture = fixture("0.1.0");
        let client = fixture.client(&dir.0);
        let now = 1_000_000;
        tauri::async_runtime::block_on(client.refresh_at(
            &FakeFetch::new(fixture.responses()),
            true,
            now,
        ))
        .unwrap();
        let status = client.status_at(now + 15 * 86_400);
        assert_eq!(status.source, "remote");
        assert_eq!(status.stale_days, Some(15));
        assert_eq!(status.fetched_at, Some(now));
        assert!(status.generated_at.as_deref() == Some("2026-10-22T08:00:00Z"));
    }

    #[test]
    fn mirrors_are_used_when_primary_catalog_fails() {
        let dir = TempDir::new("mirrors");
        let fixture = fixture("0.1.0");
        let mut value: Value = serde_json::from_slice(&fixture.manifest).unwrap();
        value["mirrors"] = json!([{
            "name": "raw",
            "base_url": "https://mirror.invalid/marketplace/main/"
        }]);
        let manifest = serde_json::to_vec_pretty(&value).unwrap();
        let signature = sign(&fixture.pkcs8, &manifest);
        let fetch = FakeFetch::new(vec![
            FetchResponse {
                status: 200,
                body: manifest,
                etag: None,
            },
            FetchResponse {
                status: 200,
                body: signature,
                etag: None,
            },
            FetchResponse {
                status: 500,
                body: Vec::new(),
                etag: None,
            },
            FetchResponse {
                status: 200,
                body: fixture.catalog.clone(),
                etag: None,
            },
        ]);
        let outcome =
            tauri::async_runtime::block_on(fixture.client(&dir.0).refresh(&fetch, true)).unwrap();
        assert!(outcome.changed);
        let requests = fetch.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(url, _)| url.contains("mirror.invalid"))
        );
    }

    #[test]
    fn version_comparison_ignores_prerelease_suffixes() {
        assert!(version_newer("0.2.0", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.1.0-beta.1", "0.1.0"));
        assert!(version_newer("garbage", "0.1.0"));
    }

    // ========================================================================
    // Self-hosted rehearsal: real builder, real loopback HTTP, real client
    // ========================================================================

    use oc_cli::plugin_index as builder;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::AtomicBool;
    use std::thread::{self, JoinHandle};

    struct BuiltIndex {
        dir: PathBuf,
        manifest: Vec<u8>,
        signature: Vec<u8>,
        catalog: Vec<u8>,
        catalog_path: String,
    }

    fn rehearsal_key(dir: &Path) -> (PathBuf, PathBuf, Vec<u8>) {
        let report = builder::keygen(dir, false).expect("keygen");
        let public_key = hex_decode(&report.public_key).expect("public key hex");
        (report.key_path, report.public_key_path, public_key)
    }

    fn rehearsal_catalog(tag: &str) -> Value {
        json!({
            "schema": 1,
            "generated_at": "2026-10-22T08:00:00Z",
            "entries": [{
                "id": format!("example.{tag}"),
                "name": format!("Example {tag}"),
                "vendor": "Example Audio",
                "tags": ["freeware"],
                "installs": [{
                    "kind": "direct",
                    "platform": "macos-aarch64",
                    "format": "vst3",
                    "url": "https://plugins.example.com/example.zip"
                }]
            }]
        })
    }

    fn build_index(
        dir: &Path,
        key: &Path,
        tag: &str,
        generated_at: &str,
        catalog_version: &str,
    ) -> BuiltIndex {
        let source = dir.join(format!("source-{tag}.json"));
        fs::write(
            &source,
            serde_json::to_vec_pretty(&rehearsal_catalog(tag)).unwrap(),
        )
        .unwrap();
        let out = dir.join(format!(
            "dist-{tag}-{}",
            generated_at.replace([':', '-'], "")
        ));
        let report = builder::build(&builder::BuildOptions {
            catalog_path: source,
            out_dir: out.clone(),
            key_path: Some(key.to_path_buf()),
            catalog_version: Some(catalog_version.to_string()),
            min_app_version: "0.1.0".to_string(),
            generated_at: Some(generated_at.to_string()),
            level: builder::DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect("build signed index");
        BuiltIndex {
            manifest: fs::read(out.join(builder::MANIFEST_FILE)).unwrap(),
            signature: fs::read(out.join(builder::SIGNATURE_FILE)).unwrap(),
            catalog: fs::read(out.join(&report.catalog_path)).unwrap(),
            catalog_path: report.catalog_path,
            dir: out,
        }
    }

    #[derive(Clone)]
    struct HttpReply {
        status: u16,
        body: Vec<u8>,
        content_type: &'static str,
    }

    impl HttpReply {
        fn ok(body: Vec<u8>, content_type: &'static str) -> Self {
            Self {
                status: 200,
                body,
                content_type,
            }
        }
    }

    #[derive(Clone, Debug)]
    struct RequestLog {
        method: String,
        path: String,
        query: Option<String>,
        headers: Vec<String>,
    }

    struct LoopbackServer {
        addr: SocketAddr,
        routes: Arc<Mutex<HashMap<String, HttpReply>>>,
        requests: Arc<Mutex<Vec<RequestLog>>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl LoopbackServer {
        fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("loopback addr");
            let routes = Arc::new(Mutex::new(HashMap::new()));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let routes = routes.clone();
                let requests = requests.clone();
                let stop = stop.clone();
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                // An accepted socket inherits O_NONBLOCK from a
                                // nonblocking listener on macOS; serve it blocking.
                                if stream.set_nonblocking(false).is_ok() {
                                    serve_http(stream, &routes, &requests);
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(_) => thread::sleep(Duration::from_millis(1)),
                        }
                    }
                })
            };
            Self {
                addr,
                routes,
                requests,
                stop,
                thread: Some(thread),
            }
        }

        fn base(&self, prefix: &str) -> String {
            format!("http://{}{}", self.addr, prefix)
        }

        fn publish(&self, prefix: &str, built: &BuiltIndex) {
            let prefix = prefix.trim_end_matches('/');
            let mut routes = self.routes.lock().unwrap();
            routes.insert(
                format!("{prefix}/{}", builder::MANIFEST_FILE),
                HttpReply::ok(built.manifest.clone(), "application/json"),
            );
            routes.insert(
                format!("{prefix}/{}", builder::SIGNATURE_FILE),
                HttpReply::ok(built.signature.clone(), "application/octet-stream"),
            );
            routes.insert(
                format!("{prefix}/{}", built.catalog_path),
                HttpReply::ok(built.catalog.clone(), "application/zstd"),
            );
        }

        fn set(&self, path: &str, reply: HttpReply) {
            self.routes.lock().unwrap().insert(path.to_string(), reply);
        }

        fn remove(&self, path: &str) {
            self.routes.lock().unwrap().remove(path);
        }

        fn take_requests(&self) -> Vec<RequestLog> {
            std::mem::take(&mut *self.requests.lock().unwrap())
        }
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn serve_http(
        mut stream: TcpStream,
        routes: &Mutex<HashMap<String, HttpReply>>,
        requests: &Mutex<Vec<RequestLog>>,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let Ok(reader_stream) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(reader_stream);
        let mut request_line = String::new();
        match reader.read_line(&mut request_line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push(line.trim_end().to_string());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default();
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path, Some(query.to_string())),
            None => (target, None),
        };
        requests.lock().unwrap().push(RequestLog {
            method,
            path: path.to_string(),
            query,
            headers,
        });
        let reply = routes
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or(HttpReply {
                status: 404,
                body: Vec::new(),
                content_type: "text/plain",
            });
        let reason = match reply.status {
            200 => "OK",
            304 => "Not Modified",
            _ => "Not Found",
        };
        let mut response = format!("HTTP/1.1 {} {reason}\r\n", reply.status).into_bytes();
        response.extend_from_slice(format!("Content-Length: {}\r\n", reply.body.len()).as_bytes());
        response.extend_from_slice(format!("Content-Type: {}\r\n", reply.content_type).as_bytes());
        response.extend_from_slice(b"Connection: close\r\n\r\n");
        response.extend_from_slice(&reply.body);
        let _ = stream.write_all(&response);
        let _ = stream.flush();
    }

    fn http_client(dir: &Path, public_key: &[u8], base: &str) -> IndexClient {
        IndexClient::with_bases(
            dir.to_path_buf(),
            public_key.to_vec(),
            base.to_string(),
            base.to_string(),
        )
    }

    fn assert_three_requests(
        requests: &[RequestLog],
        prefix: &str,
        catalog_path: &str,
        manifest_sha256: &str,
    ) {
        assert_eq!(requests.len(), 3, "unexpected requests: {requests:#?}");
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].path,
            format!("{prefix}{}", builder::MANIFEST_FILE)
        );
        assert_eq!(requests[0].query, None);
        assert_eq!(
            requests[1].path,
            format!("{prefix}{}", builder::SIGNATURE_FILE)
        );
        let expected_query = format!("v={manifest_sha256}");
        assert_eq!(requests[1].query.as_deref(), Some(expected_query.as_str()));
        assert_eq!(requests[2].path, format!("{prefix}{catalog_path}"));
        assert_eq!(requests[2].query, None);
        for request in requests {
            let headers = request.headers.join("\n").to_ascii_lowercase();
            assert!(
                headers.contains("cache-control: no-cache"),
                "missing no-cache: {headers}"
            );
            assert!(
                headers.contains("user-agent: opencompose/"),
                "missing user agent: {headers}"
            );
        }
    }

    fn rehearse_selfhosted(tag: &str, prefix: &str) {
        let dir = TempDir::new(tag);
        let (key, public_key_path, public_key) = rehearsal_key(&dir.0.join("signing"));
        let built = build_index(&dir.0, &key, tag, "2026-10-22T08:00:00Z", "2026.10.22.1");
        builder::verify(&built.dir, &public_key_path).expect("built index verifies");

        let server = LoopbackServer::start();
        server.publish(prefix, &built);
        let client = http_client(&dir.0.join("cache"), &public_key, &server.base(prefix));
        let fetch = HttpFetch::new().expect("http client");
        let outcome = tauri::async_runtime::block_on(client.refresh(&fetch, true))
            .expect("refresh over http");

        assert!(outcome.performed && outcome.changed);
        assert_eq!(outcome.status.source, "remote");
        assert_eq!(
            outcome.status.catalog_version.as_deref(),
            Some("2026.10.22.1")
        );
        assert_eq!(
            outcome.status.generated_at.as_deref(),
            Some("2026-10-22T08:00:00Z")
        );
        let view = client.view().expect("catalog view");
        assert_eq!(view.source, IndexSource::Remote);
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].id, format!("example.{tag}"));
        assert_eq!(view.entries[0].name, format!("Example {tag}"));
        assert_eq!(client.status().source, "remote");

        let cache = cache_path(&dir.0.join("cache"));
        assert!(cache.join(builder::MANIFEST_FILE).exists());
        assert!(cache.join(builder::SIGNATURE_FILE).exists());
        assert!(cache.join(&built.catalog_path).exists());
        assert_eq!(built.signature.len(), 64);

        let requests = server.take_requests();
        assert_three_requests(
            &requests,
            prefix,
            &built.catalog_path,
            &sha256_hex(&built.manifest),
        );
    }

    #[test]
    fn selfhosted_rehearsal_serves_signed_index_over_plain_http() {
        rehearse_selfhosted("root", "/");
    }

    #[test]
    fn selfhosted_rehearsal_serves_signed_index_under_a_path_prefix() {
        rehearse_selfhosted("prefix", "/plugin-index/");
    }

    #[test]
    fn selfhosted_rehearsal_tampered_catalog_reports_catalog_hash() {
        let dir = TempDir::new("http-tampered-catalog");
        let (key, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let built = build_index(
            &dir.0,
            &key,
            "catalog",
            "2026-10-22T08:00:00Z",
            "2026.10.22.1",
        );
        let server = LoopbackServer::start();
        server.publish("/", &built);
        let mut tampered = built.catalog.clone();
        tampered[0] ^= 0xff;
        server.set(
            &format!("/{}", built.catalog_path),
            HttpReply::ok(tampered, "application/zstd"),
        );

        let client = http_client(&dir.0.join("cache"), &public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, true))
            .expect_err("tampered catalog");
        assert_eq!(error.stage, "catalog_hash");
        assert_eq!(client.status().source, "bundled");
        assert!(
            !cache_path(&dir.0.join("cache"))
                .join(builder::MANIFEST_FILE)
                .exists()
        );
    }

    #[test]
    fn selfhosted_rehearsal_tampered_manifest_reports_signature() {
        let dir = TempDir::new("http-tampered-manifest");
        let (key, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let built = build_index(
            &dir.0,
            &key,
            "manifest",
            "2026-10-22T08:00:00Z",
            "2026.10.22.1",
        );
        let server = LoopbackServer::start();
        server.publish("/", &built);
        let manifest = String::from_utf8(built.manifest.clone())
            .unwrap()
            .replace("2026.10.22.1", "2026.10.22.2");
        server.set(
            &format!("/{}", builder::MANIFEST_FILE),
            HttpReply::ok(manifest.into_bytes(), "application/json"),
        );

        let client = http_client(&dir.0.join("cache"), &public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, true))
            .expect_err("tampered manifest");
        assert_eq!(error.stage, "signature");
        assert_eq!(client.status().source, "bundled");
    }

    #[test]
    fn selfhosted_rehearsal_foreign_signature_reports_key_id() {
        let dir = TempDir::new("http-foreign-key");
        let (_, _, pinned_public_key) = rehearsal_key(&dir.0.join("pinned"));
        let (other_key, _, _) = rehearsal_key(&dir.0.join("other"));
        let foreign = build_index(
            &dir.0,
            &other_key,
            "foreign",
            "2026-10-22T08:00:00Z",
            "2026.10.22.1",
        );
        let server = LoopbackServer::start();
        server.publish("/", &foreign);

        let client = http_client(&dir.0.join("cache"), &pinned_public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");
        let error =
            tauri::async_runtime::block_on(client.refresh(&fetch, true)).expect_err("foreign key");
        assert_eq!(error.stage, "key_id");
        assert_eq!(client.status().source, "bundled");
    }

    #[test]
    fn selfhosted_rehearsal_missing_signature_keeps_last_good_cache() {
        let dir = TempDir::new("http-missing-signature");
        let (key, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let first = build_index(
            &dir.0,
            &key,
            "first",
            "2026-10-23T08:00:00Z",
            "2026.10.23.1",
        );
        let next = build_index(&dir.0, &key, "next", "2026-10-24T08:00:00Z", "2026.10.24.1");
        let server = LoopbackServer::start();
        server.publish("/", &first);
        let client = http_client(&dir.0.join("cache"), &public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");
        tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_000))
            .expect("first refresh");

        let before = snapshot(&dir.0.join("cache"));
        server.publish("/", &next);
        server.remove(&format!("/{}", builder::SIGNATURE_FILE));
        let error = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400))
            .expect_err("missing signature must fail closed");
        assert_eq!(error.stage, "network");
        assert_eq!(snapshot(&dir.0.join("cache")), before);
        let view = client.view().unwrap();
        assert_eq!(view.source, IndexSource::Remote);
        assert_eq!(view.entries[0].id, "example.first");
        assert_eq!(
            client.status_at(1_000_400).catalog_version.as_deref(),
            Some("2026.10.23.1")
        );
        assert!(
            client
                .status_at(1_000_400)
                .last_error
                .is_some_and(|error| error.starts_with("network:"))
        );

        server.publish("/", &next);
        let recovered = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_800))
            .expect("recover once the signature is back");
        assert!(recovered.changed);
        assert_eq!(
            recovered.status.catalog_version.as_deref(),
            Some("2026.10.24.1")
        );
    }

    #[test]
    fn selfhosted_rehearsal_404_reports_network() {
        let dir = TempDir::new("http-404");
        let (_, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let server = LoopbackServer::start();
        let client = http_client(&dir.0.join("cache"), &public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");
        let error = tauri::async_runtime::block_on(client.refresh(&fetch, true)).expect_err("404");
        assert_eq!(error.stage, "network");
        assert!(error.message.contains("404"), "message: {}", error.message);
        assert_eq!(client.status().source, "bundled");
        let requests = server.take_requests();
        assert!(!requests.is_empty(), "error: {error}");
        assert!(
            requests
                .iter()
                .all(|request| request.path == format!("/{}", builder::MANIFEST_FILE)),
            "requests: {requests:#?}"
        );
    }

    #[test]
    fn another_client_publication_invalidates_fresh_gui_install_authority() {
        let dir = TempDir::new("held-view");
        let (key, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let first = build_index(&dir.0, &key, "a", "2026-10-23T08:00:00Z", "first");
        let newer = build_index(&dir.0, &key, "b", "2026-10-24T08:00:00Z", "newer");
        let server = LoopbackServer::start();
        let cache = dir.0.join("cache");
        let client = http_client(&cache, &public_key, &server.base("/"));
        let fetch = HttpFetch::new().unwrap();
        server.publish("/", &first);
        tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_000)).unwrap();
        let held = client.view().unwrap();
        held.authority.check().unwrap();
        let other = http_client(&cache, &public_key, &server.base("/"));
        server.publish("/", &newer);
        tauri::async_runtime::block_on(other.refresh_at(&fetch, true, 1_000_400)).unwrap();

        assert_eq!(
            held.entries[0].id, "example.a",
            "last-good browsing remains available"
        );
        assert!(
            held.authority
                .publication_guard()
                .unwrap_err()
                .contains("rollback")
        );
        let requests = server.requests.lock().unwrap().len();
        let error = tauri::async_runtime::block_on(crate::plugin_store::install(
            &held.authority,
            held.entries.as_slice(),
            "example.a",
            false,
            Some(&dir.0.join("plugins")),
        ))
        .unwrap_err();
        assert!(error.contains("rollback"), "{error}");
        assert!(!dir.0.join("plugins").exists());
        assert_eq!(server.requests.lock().unwrap().len(), requests);
        let no_requests = FakeFetch::new(Vec::new());
        let error =
            tauri::async_runtime::block_on(client.refresh_at(&no_requests, false, 1_000_460))
                .unwrap_err();
        assert_eq!(error.stage, "rollback");
        assert!(no_requests.requests.lock().unwrap().is_empty());
        assert!(
            client
                .status_at(1_000_460)
                .last_error
                .unwrap()
                .contains("rollback")
        );
    }

    #[test]
    fn rollback_rejects_older_generated_at_and_accepts_fresh_republish() {
        let dir = TempDir::new("rollback");
        let (key, _, public_key) = rehearsal_key(&dir.0.join("signing"));
        let first = build_index(&dir.0, &key, "a", "2026-10-23T08:00:00Z", "2026.10.23.1");
        let newer = build_index(&dir.0, &key, "b", "2026-10-24T08:00:00Z", "2026.10.24.1");
        let republished = build_index(&dir.0, &key, "a", "2026-10-25T08:00:00Z", "2026.10.23.1");
        assert_eq!(
            first.catalog_path, republished.catalog_path,
            "a rollback repoints at the existing immutable catalog"
        );

        let server = LoopbackServer::start();
        let client = http_client(&dir.0.join("cache"), &public_key, &server.base("/"));
        let fetch = HttpFetch::new().expect("http client");

        server.publish("/", &first);
        let accepted = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_000))
            .expect("first publish");
        assert!(accepted.changed);
        assert_eq!(
            accepted.status.catalog_version.as_deref(),
            Some("2026.10.23.1")
        );
        assert_eq!(client.view().unwrap().entries[0].id, "example.a");

        server.publish("/", &newer);
        let accepted = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_400))
            .expect("newer catalog");
        assert!(accepted.changed);
        assert_eq!(client.view().unwrap().entries[0].id, "example.b");

        // A rollback is legitimate when it carries a fresh generated_at.
        server.publish("/", &republished);
        let accepted = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_000_800))
            .expect("fresh rollback republish");
        assert!(accepted.changed);
        assert_eq!(
            accepted.status.catalog_version.as_deref(),
            Some("2026.10.23.1")
        );
        assert_eq!(
            accepted.status.generated_at.as_deref(),
            Some("2026-10-25T08:00:00Z")
        );
        assert_eq!(client.view().unwrap().entries[0].id, "example.a");

        // Replaying the old signed manifest must not downgrade the cache.
        let before = snapshot(&dir.0.join("cache"));
        server.publish("/", &first);
        let error = tauri::async_runtime::block_on(client.refresh_at(&fetch, true, 1_001_200))
            .expect_err("older manifest must be rejected");
        assert_eq!(error.stage, "rollback");
        assert!(
            error.message.contains("2026-10-23T08:00:00Z")
                && error.message.contains("2026-10-25T08:00:00Z"),
            "message: {}",
            error.message
        );
        assert_eq!(snapshot(&dir.0.join("cache")), before);
        assert_eq!(client.view().unwrap().entries[0].id, "example.a");
        assert_eq!(
            client.status_at(1_001_200).generated_at.as_deref(),
            Some("2026-10-25T08:00:00Z")
        );
        assert!(
            client
                .status()
                .last_error
                .is_some_and(|error| error.starts_with("rollback:"))
        );
    }
}
