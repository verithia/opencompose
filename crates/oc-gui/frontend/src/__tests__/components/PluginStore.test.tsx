/**
 * Plugin Store catalog rendering, filtering, installed badges, and actions.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { PluginStore } from "../../components/PluginStore";
import { StoreBrowser } from "../../components/StoreBrowser";
import { pluginStore } from "../../stores/pluginStore";
import type { PluginCatalog, PluginCatalogEntry } from "../../stores/api";
import { askDialogMock, expectInvoked, getLastCallArgs, invokeMock } from "../setup";

const HOMEPAGE = "https://asb2m10.github.io/dexed/";
const ZIP_URL =
  "https://github.com/asb2m10/dexed/releases/download/v1.0.1/Dexed-1.0.1-macOS.zip";

const dexed: PluginCatalogEntry = {
  id: "asb2m10.dexed",
  name: "Dexed",
  vendor: "asb2m10",
  tags: ["synth", "open-source"],
  homepage: HOMEPAGE,
  description: "A multi platform FM plugin synth.",
  installs: [
    {
      kind: "github_release",
      platform: "macos-aarch64",
      format: "vst3",
      archive: "zip",
      url: ZIP_URL,
    },
    {
      kind: "vendor_page",
      platform: "macos-aarch64",
      format: "clap",
      url: HOMEPAGE,
    },
  ],
};

const talReverb: PluginCatalogEntry = {
  id: "tal-software.tal-reverb-4",
  name: "TAL-Reverb-4",
  vendor: "TAL Software",
  tags: ["reverb", "freeware"],
  homepage: "https://tal-software.com/products/tal-reverb-4",
  installs: [
    {
      kind: "vendor_page",
      platform: "macos-aarch64",
      format: "vst3",
      url: "https://tal-software.com/products/tal-reverb-4",
    },
  ],
};

const cardinal: PluginCatalogEntry = {
  id: "distrho.cardinal",
  name: "Cardinal",
  vendor: "DISTRHO",
  tags: ["synth", "open-source"],
  homepage: "https://github.com/DISTRHO/Cardinal",
  needs_admin: true,
  installs: [
    {
      kind: "github_release",
      platform: "macos-aarch64",
      format: "vst3",
      archive: "pkg",
      url: "https://github.com/DISTRHO/Cardinal/releases/download/26.02/Cardinal-macOS-universal-26.02.pkg",
    },
  ],
};

const windowsOnly: PluginCatalogEntry = {
  id: "gvst.gsnap",
  name: "GSnap",
  vendor: "GVST",
  tags: ["vocal", "freeware"],
  homepage: "https://gvst.uk/Downloads/GSnap",
  installs: [
    {
      kind: "vendor_page",
      platform: "windows-x86_64",
      format: "vst3",
      url: "https://gvst.uk/Downloads/GSnap",
    },
  ],
};

const intelOnly: PluginCatalogEntry = {
  ...talReverb,
  id: "examples.intel-only",
  name: "Intel Only",
  installs: [
    {
      kind: "vendor_page",
      platform: "macos-x86_64",
      format: "vst3",
      url: "https://example.com/intel",
    },
  ],
};

const catalog: PluginCatalog = {
  platform: "macos-aarch64",
  entries: [dexed, talReverb, cardinal, windowsOnly, intelOnly],
  installed: [{ name: "Dexed", format: "vst3", path: "/Users/test/VST3/Dexed.vst3" }],
  source: "bundled",
};

const bundledStatus = {
  source: "bundled" as const,
  generated_at: null,
  fetched_at: null,
  stale_days: null,
  stale_warning: false,
  last_error: null,
  catalog_version: null,
  min_app_version: null,
  next_refresh_in_seconds: 0,
};

function mockCatalog(overrides?: Partial<PluginCatalog>) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "plugin_catalog") return { ...catalog, ...overrides };
    if (cmd === "plugin_index_status") return bundledStatus;
    if (cmd === "plugin_index_refresh") {
      return { performed: true, changed: false, status: bundledStatus };
    }
    if (cmd === "plugin_store_open") return undefined;
    if (cmd === "plugin_manual_page") return "https://tal-software.com/products/tal-reverb-4";
    if (cmd === "plugin_installed_check") {
      return { id: "", name: "", installed: false, matched: null, scanned: 0 };
    }
    if (cmd === "plugin_store_install") {
      return { status: "installed", path: "/tmp/Dexed.vst3", message: "Installed Dexed" };
    }
    return {};
  });
}

async function renderPluginStore() {
  const view = render(() => <PluginStore />);
  await waitFor(() => expect(view.container.querySelectorAll(".plugin-row").length).toBe(3));
  return view;
}

describe("PluginStore", () => {
  beforeEach(() => {
    pluginStore.reset();
    askDialogMock.mockReset();
    askDialogMock.mockResolvedValue(true);
    mockCatalog();
  });

  it("paints a loading skeleton while the catalog is unresolved", async () => {
    // Opening the surface must not wait for the walk over the plugin folders.
    invokeMock.mockImplementation((command: string) =>
      command === "plugin_catalog" ? new Promise(() => {}) : Promise.resolve({}),
    );
    const { container } = render(() => <PluginStore />);
    await waitFor(() => expect(container.querySelector(".plugin-store-skeleton")).not.toBeNull());
    const skeleton = container.querySelector(".plugin-store-skeleton")!;
    expect(skeleton.getAttribute("aria-busy")).toBe("true");
    expect(skeleton.textContent).toContain("Loading catalog...");
    expect(container.querySelectorAll(".plugin-row-skeleton")).toHaveLength(5);
    expect(container.querySelectorAll(".plugin-row:not(.plugin-row-skeleton)")).toHaveLength(0);
  });

  it("asks the index once when the browser opens", async () => {
    const refreshCalls = () =>
      invokeMock.mock.calls.filter(([command]) => command === "plugin_index_refresh");
    const before = refreshCalls().length;
    const { container } = await renderPluginStore();
    // One non-forced check per open: the backend answers from the cache while
    // the last check is inside the freshness interval.
    await waitFor(() => expect(refreshCalls()).toHaveLength(before + 1));
    expectInvoked("plugin_index_refresh", { force: false });
    expect(container.querySelectorAll(".plugin-row")).toHaveLength(3);
  });

  it("keeps the catalog across mounts and checks the index once per open", async () => {
    const catalogCalls = () =>
      invokeMock.mock.calls.filter(([command]) => command === "plugin_catalog").length;
    const refreshCalls = () =>
      invokeMock.mock.calls.filter(([command]) => command === "plugin_index_refresh").length;
    const before = catalogCalls();
    const checks = refreshCalls();
    const first = await renderPluginStore();
    expect(catalogCalls() - before).toBe(1);
    await waitFor(() => expect(refreshCalls()).toBe(checks + 1));
    first.unmount();
    // The second mount paints the rows from memory; the open still asks the
    // index for one check, and an unchanged catalog is not read again.
    const second = render(() => <PluginStore />);
    expect(second.container.querySelectorAll(".plugin-row")).toHaveLength(3);
    await waitFor(() => expect(refreshCalls()).toBe(checks + 2));
    expect(catalogCalls() - before).toBe(1);
  });

  it("renders only entries for the current platform with category and formats", async () => {
    const { container } = await renderPluginStore();
    expect(container.querySelector('[data-plugin-id="asb2m10.dexed"]')).toBeTruthy();
    expect(container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')).toBeTruthy();
    expect(container.querySelector('[data-plugin-id="distrho.cardinal"]')).toBeTruthy();
    expect(container.querySelector('[data-plugin-id="gvst.gsnap"]')).toBeNull();
    expect(container.querySelector('[data-plugin-id="examples.intel-only"]')).toBeNull();
    const dexedRow = container.querySelector('[data-plugin-id="asb2m10.dexed"]')!;
    expect(dexedRow.textContent).toContain("Dexed");
    expect(dexedRow.textContent).toContain("synth");
    expect(dexedRow.textContent).toContain("open-source");
    expect(dexedRow.textContent).toContain("vst3/clap");
    expect(container.textContent).toContain("2 hidden for this OS");
  });

  it("renders vst2 and lv2 entries with the catalog license and format labels", async () => {
    const dragonfly: PluginCatalogEntry = {
      id: "dragonfly-reverb.dragonfly-reverb",
      name: "Dragonfly Reverb",
      vendor: "Dragonfly Reverb",
      homepage: "https://michaelwillis.github.io/dragonfly-reverb/",
      license: "GPL-3.0",
      tags: ["reverb", "open-source"],
      description: "A family of algorithmic reverbs.",
      installs: [
        {
          kind: "github_release",
          platform: "macos-aarch64",
          format: "vst2",
          archive: "dmg",
          url: "https://github.com/michaelwillis/dragonfly-reverb/releases/download/3.2.10/dragonfly-reverb-3.2.10-macos-universal.dmg",
        },
        {
          kind: "github_release",
          platform: "macos-aarch64",
          format: "lv2",
          archive: "dmg",
          url: "https://github.com/michaelwillis/dragonfly-reverb/releases/download/3.2.10/dragonfly-reverb-3.2.10-macos-universal.dmg",
        },
      ],
    };
    mockCatalog({ entries: [dragonfly] });
    const view = render(() => <PluginStore />);
    await waitFor(() => expect(view.container.querySelectorAll(".plugin-row").length).toBe(1));
    const row = view.container.querySelector(
      '[data-plugin-id="dragonfly-reverb.dragonfly-reverb"]',
    )!;
    expect(row.textContent).toContain("GPL-3.0");
    expect(row.textContent).toContain("vst2/lv2");
    expect(
      row.querySelector<HTMLButtonElement>(".plugin-action.primary")!.disabled,
    ).toBe(false);

    fireEvent.input(view.container.querySelector(".plugin-store-search")!, {
      target: { value: "lv2" },
    });
    expect(view.container.querySelectorAll(".plugin-row")).toHaveLength(1);
    fireEvent.input(view.container.querySelector(".plugin-store-search")!, {
      target: { value: "aax" },
    });
    expect(view.container.querySelectorAll(".plugin-row")).toHaveLength(0);
  });

  it("filters by search and by category", async () => {
    const { container } = await renderPluginStore();
    fireEvent.input(container.querySelector(".plugin-store-search")!, {
      target: { value: "reverb" },
    });
    expect(container.querySelectorAll(".plugin-row")).toHaveLength(1);
    expect(container.textContent).toContain("TAL-Reverb-4");

    fireEvent.input(container.querySelector(".plugin-store-search")!, { target: { value: "" } });
    fireEvent.change(container.querySelector(".plugin-store-category")!, {
      target: { value: "synth" },
    });
    expect(container.querySelectorAll(".plugin-row")).toHaveLength(2);
    expect(container.textContent).not.toContain("TAL-Reverb-4");
  });

  it("shows an installed badge from scanned bundle names", async () => {
    const { container } = await renderPluginStore();
    const dexedRow = container.querySelector('[data-plugin-id="asb2m10.dexed"]')!;
    expect(dexedRow.querySelector(".plugin-badge.installed")).toBeTruthy();
    const talRow = container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')!;
    expect(talRow.querySelector(".plugin-badge.installed")).toBeNull();
  });

  it("offers a vendor download and a check only for page-only entries", async () => {
    const { container } = await renderPluginStore();
    const pageRow = container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')!;
    const vendor = pageRow.querySelector<HTMLButtonElement>(".plugin-vendor-download");
    expect(vendor).toBeTruthy();
    expect(vendor!.disabled).toBe(false);
    expect(vendor!.textContent).toContain("Download from vendor");
    const check = pageRow.querySelector<HTMLButtonElement>(".plugin-check-installed");
    expect(check).toBeTruthy();
    expect(check!.textContent).toContain("Check again");
    // Install stays disabled: the catalog pins no artifact for this entry.
    expect(pageRow.querySelector<HTMLButtonElement>(".plugin-action.primary")!.disabled).toBe(true);

    const directRow = container.querySelector('[data-plugin-id="asb2m10.dexed"]')!;
    expect(directRow.querySelector(".plugin-vendor-download")).toBeNull();
    expect(directRow.querySelector(".plugin-check-installed")).toBeNull();
    const directButtons = directRow.querySelectorAll<HTMLButtonElement>(".plugin-action");
    expect(directButtons[1].disabled).toBe(false);
    expect(directButtons[2].disabled).toBe(false);
  });

  it("opens the vendor page through plugin_manual_page by catalog id", async () => {
    const { container } = await renderPluginStore();
    const row = container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')!;
    fireEvent.click(row.querySelector(".plugin-vendor-download")!);
    await waitFor(() =>
      expectInvoked("plugin_manual_page", { id: "tal-software.tal-reverb-4" }),
    );
    expect(invokeMock.mock.calls.some(([command]) => command === "plugin_store_open")).toBe(false);
  });

  it("flips a page-only entry to installed after the check finds its bundle", async () => {
    let checked = false;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "plugin_installed_check") {
        checked = true;
        return { id: "tal-software.tal-reverb-4", name: "TAL-Reverb-4", installed: true, matched: "TAL-Reverb-4", scanned: 12 };
      }
      if (command === "plugin_catalog") {
        // The backend recomputes the flag after the walk, so the row carries it.
        return {
          ...catalog,
          entries: checked
            ? catalog.entries.map((entry) =>
                entry.id === "tal-software.tal-reverb-4" ? { ...entry, installed: true } : entry,
              )
            : catalog.entries,
        };
      }
      if (command === "plugin_index_status") return bundledStatus;
      return undefined;
    });
    const { container } = await renderPluginStore();
    const row = container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')!;
    expect(row.querySelector(".plugin-badge.installed")).toBeNull();
    fireEvent.click(row.querySelector(".plugin-check-installed")!);
    await waitFor(() => expectInvoked("plugin_installed_check", { id: "tal-software.tal-reverb-4" }));
    await waitFor(() =>
      expect(
        container
          .querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')
          ?.querySelector(".plugin-badge.installed"),
      ).toBeTruthy(),
    );
    expect(container.textContent).toContain("Found TAL-Reverb-4.");
    // An installed entry no longer offers the check.
    expect(
      container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"] .plugin-check-installed'),
    ).toBeNull();
    // The check never opens a download.
    expect(invokeMock.mock.calls.some(([command]) => command === "plugin_store_open")).toBe(false);
  });

  it("reports a page-only entry the vendor installer has not added yet", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "plugin_installed_check") {
        return { id: "tal-software.tal-reverb-4", name: "TAL-Reverb-4", installed: false, matched: null, scanned: 4 };
      }
      if (command === "plugin_catalog") return catalog;
      if (command === "plugin_index_status") return bundledStatus;
      return undefined;
    });
    const { container } = await renderPluginStore();
    const row = container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"]')!;
    fireEvent.click(row.querySelector(".plugin-check-installed")!);
    await waitFor(() =>
      expect(container.textContent).toContain("No TAL-Reverb-4 bundle yet."),
    );
    expect(
      container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"] .plugin-badge.installed'),
    ).toBeNull();
    expect(
      container.querySelector('[data-plugin-id="tal-software.tal-reverb-4"] .plugin-check-installed'),
    ).toBeTruthy();
  });

  it("opens the product page and direct download through the backend", async () => {
    const { container } = await renderPluginStore();
    const row = container.querySelector('[data-plugin-id="asb2m10.dexed"]')!;
    const buttons = row.querySelectorAll<HTMLButtonElement>(".plugin-action");
    fireEvent.click(buttons[0]);
    await waitFor(() => expectInvoked("plugin_store_open", { url: HOMEPAGE }));
    fireEvent.click(buttons[1]);
    await waitFor(() => expectInvoked("plugin_store_open", { url: ZIP_URL }));
  });

  it("installs a managed separator in one click", async () => {
    const runtime: PluginCatalogEntry = { ...dexed, id: "spleeter", name: "Spleeter", installs: [{
      kind: "direct", platform: "macos-aarch64", format: "standalone", archive: "tar-gz",
      url: "https://example.test/spleeter.tar.gz", sha256: "a".repeat(64), bytes: 1024,
      runtime: { provider: "spleeter", layout_version: 1, launcher: "bin/spleeter", required_files: [] },
    }] };
    mockCatalog({ entries: [runtime, ...catalog.entries.slice(1)] });
    const { container } = await renderPluginStore();
    const button = container.querySelector<HTMLButtonElement>('[data-plugin-id="spleeter"] .plugin-action.primary')!;
    expect(button.disabled).toBe(false);
    fireEvent.click(button);
    await waitFor(() => expectInvoked("plugin_store_install", { id: "spleeter", overwrite: false }));
    expect(askDialogMock).not.toHaveBeenCalled();
  });

  it("shows the installer outcome without repeating its rescan guidance", async () => {
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, args?: unknown) => command === "plugin_store_install"
      ? Promise.resolve({ status: "installed", path: "/tmp/Dexed.vst3", message: "Installed Dexed. Rescan the plugin browser to load it." })
      : original(command, args));
    const { container } = await renderPluginStore();
    fireEvent.click(container.querySelector('[data-plugin-id="asb2m10.dexed"] .plugin-action.primary')!);
    await waitFor(() => expect(container.textContent).toContain("Installed Dexed."));
    expect((container.querySelector('[data-plugin-id="asb2m10.dexed"]')?.textContent?.match(/Rescan/g) ?? [])).toHaveLength(1);
  });

  it("installs on click and asks before overwriting an existing bundle", async () => {
    let calls = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "plugin_catalog") return catalog;
      if (cmd === "plugin_store_install") {
        calls += 1;
        return calls === 1
          ? { status: "needs_confirmation", path: "/tmp/Dexed.vst3", message: "exists" }
          : { status: "installed", path: "/tmp/Dexed.vst3", message: "Installed Dexed" };
      }
      return undefined;
    });
    const { container } = await renderPluginStore();
    const installButton = container.querySelector<HTMLButtonElement>(
      '[data-plugin-id="asb2m10.dexed"] .plugin-action.primary',
    )!;
    fireEvent.click(installButton);
    await waitFor(() => expect(calls).toBe(2));
    expect(askDialogMock).toHaveBeenCalledTimes(1);
    expect(getLastCallArgs("plugin_store_install")).toEqual({
      id: "asb2m10.dexed",
      overwrite: true,
    });
    await waitFor(() => expect(container.textContent).toContain("Installed Dexed"));
  });

  it("shows the catalog source, stale warning, and refresh backoff", async () => {
    const fetchedAt = Math.floor(Date.now() / 1000) - 15 * 86_400;
    const status = {
      source: "cache" as const,
      generated_at: "2026-10-01T08:00:00Z",
      fetched_at: fetchedAt,
      stale_days: 15,
      stale_warning: true,
      last_error: null,
      catalog_version: "2026.10.01.1",
      min_app_version: "0.1.0",
      next_refresh_in_seconds: 240,
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "plugin_catalog") {
        return { ...catalog, source: "cache", generated_at: status.generated_at, fetched_at: fetchedAt, stale_days: 15 };
      }
      if (cmd === "plugin_index_status") return status;
      return undefined;
    });
    const { container } = await renderPluginStore();
    expect(container.textContent).toContain("From 2026-10-01 cache");
    expect(container.querySelector(".plugin-store-warning")?.textContent).toContain("15 days old");
    const refresh = container.querySelector<HTMLButtonElement>(".plugin-store-refresh")!;
    expect(refresh.disabled).toBe(true);
    expect(refresh.getAttribute("aria-label")).toContain("4 min");
  });

  it("refreshes through the backend and reports the new source", async () => {
    const fetchedAt = Math.floor(Date.now() / 1000);
    const status = {
      source: "remote" as const,
      generated_at: "2026-10-22T08:00:00Z",
      fetched_at: fetchedAt,
      stale_days: 0,
      stale_warning: false,
      last_error: null,
      catalog_version: "2026.10.22.1",
      min_app_version: "0.1.0",
      next_refresh_in_seconds: 300,
    };
    let refreshed = false;
    invokeMock.mockImplementation(async (cmd: string, args?: { force?: boolean }) => {
      if (cmd === "plugin_catalog") {
        return refreshed
          ? { ...catalog, source: "remote", generated_at: status.generated_at, fetched_at: fetchedAt }
          : { ...catalog, source: "cache", generated_at: "2026-10-01T08:00:00Z", fetched_at: fetchedAt - 1000 };
      }
      if (cmd === "plugin_index_status") {
        return refreshed
          ? status
          : { ...status, source: "cache", next_refresh_in_seconds: 0 };
      }
      if (cmd === "plugin_index_refresh") {
        // The open checks the index; only the explicit action forces one.
        if (!args?.force) {
          return {
            performed: false,
            changed: false,
            status: { ...status, source: "cache", next_refresh_in_seconds: 0 },
          };
        }
        refreshed = true;
        return { performed: true, changed: true, status };
      }
      return undefined;
    });
    const { container } = await renderPluginStore();
    const refresh = container.querySelector<HTMLButtonElement>(".plugin-store-refresh")!;
    expect(refresh.disabled).toBe(false);
    fireEvent.click(refresh);
    await waitFor(() => expectInvoked("plugin_index_refresh", { force: true }));
    await waitFor(() => expect(container.textContent).toContain("Refreshed 0 min ago"));
    expect(container.querySelector(".plugin-store-refresh")?.hasAttribute("disabled")).toBe(true);
  });

  it("reads the fetched age in whole minutes", async () => {
    const fetchedAt = Math.floor(Date.now() / 1000) - 30;
    const status = {
      source: "remote" as const,
      generated_at: "2026-10-22T08:00:00Z",
      fetched_at: fetchedAt,
      stale_days: 0,
      stale_warning: false,
      last_error: null,
      catalog_version: "2026.10.22.1",
      min_app_version: "0.1.0",
      next_refresh_in_seconds: 300,
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "plugin_catalog") {
        return { ...catalog, source: "remote", generated_at: status.generated_at, fetched_at: fetchedAt };
      }
      if (cmd === "plugin_index_status") return status;
      return undefined;
    });
    const { container } = await renderPluginStore();
    // 30 s is "0 min", not "0.5 min": the label floors before it divides.
    expect(container.textContent).toContain("Refreshed 0 min ago");
  });

  it("reads the fetched age in whole minutes past the first minute", async () => {
    const fetchedAt = Math.floor(Date.now() / 1000) - 90;
    const status = {
      source: "remote" as const,
      generated_at: "2026-10-22T08:00:00Z",
      fetched_at: fetchedAt,
      stale_days: 0,
      stale_warning: false,
      last_error: null,
      catalog_version: "2026.10.22.1",
      min_app_version: "0.1.0",
      next_refresh_in_seconds: 300,
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "plugin_catalog") {
        return { ...catalog, source: "remote", generated_at: status.generated_at, fetched_at: fetchedAt };
      }
      if (cmd === "plugin_index_status") return status;
      return undefined;
    });
    const { container } = await renderPluginStore();
    expect(container.textContent).toContain("Refreshed 1 min ago");
  });

  it("reports a failing refresh stage in the panel", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "plugin_catalog") return catalog;
      if (cmd === "plugin_index_status") return bundledStatus;
      if (cmd === "plugin_index_refresh") {
        throw { stage: "policy", message: "try again in 180 seconds" };
      }
      return undefined;
    });
    const { container } = await renderPluginStore();
    // Opening the browser asked for a check, so the panel names the stage; the
    // failure is a remark in the panel, not a background log line.
    await waitFor(() =>
      expect(container.textContent).toContain("policy: try again in 180 seconds"),
    );
  });

  it("shows an admin badge for installers that need approval", async () => {
    const { container } = await renderPluginStore();
    const adminRow = container.querySelector('[data-plugin-id="distrho.cardinal"]')!;
    const badge = adminRow.querySelector(".plugin-badge.admin");
    expect(badge).toBeTruthy();
    expect(badge?.getAttribute("aria-label")).toContain("administrator");
  });
});

describe("StoreBrowser Plugins tab", () => {
  beforeEach(() => {
    pluginStore.reset();
    mockCatalog();
  });

  it("switches from curated sites to the plugin catalog", async () => {
    const refreshCalls = () =>
      invokeMock.mock.calls.filter(([command]) => command === "plugin_index_refresh").length;
    const before = refreshCalls();
    const { container } = render(() => <StoreBrowser />);
    expect(container.querySelectorAll(".store-card")).toHaveLength(14);
    // The index is fetched when the browser opens, not while the app shell or
    // the curated site list paints.
    expect(refreshCalls()).toBe(before);
    fireEvent.click(container.querySelectorAll<HTMLButtonElement>(".store-tab")[1]);
    await waitFor(() => expect(container.querySelector(".plugin-store")).toBeTruthy());
    await waitFor(() => expect(container.querySelectorAll(".plugin-row").length).toBe(3));
    expect(container.querySelectorAll(".store-card")).toHaveLength(0);
    await waitFor(() => expect(refreshCalls()).toBe(before + 1));
  });
});

describe("cancelling an install", () => {
  it("offers a cancel button while the row is installing", async () => {
    const original = invokeMock.getMockImplementation()!;
    let release!: (value: unknown) => void;
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "plugin_store_install") {
        return new Promise((resolve) => { release = resolve; });
      }
      if (command === "install_cancel") return Promise.resolve(undefined);
      return original(command, args);
    });
    const { container } = await renderPluginStore();
    const row = container.querySelector('[data-plugin-id="asb2m10.dexed"]')!;
    fireEvent.click(row.querySelector(".plugin-action.primary")!);

    const cancel = await waitFor(() => {
      const button = [...row.querySelectorAll<HTMLButtonElement>(".plugin-action")]
        .find((entry) => entry.textContent === "Cancel");
      expect(button).toBeTruthy();
      return button!;
    });
    fireEvent.click(cancel);
    await waitFor(() => expectInvoked("install_cancel", { id: "asb2m10.dexed" }));
    release({ status: "installed", path: "/tmp/x", message: "done" });
  });
});
