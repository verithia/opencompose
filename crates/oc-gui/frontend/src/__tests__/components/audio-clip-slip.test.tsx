/**
 * Audio clip slip: Alt-drag moves the content under a fixed clip. One IPC call
 * per gesture, a live readout, the clamp at the hidden tail, and the inert
 * state when the clip already shows the whole file.
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

const LANE_TOP = 22;
const LANE_HEIGHT = 64;
const RATE = 48_000;

/** A one-level peak pyramid over `frames` of media, so the block knows its length. */
function peakPayload(frames: number): string {
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
  view.setUint32(14, RATE, true);
  view.setBigUint64(18, BigInt(frames), true);
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

/** A four-second clip (two bars at 120 BPM) over `mediaSeconds` of media. */
async function mountClip(mediaSeconds: number, offsetSeconds = 0) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
    if (cmd === "audio_clip_peaks") return peakPayload(RATE * mediaSeconds);
    if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
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
        audio_clips: [makeAudioClip({ offset_seconds: offsetSeconds })],
      },
    });
  });
  await store.initProject();
  await store.seek(0);
  const view = render(() => <Timeline />);
  stubGeometry(view.container as HTMLElement);
  const block = view.container.querySelector(".audio-clip-block") as HTMLElement;
  return { ...view, block };
}

function clipCalls(cmd: string) {
  return invokeMock.mock.calls.filter(([name]) => name === cmd);
}

function slipReadout(block: HTMLElement): string {
  return block.querySelector(".clip-envelope-readout")?.textContent ?? "";
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetPeaks();
  resetGridDivision();
  store.setError(null);
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
  // A drag leaves a one-tick click swallower on the document; let it expire.
  await new Promise((resolve) => setTimeout(resolve, 0));
  vi.restoreAllMocks();
});

describe("audio clip slip gesture", () => {
  it("writes the offset the readout showed, once, on release", async () => {
    const { block } = await mountClip(10);
    const revision = store.projectRevision;
    fireEvent.pointerDown(block, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 20, altKey: true,
    });
    // Dragging left hides content: 48 px is two beats, one second at 120 BPM.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 12, clientY: LANE_TOP + 20 });
    expect(slipReadout(block)).toContain("Content offset 1 s");
    expect(clipCalls("set_audio_clip_offset").length, "a drag is a preview").toBe(0);
    expect(store.applyAiSnapshot({ ...makeSnapshot({ ...store.project }), revision: revision + 1 })).toBe(true);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 12, clientY: LANE_TOP + 20 });

    expect(clipCalls("set_audio_clip_offset").length).toBe(1);
    expect(clipCalls("begin_undo_gesture").length).toBe(1);
    await waitFor(() => expect(clipCalls("end_undo_gesture").length).toBe(1));
    const args = getLastCallArgs("set_audio_clip_offset")!;
    expect(args.clipIndex).toBe(0);
    expect(args.expectedRevision).toBe(revision);
    expect(args.offsetSeconds as number).toBeCloseTo(1, 3);
    expect(block.querySelector(".clip-envelope-readout")).toBeNull();
  });

  it("clamps at the hidden tail and says so in the readout", async () => {
    const { block } = await mountClip(10);
    fireEvent.pointerDown(block, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 20, altKey: true,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: -1000, clientY: LANE_TOP + 20 });
    expect(slipReadout(block)).toContain("end of media");
    fireEvent.pointerUp(document, { pointerId: 1, clientX: -1000, clientY: LANE_TOP + 20 });

    // Ten seconds of media under a four-second clip leave six to slip.
    expect(getLastCallArgs("set_audio_clip_offset")!.offsetSeconds).toBeCloseTo(6, 3);
  });

  it("writes nothing when the drag returns to the stored offset", async () => {
    const { block } = await mountClip(10);
    fireEvent.pointerDown(block, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 20, altKey: true,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 12, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 20 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 20 });

    expect(clipCalls("set_audio_clip_offset").length).toBe(0);
    expect(clipCalls("begin_undo_gesture").length).toBe(0);
  });

  it("refuses a clip that already shows the whole file", async () => {
    // 16 ms of media under a four-second clip: nothing is hidden.
    const { block } = await mountClip(0.016);
    fireEvent.pointerDown(block, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 20, altKey: true,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 12, clientY: LANE_TOP + 20 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 12, clientY: LANE_TOP + 20 });

    expect(clipCalls("set_audio_clip_offset").length).toBe(0);
    expect(store.error()).toBe("Nothing to slip: this clip already shows the whole file.");
    expect(block.querySelector(".clip-envelope-readout")).toBeNull();
  });

  it("still moves the clip when Alt is not held", async () => {
    const { block } = await mountClip(10);
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 108, clientY: LANE_TOP + 20 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 108, clientY: LANE_TOP + 20 });

    expect(clipCalls("set_audio_clip_offset").length).toBe(0);
    expect(clipCalls("move_audio_clip").length).toBe(1);
  });
});

describe("audio clip slip readout and chip", () => {
  it("shows the hidden head on the block the project carries", async () => {
    const { block } = await mountClip(10, 1.5);
    await waitFor(() => expect(block.textContent).toContain("slip 1.5 s"));
  });
});
