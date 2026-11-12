/**
 * Compact free-plugin catalog list inside the Store surface.
 *
 * The catalog may come from the signed remote index cache or the bundled
 * fallback; every URL is resolved by the Rust backend from the active catalog.
 */
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { ask } from "@tauri-apps/plugin-dialog";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import {
  PLUGIN_CATEGORIES,
  entryCategory,
  entryRefusal,
  entryDownloadUrl,
  entryFormats,
  entryInstallable,
  entryLicense,
  entryPageOnly,
  entrySupportsPlatform,
  entryVendorPage,
  filterCatalog,
  installNeedsAdmin,
  isEntryInstalled,
  pluginStore,
} from "../stores/pluginStore";
import { cancelInstall as cancelInstallRequest } from "../stores/api";
import type { PluginCatalogEntry } from "../stores/api";

interface RowMessage {
  text: string;
  error: boolean;
}

/** Grey rows that stand in for the catalog until it answers. */
const SKELETON_ROWS = [0, 1, 2, 3, 4];

export function PluginStore() {
  const [query, setQuery] = createSignal("");
  const [category, setCategory] = createSignal("all");
  const [busyId, setBusyId] = createSignal<string | null>(null);
  const [checkingId, setCheckingId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Record<string, RowMessage>>({});

  onMount(() => {
    // Keep the catalog across mounts: only an empty store reads it again, so
    // reopening the surface paints from memory instead of waiting. The open is
    // also what asks the index for a check; nothing fetches before it.
    void pluginStore.open();
  });

  const platform = () => pluginStore.catalog()?.platform ?? "macos-aarch64";
  const entries = () => pluginStore.catalog()?.entries ?? [];
  const installed = () => pluginStore.catalog()?.installed ?? [];
  const filtered = createMemo(() => filterCatalog(entries(), platform(), query(), category()));
  const hiddenCount = createMemo(
    () => entries().filter((entry) => !entrySupportsPlatform(entry, platform())).length,
  );
  const totalForOs = createMemo(
    () => entries().filter((entry) => entrySupportsPlatform(entry, platform())).length,
  );

  const indexStatus = () => pluginStore.status();

  const sourceLabel = createMemo(() => {
    const status = indexStatus();
    const source = status?.source ?? pluginStore.catalog()?.source ?? "bundled";
    if (source === "bundled") return i18n.t("store.plugins.sourceBundled");
    if (status?.fetched_at) {
      // Whole minutes: a fractional readout printed "0.98 min ago" right after
      // a fetch and made the label depend on which side of a second it ran.
      const minutes = Math.floor(Math.max(0, Date.now() / 1000 - status.fetched_at) / 60);
      if (source === "remote") return i18n.t("store.plugins.sourceRemote", { minutes });
    }
    if (source === "remote") return i18n.t("store.plugins.sourceRemoteNow");
    const date =
      status?.generated_at?.slice(0, 10) ??
      (status?.fetched_at
        ? new Date(status.fetched_at * 1000).toISOString().slice(0, 10)
        : "");
    return date
      ? i18n.t("store.plugins.sourceCache", { date })
      : i18n.t("store.plugins.sourceBundled");
  });

  const cooldownMinutes = () => {
    const remaining = indexStatus()?.next_refresh_in_seconds ?? 0;
    return remaining > 0 ? Math.ceil(remaining / 60) : 0;
  };
  const refreshDisabled = () => pluginStore.refreshing() || cooldownMinutes() > 0;
  const refreshTitle = () =>
    cooldownMinutes() > 0
      ? i18n.t("store.plugins.refreshCooldown", { minutes: cooldownMinutes() })
      : i18n.t("store.plugins.refreshHint");

  const setMessage = (id: string, text: string, error = false) => {
    setMessages((previous) => ({ ...previous, [id]: { text, error } }));
  };

  const handleOpenPage = async (entry: PluginCatalogEntry) => {
    setMessage(entry.id, "");
    try {
      await pluginStore.openPage(entry);
    } catch (cause) {
      setMessage(entry.id, i18n.t("store.plugins.actionFailed", { error: String(cause) }), true);
    }
  };

  const handleDownload = async (entry: PluginCatalogEntry) => {
    const url = entryDownloadUrl(entry, platform());
    if (!url) return;
    setMessage(entry.id, "");
    try {
      await pluginStore.openDownload(url);
    } catch (cause) {
      setMessage(entry.id, i18n.t("store.plugins.actionFailed", { error: String(cause) }), true);
    }
  };

  /** The vendor hosts the installer: open their page and let the person finish. */
  const handleVendorDownload = async (entry: PluginCatalogEntry) => {
    setMessage(entry.id, "");
    try {
      await pluginStore.openManualPage(entry);
    } catch (cause) {
      setMessage(entry.id, i18n.t("store.plugins.actionFailed", { error: String(cause) }), true);
    }
  };

  /** Re-walk the plugin folders for a bundle the vendor installer just added. */
  const handleCheckAgain = async (entry: PluginCatalogEntry) => {
    setCheckingId(entry.id);
    setMessage(entry.id, "");
    try {
      const result = await pluginStore.checkInstalled(entry.id);
      if (result.installed) {
        setMessage(
          entry.id,
          i18n.t("store.plugins.checkFound", { name: result.matched ?? entry.name }),
        );
        // Reload so the row, the badge and the browser share one answer.
        await pluginStore.load(true);
      } else {
        setMessage(entry.id, i18n.t("store.plugins.checkMissing", { name: entry.name }));
      }
    } catch (cause) {
      setMessage(entry.id, i18n.t("store.plugins.checkFailed", { error: String(cause) }), true);
    } finally {
      setCheckingId(null);
    }
  };

  /** Ask the backend to stop the download or the unpack where it stands. */
  const cancelInstall = async (id: string) => {
    try {
      await cancelInstallRequest(id);
    } catch (cause) {
      setMessage(id, i18n.t("store.plugins.installFailed", { error: String(cause) }), true);
    }
  };

  const handleInstall = async (entry: PluginCatalogEntry) => {
    setBusyId(entry.id);
    setMessage(entry.id, "");
    try {
      let outcome = await pluginStore.install(entry);
      if (outcome.status === "needs_confirmation") {
        const confirmed = await ask(
          i18n.t("store.plugins.confirmOverwrite", { name: entry.name }),
          { title: i18n.t("store.plugins.install"), kind: "warning" },
        );
        if (!confirmed) return;
        outcome = await pluginStore.install(entry, true);
      }
      if (outcome.status === "installed") {
        setMessage(
          entry.id,
          outcome.message,
        );
        await pluginStore.load(true);
      } else {
        setMessage(entry.id, i18n.t("store.plugins.installerOpened"));
      }
    } catch (cause) {
      setMessage(entry.id, i18n.t("store.plugins.installFailed", { error: String(cause) }), true);
    } finally {
      setBusyId(null);
    }
  };

  const installTitle = (entry: PluginCatalogEntry): string => {
    // The backend resolver says why, in the same words the agent tools use.
    if (!entryInstallable(entry, platform())) {
      return entryRefusal(entry) ?? i18n.t("store.plugins.noInstall");
    }
    if (installNeedsAdmin(entry, platform())) return i18n.t("store.plugins.adminHint");
    return i18n.t("store.plugins.install");
  };

  return (
    <div class="plugin-store">
      <div class="plugin-store-filters">
        <input
          type="text"
          class="plugin-store-search"
          placeholder={i18n.t("store.plugins.search")}
          aria-label={i18n.t("store.plugins.search")}
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <select
          class="plugin-store-category"
          aria-label={i18n.t("store.plugins.category")}
          value={category()}
          onChange={(event) => setCategory(event.currentTarget.value)}
        >
          <option value="all">{i18n.t("store.plugins.all")}</option>
          <For each={PLUGIN_CATEGORIES}>
            {(id) => <option value={id}>{i18n.t(`store.category.${id}`)}</option>}
          </For>
        </select>
      </div>

      <div class="plugin-store-meta">
        <span class="plugin-store-source" {...useTooltip(() => indexStatus()?.catalog_version ?? "")}>
          {sourceLabel()}
        </span>
        <span class="plugin-store-meta-right">
          <span>
            {i18n.t("store.plugins.count", { shown: filtered().length, total: totalForOs() })}
          </span>
          <Show when={hiddenCount() > 0}>
            <span class="plugin-store-hidden">
              {i18n.t("store.plugins.hidden", { count: hiddenCount() })}
            </span>
          </Show>
          <button
            class="plugin-action plugin-store-refresh"
            disabled={refreshDisabled()}
            aria-label={refreshTitle()}
            {...useTooltip(() => refreshTitle())}
            onClick={() => void pluginStore.refresh(true)}
          >
            {pluginStore.refreshing()
              ? i18n.t("store.plugins.refreshing")
              : i18n.t("store.plugins.refresh")}
          </button>
        </span>
      </div>

      <Show when={indexStatus()?.stale_warning}>
        <div class="plugin-store-warning" role="status">
          {i18n.t("store.plugins.stale", { days: indexStatus()?.stale_days ?? 0 })}
        </div>
      </Show>

      <Show when={pluginStore.refreshError() ?? indexStatus()?.last_error}>
        <div class="plugin-store-warning error selectable" role="alert">
          {i18n.t("store.plugins.refreshFailed", {
            error: pluginStore.refreshError() ?? indexStatus()?.last_error ?? "",
          })}
        </div>
      </Show>

      <div class="plugin-store-scroll">
        <Show
          when={pluginStore.catalog()}
          fallback={
            <Show
              when={pluginStore.error()}
              fallback={
                <div class="plugin-store-skeleton" role="status" aria-busy="true">
                  <div class="empty-state-text">{i18n.t("store.plugins.loading")}</div>
                  <For each={SKELETON_ROWS}>
                    {() => (
                      <div class="plugin-row plugin-row-skeleton" aria-hidden="true">
                        <span class="skeleton-line skeleton-line-title" />
                        <span class="skeleton-line skeleton-line-meta" />
                      </div>
                    )}
                  </For>
                </div>
              }
            >
              <div class="plugin-store-empty" role="alert">
                <div class="empty-state-text">{i18n.t("store.plugins.error")}</div>
                <div class="empty-state-hint selectable">{pluginStore.error()}</div>
                <button class="plugin-action" onClick={() => void pluginStore.load(true)}>
                  {i18n.t("store.plugins.retry")}
                </button>
              </div>
            </Show>
          }
        >
          <For each={filtered()}>
            {(entry) => {
              const downloadUrl = () => entryDownloadUrl(entry, platform());
              const vendorPage = () => entryVendorPage(entry, platform());
              const pageOnly = () => entryPageOnly(entry, platform());
              const installable = () => entryInstallable(entry, platform());
              const busy = () => busyId() === entry.id;
              const checking = () => checkingId() === entry.id;
              const message = () => messages()[entry.id];
              return (
                <div class="plugin-row" data-plugin-id={entry.id}>
                  <div class="plugin-row-head">
                    <span class="plugin-row-name selectable" {...useTooltip(() => entry.name)}>
                      {entry.name}
                    </span>
                    <Show when={isEntryInstalled(entry, installed())}>
                      <span class="plugin-badge installed">
                        {i18n.t("store.plugins.installed")}
                      </span>
                    </Show>
                    <Show when={installNeedsAdmin(entry, platform())}>
                      <span
                        class="plugin-badge admin"
                        aria-label={i18n.t("store.plugins.adminHint")}
                        {...useTooltip(() => i18n.t("store.plugins.adminHint"))}
                      >
                        {i18n.t("store.plugins.admin")}
                      </span>
                    </Show>
                  </div>
                  <div class="plugin-row-meta selectable">
                    {entry.vendor} · {entryCategory(entry)} · {entryLicense(entry)} ·{" "}
                    {entryFormats(entry, platform()).join("/")}
                  </div>
                  <div class="plugin-row-actions">
                    <button class="plugin-action" onClick={() => void handleOpenPage(entry)}>
                      {i18n.t("store.plugins.openPage")}
                    </button>
                    <Show
                      when={pageOnly()}
                      fallback={
                        <button
                          class="plugin-action"
                          disabled={!downloadUrl()}
                          {...useTooltip(() => downloadUrl() ? downloadUrl() : i18n.t("store.plugins.noDownload"))}
                          onClick={() => void handleDownload(entry)}
                        >
                          {i18n.t("store.plugins.download")}
                        </button>
                      }
                    >
                      <button
                        class="plugin-action plugin-vendor-download"
                        {...useTooltip(() => vendorPage() ?? i18n.t("store.plugins.downloadFromVendorHint"))}
                        onClick={() => void handleVendorDownload(entry)}
                      >
                        {i18n.t("store.plugins.downloadFromVendor")}
                      </button>
                    </Show>
                    <button
                      class="plugin-action primary"
                      disabled={!installable() || busyId() !== null}
                      {...useTooltip(() => installTitle(entry))}
                      onClick={() => void handleInstall(entry)}
                    >
                      {busy()
                        ? i18n.t("store.plugins.installing")
                        : i18n.t("store.plugins.install")}
                    </button>
                    <Show when={busy()}>
                      <button
                        class="plugin-action"
                        aria-label={i18n.t("store.installCancel", { name: entry.name })}
                        onClick={() => void cancelInstall(entry.id)}
                      >{i18n.t("store.installCancelShort")}</button>
                    </Show>
                    <Show when={pageOnly() && !isEntryInstalled(entry, installed())}>
                      <button
                        class="plugin-action plugin-check-installed"
                        disabled={checking()}
                        {...useTooltip(() => i18n.t("store.plugins.checkHint"))}
                        onClick={() => void handleCheckAgain(entry)}
                      >
                        {checking()
                          ? i18n.t("store.plugins.checking")
                          : i18n.t("store.plugins.checkAgain")}
                      </button>
                    </Show>
                  </div>
                  <Show when={message()?.text}>
                    <div
                      class="plugin-row-message selectable"
                      classList={{ error: message()?.error }}
                    >
                      {message()?.text}
                    </div>
                  </Show>
                </div>
              );
            }}
          </For>

          <Show when={filtered().length === 0}>
            <div class="plugin-store-empty">
              <div class="empty-state-text">{i18n.t("store.plugins.empty")}</div>
              <div class="empty-state-hint">{i18n.t("store.plugins.emptyHint")}</div>
            </div>
          </Show>
        </Show>
      </div>
    </div>
  );
}
