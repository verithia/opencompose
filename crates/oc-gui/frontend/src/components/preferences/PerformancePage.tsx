/**
 * The Performance page: the offline renderer's worker count and whether track
 * rendering runs in parallel, plus the read-only facts those choices act on.
 *
 * The worker count is a real setting: it is persisted and published to the
 * engine, which reads it at the start of every offline render.
 */
import { For, Show, createMemo, onMount } from "solid-js";
import { i18n } from "../../stores/i18n";
import { performanceStore } from "../../stores/performance";

export function PerformancePage() {
  // The page owns the read; a second mount after a page switch reads again.
  onMount(() => void performanceStore.load());

  const facts = () => performanceStore.facts();
  const cores = () => Math.max(1, facts()?.cores ?? 1);
  const threadOptions = createMemo(() =>
    Array.from({ length: Math.min(cores(), 64) }, (_, index) => index + 1),
  );

  const milliseconds = (frames: number): string => {
    const rate = facts()?.sample_rate ?? 0;
    if (rate <= 0) return "—";
    return `${((frames / rate) * 1000).toFixed(2)} ms`;
  };

  const firstBlockText = () => {
    const block = facts()?.first_block;
    if (!block) return i18n.t("preferences.performance.firstBlockNone");
    const ms = block.latency_ms ?? block.since_start_ms;
    return i18n.t("preferences.performance.firstBlockValue", {
      ms: ms.toFixed(1),
      frames: block.frames_computed,
      path: block.path,
    });
  };

  const save = (next: { renderThreads?: number | null; parallel?: boolean }) => {
    const current = facts();
    if (!current) return;
    void performanceStore.save({
      render_threads: next.renderThreads === undefined ? current.render_threads : next.renderThreads,
      parallel_tracks: next.parallel === undefined ? current.parallel_tracks : next.parallel,
    });
  };

  return (
    <>
      <Show
        when={facts()}
        fallback={
          <p class="preferences-hint">
            {performanceStore.error() ?? i18n.t("preferences.performance.loading")}
          </p>
        }
      >
        {(loaded) => (
          <>
            <div class="preferences-field">
              <label class="preferences-field-label" for="preferences-render-threads">
                {i18n.t("preferences.performance.threads")}
              </label>
              <select
                id="preferences-render-threads"
                class="theme-select preferences-render-threads"
                aria-label={i18n.t("preferences.performance.threads")}
                value={loaded().render_threads === null ? "auto" : String(loaded().render_threads)}
                disabled={performanceStore.saving()}
                onChange={(event) =>
                  save({
                    renderThreads:
                      event.currentTarget.value === "auto"
                        ? null
                        : Number(event.currentTarget.value),
                  })
                }
              >
                <option value="auto">
                  {i18n.t("preferences.performance.auto", { cores: loaded().cores })}
                </option>
                <For each={threadOptions()}>
                  {(count) => (
                    <option value={String(count)}>
                      {i18n.t("preferences.performance.threadCount", { count })}
                    </option>
                  )}
                </For>
              </select>
              <p class="preferences-hint">
                {i18n.t("preferences.performance.effective", {
                  workers: loaded().effective_workers,
                  cores: loaded().cores,
                })}
              </p>
            </div>

            <div class="midi-monitor-row">
              <label class="midi-monitor-label">
                <input
                  type="checkbox"
                  class="preferences-parallel-tracks"
                  checked={loaded().parallel_tracks}
                  disabled={performanceStore.saving()}
                  onChange={(event) => save({ parallel: event.currentTarget.checked })}
                />
                <span>{i18n.t("preferences.performance.parallel")}</span>
              </label>
            </div>
            <p class="preferences-hint">{i18n.t("preferences.performance.parallelHint")}</p>

            <Show when={performanceStore.error()}>
              {(message) => (
                <p class="preferences-error selectable">
                  {i18n.t("preferences.performance.saveFailed", { error: message() })}
                </p>
              )}
            </Show>

            <div class="preferences-divider" />

            <details class="preferences-engine-diagnostics">
              <summary>{i18n.t("preferences.performance.diagnostics")}</summary>
            <section class="preferences-facts" aria-label={i18n.t("preferences.page.performance")}>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.cores")}</span>
                <span class="preferences-fact-value preferences-cores">{loaded().cores}</span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.lookahead")}</span>
                <span class="preferences-fact-value preferences-lookahead">
                  {i18n.t("preferences.performance.frames", {
                    frames: loaded().lookahead_frames,
                    ms: milliseconds(loaded().lookahead_frames),
                  })}
                </span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.block")}</span>
                <span class="preferences-fact-value preferences-block">
                  {i18n.t("preferences.performance.frames", {
                    frames: loaded().block_frames,
                    ms: milliseconds(loaded().block_frames),
                  })}
                </span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.firstChunk")}</span>
                <span class="preferences-fact-value preferences-first-chunk">
                  {i18n.t("preferences.performance.frames", {
                    frames: loaded().first_chunk_frames,
                    ms: milliseconds(loaded().first_chunk_frames),
                  })}
                </span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.sampleRate")}</span>
                <span class="preferences-fact-value preferences-sample-rate">
                  {`${loaded().sample_rate} Hz`}
                </span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.firstBlock")}</span>
                <span class="preferences-fact-value preferences-first-block">
                  {firstBlockText()}
                </span>
              </div>
              <div class="preferences-fact">
                <span class="preferences-fact-label">{i18n.t("preferences.performance.budget")}</span>
                <span class="preferences-fact-value preferences-first-block-budget">
                  {i18n.t("preferences.performance.budgetValue", {
                    ms: loaded().first_block_budget_ms,
                  })}
                </span>
              </div>
            </section>
            </details>
          </>
        )}
      </Show>
    </>
  );
}
