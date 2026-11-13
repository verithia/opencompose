//! AI editing commands and provider configuration file I/O.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;
use zeroize::Zeroize;

use crate::ai::project_instructions::{self, ProjectInstructions};
use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope};

use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};

/// Build AI context for editing a subtree.
#[tauri::command]
pub fn ai_build_context(
    target_path: String,
    user_instruction: String,
    state: State<AppState>,
) -> Result<oc_dsl::ai::AiContext, String> {
    let inner = state.lock_inner();
    oc_dsl::ai::build_context(&inner.project, &target_path, &user_instruction)
}

/// Build AI context for generating new content.
#[tauri::command]
pub fn ai_build_generate_context(
    what: String,
    user_instruction: String,
    state: State<AppState>,
) -> Result<oc_dsl::ai::AiContext, String> {
    let inner = state.lock_inner();
    oc_dsl::ai::build_generate_context(&inner.project, &what, &user_instruction)
}

/// Validate AI model output.
#[tauri::command]
pub fn ai_validate_edit(
    new_dsl: String,
    target_path: String,
    state: State<AppState>,
) -> Result<oc_dsl::ai::ValidationResult, String> {
    let inner = state.lock_inner();
    Ok(validate_edit_in_project(&inner, new_dsl, target_path))
}

/// Validate an edit and the media it declares against the project directory.
///
/// The semantic validator does not know the DSP factory, so a value it accepts
/// can still make the render fail. The check below runs that factory, and only
/// refuses what the edit *adds*: an effect the project already had wrong must
/// not block an unrelated replacement.
pub fn validate_edit_in_project(
    inner: &crate::state::AppStateInner,
    new_dsl: String,
    target_path: String,
) -> oc_dsl::ai::ValidationResult {
    let edit = oc_dsl::ai::AiEdit {
        new_dsl,
        target_path,
        explanation: None,
    };
    let root = super::shared::project_root(inner);
    let mut result = oc_dsl::ai::validate_edit_in(&edit, &inner.project, root.as_deref());
    if !result.valid {
        return result;
    }
    let Ok(candidate) = oc_dsl::ai::candidate_project(&edit, &inner.project) else {
        return result;
    };
    let existing = oc_engine::graph::validate_effect_parameters(&inner.project, 48_000);
    let introduced: Vec<String> = oc_engine::graph::validate_effect_parameters(&candidate, 48_000)
        .into_iter()
        .filter(|error| !existing.contains(error))
        .collect();
    if !introduced.is_empty() {
        result.valid = false;
        result.parsed_dsl = None;
        result.errors.extend(introduced);
    }
    result
}

/// Apply a validated AI edit to the project.
/// The edit is recorded in the undo stack as a patch with a descriptive label.
#[tauri::command]
pub fn ai_apply_edit(
    new_dsl: String,
    target_path: String,
    expected_dsl: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    apply_edit_to_project(&mut inner, new_dsl, target_path, expected_dsl)
}

/// Apply an edit atomically: nothing changes when it fails validation.
pub fn apply_edit_to_project(
    inner: &mut crate::state::AppStateInner,
    new_dsl: String,
    target_path: String,
    expected_dsl: String,
) -> Result<ProjectSnapshot, String> {
    // Applying straight from a proposal would land on whichever preview side is
    // on screen; the preview must be accepted or declined first.
    inner.refuse_in_preview()?;
    if inner.dsl_source != expected_dsl {
        return Err(
            "Project changed since this proposal. Ask the assistant to regenerate it.".into(),
        );
    }
    let old_project = inner.project.clone();
    let edit = oc_dsl::ai::AiEdit {
        new_dsl,
        target_path: target_path.clone(),
        explanation: None,
    };
    let mut candidate = inner.project.clone();
    let root = super::shared::project_root(inner);
    let result = oc_dsl::ai::apply_edit_in(&mut candidate, &edit, root.as_deref())?;
    inner.project = candidate;

    // Use resync_with_undo for standard undo push + audio preload
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("AI edit", UndoScope::Project),
    )?;
    // Override DSL with AI result text (preserves subtree formatting)
    inner.dsl_source = result.dsl_text;
    // Keep a recovery copy of every applied edit: a process killed between
    // timer ticks must not lose the turn's work.
    crate::autosave::flush_dirty(inner);
    // The file is the durable record; the recovery copy is only the crash net.
    // A batch in progress is published by its commit instead.
    // An applied edit the file did not take is a failure the operator has to
    // see; the turn continues so its remaining edits are not refused.
    if let Err(reason) = crate::project_persist::flush(inner) {
        log::error!("[ai] applied edit not written to the project file: {reason}");
    }
    Ok(snapshot(inner))
}

/// Commits a batch a panel turn left open, e.g. when the process that ran the
/// turn died between its edits and its commit. The edits stay applied, as one
/// undo entry; the saved conversation marks the calls that never reported.
/// Returns the published document, or `None` when no panel batch was open.
///
/// Only a panel-owned batch is recovered: a batch an external client opened is
/// that client's to finish, so it is left alone. Calling this with no open batch
/// and calling it twice are both no-ops.
pub fn recover_panel_batch(state: &AppState) -> Result<Option<ProjectSnapshot>, String> {
    let Some(owner) = state.batch_owner() else {
        return Ok(None);
    };
    if !owner.starts_with(crate::state::PANEL_BATCH_OWNER_PREFIX) {
        return Ok(None);
    }
    state.commit_transaction()?;
    if let Err(reason) = crate::project_persist::flush_state(state) {
        log::error!("[ai] recovered batch not written to the project file: {reason}");
    }
    let inner = state.lock_inner();
    Ok(Some(snapshot(&inner)))
}

/// The panel calls this once on mount. A turn still running on the backend
/// owns its batch and commits it itself, so recovery waits for it to end.
#[tauri::command]
pub fn ai_batch_recover(
    state: State<AppState>,
    requests: State<crate::ai_service::Requests>,
) -> Result<Option<ProjectSnapshot>, String> {
    // Recovery holds the request slot, so no turn can open a batch between
    // the check and the commit.
    let Some(claim) = requests.claim(RECOVER_SLOT)? else {
        return Ok(None);
    };
    let recovered = recover_panel_batch(&state);
    let _ = requests.release_owned(RECOVER_SLOT, &claim.identity);
    recovered
}

/// The request slot batch recovery holds while it commits.
const RECOVER_SLOT: &str = "panel-batch-recover";

/// Extract a subtree as DSL text (for inspection / AI context).
#[tauri::command]
pub fn extract_subtree(
    path: String,
    state: State<AppState>,
) -> Result<oc_dsl::ops::SubtreeExtract, String> {
    let inner = state.lock_inner();
    oc_dsl::ops::extract(&inner.project, &path)
}

/// Diff old vs new DSL text.
#[tauri::command]
pub fn diff_dsl(old_dsl: String, new_dsl: String) -> oc_dsl::ai::DslDiff {
    oc_dsl::ai::diff(&old_dsl, &new_dsl)
}

/// The note a project adds to the system prompt, empty when it has none.
#[tauri::command]
pub fn ai_project_instructions_get(
    project: Option<String>,
    state: State<AppState>,
) -> Result<ProjectInstructions, String> {
    let open = state.lock_inner().file_path.clone();
    instructions_get(project.as_deref(), open.as_deref())
}

/// Writes the project's note, or clears it with blank text.
#[tauri::command]
pub fn ai_project_instructions_set(
    project: Option<String>,
    text: String,
    state: State<AppState>,
) -> Result<ProjectInstructions, String> {
    let open = state.lock_inner().file_path.clone();
    instructions_set(project.as_deref(), open.as_deref(), &text)
}

/// The project a note belongs to: the path the request named, else the open one.
fn note_project(project: Option<&str>, open: Option<&Path>) -> Option<PathBuf> {
    project
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| open.map(Path::to_path_buf))
}

/// Reads a project's note; a project with no path has none to read.
pub fn instructions_get(
    project: Option<&str>,
    open: Option<&Path>,
) -> Result<ProjectInstructions, String> {
    let Some(path) = note_project(project, open) else {
        return Ok(ProjectInstructions::none());
    };
    project_instructions::Store::config().load(&path)
}

/// Writes a project's note. A project that was never saved has no path to key
/// the note by, so it refuses instead of guessing a directory.
pub fn instructions_set(
    project: Option<&str>,
    open: Option<&Path>,
    text: &str,
) -> Result<ProjectInstructions, String> {
    let Some(path) = note_project(project, open) else {
        return Err(
            "this project has no file yet; save the project first, then write its instructions"
                .into(),
        );
    };
    project_instructions::Store::config().save(&path, text)
}

// ============================================================================
// AI Provider Config (YAML file I/O)
// ============================================================================

/// AI provider configuration — matches the frontend AiConfig type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AiProviderConfig {
    #[serde(default = "default_config_version")]
    pub version: u32,
    pub active_provider: String,
    pub providers: Vec<AiProviderEntry>,
    /// Entries the catalog cannot describe, from the last load or save. Read-only
    /// for the panel: never written to the file, and re-reported on every save.
    #[serde(default, skip_serializing)]
    pub warnings: Vec<crate::ai_service::ProviderWarning>,
}

/// Legacy saved protocol selector. Configs written before the catalog carried
/// wires name their protocol here; `normalize_ai_config` copies it into `wire`
/// once and keeps this field as a mirror so an older build can still read the
/// file. Nothing selects an adapter from this value.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    #[default]
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "gemini_interactions")]
    GeminiInteractions,
    #[serde(rename = "gemini_generate")]
    GeminiGenerate,
    #[serde(other)]
    Unknown,
}

impl ProviderKind {
    /// Resolve unknown to the default protocol.
    pub fn normalized(self) -> Self {
        match self {
            Self::Unknown => Self::OpenAiChatCompletions,
            other => other,
        }
    }

    /// Catalog protocol id for a config saved before entries carried a wire.
    pub fn wire(self) -> Wire {
        match self {
            Self::OpenAiChatCompletions => Wire::OpenAiChatCompletions,
            Self::OpenAiResponses => Wire::OpenAiResponses,
            Self::AnthropicMessages => Wire::AnthropicMessages,
            Self::GeminiInteractions => Wire::GeminiInteractions,
            Self::GeminiGenerate => Wire::GeminiGenerate,
            Self::Unknown => Wire::Unknown,
        }
    }
}

/// Protocol id from the bundled catalog. Adapters are selected from this value,
/// so serving an existing wire format from a new gateway is a catalog edit.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    #[serde(rename = "openai-chat-completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "gemini-interactions")]
    GeminiInteractions,
    #[serde(rename = "gemini-generate")]
    GeminiGenerate,
    #[serde(other)]
    Unknown,
}

impl Wire {
    /// Resolve unknown to the default wire.
    pub fn normalized(self) -> Self {
        match self {
            Self::Unknown => Self::OpenAiChatCompletions,
            other => other,
        }
    }

    /// Saved behaviour enum for a known wire, kept as the legacy mirror.
    pub fn kind(self) -> Option<ProviderKind> {
        Some(match self {
            Self::OpenAiChatCompletions => ProviderKind::OpenAiChatCompletions,
            Self::OpenAiResponses => ProviderKind::OpenAiResponses,
            Self::AnthropicMessages => ProviderKind::AnthropicMessages,
            Self::GeminiInteractions => ProviderKind::GeminiInteractions,
            Self::GeminiGenerate => ProviderKind::GeminiGenerate,
            Self::Unknown => return None,
        })
    }

    /// Request path a gateway publishes for this protocol, relative to the
    /// provider's base URL. A catalog entry may override it for a gateway that
    /// mounts the same protocol elsewhere.
    pub fn default_path(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "chat/completions",
            Self::OpenAiResponses => "responses",
            Self::AnthropicMessages => "messages",
            Self::GeminiInteractions => "interactions",
            Self::GeminiGenerate => "models/{model}:streamGenerateContent",
            Self::Unknown => "",
        }
    }

    /// Query pairs the protocol adds to every request. generateContent streams
    /// only when asked for SSE.
    pub fn default_query(self) -> Vec<(String, String)> {
        match self {
            Self::GeminiGenerate => vec![("alt".into(), "sse".into())],
            _ => Vec::new(),
        }
    }

    /// Operator-facing protocol name, for lines that name what a model belongs to.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GeminiInteractions => "Gemini Interactions",
            Self::GeminiGenerate => "Gemini generateContent",
            Self::Unknown => "this protocol",
        }
    }
}

/// Auth path for a provider entry.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    #[default]
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
    #[serde(other)]
    Unknown,
}

impl AuthKind {
    /// Resolve unknown to API-key authentication.
    pub fn normalized(self) -> Self {
        match self {
            Self::Unknown => Self::ApiKey,
            other => other,
        }
    }
}

/// The OAuth application a token belongs to. Selects its endpoints, refresh
/// implementation and endpoint host allowlist.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthProvider {
    #[serde(rename = "openai_chat_gpt")]
    OpenAiChatGpt,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini_cli")]
    GeminiCli,
    #[serde(rename = "xai")]
    Xai,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AuthProvider {
    /// Wire spelling of the OAuth provider.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatGpt => "openai_chat_gpt",
            Self::Anthropic => "anthropic",
            Self::GeminiCli => "gemini_cli",
            Self::Xai => "xai",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this provider has an OAuth implementation.
    pub fn supported(self) -> bool {
        matches!(self, Self::OpenAiChatGpt)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
/// One configured AI provider.
pub struct AiProviderEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    /// Protocol this entry speaks. Written by the catalog when the entry is
    /// created and filled from the legacy `kind` once when an older config is
    /// read; this value alone selects the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<Wire>,
    #[serde(default)]
    pub auth: AuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_provider: Option<AuthProvider>,
    /// Extra exact hosts this entry may send an OAuth token to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oauth_allowed_hosts: Vec<String>,
    pub endpoint: String,
    pub model: String,
    /// Extra model ids this provider offers; the first is the default `model`.
    /// Absent in configs saved before the add-provider form, so an empty list
    /// keeps an existing file byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default = "default_context_length")]
    pub context_length: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Effort sent to protocols that declare thinking levels for the model.
    #[serde(default = "default_thinking_level")]
    pub thinking_level: String,
    #[serde(default)]
    pub capabilities: AiCapabilities,
}

impl AiProviderEntry {
    /// The protocol this entry speaks. `normalize_ai_config` guarantees this is
    /// set for every config read or saved; the fallback only covers a value
    /// built in memory without one, and never consults the legacy `kind`.
    pub fn wire(&self) -> Wire {
        self.wire.unwrap_or(Wire::OpenAiChatCompletions)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
/// Capabilities reported for a provider.
pub struct AiCapabilities {
    #[serde(default = "default_true")]
    pub text: bool,
    #[serde(default)]
    pub audio_in: bool,
    #[serde(default)]
    pub audio_out: bool,
    #[serde(default)]
    pub native_tool_calling: bool,
    #[serde(default)]
    pub streaming: bool,
}

fn default_config_version() -> u32 {
    1
}
/// A freshly added provider starts at the same budget the panel's model
/// defaults use: a 1M window and a 16K reply. This is the window for a model
/// the catalog cannot describe, so it must not shrink to a smaller guess: a
/// saved provider entry that stores a smaller number still gets the catalog
/// window whenever the catalog resolves its model.
fn default_context_length() -> u64 {
    1_000_000
}
fn default_max_tokens() -> u64 {
    16_000
}
fn default_temperature() -> f64 {
    0.3
}
fn default_thinking_level() -> String {
    crate::ai::catalog::DEFAULT_THINKING_LEVEL.to_owned()
}
fn default_true() -> bool {
    true
}

fn ai_config_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = crate::ai_service::config_path_override() {
        return path;
    }
    crate::private_fs::config_dir().join("ai-providers.yaml")
}

pub(crate) fn parse_ai_config(text: &str) -> Result<AiProviderConfig, String> {
    let mut config: AiProviderConfig =
        serde_yaml::from_str(text).map_err(|e| format!("Failed to parse YAML: {}", e))?;
    drop_retired_providers(&mut config, text);
    Ok(config)
}

/// Protocol and OAuth tags of integrations this build no longer ships. The
/// shared enums read them as `Unknown`, which would leave a broken entry that
/// still points at the retired host, so the loader recognizes them here, at the
/// file adapter, and nowhere else.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
enum RetiredTag {
    #[serde(rename = "antigravity")]
    Antigravity,
    #[serde(rename = "github_copilot")]
    GitHubCopilot,
    #[serde(other)]
    Current,
}

/// The fields of a saved entry that can carry a retired tag.
#[derive(Deserialize)]
struct RetiredProbe {
    #[serde(default)]
    kind: Option<RetiredTag>,
    #[serde(default)]
    wire: Option<RetiredTag>,
    #[serde(default)]
    auth_provider: Option<RetiredTag>,
}

impl RetiredProbe {
    fn retired(&self) -> bool {
        [self.kind, self.wire, self.auth_provider]
            .into_iter()
            .flatten()
            .any(|tag| tag != RetiredTag::Current)
    }
}

#[derive(Deserialize)]
struct RetiredProbeConfig {
    #[serde(default)]
    providers: Vec<RetiredProbe>,
}

/// Drops entries of retired integrations so the rest of the file still loads.
/// The entry is never handed to a request, so its stored OAuth token is not
/// read again. The next save writes the file without it.
fn drop_retired_providers(config: &mut AiProviderConfig, text: &str) {
    let Ok(probe) = serde_yaml::from_str::<RetiredProbeConfig>(text) else {
        return;
    };
    if probe.providers.len() != config.providers.len() {
        return;
    }
    let mut retired = probe.providers.iter().map(RetiredProbe::retired);
    let mut dropped_active = false;
    let active = config.active_provider.clone();
    config.providers.retain(|entry| {
        let drop = retired.next().unwrap_or(false);
        if drop {
            log::warn!(
                "[ai] ignoring saved provider '{}': its integration is no longer available",
                entry.id
            );
            dropped_active |= entry.id == active;
        }
        !drop
    });
    // The active id must name a listed entry, so it moves to the first one left.
    if dropped_active {
        config.active_provider = config
            .providers
            .first()
            .map(|entry| entry.id.clone())
            .unwrap_or_default();
    }
}

/// Schema v1 → v2 (and the wire split): resolve the protocol once, then keep
/// `kind` as a mirror for older builds. Ids and endpoints are untouched, so the
/// keychain account (`id:sha256(endpoint)`) still resolves.
pub(crate) fn normalize_ai_config(config: &mut AiProviderConfig) {
    config.version = 2;
    for provider in &mut config.providers {
        provider.kind = provider.kind.normalized();
        let wire = provider.wire.unwrap_or_else(|| provider.kind.wire());
        provider.wire = Some(wire);
        if let Some(kind) = wire.kind() {
            provider.kind = kind;
        }
        provider.auth = provider.auth.normalized();
        crate::ai::catalog::heal_renamed_model(provider);
    }
}

#[tauri::command]
pub fn load_ai_config() -> Result<AiProviderConfig, String> {
    let path = ai_config_path();
    if !path.exists() {
        // Return empty config — frontend will merge with defaults
        return Ok(AiProviderConfig {
            version: 2,
            active_provider: String::new(),
            providers: vec![],
            warnings: Vec::new(),
        });
    }
    crate::private_fs::repair_file_mode(&path);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let mut config = parse_ai_config(&text)?;
    normalize_ai_config(&mut config);
    config.warnings = match crate::ai_service::validate_config(&config) {
        Ok(warnings) => warnings,
        Err(error) => {
            // A structurally broken file still loads: the panel can show and fix
            // it, and the next save names the problem.
            log::warn!("[ai] the saved config is not valid: {error}");
            Vec::new()
        }
    };
    log::info!("Loaded AI config from {}", path.display());
    Ok(config)
}

/// What a save wrote and which entries the panel should mention. Warnings never
/// fail a save: the catalog is metadata, and a stale id must not block changes.
#[derive(Debug, Serialize)]
pub struct SaveResult {
    pub path: String,
    pub warnings: Vec<crate::ai_service::ProviderWarning>,
}

#[tauri::command]
pub fn save_ai_config(config: AiProviderConfig) -> Result<SaveResult, String> {
    save_ai_config_with(
        config,
        crate::private_fs::write_atomic,
        &crate::ai::credential_lifecycle::KeyringSink,
    )
}

/// The file is the source of truth: credentials the new settings no longer
/// reference are pruned only once the file is on disk, so a failed save can
/// never leave a listed provider without its key.
pub(crate) fn save_ai_config_with(
    mut config: AiProviderConfig,
    write: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
    sink: &dyn crate::ai::credential_lifecycle::CredentialSink,
) -> Result<SaveResult, String> {
    normalize_ai_config(&mut config);
    let warnings = crate::ai_service::validate_config(&config)?;
    let previous = match load_ai_config() {
        Ok(previous) => previous.providers,
        Err(error) => {
            log::warn!("[ai] could not read the previous config for credential cleanup: {error}");
            Vec::new()
        }
    };
    let path = ai_config_path();
    let yaml =
        serde_yaml::to_string(&config).map_err(|e| format!("Failed to serialize YAML: {}", e))?;
    if write(&path, yaml.as_bytes()).is_err() {
        return Err(
            "Failed to atomically save AI settings; existing settings were retained.".into(),
        );
    }
    // A key that outlives its provider is invisible; a provider without its key
    // is a broken connection, so an obsolete credential is not worth failing.
    if let Err(error) =
        crate::ai::credential_lifecycle::delete_stale_with(sink, &previous, &config.providers)
    {
        log::warn!("[ai] saved the settings but could not remove an obsolete credential: {error}");
    }
    log::info!("Saved AI config to {}", path.display());
    Ok(SaveResult {
        path: path.display().to_string(),
        warnings,
    })
}

// ============================================================================
// Adding a provider from the panel
// ============================================================================

/// The add-provider modal's payload. It carries no secret: an API key goes to
/// the OS credential store through `ai_set_key` and never into this file.
#[derive(Debug, Clone, Deserialize)]
pub struct AiProviderDraft {
    pub name: String,
    /// Protocol the new provider speaks. The modal picks the catalog id, so a
    /// gateway that serves an existing wire needs no new code.
    pub wire: Wire,
    pub endpoint: String,
    pub auth: AuthKind,
    /// Model ids in the order the modal listed them; the first becomes `model`.
    pub models: Vec<String>,
    /// Per-model settings for the enabled ids, written to the existing user layer.
    #[serde(default)]
    pub limits: BTreeMap<String, crate::ai::limits::ModelLimits>,
}

#[derive(Debug, Serialize)]
pub struct AiAddedProvider {
    pub provider_id: String,
    pub config: AiProviderConfig,
}

const MAX_PROVIDER_NAME_CHARS: usize = 64;
const MAX_PROVIDER_MODELS: usize = 32;
const MAX_MODEL_ID_CHARS: usize = 200;

/// A stable, lowercase, hyphenated id derived from the display name. Ids key the
/// credential store and the per-model override layer, so a re-add of the same
/// name must not reuse one, and a catalog id is never taken over.
fn provider_id_from_name(name: &str, taken: &[String]) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut base: String = slug.trim_matches('-').chars().take(60).collect();
    if base.is_empty() {
        base = "provider".into();
    }
    let reserved = reserved_provider_ids();
    let mut id = base.clone();
    let mut suffix = 2;
    while taken.iter().any(|existing| existing == &id) || reserved.contains(&id.as_str()) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

/// Provider ids the catalog and the per-model override layer already key on.
/// Derived from the bundled catalog so the two cannot drift; `custom` names the
/// user's own endpoint and stays reserved even if that entry is ever removed.
fn reserved_provider_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = crate::ai::catalog::catalog()
        .providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect();
    ids.push("custom");
    ids
}

/// Builds the next config without touching the filesystem, so the rules are
/// testable and the command only has to load and save.
pub(crate) fn append_provider(
    mut config: AiProviderConfig,
    draft: AiProviderDraft,
) -> Result<AiAddedProvider, String> {
    let name = draft.name.trim();
    if name.is_empty() || name.chars().count() > MAX_PROVIDER_NAME_CHARS {
        return Err(format!(
            "Enter a provider name of 1–{MAX_PROVIDER_NAME_CHARS} characters."
        ));
    }
    if draft.auth != AuthKind::ApiKey {
        return Err("OAuth is available only for providers from the catalog".into());
    }
    let mut models: Vec<String> = Vec::new();
    for model in draft.models {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if model.chars().count() > MAX_MODEL_ID_CHARS {
            return Err(format!(
                "Model ids must be at most {MAX_MODEL_ID_CHARS} characters"
            ));
        }
        if !models.iter().any(|existing| existing == model) {
            models.push(model.to_string());
        }
    }
    if models.is_empty() {
        return Err("Add at least one model id.".into());
    }
    if models.len() > MAX_PROVIDER_MODELS {
        return Err(format!(
            "At most {MAX_PROVIDER_MODELS} model ids per provider"
        ));
    }
    for model_id in draft.limits.keys() {
        if !models.iter().any(|model| model == model_id) {
            return Err(format!(
                "Per-model settings name \"{model_id}\", which is not in the model list"
            ));
        }
    }
    let taken: Vec<String> = config.providers.iter().map(|p| p.id.clone()).collect();
    let provider_id = provider_id_from_name(name, &taken);
    let wire = draft.wire.normalized();
    config.providers.push(AiProviderEntry {
        id: provider_id.clone(),
        name: name.to_string(),
        kind: wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions),
        wire: Some(wire),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: draft.endpoint.trim().to_string(),
        model: models[0].clone(),
        models,
        context_length: default_context_length(),
        max_tokens: default_max_tokens(),
        temperature: default_temperature(),
        thinking_level: default_thinking_level(),
        capabilities: AiCapabilities {
            text: true,
            streaming: true,
            ..Default::default()
        },
    });
    // The new provider becomes current: the operator added it to use it, and the
    // saved file is the one source of the active id, so the panel reloads with
    // nothing pending.
    config.active_provider = provider_id.clone();
    crate::ai_service::validate_config(&config)?;
    Ok(AiAddedProvider {
        provider_id,
        config,
    })
}

/// Adds one provider to the user config, atomically. No secret is accepted here:
/// the panel stores a collected key through `ai_set_key`.
///
/// The per-model settings are written first: a vendor host only accepts model
/// ids the bundled catalog does not declare once the user layer names them, and
/// both files are restored if the provider itself is rejected.
#[tauri::command]
pub fn ai_add_provider(draft: AiProviderDraft) -> Result<AiAddedProvider, String> {
    ai_add_provider_with(draft, |config| save_ai_config(config).map(|_| ()))
}

pub(crate) fn ai_add_provider_with(
    draft: AiProviderDraft,
    save: impl FnOnce(AiProviderConfig) -> Result<(), String>,
) -> Result<AiAddedProvider, String> {
    let config = load_ai_config()?;
    let taken: Vec<String> = config.providers.iter().map(|p| p.id.clone()).collect();
    let provider_id = provider_id_from_name(draft.name.trim(), &taken);
    let previous = write_model_limits(&provider_id, &draft.limits)?;
    let added = append_provider(config, draft).and_then(|added| {
        save(added.config.clone())?;
        Ok(added)
    });
    match added {
        Ok(added) => {
            debug_assert_eq!(added.provider_id, provider_id);
            log::info!("[ai] added provider {}", added.provider_id);
            Ok(added)
        }
        Err(error) => {
            restore_model_limits(&provider_id, previous);
            Err(error)
        }
    }
}

/// Writes the model settings of a provider that does not exist yet, returning
/// what was there before so a rejected provider leaves no layer behind.
fn write_model_limits(
    provider_id: &str,
    limits: &BTreeMap<String, crate::ai::limits::ModelLimits>,
) -> Result<Vec<(String, Option<crate::ai::limits::ModelLimits>)>, String> {
    if limits.is_empty() {
        return Ok(Vec::new());
    }
    let previous: Vec<_> = limits
        .keys()
        .map(|model| (model.clone(), crate::ai::limits::get(provider_id, model)))
        .collect();
    if let Err(error) = crate::ai::limits::put_many(provider_id, limits) {
        restore_model_limits(provider_id, previous);
        return Err(error);
    }
    Ok(previous)
}

fn restore_model_limits(
    provider_id: &str,
    previous: Vec<(String, Option<crate::ai::limits::ModelLimits>)>,
) {
    for (model, limits) in previous {
        let _ = match limits {
            Some(limits) => crate::ai::limits::put(provider_id, &model, limits),
            None => crate::ai::limits::remove(provider_id, &model),
        };
    }
}

// ============================================================================
// Listing a provider's models before it is saved
// ============================================================================

/// One `GET {base}/models` for an unsaved provider, ids plus the limits the
/// provider reports. The key is used once and dropped: no result, error or log
/// line carries it.
#[tauri::command]
pub async fn ai_fetch_models(
    endpoint: String,
    wire: Wire,
    auth: AuthKind,
    mut key: Option<String>,
) -> Result<crate::ai::models::ModelListing, String> {
    if auth != AuthKind::ApiKey {
        if let Some(mut value) = key.take() {
            value.zeroize();
        }
        return Err("OAuth is available only for providers from the catalog".into());
    }
    let key = key.take().map(|mut value| {
        let trimmed = value.trim().to_string();
        value.zeroize();
        crate::ai::secret::Secret::new(trimmed)
    });
    let key = key.filter(|key| !key.is_empty());
    let wire = wire.normalized();
    let entry = AiProviderEntry {
        id: "draft".into(),
        name: "New provider".into(),
        kind: wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions),
        wire: Some(wire),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: endpoint.trim().to_string(),
        model: String::new(),
        models: Vec::new(),
        context_length: default_context_length(),
        max_tokens: default_max_tokens(),
        temperature: default_temperature(),
        thinking_level: default_thinking_level(),
        capabilities: AiCapabilities::default(),
    };
    let listing = crate::ai::models::fetch(&entry, key).await?;
    log::info!(
        "[ai] listed {} models of {} for a new provider",
        listing.models.len(),
        listing.total
    );
    Ok(listing)
}

// ============================================================================
// Per-model limits (context, output, input modalities)
// ============================================================================

/// Bundled values plus the user layer for one model. The dialog shows the
/// bundled values as the baseline and "reset to catalog" clears the layer.
#[derive(Debug, Serialize, Deserialize)]
pub struct AiModelLimitsView {
    pub provider_id: String,
    pub model_id: String,
    /// Declared by the bundled catalog; null when the model is the user's own.
    pub bundled: Option<crate::ai::limits::ModelLimits>,
    /// Present when the user layer overrides this model.
    pub overridden: Option<crate::ai::limits::ModelLimits>,
    /// The input window a request for this model uses. The panel shows it
    /// beside the stored provider default, which the catalog overrides.
    pub effective_context: u64,
}

/// The window a request for this provider and model uses. A saved entry whose
/// model is the one asked about answers through the same resolution the request
/// path uses: the override, then the catalog, then the known list, and only
/// then the stored number.
fn effective_context(provider_id: &str, model_id: &str, saved: &[AiProviderEntry]) -> u64 {
    if let Some(entry) = saved
        .iter()
        .find(|entry| entry.id == provider_id && entry.model == model_id)
    {
        return crate::ai::limits::context_length(entry);
    }
    bundled_limits(provider_id, model_id, saved)
        .map(|limits| limits.context)
        .unwrap_or_else(default_context_length)
}

/// The bundled model for this saved provider. Saved ids may differ from catalog
/// ids (the defaults do), so the model is matched by the protocol as a fallback.
fn bundled_model(
    provider_id: &str,
    model_id: &str,
    saved: &[AiProviderEntry],
) -> Option<&'static crate::ai::catalog::CatalogModel> {
    let catalog = crate::ai::catalog::catalog();
    // The saved id may carry a vendor prefix or tag the catalog does not use,
    // so the exact id is tried first and the normalised one second.
    let find = |id: &str| -> Option<&'static crate::ai::catalog::CatalogModel> {
        if let Some(model) = catalog
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .and_then(|provider| provider.models.iter().find(|model| model.id == id))
        {
            return Some(model);
        }
        let wire = saved
            .iter()
            .find(|provider| provider.id == provider_id)?
            .wire();
        catalog
            .providers
            .iter()
            .filter(|provider| provider.wire() == wire)
            .flat_map(|provider| provider.models.iter())
            .find(|model| model.id == id)
    };
    find(model_id).or_else(|| {
        crate::ai::catalog::normalised_id(model_id).and_then(|normalised| find(&normalised))
    })
}

fn bundled_limits(
    provider_id: &str,
    model_id: &str,
    saved: &[AiProviderEntry],
) -> Option<crate::ai::limits::ModelLimits> {
    let model = bundled_model(provider_id, model_id, saved)?;
    Some(crate::ai::limits::ModelLimits {
        context: model.context,
        max_output: model.max_output,
        input: model.inputs(),
        price: model.price.clone(),
    })
}

#[tauri::command]
pub fn ai_model_limits(provider_id: String, model_id: String) -> AiModelLimitsView {
    let saved = load_ai_config()
        .map(|config| config.providers)
        .unwrap_or_default();
    AiModelLimitsView {
        bundled: bundled_limits(&provider_id, &model_id, &saved),
        overridden: crate::ai::limits::get(&provider_id, &model_id),
        effective_context: effective_context(&provider_id, &model_id, &saved),
        provider_id,
        model_id,
    }
}

/// Validates and saves one model's limits. The merged catalog is rebuilt on the
/// next read, so the change applies to the next request without a restart.
#[tauri::command]
pub fn ai_save_model_limits(
    provider_id: String,
    model_id: String,
    limits: crate::ai::limits::ModelLimits,
) -> Result<String, String> {
    crate::ai::limits::put(&provider_id, &model_id, limits).map(|path| path.display().to_string())
}

/// Removes the user layer for one model, returning it to the bundled values.
#[tauri::command]
pub fn ai_reset_model_limits(provider_id: String, model_id: String) -> Result<(), String> {
    crate::ai::limits::remove(&provider_id, &model_id).map(|_| ())
}

#[cfg(test)]
mod limits_tests {
    use super::*;

    fn saved(id: &str, wire: Wire) -> AiProviderEntry {
        AiProviderEntry {
            id: id.into(),
            name: id.into(),
            kind: wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions),
            wire: Some(wire),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: "https://models.example/v1/chat/completions".into(),
            model: "fixture".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: default_thinking_level(),
            capabilities: Default::default(),
        }
    }

    #[test]
    fn the_dialog_baseline_finds_bundled_models_behind_saved_provider_ids() {
        let provider = saved("deepseek-v4-flash", Wire::OpenAiChatCompletions);
        let model = bundled_model("deepseek-v4-flash", "deepseek-flash", &[provider]).unwrap();
        assert_eq!(model.id, "deepseek-flash");
        assert!(model.context > 0);
        // A model the bundled catalog does not declare has no baseline.
        let provider = saved("deepseek-v4-flash", Wire::OpenAiChatCompletions);
        assert!(bundled_model("deepseek-v4-flash", "my-own-model", &[provider]).is_none());
    }

    /// A custom gateway entry with a stored window below its catalog baseline.
    fn gateway(model: &str, context_length: u64) -> AiProviderEntry {
        let mut provider = saved("gateway", Wire::OpenAiChatCompletions);
        provider.model = model.into();
        provider.context_length = context_length;
        provider
    }

    #[test]
    fn the_dialog_reports_the_window_a_request_actually_uses() {
        let provider = gateway("deepseek/deepseek-flash", 65_536);
        assert_eq!(
            bundled_limits(
                "gateway",
                "deepseek/deepseek-flash",
                std::slice::from_ref(&provider)
            )
            .unwrap()
            .context,
            1_000_000,
            "the panel baseline is the catalog window, not the stored guess"
        );
        assert_eq!(
            effective_context("gateway", "deepseek/deepseek-flash", &[provider]),
            1_000_000
        );
    }

    #[test]
    fn the_dialog_reports_the_stored_window_when_nothing_describes_the_model() {
        let provider = gateway("some-vendor/nonexistent", 65_536);
        assert!(
            bundled_limits(
                "gateway",
                "some-vendor/nonexistent",
                std::slice::from_ref(&provider)
            )
            .is_none()
        );
        assert_eq!(
            effective_context("gateway", "some-vendor/nonexistent", &[provider]),
            65_536
        );
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;

    fn existing(id: &str) -> AiProviderEntry {
        AiProviderEntry {
            id: id.into(),
            name: id.into(),
            kind: ProviderKind::OpenAiChatCompletions,
            wire: Some(Wire::OpenAiChatCompletions),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: "https://models.example/v1/chat/completions".into(),
            model: "fixture".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 4_096,
            temperature: 0.3,
            thinking_level: default_thinking_level(),
            capabilities: Default::default(),
        }
    }

    fn config() -> AiProviderConfig {
        AiProviderConfig {
            version: 2,
            active_provider: "fixture".into(),
            providers: vec![existing("fixture")],
            warnings: Vec::new(),
        }
    }

    fn draft(name: &str, models: &[&str]) -> AiProviderDraft {
        AiProviderDraft {
            name: name.into(),
            wire: Wire::OpenAiChatCompletions,
            endpoint: "https://llm.acme.test/v1/chat/completions".into(),
            auth: AuthKind::ApiKey,
            models: models.iter().map(|model| (*model).into()).collect(),
            limits: BTreeMap::new(),
        }
    }

    fn limits(context: u64, max_output: u64) -> crate::ai::limits::ModelLimits {
        crate::ai::limits::ModelLimits {
            context,
            max_output,
            input: crate::ai::limits::InputModalities {
                text: true,
                ..Default::default()
            },
            price: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-ai-provider", name)
    }

    #[test]
    fn an_added_provider_round_trips_through_the_config_file() {
        let added = append_provider(
            config(),
            draft("Acme Gateway", &["acme-large", "acme-small"]),
        )
        .unwrap();
        assert_eq!(added.provider_id, "acme-gateway");
        // The new provider is saved to the user config file format and parsed back.
        let dir = temp_dir("round-trip");
        let target = dir.join("ai-providers.yaml");
        let yaml = serde_yaml::to_string(&added.config).unwrap();
        crate::private_fs::write_atomic(&target, yaml.as_bytes()).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        let mut reloaded = parse_ai_config(&text).unwrap();
        normalize_ai_config(&mut reloaded);
        let saved = reloaded
            .providers
            .iter()
            .find(|provider| provider.id == "acme-gateway")
            .expect("the added provider is in the file");
        assert_eq!(saved.name, "Acme Gateway");
        assert_eq!(saved.endpoint, "https://llm.acme.test/v1/chat/completions");
        assert_eq!(saved.model, "acme-large");
        assert_eq!(saved.models, vec!["acme-large", "acme-small"]);
        assert_eq!(saved.kind, ProviderKind::OpenAiChatCompletions);
        assert_eq!(saved.wire(), Wire::OpenAiChatCompletions);
        assert_eq!(saved.auth, AuthKind::ApiKey);
        assert!(saved.capabilities.text && saved.capabilities.streaming);
        // The credentials live in the OS store: the file carries no key field.
        assert!(!text.contains("api_key:"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        // The provider list itself is unchanged apart from the append, and the
        // added provider is now the active one.
        assert_eq!(reloaded.providers.len(), 2);
        assert_eq!(reloaded.active_provider, "acme-gateway");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_model_is_the_default_and_duplicates_collapse() {
        let added = append_provider(
            config(),
            draft("Acme", &[" acme-large ", "acme-large", "", "acme-small"]),
        )
        .unwrap();
        let saved = added
            .config
            .providers
            .iter()
            .find(|provider| provider.id == "acme")
            .unwrap();
        assert_eq!(saved.model, "acme-large");
        assert_eq!(saved.models, vec!["acme-large", "acme-small"]);
    }

    #[test]
    fn ids_are_slugs_and_stay_unique() {
        let mut current = config();
        let first = append_provider(current.clone(), draft("Acme Gateway", &["m"])).unwrap();
        current = first.config;
        let second = append_provider(current.clone(), draft("Acme Gateway", &["m"])).unwrap();
        current = second.config;
        let third = append_provider(current, draft("  ACME/Gateway 2 ", &["m"])).unwrap();
        assert_eq!(first.provider_id, "acme-gateway");
        assert_eq!(second.provider_id, "acme-gateway-2");
        assert_eq!(third.provider_id, "acme-gateway-2-2");
        let ids: Vec<&str> = third
            .config
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "fixture",
                "acme-gateway",
                "acme-gateway-2",
                "acme-gateway-2-2"
            ]
        );
    }

    #[test]
    fn a_provider_named_after_a_catalog_entry_gets_a_free_id() {
        // Derived from the bundled catalog, so the reserved set cannot drift.
        let reserved = reserved_provider_ids();
        assert!(reserved.contains(&"openai"));
        assert!(reserved.contains(&"custom"));
        assert_eq!(provider_id_from_name("OpenAI", &[]), "openai-2");
        assert_eq!(provider_id_from_name("Ollama", &[]), "ollama-2");
        assert_eq!(provider_id_from_name("Custom", &[]), "custom-2");
        for id in &reserved {
            assert_ne!(provider_id_from_name(id, &[]), *id);
        }
        // Two such providers in one config stay distinct.
        let first = append_provider(config(), draft("OpenAI", &["m"])).unwrap();
        let second = append_provider(first.config.clone(), draft("OpenAI", &["m"])).unwrap();
        assert_eq!(first.provider_id, "openai-2");
        assert_eq!(second.provider_id, "openai-3");
        assert_ne!(first.provider_id, second.provider_id);
    }

    #[test]
    fn an_id_already_in_the_config_still_wins_over_the_reserved_list() {
        // A config written before the reservation can already hold `openai`.
        let taken = vec!["openai".to_string(), "openai-2".to_string()];
        assert_eq!(provider_id_from_name("OpenAI", &taken), "openai-3");
        let unique = provider_id_from_name("OpenAI", &taken);
        assert!(!taken.contains(&unique));
    }

    #[test]
    fn the_added_provider_becomes_active() {
        let empty = AiProviderConfig {
            version: 2,
            active_provider: String::new(),
            providers: Vec::new(),
            warnings: Vec::new(),
        };
        let added = append_provider(empty, draft("Acme", &["m"])).unwrap();
        assert_eq!(added.config.active_provider, "acme");
    }

    #[test]
    fn drafts_that_cannot_work_are_rejected_before_anything_is_written() {
        let cases: [(AiProviderDraft, &str); 6] = [
            (draft("", &["m"]), "name"),
            (draft("Acme", &[]), "model id"),
            (draft("Acme", &["  "]), "model id"),
            (
                AiProviderDraft {
                    auth: AuthKind::OAuth,
                    ..draft("Acme", &["m"])
                },
                "OAuth",
            ),
            (
                AiProviderDraft {
                    endpoint: "http://llm.acme.test/v1".into(),
                    ..draft("Acme", &["m"])
                },
                "http",
            ),
            (
                AiProviderDraft {
                    endpoint: "not a url".into(),
                    ..draft("Acme", &["m"])
                },
                "endpoint",
            ),
        ];
        for (draft, expected) in cases {
            let error = append_provider(config(), draft).unwrap_err();
            assert!(
                error.to_lowercase().contains(&expected.to_lowercase()),
                "{expected}: {error}"
            );
        }
        let too_many: Vec<String> = (0..33).map(|index| format!("model-{index}")).collect();
        let models: Vec<&str> = too_many.iter().map(String::as_str).collect();
        assert!(
            append_provider(config(), draft("Acme", &models))
                .unwrap_err()
                .contains("32")
        );
    }

    #[test]
    fn the_enabled_models_and_their_settings_land_in_the_override_file() {
        let dir = temp_dir("limits");
        let target = dir.join("ai").join("models.json");
        let _guard = crate::ai::limits::test_path(target.clone());
        let mut draft = draft("Acme Gateway", &["acme-large", "acme-small"]);
        draft
            .limits
            .insert("acme-large".into(), limits(64_000, 8_192));
        draft
            .limits
            .insert("acme-small".into(), limits(16_000, 2_048));

        let added = append_provider(config(), draft.clone()).unwrap();
        write_model_limits(&added.provider_id, &draft.limits).unwrap();

        assert_eq!(
            crate::ai::limits::get("acme-gateway", "acme-large")
                .unwrap()
                .max_output,
            8_192
        );
        // One file write carries every enabled model of the new provider.
        let text = std::fs::read_to_string(&target).unwrap();
        let saved = crate::ai::limits::parse(&text).unwrap();
        let layer = saved
            .providers
            .get("acme-gateway")
            .expect("the provider names the user layer");
        assert_eq!(layer.len(), 2);
        assert_eq!(layer["acme-large"].context, 64_000);
        assert_eq!(layer["acme-small"].max_output, 2_048);
        // Reset the in-memory layer so the fixture does not leak to other tests.
        crate::ai::limits::remove("acme-gateway", "acme-large").unwrap();
        crate::ai::limits::remove("acme-gateway", "acme-small").unwrap();
        drop(_guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rejected_provider_leaves_no_model_layer_behind() {
        let dir = temp_dir("rollback");
        let target = dir.join("ai").join("models.json");
        let _guard = crate::ai::limits::test_path(target.clone());
        // A unique id: the store is process-global and other tests install layers.
        let mut draft = draft("Rollback Acme", &["acme-large"]);
        draft
            .limits
            .insert("acme-large".into(), limits(32_000, 4_096));
        // A bad protocol makes the provider impossible after its layer is written.
        draft.auth = AuthKind::OAuth;
        let previous = write_model_limits("rollback-acme", &draft.limits).unwrap();
        assert!(append_provider(config(), draft).is_err());
        restore_model_limits("rollback-acme", previous);
        assert!(crate::ai::limits::get("rollback-acme", "acme-large").is_none());
        let saved = crate::ai::limits::parse(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert!(!saved.providers.contains_key("rollback-acme"));
        drop(_guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_for_a_model_outside_the_list_are_rejected() {
        let mut draft = draft("Acme", &["acme-large"]);
        draft
            .limits
            .insert("acme-small".into(), limits(32_000, 4_096));
        let error = append_provider(config(), draft).unwrap_err();
        assert!(error.contains("acme-small"), "{error}");
    }

    #[test]
    fn a_fetched_id_on_a_vendor_host_is_saved_with_its_own_settings() {
        // The catalog is metadata, not an allowlist: an id it does not carry is
        // accepted on a vendor host, and the modal's row supplies the limits.
        let dir = temp_dir("fetched");
        let target = dir.join("ai").join("models.json");
        let _guard = crate::ai::limits::test_path(target.clone());
        let vendor = AiProviderDraft {
            endpoint: "https://api.deepseek.com/chat/completions".into(),
            ..draft("Acme", &["acme-fetched"])
        };
        assert!(append_provider(config(), vendor.clone()).is_ok());
        // A per-model layer for the same id is accepted too, and still applies.
        crate::ai::limits::install_for_test("acme", "acme-fetched", limits(32_000, 4_096));
        assert!(append_provider(config(), vendor).is_ok());
        crate::ai::limits::remove("acme", "acme-fetched").unwrap();
        drop(_guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// How audition audio is kept beside a conversation.
#[tauri::command]
pub fn session_audio_settings() -> Value {
    let audio = crate::gui_settings::load().session_audio;
    json!({
        "format": match audio.format {
            crate::gui_settings::SessionAudioFormat::Wav => "wav",
            crate::gui_settings::SessionAudioFormat::Mp3 => "mp3",
        },
        "mp3Kbps": audio.mp3_kbps,
    })
}

/// Persist the audition audio format; a bitrate outside 64–320 is clamped.
#[tauri::command]
pub fn session_audio_save(
    format: crate::gui_settings::SessionAudioFormat,
    mp3_kbps: u32,
) -> Result<Value, String> {
    let mut stored = crate::gui_settings::load();
    stored.session_audio = crate::gui_settings::SessionAudioSettings { format, mp3_kbps };
    let stored = stored.sanitized();
    crate::gui_settings::save(&stored)?;
    Ok(json!({
        "format": match stored.session_audio.format {
            crate::gui_settings::SessionAudioFormat::Wav => "wav",
            crate::gui_settings::SessionAudioFormat::Mp3 => "mp3",
        },
        "mp3Kbps": stored.session_audio.mp3_kbps,
    }))
}
