/**
 * Bounce / consolidate: the default range, the command the prompt sends, the
 * pending state, the refusal text and the report line in the status bar.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import App from "../App";
import { closeContextMenu } from "../components/ContextMenu";
import {
  bounceStore,
  parseBarBeat,
  rangeCutsNote,
  rangeFailureKey,
  trackContentBounds,
  trackSoundingEnd,
} from "../stores/bounce";
import { i18n } from "../stores/i18n";
import { store } from "../stores/project";
import type { Project, ProjectSnapshot } from "../types/project";
import { invokeMock, makeAudioClip, makeNote, makeSnapshot, makeTrack } from "./setup";
import { installAuditBackend } from "./ui-audit-support";

/** Two tracks: a phrase track and a track holding one audio clip. */
function bounceProject(): ProjectSnapshot {
  const lead = makeTrack("Lead", 0);
  lead.phrases = [{
    name: "held",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 5, beat: 1, subdivision: 0 },
    },
    events: [],
  }];
  const tape = makeTrack("Tape", 1);
  tape.phrases = [];
  return makeSnapshot({
    tracks: [lead, tape],
    properties: {
      tempo: 120,
      time_signature: { numerator: 4, denominator: 4 },
      key: null,
      swing: null,
      tempo_map: [],
      time_sig_map: [],
      key_map: [],
      audio_clips: [makeAudioClip({ name: "take", track: "Tape" })],
    },
  });
}

/** One whole note from bar 1 that outlasts a range ending inside it. */
function heldNoteProject(): ProjectSnapshot {
  const lead = makeTrack("Lead", 0);
  lead.phrases = [{
    name: "held",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 5, beat: 1, subdivision: 0 },
    },
    events: [
      makeNote({
        duration: { base: "Whole", dots: 0, triplet: false, tuplet: null, ties: [] },
      }),
    ],
  }];
  const tape = makeTrack("Tape", 1);
  tape.phrases = [];
  return makeSnapshot({
    tracks: [lead, tape],
    properties: {
      tempo: 120,
      time_signature: { numerator: 4, denominator: 4 },
      key: null,
      swing: null,
      tempo_map: [],
      time_sig_map: [],
      key_map: [],
      audio_clips: [makeAudioClip({ name: "take", track: "Tape" })],
    },
  });
}

function bounceReply(snapshot = bounceProject()) {
  return {
    project: snapshot,
    report:
      "Bounced 'Lead' to media/bounce-lead-1.wav (8.00s, 384000 frames at 48000 Hz) onto 'Lead (bounced)'; the source track is muted. Baked insert chain (limiter).",
    clip: "Lead bounce",
    track: "Lead",
    target_track: "Lead (bounced)",
    media: "media/bounce-lead-1.wav",
    path: "/tmp/media/bounce-lead-1.wav",
    mode: "new_track",
    frames: 384000,
    sample_rate: 48000,
    baked: ["limiter"],
  };
}

function consolidateReply(snapshot = bounceProject()) {
  return {
    project: snapshot,
    report:
      "Consolidated 1 audio clip on 'Tape' into media/consolidate-tape-1.wav (4.00s, 192000 frames at 48000 Hz); one clip replaces them over 1:1 to 3:1.",
    clip: "Tape consolidate",
    track: "Tape",
    media: "media/consolidate-tape-1.wav",
    path: "/tmp/media/consolidate-tape-1.wav",
    clips: 1,
    frames: 192000,
    sample_rate: 48000,
  };
}

/** Load the fixture and answer the two bounce commands. */
async function prepare(
  extra?: (command: string) => unknown,
  snapshot = bounceProject(),
) {
  installAuditBackend((command) => {
    if (command === "get_project") return snapshot;
    const handled = extra?.(command);
    if (handled !== undefined) return handled;
    if (command === "bounce_track_to_audio") return bounceReply(snapshot);
    if (command === "consolidate_selection") return consolidateReply(snapshot);
    return undefined;
  });
  await store.initProject();
  await store.stop();
  store.setSelectedTrackIndex(null);
  store.setSelectedPatternName(null);
  store.setEditorSelection(null);
  store.setBlockSelection([]);
  store.setError(null);
  invokeMock.mockClear();
}

/** Open every menu group, so a test can find a row that lives one level down. */
function expandAllMenuGroups(): void {
  for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
    if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
  }
}

function menuItem(label: string): HTMLElement {
  let item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")].find(
    (element) =>
      element.querySelector(".context-menu-label")?.textContent?.trim() === label,
  );
  if (!item) {
    expandAllMenuGroups();
    item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")].find(
      (element) =>
        element.querySelector(".context-menu-label")?.textContent?.trim() === label,
    );
  }
  if (!item) {
    const labels = [...document.querySelectorAll<HTMLElement>(".context-menu-label")]
      .map((element) => element.textContent?.trim())
      .join(", ");
    throw new Error(`no menu item ${label}; menu has ${labels}`);
  }
  return item;
}

function input(container: HTMLElement, label: string): HTMLInputElement {
  const field = container.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
  if (!field) throw new Error(`no input labelled ${label}`);
  return field;
}

function dialog(container: HTMLElement): HTMLElement | null {
  return container.querySelector<HTMLElement>('[role="dialog"]');
}

async function openTrackMenu(container: HTMLElement, name = "Lead") {
  const header = [...container.querySelectorAll<HTMLElement>(".track-header")].find(
    (element) => element.textContent?.includes(name),
  );
  if (!header) throw new Error(`no track header for ${name}`);
  fireEvent.contextMenu(header, { clientX: 40, clientY: 40 });
  await waitFor(() => expect(document.querySelectorAll(".context-menu-item").length).toBeGreaterThan(0));
}

beforeEach(() => {
  i18n.setLocale("en");
  closeContextMenu();
  bounceStore.close();
});

afterEach(() => {
  closeContextMenu();
  bounceStore.close();
  i18n.setLocale("en");
});

describe("bounce range", () => {
  it("parses bar:beat:subdivision and refuses anything else", () => {
    expect(parseBarBeat("9:1")).toEqual({ bar: 9, beat: 1, subdivision: 0 });
    expect(parseBarBeat("9:2:2")).toEqual({ bar: 9, beat: 2, subdivision: 2 });
    expect(parseBarBeat(" 9 ")).toEqual({ bar: 9, beat: 1, subdivision: 0 });
    expect(parseBarBeat("0:1")).toBeNull();
    expect(parseBarBeat("9:0")).toBeNull();
    expect(parseBarBeat("9:1:4")).toBeNull();
    expect(parseBarBeat("nine")).toBeNull();
    expect(parseBarBeat("")).toBeNull();
  });

  it("reports why a pair of fields is not a span", () => {
    const properties = bounceProject().project.properties;
    expect(rangeFailureKey(properties, "2:1", "3:1")).toBeNull();
    expect(rangeFailureKey(properties, "3:1", "2:1")).toBe("bounce.rangeOrder");
    expect(rangeFailureKey(properties, "3:1", "3:1")).toBe("bounce.rangeOrder");
    expect(rangeFailureKey(properties, "nope", "3:1")).toBe("bounce.rangeSyntax");
  });

  it("measures where the notes stop and warns when the range cuts one", () => {
    const project: Project = heldNoteProject().project;
    expect(trackSoundingEnd(project, "Lead")).toBe(4);
    expect(rangeCutsNote(project, "Lead", "1:2")).toBe(true);
    expect(rangeCutsNote(project, "Lead", "2:1")).toBe(false);
    expect(rangeCutsNote(project, "Lead", "nope")).toBe(false);
  });

  it("defaults to the track's content bounds and to one bar when it has none", () => {
    const project: Project = bounceProject().project;
    expect(trackContentBounds(project, "Lead")).toEqual({ from: "1:1", to: "5:1" });
    expect(trackContentBounds(project, "Tape")).toEqual({ from: "1:1", to: "3:1" });
    const empty = { ...project, tracks: [...project.tracks, { ...makeTrack("Pad", 2), phrases: [] }] };
    expect(trackContentBounds(empty, "Pad")).toEqual({ from: "1:1", to: "2:1" });
  });
});

describe("bounce prompt", () => {
  it("opens from the track menu with the track's content bounds", async () => {
    await prepare();
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    expect(input(view.container, i18n.t("bounce.from")).value).toBe("1:1");
    expect(input(view.container, i18n.t("bounce.to")).value).toBe("5:1");
    expect(bounceStore.request()?.mode).toBe("new_track");
  });

  it("sends the range and the chosen target, then reports the line", async () => {
    await prepare();
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    fireEvent.click([...view.container.querySelectorAll<HTMLInputElement>('input[type="radio"]')][1]);
    fireEvent.click(
      [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0],
    );
    await waitFor(() => expect(bounceStore.report()).not.toBeNull());
    const calls = invokeMock.mock.calls.filter(([command]) => command === "bounce_track_to_audio");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({
      track: "Lead",
      from: "1:1",
      to: "5:1",
      mode: "in_place",
      tail: "include_release",
    });
    expect(bounceStore.report()).toContain("media/bounce-lead-1.wav");
    await waitFor(() => expect(dialog(view.container)).toBeNull());
    expect(view.container.querySelector(".status-bar")?.textContent).toContain(
      "Baked insert chain (limiter)",
    );
  });

  it("warns about a cut release and sends the chosen tail", async () => {
    await prepare(undefined, heldNoteProject());
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    expect(bounceStore.request()?.tail).toBe("include_release");
    expect(view.container.querySelector(".bounce-warning")).toBeNull();

    fireEvent.input(input(view.container, i18n.t("bounce.to")), { target: { value: "1:2" } });
    await waitFor(() => expect(view.container.querySelector(".bounce-warning")).toBeTruthy());
    const tailChoice = [...view.container.querySelectorAll<HTMLElement>(".bounce-mode")].at(-1);
    const radios = [...(tailChoice?.querySelectorAll<HTMLInputElement>('input[type="radio"]') ?? [])];
    expect(radios).toHaveLength(2);
    expect(radios[0]?.checked).toBe(true);
    fireEvent.click(radios[1]!);
    await waitFor(() => expect(bounceStore.request()?.tail).toBe("cut_exactly"));

    fireEvent.click(
      [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0],
    );
    await waitFor(() => expect(bounceStore.report()).not.toBeNull());
    const calls = invokeMock.mock.calls.filter(([command]) => command === "bounce_track_to_audio");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({
      track: "Lead",
      from: "1:1",
      to: "1:2",
      mode: "new_track",
      tail: "cut_exactly",
    });
  });

  it("keeps the prompt and its pending state while the render runs", async () => {
    let release: ((value: unknown) => void) | undefined;
    const pending = new Promise((resolve) => {
      release = resolve;
    });
    await prepare((command) => (command === "bounce_track_to_audio" ? pending : undefined));
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    fireEvent.click(
      [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0],
    );
    await waitFor(() => {
      const button = [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0];
      expect(button?.disabled).toBe(true);
      expect(button?.textContent).toBe(i18n.t("bounce.pendingBounce"));
    });
    expect(dialog(view.container)).toBeTruthy();
    release?.(bounceReply());
    await waitFor(() => expect(dialog(view.container)).toBeNull());
  });

  it("shows a refusal inside the prompt and keeps the dialog open", async () => {
    await prepare((command) => {
      if (command !== "bounce_track_to_audio") return undefined;
      throw new Error("the bounce range ends at or before its start: 5:1 to 1:1");
    });
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    const reportBefore = bounceStore.report();
    fireEvent.click(
      [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0],
    );
    await waitFor(() =>
      expect(view.container.querySelector(".bounce-failure")?.textContent).toContain(
        "ends at or before its start",
      ),
    );
    expect(dialog(view.container)).toBeTruthy();
    expect(bounceStore.report()).toBe(reportBefore);
  });

  it("blocks a range whose end is not after its start", async () => {
    await prepare();
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelectorAll(".track-header").length).toBe(2));
    await openTrackMenu(view.container);
    fireEvent.click(menuItem(i18n.t("menu.track.bounce")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    fireEvent.input(input(view.container, i18n.t("bounce.to")), { target: { value: "0:1" } });
    await waitFor(() =>
      expect(view.container.querySelector(".bounce-failure")?.textContent).toBe(
        i18n.t("bounce.rangeSyntax"),
      ),
    );
    const confirm = [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0];
    expect(confirm?.disabled).toBe(true);
    expect(
      invokeMock.mock.calls.filter(([command]) => command === "bounce_track_to_audio"),
    ).toHaveLength(0);
  });
});

describe("consolidate prompt", () => {
  it("opens from the audio clip menu and sends the clip index", async () => {
    await prepare();
    const view = render(() => <App />);
    await waitFor(() => expect(view.container.querySelector(".audio-clip-block")).toBeTruthy());
    fireEvent.contextMenu(view.container.querySelector<HTMLElement>(".audio-clip-block")!, {
      clientX: 40,
      clientY: 40,
    });
    await waitFor(() => expect(document.querySelectorAll(".context-menu-item").length).toBeGreaterThan(0));
    fireEvent.click(menuItem(i18n.t("menu.clip.consolidate")));
    await waitFor(() => expect(dialog(view.container)).toBeTruthy());
    expect(input(view.container, i18n.t("bounce.from")).value).toBe("1:1");
    expect(input(view.container, i18n.t("bounce.to")).value).toBe("3:1");
    expect(input(view.container, i18n.t("bounce.from")).readOnly).toBe(true);
    fireEvent.click(
      [...document.querySelectorAll<HTMLButtonElement>(".text-prompt-btn.primary")][0],
    );
    await waitFor(() => expect(bounceStore.report()).not.toBeNull());
    const calls = invokeMock.mock.calls.filter(([command]) => command === "consolidate_selection");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({ track: "Tape", clipIndices: [0] });
    expect(bounceStore.report()).toContain("Consolidated 1 audio clip on 'Tape'");
  });
});
