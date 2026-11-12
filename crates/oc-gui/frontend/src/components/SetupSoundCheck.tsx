/** A reversible output check using the same audition lifecycle as the editor. */
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { audioIoStore } from "../stores/audioIo";
import { preferencesUi } from "../stores/preferencesUi";
import { i18n } from "../stores/i18n";

const TEST_PATCH: [string, string | null][] = [
  ["synth_version", "2"], ["osc.a.waveform", "0"], ["osc.a.level", "0.12"],
  ["osc.a.unison", "1"], ["osc.b.level", "0"], ["osc.c.level", "0"],
  ["sub.level", "0"], ["noise.level", "0"],
  ["amp.attack", "0.02"], ["amp.release", "0.12"],
];

export function SetupSoundCheck() {
  const label = `OpenCompose: ${i18n.t("onboarding.sound.play")}`;
  const [requested, setRequested] = createSignal(false);
  const owned = () => store.auditionActive() && store.audition()?.preset === label;
  const otherPreview = () => !owned() && (store.auditionActive() || store.transport().playing);
  onMount(() => { void audioIoStore.readStatus(); }); // Read only; never opens a stream.
  onCleanup(() => { if (owned()) void store.stopAudition(); });
  const toggle = () => {
    if (owned()) { void store.stopAudition(); return; }
    if (otherPreview()) return;
    setRequested(true);
    store.setError(null);
    void store.auditionFactoryPatch(label, TEST_PATCH);
  };
  return (
    <section class="onboarding-sound" aria-label={i18n.t("onboarding.sound.title")}>
      <strong>{i18n.t("onboarding.sound.title")}</strong>
      <p>{i18n.t("onboarding.sound.body")}</p>
      <p class="onboarding-footnote">{audioIoStore.opened()?.name
        ?? audioIoStore.status().selection.output_device_name
        ?? i18n.t("onboarding.sound.defaultOutput")}</p>
      <div class="onboarding-sound-actions">
        <button type="button" class="onboarding-secondary" disabled={otherPreview()} onClick={toggle}>
          {owned() ? i18n.t("onboarding.sound.stop") : i18n.t("onboarding.sound.play")}
        </button>
        <button type="button" class="onboarding-ghost" onClick={event => {
          if (owned()) void store.stopAudition();
          preferencesUi.openOn("audio", "connection", event.currentTarget);
        }}>{i18n.t("onboarding.sound.settings")}</button>
      </div>
      <Show when={otherPreview()}><p class="onboarding-footnote">{i18n.t("onboarding.sound.otherPreview")}</p></Show>
      <Show when={owned() && store.audition()?.status === "loading"}>
        <p class="onboarding-footnote" role="status">{i18n.t("onboarding.sound.loading")}</p>
      </Show>
      <Show when={requested() && store.error()}>
        {error => <p class="onboarding-tools-error" role="alert">{error()}</p>}
      </Show>
    </section>
  );
}
