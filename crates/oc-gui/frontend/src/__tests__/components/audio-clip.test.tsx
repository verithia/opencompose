/**
 * Audio clip timeline block: waveform request, trim, move and split wiring.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import {
  invokeMock,
  makeAudioClip,
  makeSnapshot,
  makeTrack,
  getLastCallArgs,
  TRANSPORT_STOPPED,
} from "../setup";
import { store } from "../../stores/project";
import { resetPeaks } from "../../stores/audioPeaks";
import { resetGridDivision } from "../../stores/grid";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer } from "../../components/ContextMenu";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;

function peakPayload(): string {
  const level = {
    bucket_frames: 256,
    min: [-0.5, 0, 0.25],
    max: [0.5, 0.1, 0.25],
    rms: [0.3, 0.05, 0.25],
  };
  const body = 8 + 3 * 3 * 4;
  const bytes = new Uint8Array(30 + body);
  const view = new DataView(bytes.buffer);
  bytes.set([0x4f, 0x43, 0x50, 0x4b], 0);
  view.setUint8(4, 1);
  view.setUint32(14, 48_000, true);
  view.setBigUint64(18, 768n, true);
  view.setUint32(26, body, true);
  view.setUint32(30, level.bucket_frames, true);
  view.setUint32(34, level.min.length, true);
  let cursor = 38;
  for (const values of [level.min, level.max, level.rms]) {
    for (const value of values) {
      view.setFloat32(cursor, value, true);
      cursor += 4;
    }
  }
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function audioSnapshot() {
  return makeSnapshot({
    tracks: [makeTrack("tape", 0)],
    patterns: [],
    properties: {
      tempo: 120,
      time_signature: { numerator: 4, denominator: 4 },
      key: null,
      swing: null,
      tempo_map: [],
      time_sig_map: [],
      key_map: [],
      audio_clips: [
        makeAudioClip({
          gain_db: -3,
          fade_in_seconds: 0.1,
          fade_out_seconds: 0.2,
        }),
      ],
    },
  });
}

function closeContextMenu() {
  document.body.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
}

function menuItem(label: string): HTMLButtonElement | undefined {
  const find = () => [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")].find(
    (item) => item.querySelector(".context-menu-label")?.textContent === label,
  );
  const first = find();
  if (first) return first;
  for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
    if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
  }
  return find();
}

function stubGeometry(container: HTMLElement) {
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

async function mountClip(positionBeats = 0, snapshot = audioSnapshot()) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") {
      return { ...TRANSPORT_STOPPED(), position_beats: positionBeats };
    }
    if (cmd === "audio_clip_peaks") return peakPayload();
    if (cmd === "transport_seek") {
      return { ...TRANSPORT_STOPPED(), position_beats: positionBeats };
    }
    return snapshot;
  });
  await store.initProject();
  await store.seek(positionBeats);
  const view = render(() => (
    <>
      <Timeline />
      <ContextMenuLayer />
    </>
  ));
  stubGeometry(view.container as HTMLElement);
  const block = view.container.querySelector(".audio-clip-block") as HTMLElement;
  return { ...view, block };
}

function clipCalls(cmd: string) {
  return invokeMock.mock.calls.filter(([name]) => name === cmd);
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetPeaks();
  resetGridDivision();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    clearRect: vi.fn(),
    fillRect: vi.fn(),
    beginPath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    stroke: vi.fn(),
    setTransform: vi.fn(),
    globalAlpha: 1,
    lineWidth: 1,
    strokeStyle: "",
    fillStyle: "",
  } as unknown as CanvasRenderingContext2D);
});

afterEach(async () => {
  closeContextMenu();
  // A drag leaves a one-tick click swallower on the document; let it expire.
  await new Promise((resolve) => setTimeout(resolve, 0));
  vi.restoreAllMocks();
});

describe("audio clip block", () => {
  it("draws and splits an actual interval whose musical anchors are equal", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips![0] = makeAudioClip({
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 1, beat: 1, subdivision: 0 } },
      placement: { start_offset_seconds: 1, end_offset_seconds: 3 },
    });
    const { block } = await mountClip(4, snapshot);
    expect(parseFloat(block.style.left)).toBeCloseTo(48);
    expect(parseFloat(block.style.width)).toBeCloseTo(96);
    fireEvent.contextMenu(block, { clientX: 70, clientY: 40 });
    const split = menuItem("Split at playhead");
    expect(split).toBeTruthy();
    expect(split!.disabled).toBe(false);
    fireEvent.click(split!);
    await waitFor(() => expect(getLastCallArgs("split_audio_clip")).toMatchObject({
      clipIndex: 0, atBar: 2, atBeat: 1, atSub: 0,
    }));
  });

  it("identifies the trimmed edge when returning a fine-shifted head to its existing anchor", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips![0].placement = {
      start_offset_seconds: 0.125, end_offset_seconds: 0.25,
    };
    const { block } = await mountClip(0, snapshot);
    const handle = block.querySelector(".audio-clip-trim.left") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 6, clientY: 40 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 0, clientY: 40 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 0, clientY: 40 });
    expect(getLastCallArgs("trim_audio_clip")).toMatchObject({
      clipIndex: 0, startBar: 1, startBeat: 1, startSub: 0,
      endBar: 3, endBeat: 1, endSub: 0, edge: "start",
    });
  });

  it("draws fine placement while keeping coarse move callbacks on nominal anchors", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips![0].placement = {
      start_offset_seconds: 0.125, end_offset_seconds: 0.25,
    };
    const { block } = await mountClip(0, snapshot);
    expect(parseFloat(block.style.left)).toBeCloseTo(6);
    expect(parseFloat(block.style.width)).toBeCloseTo(198);
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 20, clientY: 40 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 44, clientY: 40 });
    expect(parseFloat(block.style.left)).toBeCloseTo(30);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 44, clientY: 40 });
    expect(getLastCallArgs("move_audio_clip")).toMatchObject({ startBar: 1, startBeat: 2, startSub: 0 });
  });

  it("draws the block and requests peaks once per file", async () => {
    const { block, container } = await mountClip();
    expect(block).toBeTruthy();
    expect(block.querySelector("canvas.audio-clip-waveform")).toBeTruthy();
    expect(block.textContent).toContain("hit.wav");
    expect(block.textContent).toContain("-3dB");
    await waitFor(() => expect(clipCalls("audio_clip_peaks").length).toBe(1));
    expect(getLastCallArgs("audio_clip_peaks")).toMatchObject({ file: "media/hit.wav" });
    // One bar is 96px at 24px/beat; [1:1 - 3:1] spans 192px from the lane start.
    expect(block.style.left).toBe("0px");
    expect(block.style.width).toBe("192px");
    expect(container.querySelectorAll(".audio-clip-block").length).toBe(1);
  });

  it("shares one peaks request between two clips on the same file", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips = [
      makeAudioClip({ name: "a" }),
      makeAudioClip({ name: "b", time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 6, beat: 1, subdivision: 0 } } }),
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "audio_clip_peaks") return peakPayload();
      return snapshot;
    });
    await store.initProject();
    const view = render(() => <Timeline />);
    stubGeometry(view.container as HTMLElement);
    await waitFor(() => expect(clipCalls("audio_clip_peaks").length).toBe(1));
    expect(view.container.querySelectorAll(".audio-clip-block").length).toBe(2);
  });

  it("drags a warp marker without changing its source time or writing during the drag", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips![0].warp_markers = [
      { source_seconds: 0.5, target_seconds: 0.5 },
    ];
    const { block } = await mountClip(0, snapshot);
    const marker = block.querySelector(".audio-clip-warp-marker") as HTMLElement;
    fireEvent.pointerDown(marker, { button: 0, pointerId: 1, clientX: 24, clientY: 34 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 34 });
    expect(clipCalls("set_audio_clip_playback")).toHaveLength(0);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 34 });
    expect(clipCalls("set_audio_clip_playback")).toHaveLength(1);
    expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      clipIndex: 0, playback: { warp_markers: [{ source_seconds: 0.5, target_seconds: 1 }] },
    });
  });

  it("submits the revision from pointer-down if the project changes during a warp drag", async () => {
    const snapshot = audioSnapshot();
    snapshot.revision = 10;
    snapshot.project.properties.audio_clips![0].warp_markers = [
      { source_seconds: 0.5, target_seconds: 0.5 },
    ];
    const { block } = await mountClip(0, snapshot);
    const marker = block.querySelector(".audio-clip-warp-marker") as HTMLElement;
    fireEvent.pointerDown(marker, { button: 0, pointerId: 1, clientX: 24, clientY: 34 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: 34 });
    store.applyPreviewSnapshot({ ...snapshot, revision: 11 });
    expect(store.projectRevision).toBe(11);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: 34 });
    expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({ expectedRevision: 10 });
  });

  it("keeps an off-grid warp anchor when the pointer stays at or returns to its starting x", async () => {
    const snapshot = audioSnapshot();
    snapshot.project.properties.audio_clips![0].warp_markers = [
      { source_seconds: 0.5, target_seconds: 0.512 },
    ];
    const { block } = await mountClip(0, snapshot);
    const marker = block.querySelector(".audio-clip-warp-marker") as HTMLElement;
    const originalLeft = marker.style.left;
    const down = { button: 0, pointerId: 1, clientX: 24, clientY: 34 };
    const vertical = { pointerId: 1, clientX: 24, clientY: 50 };

    fireEvent.pointerDown(marker, down);
    fireEvent.pointerMove(document, vertical);
    fireEvent.pointerUp(document, vertical);
    expect(marker.style.left).toBe(originalLeft);
    expect(clipCalls("set_audio_clip_playback")).toHaveLength(0);

    fireEvent.pointerDown(marker, down);
    fireEvent.pointerMove(document, { ...vertical, clientX: 48 });
    expect(marker.style.left).not.toBe(originalLeft);
    fireEvent.pointerMove(document, vertical);
    expect(marker.style.left).toBe(originalLeft);
    fireEvent.pointerUp(document, vertical);
    expect(clipCalls("set_audio_clip_playback")).toHaveLength(0);
  });

  it("stretches from the selected clip’s visible handle without a modifier", async () => {
    const { block } = await mountClip();
    fireEvent.click(block);
    const handle = block.querySelector(".audio-clip-stretch-handle") as HTMLElement;
    expect(handle).toBeTruthy();
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 188, clientY: 55 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 284, clientY: 55 });
    expect(clipCalls("set_audio_clip_playback")).toHaveLength(0);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 284, clientY: 55 });
    expect(clipCalls("trim_audio_clip")).toHaveLength(0);
    expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      clipIndex: 0, playback: { stretch_to: { bar: 4, beat: 1, subdivision: 0 } },
    });
  });

  it.each(["musical", "varispeed"] as const)(
    "stretches %s on Alt-drag of the right edge with one edit on release",
    async (mode) => {
      const snapshot = audioSnapshot();
      snapshot.project.properties.audio_clips![0].stretch_mode = mode;
      const { block } = await mountClip(0, snapshot);
      const handle = block.querySelector(".audio-clip-trim.right") as HTMLElement;
      fireEvent.pointerDown(handle, { button: 0, pointerId: 1, altKey: true, clientX: 192, clientY: 32 });
      fireEvent.pointerMove(document, { pointerId: 1, clientX: 96, clientY: 32 });
      expect(clipCalls("set_audio_clip_playback")).toHaveLength(0);
      fireEvent.pointerUp(document, { pointerId: 1, clientX: 96, clientY: 32 });
      expect(clipCalls("set_audio_clip_playback")).toHaveLength(1);
      expect(clipCalls("trim_audio_clip")).toHaveLength(0);
      expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
        clipIndex: 0, playback: { stretch_to: { bar: 2, beat: 1, subdivision: 0 } },
      });
    },
  );

  it("trims the clip edge once on release", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-trim.right") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 192, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 150, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 96, clientY: LANE_TOP + 10 });
    expect(clipCalls("trim_audio_clip").length).toBe(0);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 96, clientY: LANE_TOP + 10 });

    expect(clipCalls("trim_audio_clip").length).toBe(1);
    expect(getLastCallArgs("trim_audio_clip")).toMatchObject({
      clipIndex: 0,
      startBar: 1, startBeat: 1, startSub: 0,
      endBar: 2, endBeat: 1, endSub: 0,
    });
  });

  it("trims the start edge and keeps the end", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-trim.left") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    expect(clipCalls("trim_audio_clip").length).toBe(1);
    expect(getLastCallArgs("trim_audio_clip")).toMatchObject({
      clipIndex: 0,
      startBar: 1, startBeat: 3, startSub: 0,
      endBar: 3, endBeat: 1, endSub: 0,
    });
  });

  it("moves once on release at the snapped grid position", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 30, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    expect(clipCalls("move_audio_clip").length).toBe(0);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });

    expect(clipCalls("move_audio_clip").length).toBe(1);
    expect(getLastCallArgs("move_audio_clip")).toMatchObject({
      clipIndex: 0,
      startBar: 1, startBeat: 3, startSub: 0,
      track: "tape",
    });
  });

  it("writes nothing when the pointer is cancelled mid-drag", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    expect(block.style.left).toBe("48px");

    // The release never arrives: the pointer was taken away.
    fireEvent.pointerCancel(window, { pointerId: 1 });
    expect(block.style.left).toBe("0px");
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    expect(clipCalls("move_audio_clip").length).toBe(0);
  });

  it("writes nothing when the window loses focus mid-drag", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    fireEvent.blur(window);
    expect(block.style.left).toBe("0px");
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    expect(clipCalls("move_audio_clip").length).toBe(0);
  });

  it("refuses a drop below the last lane instead of retargeting the clip", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 300 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 300 });

    expect(clipCalls("move_audio_clip").length).toBe(0);
  });

  it("starts a fresh drag when a press arrives before the last release", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 30 });
    // The first release never arrives, so the second press owns the gesture.
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 30 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 96, clientY: LANE_TOP + 30 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 96, clientY: LANE_TOP + 30 });

    expect(clipCalls("move_audio_clip").length).toBe(1);
    expect(getLastCallArgs("move_audio_clip")).toMatchObject({
      clipIndex: 0, startBar: 2, startBeat: 1, startSub: 0,
    });
  });

  it("deletes the selected clip from the keyboard", async () => {
    const { block } = await mountClip();
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 10, clientY: LANE_TOP + 30 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 10, clientY: LANE_TOP + 30 });
    expect(block.classList.contains("selected")).toBe(true);
    fireEvent.keyDown(document, { key: "Delete" });
    await waitFor(() => expect(clipCalls("remove_audio_clip").length).toBe(1));
    expect(getLastCallArgs("remove_audio_clip")).toMatchObject({ clipIndex: 0 });
  });

  it("splits at the playhead from the context menu", async () => {
    const { block } = await mountClip(4);
    fireEvent.contextMenu(block, { clientX: 40, clientY: LANE_TOP + 10 });
    await waitFor(() => expect(menuItem("Split at playhead")).toBeTruthy());
    expect(menuItem("Split at playhead")!.getAttribute("aria-disabled")).toBeNull();
    menuItem("Split at playhead")!.click();
    await waitFor(() => expect(clipCalls("split_audio_clip").length).toBe(1));
    expect(getLastCallArgs("split_audio_clip")).toMatchObject({
      clipIndex: 0, atBar: 2, atBeat: 1, atSub: 0,
    });
    // The split commits without seeking the transport.
    expect(clipCalls("transport_seek").length).toBe(1);
  });

  it("disables the split entry when the playhead is outside the clip", async () => {
    const { block } = await mountClip(0);
    fireEvent.contextMenu(block, { clientX: 40, clientY: LANE_TOP + 10 });
    await waitFor(() => expect(menuItem("Split at playhead")).toBeTruthy());
    const split = menuItem("Split at playhead")!;
    expect(split.getAttribute("aria-disabled")).toBe("true");
    split.click();
    expect(clipCalls("split_audio_clip").length).toBe(0);
  });
});
