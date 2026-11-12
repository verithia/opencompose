//! Bundled model prices. The snapshot next to the catalog carries the published
//! input, output and cached-input price per million tokens for the models the
//! catalog declares, taken from the models.dev dataset by
//! `scripts/refresh-ai-prices.py`.
//!
//! A model the dataset does not price has no record: its price is `None`, never
//! zero, and the panel shows no cost for it. A user price in the model limits
//! dialog wins over the snapshot.
use super::catalog::{self, Catalog};
use super::limits::{self, Price};
use crate::commands::Wire;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;

const PRICES_JSON: &str = include_str!("../../ai-prices.json");

/// The snapshot as published: one record per priced (provider, model) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    pub source: String,
    pub unit: String,
    #[serde(default)]
    pub records: Vec<Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One model price record.
pub struct Record {
    pub provider: String,
    pub model: String,
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<f64>,
    #[serde(default = "default_currency")]
    pub currency: String,
}

impl Record {
    fn price(&self) -> Price {
        Price {
            input: self.input,
            output: self.output,
            cached_input: self.cached_input,
            currency: self.currency.clone(),
        }
    }
}

fn default_currency() -> String {
    limits::DEFAULT_CURRENCY.to_owned()
}

/// The snapshot indexed for lookup. Parsed once: the table is small and static.
#[derive(Debug)]
pub struct Prices {
    by_model: BTreeMap<(String, String), Price>,
}

impl Prices {
    /// The published price for one catalog provider and model, or `None` when
    /// the dataset carries no price for it.
    pub fn bundled(&self, provider: &str, model: &str) -> Option<&Price> {
        self.by_model.get(&(provider.to_owned(), model.to_owned()))
    }

    /// Attaches the bundled price to every catalog model that does not already
    /// carry one, so the catalog is the single place a price is read from.
    pub fn apply(&self, catalog: &mut Catalog) {
        for provider in &mut catalog.providers {
            for model in &mut provider.models {
                if model.price.is_none() {
                    model.price = self.bundled(&provider.id, &model.id).cloned();
                }
            }
        }
    }
}

static PRICES: LazyLock<Prices> =
    LazyLock::new(|| parse(PRICES_JSON).expect("bundled AI price snapshot is valid JSON"));

fn parse(text: &str) -> Result<Prices, serde_json::Error> {
    let snapshot: Snapshot = serde_json::from_str(text)?;
    let by_model = snapshot
        .records
        .iter()
        .map(|record| {
            (
                (record.provider.clone(), record.model.clone()),
                record.price(),
            )
        })
        .collect();
    Ok(Prices { by_model })
}

/// Committed price table.
pub fn prices() -> &'static Prices {
    &PRICES
}

/// The snapshot as committed, for tests that guard the generated file.
#[cfg(test)]
pub fn snapshot() -> &'static Snapshot {
    static SNAPSHOT: LazyLock<Snapshot> = LazyLock::new(|| {
        serde_json::from_str(PRICES_JSON).expect("bundled AI price snapshot is valid JSON")
    });
    &SNAPSHOT
}

/// Price one request is charged at: the user's override for this saved provider
/// first, then the catalog record of the provider itself, then the same model id
/// on the same protocol.
///
/// The last step covers an id the provider does not list (a saved id it
/// renamed, a custom endpoint naming a known model). A model no record prices
/// stays unpriced: a local server has no per-token price. A plan that bills a
/// flat fee still shows the published list price of the model it runs, which the
/// user can replace with their own price.
pub fn resolve(provider_id: &str, wire: Wire, model: &str) -> Option<Price> {
    if let Some(price) = limits::get(provider_id, model).and_then(|limits| limits.price) {
        return Some(price);
    }
    let catalog = catalog::merged();
    let wanted = catalog::resolve_alias(wire, model).unwrap_or_else(|| model.to_owned());
    let own = catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .and_then(|provider| provider.models.iter().find(|entry| entry.id == wanted))
        .and_then(|entry| entry.price.clone());
    own.or_else(|| {
        catalog
            .providers
            .iter()
            .filter(|provider| provider.wire() == wire)
            .flat_map(|provider| provider.models.iter())
            .find(|entry| entry.id == wanted)
            .and_then(|entry| entry.price.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_model(provider: &str, model: &str) -> Option<catalog::CatalogModel> {
        catalog::catalog()
            .providers
            .iter()
            .find(|entry| entry.id == provider)?
            .models
            .iter()
            .find(|entry| entry.id == model)
            .cloned()
    }

    #[test]
    fn the_snapshot_is_self_describing() {
        let snapshot = snapshot();
        assert!(!snapshot.version.is_empty());
        assert_eq!(snapshot.unit, "USD per million tokens");
        assert!(snapshot.source.starts_with("https://"));
        assert!(!snapshot.records.is_empty());
        for record in &snapshot.records {
            assert_eq!(
                record.currency, "USD",
                "{}/{}",
                record.provider, record.model
            );
            assert!(
                record.input >= 0.0 && record.input.is_finite(),
                "{record:?}"
            );
            assert!(
                record.output >= 0.0 && record.output.is_finite(),
                "{record:?}"
            );
        }
    }

    #[test]
    fn every_record_names_a_model_the_catalog_declares() {
        // A typo in the generated snapshot would otherwise price nothing at all.
        for record in &snapshot().records {
            let model = catalog_model(&record.provider, &record.model).unwrap_or_else(|| {
                panic!("{}/{} is not in the catalog", record.provider, record.model)
            });
            assert_eq!(
                model.price.as_ref().map(|price| price.input),
                Some(record.input),
                "{}/{}",
                record.provider,
                record.model
            );
        }
    }

    #[test]
    fn a_priced_model_carries_its_bundled_price_in_the_catalog() {
        let model = catalog_model("openai", "gpt-6.1-sol").unwrap();
        let price = model.price.expect("the snapshot prices this model");
        assert_eq!((price.input, price.output), (2.0, 10.0));
        assert_eq!(price.cached_input, Some(0.1));
        assert_eq!(price.currency, "USD");
    }

    #[test]
    fn a_model_the_dataset_does_not_price_has_no_price_at_all() {
        let model = catalog_model("ollama", "llama3.1").unwrap();
        assert!(model.price.is_none(), "a local server has no token price");
        assert_eq!(prices().bundled("ollama", "llama3.1"), None);
    }

    #[test]
    fn resolving_prefers_the_providers_own_record_then_the_protocol() {
        // A saved id the provider renamed still finds the model on its protocol.
        let renamed = resolve(
            "deepseek-v4-flash",
            Wire::OpenAiChatCompletions,
            "deepseek-flash",
        )
        .expect("the model is priced on this protocol");
        assert_eq!(renamed.input, 0.15);
        // A provider with no price list of its own, such as a saved gateway,
        // falls back to the same model id on the same protocol.
        assert_eq!(
            resolve("claude-gateway", Wire::AnthropicMessages, "claude-opus-4-6")
                .map(|price| price.input),
            Some(5.0)
        );
        // An id nothing prices stays unpriced on every path.
        assert_eq!(
            resolve("openai", Wire::OpenAiResponses, "gpt-9-imaginary"),
            None
        );
        assert_eq!(
            resolve("openai", Wire::OpenAiResponses, "gpt-6.1-sol").map(|price| price.output),
            Some(10.0)
        );
    }

    #[test]
    fn a_user_price_beats_the_bundled_one() {
        let user = Price {
            input: 1.0,
            output: 2.0,
            cached_input: Some(0.5),
            currency: "EUR".into(),
        };
        let provider = "pricing-fixture";
        limits::install_for_test(
            provider,
            "gpt-6.1-sol",
            limits::ModelLimits {
                context: 1_000_000,
                max_output: 64_000,
                input: limits::InputModalities {
                    text: true,
                    ..Default::default()
                },
                price: Some(user.clone()),
            },
        );
        let resolved = resolve(provider, Wire::OpenAiResponses, "gpt-6.1-sol")
            .expect("the user price applies");
        assert_eq!(resolved, user);
        limits::clear_for_test(provider, "gpt-6.1-sol");
        assert_eq!(
            resolve(provider, Wire::OpenAiResponses, "gpt-6.1-sol").map(|price| price.output),
            Some(10.0),
            "clearing the layer returns the bundled price"
        );
    }

    #[test]
    fn a_snapshot_without_a_currency_defaults_to_usd() {
        let parsed = parse(
            r#"{"version":"1","source":"s","unit":"u","records":[
                {"provider":"p","model":"m","input":1.0,"output":2.0}
            ]}"#,
        )
        .unwrap();
        let price = parsed.bundled("p", "m").unwrap();
        assert_eq!(price.currency, "USD");
        assert_eq!(price.cached_input, None);
        let mut catalog = Catalog {
            version: "fixture".into(),
            providers: vec![catalog::CatalogProvider {
                id: "p".into(),
                name: "P".into(),
                wire: Wire::OpenAiResponses,
                base_url: None,
                path: None,
                query: Vec::new(),
                auth: Default::default(),
                env: Vec::new(),
                hosts: Vec::new(),
                auth_providers: Vec::new(),
                aliases: BTreeMap::new(),
                models: vec![catalog::CatalogModel {
                    audio_input: None,
                    id: "m".into(),
                    name: "M".into(),
                    context: 1,
                    max_output: 1,
                    thinking_levels: Vec::new(),
                    temperature: true,
                    top_p: true,
                    reasoning: None,
                    capabilities: Default::default(),
                    input: None,
                    price: None,
                    status: "active".into(),
                    deprecated_on: None,
                }],
            }],
        };
        parsed.apply(&mut catalog);
        assert_eq!(
            catalog.providers[0].models[0].price.as_ref().unwrap().input,
            1.0
        );
    }
}
