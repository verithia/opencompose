/**
 * First-run splash and guide. The splash is painted before the workspace so a
 * new install never flashes the empty DAW; the guide then explains the three
 * bottom panels, offers the AI settings page, then shows what this computer
 * already has and recommends one free plugin per role from the signed catalog.
 */
import { aiConfig } from "../ai/config";
import { preferencesUi } from "../stores/preferencesUi";
import { useDialogFocus } from "./dialogFocus";
import { useTopmostEscape } from "./modalEscape";
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
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
import type { LocalContentKind, LocalContentSummary, PluginCatalogEntry, SoundfontItem } from "../stores/api";
import { pickRecommendations } from "../stores/onboardingPicks";
import { FolderIcon, PlugIcon, WaveIcon } from "./icons";

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


/** Packs the guide offers at most; the Store lists the rest. */
const MAX_PACK_PICKS = 2;

/** Format names are product names, so only the sampler asset is translated. */
function formatLabel(format: string): string {
  return format === "sampler_asset" ? i18n.t("onboarding.local.samplerAsset") : format.toUpperCase();
}

function formatLine(kind: LocalContentKind): string {
  return kind.formats.map((row) => `${formatLabel(row.format)} ${row.count}`).join(" · ");
}

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

  const recommended = createMemo(() =>
    pickRecommendations(
      pluginStore.catalog()?.entries ?? [],
      isInstalled,
      (entry) => entryInstallable(entry, platform()),
    ),
  );

  const [local, setLocal] = createSignal<LocalContentSummary | null>(null);
  const [localError, setLocalError] = createSignal<string | null>(null);
  const [folderBusy, setFolderBusy] = createSignal(false);
  const [packs, setPacks] = createSignal<SoundfontItem[]>([]);

  const loadLocal = async () => {
    try {
      setLocal((await api.localContentSummary()) ?? null);
      setLocalError(null);
    } catch (cause) {
      setLocalError(i18n.t("onboarding.local.error", { error: String(cause) }));
    }
  };

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

  /** Ask for a folder, then let the backend own the list and the recount. */
  const addFolder = async (kind: "plugins" | "sounds") => {
    if (folderBusy()) return;
    const chosen = await open({
      directory: true,
      multiple: false,
      title: i18n.t(kind === "plugins" ? "onboarding.local.pickPlugins" : "onboarding.local.pickSounds"),
    });
    if (typeof chosen !== "string") return;
    setFolderBusy(true);
    try {
      setLocal(kind === "plugins" ? await api.pluginFolderAdd(chosen) : await api.soundFolderAdd(chosen));
      setLocalError(null);
      if (kind === "plugins") void pluginStore.load(true);
    } catch (cause) {
      setLocalError(i18n.t("onboarding.local.failed", { error: String(cause) }));
    } finally {
      setFolderBusy(false);
    }
  };

  const removeFolder = async (kind: "plugins" | "sounds", folder: api.SearchedFolder) => {
    if (folderBusy()) return;
    setFolderBusy(true);
    try {
      setLocal(
        kind === "plugins"
          ? await api.pluginFolderRemove(folder.path)
          : await api.soundFolderRemove(folder.library_id ?? ""),
      );
      setLocalError(null);
    } catch (cause) {
      setLocalError(i18n.t("onboarding.local.error", { error: String(cause) }));
    } finally {
      setFolderBusy(false);
    }
  };

  /** Folders worth listing: the ones that exist plus every user folder. */
  const listedFolders = (kind: "plugins" | "sounds") =>
    (local()?.[kind].folders ?? []).filter((folder) => folder.exists || folder.origin === "user");

  const bytesOf = (entry: PluginCatalogEntry): number | undefined =>
    hostInstall(entry, platform())?.bytes;

  const messageFor = (id: string) => messages()[id];

  onMount(() => {
    // Use the bundled or cached catalog: the guide must paint on a first run
    // with no network. The Store's explicit refresh remains the network path.
    void pluginStore.load();
    // Counting reads directory entries only, so it is ready long before the
    // background scan that reads each plugin's details.
    void loadLocal();
    void loadPacks();
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<number>("plugin_inventory_updated", () => {
      void loadLocal();
      void pluginStore.load(true);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else stop = unlisten;
    });
    onCleanup(() => {
      disposed = true;
      stop?.();
    });
  });

  const installPack = async (item: SoundfontItem) => {
    if (busy()) return;
    setBusy(item.id);
    setMessages((previous) => ({ ...previous, [item.id]: { text: "", error: false } }));
    try {
      await api.soundfontInstall(item.id);
      await Promise.all([loadLocal(), loadPacks()]);
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
          <section class="onboarding-local" aria-labelledby="onboarding-local-title">
            <h2 class="onboarding-section-title" id="onboarding-local-title">{i18n.t("onboarding.local.title")}</h2>
            <Show when={localError()}>
              {(error) => <p class="onboarding-tools-error" role="alert">{error()}</p>}
            </Show>
            <div class="onboarding-local-grid">
              <For each={["plugins", "sounds"] as const}>
                {(kind) => {
                  const summary = () => local()?.[kind];
                  return (
                    <div class="onboarding-local-card" data-kind={kind}>
                      <span class="onboarding-tool-icon" aria-hidden="true">
                        {kind === "plugins" ? <PlugIcon /> : <WaveIcon />}
                      </span>
                      <div class="onboarding-local-copy">
                        <div class="onboarding-local-head">
                          <strong class="onboarding-local-count" aria-label={i18n.t(`onboarding.local.${kind}Count`, { count: summary()?.total ?? 0 })}>
                            {summary() ? summary()!.total : "…"}
                          </strong>
                          <h3>{i18n.t(`onboarding.local.${kind}`)}</h3>
                        </div>
                        <p class="onboarding-local-formats">
                          {summary()
                            ? summary()!.total > 0
                              ? formatLine(summary()!)
                              : i18n.t(`onboarding.local.${kind}None`)
                            : i18n.t("onboarding.local.loading")}
                        </p>
                      </div>
                      <button
                        type="button"
                        class="onboarding-secondary onboarding-local-add"
                        disabled={folderBusy()}
                        onClick={() => void addFolder(kind)}
                      >
                        <FolderIcon />
                        {i18n.t(`onboarding.local.add.${kind}`)}
                      </button>
                    </div>
                  );
                }}
              </For>
            </div>
            <Show when={local()?.scanning}>
              <p class="onboarding-local-note" role="status">{i18n.t("onboarding.local.scanning")}</p>
            </Show>
            <Show when={local()}>
              <details class="onboarding-folders">
                <summary>
                  {i18n.t("onboarding.local.folders", {
                    count: listedFolders("plugins").length + listedFolders("sounds").length,
                  })}
                </summary>
                <For each={["plugins", "sounds"] as const}>
                  {(kind) => (
                    <ul class="onboarding-folder-list" aria-label={i18n.t(`onboarding.local.${kind}`)}>
                      <For each={listedFolders(kind)}>
                        {(folder) => (
                          <li classList={{ missing: !folder.exists }}>
                            <span class="onboarding-folder-kind">{i18n.t(`onboarding.local.${kind}`)}</span>
                            <code class="onboarding-folder-path" title={folder.path}>{folder.path}</code>
                            <span class="onboarding-folder-count">{folder.count}</span>
                            <Show when={folder.origin === "user"}>
                              <span class="onboarding-badge">{i18n.t("onboarding.local.added")}</span>
                              <button
                                type="button"
                                class="onboarding-ghost onboarding-folder-remove"
                                disabled={folderBusy()}
                                aria-label={i18n.t("onboarding.local.removeFolder", { path: folder.path })}
                                onClick={() => void removeFolder(kind, folder)}
                              >
                                {i18n.t("onboarding.local.remove")}
                              </button>
                            </Show>
                          </li>
                        )}
                      </For>
                    </ul>
                  )}
                </For>
              </details>
            </Show>
          </section>

          <section class="onboarding-picks" aria-labelledby="onboarding-picks-title">
            <h2 class="onboarding-section-title" id="onboarding-picks-title">{i18n.t("onboarding.picks.title")}</h2>
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
            </div>
          </section>
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
