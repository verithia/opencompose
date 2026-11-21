//! The one network client. Everything else in this crate is pure.

use std::io::Read;
use std::time::Duration;

use crate::model::{FailureKind, MAX_RETRY_AFTER_SECS};
use crate::provider::{FetchError, FetchResponse, SampleFetch};

/// Deadline for one search request. A search fans out to several markets in
/// parallel, so one unreachable market must not hold the others' results.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Deadline for one file download: a resolve plus a whole sample file.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// Sent to both catalogues. Wikimedia asks for an identifying agent, and
/// Freesound throttles anonymous-looking clients.
const USER_AGENT: &str = concat!(
    "OpenCompose/",
    env!("CARGO_PKG_VERSION"),
    " (sample marketplace; +https://opencompose.org)"
);

/// The proxy decision a client was built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyChoice<'a> {
    System,
    Manual(&'a str),
    None,
}

/// The blocking HTTP client every sample provider fetches through.
pub struct HttpFetch {
    client: reqwest::blocking::Client,
    network: crate::local_plugin::Network,
}

impl HttpFetch {
    /// Build a client with the download deadline, [`DOWNLOAD_TIMEOUT`].
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(DOWNLOAD_TIMEOUT)
    }

    /// A client that follows an explicit proxy decision instead of the system.
    pub fn with_proxy(timeout: Duration, proxy: ProxyChoice<'_>) -> Result<Self, String> {
        let network = crate::local_plugin::Network {
            proxy: match proxy {
                ProxyChoice::System => crate::local_plugin::Proxy::System,
                ProxyChoice::None => crate::local_plugin::Proxy::None,
                ProxyChoice::Manual(url) => crate::local_plugin::Proxy::Manual(url.to_owned()),
            },
            timeout_seconds: timeout.as_secs(),
        };
        let mut builder = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout);
        match proxy {
            ProxyChoice::System => {}
            ProxyChoice::None => {
                builder = builder.no_proxy();
            }
            ProxyChoice::Manual(url) => {
                let parsed = reqwest::Proxy::all(url)
                    .map_err(|error| format!("invalid proxy '{url}': {error}"))?;
                builder = builder.proxy(parsed);
            }
        }
        let client = builder
            .build()
            .map_err(|error| format!("Failed to create the sample HTTP client: {error}"))?;
        Ok(Self { client, network })
    }

    /// A client whose whole request must finish inside `timeout`. A search uses
    /// a short one: one unreachable market must not hold the other markets'
    /// results for minutes.
    pub fn with_timeout(timeout: Duration) -> Result<Self, String> {
        Self::with_proxy(timeout, ProxyChoice::System)
    }
}

impl SampleFetch for HttpFetch {
    fn plugin_network(&self) -> crate::local_plugin::Network {
        self.network.clone()
    }

    fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, FetchError> {
        self.get_with(url, max_bytes, &[])
    }

    fn get_with(
        &self,
        url: &str,
        max_bytes: usize,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, FetchError> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        // Every message below goes through `redact`: a provider that still puts
        // a credential in the query string must not leak it through an error.
        let response = request.send().map_err(|error| {
            let kind = if error.is_timeout() {
                FailureKind::Timeout
            } else {
                FailureKind::Network
            };
            FetchError::new(
                kind,
                format!(
                    "request to {} failed: {}",
                    redact(url),
                    redact(&error.to_string())
                ),
            )
        })?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok());
            return Err(status_error(status, retry_after, &redact(url)));
        }
        if let Some(length) = response.content_length()
            && length > max_bytes as u64
        {
            return Err(FetchError::new(
                FailureKind::Other,
                format!(
                    "{} declares {length} bytes, more than the {max_bytes} byte limit",
                    redact(url)
                ),
            ));
        }
        let mut body = Vec::new();
        response
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| {
                let kind = if error.kind() == std::io::ErrorKind::TimedOut {
                    FailureKind::Timeout
                } else {
                    FailureKind::Network
                };
                FetchError::new(kind, format!("reading {} failed: {error}", redact(url)))
            })?;
        if body.len() > max_bytes {
            return Err(FetchError::new(
                FailureKind::Other,
                format!("{} is larger than {max_bytes} bytes", redact(url)),
            ));
        }
        Ok(FetchResponse { status, body })
    }
}

/// The failure for a non-2xx status. `url` is already redacted.
fn status_error(status: u16, retry_after: Option<&str>, url: &str) -> FetchError {
    let kind = match status {
        429 => FailureKind::RateLimited,
        401 | 403 => FailureKind::Unauthorized,
        404 | 410 => FailureKind::NotFound,
        500..=599 => FailureKind::Unavailable,
        _ => FailureKind::Other,
    };
    // Only a throttle or an outage asks the client to wait; a `Retry-After` on
    // any other status says nothing about when to come back.
    let retry_after_secs = match kind {
        FailureKind::RateLimited | FailureKind::Unavailable => {
            retry_after.and_then(parse_retry_after)
        }
        _ => None,
    };
    FetchError {
        kind,
        status: Some(status),
        retry_after_secs,
        message: format!("HTTP {status} from {url}"),
    }
}

/// Read a `Retry-After` header given in seconds. The HTTP-date form gives a
/// wall-clock time this client does not trust its own clock against, so it
/// reads as absent. The value is capped at [`MAX_RETRY_AFTER_SECS`].
pub fn parse_retry_after(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value.len() > 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.min(MAX_RETRY_AFTER_SECS))
}

/// Query parameters whose value is a credential. The comparison is
/// case-insensitive and matches the whole name.
const SECRET_PARAMS: [&str; 8] = [
    "token",
    "api_key",
    "client_id",
    "apikey",
    "key",
    "secret",
    "password",
    "access_token",
];

/// The replacement for a credential value.
const REDACTED: &str = "[redacted]";

/// Replace the value of every secret query parameter in `text` with
/// `[redacted]`, leaving the rest of the URL readable. `text` may be a URL or
/// a message that embeds one, because a transport error repeats the request
/// URL it failed on.
fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(value_start) = next_secret_value(rest) {
        out.push_str(&rest[..value_start]);
        out.push_str(REDACTED);
        let tail = &rest[value_start..];
        let end = tail
            .find(['&', '#', ' ', ')', '"', '\''])
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Where the value of the first secret `?name=` or `&name=` starts.
fn next_secret_value(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != b'?' && bytes[index] != b'&' {
            continue;
        }
        let start = index + 1;
        let end = bytes[start..]
            .iter()
            .position(|byte| !is_parameter_name_byte(*byte))
            .map(|offset| start + offset)
            .unwrap_or(bytes.len());
        if bytes.get(end) != Some(&b'=') {
            continue;
        }
        if SECRET_PARAMS
            .iter()
            .any(|secret| text[start..end].eq_ignore_ascii_case(secret))
        {
            return Some(end + 1);
        }
    }
    None
}

fn is_parameter_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// Answer the first connection with `response` and close it.
    fn serve_once(response: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    /// Accept the first connection and never answer, so the client times out.
    fn serve_stalled() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        port
    }

    /// A client that bypasses the system proxy, so the loopback address is
    /// reached directly.
    fn client(timeout: Duration) -> HttpFetch {
        HttpFetch::with_proxy(timeout, ProxyChoice::None).unwrap()
    }

    #[test]
    fn a_secret_query_value_is_redacted_and_the_rest_survives() {
        assert_eq!(
            redact("https://freesound.org/apiv2/search/?fields=id&token=sk-secret&page=2"),
            "https://freesound.org/apiv2/search/?fields=id&token=[redacted]&page=2"
        );
        assert_eq!(
            redact("HTTP 401 from https://example.org/a?key=abc"),
            "HTTP 401 from https://example.org/a?key=[redacted]"
        );
    }

    #[test]
    fn every_secret_name_is_redacted_case_insensitively() {
        for name in SECRET_PARAMS {
            let url = format!("https://example.org/a?{name}=value");
            let redacted = redact(&url);
            assert!(!redacted.contains("value"), "{name}: {redacted}");
            assert_eq!(redacted, format!("https://example.org/a?{name}={REDACTED}"));
        }
        assert_eq!(
            redact("https://example.org/a?API_KEY=value"),
            "https://example.org/a?API_KEY=[redacted]"
        );
    }

    #[test]
    fn a_parameter_that_merely_contains_a_secret_name_is_kept() {
        assert_eq!(
            redact("https://example.org/a?monkey=1&page=2&keyboard=3"),
            "https://example.org/a?monkey=1&page=2&keyboard=3"
        );
    }

    #[test]
    fn a_text_without_a_secret_parameter_is_unchanged() {
        assert_eq!(
            redact("https://example.org/a/b?c"),
            "https://example.org/a/b?c"
        );
        assert_eq!(redact("no url at all"), "no url at all");
    }

    #[test]
    fn a_failing_request_does_not_repeat_the_key() {
        let port = serve_once(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let url = format!("http://127.0.0.1:{port}/search/?fields=id&token=sk-secret");
        let error = client(Duration::from_secs(5)).get(&url, 1024).unwrap_err();

        assert_eq!(error.kind, FailureKind::Unauthorized);
        assert_eq!(error.status, Some(401));
        let error = error.message;
        assert!(error.contains("HTTP 401"), "{error}");
        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
    }

    #[test]
    fn a_throttled_request_carries_its_retry_after() {
        let port = serve_once(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 42\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let url = format!("http://127.0.0.1:{port}/search/");
        let error = client(Duration::from_secs(5)).get(&url, 1024).unwrap_err();
        assert_eq!(error.kind, FailureKind::RateLimited);
        assert_eq!(error.status, Some(429));
        assert_eq!(error.retry_after_secs, Some(42));
    }

    #[test]
    fn a_status_maps_to_one_failure_kind() {
        let cases = [
            (429, FailureKind::RateLimited),
            (401, FailureKind::Unauthorized),
            (403, FailureKind::Unauthorized),
            (404, FailureKind::NotFound),
            (410, FailureKind::NotFound),
            (500, FailureKind::Unavailable),
            (503, FailureKind::Unavailable),
            (504, FailureKind::Unavailable),
            (400, FailureKind::Other),
            (418, FailureKind::Other),
        ];
        for (status, kind) in cases {
            let error = status_error(status, Some("5"), "https://example.org/");
            assert_eq!(error.kind, kind, "{status}");
            assert_eq!(error.status, Some(status));
        }
        assert_eq!(status_error(503, Some("7"), "u").retry_after_secs, Some(7));
        assert_eq!(status_error(404, Some("7"), "u").retry_after_secs, None);
        assert_eq!(status_error(429, None, "u").retry_after_secs, None);
    }

    #[test]
    fn a_retry_after_is_read_in_seconds_and_bounded() {
        assert_eq!(parse_retry_after(" 120 "), Some(120));
        assert_eq!(parse_retry_after("0"), Some(0));
        assert_eq!(
            parse_retry_after("999999999999"),
            Some(MAX_RETRY_AFTER_SECS)
        );
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after("1.5"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after(&"9".repeat(40)), None);
    }

    #[test]
    fn a_declared_size_error_does_not_repeat_the_key() {
        let port =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n");
        let url = format!("http://127.0.0.1:{port}/search/?token=sk-secret");
        let error = client(Duration::from_secs(5))
            .get(&url, 16)
            .unwrap_err()
            .message;

        assert!(error.contains("declares 4096 bytes"), "{error}");
        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
    }

    #[test]
    fn a_size_error_does_not_repeat_the_key() {
        // No `Content-Length`, so the body ends at EOF and the byte limit
        // reports the overflow.
        let port = serve_once("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n0123456789abcdef");
        let url = format!("http://127.0.0.1:{port}/search/?token=sk-secret");
        let error = client(Duration::from_secs(5))
            .get(&url, 8)
            .unwrap_err()
            .message;

        assert!(error.contains("larger than 8 bytes"), "{error}");
        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
    }

    #[test]
    fn a_timed_out_request_does_not_repeat_the_key() {
        let port = serve_stalled();
        let url = format!("http://127.0.0.1:{port}/search/?fields=id&token=sk-secret");
        let error = client(Duration::from_millis(300))
            .get(&url, 1024)
            .unwrap_err();

        assert_eq!(error.kind, FailureKind::Timeout);
        let error = error.message;
        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
        assert!(error.contains("127.0.0.1"), "{error}");
    }
}
