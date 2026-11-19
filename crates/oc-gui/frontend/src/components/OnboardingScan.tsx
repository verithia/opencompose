/**
 * The guide's scan page: progress while the plugin inventory and the sound
 * folders are read, then what was found, in folded scrollable lists, with the
 * folders that were searched and a way to add more.
 */
import { For, Show, createMemo, createSignal } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { i18n } from "../stores/i18n";
import * as api from "../stores/api";
import type { FoundItem, LocalContentKind, LocalScanProgress, SearchedFolder } from "../stores/api";
import { localScan } from "../stores/localScan";
import { FolderIcon, KeyboardIcon, PlugIcon, WaveIcon } from "./icons";

type Kind = "plugins" | "instruments" | "samples";
const KINDS: readonly Kind[] = ["plugins", "instruments", "samples"];

/** Format names are product names, so only the sampler asset is translated. */
export function formatLabel(format: string): string {
  return format === "sampler_asset" ? i18n.t("onboarding.local.samplerAsset") : format.toUpperCase();
}

function formatLine(kind: LocalContentKind): string {
  return kind.formats.map((row) => `${formatLabel(row.format)} ${row.count}`).join(" · ");
}

const count = (value: number) => value.toLocaleString(i18n.locale());

function percent(progress: LocalScanProgress | null): number | null {
  if (!progress || progress.total <= 0) return null;
  return Math.min(100, Math.round((progress.done / progress.total) * 100));
}

/** Ask the user for a plugin or sound folder; `null` when they cancel. */
export async function chooseFolder(kind: "plugins" | "sounds"): Promise<string | null> {
  const chosen = await open({
    directory: true,
    multiple: false,
    title: i18n.t(kind === "plugins" ? "onboarding.local.pickPlugins" : "onboarding.local.pickSounds"),
  });
  return typeof chosen === "string" ? chosen : null;
}

/** One searched folder with what it holds across instruments and samples. */
interface FolderRow {
  kind: "plugins" | "sounds";
  folder: SearchedFolder;
  counts: string;
}

export function OnboardingScan(props: { busy: boolean; onBusy: (busy: boolean) => void }) {
  const summary = localScan.summary;
  const progress = localScan.progress;

  /** The plugin step is over once the sound step reports or the scan returns. */
  const phaseDone = (phase: "plugins" | "sounds") =>
    !localScan.running() || (phase === "plugins" && progress().sounds !== null);

  const phaseText = (phase: "plugins" | "sounds"): string => {
    const state = progress()[phase];
    if (phaseDone(phase)) return i18n.t("onboarding.scan.phaseDone");
    if (phase === "sounds" && !phaseDone("plugins")) return i18n.t("onboarding.scan.waiting");
    if (!state || state.total === 0) {
      return i18n.t(phase === "plugins" ? "onboarding.scan.findingPlugins" : "onboarding.scan.findingSounds");
    }
    return i18n.t(phase === "plugins" ? "onboarding.scan.readingPlugin" : "onboarding.scan.searchingFolder", {
      name: state.current ?? "",
      done: count(Math.min(state.done + (phase === "sounds" ? 1 : 0), state.total)),
      total: count(state.total),
    });
  };

  const folderRows = createMemo<FolderRow[]>(() => {
    const current = summary();
    if (!current) return [];
    const rows: FolderRow[] = current.plugins.folders.map((folder) => ({
      kind: "plugins",
      folder,
      counts: i18n.t("onboarding.scan.folderPlugins", { count: count(folder.count) }),
    }));
    current.instruments.folders.forEach((folder, index) => {
      const samples = current.samples.folders[index]?.count ?? 0;
      rows.push({
        kind: "sounds",
        folder,
        counts: i18n.t("onboarding.scan.folderSounds", {
          instruments: count(folder.count),
          samples: count(samples),
        }),
      });
    });
    return rows;
  });

  /** Folders worth listing: the ones that exist plus every user folder. */
  const listed = () => folderRows().filter((row) => row.folder.exists || row.folder.origin === "user");
  const absent = () => folderRows().length - listed().length;

  const [folderMessage, setFolderMessage] = createSignal<string | null>(null);
  const folderError = (text: string) => setFolderMessage(text);

  const addFolder = async (kind: "plugins" | "sounds") => {
    if (props.busy) return;
    const chosen = await chooseFolder(kind);
    if (!chosen) return;
    props.onBusy(true);
    setFolderMessage(null);
    try {
      await localScan.addFolder(kind, chosen);
    } catch (cause) {
      folderError(i18n.t("onboarding.local.failed", { error: String(cause) }));
    } finally {
      props.onBusy(false);
    }
  };

  const removeFolder = async (row: FolderRow) => {
    if (props.busy) return;
    props.onBusy(true);
    setFolderMessage(null);
    try {
      localScan.update(
        row.kind === "plugins"
          ? await api.pluginFolderRemove(row.folder.path)
          : await api.soundFolderRemove(row.folder.library_id ?? ""),
      );
    } catch (cause) {
      folderError(i18n.t("onboarding.local.error", { error: String(cause) }));
    } finally {
      props.onBusy(false);
    }
  };


  /** Origins this build does not know get no badge rather than failing the list. */
  const originBadge = (origin: SearchedFolder["origin"]): string | null => {
    switch (origin) {
      case "user": return i18n.t("onboarding.local.added");
      case "host_settings": return i18n.t("onboarding.scan.fromHost");
      case "environment": return i18n.t("onboarding.scan.fromEnvironment");
      case "managed": return i18n.t("onboarding.scan.managed");
      default: return null;
    }
  };

  const icon = (kind: Kind) =>
    kind === "plugins" ? <PlugIcon /> : kind === "instruments" ? <KeyboardIcon /> : <WaveIcon />;

  return (
    <section class="onboarding-scan" aria-labelledby="onboarding-title">
      <Show when={localScan.running() || !summary()}>
      <ul class="onboarding-scan-phases" aria-live="polite">
        <For each={["plugins", "sounds"] as const}>
          {(phase) => {
            const value = () => (phaseDone(phase) ? 100 : percent(progress()[phase]));
            return (
              <li class="onboarding-scan-phase" classList={{ done: phaseDone(phase) }} data-phase={phase}>
                <span class="onboarding-tool-icon" aria-hidden="true">
                  {phase === "plugins" ? <PlugIcon /> : <WaveIcon />}
                </span>
                <div class="onboarding-scan-phase-copy">
                  <div class="onboarding-scan-phase-head">
                    <h2>{i18n.t(phase === "plugins" ? "onboarding.scan.plugins" : "onboarding.scan.sounds")}</h2>
                    <Show when={value() !== null && !phaseDone(phase)}>
                      <span class="onboarding-scan-percent">{value()}%</span>
                    </Show>
                  </div>
                  <div
                    class="onboarding-progress"
                    role="progressbar"
                    aria-label={i18n.t(phase === "plugins" ? "onboarding.scan.plugins" : "onboarding.scan.sounds")}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={value() ?? undefined}
                  >
                    <span
                      class="onboarding-progress-bar"
                      classList={{ indeterminate: value() === null && localScan.running() }}
                      style={{ width: value() === null ? undefined : `${value()}%` }}
                    />
                  </div>
                  <p class="onboarding-scan-status" title={progress()[phase]?.current ?? undefined}>
                    {phaseText(phase)}
                  </p>
                </div>
              </li>
            );
          }}
        </For>
      </ul>
      </Show>

      <Show when={localScan.error()}>
        {(error) => <p class="onboarding-tools-error" role="alert">{i18n.t("onboarding.local.error", { error: error() })}</p>}
      </Show>
      <Show when={folderMessage()}>
        {(message) => <p class="onboarding-tools-error" role="alert">{message()}</p>}
      </Show>

      <Show when={summary()}>
        {(current) => (
          <>
            <div class="onboarding-local-grid">
              <For each={KINDS}>
                {(kind) => (
                  <div class="onboarding-local-card" data-kind={kind}>
                    <span class="onboarding-tool-icon" aria-hidden="true">{icon(kind)}</span>
                    <div class="onboarding-local-copy">
                      <div class="onboarding-local-head">
                        <strong class="onboarding-local-count">{count(current()[kind].total)}</strong>
                        <h3>{i18n.t(`onboarding.scan.${kind}`)}</h3>
                      </div>
                      <p class="onboarding-local-formats">
                        {current()[kind].total > 0
                          ? kind === "samples"
                            ? i18n.t("onboarding.scan.samplesFormats")
                            : formatLine(current()[kind])
                          : i18n.t(`onboarding.scan.${kind}None`)}
                      </p>
                    </div>
                  </div>
                )}
              </For>
            </div>

            <div class="onboarding-found">
              <For each={["plugins", "instruments"] as const}>
                {(kind) => (
                  <Show when={current()[kind].items.length > 0}>
                    <details class="onboarding-fold" data-kind={kind}>
                      <summary>{i18n.t(`onboarding.scan.list.${kind}`, { count: count(current()[kind].total) })}</summary>
                      <ul class="onboarding-found-list" aria-label={i18n.t(`onboarding.scan.${kind}`)}>
                        <For each={current()[kind].items}>
                          {(item: FoundItem) => (
                            <li classList={{ unreadable: !item.readable }} title={item.path}>
                              <span class="onboarding-found-name">{item.name}</span>
                              <span class="onboarding-found-detail">
                                {item.readable ? item.detail : i18n.t("onboarding.scan.unreadable")}
                              </span>
                              <span class="onboarding-found-format">{formatLabel(item.format)}</span>
                            </li>
                          )}
                        </For>
                      </ul>
                      <Show when={current()[kind].truncated}>
                        <p class="onboarding-local-note">
                          {i18n.t("onboarding.scan.truncated", { shown: count(current()[kind].items.length) })}
                        </p>
                      </Show>
                    </details>
                  </Show>
                )}
              </For>
              <details class="onboarding-fold" data-kind="folders">
                <summary>{i18n.t("onboarding.local.folders", { count: count(listed().length) })}</summary>
                <ul class="onboarding-folder-list">
                  <For each={listed()}>
                    {(row) => (
                      <li classList={{ missing: !row.folder.exists }}>
                        <span class="onboarding-folder-kind">
                          {i18n.t(row.kind === "plugins" ? "onboarding.scan.plugins" : "onboarding.scan.sounds")}
                        </span>
                        <code class="onboarding-folder-path" title={row.folder.path}>{row.folder.path}</code>
                        <span class="onboarding-folder-count">{row.counts}</span>
                        <Show when={originBadge(row.folder.origin)}>
                          {(badge) => <span class="onboarding-badge">{badge()}</span>}
                        </Show>
                        <Show when={row.folder.origin === "user"}>
                          <button
                            type="button"
                            class="onboarding-ghost onboarding-folder-remove"
                            disabled={props.busy}
                            aria-label={i18n.t("onboarding.local.removeFolder", { path: row.folder.path })}
                            onClick={() => void removeFolder(row)}
                          >
                            {i18n.t("onboarding.local.remove")}
                          </button>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
                <Show when={absent() > 0}>
                  <p class="onboarding-local-note">{i18n.t("onboarding.scan.absent", { count: count(absent()) })}</p>
                </Show>
              </details>
            </div>
          </>
        )}
      </Show>

      <div class="onboarding-scan-add">
        <p>{i18n.t("onboarding.scan.addHint")}</p>
        <div class="onboarding-scan-actions">
          <button type="button" class="onboarding-secondary" disabled={props.busy} onClick={() => void addFolder("plugins")}>
            <FolderIcon />
            {i18n.t("onboarding.local.add.plugins")}
          </button>
          <button type="button" class="onboarding-secondary" disabled={props.busy} onClick={() => void addFolder("sounds")}>
            <FolderIcon />
            {i18n.t("onboarding.local.add.sounds")}
          </button>
          <button
            type="button"
            class="onboarding-ghost"
            disabled={props.busy || localScan.running()}
            onClick={() => void localScan.start(true)}
          >
            {i18n.t("onboarding.scan.again")}
          </button>
        </div>
      </div>
    </section>
  );
}
