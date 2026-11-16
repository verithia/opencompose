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

it("repainting the same conversation preserves an unsaved draft and its empty state", () => {
  vi.useFakeTimers();
  aiSession.setOpenScoped("workspace/project", "session");
  aiSession.setDraft("A sent prompt");
  vi.advanceTimersByTime(500);
  aiSession.setDraft("");
  aiSession.setOpenScoped("workspace/project", "session");
  expect(aiSession.draft()).toBe("");
  aiSession.setDraft("An unsent next instruction");
  aiSession.setOpenScoped("workspace/project", "session");
  expect(aiSession.draft()).toBe("An unsent next instruction");
});

describe("queued rows", () => {
  it("gives a stored row without an id one, and keeps it across reads", () => {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({
      v: 1, open: "s1", conversations: { s1: { queue: [{ text: "older row", media: [] }] } }, turn: null,
    }));
    aiSession.reload();
    const [row] = aiSession.queued();
    expect(row.text).toBe("older row");
    expect(typeof row.id).toBe("string");
    expect(row.id).not.toBe("");

    // The next write stores the id, so a later read names the same row.
    aiSession.setGoal("keep the id");
    aiSession.reload();
    expect(aiSession.queued().map((entry) => entry.id)).toEqual([row.id]);
  });

  it("acknowledges a steered row by id after the store was read again", () => {
    aiSession.setOpen("s1");
    aiSession.setQueued([{ text: "steer me", media: [] }, { text: "after it", media: [] }]);
    const [steered] = aiSession.queued();
    aiSession.beginSteer("s1", { id: steered.id });
    aiSession.reload();
    // The read gives new objects; the mark and the ids survive it.
    expect(aiSession.queued()[0]).not.toBe(steered);
    expect(aiSession.steering()).toEqual({ id: steered.id });

    aiSession.acknowledgeQueued("s1", steered.id);
    expect(aiSession.queued().map((entry) => entry.text)).toEqual(["after it"]);
    expect(aiSession.steering()).toBeNull();
    expect(aiSession.steerLive(steered.id)).toBe(false);
  });

  it("acknowledges a row of a conversation that is not open", () => {
    aiSession.setOpen("s1");
    aiSession.setQueued([{ text: "for s1", media: [] }]);
    const [row] = aiSession.queued();
    aiSession.beginSteer("s1", { id: row.id });
    aiSession.setOpen("s2");
    aiSession.acknowledgeQueued("s1", row.id);
    aiSession.setOpen("s1");
    expect(aiSession.queued()).toEqual([]);
    expect(aiSession.steering()).toBeNull();
  });
});
