//! Network preferences: the proxy every outbound client follows, and a probe
//! that proves the choice before the user relies on it.

use serde_json::{Value, json};

use crate::gui_settings::{self, NetworkSettings, ProxyMode};
use crate::network_probe::{self, NetworkDiagnosis, ReqwestProbe};

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

/// Check the saved route, a direct connection and every local proxy candidate
/// against the plugin index, and report what answered.
#[tauri::command]
pub async fn network_diagnose() -> Result<NetworkDiagnosis, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let settings = gui_settings::load().network;
        let candidates = network_probe::local_proxy_candidates();
        network_probe::diagnose(&ReqwestProbe, &settings, &candidates)
    })
    .await
    .map_err(|error| format!("network diagnosis failed: {error}"))
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
