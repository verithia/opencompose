import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProjectSnapshot } from "../types/project";
import type { ModelLimits } from "../stores/aiLimits";
import type { AuthKind, Wire } from "./config";
import type { ApprovalRequest, ToolDef, TurnStop, WireMessage } from "./wire";
import type { PermissionMode } from "./permission";
import type { CompactionResult, SessionMeta } from "./sessions";
import type { SkillRef } from "./system-prompt";
import { toolsetArg } from "./toolsets";
import type { DiffResult } from "./transcript";
import type { MediaNotice } from "./media";
import { invoke } from "./transport";

export function cancel(requestId: string) {
  return invoke<void>("ai_cancel", { requestId });
}

/** One saved message, a tool starting, a question, or a note, from a running turn. */
export type TurnEvent =
  | { type: "message"; message: WireMessage }
  | { type: "running"; call_id: string; tool: string }
  | { type: "ask"; request: ApprovalRequest }
  | { type: "preview"; call_id: string; tool: string; path?: string; before: string; after: string }
  | { type: "notice"; text: string; failed: boolean }
  | { type: "media"; notices: MediaNotice[] };

/** A turn event and the turn it belongs to. */
export type TurnEnvelope = TurnEvent & { request_id: string; session_id: string };

/** How a turn ended; `failed` marks a turn that did not answer. */
export interface TurnOutcome {
  text: string;
  failed: boolean;
  stop?: TurnStop;
  error?: { kind: string; message: string; reauth?: boolean; retryable?: boolean };
}

/** A person's answer to a question a running turn asked. */
export interface TurnDecision { approved: boolean; target?: string; always?: boolean }

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
    onDelta, onThinking, onPreparing,
  });
}

export function answerTurn(requestId: string, callId: string, decision: TurnDecision) {
  return invoke<void>("ai_turn_answer", { requestId, callId, decision });
}

/** The questions a running turn waits on, for a window that reloaded. */
export function turnQuestions(requestId: string) {
  return invoke<ApprovalRequest[]>("ai_turn_questions", { requestId });
}

export function listenTurn(handler: (event: TurnEnvelope) => void) {
  return listen<TurnEnvelope>("ai_turn", (event) => handler(event.payload));
}

/** The document a turn this panel runs just changed. */
export function listenTurnProject(handler: (snapshot: ProjectSnapshot) => void) {
  return listen<ProjectSnapshot>("ai_turn_project", (event) => handler(event.payload));
}

/**
 * Restores the project to the snapshot a person's message was sent against
 * and cuts the conversation before it, as one undo step.
 */
export function revertBefore(sessionId: string, index: number) {
  return invoke<{ kept: number; snapshot: ProjectSnapshot }>("ai_revert_before", { sessionId, index });
}

/** The full result behind a truncated preview, while the backend keeps it. */
export function toolResult(sessionId: string, callId: string) {
  return invoke<string | null>("ai_tool_result", { sessionId, callId });
}

/**
 * Commits a batch a panel turn left open, e.g. after a window reload. Returns
 * the document, or `null` when no panel batch was open.
 */
export function recoverBatch() {
  return invoke<ProjectSnapshot | null>("ai_batch_recover");
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

export function compactSession(id: string) {
  return invoke<CompactionResult>("sessions_compact", { id });
}

export function dropSessionHistory(id: string, keep: number) {
  return invoke<{ dropped_records: number; removed_assets: number }>("sessions_drop_history", { id, keep });
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
