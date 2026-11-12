//! Site icons for the Store page.
//!
//! The repository ships no third-party logos. When the Store page shows a
//! site, the backend fetches that site's own favicon once, through the proxy
//! chosen in Preferences, and keeps it in the app cache. The page receives a
//! `data:` URL, so the webview never contacts the site itself, and anything
//! that fails a check here leaves the card on our category glyph.
//!
//! Every response is untrusted: only the site's own origin is asked, redirects
//! stay on public https hosts, the body is capped while it streams, and only
//! raster formats an `<img>` decodes inertly (PNG, ICO, WebP) are accepted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use reqwest::Url;
use sha2::{Digest, Sha256};

/// Largest icon accepted. A favicon is a few kilobytes; the cap bounds memory,
/// the cache file and the `data:` URL sent to the page.
pub const MAX_ICON_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Total time for one candidate, body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_REDIRECTS: usize = 3;
/// A cached icon is refreshed after a month.
const HIT_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A site without a usable icon is asked again after a day, not on every open.
const MISS_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Paths tried in order on the site's own origin.
const CANDIDATE_PATHS: [&str; 3] = ["/favicon.ico", "/apple-touch-icon.png", "/favicon.png"];
const MISS_EXTENSION: &str = "miss";

/// Raster formats the page may display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconFormat {
    Png,
    Ico,
    Webp,
}

impl IconFormat {
    const ALL: [IconFormat; 3] = [IconFormat::Png, IconFormat::Ico, IconFormat::Webp];

    fn mime(self) -> &'static str {
        match self {
            IconFormat::Png => "image/png",
            IconFormat::Ico => "image/x-icon",
            IconFormat::Webp => "image/webp",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            IconFormat::Png => "png",
            IconFormat::Ico => "ico",
            IconFormat::Webp => "webp",
        }
    }
}

/// Why a site or a response was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconError {
    InvalidUrl,
    NotHttps,
    /// An IP literal, a loopback or local-network name, or a single label.
    NotPublicHost,
    Credentials,
    NonDefaultPort,
    Status(u16),
    TooLarge,
    NotAnImage,
    Network,
}

/// Checks that `url` names a public https host on the default port.
fn check_remote(url: &Url) -> Result<String, IconError> {
    if url.scheme() != "https" {
        return Err(IconError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(IconError::Credentials);
    }
    if url.port().is_some() {
        return Err(IconError::NonDefaultPort);
    }
    // `domain()` is None for an IP literal, so only names reach the checks.
    let Some(host) = url.domain() else {
        return Err(IconError::NotPublicHost);
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let local = host == "localhost"
        || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix));
    if local || !host.contains('.') {
        return Err(IconError::NotPublicHost);
    }
    Ok(host)
}

/// The origin of a Store site page, and its host as the cache key.
pub fn site_origin(page_url: &str) -> Result<(Url, String), IconError> {
    let url = Url::parse(page_url).map_err(|_| IconError::InvalidUrl)?;
    let host = check_remote(&url)?;
    let origin = Url::parse(&format!("https://{host}/")).map_err(|_| IconError::InvalidUrl)?;
    Ok((origin, host))
}

/// The icon addresses asked for one origin, in order.
pub fn candidates(origin: &Url) -> Vec<Url> {
    CANDIDATE_PATHS
        .iter()
        .filter_map(|path| origin.join(path).ok())
        .collect()
}

/// Identifies the format from the bytes; the Content-Type header is not
/// trusted, because many servers label a PNG favicon as `image/x-icon`.
pub fn sniff(bytes: &[u8]) -> Option<IconFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(IconFormat::Png);
    }
    // ICONDIR: reserved 0, type 1, then a non-zero image count and at least one
    // 16-byte directory entry.
    if bytes.len() >= 22 && bytes[..4] == [0, 0, 1, 0] && (bytes[4] != 0 || bytes[5] != 0) {
        return Some(IconFormat::Ico);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(IconFormat::Webp);
    }
    None
}

/// The `data:` URL the page displays.
pub fn data_url(format: IconFormat, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        format.mime(),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Timeouts, redirect rules and identification shared by every icon client.
fn base_builder() -> reqwest::ClientBuilder {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if check_remote(attempt.url()).is_err() {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(policy)
        .user_agent(concat!("OpenCompose/", env!("CARGO_PKG_VERSION")))
}

fn client() -> Result<reqwest::Client, IconError> {
    crate::net_proxy::apply(base_builder())
        .https_only(true)
        .build()
        .map_err(|_| IconError::Network)
}

/// Fetches one candidate, refusing a body over [`MAX_ICON_BYTES`] before it is
/// read in full, and anything that is not an accepted raster format.
pub async fn fetch_bounded(
    client: &reqwest::Client,
    url: Url,
) -> Result<(IconFormat, Vec<u8>), IconError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| IconError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(IconError::Status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ICON_BYTES as u64)
    {
        return Err(IconError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| IconError::Network)? {
        if body.len() + chunk.len() > MAX_ICON_BYTES {
            return Err(IconError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let format = sniff(&body).ok_or(IconError::NotAnImage)?;
    Ok((format, body))
}

/// What the cache knows about one host.
#[derive(Debug, PartialEq, Eq)]
pub enum Cached {
    Icon(IconFormat, Vec<u8>),
    /// Asked recently and nothing usable came back.
    Missing,
    Unknown,
}

/// Icons on disk, one file per host, named by a hash of the host.
pub struct IconCache {
    dir: PathBuf,
}

impl IconCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, host: &str, extension: &str) -> PathBuf {
        let digest = Sha256::digest(host.as_bytes());
        let name: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.dir.join(format!("{name}.{extension}"))
    }

    fn fresh(path: &Path, ttl: Duration, now: SystemTime) -> bool {
        let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
            return false;
        };
        // A timestamp in the future counts as fresh rather than as expired.
        now.duration_since(modified).map_or(true, |age| age < ttl)
    }

    pub fn lookup(&self, host: &str, now: SystemTime) -> Cached {
        for format in IconFormat::ALL {
            let path = self.path(host, format.extension());
            if !Self::fresh(&path, HIT_TTL, now) {
                continue;
            }
            // A truncated or replaced file is ignored and fetched again.
            if let Ok(bytes) = fs::read(&path)
                && bytes.len() <= MAX_ICON_BYTES
                && sniff(&bytes) == Some(format)
            {
                return Cached::Icon(format, bytes);
            }
        }
        if Self::fresh(&self.path(host, MISS_EXTENSION), MISS_TTL, now) {
            return Cached::Missing;
        }
        Cached::Unknown
    }

    /// Writes beside the target and renames, so a reader never sees half a file.
    fn publish(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let scratch = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&scratch, bytes)?;
        fs::rename(&scratch, path).inspect_err(|_| {
            let _ = fs::remove_file(&scratch);
        })
    }

    pub fn store_icon(&self, host: &str, format: IconFormat, bytes: &[u8]) -> io::Result<()> {
        self.publish(&self.path(host, format.extension()), bytes)?;
        // The newest answer is the only one left for this host.
        for other in IconFormat::ALL.into_iter().filter(|other| *other != format) {
            let _ = fs::remove_file(self.path(host, other.extension()));
        }
        let _ = fs::remove_file(self.path(host, MISS_EXTENSION));
        Ok(())
    }

    pub fn store_miss(&self, host: &str) -> io::Result<()> {
        self.publish(&self.path(host, MISS_EXTENSION), b"")
    }
}

/// Answers from the cache, or asks each candidate in turn and records the
/// result. A host that yields nothing usable is remembered as missing.
pub async fn fetch_and_cache(
    cache: &IconCache,
    client: &reqwest::Client,
    host: &str,
    candidates: Vec<Url>,
    now: SystemTime,
) -> Option<String> {
    match cache.lookup(host, now) {
        Cached::Icon(format, bytes) => return Some(data_url(format, &bytes)),
        Cached::Missing => return None,
        Cached::Unknown => {}
    }
    for candidate in candidates {
        let Ok((format, bytes)) = fetch_bounded(client, candidate).await else {
            continue;
        };
        if let Err(error) = cache.store_icon(host, format, &bytes) {
            log::debug!("store icon cache: {error}");
        }
        return Some(data_url(format, &bytes));
    }
    if let Err(error) = cache.store_miss(host) {
        log::debug!("store icon cache: {error}");
    }
    None
}

/// The icon for a Store site page as a `data:` URL, or `None` when the site is
/// refused or offers nothing usable; the page then shows our glyph.
#[tauri::command]
pub async fn store_site_icon(app: tauri::AppHandle, url: String) -> Option<String> {
    use tauri::Manager as _;
    let (origin, host) = site_origin(&url).ok()?;
    let dir = app.path().app_cache_dir().ok()?.join("store-icons");
    let client = client().ok()?;
    fetch_and_cache(
        &IconCache::new(dir),
        &client,
        &host,
        candidates(&origin),
        SystemTime::now(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

    fn ico() -> Vec<u8> {
        let mut bytes = vec![0, 0, 1, 0, 1, 0];
        bytes.extend_from_slice(&[0u8; 16]);
        bytes
    }

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            body,
        )
    }

    /// Serves one canned response per connection and returns the request lines.
    fn spawn_server(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&request);
                requests.push(text.lines().next().unwrap_or_default().to_owned());
                // The client may hang up early on an oversized body.
                let _ = stream.write_all(&reply);
            }
            requests
        });
        (format!("http://{address}"), handle)
    }

    fn test_client() -> reqwest::Client {
        base_builder().no_proxy().build().unwrap()
    }

    fn url(base: &str, path: &str) -> Url {
        Url::parse(&format!("{base}{path}")).unwrap()
    }

    fn cache() -> (tempfile::TempDir, IconCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = IconCache::new(dir.path().join("store-icons"));
        (dir, cache)
    }

    #[test]
    fn a_site_page_resolves_to_its_https_origin() {
        let (origin, host) =
            site_origin("https://Surge-Synthesizer.GitHub.io/some/page?x=1").unwrap();
        assert_eq!(origin.as_str(), "https://surge-synthesizer.github.io/");
        assert_eq!(host, "surge-synthesizer.github.io");
        let paths: Vec<String> = candidates(&origin)
            .iter()
            .map(|url| url.path().to_owned())
            .collect();
        assert_eq!(
            paths,
            ["/favicon.ico", "/apple-touch-icon.png", "/favicon.png"]
        );
    }

    #[test]
    fn only_public_https_hosts_are_asked() {
        let refused = [
            ("http://freesound.org/", IconError::NotHttps),
            ("ftp://freesound.org/", IconError::NotHttps),
            ("https://127.0.0.1/", IconError::NotPublicHost),
            ("https://[::1]/", IconError::NotPublicHost),
            ("https://10.0.0.8/", IconError::NotPublicHost),
            ("https://localhost/", IconError::NotPublicHost),
            ("https://studio.localhost/", IconError::NotPublicHost),
            ("https://nas.local/", IconError::NotPublicHost),
            ("https://router.home.arpa/", IconError::NotPublicHost),
            ("https://intranet/", IconError::NotPublicHost),
            ("https://user:secret@freesound.org/", IconError::Credentials),
            ("https://freesound.org:8443/", IconError::NonDefaultPort),
            ("not a url", IconError::InvalidUrl),
        ];
        for (page, expected) in refused {
            assert_eq!(site_origin(page).unwrap_err(), expected, "{page}");
        }
        // The default port written out is still the default port.
        assert!(site_origin("https://freesound.org:443/").is_ok());
    }

    #[test]
    fn only_inert_raster_formats_are_recognised() {
        assert_eq!(sniff(PNG), Some(IconFormat::Png));
        assert_eq!(sniff(&ico()), Some(IconFormat::Ico));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some(IconFormat::Webp));
        for refused in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
            b"<!doctype html><html></html>",
            b"GIF89a\x01\0\x01\0",
            b"",
            // An ICO header that declares no images.
            &[
                0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            // A directory header without its entry.
            &[0, 0, 1, 0, 1, 0],
        ] {
            assert_eq!(sniff(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn the_data_url_names_the_sniffed_type() {
        assert_eq!(
            data_url(IconFormat::Ico, b"\0\0\x01\0"),
            "data:image/x-icon;base64,AAABAA=="
        );
    }

    #[tokio::test]
    async fn a_small_icon_is_returned_with_its_format() {
        let (base, server) = spawn_server(vec![ok(PNG)]);
        let fetched = fetch_bounded(&test_client(), url(&base, "/favicon.ico")).await;
        assert_eq!(fetched, Ok((IconFormat::Png, PNG.to_vec())));
        assert_eq!(server.join().unwrap(), ["GET /favicon.ico HTTP/1.1"]);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_whether_or_not_it_is_declared() {
        let declared = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", MAX_ICON_BYTES + 1),
            b"",
        );
        let mut streamed_body = PNG.to_vec();
        streamed_body.resize(MAX_ICON_BYTES + 1, 0);
        // No Content-Length: the body runs until the server closes.
        let streamed = response("200 OK", "", &streamed_body);
        let (base, server) = spawn_server(vec![declared, streamed]);
        let client = test_client();
        assert_eq!(
            fetch_bounded(&client, url(&base, "/a")).await,
            Err(IconError::TooLarge)
        );
        assert_eq!(
            fetch_bounded(&client, url(&base, "/b")).await,
            Err(IconError::TooLarge)
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn an_error_status_or_a_non_image_is_refused() {
        let (base, server) = spawn_server(vec![
            response("404 Not Found", "Content-Length: 0\r\n", b""),
            ok(b"<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>"),
        ]);
        let client = test_client();
        assert_eq!(
            fetch_bounded(&client, url(&base, "/favicon.ico")).await,
            Err(IconError::Status(404))
        );
        assert_eq!(
            fetch_bounded(&client, url(&base, "/favicon.ico")).await,
            Err(IconError::NotAnImage)
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn a_redirect_off_public_https_is_not_followed() {
        let (base, server) = spawn_server(vec![response(
            "302 Found",
            "Location: http://127.0.0.1:1/favicon.ico\r\nContent-Length: 0\r\n",
            b"",
        )]);
        assert_eq!(
            fetch_bounded(&test_client(), url(&base, "/favicon.ico")).await,
            Err(IconError::Status(302))
        );
        // One request: the redirect target is never contacted.
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn the_cache_returns_a_fresh_icon_and_forgets_a_stale_one() {
        let (_dir, cache) = cache();
        let now = SystemTime::now();
        assert_eq!(cache.lookup("freesound.org", now), Cached::Unknown);
        cache
            .store_icon("freesound.org", IconFormat::Png, PNG)
            .unwrap();
        assert_eq!(
            cache.lookup("freesound.org", now),
            Cached::Icon(IconFormat::Png, PNG.to_vec())
        );
        assert_eq!(cache.lookup("vital.audio", now), Cached::Unknown);
        let later = now + HIT_TTL + Duration::from_secs(60);
        assert_eq!(cache.lookup("freesound.org", later), Cached::Unknown);
    }

    #[test]
    fn a_miss_is_remembered_for_a_day_and_an_icon_replaces_it() {
        let (_dir, cache) = cache();
        let now = SystemTime::now();
        cache.store_miss("kvraudio.com").unwrap();
        assert_eq!(cache.lookup("kvraudio.com", now), Cached::Missing);
        let tomorrow = now + MISS_TTL + Duration::from_secs(60);
        assert_eq!(cache.lookup("kvraudio.com", tomorrow), Cached::Unknown);

        cache
            .store_icon("kvraudio.com", IconFormat::Ico, &ico())
            .unwrap();
        assert_eq!(
            cache.lookup("kvraudio.com", now),
            Cached::Icon(IconFormat::Ico, ico())
        );
        // A newer answer in another format is the only one kept.
        cache
            .store_icon("kvraudio.com", IconFormat::Png, PNG)
            .unwrap();
        let files = fs::read_dir(&cache.dir).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn a_corrupted_cache_file_is_ignored() {
        let (_dir, cache) = cache();
        cache
            .store_icon("freesound.org", IconFormat::Png, PNG)
            .unwrap();
        fs::write(cache.path("freesound.org", "png"), b"<html>").unwrap();
        assert_eq!(
            cache.lookup("freesound.org", SystemTime::now()),
            Cached::Unknown
        );
    }

    #[tokio::test]
    async fn the_first_usable_candidate_is_cached_and_not_fetched_again() {
        let (base, server) = spawn_server(vec![
            response("404 Not Found", "Content-Length: 0\r\n", b""),
            ok(PNG),
        ]);
        let (_dir, cache) = cache();
        let client = test_client();
        let list = vec![
            url(&base, "/favicon.ico"),
            url(&base, "/apple-touch-icon.png"),
        ];
        let now = SystemTime::now();
        let expected = data_url(IconFormat::Png, PNG);

        let first = fetch_and_cache(&cache, &client, "example.org", list.clone(), now).await;
        assert_eq!(first.as_deref(), Some(expected.as_str()));
        // The server accepts two connections only, so a second fetch would be
        // refused and this answer would be None.
        let second = fetch_and_cache(&cache, &client, "example.org", list, now).await;
        assert_eq!(second.as_deref(), Some(expected.as_str()));
        assert_eq!(
            server.join().unwrap(),
            [
                "GET /favicon.ico HTTP/1.1",
                "GET /apple-touch-icon.png HTTP/1.1"
            ]
        );
    }

    #[tokio::test]
    async fn a_site_without_a_usable_icon_is_remembered_as_missing() {
        let (base, server) = spawn_server(vec![
            response("404 Not Found", "Content-Length: 0\r\n", b""),
            ok(b"<!doctype html>"),
        ]);
        let (_dir, cache) = cache();
        let client = test_client();
        let list = vec![url(&base, "/favicon.ico"), url(&base, "/favicon.png")];
        let now = SystemTime::now();
        assert_eq!(
            fetch_and_cache(&cache, &client, "example.org", list.clone(), now).await,
            None
        );
        assert_eq!(cache.lookup("example.org", now), Cached::Missing);
        // Remembered: no request is made while the miss is fresh.
        assert_eq!(
            fetch_and_cache(&cache, &client, "example.org", list, now).await,
            None
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }
}
