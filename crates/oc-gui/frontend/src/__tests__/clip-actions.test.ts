/**
 * Pattern clip edits keep the playback window that makes a clip sound like
 * itself.
 *
 * A split runs as one command, so one undo restores the clip and a refusal
 * leaves the document untouched. Copy, duplicate and paste carry the slip and
 * loop a pattern clip was playing instead of restarting at the pattern head.
 */
import { beforeEach, describe, expect, it } from "vitest";
import {
  TRANSPORT_STOPPED,
  getLastCallArgs,
  invokeMock,
  makeNote,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "./setup";
import { i18n } from "../stores/i18n";
import { store } from "../stores/project";
import { patternKey } from "../stores/clipPool";
import {
  copyClip,
  copyClipToClipboard,
  duplicateClip,
  pasteBlockAtPlayhead,
  splitClip,
} from "../stores/clipActions";
import type { Clip, ProjectSnapshot } from "../types/project";

const SLIP = 1.5;
const LOOP = 2;

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

/** A pattern whose content runs seven quarters, so the window fits inside it. */
function pattern(name: string) {
  return {
    ...makePattern(name),
    events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 6 })],
  };
}

function clip(overrides: Partial<Clip> = {}): Clip {
  return {
    pattern: "hook",
    time_range: range([1, 1, 0], [5, 1, 0]),
    offset_quarters: SLIP,
    loop_quarters: LOOP,
    ...overrides,
  };
}

function project(clips: Clip[] = [clip()], filePath: string | null = null): ProjectSnapshot {
  const snap = makeSnapshot({
    patterns: [pattern("hook")],
    tracks: [{ ...makeTrack("Lead", 0), clips }],
  });
  snap.file_path = filePath;
  return snap;
}

function commandCount(name: string): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

/** Load one project and answer `split_clip` with the halves it hands back. */
async function load(snap: ProjectSnapshot, splitInto?: Clip[], refusal?: string) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "transport_seek") {
      return { ...TRANSPORT_STOPPED(), position_beats: 4 };
    }
    if (cmd === "split_clip") {
      if (refusal) throw new Error(refusal);
      return project(splitInto ?? snap.project.tracks[0].clips);
    }
    return snap;
  });
  await store.initProject();
  invokeMock.mockClear();
}

beforeEach(async () => {
  invokeMock.mockClear();
  i18n.setLocale("en");
  await load(project());
  invokeMock.mockClear();
});

describe("split", () => {
  it("runs as one command, never as a remove followed by two adds", async () => {
    const halves = [
      clip({ time_range: range([1, 1, 0], [2, 1, 0]) }),
      clip({ time_range: range([2, 1, 0], [5, 1, 0]) }),
    ];
    await load(project(), halves);
    await store.seek(4);

    expect(await splitClip(patternKey(0, 0))).toBeNull();

    expect(commandCount("split_clip")).toBe(1);
    expect(getLastCallArgs("split_clip")).toMatchObject({
      trackIndex: 0,
      clipIndex: 0,
      atBar: 2,
      atBeat: 1,
      atSub: 0,
    });
    // Three separate mutations cost three undo entries and can lose the clip.
    expect(commandCount("remove_clip")).toBe(0);
    expect(commandCount("add_clip")).toBe(0);
    expect(store.project.tracks[0].clips).toHaveLength(2);
  });

  it("leaves the clip in place when the backend refuses the split", async () => {
    const refusal = "The right half would start past the pattern's content; move the split earlier.";
    await load(project(), undefined, refusal);
    await store.seek(4);

    expect(await splitClip(patternKey(0, 0))).toContain(refusal);
    expect(store.error()).toContain(refusal);
    expect(commandCount("split_clip")).toBe(1);
    expect(commandCount("remove_clip")).toBe(0);
    expect(commandCount("add_clip")).toBe(0);
    expect(store.project.tracks[0].clips).toEqual([clip()]);
  });
});

describe("copy", () => {
  it("carries the slip and loop a copy was playing", async () => {
    await copyClip(patternKey(0, 0), { bar: 5, beat: 1, subdivision: 0 });

    expect(commandCount("add_clip")).toBe(1);
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0,
      patternName: "hook",
      offsetQuarters: SLIP,
      loopQuarters: LOOP,
    });
  });

  it("carries them through duplicate as well", async () => {
    await duplicateClip(patternKey(0, 0));

    expect(commandCount("add_clip")).toBe(1);
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0,
      patternName: "hook",
      offsetQuarters: SLIP,
      loopQuarters: LOOP,
    });
  });

  it("carries them through the clipboard and a paste", async () => {
    copyClipToClipboard(patternKey(0, 0));
    await pasteBlockAtPlayhead();

    expect(commandCount("add_clip")).toBe(1);
    expect(getLastCallArgs("add_clip")).toMatchObject({
      patternName: "hook",
      offsetQuarters: SLIP,
      loopQuarters: LOOP,
    });
  });

  it("sends no window for a clip that plays its pattern from the head", async () => {
    const plain = project([clip({ offset_quarters: undefined, loop_quarters: undefined })]);
    await load(plain);

    await copyClip(patternKey(0, 0), { bar: 5, beat: 1, subdivision: 0 });

    expect(getLastCallArgs("add_clip")).toMatchObject({
      offsetQuarters: null,
      loopQuarters: null,
    });
  });
});

describe("document identity", () => {
  it("advances on a load and stays put on an edit", async () => {
    const first = store.documentIdentity();

    // An edit changes the document's content, not which document is open.
    await copyClip(patternKey(0, 0), { bar: 5, beat: 1, subdivision: 0 });
    expect(store.documentIdentity()).toBe(first);

    await load(project([clip()], "/tmp/second.ocp"));
    expect(store.documentIdentity().path).toBe("/tmp/second.ocp");
    expect(store.documentIdentity().generation).toBe(first.generation + 1);
  });
});
