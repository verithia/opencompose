//! Which external markets this build speaks to, what each one can serve, and
//! why one is unavailable.
//!
//! The registry is data, not a trait object: a caller can list every provider
//! and its state without touching the network, and a search result reports the
//! same rows so a skipped provider is never mistaken for an empty one.
//!
//! A provider that has no programmatic search or download is recorded with the
//! reason and no implementation. This build never scrapes a page whose terms
//! forbid programmatic download.

use serde::Serialize;

use crate::model::SampleKind;
use crate::providers::{
    bedroomproducersblog, commons, cymatics, freesound, internet_archive, jamendo, looperman,
    mixkit, openverse, pixabay, samplefocus, sampleswap, soundbible, splice,
};

/// Keyring service for sample-market keys. The account is the provider id.
pub const KEYRING_SERVICE: &str = "org.opencompose.samples";

/// One market, declared once.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// Stable id used in a tool argument and in a receipt.
    pub id: &'static str,
    /// Human name shown in the panel and in results.
    pub label: &'static str,
    /// Prefix every id this provider issues carries, so a pull can infer the
    /// provider from the id alone.
    pub id_prefix: &'static str,
    /// Whether the provider needs an API key.
    pub requires_key: bool,
    /// Environment variable that overrides the saved key.
    pub key_env: Option<&'static str>,
    /// Keyring account inside [`KEYRING_SERVICE`].
    pub key_account: Option<&'static str>,
    /// Kinds the catalogue can plausibly serve.
    pub kinds: &'static [SampleKind],
    /// A sentence about the licences the source publishes.
    pub license: &'static str,
    /// Whether the provider implements [`super::provider::SampleProvider::search`].
    pub search: bool,
    /// Whether the provider can resolve a direct file URL.
    pub download: bool,
    /// Why a page-only market is not wired, recorded instead of scraped.
    pub page_only: Option<&'static str>,
    /// Optional source-page header required by a download host.
    pub download_referer: Option<&'static str>,
    /// Whether a search that names no market includes this one. A market that
    /// is reachable but answers slowly is kept out of the default fan-out and
    /// stays selectable by name.
    pub default_search: bool,
}

/// Every market this build knows about, available or not.
pub const PROVIDERS: &[ProviderSpec] = &[
    commons::SPEC,
    openverse::SPEC,
    internet_archive::SPEC,
    freesound::SPEC,
    jamendo::SPEC,
    looperman::SPEC,
    splice::SPEC,
    sampleswap::SPEC,
    soundbible::SPEC,
    mixkit::SPEC,
    pixabay::SPEC,
    samplefocus::SPEC,
    bedroomproducersblog::SPEC,
    cymatics::SPEC,
];

/// Keys a caller resolved. A missing key is `None`, never an empty string.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderKeys<'a> {
    pub freesound: Option<&'a str>,
    pub jamendo: Option<&'a str>,
    pub pixabay: Option<&'a str>,
}

impl<'a> ProviderKeys<'a> {
    /// The key for one spec, if the caller supplied a non-blank one.
    pub fn key_for(&self, spec: &ProviderSpec) -> Option<&'a str> {
        let key = match spec.id {
            freesound::PROVIDER_ID => self.freesound,
            jamendo::PROVIDER_ID => self.jamendo,
            pixabay::PROVIDER_ID => self.pixabay,
            _ => None,
        };
        key.map(str::trim).filter(|key| !key.is_empty())
    }
}

/// One provider as a search result, a panel chip or a refusal reports it.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAvailability {
    pub id: &'static str,
    pub label: &'static str,
    pub requires_key: bool,
    pub kinds: Vec<&'static str>,
    pub license: &'static str,
    pub search: bool,
    pub download: bool,
    pub available: bool,
    /// Whether a search that names no market includes this provider.
    pub default_search: bool,
    /// `None` when the provider is available; otherwise what stands in the way.
    pub reason: Option<String>,
}

/// The spec for an id, an alias or a label. Aliases are the short forms a user
/// types on a command line.
pub fn spec(id: &str) -> Option<&'static ProviderSpec> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let alias = match trimmed.to_ascii_lowercase().as_str() {
        "commons" | "wikimedia" => Some(commons::PROVIDER_ID),
        "ia" | "archive" | "internetarchive" => Some(internet_archive::PROVIDER_ID),
        "pixabay-audio" => Some(pixabay::PROVIDER_ID),
        "sample-swap" => Some(sampleswap::PROVIDER_ID),
        "splice-sounds" => Some(splice::PROVIDER_ID),
        _ => None,
    };
    let wanted = alias.unwrap_or(trimmed);
    PROVIDERS.iter().find(|spec| {
        spec.id.eq_ignore_ascii_case(wanted) || spec.label.eq_ignore_ascii_case(wanted)
    })
}

/// The provider an id came from, when the prefix matches one.
pub fn provider_of_id(id: &str) -> Option<&'static ProviderSpec> {
    PROVIDERS
        .iter()
        .filter(|spec| !spec.id_prefix.is_empty() && id.starts_with(spec.id_prefix))
        .max_by_key(|spec| spec.id_prefix.len())
}

/// Why a provider cannot serve a search, or `None` when it can.
pub fn unavailable_reason(spec: &ProviderSpec, keys: ProviderKeys) -> Option<String> {
    unavailable_reason_with(
        spec,
        keys,
        &crate::runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Why this provider cannot answer, when it cannot.
pub fn unavailable_reason_with(
    spec: &ProviderSpec,
    keys: ProviderKeys,
    runtime: &dyn crate::runtime::ProviderRuntime,
) -> Option<String> {
    if let Some(reason) = spec.page_only {
        return Some(reason.to_string());
    }
    if !spec.search {
        return Some("this build has no programmatic search for it".to_string());
    }
    if !runtime.is_installed(spec) {
        return Some(crate::local_plugin::UNAVAILABLE.into());
    }
    if spec.requires_key && keys.key_for(spec).is_none() {
        return Some(format!(
            "needs an API key: set {} (or the {KEYRING_SERVICE}/{} keyring item)",
            spec.key_env.unwrap_or("the provider key"),
            spec.key_account.unwrap_or(spec.id)
        ));
    }
    None
}

/// One row, with availability resolved against the keys at hand.
pub fn availability_of(spec: &ProviderSpec, keys: ProviderKeys) -> ProviderAvailability {
    availability_of_with(
        spec,
        keys,
        &crate::runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Describe one provider's availability for the given keys and runtime.
pub fn availability_of_with(
    spec: &ProviderSpec,
    keys: ProviderKeys,
    runtime: &dyn crate::runtime::ProviderRuntime,
) -> ProviderAvailability {
    let reason = unavailable_reason_with(spec, keys, runtime);
    ProviderAvailability {
        id: spec.id,
        label: spec.label,
        requires_key: spec.requires_key,
        kinds: spec.kinds.iter().map(|kind| kind.as_str()).collect(),
        license: spec.license,
        search: spec.search,
        download: spec.download,
        available: reason.is_none(),
        default_search: spec.default_search,
        reason,
    }
}

/// Every provider, in registry order.
pub fn availability(keys: ProviderKeys) -> Vec<ProviderAvailability> {
    availability_with(
        keys,
        &crate::runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Describe every known provider's availability.
pub fn availability_with(
    keys: ProviderKeys,
    runtime: &dyn crate::runtime::ProviderRuntime,
) -> Vec<ProviderAvailability> {
    PROVIDERS
        .iter()
        .map(|spec| availability_of_with(spec, keys, runtime))
        .collect()
}

/// Ids a search can use right now. A keyed provider without its key is left
/// out, but it still appears in the availability map.
pub fn available_ids(keys: ProviderKeys) -> Vec<&'static str> {
    available_ids_with(
        keys,
        &crate::runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Ids of the providers that can answer right now.
pub fn available_ids_with(
    keys: ProviderKeys,
    runtime: &dyn crate::runtime::ProviderRuntime,
) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|spec| unavailable_reason_with(spec, keys, runtime).is_none())
        .map(|spec| spec.id)
        .collect()
}

/// Markets a search that names none should include: available and in the
/// default fan-out.
pub fn default_search_ids(keys: ProviderKeys) -> Vec<&'static str> {
    default_search_ids_with(
        keys,
        &crate::runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Ids a default search queries, in catalogue order.
pub fn default_search_ids_with(
    keys: ProviderKeys,
    runtime: &dyn crate::runtime::ProviderRuntime,
) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|spec| {
            spec.default_search && unavailable_reason_with(spec, keys, runtime).is_none()
        })
        .map(|spec| spec.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_availability(keys: ProviderKeys) -> Vec<ProviderAvailability> {
        availability_with(keys, &crate::runtime::FixtureBuiltinRuntime)
    }
    fn fixture_available_ids(keys: ProviderKeys) -> Vec<&'static str> {
        available_ids_with(keys, &crate::runtime::FixtureBuiltinRuntime)
    }

    #[test]
    fn every_known_market_is_reported_with_its_terms() {
        let rows = fixture_availability(ProviderKeys::default());
        let ids: Vec<&str> = rows.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![
                "wikimedia-commons",
                "openverse",
                "internet-archive",
                "freesound",
                "jamendo",
                "looperman",
                "splice",
                "sampleswap",
                "soundbible",
                "mixkit",
                "pixabay",
                "samplefocus",
                "bedroomproducersblog",
                "cymatics",
            ]
        );
        assert!(rows.iter().all(|row| !row.license.is_empty()));
    }

    #[test]
    fn the_keyless_aggregators_are_available_without_a_key() {
        let rows = fixture_availability(ProviderKeys::default());
        for id in ["wikimedia-commons", "openverse", "internet-archive"] {
            let row = rows.iter().find(|row| row.id == id).unwrap();
            assert!(row.available, "{id}: {:?}", row.reason);
            assert!(!row.requires_key);
            assert!(row.reason.is_none());
        }
    }

    #[test]
    fn a_keyed_provider_is_unavailable_until_its_key_arrives() {
        let rows = fixture_availability(ProviderKeys::default());
        let freesound = rows.iter().find(|row| row.id == "freesound").unwrap();
        assert!(!freesound.available);
        assert!(freesound.requires_key);
        assert!(
            freesound
                .reason
                .as_deref()
                .unwrap()
                .contains("FREESOUND_API_KEY"),
            "{:?}",
            freesound.reason
        );

        let keys = ProviderKeys {
            freesound: Some("abc"),
            ..Default::default()
        };
        let rows = fixture_availability(keys);
        assert!(
            rows.iter()
                .find(|row| row.id == "freesound")
                .unwrap()
                .available
        );
        // A blank key is the same as a missing one.
        let blank = ProviderKeys {
            freesound: Some("  "),
            ..Default::default()
        };
        assert!(!fixture_available_ids(blank).contains(&"freesound"));
    }

    #[test]
    fn a_page_only_market_is_recorded_with_its_reason_not_scraped() {
        let rows = fixture_availability(ProviderKeys::default());
        for (id, needle) in [
            ("looperman", "no documented public search API"),
            ("sampleswap", "no documented public search API"),
            ("splice", "no documented public catalog API"),
            ("soundbible", "no public search API"),
            ("mixkit", "JavaScript"),
            ("pixabay", "not wired"),
            ("samplefocus", "Cloudflare"),
            ("bedroomproducersblog", "zip"),
            ("cymatics", "email gate"),
        ] {
            let row = rows.iter().find(|row| row.id == id).unwrap();
            assert!(!row.available, "{id} must not claim availability");
            assert!(row.reason.as_deref().unwrap().contains(needle), "{id}");
        }
    }

    #[test]
    fn aliases_and_labels_resolve_to_one_spec() {
        assert_eq!(spec("commons").unwrap().id, "wikimedia-commons");
        assert_eq!(spec("Wikimedia Commons").unwrap().id, "wikimedia-commons");
        assert_eq!(spec("ia").unwrap().id, "internet-archive");
        assert_eq!(spec("Internet Archive").unwrap().id, "internet-archive");
        assert_eq!(spec("  openverse ").unwrap().id, "openverse");
        assert_eq!(spec("sample-swap").unwrap().id, "sampleswap");
        assert_eq!(spec("SampleSwap").unwrap().id, "sampleswap");
        assert_eq!(spec("splice-sounds").unwrap().id, "splice");
        assert_eq!(spec("Splice").unwrap().id, "splice");
        assert!(spec("loopcloud").is_none());
        assert!(spec("").is_none());
    }

    #[test]
    fn an_id_names_its_provider_by_prefix() {
        assert_eq!(
            provider_of_id("commons:File:Snare.ogg").unwrap().id,
            "wikimedia-commons"
        );
        assert_eq!(provider_of_id("openverse:9f0c").unwrap().id, "openverse");
        assert_eq!(
            provider_of_id("ia:some-item").unwrap().id,
            "internet-archive"
        );
        assert_eq!(provider_of_id("jamendo:123").unwrap().id, "jamendo");
        assert!(provider_of_id("loopcloud:1").is_none());
    }
}
