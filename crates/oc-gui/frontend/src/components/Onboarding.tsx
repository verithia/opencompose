/**
 * First-run splash and guide. The splash is painted before the workspace so a
 * new install never flashes the empty DAW. The guide opens on a scan of this
 * computer's plugins and sounds; when it finds few, the next page suggests
 * free plugins and starter sounds. Then it explains the three bottom panels
 * and offers the AI settings page.
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
import * as api from "../stores/api";
import type { PluginCatalogEntry, SoundfontItem } from "../stores/api";
import { pickRecommendations } from "../stores/onboardingPicks";
import { localScan } from "../stores/localScan";
import { needsPluginPicks, pickStarterSounds, type StarterSound } from "../stores/starterSounds";
import { FolderIcon, KeyboardIcon, PlugIcon, WaveIcon } from "./icons";
import { OnboardingScan, chooseFolder } from "./OnboardingScan";

type Step = "scan" | "picks" | "basics" | "ai";

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


/** Packs the guide offers at most; the Store lists the rest. */
const MAX_PACK_PICKS = 2;

export function Onboarding(props: { onDone: () => void }) {
  let dialog: HTMLDivElement | undefined;
  const [step, setStep] = createSignal<Step>("scan");
  const [busy, setBusy] = createSignal<string | null>(null);
  const [folderBusy, setFolderBusy] = createSignal(false);
  const [messages, setMessages] = createSignal<Record<string, { text: string; error: boolean }>>({});

  const isTopmost = useTopmostEscape(props.onDone, () => !busy());
  useDialogFocus(() => dialog, isTopmost);

  const platform = () => pluginStore.catalog()?.platform ?? "macos-aarch64";

  const isInstalled = (entry: PluginCatalogEntry): boolean =>
    entry.installed ?? isEntryInstalled(entry, pluginStore.catalog()?.installed ?? []);

  const recommended = createMemo(() =>
    pickRecommendations(
      pluginStore.catalog()?.entries ?? [],
      isInstalled,
      (entry) => entryInstallable(entry, platform()),
    ),
  );

  const [packs, setPacks] = createSignal<SoundfontItem[]>([]);

  /** Until the scan answers, both suggestion lists stay available. */
  const showPluginPicks = () => {
    const summary = localScan.summary();
    return !summary || needsPluginPicks(summary);
  };
  const starters = createMemo<StarterSound[]>(() => {
    const summary = localScan.summary();
    return summary ? pickStarterSounds(summary) : [];
  });
  const showSoundPicks = () => !localScan.summary() || starters().length > 0;

  const steps = createMemo<Step[]>(() =>
    showPluginPicks() || showSoundPicks() ? ["scan", "picks", "basics", "ai"] : ["scan", "basics", "ai"],
  );
  /** A step that disappears while it is shown resolves to the next one. */
  const position = () => {
    const index = steps().indexOf(step());
    return index >= 0 ? index : steps().indexOf("basics");
  };
  const current = () => steps()[position()];
  const isLast = () => position() === steps().length - 1;

  const title = createMemo(() => {
    switch (current()) {
      case "scan":
        return i18n.t(localScan.summary() && !localScan.running() ? "onboarding.scan.titleDone" : "onboarding.scan.title");
      case "picks": return i18n.t("onboarding.picks.title");
      case "basics": return i18n.t("onboarding.basics.title");
      default: return i18n.t("onboarding.ai.title");
    }
  });

  const loadPacks = async () => {
    try {
      const listing = await api.soundfontsList();
      setPacks(
        (listing?.items ?? [])
          .filter((item) => item.source === "index" && !item.installed)
          .slice(0, MAX_PACK_PICKS),
      );
    } catch {
      // Packs are an optional extra; the plugin picks stand on their own.
      setPacks([]);
    }
  };

  const addSoundFolder = async () => {
    if (folderBusy()) return;
    const chosen = await chooseFolder("sounds");
    if (!chosen) return;
    setFolderBusy(true);
    try {
      await localScan.addFolder("sounds", chosen);
      setMessages((previous) => ({ ...previous, folder: { text: "", error: false } }));
    } catch (cause) {
      setMessages((previous) => ({
        ...previous,
        folder: { text: i18n.t("onboarding.local.failed", { error: String(cause) }), error: true },
      }));
    } finally {
      setFolderBusy(false);
    }
  };

  const openStarter = async (sound: StarterSound) => {
    try {
      await api.openUrl(sound.url);
      setMessages((previous) => ({
        ...previous,
        [sound.id]: { text: i18n.t("onboarding.starters.opened", { name: sound.name }), error: false },
      }));
    } catch (cause) {
      setMessages((previous) => ({
        ...previous,
        [sound.id]: { text: i18n.t("onboarding.starters.failed", { error: String(cause) }), error: true },
      }));
    }
  };

  const bytesOf = (entry: PluginCatalogEntry): number | undefined =>
    hostInstall(entry, platform())?.bytes;

  const messageFor = (id: string) => messages()[id];

  onMount(() => {
    // Use the bundled or cached catalog: the guide must paint on a first run
    // with no network. The Store's explicit refresh remains the network path.
    void pluginStore.load();
    void loadPacks();
    if (!localScan.running()) void localScan.start();
  });

  const installPack = async (item: SoundfontItem) => {
    if (busy()) return;
    setBusy(item.id);
    setMessages((previous) => ({ ...previous, [item.id]: { text: "", error: false } }));
    try {
      await api.soundfontInstall(item.id);
      await loadPacks();
    } catch (cause) {
      setMessages((previous) => ({
        ...previous,
        [item.id]: { text: i18n.t("onboarding.plugins.failed", { error: String(cause) }), error: true },
      }));
    } finally {
      setBusy(null);
    }
  };

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
    if (isLast()) props.onDone();
    else setStep(steps()[position() + 1]);
  };

  const back = () => {
    if (busy()) return;
    if (position() > 0) setStep(steps()[position() - 1]);
  };

  const primaryLabel = () => {
    switch (current()) {
      case "picks": return i18n.t("onboarding.plugins.defer");
      case "ai": return i18n.t("onboarding.ai.defer");
      default: return i18n.t("onboarding.next");
    }
  };

  const starterKind = (sound: StarterSound) => i18n.t(`onboarding.starters.kind.${sound.kind}`);

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
          <span class="onboarding-kicker">{i18n.t("onboarding.step", { current: position() + 1, total: steps().length })}</span>
          <div class="onboarding-dots" aria-hidden="true">
            <For each={steps()}>
              {(_, index) => <span class="onboarding-dot" classList={{ active: index() === position(), done: index() < position() }} />}
            </For>
          </div>
        </div>

        <h1 class="onboarding-title" id="onboarding-title">{title()}</h1>

        <Show when={current() === "scan"}>
          <p class="onboarding-body">{i18n.t("onboarding.scan.body")}</p>
          <OnboardingScan busy={folderBusy() || busy() !== null} onBusy={setFolderBusy} />
        </Show>

        <Show when={current() === "basics"}>
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

        <Show when={current() === "ai"}>
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

        <Show when={current() === "picks"}>
          <p class="onboarding-body">
            {i18n.t(localScan.summary() ? "onboarding.picks.bodyFew" : "onboarding.picks.body")}
          </p>
          <Show when={showPluginPicks()}>
          <section class="onboarding-picks" aria-labelledby="onboarding-picks-title">
            <h2 class="onboarding-section-title" id="onboarding-picks-title">{i18n.t("onboarding.picks.plugins")}</h2>
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
              <Show when={!pluginStore.loading() && !pluginStore.error() && recommended().length === 0 && packs().length === 0}>
                <p class="onboarding-tools-loading">
                  {i18n.t("onboarding.plugins.empty")} {i18n.t("onboarding.plugins.emptyHint")}
                </p>
              </Show>
              <For each={recommended()}>
                {({ role, entry }) => {
                  const running = () => busy() === entry.id;
                  const installed = () => isInstalled(entry);
                  return (
                    <article class="onboarding-tool" classList={{ installed: installed() }}>
                      <span class="onboarding-tool-icon" aria-hidden="true"><PlugIcon /></span>
                      <div class="onboarding-tool-copy">
                        <div class="onboarding-tool-head">
                          <span class="onboarding-tool-role">{i18n.t(`onboarding.picks.${role}`)}</span>
                          <h2>{entry.name}</h2>
                          <span class="onboarding-tool-size">{formatBytes(bytesOf(entry))}</span>
                          <span class="onboarding-tool-license">{entryLicense(entry)}</span>
                        </div>
                        <p>{entry.description || entry.vendor}</p>
                        <Show when={!installed() && entry.install_mode === "installer"}>
                          <p class="onboarding-tool-reason">{i18n.t("onboarding.plugins.installer")}</p>
                        </Show>
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
          </section>
          </Show>

          <Show when={showSoundPicks() || packs().length > 0}>
          <section class="onboarding-picks" aria-labelledby="onboarding-starters-title">
            <div class="onboarding-section-head">
              <h2 class="onboarding-section-title" id="onboarding-starters-title">{i18n.t("onboarding.starters.title")}</h2>
              <button type="button" class="onboarding-secondary onboarding-local-add" disabled={folderBusy()} onClick={() => void addSoundFolder()}>
                <FolderIcon />
                {i18n.t("onboarding.local.add.sounds")}
              </button>
            </div>
            <Show when={messageFor("folder")?.error}>
              <p class="onboarding-tools-error" role="alert">{messageFor("folder")!.text}</p>
            </Show>
            <div class="onboarding-tools">
              <For each={packs()}>
                {(item) => {
                  const running = () => busy() === item.id;
                  return (
                    <article class="onboarding-tool">
                      <span class="onboarding-tool-icon" aria-hidden="true"><WaveIcon /></span>
                      <div class="onboarding-tool-copy">
                        <div class="onboarding-tool-head">
                          <span class="onboarding-tool-role">{i18n.t("onboarding.picks.pack")}</span>
                          <h2>{item.name}</h2>
                          <span class="onboarding-tool-size">{formatBytes(item.bytes)}</span>
                          <span class="onboarding-tool-license">{item.license.name}</span>
                        </div>
                        <p>{item.description}</p>
                        <Show when={running()}>
                          <div class="onboarding-progress" role="status" aria-label={i18n.t("onboarding.plugins.installing")}>
                            <span class="onboarding-progress-bar indeterminate" />
                          </div>
                        </Show>
                        <Show when={messageFor(item.id)}>
                          {(message) => (
                            <p class="onboarding-tool-message" classList={{ error: message().error }} role={message().error ? "alert" : "status"}>
                              {message().text}
                            </p>
                          )}
                        </Show>
                      </div>
                      <div class="onboarding-tool-action">
                        <button
                          type="button"
                          class="onboarding-secondary"
                          disabled={busy() !== null}
                          onClick={() => void installPack(item)}
                        >
                          {running() ? i18n.t("onboarding.plugins.installing") : i18n.t("onboarding.plugins.install")}
                        </button>
                      </div>
                    </article>
                  );
                }}
              </For>
              <For each={starters()}>
                {(sound) => (
                  <article class="onboarding-tool" data-starter={sound.id}>
                    <span class="onboarding-tool-icon" aria-hidden="true">
                      {sound.kind === "samples" ? <WaveIcon /> : <KeyboardIcon />}
                    </span>
                    <div class="onboarding-tool-copy">
                      <div class="onboarding-tool-head">
                        <span class="onboarding-tool-role">{starterKind(sound)}</span>
                        <h2>{sound.name}</h2>
                        <span class="onboarding-tool-size">{formatBytes(sound.bytes)}</span>
                        <span class="onboarding-tool-license">{sound.license}</span>
                      </div>
                      <p>{sound.description} · {sound.publisher}</p>
                      <Show when={messageFor(sound.id)}>
                        {(message) => (
                          <p class="onboarding-tool-message" classList={{ error: message().error }} role={message().error ? "alert" : "status"}>
                            {message().text}
                          </p>
                        )}
                      </Show>
                    </div>
                    <div class="onboarding-tool-action">
                      <button type="button" class="onboarding-secondary" onClick={() => void openStarter(sound)}>
                        {i18n.t("onboarding.starters.open")}
                      </button>
                    </div>
                  </article>
                )}
              </For>
            </div>
          </section>
          </Show>
        </Show>

        <footer class="onboarding-actions">
          <button
            type="button"
            class="onboarding-ghost"
            disabled={position() === 0 || busy() !== null}
            onClick={back}
          >
            {i18n.t("onboarding.back")}
          </button>
          <div class="onboarding-actions-right">
            <button
              type="button"
              class="onboarding-primary"
              disabled={busy() !== null}
              onClick={next}
            >
              {primaryLabel()}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
