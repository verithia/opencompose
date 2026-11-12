/**
 * Effect chain for a track/bus/master. The add menu lists the built-in devices
 * and the scanned plugin effects; existing plugin effects keep their raw
 * parameter editor.
 */
import { For, Index, Show, createEffect, createSignal, onMount } from "solid-js";
import { store } from "../stores/project";
import { scanPlugins } from "../stores/api";
import type { Effect, EffectType, ParamValue } from "../types/project";
import type { DeviceDef } from "../types/devices";
import { FALLBACK_DEVICES, findDevice, loadDevicesCatalog } from "../devices/catalog";
import { DevicePicker, type PickerAnchor, type PickerPlugin } from "./DevicePicker";
import { DevicePanel } from "./DevicePanel";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import { openEffectContextMenu } from "./effectMenu";
import { EffectIcon } from "./effectIcons";
import { EffectAutomation } from "./EffectAutomation";
import { effectDeviceId, type DeviceId } from "../devices/effectIds";

// Compile error when a device id above has no colour.
const DEVICE_COLORS: Record<DeviceId, string> = {
  gain: "#90a4ae",
  eq: "#4fc3f7",
  "dynamic-eq": "#26a69a",
  compression: "#ffb74d",
  "de-esser": "#ffd54f",
  "breath-split": "#ffa726",
  multiband: "#ff8a65",
  leveler: "#4db6ac",
  opto: "#f06292",
  fet: "#ffb74d",
  iron: "#a1887f",
  "passive-eq": "#7e57c2",
  doubler: "#8d6e63",
  exciter: "#ffd54f",
  "ducked-send": "#4db6ac",
  resonance: "#9ccc65",
  tuner: "#80deea",
  limiter: "#ef5350",
  gate: "#b0bec5",
  reverb: "#ce93d8",
  "reverb-fdn": "#ab47bc",
  delay: "#81c784",
  chorus: "#aed581",
  saturator: "#ff7043",
  distortion: "#ff8a65",
  flanger: "#4dd0e1",
  phaser: "#f48fb1",
  stereo_width: "#9575cd",
};

export { effectDeviceId };

function effectLabel(type: EffectType, devices: DeviceDef[]): string {
  const id = effectDeviceId(type);
  if (id) return findDevice(devices, id)?.name ?? id;
  return typeof type === "string" ? type : (type.Plugin.split(".").pop() ?? type.Plugin);
}

function effectColor(type: EffectType): string {
  const id = effectDeviceId(type);
  return (id && DEVICE_COLORS[id as DeviceId]) || "#9c27b0";
}

export function EffectsRack(props: {
  target: string;
  effects: Effect[];
  inheritedEffects?: Effect[];
  label: string;
}) {
  const [showAdd, setShowAdd] = createSignal(false);
  const [anchor, setAnchor] = createSignal<PickerAnchor | null>(null);
  const [expandedSlot, setExpandedSlot] = createSignal<number | null>(null);
  const [devices, setDevices] = createSignal<DeviceDef[]>(FALLBACK_DEVICES);
  const [plugins, setPlugins] = createSignal<PickerPlugin[]>([]);
  const [addError, setAddError] = createSignal<string | null>(null);
  let addButton: HTMLButtonElement | undefined;

  onMount(() => {
    void loadDevicesCatalog().then(setDevices);
    void scanPlugins(false)
      .then((found) => setPlugins(
        found
          .filter((plugin) => plugin.category === "Effect")
          .map(({ id, name, vendor, format }) => ({ id, name, vendor, format })),
      ))
      // A failed scan leaves the built-in devices; the plugin browser reports it.
      .catch(() => setPlugins([]));
  });

  createEffect(() => {
    const slot = expandedSlot();
    if (slot !== null && slot >= props.effects.length) setExpandedSlot(null);
  });

  function togglePicker() {
    if (showAdd()) {
      setShowAdd(false);
      return;
    }
    const rect = addButton?.getBoundingClientRect();
    setAnchor({
      left: rect?.left ?? 8,
      top: rect?.top ?? 8,
      bottom: rect?.bottom ?? 8,
    });
    setShowAdd(true);
  }

  async function insert(effectType: string) {
    setShowAdd(false);
    setAddError(null);
    const before = props.effects.length;
    const error = await store.addEffect(props.target, effectType);
    if (error) {
      setAddError(i18n.t("effects.addFailed", { error }));
      return;
    }
    if (props.effects.length > before) setExpandedSlot(props.effects.length - 1);
  }

  async function addDevice(device: DeviceDef) {
    await insert(device.id);
  }

  async function addPlugin(plugin: PickerPlugin) {
    await insert(`plugin:${plugin.id}`);
  }

  /** Move one effect and keep the open panel on it, not on its old index. */
  async function moveEffectTo(from: number, to: number) {
    if (!(await store.moveEffect(props.target, from, to))) return;
    if (expandedSlot() === from) setExpandedSlot(to);
  }

  return (
    <div class="effects-rack">
      {/* Header: label + built-in device picker */}
      <div class="effects-rack-header">
        <span class="effects-rack-label">{props.label}</span>
        <button
          ref={addButton}
          class="effects-rack-add-btn"
          aria-label={i18n.t("device.add")}
          {...useTooltip(() => i18n.t("device.add"))}
          onClick={togglePicker}
        >+</button>
      </div>

      <Show when={addError()}>
        <div class="effects-rack-error selectable" role="alert" style={{
          color: "var(--warning, #e2a842)", "font-size": "10px", padding: "2px 4px",
        }}>{addError()}</div>
      </Show>

      <Show when={showAdd() && anchor()}>
        <DevicePicker
          devices={devices()}
          plugins={plugins()}
          anchor={anchor()!}
          onSelect={(device) => void addDevice(device)}
          onSelectPlugin={(plugin) => void addPlugin(plugin)}
          onClose={() => setShowAdd(false)}
        />
      </Show>

      <div class="effects-rack-slots">
        <Show when={props.inheritedEffects?.length}>
          <div class="profile-label">{i18n.t("preset.inheritedEffects")}</div>
          <Index each={props.inheritedEffects}>
            {(effect) => (
              <div class="effect-slot effect-slot-readonly" classList={{ bypassed: effect().bypass === true }}>
                <span class="effect-slot-name">{effectLabel(effect().effect_type, devices())}</span>
              </div>
            )}
          </Index>
        </Show>
        <Index each={props.effects}>
          {(effect, index) => {
            const device = () => findDevice(devices(), effectDeviceId(effect().effect_type) ?? "");
            return (
              <div>
                <div
                  class="effect-slot"
                  classList={{
                    expanded: expandedSlot() === index,
                    bypassed: effect().bypass === true,
                  }}
                  data-effect-slot={index}
                  onClick={() => setExpandedSlot(expandedSlot() === index ? null : index)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    openEffectContextMenu({
                      x: event.clientX,
                      y: event.clientY,
                      target: props.target,
                      index,
                      count: props.effects.length,
                      effect: effect(),
                      onMove: (from, to) => void moveEffectTo(from, to),
                    });
                  }}
                >
                  <div class="effect-slot-color" style={{ background: effectColor(effect().effect_type) }} />
                  <span class="effect-slot-icon" style={{ color: effectColor(effect().effect_type) }}>
                    <EffectIcon type={effect().effect_type} size={12} />
                  </span>
                  <span class="effect-slot-name">{effectLabel(effect().effect_type, devices())}</span>
                  <Show when={effect().params.length > 0}>
                    <span class="effect-slot-params">{effect().params.length}p</span>
                  </Show>
                  <button
                    class="effect-slot-remove"
                    aria-label={i18n.t("effects.remove", { name: effectLabel(effect().effect_type, devices()) })}
                    onClick={(event) => {
                      event.stopPropagation();
                      store.removeEffect(props.target, index);
                    }}
                    {...useTooltip(() => i18n.t("menu.effect.remove"))}
                  >x</button>
                </div>

                <Show when={expandedSlot() === index}>
                  <Show
                    when={device()}
                    fallback={
                      <RawEffectParams
                        target={props.target}
                        index={index}
                        effect={effect()}
                      />
                    }
                  >
                    {(resolved) => (
                      <DevicePanel
                        target={props.target}
                        index={index}
                        count={props.effects.length}
                        effect={effect()}
                        device={resolved()}
                        onMove={(from, to) => void moveEffectTo(from, to)}
                        onClose={() => setExpandedSlot(null)}
                      />
                    )}
                  </Show>
                  <Show when={device()?.ports?.some((port) => port.kind === "control")}>
                    <ControlInputRow target={props.target} index={index} />
                  </Show>
                </Show>
              </div>
            );
          }}
        </Index>

        <Show when={props.effects.length === 0}>
          <div class="effects-rack-empty">{i18n.t("effects.empty")}</div>
        </Show>
      </div>

      <EffectAutomation target={props.target} />
    </div>
  );
}

/** One compact row for a device's control input: pick a @control_source or
 * disconnect. The same edge the routing graph draws by dragging a cable. */
function ControlInputRow(props: { target: string; index: number }) {
  const [error, setError] = createSignal<string | null>(null);
  const sources = () => store.project.properties.control_sources ?? [];
  const address = () => `fx${props.index + 1}`;


  const current = (): string => {
    const routes = props.target === "master"
      ? store.project.master?.input_routes
      : props.target.startsWith("bus:")
        ? store.project.buses.find((bus) => `bus:${bus.name}` === props.target)?.input_routes
        : store.project.tracks[Number(props.target.slice(6))]?.input_routes;
    const route = (routes ?? []).find((candidate) =>
      candidate.kind === "control" && candidate.target === address()
    );
    return route?.source ?? "";
  };

  async function choose(source: string) {
    const message = source
      ? await store.addControlRoute(props.target, address(), source)
      : await store.removeControlRoute(props.target, address());
    setError(message);
  }

  return (
    <div class="effect-control-input" role="group" aria-label={i18n.t("mixer.controlInput")}>
      <span
        class="effect-control-input-label"
        {...useTooltip(() => i18n.t("routing.controlHint"))}
      >{i18n.t("mixer.controlInput")}</span>
      <select
        class="effect-control-input-select"
        aria-label={i18n.t("mixer.controlConnect")}
        value={current()}
        disabled={sources().length === 0}
        onChange={(event) => void choose(event.currentTarget.value)}
        {...useTooltip(() => sources().length === 0
          ? i18n.t("mixer.controlNoSources")
          : i18n.t("mixer.controlConnect"))}
      >
        <option value="">{i18n.t("mixer.controlNone")}</option>
        <For each={sources()}>
          {(source) => <option value={source.name}>{i18n.t("routing.controlSourceLabel", { name: source.name })}</option>}
        </For>
      </select>
      <Show when={sources().length === 0}>
        <span class="effect-control-input-hint">{i18n.t("mixer.controlNoSources")}</span>
      </Show>
      <Show when={current()}>
        <button
          class="effect-control-input-remove"
          aria-label={i18n.t("mixer.controlDisconnect")}
          onClick={() => void choose("")}
        >{i18n.t("routing.remove")}</button>
      </Show>
      <Show when={error()}>
        {(message) => (
          <span class="effect-control-input-error" role="alert">{message()}</span>
        )}
      </Show>
    </div>
  );
}

function RawEffectParams(props: { target: string; index: number; effect: Effect }) {
  const bypassed = () => props.effect.bypass === true;
  return (
    <Show when={bypassed() || props.effect.params.length > 0}>
      <div class="effect-params-panel" classList={{ bypassed: bypassed() }}>
        <Show when={bypassed()}>
          <span class="device-bypassed-badge" data-effect-bypassed>
            {i18n.t("effects.bypassed")}
          </span>
        </Show>
        <For each={props.effect.params}>
          {([name, value]) => {
            const [editing, setEditing] = createSignal(false);
            const [editVal, setEditVal] = createSignal(formatRawParamValue(value));
            return (
              <div
                class="effect-param-row"
                style={{ cursor: "pointer" }}
                onClick={() => {
                  setEditing(!editing());
                  setEditVal(formatRawParamValue(value));
                }}
              >
                <span class="effect-param-name">{name}</span>
                {editing() ? (
                  <input
                    class="effect-param-input"
                    value={editVal()}
                    onInput={(event) => setEditVal(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        store.setEffectParam(props.target, props.index, name, editVal());
                        setEditing(false);
                      }
                      if (event.key === "Escape") setEditing(false);
                    }}
                    onBlur={() => {
                      store.setEffectParam(props.target, props.index, name, editVal());
                      setEditing(false);
                    }}
                    onClick={(event) => event.stopPropagation()}
                    style={{
                      width: "80px", background: "var(--bg-input)", color: "var(--text-primary)",
                      border: "1px solid var(--border)", "border-radius": "3px",
                      padding: "1px 4px", "font-size": "11px",
                    }}
                  />
                ) : (
                  <span class="effect-param-value">{formatRawParamValue(value)}</span>
                )}
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}

function formatRawParamValue(v: ParamValue): string {
  if ("Number" in v) return v.Number.toFixed(1);
  if ("Db" in v) return `${v.Db > 0 ? "+" : ""}${v.Db.toFixed(1)}dB`;
  if ("Percentage" in v) return `${v.Percentage.toFixed(0)}%`;
  if ("Frequency" in v) return v.Frequency >= 1000 ? `${(v.Frequency / 1000).toFixed(1)}kHz` : `${v.Frequency.toFixed(0)}Hz`;
  if ("Time" in v) return v.Time >= 1 ? `${v.Time.toFixed(2)}s` : `${(v.Time * 1000).toFixed(0)}ms`;
  if ("Cents" in v) return `${v.Cents.toFixed(0)}ct`;
  if ("String" in v) return v.String;
  if ("Bool" in v) return v.Bool ? "on" : "off";
  if ("Array" in v) return v.Array.join(", ");
  return "?";
}
