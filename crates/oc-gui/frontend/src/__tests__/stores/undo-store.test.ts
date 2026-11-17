import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock, makeSnapshot } from "../setup";
import { HISTORY_NOTICE_MS, createUndoStore } from "../../stores/undoStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const state = () => ({ can_undo: true, can_redo: false });
const entries = (track: string) => [{
  index: 1,
  changes: { changes: [{ kind: "track_changed" as const, track }], more: 0 },
  cause: "edit" as const,
  author: { kind: "person" as const },
  scopes: [],
  timestamp: 0,
  applied: true,
}];
const history = (track: string) => ({ entries: entries(track), total: 1, applied: 1, journaled: true });
const calls = (command: string) => invokeMock.mock.calls.filter(([name]) => name === command);

/** A store with the History view open, its first read already answered. */
async function watched(...args: Parameters<typeof createUndoStore>) {
  const store = createUndoStore(...args);
  store.watchHistory();
  await vi.waitFor(() => expect(calls("history_page").length).toBeGreaterThan(0));
  await Promise.resolve();
  invokeMock.mockClear();
  return store;
}

beforeEach(() => { invokeMock.mockReset(); });
afterEach(() => { vi.useRealTimers(); });

describe("undo reply ownership", () => {
  it("reads no history page while no History view is open", async () => {
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? state() : history("A")));
    const store = createUndoStore(vi.fn(), vi.fn());
    await store.refreshUndoState();
    expect(store.undoState().canUndo).toBe(true);
    expect(calls("history_page")).toHaveLength(0);
    const close = store.watchHistory();
    await vi.waitFor(() => expect(store.historyView().entries).toEqual(entries("A")));
    close();
    invokeMock.mockClear();
    await store.refreshUndoState();
    expect(calls("history_page")).toHaveLength(0);
  });

  it("rejects a prior document's state without starting its history read", async () => {
    let context = "A:1";
    invokeMock.mockImplementation(() => Promise.resolve(history("first")));
    const store = await watched(vi.fn(), vi.fn(), () => context);
    const old = deferred<ReturnType<typeof state>>();
    invokeMock.mockImplementation(command => command === "get_undo_state"
      ? old.promise : Promise.resolve(history("unexpected")));
    const request = store.refreshUndoState();
    context = "B:2";
    old.resolve(state());
    await request;
    expect(store.undoState().canUndo).toBe(false);
    expect(calls("history_page")).toHaveLength(0);
  });

  it("rejects a late history reply after the document or revision changes", async () => {
    let context = "A:1";
    invokeMock.mockImplementation(() => Promise.resolve(history("first")));
    const store = await watched(vi.fn(), vi.fn(), () => context);
    const old = deferred<ReturnType<typeof history>>();
    invokeMock.mockImplementation(command => command === "get_undo_state"
      ? Promise.resolve(state()) : old.promise);
    const pending = store.refreshUndoState();
    await vi.waitFor(() => expect(calls("history_page")).toHaveLength(1));
    context = "B:2";
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? state() : history("B edit")));
    await store.refreshUndoState();
    old.resolve(history("A edit"));
    await pending;
    expect(store.undoState().canUndo).toBe(true);
    expect(store.historyView().entries).toEqual(entries("B edit"));
  });

  it("keeps the newest refresh when replies arrive in reverse order in one context", async () => {
    invokeMock.mockImplementation(() => Promise.resolve(history("first")));
    const store = await watched(vi.fn(), vi.fn());
    const first = deferred<ReturnType<typeof state>>();
    const second = deferred<ReturnType<typeof state>>();
    invokeMock.mockResolvedValue(history("New"));
    invokeMock.mockImplementationOnce(() => first.promise).mockImplementationOnce(() => second.promise);
    const old = store.refreshUndoState();
    const latest = store.refreshUndoState();
    second.resolve(state());
    await latest;
    first.resolve(state());
    await old;
    expect(store.undoState().canUndo).toBe(true);
    expect(store.historyView().entries).toEqual(entries("New"));
    expect(calls("history_page")).toHaveLength(1);
  });

  it("keeps the latest direct history request", async () => {
    invokeMock.mockImplementation(() => Promise.resolve(history("first")));
    const store = await watched(vi.fn(), vi.fn());
    const old = deferred<ReturnType<typeof history>>();
    invokeMock.mockImplementationOnce(() => old.promise).mockResolvedValue(history("New"));
    const pending = store.refreshHistory();
    await store.refreshHistory();
    old.resolve(history("Old"));
    await pending;
    expect(store.historyView().entries).toEqual(entries("New"));
  });

  it("clears old labels immediately and invalidates in-flight reads on document reset", async () => {
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? state() : history("Old")));
    const store = await watched(vi.fn(), vi.fn());
    await store.refreshUndoState();
    const pendingHistory = deferred<ReturnType<typeof history>>();
    invokeMock.mockReturnValueOnce(pendingHistory.promise);
    const pending = store.refreshHistory();
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? {
      can_undo: false, can_redo: false,
    } : { entries: [], total: 0, applied: 0, journaled: true }));
    store.clearUndoHistory();
    expect(store.undoState().canUndo).toBe(false);
    expect(store.historyView().entries).toEqual([]);
    pendingHistory.resolve(history("Old"));
    await pending;
    await vi.waitFor(() => expect(store.undoState().canUndo).toBe(false));
    expect(store.historyView().entries).toEqual([]);
  });

  it.each(["undo", "redo", "restoreUndoLevel"] as const)("ignores an obsolete %s error from another document", async action => {
    let context = 1;
    const pending = deferred<unknown>();
    invokeMock.mockReturnValue(pending.promise);
    const error = vi.fn();
    const apply = vi.fn();
    const store = createUndoStore(apply, error, () => context);
    const request = action === "restoreUndoLevel" ? store.restoreUndoLevel(0) : store[action]();
    const outcome = request.then(() => "ignored", failure => String(failure));
    context = 2;
    pending.reject(new Error("Old document failed"));
    expect(await outcome).toBe("ignored");
    expect(error).not.toHaveBeenCalled();
    expect(apply).not.toHaveBeenCalled();
  });

  it("does not apply a completed undo snapshot after a document replacement", async () => {
    let context = 1;
    const pending = deferred<unknown>();
    invokeMock.mockReturnValue(pending.promise);
    const apply = vi.fn();
    const store = createUndoStore(apply, vi.fn(), () => context);
    const request = store.undo();
    context = 2;
    pending.resolve({ status: "applied", snapshot: makeSnapshot() });
    await request;
    expect(apply).not.toHaveBeenCalled();
    expect(calls("get_undo_state")).toHaveLength(0);
  });
});

describe("the footer's history line", () => {
  const brief = {
    changes: { changes: [{ kind: "tempo" as const, from: 120, to: 132 }], more: 0 },
    cause: "edit" as const,
    author: { kind: "agent" as const, session: "s1" },
  };

  it("names what an undo and a redo did, then clears", async () => {
    vi.useFakeTimers();
    invokeMock.mockImplementation(command => Promise.resolve(
      command === "history_undo" || command === "history_redo"
        ? { status: "applied", snapshot: makeSnapshot(), brief }
        : state(),
    ));
    const apply = vi.fn();
    const store = createUndoStore(apply, vi.fn());
    await store.undo();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(store.historyNotice()).toEqual({ kind: "undid", brief });
    await store.redo();
    expect(store.historyNotice()).toEqual({ kind: "redid", brief });
    vi.advanceTimersByTime(HISTORY_NOTICE_MS);
    expect(store.historyNotice()).toBeNull();
  });

  it("says the agent is editing when the backend refuses, and changes nothing", async () => {
    invokeMock.mockResolvedValue({ status: "refused", reason: "agent_editing" });
    const apply = vi.fn();
    const error = vi.fn();
    const store = createUndoStore(apply, error);
    await store.undo();
    expect(store.historyNotice()).toEqual({ kind: "agent_editing" });
    expect(apply).not.toHaveBeenCalled();
    expect(error).not.toHaveBeenCalled();
  });

  it("stays quiet when there is nothing to undo", async () => {
    invokeMock.mockResolvedValue({ status: "refused", reason: "nothing_to_undo" });
    const error = vi.fn();
    const store = createUndoStore(vi.fn(), error);
    await store.undo();
    expect(store.historyNotice()).toBeNull();
    expect(error).not.toHaveBeenCalled();
  });
});
