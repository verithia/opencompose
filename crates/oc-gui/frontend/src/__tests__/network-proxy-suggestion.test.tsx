/**
 * The global proxy notice: it only appears for a route the diagnosis verified,
 * and applying it writes the manual proxy setting.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { i18n } from "../stores/i18n";
import { networkStore } from "../stores/network";
import { ProxySuggestion } from "../components/ProxySuggestion";

beforeEach(() => {
  invokeMock.mockClear();
  networkStore.reset();
  i18n.setLocale("en");
  invokeMock.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
    if (cmd === "network_save") return args.settings;
    return null;
  });
});

afterEach(() => invokeMock.mockReset());

describe("proxy suggestion", () => {
  it("applies a verified proxy", async () => {
    invokeMock.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === "network_diagnose") {
        return {
          current: { kind: "system", url: null, ok: false, ms: 5, error: "tls handshake eof" },
          routes: [
            { kind: "proxy", url: "http://127.0.0.1:7897", ok: true, ms: 30, error: null },
            { kind: "direct", url: null, ok: false, ms: 20, error: "timeout" },
          ],
        };
      }
      if (cmd === "network_save") return args.settings;
      return null;
    });
    await networkStore.diagnose();

    const view = render(() => <ProxySuggestion />);
    const notice = view.container.querySelector("[data-proxy-suggestion]");
    expect(notice?.getAttribute("data-proxy-suggestion")).toBe("http://127.0.0.1:7897");
    expect(view.container.textContent).toContain("http://127.0.0.1:7897");

    fireEvent.click(view.getByText(i18n.t("network.useThisProxy")));
    await vi.waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "network_save")).toEqual([
        ["network_save", { settings: { proxyMode: "manual", proxyUrl: "http://127.0.0.1:7897" } }],
      ])
    );
    await vi.waitFor(() => expect(networkStore.offer()).toBeNull());
  });

  it("stays away when no route works", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "network_diagnose") {
        return {
          current: { kind: "system", url: null, ok: false, ms: 5, error: "tls handshake eof" },
          routes: [
            { kind: "direct", url: null, ok: false, ms: 2000, error: "timeout" },
          ],
        };
      }
      return null;
    });
    await networkStore.diagnose();
    const view = render(() => <ProxySuggestion />);
    expect(view.container.querySelector("[data-proxy-suggestion]")).toBeNull();
  });
});
