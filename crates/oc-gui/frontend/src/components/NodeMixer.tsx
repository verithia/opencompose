/**
 * NodeMixer — detail panel for a selected graph node.
 * Shows full mixer controls (fader, pan, effects, sends) for the selected track/bus/master.
 */
import { Show, For, createMemo, createSignal, createEffect, onCleanup } from "solid-js";
import { store } from "../stores/project";
import { busReferences, busTotalReferences, describeBusReferences } from "../utils/mixerRouting";
import { ask } from "@tauri-apps/plugin-dialog";
import * as api from "../stores/api";
import type { TrackLatencyReadoutResponse } from "../stores/api";
import { LatencyReadout } from "./LatencyReadout";
import { EffectsRack } from "./EffectsRack";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import { openContextMenu } from "./ContextMenu";
import { openTrackContextMenu } from "./trackMenu";
import { TextPromptDialog } from "./TextPromptDialog";
import { beginUndoGesture, endUndoGesture } from "../stores/gesture";

function dbToFaderPos(db: number): number {
  if (db <= -60) return 0;
  if (db >= 12) return 1;
  return db <= 0 ? ((db + 60) / 60) * 0.75 : 0.75 + (db / 12) * 0.25;
}

function faderPosToDb(pos: number): number {
  if (pos <= 0) return -60;
  if (pos >= 1) return 12;
  return pos <= 0.75 ? (pos / 0.75) * 60 - 60 : ((pos - 0.75) / 0.25) * 12;
}

export function NodeMixer(props: { nodeId: string }) {
  const [sendTarget, setSendTarget] = createSignal("");
  const [busRouteTarget, setBusRouteTarget] = createSignal("");
  const [latency, setLatency] = createSignal<TrackLatencyReadoutResponse | null>(null);
  /** The in-app rename prompt; the webview has no `window.prompt`. */
  const [renamePrompt, setRenamePrompt] = createSignal<{ index: number; name: string } | null>(null);
  const nodeKind = () => {
    if (props.nodeId === "master") return "master";
    if (props.nodeId.startsWith("bus:")) return "bus";
    return "track";
  };

  const trackIndex = createMemo(() => {
    if (!props.nodeId.startsWith("track:")) return null;
    return parseInt(props.nodeId.split(":")[1], 10);
  });

  const track = createMemo(() => {
    const idx = trackIndex();
    if (idx === null) return null;
    return store.project.tracks[idx] ?? null;
  });

  const bus = createMemo(() => {
    if (!props.nodeId.startsWith("bus:")) return null;
    const name = props.nodeId.slice(4);
    return store.project.buses.find(b => b.name === name) ?? null;
  });

  const nodeLabel = createMemo(() => {
    if (props.nodeId === "master") return "Master";
    if (track()) return track()!.mix.overrides?.display_name || track()!.name;
    if (bus()) return bus()!.name;
    return props.nodeId;
  });

  // Fader state (tracks and master)
  const volumeDb = () =>
    nodeKind() === "master" ? (store.project.master?.volume_db ?? 0) : (track()?.mix.volume_db ?? 0);
  const pan = () => track()?.mix.pan ?? 0;
  const isMuted = () => track()?.mix.mute ?? false;
  const isSolo = () => track()?.mix.solo ?? false;

  const openNodeMenu = (event: MouseEvent) => {
    if (nodeKind() === "master") return;
    event.preventDefault();
    if (nodeKind() === "track") {
      const idx = trackIndex();
      if (idx !== null) {
        openTrackContextMenu({
          x: event.clientX,
          y: event.clientY,
          trackIndex: idx,
          onRename: () => setRenamePrompt({ index: idx, name: track()?.name ?? "" }),
        });
      }
      return;
    }
    const currentBus = bus();
    if (!currentBus) return;
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "mixer-context-menu",
      items: [{
        label: i18n.t("menu.mixer.deleteBus"),
        danger: true,
        onSelect: () => void deleteBus(currentBus.name),
      }],
    });
  };

  /** Delete a bus, asking first when tracks, sends or buses still point at it.
   * The store refuses a bare delete of a referenced bus, so this is the one
   * path that can drop it. */
  const deleteBus = async (name: string) => {
    const refs = busReferences(store.project, name);
    if (busTotalReferences(refs) === 0) {
      void store.removeBus(name);
      return;
    }
    const confirmed = await ask(
      i18n.t("routing.deleteInUseBody", { name, refs: describeBusReferences(refs, i18n.t) }),
      { title: i18n.t("routing.deleteInUseTitle"), kind: "warning" },
    );
    if (confirmed) void store.removeBusReroute(name);
  };

  const [faderDraft, setFaderDraft] = createSignal<number | null>(null);
  const shownDb = () => faderDraft() ?? volumeDb();

  /** The drag that owns the pointer, so an unmount cannot leave it open. */
  let stopFaderDrag: (() => void) | null = null;
  onCleanup(() => stopFaderDrag?.());

  /**
   * Move the fader live and commit once on release. The live command patches
   * the graph only; the document, undo history and snapshot wait for `finish`.
   */
  const startFaderDrag = (event: MouseEvent, container: HTMLElement) => {
    const index = trackIndex();
    const target = nodeKind() === "master" ? "master" : index !== null ? `track:${index}` : null;
    if (target === null) return;
    event.preventDefault();
    stopFaderDrag?.();
    beginUndoGesture("mixer_levels");

    const dbAt = (clientY: number) => {
      const rect = container.getBoundingClientRect();
      const pos = 1 - (clientY - rect.top) / rect.height;
      const clamped = Math.max(0, Math.min(1, pos));
      return Math.round(faderPosToDb(clamped) * 10) / 10;
    };

    let finished = false;
    let frame: number | null = null;
    let pending = dbAt(event.clientY);
    setFaderDraft(pending);

    // The begin must land before the first live update, or that update finds
    // no open gesture and is refused.
    let queue: Promise<unknown> = api.beginMixerTouch(target).catch(() => undefined);
    const flush = () => {
      frame = null;
      const db = pending;
      queue = queue.then(() => api.updateMixerTouch(target, db)).catch(() => undefined);
    };
    const onMove = (moveEvent: MouseEvent) => {
      pending = dbAt(moveEvent.clientY);
      setFaderDraft(pending);
      if (frame === null) frame = requestAnimationFrame(flush);
    };
    const finish = () => {
      if (finished) return;
      finished = true;
      if (frame !== null) {
        cancelAnimationFrame(frame);
        frame = null;
      }
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", finish);
      window.removeEventListener("blur", finish);
      document.removeEventListener("pointercancel", finish);
      stopFaderDrag = null;
      const db = pending;
      void queue
        .then(async () => {
          if (target === "master") await store.updateMasterVolume(db);
          else if (index !== null) await store.updateTrackMix(index, db, undefined);
        })
        .finally(() => {
          setFaderDraft(null);
          endUndoGesture();
        });
    };

    stopFaderDrag = finish;
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", finish);
    window.addEventListener("blur", finish);
    document.addEventListener("pointercancel", finish);
  };

  // The readout is planned per selected track, so a stale response never wins.
  // A bypass adds or drops an effect's latency, so the plan follows the chain.
  const latencyPlanKey = createMemo(() => {
    const idx = trackIndex();
    if (idx === null) return "";
    return (store.project.tracks[idx]?.effects ?? [])
      .map((effect) => `${effect.effect_type}:${effect.bypass === true}`)
      .join("|");
  });

  createEffect(() => {
    const idx = trackIndex();
    if (idx === null) {
      setLatency(null);
      return;
    }
    void latencyPlanKey();
    let active = true;
    onCleanup(() => {
      active = false;
    });
    void api
      .trackLatencyReadout(idx)
      .then((response) => {
        if (active) setLatency(response);
      })
      .catch(() => {
        if (active) setLatency(null);
      });
  });

  const effectsTarget = createMemo(() => {
    if (props.nodeId === "master") return "master";
    if (props.nodeId.startsWith("bus:")) return props.nodeId;
    const idx = trackIndex();
    return idx !== null ? `track:${idx}` : "";
  });

  const effects = createMemo(() => {
    if (props.nodeId === "master") return store.project.master?.effects ?? [];
    if (bus()) return bus()!.effects;
    if (track()) return track()!.effects;
    return [];
  });

  return (
    <div class="node-mixer" data-mixer-node={props.nodeId} onContextMenu={openNodeMenu} style={{
      display: "grid",
      "grid-template-columns": "minmax(0, 1fr) 48px",
      gap: "12px",
      padding: "8px 12px",
      background: "var(--bg-primary)",
      "border-bottom": "1px solid var(--border)",
      "align-items": "start",
      "flex-shrink": "0",
      "overflow-x": "auto",
    }}>
      {/* Header */}
      <div style={{ display: "flex", "flex-direction": "column", "min-width": "80px", gap: "4px" }}>
        <div style={{
          "font-size": "var(--font-size-sm)",
          "font-weight": "600",
          color: nodeKind() === "master" ? "var(--accent)" : "var(--text-primary)",
        }}>
          {nodeLabel()}
        </div>
        <div style={{ "font-size": "9px", color: "var(--text-muted)", "text-transform": "uppercase" }}>
          {nodeKind()}
        </div>

        {/* Mute / Solo buttons (tracks only) */}
        <Show when={nodeKind() === "track" && trackIndex() !== null}>
          <div style={{ display: "flex", gap: "4px", "margin-top": "8px" }}>
            <button
              class="track-btn"
              classList={{ "mute-active": isMuted() }}
              aria-label={isMuted() ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute")}
              onClick={() => { const idx = trackIndex(); if (idx !== null) store.toggleMute(idx); }}
              {...useTooltip(() => isMuted() ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute"))}
              style={{ "font-size": "10px", padding: "2px 6px" }}
            >M</button>
            <button
              class="track-btn"
              classList={{ "solo-active": isSolo() }}
              aria-label={isSolo() ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo")}
              onClick={() => { const idx = trackIndex(); if (idx !== null) store.toggleSolo(idx); }}
              {...useTooltip(() => isSolo() ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo"))}
              style={{ "font-size": "10px", padding: "2px 6px" }}
            >S</button>
          </div>
        </Show>
      </div>

      {/* Fader (tracks and master) */}
      <Show when={nodeKind() !== "bus"}>
        <div style={{ display: "flex", "flex-direction": "column", "align-items": "center", "min-width": "40px" }}>
          <div style={{ "font-size": "10px", "font-family": "var(--font-mono)", color: "var(--text-secondary)", "margin-bottom": "4px" }}>
            {shownDb() > 0 ? "+" : ""}{shownDb().toFixed(1)}dB
          </div>
          <div
            class="mixer-fader-container"
            style={{ height: "80px", width: "24px", position: "relative" }}
            onMouseDown={(e) => startFaderDrag(e, e.currentTarget as HTMLElement)}
          >
            <div
              class="mixer-fader-fill"
              style={{
                height: `${dbToFaderPos(shownDb()) * 100}%`,
                background: isMuted() ? "var(--text-muted)" : "var(--fader-fill)",
              }}
            />
            <div
              class="mixer-fader-thumb"
              style={{ top: `${(1 - dbToFaderPos(shownDb())) * 80 - 3}px` }}
            />
          </div>
          <Show when={nodeKind() === "track"}>
            <div style={{ "font-size": "10px", "font-family": "var(--font-mono)", color: "var(--text-secondary)", "margin-top": "4px" }}>
              {i18n.t("mixer.panReadout", { value: Math.round(pan() * 100) })}
            </div>
          </Show>
        </div>
      </Show>

      {/* Three-part latency readout (tracks only) */}
      <Show when={nodeKind() === "track"}>
        <div style={{ "grid-column": "1 / -1", "min-width": "0" }}>
          <LatencyReadout readout={latency()} />
        </div>
      </Show>

      {/* Effects chain */}
      <div style={{ "grid-column": "1 / -1", "min-width": "0" }}>
        <EffectsRack
          target={effectsTarget()}
          effects={effects()}
          label={i18n.t("mixer.effectsChain")}
        />
      </div>

      {/* Sends (tracks only) */}
      <Show when={nodeKind() === "track"}>
        <div style={{ "grid-column": "1 / -1", "min-width": "0", "font-size": "10px" }}>
          <div style={{ "font-weight": "500", "margin-bottom": "4px", color: "var(--text-muted)" }}>{i18n.t("mixer.routing")}</div>
          <label>
            {i18n.t("device.meter.output")}
            <select aria-label={i18n.t("mixer.trackOutput")} value={track()?.mix.output ?? "master"}
              onChange={(e) => {
                const idx = trackIndex();
                if (idx !== null) void store.setTrackOutput(idx, e.currentTarget.value);
              }}>
              <option value="master">{i18n.t("mixer.master")}</option>
              <For each={store.project.buses}>{(bus) => <option value={bus.name}>{bus.name}</option>}</For>
            </select>
          </label>
          <Show when={store.project.buses.length > 0} fallback={<div style={{ color: "var(--text-muted)" }}>{i18n.t("mixer.createBusForSends")}</div>}>
            <div style={{ display: "flex", gap: "4px", "margin-top": "4px" }}>
              <select
                aria-label={i18n.t("mixer.sendTargetBus")}
                value={sendTarget()}
                onChange={(e) => setSendTarget(e.currentTarget.value)}
                style={{ "min-width": "0", flex: "1" }}
              >
                <option value="">{i18n.t("mixer.addSendOption")}</option>
                <For each={store.project.buses.filter(candidate => !track()?.mix.sends.some(send => send.target === candidate.name))}>
                  {(candidate) => <option value={candidate.name}>{candidate.name}</option>}
                </For>
              </select>
              <button
                aria-label={i18n.t("mixer.addSend")}
                class="track-btn"
                disabled={!sendTarget()}
                title={sendTarget() ? undefined : i18n.t("mixer.pickTargetFirst")}
                onClick={async () => {
                  const idx = trackIndex();
                  if (idx !== null && sendTarget()) {
                    await store.addSend(idx, sendTarget(), 0.25);
                    setSendTarget("");
                  }
                }}
              >+</button>
            </div>
          </Show>
        </div>
      </Show>

      {/* Bus routing */}
      <Show when={nodeKind() === "bus" && bus()}>
        <div style={{ "min-width": "150px", "font-size": "10px" }}>
          <div style={{ "font-weight": "500", "margin-bottom": "4px", color: "var(--text-muted)" }}>{i18n.t("mixer.routesTo")}</div>
          <For each={bus()?.routes ?? []}>
            {(route) => (
              <div style={{ display: "flex", "align-items": "center", "justify-content": "space-between", gap: "4px" }}>
                <span>{route}</span>
                <button
                  aria-label={i18n.t("mixer.removeRoute", { target: route })}
                  class="track-btn"
                  onClick={() => store.setBusRoutes(bus()!.name, bus()!.routes.filter(target => target !== route))}
                >×</button>
              </div>
            )}
          </For>
          <div style={{ display: "flex", gap: "4px", "margin-top": "4px" }}>
            <select
              aria-label={i18n.t("mixer.busRouteTarget")}
              value={busRouteTarget()}
              onChange={(e) => setBusRouteTarget(e.currentTarget.value)}
              style={{ "min-width": "0", flex: "1" }}
            >
              <option value="">{i18n.t("mixer.addRouteOption")}</option>
              <For each={[
                "master",
                ...store.project.buses.filter(candidate => candidate.name !== bus()!.name).map(candidate => candidate.name),
              ].filter(target => !bus()!.routes.includes(target))}>
                {(target) => <option value={target}>{target}</option>}
              </For>
            </select>
            <button
              aria-label={i18n.t("mixer.addRoute")}
              class="track-btn"
              disabled={!busRouteTarget()}
              title={busRouteTarget() ? undefined : i18n.t("mixer.pickTargetFirst")}
              onClick={async () => {
                if (busRouteTarget()) {
                  await store.setBusRoutes(bus()!.name, [...bus()!.routes, busRouteTarget()]);
                  setBusRouteTarget("");
                }
              }}
            >+</button>
          </div>
          <button
            aria-label={i18n.t("mixer.deleteBusLabel", { name: bus()!.name })}
            class="track-btn"
            style={{ "margin-top": "8px" }}
            title={i18n.t("mixer.deleteBusHint")}
            onClick={() => void deleteBus(bus()!.name)}
          >{i18n.t("menu.mixer.deleteBus")}</button>
        </div>
      </Show>

      <Show when={renamePrompt()}>
        {(request) => (
          <TextPromptDialog
            title={i18n.t("menu.renamePrompt")}
            initial={request().name}
            onCancel={() => setRenamePrompt(null)}
            onConfirm={(name) => {
              void store.renameTrack(request().index, name);
              setRenamePrompt(null);
            }}
          />
        )}
      </Show>
    </div>
  );
}
