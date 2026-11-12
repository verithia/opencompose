/**
 * Compact session stats line under the AI panel's composer, with a hover/focus
 * detail card. The line is one muted row; only the info button is focusable.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { formatBytes } from "../ai/media";
import { accountingErrorMessage } from "../ai/errors";
import { i18n } from "../stores/i18n";
import { aiLimits } from "../stores/aiLimits";
import {
  aiStats,
  averageTtft,
  averageTps,
  formatCost,
  formatDuration,
  formatTokens,
  formatTps,
  sessionCachePercent,
} from "../stores/aiStats";
import { useTooltip } from "./Tooltip";

interface DetailRow {
  label: string;
  value: string;
  note?: string;
}

const DETAIL_ID = "ai-stats-detail";
/** The default fraction of the model window that offers manual compaction. */
export const COMPACT_AT = 0.75;

/** Context spent by each part of the next request. */
export interface ContextBreakdown {
  system: number;
  tools: number;
  history: number;
  project: number;
  draft: number;
  attachments: number;
  attachmentBytes: number;
}

export function AiStats(props: {
  used?: number;
  breakdown?: ContextBreakdown;
  compactAt?: number;
  onCompactAtChange?: (fraction: number) => void;
  onCompact?: () => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [pinned, setPinned] = createSignal(false);
  let anchor: HTMLDivElement | undefined;
  let infoButton: HTMLButtonElement | undefined;
  const closeCard = (restoreFocus = false) => {
    if (restoreFocus && anchor?.contains(document.activeElement)) infoButton?.focus();
    setPinned(false); setOpen(false);
  };

  const turn = aiStats.turn;
  const session = aiStats.session;

  /**
   * The line carries the number the user actually watches: tokens spent. Turns,
   * speed, time-to-first-token and cache hit live in the hover card, so a narrow
   * panel keeps one readable figure instead of six cramped ones.
   */
  const lineItems = createMemo(() => {
    const stats = session();
    const tokens = stats.input + stats.output;
    return tokens > 0 ? [`${formatTokens(tokens)} ${i18n.t("ai.stats.tokenUnit")}`] : [];
  });

  /** The prompt estimate over the shown model's context limit, when known. */
  const budget = createMemo(() => aiLimits.contextBudget(props.used ?? 0, aiLimits.activeContext()));
  const contextPercent = createMemo(() => {
    const shown = budget();
    return shown ? `${Math.round((shown.used / shown.context) * 100)}%` : null;
  });
  const contextTooltip = () => {
    const shown = budget();
    return shown
      ? i18n.t("ai.limits.budget", {
        used: shown.used,
        context: shown.context,
        cpt: aiLimits.CHARACTERS_PER_TOKEN,
      })
      : null;
  };
  const contextTip = useTooltip(contextTooltip);
  const contextRow = createMemo<DetailRow | null>(() => {
    const shown = budget();
    if (!shown) return null;
    return {
      label: i18n.t("ai.stats.used"),
      value: i18n.t("ai.stats.contextUsed", { used: shown.used, context: shown.context }),
      note: i18n.t("ai.stats.contextEstimate", { cpt: aiLimits.CHARACTERS_PER_TOKEN }),
    };
  });
  const compactAt = () => props.compactAt ?? COMPACT_AT;
  const contextOver = createMemo(() => {
    const shown = budget();
    return shown !== null && shown.used / shown.context >= compactAt();
  });
  const breakdownRows = createMemo<DetailRow[]>(() => {
    const parts = props.breakdown;
    if (!parts) return [];
    const rows: DetailRow[] = [
      { label: i18n.t("ai.stats.contextSystem"), value: formatTokens(parts.system) },
      { label: i18n.t("ai.stats.contextTools"), value: formatTokens(parts.tools) },
      { label: i18n.t("ai.stats.contextHistory"), value: formatTokens(parts.history) },
      { label: i18n.t("ai.stats.contextProject"), value: formatTokens(parts.project) },
      { label: i18n.t("ai.stats.contextDraft"), value: formatTokens(parts.draft) },
    ];
    if (parts.attachments > 0) {
      rows.push({
        label: i18n.t("ai.stats.contextAttachments"),
        value: `${parts.attachments} · ${formatBytes(parts.attachmentBytes)}`,
      });
    }
    return rows;
  });
  const usageNotes = createMemo(() => {
    const total = session();
    const notes: string[] = [];
    if (aiStats.warning()) notes.push(i18n.t("ai.stats.unsaved"));
    if (total.legacyUnknown) notes.push(i18n.t("ai.stats.legacyUnknown"));
    if (total.unknownPrice > 0) notes.push(i18n.t("ai.stats.unknownPrice", { count: total.unknownPrice }));
    if (total.unreported > 0) notes.push(i18n.t("ai.stats.unreported", { count: total.unreported }));
    if (total.interrupted > 0) notes.push(i18n.t("ai.stats.interrupted", { count: total.interrupted }));
    if (total.pending > 0) notes.push(i18n.t("ai.stats.pending", { count: total.pending }));
    return notes;
  });
  const inlineStatus = createMemo(() => {
    const total = session();
    if (aiStats.warning()) return i18n.t("ai.stats.shortUnsaved");
    if (total.unreported > 0) return i18n.t("ai.stats.shortUnreported");
    if (total.interrupted > 0) return i18n.t("ai.stats.shortInterrupted");
    if (total.legacyUnknown) return i18n.t("ai.stats.shortUnknown");
    if (total.unknownPrice > 0) return i18n.t("ai.stats.shortPrice");
    if (total.pending > 0) return i18n.t("ai.stats.shortPending");
    return null;
  });
  const hasLine = createMemo(() => lineItems().length > 0 || contextPercent() !== null || usageNotes().length > 0);

  const detailRows = createMemo<DetailRow[]>(() => {
    const stats = turn();
    if (!stats) return [];
    const rows: DetailRow[] = [
      { label: i18n.t("ai.stats.model"), value: stats.model },
      { label: i18n.t("ai.stats.wire"), value: stats.wire },
    ];
    if (stats.failed) {
      rows.push({ label: i18n.t("ai.stats.status"), value: i18n.t("ai.stats.failed") });
    }
    if (stats.ttft_ms !== null) {
      rows.push({ label: i18n.t("ai.stats.ttft"), value: formatDuration(stats.ttft_ms) });
    }
    if (stats.decode_ms !== null) {
      rows.push({
        label: i18n.t("ai.stats.decodeWindow"),
        value: formatDuration(stats.decode_ms),
      });
    }
    if (stats.decode_tps !== null && stats.decode_ms !== null) {
      rows.push({
        label: i18n.t("ai.stats.decodeTps"),
        value: `${formatTps(stats.decode_tps)} ${i18n.t("ai.stats.tpsUnit")}`,
        note: i18n.t("ai.stats.decodeTpsDef"),
      });
    }
    if (stats.wall_tps !== null) {
      rows.push({
        label: i18n.t("ai.stats.wallTps"),
        value: `${formatTps(stats.wall_tps)} ${i18n.t("ai.stats.tpsUnit")}`,
        note: i18n.t("ai.stats.wallTpsDef"),
      });
    }
    if (stats.usage.input > 0) {
      const cached = stats.usage.cached_input;
      const fresh = cached === null ? stats.usage.input : Math.max(0, stats.usage.input - cached);
      const parts = [i18n.t("ai.stats.fresh", { value: formatTokens(fresh) })];
      if (cached !== null) parts.push(i18n.t("ai.stats.cached", { value: formatTokens(cached) }));
      rows.push({
        label: i18n.t("ai.stats.input"),
        value: `${formatTokens(stats.usage.input)} · ${parts.join(" · ")}`,
      });
    }
    if (stats.usage.output > 0) {
      const thoughts = stats.usage.reasoning;
      const visible = Math.max(0, stats.usage.output - thoughts);
      const parts = [i18n.t("ai.stats.visible", { value: formatTokens(visible) })];
      if (thoughts > 0) parts.push(i18n.t("ai.stats.thoughts", { value: formatTokens(thoughts) }));
      rows.push({
        label: i18n.t("ai.stats.output"),
        value: `${formatTokens(stats.usage.output)} · ${parts.join(" · ")}`,
      });
    }
    if (stats.wall_ms > 0) {
      rows.push({ label: i18n.t("ai.stats.wall"), value: formatDuration(stats.wall_ms) });
    }
    rows.push({ label: i18n.t("ai.stats.retries"), value: `${stats.retries}` });
    if (stats.request_id) {
      rows.push({ label: i18n.t("ai.stats.requestId"), value: stats.request_id });
    }
    if (stats.cost) {
      rows.push({
        label: i18n.t("ai.stats.cost"),
        value: formatCost(stats.cost.amount, stats.cost.currency),
      });
    }
    return rows;
  });

  const sessionRows = createMemo(() => {
    const stats = session();
    const rows: DetailRow[] = stats.known ? [
      { label: i18n.t("ai.stats.requests"), value: `${stats.turns}` },
      { label: i18n.t("ai.stats.attempts"), value: `${stats.attempts}` },
    ] : [];
    if (stats.input > 0 || stats.output > 0) {
      rows.push({
        label: i18n.t("ai.stats.sessionTokens"),
        value: `${formatTokens(stats.input)} / ${formatTokens(stats.output)}`,
      });
    }
    const ttft = averageTtft(stats);
    if (ttft !== null) {
      rows.push({ label: i18n.t("ai.stats.sessionTtft"), value: formatDuration(ttft) });
    }
    const tps = averageTps(stats);
    if (tps !== null) {
      rows.push({
        label: i18n.t("ai.stats.sessionTps"),
        value: `${formatTps(tps)} ${i18n.t("ai.stats.tpsUnit")}`,
      });
    }
    const cache = sessionCachePercent(stats);
    if (cache !== null) {
      rows.push({ label: i18n.t("ai.stats.sessionCache"), value: `${cache} %` });
    }
    for (const [currency, cost] of Object.entries(stats.costs)) {
      rows.push({ label: `${i18n.t("ai.stats.cost")} (${currency})`, value: formatCost(cost.amount, currency) });
    }
    for (const note of usageNotes()) rows.push({ label: i18n.t("ai.stats.coverage"), value: note });
    const failure = aiStats.warning();
    if (failure) {
      const localized = accountingErrorMessage(failure);
      if (localized) rows.push({ label: i18n.t("ai.stats.coverage"), value: localized });
      rows.push({ label: i18n.t("ai.stats.technicalDetails"), value: `${failure.code}${failure.message ? `: ${failure.message}` : ""}` });
    }
    return rows;
  });

  // Escape dismisses the card from anywhere while it is open.
  createEffect(() => {
    if (!open()) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeCard(true);
    };
    const onOutsideInteraction = (event: Event) => {
      if (!(event.target instanceof Node) || !anchor?.contains(event.target)) closeCard();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onOutsideInteraction, true);
    // Accessibility activation can dispatch click without a pointer sequence.
    window.addEventListener("click", onOutsideInteraction, true);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onOutsideInteraction, true);
      window.removeEventListener("click", onOutsideInteraction, true);
    });
  });

  const closeWhenFocusLeaves = (event: FocusEvent & { currentTarget: HTMLElement }) => {
    const next = event.relatedTarget as Node | null;
    if ((!next && !pinned()) || (next && !event.currentTarget.contains(next))) closeCard();
  };

  return (
    <Show when={hasLine()}>
      <div class="ai-stats">
        <div class="ai-stats-line">
          <Show when={inlineStatus()}>
            <span class="ai-stats-note ai-stats-status" role="status">{inlineStatus()}</span>
          </Show>
          <For each={lineItems()}>
            {(item, index) => (
              <>
                <Show when={index() > 0}>
                  <span class="ai-stats-sep">·</span>
                </Show>
                <span class="ai-stats-item">{item}</span>
              </>
            )}
          </For>
        </div>
        {/* The context figure is pinned: the session items clip first in a narrow panel. */}
        <Show when={contextPercent()}>
          {(percent) => (
            <>
              <Show when={lineItems().length > 0}>
                <span class="ai-stats-sep">·</span>
              </Show>
              <span class="ai-stats-item ai-stats-context" {...contextTip}>{percent()}</span>
            </>
          )}
        </Show>
        <div
          ref={anchor}
          class="ai-stats-anchor"
          onMouseEnter={() => setOpen(true)}
          onMouseLeave={() => { if (!pinned()) setOpen(false); }}
          onFocusIn={() => setOpen(true)}
          onFocusOut={closeWhenFocusLeaves}
        >
          <button
            ref={infoButton}
            type="button"
            class="ai-stats-info"
            aria-label={i18n.t("ai.stats.info")}
            aria-expanded={open()}
            aria-describedby={open() ? DETAIL_ID : undefined}
            onClick={() => {
              const next = !pinned();
              setPinned(next); setOpen(next);
            }}
          >
            i
          </button>
          <Show when={open() && (turn() || contextRow() || sessionRows().length > 0 || breakdownRows().length > 0)}>
            <div class="ai-stats-card" role="tooltip" id={DETAIL_ID} tabIndex={0} aria-label={i18n.t("ai.stats.info")}>
              <Show when={turn()}>
                <div class="ai-stats-section">{i18n.t("ai.stats.turn")}</div>
                <div class="ai-stats-rows">
                  <For each={detailRows()}>
                    {row => (
                      <>
                        <span class="ai-stats-label">{row.label}</span>
                        <span class="ai-stats-value">
                          {row.value}
                          <Show when={row.note}>
                            <span class="ai-stats-note">{row.note}</span>
                          </Show>
                        </span>
                      </>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={sessionRows().length > 0}>
                <div class="ai-stats-section">{i18n.t("ai.stats.session")}</div>
                <div class="ai-stats-rows">
                  <For each={sessionRows()}>
                    {row => (
                      <>
                        <span class="ai-stats-label">{row.label}</span>
                        <span class="ai-stats-value">
                          {row.value}
                          <Show when={row.note}>
                            <span class="ai-stats-note">{row.note}</span>
                          </Show>
                        </span>
                      </>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={contextRow()}>
                {(row) => (
                  <>
                    <div class="ai-stats-section">{i18n.t("ai.stats.context")}</div>
                    <div class="ai-stats-rows">
                      <span class="ai-stats-label">{row().label}</span>
                      <span class="ai-stats-value">
                        {row().value}
                        <Show when={row().note}>
                          <span class="ai-stats-note">{row().note}</span>
                        </Show>
                      </span>
                    </div>
                    <For each={breakdownRows()}>
                      {part => (
                        <>
                          <span class="ai-stats-label">{part.label}</span>
                          <span class="ai-stats-value">{part.value}</span>
                        </>
                      )}
                    </For>
                    <div class="ai-stats-compact">
                      <label class="ai-stats-note" for="ai-compact-at">{i18n.t("ai.stats.compactAt")}</label>
                      <input
                        id="ai-compact-at"
                        type="number"
                        min="50"
                        max="95"
                        step="5"
                        value={Math.round(compactAt() * 100)}
                        onChange={(event) => {
                          const percent = Number(event.currentTarget.value);
                          if (Number.isFinite(percent) && percent >= 50 && percent <= 95) {
                            props.onCompactAtChange?.(percent / 100);
                          }
                        }}
                      />
                      <span class="ai-stats-note">%</span>
                      <Show when={contextOver()}>
                        <span class="ai-stats-note">
                          {i18n.t("ai.stats.contextOver", { percent: Math.round(compactAt() * 100) })}
                        </span>
                      </Show>
                      <Show when={contextOver() && props.onCompact}>
                        <button
                          type="button"
                          class="toolbar-btn text-btn"
                          onClick={() => props.onCompact?.()}
                        >{i18n.t("ai.stats.compactNow")}</button>
                      </Show>
                    </div>
                  </>
                )}
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </Show>
  );
}
