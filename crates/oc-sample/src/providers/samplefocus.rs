//! Sample Focus: a community sample library behind a browser challenge.
//!
//! The search page and every sample page answer HTTP 403 with a Cloudflare
//! interstitial to a plain client (checked 2026-11-02), so this build records
//! the site as page-only instead of driving a challenge or claiming an empty
//! catalogue. The reason appears in the registry and in every availability
//! map.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "samplefocus";
/// Why this provider is browse-only.
pub const PAGE_ONLY_REASON: &str = "Cloudflare answers a browser challenge to the search page and to every sample page (HTTP 403 to a plain client, checked 2026-11-02)";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Sample Focus",
    id_prefix: "samplefocus:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &crate::model::SampleKind::ALL,
    license: "Per upload, set by the contributor; the site is not wired, so no file is pulled and no licence is inferred.",
    search: false,
    download: false,
    page_only: Some(PAGE_ONLY_REASON),
    download_referer: None,
    default_search: true,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_recorded_as_page_only_with_a_reason() {
        const { assert!(!SPEC.search) };
        const { assert!(!SPEC.download) };
        assert!(SPEC.page_only.unwrap().contains("Cloudflare"));
    }
}
