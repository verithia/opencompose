//! Typed arguments for content-library and preset listings.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// List registered content libraries and their instruments, plus discovered_soundfonts from known local SF2/SF3 locations without requiring registration. Read-only. Load a discovered path using presets_add(source=soundfont, file=path, program=0), then tracks_preset.
pub struct ContentLibrariesArgs {
    #[schemars(description = "Optional catalogue path; defaults to this profile's catalogue")]
    pub catalogue: Option<String>,
}

impl ToolArguments for ContentLibrariesArgs {
    const ID: ToolId = ToolId::ContentLibraries;
    const FIELD_ORDER: &'static [&'static str] = &["catalogue"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Register a content-library root (a directory of .sf2/.sf3/.sfz instruments or a single instrument file). The root is scanned first; a write is backed up and verified, and a failed write restores the previous catalogue.
pub struct ContentLibraryRegisterArgs {
    #[schemars(description = "Directory or instrument file to register")]
    pub path: String,
    #[schemars(description = "Optional display name")]
    pub name: Option<String>,
    #[schemars(description = "Optional catalogue path; defaults to this profile's catalogue")]
    pub catalogue: Option<String>,
}

impl ToolArguments for ContentLibraryRegisterArgs {
    const ID: ToolId = ToolId::ContentLibraryRegister;
    const FIELD_ORDER: &'static [&'static str] = &["path"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Remove a registered content library by id. The files on disk are never touched; the catalogue is backed up first.
pub struct ContentLibraryRemoveArgs {
    #[schemars(description = "Library id from content_libraries")]
    pub id: String,
    #[schemars(description = "Optional catalogue path; defaults to this profile's catalogue")]
    pub catalogue: Option<String>,
}

impl ToolArguments for ContentLibraryRemoveArgs {
    const ID: ToolId = ToolId::ContentLibraryRemove;
    const FIELD_ORDER: &'static [&'static str] = &["id"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Rescan one registered library or all of them, updating the instrument list. Missing roots are kept and marked missing; the catalogue is backed up first.
pub struct ContentLibraryRescanArgs {
    #[schemars(description = "Optional library id; omit to rescan every library")]
    pub id: Option<String>,
    #[schemars(description = "Optional catalogue path; defaults to this profile's catalogue")]
    pub catalogue: Option<String>,
}

impl ToolArguments for ContentLibraryRescanArgs {
    const ID: ToolId = ToolId::ContentLibraryRescan;
    const FIELD_ORDER: &'static [&'static str] = &["id", "catalogue"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// List authored project presets and SoundFonts. With name, inspect one preset and its effective inherited parameters and insert order without scanning SoundFonts.
pub struct PresetsListArgs {
    #[schemars(description = "Exact project preset name, optional")]
    pub name: Option<String>,
}

impl ToolArguments for PresetsListArgs {
    const ID: ToolId = ToolId::PresetsList;
    const FIELD_ORDER: &'static [&'static str] = &["name"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn typed_content_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::ContentLibraries,
            ToolId::ContentLibraryRegister,
            ToolId::ContentLibraryRemove,
            ToolId::ContentLibraryRescan,
            ToolId::PresetsList,
        ] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_content_requests_decode_their_optional_fields() {
        let register = ContentLibraryRegisterArgs::decode(&json!({
            "path": "library/",
            "name": null,
            "catalogue": null,
        }))
        .unwrap();
        assert_eq!(register.path, "library/");
        assert!(register.name.is_none());
        assert!(register.catalogue.is_none());

        let rescan = ContentLibraryRescanArgs::decode(&json!({})).unwrap();
        assert!(rescan.id.is_none());
        assert!(rescan.catalogue.is_none());

        assert!(ContentLibraryRemoveArgs::decode(&json!({})).is_err());
    }
}
