/**
 * Connection help after a failed index fetch.
 *
 * It diagnoses the saved route, a direct connection and local proxy candidates
 * once, then offers only a route that the plugin index answered.
 */
import { Show, createSignal, onMount } from "solid-js";
import { i18n } from "../stores/i18n";
import { networkStore } from "../stores/network";
import { preferencesUi } from "../stores/preferencesUi";

export function ConnectionHelp(props: { onRetry: () => Promise<unknown> | void }) {
  const [applying, setApplying] = createSignal(false);
  const [actionError, setActionError] = createSignal<string | null>(null);

  onMount(() => {
    void networkStore.diagnose();
  });

  const apply = async () => {
    if (applying()) return;
    setApplying(true);
    setActionError(null);
    try {
      await networkStore.applyOffer();
      await props.onRetry();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setApplying(false);
    }
  };

  return (
    <section class="connection-help" aria-live="polite">
      <Show
        when={!networkStore.diagnosing()}
        fallback={<p class="connection-help-checking" role="status">{i18n.t("network.checkingConnections")}</p>}
      >
        <Show
          when={networkStore.offer()}
          fallback={
            <Show
              when={networkStore.failingProxies()[0]}
              fallback={
                <Show
                  when={networkStore.diagnoseError()}
                  fallback={<p class="connection-help-note">{i18n.t("network.noWorkingConnection")}</p>}
                >
                  {(error) => (
                    <p class="connection-help-note" role="alert">
                      {i18n.t("network.checkFailed", { error: error() })}
                    </p>
                  )}
                </Show>
              }
            >
              {(route) => (
                <p class="connection-help-note">
                  {i18n.t("network.proxyFailing", { url: route().url ?? "" })}
                </p>
              )}
            </Show>
          }
        >
          {(target) => (
            <div class="connection-help-offer">
              <p class="connection-help-note">
                {target().kind === "proxy"
                  ? i18n.t("network.proxyWorks", { url: target().url ?? "" })
                  : i18n.t("network.directWorks")}
              </p>
              <button type="button" class="connection-help-use" disabled={applying()} onClick={() => void apply()}>
                {target().kind === "proxy"
                  ? i18n.t("network.useThisProxy")
                  : i18n.t("network.connectDirectly")}
              </button>
            </div>
          )}
        </Show>
        <Show when={actionError()}>
          {(error) => (
            <p class="connection-help-note connection-help-error" role="alert">
              {i18n.t("network.applyFailed", { error: error() })}
            </p>
          )}
        </Show>
      </Show>
      <button
        type="button"
        class="connection-help-settings"
        onClick={(event) => preferencesUi.openOn("network", "connection", event.currentTarget)}
      >
        {i18n.t("network.openNetworkSettings")}
      </button>
    </section>
  );
}
