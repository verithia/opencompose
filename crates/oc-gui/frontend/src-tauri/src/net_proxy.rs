//! The egress proxy selected in Preferences ▸ Network.
//!
//! Sample fetching has always taken it; AI providers and the plugin index use
//! the same choice, because both are reached through the local proxy.
//! A loopback endpoint stays direct: a local provider is not reachable through
//! the proxy, and routing it there turns a connection refusal into a 5xx.

use crate::gui_settings::ProxyMode;

/// Apply the user's choice to a client builder.
pub fn apply(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let network = crate::gui_settings::load().network;
    match network.proxy_mode {
        ProxyMode::Manual => {
            let url = network.proxy_url.trim();
            match reqwest::Proxy::all(url) {
                Ok(proxy) => builder.proxy(proxy),
                Err(_) => builder,
            }
        }
        ProxyMode::None => builder.no_proxy(),
        ProxyMode::System => builder,
    }
}

/// The same choice for a blocking builder. A command that already runs on the
/// blocking pool does not need a runtime to take the proxy.
pub fn apply_blocking(
    builder: reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::ClientBuilder {
    let network = crate::gui_settings::load().network;
    match network.proxy_mode {
        ProxyMode::Manual => {
            let url = network.proxy_url.trim();
            match reqwest::Proxy::all(url) {
                Ok(proxy) => builder.proxy(proxy),
                Err(_) => builder,
            }
        }
        ProxyMode::None => builder.no_proxy(),
        ProxyMode::System => builder,
    }
}
