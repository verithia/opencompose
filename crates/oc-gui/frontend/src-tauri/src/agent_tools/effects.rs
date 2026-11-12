//! Typed arguments for effect-chain edits.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Add an effect to track:N, bus:name, master, or preset:name.
pub struct EffectsAddArgs {
    #[schemars(description = "track:N | bus:name | master | preset:name")]
    pub target: String,
    #[schemars(description = "eq | compression | limiter | reverb | delay | ...")]
    pub effect_type: String,
}

impl ToolArguments for EffectsAddArgs {
    const ID: ToolId = ToolId::EffectsAdd;
    const FIELD_ORDER: &'static [&'static str] = &["target", "effect_type"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Remove an effect by index.
pub struct EffectsRemoveArgs {
    #[schemars(description = "track:N | bus:name | master | preset:name")]
    pub target: String,
    #[schemars(description = "Effect index")]
    pub index: usize,
}

impl ToolArguments for EffectsRemoveArgs {
    const ID: ToolId = ToolId::EffectsRemove;
    const FIELD_ORDER: &'static [&'static str] = &["target", "index"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Set an effect parameter.
pub struct EffectsSetParamArgs {
    #[schemars(description = "track:N | bus:name | master | preset:name")]
    pub target: String,
    #[schemars(description = "Effect index")]
    pub index: usize,
    #[schemars(description = "Parameter name")]
    pub param: String,
    #[schemars(description = "Parameter value")]
    pub value: String,
}

impl ToolArguments for EffectsSetParamArgs {
    const ID: ToolId = ToolId::EffectsSetParam;
    const FIELD_ORDER: &'static [&'static str] = &["target", "index", "param", "value"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Bypass or enable an existing effect without changing its parameters or removing the insert. The index is zero-based within the target's effects chain.
pub struct EffectsSetBypassArgs {
    #[schemars(description = "track:N | bus:name | master | preset:name")]
    pub target: String,
    #[schemars(description = "Zero-based effect index in the target's chain")]
    pub index: usize,
    #[schemars(description = "true bypasses the insert; false enables it")]
    pub bypass: bool,
}

impl ToolArguments for EffectsSetBypassArgs {
    const ID: ToolId = ToolId::EffectsSetBypass;
    const FIELD_ORDER: &'static [&'static str] = &["target", "index", "bypass"];
}
