//! Typed arguments for track tools.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

macro_rules! track_arguments {
    ($($request:ident { $($(#[$attribute:meta])* $field:ident: $field_type:ty),* $(,)? })+) => {
        $(
            #[derive(Debug, Deserialize, JsonSchema)]
            pub struct $request { $($(#[$attribute])* pub $field: $field_type),* }
        )+
    };
}

track_arguments! {
    TracksAddArgs {
        #[schemars(description = "Track name")]
        name: String,
        #[schemars(description = "Instrument category (default: synth)")]
        instrument_category: Option<String>,
        #[schemars(description = "Instrument name (default: default)")]
        instrument_name: Option<String>,
        #[schemars(description = "Optional existing playable preset name")]
        preset: Option<String>,
    }
    TracksCreateArgs {
        #[schemars(description = "Track name")]
        name: String,
        #[schemars(description = "Complete @track ... DSL block")]
        dsl: String,
        #[schemars(description = "Instrument category (default: synth)")]
        instrument_category: Option<String>,
        #[schemars(description = "Instrument name (default: default)")]
        instrument_name: Option<String>,
        #[schemars(description = "Optional existing playable preset name")]
        preset: Option<String>,
    }
    TracksRemoveArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
    }
    TracksDuplicateArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Source track index")]
        index: usize,
        #[schemars(description = "Name for the copy")]
        name: String,
    }
    TracksRenameArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "New name")]
        name: String,
    }
    TracksMixArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Volume in dB")]
        volume_db: Option<f64>,
        #[schemars(description = "Pan, -1.0 to 1.0")]
        pan: Option<f64>,
    }
    TracksMuteArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Mute state")]
        mute: bool,
    }
    TracksSoloArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Solo state")]
        solo: bool,
    }
    TracksPresetArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Preset name, or null to clear")]
        preset: Option<String>,
    }
    TracksOutputArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "master or bus name")]
        output: String,
    }
    TracksSendAddArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Bus name")]
        bus: String,
        #[schemars(description = "Send amount, 0.0-4.0 (+12 dB)")]
        amount: Option<f64>,
        #[schemars(description = "Tap before the channel fader (default false)")]
        pre_fader: Option<bool>,
        #[schemars(description = "Balance at the bus input, -1.0 to 1.0 (default 0)")]
        pan: Option<f64>,
        #[schemars(description = "Plugin-hosting track or preset name for an auxiliary output")]
        source_owner: Option<String>,
        #[schemars(with = "Option<i64>")]
        #[schemars(description = "Auxiliary output bus, 2 or higher (out1 is the main pair)")]
        source_output: Option<u8>,
    }
    TracksSendRemoveArgs {
        #[schemars(with = "i64")]
        #[schemars(description = "Track index")]
        index: usize,
        #[schemars(description = "Bus name")]
        bus: String,
        #[schemars(description = "Plugin-hosting owner when removing an auxiliary send")]
        source_owner: Option<String>,
        #[schemars(with = "Option<i64>")]
        #[schemars(description = "Auxiliary output bus, 2 or higher")]
        source_output: Option<u8>,
    }
}

impl ToolArguments for TracksAddArgs {
    const ID: ToolId = ToolId::TracksAdd;
}

impl ToolArguments for TracksCreateArgs {
    const ID: ToolId = ToolId::TracksCreate;
    const FIELD_ORDER: &'static [&'static str] = &["name", "dsl"];
}

impl ToolArguments for TracksRemoveArgs {
    const ID: ToolId = ToolId::TracksRemove;
}

impl ToolArguments for TracksDuplicateArgs {
    const ID: ToolId = ToolId::TracksDuplicate;
    const FIELD_ORDER: &'static [&'static str] = &["index", "name"];
}

impl ToolArguments for TracksRenameArgs {
    const ID: ToolId = ToolId::TracksRename;
    const FIELD_ORDER: &'static [&'static str] = &["index", "name"];
}

impl ToolArguments for TracksMixArgs {
    const ID: ToolId = ToolId::TracksMix;
}

impl ToolArguments for TracksMuteArgs {
    const ID: ToolId = ToolId::TracksMute;
    const FIELD_ORDER: &'static [&'static str] = &["index", "mute"];
}

impl ToolArguments for TracksSoloArgs {
    const ID: ToolId = ToolId::TracksSolo;
    const FIELD_ORDER: &'static [&'static str] = &["index", "solo"];
}

impl ToolArguments for TracksPresetArgs {
    const ID: ToolId = ToolId::TracksPreset;
}

impl ToolArguments for TracksOutputArgs {
    const ID: ToolId = ToolId::TracksOutput;
    const FIELD_ORDER: &'static [&'static str] = &["index", "output"];
}

impl ToolArguments for TracksSendAddArgs {
    const ID: ToolId = ToolId::TracksSendAdd;
    const FIELD_ORDER: &'static [&'static str] = &["index", "bus"];
}

impl ToolArguments for TracksSendRemoveArgs {
    const ID: ToolId = ToolId::TracksSendRemove;
    const FIELD_ORDER: &'static [&'static str] = &["index", "bus"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn typed_track_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::TracksAdd,
            ToolId::TracksCreate,
            ToolId::TracksRemove,
            ToolId::TracksDuplicate,
            ToolId::TracksRename,
            ToolId::TracksMix,
            ToolId::TracksMute,
            ToolId::TracksSolo,
            ToolId::TracksPreset,
            ToolId::TracksOutput,
            ToolId::TracksSendAdd,
            ToolId::TracksSendRemove,
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
    fn typed_track_arguments_decode_optionals_and_defaults() {
        let add = TracksAddArgs::decode(&json!({"name": "Lead"})).unwrap();
        assert_eq!(add.name, "Lead");
        assert!(add.instrument_category.is_none());
        assert!(add.preset.is_none());

        let send = TracksSendAddArgs::decode(&json!({
            "index": 2,
            "bus": "reverb",
            "source_output": 3,
        }))
        .unwrap();
        assert_eq!(send.index, 2);
        assert_eq!(send.bus, "reverb");
        assert!(send.amount.is_none());
        assert_eq!(send.source_output, Some(3));

        let preset = TracksPresetArgs::decode(&json!({"index": 0, "preset": null})).unwrap();
        assert!(preset.preset.is_none());
    }
}
