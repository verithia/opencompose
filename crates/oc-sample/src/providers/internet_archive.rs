//! Catalog metadata for the internet archive adapter.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "internet-archive";
/// Prefix used by this provider's sample ids.
pub const ID_PREFIX: &str = "ia:";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Internet Archive",
    id_prefix: ID_PREFIX,
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &crate::model::SampleKind::ALL,
    license: "Per item from metadata.licenseurl: CC0, public domain, CC BY and CC BY-SA pass; an item without a licence URL is refused.",
    search: true,
    download: true,
    page_only: None,
    download_referer: None,
    default_search: true,
};
