/**
 * The Folders page: every folder searched for plugins and sounds as one
 * browsable list. Tabs narrow it to one kind, each origin is a group that
 * folds, a row reads as a folder name with its location dimmed beside it, and
 * the selected folder's full path, source and contents show in a details
 * strip at the foot of the list.
 *
 * Folders that come from the system, the environment or another app's
 * settings are shown read-only: they change where they are set, not here.
 */
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { i18n, type I18nKey } from "../../stores/i18n";
import * as api from "../../stores/api";
import type { FolderOrigin } from "../../stores/api";
import { localScan } from "../../stores/localScan";
import { FolderIcon, PlugIcon, WaveIcon } from "../icons";
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

const KINDS: readonly FolderKind[] = ["plugins", "sounds"];
type Tab = "all" | FolderKind;
const TABS: readonly { id: Tab; key: I18nKey }[] = [
  { id: "all", key: "folders.tab.all" },
  { id: "plugins", key: "folders.tab.plugins" },
  { id: "sounds", key: "folders.tab.sounds" },
];

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

/**
 * A path split into its last segment and the folder that holds it, for either
 * separator, so a Windows path from another app's settings reads the same way.
 * A trailing separator is ignored; a root keeps its whole text as the name.
 */
export function splitPath(path: string): { name: string; parent: string } {
  const trimmed = path.length > 1 ? path.replace(/[\\/]+$/, "") : path;
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (cut < 0 || cut === trimmed.length - 1) return { name: trimmed || path, parent: "" };
  return { name: trimmed.slice(cut + 1), parent: trimmed.slice(0, cut + 1) };
}

const rowKey = (row: FolderRow) => `${row.kind}:${row.folder.path}`;
const groupId = (kind: FolderKind, key: GroupKey) => `${kind}:${key ?? "other"}`;

export function FoldersPage() {
  const [busy, setBusy] = createSignal(false);
  const [tab, setTab] = createSignal<Tab>("all");
  const [folded, setFolded] = createSignal<ReadonlySet<string>>(new Set());
  const [selectedKey, setSelectedKey] = createSignal<string | null>(null);
  const [revealError, setRevealError] = createSignal<string | null>(null);
  const editing = useFolderEditing({ busy, onBusy: setBusy });
  const rows = createMemo(() => folderRows(localScan.summary()));
  const listedOf = (kind: FolderKind) => rows().filter((row) => row.kind === kind && isListed(row));
  const shownKinds = () => KINDS.filter((kind) => tab() === "all" || tab() === kind);
  /** The selection follows the list: a folder that leaves it is unselected. */
  const selected = createMemo(() => {
    const key = selectedKey();
    const row = key ? rows().find((candidate) => rowKey(candidate) === key) : undefined;
    return row && shownKinds().includes(row.kind) && isListed(row) ? row : null;
  });

  onMount(() => {
    void localScan.load();
  });

  const toggleGroup = (id: string) => {
    const next = new Set(folded());
    if (!next.delete(id)) next.add(id);
    setFolded(next);
  };

  const reveal = async (path: string) => {
    setRevealError(null);
    try {
      await api.revealPath(path);
    } catch (error) {
      setRevealError(i18n.t("folders.revealFailed", { error: String(error) }));
    }
  };

  /**
   * Arrow keys, Home and End move between the tabs and select the one they
   * land on, so the tab list is a single Tab stop as a tab list is expected
   * to be.
   */
  const onTabKey = (event: KeyboardEvent) => {
    const at = TABS.findIndex((entry) => entry.id === tab());
    const next = {
      ArrowRight: (at + 1) % TABS.length,
      ArrowLeft: (at - 1 + TABS.length) % TABS.length,
      Home: 0,
      End: TABS.length - 1,
    }[event.key];
    if (next === undefined) return;
    event.preventDefault();
    setTab(TABS[next].id);
    (event.currentTarget as HTMLElement)
      .querySelector<HTMLElement>(`[data-tab="${TABS[next].id}"]`)
      ?.focus();
  };

  const tabCount = (id: Tab) =>
    id === "all" ? KINDS.reduce((sum, kind) => sum + listedOf(kind).length, 0) : listedOf(id).length;

  const folderRow = (row: FolderRow) => {
    const parts = splitPath(row.folder.path);
    const isSelected = () => selected() === row;
    return (
      <li
        class="preferences-path-row preferences-folder-row"
        classList={{ missing: !row.folder.exists, selected: isSelected() }}
        data-kind={row.kind}
      >
        <button
          type="button"
          class="preferences-folder-select"
          aria-pressed={isSelected()}
          title={row.folder.path}
          onClick={() => {
            setRevealError(null);
            setSelectedKey(isSelected() ? null : rowKey(row));
          }}
        >
          <span class="preferences-folder-icon" aria-hidden="true">
            {row.kind === "plugins" ? <PlugIcon /> : <WaveIcon />}
          </span>
          <span class="preferences-folder-name">{parts.name}</span>
          <span class="preferences-path">{parts.parent}</span>
        </button>
        <Show
          when={row.folder.exists}
          fallback={<span class="preferences-size preferences-folder-missing">{i18n.t("folders.notFound")}</span>}
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
    );
  };

  const section = (kind: FolderKind) => {
    const all = () => rows().filter((row) => row.kind === kind);
    const listed = () => listedOf(kind);
    return (
      <section class="preferences-folders" data-kind={kind} aria-label={i18n.t(kind === "plugins" ? "folders.plugins" : "folders.sounds")}>
        <div class="preferences-folder-kind">
          <span class="preferences-folder-icon" aria-hidden="true">
            {kind === "plugins" ? <PlugIcon /> : <WaveIcon />}
          </span>
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
            const id = groupId(kind, key);
            const open = () => !folded().has(id);
            const listId = `preferences-folder-group-${kind}-${key ?? "other"}`;
            return (
              <div class="preferences-folder-group" data-origin={key ?? "other"} classList={{ folded: !open() }}>
                <Show when={key}>
                  {(origin) => (
                    <button
                      type="button"
                      class="preferences-folder-group-head"
                      aria-expanded={open()}
                      aria-controls={listId}
                      title={i18n.t(ORIGIN_TEXT[origin()].hint)}
                      onClick={() => toggleGroup(id)}
                    >
                      <span class="preferences-folder-caret" aria-hidden="true" />
                      <span class="preferences-field-label">{i18n.t(ORIGIN_TEXT[origin()].title)}</span>
                      <span class="preferences-folder-group-count">{count(members().length)}</span>
                      <span class="preferences-hint preferences-folder-group-hint">{i18n.t(ORIGIN_TEXT[origin()].hint)}</span>
                    </button>
                  )}
                </Show>
                <Show when={open()}>
                  <Show when={key === "user" && members().length === 0}>
                    <p class="preferences-hint preferences-folder-empty">{i18n.t("folders.noneAdded")}</p>
                  </Show>
                  <ul class="preferences-folder-list" id={listId}>
                    <For each={members()}>{folderRow}</For>
                  </ul>
                </Show>
              </div>
            );
          }}
        </For>
        <Show when={all().length > listed().length}>
          <p class="preferences-hint preferences-folder-absent">
            {i18n.t("onboarding.scan.absent", { count: count(all().length - listed().length) })}
          </p>
        </Show>
      </section>
    );
  };

  const details = () => (
    <div class="preferences-folder-details" aria-live="polite">
      <Show
        when={selected()}
        fallback={<p class="preferences-hint">{i18n.t("folders.details.empty")}</p>}
      >
        {(row) => {
          const origin = () => ORIGIN_TEXT[row().folder.origin] as (typeof ORIGIN_TEXT)[FolderOrigin] | undefined;
          return (
            <>
              <dl class="preferences-folder-facts">
                <dt>{i18n.t("folders.details.name")}</dt>
                <dd class="preferences-folder-details-name">{splitPath(row().folder.path).name}</dd>
                <dt>{i18n.t("folders.details.path")}</dt>
                <dd class="preferences-folder-details-path selectable">{row().folder.path}</dd>
                <dt>{i18n.t("folders.details.source")}</dt>
                <dd>
                  <Show when={origin()} fallback={row().folder.origin}>
                    {(text) => (
                      <>
                        <span class="preferences-folder-details-origin">{i18n.t(text().title)}</span>
                        <span class="preferences-folder-details-hint">{i18n.t(text().hint)}</span>
                      </>
                    )}
                  </Show>
                </dd>
                <dt>{i18n.t("folders.details.contents")}</dt>
                <dd>{row().folder.exists ? row().counts : i18n.t("folders.notFound")}</dd>
              </dl>
              <button
                type="button"
                class="toolbar-btn text-btn preferences-folder-reveal"
                disabled={!row().folder.exists}
                onClick={() => void reveal(row().folder.path)}
              >
                {i18n.t("browse.reveal")}
              </button>
            </>
          );
        }}
      </Show>
      <Show when={revealError()}>
        {(message) => <p class="preferences-error" role="alert">{message()}</p>}
      </Show>
    </div>
  );

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
        <div class="preferences-block preferences-folder-browser">
          <div
            class="preferences-folder-tabs"
            role="tablist"
            aria-label={i18n.t("folders.tabs")}
            onKeyDown={onTabKey}
          >
            <For each={TABS}>
              {(entry) => (
                <button
                  type="button"
                  role="tab"
                  class="preferences-folder-tab"
                  classList={{ active: tab() === entry.id }}
                  aria-selected={tab() === entry.id}
                  tabIndex={tab() === entry.id ? 0 : -1}
                  id={`preferences-folder-tab-${entry.id}`}
                  aria-controls="preferences-folder-panel"
                  data-tab={entry.id}
                  onClick={() => setTab(entry.id)}
                >
                  {i18n.t(entry.key)}{" "}
                  <span class="preferences-folder-tab-count">{count(tabCount(entry.id))}</span>
                </button>
              )}
            </For>
          </div>
          <div
            class="preferences-folder-panel"
            id="preferences-folder-panel"
            role="tabpanel"
            aria-labelledby={`preferences-folder-tab-${tab()}`}
          >
            <For each={shownKinds()}>{section}</For>
          </div>
          {details()}
        </div>
      </Show>
    </>
  );
}
