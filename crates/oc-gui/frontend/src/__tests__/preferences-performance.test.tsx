/**
 * Performance page: the render-thread setting is persisted through the backend,
 * the parallel switch maps to one worker, and the read-only facts come from the
 * snapshot the backend answers.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { PerformancePage } from "../components/preferences/PerformancePage";
import { performanceStore } from "../stores/performance";
import { i18n } from "../stores/i18n";
import type { PerformanceFacts } from "../stores/api";

function facts(overrides: Partial<PerformanceFacts> = {}): PerformanceFacts {
  return {
    app_version: "0.1.0",
    dsl_version: "0.2",
    cores: 8,
    render_threads: null,
    parallel_tracks: true,
    effective_workers: 8,
    lookahead_frames: 12000,
    first_chunk_frames: 1024,
    block_frames: 4096,
    sample_rate: 48000,
    first_block_budget_ms: 50,
    first_block: null,
    ...overrides,
  };
}

let saved: { render_threads: number | null; parallel_tracks: boolean } | null = null;

function mockFacts(initial: PerformanceFacts = facts()) {
  let current = initial;
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "performance_facts") return current;
    if (cmd === "set_performance_settings") {
      saved = (args as { settings: typeof saved })!.settings as typeof saved;
      current = {
        ...current,
        render_threads: saved!.render_threads,
        parallel_tracks: saved!.parallel_tracks,
        effective_workers: saved!.parallel_tracks ? (saved!.render_threads ?? current.cores) : 1,
      };
      return current;
    }
    return undefined;
  });
  return () => current;
}

async function renderPage() {
  const rendered = render(() => <PerformancePage />);
  await vi.waitFor(() =>
    expect(rendered.container.querySelector(".preferences-render-threads")).toBeTruthy(),
  );
  return rendered;
}

beforeEach(() => {
  i18n.setLocale("en");
  performanceStore.reset();
  saved = null;
  invokeMock.mockReset();
  mockFacts();
});

afterEach(() => cleanup());

describe("the Performance page", () => {
  it("shows the thread setting and the facts beside it", async () => {
    const { container } = await renderPage();

    const select = container.querySelector(".preferences-render-threads") as HTMLSelectElement;
    expect(select.value).toBe("auto");
    expect(select.options[0].textContent).toBe(i18n.t("preferences.performance.auto", { cores: 8 }));
    expect(container.querySelector(".preferences-cores")?.textContent).toBe("8");
    expect(container.querySelector(".preferences-block")?.textContent).toContain("4096 frames");
    expect(container.querySelector(".preferences-block")?.textContent).toContain("85.33 ms");
    expect(container.querySelector(".preferences-sample-rate")?.textContent).toBe("48000 Hz");
    expect(container.querySelector(".preferences-first-block")?.textContent).toBe(
      i18n.t("preferences.performance.firstBlockNone"),
    );
    expect(container.querySelector(".preferences-first-block-budget")?.textContent).toBe(
      i18n.t("preferences.performance.budgetValue", { ms: 50 }),
    );
  });

  it("persists a worker count and reports the effective count", async () => {
    const { container } = await renderPage();

    fireEvent.change(container.querySelector(".preferences-render-threads")!, {
      target: { value: "3" },
    });

    await vi.waitFor(() => expect(saved).toEqual({ render_threads: 3, parallel_tracks: true }));
    await vi.waitFor(() =>
      expect(container.textContent).toContain(
        i18n.t("preferences.performance.effective", { workers: 3, cores: 8 }),
      ),
    );
  });

  it("turns parallel track rendering off, which leaves one worker", async () => {
    const { container } = await renderPage();

    fireEvent.click(container.querySelector(".preferences-parallel-tracks")!);

    await vi.waitFor(() => expect(saved).toEqual({ render_threads: null, parallel_tracks: false }));
    await vi.waitFor(() =>
      expect(container.textContent).toContain(
        i18n.t("preferences.performance.effective", { workers: 1, cores: 8 }),
      ),
    );
  });

  it("keeps engine counters behind a closed diagnostics disclosure", async () => {
    const { container } = await renderPage();
    const details = container.querySelector<HTMLDetailsElement>("details.preferences-engine-diagnostics")!;
    expect(details.open).toBe(false);
    expect(details.querySelector("summary")).toHaveTextContent("Engine diagnostics");
    expect(details.querySelector(".preferences-block")).toHaveTextContent("4096 frames");
    expect(details.contains(container.querySelector(".preferences-render-threads"))).toBe(false);
  });

  it("reports the last first block with its start path", async () => {
    performanceStore.reset();
    invokeMock.mockReset();
    mockFacts(
      facts({
        first_block: {
          path: "play",
          seq: 3,
          delivered: true,
          latency_ms: 2.4,
          since_start_ms: 2.4,
          block_frames: 512,
          frames_computed: 1536,
          blocks_computed: 3,
          queued_frames: 0,
          splice_ms: null,
        },
      }),
    );
    const { container } = await renderPage();

    expect(container.querySelector(".preferences-first-block")?.textContent).toBe(
      i18n.t("preferences.performance.firstBlockValue", {
        ms: "2.4",
        frames: 1536,
        path: "play",
      }),
    );
  });
});
