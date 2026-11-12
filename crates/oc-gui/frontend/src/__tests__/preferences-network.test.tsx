/**
 * The Network page: which proxy outbound requests follow, and a probe that
 * reports what actually happened.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { PreferencesModal } from "../components/PreferencesModal";
import { invokeMock } from "./setup";

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args: any) => {
    if (cmd === "network_settings") return { proxyMode: "system", proxyUrl: "" };
    if (cmd === "network_save") return args.settings;
    if (cmd === "network_test") {
      return { ok: true, status: 200, ms: 42, bytes: 2161 };
    }
    return null;
  });
});

describe("the Network preferences page", () => {
  it("saves a manual proxy and reports the probe", async () => {
    const view = render(() => <PreferencesModal open onClose={() => {}} />);
    fireEvent.click(view.container.querySelector('[data-page="network"]')!);

    const mode = await waitFor(() => {
      const select = view.container.querySelector<HTMLSelectElement>(".preferences-proxy-mode");
      expect(select).toBeTruthy();
      return select!;
    });
    fireEvent.change(mode, { target: { value: "manual" } });

    const url = view.container.querySelector<HTMLInputElement>(".preferences-proxy-url")!;
    await waitFor(() => expect(url.disabled).toBe(false));
    fireEvent.change(url, { target: { value: "http://proxy.example.invalid:8080" } });

    await waitFor(() =>
      expect(
        invokeMock.mock.calls.some(
          (call) =>
            call[0] === "network_save" &&
            call[1]?.settings?.proxyMode === "manual" &&
            call[1]?.settings?.proxyUrl === "http://proxy.example.invalid:8080",
        ),
      ).toBe(true),
    );

    fireEvent.click(view.container.querySelector(".preferences-network-test")!);
    await waitFor(() => expect(view.container.querySelector(".preferences-network-result")).toBeTruthy());
    expect(view.container.querySelector(".preferences-network-result")?.textContent).toContain("42");
    expect(invokeMock).toHaveBeenCalledWith("network_test", {
      proxyMode: "manual",
      proxyUrl: "http://proxy.example.invalid:8080",
    });
  });

  it("keeps the URL field disabled unless the mode is manual", async () => {
    const view = render(() => <PreferencesModal open onClose={() => {}} />);
    fireEvent.click(view.container.querySelector('[data-page="network"]')!);
    await waitFor(() => expect(view.container.querySelector(".preferences-proxy-url")).toBeTruthy());
    expect(view.container.querySelector<HTMLInputElement>(".preferences-proxy-url")!.disabled).toBe(true);
  });
});
