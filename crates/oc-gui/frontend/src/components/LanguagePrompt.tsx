/**
 * First-run language chooser. Shown once, before any project work, because the
 * language decides every later label; picking one stores it, so the dialog never
 * returns unless the user clears the setting.
 */
import { For, createSignal } from "solid-js";
import { i18n, type LocaleName } from "../stores/i18n";

export function LanguagePrompt(props: { onDone: () => void }) {
  const [chosen, setChosen] = createSignal<LocaleName>(i18n.locale());

  // Picking a language applies it at once, so the dialog's own labels switch
  // before the user confirms.
  const confirm = () => {
    if (i18n.followsSystemLocale() && chosen() === i18n.locale()) i18n.useSystemLocale();
    else i18n.setLocale(chosen());
    props.onDone();
  };

  return (
    <div class="language-prompt-overlay" role="dialog" aria-modal="true" aria-label={i18n.t("language.prompt.title")}>
      <div class="language-prompt">
        <h2 class="language-prompt-title">{i18n.t("language.prompt.title")}</h2>
        <p class="language-prompt-hint">{i18n.t("language.prompt.hint")}</p>
        <div class="language-prompt-list" role="radiogroup" aria-label={i18n.t("language.prompt.title")}>
          <For each={i18n.locales}>
            {(locale) => (
              <button
                type="button"
                class="language-prompt-choice"
                classList={{ active: chosen() === locale }}
                role="radio"
                aria-checked={chosen() === locale}
                onClick={() => {
                  setChosen(locale);
                  i18n.setLocale(locale);
                }}
                onDblClick={() => {
                  setChosen(locale);
                  confirm();
                }}
              >
                {i18n.labels[locale]}
              </button>
            )}
          </For>
        </div>
        <button type="button" class="language-prompt-confirm" onClick={confirm}>
          {i18n.t("language.prompt.confirm")}
        </button>
      </div>
    </div>
  );
}
