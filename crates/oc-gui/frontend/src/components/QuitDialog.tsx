/**
 * The unsaved-changes prompt. It is a modal: Escape and the overlay close it
 * without an answer, which is the same as Cancel — nothing is written and
 * nothing closes. Focus starts on Save, stays inside while the prompt is open,
 * and returns to where it came from when it closes.
 */
import { Show, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { useTopmostEscape } from "./modalEscape";

/** Elements that take focus inside the prompt, in document order. */
const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function QuitDialog(props: {
  /** Overrides the quit wording; the open guard shares this modal. */
  body?: string;
  busy?: boolean;
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}) {
  let dialog: HTMLDivElement | undefined;
  let saveButton: HTMLButtonElement | undefined;
  let restoreFocus: HTMLElement | null = null;

  /** Tab and Shift+Tab wrap inside the prompt; nothing behind it takes focus. */
  const trapTab = (event: KeyboardEvent) => {
    if (event.key !== "Tab" || !dialog) return;
    const items = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)];
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    const inside = active instanceof HTMLElement && dialog.contains(active);
    const edge = event.shiftKey ? first : last;
    if (!inside || active === edge) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
    }
  };

  useTopmostEscape(() => props.onCancel());

  onMount(() => {
    restoreFocus = document.activeElement as HTMLElement | null;
    saveButton?.focus();
    document.addEventListener("keydown", trapTab, true);
  });

  onCleanup(() => {
    document.removeEventListener("keydown", trapTab, true);
    if (restoreFocus && restoreFocus !== document.body && restoreFocus.isConnected) {
      restoreFocus.focus();
    }
  });

  return (
    <div class="quit-overlay"
      data-tauri-drag-region="false" onClick={() => props.onCancel()}>
      <div
        class="quit-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("dialog.unsavedTitle")}
        ref={(element) => { dialog = element; }}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="quit-dialog-title">{i18n.t("dialog.unsavedTitle")}</div>
        <div class="quit-dialog-body">
          {props.body ?? i18n.t("dialog.unsavedBody", { project: store.project.name || i18n.t("status.untitled") })}
        </div>
        <Show when={store.error()}>
          <div class="quit-dialog-body selectable" role="alert">{store.error()}</div>
        </Show>
        <div class="quit-dialog-actions">
          <button
            class="quit-btn quit-btn-save"
            disabled={props.busy}
            ref={(element) => { saveButton = element; }}
            onClick={() => props.onSave()}
          >{i18n.t("dialog.save")}</button>
          <button class="quit-btn quit-btn-discard" disabled={props.busy} onClick={() => props.onDiscard()}>{i18n.t("dialog.dontSave")}</button>
          <button class="quit-btn quit-btn-cancel" disabled={props.busy} onClick={() => props.onCancel()}>{i18n.t("dialog.cancel")}</button>
        </div>
      </div>
    </div>
  );
}
