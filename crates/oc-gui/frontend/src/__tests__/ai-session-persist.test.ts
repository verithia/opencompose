/**
 * The panel store writes its draft on a short delay instead of serialising the
 * whole conversation for every keystroke, and still guarantees a reloaded
 * window sees the last one.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { aiSession } from "../stores/aiSession";

const STORAGE_KEY = "opencompose.ai.panel.v1";

// jsdom does not expose Web Storage in this environment; the store only needs a map.
const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      get length() {
        return storage.size;
      },
      key: (index: number) => Array.from(storage.keys())[index] ?? null,
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => { storage.set(key, String(value)); },
      removeItem: (key: string) => { storage.delete(key); },
      clear: () => { storage.clear(); },
    },
  });
}

interface Stored {
  conversations?: Record<string, { draft?: string; queue?: { text: string }[] }>;
}

function stored(): Stored {
  return JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}") as Stored;
}

function typedDraft(): string | undefined {
  return stored().conversations?.[""]?.draft;
}

beforeEach(() => {
  vi.useRealTimers();
  aiSession.reset();
});

afterEach(() => {
  vi.useRealTimers();
  aiSession.reset();
});

describe("the draft write", () => {
  it("holds the value in memory at once and writes it when typing pauses", () => {
    vi.useFakeTimers();
    aiSession.setDraft("half typed");

    expect(aiSession.draft()).toBe("half typed");
    expect(typedDraft()).toBeUndefined();
    vi.advanceTimersByTime(499);
    expect(typedDraft()).toBeUndefined();
    vi.advanceTimersByTime(1);
    expect(typedDraft()).toBe("half typed");
  });

  it("writes a pending draft when the page goes away", () => {
    vi.useFakeTimers();
    aiSession.setDraft("kept across a reload");

    window.dispatchEvent(new Event("pagehide"));

    expect(typedDraft()).toBe("kept across a reload");
  });

  it("writes a pending draft before the store is read again", () => {
    vi.useFakeTimers();
    aiSession.setDraft("read back");
    aiSession.reload();

    expect(typedDraft()).toBe("read back");
    expect(aiSession.draft()).toBe("read back");
  });

  it("carries the pending draft when another change writes first", () => {
    vi.useFakeTimers();
    aiSession.setDraft("next");
    // Queuing a message writes the store at once, so the draft rides along and
    // the pending timer has nothing left to add.
    aiSession.setQueued([{ text: "next", media: [] }]);
    expect(stored().conversations?.[""]?.queue?.map((row) => row.text)).toEqual(["next"]);
    expect(typedDraft()).toBe("next");

    // Clearing the composer is a draft change too: it lands on the same delay.
    aiSession.setDraft("");
    expect(typedDraft()).toBe("next");
    vi.advanceTimersByTime(500);
    expect(typedDraft()).toBeUndefined();
  });
});
