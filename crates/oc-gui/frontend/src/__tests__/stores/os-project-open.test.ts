import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { createRoot, createSignal } from "solid-js";
import { invokeMock, listenMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { mountOsProjectOpen } from "../../stores/osProjectOpen";
import { store } from "../../stores/project";

let pending: { id: number; path: string }[];
let dispose: (() => void) | undefined;
let stop: (() => void) | undefined;
let dirty = false, failOpen = false, failSave = false;
beforeEach(async () => {
  pending = []; dirty = false; failOpen = false; failSave = false;
  invokeMock.mockClear(); listenMock.mockClear();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "peek_project_open_request") return pending[0] ?? null;
    if (command === "acknowledge_project_open_request") {
      if ((args as {id:number}).id === pending[0]?.id) pending.shift();
      return true;
    }
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "save_file") { if (failSave) throw new Error("Save failed"); return { path: "/music/current.ocp", snapshot: null }; }
    if (command === "open_file") {
      if (failOpen) throw new Error("Open failed");
      return {...makeSnapshot({ name: "Opened" }), file_path: (args as { path: string }).path};
    }
    if (command === "get_project") return {...makeSnapshot(), file_path: "/music/current.ocp", dirty};
    return undefined;
  });
  await store.initProject();
});
afterEach(() => { stop?.(); dispose?.(); stop = undefined; dispose = undefined; store.answerUnsavedPrompt("cancel"); });
function mount(ready: Promise<unknown> = Promise.resolve(), blocked?: () => boolean) {
  createRoot(end => { dispose = end; stop = mountOsProjectOpen(ready, blocked); });
}
function wake() {
  const callback = listenMock.mock.calls.find(call => call[0] === "project_open_requested")?.[1] as (() => void) | undefined;
  callback?.();
}
const opened = () => invokeMock.mock.calls.filter(([command]) => command === "open_file");
const acked = () => invokeMock.mock.calls.filter(([command]) => command === "acknowledge_project_open_request");

describe("OS project delivery", () => {
  it("keeps cold requests until startup initialization completes", async () => {
    let finish!: () => void;
    pending.push({id:1,path:"/music/cold.ocp"});
    mount(new Promise<void>(resolve => { finish = resolve; }));
    await Promise.resolve(); wake(); expect(opened()).toHaveLength(0); finish();
    await vi.waitFor(() => expect(opened()).toHaveLength(1));
    expect(opened()[0][1]).toEqual({path:"/music/cold.ocp"});
    await vi.waitFor(() => expect(pending).toHaveLength(0));
  });
  it("handles warm notifications once and ignores current-document delivery", async () => {
    mount(); await vi.waitFor(() => expect(listenMock).toHaveBeenCalled());
    pending.push({id:1,path:"/music/warm.ocp"}); wake(); wake();
    await vi.waitFor(() => expect(acked()).toHaveLength(1));
    pending.push({id:2,path:"/music/warm.ocp"}); wake();
    await vi.waitFor(() => expect(acked()).toHaveLength(2)); expect(opened()).toHaveLength(1);
  });
  it("acknowledges Cancel without replacing the dirty document", async () => {
    dirty = true; await store.initProject(); pending.push({id:1,path:"/music/other.ocp"}); mount();
    await vi.waitFor(() => expect(store.unsavedPrompt()).toBeTruthy()); store.answerUnsavedPrompt("cancel");
    await vi.waitFor(() => expect(acked()).toHaveLength(1)); expect(opened()).toHaveLength(0);
    expect(store.filePath()).toBe("/music/current.ocp"); expect(store.dirty()).toBe(true);
  });
  it("replaces the document only after an explicit Don't Save answer", async () => {
    dirty = true; await store.initProject(); pending.push({id:1,path:"/music/other.ocp"}); mount();
    await vi.waitFor(() => expect(store.unsavedPrompt()).toBeTruthy());
    expect(opened()).toHaveLength(0);
    store.answerUnsavedPrompt("discard");
    await vi.waitFor(() => expect(opened()).toHaveLength(1));
    expect(store.filePath()).toBe("/music/other.ocp");
  });
  it("waits through a failed Save and opens only after a successful retry", async () => {
    dirty = true; failSave = true; await store.initProject(); pending.push({id:1,path:"/music/other.ocp"}); mount();
    await vi.waitFor(() => expect(store.unsavedPrompt()).toBeTruthy()); await store.saveForUnsavedPrompt();
    expect(store.error()).toContain("Save failed"); expect(opened()).toHaveLength(0); expect(acked()).toHaveLength(0);
    failSave = false; await store.saveForUnsavedPrompt(); await vi.waitFor(() => expect(opened()).toHaveLength(1));
  });
  it("preserves the current document after failed Open and consumes that request", async () => {
    failOpen = true; pending.push({id:1,path:"/music/missing.ocp"}); mount();
    await vi.waitFor(() => expect(acked()).toHaveLength(1)); expect(store.filePath()).toBe("/music/current.ocp");
    expect(store.error()).toContain("Open failed");
  });
  it("defers while another modal owns the document", async () => {
    const [blocked, setBlocked] = createSignal(true); pending.push({id:1,path:"/music/later.ocp"});
    mount(Promise.resolve(), blocked); await Promise.resolve(); expect(opened()).toHaveLength(0);
    setBlocked(false); await vi.waitFor(() => expect(opened()).toHaveLength(1));
  });
  it("rejects malformed bridge data and disposes listeners", async () => {
    const unlisten = vi.fn(); listenMock.mockResolvedValueOnce(unlisten);
    pending.push({id:1,path:"https://example.test/not-local.ocp"}); mount();
    await vi.waitFor(() => expect(listenMock).toHaveBeenCalled()); stop?.();
    expect(unlisten).toHaveBeenCalledOnce(); expect(opened()).toHaveLength(0);
  });
});
