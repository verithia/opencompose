//! Typed tool arguments and their provider-facing JSON schemas.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{ToolId, ToolSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of typed-argument failure.
pub enum ArgumentErrorCode {
    Missing,
    InvalidType,
    Decode,
    WrongTool,
}

#[derive(Debug)]
/// A typed-argument decode failure for one tool.
pub struct ArgumentError {
    pub tool: ToolId,
    pub code: ArgumentErrorCode,
    pub field: Option<String>,
    source: serde_json::Error,
}
impl ArgumentError {
    /// An error for a handler that does not serve this tool.
    pub fn wrong_tool(tool: ToolId) -> Self {
        Self {
            tool,
            code: ArgumentErrorCode::WrongTool,
            field: None,
            source: <serde_json::Error as serde::de::Error>::custom(
                "tool is not supported by this handler",
            ),
        }
    }
    fn decode<A: ToolArguments>(value: &Value, source: serde_json::Error) -> Self {
        let schema = schema::<A>();
        let required: Vec<_> = schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for field in &required {
            if value.get(*field).is_none() {
                return Self {
                    tool: A::ID,
                    code: ArgumentErrorCode::Missing,
                    field: Some((*field).to_owned()),
                    source,
                };
            }
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (field, schema) in properties {
                if let Some(value) = value.get(field) {
                    if value.is_null() && !required.contains(&field.as_str()) {
                        continue;
                    }
                    if let Some(kind) = schema["type"].as_str()
                        && !super::matches_type(kind, value)
                    {
                        return Self {
                            tool: A::ID,
                            code: ArgumentErrorCode::InvalidType,
                            field: Some(field.clone()),
                            source,
                        };
                    }
                }
            }
        }
        Self {
            tool: A::ID,
            code: ArgumentErrorCode::Decode,
            field: None,
            source,
        }
    }
}
impl std::fmt::Display for ArgumentError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.code, self.field.as_deref()) {
            (ArgumentErrorCode::Missing, Some(field)) => write!(out, "missing argument '{field}'"),
            (_, Some(field)) => write!(out, "argument '{field}': {}", self.source),
            _ => write!(out, "arguments for '{}': {}", self.tool.name(), self.source),
        }
    }
}
impl std::error::Error for ArgumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// A tool typed argument request.
pub trait ToolArguments: DeserializeOwned + JsonSchema {
    const ID: ToolId;
    /// Declared field order preserves the ordering of required wire keys.
    const FIELD_ORDER: &'static [&'static str] = &[];
    /// Tools with no parameters ignore their legacy external payload. The panel
    /// still validates an empty object with no additional properties.
    const IGNORES_INPUT: bool = false;

    /// Decode a JSON object into the typed request.
    fn decode(value: &Value) -> Result<Self, ArgumentError> {
        static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
        let value = if Self::IGNORES_INPUT { &*EMPTY } else { value };
        if !value.is_object() {
            return Err(ArgumentError::decode::<Self>(
                value,
                <serde_json::Error as serde::de::Error>::custom("expected an argument object"),
            ));
        }
        Self::deserialize(value).map_err(|error| ArgumentError::decode::<Self>(value, error))
    }
}

/// Identity, schema and decoder all refer to the same request type.
pub fn tool<A: ToolArguments>(description: &'static str) -> ToolSpec {
    ToolSpec {
        id: A::ID,
        description,
        schema: schema::<A>(),
    }
}

fn object_schema<A: JsonSchema>() -> Value {
    let mut settings = schemars::r#gen::SchemaSettings::draft07();
    // Panel schemas describe present optional values; external Option decoding
    // retains its separate compatibility rule that null means omission.
    settings.option_add_null_type = false;
    settings.meta_schema = None;
    let generator = settings.into_generator();
    let mut schema = serde_json::to_value(generator.into_root_schema_for::<A>())
        .expect("a tool argument schema serializes");
    if let Some(object) = schema.as_object_mut() {
        object.remove("title");
        object.insert("additionalProperties".into(), Value::Bool(false));
        object.entry("properties").or_insert_with(|| json!({}));
        object.entry("required").or_insert_with(|| json!([]));
    }
    strip_numeric_storage_formats(&mut schema);
    schema
}

/// Provider-facing schema for a tool arguments.
pub fn schema<A: ToolArguments>() -> Value {
    let mut schema = object_schema::<A>();
    if let Some(required) = schema["required"].as_array_mut()
        && !A::FIELD_ORDER.is_empty()
    {
        required.sort_by_key(|name| {
            A::FIELD_ORDER
                .iter()
                .position(|field| Some(*field) == name.as_str())
                .unwrap_or(usize::MAX)
        });
    }
    schema
}

fn strip_numeric_storage_formats(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            // Rust storage widths do not add a restriction to the tool protocol.
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("number" | "integer")
            ) {
                object.remove("format");
            }
            for value in object.values_mut() {
                strip_numeric_storage_formats(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_numeric_storage_formats(value);
            }
        }
        _ => {}
    }
}

macro_rules! empty_arguments {
    ($($(#[$attribute:meta])* $request:ident => $id:ident),+ $(,)?) => {
        $(
            $(#[$attribute])*
            #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
            pub struct $request {}
            impl $crate::tool_registry::arguments::ToolArguments for $request {
                const ID: $crate::tool_registry::ToolId = $crate::tool_registry::ToolId::$id;
                const IGNORES_INPUT: bool = true;
            }
        )+
    };
}
pub(crate) use empty_arguments;

/// The shared description of a subtree path does not replace domain parsing.
pub const SUBTREE_PATH_DESCRIPTION: &str = "DSL subtree path: project, track:<name>, track:<n>, track:<name>.phrase:<name>, bus:<name>, preset:<name>, pattern:<name>, project.properties.\nNot replaceable: the whole project, pattern:<name>, sections; a replacement may target a track (or its .phrase/.mix/.effects), project.properties, bus:<name>, preset:<name> or master.";

/// Schema for a subtree-path argument.
pub fn subtree_path_schema(
    generator: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    let mut schema = <String as JsonSchema>::json_schema(generator).into_object();
    schema.metadata().description = Some(SUBTREE_PATH_DESCRIPTION.to_owned());
    schema.into()
}

macro_rules! typed_arguments {
    ($($(#[$struct_attribute:meta])* $visibility:vis struct $request:ident => $id:ident { $($(#[$attribute:meta])* $field_visibility:vis $field:ident: $field_type:ty),* $(,)? })+) => {
        $(
            $(#[$struct_attribute])*
            #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
            $visibility struct $request { $($(#[$attribute])* $field_visibility $field: $field_type),* }
            impl $crate::tool_registry::arguments::ToolArguments for $request {
                const ID: $crate::tool_registry::ToolId = $crate::tool_registry::ToolId::$id;
                const FIELD_ORDER: &'static [&'static str] = &[$(stringify!($field)),*];
            }
        )+
    };
}
pub(crate) use typed_arguments;

/// A project revision retains the protocol's integer schema; unsigned decoding
/// owns which concrete revision values the application can represent.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(transparent)]
pub struct ProjectRevision(pub u64);
impl JsonSchema for ProjectRevision {
    fn schema_name() -> String {
        "ProjectRevision".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::Integer.into()),
            ..Default::default()
        }
        .into()
    }
}

/// The revision is a shared protocol envelope, not a business argument repeated
/// in every mutating operation. Legacy external requests may omit that envelope.
#[derive(Debug, serde::Deserialize, JsonSchema)]
pub struct RevisionGuard {
    /// Project revision guard; the call fails if the project moved to another revision
    pub expected_revision: Option<ProjectRevision>,
}
impl RevisionGuard {
    /// Decode a revision guard from tool arguments.
    pub fn decode(tool: ToolId, value: &Value) -> Result<Self, ArgumentError> {
        static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
        let value = if value.is_object() { value } else { &*EMPTY };
        Self::deserialize(value).map_err(|source| ArgumentError {
            tool,
            code: if value
                .get("expected_revision")
                .is_some_and(|value| !super::matches_type("integer", value))
            {
                ArgumentErrorCode::InvalidType
            } else {
                ArgumentErrorCode::Decode
            },
            field: Some("expected_revision".into()),
            source,
        })
    }
    /// Expected revision as a plain number.
    pub fn expected(self) -> Option<u64> {
        self.expected_revision.map(|revision| revision.0)
    }
    /// Schema for the `expected_revision` property.
    pub fn property_schema() -> Value {
        object_schema::<Self>()["properties"]["expected_revision"].clone()
    }
}
