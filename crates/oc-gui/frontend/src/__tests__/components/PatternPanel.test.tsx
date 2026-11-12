/**
 * PatternPanel interaction tests.
 * Tests pattern selection, filtering, and property display.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, makePattern, makeNote } from "../setup";
import { store } from "../../stores/project";
import { PatternPanel } from "../../components/PatternPanel";

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    return makeSnapshot({
      tracks: [makeTrack("Piano", 0)],
      patterns: [
        makePattern("drum_beat"),
        { ...makePattern("fill"), name: "fill", hits: [], events: [] },
        { ...makePattern("bass_line"), name: "bass_line", instrument: { category: "bass", name: "electric" }, hits: [], events: [{ start_beat: 0, explicit_position: false, pitch: { note: "E", accidental: "Natural", octave: 2 }, duration: { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] }, dynamics: "MF", velocity: null, articulation: null, vibrato: null, expression: null, pitch_bend: null, muted: false, grace: false, text: null, modifiers: [] }] },
      ],
    });
  });
  await store.initProject();
});

describe("PatternPanel", () => {
  it("renders without crashing", () => {
    const { container } = render(() => <PatternPanel />);
    expect(container).toBeTruthy();
  });

  it("displays all patterns", () => {
    const { container } = render(() => <PatternPanel />);
    const text = container.textContent;
    expect(text).toContain("drum_beat");
    expect(text).toContain("fill");
    expect(text).toContain("bass_line");
  });

  it("shows note/hit counts for patterns", () => {
    const { container } = render(() => <PatternPanel />);
    const text = container.textContent;
    // drum_beat has 2 hits
    expect(text).toContain("2 hits");
    // bass_line has 1 note
    expect(text).toContain("1 note");
    // fill is empty
    expect(text).toContain("empty");
  });

  it("clicking a pattern selects it", () => {
    const { container } = render(() => <PatternPanel />);
    const items = container.querySelectorAll(".pattern-item");
    expect(items.length).toBe(3);

    fireEvent.click(items[0]);
    expect(store.selectedPatternName()).toBe("drum_beat");
  });

  it("selected pattern gets highlighted styling", () => {
    store.setSelectedPatternName("fill");
    const { container } = render(() => <PatternPanel />);
    const items = container.querySelectorAll(".pattern-item");

    // Find the fill item
    let fillItem: Element | null = null;
    for (const item of items) {
      if (item.textContent?.includes("fill")) { fillItem = item; break; }
    }
    expect(fillItem?.classList.contains("selected")).toBe(true);
  });

  it("shows filter input", () => {
    const { container } = render(() => <PatternPanel />);
    const input = container.querySelector("input[type='text'], input[placeholder]");
    expect(input).toBeTruthy();
  });

  it("filtering narrows the list", () => {
    const { container } = render(() => <PatternPanel />);
    const input = container.querySelector("input") as HTMLInputElement;
    if (!input) return;

    fireEvent.input(input, { target: { value: "bass" } });
    // Only bass_line should be visible
    const items = container.querySelectorAll(".pattern-item");
    expect(items.length).toBe(1);
    expect(items[0].textContent).toContain("bass_line");
  });

  it("shows empty state when filter matches nothing", () => {
    const { container } = render(() => <PatternPanel />);
    const input = container.querySelector("input") as HTMLInputElement;
    if (!input) return;

    fireEvent.input(input, { target: { value: "nonexistent_xyz" } });
    const text = container.textContent;
    expect(text).toContain("No matching patterns");
  });

  it("shows instrument info for each pattern", () => {
    const { container } = render(() => <PatternPanel />);
    const text = container.textContent;
    // drum_beat has drums/acoustic_kit
    expect(text).toContain("drums");
    // bass_line has bass/electric
    expect(text).toContain("bass");
  });

  it("pattern items are draggable", () => {
    const { container } = render(() => <PatternPanel />);
    const items = container.querySelectorAll(".pattern-item");
    for (const item of items) {
      expect(item.getAttribute("draggable")).toBe("true");
    }
  });

  it("shows empty state when no patterns exist", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      return makeSnapshot({ tracks: [], patterns: [] });
    });
    await store.initProject();

    const { container } = render(() => <PatternPanel />);
    expect(container.textContent).toContain("No named patterns");
  });

  async function renderLongPattern() {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        patterns: [{
          ...makePattern("long"),
          bars: 2,
          hits: [],
          events: [makeNote()],
        }],
      });
    });
    await store.initProject();
    store.setSelectedPatternName("long");
    return render(() => <PatternPanel />);
  }

  const lengthInput = (container: HTMLElement) =>
    container.querySelector(".pattern-length-input") as HTMLInputElement;
  const button = (container: HTMLElement, label: string) =>
    [...container.querySelectorAll("button")].find((element) => element.textContent === label)!;

  it("asks before shortening a pattern", async () => {
    const { container } = await renderLongPattern();
    expect(lengthInput(container).value).toBe("2");

    fireEvent.change(lengthInput(container), { target: { value: "1" } });

    expect(container.querySelector(".pattern-length-prompt")).toBeTruthy();
    expect(container.textContent).toContain("past the new end");
    // The field does not advertise a length the document has not taken.
    expect(lengthInput(container).value).toBe("2");
  });

  it("keeps out-of-range notes as one undo entry", async () => {
    const { container } = await renderLongPattern();
    fireEvent.change(lengthInput(container), { target: { value: "1" } });
    fireEvent.click(button(container, "Keep notes"));
    await new Promise((resolve) => setTimeout(resolve, 0));

    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_pattern_bars");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ patternName: "long", bars: 1 });
  });

  it("trims to the new length as one undo entry", async () => {
    const { container } = await renderLongPattern();
    fireEvent.change(lengthInput(container), { target: { value: "1" } });
    fireEvent.click(button(container, "Trim notes"));
    await new Promise((resolve) => setTimeout(resolve, 0));

    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "trim_pattern_to_bars");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ patternName: "long", bars: 1 });
  });

  const nameInput = (container: HTMLElement) =>
    container.querySelector(".pattern-properties input") as HTMLInputElement;

  async function renderQuotedPattern(rename: (patternName: string, newName: string) => Promise<unknown>) {
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      if (cmd === "rename_pattern") {
        return rename(String(args?.patternName), String(args?.newName));
      }
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        patterns: [{ ...makePattern("Beat 2"), name: "Beat 2", hits: [], events: [] }],
      });
    });
    await store.initProject();
    store.setSelectedPatternName("Beat 2");
    return render(() => <PatternPanel />);
  }

  it("renames a quoted pattern and keeps it selected", async () => {
    const { container } = await renderQuotedPattern(async (_patternName, newName) =>
      makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        patterns: [{ ...makePattern(newName), name: newName, hits: [], events: [] }],
      }),
    );

    const input = nameInput(container);
    fireEvent.input(input, { target: { value: "Verse 2" } });
    fireEvent.blur(input);
    await new Promise((resolve) => setTimeout(resolve, 0));

    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "rename_pattern");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ patternName: "Beat 2", newName: "Verse 2" });
    expect(store.selectedPatternName()).toBe("Verse 2");
    expect(nameInput(container).value).toBe("Verse 2");
  });

  it("puts the old name back when the rename is refused", async () => {
    const { container } = await renderQuotedPattern(async () => {
      throw new Error("A pattern named 'Beat 2' already exists");
    });

    const input = nameInput(container);
    fireEvent.input(input, { target: { value: "Beat 2!" } });
    fireEvent.blur(input);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(store.error()).toContain("already exists");
    expect(input.value).toBe("Beat 2");
    expect(store.selectedPatternName()).toBe("Beat 2");
  });
});

describe("pattern swing", () => {
  async function renderSwing(projectSwing: number | null) {
    let swing: number | null = null;
    const defaults = makeSnapshot().project.properties;
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      if (cmd === "set_pattern_swing") swing = args?.swing as number | null;
      return makeSnapshot({
        properties: { ...defaults, swing: projectSwing },
        patterns: [{ ...makePattern("beat"), swing }],
      });
    });
    await store.initProject();
    store.setSelectedPatternName("beat");
    return render(() => <PatternPanel />);
  }

  it("shows the inherited value, writes explicit zero, and clears back to inheritance", async () => {
    const view = await renderSwing(62);
    const input = () => view.getByRole("spinbutton", { name: "Swing" }) as HTMLInputElement;
    expect(input().value).toBe("");
    expect(view.getByText("Project 62%")).toBeTruthy();

    fireEvent.change(input(), { target: { value: "0" } });
    await vi.waitFor(() => expect(view.getByText("Clear to inherit")).toBeTruthy());
    expect(input().value).toBe("0");

    fireEvent.change(input(), { target: { value: "" } });
    await vi.waitFor(() => expect(view.getByText("Project 62%")).toBeTruthy());
    expect(input().value).toBe("");
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_pattern_swing")).toEqual([
      ["set_pattern_swing", { patternName: "beat", swing: 0 }],
      ["set_pattern_swing", { patternName: "beat", swing: null }],
    ]);
  });

  it("writes the project swing grid through the subdivision command", async () => {
    let subdivision: string | null = null;
    const defaults = makeSnapshot().project.properties;
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      if (cmd === "set_project_swing_subdivision") {
        subdivision = args?.subdivision as string;
        return makeSnapshot({
          properties: { ...defaults, swing_subdivision: "sixteenth" },
          patterns: [makePattern("beat")],
        });
      }
      return makeSnapshot({ properties: defaults, patterns: [makePattern("beat")] });
    });
    await store.initProject();
    store.setSelectedPatternName("beat");
    const view = render(() => <PatternPanel />);
    const select = view.getByRole("combobox", { name: "Swing grid" }) as HTMLSelectElement;
    expect(select.value).toBe("8th");

    fireEvent.change(select, { target: { value: "16th" } });
    await vi.waitFor(() => expect(select.value).toBe("16th"));
    expect(subdivision).toBe("16th");
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_project_swing_subdivision")).toEqual([
      ["set_project_swing_subdivision", { subdivision: "16th" }],
    ]);
  });

  it("shows straight timing when the project has no swing", async () => {
    const view = await renderSwing(null);
    const input = view.getByRole("spinbutton", { name: "Swing" }) as HTMLInputElement;
    expect(input.value).toBe("");
    expect(view.getByText("Straight (50%)")).toBeTruthy();
  });
});
