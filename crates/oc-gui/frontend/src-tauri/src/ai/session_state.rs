//! Typed conversation goal and plan. The model reads the latest text derived
//! from these values, and the panel paints the rows. The newest row of each
//! kind is the current state, so a rewind cuts it and a fork copies it.

use crate::ai::sessions::MessageKind;
use crate::ai::sessions::message_protocol::{self, MessageMeta};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A character a goal or plan text treats as blank. This is the owning set:
/// Unicode White_Space plus the byte-order mark. The panel copies it exactly,
/// because JavaScript's `trim` neither removes U+0085 nor keeps the same set.
pub fn is_blank_char(value: char) -> bool {
    value.is_whitespace() || value == '\u{FEFF}'
}

/// Trim the blank characters Rust and the panel agree on.
pub fn trim_text(text: &str) -> &str {
    text.trim_matches(is_blank_char)
}

/// Whether a text holds nothing but blanks both sides agree on.
pub fn is_blank_text(text: &str) -> bool {
    trim_text(text).is_empty()
}

/// Longest goal the store accepts.
pub const MAX_GOAL_CHARS: usize = 600;
/// Most plan items one plan may carry.
pub const MAX_PLAN_ITEMS: usize = 32;
/// Longest plan item text the store accepts.
pub const MAX_PLAN_ITEM_CHARS: usize = 200;
/// Wire marker for the derived context row the model reads every round. It is
/// never saved, so it carries no message metadata.
pub const CONTEXT_STATE: &str = "session_state";

/// Who wrote a goal change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateAuthor {
    Person,
    Agent,
}

/// Where one plan step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
}

impl PlanStatus {
    /// The English words the model reads.
    pub fn model_label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in progress",
            Self::Done => "done",
        }
    }
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlanItem {
    #[schemars(length(min = 1, max = 200))]
    pub text: String,
    pub status: PlanStatus,
}

/// A goal as the conversation currently holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub text: String,
    pub author: StateAuthor,
}

/// A typed change to the conversation's goal or plan, saved as one
/// `session_state` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionState {
    GoalSet { text: String, author: StateAuthor },
    GoalCleared { author: StateAuthor },
    PlanSet { items: Vec<PlanItem> },
}

impl SessionState {
    /// Whether this change writes the plan rather than the goal.
    pub fn is_plan(&self) -> bool {
        matches!(self, Self::PlanSet { .. })
    }

    /// Whether every text and count is bounded. A row outside these bounds is
    /// not part of the conversation state.
    pub fn valid(&self) -> bool {
        match self {
            Self::GoalSet { text, .. } => {
                !is_blank_text(text) && text.chars().count() <= MAX_GOAL_CHARS
            }
            Self::GoalCleared { .. } => true,
            Self::PlanSet { items } => {
                items.len() <= MAX_PLAN_ITEMS
                    && items.iter().all(|item| {
                        !is_blank_text(&item.text)
                            && item.text.chars().count() <= MAX_PLAN_ITEM_CHARS
                    })
                    && items
                        .iter()
                        .filter(|item| item.status == PlanStatus::InProgress)
                        .count()
                        <= 1
            }
        }
    }

    /// The English text the model reads, derived only from the typed value.
    pub fn model_text(&self) -> String {
        match self {
            Self::GoalSet { text, .. } => format!("Goal: {text}"),
            Self::GoalCleared { .. } => "Goal cleared.".into(),
            Self::PlanSet { items } if items.is_empty() => "Plan cleared.".into(),
            Self::PlanSet { items } => plan_text(items),
        }
    }
}

/// The saved row a state change becomes: a user row whose metadata carries the
/// typed value and whose content is the text the model reads.
pub fn state_row(state: &SessionState) -> Value {
    let mut meta = MessageMeta::new(MessageKind::SessionState);
    meta.state = Some(state.clone());
    message_protocol::typed_row(meta, state.model_text())
}

/// The current goal and plan: the newest goal row decides the goal, and the
/// newest plan row decides the plan. A rewind removes the rows after its cut,
/// so the state rewinds with the conversation.
pub fn current_state(view: &[Value]) -> (Option<Goal>, Option<Vec<PlanItem>>) {
    let rows = latest_state_rows(view);
    let goal = rows.iter().find_map(|(_, state)| match state {
        SessionState::GoalSet { text, author } => Some(Goal {
            text: text.clone(),
            author: *author,
        }),
        _ => None,
    });
    let plan = rows.iter().find_map(|(_, state)| match state {
        SessionState::PlanSet { items } => Some(items.clone()),
        _ => None,
    });
    (goal, plan)
}

/// The newest goal row and the newest plan row, with their view indices, in
/// view order. An explicit clear is the newest row of its kind and is kept as
/// such. This is the one place that decides which state rows are current.
pub fn latest_state_rows(view: &[Value]) -> Vec<(usize, SessionState)> {
    let mut rows = Vec::new();
    let mut goal = false;
    let mut plan = false;
    for (index, message) in view.iter().enumerate().rev() {
        let Some(state) = crate::ai::sessions::saved_session_state(message) else {
            continue;
        };
        let seen = if state.is_plan() {
            &mut plan
        } else {
            &mut goal
        };
        if !*seen {
            *seen = true;
            rows.push((index, state));
        }
        if goal && plan {
            break;
        }
    }
    rows.reverse();
    rows
}

/// The latest goal and plan changes, including explicit clears, independent
/// of which transcript page a window loads.
pub fn latest_states(view: &[Value]) -> Vec<SessionState> {
    latest_state_rows(view)
        .into_iter()
        .map(|(_, state)| state)
        .collect()
}

/// Insert one derived row after the leading system rows, so the model reads it
/// before the conversation. Returns the index it took.
pub fn insert_after_system(messages: &mut Vec<Value>, row: Value) -> usize {
    let head = messages
        .iter()
        .take_while(|message| message["role"] == "system")
        .count();
    messages.insert(head, row);
    head
}

/// The English plan text the model reads, shared by a plan row and the
/// request's derived context row.
fn plan_text(items: &[PlanItem]) -> String {
    let mut text = String::from("Plan:");
    for (index, item) in items.iter().enumerate() {
        text.push_str(&format!(
            "\n{}. [{}] {}",
            index + 1,
            item.status.model_label(),
            item.text
        ));
    }
    text
}

/// The one short context row a request carries before its conversation. The
/// row is derived, never saved, and refreshed after `plan_update`.
pub fn context_row(goal: Option<&Goal>, plan: Option<&[PlanItem]>) -> Option<Value> {
    let mut text = String::new();
    if let Some(goal) = goal {
        text.push_str(&format!("Goal: {}", goal.text));
    }
    if let Some(items) = plan.filter(|items| !items.is_empty()) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&plan_text(items));
    }
    (!text.is_empty()).then(|| {
        json!({
            "role": "user",
            "context": CONTEXT_STATE,
            "content": text,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::arguments::ToolArguments;

    fn decode_plan_update(value: Value) -> Result<Vec<SessionState>, String> {
        crate::ai::plan_update::PlanUpdateArgs::decode(&value)
            .map_err(|error| error.to_string())
            .and_then(|args| args.into_states())
    }

    fn goal_set(text: &str) -> SessionState {
        SessionState::GoalSet {
            text: text.into(),
            author: StateAuthor::Person,
        }
    }

    fn plan(items: &[(&str, PlanStatus)]) -> SessionState {
        SessionState::PlanSet {
            items: items
                .iter()
                .map(|(text, status)| PlanItem {
                    text: (*text).into(),
                    status: *status,
                })
                .collect(),
        }
    }

    #[test]
    fn bounds_refuse_oversized_and_ambiguous_states() {
        assert!(goal_set("finish the bridge").valid());
        assert!(!goal_set("   ").valid());
        assert!(!goal_set(&"x".repeat(MAX_GOAL_CHARS + 1)).valid());
        let many = (0..=MAX_PLAN_ITEMS)
            .map(|index| (format!("step {index}"), PlanStatus::Pending))
            .collect::<Vec<_>>();
        assert!(
            !SessionState::PlanSet {
                items: many
                    .iter()
                    .map(|(text, status)| PlanItem {
                        text: text.clone(),
                        status: *status,
                    })
                    .collect()
            }
            .valid()
        );
        assert!(
            !plan(&[
                ("one", PlanStatus::InProgress),
                ("two", PlanStatus::InProgress)
            ])
            .valid()
        );
        assert!(plan(&[]).valid(), "an empty plan clears the plan");
        assert!(
            !SessionState::PlanSet {
                items: vec![PlanItem {
                    text: "x".repeat(MAX_PLAN_ITEM_CHARS + 1),
                    status: PlanStatus::Pending,
                }]
            }
            .valid()
        );
    }

    #[test]
    fn model_text_is_derived_from_the_typed_value() {
        assert_eq!(goal_set("finish").model_text(), "Goal: finish");
        assert_eq!(
            SessionState::GoalCleared {
                author: StateAuthor::Agent
            }
            .model_text(),
            "Goal cleared."
        );
        assert_eq!(plan(&[]).model_text(), "Plan cleared.");
        assert_eq!(
            plan(&[
                ("write", PlanStatus::Done),
                ("mix", PlanStatus::InProgress),
                ("check", PlanStatus::Pending),
            ])
            .model_text(),
            "Plan:\n1. [done] write\n2. [in progress] mix\n3. [pending] check"
        );
    }

    #[test]
    fn a_state_row_round_trips_through_the_saved_reader() {
        let row = state_row(&goal_set("finish the bridge"));
        assert_eq!(row["role"], "user");
        assert_eq!(row["message_meta"]["kind"], "session_state");
        assert_eq!(row["message_meta"]["state"]["kind"], "goal_set");
        assert_eq!(row["content"], "Goal: finish the bridge");
        let state = crate::ai::sessions::saved_session_state(&row).unwrap();
        assert_eq!(state, goal_set("finish the bridge"));
    }

    #[test]
    fn plan_update_parses_only_bounded_typed_fields() {
        let states = decode_plan_update(json!({
            "goal": "finish",
            "items": [{"text": "write", "status": "done"}],
        }))
        .unwrap();
        assert_eq!(states.len(), 2);
        assert!(states.iter().all(SessionState::valid));
        assert_eq!(
            decode_plan_update(json!({"goal": null, "items": []}))
                .unwrap()
                .len(),
            2
        );
        assert!(decode_plan_update(json!({})).is_err());
        assert!(
            decode_plan_update(json!({"items": [
                {"text":"one","status":"in_progress"},
                {"text":"two","status":"in_progress"}
            ]}))
            .is_err()
        );
        assert!(decode_plan_update(json!({"goal": {"text": "x"}})).is_err());
    }

    #[test]
    fn blanks_javascript_trims_are_blank_here_too() {
        // The panel validates with JavaScript semantics; every character its
        // `trim` removes, including the byte-order mark, must read as blank.
        for blank in [
            "\u{FEFF}",
            "\u{00A0}",
            "\u{2028}",
            "\u{2029}",
            "\u{3000}",
            "\u{0085}",
            "\u{FEFF}\u{3000}\u{00A0}",
        ] {
            assert!(!goal_set(blank).valid(), "{blank:?} is blank");
            assert!(
                !plan(&[(blank, PlanStatus::Pending)]).valid(),
                "{blank:?} is blank in a plan item"
            );
            assert!(is_blank_text(blank));
        }
        assert!(!is_blank_text("\u{FEFF}finish\u{3000}"));

        let states = decode_plan_update(json!({
            "goal": "\u{FEFF}finish\u{FEFF}",
            "items": [{"text": "\u{00A0}write\u{3000}", "status": "done"}],
        }))
        .unwrap();
        assert_eq!(
            states[0],
            SessionState::GoalSet {
                text: "finish".into(),
                author: StateAuthor::Agent,
            }
        );
        assert_eq!(states[1], plan(&[("write", PlanStatus::Done)]));
    }

    #[test]
    fn the_newest_goal_and_plan_rows_decide_the_state() {
        let view = vec![
            state_row(&goal_set("first")),
            state_row(&plan(&[("one", PlanStatus::Pending)])),
            state_row(&goal_set("second")),
            state_row(&plan(&[("one", PlanStatus::Done)])),
        ];
        let (goal, plan) = current_state(&view);
        assert_eq!(goal.unwrap().text, "second");
        assert_eq!(plan.unwrap()[0].status, PlanStatus::Done);
    }

    #[test]
    fn a_cut_rewinds_the_state() {
        let view = [
            state_row(&goal_set("first")),
            state_row(&plan(&[("one", PlanStatus::Pending)])),
            state_row(&goal_set("second")),
        ];
        let (goal, plan) = current_state(&view[..2]);
        assert_eq!(goal.unwrap().text, "first");
        assert!(plan.is_some());
        let (goal, plan) = current_state(&view[..1]);
        assert_eq!(goal.unwrap().text, "first");
        assert!(plan.is_none());
        let (goal, plan) = current_state(&view[..0]);
        assert!(goal.is_none() && plan.is_none());
    }

    #[test]
    fn a_clear_row_removes_the_goal() {
        let view = vec![
            state_row(&goal_set("first")),
            state_row(&SessionState::GoalCleared {
                author: StateAuthor::Person,
            }),
        ];
        assert!(current_state(&view).0.is_none());
    }

    #[test]
    fn the_context_row_carries_goal_and_plan_without_metadata() {
        let goal = Goal {
            text: "finish".into(),
            author: StateAuthor::Person,
        };
        let items = [
            PlanItem {
                text: "write".into(),
                status: PlanStatus::Done,
            },
            PlanItem {
                text: "mix".into(),
                status: PlanStatus::InProgress,
            },
        ];
        let row = context_row(Some(&goal), Some(&items)).unwrap();
        assert_eq!(row["role"], "user");
        assert_eq!(row["context"], CONTEXT_STATE);
        assert!(row.get("message_meta").is_none());
        assert_eq!(
            row["content"],
            "Goal: finish\nPlan:\n1. [done] write\n2. [in progress] mix"
        );
        assert!(context_row(None, Some(&[])).is_none());
    }
}
