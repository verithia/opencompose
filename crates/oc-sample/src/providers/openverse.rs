//! Catalog metadata for the openverse adapter.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "openverse";
/// Prefix used by this provider's sample ids.
pub const ID_PREFIX: &str = "openverse:";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Openverse",
    id_prefix: ID_PREFIX,
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &crate::model::SampleKind::ALL,
    license: "Aggregated per result: CC0, public domain, CC BY, CC BY-SA pass; non-commercial, no-derivatives and unknown licences are refused.",
    search: true,
    download: true,
    page_only: None,
    download_referer: None,
    default_search: false,
};
