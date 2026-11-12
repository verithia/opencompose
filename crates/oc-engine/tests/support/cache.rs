//! Shared production cache setup without mutating the process environment.
use std::sync::Once;

pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let root = oc_audio::private_fs::test_root();
        let media = oc_engine::media_cache::MediaCache::from_env();
        let derived = oc_engine::derived_cache::DerivedCache::from_env();
        // Other modules may have initialized the same process cache first. All
        // default test paths resolve through the same guarded private_fs root.
        for dir in [media.dir(), derived.dir()] {
            assert!(dir.is_none_or(|dir| dir.starts_with(&root)),
                "integration cache overrides must stay inside the guarded test root; unset external OCP_*_CACHE_DIR overrides");
        }
        oc_engine::media_cache::init_shared_cache(media).expect("matching process media cache");
        oc_engine::derived_cache::init_shared_cache(derived).expect("matching process derived cache");
    });
}
