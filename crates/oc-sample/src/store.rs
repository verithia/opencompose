//! The local half: download one item into a directory, verify it, and leave a
//! sidecar and a receipt behind.
//!
//! The verification order is fixed and happens before anything is written: the
//! licence rule first, then the declared size, then the declared checksum. A
//! refusal preserves existing media and receipts. A successful pull stages the
//! audio, `<stem>.json` credit sidecar and index together, publishes new files
//! without replacing existing paths, then atomically commits `index.json`.
//! A repeat pull uses a versioned name when either portable path is occupied.
//!
//! The sidecar is the portable half: it sits next to the audio, so copying the
//! project folder copies the licence with it. The index is the local half: it
//! is what a later tool reads instead of walking the folder.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SampleKind};
use crate::provider::SampleFetch;

/// Registry written into `<dir>/index.json`. Unknown versions are refused.
pub const INDEX_SCHEMA: &str = "opencompose.samples.v1";
/// Schema written into every sidecar.
pub const SIDECAR_SCHEMA: &str = "opencompose.sample.v1";
/// Name of the receipt index inside a sample directory.
pub const INDEX_FILE: &str = "index.json";
/// Lock file beside the index; hidden, so it never reads as a sample.
const INDEX_LOCK_FILE: &str = ".index.lock";
/// Serialises index updates inside this process. The file lock covers the
/// other processes, this one covers the threads that would otherwise all hold
/// the same file lock through different descriptors.
static INDEX_LOCK: Mutex<()> = Mutex::new(());
/// One sample may not be larger than this. A DAW pulls samples, not sample
/// libraries; the bound turns a wrong URL into a refusal instead of a full disk.
pub const MAX_SAMPLE_BYTES: usize = 96 * 1024 * 1024;
/// Longest slug taken from a sample name.
const MAX_SLUG: usize = 48;
/// Audio suffixes occupy a namespace distinct from JSON sidecars and the index.
const AUDIO_EXTENSIONS: [&str; 9] = [
    "wav", "wave", "flac", "aiff", "aif", "mp3", "ogg", "oga", "m4a",
];

/// What one pull wrote, and everything a project has to carry with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleReceipt {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub kind: SampleKind,
    pub duration_seconds: Option<f64>,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
    pub license: License,
    /// The one-line credit a project repeats wherever the sample is used.
    pub attribution: String,
    pub source_url: String,
    pub quality: Quality,
    /// File name inside the store directory.
    pub file: String,
    /// Absolute path of the audio file.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub fetched_at: u64,
    /// True when the user accepted a licence that is not free-to-reuse
    /// (non-commercial, no-derivatives, GFDL or unknown).
    #[serde(default)]
    pub licence_accepted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Index {
    schema: String,
    #[serde(default)]
    samples: Vec<SampleReceipt>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    schema: String,
    #[serde(flatten)]
    sample: SampleReceipt,
}

/// One sample folder: the project's `media/samples`, or a directory the CLI is
/// pointed at.
pub struct SampleStore {
    dir: PathBuf,
}

impl SampleStore {
    /// Open the store rooted at `dir` without creating it yet.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory this store owns.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the receipt index in this directory.
    pub fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    /// Receipts already recorded. A missing index is an empty store; an index
    /// that does not parse is an error, never silently replaced. Atomic index
    /// replacement exposes either complete generation; both retain their files.
    pub fn receipts(&self) -> Result<Vec<SampleReceipt>, SampleError> {
        let path = self.index_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(SampleError::Failed(format!(
                    "failed to read '{}': {error}",
                    path.display()
                )));
            }
        };
        let index: Index = serde_json::from_slice(&bytes).map_err(|error| {
            SampleError::Failed(format!(
                "invalid sample index '{}': {error}",
                path.display()
            ))
        })?;
        if index.schema != INDEX_SCHEMA {
            return Err(SampleError::Failed(format!(
                "unsupported sample index schema '{}' in '{}'",
                index.schema,
                path.display()
            )));
        }
        Ok(index.samples)
    }

    /// Download, verify, rename, record. The URL always comes from the item the
    /// provider resolved; a caller never supplies one.
    pub fn pull(
        &self,
        item: &SampleItem,
        fetch: &dyn SampleFetch,
        accept_licence: bool,
    ) -> Result<SampleReceipt, SampleError> {
        self.pull_with_headers(item, fetch, accept_licence, &[])
    }

    /// The same pull with extra request headers, for a source that serves its
    /// original file only to an authenticated client.
    pub fn pull_with_headers(
        &self,
        item: &SampleItem,
        fetch: &dyn SampleFetch,
        accept_licence: bool,
        extra: &[(&str, &str)],
    ) -> Result<SampleReceipt, SampleError> {
        if item.license.needs_notice() && !accept_licence {
            return Err(SampleError::refused(
                "licence_notice",
                item.license.notice(),
            ));
        }
        if item.download_url.trim().is_empty() {
            return Err(SampleError::refused(
                "no_download",
                format!(
                    "'{}' has no downloadable file at the source; pick another result.",
                    item.name
                ),
            ));
        }
        // Some hosts check where the file is requested from; the provider
        // spec names the referer rather than the store guessing one.
        let referer = crate::registry::spec(&item.provider).and_then(|spec| spec.download_referer);
        let mut headers: Vec<(&str, &str)> = referer
            .map(|referer| vec![("Referer", referer)])
            .unwrap_or_default();
        headers.extend_from_slice(extra);
        let response = fetch
            .get_with(&item.download_url, MAX_SAMPLE_BYTES, &headers)
            .map_err(|error| SampleError::Failed(format!("{}: {error}", item.provider)))?;
        if !(200..300).contains(&response.status) || response.body.len() > MAX_SAMPLE_BYTES {
            return Err(SampleError::refused(
                "invalid_download",
                "The sample response failed its status or size limit.",
            ));
        }
        let bytes = response.body;
        let sha256 = hex(&Sha256::digest(&bytes));
        verify_declared(item, bytes.len() as u64, &sha256)?;
        let detected = crate::store_format::audio_extension(&bytes).ok_or_else(|| {
            SampleError::refused(
                "invalid_audio",
                "The download is not a supported audio container or its header is incomplete.",
            )
        })?;

        fs::create_dir_all(&self.dir).map_err(|error| {
            SampleError::Failed(format!(
                "failed to create '{}': {error}",
                self.dir.display()
            ))
        })?;
        // Keep naming, publication and the receipt update in one critical section.
        let _guard = INDEX_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _lock = self.lock_index()?;
        let mut samples = self.receipts()?;
        let dir = fs::canonicalize(&self.dir).map_err(|error| {
            SampleError::Failed(format!(
                "failed to resolve '{}': {error}",
                self.dir.display()
            ))
        })?;
        let suggested = self.file_name(item);
        let suggested_extension = extension(&suggested).unwrap();
        let compatible = match detected {
            "wav" => matches!(suggested_extension.as_str(), "wav" | "wave"),
            "aiff" => matches!(suggested_extension.as_str(), "aiff" | "aif"),
            "ogg" => matches!(suggested_extension.as_str(), "ogg" | "oga"),
            _ => suggested_extension == detected,
        };
        let base = stem(&suggested);
        let suffix = if compatible {
            &suggested_extension
        } else {
            detected
        };
        let mut file = format!("{base}.{suffix}");
        let mut version = 0u64;
        while dir.join(&file).symlink_metadata().is_ok()
            || sidecar_path(&dir, &file).symlink_metadata().is_ok()
        {
            version += 1;
            file = format!("{base}-{}-{version}.{suffix}", &sha256[..12]);
        }
        let target = dir.join(&file);
        let receipt = SampleReceipt {
            id: item.id.clone(),
            provider: item.provider.clone(),
            name: item.name.clone(),
            kind: item.kind,
            duration_seconds: item.duration_seconds,
            channels: item.channels,
            sample_rate: item.sample_rate,
            license: item.license.clone(),
            attribution: item.attribution.clone(),
            source_url: item.source_url.clone(),
            quality: item.quality,
            file,
            path: target.display().to_string(),
            bytes: bytes.len() as u64,
            sha256,
            fetched_at: now_seconds(),
            licence_accepted: item.license.needs_notice() && accept_licence,
        };
        samples.retain(|row| row.id != receipt.id);
        samples.push(receipt.clone());
        samples.sort_by(|left, right| left.id.cmp(&right.id));
        let index = Index {
            schema: INDEX_SCHEMA.to_string(),
            samples,
        };
        let sidecar = Sidecar {
            schema: SIDECAR_SCHEMA.to_string(),
            sample: receipt.clone(),
        };
        let stage = StagedPull::new(&dir).map_err(store_io)?;
        stage.write("audio", &bytes).map_err(store_io)?;
        stage
            .write("sidecar", &json_document(&sidecar)?)
            .map_err(store_io)?;
        stage
            .write("index", &json_document(&index)?)
            .map_err(store_io)?;
        stage.publish(
            &target,
            &sidecar_path(&dir, &receipt.file),
            &self.index_path(),
        )?;
        Ok(receipt)
    }

    /// The preferred `<provider>-<slug>-<id hash>.<ext>` path for a new item.
    /// Only audio suffixes from a URL or name are used. Pull verifies the
    /// container and may correct the suffix or choose a versioned path to
    /// preserve an existing file. Callers must use the returned receipt path.
    pub fn file_name(&self, item: &SampleItem) -> String {
        let provider = slug(&item.provider);
        let name = slug(&item.name);
        let name = if name.is_empty() {
            "sample".to_string()
        } else {
            name
        };
        let mut hash = hex(&Sha256::digest(item.id.as_bytes()));
        hash.truncate(8);
        let audio_suffix = |value: &str| {
            extension(value).filter(|suffix| AUDIO_EXTENSIONS.contains(&suffix.as_str()))
        };
        let extension = audio_suffix(&item.download_url)
            .or_else(|| audio_suffix(&item.name))
            .unwrap_or_else(|| "bin".to_string());
        format!("{provider}-{name}-{hash}.{extension}")
    }

    /// Take the advisory lock on the store's lock file. The returned handle
    /// holds it; dropping the handle releases it.
    fn lock_index(&self) -> Result<fs::File, SampleError> {
        let path = self.dir.join(INDEX_LOCK_FILE);
        fs::create_dir_all(&self.dir).map_err(|error| {
            SampleError::Failed(format!(
                "failed to create '{}': {error}",
                self.dir.display()
            ))
        })?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                SampleError::Failed(format!("failed to open '{}': {error}", path.display()))
            })?;
        file.lock().map_err(|error| {
            SampleError::Failed(format!("failed to lock '{}': {error}", path.display()))
        })?;
        Ok(file)
    }
}

/// The declared size and checksum, checked in that order. Both are optional;
/// a source that offers neither still gets its sha256 recorded in the receipt.
fn verify_declared(
    item: &SampleItem,
    actual_bytes: u64,
    actual_sha256: &str,
) -> Result<(), SampleError> {
    if let Some(expected) = item.bytes
        && actual_bytes != expected
    {
        return Err(SampleError::refused(
            "size_mismatch",
            format!(
                "'{}' downloaded {actual_bytes} bytes but the source declares {expected}; nothing was written.",
                item.name
            ),
        ));
    }
    if let Some(expected) = &item.sha256
        && !expected.eq_ignore_ascii_case(actual_sha256)
    {
        return Err(SampleError::refused(
            "checksum_mismatch",
            format!(
                "'{}' failed sha256 verification (expected {expected}, got {actual_sha256}); nothing was written.",
                item.name
            ),
        ));
    }
    Ok(())
}

fn write_part(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn store_io(error: io::Error) -> SampleError {
    SampleError::Failed(format!("sample publication failed: {error}"))
}

fn json_document(value: &impl Serialize) -> Result<Vec<u8>, SampleError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        SampleError::Failed(format!("failed to serialize sample metadata: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sidecar_path(dir: &Path, file: &str) -> PathBuf {
    dir.join(format!("{}.json", stem(file)))
}

/// Staging is private to this pull. Hard links publish complete files while
/// refusing an occupied destination on every platform. The index rename is
/// the commit point; rollback removes only the two paths this pull created.
struct StagedPull {
    dir: PathBuf,
}

impl StagedPull {
    fn new(parent: &Path) -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = parent.join(format!(
                ".sample-{}-{nanos}-{sequence}.part",
                std::process::id()
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Self { dir }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        write_part(&self.dir.join(name), bytes)
    }

    fn publish(&self, audio: &Path, sidecar: &Path, index: &Path) -> Result<(), SampleError> {
        fs::hard_link(self.dir.join("audio"), audio).map_err(|error| {
            SampleError::Failed(format!(
                "failed to publish sample audio without replacing '{}': {error}",
                audio.display()
            ))
        })?;
        if let Err(error) = fs::hard_link(self.dir.join("sidecar"), sidecar) {
            let _ = fs::remove_file(audio);
            return Err(SampleError::Failed(format!(
                "failed to publish sample credit without replacing '{}': {error}",
                sidecar.display()
            )));
        }
        if let Err(error) = fs::rename(self.dir.join("index"), index) {
            let _ = fs::remove_file(sidecar);
            let _ = fs::remove_file(audio);
            return Err(store_io(error));
        }
        Ok(())
    }
}

impl Drop for StagedPull {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// File stem without the audio extension, so the sidecar sorts beside the file.
fn stem(file: &str) -> &str {
    match file.rfind('.') {
        Some(dot) if dot > 0 => &file[..dot],
        _ => file,
    }
}

fn extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next()?;
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if extension.is_empty()
        || extension.len() > 5
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(extension)
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = true;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(MAX_SLUG);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::FetchResponse;
    use std::sync::Mutex;

    struct FixtureFetch {
        body: Vec<u8>,
        urls: Mutex<Vec<String>>,
    }

    impl SampleFetch for FixtureFetch {
        fn get(&self, url: &str, _max_bytes: usize) -> Result<FetchResponse, String> {
            self.urls.lock().unwrap().push(url.to_string());
            Ok(FetchResponse {
                status: 200,
                body: self.body.clone(),
            })
        }
    }

    fn fetch(body: &[u8]) -> FixtureFetch {
        FixtureFetch {
            body: body.to_vec(),
            urls: Mutex::new(Vec::new()),
        }
    }

    /// Records the request headers, so a pull can be shown to carry the
    /// caller's `Authorization` header.
    struct HeaderFetch {
        body: Vec<u8>,
        headers: Mutex<Vec<(String, String)>>,
    }

    impl SampleFetch for HeaderFetch {
        fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, String> {
            self.get_with(url, max_bytes, &[])
        }

        fn get_with(
            &self,
            _url: &str,
            _max_bytes: usize,
            headers: &[(&str, &str)],
        ) -> Result<FetchResponse, String> {
            *self.headers.lock().unwrap() = headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            Ok(FetchResponse {
                status: 200,
                body: self.body.clone(),
            })
        }
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn path(&self) -> &Path {
            &self.path
        }

        fn new(label: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("oc-sample-{label}-{}-{unique}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn item(license: License) -> SampleItem {
        SampleItem {
            id: "commons:File:Snare drum unmuffled.ogg".into(),
            provider: "wikimedia-commons".into(),
            name: "Snare drum unmuffled".into(),
            kind: SampleKind::DrumOneShot,
            duration_seconds: Some(4.82),
            channels: Some(2),
            sample_rate: Some(44_100),
            license,
            attribution: "Fixture User — CC BY-SA 3.0 — https://example.org/file".into(),
            source_url: "https://commons.wikimedia.org/wiki/File:Snare_drum_unmuffled.ogg".into(),
            preview_url: None,
            download_url: "https://upload.wikimedia.org/wikipedia/commons/b/b2/Snare.ogg".into(),
            sha256: None,
            bytes: None,
            quality: Quality::Original,
            tags: vec!["Snare drums".into()],
        }
    }

    fn wave(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let length = (samples.len() * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + length).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44_100u32.to_le_bytes());
        bytes.extend_from_slice(&88_200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&length.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn a_pull_writes_the_audio_the_sidecar_and_the_receipt() {
        let temp = TempDir::new("pull");
        let store = SampleStore::new(temp.path().join("media/samples"));
        let bytes = wave(&[0, 1000, -1000]);
        let fetch = fetch(&bytes);
        let license = License::public_domain();
        let receipt = store.pull(&item(license), &fetch, false).unwrap();

        assert_eq!(receipt.bytes, bytes.len() as u64);
        assert_eq!(receipt.sha256, hex(&Sha256::digest(&bytes)));
        assert_eq!(
            receipt.file,
            "wikimedia-commons-snare-drum-unmuffled-72a91f6b.wav"
        );
        assert_eq!(receipt.kind, SampleKind::DrumOneShot);
        assert_eq!(receipt.license.id, "public-domain");
        assert_eq!(fs::read(&receipt.path).unwrap(), bytes);
        assert!(Path::new(&receipt.path).is_absolute(), "{}", receipt.path);

        let sidecar_path = store.dir().join(format!("{}.json", stem(&receipt.file)));
        let sidecar: serde_json::Value =
            serde_json::from_slice(&fs::read(&sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar["schema"], SIDECAR_SCHEMA);
        assert_eq!(sidecar["id"], "commons:File:Snare drum unmuffled.ogg");
        assert_eq!(sidecar["license"]["id"], "public-domain");
        assert!(
            sidecar["attribution"]
                .as_str()
                .unwrap()
                .contains("CC BY-SA")
        );

        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(store.index_path()).unwrap()).unwrap();
        assert_eq!(index["schema"], INDEX_SCHEMA);
        assert_eq!(index["samples"][0]["sha256"], receipt.sha256);
        assert_eq!(store.receipts().unwrap().len(), 1);
        // The part file is gone; only the audio and the two documents remain.
        let names: Vec<String> = fs::read_dir(store.dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".part")),
            "{names:?}"
        );
        assert!(names.contains(&format!("{}.json", stem(&receipt.file))));
    }

    #[test]
    fn a_pull_forwards_the_authorization_header() {
        let temp = TempDir::new("auth-header");
        let store = SampleStore::new(temp.path().join("media/samples"));
        let bytes = wave(&[0, 1000, -1000]);
        let fetch = HeaderFetch {
            body: bytes.to_vec(),
            headers: Mutex::new(Vec::new()),
        };
        let receipt = store
            .pull_with_headers(
                &item(License::cc0()),
                &fetch,
                false,
                &[("Authorization", "Bearer fixture-token")],
            )
            .unwrap();

        assert_eq!(receipt.quality, Quality::Original);
        assert_eq!(receipt.bytes, bytes.len() as u64);
        assert_eq!(
            fetch.headers.lock().unwrap().as_slice(),
            &[(
                "Authorization".to_string(),
                "Bearer fixture-token".to_string()
            )]
        );
    }

    #[test]
    fn a_size_mismatch_is_refused_before_anything_is_written() {
        let temp = TempDir::new("size");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.bytes = Some(999_999);
        let error = store.pull(&item, &fetch(b"short body"), false).unwrap_err();
        assert_eq!(error.code(), Some("size_mismatch"));
        assert!(error.to_string().contains("declares 999999"), "{error}");
        assert!(
            fs::read_dir(store.dir()).unwrap().next().is_none(),
            "the directory stays empty"
        );
        assert!(!store.index_path().exists());
    }

    #[test]
    fn a_checksum_mismatch_is_refused_before_anything_is_written() {
        let temp = TempDir::new("checksum");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.bytes = Some(4);
        item.sha256 = Some("00".repeat(32));
        let error = store.pull(&item, &fetch(b"body"), false).unwrap_err();
        assert_eq!(error.code(), Some("checksum_mismatch"));
        assert!(error.to_string().contains("failed sha256"), "{error}");
        assert!(fs::read_dir(store.dir()).unwrap().next().is_none());
    }

    #[test]
    fn a_declared_checksum_that_matches_is_accepted() {
        let temp = TempDir::new("checksum-ok");
        let store = SampleStore::new(temp.path());
        let body = wave(&[0, 1000]);
        let mut item = item(License::cc0());
        item.bytes = Some(body.len() as u64);
        item.sha256 = Some(hex(&Sha256::digest(&body)));
        assert!(store.pull(&item, &fetch(&body), false).is_ok());
    }

    #[test]
    fn a_non_free_licence_asks_for_acceptance_and_writes_nothing() {
        let temp = TempDir::new("licence");
        let store = SampleStore::new(temp.path());
        let license = License::from_freesound("Attribution NonCommercial");
        let error = store
            .pull(&item(license), &fetch(b"body"), false)
            .unwrap_err();
        assert_eq!(error.code(), Some("licence_notice"));
        assert!(error.to_string().contains("non-commercial"), "{error}");
        assert!(
            error.to_string().contains("accept_licence: true"),
            "{error}"
        );
        assert!(
            fs::read_dir(temp.path()).unwrap().next().is_none(),
            "not even the store directory is created"
        );
    }

    #[test]
    fn accepting_the_notice_downloads_the_file_and_records_the_acceptance() {
        let temp = TempDir::new("licence-accepted");
        let store = SampleStore::new(temp.path());
        let license = License::from_freesound("Attribution NonCommercial");
        let receipt = store
            .pull(&item(license), &fetch(&wave(&[0, 1000])), true)
            .unwrap();
        assert!(receipt.licence_accepted);
        assert!(receipt.license.non_commercial);
        assert!(std::path::Path::new(&receipt.path).is_file());
        assert_eq!(receipt.license.name, "Attribution NonCommercial");
    }

    #[test]
    fn an_item_without_a_download_url_is_refused() {
        let temp = TempDir::new("no-url");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.download_url = String::new();
        let error = store.pull(&item, &fetch(b"body"), false).unwrap_err();
        assert_eq!(error.code(), Some("no_download"));
    }

    #[test]
    fn a_second_pull_of_the_same_id_replaces_its_receipt() {
        let temp = TempDir::new("again");
        let store = SampleStore::new(temp.path());
        let item = item(License::cc0());
        let first = store.pull(&item, &fetch(&wave(&[0, 1])), false).unwrap();
        let original = fs::read(&first.path).unwrap();
        let second_body = wave(&[0, 2, 3]);
        store.pull(&item, &fetch(&second_body), false).unwrap();
        let receipts = store.receipts().unwrap();
        assert_eq!(receipts.len(), 1, "one id has one receipt");
        assert_eq!(receipts[0].bytes, second_body.len() as u64);
        assert_eq!(fs::read(&receipts[0].path).unwrap(), second_body);
        assert_eq!(fs::read(first.path).unwrap(), original);
    }

    #[test]
    fn concurrent_pulls_keep_every_receipt_and_audio_file() {
        const THREADS: usize = 8;
        let temp = TempDir::new("concurrent");
        let store = SampleStore::new(temp.path());
        let barrier = std::sync::Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for index in 0..THREADS {
                let barrier = &barrier;
                let store = &store;
                scope.spawn(move || {
                    let mut item = item(License::cc0());
                    item.id = format!("fixture:{index}");
                    barrier.wait();
                    store
                        .pull(&item, &fetch(&wave(&[index as i16, 1])), false)
                        .unwrap();
                });
            }
        });
        let receipts = store.receipts().unwrap();
        assert_eq!(receipts.len(), THREADS);
        for receipt in receipts {
            assert_eq!(fs::read(&receipt.path).unwrap().len() as u64, receipt.bytes);
            assert!(sidecar_path(store.dir(), &receipt.file).is_file());
        }
    }

    #[test]
    fn a_missing_index_is_an_empty_store() {
        let temp = TempDir::new("empty");
        let store = SampleStore::new(temp.path());
        assert!(store.receipts().unwrap().is_empty());
    }

    #[test]
    fn a_corrupt_index_is_reported_not_replaced() {
        let temp = TempDir::new("corrupt");
        let store = SampleStore::new(temp.path());
        fs::write(store.index_path(), b"{ not json").unwrap();
        let error = store
            .pull(&item(License::cc0()), &fetch(&wave(&[0, 1])), false)
            .unwrap_err();
        assert!(
            error.to_string().contains("invalid sample index"),
            "{error}"
        );
        assert_eq!(fs::read(store.index_path()).unwrap(), b"{ not json");
    }

    #[test]
    fn content_selects_an_audio_suffix_for_json_query_and_extensionless_urls() {
        for (index, url) in [
            "https://example.org/download.json",
            "https://example.org/download.JSON",
            "https://example.org/download.json?format=.mp3#file.wav",
            "https://example.org/download",
            "https://example.org/download.WAV?token=fixture",
            "https://example.org/download.wav",
        ]
        .iter()
        .enumerate()
        {
            let temp = TempDir::new(&format!("format-{index}"));
            let store = SampleStore::new(temp.path());
            let mut item = item(License::cc0());
            item.download_url = (*url).into();
            let body = wave(&[0, 2, -2]);
            let receipt = store.pull(&item, &fetch(&body), false).unwrap();
            assert!(receipt.file.ends_with(".wav"), "{}", receipt.file);
            assert_eq!(fs::read(&receipt.path).unwrap(), body);
            let sidecar = sidecar_path(store.dir(), &receipt.file);
            assert_ne!(Path::new(&receipt.path), sidecar);
            let saved: Sidecar = serde_json::from_slice(&fs::read(sidecar).unwrap()).unwrap();
            assert_eq!(saved.sample, receipt);
            assert_eq!(store.receipts().unwrap(), vec![receipt]);
        }
    }

    #[test]
    fn audio_suffix_aliases_keep_the_documented_pair_paths() {
        let temp = TempDir::new("wave-alias");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.name = "voice.wave".into();
        item.download_url = "https://example.org/metadata.JSON?download=true".into();
        let expected = store.file_name(&item);
        assert!(expected.ends_with(".wave"));
        let receipt = store.pull(&item, &fetch(&wave(&[0, 1])), false).unwrap();
        assert_eq!(receipt.file, expected);
        assert!(sidecar_path(store.dir(), &receipt.file).is_file());
    }

    #[test]
    fn recognized_compressed_containers_keep_audio_and_credit_separate() {
        // These container headers exercise identification, not codec decoding.
        let mut ogg = vec![0u8; 27];
        ogg[..4].copy_from_slice(b"OggS");
        ogg[26] = 1;
        ogg.push(19);
        ogg.extend_from_slice(b"OpusHead\x01\x01\0\0\x80\xbb\0\0\0\0\0");
        let mut flac = b"fLaC\x80\0\0\x22".to_vec();
        flac.extend_from_slice(&[0; 34]);
        flac.extend_from_slice(&[0xff, 0xf8, 0]);
        let mp3 = [0xff, 0xfb, 0x90, 0, 0, 0].to_vec();
        let m4a = b"\0\0\0\x10ftypM4A \0\0\0\0".to_vec();
        for (suffix, bytes) in [("ogg", ogg), ("flac", flac), ("mp3", mp3), ("m4a", m4a)] {
            let temp = TempDir::new(suffix);
            let store = SampleStore::new(temp.path());
            let mut item = item(License::cc0());
            item.download_url = format!("https://example.org/sample.{suffix}");
            let receipt = store.pull(&item, &fetch(&bytes), false).unwrap();
            assert!(receipt.file.ends_with(&format!(".{suffix}")));
            assert_eq!(fs::read(&receipt.path).unwrap(), bytes);
            assert!(sidecar_path(store.dir(), &receipt.file).is_file());
        }
    }

    #[test]
    fn malformed_media_and_error_pages_leave_an_existing_store_untouched() {
        let temp = TempDir::new("bad-media");
        let store = SampleStore::new(temp.path());
        let original = store
            .pull(&item(License::cc0()), &fetch(&wave(&[0, 1])), false)
            .unwrap();
        let before: Vec<_> = fs::read_dir(store.dir())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (path.clone(), fs::read(path).unwrap())
            })
            .collect();
        let mut truncated = wave(&[0, 1]);
        truncated.pop();
        let mut item = item(License::cc0());
        item.download_url = "https://example.org/download.json".into();
        for bytes in [
            b"".as_slice(),
            b"{\"error\":\"access denied\"}",
            b"<html>not an audio file</html>",
            b"RIFF original audio body",
            b"OggS fake audio body",
            b"ID3",
            truncated.as_slice(),
        ] {
            let error = store.pull(&item, &fetch(bytes), false).unwrap_err();
            assert_eq!(error.code(), Some("invalid_audio"), "{error}");
            for (path, bytes) in &before {
                assert_eq!(&fs::read(path).unwrap(), bytes);
            }
            assert_eq!(fs::read_dir(store.dir()).unwrap().count(), before.len());
        }
        assert_eq!(store.receipts().unwrap(), vec![original]);
    }

    #[test]
    fn preexisting_media_sidecar_and_partial_paths_are_preserved() {
        let temp = TempDir::new("occupied");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.download_url = "https://example.org/audio.wav".into();
        let file = store.file_name(&item);
        let audio = store.dir().join(&file);
        let sidecar = sidecar_path(store.dir(), &file);
        let stale_part = store.dir().join(format!(".{file}.part"));
        fs::write(&audio, b"existing project media").unwrap();
        fs::write(&sidecar, b"existing project credit").unwrap();
        fs::write(&stale_part, b"another pull owns this").unwrap();
        let body = wave(&[0, 3]);
        let receipt = store.pull(&item, &fetch(&body), false).unwrap();
        assert_ne!(receipt.file, file);
        assert_eq!(fs::read(&audio).unwrap(), b"existing project media");
        assert_eq!(fs::read(&sidecar).unwrap(), b"existing project credit");
        assert_eq!(fs::read(&stale_part).unwrap(), b"another pull owns this");
        assert_eq!(fs::read(&receipt.path).unwrap(), body);
        assert!(sidecar_path(store.dir(), &receipt.file).is_file());
    }

    #[test]
    fn an_occupied_sidecar_alone_chooses_a_new_pair() {
        let temp = TempDir::new("occupied-sidecar");
        let store = SampleStore::new(temp.path());
        let mut item = item(License::cc0());
        item.download_url = "https://example.org/audio.wav".into();
        let file = store.file_name(&item);
        let sidecar = sidecar_path(store.dir(), &file);
        fs::create_dir(&sidecar).unwrap();
        let receipt = store.pull(&item, &fetch(&wave(&[0, 1])), false).unwrap();
        assert_ne!(receipt.file, file);
        assert!(sidecar.is_dir());
        assert!(!store.dir().join(file).exists());
    }

    #[test]
    fn a_failed_repull_keeps_the_previous_download_and_receipt_sidecar() {
        let temp = TempDir::new("failed-repull");
        let store = SampleStore::new(temp.path());
        let item = item(License::cc0());
        let first = store.pull(&item, &fetch(&wave(&[0, 1])), false).unwrap();
        let audio = fs::read(&first.path).unwrap();
        let sidecar_path = sidecar_path(store.dir(), &first.file);
        let sidecar = fs::read(&sidecar_path).unwrap();
        fs::write(store.index_path(), b"corrupt index").unwrap();
        let error = store
            .pull(&item, &fetch(&wave(&[0, 9])), false)
            .unwrap_err();
        assert!(error.to_string().contains("invalid sample index"));
        assert_eq!(fs::read(&first.path).unwrap(), audio);
        assert_eq!(fs::read(sidecar_path).unwrap(), sidecar);
        assert_eq!(fs::read(store.index_path()).unwrap(), b"corrupt index");
    }

    #[test]
    fn staged_publication_cleans_new_files_when_index_commit_fails() {
        let temp = TempDir::new("publish-failure");
        let index = temp.path().join("index.json");
        fs::create_dir(&index).unwrap();
        let audio = temp.path().join("new.wav");
        let sidecar = temp.path().join("new.json");
        let existing = temp.path().join("existing.wav");
        fs::write(&existing, b"previous audio").unwrap();
        {
            let stage = StagedPull::new(temp.path()).unwrap();
            stage.write("audio", &wave(&[0, 1])).unwrap();
            stage.write("sidecar", b"{}").unwrap();
            stage.write("index", b"{}").unwrap();
            assert!(stage.publish(&audio, &sidecar, &index).is_err());
        }
        assert!(!audio.exists());
        assert!(!sidecar.exists());
        assert!(index.is_dir());
        assert_eq!(fs::read(existing).unwrap(), b"previous audio");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn staged_publication_does_not_replace_a_racing_sidecar() {
        let temp = TempDir::new("publish-collision");
        let audio = temp.path().join("new.wav");
        let sidecar = temp.path().join("new.json");
        let index = temp.path().join("index.json");
        fs::write(&sidecar, b"prior credit").unwrap();
        fs::write(&index, b"prior index").unwrap();
        {
            let stage = StagedPull::new(temp.path()).unwrap();
            stage.write("audio", &wave(&[0, 1])).unwrap();
            stage.write("sidecar", b"{}").unwrap();
            stage.write("index", b"{}").unwrap();
            assert!(stage.publish(&audio, &sidecar, &index).is_err());
        }
        assert!(!audio.exists());
        assert_eq!(fs::read(&sidecar).unwrap(), b"prior credit");
        assert_eq!(fs::read(&index).unwrap(), b"prior index");
    }

    #[test]
    fn a_file_name_is_stable_readable_and_bounded() {
        let store = SampleStore::new("unused");
        let mut item = item(License::cc0());
        assert_eq!(
            store.file_name(&item),
            "wikimedia-commons-snare-drum-unmuffled-72a91f6b.ogg"
        );
        // A second call cannot produce a different name for the same id.
        assert_eq!(store.file_name(&item), store.file_name(&item));

        item.name = "Snare / Drum: 01 !!".into();
        item.id = "commons:other".into();
        let name = store.file_name(&item);
        assert!(
            name.starts_with("wikimedia-commons-snare-drum-01-"),
            "{name}"
        );
        assert!(!name.contains('/') && !name.contains(':'), "{name}");

        item.name = "x".repeat(200);
        assert!(store.file_name(&item).len() <= 100);

        item.download_url = "https://example.org/file".into();
        assert!(store.file_name(&item).ends_with(".bin"));
    }

    #[test]
    fn an_extension_comes_from_the_url_path_only() {
        assert_eq!(extension("https://a/b/c.OGG?x=1").as_deref(), Some("ogg"));
        assert_eq!(extension("https://a/b/c").as_deref(), None);
        assert_eq!(extension("https://a/b/c.toolongext").as_deref(), None);
    }

    #[test]
    fn a_name_supplies_the_extension_when_the_url_has_none() {
        let store = SampleStore::new("unused");
        let mut item = item(License::cc0());
        item.id = "freesound:14854".into();
        item.provider = "freesound".into();
        item.name = "snare_01.wav".into();
        item.download_url = "https://freesound.org/apiv2/sounds/14854/download/".into();
        let name = store.file_name(&item);
        assert!(name.ends_with(".wav"), "{name}");

        // A name that is not an audio file keeps the neutral fallback.
        item.name = "take.01".into();
        assert!(store.file_name(&item).ends_with(".bin"));
    }
}
