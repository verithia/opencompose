//! Typed arguments for arrangement-section tools.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde::Deserializer;
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

fn inline_schema(value: Value) -> Schema {
    serde_json::from_value(value).expect("an inline tool schema is valid")
}

struct RepeatEditSchema;

impl JsonSchema for RepeatEditSchema {
    fn schema_name() -> String {
        "RepeatEdit".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": ["integer", "null"],
            "description": "Plays this many times; null or 1 removes the repeat"
        }))
    }
}

struct LabelEditSchema;

impl JsonSchema for LabelEditSchema {
    fn schema_name() -> String {
        "LabelEdit".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": ["string", "null"],
            "description": "New label; null or an empty string clears it"
        }))
    }
}

/// Distinguish an absent field from an explicit null.
fn explicit_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Add a section: a named span of the arrangement, plus an optional repeat. Returns the updated section list and rendered end. Spans must not overlap; the list stays in written order and the arrangement must fit the 600-second project extent.
pub struct SectionsAddArgs {
    #[schemars(description = "Section name, e.g. verse")]
    pub name: String,
    #[schemars(description = "Start as bar:beat:subdivision (1-based), e.g. \"9:1\" or \"9\"")]
    pub start: String,
    #[schemars(description = "End-exclusive end as bar:beat:subdivision, e.g. \"17:1\"")]
    pub end: String,
    #[schemars(with = "Option<i64>")]
    #[schemars(description = "How many times the span plays (default 1)")]
    pub repeat: Option<u32>,
    #[schemars(description = "Optional display label")]
    pub label: Option<String>,
}

impl ToolArguments for SectionsAddArgs {
    const ID: ToolId = ToolId::SectionsAdd;
    const FIELD_ORDER: &'static [&'static str] = &["name", "start", "end"];
}

// Distinguishes an absent edit from an explicit null without advertising a
// default, which would tell a caller that omission clears the value.
#[derive(Debug, JsonSchema)]
/// Move or resize a section, set its repeat, rename it or change its label — use it when a part should repeat, last longer or move. Omitted fields keep their value; `repeat: null` or 1 clears a repeat and `label: null` or an empty string clears the label. Returns the updated section list and rendered end.
pub struct SectionsSetArgs {
    #[schemars(description = "Current section name")]
    pub name: String,
    #[schemars(description = "Rename to this name; it must be unique")]
    pub new_name: Option<String>,
    #[schemars(description = "New start as bar:beat:subdivision")]
    pub start: Option<String>,
    #[schemars(description = "New end-exclusive end as bar:beat:subdivision")]
    pub end: Option<String>,
    #[schemars(with = "Option<RepeatEditSchema>")]
    pub repeat: Option<Option<u32>>,
    #[schemars(with = "Option<LabelEditSchema>")]
    pub label: Option<Option<String>>,
}

impl<'de> Deserialize<'de> for SectionsSetArgs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            name: String,
            #[serde(default)]
            new_name: Option<String>,
            #[serde(default)]
            start: Option<String>,
            #[serde(default)]
            end: Option<String>,
            #[serde(default, deserialize_with = "explicit_option")]
            repeat: Option<Option<u32>>,
            #[serde(default, deserialize_with = "explicit_option")]
            label: Option<Option<String>>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            name: wire.name,
            new_name: wire.new_name,
            start: wire.start,
            end: wire.end,
            repeat: wire.repeat,
            label: wire.label,
        })
    }
}

impl SectionsSetArgs {
    /// Absent keeps the repeat, null clears it and a value sets it.
    pub fn repeat_edit(&self) -> Option<u32> {
        self.repeat.map(|value| value.unwrap_or(1))
    }

    /// Absent keeps the label, null clears it and a value sets it.
    pub fn label_edit(&self) -> Option<String> {
        self.label.clone().map(|value| value.unwrap_or_default())
    }
}

impl ToolArguments for SectionsSetArgs {
    const ID: ToolId = ToolId::SectionsSet;
    const FIELD_ORDER: &'static [&'static str] = &["name"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Remove a section by name. Returns the updated section list and rendered end.
pub struct SectionsRemoveArgs {
    #[schemars(description = "Section name")]
    pub name: String,
}

impl ToolArguments for SectionsRemoveArgs {
    const ID: ToolId = ToolId::SectionsRemove;
    const FIELD_ORDER: &'static [&'static str] = &["name"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_section_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::SectionsAdd,
            ToolId::SectionsSet,
            ToolId::SectionsRemove,
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
    fn typed_section_edit_treats_null_as_clear_and_absent_as_keep() {
        let absent = SectionsSetArgs::decode(&json!({"name": "verse"})).unwrap();
        assert_eq!(absent.repeat_edit(), None);
        assert_eq!(absent.label_edit(), None);

        let cleared = SectionsSetArgs::decode(&json!({
            "name": "verse",
            "repeat": null,
            "label": null,
        }))
        .unwrap();
        assert_eq!(cleared.repeat_edit(), Some(1));
        assert_eq!(cleared.label_edit(), Some(String::new()));

        let set = SectionsSetArgs::decode(&json!({
            "name": "verse",
            "repeat": 3,
            "label": "Verse",
        }))
        .unwrap();
        assert_eq!(set.repeat_edit(), Some(3));
        assert_eq!(set.label_edit(), Some("Verse".to_owned()));

        assert!(SectionsSetArgs::decode(&json!({"name": "verse", "repeat": "twice"})).is_err());
        assert!(SectionsSetArgs::decode(&json!({"name": "verse", "label": 7})).is_err());
    }

    #[test]
    fn typed_section_add_requires_a_name_and_span() {
        let error = SectionsAddArgs::decode(&json!({"name": "verse"})).unwrap_err();
        assert_eq!(error.field.as_deref(), Some("start"));
        let add = SectionsAddArgs::decode(&json!({
            "name": "verse",
            "start": "9:1",
            "end": "17:1",
            "repeat": 2,
            "label": "Verse",
        }))
        .unwrap();
        assert_eq!(add.repeat, Some(2));
        assert_eq!(add.label.as_deref(), Some("Verse"));
    }
}
