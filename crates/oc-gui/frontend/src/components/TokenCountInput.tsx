/**
 * Text field for a token count. The raw text stays editable while the user
 * types; a parseable value is committed at once, an unparseable one shows an
 * inline error and leaves the committed value untouched.
 */
import { Show, createEffect, createMemo, createSignal } from "solid-js";
import { formatTokenCount, parseTokenCount } from "../ai/tokens";
import { i18n, type I18nKey } from "../stores/i18n";

interface Props {
  id?: string;
  class?: string;
  value: number;
  ariaLabel: string;
  /** Field message for a count that is not positive; other text is a format error. */
  errorKey: I18nKey;
  onCommit: (value: number) => void;
  /** Reports the visible problem, so the form can block submit while it stands. */
  onProblem?: (problem: I18nKey | null) => void;
  /** Off when the form renders the problem in its own notice area. */
  inlineError?: boolean;
}

export function TokenCountInput(props: Props) {
  const [text, setText] = createSignal(formatTokenCount(props.value));
  const [invalid, setInvalid] = createSignal(false);

  // A load, a reset or a sibling can change the committed value.
  createEffect(() => {
    setText(formatTokenCount(props.value));
    setInvalid(false);
  });

  const problem = createMemo<I18nKey | null>(() => {
    if (!invalid()) return null;
    const trimmed = text().trim();
    const nonPositive = /^[+-]?\d+$/.test(trimmed) && Number(trimmed) <= 0;
    return nonPositive ? props.errorKey : "ai.limits.error.tokenCount";
  });

  createEffect(() => props.onProblem?.(problem()));

  const commit = (raw: string) => {
    const parsed = parseTokenCount(raw);
    if (parsed === null) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    setText(formatTokenCount(parsed));
    if (parsed !== props.value) props.onCommit(parsed);
  };

  return (
    <>
      <input
        id={props.id}
        class={props.class}
        type="text"
        autocomplete="off"
        spellcheck={false}
        aria-label={props.ariaLabel}
        aria-invalid={invalid()}
        value={text()}
        onInput={(event) => {
          const raw = event.currentTarget.value;
          setText(raw);
          commit(raw);
        }}
        onBlur={() => commit(text())}
      />
      <Show when={(props.inlineError ?? true) && problem()}>
        {(key) => <small class="ai-auth-error selectable">{i18n.t(key())}</small>}
      </Show>
    </>
  );
}
