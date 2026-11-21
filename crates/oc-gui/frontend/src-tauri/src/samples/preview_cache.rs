//! Downloaded preview files, kept for the session.
//!
//! Auditioning a result and then adding it fetch the same file, and clicking
//! between results replays files already heard. The cache keeps each fetched
//! body on disk, keyed by provider, sample id and URL, so a repeated preview
//! skips the resolve and the download, and an add whose resolved URL matches
//! reads the bytes back instead of fetching them again. The store still
//! verifies the declared size and checksum of whatever bytes it receives.
//!
//! The directory belongs to the running app: it is emptied when the cache
//! opens, and bounded by entry count and total bytes with least-recently-used
//! eviction. Every file is written to a temporary name in the same directory
//! and renamed into place, so a reader never sees a partial file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oc_sample::model::SampleItem;
use oc_sample::provider::{FetchError, FetchResponse, SampleFetch};
use sha2::{Digest, Sha256};

/// Most files the cache keeps.
pub(crate) const PREVIEW_CACHE_ENTRIES: usize = 32;
/// Most bytes the cache keeps across all files.
pub(crate) const PREVIEW_CACHE_BYTES: u64 = 256 * 1024 * 1024;
/// Directory under the app cache directory.
pub(crate) const PREVIEW_CACHE_DIR: &str = "sample-previews";

/// The bounds one cache enforces.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CacheLimits {
    pub(crate) entries: usize,
    pub(crate) bytes: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            entries: PREVIEW_CACHE_ENTRIES,
            bytes: PREVIEW_CACHE_BYTES,
        }
    }
}

struct Entry {
    provider: String,
    id: String,
    url: String,
    path: PathBuf,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    total: u64,
    clock: u64,
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn remove(&mut self, index: usize) {
        let entry = self.entries.swap_remove(index);
        self.total = self.total.saturating_sub(entry.bytes);
        let _ = std::fs::remove_file(&entry.path);
    }
}

/// A session cache of preview and download bodies.
pub(crate) struct PreviewCache {
    dir: PathBuf,
    limits: CacheLimits,
    state: Mutex<State>,
}

impl PreviewCache {
    /// Open an empty cache in `dir`, removing what an earlier session left.
    pub(crate) fn open(dir: impl Into<PathBuf>, limits: CacheLimits) -> std::io::Result<Self> {
        let dir = dir.into();
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            limits,
            state: Mutex::new(State::default()),
        })
    }

    /// The most recently used file cached for this sample, whatever URL it
    /// came from. A preview plays any audio of the sample, so it needs no
    /// fresh resolve to pick one.
    pub(crate) fn file_for(&self, provider: &str, id: &str) -> Option<PathBuf> {
        let mut state = self.state.lock().ok()?;
        let tick = state.tick();
        let entry = state
            .entries
            .iter_mut()
            .filter(|entry| entry.provider == provider && entry.id == id)
            .max_by_key(|entry| entry.used)?;
        entry.used = tick;
        Some(entry.path.clone())
    }

    /// The cached body of exactly this URL, when it fits `max_bytes`.
    pub(crate) fn bytes_for(
        &self,
        provider: &str,
        id: &str,
        url: &str,
        max_bytes: usize,
    ) -> Option<Vec<u8>> {
        let mut state = self.state.lock().ok()?;
        let tick = state.tick();
        let index = state
            .entries
            .iter()
            .position(|entry| entry.provider == provider && entry.id == id && entry.url == url)?;
        if state.entries[index].bytes > max_bytes as u64 {
            return None;
        }
        match std::fs::read(&state.entries[index].path) {
            Ok(body) if body.len() as u64 == state.entries[index].bytes => {
                state.entries[index].used = tick;
                Some(body)
            }
            // A file that vanished or changed size is not a cache hit.
            _ => {
                state.remove(index);
                None
            }
        }
    }

    /// Keep `body` as the file behind `url`. Returns its path, or `None` when
    /// the body alone exceeds the byte bound or the write failed; a cache
    /// that cannot store a file only costs a later download.
    pub(crate) fn store(
        &self,
        provider: &str,
        id: &str,
        url: &str,
        extension: &str,
        body: &[u8],
    ) -> Option<PathBuf> {
        let size = body.len() as u64;
        if size > self.limits.bytes || self.limits.entries == 0 {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        if let Some(index) = state
            .entries
            .iter()
            .position(|entry| entry.provider == provider && entry.id == id && entry.url == url)
        {
            state.remove(index);
        }
        while !state.entries.is_empty()
            && (state.entries.len() >= self.limits.entries
                || state.total.saturating_add(size) > self.limits.bytes)
        {
            let oldest = state
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)?;
            state.remove(oldest);
        }
        let path = self.dir.join(file_name(provider, id, url, extension));
        if let Err(error) = write_atomically(&self.dir, &path, body) {
            log::warn!("[samples] a preview could not be cached: {error}");
            return None;
        }
        let tick = state.tick();
        state.total = state.total.saturating_add(size);
        state.entries.push(Entry {
            provider: provider.to_string(),
            id: id.to_string(),
            url: url.to_string(),
            path: path.clone(),
            bytes: size,
            used: tick,
        });
        Some(path)
    }

    /// Drop the entry behind `path`, for a file that turned out unreadable.
    pub(crate) fn forget(&self, path: &Path) {
        if let Ok(mut state) = self.state.lock()
            && let Some(index) = state.entries.iter().position(|entry| entry.path == path)
        {
            state.remove(index);
        }
    }

    #[cfg(test)]
    fn usage(&self) -> (usize, u64) {
        let state = self.state.lock().unwrap();
        (state.entries.len(), state.total)
    }
}

/// A stable name that does not repeat the URL, which may carry a signed
/// query.
fn file_name(provider: &str, id: &str, url: &str, extension: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [provider, id, url] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let extension = if !extension.is_empty()
        && extension.len() <= 5
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        extension
    } else {
        "bin"
    };
    format!("{hex}.{extension}")
}

fn write_atomically(dir: &Path, path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix(".partial-")
        .tempfile_in(dir)?;
    file.write_all(body)?;
    file.flush()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Whether `body` is what the item declares for its download, when it
/// declares anything. A preview of the download URL is only cached when it
/// is, so a later add never starts from bytes the store would refuse.
pub(crate) fn matches_declared(item: &SampleItem, body: &[u8]) -> bool {
    if item.bytes.is_some_and(|bytes| bytes != body.len() as u64) {
        return false;
    }
    match item.sha256.as_deref().map(str::trim) {
        Some(declared) if !declared.is_empty() => {
            let actual: String = Sha256::digest(body)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            declared.eq_ignore_ascii_case(&actual)
        }
        _ => true,
    }
}

/// A fetcher that answers from the cache for one sample and fills it on a
/// miss. A request with an `Authorization` header always goes to the network:
/// its body depends on the credential, not only on the URL.
pub(crate) struct CachedFetch<'a> {
    pub(crate) inner: &'a dyn SampleFetch,
    pub(crate) cache: &'a PreviewCache,
    pub(crate) provider: &'a str,
    pub(crate) id: &'a str,
}

impl SampleFetch for CachedFetch<'_> {
    fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, FetchError> {
        self.get_with(url, max_bytes, &[])
    }

    fn plugin_network(&self) -> oc_sample::local_plugin::Network {
        self.inner.plugin_network()
    }

    fn get_with(
        &self,
        url: &str,
        max_bytes: usize,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, FetchError> {
        if headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        {
            return self.inner.get_with(url, max_bytes, headers);
        }
        if let Some(body) = self.cache.bytes_for(self.provider, self.id, url, max_bytes) {
            return Ok(FetchResponse { status: 200, body });
        }
        let response = self.inner.get_with(url, max_bytes, headers)?;
        if (200..300).contains(&response.status) {
            self.cache.store(
                self.provider,
                self.id,
                url,
                url_extension(url),
                &response.body,
            );
        }
        Ok(response)
    }
}

/// The file extension a URL names, or `bin`. The decoder uses it as a hint.
pub(crate) fn url_extension(url: &str) -> &str {
    Path::new(url.split(['?', '#']).next().unwrap_or(url))
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty() && extension.len() <= 5)
        .unwrap_or("bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_sample::model::{Quality, SampleKind};

    fn cache(limits: CacheLimits) -> (tempfile::TempDir, PreviewCache) {
        let root = tempfile::tempdir().unwrap();
        let cache = PreviewCache::open(root.path().join(PREVIEW_CACHE_DIR), limits).unwrap();
        (root, cache)
    }

    #[test]
    fn opening_the_cache_clears_an_earlier_session() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(PREVIEW_CACHE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale.mp3"), b"old").unwrap();
        let cache = PreviewCache::open(&dir, CacheLimits::default()).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(cache.file_for("p", "1"), None);
    }

    #[test]
    fn a_stored_body_is_found_by_sample_and_by_exact_url() {
        let (_root, cache) = cache(CacheLimits::default());
        let path = cache
            .store(
                "openverse",
                "openverse:a",
                "https://x.org/a.mp3",
                "mp3",
                b"abc",
            )
            .unwrap();
        assert!(path.extension().is_some_and(|extension| extension == "mp3"));
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        assert!(!path.to_string_lossy().contains("x.org"));
        assert_eq!(cache.file_for("openverse", "openverse:a"), Some(path));
        assert_eq!(
            cache.bytes_for("openverse", "openverse:a", "https://x.org/a.mp3", 16),
            Some(b"abc".to_vec())
        );
        // Another URL of the same sample is not the same file.
        assert_eq!(
            cache.bytes_for("openverse", "openverse:a", "https://x.org/a.wav", 16),
            None
        );
        // A body larger than the caller's bound is not served.
        assert_eq!(
            cache.bytes_for("openverse", "openverse:a", "https://x.org/a.mp3", 2),
            None
        );
        assert_eq!(cache.file_for("openverse", "openverse:b"), None);
        // No temporary file is left beside the entry.
        let names: Vec<String> = std::fs::read_dir(&cache.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted_by_count() {
        let (_root, cache) = cache(CacheLimits {
            entries: 2,
            bytes: 1024,
        });
        cache.store("p", "1", "u1", "mp3", b"1").unwrap();
        cache.store("p", "2", "u2", "mp3", b"2").unwrap();
        // Touch 1, so 2 is the oldest.
        assert!(cache.file_for("p", "1").is_some());
        let evicted = cache.file_for("p", "2").unwrap();
        assert!(cache.file_for("p", "1").is_some());
        cache.store("p", "3", "u3", "mp3", b"3").unwrap();
        assert_eq!(cache.file_for("p", "2"), None);
        assert!(!evicted.exists(), "an evicted file is deleted");
        assert!(cache.file_for("p", "1").is_some());
        assert!(cache.file_for("p", "3").is_some());
        assert_eq!(cache.usage(), (2, 2));
    }

    #[test]
    fn the_byte_bound_evicts_and_an_oversized_body_is_not_cached() {
        let (_root, cache) = cache(CacheLimits {
            entries: 10,
            bytes: 10,
        });
        cache.store("p", "1", "u1", "bin", &[0; 6]).unwrap();
        cache.store("p", "2", "u2", "bin", &[0; 4]).unwrap();
        assert_eq!(cache.usage(), (2, 10));
        cache.store("p", "3", "u3", "bin", &[0; 5]).unwrap();
        assert_eq!(cache.file_for("p", "1"), None);
        assert_eq!(cache.usage(), (2, 9));
        assert_eq!(cache.store("p", "4", "u4", "bin", &[0; 11]), None);
        assert_eq!(cache.usage(), (2, 9));
        // Storing the same URL again replaces its entry.
        cache.store("p", "3", "u3", "bin", &[1; 2]).unwrap();
        assert_eq!(cache.usage(), (2, 6));
        assert_eq!(cache.bytes_for("p", "3", "u3", 64), Some(vec![1; 2]));
    }

    #[test]
    fn a_vanished_file_is_a_miss_and_leaves_the_index() {
        let (_root, cache) = cache(CacheLimits::default());
        let path = cache.store("p", "1", "u1", "mp3", b"abc").unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cache.bytes_for("p", "1", "u1", 64), None);
        assert_eq!(cache.usage(), (0, 0));
        let path = cache.store("p", "1", "u1", "mp3", b"abc").unwrap();
        cache.forget(&path);
        assert_eq!(cache.file_for("p", "1"), None);
        assert!(!path.exists());
    }

    fn item(bytes: Option<u64>, sha256: Option<&str>) -> SampleItem {
        SampleItem {
            id: "p:1".into(),
            provider: "p".into(),
            name: "x".into(),
            kind: SampleKind::Unknown,
            duration_seconds: None,
            channels: None,
            sample_rate: None,
            license: oc_sample::license::License::cc0(),
            attribution: String::new(),
            source_url: String::new(),
            preview_url: None,
            download_url: "https://x.org/a.wav".into(),
            sha256: sha256.map(str::to_string),
            bytes,
            quality: Quality::Original,
            tags: Vec::new(),
        }
    }

    #[test]
    fn a_body_is_checked_against_the_declared_size_and_checksum() {
        // sha256("abc")
        let digest = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        assert!(matches_declared(&item(None, None), b"abc"));
        assert!(matches_declared(&item(Some(3), Some(digest)), b"abc"));
        assert!(!matches_declared(&item(Some(4), None), b"abc"));
        assert!(!matches_declared(&item(None, Some(digest)), b"abd"));
    }

    struct CountingFetch {
        calls: Mutex<Vec<(String, bool)>>,
    }

    impl SampleFetch for CountingFetch {
        fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, FetchError> {
            self.get_with(url, max_bytes, &[])
        }

        fn get_with(
            &self,
            url: &str,
            _max_bytes: usize,
            headers: &[(&str, &str)],
        ) -> Result<FetchResponse, FetchError> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), !headers.is_empty()));
            if url.contains("missing") {
                return Err(format!("HTTP 404 from {url}").into());
            }
            Ok(FetchResponse {
                status: 200,
                body: url.as_bytes().to_vec(),
            })
        }
    }

    #[test]
    fn a_cached_fetch_reuses_a_body_and_never_caches_an_authorized_one() {
        let (_root, cache) = cache(CacheLimits::default());
        let inner = CountingFetch {
            calls: Mutex::new(Vec::new()),
        };
        let fetch = CachedFetch {
            inner: &inner,
            cache: &cache,
            provider: "p",
            id: "p:1",
        };
        let url = "https://x.org/a.mp3";
        assert_eq!(fetch.get(url, 1024).unwrap().body, url.as_bytes());
        // A referer does not change the file, so the cached body answers.
        assert_eq!(
            fetch
                .get_with(url, 1024, &[("Referer", "https://x.org/")])
                .unwrap()
                .body,
            url.as_bytes()
        );
        assert_eq!(inner.calls.lock().unwrap().len(), 1);
        // A credentialed request always reaches the network.
        fetch
            .get_with(url, 1024, &[("Authorization", "Bearer t")])
            .unwrap();
        assert_eq!(inner.calls.lock().unwrap().len(), 2);
        // A failure is returned as is and caches nothing.
        assert!(fetch.get("https://x.org/missing.mp3", 1024).is_err());
        assert!(fetch.get("https://x.org/missing.mp3", 1024).is_err());
        assert_eq!(inner.calls.lock().unwrap().len(), 4);
        assert_eq!(cache.usage().0, 1);
    }
}
