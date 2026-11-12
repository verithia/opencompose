/**
 * The Storage page: what this build keeps on disk. Every block reports its own
 * size and can be rescanned on its own, because scanning plugins or a large
 * conversation store takes long enough to be worth isolating.
 */
import { For, Show, createSignal, onMount } from "solid-js";
import { ask } from "@tauri-apps/plugin-dialog";
import { deleteSession } from "../../ai/api";
import { i18n } from "../../stores/i18n";
import type { ClipCacheUsage } from "../../stores/api";
import { storageStore, type StorageSection } from "../../stores/storage";
import {
  openPreferencesExportDialog,
  openPreferencesImportDialog,
  preferencesStore,
} from "../../stores/preferencesBundle";
import { useTooltip } from "../Tooltip";

/** The directory part of a path, for the "open folder" action. */
function parentOf(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut > 0 ? path.slice(0, cut) : path;
}

function megabytes(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** One archive directory's files and size against the shared budget. */
function auditionUsage(files: number, bytes: number, budget: number): string {
  return i18n.t("preferences.storage.auditionUsage", {
    files,
    size: megabytes(bytes),
    budget: megabytes(budget),
  });
}

/** A block header: its name on the left, size and rescan on the right. */
function BlockHeader(props: { label: string; size: string; section: StorageSection }) {
  const busy = () => storageStore.scanning() === props.section;
  return (
    <div class="preferences-block-head">
      <span class="preferences-fact-label">{props.label}</span>
      <span class="preferences-block-meta">
        <span class="preferences-size">{props.size}</span>
        <button
          type="button"
          class="toolbar-btn text-btn preferences-scan"
          classList={{ scanning: busy() }}
          aria-label={i18n.t("preferences.storage.scan")}
          disabled={busy()}
          onClick={() => void storageStore.scan(props.section)}
        >{busy() ? i18n.t("preferences.storage.scanning") : i18n.t("preferences.storage.scan")}</button>
      </span>
    </div>
  );
}

function clipTemporaryBytes(usage: ClipCacheUsage): number {
  return usage.temporary_bytes ?? usage.live_temporary_bytes;
}

function ClipCacheRow(props: { label: string; usage: ClipCacheUsage }) {
  return (
    <div class="preferences-field">
      <span class="preferences-fact-label">{props.label}</span>
      <p class="preferences-hint">
        {i18n.t("preferences.storage.clipCacheUsage", {
          cached: megabytes(props.usage.cached_bytes),
          open: megabytes(props.usage.open_cached_bytes),
          temporary: megabytes(clipTemporaryBytes(props.usage)),
          playing: megabytes(props.usage.live_temporary_bytes),
          budget: megabytes(props.usage.budget_bytes),
        })}
      </p>
      <Show when={props.usage.dir}>
        {(dir) => (
          <div class="preferences-path-row">
            <span class="preferences-path selectable">{dir()}</span>
            <button
              type="button"
              class="toolbar-btn text-btn preferences-open-folder"
              disabled={props.usage.cached_bytes === 0}
              onClick={() => void storageStore.openFolder(dir())}
            >{i18n.t("preferences.storage.openFolder")}</button>
          </div>
        )}
      </Show>
    </div>
  );
}

export function StoragePage(props: { onClose: () => void }) {
  void props;
  onMount(() => {
    void storageStore.load();
    void storageStore.loadSessionAudio();
  });

  const report = () => storageStore.report();
  /** Workspaces start folded: a store can hold hundreds of conversations. */
  const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(new Set());
  const toggleWorkspace = (key: string) =>
    setExpanded((open) => {
      const next = new Set(open);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const removeSession = async (id: string, title: string) => {
    const confirmed = await ask(
      i18n.t("preferences.storage.deleteSessionConfirm", { title }),
      { title: i18n.t("preferences.storage.deleteSession", { title }), kind: "warning" },
    );
    if (!confirmed) return;
    try {
      await deleteSession(id);
      await storageStore.scan("sessions");
    } catch {
      // The next scan shows whatever is actually left on disk.
    }
  };

  /** Clears both audition directories, after the operator confirms. */
  const clearAuditions = async () => {
    const confirmed = await ask(
      i18n.t("preferences.storage.clearAuditionsConfirm"),
      { title: i18n.t("preferences.storage.clearAuditions"), kind: "warning" },
    );
    if (confirmed) await storageStore.clearAuditions();
  };

  const clearClipCache = async () => {
    const confirmed = await ask(
      i18n.t("preferences.storage.clipCacheClearConfirm"),
      { title: i18n.t("preferences.storage.clipCacheClear"), kind: "warning" },
    );
    if (confirmed) await storageStore.clearClipPcmCache();
  };

  /** Clears the sample cache only after the operator accepts the loss. */
  const clearSamples = async () => {
    const confirmed = await ask(
      i18n.t("preferences.storage.clearSamplesConfirm"),
      { title: i18n.t("preferences.storage.clear"), kind: "warning" },
    );
    if (confirmed) await storageStore.clearSamples();
  };

  return (
    <>
      <div class="preferences-divider" />

      <section class="preferences-bundle-section" aria-label={i18n.t("preferences.bundle")}>
        <div class="keymap-title">{i18n.t("preferences.bundle")}</div>
        <div class="preferences-bundle-actions-row">
          <button
            type="button"
            class="toolbar-btn text-btn preferences-export-btn"
            onClick={() => {
              void openPreferencesExportDialog().then((chosen) => {
                if (chosen) props.onClose();
              });
            }}
          >{i18n.t("preferences.export")}</button>
          <button
            type="button"
            class="toolbar-btn text-btn preferences-import-btn"
            onClick={() => {
              void openPreferencesImportDialog().then((chosen) => {
                if (chosen) props.onClose();
              });
            }}
          >{i18n.t("preferences.import")}</button>
        </div>
        <Show when={preferencesStore.pickError()}>
          {(message) => <p class="preferences-bundle-error-hint selectable">{message()}</p>}
        </Show>
      </section>


      <Show
        when={report()}
        fallback={
          <p class="preferences-hint">
            {storageStore.error() ?? i18n.t("preferences.storage.loading")}
          </p>
        }
      >
        {(loaded) => (
          <>
            <section class="preferences-facts" aria-label={i18n.t("preferences.page.storage")}>
              {/* Audition audio container. */}
              <div class="preferences-block">
                <div class="preferences-field">
                  <label class="preferences-field-label" for="preferences-session-audio-format">
                    {i18n.t("preferences.storage.sessionAudio")}
                  </label>
                  <div class="preferences-inline-row">
                    <select
                      id="preferences-session-audio-format"
                      class="theme-select preferences-session-audio-format"
                      aria-label={i18n.t("preferences.storage.sessionAudio")}
                      value={storageStore.sessionAudio().format}
                      onChange={(event) =>
                        void storageStore.saveSessionAudio({
                          format: event.currentTarget.value as "wav" | "mp3",
                          mp3Kbps: storageStore.sessionAudio().mp3Kbps,
                        })
                      }
                    >
                      <option value="wav">{i18n.t("preferences.storage.audioWav")}</option>
                      <option value="mp3">{i18n.t("preferences.storage.audioMp3")}</option>
                    </select>
                    <Show when={storageStore.sessionAudio().format === "mp3"}>
                    <select
                      class="theme-select preferences-session-audio-kbps"
                      aria-label={i18n.t("preferences.storage.audioBitrate")}
                      value={String(storageStore.sessionAudio().mp3Kbps)}
                      onChange={(event) =>
                        void storageStore.saveSessionAudio({
                          format: storageStore.sessionAudio().format,
                          mp3Kbps: Number(event.currentTarget.value),
                        })
                      }
                    >
                      <option value="128">128 kbps</option>
                      <option value="192">192 kbps</option>
                      <option value="256">256 kbps</option>
                      <option value="320">320 kbps</option>
                    </select>
                    </Show>
                  </div>
                  <p class="preferences-hint">{i18n.t("preferences.storage.sessionAudioHint")}</p>
                </div>
              </div>

              {/* Conversations: one group per workspace, one row per session. */}
              <div class="preferences-block">
                <BlockHeader
                  label={i18n.t("preferences.storage.sessions")}
                  size={megabytes(loaded().sessions.total_bytes)}
                  section="sessions"
                />
                <p class="preferences-hint">
                  {i18n.t("preferences.storage.sessionUsage", {
                    size: megabytes(loaded().sessions.total_bytes),
                    audio: megabytes(loaded().sessions.audio_bytes),
                  })}
                </p>
                <For each={loaded().sessions.workspaces}>
                  {(workspace) => (
                    <>
                      <button
                        type="button"
                        class="preferences-workspace-head"
                        aria-expanded={expanded().has(workspace.key)}
                        onClick={() => toggleWorkspace(workspace.key)}
                      >
                        <span class="preferences-workspace-toggle">
                          {expanded().has(workspace.key) ? "▾" : "▸"}
                        </span>
                        <span class="preferences-path selectable">{workspace.path}</span>
                        <span class="preferences-size">
                          {i18n.t("preferences.storage.workspaceSummary", {
                            count: workspace.sessions.length,
                            size: megabytes(workspace.bytes),
                          })}
                        </span>
                      </button>
                      <Show when={expanded().has(workspace.key)}>
                      <For each={workspace.sessions}>
                        {(session) => (
                          <div class="preferences-session-row">
                            <span class="preferences-session-title" title={session.id}>
                              {session.title}
                            </span>
                            <span class="preferences-size">{megabytes(session.bytes)}</span>
                            <button
                              type="button"
                              class="toolbar-btn text-btn preferences-delete-session"
                              aria-label={i18n.t("preferences.storage.deleteSession", {
                                title: session.title,
                              })}
                              onClick={() => void removeSession(session.id, session.title)}
                            >{i18n.t("preferences.storage.delete")}</button>
                          </div>
                        )}
                      </For>
                      </Show>
                    </>
                  )}
                </For>
                <div class="preferences-path-row">
                  <span class="preferences-path selectable">{loaded().sessions.root}</span>
                  <button
                    type="button"
                    class="toolbar-btn text-btn preferences-open-folder"
                    onClick={() => void storageStore.openFolder(loaded().sessions.root)}
                  >{i18n.t("preferences.storage.openFolder")}</button>
                </div>

                {/* The audition archive: delivery clips, and the renders the
                    operator's copy is cut from. One budget covers both. */}
                <div class="preferences-audition">
                  <div class="preferences-path-row preferences-audition-media">
                    <span class="preferences-path selectable">
                      {loaded().sessions.auditions.media.dir}
                    </span>
                    <span class="preferences-size">
                      {auditionUsage(
                        loaded().sessions.auditions.media.files,
                        loaded().sessions.auditions.media.bytes,
                        loaded().sessions.auditions.budget_bytes,
                      )}
                    </span>
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-open-folder"
                      onClick={() =>
                        void storageStore.openFolder(loaded().sessions.auditions.media.dir)
                      }
                    >{i18n.t("preferences.storage.openFolder")}</button>
                  </div>
                  <div class="preferences-path-row preferences-audition-renders">
                    <span class="preferences-path selectable">
                      {loaded().sessions.auditions.renders.dir}
                    </span>
                    <span class="preferences-size">
                      {auditionUsage(
                        loaded().sessions.auditions.renders.files,
                        loaded().sessions.auditions.renders.bytes,
                        loaded().sessions.auditions.budget_bytes,
                      )}
                    </span>
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-open-folder"
                      onClick={() =>
                        void storageStore.openFolder(loaded().sessions.auditions.renders.dir)
                      }
                    >{i18n.t("preferences.storage.openFolder")}</button>
                  </div>
                  <p class="preferences-hint">
                    {i18n.t("preferences.storage.auditionHint")}
                  </p>
                  <div class="preferences-path-row">
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-clear-auditions"
                      disabled={
                        storageStore.clearing()
                        || loaded().sessions.auditions.media.files
                          + loaded().sessions.auditions.renders.files
                          === 0
                      }
                      {...useTooltip(() => i18n.t("preferences.storage.clearAuditionsHint"))}
                      onClick={() => void clearAuditions()}
                    >{i18n.t("preferences.storage.clearAuditions")}</button>
                  </div>
                </div>
              </div>

              {/* SoundFonts: packs the index knows, and files on disk. */}
              <div class="preferences-block">
                <BlockHeader
                  label={i18n.t("preferences.storage.soundfonts")}
                  size={megabytes(loaded().soundfonts.bytes)}
                  section="soundfonts"
                />
                <span class="preferences-fact-value preferences-soundfont-count">
                  {i18n.t("preferences.storage.installedCount", {
                    count: loaded().soundfonts.installed,
                  })}
                </span>
                <p class="preferences-hint">
                  {i18n.t("preferences.storage.soundfontUsage", {
                    packs: loaded().soundfonts.installed,
                    files: loaded().soundfonts.files,
                  })}
                </p>
                <div class="preferences-path-row">
                  <span class="preferences-path preferences-soundfonts-path selectable">
                    {loaded().soundfonts.dir}
                  </span>
                  <button
                    type="button"
                    class="toolbar-btn text-btn preferences-open-folder preferences-open-soundfonts"
                    onClick={() => void storageStore.openFolder(loaded().soundfonts.dir)}
                  >{i18n.t("preferences.storage.openFolder")}</button>
                </div>
              </div>

              {/* Plugins: totals by format, then each search folder and its size. */}
              <div class="preferences-block">
                <BlockHeader
                  label={i18n.t("preferences.storage.plugins")}
                  size={megabytes(loaded().plugins.bytes)}
                  section="plugins"
                />
                <Show
                  when={loaded().plugins.by_format.length > 0}
                  fallback={
                    <span class="preferences-fact-value preferences-plugin-count">
                      {i18n.t("preferences.storage.none")}
                    </span>
                  }
                >
                  <span class="preferences-fact-value preferences-plugin-count">
                    <For each={loaded().plugins.by_format}>
                      {(entry) => (
                        <span class="preferences-plugin-format" data-format={entry.format}>
                          {i18n.t("preferences.storage.formatCount", {
                            count: entry.count,
                            format: entry.format,
                          })}
                          {" · "}
                          {megabytes(entry.bytes)}
                        </span>
                      )}
                    </For>
                  </span>
                </Show>
                <For each={loaded().plugins.folders}>
                  {(folder) => (
                    <div class="preferences-path-row">
                      <span class="preferences-path selectable">{folder.path}</span>
                      <span class="preferences-size">{megabytes(folder.bytes)}</span>
                      <button
                        type="button"
                        class="toolbar-btn text-btn preferences-open-folder"
                        onClick={() => void storageStore.openFolder(folder.path)}
                      >{i18n.t("preferences.storage.openFolder")}</button>
                    </div>
                  )}
                </For>
              </div>

              <Show when={loaded().clip_pcm}>
                {(pcm) => (
                  <div class="preferences-block preferences-clip-cache">
                    <BlockHeader
                      label={i18n.t("preferences.storage.clipCache")}
                      size={megabytes(
                        pcm().decoded.cached_bytes + clipTemporaryBytes(pcm().decoded)
                        + pcm().processed.cached_bytes + clipTemporaryBytes(pcm().processed),
                      )}
                      section="clip_pcm"
                    />
                    <ClipCacheRow
                      label={i18n.t("preferences.storage.clipCacheDecoded")}
                      usage={pcm().decoded}
                    />
                    <ClipCacheRow
                      label={i18n.t("preferences.storage.clipCacheProcessed")}
                      usage={pcm().processed}
                    />
                    <div class="preferences-path-row">
                      <button
                        type="button"
                        class="toolbar-btn text-btn preferences-clear-clip-cache"
                        disabled={storageStore.clearing()
                          || pcm().decoded.cached_bytes + pcm().processed.cached_bytes === 0}
                        onClick={() => void clearClipCache()}
                      >{i18n.t("preferences.storage.clipCacheClear")}</button>
                    </div>
                    <Show when={storageStore.status() === "clip-cache-cleared"}>
                      <p class="preferences-hint" role="status">
                        {i18n.t("preferences.storage.clipCacheCleared")}
                      </p>
                    </Show>
                  </div>
                )}
              </Show>

              {/* Sample cache of the open project. */}
              <div class="preferences-block">
                <BlockHeader
                  label={i18n.t("preferences.storage.sampleCache")}
                  size={loaded().samples.dir ? megabytes(loaded().samples.bytes) : i18n.t("preferences.storage.noProject")}
                  section="samples"
                />
                <Show
                  when={loaded().samples.dir}
                  fallback={
                    <span class="preferences-fact-value preferences-sample-count">
                      {i18n.t("preferences.storage.noProject")}
                    </span>
                  }
                >
                  {(dir) => (
                    <>
                      <span class="preferences-fact-value preferences-sample-count">
                        {i18n.t("preferences.storage.sampleUsage", {
                          files: loaded().samples.files,
                          size: megabytes(loaded().samples.bytes),
                        })}
                      </span>
                      <p class="preferences-hint">{dir()}</p>
                      <div class="preferences-path-row">
                        <button
                          type="button"
                          class="toolbar-btn text-btn preferences-open-folder preferences-open-samples"
                          onClick={() => void storageStore.openFolder(dir())}
                        >{i18n.t("preferences.storage.openFolder")}</button>
                        <button
                          type="button"
                          class="toolbar-btn text-btn preferences-clear-samples"
                          disabled={storageStore.clearing() || loaded().samples.files === 0}
                          {...useTooltip(() => i18n.t("preferences.storage.clearHint"))}
                          onClick={() => void clearSamples()}
                        >{i18n.t("preferences.storage.clear")}</button>
                      </div>
                    </>
                  )}
                </Show>
              </div>

              <div class="preferences-block">
                <div class="preferences-field">
                  <span class="preferences-field-label">{i18n.t("preferences.storage.settingsFile")}</span>
                  <div class="preferences-path-row">
                    <span class="preferences-path selectable">{loaded().preferences}</span>
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-open-folder preferences-open-preferences"
                      onClick={() => void storageStore.openFolder(parentOf(loaded().preferences))}
                    >{i18n.t("preferences.storage.openFolder")}</button>
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-export-bundle"
                      onClick={() => void openPreferencesExportDialog()}
                    >{i18n.t("preferences.export")}</button>
                    <button
                      type="button"
                      class="toolbar-btn text-btn preferences-import-bundle"
                      onClick={() => void openPreferencesImportDialog()}
                    >{i18n.t("preferences.import")}</button>
                  </div>
                </div>
              </div>
            </section>

            <Show when={storageStore.error()}>
        {(message) => <p class="preferences-error selectable">{message()}</p>}
      </Show>

      <Show when={storageStore.status() === "cleared"}>
              <p class="preferences-hint preferences-cleared" role="status">
                {i18n.t("preferences.storage.cleared")}
              </p>
            </Show>

      <Show when={storageStore.status() === "auditions-cleared"}>
              <p class="preferences-hint preferences-auditions-cleared" role="status">
                {i18n.t("preferences.storage.auditionsCleared")}
              </p>
            </Show>
          </>
        )}
      </Show>
    </>
  );
}
