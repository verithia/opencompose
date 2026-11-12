//! VST2 discovery and metadata scanning.
//!
//! Discovery is filesystem-only and cheap. Metadata comes from a real plugin
//! load, which is what the disposable scan helper process is for.

use std::path::{Path, PathBuf};

use super::Vst2Instance;
use crate::{PluginFormat, PluginInfo};

/// Extra directories from `OCP_VST2_PATH` (platform path separator).
pub fn extra_search_paths() -> Vec<PathBuf> {
    let value = std::env::var_os("OCP_VST2_PATH");
    extra_search_paths_from(value.as_deref())
}

fn extra_search_paths_from(value: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    value
        .map(|value| std::env::split_paths(value).collect())
        .unwrap_or_default()
}

/// Standard VST2 directories for this platform plus configured extras.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST"));
    }
    #[cfg(target_os = "windows")]
    {
        for base in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(base) {
                let root = PathBuf::from(root);
                paths.push(root.join("Common Files/VST2"));
                paths.push(root.join("VstPlugins"));
                paths.push(root.join("Steinberg/VstPlugins"));
                paths.push(root.join("Common Files/Steinberg/VST2"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/VST2"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst"));
        }
        for directory in [
            "/usr/lib/vst",
            "/usr/local/lib/vst",
            "/usr/lib/lxvst",
            "/usr/local/lib/lxvst",
        ] {
            paths.push(PathBuf::from(directory));
        }
    }
    paths.extend(extra_search_paths());
    paths
}

/// Every VST2 bundle or library file under the search paths.
pub fn discover_bundles() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for directory in search_paths() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_plugin_path(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Whether `path` has the native VST2 extension for this platform.
pub fn is_plugin_path(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        extension.eq_ignore_ascii_case("vst")
    }
    #[cfg(target_os = "windows")]
    {
        extension.eq_ignore_ascii_case("dll")
    }
    #[cfg(target_os = "linux")]
    {
        extension.eq_ignore_ascii_case("so")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = extension;
        false
    }
}

/// Load `path` and describe the single plugin it contains.
///
/// Returns an error when the bundle cannot be loaded; the scan helper turns
/// that into a failed receipt so the bundle gets quarantined instead of being
/// retried on every scan.
pub fn scan_bundle(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let instance = Vst2Instance::load(path, 44_100, 512)?;
    let name = instance.name().to_string();
    let version = version_string(instance.version());
    Ok(vec![PluginInfo {
        id: format!("vst2:{}", id_slug(&name)),
        name,
        vendor: instance.vendor().to_string(),
        format: PluginFormat::Vst2,
        path: path.to_string_lossy().into_owned(),
        category: instance.category(),
        num_params: instance.param_count().max(0) as u32,
        version,
        architectures: Vec::new(),
        state_abi: None,
    }])
}

fn id_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}

fn version_string(version: i32) -> Option<String> {
    // Vendors encode this integer differently; report it verbatim.
    (version > 0).then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_stable_across_punctuation() {
        assert_eq!(id_slug("Comp 3"), "comp_3");
        assert_eq!(id_slug("  Acme  EQ 4  "), "acme_eq_4");
        assert_eq!(id_slug("A/B::C"), "a_b_c");
    }

    #[test]
    fn version_strings_are_reported_verbatim() {
        assert_eq!(version_string(0), None);
        assert_eq!(version_string(10300).as_deref(), Some("10300"));
        assert_eq!(version_string(300).as_deref(), Some("300"));
    }

    #[test]
    fn extra_paths_parse_the_platform_separator() {
        let expected = vec![PathBuf::from("one"), PathBuf::from("two plugins")];
        let encoded = std::env::join_paths(&expected).unwrap();
        assert_eq!(extra_search_paths_from(Some(&encoded)), expected);
        assert!(extra_search_paths_from(None).is_empty());
    }
}
