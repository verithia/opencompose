/**
 * Control cables: a @control_source output drags into a device control input.
 *
 * The graph draws a control edge with its own line style, refuses an
 * audio-only destination with a readable reason, and offers the same connect
 * path from the keyboard and from the effect rack's control-input row.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { invokeMock, makeSnapshot, makeTrack, makeEffect } from "../setup";
import { store } from "../../stores/project";
import {
  BusGraph,
  NODE_H,
  NODE_W,
  type RoutingEdge,
} from "../../components/BusGraph";
import { ConnectionDetail } from "../../components/ConnectionDetail";
import { EffectsRack } from "../../components/EffectsRack";
import { i18n, LOCALES } from "../../stores/i18n";
import type { Project, ProjectProperties } from "../../types/project";

const CONTROL_SOURCES: NonNullable<ProjectProperties["control_sources"]> = [{
  name: "kick_env",
  source_track: "Kick",
  attack: 0,
  release: 0.1,
}];

function baseSnapshot() {
  return makeSnapshot({
    tracks: [
      makeTrack("Kick", 0),
      {
        ...makeTrack("Bass", 1),
        effects: [makeEffect("Compression")],
      },
      {
        ...makeTrack("Guitar", 2),
        effects: [makeEffect("Eq")],
      },
    ],
    properties: {
      ...makeSnapshot().project.properties,
      control_sources: CONTROL_SOURCES,
    },
  });
}

/** Snapshot with the control edge already wired and rendered. */
function wiredSnapshot() {
  const snapshot = baseSnapshot();
  snapshot.project.tracks[1].input_routes = [{
    kind: "control",
    source: "kick_env",
    target: "fx1",
  }];
  return snapshot;
}

async function load(snapshot = baseSnapshot()) {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "get_undo_state") {
      return {
        can_undo: false, can_redo: false, undo_label: null, redo_label: null,
        undo_labels: [], redo_labels: [],
      };
    }
    if (cmd === "devices_catalog") return undefined;
    return snapshot;
  });
  await store.initProject();
}

beforeEach(async () => {
  i18n.setLocale("en");
  await load();
});

function renderGraph(snapshot: Project = baseSnapshot().project) {
  const [selected, setSelected] = createSignal<string | null>(null);
  const result = render(() => (
    <BusGraph selectedNode={selected()} onSelectNode={setSelected} />
  ));
  return { ...result, snapshot };
}

function nodeRect(container: HTMLElement, id: string) {
  const rect = container.querySelector(`[data-node="${id}"] rect`)!;
  return {
    x: Number(rect.getAttribute("x")),
    y: Number(rect.getAttribute("y")),
  };
}

function dragCable(
  socket: Element,
  from: { x: number; y: number },
  to: { x: number; y: number },
) {
  fireEvent.pointerDown(socket, {
    button: 0, pointerId: 7, clientX: from.x, clientY: from.y,
  });
  fireEvent.pointerMove(document, {
    pointerId: 7, buttons: 1, clientX: to.x, clientY: to.y,
  });
  fireEvent.pointerUp(document, {
    pointerId: 7, button: 0, clientX: to.x, clientY: to.y,
  });
}

describe("control cables", () => {
  it("draws a wired control edge with a distinct dashed style and name", async () => {
    const snapshot = wiredSnapshot();
    await load(snapshot);
    const { container } = renderGraph(snapshot.project);
    const edge = container.querySelector(".bus-graph-edge.control");
    expect(edge).toBeTruthy();
    const visible = edge!.querySelector("path")!;
    expect(visible.getAttribute("stroke-dasharray")).toBe("2,3");
    expect(visible.getAttribute("stroke")).toContain("accent-cool");
    const hit = edge!.querySelector(".bus-graph-edge-hit")!;
    expect(hit.getAttribute("aria-label")).toContain("kick_env");
    expect(hit.getAttribute("aria-label")).toContain("Compressor");
  });

  it("exposes a control output socket on the feed track", () => {
    const { container } = renderGraph();
    const socket = container.querySelector('[data-control-source="kick_env"]');
    expect(socket).toBeTruthy();
    expect(container.querySelector('[aria-label="kick_env control output"]')).toBe(socket);
  });

  it("connects a dragged control cable to a device that accepts it", async () => {
    const { container } = renderGraph();
    const socket = container.querySelector('[data-control-source="kick_env"]')!;
    const kick = nodeRect(container, "track:0");
    const bass = nodeRect(container, "track:1");
    dragCable(
      socket,
      { x: kick.x + NODE_W + 5, y: kick.y + NODE_H / 2 },
      { x: bass.x + 40, y: bass.y + NODE_H / 2 },
    );
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "add_control_route",
        { owner: "track:1", target: "fx1", source: "kick_env" },
      );
    });
    await vi.waitFor(() => {
      expect(container.querySelector(".bus-graph-status")?.textContent)
        .toContain("Connected kick_env");
    });
  });

  it("refuses a control cable dropped on an audio-only input with a readable reason", async () => {
    const { container } = renderGraph();
    const socket = container.querySelector('[data-control-source="kick_env"]')!;
    const kick = nodeRect(container, "track:0");
    const guitar = nodeRect(container, "track:2");
    dragCable(
      socket,
      { x: kick.x + NODE_W + 5, y: kick.y + NODE_H / 2 },
      { x: guitar.x + 40, y: guitar.y + NODE_H / 2 },
    );
    const status = await vi.waitFor(() => {
      const element = container.querySelector(".bus-graph-status.error");
      expect(element).toBeTruthy();
      return element!;
    });
    expect(status.textContent).toContain("Control cable refused");
    expect(status.textContent).toContain("Guitar");
    expect(status.textContent).toContain("no control input");
    expect(status.getAttribute("role")).toBe("alert");
    expect(
      invokeMock.mock.calls.some(([cmd]) => cmd === "add_control_route"),
    ).toBe(false);
  });

  it("refuses a cable dropped back on its own track", async () => {
    const { container } = renderGraph();
    const socket = container.querySelector('[data-control-source="kick_env"]')!;
    const kick = nodeRect(container, "track:0");
    dragCable(
      socket,
      { x: kick.x + NODE_W + 5, y: kick.y + NODE_H / 2 },
      { x: kick.x + 40, y: kick.y + NODE_H / 2 },
    );
    const status = await vi.waitFor(() => {
      const element = container.querySelector(".bus-graph-status.error");
      expect(element).toBeTruthy();
      return element!;
    });
    expect(status.textContent).toContain("own control input");
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "add_control_route")).toBe(false);
  });

  it("connects from the keyboard: arm the socket, then Enter on the target", async () => {
    const { container } = renderGraph();
    const socket = container.querySelector('[aria-label="kick_env control output"]')!;
    fireEvent.keyDown(socket, { key: "Enter" });
    expect(container.querySelector(".bus-graph-status")?.textContent)
      .toContain("selected");
    const bass = container.querySelector('[data-node="track:1"]')!;
    fireEvent.keyDown(bass, { key: "Enter" });
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "add_control_route",
        { owner: "track:1", target: "fx1", source: "kick_env" },
      );
    });
  });

  it("cancels an armed cable with Escape", () => {
    const { container } = renderGraph();
    fireEvent.keyDown(
      container.querySelector('[aria-label="kick_env control output"]')!,
      { key: "Enter" },
    );
    fireEvent.keyDown(container.querySelector(".bus-graph")!, { key: "Escape" });
    expect(container.querySelector(".bus-graph-status")).toBeNull();
    const bass = container.querySelector('[data-node="track:1"]')!;
    fireEvent.keyDown(bass, { key: "Enter" });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "add_control_route")).toBe(false);
  });

  it("removes a control edge from the inspector card", async () => {
    await load(wiredSnapshot());
    const edge: RoutingEdge = {
      kind: "control",
      target: "fx1",
      controlSource: "kick_env",
      controlFeed: 0,
      controlOwner: "track:1",
      controlDevice: "Compressor",
    };
    const { container } = render(() => (
      <ConnectionDetail edge={edge} onSelectEdge={() => {}} onClose={() => {}} />
    ));
    const getByLabelText = (label: string) => {
      const element = container.querySelector<HTMLElement>(`[aria-label="${label}"]`);
      if (!element) throw new Error(`no element labelled ${label}`);
      return element;
    };
    expect(getByLabelText("Disconnect control input")).toBeTruthy();
    fireEvent.click(getByLabelText("Disconnect control input"));
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "remove_control_route",
        { owner: "track:1", target: "fx1" },
      );
    });
  });

  it("offers the same connect path in the effect rack, with an empty-source state", async () => {
    const project = baseSnapshot().project;
    const track = project.tracks[1];
    const { container } = render(() => (
      <EffectsRack target="track:1" effects={track.effects} label="Bass" />
    ));
    fireEvent.click(container.querySelector(".effect-slot")!);
    const select = await vi.waitFor(() => {
      const element = container.querySelector<HTMLSelectElement>(".effect-control-input-select");
      expect(element).toBeTruthy();
      return element!;
    });
    expect(select.disabled).toBe(false);
    fireEvent.change(select, { target: { value: "kick_env" } });
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "add_control_route",
        { owner: "track:1", target: "fx1", source: "kick_env" },
      );
    });
  });

  it("disables the rack picker when the project has no control source", async () => {
    const snapshot = baseSnapshot();
    snapshot.project.properties.control_sources = [];
    await load(snapshot);
    const project = snapshot.project;
    const { container } = render(() => (
      <EffectsRack target="track:1" effects={project.tracks[1].effects} label="Bass" />
    ));
    fireEvent.click(container.querySelector(".effect-slot")!);
    const select = await vi.waitFor(() => {
      const element = container.querySelector<HTMLSelectElement>(".effect-control-input-select");
      expect(element).toBeTruthy();
      return element!;
    });
    expect(select.disabled).toBe(true);
    expect(container.querySelector(".effect-control-input-hint")?.textContent)
      .toBe(i18n.t("mixer.controlNoSources"));
  });

  it("translates every control string into all five locales", () => {
    const keys = i18n.keys().filter(
      (key) => key.startsWith("routing.control") || key.startsWith("mixer.control"),
    );
    expect(keys.length).toBeGreaterThan(10);
    for (const locale of LOCALES) {
      for (const key of keys) {
        expect(i18n.has(locale, key), `${locale} ${key}`).toBe(true);
      }
    }
  });

  it("falls back to English placeholders for an unknown control locale string", () => {
    i18n.setLocale("es");
    expect(i18n.t("routing.controlRefused", { node: "Guitar" }))
      .toContain("Guitar");
  });
});
