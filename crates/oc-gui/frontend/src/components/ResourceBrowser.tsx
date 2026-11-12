/**
 * The resource browser: one in-app overlay with the plugin catalogue, the
 * sample markets and the built-in effect catalogue.
 *
 * Plugins and samples reuse the surfaces the app already ships (`PluginStore`,
 * `SampleMarketPanel`) so a result chosen here behaves exactly like one chosen
 * in the bottom panel. Effects are the built-in devices plus the scanned plugin
 * effects, added to the selected track.
 */
import { For, Match, Show, Switch, createMemo, createSignal, onMount } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { store } from "../stores/project";
import { scanPlugins, type PluginInfo } from "../stores/api";
import { CATEGORY_ORDER, loadDevicesCatalog } from "../devices/catalog";
import { categoryLabel, deviceDescription, pluginFormatLabel } from "../devices/labels";
import type { DeviceCategory, DeviceDef } from "../types/devices";
import { PluginStore } from "./PluginStore";
import { SampleMarketPanel } from "./SampleMarketPanel";
import { useTopmostEscape } from "./modalEscape";
import "./ResourceBrowser.css";

type BrowseTab = "plugins" | "samples" | "effects";

const TABS: { id: BrowseTab; key: I18nKey }[] = [
  { id: "plugins", key: "browse.tab.plugins" },
  { id: "samples", key: "browse.tab.samples" },
  { id: "effects", key: "browse.tab.effects" },
];

export function ResourceBrowser(props: { open: boolean; onClose: () => void }) {
  return (
    <Show when={props.open}>
      <ResourceBrowserDialog onClose={props.onClose} />
    </Show>
  );
}

function ResourceBrowserDialog(props: { onClose: () => void }) {
  const [tab, setTab] = createSignal<BrowseTab>("plugins");

  useTopmostEscape(() => props.onClose());

  return (
    <div
      class="resource-browser-overlay"
      data-tauri-drag-region="false"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div
        class="resource-browser"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("browse.title")}
      >
        <header class="resource-browser-header">
          <span class="resource-browser-title">{i18n.t("browse.title")}</span>
          <button
            type="button"
            class="resource-browser-close"
            aria-label={i18n.t("browse.close")}
            onClick={() => props.onClose()}
          >×</button>
        </header>

        <div class="resource-browser-tabs" role="tablist" aria-label={i18n.t("browse.title")}>
          <For each={TABS}>
            {(entry) => (
              <button
                type="button"
                role="tab"
                class="resource-browser-tab"
                classList={{ active: tab() === entry.id }}
                aria-selected={tab() === entry.id}
                aria-controls="resource-browser-panel"
                data-tab={entry.id}
                onClick={() => setTab(entry.id)}
              >
                {i18n.t(entry.key)}
              </button>
            )}
          </For>
        </div>

        <div
          class="resource-browser-body"
          id="resource-browser-panel"
          role="tabpanel"
          data-panel={tab()}
        >
          <Switch>
            <Match when={tab() === "plugins"}>
              <PluginStore />
            </Match>
            <Match when={tab() === "samples"}>
              <SampleMarketPanel />
            </Match>
            <Match when={tab() === "effects"}>
              <EffectsTab />
            </Match>
          </Switch>
        </div>
      </div>
    </div>
  );
}

/** The built-in device and scanned plugin effect catalogue, added to the
 *  selected track. */
function EffectsTab() {
  const [devices, setDevices] = createSignal<DeviceDef[]>([]);
  const [plugins, setPlugins] = createSignal<PluginInfo[]>([]);
  const [status, setStatus] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);

  onMount(() => {
    void loadDevicesCatalog().then(setDevices);
    void scanPlugins(false)
      .then((found) => setPlugins(found.filter((plugin) => plugin.category === "Effect")))
      // A failed scan leaves the built-in devices; the plugin tab reports it.
      .catch(() => setPlugins([]));
  });

  const groups = createMemo(() => {
    const list = devices();
    return CATEGORY_ORDER.map((category: DeviceCategory) => ({
      category,
      label: categoryLabel(category),
      devices: list.filter((device) => device.category === category),
    })).filter((group) => group.devices.length > 0);
  });

  const selectedTrack = () => store.selectedTrack();
  const addDisabledReason = () =>
    selectedTrack() ? null : i18n.t("browse.effects.noTrack");

  const add = async (effectId: string, name: string) => {
    const index = store.selectedTrackIndex();
    if (index === null) {
      setStatus(i18n.t("browse.effects.noTrack"));
      return;
    }
    setBusy(effectId);
    setStatus(null);
    const error = await store.addEffect(`track:${index}`, effectId);
    setBusy(null);
    if (error) {
      setStatus(i18n.t("browse.effects.failed", { name, error }));
      return;
    }
    setStatus(i18n.t("browse.effects.added", {
      name,
      track: selectedTrack()?.name ?? "",
    }));
  };

  return (
    <div class="resource-effects">
      <p class="resource-effects-hint">{i18n.t("browse.effects.hint")}</p>
      <div class="resource-effects-list">
        <For each={groups()}>
          {(group) => (
            <section class="resource-effects-group" aria-label={group.label}>
              <div class="resource-effects-group-label">{group.label}</div>
              <For each={group.devices}>
                {(device) => (
                  <div class="resource-effects-row" data-device-id={device.id}>
                    <div class="resource-effects-row-main">
                      <span class="resource-effects-name">{device.name}</span>
                      <span class="resource-effects-description">
                        {deviceDescription(device)}
                      </span>
                    </div>
                    <button
                      type="button"
                      class="toolbar-btn text-btn resource-effects-add"
                      disabled={addDisabledReason() !== null || busy() !== null}
                      title={addDisabledReason() ?? undefined}
                      onClick={() => void add(device.id, device.name)}
                    >
                      {i18n.t("browse.effects.add")}
                    </button>
                  </div>
                )}
              </For>
            </section>
          )}
        </For>
        <Show when={plugins().length > 0}>
          <section class="resource-effects-group" aria-label={i18n.t("device.plugins")}>
            <div class="resource-effects-group-label">{i18n.t("device.plugins")}</div>
            <For each={plugins()}>
              {(plugin) => (
                <div class="resource-effects-row" data-plugin-id={plugin.id}>
                  <div class="resource-effects-row-main">
                    <span class="resource-effects-name">{plugin.name}</span>
                    <span class="resource-effects-description">
                      {plugin.vendor ? `${plugin.vendor} · ` : ""}
                      {pluginFormatLabel(plugin.format)}
                    </span>
                  </div>
                  <button
                    type="button"
                    class="toolbar-btn text-btn resource-effects-add"
                    disabled={addDisabledReason() !== null || busy() !== null}
                    title={addDisabledReason() ?? undefined}
                    onClick={() => void add(`plugin:${plugin.id}`, plugin.name)}
                  >
                    {i18n.t("plugin.addEffect")}
                  </button>
                </div>
              )}
            </For>
          </section>
        </Show>
        <Show when={devices().length === 0 && plugins().length === 0}>
          <p class="resource-effects-hint">{i18n.t("browse.effects.empty")}</p>
        </Show>
      </div>
      <Show when={status()}>
        <div class="resource-effects-status" role="status">{status()}</div>
      </Show>
    </div>
  );
}
