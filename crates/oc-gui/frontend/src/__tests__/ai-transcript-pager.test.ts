import { describe, expect, it, vi } from "vitest";
import { createTranscriptPager } from "../ai/transcriptPager";
import { TRANSCRIPT_RENDER_LIMIT, type ChatMessage } from "../ai/transcript";
import type { LoadedPage } from "../ai/sessions";
import type { WireMessage } from "../ai/wire";

function loaded(overrides: Partial<LoadedPage> = {}): LoadedPage {
  return {
    meta: { id: "s1", title: "paged", created: "", updated: "", model: "", provider: "", workspace_key: "", turns: 0, tokens: 0 },
    messages: [],
    skipped: 0,
    start: 4,
    total: 6,
    revision: "rev-1",
    older_tokens: 40,
    state: [],
    ...overrides,
  };
}

function harness(page: unknown, initial: ChatMessage[] = []) {
  let rows = initial;
  let repaints = 0;
  const errors: string[] = [];
  const pager = createTranscriptPager({
    messages: () => rows,
    setMessages: (updater) => { rows = updater(rows); },
    openId: () => "s1",
    epoch: () => 1,
    paintRequest: () => 3,
    locked: () => false,
    originalView: () => false,
    transcript: () => undefined,
    repaint: async () => { repaints += 1; return true; },
    readPage: async () => page,
    onError: (message) => errors.push(message),
  });
  return { pager, rows: () => rows, repaints: () => repaints, errors };
}

describe("the transcript pager", () => {
  it("owns the page window and resets it on a switch", () => {
    const { pager } = harness(null);
    pager.applyPage(loaded());
    expect(pager.pageStart()).toBe(4);
    expect(pager.pageRevision()).toBe("rev-1");
    expect(pager.olderTokens()).toBe(40);
    pager.showMoreRows();
    expect(pager.renderLimit()).toBe(TRANSCRIPT_RENDER_LIMIT * 2);
    pager.resetForOriginal();
    expect(pager.pageStart()).toBe(0);
    expect(pager.pageRevision()).toBe("");
    expect(pager.olderTokens()).toBe(0);
    pager.reset();
    expect(pager.renderLimit()).toBe(TRANSCRIPT_RENDER_LIMIT);
  });

  it("prepends an older page and keeps its rows visible", async () => {
    const message: WireMessage = { role: "user", content: "older question" };
    const { pager, rows } = harness({
      status: "page", messages: [message], start: 2, total: 6, revision: "rev-1", older_tokens: 10,
    }, [{ role: "user", content: "newest question" }]);
    pager.applyPage(loaded());
    await pager.loadOlderPage("s1");
    expect(rows().map((row) => row.content)).toEqual(["older question", "newest question"]);
    expect(pager.pageStart()).toBe(2);
    expect(pager.olderTokens()).toBe(10);
  });

  it("repaints the conversation when the saved view changed shape", async () => {
    const { pager, repaints, errors } = harness({ status: "stale" });
    pager.applyPage(loaded());
    await pager.loadOlderPage("s1");
    expect(repaints()).toBe(1);
    expect(errors).toEqual([]);
  });

  it("stays put when there is no older page to ask for", async () => {
    const readPage = vi.fn(async () => ({ status: "stale" }));
    let rows: ChatMessage[] = [];
    const pager = createTranscriptPager({
      messages: () => rows,
      setMessages: (updater) => { rows = updater(rows); },
      openId: () => "s1", epoch: () => 1, paintRequest: () => 1,
      locked: () => false, originalView: () => false, transcript: () => undefined,
      repaint: async () => true, readPage, onError: () => {},
    });
    await pager.loadOlderPage("s1");
    expect(readPage).not.toHaveBeenCalled();
  });
});
