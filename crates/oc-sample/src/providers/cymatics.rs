//! Cymatics: free sample packs behind an email gate.
//!
//! The free downloads are delivered after an email signup, and the site
//! publishes no per-sample file, so this build records it as page-only rather
//! than pretending the catalogue is empty.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "cymatics";
/// Why this provider is browse-only.
pub const PAGE_ONLY_REASON: &str =
    "free packs are delivered behind an email gate and the site publishes no per-sample file";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Cymatics",
    id_prefix: "cymatics:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &crate::model::SampleKind::ALL,
    license: "Packs are royalty-free under Cymatics' own terms, which this build does not fetch; no file is pulled.",
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
        assert!(SPEC.page_only.unwrap().contains("email gate"));
    }
}
