//! Pixabay documents image and video search. Audio is a website-only entry.

use crate::registry::ProviderSpec;

/// Stable provider id used by search and pull.
pub const PROVIDER_ID: &str = "pixabay";
/// Why this provider is not wired.
pub const NOT_WIRED_REASON: &str =
    "no documented public audio API; audio search is not wired in this build";

/// Capabilities and license metadata for this provider.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: PROVIDER_ID,
    label: "Pixabay Audio",
    id_prefix: "pixabay:",
    requires_key: false,
    key_env: None,
    key_account: None,
    kinds: &[
        crate::model::SampleKind::DrumOneShot,
        crate::model::SampleKind::Loop,
        crate::model::SampleKind::Vocal,
        crate::model::SampleKind::Fx,
    ],
    license: "Pixabay Content License; it is not a Creative Commons deed, so the licence rule would refuse it until it is represented.",
    search: false,
    download: false,
    page_only: Some(NOT_WIRED_REASON),
    download_referer: None,
    default_search: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_website_only() {
        const { assert!(!SPEC.requires_key) };
        const { assert!(!SPEC.search) };
        assert!(SPEC.page_only.unwrap().contains("not wired"));
    }
}
