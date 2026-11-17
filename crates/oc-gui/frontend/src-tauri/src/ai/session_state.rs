//! Typed conversation goal and plan. The model reads the latest text derived
//! from these values, and the panel paints the rows. The newest row of each
//! kind is the current state, so a rewind cuts it and a fork copies it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
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
            Self::PlanSet { items } => {
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
        }
    }
}

/// The typed changes a `plan_update` call asks for. A missing field leaves
/// that part unchanged; `goal: null` and an empty item list clear it.
pub fn plan_update_states(args: &Map<String, Value>) -> Result<Vec<SessionState>, String> {
    let mut states = Vec::new();
    if let Some(goal) = args.get("goal") {
        let state = match goal {
            Value::Null => SessionState::GoalCleared {
                author: StateAuthor::Agent,
            },
            Value::String(text) => SessionState::GoalSet {
                text: trim_text(text).to_owned(),
                author: StateAuthor::Agent,
            },
            _ => return Err("goal must be a string or null".into()),
        };
        if !state.valid() {
            return Err(format!("a goal is 1 to {} characters", MAX_GOAL_CHARS));
        }
        states.push(state);
    }
    if let Some(items) = args.get("items") {
        let items = items
            .as_array()
            .ok_or("items must be a list")?
            .iter()
            .map(|item| {
                let mut item: PlanItem = serde_json::from_value(item.clone())
                    .map_err(|_| "each item needs text and status".to_owned())?;
                item.text = trim_text(&item.text).to_owned();
                Ok(item)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let state = SessionState::PlanSet { items };
        if !state.valid() {
            return Err(format!(
                "a plan is at most {} items of 1 to {} characters, with at most one in progress",
                MAX_PLAN_ITEMS, MAX_PLAN_ITEM_CHARS
            ));
        }
        states.push(state);
    }
    if states.is_empty() {
        return Err("plan_update needs a goal or items".into());
    }
    Ok(states)
}

/// The saved row a state change becomes: a user row whose metadata carries the
/// typed value and whose content is the text the model reads.
pub fn state_row(state: &SessionState) -> Value {
    json!({
        "role": "user",
        "message_meta": {
            "version": 1,
            "kind": "session_state",
            "state": state,
        },
        "content": state.model_text(),
    })
}

/// The current goal and plan: the newest goal row decides the goal, and the
/// newest plan row decides the plan. A rewind removes the rows after its cut,
/// so the state rewinds with the conversation.
pub fn current_state(view: &[Value]) -> (Option<Goal>, Option<Vec<PlanItem>>) {
    let mut goal = None;
    let mut goal_found = false;
    let mut plan = None;
    let mut plan_found = false;
    for message in view.iter().rev() {
        let Some(state) = crate::ai::sessions::saved_session_state(message) else {
            continue;
        };
        match state {
            SessionState::GoalSet { text, author } if !goal_found => {
                goal_found = true;
                goal = Some(Goal { text, author });
            }
            SessionState::GoalCleared { .. } if !goal_found => {
                goal_found = true;
            }
            SessionState::PlanSet { items } if !plan_found => {
                plan_found = true;
                plan = Some(items);
            }
            _ => {}
        }
        if goal_found && plan_found {
            break;
        }
    }
    (goal, plan)
}

/// The latest goal and plan changes, including explicit clears, independent
/// of which transcript page a window loads.
pub fn latest_states(view: &[Value]) -> Vec<SessionState> {
    let mut states = Vec::new();
    let mut goal = false;
    let mut plan = false;
    for state in view
        .iter()
        .rev()
        .filter_map(crate::ai::sessions::saved_session_state)
    {
        let seen = if state.is_plan() {
            &mut plan
        } else {
            &mut goal
        };
        if !*seen {
            *seen = true;
            states.push(state);
        }
        if goal && plan {
            break;
        }
    }
    states.reverse();
    states
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
        text.push_str("Plan:");
        for (index, item) in items.iter().enumerate() {
            text.push_str(&format!(
                "\n{}. [{}] {}",
                index + 1,
                item.status.model_label(),
                item.text
            ));
        }
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
        let mut args = Map::new();
        args.insert("goal".into(), json!("finish"));
        args.insert("items".into(), json!([{"text": "write", "status": "done"}]));
        let states = plan_update_states(&args).unwrap();
        assert_eq!(states.len(), 2);
        assert!(states.iter().all(SessionState::valid));
        let mut clear = Map::new();
        clear.insert("goal".into(), Value::Null);
        clear.insert("items".into(), json!([]));
        assert_eq!(plan_update_states(&clear).unwrap().len(), 2);
        assert!(plan_update_states(&Map::new()).is_err());
        let mut bad = Map::new();
        bad.insert(
            "items".into(),
            json!([
                {"text":"one","status":"in_progress"},
                {"text":"two","status":"in_progress"}
            ]),
        );
        assert!(plan_update_states(&bad).is_err());
        let mut unknown = Map::new();
        unknown.insert("goal".into(), json!({ "text": "x" }));
        assert!(plan_update_states(&unknown).is_err());
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

        let mut args = Map::new();
        args.insert("goal".into(), json!("\u{FEFF}finish\u{FEFF}"));
        args.insert(
            "items".into(),
            json!([{"text": "\u{00A0}write\u{3000}", "status": "done"}]),
        );
        let states = plan_update_states(&args).unwrap();
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
