/**
 * Audio clip loop points: the menu sets and clears a loop, the block draws one
 * dashed seam per repeat, the seam handle tunes the length in one gesture, and
 * a file shorter than 100 ms states why it cannot loop.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import {
  getLastCallArgs,
  invokeMock,
  makeAudioClip,
  makeSnapshot,
  makeTrack,
  TRANSPORT_STOPPED,
} from "../setup";
import { store } from "../../stores/project";
import { resetPeaks } from "../../stores/audioPeaks";
import { resetGridDivision } from "../../stores/grid";
import { closeContextMenu, ContextMenuLayer } from "../../components/ContextMenu";
import type { TempoPoint } from "../../types/project";
import { Timeline } from "../../components/Timeline";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;
const RATE = 48_000;

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

/** Six seconds of media under a four-second clip (two bars at 120 BPM). */
async function mountClip(mediaSeconds: number, loopSeconds = 0, tempoMap: TempoPoint[] = []) {
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
        tempo_map: tempoMap,
        time_sig_map: [],
        key_map: [],
        audio_clips: [makeAudioClip({ loop_seconds: loopSeconds })],
      },
    });
  });
  await store.initProject();
  await store.seek(0);
  const view = render(() => <><Timeline /><ContextMenuLayer /></>);
  stubGeometry(view.container as HTMLElement);
  const block = view.container.querySelector(".audio-clip-block") as HTMLElement;
  return { ...view, block };
}

function menuLabels(): string[] {
  return [...document.querySelectorAll<HTMLElement>(".context-menu-item .context-menu-label")]
    .map((element) => element.textContent?.trim() ?? "");
}

/** The loop rows live in the clip-content group, which starts closed. */
function openContentGroup(): void {
  const group = [...document.querySelectorAll<HTMLElement>(".context-menu-group")]
    .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === "Clip content");
  if (group && group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
}

function menuItem(label: string): HTMLElement {
  const item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((element) => element.querySelector(".context-menu-label")?.textContent?.trim() === label);
  if (!item) throw new Error(`no menu item ${label}; menu has ${menuLabels().join(", ")}`);
  return item;
}

function loopCalls() {
  return invokeMock.mock.calls.filter(([name]) => name === "set_audio_clip_loop");
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetPeaks();
  resetGridDivision();
  store.setError(null);
  closeContextMenu();
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
  await new Promise((resolve) => setTimeout(resolve, 0));
  vi.restoreAllMocks();
});

describe("audio clip loop menu", () => {
  it("loops the media left after the offset, at most half the clip", async () => {
    const { block } = await mountClip(6);
    fireEvent.contextMenu(block, { clientX: 20, clientY: 20 });
    openContentGroup();
    await waitFor(() => expect(menuLabels()).toContain("Repeat to fill"));
    fireEvent.click(menuItem("Repeat to fill"));

    await waitFor(() => expect(loopCalls().length).toBe(1));
    // Six seconds of media in a four-second clip: half the clip is two seconds.
    expect(getLastCallArgs("set_audio_clip_loop")!.loopSeconds).toBeCloseTo(2, 3);
  });

  it("offers Stop repeating once the clip repeats and clears it", async () => {
    const { block } = await mountClip(6, 1.5);
    await waitFor(() => expect(block.textContent).toContain("loop 1.5 s"));
    fireEvent.contextMenu(block, { clientX: 20, clientY: 20 });
    openContentGroup();
    await waitFor(() => expect(menuLabels()).toContain("Stop repeating"));
    expect(menuLabels()).not.toContain("Repeat to fill");
    fireEvent.click(menuItem("Stop repeating"));

    await waitFor(() => expect(loopCalls().length).toBe(1));
    expect(getLastCallArgs("set_audio_clip_loop")!.loopSeconds).toBeNull();
  });

  it("states why a file shorter than 100 ms cannot loop", async () => {
    const { block } = await mountClip(0.05);
    fireEvent.contextMenu(block, { clientX: 20, clientY: 20 });
    openContentGroup();
    await waitFor(() => expect(menuLabels()).toContain("Repeat to fill"));
    const item = menuItem("Repeat to fill");
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe(
      "Too short to loop: this file is shorter than 100 ms.",
    );
    fireEvent.click(item);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(loopCalls().length).toBe(0);
  });
});

describe("audio clip loop seams", () => {
  it("draws one dashed seam per repeat inside the block", async () => {
    // A four-second clip repeating every second: seams at 1 s, 2 s and 3 s.
    const { block } = await mountClip(6, 1);
    await waitFor(() =>
      expect(block.querySelectorAll(".audio-clip-loop-seam").length).toBe(3),
    );
    expect(block.querySelector(".audio-clip-loop-handle")).toBeTruthy();
  });

  it("positions repeats by elapsed time across a tempo change", async () => {
    const { block } = await mountClip(6, 1, [{ position: { bar: 2, beat: 1, subdivision: 0 }, bpm: 60 }]);
    await waitFor(() => {
      const seams = Array.from(block.querySelectorAll<HTMLElement>(".audio-clip-loop-seam"));
      expect(seams.map((seam) => seam.style.left)).toEqual(["48px", "96px", "120px", "144px", "168px"]);
    });
  });

  it("stops the seam one grid step below the clip length", async () => {
    // Six seconds of media in a four-second clip: the seam can never reach the
    // clip's own length, because a loop that long is refused.
    const { block } = await mountClip(6, 1);
    const handle = block.querySelector(".audio-clip-loop-handle") as HTMLElement;
    await waitFor(() => expect(handle.style.left).toBe("48px"));

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 48, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 2000, clientY: LANE_TOP + 20 });
    // 120 BPM at the default 1/16 grid: the ceiling is 4 s - 0.125 s.
    expect(block.querySelector(".clip-envelope-readout")!.textContent).toContain("3.88 s");

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 2000, clientY: LANE_TOP + 20 });
    await waitFor(() => expect(loopCalls().length).toBe(1));
    expect(getLastCallArgs("set_audio_clip_loop")!.loopSeconds).toBeCloseTo(3.875, 6);
  });

  it("tunes the loop length with one write on release", async () => {
    const { block } = await mountClip(6, 1);
    const revision = store.projectRevision;
    const handle = block.querySelector(".audio-clip-loop-handle") as HTMLElement;
    await waitFor(() => expect(handle.style.left).toBe("48px"));

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 48, clientY: LANE_TOP + 20 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 20 });
    expect(block.querySelector(".clip-envelope-readout")!.textContent).toContain("Loop 1.5 s");
    expect(loopCalls().length).toBe(0);
    expect(store.applyAiSnapshot({ ...makeSnapshot({ ...store.project }), revision: revision + 1 })).toBe(true);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 20 });
    await waitFor(() => expect(loopCalls().length).toBe(1));
    expect(getLastCallArgs("set_audio_clip_loop")!.loopSeconds).toBeCloseTo(1.5, 3);
    expect(getLastCallArgs("set_audio_clip_loop")!.expectedRevision).toBe(revision);
  });
});
