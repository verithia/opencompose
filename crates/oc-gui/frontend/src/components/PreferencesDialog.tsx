/**
 * Preferences bundle dialog: write the current settings to a portable file, or
 * read one back behind a confirmation and a per-field report.
 *
 * The import never applies while it validates: the file is read first, the
 * dialog names how many settings it would replace, and only Replace touches the
 * stores. Every leaf the stores refuse is listed with its reason.
 */
import { For, Show, createSignal, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { i18n } from "../stores/i18n";
import { useTopmostEscape } from "./modalEscape";
import {
  applyPreferences,
  exportPreferencesFile,
  importPreferencesFile,
  type PreferencesApplySummary,
  type PreferencesImportPreview,
  type PreferencesRequest,
  type SkippedPreference,
} from "../stores/preferencesBundle";

type Page = "working" | "confirm" | "applying" | "done" | "empty" | "failed";

export function PreferencesDialog(props: { request: PreferencesRequest; onClose: () => void }) {
  const importing = props.request.kind === "import";
  const [page, setPage] = createSignal<Page>("working");
  const [preview, setPreview] = createSignal<PreferencesImportPreview | null>(null);
  const [exported, setExported] = createSignal<{ count: number; path: string } | null>(null);
  const [summary, setSummary] = createSignal<PreferencesApplySummary | null>(null);
  const [readSkipped, setReadSkipped] = createSignal<SkippedPreference[]>([]);
  const [error, setError] = createSignal<string | null>(null);

  const busy = () => page() === "working" || page() === "applying";
  const title = () =>
    importing ? i18n.t("preferences.importTitle") : i18n.t("preferences.exportTitle");

  /** Refusals and per-field skips, in the order they were found. */
  const skipped = (): SkippedPreference[] => [...readSkipped(), ...(summary()?.skipped ?? [])];

  useTopmostEscape(() => {
    if (!busy()) props.onClose();
  });

  const runExport = async () => {
    try {
      const count = await exportPreferencesFile(props.request.path);
      setExported({ count, path: props.request.path });
      setPage("done");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setPage("failed");
    }
  };

  const loadImport = async () => {
    try {
      const result = await importPreferencesFile(props.request.path);
      if (result.errors.length > 0) {
        setError(result.errors.join(" "));
        setPage("failed");
        return;
      }
      setPreview(result);
      setReadSkipped(result.skipped);
      setPage(result.applied.length === 0 ? "empty" : "confirm");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setPage("failed");
    }
  };

  onMount(() => {
    void (importing ? loadImport() : runExport());
  });

  const apply = async () => {
    const current = preview();
    if (!current || busy()) return;
    setPage("applying");
    try {
      setSummary(await applyPreferences(current.bundle));
      setPage("done");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setPage("failed");
    }
  };

  return (
    <Portal>
      <div class="preferences-bundle-overlay" onClick={() => { if (!busy()) props.onClose(); }}>
        <div
          class="preferences-bundle-dialog"
          role="dialog"
          aria-modal="true"
          aria-label={title()}
          aria-busy={busy() ? "true" : undefined}
          onClick={(event) => event.stopPropagation()}
        >
          <header class="preferences-bundle-head">
            <span class="preferences-bundle-title">{title()}</span>
            <button
              type="button"
              class="preferences-bundle-close"
              aria-label={i18n.t("preferences.close")}
              disabled={busy()}
              onClick={() => props.onClose()}
            >×</button>
          </header>

          <p class="preferences-bundle-path selectable">{props.request.path}</p>

          <Show when={page() === "working"}>
            <p class="preferences-bundle-status" role="status">
              {importing
                ? i18n.t("preferences.importing", { path: props.request.path })
                : i18n.t("preferences.exporting", { path: props.request.path })}
            </p>
          </Show>

          <Show when={page() === "confirm" && preview()}>
            {(current) => (
              <>
                <p class="preferences-bundle-body">
                  {i18n.t("preferences.confirm", { count: current().applied.length })}
                </p>
                <Show when={current().skipped.length > 0}>
                  <p class="preferences-bundle-body">
                    {i18n.t("preferences.confirmSkipped", { count: current().skipped.length })}
                  </p>
                </Show>
                <div class="preferences-bundle-actions">
                  <button
                    type="button"
                    class="preferences-bundle-btn preferences-bundle-cancel"
                    onClick={() => props.onClose()}
                  >
                    {i18n.t("preferences.cancel")}
                  </button>
                  <button
                    type="button"
                    class="preferences-bundle-btn preferences-bundle-apply"
                    onClick={() => void apply()}
                  >
                    {i18n.t("preferences.apply")}
                  </button>
                </div>
              </>
            )}
          </Show>

          <Show when={page() === "applying"}>
            <p class="preferences-bundle-status" role="status">
              {i18n.t("preferences.applying", { count: preview()?.applied.length ?? 0 })}
            </p>
          </Show>

          <Show when={page() === "empty"}>
            <p class="preferences-bundle-body">{i18n.t("preferences.nothing")}</p>
            <div class="preferences-bundle-actions">
              <button
                type="button"
                class="preferences-bundle-btn preferences-bundle-close-action"
                onClick={() => props.onClose()}
              >
                {i18n.t("preferences.close")}
              </button>
            </div>
          </Show>

          <Show when={page() === "done" && summary()}>
            {(result) => (
              <>
                <p class="preferences-bundle-body preferences-bundle-applied">
                  {i18n.t("preferences.applied", { count: result().applied.length })}
                </p>
                <Show when={skipped().length > 0}>
                  <div class="preferences-bundle-skipped">
                    <p class="preferences-bundle-skipped-head">
                      {i18n.t("preferences.skippedTitle", { count: skipped().length })}
                    </p>
                    <ul class="preferences-bundle-skipped-list">
                      <For each={skipped()}>
                        {(entry) => (
                          <li class="preferences-bundle-skipped-row">
                            <code class="preferences-bundle-skipped-field selectable">
                              {entry.field}
                            </code>
                            <span class="preferences-bundle-skipped-reason selectable">
                              {entry.reason}
                            </span>
                          </li>
                        )}
                      </For>
                    </ul>
                  </div>
                </Show>
                <div class="preferences-bundle-actions">
                  <button
                    type="button"
                    class="preferences-bundle-btn preferences-bundle-close-action"
                    onClick={() => props.onClose()}
                  >
                    {i18n.t("preferences.close")}
                  </button>
                </div>
              </>
            )}
          </Show>

          <Show when={page() === "done" && exported()}>
            {(result) => (
              <>
                <p class="preferences-bundle-body preferences-bundle-applied">
                  {i18n.t("preferences.exported", {
                    count: result().count,
                    path: result().path,
                  })}
                </p>
                <div class="preferences-bundle-actions">
                  <button
                    type="button"
                    class="preferences-bundle-btn preferences-bundle-close-action"
                    onClick={() => props.onClose()}
                  >
                    {i18n.t("preferences.close")}
                  </button>
                </div>
              </>
            )}
          </Show>

          <Show when={page() === "failed"}>
            <p class="preferences-bundle-error selectable" role="alert">{error()}</p>
            <div class="preferences-bundle-actions">
              <button
                type="button"
                class="preferences-bundle-btn preferences-bundle-close-action"
                onClick={() => props.onClose()}
              >
                {i18n.t("preferences.close")}
              </button>
            </div>
          </Show>
        </div>
      </div>
    </Portal>
  );
}
