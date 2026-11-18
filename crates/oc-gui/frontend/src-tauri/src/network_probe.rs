//! Connection diagnosis for the network preference.
//!
//! A saved proxy may be running while its upstream is unreachable, so the
//! preference is only trusted after the plugin index answers through it. The
//! same module finds local proxy candidates by asking them to tunnel the index
//! host, then verifies each route against the index.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use oc_market::index::{MANIFEST_FILE, PRIMARY_BASE_URL};
use serde::Serialize;

use crate::gui_settings::{NetworkSettings, ProxyMode};

/// Ports a local proxy commonly listens on: the 7890-7899 block and the
/// defaults of other common proxy applications.
pub const LOCAL_PROXY_PORTS: &[u16] = &[
    7890, 7891, 7892, 7893, 7894, 7895, 7896, 7897, 7898, 7899, 1080, 1087, 1089, 3128, 6152, 8080,
    8118, 8888, 8889, 10809, 20171, 20172, 33210,
];

const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
const IO_TIMEOUT: Duration = Duration::from_millis(350);
const CONNECT_LINE_LIMIT: usize = 128;
const ROUTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ROUTE_TOTAL_TIMEOUT: Duration = Duration::from_secs(8);

/// Which route a probe tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    System,
    Direct,
    Proxy,
}

/// One tested route and what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteProbe {
    pub kind: RouteKind,
    pub url: Option<String>,
    pub ok: bool,
    pub ms: u64,
    pub error: Option<String>,
}

/// The saved route beside every route that was tested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDiagnosis {
    pub current: RouteProbe,
    pub routes: Vec<RouteProbe>,
}

/// The index document every route fetches.
fn index_url() -> String {
    format!("{PRIMARY_BASE_URL}{MANIFEST_FILE}")
}

/// The authority a local proxy must tunnel: the host and port of the index.
fn index_authority() -> Option<String> {
    let url = reqwest::Url::parse(PRIMARY_BASE_URL).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default().unwrap_or(443);
    Some(format!("{host}:{port}"))
}

/// True when `line` is an HTTP/1.x status line with code 200.
fn status_line_accepted(line: &[u8]) -> bool {
    let line = std::str::from_utf8(line).unwrap_or("");
    let mut parts = line.split_whitespace();
    matches!(
        (parts.next(), parts.next()),
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") && code == "200"
    )
}

/// Ask one loopback port to tunnel the index authority.
///
/// Only a CONNECT answer of 200 marks a proxy: an ordinary web server replies
/// with its own status, and a non-HTTP service fails the status check.
fn probe_connect(port: u16, authority: &str) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return false;
    };
    if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
    {
        return false;
    }
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut line = Vec::with_capacity(CONNECT_LINE_LIMIT);
    let mut byte = [0u8; 1];
    while line.len() < CONNECT_LINE_LIMIT {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.ends_with(b"\r\n") {
                    break;
                }
            }
        }
    }
    status_line_accepted(&line)
}

/// Local ports that answered CONNECT 200 for the index authority.
fn candidates_for(authority: &str) -> Vec<u16> {
    thread::scope(|scope| {
        let handles: Vec<_> = LOCAL_PROXY_PORTS
            .iter()
            .map(|&port| scope.spawn(move || (port, probe_connect(port, authority))))
            .collect();
        let mut found: Vec<u16> = handles
            .into_iter()
            .filter_map(|handle| match handle.join() {
                Ok((port, true)) => Some(port),
                _ => None,
            })
            .collect();
        found.sort_unstable();
        found
    })
}

/// Local proxy candidates for the real index host.
pub fn local_proxy_candidates() -> Vec<u16> {
    match index_authority() {
        Some(authority) => candidates_for(&authority),
        None => Vec::new(),
    }
}

/// A GET of one index URL through one route.
pub trait ProbeFetch: Sync {
    fn fetch(&self, url: &str, mode: ProxyMode, proxy_url: &str) -> Result<u16, String>;
}

/// The production fetch: reqwest with the route applied through `net_proxy`.
pub struct ReqwestProbe;

impl ProbeFetch for ReqwestProbe {
    fn fetch(&self, url: &str, mode: ProxyMode, proxy_url: &str) -> Result<u16, String> {
        let builder = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "OpenCompose/",
                env!("CARGO_PKG_VERSION"),
                " network-probe"
            ))
            .connect_timeout(ROUTE_CONNECT_TIMEOUT)
            .timeout(ROUTE_TOTAL_TIMEOUT);
        let client = crate::net_proxy::apply_blocking_choice(builder, mode, proxy_url)
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(url)
            .send()
            .map_err(|error| error_chain(&error))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(format!("HTTP {status}"));
        }
        // The status proves the tunnel and the TLS handshake; reading the body
        // adds nothing, so the probe closes the response here.
        Ok(status)
    }
}

/// The error and its sources, outermost first. The cause that tells routes
/// apart (a refused connection, a cut TLS handshake, a timeout) sits at the
/// end of the chain, so the outer message alone hides it.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        if !text.contains(&message) {
            text.push_str(": ");
            text.push_str(&message);
        }
        source = cause.source();
    }
    text
}

#[derive(Debug, Clone)]
struct RouteSpec {
    kind: RouteKind,
    url: Option<String>,
    mode: ProxyMode,
    proxy_url: String,
}

fn current_spec(settings: &NetworkSettings) -> RouteSpec {
    match settings.proxy_mode {
        ProxyMode::System => RouteSpec {
            kind: RouteKind::System,
            url: None,
            mode: ProxyMode::System,
            proxy_url: String::new(),
        },
        ProxyMode::None => RouteSpec {
            kind: RouteKind::Direct,
            url: None,
            mode: ProxyMode::None,
            proxy_url: String::new(),
        },
        ProxyMode::Manual => RouteSpec {
            kind: RouteKind::Proxy,
            url: settings.manual_url().map(str::to_string),
            mode: ProxyMode::Manual,
            proxy_url: settings.proxy_url.trim().to_string(),
        },
    }
}

fn route_specs(settings: &NetworkSettings, candidates: &[u16]) -> Vec<RouteSpec> {
    let current = current_spec(settings);
    let mut specs = vec![current.clone()];
    if current.kind != RouteKind::Direct {
        specs.push(RouteSpec {
            kind: RouteKind::Direct,
            url: None,
            mode: ProxyMode::None,
            proxy_url: String::new(),
        });
    }
    let mut seen = std::collections::HashSet::new();
    for &port in candidates {
        if !seen.insert(port) {
            continue;
        }
        let url = format!("http://127.0.0.1:{port}");
        if current.kind == RouteKind::Proxy && current.url.as_deref() == Some(url.as_str()) {
            continue;
        }
        specs.push(RouteSpec {
            kind: RouteKind::Proxy,
            url: Some(url.clone()),
            mode: ProxyMode::Manual,
            proxy_url: url,
        });
    }
    specs
}

fn probe_one(fetch: &dyn ProbeFetch, url: &str, spec: &RouteSpec) -> RouteProbe {
    let started = Instant::now();
    match fetch.fetch(url, spec.mode, &spec.proxy_url) {
        Ok(_status) => RouteProbe {
            kind: spec.kind,
            url: spec.url.clone(),
            ok: true,
            ms: started.elapsed().as_millis() as u64,
            error: None,
        },
        Err(error) => RouteProbe {
            kind: spec.kind,
            url: spec.url.clone(),
            ok: false,
            ms: started.elapsed().as_millis() as u64,
            error: Some(error),
        },
    }
}

/// Test the saved route, a direct connection and every candidate proxy.
pub fn diagnose(
    fetch: &dyn ProbeFetch,
    settings: &NetworkSettings,
    candidates: &[u16],
) -> NetworkDiagnosis {
    diagnose_with(fetch, settings, candidates, &index_url())
}

fn diagnose_with(
    fetch: &dyn ProbeFetch,
    settings: &NetworkSettings,
    candidates: &[u16],
    url: &str,
) -> NetworkDiagnosis {
    let specs = route_specs(settings, candidates);
    let mut probes: Vec<RouteProbe> = thread::scope(|scope| {
        let handles: Vec<_> = specs
            .iter()
            .cloned()
            .map(|spec| {
                let url = url.to_string();
                scope.spawn(move || probe_one(fetch, &url, &spec))
            })
            .collect();
        specs
            .iter()
            .zip(handles)
            .map(|(spec, handle)| match handle.join() {
                Ok(probe) => probe,
                Err(_) => RouteProbe {
                    kind: spec.kind,
                    url: spec.url.clone(),
                    ok: false,
                    ms: 0,
                    error: Some("probe thread failed".to_string()),
                },
            })
            .collect()
    });
    let current = probes.first().cloned().unwrap_or_else(|| RouteProbe {
        kind: RouteKind::System,
        url: None,
        ok: false,
        ms: 0,
        error: Some("no route was tested".to_string()),
    });
    probes.sort_by(|left, right| {
        (
            !left.ok,
            left.ms,
            route_order(left.kind),
            left.url.as_deref(),
        )
            .cmp(&(
                !right.ok,
                right.ms,
                route_order(right.kind),
                right.url.as_deref(),
            ))
    });
    NetworkDiagnosis {
        current,
        routes: probes,
    }
}

fn route_order(kind: RouteKind) -> u8 {
    match kind {
        RouteKind::System => 0,
        RouteKind::Direct => 1,
        RouteKind::Proxy => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(answer: &'static [u8]) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let port = listener.local_addr().expect("address").port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(answer);
            }
        });
        port
    }

    #[test]
    fn a_connect_200_answer_is_a_candidate() {
        let port = serve_once(b"HTTP/1.1 200 Connection established\r\n\r\n");
        assert!(probe_connect(port, "verithia.github.io:443"));
    }

    #[test]
    fn a_plain_http_404_answer_is_not_a_candidate() {
        let port = serve_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        assert!(!probe_connect(port, "verithia.github.io:443"));
    }

    #[test]
    fn a_plain_http_400_answer_is_not_a_candidate() {
        let port = serve_once(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        assert!(!probe_connect(port, "verithia.github.io:443"));
    }

    #[test]
    fn a_non_http_banner_is_not_a_candidate() {
        let port = serve_once(b"SSH-2.0-OpenSSH_9.0\r\n");
        assert!(!probe_connect(port, "verithia.github.io:443"));
    }

    #[test]
    fn a_closed_port_is_not_a_candidate() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        assert!(!probe_connect(port, "verithia.github.io:443"));
    }

    #[test]
    fn the_port_list_covers_the_usual_proxy_block() {
        for port in 7890..=7897 {
            assert!(LOCAL_PROXY_PORTS.contains(&port), "missing port {port}");
        }
        assert!(LOCAL_PROXY_PORTS.contains(&1080));
        assert!(LOCAL_PROXY_PORTS.contains(&8080));
    }

    struct FakeFetch;

    impl ProbeFetch for FakeFetch {
        fn fetch(&self, _url: &str, mode: ProxyMode, proxy_url: &str) -> Result<u16, String> {
            match (mode, proxy_url) {
                (ProxyMode::System, _) => Err("tls handshake eof".to_string()),
                (ProxyMode::None, _) => Ok(200),
                (ProxyMode::Manual, "http://127.0.0.1:7891") => Ok(200),
                (ProxyMode::Manual, _) => Err("HTTP 502".to_string()),
            }
        }
    }

    #[test]
    fn diagnosis_reports_the_current_error_and_ranks_working_routes() {
        let settings = NetworkSettings::default();
        let diagnosis = diagnose_with(
            &FakeFetch,
            &settings,
            &[7892, 7891],
            "https://example.test/",
        );
        assert!(!diagnosis.current.ok);
        assert_eq!(diagnosis.current.kind, RouteKind::System);
        assert_eq!(
            diagnosis.current.error.as_deref(),
            Some("tls handshake eof")
        );
        let working = diagnosis.routes.iter().filter(|route| route.ok).count();
        assert_eq!(working, 2);
        let first_ok = diagnosis.routes.iter().position(|route| route.ok).unwrap();
        let last_ok = diagnosis.routes.iter().rposition(|route| route.ok).unwrap();
        let first_failed = diagnosis.routes.iter().position(|route| !route.ok).unwrap();
        assert!(last_ok < first_failed);
        assert_eq!(first_ok, 0);
        let proxy = diagnosis
            .routes
            .iter()
            .find(|route| route.kind == RouteKind::Proxy && route.ok)
            .expect("a working proxy");
        assert_eq!(proxy.url.as_deref(), Some("http://127.0.0.1:7891"));
    }

    #[test]
    fn a_route_error_keeps_its_root_cause() {
        #[derive(Debug)]
        struct Layer(&'static str, Option<Box<Layer>>);
        impl std::fmt::Display for Layer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|layer| layer as _)
            }
        }
        let error = Layer(
            "error sending request",
            Some(Box::new(Layer(
                "client error (Connect)",
                Some(Box::new(Layer("tls handshake eof", None))),
            ))),
        );
        assert_eq!(
            error_chain(&error),
            "error sending request: client error (Connect): tls handshake eof"
        );
    }

    #[test]
    fn a_failing_current_proxy_is_not_repeated_as_a_candidate() {
        let settings = NetworkSettings {
            proxy_mode: ProxyMode::Manual,
            proxy_url: "http://127.0.0.1:7892".to_string(),
        };
        let diagnosis = diagnose_with(&FakeFetch, &settings, &[7892], "https://example.test/");
        let matching = diagnosis
            .routes
            .iter()
            .filter(|route| route.url.as_deref() == Some("http://127.0.0.1:7892"))
            .count();
        assert_eq!(matching, 1);
        assert_eq!(diagnosis.current.kind, RouteKind::Proxy);
    }
}
