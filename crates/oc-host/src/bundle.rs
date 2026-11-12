//! Locating the loadable binary inside a plugin bundle.
//!
//! A bundle is a directory whose layout is fixed by the format specification:
//! the binary lives in a platform directory under `Contents`. The directory
//! name differs per platform and, on Linux and Windows, per architecture.

use std::path::{Path, PathBuf};

/// Candidate binary file names for a bundle with the given stem, in probe order.
fn binary_names(stem: &str) -> Vec<String> {
    let mut names = vec![stem.to_string()];
    for suffix in ["so", "vst3", "dll", "dylib"] {
        names.push(format!("{stem}.{suffix}"));
        names.push(format!("lib{stem}.{suffix}"));
    }
    names
}

/// Bundle platform directories in probe order, host architecture first.
fn binary_dirs() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        vec!["Contents/MacOS".to_string()]
    }

    #[cfg(target_os = "linux")]
    {
        let mut dirs: Vec<String> = Vec::new();
        for arch in [std::env::consts::ARCH, "x86_64", "aarch64", "i386", "arm"] {
            let dir = format!("Contents/{arch}-linux");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    #[cfg(target_os = "windows")]
    {
        let mut dirs: Vec<String> = Vec::new();
        for arch in [std::env::consts::ARCH, "x86_64", "x86", "arm64"] {
            let dir = format!("Contents/{arch}-win");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        vec!["Contents".to_string()]
    }
}

/// Resolve the loadable binary of a bundle, or of a bare library file.
///
/// Returns `None` when `path` is a bundle directory that holds no binary in a
/// layout this platform understands.
pub fn binary_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let names = binary_names(stem);
    for dir in binary_dirs() {
        let dir = path.join(dir);
        for name in &names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::binary_path;

    fn bundle(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let bundle = dir.join(name);
        std::fs::create_dir_all(&bundle).expect("create bundle");
        bundle
    }

    #[test]
    fn bare_file_resolves_to_itself() {
        let dir = std::env::temp_dir().join(format!("oc-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("plugin.so");
        std::fs::write(&file, b"x").expect("write file");
        assert_eq!(binary_path(&file), Some(file));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_bundle_binary_is_none() {
        let dir = std::env::temp_dir().join(format!("oc-bundle-empty-{}", std::process::id()));
        let path = bundle(&dir, "Empty.vst3");
        assert_eq!(binary_path(&path), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn platform_layout_resolves() {
        let dir = std::env::temp_dir().join(format!("oc-bundle-layout-{}", std::process::id()));
        let path = bundle(&dir, "Demo.vst3");
        assert_eq!(binary_path(&path), None);

        #[cfg(target_os = "macos")]
        let binary = path.join("Contents/MacOS/Demo");
        #[cfg(target_os = "linux")]
        let binary = path.join(format!("Contents/{}-linux/Demo.so", std::env::consts::ARCH));
        #[cfg(target_os = "windows")]
        let binary = path.join(format!("Contents/{}-win/Demo.vst3", std::env::consts::ARCH));

        std::fs::create_dir_all(binary.parent().expect("binary dir")).expect("create dir");
        std::fs::write(&binary, b"x").expect("write binary");
        assert_eq!(binary_path(&path), Some(binary));
        std::fs::remove_dir_all(&dir).ok();
    }
}
