/**
 * BusGraph interaction tests.
 * Tests node rendering, click selection, hover, and edge drawing.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { readFileSync } from "node:fs";
import { invokeMock, makeSnapshot, makeTrack, makePattern } from "../setup";
import { store } from "../../stores/project";
import { BusGraph, NODE_W, NODE_H, ROW_GAP, COL_GAP, PADDING, type RoutingEdge } from "../../components/BusGraph";
import { NodeMixer } from "../../components/NodeMixer";

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    return makeSnapshot({
      tracks: [
        makeTrack("Piano", 0),
        { ...makeTrack("Drums", 1), mix: { volume_db: -3, pan: 0, sends: [{ target: "reverb_bus", amount: 0.5 }], mute: false, solo: false } },
        { ...makeTrack("Bass", 2), mix: { volume_db: 0, pan: 0, sends: [{ target: "reverb_bus", amount: 0.2 }], mute: true, solo: false } },
      ],
      buses: [
        { name: "reverb_bus", routes: ["master"], effects: [{ effect_type: "Reverb", params: [["mix", { Number: 0.3 }]] }] },
      ],
      patterns: [makePattern("beat")],
    });
  });
  await store.initProject();
});

describe("BusGraph", () => {
  function renderGraph() {
    const [selected, setSelected] = createSignal<string | null>(null);
    const result = render(() => (
      <BusGraph selectedNode={selected()} onSelectNode={setSelected} />
    ));
    return { ...result, selected, setSelected };
  }

  it("renders without crashing", () => {
    const { container } = renderGraph();
    expect(container.querySelector(".bus-graph")).toBeTruthy();
  });

  it("renders an SVG element", () => {
    const { container } = renderGraph();
    expect(container.querySelector("svg")).toBeTruthy();
  });

  it("renders correct number of nodes (3 tracks + 1 bus + 1 master = 5)", () => {
    const { container } = renderGraph();
    const rects = container.querySelectorAll("rect");
    expect(rects.length).toBe(5);
  });

  it("renders edges as paths", () => {
    const { container } = renderGraph();
    const paths = container.querySelectorAll("path");
    // Three direct track→master edges, two auxiliary sends, one bus→master.
    expect(paths.length).toBeGreaterThanOrEqual(6);
  });

  it("nodes with sends through bus are at higher depth than master", () => {
    const { container } = renderGraph();
    const svg = container.querySelector("svg")!;

    // Get Y positions of nodes by their rects
    const nodeYs: { label: string; y: number }[] = [];
    const groups = svg.querySelectorAll("g");
    for (const g of groups) {
      const rect = g.querySelector("rect");
      const text = g.querySelector("text");
      if (rect && text) {
        nodeYs.push({ label: text.textContent ?? "", y: parseFloat(rect.getAttribute("y") ?? "0") });
      }
    }

    // Find positions
    const masterNode = nodeYs.find(n => n.label === "Master");
    const drumsNode = nodeYs.find(n => n.label === "Drums");

    if (masterNode && drumsNode) {
      // Drums sends to bus which routes to master → depth 2
      // Master is depth 0 → higher y (lower on screen)
      expect(drumsNode.y).toBeLessThan(masterNode.y);
    }
  });

  it("clicking a node triggers onSelectNode", () => {
    const { container, selected } = renderGraph();
    const svg = container.querySelector("svg")!;
    const groups = svg.querySelectorAll("g");

    // Find Piano node and click it
    for (const g of groups) {
      if (g.textContent?.includes("Piano")) {
        fireEvent.click(g);
        break;
      }
    }
    expect(selected()).toBe("track:0");
  });

  it("clicking master node selects master", () => {
    const { container, selected } = renderGraph();
    const svg = container.querySelector("svg")!;
    const groups = svg.querySelectorAll("g");

    for (const g of groups) {
      if (g.textContent?.includes("Master")) {
        fireEvent.click(g);
        break;
      }
    }
    expect(selected()).toBe("master");
  });

  it("hover highlights a node but never opens a floating details box", () => {
    const { container } = renderGraph();
    const node = container.querySelector('[data-node="track:1"]')!;
    const rect = node.querySelector("rect")!;
    const before = rect.getAttribute("fill");
    fireEvent.mouseEnter(node);
    expect(rect.getAttribute("fill")).not.toBe(before);
    expect(container.querySelector("foreignObject")).toBeNull();
    fireEvent.mouseLeave(node);
    expect(rect.getAttribute("fill")).toBe(before);
  });

  it("lays tracks, buses and master out as left-to-right columns", () => {
    const { container } = renderGraph();
    const x = (id: string) => Number(container.querySelector(`[data-node="${id}"] rect`)!.getAttribute("x"));
    const y = (id: string) => Number(container.querySelector(`[data-node="${id}"] rect`)!.getAttribute("y"));
    expect(x("track:0")).toBe(x("track:2"));
    // Tracks stack vertically (grouped by send target, so order may differ).
    expect(new Set(["track:0", "track:1", "track:2"].map(y)).size).toBe(3);
    expect(x("track:0")).toBeLessThan(x("bus:reverb_bus"));
    expect(x("bus:reverb_bus")).toBeLessThan(x("master"));
  });

  it("selects a node from the keyboard", () => {
    const { container, selected } = renderGraph();
    fireEvent.keyDown(container.querySelector('[data-node="bus:reverb_bus"]')!, { key: "Enter" });
    expect(selected()).toBe("bus:reverb_bus");
  });

  it("muted node has reduced opacity", () => {
    const { container } = renderGraph();
    const svg = container.querySelector("svg")!;
    const groups = svg.querySelectorAll("g");

    // Bass is muted — find its rect
    for (const g of groups) {
      if (g.textContent?.includes("Bass")) {
        const rect = g.querySelector("rect");
        expect(rect?.getAttribute("opacity")).toBe("0.4");
        break;
      }
    }
  });

  it("send edges have proportional opacity/thickness", () => {
    const { container } = renderGraph();
    const svg = container.querySelector("svg")!;
    const paths = svg.querySelectorAll("path");

    // Find dashed paths (send edges)
    const dashedPaths = Array.from(paths).filter(p => p.getAttribute("stroke-dasharray") !== "none");
    expect(dashedPaths.length).toBeGreaterThan(0);

    // Send edges should have varying stroke-width based on amount
    const widths = dashedPaths.map(p => parseFloat(p.getAttribute("stroke-width") ?? "1"));
    // 0.5 amount → width = 1.5 + 0.5*1.5 = 2.25, 0.2 amount → width = 1.5 + 0.2*1.5 = 1.8
    expect(widths.some(w => w > 1.5)).toBe(true);
  });

  it("selected node has accent styling", () => {
    const [selected, setSelected] = createSignal<string | null>("track:0");
    const { container } = render(() => (
      <BusGraph selectedNode={selected()} onSelectNode={setSelected} />
    ));

    const svg = container.querySelector("svg")!;
    const groups = svg.querySelectorAll("g");

    for (const g of groups) {
      if (g.textContent?.includes("Piano")) {
        const rect = g.querySelector("rect");
        expect(rect?.getAttribute("stroke-width")).toBe("2");
        break;
      }
    }
  });

  it("bus node shows an effect-chain thumbnail with one glyph per device", () => {
    const { container } = renderGraph();
    const glyphs = container.querySelectorAll('[data-node="bus:reverb_bus"] .effect-glyph');
    expect(glyphs.length).toBe(1);
    expect(glyphs[0].getAttribute("data-effect")).toBe("Reverb");
    expect(glyphs[0].querySelector("title")?.textContent).toBe("Reverb");
    expect(container.querySelector('[data-node="track:0"] .effect-glyph')).toBeNull();
  });

  it("renders at the graph's natural scale, never the panel's", async () => {
    const columnHeight = (rows: number) => rows * NODE_H + Math.max(0, rows - 1) * ROW_GAP;
    // Two buses make three columns: tracks, buses, master.
    const expectedWidth = 3 * NODE_W + 2 * COL_GAP + PADDING * 2;
    const expectedHeight = Math.max(columnHeight(4), columnHeight(2), columnHeight(1)) + PADDING * 2;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      return makeSnapshot({
        tracks: ["Keys", "Guitar", "Vox", "Drum Bus"].map((name, trackIdx) => makeTrack(name, trackIdx)),
        buses: [
          { name: "reverb_bus", routes: ["master"], effects: [] },
          { name: "delay_bus", routes: ["master"], effects: [] },
        ],
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();

    const { container } = renderGraph();
    const svg = container.querySelector("svg")!;

    // A 644 × 154 graph is far smaller than the panel, but the SVG must not grow.
    expect(expectedWidth).toBe(644);
    expect(expectedHeight).toBe(154);
    expect(Number(svg.getAttribute("width"))).toBe(expectedWidth);
    expect(Number(svg.getAttribute("height"))).toBe(expectedHeight);
    expect(svg.getAttribute("viewBox")).toBe(`0 0 ${expectedWidth} ${expectedHeight}`);

    const nodes = svg.querySelectorAll(".bus-graph-node");
    expect(nodes.length).toBe(7);
    for (const node of nodes) {
      const rect = node.querySelector("rect")!;
      expect(Number(rect.getAttribute("width"))).toBe(172);
      expect(Number(rect.getAttribute("height"))).toBe(30);
    }
  });

  it("keeps the graph at its natural size in a scrollable, grabbable panel", () => {
    const mixerCss = readFileSync(`${process.cwd()}/src/styles/daw/mixer.css`, "utf8");
    const rules = new Map<string, string>();
    for (const match of mixerCss.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
      const selector = match[1].trim();
      if (selector === ".bus-graph" || selector === ".bus-graph-svg") rules.set(selector, match[2]);
      if (selector === ".bus-graph.panning") rules.set(selector, match[2]);
    }

    expect(rules.get(".bus-graph")).toMatch(/overflow:\s*auto/);
    expect(rules.get(".bus-graph")).toMatch(/cursor:\s*grab/);
    expect(rules.get(".bus-graph.panning")).toMatch(/cursor:\s*grabbing/);
    expect(rules.get(".bus-graph-svg")).not.toMatch(/width:\s*100%/);
    expect(rules.get(".bus-graph-svg")).not.toMatch(/height:\s*100%/);
    expect(rules.get(".bus-graph-svg")).not.toMatch(/max-width/);
  });

  describe("drag to pan", () => {
    const graphOf = (container: HTMLElement) => container.querySelector(".bus-graph") as HTMLDivElement;

    it("moves the graph by the pointer delta", () => {
      const { container } = renderGraph();
      const graph = graphOf(container);
      graph.scrollLeft = 120;
      graph.scrollTop = 40;

      fireEvent.pointerDown(graph, { button: 0, clientX: 300, clientY: 200, pointerId: 7 });
      expect(graph.classList.contains("panning")).toBe(true);
      fireEvent.pointerMove(document, { clientX: 260, clientY: 170, pointerId: 7, buttons: 1 });

      // Grab semantics: the content follows the pointer, so a leftward drag
      // reveals what was off-screen to the right.
      expect(graph.scrollLeft).toBe(160);
      expect(graph.scrollTop).toBe(70);

      fireEvent.pointerUp(document, { clientX: 260, clientY: 170, pointerId: 7, button: 0 });
      expect(graph.classList.contains("panning")).toBe(false);

      // Released: moving the pointer no longer pans.
      fireEvent.pointerMove(document, { clientX: 200, clientY: 100, pointerId: 7, buttons: 1 });
      expect(graph.scrollLeft).toBe(160);
      expect(graph.scrollTop).toBe(70);
    });

    it("keeps panning when the pointer leaves the graph", () => {
      const { container } = renderGraph();
      const graph = graphOf(container);

      fireEvent.pointerDown(graph, { button: 0, clientX: 300, clientY: 200, pointerId: 9 });
      // The pointer now sits outside the panel (and over another element); the
      // drag still owns it through pointer capture.
      fireEvent.pointerMove(document.body, { clientX: 100, clientY: 200, pointerId: 9, buttons: 1 });
      expect(graph.scrollLeft).toBe(200);
      fireEvent.pointerUp(document.body, { clientX: 100, clientY: 200, pointerId: 9, button: 0 });
    });

    it("does not pan when the drag starts on a node", () => {
      const { container } = renderGraph();
      const graph = graphOf(container);
      const node = container.querySelector('[data-node="track:1"]')!;

      fireEvent.pointerDown(node, { button: 0, clientX: 300, clientY: 200, pointerId: 3 });
      fireEvent.pointerMove(document, { clientX: 240, clientY: 180, pointerId: 3, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 240, clientY: 180, pointerId: 3, button: 0 });

      expect(graph.scrollLeft).toBe(0);
      expect(graph.scrollTop).toBe(0);
      expect(graph.classList.contains("panning")).toBe(false);
    });

    it("does not select a node when the click ends a drag", () => {
      const { container, selected } = renderGraph();
      const graph = graphOf(container);
      const node = container.querySelector('[data-node="track:0"]')!;

      fireEvent.pointerDown(graph, { button: 0, clientX: 300, clientY: 200, pointerId: 5 });
      fireEvent.pointerMove(document, { clientX: 280, clientY: 196, pointerId: 5, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 280, clientY: 196, pointerId: 5, button: 0 });
      // The pan ends over a node; the resulting click must not select it.
      fireEvent.click(node);
      expect(selected()).toBeNull();

      // The next press re-arms selection: a plain click still works.
      fireEvent.pointerDown(node, { button: 0, clientX: 300, clientY: 200, pointerId: 6 });
      fireEvent.click(node);
      expect(selected()).toBe("track:0");
    });

    it("leaves the node geometry untouched while panning", () => {
      const { container } = renderGraph();
      const graph = graphOf(container);
      const rectOf = () => {
        const rect = container.querySelector('[data-node="master"] rect')!;
        return ["x", "y", "width", "height"].map((a) => rect.getAttribute(a));
      };
      const before = rectOf();

      fireEvent.pointerDown(graph, { button: 0, clientX: 300, clientY: 200, pointerId: 11 });
      fireEvent.pointerMove(document, { clientX: 120, clientY: 90, pointerId: 11, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 120, clientY: 90, pointerId: 11, button: 0 });

      expect(graph.scrollLeft).toBe(180);
      expect(rectOf()).toEqual(before);
    });

    it("ignores a right-button press", () => {
      const { container } = renderGraph();
      const graph = graphOf(container);

      fireEvent.pointerDown(graph, { button: 2, clientX: 300, clientY: 200, pointerId: 13 });
      fireEvent.pointerMove(document, { clientX: 200, clientY: 150, pointerId: 13, buttons: 2 });
      fireEvent.pointerUp(document, { clientX: 200, clientY: 150, pointerId: 13, button: 2 });

      expect(graph.scrollLeft).toBe(0);
    });
  });

  it("edits an existing bus route from the detail panel", async () => {
    const { getByLabelText } = render(() => <NodeMixer nodeId="bus:reverb_bus" />);

    fireEvent.click(getByLabelText("Remove route to master"));

    await expect.poll(() => invokeMock.mock.calls).toContainEqual([
      "set_bus_routes",
      { name: "reverb_bus", routes: [] },
    ]);
  });

  it("selects an exclusive track output independently of sends", async () => {
    const { getByLabelText } = render(() => <NodeMixer nodeId="track:0" />);
    fireEvent.change(getByLabelText("Track output"), { target: { value: "reverb_bus" } });
    await expect.poll(() => invokeMock.mock.calls).toContainEqual([
      "set_track_output", { trackIndex: 0, output: "reverb_bus" },
    ]);
    expect(invokeMock.mock.calls.some(call => call[0] === "add_send")).toBe(false);
  });

  describe("connection selection", () => {
    const SEND_EDGE = '[data-edge-hit="send:1:reverb_bus:"]';

    function renderEditable(initialEdge: RoutingEdge | null = null) {
      const [selected, setSelected] = createSignal<string | null>(null);
      const [edge, setEdge] = createSignal<RoutingEdge | null>(initialEdge);
      const view = render(() => (
        <BusGraph
          selectedNode={selected()}
          onSelectNode={setSelected}
          selectedEdge={edge()}
          onSelectEdge={setEdge}
        />
      ));
      return { ...view, selected, edge };
    }

    it("selects a send edge with a click and reports its source and target", () => {
      const { container, edge } = renderEditable();
      fireEvent.click(container.querySelector(SEND_EDGE)!);
      expect(edge()?.kind).toBe("send");
      expect(edge()?.trackIndex).toBe(1);
      expect(edge()?.target).toBe("reverb_bus");
    });

    it("selects an edge from the keyboard", () => {
      const { container, edge } = renderEditable();
      fireEvent.keyDown(container.querySelector(SEND_EDGE)!, { key: "Enter" });
      expect(edge()?.kind).toBe("send");
      expect(edge()?.target).toBe("reverb_bus");
    });

    it("clears the edge selection from the background", () => {
      const { container, edge } = renderEditable({ kind: "send", trackIndex: 1, target: "reverb_bus" });
      expect(edge()).not.toBeNull();
      fireEvent.click(container.querySelector(".bus-graph")!);
      expect(edge()).toBeNull();
    });

    it("draws the selected edge thicker", () => {
      const { container } = renderEditable({ kind: "send", trackIndex: 1, target: "reverb_bus" });
      const path = container.querySelector('[data-edge="send:1:reverb_bus:"] path')!;
      expect(path.getAttribute("stroke-width")).toBe("2.6");
      expect(container.querySelector('[data-edge="send:1:reverb_bus:"]')!.classList.contains("selected")).toBe(true);
    });

    it("does not pan when the press starts on an edge", () => {
      const { container } = renderEditable();
      const graph = container.querySelector(".bus-graph") as HTMLDivElement;
      const hit = container.querySelector(SEND_EDGE)!;

      fireEvent.pointerDown(hit, { button: 0, clientX: 300, clientY: 200, pointerId: 21 });
      fireEvent.pointerMove(document, { clientX: 240, clientY: 180, pointerId: 21, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 240, clientY: 180, pointerId: 21, button: 0 });

      expect(graph.scrollLeft).toBe(0);
      expect(graph.scrollTop).toBe(0);
      expect(graph.classList.contains("panning")).toBe(false);
    });

    it("keeps the graph a pure viewer when no select callback is passed", () => {
      const { container } = render(() => <BusGraph selectedNode={null} onSelectNode={() => {}} />);
      fireEvent.click(container.querySelector(SEND_EDGE)!);
      expect(container.querySelector('[data-edge="send:1:reverb_bus:"]')!.classList.contains("selected")).toBe(false);
    });

    it("keeps the invisible edge hit stroke at least 10px wide", () => {
      const { container } = renderEditable();
      const hit = container.querySelector(SEND_EDGE)!;
      expect(Number(hit.getAttribute("stroke-width"))).toBeGreaterThanOrEqual(10);
      expect(hit.getAttribute("stroke")).toBe("transparent");
      expect((hit as SVGElement).style.pointerEvents).toBe("stroke");
    });

    it("selects an edge with Space as well as Enter", () => {
      const { container, edge } = renderEditable();
      fireEvent.keyDown(container.querySelector(SEND_EDGE)!, { key: " " });
      expect(edge()?.kind).toBe("send");
    });

    it("never starts a pan from the widened edge hit target", () => {
      const { container, edge } = renderEditable();
      const graph = container.querySelector(".bus-graph") as HTMLDivElement;
      const hit = container.querySelector(SEND_EDGE)!;

      fireEvent.pointerDown(hit, { button: 0, clientX: 300, clientY: 200, pointerId: 41 });
      fireEvent.pointerMove(document, { clientX: 240, clientY: 180, pointerId: 41, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 240, clientY: 180, pointerId: 41, button: 0 });

      expect(graph.scrollLeft).toBe(0);
      expect(graph.classList.contains("panning")).toBe(false);
      // The click that ends the gesture still selects; nothing was suppressed.
      fireEvent.click(hit);
      expect(edge()?.kind).toBe("send");
    });

    it("keeps a sub-threshold press a click instead of a nudge", () => {
      const { container, selected } = renderGraph();
      const graph = container.querySelector(".bus-graph") as HTMLDivElement;
      const node = container.querySelector('[data-node="track:0"]')!;

      // 2px of travel is below PAN_THRESHOLD (3px): no scroll, and the click
      // that follows still selects the node under the pointer.
      fireEvent.pointerDown(graph, { button: 0, clientX: 300, clientY: 200, pointerId: 51 });
      fireEvent.pointerMove(document, { clientX: 298, clientY: 200, pointerId: 51, buttons: 1 });
      fireEvent.pointerUp(document, { clientX: 298, clientY: 200, pointerId: 51, button: 0 });
      fireEvent.click(node);

      expect(graph.scrollLeft).toBe(0);
      expect(selected()).toBe("track:0");
    });
  });
});
