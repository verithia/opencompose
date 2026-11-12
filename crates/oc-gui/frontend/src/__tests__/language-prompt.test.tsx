/**
 * First-run language chooser: it must offer every locale, apply the pick before
 * the shell is used, and remember it so the dialog never returns.
 */
import { afterEach, describe, expect, it } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { LanguagePrompt } from "../components/LanguagePrompt";
import { i18n } from "../stores/i18n";

afterEach(() => i18n.setLocale("en"));

describe("the first-run language prompt", () => {
  it("offers every locale and applies the confirmed one", () => {
    let done = 0;
    const view = render(() => <LanguagePrompt onDone={() => { done += 1; }} />);
    const choices = view.container.querySelectorAll(".language-prompt-choice");
    expect(choices).toHaveLength(i18n.locales.length);

    fireEvent.click(view.getByText("简体中文"));
    fireEvent.click(view.getByText("继续"));

    expect(i18n.locale()).toBe("zh-Hans");
    expect(done).toBe(1);
  });

  it("keeps following the OS when the detected default is simply confirmed", () => {
    i18n.useSystemLocale();
    const view = render(() => <LanguagePrompt onDone={() => {}} />);
    fireEvent.click(view.container.querySelector(".language-prompt-confirm")!);
    expect(i18n.followsSystemLocale()).toBe(true);
    expect(i18n.hasStoredLocale()).toBe(true);
  });

  it("preselects the current locale when the dialog reopens", () => {
    i18n.setLocale("zh-Hant");
    const view = render(() => <LanguagePrompt onDone={() => {}} />);
    const active = view.container.querySelector(".language-prompt-choice.active");
    expect(active?.textContent).toBe("繁體中文");
  });
});
