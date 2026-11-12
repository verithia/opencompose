//! Catalog metadata for the jamendo adapter.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "jamendo";
/// Prefix used by this provider's sample ids.
pub const ID_PREFIX: &str = "jamendo:";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Jamendo",
    id_prefix: ID_PREFIX,
    requires_key: true,
    key_env: Some("JAMENDO_CLIENT_ID"),
    key_account: Some("jamendo"),
    kinds: &[
        crate::model::SampleKind::Loop,
        crate::model::SampleKind::Vocal,
        crate::model::SampleKind::InstrumentPhrase,
        crate::model::SampleKind::Unknown,
    ],
    license: "Per track from license_ccurl: CC0, public domain, CC BY and CC BY-SA pass; non-commercial and no-derivatives tracks are refused.",
    search: true,
    download: true,
    page_only: None,
    download_referer: None,
    default_search: true,
};
