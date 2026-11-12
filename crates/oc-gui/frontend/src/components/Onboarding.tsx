/**
 * First-run splash and guide. The splash is painted before the workspace so a
 * new install never flashes the empty DAW; the guide then explains the three
 * bottom panels, offers the AI settings page, and recommends plugins from the
 * signed catalog.
 */
import { aiConfig } from "../ai/config";
import { preferencesUi } from "../stores/preferencesUi";
import { useDialogFocus } from "./dialogFocus";
import { useTopmostEscape } from "./modalEscape";
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { LOGO_MARK } from "../stores/branding";
import { i18n } from "../stores/i18n";
import {
  entryInstallable,
  entryLicense,
  hostInstall,
  isEntryInstalled,
  pluginStore,
} from "../stores/pluginStore";
import type { PluginCatalogEntry } from "../stores/api";
import { PlugIcon } from "./icons";

type Step = 0 | 1 | 2;

const STEP_COUNT = 3;

const WORKSPACES = [
  { key: "C", shot: "clips", title: "onboarding.basics.clips", body: "onboarding.basics.clipsBody" },
  { key: "R", shot: "piano", title: "onboarding.basics.piano", body: "onboarding.basics.pianoBody" },
  { key: "P", shot: "presets", title: "onboarding.basics.presets", body: "onboarding.basics.presetsBody" },
] as const;

/** Captures exist in English and Chinese; other locales fall back to English. */
function shot(name: string): string {
  return `/onboarding/${i18n.locale().startsWith("zh") ? "zh" : "en"}/${name}.png`;
}

export function Splash() {
  return (
    <div class="onboarding-splash" role="status" aria-label={i18n.t("onboarding.splash")}>
      <div class="onboarding-splash-lockup">
        <span
          class="onboarding-splash-mark"
          role="img"
          aria-label="OpenCompose"
          style={{
            "mask-image": `url(${LOGO_MARK})`,
            "-webkit-mask-image": `url(${LOGO_MARK})`,
          }}
        />
        <span class="onboarding-splash-name">OpenCompose</span>
        <span class="onboarding-splash-hint">{i18n.t("onboarding.splashHint")}</span>
      </div>
      <span class="onboarding-splash-ring" aria-hidden="true" />
    </div>
  );
}







const RECOMMENDED_IDS = [
  "surge-synth-team.surge-xt",
  "asb2m10.dexed",
  "sfztools.sfizz",
  "valhalla-dsp.supermassive",
  "tokyo-dawn.tdr-nova",
  // These two are optional index entries: they appear only when the signed
  // catalog publishes a managed runtime for them.
  "spleeter",
  "ffmpeg",
];

export function Onboarding(props: { onDone: () => void }) {
  let dialog: HTMLDivElement | undefined;
  const [step, setStep] = createSignal<Step>(0);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Record<string, { text: string; error: boolean }>>({});

  const isTopmost = useTopmostEscape(props.onDone, () => !busy());
  useDialogFocus(() => dialog, isTopmost);

  const title = createMemo(() => {
    switch (step()) {
      case 0: return i18n.t("onboarding.basics.title");
      case 1: return i18n.t("onboarding.ai.title");
      default: return i18n.t("onboarding.plugins.title");
    }
  });

  const platform = () => pluginStore.catalog()?.platform ?? "macos-aarch64";

  const isInstalled = (entry: PluginCatalogEntry): boolean =>
    entry.installed ?? isEntryInstalled(entry, pluginStore.catalog()?.installed ?? []);

  const recommended = createMemo<PluginCatalogEntry[]>(() => {
    const entries = pluginStore.catalog()?.entries ?? [];
    return RECOMMENDED_IDS
      .map((id) => entries.find((entry) => entry.id === id))
      .filter((entry): entry is PluginCatalogEntry => entry !== undefined)
      .filter((entry) => isInstalled(entry) || entryInstallable(entry, platform()));
  });

  const bytesOf = (entry: PluginCatalogEntry): number | undefined =>
    hostInstall(entry, platform())?.bytes;

  const messageFor = (id: string) => messages()[id];

  onMount(() => {
    // Use the bundled or cached catalog: the guide must paint on a first run
    // with no network. The Store's explicit refresh remains the network path.
    void pluginStore.load();
  });

  const install = async (entry: PluginCatalogEntry) => {
    if (busy()) return;
    setBusy(entry.id);
    setMessages((previous) => ({ ...previous, [entry.id]: { text: "", error: false } }));
    try {
      const outcome = await pluginStore.install(entry);
      if (outcome.status === "installed") {
        await pluginStore.load(true);
      } else {
        setMessages((previous) => ({
          ...previous,
          [entry.id]: {
            text: i18n.t(
              outcome.status === "needs_confirmation"
                ? "onboarding.plugins.needsConfirmation"
                : "onboarding.plugins.opened",
              { name: entry.name },
            ),
            error: false,
          },
        }));
      }
    } catch (cause) {
      setMessages((previous) => ({
        ...previous,
        [entry.id]: { text: i18n.t("onboarding.plugins.failed", { error: String(cause) }), error: true },
      }));
    } finally {
      setBusy(null);
    }
  };

  const formatBytes = (bytes: number | undefined): string => {
    if (!bytes) return "";
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const next = () => {
    if (busy()) return;
    if (step() < 2) setStep((step() + 1) as Step);
    else props.onDone();
  };

  const back = () => {
    if (busy()) return;
    if (step() > 0) setStep((step() - 1) as Step);
  };

  return (
    <div ref={dialog} tabIndex={-1} class="onboarding-backdrop" role="dialog" aria-modal="true" aria-labelledby="onboarding-title">
      <div class="onboarding-card">
        <header class="onboarding-head">
          <div class="onboarding-brand">
            <span
              class="onboarding-brand-mark"
              role="img"
              aria-label="OpenCompose"
              style={{
                "mask-image": `url(${LOGO_MARK})`,
                "-webkit-mask-image": `url(${LOGO_MARK})`,
              }}
            />
            <span class="onboarding-brand-name">OpenCompose</span>
          </div>
          <button type="button" class="onboarding-skip" disabled={busy() !== null} onClick={() => { if (!busy()) props.onDone(); }}>
            {i18n.t("onboarding.skip")}
          </button>
        </header>

        <div class="onboarding-step-meta">
          <span class="onboarding-kicker">{i18n.t("onboarding.step", { current: step() + 1, total: STEP_COUNT })}</span>
          <div class="onboarding-dots" aria-hidden="true">
            <For each={[0, 1, 2]}>
              {(index) => <span class="onboarding-dot" classList={{ active: index === step(), done: index < step() }} />}
            </For>
          </div>
        </div>

        <h1 class="onboarding-title" id="onboarding-title">{title()}</h1>

        <Show when={step() === 0}>
          <div class="onboarding-map">
            <div class="onboarding-timeline-block">
              <div class="onboarding-shot">
                <img src={shot("timeline")} alt={i18n.t("onboarding.basics.timeline")} />
                <span class="onboarding-shot-badge" style={{ left: "30%", top: "5%" }}>1</span>
                <span class="onboarding-shot-badge" style={{ left: "9%", top: "38%" }}>2</span>
                <span class="onboarding-shot-badge" style={{ left: "30%", top: "82%" }}>3</span>
              </div>
              <ul class="onboarding-legend">
                <li><b>1</b>{i18n.t("onboarding.basics.bar")}</li>
                <li><b>2</b>{i18n.t("onboarding.basics.track")}</li>
                <li><b>3</b>{i18n.t("onboarding.basics.clip")}</li>
              </ul>
            </div>
            <div class="onboarding-map-panels">
              <For each={WORKSPACES}>
                {(workspace) => (
                  <div class="onboarding-map-panel">
                    <div class="onboarding-panel-head">
                      <kbd>{workspace.key}</kbd>
                      <h2>{i18n.t(workspace.title)}</h2>
                    </div>
                    <img class="onboarding-panel-shot" src={shot(workspace.shot)} alt={i18n.t(workspace.title)} />
                    <p>{i18n.t(workspace.body)}</p>
                  </div>
                )}
              </For>
            </div>
          </div>
          <p class="onboarding-footnote">{i18n.t("onboarding.basics.space")}</p>
        </Show>

        <Show when={step() === 1}>
          <p class="onboarding-body">{i18n.t("onboarding.ai.body")}</p>
          <div class="onboarding-providers" role="listbox" aria-label={i18n.t("onboarding.ai.title")}>
            <For each={aiConfig.config.providers}>
              {(provider) => {
                const active = () => aiConfig.getActiveProvider().id === provider.id;
                return (
                  <button
                    type="button"
                    role="option"
                    aria-selected={active()}
                    class="onboarding-provider"
                    classList={{ active: active() }}
                    onClick={() => aiConfig.setActiveProvider(provider.id)}
                  >
                    <span class="onboarding-provider-name">{provider.name}</span>
                    <span class="onboarding-provider-model">{provider.model}</span>
                  </button>
                );
              }}
            </For>
          </div>
          <Show when={aiConfig.config.providers.length === 0}>
            <p class="onboarding-body">{i18n.t("onboarding.ai.empty")}</p>
          </Show>
          <div class="onboarding-ai-actions">
            <button
              type="button"
              class="onboarding-secondary"
              onClick={(event) => preferencesUi.openOn("ai", "connection", event.currentTarget)}
            >
              {i18n.t("onboarding.ai.setup")}
            </button>
            <span class="onboarding-ai-path">{i18n.t("onboarding.ai.status")}</span>
          </div>
          <p class="onboarding-footnote">{i18n.t("onboarding.ai.later")}</p>
        </Show>

        <Show when={step() === 2}>
          <p class="onboarding-body">{i18n.t("onboarding.plugins.body")}</p>
          <div class="onboarding-tools">
            <Show when={pluginStore.loading()}>
              <p class="onboarding-tools-loading">{i18n.t("onboarding.plugins.loading")}</p>
            </Show>
            <Show when={pluginStore.error()}>
              {(error) => (
                <p class="onboarding-tools-error" role="alert">
                  {i18n.t("onboarding.plugins.error", { error: error() })}
                </p>
              )}
            </Show>
            <Show when={!pluginStore.loading() && !pluginStore.error() && recommended().length === 0}>
              <p class="onboarding-tools-loading">
                {i18n.t("onboarding.plugins.empty")} {i18n.t("onboarding.plugins.emptyHint")}
              </p>
            </Show>
            <For each={recommended()}>
              {(entry) => {
                const running = () => busy() === entry.id;
                const installed = () => isInstalled(entry);
                return (
                  <article class="onboarding-tool" classList={{ installed: installed() }}>
                    <span class="onboarding-tool-icon" aria-hidden="true"><PlugIcon /></span>
                    <div class="onboarding-tool-copy">
                      <div class="onboarding-tool-head">
                        <h2>{entry.name}</h2>
                        <span class="onboarding-tool-size">{formatBytes(bytesOf(entry))}</span>
                        <span class="onboarding-tool-license">{entryLicense(entry)}</span>
                      </div>
                      <p>{entry.description || entry.vendor}</p>
                      <Show when={running()}>
                        <div class="onboarding-progress" role="status" aria-label={i18n.t("onboarding.plugins.installing")}>
                          <span class="onboarding-progress-bar indeterminate" />
                        </div>
                        <p class="onboarding-tool-message">{i18n.t("onboarding.plugins.installing")}</p>
                      </Show>
                      <Show when={messageFor(entry.id)}>
                        {(message) => (
                          <p class="onboarding-tool-message" classList={{ error: message().error }} role={message().error ? "alert" : "status"}>
                            {message().text}
                          </p>
                        )}
                      </Show>
                    </div>
                    <div class="onboarding-tool-action">
                      <Show
                        when={!installed()}
                        fallback={<span class="onboarding-badge">{i18n.t("onboarding.plugins.installed")}</span>}
                      >
                        <button
                          type="button"
                          class="onboarding-secondary"
                          disabled={busy() !== null}
                          onClick={() => void install(entry)}
                        >
                          {running() ? i18n.t("onboarding.plugins.installing") : i18n.t("onboarding.plugins.install")}
                        </button>
                      </Show>
                    </div>
                  </article>
                );
              }}
            </For>
          </div>
        </Show>

        <footer class="onboarding-actions">
          <button
            type="button"
            class="onboarding-ghost"
            disabled={step() === 0 || busy() !== null}
            onClick={back}
          >
            {i18n.t("onboarding.back")}
          </button>
          <div class="onboarding-actions-right">
            <button
              type="button"
              class="onboarding-primary"
              disabled={busy() !== null}
              onClick={() => (step() === 2 ? props.onDone() : next())}
            >
              {step() === 0
                ? i18n.t("onboarding.next")
                : step() === 1
                  ? i18n.t("onboarding.ai.defer")
                  : i18n.t("onboarding.plugins.defer")}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
