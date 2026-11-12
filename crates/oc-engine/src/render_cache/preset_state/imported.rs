//! Imported processor-state materialization with bounded retained cache storage.

use super::MAX_STATE_BYTES;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

type StateKey = (PathBuf, Option<SystemTime>, u64);
const CACHE_ENTRIES: usize = 64;
const CACHE_BYTES: usize = 128 * 1024 * 1024;

struct ImportedStateCache {
    entries: HashMap<StateKey, Arc<Vec<u8>>>,
    retained_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}
impl ImportedStateCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            retained_bytes: 0,
            max_entries,
            max_bytes,
        }
    }
    fn insert(&mut self, key: StateKey, value: Arc<Vec<u8>>) {
        if value.capacity() > self.max_bytes || self.max_entries == 0 {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.retained_bytes -= old.capacity();
        }
        if self.entries.len() >= self.max_entries
            || self
                .retained_bytes
                .checked_add(value.capacity())
                .is_none_or(|total| total > self.max_bytes)
        {
            self.entries.clear();
            self.retained_bytes = 0;
        }
        self.retained_bytes += value.capacity();
        self.entries.insert(key, value);
    }
}
static CACHE: OnceLock<Mutex<ImportedStateCache>> = OnceLock::new();

pub(super) fn imported_vst3_state(
    path: &str,
    resource_root: Option<&Path>,
) -> Result<Arc<Vec<u8>>, String> {
    let resolved = super::resolve_state_resource_path(path, resource_root);
    let file =
        oc_host::plugin_state_io::StateFile::open(&resolved, MAX_STATE_BYTES).map_err(|error| {
            format!(
                "failed to read preset state file '{path}' (resolved to '{}'): {error}",
                resolved.display()
            )
        })?;
    let modified = file.metadata().modified().ok();
    let key = (resolved.clone(), modified, file.metadata().len());
    let cache =
        CACHE.get_or_init(|| Mutex::new(ImportedStateCache::new(CACHE_ENTRIES, CACHE_BYTES)));
    // No modification timestamp means there is no reliable version identity.
    if modified.is_some()
        && let Some(found) = cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .get(&key)
    {
        return Ok(Arc::clone(found));
    }
    let bytes = file.read().map_err(|error| {
        format!(
            "failed to read preset state file '{path}' (resolved to '{}'): {error}",
            resolved.display()
        )
    })?;
    let state = oc_synth::synth::foreign_preset::ForeignPreset::parse(&bytes)?.to_vst3_state()?;
    if state.len() > MAX_STATE_BYTES {
        return Err(format!(
            "imported processor state exceeds the {MAX_STATE_BYTES}-byte limit"
        ));
    }
    let state = Arc::new(state);
    if modified.is_some() {
        cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, Arc::clone(&state));
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(name: &str) -> StateKey {
        (PathBuf::from(name), None, 0)
    }
    #[test]
    fn imported_state_cache_retention_is_bounded_by_bytes_and_entries() {
        let mut cache = ImportedStateCache::new(2, 8);
        cache.insert(key("one"), Arc::new(vec![0; 5]));
        cache.insert(key("two"), Arc::new(vec![0; 4]));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.retained_bytes, 4);
        cache.insert(key("two"), Arc::new(vec![0; 3]));
        assert_eq!(cache.retained_bytes, 3);
        cache.insert(key("three"), Arc::new(vec![0; 2]));
        assert_eq!(cache.entries.len(), 2);
        cache.insert(key("four"), Arc::new(vec![0; 1]));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.retained_bytes, 1);
        cache.insert(key("oversized"), Arc::new(vec![0; 9]));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.retained_bytes, 1);
        let mut reserved = Vec::with_capacity(9);
        reserved.push(0);
        cache.insert(key("reserved"), Arc::new(reserved));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.retained_bytes, 1);
        assert_eq!(
            cache.retained_bytes,
            cache
                .entries
                .values()
                .map(|state| state.capacity())
                .sum::<usize>()
        );
    }
    #[test]
    fn imported_state_cache_distinguishes_subsecond_file_versions() {
        let mut cache = ImportedStateCache::new(2, 8);
        let first = (
            PathBuf::from("one"),
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(1)),
            1,
        );
        let second = (
            PathBuf::from("one"),
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(2)),
            1,
        );
        cache.insert(first.clone(), Arc::new(vec![1]));
        cache.insert(second.clone(), Arc::new(vec![2]));
        assert_eq!(cache.entries[&first].as_slice(), [1]);
        assert_eq!(cache.entries[&second].as_slice(), [2]);
    }
}
