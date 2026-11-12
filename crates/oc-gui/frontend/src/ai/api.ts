import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProjectSnapshot } from "../types/project";
import type { ModelLimits } from "../stores/aiLimits";
import type { AuthKind, Wire } from "./config";
import type { ToolDef, WireMessage } from "./agent";
import type { PermissionMode } from "./permission";
import type { CompactionResult, SessionMeta } from "./sessions";
import type { SkillRef } from "./system-prompt";
import { toolsetArg } from "./toolsets";
import type { DiffResult } from "./transcript";
import { invoke } from "./transport";

export function cancel(requestId: string) {
  return invoke<void>("ai_cancel", { requestId });
}

export function chat(
  args: { providerId: string; requestId: string; sessionId?: string; messages: WireMessage[]; useTools: boolean },
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
  return invoke<WireMessage>("ai_chat", { ...args, onDelta, onThinking, onPreparing });
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

export function applyEdit(newDsl: string, targetPath: string, expectedDsl: string) {
  return invoke<ProjectSnapshot>("ai_apply_edit", { newDsl, targetPath, expectedDsl });
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

export function loadSessionSnapshot(id: string, sha: string) {
  return invoke<{ dsl: string }>("sessions_version", { id, sha });
}

export function rewindSession(id: string, keep: number) {
  return invoke<void>("sessions_rewind", { id, keep });
}

export function checkpointSession(
  id: string,
  messages: WireMessage[],
  outcome?: { status: "applied" | "rejected"; path: string },
) {
  return invoke<void>("sessions_checkpoint", { id, messages, ...outcome });
}

export function listenSession(handler: (payload: unknown) => void) {
  return listen("ai_session", (event) => handler(event.payload));
}
