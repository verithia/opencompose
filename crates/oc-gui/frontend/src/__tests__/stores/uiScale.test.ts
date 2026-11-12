import { describe, it, expect, beforeEach } from "vitest";
import { recommendedUiScale, uiScaleStore } from "../../stores/uiScale";

function publishedScale(): string {
  return document.documentElement.style.getPropertyValue("--ui-scale");
}

beforeEach(() => {
  document.documentElement.style.removeProperty("--ui-scale");
});

describe("uiScaleStore", () => {
  it("publishes the scale as --ui-scale on init", () => {
    uiScaleStore.set(0.8);
    document.documentElement.style.removeProperty("--ui-scale");

    uiScaleStore.init();

    expect(publishedScale()).toBe("0.8");
  });

  it("updates --ui-scale on every change", () => {
    uiScaleStore.set(1.5);
    expect(publishedScale()).toBe("1.5");

    uiScaleStore.set(0.7);
    expect(publishedScale()).toBe("0.7");
  });
});


it.each([
  [1440, 900, 1], [1920, 1080, 1], [1024, 640, 1], [960, 600, 0.9],
  [864, 576, 0.8], [700, 440, 0.7], [0, 0, 1], [Infinity, 900, 1],
])("chooses a usable scale from logical viewport %d × %d", (width, height, expected) => {
  expect(recommendedUiScale(width, height)).toBe(expected);
});

it("reset forgets the override and restores the recommendation", () => {
  const original = Object.getOwnPropertyDescriptor(window, "localStorage");
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", { configurable: true, value: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  } });
  try {
    uiScaleStore.set(1.4);
    expect(values.get("opencompose.uiScale")).toBe("1.4");
    uiScaleStore.reset();
    expect(uiScaleStore.scale()).toBe(uiScaleStore.recommended());
    expect(values.has("opencompose.uiScale")).toBe(false);
  } finally {
    if (original) Object.defineProperty(window, "localStorage", original);
    else Reflect.deleteProperty(window, "localStorage");
  }
});
