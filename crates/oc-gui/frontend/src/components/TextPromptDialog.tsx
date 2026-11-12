/**
 * A single-line text prompt the app owns.
 *
 * The webview has no JavaScript prompt: wry implements only the file-upload
 * panel, so `window.prompt` returns without showing anything. Every rename
 * therefore goes through this dialog, which is keyboard-operable and can be
 * cancelled with Escape or a click outside.
 */
import { createSignal, onMount } from "solid-js";
import { i18n } from "../stores/i18n";

export function TextPromptDialog(props: {
  title: string;
  initial: string;
  onCancel: () => void;
  onConfirm: (value: string) => void;
}) {
  const [value, setValue] = createSignal(props.initial);
  let input: HTMLInputElement | undefined;

  onMount(() => {
    input?.focus();
    input?.select();
  });

  const submit = () => {
    const next = value().trim();
    if (next && next !== props.initial) props.onConfirm(next);
    else props.onCancel();
  };

  return (
    <div class="text-prompt-overlay" onPointerDown={(event) => {
      if (event.target === event.currentTarget) props.onCancel();
    }}>
      <form
        class="text-prompt"
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        onSubmit={(event) => { event.preventDefault(); submit(); }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            props.onCancel();
          }
        }}
      >
        <label class="text-prompt-label">
          <span>{props.title}</span>
          <input
            ref={input}
            class="text-prompt-input"
            aria-label={props.title}
            value={value()}
            onInput={(event) => setValue(event.currentTarget.value)}
          />
        </label>
        <div class="text-prompt-actions">
          <button type="button" class="text-prompt-btn" onClick={props.onCancel}>
            {i18n.t("dialog.cancel")}
          </button>
          <button type="submit" class="text-prompt-btn primary" disabled={!value().trim()}>
            {i18n.t("dialog.ok")}
          </button>
        </div>
      </form>
    </div>
  );
}
