/**
 * The History tab: the open project's edit history, newest first, loaded a
 * page (64 entries) at a time. Clicking an entry moves the history cursor
 * there; the header shows the space the history occupies and clears older
 * entries on request.
 */
import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import * as api from "../stores/api";
import type { UndoHistoryEntry, UndoScope } from "../stores/api";
import { authorName, entryText as historyEntryText, sizeText } from "../stores/historyText";
import { historyStorage } from "../stores/historyStorage";
import "./HistoryPanel.css";

/** What "Clear older history" keeps unless the person types another count. */
export const DEFAULT_KEEP = 1000;

function scopeLabel(scope: UndoScope): string {
  return i18n.t(`history.scope.${scope}` as Parameters<typeof i18n.t>[0]);
}

/** An entry's time in the app's language, not the system's. */
export function formatTimestamp(timestamp: number): string {
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
  return new Date(timestamp).toLocaleString(i18n.locale(), {
    month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function entryText(entry: UndoHistoryEntry): string {
  return historyEntryText(entry, { properties: store.project.properties });
}

export function HistoryPanel() {
  const [keep, setKeep] = createSignal(DEFAULT_KEEP);
  const [clearing, setClearing] = createSignal(false);
  const [report, setReport] = createSignal<string | null>(null);

  onMount(() => {
    onCleanup(store.watchHistory());
    void historyStorage.refresh();
  });
  // The size follows the history: a new entry or a reopened project moves it.
  createEffect(on(() => store.historyView().total, () => void historyStorage.refresh(), { defer: true }));

  const view = () => store.historyView();
  const hasOlder = () => (view().entries.at(-1)?.index ?? 1) > 1;
  const droppable = () => Math.max(0, Math.min(view().applied, view().total - keep()));

  async function clearOlder() {
    const count = droppable();
    if (count <= 0 || clearing()) return;
    const { ask } = await import("@tauri-apps/plugin-dialog");
    const confirmed = await ask(
      i18n.t("history.clearConfirm", { count: String(count) }),
      { title: i18n.t("history.clearOlder") },
    );
    if (!confirmed) return;
    setClearing(true);
    try {
      const outcome = await api.historyClearOlder(keep());
      if (outcome.status === "refused") {
        setReport(i18n.t("history.agentEditing"));
        return;
      }
      if (outcome.status === "failed") {
        store.setError(outcome.message);
        return;
      }
      setReport(i18n.t("history.cleared", {
        count: String(outcome.dropped),
        size: sizeText(outcome.freed),
      }));
      await historyStorage.refresh();
      await store.refreshHistory();
      await store.refreshUndoState();
    } catch (error) {
      store.setError(String(error));
    } finally {
      setClearing(false);
    }
  }

  return (
    <section class="history-panel" aria-label={i18n.t("history.title")}>
      <header class="history-header">
        <span class="history-summary">
          {i18n.t("history.summary", { count: String(view().total) })}
          <Show
            when={historyStorage.info()?.journaled}
            fallback={<span class="history-size">{i18n.t("history.notJournaled")}</span>}
          >
            <span class="history-size" data-history-bytes={historyStorage.info()?.bytes}>
              {i18n.t("history.size", { size: sizeText(historyStorage.info()?.bytes ?? 0) })}
            </span>
          </Show>
        </span>
        <Show when={historyStorage.overNotice()}>
          <span class="history-notice" role="status">
            {i18n.t("history.sizeNotice", { size: sizeText(historyStorage.info()?.bytes ?? 0) })}
          </span>
        </Show>
        <span class="history-clear">
          <label>
            {i18n.t("history.keepNewest")}
            <input
              type="number"
              min="0"
              step="1"
              value={keep()}
              aria-label={i18n.t("history.keepNewest")}
              onInput={(event) => {
                const next = Math.floor(Number(event.currentTarget.value));
                if (Number.isFinite(next) && next >= 0) setKeep(next);
              }}
            />
          </label>
          <button
            class="history-clear-button"
            disabled={clearing() || droppable() === 0}
            onClick={() => void clearOlder()}
          >{i18n.t("history.clearOlder")}</button>
        </span>
        <Show when={report()}>
          {(line) => <span class="history-report">{line()}</span>}
        </Show>
      </header>
      <Show
        when={view().entries.length > 0}
        fallback={<p class="undo-history-empty">{i18n.t("history.empty")}</p>}
      >
        <ol class="undo-history-list">
          <For each={view().entries}>
            {(entry) => (
              <li>
                <button
                  class="undo-history-entry"
                  classList={{
                    applied: entry.applied,
                    redo: !entry.applied,
                    current: entry.index === view().applied,
                  }}
                  data-level={entry.index}
                  aria-label={i18n.t("history.jump", { level: String(entry.index) })}
                  onClick={() => void store.restoreUndoLevel(entry.index)}
                >
                  <span class="undo-history-index">{entry.index}</span>
                  <span class="undo-history-name">
                    {entryText(entry)}
                    <span class="history-author"> ({authorName(entry.author)})</span>
                  </span>
                  <span class="undo-history-scopes">
                    <For each={entry.scopes}>
                      {(scope) => (
                        <span class="undo-history-scope" data-scope={scope}>
                          {scopeLabel(scope)}
                        </span>
                      )}
                    </For>
                  </span>
                  <span class="undo-history-time">{formatTimestamp(entry.timestamp)}</span>
                  <Show when={!entry.applied}>
                    <span class="undo-history-state">{i18n.t("history.redoAvailable")}</span>
                  </Show>
                </button>
              </li>
            )}
          </For>
          <Show
            when={hasOlder()}
            fallback={
              <li>
                <button
                  class="undo-history-entry history-start"
                  classList={{ current: view().applied === 0 }}
                  data-level={0}
                  onClick={() => void store.restoreUndoLevel(0)}
                >
                  <span class="undo-history-index">0</span>
                  <span class="undo-history-name">{i18n.t("history.start")}</span>
                </button>
              </li>
            }
          >
            <li>
              <button class="history-load-older" onClick={() => void store.loadOlderHistory()}>
                {i18n.t("history.loadOlder")}
              </button>
            </li>
          </Show>
        </ol>
      </Show>
    </section>
  );
}
