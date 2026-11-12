//! The one network client. Everything else in this crate is pure.

use std::io::Read;
use std::time::Duration;

use crate::provider::{FetchResponse, SampleFetch};

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
    /// Build a client with the crate's default 180-second timeout.
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(Duration::from_secs(180))
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

    fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, String> {
        self.get_with(url, max_bytes, &[])
    }

    fn get_with(
        &self,
        url: &str,
        max_bytes: usize,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, String> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        // Every message below goes through `redact`: a provider that still puts
        // a credential in the query string must not leak it through an error.
        let response = request.send().map_err(|error| {
            format!(
                "request to {} failed: {}",
                redact(url),
                redact(&error.to_string())
            )
        })?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(format!("HTTP {status} from {}", redact(url)));
        }
        if let Some(length) = response.content_length()
            && length > max_bytes as u64
        {
            return Err(format!(
                "{} declares {length} bytes, more than the {max_bytes} byte limit",
                redact(url)
            ));
        }
        let mut body = Vec::new();
        response
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| format!("reading {} failed: {error}", redact(url)))?;
        if body.len() > max_bytes {
            return Err(format!("{} is larger than {max_bytes} bytes", redact(url)));
        }
        Ok(FetchResponse { status, body })
    }
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

        assert!(error.contains("HTTP 401"), "{error}");
        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
    }

    #[test]
    fn a_declared_size_error_does_not_repeat_the_key() {
        let port =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n");
        let url = format!("http://127.0.0.1:{port}/search/?token=sk-secret");
        let error = client(Duration::from_secs(5)).get(&url, 16).unwrap_err();

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
        let error = client(Duration::from_secs(5)).get(&url, 8).unwrap_err();

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

        assert!(error.contains("token=[redacted]"), "{error}");
        assert!(!error.contains("sk-secret"), "{error}");
        assert!(error.contains("127.0.0.1"), "{error}");
    }
}
