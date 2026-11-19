//! Plugin folders beyond the per-format standard locations.
//!
//! Plugins often live somewhere other than the folders each format's
//! specification names: a distribution installs into `lib64`, an installer
//! writes its VST2 folder into the registry, a user points another host at a
//! private folder, or the environment names extra folders. This module lists
//! those, so the scan finds what the machine's other audio software already
//! sees.
//!
//! Only folders that exist are returned. Settings files from other hosts are
//! untrusted input: they are read with a size cap and contribute at most
//! [`MAX_HOST_FOLDERS`] absolute paths.

use std::path::{Path, PathBuf};

/// A settings file larger than this is not read.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
/// Folders taken from other hosts' settings, at most.
pub const MAX_HOST_FOLDERS: usize = 64;

/// Why a folder is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownSource {
    /// A conventional location outside the format's own standard list.
    Convention,
    /// Named by an environment variable a host is expected to honour.
    Environment,
    /// Configured in another audio host's settings.
    HostSettings,
}

/// One extra folder and why it is searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFolder {
    pub path: PathBuf,
    pub source: KnownSource,
}

/// Every extra plugin folder that exists on this machine, without duplicates.
pub fn plugin_folders() -> Vec<KnownFolder> {
    let mut folders = Vec::new();
    let mut push = |path: PathBuf, source: KnownSource| {
        if path.is_absolute()
            && path.is_dir()
            && !folders.iter().any(|known: &KnownFolder| known.path == path)
        {
            folders.push(KnownFolder { path, source });
        }
    };
    for path in environment_folders() {
        push(path, KnownSource::Environment);
    }
    for path in convention_folders() {
        push(path, KnownSource::Convention);
    }
    for path in host_settings_folders() {
        push(path, KnownSource::HostSettings);
    }
    folders
}

/// Extra LV2 folders, for the LV2 loader's own search list.
pub fn lv2_folders() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // The LV2 filesystem layout names the roaming profile for user bundles.
        std::env::var_os("APPDATA")
            .map(|roaming| PathBuf::from(roaming).join("LV2"))
            .into_iter()
            .collect()
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/lib64/lv2"),
            PathBuf::from("/usr/local/lib64/lv2"),
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// Folders named by the variables the CLAP, VST3, VST2 and LV2 conventions
/// tell hosts to read. CLAP makes `CLAP_PATH` mandatory for hosts.
fn environment_folders() -> Vec<PathBuf> {
    [
        "CLAP_PATH",
        "VST3_PATH",
        "VST_PATH",
        "LXVST_PATH",
        "LV2_PATH",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
    .collect()
}

fn convention_folders() -> Vec<PathBuf> {
    let mut paths = lv2_folders();
    #[cfg(target_os = "macos")]
    {
        // Network-wide VST3 folder from the VST3 location rules.
        paths.push(PathBuf::from("/Network/Library/Audio/Plug-ins/VST3"));
        // The LV2 layout's default search path on macOS includes these.
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".lv2"));
        }
        paths.push(PathBuf::from("/usr/local/lib/lv2"));
        paths.push(PathBuf::from("/opt/homebrew/lib/lv2"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(programs) = std::env::var_os("ProgramFiles") {
            let programs = PathBuf::from(programs);
            // Some hosts list a VST3 folder directly under Program Files.
            paths.push(programs.join("VST3"));
            paths.push(programs.join("Cakewalk").join("Vstplugins"));
        }
        if let Some(documents) = windows_documents() {
            paths.push(documents.join("Plugins").join("VST"));
        }
        paths.extend(registry::vst2_folders());
    }
    #[cfg(target_os = "linux")]
    {
        // Distributions that keep 64-bit libraries in `lib64`.
        for root in ["/usr/lib64", "/usr/local/lib64"] {
            for format in ["vst3", "clap", "vst", "lxvst", "linux_vst"] {
                paths.push(Path::new(root).join(format));
            }
        }
        paths.push(PathBuf::from("/usr/lib/linux_vst"));
        paths.push(PathBuf::from("/usr/local/lib/linux_vst"));
    }
    paths
}

#[cfg(target_os = "windows")]
fn windows_documents() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join("Documents"))
}

/// Folders other hosts were told to search, read from their settings files.
fn host_settings_folders() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(resource) = reaper_resource_dir() {
        // The resource path's own plugin folder is searched by that host by
        // default, so plugins are often installed there.
        paths.push(resource.join("UserPlugins").join("FX"));
        if let Some(text) = read_bounded(&resource.join("reaper.ini")) {
            paths.extend(reaper_plugin_paths(&text));
        }
    }
    paths.retain(|path| !is_32_bit_location(path));
    paths.truncate(MAX_HOST_FOLDERS);
    paths
}

/// The settings folder of a host whose plugin paths live in an INI file.
fn reaper_resource_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|roaming| PathBuf::from(roaming).join("REAPER"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/REAPER"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|config| config.join("REAPER"))
    }
}

fn read_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The plugin folders in an INI settings file: every `vstpath*` and
/// `clap_path*` value, split on `;`. Only absolute paths are kept, because a
/// relative one would resolve against this app's working directory.
fn reaper_plugin_paths(text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if !(key.starts_with("vstpath") || key.starts_with("clap_path")) {
            continue;
        }
        for entry in value.split(';') {
            let path = PathBuf::from(entry.trim());
            if path.is_absolute() && !paths.contains(&path) {
                paths.push(path);
                if paths.len() == MAX_HOST_FOLDERS {
                    return paths;
                }
            }
        }
    }
    paths
}

/// Whether a folder holds 32-bit plugins, which this 64-bit host cannot load.
fn is_32_bit_location(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("ProgramFiles(x86)")
            .is_some_and(|x86| path.starts_with(PathBuf::from(x86)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "windows")]
mod registry {
    //! The VST2 folder installers record under `Software\VST`.

    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
        RegGetValueW,
    };

    /// Longest value read, in UTF-16 units; a folder path is far shorter.
    const MAX_VALUE_UNITS: usize = 4096;

    pub fn vst2_folders() -> Vec<PathBuf> {
        [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
            .into_iter()
            .filter_map(|root| read_string(root, "Software\\VST", "VSTPluginsPath"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn read_string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let key = wide(key);
        let value = wide(value);
        let mut buffer = vec![0u16; MAX_VALUE_UNITS];
        let mut bytes = (buffer.len() * std::mem::size_of::<u16>()) as u32;
        // SAFETY: `key` and `value` are NUL-terminated UTF-16 strings that
        // outlive the call; `buffer` is writable for `bytes` bytes, and the
        // function writes at most that many and stores the length it wrote.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let units = (bytes as usize / std::mem::size_of::<u16>()).min(buffer.len());
        let text = &buffer[..units];
        let end = text
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(text.len());
        Some(
            OsString::from_wide(&text[..end])
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn host_settings_contribute_absolute_plugin_paths_only() {
        let text = "[REAPER]\n\
                    vstpath64=/opt/vst;relative/vst; /opt/vst ;/srv/clap\n\
                    clap_path_linux-x86_64=/srv/clap;/srv/more\n\
                    lastproject=/home/user/song.rpp\n\
                    broken line\n";
        assert_eq!(
            reaper_plugin_paths(text),
            vec![
                PathBuf::from("/opt/vst"),
                PathBuf::from("/srv/clap"),
                PathBuf::from("/srv/more"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_settings_paths_are_capped() {
        let value: Vec<String> = (0..MAX_HOST_FOLDERS + 10)
            .map(|index| format!("/p/{index}"))
            .collect();
        let text = format!("vstpath={}", value.join(";"));
        assert_eq!(reaper_plugin_paths(&text).len(), MAX_HOST_FOLDERS);
    }

    #[test]
    fn listed_folders_exist_and_are_absolute() {
        for folder in plugin_folders() {
            assert!(folder.path.is_absolute(), "{}", folder.path.display());
            assert!(folder.path.is_dir(), "{}", folder.path.display());
        }
    }
}
