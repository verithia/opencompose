/**
 * Context-usage estimate for models with no tokenizer: a heuristic floor, a
 * provider anchor, and a calibration ratio learned from reported usage.
 */
import { beforeEach, describe, expect, it } from "vitest";
import {
  contextUsed,
  contextMeter,
  estimateContext,
  estimateFromBase,
  observeUsage,
  resetContextCalibration,
  resetContextMeter,
  setRequestOverhead,
  textTokens,
} from "../stores/aiContext";

beforeEach(() => resetContextMeter());

describe("the context meter", () => {
  it("counts non-message overhead such as the system prompt and tool schemas", () => {
    const bare = estimateContext("hello", "");
    setRequestOverhead("s".repeat(4_000), []);
    const withPrompt = estimateContext("hello", "");
    expect(withPrompt - bare).toBeGreaterThanOrEqual(999);
    expect(withPrompt - bare).toBeLessThanOrEqual(1_001);
  });

  it("learns the model's real characters per token from reported usage", () => {
    const estimate = estimateContext("x".repeat(4_000), "");
    // The provider says the same text cost 1.5x what we guessed: the model
    // tokenizes more densely than four characters per token.
    observeUsage(Math.round(estimate * 1.5), estimate);
    expect(contextMeter.ratio()).toBeCloseTo(1.5, 2);
    expect(estimateContext("x".repeat(4_000), "")).toBe(Math.round(estimate * 1.5));
  });

  it("ignores a wild report instead of letting one sample move the average", () => {
    observeUsage(1_000, 1_000);
    observeUsage(1_000_000, 1_000);
    expect(contextMeter.ratio()).toBe(1);
  });

  it("never reports less than the provider's own figure", () => {
    observeUsage(4_200, 1_000);
    expect(contextUsed(10)).toBe(4_200);
    expect(contextUsed(9_000)).toBe(9_000);
  });

  it("counts a memoized transcript plus the draft, not the transcript again", () => {
    setRequestOverhead("s".repeat(400), []);
    const base = "x".repeat(40_000);
    // The split the composer uses: the base is counted once, the draft per key.
    expect(estimateFromBase(textTokens(base), "hello")).toBe(Math.round(estimateContext(base, "hello")));
  });

  it("drops the calibration and the anchor when the conversation changes", () => {
    setRequestOverhead("s".repeat(400), []);
    const estimate = estimateContext("x".repeat(4_000), "");
    observeUsage(Math.round(estimate * 1.4), estimate);
    expect(contextMeter.ratio()).toBeCloseTo(1.4, 2);
    expect(contextMeter.reported()).toBeGreaterThan(0);

    resetContextCalibration();
    expect(contextMeter.ratio()).toBe(1);
    expect(contextMeter.reported()).toBe(0);
    // The overhead is what the next request carries, so it survives the switch
    // (400 characters of prompt plus the empty schema list).
    expect(contextMeter.overheadChars()).toBe(402);
  });

  it("stays within the clamped band after several samples", () => {
    for (let i = 0; i < 20; i += 1) observeUsage(500, 1_000);
    expect(contextMeter.ratio()).toBeGreaterThanOrEqual(0.6);
    expect(contextMeter.ratio()).toBeLessThanOrEqual(1.8);
  });
});
