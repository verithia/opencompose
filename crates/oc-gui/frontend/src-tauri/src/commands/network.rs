//! Network preferences: the proxy every outbound client follows, and a probe
//! that proves the choice before the user relies on it.

use serde_json::{Value, json};

use crate::gui_settings::{self, NetworkSettings, ProxyMode};

/// The saved network settings for the Preferences page.
#[tauri::command]
pub fn network_settings() -> Value {
    let network = gui_settings::load().network;
    json!({
        "proxyMode": match network.proxy_mode {
            ProxyMode::System => "system",
            ProxyMode::Manual => "manual",
            ProxyMode::None => "none",
        },
        "proxyUrl": network.proxy_url,
    })
}

/// Persist the network settings; the sanitizer refuses a manual mode without a
/// usable URL, so a page cannot silently send traffic around the proxy.
#[tauri::command]
pub fn network_save(settings: NetworkSettings) -> Result<Value, String> {
    let mut stored = gui_settings::load();
    stored.network = settings;
    let stored = stored.sanitized();
    gui_settings::save(&stored)?;
    Ok(json!({
        "proxyMode": match stored.network.proxy_mode {
            ProxyMode::System => "system",
            ProxyMode::Manual => "manual",
            ProxyMode::None => "none",
        },
        "proxyUrl": stored.network.proxy_url,
    }))
}

/// Ports a local proxy commonly listens on.
fn local_proxy_ports() -> std::ops::RangeInclusive<u16> {
    7890..=7897
}

/// A local HTTP proxy the app may offer when the page asks.
///
/// The scan runs only when the page asks; it probes the usual ports with a
/// short timeout and answers with the first one that speaks HTTP.
#[tauri::command]
pub async fn detect_local_proxy() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| local_proxy_ports().find_map(probe_local_proxy))
        .await
        .unwrap_or(None)
}

fn probe_local_proxy(port: u16) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(120)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .write_all(
            b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n",
        )
        .ok()?;
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).ok()?;
    head.starts_with(b"HTTP/")
        .then(|| format!("http://127.0.0.1:{port}"))
}

/// Ask one URL through the chosen proxy. `https://api.openverse.org/v1/` is a
/// small page that answers when the path to a sample market works at all.
#[tauri::command]
pub async fn network_test(proxy_mode: String, proxy_url: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || probe(&proxy_mode, &proxy_url))
        .await
        .unwrap_or_else(|error| Err(format!("probe failed: {error}")))
}

fn probe(proxy_mode: &str, proxy_url: &str) -> Result<Value, String> {
    use oc_sample::http::{HttpFetch, ProxyChoice};
    use oc_sample::provider::SampleFetch;

    let mode = match proxy_mode {
        "manual" => ProxyMode::Manual,
        "none" => ProxyMode::None,
        _ => ProxyMode::System,
    };
    let settings = NetworkSettings {
        proxy_mode: mode,
        proxy_url: proxy_url.to_string(),
    };
    let choice = match (mode, settings.manual_url()) {
        (ProxyMode::Manual, Some(url)) => ProxyChoice::Manual(url),
        (ProxyMode::None, _) => ProxyChoice::None,
        _ => ProxyChoice::System,
    };
    let fetch = HttpFetch::with_proxy(std::time::Duration::from_secs(12), choice)
        .map_err(|error| format!("network: {error}"))?;
    let started = std::time::Instant::now();
    match fetch.get("https://api.openverse.org/v1/", 512 * 1024) {
        Ok(response) => Ok(json!({
            "ok": true,
            "status": response.status,
            "ms": started.elapsed().as_millis() as u64,
            "bytes": response.body.len(),
        })),
        Err(error) => Ok(json!({
            "ok": false,
            "ms": started.elapsed().as_millis() as u64,
            "error": error,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_mode_without_a_url_is_not_a_proxy() {
        let settings = NetworkSettings {
            proxy_mode: ProxyMode::Manual,
            proxy_url: "   ".into(),
        };
        assert!(settings.manual_url().is_none());
    }

    #[test]
    fn a_manual_http_proxy_is_accepted() {
        let settings = NetworkSettings {
            proxy_mode: ProxyMode::Manual,
            proxy_url: " http://proxy.example.invalid:8080 ".into(),
        };
        assert_eq!(
            settings.manual_url(),
            Some("http://proxy.example.invalid:8080")
        );
    }

    #[test]
    fn the_system_mode_never_uses_the_manual_field() {
        let settings = NetworkSettings {
            proxy_mode: ProxyMode::System,
            proxy_url: "http://proxy.example.invalid:8080".into(),
        };
        assert!(settings.manual_url().is_none());
    }
}

#[cfg(test)]
mod local_proxy_tests {
    use super::probe_local_proxy;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn a_port_that_answers_http_is_reported_as_a_proxy() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let port = listener.local_addr().expect("address").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            }
        });
        assert_eq!(
            probe_local_proxy(port).as_deref(),
            Some(format!("http://127.0.0.1:{port}").as_str())
        );
    }

    #[test]
    fn a_port_that_does_not_speak_http_is_ignored() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let port = listener.local_addr().expect("address").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.0\r\n");
            }
        });
        assert_eq!(probe_local_proxy(port), None);
    }
}
