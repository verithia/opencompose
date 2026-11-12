import { Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { BusGraph, edgeSourceNode, type RoutingEdge } from "./BusGraph";
import { RoutingToolbar } from "./RoutingToolbar";
import { ChannelDetail } from "./ChannelStrips";
import { NodeMixer } from "./NodeMixer";
import { BusActions } from "./BusActions";
import { ConnectionDetail, removeConnection } from "./ConnectionDetail";
import { i18n } from "../stores/i18n";
import { Dropdown } from "./Dropdown";

export function Mixer() {
  const selectedTrack = store.selectedTrackIndex();
  const initialNode = selectedTrack !== null && selectedTrack < store.project.tracks.length
    ? `track:${selectedTrack}`
    : store.project.tracks.length > 0 ? "track:0" : "master";
  const [selectedNode, setSelectedNode] = createSignal<string | null>(initialNode);
  /** The connection the inspector edits; independent of the node detail. */
  const [selectedEdge, setSelectedEdge] = createSignal<RoutingEdge | null>(null);

  const selectedBus = createMemo(() => selectedNode()?.startsWith("bus:")
    ? store.project.buses.find((bus) => `bus:${bus.name}` === selectedNode()) ?? null
    : null);

  const selectNode = (id: string | null) => {
    setSelectedNode(id);
    setSelectedEdge(null);
  };

  const selectEdge = (edge: RoutingEdge | null) => {
    setSelectedEdge(edge);
    if (edge) setSelectedNode(edgeSourceNode(edge));
  };

  /** Keyboard focus lands on the affected graph node, or the graph itself when
   * the node is gone or not focusable (SVG focus support varies). */
  const focusNode = (id: string | null) => {
    const node = id ? document.querySelector<Element>(`[data-node="${id}"]`) : null;
    const focus = node && (node as { focus?: () => void }).focus;
    if (typeof focus === "function") {
      focus.call(node);
      if (document.activeElement === node) return;
    }
    document.querySelector<HTMLElement>(".bus-graph")?.focus();
  };

  const closeEdge = (edge: RoutingEdge) => {
    setSelectedEdge(null);
    focusNode(edgeSourceNode(edge));
  };

  // Escape leaves connection editing; Delete removes the selected connection.
  // Both work wherever focus is inside the mixer, but never while typing into
  // a field (except Escape, which is the way out of a field).
  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const edge = selectedEdge();
      if (!edge) return;
      const typing = Boolean(
        (event.target as Element | null)?.closest?.(
          "input, textarea, select, [contenteditable='true']",
        ),
      );
      if (event.key === "Escape") {
        event.preventDefault();
        closeEdge(edge);
        return;
      }
      if (typing) return;
      if (event.key === "Delete" || event.key === "Backspace") {
        event.preventDefault();
        void removeConnection(edge).then((removed) => {
          if (removed) closeEdge(edge);
        });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  // A delete performed outside the inspector (bus context menu) leaves the
  // inspector pointing at a node that no longer exists; fall back to master.
  createEffect(() => {
    const id = selectedNode();
    if (!id || id === "master") return;
    if (id.startsWith("track:")) {
      if (Number(id.slice(6)) >= store.project.tracks.length) setSelectedNode("master");
      return;
    }
    if (id.startsWith("bus:") && !store.project.buses.some((bus) => `bus:${bus.name}` === id)) {
      setSelectedNode("master");
    }
  });

  return (
    <div
      class="mixer-panel"
      onDragOver={(e) => {
        if (e.dataTransfer?.types.includes("application/x-preset")) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
        }
      }}
      onDrop={async (e) => {
        const presetName = e.dataTransfer?.getData("application/x-preset");
        if (presetName) {
          e.preventDefault();
          const trackName = presetName.replace(/\s+\d+$/, "");
          const preset = store.project.presets.find(p => p.name === presetName);
          const isDrums = preset?.source === "soundfont" && preset?.program === 0;
          const instCat = isDrums ? "drums" : preset?.source === "clap" ? "synth" : "keys";
          const instName = isDrums ? "acoustic_kit" : preset?.source === "clap" ? `clap_${presetName}` : "piano";
          const newIdx = await store.addTrack(trackName, instCat, instName);
          if (newIdx >= 0) {
            await store.setTrackPreset(newIdx, presetName);
          }
        }
      }}
    >
      <aside class="mixer-inspector">
      <div class="mixer-channel-controls">
        <Dropdown
          label={i18n.t("mixer.channel")}
          stack
          class="mixer-channel-select"
          value={selectedNode() ?? "master"}
          options={[
            { value: "master", label: i18n.t("mixer.master") },
            ...store.project.tracks.map((track, index) => ({ value: `track:${index}`, label: track.name })),
            ...store.project.buses.map((bus) => ({ value: `bus:${bus.name}`, label: i18n.t("mixer.busOption", { name: bus.name }) })),
          ]}
          onSelect={selectNode}
        />
      </div>

      {/* Vertical channel inspector: everything about the selected node */}
      <Show when={selectedNode()}>
        <NodeMixer nodeId={selectedNode()!} />
        <Show when={selectedEdge()}>
          {(edge) => (
            <ConnectionDetail
              edge={edge()}
              onSelectEdge={selectEdge}
              onClose={(nodeId) => {
                setSelectedEdge(null);
                focusNode(nodeId);
              }}
            />
          )}
        </Show>
        <ChannelDetail nodeId={selectedNode()!} />
        <Show when={selectedBus()}>
          {(bus) => (
            <BusActions
              busName={bus().name}
              onDeleted={() => {
                selectNode("master");
                focusNode("master");
              }}
            />
          )}
        </Show>
      </Show>
      </aside>

      {/* Routing graph: the toolbar creates buses; the inspector edits nodes
          and connections. Clicking a node (or an edge) drives the inspector. */}
      <div class="mixer-routing-workspace">
        <RoutingToolbar onSelectNode={selectNode} />
        <div class="mixer-graph-area">
          <Show when={store.project.tracks.length === 0}>
            <div class="empty-state">
              <div class="empty-state-text">{i18n.t("timeline.noTracks")}</div>
              <div class="empty-state-hint">{i18n.t("timeline.dragPresetHint")}</div>
            </div>
          </Show>
          <Show when={store.project.tracks.length > 0 || store.project.buses.length > 0}>
            <BusGraph
              selectedNode={selectedNode()}
              onSelectNode={selectNode}
              selectedEdge={selectedEdge()}
              onSelectEdge={selectEdge}
            />
          </Show>
        </div>
      </div>
    </div>
  );
}
