/**
 * `@` references: the paths a project offers, the token the composer is typing,
 * and the filter behind the palette.
 */
import { describe, expect, it } from "vitest";
import {
  filterReferences,
  projectReferences,
  referenceQuery,
  withoutReferenceQuery,
} from "../ai/references";
import { makePattern, makeSnapshot, makeTrack } from "./setup";

const project = makeSnapshot({
  tracks: [makeTrack("Bass", 0), makeTrack("Pad", 1)],
  patterns: [makePattern("Verse")],
}).project;

describe("the paths a project exposes", () => {
  it("lists the project, then tracks, presets, patterns and buses", () => {
    expect(projectReferences(project).map(reference => reference.path)).toEqual([
      "project",
      "track:Bass",
      "track:Pad",
      "preset:Piano 1",
      "pattern:Verse",
    ]);
  });

  it("skips a name that cannot be written as one path segment", () => {
    const dotted = makeSnapshot({
      tracks: [makeTrack("Bass.01", 0), makeTrack("bass@2", 1), makeTrack("Pad", 2)],
    }).project;
    expect(projectReferences(dotted).map(reference => reference.path)).toEqual([
      "project",
      "track:Pad",
      "preset:Piano 1",
    ]);
  });

  it("lists a name once even when two collections repeat it", () => {
    const repeated = makeSnapshot({
      tracks: [makeTrack("Bass", 0), makeTrack("Bass", 1)],
    }).project;
    expect(projectReferences(repeated).map(reference => reference.path)).toEqual([
      "project",
      "track:Bass",
      "preset:Piano 1",
    ]);
  });
});

describe("the reference token in the composer", () => {
  it("reads the query after a trailing @", () => {
    expect(referenceQuery("make the bass louder @Ba")).toBe("ba");
    expect(referenceQuery("@")).toBe("");
    expect(referenceQuery("  @track:")).toBe("track:");
  });

  it("ignores an @ inside a word and a token that is not at the end", () => {
    expect(referenceQuery("mail me at me@example")).toBeNull();
    expect(referenceQuery("look at @bass now")).toBeNull();
    expect(referenceQuery("no reference")).toBeNull();
  });

  it("removes the token and the space before it", () => {
    expect(withoutReferenceQuery("make the bass louder @ba")).toBe("make the bass louder");
    expect(withoutReferenceQuery("only @")).toBe("only");
    expect(withoutReferenceQuery("no reference")).toBe("no reference");
  });

  it("filters paths by the query, case-insensitively, in project order", () => {
    const references = projectReferences(project);
    expect(filterReferences(references, "tr").map(reference => reference.path)).toEqual([
      "track:Bass",
      "track:Pad",
    ]);
    expect(filterReferences(references, "BASS").map(reference => reference.path)).toEqual(["track:Bass"]);
    expect(filterReferences(references, "")).toHaveLength(references.length);
    expect(filterReferences(references, "zzz")).toEqual([]);
  });
});
