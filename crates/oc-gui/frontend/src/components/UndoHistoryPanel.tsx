/**
 * Undo history panel — every level with its caller-supplied name and scopes.
 * Clicking a level calls the backend cursor command, so a user can jump back
 * several edits at once.
 */
import { For, Show, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import type { UndoScope } from "../stores/api";
import "./UndoHistoryPanel.css";

function scopeLabel(scope: UndoScope): string {
  return i18n.t(`history.scope.${scope}` as Parameters<typeof i18n.t>[0]);
}

function formatTimestamp(timestamp: number): string {
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
  return new Date(timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function UndoHistoryPanel(props: { open: boolean; onClose: () => void }) {
  onMount(() => { void store.refreshUndoHistory(); });

  return (
    <Show when={props.open}>
      <section class="undo-history-panel" aria-label={i18n.t("history.title")}>
        <header class="undo-history-header">
          <strong>{i18n.t("history.title")}</strong>
          <button
            class="undo-history-close"
            aria-label={i18n.t("panel.close")}
            onClick={props.onClose}
          >×</button>
        </header>
        <Show
          when={store.undoHistory().length > 0}
          fallback={<p class="undo-history-empty">{i18n.t("history.empty")}</p>}
        >
          <ol class="undo-history-list">
            <For each={[...store.undoHistory()].reverse()}>
              {(entry) => (
                <li>
                  <button
                    class="undo-history-entry"
                    classList={{ applied: entry.applied, redo: !entry.applied }}
                    data-level={entry.index}
                    aria-label={i18n.t("history.jump", { level: String(entry.index) })}
                    onClick={() => void store.restoreUndoLevel(entry.index)}
                  >
                    <span class="undo-history-index">{entry.index}</span>
                    <span class="undo-history-name">{entry.name}</span>
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
          </ol>
        </Show>
      </section>
    </Show>
  );
}
