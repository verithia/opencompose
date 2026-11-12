/** Local transcript semantics. These tags are never provider API fields. */
export type MessageKind = "user" | "assistant" | "tool" | "application_result" | "round_budget"
  | "audition" | "comparison_reference" | "comparison_candidate" | "summary"
  | "transaction_rollback" | "proposal_outcome" | "host_notice";
export interface MessageMeta {
  version: 1;
  kind: MessageKind;
  standing_intent?: string;
  discarded_auditions?: number;
  rollback_notice?: string;
  proposal_notice?: string;
  audio_range?: { start_seconds: number; end_seconds: number; source: boolean };
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
};

/** Validate an untrusted saved row against the exhaustive kind table. */
function isMessageKind(value: unknown): value is MessageKind {
  return typeof value === "string" && Object.hasOwn(MESSAGE_KINDS, value);
}

export function messageKind(message: MessageLike): MessageKind {
  if (message.message_meta !== undefined) {
    const meta = message.message_meta;
    if (!meta || typeof meta !== "object") return structuralKind(message);
    const compatible = meta.kind === "assistant" ? message.role === "assistant"
      : meta.kind === "tool" ? message.role === "tool"
      : meta.kind === "proposal_outcome" ? message.role === "user" || message.role === "tool"
      : message.role === "user";
    const summaryFields = meta.standing_intent !== undefined || meta.discarded_auditions !== undefined
      || meta.rollback_notice !== undefined || meta.proposal_notice !== undefined;
    const invalidText = [meta.standing_intent, meta.rollback_notice, meta.proposal_notice]
      .some(value => value !== undefined && (typeof value !== "string" || [...value].length > 4096));
    const range = meta.audio_range;
    const invalidRange = range !== undefined && (!range || typeof range !== "object"
      || !Number.isFinite(range.start_seconds) || !Number.isFinite(range.end_seconds)
      || range.start_seconds < 0 || range.end_seconds <= range.start_seconds);
    if (meta.version !== 1 || !isMessageKind(meta.kind) || !compatible || invalidText || invalidRange
      || (summaryFields && meta.kind !== "summary") || (meta.audio_range && !isAuditionKind(meta.kind))) return structuralKind(message);
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
