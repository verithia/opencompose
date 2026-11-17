//! Typed request for a question the panel asks its own user.

use crate::tool_registry::arguments::typed_arguments;
use serde_json::Value;

/// Longest question the loop asks, in characters.
pub const MAX_QUESTION_CHARS: usize = 500;
/// Most options one question may offer.
pub const MAX_OPTIONS: usize = 6;
/// Longest option text, in characters.
pub const MAX_OPTION_CHARS: usize = 80;

typed_arguments! {
    /// Ask the person one question when a decision is theirs. Omit options for a free-text answer, or offer at most six short choices.
    pub struct AskUserArgs => AskUser {
        #[schemars(length(min = 1, max = 500), description = "The question, at most 500 characters")]
        pub question: String,
        #[schemars(length(max = 6), description = "At most six short choices")]
        pub options: Option<Vec<String>>,
        #[schemars(description = "Whether a free-text answer is accepted (default true)")]
        pub free_text: Option<bool>,
    }
}

/// A decoded question that fits the loop's bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionPrompt {
    pub question: String,
    pub options: Vec<String>,
    pub free_text: bool,
}

impl AskUserArgs {
    /// Check the text bounds the JSON schema cannot enforce on an external
    /// caller. A question over the bound is refused before anything is saved.
    pub fn into_prompt(self) -> Result<QuestionPrompt, String> {
        let question = self.question.trim();
        if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
            return Err(format!(
                "question must be 1 to {MAX_QUESTION_CHARS} characters"
            ));
        }
        let options = self.options.unwrap_or_default();
        if options.len() > MAX_OPTIONS {
            return Err(format!("at most {MAX_OPTIONS} options are allowed"));
        }
        if options
            .iter()
            .any(|option| option.trim().is_empty() || option.chars().count() > MAX_OPTION_CHARS)
        {
            return Err(format!(
                "each option must be 1 to {MAX_OPTION_CHARS} characters"
            ));
        }
        Ok(QuestionPrompt {
            question: question.to_owned(),
            options,
            free_text: self.free_text.unwrap_or(true),
        })
    }
}

/// The tool result when nobody answers, with the typed reason.
pub fn unanswered(reason: &str) -> Value {
    serde_json::json!({ "answered": false, "reason": reason })
}
