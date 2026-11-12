/**
 * The Network page: which proxy outbound requests use, and a probe that proves
 * the choice against a real host.
 */
import { Show, createSignal, onMount } from "solid-js";
import { i18n } from "../../stores/i18n";
import { networkStore, type ProxyMode } from "../../stores/network";

const MODES: { id: ProxyMode; key: "network.mode.system" | "network.mode.manual" | "network.mode.none" }[] = [
  { id: "system", key: "network.mode.system" },
  { id: "manual", key: "network.mode.manual" },
  { id: "none", key: "network.mode.none" },
];

export function NetworkPage() {
  const [draft, setDraft] = createSignal<ProxyMode>("system");
  const [urlDraft, setUrlDraft] = createSignal("");
  const [saved, setSaved] = createSignal(false);
  // The page waits for the saved value: rendering first let a late load
  // overwrite a change the user had already made.
  const [ready, setReady] = createSignal(false);

  onMount(async () => {
    await networkStore.load();
    setDraft(networkStore.settings().proxyMode);
    setUrlDraft(networkStore.settings().proxyUrl);
    setReady(true);
  });

  const save = async (mode: ProxyMode, url: string) => {
    try {
      await networkStore.save({ proxyMode: mode, proxyUrl: url });
      const stored = networkStore.settings();
      // The server may refuse a manual mode without a usable URL; follow that
      // decision, but never overwrite what the user is still typing.
      if (stored.proxyMode !== mode) {
        setDraft(stored.proxyMode);
        setUrlDraft(stored.proxyUrl);
      }
      setSaved(true);
    } catch (error) {
      setSaved(false);
    }
  };

  return (
    <Show when={ready()} fallback={<p class="preferences-hint">{i18n.t("network.loading")}</p>}>
      <div class="preferences-field">
        <label class="preferences-field-label" for="preferences-proxy-mode">
          {i18n.t("network.proxyMode")}
        </label>
        <select
          id="preferences-proxy-mode"
          class="theme-select preferences-proxy-mode"
          aria-label={i18n.t("network.proxyMode")}
          value={draft()}
          onChange={(event) => {
            const mode = event.currentTarget.value as ProxyMode;
            setDraft(mode);
            setSaved(false);
            void save(mode, urlDraft());
          }}
        >
          {MODES.map((mode) => (
            <option value={mode.id}>{i18n.t(mode.key)}</option>
          ))}
        </select>
        <p class="preferences-hint">{i18n.t("network.proxyHint")}</p>
      </div>

      <div class="preferences-field">
        <label class="preferences-field-label" for="preferences-proxy-url">
          {i18n.t("network.proxyUrl")}
        </label>
        <input
          id="preferences-proxy-url"
          class="preferences-text-input preferences-proxy-url"
          aria-label={i18n.t("network.proxyUrl")}
          value={urlDraft()}
          disabled={draft() !== "manual"}
          onChange={(event) => {
            setUrlDraft(event.currentTarget.value);
            setSaved(false);
            void save(draft(), event.currentTarget.value);
          }}
        />
      </div>

      <div class="preferences-field">
        <button
          type="button"
          class="toolbar-btn text-btn preferences-network-test"
          disabled={networkStore.probing()}
          onClick={() => void networkStore.test(draft(), urlDraft())}
        >{networkStore.probing() ? i18n.t("network.testing") : i18n.t("network.test")}</button>
        <Show when={networkStore.probe()}>
          {(result) => (
            <p class="preferences-hint preferences-network-result" classList={{ ok: result().ok }}>
              {result().ok
                ? i18n.t("network.testOk", { ms: result().ms, status: result().status ?? 0 })
                : i18n.t("network.testFailed", { ms: result().ms, error: result().error ?? "" })}
            </p>
          )}
        </Show>
        <Show when={saved()}>
          <p class="preferences-hint preferences-network-saved">{i18n.t("network.saved")}</p>
        </Show>
      </div>
    </Show>
  );
}
