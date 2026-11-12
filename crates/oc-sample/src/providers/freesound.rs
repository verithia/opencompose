//! Catalog metadata for the freesound adapter.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "freesound";
/// Prefix used by this provider's sample ids.
pub const ID_PREFIX: &str = "freesound:";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Freesound",
    id_prefix: ID_PREFIX,
    requires_key: true,
    key_env: Some(crate::FREESOUND_KEY_ENV),
    key_account: Some("freesound"),
    kinds: &crate::model::SampleKind::ALL,
    license: "Per sound label: Creative Commons 0, Attribution and Attribution ShareAlike pass; NonCommercial and Sampling+ sounds are refused. The public file is the 128 kbps preview; an OAuth2 connection in the DAW pulls the original.",
    search: true,
    download: true,
    page_only: None,
    download_referer: None,
    default_search: true,
};
