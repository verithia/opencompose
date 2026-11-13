//! The one decision every call passes before it runs: does it change the
//! project DSL, and what does the session's permission need first.
//!
//! The registry's `mutation` flag, the panel-local `edit_subtree` and the
//! `confirm` approval policy are the whole set of modifying calls, so a new
//! tool cannot bypass the gate.

use crate::ai::sessions::SessionPermission;
use crate::tool_registry::{Approval, ToolId};
use serde_json::{Map, Value, json};

/// What the session needs before a DSL-modifying call runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    None,
    /// A person approves or declines the call before it runs.
    Approval,
    /// The call runs; the change is shown and validated after it lands.
    Review,
}

/// The decision for one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub modifies: bool,
    pub need: Need,
}

/// A read-only tool needs nothing in every mode. A `confirm` tool deletes an
/// object the person built, so it asks unless this conversation allowed it;
/// every other modifying call follows the session's mode.
pub fn gate(tool: &str, mode: SessionPermission, allowed: &[String]) -> Gate {
    let Some(id) = ToolId::parse(tool) else {
        return Gate {
            modifies: false,
            need: Need::None,
        };
    };
    let meta = id.metadata();
    let confirm = meta.approval == Approval::Confirm;
    if !(meta.mutation || id == ToolId::EditSubtree || confirm) {
        return Gate {
            modifies: false,
            need: Need::None,
        };
    }
    if confirm && !allowed.iter().any(|name| name == tool) {
        return Gate {
            modifies: true,
            need: Need::Approval,
        };
    }
    Gate {
        modifies: true,
        need: match mode {
            SessionPermission::RequireApproval => Need::Approval,
            SessionPermission::Autoreview => Need::Review,
            SessionPermission::Write => Need::None,
        },
    }
}

/// The subtree or object a call names, when it names one: the path an approval
/// card shows and the hint a review row renders.
pub fn call_path(tool: &str, args: &Map<String, Value>) -> Option<String> {
    let named = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if tool == "edit_subtree" {
        return named("path");
    }
    ["path", "target", "track"].into_iter().find_map(named)
}

/// One argument object as the model reads it in a refusal: bounded, so a
/// state blob cannot flood the turn.
fn argument_summary(args: &Map<String, Value>) -> String {
    let text = Value::Object(args.clone()).to_string();
    if text.chars().count() > 500 {
        format!("{}...", text.chars().take(500).collect::<String>())
    } else {
        text
    }
}

/// A call the person declined. It is a normal result, not a failure: the turn
/// continues so the model can propose something else.
pub fn declined(tool: &str, args: &Map<String, Value>, target: Option<&str>) -> Value {
    let called = match target {
        Some(target) => format!("{tool} for \"{target}\""),
        None => format!("{tool} (arguments: {})", argument_summary(args)),
    };
    let mut payload = json!({
        "declined": true,
        "tool": tool,
        "arguments": args,
        "error": format!(
            "the user declined {called}; nothing in the project changed. Do not send the same call again; propose a different change or ask what the user wants instead."
        ),
    });
    if let Some(target) = target {
        payload["target"] = json!(target);
    }
    payload
}

/// A call that needs a person in a session with nobody to ask.
pub fn unapproved(tool: &str, args: &Map<String, Value>) -> Value {
    json!({
        "declined": false,
        "refused": true,
        "tool": tool,
        "arguments": args,
        "error": format!(
            "no user is available to approve {tool} (arguments: {}); a call that changes the project cannot run in a session with nobody to ask, so nothing in the project changed. Make a reversible edit instead, or say what you would remove and why.",
            argument_summary(args)
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_need_nothing_and_a_confirm_tool_asks_until_allowed() {
        for mode in [
            SessionPermission::RequireApproval,
            SessionPermission::Autoreview,
            SessionPermission::Write,
        ] {
            assert_eq!(gate("project_get", mode, &[]).need, Need::None);
            assert!(!gate("project_get", mode, &[]).modifies);
            assert_eq!(gate("tracks_remove", mode, &[]).need, Need::Approval);
            assert!(
                gate("unknown_tool", mode, &[])
                    == Gate {
                        modifies: false,
                        need: Need::None
                    }
            );
        }
        let allowed = vec!["tracks_remove".to_owned()];
        assert_eq!(
            gate("tracks_remove", SessionPermission::Write, &allowed).need,
            Need::None
        );
        assert_eq!(
            gate(
                "tracks_remove",
                SessionPermission::RequireApproval,
                &allowed
            )
            .need,
            Need::Approval,
            "an allowed tool still asks when the mode asks for everything"
        );
    }

    #[test]
    fn a_modifying_call_follows_the_mode() {
        let need = |mode| gate("edit_subtree", mode, &[]).need;
        assert_eq!(need(SessionPermission::RequireApproval), Need::Approval);
        assert_eq!(need(SessionPermission::Autoreview), Need::Review);
        assert_eq!(need(SessionPermission::Write), Need::None);
        assert!(gate("project_set_tempo", SessionPermission::Write, &[]).modifies);
    }

    #[test]
    fn a_path_comes_from_the_first_named_field() {
        let args = |value: Value| value.as_object().unwrap().clone();
        assert_eq!(
            call_path("edit_subtree", &args(json!({"path":" track:a "}))).as_deref(),
            Some("track:a")
        );
        assert_eq!(
            call_path("x", &args(json!({"target":"master","track":"b"}))).as_deref(),
            Some("master")
        );
        assert_eq!(call_path("x", &args(json!({"path":"  "}))), None);
        let long = args(json!({"blob":"x".repeat(2000)}));
        let refused = unapproved("tracks_remove", &long);
        assert!(refused["error"].as_str().unwrap().len() < 1000);
        assert_eq!(
            declined("tracks_remove", &long, Some("Bass"))["target"],
            "Bass"
        );
    }
}
