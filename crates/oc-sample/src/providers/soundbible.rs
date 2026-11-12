//! SoundBible: public-domain sound effects with no programmatic interface.
//!
//! The site publishes a browsable catalogue of public-domain effects, but it
//! exposes no search API and its terms do not allow a client to walk and
//! download pages. This build therefore records SoundBible as page-only: the
//! provider appears in the registry and in every availability map with the
//! reason, and a search never invents an empty result for it.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "soundbible";
/// Why this provider is browse-only.
pub const PAGE_ONLY_REASON: &str = "no public search API; the site is browsed page by page and its terms do not allow programmatic download";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "SoundBible",
    id_prefix: "soundbible:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &[crate::model::SampleKind::Fx],
    license: "Public-domain sound effects; the licence is read from each effect's page, which this build does not fetch.",
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
        assert!(SPEC.page_only.unwrap().contains("no public search API"));
    }
}
