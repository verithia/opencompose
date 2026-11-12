//! Typed arguments for read-only host queries with no payload.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

macro_rules! empty_tool {
    ($request:ident, $id:ident) => {
        #[derive(Debug, Deserialize, JsonSchema)]
        pub struct $request {}

        impl ToolArguments for $request {
            const ID: ToolId = ToolId::$id;
        }
    };
}

empty_tool!(AudioDevicesArgs, AudioDevices);
empty_tool!(AudioConfigArgs, AudioConfig);
empty_tool!(UndoStateArgs, UndoState);
empty_tool!(UndoHistoryArgs, UndoHistory);
empty_tool!(SamplerAssetsArgs, SamplerAssets);
empty_tool!(SystemCapabilitiesArgs, SystemCapabilities);
empty_tool!(AudioClipsListArgs, AudioClipsList);
empty_tool!(TracksListArgs, TracksList);
empty_tool!(SectionsListArgs, SectionsList);
