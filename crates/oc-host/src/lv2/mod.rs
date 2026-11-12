//! LV2 plugin support: bundle discovery, metadata, instantiation and processing.
//!
//! The host implements the LV2 core ABI directly. It parses `manifest.ttl`
//! itself, loads the bundle binary with the platform loader, and only advertises
//! the URID and bounded-block-length features. Worker, options, log, make-path
//! and UI extensions are deliberately not hosted; a plugin that requires one of
//! them is rejected before instantiation with a named error.

pub mod abi;
mod instance;
pub mod loader;
pub mod metadata;
pub mod turtle;
pub mod urid;

pub use instance::{Lv2Instance, Param};
pub use metadata::{Plugin, Port, PortDirection, PortKind};

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::PluginInfo;

static EXTRA_SEARCH_PATHS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

fn extra_search_paths() -> Vec<PathBuf> {
    EXTRA_SEARCH_PATHS
        .get()
        .map(|paths| {
            paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
        .unwrap_or_default()
}

/// Add a process-local bundle root. Tests and embedders use this instead of
/// mutating the shared `LV2_PATH` environment.
pub fn add_search_path(path: PathBuf) {
    let paths = EXTRA_SEARCH_PATHS.get_or_init(Default::default);
    let mut paths = paths.lock().unwrap_or_else(|error| error.into_inner());
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// Drop test-added LV2 search roots.
pub fn clear_search_paths() {
    if let Some(paths) = EXTRA_SEARCH_PATHS.get() {
        paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// LV2 search roots including test additions.
pub fn search_paths() -> Vec<PathBuf> {
    metadata::search_paths()
}

/// Bundles under the LV2 search paths.
pub fn discover_bundles() -> Vec<PathBuf> {
    metadata::discover_bundles()
}

/// Scan installed bundles; returns the plugins and one message per bad bundle.
pub fn scan_installed() -> (Vec<Plugin>, Vec<String>) {
    metadata::scan_installed()
}

/// Convert a discovered plugin to host info.
pub fn plugin_info(plugin: &Plugin) -> PluginInfo {
    plugin.info()
}

/// Resolve one plugin URI against the search path (including added roots).
pub fn find_plugin(uri: &str) -> Option<Plugin> {
    metadata::find_in_paths(uri, &search_paths())
}

/// Resolve an explicitly registered URI without scanning the system or user
/// plug-in directories.
pub(crate) fn find_registered_plugin(uri: &str) -> Option<Plugin> {
    metadata::find_in_paths(uri, &extra_search_paths())
}

/// Resolve an LV2 plugin by URI, display name, bundle path or symbol.
pub fn resolve_plugin(query: &str) -> Option<Plugin> {
    if let Some(plugin) = find_plugin(query) {
        return Some(plugin);
    }
    let (plugins, _) = scan_installed();
    if let Some(plugin) = plugins.iter().find(|plugin| {
        plugin.uri.eq_ignore_ascii_case(query) || plugin.name.eq_ignore_ascii_case(query)
    }) {
        return Some(plugin.clone());
    }
    let query_key = normalize_key(query);
    plugins
        .into_iter()
        .find(|plugin| {
            normalize_key(&plugin.uri) == query_key || normalize_key(&plugin.name) == query_key
        })
        .or_else(|| {
            let bundle = PathBuf::from(query);
            if bundle.is_dir() {
                metadata::scan_bundle(&bundle).ok().and_then(|plugins| {
                    plugins.into_iter().find(|plugin| {
                        plugin.name.eq_ignore_ascii_case(query)
                            || plugin
                                .bundle_path
                                .to_string_lossy()
                                .eq_ignore_ascii_case(query)
                    })
                })
            } else {
                None
            }
        })
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_kind_helpers_are_consistent() {
        let port = Port {
            index: 0,
            symbol: "gain".into(),
            name: "Gain".into(),
            kind: PortKind::Control,
            direction: PortDirection::Input,
            minimum: 0.0,
            maximum: 1.0,
            default: 0.5,
            designation: None,
            supports_midi: false,
            atom_buffer_type: None,
            unit: None,
            group: None,
            steps: None,
        };
        assert!(port.is_control());
        assert!(port.is_input());
        assert!(!port.is_audio());
    }
}
