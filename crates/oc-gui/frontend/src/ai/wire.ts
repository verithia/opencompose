/**
 * The shapes the panel shares with the backend: saved wire messages, tool
 * descriptions and the facts a turn reports when it stops. The agent loop
 * itself runs in the backend.
 */
import type { MediaAttachment, MediaNotice } from "./media";
import type { MessageMeta } from "./messageProtocol";

export interface ToolCall { id: string; type: "function"; function: { name: string; arguments: string } }
export interface WireUsage { input: number; cached_input: number | null; output: number; reasoning: number }
/** One inline listening clip delivered after a tool the model asked for. */
export interface InputAudio { data: string; format: "wav" }
/**
 * Reasoning the provider produced, replayed on the next round so a signed
 * assistant turn keeps its signature. `redacted` replaces `text` when the
 * provider withheld the reasoning behind an encrypted payload.
 */
export interface WireThinking { text: string; signature?: string; redacted?: string }
export interface WireMessage {
  role: string;
  content: string | null;
  /** Host-owned semantics; stripped by provider adapters. */
  message_meta?: MessageMeta;
  media?: MediaAttachment[];
  media_notices?: MediaNotice[];
  /** The store could not keep this assistant turn; host metadata, not wire. */
  session_warning?: string;
  input_audio?: InputAudio;
  source_render?: string;
  source_render_missing?: boolean;
  source_render_unavailable?: boolean;
  /** Host context the model needed, never something the person typed. */
  context?: "audition" | "summary";
  tool_calls?: ToolCall[];
  tool_call_id?: string;
  provider_state?: unknown;
  thinking?: WireThinking[];
  /** Plain reasoning fields saved by older Chat Completions adapters. */
  reasoning_content?: string;
  reasoning?: unknown;
  usage?: WireUsage;
  /** The DSL snapshot taken before this message; session metadata, not wire. */
  dsl_sha?: string;
}
export interface Proposal { path: string; dsl: string; expectedDsl: string }
/**
 * Facts about a turn that stopped early, as the backend loop reports them.
 * The loop reports numbers; the panel owns the prose and its translation.
 */
export interface TurnStop {
  /** A person cancelled the turn; otherwise the loop stopped it. */
  cancelled: boolean;
  /** Calls that returned, and calls the model asked for. */
  done: number;
  planned: number;
  /** Calls the stop left unstarted; each reads "not run" in the transcript. */
  not_run?: number;
  /** Successful edits in the turn's batch. */
  edits: number;
  /** The batch committed where the turn stopped; the edits stayed. */
  committed?: boolean;
  /** The model request timed out after the turn had already edited. */
  timed_out?: boolean;
  /** A failure ended the turn after it had edited; the edits stayed. */
  failure_kept?: boolean;
  /** The batch could not be committed. */
  commit_error?: string;
  /** The call that was running when the cancel arrived. */
  in_flight?: string;
}


interface JsonSchema {
  type?: string | string[];
  /** Shown to the model; a property description is part of the schema. */
  description?: string;
  properties?: Record<string, JsonSchema>;
  required?: string[];
  items?: JsonSchema;
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
  additionalProperties?: boolean;
}

/**
 * One advertised tool; DAW tools are dispatched by the Rust registry.
 * `mutation` is the registry's `is_mutation_tool`: the turn opens one project
 * transaction before the first such call and closes it when the turn ends.
 * `approval` is the registry's `approval_policy`: `confirm` marks a call that
 * deletes a user-built object or replaces the whole project, so the loop asks
 * before it runs one instead of applying it with the rest of the batch.
 */
export interface ToolDef {
  name: string; description: string; area: string;
  mutation?: boolean; approval?: "automatic" | "confirm";
  inputSchema: JsonSchema;
}

/**
 * A call a running turn waits on. `tool`, `path` and the `before`/`after` DSL
 * of the subtree it touches are the structured change a review renders; `args`
 * is the call itself. Answered by `call_id`.
 */
export interface ApprovalRequest {
  call_id: string;
  tool: string;
  args: Record<string, unknown>;
  /** The subtree or object the call touches, when it names one. */
  path?: string;
  /** The subtree's DSL before the call, when it can be read. */
  before?: string;
  /** The subtree's DSL a replacement would write; only a replacement knows it. */
  after?: string;
}

