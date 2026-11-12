//! Typed arguments for the device preset library.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// List the device preset library: the shipped factory bank merged with the user library under ~/.opencompose, optionally for one device. Every row carries source (factory or user), shadows (a user row replaced a factory row with the same id) and deletable (false for factory rows). A broken user file is reported in warnings while the factory rows still list.
pub struct DevicePresetsListArgs {
    #[schemars(description = "Optional device id from devices_catalog; omit to list every device")]
    pub device: Option<String>,
}

impl ToolArguments for DevicePresetsListArgs {
    const ID: ToolId = ToolId::DevicePresetsList;
    const FIELD_ORDER: &'static [&'static str] = &["device"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Apply a device preset to the effect insert at index on target, as one undo entry. The preset must belong to the device the insert holds: a preset for another device is refused with the same sentence the apply_device_preset command returns. An unknown preset id is refused by name. Use device_presets_list for ids and devices_catalog for the insert's device.
pub struct DevicePresetApplyArgs {
    #[schemars(description = "Preset id from device_presets_list")]
    pub id: String,
    #[schemars(description = "track:N | bus:name | master | preset:name")]
    pub target: String,
    #[schemars(description = "Effect index in the target's chain")]
    pub effect_index: usize,
    #[schemars(
        description = "Device id the effect insert holds; a preset for another device is refused"
    )]
    pub device: String,
}

impl ToolArguments for DevicePresetApplyArgs {
    const ID: ToolId = ToolId::DevicePresetApply;
    const FIELD_ORDER: &'static [&'static str] = &["id", "target", "effect_index", "device"];
}
