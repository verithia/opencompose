/**
 * ConnectionDetail — the inspector card for the selected routing connection.
 *
 * It owns every edge-level edit (re-point, send level, pre/post tap, pan,
 * remove), so the toolbar above the graph stays project-level and the graph
 * itself only selects. The card resolves the edge against the live project,
 * which keeps it honest after an edit or a delete elsewhere.
 */
import { For, Show, createMemo, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import type { Send } from "../types/project";
import { edgeSourceNode, type RoutingEdge } from "./BusGraph";
import {
  amountToDb,
  dbToAmount,
  effectiveBusRoutes,
} from "../utils/mixerRouting";

function sameSource(
  a: { owner: string; output: number } | undefined,
  b: { owner: string; output: number } | undefined,
): boolean {
  if (!a || !b) return !a && !b;
  return a.owner === b.owner && a.output === b.output;
}

/** The edge as the project currently holds it, or null when it is gone. */
function resolveEdge(ref: RoutingEdge): RoutingEdge | null {
  if (ref.kind === "main") {
    const track = store.project.tracks[ref.trackIndex ?? -1];
    return track ? { ...ref, target: track.mix.output ?? "master" } : null;
  }
  if (ref.kind === "send") {
    const track = store.project.tracks[ref.trackIndex ?? -1];
    const send = track?.mix.sends.find(
      (candidate) =>
        candidate.target === ref.target && sameSource(candidate.source ?? undefined, ref.source),
    );
    return send ? { ...ref, target: send.target, source: send.source ?? undefined } : null;
  }
  if (ref.kind === "control") {
    const order = (routes: { input_routes?: { kind: string; source: string; target: string }[] } | null | undefined) =>
      routes?.input_routes ?? [];
    const owner = ref.controlOwner ?? "";
    const effects = owner === "master"
      ? order(store.project.master)
      : owner.startsWith("bus:")
        ? order(store.project.buses.find((bus) => `bus:${bus.name}` === owner))
        : order(store.project.tracks[Number(owner.slice(6))]);
    const find = (route: { kind: string; source: string; target: string }) =>
      route.kind === "control" && route.source === ref.controlSource && route.target === ref.target;
    return effects.some(find) ? { ...ref } : null;
  }
  const bus = store.project.buses.find((candidate) => candidate.name === ref.bus);
  if (!bus || !effectiveBusRoutes(bus).includes(ref.target)) return null;
  return { ...ref };
}

/** Remove one connection through the store. True when it no longer exists. */
export async function removeConnection(edge: RoutingEdge): Promise<boolean> {
  if (edge.kind === "send") {
    await store.removeSend(edge.trackIndex!, edge.target, edge.source);
    return resolveEdge(edge) === null;
  }
  if (edge.kind === "route") {
    const bus = store.project.buses.find((candidate) => candidate.name === edge.bus);
    if (!bus) return false;
    const routes = effectiveBusRoutes(bus).filter((route) => route !== edge.target);
    return store.setBusRoutes(edge.bus!, routes);
  }
  if (edge.kind === "control") {
    const error = await store.removeControlRoute(edge.controlOwner!, edge.target);
    return error === null;
  }
  return false;
}

export function ConnectionDetail(props: {
  edge: RoutingEdge;
  /** Re-pointing changes the edge identity; the parent stores the new one. */
  onSelectEdge: (edge: RoutingEdge) => void;
  /** Called after a delete with the node that should get keyboard focus. */
  onClose: (focusNodeId: string) => void;
}) {
  let card: HTMLDivElement | undefined;

  onMount(() => card?.focus());

  const current = createMemo(() => resolveEdge(props.edge));

  const send = createMemo<Send | null>(() => {
    const edge = current();
    if (!edge || edge.kind !== "send") return null;
    const track = store.project.tracks[edge.trackIndex ?? -1];
    return track?.mix.sends.find(
      (candidate) =>
        candidate.target === edge.target && sameSource(candidate.source ?? undefined, edge.source),
    ) ?? null;
  });

  const sourceLabel = (edge: RoutingEdge): string => {
    if (edge.kind === "control") {
      return i18n.t("routing.controlSourceLabel", { name: edge.controlSource ?? "" });
    }
    if (edge.kind === "route") return i18n.t("routing.busSource", { name: edge.bus ?? "" });
    const track = store.project.tracks[edge.trackIndex ?? -1];
    return track?.mix.overrides?.display_name || track?.name
      || i18n.t("routing.trackSource", { index: (edge.trackIndex ?? 0) + 1 });
  };

  const kindLabel = (edge: RoutingEdge): string => i18n.t(
    edge.kind === "main"
      ? "routing.kindMain"
      : edge.kind === "send"
        ? "routing.kindSend"
        : edge.kind === "control"
          ? "routing.kindControl"
          : "routing.kindRoute",
  );

  const targets = (): string[] => {
    const edge = current();
    if (!edge || edge.kind === "control") return [];
    const buses = store.project.buses.map((bus) => bus.name);
    if (edge.kind === "main") return ["master", ...buses];
    if (edge.kind === "send") {
      const track = store.project.tracks[edge.trackIndex ?? -1];
      const used = track?.mix.sends
        .filter((candidate) => !candidate.source)
        .map((candidate) => candidate.target) ?? [];
      return buses.filter((target) => target === edge.target || !used.includes(target));
    }
    const bus = store.project.buses.find((candidate) => candidate.name === edge.bus);
    const used = bus ? effectiveBusRoutes(bus) : [];
    return ["master", ...buses.filter((name) => name !== edge.bus)]
      .filter((target) => target === edge.target || !used.includes(target));
  };

  async function repoint(next: string) {
    const edge = current();
    if (!edge) return;
    if (edge.kind === "main") {
      if (await store.setTrackOutput(edge.trackIndex!, next)) {
        props.onSelectEdge({ ...edge, target: next });
      }
      return;
    }
    if (edge.kind === "send") {
      if (await store.setSendTarget(edge.trackIndex!, edge.target, next, edge.source)) {
        props.onSelectEdge({ ...edge, target: next });
      }
      return;
    }
    const bus = store.project.buses.find((candidate) => candidate.name === edge.bus);
    if (!bus) return;
    const routes = effectiveBusRoutes(bus).map((route) => route === edge.target ? next : route);
    if (await store.setBusRoutes(edge.bus!, routes)) {
      props.onSelectEdge({ ...edge, target: next });
    }
  }

  async function setLevel(db: number) {
    const edge = current();
    if (!edge || edge.kind !== "send" || !Number.isFinite(db)) return;
    await store.setSendAmount(edge.trackIndex!, edge.target, dbToAmount(db), edge.source);
  }

  async function setPan(pan: number) {
    const edge = current();
    if (!edge || edge.kind !== "send" || !Number.isFinite(pan)) return;
    await store.setSendPan(
      edge.trackIndex!, edge.target, Math.max(-1, Math.min(1, pan)), edge.source,
    );
  }

  async function toggleTap() {
    const edge = current();
    const value = send();
    if (!edge || !value || value.source) return;
    await store.setSendPreFader(
      edge.trackIndex!, edge.target, !(value.pre_fader ?? false), edge.source,
    );
  }

  async function remove() {
    const edge = current();
    if (!edge) return;
    if (await removeConnection(edge)) props.onClose(edgeSourceNode(edge));
  }

  /** A send is always removable. A bus route is removable unless removing it
   * would only re-assert the master default the bus already has. */
  const canRemove = (): boolean => {
    const edge = current();
    if (!edge || edge.kind === "main") return false;
    if (edge.kind === "send" || edge.kind === "control") return true;
    const bus = store.project.buses.find((candidate) => candidate.name === edge.bus);
    const routes = bus ? effectiveBusRoutes(bus) : [];
    return !(routes.length === 1 && routes[0] === "master");
  };

  return (
    <Show when={current()}>
      {(edge) => (
        <div
          class="connection-detail"
          role="group"
          aria-label={i18n.t("routing.connection")}
          tabIndex={-1}
          ref={(element) => { card = element; }}
        >
          <div class="connection-detail-header">
            <span class="routing-edge-kind">{kindLabel(edge())}</span>
            <span class="connection-detail-route">
              {sourceLabel(edge())} → {edge().kind === "control"
                ? (edge().controlDevice ?? edge().target)
                : edge().target}
            </span>
          </div>

          <Show when={edge().kind !== "control"}>
          <label class="connection-detail-field">
            <span>{i18n.t("routing.target")}</span>
            <select
              aria-label={i18n.t("routing.targetLabel")}
              value={edge().target}
              onChange={(event) => void repoint(event.currentTarget.value)}
            >
              <For each={targets()}>{(target) => <option value={target}>{target}</option>}</For>
            </select>
          </label>
          </Show>

          <Show when={send()}>
            {(value) => (
              <>
                <label class="connection-detail-field">
                  <span>{i18n.t("routing.level")}</span>
                  <input
                    aria-label={i18n.t("routing.levelLabel")}
                    type="number"
                    step="0.5"
                    min="-60"
                    max="12"
                    value={Number.isFinite(amountToDb(value().amount))
                      ? amountToDb(value().amount).toFixed(1)
                      : "-60"}
                    onChange={(event) => void setLevel(Number(event.currentTarget.value))}
                  />
                </label>

                <label class="connection-detail-field">
                  <span>{i18n.t("routing.pan")}</span>
                  <input
                    aria-label={i18n.t("routing.panLabel")}
                    type="number"
                    step="0.05"
                    min="-1"
                    max="1"
                    value={(value().pan ?? 0).toFixed(2)}
                    onChange={(event) => void setPan(Number(event.currentTarget.value))}
                  />
                </label>

                <div class="connection-detail-actions">
                  <Show
                    when={!value().source}
                    fallback={
                      <span class="connection-detail-hint" title={i18n.t("routing.auxHint")}>
                        {i18n.t("routing.aux")}
                      </span>
                    }
                  >
                    <button
                      class="routing-btn"
                      classList={{ "tap-pre": value().pre_fader ?? false }}
                      aria-label={i18n.t("routing.tapLabel")}
                      aria-pressed={value().pre_fader ?? false}
                      onClick={() => void toggleTap()}
                    >{value().pre_fader ? i18n.t("routing.pre") : i18n.t("routing.post")}</button>
                  </Show>
                  <Show when={canRemove()}>
                    <button
                      class="routing-btn routing-danger"
                      aria-label={i18n.t("routing.removeConnection")}
                      onClick={() => void remove()}
                    >{i18n.t("routing.remove")}</button>
                  </Show>
                </div>
              </>
            )}
          </Show>

          <Show when={edge().kind === "control"}>
            <span class="connection-detail-hint">{i18n.t("routing.controlHint")}</span>
            <Show when={canRemove()}>
              <div class="connection-detail-actions">
                <button
                  class="routing-btn routing-danger"
                  aria-label={i18n.t("routing.controlRemove")}
                  onClick={() => void remove()}
                >{i18n.t("routing.remove")}</button>
              </div>
            </Show>
          </Show>

          <Show when={edge().kind === "main"}>
            <span class="connection-detail-hint">{i18n.t("routing.mainHint")}</span>
          </Show>

          <Show when={edge().kind === "route" && canRemove()}>
            <div class="connection-detail-actions">
              <button
                class="routing-btn routing-danger"
                aria-label={i18n.t("routing.removeConnection")}
                onClick={() => void remove()}
              >{i18n.t("routing.remove")}</button>
            </div>
          </Show>
        </div>
      )}
    </Show>
  );
}
