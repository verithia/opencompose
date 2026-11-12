//! Immutable native patch dependencies owned by the user's preset library.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use oc_synth::synth::{PROJECT_ASSET_KEYS, document::PatchDocument, load_project_patch};

use super::SoundPreset;

const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_FILES: usize = 1024;

/// New blobs roll back if the library record fails to publish. The library
/// write lock is held until this guard is dropped.
pub(super) struct Assets {
    root: PathBuf,
    created: Vec<PathBuf>,
    sources: BTreeMap<PathBuf, PathBuf>,
    bytes: usize,
}

impl Assets {
    pub(super) fn new(library: &Path) -> Self {
        Self {
            root: library.join("assets"),
            created: Vec::new(),
            sources: BTreeMap::new(),
            bytes: 0,
        }
    }

    pub(super) fn snapshot(
        &mut self,
        preset: &mut SoundPreset,
        project_dir: Option<&Path>,
    ) -> Result<Option<String>, String> {
        let asset_key = |key: &str| PROJECT_ASSET_KEYS.contains(&key);
        let mut resources: Vec<_> = preset
            .params
            .iter()
            .filter(|(key, _)| asset_key(key))
            .cloned()
            .collect();
        if resources.is_empty() {
            return Ok(None);
        }
        oc_synth::synth::anchor_project_patch_assets(&mut resources, project_dir)?;
        let mut document = PatchDocument {
            version: PatchDocument::PORTABLE_VERSION,
            name: preset.name.trim().to_owned(),
            author: "OpenCompose".into(),
            category: preset.category.label().into(),
            use_policy: "User library snapshot".into(),
            patch: load_project_patch(&resources, None)?,
        };
        for sample in document.sample_paths_mut() {
            let saved = self.copy(sample)?;
            *sample = PathBuf::from(
                saved
                    .file_name()
                    .ok_or("Collected sample has no filename")?,
            );
        }
        let state = self.store(&document.to_json()?, "json")?;
        let state = state
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Library asset filename is not UTF-8")?;
        preset.params.retain(|(key, _)| !asset_key(key));
        Ok(Some(state.into()))
    }

    fn copy(&mut self, path: &Path) -> Result<PathBuf, String> {
        let source = path
            .canonicalize()
            .map_err(|error| format!("Cannot locate patch sample {}: {error}", path.display()))?;
        if let Some(existing) = self.sources.get(&source) {
            return Ok(existing.clone());
        }
        if self.sources.len() >= MAX_FILES || !source.is_file() {
            return Err(format!("Cannot collect patch sample {}", source.display()));
        }
        let bytes = read_bounded(&source, MAX_FILE_BYTES.min(MAX_TOTAL_BYTES - self.bytes))?;
        self.bytes += bytes.len();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| ext.len() <= 8 && ext.bytes().all(|byte| byte.is_ascii_alphanumeric()))
            .unwrap_or("asset");
        let target = self.store(&bytes, &extension.to_ascii_lowercase())?;
        self.sources.insert(source, target.clone());
        Ok(target)
    }

    fn store(&mut self, bytes: &[u8], extension: &str) -> Result<PathBuf, String> {
        let root = std::path::absolute(&self.root).map_err(|error| error.to_string())?;
        if std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err("Preset asset store must be a real directory".into());
        }
        oc_audio::private_fs::ensure_dir(&root)
            .map_err(|error| format!("Cannot create preset asset store: {error}"))?;
        let path = root.join(format!("{:x}.{extension}", Md5::digest(bytes)));
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(format!(
                "Preset asset must be a regular file: {}",
                path.display()
            ));
        }
        if path.exists() {
            if read_bounded(&path, bytes.len())? != bytes {
                return Err(format!(
                    "Preset asset contents do not match {}",
                    path.display()
                ));
            }
        } else {
            oc_audio::private_fs::write_atomic(&path, bytes)
                .map_err(|error| format!("Cannot save preset asset {}: {error}", path.display()))?;
            self.created.push(path.clone());
        }
        Ok(path)
    }

    pub(super) fn retain(&mut self) {
        self.created.clear();
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        for path in self.created.drain(..).rev() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("Cannot read preset asset {}: {error}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!(
            "Preset asset exceeds the collection limit: {}",
            path.display()
        ));
    }
    Ok(bytes)
}
