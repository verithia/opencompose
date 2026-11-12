import { createOwnedUndoFixture } from "../setup";
import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { cleanup, render, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { TRANSPORT_STOPPED, invokeMock, makeSnapshot, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import type { ParamValue, PresetDef } from "../../types/project";
import { EXTRA_PRESETS } from "../../data/factoryPresets";

const ownedUndo = createOwnedUndoFixture();

const BUILT_IN_NAMES = ["Init", "Neon Bass", "Glass Pad", "Pulse Pluck"];
const ALL_PATCH_NAMES = [...BUILT_IN_NAMES, ...EXTRA_PRESETS.map((preset) => preset.name)];
const CATEGORY_OF = new Map(EXTRA_PRESETS.map((preset) => [preset.name, preset.category]));

/** Same predicate the browser uses: name or category contains the query. */
function expectedMatches(query: string): string[] {
  return ALL_PATCH_NAMES.filter((name) =>
    name.toLowerCase().includes(query) || (CATEGORY_OF.get(name) ?? "").includes(query));
}

function param(value: string): ParamValue {
  return { Number: Number(value) };
}

function snapshotWith(params: [string, ParamValue][] = [], patch: string | null = null) {
  const preset: PresetDef = {
    name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
    program: null, patch, file: null, root_note: null, extends: null,
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

function mockLoad() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "set_preset_params") {
      const { params, patchLabel } = args as {
        params: [string, string | null][];
        patchLabel?: string | null;
      };
      return { ...snapshotWith(applyBatch(params), patchLabel ?? null), dirty: true };
    }
    return snapshotWith();
  });
}

async function openEditor() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "OpenSynth Init");
  return view;
}

/** The header field opens the factory + user-library browser. */
function openBrowser() {
  fireEvent.click(screen.getByLabelText("Preset browser"));
}

function rowNames(): string[] {
  return Array.from(
    document.querySelectorAll(".factory-patch-row .factory-patch-name"),
    (node) => node.textContent ?? "",
  );
}

function loadCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned");
}

function auditionCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "audition_factory_patch");
}

function stopCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "stop_audition");
}

function rowButton(name: string): HTMLButtonElement {
  for (const row of document.querySelectorAll(".factory-patch-row")) {
    if (row.querySelector(".factory-patch-name")?.textContent === name) {
      return row.querySelector(".factory-patch-audition") as HTMLButtonElement;
    }
  }
  throw new Error(`no factory row for ${name}`);
}

afterEach(cleanup);

beforeEach(async () => {
  invokeMock.mockClear();
  mockLoad();
  await store.initProject();
});

describe("OpenSynth factory patch browser", () => {
  it("opens from the preset field with every factory patch and no native select", async () => {
    await openEditor();
    expect(document.querySelector("select[aria-label='Factory patch']")).toBeNull();

    openBrowser();

    await waitFor(() => expect(rowNames()).toEqual(ALL_PATCH_NAMES));
    expect(screen.getByRole("dialog", { name: "Presets" })).toBeTruthy();
    expect(screen.getByLabelText("Search presets")).toBeTruthy();
  });

  it("filters on name and on category", async () => {
    await openEditor();
    openBrowser();
    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "glass" } });
    await waitFor(() => expect(rowNames()).toEqual(expectedMatches("glass")));

    // A category word matches every preset filed under it, whichever group header it sits in.
    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "brass" } });
    await waitFor(() => expect(rowNames()).toEqual(expectedMatches("brass")));
    expect(rowNames().length).toBeGreaterThan(1);
  });

  it("groups the authored presets by category", async () => {
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));
    const headers = [...document.querySelectorAll(".factory-patch-group-label")].map((node) => node.textContent);
    for (const category of ["bass", "lead", "pad", "pluck", "keys", "brass", "fx"]) {
      expect(headers.some((header) => header?.toLowerCase().includes(category))).toBe(true);
    }
  });

  it("shows an empty state when nothing matches", async () => {
    await openEditor();
    openBrowser();
    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "zzz" } });

    await waitFor(() => expect(rowNames()).toEqual([]));
    expect(screen.getByText("No preset matches “zzz”.")).toBeTruthy();
  });

  it("loads the first hit on Enter with the atomic batch", async () => {
    await openEditor();
    openBrowser();
    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "neon" } });
    await waitFor(() => expect(rowNames()).toEqual(["Neon Bass"]));

    fireEvent.keyDown(screen.getByLabelText("Search presets"), { key: "Enter" });

    await waitFor(() => expect(loadCalls()).toHaveLength(1));
    const [, args] = loadCalls()[0] as [string, { params: [string, string | null][]; replaceSynth: boolean }];
    expect(args.replaceSynth).toBe(true);
    expect(args.params).toEqual(expect.arrayContaining([
      // The patch's own keys.
      ["filter.cutoff", "740"], ["osc.a.level", "0.74"], ["voice.mono", "1"], ["sub.level", "0.42"],
      // Init defaults the patch leaves alone, plus the explicit modulation resets.
      ["osc.a.waveform", "1"], ["osc.c.level", "0"], ["filter.model", "0"],
      ["voice.polyphony", "16"], ["tuning.reference_hz", "440"],
      ["macro.8", "0"], ["step.16", "0"], ["mod.64.amount", "0"],
      ["wavetable.file", null], ["open_synth_state_file", null],
    ]));
    expect(new Set(args.params.map(([key]) => key)).size).toBe(args.params.length);
    expect(args.params).toHaveLength(448);
  });

  it("closes on Escape without loading anything", async () => {
    await openEditor();
    openBrowser();
    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "pad" } });
    fireEvent.keyDown(screen.getByLabelText("Search presets"), { key: "Escape" });

    await waitFor(() => expect(screen.queryByLabelText("Search presets")).toBeNull());
    expect(loadCalls()).toHaveLength(0);
    // The header keeps the project patch name; the factory origin is a sublabel.
    expect(document.querySelector(".synth-patch-name")?.textContent).toBe("OpenSynth Init");
    expect(document.querySelector(".synth-patch-origin")).toBeNull();
  });

  it("opens from the patch field and shows the loaded name afterwards", async () => {
    await openEditor();
    expect(screen.getByLabelText("Preset browser").textContent).toContain("Init");

    fireEvent.click(screen.getByLabelText("Preset browser"));
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.dblClick(screen.getByText("Glass Pad"));
    await waitFor(() => expect(document.querySelector(".synth-patch-origin")?.textContent).toBe("Glass Pad"));
    expect(document.querySelector(".synth-patch-name")?.textContent).toBe("OpenSynth Init");

    // Reopening from the field highlights the patch that is loaded now.
    fireEvent.click(screen.getByLabelText("Preset browser"));
    const current = await waitFor(() => {
      const row = document.querySelector(".factory-patch-row[aria-selected='true']");
      expect(row?.textContent).toContain("Glass Pad");
      return row;
    });
    expect(current).toBeTruthy();
    // Two full opens of the whole bank, so this one needs more than the default budget.
  }, 20_000);

  it("moves the highlight with the arrow keys and loads on Enter", async () => {
    await openEditor();
    openBrowser();
    fireEvent.keyDown(screen.getByLabelText("Search presets"), { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByLabelText("Search presets"), { key: "Enter" });

    await waitFor(() => expect(loadCalls()).toHaveLength(1));
    const [, args] = loadCalls()[0] as [string, { params: [string, string | null][] }];
    expect(args.params).toEqual(expect.arrayContaining([["filter.cutoff", "740"], ["voice.mono", "1"]]));
    await waitFor(() => expect(screen.getByLabelText("Preset browser").textContent).toContain("Neon Bass"));
  });

  it("closes without loading when the pointer leaves the panel", async () => {
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.pointerDown(document.body);

    await waitFor(() => expect(screen.queryByLabelText("Search presets")).toBeNull());
    expect(loadCalls()).toHaveLength(0);
  });
});

function auditionTransport(label: string, status: "loading" | "ready") {
  return {
    ...TRANSPORT_STOPPED(),
    audition_active: true,
    audition: { preset: label, status, ready_ms: 0.5, instances: 0, loads: 1, error: null },
  };
}

/** Report whatever candidate the last audition call queued, like the backend. */
function mockAudition() {
  let label = "Init";
  let status: "loading" | "ready" = "ready";
  let paused = false;
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "audition_factory_patch") {
      label = (args as { label: string }).label;
      status = "loading";
      paused = false;
      return undefined;
    }
    if (cmd === "stop_audition") return undefined;
    if (cmd === "pause_audition") paused = true;
    if (cmd === "resume_audition") paused = false;
    if (["get_transport_state", "pause_audition", "resume_audition"].includes(cmd)) {
      const state = auditionTransport(label, status);
      return { ...state, audition_active: !paused, audition: { ...state.audition, paused } };
    }
    if (cmd === "set_preset_params") {
      const { params, patchLabel } = args as {
        params: [string, string | null][];
        patchLabel?: string | null;
      };
      return { ...snapshotWith(applyBatch(params), patchLabel ?? null), dirty: true };
    }
    return snapshotWith();
  });
  return {
    ready() {
      status = "ready";
    },
  };
}

describe("factory patch audition", () => {
  afterEach(async () => {
    if (store.auditionActive()) await store.stopAudition();
  });

  it("auditions a candidate from its row without loading it", async () => {
    mockAudition();
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.click(rowButton("Neon Bass"));

    await waitFor(() => expect(auditionCalls()).toHaveLength(1));
    const [, args] = auditionCalls()[0] as [
      string,
      { params: [string, string | null][]; label: string },
    ];
    expect(args.label).toBe("Neon Bass");
    expect(args.params).toHaveLength(448);
    expect(args.params).toEqual(expect.arrayContaining([
      ["filter.cutoff", "740"], ["voice.mono", "1"], ["output.gain", "0.26"],
      ["wavetable.file", null], ["mod.64.amount", "0"],
    ]));
    expect(loadCalls()).toHaveLength(0);

    await waitFor(() => expect(rowButton("Neon Bass").getAttribute("aria-busy")).toBe("true"));
    expect(rowButton("Neon Bass").textContent).toBe("…");
    expect(rowButton("Neon Bass").classList.contains("playing")).toBe(true);
  });

  it("shows the sounding row once the backend reports ready", async () => {
    const mock = mockAudition();
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.click(rowButton("Glass Pad"));
    await waitFor(() => expect(auditionCalls()).toHaveLength(1));
    mock.ready();

    await waitFor(() => expect(rowButton("Glass Pad").getAttribute("aria-busy")).toBe("false"));
    expect(rowButton("Glass Pad").textContent).toBe("Ⅱ");
    expect(rowButton("Glass Pad").getAttribute("aria-label")).toBe("Pause preview: Glass Pad");
    expect(rowButton("Neon Bass").textContent).toBe("▶");
  });

  it("supersedes with the next candidate's own batch", async () => {
    const mock = mockAudition();
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.click(rowButton("Neon Bass"));
    await waitFor(() => expect(auditionCalls()).toHaveLength(1));
    mock.ready();
    await waitFor(() => expect(rowButton("Neon Bass").getAttribute("aria-busy")).toBe("false"));

    fireEvent.click(rowButton("Glass Pad"));
    await waitFor(() => expect(auditionCalls()).toHaveLength(2));
    const [, args] = auditionCalls()[1] as [
      string,
      { params: [string, string | null][]; label: string },
    ];
    expect(args.label).toBe("Glass Pad");
    expect(args.params).toEqual(expect.arrayContaining([
      ["osc.a.waveform", "4"], ["filter.cutoff", "4200"], ["amp.release", "3.2"],
    ]));
  });

  it("ignores audition clicks while a patch load is in flight", async () => {
    let finish!: () => void;
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return Promise.resolve(ownership.value);
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "set_preset_params") {
        return new Promise((resolve) => {
          finish = () => resolve({
            ...snapshotWith(applyBatch((args as { params: [string, string | null][] }).params)),
            dirty: true,
          });
        });
      }
      if (cmd === "get_transport_state") return Promise.resolve(TRANSPORT_STOPPED());
      return Promise.resolve(snapshotWith());
    });
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.dblClick(screen.getByText("Glass Pad"));
    await waitFor(() => expect(loadCalls()).toHaveLength(1));
    const button = rowButton("Neon Bass");
    expect(button.disabled).toBe(true);

    fireEvent.click(button);

    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(auditionCalls()).toHaveLength(0);
    finish();
    await waitFor(() => expect(screen.queryByLabelText("Search presets")).toBeNull());
  });

  it("pauses and resumes from the row affordance, and stops from Escape", async () => {
    const mock = mockAudition();
    await openEditor();
    openBrowser();
    await waitFor(() => expect(rowNames()).toHaveLength(ALL_PATCH_NAMES.length));

    fireEvent.click(rowButton("Neon Bass"));
    await waitFor(() => expect(auditionCalls()).toHaveLength(1));
    mock.ready();
    await waitFor(() => expect(rowButton("Neon Bass").textContent).toBe("Ⅱ"));

    fireEvent.click(rowButton("Neon Bass"));
    await waitFor(() => expect(rowButton("Neon Bass").getAttribute("aria-label")).toBe("Resume preview: Neon Bass"));
    expect(store.auditionActive()).toBe(true);
    expect(stopCalls()).toHaveLength(0);
    fireEvent.click(rowButton("Neon Bass"));
    await waitFor(() => expect(rowButton("Neon Bass").getAttribute("aria-label")).toBe("Pause preview: Neon Bass"));
    expect(auditionCalls()).toHaveLength(1);

    fireEvent.click(rowButton("Glass Pad"));
    await waitFor(() => expect(auditionCalls()).toHaveLength(2));
    mock.ready();
    await waitFor(() => expect(rowButton("Glass Pad").textContent).toBe("Ⅱ"));

    fireEvent.keyDown(screen.getByLabelText("Search presets"), { key: "Escape" });
    await waitFor(() => expect(stopCalls()).toHaveLength(1));
    await waitFor(() => expect(screen.queryByLabelText("Search presets")).toBeNull());
    await waitFor(() => expect(store.auditionActive()).toBe(false));
  });
});

