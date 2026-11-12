/**
 * The Preferences surface: one in-app modal with a page per subject, opened
 * from the toolbar button.
 *
 * Every page reads what it shows when it mounts, so opening Display does not
 * touch the audio backend at all — and opening Audio still only reads the
 * device state. Escape, the close button and a click on the backdrop close it.
 */
import { Portal } from "solid-js/web";
import { For, Match, Show, Switch, createEffect, createSignal } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { useDialogFocus } from "./dialogFocus";
import { useTopmostEscape } from "./modalEscape";
import { DisplayPage } from "./preferences/DisplayPage";
import { AudioPage } from "./preferences/AudioPage";
import { KeymapPage } from "./preferences/KeymapPage";
import { PerformancePage } from "./preferences/PerformancePage";
import { StoragePage } from "./preferences/StoragePage";
import { ExternalSourcesPage } from "./preferences/ExternalSourcesPage";
import { NetworkPage } from "./preferences/NetworkPage";
import { HelpPage } from "./preferences/HelpPage";
import { AiPage } from "./preferences/AiPage";
import { preferencesUi, type PreferencesPageId } from "../stores/preferencesUi";
import { aiWorkspace } from "../stores/aiWorkspace";
import "./preferences/Preferences.css";

const PAGES: { id: PreferencesPageId; key: I18nKey }[] = [
  { id: "display", key: "preferences.page.display" },
  { id: "audio", key: "preferences.page.audio" },
  { id: "keymap", key: "preferences.page.keymap" },
  { id: "performance", key: "preferences.page.performance" },
  { id: "storage", key: "preferences.page.storage" },
  { id: "network", key: "preferences.page.network" },
  { id: "sources", key: "preferences.page.sources" },
  { id: "ai", key: "preferences.page.ai" },
  { id: "help", key: "preferences.page.help" },
];

/** The one host: the toolbar mounts it, and a surface that opens Preferences
 * from elsewhere only moves the shared page. */
export function PreferencesHost(props: { onClose?: () => void }) {
  return (
    <Portal>
      <PreferencesModal
        open={preferencesUi.open()}
        page={preferencesUi.page()}
        onClose={props.onClose ?? (() => preferencesUi.close())}
      />
    </Portal>
  );
}

export function PreferencesModal(props: {
  open: boolean;
  onClose: () => void;
  /** The page to show when it opens; the toolbar keeps the AI button's choice. */
  page?: PreferencesPageId;
}) {
  return (
    <Show when={props.open}>
      <PreferencesDialog onClose={props.onClose} page={props.page} />
    </Show>
  );
}

function PreferencesDialog(props: { onClose: () => void; page?: PreferencesPageId }) {
  const [page, setPage] = createSignal<PreferencesPageId>(props.page ?? "display");

  createEffect(() => {
    if (props.page) setPage(props.page);
  });

  const requestClose = () => preferencesUi.requestClose(props.onClose);

  let dialog: HTMLDivElement | undefined;
  const isTopmost = useTopmostEscape(() => void requestClose(), () => !aiWorkspace.settingsBusy());
  useDialogFocus(() => dialog, isTopmost);

  return (
    <div
      class="preferences-overlay"
      data-tauri-drag-region="false"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) void requestClose();
      }}
    >
      <div
        ref={dialog}
        tabIndex={-1}
        class="preferences-modal"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("preferences.title")}
      >
        <header class="preferences-modal-header">
          <span class="preferences-modal-title">{i18n.t("preferences.title")}</span>
          <button
            type="button"
            class="preferences-modal-close"
            aria-label={i18n.t("preferences.close")}
            onClick={() => void requestClose()}
          >×</button>
        </header>

        <div class="preferences-modal-body">
          <nav class="preferences-nav" aria-label={i18n.t("preferences.title")}>
            <For each={PAGES}>
              {(entry) => (
                <button
                  type="button"
                  class="preferences-nav-item"
                  classList={{ active: page() === entry.id }}
                  aria-current={page() === entry.id ? "page" : undefined}
                  data-page={entry.id}
                  onClick={() => {
                    setPage(entry.id);
                    preferencesUi.show(entry.id);
                  }}
                >
                  {i18n.t(entry.key)}
                </button>
              )}
            </For>
          </nav>

          <section class="preferences-page" data-page={page()} aria-label={i18n.t("preferences.title")}>
            <Switch>
              <Match when={page() === "display"}><DisplayPage /></Match>
              <Match when={page() === "audio"}><AudioPage /></Match>
              <Match when={page() === "keymap"}><KeymapPage /></Match>
              <Match when={page() === "performance"}><PerformancePage /></Match>
              <Match when={page() === "storage"}><StoragePage onClose={() => void requestClose()} /></Match>
              <Match when={page() === "network"}><NetworkPage /></Match>
              <Match when={page() === "sources"}><ExternalSourcesPage /></Match>
              <Match when={page() === "ai"}><AiPage /></Match>
              <Match when={page() === "help"}><HelpPage /></Match>
            </Switch>
          </section>
        </div>
      </div>
    </div>
  );
}
