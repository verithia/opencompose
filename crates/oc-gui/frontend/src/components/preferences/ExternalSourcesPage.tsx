import { For, Show, createSignal, onMount } from "solid-js";
import * as api from "../../stores/api";
import { i18n, type I18nKey } from "../../stores/i18n";
import { notifySampleCredentialsChanged } from "../../stores/sampleCredentials";
import { FreesoundConnection } from "./FreesoundConnection";

const KEYED = [
  { id: "freesound", name: "Freesound", label: "sample.keyPlaceholder", docs: "https://freesound.org/docs/api/authentication.html" },
  { id: "jamendo", name: "Jamendo", label: "sample.freesound.clientId", docs: "https://developer.jamendo.com/v3.0/authentication" },
] as const;

const PUBLIC = [
  { name: "Openverse", docs: "https://api.openverse.org/v1/" },
  { name: "Wikimedia Commons", docs: "https://www.mediawiki.org/wiki/API:Search" },
  { name: "Internet Archive", docs: "https://archive.org/advancedsearch.php" },
] as const;

const KEY_STATUS: Record<api.SampleKeyStatus, I18nKey> = {
  saved: "sources.saved",
  environment: "sources.environment",
  missing: "sources.missing",
};

export function ExternalSourcesPage() {
  return (
    <>
      <h2 class="preferences-source-heading">{i18n.t("preferences.page.sources")}</h2>
      <p class="preferences-hint">{i18n.t("sources.hint")}</p>
      <p class="preferences-hint">{i18n.t("sources.localOnly")}</p>
      <For each={KEYED}>
        {(provider) => <SourceCredential provider={provider} />}
      </For>
      <section class="preferences-field">
        <FreesoundConnection />
      </section>
      <section class="preferences-field">
        <h3 class="preferences-field-label preferences-source-heading">{i18n.t("sources.public")}</h3>
        <div class="preferences-scale-row">
          <For each={PUBLIC}>{(provider) => <DocsButton url={provider.docs} name={provider.name} />}</For>
        </div>
      </section>
      <section class="preferences-field">
        <h3 class="preferences-field-label preferences-source-heading">Pixabay Audio</h3>
        <p class="preferences-hint">{i18n.t("sources.pixabay")}</p>
        <div><DocsButton url="https://pixabay.com/api/docs/" /></div>
      </section>
    </>
  );
}

function DocsButton(props: { url: string; name?: string }) {
  const [failed, setFailed] = createSignal(false);
  return (
    <>
      <button type="button" class="toolbar-btn text-btn" onClick={() => {
        setFailed(false);
        void api.openUrl(props.url).catch(() => setFailed(true));
      }}>
        {props.name ?? i18n.t("sources.docs")}
      </button>
      <Show when={failed()}><span role="alert" class="preferences-error">{i18n.t("sources.docsError")}</span></Show>
    </>
  );
}

function SourceCredential(props: { provider: (typeof KEYED)[number] }) {
  const [status, setStatus] = createSignal<api.SampleKeyStatus>();
  const [busy, setBusy] = createSignal(true);
  const [draft, setDraft] = createSignal("");
  const [error, setError] = createSignal<I18nKey>();
  const id = () => `external-source-${props.provider.id}`;

  async function refresh() {
    try {
      setStatus(await api.sampleMarketKeyStatus(props.provider.id));
    } catch {
      setError("sources.statusError");
    }
  }

  onMount(async () => {
    await refresh();
    setBusy(false);
  });

  async function update(remove = false) {
    const key = draft().trim();
    if (busy() || (!remove && !key)) return;
    setBusy(true);
    setError(undefined);
    try {
      if (remove) await api.sampleMarketRemoveKey(props.provider.id);
      else await api.sampleMarketSaveKey(props.provider.id, key);
      setDraft("");
      notifySampleCredentialsChanged();
      await refresh();
    } catch {
      setError("sources.updateError");
    } finally {
      setBusy(false);
    }
  }

  return (
    <form class="preferences-field" onSubmit={(event) => {
      event.preventDefault();
      void update();
    }}>
      <h3 class="preferences-field-label preferences-source-heading">{props.provider.name}</h3>
      <Show when={status()}>{(value) => (
        <p class="preferences-hint" role="status">{i18n.t(KEY_STATUS[value()])}</p>
      )}</Show>
      <label class="preferences-field-label" for={id()}>{i18n.t(props.provider.label)}</label>
      <input
        id={id()}
        type="password"
        class="preferences-text-input"
        autocomplete="off"
        value={draft()}
        disabled={busy() || status() === "environment"}
        onInput={(event) => setDraft(event.currentTarget.value)}
      />
      <div class="preferences-scale-row">
        <button type="submit" class="toolbar-btn text-btn" disabled={busy() || !draft().trim() || status() === "environment"}>
          {i18n.t("sample.keySave")}
        </button>
        <Show when={status() === "saved"}>
          <button type="button" class="toolbar-btn text-btn" disabled={busy()} onClick={() => void update(true)}>
            {i18n.t("sources.remove")}
          </button>
        </Show>
        <DocsButton url={props.provider.docs} />
      </div>
      <Show when={error()}>{(key) => <p class="preferences-error" role="alert">{i18n.t(key())}</p>}</Show>
    </form>
  );
}
