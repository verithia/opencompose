//! Typed host notices the model reads as user rows. The display transcript and
//! its localization stay separate: the row carries the typed value, and its
//! English text is derived from that value alone.

use crate::edit_history::{
    Author, MAX_OUTSIDE_AUTHOR_CHARS, MAX_OUTSIDE_COUNT, MAX_OUTSIDE_ENTRIES, OutsideEntry,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Longest project path a notice may carry.
pub const MAX_NOTICE_PATH_CHARS: usize = 4096;

/// A host notice a saved conversation carries. New variants extend this enum;
/// each kind states whether a fold and a compaction keep it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostNotice {
    /// The conversation's project binding changed.
    ProjectBound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        cause: BindCause,
    },
    /// Other people, conversations or clients edited the bound project while
    /// this conversation was away, or undo moved it back.
    OutsideChanges {
        entries: Vec<OutsideEntry>,
        more: usize,
        undone: usize,
        #[serde(default)]
        rewritten: bool,
    },
}

/// Why a conversation's project binding changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindCause {
    /// A person chose the project for this conversation.
    Chosen,
    /// The project file moved and the conversation followed it.
    Moved,
}

/// A binding a saved conversation owes the model on its next turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub cause: BindCause,
}

impl HostNotice {
    /// The stable kind tag, used to keep the newest notice of each kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ProjectBound { .. } => "project_bound",
            Self::OutsideChanges { .. } => "outside_changes",
        }
    }

    /// Whether a fold or a compaction keeps the newest row of this kind: the
    /// model needs the current binding to interpret every later path.
    pub fn pinned(&self) -> bool {
        matches!(self, Self::ProjectBound { .. })
    }

    /// Whether every path and count is bounded, so an untrusted saved row
    /// cannot grow the transcript without limit.
    pub fn valid(&self) -> bool {
        match self {
            Self::ProjectBound { from, to, .. } => {
                !to.is_empty()
                    && to.chars().count() <= MAX_NOTICE_PATH_CHARS
                    && from.as_ref().is_none_or(|path| {
                        !path.is_empty() && path.chars().count() <= MAX_NOTICE_PATH_CHARS
                    })
            }
            Self::OutsideChanges {
                entries,
                more,
                undone,
                rewritten: _,
            } => {
                entries.len() <= MAX_OUTSIDE_ENTRIES
                    && *more <= MAX_OUTSIDE_COUNT
                    && *undone <= MAX_OUTSIDE_COUNT
                    && entries.iter().all(valid_outside_entry)
            }
        }
    }

    /// The English text the model reads, derived only from the typed value.
    pub fn model_text(&self) -> String {
        match self {
            Self::ProjectBound { from, to, cause } => match (cause, from) {
                (BindCause::Chosen, Some(from)) => format!(
                    "This conversation is now bound to the project file {to}. Edits apply there; \
                     earlier paths in this conversation referred to {from}."
                ),
                (BindCause::Chosen, None) => format!(
                    "This conversation is now bound to the project file {to}. Edits apply there."
                ),
                (BindCause::Moved, Some(from)) => format!(
                    "The project file of this conversation moved from {from} to {to}. Edits apply \
                     at the new path."
                ),
                (BindCause::Moved, None) => {
                    format!("The project file of this conversation is {to}. Edits apply there.")
                }
            },
            Self::OutsideChanges {
                entries,
                more,
                undone,
                rewritten,
            } => {
                let mut text = String::from(
                    "Since your previous turn, the project changed outside this conversation.",
                );
                if *undone > 0 {
                    text.push_str(&format!(" Undo moved it back {undone} {}.", steps(*undone)));
                }
                if !entries.is_empty() {
                    let facts: Vec<Value> = entries.iter().map(outside_entry_fact).collect();
                    text.push_str(&format!(
                        " Edits by others, newest first: {}.",
                        Value::Array(facts)
                    ));
                }
                if *more > 0 {
                    text.push_str(&format!(" {more} more {}.", more_edits(*more)));
                }
                if *rewritten {
                    text.push_str(
                        " The history before your previous turn was rewritten, so earlier edits \
                         you saw may be gone.",
                    );
                }
                text
            }
        }
    }

    /// A copy whose paths, entries and counts fit the saved bound.
    fn bounded(&self) -> Self {
        match self {
            Self::ProjectBound { from, to, cause } => Self::ProjectBound {
                from: from.as_ref().map(|path| bounded_path(path)),
                to: bounded_path(to),
                cause: *cause,
            },
            Self::OutsideChanges {
                entries,
                more,
                undone,
                rewritten,
            } => {
                let mut entries = entries.clone();
                let mut more = *more;
                if entries.len() > MAX_OUTSIDE_ENTRIES {
                    more += entries.len() - MAX_OUTSIDE_ENTRIES;
                    entries.truncate(MAX_OUTSIDE_ENTRIES);
                }
                Self::OutsideChanges {
                    entries,
                    more,
                    undone: *undone,
                    rewritten: *rewritten,
                }
            }
        }
    }
}

fn valid_outside_entry(entry: &OutsideEntry) -> bool {
    let bounded_author = match &entry.author {
        Author::Person | Author::Outside => true,
        Author::Agent { session } => session.as_ref().is_none_or(|session| {
            !session.is_empty() && session.chars().count() <= MAX_OUTSIDE_AUTHOR_CHARS
        }),
        Author::Client { owner } => {
            !owner.is_empty() && owner.chars().count() <= MAX_OUTSIDE_AUTHOR_CHARS
        }
    };
    bounded_author && entry.changes.changes.len() <= oc_dsl::change::MAX_CHANGES
}

fn outside_entry_fact(entry: &OutsideEntry) -> Value {
    let mut fact = serde_json::Map::new();
    fact.insert(
        "author".into(),
        serde_json::to_value(&entry.author).unwrap_or(Value::Null),
    );
    if !entry.cause.is_edit() {
        fact.insert(
            "cause".into(),
            serde_json::to_value(entry.cause).unwrap_or(Value::Null),
        );
    }
    fact.insert(
        "changes".into(),
        serde_json::to_value(&entry.changes).unwrap_or(Value::Null),
    );
    Value::Object(fact)
}

fn steps(count: usize) -> &'static str {
    if count == 1 { "step" } else { "steps" }
}

fn more_edits(count: usize) -> &'static str {
    if count == 1 {
        "edit is not listed"
    } else {
        "edits are not listed"
    }
}

fn bounded_path(path: &str) -> String {
    if path.chars().count() <= MAX_NOTICE_PATH_CHARS {
        return path.to_owned();
    }
    let mut clipped: String = path
        .chars()
        .take(MAX_NOTICE_PATH_CHARS.saturating_sub(1))
        .collect();
    clipped.push('…');
    clipped
}

/// The saved row a notice becomes: a user-kind row whose metadata carries the
/// typed value and whose content is the text the model reads.
pub fn notice_row(notice: &HostNotice) -> Value {
    let notice = notice.bounded();
    json!({
        "role": "user",
        "message_meta": {
            "version": 1,
            "kind": "host_notice",
            "notice": notice,
        },
        "content": notice.model_text(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_text_names_the_old_path_only_when_there_is_one() {
        let bound = HostNotice::ProjectBound {
            from: Some("/songs/old.ocp".into()),
            to: "/songs/new.ocp".into(),
            cause: BindCause::Chosen,
        };
        assert!(bound.model_text().contains("/songs/old.ocp"));
        let fresh = HostNotice::ProjectBound {
            from: None,
            to: "/songs/new.ocp".into(),
            cause: BindCause::Chosen,
        };
        assert!(!fresh.model_text().contains("referred to"));
        assert!(fresh.model_text().contains("/songs/new.ocp"));
    }

    #[test]
    fn moved_text_names_both_paths() {
        let moved = HostNotice::ProjectBound {
            from: Some("/songs/old.ocp".into()),
            to: "/songs/new.ocp".into(),
            cause: BindCause::Moved,
        };
        let text = moved.model_text();
        assert!(text.contains("/songs/old.ocp"));
        assert!(text.contains("/songs/new.ocp"));
        assert!(text.contains("moved"));
    }

    #[test]
    fn a_notice_row_carries_the_typed_value_and_derived_text() {
        let notice = HostNotice::ProjectBound {
            from: None,
            to: "/songs/song.ocp".into(),
            cause: BindCause::Chosen,
        };
        let row = notice_row(&notice);
        assert_eq!(row["role"], "user");
        assert_eq!(row["message_meta"]["version"], 1);
        assert_eq!(row["message_meta"]["kind"], "host_notice");
        assert_eq!(row["message_meta"]["notice"]["kind"], "project_bound");
        assert_eq!(row["message_meta"]["notice"]["to"], "/songs/song.ocp");
        assert_eq!(row["content"], notice.model_text());
        assert!(notice.pinned());
        assert!(notice.valid());
    }

    #[test]
    fn an_oversized_path_is_clipped_when_the_row_is_written() {
        let long = "x".repeat(MAX_NOTICE_PATH_CHARS + 20);
        let row = notice_row(&HostNotice::ProjectBound {
            from: None,
            to: long,
            cause: BindCause::Chosen,
        });
        let to = row["message_meta"]["notice"]["to"].as_str().unwrap();
        assert_eq!(to.chars().count(), MAX_NOTICE_PATH_CHARS);
        assert!(to.ends_with('…'));
        let notice: HostNotice =
            serde_json::from_value(row["message_meta"]["notice"].clone()).unwrap();
        assert!(notice.valid(), "a saved notice stays readable");
    }
}
