import { afterEach, describe, expect, it } from "vitest";
import { applyPlatform, isMac, isWindows } from "../stores/platform";

const original = navigator.userAgent;
function withUserAgent(value: string) {
  Object.defineProperty(navigator, "userAgent", { value, configurable: true });
}

afterEach(() => withUserAgent(original));

describe("platform", () => {
  it("tags macOS so the chrome reserves the traffic-light gutter", () => {
    withUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15");
    expect(isMac()).toBe(true);
    expect(isWindows()).toBe(false);
    applyPlatform();
    expect(document.documentElement.dataset.platform).toBe("macos");
  });

  it("tags Windows so the native frame is left alone", () => {
    withUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
    expect(isWindows()).toBe(true);
    applyPlatform();
    expect(document.documentElement.dataset.platform).toBe("windows");
  });

  it("falls back to linux", () => {
    withUserAgent("Mozilla/5.0 (X11; Linux x86_64)");
    applyPlatform();
    expect(document.documentElement.dataset.platform).toBe("linux");
  });
});
