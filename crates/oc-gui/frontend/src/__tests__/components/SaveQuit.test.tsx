import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, listenMock, makeSnapshot, saveDialogMock, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import App from "../../App";

const windowMock = vi.hoisted(() => ({
  destroy: vi.fn(),
  onCloseRequested: vi.fn(),
  setTitle: vi.fn(),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => windowMock }));

/** The commands that decide what happens to the document, in the order sent. */
const order: string[] = [];

/** A dirty open project; `save_file` succeeds unless a case overrides it. */
function mockCommands(overrides: Record<string, () => unknown> = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (["prepare_quit", "stop_recording", "save_file", "discard_unsaved", "quit_now"].includes(cmd)) {
      order.push(cmd);
    }
    if (cmd === "scan_plugins") return [];
    const override = overrides[cmd];
    if (override) return override();
    if (cmd === "prepare_quit") {
      const project = overrides.get_project?.() as { dirty: boolean } | undefined;
      return { dirty: project?.dirty ?? true, recording: false };
    }
    if (cmd === "save_file") return { path: "/old/song.ocp", snapshot: null };
    if (cmd === "discard_unsaved") {
      return { ...makeSnapshot(), file_path: "/old/song.ocp", dirty: false };
    }
    return { ...makeSnapshot(), file_path: "/old/song.ocp", dirty: true };
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  order.length = 0;
  windowMock.onCloseRequested.mockResolvedValue(() => {});
  windowMock.destroy.mockImplementation(() => { order.push("destroy"); });
  mockCommands();
});

async function requestQuit() {
  const view = render(() => <App />);
  await waitFor(() => expect(windowMock.onCloseRequested).toHaveBeenCalled());
  await waitFor(() => expect(store.dirty()).toBe(true));
  const preventDefault = vi.fn();
  await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault });
  expect(preventDefault).toHaveBeenCalled();
  return view;
}

describe("Save and Quit", () => {
  it("prevents native destruction and exits the app when closing a clean document", async () => {
    mockCommands({
      take_startup_project: () => null,
      get_project: () => ({ ...makeSnapshot(), file_path: "/old/song.ocp", dirty: false }),
    });
    render(() => <App />);
    await waitFor(() => expect(windowMock.onCloseRequested).toHaveBeenCalled());
    await waitFor(() => expect(store.dirty()).toBe(false));
    const preventDefault = vi.fn();
    await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault });
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(order).toContain("quit_now");
    expect(windowMock.destroy).not.toHaveBeenCalled();
  });

  it("offers to save an active take in a clean document and keeps it running on Cancel", async () => {
    mockCommands({
      take_startup_project: () => null,
      get_project: () => ({ ...makeSnapshot(), file_path: "/old/song.ocp", dirty: false }),
      prepare_quit: () => ({ dirty: false, recording: true }),
      stop_recording: () => ({
        ...TRANSPORT_STOPPED(),
        project: { ...makeSnapshot(), file_path: "/old/song.ocp", dirty: true },
        record: { recording: false },
      }),
    });
    const { container } = render(() => <App />);
    await waitFor(() => expect(windowMock.onCloseRequested).toHaveBeenCalled());
    await waitFor(() => expect(store.dirty()).toBe(false));
    await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault: vi.fn() });
    expect(container.querySelector(".quit-overlay")).toBeInTheDocument();
    expect(store.dirty()).toBe(false);
    fireEvent.click(container.querySelector(".quit-btn-cancel")!);
    expect(order).not.toContain("stop_recording");
    expect(order).not.toContain("quit_now");

    await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault: vi.fn() });
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(order).toEqual(["prepare_quit", "prepare_quit", "stop_recording", "save_file", "quit_now"]));
  });

  it("keeps the window open if preparing or arming Quit fails", async () => {
    mockCommands({ prepare_quit: () => { throw new Error("The take is on disk"); } });
    const { container } = await requestQuit();
    expect(store.error()).toContain("The take is on disk");
    expect(order).not.toContain("quit_now");
    expect(windowMock.destroy).not.toHaveBeenCalled();

    mockCommands({ quit_now: () => { throw new Error("Stop recording before quitting"); } });
    await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault: vi.fn() });
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(store.error()).toContain("Stop recording before quitting"));
    expect(container.querySelector(".quit-overlay")).toBeInTheDocument();
    expect(windowMock.destroy).not.toHaveBeenCalled();
  });

  it("keeps the decision open if stopping an active take fails", async () => {
    mockCommands({
      prepare_quit: () => ({ dirty: true, recording: true }),
      stop_recording: () => { throw new Error("Could not finalize the take"); },
    });
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-discard")!);
    await waitFor(() => expect(store.error()).toContain("Could not finalize the take"));
    expect(order).toEqual(["prepare_quit", "stop_recording"]);
    expect(container.querySelector(".quit-overlay")).toBeInTheDocument();
    expect(windowMock.destroy).not.toHaveBeenCalled();
  });

  it("releases a window listener that finishes registering after unmount", async () => {
    let finish!: (release: () => void) => void;
    const release = vi.fn();
    windowMock.onCloseRequested.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    render(() => <App />);
    await waitFor(() => expect(finish).toBeTypeOf("function"));
    cleanup();
    finish(release);
    await waitFor(() => expect(release).toHaveBeenCalledOnce());
  });

  it("stays open after a rejected save and closes only after a successful retry", async () => {
    mockCommands({ save_file: () => { throw new Error("Save As resource collision"); } });
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(store.error()).toContain("resource collision"));
    expect(windowMock.destroy).not.toHaveBeenCalled();
    expect(container.querySelector(".quit-overlay")).toBeInTheDocument();
    expect(container.querySelector(".quit-dialog [role=alert]")).toHaveTextContent("resource collision");
    expect(store.dirty()).toBe(true);
    expect(store.filePath()).toBe("/old/song.ocp");
    mockCommands();
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(order).toContain("quit_now"));
    expect(store.dirty()).toBe(false);
  });

  it("does not close an unsaved project when the save dialog is cancelled", async () => {
    // A project that has never been saved: Save must ask for a path first.
    const untitled = () => ({ ...makeSnapshot(), file_path: null, dirty: true });
    mockCommands({
      take_startup_project: () => null,
      get_project: untitled,
      open_file: untitled,
    });
    saveDialogMock.mockResolvedValueOnce(null);
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(saveDialogMock).toHaveBeenCalled());
    expect(windowMock.destroy).not.toHaveBeenCalled();
    expect(order).not.toContain("quit_now");
    expect(store.dirty()).toBe(true);
    expect(container.querySelector(".quit-overlay")).toBeInTheDocument();
  });

  it("writes the file before closing when Save succeeds", async () => {
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-save")!);
    await waitFor(() => expect(order).toContain("quit_now"));
    expect(order.indexOf("save_file")).toBeLessThan(order.indexOf("quit_now"));
    expect(order).not.toContain("discard_unsaved");
    expect(store.dirty()).toBe(false);
  });

  it("discards the document before closing when Don't Save is answered", async () => {
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-discard")!);
    await waitFor(() => expect(order).toContain("quit_now"));
    expect(order).toContain("discard_unsaved");
    expect(order.indexOf("discard_unsaved")).toBeLessThan(order.indexOf("quit_now"));
    expect(order).not.toContain("save_file");
    expect(store.dirty()).toBe(false);
  });

  it("keeps the document dirty and writes nothing when Cancel is answered", async () => {
    const { container } = await requestQuit();
    fireEvent.click(container.querySelector(".quit-btn-cancel")!);
    await waitFor(() => expect(container.querySelector(".quit-overlay")).not.toBeInTheDocument());
    expect(order).not.toContain("save_file");
    expect(order).not.toContain("discard_unsaved");
    expect(windowMock.destroy).not.toHaveBeenCalled();
    expect(order).not.toContain("quit_now");
    expect(store.dirty()).toBe(true);
  });

  it("cancels on Escape without writing or closing", async () => {
    const { container } = await requestQuit();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(container.querySelector(".quit-overlay")).not.toBeInTheDocument());
    expect(order).not.toContain("save_file");
    expect(order).not.toContain("discard_unsaved");
    expect(windowMock.destroy).not.toHaveBeenCalled();
    expect(order).not.toContain("quit_now");
    expect(store.dirty()).toBe(true);
  });

  it("leaves the application instead of the window when the backend asks for a quit", async () => {
    const view = render(() => <App />);
    await waitFor(() => expect(listenMock).toHaveBeenCalledWith("quit_requested", expect.any(Function)));
    const handler = listenMock.mock.calls.find((call) => call[0] === "quit_requested")![1] as () => void;
    await waitFor(() => expect(store.dirty()).toBe(true));
    handler();
    await waitFor(() => expect(view.container.querySelector(".quit-overlay")).toBeInTheDocument());

    fireEvent.click(view.container.querySelector(".quit-btn-discard")!);
    await waitFor(() => expect(order).toContain("quit_now"));
    expect(order.indexOf("discard_unsaved")).toBeLessThan(order.indexOf("quit_now"));
    expect(windowMock.destroy).not.toHaveBeenCalled();
  });
});
