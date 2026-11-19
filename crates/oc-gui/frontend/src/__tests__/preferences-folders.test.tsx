/**
 * The Folders page: the searched plugin and sound folders grouped by origin,
 * adding and removing the user's own, refusals worded from the backend's code,
 * and a rescan with the first-run scan's progress.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { PreferencesModal } from "../components/PreferencesModal";
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

describe("the Folders preferences page", () => {
  it("groups searched folders by origin and keeps the non-user ones read-only", async () => {
    await openFolders();
    expect(invokeMock).toHaveBeenCalledWith("local_content_summary");

    const standard = group("plugins", "standard");
    expect(within(standard).getByText("Standard locations")).toBeInTheDocument();
    expect(within(standard).getByText(/always searched and cannot be removed here/)).toBeInTheDocument();
    expect(within(standard).getByText("/Library/Audio/Plug-Ins/VST3")).toBeInTheDocument();
    expect(within(standard).queryByRole("button")).toBeNull();
    // A standard folder that does not exist is counted, not listed.
    expect(within(block("plugins")).queryByText("/Library/Audio/Plug-Ins/LV2")).toBeNull();
    expect(within(block("plugins")).getByText("Usual locations not on this computer: 1")).toBeInTheDocument();

    const host = group("plugins", "host_settings");
    expect(within(host).getByText("From another app's settings")).toBeInTheDocument();
    expect(within(host).getByText("Change them in that app.", { exact: false })).toBeInTheDocument();
    expect(within(host).queryByRole("button")).toBeNull();

    const user = group("plugins", "user");
    expect(within(user).getByText("/Volumes/Studio/Plugins")).toBeInTheDocument();
    expect(within(user).getByRole("button", { name: "Stop searching /Volumes/Studio/Plugins" })).toBeEnabled();

    expect(within(group("sounds", "standard")).queryByRole("button")).toBeNull();
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
    expect(await within(group("plugins", "user")).findByText("/Volumes/More/Plugins")).toBeInTheDocument();
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
    await waitFor(() => expect(within(block("sounds")).queryByText("/Volumes/Studio/Fonts")).toBeNull());
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
    expect(within(group("plugins", "user")).getByText("/Volumes/Studio/Plugins")).toBeInTheDocument();
    expect(invokeMock.mock.calls.some(([command]) => command === "local_content_scan")).toBe(false);

    // A failure without a code keeps the detail it has.
    fireEvent.click(within(block("plugins")).getByRole("button", { name: "Stop searching /Volumes/Studio/Plugins" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("Could not remove the folder: the helper went away"),
    );
    expect(within(group("plugins", "user")).getByText("/Volumes/Studio/Plugins")).toBeInTheDocument();
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
