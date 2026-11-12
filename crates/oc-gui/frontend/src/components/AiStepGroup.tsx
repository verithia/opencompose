import { For, createMemo, type JSX } from "solid-js";
import { i18n } from "../stores/i18n";
import type { ChatMessage } from "../ai/transcript";
import { LazyDetails } from "./LazyDetails";

/** `xx m xx s`, or seconds only under a minute. */
export function stepDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return minutes > 0 ? `${minutes}m ${String(seconds).padStart(2, "0")}s` : `${seconds}s`;
}

/**
 * The disclosure above a run of tool work. It stays open while the turn works
 * and folds when the next assistant message closes the step; a restored step
 * with no timestamps still shows its tool count.
 */
export function AiStepGroup(props: {
  rows: ChatMessage[];
  start: number | null;
  end: number | null;
  toolCalls: number;
  running: boolean;
  /** Latest tool or thinking line, shown while a folded step is still running. */
  latest?: string;
  now: () => number;
  render: (message: ChatMessage) => JSX.Element;
}) {
  const elapsed = createMemo(() => {
    if (props.start === null) return null;
    const end = props.running ? props.now() : props.end;
    if (end === null) return null;
    return Math.max(0, end - props.start);
  });
  const label = createMemo(() => {
    const one = props.toolCalls === 1;
    const duration = elapsed();
    if (props.running) {
      const elapsed = duration === null ? "0s" : stepDuration(duration);
      if (props.latest) {
        return i18n.t("ai.step.runningLatest", { duration: elapsed, latest: props.latest });
      }
      return i18n.t(one ? "ai.step.runningOne" : "ai.step.running", {
        duration: elapsed,
        count: props.toolCalls,
      });
    }
    if (duration === null) {
      return i18n.t(one ? "ai.step.completedNoTimeOne" : "ai.step.completedNoTime", {
        count: props.toolCalls,
      });
    }
    return i18n.t(one ? "ai.step.completedOne" : "ai.step.completed", {
      duration: stepDuration(duration),
      count: props.toolCalls,
    });
  });
  return (
    <LazyDetails
      class="ai-step-group"
      open={props.running}
      summary={<span class="ai-step-label">{label()}</span>}
    >
      <For each={props.rows}>{props.render}</For>
    </LazyDetails>
  );
}
