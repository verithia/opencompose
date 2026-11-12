/**
 * Generated parameter panel for a hosted plugin that has no editor.
 *
 * The parameter list comes from the backend's typed surface: a format that
 * cannot enumerate says why instead of rendering an empty box. Controls are
 * grouped when the plugin names a group, otherwise they stay in enum order;
 * the read-out is the plugin's own value and unit. A drag rides the hosted
 * plugin's gesture command (one undo entry), double-click resets, and the
 * context menu writes a lane point through the same automation target the
 * built-in synth uses.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, untrack } from "solid-js";

import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import type { PluginPanelParam } from "../stores/api";
import {
  automatePluginParam,
  beginPluginParamDrag,
  dragPluginParam,
  endPluginParamDrag,
  hasPluginAutomation,
  loadPluginPanel,
  panelTrackIndex,
  panelValue,
  type PluginParamDragHandle,
  pluginPanel,
  resetPluginParam,
} from "../stores/pluginPanel";
import { i18n } from "../stores/i18n";
import { automationLaneName, pluginAutomationTarget } from "../utils/automationTarget";
import {
  automationReadout,
  unwatchAutomation,
  watchAutomation,
} from "../stores/automationReadout";
import { useTooltip } from "./Tooltip";
import { ControlLinkEditor } from "./ControlLinkEditor";
import { rememberTweaked } from "../stores/controlLinks";
import { store } from "../stores/project";

/** Parameters of one group, in enum order; an ungrouped plugin is one section. */
function groupedParams(params: PluginPanelParam[]): { label: string | null; params: PluginPanelParam[] }[] {
  const groups = new Map<string, PluginPanelParam[]>();
  for (const param of params) {
    const key = param.group?.trim() ?? "";
    const list = groups.get(key);
    if (list) list.push(param);
    else groups.set(key, [param]);
  }
  if (groups.size === 1 && groups.has("")) {
    return [{ label: null, params: groups.get("") ?? [] }];
  }
  return [...groups.entries()].map(([label, list]) => ({
    label: label === "" ? i18n.t("plugin.panel.general") : label,
    params: list,
  }));
}

function formatValue(value: number, steps: number | null): string {
  if (!Number.isFinite(value)) return "-";
  if (steps !== null && steps > 1 && steps <= 32) return String(Math.round(value));
  const rounded = (Math.abs(value) >= 100 ? value.toFixed(1) : value.toFixed(2))
    .replace(/(\.\d*?)0+$/, "$1")
    .replace(/\.$/, "");
  return rounded === "-0" ? "0" : rounded;
}

/**
 * The plugin's own text until the control has been moved; after that the host's
 * numeric form, because a moved value has no plugin-formatted string yet.
 */
function valueText(param: PluginPanelParam, current: number, moved: boolean): string {
  if (!moved && param.display.trim() !== "") return param.display;
  const value = formatValue(current, param.steps);
  return param.unit.trim() === "" ? value : `${value} ${param.unit}`;
}

/** The typed "no panel" states, each with its own sentence. */
function Unavailable(props: { format: string; message: string }) {
  return (
    <div class="plugin-panel-unavailable" role="status">
      <div class="plugin-panel-unavailable-format">{props.format.toUpperCase()}</div>
      <div class="plugin-panel-unavailable-text">{props.message}</div>
    </div>
  );
}

function unavailableMessage(): string {
  const state = pluginPanel();
  if (state.status === "error") {
    return i18n.t("plugin.panel.notAvailable.failed", { message: state.error ?? "" });
  }
  const issue = state.issue;
  if (!issue) return i18n.t("plugin.panel.notAvailable.none");
  switch (issue.kind) {
    case "format_has_no_parameters":
      return i18n.t("plugin.panel.notAvailable.format", { format: issue.format });
    case "editor_format":
      return i18n.t("plugin.panel.notAvailable.editor", { format: issue.format });
    case "plugin_reports_no_parameters":
      return i18n.t("plugin.panel.notAvailable.none");
    case "query_failed":
      return i18n.t("plugin.panel.notAvailable.failed", { message: issue.message });
    default:
      return i18n.t("plugin.panel.notAvailable.none");
  }
}

function ParamRow(props: { preset: string; param: PluginPanelParam; track: string | null }) {
  const target = () => pluginAutomationTarget(props.param);
  const value = () => panelValue(props.param);
  const moved = () => panelValue(props.param).moved;

  /** The lane on this preset's track that drives this parameter, if any. */
  const lane = (): string | null => {
    const index = panelTrackIndex(props.preset);
    if (index < 0) return null;
    const name = automationLaneName(props.param.name);
    const found = (store.project.tracks[index]?.automation ?? []).some((entry) => entry.param === name);
    return found ? name : null;
  };
  createEffect(() => {
    const watched = lane();
    const index = panelTrackIndex(props.preset);
    if (!watched || index < 0) return;
    const owner = `track:${index}`;
    watchAutomation(owner, watched, untrack(() => store.transport().position_beats));
    onCleanup(() => unwatchAutomation(owner, watched));
  });
  const answer = () => {
    const watched = lane();
    const index = panelTrackIndex(props.preset);
    return watched && index >= 0 ? automationReadout(`track:${index}`, watched) ?? null : null;
  };
  /** The driven value while no drag owns the control; the plugin's otherwise. */
  const driving = () => {
    if (moved()) return null;
    const current = answer();
    return current?.status === "driven" ? current : null;
  };
  const drivenValue = () => {
    const current = driving();
    if (!current) return null;
    const resolved = target();
    return resolved.available ? resolved.decode(current.value) : current.value;
  };
  const displayed = () => driving()?.value ?? value().normalized;
  const displayedText = () => {
    const physical = drivenValue();
    return physical === null
      ? valueText(props.param, value().current, moved())
      : valueText(props.param, physical, true);
  };
  const laneReason = () => {
    if (!lane() || moved()) return null;
    const current = answer();
    return current?.status === "not_driven" ? current.reason : null;
  };
  const step = () => {
    const steps = props.param.steps;
    return steps !== null && steps > 1 ? 1 / (steps - 1) : 0.001;
  };
  let dragOwner: PluginParamDragHandle | null = null;
  let pointerSequence = false;
  let removePointerListeners = () => {};
  const finish = (owner: PluginParamDragHandle, canceled = false) => {
    if (dragOwner === owner) {
      dragOwner = null;
      removePointerListeners();
      removePointerListeners = () => {};
    }
    void endPluginParamDrag(owner, canceled);
  };
  const beginPointer = (event: PointerEvent) => {
    if ((event.button ?? 0) !== 0) return;
    if (dragOwner) finish(dragOwner);
    pointerSequence = true;
    const owner = beginPluginParamDrag(props.preset, props.param);
    dragOwner = owner;
    const id = event.pointerId ?? 0;
    const release = (end: PointerEvent) => { if ((end.pointerId ?? 0) === id) finish(owner); };
    const cancel = (end: PointerEvent) => { if ((end.pointerId ?? 0) === id) finish(owner, true); };
    const blur = () => finish(owner, true);
    removePointerListeners = () => {
      window.removeEventListener("pointerup", release, true);
      window.removeEventListener("pointercancel", cancel, true);
      window.removeEventListener("blur", blur);
    };
    window.addEventListener("pointerup", release, true);
    window.addEventListener("pointercancel", cancel, true);
    window.addEventListener("blur", blur);
  };
  const drag = (value: number) => {
    if (pointerSequence && !dragOwner) pointerSequence = false;
    const owner = dragOwner ?? beginPluginParamDrag(props.preset, props.param);
    dragOwner = owner;
    dragPluginParam(props.preset, props.param, value, owner);
  };
  const endDrag = () => { if (dragOwner) finish(dragOwner); };
  onCleanup(endDrag);
  const reset = (event: Event) => {
    event.preventDefault();
    void resetPluginParam(props.preset, props.param);
  };
  const menu = (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const resolved = target();
    const label = resolved.available ? resolved.label : props.param.name;
    const items: ContextMenuItem[] = [{ type: "heading", label }];
    if (!resolved.available) {
      items.push({
        type: "item",
        label: i18n.t("plugin.panel.automate"),
        disabled: true,
        hint: resolved.reason,
        onSelect: () => {},
      });
    } else {
      const automated = hasPluginAutomation(props.preset, props.param.name);
      items.push({
        type: "item",
        label: i18n.t("plugin.panel.automate"),
        onSelect: () => void automatePluginParam(props.preset, props.param, value().normalized),
      });
      items.push({
        type: "item",
        label: i18n.t("plugin.panel.reset"),
        onSelect: () => void resetPluginParam(props.preset, props.param),
      });
      if (automated) {
        items.push({
          type: "item",
          label: i18n.t("plugin.panel.automated"),
          disabled: true,
          onSelect: () => {},
        });
      }
    }
    openContextMenu({ x: event.clientX, y: event.clientY, ariaLabel: label, items });
  };
  const hint = () => {
    const parts = [i18n.t("plugin.panel.valueHint", { name: props.param.name })];
    if (props.param.automatable) parts.push(i18n.t("plugin.panel.doubleClick"));
    const value = target();
    if (!value.available) parts.push(value.reason);
    const reason = laneReason();
    if (reason) parts.push(reason);
    return parts.join(" · ");
  };
  return (
    <div
      class="plugin-panel-row"
      classList={{
        bypassing: props.param.by_passing,
        stepped: props.param.steps !== null,
        driven: driving() !== null,
      }}
      data-param={props.param.name}
      data-automation-value={value().normalized}
      onPointerDown={() => {
        if (props.track) {
          rememberTweaked({
            track: props.track,
            trackIndex: store.project.tracks.findIndex((entry) => entry.name === props.track),
            param: props.param.name,
            label: props.param.name,
          });
        }
      }}
      onDblClick={reset}
      onContextMenu={menu}
    >
      <span class="plugin-panel-name" {...useTooltip(hint)}>
        <Show when={driving()}>
          <span
            class="plugin-panel-driven-badge"
            data-driven-badge="true"
            aria-label={i18n.t("automation.drivenBadge")}
            title={i18n.t("automation.drivenTitle")}
          >A</span>
        </Show>
        {props.param.name}
      </span>
      <input
        class="plugin-panel-slider"
        aria-label={props.param.name}
        type="range"
        min={0}
        max={1}
        step={step()}
        value={displayed()}
        onPointerDown={beginPointer}
        onInput={(event) => drag(Number(event.currentTarget.value))}
        onChange={() => { if (!pointerSequence) endDrag(); }}
        onKeyDown={() => { endDrag(); pointerSequence = false; }}
        onBlur={endDrag}
      />
      <span class="plugin-panel-value" {...useTooltip(hint)}>{displayedText()}</span>
      <Show when={props.param.by_passing}>
        <span class="plugin-panel-bypass" {...useTooltip(() => i18n.t("plugin.panel.bypassing"))}>
          {i18n.t("plugin.panel.bypassShort")}
        </span>
      </Show>
    </div>
  );
}

export function PluginParamPanel(props: { preset: string }) {
  const [loadedPreset, setLoadedPreset] = createSignal<string | null>(null);
  const trackName = createMemo(() => {
    const selected = store.selectedTrackIndex();
    if (selected !== null && store.project.tracks[selected]?.preset === props.preset) {
      return store.project.tracks[selected].name;
    }
    return store.project.tracks.find((track) => track.preset === props.preset)?.name ?? null;
  });

  // One read per preset; the backend keeps the instance alive, so a revisit is cheap.
  createEffect(() => {
    const name = props.preset;
    if (!name || loadedPreset() === name) return;
    setLoadedPreset(name);
    void loadPluginPanel(name);
  });

  const state = pluginPanel;
  const params = createMemo(() => state().params);
  const groups = createMemo(() => groupedParams(params()));

  return (
    <Show when={state().status !== "idle"} fallback={
      <div class="plugin-panel-status" role="status">{i18n.t("plugin.panel.loading")}</div>
    }>
      <Show when={state().status === "loading"}>
        <div class="plugin-panel-status" role="status">{i18n.t("plugin.panel.loading")}</div>
      </Show>
      <Show when={state().status === "unavailable" || state().status === "error"}>
        <Unavailable format={state().format} message={unavailableMessage()} />
      </Show>
      <Show when={state().status === "ready"}>
        <div class="plugin-panel">
          <div class="plugin-panel-header">
            <span class="plugin-panel-title">{i18n.t("plugin.panel.title")}</span>
            <span class="plugin-panel-count">
              {i18n.t("plugin.panel.count", { count: String(params().length) })}
            </span>
          </div>
          <For each={groups()}>
            {(group) => (
              <section class="plugin-panel-group">
                <Show when={group.label}>
                  <div class="plugin-panel-group-name">{group.label}</div>
                </Show>
                <For each={group.params}>
                  {(param) => <ParamRow preset={props.preset} param={param} track={trackName()} />}
                </For>
              </section>
            )}
          </For>
          <Show when={state().error}>
            <div class="plugin-panel-error">{state().error}</div>
          </Show>
          <ControlLinkEditor />
        </div>
      </Show>
    </Show>
  );
}
