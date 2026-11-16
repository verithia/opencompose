import { afterEach, describe, expect, it } from "vitest";
import { changeSetText, changeText } from "../../stores/changeText";
import { i18n } from "../../stores/i18n";
import { isChangeSet, type Change, type ChangeSet } from "../../types/change";
import type { ProjectProperties } from "../../types/project";

const properties: ProjectProperties = {
  tempo: 120,
  time_signature: { numerator: 4, denominator: 4 },
  key: null,
  swing: null,
  tempo_map: [],
  time_sig_map: [],
  key_map: [],
};

const set = (...changes: Change[]): ChangeSet => ({ changes, more: 0 });

afterEach(() => i18n.setLocale("en"));

describe("change text", () => {
  it("names project settings, tempo, meter, key and swing facts", () => {
    i18n.setLocale("en");
    expect(changeText({ kind: "project_renamed", from: null, to: "Song" })).toBe("Name the project “Song”");
    expect(changeText({ kind: "project_renamed", from: "Draft", to: "Song" })).toBe("Rename project “Draft” to “Song”");
    expect(changeText({ kind: "project_renamed", from: "Draft", to: null })).toBe("Clear the project name");
    expect(changeText({ kind: "project_version", from: "1.0", to: "1.1" })).toBe("Change project version from 1.0 to 1.1");
    expect(changeText({ kind: "tempo", from: 120, to: 132 })).toBe("Change tempo from 120 to 132 BPM");
    expect(changeText({
      kind: "time_signature",
      from: { numerator: 4, denominator: 4 },
      to: { numerator: 7, denominator: 8 },
    })).toBe("Change time signature from 4/4 to 7/8");
    expect(changeText({
      kind: "key",
      from: { root: "C", mode: "major" },
      to: { root: "F#", mode: "minor" },
    })).toBe("Change key from C major to F# minor");
    expect(changeText({ kind: "key", from: null, to: { root: "Bb", mode: "major" } })).toBe("Set key to Bb major");
    expect(changeText({ kind: "key", from: { root: "C", mode: "major" }, to: null })).toBe("Clear the key");
    expect(changeText({ kind: "swing", from: null, to: 20 })).toBe("Set swing to 20%");
    expect(changeText({ kind: "swing", from: 20, to: 0 })).toBe("Change swing from 20% to 0%");
    expect(changeText({ kind: "swing", from: 20, to: null })).toBe("Clear swing");
    expect(changeText({ kind: "tempo_map" })).toBe("Edit the tempo map");
    expect(changeText({ kind: "project_settings" })).toBe("Change project settings");
  });

  it("names tracks, effects and mix parameters", () => {
    i18n.setLocale("en");
    expect(changeText({ kind: "track_added", track: "keys" })).toBe("Add track “keys”");
    expect(changeText({ kind: "track_renamed", from: "keys", to: "lead" })).toBe("Rename track “keys” to “lead”");
    expect(changeText({
      kind: "track_mix",
      track: "kick",
      params: [{ kind: "volume", from: 0, to: -3 }, { kind: "mute", from: false, to: true }],
    })).toBe("Change volume · mute on “kick”");
    expect(changeText({
      kind: "effect_added", owner: { kind: "track", name: "keys" }, effect: "reverb",
    })).toBe("Add Reverb to track “keys”");
    expect(changeText({
      kind: "effect_added", owner: { kind: "master" }, effect: "plugin:fabfilter.pro-q",
    })).toBe("Add fabfilter.pro-q to the master");
    expect(changeText({
      kind: "effect_bypassed", owner: { kind: "bus", name: "verb" }, effect: "eq", bypassed: false,
    })).toBe("Enable EQ on bus “verb”");
    expect(changeText({ kind: "effects_reordered", owner: { kind: "master" } })).toBe("Reorder effects on the master");
    expect(changeText({
      kind: "bus_mix",
      bus: "verb",
      params: [{ kind: "volume", from: 0, to: 1 }, { kind: "routes" }],
    })).toBe("Change volume · routes on bus “verb”");
  });

  it("names content facts and converts beat spans to bars", () => {
    i18n.setLocale("en");
    expect(changeText({
      kind: "notes_edited", track: "lead", span: { start: 0, end: 16 }, added: 2, removed: 1, changed: 0,
    }, { properties })).toBe("Edit notes on “lead”: 2 added, 1 removed, 0 changed in bars 1–4");
    expect(changeText({
      kind: "clips_edited", track: "lead", span: { start: 4, end: 6 },
    }, { properties })).toBe("Edit clips on “lead” in bar 2 beats 1–2");
    expect(changeText({
      kind: "automation_edited", owner: { kind: "master" }, span: { start: 0, end: 16 },
    }, { properties })).toBe("Edit automation on the master in bars 1–4");
    expect(changeText({
      kind: "pattern_changed", pattern: "hook", span: { start: 0, end: 16 },
    }, { properties })).toBe("Change pattern “hook” in bars 1–4");
    expect(changeText({ kind: "audio_clips_edited", track: "vox", span: null })).toBe("Edit audio clips on “vox”");
    expect(changeText({ kind: "sections_edited", span: { start: 0, end: 16 } }, { properties })).toBe("Edit sections in bars 1–4");
    expect(changeText({ kind: "markers_edited", added: 1, removed: 2, changed: 0 }))
      .toBe("Edit markers: 1 added, 2 removed, 0 changed");
    expect(changeText({ kind: "preset_added", preset: "warm pad" })).toBe("Add preset “warm pad”");
    expect(changeText({ kind: "preset_renamed", from: "pad", to: "warm pad" }))
      .toBe("Rename preset “pad” to “warm pad”");
    expect(changeText({ kind: "pattern_renamed", from: "riff", to: "hook" }))
      .toBe("Rename pattern “riff” to “hook”");
    expect(changeText({ kind: "bus_removed", bus: "verb" })).toBe("Remove bus “verb”");
    expect(changeText({ kind: "bus_renamed", from: "verb", to: "space" }))
      .toBe("Rename bus “verb” to “space”");
    expect(changeText({ kind: "groups_edited" })).toBe("Edit clip groups");
  });

  it("uses the project meter when a span becomes bars", () => {
    i18n.setLocale("en");
    const threeFour = { ...properties, time_signature: { numerator: 3, denominator: 4 } };
    expect(changeText({ kind: "clips_edited", track: "lead", span: { start: 0, end: 12 } }, { properties: threeFour }))
      .toBe("Edit clips on “lead” in bars 1–4");
    expect(changeText({ kind: "clips_edited", track: "lead", span: { start: 0, end: 15 } }, { properties: threeFour }))
      .toBe("Edit clips on “lead” in bars 1–5");
  });

  it("joins a few facts and counts the remainder", () => {
    i18n.setLocale("en");
    const many = set(
      { kind: "tempo", from: 120, to: 132 },
      { kind: "track_added", track: "keys" },
      { kind: "preset_added", preset: "pad" },
    );
    expect(changeSetText(many, { max: 2 })).toBe("Change tempo from 120 to 132 BPM · Add track “keys” · +1 more");
    expect(changeSetText({ changes: [{ kind: "tempo", from: 120, to: 132 }], more: 4 }))
      .toBe("Change tempo from 120 to 132 BPM · +4 more");
  });

  it("reads an empty set and an unknown fact as a generic edit", () => {
    i18n.setLocale("en");
    expect(changeSetText({ changes: [], more: 0 })).toBe("Edit");
    expect(changeText({ kind: "future_fact" } as unknown as Change)).toBe("Edit");
  });

  it("translates facts, spans and the remainder into zh-Hans", () => {
    i18n.setLocale("zh-Hans");
    expect(changeText({ kind: "tempo", from: 100, to: 132 })).toBe("将速度从 100 BPM 改为 132 BPM");
    expect(changeText({
      kind: "notes_edited", track: "lead", span: { start: 0, end: 16 }, added: 2, removed: 0, changed: 0,
    }, { properties })).toBe("编辑“lead”的音符：新增 2，删除 0，修改 0（第1–4小节）");
    expect(changeSetText({ changes: [{ kind: "track_added", track: "keys" }], more: 4 }))
      .toBe("添加轨道“keys” · 另 4 项");
  });
});

describe("change set guard", () => {
  it("accepts a bounded list, keeps unknown kinds and drops a malformed count", () => {
    expect(isChangeSet(null)).toBe(false);
    expect(isChangeSet({ changes: "no" })).toBe(false);
    const guarded = { changes: [{ kind: "future_fact" }], more: "4" };
    expect(isChangeSet(guarded)).toBe(true);
    expect(guarded.more).toBe(0);
    expect(changeSetText(guarded as unknown as ChangeSet)).toBe("Edit");
  });

  it("cuts an oversized list to the bound", () => {
    const huge = { changes: Array.from({ length: 200 }, () => ({ kind: "tempo", from: 1, to: 2 })), more: 0 };
    expect(isChangeSet(huge)).toBe(true);
    expect(huge.changes).toHaveLength(64);
  });
});
