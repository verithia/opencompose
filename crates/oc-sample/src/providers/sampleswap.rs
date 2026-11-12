//! Availability metadata for SampleSwap.

use crate::model::SampleKind;
use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "sampleswap";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "SampleSwap",
    id_prefix: "sampleswap:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &SampleKind::ALL,
    license: "Community terms apply to files obtained from the site.",
    search: false,
    download: false,
    page_only: Some("website search only; no documented public search API"),
    download_referer: None,
    default_search: false,
};
