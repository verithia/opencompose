/**
 * The guide's scan page: progress while the plugin inventory and the sound
 * folders are read, then what was found, in folded scrollable lists, with the
 * folders that were searched and a way to add more.
 */
import { For, Show, createMemo } from "solid-js";
import { i18n } from "../stores/i18n";
import type { FoundItem, LocalContentKind } from "../stores/api";
import { localScan } from "../stores/localScan";
import { FolderIcon, KeyboardIcon, PlugIcon, WaveIcon } from "./icons";
import { ScanPhases, count, folderRows, isListed, originBadge, useFolderEditing } from "./LocalFolders";

type Kind = "plugins" | "instruments" | "samples";
const KINDS: readonly Kind[] = ["plugins", "instruments", "samples"];

/** Format names are product names, so only the sampler asset is translated. */
export function formatLabel(format: string): string {
  return format === "sampler_asset" ? i18n.t("onboarding.local.samplerAsset") : format.toUpperCase();
}

function formatLine(kind: LocalContentKind): string {
  return kind.formats.map((row) => `${formatLabel(row.format)} ${row.count}`).join(" · ");
}

export function OnboardingScan(props: { busy: boolean; onBusy: (busy: boolean) => void }) {
  const summary = localScan.summary;
  const rows = createMemo(() => folderRows(summary()));
  const listed = () => rows().filter(isListed);
  const absent = () => rows().length - listed().length;
  const editing = useFolderEditing({ busy: () => props.busy, onBusy: (busy) => props.onBusy(busy) });

  const icon = (kind: Kind) =>
    kind === "plugins" ? <PlugIcon /> : kind === "instruments" ? <KeyboardIcon /> : <WaveIcon />;

  return (
    <section class="onboarding-scan" aria-labelledby="onboarding-title">
      <Show when={localScan.running() || !summary()}>
        <ScanPhases />
      </Show>

      <Show when={localScan.error()}>
        {(error) => <p class="onboarding-tools-error" role="alert">{i18n.t("onboarding.local.error", { error: error() })}</p>}
      </Show>
      <Show when={editing.message()}>
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
                            onClick={() => void editing.remove(row)}
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
          <button type="button" class="onboarding-secondary" disabled={props.busy} onClick={() => void editing.add("plugins")}>
            <FolderIcon />
            {i18n.t("onboarding.local.add.plugins")}
          </button>
          <button type="button" class="onboarding-secondary" disabled={props.busy} onClick={() => void editing.add("sounds")}>
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
