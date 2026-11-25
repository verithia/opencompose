import { describe, expect, it } from "vitest";
import { trackDisplayName } from "../utils/trackLabel";
import { makeTrack } from "./setup";

describe("track display labels", () => {
  it("uses the authored label without mutating the routing identifier", () => {
    const track = makeTrack("source_042", 0);
    track.mix.overrides = { display_name: "  Opening lead  " };
    expect(trackDisplayName(track)).toBe("Opening lead");
    expect(track.name).toBe("source_042");
    expect(track.mix.overrides.display_name).toBe("  Opening lead  ");
  });
  it("keeps the identifier as a fallback for absent or blank labels", () => {
    const track = makeTrack("Piano", 0);
    expect(trackDisplayName(track)).toBe("Piano");
    track.mix.overrides = { display_name: "   " };
    expect(trackDisplayName(track)).toBe("Piano");
  });
});
