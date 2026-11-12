//! Typed arguments for pulling a market sample.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Download one sample by provider and id from sample_search into the project's media/samples folder and return its path, media path and credit line. The licence and attribution are written into a .json sidecar beside the audio and recorded in index.json, so they travel with the project. The id is resolved by the named provider or by its prefix (commons:, openverse:, ia:, freesound:, jamendo:); a file URL always comes from the provider, never from the caller. Pass accept_licence: true after telling the user the licence; a non-free licence (non-commercial, no-derivatives, GFDL, a community licence or an unknown one) is refused with licence_notice and its terms sentence until then, and the receipt records licence_accepted. Refused with provider_unknown for an id no market carries, provider_unavailable for a page-only market or a keyed one without its key, size_mismatch or checksum_mismatch when the declared size or checksum does not match, and project_unsaved when the project has no folder. A Freesound pull fetches the original when an OAuth2 token is stored, otherwise its preview, and quality reports which one landed. Nothing is written on a refusal.
pub struct SamplePullArgs {
    #[schemars(
        description = "Sample id from sample_search, e.g. openverse:9f0c6b7a-…, ia:prelinger-snare, commons:File:Snare drum unmuffled.ogg"
    )]
    pub id: String,
    #[schemars(
        description = "Market id, e.g. openverse | internet-archive | wikimedia-commons | freesound | jamendo; inferred from the id prefix when omitted"
    )]
    pub provider: Option<String>,
    #[schemars(
        description = "Optional destination directory: absolute, or relative to the project folder. Defaults to media/samples"
    )]
    pub dest: Option<String>,
    #[serde(default)]
    #[schemars(
        default,
        description = "Set true to pull a sample whose licence is not free to reuse after telling the user its terms; the file keeps that licence and its credit line"
    )]
    pub accept_licence: bool,
}

impl ToolArguments for SamplePullArgs {
    const ID: ToolId = ToolId::SamplePull;
    const FIELD_ORDER: &'static [&'static str] = &["id", "provider", "dest", "accept_licence"];
}
