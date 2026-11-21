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
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { listen } from "@tauri-apps/api/event";
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
import type { PluginCatalogEntry, SoundPackItem, SoundPackPhase, SoundfontItem, SoundfontProgress } from "../stores/api";
import { pickRecommendations } from "../stores/onboardingPicks";
import { localScan } from "../stores/localScan";
import { soundPacks } from "../stores/soundPacks";
import { needsPluginPicks, needsSoundPicks, pickStarterSounds } from "../stores/starterSounds";
import { FolderIcon, KeyboardIcon, PlugIcon, WaveIcon } from "./icons";
import { OnboardingScan } from "./OnboardingScan";
import { chooseFolder, folderErrorText } from "./LocalFolders";
import { ConnectionHelp } from "./ConnectionHelp";

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

const PHASES: readonly SoundPackPhase[] = ["download", "verify", "extract"];

/** Sizes in binary units, with one decimal only where it carries information. */
export function formatBytes(bytes: number | undefined): string {
  if (!bytes || bytes <= 0) return "";
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  if (bytes < MB) return `${Math.max(1, Math.round(bytes / KB))} KB`;
  if (bytes < GB) {
    const mb = bytes / MB;
    return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
  }
  return `${(bytes / GB).toFixed(1)} GB`;
}

export function Onboarding(props: { onDone: () => void }) {
  let dialog: HTMLDivElement | undefined;
  const [step, setStep] = createSignal<Step>("scan");
  const [busy, setBusy] = createSignal<string | null>(null);
  const [folderBusy, setFolderBusy] = createSignal(false);
  const [indexRetrying, setIndexRetrying] = createSignal(false);
  const [messages, setMessages] = createSignal<Record<string, { text: string; error: boolean }>>({});
  /** Plugin installs whose cancel the user asked for, so the rejection reads as a cancel. */
  const cancelling = new Set<string>();

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
  /** Download progress per index SoundFont; the event carries bytes, not phases. */
  const [packProgress, setPackProgress] = createSignal<Record<string, SoundfontProgress>>({});

  /** Until the scan answers, both suggestion lists stay available. */
  const showPluginPicks = () => {
    const summary = localScan.summary();
    return !summary || needsPluginPicks(summary);
  };
  const showSoundPicks = () => {
    const summary = localScan.summary();
    return !summary || needsSoundPicks(summary);
  };
  /**
   * Catalog packs for this machine, plus every pack the user acted on here:
   * an install's own rescan may raise the counts, and its card keeps showing
   * the result instead of vanishing.
   */
  const starters = createMemo<SoundPackItem[]>(() => {
    const all = soundPacks.items();
    const picked = new Set(pickStarterSounds(localScan.summary(), all).map((item) => item.id));
    return all.filter((item) => picked.has(item.id) || soundPacks.stateOf(item.id) !== undefined);
  });
  /** Index SoundFonts the sound-pack catalog does not already offer. */
  const extraPacks = createMemo(() => {
    const offered = new Set(soundPacks.items().map((item) => item.id));
    return packs().filter((item) => !offered.has(item.id));
  });

  /** Once shown, the suggestions page stays, so a rescan never pulls it from under the user. */
  const [picksSeen, setPicksSeen] = createSignal(false);
  const steps = createMemo<Step[]>(() =>
    picksSeen() || showPluginPicks() || showSoundPicks() ? ["scan", "picks", "basics", "ai"] : ["scan", "basics", "ai"],
  );
  /** A step that disappears while it is shown resolves to the next one. */
  const position = () => {
    const index = steps().indexOf(step());
    return index >= 0 ? index : steps().indexOf("basics");
  };
  const current = () => steps()[position()];
  const isLast = () => position() === steps().length - 1;
  createEffect(() => {
    if (current() === "picks") setPicksSeen(true);
  });

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
        folder: { text: i18n.t("onboarding.local.failed", { error: folderErrorText(cause) }), error: true },
      }));
    } finally {
      setFolderBusy(false);
    }
  };

  const hasCatalogEntries = () => (pluginStore.catalog()?.entries.length ?? 0) > 0;

  /** The catalog has not answered yet; an error is an answer, not a pending one. */
  const catalogPending = () =>
    pluginStore.catalog() === null && !pluginStore.error() && !pluginStore.refreshError();

  const indexBusy = () =>
    pluginStore.loading() || pluginStore.refreshing() || indexRetrying();

  /** Nothing to show yet: the first load or the index fetch is still running. */
  const showIndexLoading = () => !hasCatalogEntries() && (indexBusy() || catalogPending());

  /** No catalog and no operation left to wait for: offer the reason and Try again. */
  const showOffline = () => !showIndexLoading() && !hasCatalogEntries();

  /** Fetch the index again, then read the catalog and sound packs the page draws from. */
  const retryIndex = async () => {
    if (indexRetrying() || pluginStore.refreshing()) return;
    setIndexRetrying(true);
    try {
      const outcome = await pluginStore.refresh(true);
      if (!outcome?.changed) await pluginStore.load(true);
      await soundPacks.load();
    } finally {
      setIndexRetrying(false);
    }
  };

  /** A first run with no cached catalog asks the index once, on the automatic path. */
  const openCatalog = async () => {
    await pluginStore.load();
    if (!hasCatalogEntries()) await pluginStore.refresh(false);
  };

  /** Opens a license or publisher page; a failure is reported on that card. */
  const openLink = async (id: string, url: string) => {
    setMessages((previous) => ({ ...previous, [`link:${id}`]: { text: "", error: false } }));
    try {
      await api.openUrl(url);
    } catch (cause) {
      setMessages((previous) => ({
        ...previous,
        [`link:${id}`]: { text: i18n.t("onboarding.starters.failed", { error: String(cause) }), error: true },
      }));
    }
  };

  /** One install at a time, whether it is a plugin or a pack. */
  const installLocked = () => busy() !== null || soundPacks.active() !== null;

  const bytesOf = (entry: PluginCatalogEntry): number | undefined =>
    hostInstall(entry, platform())?.bytes;

  const messageFor = (id: string) => messages()[id];

  let disposed = false;
  const stops: (() => void)[] = [];
  const keep = (stop: () => void) => {
    if (disposed) stop();
    else stops.push(stop);
  };
  onMount(() => {
    // Paint a cached catalog at once, then fetch the index on a first run that
    // has none; the automatic refresh path leaves the manual floor untouched.
    void openCatalog();
    void loadPacks();
    void soundPacks.load();
    if (!localScan.running()) void localScan.start();
    void listen<SoundfontProgress>(api.SOUNDFONT_PROGRESS_EVENT, (event) => {
      const progress = event.payload;
      setPackProgress((previous) => ({ ...previous, [progress.id]: progress }));
    }).then(keep).catch(() => {});
  });
  onCleanup(() => {
    disposed = true;
    for (const stop of stops.splice(0)) stop();
  });

  const clearProgress = (id: string) =>
    setPackProgress((previous) => {
      const next = { ...previous };
      delete next[id];
      return next;
    });

  const installPack = async (item: SoundfontItem) => {
    if (installLocked()) return;
    cancelling.delete(item.id);
    setBusy(item.id);
    clearProgress(item.id);
    setMessages((previous) => ({ ...previous, [item.id]: { text: "", error: false } }));
    try {
      await api.soundfontInstall(item.id);
      await loadPacks();
      // The installed font is searched from now on; a fresh count shows it.
      void localScan.start();
    } catch (cause) {
      const cancelled = cancelling.has(item.id);
      setMessages((previous) => ({
        ...previous,
        [item.id]: {
          text: cancelled
            ? i18n.t("onboarding.starters.cancelled")
            : i18n.t("soundfont.installFailed", { error: String(cause) }),
          error: !cancelled,
        },
      }));
    } finally {
      cancelling.delete(item.id);
      clearProgress(item.id);
      setBusy(null);
    }
  };

  const cancelPack = async (id: string) => {
    if (busy() !== id) return;
    cancelling.add(id);
    try {
      await api.cancelInstall(id);
    } catch (cause) {
      cancelling.delete(id);
      setMessages((previous) => ({
        ...previous,
        [id]: {
          text: i18n.t("onboarding.starters.cancelFailed", { error: String(cause) }),
          error: true,
        },
      }));
    }
  };

  const cancelPlugin = async (id: string) => {
    if (busy() !== id) return;
    cancelling.add(id);
    try {
      await api.cancelInstall(id);
    } catch (cause) {
      cancelling.delete(id);
      setMessages((previous) => ({
        ...previous,
        [id]: {
          text: i18n.t("onboarding.starters.cancelFailed", { error: String(cause) }),
          error: true,
        },
      }));
    }
  };

  const install = async (entry: PluginCatalogEntry) => {
    if (installLocked()) return;
    cancelling.delete(entry.id);
    setBusy(entry.id);
    setMessages((previous) => ({ ...previous, [entry.id]: { text: "", error: false } }));
    try {
      const outcome = await pluginStore.install(entry);
      if (outcome.status === "installed") {
        await pluginStore.load(true);
        // The installed bundle is searched from now on; a fresh count shows it.
        void localScan.start();
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
      const cancelled = cancelling.has(entry.id);
      setMessages((previous) => ({
        ...previous,
        [entry.id]: {
          text: cancelled
            ? i18n.t("onboarding.starters.cancelled")
            : i18n.t("onboarding.plugins.failed", { error: String(cause) }),
          error: !cancelled,
        },
      }));
    } finally {
      cancelling.delete(entry.id);
      setBusy(null);
    }
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

  /** Content labels for the kinds this build knows; anything else shows no label. */
  const contentLabel = (item: SoundPackItem): string =>
    item.content === "soundfont" || item.content === "sfz" || item.content === "samples"
      ? i18n.t(`onboarding.starters.kind.${item.content}`)
      : "";

  const packSize = (item: SoundPackItem): string => {
    const download = formatBytes(item.bytes);
    return item.unpacked_bytes > item.bytes && download
      ? i18n.t("onboarding.starters.size", { download, unpacked: formatBytes(item.unpacked_bytes) })
      : download;
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
          <button type="button" class="onboarding-skip" onClick={() => props.onDone()}>
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
          <Show when={showIndexLoading()}>
            <p class="onboarding-tools-loading" role="status">{i18n.t("onboarding.plugins.loading")}</p>
          </Show>
          <Show when={showOffline()}>
            <section class="onboarding-picks onboarding-picks-offline" aria-labelledby="onboarding-picks-offline">
              <p class="onboarding-tools-loading" id="onboarding-picks-offline">
                {i18n.t("onboarding.picks.offline")}
              </p>
              <Show when={messageFor("folder")?.error}>
                <p class="onboarding-tools-error" role="alert">{messageFor("folder")!.text}</p>
              </Show>
              <Show when={pluginStore.error()}>
                {(error) => (
                  <p class="onboarding-tools-error" role="alert">
                    {i18n.t("onboarding.plugins.error", { error: error() })}
                  </p>
                )}
              </Show>
              <Show when={pluginStore.refreshError()}>
                {(error) => (
                  <p class="onboarding-tools-error" role="alert">
                    {i18n.t("store.plugins.refreshFailed", { error: error() })}
                  </p>
                )}
              </Show>
              <Show when={pluginStore.refreshError() || pluginStore.error()}>
                <ConnectionHelp onRetry={retryIndex} />
              </Show>
              <div class="onboarding-scan-actions">
                <button
                  type="button"
                  class="onboarding-secondary"
                  disabled={indexRetrying() || pluginStore.refreshing() || folderBusy()}
                  onClick={() => void retryIndex()}
                >
                  {i18n.t("onboarding.picks.tryAgain")}
                </button>
                <button
                  type="button"
                  class="onboarding-secondary"
                  disabled={folderBusy()}
                  onClick={() => void addSoundFolder()}
                >
                  <FolderIcon />
                  {i18n.t("onboarding.local.add.sounds")}
                </button>
              </div>
            </section>
          </Show>
          <Show when={hasCatalogEntries()}>
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
                          <Show
                            when={running()}
                            fallback={
                              <button
                                type="button"
                                class="onboarding-secondary onboarding-install"
                                disabled={installLocked()}
                                onClick={() => void install(entry)}
                              >
                                {i18n.t("onboarding.plugins.install")}
                              </button>
                            }
                          >
                            <button
                              type="button"
                              class="onboarding-secondary"
                              onClick={() => void cancelPlugin(entry.id)}
                            >
                              {i18n.t("onboarding.starters.cancel")}
                            </button>
                          </Show>
                        </Show>
                      </div>
                    </article>
                  );
                }}
              </For>
            </div>
          </section>
          </Show>

          <Show when={showSoundPicks() || extraPacks().length > 0 || starters().length > 0}>
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
            <Show when={starters().length > 0 || extraPacks().length > 0}>
            <div class="onboarding-tools">
              <For each={starters()}>
                {(item) => {
                  const state = () => soundPacks.stateOf(item.id);
                  const installing = () => state()?.status === "installing";
                  const installed = () => item.installed || state()?.status === "installed";
                  const progress = () => {
                    const current = state();
                    return current?.status === "installing" ? current.progress : null;
                  };
                  const percent = () => {
                    const current = progress();
                    if (!current || current.total <= 0) return null;
                    return Math.min(100, Math.round((current.done / current.total) * 100));
                  };
                  const phase = () => {
                    const current = progress()?.phase;
                    return current && PHASES.includes(current) ? current : "download";
                  };
                  const phaseLabel = () => i18n.t(`onboarding.starters.phase.${phase()}`);
                  const linkMessage = () => messageFor(`link:${item.id}`);
                  return (
                    <article class="onboarding-tool onboarding-starter" classList={{ installed: installed() }} data-starter={item.id}>
                      <span class="onboarding-tool-icon" aria-hidden="true">
                        {item.content === "samples" ? <WaveIcon /> : <KeyboardIcon />}
                      </span>
                      <div class="onboarding-tool-copy">
                        <div class="onboarding-tool-head">
                          <Show when={contentLabel(item)}>
                            {(label) => <span class="onboarding-tool-role">{label()}</span>}
                          </Show>
                          <h2>{item.name}</h2>
                          <span class="onboarding-tool-size">{packSize(item)}</span>
                          <Show
                            when={item.license.url}
                            fallback={<span class="onboarding-tool-license">{item.license.name}</span>}
                          >
                            {(url) => (
                              <button
                                type="button"
                                class="onboarding-link onboarding-tool-license"
                                title={i18n.t("onboarding.starters.licenseLink", { name: item.license.name })}
                                aria-label={i18n.t("onboarding.starters.licenseLink", { name: item.license.name })}
                                onClick={() => void openLink(item.id, url())}
                              >
                                {item.license.name}
                              </button>
                            )}
                          </Show>
                        </div>
                        <p>
                          <Show when={item.description}>{item.description} · </Show>
                          <Show when={item.homepage} fallback={item.publisher}>
                            {(homepage) => (
                              <button
                                type="button"
                                class="onboarding-link"
                                title={i18n.t("onboarding.starters.publisherLink", { publisher: item.publisher })}
                                aria-label={i18n.t("onboarding.starters.publisherLink", { publisher: item.publisher })}
                                onClick={() => void openLink(item.id, homepage())}
                              >
                                {item.publisher}
                              </button>
                            )}
                          </Show>
                        </p>
                        <Show when={item.license.text}>
                          {(text) => <p class="onboarding-tool-attribution">{text()}</p>}
                        </Show>
                        <Show when={installing()}>
                          <div
                            class="onboarding-progress"
                            role="progressbar"
                            aria-label={i18n.t("onboarding.starters.progress", { name: item.name, phase: phaseLabel() })}
                            aria-valuemin={0}
                            aria-valuemax={100}
                            aria-valuenow={percent() ?? undefined}
                          >
                            <span
                              class="onboarding-progress-bar"
                              classList={{ indeterminate: percent() === null }}
                              style={{ width: percent() === null ? undefined : `${percent()}%` }}
                            />
                          </div>
                          <p class="onboarding-tool-message onboarding-starter-phase" role="status">
                            <span>{phaseLabel()}</span>
                            <Show when={percent() !== null}>
                              <span class="onboarding-starter-percent">{percent()}%</span>
                            </Show>
                          </p>
                        </Show>
                        <Show when={(() => { const current = state(); return current?.status === "installing" ? current.cancelError : null; })()}>
                          {(error) => (
                            <p class="onboarding-tool-message error" role="alert">
                              {i18n.t("onboarding.starters.cancelFailed", { error: error() })}
                            </p>
                          )}
                        </Show>
                        <Show when={state()?.status === "installed"}>
                          <p class="onboarding-tool-message" role="status">{i18n.t("onboarding.starters.done")}</p>
                        </Show>
                        <Show when={state()?.status === "cancelled"}>
                          <p class="onboarding-tool-message" role="status">{i18n.t("onboarding.starters.cancelled")}</p>
                        </Show>
                        <Show when={(() => { const current = state(); return current?.status === "failed" ? current.error : null; })()}>
                          {(error) => (
                            <p class="onboarding-tool-message error" role="alert">
                              {i18n.t("onboarding.starters.installFailed", { error: error() })}
                            </p>
                          )}
                        </Show>
                        <Show when={linkMessage()?.error}>
                          <p class="onboarding-tool-message error" role="alert">{linkMessage()!.text}</p>
                        </Show>
                      </div>
                      <div class="onboarding-tool-action">
                        <Show
                          when={!installed()}
                          fallback={<span class="onboarding-badge">{i18n.t("onboarding.plugins.installed")}</span>}
                        >
                          <Show
                            when={installing()}
                            fallback={
                              <button
                                type="button"
                                class="onboarding-secondary onboarding-install"
                                disabled={installLocked()}
                                onClick={() => void soundPacks.install(item.id)}
                              >
                                {i18n.t("onboarding.plugins.install")}
                              </button>
                            }
                          >
                            <button
                              type="button"
                              class="onboarding-secondary"
                              onClick={() => void soundPacks.cancel(item.id)}
                            >
                              {i18n.t("onboarding.starters.cancel")}
                            </button>
                          </Show>
                        </Show>
                      </div>
                    </article>
                  );
                }}
              </For>
              <For each={extraPacks()}>
                {(item) => {
                  const running = () => busy() === item.id;
                  const progress = () => packProgress()[item.id];
                  const phase = (): "download" | "verify" => {
                    const current = progress();
                    return current && current.total > 0 && current.received >= current.total ? "verify" : "download";
                  };
                  const percent = () => {
                    const current = progress();
                    if (!current || current.total <= 0) return null;
                    return Math.min(100, Math.round((current.received / current.total) * 100));
                  };
                  const phaseLabel = () => i18n.t(`onboarding.starters.phase.${phase()}`);
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
                          <div
                            class="onboarding-progress"
                            role="progressbar"
                            aria-label={i18n.t("onboarding.starters.progress", { name: item.name, phase: phaseLabel() })}
                            aria-valuemin={0}
                            aria-valuemax={100}
                            aria-valuenow={percent() ?? undefined}
                          >
                            <span
                              class="onboarding-progress-bar"
                              classList={{ indeterminate: percent() === null }}
                              style={{ width: percent() === null ? undefined : `${percent()}%` }}
                            />
                          </div>
                          <p class="onboarding-tool-message onboarding-starter-phase" role="status">
                            <span>{phaseLabel()}</span>
                            <Show when={percent() !== null}>
                              <span class="onboarding-starter-percent">{percent()}%</span>
                            </Show>
                          </p>
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
                        <Show
                          when={running()}
                          fallback={
                            <button
                              type="button"
                              class="onboarding-secondary onboarding-install"
                              disabled={installLocked()}
                              onClick={() => void installPack(item)}
                            >
                              {i18n.t("onboarding.plugins.install")}
                            </button>
                          }
                        >
                          <button
                            type="button"
                            class="onboarding-secondary"
                            onClick={() => void cancelPack(item.id)}
                          >
                            {i18n.t("onboarding.starters.cancel")}
                          </button>
                        </Show>
                      </div>
                    </article>
                  );
                }}
              </For>
            </div>
            </Show>
          </section>
          </Show>
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
