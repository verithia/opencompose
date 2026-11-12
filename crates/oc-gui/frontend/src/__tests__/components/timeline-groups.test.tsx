/**
 * Clip groups in the timeline: selection, marquee, drag, the clip menu and the
 * badge. The engine-facing rules (same delta, pruning, the bar-1 refusal, the
 * DSL round trip) are covered by the Rust command and DSL suites; here the
 * surface is what the user touches.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makePattern, makeTrack, getLastCallArgs } from "../setup";
import { store } from "../../stores/project";
import { resetGridDivision, setGridDivision } from "../../stores/grid";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import { i18n } from "../../stores/i18n";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

interface BackendState {
  tracks: any[];
  patterns: any[];
  groups?: { id: string; members: string[] }[];
}

/** Two clips on one track: quarters 0-8 and 8-16, so 24 px per beat. */
function baseState(overrides: Partial<BackendState> = {}): BackendState {
  return {
    patterns: [makePattern("beat")],
    tracks: [{
      ...makeTrack("Keys", 0),
      phrases: [],
      clips: [
        { pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) },
        { pattern: "beat", time_range: range([3, 1, 0], [5, 1, 0]) },
      ],
    }],
    ...overrides,
  };
}

function groupedState(): BackendState {
  const state = baseState();
  state.tracks[0].clips[0].id = "c1";
  state.tracks[0].clips[1].id = "c2";
  state.groups = [{ id: "g1", members: ["c1", "c2"] }];
  return state;
}

function installBackend(state: BackendState, handlers: Record<string, (a: any) => unknown> = {}) {
  invokeMock.mockImplementation(async (cmd: string, args?: any) => {
    const a = args ?? {};
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    const handler = handlers[cmd];
    if (handler) {
      const result = handler(a);
      if (result instanceof Error) throw result;
      if (result !== undefined) return result;
    }
    // The real backend serializes a fresh tree per reply, so hand back copies.
    return makeSnapshot({
      tracks: structuredClone(state.tracks),
      patterns: state.patterns,
      groups: state.groups ? structuredClone(state.groups) : undefined,
    });
  });
}

function stubGeometry(container: HTMLElement) {
  const column = container.querySelector(".timeline-lane-column") as HTMLElement;
  column.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: 2000, bottom: 2000, width: 2000, height: 2000, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  const ruler = container.querySelector(".timeline-ruler") as HTMLElement;
  ruler.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: 2000, bottom: LANE_TOP, width: 2000, height: LANE_TOP, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  const lanes = Array.from(container.querySelectorAll<HTMLElement>(".track-lane"));
  lanes.forEach((lane, index) => {
    const top = LANE_TOP + index * LANE_HEIGHT;
    lane.getBoundingClientRect = () =>
      ({ left: 0, top, right: 2000, bottom: top + LANE_HEIGHT, width: 2000, height: LANE_HEIGHT, x: 0, y: top, toJSON: () => ({}) }) as DOMRect;
  });
}

async function mount(state: BackendState, handlers: Record<string, (a: any) => unknown> = {}) {
  installBackend(state, handlers);
  await store.initProject();
  const view = render(() => <><Timeline /><ContextMenuLayer /></>);
  stubGeometry(view.container as HTMLElement);
  return view;
}

function lane(container: HTMLElement, index = 0): HTMLElement {
  return container.querySelectorAll<HTMLElement>(".track-lane")[index];
}

function clipBlocks(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(".clip-block:not(.clip-drag-ghost)"));
}

function menuItem(label: string): HTMLElement {
  let item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === label);
  if (!item) {
    for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
      if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
    }
    item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === label);
  }
  if (!item) {
    const labels = [...document.querySelectorAll<HTMLElement>(".context-menu-label")]
      .map((element) => element.textContent?.trim());
    throw new Error(`no menu item ${label}; menu has ${labels.join(", ")}`);
  }
  return item;
}

async function openClipMenu(container: HTMLElement, index = 0) {
  fireEvent.contextMenu(clipBlocks(container)[index], { clientX: 40, clientY: 40 });
  await Promise.resolve();
}

const settleClick = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(async () => {
  invokeMock.mockClear();
  resetGridDivision();
  closeContextMenu();
  i18n.setLocale("en");
  await store.initProject();
});

afterEach(async () => {
  closeContextMenu();
  await settleClick();
  store.setBlockSelection([]);
  store.setError(null);
  i18n.setLocale("en");
});

describe("group selection", () => {
  it("clicking one member selects every clip of its group", async () => {
    const view = await mount(groupedState());
    fireEvent.click(clipBlocks(view.container)[0]);

    expect(store.blockSelection()).toEqual([
      { kind: "clip", trackIndex: 0, index: 0 },
      { kind: "clip", trackIndex: 0, index: 1 },
    ]);
    expect(view.container.querySelectorAll(".clip-block.selected").length).toBe(2);
  });

  it("a marquee that touches one member takes the whole group", async () => {
    const view = await mount(groupedState());
    // x 0-100 covers only the first clip (0-192 px).
    fireEvent.pointerDown(lane(view.container), { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 100, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 100, clientY: LANE_TOP + 10 });

    expect(store.blockSelection()).toEqual([
      { kind: "clip", trackIndex: 0, index: 0 },
      { kind: "clip", trackIndex: 0, index: 1 },
    ]);
  });
});

describe("group drag", () => {
  it("dragging one member asks the backend to move the whole group", async () => {
    const state = groupedState();
    const view = await mount(state, {
      move_clip_group: () => {
        for (const clip of state.tracks[0].clips) {
          const start = clip.time_range.start;
          const quarters = (start.bar - 1) * 4 + (start.beat - 1) + start.subdivision / 4;
          const moved = quarters + 4;
          clip.time_range = {
            start: { bar: Math.floor(moved / 4) + 1, beat: (moved % 4) + 1, subdivision: 0 },
            end: { bar: Math.floor((moved + 8) / 4) + 1, beat: ((moved + 8) % 4) + 1, subdivision: 0 },
          };
        }
        return makeSnapshot({
          tracks: structuredClone(state.tracks),
          patterns: state.patterns,
          groups: state.groups,
        });
      },
    });
    const [first, second] = clipBlocks(view.container);

    fireEvent.pointerDown(first, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 24, clientY: LANE_TOP + 10 });
    // Both members follow the pointer, not just the grabbed one.
    expect(second.style.left).toBe("216px");
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 24, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "move_clip_group").length,
    ).toBe(1));
    expect(getLastCallArgs("move_clip_group")).toMatchObject({
      trackIndex: 0, clipIndex: 0, startBar: 1, startBeat: 2, startSub: 0,
    });
    await vi.waitFor(() => {
      expect(store.project.tracks[0].clips[0].time_range.start).toMatchObject({ bar: 2, beat: 1, subdivision: 0 });
      expect(store.project.tracks[0].clips[1].time_range.start).toMatchObject({ bar: 4, beat: 1, subdivision: 0 });
    });
  });

  it("shows the reason when the group refuses to cross bar 1", async () => {
    const view = await mount(groupedState(), {
      move_clip_group: () => new Error("clip 'beat' would start before bar 1"),
    });
    const [first] = clipBlocks(view.container);

    fireEvent.pointerDown(first, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: -240, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: -240, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(store.error()).toContain("did not move"));
    expect(store.error()).toContain("would start before bar 1");
  });
});

describe("group menu", () => {
  it("groups the selected clips and shows the badge", async () => {
    const state = baseState();
    const view = await mount(state, {
      group_clips: () => {
        state.tracks[0].clips[0].id = "c1";
        state.tracks[0].clips[1].id = "c2";
        state.groups = [{ id: "g1", members: ["c1", "c2"] }];
        const snapshot = makeSnapshot({
          tracks: structuredClone(state.tracks),
          patterns: state.patterns,
          groups: state.groups,
        });
        return {
          ...snapshot,
          dsl_source: '@project "t"\n\n@group g1 {\n  members: c1, c2\n}\n',
        };
      },
    });
    const [first, second] = clipBlocks(view.container);
    fireEvent.click(first);
    fireEvent.click(second, { shiftKey: true });
    expect(store.blockSelection().length).toBe(2);

    await openClipMenu(view.container, 0);
    fireEvent.click(menuItem("Group clips"));

    await vi.waitFor(() => expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "group_clips").length,
    ).toBe(1));
    expect(getLastCallArgs("group_clips")).toMatchObject({
      clips: [{ trackIndex: 0, clipIndex: 0 }, { trackIndex: 0, clipIndex: 1 }],
    });
    await vi.waitFor(() => expect(store.project.groups?.[0]?.members).toEqual(["c1", "c2"]));
    expect(view.container.querySelectorAll(".clip-group-badge").length).toBe(2);
  });

  it("disables Group for one clip and says what to do", async () => {
    const view = await mount(baseState());
    fireEvent.click(clipBlocks(view.container)[0]);
    await openClipMenu(view.container, 0);

    const item = menuItem("Group clips");
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toContain("two named clips");
  });

  it("disables Ungroup for a clip outside any group", async () => {
    const view = await mount(baseState());
    await openClipMenu(view.container, 0);

    const item = menuItem("Ungroup clips");
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toContain("not in a group");
  });

  it("ungroups a grouped clip through the menu", async () => {
    const state = groupedState();
    const view = await mount(state, {
      ungroup_clips: () => {
        state.tracks[0].clips.forEach((clip: any) => { delete clip.id; });
        state.groups = [];
        return makeSnapshot({
          tracks: structuredClone(state.tracks),
          patterns: state.patterns,
        });
      },
    });
    await openClipMenu(view.container, 0);
    const item = menuItem("Ungroup clips");
    expect(item.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(item);

    await vi.waitFor(() => expect(store.project.groups ?? []).toEqual([]));
    await vi.waitFor(() => expect(view.container.querySelectorAll(".clip-group-badge").length).toBe(0));
  });

  it("groups the selection from the keyboard", async () => {
    const state = baseState();
    const view = await mount(state, {
      group_clips: () => {
        state.tracks[0].clips[0].id = "c1";
        state.tracks[0].clips[1].id = "c2";
        state.groups = [{ id: "g1", members: ["c1", "c2"] }];
        return makeSnapshot({ tracks: structuredClone(state.tracks), patterns: state.patterns, groups: state.groups });
      },
    });
    const [first, second] = clipBlocks(view.container);
    fireEvent.click(first);
    fireEvent.click(second, { shiftKey: true });
    fireEvent.keyDown(view.container.querySelector(".timeline-panel") as HTMLElement, {
      key: "g", metaKey: true,
    });

    await vi.waitFor(() => expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "group_clips").length,
    ).toBe(1));
  });
});

describe("block-position note", () => {
  it("states the 16th floor when the grid is finer than a block position", async () => {
    setGridDivision(8);
    const view = await mount(baseState());
    const note = view.container.querySelector(".timeline-grid-floor");
    expect(note).toBeTruthy();
    expect(note?.textContent).toContain("1/16");
    expect(note?.textContent).toContain("1/32");
  });

  it("disappears when the grid is a block position", async () => {
    setGridDivision(4);
    const view = await mount(baseState());
    expect(view.container.querySelector(".timeline-grid-floor")).toBeFalsy();
  });
});
