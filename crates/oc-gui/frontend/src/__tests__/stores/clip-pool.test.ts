/**
 * The typed clip pool and the one dispatch path above it.
 *
 * The pool lists pattern clips, audio clips and automation lanes in document
 * order with stable keys; the actions resolve a key, check the capability
 * matrix and forward to the store method that owns that kind. A refusal comes
 * back as the sentence the backend wrote, never as a generic failure.
 */
import { beforeEach, describe, expect, it } from "vitest";
import {
  TRANSPORT_STOPPED,
  invokeMock,
  makeAudioClip,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { i18n } from "../../stores/i18n";
import { store } from "../../stores/project";
import {
  audioKey,
  automationKey,
  clipAt,
  clipLabel,
  clipPool,
  patternKey,
} from "../../stores/clipPool";
import {
  clipCapability,
  clipMenuItems,
  deleteClip,
  duplicateClip,
  moveClip,
  resizeClip,
  splitClip,
} from "../../stores/clipActions";
import type { ContextMenuItem } from "../../components/ContextMenu";
import type { ProjectSnapshot } from "../../types/project";
import type { AutomationLane } from "../../types/project";

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

function lane(param: string, overrides: Partial<AutomationLane> = {}): AutomationLane {
  return { param, points: [], ...overrides };
}

/** One of each kind, plus an automation clip and a whole-timeline lane. */
function project(): ProjectSnapshot {
  const track = {
    ...makeTrack("Lead", 0),
    clips: [{ pattern: "hook", time_range: range([1, 1, 0], [3, 1, 0]) }],
    automation: [
      lane("volume", { time_range: range([1, 1, 0], [3, 1, 0]) }),
      lane("pan"),
    ],
  };
  return makeSnapshot({
    patterns: [makePattern("hook")],
    tracks: [track],
    buses: [{
      name: "drums",
      routes: [],
      effects: [],
      automation: [lane("fx1.mix", { time_range: range([1, 1, 0], [3, 1, 0]) })],
    }],
    master: {
      effects: [],
      automation: [lane("fx2.mix")],
      loudness_target: null,
      volume_db: 0,
    },
    properties: {
      ...makeSnapshot().project.properties,
      audio_clips: [makeAudioClip({ track: "Lead" })],
    },
  });
}

async function loadProject(): Promise<void> {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return project();
  });
  await store.initProject();
}

function commandCount(name: string): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

function labels(items: ContextMenuItem[]): string[] {
  return items.flatMap((item) =>
    "label" in item && item.type !== "heading" ? [item.label] : [],
  );
}

type ActionItem = Extract<ContextMenuItem, { onSelect: unknown }>;

function item(items: ContextMenuItem[], label: string): ActionItem | undefined {
  const rows = items.flatMap((entry) =>
    entry.type === "group" ? entry.items : [entry],
  );
  return rows.find(
    (entry): entry is ActionItem => "onSelect" in entry && entry.label === label,
  );
}

beforeEach(async () => {
  invokeMock.mockClear();
  i18n.setLocale("en");
  await loadProject();
  invokeMock.mockClear();
});

describe("clip pool", () => {
  it("lists one of each kind in document order with its label", () => {
    const clips = clipPool();
    expect(clips.map((clip) => clip.key)).toEqual([
      "track:0/clip:0",
      "audio:0",
      "automation:track:0/0",
      "automation:track:0/1",
      "automation:bus:drums/0",
      "automation:master/0",
    ]);
    expect(clips.map((clip) => clip.label)).toEqual([
      "hook",
      "hit",
      "track:0/volume",
      "track:0/pan",
      "bus:drums/fx1.mix",
      "master/fx2.mix",
    ]);
    expect(clips.map((clip) => clip.kind)).toEqual([
      "pattern",
      "audio",
      "automation",
      "automation",
      "automation",
      "automation",
    ]);
  });

  it("resolves a key to its clip and its label", () => {
    expect(clipAt(patternKey(0, 0))?.label).toBe("hook");
    expect(clipAt(audioKey(0))?.label).toBe("hit");
    expect(clipLabel(automationKey("bus:drums", 0))).toBe("bus:drums/fx1.mix");
    expect(clipAt("track:9/clip:9")).toBeNull();
    expect(clipLabel("audio:9")).toBeNull();
  });

  it("keeps a whole-timeline lane in the list without a span", () => {
    const whole = clipAt(automationKey("track:0", 1));
    expect(whole?.time_range).toBeNull();
    expect(clipAt(automationKey("track:0", 0))?.time_range).not.toBeNull();
  });
});

describe("clip menu", () => {
  it("offers the pattern actions in their order", () => {
    const items = clipMenuItems(patternKey(0, 0));
    expect(labels(items)).toEqual([
      "Inspect clip",
      "Preview clip",
      "Open editor",
      "Rename pattern",
      "Colour",
      "Edit",
      "Clip content",
      "Selection",
      "Delete",
    ]);
    // The playhead sits outside the clip, so Split states why it cannot run.
    expect(item(items, "Split at playhead")?.hint).toBe(i18n.t("menu.clip.splitHint"));
    expect(item(items, "Group clips")?.hint).toBe(i18n.t("menu.clip.groupHint"));
  });

  it("offers the audio actions in their order", () => {
    const items = clipMenuItems(audioKey(0));
    expect(labels(items)).toEqual([
      "Inspect clip",
      "Preview clip",
      "Speed and pitch…",
      "Clip gain",
      "Add gain envelope",
      "Clip content",
      "Delete",
    ]);
    expect(item(items, "Split at playhead")?.hint).toBe(i18n.t("menu.clip.splitHint"));
    // No audio media is loaded, so Loop states the reason instead of running.
    expect(item(items, "Repeat to fill")?.hint).toBe(i18n.t("clip.loopNeedsMedia"));
  });

  it("offers the automation actions and refuses the clip gestures on a whole-timeline lane", () => {
    const clipLane = clipMenuItems(automationKey("track:0", 0));
    expect(labels(clipLane)).toEqual([
      "Inspect clip",
      "Copy to playhead",
      "Duplicate",
      "Split at playhead",
      "Delete",
    ]);

    const whole = clipMenuItems(automationKey("track:0", 1));
    const reason = i18n.t("timeline.automation.wholeTimeline");
    expect(labels(whole)).toEqual([
      "Inspect clip",
      "Copy to playhead",
      "Duplicate",
      "Split at playhead",
      "Delete",
    ]);
    for (const label of labels(whole)) {
      const entry = item(whole, label);
      if (label === "Delete" || label === "Inspect clip") {
        // Inspect and delete do not require a bounded clip span.
        expect(entry?.disabled, label).toBeUndefined();
        expect(entry?.hint, label).toBeUndefined();
        continue;
      }
      expect(entry?.disabled, label).toBe(true);
      expect(entry?.hint, label).toBe(reason);
    }
  });

  it("names no actions for a key that no longer resolves", () => {
    expect(clipMenuItems(audioKey(9))).toEqual([]);
  });
});

describe("clip dispatch", () => {
  it("deletes each kind through the command that owns it", async () => {
    expect(await deleteClip(patternKey(0, 0))).toBeNull();
    expect(commandCount("remove_clip")).toBe(1);

    expect(await deleteClip(audioKey(0))).toBeNull();
    expect(commandCount("remove_audio_clip")).toBe(1);

    expect(await deleteClip(automationKey("track:0", 0))).toBeNull();
    expect(commandCount("delete_automation_clip")).toBe(1);
  });

  it("returns the backend message when a move is refused", async () => {
    const refusal =
      "Clip [2:1 - 4:1] would overlap [1:1 - 3:1] on 'Lead'";
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "move_clip") throw new Error(refusal);
      return project();
    });

    const reason = await moveClip(patternKey(0, 0), { bar: 2, beat: 1, subdivision: 0 });
    expect(reason).toContain(refusal);
    expect(store.error()).toContain(refusal);
  });

  it("refuses every gesture on a whole-timeline lane with the stated reason", async () => {
    const key = automationKey("track:0", 1);
    const reason = i18n.t("timeline.automation.wholeTimeline");
    const to = { bar: 2, beat: 1, subdivision: 0 };

    expect(await moveClip(key, to)).toBe(reason);
    expect(await resizeClip(key, "end", { bar: 4, beat: 1, subdivision: 0 })).toBe(reason);
    expect(await duplicateClip(key)).toBe(reason);
    expect(await splitClip(key)).toBe(reason);
    expect(commandCount("move_automation_clip")).toBe(0);
    expect(commandCount("resize_automation_clip")).toBe(0);
    expect(commandCount("duplicate_automation_clip")).toBe(0);
    expect(commandCount("split_automation_clip")).toBe(0);
  });

  it("refuses to duplicate an audio clip but moves it", async () => {
    expect(clipCapability(audioKey(0), "duplicate").reason).toBe(
      i18n.t("clip.noDuplicate"),
    );
    expect(await duplicateClip(audioKey(0))).toBe(i18n.t("clip.noDuplicate"));

    expect(await moveClip(audioKey(0), { bar: 5, beat: 1, subdivision: 0 })).toBeNull();
    expect(commandCount("move_audio_clip")).toBe(1);
  });

  it("splits a pattern clip at the playhead in one command", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: 4 };
      return project();
    });
    await store.seek(4);
    expect(await splitClip(patternKey(0, 0))).toBeNull();
    expect(commandCount("split_clip")).toBe(1);
    expect(commandCount("remove_clip")).toBe(0);
    expect(commandCount("add_clip")).toBe(0);
  });
});
