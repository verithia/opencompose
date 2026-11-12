import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { AiStats } from "../components/AiStats";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { aiConfig, getActiveProvider } from "../ai/config";
import { MEDIA_EXTENSIONS } from "../ai/media";
import { aiLimits } from "../stores/aiLimits";
import { i18n, type I18nKey } from "../stores/i18n";
import { invokeMock, makeSnapshot, listenMock } from "./setup";
import { contextMeter, resetContextMeter } from "../stores/aiContext";
import type { AccountingUpdate, SessionUsageSnapshot, SessionUsageTotals, StatsEvent } from "../ai/sessions";
import {
  aiStats, acceptAccounting, beginUsageHydration, cancelUsageHydration, usageHydrationNeedsReconcile, hydrateSessionUsage, selectStatsSession,
  noteSentEstimate, startAiStatsSession, resetSentEstimate,
  cacheHitPercent, footerStats, formatCost, formatDuration, formatTokens, formatTps,
  recordTurn, resetStats, type TurnStats,
} from "../stores/aiStats";

function turn(overrides: Partial<TurnStats> = {}): TurnStats {
  return { model: "fixture-model", wire: "openai-chat-completions", request_id: "provider-1",
    retries: 0, ttft_ms: 412, decode_ms: 8123, wall_ms: 8540, decode_tps: 38.2,
    wall_tps: 36.4, usage: { input: 1200, cached_input: 744, output: 310, reasoning: 60 },
    failed: false, cost: null, ...overrides };
}
function usage(totals: Partial<SessionUsageTotals> = {}, fields: Partial<SessionUsageSnapshot> = {}): SessionUsageSnapshot {
  return { session_id: "s1", incarnation: "life-1", revision: 1, pending_requests: 0,
    last_turn: turn(), totals: { requests: 1, attempts: 1, input: 1200, output: 310, reasoning: 60,
      cached_input: 744, cache_input: 1200, failed_attempts: 0, unknown_price_attempts: 0,
      unreported_usage_attempts: 0, interrupted_requests: 0, legacy_unknown: false,
      ttft_ms_sum: 412, ttft_count: 1, decode_tps_sum: 38.2, decode_tps_count: 1, costs: {}, ...totals }, ...fields };
}
function showUsage(value: SessionUsageSnapshot) {
  selectStatsSession(value.session_id);
  hydrateSessionUsage(value.session_id, value);
}
function update(value: SessionUsageSnapshot, saved = true): AccountingUpdate {
  return { session_id: value.session_id, incarnation: value.incarnation, revision: value.revision,
    snapshot: value, saved, ...(saved ? {} : { error: { code: "accounting_storage", message: "disk full" } }) };
}
function event(value: SessionUsageSnapshot, fields: Partial<StatsEvent> = {}): StatsEvent {
  return { ...turn(), client_request_id: "local-1", purpose: "chat", accounting: update(value), ...fields };
}
function mockPanelBackend() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") return { version: 2, active_provider: "", providers: [] };
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_toolsets" || cmd === "ai_tools") return [];
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    return makeSnapshot();
  });
}
function follows(later: Element, earlier: Element): boolean {
  return Boolean(earlier.compareDocumentPosition(later) & Node.DOCUMENT_POSITION_FOLLOWING);
}
beforeEach(() => { i18n.setLocale("en"); resetStats(); resetContextMeter(); aiLimits.setActiveContext(null); invokeMock.mockReset(); mockPanelBackend(); });
afterEach(() => { resetStats(); resetContextMeter(); aiLimits.setActiveContext(null); i18n.setLocale("en"); });

describe("number formatting", () => {
  it("uses milliseconds under a second and one decimal of seconds above it", () => {
    expect(formatDuration(412)).toBe("412 ms");
    expect(formatDuration(999)).toBe("999 ms");
    expect(formatDuration(1000)).toBe("1.0 s");
    expect(formatDuration(8540)).toBe("8.5 s");
  });

  it("abbreviates thousands and millions with one decimal", () => {
    expect(formatTokens(310)).toBe("310");
    expect(formatTokens(1234)).toBe("1.2k");
    expect(formatTokens(12_345)).toBe("12.3k");
    expect(formatTokens(1_200_000)).toBe("1.2M");
  });

  it("prints TPS with one decimal", () => {
    expect(formatTps(38.24)).toBe("38.2");
    expect(formatTps(100)).toBe("100.0");
  });

  it("derives the cache hit from reported tokens only", () => {
    expect(cacheHitPercent({ input: 1200, cached_input: 744, output: 10, reasoning: 0 })).toBe(62);
    expect(cacheHitPercent({ input: 1200, cached_input: null, output: 10, reasoning: 0 })).toBeNull();
    expect(cacheHitPercent({ input: 0, cached_input: 0, output: 10, reasoning: 0 })).toBeNull();
  });

  it("hides footer items without a measurement", () => {
    expect(footerStats(null)).toEqual({
      ttftMs: null,
      decodeTps: null,
      cachePct: null,
      input: null,
      output: null,
    });
    const failed = turn({
      ttft_ms: 120,
      decode_ms: null,
      decode_tps: null,
      wall_tps: null,
      usage: { input: 0, cached_input: null, output: 0, reasoning: 0 },
    });
    const visible = footerStats(failed);
    expect(visible.ttftMs).toBe(120);
    expect(visible.decodeTps).toBeNull();
    expect(visible.cachePct).toBeNull();
    expect(visible.input).toBeNull();
    expect(visible.output).toBeNull();
  });

  it("prints a cost in the currency the price was published in", () => {
    expect(formatCost(6.62, "USD")).toBe("$6.62");
    // A single request is usually worth a fraction of a cent.
    expect(formatCost(0.0021, "USD")).toBe("$0.0021");
    expect(formatCost(239.5, "EUR")).toContain("239.50");
  });

});

describe("durable session projection", () => {
  it("replaces totals instead of adding repeated or out-of-order events", () => {
    showUsage(usage());
    const newer = usage({ input: 2000, output: 410, requests: 2 }, { revision: 3 });
    acceptAccounting(update(newer)); acceptAccounting(update(newer));
    acceptAccounting(update(usage({ input: 99999 }, { revision: 2 })));
    expect(aiStats.session().input).toBe(2000);
    expect(aiStats.session().turns).toBe(2);
  });
  it("ignores delayed hydration after a newer event and rejects another incarnation", () => {
    showUsage(usage());
    const read = beginUsageHydration("s1");
    acceptAccounting(update(usage({ input: 2400 }, { revision: 4 })));
    hydrateSessionUsage("s1", usage(), undefined, read);
    acceptAccounting(update(usage({ input: 99999 }, { incarnation: "old-life", revision: 500 })));
    expect(aiStats.session().input).toBe(2400);
    hydrateSessionUsage("s1", usage({ input: 7 }, { incarnation: "new-life", revision: 1 }));
    expect(aiStats.session().input).toBe(7);
  });
  it("does not let an admitted old reply replace a hydrated incarnation", () => {
    showUsage(usage());
    noteSentEstimate(100, "local-1", "s1");
    const replacement = usage({ input: 7 }, { incarnation: "replacement", last_turn: turn({ model: "new-model" }) });
    hydrateSessionUsage("s1", replacement);
    expect(acceptAccounting(update(usage({ input: 99999 }, { revision: 500 }), false), true)).toBe(false);
    recordTurn(event(usage({ input: 99999 }, { revision: 501 }), { model: "old-model" }));
    recordTurn(event(usage(), { model: "old-model", accounting: undefined }));
    expect(aiStats.snapshot()?.incarnation).toBe("replacement");
    expect(aiStats.session().input).toBe(7);
    expect(aiStats.turn()?.model).toBe("new-model");
    expect(aiStats.warning()).toBeNull();
  });
  it("allows request admission to establish an empty projection", () => {
    selectStatsSession("s1");
    expect(acceptAccounting(update(usage()), true)).toBe(true);
    expect(aiStats.snapshot()?.incarnation).toBe("life-1");
  });
  it("keeps a same-revision unsaved warning through duplicates and clears on durable advance", () => {
    showUsage(usage());
    acceptAccounting(update(usage(), false));
    acceptAccounting(update(usage()));
    expect(aiStats.warning()?.message).toBe("disk full");
    acceptAccounting(update(usage({}, { revision: 2 })));
    acceptAccounting(update(usage(), false));
    expect(aiStats.warning()).toBeNull();
    acceptAccounting(update(usage({}, { revision: 2 }), false));
    hydrateSessionUsage("s1", usage({}, { revision: 2 }));
    expect(aiStats.warning()?.message).toBe("disk full");
    hydrateSessionUsage("s1", undefined, { code: "accounting_storage", message: "read failed" });
    expect(aiStats.warning()?.message).toBe("read failed");
    expect(aiStats.session().known).toBe(true);
    hydrateSessionUsage("s1", usage({}, { revision: 2 }));
    expect(aiStats.warning()?.message).toBe("disk full");
    hydrateSessionUsage("s1", usage({}, { revision: 3 }));
    expect(aiStats.warning()).toBeNull();
  });
  it("clears a read-only failure on a recovered same-revision read", () => {
    showUsage(usage());
    hydrateSessionUsage("s1", undefined, { code: "accounting_storage", message: "read failed" });
    const read = beginUsageHydration("s1");
    hydrateSessionUsage("s1", usage(), undefined, read);
    expect(aiStats.warning()).toBeNull();
  });
  it("clears a raced unsaved warning when hydration supplies a strictly newer durable revision", () => {
    showUsage(usage());
    const read = beginUsageHydration("s1");
    acceptAccounting(update(usage(), false));
    hydrateSessionUsage("s1", usage({ input: 2000 }, { revision: 2 }), undefined, read);
    expect(aiStats.session().input).toBe(2000);
    expect(aiStats.warning()).toBeNull();
  });
  it("keeps a raced unsaved warning when hydration is not newer", () => {
    showUsage(usage({}, { revision: 2 }));
    const read = beginUsageHydration("s1");
    acceptAccounting(update(usage({}, { revision: 2 }), false));
    hydrateSessionUsage("s1", usage({}, { revision: 2 }), undefined, read);
    expect(aiStats.warning()?.message).toBe("disk full");
    hydrateSessionUsage("s1", usage(), undefined, read);
    expect(aiStats.warning()?.message).toBe("disk full");
    expect(aiStats.snapshot()?.revision).toBe(2);
  });
  it("cannot restore an older footer or calibrate a retired local UUID", () => {
    showUsage(usage());
    noteSentEstimate(100, "local-1", "s1");
    const old = event(usage(), { model: "old-model", usage: { input: 120, cached_input: 80, output: 1, reasoning: 0 } });
    recordTurn(old);
    const latest = turn({ model: "latest-model" });
    acceptAccounting(update(usage({}, { revision: 2, last_turn: latest })));
    recordTurn(old);
    expect(aiStats.turn()?.model).toBe("latest-model");
    noteSentEstimate(1000, "local-2", "s1");
    recordTurn(event(usage({}, { revision: 2, last_turn: latest }), { client_request_id: "local-1", model: "old-model" }));
    expect(aiStats.turn()?.model).toBe("latest-model");
    expect(contextMeter.reported()).toBe(120);
    expect(contextMeter.samples()).toBe(1);
  });
  it.each([false, true])("keeps the authoritative newer chat footer when an older request is acknowledged (initial event lost=%s)", lost => {
    showUsage(usage({}, { last_turn: undefined }));
    noteSentEstimate(100, "request-A", "s1");
    const rawA = { ...turn({ model: "request-A-model", request_id: "provider-A",
      usage: { input: 120, cached_input: 80, output: 1, reasoning: 0 } }),
      client_request_id: "request-A", purpose: "chat" as const,
      accounting: update(usage({}, { last_turn: undefined }), false) };
    if (!lost) {
      recordTurn(rawA);
      expect(aiStats.turn()?.model).toBe("request-A-model");
    }
    const durableB = turn({ model: "request-B-model", request_id: "provider-B" });
    acceptAccounting(update(usage({}, { revision: 2, last_turn: durableB })));
    recordTurn({ ...rawA, accounting: update(usage({}, { revision: 3, last_turn: durableB })) });
    expect(aiStats.snapshot()?.revision).toBe(3);
    expect(aiStats.turn()?.model).toBe("request-B-model");
    expect(contextMeter.samples()).toBe(1);
    recordTurn({ ...rawA, accounting: update(usage({}, { revision: 3, last_turn: durableB }), false) });
    expect(aiStats.turn()?.model).toBe("request-B-model");
    expect(contextMeter.samples()).toBe(1);
  });
  it("cancels only its exact pending hydration ticket", () => {
    showUsage(usage());
    const oldRead = beginUsageHydration("s2");
    const newRead = beginUsageHydration("s3");
    cancelUsageHydration(oldRead);
    acceptAccounting(update(usage({}, { session_id: "s3", incarnation: "life-3", revision: 2 })));
    expect(usageHydrationNeedsReconcile(newRead)).toBe(true);
    cancelUsageHydration(newRead);
    expect(usageHydrationNeedsReconcile(newRead)).toBe(false);
    expect(aiStats.activeSession()).toBe("s1");
  });
  it("keeps missing/error coverage visible and cannot contaminate another session", () => {
    selectStatsSession("s2");
    hydrateSessionUsage("s2", undefined, { code: "accounting_storage", message: "unreadable" });
    acceptAccounting(update(usage({ input: 99999 })));
    expect(aiStats.session().known).toBe(false);
    expect(aiStats.session().legacyUnknown).toBe(true);
    expect(aiStats.warning()?.message).toBe("unreadable");
    const { container } = render(() => <AiStats />);
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Not saved");
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    expect(container.querySelector('[role="tooltip"]')?.textContent).toContain("Earlier usage is unknown");
  });
  it("calibrates inclusive input once for its request and excludes titles and retired estimates", () => {
    showUsage(usage());
    noteSentEstimate(100, "local-1", "s1");
    const first = event(usage(), { usage: { input: 120, cached_input: 80, output: 10, reasoning: 0 } });
    recordTurn(first); recordTurn(first);
    expect(contextMeter.reported()).toBe(120);
    expect(contextMeter.samples()).toBe(1);
    recordTurn(event(usage({}, { revision: 2 }), { purpose: "title", model: "title-model" }));
    expect(aiStats.turn()?.model).not.toBe("title-model");
    noteSentEstimate(1000, "local-2", "s1");
    recordTurn(first);
    expect(contextMeter.reported()).toBe(120);
    resetSentEstimate(); recordTurn(event(usage()));
    expect(contextMeter.samples()).toBe(1);
  });
  it("hydrates a new fork's empty spend and keeps a title from replacing chat timing", () => {
    showUsage(usage());
    selectStatsSession("fork");
    hydrateSessionUsage("fork", usage({ requests: 0, attempts: 0, input: 0, output: 0, reasoning: 0,
      cached_input: 0, cache_input: 0, ttft_ms_sum: 0, ttft_count: 0, decode_tps_sum: 0, decode_tps_count: 0 },
      { session_id: "fork", incarnation: "fork-life", revision: 0, last_turn: undefined }));
    expect(aiStats.session().input).toBe(0);
    expect(aiStats.session().legacyUnknown).toBe(false);
    expect(aiStats.turn()).toBeNull();
  });
  it("uses separate provider-round estimates and cannot calibrate another session", () => {
    showUsage(usage());
    noteSentEstimate(100, "local-1", "s1");
    recordTurn(event(usage(), { usage: { input: 120, cached_input: 80, output: 1, reasoning: 0 } }));
    noteSentEstimate(1000, "local-2", "s1");
    recordTurn(event(usage({}, { revision: 2 }), { client_request_id: "local-2", usage: { input: 1100, cached_input: 800, output: 1, reasoning: 0 } }));
    expect(contextMeter.reported()).toBe(1100);
    expect(contextMeter.samples()).toBe(2);
    selectStatsSession("s2");
    recordTurn(event(usage({}, { revision: 3 }), { client_request_id: "local-2" }));
    expect(aiStats.session().input).toBe(0);
    expect(contextMeter.samples()).toBe(2);
  });
  it("keeps projection across presentation remount and installs one panel listener", async () => {
    showUsage(usage({ requests: 4 }));
    const first = render(() => <AiStats />); first.unmount();
    const second = render(() => <AiStats />);
    expect(aiStats.session().turns).toBe(4);
    expect(second.container.textContent).toContain("1.5k tok");
    const before = listenMock.mock.calls.filter(call => call[0] === "ai_stats").length;
    const stopA = startAiStatsSession(); const stopB = startAiStatsSession();
    await Promise.resolve();
    expect(listenMock.mock.calls.filter(call => call[0] === "ai_stats").length - before).toBe(1);
    stopA(); stopB();
    expect(aiStats.session().turns).toBe(4);
  });
});

describe("AiStats durable presentation", () => {
  it("shows exact totals, each currency, and independent coverage flags", () => {
    showUsage(usage({ input: 2000, output: 410, requests: 2, attempts: 3,
      costs: { USD: { amount: 0.01, attempts: 1 }, EUR: { amount: 2, attempts: 1 } },
      unknown_price_attempts: 1, unreported_usage_attempts: 1, interrupted_requests: 1, legacy_unknown: true }));
    const { container } = render(() => <AiStats />);
    expect(container.querySelector(".ai-stats-line")?.textContent).toContain("2.4k tok");
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    const card = container.querySelector('[role="tooltip"]');
    expect(card?.textContent).toContain("$0.01"); expect(card?.textContent).toContain("€2.00");
    expect(card?.textContent).toContain("Unknown prices: 1");
    expect(card?.textContent).toContain("Usage not reported: 1");
    expect(card?.textContent).toContain("Interrupted requests: 1");
    expect(card?.textContent).toContain("Attempts");
  });
  it("keeps one prioritized short inline status and all coverage details behind the info button", () => {
    showUsage(usage({ legacy_unknown: true, unknown_price_attempts: 1, unreported_usage_attempts: 1,
      interrupted_requests: 1 }, { pending_requests: 1 }));
    const failed = update(usage({}, { pending_requests: 1 }), false);
    // The unchanged durable snapshot must retain every independently reported flag.
    failed.snapshot = aiStats.snapshot()!;
    acceptAccounting(failed);
    aiLimits.setActiveContext(1_000_000);
    const { container } = render(() => <AiStats used={450_000} />);
    const line = container.querySelector(".ai-stats-line")!;
    expect(line.querySelectorAll('[role="status"]')).toHaveLength(1);
    expect(line.querySelector('[role="status"]')?.textContent).toBe("Not saved");
    expect(line.textContent).toContain("1.5k tok");
    expect(line.textContent).not.toContain("Unknown prices");
    expect(container.querySelector(".ai-stats-context")?.textContent).toBe("45%");
    expect(container.querySelector(".ai-stats-info")?.getAttribute("aria-label")).toBeTruthy();
    expect(container.textContent).not.toContain("accounting_storage");
    expect(container.textContent).not.toContain("disk full");
    fireEvent.focusIn(container.querySelector(".ai-stats-info") as HTMLElement);
    const card = container.querySelector('[role="tooltip"]')!;
    for (const detail of ["Usage was not saved", "Earlier usage is unknown", "Unknown prices: 1",
      "Usage not reported: 1", "Interrupted requests: 1", "Pending requests: 1"]) expect(card.textContent).toContain(detail);
    expect(card.textContent).toContain("Technical details");
    expect(card.textContent).toContain("accounting_storage: disk full");
    expect(card.textContent).toContain("Usage data could not be accessed. Check disk space and file access.");
  });
  it("opens with chat timings on focus and closes on Escape", async () => {
    showUsage(usage());
    const { container } = render(() => <AiStats />);
    fireEvent.focusIn(container.querySelector(".ai-stats-info") as HTMLElement);
    expect(container.querySelector('[role="tooltip"]')?.textContent).toContain("Decode TPS");
    fireEvent.keyDown(window, { key: "Escape" });
    await vi.waitFor(() => expect(container.querySelector('[role="tooltip"]')).toBeFalsy());
  });
  it("keeps the first hover-focus-click open and lets the next click close it", () => {
    showUsage(usage());
    const { container } = render(() => <AiStats />);
    const anchor = container.querySelector(".ai-stats-anchor") as HTMLElement;
    const button = container.querySelector(".ai-stats-info") as HTMLElement;
    fireEvent.mouseEnter(anchor);
    fireEvent.focusIn(button);
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector('[role="tooltip"]')).toBeTruthy();
    fireEvent.mouseLeave(anchor);
    expect(container.querySelector('[role="tooltip"]')).toBeTruthy();
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector('[role="tooltip"]')).toBeFalsy();
  });
  it("dismisses pinned details on outside press or keyboard navigation", () => {
    showUsage(usage());
    const { container } = render(() => <><AiStats /><button data-outside>Outside</button></>);
    const button = container.querySelector(".ai-stats-info") as HTMLElement;
    const outside = container.querySelector("[data-outside]") as HTMLElement;
    fireEvent.focusIn(button);
    fireEvent.click(button, { detail: 0 });
    fireEvent.pointerDown(outside);
    expect(container.querySelector('[role="tooltip"]')).toBeFalsy();
    fireEvent.click(button, { detail: 0 });
    expect(container.querySelector('[role="tooltip"]')).toBeTruthy();
    fireEvent.click(outside, { detail: 0 });
    expect(container.querySelector('[role="tooltip"]')).toBeFalsy();
    fireEvent.click(button, { detail: 0 });
    fireEvent.focusOut(button, { relatedTarget: outside });
    expect(container.querySelector('[role="tooltip"]')).toBeFalsy();
  });
  it("shows session aggregates only in their session section", () => {
    showUsage(usage());
    const { container } = render(() => <AiStats />);
    fireEvent.focusIn(container.querySelector(".ai-stats-info") as HTMLElement);
    const labels = [...container.querySelectorAll(".ai-stats-card .ai-stats-label")].map(node => node.textContent);
    expect(labels.filter(label => label === i18n.t("ai.stats.requests"))).toHaveLength(1);
    expect(labels.filter(label => label === i18n.t("ai.stats.sessionTps"))).toHaveLength(1);
    const turnRows = container.querySelector(".ai-stats-card .ai-stats-rows")!;
    expect(turnRows.textContent).toContain("fixture-model");
    expect(turnRows.textContent).not.toContain(i18n.t("ai.stats.requests"));
  });
  it("allows keyboard focus in scrollable details and restores it on Escape", () => {
    showUsage(usage());
    const { container } = render(() => <AiStats />);
    const button = container.querySelector(".ai-stats-info") as HTMLButtonElement;
    button.focus();
    fireEvent.click(button, { detail: 0 });
    const card = container.querySelector('[role="tooltip"]') as HTMLElement;
    card.focus();
    expect(document.activeElement).toBe(card);
    expect(container.querySelector('[role="tooltip"]')).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(document.activeElement).toBe(button);
    expect(container.querySelector('[role="tooltip"]')).toBeFalsy();
  });
  it("keeps context readable before any accounting", () => {
    aiLimits.setActiveContext(1_000_000);
    const { container } = render(() => <AiStats used={450_000} />);
    expect(container.querySelector(".ai-stats-context")?.textContent).toBe("45%");
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    expect(container.querySelector('[role="tooltip"]')?.textContent).toContain("450000 / 1000000 tokens");
  });
  it("renders no empty row before a conversation exists", () => {
    const { container } = render(() => <AiStats />);
    expect(container.querySelector(".ai-stats")).toBeFalsy();
  });
  it("renders every new status in each supported locale", () => {
    showUsage(usage({ legacy_unknown: true }));
    for (const locale of ["en", "zh-Hans", "zh-Hant", "ja", "es"] as const) {
      i18n.setLocale(locale);
      const key = "ai.stats.legacyUnknown" as I18nKey;
      expect(i18n.t(key)).not.toBe(key);
    }
  });
});

describe("composer layout", () => {
  it("renders the stats line after the input bar", async () => {
    const { container } = render(() => <AiPanel />);
    showUsage(usage());
    const line = await vi.waitFor(() => {
      const element = container.querySelector(".ai-stats") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(follows(line, container.querySelector(".ai-input-bar") as HTMLElement)).toBe(true);
  });

  it("reports a settings write that failed, and stays quiet when it works", async () => {
    const { container } = render(() => <AiPanel />);
    // A settings change is written without ceremony, and a good write says nothing.
    await vi.waitFor(() => expect(container.querySelector(".ai-save-notice")).toBeFalsy());
    aiConfig.updateProvider(getActiveProvider().id, { temperature: 0.7 });
    await expect(aiConfig.flushSave()).resolves.toBe(true);
    expect(container.querySelector(".ai-save-notice")).toBeFalsy();

    // A failed write is the one case that shows a notice, and it names the cause.
    const original = invokeMock.getMockImplementation();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "save_ai_config") throw new Error("the store is locked");
      return original?.(command, {} as never);
    });
    await expect(aiConfig.flushSave()).resolves.toBe(false);
    const notice = await vi.waitFor(() => {
      const element = container.querySelector(".ai-save-notice") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(notice.textContent).toContain("the store is locked");
    expect(follows(notice, container.querySelector(".ai-input-bar") as HTMLElement)).toBe(true);
    if (original) invokeMock.mockImplementation(original);
    await aiConfig.flushSave();
  });
});

describe("attach button", () => {
  it("shows a paperclip and names the accepted formats in its tooltip", async () => {
    const { container } = render(() => <><AiPanel /><TooltipLayer /></>);
    const button = container.querySelector(".ai-attach-btn") as HTMLButtonElement;
    expect(button.getAttribute("aria-label")).toBe("Attach a file");
    expect(button.textContent?.trim()).toBe("");
    expect(button.querySelector("svg")).toBeTruthy();

    fireEvent.focusIn(button);
    const tip = await vi.waitFor(() => {
      const element = document.querySelector(".oc-tooltip") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(tip.textContent).toContain(MEDIA_EXTENSIONS.join(", "));
  });
});

describe("stats i18n", () => {
  const CJK = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/;
  const keys = (): string[] => i18n.keys().filter(key => key.startsWith("ai.stats."));

  it("keeps every stats key translated in both Chinese locales and English free of CJK", () => {
    const statsKeys = keys();
    expect(statsKeys.length).toBeGreaterThanOrEqual(25);
    i18n.setLocale("en");
    for (const key of statsKeys) {
      const statsKey = key as I18nKey;
      expect(i18n.has("zh-Hans", statsKey), `zh-Hans ${key}`).toBe(true);
      expect(i18n.has("zh-Hant", statsKey), `zh-Hant ${key}`).toBe(true);
      const text = i18n.t(statsKey);
      expect(text.trim(), key).not.toBe("");
      expect(CJK.test(text), `en ${key}: ${text}`).toBe(false);
    }
  });
});

describe("context breakdown and compaction trigger", () => {
  it("names each part of the next request and compacts at the threshold", () => {
    aiLimits.setActiveContext(1000);
    const onCompact = vi.fn();
    const { container } = render(() => (
      <AiStats
        used={800}
        breakdown={{
          system: 100,
          tools: 50,
          history: 300,
          project: 200,
          draft: 100,
          attachments: 2,
          attachmentBytes: 2048,
        }}
        onCompact={onCompact}
      />
    ));
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    const card = container.querySelector('[role="tooltip"]');
    expect(card?.textContent).toContain("System prompt");
    expect(card?.textContent).toContain("Tool schemas");
    expect(card?.textContent).toContain("Conversation");
    expect(card?.textContent).toContain("Project text");
    expect(card?.textContent).toContain("Draft");
    expect(card?.textContent).toContain("2 · 2.0 KiB");
    expect(card?.textContent).toContain("At the 75% compaction threshold");
    const compact = [...(card?.querySelectorAll("button") ?? [])]
      .find(button => button.textContent?.includes("Compact now"));
    expect(compact).toBeTruthy();
    fireEvent.click(compact!);
    expect(onCompact).toHaveBeenCalledOnce();
  });

  it("lets the operator change the compaction threshold", () => {
    aiLimits.setActiveContext(1000);
    const onCompactAtChange = vi.fn();
    const { container } = render(() => (
      <AiStats used={800} compactAt={0.5} onCompactAtChange={onCompactAtChange} />
    ));
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    const input = container.querySelector("#ai-compact-at") as HTMLInputElement;
    expect(input.value).toBe("50");
    fireEvent.change(input, { target: { value: "60" } });
    expect(onCompactAtChange).toHaveBeenCalledWith(0.6);
  });

  it("does not offer compaction below the threshold", () => {
    aiLimits.setActiveContext(1000);
    const { container } = render(() => (
      <AiStats used={500} breakdown={{ system: 1, tools: 1, history: 1, project: 1, draft: 1, attachments: 0, attachmentBytes: 0 }} onCompact={() => {}} />
    ));
    fireEvent.mouseEnter(container.querySelector(".ai-stats-anchor") as HTMLElement);
    expect(container.querySelector('[role="tooltip"]')?.textContent).not.toContain("Compact now");
  });
});
