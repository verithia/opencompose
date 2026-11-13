/**
 * A small stand-in for the backend agent loop, for panel tests. The panel
 * starts a turn with `ai_turn_start` and paints the `ai_turn` events the loop
 * sends; this fake sends the same events from a scripted provider and tool
 * handler, so a view test can stay at the view layer. The loop's own rules
 * (gating, batches, stop accounting) are tested in Rust.
 */
import { listenMock } from "../setup";
import type { ApprovalRequest, ToolCall, TurnStop, WireMessage } from "../../ai/wire";
import type { TurnDecision, TurnEvent, TurnOutcome } from "../../ai/api";

/** The arguments the panel sends to `ai_turn_start`. */
export interface TurnStartArgs {
  providerId: string;
  requestId: string;
  sessionId: string;
  messages: WireMessage[];
  toolsets?: string[];
  label?: string;
  allowed?: string[];
  onDelta?: { onmessage: (text: string) => void };
  onThinking?: { onmessage: (text: string) => void };
  onPreparing?: { onmessage: (name: string) => void };
}

export interface FakeTurnBackend {
  /** One provider round over the wire the fake holds, with the turn's channels. */
  chat: (round: TurnStartArgs) => Promise<unknown>;
  /** One tool call; a rejection becomes an `{error}` result. */
  tool?: (call: { tool: string; args: Record<string, unknown> }) => Promise<unknown>;
  /** Tools the fake asks about before running; the answer comes through `ai_turn_answer`. */
  ask?: (call: { tool: string; args: Record<string, unknown> }) => boolean;
  /** Tools whose successful result counts as an edit. */
  mutation?: (tool: string) => boolean;
}

interface Running {
  cancelled: boolean;
  answers: Map<string, (decision: TurnDecision) => void>;
  stop: () => void;
}

const running = new Map<string, Running>();

/** Every `ai_turn` and `ai_turn_project` handler the panel registered, newest last. */
function handlers(name: string): ((event: { payload: unknown }) => void)[] {
  return (listenMock.mock.calls as [string, (event: { payload: unknown }) => void][])
    .filter(([event]) => event === name)
    .map(([, handler]) => handler);
}

/** Sends one turn event to the panel, as the backend's `ai_turn` does. */
export function emitTurnEvent(requestId: string, sessionId: string, event: TurnEvent): void {
  for (const handler of handlers("ai_turn")) {
    handler({ payload: { request_id: requestId, session_id: sessionId, ...event } });
  }
}

/** Sends the document a turn changed, as the backend's `ai_turn_project` does. */
export function emitTurnProject(snapshot: unknown): void {
  for (const handler of handlers("ai_turn_project")) handler({ payload: snapshot });
}

/** `ai_cancel` for a fake turn: the round in flight stops. */
export function cancelFakeTurn(requestId: string): boolean {
  const turn = running.get(requestId);
  if (!turn) return false;
  turn.cancelled = true;
  turn.stop();
  return true;
}

/** `ai_turn_answer` for a fake turn. */
export function answerFakeTurn(requestId: string, callId: string, decision: TurnDecision): void {
  const answer = running.get(requestId)?.answers.get(callId);
  if (!answer) throw new Error("Nothing is waiting for an answer to this question");
  answer(decision);
}

/** Whether a fake turn is still running. */
export function fakeTurnRunning(requestId?: string): boolean {
  return requestId ? running.has(requestId) : running.size > 0;
}

/** Forgets every fake turn; call between tests. */
export function resetFakeTurns(): void {
  for (const turn of running.values()) turn.stop();
  running.clear();
}

function errorOf(error: unknown): NonNullable<TurnOutcome["error"]> {
  if (error && typeof error === "object") {
    const fields = error as { kind?: unknown; message?: unknown; retryable?: unknown; reauth?: unknown };
    return {
      kind: typeof fields.kind === "string" ? fields.kind : "provider",
      message: typeof fields.message === "string" ? fields.message : String(error),
      ...(typeof fields.retryable === "boolean" ? { retryable: fields.retryable } : {}),
      ...(typeof fields.reauth === "boolean" ? { reauth: fields.reauth } : {}),
    };
  }
  return { kind: "provider", message: String(error) };
}

const NOT_RUN = JSON.stringify({ error: "the turn stopped before this call ran; nothing in the project changed" });

/**
 * Runs one turn the way the backend loop does, as far as a view can tell:
 * each saved message is sent as a `message` event, each call as `running`,
 * a question as `ask`, and the outcome is the command's answer.
 */
export async function runFakeTurn(args: TurnStartArgs, backend: FakeTurnBackend): Promise<TurnOutcome> {
  const { requestId, sessionId } = args;
  let stopRound: () => void = () => {};
  const turn: Running = { cancelled: false, answers: new Map(), stop: () => stopRound() };
  running.set(requestId, turn);
  const wire: WireMessage[] = [...args.messages];
  let done = 0;
  let planned = 0;
  let edits = 0;
  const emit = (event: TurnEvent) => emitTurnEvent(requestId, sessionId, event);
  const save = (message: WireMessage) => {
    wire.push(message);
    emit({ type: "message", message });
  };
  const stop = (extra: Partial<TurnStop> = {}): TurnStop => ({
    cancelled: turn.cancelled, done, planned, edits, committed: edits > 0, ...extra,
  });
  const cancelled = (): TurnOutcome => ({ text: "cancelled", failed: true, stop: stop({ cancelled: true }) });
  try {
    for (let round = 0; round < 64; round++) {
      if (turn.cancelled) return cancelled();
      let reply: WireMessage;
      try {
        reply = await new Promise<WireMessage>((resolve, reject) => {
          stopRound = () => reject(new Error("Request cancelled"));
          backend.chat({ ...args, messages: [...wire] })
            .then(value => resolve(value as WireMessage), reject);
        });
      } catch (error) {
        if (turn.cancelled) return cancelled();
        const failure = errorOf(error);
        return edits > 0
          ? { text: failure.message, failed: true, error: failure, stop: stop({ failure_kept: true }) }
          : { text: failure.message, failed: true, error: failure };
      }
      if (turn.cancelled) {
        save({ role: "assistant", content: reply.content ?? null, ...(reply.tool_calls ? { tool_calls: reply.tool_calls } : {}) });
        return cancelled();
      }
      const calls: ToolCall[] = reply.tool_calls ?? [];
      const { media_notices: notices, ...saved } = reply;
      if (notices?.length) emit({ type: "media", notices });
      save(saved);
      if (!calls.length) {
        const text = typeof reply.content === "string" ? reply.content : "";
        return text.trim()
          ? { text, failed: false }
          : { text: "Provider returned no text.", failed: true };
      }
      planned += calls.length;
      for (const [index, call] of calls.entries()) {
        if (turn.cancelled) {
          for (const left of calls.slice(index)) save({ role: "tool", tool_call_id: left.id, content: NOT_RUN });
          return { ...cancelled(), stop: stop({ cancelled: true, not_run: calls.length - index }) };
        }
        let callArgs: Record<string, unknown> = {};
        try { callArgs = JSON.parse(call.function.arguments || "{}"); } catch { /* the tool reads {} */ }
        const tool = call.function.name;
        let content: string;
        if (backend.ask?.({ tool, args: callArgs }) && !(args.allowed ?? []).includes(tool)) {
          const request: ApprovalRequest = { call_id: call.id, tool, args: callArgs };
          const decision = await new Promise<TurnDecision | null>((resolve) => {
            turn.answers.set(call.id, resolve);
            stopRound = () => resolve(null);
            emit({ type: "ask", request });
          });
          turn.answers.delete(call.id);
          if (turn.cancelled || !decision) {
            for (const left of calls.slice(index)) save({ role: "tool", tool_call_id: left.id, content: NOT_RUN });
            return { ...cancelled(), stop: stop({ cancelled: true, not_run: calls.length - index }) };
          }
          if (!decision.approved) {
            save({
              role: "tool", tool_call_id: call.id,
              content: JSON.stringify({ declined: true, tool, error: `the user declined ${tool}` }),
            });
            done += 1;
            continue;
          }
        }
        emit({ type: "running", call_id: call.id, tool });
        try {
          const result = backend.tool ? await backend.tool({ tool, args: callArgs }) : {};
          content = JSON.stringify(result);
          if (backend.mutation?.(tool)) edits += 1;
        } catch (error) {
          content = JSON.stringify({ error: errorOf(error).message });
        }
        done += 1;
        save({ role: "tool", tool_call_id: call.id, content });
      }
    }
    return { text: "stopped after 64 tool rounds", failed: true, stop: stop() };
  } finally {
    running.delete(requestId);
  }
}
