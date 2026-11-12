/**
 * Clip hover tooltip: block kind, track, bar range, length, content and state.
 * Rows are read from the shared tooltip layer, not from private helpers.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  invokeMock,
  makeAudioClip,
  makeNote,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { peaksFor, resetPeaks } from "../../stores/audioPeaks";
import { Timeline } from "../../components/Timeline";
import { TooltipLayer } from "../../components/Tooltip";

const PROPERTIES = {
  tempo: 120,
  time_signature: { numerator: 4, denominator: 4 },
  key: null,
  swing: null,
  tempo_map: [],
  time_sig_map: [],
  key_map: [],
};

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

/** Minimal OCPK payload: one level, four buckets, the given rate. */
function peakPayload(sampleRate: number): string {
  const buckets = 4;
  const bodyBytes = 8 + buckets * 3 * 4;
  const bytes = new Uint8Array(30 + bodyBytes);
  const view = new DataView(bytes.buffer);
  bytes.set([0x4f, 0x43, 0x50, 0x4b], 0);
  view.setUint8(4, 1);
  view.setUint32(14, sampleRate, true);
  view.setBigUint64(18, BigInt(sampleRate), true);
  view.setUint32(26, bodyBytes, true);
  view.setUint32(30, 256, true);
  view.setUint32(34, buckets, true);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function snapshotWith(options: {
  tracks: any[];
  patterns?: any[];
  audioClips?: any[];
  properties?: any;
}) {
  return makeSnapshot({
    properties: { ...PROPERTIES, ...(options.properties ?? {}), audio_clips: options.audioClips ?? [] } as any,
    patterns: options.patterns ?? [],
    tracks: options.tracks,
  });
}

function mockBackend(snapshot: any, sampleRate = 48000) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return sampleRate > 0 ? peakPayload(sampleRate) : "";
    return snapshot;
  });
}

function tooltipRow(label: string): string | undefined {
  const row = [...document.querySelectorAll(".oc-tooltip-row")]
    .find((candidate) => candidate.querySelector(".oc-tooltip-label")?.textContent === label);
  return row?.querySelector(".oc-tooltip-value")?.textContent ?? undefined;
}

function tooltipLabels(): string[] {
  return [...document.querySelectorAll(".oc-tooltip-label")].map((label) => label.textContent ?? "");
}

function tooltipTitle(): string | undefined {
  return document.querySelector(".oc-tooltip-title")?.textContent ?? undefined;
}

function tooltipSubtitle(): string | undefined {
  return document.querySelector(".oc-tooltip-subtitle")?.textContent ?? undefined;
}

async function hover(container: HTMLElement, selector: string) {
  const block = container.querySelector(selector) as HTMLElement;
  expect(block).toBeTruthy();
  vi.useFakeTimers();
  fireEvent.mouseEnter(block);
  vi.advanceTimersByTime(400);
  return block;
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetPeaks();
  i18n.setLocale("en");
  await store.initProject();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("clip tooltip detail", () => {
  it("shows the named clip's kind, track, bar range, length and content", async () => {
    const snapshot = snapshotWith({
      patterns: [makePattern("lead")],
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [],
        clips: [{ pattern: "lead", time_range: range([1, 1, 0], [3, 1, 0]) }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".clip-block");

    expect(tooltipTitle()).toBe("lead");
    expect(tooltipSubtitle()).toBe("Pattern clip · Piano");
    expect(tooltipRow("Bars")).toBe("1.1 → 3.1");
    expect(tooltipRow("Length")).toBe("2 bars · 8 beats · 0:04.000");
    expect(tooltipRow("Sound")).toBe("Piano 1");
    expect(tooltipRow("Hits")).toBe("2");
    expect(tooltipLabels()).not.toContain("State");
    expect(document.querySelector(".oc-tooltip")?.textContent).toContain("Double-click to edit");
  });

  it("uses the track sound even when the pattern declares another preset", async () => {
    const snapshot = snapshotWith({
      patterns: [{ ...makePattern("lead"), preset: "other_preset" }],
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [],
        clips: [{ pattern: "lead", time_range: range([1, 1, 0], [3, 1, 0]) }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".clip-block");

    expect(tooltipRow("Sound")).toBe("Piano 1");
  });

  it("shows a phrase's project sound and note count", async () => {
    const snapshot = snapshotWith({
      tracks: [{
        ...makeTrack("Piano", 0),
        clips: [],
        phrases: [{
          name: "melody",
          time_range: range([1, 1, 0], [5, 1, 0]),
          events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1 })],
        }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".phrase-note-block");

    expect(tooltipTitle()).toBe("melody");
    expect(tooltipSubtitle()).toBe("Phrase · Piano");
    expect(tooltipRow("Bars")).toBe("1.1 → 5.1");
    expect(tooltipRow("Length")).toBe("4 bars · 16 beats · 0:08.000");
    expect(tooltipRow("Sound")).toBe("Piano 1");
    expect(tooltipRow("Notes")).toBe("2");
  });

  it("counts bars and beats in the block's meter", async () => {
    const snapshot = snapshotWith({
      properties: { time_signature: { numerator: 3, denominator: 4 } },
      patterns: [makePattern("lead")],
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [],
        clips: [{ pattern: "lead", time_range: range([1, 1, 0], [3, 1, 0]) }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".clip-block");

    expect(tooltipRow("Length")).toBe("2 bars · 6 beats · 0:03.000");
  });

  it("counts an inline drum pattern's played length from repeat", async () => {
    const snapshot = snapshotWith({
      tracks: [{
        ...makeTrack("Drums", 0),
        phrases: [],
        clips: [],
        patterns: [{
          name: "main_beat",
          time_range: range([1, 1, 0], [2, 1, 0]),
          repeat: 4,
          hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] }],
        }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".pattern-block");

    expect(tooltipTitle()).toBe("main_beat");
    expect(tooltipSubtitle()).toBe("Pattern lane · Drums");
    expect(tooltipRow("Bars")).toBe("1.1 → 5.1");
    expect(tooltipRow("Length")).toBe("4 bars · 16 beats · 0:08.000");
    expect(tooltipRow("Loop")).toBe("4×");
    expect(tooltipRow("Hits")).toBe("1");
  });

  it("shows the clip's non-default state in one row", async () => {
    const pattern = {
      ...makePattern("lead"),
      preset: "Piano 1",
      events: [makeNote()],
      hits: [],
      transpose: 2,
      swing: 55,
      color: "#ff8844",
    };
    const base = makeTrack("Piano", 0);
    const snapshot = snapshotWith({
      patterns: [pattern],
      tracks: [{
        ...base,
        mix: { ...base.mix, mute: true },
        phrases: [],
        clips: [{ pattern: "lead", time_range: range([1, 1, 0], [3, 1, 0]) }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);
    const block = view.container.querySelector(".clip-block") as HTMLElement;
    fireEvent.click(block);

    await hover(view.container as HTMLElement, ".clip-block");

    expect(tooltipRow("State")).toBe("Transpose +2 · Swing 55% · Colour #ff8844 · Muted · Selected");
  });

  it("shows an audio clip's media, offset, stretch and mix detail", async () => {
    const snapshot = snapshotWith({
      tracks: [{
        ...makeTrack("tape", 0),
        mix: { ...makeTrack("tape", 0).mix, volume_db: 3, pan: -0.25 },
        phrases: [],
        clips: [],
      }],
      audioClips: [{
        ...makeAudioClip({ offset_seconds: 0.5 }),
        stretch: 1.5,
        stretch_mode: "musical",
        tempo_follow: false,
      } as any],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await vi.waitFor(() => expect(peaksFor("media/hit.wav")).toBeTruthy());
    await hover(view.container as HTMLElement, ".audio-clip-block");

    expect(tooltipTitle()).toBe("hit");
    expect(tooltipSubtitle()).toBe("Audio clip · tape");
    expect(tooltipRow("Bars")).toBe("1.1 → 3.1");
    expect(tooltipRow("Source")).toBe("hit.wav");
    expect(tooltipRow("Sample rate")).toBe("48 kHz");
    expect(tooltipRow("Channels")).toBe("2 ch");
    expect(tooltipRow("Offset")).toBe("0.500 s");
    expect(tooltipRow("Stretch")).toBe("1.500 × musical");
    expect(tooltipRow("State")).toBe("Gain +3.0 dB · Pan L25");
  });

  it("omits offset, stretch and state rows when everything is default", async () => {
    const snapshot = snapshotWith({
      tracks: [{
        ...makeTrack("tape", 0),
        phrases: [],
        clips: [],
      }],
      audioClips: [{
        ...makeAudioClip(),
        stretch: 1,
        stretch_mode: "musical",
        tempo_follow: false,
      } as any],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await vi.waitFor(() => expect(peaksFor("media/hit.wav")).toBeTruthy());
    await hover(view.container as HTMLElement, ".audio-clip-block");

    expect(tooltipRow("Source")).toBe("hit.wav");
    expect(tooltipLabels()).not.toContain("Stretch");
    expect(tooltipLabels()).not.toContain("Offset");
    expect(tooltipLabels()).not.toContain("State");
  });

  it("falls back to the unnamed marker and hides default mix rows", async () => {
    const snapshot = snapshotWith({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [],
        clips: [{ pattern: "", time_range: range([1, 1, 0], [3, 1, 0]) }],
      }],
    });
    mockBackend(snapshot);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /></>);

    await hover(view.container as HTMLElement, ".clip-block");

    expect(tooltipTitle()).toBe("(unnamed)");
    expect(tooltipLabels()).not.toContain("Gain");
    expect(tooltipLabels()).not.toContain("Pan");
  });
});
