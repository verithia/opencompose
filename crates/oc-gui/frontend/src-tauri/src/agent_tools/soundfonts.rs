//! Typed arguments for installed instrument-pack management.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Download one instrument pack from the verified index into the user's soundfont folder, checking the declared size and sha256 before the atomic rename. A refused or aborted download leaves no file behind.
pub struct SoundfontInstallArgs {
    #[schemars(description = "Pack id from soundfonts_list, e.g. ms-basic")]
    pub id: String,
}

impl ToolArguments for SoundfontInstallArgs {
    const ID: ToolId = ToolId::SoundfontInstall;
    const FIELD_ORDER: &'static [&'static str] = &["id"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Delete an installed instrument pack from the user's soundfont folder. Refused when the pack is not installed.
pub struct SoundfontsRemoveArgs {
    #[schemars(description = "Pack id from soundfonts_list")]
    pub id: String,
}

impl ToolArguments for SoundfontsRemoveArgs {
    const ID: ToolId = ToolId::SoundfontsRemove;
    const FIELD_ORDER: &'static [&'static str] = &["id"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// List the downloadable instrument packs the signed index offers beside what is installed in the user's soundfont folder, with each pack's size and license. The discovered field also lists existing SF2/SF3 files found in known local instrument folders (15-second cache), without registration. DLS is not supported by the SoundFont engine. Load a discovered path using presets_add(source=soundfont, file=path, program=0), then assign it with tracks_preset. Discovered files are not managed packs and cannot be removed with soundfonts_remove. Read-only; installs nothing. An installed pack is playable through a preset with source: soundfont and that pack's path. The plugin browser's Instruments section is the user-facing install path: name the pack and let the user click Install.
pub struct SoundfontsListArgs {
    #[schemars(
        description = "Optional discovered SF2/SF3 path to inspect with the engine parser (max 128 MiB). Returns only inspection: bounded program metadata, totals, next_offset and warning summary; no sample data or unrelated pack listing. For piano use bank=0 and query=piano. validated=true means parsing succeeded; warning summaries are non-fatal. Prefer bank=0 programs with zones>0 for melodic tracks."
    )]
    pub path: Option<String>,
    #[schemars(
        description = "Optional bank filter when inspecting a path; 0 is the standard melodic bank"
    )]
    pub bank: Option<u16>,
    #[schemars(description = "Optional case-insensitive program-name filter, e.g. piano")]
    pub query: Option<String>,
    #[schemars(
        description = "Program page offset, default 0; use inspection.next_offset for the next page"
    )]
    pub offset: Option<usize>,
    #[schemars(description = "Programs per page, default 32, clamped to 1..64")]
    pub limit: Option<usize>,
}

impl ToolArguments for SoundfontsListArgs {
    const ID: ToolId = ToolId::SoundfontsList;
    const FIELD_ORDER: &'static [&'static str] = &["path", "bank", "query", "offset", "limit"];
}
