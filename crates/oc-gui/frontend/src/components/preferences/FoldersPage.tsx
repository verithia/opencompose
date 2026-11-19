/**
 * The Folders page: every folder searched for plugins and sounds, grouped by
 * why it is searched, with the user's own folders editable and a rescan that
 * reports the same progress as the first-run scan.
 *
 * Folders that come from the system, the environment or another app's
 * settings are shown read-only: they change where they are set, not here.
 */
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { i18n, type I18nKey } from "../../stores/i18n";
import type { FolderOrigin } from "../../stores/api";
import { localScan } from "../../stores/localScan";
import { FolderIcon } from "../icons";
import { ScanPhases, count, folderRows, isListed, useFolderEditing, type FolderKind, type FolderRow } from "../LocalFolders";

/** Group order per kind: the editable group first, then the read-only ones. */
const ORIGINS: Record<FolderKind, readonly FolderOrigin[]> = {
  plugins: ["user", "standard", "convention", "environment", "host_settings"],
  sounds: ["user", "managed", "standard", "convention"],
};

const ORIGIN_TEXT: Record<FolderOrigin, { title: I18nKey; hint: I18nKey }> = {
  user: { title: "folders.origin.user", hint: "folders.originHint.user" },
  standard: { title: "folders.origin.standard", hint: "folders.originHint.standard" },
  convention: { title: "folders.origin.convention", hint: "folders.originHint.convention" },
  environment: { title: "folders.origin.environment", hint: "folders.originHint.environment" },
  host_settings: { title: "folders.origin.hostSettings", hint: "folders.originHint.hostSettings" },
  managed: { title: "folders.origin.managed", hint: "folders.originHint.managed" },
};

/** `null` stands for origins this build does not know. */
type GroupKey = FolderOrigin | null;

const inGroup = (row: FolderRow, key: GroupKey, kind: FolderKind) =>
  key === null ? !ORIGINS[kind].includes(row.folder.origin) : row.folder.origin === key;

/**
 * The groups one kind's listed rows fall into, in `ORIGINS` order. Folders of
 * an unknown origin are still listed, in a last group without a title. The
 * user's group stays even when empty, so an added folder has a place to land.
 */
function groupKeys(rows: FolderRow[], kind: FolderKind): GroupKey[] {
  const keys: GroupKey[] = ORIGINS[kind].filter(
    (origin) => origin === "user" || rows.some((row) => row.folder.origin === origin),
  );
  if (rows.some((row) => inGroup(row, null, kind))) keys.push(null);
  return keys;
}

export function FoldersPage() {
  const [busy, setBusy] = createSignal(false);
  const editing = useFolderEditing({ busy, onBusy: setBusy });
  const rows = createMemo(() => folderRows(localScan.summary()));

  onMount(() => {
    void localScan.load();
  });

  const section = (kind: FolderKind) => {
    const all = () => rows().filter((row) => row.kind === kind);
    const listed = () => all().filter(isListed);
    return (
      <div class="preferences-block preferences-folders" data-kind={kind}>
        <div class="preferences-block-head">
          <span class="preferences-fact-label">
            {i18n.t(kind === "plugins" ? "folders.plugins" : "folders.sounds")}
          </span>
          <button
            type="button"
            class="toolbar-btn text-btn preferences-folder-add"
            disabled={busy()}
            onClick={() => void editing.add(kind)}
          >
            <FolderIcon />
            {i18n.t(kind === "plugins" ? "onboarding.local.add.plugins" : "onboarding.local.add.sounds")}
          </button>
        </div>
        <For each={groupKeys(listed(), kind)}>
          {(key) => {
            const members = () => listed().filter((row) => inGroup(row, key, kind));
            return (
              <div class="preferences-folder-group" data-origin={key ?? "other"}>
                <Show when={key}>
                  {(origin) => (
                    <>
                      <span class="preferences-field-label">{i18n.t(ORIGIN_TEXT[origin()].title)}</span>
                      <p class="preferences-hint">{i18n.t(ORIGIN_TEXT[origin()].hint)}</p>
                    </>
                  )}
                </Show>
                <Show when={key === "user" && members().length === 0}>
                  <p class="preferences-hint preferences-folder-empty">{i18n.t("folders.noneAdded")}</p>
                </Show>
                <ul class="preferences-folder-list">
                  <For each={members()}>
                    {(row) => (
                      <li class="preferences-path-row preferences-folder-row" classList={{ missing: !row.folder.exists }}>
                        <span class="preferences-path selectable" title={row.folder.path}>{row.folder.path}</span>
                        <Show
                          when={row.folder.exists}
                          fallback={<span class="preferences-size">{i18n.t("folders.notFound")}</span>}
                        >
                          <span class="preferences-size">{row.counts}</span>
                        </Show>
                        <Show when={row.folder.origin === "user"}>
                          <button
                            type="button"
                            class="toolbar-btn text-btn preferences-folder-remove"
                            disabled={busy() || (row.kind === "sounds" && !row.folder.library_id)}
                            aria-label={i18n.t("onboarding.local.removeFolder", { path: row.folder.path })}
                            onClick={() => void editing.remove(row)}
                          >
                            {i18n.t("onboarding.local.remove")}
                          </button>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            );
          }}
        </For>
        <Show when={all().length > listed().length}>
          <p class="preferences-hint">
            {i18n.t("onboarding.scan.absent", { count: count(all().length - listed().length) })}
          </p>
        </Show>
      </div>
    );
  };

  return (
    <>
      <p class="preferences-hint">{i18n.t("folders.intro")}</p>

      <div class="preferences-block preferences-folders-scan">
        <div class="preferences-block-head">
          <span class="preferences-fact-label">{i18n.t("onboarding.scan.titleDone")}</span>
          <button
            type="button"
            class="toolbar-btn text-btn preferences-scan preferences-folders-rescan"
            classList={{ scanning: localScan.running() }}
            disabled={busy() || localScan.running()}
            onClick={() => void localScan.start(true)}
          >
            {i18n.t("onboarding.scan.again")}
          </button>
        </div>
        <Show when={localScan.summary()}>
          {(current) => (
            <span class="preferences-fact-value preferences-folders-totals">
              <For each={["plugins", "instruments", "samples"] as const}>
                {(kind) => (
                  <span class="preferences-plugin-format" data-kind={kind}>
                    {i18n.t("folders.total", {
                      kind: i18n.t(`onboarding.scan.${kind}`),
                      count: count(current()[kind].total),
                    })}
                  </span>
                )}
              </For>
            </span>
          )}
        </Show>
        <Show when={localScan.running()}>
          <ScanPhases />
        </Show>
        <Show when={!localScan.summary() && !localScan.running() && !localScan.error()}>
          <p class="preferences-hint">{i18n.t("folders.loading")}</p>
        </Show>
        <p class="preferences-hint">{i18n.t("folders.rescanHint")}</p>
      </div>

      <Show when={localScan.error()}>
        {(error) => (
          <p class="preferences-error" role="alert">{i18n.t("onboarding.local.error", { error: error() })}</p>
        )}
      </Show>
      <Show when={editing.message()}>
        {(message) => <p class="preferences-error preferences-folder-error" role="alert">{message()}</p>}
      </Show>

      <Show when={localScan.summary()}>
        {section("plugins")}
        {section("sounds")}
      </Show>
    </>
  );
}
