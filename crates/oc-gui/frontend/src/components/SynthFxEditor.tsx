import { formatParameterValue } from "../utils/parameterUnits";
import type { UnitId } from "../data/units.gen";
import { For, Show, createMemo, createSignal } from "solid-js";
import type { ParamValue, PresetDef } from "../types/project";
import { Dropdown } from "./Dropdown";
import { SYNTH_CATALOG } from "../data/synthCatalog";
import { synthOptionLabel } from "../utils/synthOptionLabel";
import { i18n, type I18nKey } from "../stores/i18n";

type FxKind = string;

interface ParamSpec {
  key: string;
  label: string;
  min: number;
  max: number;
  step: number;
  fallback: number;
  unit: UnitId;
  /** Discrete choices render a select instead of a range slider. */
  choices?: { value: number; label: string }[];
}

interface FxModule {
  /** Project parameter index (1-based, matching `fx.N.*`). */
  index: number;
  kind: FxKind;
  enabled: boolean;
  /** Every non-type/enabled `fx.N.<field>` key, kept as raw text. */
  fields: [string, string][];
}

// The rack's module list and per-module controls come from the parameter
// catalog; the extra nine modules are rack-ready but have no editor controls.
const FX_TYPES = SYNTH_CATALOG.fxTypes.map((entry) => ({ id: entry.id, label: entry.label }));

const effectLabel = (kind: FxKind) =>
  synthOptionLabel(FX_TYPES.find(entry => entry.id === kind)?.label ?? kind);

function catalogSpecs(kind: FxKind): ParamSpec[] {
  const entry = SYNTH_CATALOG.fxTypes.find((fx) => fx.id === kind);
  if (!entry) return [];
  return entry.params
    .filter((param) => param.control !== "none")
    .map((param) => ({
      key: param.key,
      label: i18n.t(param.label as I18nKey),
      min: param.min,
      max: param.max,
      step: param.step,
      fallback: param.default,
      unit: param.unit,
      choices: param.options.length > 0
        ? param.options.map((option) => ({ value: option.id, label: synthOptionLabel(option.label) }))
        : undefined,
    }));
}

function paramText(value: ParamValue): string {
  if ("String" in value) return value.String;
  if ("Array" in value) return value.Array.join(", ");
  if ("Number" in value) return String(value.Number);
  if ("Db" in value) return String(value.Db);
  if ("Percentage" in value) return String(value.Percentage);
  if ("Frequency" in value) return String(value.Frequency);
  if ("Cents" in value) return String(value.Cents);
  if ("Time" in value) return String(value.Time);
  if ("Bool" in value) return value.Bool ? "1" : "0";
  return "";
}

function numberFromParam(value: ParamValue | undefined, fallback: number): number {
  if (!value) return fallback;
  if ("String" in value) {
    const parsed = Number.parseFloat(value.String);
    return Number.isFinite(parsed) ? parsed : fallback;
  }
  const parsed = Number.parseFloat(paramText(value));
  return Number.isFinite(parsed) ? parsed : fallback;
}

function kindFromParam(value: ParamValue | undefined): FxKind {
  if (!value) return "drive";
  if ("String" in value) {
    const normalized = value.String.trim().toLowerCase().replaceAll(/[ _-]/g, "");
    const named = FX_TYPES.find((entry) =>
      entry.id === normalized || entry.label.toLowerCase().replaceAll(/[ _-]/g, "") === normalized
    );
    if (named) return named.id;
    if (normalized === "eq" || normalized === "equalizer") return "eq3";
    if (normalized === "comp") return "compressor";
    if (normalized === "overdrive") return "drive";
    if (normalized === "dist" || normalized === "saturation" || normalized === "saturator") return "distortion";
    return "drive";
  }
  const index = Math.round(numberFromParam(value, 0));
  return SYNTH_CATALOG.fxTypes[index]?.id ?? "drive";
}

function defaultFields(kind: FxKind): [string, string][] {
  return catalogSpecs(kind).map((spec): [string, string] => [spec.key, String(spec.fallback)]);
}

function readModules(preset: PresetDef): FxModule[] {
  const indexes = new Set<number>();
  for (const [key] of preset.params) {
    const match = /^fx\.(\d+)\./.exec(key);
    if (match) indexes.add(Number(match[1]));
  }
  return [...indexes]
    .sort((left, right) => left - right)
    .map((index) => {
      const lookup = (field: string) =>
        preset.params.find(([key]) => key === `fx.${index}.${field}`)?.[1];
      const enabledValue = lookup("enabled");
      const fields: [string, string][] = [];
      for (const [key, value] of preset.params) {
        if (!key.startsWith(`fx.${index}.`)) continue;
        const field = key.slice(`fx.${index}.`.length);
        if (field === "type" || field === "enabled" || !field) continue;
        fields.push([field, paramText(value)]);
      }
      return {
        index,
        kind: kindFromParam(lookup("type")),
        enabled: enabledValue === undefined ? true : numberFromParam(enabledValue, 1) >= 0.5,
        fields,
      };
    });
}

/**
 * Rebuild the whole rack as one batch. Deleting the old keys before writing the
 * new ones keeps removed/reordered modules from leaving stale `fx.N.*` entries.
 */
function rackParams(current: FxModule[], next: FxModule[]): [string, string | null][] {
  const params: [string, string | null][] = [];
  for (const module of current) {
    params.push([`fx.${module.index}.type`, null], [`fx.${module.index}.enabled`, null]);
    for (const [field] of module.fields) {
      params.push([`fx.${module.index}.${field}`, null]);
    }
  }
  next.forEach((module, position) => {
    const index = position + 1;
    params.push([`fx.${index}.type`, module.kind]);
    params.push([`fx.${index}.enabled`, module.enabled ? "1" : "0"]);
    const fields = module.fields.length > 0 ? module.fields : defaultFields(module.kind);
    for (const [field, value] of fields) {
      params.push([`fx.${index}.${field}`, value]);
    }
  });
  // Explicit count lets an empty rack clear FX imported from a preset.
  params.push(["fx.count", String(next.length)]);
  return params;
}

function displayValue(spec: ParamSpec, value: number): string {
  return formatParameterValue(value, spec.step, spec.unit);
}

function displayParam(spec: ParamSpec, value: number): string {
  const choice = spec.choices?.find((entry) => entry.value === Math.round(value));
  return choice?.label ?? displayValue(spec, value);
}

export function SynthFxEditor(props: {
  preset: PresetDef;
  setParams: (params: [string, string | null][], replaceSynth?: boolean) => Promise<string | null>;
}) {
  const [newType, setNewType] = createSignal<FxKind>("drive");
  const [drafts, setDrafts] = createSignal<Record<string, number>>({});
  const modules = createMemo(() => readModules(props.preset));
  const values = createMemo(() => {
    const map = new Map<string, number>();
    for (const [key, value] of props.preset.params) {
      if (key.startsWith("fx.")) map.set(key, numberFromParam(value, 0));
    }
    return map;
  });
  const value = (key: string, fallback: number) => drafts()[key] ?? values().get(key) ?? fallback;
  const commitRack = async (next: FxModule[]) => {
    await props.setParams(rackParams(modules(), next));
    setDrafts({});
  };
  const setEnabled = async (module: FxModule, enabled: boolean) => {
    await props.setParams([[`fx.${module.index}.enabled`, enabled ? "1" : "0"]]);
  };
  const setParam = async (module: FxModule, key: string, next: number) => {
    await props.setParams([[`fx.${module.index}.${key}`, String(next)]]);
    setDrafts((current) => {
      const copy = { ...current };
      delete copy[`fx.${module.index}.${key}`];
      return copy;
    });
  };
  const setDraft = (key: string, next: number) =>
    setDrafts((current) => ({ ...current, [key]: next }));

  return (
    <section class="synth-fx-rack" aria-label={i18n.t("synth.ui.fxRack")}>
      <header class="synth-fx-rack-header">
        <div class="synth-fx-rack-title">
          <b>{i18n.t("synth.ui.fxRack")}</b>
        </div>
        <div class="synth-fx-add">
          <Dropdown
            label={i18n.t("synth.ui.fxType")}
            hideLabel
            class="synth-fx-type-picker"
            value={newType()}
            options={FX_TYPES.map((entry) => ({ value: entry.id, label: synthOptionLabel(entry.label) }))}
            onSelect={(next) => setNewType(next)}
          />
          <button
            class="synth-fx-add-button"
            onClick={() => void commitRack([
              ...modules(),
              { index: 0, kind: newType(), enabled: true, fields: defaultFields(newType()) },
            ])}
          >{i18n.t("synth.ui.fxAdd")}</button>
        </div>
      </header>

      <Show
        when={modules().length > 0}
        fallback={<div class="synth-fx-empty">{i18n.t("synth.ui.fxEmpty")}</div>}
      >
        <div class="synth-fx-chain">
          <For each={modules()}>
            {(module, position) => (
              <article class="synth-fx-module" classList={{ bypassed: !module.enabled }}>
                <header>
                  <span class="synth-fx-module-index">{String(position() + 1).padStart(2, "0")}</span>
                  <Dropdown
                    label={i18n.t("synth.ui.fxModuleType", { n: position() + 1 })}
                    hideLabel
                    class="synth-fx-type"
                    value={module.kind}
                    options={FX_TYPES.map((entry) => ({ value: entry.id, label: synthOptionLabel(entry.label) }))}
                    onSelect={(kind) => {
                      const next = modules().map((entry, index) =>
                        index === position() ? { ...entry, kind, fields: defaultFields(kind) } : entry
                      );
                      void commitRack(next);
                    }}
                  />
                  <label class="synth-fx-enable">
                    <input
                      type="checkbox"
                      checked={module.enabled}
                      aria-label={i18n.t("synth.ui.fxEnable", { name: effectLabel(module.kind) })}
                      onChange={(event) => void setEnabled(module, event.currentTarget.checked)}
                    />
                    <span>{i18n.t("synth.ui.fxEnabled")}</span>
                  </label>
                  <div class="synth-fx-module-actions">
                    <button
                      aria-label={i18n.t("synth.ui.fxUp")}
                      disabled={position() === 0}
                      onClick={() => {
                        const next = [...modules()];
                        const target = next[position()];
                        next[position()] = next[position() - 1];
                        next[position() - 1] = target;
                        void commitRack(next);
                      }}
                    >↑</button>
                    <button
                      aria-label={i18n.t("synth.ui.fxDown")}
                      disabled={position() === modules().length - 1}
                      onClick={() => {
                        const next = [...modules()];
                        const target = next[position()];
                        next[position()] = next[position() + 1];
                        next[position() + 1] = target;
                        void commitRack(next);
                      }}
                    >↓</button>
                    <button
                      aria-label={i18n.t("synth.ui.fxRemove")}
                      onClick={() => void commitRack(modules().filter((_, index) => index !== position()))}
                    >×</button>
                  </div>
                </header>
                <Show when={catalogSpecs(module.kind).length === 0}>
                  <p class="synth-fx-note">{i18n.t("synth.ui.fxNoControls")}</p>
                </Show>
                <div class="synth-fx-params">
                  <For each={catalogSpecs(module.kind)}>
                    {(spec) => {
                      const key = `${module.index}.${spec.key}`;
                      const current = () => value(`fx.${key}`, spec.fallback);
                      return (
                        <div class="synth-fx-param" data-param={`fx.${key}`}>
                          <span>
                            <em>{spec.label}</em>
                            <output>{displayParam(spec, current())}</output>
                          </span>
                          <Show
                            when={spec.choices}
                            fallback={
                              <input
                                type="range"
                                aria-label={i18n.t("synth.ui.fxParam", { effect: effectLabel(module.kind), param: spec.label })}
                                min={spec.min}
                                max={spec.max}
                                step={spec.step}
                                value={current()}
                                onInput={(event) => setDraft(`fx.${key}`, Number(event.currentTarget.value))}
                                onChange={(event) => void setParam(module, spec.key, Number(event.currentTarget.value))}
                                onDblClick={() => void setParam(module, spec.key, spec.fallback)}
                              />
                            }
                          >
                            <Dropdown
                              label={i18n.t("synth.ui.fxParam", { effect: effectLabel(module.kind), param: spec.label })}
                              hideLabel
                              class="synth-fx-select"
                              value={Math.round(current())}
                              options={(spec.choices ?? []).map((choice) => ({ value: choice.value, label: choice.label }))}
                              onSelect={(next) => void setParam(module, spec.key, next)}
                            />
                          </Show>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </article>
            )}
          </For>
        </div>
      </Show>
    </section>
  );
}
