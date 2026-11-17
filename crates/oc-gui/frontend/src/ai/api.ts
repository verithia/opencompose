import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProjectSnapshot } from "../types/project";
import type { ChangeSet } from "../types/change";
import type { HistoryAuthor } from "../types/undo";
import type { ModelLimits } from "../stores/aiLimits";
import type { AuthKind, Wire } from "./config";
import type { ApprovalRequest, PendingQuestion, ToolDef, TurnStop, UserQuestion, WireMessage } from "./wire";
import type { PermissionMode } from "./permission";
import type { ClearReport, CompactionResult, SessionMeta } from "./sessions";
import type { SkillRef } from "./system-prompt";
import { toolsetArg } from "./toolsets";
import type { DiffResult } from "./transcript";
import type { MediaAttachment, MediaNotice } from "./media";
import { invoke } from "./transport";

export function cancel(requestId: string) {
  return invoke<void>("ai_cancel", { requestId });
}

/** One saved message, a tool starting, a question, or a note, from a running turn. */
export type TurnEvent =
  | { type: "message"; message: WireMessage }
  | { type: "running"; call_id: string; tool: string }
  | { type: "ask"; request: ApprovalRequest }
  | { type: "ask_user"; request: UserQuestion }
  | { type: "expired"; call_id: string }
  | { type: "preview"; call_id: string; tool: string; path?: string; before: string; after: string }
  | { type: "wire_folded"; before_rows: number; before_bytes: number; after_rows: number; after_bytes: number; folded: number; kept: number }
  | { type: "media"; notices: MediaNotice[] };

/** A turn event and the turn it belongs to. */
export type TurnEnvelope = TurnEvent & { request_id: string; session_id: string };

/** How a turn ended; `failed` marks a turn that did not answer. */
export interface TurnOutcome {
  text: string;
  failed: boolean;
  stop?: TurnStop;
  /** The typed facts the committed batch produced, reported once per turn. */
  changes?: ChangeSet;
  error?: {
    kind: string;
    message: string;
    reauth?: boolean;
    retryable?: boolean;
    /** A typed cause, such as the two paths of a `session_binding` refusal. */
    detail?: { kind: string; bound?: string; open?: string };
  };
}

/** A person's answer to a question a running turn asked. */
export interface TurnDecision { approved: boolean; target?: string; always?: boolean }

/** A person's answer to `ask_user`: a chosen option, free text, or both. */
export interface TurnReply { option?: number; text?: string }

/** Why the host refused a question answer. */
export type ReplyError =
  | "not_waiting"
  | "option_out_of_range"
  | "text_not_accepted"
  | "text_too_long"
  | "turn_stopped";

const REPLY_ERRORS: Readonly<Record<ReplyError, true>> = {
  not_waiting: true,
  option_out_of_range: true,
  text_not_accepted: true,
  text_too_long: true,
  turn_stopped: true,
};

/** The typed refusal behind a rejected `ai_turn_reply`, or null when it was not one. */
export function replyErrorOf(error: unknown): ReplyError | null {
  return typeof error === "string" && Object.hasOwn(REPLY_ERRORS, error)
    ? error as ReplyError : null;
}

/**
 * Runs one agent turn in the backend. `messages` is the system prompt and the
 * new user message, or the system prompt alone to resume the saved
 * conversation; the backend replays the history from the store.
 */
export function startTurn(
  args: {
    providerId: string;
    requestId: string;
    sessionId: string;
    messages: WireMessage[];
    toolsets: string[] | null;
    label: string;
    allowed: readonly string[];
    /** Whether the system prompt asks for tool calls written as text. */
    textProtocol: boolean;
  },
  delta: (text: string) => void,
  thinking: (text: string) => void,
  preparing?: (name: string) => void,
) {
  const onDelta = new Channel<string>();
  onDelta.onmessage = delta;
  const onThinking = new Channel<string>();
  onThinking.onmessage = thinking;
  const onPreparing = new Channel<string>();
  onPreparing.onmessage = name => preparing?.(name);
  return invoke<TurnOutcome>("ai_turn_start", {
    providerId: args.providerId,
    requestId: args.requestId,
    sessionId: args.sessionId,
    messages: args.messages,
    ...(args.toolsets ? { toolsets: args.toolsets } : {}),
    label: args.label,
    allowed: [...args.allowed],
    textProtocol: args.textProtocol,
    onDelta, onThinking, onPreparing,
  });
}

/** Resolves once the running turn has saved the message at a round boundary. */
export function steerTurn(requestId: string, sessionId: string, message: { id?: string; content: string; media: WireMessage["media"] }) {
  return invoke<void>("ai_turn_steer", { requestId, sessionId, message });
}

export function answerTurn(requestId: string, callId: string, decision: TurnDecision) {
  return invoke<void>("ai_turn_answer", { requestId, callId, decision });
}

/** Answers a question the model asked, without an approval decision. */
export function replyTurn(requestId: string, callId: string, reply: TurnReply) {
  return invoke<void>("ai_turn_reply", { requestId, callId, reply });
}

/** The questions a running turn waits on, for a window that reloaded. */
export function turnQuestions(requestId: string) {
  return invoke<PendingQuestion[]>("ai_turn_questions", { requestId });
}

export function listenTurn(handler: (event: TurnEnvelope) => void) {
  return listen<TurnEnvelope>("ai_turn", (event) => handler(event.payload));
}

/** The document a turn this panel runs just changed. */
export function listenTurnProject(handler: (snapshot: ProjectSnapshot) => void) {
  return listen<ProjectSnapshot>("ai_turn_project", (event) => handler(event.payload));
}

/** Why a recall did not run; the panel localizes each reason. */
export type RecallRefusal =
  | "turn_running"
  | "batch_open"
  | "other_project"
  | "no_recorded_position"
  | "position_cleared"
  | "not_a_person_message"
  | "conversation_changed"
  | "edits_changed"
  | "preview_open";

/** One author whose edits a recall would undo, and how many. */
export interface RecallOther {
  author: HistoryAuthor;
  count: number;
}

/** The history position a confirmation was shown for. */
export interface RecallToken {
  applied: number;
  newest?: number | null;
}

/** The message a recall cut, for the composer to take back. */
export interface RecallMessage {
  content: string;
  media?: MediaAttachment[];
}

/** The answer to a recall. */
export type RecallOutcome =
  | {
    status: "recalled";
    kept: number;
    message: RecallMessage;
    snapshot?: ProjectSnapshot;
    unsaved?: string;
  }
  | { status: "needs_confirmation"; others: RecallOther[]; token: RecallToken }
  | { status: "refused"; reason: RecallRefusal };

/**
 * Cuts the conversation before a person's message. With `undoEdits` it also
 * moves the project's history cursor back to the position that message
 * recorded; a redo brings the edits back.
 */
export function recall(sessionId: string, index: number, undoEdits: boolean, confirmed: boolean, token?: RecallToken) {
  return invoke<RecallOutcome>("ai_recall", { sessionId, index, undoEdits, confirmed, token });
}

/** The full result behind a truncated preview, while the backend keeps it. */
export function toolResult(sessionId: string, callId: string) {
  return invoke<string | null>("ai_tool_result", { sessionId, callId });
}

/**
 * Commits a batch a turn left open, e.g. after a window reload. Returns the
 * document and, when the file did not take it, why; `null` when no such
 * batch was open.
 */
export function recoverBatch() {
  return invoke<{ snapshot: ProjectSnapshot; unsaved?: string } | null>("ai_batch_recover");
}

/** Whether a saved key is usable; a cached refusal keeps its reason. */
export function keyStatus(providerId: string) {
  return invoke<boolean>("ai_key_status", { providerId });
}

/** Ask the OS credential store again, bypassing a cached refusal. */
export function readKey(providerId: string) {
  return invoke<boolean>("ai_read_key", { providerId });
}

export function setKey(providerId: string, key: string) {
  return invoke<void>("ai_set_key", { providerId, key });
}

export function removeKey(providerId: string) {
  return invoke<void>("ai_remove_key", { providerId });
}

/** One model id returned by a provider listing, with optional advertised caps. */
export interface ListedModel {
  id: string;
  context?: number | null;
  max_output?: number | null;
}

export interface ModelListing {
  models: ListedModel[];
  /** Distinct ids the provider returned, before the backend's cap. */
  total: number;
}

export interface ProviderDraft {
  name: string;
  wire: Wire;
  endpoint: string;
  auth: AuthKind;
  models: string[];
  limits: Record<string, ModelLimits>;
}

export function fetchModels(args: {
  endpoint: string;
  wire: Wire;
  auth: AuthKind;
  key: string | null;
}) {
  return invoke<ModelListing>("ai_fetch_models", args);
}

export function addProvider(draft: ProviderDraft) {
  return invoke<{ provider_id: string }>("ai_add_provider", { draft });
}
export function readActiveTurn() {
  return invoke<unknown>("ai_active_turn");
}

export function buildContext(targetPath: string, userInstruction: string) {
  return invoke<{ dsl_context: string }>("ai_build_context", { targetPath, userInstruction });
}

export function listTools(selection: string[] | null) {
  return invoke<ToolDef[]>("ai_tools", toolsetArg(selection));
}

export function listSkills() {
  return invoke<SkillRef[]>("ai_skills");
}

export function getProject() {
  return invoke<ProjectSnapshot>("get_project");
}

export function validateEdit(newDsl: string, targetPath: string) {
  return invoke<{ valid: boolean; errors?: string[] }>("ai_validate_edit", { newDsl, targetPath });
}

export function diffDsl(oldDsl: string, newDsl: string) {
  return invoke<DiffResult>("diff_dsl", { oldDsl, newDsl });
}

export function listSessions() {
  return invoke<unknown>("sessions_list", {});
}

export function loadSession(id: string) {
  return invoke<unknown>("sessions_load", { id });
}

/** The newest page of a saved conversation, with where it sits in the whole. */
export function openSession(id: string, turns?: number) {
  return invoke<unknown>("sessions_open", { id, ...(turns === undefined ? {} : { turns }) });
}

/** The page of turns before `before`, or a refusal when the view changed. */
export function sessionPage(id: string, before: number, revision: string, turns?: number) {
  return invoke<unknown>("sessions_page", {
    id,
    before,
    ...(turns === undefined ? {} : { turns }),
    revision,
  });
}

/** Binds a conversation to the project that is open now. */
export function bindSession(id: string) {
  return invoke<SessionMeta>("sessions_bind", { id });
}

/** Whether a conversation works on the open project. */
export function sessionBinding(id: string) {
  return invoke<unknown>("sessions_binding", { id });
}

export function newSession(title?: string, parent?: string, through?: number) {
  return invoke<SessionMeta>("sessions_new", {
    ...(title ? { title } : {}),
    ...(parent ? { parent } : {}),
    ...(through === undefined ? {} : { through }),
  });
}

export function nameSession(id: string, providerId: string) {
  return invoke<void>("ai_name_session", { id, providerId });
}

export function renameSession(id: string, title: string) {
  return invoke<void>("sessions_rename", { id, title });
}

export function deleteSession(id: string) {
  return invoke<void>("sessions_delete", { id });
}

export function setSessionPermission(id: string, permission: PermissionMode) {
  return invoke<void>("sessions_set_permission", { id, permission });
}

/** Sets the conversation's goal, or clears it with `null`. */
export function setSessionGoal(id: string, text: string | null) {
  return invoke<SessionMeta>("sessions_set_goal", { id, text });
}

export function compactSession(id: string) {
  return invoke<CompactionResult>("sessions_compact", { id });
}

export function clearSessionHistory(id: string) {
  return invoke<ClearReport>("sessions_clear_history", { id });
}

export function clearAllSessionHistory() {
  return invoke<ClearReport>("sessions_clear_all_history");
}

export function originalSession(id: string) {
  return invoke<{ messages: WireMessage[]; compactions: number }>("sessions_original", { id });
}

export function saveSessionSnapshot(id: string, dsl: string) {
  return invoke<{ sha: string }>("sessions_snapshot", { id, dsl });
}

export function listenSession(handler: (payload: unknown) => void) {
  return listen("ai_session", (event) => handler(event.payload));
}
