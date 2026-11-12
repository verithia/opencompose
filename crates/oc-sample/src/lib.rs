//! Sample marketplace client shared by the GUI tools, the panel and `ocp`.
//!
//! This build is a **client**: it searches other people's catalogues and pulls
//! one file into a project. It hosts no catalogue and maintains none.
//!
//! Three pieces sit behind one seam:
//!
//! * a [`provider::SampleProvider`] searches one external market and resolves
//!   one id into a downloadable [`model::SampleItem`],
//! * [`store::SampleStore`] downloads that item into a directory, verifies it
//!   and writes the licence sidecar and receipt, and
//! * [`license`] decides which licences may enter a project and renders the
//!   credit line that travels with the file.
//!
//! [`registry`] lists every market this build knows about, whether it can be
//! searched, and why one is unavailable. A search result repeats that map, so
//! a skipped provider is never mistaken for an empty catalogue.
//!
//! Network access is confined to [`http::HttpFetch`]; every parser is tested
//! against recorded JSON, so a test never touches the internet.

pub mod http;
pub mod license;
#[doc(hidden)]
pub mod local_plugin;
pub mod model;
pub mod provider;
/// The external markets this build speaks to.
pub mod providers;
pub mod registry;
pub mod runtime;
pub mod store;
mod store_format;

#[cfg(any(test, feature = "local-provider-runner", feature = "test-providers"))]
mod adapters;

use crate::model::SampleError;
use crate::provider::SampleProvider;
use crate::registry::{KEYRING_SERVICE, ProviderKeys, ProviderSpec};

/// Environment override so a headless session can search Freesound.
pub const FREESOUND_KEY_ENV: &str = "FREESOUND_API_KEY";

/// Keyless provider used when the caller does not name one.
pub const DEFAULT_PROVIDER: &str = "wikimedia-commons";

pub use registry::ProviderAvailability;

/// A refusal for an id no market in this build carries.
fn unknown_provider(id: &str) -> SampleError {
    let known: Vec<&str> = registry::PROVIDERS.iter().map(|spec| spec.id).collect();
    SampleError::refused(
        "provider_unknown",
        format!(
            "Unknown sample provider '{id}'. This build knows: {}.",
            known.join(", ")
        ),
    )
}

/// A refusal for a market that exists but cannot be searched or downloaded.
fn unavailable(spec: &ProviderSpec, reason: &str) -> SampleError {
    SampleError::refused(
        "provider_unavailable",
        format!("{} is not available in this build: {reason}.", spec.label),
    )
}

/// The one refusal line for a provider that needs a key and has none.
fn key_required(spec: &ProviderSpec) -> SampleError {
    SampleError::refused(
        "key_required",
        format!(
            "{} needs an API key: set {} (or the {KEYRING_SERVICE}/{} keyring item), then search again.",
            spec.label,
            spec.key_env.unwrap_or("the provider key"),
            spec.key_account.unwrap_or(spec.id)
        ),
    )
}

/// Resolve a provider id, alias or label into a working provider.
///
/// An unknown id is `provider_unknown`; a page-only or unwired market is
/// `provider_unavailable`; a keyed market without its key is `key_required`.
pub fn provider_for(
    id: Option<&str>,
    keys: ProviderKeys<'_>,
) -> Result<Box<dyn SampleProvider>, SampleError> {
    provider_for_with(
        id,
        keys,
        &runtime::LocalRuntime::from_process_configuration(),
    )
}

/// Resolve with the same explicit runtime used by availability reporting.
pub fn provider_for_with(
    id: Option<&str>,
    keys: ProviderKeys<'_>,
    runtime: &dyn runtime::ProviderRuntime,
) -> Result<Box<dyn SampleProvider>, SampleError> {
    let requested = id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(DEFAULT_PROVIDER);
    let spec = registry::spec(requested).ok_or_else(|| unknown_provider(requested))?;
    if !spec.search {
        return Err(unavailable(
            spec,
            spec.page_only
                .unwrap_or("this build has no programmatic search for it"),
        ));
    }
    if !runtime.is_installed(spec) {
        return Err(unavailable(spec, local_plugin::UNAVAILABLE));
    }
    let key = if spec.requires_key {
        Some(keys.key_for(spec).ok_or_else(|| key_required(spec))?)
    } else {
        None
    };
    runtime.open(spec, key)
}

#[cfg(any(test, feature = "local-provider-runner", feature = "test-providers"))]
fn builtin_provider(
    spec: &ProviderSpec,
    key: Option<&str>,
) -> Result<Box<dyn SampleProvider>, SampleError> {
    match spec.id {
        providers::commons::PROVIDER_ID => Ok(Box::new(adapters::commons::CommonsProvider::new())),
        providers::openverse::PROVIDER_ID => {
            Ok(Box::new(adapters::openverse::OpenverseProvider::new()))
        }
        providers::internet_archive::PROVIDER_ID => Ok(Box::new(
            adapters::internet_archive::InternetArchiveProvider::new(),
        )),
        providers::freesound::PROVIDER_ID => {
            let key = key.ok_or_else(|| key_required(spec))?;
            Ok(Box::new(adapters::freesound::FreesoundProvider::new(key)))
        }
        providers::jamendo::PROVIDER_ID => {
            let key = key.ok_or_else(|| key_required(spec))?;
            Ok(Box::new(adapters::jamendo::JamendoProvider::new(key)))
        }
        _ => Err(unavailable(spec, "this build has no implementation for it")),
    }
}

/// The provider an id came from, so a pull can infer it from the prefix.
pub fn provider_of_id(id: &str) -> Option<&'static ProviderSpec> {
    registry::provider_of_id(id)
}

/// Every market as rows for a tool result or a panel chip.
pub fn provider_list(keys: ProviderKeys<'_>) -> Vec<serde_json::Value> {
    provider_list_with(keys, &runtime::LocalRuntime::from_process_configuration())
}

/// Serialize availability for every market using the given keys and runtime.
pub fn provider_list_with(
    keys: ProviderKeys<'_>,
    runtime: &dyn runtime::ProviderRuntime,
) -> Vec<serde_json::Value> {
    registry::availability_with(keys, runtime)
        .into_iter()
        .map(|row| serde_json::to_value(row).unwrap_or(serde_json::Value::Null))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_provider_for(
        id: Option<&str>,
        keys: ProviderKeys<'_>,
    ) -> Result<Box<dyn SampleProvider>, SampleError> {
        provider_for_with(id, keys, &runtime::FixtureBuiltinRuntime)
    }
    fn fixture_provider_list(keys: ProviderKeys<'_>) -> Vec<serde_json::Value> {
        provider_list_with(keys, &runtime::FixtureBuiltinRuntime)
    }

    #[test]
    fn the_keyless_provider_is_the_default() {
        let keys = ProviderKeys::default();
        assert_eq!(
            fixture_provider_for(None, keys).unwrap().id(),
            "wikimedia-commons"
        );
        assert_eq!(
            fixture_provider_for(Some("commons"), keys).unwrap().id(),
            "wikimedia-commons"
        );
        assert_eq!(
            fixture_provider_for(Some("openverse"), keys).unwrap().id(),
            "openverse"
        );
        assert_eq!(
            fixture_provider_for(Some("ia"), keys).unwrap().id(),
            "internet-archive"
        );
    }

    #[test]
    fn freesound_without_a_key_is_refused_in_the_key_vocabulary() {
        let error = fixture_provider_for(Some("freesound"), ProviderKeys::default())
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("key_required"));
        assert!(error.to_string().contains(FREESOUND_KEY_ENV), "{error}");
        assert!(
            error
                .to_string()
                .contains("org.opencompose.samples/freesound"),
            "a keyless session is told the keyring item: {error}"
        );
        // A blank key is the same as a missing one.
        let blank = ProviderKeys {
            freesound: Some("  "),
            ..Default::default()
        };
        assert_eq!(
            fixture_provider_for(Some("freesound"), blank)
                .err()
                .unwrap()
                .code(),
            Some("key_required")
        );
    }

    #[test]
    fn jamendo_is_wired_behind_its_client_id() {
        let error = fixture_provider_for(Some("jamendo"), ProviderKeys::default())
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("key_required"));
        assert!(error.to_string().contains("JAMENDO_CLIENT_ID"), "{error}");
        let keys = ProviderKeys {
            jamendo: Some("client"),
            ..Default::default()
        };
        assert_eq!(
            fixture_provider_for(Some("jamendo"), keys).unwrap().id(),
            "jamendo"
        );
    }

    #[test]
    fn an_unknown_provider_lists_what_this_build_offers() {
        let error = fixture_provider_for(Some("loopcloud"), ProviderKeys::default())
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("provider_unknown"));
        assert!(error.to_string().contains("openverse"), "{error}");
        assert!(error.to_string().contains("internet-archive"), "{error}");
    }

    #[test]
    fn a_page_only_market_is_refused_with_its_recorded_reason() {
        for id in [
            "soundbible",
            "mixkit",
            "samplefocus",
            "bedroomproducersblog",
            "cymatics",
        ] {
            let error = fixture_provider_for(Some(id), ProviderKeys::default())
                .err()
                .unwrap();
            assert_eq!(error.code(), Some("provider_unavailable"), "{id}");
        }
        let error = fixture_provider_for(Some("pixabay"), ProviderKeys::default())
            .err()
            .unwrap();
        assert_eq!(error.code(), Some("provider_unavailable"));
        assert!(error.to_string().contains("not wired"), "{error}");
    }

    #[test]
    fn an_id_prefix_names_a_provider() {
        assert_eq!(
            provider_of_id("commons:File:X.ogg").unwrap().id,
            "wikimedia-commons"
        );
        assert_eq!(provider_of_id("openverse:abc").unwrap().id, "openverse");
        assert_eq!(provider_of_id("ia:item").unwrap().id, "internet-archive");
        assert_eq!(provider_of_id("freesound:1").unwrap().id, "freesound");
        assert_eq!(provider_of_id("jamendo:1").unwrap().id, "jamendo");
        assert_eq!(provider_of_id("splice:abc").unwrap().id, "splice");
        assert_eq!(
            provider_of_id("sampleswap:/samples-ghost/a.mp3")
                .unwrap()
                .id,
            "sampleswap"
        );
        assert!(provider_of_id("loopcloud:1").is_none());
    }

    #[test]
    fn the_provider_list_reports_availability_as_data() {
        let rows = fixture_provider_list(ProviderKeys::default());
        assert_eq!(rows.len(), registry::PROVIDERS.len());
        let openverse = rows
            .iter()
            .find(|row| row["id"] == "openverse")
            .expect("openverse is listed");
        assert_eq!(openverse["available"], true);
        assert!(openverse["reason"].is_null());
        let mixkit = rows.iter().find(|row| row["id"] == "mixkit").unwrap();
        assert_eq!(mixkit["available"], false);
        assert!(mixkit["reason"].as_str().unwrap().contains("JavaScript"));
    }
}
