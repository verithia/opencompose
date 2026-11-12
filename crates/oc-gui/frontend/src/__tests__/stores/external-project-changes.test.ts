import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { invokeMock, listenMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { mountExternalProjectChanges, type ExternalProjectChange } from "../../stores/externalProjectChanges";

let stop: (() => void) | undefined;
const snapshot = (revision: number, name = "Current", path: string | null = "/music/current.ocp") => ({
  ...makeSnapshot({ name }), revision, file_path: path,
});
const emit = (event: ExternalProjectChange) => {
  const callback = listenMock.mock.calls.find(call => call[0] === "agent_project_changed")![1];
  callback({ payload: event });
};
const calls = (command: string) => invokeMock.mock.calls.filter(([name]) => name === command);

beforeEach(async () => {
  invokeMock.mockReset(); listenMock.mockReset();
  listenMock.mockResolvedValue(() => {});
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "get_project") return snapshot(10);
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "get_undo_state") return { can_undo: true, can_redo: false, undo_label: "External edit", undo_labels: [], redo_labels: [] };
    if (command === "get_undo_history") return [];
    return undefined;
  });
  await store.initProject();
  invokeMock.mockClear();
});
afterEach(() => { stop?.(); stop = undefined; });
async function mount(ready: Promise<unknown> = Promise.resolve()) {
  stop = mountExternalProjectChanges(ready);
  await Promise.resolve();
}

describe("external project synchronization", () => {
  it("adopts an edit and refreshes backend history without creating another undo entry", async () => {
    await mount();
    emit({ snapshot: snapshot(11, "Edited"), replaces_document: false });
    expect(store.project.name).toBe("Edited");
    expect(store.projectRevision).toBe(11);
    await vi.waitFor(() => expect(calls("get_undo_history")).toHaveLength(1));
    expect(calls("get_undo_state")).toHaveLength(1);
    expect(store.undoState().undoLabel).toBe("External edit");
    expect(invokeMock.mock.calls.map(([name]) => name)).toEqual(["get_undo_state", "get_undo_history"]);
  });

  it("ignores stale and duplicate snapshots from the same document", async () => {
    await mount();
    emit({ snapshot: snapshot(11, "Newest"), replaces_document: false });
    emit({ snapshot: snapshot(10, "Stale"), replaces_document: false });
    emit({ snapshot: snapshot(11, "Duplicate"), replaces_document: false });
    expect(store.project.name).toBe("Newest");
    expect(calls("get_undo_state")).toHaveLength(1);
  });

  it("refuses ordinary edits for a different document even at a newer revision", async () => {
    await mount();
    emit({ snapshot: snapshot(20, "Other", "/music/other.ocp"), replaces_document: false });
    expect(store.project.name).toBe("Current");
    expect(calls("get_undo_state")).toHaveLength(0);
  });

  it("advances document identity and drops picks for an explicit replacement", async () => {
    await mount();
    const generation = store.documentIdentity().generation;
    store.setBlockSelection([{ kind: "clip", trackIndex: 0, index: 0 }]);
    emit({ snapshot: snapshot(12, "Opened", "/music/other.ocp"), replaces_document: true });
    expect(store.project.name).toBe("Opened");
    expect(store.filePath()).toBe("/music/other.ocp");
    expect(store.documentIdentity().generation).toBe(generation + 1);
    expect(store.blockSelection()).toEqual([]);
  });

  it("leaves an active local document load in control", async () => {
    await mount();
    let finish!: (value: ReturnType<typeof snapshot>) => void;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_project") return new Promise(resolve => { finish = resolve; });
      return undefined;
    });
    const loading = store.initProject();
    emit({ snapshot: snapshot(30, "External", "/music/external.ocp"), replaces_document: true });
    expect(store.project.name).toBe("Current");
    finish(snapshot(15, "Local load"));
    await loading;
    expect(store.project.name).toBe("Local load");
  });

  it("buffers startup events and keeps replacement identity for a subsequent edit", async () => {
    let finish!: () => void;
    await mount(new Promise<void>(resolve => { finish = resolve; }));
    emit({ snapshot: snapshot(12, "Opened", "/music/other.ocp"), replaces_document: true });
    emit({ snapshot: snapshot(13, "Edited", "/music/other.ocp"), replaces_document: false });
    expect(store.project.name).toBe("Current");
    finish();
    await vi.waitFor(() => expect(store.project.name).toBe("Edited"));
    expect(store.filePath()).toBe("/music/other.ocp");
  });

  it("unsubscribes and ignores events after disposal", async () => {
    const release = vi.fn();
    listenMock.mockResolvedValueOnce(release);
    await mount();
    stop!();
    expect(release).toHaveBeenCalledOnce();
    emit({ snapshot: snapshot(12, "Disposed"), replaces_document: false });
    expect(store.project.name).toBe("Current");
  });

  it("releases a subscription that finishes registering after disposal", async () => {
    let register!: (release: () => void) => void;
    const release = vi.fn();
    listenMock.mockImplementationOnce(() => new Promise(resolve => { register = resolve; }));
    await mount();
    stop!(); register(release);
    await vi.waitFor(() => expect(release).toHaveBeenCalledOnce());
  });
});
