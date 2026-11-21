import { describe, expect, it, vi } from "vitest";
import {
  SampleMarketError,
  createSampleMarketSearchPager,
  sampleMarketError,
  type SampleMarketItem,
  type SampleMarketSearchResult,
} from "../stores/sampleMarket";

describe("sampleMarketError", () => {
  it("keeps a refusal's code and message from the command", () => {
    const error = sampleMarketError({
      code: "licence_notice",
      message: "Licence: CC BY-NC 4.0.",
    });
    expect(error.code).toBe("licence_notice");
    expect(error.message).toBe("Licence: CC BY-NC 4.0.");
    expect(String(error)).toBe("Licence: CC BY-NC 4.0.");
  });

  it("treats a transport failure without a refusal code as no code", () => {
    expect(sampleMarketError({ code: null, message: "offline" }).code).toBeNull();
    expect(sampleMarketError("offline").code).toBeNull();
    expect(sampleMarketError("offline").message).toBe("offline");
  });

  it("returns an already normalized error unchanged", () => {
    const error = new SampleMarketError("invalid_cursor", "Start the search again.");
    expect(sampleMarketError(error)).toBe(error);
  });
});

function item(provider: string, id: string, source = `https://example.test/${id}`): SampleMarketItem {
  return {
    id,
    provider,
    name: id,
    kind: "loop",
    duration_seconds: 1,
    channels: 1,
    sample_rate: 44100,
    license: {
      id: "cc0-1.0",
      name: "CC0 1.0",
      url: null,
      attribution_required: false,
      share_alike: false,
      non_commercial: false,
      no_derivatives: false,
    },
    attribution: "",
    source_url: source,
    preview_url: null,
    download_url: source,
    quality: "original",
    tags: [],
    license_ok: true,
  };
}

function result(items: SampleMarketItem[], cursor: string | null = null): SampleMarketSearchResult {
  return {
    status: "ok",
    query: "snare",
    kind: null,
    commercial_ok: false,
    offset: 0,
    limit: 20,
    providers: [],
    pages: [],
    count: items.length,
    duplicates: 0,
    next_offset: null,
    cursor,
    items,
  };
}

describe("sample market pager", () => {
  it("appends a page and keeps a provider id or source URL once", async () => {
    const first = item("openverse", "a", "https://x.test/s/1/");
    const second = item("openverse", "b", "https://x.test/s/2/");
    const duplicateId = item("openverse", "a", "https://x.test/elsewhere/");
    const duplicateSource = item("freesound", "c", "https://X.TEST/s/2");
    const third = item("openverse", "d", "https://x.test/s/3");
    const answers = [
      result([first, second], "cursor-1"),
      result([duplicateId, duplicateSource, third], null),
    ];
    const run = vi.fn(async () => answers.shift()!);
    const pager = createSampleMarketSearchPager(run);

    await pager.search({ query: "snare", providers: ["openverse"] });
    expect(pager.items().map((entry) => entry.id)).toEqual(["a", "b"]);
    expect(pager.cursor()).toBe("cursor-1");

    const more = await pager.loadMore();
    expect(more).not.toBeNull();
    expect(pager.items().map((entry) => entry.id)).toEqual(["a", "b", "d"]);
    expect(pager.cursor()).toBeNull();
    expect(run).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: "snare", providers: ["openverse"], cursor: "cursor-1" }),
    );
  });

  it("drops a load-more answer that lands after a new search", async () => {
    let finishMore!: (value: SampleMarketSearchResult) => void;
    const run = vi
      .fn<() => Promise<SampleMarketSearchResult>>()
      .mockResolvedValueOnce(result([item("openverse", "first")], "cursor-1"))
      .mockImplementationOnce(() => new Promise<SampleMarketSearchResult>((resolve) => {
        finishMore = resolve;
      }))
      .mockResolvedValueOnce(result([item("openverse", "second")], null));
    const pager = createSampleMarketSearchPager(run);

    await pager.search({ query: "one" });
    const pending = pager.loadMore();
    await pager.search({ query: "two" });
    finishMore(result([item("openverse", "stale")], null));
    await pending;

    expect(pager.items().map((entry) => entry.id)).toEqual(["second"]);
    expect(pager.cursor()).toBeNull();
  });

  it("resets rows and cursor so an old page cannot load", async () => {
    const run = vi.fn(async () => result([item("openverse", "a")], "cursor-1"));
    const pager = createSampleMarketSearchPager(run);

    await pager.search({ query: "snare" });
    pager.reset();

    expect(pager.items()).toEqual([]);
    expect(pager.cursor()).toBeNull();
    await expect(pager.loadMore()).resolves.toBeNull();
    expect(run).toHaveBeenCalledTimes(1);
  });
});
