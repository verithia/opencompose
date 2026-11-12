//! Typed arguments for agent transactions and saved sessions.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Commit the active agent transaction and squash its edits into one undo entry.
pub struct TransactionCommitArgs {}

impl ToolArguments for TransactionCommitArgs {
    const ID: ToolId = ToolId::TransactionCommit;
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Discard this turn's agent transaction, restoring the project and undo history to before the turn's first edit. The panel asks the user before it runs.
pub struct TransactionRollbackArgs {}

impl ToolArguments for TransactionRollbackArgs {
    const ID: ToolId = ToolId::TransactionRollback;
}

#[derive(Debug, Deserialize, JsonSchema)]
/// List saved AI conversations newest first, with title, turns, tokens and update time. Omit project to use the open project; all lists every project.
pub struct SessionsListArgs {
    #[schemars(description = "Optional absolute .ocp path; omit for the open project")]
    pub project: Option<String>,
    #[schemars(description = "List every project's conversations")]
    pub all: Option<bool>,
}

impl ToolArguments for SessionsListArgs {
    const ID: ToolId = ToolId::SessionsList;
    const FIELD_ORDER: &'static [&'static str] = &["project", "all"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Return a saved conversation's metadata and its wire messages, ready to replay. Loading makes it the autosave target.
pub struct SessionsLoadArgs {
    #[schemars(description = "Session id from sessions_list")]
    pub id: String,
}

impl ToolArguments for SessionsLoadArgs {
    const ID: ToolId = ToolId::SessionsLoad;
    const FIELD_ORDER: &'static [&'static str] = &["id"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Delete a saved AI conversation and its messages.
pub struct SessionsDeleteArgs {
    #[schemars(description = "Session id")]
    pub id: String,
}

impl ToolArguments for SessionsDeleteArgs {
    const ID: ToolId = ToolId::SessionsDelete;
    const FIELD_ORDER: &'static [&'static str] = &["id"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Start a saved AI conversation and make it the one this session autosaves into. parent forks that conversation.
pub struct SessionsNewArgs {
    #[schemars(description = "Optional absolute .ocp path; omit for the open project")]
    pub project: Option<String>,
    #[schemars(description = "Optional title; the first user message is the default")]
    pub title: Option<String>,
    #[schemars(description = "Session id to fork")]
    pub parent: Option<String>,
}

impl ToolArguments for SessionsNewArgs {
    const ID: ToolId = ToolId::SessionsNew;
    const FIELD_ORDER: &'static [&'static str] = &["project", "title", "parent"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Rename a saved AI conversation.
pub struct SessionsRenameArgs {
    #[schemars(description = "Session id")]
    pub id: String,
    #[schemars(description = "New title")]
    pub title: String,
}

impl ToolArguments for SessionsRenameArgs {
    const ID: ToolId = ToolId::SessionsRename;
    const FIELD_ORDER: &'static [&'static str] = &["id", "title"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Fold a saved conversation's older turns into one plain-text summary that also lists the most recent tool calls as text. No tool call, provider state or signature survives, so a model that requires thought signatures can replay it. Returns the compacted messages.
pub struct SessionsCompactArgs {
    #[schemars(description = "Session id")]
    pub id: String,
    #[schemars(description = "Optional token target for the summary")]
    pub target_tokens: Option<u64>,
}

impl ToolArguments for SessionsCompactArgs {
    const ID: ToolId = ToolId::SessionsCompact;
    const FIELD_ORDER: &'static [&'static str] = &["id", "target_tokens"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Begin an atomic batch of agent edits. Returns the base revision to pass as expected_revision until the batch is committed or rolled back.
pub struct TransactionBeginArgs {
    #[schemars(description = "Optional batch label")]
    pub label: Option<String>,
}

impl ToolArguments for TransactionBeginArgs {
    const ID: ToolId = ToolId::TransactionBegin;
    const FIELD_ORDER: &'static [&'static str] = &["label"];
}
