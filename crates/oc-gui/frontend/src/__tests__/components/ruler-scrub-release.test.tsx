/**
 * A ruler scrub mutes the transport while it runs, so every way the gesture can
 * end without a release — a lost capture, a blur, Escape, unmount — has to drop
 * the scrub position and bring the audio back exactly once. A release that lost
 * the gesture must not seek either.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  invokeMock,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { adoptRecord } from "../../stores/record";
import { Timeline } from "../../components/Timeline";
import { PianoRoll } from "../../components/PianoRoll";

const count = (command: string) =>
  invokeMock.mock.calls.filter(([cmd]) => cmd === command).length;

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot({
      tracks: [makeTrack("Piano", 0)],
      patterns: [makePattern("beat")],
    });
  });
  await store.initProject();
  store.setScrubPosition(null);
});

afterEach(() => adoptRecord({ recording: false }));

it.each([[".timeline-ruler", Timeline], [".pianoroll-ruler", PianoRoll]] as const)(
  "does not mute or seek from %s during a take",
  (selector, Editor) => {
    store.setSelectedPatternName("beat");
    adoptRecord({ recording: true, armed: "Piano" });
    const { container } = render(() => <Editor />);
    const ruler = container.querySelector(selector) as HTMLElement;
    fireEvent.pointerDown(ruler, { button: 0, pointerId: 20, clientX: 48 });
    fireEvent.pointerUp(document, { button: 0, pointerId: 20, clientX: 72 });
    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_mute")).toBe(0);
    expect(count("transport_seek")).toBe(0);
  },
);

describe("timeline ruler scrub", () => {
  it("drops the scrub and unmutes once when the pointer capture is lost", () => {
    const { container } = render(() => <Timeline />);
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, pointerId: 5, clientX: 48 });
    expect(store.scrubPosition()).toBe(2);
    expect(count("transport_mute")).toBe(1);

    fireEvent.lostPointerCapture(ruler, { pointerId: 5 });

    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    expect(count("transport_seek")).toBe(0);

    // The gesture is over: a late release cannot commit a seek or unmute twice.
    fireEvent.pointerUp(document, { pointerId: 5, clientX: 96, button: 0 });
    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    expect(count("transport_seek")).toBe(0);
  });

  it("drops the scrub and unmutes once when the window loses focus", () => {
    const { container } = render(() => <Timeline />);
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, pointerId: 6, clientX: 72 });
    expect(store.scrubPosition()).toBe(3);

    fireEvent(window, new Event("blur"));

    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    expect(count("transport_seek")).toBe(0);
  });

  it("drops the scrub and unmutes once on Escape", () => {
    const { container } = render(() => <Timeline />);
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, pointerId: 7, clientX: 48 });
    fireEvent.keyDown(document, { key: "Escape" });

    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    expect(count("transport_seek")).toBe(0);
  });
});

describe("piano roll ruler scrub", () => {
  it("drops the scrub and unmutes once when the window loses focus", () => {
    store.setSelectedPatternName("beat");
    const { container } = render(() => <PianoRoll />);
    const ruler = container.querySelector(".pianoroll-ruler") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, pointerId: 8, clientX: 40 });
    expect(store.scrubPosition()).not.toBeNull();
    expect(count("transport_mute")).toBe(1);

    fireEvent(window, new Event("blur"));

    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    expect(count("transport_seek")).toBe(0);
  });
});
