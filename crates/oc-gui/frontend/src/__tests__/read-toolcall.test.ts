/**
 * Long tool results: the model sees a bounded preview that names the call id,
 * and pages through the rest with `read_toolcall`. The store is capped so one
 * session cannot grow without bound.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { clearStoredResults, readStoredResult, rememberToolResult, storedToolResult } from "../ai/agent";

beforeEach(() => {
  // Each case seeds its own id; the store keeps the last 64 entries.
});

describe("a stored tool result", () => {
  it("returns the requested window and the true total", () => {
    const text = "x".repeat(20_000) + "END";
    rememberToolResult("call_big", text);

    const page = readStoredResult("call_big", 20_000, 10);
    expect("text" in page && page.text).toBe("END");
    expect("total" in page && page.total).toBe(20_003);

    const tail = readStoredResult("call_big", 19_998, 5);
    expect("text" in tail && tail.text).toBe("xxEND");
  });

  it("clamps the offset to the stored length and refuses an unknown id", () => {
    rememberToolResult("call_short", "abc");
    const past = readStoredResult("call_short", 99, 10);
    expect("text" in past && past.text).toBe("");

    const missing = readStoredResult("call_missing", 0, 10);
    expect("error" in missing && missing.error).toContain("No stored result");
  });

  it("is forgotten when the conversation changes", () => {
    rememberToolResult("call_old", "x".repeat(100));
    expect(storedToolResult("call_old")).not.toBeNull();

    clearStoredResults();
    expect(storedToolResult("call_old")).toBeNull();
    const page = readStoredResult("call_old", 0, 10);
    expect("error" in page && page.error).toContain("No stored result");
  });

  it("caps the page size", () => {
    rememberToolResult("call_huge", "y".repeat(100_000));
    const page = readStoredResult("call_huge", 0, 500_000);
    expect("limit" in page && page.limit).toBe(32_000);
    expect("text" in page && page.text.length).toBe(32_000);
  });
});
