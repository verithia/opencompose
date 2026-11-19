/**
 * The searched plugin and sound folders, shared by the first-run scan page and
 * the Folders page in Preferences: the folder picker, one row per folder with
 * its counts and origin, adding and removing the user's own folders, and the
 * scan's progress.
 */
import { For, Show, createSignal } from "solid-js";
import { open } from "@tauri-apps/plugin-dialog";
import { i18n, type I18nKey } from "../stores/i18n";
import * as api from "../stores/api";
import type { FolderErrorCode, LocalContentSummary, LocalScanProgress, SearchedFolder } from "../stores/api";
import { localScan } from "../stores/localScan";
import { PlugIcon, WaveIcon } from "./icons";

export type FolderKind = "plugins" | "sounds";

export const count = (value: number) => value.toLocaleString(i18n.locale());

/** Ask the user for a plugin or sound folder; `null` when they cancel. */
export async function chooseFolder(kind: FolderKind): Promise<string | null> {
  const chosen = await open({
    directory: true,
    multiple: false,
    title: i18n.t(kind === "plugins" ? "onboarding.local.pickPlugins" : "onboarding.local.pickSounds"),
  });
  return typeof chosen === "string" ? chosen : null;
}

/** One searched folder with what it holds across instruments and samples. */
export interface FolderRow {
  kind: FolderKind;
  folder: SearchedFolder;
  counts: string;
}

/** Plugin folders, then sound folders, each with its localized counts. */
export function folderRows(summary: LocalContentSummary | null): FolderRow[] {
  if (!summary) return [];
  const rows: FolderRow[] = summary.plugins.folders.map((folder) => ({
    kind: "plugins",
    folder,
    counts: i18n.t("onboarding.scan.folderPlugins", { count: count(folder.count) }),
  }));
  summary.instruments.folders.forEach((folder, index) => {
    const samples = summary.samples.folders[index]?.count ?? 0;
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
}

/** Folders worth listing: the ones that exist plus every user folder. */
export const isListed = (row: FolderRow) => row.folder.exists || row.folder.origin === "user";

/** Origins this build does not know get no badge rather than failing the list. */
export function originBadge(origin: SearchedFolder["origin"]): string | null {
  switch (origin) {
    case "user": return i18n.t("onboarding.local.added");
    case "host_settings": return i18n.t("onboarding.scan.fromHost");
    case "environment": return i18n.t("onboarding.scan.fromEnvironment");
    case "managed": return i18n.t("onboarding.scan.managed");
    default: return null;
  }
}

const ERROR_TEXT: Record<Exclude<FolderErrorCode, "failed">, I18nKey> = {
  not_a_directory: "folders.error.notADirectory",
  already_searched: "folders.error.alreadySearched",
  not_listed: "folders.error.notListed",
  too_many: "folders.error.tooMany",
  invalid: "folders.error.invalid",
  no_instruments: "folders.error.noInstruments",
};

/** The reason a folder change was refused, worded from the backend's code. */
export function folderErrorText(cause: unknown): string {
  const error = api.toFolderChangeError(cause);
  if (error.code === null) return error.message;
  if (error.code === "failed") return i18n.t("folders.error.failed", { error: error.message });
  return i18n.t(ERROR_TEXT[error.code]);
}

/**
 * Adding and removing user folders. `busy` is shared with the caller so its
 * other actions wait too; `message` holds the last refusal.
 */
export function useFolderEditing(options: { busy: () => boolean; onBusy: (busy: boolean) => void }) {
  const [message, setMessage] = createSignal<string | null>(null);

  const add = async (kind: FolderKind) => {
    if (options.busy()) return;
    const chosen = await chooseFolder(kind);
    if (!chosen) return;
    options.onBusy(true);
    setMessage(null);
    try {
      await localScan.addFolder(kind, chosen);
    } catch (cause) {
      setMessage(i18n.t("onboarding.local.failed", { error: folderErrorText(cause) }));
    } finally {
      options.onBusy(false);
    }
  };

  const remove = async (row: FolderRow) => {
    if (options.busy()) return;
    options.onBusy(true);
    setMessage(null);
    try {
      localScan.update(
        row.kind === "plugins"
          ? await api.pluginFolderRemove(row.folder.path)
          : await api.soundFolderRemove(row.folder.library_id ?? ""),
      );
    } catch (cause) {
      setMessage(i18n.t("folders.removeFailed", { error: folderErrorText(cause) }));
    } finally {
      options.onBusy(false);
    }
  };

  return { message, add, remove };
}

function percent(progress: LocalScanProgress | null): number | null {
  if (!progress || progress.total <= 0) return null;
  return Math.min(100, Math.round((progress.done / progress.total) * 100));
}

/** The plugin step, then the sound step, with a bar and a status line each. */
export function ScanPhases() {
  const progress = localScan.progress;

  /** The plugin step is over once the sound step reports or the scan returns. */
  const phaseDone = (phase: FolderKind) =>
    !localScan.running() || (phase === "plugins" && progress().sounds !== null);

  const phaseText = (phase: FolderKind): string => {
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

  return (
    <ul class="onboarding-scan-phases" aria-live="polite">
      <For each={["plugins", "sounds"] as const}>
        {(phase) => {
          const value = () => (phaseDone(phase) ? 100 : percent(progress()[phase]));
          const label = () => i18n.t(phase === "plugins" ? "onboarding.scan.plugins" : "onboarding.scan.sounds");
          return (
            <li class="onboarding-scan-phase" classList={{ done: phaseDone(phase) }} data-phase={phase}>
              <span class="onboarding-tool-icon" aria-hidden="true">
                {phase === "plugins" ? <PlugIcon /> : <WaveIcon />}
              </span>
              <div class="onboarding-scan-phase-copy">
                <div class="onboarding-scan-phase-head">
                  <h2>{label()}</h2>
                  <Show when={value() !== null && !phaseDone(phase)}>
                    <span class="onboarding-scan-percent">{value()}%</span>
                  </Show>
                </div>
                <div
                  class="onboarding-progress"
                  role="progressbar"
                  aria-label={label()}
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
  );
}
