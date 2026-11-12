/**
 * Which Preferences page is open, and the one place a button asks for it.
 *
 * The modal itself is mounted once by the toolbar. The AI panel's settings
 * button opens the same surface on the AI page instead of stacking a second
 * dialog on top of it.
 */
import { createSignal } from "solid-js";
import { ask } from "@tauri-apps/plugin-dialog";
import { aiConfigLoaded, discardConfigChanges, hasAnyKeyDraft, hasUnsavedConfig } from "../ai/config";
import { errorMessage } from "../ai/errors";
import { aiWorkspace } from "./aiWorkspace";
import { i18n } from "./i18n";

export type PreferencesPageId =
  | "display"
  | "audio"
  | "keymap"
  | "performance"
  | "storage"
  | "network"
  | "sources"
  | "ai"
  | "help";

/** The field the AI page focuses when it opens. */
export type PreferencesFocus = "connection" | "toolsets" | "auth";

const [open, setOpen] = createSignal(false);
const [page, setPage] = createSignal<PreferencesPageId>("display");
const [focus, setFocus] = createSignal<PreferencesFocus>("connection");
const [opener, setOpener] = createSignal<HTMLElement | null>(null);
let closing = false;

export const preferencesUi = {
  open,
  page,
  focus,
  /** Opens Preferences on one page; a second call from another button moves it. */
  openOn(next: PreferencesPageId, field: PreferencesFocus = "connection", from?: HTMLElement | null) {
    setPage(next);
    setFocus(field);
    setOpener(from ?? null);
    setOpen(true);
  },
  close() {
    const from = opener();
    setOpen(false);
    setOpener(null);
    if (from) queueMicrotask(() => from.focus({ preventScroll: true }));
  },
  /**
   * Leaves the surface. A provider edit that is still a draft is either kept
   * by saving or discarded here; a save or connection test in flight owns the
   * dialog until it settles.
   */
  async requestClose(onClose: () => void) {
    if (closing || aiWorkspace.settingsBusy()) return;
    closing = true;
    try {
      if (aiConfigLoaded() && (hasUnsavedConfig() || hasAnyKeyDraft())) {
        let discard = false;
        try {
          discard = await ask(i18n.t("preferences.ai.discard"), {
            title: i18n.t("preferences.title"),
            kind: "warning",
          });
        } catch {
          return;
        }
        if (!discard) return;
        try {
          await discardConfigChanges();
        } catch (error) {
          aiWorkspace.notify(errorMessage(error), "error");
          return;
        }
      }
      onClose();
    } finally {
      closing = false;
    }
  },
  show(next: PreferencesPageId) {
    setPage(next);
  },
};
