/**
 * The workspace chooser: the start surface that stands in for an empty timeline,
 * the recents it lists, and the two ways of leaving it — a folder (whose project
 * files it lists) and "continue without a workspace".
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openDialogMock, saveDialogMock } from "./setup";
import { store } from "../stores/project";
import { workspaceStore, type WorkspaceEntry, type WorkspaceProject } from "../stores/workspace";
import { WorkspaceChooser } from "../components/WorkspaceChooser";
import App from "../App";
import { i18n } from "../stores/i18n";

const RECENTS: WorkspaceEntry[] = [
  { name: "songs", path: "/Users/me/songs", opened: "2026-10-29T09:00:00.000Z" },
  { name: "sketches", path: "/Users/me/sketches", opened: "2026-10-28T09:00:00.000Z" },
];

const PROJECTS: WorkspaceProject[] = [
  { name: "a.ocp", path: "/Users/me/songs/a.ocp" },
  { name: "b.ocp", path: "/Users/me/songs/b.ocp" },
];

const backend = {
  recents: [] as WorkspaceEntry[],
  path: null as string | null,
  detached: false,
  chosen: false,
  exists: true,
  projects: [] as WorkspaceProject[],
  /** Project files a directory answers with when it is opened. */
  projectsIn: {} as Record<string, WorkspaceProject[]>,
  /** The project count before the backend cut the list. */
  totals: {} as Record<string, number>,
  opened: [] as string[],
  openedFiles: [] as string[],
  forgotten: [] as string[],
  saved: [] as string[],
  created: 0,
  /** Paths the one-command New wrote, in order. */
  createdAt: [] as string[],
};

function installBackend() {
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (command) {
      case "take_startup_project": return null;
      case "workspace_current": return view();
      case "workspaces_list": return backend.recents;
      case "workspace_open": {
        const path = String(typed.path);
        backend.opened.push(path);
        backend.path = path;
        backend.detached = false;
        backend.chosen = true;
        backend.exists = true;
        backend.projects = backend.projectsIn[path] ?? [];
        return view();
      }
      case "workspace_detached":
        backend.path = null;
        backend.detached = true;
        backend.chosen = false;
        backend.projects = [];
        return view();
      case "workspace_forget":
        backend.forgotten.push(String(typed.path));
        backend.recents = backend.recents.filter((entry) => entry.path !== typed.path);
        return backend.recents;
      case "new_project":
        backend.created += 1;
        return makeSnapshot();
      case "new_project_at": {
        const path = String(typed.path);
        backend.created += 1;
        backend.createdAt.push(path);
        return {
          ...makeSnapshot(),
          file_path: path,
          project: { ...makeSnapshot().project, name: path.split("/").pop()!.replace(/\.ocp$/i, "") },
        };
      }
      case "set_project_name":
        return { ...makeSnapshot(), project: { ...makeSnapshot().project, name: String(typed.name) }, dirty: true };
      case "save_file":
        backend.saved.push(String(typed.path));
        return { path: String(typed.path), snapshot: null };
      case "open_file":
        backend.openedFiles.push(String(typed.path));
        return { ...makeSnapshot(), file_path: String(typed.path) };
      case "get_project": return makeSnapshot();
      default: return makeSnapshot();
    }
  });
}

function view() {
  return {
    path: backend.path,
    name: backend.path ? backend.path.split("/").pop() ?? null : null,
    detached: backend.detached,
    chosen: backend.chosen,
    exists: backend.exists,
    projects: backend.projects,
    total_projects: backend.path ? backend.totals[backend.path] ?? backend.projects.length : 0,
  };
}

const chooser = () => document.querySelector(".workspace-chooser");
const recentRows = () => [...document.querySelectorAll(".workspace-recent")];
const recentNames = () => recentRows().map((row) => row.querySelector(".workspace-recent-name")?.textContent);
const recentPaths = () => recentRows().map((row) => row.querySelector(".workspace-recent-path")?.textContent);
const projectNames = () => [...document.querySelectorAll(".workspace-project")].map((node) => node.textContent);
/** The surface disables its actions while an open is in flight. */
const settled = () => waitFor(() => expect(workspaceStore.busy()).toBe(false));

beforeEach(async () => {
  invokeMock.mockReset();
  openDialogMock.mockReset();
  saveDialogMock.mockReset();
  saveDialogMock.mockResolvedValue(null);
  backend.recents = RECENTS.map((entry) => ({ ...entry }));
  backend.path = null;
  backend.detached = false;
  backend.chosen = false;
  backend.exists = true;
  backend.projects = [];
  backend.projectsIn = { "/Users/me/songs": PROJECTS.map((project) => ({ ...project })) };
  backend.totals = {};
  backend.opened = [];
  backend.openedFiles = [];
  backend.forgotten = [];
  backend.saved = [];
  backend.created = 0;
  backend.createdAt = [];
  i18n.setLocale("en");
  workspaceStore.reset();
  installBackend();
  // No project open: the fixture starts on the start surface.
  await store.newProject();
});

afterEach(cleanup);

describe("the start surface", () => {
  it("lists the recent workspaces newest first, by name and full path", async () => {
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    expect(recentNames()).toEqual(["songs", "sketches"]);
    expect(recentPaths()).toEqual(["/Users/me/songs", "/Users/me/sketches"]);
    // The three actions the surface offers are there, in one row.
    const actions = [...view.container.querySelectorAll(".workspace-action")].map((node) => node.textContent);
    expect(actions).toEqual(["Open folder…", "Open project file…", "Continue without a workspace"]);
  });

  it("opens a recent folder, lists its project files and opens the one picked", async () => {
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-recent-open")!);
    await waitFor(() => expect(projectNames()).toEqual(["a.ocp", "b.ocp"]));
    expect(backend.opened).toEqual(["/Users/me/songs"]);
    expect(document.querySelector(".workspace-current-name")?.textContent).toBe("songs");
    expect(document.querySelector(".workspace-current-path")?.textContent).toBe("/Users/me/songs");
    await waitFor(() => expect(recentNames()[0]).toBe("songs"));

    fireEvent.click(document.querySelectorAll(".workspace-project")[1]);
    await waitFor(() => expect(backend.openedFiles).toEqual(["/Users/me/songs/b.ocp"]));
    // The file's directory is the workspace, so the list stays on this folder.
    expect(backend.opened).toEqual(["/Users/me/songs", "/Users/me/songs/b.ocp"]);
  });

  it("opens the folder the picker returns", async () => {
    openDialogMock.mockResolvedValueOnce("/Users/me/sketches");
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-action-open-folder")!);
    await waitFor(() => expect(backend.opened).toEqual(["/Users/me/sketches"]));
    expect(openDialogMock.mock.calls[0][0]).toMatchObject({ directory: true, multiple: false });
  });

  it("offers to create the first project of a folder that has none", async () => {
    openDialogMock.mockResolvedValueOnce("/Users/me/empty");
    saveDialogMock.mockResolvedValueOnce("/Users/me/empty/Quiet Morning.ocp");
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-action-open-folder")!);
    await waitFor(() =>
      expect(document.querySelector(".workspace-empty")?.textContent).toBe("This folder has no project files yet."));
    await settled();

    // The empty folder is not a dead end: the surface can create the first file.
    const create = view.container.querySelector(".workspace-action-new-project") as HTMLElement;
    expect(create).toBeTruthy();
    // The action names itself; the menu's shortcut has no meaning here.
    expect(create.textContent).toBe("New project");
    const created = backend.created;
    invokeMock.mockClear();
    fireEvent.click(create);
    await waitFor(() => expect(backend.createdAt).toEqual(["/Users/me/empty/Quiet Morning.ocp"]));
    expect(backend.created).toBe(created + 1);
    // One command creates, names and writes the project: no rename, no save.
    const commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands).toContain("new_project_at");
    expect(commands).not.toContain("new_project");
    expect(commands).not.toContain("set_project_name");
    expect(commands).not.toContain("save_file");
    expect(backend.saved).toEqual([]);
    await waitFor(() => expect(workspaceStore.startSurface()).toBe(false));
    expect(store.filePath()).toBe("/Users/me/empty/Quiet Morning.ocp");
    expect(store.project.name).toBe("Quiet Morning");
    expect(saveDialogMock).toHaveBeenCalledWith(expect.objectContaining({ defaultPath: "/Users/me/empty/untitled.ocp", title: "New project" }));
  });

  it("creates the bundled starter in one command at the chosen filename", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    saveDialogMock.mockResolvedValueOnce("/Users/me/songs/My first sketch.ocp");
    const view = render(() => <WorkspaceChooser />);
    invokeMock.mockClear();
    fireEvent.click(view.container.querySelector(".workspace-action-starter-project")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("new_project_at", {
      path: "/Users/me/songs/My first sketch.ocp", template: "starter",
    }));
    const commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands).not.toContain("new_project");
    expect(commands).not.toContain("update_dsl");
    expect(commands).not.toContain("save_file");
    expect(saveDialogMock).toHaveBeenCalledWith(expect.objectContaining({ title: "Create starter sketch" }));
    await waitFor(() => expect(workspaceStore.startSurface()).toBe(false));
  });

  it("cancels starter creation before any project command", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    invokeMock.mockClear();
    expect(await workspaceStore.newProjectHere("starter")).toBe(false);
    expect(invokeMock.mock.calls.some(([command]) => command === "new_project_at")).toBe(false);
    expect(workspaceStore.busy()).toBe(false);
  });

  it("preserves the current project and reports a refused starter filename", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    saveDialogMock.mockResolvedValueOnce("/Users/me/songs/a.ocp");
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "new_project_at") throw new Error("File already exists");
      return original(command, args);
    });
    const before = JSON.stringify(store.project);
    expect(await workspaceStore.newProjectHere("starter")).toBe(false);
    expect(JSON.stringify(store.project)).toBe(before);
    expect(workspaceStore.error()).toContain("File already exists");
  });

  it("suggests a free filename but creates only the picker choice", async () => {
    saveDialogMock.mockResolvedValueOnce("/Users/me/songs/Piano Sketch");
    backend.projectsIn["/Users/me/songs"] = [
      { name: "Untitled.ocp", path: "/Users/me/songs/Untitled.ocp" },
      { name: "untitled 2.ocp", path: "/Users/me/songs/untitled 2.ocp" },
    ];
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-recent-open")!);
    await waitFor(() => expect(projectNames()).toEqual(["Untitled.ocp", "untitled 2.ocp"]));
    await settled();
    fireEvent.click(view.container.querySelector(".workspace-action-new-project")!);
    await waitFor(() => expect(backend.createdAt).toEqual(["/Users/me/songs/Piano Sketch.ocp"]));
    expect(store.project.name).toBe("Piano Sketch");
    expect(saveDialogMock).toHaveBeenCalledWith(expect.objectContaining({ defaultPath: "/Users/me/songs/untitled 3.ocp" }));
  });

  it("cancels the filename picker without resetting or saving the project", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    invokeMock.mockClear();
    const before = JSON.stringify(store.project);
    expect(await workspaceStore.newProjectHere()).toBe(false);
    expect(JSON.stringify(store.project)).toBe(before);
    expect(invokeMock.mock.calls.some(([command]) => command === "new_project_at")).toBe(false);
    expect(workspaceStore.busy()).toBe(false);
    expect(workspaceStore.error()).toBeNull();
  });

  it("reports picker failure before making any destructive call", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    invokeMock.mockClear();
    saveDialogMock.mockRejectedValueOnce(new Error("Picker unavailable"));
    expect(await workspaceStore.newProjectHere()).toBe(false);
    expect(workspaceStore.error()).toBe("Picker unavailable");
    expect(invokeMock.mock.calls.some(([command]) => command === "new_project_at")).toBe(false);
    expect(workspaceStore.busy()).toBe(false);
  });

  it("stops creation when the create command fails", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    saveDialogMock.mockResolvedValueOnce("/Users/me/songs/Named.ocp");
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "new_project_at") throw new Error("Creation failed");
      return original(command, args);
    });
    expect(await workspaceStore.newProjectHere()).toBe(false);
    expect(workspaceStore.error()).toContain("Creation failed");
    expect(backend.createdAt).toEqual([]);
    expect(workspaceStore.busy()).toBe(false);
  });

  it("preserves a draft edited while the filename picker is open", async () => {
    await workspaceStore.init();
    await workspaceStore.open("/Users/me/songs");
    let choose!: (path: string) => void;
    saveDialogMock.mockImplementationOnce(() => new Promise<string>((resolve) => { choose = resolve; }));
    const pending = workspaceStore.newProjectHere();
    await waitFor(() => expect(saveDialogMock).toHaveBeenCalled());
    await store.setProjectName("Unsaved draft");
    const created = backend.created;
    choose("/Users/me/songs/New.ocp");
    expect(await pending).toBe(false);
    expect(backend.created).toBe(created);
    expect(backend.createdAt).toEqual([]);
    expect(store.project.name).toBe("Unsaved draft");
    expect(workspaceStore.error()).toBe("Unsaved changes");
  });

  it("does not offer to create a project while one is open", async () => {
    await workspaceStore.init();
    await store.openFile("/Users/me/songs/a.ocp");
    const view = render(() => <WorkspaceChooser />);

    expect(workspaceStore.startSurface()).toBe(false);
    expect(view.container.querySelector(".workspace-action-new-project")).toBeFalsy();
    expect(view.container.querySelector(".workspace-action-starter-project")).toBeFalsy();
  });

  it("forgets a recent directory without touching the others", async () => {
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-recent-forget")!);
    await waitFor(() => expect(recentNames()).toEqual(["sketches"]));
    expect(backend.forgotten).toEqual(["/Users/me/songs"]);
  });

  it("marks a remembered folder that is gone", async () => {
    backend.recents[0] = { ...backend.recents[0], exists: false };
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    await waitFor(() =>
      expect(recentRows()[0].querySelector(".workspace-recent-missing")?.textContent).toBe("not found"));
    expect(recentRows()[1].querySelector(".workspace-recent-missing")).toBeFalsy();
    // The row still offers the way out of the list.
    fireEvent.click(recentRows()[0].querySelector(".workspace-recent-forget")!);
    await waitFor(() => expect(backend.forgotten).toEqual(["/Users/me/songs"]));
    expect(view.container.querySelector(".workspace-recent-missing")).toBeFalsy();
  });

  it("marks a current workspace whose folder is gone", async () => {
    backend.path = "/Users/me/songs";
    backend.chosen = true;
    backend.exists = false;
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    expect(view.container.querySelector(".workspace-missing")?.textContent).toBe("not found");
  });

  it("says how many project files the list cut", async () => {
    backend.path = "/Users/me/songs";
    backend.chosen = true;
    backend.projects = PROJECTS;
    backend.totals = { "/Users/me/songs": 240 };
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    expect(view.container.querySelector(".workspace-section-label")?.textContent)
      .toContain("Showing the first 2 of 240 project files.");
  });

  it("names the cap and the new-project action in every locale", () => {
    for (const locale of ["en", "zh-Hans", "zh-Hant", "ja", "es"] as const) {
      expect(i18n.has(locale, "workspace.truncated"), `workspace.truncated ${locale}`).toBe(true);
      expect(i18n.has(locale, "workspace.newProject"), `workspace.newProject ${locale}`).toBe(true);
      expect(i18n.has(locale, "workspace.starterProject"), `workspace.starterProject ${locale}`).toBe(true);
    }
    // The new-project label carries no keyboard shortcut; the menu keeps that.
    expect(i18n.t("workspace.newProject")).toBe("New project");
    expect(i18n.t("toolbar.newProject")).toBe("New project (Cmd+N)");
  });

  it("scrolls a long list inside the card instead of growing it", async () => {
    backend.path = "/Users/me/songs";
    backend.chosen = true;
    backend.projects = PROJECTS;
    await workspaceStore.init();
    render(() => <WorkspaceChooser />);

    const projects = document.querySelector(".workspace-project-list") as HTMLElement;
    const recents = document.querySelector(".workspace-recent-list") as HTMLElement;
    expect(projects.style.maxHeight).toBe("32vh");
    expect(projects.style.overflowY).toBe("auto");
    expect(recents.style.maxHeight).toBe("30vh");
    expect(recents.style.overflowY).toBe("auto");
  });

  it("continues without a workspace and says so", async () => {
    await workspaceStore.init();
    const view = render(() => <WorkspaceChooser />);

    fireEvent.click(view.container.querySelector(".workspace-action-detached")!);
    await waitFor(() => expect(backend.detached).toBe(true));
    expect(invokeMock.mock.calls.some(([command]) => command === "workspace_detached")).toBe(true);
    expect(workspaceStore.startSurface()).toBe(false);
    expect(workspaceStore.scopePath()).toBeNull();
  });
});

describe("the scope conversations live in", () => {
  it("follows the open project while no folder was chosen", async () => {
    backend.path = "/Users/me/a";
    backend.projects = [];
    await workspaceStore.init();
    await store.openFile("/Users/me/a/song.ocp");
    expect(workspaceStore.scopePath()).toBe("/Users/me/a");

    // Opening a file elsewhere from the File menu moves the scope at once.
    await store.openFile("/Users/me/b/other.ocp");
    expect(workspaceStore.scopePath()).toBe("/Users/me/b");
  });

  it("stays on the chosen folder when a file outside it is opened", async () => {
    backend.path = "/Users/me/songs";
    backend.chosen = true;
    backend.projects = PROJECTS;
    await workspaceStore.init();

    await store.openFile("/Users/me/elsewhere/other.ocp");
    expect(workspaceStore.scopePath()).toBe("/Users/me/songs");
    expect(workspaceStore.startSurface()).toBe(false);
  });

  it("re-reads the view when the chooser is opened, so it is never stale", async () => {
    await workspaceStore.init();
    expect(workspaceStore.view()?.path).toBeNull();
    // A project opened from the File menu moves the backend's answer.
    backend.path = "/Users/me/b";
    backend.projects = [];
    workspaceStore.openChooser();
    await waitFor(() => expect(workspaceStore.view()?.path).toBe("/Users/me/b"));
    expect(invokeMock.mock.calls.filter(([command]) => command === "workspace_current").length).toBeGreaterThan(1);
  });
});

describe("the app's start surface", () => {
  it("keeps the surface while a folder has no project open, then gives the timeline back", async () => {
    const view = render(() => <App />);
    await waitFor(() => expect(chooser()).toBeTruthy());
    expect(view.container.querySelector(".timeline-panel")).toBeFalsy();

    // Choosing a folder lists its files; it does not open one.
    openDialogMock.mockResolvedValueOnce("/Users/me/songs");
    fireEvent.click(view.container.querySelector(".workspace-action-open-folder")!);
    await waitFor(() => expect(backend.path).toBe("/Users/me/songs"));
    await waitFor(() => expect(projectNames()).toEqual(["a.ocp", "b.ocp"]));
    expect(view.container.querySelector(".workspace-chooser")).toBeTruthy();
    expect(view.container.querySelector(".timeline-panel")).toBeFalsy();
    await settled();

    fireEvent.click(document.querySelectorAll(".workspace-project")[0]);
    await waitFor(() => expect(backend.openedFiles).toEqual(["/Users/me/songs/a.ocp"]));
    await waitFor(() => expect(view.container.querySelector(".timeline-panel")).toBeTruthy());
    expect(view.container.querySelector(".workspace-chooser")).toBeFalsy();
  });

  it("reopens the chooser from the File menu after a workspace is open", async () => {
    const view = render(() => <App />);
    await waitFor(() => expect(chooser()).toBeTruthy());
    fireEvent.click(view.container.querySelector(".workspace-action-detached")!);
    await waitFor(() => expect(backend.detached).toBe(true));
    await waitFor(() => expect(view.container.querySelector(".workspace-chooser")).toBeFalsy());

    fireEvent.click(view.container.querySelector('[aria-label="More file actions"]')!);
    fireEvent.click(screenItem("Switch workspace…"));
    await waitFor(() => expect(document.querySelector(".workspace-chooser-dialog")).toBeTruthy());
  });
});

/** The menu item with this text, from the toolbar's open file menu. */
function screenItem(label: string): HTMLElement {
  const item = [...document.querySelectorAll<HTMLElement>(".toolbar-menu-item")]
    .find((node) => node.textContent?.trim() === label);
  if (!item) throw new Error(`no menu item "${label}"`);
  return item;
}

describe("the chooser reached from the panel", () => {
  it("is opened by the AI panel's scope line through the shared store", async () => {
    await workspaceStore.init();
    expect(workspaceStore.chooserOpen()).toBe(false);
    workspaceStore.openChooser();
    expect(workspaceStore.chooserOpen()).toBe(true);
    workspaceStore.closeChooser();
    expect(workspaceStore.chooserOpen()).toBe(false);
  });
});
