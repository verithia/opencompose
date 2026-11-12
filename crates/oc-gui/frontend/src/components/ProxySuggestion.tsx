/**
 * One-time notice for a proxy found on the usual local ports.
 *
 * It only appears after the network store detects a candidate and the user has
 * not dismissed that URL; applying it writes the manual proxy setting.
 */
import { Show } from "solid-js";
import { i18n } from "../stores/i18n";
import { networkStore } from "../stores/network";

export function ProxySuggestion() {
  return (
    <Show when={networkStore.suggestion()}>
      {(url) => (
        <div class="proxy-suggestion" role="status" data-proxy-suggestion={url()}>
          <span class="proxy-suggestion-text">
            {i18n.t("network.proxyDetected", { url: url() })}
          </span>
          <button type="button" onClick={() => void networkStore.useSuggestion()}>
            {i18n.t("network.useProxy")}
          </button>
          <button type="button" onClick={() => networkStore.dismiss()}>
            {i18n.t("network.dismiss")}
          </button>
        </div>
      )}
    </Show>
  );
}
