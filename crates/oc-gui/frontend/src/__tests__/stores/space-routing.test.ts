/**
 * Space routing: the active editing region wins over selection and panel mode.
 */
import { describe, it, expect } from "vitest";
import { spaceCommand } from "../../stores/panels";

describe("space routing", () => {
  it("starts on the timeline before any panel click", () => {
    expect(spaceCommand("timeline", null, null, null, false)).toEqual({
      kind: "transport",
    });
  });

  it("auditions a selected preset when the bottom panel is the active region", () => {
    expect(spaceCommand("bottom", "presets", "Piano 1", null, false)).toEqual({
      kind: "audition-preset",
      preset: "Piano 1",
    });
  });

  it("auditions the selected clip from the Clips panel", () => {
    expect(spaceCommand("bottom", "clips", null, "piano_hook", false)).toEqual({
      kind: "audition-pattern",
      pattern: "piano_hook",
    });
  });

  it("auditions the edited clip from the Piano Roll", () => {
    expect(spaceCommand("bottom", "pianoroll", null, "piano_hook", false)).toEqual({
      kind: "audition-pattern",
      pattern: "piano_hook",
    });
  });

  it("transports when the presets panel is active but no preset is selected", () => {
    expect(spaceCommand("bottom", "presets", null, "piano_hook", false)).toEqual({
      kind: "transport",
    });
  });

  it("transports in panels that have no audition target", () => {
    expect(spaceCommand("bottom", "mixer", "Piano 1", "piano_hook", false)).toEqual({
      kind: "transport",
    });
  });

  it("toggles the active audition from the bottom region", () => {
    expect(spaceCommand("bottom", "presets", "Piano 1", null, true)).toEqual({
      kind: "toggle-audition",
    });
  });

  it("transports when the last pointer-down was in the timeline", () => {
    expect(spaceCommand("timeline", "presets", "Piano 1", "piano_hook", false)).toEqual({
      kind: "transport",
    });
  });

  it("toggles the active audition even from the timeline", () => {
    expect(spaceCommand("timeline", "presets", "Piano 1", null, true)).toEqual({
      kind: "toggle-audition",
    });
  });
});
