//! The typed `plan_update` request: a tri-state goal and optional plan items.

use crate::ai::session_state::{
    MAX_GOAL_CHARS, MAX_PLAN_ITEM_CHARS, MAX_PLAN_ITEMS, PlanItem, SessionState, StateAuthor,
    trim_text,
};
use crate::tool_registry::arguments::typed_arguments;
use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{
    InstanceType, Metadata, Schema, SchemaObject, SingleOrVec, StringValidation,
};
use serde::{Deserialize, Deserializer, de};

/// The goal part of a `plan_update`: absent leaves it unchanged, `null`
/// clears it and a string sets it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalUpdate(pub Option<Option<String>>);

impl<'de> Deserialize<'de> for GoalUpdate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(Self(Some(None))),
            serde_json::Value::String(text) => Ok(Self(Some(Some(text)))),
            _ => Err(de::Error::custom("goal must be a string or null")),
        }
    }
}

impl JsonSchema for GoalUpdate {
    fn schema_name() -> String {
        "GoalUpdate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::String,
                InstanceType::Null,
            ])),
            string: Some(Box::new(StringValidation {
                max_length: Some(MAX_GOAL_CHARS as u32),
                ..Default::default()
            })),
            metadata: Some(Box::new(Metadata {
                description: Some("The goal, or null to clear it".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

typed_arguments! {
    /// Record the goal this conversation works toward and the plan it follows.
    pub struct PlanUpdateArgs => PlanUpdate {
        #[serde(default)]
        pub goal: GoalUpdate,
        #[serde(default)]
        #[schemars(length(max = 32))]
        pub items: Option<Vec<PlanItem>>,
    }
}

impl PlanUpdateArgs {
    /// The typed state changes this call asks for. A missing field leaves that
    /// part unchanged; `goal: null` and an empty item list clear it.
    pub fn into_states(self) -> Result<Vec<SessionState>, String> {
        let mut states = Vec::new();
        match self.goal.0 {
            Some(None) => states.push(SessionState::GoalCleared {
                author: StateAuthor::Agent,
            }),
            Some(Some(text)) => {
                let state = SessionState::GoalSet {
                    text: trim_text(&text).to_owned(),
                    author: StateAuthor::Agent,
                };
                if !state.valid() {
                    return Err(format!("a goal is 1 to {MAX_GOAL_CHARS} characters"));
                }
                states.push(state);
            }
            None => {}
        }
        if let Some(items) = self.items {
            let items = items
                .into_iter()
                .map(|mut item| {
                    item.text = trim_text(&item.text).to_owned();
                    item
                })
                .collect();
            let state = SessionState::PlanSet { items };
            if !state.valid() {
                return Err(format!(
                    "a plan is at most {MAX_PLAN_ITEMS} items of 1 to {MAX_PLAN_ITEM_CHARS} \
                     characters, with at most one in progress"
                ));
            }
            states.push(state);
        }
        if states.is_empty() {
            return Err("plan_update needs a goal or items".into());
        }
        Ok(states)
    }
}
