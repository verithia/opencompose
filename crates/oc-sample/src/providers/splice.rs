//! Availability metadata for Splice.

use crate::model::SampleKind;
use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "splice";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Splice",
    id_prefix: "splice:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &SampleKind::ALL,
    license: "Licensed downloads are available through Splice.",
    search: false,
    download: false,
    page_only: Some("no documented public catalog API for third-party search"),
    download_referer: None,
    default_search: false,
};
