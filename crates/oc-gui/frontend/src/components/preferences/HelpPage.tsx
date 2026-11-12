/**
 * The Help & About page: the version this build reports, the DSL version its
 * parser accepts, the public repository and the licence. The diagnostics
 * archive lives here because it exists to answer a bug report.
 */
import { agplLicense, mitLicense, apacheLicense } from "virtual:opencompose-licenses";
import { AiMarkdown } from "../AiMarkdown";
import manual from "../../../../../../docs/manual/gui.md?raw";
import { helpUi } from "../../stores/helpUi";
import { onboardingStore } from "../../stores/onboarding";
import { preferencesUi } from "../../stores/preferencesUi";
import { Show, createSignal, onMount } from "solid-js";
import { i18n } from "../../stores/i18n";
import { appInfo, openUrl, type AppInfo } from "../../stores/api";
import { diagnosticsStore } from "../../stores/diagnosticsUi";

const REPOSITORY_URL = "https://github.com/verithia/opencompose";

export function HelpPage() {
  const [info, setInfo] = createSignal<AppInfo | null>(null);
  const [infoError, setInfoError] = createSignal<string | null>(null);
  const [notices, setNotices] = createSignal<string | null>(null);

  // The notices cover every bundled crate, package and asset, so they load
  // only when the reader opens them.
  const loadNotices = (open: boolean) => {
    if (!open || notices() !== null) return;
    void import("virtual:opencompose-third-party")
      .then((module) => setNotices(module.thirdPartyNotices))
      .catch((error) => setNotices(String(error)));
  };

  onMount(() => {
    void appInfo()
      .then(setInfo)
      .catch((error) => setInfoError(String(error)));
  });

  return (
    <>
      <section class="preferences-help-resources" aria-label={i18n.t("preferences.help.resources")}>
        <div class="preferences-help-actions">
          <button type="button" class="toolbar-btn text-btn" onClick={() => void preferencesUi.requestClose(() => {
            preferencesUi.close();
            onboardingStore.reset();
          })}>{i18n.t("preferences.help.gettingStarted")}</button>
          <button type="button" class="toolbar-btn text-btn" onClick={() => preferencesUi.openOn("audio")}>
            {i18n.t("preferences.page.audio")}
          </button>
          <button type="button" class="toolbar-btn text-btn" onClick={() => preferencesUi.openOn("keymap")}>
            {i18n.t("preferences.page.keymap")}
          </button>
        </div>
        <details class="preferences-help-manual" open={helpUi.manualOpen()}
          onToggle={(event) => helpUi.setManualOpen(event.currentTarget.open)}>
          <summary>{i18n.t("preferences.help.offlineManual")}</summary>
          <Show when={helpUi.manualOpen()}><AiMarkdown content={manual} /></Show>
        </details>
      </section>
      <div class="preferences-divider" />
      <section class="preferences-facts" aria-label={i18n.t("preferences.page.help")}>
        <div class="preferences-fact">
          <span class="preferences-fact-label">{i18n.t("preferences.help.version")}</span>
          <span class="preferences-fact-value preferences-app-version">
            {info()?.version ?? (infoError() ? "—" : i18n.t("preferences.help.loading"))}
          </span>
        </div>
        <div class="preferences-fact">
          <span class="preferences-fact-label">{i18n.t("preferences.help.dslVersion")}</span>
          <span class="preferences-fact-value preferences-dsl-version">
            {info()?.dsl_version ?? (infoError() ? "—" : i18n.t("preferences.help.loading"))}
          </span>
        </div>
        <div class="preferences-fact">
          <span class="preferences-fact-label">{i18n.t("preferences.help.licence")}</span>
          <span class="preferences-fact-value preferences-licence">AGPL-3.0-or-later</span>
        </div>
        <div class="preferences-fact">
          <span class="preferences-fact-label">{i18n.t("preferences.help.repository")}</span>
          <span class="preferences-fact-value preferences-repository">
            <button
              type="button"
              class="preferences-repository-link"
              title={REPOSITORY_URL}
              onClick={() => void openUrl(REPOSITORY_URL)}
            >
              github.com/verithia/opencompose
            </button>
          </span>
        </div>
        <div class="preferences-fact"><span class="preferences-fact-label">{i18n.t("preferences.help.build")}</span>
          <span class="preferences-fact-value preferences-build-identity">{info() ? `${info()!.source_revision ?? "—"} · ${info()!.build_profile ?? "—"} · ${info()!.os ?? "—"}/${info()!.arch ?? "—"}` : "—"}</span>
        </div>
      </section>
      <details class="preferences-help-credits"><summary>{i18n.t("preferences.help.credits")}</summary>
        <p>OpenCompose contributors · Tauri · SolidJS · Rust audio ecosystem</p>
        <details><summary>OpenCompose · AGPL-3.0-or-later</summary><pre>{agplLicense}</pre></details>
        <details><summary>{i18n.t("preferences.help.languageComponents")} · MIT</summary><pre>{mitLicense}</pre></details>
        <details><summary>{i18n.t("preferences.help.languageComponents")} · Apache-2.0</summary><pre>{apacheLicense}</pre></details>
        <details class="preferences-third-party" onToggle={(event) => loadNotices(event.currentTarget.open)}>
          <summary>{i18n.t("preferences.help.thirdParty")}</summary>
          <pre class="selectable">{notices() ?? i18n.t("preferences.help.loading")}</pre>
        </details>
      </details>

      <Show when={infoError()} >
        {(message) => <p class="preferences-error selectable">{message()}</p>}
      </Show>

      <div class="preferences-divider" />

      <section class="diagnostics-section" aria-label={i18n.t("toolbar.diagnostics")}>
        <p id="diagnostics-contents" class="midi-hint">{i18n.t("preferences.help.diagnosticsContents")}</p>
        <button
          type="button"
          class="toolbar-btn text-btn diagnostics-btn"
          aria-label={i18n.t("toolbar.diagnostics")}
          aria-describedby="diagnostics-contents"
          disabled={diagnosticsStore.busy()}
          onClick={() => void diagnosticsStore.exportBundle()}
        >
          {diagnosticsStore.busy()
            ? i18n.t("toolbar.diagnosticsBusy")
            : i18n.t("toolbar.diagnostics")}
        </button>
        <Show when={diagnosticsStore.status()}>
          <p class="midi-hint diagnostics-status selectable">{diagnosticsStore.status()}</p>
        </Show>
      </section>
    </>
  );
}
