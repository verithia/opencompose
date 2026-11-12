/**
 * Take lanes: one row per take with M/S, a muted row that reads muted, comp
 * segments for the playing take, and a drag that asks for the snapped range.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { makeAudioClip, makeSnapshot, makeTrack, invokeMock, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { resetGridDivision, setGridDivision } from "../../stores/grid";
import { i18n } from "../../stores/i18n";
import { TakeHeaders, TakeLanes } from "../../components/TakeLanes";
import { Timeline } from "../../components/Timeline";
import { createProjectClock } from "../../utils/projectClock";

function takeClips() {
  return [
    makeAudioClip({
      name: "vox take 1", take_group: "vox take 9", take: 1, muted: true,
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
    }),
    makeAudioClip({
      name: "vox take 2", take_group: "vox take 9", take: 2, muted: false,
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
    }),
  ];
}

async function mount() {
  const clips = takeClips();
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "get_project") return makeSnapshot({
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
        audio_clips: clips,
      },
    });
    return makeSnapshot();
  });
  await store.initProject();
  const onCompMock = vi.fn();
  const onMuteMock = vi.fn();
  const onComp = (track: string, take: number, start: number, end: number) =>
    onCompMock(track, take, start, end);
  const onMute = (track: string, take: number, muted: boolean) =>
    onMuteMock(track, take, muted);
  const onSolo = vi.fn();
  const view = render(() => (
    <>
      <TakeHeaders
        trackName="tape"
        clips={clips}
        solo={null}
        open
        onToggle={() => {}}
        onMute={onMute}
        onSolo={onSolo}
      />
      <TakeLanes
        trackName="tape"
        clips={clips}
        pxPerBeat={50}
        width={800}
        open
        onComp={onComp}
      />
    </>
  ));
  return { ...view, onCompMock, onMuteMock, onSolo };
}

function stubRow(container: HTMLElement, take: number) {
  const row = container.querySelector(`.take-row[data-take="${take}"]`) as HTMLElement;
  row.getBoundingClientRect = () => ({
    left: 0, top: 0, right: 800, bottom: 20, width: 800, height: 20, x: 0, y: 0,
    toJSON: () => ({}),
  }) as DOMRect;
  return row;
}

beforeEach(() => {
  invokeMock.mockClear();
  resetGridDivision();
  i18n.setLocale("en");
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

afterEach(() => {
  vi.restoreAllMocks();
  i18n.setLocale("en");
});

describe("take lanes", () => {
  it("renders one row per take with M and S on the header", async () => {
    const { container, onMuteMock } = await mount();
    expect(container.querySelectorAll(".take-header-row").length).toBe(2);
    expect(container.querySelectorAll(".take-row").length).toBe(2);
    expect(container.querySelectorAll(".take-header-row .track-btn").length).toBe(4);
    const mute = container.querySelector(
      '.take-header-row[data-take="1"] [aria-label="Unmute take 1"]',
    ) as HTMLElement;
    fireEvent.click(mute);
    expect(onMuteMock).toHaveBeenCalledWith("tape", 1, false);
  });

  it("shows a muted take faded and the comp row with the chosen segment", async () => {
    const { container } = await mount();
    expect(container.querySelector('.take-row[data-take="1"]')?.classList.contains("muted")).toBe(true);
    expect(container.querySelector('.take-row[data-take="1"] .take-row-block')?.classList.contains("muted")).toBe(true);
    expect(container.querySelector('.take-row[data-take="2"]')?.classList.contains("muted")).toBe(false);
    const segments = [...container.querySelectorAll(".take-comp-segment")];
    expect(segments.length).toBe(1);
    expect(segments[0].getAttribute("data-take")).toBe("2");
    expect(segments[0].textContent).toContain("Take 2");
  });

  it("comping a drag calls the command with the snapped quarter range", async () => {
    const { container, onCompMock } = await mount();
    const row = stubRow(container, 2);
    fireEvent.pointerDown(row, { button: 0, clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(document, { clientX: 300, pointerId: 1 });
    fireEvent.pointerUp(document, { clientX: 300, pointerId: 1 });
    expect(onCompMock).toHaveBeenCalledTimes(1);
    expect(onCompMock).toHaveBeenCalledWith("tape", 2, 2, 6);
  });

  it("drops the drag when the window loses focus, comping nothing", async () => {
    const { container, onCompMock } = await mount();
    const row = stubRow(container, 2);
    fireEvent.pointerDown(row, { button: 0, clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(document, { clientX: 300, pointerId: 1 });
    expect(container.querySelector(".take-comp-marquee")).toBeTruthy();

    // Cmd+Tab mid-drag: no release arrives, so the comp must not be written.
    fireEvent.blur(window);
    expect(container.querySelector(".take-comp-marquee")).toBeFalsy();
    fireEvent.pointerUp(document, { clientX: 300, pointerId: 1 });
    expect(onCompMock).not.toHaveBeenCalled();
  });

  it("snaps a sub-grid drag to the shared grid", async () => {
    setGridDivision(1);
    const { container, onCompMock } = await mount();
    const row = stubRow(container, 2);
    fireEvent.pointerDown(row, { button: 0, clientX: 160, pointerId: 1 });
    fireEvent.pointerMove(document, { clientX: 310, pointerId: 1 });
    fireEvent.pointerUp(document, { clientX: 310, pointerId: 1 });
    expect(onCompMock).toHaveBeenCalledWith("tape", 2, 3, 6);
  });

  it("wires a timeline drag to the comp command with the snapped bar/beat range", async () => {
    const clips = takeClips();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_project") return makeSnapshot({
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
          audio_clips: clips,
        },
      });
      if (command === "get_transport_state" || command === "transport_seek") {
        return TRANSPORT_STOPPED();
      }
      if (command === "audio_clip_peaks") return "";
      return makeSnapshot();
    });
    await store.initProject();
    await store.seek(0);
    const view = render(() => <Timeline />);
    const disclosure = view.container.querySelector(".take-disclosure") as HTMLElement;
    expect(disclosure).not.toBeNull();
    fireEvent.click(disclosure);
    const row = stubRow(view.container as HTMLElement, 2);
    // The timeline opens at 24 px per beat.
    fireEvent.pointerDown(row, { button: 0, clientX: 48, pointerId: 1 });
    fireEvent.pointerMove(document, { clientX: 144, pointerId: 1 });
    fireEvent.pointerUp(document, { clientX: 144, pointerId: 1 });
    const call = invokeMock.mock.calls.find(([name]) => name === "comp_audio_takes");
    expect(call, "the drag reaches the comp command").toBeTruthy();
    const clock = createProjectClock(store.project.properties);
    const start = clock.barBeatAt(2);
    const end = clock.barBeatAt(6);
    expect(call![1]).toMatchObject({
      track: "tape",
      take: 2,
      startBar: start.bar,
      startBeat: start.beat,
      startSub: start.subdivision,
      endBar: end.bar,
      endBeat: end.beat,
      endSub: end.subdivision,
    });
  });
});
