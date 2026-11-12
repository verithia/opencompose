/**
 * The shortcut editor: every command, its current binding, and a one-key
 * capture that writes the override immediately.
 *
 * A refused capture states why inline and leaves the old binding in place. The
 * table reads the keymap signal, so the palette and the preferences page follow
 * the same change without a reload.
 *
 * `ShortcutEditorContent` is the surface on its own: the Preferences keymap page
 * embeds it, and `ShortcutEditorBody` wraps the same content in a dialog.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { i18n } from "../stores/i18n";
import {
  COMMANDS,
  bindingFromEvent,
  categoryLabel,
  commandLabel,
  formatBinding,
  type Binding,
  type Command,
} from "../stores/commands";
import {
  commandBindingLabel,
  keymapOverrides,
  resetAllBindings,
  resetBinding,
  setBinding,
} from "../stores/keymap";
import { overlayStore } from "../stores/overlays";
import { useTopmostEscape } from "./modalEscape";
import "./ShortcutEditor.css";

/** The palette trigger is fixed by the app and cannot be reassigned. */
const FIXED_COMMAND_ID = "command.palette.open";

/** Search, the binding table and the two reset actions. */
export function ShortcutEditorContent(props: {
  focusSearch?: boolean;
  /** Told when a key capture opens or closes, so Escape can be yielded. */
  onRecordingChange?: (recording: boolean) => void;
}) {
  const [query, setQuery] = createSignal("");
  const [recordingId, setRecordingId] = createSignal<string | null>(null);
  const [rowErrors, setRowErrors] = createSignal<Record<string, string>>({});
  const [status, setStatus] = createSignal("");
  const [confirmReset, setConfirmReset] = createSignal(false);
  let search: HTMLInputElement | undefined;

  const rows = createMemo(() => {
    const value = query().trim().toLowerCase();
    if (!value) return COMMANDS;
    return COMMANDS.filter((command) => {
      const haystack = `${commandLabel(command)} ${categoryLabel(command.category)}`.toLowerCase();
      return value.split(/\s+/).filter(Boolean).every((token) => haystack.includes(token));
    });
  });

  const isCustom = (command: Command) => Boolean(keymapOverrides()[command.id]);

  createEffect(() => props.onRecordingChange?.(recordingId() !== null));

  const clearError = (commandId: string) => {
    setRowErrors((current) => {
      if (!(commandId in current)) return current;
      const next = { ...current };
      delete next[commandId];
      return next;
    });
  };

  const record = (command: Command, binding: Binding) => {
    const result = setBinding(command.id, binding);
    setRecordingId(null);
    if (!result.ok) {
      setRowErrors((current) => ({ ...current, [command.id]: result.message }));
      setStatus("");
      return;
    }
    clearError(command.id);
    setStatus(i18n.t("keymap.bound", {
      command: commandLabel(command),
      binding: formatBinding(result.binding),
    }));
  };

  createEffect(() => {
    const commandId = recordingId();
    if (!commandId) return;
    const command = COMMANDS.find((entry) => entry.id === commandId);
    if (!command) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setRecordingId(null);
        return;
      }
      const binding = bindingFromEvent(event);
      // A modifier-only press keeps the capture open instead of binding Shift.
      if (!binding) return;
      event.preventDefault();
      event.stopPropagation();
      record(command, binding);
    };
    document.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown, true));
  });

  const resetOne = (command: Command) => {
    resetBinding(command.id);
    clearError(command.id);
    setStatus(i18n.t("keymap.reset", { command: commandLabel(command) }));
  };

  const confirmRestoreDefaults = () => {
    resetAllBindings();
    setRowErrors({});
    setRecordingId(null);
    setConfirmReset(false);
    setStatus(i18n.t("keymap.resetAll"));
  };

  onMount(() => {
    if (props.focusSearch) search?.focus();
  });

  return (
    <div class="shortcut-editor-body">
      <input
        ref={search}
        class="shortcut-editor-search"
        type="search"
        aria-label={i18n.t("shortcuts.search")}
        placeholder={i18n.t("shortcuts.search")}
        value={query()}
        onInput={(event) => setQuery(event.currentTarget.value)}
      />

      <div class="shortcut-table" role="table" aria-label={i18n.t("shortcuts.title")}>
        <div class="shortcut-head" role="row">
          <span role="columnheader">{i18n.t("shortcuts.command")}</span>
          <span role="columnheader">{i18n.t("shortcuts.binding")}</span>
          <span role="columnheader">{i18n.t("shortcuts.category")}</span>
          <span role="columnheader">{i18n.t("shortcuts.reset")}</span>
        </div>
        <Show
          when={rows().length > 0}
          fallback={<p class="shortcut-editor-empty">{i18n.t("shortcuts.empty")}</p>}
        >
          <For each={rows()}>
            {(command) => {
              const recording = () => recordingId() === command.id;
              return (
                <div
                  class="shortcut-row"
                  classList={{ recording: recording() }}
                  role="row"
                  data-command-id={command.id}
                >
                  <span class="shortcut-command" role="cell">
                    {commandLabel(command)}
                    <Show when={isCustom(command)}>
                      <span class="shortcut-custom">{i18n.t("shortcuts.custom")}</span>
                    </Show>
                  </span>
                  <span role="cell">
                    <button
                      type="button"
                      class="shortcut-binding"
                      classList={{ recording: recording() }}
                      aria-label={i18n.t("shortcuts.record", { command: commandLabel(command) })}
                      disabled={command.id === FIXED_COMMAND_ID}
                      onClick={() => setRecordingId(command.id)}
                    >
                      {recording()
                        ? i18n.t("shortcuts.recording")
                        : commandBindingLabel(command.id) ?? i18n.t("shortcuts.unbound")}
                    </button>
                  </span>
                  <span class="shortcut-category" role="cell">{categoryLabel(command.category)}</span>
                  <span role="cell">
                    <button
                      type="button"
                      class="shortcut-row-reset"
                      aria-label={i18n.t("shortcuts.resetOne", { command: commandLabel(command) })}
                      disabled={!isCustom(command)}
                      onClick={() => resetOne(command)}
                    >{i18n.t("shortcuts.reset")}</button>
                  </span>
                  <Show when={rowErrors()[command.id]}>
                    {(message) => (
                      <p class="shortcut-error" role="alert">{message()}</p>
                    )}
                  </Show>
                  <Show when={recording()}>
                    <p class="shortcut-recording-hint">
                      {i18n.t("shortcuts.recording")} {i18n.t("shortcuts.pressEsc")}
                    </p>
                  </Show>
                </div>
              );
            }}
          </For>
        </Show>
      </div>

      <footer class="shortcut-editor-footer">
        <span class="shortcut-status" role="status">{status()}</span>
        <button
          type="button"
          class="shortcut-restore"
          onClick={() => setConfirmReset(true)}
        >{i18n.t("shortcuts.resetAll")}</button>
      </footer>

      <Show when={confirmReset()}>
        <div class="shortcut-confirm" role="alertdialog" aria-label={i18n.t("shortcuts.restoreTitle")}>
          <div class="shortcut-confirm-title">{i18n.t("shortcuts.restoreTitle")}</div>
          <p class="shortcut-confirm-body">{i18n.t("shortcuts.restoreBody")}</p>
          <div class="shortcut-confirm-actions">
            <button
              type="button"
              class="shortcut-confirm-cancel"
              onClick={() => setConfirmReset(false)}
            >{i18n.t("shortcuts.cancel")}</button>
            <button
              type="button"
              class="shortcut-confirm-apply"
              onClick={confirmRestoreDefaults}
            >{i18n.t("shortcuts.restoreConfirm")}</button>
          </div>
        </div>
      </Show>
    </div>
  );
}

/** The same surface inside the Preferences keymap page. */
export function ShortcutEditorPanel() {
  return (
    <div class="shortcut-editor-panel">
      <ShortcutEditorContent />
    </div>
  );
}

export function ShortcutEditorBody() {
  const [recording, setRecording] = createSignal(false);
  const previousFocus = document.activeElement as HTMLElement | null;

  useTopmostEscape(
    () => overlayStore.closeShortcutEditor(),
    () => !recording(),
  );

  onCleanup(() => {
    previousFocus?.focus?.({ preventScroll: true });
  });

  return (
    <div
      class="shortcut-editor-overlay"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) overlayStore.closeShortcutEditor();
      }}
    >
      <div
        class="shortcut-editor"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("shortcuts.title")}
      >
        <header class="shortcut-editor-header">
          <span class="shortcut-editor-title">{i18n.t("shortcuts.title")}</span>
          <button
            type="button"
            class="shortcut-editor-close"
            aria-label={i18n.t("shortcuts.cancel")}
            onClick={() => overlayStore.closeShortcutEditor()}
          >×</button>
        </header>

        <ShortcutEditorContent focusSearch onRecordingChange={setRecording} />
      </div>
    </div>
  );
}

export function ShortcutEditor() {
  return (
    <Show when={overlayStore.shortcutEditorOpen()}>
      <Portal>
        <ShortcutEditorBody />
      </Portal>
    </Show>
  );
}
