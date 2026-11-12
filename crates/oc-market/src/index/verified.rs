//! Authentication and bounded request sequencing for signed catalogs.

use std::collections::HashSet;
use std::io::Read;
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

use super::{key_id, verify_manifest_signature};
use crate::sha256_hex;

/// Primary base URL for the signed plugin index.
// TODO(release): replace the placeholder with the self-hosted origin. Remote
// catalogs stay disabled while `plugin-index.pub` is empty.
pub const PRIMARY_BASE_URL: &str = "https://plugin-index.invalid/";
/// Raw-content fallback base URL for the signed index.
pub const FALLBACK_BASE_URL: &str = "https://plugin-index-fallback.invalid/";
/// Manifest file name inside an index base.
pub const MANIFEST_FILE: &str = "index.json";
/// Detached signature file name.
pub const SIGNATURE_FILE: &str = "index.json.sig";
/// Manifest schema this build understands.
pub const MANIFEST_SCHEMA: u32 = 1;
/// Oldest catalog schema this build accepts.
pub const MIN_CATALOG_SCHEMA: u32 = 1;
/// Catalog schema this build writes and prefers.
pub const CATALOG_SCHEMA: u32 = 2;
/// Largest manifest body accepted from an index.
pub const MAX_MANIFEST_BYTES: usize = 512 * 1024;
/// Largest detached signature accepted.
pub const MAX_SIGNATURE_BYTES: usize = super::SIGNATURE_BYTES;
/// Largest expanded catalog accepted.
pub const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// Largest catalog download accepted.
pub const MAX_CATALOG_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;
/// Largest catalog entry count accepted.
pub const MAX_CATALOG_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// A staged failure while fetching or authenticating the index.
pub struct IndexError {
    pub stage: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<crate::catalog_wire::CatalogError>,
}
impl IndexError {
    /// An error at `stage` with a message.
    pub fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            catalog: None,
        }
    }
    /// A network-stage failure.
    pub fn network(message: impl Into<String>) -> Self {
        Self::new("network", message)
    }
    /// A storage-stage failure.
    pub fn storage(message: impl Into<String>) -> Self {
        Self::new("storage", message)
    }
}
impl std::fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.message)
    }
}
impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.catalog
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}
impl From<crate::catalog_wire::CatalogError> for IndexError {
    fn from(error: crate::catalog_wire::CatalogError) -> Self {
        let stage = if error.code == crate::catalog_wire::CatalogErrorCode::Timestamp {
            "schema"
        } else {
            "catalog_validation"
        };
        Self {
            stage,
            message: error.to_string(),
            catalog: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Signed index manifest naming the catalog and its mirrors.
pub struct Manifest {
    pub schema: u32,
    pub generated_at: String,
    pub catalog_version: String,
    pub min_app_version: String,
    pub catalog: CatalogRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<Mirror>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Where the signed catalog lives and how it is encoded.
pub struct CatalogRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub uncompressed_bytes: u64,
    pub encoding: String,
    pub compression_level: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Detached signature metadata.
pub struct SignatureInfo {
    pub algorithm: String,
    pub path: String,
    pub key_id: String,
    pub encoding: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// One alternate HTTPS base for the index.
pub struct Mirror {
    #[serde(default)]
    pub name: String,
    pub base_url: String,
}

/// Parse the canonical UTC `generated_at` timestamp.
pub fn generated_at(value: &str) -> Result<DateTime<Utc>, IndexError> {
    if value.len() != 20 || !value.ends_with('Z') {
        return Err(IndexError::new(
            "schema",
            "generated_at must be canonical UTC YYYY-MM-DDTHH:MM:SSZ",
        ));
    }
    let date = DateTime::parse_from_rfc3339(value).map_err(|_| {
        IndexError::new(
            "schema",
            "generated_at is not a valid UTC calendar timestamp",
        )
    })?;
    if date.nanosecond() != 0 || date.format("%Y-%m-%dT%H:%M:%SZ").to_string() != value {
        return Err(IndexError::new(
            "schema",
            "generated_at must be canonical UTC YYYY-MM-DDTHH:MM:SSZ",
        ));
    }
    Ok(date.with_timezone(&Utc))
}

/// Whether `required` is newer than `running`; unparsable versions count as newer.
pub fn version_newer(required: &str, running: &str) -> bool {
    match (
        semver::Version::parse(required),
        semver::Version::parse(running),
    ) {
        (Ok(required), Ok(running)) => required.cmp_precedence(&running).is_gt(),
        _ => true,
    }
}

/// Check catalog path, hash, encoding and size limits.
pub fn validate_catalog_ref(catalog: &CatalogRef) -> Result<(), IndexError> {
    if catalog.encoding != "zstd" || !(1..=22).contains(&catalog.compression_level) {
        return Err(IndexError::new(
            "schema",
            "Unsupported catalog encoding or compression level",
        ));
    }
    if catalog.sha256.len() != 64 || !catalog.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IndexError::new(
            "schema",
            "catalog sha256 must be 64 hex characters",
        ));
    }
    let expected = format!("catalog-{}.json.zst", catalog.sha256);
    if catalog.path != expected {
        return Err(IndexError::new(
            "schema",
            format!("catalog path must be exactly {expected}"),
        ));
    }
    if catalog.bytes == 0
        || catalog.bytes > MAX_CATALOG_DOWNLOAD_BYTES as u64
        || catalog.uncompressed_bytes == 0
        || catalog.uncompressed_bytes > MAX_CATALOG_BYTES as u64
    {
        return Err(IndexError::new(
            "catalog_size",
            "Catalog compressed or uncompressed size exceeds the catalog limit",
        ));
    }
    Ok(())
}

/// Check manifest schema, timestamp, catalog ref and mirrors.
pub fn validate_manifest(manifest: &Manifest) -> Result<(), IndexError> {
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(IndexError::new(
            "schema",
            format!("unsupported manifest schema {}", manifest.schema),
        ));
    }
    generated_at(&manifest.generated_at)?;
    if manifest.catalog_version.trim().is_empty() {
        return Err(IndexError::new(
            "schema",
            "catalog_version must not be empty",
        ));
    }
    semver::Version::parse(&manifest.min_app_version)
        .map_err(|_| IndexError::new("schema", "min_app_version must be a semantic version"))?;
    validate_catalog_ref(&manifest.catalog)?;
    if manifest.mirrors.len() > 8 {
        return Err(IndexError::new(
            "schema",
            "At most eight catalog mirrors are supported",
        ));
    }
    for mirror in &manifest.mirrors {
        if mirror.name.trim().is_empty() {
            return Err(IndexError::new("schema", "mirror name must not be empty"));
        }
        if !mirror.base_url.starts_with("https://")
            || !mirror.base_url.ends_with('/')
            || !crate::is_install_url(&mirror.base_url)
        {
            return Err(IndexError::new(
                "schema",
                "Catalog mirrors require an HTTPS base URL ending with /",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
/// Trust inputs for verifying an index.
pub struct Policy {
    pub public_key: Vec<u8>,
    pub origins: Vec<String>,
    /// Publisher verification validates syntax without targeting an app build.
    pub app_version: Option<String>,
}
impl Policy {
    /// Stable scope string for the pinned key and origins.
    pub fn scope(&self) -> String {
        super::trust_scope(
            &self.public_key,
            &self.origins.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }
    /// Refuse a manifest that requires a newer app version.
    pub fn compatible(&self, manifest: &Manifest) -> Result<(), IndexError> {
        if self
            .app_version
            .as_deref()
            .is_some_and(|running| version_newer(&manifest.min_app_version, running))
        {
            return Err(IndexError::new(
                "min_app_version",
                format!(
                    "Plugin catalog requires OpenCompose {}",
                    manifest.min_app_version
                ),
            ));
        }
        Ok(())
    }
}

/// Verify the detached signature and validate the manifest.
pub fn authenticate(
    manifest_bytes: &[u8],
    signature: &[u8],
    public_key: &[u8],
) -> Result<Manifest, IndexError> {
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(IndexError::new("schema", "Manifest exceeds the size limit"));
    }
    let manifest: Manifest = serde_json::from_slice(manifest_bytes)
        .map_err(|error| IndexError::new("schema", format!("manifest JSON: {error}")))?;
    let info = manifest
        .signature
        .as_ref()
        .ok_or_else(|| IndexError::new("signature", "manifest is unsigned; refusing to use it"))?;
    if info.algorithm != "ed25519" || info.encoding != "raw" || info.path != SIGNATURE_FILE {
        return Err(IndexError::new(
            "signature",
            "Unsupported signature algorithm, encoding or path",
        ));
    }
    if public_key.len() != 32 || info.key_id != key_id(public_key) {
        return Err(IndexError::new(
            "key_id",
            "manifest key id does not match the pinned key",
        ));
    }
    verify_manifest_signature(manifest_bytes, signature, public_key)
        .map_err(|error| IndexError::new("signature", error))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Decompress a catalog and enforce its declared size.
pub fn decompress_catalog(bytes: &[u8], expected: u64) -> Result<Vec<u8>, IndexError> {
    decompress_checked(bytes, expected).map(|(bytes, _)| bytes)
}
fn decompress_checked(
    bytes: &[u8],
    expected: u64,
) -> Result<(Vec<u8>, crate::catalog_wire::Catalog), IndexError> {
    if expected == 0
        || expected > MAX_CATALOG_BYTES as u64
        || bytes.len() > MAX_CATALOG_DOWNLOAD_BYTES
    {
        return Err(IndexError::new(
            "catalog_size",
            "Catalog exceeds the size limit",
        ));
    }
    let decoder = zstd::stream::read::Decoder::new(bytes)
        .map_err(|error| IndexError::new("decompression", error.to_string()))?;
    let mut raw = Vec::new();
    decoder
        .take(expected + 1)
        .read_to_end(&mut raw)
        .map_err(|error| IndexError::new("decompression", error.to_string()))?;
    if raw.len() as u64 != expected {
        return Err(IndexError::new(
            "decompression",
            if raw.len() as u64 > expected {
                "Catalog exceeds its declared uncompressed byte ceiling"
            } else {
                "Catalog uncompressed size does not match the manifest"
            },
        ));
    }
    let document = crate::catalog_wire::parse(&raw).map_err(IndexError::from)?;
    Ok((raw, document))
}

#[derive(Debug, Clone)]
/// Rollback and equivocation marker for one authenticated manifest.
pub struct Stamp {
    pub generated_at: DateTime<Utc>,
    pub digest: String,
}
impl Stamp {
    /// Authenticate manifest bytes and record their digest.
    pub fn authenticated(bytes: &[u8], signature: &[u8], key: &[u8]) -> Result<Self, IndexError> {
        let manifest = authenticate(bytes, signature, key)?;
        Ok(Self {
            generated_at: generated_at(&manifest.generated_at)?,
            digest: sha256_hex(bytes),
        })
    }
    /// Refuse rollback or equivocation against this stamp.
    pub fn check(&self, incoming: &Self) -> Result<(), IndexError> {
        if incoming.generated_at < self.generated_at {
            return Err(IndexError::new(
                "rollback",
                format!(
                    "Catalog generated_at {} is older than the highest committed {}",
                    incoming.generated_at.format("%Y-%m-%dT%H:%M:%SZ"),
                    self.generated_at.format("%Y-%m-%dT%H:%M:%SZ")
                ),
            ));
        }
        if incoming.generated_at == self.generated_at && incoming.digest != self.digest {
            return Err(IndexError::new(
                "equivocation",
                "Catalog reuses generated_at for different manifest bytes; publish with a fresh timestamp",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
/// A fully verified catalog and the bytes it came from.
pub struct VerifiedIndex {
    pub manifest: Manifest,
    pub manifest_bytes: Arc<[u8]>,
    pub signature: Arc<[u8]>,
    pub compressed: Arc<[u8]>,
    pub catalog: Arc<[u8]>,
    pub document: Arc<crate::catalog_wire::Catalog>,
    pub source_base: String,
    pub etag: Option<String>,
}
impl VerifiedIndex {
    /// The rollback marker for this index.
    pub fn stamp(&self) -> Result<Stamp, IndexError> {
        Ok(Stamp {
            generated_at: generated_at(&self.manifest.generated_at)?,
            digest: self.digest(),
        })
    }
    /// sha256 of the manifest bytes.
    pub fn digest(&self) -> String {
        sha256_hex(&self.manifest_bytes)
    }
}

/// Verify a manifest, signature and catalog as one unit.
pub fn verify_parts(
    policy: &Policy,
    manifest_bytes: Vec<u8>,
    signature: Vec<u8>,
    compressed: Vec<u8>,
    source_base: String,
    etag: Option<String>,
) -> Result<VerifiedIndex, IndexError> {
    let manifest = authenticate(&manifest_bytes, &signature, &policy.public_key)?;
    policy.compatible(&manifest)?;
    if !policy.origins.is_empty()
        && !policy.origins.contains(&source_base)
        && !manifest
            .mirrors
            .iter()
            .any(|mirror| mirror.base_url == source_base)
    {
        return Err(IndexError::new(
            "trust",
            "Catalog source is outside the configured origins",
        ));
    }
    if etag.as_ref().is_some_and(|etag| etag.len() > 4096) {
        return Err(IndexError::new(
            "schema",
            "Index ETag exceeds its byte limit",
        ));
    }
    if compressed.len() as u64 != manifest.catalog.bytes {
        return Err(IndexError::new(
            "catalog_size",
            "Catalog compressed size does not match the manifest",
        ));
    }
    if sha256_hex(&compressed) != manifest.catalog.sha256 {
        return Err(IndexError::new(
            "catalog_hash",
            "Plugin catalog failed sha256 verification",
        ));
    }
    let (catalog, document) = decompress_checked(&compressed, manifest.catalog.uncompressed_bytes)?;
    Ok(VerifiedIndex {
        manifest,
        manifest_bytes: manifest_bytes.into(),
        signature: signature.into(),
        compressed: compressed.into(),
        catalog: catalog.into(),
        document: Arc::new(document),
        source_base,
        etag,
    })
}

#[derive(Debug, Clone)]
/// One HTTP request the fetch session wants.
pub struct Request {
    pub url: String,
    pub if_none_match: Option<String>,
    pub max_bytes: usize,
}
#[derive(Debug)]
/// The HTTP response supplied back to a fetch session.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Manifest,
    Signature,
    Catalog,
    Done,
}

fn deduplicate(bases: &mut Vec<String>) {
    let mut seen = HashSet::new();
    bases.retain(|base| seen.insert(base.clone()));
}

/// Bounded multi-step fetch of a signed index.
pub struct FetchSession {
    policy: Policy,
    cache: Option<VerifiedIndex>,
    watermark: Option<Stamp>,
    phase: Phase,
    bases: Vec<String>,
    attempt: usize,
    base: String,
    manifest_bytes: Vec<u8>,
    signature: Vec<u8>,
    manifest: Option<Manifest>,
    etag: Option<String>,
    error: Option<IndexError>,
    result: Option<VerifiedIndex>,
}
impl FetchSession {
    /// Create a session from a trusted policy, optional cache and watermark.
    pub fn new(
        mut policy: Policy,
        cache: Option<VerifiedIndex>,
        watermark: Option<Stamp>,
    ) -> Result<Self, IndexError> {
        if policy.public_key.len() != 32 || policy.origins.is_empty() || policy.origins.len() > 8 {
            return Err(IndexError::new(
                "trust",
                "Configure a signing authority and index origins",
            ));
        }
        for origin in &mut policy.origins {
            if !crate::is_install_url(origin) {
                return Err(IndexError::new(
                    "trust",
                    "Refusing an unsafe plugin-index origin",
                ));
            }
            if !origin.ends_with('/') {
                origin.push('/');
            }
        }
        deduplicate(&mut policy.origins);
        Ok(Self {
            bases: policy.origins.clone(),
            policy,
            cache,
            watermark,
            phase: Phase::Manifest,
            attempt: 0,
            base: String::new(),
            manifest_bytes: Vec::new(),
            signature: Vec::new(),
            manifest: None,
            etag: None,
            error: None,
            result: None,
        })
    }
    /// The next request, or `None` when the fetch is done.
    pub fn request(&self) -> Result<Option<Request>, IndexError> {
        if self.phase == Phase::Done {
            return Ok(None);
        }
        let base = self.bases.get(self.attempt).ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| IndexError::network("No index origin answered"))
        })?;
        let (path, max_bytes, if_none_match) = match self.phase {
            Phase::Manifest => (
                MANIFEST_FILE.into(),
                MAX_MANIFEST_BYTES,
                self.cache
                    .as_ref()
                    .filter(|cache| cache.source_base == *base)
                    .and_then(|cache| cache.etag.clone()),
            ),
            Phase::Signature => (
                format!("{SIGNATURE_FILE}?v={}", sha256_hex(&self.manifest_bytes)),
                MAX_SIGNATURE_BYTES,
                None,
            ),
            Phase::Catalog => (
                self.manifest
                    .as_ref()
                    .ok_or_else(|| IndexError::new("schema", "Missing authenticated manifest"))?
                    .catalog
                    .path
                    .clone(),
                MAX_CATALOG_DOWNLOAD_BYTES,
                None,
            ),
            Phase::Done => unreachable!(),
        };
        Ok(Some(Request {
            url: format!("{base}{path}"),
            max_bytes,
            if_none_match,
        }))
    }
    fn finish(&mut self, index: VerifiedIndex) -> Result<(), IndexError> {
        let index = verify_parts(
            &self.policy,
            index.manifest_bytes.to_vec(),
            index.signature.to_vec(),
            index.compressed.to_vec(),
            index.source_base,
            index.etag,
        )?;
        self.policy.compatible(&index.manifest)?;
        let stamp = Stamp::authenticated(
            &index.manifest_bytes,
            &index.signature,
            &self.policy.public_key,
        )?;
        if let Some(highest) = &self.watermark {
            highest.check(&stamp)?;
        }
        self.result = Some(index);
        self.phase = Phase::Done;
        Ok(())
    }
    /// Feed one response into the session.
    pub fn accept(&mut self, response: Result<Response, String>) -> Result<(), IndexError> {
        let request = self
            .request()?
            .ok_or_else(|| IndexError::new("schema", "Index fetch already finished"))?;
        let response = match response {
            Ok(response) if response.body.len() <= request.max_bytes => response,
            Ok(_) => {
                return Err(IndexError::new(
                    "catalog_size",
                    "Index response exceeds its request limit",
                ));
            }
            Err(error) => {
                if self.error.as_ref().is_none_or(|old| old.stage == "network") {
                    self.error = Some(IndexError::network(error));
                }
                self.attempt += 1;
                return Ok(());
            }
        };
        if self.phase == Phase::Manifest && response.status == 304 {
            let cached = self
                .cache
                .as_ref()
                .filter(|cache| request.url == format!("{}{MANIFEST_FILE}", cache.source_base))
                .cloned();
            if let Some(mut cached) = cached {
                cached.etag = response.etag.or(cached.etag);
                return self.finish(cached);
            }
        }
        if response.status != 200 {
            self.error.get_or_insert_with(|| {
                IndexError::network(format!("{}: HTTP {}", request.url, response.status))
            });
            self.attempt += 1;
            return Ok(());
        }
        match self.phase {
            Phase::Manifest => {
                let preliminary: Manifest =
                    serde_json::from_slice(&response.body).map_err(|error| {
                        IndexError::new("schema", format!("manifest JSON: {error}"))
                    })?;
                if preliminary.signature.is_none() {
                    return Err(IndexError::new(
                        "signature",
                        "manifest is unsigned; refusing to use it",
                    ));
                }
                self.base = self.bases[self.attempt].clone();
                self.etag = response.etag;
                if let Some(mut cached) = self
                    .cache
                    .as_ref()
                    .filter(|cache| cache.manifest_bytes.as_ref() == response.body.as_slice())
                    .cloned()
                {
                    cached.etag = self.etag.clone().or(cached.etag);
                    cached.source_base = self.base.clone();
                    return self.finish(cached);
                }
                self.manifest_bytes = response.body;
                self.phase = Phase::Signature;
                self.bases = vec![self.base.clone()];
                self.bases.extend(self.policy.origins.clone());
                deduplicate(&mut self.bases);
                self.attempt = 0;
                self.error = None;
            }
            Phase::Signature => {
                let manifest = authenticate(
                    &self.manifest_bytes,
                    &response.body,
                    &self.policy.public_key,
                )?;
                self.policy.compatible(&manifest)?;
                let incoming = Stamp::authenticated(
                    &self.manifest_bytes,
                    &response.body,
                    &self.policy.public_key,
                )?;
                if let Some(highest) = &self.watermark {
                    highest.check(&incoming)?;
                }
                self.signature = response.body;
                if let Some(cached) = &self.cache
                    && cached.manifest.catalog.sha256 == manifest.catalog.sha256
                {
                    let index = verify_parts(
                        &self.policy,
                        self.manifest_bytes.clone(),
                        self.signature.clone(),
                        cached.compressed.to_vec(),
                        self.base.clone(),
                        self.etag.clone(),
                    )?;
                    return self.finish(index);
                }
                self.bases = vec![self.base.clone()];
                self.bases.extend(
                    manifest
                        .mirrors
                        .iter()
                        .map(|mirror| mirror.base_url.clone()),
                );
                deduplicate(&mut self.bases);
                self.manifest = Some(manifest);
                self.phase = Phase::Catalog;
                self.attempt = 0;
                self.error = None;
            }
            Phase::Catalog => match verify_parts(
                &self.policy,
                self.manifest_bytes.clone(),
                self.signature.clone(),
                response.body,
                self.base.clone(),
                self.etag.clone(),
            ) {
                Ok(index) => return self.finish(index),
                Err(error) => {
                    self.error = Some(error);
                    self.attempt += 1;
                }
            },
            Phase::Done => unreachable!(),
        }
        Ok(())
    }
    /// The verified index, or an error when the fetch did not finish.
    pub fn into_verified(self) -> Result<VerifiedIndex, IndexError> {
        self.result
            .ok_or_else(|| IndexError::network("Index fetch did not finish"))
    }
}
