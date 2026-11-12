/**
 * Hover-card placement table: the card prefers the space below its anchor,
 * flips above near the bottom edge, and when neither side holds it at full
 * height it stays attached on the roomier side with a scrolling max height.
 */
import { describe, expect, it } from "vitest";
import { placeTooltip } from "../../components/Tooltip";

const VIEWPORT = { width: 800, height: 768 };

const anchor = (top: number, height = 20) => ({
  left: 100,
  right: 180,
  top,
  bottom: top + height,
  width: 80,
  height,
});

interface Case {
  name: string;
  anchor: ReturnType<typeof anchor>;
  size: { width: number; height: number };
  side: "top" | "bottom";
  y: number;
  maxHeight: number | null;
}

const CASES: Case[] = [
  {
    name: "fits below the anchor",
    anchor: anchor(100),
    size: { width: 200, height: 80 },
    side: "bottom",
    y: 128,
    maxHeight: null,
  },
  {
    name: "flips above near the bottom edge",
    anchor: anchor(700),
    size: { width: 200, height: 80 },
    side: "top",
    y: 612,
    maxHeight: null,
  },
  {
    name: "clamps below when neither side fits and below has more room",
    anchor: anchor(2),
    size: { width: 200, height: 900 },
    side: "bottom",
    y: 30,
    maxHeight: 730,
  },
  {
    name: "clamps above when neither side fits and above has more room",
    anchor: anchor(600),
    size: { width: 200, height: 600 },
    side: "top",
    y: 8,
    maxHeight: 584,
  },
  {
    name: "never yields a negative Y",
    anchor: anchor(0),
    size: { width: 200, height: 1200 },
    side: "bottom",
    y: 28,
    maxHeight: 732,
  },
];

describe("placeTooltip", () => {
  it.each(CASES)("$name", (testCase) => {
    const placed = placeTooltip(testCase.anchor, testCase.size, VIEWPORT);
    expect(placed.side).toBe(testCase.side);
    expect(placed.y).toBe(testCase.y);
    expect(placed.maxHeight).toBe(testCase.maxHeight);
    expect(placed.y).toBeGreaterThanOrEqual(8);
    expect(placed.x).toBeGreaterThanOrEqual(8);
    expect(placed.x).toBeLessThanOrEqual(VIEWPORT.width - 8 - testCase.size.width);
  });

  it("keeps the clamped card inside the viewport and still scrollable", () => {
    const placed = placeTooltip(anchor(2), { width: 200, height: 900 }, VIEWPORT);
    expect(placed.maxHeight).not.toBeNull();
    expect(placed.y + placed.maxHeight!).toBeLessThanOrEqual(VIEWPORT.height - 8);
  });
});
