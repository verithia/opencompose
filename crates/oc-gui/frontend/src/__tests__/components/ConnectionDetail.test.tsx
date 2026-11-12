/**
 * ConnectionDetail — the inspector card for the selected connection: re-point,
 * level, pre/post tap, pan and remove. The toolbar holds none of it.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import {
  TRANSPORT_STOPPED,
  expectInvoked,
  invokeMock,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { ConnectionDetail } from "../../components/ConnectionDetail";
import type { RoutingEdge } from "../../components/BusGraph";
import type { Project } from "../../types/project";

const UNDO_NONE = {
  can_undo: false, can_redo: false,
  undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
};

function scenario(overrides: Partial<Project> = {}): Project {
  return makeSnapshot({
    tracks: [
      {
        ...makeTrack("Keys", 0),
        mix: {
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "space", amount: 0.5 }],
        },
      },
      {
        ...makeTrack("Drums", 1),
        mix: {
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "glue", amount: 0.25, pre_fader: true }],
        },
      },
    ],
    buses: [
      { name: "space", routes: ["master"], effects: [] },
      { name: "glue", routes: ["space", "master"], effects: [] },
      { name: "room", routes: ["master"], effects: [] },
    ],
    ...overrides,
  }).project;
}

function mockScenario(project: Project = scenario()) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return UNDO_NONE;
    return makeSnapshot(project);
  });
}

function renderDetail(initial: RoutingEdge) {
  const [edge, setEdge] = createSignal<RoutingEdge | null>(initial);
  const closed: string[] = [];
  const view = render(() => (
    (() => {
      const current = edge();
      return current
        ? (
          <ConnectionDetail
            edge={current}
            onSelectEdge={(next) => setEdge(next)}
            onClose={(focus) => { closed.push(focus); setEdge(null); }}
          />
        )
        : <div class="connection-detail closed" />;
    })()
  ));
  return { ...view, edge, closed };
}

beforeEach(async () => {
  invokeMock.mockClear();
  mockScenario();
  await store.initProject();
});

describe("ConnectionDetail", () => {
  it("re-points a track's main output as one command", async () => {
    const { getByLabelText, edge } = renderDetail({
      kind: "main", trackIndex: 0, target: "master",
    });
    fireEvent.change(getByLabelText("Connection target"), { target: { value: "glue" } });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("set_track_output", { trackIndex: 0, output: "glue" });
    await expect.poll(() => edge()?.target).toBe("glue");
  });

  it("re-points a send without touching its level or tap", async () => {
    const { getByLabelText, edge } = renderDetail({
      kind: "send", trackIndex: 1, target: "glue",
    });
    fireEvent.change(getByLabelText("Connection target"), { target: { value: "room" } });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("set_send_target", {
      trackIndex: 1, fromBus: "glue", toBus: "room", sourceOwner: null, sourceOutput: null,
    });
    await expect.poll(() => edge()?.target).toBe("room");
  });

  it("re-points a bus route as one command", async () => {
    const { getByLabelText, edge } = renderDetail({
      kind: "route", bus: "glue", target: "master",
    });
    fireEvent.change(getByLabelText("Connection target"), { target: { value: "room" } });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expect(invokeMock.mock.calls).toContainEqual([
      "set_bus_routes", { name: "glue", routes: ["space", "room"] },
    ]);
    await expect.poll(() => edge()?.target).toBe("room");
  });

  it("changes a send level, pan and pre/post tap", async () => {
    const { getByLabelText } = renderDetail({
      kind: "send", trackIndex: 1, target: "glue",
    });
    // Drums sends to glue pre-fader at 25%.
    expect((getByLabelText("Connection level") as HTMLInputElement).value).toBe("-12.0");
    expect((getByLabelText("Connection tap") as HTMLButtonElement).textContent).toBe("PRE");

    fireEvent.change(getByLabelText("Connection level"), { target: { value: "-6" } });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("set_send", { trackIndex: 1, busName: "glue", amount: 10 ** (-6 / 20) });

    invokeMock.mockClear();
    fireEvent.change(getByLabelText("Connection pan"), { target: { value: "0.4" } });
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("set_send", { trackIndex: 1, busName: "glue", pan: 0.4 });

    invokeMock.mockClear();
    fireEvent.click(getByLabelText("Connection tap"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("set_send", { trackIndex: 1, busName: "glue", preFader: false });
  });

  it("removes a send connection and reports where focus should go", async () => {
    // Start with the send present; only the remove command's snapshot drops it.
    const withoutSend = scenario();
    withoutSend.tracks[0].mix.sends = [];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_state") return UNDO_NONE;
      if (cmd === "remove_send") return makeSnapshot(withoutSend);
      return makeSnapshot(scenario());
    });
    await store.initProject();

    const { getByLabelText, closed, edge } = renderDetail({
      kind: "send", trackIndex: 0, target: "space",
    });
    fireEvent.click(getByLabelText("Remove connection"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("remove_send", {
      trackIndex: 0, busName: "space", sourceOwner: null, sourceOutput: null,
    });
    await expect.poll(() => closed).toEqual(["track:0"]);
    expect(edge()).toBeNull();
  });

  it("removes one of several bus routes without dropping the others", async () => {
    const { getByLabelText } = renderDetail({
      kind: "route", bus: "glue", target: "master",
    });
    fireEvent.click(getByLabelText("Remove connection"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expect(invokeMock.mock.calls).toContainEqual([
      "set_bus_routes", { name: "glue", routes: ["space"] },
    ]);
  });

  it("shows AUX instead of a pre/post tap for a plugin auxiliary send", async () => {
    const withAux = scenario({
      tracks: [{
        ...makeTrack("Keys", 0),
        mix: {
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "space", amount: 0.5, source: { owner: "Piano 1", output: 2 } }],
        },
      }],
    });
    mockScenario(withAux);
    await store.initProject();

    const { queryByLabelText, getByText, edge } = renderDetail({
      kind: "send", trackIndex: 0, target: "space", source: { owner: "Piano 1", output: 2 },
    });
    expect(edge()?.kind).toBe("send");
    expect(queryByLabelText("Connection tap")).toBeNull();
    expect(getByText("AUX")).toBeTruthy();
  });

  it("does not offer remove for a main output", () => {
    const { queryByLabelText, getByText } = renderDetail({
      kind: "main", trackIndex: 0, target: "master",
    });
    expect(queryByLabelText("Remove connection")).toBeNull();
    expect(getByText("The main output is exclusive; re-point it here.")).toBeTruthy();
  });

  it("renders nothing once the selected connection no longer exists", () => {
    const { container } = renderDetail({
      kind: "send", trackIndex: 0, target: "deleted_bus",
    });
    expect(container.querySelector(".connection-detail")).toBeNull();
  });
});
