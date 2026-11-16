//! The prompt the backend owns for writing a conversation summary.
//!
//! The model reads only the part of a saved conversation a compaction drops,
//! never the whole history on disk. The order of the requested sections is the
//! order a continuing model needs: what the person wants, what was decided,
//! where the project stands, what is broken and what remains.

/// System prompt for one compaction summary request.
pub const COMPACTION_PROMPT: &str = "Summarize the conversation below so another model can continue the work. Write plain text for that model, not for the person. Keep this order:
1. The person's goals, explicit requests and constraints.
2. Decisions taken and options rejected.
3. Current project state by path, for example track:<name> or preset:<name>.
4. Open problems, errors and unanswered questions.
5. What remains to do, in order.
Do not invent anything. Do not copy tool output. Be concise.";

/// Tokens the prompt costs in every request, sized with the store's estimator.
pub fn prompt_tokens() -> u64 {
    crate::ai::sessions::estimate_text_tokens(COMPACTION_PROMPT)
}
