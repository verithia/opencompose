/**
 * Automation rows under the track lanes.
 *
 * Geometry comes from the same beat-to-pixel mapping the clips use, a clip's
 * span is a block, a whole-timeline lane is not, a track without lanes shows
 * the help line, and every point gesture reaches one store method with the
 * snapped position. A muted lane shows its reason and writes nothing.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { getLastCallArgs, invokeMock, makePattern, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { resetGridDivision } from "../../stores/grid";
import { i18n } from "../../stores/i18n";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import {
  AutomationLaneRow,
  type AutomationLaneRowProps,
} from "../../components/TimelineAutomation";
import { AUTOMATION_ROW_HEIGHT, laneDomain, lanePolyline } from "../../utils/automationRow";
import type { AutomationCurve, AutomationLane, BarBeat } from "../../types/project";

const LANE_PARAM = "compression.threshold";

/** Quarters in a 4/4 project for a point on the beat grid. */
function beatQuarter(position: BarBeat): number {
  return (position.bar - 1) * 4 + (position.beat - 1);
}

/** A polyline's vertices, parsed back into pixel positions. */
function vertices(points: string): Array<{ x: number; y: number }> {
  return points
    .split(" ")
    .filter((vertex) => vertex.length > 0)
    .map((vertex) => {
      const [x, y] = vertex.split(",").map(Number);
      return { x, y };
    });
}

/** A lane with two bare numbers, so its row domain is exactly -0.08..1.08. */
function lane(overrides: Partial<AutomationLane> = {}): AutomationLane {
  return {
    param: LANE_PARAM,
    points: [
      { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Number: 0 } },
      { position: { bar: 2, beat: 1, subdivision: 0 }, value: { Number: 1 } },
    ],
    ...overrides,
  };
}

/** A row mounted on its own, with spies for every gesture. */
function rowProps(overrides: Partial<AutomationLaneRowProps> = {}): AutomationLaneRowProps {
  return {
    lane: lane(),
    laneIndex: 0,
    pxPerBeat: 24,
    width: 4000,
    selectedPoint: null,
    onSelectPoint: () => {},
    onAddPoint: () => {},
    onMovePoint: () => {},
    onDeletePoint: () => {},
    onCurve: () => {},
    onTension: () => {},
    onToggleMute: () => {},
    blockSelected: false,
    onSelectBlock: () => {},
    onMoveBlock: () => {},
    onResizeBlock: () => {},
    onDuplicateBlock: () => {},
    onCopyBlock: () => {},
    onSplitBlock: () => {},
    onDeleteBlock: () => {},
    ...overrides,
  };
}

/** The row's own rect: content coordinates start at the row's left edge. */
function stubRowRect(container: HTMLElement) {
  for (const row of container.querySelectorAll<HTMLElement>(".automation-lane")) {
    row.getBoundingClientRect = () =>
      ({
        left: 0,
        top: 0,
        right: 4000,
        bottom: 38,
        width: 4000,
        height: 38,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }) as DOMRect;
  }
}

/** Accept a track list (wrapped into a snapshot) or a whole snapshot. */
function installBackend(project: unknown[] | ReturnType<typeof makeSnapshot>) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return Array.isArray(project) ? makeSnapshot({ tracks: project } as never) : project;
  });
}

function commandCount(name: string): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

async function mountTimeline(project: unknown[] | ReturnType<typeof makeSnapshot>) {
  installBackend(project);
  await store.initProject();
  // The track menu is a document-level layer, mounted beside the timeline.
  const view = render(() => (
    <>
      <Timeline />
      <ContextMenuLayer />
    </>
  ));
  stubRowRect(view.container as HTMLElement);
  return view;
}

/**
 * Open the automation area of the first track. The toggle lives in the track's
 * right-click menu, not on the header.
 */
function openAutomation(container: HTMLElement) {
  // A clip menu from an earlier step would take the next right click.
  fireEvent.keyDown(document, { key: "Escape" });
  const header = container.querySelector(".track-header") as HTMLElement;
  fireEvent.contextMenu(header, { clientX: 10, clientY: 10 });
  const entry = [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")].find(
    (item) => item.querySelector(".context-menu-label")?.textContent === i18n.t("timeline.automation.show"),
  );
  expect(entry, "the show-automation entry").toBeTruthy();
  fireEvent.click(entry as HTMLElement);
}

/** A track whose only lane is `overrides`. */
function trackWithLane(overrides: Partial<AutomationLane> = {}, name = "Piano") {
  return { ...makeTrack(name, 0), automation: [lane(overrides)] };
}

/** The span a clip block covers: bars 1-2, so quarters 0..8. */
const CLIP_RANGE = {
  start: { bar: 1, beat: 1, subdivision: 0 },
  end: { bar: 3, beat: 1, subdivision: 0 },
};

/** A track whose only lane is a clip. */
function trackWithClipLane(name = "Piano") {
  return trackWithLane({ time_range: CLIP_RANGE }, name);
}

/** The backend for one project, with a playhead at `playheadBeats`. */
function installClipBackend(project: unknown[], playheadBeats = 0) {
  const snapshot = makeSnapshot({ tracks: project } as never);
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "transport_seek") {
      return { ...TRANSPORT_STOPPED(), position_beats: playheadBeats };
    }
    return snapshot;
  });
  return snapshot;
}

/** One menu entry by the text it shows. */
function menuItem(text: string): HTMLElement {
  const find = () => [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((item) => item.textContent?.includes(text));
  const first = find();
  if (first) return first;
  for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
    if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
  }
  return find()!;
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetGridDivision();
  i18n.setLocale("en");
  await store.initProject();
});

afterEach(() => {
  closeContextMenu();
});

describe("automation row geometry", () => {
  it("puts a point's x at the shared beat-to-pixel scale", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    // A point at bar 2 beat 1 is quarter 4, so its x is 4 x pxPerBeat.
    for (const pxPerBeat of [6, 24, 120]) {
      const view = render(() => <AutomationLaneRow {...rowProps({ pxPerBeat })} />);
      const points = [...view.container.querySelectorAll<HTMLElement>(".automation-point")];
      expect(points.length, "one element per point").toBe(2);
      expect(points[0].style.left).toBe("0px");
      expect(points[1].style.left, `at ${pxPerBeat}px/beat`).toBe(`${4 * pxPerBeat}px`);
      view.unmount();
    }
  });

  it("draws a clip span as a block and a whole-timeline lane without one", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();

    const clip = lane({
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    });
    const withSpan = render(() => <AutomationLaneRow {...rowProps({ lane: clip })} />);
    const clipRow = withSpan.container.querySelector(".automation-lane") as HTMLElement;
    expect(clipRow.getAttribute("data-span")).toBe("clip");
    const block = withSpan.container.querySelector(".automation-span") as HTMLElement;
    expect(block, "a clip draws a block").toBeTruthy();
    expect(block.style.left).toBe("0px");
    // Two bars of 4/4 is eight quarters.
    expect(block.style.width).toBe(`${8 * 24}px`);
    withSpan.unmount();

    const whole = render(() => <AutomationLaneRow {...rowProps()} />);
    const wholeRow = whole.container.querySelector(".automation-lane") as HTMLElement;
    expect(wholeRow.getAttribute("data-span")).toBe("whole");
    expect(whole.container.querySelector(".automation-span"), "no block").toBeNull();
    expect(whole.container.querySelector(".automation-polyline")?.getAttribute("points"))
      .toBe("0.00,35.38 96.00,2.62");
    whole.unmount();
  });
});

describe("automation curve shapes", () => {
  const DOMAIN = { min: 0, max: 1 };
  const HEIGHT = AUTOMATION_ROW_HEIGHT;

  /** One segment from value 0 to value 1, shaped by `curve`. */
  function segment(curve: AutomationCurve, tension: number | null = null): AutomationLane {
    return lane({
      points: [
        {
          position: { bar: 1, beat: 1, subdivision: 0 },
          value: { Number: 0 },
          curve,
          tension,
        },
        { position: { bar: 2, beat: 1, subdivision: 0 }, value: { Number: 1 } },
      ],
    });
  }

  function drawnPoints(overrides: AutomationLane): string {
    return lanePolyline(overrides, beatQuarter, 24, DOMAIN, HEIGHT);
  }

  it("samples every curved segment and leaves a linear segment at its points", () => {
    expect(vertices(drawnPoints(lane())), "linear is one vertex per point").toHaveLength(2);
    for (const curve of ["half_sine", "smooth"] as const) {
      expect(vertices(drawnPoints(segment(curve))).length, curve).toBeGreaterThan(2);
    }
  });

  it("draws a stairs segment as a hold that jumps at the segment end", () => {
    const drawn = vertices(drawnPoints(segment("stairs")));
    // Hold at the first point's height across the whole segment, then drop.
    expect(drawn).toEqual([
      { x: 0, y: 38 },
      { x: 96, y: 38 },
      { x: 96, y: 0 },
    ]);
  });

  it("matches the engine's curve values at every sampled point", () => {
    // Playback evaluates a segment as from + (to - from) * shape(curve,
    // tension, progress), with `shape` defined in oc-dsl/src/automation.rs:
    // stairs holds, half sine is a half-sine ease, smooth blends towards a
    // smoothstep by tension, and an absent tension reads as 1.
    const engineShape = (curve: AutomationCurve, tension: number, progress: number): number => {
      if (curve === "stairs") return 0;
      if (curve === "half_sine") return 0.5 - 0.5 * Math.cos(Math.PI * progress);
      if (curve === "smooth") {
        const smoothstep = progress * progress * (3 - 2 * progress);
        return progress + (smoothstep - progress) * tension;
      }
      return progress;
    };
    const cases: Array<[AutomationCurve, number | null]> = [
      ["linear", null],
      ["stairs", null],
      ["half_sine", null],
      ["smooth", null],
      ["smooth", 0.25],
    ];
    for (const [curve, tension] of cases) {
      const drawn = vertices(drawnPoints(segment(curve, tension)));
      const first = drawn[0];
      const last = drawn[drawn.length - 1];
      // A shaped segment has interior samples to compare, not just endpoints.
      if (curve === "half_sine" || curve === "smooth") {
        expect(drawn.length, `${curve} samples`).toBeGreaterThan(2);
      }
      // Every vertex but the closing one belongs to the segment's own shape.
      for (const vertex of drawn.slice(0, -1)) {
        const progress = (vertex.x - first.x) / (last.x - first.x);
        const expected = engineShape(curve, tension ?? 1, progress);
        // The domain is 0..1 inverted into the row, so the value is 1 - y/h.
        expect(1 - vertex.y / HEIGHT, `${curve} tension=${tension} at ${progress}`)
          .toBeCloseTo(expected, 3);
      }
    }
    // Anchor values from the engine's closed form, so a mirrored-but-wrong
    // formula cannot agree with itself.
    expect(engineShape("half_sine", 1, 0.25)).toBeCloseTo(0.146446609406726, 12);
    expect(engineShape("smooth", 1, 0.25)).toBeCloseTo(0.15625, 12);
    expect(engineShape("smooth", 0.25, 0.25)).toBeCloseTo(0.2265625, 12);
    expect(engineShape("stairs", 1, 0.5)).toBe(0);
  });

  it("keeps the vertices ordered when a point sits past its neighbour", () => {
    // The lane stores the second point earlier in time: sorting by x is what
    // keeps the drawn shape from folding back on itself.
    const outOfOrder = lane({
      points: [
        {
          position: { bar: 2, beat: 1, subdivision: 0 },
          value: { Number: 1 },
          curve: "half_sine",
        },
        {
          position: { bar: 1, beat: 3, subdivision: 0 },
          value: { Number: 0 },
          curve: "smooth",
        },
        { position: { bar: 2, beat: 3, subdivision: 0 }, value: { Number: 0.5 } },
      ],
    });
    const xs = vertices(drawnPoints(outOfOrder)).map((vertex) => vertex.x);
    expect(xs).toEqual([...xs].sort((a, b) => a - b));
  });
});

describe("automation row gestures", () => {
  it("adds a point at the snapped position and the value under the pointer", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onAddPoint = vi.fn();
    const view = render(() => (
      <AutomationLaneRow {...rowProps({ onAddPoint })} />
    ));
    stubRowRect(view.container as HTMLElement);
    const row = view.container.querySelector(".automation-lane") as HTMLElement;

    // 96px at 24px/beat is quarter 4; half the row height is the domain's middle.
    fireEvent.dblClick(row, { clientX: 96, clientY: 19 });

    expect(onAddPoint).toHaveBeenCalledTimes(1);
    const [quarter, value] = onAddPoint.mock.calls[0];
    expect(quarter).toBe(4);
    expect(value).toBeCloseTo(0.5, 9);
    view.unmount();
  });

  it("moves a point in both axes and commits once on release", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onMovePoint = vi.fn();
    const onSelectPoint = vi.fn();
    const view = render(() => (
      <AutomationLaneRow {...rowProps({ onMovePoint, onSelectPoint })} />
    ));
    stubRowRect(view.container as HTMLElement);
    const point = view.container.querySelector(".automation-point") as HTMLElement;

    fireEvent.pointerDown(point, { button: 0, pointerId: 1, clientX: 0, clientY: 35 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 72, clientY: 0 });
    expect(view.container.querySelector(".automation-ghost"), "a ghost follows").toBeTruthy();
    expect(onMovePoint, "no write while dragging").not.toHaveBeenCalled();
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 72, clientY: 0 });

    expect(onSelectPoint).toHaveBeenCalledWith(0);
    expect(onMovePoint).toHaveBeenCalledTimes(1);
    const [index, quarter, value] = onMovePoint.mock.calls[0];
    expect(index).toBe(0);
    expect(quarter).toBe(3);
    expect(value).toBeCloseTo(1.08, 9);
    expect(view.container.querySelector(".automation-ghost"), "the ghost is cleared").toBeNull();
    view.unmount();
  });

  it("authors a gain point across the track's whole range, not the points' band", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onMovePoint = vi.fn();
    // A volume lane sitting in 0..-12 dB: its own band would stop at 0 dB.
    const gain = lane({
      param: "volume",
      points: [
        { position: { bar: 1, beat: 1, subdivision: 0 }, value: { Db: 0 } },
        { position: { bar: 2, beat: 1, subdivision: 0 }, value: { Db: -12 } },
      ],
    });
    const view = render(() => (
      <AutomationLaneRow {...rowProps({ lane: gain, onMovePoint })} />
    ));
    stubRowRect(view.container as HTMLElement);
    const point = view.container.querySelector(".automation-point") as HTMLElement;

    // The row's top is the track gain's +6 dB ceiling, not the points' band.
    fireEvent.pointerDown(point, { button: 0, pointerId: 3, clientX: 0, clientY: 30 });
    fireEvent.pointerMove(document, { pointerId: 3, clientX: 0, clientY: 0 });
    fireEvent.pointerUp(document, { pointerId: 3, clientX: 0, clientY: 0 });

    expect(onMovePoint).toHaveBeenCalledTimes(1);
    const [, , value] = onMovePoint.mock.calls[0];
    expect(value).toBeCloseTo(6, 9);
    // A pan lane reads the mixer's hard left..hard right range instead.
    expect(laneDomain({ ...gain, param: "pan" })).toEqual({ min: -1, max: 1 });
    view.unmount();
  });

  it("scales an unknown target to the points it carries", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    // A control lane with no mixer range keeps the padded point band.
    expect(laneDomain(lane({ param: "modulation" }))).toEqual({
      min: -0.08,
      max: 1.08,
    });
  });

  it("abandons a point drag on Escape, pointercancel, blur and unmount", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();

    /** Start a drag that has already moved away from the point. */
    function startDrag() {
      const onMovePoint = vi.fn();
      const view = render(() => <AutomationLaneRow {...rowProps({ onMovePoint })} />);
      stubRowRect(view.container as HTMLElement);
      const point = view.container.querySelector(".automation-point") as HTMLElement;
      fireEvent.pointerDown(point, { button: 0, pointerId: 7, clientX: 0, clientY: 35 });
      fireEvent.pointerMove(document, { pointerId: 7, clientX: 72, clientY: 0 });
      expect(view.container.querySelector(".automation-ghost"), "the drag started").toBeTruthy();
      return { view, onMovePoint };
    }

    /** The release the user did not mean to commit. */
    function release() {
      fireEvent.pointerUp(document, { pointerId: 7, clientX: 72, clientY: 0 });
    }

    const escape = startDrag();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(escape.view.container.querySelector(".automation-ghost"), "Escape clears the ghost").toBeNull();
    release();
    expect(escape.onMovePoint, "Escape commits nothing").not.toHaveBeenCalled();
    // The listeners are gone, so a later move cannot revive the drag.
    fireEvent.pointerMove(document, { pointerId: 7, clientX: 48, clientY: 19 });
    expect(escape.view.container.querySelector(".automation-ghost")).toBeNull();
    escape.view.unmount();

    const cancel = startDrag();
    fireEvent.pointerCancel(document, { pointerId: 7 });
    expect(cancel.view.container.querySelector(".automation-ghost"), "cancel clears the ghost").toBeNull();
    release();
    expect(cancel.onMovePoint, "pointercancel commits nothing").not.toHaveBeenCalled();
    cancel.view.unmount();

    const blur = startDrag();
    fireEvent(window, new Event("blur"));
    expect(blur.view.container.querySelector(".automation-ghost"), "blur clears the ghost").toBeNull();
    release();
    expect(blur.onMovePoint, "blur commits nothing").not.toHaveBeenCalled();
    blur.view.unmount();

    const unmounted = startDrag();
    unmounted.view.unmount();
    release();
    expect(unmounted.onMovePoint, "an unmounted row commits nothing").not.toHaveBeenCalled();
  });

  it("deletes the selected point from the timeline command", async () => {
    const view = await mountTimeline([trackWithLane()]);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    const point = view.container.querySelector(".automation-point") as HTMLElement;

    // A click selects without writing.
    fireEvent.pointerDown(point, { button: 0, pointerId: 1, clientX: 0, clientY: 35 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 0, clientY: 35 });
    expect(commandCount("move_automation_point"), "a click never writes").toBe(0);

    fireEvent.keyDown(document, { key: "Delete" });

    await vi.waitFor(() => expect(commandCount("remove_automation_point")).toBe(1));
    expect(getLastCallArgs("remove_automation_point")).toMatchObject({
      target: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
      pointIndex: 0,
    });
  });

  it("adds and moves through the store from the timeline row", async () => {
    const view = await mountTimeline([trackWithLane()]);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    const row = view.container.querySelector(".automation-lane") as HTMLElement;

    fireEvent.dblClick(row, { clientX: 96, clientY: 19 });

    await vi.waitFor(() => expect(commandCount("add_automation_point")).toBe(1));
    const added = getLastCallArgs("add_automation_point")!;
    expect(added).toMatchObject({
      target: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
      positionBeats: 4,
    });
    expect(added.value as number).toBeCloseTo(0.5, 9);

    const point = view.container.querySelector(".automation-point") as HTMLElement;
    fireEvent.pointerDown(point, { button: 0, pointerId: 1, clientX: 0, clientY: 35 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 19 });

    await vi.waitFor(() => expect(commandCount("move_automation_point")).toBe(1));
    expect(getLastCallArgs("move_automation_point")).toMatchObject({
      target: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
      pointIndex: 0,
      positionBeats: 2,
    });
  });

  it("offers the four curves and a tension control on a right-click", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onCurve = vi.fn();
    const onTension = vi.fn();
    const view = render(() => (
      <>
        <AutomationLaneRow {...rowProps({ onCurve, onTension })} />
        <ContextMenuLayer />
      </>
    ));
    const point = view.container.querySelector(".automation-point") as HTMLElement;

    fireEvent.contextMenu(point, { clientX: 10, clientY: 10 });

    const labels = [...document.querySelectorAll(".context-menu-label")]
      .map((item) => item.textContent ?? "");
    for (const curve of ["Linear", "Stairs", "Half sine", "Smooth"]) {
      expect(labels, curve).toContain(curve);
    }
    expect(labels).toContain(i18n.t("timeline.automation.tension"));
    expect(onCurve, "opening the menu writes nothing").not.toHaveBeenCalled();

    const smooth = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((item) => item.textContent?.includes("Smooth"))!;
    fireEvent.click(smooth);

    expect(onCurve).toHaveBeenCalledWith(0, "smooth", 1);
    view.unmount();
  });

  it("asks for a tension value from the point menu", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onTension = vi.fn();
    const view = render(() => (
      <>
        <AutomationLaneRow {...rowProps({ onTension })} />
        <ContextMenuLayer />
      </>
    ));
    const point = view.container.querySelector(".automation-point") as HTMLElement;
    fireEvent.contextMenu(point, { clientX: 10, clientY: 10 });
    const item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((row) => row.textContent?.includes(i18n.t("timeline.automation.tension")))!;
    fireEvent.click(item);
    expect(onTension).toHaveBeenCalledWith(0);
    view.unmount();
  });
});

describe("automation clip block gestures", () => {
  it("drags a block by a snapped delta and commits once on release", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onMoveBlock = vi.fn();
    const onSelectBlock = vi.fn();
    const view = render(() => (
      <AutomationLaneRow
        {...rowProps({
          lane: lane({ time_range: CLIP_RANGE }),
          onMoveBlock,
          onSelectBlock,
        })}
      />
    ));
    stubRowRect(view.container as HTMLElement);
    const block = view.container.querySelector(".automation-span") as HTMLElement;

    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: 19 });
    // 48px at 24px/beat is two quarters, so the block starts on bar 1 beat 3.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 19 });
    expect(
      view.container.querySelector("[data-automation-block-ghost]"),
      "a ghost follows the pointer",
    ).toBeTruthy();
    expect(onMoveBlock, "no write while dragging").not.toHaveBeenCalled();
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 19 });

    expect(onSelectBlock).toHaveBeenCalledTimes(1);
    expect(onMoveBlock).toHaveBeenCalledTimes(1);
    expect(onMoveBlock.mock.calls[0][0]).toEqual({ bar: 1, beat: 3, subdivision: 0 });
    expect(
      view.container.querySelector("[data-automation-block-ghost]"),
      "the ghost is cleared",
    ).toBeNull();
    expect(block.style.left, "the block waits for the snapshot").toBe("0px");
    view.unmount();
  });

  it("abandons a block drag when the window loses focus", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onMoveBlock = vi.fn();
    const view = render(() => (
      <AutomationLaneRow
        {...rowProps({ lane: lane({ time_range: CLIP_RANGE }), onMoveBlock })}
      />
    ));
    stubRowRect(view.container as HTMLElement);
    const block = view.container.querySelector(".automation-span") as HTMLElement;

    fireEvent.pointerDown(block, { button: 0, pointerId: 5, clientX: 0, clientY: 19 });
    fireEvent.pointerMove(document, { pointerId: 5, clientX: 48, clientY: 19 });
    expect(view.container.querySelector("[data-automation-block-ghost]")).toBeTruthy();

    fireEvent(window, new Event("blur"));
    expect(
      view.container.querySelector("[data-automation-block-ghost]"),
      "blur clears the ghost",
    ).toBeNull();
    fireEvent.pointerUp(document, { pointerId: 5, clientX: 48, clientY: 19 });
    expect(onMoveBlock, "blur commits nothing").not.toHaveBeenCalled();
    view.unmount();
  });

  it("drags either block edge to resize it", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onResizeBlock = vi.fn();
    const view = render(() => (
      <AutomationLaneRow
        {...rowProps({ lane: lane({ time_range: CLIP_RANGE }), onResizeBlock })}
      />
    ));
    stubRowRect(view.container as HTMLElement);

    // The tail sits at quarter 8 (192px); 240px is quarter 10, bar 3 beat 3.
    const end = view.container.querySelector(".automation-resize-handle.end") as HTMLElement;
    fireEvent.pointerDown(end, { button: 0, pointerId: 2, clientX: 192, clientY: 19 });
    fireEvent.pointerMove(document, { pointerId: 2, clientX: 240, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 2, clientX: 240, clientY: 19 });
    expect(onResizeBlock).toHaveBeenCalledTimes(1);
    expect(onResizeBlock.mock.calls[0][0]).toBe("end");
    expect(onResizeBlock.mock.calls[0][1]).toEqual({ bar: 3, beat: 3, subdivision: 0 });

    // The head sits at 0; 24px is quarter 1, bar 1 beat 2.
    const start = view.container.querySelector(".automation-resize-handle.start") as HTMLElement;
    fireEvent.pointerDown(start, { button: 0, pointerId: 3, clientX: 0, clientY: 19 });
    fireEvent.pointerMove(document, { pointerId: 3, clientX: 24, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 3, clientX: 24, clientY: 19 });
    expect(onResizeBlock).toHaveBeenCalledTimes(2);
    expect(onResizeBlock.mock.calls[1][0]).toBe("start");
    expect(onResizeBlock.mock.calls[1][1]).toEqual({ bar: 1, beat: 2, subdivision: 0 });
    view.unmount();
  });

  it("copies instead of moving while Alt is held", async () => {
    installBackend([makeTrack("Piano", 0)]);
    await store.initProject();
    const onMoveBlock = vi.fn();
    const onDuplicateBlock = vi.fn();
    const view = render(() => (
      <AutomationLaneRow
        {...rowProps({
          lane: lane({ time_range: CLIP_RANGE }),
          onMoveBlock,
          onDuplicateBlock,
        })}
      />
    ));
    stubRowRect(view.container as HTMLElement);
    const block = view.container.querySelector(".automation-span") as HTMLElement;

    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: 19 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 19, altKey: true });
    const ghost = view.container.querySelector("[data-automation-block-ghost]") as HTMLElement;
    expect(ghost.classList.contains("copy"), "the ghost marks the copy").toBe(true);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 19 });

    expect(onDuplicateBlock).toHaveBeenCalledTimes(1);
    expect(onDuplicateBlock.mock.calls[0][0]).toEqual({ bar: 1, beat: 3, subdivision: 0 });
    expect(onMoveBlock, "nothing is moved").not.toHaveBeenCalled();
    view.unmount();
  });

  it("splits a clip at the playhead from the block menu", async () => {
    installClipBackend([trackWithClipLane()], 4);
    await store.initProject();
    const view = render(() => (
      <>
        <Timeline />
        <ContextMenuLayer />
      </>
    ));
    stubRowRect(view.container as HTMLElement);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    await store.seek(4);
    expect(store.transport().position_beats).toBe(4);

    const block = view.container.querySelector(".automation-span") as HTMLElement;
    fireEvent.contextMenu(block, { clientX: 10, clientY: 10 });
    fireEvent.click(menuItem(i18n.t("menu.clip.split")));

    await vi.waitFor(() => expect(commandCount("split_automation_clip")).toBe(1));
    // Quarter 4 is bar 2 beat 1.
    expect(getLastCallArgs("split_automation_clip")).toMatchObject({
      owner: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
      atBar: 2,
      atBeat: 1,
      atSubdivision: 0,
    });
    // The menu also offers the other three block gestures.
    fireEvent.contextMenu(block, { clientX: 10, clientY: 10 });
    const labels = [...document.querySelectorAll(".context-menu-label")]
      .map((item) => item.textContent ?? "");
    expect(labels).toContain(i18n.t("timeline.automation.copy"));
    expect(labels).toContain(i18n.t("menu.clip.duplicate"));
    expect(labels).toContain(i18n.t("menu.clip.delete"));
    view.unmount();
  });

  it("shows the backend message and keeps the block when a move is refused", async () => {
    installClipBackend([trackWithClipLane()]);
    await store.initProject();
    const view = render(() => (
      <>
        <Timeline />
        <ContextMenuLayer />
      </>
    ));
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    const refusal = "Automation clip [2:1 - 4:1] would overlap [1:1 - 3:1] on 'compression.threshold'";
    const snapshot = makeSnapshot({ tracks: [trackWithClipLane()] } as never);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "move_automation_clip") throw new Error(refusal);
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return snapshot;
    });

    const block = view.container.querySelector(".automation-span") as HTMLElement;
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: 19 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 19 });

    await vi.waitFor(() => expect(commandCount("move_automation_clip")).toBe(1));
    await vi.waitFor(() => expect(store.error()).toContain(refusal));
    expect(block.style.left, "the block snaps back").toBe("0px");
    view.unmount();
  });

  it("offers the block gestures in the clip menu of the same track", async () => {
    const project = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        clips: [{ pattern: "beat", time_range: CLIP_RANGE }],
        automation: [lane({ time_range: CLIP_RANGE })],
      }],
      patterns: [makePattern("beat")],
    } as never);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return project;
    });
    await store.initProject();
    const view = render(() => (
      <>
        <Timeline />
        <ContextMenuLayer />
      </>
    ));
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);

    // Selecting the block puts its gestures in the clip menu, where they fit.
    const block = view.container.querySelector(".automation-span") as HTMLElement;
    fireEvent.pointerDown(block, { button: 0, pointerId: 9, clientX: 0, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 9, clientX: 0, clientY: 19 });

    const clip = view.container.querySelector(".clip-block") as HTMLElement;
    fireEvent.contextMenu(clip, { clientX: 10, clientY: 10 });

    // A clip right-click opens the clip menu only: appending the automation
    // block's menu produced two headings and duplicate actions.
    for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
      if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
    }
    const labels = [...document.querySelectorAll(".context-menu-label")]
      .map((item) => item.textContent ?? "");
    for (const label of [i18n.t("menu.clip.duplicate"), i18n.t("menu.clip.split"), i18n.t("menu.clip.delete")]) {
      expect(labels, label).toContain(label);
    }
    expect(labels).not.toContain(i18n.t("timeline.automation.copy"));
  });

  it("drops a block pick when another document replaces the project", async () => {
    const first = {
      ...makeSnapshot({ tracks: [trackWithClipLane()] } as never),
      file_path: "/tmp/first.ocp",
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return first;
    });
    await store.initProject();
    const view = render(() => (
      <>
        <Timeline />
        <ContextMenuLayer />
      </>
    ));
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);

    const block = view.container.querySelector(".automation-span") as HTMLElement;
    fireEvent.pointerDown(block, { button: 0, pointerId: 9, clientX: 0, clientY: 19 });
    fireEvent.pointerUp(document, { pointerId: 9, clientX: 0, clientY: 19 });

    // The second document has an automation clip at the same index and range,
    // so an index-only pick would delete that one instead.
    const second = {
      ...makeSnapshot({ tracks: [trackWithClipLane()] } as never),
      file_path: "/tmp/second.ocp",
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return second;
    });
    await store.initProject();
    const lanes = JSON.parse(JSON.stringify(store.project.tracks[0].automation));

    fireEvent.keyDown(document, { key: "Delete" });

    expect(commandCount("delete_automation_clip"), "the stale pick is dropped").toBe(0);
    expect(store.blockSelection()).toEqual([]);
    expect(store.project.tracks[0].automation, "the second document is unchanged").toEqual(lanes);
  });

  it("keeps the clip gestures disabled on a whole-timeline lane but offers delete", async () => {
    installBackend([trackWithLane()]);
    await store.initProject();
    const view = render(() => (
      <>
        <Timeline />
        <ContextMenuLayer />
      </>
    ));
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    const row = view.container.querySelector(".automation-lane") as HTMLElement;

    fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });

    const reason = i18n.t("timeline.automation.wholeTimeline");
    for (const label of [
      i18n.t("timeline.automation.copy"),
      i18n.t("menu.clip.duplicate"),
      i18n.t("menu.clip.split"),
    ]) {
      const item = menuItem(label);
      expect(item, label).toBeTruthy();
      expect(item.getAttribute("aria-disabled"), label).toBe("true");
      expect(item.querySelector(".context-menu-hint")?.textContent, label).toBe(reason);
    }

    // The lane itself can still be removed, through its own command.
    const remove = menuItem(i18n.t("menu.clip.delete"));
    expect(remove.getAttribute("aria-disabled"), "delete is offered").toBeNull();
    fireEvent.click(remove);
    await vi.waitFor(() => expect(commandCount("remove_automation_lane")).toBe(1));
    expect(getLastCallArgs("remove_automation_lane")).toMatchObject({
      owner: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
    });
  });
});

describe("empty and muted rows", () => {
  it("shows the help line for a track with no lanes", async () => {
    const view = await mountTimeline([makeTrack("Piano", 0)]);
    const emptyBefore = view.container.querySelector(".automation-empty-lane");
    expect(emptyBefore, "collapsed by default").toBeNull();

    openAutomation(view.container as HTMLElement);

    const help = view.container.querySelector(".automation-empty-lane") as HTMLElement;
    expect(help).toBeTruthy();
    expect(help.textContent).toBe(i18n.t("timeline.automation.empty"));
    expect(view.container.querySelector(".automation-lane"), "no row without lanes").toBeNull();
  });

  it("says a lane is muted and refuses every edit", async () => {
    const view = await mountTimeline([trackWithLane({ muted: true })]);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);

    const note = view.container.querySelector(".automation-muted-note") as HTMLElement;
    expect(note.textContent).toBe(i18n.t("timeline.automation.muted"));
    const mute = view.container.querySelector(".automation-mute-btn") as HTMLElement;
    expect(mute.getAttribute("aria-pressed")).toBe("true");

    const row = view.container.querySelector(".automation-lane") as HTMLElement;
    fireEvent.dblClick(row, { clientX: 96, clientY: 19 });
    const point = view.container.querySelector(".automation-point") as HTMLElement;
    fireEvent.pointerDown(point, { button: 0, pointerId: 1, clientX: 0, clientY: 35 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 72, clientY: 0 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 72, clientY: 0 });
    // A muted lane's point still selects; Delete then refuses as well.
    fireEvent.keyDown(document, { key: "Delete" });

    expect(commandCount("add_automation_point"), "no point is added").toBe(0);
    expect(commandCount("move_automation_point"), "no point is moved").toBe(0);
    expect(commandCount("remove_automation_point"), "no point is removed").toBe(0);
  });

  it("toggles a lane's mute from the row header", async () => {
    const view = await mountTimeline([trackWithLane()]);
    openAutomation(view.container as HTMLElement);
    const mute = view.container.querySelector(".automation-mute-btn") as HTMLElement;
    expect(mute.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(mute);

    await vi.waitFor(() => expect(commandCount("set_automation_lane_mute")).toBe(1));
    expect(getLastCallArgs("set_automation_lane_mute")).toMatchObject({
      target: "track:0",
      laneParam: LANE_PARAM,
      laneIndex: 0,
      muted: true,
    });
  });

  it("draws bus and master lanes with no track header to open", async () => {
    const snapshot = makeSnapshot({
      tracks: [makeTrack("Piano", 0)],
      buses: [{ name: "Drums bus", routes: ["master"], effects: [], automation: [lane()] }],
      master: {
        effects: [],
        loudness_target: null,
        volume_db: 0,
        automation: [lane({ param: "fx1.mix" })],
      },
    });
    const view = await mountTimeline(snapshot);

    const column = view.container.querySelector(".timeline-lane-column") as HTMLElement;
    const strips = [...column.querySelectorAll<HTMLElement>(".automation-owner-strip")];
    expect(strips.map((strip) => strip.textContent)).toEqual([
      i18n.t("timeline.automation.owner", { owner: "Drums bus" }),
      i18n.t("timeline.automation.owner", { owner: i18n.t("mixer.master") }),
    ]);
    const rows = [...column.querySelectorAll<HTMLElement>(".automation-lane")];
    expect(rows.map((row) => row.getAttribute("data-lane"))).toEqual([LANE_PARAM, "fx1.mix"]);
  });

  it("follows the timeline zoom", async () => {
    const view = await mountTimeline([trackWithLane()]);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);
    const second = () =>
      [...view.container.querySelectorAll<HTMLElement>(".automation-point")][1];
    expect(second().style.left, "24 px/beat").toBe("96px");

    const zoomIn = [...view.container.querySelectorAll<HTMLElement>(".zoom-step")]
      .find((button) => button.textContent === "+")!;
    fireEvent.click(zoomIn);

    expect(second().style.left, "28 px/beat").toBe("112px");
  });
});

describe("one pick at a time", () => {
  /** A track with a point lane and one named clip under it. */
  function trackWithPointLaneAndClip() {
    return {
      ...trackWithLane(),
      patterns: [],
      clips: [{ pattern: "beat", time_range: CLIP_RANGE }],
    };
  }

  it("drops a point pick when a clip is clicked, so Delete removes the clip", async () => {
    const view = await mountTimeline([trackWithPointLaneAndClip()]);
    openAutomation(view.container as HTMLElement);
    stubRowRect(view.container as HTMLElement);

    const point = view.container.querySelector(".automation-point") as HTMLElement;
    expect(point).toBeTruthy();
    fireEvent.pointerDown(point, { button: 0, pointerId: 21, clientX: 12, clientY: 12 });
    fireEvent.pointerUp(document, { pointerId: 21, clientX: 12, clientY: 12, button: 0 });
    expect(point.classList.contains("selected")).toBe(true);

    // Picking the clip takes the pick from the point, as it takes it from the
    // clip selection.
    const clip = view.container.querySelector(".clip-block") as HTMLElement;
    expect(clip).toBeTruthy();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 22, clientX: 0, clientY: 12 });
    fireEvent.pointerUp(document, { pointerId: 22, clientX: 0, clientY: 12, button: 0 });
    expect(store.blockSelection().length).toBe(1);
    expect(view.container.querySelector(".automation-point.selected")).toBeFalsy();

    fireEvent.keyDown(document, { key: "Delete" });

    await vi.waitFor(() => expect(commandCount("remove_clip")).toBe(1));
    expect(commandCount("remove_automation_point"), "the stale point keeps its value").toBe(0);
  });
});
