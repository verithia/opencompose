/**
 * The floating clip gain window: one instance, Basic sliders and the optional
 * point envelope. Backend writes go through set_audio_clip_envelope.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  getLastCallArgs,
  invokeMock,
  makeAudioClip,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { GainEnvelopeWindow } from "../../components/GainEnvelopeWindow";
import { gainEnvelopeWindow } from "../../stores/gainEnvelopeWindow";
import { store } from "../../stores/project";

function snapshot(points?: { position: number; gain_db: number }[]) {
  const clip = makeAudioClip({
    track: "tape",
    gain_db: -3,
    fade_in_seconds: 0.08,
    fade_out_seconds: 0.25,
    crossfade_seconds: 0,
    ...(points ? { gain_envelope: points } : {}),
  });
  return makeSnapshot({
    tracks: [makeTrack("tape", 0)],
    patterns: [],
    properties: {
      ...makeSnapshot().project.properties,
      audio_clips: [clip],
    },
  });
}

async function mount(points?: { position: number; gain_db: number }[]) {
  let current = snapshot(points);
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "get_project") return JSON.parse(JSON.stringify(current));
    if (command === "get_transport_state") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
    if (command === "preview_audio_clip_source") return { audio: { mime: "audio/wav", base64: "UklGRg==" } };
    if (command === "audio_clip_transients") return {
      source_duration_seconds: 4,
      analyzed_end_seconds: 0.325,
      resolution_seconds: 0.005,
      transients: [
        { source_seconds: 0.29, strength: 0.9 },
        { source_seconds: 0.24, strength: 0.2 },
      ],
    };
    if (command === "set_audio_clip_envelope") {
      const typed = args as {
        clipIndex: number;
        gainDb?: number;
        fadeInSeconds?: number;
        fadeOutSeconds?: number;
        crossfadeSeconds?: number;
        gainEnvelope?: unknown;
      };
      const clip = current.project.properties.audio_clips![typed.clipIndex];
      if (typed.gainDb !== undefined) clip.gain_db = typed.gainDb;
      if (typed.fadeInSeconds !== undefined) clip.fade_in_seconds = typed.fadeInSeconds;
      if (typed.fadeOutSeconds !== undefined) clip.fade_out_seconds = typed.fadeOutSeconds;
      if (typed.crossfadeSeconds !== undefined) clip.crossfade_seconds = typed.crossfadeSeconds;
      if (typed.gainEnvelope !== undefined) {
        clip.gain_envelope = typed.gainEnvelope as typeof clip.gain_envelope;
      }
      current = { ...current, project: { ...current.project } };
      return JSON.parse(JSON.stringify(current));
    }
    return JSON.parse(JSON.stringify(current));
  });
  await store.initProject();
  gainEnvelopeWindow.open(0, "basic");
  const view = render(() => <GainEnvelopeWindow />);
  await waitFor(() => expect(document.querySelector(".gain-envelope-window")).toBeTruthy());
  return view;
}

beforeEach(() => {
  invokeMock.mockClear();
  gainEnvelopeWindow.close();
});

afterEach(() => {
  cleanup();
  gainEnvelopeWindow.close();
  vi.useRealTimers();
});

describe("gain envelope window", () => {
  it("sets an exact start and nudges placement through the shared operation", async () => {
    await mount();
    const start = screen.getByLabelText("Start time (s)");
    fireEvent.input(start, { target: { value: "1.234567" } });
    fireEvent.click(screen.getByRole("button", { name: "Set start" }));
    await waitFor(() => expect(getLastCallArgs("audio_clip_set_placement")).toMatchObject({
      clipIndex: 0, startSeconds: 1.234567,
    }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Earlier" }) as HTMLButtonElement).disabled).toBe(false));
    const nudge = screen.getByLabelText("Nudge (ms)");
    fireEvent.input(nudge, { target: { value: "2.5" } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Earlier" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Earlier" }));
    await waitFor(() => expect(getLastCallArgs("audio_clip_set_placement")).toMatchObject({
      clipIndex: 0, nudgeMs: -2.5,
    }));
    expect(getLastCallArgs("set_audio_clip_offset")).toBeUndefined();
  });

  it("prepares a stereo source excerpt without applying clip edits", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    const details = document.querySelector<HTMLDetailsElement>(".clip-source-preview")!;
    details.open = true;
    fireEvent(details, new Event("toggle"));
    const start = screen.getByLabelText("Source start (s)");
    fireEvent.input(start, { target: { value: "1.25" } });
    expect(fireEvent.keyDown(start, { key: "Enter" })).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Load audio" }));
    await waitFor(() => expect(details.querySelector("audio")?.getAttribute("src")).toBe("data:audio/wav;base64,UklGRg=="));
    expect(getLastCallArgs("preview_audio_clip_source")).toMatchObject({
      clipIndex: 0, sourceStartSeconds: 1.25, durationSeconds: 5,
    });
    expect(getLastCallArgs("set_audio_clip_playback")).toBeUndefined();
    const audio = details.querySelector("audio")!;
    const pause = vi.spyOn(audio, "pause").mockImplementation(() => {});
    Object.defineProperty(audio, "paused", { value: false });
    details.open = false;
    fireEvent(details, new Event("toggle"));
    expect(details.querySelector("audio")).toBeNull();
    expect(pause).toHaveBeenCalledOnce();
  });

  it("bypasses processing while submitting the existing warp settings", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    fireEvent.click(screen.getByRole("button", { name: "Add marker" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Bypass processing" }));
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      playback: {
        bypass_processing: true,
        stretch_mode: "musical",
        warp_markers: [{ source_seconds: 0.25, target_seconds: 0.25 }],
      },
    }));
  });

  it("snaps only the source time to the nearest measured attack", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    fireEvent.click(screen.getByRole("button", { name: "Add marker" }));
    fireEvent.click(screen.getByRole("button", { name: "Snap source to nearby transient" }));
    await waitFor(() => expect((screen.getByLabelText("Source (s)") as HTMLInputElement).value).toBe("0.24"));
    expect(getLastCallArgs("audio_clip_transients")).toMatchObject({
      clipIndex: 0, sourceStartSeconds: 0.175, durationSeconds: expect.closeTo(0.15),
    });
    expect(getLastCallArgs("set_audio_clip_playback")).toBeUndefined();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      playback: { warp_markers: [{ source_seconds: 0.24, target_seconds: 0.25 }] },
    }));
  });

  it("applies the speed and pitch together to the opened audio clip", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    fireEvent.input(screen.getByLabelText("Speed (×)"), { target: { value: "2" } });
    fireEvent.input(screen.getByLabelText("Pitch (semitones)"), { target: { value: "-3.5" } });
    fireEvent.input(screen.getByLabelText("Formant shift (semitones)"), { target: { value: "2" } });
    fireEvent.click(screen.getByLabelText("Preserve formants when pitching"));
    fireEvent.click(screen.getByRole("button", { name: "Add marker" }));
    fireEvent.change(screen.getByLabelText("Target (s)"), { target: { value: "0.5" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      clipIndex: 0,
      playback: {
        stretch: 0.5, pitch: -3.5, stretch_mode: "musical", tempo_follow: false,
        formant: 2, preserve_formants: true,
        warp_markers: [{ source_seconds: 0.25, target_seconds: 0.5 }],
      },
    }));
  });

  it("submits linked tape speed and clears independent processing", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    fireEvent.input(screen.getByLabelText("Pitch (semitones)"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Add marker" }));
    fireEvent.change(screen.getByLabelText("Processing"), { target: { value: "varispeed" } });
    expect(screen.queryByLabelText("Pitch (semitones)")).toBeNull();
    expect(screen.queryByRole("button", { name: "Add marker" })).toBeNull();
    fireEvent.input(screen.getByLabelText("Speed (×)"), { target: { value: "0.5" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(getLastCallArgs("set_audio_clip_playback")).toMatchObject({
      playback: {
        stretch_mode: "varispeed", stretch: 2, pitch: 0, formant: 0,
        preserve_formants: false, warp_markers: [], tempo_follow: false,
      },
    }));
  });

  it("keeps one window even when opened again", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "env");
    expect(document.querySelectorAll(".gain-envelope-window")).toHaveLength(1);
  });

  it("keeps the editor for an edit to the same clip and closes it when another clip takes its index", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    const updated = snapshot();
    updated.revision = store.projectRevision + 1;
    const clips = updated.project.properties.audio_clips!;
    clips[0].pitch = 4;
    clips.push(makeAudioClip({ name: "Another clip", track: "tape" }));
    expect(store.applyAiSnapshot(updated)).toBe(true);
    expect((screen.getByLabelText("Pitch (semitones)") as HTMLInputElement).value).toBe("4");

    const shifted = structuredClone(updated);
    shifted.revision += 1;
    shifted.project.properties.audio_clips!.shift();
    expect(store.applyAiSnapshot(shifted)).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("closes the editor when opening a project with a clip of the same name", async () => {
    await mount();
    gainEnvelopeWindow.open(0, "playback");
    fireEvent.input(screen.getByLabelText("Pitch (semitones)"), { target: { value: "5" } });
    const replacement = snapshot();
    replacement.file_path = "/another/project.ocp";
    invokeMock.mockImplementation(async (command: string) =>
      command === "get_transport_state" ? TRANSPORT_STOPPED() : replacement);

    await store.openFile(replacement.file_path);

    expect(store.filePath()).toBe(replacement.file_path);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(getLastCallArgs("set_audio_clip_playback")).toBeUndefined();
  });

  it("closes from its close button", async () => {
    await mount();
    fireEvent.click(document.querySelector(".gain-envelope-close") as HTMLElement);
    expect(document.querySelector(".gain-envelope-window")).toBeNull();
  });

  it("moves the window by dragging its title", async () => {
    await mount();
    const panel = document.querySelector(".gain-envelope-window") as HTMLElement;
    const title = document.querySelector(".gain-envelope-title") as HTMLElement;
    const before = panel.style.left;
    fireEvent.pointerDown(title, { button: 0, pointerId: 1, clientX: 500, clientY: 110 });
    fireEvent.pointerMove(window, { pointerId: 1, clientX: 560, clientY: 150 });
    fireEvent.pointerUp(window, { pointerId: 1, clientX: 560, clientY: 150 });
    expect(panel.style.left).not.toBe(before);
  });

  it("drags an interior envelope point", async () => {
    const initialPoints = [
      { position: 0, gain_db: 0 },
      { position: 0.5, gain_db: -6 },
      { position: 1, gain_db: 0 },
    ];
    await mount(initialPoints);
    const revision = store.projectRevision;
    fireEvent.click(document.querySelector('[data-tab="env"]') as HTMLElement);
    const plot = document.querySelector(".gain-env-plot") as HTMLElement;
    plot.getBoundingClientRect = () =>
      ({ left: 0, top: 0, width: 200, height: 200, right: 200, bottom: 200, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
    const points = document.querySelectorAll("[data-envelope-point]");
    expect(points).toHaveLength(3);
    const target = points[1] as HTMLElement;
    fireEvent.pointerDown(target, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(window, { pointerId: 1, clientX: 100, clientY: 160 });
    const newer = snapshot(initialPoints);
    newer.revision = revision + 1;
    expect(store.applyAiSnapshot(newer)).toBe(true);
    fireEvent.pointerUp(window, { pointerId: 1, clientX: 100, clientY: 160 });
    await waitFor(() => {
      expect(getLastCallArgs("set_audio_clip_envelope")?.expectedRevision).toBe(revision);
      const envelope = getLastCallArgs("set_audio_clip_envelope")?.gainEnvelope as
        | { position: number; gain_db: number }[]
        | undefined;
      expect(envelope?.[1].gain_db).toBeLessThan(-20);
    });
  });

  it("removes an interior point on double-click", async () => {
    await mount([
      { position: 0, gain_db: 0 },
      { position: 0.5, gain_db: -6 },
      { position: 1, gain_db: 0 },
    ]);
    fireEvent.click(document.querySelector('[data-tab="env"]') as HTMLElement);
    const points = document.querySelectorAll("[data-envelope-point]");
    expect(points).toHaveLength(3);
    fireEvent.dblClick(points[1]);
    await waitFor(() => expect(document.querySelectorAll("[data-envelope-point]")).toHaveLength(2));
    await waitFor(() => {
      const envelope = getLastCallArgs("set_audio_clip_envelope")?.gainEnvelope as
        | { position: number; gain_db: number }[]
        | undefined;
      expect(envelope).toHaveLength(2);
    });
  });

  it("commits a fade from the Basic tab", async () => {
    await mount();
    const sliders = document.querySelectorAll<HTMLInputElement>(".gain-env-param input");
    expect(sliders).toHaveLength(3);
    fireEvent.input(sliders[0], { target: { value: "0.5" } });
    fireEvent.change(sliders[0], { target: { value: "0.5" } });
    await waitFor(() =>
      expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({ fadeInSeconds: 0.5 }),
    );
  });

  it("adds an envelope from an empty Env tab", async () => {
    await mount();
    fireEvent.click(document.querySelector('[data-tab="env"]') as HTMLElement);
    await waitFor(() => expect(document.querySelector(".gain-env-add")).toBeTruthy());
    fireEvent.click(document.querySelector(".gain-env-add") as HTMLElement);
    await waitFor(() => expect(document.querySelectorAll("[data-envelope-point]")).toHaveLength(2));
    expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({
      gainEnvelope: [
        { position: 0, gain_db: 0 },
        { position: 1, gain_db: 0 },
      ],
    });
  });

  it("drives the crossfade slider", async () => {
    await mount();
    const sliders = document.querySelectorAll<HTMLInputElement>(".gain-env-param input");
    fireEvent.input(sliders[2], { target: { value: "0.3" } });
    fireEvent.change(sliders[2], { target: { value: "0.3" } });
    await waitFor(() =>
      expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({ crossfadeSeconds: 0.3 }),
    );
  });
});
