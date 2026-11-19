/**
 * A verified route found by the connection diagnosis.
 *
 * The notice only appears for a route that answered the plugin index, so it
 * never offers a proxy whose upstream is dead.
 */
import { Show } from "solid-js";
import { i18n } from "../stores/i18n";
import { networkStore } from "../stores/network";

export function ProxySuggestion() {
  return (
    <Show when={networkStore.offer()}>
      {(target) => (
        <div
          class="proxy-suggestion"
          role="status"
          data-proxy-suggestion={target().url ?? "direct"}
        >
          <span class="proxy-suggestion-text">
            {target().kind === "proxy"
              ? i18n.t("network.proxyWorks", { url: target().url ?? "" })
              : i18n.t("network.directWorks")}
          </span>
          <button type="button" onClick={() => void networkStore.applyOffer(target())}>
            {target().kind === "proxy"
              ? i18n.t("network.useThisProxy")
              : i18n.t("network.connectDirectly")}
          </button>
        </div>
      )}
    </Show>
  );
}
