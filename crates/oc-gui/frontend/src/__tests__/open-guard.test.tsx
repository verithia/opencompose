/**
 * The Open/New guard: a dirty document asks before a load replaces it.
 *
 * Save writes first and aborts the load when it fails, Don't Save loads, and
 * Cancel writes nothing and touches neither the document nor the file.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import {
  invokeMock, makeSnapshot, openDialogMock, saveDialogMock,
} from "./setup";
import { store } from "../stores/project";
import { recoveryStore } from "../stores/recovery";
import { workspaceStore } from "../stores/workspace";
import { i18n } from "../stores/i18n";
import { Toolbar } from "../components/Toolbar";

const OLD_PATH = "/songs/old.ocp";
const NEW_PATH = "/songs/new.ocp";

/** The commands the app sends, in order. */
let calls: string[] = [];

function dirtySnapshot() {
  return { ...makeSnapshot(), file_path: OLD_PATH, dirty: true };
}

function cleanSnapshot() {
  return { ...makeSnapshot(), file_path: OLD_PATH, dirty: false };
}

function mockBackend(options: {
  project?: () => unknown;
  recoveries?: () => unknown;
  save?: () => unknown;
} = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    calls.push(cmd);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_project") return options.project ? options.project() : cleanSnapshot();
    if (cmd === "open_file") {
      return { ...makeSnapshot({ name: "Opened" }), file_path: NEW_PATH };
    }
    if (cmd === "new_project") return makeSnapshot({ name: "Fresh" });
    if (cmd === "save_file") return options.save ? options.save() : { path: OLD_PATH, snapshot: null };
    if (cmd === "list_recoveries") return options.recoveries ? options.recoveries() : [];
    if (cmd === "set_loop_region") return { enabled: false, start_beats: null, end_beats: null };
    if (cmd === "workspace_open") {
      return {
        path: "/songs", detached: false, chosen: true, exists: true,
        projects: [], total_projects: 0,
      };
    }
    if (cmd === "workspaces_list") return [];
    if (cmd === "get_undo_state") {
      return {
        can_undo: false, can_redo: false, undo_label: null, redo_label: null,
        undo_labels: [], redo_labels: [],
      };
    }
    if (cmd === "get_undo_history") return [];
    return makeSnapshot();
  });
}

function overlay(container: HTMLElement) {
  return container.querySelector(".quit-overlay");
}

beforeEach(async () => {
  vi.clearAllMocks();
  i18n.setLocale("en");
  calls = [];
  workspaceStore.reset();
  // A prompt left open by an earlier case must not answer this one.
  store.answerUnsavedPrompt("cancel");
  openDialogMock.mockReset();
  openDialogMock.mockResolvedValue(NEW_PATH);
  saveDialogMock.mockReset();
  saveDialogMock.mockResolvedValue(null);
});

describe("the Open guard", () => {
  it("does not open or write anything when Cancel answers the prompt", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.openFile")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-cancel")!);
    await waitFor(() => expect(overlay(view.container)).not.toBeInTheDocument());

    expect(calls).not.toContain("open_file");
    expect(calls).not.toContain("save_file");
    expect(openDialogMock).not.toHaveBeenCalled();
    expect(store.filePath()).toBe(OLD_PATH);
    expect(store.dirty()).toBe(true);
  });

  it("opens the picked file after Don't Save", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.openFile")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-discard")!);

    await waitFor(() => expect(calls).toContain("open_file"));
    expect(openDialogMock).toHaveBeenCalled();
    expect(store.filePath()).toBe(NEW_PATH);
    expect(store.dirty()).toBe(false);
  });

  it("writes the file before opening when Save succeeds", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.openFile")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-save")!);

    await waitFor(() => expect(calls).toContain("open_file"));
    expect(calls.indexOf("save_file")).toBeLessThan(calls.indexOf("open_file"));
    expect(calls.indexOf("save_file")).toBeGreaterThan(-1);
    expect(overlay(view.container)).not.toBeInTheDocument();
  });

  it("keeps the prompt open and aborts the open when Save fails", async () => {
    mockBackend({
      project: dirtySnapshot,
      save: () => { throw new Error("Save As resource collision"); },
    });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.openFile")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-save")!);

    await waitFor(() => expect(store.error()).toContain("resource collision"));
    expect(overlay(view.container)).toBeInTheDocument();
    expect(calls).not.toContain("open_file");
    expect(openDialogMock).not.toHaveBeenCalled();
    expect(store.filePath()).toBe(OLD_PATH);
    expect(store.dirty()).toBe(true);
  });

  it("opens immediately when the project is clean", async () => {
    mockBackend();
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.openFile")));
    await waitFor(() => expect(calls).toContain("open_file"));
    expect(overlay(view.container)).not.toBeInTheDocument();
    expect(openDialogMock).toHaveBeenCalled();
  });

  it("guards a recent-file click before adopting its workspace", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    const opening = workspaceStore.openProject(NEW_PATH);
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-cancel")!);

    expect(await opening).toBe(false);
    expect(calls).not.toContain("workspace_open");
    expect(calls).not.toContain("open_file");
    expect(store.filePath()).toBe(OLD_PATH);
  });
});

describe("the New guard", () => {
  it("does not create a project when Cancel answers the prompt", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.newProject")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-cancel")!);
    await waitFor(() => expect(overlay(view.container)).not.toBeInTheDocument());

    expect(calls).not.toContain("new_project");
    expect(calls).not.toContain("save_file");
    expect(store.filePath()).toBe(OLD_PATH);
  });

  it("creates the project after Don't Save", async () => {
    mockBackend({ project: dirtySnapshot });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.newProject")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-discard")!);

    await waitFor(() => expect(calls).toContain("new_project"));
    expect(store.project.name).toBe("Fresh");
  });

  it("does not create a project when Save fails", async () => {
    mockBackend({
      project: dirtySnapshot,
      save: () => { throw new Error("disk is full"); },
    });
    await store.initProject();
    const view = render(() => <Toolbar />);

    fireEvent.click(view.getByLabelText(i18n.t("toolbar.newProject")));
    await waitFor(() => expect(overlay(view.container)).toBeInTheDocument());
    fireEvent.click(view.container.querySelector(".quit-btn-save")!);

    await waitFor(() => expect(store.error()).toContain("disk is full"));
    expect(calls).not.toContain("new_project");
    expect(overlay(view.container)).toBeInTheDocument();
  });
});

describe("recovery after a load", () => {
  it("offers a copy that is newer than the file just opened", async () => {
    const copy = {
      path: "/home/.opencompose/recovery/abc/2000-old.ocp",
      project_name: "Old",
      saved_at_ms: 2000,
      size: 128,
      newer_than_disk: true,
      restore_disabled_reason: null,
    };
    mockBackend({ recoveries: () => [copy] });
    await store.initProject();

    await store.openFile(NEW_PATH);

    await waitFor(() => expect(recoveryStore.promptOpen()).toBe(true));
    expect(recoveryStore.newer()?.path).toBe(copy.path);
    // The list was read after the open, not only at startup.
    expect(calls.lastIndexOf("list_recoveries")).toBeGreaterThan(calls.indexOf("open_file"));
  });
});
