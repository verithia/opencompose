/**
 * The saved-conversation events and payloads the backend sends. A session is
 * persisted without the panel asking: every turn appends to
 * `~/.opencompose/ai/sessions`, and an auto-compaction is announced so the
 * panel can say what happened instead of silently losing history.
 */
import type { TurnStats } from "../stores/aiStats";
import type { WireMessage } from "./wire";
import type { PermissionMode } from "./permission";
import { i18n } from "../stores/i18n";

/** What a compaction dropped and kept, from `ai_session` or `sessions_compact`. */
export interface CompactionReport {
  /** Message indexes [from, to) folded into the summary. */
  dropped_from: number;
  dropped_to: number;
  tokens_before: number;
  tokens_after: number;
  /** Tool calls the summary carries as text. */
  retained_pairs: number;
}

/** Payload of the backend's `ai_session` event after an auto-compaction. */
export interface SessionCompaction extends CompactionReport {
  session_id: string;
  compacted: boolean;
  provider: string;
  model: string;
  /** The conversation as it was sent after compaction. */
  messages: WireMessage[];
}

/** One archive file a turn evicted to stay inside its budget. */
export interface EvictedAudition {
  /** `audition-media` or `audition-renders`. */
  dir: string;
  file: string;
  bytes: number;
}

/** Payload of the backend's `ai_session` event after an audition eviction. */
export interface SessionArchiveEviction {
  session_id: string;
  evicted: EvictedAudition[];
}

/** One saved conversation, as the list command reports it. */
export interface SessionMeta {
  /** A person or the naming pass chose the title; otherwise it is provisional. */
  named?: boolean;
  id: string;
  title: string;
  created: string;
  updated: string;
  model: string;
  provider: string;
  /** Directory the conversation belongs to; `detached` when there is none. */
  workspace_key: string;
  /** The project file this conversation is bound to. */
  project_path?: string | null;
  /** A binding the next turn tells the model about. */
  pending_binding?: { from?: string | null; cause: "chosen" | "moved" } | null;
  workspace?: string | null;
  /** What this conversation may do to the DSL without asking; absent on an
   * older payload, which reads as the default. */
  permission?: PermissionMode;
  turns: number;
  tokens: number;
  compacted_at?: string | null;
  parent?: string | null;
}

export interface AccountingError { code: string; message: string }
export interface SessionUsageTotals {
  requests: number; attempts: number; input: number; output: number; reasoning: number;
  cached_input: number; cache_input: number; failed_attempts: number;
  unknown_price_attempts: number; unreported_usage_attempts: number;
  interrupted_requests: number; legacy_unknown: boolean;
  ttft_ms_sum: number; ttft_count: number; decode_tps_sum: number; decode_tps_count: number;
  costs: Record<string, { amount: number; attempts: number }>;
}
export interface SessionUsageSnapshot {
  session_id: string; incarnation: string; revision: number;
  totals: SessionUsageTotals; pending_requests: number; last_turn?: TurnStats;
}
export interface AccountingUpdate {
  session_id: string; incarnation: string; revision: number;
  snapshot: SessionUsageSnapshot; saved: boolean; error?: AccountingError;
}
export interface StatsEvent extends TurnStats {
  client_request_id: string; purpose: "chat" | "title"; accounting?: AccountingUpdate;
}

/** `sessions_load`: the metadata plus the wire conversation to replay. */
export interface LoadedSession {
  meta: SessionMeta;
  messages: WireMessage[];
  skipped: number;
  /** A compaction replaced older turns; the panel offers them behind a chip. */
  compacted?: boolean;
  usage?: SessionUsageSnapshot;
  usage_error?: AccountingError;
}

/** `sessions_open`: the newest page of a saved conversation and its span. */
export interface LoadedPage extends LoadedSession {
  /** Index in the whole view where `messages` begins. */
  start: number;
  /** Length of the whole view. */
  total: number;
  /** The view's shape, which a later page must still match. */
  revision: string;
  /** The panel estimate of the messages before `start`. */
  older_tokens: number;
}

/** One older page of the whole view. */
export interface SessionPage {
  messages: WireMessage[];
  start: number;
  total: number;
  revision: string;
  older_tokens: number;
}

/** `sessions_page`: the page, or a refusal when the view changed shape. */
export type SessionPageRead =
  | ({ status: "page" } & SessionPage)
  | { status: "stale" };

/** `sessions_compact`: the conversation after folding. */
export interface CompactionResult extends CompactionReport {
  id: string;
  compacted_at: string;
  messages: WireMessage[];
}

/** Why a model-written summary was replaced by the mechanical one. */
export type CompactionFailure = "provider_error" | "cancelled" | "empty" | "conflict";

/** Payload of the backend's `ai_session` event after a failed summary call. */
export interface SessionCompactionFailure {
  session_id: string;
  notice: "compaction_failed";
  reason: CompactionFailure;
}

/** What clearing old conversation history dropped and freed. */
export interface ClearReport {
  sessions: number;
  dropped_records: number;
  removed_assets: number;
  freed_bytes: number;
}

export function sessionCompaction(event: unknown): SessionCompaction | null {
  const value = event as Partial<SessionCompaction> | null;
  if (!value || value.compacted !== true || typeof value.tokens_after !== "number") return null;
  return value as SessionCompaction;
}

/** The failure notice a compaction's `ai_session` event carries, if any. */
export function sessionCompactionFailure(event: unknown): SessionCompactionFailure | null {
  const value = event as Partial<SessionCompactionFailure> | null;
  if (!value || value.notice !== "compaction_failed" || typeof value.session_id !== "string") return null;
  if (value.reason !== "provider_error" && value.reason !== "cancelled"
    && value.reason !== "empty" && value.reason !== "conflict") return null;
  return value as SessionCompactionFailure;
}

const COMPACTION_FAILURE_KEYS = {
  provider_error: "ai.sessions.compactionFailedProvider",
  cancelled: "ai.sessions.compactionFailedCancelled",
  empty: "ai.sessions.compactionFailedEmpty",
  conflict: "ai.sessions.compactionFailedConflict",
} as const;

/** The transcript line for a compaction whose summary call failed. */
export function compactionFailureNotice(failure: SessionCompactionFailure): string {
  return i18n.t("ai.sessions.compactionFailed", {
    reason: i18n.t(COMPACTION_FAILURE_KEYS[failure.reason]),
  });
}

/** The eviction report a turn's `ai_session` event carries, if it has one. */
export function sessionArchiveEviction(event: unknown): SessionArchiveEviction | null {
  const value = event as Partial<SessionArchiveEviction> | null;
  if (!value || !Array.isArray(value.evicted) || value.evicted.length === 0) return null;
  if (typeof value.session_id !== "string") return null;
  return { session_id: value.session_id, evicted: value.evicted };
}

/** The transcript line for an eviction: how many, and which files it started with. */
export function archiveNotice(eviction: SessionArchiveEviction): string {
  const names = eviction.evicted
    .slice(0, 3)
    .map((entry) => `${entry.dir}/${entry.file}`)
    .join(", ");
  return i18n.t("ai.sessions.archiveEvicted", {
    count: eviction.evicted.length,
    names,
  });
}

/** The transcript line the panel adds; no numbers are invented when absent. */
export function compactionNotice(compaction: CompactionReport): string {
  const dropped = Math.max(0, compaction.dropped_to - compaction.dropped_from);
  const pairs = compaction.retained_pairs === 1
    ? i18n.t("ai.sessions.retainedOne")
    : i18n.t("ai.sessions.retained", { count: compaction.retained_pairs });
  return i18n.t("ai.sessions.compaction", {
    dropped,
    before: compaction.tokens_before,
    after: compaction.tokens_after,
    pairs,
  });
}

/** The list payload, or an empty list when the backend answered something else. */
export function sessionList(payload: unknown): SessionMeta[] {
  if (!Array.isArray(payload)) return [];
  return payload.filter((entry): entry is SessionMeta => {
    const value = entry as Partial<SessionMeta> | null;
    return !!value && typeof value.id === "string" && typeof value.title === "string";
  });
}

/**
 * `ai_active_turn`: the turn the backend is running, named by the conversation
 * its reply will be saved into. `session_id` is absent until the backend has
 * resolved the conversation, so a window never claims one it was not told.
 */
export interface ActiveTurn {
  request_id: string;
  session_id?: string | null;
}

/** The running turn, or `null` when the backend reports none. */
export function activeTurn(payload: unknown): ActiveTurn | null {
  const value = payload as Partial<ActiveTurn> | null;
  if (!value || typeof value !== "object") return null;
  if (typeof value.request_id !== "string" || !value.request_id) return null;
  return {
    request_id: value.request_id,
    session_id: typeof value.session_id === "string" ? value.session_id : null,
  };
}

/** The loaded conversation, or `null` when the payload is not one. */
export function loadedSession(payload: unknown): LoadedSession | null {
  const value = payload as Partial<LoadedSession> | null;
  if (!value || typeof value !== "object") return null;
  if (!value.meta || typeof value.meta.id !== "string") return null;
  if (!Array.isArray(value.messages)) return null;
  return value as LoadedSession;
}

function pageSpan(value: Partial<SessionPage>): value is SessionPage {
  return typeof value.start === "number"
    && typeof value.total === "number"
    && typeof value.revision === "string"
    && typeof value.older_tokens === "number";
}

/** The newest page of a conversation, or `null` when the payload is not one. */
export function loadedPage(payload: unknown): LoadedPage | null {
  const session = loadedSession(payload);
  if (!session) return null;
  const value = payload as Partial<LoadedPage>;
  return pageSpan(value) ? value as LoadedPage : null;
}

/** An older page, or `null` when the payload names neither status. */
export function sessionPageRead(payload: unknown): SessionPageRead | null {
  const value = payload as Partial<{ status: string }> | null;
  if (!value || typeof value !== "object") return null;
  if (value.status === "stale") return { status: "stale" };
  if (value.status !== "page") return null;
  const page = payload as Partial<SessionPage> & { status?: string };
  if (!Array.isArray(page.messages) || !pageSpan(page)) return null;
  return { status: "page", ...page } as SessionPageRead;
}

/**
 * The directory the list is scoped to: the store's own record when it has one,
 * the open project's directory otherwise. Two `.ocp` files in one directory
 * share one set of conversations, so the project file never narrows the scope.
 */
export function workspaceScope(
  sessions: SessionMeta[],
  projectPath: string | null | undefined,
): string | null {
  const recorded = sessions.find((session) => typeof session.workspace === "string" && session.workspace)?.workspace;
  if (recorded) return recorded;
  return projectPath ? directoryOf(projectPath) : null;
}

/** The directory a project file lives in. */
export function directoryOf(path: string): string | null {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut > 0 ? path.slice(0, cut) : null;
}

/** The name a header shows for a directory: its own, never the whole path. */
export function directoryName(path: string | null | undefined): string | null {
  if (!path) return null;
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut >= 0 ? trimmed.slice(cut + 1) || trimmed : trimmed;
}

/** `meta.updated` as a list row shows it: the recorded time, never a fresh one. */
export function updatedLabel(updated: string): string {
  const at = new Date(updated);
  if (Number.isNaN(at.getTime())) return "";
  return at.toLocaleString(i18n.locale(), { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}
