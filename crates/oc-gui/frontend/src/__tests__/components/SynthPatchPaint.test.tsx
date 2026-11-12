import { createOwnedUndoFixture } from "../setup";
/**
 * The preset browser must paint the chosen row before the load resolves, and a
 * later click must win over an earlier one still in flight.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import type { ParamValue, PresetDef } from "../../types/project";

const ownedUndo = createOwnedUndoFixture();

function param(value: string): ParamValue {
  return { Number: Number(value) };
}

function snapshotWith(params: [string, ParamValue][] = []) {
  const preset: PresetDef = {
    name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
    program: null, patch: null, file: null, root_note: null, extends: null,
    params, effects: [],
  };
  return makeSnapshot({ presets: [preset], tracks: [] });
}

/** Mirror the Rust batch: last write wins, null removes. */
function applyBatch(batch: [string, string | null][]): [string, ParamValue][] {
  const applied: [string, ParamValue][] = [];
  for (const [key, value] of batch) {
    const index = applied.findIndex(([existing]) => existing === key);
    if (value === null) {
      if (index >= 0) applied.splice(index, 1);
    } else if (index >= 0) {
      applied[index] = [key, param(value)];
    } else {
      applied.push([key, param(value)]);
    }
  }
  return applied;
}

/** Loads wait until the test releases them, so "in flight" is observable. */
let pending: Array<() => void> = [];

function mockDeferredLoad() {
  pending = [];
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "set_preset_params") {
      const { params } = args as { params: [string, string | null][] };
      const applied = applyBatch(params);
      await new Promise<void>((resolve) => pending.push(resolve));
      return { ...snapshotWith(applied), dirty: true };
    }
    return snapshotWith();
  });
}

async function openEditorWithBrowser() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "OpenSynth Init");
  fireEvent.click(view.container.querySelector<HTMLButtonElement>(".synth-patch-field")!);
  return view;
}

function row(name: string): HTMLElement {
  const element = screen.getByText(name).closest<HTMLElement>(".factory-patch-row");
  if (!element) throw new Error(`no browser row for ${name}`);
  return element;
}

function level(view: Awaited<ReturnType<typeof openEditorWithBrowser>>) {
  return view.container.querySelector("[data-param='osc.a.level'] .synth-param-value");
}

/** Track-preset replies resolve in the order the test releases them. */
function mockDeferredTrackPreset() {
  const releases: Array<() => void> = [];
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "set_track_preset") {
      const { presetName } = args as { presetName: string | null };
      const snapshot = snapshotWith();
      snapshot.project.tracks = [{
        name: "lead", instrument: { category: "synth", name: "lead_square" },
        preset: presetName,
      } as never];
      await new Promise<void>((resolve) => releases.push(resolve));
      return snapshot;
    }
    return snapshotWith();
  });
  return releases;
}

beforeEach(async () => {
  invokeMock.mockClear();
  mockDeferredLoad();
  await store.initProject();
});

describe("choosing a factory patch", () => {
  it("paints the chosen row before the load resolves", async () => {
    const view = await openEditorWithBrowser();
    const chosen = row("Glass Pad");
    expect(chosen.getAttribute("aria-selected")).toBe("false");

    fireEvent.dblClick(screen.getByText("Glass Pad"));
    expect(row("Glass Pad").getAttribute("aria-selected")).toBe("true");
    await waitFor(() => expect(pending).toHaveLength(1));

    // The load is still running: no snapshot has been applied yet.
    expect(level(view)?.textContent).toContain("78%");
    // The row the operator clicked already reads as the choice.
    expect(row("Glass Pad").getAttribute("aria-selected")).toBe("true");
    expect(row("Glass Pad").className).toContain("chosen");
    expect(row("Glass Pad").getAttribute("aria-busy")).toBe("true");

    pending[0]();
    await waitFor(() => expect(level(view)?.textContent).toContain("58%"));
  });

  it("orders running loads and skips superseded queued choices", async () => {
    const view = await openEditorWithBrowser();
    fireEvent.dblClick(screen.getByText("Neon Bass"));
    await waitFor(() => expect(pending).toHaveLength(1));
    fireEvent.dblClick(screen.getByText("Init"));
    fireEvent.dblClick(screen.getByText("Glass Pad"));
    expect(row("Glass Pad").getAttribute("aria-selected")).toBe("true");
    expect(row("Neon Bass").getAttribute("aria-selected")).toBe("false");
    expect(pending).toHaveLength(1); // The first backend load still owns the queue.

    pending[0]();
    await waitFor(() => expect(pending).toHaveLength(2));
    const loads = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_params_owned");
    expect(loads.map(([, args]) => args.patchLabel)).toEqual(["Neon Bass", "Glass Pad"]);
    // Completing the old running load must leave the newest choice visible.
    expect(row("Glass Pad").getAttribute("aria-selected")).toBe("true");
    expect(row("Glass Pad").getAttribute("aria-busy")).toBe("true");
    pending[1]();
    await waitFor(() => expect(level(view)?.textContent).toContain("58%"));
    await waitFor(() => expect(screen.queryByText("Neon Bass")).toBeNull());
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_params_owned")).toHaveLength(2);
  });

  it("keeps the newest track-preset reply when two land out of order", async () => {
    const releases = mockDeferredTrackPreset();
    await store.initProject();

    const first = store.setTrackPreset(0, "Neon Bass");
    const second = store.setTrackPreset(0, "Glass Pad");
    await Promise.resolve();
    expect(releases).toHaveLength(2);

    // The older change answers last; it must not revert the newer one.
    releases[1]();
    await second;
    releases[0]();
    await first;
    expect(store.project.tracks[0]?.preset).toBe("Glass Pad");
  });

  it("clears the pending row when the load fails", async () => {
    const view = await openEditorWithBrowser();
    invokeMock.mockImplementation(async (cmd: string) => {
    const ownership = ownedUndo.route(cmd, undefined);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, undefined);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") {
        return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      }
      if (cmd === "set_preset_params") throw new Error("unknown parameter");
      return snapshotWith();
    });

    fireEvent.dblClick(screen.getByText("Glass Pad"));
    await waitFor(() => expect(row("Glass Pad").getAttribute("aria-selected")).toBe("false"));
    expect(level(view)?.textContent).toContain("78%");
  });
});
