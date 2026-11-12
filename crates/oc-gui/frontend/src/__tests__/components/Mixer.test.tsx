/**
 * Mixer component tests — now tests the split view (NodeMixer + BusGraph).
 * Runs in jsdom — no browser required.
 */
import { afterEach, describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { chooseDropdown, invokeMock, makeSnapshot, makeTrack, makePattern, expectInvoked, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { Mixer } from "../../components/Mixer";
import { NodeMixer } from "../../components/NodeMixer";

afterEach(() => {
  vi.unstubAllGlobals();
});

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    return makeSnapshot({
      tracks: [
        makeTrack("Piano", 0),
        { ...makeTrack("Drums", 1), mix: { volume_db: -6, pan: 0.5, sends: [{ target: "reverb_bus", amount: 0.4 }], mute: false, solo: false } },
      ],
      buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
      patterns: [makePattern("beat")],
    });
  });
  await store.initProject();
});

describe("Mixer (split view)", () => {
  it("selects a channel without graph clicks and changes its output", () => {
    const {getByLabelText} = render(() => <Mixer />);
    chooseDropdown(getByLabelText("Channel"), "Drums");
    fireEvent.change(getByLabelText('Track output'),{target:{value:'reverb_bus'}});
    expectInvoked('set_track_output',{trackIndex:1,output:'reverb_bus'});
    chooseDropdown(getByLabelText("Channel"), "Bus: reverb_bus");
    expect(getByLabelText('Bus route target')).toBeTruthy();
  });
  it("renders without crashing", () => {
    const { container } = render(() => <Mixer />);
    expect(container.querySelector(".mixer-panel")).toBeTruthy();
  });

  it("renders the NodeMixer detail panel", () => {
    const { container } = render(() => <Mixer />);
    const detail = container.querySelector(".node-mixer");
    expect(detail).toBeTruthy();
  });

  it("renders the BusGraph DAG", () => {
    const { container } = render(() => <Mixer />);
    const graph = container.querySelector(".bus-graph");
    expect(graph).toBeTruthy();
  });

  it("clips both columns to the panel so the graph can scroll inside it", () => {
    // A grid row of minmax(0, 1fr) plus stretch keeps a tall graph from growing
    // its own box: without it the graph is clipped and cannot be scrolled/panned.
    const presetsCss = readFileSync(`${process.cwd()}/src/styles/daw/presets.css`, "utf8")
      .replace(/\/\*[\s\S]*?\*\//g, "");
    const bodies: string[] = [];
    for (const match of presetsCss.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
      if (match[1].trim() === ".mixer-panel") bodies.push(match[2]);
    }
    const panel = bodies.join("\n");
    expect(panel).toMatch(/grid-template-rows:\s*minmax\(0,\s*1fr\)/);
    expect(panel).toMatch(/align-items:\s*stretch/);
  });

  it("keeps the routing toolbar above a graph that still owns the remaining height", () => {
    // The toolbar wraps instead of widening its column, and it never grows:
    // the graph's flex:1 min-height:0 row keeps the scrollable area intact at
    // every UI scale.
    const mixerCss = readFileSync(`${process.cwd()}/src/styles/daw/mixer.css`, "utf8")
      .replace(/\/\*[\s\S]*?\*\//g, "");
    const rules = new Map<string, string>();
    for (const match of mixerCss.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
      rules.set(match[1].trim(), match[2]);
    }
    const toolbar = rules.get(".routing-toolbar") ?? "";
    expect(toolbar).toMatch(/flex:\s*0 0 auto/);
    expect(toolbar).toMatch(/flex-wrap:\s*wrap/);
    expect(rules.get(".mixer-graph-area")).toMatch(/min-height:\s*0/);
    expect(rules.get(".mixer-graph-area")).toMatch(/flex:\s*1 1 auto/);
  });

  it("renders SVG nodes for tracks, buses, and master", () => {
    const { container } = render(() => <Mixer />);
    const svg = container.querySelector("svg");
    expect(svg).toBeTruthy();
    // Should have rect elements for nodes (2 tracks + 1 bus + 1 master = 4)
    const rects = svg!.querySelectorAll("rect");
    expect(rects.length).toBe(4);
  });

  it("renders edges between nodes", () => {
    const { container } = render(() => <Mixer />);
    const svg = container.querySelector("svg");
    const paths = svg!.querySelectorAll("path");
    // Piano → master, Drums → reverb_bus, Drums → master, reverb_bus → master = 4 edges
    expect(paths.length).toBeGreaterThanOrEqual(3);
  });

  it("node click selects it and shows in NodeMixer", () => {
    const { container } = render(() => <Mixer />);
    const svg = container.querySelector("svg");
    // Click on a node group (the <g> elements are the clickable nodes)
    const groups = svg!.querySelectorAll("g");
    // Find the group that has "Drums" text
    let drumsGroup: Element | null = null;
    for (const g of groups) {
      if (g.textContent?.includes("Drums")) { drumsGroup = g; break; }
    }
    if (drumsGroup) {
      fireEvent.click(drumsGroup);
      // NodeMixer should now show "Drums"
      const detail = container.querySelector(".node-mixer");
      expect(detail?.textContent).toContain("Drums");
    }
  });

  it("clicking a graph node shows its full channel configuration in the inspector", () => {
    const { container } = render(() => <Mixer />);
    // No strip row above the graph any more.
    expect(container.querySelector(".mixer-channel-strips")).toBeNull();
    fireEvent.click(container.querySelector('[data-node="track:1"]')!);
    const inspector = container.querySelector(".mixer-inspector")!;
    expect(inspector.querySelector(".node-mixer")?.textContent).toContain("Drums");
    expect(inspector.querySelector('[data-channel-strip="track:1"]')).toBeTruthy();
    expect(inspector.querySelector('[aria-label="Send level to reverb_bus"]')).toBeTruthy();
    fireEvent.click(container.querySelector('[data-node="bus:reverb_bus"]')!);
    expect(inspector.querySelector('[data-channel-strip="bus:reverb_bus"]')).toBeTruthy();
  });

  it("NodeMixer shows mute/solo buttons for track nodes", () => {
    const { container } = render(() => <Mixer />);
    // Default selection is track:0
    const detail = container.querySelector(".node-mixer");
    const btns = detail?.querySelectorAll(".track-btn");
    expect(btns?.length).toBeGreaterThanOrEqual(2);
  });

  it("NodeMixer shows effects rack", () => {
    const { container } = render(() => <Mixer />);
    const detail = container.querySelector(".node-mixer");
    const rack = detail?.querySelector(".effects-rack");
    expect(rack).toBeTruthy();
  });

  it("toggles mute from NodeMixer", async () => {
    const { container } = render(() => <Mixer />);
    const detail = container.querySelector(".node-mixer");
    const btns = detail?.querySelectorAll(".track-btn");
    if (!btns || btns.length < 1) return;

    invokeMock.mockResolvedValueOnce(makeSnapshot({
      tracks: [
        { ...makeTrack("Piano", 0), mix: { volume_db: 0, pan: 0, sends: [], mute: true, solo: false } },
        makeTrack("Drums", 1),
      ],
    }));
    fireEvent.click(btns[0]); // mute button

    await vi.waitFor(() => {
      expectInvoked("set_track_mute", { trackIndex: 0, mute: true });
    });
  });

  it("keeps a fader drag to one whole-document commit", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});

    const { container } = render(() => <NodeMixer nodeId="track:0" />);
    const fader = container.querySelector<HTMLElement>(".mixer-fader-container")!;
    fader.getBoundingClientRect = () => ({
      top: 0, bottom: 80, left: 0, right: 24, width: 24, height: 80,
      x: 0, y: 0, toJSON: () => ({}),
    }) as DOMRect;

    // Count only the drag: the mount already asked for latency and undo state.
    invokeMock.mockClear();
    fireEvent.mouseDown(fader, { clientY: 20 });
    for (let step = 0; step < 30; step += 1) {
      fireEvent.mouseMove(document, { clientY: 10 + step });
    }
    // Thirty moves share one animation frame.
    expect(frames).toHaveLength(1);
    frames.shift()!(0);
    fireEvent.mouseUp(document, { clientY: 40 });

    await vi.waitFor(() => {
      const commits = invokeMock.mock.calls.filter(([command]) => command === "update_track_mix");
      expect(commits).toHaveLength(1);
    });
    const commands = invokeMock.mock.calls.map(([command]) => command);
    const wholeDocument = commands.filter(
      (command) => command === "update_track_mix" || command === "set_master_volume",
    );
    const live = commands.filter((command) => command === "update_mixer_touch");
    expect(wholeDocument).toHaveLength(1);
    expect(live.length).toBeGreaterThan(0);
    expect(live.length).toBeLessThan(30);
    expect(commands.filter((command) => command === "begin_mixer_touch")).toHaveLength(1);
  });

  it("creates a master-routed bus", async () => {
    const { getByLabelText } = render(() => <Mixer />);
    const input = getByLabelText("New bus name") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "delay_bus" } });
    fireEvent.click(getByLabelText("Create bus"));

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("add_bus", {
        name: "delay_bus",
        routes: ["master"],
      });
    });
  });

  it("adds a send from the selected track", async () => {
    const { getByLabelText } = render(() => <Mixer />);
    const select = getByLabelText("Send target bus") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "reverb_bus" } });
    fireEvent.click(getByLabelText("Add send"));

    await vi.waitFor(() => {
      expectInvoked("add_send", { trackIndex: 0, busName: "reverb_bus", amount: 0.25 });
    });
  });

  it("shows empty state when no tracks", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      return makeSnapshot({ tracks: [], buses: [] });
    });
    await store.initProject();

    const { container } = render(() => <Mixer />);
    expect(container.textContent).toContain("No tracks");
  });

  it("opens the edge editor from a graph connection and closes it on a node click", () => {
    const { container, queryByLabelText } = render(() => <Mixer />);
    fireEvent.click(container.querySelector('[data-edge-hit="send:1:reverb_bus:"]')!);

    // An edge selection also selects its source node, so the inspector fills.
    expect(queryByLabelText("Connection target")).toBeTruthy();
    expect(container.querySelector(".node-mixer")?.textContent).toContain("Drums");
    expect((queryByLabelText("Connection level") as HTMLInputElement).value).toBe("-8.0");

    fireEvent.click(container.querySelector('[data-node="track:0"]')!);
    expect(queryByLabelText("Connection target")).toBeNull();
    expect(container.querySelector(".node-mixer")?.textContent).toContain("Piano");
  });

  it("selects a newly created bus node in the routing view", async () => {
    const buses = [
      { name: "reverb_bus", routes: ["master"], effects: [] },
      { name: "delay_bus", routes: ["master"], effects: [] },
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        buses: cmd === "add_bus" ? buses : buses.slice(0, 1),
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();

    const { getByLabelText, container, queryByLabelText } = render(() => <Mixer />);
    fireEvent.input(getByLabelText("New bus name"), { target: { value: "delay_bus" } });
    fireEvent.click(getByLabelText("Create bus"));

    await vi.waitFor(() => {
      expect(container.querySelector('[data-node="bus:delay_bus"]')).toBeTruthy();
    });
    expect(queryByLabelText("Delete selected bus")).toBeTruthy();
  });

  it("keeps the project toolbar identical when a connection is selected", () => {
    const { container } = render(() => <Mixer />);
    const toolbar = container.querySelector(".routing-toolbar")!;
    const before = toolbar.innerHTML;
    fireEvent.click(container.querySelector('[data-edge-hit="send:1:reverb_bus:"]')!);
    expect(container.querySelector(".connection-detail")).toBeTruthy();
    expect(toolbar.innerHTML).toBe(before);
  });

  it("routes connection editing through the inspector card, never the toolbar", () => {
    const { container, getByLabelText } = render(() => <Mixer />);
    fireEvent.click(container.querySelector('[data-edge-hit="send:1:reverb_bus:"]')!);
    expect(getByLabelText("Connection target").closest(".connection-detail")).toBeTruthy();
    expect(container.querySelector(".routing-toolbar .track-btn")).toBeNull();
    expect(container.querySelector(".connection-detail .track-btn")).toBeNull();
    expect(container.querySelector(".routing-node-actions .track-btn")).toBeNull();
  });

  it("leaves connection editing on Escape and focuses the graph", () => {
    const { container, queryByLabelText } = render(() => <Mixer />);
    fireEvent.click(container.querySelector('[data-edge-hit="send:1:reverb_bus:"]')!);
    expect(queryByLabelText("Connection target")).toBeTruthy();
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(queryByLabelText("Connection target")).toBeNull();
    const active = document.activeElement as Element | null;
    expect(
      active?.classList.contains("bus-graph") || active?.getAttribute("data-node") === "track:1",
    ).toBe(true);
  });

  it("removes the selected connection on Delete and focuses its source node", async () => {
    // The mock applies the removal so the inspector can observe that the send is gone.
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      if (cmd === "remove_send") {
        return makeSnapshot({
          tracks: [
            makeTrack("Piano", 0),
            { ...makeTrack("Drums", 1), mix: { volume_db: -6, pan: 0.5, sends: [], mute: false, solo: false } },
          ],
          buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
          patterns: [makePattern("beat")],
        });
      }
      return makeSnapshot({
        tracks: [
          makeTrack("Piano", 0),
          { ...makeTrack("Drums", 1), mix: { volume_db: -6, pan: 0.5, sends: [{ target: "reverb_bus", amount: 0.4 }], mute: false, solo: false } },
        ],
        buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();

    const { container, queryByLabelText } = render(() => <Mixer />);
    fireEvent.click(container.querySelector('[data-edge-hit="send:1:reverb_bus:"]')!);
    fireEvent.keyDown(document.body, { key: "Delete" });
    await vi.waitFor(() => {
      expectInvoked("remove_send", {
        trackIndex: 1, busName: "reverb_bus", sourceOwner: null, sourceOutput: null,
      });
    });
    await vi.waitFor(() => {
      expect(queryByLabelText("Connection target")).toBeNull();
      const active = document.activeElement as Element | null;
      expect(
        active?.classList.contains("bus-graph") || active?.getAttribute("data-node") === "track:1",
      ).toBe(true);
    });
  });

  it("deletes a free bus and moves focus back to the graph", async () => {
    const withBus = () => makeSnapshot({
      tracks: [makeTrack("Piano", 0)],
      buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      if (cmd === "remove_bus") {
        return makeSnapshot({
          tracks: [makeTrack("Piano", 0)],
          buses: [],
          patterns: [makePattern("beat")],
        });
      }
      return withBus();
    });
    await store.initProject();

    const { container, queryByLabelText } = render(() => <Mixer />);
    fireEvent.click(container.querySelector('[data-node="bus:reverb_bus"]')!);
    fireEvent.click(queryByLabelText("Delete selected bus")!);
    await vi.waitFor(() => {
      expectInvoked("remove_bus", { name: "reverb_bus", reroute: false });
    });
    await vi.waitFor(() => {
      expect(queryByLabelText("Delete selected bus")).toBeNull();
    });
    const active = document.activeElement as Element | null;
    expect(
      active?.classList.contains("bus-graph") || active?.getAttribute("data-node") === "master",
    ).toBe(true);
  });
});
