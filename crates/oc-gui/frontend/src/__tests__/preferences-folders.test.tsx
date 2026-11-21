/**
 * The Folders page: the searched plugin and sound folders under kind tabs and
 * folding origin groups, the selected folder's details, adding and removing
 * the user's own, refusals worded from the backend's code, and a rescan with
 * the first-run scan's progress.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { PreferencesModal } from "../components/PreferencesModal";
import { reserveBelow, splitPath } from "../components/preferences/FoldersPage";
import { i18n } from "../stores/i18n";
import { localScan } from "../stores/localScan";
import type { LocalContentSummary, LocalScanProgress, SearchedFolder } from "../stores/api";
import { invokeMock, listenMock, openDialogMock } from "./setup";

const kind = (folders: SearchedFolder[], total: number) => ({
  total,
  formats: [],
  folders,
  items: [],
  truncated: false,
});

const USER_PLUGINS: SearchedFolder = { path: "/Volumes/Studio/Plugins", origin: "user", exists: true, count: 4 };
const LIBRARY: SearchedFolder = {
  path: "/Volumes/Studio/Fonts",
  origin: "user",
  exists: true,
  count: 2,
  library_id: "lib-fonts",
};

function summary(options: { plugins?: SearchedFolder[]; sounds?: SearchedFolder[]; pluginTotal?: number } = {}): LocalContentSummary {
  const plugins = options.plugins ?? [
    { path: "/Library/Audio/Plug-Ins/VST3", origin: "standard", exists: true, count: 2 },
    { path: "/Library/Audio/Plug-Ins/LV2", origin: "standard", exists: false, count: 0 },
    { path: "/Users/me/Library/Application Support/REAPER/UserPlugins/FX", origin: "host_settings", exists: true, count: 1 },
    USER_PLUGINS,
  ];
  const sounds = options.sounds ?? [
    { path: "/Users/me/Music/SoundFonts", origin: "standard", exists: true, count: 1 },
    LIBRARY,
  ];
  return {
    plugins: kind(plugins, options.pluginTotal ?? 7),
    instruments: kind(sounds, 3),
    samples: kind(sounds.map((folder) => ({ ...folder, count: 5 })), 10),
    scanning: false,
  };
}

let current: LocalContentSummary;

beforeEach(() => {
  i18n.setLocale("en");
  localScan.reset();
  current = summary();
  invokeMock.mockReset();
  openDialogMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "local_content_summary" || command === "local_content_scan") return current;
    throw new Error(`unexpected command ${command}`);
  });
});

afterEach(() => cleanup());

async function openFolders() {
  render(() => <PreferencesModal open onClose={() => {}} />);
  fireEvent.click(document.querySelector('[data-page="folders"]')!);
  await screen.findByText("Plugin folders");
}

const block = (kindName: "plugins" | "sounds") =>
  document.querySelector<HTMLElement>(`.preferences-folders[data-kind="${kindName}"]`)!;
const group = (kindName: "plugins" | "sounds", origin: string) =>
  block(kindName).querySelector<HTMLElement>(`.preferences-folder-group[data-origin="${origin}"]`)!;
/** The row of one folder, found by its full path. */
const row = (path: string) => screen.getByTitle(path).closest<HTMLElement>(".preferences-folder-row")!;
const details = () => document.querySelector<HTMLElement>(".preferences-folder-details")!;

describe("the Folders preferences page", () => {
  it("groups searched folders by origin and keeps the non-user ones read-only", async () => {
    await openFolders();
    expect(invokeMock).toHaveBeenCalledWith("local_content_summary");

    const standard = group("plugins", "standard");
    expect(within(standard).getByText("Standard locations")).toBeInTheDocument();
    expect(within(standard).getByText(/always searched and cannot be removed here/)).toBeInTheDocument();
    // A row reads as the folder's name with the folder that holds it beside it.
    const vst3 = within(standard).getByTitle("/Library/Audio/Plug-Ins/VST3");
    expect(within(vst3).getByText("VST3")).toBeInTheDocument();
    expect(within(vst3).getByText("/Library/Audio/Plug-Ins/")).toBeInTheDocument();
    expect(within(standard).queryByRole("button", { name: /Stop searching/ })).toBeNull();
    // A standard folder that does not exist is counted, not listed.
    expect(within(block("plugins")).queryByTitle("/Library/Audio/Plug-Ins/LV2")).toBeNull();
    expect(within(block("plugins")).getByText("Usual locations not on this computer: 1")).toBeInTheDocument();

    const host = group("plugins", "host_settings");
    expect(within(host).getByText("From another app's settings")).toBeInTheDocument();
    expect(within(host).getByText("Change them in that app.", { exact: false })).toBeInTheDocument();
    expect(within(host).queryByRole("button", { name: /Stop searching/ })).toBeNull();

    const user = group("plugins", "user");
    expect(within(user).getByTitle("/Volumes/Studio/Plugins")).toBeInTheDocument();
    expect(within(user).getByRole("button", { name: "Stop searching /Volumes/Studio/Plugins" })).toBeEnabled();

    expect(within(group("sounds", "standard")).queryByRole("button", { name: /Stop searching/ })).toBeNull();
    expect(within(group("sounds", "user")).getByText("Instruments 2 · Samples 5")).toBeInTheDocument();

    const totals = document.querySelector(".preferences-folders-totals")!;
    expect(totals.textContent).toContain("Plugins 7");
    expect(totals.textContent).toContain("Instruments 3");
    expect(totals.textContent).toContain("Samples 10");
  });

  it("adds a plugin folder from the picker and scans for what it holds", async () => {
    openDialogMock.mockResolvedValueOnce("/Volumes/More/Plugins");
    const added = summary({
      plugins: [...summary().plugins.folders, { path: "/Volumes/More/Plugins", origin: "user", exists: true, count: 3 }],
      pluginTotal: 10,
    });
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "plugin_folder_add") {
        current = added;
        return added;
      }
      return original(command, ...args);
    });
    await openFolders();

    fireEvent.click(within(block("plugins")).getByRole("button", { name: "Add plugin folder…" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_folder_add", { path: "/Volumes/More/Plugins" }),
    );
    expect(await within(group("plugins", "user")).findByTitle("/Volumes/More/Plugins")).toBeInTheDocument();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_content_scan", { rescan: false }));
    await waitFor(() => expect(document.querySelector(".preferences-folders-totals")!.textContent).toContain("Plugins 10"));
  });

  it("removes a user sound folder by its library id", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "sound_folder_remove") {
        current = summary({ sounds: [summary().instruments.folders[0]] });
        return current;
      }
      return original(command, ...args);
    });
    await openFolders();

    fireEvent.click(within(block("sounds")).getByRole("button", { name: "Stop searching /Volumes/Studio/Fonts" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sound_folder_remove", { id: "lib-fonts" }));
    await waitFor(() => expect(within(block("sounds")).queryByTitle("/Volumes/Studio/Fonts")).toBeNull());
    expect(within(group("sounds", "user")).getByText("You have not added any folders.")).toBeInTheDocument();
  });

  it("words a refusal from the backend's code and leaves the list as it was", async () => {
    openDialogMock.mockResolvedValueOnce("/Volumes/Eleventh");
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "plugin_folder_add") {
        throw { code: "too_many", message: "at most 32 plugin folders can be added" };
      }
      if (command === "plugin_folder_remove") throw "the helper went away";
      return original(command, ...args);
    });
    await openFolders();

    fireEvent.click(within(block("plugins")).getByRole("button", { name: "Add plugin folder…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not add the folder: Your list of plugin folders is full. Remove one first.",
    );
    expect(screen.getByRole("alert")).not.toHaveTextContent("at most 32");
    expect(within(group("plugins", "user")).getByTitle("/Volumes/Studio/Plugins")).toBeInTheDocument();
    expect(invokeMock.mock.calls.some(([command]) => command === "local_content_scan")).toBe(false);

    // A failure without a code keeps the detail it has.
    fireEvent.click(within(block("plugins")).getByRole("button", { name: "Stop searching /Volumes/Studio/Plugins" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("Could not remove the folder: the helper went away"),
    );
    expect(within(group("plugins", "user")).getByTitle("/Volumes/Studio/Plugins")).toBeInTheDocument();
  });

  it("narrows the list to one kind with the tabs, each counting its folders", async () => {
    await openFolders();
    const tabs = within(screen.getByRole("tablist", { name: "Folder kinds" }));
    const all = tabs.getByRole("tab", { name: "All 5" });
    expect(all).toHaveAttribute("aria-selected", "true");
    expect(block("plugins")).toBeInTheDocument();
    expect(block("sounds")).toBeInTheDocument();

    fireEvent.click(tabs.getByRole("tab", { name: "Sounds 2" }));
    expect(document.querySelector('.preferences-folders[data-kind="plugins"]')).toBeNull();
    expect(within(block("sounds")).getByRole("button", { name: "Add sound folder…" })).toBeInTheDocument();
    expect(all).toHaveAttribute("aria-selected", "false");

    fireEvent.click(tabs.getByRole("tab", { name: "Plugins 3" }));
    expect(document.querySelector('.preferences-folders[data-kind="sounds"]')).toBeNull();
    expect(block("plugins")).toBeInTheDocument();
  });

  it("moves between the tabs with the arrow keys and keeps one Tab stop", async () => {
    await openFolders();
    const list = screen.getByRole("tablist", { name: "Folder kinds" });
    const tab = (name: RegExp) => within(list).getByRole("tab", { name });
    const all = tab(/^All/);
    const plugins = tab(/^Plugins/);
    const sounds = tab(/^Sounds/);
    expect([all, plugins, sounds].map((entry) => entry.tabIndex)).toEqual([0, -1, -1]);
    expect(screen.getByRole("tabpanel")).toHaveAttribute("aria-labelledby", all.id);

    all.focus();
    fireEvent.keyDown(all, { key: "ArrowRight" });
    expect(plugins).toHaveFocus();
    expect(plugins).toHaveAttribute("aria-selected", "true");
    expect([all, plugins, sounds].map((entry) => entry.tabIndex)).toEqual([-1, 0, -1]);
    expect(document.querySelector('.preferences-folders[data-kind="sounds"]')).toBeNull();
    expect(screen.getByRole("tabpanel")).toHaveAttribute("aria-labelledby", plugins.id);

    fireEvent.keyDown(plugins, { key: "ArrowRight" });
    expect(sounds).toHaveFocus();
    fireEvent.keyDown(sounds, { key: "ArrowRight" });
    expect(all).toHaveFocus();
    fireEvent.keyDown(all, { key: "ArrowLeft" });
    expect(sounds).toHaveFocus();
    fireEvent.keyDown(sounds, { key: "Home" });
    expect(all).toHaveFocus();
    fireEvent.keyDown(all, { key: "End" });
    expect(sounds).toHaveAttribute("aria-selected", "true");

    // Other keys are left to the browser.
    fireEvent.keyDown(sounds, { key: "ArrowDown" });
    expect(sounds).toHaveAttribute("aria-selected", "true");
  });

  it("folds an origin group and keeps its title, count and reason in view", async () => {
    await openFolders();
    const head = within(group("plugins", "standard")).getByRole("button", { name: /Standard locations/ });
    expect(head).toHaveAttribute("aria-expanded", "true");
    expect(head).toHaveTextContent("1");

    fireEvent.click(head);
    expect(head).toHaveAttribute("aria-expanded", "false");
    expect(within(group("plugins", "standard")).queryByTitle("/Library/Audio/Plug-Ins/VST3")).toBeNull();
    expect(head).toHaveTextContent(/always searched and cannot be removed here/);
    // Folding one kind's group leaves the other kind's group of that origin open.
    expect(within(group("sounds", "standard")).getByTitle("/Users/me/Music/SoundFonts")).toBeInTheDocument();

    fireEvent.click(head);
    expect(within(group("plugins", "standard")).getByTitle("/Library/Audio/Plug-Ins/VST3")).toBeInTheDocument();
  });

  it("shows the selected folder's full path, source and contents, and reveals it", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "reveal_path") return undefined;
      return original(command, ...args);
    });
    await openFolders();
    expect(details()).toHaveTextContent("Select a folder to see its full path");
    expect(within(details()).queryByRole("button")).toBeNull();

    const pick = screen.getByTitle("/Volumes/Studio/Fonts");
    fireEvent.click(pick);
    expect(pick).toHaveAttribute("aria-pressed", "true");
    expect(row("/Volumes/Studio/Fonts")).toHaveClass("selected");
    expect(within(details()).getByText("Fonts")).toBeInTheDocument();
    expect(within(details()).getByText("/Volumes/Studio/Fonts")).toBeInTheDocument();
    expect(within(details()).getByText("Added by you")).toBeInTheDocument();
    expect(within(details()).getByText("Remove a folder to stop searching it. Its files stay on disk.")).toBeInTheDocument();
    expect(details()).toHaveTextContent("Instruments 2 · Samples 5");

    fireEvent.click(within(details()).getByRole("button", { name: "Reveal in file manager" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("reveal_path", { path: "/Volumes/Studio/Fonts" }));

    // A second click clears the selection; a tab that hides the row does too.
    fireEvent.click(pick);
    expect(details()).toHaveTextContent("Select a folder");
    fireEvent.click(pick);
    fireEvent.click(screen.getByRole("tab", { name: /^Plugins/ }));
    expect(details()).toHaveTextContent("Select a folder");
  });

  it("words a failed reveal and keeps the selection", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "reveal_path") throw "file not found: /Volumes/Studio/Plugins";
      return original(command, ...args);
    });
    await openFolders();
    fireEvent.click(screen.getByTitle("/Volumes/Studio/Plugins"));
    fireEvent.click(within(details()).getByRole("button", { name: "Reveal in file manager" }));
    expect(await within(details()).findByRole("alert")).toHaveTextContent(
      "Could not show the folder: file not found: /Volumes/Studio/Plugins",
    );
    expect(within(details()).getByText("/Volumes/Studio/Plugins")).toBeInTheDocument();
  });

  it("words the refusal of a folder with neither samples nor instruments", async () => {
    openDialogMock.mockResolvedValueOnce("/Users/me/Documents");
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "sound_folder_add") {
        throw { code: "no_sounds", message: "no audio samples or instrument files found under '/Users/me/Documents'" };
      }
      return original(command, ...args);
    });
    await openFolders();
    fireEvent.click(within(block("sounds")).getByRole("button", { name: "Add sound folder…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not add the folder: That folder has no audio samples and no SoundFont, SFZ or sampler files.",
    );
  });

  it("keeps a folder removed during a scan removed when the scan returns", async () => {
    let finish!: (value: LocalContentSummary) => void;
    const started = summary();
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "local_content_scan") return new Promise((resolve) => { finish = resolve; });
      if (command === "sound_folder_remove") {
        current = summary({ sounds: [started.instruments.folders[0]], pluginTotal: 7 });
        return Promise.resolve(current);
      }
      return original(command, ...args);
    });
    await openFolders();

    fireEvent.click(screen.getByRole("button", { name: "Scan again" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_content_scan", { rescan: true }));
    fireEvent.click(within(block("sounds")).getByRole("button", { name: "Stop searching /Volumes/Studio/Fonts" }));
    await waitFor(() => expect(within(block("sounds")).queryByTitle("/Volumes/Studio/Fonts")).toBeNull());

    // The scan started before the removal, so its answer still lists the folder.
    const recounts = invokeMock.mock.calls.filter(([command]) => command === "local_content_summary").length;
    current = summary({ sounds: [started.instruments.folders[0]], pluginTotal: 12 });
    finish({ ...started, plugins: { ...started.plugins, total: 12 } });
    await waitFor(() => expect(screen.getByRole("button", { name: "Scan again" })).toBeEnabled());
    expect(invokeMock.mock.calls.filter(([command]) => command === "local_content_summary")).toHaveLength(recounts + 1);
    expect(within(block("sounds")).queryByTitle("/Volumes/Studio/Fonts")).toBeNull();
    expect(within(group("sounds", "user")).getByText("You have not added any folders.")).toBeInTheDocument();
    // The recount carries the plugins the scan read.
    expect(document.querySelector(".preferences-folders-totals")!.textContent).toContain("Plugins 12");
  });

  it("rescans every plugin and shows the scan's progress while it runs", async () => {
    let emit!: (payload: LocalScanProgress) => void;
    listenMock.mockImplementation(async (event: string, handler: (event: { payload: LocalScanProgress }) => void) => {
      if (event === "local_content_scan_progress") emit = (payload) => handler({ payload });
      return () => {};
    });
    let finish!: (value: LocalContentSummary) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) =>
      command === "local_content_scan"
        ? new Promise((resolve) => { finish = resolve; })
        : original(command, ...args));
    await openFolders();

    const again = screen.getByRole("button", { name: "Scan again" });
    fireEvent.click(again);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_content_scan", { rescan: true }));
    await waitFor(() => expect(emit).toBeDefined());
    expect(again).toBeDisabled();

    emit({ phase: "plugins", done: 3, total: 12, current: "Synth.vst3" });
    await waitFor(() =>
      expect(screen.getByRole("progressbar", { name: "Plugins" })).toHaveAttribute("aria-valuenow", "25"),
    );
    expect(screen.getByText("Reading Synth.vst3 · 3 of 12")).toBeInTheDocument();

    finish(summary({ pluginTotal: 9 }));
    await waitFor(() => expect(screen.queryByRole("progressbar")).toBeNull());
    expect(again).toBeEnabled();
    expect(document.querySelector(".preferences-folders-totals")!.textContent).toContain("Plugins 9");
  });
});

describe("splitPath", () => {
  it("splits on either separator and ignores a trailing one", () => {
    expect(splitPath("/Library/Audio/Plug-Ins/VST3")).toEqual({ name: "VST3", parent: "/Library/Audio/Plug-Ins/" });
    expect(splitPath("D:\\Audio\\VST3\\")).toEqual({ name: "VST3", parent: "D:\\Audio\\" });
    expect(splitPath("/Volumes")).toEqual({ name: "Volumes", parent: "/" });
    expect(splitPath("/")).toEqual({ name: "/", parent: "" });
    expect(splitPath("Samples")).toEqual({ name: "Samples", parent: "" });
  });
});

describe("reserveBelow", () => {
  it("keeps the page's scroll padding equal to the strip's height and clears it after", () => {
    let resized!: () => void;
    let observed: Element | undefined;
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: () => void) { resized = callback; }
      observe(target: Element) { observed = target; }
      disconnect = disconnect;
    });
    const page = document.createElement("div");
    page.className = "preferences-page";
    const strip = document.createElement("div");
    page.append(strip);
    let height = 96.4;
    strip.getBoundingClientRect = () => ({ height } as DOMRect);

    const release = reserveBelow(strip);
    expect(page.style.getPropertyValue("--folder-details-height")).toBe("97px");
    expect(observed).toBe(strip);
    height = 140;
    resized();
    expect(page.style.getPropertyValue("--folder-details-height")).toBe("140px");

    release();
    expect(disconnect).toHaveBeenCalled();
    expect(page.style.getPropertyValue("--folder-details-height")).toBe("");
    vi.unstubAllGlobals();
  });

  it("does nothing outside a preferences page", () => {
    const strip = document.createElement("div");
    expect(() => reserveBelow(strip)()).not.toThrow();
  });
});
