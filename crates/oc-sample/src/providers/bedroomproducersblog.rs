//! Bedroom Producers Blog: a free-pack blog with no per-sample file.
//!
//! A page request answers HTTP 403 to a plain client. The site's WordPress API
//! does answer, and it lists the free packs, but each pack is one zip file
//! rather than a sample this build can pull into a project, so the provider is
//! recorded as page-only with that reason.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "bedroomproducersblog";
/// Why this provider is browse-only.
pub const PAGE_ONLY_REASON: &str = "page requests answer HTTP 403 to a plain client; the WordPress API lists free packs, but each pack is one zip rather than a single sample";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Bedroom Producers Blog",
    id_prefix: "bedroomproducersblog:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &crate::model::SampleKind::ALL,
    license: "Each free pack carries its own terms, which this build does not fetch; no file is pulled.",
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
        assert!(SPEC.page_only.unwrap().contains("zip"));
    }
}
