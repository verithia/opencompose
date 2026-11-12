/**
 * Audio clip gain and fade handles: one IPC call per gesture, the live readout,
 * the 20 ms fade refusal, and the inert state when the media is gone.
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
import { CLIP_GAIN_MAX_DB } from "../../utils/waveform";

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

async function mountClip(options: { missingMedia?: boolean } = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
    if (cmd === "audio_clip_peaks") {
      if (options.missingMedia) throw new Error("media/hit.wav: no such file");
      return peakPayload();
    }
    if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
    return audioSnapshot();
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

function envelopeCalls() {
  return clipCalls("set_audio_clip_envelope");
}

/** The gain a readout says it is showing, e.g. "Clip gain -1.8 dB" -> -1.8. */
function readoutGain(block: HTMLElement): number {
  const text = block.querySelector(".clip-envelope-readout")?.textContent ?? "";
  const match = text.match(/-?\d+\.\d/);
  if (!match) throw new Error(`no gain in readout: ${JSON.stringify(text)}`);
  return Number(match[0]);
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

describe("audio clip envelope paint", () => {
  interface Op {
    op: string;
    x?: number;
    y?: number;
  }

  /** The last `beginPath`..`stroke` pair: the envelope path. */
  function envelopePath(ops: Op[]): Op[] {
    const start = ops.map((op) => op.op).lastIndexOf("beginPath");
    const end = ops.findIndex((op, index) => index > start && op.op === "stroke");
    return ops.slice(start + 1, end < 0 ? undefined : end);
  }

  it("draws the fade ramps and the gain line over the waveform", async () => {
    const ops: Op[] = [];
    const recorder = {
      strokeStyle: "",
      fillStyle: "",
      globalAlpha: 1,
      lineWidth: 1,
      clearRect: () => {},
      fillRect: () => {},
      setTransform: () => {},
      beginPath: () => ops.push({ op: "beginPath" }),
      stroke: () => ops.push({ op: "stroke" }),
      moveTo: (x: number, y: number) => ops.push({ op: "moveTo", x, y }),
      lineTo: (x: number, y: number) => ops.push({ op: "lineTo", x, y }),
    };
    (
      HTMLCanvasElement.prototype.getContext as unknown as {
        mockReturnValue: (value: unknown) => void;
      }
    ).mockReturnValue(recorder);

    await mountClip();
    await waitFor(() => expect(envelopePath(ops)).toHaveLength(4));
    const path = envelopePath(ops);
    // The clip is 192 px wide and its waveform 39 px tall: a 4.8 px fade-in
    // ramp, a flat line at the -3 dB level, then a 9.6 px fade-out ramp.
    expect(path[0]).toEqual({ op: "moveTo", x: 0, y: 39 });
    expect(path[1].x).toBeCloseTo(4.8, 1);
    expect(path[2].x).toBeCloseTo(182.4, 1);
    expect(path[3]).toEqual({ op: "lineTo", x: 192, y: 39 });
    expect(path[1].y).toBeCloseTo(path[2].y ?? 0, 6);
  });
});

describe("audio clip gain handle", () => {
  it("writes the gain the readout showed, once, on release", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-gain") as HTMLElement;
    expect(handle).toBeTruthy();
    const lineY = Number.parseFloat(handle.style.top);

    fireEvent.pointerDown(handle, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY,
    });
    fireEvent.pointerMove(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 4,
    });
    expect(block.querySelector(".clip-envelope-readout")).toBeTruthy();
    fireEvent.pointerMove(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 8,
    });
    const shown = readoutGain(block);
    expect(shown).toBeGreaterThan(-3);
    expect(clipCalls("set_audio_clip_envelope").length).toBe(0);

    fireEvent.pointerUp(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 8,
    });

    expect(envelopeCalls().length).toBe(1);
    // One gesture around one edit: the backend merges it into one history entry.
    expect(clipCalls("begin_undo_gesture").length).toBe(1);
    await waitFor(() => expect(clipCalls("end_undo_gesture").length).toBe(1));
    const args = getLastCallArgs("set_audio_clip_envelope")!;
    expect(args.clipIndex).toBe(0);
    expect(args.expectedRevision).toBe(store.projectRevision);
    expect(args.gainDb as number).toBeCloseTo(shown, 1);
    expect(args.fadeInSeconds).toBeUndefined();
    expect(args.fadeOutSeconds).toBeUndefined();
    expect(block.querySelector(".clip-envelope-readout")).toBeNull();
  });

  it("stops at the stated ceiling and says so in the readout", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-gain") as HTMLElement;
    const lineY = Number.parseFloat(handle.style.top);

    fireEvent.pointerDown(handle, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY,
    });
    fireEvent.pointerMove(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 400,
    });
    const readout = block.querySelector(".clip-envelope-readout")!;
    expect(readout.textContent).toContain(`${CLIP_GAIN_MAX_DB.toFixed(1)} dB`);
    expect(readout.textContent).toContain("limit");
    fireEvent.pointerUp(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 400,
    });

    expect(envelopeCalls().length).toBe(1);
    expect(getLastCallArgs("set_audio_clip_envelope")!.gainDb).toBeCloseTo(CLIP_GAIN_MAX_DB, 9);
  });

  it("leaves no entry when the gain is dragged back to where it started", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-gain") as HTMLElement;
    const lineY = Number.parseFloat(handle.style.top);

    fireEvent.pointerDown(handle, {
      button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY,
    });
    fireEvent.pointerMove(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY - 8,
    });
    fireEvent.pointerMove(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY,
    });
    fireEvent.pointerUp(document, {
      pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY,
    });

    expect(envelopeCalls().length).toBe(0);
  });
});

describe("audio clip fade handles", () => {
  it("writes the fade-in duration from the horizontal drag", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-fade.in") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 30, clientY: LANE_TOP + 20 });
    expect(block.querySelector(".clip-envelope-readout")!.textContent).toContain("Fade in");
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 20 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 20 });

    // 48 px at 24 px per quarter is two quarters, one second at 120 BPM.
    expect(envelopeCalls().length).toBe(1);
    expect(getLastCallArgs("set_audio_clip_envelope")!.fadeInSeconds).toBeCloseTo(1, 3);
  });

  it("writes the fade-out duration from a leftward drag", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-fade.out") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 192, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 144, clientY: LANE_TOP + 20 });
    expect(block.querySelector(".clip-envelope-readout")!.textContent).toContain("Fade out");
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 144, clientY: LANE_TOP + 20 });

    expect(envelopeCalls().length).toBe(1);
    expect(getLastCallArgs("set_audio_clip_envelope")!.fadeOutSeconds).toBeCloseTo(1, 3);
  });

  it("refuses a fade below 20 ms and snaps back to the stored value", async () => {
    const { block } = await mountClip();
    const handle = block.querySelector(".audio-clip-fade.in") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 20 });
    // 0.5 px is 10 ms at this zoom; the 5 px of vertical travel only arms the drag.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 0.5, clientY: LANE_TOP + 25 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 0.5, clientY: LANE_TOP + 25 });

    expect(envelopeCalls().length).toBe(0);
    expect(clipCalls("begin_undo_gesture").length).toBe(0);
    expect(store.error()).toBe("A fade needs at least 20 ms.");
    expect(block.querySelector(".clip-envelope-readout")).toBeNull();
    // The block still shows the project's 100 ms fade: the refusal wrote nothing.
    expect(block.textContent).toContain("in 100ms");
  });
});

describe("audio clip envelope without media", () => {
  it("keeps both handle families inert and reports why", async () => {
    const { block } = await mountClip({ missingMedia: true });
    const gain = block.querySelector(".audio-clip-gain") as HTMLElement;
    const fadeIn = block.querySelector(".audio-clip-fade.in") as HTMLElement;
    await waitFor(() => expect(gain.getAttribute("aria-disabled")).toBe("true"));
    expect(fadeIn.getAttribute("aria-disabled")).toBe("true");
    expect(gain.getAttribute("title")).toBe(
      "This clip has no audio media, so its gain and fades cannot be edited.",
    );

    const lineY = Number.parseFloat(gain.style.top);
    fireEvent.pointerDown(gain, { button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + lineY });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 10 });
    expect(envelopeCalls().length).toBe(0);
    expect(store.error()).toBe(
      "This clip has no audio media, so its gain and fades cannot be edited.",
    );
  });
});

describe("audio clip gain keyboard", () => {
  async function selectClip(block: HTMLElement) {
    fireEvent.pointerDown(block, { button: 0, pointerId: 1, clientX: 10, clientY: LANE_TOP + 30 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 10, clientY: LANE_TOP + 30 });
    expect(block.classList.contains("selected")).toBe(true);
  }

  it("nudges the selected clip by 0.5 dB with Alt+Up and resets with Alt+0", async () => {
    const { block } = await mountClip();
    await selectClip(block);

    fireEvent.keyDown(document, { key: "ArrowUp", altKey: true });
    await waitFor(() => expect(envelopeCalls().length).toBe(1));
    expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({
      clipIndex: 0,
      gainDb: -2.5,
    });

    fireEvent.keyDown(document, { key: "ArrowDown", altKey: true });
    await waitFor(() => expect(envelopeCalls().length).toBe(2));
    expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({ gainDb: -3.5 });

    fireEvent.keyDown(document, { key: "0", altKey: true });
    await waitFor(() => expect(envelopeCalls().length).toBe(3));
    expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({ gainDb: 0 });
  });

  it("ignores the nudge when no clip is selected", async () => {
    await mountClip();
    fireEvent.keyDown(document, { key: "ArrowUp", altKey: true });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(envelopeCalls().length).toBe(0);
  });
});
