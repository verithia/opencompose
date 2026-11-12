/**
 * Sample markets panel — provider chips, a search, a preview and an add.
 *
 * The backend is mocked at the `invoke` boundary: these tests assert the panel
 * sends the search arguments the tool contract expects and renders the licence
 * and credit the result carries.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_PLAYING, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { SampleMarketPanel } from "../../components/SampleMarketPanel";
import { ContextMenuLayer } from "../../components/ContextMenu";

const PROVIDERS = [
  {
    id: "openverse",
    label: "Openverse",
    requires_key: false,
    kinds: ["drum_one_shot", "loop", "vocal", "fx", "instrument_phrase", "unknown"],
    license: "Aggregated per result.",
    search: true,
    download: true,
    available: true,
    reason: null,
  },
  {
    id: "wikimedia-commons",
    label: "Wikimedia Commons",
    requires_key: false,
    kinds: ["loop", "drum_one_shot"],
    license: "Per file.",
    search: true,
    download: true,
    available: true,
    default_search: true,
    reason: null,
  },
  {
    id: "freesound",
    label: "Freesound",
    requires_key: true,
    kinds: ["drum_one_shot"],
    license: "Per sound label.",
    search: true,
    download: true,
    available: false,
    reason: "needs an API key: set FREESOUND_API_KEY (or the org.opencompose.samples/freesound keyring item)",
  },
  {
    id: "mixkit",
    label: "Mixkit",
    requires_key: false,
    kinds: ["fx", "loop"],
    license: "Mixkit licence.",
    search: false,
    download: false,
    available: false,
    reason: "no search API; the download flow is JavaScript-only",
  },
];

const ITEMS = [
  {
    id: "openverse:snare",
    provider: "openverse",
    provider_name: "Openverse",
    name: "Snare hit 01",
    kind: "drum_one_shot",
    duration_seconds: 0.42,
    channels: 1,
    sample_rate: 44100,
    license: {
      id: "cc0-1.0",
      name: "CC0 1.0",
      url: "https://creativecommons.org/publicdomain/zero/1.0/",
      attribution_required: false,
      share_alike: false,
      non_commercial: false,
      no_derivatives: false,
    },
    attribution: "bassmosphere — CC0 1.0 — https://freesound.org/s/2/",
    source_url: "https://freesound.org/s/2/",
    preview_url: "https://cdn.freesound.org/previews/1/2_3-hq.mp3",
    download_url: "https://cdn.freesound.org/previews/1/2_3-hq.mp3",
    quality: "preview_mp3" as const,
    tags: ["drums"],
    license_ok: true,
  },
  {
    id: "openverse:night",
    provider: "openverse",
    provider_name: "Openverse",
    name: "Night Drive",
    kind: "instrument_phrase",
    duration_seconds: 214,
    channels: 2,
    sample_rate: 44100,
    license: {
      id: "cc-by-nc-4.0",
      name: "CC BY-NC 4.0",
      url: "https://creativecommons.org/licenses/by-nc/4.0/",
      attribution_required: true,
      share_alike: false,
      non_commercial: true,
      no_derivatives: false,
    },
    attribution: "The Frozenauts — CC BY-NC 4.0 — https://www.jamendo.com/track/123456",
    source_url: "https://www.jamendo.com/track/123456",
    preview_url: null,
    download_url: "https://prod-1.storage.jamendo.com/?trackid=123456",
    quality: "original" as const,
    tags: ["electronic"],
    license_ok: false,
  },
];

const SEARCH_RESULT = {
  status: "ok" as const,
  query: "snare",
  kind: null,
  offset: 0,
  limit: 20,
  providers: PROVIDERS,
  pages: [
    { provider: "openverse", provider_name: "Openverse", count: 2, total: 2, next_offset: null },
  ],
  count: 2,
  next_offset: null,
  items: ITEMS,
};

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "sample_market_providers") return PROVIDERS;
    if (cmd === "sample_market_search") return SEARCH_RESULT;
    if (cmd === "sample_preview") {
      return { status: "ok", provider: "openverse", id: "openverse:snare", name: "Snare hit 01", seconds: 0.42 };
    }
    if (cmd === "stop_audition") return undefined;
    if (cmd === "sample_market_save_key") return { status: "ok", provider: "freesound" };
    if (cmd === "sample_market_add") {
      return {
        status: "ok",
        provider: "openverse",
        provider_name: "Openverse",
        track: "Samples",
        receipt: {
          id: "openverse:snare",
          name: "Snare hit 01",
          file: "openverse-snare-hit-01-abc12345.mp3",
          path: "/tmp/project/media/samples/openverse-snare-hit-01-abc12345.mp3",
          bytes: 16384,
          license: ITEMS[0].license,
          attribution: ITEMS[0].attribution,
          quality: "preview_mp3",
        },
        snapshot: makeSnapshot({ tracks: [makeTrack("Samples", 0)] }),
      };
    }
    return makeSnapshot();
  });
  await store.initProject();
});

async function search(container: HTMLElement) {
  const input = container.querySelector(".sample-market-search") as HTMLInputElement;
  fireEvent.input(input, { target: { value: "snare" } });
  fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);
  await waitFor(() =>
    expect(container.querySelector(".sample-market-row")).toBeTruthy(),
  );
}

describe("SampleMarketPanel", () => {
  it("shows every market with its availability", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Openverse")).toBeInTheDocument());
    expect(getByText("Freesound")).toBeInTheDocument();
    expect(getByText("Mixkit")).toBeInTheDocument();
    expect(
      container.querySelector('[data-provider="mixkit"]')!.classList.contains("unavailable"),
    ).toBe(true);
    expect(
      container.querySelector('[data-provider="openverse"]')!.classList.contains("unavailable"),
    ).toBe(false);
    // A market the registry adds later needs no panel change: the chip is
    // rendered from the availability map and is included by default.
    const commons = container.querySelector('[data-provider="wikimedia-commons"]')!;
    expect(commons.classList.contains("unavailable")).toBe(false);
    expect(commons.classList.contains("on")).toBe(true);
  });

  it("paints the search field and a skeleton before the markets answer", () => {
    // Reading the keyring is the slow part; the panel must not sit empty.
    invokeMock.mockImplementation((command: string) =>
      command === "sample_market_providers" ? new Promise(() => {}) : Promise.resolve(undefined),
    );
    const { container } = render(() => <SampleMarketPanel />);
    expect(container.querySelector(".sample-market-search")).not.toBeNull();
    expect(
      container.querySelectorAll(".sample-market-chip:not(.sample-market-chip-loading)"),
    ).toHaveLength(5);
    expect(container.querySelector(".sample-market-chip-loading")?.textContent).toContain(
      "Loading markets",
    );
    const skeleton = container.querySelector(".sample-market-skeleton");
    expect(skeleton?.getAttribute("aria-busy")).toBe("true");
    expect(skeleton?.textContent).toContain("Loading markets");
    expect(container.querySelectorAll(".sample-market-row-skeleton")).toHaveLength(4);
    // No empty-state line while the markets are still being read.
    expect(container.textContent).not.toContain("Search a market to see its samples.");
  });

  it("searches the included markets and shows licence and credit", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Openverse")).toBeInTheDocument());
    await search(container);

    expect(invokeMock).toHaveBeenCalledWith("sample_market_search", {
      args: expect.objectContaining({
        query: "snare",
        providers: ["openverse", "wikimedia-commons"],
      }),
    });
    expect(getByText("Snare hit 01")).toBeInTheDocument();
    expect(getByText("CC0 1.0")).toBeInTheDocument();
    expect(getByText("CC BY-NC 4.0").classList.contains("notice")).toBe(true);
    expect(getByText(/bassmosphere/)).toBeInTheDocument();
  });

  it("starts a result as a private paused preview before resuming it", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Openverse")).toBeInTheDocument());
    await search(container);

    fireEvent.click(getByText("Snare hit 01"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sample_preview", {
        provider: "openverse",
        id: "openverse:snare",
        label: "Snare hit 01",
        startPaused: true,
      }),
    );
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_audition")).toBe(true));
  });

  it("keeps the arrangement parked when Space is pressed during a sample download", async () => {
    const backend = invokeMock.getMockImplementation()!;
    let finish!: (value: { status: "ok" }) => void;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") return TRANSPORT_STOPPED();
      if (cmd === "sample_preview") {
        return new Promise<{ status: "ok" }>(resolve => { finish = resolve; });
      }
      if (cmd === "resume_audition") return { ...TRANSPORT_STOPPED(),
        audition: { preset: "Snare hit 01", status: "ready", paused: false } };
      return backend(cmd, args);
    });
    const panel = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(panel.getByText("Openverse")).toBeInTheDocument());
    await search(panel.container);
    await store.play();
    fireEvent.click(panel.getByText("Snare hit 01"));
    await waitFor(() => expect(finish).toBeTypeOf("function"));
    await store.togglePlayPause();
    finish({ status: "ok" });
    await waitFor(() => expect(panel.getByRole("button", { name: "Resume preview" })).toBeInTheDocument());
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(0);
    fireEvent.click(panel.getByRole("button", { name: "Resume preview" }));
    await waitFor(() => expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(1));
    expect(panel.getByRole("button", { name: "Pause preview" })).toBeInTheDocument();
    await store.stop();
  });

  it("closing the first browser leaves the same result playing from a second browser", async () => {
    const first = render(() => <SampleMarketPanel />);
    const second = render(() => <SampleMarketPanel />);
    for (const panel of [first, second]) {
      await waitFor(() => expect(within(panel.container).getByText("Openverse")).toBeInTheDocument());
      await search(panel.container);
      fireEvent.click(within(panel.container).getByText("Snare hit 01"));
      await waitFor(() => expect(within(panel.container).getByRole("button", { name: "Pause preview" })).toBeInTheDocument());
    }
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "sample_preview")).toHaveLength(2);
    invokeMock.mockClear();
    first.unmount();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "stop_audition")).toBe(false);
    expect(within(second.container).getByRole("button", { name: "Pause preview" })).toBeInTheDocument();
    second.unmount();
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "stop_audition")).toBe(true));
  });

  it("adds a result to the project at the playhead", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Openverse")).toBeInTheDocument());
    await search(container);

    const row = getByText("Snare hit 01").closest(".sample-market-row") as HTMLElement;
    fireEvent.click(within(row).getByText("Add to project"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sample_market_add", {
        provider: "openverse",
        id: "openverse:snare",
        track: null,
        acceptLicence: false,
      }),
    );
    await waitFor(() => expect(store.project.tracks.length).toBe(1));
    expect(getByText("Added to Samples")).toBeInTheDocument();
  });

  it("asks before adding a result whose licence is not free to reuse", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Openverse")).toBeInTheDocument());
    await search(container);

    const row = getByText("Night Drive").closest(".sample-market-row") as HTMLElement;
    const add = within(row).getByText("Add to project");
    expect(add).not.toBeDisabled();
    invokeMock.mockRejectedValueOnce(
      "licence_notice: Licence: CC BY-NC 4.0. The file keeps this licence.",
    );
    fireEvent.click(add);

    await waitFor(() => expect(getByText("Download anyway")).toBeInTheDocument());
    fireEvent.click(getByText("Download anyway"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("sample_market_add", {
        provider: "openverse",
        id: "openverse:night",
        track: null,
        acceptLicence: true,
      }),
    );
  });

  it("asks for a keyed market's key inline", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await waitFor(() => expect(getByText("Freesound")).toBeInTheDocument());
    fireEvent.click(container.querySelector('[data-provider="freesound"]')!);
    await waitFor(() => expect(container.querySelector(".sample-market-key")).toBeTruthy());

    const input = container.querySelector(".sample-market-key input") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "KEY" } });
    fireEvent.submit(container.querySelector(".sample-market-key") as HTMLFormElement);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sample_market_save_key", {
        provider: "freesound",
        key: "KEY",
      }),
    );
  });
});

describe("sample row menu", () => {
  it("copies the credit and the source link", async () => {
    const writeText = vi.fn(async () => {});
    Object.assign(navigator, { clipboard: { writeText } });
    const { container } = render(() => <><SampleMarketPanel /><ContextMenuLayer /></>);
    await waitFor(() => expect(container.querySelector(".sample-market-search")).toBeTruthy());
    await search(container);
    fireEvent.contextMenu(container.querySelector(".sample-market-row")!, { clientX: 20, clientY: 20 });
    const labels = [...document.querySelectorAll(".context-menu-label")].map((node) => node.textContent);
    expect(labels).toContain("Copy credit");
    expect(labels).toContain("Copy source link");

    const credit = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((item) => item.querySelector(".context-menu-label")?.textContent === "Copy credit")!;
    fireEvent.click(credit);
    await waitFor(() => expect(writeText).toHaveBeenCalled());
  });
});
