/**
 * Clip/pattern naming: every creation path must yield a non-empty, unique,
 * DSL-quotable name that survives the project snapshot round-trip.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import {
  invokeMock, makeSnapshot, makeTrack, makePattern, getLastCallArgs, TRANSPORT_STOPPED,
} from "./setup";
import { store } from "../stores/project";
import { sanitizeName, uniqueName } from "../utils/naming";
import { Timeline } from "../components/Timeline";
import { PatternPanel } from "../components/PatternPanel";
import { ContextMenuLayer } from "../components/ContextMenu";

function clipTrack(name: string, pattern: string) {
  return {
    ...makeTrack(name, 0),
    phrases: [],
    clips: [{
      pattern,
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }],
  };
}

function archiveCalls(cmd: string) {
  return invokeMock.mock.calls.filter(([name]) => name === cmd);
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot({ patterns: [makePattern("beat")], tracks: [makeTrack("Drums", 0)] });
  });
  await store.initProject();
});

describe("naming", () => {
  it("derives unique numbered names instead of duplicates", () => {
    expect(uniqueName("Piano", [])).toBe("Piano");
    expect(uniqueName("Piano", ["Piano"])).toBe("Piano 2");
    expect(uniqueName("Piano", ["Piano", "Piano 2"])).toBe("Piano 3");
    expect(uniqueName("  Piano  ", ["Piano"])).toBe("Piano 2");
  });

  it("never returns an empty name and strips characters the DSL cannot quote", () => {
    expect(uniqueName("", [])).not.toBe("");
    expect(uniqueName("   ", [])).not.toBe("");
    const cleaned = sanitizeName('  my\tclip "x"\n ');
    expect(cleaned).toBe("my clip x");
    expect(cleaned).not.toContain('"');
  });
});

describe("pattern creation paths", () => {
  it("names a context-menu pattern after its track and keeps it unique", async () => {
    let patterns = [makePattern("Drums")];
    const tracks = [makeTrack("Drums", 0)];
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "add_pattern") {
        patterns = [...patterns, makePattern(String(args?.name))];
        return makeSnapshot({ patterns, tracks });
      }
      if (cmd === "add_clip") return makeSnapshot({ patterns, tracks });
      return makeSnapshot({ patterns, tracks });
    });
    await store.initProject();

    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const lane = container.querySelector(".track-header") as HTMLElement;
    fireEvent.contextMenu(lane, { clientX: 30, clientY: 30 });
    const item = [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
      .find((b) => b.querySelector(".context-menu-label")?.textContent === "Create pattern here");
    expect(item).toBeTruthy();
    fireEvent.click(item!);
    await new Promise((resolve) => setTimeout(resolve, 0));

    const added = getLastCallArgs("add_pattern");
    expect(added?.name).toBe("Drums 2");
    expect(getLastCallArgs("add_clip")?.patternName).toBe("Drums 2");
  });

  it("creates the next free name in the pattern panel instead of failing", async () => {
    const patterns = [makePattern("fill")];
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "add_pattern") {
        return makeSnapshot({ patterns: [...patterns, makePattern(String(args?.name))], tracks: [] });
      }
      return makeSnapshot({ patterns, tracks: [] });
    });
    await store.initProject();

    const { getByLabelText, getByPlaceholderText, getByText } = render(() => <><PatternPanel /><ContextMenuLayer /></>);
    fireEvent.click(getByLabelText("New pattern"));
    fireEvent.input(getByPlaceholderText("name"), { target: { value: "fill" } });
    fireEvent.click(getByText("Create"));

    expect(getLastCallArgs("add_pattern")?.name).toBe("fill 2");
  });
});

describe("clip creation paths", () => {
  it("preserves the source pattern name when duplicating", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ patterns: [makePattern("beat")], tracks: [clipTrack("Drums", "beat")] });
    });
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const clip = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.click(clip);
    fireEvent.keyDown(document, { key: "d", metaKey: true });

    const added = archiveCalls("add_clip");
    expect(added.length).toBe(1);
    expect(getLastCallArgs("add_clip")?.patternName).toBe("beat");
  });

  it("preserves the source pattern name when pasting", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ patterns: [makePattern("beat")], tracks: [clipTrack("Drums", "beat")] });
    });
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const clip = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.click(clip);
    fireEvent.keyDown(document, { key: "c", metaKey: true });
    fireEvent.keyDown(document, { key: "v", metaKey: true });

    expect(archiveCalls("add_clip").length).toBe(1);
    expect(getLastCallArgs("add_clip")?.patternName).toBe("beat");
  });

  it("keeps the pattern name after a drag move", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ patterns: [makePattern("beat")], tracks: [clipTrack("Drums", "beat")] });
    });
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;
    ruler.getBoundingClientRect = () => ({ left: 0, top: 0, right: 2000, bottom: 22, width: 2000, height: 22, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
    const lane = container.querySelector(".track-lane") as HTMLElement;
    lane.getBoundingClientRect = () => ({ left: 0, top: 22, right: 2000, bottom: 86, width: 2000, height: 64, x: 0, y: 22, toJSON: () => ({}) }) as DOMRect;
    const clip = container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 30 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 30 });

    expect(getLastCallArgs("move_clip")).toMatchObject({ trackIndex: 0, clipIndex: 0 });
    expect(store.project.tracks[0].clips[0].pattern).toBe("beat");
  });

  it("keeps a created name through a project snapshot round-trip", async () => {
    // The backend serializes names as quoted literals when they are not bare
    // identifiers; reopening must hand the same name back to the store.
    const snapshot = makeSnapshot({ patterns: [makePattern("Drums 2")], tracks: [] });
    snapshot.dsl_source = '@project "Round"\n  tempo: 120\n  time_signature: 4/4\n\n@pattern "Drums 2" | instrument: drums/acoustic_kit\n';
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return snapshot;
    });
    await store.initProject();

    expect(store.project.patterns[0].name).toBe("Drums 2");
    expect(store.dslSource()).toContain('@pattern "Drums 2"');
  });
});
