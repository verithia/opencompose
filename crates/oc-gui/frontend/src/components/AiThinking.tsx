import { Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { preparingActivity, thinkingPreview } from "../ai/activity";
import { AiMarkdown } from "./AiMarkdown";
import { i18n } from "../stores/i18n";

/**
 * The waiting row of a live turn: which phase the turn is in, how long that
 * phase has run, and a CSS indicator. The phase is derived from what the panel
 * already tracks, so the panel adds no state of its own.
 */

export type ThinkingPhase = "waiting" | "preparing" | "streaming" | "tool" | "approval" | "retry";

export interface ThinkingFacts {
  /** The turn is in flight; false once it settles and the row goes away. */
  loading: boolean;
  /** The text streamed so far; empty until the first token. */
  partial: string;
  /** A destructive call is paused on the user's answer. */
  approval: boolean;
  /** The tool the transcript last reported, or null before the first one. */
  lastTool: string | null;
  /** The identity string the header shows: `provider/model`. */
  identity: string;
  /** The request is being sent again after a transport failure. */
  retrying?: boolean;
  /** Text is arriving. The row names the phase; the transcript paints the
   * text itself, so the row must not repeat it. */
  streaming?: boolean;
  /** A caller-owned line for the phase, when it knows more than the phase does
   * (a stop that has not landed yet, for instance). */
  label?: string;
  /** Provider-exposed plain text only; omitted when none was supplied. */
  reasoning?: string;
  preparingTool?: string | null;
}

export interface ThinkingState {
  phase: ThinkingPhase;
  /** The translated line for this phase; empty while text streams. */
  label: string;
}

/** The tool the transcript last reported. A tool row is the only tool
 * knowledge the panel holds, so it names the tool an ongoing round uses. */
export function lastToolOf(messages: readonly { role: string; tool?: { name: string } }[]): string | null {
  const last = messages[messages.length - 1];
  return last?.role === "tool" ? last.tool?.name ?? null : null;
}

/** The phase of one turn, or null once the turn is over. */
export function thinkingPhase(facts: ThinkingFacts): ThinkingPhase | null {
  if (!facts.loading) return null;
  if (facts.approval) return "approval";
  if (facts.preparingTool) return "preparing";
  if (facts.streaming || facts.partial) return "streaming";
  if (facts.retrying) return "retry";
  if (facts.lastTool) return "tool";
  return "waiting";
}

/** Phase plus its translated line: the whole row's text in one place. */
export function thinkingState(facts: ThinkingFacts): ThinkingState | null {
  switch (thinkingPhase(facts)) {
    case null: return null;
    case "preparing": return { phase: "preparing", label: preparingActivity(facts.preparingTool ?? "") };
    case "waiting": return { phase: "waiting", label: i18n.t("ai.thinking.waiting", { model: facts.identity }) };
    case "tool": return { phase: "tool", label: i18n.t("ai.thinking.tool", { tool: facts.lastTool ?? "" }) };
    case "approval": return { phase: "approval", label: i18n.t("ai.thinking.approval") };
    case "retry": return { phase: "retry", label: i18n.t("ai.thinking.retry") };
    case "streaming": return {
      phase: "streaming",
      label: facts.streaming ? i18n.t("ai.thinking.streaming") : "",
    };
  }
}

export function AiThinking(props: ThinkingFacts) {
  const [elapsed, setElapsed] = createSignal(0);
  const state = () => thinkingState(props) ?? (props.reasoning?.trim()
    ? { phase: "waiting" as const, label: i18n.t("ai.activity.thinking") } : null);

  // The counter measures one phase, so a new phase starts it at zero.
  createEffect(on(() => thinkingPhase(props), () => setElapsed(0), { defer: true }));

  createEffect(() => {
    if (!props.loading) return;
    const timer = setInterval(() => setElapsed(value => value + 1), 1000);
    onCleanup(() => clearInterval(timer));
  });

  return (
    <Show when={state()}>
      {(current) => (
        <div
          class="ai-thinking"
          classList={{ streaming: current().phase === "streaming", reasoning: !!props.reasoning?.trim() }}
          role={props.loading ? "status" : undefined}
          aria-live={props.loading ? "polite" : "off"}
          aria-busy={props.loading}
          data-phase={current().phase}
          ref={(element) => {
            // One scroll when the row appears; a user who scrolls away keeps
            // their offset, because nothing scrolls this element again.
            if (props.loading) queueMicrotask(() => element.scrollIntoView?.({ block: "nearest" }));
          }}
        >
          <Show when={props.reasoning?.trim()} fallback={
          <Show
            when={current().phase !== "streaming" || props.streaming}
            fallback={
              <div class="ai-thinking-stream selectable" aria-live="off">
                {props.partial}
                <span class="ai-thinking-caret" aria-hidden="true" />
              </div>
            }
          >
            <span class="ai-thinking-mark" aria-hidden="true">
              <span class="ai-thinking-dot" />
              <span class="ai-thinking-dot" />
              <span class="ai-thinking-dot" />
            </span>
            <span class="ai-thinking-static" aria-hidden="true" />
            <span class="ai-thinking-label">{props.label ?? current().label}</span>
            <span class="ai-thinking-elapsed" aria-hidden="true">
              {i18n.t("ai.thinking.elapsed", { seconds: elapsed() })}
            </span>
          </Show>
          }>
            <details class="ai-thinking-details">
              <summary class="ai-thinking-summary">
                <Show when={props.loading}>
                  <span class="ai-thinking-mark" aria-hidden="true"><span class="ai-thinking-dot" /><span class="ai-thinking-dot" /><span class="ai-thinking-dot" /></span>
                  <span class="ai-thinking-static" aria-hidden="true" />
                </Show>
                <span class="ai-thinking-label">
                  {i18n.t("ai.activity.thinking")}
                  <span class="ai-thinking-preview" aria-live="off">{thinkingPreview(props.reasoning ?? "")}</span>
                </span>
                <span class="ai-thinking-elapsed" aria-hidden="true">{props.loading ? i18n.t("ai.thinking.elapsed", { seconds: elapsed() }) : ""}</span>
              </summary>
              <div class="ai-thinking-content selectable" aria-live="off">
                <AiMarkdown content={props.reasoning ?? ""} />
              </div>
            </details>
          </Show>
        </div>
      )}
    </Show>
  );
}
