/**
 * The Network page: which proxy outbound requests use, and a probe that proves
 * the choice against a real host.
 */
import { For, Show, createSignal, onMount } from "solid-js";
import { i18n } from "../../stores/i18n";
import { networkStore, type ProxyMode, type RouteProbe } from "../../stores/network";

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

  const routeLabel = (route: RouteProbe): string => {
    if (route.kind === "proxy") return i18n.t("network.routeProxy", { url: route.url ?? "" });
    if (route.kind === "direct") return i18n.t("network.routeDirect");
    return i18n.t("network.routeSystem");
  };

  const useRoute = async (route: RouteProbe) => {
    try {
      await networkStore.applyRoute(route);
      const stored = networkStore.settings();
      setDraft(stored.proxyMode);
      setUrlDraft(stored.proxyUrl);
      setSaved(true);
    } catch {
      setSaved(false);
    }
  };

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

      <div class="preferences-field">
        <button
          type="button"
          class="toolbar-btn text-btn preferences-network-find"
          disabled={networkStore.diagnosing()}
          onClick={() => void networkStore.diagnose()}
        >{networkStore.diagnosing() ? i18n.t("network.checkingConnections") : i18n.t("network.findProxy")}</button>
        <Show when={networkStore.diagnosis()}>
          {(result) => (
            <ul class="preferences-network-routes">
              <For each={result().routes}>
                {(route) => (
                  <li class="preferences-network-route" classList={{ ok: route.ok }}>
                    <span class="preferences-network-route-label">{routeLabel(route)}</span>
                    <span class="preferences-network-route-status">
                      {route.ok
                        ? i18n.t("network.routeOk", { ms: route.ms })
                        : i18n.t("network.routeFailed", { ms: route.ms, error: route.error ?? "" })}
                    </span>
                    <Show when={route.ok}>
                      <button
                        type="button"
                        class="toolbar-btn text-btn preferences-network-use"
                        onClick={() => void useRoute(route)}
                      >{i18n.t("network.use")}</button>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          )}
        </Show>
        <Show when={networkStore.diagnoseError()}>
          {(error) => (
            <p class="preferences-hint" role="alert">
              {i18n.t("network.checkFailed", { error: error() })}
            </p>
          )}
        </Show>
      </div>
    </Show>
  );
}
