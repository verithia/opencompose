/**
 * The click store: toggle, level clamp, count-in gating, and adopting the
 * backend's snapshot.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "../setup";
import { clickStore, type ClickSettings, type ClickView } from "../../stores/click";

function view(overrides: Partial<ClickView> = {}): ClickView {
  return {
    enabled: false,
    level_db: -6,
    count_in: "off",
    count_in_available: false,
    ...overrides,
  };
}

beforeEach(() => {
  clickStore.dispose();
  invokeMock.mockReset();
});

afterEach(() => {
  clickStore.dispose();
});

describe("clickStore", () => {
  it("starts with the click off and the count-in off", () => {
    expect(clickStore.enabled()).toBe(false);
    expect(clickStore.countIn()).toBe("off");
    expect(clickStore.countInAvailable()).toBe(false);
  });

  it("adopts the settings the transport snapshot carries", () => {
    clickStore.adoptClick(view({ enabled: true, level_db: -12, count_in: "one_bar", count_in_available: true }));
    expect(clickStore.enabled()).toBe(true);
    expect(clickStore.levelDb()).toBe(-12);
    expect(clickStore.countIn()).toBe("one_bar");
    expect(clickStore.countInAvailable()).toBe(true);
  });

  it("ignores a snapshot without the click fields", () => {
    clickStore.adoptClick(undefined);
    clickStore.adoptClick({} as ClickView);
    expect(clickStore.enabled()).toBe(false);
    expect(clickStore.levelDb()).toBe(-6);
  });

  it("sends the toggle and adopts the result", async () => {
    const sent: ClickSettings[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "set_click") {
        const settings = (args as { settings: ClickSettings }).settings;
        sent.push(settings);
        return view({ ...settings, count_in_available: false });
      }
      return {};
    });
    await clickStore.setEnabled(true);
    expect(sent[0]).toEqual({ enabled: true, level_db: -6, count_in: "off" });
    expect(clickStore.enabled()).toBe(true);
  });

  it("clamps the click level into the accepted range", async () => {
    const sent: ClickSettings[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "set_click") {
        const settings = (args as { settings: ClickSettings }).settings;
        sent.push(settings);
        return view(settings);
      }
      return {};
    });
    await clickStore.setLevelDb(-200);
    await clickStore.setLevelDb(40);
    expect(sent[0].level_db).toBe(-60);
    expect(sent[1].level_db).toBe(6);
  });

  it("refuses a count-in until a track is armed", async () => {
    let calls = 0;
    invokeMock.mockImplementation(async () => {
      calls += 1;
      return view();
    });
    const result = await clickStore.setCountIn("one_bar");
    expect(result).toBeNull();
    expect(calls).toBe(0);
    expect(clickStore.countIn()).toBe("off");
  });

  it("sends the count-in when arming makes it available", async () => {
    const sent: ClickSettings[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "set_click") {
        const settings = (args as { settings: ClickSettings }).settings;
        sent.push(settings);
        return view({ ...settings, count_in_available: true });
      }
      return {};
    });
    clickStore.adoptClick(view({ count_in_available: true }));
    await clickStore.setCountIn("two_bars");
    expect(sent[0].count_in).toBe("two_bars");
    expect(clickStore.countIn()).toBe("two_bars");
  });

  it("keeps the failure the backend reports", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_click") throw new Error("the click stream is unavailable");
      return {};
    });
    const result = await clickStore.setEnabled(true);
    expect(result).toBeNull();
    expect(clickStore.error()).toContain("the click stream is unavailable");
  });
});
