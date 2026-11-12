/**
 * The pattern preset row: the dropdown writes the pattern's own preset, keeps
 * the chosen value, reports a pending write and surfaces a failure as text.
 * The hosting track's preset is never touched.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  invokeMock,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { PatternPanel, patternPresetValue } from "../../components/PatternPanel";
import type { Clip, ProjectSnapshot } from "../../types/project";

const CLIP: Clip = {
  pattern: "riff",
  time_range: {
    start: { bar: 1, beat: 1, subdivision: 0 },
    end: { bar: 2, beat: 1, subdivision: 0 },
  },
};

function preset(name: string) {
  return {
    name,
    source: "builtin",
    plugin: "builtin.open_synth",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params: [],
    effects: [],
  };
}

/** Two tracks host the pattern, both with their own preset. */
function sourceSnapshot(patternPreset: string | null = null): ProjectSnapshot {
  return makeSnapshot({
    presets: [preset("Piano 1"), preset("Lead"), preset("Drums 1")],
    patterns: [{ ...makePattern("riff"), preset: patternPreset }],
    tracks: [
      { ...makeTrack("Piano", 0), preset: "Piano 1", clips: [CLIP] },
      { ...makeTrack("Drums", 1), preset: "Drums 1", clips: [CLIP] },
    ],
  });
}

/** What the backend returns when the write lands. */
function writtenSnapshot(presetName: string | null): ProjectSnapshot {
  return sourceSnapshot(presetName);
}

function selectOf(container: HTMLElement): HTMLSelectElement {
  const select = container.querySelector<HTMLSelectElement>(".pattern-preset-select");
  if (!select) throw new Error("the pattern panel has no preset dropdown");
  return select;
}

beforeEach(async () => {
  i18n.setLocale("en");
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_history") return [];
    return sourceSnapshot();
  });
  await store.initProject();
  store.setSelectedPatternName("riff");
});

describe("the pattern's preset dropdown", () => {
  it("writes the pattern's preset and keeps the chosen value", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_history") return [];
      if (cmd === "set_pattern_preset") return writtenSnapshot("Lead");
      return sourceSnapshot();
    });
    const { container } = render(() => <PatternPanel />);
    const select = selectOf(container);

    fireEvent.change(select, { target: { value: "Lead" } });

    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.find(([cmd]) => cmd === "set_pattern_preset")?.[1]).toEqual({
        patternName: "riff",
        preset: "Lead",
      }),
    );
    // No spring-back: the row shows what was chosen once the snapshot lands.
    await vi.waitFor(() => expect(selectOf(container).value).toBe("Lead"));
    expect(store.project.patterns[0].preset).toBe("Lead");
    // The hosting tracks keep their own presets; only the pattern moved.
    expect(store.project.tracks[0].preset).toBe("Piano 1");
    expect(store.project.tracks[1].preset).toBe("Drums 1");
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain("set_track_preset");
  });

  it("clears the override by asking for no pattern preset", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_history") return [];
      if (cmd === "set_pattern_preset") return writtenSnapshot("oc_default_lead");
      return sourceSnapshot("Lead");
    });
    await store.initProject();
    store.setSelectedPatternName("riff");
    const { container } = render(() => <PatternPanel />);
    const select = selectOf(container);
    expect(select.value).toBe("Lead");

    fireEvent.change(select, { target: { value: "" } });

    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.find(([cmd]) => cmd === "set_pattern_preset")?.[1]).toEqual({
        patternName: "riff",
        preset: null,
      }),
    );
    // The resync injects a factory default; it is not an override.
    await vi.waitFor(() => expect(selectOf(container).value).toBe(""));
  });

  it("shows a pending state while the write is in flight", async () => {
    let release!: (snapshot: ProjectSnapshot) => void;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_history") return [];
      if (cmd === "set_pattern_preset") {
        return new Promise<ProjectSnapshot>((resolve) => {
          release = resolve;
        });
      }
      return sourceSnapshot();
    });
    const { container } = render(() => <PatternPanel />);
    const select = selectOf(container);

    fireEvent.change(select, { target: { value: "Lead" } });

    await vi.waitFor(() => expect(selectOf(container).disabled).toBe(true));
    expect(container.querySelector(".pattern-preset-pending")?.textContent).toBe("Saving…");

    release(writtenSnapshot("Lead"));
    await vi.waitFor(() => expect(selectOf(container).disabled).toBe(false));
    expect(selectOf(container).value).toBe("Lead");
    expect(container.querySelector(".pattern-preset-pending")).toBeNull();
  });

  it("surfaces a failed write as text and puts the row back", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_history") return [];
      if (cmd === "set_pattern_preset") throw new Error("pattern preset boom");
      return sourceSnapshot();
    });
    const { container } = render(() => <PatternPanel />);
    const select = selectOf(container);

    fireEvent.change(select, { target: { value: "Lead" } });

    await vi.waitFor(() => {
      const failure = container.querySelector(".pattern-preset-error");
      expect(failure?.getAttribute("role")).toBe("alert");
      expect(failure?.textContent).toContain("pattern preset boom");
    });
    expect(store.project.patterns[0].preset).toBeNull();
    expect(
      selectOf(container).value,
      "the row must not keep a value that was refused",
    ).toBe("");
  });

  it("shows a factory default as following the track", () => {
    expect(patternPresetValue({ ...makePattern("riff"), preset: "oc_default_lead" })).toBe("");
    expect(patternPresetValue({ ...makePattern("riff"), preset: "Lead" })).toBe("Lead");
    expect(patternPresetValue(makePattern("riff"))).toBe("");
  });
});
