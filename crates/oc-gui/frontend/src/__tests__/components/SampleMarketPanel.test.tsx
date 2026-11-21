/**
 * Samples panel — the Sources list, a search, a preview and an add.
 *
 * The backend is mocked at the `invoke` boundary: these tests assert the panel
 * sends the search arguments the tool contract expects and renders the licence
 * and credit the result carries.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_PLAYING, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { preferencesUi } from "../../stores/preferencesUi";
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

const EXTRA_ITEM = {
  ...ITEMS[0],
  id: "openverse:extra",
  name: "Extra clap",
  source_url: "https://example.test/extra-clap",
  download_url: "https://example.test/extra-clap.wav",
};

const LOCAL_ITEMS = [
  {
    path: "/Users/me/Music/Samples/Drums/Snare Tight.wav",
    name: "Snare Tight",
    folder: "/Users/me/Music/Samples",
    relative: "Drums/Snare Tight.wav",
    format: "wav",
    kind: "drum_one_shot",
    duration_seconds: null,
  },
  {
    path: "/Users/me/Music/Samples/Loops/snare roll.aif",
    name: "snare roll",
    folder: "/Users/me/Music/Samples",
    relative: "Loops/snare roll.aif",
    format: "aif",
    kind: "loop",
    duration_seconds: 1.5,
  },
];

const LOCAL_RESULT = { items: LOCAL_ITEMS, truncated: false, folders: 3 };
const NO_LOCAL = { items: [], truncated: false, folders: 3 };

const PAGED_RESULT = { ...SEARCH_RESULT, cursor: "next-page" };
const MORE_RESULT = {
  ...SEARCH_RESULT,
  cursor: null,
  items: [EXTRA_ITEM, ITEMS[0]],
};

const NOTICE_RESULT = {
  ...SEARCH_RESULT,
  pages: [
    {
      provider: "openverse",
      provider_name: "Openverse",
      count: 0,
      error: "openverse detail",
      error_kind: "needs_key",
    },
    {
      provider: "freesound",
      provider_name: "Freesound",
      count: 0,
      error: "freesound detail",
      error_kind: "rate_limited",
      retry_after_secs: 7,
    },
    {
      provider: "jamendo",
      provider_name: "Jamendo",
      count: 0,
      skipped: true,
      skip_reason: "circuit_open",
      error: "jamendo detail",
      error_kind: "skipped",
    },
    {
      provider: "pixabay",
      provider_name: "Pixabay",
      count: 0,
      error: "pixabay detail",
      error_kind: "network",
    },
    {
      provider: "internet-archive",
      provider_name: "Internet Archive",
      count: 0,
      error: "archive detail",
      error_kind: "other",
    },
  ],
  items: [],
};

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "sample_market_providers") return PROVIDERS;
    if (cmd === "sample_market_search") return SEARCH_RESULT;
    if (cmd === "local_sample_search") return NO_LOCAL;
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

/** The providers have answered: the Sources button counts the included ones. */
async function ready(container: HTMLElement) {
  await waitFor(() =>
    expect(container.querySelector(".sample-market-sources-count")?.textContent).toBe("2"),
  );
}

/** Open the Sources list and return it. */
function openSources(container: HTMLElement): HTMLElement {
  fireEvent.click(container.querySelector(".sample-market-sources-toggle")!);
  return container.querySelector(".sample-market-sources-list") as HTMLElement;
}

function sourceBox(container: HTMLElement, id: string): HTMLInputElement {
  return container.querySelector(`.sample-market-source[data-provider="${id}"] input`) as HTMLInputElement;
}

async function search(container: HTMLElement) {
  const input = container.querySelector(".sample-market-search") as HTMLInputElement;
  fireEvent.input(input, { target: { value: "snare" } });
  fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);
  await waitFor(() =>
    expect(
      container.querySelector(".sample-market-row:not(.sample-market-row-skeleton)"),
    ).toBeTruthy(),
  );
}

describe("SampleMarketPanel", () => {
  it("lists every source as a checkbox under the Sources button", async () => {
    const { container, getByRole } = render(() => <SampleMarketPanel />);
    await ready(container);
    // Closed by default: the toolbar stays one strip.
    const toggle = getByRole("button", { name: "Sources" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".sample-market-sources-list")).toBeNull();

    const list = openSources(container);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(toggle.getAttribute("aria-controls")).toBe(list.id);
    // The list opens beneath the toolbar, not inside it.
    expect(container.querySelector(".sample-market-toolbar")!.nextElementSibling).toBe(list);
    expect(
      [...list.querySelectorAll<HTMLElement>(".sample-market-source")].map((label) => label.dataset.provider),
    ).toEqual(["openverse", "wikimedia-commons", "freesound", "mixkit"]);
    expect(sourceBox(container, "openverse").checked).toBe(true);
    // A market the registry adds later needs no panel change: it is listed
    // from the availability map and is included by default.
    expect(sourceBox(container, "wikimedia-commons").checked).toBe(true);
    for (const id of ["freesound", "mixkit"]) {
      expect(sourceBox(container, id).disabled).toBe(true);
      expect(sourceBox(container, id).checked).toBe(false);
      expect(
        container.querySelector(`.sample-market-source[data-provider="${id}"]`)!.classList.contains("unavailable"),
      ).toBe(true);
    }

    fireEvent.click(toggle);
    expect(container.querySelector(".sample-market-sources-list")).toBeNull();
  });

  it("greys out a source without a key and says where the key goes", async () => {
    const { container, getByRole, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    openSources(container);

    const freesound = getByRole("checkbox", { name: "Freesound" });
    expect(freesound).toBeDisabled();
    expect(freesound).toHaveAccessibleDescription(
      "Add a Freesound API key in Preferences › External Sources to search it.",
    );
    // The backend's sentence is not the label; the panel words it per locale.
    expect(queryByText(/FREESOUND_API_KEY/)).toBeNull();
    expect(getByRole("checkbox", { name: "Mixkit" })).toHaveAccessibleDescription(
      "Mixkit can't be searched from here.",
    );
    expect(getByRole("checkbox", { name: "Openverse" })).not.toHaveAccessibleDescription();

    // Ticking it does nothing, and no key form appears in the panel.
    fireEvent.click(freesound);
    expect(freesound).not.toBeChecked();
    expect(container.querySelector('input[type="password"]')).toBeNull();
    await search(container);
    expect(invokeMock).toHaveBeenCalledWith("sample_market_search", {
      args: expect.objectContaining({ providers: ["openverse", "wikimedia-commons"] }),
    });
  });

  it("paints the search field and a skeleton before the markets answer", () => {
    // Reading the keyring is the slow part; the panel must not sit empty.
    invokeMock.mockImplementation((command: string) =>
      command === "sample_market_providers" ? new Promise(() => {}) : Promise.resolve(undefined),
    );
    const { container } = render(() => <SampleMarketPanel />);
    expect(container.querySelector(".sample-market-search")).not.toBeNull();
    // The kinds are ready; the Sources list holds only the placeholder, so no
    // source can be ticked yet.
    expect(container.querySelectorAll(".sample-market-kinds .sample-market-chip")).toHaveLength(6);
    expect(container.querySelector(".sample-market-sources-count")?.textContent).toBe("…");
    const list = openSources(container);
    expect(list.querySelectorAll(".sample-market-source")).toHaveLength(0);
    expect(list.querySelector(".sample-market-source-loading")?.textContent).toContain(
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
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
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
    // A finished search has no cursor, so it offers no continuation.
    expect(queryByText("Load more")).toBeNull();
  });

  it("maps kind, duration and commercial filters to the search arguments", async () => {
    const { container, getByRole } = render(() => <SampleMarketPanel />);
    await ready(container);

    fireEvent.click(getByRole("button", { name: "Instrument" }));
    fireEvent.change(getByRole("combobox", { name: "Duration" }), {
      target: { value: "medium" },
    });
    fireEvent.click(getByRole("checkbox", { name: "Commercial use OK" }));
    const input = container.querySelector(".sample-market-search") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "snare" } });
    fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);

    await waitFor(() => expect(container.querySelector(".sample-market-row")).toBeTruthy());
    expect(invokeMock).toHaveBeenCalledWith("sample_market_search", {
      args: expect.objectContaining({
        query: "snare",
        kind: "instrument",
        min_seconds: 2,
        max_seconds: 10,
        commercial_ok: true,
        providers: ["openverse", "wikimedia-commons"],
      }),
    });
  });

  it("shows Load more for a cursor and appends the next page without repeats", async () => {
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "sample_market_search") {
        const cursor = (args as { args?: { cursor?: string } } | undefined)?.args?.cursor;
        return cursor ? MORE_RESULT : PAGED_RESULT;
      }
      return backend(cmd, args);
    });
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);

    fireEvent.click(getByText("Load more"));
    await waitFor(() => expect(getByText("Extra clap")).toBeInTheDocument());

    expect(invokeMock).toHaveBeenCalledWith("sample_market_search", {
      args: expect.objectContaining({
        query: "snare",
        providers: ["openverse", "wikimedia-commons"],
        cursor: "next-page",
      }),
    });
    expect(getByText("Snare hit 01")).toBeInTheDocument();
    expect(container.querySelectorAll(".sample-market-row")).toHaveLength(3);
    expect(container.textContent).not.toContain("Load more");
  });

  it("keeps Load more inside the scrolling list instead of a footer row", async () => {
    // The panel's height belongs to the results: a continuation is the last
    // row of the list, so an open cursor never shortens the visible rows.
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "sample_market_search") return PAGED_RESULT;
      return backend(cmd, args);
    });
    const { container } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);

    const more = container.querySelector(".sample-market-more");
    expect(more).not.toBeNull();
    expect(more!.parentElement?.classList.contains("sample-market-list")).toBe(true);
  });

  it("orders a row: name, preview, Add, details, licence", async () => {
    // The compact row reads left to right as name, what you can do with it,
    // then what it is. The Add button is short on screen and named in full
    // for assistive tech.
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);

    const row = getByText("Snare hit 01").closest(".sample-market-row") as HTMLElement;
    const parts = ["sample-market-name", "sample-market-play", "sample-market-add",
      "sample-market-meta", "sample-market-credit", "sample-market-license"];
    expect(
      [...row.children].map((child) => parts.find((part) => child.classList.contains(part))),
    ).toEqual(parts);
    const add = within(row).getByRole("button", { name: "Add to project" });
    expect(add.textContent).toBe("Add");
    expect(within(row).getByRole("button", { name: "Preview" })).toBeInTheDocument();
  });

  it("lays the rows out in the shared result grid", async () => {
    // One grid per section, so local and market rows wrap two to a line on a
    // wide panel without a section's rows spilling into the other's.
    const { container } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);

    const grids = [...container.querySelectorAll(".sample-market-grid")];
    expect(grids).toHaveLength(2);
    for (const row of container.querySelectorAll(".sample-market-row")) {
      expect(row.parentElement?.classList.contains("sample-market-grid")).toBe(true);
    }
  });

  it("keeps the toolbar to one strip of controls", async () => {
    // Search, duration, licence, kinds and Sources share one strip: the bottom
    // panel is short, so each extra toolbar row costs a result.
    const { container } = render(() => <SampleMarketPanel />);
    await ready(container);

    const toolbar = container.querySelector(".sample-market-toolbar");
    expect(toolbar).not.toBeNull();
    expect(toolbar!.querySelector(".sample-market-header")).not.toBeNull();
    expect(toolbar!.querySelector(".sample-market-filters")).not.toBeNull();
    // Kinds and the Sources button wrap together, so a narrow panel loses no
    // extra line; the sources themselves are not in the strip.
    const markets = toolbar!.querySelector(".sample-market-markets");
    expect(markets?.querySelector(".sample-market-kinds")).not.toBeNull();
    expect(markets?.querySelector(".sample-market-sources-toggle")).not.toBeNull();
    expect(toolbar!.querySelector(".sample-market-source")).toBeNull();
  });

  it("opens External Sources from the Sources list", async () => {
    preferencesUi.close();
    const { container, getByRole } = render(() => <SampleMarketPanel />);
    await ready(container);
    openSources(container);

    fireEvent.click(getByRole("button", { name: "External Sources…" }));
    expect(preferencesUi.open()).toBe(true);
    expect(preferencesUi.page()).toBe("sources");
    preferencesUi.close();
  });

  it("renders per-market notices from typed error kinds", async () => {
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "sample_market_search") return NOTICE_RESULT;
      return backend(cmd, args);
    });
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    const input = container.querySelector(".sample-market-search") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "snare" } });
    fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);

    await waitFor(() =>
      expect(getByText("Freesound is rate limited. Retry in 7 s.")).toBeInTheDocument(),
    );
    // A missing key is the Sources list's to explain, never a search notice.
    expect(container.textContent).not.toMatch(/API key/);
    expect(container.querySelectorAll(".sample-market-notice")).toHaveLength(4);
    expect(getByText("Jamendo is temporarily skipped.")).toBeInTheDocument();
    expect(getByText("Couldn't reach Pixabay.")).toBeInTheDocument();
    expect(getByText("Internet Archive could not complete the search.")).toBeInTheDocument();
    // The provider's sentence is a fallback detail, not the visible label.
    expect(queryByText("pixabay detail")).toBeNull();
    expect(getByText("Couldn't reach Pixabay.").getAttribute("title")).toBe("pixabay detail");
  });

  it("maps a search refusal code to a localized message", async () => {
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "sample_market_search") {
        throw { code: "invalid_cursor", message: "cursor expired" };
      }
      return backend(cmd, args);
    });
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);

    const input = container.querySelector(".sample-market-search") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "snare" } });
    fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);

    await waitFor(() =>
      expect(
        getByText("Search failed: The search expired. Start it again."),
      ).toBeInTheDocument(),
    );
    expect(queryByText("Search failed: cursor expired")).toBeNull();
  });

  it("starts a result as a private paused preview before resuming it", async () => {
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
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
    await ready(panel.container);
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
      await ready(panel.container);
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
    await ready(container);
    await search(container);

    const row = getByText("Snare hit 01").closest(".sample-market-row") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Add to project" }));
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
    await ready(container);
    await search(container);

    const row = getByText("Night Drive").closest(".sample-market-row") as HTMLElement;
    const add = within(row).getByRole("button", { name: "Add to project" });
    expect(add).not.toBeDisabled();
    invokeMock.mockRejectedValueOnce({
      code: "licence_notice",
      message: "Licence: CC BY-NC 4.0. The file keeps this licence.",
    });
    fireEvent.click(add);

    await waitFor(() => expect(getByText("Download anyway")).toBeInTheDocument());
    expect(getByText("Licence: CC BY-NC 4.0. The file keeps this licence.")).toBeInTheDocument();
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

  it("shows a refusal's message when it is not a licence notice", async () => {
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);

    const row = getByText("Snare hit 01").closest(".sample-market-row") as HTMLElement;
    invokeMock.mockRejectedValueOnce({
      code: "checksum_mismatch",
      message: "The download does not match its declared checksum.",
    });
    fireEvent.click(within(row).getByRole("button", { name: "Add to project" }));

    await waitFor(() =>
      expect(
        getByText("The download does not match its checksum. Nothing was added."),
      ).toBeInTheDocument(),
    );
    expect(queryByText("The download does not match its declared checksum.")).toBeNull();
    expect(queryByText("Download anyway")).toBeNull();
  });
});

describe("local samples", () => {
  function withLocal(result: unknown) {
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "local_sample_search") return result;
      if (cmd === "local_sample_preview") {
        return { status: "ok", provider: "local", id: LOCAL_ITEMS[0].path, name: "Snare Tight", seconds: 0.4 };
      }
      if (cmd === "local_sample_add") {
        return {
          status: "ok",
          track: "Samples",
          file: "/tmp/project/media/local-samples/Snare-Tight.wav",
          snapshot: makeSnapshot({ tracks: [makeTrack("Samples", 0)] }),
        };
      }
      return backend(cmd, args);
    });
  }

  it("lists files on this computer before the market results", async () => {
    withLocal(LOCAL_RESULT);
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);
    await waitFor(() => expect(getByText("Snare Tight")).toBeInTheDocument());

    const rows = [...container.querySelectorAll(".sample-market-row")].map(
      (row) => row.querySelector(".sample-market-name")!.textContent,
    );
    expect(rows).toEqual(["Snare Tight", "snare roll", "Snare hit 01", "Night Drive"]);
    const headings = [...container.querySelectorAll(".sample-market-section-title")].map(
      (node) => node.textContent,
    );
    expect(headings).toEqual(["On this computer", "Online"]);
    // A local file shows where it sits and that it is the user's own file.
    const local = getByText("Snare Tight").closest(".sample-market-row") as HTMLElement;
    expect(within(local).getByText("Drums/Snare Tight.wav")).toBeInTheDocument();
    expect(within(local).getByText("Local file")).toBeInTheDocument();
    expect(within(local).queryByText("unknown")).toBeNull();
    const roll = getByText("snare roll").closest(".sample-market-row") as HTMLElement;
    expect(within(roll).getByText("0:01.5")).toBeInTheDocument();
  });

  it("sends the kind and duration filters to the local search too", async () => {
    withLocal(LOCAL_RESULT);
    const { container, getByRole } = render(() => <SampleMarketPanel />);
    await ready(container);
    fireEvent.click(getByRole("button", { name: "Loop" }));
    fireEvent.change(getByRole("combobox", { name: "Duration" }), { target: { value: "short" } });
    await search(container);
    expect(invokeMock).toHaveBeenCalledWith("local_sample_search", {
      args: { query: "snare", kind: "loop", min_seconds: undefined, max_seconds: 2 },
    });
  });

  it("searches this computer even when no market is included", async () => {
    withLocal(LOCAL_RESULT);
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    openSources(container);
    fireEvent.click(sourceBox(container, "openverse"));
    fireEvent.click(sourceBox(container, "wikimedia-commons"));
    expect(container.querySelector(".sample-market-sources-count")?.textContent).toBe("0");
    await search(container);

    await waitFor(() => expect(getByText("Snare Tight")).toBeInTheDocument());
    expect(getByText("Open Sources to choose the online sources to search.")).toBeInTheDocument();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "sample_market_search")).toBe(false);
  });

  it("says when no local file matches and when there are no sound folders", async () => {
    withLocal(NO_LOCAL);
    const first = render(() => <SampleMarketPanel />);
    await ready(first.container);
    await search(first.container);
    await waitFor(() =>
      expect(first.getByText("No matching files in your sound folders.")).toBeInTheDocument(),
    );
    first.unmount();

    withLocal({ items: [], truncated: false, folders: 0 });
    const second = render(() => <SampleMarketPanel />);
    await ready(second.container);
    await search(second.container);
    await waitFor(() =>
      expect(
        second.getByText("No sound folders were found on this computer."),
      ).toBeInTheDocument(),
    );
  });

  it("previews a local file by its path and adds it to the project", async () => {
    withLocal(LOCAL_RESULT);
    const { container, getByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);
    await waitFor(() => expect(getByText("Snare Tight")).toBeInTheDocument());

    fireEvent.click(getByText("Snare Tight"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("local_sample_preview", {
        path: LOCAL_ITEMS[0].path,
        label: "Snare Tight",
        startPaused: true,
      }),
    );

    const row = getByText("Snare Tight").closest(".sample-market-row") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Add to project" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("local_sample_add", {
        path: LOCAL_ITEMS[0].path,
        track: null,
      }),
    );
    await waitFor(() => expect(getByText("Added to Samples")).toBeInTheDocument());
    expect(store.project.tracks.length).toBe(1);
  });

  it("names a local refusal by its code", async () => {
    withLocal(LOCAL_RESULT);
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);
    await waitFor(() => expect(getByText("Snare Tight")).toBeInTheDocument());

    invokeMock.mockRejectedValueOnce({ code: "not_found", message: "'/x.wav' is gone" });
    const row = getByText("Snare Tight").closest(".sample-market-row") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Add to project" }));
    await waitFor(() =>
      expect(getByText("That file is no longer on this computer.")).toBeInTheDocument(),
    );
    expect(queryByText("'/x.wav' is gone")).toBeNull();
  });

  it("drops a local answer that a newer search replaced", async () => {
    const answers: ((value: unknown) => void)[] = [];
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "local_sample_search") return new Promise((resolve) => { answers.push(resolve); });
      return backend(cmd, args);
    });
    const { container, getByText, queryByText } = render(() => <SampleMarketPanel />);
    await ready(container);
    await search(container);
    await search(container);
    await waitFor(() => expect(answers).toHaveLength(2));

    answers[0](LOCAL_RESULT);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(queryByText("Snare Tight")).toBeNull();
    expect(getByText("Searching your sound folders…")).toBeInTheDocument();

    answers[1](NO_LOCAL);
    await waitFor(() =>
      expect(getByText("No matching files in your sound folders.")).toBeInTheDocument(),
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
