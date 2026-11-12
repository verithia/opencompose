/**
 * Recovery dialog: the File menu's `Recover autosave…` surface and the startup
 * prompt. One row per copy, newest first. Restore is disabled with a reason when
 * a copy is not newer than the saved file; Discard asks before deleting one.
 */
import { For, Show, createSignal } from "solid-js";
import { Portal } from "solid-js/web";
import { ask } from "@tauri-apps/plugin-dialog";
import { i18n } from "../stores/i18n";
import { store } from "../stores/project";
import { recoveryStore, formatSavedAt, restoreDisabledReason } from "../stores/recovery";
import { useTopmostEscape } from "./modalEscape";
import type { RecoveryEntry } from "../types/project";

export function RecoveryDialog(props: { prompt: boolean; onClose: () => void }) {
  const [restoringPath, setRestoringPath] = createSignal<string | null>(null);
  const busy = () => recoveryStore.checking() || recoveryStore.restoring();
  const title = () => props.prompt ? i18n.t("recovery.promptTitle") : i18n.t("recovery.title");

  useTopmostEscape(() => {
    if (!recoveryStore.restoring()) props.onClose();
  });

  const restore = async (entry: RecoveryEntry) => {
    if (busy() || restoreDisabledReason(entry)) return;
    setRestoringPath(entry.path);
    await store.restoreFromRecovery(entry.path);
    setRestoringPath(null);
  };

  const discard = async (entry: RecoveryEntry) => {
    if (busy()) return;
    const confirmed = await ask(
      i18n.t("recovery.discardConfirm", {
        project: entry.project_name,
        time: formatSavedAt(entry.saved_at_ms),
      }),
      { title: i18n.t("recovery.discardTitle"), kind: "warning" },
    );
    if (confirmed) await recoveryStore.discard(entry);
  };

  const rowTime = (entry: RecoveryEntry) => formatSavedAt(entry.saved_at_ms);
  const sizeLabel = (entry: RecoveryEntry) =>
    i18n.t("recovery.size", { size: Math.max(1, Math.round(entry.size / 1024)) });

  return (
    <Portal>
      <div class="recovery-overlay" onClick={() => { if (!recoveryStore.restoring()) props.onClose(); }}>
        <div
          class="recovery-dialog"
          role="dialog"
          aria-modal="true"
          aria-label={title()}
          aria-busy={busy() ? "true" : undefined}
          onClick={(event) => event.stopPropagation()}
        >
          <header class="recovery-head">
            <span class="recovery-title">{title()}</span>
            <button
              type="button"
              class="recovery-close"
              aria-label={i18n.t("recovery.close")}
              disabled={recoveryStore.restoring()}
              onClick={() => props.onClose()}
            >×</button>
          </header>

          <Show when={props.prompt && recoveryStore.newer()}>
            {(entry) => (
              <p class="recovery-body">
                {i18n.t("recovery.promptBody", {
                  project: entry().project_name,
                  time: rowTime(entry()),
                })}
              </p>
            )}
          </Show>

          <Show when={recoveryStore.checking()}>
            <p class="recovery-status" role="status">{i18n.t("recovery.checking")}</p>
          </Show>

          <Show when={recoveryStore.error()}>
            <p class="recovery-error selectable" role="alert">{recoveryStore.error()}</p>
          </Show>

          <Show when={!recoveryStore.checking() && recoveryStore.entries().length === 0}>
            <p class="recovery-empty">{i18n.t("recovery.empty")}</p>
          </Show>

          <ul class="recovery-list">
            <For each={recoveryStore.entries()}>
              {(entry) => {
                const reason = () => restoreDisabledReason(entry);
                const restoringThis = () => restoringPath() === entry.path;
                return (
                  <li class="recovery-row" classList={{ restored: restoringThis() }}>
                    <div class="recovery-row-main">
                      <span class="recovery-row-title">
                        {entry.kind === "backup"
                          ? i18n.t("recovery.previousRow", { project: entry.project_name, time: rowTime(entry) })
                          : i18n.t("recovery.row", { project: entry.project_name, time: rowTime(entry) })}
                      </span>
                      <span class="recovery-row-meta">
                        {sizeLabel(entry)}
                        <Show when={entry.newer_than_disk}>
                          {" · "}
                          <span class="recovery-newer">{i18n.t("recovery.newer")}</span>
                        </Show>
                      </span>
                    </div>
                    <div class="recovery-row-actions">
                      <button
                        type="button"
                        class="recovery-btn recovery-restore"
                        disabled={busy() || reason() !== null}
                        title={reason() ?? undefined}
                        onClick={() => void restore(entry)}
                      >
                        {restoringThis() && recoveryStore.restoring()
                          ? i18n.t("recovery.restoring")
                          : i18n.t("recovery.restore")}
                      </button>
                      <Show when={entry.kind !== "backup"}>
                        <button
                          type="button"
                          class="recovery-btn recovery-discard"
                          disabled={busy() || recoveryStore.discarding() === entry.path}
                          onClick={() => void discard(entry)}
                        >
                          {recoveryStore.discarding() === entry.path
                            ? i18n.t("recovery.discarding")
                            : i18n.t("recovery.discard")}
                        </button>
                      </Show>
                    </div>
                  </li>
                );
              }}
            </For>
          </ul>

          <footer class="recovery-actions">
            <button type="button" class="recovery-btn" onClick={() => props.onClose()}>
              {props.prompt ? i18n.t("recovery.later") : i18n.t("recovery.close")}
            </button>
          </footer>
        </div>
      </div>
    </Portal>
  );
}
