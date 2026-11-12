import { describe, expect, it } from "vitest";
import {
  ARTICULATIONS,
  articulationForToken,
  articulationTag,
  articulationToken,
} from "../types/project";

// The roll draws a two-letter tag and writes a DSL token; both must name exactly
// one articulation, or a note would show another note's marker.
describe("articulation table", () => {
  it("covers every articulation the DSL can write", () => {
    expect(ARTICULATIONS).toHaveLength(29);
  });

  it("gives every articulation a unique two-letter tag", () => {
    const tags = ARTICULATIONS.map((entry) => entry.tag);
    const duplicate = tags.find((tag, index) => tags.indexOf(tag) !== index);
    expect(duplicate).toBeUndefined();
    for (const tag of tags) expect(tag).toMatch(/^[a-z]{2}$/);
  });

  it("round-trips every token", () => {
    for (const entry of ARTICULATIONS) {
      expect(articulationForToken(entry.token)).toBe(entry.value);
      expect(articulationToken(entry.value)).toBe(entry.token);
      expect(articulationTag(entry.value)).toBe(entry.tag);
    }
  });

  it("reports nothing for an unset or unknown articulation", () => {
    expect(articulationToken(null)).toBe("");
    expect(articulationTag(undefined)).toBe("");
    expect(articulationForToken("col_legno_ish")).toBeNull();
  });
});
