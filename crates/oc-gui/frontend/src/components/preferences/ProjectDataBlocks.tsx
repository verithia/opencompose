/**
 * Storage page blocks for what the app keeps outside a project's folder: the
 * data of each project and workspace folder, and the app's other folders.
 * Sizes come from the backend scan; removal is offered only for data whose
 * file or folder is gone, and the backend checks that again.
 */
import { For, Show, createMemo } from "solid-js";
import { ask } from "@tauri-apps/plugin-dialog";
import { i18n } from "../../stores/i18n";
import type { AppFolder, DataItem, DataKind, ProjectDataStorage } from "../../stores/api";
import { storageStore, type StorageSection } from "../../stores/storage";
import { sizeText } from "../../stores/historyText";

/** The items of one project or folder, in a fixed kind order. */
interface DataGroup {
  path: string | null;
  items: DataItem[];
  bytes: number;
  gone: boolean;
}

const KIND_ORDER: readonly DataKind[] = ["history", "note", "recovery", "conversations"];

export function groupProjectData(items: readonly DataItem[]): DataGroup[] {
  const groups = new Map<string, DataGroup>();
  for (const item of items) {
    const id = item.path ?? `\u0000${item.kind}:${item.key}`;
    const group = groups.get(id) ?? { path: item.path ?? null, items: [], bytes: 0, gone: false };
    group.items.push(item);
    group.bytes += item.bytes;
    group.gone ||= item.gone;
    groups.set(id, group);
  }
  for (const group of groups.values()) {
    group.items.sort((left, right) => KIND_ORDER.indexOf(left.kind) - KIND_ORDER.indexOf(right.kind));
  }
  // Gone data first: it is what the person may want to act on.
  return [...groups.values()].sort((left, right) =>
    Number(right.gone) - Number(left.gone) || right.bytes - left.bytes);
}

function kindName(kind: DataKind): string {
  return i18n.t(`preferences.storage.dataKind.${kind}`);
}

function ScanHeader(props: { label: string; size: string; section: StorageSection }) {
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

export function ProjectDataBlock(props: { data: ProjectDataStorage }) {
  const groups = createMemo(() => groupProjectData(props.data.items));
  const remove = async (item: DataItem, path: string) => {
    const confirmed = await ask(
      i18n.t("preferences.storage.removeDataConfirm", { kind: kindName(item.kind), path }),
      { title: i18n.t("preferences.storage.removeData"), kind: "warning" },
    );
    if (confirmed) await storageStore.removeProjectData(item.kind, item.key);
  };
  return (
    <div class="preferences-block preferences-project-data">
      <ScanHeader
        label={i18n.t("preferences.storage.projectData")}
        size={sizeText(props.data.total_bytes)}
        section="project_data"
      />
      <p class="preferences-hint">
        {i18n.t("preferences.storage.projectDataHint", { days: props.data.stale_after_days })}
      </p>
      <For each={groups()}>
        {(group) => {
          const path = () => group.path ?? i18n.t("preferences.storage.dataUnknownPath");
          return (
            <div class="preferences-data-group" data-gone={group.gone ? "true" : undefined}>
              <div class="preferences-path-row">
                <span class="preferences-path selectable">{path()}</span>
                <span class="preferences-size">{sizeText(group.bytes)}</span>
              </div>
              <For each={group.items}>
                {(item) => (
                  <div class="preferences-session-row preferences-data-row" data-kind={item.kind}>
                    <span class="preferences-session-title">{kindName(item.kind)}</span>
                    <span class="preferences-size">{sizeText(item.bytes)}</span>
                    <Show when={item.gone}>
                      <span class="preferences-data-state">
                        {item.due
                          ? i18n.t("preferences.storage.dataDue")
                          : i18n.t("preferences.storage.dataGone")}
                      </span>
                      <button
                        type="button"
                        class="toolbar-btn text-btn preferences-remove-data"
                        disabled={storageStore.clearing()}
                        onClick={() => void remove(item, path())}
                      >{i18n.t("preferences.storage.removeData")}</button>
                    </Show>
                  </div>
                )}
              </For>
            </div>
          );
        }}
      </For>
    </div>
  );
}

export function AppFoldersBlock(props: { folders: readonly AppFolder[] }) {
  const total = () => props.folders.reduce((sum, folder) => sum + folder.bytes, 0);
  return (
    <div class="preferences-block preferences-app-folders">
      <ScanHeader
        label={i18n.t("preferences.storage.appFolders")}
        size={sizeText(total())}
        section="app_folders"
      />
      <For each={props.folders}>
        {(folder) => (
          <div class="preferences-field" data-folder={folder.id}>
            <span class="preferences-fact-label">
              {i18n.t(`preferences.storage.folder.${folder.id}`)}
            </span>
            <div class="preferences-path-row">
              <span class="preferences-path selectable">{folder.path}</span>
              <span class="preferences-size">
                {i18n.t("preferences.storage.folderUsage", {
                  files: folder.files,
                  size: sizeText(folder.bytes),
                })}
              </span>
              <button
                type="button"
                class="toolbar-btn text-btn preferences-open-folder"
                disabled={folder.files === 0}
                onClick={() => void storageStore.openFolder(folder.path)}
              >{i18n.t("preferences.storage.openFolder")}</button>
            </div>
          </div>
        )}
      </For>
    </div>
  );
}
