//! Memoize exact requests within one published plugin inventory generation.

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use super::{PluginFormat, PluginInfo};

type Key = (String, PluginFormat);

#[derive(Default)]
struct Entries {
    generation: u64,
    plugins: HashMap<Key, PluginInfo>,
}

#[derive(Default)]
pub(super) struct ResolutionCache {
    entries: Mutex<Entries>,
}

impl ResolutionCache {
    fn entries(&self) -> MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn clear(&self) {
        let mut entries = self.entries();
        entries.generation = entries.generation.wrapping_add(1);
        entries.plugins.clear();
    }

    pub(super) fn resolve(
        &self,
        selector: &str,
        format: PluginFormat,
        resolve: impl FnOnce() -> Option<PluginInfo>,
    ) -> Option<PluginInfo> {
        // Lookup precedence depends on the spelling of an exact ID, name or path.
        let key = (selector.trim().to_owned(), format);
        let generation = {
            let entries = self.entries();
            if let Some(info) = entries.plugins.get(&key) {
                return Some(info.clone());
            }
            entries.generation
        };
        // Actual helper admission belongs to the scanner and is keyed by
        // bundle identity, so an unrelated slow lookup cannot hold this cache.
        let info = resolve()?;
        let mut entries = self.entries();
        if entries.generation == generation {
            entries.plugins.entry(key).or_insert_with(|| info.clone());
        }
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::{PluginQuery, resolve_plugin};

    fn descriptor(id: &str, path: &str) -> PluginInfo {
        let mut info = crate::builtin::builtin_plugin_list().remove(0);
        info.id = id.into();
        info.name = id.into();
        info.path = path.into();
        info.format = PluginFormat::Vst3;
        info
    }

    #[test]
    fn differently_cased_exact_queries_keep_their_own_class_and_path() {
        let plugins = [
            descriptor("Example", "/upper/Example.vst3"),
            descriptor("example", "/lower/example.vst3"),
        ];
        let resolve = |selector: &str| {
            resolve_plugin(
                &plugins,
                &PluginQuery::parse(selector).unwrap(),
                PluginFormat::Vst3,
            )
            .cloned()
        };
        let cache = ResolutionCache::default();
        for (selector, expected) in [("Example", &plugins[0]), ("example", &plugins[1])] {
            let found = cache
                .resolve(selector, PluginFormat::Vst3, || resolve(selector))
                .unwrap();
            assert_eq!(found.path, expected.path);
        }
        for plugin in &plugins {
            assert_eq!(
                cache
                    .resolve(&plugin.id, PluginFormat::Vst3, || panic!("cached"))
                    .unwrap()
                    .path,
                plugin.path
            );
        }
    }

    #[test]
    fn refresh_invalidates_early_scans_and_refuses_to_cache_inflight_results() {
        let cache = ResolutionCache::default();
        let old = descriptor("Example", "/old/Example.vst3");
        let new = descriptor("Example", "/new/Example.vst3");
        assert!(
            cache
                .resolve("Example", PluginFormat::Vst3, || {
                    cache.clear();
                    Some(old.clone())
                })
                .is_some()
        );
        assert_eq!(
            cache
                .resolve("Example", PluginFormat::Vst3, || Some(new.clone()))
                .unwrap()
                .path,
            new.path
        );
    }

    #[test]
    fn an_unrelated_resolution_can_finish_while_one_is_waiting() {
        use std::sync::mpsc;
        use std::time::Duration;
        let cache = ResolutionCache::default();
        std::thread::scope(|threads| {
            let (started, started_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let shared = &cache;
            let slow = threads.spawn(move || {
                shared.resolve("Slow", PluginFormat::Vst3, || {
                    started.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    None
                })
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let fast = descriptor("Fast", "/plugins/Fast.vst3");
            assert_eq!(
                cache
                    .resolve("Fast", PluginFormat::Vst3, || Some(fast.clone()))
                    .unwrap()
                    .path,
                fast.path,
            );
            release.send(()).unwrap();
            assert!(slow.join().unwrap().is_none());
        });
    }
}
