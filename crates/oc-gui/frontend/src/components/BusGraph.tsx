/**
 * BusGraph — DAG visualization of the audio routing graph.
 *
 * Structure:
 * - Leaves = Tracks (top, deepest depth)
 * - Internal nodes = Buses (effects/mixing nodes)
 * - Root = Master (bottom)
 *
 * Depth = max(depth(children)) + 1. Deeper nodes sit on top.
 * Nodes are thumbnails (level, pan, fx slots, sends, M/S). Click selects the
 * node; the mixer inspector on the left shows its full configuration.
 * The SVG renders at the graph's natural pixel size, which is often larger
 * than the panel, so the background is grabbable: dragging pans the view.
 */
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import type { Track, Bus, Effect, Send } from "../types/project";
import type { DeviceDef } from "../types/devices";
import { FALLBACK_DEVICES, findDevice, loadDevicesCatalog } from "../devices/catalog";
import { EffectGlyph, effectLabel } from "./effectIcons";
import { effectDeviceId } from "./EffectsRack";

// ============================================================================
// DAG Model
// ============================================================================

export type NodeKind = "track" | "bus" | "master";

export interface GraphNode {
  id: string;           // "track:0", "bus:reverb_bus", "master"
  kind: NodeKind;
  label: string;
  depth: number;        // computed: max(depth of children) + 1
  effects: Effect[];
  // For tracks:
  volumeDb?: number;
  pan?: number;
  mute?: boolean;
  solo?: boolean;
  sends?: Send[];
  // For buses:
  routes?: string[];
  /** Control sources fed by this track, exposed as graph output sockets. */
  controlOutputs?: string[];
}

export type RoutingEdgeKind = "main" | "send" | "route" | "control";

/** One editable connection in the routing graph. */
export interface RoutingEdge {
  kind: RoutingEdgeKind;
  /** Destination bus name, "master", or an effect address for a control edge. */
  target: string;
  trackIndex?: number;
  /** Source bus for a bus-route edge. */
  bus?: string;
  /** Auxiliary tap of a send edge; absent for channel sends. */
  source?: { owner: string; output: number };
  /** Control source feeding this edge; set only for `control`. */
  controlSource?: string;
  /** Track index whose audio feeds the control source; set only for `control`. */
  controlFeed?: number;
  /** Node that owns the control input: "track:N", "bus:name" or "master". */
  controlOwner?: string;
  /** Human label of the device whose control input this edge lands on. */
  controlDevice?: string;
}

export interface GraphEdge extends RoutingEdge {
  id: string;
  from: string;  // source node id (deeper, sits above)
  to: string;    // target node id (shallower, sits below)
  amount?: number; // send amount if applicable
}

/** Stable identity for edge selection; target and tap are part of it. */
export function edgeId(edge: RoutingEdge): string {
  switch (edge.kind) {
    case "main":
      return `main:${edge.trackIndex}`;
    case "send":
      return `send:${edge.trackIndex}:${encodeURIComponent(edge.target)}:${
        edge.source ? `${encodeURIComponent(edge.source.owner)}.out${edge.source.output}` : ""
      }`;
    case "route":
      return `route:${encodeURIComponent(edge.bus ?? "")}:${encodeURIComponent(edge.target)}`;
    case "control":
      return `control:${edge.controlOwner}:${encodeURIComponent(edge.controlSource ?? "")}:${
        encodeURIComponent(edge.target)
      }`;
  }
}

/** The node an edge leaves from, so a selection can follow it. */
export function edgeSourceNode(edge: RoutingEdge): string {
  if (edge.kind === "route") return `bus:${edge.bus}`;
  if (edge.kind === "control") return `track:${edge.controlFeed}`;
  return `track:${edge.trackIndex}`;
}

/** Build the DAG from project state. */
function buildGraph(
  tracks: Track[],
  buses: Bus[],
  hasMaster: boolean,
): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const nodes: GraphNode[] = [];
  const edges: GraphEdge[] = [];
  const depthMap = new Map<string, number>();

  // Add master node
  if (hasMaster || true) { // Always show master
    nodes.push({
      id: "master",
      kind: "master",
      label: i18n.t("mixer.master"),
      depth: 0,
      effects: store.project.master?.effects ?? [],
    });
    depthMap.set("master", 0);
  }

  // Add bus nodes — determine depth based on routing
  // Buses can route to other buses or to master
  for (const bus of buses) {
    nodes.push({
      id: `bus:${bus.name}`,
      kind: "bus",
      label: bus.name,
      depth: 1, // placeholder, computed below
      effects: bus.effects,
      routes: bus.routes,
    });
  }

  // Add track nodes
  for (let i = 0; i < tracks.length; i++) {
    const t = tracks[i];
    nodes.push({
      id: `track:${i}`,
      kind: "track",
      label: t.mix.overrides?.display_name || t.name,
      depth: 2, // placeholder
      effects: t.effects,
      volumeDb: t.mix.volume_db,
      pan: t.mix.pan,
      mute: t.mix.mute,
      solo: t.mix.solo,
      sends: t.mix.sends,
    });
  }

  // Main output is exclusive; sends remain additional parallel paths.
  for (let i = 0; i < tracks.length; i++) {
    const t = tracks[i];
    const output = t.mix.output ?? "master";
    const main: RoutingEdge = { kind: "main", trackIndex: i, target: output };
    edges.push({
      ...main,
      id: edgeId(main),
      from: `track:${i}`,
      to: output === "master" ? "master" : `bus:${output}`,
    });
    for (const send of t.mix.sends) {
      const ref: RoutingEdge = {
        kind: "send",
        trackIndex: i,
        target: send.target,
        source: send.source ?? undefined,
      };
      edges.push({
        ...ref,
        id: edgeId(ref),
        from: `track:${i}`,
        to: `bus:${send.target}`,
        amount: send.amount,
      });
    }
  }

  for (const bus of buses) {
    // Bus routes to master (or other buses named in routes). Empty routes are
    // the DSL's legacy master default, so the master edge still exists.
    const routes = bus.routes.length === 0 ? ["master"] : bus.routes;
    for (const route of routes) {
      const ref: RoutingEdge = { kind: "route", bus: bus.name, target: route };
      edges.push({
        ...ref,
        id: edgeId(ref),
        from: `bus:${bus.name}`,
        to: route === "master" ? "master" : `bus:${route}`,
      });
    }
  }

  // Control edges. A control source is an output of the track whose audio
  // feeds it; its consumer is the node whose chain holds the control input.
  const controlSources = store.project.properties.control_sources ?? [];
  const feedOf = (name: string): number => {
    const source = controlSources.find((candidate) => candidate.name === name);
    if (!source) return -1;
    return tracks.findIndex((track) => track.name === source.source_track);
  };
  for (let i = 0; i < tracks.length; i++) {
    const outputs = controlSources
      .filter((source) => feedOf(source.name) === i)
      .map((source) => source.name);
    if (outputs.length > 0) {
      nodes.find((node) => node.id === `track:${i}`)!.controlOutputs = outputs;
    }
  }
  const collectControlRoutes = (
    ownerId: string,
    effects: Effect[],
    routes: { kind: string; source: string; target: string }[] | undefined,
    trackIndex?: number,
  ) => {
    for (const route of routes ?? []) {
      if (route.kind !== "control") continue;
      const feed = feedOf(route.source);
      if (feed < 0) continue;
      const index = resolveEffectIndex(effects, route.target);
      const effect = index >= 0 ? effects[index] : undefined;
      edges.push({
        kind: "control",
        target: route.target,
        trackIndex,
        controlSource: route.source,
        controlFeed: feed,
        controlOwner: ownerId,
        controlDevice: effect ? effectLabel(effect.effect_type) : route.target,
        id: edgeId({
          kind: "control",
          target: route.target,
          trackIndex,
          controlSource: route.source,
          controlFeed: feed,
          controlOwner: ownerId,
        }),
        from: `track:${feed}`,
        to: ownerId,
      });
    }
  };
  for (let i = 0; i < tracks.length; i++) {
    collectControlRoutes(`track:${i}`, tracks[i].effects, tracks[i].input_routes, i);
  }
  for (const bus of buses) {
    collectControlRoutes(`bus:${bus.name}`, bus.effects, bus.input_routes);
  }
  collectControlRoutes("master", store.project.master?.effects ?? [], store.project.master?.input_routes);

  // Compute depth as longest path FROM this node TO master.
  // Master = 0, direct feeders to master = 1, their feeders = 2, etc.
  function longestPathToMaster(nodeId: string, memo: Map<string, number>, visiting?: Set<string>): number {
    if (memo.has(nodeId)) return memo.get(nodeId)!;
    if (nodeId === "master") { memo.set(nodeId, 0); return 0; }

    // Cycle detection: if we're already visiting this node, break the cycle
    const visited = visiting ?? new Set<string>();
    if (visited.has(nodeId)) { memo.set(nodeId, 1); return 1; }
    visited.add(nodeId);

    const outEdges = edges.filter(e => e.from === nodeId);
    if (outEdges.length === 0) {
      // Disconnected — treat as direct to master
      memo.set(nodeId, 1);
      return 1;
    }

    let maxD = 0;
    for (const e of outEdges) {
      const d = longestPathToMaster(e.to, memo, visited);
      if (d + 1 > maxD) maxD = d + 1;
    }
    memo.set(nodeId, maxD);
    return maxD;
  }

  const memo = new Map<string, number>();
  for (const node of nodes) {
    node.depth = longestPathToMaster(node.id, memo);
  }

  return { nodes, edges };
}

// ============================================================================
/** Resolve an effect address the way the DSL does: `fx2` or a type name. */
function resolveEffectIndex(effects: Effect[], target: string): number {
  const match = /^fx(\d+)$/.exec(target);
  if (match) return Number(match[1]) - 1;
  return effects.findIndex((effect) => effectDeviceId(effect.effect_type) === target);
}

/** Fader position 0..1 for a level in dB — same law as the inspector fader. */
function levelToPos(db: number): number {
  if (db <= -60) return 0;
  if (db >= 12) return 1;
  return db <= 0 ? ((db + 60) / 60) * 0.75 : 0.75 + (db / 12) * 0.25;
}

// Node summary (accessible name)
// ============================================================================

/** Node geometry in CSS pixels; the SVG renders at 1 user unit = 1 px. */
export const NODE_W = 172;
export const NODE_H = 30;
export const ROW_GAP = 6;
export const COL_GAP = 56;
export const PADDING = 8;

const FX_MAX = 5;
const FX_STEP = 13;

/** Pointer travel, in px, that turns a press into a pan instead of a click. */
const PAN_THRESHOLD = 3;

/** Invisible stroke that makes a 1px edge curve clickable and tabbable. It is
 * kept at 12px so the hit target stays comfortable after the SVG is scaled. */
export const EDGE_HIT_WIDTH = 12;

function nodePreview(node: GraphNode): string {
  const lines: string[] = [node.label];
  if (node.kind === "track") {
    lines.push(`Vol: ${(node.volumeDb ?? 0) > 0 ? "+" : ""}${(node.volumeDb ?? 0).toFixed(1)}dB`);
    lines.push(`Pan: ${Math.round((node.pan ?? 0) * 100)}`);
    if (node.mute) lines.push("[MUTED]");
    if (node.solo) lines.push("[SOLO]");
    if (node.sends && node.sends.length > 0) {
      lines.push(`Sends: ${node.sends.map(s => `${s.target} (${Math.round(s.amount * 100)}%)`).join(", ")}`);
    }
  }
  if (node.effects.length > 0) {
    lines.push(`FX: ${node.effects.map(e => effectLabel(e.effect_type)).join(" → ")}`);
  }
  if (node.kind === "bus" && node.routes) {
    lines.push(`Routes: ${node.routes.join(", ")}`);
  }
  return lines.join("\n");
}

/** Accessible name for one edge, e.g. "Drums: send to reverb_bus". */
function edgePreview(edge: GraphEdge): string {
  if (edge.kind === "control") {
    return i18n.t("routing.controlEdgeLabel", {
      source: edge.controlSource ?? "",
      target: edge.controlDevice ?? edge.target,
    });
  }
  const track = edge.trackIndex === undefined ? undefined : store.project.tracks[edge.trackIndex];
  const source = edge.kind === "route"
    ? `Bus ${edge.bus}`
    : track?.mix.overrides?.display_name || track?.name || `Track ${edge.trackIndex}`;
  const what = edge.kind === "main"
    ? "main output"
    : edge.kind === "send"
      ? (edge.source ? `auxiliary output ${edge.source.owner}.out${edge.source.output}` : "send")
      : "route";
  const level = edge.amount !== undefined ? ` at ${Math.round(edge.amount * 100)}%` : "";
  return `${source}: ${what} to ${edge.target}${level}`;
}

// ============================================================================
// Component
// ============================================================================

export function BusGraph(props: {
  selectedNode: string | null;
  onSelectNode: (id: string) => void;
  /** Edge selection is optional so the graph can stay a pure viewer. */
  selectedEdge?: RoutingEdge | null;
  onSelectEdge?: (edge: RoutingEdge | null) => void;
}) {
  const [hoveredNode, setHoveredNode] = createSignal<string | null>(null);
  const [hoveredEdge, setHoveredEdge] = createSignal<string | null>(null);
  const [devices, setDevices] = createSignal<DeviceDef[]>(FALLBACK_DEVICES);
  const [status, setStatus] = createSignal<{ tone: "info" | "error"; text: string } | null>(null);
  const [armed, setArmed] = createSignal<{ feed: number; source: string } | null>(null);
  const [dragging, setDragging] = createSignal<{
    feed: number; source: string; x1: number; y1: number; x2: number; y2: number;
  } | null>(null);
  let dragPointer: { pointerId: number; feed: number; source: string } | null = null;
  let svgRef: SVGSVGElement | null = null;

  onMount(() => {
    void loadDevicesCatalog().then(setDevices);
  });

  const isSelectedEdge = (edge: GraphEdge) =>
    props.selectedEdge !== undefined && props.selectedEdge !== null
    && edgeId(props.selectedEdge) === edge.id;

  // Drag-to-pan. A press on the background grabs the graph and the pointer
  // delta scrolls the panel; pointer capture keeps the grab if it leaves.
  let graphRef: HTMLDivElement | null = null;
  let pan: { pointerId: number; x: number; y: number; left: number; top: number; moved: boolean } | null = null;
  // Set when a pan moved, so the click that ends it never selects a node.
  let suppressClick = false;
  const [panning, setPanning] = createSignal(false);

  const movePan = (e: PointerEvent) => {
    if (pan === null || e.pointerId !== pan.pointerId || graphRef === null) return;
    const dx = e.clientX - pan.x;
    const dy = e.clientY - pan.y;
    if (!pan.moved && Math.abs(dx) + Math.abs(dy) >= PAN_THRESHOLD) pan.moved = true;
    // Below the threshold the press is still a click, so it must not nudge the
    // view either; a 2px wobble should never scroll the graph.
    if (!pan.moved) return;
    const { left, top } = pan;
    graphRef.scrollLeft = left - dx;
    graphRef.scrollTop = top - dy;
  };

  const detachPan = () => {
    document.removeEventListener("pointermove", movePan);
    document.removeEventListener("pointerup", endPan);
    document.removeEventListener("pointercancel", endPan);
  };

  const endPan = (e: PointerEvent) => {
    if (pan === null || e.pointerId !== pan.pointerId) return;
    try { graphRef?.releasePointerCapture?.(e.pointerId); } catch { /* not supported */ }
    detachPan();
    suppressClick = pan.moved;
    pan = null;
    setPanning(false);
  };

  const startPan = (e: PointerEvent) => {
    suppressClick = false; // a fresh press re-arms node selection
    if (e.button !== 0 || graphRef === null) return;
    const target = e.target as Element | null;
    if (target?.closest?.(".bus-graph-node") || target?.closest?.(".bus-graph-edge")) return;
    pan = {
      pointerId: e.pointerId,
      x: e.clientX, y: e.clientY,
      left: graphRef.scrollLeft, top: graphRef.scrollTop,
      moved: false,
    };
    try { graphRef.setPointerCapture?.(e.pointerId); } catch { /* not supported */ }
    document.addEventListener("pointermove", movePan);
    document.addEventListener("pointerup", endPan);
    document.addEventListener("pointercancel", endPan);
    setPanning(true);
  };

  onCleanup(detachPan);

  const graph = createMemo(() => buildGraph(
    store.project.tracks,
    store.project.buses,
    store.project.master !== null,
  ));

  // Group nodes into fixed layers: tracks (top), buses (middle), master (bottom)
  // This avoids the confusing mixed-depth rows from the original depth-based layout.
  const rows = createMemo(() => {
    const g = graph();
    const tracks = g.nodes.filter(n => n.kind === "track");
    const buses = g.nodes.filter(n => n.kind === "bus");
    const master = g.nodes.filter(n => n.kind === "master");

    // Sort tracks to reduce edge crossings: group by primary send target,
    // then within each group sort by original index.
    // Tracks with no sends (direct to master) go in the center.
    const busOrder = buses.map(b => b.id);
    tracks.sort((a, b) => {
      const aEdges = g.edges.filter(e => e.from === a.id && e.to !== "master");
      const bEdges = g.edges.filter(e => e.from === b.id && e.to !== "master");
      const aBus = aEdges.length > 0 ? busOrder.indexOf(aEdges[0].to) : busOrder.length;
      const bBus = bEdges.length > 0 ? busOrder.indexOf(bEdges[0].to) : busOrder.length;
      if (aBus !== bBus) return aBus - bBus;
      // Same bus group — preserve original track order
      const ai = parseInt(a.id.split(":")[1]) || 0;
      const bi = parseInt(b.id.split(":")[1]) || 0;
      return ai - bi;
    });

    // Sort buses by median position of their incoming tracks (reduce crossings)
    buses.sort((a, b) => {
      const aIncoming = tracks.filter(t => g.edges.some(e => e.from === t.id && e.to === a.id));
      const bIncoming = tracks.filter(t => g.edges.some(e => e.from === t.id && e.to === b.id));
      const median = (arr: typeof tracks) => {
        if (arr.length === 0) return tracks.length / 2;
        const indices = arr.map(t => tracks.indexOf(t));
        indices.sort((x, y) => x - y);
        return indices[Math.floor(indices.length / 2)];
      };
      return median(aIncoming) - median(bIncoming);
    });

    const result: GraphNode[][] = [];
    if (tracks.length > 0) result.push(tracks);
    if (buses.length > 0) result.push(buses);
    if (master.length > 0) result.push(master);
    return result;
  });

  // Vertical signal flow, left → right: tracks | buses | master. Each column
  // stacks compact thumbnails, so a dozen tracks fit in the panel height and
  // the routing reads like a console. Details live in the inspector (click).

  const nodePositions = createMemo(() => {
    const r = rows();
    const g = graph();
    const positions = new Map<string, { x: number; y: number }>();
    const colHeight = (n: number) => n * NODE_H + Math.max(0, n - 1) * ROW_GAP;
    const tallest = Math.max(...r.map((col) => colHeight(col.length)), NODE_H);
    r.forEach((col, ci) => {
      const x = PADDING + ci * (NODE_W + COL_GAP);
      if (ci === 0) {
        col.forEach((node, i) => positions.set(node.id, { x, y: PADDING + i * (NODE_H + ROW_GAP) }));
        return;
      }
      // Later columns sit at the mean height of what feeds them, then are
      // pushed apart so they never overlap.
      const wanted = col.map((node) => {
        const feeders = g.edges.filter((e) => e.to === node.id).map((e) => positions.get(e.from)).filter(Boolean);
        return feeders.length > 0
          ? feeders.reduce((sum, p) => sum + p!.y, 0) / feeders.length
          : PADDING + (tallest - NODE_H) / 2;
      });
      let next = PADDING;
      col.forEach((node, i) => {
        const y = Math.max(next, Math.min(wanted[i], PADDING + tallest - NODE_H));
        positions.set(node.id, { x, y });
        next = y + NODE_H + ROW_GAP;
      });
    });
    return positions;
  });

  const svgHeight = createMemo(() => {
    let max = 0;
    for (const p of nodePositions().values()) max = Math.max(max, p.y + NODE_H);
    return max + PADDING;
  });

  const svgWidth = createMemo(() => {
    const cols = Math.max(rows().length, 1);
    return PADDING * 2 + cols * NODE_W + (cols - 1) * COL_GAP;
  });

  // ==========================================================================
  // Control cables
  // ==========================================================================

  const nodeEffects = (nodeId: string): Effect[] => {
    if (nodeId === "master") return store.project.master?.effects ?? [];
    if (nodeId.startsWith("bus:")) {
      return store.project.buses.find((bus) => `bus:${bus.name}` === nodeId)?.effects ?? [];
    }
    const index = Number(nodeId.slice(6));
    return store.project.tracks[index]?.effects ?? [];
  };

  const nodeLabel = (nodeId: string): string => {
    if (nodeId === "master") return i18n.t("mixer.master");
    if (nodeId.startsWith("bus:")) return nodeId.slice(4);
    const track = store.project.tracks[Number(nodeId.slice(6))];
    return track?.mix.overrides?.display_name || track?.name || nodeId;
  };

  /** The first effect in this node whose device declares a control input. */
  const controlInput = (nodeId: string): { index: number; device: DeviceDef } | null => {
    for (const [index, effect] of nodeEffects(nodeId).entries()) {
      const id = effectDeviceId(effect.effect_type);
      const device = id ? findDevice(devices(), id) : undefined;
      if (device?.ports?.some((port) => port.kind === "control")) return { index, device };
    }
    return null;
  };

  /** Sockets exposed by tracks that feed a control source. */
  const controlSockets = createMemo(() => {
    const positions = nodePositions();
    const sockets: { nodeId: string; feed: number; source: string; x: number; y: number }[] = [];
    for (const node of graph().nodes) {
      const outputs = node.controlOutputs;
      const position = positions.get(node.id);
      if (!outputs || !position) continue;
      const feed = Number(node.id.slice(6));
      outputs.forEach((source, index) => {
        sockets.push({
          nodeId: node.id,
          feed,
          source,
          x: position.x + NODE_W + 5,
          y: position.y + NODE_H / 2 + (index - (outputs.length - 1) / 2) * 12,
        });
      });
    }
    return sockets;
  });

  const connectControl = async (feed: number, source: string, targetId: string) => {
    setArmed(null);
    if (targetId === `track:${feed}`) {
      setStatus({
        tone: "error",
        text: i18n.t("routing.controlSelf", { node: nodeLabel(targetId) }),
      });
      return;
    }
    const input = controlInput(targetId);
    if (!input) {
      setStatus({
        tone: "error",
        text: i18n.t("routing.controlRefused", { node: nodeLabel(targetId) }),
      });
      return;
    }
    const error = await store.addControlRoute(targetId, `fx${input.index + 1}`, source);
    if (error) {
      setStatus({ tone: "error", text: error });
      return;
    }
    setStatus({
      tone: "info",
      text: i18n.t("routing.controlConnected", { source, node: nodeLabel(targetId) }),
    });
  };

  const armControl = (feed: number, source: string) => {
    setArmed({ feed, source });
    setStatus({
      tone: "info",
      text: i18n.t("routing.controlArmHint", { source }),
    });
  };

  /** Pointer position in SVG user units. The SVG renders at its natural pixel
   * size, so this is the client delta scaled by the viewBox ratio; it stays
   * correct for a scaled panel and needs no layout in a headless test. */
  const svgPoint = (event: PointerEvent): { x: number; y: number } | null => {
    if (!svgRef) return null;
    const rect = svgRef.getBoundingClientRect();
    const width = svgWidth();
    const scale = rect.width > 0 ? width / rect.width : 1;
    return {
      x: (event.clientX - rect.left) * scale,
      y: (event.clientY - rect.top) * scale,
    };
  };

  /** The node under an SVG point, using the same geometry the render uses. */
  const nodeAt = (point: { x: number; y: number }): string | null => {
    const positions = nodePositions();
    for (const node of graph().nodes) {
      const position = positions.get(node.id);
      if (!position) continue;
      if (
        point.x >= position.x - 8 && point.x <= position.x + NODE_W + 10
        && point.y >= position.y - 2 && point.y <= position.y + NODE_H + 2
      ) {
        return node.id;
      }
    }
    return null;
  };

  const moveControlDrag = (event: PointerEvent) => {
    const cable = dragging();
    if (!cable || !dragPointer || event.pointerId !== dragPointer.pointerId) return;
    const point = svgPoint(event);
    if (!point) return;
    setDragging({ ...cable, x2: point.x, y2: point.y });
  };

  const detachControlDrag = () => {
    document.removeEventListener("pointermove", moveControlDrag);
    document.removeEventListener("pointerup", endControlDrag);
    document.removeEventListener("pointercancel", cancelControlDrag);
    dragPointer = null;
    setDragging(null);
  };

  const endControlDrag = (event: PointerEvent) => {
    if (!dragPointer || event.pointerId !== dragPointer.pointerId) return;
    const cable = dragging();
    const point = svgPoint(event) ?? (cable ? { x: cable.x2, y: cable.y2 } : null);
    detachControlDrag();
    if (!cable || !point) return;
    const targetId = nodeAt(point);
    if (targetId) void connectControl(cable.feed, cable.source, targetId);
  };

  const cancelControlDrag = () => {
    detachControlDrag();
  };

  const startControlDrag = (
    event: PointerEvent,
    feed: number,
    source: string,
    x: number,
    y: number,
  ) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    suppressClick = true;
    dragPointer = { pointerId: event.pointerId, feed, source };
    setStatus(null);
    setDragging({ feed, source, x1: x, y1: y, x2: x, y2: y });
    document.addEventListener("pointermove", moveControlDrag);
    document.addEventListener("pointerup", endControlDrag);
    document.addEventListener("pointercancel", cancelControlDrag);
  };

  onCleanup(detachControlDrag);

  return (
    <div
      class="bus-graph"
      classList={{ panning: panning() }}
      ref={(el) => { graphRef = el; }}
      tabIndex={0}
      role="group"
      aria-label={i18n.t("mixer.routingGraph")}
      onPointerDown={startPan}
      onKeyDown={(e) => {
        if (e.key === "Escape" && (armed() || dragging())) {
          e.preventDefault();
          setArmed(null);
          setStatus(null);
          detachControlDrag();
        }
      }}
      onClick={(e) => {
        if (suppressClick) { suppressClick = false; return; }
        const target = e.target as Element | null;
        if (target?.closest?.(".bus-graph-node") || target?.closest?.(".bus-graph-edge")) return;
        props.onSelectEdge?.(null);
        setArmed(null);
        setStatus(null);
      }}
    >
      <Show when={status()}>
        {(message) => (
          <div
            class="bus-graph-status"
            classList={{ error: message().tone === "error" }}
            role={message().tone === "error" ? "alert" : "status"}
            aria-live={message().tone === "error" ? "assertive" : "polite"}
          >{message().text}</div>
        )}
      </Show>
      <svg
        class="bus-graph-svg"
        ref={(element) => { svgRef = element; }}
        viewBox={`0 0 ${svgWidth()} ${svgHeight()}`}
        width={svgWidth()}
        height={svgHeight()}
        preserveAspectRatio="xMinYMin meet"
        role="group"
        aria-label={i18n.t("mixer.routingGraph")}
      >
        {/* Edges — horizontal curves from a node's right edge to its target's left edge */}
        <For each={graph().edges}>
          {(edge) => {
            const key = edge.id;
            const fromPos = () => nodePositions().get(edge.from);
            const toPos = () => nodePositions().get(edge.to);
            const geometry = () => {
              const fp = fromPos();
              const tp = toPos();
              if (!fp || !tp) return null;
              const x1 = fp.x + NODE_W;
              const y1 = fp.y + NODE_H / 2;
              // Two tracks share a column: a control cable between them loops
              // out to the right so it never runs through the node stack.
              const sameColumn = fp.x === tp.x;
              const x2 = sameColumn ? tp.x + NODE_W + 10 : tp.x;
              const y2 = tp.y + NODE_H / 2;
              const dx = Math.max(24, Math.abs(x2 - x1) * 0.5);
              return { x1, y1, x2, y2, d: `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}` };
            };
            const selected = () => isSelectedEdge(edge);
            const focused = () =>
              selected() || props.selectedNode === edge.from || props.selectedNode === edge.to ||
              hoveredEdge() === key || hoveredNode() === edge.from || hoveredNode() === edge.to;
            // Percentages only for the focused route: labelling every send at
            // once stacked unreadable numbers on top of each other.
            const showAmount = () => edge.amount !== undefined && edge.amount < 1 && focused();
            return (
              <Show when={geometry()}>
                {(geo) => (
                  <g
                    class="bus-graph-edge"
                    classList={{
                      send: edge.amount !== undefined,
                      control: edge.kind === "control",
                      active: focused(),
                      selected: selected(),
                    }}
                    data-edge={edge.id}
                    onMouseEnter={() => setHoveredEdge(key)}
                    onMouseLeave={() => setHoveredEdge(null)}
                  >
                    <path
                      d={geo().d}
                      fill="none"
                      stroke={edge.kind === "control"
                        ? "var(--accent-cool)"
                        : edge.amount !== undefined || focused() ? "var(--accent)" : "var(--text-muted)"}
                      stroke-opacity={selected() ? 1 : focused() ? 0.95 : edge.kind === "control" ? 0.9 : edge.amount !== undefined ? 0.3 + edge.amount * 0.4 : 0.35}
                      stroke-width={edge.kind === "control" ? (selected() ? 2.6 : 1.8) : selected() ? 2.6 : focused() ? 1.8 : edge.amount !== undefined ? 1.1 + edge.amount * 1.1 : 1}
                      stroke-dasharray={edge.kind === "control" ? "2,3" : edge.amount !== undefined ? "4,3" : "none"}
                    />
                    {/* A control cable ends in a diamond so its kind survives a
                        grayscale or colour-blind rendering. */}
                    <Show when={edge.kind === "control"}>
                      <rect
                        x={geo().x2 - 4}
                        y={geo().y2 - 4}
                        width={8}
                        height={8}
                        transform={`rotate(45 ${geo().x2} ${geo().y2})`}
                        fill="var(--accent-cool)"
                      />
                    </Show>
                    <Show when={showAmount()}>
                      <text
                        x={geo().x2 - 6}
                        y={geo().y2 - 5}
                        text-anchor="end"
                        font-size="9.5"
                        fill="var(--text-primary)"
                        font-family="var(--font-mono)"
                        stroke="var(--bg-primary)"
                        stroke-width={3}
                        paint-order="stroke"
                      >
                        {Math.round((edge.amount ?? 1) * 100)}%
                      </text>
                    </Show>
                    {/* Invisible wide stroke: the click/keyboard target for the
                        1px curve. Selecting an edge never starts a pan. */}
                    <path
                      class="bus-graph-edge-hit"
                      data-edge-hit={edge.id}
                      d={geo().d}
                      fill="none"
                      stroke="transparent"
                      stroke-width={EDGE_HIT_WIDTH}
                      stroke-dasharray="none"
                      role="button"
                      tabIndex={0}
                      aria-pressed={selected()}
                      aria-label={edgePreview(edge)}
                      style={{ "pointer-events": "stroke", cursor: "pointer" }}
                      onClick={(e) => {
                        e.stopPropagation();
                        if (suppressClick) { suppressClick = false; return; }
                        props.onSelectEdge?.(edge);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          props.onSelectEdge?.(edge);
                        }
                      }}
                    />
                  </g>
                )}
              </Show>
            );
          }}
        </For>

        {/* Nodes — thumbnails; click selects, the inspector shows the details */}
        <For each={graph().nodes}>
          {(node) => {
            const pos = () => nodePositions().get(node.id);
            const isSelected = () => props.selectedNode === node.id;
            const isHovered = () => hoveredNode() === node.id;
            const trackIndex = () => node.kind === "track" ? parseInt(node.id.split(":")[1]) || 0 : -1;
            const badge = () => node.mute ? "M" : node.solo ? "S" : null;
            const level = () => node.kind === "track" ? node.volumeDb ?? 0
              : node.kind === "bus" ? store.project.buses.find((b) => `bus:${b.name}` === node.id)?.volume_db ?? 0
              : store.project.master?.volume_db ?? 0;
            const sendCount = () => node.sends?.length ?? 0;

            const fill = () => {
              if (isSelected()) return "var(--bg-track-selected)";
              if (isHovered()) return "var(--bg-elevated)";
              switch (node.kind) {
                case "track": return "var(--bg-surface)";
                case "bus": return "color-mix(in srgb, var(--bg-surface) 82%, var(--accent-cool) 18%)";
                case "master": return "color-mix(in srgb, var(--bg-surface) 80%, var(--accent) 20%)";
              }
            };
            const stroke = () => {
              if (isSelected()) return "var(--accent)";
              if (node.solo) return "var(--accent-cool)";
              switch (node.kind) {
                case "track": return "var(--border-light)";
                case "bus": return "var(--accent-cool)";
                case "master": return "var(--accent)";
              }
            };
            const railColor = () => node.kind === "track" ? `var(--track-color-${trackIndex() % 8})`
              : node.kind === "bus" ? "var(--accent-cool)" : "var(--accent)";
            const label = () => node.label.length > 18 ? node.label.slice(0, 17) + "…" : node.label;
            const activate = () => {
              const pending = armed();
              if (pending) {
                void connectControl(pending.feed, pending.source, node.id);
                return;
              }
              props.onSelectNode(node.id);
            };
            const handleClick = () => {
              if (suppressClick) { suppressClick = false; return; }
              activate();
            };

            return (
              <Show when={pos()}>
                {(p) => {
                  const meterX = () => p().x + 10;
                  const meterW = NODE_W - 26 - FX_STEP * Math.min(node.effects.length, FX_MAX) - (node.effects.length > FX_MAX ? 12 : 0);
                  const meterY = () => p().y + NODE_H - 7;
                  return (
                    <g
                      class="bus-graph-node"
                      classList={{ muted: !!node.mute, selected: isSelected() }}
                      data-node={node.id}
                      role="button"
                      tabIndex={0}
                      aria-pressed={isSelected()}
                      aria-label={nodePreview(node).replaceAll("\n", ", ")}
                      style={{ cursor: "pointer" }}
                      onMouseEnter={() => setHoveredNode(node.id)}
                      onMouseLeave={() => setHoveredNode(null)}
                      onClick={handleClick}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") { e.preventDefault(); activate(); }
                      }}
                    >
                      <rect
                        x={p().x}
                        y={p().y}
                        width={NODE_W}
                        height={NODE_H}
                        rx={5}
                        fill={fill()}
                        stroke={stroke()}
                        stroke-width={isSelected() ? 2 : 1}
                        stroke-dasharray={node.mute ? "3,3" : "none"}
                        opacity={node.mute ? 0.4 : 1}
                      />
                      {/* Data rail: track colour / bus / master */}
                      <line
                        class="bus-graph-rail"
                        x1={p().x + 3.5} y1={p().y + 6} x2={p().x + 3.5} y2={p().y + NODE_H - 6}
                        stroke={railColor()} stroke-width={3} stroke-linecap="round"
                        opacity={node.mute ? 0.4 : 1}
                      />
                      {/* Name */}
                      <text
                        x={p().x + 10}
                        y={p().y + 13}
                        font-size="11"
                        font-weight={isSelected() ? "700" : "600"}
                        fill="var(--text-primary)"
                        opacity={node.mute ? 0.55 : 1}
                        font-family="var(--font-ui)"
                      >{label()}</text>
                      {/* Level readout, right-aligned */}
                      <text
                        x={p().x + NODE_W - 8}
                        y={p().y + 13}
                        text-anchor="end"
                        font-size="9"
                        fill="var(--text-secondary)"
                        font-family="var(--font-mono)"
                      >{`${level() > 0 ? "+" : ""}${level().toFixed(1)}dB`}</text>
                      {/* Fader thumbnail: track + fill to the fader position */}
                      <line
                        class="bus-graph-meter"
                        x1={meterX()} y1={meterY()} x2={meterX() + meterW} y2={meterY()}
                        stroke="var(--fader-track)" stroke-width={3} stroke-linecap="round"
                      />
                      <line
                        class="bus-graph-meter-fill"
                        x1={meterX()} y1={meterY()}
                        x2={meterX() + Math.max(1, meterW * levelToPos(level()))} y2={meterY()}
                        stroke={node.mute ? "var(--text-muted)" : "var(--fader-fill)"}
                        stroke-width={3} stroke-linecap="round"
                      />
                      {/* Pan tick (tracks) */}
                      <Show when={node.kind === "track"}>
                        <line
                          class="bus-graph-pan"
                          x1={meterX() + meterW / 2 + (node.pan ?? 0) * (meterW / 2)} y1={meterY() - 4}
                          x2={meterX() + meterW / 2 + (node.pan ?? 0) * (meterW / 2)} y2={meterY() + 4}
                          stroke="var(--text-primary)" stroke-width={1.5}
                        />
                      </Show>
                      {/* Effect chain thumbnail: one glyph per device, in signal order */}
                      <g class="bus-graph-fx" style={{ color: "var(--accent)" }}>
                        <For each={node.effects.slice(0, FX_MAX)}>
                          {(effect, i) => (
                            <EffectGlyph
                              type={effect.effect_type}
                              x={p().x + NODE_W - 12 - FX_STEP * (Math.min(node.effects.length, FX_MAX) - i()) - (sendCount() > 0 ? 14 : 0)}
                              y={meterY() - 5}
                              size={10}
                            />
                          )}
                        </For>
                        <Show when={node.effects.length > FX_MAX}>
                          <text
                            x={p().x + NODE_W - 10 - (sendCount() > 0 ? 14 : 0)}
                            y={meterY() + 3}
                            font-size="8"
                            fill="var(--text-secondary)"
                            font-family="var(--font-mono)"
                          >{`+${node.effects.length - FX_MAX}`}</text>
                        </Show>
                      </g>
                      {/* Send count (tracks) */}
                      <Show when={sendCount() > 0}>
                        <text
                          x={p().x + NODE_W - 8}
                          y={meterY() - 5}
                          text-anchor="end"
                          font-size="8.5"
                          fill="var(--accent)"
                          font-family="var(--font-mono)"
                        >{`↗${sendCount()}`}</text>
                      </Show>
                      {/* Mute / solo badge on the top-left corner */}
                      <Show when={badge()}>
                        <circle
                          class="bus-graph-badge"
                          cx={p().x + NODE_W - 4}
                          cy={p().y + 1}
                          r={6.5}
                          fill={node.mute ? "var(--meter-yellow)" : "var(--accent-cool)"}
                        />
                        <text
                          x={p().x + NODE_W - 4}
                          y={p().y + 4}
                          text-anchor="middle"
                          font-size="8.5"
                          font-weight="700"
                          font-family="var(--font-mono)"
                          fill="var(--bg-void)"
                        >{badge()}</text>
                      </Show>
                    </g>
                  );
                }}
              </Show>
            );
          }}
        </For>

        {/* Control outputs: one socket per @control_source fed by a track. */}
        <For each={controlSockets()}>
          {(socket) => (
            <g
              class="bus-graph-control-socket"
              classList={{ armed: armed()?.source === socket.source }}
              data-control-socket={`${socket.feed}:${socket.source}`}
              data-control-source={socket.source}
              role="button"
              tabIndex={0}
              aria-label={i18n.t("routing.controlSocket", { source: socket.source })}
              transform={`translate(${socket.x} ${socket.y})`}
              onPointerDown={(event) =>
                startControlDrag(event, socket.feed, socket.source, socket.x, socket.y)}
              onClick={(event) => {
                event.stopPropagation();
                if (suppressClick) { suppressClick = false; return; }
                armControl(socket.feed, socket.source);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  event.stopPropagation();
                  armControl(socket.feed, socket.source);
                }
              }}
            >
              <title>{i18n.t("routing.controlSocket", { source: socket.source })}</title>
              <rect
                x={-4.5}
                y={-4.5}
                width={9}
                height={9}
                transform="rotate(45)"
                fill="var(--bg-primary)"
                stroke="var(--accent-cool)"
                stroke-width={1.6}
              />
            </g>
          )}
        </For>

        {/* The cable follows the pointer until it is dropped. */}
        <Show when={dragging()}>
          {(cable) => (
            <path
              class="bus-graph-control-cable"
              d={`M ${cable().x1} ${cable().y1} C ${cable().x1 + 24} ${cable().y1}, ${cable().x2 - 24} ${cable().y2}, ${cable().x2} ${cable().y2}`}
              fill="none"
              stroke="var(--accent-cool)"
              stroke-width={1.8}
              stroke-dasharray="2,3"
              pointer-events="none"
            />
          )}
        </Show>
      </svg>
    </div>
  );
}
