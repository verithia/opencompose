/**
 * Resource browser: one overlay with Plugins, Samples and Effects tabs. The
 * plugin and sample tabs reuse the shipped surfaces; the effects tab is the
 * built-in catalogue and adds to the selected track.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { expectInvoked, invokeMock, makeSnapshot, makeTrack } from "./setup";
import { ResourceBrowser } from "../components/ResourceBrowser";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { pluginStore } from "../stores/pluginStore";

const PROVIDERS = [
  {
    id: "openverse",
    label: "Openverse",
    requires_key: false,
    kinds: ["loop", "fx"],
    license: "CC",
    search: true,
    download: true,
    available: true,
    reason: null,
  },
];

const ITEM = {
  id: "openverse:1",
  provider: "openverse",
  provider_name: "Openverse",
  name: "Dusty break",
  kind: "loop",
  duration_seconds: 2.5,
  channels: 2,
  sample_rate: 44100,
  license: {
    id: "cc0",
    name: "CC0",
    url: null,
    attribution_required: false,
    share_alike: false,
    non_commercial: false,
    no_derivatives: false,
  },
  attribution: "Nobody",
  source_url: "https://example.invalid/1",
  preview_url: null,
  download_url: "https://example.invalid/1.wav",
  quality: "original" as const,
  tags: [],
  license_ok: true,
};

function mockCommands() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "plugin_catalog") {
      return {
        platform: "macos-aarch64",
        source: "bundled",
        entries: [
          {
            id: "surge",
            name: "Surge XT",
            vendor: "Surge Synth Team",
            tags: ["instrument"],
            installs: [{ kind: "archive", platform: "macos-aarch64", format: "vst3", url: "" }],
            license: "GPL-3.0",
          },
        ],
        installed: [],
      };
    }
    if (cmd === "plugin_index_status") {
      return {
        source: "bundled",
        generated_at: "2026-01-01",
        fetched_at: null,
        stale_days: null,
        last_error: null,
        catalog_version: "1",
        next_refresh_in_seconds: 300,
      };
    }
    if (cmd === "sample_market_providers") return PROVIDERS;
    if (cmd === "sample_market_search") {
      const query = (args as { args: { query: string } }).args.query;
      return {
        status: "ok",
        query,
        kind: null,
        offset: 0,
        limit: 20,
        providers: PROVIDERS,
        pages: [],
        count: 1,
        next_offset: null,
        items: [ITEM],
      };
    }
    if (cmd === "get_project") return makeSnapshot({ tracks: [makeTrack("Vox", 0)] });
    if (cmd === "add_effect") return makeSnapshot({ tracks: [makeTrack("Vox", 0)] });
    if (cmd === "scan_plugins") {
      return [{
        id: "com.example.verb",
        name: "Example Verb",
        vendor: "Acme Audio",
        format: "Vst3",
        category: "Effect",
        path: "/plugins/Example Verb.vst3",
        num_params: 0,
        version: null,
        architectures: [],
        state_abi: null,
      }];
    }
    if (cmd === "devices_catalog") {
      return {
        devices: [
          {
            id: "reverb",
            name: "Reverb",
            category: "time",
            params: [],
          },
        ],
      };
    }
    if (cmd === "stop_audition") return undefined;
    return makeSnapshot();
  });
}

async function renderBrowser() {
  const rendered = render(() => <ResourceBrowser open onClose={() => {}} />);
  await vi.waitFor(() => expect(rendered.container.querySelector(".resource-browser")).toBeTruthy());
  return rendered;
}

beforeEach(async () => {
  i18n.setLocale("en");
  pluginStore.reset();
  invokeMock.mockReset();
  mockCommands();
  await store.initProject();
  store.setSelectedTrackIndex(0);
});

afterEach(() => cleanup());

describe("the resource browser", () => {
  it("offers the three tabs and opens on the plugin list", async () => {
    const { container } = await renderBrowser();

    const tabs = [...container.querySelectorAll(".resource-browser-tab")].map((node) =>
      node.textContent,
    );
    expect(tabs).toEqual([
      i18n.t("browse.tab.plugins"),
      i18n.t("browse.tab.samples"),
      i18n.t("browse.tab.effects"),
    ]);

    const plugins = container.querySelector('[data-tab="plugins"]')!;
    expect(plugins.getAttribute("aria-selected")).toBe("true");
    await vi.waitFor(() =>
      expect(container.querySelector('[data-plugin-id="surge"]')).toBeTruthy(),
    );
    expect(container.textContent).toContain("Surge XT");
  });

  it("searches the sample markets from the Samples tab", async () => {
    const { container } = await renderBrowser();

    fireEvent.click(container.querySelector('[data-tab="samples"]')!);
    await vi.waitFor(() =>
      expect(container.querySelector(".sample-market-search")).toBeTruthy(),
    );

    fireEvent.input(container.querySelector(".sample-market-search")!, {
      target: { value: "break" },
    });
    fireEvent.submit(container.querySelector(".sample-market-header")!);

    await vi.waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          ([cmd, args]) =>
            cmd === "sample_market_search"
            && JSON.stringify(args) === JSON.stringify({ args: { query: "break", providers: ["openverse"] } }),
        ),
      ).toBe(true),
    );
    await vi.waitFor(() => expect(container.textContent).toContain("Dusty break"));
  });

  it("lists the built-in effects and adds one to the selected track", async () => {
    const { container } = await renderBrowser();
    expect(store.selectedTrackIndex()).toBe(0);

    fireEvent.click(container.querySelector('[data-tab="effects"]')!);
    await vi.waitFor(() =>
      expect(container.querySelector('[data-device-id="reverb"]')).toBeTruthy(),
    );
    expect(container.querySelector(".resource-effects-name")?.textContent).toBe("Reverb");
    expect(container.querySelector(".resource-effects-description")?.textContent).toBe(
      i18n.t("device.desc.reverb"),
    );

    fireEvent.click(container.querySelector('[data-device-id="reverb"] .resource-effects-add')!);
    await vi.waitFor(() =>
      expectInvoked("add_effect", { target: `track:${store.selectedTrackIndex()}`, effectType: "reverb" }),
    );
  });

  it("lists a scanned plugin effect and adds it to the selected track", async () => {
    const { container } = await renderBrowser();

    fireEvent.click(container.querySelector('[data-tab="effects"]')!);
    await vi.waitFor(() =>
      expect(container.querySelector('[data-plugin-id="com.example.verb"]')).toBeTruthy(),
    );

    fireEvent.click(
      container.querySelector('[data-plugin-id="com.example.verb"] .resource-effects-add')!,
    );
    await vi.waitFor(() =>
      expectInvoked("add_effect", {
        target: `track:${store.selectedTrackIndex()}`,
        effectType: "plugin:com.example.verb",
      }),
    );
  });

  it("keeps the effect action disabled when no track is selected", async () => {
    store.setSelectedTrackIndex(null);
    const { container } = await renderBrowser();

    fireEvent.click(container.querySelector('[data-tab="effects"]')!);
    await vi.waitFor(() =>
      expect(container.querySelector('[data-device-id="reverb"]')).toBeTruthy(),
    );

    const add = container.querySelector(
      '[data-device-id="reverb"] .resource-effects-add',
    ) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    expect(add.getAttribute("title")).toBe(i18n.t("browse.effects.noTrack"));
  });
});
