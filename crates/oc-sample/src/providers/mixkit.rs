//! Mixkit: free sound effects and music with direct file URLs but no API.
//!
//! The catalogue is browsable and its audio files sit on a CDN, yet the site
//! exposes no search endpoint and the download control is driven by JavaScript
//! on the page. This build records Mixkit as page-only instead of scraping a
//! flow that changes shape with the page script.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "mixkit";
/// Why this provider is browse-only.
pub const PAGE_ONLY_REASON: &str =
    "no search API; the catalogue is browsed in the page and the download flow is JavaScript-only";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Mixkit",
    id_prefix: "mixkit:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &[crate::model::SampleKind::Fx, crate::model::SampleKind::Loop],
    license: "Free for commercial use under the Mixkit licence, read from each item's page, which this build does not fetch.",
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
        assert!(SPEC.page_only.unwrap().contains("JavaScript-only"));
    }
}
