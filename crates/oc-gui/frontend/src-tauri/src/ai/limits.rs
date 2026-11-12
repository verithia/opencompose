//! Per-model limits set by the user: context length, output token limit, input
//! modalities and optional prices. Stored under `~/.opencompose/ai/models.json`
//! and merged over the bundled catalog at read time, so an override for a model
//! the catalog does not know creates a usable entry instead of being ignored.
//!
//! A corrupt or invalid file never blocks the AI panel: the store falls back to
//! the bundled catalog and records a warning.
use super::catalog::{Catalog, CatalogModel};
use crate::commands::{AiCapabilities, AiProviderEntry};
use crate::private_fs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

/// Model override file name.
pub const FILE_NAME: &str = "models.json";
const VERSION: u32 = 1;
const MAX_PROVIDER_ID_CHARS: usize = 100;
const MAX_MODEL_ID_CHARS: usize = 200;

/// One kind of content a user can attach to a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
    Video,
    Audio,
}

impl InputModality {
    /// Wire spelling of the modality.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }
}

/// Input modalities a model accepts. At least one must be enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputModalities {
    #[serde(default)]
    pub text: bool,
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub video: bool,
    #[serde(default)]
    pub audio: bool,
}

impl InputModalities {
    /// Whether any input modality is enabled.
    pub fn any(self) -> bool {
        self.text || self.image || self.video || self.audio
    }

    /// Whether the set allows `modality`.
    pub fn allows(self, modality: InputModality) -> bool {
        match modality {
            InputModality::Text => self.text,
            InputModality::Image => self.image,
            InputModality::Video => self.video,
            InputModality::Audio => self.audio,
        }
    }

    /// Bundled models only declare text and audio; image and video are opt-in.
    pub fn from_capabilities(capabilities: &AiCapabilities) -> Self {
        Self {
            text: capabilities.text,
            image: false,
            video: false,
            audio: capabilities.audio_in,
        }
    }

    /// Comma-separated labels of enabled modalities.
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for modality in [
            InputModality::Text,
            InputModality::Image,
            InputModality::Video,
            InputModality::Audio,
        ] {
            if self.allows(modality) {
                parts.push(modality.as_str());
            }
        }
        parts.join(", ")
    }
}

/// Price per 1M tokens, matching how providers bill. Optional as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Price of a cached input token. A catalogue that leaves it out bills
    /// cached tokens at `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<f64>,
    /// ISO 4217 code of the amounts above.
    #[serde(
        default = "default_currency",
        skip_serializing_if = "is_default_currency"
    )]
    pub currency: String,
}

/// Currency a price is in when nothing else says. The bundled snapshot is USD.
pub const DEFAULT_CURRENCY: &str = "USD";

fn default_currency() -> String {
    DEFAULT_CURRENCY.to_owned()
}

fn is_default_currency(currency: &str) -> bool {
    currency == DEFAULT_CURRENCY
}

/// The fields a user may override for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    pub context: u64,
    pub max_output: u64,
    pub input: InputModalities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
}

impl ModelLimits {
    /// Validate context/output limits, modalities and price.
    pub fn validate(&self, path: &str) -> Result<(), String> {
        if self.context == 0 {
            return Err(format!("{path}.context must be greater than 0"));
        }
        if self.max_output == 0 {
            return Err(format!("{path}.max_output must be greater than 0"));
        }
        if self.max_output > self.context {
            return Err(format!(
                "{path}.max_output ({}) must not exceed {path}.context ({})",
                self.max_output, self.context
            ));
        }
        if !self.input.any() {
            return Err(format!(
                "{path}.input must enable at least one of text, image, video or audio"
            ));
        }
        if let Some(price) = &self.price {
            for (field, value) in [("input", price.input), ("output", price.output)] {
                if !value.is_finite() || value < 0.0 {
                    return Err(format!(
                        "{path}.price.{field} must be a finite number of 0 or more"
                    ));
                }
            }
            if let Some(cached) = price.cached_input
                && (!cached.is_finite() || cached < 0.0)
            {
                return Err(format!(
                    "{path}.price.cached_input must be a finite number of 0 or more"
                ));
            }
            if price.currency.len() != 3
                || !price
                    .currency
                    .chars()
                    .all(|character| character.is_ascii_uppercase())
            {
                return Err(format!(
                    "{path}.price.currency must be a three-letter ISO 4217 code such as USD"
                ));
            }
        }
        Ok(())
    }

    /// A catalog entry for a model the bundled catalog does not declare.
    pub fn to_model(&self, id: &str) -> CatalogModel {
        CatalogModel {
            audio_input: None,
            id: id.to_owned(),
            name: id.to_owned(),
            context: self.context,
            max_output: self.max_output,
            thinking_levels: Vec::new(),
            temperature: true,
            top_p: true,
            reasoning: None,
            capabilities: AiCapabilities {
                text: self.input.text,
                audio_in: self.input.audio,
                audio_out: false,
                native_tool_calling: false,
                streaming: true,
            },
            input: Some(self.input),
            price: self.price.clone(),
            status: "active".into(),
            deprecated_on: None,
        }
    }

    fn apply(&self, model: &mut CatalogModel) {
        model.context = self.context;
        model.max_output = self.max_output;
        model.input = Some(self.input);
        // An absent override price keeps the bundled one: saving a context limit
        // must not delete the price the catalog published.
        if self.price.is_some() {
            model.price = self.price.clone();
        }
        model.capabilities.text = self.input.text;
        model.capabilities.audio_in = self.input.audio;
    }
}

/// The whole override file: provider id -> model id -> limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, BTreeMap<String, ModelLimits>>,
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            version: VERSION,
            providers: BTreeMap::new(),
        }
    }
}

fn default_version() -> u32 {
    VERSION
}

impl Overrides {
    /// Validate the file version and every override.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!(
                "{FILE_NAME}: unsupported version {}; expected {VERSION}",
                self.version
            ));
        }
        for (provider_id, models) in &self.providers {
            validate_key("provider", provider_id, MAX_PROVIDER_ID_CHARS)?;
            for (model_id, limits) in models {
                validate_key("model", model_id, MAX_MODEL_ID_CHARS)?;
                limits.validate(&format!("providers.{provider_id}.{model_id}"))?;
            }
        }
        Ok(())
    }

    /// Look up an override by provider and model.
    pub fn get(&self, provider_id: &str, model_id: &str) -> Option<&ModelLimits> {
        self.providers.get(provider_id)?.get(model_id)
    }

    /// Applies the layer to a catalog: replaces declared fields on a bundled
    /// model and appends models the catalog does not declare.
    pub fn merge_into(&self, catalog: &mut Catalog) {
        for (provider_id, models) in &self.providers {
            let Some(provider) = catalog
                .providers
                .iter_mut()
                .find(|provider| &provider.id == provider_id)
            else {
                continue;
            };
            for (model_id, limits) in models {
                match provider
                    .models
                    .iter_mut()
                    .find(|model| &model.id == model_id)
                {
                    Some(model) => limits.apply(model),
                    None => provider.models.push(limits.to_model(model_id)),
                }
            }
        }
    }
}

fn validate_key(kind: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{FILE_NAME}: a {kind} id is empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "{FILE_NAME}: {kind} id is longer than {max_chars} characters"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!(
            "{FILE_NAME}: {kind} id contains a control character"
        ));
    }
    Ok(())
}

/// Parses and validates the file contents. Used by the save command so field
/// errors are reported before anything is written.
pub fn parse(text: &str) -> Result<Overrides, String> {
    let overrides: Overrides =
        serde_json::from_str(text).map_err(|error| format!("{FILE_NAME} is not valid: {error}"))?;
    overrides
        .validate()
        .map_err(|error| format!("{FILE_NAME}: {error}"))?;
    Ok(overrides)
}

/// Reads the file. Any problem yields an empty store plus a warning: overrides
/// are an optional layer, never a reason to fail startup.
pub fn load_from(path: &Path) -> (Overrides, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (Overrides::default(), None);
        }
        Err(error) => {
            return (
                Overrides::default(),
                Some(format!(
                    "could not read {}: {error}; using only the bundled catalog",
                    path.display()
                )),
            );
        }
    };
    match parse(&text) {
        Ok(overrides) => (overrides, None),
        Err(error) => (
            Overrides::default(),
            Some(format!("{error}; using only the bundled catalog")),
        ),
    }
}

#[derive(Debug, Default)]
struct State {
    overrides: Overrides,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(initial_state()));

fn initial_state() -> State {
    #[cfg(test)]
    {
        // Tests install their own layer; never read the developer's home file.
        State::default()
    }
    #[cfg(not(test))]
    {
        let (overrides, warning) = load_from(&path());
        if let Some(warning) = &warning {
            log::warn!("[ai] {warning}");
        }
        State { overrides }
    }
}

#[cfg(test)]
static TEST_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
/// Serializes tests that redirect the store: the path is process-global.
#[cfg(test)]
static TEST_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The user's store. Writes go through [`writable_path`], which a test build
/// restricts to a path installed by [`test_path`].
#[cfg_attr(test, allow(dead_code))]
pub fn path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_PATH.lock().ok().and_then(|path| path.clone()) {
        return path;
    }
    private_fs::config_dir().join("ai").join(FILE_NAME)
}

/// Redirects the store at a fixture file until the guard drops, so a test never
/// writes the developer's home file. Only one such test runs at a time; hold the
/// guard for the whole test so its lock covers every mutation.
#[cfg(test)]
#[must_use = "the redirect ends when the guard drops; hold it for the whole test"]
pub(crate) fn test_path(path: PathBuf) -> TestPathGuard {
    let lock = TEST_PATH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_PATH.lock() {
        *slot = Some(path);
    }
    TestPathGuard { _lock: lock }
}

#[cfg(test)]
pub(crate) struct TestPathGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestPathGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_PATH.lock() {
            *slot = None;
        }
    }
}

pub fn overrides() -> Overrides {
    STATE
        .read()
        .map(|state| state.overrides.clone())
        .unwrap_or_default()
}

/// The file a write may use. In a test build the path must come from
/// [`test_path`], so a test that forgot the guard fails instead of rewriting the
/// developer's own store.
#[cfg(test)]
fn writable_path() -> Result<PathBuf, String> {
    match TEST_PATH.lock().ok().and_then(|path| path.clone()) {
        Some(path) => Ok(path),
        None => Err(
            "model limits: a test tried to write the real store; install limits::test_path first"
                .to_string(),
        ),
    }
}

/// The file a write may use; outside tests that is the user's own store.
#[cfg(not(test))]
fn writable_path() -> Result<PathBuf, String> {
    Ok(path())
}

/// Applies one change under the write lock and installs it only after the file
/// write succeeded, so a failed save leaves neither the file nor the merged
/// catalog carrying a layer the panel reported as unsaved. The change returns
/// whether there is anything to persist.
fn mutate(apply: impl FnOnce(&mut Overrides) -> bool) -> Result<PathBuf, String> {
    let path = writable_path()?;
    let mut state = STATE
        .write()
        .map_err(|_| "Model limits lock failed".to_string())?;
    let mut next = state.overrides.clone();
    if !apply(&mut next) {
        return Ok(path);
    }
    let mut text = serde_json::to_string_pretty(&next)
        .map_err(|_| "Could not serialize model limits".to_string())?;
    text.push('\n');
    private_fs::write_atomic(&path, text.as_bytes())
        .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
    state.overrides = next;
    Ok(path)
}

pub fn put(provider_id: &str, model_id: &str, limits: ModelLimits) -> Result<PathBuf, String> {
    let mut models = BTreeMap::new();
    models.insert(model_id.to_owned(), limits);
    put_many(provider_id, &models)
}

/// Writes one provider's models in a single atomic file write. Validation is
/// all-or-nothing, so a bad row cannot leave a half-written layer.
pub fn put_many(
    provider_id: &str,
    models: &BTreeMap<String, ModelLimits>,
) -> Result<PathBuf, String> {
    validate_key("provider", provider_id, MAX_PROVIDER_ID_CHARS)?;
    if models.is_empty() {
        return writable_path();
    }
    for (model_id, limits) in models {
        validate_key("model", model_id, MAX_MODEL_ID_CHARS)?;
        limits.validate(&format!("providers.{provider_id}.{model_id}"))?;
    }
    mutate(|overrides| {
        let entry = overrides
            .providers
            .entry(provider_id.to_owned())
            .or_default();
        for (model_id, limits) in models {
            entry.insert(model_id.clone(), limits.clone());
        }
        true
    })
}

pub fn remove(provider_id: &str, model_id: &str) -> Result<PathBuf, String> {
    mutate(|overrides| {
        let Some(models) = overrides.providers.get_mut(provider_id) else {
            return false;
        };
        let removed = models.remove(model_id).is_some();
        if models.is_empty() {
            overrides.providers.remove(provider_id);
        }
        removed
    })
}

/// The merged catalog: the bundled list plus this user's layer.
pub fn merged_catalog(bundled: &Catalog, overrides: &Overrides) -> Catalog {
    let mut catalog = bundled.clone();
    overrides.merge_into(&mut catalog);
    catalog
}

pub fn get(provider_id: &str, model_id: &str) -> Option<ModelLimits> {
    overrides().get(provider_id, model_id).cloned()
}

pub fn is_overridden(provider_id: &str, model_id: &str) -> bool {
    overrides().get(provider_id, model_id).is_some()
}

/// Merged model for a saved entry. The provider-keyed override wins over the
/// wire-level catalog lookup because it names exactly this provider. The lookup
/// tries the saved id first and the id without its vendor prefix or tag second.
pub fn model_for(entry: &AiProviderEntry) -> Option<CatalogModel> {
    if let Some(limits) = overrides().get(&entry.id, &entry.model) {
        return Some(limits.to_model(&entry.model));
    }
    super::catalog::model(entry.wire(), &entry.model).ok()
}

/// Output tokens sent on the wire: the merged model's limit, or the saved entry
/// value for models with no catalog entry and no override.
pub fn max_output(entry: &AiProviderEntry) -> u64 {
    model_for(entry)
        .map(|model| model.max_output)
        .filter(|max_output| *max_output > 0)
        .unwrap_or_else(|| entry.max_tokens.max(1))
}

/// Context windows of models the bundled catalog does not carry yet. A saved
/// provider entry is a fallback, not the truth: a stale or guessed entry number
/// made a 1M-token model compact at 75% of 64K after one large tool result.
const KNOWN_CONTEXTS: &[(&str, u64)] = &[("xiaomi/mimo-v2.6-pro", 1_000_000)];

fn known_context(id: &str) -> Option<u64> {
    let lookup = |candidate: &str| {
        KNOWN_CONTEXTS
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(candidate))
            .map(|(_, context)| *context)
    };
    // The saved id is tried first, then the same id without its vendor prefix or
    // tag, matching the order the catalog lookup uses.
    lookup(id).or_else(|| super::catalog::normalised_id(id).and_then(|id| lookup(&id)))
}

/// Input window a request for this entry uses: the catalog or known value when
/// one describes the model, otherwise the saved entry's own number. Auto-
/// compaction sizes itself from this.
pub fn context_length(entry: &AiProviderEntry) -> u64 {
    model_for(entry)
        .map(|model| model.context)
        .filter(|context| *context > 0)
        .or_else(|| known_context(&entry.model))
        .unwrap_or_else(|| entry.context_length.max(1))
}

/// Effective input modalities for a saved entry.
pub fn input_modalities(entry: &AiProviderEntry) -> InputModalities {
    model_for(entry)
        .map(|model| model.inputs())
        .unwrap_or_else(|| InputModalities::from_capabilities(&entry.capabilities))
}

pub fn modality_allowed(entry: &AiProviderEntry, modality: InputModality) -> bool {
    input_modalities(entry).allows(modality)
}

fn model_label(entry: &AiProviderEntry) -> String {
    format!("{} ({})", entry.name, entry.model)
}

fn refusal(entry: &AiProviderEntry, modality: InputModality) -> super::ProviderError {
    super::ProviderError::config(format!(
        "{} does not accept {} input: its model limits declare {}. Remove the attachment or \
         choose a model that accepts {}.",
        model_label(entry),
        modality.as_str(),
        input_modalities(entry).label(),
        modality.as_str()
    ))
}

/// Refuses attached media the merged model does not declare. Runs before the
/// request is lowered, so a rejected attachment never opens a socket.
pub fn gate_input(entry: &AiProviderEntry, messages: &Value) -> Result<(), super::ProviderError> {
    let Some(messages) = messages.as_array() else {
        return Ok(());
    };
    for message in messages {
        if let Some(attachments) = message.get("media").and_then(Value::as_array) {
            for attachment in attachments {
                // A reference whose file is gone is skipped when the request is
                // built, so it must not refuse the turn here either.
                if attachment.get("missing").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| !path.is_empty() && !Path::new(path).is_file())
                {
                    continue;
                }
                let Some(modality) = attachment_modality(attachment) else {
                    continue;
                };
                if !modality_allowed(entry, modality) {
                    return Err(refusal(entry, modality));
                }
            }
        }
        if message.get("input_audio").is_some() && !modality_allowed(entry, InputModality::Audio) {
            return Err(refusal(entry, InputModality::Audio));
        }
        if let Some(parts) = message.get("content").and_then(Value::as_array) {
            for part in parts {
                if let Some(modality) = part_modality(part)
                    && !modality_allowed(entry, modality)
                {
                    return Err(refusal(entry, modality));
                }
            }
        }
    }
    Ok(())
}

/// The modality one saved attachment carries; `None` for a kind no wire accepts.
fn attachment_modality(attachment: &Value) -> Option<InputModality> {
    match attachment.get("kind").and_then(Value::as_str)? {
        "audio" => Some(InputModality::Audio),
        "image" => Some(InputModality::Image),
        "video" => Some(InputModality::Video),
        _ => None,
    }
}

/// Lowers every historical attachment the merged model cannot take to one text
/// line naming the file, so a conversation that switched to a text-only model
/// keeps running instead of refusing every later turn. The turn's own user
/// message is left alone: the composer owns that attachment, its chip says why
/// it was refused, and removing it is the user's decision. Returns how many
/// attachments were lowered.
pub fn lower_unaccepted_media(entry: &AiProviderEntry, messages: &mut Value) -> usize {
    let Some(messages) = messages.as_array_mut() else {
        return 0;
    };
    let current = messages
        .iter()
        .rposition(|message| message["role"] == "user");
    let mut lowered = 0;
    for (index, message) in messages.iter_mut().enumerate() {
        if current == Some(index) {
            break;
        }
        let Some(attachments) = message.get("media").and_then(Value::as_array).cloned() else {
            continue;
        };
        let mut kept = Vec::new();
        let mut lines = Vec::new();
        for attachment in &attachments {
            match attachment_modality(attachment) {
                Some(modality) if !modality_allowed(entry, modality) => {
                    lowered += 1;
                    lines.push(format!(
                        "Attachment \"{}\" was not sent: {} does not accept {} input.",
                        attachment["name"].as_str().unwrap_or("attachment"),
                        model_label(entry),
                        modality.as_str()
                    ));
                }
                _ => kept.push(attachment.clone()),
            }
        }
        if lines.is_empty() {
            continue;
        }
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        if kept.is_empty() {
            object.remove("media");
        } else {
            object.insert("media".into(), Value::Array(kept));
        }
        let lowered_text = lines.join("\n");
        let content = match object.get("content") {
            Some(Value::String(text)) if !text.is_empty() => {
                Value::String(format!("{text}\n{lowered_text}"))
            }
            Some(Value::Array(parts)) => {
                let mut parts = parts.clone();
                parts.push(serde_json::json!({"type": "text", "text": lowered_text}));
                Value::Array(parts)
            }
            _ => Value::String(lowered_text),
        };
        object.insert("content".into(), content);
    }
    lowered
}

/// Content-array part types used by the OpenAI, Anthropic and Gemini wires.
fn part_modality(part: &Value) -> Option<InputModality> {
    match part.get("type").and_then(Value::as_str)? {
        "image_url" | "input_image" | "image" => Some(InputModality::Image),
        "input_audio" | "audio" => Some(InputModality::Audio),
        "video_url" | "input_video" | "video" => Some(InputModality::Video),
        _ => None,
    }
}

/// Drops an installed fixture without touching the developer's own layer.
#[cfg(test)]
pub(crate) fn clear_for_test(provider_id: &str, model_id: &str) {
    if let Ok(mut state) = STATE.write()
        && let Some(models) = state.overrides.providers.get_mut(provider_id)
    {
        models.remove(model_id);
        if models.is_empty() {
            state.overrides.providers.remove(provider_id);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_for_test(provider_id: &str, model_id: &str, limits: ModelLimits) {
    // In memory only: a test must never write the developer's own layer.
    if let Ok(mut state) = STATE.write() {
        state
            .overrides
            .providers
            .entry(provider_id.to_owned())
            .or_default()
            .insert(model_id.to_owned(), limits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{ProviderKind, Wire};

    fn fixture(context: u64, max_output: u64, input: InputModalities) -> ModelLimits {
        ModelLimits {
            context,
            max_output,
            input,
            price: None,
        }
    }

    fn text_only() -> InputModalities {
        InputModalities {
            text: true,
            ..Default::default()
        }
    }

    fn everything() -> InputModalities {
        InputModalities {
            text: true,
            image: true,
            video: true,
            audio: true,
        }
    }

    #[test]
    fn a_known_model_keeps_its_real_window_over_a_stale_entry() {
        let mut saved = entry("gateway", "xiaomi/mimo-v2.6-pro");
        // The saved entry says 64K; the model is the 1M MiMo build.
        saved.context_length = 65_536;
        assert_eq!(context_length(&saved), 1_000_000);
    }

    #[test]
    fn an_unknown_model_falls_back_to_the_entry() {
        let mut saved = entry("vendor", "vendor/unknown");
        saved.context_length = 32_768;
        assert_eq!(context_length(&saved), 32_768);
    }

    #[test]
    fn a_vendor_prefixed_id_resolves_to_the_catalog_window() {
        let mut saved = entry("gateway", "deepseek/deepseek-flash");
        // The stored number is a legacy 64K guess; the catalog declares 1M.
        saved.context_length = 65_536;
        assert_eq!(context_length(&saved), 1_000_000);
    }

    #[test]
    fn the_bare_catalog_id_still_resolves() {
        let mut saved = entry("gateway", "deepseek-flash");
        saved.context_length = 65_536;
        assert_eq!(context_length(&saved), 1_000_000);
    }

    #[test]
    fn an_explicit_override_wins_over_the_catalog() {
        install_for_test(
            "override-fixture",
            "google/gemini-4-argon",
            fixture(200_000, 19_000, text_only()),
        );
        let saved = entry("override-fixture", "google/gemini-4-argon");
        assert_eq!(context_length(&saved), 200_000);
        clear_for_test("override-fixture", "google/gemini-4-argon");
    }

    #[test]
    fn an_unknown_vendor_prefixed_id_keeps_the_saved_window() {
        let mut saved = entry("vendor", "some-vendor/nonexistent");
        saved.context_length = 65_536;
        assert_eq!(context_length(&saved), 65_536);
    }

    #[test]
    fn a_free_suffix_resolves_the_same_as_its_base() {
        let base = entry("vendor", "deepseek-flash");
        let tagged = entry("vendor", "deepseek-flash:free");
        assert_eq!(context_length(&tagged), context_length(&base));
        assert_eq!(context_length(&tagged), 1_000_000);
        let both = entry("vendor", "deepseek/deepseek-flash:free");
        assert_eq!(context_length(&both), 1_000_000);
    }

    #[test]
    fn the_auto_compact_threshold_follows_the_catalog_window() {
        let mut saved = entry("gateway", "deepseek/deepseek-flash");
        saved.context_length = 65_536;
        let window = context_length(&saved);
        let threshold = (window as f64 * crate::ai::sessions::AUTO_COMPACT_FRACTION) as u64;
        assert_eq!(threshold, 750_000, "75% of the catalog's 1M window");
    }

    fn entry(provider_id: &str, model: &str) -> AiProviderEntry {
        AiProviderEntry {
            id: provider_id.into(),
            name: "Third-party API".into(),
            kind: ProviderKind::OpenAiChatCompletions,
            wire: Some(Wire::OpenAiChatCompletions),
            auth: Default::default(),
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: "https://models.example/v1/chat/completions".into(),
            model: model.into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }

    fn overrides(provider_id: &str, model_id: &str, limits: ModelLimits) -> Overrides {
        let mut overrides = Overrides::default();
        overrides
            .providers
            .entry(provider_id.into())
            .or_default()
            .insert(model_id.into(), limits);
        overrides
    }

    fn temp_path(name: &str) -> PathBuf {
        oc_test_support::temp_path("opencompose-limits", name)
    }

    /// A file that exists, so the gate checks the modality rather than the path.
    fn existing_media(name: &str) -> String {
        let path = temp_path(name);
        std::fs::write(&path, b"fixture bytes").unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn override_on_a_bundled_model_wins_and_keeps_the_rest_of_the_entry() {
        let bundled = super::super::catalog::catalog();
        let base = bundled
            .providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .find(|model| model.id == "gpt-6.1-sol")
            .unwrap();
        assert_eq!(base.context, 1_050_000);
        assert_eq!(base.max_output, 128_000);

        let layer = overrides(
            "openai",
            "gpt-6.1-sol",
            ModelLimits {
                context: 200_000,
                max_output: 1_234,
                input: InputModalities {
                    text: true,
                    image: true,
                    ..Default::default()
                },
                price: Some(Price {
                    input: 1.5,
                    output: 6.0,
                    cached_input: Some(0.5),
                    currency: "USD".into(),
                }),
            },
        );
        let merged = merged_catalog(bundled, &layer);
        let model = merged
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .unwrap()
            .models
            .iter()
            .find(|model| model.id == "gpt-6.1-sol")
            .unwrap();
        assert_eq!(model.context, 200_000);
        assert_eq!(model.max_output, 1_234);
        assert_eq!(model.inputs(), everything_without_video());
        assert_eq!(model.price.as_ref().unwrap().output, 6.0);
        assert!(model.capabilities.text && !model.capabilities.audio_in);
        assert_eq!(model.name, base.name, "the declared name is preserved");
    }

    fn everything_without_video() -> InputModalities {
        InputModalities {
            text: true,
            image: true,
            ..Default::default()
        }
    }

    #[test]
    fn unknown_model_id_becomes_a_listed_usable_entry() {
        let bundled = super::super::catalog::catalog();
        let layer = overrides(
            "custom",
            "third-party-7b",
            fixture(64_000, 4_096, everything()),
        );
        let merged = merged_catalog(bundled, &layer);
        let provider = merged
            .providers
            .iter()
            .find(|provider| provider.id == "custom")
            .unwrap();
        let model = provider
            .models
            .iter()
            .find(|model| model.id == "third-party-7b")
            .expect("the override creates the listing entry");
        assert_eq!(model.name, "third-party-7b");
        assert_eq!(model.context, 64_000);
        assert_eq!(model.max_output, 4_096);
        assert_eq!(model.status, "active");
        assert_eq!(model.inputs(), everything());
        assert!(model.capabilities.streaming);
        let serialized = serde_json::to_string(&merged).unwrap();
        assert!(serialized.contains("third-party-7b"));
    }

    #[test]
    fn a_catalog_without_overrides_matches_the_bundled_listing() {
        let bundled = super::super::catalog::catalog();
        let merged = merged_catalog(bundled, &Overrides::default());
        assert_eq!(
            serde_json::to_value(bundled).unwrap(),
            serde_json::to_value(&merged).unwrap()
        );
        let counts = |catalog: &Catalog| {
            catalog
                .providers
                .iter()
                .map(|provider| (provider.id.clone(), provider.models.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(counts(bundled), counts(&merged));
    }

    #[test]
    fn each_rejected_field_names_itself() {
        let cases = [
            (fixture(0, 10, text_only()), "providers.custom.m.context"),
            (
                fixture(100, 0, text_only()),
                "providers.custom.m.max_output",
            ),
            (
                fixture(100, 101, text_only()),
                "providers.custom.m.max_output",
            ),
            (
                fixture(100, 10, InputModalities::default()),
                "providers.custom.m.input",
            ),
        ];
        for (limits, field) in cases {
            let error = limits.validate("providers.custom.m").unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
        let negative = ModelLimits {
            price: Some(Price {
                input: -1.0,
                output: 1.0,
                cached_input: None,
                currency: "USD".into(),
            }),
            ..fixture(100, 10, text_only())
        };
        assert!(
            negative
                .validate("providers.custom.m")
                .unwrap_err()
                .contains("price.input")
        );
        let bad_currency = ModelLimits {
            price: Some(Price {
                input: 1.0,
                output: 2.0,
                cached_input: None,
                currency: "usd".into(),
            }),
            ..fixture(100, 10, text_only())
        };
        assert!(
            bad_currency
                .validate("providers.custom.m")
                .unwrap_err()
                .contains("price.currency")
        );
    }

    #[test]
    fn a_price_without_a_currency_is_usd_and_a_zero_price_stays_a_price() {
        let parsed = parse(
            r#"{"providers":{"custom":{"m":{"context":100,"max_output":10,"input":{"text":true},"price":{"input":0,"output":0}}}}}"#,
        )
        .unwrap();
        let price = parsed.get("custom", "m").unwrap().price.clone().unwrap();
        assert_eq!(price.currency, "USD");
        assert_eq!((price.input, price.output), (0.0, 0.0));
        // The default is written back only when it is not USD.
        let text = serde_json::to_string(&parsed).unwrap();
        assert!(!text.contains("currency"), "{text}");
    }

    #[test]
    fn unknown_keys_and_missing_fields_are_rejected_by_name() {
        let unknown = parse(
            r#"{"providers":{"custom":{"m":{"context":100,"max_output":10,"input":{"text":true},"contxt":200}}}}"#,
        )
        .unwrap_err();
        assert!(unknown.contains("unknown field"), "{unknown}");
        assert!(unknown.contains("contxt"), "{unknown}");
        let missing =
            parse(r#"{"providers":{"custom":{"m":{"context":100,"max_output":10}}}}"#).unwrap_err();
        assert!(missing.contains("missing field `input`"), "{missing}");
        let bad_modality =
            parse(r#"{"providers":{"custom":{"m":{"context":100,"max_output":10,"input":{"text":true,"scent":false}}}}}"#)
                .unwrap_err();
        assert!(bad_modality.contains("scent"), "{bad_modality}");
        let bad_price = parse(
            r#"{"providers":{"custom":{"m":{"context":100,"max_output":10,"input":{"text":true},"price":{"in":1,"output":2}}}}}"#,
        )
        .unwrap_err();
        assert!(bad_price.contains("unknown field"), "{bad_price}");
    }

    #[test]
    fn corrupt_file_yields_an_empty_store_and_a_warning() {
        let path = temp_path("corrupt.json");
        std::fs::write(&path, "{ not json").unwrap();
        let (overrides, warning) = load_from(&path);
        assert!(overrides.providers.is_empty());
        let warning = warning.expect("a warning is recorded");
        assert!(warning.contains("not valid"), "{warning}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_store_without_a_warning() {
        let path = temp_path("missing.json");
        let (overrides, warning) = load_from(&path);
        assert!(overrides.providers.is_empty());
        assert!(warning.is_none());
    }

    #[test]
    fn save_writes_atomically_and_round_trips() {
        let path = temp_path("dir");
        let target = path.join("models.json");
        let layer = overrides("custom", "round-trip", fixture(32_768, 2_048, text_only()));
        let text = format!("{}\n", serde_json::to_string_pretty(&layer).unwrap());
        crate::private_fs::write_atomic(&target, text.as_bytes()).unwrap();
        let (loaded, warning) = load_from(&target);
        assert!(warning.is_none());
        assert_eq!(loaded, layer);
        let leftovers = std::fs::read_dir(&path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "models.json")
            .count();
        assert_eq!(leftovers, 0, "the temporary file is renamed");
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn a_failed_write_leaves_no_layer_in_memory() {
        let dir = temp_path("write-fails");
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the file belongs fails every replace-by-rename.
        let target = dir.join("models.json");
        std::fs::create_dir_all(&target).unwrap();
        let _guard = test_path(target.clone());
        let error = put(
            "write-failure",
            "write-failure-model",
            fixture(32_000, 4_096, text_only()),
        )
        .unwrap_err();
        assert!(error.contains("models.json"), "{error}");
        // The panel reported a failed save, so nothing may apply this session.
        assert!(get("write-failure", "write-failure-model").is_none());
        // Once the path is usable the same write lands and is installed.
        std::fs::remove_dir_all(&target).unwrap();
        put(
            "write-failure",
            "write-failure-model",
            fixture(32_000, 4_096, text_only()),
        )
        .unwrap();
        assert_eq!(
            get("write-failure", "write-failure-model").unwrap().context,
            32_000
        );
        remove("write-failure", "write-failure-model").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_write_without_an_installed_test_path_is_refused() {
        // Hold the redirect lock: no other test may install a path while this
        // one proves that a missing guard is refused.
        let lock = TEST_PATH_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            TEST_PATH
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "a previous test left its redirect installed"
        );
        let error = put(
            "no-guard",
            "no-guard-model",
            fixture(4_000, 100, text_only()),
        )
        .expect_err("a test write without an installed path is refused");
        assert!(error.contains("install limits::test_path first"), "{error}");
        assert!(get("no-guard", "no-guard-model").is_none());
        drop(lock);

        // The same write lands once the test installs a path, so the refusal is
        // about the missing guard and not about the write itself.
        let dir = temp_path("guard-installed");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("models.json");
        let _guard = test_path(target);
        put(
            "no-guard",
            "no-guard-model",
            fixture(4_000, 100, text_only()),
        )
        .unwrap();
        assert_eq!(get("no-guard", "no-guard-model").unwrap().context, 4_000);
        remove("no-guard", "no-guard-model").unwrap();
        drop(_guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One guarded suite: writes one model to the file it installs and checks
    /// that the other suite's file never received it.
    fn run_guarded_suite(target: &Path, other: &Path, provider: &str, model: &str, context: u64) {
        let _guard = test_path(target.to_path_buf());
        put(provider, model, fixture(context, 100, text_only())).unwrap();
        let saved = parse(&std::fs::read_to_string(target).unwrap()).unwrap();
        assert_eq!(
            saved.get(provider, model).map(|limits| limits.context),
            Some(context),
            "{provider} writes to the file it installed"
        );
        if other.exists() {
            assert!(
                !std::fs::read_to_string(other).unwrap().contains(model),
                "the other suite's file stays untouched"
            );
        }
        // Leave the process-global layer clean for the next suite.
        remove(provider, model).unwrap();
    }

    #[test]
    fn two_suites_in_sequence_each_see_their_own_file() {
        let first_dir = temp_path("suite-first");
        let second_dir = temp_path("suite-second");
        std::fs::create_dir_all(&first_dir).unwrap();
        std::fs::create_dir_all(&second_dir).unwrap();
        let first = first_dir.join("models.json");
        let second = second_dir.join("models.json");
        run_guarded_suite(&first, &second, "first-suite", "first-model", 1_000);
        assert!(
            TEST_PATH
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none(),
            "the suite cleared the redirect when its guard dropped"
        );
        run_guarded_suite(&second, &first, "second-suite", "second-model", 2_000);
        let first_saved = parse(&std::fs::read_to_string(&first).unwrap()).unwrap();
        assert!(first_saved.get("second-suite", "second-model").is_none());
        let second_saved = parse(&std::fs::read_to_string(&second).unwrap()).unwrap();
        assert!(second_saved.get("first-suite", "first-model").is_none());
        std::fs::remove_dir_all(&first_dir).unwrap();
        std::fs::remove_dir_all(&second_dir).unwrap();
    }

    #[test]
    fn a_saved_override_is_applied_on_the_next_read() {
        install_for_test(
            "limits-fixture",
            "fixture-limit-model",
            fixture(8_000, 1_234, text_only()),
        );
        let entry = entry("limits-fixture", "fixture-limit-model");
        assert_eq!(max_output(&entry), 1_234);
        assert!(
            super::overrides()
                .get("limits-fixture", "fixture-limit-model")
                .is_some()
        );
    }

    #[test]
    fn gate_refuses_a_modality_the_merged_model_does_not_declare() {
        install_for_test(
            "gate-fixture",
            "text-only-model",
            fixture(100_000, 4_096, text_only()),
        );
        let entry = entry("gate-fixture", "text-only-model");
        let path = existing_media("gate.wav");
        let messages = serde_json::json!([
            {"role":"user","content":"transcribe this",
             "media":[{"kind":"audio","name":"take.wav","path":path}]}
        ]);
        let error = gate_input(&entry, &messages).unwrap_err();
        assert!(
            error.message.contains("does not accept audio"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("text-only-model"),
            "{}",
            error.message
        );
        // A file that no longer exists is skipped by the request builder, so the
        // gate does not refuse the rest of the message for it.
        let missing = serde_json::json!([
            {"role":"user","content":"keep going",
             "media":[{"kind":"audio","name":"gone.wav","path":"/no/such/opencompose/file.wav"}]}
        ]);
        assert!(gate_input(&entry, &missing).is_ok());
        assert!(gate_input(&entry, &serde_json::json!([{"role":"user","content":"hi"}])).is_ok());
    }

    #[test]
    fn a_historical_attachment_the_model_cannot_take_becomes_a_text_line() {
        install_for_test(
            "lower-fixture",
            "text-only-later",
            fixture(100_000, 4_096, text_only()),
        );
        let entry = entry("lower-fixture", "text-only-later");
        let image = existing_media("lower.png");
        let mut messages = serde_json::json!([
            {"role":"user","content":"what is in this mix?",
             "media":[{"kind":"image","name":"mix.png","path":image}]},
            {"role":"assistant","content":"It looks like a house mix."},
            {"role":"user","content":"add a kick"}
        ]);
        assert_eq!(lower_unaccepted_media(&entry, &mut messages), 1);
        // The old turn keeps its text and gains one line naming the file.
        assert!(messages[0].get("media").is_none());
        let content = messages[0]["content"].as_str().unwrap();
        assert!(content.contains("what is in this mix?"), "{content}");
        assert!(content.contains("mix.png"), "{content}");
        assert!(content.contains("does not accept image input"), "{content}");
        // The turn's own attachment is the composer's: it still refuses.
        assert!(gate_input(&entry, &messages).is_ok());
        let current = serde_json::json!([
            {"role":"user","content":"look",
             "media":[{"kind":"image","name":"new.png","path":existing_media("new.png")}]}
        ]);
        let mut untouched = current.clone();
        assert_eq!(lower_unaccepted_media(&entry, &mut untouched), 0);
        assert_eq!(untouched, current);
        assert!(gate_input(&entry, &current).is_err());
    }

    #[test]
    fn a_supported_historical_attachment_is_left_alone() {
        install_for_test(
            "lower-all-fixture",
            "all-input-later",
            fixture(100_000, 4_096, everything()),
        );
        let entry = entry("lower-all-fixture", "all-input-later");
        let audio = existing_media("keep.wav");
        let mut messages = serde_json::json!([
            {"role":"user","content":"listen",
             "media":[{"kind":"audio","name":"take.wav","path":audio}]},
            {"role":"user","content":"again"}
        ]);
        assert_eq!(lower_unaccepted_media(&entry, &mut messages), 0);
        assert_eq!(messages[0]["media"].as_array().unwrap().len(), 1);
        assert!(
            !messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("was not sent")
        );
    }

    #[test]
    fn gate_accepts_a_modality_the_merged_model_declares() {
        install_for_test(
            "gate-all-fixture",
            "all-input-model",
            fixture(100_000, 4_096, everything()),
        );
        let entry = entry("gate-all-fixture", "all-input-model");
        let path = existing_media("gate-all.wav");
        let messages = serde_json::json!([
            {"role":"user","content":"listen",
             "media":[{"kind":"audio","name":"take.wav","path":path}]}
        ]);
        assert!(gate_input(&entry, &messages).is_ok());
        let image = serde_json::json!([{"role":"user","content":[
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}
        ]}]);
        assert!(gate_input(&entry, &image).is_ok());
        let video = serde_json::json!([{"role":"user","content":[{"type":"video_url"}]}]);
        assert!(gate_input(&entry, &video).is_ok());
    }

    #[test]
    fn gate_uses_the_saved_capabilities_when_no_catalog_entry_exists() {
        let mut entry = entry("plain-fixture", "unknown-model");
        entry.capabilities.audio_in = false;
        let messages = serde_json::json!([
            {"role":"user","content":"","media":[{"kind":"audio","name":"a.wav"}]}
        ]);
        assert!(gate_input(&entry, &messages).is_err());
        entry.capabilities.audio_in = true;
        assert!(gate_input(&entry, &messages).is_ok());
    }

    #[test]
    fn the_audio_gate_reads_the_merged_capability() {
        // A provider id the bundled catalog knows: the merged listing carries the
        // override, so a gate that reads catalog::model sees it.
        install_for_test(
            "custom",
            "audio-gate-bundled",
            fixture(128_000, 4_096, text_only()),
        );
        let merged =
            super::super::catalog::model(Wire::OpenAiChatCompletions, "audio-gate-bundled")
                .unwrap();
        assert!(
            !merged.capabilities.audio_in,
            "capabilities.audio_in is the merged audio flag"
        );
        assert!(!merged.inputs().audio);

        // Any provider id, including saved ids that differ from catalog ids: the
        // gate must read this helper, not the wire-level catalog lookup.
        install_for_test(
            "audio-gate-fixture",
            "silent-model",
            fixture(128_000, 4_096, text_only()),
        );
        let silent = entry("audio-gate-fixture", "silent-model");
        assert!(!modality_allowed(&silent, InputModality::Audio));
        install_for_test(
            "audio-gate-fixture",
            "hearing-model",
            fixture(128_000, 4_096, everything()),
        );
        let hearing = entry("audio-gate-fixture", "hearing-model");
        assert!(modality_allowed(&hearing, InputModality::Audio));
    }
}
