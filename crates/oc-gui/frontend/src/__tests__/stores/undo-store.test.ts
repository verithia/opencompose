import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock, makeSnapshot } from "../setup";
import { createUndoStore } from "../../stores/undoStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const state = (label: string) => ({
  can_undo: true, can_redo: false, undo_label: label, redo_label: null,
  undo_labels: [label], redo_labels: [],
});
const history = (label: string) => [{ level: 1, label, applied: true }];
const calls = (command: string) => invokeMock.mock.calls.filter(([name]) => name === command);

beforeEach(() => { invokeMock.mockReset(); });

describe("undo reply ownership", () => {
  it("rejects a prior document's state without starting its history read", async () => {
    let context = "A:1";
    const old = deferred<ReturnType<typeof state>>();
    invokeMock.mockImplementation(command => command === "get_undo_state"
      ? old.promise : Promise.resolve(history("unexpected")));
    const store = createUndoStore(vi.fn(), vi.fn(), () => context);
    const request = store.refreshUndoState();
    context = "B:2";
    old.resolve(state("A edit"));
    await request;
    expect(store.undoState().canUndo).toBe(false);
    expect(store.undoHistory()).toEqual([]);
    expect(calls("get_undo_history")).toHaveLength(0);
  });

  it("rejects a late history reply after the document or revision changes", async () => {
    let context = "A:1";
    const old = deferred<ReturnType<typeof history>>();
    invokeMock.mockImplementation(command => command === "get_undo_state"
      ? Promise.resolve(state(context)) : old.promise);
    const store = createUndoStore(vi.fn(), vi.fn(), () => context);
    const pending = store.refreshUndoState();
    await vi.waitFor(() => expect(calls("get_undo_history")).toHaveLength(1));
    context = "B:2";
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? state("B edit") : history("B edit")));
    await store.refreshUndoState();
    old.resolve(history("A edit"));
    await pending;
    expect(store.undoState().undoLabel).toBe("B edit");
    expect(store.undoHistory()).toEqual(history("B edit"));
  });

  it("keeps the newest refresh when replies arrive in reverse order in one context", async () => {
    const first = deferred<ReturnType<typeof state>>();
    const second = deferred<ReturnType<typeof state>>();
    invokeMock.mockResolvedValue(history("New"));
    invokeMock.mockImplementationOnce(() => first.promise).mockImplementationOnce(() => second.promise);
    const store = createUndoStore(vi.fn(), vi.fn());
    const old = store.refreshUndoState();
    const latest = store.refreshUndoState();
    second.resolve(state("New"));
    await latest;
    first.resolve(state("Old"));
    await old;
    expect(store.undoState().undoLabel).toBe("New");
    expect(store.undoHistory()).toEqual(history("New"));
    expect(calls("get_undo_history")).toHaveLength(1);
  });

  it("does not overwrite a newer explicit history refresh with an older state follow-up", async () => {
    const pendingState = deferred<ReturnType<typeof state>>();
    invokeMock.mockImplementation(command => command === "get_undo_state" ? pendingState.promise : Promise.resolve(history("Newest")));
    const store = createUndoStore(vi.fn(), vi.fn());
    const pending = store.refreshUndoState();
    await store.refreshUndoHistory();
    pendingState.resolve(state("State"));
    await pending;
    expect(store.undoState().undoLabel).toBe("State");
    expect(store.undoHistory()).toEqual(history("Newest"));
    expect(calls("get_undo_history")).toHaveLength(1);
  });

  it("keeps the latest direct history request", async () => {
    const old = deferred<ReturnType<typeof history>>();
    invokeMock.mockImplementationOnce(() => old.promise).mockResolvedValue(history("New"));
    const store = createUndoStore(vi.fn(), vi.fn());
    const pending = store.refreshUndoHistory();
    await store.refreshUndoHistory();
    old.resolve(history("Old"));
    await pending;
    expect(store.undoHistory()).toEqual(history("New"));
  });

  it("clears old labels immediately and invalidates in-flight reads on document reset", async () => {
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? state("Old") : history("Old")));
    const store = createUndoStore(vi.fn(), vi.fn());
    await store.refreshUndoState();
    const pendingHistory = deferred<ReturnType<typeof history>>();
    invokeMock.mockReturnValueOnce(pendingHistory.promise);
    const pending = store.refreshUndoHistory();
    invokeMock.mockImplementation(command => Promise.resolve(command === "get_undo_state" ? {
      can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
    } : []));
    store.clearUndoHistory();
    expect(store.undoState().canUndo).toBe(false);
    expect(store.undoHistory()).toEqual([]);
    pendingHistory.resolve(history("Old"));
    await pending;
    await vi.waitFor(() => expect(store.undoState().undoLabel).toBeNull());
    expect(store.undoHistory()).toEqual([]);
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
    const pending = deferred<ReturnType<typeof makeSnapshot>>();
    invokeMock.mockReturnValue(pending.promise);
    const apply = vi.fn();
    const store = createUndoStore(apply, vi.fn(), () => context);
    const request = store.undo();
    context = 2;
    pending.resolve(makeSnapshot());
    await request;
    expect(apply).not.toHaveBeenCalled();
    expect(calls("get_undo_state")).toHaveLength(0);
  });
});
