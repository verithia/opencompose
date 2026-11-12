import { Show, createMemo, createSignal, onMount } from "solid-js";
import * as api from "../../stores/api";
import { i18n } from "../../stores/i18n";
import "../SampleMarketPanel.css";

export function FreesoundConnection() {
  const [status, setStatus] = createSignal("");
  const [freesound, setFreesound] = createSignal<api.FreesoundOAuthStatus | null>(null);
  const [freesoundClientId, setFreesoundClientId] = createSignal("");
  const [freesoundClientSecret, setFreesoundClientSecret] = createSignal("");
  const [freesoundBusy, setFreesoundBusy] = createSignal(false);
  const freesoundConnected = createMemo(() => freesound()?.state === "connected");

  /** The connection state as one localized line. */
  const freesoundHint = createMemo(() => {
    const state = freesound();
    if (!state) return "";
    if (state.state === "connected") return i18n.t("sample.freesound.connected");
    if (state.state === "expired") {
      return i18n.t(
        state.has_refresh_token
          ? "sample.freesound.expired"
          : "sample.freesound.expiredNoRefresh",
      );
    }
    return i18n.t("sample.freesound.disconnected");
  });

  async function loadFreesound() {
    try {
      setFreesound(await api.freesoundOAuthStatus());
    } catch {
      setStatus(i18n.t("sources.statusError"));
    }
  }

  async function connectFreesound() {
    if (freesoundBusy()) return;
    setFreesoundBusy(true);
    setStatus("");
    try {
      const next = await api.freesoundOAuthBegin(
        freesoundClientId().trim(),
        freesoundClientSecret().trim(),
      );
      setFreesound(next);
      setFreesoundClientSecret("");
      setStatus(i18n.t("sample.freesound.connectedStatus"));
    } catch (error) {
      setStatus(i18n.t("sample.freesound.failed", { message: String(error) }));
    } finally {
      setFreesoundBusy(false);
    }
  }

  async function disconnectFreesound() {
    if (freesoundBusy()) return;
    setFreesoundBusy(true);
    setStatus("");
    try {
      setFreesound(await api.freesoundOAuthSignOut());
      setStatus(i18n.t("sample.freesound.disconnectedStatus"));
    } catch (error) {
      setStatus(i18n.t("sample.freesound.failed", { message: String(error) }));
    } finally {
      setFreesoundBusy(false);
    }
  }

  onMount(() => void loadFreesound());

  return (
    <>
      <div class="sample-market-key sample-market-oauth">
        <span class="sample-market-key-label">
          {i18n.t("sample.freesound.title")}
        </span>
        <span class="sample-market-oauth-status" role="status">
          {freesoundHint()}
        </span>
        <Show when={!freesoundConnected()}>
          <input
            class="sample-market-search"
            type="text"
            value={freesoundClientId()}
            placeholder={i18n.t(
              freesound()?.client_configured
                ? "sample.freesound.credentialsStored"
                : "sample.freesound.clientId",
            )}
            aria-label={i18n.t("sample.freesound.clientId")}
            onInput={(event) => setFreesoundClientId(event.currentTarget.value)}
          />
          <input
            class="sample-market-search"
            type="password"
            value={freesoundClientSecret()}
            placeholder={i18n.t(
              freesound()?.client_configured
                ? "sample.freesound.credentialsStored"
                : "sample.freesound.clientSecret",
            )}
            aria-label={i18n.t("sample.freesound.clientSecret")}
            onInput={(event) => setFreesoundClientSecret(event.currentTarget.value)}
          />
          <button
            class="toolbar-btn text-btn"
            type="button"
            disabled={freesoundBusy()}
            onClick={() => void connectFreesound()}
          >
            {i18n.t(
              freesoundBusy()
                ? "sample.freesound.connecting"
                : "sample.freesound.connect",
            )}
          </button>
        </Show>
        <Show when={freesoundConnected()}>
          <button
            class="toolbar-btn text-btn"
            type="button"
            disabled={freesoundBusy()}
            onClick={() => void disconnectFreesound()}
          >
            {i18n.t("sample.freesound.disconnect")}
          </button>
        </Show>
      </div>
      <Show when={status()}><p class="preferences-hint" role="status">{status()}</p></Show>
    </>
  );
}
