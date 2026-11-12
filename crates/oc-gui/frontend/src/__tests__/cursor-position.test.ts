/**
 * The playhead paints the interpolated position only while the transport runs.
 *
 * The poll reports twenty positions a second; painting those alone makes the
 * head step instead of move.
 */
import { describe, expect, it } from "vitest";
import { cursorBeat } from "../utils/cursorPosition";

describe("cursor beat", () => {
  it("uses the interpolated position while playing", () => {
    expect(cursorBeat(true, 4.61, 4.5)).toBe(4.61);
  });

  it("uses the reported position when parked", () => {
    expect(cursorBeat(false, 4.61, 4.5)).toBe(4.5);
  });

  it("keeps a parked cursor exactly where the transport reports it", () => {
    // A stale interpolation must never move a stopped head.
    expect(cursorBeat(false, 9, 2)).toBe(2);
  });
});
