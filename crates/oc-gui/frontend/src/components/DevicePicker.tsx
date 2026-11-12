/**
 * Searchable device picker. Built-in devices come first, then the scanned
 * plugin effects; a plugin entry is handed back with its scanner id so the
 * caller can insert it as `plugin:<id>`.
 */
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import type { DeviceCategory, DeviceDef } from "../types/devices";
import type { PluginInfo } from "../stores/api";
import { CATEGORY_ORDER } from "../devices/catalog";
import { categoryLabel, deviceDescription, pluginFormatLabel } from "../devices/labels";
import { i18n } from "../stores/i18n";

export interface PickerAnchor {
  left: number;
  top: number;
  bottom: number;
}

/** A scanned plugin effect offered beside the built-in devices. */
export type PickerPlugin = Pick<PluginInfo, "id" | "name" | "vendor" | "format">;

type PickerEntry = DeviceDef | PickerPlugin;

/** Device definitions carry a category; plugin entries carry a vendor. */
function isPlugin(entry: PickerEntry): entry is PickerPlugin {
  return "vendor" in entry;
}

const PICKER_WIDTH = 280;
const PICKER_HEIGHT = 320;

export function DevicePicker(props: {
  devices: DeviceDef[];
  /** Scanned plugin effects, listed after the built-in devices. */
  plugins?: PickerPlugin[];
  anchor: PickerAnchor;
  onSelect: (device: DeviceDef) => void;
  onSelectPlugin?: (plugin: PickerPlugin) => void;
  onClose: () => void;
}) {
  const [search, setSearch] = createSignal("");
  const [active, setActive] = createSignal(0);
  let searchRef: HTMLInputElement | undefined;

  const filtered = createMemo(() => {
    const query = search().trim().toLowerCase();
    if (!query) return props.devices;
    return props.devices.filter((device) =>
      `${device.name} ${device.id} ${device.category} ${categoryLabel(device.category)} ${deviceDescription(device)}`
        .toLowerCase()
        .includes(query),
    );
  });

  const filteredPlugins = createMemo(() => {
    const query = search().trim().toLowerCase();
    const plugins = props.plugins ?? [];
    if (!query) return plugins;
    return plugins.filter((plugin) =>
      `${plugin.name} ${plugin.vendor} ${plugin.format} ${pluginFormatLabel(plugin.format)}`
        .toLowerCase()
        .includes(query),
    );
  });

  const groups = createMemo(() => devicesByCategory(filtered()));

  const flat = createMemo<PickerEntry[]>(() => [
    ...groups().flatMap((group) => group.devices),
    ...filteredPlugins(),
  ]);
  const activeIndex = () => Math.min(active(), Math.max(0, flat().length - 1));

  function select(entry: PickerEntry | undefined) {
    if (!entry) return;
    if (isPlugin(entry)) props.onSelectPlugin?.(entry);
    else props.onSelect(entry);
  }

  function onKeyDown(event: KeyboardEvent) {
    const entries = flat();
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (entries.length > 0) setActive((activeIndex() + 1) % entries.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (entries.length > 0) setActive((activeIndex() - 1 + entries.length) % entries.length);
    } else if (event.key === "Enter") {
      event.preventDefault();
      select(entries[activeIndex()]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      props.onClose();
    }
  }

  onMount(() => searchRef?.focus());

  function menuStyle(): Record<string, string> {
    const anchor = props.anchor;
    const viewportWidth = typeof window === "undefined" ? 1024 : window.innerWidth;
    const viewportHeight = typeof window === "undefined" ? 768 : window.innerHeight;
    const left = Math.max(8, Math.min(anchor.left, viewportWidth - PICKER_WIDTH - 8));
    const below = viewportHeight - anchor.bottom - 8;
    const top = below >= PICKER_HEIGHT + 8
      ? anchor.bottom + 4
      : Math.max(8, anchor.top - PICKER_HEIGHT - 4);
    return {
      left: `${left}px`,
      top: `${top}px`,
      width: `${PICKER_WIDTH}px`,
      "max-height": `${Math.min(PICKER_HEIGHT, viewportHeight - 16)}px`,
    };
  }

  return (
    <>
      <div class="device-picker-backdrop" onMouseDown={() => props.onClose()} />
      <div class="device-picker" data-device-picker style={menuStyle()} onKeyDown={onKeyDown}>
        <div class="device-picker-head">
          <span class="device-picker-title">{i18n.t("device.pickerTitle")}</span>
          <button
            class="device-picker-close"
            aria-label={i18n.t("device.close")}
            onClick={() => props.onClose()}
          >×</button>
        </div>
        <input
          ref={searchRef}
          class="device-picker-search"
          aria-label={i18n.t("device.search")}
          placeholder={i18n.t("device.searchPlaceholder")}
          value={search()}
          onInput={(event) => {
            setSearch(event.currentTarget.value);
            setActive(0);
          }}
        />
        <div class="device-picker-list" role="listbox" aria-label={i18n.t("device.pickerTitle")}>
          <Show
            when={flat().length > 0}
            fallback={<div class="device-picker-empty">{i18n.t("device.noResults")}</div>}
          >
            <For each={groups()}>
              {(group) => (
                <div class="device-picker-group" role="group" aria-label={categoryLabel(group.category)}>
                  <div class="device-picker-group-title">{categoryLabel(group.category)}</div>
                  <For each={group.devices}>
                    {(device) => {
                      const isActive = () => flat().indexOf(device) === activeIndex();
                      return (
                        <div
                          role="option"
                          aria-selected={isActive()}
                          class="device-picker-item"
                          classList={{ active: isActive() }}
                          data-device-id={device.id}
                          onMouseEnter={() => setActive(flat().indexOf(device))}
                          onClick={() => select(device)}
                        >
                          <span class="device-picker-item-head">
                            <span class="device-picker-item-name">{device.name}</span>
                          </span>
                          <span class="device-picker-item-desc">{deviceDescription(device)}</span>
                        </div>
                      );
                    }}
                  </For>
                </div>
              )}
            </For>
            <Show when={filteredPlugins().length > 0}>
              <div class="device-picker-group" role="group" aria-label={i18n.t("device.plugins")}>
                <div class="device-picker-group-title">{i18n.t("device.plugins")}</div>
                <For each={filteredPlugins()}>
                  {(plugin) => {
                    const isActive = () => flat().indexOf(plugin) === activeIndex();
                    return (
                      <div
                        role="option"
                        aria-selected={isActive()}
                        class="device-picker-item"
                        classList={{ active: isActive() }}
                        data-plugin-id={plugin.id}
                        onMouseEnter={() => setActive(flat().indexOf(plugin))}
                        onClick={() => select(plugin)}
                      >
                        <span class="device-picker-item-head">
                          <span class="device-picker-item-name">{plugin.name}</span>
                        </span>
                        <span class="device-picker-item-desc">
                          {plugin.vendor ? `${plugin.vendor} · ` : ""}
                          {pluginFormatLabel(plugin.format)}
                        </span>
                      </div>
                    );
                  }}
                </For>
              </div>
            </Show>
          </Show>
        </div>
        <div class="device-picker-foot">{i18n.t("device.pickerHint")}</div>
      </div>
    </>
  );
}

/** Group devices in the picker category order. */
function devicesByCategory(devices: DeviceDef[]): { category: DeviceCategory; devices: DeviceDef[] }[] {
  return CATEGORY_ORDER
    .map((category) => ({ category, devices: devices.filter((device) => device.category === category) }))
    .filter((group) => group.devices.length > 0);
}
