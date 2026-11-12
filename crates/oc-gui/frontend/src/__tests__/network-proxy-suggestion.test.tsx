/**
 * The startup proxy offer: only in UTC+8, only once, and applying it writes the
 * manual proxy setting.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { networkStore } from "../stores/network";
import { ProxySuggestion } from "../components/ProxySuggestion";

beforeEach(() => {
  invokeMock.mockClear();
  networkStore.resetDetection();
  globalThis.localStorage?.clear?.();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "network_settings") return { proxyMode: "system", proxyUrl: "" };
    if (cmd === "detect_local_proxy") return "http://127.0.0.1:7890";
    if (cmd === "network_save") return args?.settings;
    return null;
  });
});

afterEach(() => vi.restoreAllMocks());

describe("proxy suggestion", () => {
  it("appears in UTC+8 and applies the detected proxy", async () => {
    vi.spyOn(Date.prototype, "getTimezoneOffset").mockReturnValue(-480);
    await networkStore.load();
    await networkStore.detect();

    const view = render(() => <ProxySuggestion />);
    const notice = view.container.querySelector("[data-proxy-suggestion]");
    expect(notice?.getAttribute("data-proxy-suggestion")).toBe("http://127.0.0.1:7890");
    expect(view.container.textContent).toContain("http://127.0.0.1:7890");

    fireEvent.click(view.getByText("Use it"));
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "network_save")).toEqual([
        ["network_save", { settings: { proxyMode: "manual", proxyUrl: "http://127.0.0.1:7890" } }],
      ])
    );
    await vi.waitFor(() => expect(networkStore.suggestion()).toBeNull());
  });

  it("stays away outside UTC+8", async () => {
    vi.spyOn(Date.prototype, "getTimezoneOffset").mockReturnValue(0);
    const probe = vi.fn();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "network_settings") return { proxyMode: "system", proxyUrl: "" };
      probe(cmd);
      return null;
    });
    await networkStore.load();
    await networkStore.detect();
    expect(probe).not.toHaveBeenCalledWith("detect_local_proxy");
    expect(networkStore.suggestion()).toBeNull();
  });
});
