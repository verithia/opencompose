/**
 * RoutingToolbar — the project-level band above the routing graph. It only
 * creates buses now; node and connection editing live in the inspector.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { TRANSPORT_STOPPED, invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { RoutingToolbar } from "../../components/RoutingToolbar";
import type { Project } from "../../types/project";

const UNDO_NONE = {
  can_undo: false, can_redo: false,
  undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
};

function scenario(): Project {
  return makeSnapshot({
    tracks: [makeTrack("Keys", 0)],
    buses: [
      { name: "space", routes: ["master"], effects: [] },
      { name: "glue", routes: ["master"], effects: [] },
    ],
  }).project;
}

function mockScenario() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return UNDO_NONE;
    return makeSnapshot(scenario());
  });
}

function renderToolbar() {
  const [node, setNode] = createSignal<string | null>(null);
  const view = render(() => (
    <RoutingToolbar onSelectNode={(id) => setNode(id)} />
  ));
  return { ...view, node };
}

beforeEach(async () => {
  invokeMock.mockClear();
  mockScenario();
  await store.initProject();
});

describe("RoutingToolbar", () => {
  it("creates the named bus from the field and selects it", async () => {
    const { getByLabelText, node } = renderToolbar();
    const input = getByLabelText("New bus name") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "delay_bus" } });
    fireEvent.click(getByLabelText("Create bus"));

    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expect(invokeMock.mock.calls).toContainEqual([
      "add_bus", { name: "delay_bus", routes: ["master"] },
    ]);
    await expect.poll(() => node()).toBe("bus:delay_bus");
  });

  it("names a bus sensibly when the field is empty", async () => {
    const { getByLabelText } = renderToolbar();
    fireEvent.click(getByLabelText("Create bus"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expect(invokeMock.mock.calls).toContainEqual([
      "add_bus", { name: "Bus 1", routes: ["master"] },
    ]);
  });

  it("creates the bus on Enter", async () => {
    const { getByLabelText } = renderToolbar();
    const input = getByLabelText("New bus name");
    fireEvent.input(input, { target: { value: "bus_enter" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expect(invokeMock.mock.calls).toContainEqual([
      "add_bus", { name: "bus_enter", routes: ["master"] },
    ]);
  });

  it("uses the routing button, not the track header icon button", () => {
    const { container, getByLabelText } = renderToolbar();
    expect(container.querySelector(".routing-toolbar .track-btn")).toBeNull();
    expect(getByLabelText("Create bus").classList.contains("routing-btn")).toBe(true);
  });

  it("stays project-level: no connection or node editor lives in the row", () => {
    const { container } = renderToolbar();
    expect(container.querySelector('[aria-label="Connection target"]')).toBeNull();
    expect(container.querySelector('[aria-label="Delete selected bus"]')).toBeNull();
    expect(container.querySelector('[aria-label="Add routing send"]')).toBeNull();
    expect(container.textContent).toContain("Select a node or connection");
  });
});
