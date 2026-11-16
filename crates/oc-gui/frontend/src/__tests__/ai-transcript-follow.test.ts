import { afterEach, expect, it, vi } from "vitest";
import { followTranscript } from "../ai/transcriptScroll";

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren(); });

it("keeps following through composer and scale resizing, while respecting a reader who scrolls up", async () => {
  let resized: () => void = () => {};
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resized = callback; }
    observe() {}
    disconnect() {}
  });
  const list = document.createElement("div");
  document.body.append(list);
  let height = 200;
  let extent = 1000;
  let top = 800;
  Object.defineProperties(list, {
    clientHeight: { get: () => height },
    scrollHeight: { get: () => extent },
    scrollTop: { get: () => top, set: value => { top = Math.min(Number(value), extent - height); } },
  });
  const follow = followTranscript(list);
  follow.resume();
  await Promise.resolve();
  height = 300;
  top = 700;
  list.dispatchEvent(new Event("scroll"));
  height = 150;
  list.dispatchEvent(new Event("scroll"));
  resized();
  await Promise.resolve();
  expect(top).toBe(850);
  list.dispatchEvent(new Event("scroll"));
  top = 400;
  list.dispatchEvent(new Event("scroll"));
  height = 100;
  extent = 1200;
  list.dispatchEvent(new Event("scroll"));
  resized();
  list.append("New reply text");
  await Promise.resolve();
  await Promise.resolve();
  expect(top).toBe(400);
  follow.resume();
  await Promise.resolve();
  expect(top).toBe(1100);
  follow.dispose();
});
