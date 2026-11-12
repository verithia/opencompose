//! Bundled model catalog. Static by design: opening settings must not need the
//! network, and limits/capabilities are enforced by the backend, not the UI.
//! This is the data layer: which providers exist, which protocol each speaks,
//! where its requests go, and what each model accepts.
use super::auth::endpoint_host;
use super::limits::{self, InputModalities, Price};
use crate::commands::{AiCapabilities, AiProviderEntry, Wire};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Compiled in so settings open offline.
const CATALOG_JSON: &str = include_str!("../../ai-catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Bundled AI provider catalogue.
pub struct Catalog {
    pub version: String,
    pub providers: Vec<CatalogProvider>,
}

impl Catalog {
    /// The catalog entry that serves this endpoint: the same protocol on a
    /// declared host. Custom and loopback endpoints have none.
    pub fn serving(&self, wire: Wire, endpoint: &str) -> Option<&CatalogProvider> {
        let host = endpoint_host(endpoint)?;
        self.providers.iter().find(|provider| {
            provider.wire == wire
                && provider
                    .hosts
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&host))
        })
    }
}

/// How an API key or token is attached to a request. Providers that authenticate
/// with OAuth ignore this; it describes the API-key path only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMode {
    #[default]
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "x-api-key")]
    XApiKey,
    #[serde(rename = "x-goog-api-key")]
    XGoogApiKey,
}

impl AuthMode {
    /// Default header per protocol, used when the catalog has no entry for a
    /// custom endpoint.
    pub fn default_for(wire: Wire) -> Self {
        match wire {
            Wire::AnthropicMessages => Self::XApiKey,
            Wire::GeminiInteractions => Self::XGoogApiKey,
            _ => Self::Bearer,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One provider in the catalogue.
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    /// Protocol this provider's models speak. Required data: the adapter is
    /// selected from here, never from the endpoint or a saved legacy selector.
    pub wire: Wire,
    /// Base URL requests are built from. The saved endpoint is seeded with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Request path appended to `base_url`. The protocol's own path when absent;
    /// a gateway that mounts the protocol elsewhere overrides it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Query pairs every request through this route carries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query: Vec<(String, String)>,
    #[serde(default)]
    pub auth: AuthMode,
    /// Environment variables that may supply the token when no key is stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub auth_providers: Vec<String>,
    /// Ids we ourselves renamed, mapped to the current id. Only our own renames
    /// belong here: a provider's own retired ids are not aliases.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub aliases: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

impl CatalogProvider {
    /// Wire protocol this provider speaks.
    pub fn wire(&self) -> Wire {
        self.wire
    }

    /// The route's request path: the provider's override or the protocol's own.
    pub fn request_path(&self) -> &str {
        self.path
            .as_deref()
            .unwrap_or_else(|| self.wire.default_path())
    }

    /// The route's query pairs: the provider's override or the protocol's own.
    pub fn request_query(&self) -> Vec<(String, String)> {
        if self.query.is_empty() {
            self.wire.default_query()
        } else {
            self.query.clone()
        }
    }
}

/// What a model's audio input accepts. A model card that says nothing gets
/// the conservative default: one channel at 16 kHz, which every audio-capable
/// provider in the catalog reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioInput {
    /// 1 for a mono clip, 2 when the model keeps both sides.
    #[serde(default = "default_audio_channels")]
    pub channels: u8,
    /// Inline sample rate in hertz.
    #[serde(default = "default_audio_rate")]
    pub sample_rate: u32,
    /// Containers the model accepts. The clip is WAV today, which every entry
    /// lists.
    #[serde(default = "default_audio_formats")]
    pub formats: Vec<String>,
}

fn default_audio_channels() -> u8 {
    1
}

fn default_audio_rate() -> u32 {
    16_000
}

fn default_audio_formats() -> Vec<String> {
    vec!["wav".into(), "mp3".into()]
}

impl Default for AudioInput {
    fn default() -> Self {
        Self {
            channels: default_audio_channels(),
            sample_rate: default_audio_rate(),
            formats: default_audio_formats(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One model listed for a provider.
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub context: u64,
    pub max_output: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_levels: Vec<String>,
    /// Sampling parameters this model accepts. Gemini 3+ rejects them, so the
    /// adapter omits `temperature`/`topP` when the catalog says so.
    #[serde(default = "default_true")]
    pub temperature: bool,
    #[serde(default = "default_true")]
    pub top_p: bool,
    /// Whether the model takes reasoning effort instead of sampling: the OpenAI
    /// wires send `reasoning_effort`/`reasoning` and omit `temperature`, and
    /// Chat Completions reports the budget as `max_completion_tokens`. Unset
    /// keeps the protocol's family default for ids the catalog does not carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    pub capabilities: AiCapabilities,
    /// Input modalities after the user layer is merged. Bundled entries leave
    /// this unset and derive it from `capabilities`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<InputModalities>,
    /// Audio input shape from the model card. Unset means the default: mono,
    /// 16 kHz, WAV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input: Option<AudioInput>,
    /// Effective price per 1M tokens: the user layer's when it declares one,
    /// otherwise the bundled snapshot's. `None` when neither prices the model,
    /// and the panel then shows no cost for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    pub status: String,
    /// The date the vendor retires this model, for a row whose `status` is
    /// `deprecated`. The panel shows it in the badge's tooltip; unset on a model
    /// that stays available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_on: Option<String>,
}

impl CatalogModel {
    /// Effective input modalities: the merged layer when present, otherwise the
    /// bundled capability flags.
    pub fn inputs(&self) -> InputModalities {
        self.input
            .unwrap_or_else(|| InputModalities::from_capabilities(&self.capabilities))
    }

    /// The audio shape this model's card declares, or the conservative default.
    pub fn audio_input(&self) -> AudioInput {
        self.audio_input
            .clone()
            .or_else(|| known_audio_input(&self.id))
            .unwrap_or_default()
            .sanitized()
    }
}

/// Audio input shapes taken from a model's own code or model card. These apply
/// to models the bundled catalog does not carry — a user-added id still gets
/// the shape its family documents.
fn known_audio_input(model: &str) -> Option<AudioInput> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    if bare.starts_with("mimo-") {
        // MiMo's audio tokenizer: 24 kHz, 128 mel bins, 10 ms hop, at most
        // 300 s per clip.
        return Some(AudioInput {
            channels: 1,
            sample_rate: 24_000,
            formats: vec![
                "wav".into(),
                "mp3".into(),
                "flac".into(),
                "m4a".into(),
                "ogg".into(),
            ],
        });
    }
    if bare.starts_with("gemini-") {
        // Gemini combines multi-channel audio to one channel and downsamples
        // the resolution; 32 tokens per second.
        return Some(AudioInput {
            channels: 1,
            sample_rate: 16_000,
            formats: vec!["wav".into(), "mp3".into()],
        });
    }
    None
}

#[cfg(test)]
mod audio_input_tests {
    use super::*;

    fn model(id: &str) -> CatalogModel {
        CatalogModel {
            id: id.into(),
            name: id.into(),
            context: 1_000,
            max_output: 100,
            thinking_levels: Vec::new(),
            temperature: true,
            top_p: true,
            reasoning: None,
            capabilities: crate::commands::AiCapabilities::default(),
            input: None,
            audio_input: None,
            price: None,
            status: "ready".into(),
            deprecated_on: None,
        }
    }

    #[test]
    fn a_model_card_overrides_the_family_table() {
        let mut entry = model("mimo-v2.6-pro");
        assert_eq!(entry.audio_input().sample_rate, 24_000);
        entry.audio_input = Some(AudioInput {
            channels: 2,
            sample_rate: 48_000,
            formats: vec!["wav".into()],
        });
        let spec = entry.audio_input();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
    }

    #[test]
    fn an_unknown_model_gets_the_conservative_default() {
        let spec = model("some-other-model").audio_input();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.formats, vec!["wav".to_string(), "mp3".to_string()]);
    }

    #[test]
    fn a_namespaced_id_still_finds_its_family() {
        let spec = model("xiaomi/mimo-v2.6-pro").audio_input();
        assert_eq!(spec.sample_rate, 24_000);
        assert!(spec.formats.contains(&"flac".to_string()));
    }
}

impl AudioInput {
    /// Clamp what a hand-edited catalog can say into a clip we can build.
    pub fn sanitized(mut self) -> Self {
        self.channels = self.channels.clamp(1, 2);
        self.sample_rate = self.sample_rate.clamp(8_000, 48_000);
        if self.formats.is_empty() {
            self.formats = default_audio_formats();
        }
        self
    }
}

fn default_true() -> bool {
    true
}

/// Level an existing config gets: the adapters sent `medium` before the setting
/// existed, so the default request is unchanged.
pub const DEFAULT_THINKING_LEVEL: &str = "medium";

/// Levels accepted for a model the catalog declares none for: custom ids, user
/// layer models and bundled models that take no thinking level. The adapters
/// ignore the value for those models, so a level carried over from another
/// model never blocks saving.
pub const FALLBACK_THINKING_LEVELS: &[&str] = &["low", "medium", "high"];

/// The bundled catalog with the bundled prices attached, so a price is read
/// from one place: the model entry.
static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let mut catalog = parse(CATALOG_JSON).expect("bundled AI catalog is valid JSON");
    super::prices::prices().apply(&mut catalog);
    catalog
});

/// Parses a catalog. Exposed so a test can prove that a model entry is the only
/// thing a new model needs.
pub fn parse(text: &str) -> Result<Catalog, serde_json::Error> {
    serde_json::from_str(text)
}

pub fn catalog() -> &'static Catalog {
    &CATALOG
}

/// Catalog entry by provider id. The live smoke reads the declared base URL from
/// here instead of repeating it.
#[cfg(test)]
pub fn provider(id: &str) -> Option<&'static CatalogProvider> {
    catalog()
        .providers
        .iter()
        .find(|provider| provider.id == id)
}

/// The catalog as every consumer sees it: the bundled list plus the user layer
/// under `~/.opencompose/ai/models.json`. This is the only read-time merge; the
/// bundled JSON is never written. The user layer wins: an override that
/// declares a price replaces the bundled one.
pub fn merged() -> Catalog {
    #[cfg(test)]
    if let Some(catalog) = test_catalog() {
        return with_audio_inputs(limits::merged_catalog(&catalog, &limits::overrides()));
    }
    with_audio_inputs(limits::merged_catalog(catalog(), &limits::overrides()))
}

/// Fill every model's `audio_input` with its effective, sanitized shape, so a
/// surface reading the catalog never has to apply the defaults itself.
fn with_audio_inputs(mut catalog: Catalog) -> Catalog {
    for provider in &mut catalog.providers {
        for model in &mut provider.models {
            model.audio_input = Some(model.audio_input());
        }
    }
    catalog
}

// A test may install its own catalog for the current thread, so a fixture can
// prove the data path end to end without the bundled table carrying test ids
// and without other tests seeing it.
#[cfg(test)]
thread_local! {
    static TEST_CATALOG: std::cell::RefCell<Option<Catalog>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_catalog() -> Option<Catalog> {
    TEST_CATALOG.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
pub(crate) fn install_for_test(catalog: Catalog) -> TestCatalogGuard {
    TEST_CATALOG.with(|slot| *slot.borrow_mut() = Some(catalog));
    TestCatalogGuard
}

#[cfg(test)]
pub(crate) struct TestCatalogGuard;

#[cfg(test)]
impl Drop for TestCatalogGuard {
    fn drop(&mut self) {
        TEST_CATALOG.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Bundled model metadata for one id, with our own renames resolved. The exact
/// id is tried first and the normalised id second, so a saved vendor prefix or
/// tag still finds the entry the catalog declares. Metadata is not a gate: a
/// miss means "unknown id", and callers fall back to the entry's own limits
/// rather than refusing it.
pub fn model_by_wire(wire: Wire, id: &str) -> Result<CatalogModel, String> {
    let catalog = merged();
    find_model(&catalog, wire, id)
        .or_else(|| {
            normalised_id(id).and_then(|normalised| find_model(&catalog, wire, &normalised))
        })
        .ok_or_else(|| format!("\"{id}\" is not in the catalog for this protocol."))
}

/// Exact lookup for one id: our own renames are resolved, nothing is normalised.
fn find_model(catalog: &Catalog, wire: Wire, id: &str) -> Option<CatalogModel> {
    let wanted = resolve_alias(wire, id).unwrap_or_else(|| id.to_owned());
    catalog
        .providers
        .iter()
        .filter(|provider| provider.wire() == wire)
        .flat_map(|provider| provider.models.iter())
        .find(|model| model.id == wanted)
        .cloned()
}

/// The id with one leading `<vendor>/` segment and a trailing `:suffix` removed,
/// such as `google/gemini-4-argon:free` becoming `gemini-4-argon`. Returns
/// `None` when nothing is left to try, so an id is never rewritten to empty.
pub fn normalised_id(id: &str) -> Option<String> {
    let mut rest = id;
    if let Some((vendor, remainder)) = id.split_once('/')
        && !vendor.is_empty()
        && !remainder.is_empty()
    {
        rest = remainder;
    }
    if let Some((base, suffix)) = rest.rsplit_once(':')
        && !base.is_empty()
        && !suffix.is_empty()
    {
        rest = base;
    }
    (rest != id && !rest.is_empty()).then(|| rest.to_owned())
}

/// The current id when the bundled catalog renamed `id` (we own those renames).
pub fn resolve_alias(wire: Wire, id: &str) -> Option<String> {
    catalog()
        .providers
        .iter()
        .filter(|provider| provider.wire() == wire)
        .find_map(|provider| provider.aliases.get(id).cloned())
}

pub fn model(wire: Wire, id: &str) -> Result<CatalogModel, String> {
    model_by_wire(wire, id)
}

/// Whether the model takes reasoning effort instead of sampling. `None` means
/// the catalog has no entry, so the adapter keeps its protocol-family rule.
pub fn reasoning(wire: Wire, id: &str) -> Option<bool> {
    model_by_wire(wire, id)
        .ok()
        .and_then(|model| model.reasoning)
}

/// Thinking levels the catalog declares for one model. Empty means the model
/// takes no level (or is unknown), and the request adapters omit the field.
pub fn thinking_levels(wire: Wire, model: &str) -> Vec<String> {
    model_by_wire(wire, model)
        .map(|model| model.thinking_levels)
        .unwrap_or_default()
}

/// Levels a saved entry may name, or the protocol fallback when its model
/// declares none.
pub fn allowed_thinking_levels(entry: &AiProviderEntry) -> Vec<String> {
    let declared = thinking_levels(entry.wire(), &entry.model);
    if declared.is_empty() {
        FALLBACK_THINKING_LEVELS
            .iter()
            .map(|level| (*level).to_owned())
            .collect()
    } else {
        declared
    }
}

/// Catalog entry that serves this saved entry: the same wire on a declared host.
/// Custom and loopback endpoints have none.
fn entry_provider(entry: &AiProviderEntry) -> Option<&'static CatalogProvider> {
    catalog().serving(entry.wire(), &entry.endpoint)
}

/// Header mode the catalog declares for this entry's provider. Custom endpoints
/// fall back to the protocol default, so no new saved field is needed.
pub fn auth_mode(entry: &AiProviderEntry) -> AuthMode {
    entry_provider(entry)
        .map(|provider| provider.auth)
        .unwrap_or_else(|| AuthMode::default_for(entry.wire()))
}

/// Environment variable the catalog names as a token source for this entry.
/// Only used when no key is stored in the OS credential store.
pub fn env_key(entry: &AiProviderEntry) -> Option<&'static str> {
    entry_provider(entry)?.env.first().map(String::as_str)
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Hosts the catalog serves. Renames apply here; a custom proxy or a local
/// server names its own models, so nothing of ours is written into its entry.
fn catalog_serves(entry: &AiProviderEntry) -> bool {
    let Some(host) = endpoint_host(&entry.endpoint) else {
        return false;
    };
    if is_loopback(&host) {
        return false;
    }
    catalog().providers.iter().any(|provider| {
        provider.wire() == entry.wire()
            && provider
                .hosts
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&host))
    })
}

/// Whether a vendor host from the bundled catalog serves this entry, which
/// means every request has to authenticate. A custom endpoint and a loopback
/// server are the operator's own and may be keyless.
pub fn requires_credential(entry: &AiProviderEntry) -> bool {
    catalog_serves(entry)
}

/// Whether the catalog describes this entry's model. False only where the
/// catalog is expected to know the ids: a vendor host with an id the bundled
/// catalog and the user layer both leave out. Custom endpoints and loopback
/// servers are the user's own, so they are never flagged.
pub fn entry_model_described(entry: &AiProviderEntry) -> bool {
    if !catalog_serves(entry) {
        return true;
    }
    limits::is_overridden(&entry.id, &entry.model)
        || model_by_wire(entry.wire(), &entry.model).is_ok()
}

/// One informational line: what the panel shows, never why a save failed.
pub fn model_warning(entry: &AiProviderEntry) -> String {
    format!(
        "\"{}\" uses model \"{}\", which the {} catalog does not describe; limits default to 1M context and 16K output. Pick a model in the provider's settings.",
        entry.name,
        entry.model,
        entry.wire().label()
    )
}

/// Rewrites an id we renamed to the current one. Only our own renames are
/// healed; an id we never published stays exactly as the operator typed it.
pub fn heal_renamed_model(entry: &mut AiProviderEntry) {
    if !catalog_serves(entry) {
        return;
    }
    let wire = entry.wire();
    if let Some(current) = resolve_alias(wire, &entry.model) {
        entry.model = current;
    }
    let mut healed: Vec<String> = Vec::with_capacity(entry.models.len());
    for model in entry.models.drain(..) {
        let model = resolve_alias(wire, &model).unwrap_or(model);
        if !healed.contains(&model) {
            healed.push(model);
        }
    }
    entry.models = healed;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AuthKind, AuthProvider, Wire};

    /// A private limits store for one catalog test; the path is process-global,
    /// so the guard must live for the whole test.
    fn limits_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "opencompose-catalog-limits-{}-{nanos}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("models.json")
    }

    fn entry(endpoint: &str, model: &str) -> AiProviderEntry {
        AiProviderEntry {
            id: "fixture".into(),
            name: "Fixture".into(),
            kind: crate::commands::ProviderKind::OpenAiResponses,
            wire: Some(Wire::OpenAiResponses),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: endpoint.into(),
            model: model.into(),
            models: Vec::new(),
            context_length: 128000,
            max_tokens: 4096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities::default(),
        }
    }

    #[test]
    fn bundled_catalog_has_the_required_models_and_protocols() {
        let catalog = catalog();
        assert!(!catalog.version.is_empty());
        assert!(model(Wire::OpenAiResponses, "gpt-6.1-sol").is_ok());
        assert!(model(Wire::OpenAiResponses, "gpt-5.5").is_ok());
        assert!(model(Wire::AnthropicMessages, "claude-opus-4-6").is_ok());
        let gemini = model(Wire::GeminiInteractions, "gemini-4-argon").unwrap();
        assert_eq!(gemini.context, 1_048_576);
        assert_eq!(gemini.thinking_levels, ["low", "medium", "high"]);
        assert!(model(Wire::OpenAiChatCompletions, "deepseek-flash").is_ok());
        assert!(model(Wire::OpenAiChatCompletions, "llama3.1").is_ok());
        let provider = |id: &str| catalog.providers.iter().find(|p| p.id == id).unwrap();
        assert!(provider("openai").auth_providers.is_empty());
        assert!(provider("google").auth_providers.is_empty());
        assert!(provider("anthropic").auth_providers.is_empty());
    }

    #[test]
    fn a_model_announced_for_removal_stays_listed_and_says_so() {
        let fixture = parse(
            r#"{
                "version": "fixture",
                "providers": [{
                    "id": "acme",
                    "name": "Acme",
                    "wire": "openai-chat-completions",
                    "base_url": "https://gw.acme.test/v1",
                    "hosts": ["gw.acme.test"],
                    "models": [
                        {
                            "id": "acme-old",
                            "name": "Acme Old (removal 2026-11-02)",
                            "context": 8000,
                            "max_output": 1000,
                            "capabilities": { "text": true },
                            "status": "deprecated",
                            "deprecated_on": "2026-11-02"
                        },
                        {
                            "id": "acme-new",
                            "name": "Acme New",
                            "context": 8000,
                            "max_output": 1000,
                            "capabilities": { "text": true },
                            "status": "active"
                        }
                    ]
                }]
            }"#,
        )
        .unwrap();
        let _guard = install_for_test(fixture);
        let removed = model(Wire::OpenAiChatCompletions, "acme-old").unwrap();
        assert_eq!(removed.status, "deprecated");
        assert!(removed.name.contains("2026-11-02"), "{}", removed.name);
        // The badge's tooltip reads the date from here, not from the name.
        assert_eq!(removed.deprecated_on.as_deref(), Some("2026-11-02"));
        let active = model(Wire::OpenAiChatCompletions, "acme-new").unwrap();
        assert_eq!(active.status, "active");
        assert!(active.deprecated_on.is_none());
    }

    #[test]
    fn opencode_go_is_a_data_only_generate_content_gateway() {
        let provider = provider("opencode-go").unwrap();
        assert_eq!(provider.wire, Wire::GeminiGenerate);
        assert_eq!(provider.wire(), Wire::GeminiGenerate);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://opencode.ai/inference/go/google/v1beta")
        );
        assert_eq!(provider.auth, AuthMode::Bearer);
        assert_eq!(provider.env, ["OPENCODE_API_KEY"]);
        assert_eq!(provider.hosts, ["opencode.ai"]);
        let model = model(Wire::GeminiGenerate, "gemini-4-argon").unwrap();
        assert_eq!(model.context, 1_048_576);
        assert_eq!(model.max_output, 65_536);
        assert_eq!(model.thinking_levels, ["low", "medium", "high"]);
        assert!(!model.temperature && !model.top_p, "argon rejects sampling");

        let mut entry = entry(
            "https://opencode.ai/inference/go/google/v1beta",
            "gemini-4-argon",
        );
        entry.wire = Some(Wire::GeminiGenerate);
        assert_eq!(auth_mode(&entry), AuthMode::Bearer);
        assert_eq!(env_key(&entry), Some("OPENCODE_API_KEY"));
        assert!(entry_model_described(&entry));
        entry.model = "gemini-3.8-flash".into();
        assert!(
            !entry_model_described(&entry),
            "an id the gateway list leaves out is only undescribed"
        );

        entry.wire = Some(Wire::GeminiInteractions);
        entry.model = "gemini-4-argon".into();
        entry.endpoint = "https://generativelanguage.googleapis.com/v1beta".into();
        assert_eq!(auth_mode(&entry), AuthMode::XGoogApiKey);
        entry.endpoint = "https://api.anthropic.com/v1/messages".into();
        entry.wire = Some(Wire::AnthropicMessages);
        assert_eq!(auth_mode(&entry), AuthMode::XApiKey);
        entry.endpoint = "https://example.test/v1beta".into();
        entry.wire = Some(Wire::GeminiGenerate);
        assert_eq!(
            auth_mode(&entry),
            AuthMode::Bearer,
            "custom endpoints use the protocol default"
        );
        assert_eq!(env_key(&entry), None, "custom endpoints never read our env");
    }

    #[test]
    fn every_bundled_model_is_found_from_its_wire_and_id_alone() {
        // The request path has no model allowlist: a model entry is the only
        // registration a model needs, and the lookup never falls through to a
        // hard-coded family table.
        for provider in &catalog().providers {
            for model in &provider.models {
                let found = model_by_wire(provider.wire, &model.id)
                    .unwrap_or_else(|error| panic!("{} / {}: {error}", provider.id, model.id));
                assert_eq!(found.id, model.id);
                assert_eq!(found.context, model.context);
            }
        }
        assert!(model_by_wire(Wire::OpenAiResponses, "not-in-the-table").is_err());
    }

    #[test]
    fn an_unknown_id_has_no_catalog_metadata_and_no_error() {
        let error = model(Wire::OpenAiResponses, "gpt-9-imaginary").unwrap_err();
        assert!(error.contains("gpt-9-imaginary"));
        assert!(model(Wire::AnthropicMessages, "gpt-6.1-sol").is_err());
    }

    #[test]
    fn a_vendor_prefix_and_a_tag_are_stripped_from_a_saved_id() {
        assert_eq!(
            normalised_id("google/gemini-4-argon").as_deref(),
            Some("gemini-4-argon")
        );
        assert_eq!(
            normalised_id("gemini-4-argon:free").as_deref(),
            Some("gemini-4-argon")
        );
        assert_eq!(
            normalised_id("google/gemini-4-argon:free").as_deref(),
            Some("gemini-4-argon")
        );
        // One leading segment only: a nested path keeps the rest.
        assert_eq!(
            normalised_id("vendor/team/model").as_deref(),
            Some("team/model")
        );
        // Nothing to strip, and never read an id as empty.
        assert_eq!(normalised_id("gemini-4-argon"), None);
        assert_eq!(normalised_id("vendor/"), None);
        assert_eq!(normalised_id("/model"), None);
        assert_eq!(normalised_id(":free"), None);
        assert_eq!(normalised_id(""), None);
    }

    #[test]
    fn the_exact_catalog_id_wins_over_the_normalised_one() {
        let fixture = parse(
            r#"{"version":"test","providers":[{"id":"acme","name":"Acme",
                "wire":"openai-chat-completions","models":[
                {"id":"vendor/model","name":"Prefixed","context":1000,"max_output":10,
                 "capabilities":{},"status":"active"},
                {"id":"model","name":"Bare","context":2000,"max_output":20,
                 "capabilities":{},"status":"active"}]}]}"#,
        )
        .unwrap();
        let _fixture = install_for_test(fixture);
        let exact = model_by_wire(Wire::OpenAiChatCompletions, "vendor/model").unwrap();
        assert_eq!(exact.id, "vendor/model");
        assert_eq!(exact.context, 1_000);
        let bare = model_by_wire(Wire::OpenAiChatCompletions, "model").unwrap();
        assert_eq!(bare.context, 2_000);
    }

    #[test]
    fn vendor_host_ids_are_described_or_merely_undescribed() {
        assert!(entry_model_described(&entry(
            "https://api.openai.com/v1/responses",
            "gpt-6.1-sol"
        )));
        assert!(!entry_model_described(&entry(
            "https://api.openai.com/v1/responses",
            "my-model"
        )));
        // The user's own layer describes ids the bundled table never carried.
        let target = limits_path("described");
        let _limits_guard = crate::ai::limits::test_path(target.clone());
        crate::ai::limits::install_for_test(
            "fixture",
            "my-own-model",
            crate::ai::limits::ModelLimits {
                context: 64_000,
                max_output: 8_192,
                input: crate::ai::limits::InputModalities {
                    text: true,
                    ..Default::default()
                },
                price: None,
            },
        );
        assert!(entry_model_described(&entry(
            "https://api.openai.com/v1/responses",
            "my-own-model"
        )));
        crate::ai::limits::remove("fixture", "my-own-model").unwrap();
        // Custom proxies and loopback servers name their own ids: never flagged.
        assert!(entry_model_described(&entry(
            "https://example.test/v1/responses",
            "my-model"
        )));
        assert!(entry_model_described(&entry(
            "http://127.0.0.1:11434/v1/chat/completions",
            "any"
        )));
        let mut openai = entry("https://api.openai.com/v1/responses", "gpt-6.1-sol");
        assert!(entry_model_described(&openai));
        openai.model = "gpt-9-imaginary".into();
        assert!(!entry_model_described(&openai));
        let mut claude = entry("https://api.anthropic.com/v1/messages", "claude-opus-4-6");
        claude.wire = Some(Wire::AnthropicMessages);
        assert!(entry_model_described(&claude));
        claude.model = "gemini-4-argon".into();
        assert!(!entry_model_described(&claude));
        drop(_limits_guard);
        std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn vendor_host_checks_match_exact_hosts_not_substrings() {
        for sneaky in [
            "https://api.openai.com.evil.test/v1/responses",
            "https://evilapi.openai.com/v1/responses",
            "https://api.openai.com.evil.test:8443/v1/responses",
        ] {
            // A lookalike host is the user's own: a substring match would flag it.
            assert!(
                entry_model_described(&entry(sneaky, "gpt-9-imaginary")),
                "{sneaky}"
            );
        }
    }

    #[test]
    fn the_warning_names_the_provider_the_model_the_protocol_and_the_action() {
        let entry = entry("https://api.openai.com/v1/responses", "gpt-4o");
        let warning = model_warning(&entry);
        assert!(warning.contains("\"Fixture\""), "{warning}");
        assert!(warning.contains("\"gpt-4o\""), "{warning}");
        assert!(warning.contains("OpenAI Responses"), "{warning}");
        assert!(warning.contains("provider's settings"), "{warning}");
        assert!(!warning.contains("Unknown model"), "{warning}");
    }

    #[test]
    fn the_rename_we_own_resolves_and_heals_the_saved_id() {
        let flash = model(Wire::OpenAiChatCompletions, "deepseek-v4-flash").unwrap();
        assert_eq!(flash.id, "deepseek-flash");
        // Retired ids providers own are not aliases of ours.
        assert!(model(Wire::OpenAiChatCompletions, "gpt-4o").is_err());
        assert!(model(Wire::OpenAiResponses, "gpt-4o").is_err());

        let mut old = entry(
            "https://api.deepseek.com/chat/completions",
            "deepseek-v4-flash",
        );
        old.wire = Some(Wire::OpenAiChatCompletions);
        old.models = vec!["deepseek-v4-flash".into(), "deepseek-v4-pro".into()];
        heal_renamed_model(&mut old);
        assert_eq!(old.model, "deepseek-flash");
        assert_eq!(old.models, ["deepseek-flash", "deepseek-v4-pro"]);

        // A proxy that happens to use the same id keeps it: only our own hosts heal.
        let mut proxy = entry(
            "https://example.test/v1/chat/completions",
            "deepseek-v4-flash",
        );
        proxy.wire = Some(Wire::OpenAiChatCompletions);
        heal_renamed_model(&mut proxy);
        assert_eq!(proxy.model, "deepseek-v4-flash");
    }

    #[test]
    fn every_catalog_provider_declares_its_protocol_and_route() {
        for provider in &catalog().providers {
            // The adapter is selected from this data alone.
            assert!(
                !matches!(provider.wire, Wire::Unknown),
                "{} names no protocol",
                provider.id
            );
            for auth in &provider.auth_providers {
                assert!(matches!(auth.as_str(), "openai_chat_gpt"));
            }
            if provider
                .auth_providers
                .iter()
                .any(|auth| auth == "openai_chat_gpt")
            {
                assert_eq!(provider.id, "openai", "ChatGPT OAuth is OpenAI-only");
                assert_eq!(provider.wire, Wire::OpenAiResponses);
            }
            if provider.id == "custom" {
                assert!(provider.base_url.is_none());
                continue;
            }
            assert!(
                provider.base_url.is_some(),
                "{} has no base URL",
                provider.id
            );
            assert!(
                !provider.request_path().is_empty(),
                "{} has no request path",
                provider.id
            );
            let mut saved = entry(
                provider.base_url.as_deref().unwrap(),
                provider
                    .models
                    .first()
                    .map(|model| model.id.as_str())
                    .unwrap_or("m"),
            );
            saved.wire = Some(provider.wire);
            let route = crate::ai::route::Route::for_entry_in(catalog(), &saved);
            assert!(route.is_ok(), "{}: {:?}", provider.id, route.err());
        }
    }

    #[test]
    fn oauth_chatgpt_entry_is_described_by_the_catalog() {
        let mut chatgpt = entry("https://api.openai.com/v1/responses", "gpt-6.1-sol");
        chatgpt.auth = AuthKind::OAuth;
        chatgpt.auth_provider = Some(AuthProvider::OpenAiChatGpt);
        assert!(entry_model_described(&chatgpt));
    }
}
