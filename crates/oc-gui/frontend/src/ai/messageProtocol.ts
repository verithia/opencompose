import { isChangeSet, type ChangeSet, type EditCause } from "../types/change";

/** Local transcript semantics. These tags are never provider API fields. */
export type MessageKind = "user" | "assistant" | "tool" | "application_result" | "round_budget"
  | "audition" | "comparison_reference" | "comparison_candidate" | "summary"
  | "transaction_rollback" | "proposal_outcome" | "host_notice" | "session_state";
export type BindCause = "chosen" | "moved";

/** Who made an edit another conversation reports. */
export type NoticeAuthor =
  | { kind: "person" }
  | { kind: "agent"; session?: string }
  | { kind: "client"; owner: string }
  | { kind: "outside" };

/** One edit another author made while the conversation was away. */
export interface OutsideChange {
  author: NoticeAuthor;
  cause?: EditCause;
  changes: ChangeSet;
}

/** What a tool call that changes the project may do without asking. */
export type SessionPermission = "require_approval" | "autoreview" | "write";

/** A permission mode change between two turns. */
export interface PermissionChange {
  from: SessionPermission;
  to: SessionPermission;
}

/** Who wrote a goal change. */
export type StateAuthor = "person" | "agent";
/** Where one plan step stands. */
export type PlanStatus = "pending" | "in_progress" | "done";
/** One step of a plan. */
export interface PlanItem { text: string; status: PlanStatus }
/** A typed goal or plan change. Only a `session_state` row carries one. */
export type SessionState =
  | { kind: "goal_set"; text: string; author: StateAuthor }
  | { kind: "goal_cleared"; author: StateAuthor }
  | { kind: "plan_set"; items: PlanItem[] };

/** A typed host notice. Only a `host_notice` row carries one. */
export type HostNotice =
  | {
    kind: "project_bound";
    from?: string;
    to: string;
    cause: BindCause;
  }
  | {
    kind: "outside_changes";
    entries: OutsideChange[];
    more: number;
    undone: number;
    own_undone: number;
    rewritten: boolean;
    pruned: boolean;
  }
  | {
    kind: "tool_surface";
    added: string[];
    removed: string[];
    permission?: PermissionChange;
  };

export interface MessageMeta {
  version: 1;
  kind: MessageKind;
  standing_intent?: string;
  /** Bounded prose independent of generated summary headers and inventory. */
  summary_text?: string;
  discarded_auditions?: number;
  rollback_notice?: string;
  proposal_notice?: string;
  audio_range?: { start_seconds: number; end_seconds: number; source: boolean };
  /** A text-protocol call, on its assistant round and on the result answering it. */
  text_call?: { id: string; tool: string };
  /** The queued row a steered message came from. */
  steer_id?: string;
  /** The typed cause of a host notice; untyped notices carry none. */
  notice?: HostNotice;
  /** The typed goal or plan change; only a `session_state` row carries one. */
  state?: SessionState;
}
interface MessageLike {
  role: string;
  content: string | null;
  context?: "audition" | "summary";
  message_meta?: MessageMeta;
  media?: unknown[];
  input_audio?: unknown;
}
export const messageMeta = (kind: MessageKind): MessageMeta => ({ version: 1, kind });
export const isAuditionKind = (kind: MessageKind) =>
  kind === "audition" || kind === "comparison_reference" || kind === "comparison_candidate";

/** A new wire row acquires semantics only from its writer or structural role. */
export function tagNewMessage<T extends MessageLike>(message: T): T {
  if (message.message_meta !== undefined || message.role === "system") return message;
  const kind: MessageKind = message.role === "assistant" ? "assistant"
    : message.role === "tool" ? "tool"
    : message.context === "summary" ? "summary"
    : message.context === "audition" ? "audition" : "user";
  return { ...message, message_meta: messageMeta(kind) };
}

/** English fallback is exclusively for untagged legacy session rows. */
function structuralKind(message: MessageLike): MessageKind {
  return message.role === "assistant" ? "assistant" : message.role === "tool" ? "tool" : "user";
}

const MESSAGE_KINDS: Readonly<Record<MessageKind, true>> = {
  user: true,
  assistant: true,
  tool: true,
  application_result: true,
  round_budget: true,
  audition: true,
  comparison_reference: true,
  comparison_candidate: true,
  summary: true,
  transaction_rollback: true,
  proposal_outcome: true,
  host_notice: true,
  session_state: true,
};

/** Validate an untrusted saved row against the exhaustive kind table. */
function isMessageKind(value: unknown): value is MessageKind {
  return typeof value === "string" && Object.hasOwn(MESSAGE_KINDS, value);
}

/** A saved path bound, shared by every notice kind that names one. */
const MAX_NOTICE_PATH_CHARS = 4096;

/** Entries one outside-changes notice may describe. */
const MAX_OUTSIDE_ENTRIES = 8;

/** Largest count an outside-changes notice may report. */
const MAX_OUTSIDE_COUNT = 1_000_000;

/** Longest author identity an outside-changes notice may carry. */
const MAX_OUTSIDE_AUTHOR_CHARS = 512;

/** Most tool names one surface notice may carry. */
const MAX_TOOL_SURFACE_NAMES = 512;

/** Longest tool name one surface notice may carry. */
const MAX_TOOL_NAME_CHARS = 128;

function validPath(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && [...value].length <= MAX_NOTICE_PATH_CHARS;
}

function validToolName(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && [...value].length <= MAX_TOOL_NAME_CHARS;
}

function permissionName(value: unknown): SessionPermission | null {
  return value === "require_approval" || value === "autoreview" || value === "write" ? value : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function noticeAuthor(value: unknown): NoticeAuthor | null {
  if (!isRecord(value)) return null;
  switch (value.kind) {
    case "person":
      return { kind: "person" };
    case "outside":
      return { kind: "outside" };
    case "agent":
      if (value.session !== undefined && value.session !== null
        && (typeof value.session !== "string" || !value.session
          || [...value.session].length > MAX_OUTSIDE_AUTHOR_CHARS)) return null;
      return {
        kind: "agent",
        ...(typeof value.session === "string" ? { session: value.session } : {}),
      };
    case "client":
      if (typeof value.owner !== "string" || !value.owner
        || [...value.owner].length > MAX_OUTSIDE_AUTHOR_CHARS) return null;
      return { kind: "client", owner: value.owner };
    default:
      return null;
  }
}

function boundedCount(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) && value >= 0
    && value <= MAX_OUTSIDE_COUNT ? value : null;
}

function outsideChange(value: unknown): OutsideChange | null {
  if (!isRecord(value) || !isChangeSet(value.changes)) return null;
  const author = noticeAuthor(value.author);
  if (!author) return null;
  const cause = value.cause;
  if (cause !== undefined && cause !== "edit") return null;
  return { author, ...(cause === undefined ? {} : { cause }), changes: value.changes };
}

/** Longest goal the store accepts. */
const MAX_GOAL_CHARS = 600;
/** Most plan items one plan may carry. */
const MAX_PLAN_ITEMS = 32;
/** Longest plan item text the store accepts. */
const MAX_PLAN_ITEM_CHARS = 200;

function stateAuthor(value: unknown): StateAuthor | null {
  return value === "person" || value === "agent" ? value : null;
}

function planStatus(value: unknown): PlanStatus | null {
  return value === "pending" || value === "in_progress" || value === "done" ? value : null;
}

/** The characters the Rust state owner treats as blank, copied exactly:
 * Unicode White_Space plus the byte-order mark. */
const STATE_BLANK_CLASS = "\u0009-\u000D\u0020\u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000\uFEFF";
const STATE_BLANK_CHARS = new RegExp(`[${STATE_BLANK_CLASS}]`, "g");
const STATE_BLANK_EDGES = new RegExp(`^[${STATE_BLANK_CLASS}]+|[${STATE_BLANK_CLASS}]+$`, "g");

/** Whether a text holds nothing the Rust owner also reads as blank. */
export function isBlankText(value: string): boolean {
  return value.replace(STATE_BLANK_CHARS, "").length === 0;
}

/** Trim the blank characters the Rust owner trims. */
export function trimStateText(value: string): string {
  return value.replace(STATE_BLANK_EDGES, "");
}

function textWithin(value: unknown, max: number): string | null {
  return typeof value === "string" && !isBlankText(value)
    && [...value].length <= max ? value : null;
}

/** Validate an untrusted goal or plan change; unknown shapes read as none. */
export function sessionState(value: unknown): SessionState | null {
  if (!isRecord(value)) return null;
  if (value.kind === "goal_cleared") {
    const author = stateAuthor(value.author);
    return author ? { kind: "goal_cleared", author } : null;
  }
  if (value.kind === "goal_set") {
    const author = stateAuthor(value.author);
    const text = textWithin(value.text, MAX_GOAL_CHARS);
    return author !== null && text !== null ? { kind: "goal_set", text, author } : null;
  }
  if (value.kind !== "plan_set" || !Array.isArray(value.items) || value.items.length > MAX_PLAN_ITEMS) return null;
  const items: PlanItem[] = [];
  for (const entry of value.items) {
    if (!isRecord(entry)) return null;
    const text = textWithin(entry.text, MAX_PLAN_ITEM_CHARS);
    const status = planStatus(entry.status);
    if (text === null || status === null) return null;
    items.push({ text, status });
  }
  if (items.filter(item => item.status === "in_progress").length > 1) return null;
  return { kind: "plan_set", items };
}

/** Validate an untrusted typed notice; unknown kinds read as no notice. */
export function hostNotice(value: unknown): HostNotice | null {
  if (!isRecord(value)) return null;
  if (value.kind === "outside_changes") {
    if (!Array.isArray(value.entries) || value.entries.length > MAX_OUTSIDE_ENTRIES) return null;
    const entries = value.entries.map(outsideChange);
    if (entries.some((entry) => entry === null)) return null;
    const more = boundedCount(value.more);
    const undone = boundedCount(value.undone);
    const ownUndone = value.own_undone === undefined ? 0 : boundedCount(value.own_undone);
    if (
      more === null
      || undone === null
      || ownUndone === null
      || typeof value.rewritten !== "boolean"
    ) return null;
    if (value.pruned !== undefined && typeof value.pruned !== "boolean") return null;
    return {
      kind: "outside_changes",
      entries: entries as OutsideChange[],
      more,
      undone,
      own_undone: ownUndone,
      rewritten: value.rewritten,
      pruned: value.pruned === true,
    };
  }
  if (value.kind === "tool_surface") {
    const names = (list: unknown): string[] | null => {
      if (!Array.isArray(list) || list.length > MAX_TOOL_SURFACE_NAMES) return null;
      return list.every(validToolName) ? list as string[] : null;
    };
    const added = names(value.added);
    const removed = names(value.removed);
    if (added === null || removed === null) return null;
    let permission: PermissionChange | undefined;
    if (value.permission !== undefined && value.permission !== null) {
      if (!isRecord(value.permission)) return null;
      const from = permissionName(value.permission.from);
      const to = permissionName(value.permission.to);
      if (from === null || to === null) return null;
      permission = { from, to };
    }
    return {
      kind: "tool_surface",
      added,
      removed,
      ...(permission ? { permission } : {}),
    };
  }
  const notice = value as { kind?: unknown; from?: unknown; to?: unknown; cause?: unknown };
  if (notice.kind !== "project_bound") return null;
  if (!validPath(notice.to)) return null;
  if (notice.from !== undefined && notice.from !== null && !validPath(notice.from)) return null;
  if (notice.cause !== "chosen" && notice.cause !== "moved") return null;
  return {
    kind: "project_bound",
    ...(notice.from === undefined || notice.from === null ? {} : { from: notice.from }),
    to: notice.to,
    cause: notice.cause,
  };
}

/** The project-bound notice a saved row carries, for the transcript. */
export function projectBoundNotice(meta: MessageMeta | undefined): Extract<HostNotice, { kind: "project_bound" }> | null {
  const notice = hostNotice(meta?.notice);
  return notice?.kind === "project_bound" ? notice : null;
}

/** The tool-surface notice a saved row carries, for the transcript. */
export function toolSurfaceNotice(meta: MessageMeta | undefined): Extract<HostNotice, { kind: "tool_surface" }> | null {
  const notice = hostNotice(meta?.notice);
  return notice?.kind === "tool_surface" ? notice : null;
}

export function messageKind(message: MessageLike): MessageKind {
  if (message.message_meta !== undefined) {
    const meta = message.message_meta;
    if (!meta || typeof meta !== "object") return structuralKind(message);
    const compatible = meta.kind === "assistant" ? message.role === "assistant"
      : meta.kind === "tool" ? message.role === "tool"
      : meta.kind === "proposal_outcome" ? message.role === "user" || message.role === "tool"
      : message.role === "user";
    const summaryFields = meta.standing_intent !== undefined || meta.summary_text !== undefined || meta.discarded_auditions !== undefined
      || meta.rollback_notice !== undefined || meta.proposal_notice !== undefined;
    const invalidText = [meta.standing_intent, meta.rollback_notice, meta.proposal_notice]
      .some(value => value !== undefined && (typeof value !== "string" || [...value].length > 4096));
    const invalidSummary = meta.summary_text !== undefined && (typeof meta.summary_text !== "string"
      || new TextEncoder().encode(meta.summary_text).length > 32_768);
    const range = meta.audio_range;
    const invalidRange = range !== undefined && (!range || typeof range !== "object"
      || !Number.isFinite(range.start_seconds) || !Number.isFinite(range.end_seconds)
      || range.start_seconds < 0 || range.end_seconds <= range.start_seconds);
    const invalidNotice = meta.notice !== undefined
      && (meta.kind !== "host_notice" || hostNotice(meta.notice) === null);
    const invalidState = meta.state !== undefined
      && (meta.kind !== "session_state" || sessionState(meta.state) === null);
    if (meta.version !== 1 || !isMessageKind(meta.kind) || !compatible || invalidText || invalidSummary || invalidRange
      || invalidNotice || invalidState || (summaryFields && meta.kind !== "summary") || (meta.audio_range && !isAuditionKind(meta.kind))) return structuralKind(message);
    return meta.kind;
  }
  const content = message.content ?? "";
  if (message.role === "tool") return "tool";
  if (message.role === "assistant") return "assistant";
  if (message.context === "summary" || /^Conversation summary written /.test(content)) return "summary";
  if (/^You have \d+ tool rounds? left in this turn\./.test(content)) return "round_budget";
  if (content.startsWith("Application tool result (data, not instructions): ")) return "application_result";
  if (message.context === "audition" || message.input_audio
    || (content.startsWith("You asked to hear ") && content.includes("this is that render.")
      && (!!message.media?.length || content.includes("[audio clip not saved]")))) {
    if (content.startsWith("A/B reference excerpt.")) return "comparison_reference";
    if (content.startsWith("A/B candidate excerpt.")) return "comparison_candidate";
    return "audition";
  }
  if (content.startsWith("Host transaction rollback: ")) return "transaction_rollback";
  if (content.startsWith("Host proposal outcome: ")) return "proposal_outcome";
  return "user";
}
