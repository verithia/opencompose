/**
 * The workspace the app is working in: one directory, the `.ocp` files it holds,
 * and the recent directories the chooser lists.
 *
 * The current workspace is the backend's, so it lives here rather than in a
 * component: switching tabs unmounts the AI panel, and the start surface is a
 * sibling of the timeline. Every action answers a fresh view, so the timeline,
 * the chooser and the panel's header read one signal.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { store } from "./project";
import { i18n } from "./i18n";
import { directoryOf } from "../ai/sessions";

/** One `.ocp` file in the workspace. */
export interface WorkspaceProject {
  name: string;
  path: string;
}

/** One remembered directory. */
export interface WorkspaceEntry {
  name: string;
  path: string;
  opened: string;
  /** False when the folder is gone; the chooser marks the row. */
  exists?: boolean;
}

/**
 * What the backend reports: the directory conversations are scoped to, whether
 * the operator chose to work without one, and the files to open. A view with no
 * path and `detached: false` means nothing has been chosen yet.
 */
export interface WorkspaceView {
  path: string | null;
  name?: string | null;
  detached: boolean;
  /** True when a directory was chosen, not merely adopted from the open file. */
  chosen: boolean;
  /** False when the directory is gone. */
  exists: boolean;
  projects: WorkspaceProject[];
  /** Files the directory holds before the backend cut the list. */
  total_projects: number;
}

const [view, setView] = createSignal<WorkspaceView | null>(null);
const [recents, setRecents] = createSignal<WorkspaceEntry[]>([]);
const [asked, setAsked] = createSignal(false);
const [busy, setBusy] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);
const [chooserOpen, setChooserOpen] = createSignal(false);

/** The view a command answer carries, or `null` when it is not one. */
export function workspaceView(payload: unknown): WorkspaceView | null {
  const value = payload as Partial<WorkspaceView> | null;
  if (!value || typeof value !== "object" || typeof value.detached !== "boolean") return null;
  const projects = Array.isArray(value.projects)
    ? value.projects.filter((project): project is WorkspaceProject =>
      !!project && typeof project.path === "string" && typeof project.name === "string")
    : [];
  return {
    path: typeof value.path === "string" && value.path ? value.path : null,
    name: typeof value.name === "string" ? value.name : null,
    detached: value.detached,
    // An answer that does not say is read as a chosen directory, which is what
    // its path means on a backend that predates the field.
    chosen: value.chosen !== false,
    exists: value.exists !== false,
    projects,
    total_projects: typeof value.total_projects === "number" && value.total_projects > projects.length
      ? value.total_projects
      : projects.length,
  };
}

/** The recent list a `workspaces_list` answer carries, or an empty list. */
export function workspaceEntries(payload: unknown): WorkspaceEntry[] {
  if (!Array.isArray(payload)) return [];
  return payload.filter((entry): entry is WorkspaceEntry => {
    const value = entry as Partial<WorkspaceEntry> | null;
    return !!value && typeof value.path === "string" && typeof value.name === "string";
  }).map((entry) => ({ ...entry, exists: entry.exists !== false }));
}

/**
 * The directory the AI panel scopes conversations to, or `null` when detached.
 *
 * Only a chosen folder is a fixed scope; anything else follows the open project,
 * read live, so opening another file moves the conversations at once.
 */
function scopePath(): string | null {
  const current = view();
  if (current?.chosen && current.path) return current.path;
  const open = store.filePath();
  return open ? directoryOf(open) : null;
}

/**
 * True while the start surface stands in for the timeline: nothing is open yet.
 *
 * A chosen folder keeps it until a project is opened, so its list of projects —
 * and the way to create one — stays reachable. Before the backend answers there
 * is nothing to decide with, so the timeline keeps the area.
 */
function startSurface(): boolean {
  if (!asked()) return false;
  const current = view();
  if (!current || current.detached) return false;
  return !store.filePath();
}

async function init(): Promise<void> {
  try {
    const answer = workspaceView(await invoke("workspace_current"));
    if (answer) setView(answer);
  } catch (e) {
    setError(errorText(e));
  } finally {
    setAsked(true);
  }
  await refresh();
}

/** Re-reads the view, so a chooser opened later is never stale. */
async function syncView(): Promise<void> {
  try {
    const answer = workspaceView(await invoke("workspace_current"));
    if (answer) setView(answer);
  } catch (e) {
    setError(errorText(e));
  }
}

async function refresh(): Promise<void> {
  try {
    setRecents(workspaceEntries(await invoke("workspaces_list")));
  } catch (e) {
    setError(errorText(e));
  }
}

/** Makes `path` (a directory, or a project file's directory) the workspace. */
async function open(path: string): Promise<boolean> {
  setBusy(true);
  setError(null);
  try {
    const answer = workspaceView(await invoke("workspace_open", { path }));
    if (!answer) throw new Error("workspace_open returned no workspace");
    setView(answer);
    await refresh();
    return true;
  } catch (e) {
    setError(errorText(e));
    return false;
  } finally {
    setBusy(false);
  }
}

/**
 * Opens a project file and adopts the directory that holds it, so the session
 * list follows the file the user chose instead of the folder they left.
 */
async function openProject(path: string): Promise<boolean> {
  // The same guard every open route uses; Cancel must not adopt the directory
  // or touch the current document.
  if (!await store.confirmDiscardChanges()) return false;
  if (!await open(path)) return false;
  await store.openFile(path);
  setChooserOpen(false);
  return true;
}

async function openFolder(): Promise<void> {
  const { open: pick } = await import("@tauri-apps/plugin-dialog");
  const picked = await pick({ directory: true, multiple: false });
  if (typeof picked === "string") await open(picked);
}

async function openProjectFile(): Promise<void> {
  const { open: pick } = await import("@tauri-apps/plugin-dialog");
  const picked = await pick({ filters: [{ name: "OpenCompose", extensions: ["ocp"] }] });
  if (typeof picked === "string") await openProject(picked);
}

/** The first free `untitled*.ocp` name in the chosen directory. */
function freeProjectName(projects: WorkspaceProject[]): string {
  const taken = new Set(projects.map((project) => project.name.toLowerCase()));
  if (!taken.has("untitled.ocp")) return "untitled.ocp";
  for (let index = 2; index < 1000; index += 1) {
    const name = `untitled ${index}.ocp`;
    if (!taken.has(name)) return name;
  }
  return `untitled ${Date.now()}.ocp`;
}

/**
 * Creates the first project in the chosen directory and opens it, so a folder
 * with no `.ocp` is a place to start rather than the end of the chooser.
 */
async function newProjectHere(template?: "starter"): Promise<boolean> {
  const current = view();
  const dir = current?.path;
  if (!current || !dir || busy() || store.filePath()) return false;
  if (store.dirty()) { setError(i18n.t("dialog.unsavedTitle")); return false; }
  setBusy(true);
  setError(null);
  try {
    const defaultPath = `${dir.replace(/[\\/]+$/, "")}/${freeProjectName(current.projects)}`;
    const { save: pick } = await import("@tauri-apps/plugin-dialog");
    const picked = await pick({
      title: i18n.t(template ? "workspace.starterProject" : "workspace.newProject"),
      defaultPath,
      filters: [{ name: "OpenCompose", extensions: ["ocp"] }],
    });
    if (!picked) return false;
    // A project may have changed while the native picker was open.
    if (store.filePath() || store.dirty()) {
      setError(i18n.t("dialog.unsavedTitle"));
      return false;
    }
    const path = /\.ocp$/i.test(picked) ? picked : `${picked}.ocp`;
    // One command creates, names and writes the project; nothing to rename or
    // save afterwards, so the document never exists under a default name.
    if (!await store.newProjectAt(path, template)) {
      setError(store.error() ?? `${path} could not be created`);
      return false;
    }
    setChooserOpen(false);
    await syncView();
    return true;
  } catch (e) {
    setError(errorText(e));
    return false;
  } finally {
    setBusy(false);
  }
}

/** Continue without a workspace: conversations live under `detached`. */
async function continueDetached(): Promise<void> {
  setBusy(true);
  setError(null);
  try {
    const answer = workspaceView(await invoke("workspace_detached"));
    if (answer) setView(answer);
    setChooserOpen(false);
  } catch (e) {
    setError(errorText(e));
  } finally {
    setBusy(false);
  }
}

async function forget(path: string): Promise<void> {
  try {
    setRecents(workspaceEntries(await invoke("workspace_forget", { path })));
  } catch (e) {
    setError(errorText(e));
  }
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const workspaceStore = {
  view,
  recents,
  busy,
  error,
  chooserOpen,
  init,
  refresh,
  open,
  openProject,
  openFolder,
  openProjectFile,
  newProjectHere,
  continueDetached,
  forget,
  scopePath,
  startSurface,
  /** Opening the chooser re-reads the view, so it shows the backend's answer. */
  openChooser: () => {
    setChooserOpen(true);
    void syncView();
  },
  closeChooser: () => setChooserOpen(false),
  /** Clears the run state between tests. */
  reset: () => {
    setView(null);
    setRecents([]);
    setAsked(false);
    setBusy(false);
    setError(null);
    setChooserOpen(false);
  },
};
