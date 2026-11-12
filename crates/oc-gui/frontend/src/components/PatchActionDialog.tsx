import { Show, createSignal, onMount } from "solid-js";
import { i18n } from "../stores/i18n";

/**
 * One-line naming dialog for patch lifecycle actions. The confirm handler
 * returns an error message, or null when the action succeeded.
 */
export function PatchActionDialog(props: {
  title: string;
  /** Prefilled name; selected so typing replaces it. */
  initial: string;
  confirmLabel: string;
  onConfirm: (name: string) => Promise<string | null>;
  onClose: () => void;
}) {
  const [value, setValue] = createSignal(props.initial);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let input: HTMLInputElement | undefined;

  onMount(() => {
    input?.focus();
    input?.select();
  });

  const submit = async () => {
    if (busy()) return;
    const name = value().trim();
    if (!name) {
      setError(i18n.t("synth.ui.nameRequired"));
      return;
    }
    setBusy(true);
    const message = await props.onConfirm(name);
    setBusy(false);
    if (message) {
      setError(message);
      return;
    }
    props.onClose();
  };

  return (
    <div
      class="patch-action-dialog"
      role="dialog"
      aria-label={props.title}
      onKeyDown={(event) => {
        event.stopPropagation();
        if (event.key === "Escape") {
          event.preventDefault();
          props.onClose();
        }
        if (event.key === "Enter") {
          event.preventDefault();
          void submit();
        }
      }}
    >
      <label class="patch-action-field">
        <span>{props.title}</span>
        <input
          ref={(element) => { input = element; }}
          type="text"
          value={value()}
          onInput={(event) => { setValue(event.currentTarget.value); setError(""); }}
        />
      </label>
      <Show when={error()}>
        <p class="patch-action-error" role="alert">{error()}</p>
      </Show>
      <div class="patch-action-buttons">
        <button type="button" onClick={props.onClose}>{i18n.t("dialog.cancel")}</button>
        <button type="button" class="primary" disabled={busy()} onClick={() => void submit()}>
          {props.confirmLabel}
        </button>
      </div>
    </div>
  );
}
