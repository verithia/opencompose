//! The bound on generated vocal files: the `.ocn` analyses and the `bake/`
//! windows the tune and harmony renders write beside the clip caches.
//!
//! Every file here is derived and rebuilt on a miss, so the bound evicts the
//! least recently written first. Writers call [`trim`] after a write; the
//! Storage page reports [`usage`] and clears with [`clear`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bytes the analyses and bakes may hold together.
pub const DEFAULT_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
const ANALYSIS_EXTENSION: &str = "ocn";
const BAKE_DIR: &str = "bake";

/// Files and bytes the vocal cache holds.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct VocalCacheUsage {
    pub dir: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub budget_bytes: u64,
}

fn files(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut found = Vec::new();
    let entry_of = |path: PathBuf| {
        let meta = fs::metadata(&path).ok()?;
        meta.is_file().then(|| {
            (
                path,
                meta.len(),
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            )
        })
    };
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some(ANALYSIS_EXTENSION) {
            found.extend(entry_of(path));
        }
    }
    // `bake/<identity>/<clip>.wav`: two levels, as the writers lay it out.
    for identity in fs::read_dir(dir.join(BAKE_DIR))
        .into_iter()
        .flatten()
        .flatten()
    {
        for file in fs::read_dir(identity.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            found.extend(entry_of(file.path()));
        }
    }
    found
}

/// What the vocal cache in `dir` holds.
pub fn usage(dir: Option<&Path>) -> VocalCacheUsage {
    let Some(dir) = dir else {
        return VocalCacheUsage {
            budget_bytes: DEFAULT_BUDGET_BYTES,
            ..VocalCacheUsage::default()
        };
    };
    let found = files(dir);
    VocalCacheUsage {
        dir: Some(dir.display().to_string()),
        files: found.len(),
        bytes: found.iter().map(|(_, bytes, _)| bytes).sum(),
        budget_bytes: DEFAULT_BUDGET_BYTES,
    }
}

/// Remove the oldest files until the cache fits `budget`, keeping `keep`
/// (the file just written). Answers the bytes removed. A file another reader
/// holds open and the platform refuses to remove is skipped.
pub fn trim(dir: &Path, budget: u64, keep: Option<&Path>) -> u64 {
    let mut found = files(dir);
    let mut total: u64 = found.iter().map(|(_, bytes, _)| bytes).sum();
    if total <= budget {
        return 0;
    }
    found.sort_by_key(|(_, _, modified)| *modified);
    let mut removed = 0;
    for (path, bytes, _) in found {
        if total <= budget {
            break;
        }
        if keep == Some(path.as_path()) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total -= bytes;
            removed += bytes;
            if let Some(parent) = path.parent()
                && parent.parent().is_some_and(|bake| bake.ends_with(BAKE_DIR))
            {
                let _ = fs::remove_dir(parent);
            }
        }
    }
    removed
}

/// Remove every analysis and bake in `dir`.
pub fn clear(dir: &Path) {
    for (path, _, _) in files(dir) {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir_all(dir.join(BAKE_DIR));
}

/// Apply the default bound after a write into the shared cache.
pub(crate) fn after_write(written: &Path) {
    if let Some(dir) = crate::vocal_analysis::default_cache_dir() {
        trim(&dir, DEFAULT_BUDGET_BYTES, Some(written));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize, age_s: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; bytes]).unwrap();
        let at = SystemTime::now() - std::time::Duration::from_secs(age_s);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    #[test]
    fn the_oldest_vocal_files_go_first_and_clip_caches_are_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("a.ocn");
        let bake = dir.path().join("bake/id/clip.wav");
        let new = dir.path().join("b.ocn");
        let other = dir.path().join("c.ocm");
        write(&old, 100, 300);
        write(&bake, 100, 200);
        write(&new, 100, 0);
        write(&other, 1000, 400);
        assert_eq!(usage(Some(dir.path())).bytes, 300);
        assert_eq!(trim(dir.path(), 150, Some(&new)), 200);
        assert!(!old.exists() && !bake.exists() && new.exists() && other.exists());
        assert!(
            !dir.path().join("bake/id").exists(),
            "an empty bake folder goes"
        );
        clear(dir.path());
        assert!(!new.exists() && other.exists());
    }
}
