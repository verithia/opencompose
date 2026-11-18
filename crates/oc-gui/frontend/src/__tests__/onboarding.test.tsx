/**
 * First-run splash and guide: the scan page with its progress and results,
 * the suggestions shown only when little is found, the AI handoff, and the
 * once-per-machine marker.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { DEFAULT_PROVIDERS, aiConfig, loadConfig } from "../ai/config";
import { Onboarding, Splash } from "../components/Onboarding";
import { onboardingStore } from "../stores/onboarding";
import { preferencesUi } from "../stores/preferencesUi";
import { pluginStore } from "../stores/pluginStore";
import { localScan } from "../stores/localScan";
import { soundPacks } from "../stores/soundPacks";
import type {
  FolderOrigin,
  LocalContentSummary,
  LocalScanProgress,
  SoundPackItem,
  SoundPackProgress,
} from "../stores/api";
import { aiConfigWire, invokeMock, listenMock, openDialogMock } from "./setup";

const ENTRY = {
  id: "surge-synth-team.surge-xt",
  name: "Surge XT",
  vendor: "Surge Synth Team",
  version: "1.3.4",
  description: "A free hybrid synthesizer with a large modulation matrix.",
  license: "GPL-3.0",
  tags: ["synth", "open-source"],
  installs: [
    {
      kind: "direct",
      platform: "macos-aarch64",
      format: "vst3",
      url: "https://example.test/surge.zip",
      bytes: 120_000_000,
    },
  ],
  installable: true,
  installed: false,
};

const item = (name: string, format: string, detail = "", readable = true) => ({
  name,
  detail,
  format,
  path: `/plugins/${name}.${format}`,
  readable,
});

function pack(
  id: string,
  name: string,
  content: SoundPackItem["content"],
  overrides: Partial<SoundPackItem> = {},
): SoundPackItem {
  return {
    id,
    name,
    publisher: "FreePats",
    homepage: "https://example.test/publisher",
    version: "1.0.0",
    description: "A free starter pack.",
    content,
    archive: "zip",
    bytes: 32_319_396,
    unpacked_bytes: 120_000_000,
    license: { name: "CC0-1.0", url: "https://example.test/license", text: "" },
    installed: false,
    installed_version: null,
    path: null,
    source: "index",
    ...overrides,
  };
}

const GENERAL_USER = pack("christian-collins.generaluser-gs", "GeneralUser GS", "soundfont", {
  publisher: "S. Christian Collins",
  archive: "file",
  bytes: 32_319_396,
  unpacked_bytes: 32_319_396,
  license: { name: "GeneralUser GS License v2.0", url: "https://example.test/generaluser", text: "" },
});

const MULDJORD = pack("freepats.muldjordkit-sf2", "MuldjordKit", "soundfont", {
  archive: "7z",
  bytes: 54_877_940,
  unpacked_bytes: 69_000_000,
  license: { name: "CC-BY-4.0", url: "https://example.test/cc-by", text: "Drum samples by Mads Muldjord." },
});

const AKWF = pack("adventurekid.akwf", "AKWF waveforms", "samples", {
  publisher: "Adventure Kid",
  bytes: 6_007_440,
  unpacked_bytes: 6_007_440,
});

const PACKS: SoundPackItem[] = [GENERAL_USER, MULDJORD, AKWF];

/** A machine with little on it: the guide suggests plugins and sounds. */
const SUMMARY: LocalContentSummary = {
  plugins: {
    total: 3,
    formats: [{ format: "vst3", count: 2 }, { format: "clap", count: 1 }],
    folders: [
      { path: "/Library/Audio/Plug-Ins/VST3", origin: "standard", exists: true, count: 2 },
      { path: "/Users/me/Library/Application Support/REAPER/UserPlugins/FX", origin: "host_settings", exists: true, count: 1 },
      { path: "/Library/Audio/Plug-Ins/LV2", origin: "standard", exists: false, count: 0 },
    ],
    items: [item("Broken", "vst3", "", false), item("Echo", "clap", "Acme"), item("Synth", "vst3", "Acme")],
    truncated: false,
  },
  instruments: {
    total: 1,
    formats: [{ format: "sf2", count: 1 }],
    folders: [{ path: "/Users/me/Music/SoundFonts", origin: "standard", exists: true, count: 1 }],
    items: [{ name: "Piano", detail: "/Users/me/Music/SoundFonts", format: "sf2", path: "/Users/me/Music/SoundFonts/Piano.sf2", readable: true }],
    truncated: false,
  },
  samples: {
    total: 12,
    formats: [],
    folders: [{ path: "/Users/me/Music/SoundFonts", origin: "standard", exists: true, count: 12 }],
    items: [],
    truncated: false,
  },
  scanning: false,
};

/** A well-stocked machine: no suggestion page. */
const PLENTY: LocalContentSummary = {
  ...SUMMARY,
  plugins: {
    ...SUMMARY.plugins,
    total: 40,
    items: Array.from({ length: 40 }, (_, index) => item(`Plugin ${index}`, "vst3", "Acme")),
  },
  instruments: { ...SUMMARY.instruments, total: 20 },
  samples: { ...SUMMARY.samples, total: 5_000 },
};

let installed = false;
let summary: LocalContentSummary = SUMMARY;

/** Leave the scan page once its result is shown. */
async function openPicksStep() {
  await screen.findByText("Found on this computer");
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await screen.findByText("Recommended");
}

beforeEach(() => {
  installed = false;
  summary = SUMMARY;
  localScan.reset();
  onboardingStore.reset();
  preferencesUi.close();
  pluginStore.reset();
  soundPacks.reset();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "plugin_catalog") {
      return {
        platform: "macos-aarch64",
        entries: [{ ...ENTRY, installed }],
        installed: installed ? [{ name: "Surge XT", format: "vst3", path: "/plugins/Surge XT.vst3" }] : [],
        source: "cache",
      };
    }
    if (command === "plugin_index_status") return null;
    if (command === "local_content_scan" || command === "local_content_summary") return summary;
    if (command === "soundfonts_list") return { dir: "", items: [] };
    if (command === "sound_packs_list") return { dir: "/sounds", items: PACKS };
    if (command === "sound_pack_install") {
      const id = (args as { id: string }).id;
      return { id, name: id, version: "1.0.0", path: `/sounds/${id}`, bytes: 1, unpacked_bytes: 2, files: 1 };
    }
    if (command === "sound_pack_cancel") return undefined;
    if (command === "open_url") return undefined;
    if (command === "plugin_store_install") {
      installed = true;
      return { status: "installed", path: "/plugins/Surge XT.vst3", message: "Installed" };
    }
    return undefined;
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  invokeMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
  preferencesUi.close();
  pluginStore.reset();
  localScan.reset();
  soundPacks.reset();
});

describe("first-run onboarding", () => {
  it("shows the splash without any workspace behind it", () => {
    render(() => <Splash />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "Starting OpenCompose");
  });

  it("opens on a scan that reports progress for plugins, then sounds", async () => {
    let emit!: (payload: LocalScanProgress) => void;
    listenMock.mockImplementation(async (_event: string, handler: (event: { payload: LocalScanProgress }) => void) => {
      emit = (payload) => handler({ payload });
      return () => {};
    });
    let finish!: (value: LocalContentSummary) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) =>
      command === "local_content_scan" ? new Promise((resolve) => { finish = resolve; }) : original(command, ...args));

    render(() => <Onboarding onDone={() => {}} />);
    expect(screen.getByText("Looking for plugins and sounds")).toBeInTheDocument();
    expect(screen.getByText("Step 1 of 4")).toBeInTheDocument();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_content_scan", { rescan: false }));
    await waitFor(() => expect(emit).toBeDefined());

    emit({ phase: "plugins", done: 3, total: 12, current: "Kontakt.vst3" });
    const plugins = screen.getByRole("progressbar", { name: "Plugins" });
    await waitFor(() => expect(plugins).toHaveAttribute("aria-valuenow", "25"));
    expect(screen.getByText("Reading Kontakt.vst3 · 3 of 12")).toBeInTheDocument();
    expect(screen.getByText("Starts after plugins")).toBeInTheDocument();

    emit({ phase: "sounds", done: 1, total: 4, current: "/Users/me/Music/SoundFonts" });
    await screen.findByText("Searching /Users/me/Music/SoundFonts · 2 of 4");
    expect(screen.getByRole("progressbar", { name: "Sounds" })).toHaveAttribute("aria-valuenow", "25");

    finish(SUMMARY);
    expect(await screen.findByText("Found on this computer")).toBeInTheDocument();
    // The finished scan gives way to its result.
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("Plugins (3)")).toBeInTheDocument();
  });

  it("shows counts and folded lists of what was found", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");

    expect(screen.getByText("VST3 2 · CLAP 1")).toBeInTheDocument();
    expect(screen.getByText("SF2 1")).toBeInTheDocument();
    const pluginList = screen.getByText("Plugins (3)").closest("details")!;
    expect(pluginList).not.toHaveAttribute("open");
    expect(within(pluginList).getByText("Synth")).toBeInTheDocument();
    const broken = within(pluginList).getByText("Broken").closest("li")!;
    expect(broken).toHaveClass("unreadable");
    expect(within(broken).getByText("Could not be read")).toBeInTheDocument();
    expect(screen.getByText("Instruments (1)").closest("details")).toBeInTheDocument();

    // A standard folder that does not exist is searched but not listed.
    expect(screen.getByText("Folders searched (3)")).toBeInTheDocument();
    expect(screen.queryByText("/Library/Audio/Plug-Ins/LV2")).toBeNull();
    expect(screen.getByText("Usual locations not on this computer: 1")).toBeInTheDocument();
    expect(screen.getByText("From another app")).toBeInTheDocument();
    expect(screen.getByText("Instruments 1 · Samples 12")).toBeInTheDocument();
    expect(screen.getByText(/Add the folder where your plugins or sounds are kept/)).toBeInTheDocument();
  });

  it("labels the folder OpenCompose installed and ignores an unknown origin", async () => {
    summary = {
      ...SUMMARY,
      instruments: {
        ...SUMMARY.instruments,
        folders: [
          ...SUMMARY.instruments.folders,
          {
            path: "/Users/me/Library/Application Support/OpenCompose/Sounds",
            origin: "managed",
            exists: true,
            count: 2,
          },
          { path: "/future/place", origin: "future" as FolderOrigin, exists: true, count: 1 },
        ],
      },
    };
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");

    expect(screen.getByText("Installed by OpenCompose")).toBeInTheDocument();
    expect(screen.getByText("/future/place")).toBeInTheDocument();
  });

  it("adds a plugin folder the user picks, then scans again", async () => {
    openDialogMock.mockResolvedValueOnce("/Volumes/Studio/Plugins");
    const added: LocalContentSummary = {
      ...SUMMARY,
      plugins: {
        ...SUMMARY.plugins,
        total: 5,
        formats: [...SUMMARY.plugins.formats, { format: "lv2", count: 2 }],
        folders: [
          ...SUMMARY.plugins.folders,
          { path: "/Volumes/Studio/Plugins", origin: "user", exists: true, count: 2 },
        ],
      },
    };
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "plugin_folder_add") {
        summary = added;
        return added;
      }
      if (command === "plugin_folder_remove") {
        summary = SUMMARY;
        return SUMMARY;
      }
      return original(command, ...args);
    });
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");

    fireEvent.click(screen.getByRole("button", { name: "Add plugin folder…" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_folder_add", { path: "/Volumes/Studio/Plugins" }),
    );
    expect(await screen.findByText("VST3 2 · CLAP 1 · LV2 2")).toBeInTheDocument();
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([command]) => command === "local_content_scan")).toHaveLength(2),
    );
    expect(screen.getByText("Added")).toBeInTheDocument();

    const remove = await screen.findByRole("button", { name: "Stop searching /Volumes/Studio/Plugins" });
    await waitFor(() => expect(remove).toBeEnabled());
    fireEvent.click(remove);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_folder_remove", { path: "/Volumes/Studio/Plugins" }),
    );
    expect(await screen.findByText("VST3 2 · CLAP 1")).toBeInTheDocument();
  });

  it("reports a sound folder the backend refuses and keeps the counts", async () => {
    openDialogMock.mockResolvedValueOnce("/Users/me/Empty");
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "sound_folder_add") throw "no instrument files found under '/Users/me/Empty'";
      return original(command, ...args);
    });
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");

    fireEvent.click(screen.getByRole("button", { name: "Add sound folder…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not add the folder: no instrument files found",
    );
    expect(screen.getByText("SF2 1")).toBeInTheDocument();
  });

  it("does nothing when the folder picker is cancelled", async () => {
    openDialogMock.mockResolvedValueOnce(null);
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Add plugin folder…" }));
    await waitFor(() => expect(openDialogMock).toHaveBeenCalled());
    expect(invokeMock.mock.calls.some(([command]) => command === "plugin_folder_add")).toBe(false);
  });

  it("rescans every plugin on request", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Scan again" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_content_scan", { rescan: true }));
  });

  it("suggests free plugins and starter sounds when little is found", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();

    expect(screen.getByText("Only a few plugins or sounds were found on this computer. These free ones are a good start.")).toBeInTheDocument();
    expect(await screen.findByText("Surge XT")).toBeInTheDocument();
    expect(screen.getByText("Starter sounds")).toBeInTheDocument();
    // Instruments are few (1) and samples are few (12): both kinds are offered.
    expect(screen.getByText("GeneralUser GS")).toBeInTheDocument();
    expect(screen.getByText("AKWF waveforms")).toBeInTheDocument();
    expect(screen.getByText("52 MB, 66 MB unpacked")).toBeInTheDocument();
    expect(screen.getByText("Drum samples by Mads Muldjord.")).toBeInTheDocument();

    const card = screen.getByText("GeneralUser GS").closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Read the GeneralUser GS License v2.0 license" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("open_url", { url: "https://example.test/generaluser" }),
    );
  });

  it("shows loading, not an empty catalog, before the first answer", async () => {
    let finishCatalog!: (catalog: unknown) => void;
    let finishRefresh!: (outcome: unknown) => void;
    const original = invokeMock.getMockImplementation()!;
    const status = {
      source: "none",
      generated_at: null,
      fetched_at: null,
      stale_days: null,
      stale_warning: false,
      last_error: null,
      catalog_version: null,
      min_app_version: null,
      next_refresh_in_seconds: 0,
    };
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "plugin_catalog") {
        return new Promise((resolve) => { finishCatalog = resolve; });
      }
      if (command === "plugin_index_status") return Promise.resolve(status);
      if (command === "plugin_index_refresh") {
        return new Promise((resolve) => { finishRefresh = resolve; });
      }
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Reading the plugin index…")).toBeInTheDocument();
    expect(screen.queryByText(/This index has no recommendations/)).toBeNull();
    expect(
      screen.queryByText("Connect to the internet to see free plugin and sound suggestions."),
    ).toBeNull();

    finishCatalog({ platform: "macos-aarch64", entries: [], installed: [], source: "none" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("plugin_index_refresh", { force: false }));
    finishRefresh({ performed: false, changed: false, status });
  });

  it("fetches the index once on a fresh install and shows loading until it answers", async () => {
    let catalogDownloaded = false;
    let refreshes = 0;
    let finishRefresh!: (outcome: unknown) => void;
    const original = invokeMock.getMockImplementation()!;
    const status = (source: "none" | "cache") => ({
      source,
      generated_at: null,
      fetched_at: null,
      stale_days: null,
      stale_warning: false,
      last_error: null,
      catalog_version: null,
      min_app_version: null,
      next_refresh_in_seconds: 0,
    });
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "plugin_catalog") {
        return Promise.resolve({
          platform: "macos-aarch64",
          entries: catalogDownloaded ? [{ ...ENTRY, installed }] : [],
          installed: [],
          source: catalogDownloaded ? "cache" : "none",
        });
      }
      if (command === "plugin_index_status") return Promise.resolve(status(catalogDownloaded ? "cache" : "none"));
      if (command === "plugin_index_refresh") {
        refreshes += 1;
        return new Promise((resolve) => {
          finishRefresh = (outcome) => {
            catalogDownloaded = true;
            resolve(outcome);
          };
        });
      }
      if (command === "sound_packs_list") {
        return Promise.resolve({ dir: "/sounds", items: catalogDownloaded ? PACKS : [] });
      }
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(await screen.findByText("Reading the plugin index…")).toBeInTheDocument();
    expect(
      screen.queryByText("Connect to the internet to see free plugin and sound suggestions."),
    ).toBeNull();
    expect(screen.queryByText(/This index has no recommendations/)).toBeNull();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("plugin_index_refresh", { force: false }));
    expect(refreshes).toBe(1);

    finishRefresh({ performed: true, changed: true, status: status("cache") });
    expect(await screen.findByText("Surge XT")).toBeInTheDocument();
    expect(screen.queryByText("Reading the plugin index…")).toBeNull();
  });

  it("shows the automatic refresh error and retries on the manual path", async () => {
    let catalogDownloaded = false;
    const refreshes: boolean[] = [];
    const original = invokeMock.getMockImplementation()!;
    const status = (source: "none" | "cache") => ({
      source,
      generated_at: null,
      fetched_at: null,
      stale_days: null,
      stale_warning: false,
      last_error: null,
      catalog_version: null,
      min_app_version: null,
      next_refresh_in_seconds: 0,
    });
    const rateLimit = {
      stage: "policy",
      message: "Plugin catalog refresh is limited to once every 5 minutes; try again in 120 seconds",
    };
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "plugin_catalog") {
        return {
          platform: "macos-aarch64",
          entries: catalogDownloaded ? [{ ...ENTRY, installed }] : [],
          installed: [],
          source: catalogDownloaded ? "cache" : "none",
        };
      }
      if (command === "plugin_index_status") return status(catalogDownloaded ? "cache" : "none");
      if (command === "plugin_index_refresh") {
        const force = (args[0] as { force?: boolean } | undefined)?.force ?? false;
        refreshes.push(force);
        if (!force) throw rateLimit;
        catalogDownloaded = true;
        return { performed: true, changed: true, status: status("cache") };
      }
      if (command === "sound_packs_list") {
        return { dir: "/sounds", items: catalogDownloaded ? PACKS : [] };
      }
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();

    expect(
      await screen.findByText("Connect to the internet to see free plugin and sound suggestions."),
    ).toBeInTheDocument();
    expect(await screen.findByText(/Catalog refresh failed:/)).toHaveTextContent("5 minutes");
    expect(screen.queryByText("Free plugins")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    await waitFor(() => expect(refreshes).toEqual([false, true]));
    expect(await screen.findByText("Surge XT")).toBeInTheDocument();
    expect(
      screen.queryByText("Connect to the internet to see free plugin and sound suggestions."),
    ).toBeNull();
  });

  it("installs a starter sound pack with one click and scans again", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const card = (await screen.findByText("GeneralUser GS")).closest("article")!;

    fireEvent.click(within(card).getByRole("button", { name: "Install" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sound_pack_install", {
        id: "christian-collins.generaluser-gs",
      }),
    );
    expect(await within(card).findByText("Installed")).toBeInTheDocument();
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([command]) => command === "local_content_scan").length).toBeGreaterThanOrEqual(2),
    );
  });

  it("shows download, verify and unpack progress while a starter pack installs", async () => {
    let emit!: (progress: SoundPackProgress) => void;
    listenMock.mockImplementation(async (event: string, handler: (payload: { payload: SoundPackProgress }) => void) => {
      if (event === "sound_pack_install_progress") {
        emit = (progress) => handler({ payload: progress });
      }
      return () => {};
    });
    let finish!: (value: unknown) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) =>
      command === "sound_pack_install"
        ? new Promise((resolve) => { finish = resolve; })
        : original(command, ...args));

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const card = (await screen.findByText("GeneralUser GS")).closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));

    const bar = await within(card).findByRole("progressbar");
    await waitFor(() => expect(emit).toBeDefined());
    emit({
      id: "christian-collins.generaluser-gs",
      phase: "download",
      done: 25,
      total: 100,
    });
    await waitFor(() => expect(bar).toHaveAttribute("aria-valuenow", "25"));
    expect(within(card).getByText("Downloading…")).toBeInTheDocument();

    emit({
      id: "christian-collins.generaluser-gs",
      phase: "verify",
      done: 100,
      total: 100,
    });
    await waitFor(() => expect(bar).toHaveAttribute("aria-valuenow", "100"));
    expect(within(card).getByText("Verifying…")).toBeInTheDocument();

    emit({
      id: "christian-collins.generaluser-gs",
      phase: "extract",
      done: 0,
      total: 0,
    });
    await waitFor(() => expect(within(card).getByText("Unpacking…")).toBeInTheDocument());
    expect(bar).not.toHaveAttribute("aria-valuenow");

    finish({ id: "christian-collins.generaluser-gs", name: "GeneralUser GS", version: "1.0.0", path: "/sounds/generaluser", bytes: 1, unpacked_bytes: 2, files: 1 });
    expect(await within(card).findByText("Installed")).toBeInTheDocument();
  });

  it("reports a failed starter install and offers it again", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "sound_pack_install") throw "Download failed";
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const card = (await screen.findByText("GeneralUser GS")).closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));

    expect(await within(card).findByRole("alert")).toHaveTextContent("Install failed: Download failed");
    expect(within(card).getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("cancels a starter install and reports it", async () => {
    let rejectInstall!: (reason: string) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "sound_pack_install") {
        return new Promise((_, reject) => { rejectInstall = reject; });
      }
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const card = (await screen.findByText("GeneralUser GS")).closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sound_pack_install", {
        id: "christian-collins.generaluser-gs",
      }),
    );
    fireEvent.click(await within(card).findByRole("button", { name: "Cancel" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sound_pack_cancel", {
        id: "christian-collins.generaluser-gs",
      }),
    );
    rejectInstall("cancelled");
    expect(await within(card).findByText("Install cancelled.")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("shows only the add-folder action when no starter packs are listed", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "sound_packs_list") throw "The signed catalog is unavailable";
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const section = (await screen.findByText("Starter sounds")).closest("section")!;

    expect(within(section).getByRole("button", { name: "Add sound folder…" })).toBeEnabled();
    expect(within(section).queryByRole("article")).toBeNull();
  });

  it("skips the suggestions on a well-stocked computer", async () => {
    summary = PLENTY;
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    expect(screen.getByText("Step 1 of 3")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("Shortcuts open three workspaces")).toBeInTheDocument();
  });

  it("explains the three bottom-panel keys after the suggestions", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    fireEvent.click(screen.getByRole("button", { name: "Install later" }));
    expect(screen.getByText("Shortcuts open three workspaces")).toBeInTheDocument();
    expect(screen.getByText("Step 3 of 4")).toBeInTheDocument();
    expect(screen.getByText("C")).toBeInTheDocument();
    expect(screen.getByText("P")).toBeInTheDocument();
    expect(screen.getByText("R")).toBeInTheDocument();
  });

  it("hands the model step to Preferences and finishes", async () => {
    const done = vi.fn();
    summary = PLENTY;
    render(() => <Onboarding onDone={done} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("Model settings")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Fill this in later in Preferences" }));
    expect(done).toHaveBeenCalledOnce();
    expect(preferencesUi.open()).toBe(false);
  });

  it("installs a recommended plugin from the catalog and reports it installed", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();

    const card = (await screen.findByText("Surge XT")).closest("article")!;
    expect(screen.getByText("GPL-3.0")).toBeInTheDocument();
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_store_install", {
        id: "surge-synth-team.surge-xt",
        overwrite: false,
      }),
    );
    await waitFor(() => expect(screen.getByText("Installed")).toBeInTheDocument());
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([command]) => command === "local_content_scan").length).toBeGreaterThanOrEqual(2),
    );
  });

  it("keeps navigation locked while a plugin install runs but lets Skip pass", async () => {
    let rejectInstall!: (reason: Error) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "plugin_store_install") {
        return new Promise((_, reject) => { rejectInstall = reject; });
      }
      return original(command, ...args);
    });
    const done = vi.fn();
    render(() => <Onboarding onDone={done} />);
    await openPicksStep();
    const card = (await screen.findByText("Surge XT")).closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));
    for (const name of ["Back", "Install later"]) {
      expect(screen.getByRole("button", { name })).toBeDisabled();
      fireEvent.click(screen.getByRole("button", { name }));
    }
    fireEvent.keyDown(document, { key: "Escape" });
    expect(done).not.toHaveBeenCalled();
    expect(screen.getByText("Recommended")).toBeInTheDocument();
    const skip = screen.getByRole("button", { name: "Skip setup" });
    expect(skip).toBeEnabled();
    fireEvent.click(skip);
    expect(done).toHaveBeenCalledOnce();

    rejectInstall(new Error("Download failed"));
    await screen.findByText(/Install failed/);
    expect(screen.getByRole("button", { name: "Install later" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();
  });

  it("cancels a plugin install and reports it", async () => {
    let rejectInstall!: (reason: string) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "plugin_store_install") {
        return new Promise((_, reject) => { rejectInstall = reject; });
      }
      return original(command, ...args);
    });

    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();
    const card = (await screen.findByText("Surge XT")).closest("article")!;
    fireEvent.click(within(card).getByRole("button", { name: "Install" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_store_install", {
        id: "surge-synth-team.surge-xt",
        overwrite: false,
      }),
    );
    fireEvent.click(await within(card).findByRole("button", { name: "Cancel" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("install_cancel", { id: "surge-synth-team.surge-xt" }),
    );
    rejectInstall("surge-synth-team.surge-xt was cancelled");
    expect(await within(card).findByText("Install cancelled.")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("lists the saved providers without testing credentials", async () => {
    // The guide lists what the user saved; the catalogue is not their list.
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) =>
      command === "load_ai_config" ? aiConfigWire([DEFAULT_PROVIDERS[0]]) : original(command, ...args));
    await loadConfig();
    const first = aiConfig.config.providers[0];
    summary = PLENTY;
    // Count only what the guide asks for, not the load that seeded the store.
    invokeMock.mockClear();
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText(first.name)).toBeVisible();
    expect(screen.getByText(first.model)).toBeVisible();
    expect(invokeMock.mock.calls.some(([command]) => String(command).startsWith("ai_"))).toBe(false);
  });

  it("asks for a provider instead of listing the catalogue when none is saved", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) =>
      command === "load_ai_config" ? aiConfigWire([]) : original(command, ...args));
    await loadConfig();
    summary = PLENTY;
    render(() => <Onboarding onDone={() => {}} />);
    await screen.findByText("Found on this computer");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("No provider yet — add one in AI settings.")).toBeVisible();
    expect(screen.queryByRole("option")).toBeNull();
  });

  it("finishes through the skip control and records the marker", async () => {
    const done = vi.fn();
    render(() => <Onboarding onDone={done} />);
    fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    expect(done).toHaveBeenCalledOnce();
    onboardingStore.complete();
    expect(onboardingStore.pending()).toBe(false);
  });

  it("offers an index sound pack beside the starter sounds", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, ...args: unknown[]) => {
      if (command === "soundfonts_list") {
        return {
          dir: "",
          items: [
            {
              id: "upright-piano",
              name: "Upright Piano",
              version: "1.0.0",
              description: "A small sampled upright piano.",
              bytes: 12 * 1024 * 1024,
              license: { name: "CC0-1.0", url: "", text: "" },
              installed: false,
              source: "index",
            },
          ],
        };
      }
      if (command === "soundfont_install") return { id: "upright-piano" };
      return original(command, ...args);
    });
    render(() => <Onboarding onDone={() => {}} />);
    await openPicksStep();

    expect(await screen.findByText("Upright Piano")).toBeInTheDocument();
    expect(screen.getByText("Sound pack")).toBeInTheDocument();
    const card = screen.getByText("Upright Piano").closest("article")!;
    fireEvent.click(card.querySelector("button")!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("soundfont_install", { id: "upright-piano" }),
    );
  });
});
