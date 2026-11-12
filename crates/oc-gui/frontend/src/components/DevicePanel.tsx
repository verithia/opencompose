import { createParameterGesture } from "../utils/parameterGesture";
import { withParameterTarget, type ParameterRequestOwner } from "../stores/parameterAdmission";
/**
 * Generic, schema-driven device panel with special EQ, dynamics, and time
 * views layered on top. Every edit goes through the shared effect-param store.
 */
import { For, Match, Show, Switch, createEffect, createSignal, on, onCleanup, onMount, untrack } from "solid-js";
import type { Effect, ParamValue, Project } from "../types/project";
import type { DeviceDef, DeviceParamDef, DevicePreset, DevicePresetParams } from "../types/devices";
import { canonicalDeviceId, findParam } from "../devices/catalog";
import { deviceDescription } from "../devices/labels";
import { Dropdown } from "./Dropdown";
import { TextPromptDialog } from "./TextPromptDialog";
import { ownedParameterSurface } from "../stores/ownedParameterGesture";
import {
  clamp01,
  clampParam,
  formatParamValue,
  normalizedToValue,
  readEffectParam,
  toParamString,
  valueToNormalized,
} from "../devices/params";
import * as api from "../stores/api";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { meterSnapshot, selectMeter, startMeterPolling } from "../stores/meters";
import {
  GR_HISTORY_CAPACITY,
  GR_HISTORY_RANGE_DB,
  GR_HISTORY_SECONDS,
  GrHistory,
} from "../utils/grHistory";
import { EqCurveEditor } from "./EqCurveEditor";
import { ParametricEqEditor } from "./ParametricEqEditor";
import { PassiveEqCurve } from "./PassiveEqCurve";
import { useTooltip } from "./Tooltip";
import {
  effectAutomationTarget,
  effectLaneTarget,
  ownerAutomationLanes,
} from "../utils/automationTarget";
import { automationPositionAt } from "../utils/automationSampler";
import {
  automationReadout,
  unwatchAutomation,
  watchAutomation,
} from "../stores/automationReadout";
import {
  automationWriteArmed,
  closeAutomationWrite,
  openAutomationWrite,
  sampleAutomationWrite,
  type AutomationWriteHandle,
  type AutomationWriteSurface,
} from "../stores/automationWrite";
import { openEffectContextMenu } from "./effectMenu";

export function DevicePanel(props: {
  target: string;
  index: number;
  /** Effects in the chain, so the moves know where the ends are. */
  count: number;
  effect: Effect;
  device: DeviceDef;
  /** Applied by the rack that owns the chain, so the open panel follows a move. */
  onMove?: (from: number, to: number) => void;
  onClose: () => void;
}) {
  const canonicalId = () => canonicalDeviceId(props.device.id);
  // The parametric editor owns every dynamic-EQ parameter.
  const genericParams = () => (canonicalId() === "dynamic-eq" ? [] : props.device.params);

  return (
    <div
      class="device-panel"
      classList={{ bypassed: props.effect.bypass === true }}
      data-device-panel
      data-device-id={props.device.id}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        openEffectContextMenu({
          x: event.clientX,
          y: event.clientY,
          target: props.target,
          index: props.index,
          count: props.count,
          effect: props.effect,
          onMove: props.onMove,
        });
      }}
    >
      <div class="device-panel-header">
        <div class="device-panel-heading">
          <span class="device-panel-title">
            {props.device.name}
          </span>
          <span class="device-panel-desc">{deviceDescription(props.device)}</span>
        </div>
        <Show when={props.effect.bypass === true}>
          <span class="device-bypassed-badge" data-device-bypassed>
            {i18n.t("effects.bypassed")}
          </span>
        </Show>
        <button
          class="device-panel-close"
          aria-label={i18n.t("device.close")}
          onClick={() => props.onClose()}
        >×</button>
      </div>

      <DevicePresetRow
        target={props.target}
        index={props.index}
        effect={props.effect}
        device={props.device}
      />

      <Show when={canonicalId() === "eq"}>
        <EqCurveEditor
          target={props.target}
          index={props.index}
          effect={props.effect}
          device={props.device}
        />
      </Show>
      <Show when={canonicalId() === "passive-eq"}>
        <PassiveEqCurve
          target={props.target}
          index={props.index}
          effect={props.effect}
          device={props.device}
        />
      </Show>
      <Show when={canonicalId() === "dynamic-eq"}>
        <ParametricEqEditor
          target={props.target}
          index={props.index}
          effect={props.effect}
          device={props.device}
        />
      </Show>
      <Show
        when={
          canonicalId() === "compression" ||
          canonicalId() === "limiter" ||
          canonicalId() === "dynamic-eq" ||
          canonicalId() === "de-esser" ||
          canonicalId() === "multiband" ||
          canonicalId() === "leveler" ||
          canonicalId() === "opto" ||
          canonicalId() === "fet" ||
          canonicalId() === "ducked-send"
        }
      >
        <GainReductionMeter
          target={props.target}
          index={props.index}
          history={canonicalId() === "limiter"}
        />
      </Show>
      <Show when={canonicalId() === "delay"}>
        <TimeViz kind="delay" effect={props.effect} device={props.device} target={props.target} index={props.index} />
      </Show>
      <Show when={canonicalId() === "reverb"}>
        <TimeViz kind="reverb" effect={props.effect} device={props.device} target={props.target} index={props.index} />
      </Show>

      <Show
        when={genericParams().length > 0}
        fallback={
          <Show when={canonicalId() !== "dynamic-eq"}>
            <div class="device-empty">{i18n.t("device.noParams")}</div>
          </Show>
        }
      >
        <div class="device-controls">
          <For each={genericParams()}>
            {(param) => (
              <ParamControl
                target={props.target}
                index={props.index}
                effect={props.effect}
                device={props.device}
                param={param}
              />
            )}
          </For>
        </div>
        <div class="device-panel-hint">{i18n.t("device.resetHint")}</div>
      </Show>
    </div>
  );
}

/** The chain one panel target names, for reading an applied value back. */
function ownerEffects(project: Project, target: string): Effect[] | null {
  if (target === "master") return project.master?.effects ?? null;
  if (target.startsWith("bus:")) {
    return project.buses.find((bus) => bus.name === target.slice(4))?.effects ?? null;
  }
  if (target.startsWith("preset:")) {
    return project.presets.find((preset) => preset.name === target.slice(7))?.effects ?? null;
  }
  if (target.startsWith("track:")) {
    return project.tracks[Number(target.slice(6))]?.effects ?? null;
  }
  return null;
}

/** A comparable key for one effect's parameter list. */
function paramKey(params: [string, ParamValue][]): Map<string, string> {
  return new Map(params.map(([name, value]) => [name, JSON.stringify(value)]));
}

/**
 * Derived views of another control that a preset must not pin: the device
 * stores `space`, `brightness`, `character` and `distance`; `decay`,
 * `damping`, `modulation` and `early_late` are alternate readings of them.
 * Pinning both sides would let a saved preset carry two conflicting values.
 */
const PRESET_DERIVED_CONTROLS: Record<string, string[]> = {
  "reverb-fdn": ["decay", "damping", "modulation", "early_late"],
};

/** What a saved preset carries: every owned control of the device, in its own unit. */
function presetParams(device: DeviceDef, effect: Effect): DevicePresetParams {
  const params: DevicePresetParams = {};
  const derived = PRESET_DERIVED_CONTROLS[canonicalDeviceId(device.id)] ?? [];
  for (const def of device.params) {
    if (derived.includes(def.id)) continue;
    const value = readEffectParam(effect, device.id, def);
    if (def.kind === "bool") params[def.id] = value >= 0.5;
    else if (def.kind === "enum") params[def.id] = toParamString(def, value);
    else params[def.id] = value;
  }
  return params;
}

/**
 * The preset row above the controls. It lists the factory and user presets for
 * this device, applies one as a single undoable edit, saves the current values
 * under a name, and deletes a user preset after a confirmation. A refusal keeps
 * the previous selection and shows the backend's sentence.
 */
function DevicePresetRow(props: {
  target: string;
  index: number;
  effect: Effect;
  device: DeviceDef;
}) {
  const deviceId = () => canonicalDeviceId(props.device.id);
  const [presets, setPresets] = createSignal<DevicePreset[]>([]);
  const [warnings, setWarnings] = createSignal<string[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [pending, setPending] = createSignal<"apply" | "save" | "delete" | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal("");
  const [applied, setApplied] = createSignal<Map<string, string> | null>(null);
  const [promptOpen, setPromptOpen] = createSignal(false);
  const [confirmOpen, setConfirmOpen] = createSignal(false);

  const mine = () => presets().filter((preset) => preset.device === deviceId());
  const selectedPreset = () => mine().find((preset) => preset.id === selected());
  const busy = () => pending() !== null;

  /** True when the effect no longer matches the preset that was loaded. */
  const edited = () => {
    const base = applied();
    if (!base || selected() === "") return false;
    const current = paramKey(props.effect.params);
    if (current.size !== base.size) return true;
    for (const [name, value] of base) {
      if (current.get(name) !== value) return true;
    }
    return false;
  };

  const options = () => [
    { value: "", label: i18n.t("device.presets.choose") },
    ...mine().map((preset) => ({
      value: preset.id,
      label: preset.id === selected() && edited()
        ? i18n.t("device.presets.edited", { name: preset.name })
        : preset.name,
      note: preset.shadows
        ? i18n.t("device.presets.shadowsFactory")
        : preset.source === "factory"
          ? i18n.t("device.presets.sourceFactory")
          : i18n.t("device.presets.sourceUser"),
    })),
  ];

  const pickerReason = () => {
    if (busy()) return i18n.t("device.presets.busy");
    if (loading()) return i18n.t("device.presets.loading");
    return undefined;
  };
  const saveReason = () => (busy()
    ? i18n.t("device.presets.busy")
    : loading() ? i18n.t("device.presets.loading") : undefined);
  const deleteReason = () => {
    if (busy()) return i18n.t("device.presets.busy");
    if (loading()) return i18n.t("device.presets.loading");
    const preset = selectedPreset();
    if (!preset) return i18n.t("device.presets.chooseFirst");
    if (!preset.deletable) return i18n.t("device.presets.factoryLocked");
    return undefined;
  };
  const pendingLabel = () => {
    switch (pending()) {
      case "save": return i18n.t("device.presets.saving");
      case "delete": return i18n.t("device.presets.deleting");
      case "apply": return i18n.t("device.presets.applying");
      default: return "";
    }
  };

  async function refresh() {
    setLoading(true);
    try {
      const listing = await api.devicePresets();
      // A frontend bundle from a different build can answer with another shape.
      if (!Array.isArray(listing?.presets)) {
        setFailure(i18n.t("device.presets.unreadable"));
        return;
      }
      setPresets(listing.presets);
      setWarnings(Array.isArray(listing.warnings) ? listing.warnings : []);
    } catch (error) {
      setFailure(String(error));
    } finally {
      setLoading(false);
    }
  }

  function accept(listing: { presets: DevicePreset[]; warnings: string[] }) {
    setPresets(listing.presets);
    setWarnings(listing.warnings);
  }

  async function apply(id: string) {
    if (id === "" || busy()) return;
    setPending("apply");
    setFailure(null);
    try {
      const snap = await store.applyDevicePreset(id, props.target, props.index, deviceId());
      const effect = ownerEffects(snap.project, props.target)?.[props.index];
      setApplied(effect ? paramKey(effect.params) : null);
      setSelected(id);
    } catch (error) {
      setFailure(String(error));
    } finally {
      setPending(null);
    }
  }

  async function save(name: string) {
    setPromptOpen(false);
    setPending("save");
    setFailure(null);
    try {
      const current = mine().find(
        (preset) => preset.source === "user" && preset.name.toLowerCase() === name.toLowerCase(),
      );
      const listing = await api.saveDevicePreset(
        deviceId(),
        name,
        presetParams(props.device, props.effect),
        current !== undefined,
      );
      accept(listing);
      const saved = listing.presets.find(
        (preset) => preset.device === deviceId()
          && preset.source === "user"
          && preset.name.toLowerCase() === name.toLowerCase(),
      );
      if (saved) {
        setSelected(saved.id);
        setApplied(paramKey(props.effect.params));
      }
    } catch (error) {
      setFailure(String(error));
    } finally {
      setPending(null);
    }
  }

  async function remove(id: string) {
    setConfirmOpen(false);
    setPending("delete");
    setFailure(null);
    try {
      accept(await api.deleteDevicePreset(id));
      if (selected() === id) {
        setSelected("");
        setApplied(null);
      }
    } catch (error) {
      setFailure(String(error));
    } finally {
      setPending(null);
    }
  }

  onMount(() => {
    void refresh();
  });

  // A different device in the same slot owns a different library.
  createEffect(on(deviceId, () => {
    setSelected("");
    setApplied(null);
    setFailure(null);
  }));

  return (
    <div class="device-preset-row" data-device-presets data-preset-loading={loading() ? "true" : "false"}>
      <div class="device-section-title">{i18n.t("device.presets.section")}</div>
      <div class="device-preset-controls">
        <Show
          when={!loading() && mine().length === 0}
          fallback={
            <Dropdown
              label={i18n.t("device.presets.label")}
              hideLabel
              class="device-preset-picker"
              value={selected()}
              options={options()}
              disabled={busy() || loading()}
              triggerAttrs={{ "data-preset-picker": "true" }}
              triggerProps={{ title: pickerReason() }}
              onSelect={(value) => void apply(String(value))}
            />
          }
        >
          <span class="device-preset-empty" data-preset-empty>
            {i18n.t("device.presets.empty")}
          </span>
        </Show>
        <button
          type="button"
          class="device-preset-btn"
          data-preset-save
          disabled={busy() || loading()}
          title={saveReason()}
          onClick={() => setPromptOpen(true)}
        >{i18n.t("device.presets.save")}</button>
        <button
          type="button"
          class="device-preset-btn device-preset-danger"
          data-preset-delete
          disabled={deleteReason() !== undefined}
          title={deleteReason()}
          onClick={() => setConfirmOpen(true)}
        >{i18n.t("device.presets.delete")}</button>
      </div>
      {/* Authored data, like a preset name: shown as written, never translated. */}
      <Show when={selectedPreset()?.intent}>
        {(intent) => (
          <div class="device-preset-intent" data-preset-intent>{intent()}</div>
        )}
      </Show>
      <Show when={busy()}>
        <span class="device-preset-status" data-preset-pending>{pendingLabel()}</span>
      </Show>
      <Show when={failure()}>
        {(message) => (
          <div class="device-preset-error" role="alert" data-preset-error>
            {i18n.t("device.presets.failed", { message: message() })}
          </div>
        )}
      </Show>
      <Show when={warnings().length > 0}>
        <div class="device-preset-warning" role="note" data-preset-warning>
          {warnings().map((warning) => i18n.t("device.presets.warning", { message: warning })).join(" ")}
        </div>
      </Show>
      <Show when={promptOpen()}>
        <TextPromptDialog
          title={i18n.t("device.presets.saveTitle")}
          initial=""
          onCancel={() => setPromptOpen(false)}
          onConfirm={(name) => void save(name)}
        />
      </Show>
      <Show when={confirmOpen()}>
        <div class="device-preset-confirm" onClick={() => setConfirmOpen(false)}>
          <div
            class="device-preset-confirm-panel"
            role="alertdialog"
            aria-modal="true"
            aria-label={i18n.t("device.presets.deleteTitle", { name: selectedPreset()?.name ?? "" })}
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setConfirmOpen(false);
              }
            }}
          >
            <div class="device-preset-confirm-title">
              {i18n.t("device.presets.deleteTitle", { name: selectedPreset()?.name ?? "" })}
            </div>
            <p class="device-preset-confirm-body">{i18n.t("device.presets.deleteBody")}</p>
            <div class="device-preset-confirm-actions">
              <button
                type="button"
                class="device-preset-btn"
                data-preset-cancel
                onClick={() => setConfirmOpen(false)}
              >{i18n.t("device.presets.cancel")}</button>
              <button
                type="button"
                class="device-preset-btn device-preset-danger"
                data-preset-confirm
                onClick={() => void remove(selected())}
              >{i18n.t("device.presets.deleteConfirm")}</button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}

function ParamControl(props: {
  target: string;
  index: number;
  effect: Effect;
  device: DeviceDef;
  param: DeviceParamDef;
}) {
  const [draft, setDraft] = createSignal<number | null>(null);
  /** True while a held touch owns this parameter instead of its lane. */
  const [touchOverride, setTouchOverride] = createSignal(false);
  /** The write gesture this control opened, or null when none is running. */
  interface OwnedWrite { surface: AutomationWriteSurface; handle: AutomationWriteHandle }
  let writing: OwnedWrite | null = null;
  let activeKnob: (() => void) | null = null;
  let knobGeneration = 0;
  let disposed = false;

  function captureTarget(): ParameterRequestOwner {
    const ownsDocument = store.parameterRequestOwner();
    const target = props.target;
    const index = props.index;
    const parameter = props.param.id;
    const type = JSON.stringify(props.effect.effect_type);
    return withParameterTarget(ownsDocument, () => !disposed && props.target === target && props.index === index
      && props.param.id === parameter && JSON.stringify(props.effect.effect_type) === type);
  }

  /** Bar numerator a sampled quarter is read against. */
  const writeNumerator = () => Math.max(1, store.project.properties.time_signature.numerator);

  /** What an armed drag on this parameter writes, or why it cannot write. */
  const writeTarget = () => effectAutomationTarget(props.target, props.device, props.index, props.param);

  /** The write surface for this control; null when it has no automation lane. */
  const writeSurface = (): AutomationWriteSurface | null => {
    const target = writeTarget();
    if (!target.available) return null;
    const param = props.param;
    const ownerTarget = props.target;
    const ownerIndex = props.index;
    const ownerType = JSON.stringify(props.effect.effect_type);
    const ownsDocument = store.parameterRequestOwner();
    return ownedParameterSurface({
      key: `effect\u0000${ownerTarget}\u0000${ownerIndex}\u0000${param.id}`,
      label: target.label,
      value: () => value(),
      position: (quarter) => automationPositionAt(quarter, writeNumerator()),
      isCurrent: () => !disposed && ownsDocument() && props.target === ownerTarget
        && props.index === ownerIndex && props.param.id === param.id
        && JSON.stringify(props.effect.effect_type) === ownerType,
      expectedStamp: ownsDocument.expectedStamp,
      begin: stamp => api.beginGuardedEffectParamWrite(ownerTarget, ownerIndex, param.id, stamp),
      point: (token, position, next) => api.updateOwnedEffectParamWrite(
        token, ownerTarget, ownerIndex, param.id, position,
        param.kind === "bool" || param.kind === "enum" ? String(Math.round(next)) : toParamString(param, next),
      ),
      end: token => api.endOwnedEffectParamWrite(token, ownerTarget, ownerIndex, param.id),
      apply: result => store.applyAiSnapshot(result.snapshot),
    });
  };

  /**
   * Start an armed write. A parameter with no automation range keeps the
   * ordinary edit and shows the device schema's reason instead.
   */
  function captureWrite(owns = captureTarget()): () => OwnedWrite | null {
    const armed = automationWriteArmed();
    const surface = armed ? writeSurface() : null;
    const target = armed && !surface ? writeTarget() : null;
    return () => {
      if (!owns()) return null;
      if (writing) endWrite(writing, false);
      if (!owns() || !armed) return null;
      if (!surface) {
        if (target && !target.available) store.setError(target.reason);
        return null;
      }
      const handle = openAutomationWrite(surface);
      if (!owns()) {
        if (handle) void closeAutomationWrite(handle);
        return null;
      }
      writing = handle ? { surface, handle } : null;
      return writing;
    };
  }
  function beginWrite(): OwnedWrite | null {
    return captureWrite()();
  }

  /** Finish only the interaction captured by this callback, never its successor. */
  function endWrite(owner: OwnedWrite, sample = true): void {
    if (sample) sampleAutomationWrite(owner.handle, store.transport().position_beats);
    void closeAutomationWrite(owner.handle);
    if (writing === owner) {
      writing = null;
      setDraft(null);
    }
  }

  /** The lane on this owner that drives this parameter, if one targets it. */
  const lane = (): string | null => {
    const address = `fx${props.index + 1}`;
    const type = canonicalDeviceId(props.device.id).replaceAll("-", "_");
    const found = ownerAutomationLanes(store.project, props.target).find((entry) => {
      const parts = effectLaneTarget(entry.param);
      if (!parts || parts.parameter !== props.param.id) return false;
      return parts.target === address || parts.target === type;
    });
    return found?.param ?? null;
  };
  createEffect(() => {
    const watched = lane();
    if (!watched) return;
    watchAutomation(props.target, watched, untrack(() => store.transport().position_beats));
    onCleanup(() => unwatchAutomation(props.target, watched));
  });
  const answer = () => {
    const watched = lane();
    return watched ? automationReadout(props.target, watched) ?? null : null;
  };
  /** The driven value, while no drag or held touch owns the parameter. */
  const driving = () => {
    if (draft() !== null || touchOverride()) return null;
    const current = answer();
    return current?.status === "driven" ? current : null;
  };
  const laneReason = () => {
    if (!lane() || draft() !== null || touchOverride()) return null;
    const current = answer();
    return current?.status === "not_driven" ? current.reason : null;
  };
  const value = () => {
    const current = draft();
    if (current !== null) return current;
    const live = driving();
    if (live) {
      return live.domain === "normalized"
        ? normalizedToValue(props.param, live.value)
        : live.value;
    }
    return readEffectParam(props.effect, props.device.id, props.param);
  };
  const readout = () => formatParamValue(props.param, value());
  const slider = () => props.param.unit === "Hz" || props.param.unit === "ms";

  async function commit(next: number, ordinary = false, isCurrent = captureTarget()) {
    if (!isCurrent()) return;
    const clamped = clampParam(props.param, next);
    setDraft(clamped);
    if (!ordinary && automationWriteArmed()) {
      const surface = writeSurface();
      const handle = surface ? openAutomationWrite(surface) : null;
      if (handle) {
        sampleAutomationWrite(handle, store.transport().position_beats);
        await closeAutomationWrite(handle);
        if (isCurrent()) setDraft(null);
        return;
      }
    }
    try {
      await store.setEffectParam(
        props.target,
        props.index,
        props.param.id,
        toParamString(props.param, clamped),
        isCurrent,
      );
    } finally {
      if (isCurrent()) setDraft(null);
    }
  }

  function reset() {
    void commit(props.param.default);
  }

  function onKeyDown(event: KeyboardEvent) {
    const current = valueToNormalized(props.param, value());
    const step = event.shiftKey ? 0.1 : 0.01;
    let next: number | null = null;
    switch (event.key) {
      case "ArrowUp":
      case "ArrowRight":
        next = normalizedToValue(props.param, clamp01(current + step));
        break;
      case "ArrowDown":
      case "ArrowLeft":
        next = normalizedToValue(props.param, clamp01(current - step));
        break;
      case "PageUp":
        next = normalizedToValue(props.param, clamp01(current + 0.1));
        break;
      case "PageDown":
        next = normalizedToValue(props.param, clamp01(current - 0.1));
        break;
      case "Home":
        next = props.param.min;
        break;
      case "End":
        next = props.param.max;
        break;
      default:
        return;
    }
    event.preventDefault();
    void commit(next);
  }

  /**
   * A live touch on this parameter. The backend decides whether a lane owns it;
   * until it accepts, the control keeps its ordinary edit path.
   */
  interface TouchSession {
    /** True when the backend granted the touch; resolved even after an end. */
    accepted: Promise<boolean>;
    update(value: number): void;
    end(): void;
    isEngaged(): boolean;
    onRetired(cancel: () => void): void;
  }

  let latestTouch: object | null = null;
  function captureTouch(initial: number, owns: ParameterRequestOwner): () => TouchSession {
    const target = props.target;
    const index = props.index;
    const parameter = props.param.id;
    return () => {
      const identity = {};
      let engaged = false;
      let ended = false;
      let token: api.EffectTouchToken | null = null;
      let timer: number | undefined;
      let pending: number | null = null;
      let retired = false;
      let retiredCallback: (() => void) | null = null;
      const current = () => !ended && latestTouch === identity && owns();
      const stop = () => {
        ended = true;
        if (timer !== undefined) window.clearTimeout(timer);
        timer = undefined;
        pending = null;
        const closing = token;
        token = null;
        if (closing) void api.endOwnedEffectParamTouch(closing, target, index, parameter).catch(() => {});
        if (latestTouch === identity) {
          latestTouch = null;
          setTouchOverride(false);
        }
      };
      const retire = () => {
        const notify = !ended;
        stop();
        if (notify) {
          retired = true;
          retiredCallback?.();
        }
      };
      const flush = () => {
        timer = undefined;
        const next = pending;
        pending = null;
        if (!current()) { retire(); return; }
        const owner = token;
        if (next === null || !owner) return;
        void api.updateOwnedEffectParamTouch(owner, target, index, parameter, next)
          .then(updated => { if (!updated) retire(); }).catch(retire);
      };
      const send = (next: number) => {
        if (!current()) { retire(); return; }
        pending = next;
        if (timer === undefined) timer = window.setTimeout(flush, 16);
      };
      let begin: Promise<api.EffectTouchToken | null>;
      try {
        if (!owns()) begin = Promise.resolve(null);
        else {
          latestTouch = identity;
          begin = api.beginGuardedEffectParamTouch(target, index, parameter, initial, owns.requireStamp());
        }
      } catch { begin = Promise.resolve(null); }
      const accepted = begin.then(granted => {
        if (!granted) {
          if (!owns()) retire();
          return false;
        }
        token = granted;
        engaged = true;
        if (!current()) { retire(); return true; }
        setTouchOverride(true);
        if (!current()) { retire(); return true; }
        const next = draft();
        if (next !== null) send(next);
        return true;
      }).catch(() => { retire(); return false; });
      return {
        accepted,
        update(next) { if (engaged) send(next); },
        end: stop,
        isEngaged: () => engaged,
        onRetired(cancel) { retiredCallback = cancel; if (retired) cancel(); },
      };
    };
  }

  function beginDrag(event: MouseEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    const ownsTarget = captureTarget();
    const openWrite = captureWrite(ownsTarget);
    const openTouch = captureTouch(value(), ownsTarget);
    activeKnob?.();
    if (!ownsTarget()) return;
    const generation = ++knobGeneration;
    const startY = event.clientY;
    const start = valueToNormalized(props.param, value());
    let moved = false;
    let ended = false;
    let canceled = false;
    const write = openWrite();
    if (!ownsTarget()) return;
    const touch = openTouch();
    if (!ownsTarget()) {
      touch.end();
      if (write) endWrite(write, false);
      return;
    }
    let beginDone = false;

    const commitDraft = () => {
      const next = draft();
      if (!canceled && moved && next !== null && ownsTarget()) void commit(next, true, ownsTarget);
    };
    void touch.accepted.then((accepted) => {
      if (generation !== knobGeneration) return;
      beginDone = true;
      if (accepted) {
        const current = draft();
        if (current !== null) touch.update(current);
        if (ended) setDraft(null);
      } else if (ended) {
        // No lane owns the parameter (or nothing is streaming).
        commitDraft();
      }
    });

    const move = (moveEvent: MouseEvent) => {
      if (ended) return;
      if (!ownsTarget()) { cancel(); return; }
      moved = true;
      const delta = (startY - moveEvent.clientY) / 140;
      const next = normalizedToValue(props.param, clamp01(start + delta));
      setDraft(next);
      touch.update(next);
      if (write) sampleAutomationWrite(write.handle, store.transport().position_beats);
    };
    function stopDrag() {
      ended = true;
      if (activeKnob === cancel) activeKnob = null;
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      window.removeEventListener("blur", cancel);
      document.removeEventListener("pointercancel", cancel);
    }
    function up() {
      if (ended) return;
      stopDrag();
      const engaged = touch.isEngaged();
      touch.end();
      if (write) {
        // The write owns the value: one sample at the playhead, one undo entry.
        endWrite(write);
        return;
      }
      if (!beginDone) return; // the pending begin decides
      if (engaged) {
        setDraft(null);
        return;
      }
      commitDraft();
    }
    function cancel() {
      if (ended) return;
      canceled = true;
      stopDrag();
      touch.end();
      if (write) endWrite(write, false);
      else setDraft(null);
    }
    activeKnob = cancel;
    touch.onRetired(cancel);
    if (ended) return;
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
    // A lost pointer or a window that loses focus must not leave a touch or a
    // draft behind; the release path above already closes both.
    window.addEventListener("blur", cancel);
    document.addEventListener("pointercancel", cancel);
  }

  interface SliderInteraction {
    id: number;
    write: OwnedWrite | null;
    touch: TouchSession;
    value: number;
    changed: boolean;
    ended: boolean;
    owns: ParameterRequestOwner;
    removeListeners(): void;
  }
  let sliderInteraction: SliderInteraction | null = null;
  let latestSlider: SliderInteraction | null = null;
  let sliderPointerSequence = false;
  let sliderRetiredSequence = false;
  let sliderInputOwner: ParameterRequestOwner | null = null;
  function finishSlider(owner: SliderInteraction, canceled = false): void {
    if (owner.ended) return;
    owner.ended = true;
    owner.removeListeners();
    const current = sliderInteraction === owner;
    if (current) sliderInteraction = null;
    owner.touch.end();
    if (owner.write) { endWrite(owner.write, !canceled); return; }
    if (canceled || !owner.changed) { if (current) setDraft(null); return; }
    void owner.touch.accepted.then((accepted) => {
      if (!current || latestSlider !== owner || sliderInteraction || !owner.owns()) return;
      if (accepted) setDraft(null);
      else void commit(owner.value, true, owner.owns);
    });
  }
  function beginSlider(event: PointerEvent): void {
    if ((event.button ?? 0) !== 0) return;
    const owns = captureTarget();
    const openWrite = captureWrite(owns);
    const openTouch = captureTouch(value(), owns);
    if (sliderInteraction) finishSlider(sliderInteraction, true);
    if (!owns()) return;
    sliderInputOwner = null;
    sliderPointerSequence = true;
    sliderRetiredSequence = false;
    const write = openWrite();
    if (!owns()) return;
    const touch = openTouch();
    if (!owns()) {
      touch.end();
      if (write) endWrite(write, false);
      return;
    }
    const owner: SliderInteraction = {
      id: event.pointerId ?? 0,
      owns,
      write,
      touch,
      value: value(), changed: false, ended: false, removeListeners: () => {},
    };
    sliderInteraction = owner;
    latestSlider = owner;
    const up = (release: PointerEvent) => {
      if ((release.pointerId ?? 0) === owner.id) finishSlider(owner);
    };
    const cancel = (release: PointerEvent) => {
      if ((release.pointerId ?? 0) === owner.id) finishSlider(owner, true);
    };
    const blur = () => finishSlider(owner, true);
    owner.removeListeners = () => {
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", cancel, true);
      window.removeEventListener("blur", blur);
    };
    touch.onRetired(() => {
      if (sliderInteraction === owner) sliderRetiredSequence = true;
      finishSlider(owner, true);
    });
    if (owner.ended) return;
    window.addEventListener("pointerup", up, true);
    window.addEventListener("pointercancel", cancel, true);
    window.addEventListener("blur", blur);
  }
  onCleanup(() => {
    disposed = true;
    activeKnob?.();
    if (sliderInteraction) finishSlider(sliderInteraction, true);
    if (writing) endWrite(writing, false);
  });

  interface ToggleIntent {
    owner: ParameterRequestOwner;
    next: number;
    pointer: number;
    removeListeners(): void;
  }
  let togglePointer: ToggleIntent | null = null;
  let toggleSpace: { owner: ParameterRequestOwner; next: number } | null = null;
  let suppressPhysicalToggleClick = false;
  const cancelTogglePointer = (intent: ToggleIntent) => {
    if (togglePointer !== intent) return;
    togglePointer = null;
    intent.removeListeners();
    suppressPhysicalToggleClick = true;
  };
  const beginTogglePointer = (event: PointerEvent) => {
    if ((event.button ?? 0) !== 0) return;
    const owner = captureTarget();
    if (togglePointer) cancelTogglePointer(togglePointer);
    toggleSpace = null;
    suppressPhysicalToggleClick = false;
    const button = event.currentTarget as HTMLElement;
    const intent: ToggleIntent = { owner, next: value() >= 0.5 ? 0 : 1,
      pointer: event.pointerId ?? 0, removeListeners: () => {} };
    togglePointer = intent;
    const cancel = (release: PointerEvent) => {
      if ((release.pointerId ?? 0) === intent.pointer) cancelTogglePointer(intent);
    };
    const up = (release: PointerEvent) => {
      if ((release.pointerId ?? 0) !== intent.pointer) return;
      if (!(release.target instanceof Node) || !button.contains(release.target)) cancelTogglePointer(intent);
    };
    const blur = () => cancelTogglePointer(intent);
    intent.removeListeners = () => {
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", cancel, true);
      window.removeEventListener("blur", blur);
    };
    window.addEventListener("pointerup", up, true);
    window.addEventListener("pointercancel", cancel, true);
    window.addEventListener("blur", blur);
  };
  const toggleClick = (event: MouseEvent) => {
    if (suppressPhysicalToggleClick && event.detail > 0) { event.preventDefault(); return; }
    const intent = togglePointer;
    if (intent) {
      const pointer = (event as PointerEvent).pointerId;
      if (typeof pointer === "number" && pointer !== intent.pointer) return;
      togglePointer = null;
      intent.removeListeners();
      void commit(intent.next, false, intent.owner);
    } else {
      // Keyboard activation is owned below and prevents its native synthetic
      // click. A standalone detail-zero activation can therefore remain AX input.
      const owner = captureTarget();
      void commit(value() >= 0.5 ? 0 : 1, false, owner);
    }
  };
  const toggleKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault(); event.stopPropagation();
      toggleSpace = null;
      if (togglePointer) cancelTogglePointer(togglePointer);
      return;
    }
    if (event.key !== " " && event.key !== "Enter") return;
    event.preventDefault(); event.stopPropagation();
    if (event.repeat) return;
    const owner = captureTarget();
    if (togglePointer) cancelTogglePointer(togglePointer);
    const intent = { owner, next: value() >= 0.5 ? 0 : 1 };
    if (event.key === " ") toggleSpace = intent;
    else { toggleSpace = null; void commit(intent.next, false, intent.owner); }
  };
  const toggleKeyUp = (event: KeyboardEvent) => {
    if (event.key !== " " && event.key !== "Enter") return;
    event.preventDefault(); event.stopPropagation();
    if (event.key !== " ") return;
    const intent = toggleSpace;
    toggleSpace = null;
    if (intent) void commit(intent.next, false, intent.owner);
  };
  let selectionOwner: ParameterRequestOwner | null = null;
  onCleanup(() => {
    if (togglePointer) cancelTogglePointer(togglePointer);
    toggleSpace = null;
    selectionOwner = null;
  });

  const kind = () => props.param.kind ?? "continuous";
  const isToggle = () => kind() === "bool";
  const isSelect = () => kind() === "enum";

  return (
    <div
      class="device-control"
      classList={{
        slider: kind() === "continuous" && slider(),
        toggle: isToggle(),
        select: isSelect(),
        driven: driving() !== null,
      }}
      data-param-row={props.param.id}
    >
      <span
        class="device-control-name"
        {...useTooltip(() => {
          const reason = laneReason();
          return reason ? `${props.param.name} — ${reason}` : props.param.name;
        })}
      >{props.param.name}</span>
      <Show when={driving()}>
        <span
          class="device-driven-badge"
          data-driven-badge="true"
          aria-label={i18n.t("automation.drivenBadge")}
          title={i18n.t("automation.drivenTitle")}
        >A</span>
      </Show>
      <Switch
        fallback={
          <div
            class="device-knob"
            classList={{ "touch-override": touchOverride() }}
            data-touch-override={touchOverride() ? "true" : undefined}
            role="slider"
            tabIndex={0}
            data-param-id={props.param.id}
            data-control-kind="knob"
            aria-label={`${props.device.name} ${props.param.name}`}
            aria-valuemin={props.param.min}
            aria-valuemax={props.param.max}
            aria-valuenow={value()}
            aria-valuetext={readout()}
            onMouseDown={beginDrag}
            onKeyDown={onKeyDown}
            onDblClick={reset}
          >
            <svg viewBox="0 0 48 48" aria-hidden="true">
              <path class="device-knob-track" d={knobArc(1)} />
              <path
                class="device-knob-value"
                d={knobArc(valueToNormalized(props.param, value()))}
              />
              <line
                class="device-knob-pointer"
                x1="24"
                y1="24"
                x2={knobPoint(valueToNormalized(props.param, value()), 13)[0]}
                y2={knobPoint(valueToNormalized(props.param, value()), 13)[1]}
              />
            </svg>
          </div>
        }
      >
        <Match when={isToggle()}>
          <button
            type="button"
            class="device-toggle"
            classList={{ on: value() >= 0.5 }}
            role="switch"
            aria-checked={value() >= 0.5}
            aria-label={`${props.device.name} ${props.param.name}`}
            data-param-id={props.param.id}
            data-control-kind="toggle"
            onPointerDown={beginTogglePointer}
            onKeyDown={toggleKeyDown}
            onKeyUp={toggleKeyUp}
            onBlur={() => {
              toggleSpace = null;
              if (togglePointer) cancelTogglePointer(togglePointer);
            }}
            onClick={toggleClick}
            onDblClick={reset}
          >
            <span class="device-toggle-thumb" />
          </button>
        </Match>
        <Match when={isSelect()}>
          <Dropdown
            label={`${props.device.name} ${props.param.name}`}
            hideLabel
            class="device-select"
            triggerAttrs={{ "data-param-id": props.param.id, "data-control-kind": "select" }}
            value={Math.round(value())}
            options={(props.param.options ?? []).map((option, index) => ({ value: index, label: option }))}
            onOpen={() => { selectionOwner = captureTarget(); }}
            onClose={() => { selectionOwner = null; }}
            onSelect={(next) => {
              const owner = selectionOwner;
              if (owner) void commit(next, false, owner);
            }}
            onDblClick={reset}
          />
        </Match>
        <Match when={slider()}>
          <input
            class="device-slider"
            classList={{ "touch-override": touchOverride() }}
            data-touch-override={touchOverride() ? "true" : undefined}
            type="range"
            min="0"
            max="1"
            step="0.001"
            value={valueToNormalized(props.param, value())}
            data-param-id={props.param.id}
            data-control-kind="slider"
            aria-label={`${props.device.name} ${props.param.name}`}
            aria-valuetext={readout()}
            onPointerDown={beginSlider}
            onInput={(event) => {
              if (disposed) return;
              if (sliderRetiredSequence) {
                event.currentTarget.value = String(valueToNormalized(props.param, value()));
                return;
              }
              if (sliderInteraction && !sliderInteraction.owns()) {
                sliderRetiredSequence = true;
                finishSlider(sliderInteraction, true);
                event.currentTarget.value = String(valueToNormalized(props.param, value()));
                return;
              }
              // Fresh input is an explicit accessibility edit; a trailing
              // change without input remains tied to the ended pointer stream.
              if (sliderPointerSequence && !sliderInteraction) sliderPointerSequence = false;
              if (!sliderInteraction) {
                sliderInputOwner ??= captureTarget();
                if (!sliderInputOwner()) {
                  setDraft(null);
                  event.currentTarget.value = String(valueToNormalized(props.param, value()));
                  return;
                }
              }
              const next = normalizedToValue(props.param, Number(event.currentTarget.value));
              setDraft(next);
              if (sliderInteraction) {
                sliderInteraction.value = next;
                sliderInteraction.changed = true;
                sliderInteraction.touch.update(next);
                if (sliderInteraction.write) sampleAutomationWrite(sliderInteraction.write.handle, store.transport().position_beats);
              } else if (automationWriteArmed()) {
                const owner = writing ?? beginWrite();
                if (owner) sampleAutomationWrite(owner.handle, store.transport().position_beats);
              }
            }}
            onChange={(event) => {
              if (disposed) return;
              if (sliderRetiredSequence) {
                sliderRetiredSequence = false;
                event.currentTarget.value = String(valueToNormalized(props.param, value()));
                return;
              }
              if (sliderPointerSequence) return;
              const next = normalizedToValue(props.param, Number(event.currentTarget.value));
              const admission = sliderInputOwner ?? captureTarget();
              sliderInputOwner = null;
              const owner = writing;
              if (owner) { endWrite(owner); return; }
              void commit(next, false, admission);
            }}
            onKeyDown={(event) => {
              if (disposed) return;
              sliderRetiredSequence = false;
              if (event.key === "Escape") {
                if (sliderInteraction) finishSlider(sliderInteraction, true);
                if (writing) endWrite(writing, false);
                sliderInputOwner = null;
                sliderPointerSequence = true;
                setDraft(null);
                return;
              }
              const admission = captureTarget();
              if (sliderInteraction) finishSlider(sliderInteraction, true);
              sliderInputOwner = admission;
              sliderPointerSequence = false;
            }}
            onPointerCancel={(event) => {
              sliderInputOwner = null;
              setDraft(null);
              if (sliderInteraction && (event.pointerId ?? 0) === sliderInteraction.id) finishSlider(sliderInteraction, true);
              else if (!sliderPointerSequence && writing) endWrite(writing, false);
              sliderPointerSequence = true;
            }}
            onBlur={() => {
              sliderInputOwner = null;
              setDraft(null);
              if (sliderInteraction) finishSlider(sliderInteraction, true);
              const owner = writing;
              if (owner) endWrite(owner, false);
              sliderPointerSequence = true;
            }}
            onDblClick={reset}
          />
        </Match>
      </Switch>
      <span class="device-readout" data-readout-param={props.param.id}>{readout()}</span>
    </div>
  );
}

function GainReductionMeter(props: { target: string; index: number; history?: boolean }) {
  onMount(() => {
    const release = startMeterPolling();
    onCleanup(release);
  });

  const meter = () => selectMeter(meterSnapshot(), props.target, props.index);
  const reduction = () => meter()?.gainReductionDb ?? 0;
  const maximum = () => meter()?.maxGainReductionDb ?? 0;
  const marginDb = () => meter()?.ceilingMarginDb ?? null;
  const width = () => `${Math.min(100, (reduction() / 24) * 100)}%`;

  const ring = new GrHistory();
  const [history, setHistory] = createSignal<number[]>([]);
  // Skip the snapshot already in the store: the first fresh poll seeds the ring.
  const initialSnapshot = meterSnapshot();
  createEffect(() => {
    const snapshot = meterSnapshot();
    if (snapshot === initialSnapshot) return;
    const row = selectMeter(snapshot, props.target, props.index);
    if (!props.history || !row?.available || !row.playing) return;
    ring.push(row.gainReductionDb / GR_HISTORY_RANGE_DB);
    setHistory(ring.values());
  });

  return (
    <div
      class="device-meter"
      data-meter="gain-reduction"
      data-meter-available={meter()?.available ? "true" : "false"}
    >
      <div class="device-section-title">{i18n.t("device.meter.gainReduction")}</div>
      <div class="device-meter-track" aria-hidden="true">
        <span class="device-meter-fill" style={{ width: width() }} />
      </div>
      <div class="device-meter-readout">
        <span class="device-meter-value" data-meter-gr>
          {`${reduction().toFixed(1)} dB`}
        </span>
        <span class="device-meter-peaks" data-meter-max>
          {`${i18n.t("device.meter.maxGainReduction")} ${maximum().toFixed(1)} dB`}
        </span>
      </div>
      <Show when={marginDb() !== null}>
        <div class="device-meter-readout" data-meter-margin-row>
          <span class="device-meter-peaks" data-meter-margin>
            {`${i18n.t("device.meter.ceilingMargin")} ${marginText(marginDb()!)}`}
          </span>
        </div>
      </Show>
      <div class="device-meter-readout">
        <span class="device-meter-peaks">
          {`${i18n.t("device.meter.input")} ${peakText(meter()?.inputPeakDb)} · `}
          {`${i18n.t("device.meter.output")} ${peakText(meter()?.outputPeakDb)}`}
        </span>
      </div>
      <Show when={props.history}>
        <GainReductionHistory samples={history()} />
      </Show>
      <div class="device-meter-note" role="note">
        {!meter()?.available
          ? i18n.t("device.meter.unavailable")
          : meter()?.playing
            ? ""
            : i18n.t("device.meter.idle")}
      </div>
    </div>
  );
}

function GainReductionHistory(props: { samples: number[] }) {
  const peakDb = () =>
    props.samples.reduce((peak, value) => Math.max(peak, value), 0) * GR_HISTORY_RANGE_DB;

  return (
    <div
      class="device-meter-history"
      data-meter-history
      data-history-samples={props.samples.length}
    >
      <div class="device-meter-history-head">
        <span class="device-section-title">{i18n.t("device.meter.grHistory")}</span>
        <span class="device-meter-peaks" data-history-window>
          {i18n.t("device.meter.grHistoryWindow", { seconds: GR_HISTORY_SECONDS })}
        </span>
      </div>
      <div
        class="device-meter-track device-meter-history-track"
        style={{ "--history-slots": String(GR_HISTORY_CAPACITY) }}
        aria-hidden="true"
      >
        <For each={props.samples}>
          {(value, index) => (
            <span
              class="device-meter-history-bar"
              data-history-bar
              style={{
                height: `${Math.round(Math.min(1, Math.max(0, value)) * 100)}%`,
                opacity: `${(0.55 + (0.45 * (index() + 1)) / props.samples.length).toFixed(2)}`,
              }}
            />
          )}
        </For>
      </div>
      <div class="device-meter-readout">
        <span class="device-meter-peaks" data-history-peak>
          {props.samples.length === 0
            ? i18n.t("device.meter.grHistoryEmpty")
            : i18n.t("device.meter.grHistoryPeak", { value: `${peakDb().toFixed(1)} dB` })}
        </span>
      </div>
    </div>
  );
}

function peakText(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return i18n.t("device.meter.silent");
  }
  const sign = value > 0 ? "+" : "";
  return `${sign}${value.toFixed(1)} dB`;
}

function marginText(value: number): string {
  if (!Number.isFinite(value)) return i18n.t("device.meter.silent");
  const sign = value > 0 ? "+" : "";
  return `${sign}${value.toFixed(1)} dB`;
}

function TimeViz(props: {
  kind: "delay" | "reverb";
  effect: Effect;
  device: DeviceDef;
  target: string;
  index: number;
}) {
  const number = (id: string, fallback: number) => {
    const def = findParam(props.device, id);
    return def ? readEffectParam(props.effect, props.device.id, def) : fallback;
  };
  const mix = () => clamp(number("mix", 30), 0, 100);
  const hasMix = () => Boolean(findParam(props.device, "mix"));
  // Drag in a local draft; the release is the one committed edit, so a drag
  // cannot fill the undo history with one entry per input event.
  const [mixDraft, setMixDraft] = createSignal<number | null>(null);
  const shownMix = () => (mixDraft() === null ? Math.round(mix()) : mixDraft()!);
  let disposed = false;
  let latestMixOwner: ParameterRequestOwner | null = null;
  const captureMixCommit = () => {
    const target = props.target;
    const index = props.index;
    const type = JSON.stringify(props.effect.effect_type);
    const device = props.device.id;
    const definition = findParam(props.device, "mix");
    const owner = withParameterTarget(store.parameterRequestOwner(), () => !disposed
      && props.target === target && props.index === index && props.device.id === device
      && JSON.stringify(props.effect.effect_type) === type);
    latestMixOwner = owner;
    return (next: number) => {
      if (!definition || !owner()) {
        if (latestMixOwner === owner) setMixDraft(null);
        return;
      }
      void store.setEffectParam(target, index, "mix", toParamString(definition, next), owner)
        .finally(() => { if (latestMixOwner === owner) setMixDraft(null); });
    };
  };
  const mixGesture = createParameterGesture({
    value: shownMix,
    draft: setMixDraft,
    clearDraft: () => setMixDraft(null),
    commit: next => captureMixCommit()(next),
    captureCommit: captureMixCommit,
    surface: () => null,
    quarter: () => store.transport().position_beats,
  });
  onCleanup(() => { disposed = true; mixGesture.dispose(); });
  const mixKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") { mixGesture.interrupt(); return; }
    const current = shownMix();
    const next = event.key === "Home" ? 0 : event.key === "End" ? 100
      : event.key === "ArrowLeft" || event.key === "ArrowDown" ? current - 1
      : event.key === "ArrowRight" || event.key === "ArrowUp" ? current + 1
      : event.key === "PageDown" ? current - 10 : event.key === "PageUp" ? current + 10 : null;
    if (next === null) return;
    event.preventDefault();
    mixGesture.discrete(clamp(next, 0, 100));
  };

  return (
    <div class="device-viz" data-viz={props.kind}>
      <div class="device-section-title">
        {i18n.t(props.kind === "delay" ? "device.viz.delay" : "device.viz.reverb")}
      </div>
      <Show when={props.kind === "delay"} fallback={<ReverbTail decay={number("decay", 2000)} />}>
        <DelayTaps
          timeL={number("time_l", number("time", 375))}
          timeR={number("time_r", number("time", 250))}
          feedback={number("feedback", 35)}
        />
      </Show>
      <Show when={hasMix()}>
        {/* The wet/dry bar is the control, not a readout: dragging it sets the
            effect's mix param through the same commit path as the knobs. */}
        <div class="device-viz-mix">
          <span class="device-viz-mix-label">{i18n.t("device.viz.wetDry")}</span>
          <input
            class="device-viz-mix-slider"
            type="range"
            min="0"
            max="100"
            step="1"
            value={shownMix()}
            aria-label={i18n.t("device.viz.wetDry")}
            onPointerDown={mixGesture.pointerDown}
            onPointerCancel={mixGesture.pointerCancel}
            onKeyDown={mixKeyDown}
            onBlur={() => mixGesture.interrupt()}
            onInput={(event) => {
              const def = findParam(props.device, "mix");
              if (!def) return;
              const next = Math.round(clampParam(def, Number(event.currentTarget.value)));
              mixGesture.input(next);
              event.currentTarget.value = String(next);
            }}
            onChange={(event) => {
              const def = findParam(props.device, "mix");
              if (!def) return;
              mixGesture.change(Math.round(clampParam(def, Number(event.currentTarget.value))));
            }}
            style={{ "--viz-mix": `${shownMix()}%` }}
          />
          <span class="device-viz-mix-value">{`${shownMix()}%`}</span>
        </div>
      </Show>
    </div>
  );
}

function DelayTaps(props: { timeL: number; timeR: number; feedback: number }) {
  const width = 260;
  const height = 50;
  const repeats = Math.min(8, Math.max(1, Math.round(props.feedback / 12)));
  const maxTime = Math.max(1, repeats * Math.max(props.timeL, props.timeR, 1));
  const xAt = (time: number, repeat: number) =>
    8 + (((repeat + 1) * time) / maxTime) * (width - 16);
  return (
    <svg class="device-viz-svg" viewBox={`0 0 ${width} ${height}`} role="img"
      aria-label={i18n.t("device.viz.delay")}>
      <For each={[18, 34]}>
        {(y) => <line class="device-viz-axis" x1="8" x2={width - 8} y1={y} y2={y} />}
      </For>
      <For each={Array.from({ length: repeats }, (_, index) => index)}>
        {(repeat) => (
          <>
            <circle class="device-viz-tap" cx={xAt(props.timeL, repeat)} cy="18"
              r={4 - repeat * 0.25} opacity={1 - repeat / (repeats + 1)} />
            <circle class="device-viz-tap right" cx={xAt(props.timeR, repeat)} cy="34"
              r={4 - repeat * 0.25} opacity={1 - repeat / (repeats + 1)} />
          </>
        )}
      </For>
      <text class="device-viz-label" x="10" y="12">L</text>
      <text class="device-viz-label" x="10" y="46">R</text>
    </svg>
  );
}

function ReverbTail(props: { decay: number }) {
  const width = 260;
  const height = 50;
  const length = Math.max(1, Math.min(props.decay, 10000));
  const span = Math.min(1, Math.max(0.05, length / 10000));
  const points = Array.from({ length: 32 }, (_, index) => {
    const t = index / 31;
    const x = 8 + t * span * (width - 16);
    const y = 8 + (1 - Math.exp(-3.2 * t)) * (height - 16);
    return `${index === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
  return (
    <svg class="device-viz-svg" viewBox={`0 0 ${width} ${height}`} role="img"
      aria-label={i18n.t("device.viz.reverb")}>
      <line class="device-viz-axis" x1="8" x2={width - 8} y1={height - 8} y2={height - 8} />
      <path class="device-viz-tail" d={points} />
      <text class="device-viz-label" x={width - 8} y="12" text-anchor="end">
        {`${trim(length / 1000, 2)} s`}
      </text>
    </svg>
  );
}

function knobPoint(position: number, radius: number): [number, number] {
  const radians = ((-135 + 270 * clamp01(position)) * Math.PI) / 180;
  return [24 + radius * Math.sin(radians), 24 - radius * Math.cos(radians)];
}

function knobArc(position: number): string {
  if (position <= 0.0001) return "";
  const [x0, y0] = knobPoint(0, 18);
  const [x1, y1] = knobPoint(position, 18);
  const large = 270 * position > 180 ? 1 : 0;
  return `M ${x0.toFixed(2)} ${y0.toFixed(2)} A 18 18 0 ${large} 1 ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}

function trim(value: number, decimals: number): string {
  return String(Number(value.toFixed(decimals)));
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
