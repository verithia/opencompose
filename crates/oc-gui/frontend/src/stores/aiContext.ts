/**
 * Context-usage estimate for models whose tokenizer we cannot run.
 *
 * A closed model exposes no tokenizer, so the number is always an estimate.
 * Two things make it useful anyway:
 *
 * 1. **Anchor**: the provider reports the real prompt size on every response
 *    (`usage.input`, cached tokens included). That figure is exact for the
 *    request it answers, so the next estimate starts from it plus the text that
 *    was added since, instead of re-guessing the whole conversation.
 * 2. **Calibration**: the ratio between reported and estimated prompt tokens is
 *    tracked as an exponential moving average, so the heuristic learns the
 *    model's true characters-per-token (they range from about 2.5 to 5) and a
 *    model that tokenizes CJK heavily stops being undercounted.
 *
 * The heuristic itself is the documented floor: four ASCII characters or one
 * wide character per token, plus per-message overhead. It also counts the
 * system prompt and the tool schemas, which are part of every request but were
 * invisible when only message text was counted.
 */
import { createSignal } from "solid-js";
import { estimateTokens } from "./aiLimits";

/** How much weight a new observation gets in the calibration average. */
const ALPHA = 0.3;
/** A sane band: outside it the sample is noise, not a tokenizer difference. */
const MIN_RATIO = 0.6;
const MAX_RATIO = 1.8;

const [ratio, setRatio] = createSignal(1);
const [samples, setSamples] = createSignal(0);
/** Prompt tokens the provider reported for the last completed request. */
const [reported, setReported] = createSignal(0);
/** Text that is not a message: system prompt and tool schemas, in characters. */
const [overheadChars, setOverheadChars] = createSignal(0);
/** The same overhead split for the context card. */
const [systemChars, setSystemChars] = createSignal(0);
const [toolChars, setToolChars] = createSignal(0);
/** The fraction of the model window that offers manual compaction. */
export const DEFAULT_COMPACT_AT = 0.75;
const COMPACT_AT_KEY = "opencompose.ai.compactAt";
function storedCompactAt(): number {
  try {
    const stored = Number(globalThis.localStorage?.getItem(COMPACT_AT_KEY));
    return Number.isFinite(stored) && stored >= 0.5 && stored <= 0.95
      ? stored
      : DEFAULT_COMPACT_AT;
  } catch {
    return DEFAULT_COMPACT_AT;
  }
}
const [compactAt, setCompactAtSignal] = createSignal(storedCompactAt());
/** Set and persist the manual-compaction threshold. */
export function setCompactAt(fraction: number): void {
  if (!Number.isFinite(fraction)) return;
  const clamped = Math.min(0.95, Math.max(0.5, fraction));
  setCompactAtSignal(clamped);
  try {
    globalThis.localStorage?.setItem(COMPACT_AT_KEY, String(clamped));
  } catch {
    // A private window can refuse storage; the current value still applies.
  }
}

/** Fold one provider report into the calibration. */
export function observeUsage(reportedPrompt: number, estimatedPrompt: number): void {
  if (!(reportedPrompt > 0) || !(estimatedPrompt > 0)) return;
  setReported(reportedPrompt);
  const sample = reportedPrompt / estimatedPrompt;
  if (sample < MIN_RATIO || sample > MAX_RATIO) return;
  setRatio(current => (samples() === 0 ? sample : current * (1 - ALPHA) + sample * ALPHA));
  setSamples(count => count + 1);
}

/** Declare the per-request text that is not part of the transcript. */
export function setRequestOverhead(systemPrompt: string, tools: unknown): void {
  const toolsChars = JSON.stringify(tools)?.length ?? 0;
  setSystemChars(systemPrompt.length);
  setToolChars(toolsChars);
  setOverheadChars(systemPrompt.length + toolsChars);
}

/** The calibrated estimate for a transcript plus its next message. */
export function estimateContext(messages: string, draft: string, projectText = ""): number {
  // Overhead is JSON (ASCII), so it is counted at the four-characters-per-token floor.
  const text = estimateTokens(messages + "\n" + draft + "\n" + projectText);
  return Math.round((text + overheadChars() / 4) * ratio());
}

/**
 * The token floor for one block of text. A panel that types into a long
 * conversation counts the transcript once and this, per keystroke.
 */
export function textTokens(text: string): number {
  return estimateTokens(text);
}

/** The calibrated estimate when the base text's token count is already known. */
export function estimateFromBase(baseTokens: number, draft: string): number {
  return Math.round((baseTokens + estimateTokens(draft) + overheadChars() / 4) * ratio());
}

/** What the model is using now: the reported figure, or the estimate if larger. */
export function contextUsed(estimate: number): number {
  return Math.max(reported(), estimate);
}

/**
 * A conversation or model change: the ratio and the reported prompt describe
 * the text that was sent before, so they are dropped. The overhead stays until
 * the next turn declares it again.
 */
export function resetContextCalibration(): void {
  setRatio(1);
  setSamples(0);
  setReported(0);
}

/** Test and session hook: forget the calibration and the anchor. */
export function resetContextMeter(): void {
  resetContextCalibration();
  setOverheadChars(0);
  setSystemChars(0);
  setToolChars(0);
  setCompactAtSignal(DEFAULT_COMPACT_AT);
}

export const contextMeter = {
  ratio,
  samples,
  reported,
  overheadChars,
  systemChars,
  toolChars,
  compactAt,
};
